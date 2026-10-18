//! Phase 6: yield/resume lifecycle 監査 event の統合テスト（tree backend、opt-in）。
//!
//! 設計正本は `docs/determinism-and-audit.md` §7.2（schema: `Yielded` tag5 / `Resumed` tag6）/
//! §8（emission 順序・完全性: 規則7 flush-before-Yielded / 規則8 Resumed-before-work / 規則9
//! Terminal last）。監査付き run 経路に小さい slice fuel を与えて実 `Yielded(SliceFuelExhausted)`
//! を起こし、Started↔Terminal の間に `Yielded`/`Resumed` が交互・poll_index 単調・
//! previous_reason 一致で流れることを固定する。sink 無し経路の非破壊性（byte-for-byte 不変）も
//! 検証する。crate root の公開型だけを使う。

use std::sync::Arc;

use tsumugi::{
    AuditEvent, AuditSink, AuditYieldReason, BudgetConfig, CapabilitySet, CompileOptions,
    EmbeddingContext, EmbeddingEngine, EmbeddingOutcome, EmbeddingRequest, ExecutionId, FakeClock,
    InMemoryAuditSink, LinkRequest, MonotonicClock, Source, SourceId,
};

// -----------------------------------------------------------------------------
// ヘルパ
// -----------------------------------------------------------------------------

/// 複数 slice に跨る純粋計算スクリプト（capability 不要・外部出力なし）。
///
/// `while` ループを十分な反復数で回し、小さい slice fuel の下で何度も
/// `Yielded(SliceFuelExhausted)` が起きるようにする。出力も host call もないので
/// sink の有無・slice 分割に依らず観測結果（最終 context / usage）は不変。
const LOOP_SCRIPT: &str =
    "let total = 0\nlet i = 0\nwhile i < 30\n    total = total + i\n    i = i + 1\nend\n";

fn source(id: &str, text: &'static str) -> Source<'static> {
    Source::new(SourceId::new(id).expect("valid source id"), text)
}

fn compile(
    engine: &EmbeddingEngine,
    id: &str,
    text: &'static str,
) -> tsumugi::EmbeddingCompiledScript {
    engine
        .compile(
            source(id, text),
            &CompileOptions {
                retain_source: true,
            },
        )
        .expect("compile")
}

fn link_request() -> LinkRequest {
    let clock = FakeClock::new();
    let budget = BudgetConfig::standard(&clock).expect("standard budget");
    LinkRequest::new(
        ExecutionId::new(1u128.try_into().expect("nonzero")),
        CapabilitySet::empty(),
        budget,
    )
}

/// standard budget + 共有 clock + execution_id で run request を作る（capability は空で十分）。
fn run_request(id: u128) -> EmbeddingRequest {
    let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
    let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
    EmbeddingRequest::new(budget, clock)
        .expect("standard budget は同 domain")
        .with_execution_id(ExecutionId::new(id.try_into().expect("nonzero")))
        .with_capabilities(CapabilitySet::empty())
}

/// envelope 列から Yielded/Resumed だけを sequence 順に抽出する。
fn yield_resume_events(envelopes: &[tsumugi::AuditEnvelope]) -> Vec<&AuditEvent> {
    envelopes
        .iter()
        .map(|e| &e.event)
        .filter(|e| matches!(e, AuditEvent::Yielded { .. } | AuditEvent::Resumed { .. }))
        .collect()
}

// -----------------------------------------------------------------------------
// (1) 実 multi-slice yield: Yielded/Resumed が交互・poll_index 単調・previous_reason 一致
// -----------------------------------------------------------------------------

#[test]
fn multi_slice_execution_emits_alternating_yielded_resumed() {
    let sink = Arc::new(InMemoryAuditSink::new());
    // 小さい slice fuel（16）で監査付き実行を複数 slice に分ける（§4.2: slice fuel の量子）。
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .audit_slice_fuel(16)
        .build()
        .expect("build");

    let script = compile(&engine, "m", LOOP_SCRIPT);
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, run_request(7));
    assert!(
        matches!(audited.outcome(), Some(EmbeddingOutcome::Completed { .. })),
        "小 slice fuel でも最終的に Completed: {:?}",
        audited.outcome()
    );

    let envelopes = sink.snapshot();
    assert!(!envelopes.is_empty(), "監査 event があるはず");

    // 全体の不変条件（§8 規則1/9）: 先頭 Started / 末尾 Terminal / sequence 0 始まり gap なし。
    assert!(matches!(
        envelopes[0].event,
        AuditEvent::ExecutionStarted { .. }
    ));
    assert_eq!(
        envelopes[0].sequence, 0,
        "Started は sequence 0（§8 規則1）"
    );
    assert!(matches!(
        envelopes.last().expect("non-empty").event,
        AuditEvent::Terminal { .. }
    ));
    for (i, env) in envelopes.iter().enumerate() {
        assert_eq!(env.sequence as usize, i, "sequence は 0 始まり gap なし");
    }
    // Terminal の後に event は無い（§8 規則9）。
    let terminal_pos = envelopes
        .iter()
        .position(|e| matches!(e.event, AuditEvent::Terminal { .. }))
        .expect("Terminal がある");
    assert_eq!(
        terminal_pos,
        envelopes.len() - 1,
        "Terminal は最後の event（§8 規則9）"
    );

    // Yielded/Resumed が実際に出ている（小 slice fuel で real yield が起きた）。
    let yr = yield_resume_events(&envelopes);
    assert!(
        yr.len() >= 2,
        "少なくとも 1 回の yield/resume pair が出るはず: {yr:?}"
    );

    // 交互性（§8/§15.3）: Yielded → Resumed → Yielded → … の順。先頭は Yielded。
    let mut expect_yielded = true;
    let mut last_poll_index: Option<u64> = None;
    let mut last_yielded_reason: Option<AuditYieldReason> = None;
    for ev in &yr {
        match ev {
            AuditEvent::Yielded {
                reason, poll_index, ..
            } => {
                assert!(expect_yielded, "Yielded を期待する位置: {ev:?}");
                // 実 slice fuel 枯渇の yield。
                assert_eq!(
                    *reason,
                    AuditYieldReason::SliceFuelExhausted,
                    "slice fuel 枯渇の yield"
                );
                // poll_index は厳密単調増加。
                if let Some(prev) = last_poll_index {
                    assert!(
                        *poll_index > prev,
                        "poll_index は厳密単調: {poll_index} > {prev}"
                    );
                }
                last_poll_index = Some(*poll_index);
                last_yielded_reason = Some(reason.clone());
                expect_yielded = false;
            }
            AuditEvent::Resumed {
                previous_reason,
                poll_index,
                ..
            } => {
                assert!(!expect_yielded, "Resumed を期待する位置: {ev:?}");
                // previous_reason は直前の Yielded.reason と一致。
                assert_eq!(
                    Some(previous_reason.clone()),
                    last_yielded_reason,
                    "Resumed.previous_reason は直前 Yielded.reason と一致（§15.3）"
                );
                // 対になる Yielded と同じ poll_index を共有する。
                assert_eq!(
                    Some(*poll_index),
                    last_poll_index,
                    "Resumed は pair の Yielded と同じ poll_index"
                );
                expect_yielded = true;
            }
            other => panic!("yield/resume 以外が混入: {other:?}"),
        }
    }
}

// -----------------------------------------------------------------------------
// (2) §8 規則7: 各 slice の BudgetCharged はその slice の Yielded より前に来る
// -----------------------------------------------------------------------------

#[test]
fn budget_charged_precedes_its_yielded() {
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .audit_slice_fuel(16)
        .build()
        .expect("build");

    let script = compile(&engine, "m", LOOP_SCRIPT);
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(&linked, &mut ctx, run_request(11));
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::Completed { .. })
    ));

    let envelopes = sink.snapshot();
    // 最初の Yielded より前に少なくとも 1 件の BudgetCharged がある（規則7: flush before Yielded）。
    let first_yielded = envelopes
        .iter()
        .position(|e| matches!(e.event, AuditEvent::Yielded { .. }))
        .expect("Yielded がある");
    let charged_before_first_yield = envelopes[..first_yielded]
        .iter()
        .any(|e| matches!(e.event, AuditEvent::BudgetCharged { .. }));
    assert!(
        charged_before_first_yield,
        "最初の Yielded の前に BudgetCharged が flush される（§8 規則7）"
    );
}

// -----------------------------------------------------------------------------
// (3) 非破壊性: sink 無し経路は sink 付き（小 slice fuel）と観測結果が完全一致
// -----------------------------------------------------------------------------

#[test]
fn sinkless_path_is_byte_identical_to_audited() {
    // sink 無しの既定 engine（従来経路、監査 wiring なし・単一 slice）。
    let plain_engine = EmbeddingEngine::builder().build().expect("build plain");
    let plain_script = compile(&plain_engine, "m", LOOP_SCRIPT);
    let plain_linked = plain_engine
        .link(&plain_script, link_request())
        .expect("link plain");
    let mut plain_ctx = EmbeddingContext::new(&plain_engine);
    let plain_outcome = plain_engine.run(&plain_linked, &mut plain_ctx, run_request(21));

    // sink 付き + 小 slice fuel（複数 slice・yield 発生）。
    let sink = Arc::new(InMemoryAuditSink::new());
    let audited_engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .audit_slice_fuel(16)
        .build()
        .expect("build audited");
    let audited_script = compile(&audited_engine, "m", LOOP_SCRIPT);
    let audited_linked = audited_engine
        .link(&audited_script, link_request())
        .expect("link audited");
    let mut audited_ctx = EmbeddingContext::new(&audited_engine);
    let audited = audited_engine.run_audited(&audited_linked, &mut audited_ctx, run_request(21));
    let audited_outcome = audited
        .outcome()
        .expect("audit 成功で outcome を持つ")
        .clone();

    // 観測結果（terminal outcome と usage）は slice 分割・監査の有無に依らず一致する（§3）。
    assert_eq!(
        plain_outcome, audited_outcome,
        "sink 無しと sink 付き（小 slice fuel）で ExecutionOutcome / usage が一致（非破壊性）"
    );
    // sink 無し経路は event を一切 emission しない（opt-in、§1.1）。sink 付き経路は yield を出す。
    assert!(!sink.is_empty(), "sink 付き経路は監査 event を出す");
    let audited_envelopes = sink.snapshot();
    let yr = yield_resume_events(&audited_envelopes);
    assert!(
        !yr.is_empty(),
        "sink 付き・小 slice fuel で Yielded/Resumed が出る"
    );
}

// -----------------------------------------------------------------------------
// (4) 既定 slice fuel（未設定=u64::MAX）は監査付きでも単一 slice（yield なし）
// -----------------------------------------------------------------------------

#[test]
fn default_slice_fuel_does_not_yield_under_audit() {
    let sink = Arc::new(InMemoryAuditSink::new());
    // audit_slice_fuel を設定しない → 既定 u64::MAX（単一 slice、従来挙動）。
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = compile(&engine, "m", LOOP_SCRIPT);
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(&linked, &mut ctx, run_request(31));
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::Completed { .. })
    ));

    let envelopes = sink.snapshot();
    let yr = yield_resume_events(&envelopes);
    assert!(
        yr.is_empty(),
        "既定 slice fuel（u64::MAX）では yield は起きず Yielded/Resumed は出ない: {yr:?}"
    );
}
