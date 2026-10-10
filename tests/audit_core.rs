//! Phase 6 実行時監査（audit）A-1 スライスの end-to-end 統合テスト。
//!
//! 設計正本は `docs/determinism-and-audit.md` §7/§8/§10/§11。tree engine のみ。
//! crate root から re-export される公開型だけを使い、監査の opt-in・emission 順序・
//! outcome 写像・fail-closed を固定する。

use std::sync::Arc;

use tsumugi::{
    AuditEvent, AuditSink, AuditedOutcome, BudgetConfig, CompileOptions, EmbeddingContext,
    EmbeddingEngine, EmbeddingOutcome, EmbeddingRequest, ExecutionId, FakeClock, InMemoryAuditSink,
    LinkRequest, MonotonicClock, ScriptedAuditSink, ScriptedResponse, Source, SourceId,
    TerminalOutcome,
};

/// 決定的テスト用の standard budget + 共有 clock から request を作る（監査 execution_id 付き）。
fn request_with_id(id: u128) -> EmbeddingRequest {
    let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
    let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
    EmbeddingRequest::new(budget, clock)
        .expect("standard budget は自身を生成した clock と同 domain")
        .with_execution_id(ExecutionId::new(id.try_into().expect("nonzero")))
}

/// execution_id を載せない request（no-sink parity の確認用）。
fn request_without_id() -> EmbeddingRequest {
    let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
    let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
    EmbeddingRequest::new(budget, clock).expect("standard budget は同 domain")
}

/// import なし link 用の既定 [`LinkRequest`]（C6-c の 3 引数 `new`）。
///
/// 本ファイルの script は import を持たないため、empty capabilities（resolver 未 grant）+ standard
/// budget で足りる（import 0 件は resolver を呼ばず空 graph を返す）。
fn link_request() -> LinkRequest {
    let clock = FakeClock::new();
    let budget = BudgetConfig::standard(&clock).expect("standard budget");
    LinkRequest::new(
        ExecutionId::new(1u128.try_into().expect("nonzero")),
        tsumugi::CapabilitySet::empty(),
        budget,
    )
}

fn source(id: &str, text: &'static str) -> Source<'static> {
    Source::new(SourceId::new(id).expect("valid source id"), text)
}

/// retain=true で compile → link し、runnable な linked script を返す。
fn link_source(
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

// --- (c) opt-in no-sink parity ---

#[test]
fn no_sink_means_no_emission_and_unchanged_outcome() {
    // sink を設定しない engine。既存挙動と bit-identical（監査なし）。
    let engine = EmbeddingEngine::builder().build().expect("build");
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    // 別の in-memory sink を作っても、engine に設定していないので触られない。
    let observer = InMemoryAuditSink::new();

    let outcome = engine.run(&linked, &mut ctx, request_with_id(1));
    assert!(matches!(outcome, EmbeddingOutcome::Completed { .. }));
    // sink は engine に未設定なので何も記録されない。
    assert!(observer.is_empty());
}

#[test]
fn no_sink_runtime_error_outcome_is_unchanged() {
    let engine = EmbeddingEngine::builder().build().expect("build");
    let script = link_source(&engine, "m", "let x = undefined_name\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let outcome = engine.run(&linked, &mut ctx, request_without_id());
    assert!(matches!(outcome, EmbeddingOutcome::RuntimeError { .. }));
}

// --- (d) happy-path emission ---

#[test]
fn with_sink_emits_started_then_terminal_in_order() {
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let execution_id = ExecutionId::new(42u128.try_into().unwrap());
    let request = {
        let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
        let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
        EmbeddingRequest::new(budget, clock)
            .expect("same domain")
            .with_execution_id(execution_id)
    };

    let audited = engine.run_audited(&linked, &mut ctx, request);
    match audited.outcome() {
        Some(EmbeddingOutcome::Completed { .. }) => {}
        other => panic!("期待 Completed, 実際 {other:?}"),
    }

    let envelopes = sink.snapshot();
    assert_eq!(envelopes.len(), 2, "Started と Terminal の 2 件のはず");

    // Started は sequence 0 に 1 件。
    assert_eq!(envelopes[0].sequence, 0);
    assert_eq!(envelopes[0].schema_version, 1);
    assert_eq!(envelopes[0].execution_id, execution_id);
    assert_eq!(envelopes[0].source_hash, *script.source_hash().as_bytes());
    assert_eq!(envelopes[0].language_revision, "0.20");
    assert!(matches!(
        envelopes[0].event,
        AuditEvent::ExecutionStarted { .. }
    ));

    // Terminal は最後（sequence 1）に 1 件、gap/重複なし。
    assert_eq!(envelopes[1].sequence, 1);
    assert_eq!(envelopes[1].execution_id, execution_id);
    match &envelopes[1].event {
        AuditEvent::Terminal {
            outcome,
            context_committed,
            host_effects_may_remain,
            ..
        } => {
            assert_eq!(*outcome, TerminalOutcome::Completed);
            assert!(*context_committed, "Completed は commit する");
            assert!(!*host_effects_may_remain);
        }
        other => panic!("期待 Terminal, 実際 {other:?}"),
    }
}

#[test]
fn with_sink_runtime_error_maps_to_runtime_error_terminal() {
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = undefined_name\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(7));
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::RuntimeError { .. })
    ));

    let envelopes = sink.snapshot();
    assert_eq!(envelopes.len(), 2);
    match &envelopes[1].event {
        AuditEvent::Terminal {
            outcome,
            error,
            context_committed,
            ..
        } => {
            assert_eq!(*outcome, TerminalOutcome::RuntimeError);
            assert!(!*context_committed, "RuntimeError は rollback する");
            assert!(error.is_some(), "error terminal は payload を持つ");
        }
        other => panic!("期待 Terminal, 実際 {other:?}"),
    }
}

#[test]
fn with_sink_pre_cancel_has_started_and_cancelled_terminal() {
    // pre-cancel（命令0）でも Started + Terminal(Cancelled) を持つ（§8）。
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = 1\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let token = tsumugi::CancellationToken::new();
    token.cancel();
    let request = {
        let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
        let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
        EmbeddingRequest::new(budget, clock)
            .expect("same domain")
            .with_execution_id(ExecutionId::new(99u128.try_into().unwrap()))
            .cancellation(token)
    };

    let audited = engine.run_audited(&linked, &mut ctx, request);
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::Cancelled { .. })
    ));
    let envelopes = sink.snapshot();
    assert_eq!(envelopes.len(), 2);
    assert!(matches!(
        envelopes[0].event,
        AuditEvent::ExecutionStarted { .. }
    ));
    match &envelopes[1].event {
        AuditEvent::Terminal { outcome, .. } => {
            assert_eq!(*outcome, TerminalOutcome::Cancelled);
        }
        other => panic!("期待 Terminal(Cancelled), 実際 {other:?}"),
    }
}

// --- (e) fail-closed ---

#[test]
fn failing_sink_is_fail_closed_and_not_success() {
    // Started の submit で Failed を返す sink。script work は走らず、結果は success でない。
    let sink = Arc::new(InMemoryAuditSink::failing());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    // completion するはずの script。sink が Failed なら実行結果は success にならない。
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(5));

    // fail-closed: AuditFailed を返し、success な outcome ではない。
    assert!(!audited.is_audited_ok(), "fail-closed なら監査成立ではない");
    match audited {
        AuditedOutcome::AuditFailed { withheld, .. } => {
            // script は 1 命令も走っていないので「保留 terminal」は無く、監査失敗自体を載せる。
            assert_eq!(withheld, TerminalOutcome::AuditFailure);
        }
        other => panic!("fail-closed のはずが: {other:?}"),
    }
    // failing sink は何も記録しない（submit が常に Failed）。
    assert!(sink.is_empty());
}

#[test]
fn terminal_submit_failure_is_fail_closed_and_not_success() {
    // Started は Ack、Terminal で Failed を返す sink。script は走り切るが、Terminal の配送に
    // 失敗するので監査は成立せず、結果は success ではない（§10.1、二度目失敗の fail-closed）。
    let sink = Arc::new(ScriptedAuditSink::new([
        ScriptedResponse::AckCorrect, // Started
        ScriptedResponse::Fail,       // Terminal
    ]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(11));

    assert!(
        !audited.is_audited_ok(),
        "Terminal 配送失敗なら監査成立ではない"
    );
    match audited {
        AuditedOutcome::AuditFailed { withheld, .. } => {
            // Terminal 配送失敗時は、本来報告されたはずの terminal（Completed）を withheld に残す。
            assert_eq!(withheld, TerminalOutcome::Completed);
        }
        other => panic!("Terminal fail-closed のはずが: {other:?}"),
    }
    // Started は記録済み（Ack 時に記録）、Terminal は Fail なので記録されない。
    let envelopes = sink.snapshot();
    assert_eq!(envelopes.len(), 1, "記録は Started のみ");
    assert!(matches!(
        envelopes[0].event,
        AuditEvent::ExecutionStarted { .. }
    ));
}

#[test]
fn terminal_submit_failure_leaves_committed_state_documented_a1_behavior() {
    // §14 A-1 逸脱の明文化を pin する: tree engine は Completed の language-state を
    // 評価器内（Terminal 配送より前）で commit するため、Terminal 配送が失敗して AuditFailed を
    // 返しても、完了済み script の context 変更は rollback されず残る。host は AuditFailed を
    // 受け取った context を再利用してはならない、という逸脱を回帰で固定する。
    let sink = Arc::new(ScriptedAuditSink::new([
        ScriptedResponse::AckCorrect, // 1 回目実行の Started
        ScriptedResponse::Fail,       // 1 回目実行の Terminal（配送失敗）
                                      // 以降（2 回目実行の Started/Terminal）は tail = AckCorrect
    ]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");

    // 1 回目: `x` を束縛して完了する script。Terminal 配送に失敗する。
    let define = link_source(&engine, "m", "let x = 1\n");
    let define_linked = engine.link(&define, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(&define_linked, &mut ctx, request_with_id(30));

    // fail-closed: success ではない。journal 上の Terminal は Completed/committed のまま
    // （AuditFailure へ書き換えない。§8 規則10）＝ withheld は Completed。
    assert!(!audited.is_audited_ok());
    match &audited {
        AuditedOutcome::AuditFailed { withheld, .. } => {
            assert_eq!(*withheld, TerminalOutcome::Completed);
        }
        other => panic!("Terminal fail-closed のはずが: {other:?}"),
    }

    // 2 回目: 同じ context で `x` を参照する script を走らせる。state が rollback されていれば
    // `x` は未定義で RuntimeError になるはず。だが A-1 では commit 済みのまま残るので、`x` は
    // 参照でき、完了する（＝ state は rollback されていない、という逸脱を明文化どおり pin）。
    let use_binding = link_source(&engine, "m2", "let y = x + 2\n");
    let use_linked = engine.link(&use_binding, link_request()).expect("link");
    let audited2 = engine.run_audited(&use_linked, &mut ctx, request_with_id(31));
    match audited2.outcome() {
        Some(EmbeddingOutcome::Completed { .. }) => {}
        other => panic!(
            "A-1 では 1 回目完了 script の binding `x` は rollback されず残るはず（state 継続）: {other:?}"
        ),
    }
}

#[test]
fn started_wrong_id_ack_is_fail_closed_before_script_work() {
    // Started の ack が別 execution の id を指す（§10 protocol 違反）→ script work せず fail-closed。
    let sink = Arc::new(ScriptedAuditSink::new([ScriptedResponse::AckWrongId]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    // side-effect probe: completion すれば ctx に x が束縛されるが、ここでは走ってはならない。
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(21));
    assert!(!audited.is_audited_ok(), "不正 ack は監査成立ではない");
    assert!(matches!(audited, AuditedOutcome::AuditFailed { .. }));
    // Started は submit されたが ack が不正だったので gate は開かない。
    // Terminal(AuditFailure) の submit は AckCorrect（tail）で記録されるため、
    // 「Started を試みた記録 + emergency Terminal」で 2 件になり得る。重要なのは Terminal が
    // AuditFailure であること（script work しないので Completed Terminal は出ない）。
    let envelopes = sink.snapshot();
    assert!(
        envelopes.iter().all(|e| !matches!(
            &e.event,
            AuditEvent::Terminal {
                outcome: TerminalOutcome::Completed,
                ..
            }
        )),
        "script work は起きないので Completed Terminal は出ない"
    );
}

#[test]
fn started_wrong_sequence_ack_is_fail_closed_before_script_work() {
    // Started の ack が未送信 sequence を指す（§10 protocol 違反）→ fail-closed。
    let sink = Arc::new(ScriptedAuditSink::new([ScriptedResponse::AckWrongSequence]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(22));
    assert!(
        !audited.is_audited_ok(),
        "不正 sequence ack は監査成立ではない"
    );
    assert!(matches!(audited, AuditedOutcome::AuditFailed { .. }));
    let envelopes = sink.snapshot();
    assert!(
        envelopes.iter().all(|e| !matches!(
            &e.event,
            AuditEvent::Terminal {
                outcome: TerminalOutcome::Completed,
                ..
            }
        )),
        "script work は起きないので Completed Terminal は出ない"
    );
}

#[test]
fn with_sink_but_missing_execution_id_fails_before_script_work() {
    // sink はあるが execution_id を載せていない → Started 前に fail-closed（§7.1）。
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = 1\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_without_id());
    // Started を発行する前の precondition 違反。InternalFailure（success ではない）。
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::InternalFailure { .. })
    ));
    // Started すら発行していないので sink は空。
    assert!(sink.is_empty());
}
