//! Phase 6 B-1: AuditBackpressure yield/resume の統合テスト（tree backend、opt-in）。
//!
//! 設計正本は `docs/determinism-and-audit.md` §10（sink 契約・poll resume）/ §10.2
//! （backpressure・BackpressureDeadline）/ §8（emission 順序）と、B-0 設計
//! （`.agents/tasks/audit-backpressure-yield/design-b0.md` §5 層分離 / §6.3 B-1 scope）。
//!
//! sink が `Pending` を返すと execution が実際に `Yielded(AuditBackpressure)` へ遷移し、ack で
//! `Resumed{previous_reason: AuditBackpressure}` へ resume することを envelope 列で固定する。
//! 既存 `tests/audit_yield_resume.rs` / `tests/audit_core.rs` の流儀（EmbeddingEngine builder +
//! audit_sink + snapshot）に倣い、crate root の公開型だけを使う。既存テストは無改変で緑のまま。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tsumugi::{
    AuditEnvelope, AuditEvent, AuditFailure, AuditSink, AuditYieldReason, AuditedOutcome,
    BudgetConfig, CapabilitySet, CompileOptions, EmbeddingContext, EmbeddingEngine,
    EmbeddingOutcome, EmbeddingRequest, ExecutionId, FakeClock, LinkRequest, MonotonicClock,
    MonotonicInstant, ScriptedAuditSink, ScriptedResponse, Source, SourceId,
};

// -----------------------------------------------------------------------------
// ヘルパ（audit_yield_resume.rs と同形）
// -----------------------------------------------------------------------------

/// 複数 slice に跨る純粋計算スクリプト（capability 不要・外部出力なし）。
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

fn run_request(id: u128) -> EmbeddingRequest {
    let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
    let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
    EmbeddingRequest::new(budget, clock)
        .expect("standard budget は同 domain")
        .with_execution_id(ExecutionId::new(id.try_into().expect("nonzero")))
        .with_capabilities(CapabilitySet::empty())
}

fn yield_resume_events(envelopes: &[AuditEnvelope]) -> Vec<&AuditEvent> {
    envelopes
        .iter()
        .map(|e| &e.event)
        .filter(|e| matches!(e, AuditEvent::Yielded { .. } | AuditEvent::Resumed { .. }))
        .collect()
}

// -----------------------------------------------------------------------------
// (a) Pending -> Ack: Yielded(AuditBackpressure) -> Resumed(AuditBackpressure) 交互
// -----------------------------------------------------------------------------

#[test]
fn backpressure_emits_alternating_yielded_resumed_with_shared_poll_index() {
    // 小 slice fuel で SliceFuel yield も混ぜつつ、2 番目の submit で backpressure を起こす。
    // plan: [AckCorrect(=Started), PendingThenAck{2}] → Started は Ack、2 番目に配送される
    // envelope で 2 回 Pending → 再送 Ack。以降 tail(AckCorrect)。
    let sink = Arc::new(ScriptedAuditSink::new([
        ScriptedResponse::AckCorrect,
        ScriptedResponse::PendingThenAck { pending_times: 2 },
    ]));
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
        "backpressure を挟んでも最終的に Completed: {:?}",
        audited.outcome()
    );

    let envelopes = sink.snapshot();
    // 不変条件: 先頭 Started / 末尾 Terminal / sequence 0 始まり gap なし（§8 規則1/9）。
    assert!(matches!(
        envelopes[0].event,
        AuditEvent::ExecutionStarted { .. }
    ));
    assert_eq!(envelopes[0].sequence, 0);
    for (i, env) in envelopes.iter().enumerate() {
        assert_eq!(env.sequence as usize, i, "sequence は 0 始まり gap なし");
    }
    assert!(matches!(
        envelopes.last().expect("non-empty").event,
        AuditEvent::Terminal { .. }
    ));

    // 少なくとも 1 組の AuditBackpressure Yielded/Resumed が出ている。
    let backpressure_yields = envelopes
        .iter()
        .filter(|e| {
            matches!(
                &e.event,
                AuditEvent::Yielded {
                    reason: AuditYieldReason::AuditBackpressure,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        backpressure_yields, 1,
        "PendingThenAck で backpressure yield がちょうど 1 回"
    );

    // Yielded/Resumed の pair 整合（nesting 対応）と poll_index 単調を検証する。
    //
    // backpressure は SliceFuel yield の event 配送中に起きうるため、外側 SliceFuel yield が
    // まだ resume していない状態で内側 backpressure yield がネストしうる（Y_outer, Y_inner,
    // R_inner, R_outer）。したがって「flat な交互」ではなく LIFO（stack）で pair を突き合わせる:
    // - 各 Yielded は poll_index を厳密単調増加で発番する（単一カウンタ）。
    // - 各 Resumed は stack 先頭（直近の未 resume Yielded）と reason/poll_index が一致する。
    let yr = yield_resume_events(&envelopes);
    let mut stack: Vec<(AuditYieldReason, u64)> = Vec::new();
    let mut max_poll_index: Option<u64> = None;
    let mut saw_backpressure_pair = false;
    for ev in &yr {
        match ev {
            AuditEvent::Yielded {
                reason, poll_index, ..
            } => {
                if let Some(prev) = max_poll_index {
                    assert!(
                        *poll_index > prev,
                        "poll_index は Yielded ごとに厳密単調: {poll_index} > {prev}"
                    );
                }
                max_poll_index = Some(*poll_index);
                stack.push((reason.clone(), *poll_index));
            }
            AuditEvent::Resumed {
                previous_reason,
                poll_index,
                ..
            } => {
                let (open_reason, open_index) = stack
                    .pop()
                    .expect("Resumed には対応する未 resume の Yielded がある");
                assert_eq!(
                    *previous_reason, open_reason,
                    "Resumed.previous_reason は stack 先頭 Yielded.reason と一致"
                );
                assert_eq!(
                    *poll_index, open_index,
                    "Resumed は pair の Yielded と同じ poll_index"
                );
                if *previous_reason == AuditYieldReason::AuditBackpressure {
                    saw_backpressure_pair = true;
                }
            }
            other => panic!("yield/resume 以外が混入: {other:?}"),
        }
    }
    assert!(
        stack.is_empty(),
        "全 Yielded が Resumed と pair になる: {stack:?}"
    );
    assert!(
        saw_backpressure_pair,
        "AuditBackpressure の Yielded->Resumed pair が観測されるはず"
    );
}

// -----------------------------------------------------------------------------
// (b) 単一 slice でも backpressure が立ち、resume して Completed に至る
// -----------------------------------------------------------------------------

#[test]
fn backpressure_on_single_slice_run_resumes_to_completed() {
    // 既定 slice fuel（SliceFuel yield なし）。BudgetCharged の配送で 1 回 backpressure させる。
    // plan: [AckCorrect(Started), PendingThenAck{1}] → 2 番目の envelope で 1 回 Pending→Ack。
    let sink = Arc::new(ScriptedAuditSink::new([
        ScriptedResponse::AckCorrect,
        ScriptedResponse::PendingThenAck { pending_times: 1 },
    ]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");

    let script = compile(&engine, "m", LOOP_SCRIPT);
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(&linked, &mut ctx, run_request(8));
    assert!(
        matches!(audited.outcome(), Some(EmbeddingOutcome::Completed { .. })),
        "backpressure 後に resume して Completed: {:?}",
        audited.outcome()
    );

    let envelopes = sink.snapshot();
    let backpressure_pairs = envelopes
        .iter()
        .filter(|e| {
            matches!(
                &e.event,
                AuditEvent::Yielded {
                    reason: AuditYieldReason::AuditBackpressure,
                    ..
                }
            )
        })
        .count();
    assert_eq!(backpressure_pairs, 1, "1 回 backpressure して resume");
    // 対になる Resumed(AuditBackpressure) が存在する。
    assert!(envelopes.iter().any(|e| matches!(
        &e.event,
        AuditEvent::Resumed {
            previous_reason: AuditYieldReason::AuditBackpressure,
            ..
        }
    )));
}

// -----------------------------------------------------------------------------
// (c) 永続 Pending -> deadline -> BackpressureDeadline（script の Deadline より優先）
// -----------------------------------------------------------------------------

/// `now()` を呼ぶたびに `tick_ns` だけ進む clock（budget 構築時の最初の呼び出しは 0 を返す）。
///
/// backpressure resend loop は各ステップで `checkpoint_deadline()` を呼ぶため、十分な回数で
/// `config.deadline`（= 作成時 now + 30 s）を超え、永続 Pending が BackpressureDeadline へ
/// 落ちることを決定的に検証できる（単一 thread でも clock が進む）。
struct AutoAdvanceClock {
    clock_id: u64,
    now_ns: AtomicU64,
    tick_ns: u64,
}

impl AutoAdvanceClock {
    fn new(tick_ns: u64) -> Self {
        // FakeClock と同じ clock_id 採番規則へ乗るよう、FakeClock から id を借りる。
        let seed = FakeClock::new();
        Self {
            clock_id: seed.clock_id(),
            now_ns: AtomicU64::new(0),
            tick_ns,
        }
    }
}

impl MonotonicClock for AutoAdvanceClock {
    fn clock_id(&self) -> u64 {
        self.clock_id
    }
    fn now(&self) -> MonotonicInstant {
        let prev = self.now_ns.fetch_add(self.tick_ns, Ordering::Relaxed);
        self.instant_at(prev)
    }
}

#[test]
fn persistent_pending_yields_backpressure_deadline_not_success() {
    // 永続 Pending で、resend ごとに進む clock が 30 s deadline を超え BackpressureDeadline。
    // plan: [AckCorrect(Started), PendingForever] → Started は Ack、次の envelope で永続 Pending。
    let sink = Arc::new(ScriptedAuditSink::new([
        ScriptedResponse::AckCorrect,
        ScriptedResponse::PendingForever,
    ]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");

    let script = compile(&engine, "m", LOOP_SCRIPT);
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    // 1 tick = 1 s。deadline は作成時 now + 30 s なので ~30 resend で超える（有限・決定的）。
    let clock: Arc<dyn MonotonicClock> = Arc::new(AutoAdvanceClock::new(1_000_000_000));
    let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
    let request = EmbeddingRequest::new(budget, clock)
        .expect("standard budget は同 domain")
        .with_execution_id(ExecutionId::new(9u128.try_into().expect("nonzero")))
        .with_capabilities(CapabilitySet::empty());

    let audited = engine.run_audited(&linked, &mut ctx, request);
    // success（Completed）ではなく、audit 由来の BackpressureDeadline で fail-closed。
    match &audited {
        AuditedOutcome::AuditFailed { failure, .. } => {
            assert_eq!(
                *failure,
                AuditFailure::BackpressureDeadline,
                "永続 Pending は BackpressureDeadline（§10.2、script Deadline より優先）"
            );
        }
        other => panic!("AuditFailed(BackpressureDeadline) を期待したが {other:?}"),
    }
    assert!(
        audited.outcome().is_none(),
        "backpressure deadline は success な outcome を持たない"
    );
}

// -----------------------------------------------------------------------------
// (d) backpressure なし実行の envelope 列は、常に Ack する sink と完全一致（回帰）
// -----------------------------------------------------------------------------

#[test]
fn zero_pending_matches_always_ack_envelope_sequence() {
    // (i) ScriptedAuditSink を PendingThenAck{0}（= 即 Ack）で回す。
    let scripted = Arc::new(ScriptedAuditSink::new([ScriptedResponse::PendingThenAck {
        pending_times: 0,
    }]));
    let engine_a = EmbeddingEngine::builder()
        .audit_sink(scripted.clone() as Arc<dyn AuditSink>)
        .audit_slice_fuel(16)
        .build()
        .expect("build a");
    let script_a = compile(&engine_a, "m", LOOP_SCRIPT);
    let linked_a = engine_a.link(&script_a, link_request()).expect("link a");
    let mut ctx_a = EmbeddingContext::new(&engine_a);
    let audited_a = engine_a.run_audited(&linked_a, &mut ctx_a, run_request(21));
    assert!(matches!(
        audited_a.outcome(),
        Some(EmbeddingOutcome::Completed { .. })
    ));

    // (ii) 常に Ack する InMemoryAuditSink。
    let in_memory = Arc::new(tsumugi::InMemoryAuditSink::new());
    let engine_b = EmbeddingEngine::builder()
        .audit_sink(in_memory.clone() as Arc<dyn AuditSink>)
        .audit_slice_fuel(16)
        .build()
        .expect("build b");
    let script_b = compile(&engine_b, "m", LOOP_SCRIPT);
    let linked_b = engine_b.link(&script_b, link_request()).expect("link b");
    let mut ctx_b = EmbeddingContext::new(&engine_b);
    let audited_b = engine_b.run_audited(&linked_b, &mut ctx_b, run_request(21));
    assert!(matches!(
        audited_b.outcome(),
        Some(EmbeddingOutcome::Completed { .. })
    ));

    // backpressure が一度も起きない実行では envelope 列（event 種別列）が一致する。
    let a: Vec<_> = scripted.snapshot();
    let b: Vec<_> = in_memory.snapshot();
    assert_eq!(a.len(), b.len(), "envelope 件数が一致");
    for (ea, eb) in a.iter().zip(b.iter()) {
        assert_eq!(ea.sequence, eb.sequence, "sequence 一致");
        assert_eq!(
            std::mem::discriminant(&ea.event),
            std::mem::discriminant(&eb.event),
            "event 種別列が一致（backpressure なしでは AuditBackpressure は出ない）"
        );
    }
    // AuditBackpressure は 1 件も出ない。
    assert!(!a.iter().any(|e| matches!(
        &e.event,
        AuditEvent::Yielded {
            reason: AuditYieldReason::AuditBackpressure,
            ..
        }
    )));
}
