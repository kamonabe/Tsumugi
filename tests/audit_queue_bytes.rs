//! Phase 6 B-2: audit drain queue byte 会計の統合テスト（tree backend、opt-in）。
//!
//! 設計正本は `docs/determinism-and-audit.md` §10.2（backpressure・queue 上限）/ §11（監査自体の
//! 予算）/ §8（emission 順序）と §14 Slice 5「B-2 実装スライス」（受入基準 B2-AT-01〜10）。
//!
//! queue 上限は test 専用 shim `EngineBuilder::audit_budget_for_test` で小さい
//! `max_pending_bytes` を注入して検証する。queue 上限による yield は sink `Pending` 由来と同じく
//! `AuditBackpressure` の `Yielded`/`Resumed` として記録されるので、常に Ack する sink を使うことで
//! 出てきた `AuditBackpressure` pair が queue 由来であることを特定する。B2-AT-07（既存テストの
//! 無改変緑）は既存の audit 系テストで、B2-AT-10（byte 計上の一致）は `src/audit.rs` /
//! `src/eval.rs` の unit test で担保する。

use std::collections::BTreeSet;
use std::num::NonZeroU128;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tsumugi::{
    AuditBudget, AuditEnvelope, AuditEvent, AuditFailure, AuditSink, AuditYieldReason,
    AuditedOutcome, BudgetConfig, CapabilitySet, CompileOptions, EmbeddingContext, EmbeddingEngine,
    EmbeddingOutcome, EmbeddingRequest, ExecutionId, FakeClock, FilesystemCapability,
    FilesystemRoot, FsOperation, LinkRequest, MonotonicClock, MonotonicInstant, MountName,
    OsDirectoryHandle, ScriptedAuditSink, ScriptedResponse, Source, SourceId, SymlinkPolicy,
};

// -----------------------------------------------------------------------------
// ヘルパ
// -----------------------------------------------------------------------------

/// host call 回数（各 write_file が host-call triple を 1 組出す）。
const ITERATIONS: usize = 20;

/// トップレベルのループから host call を多数行う script（B2-AT-01〜06・08）。
const TOP_LEVEL_LOOP: &str =
    "let i = 0\nwhile i < 20\n    write_file(\"@data/out.txt\", \"x\")\n    i = i + 1\nend\n";

/// 関数本体の中で host call を多数行う script（B2-AT-09）。`work()` の 1 文が返るまで
/// トップレベル境界に戻らないので、その間は queue 上限で yield できない。
const IN_FUNCTION_LOOP: &str = "fn work()\n    let i = 0\n    while i < 20\n        write_file(\"@data/out.txt\", \"x\")\n        i = i + 1\n    end\nend\nwork()\nlet done = 1\n";

/// B2-AT-01 等で使う queue 上限。host-call triple 数組ぶんで、20 反復の途中で複数回達する。
const SMALL_LIMIT: u64 = 2 * 1024;

/// 一意な一時 dir を作る（audit_hostcall.rs と同形）。
fn temp_dir(tag: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "tsg-audit-queue-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

/// 一時 dir を root に Read + Write + Create を grant した capability 集合。
fn fs_capabilities(base: &std::path::Path) -> CapabilitySet {
    let pid = NonZeroU128::new(1).expect("non-zero");
    let mut ops = BTreeSet::new();
    ops.insert(FsOperation::Read);
    ops.insert(FsOperation::Write);
    ops.insert(FsOperation::Create);
    let mount = MountName::new("data").expect("mount");
    let handle = OsDirectoryHandle::new(base.to_path_buf(), pid, SymlinkPolicy::DenyAll);
    let root = FilesystemRoot::new(mount, pid, ops, SymlinkPolicy::DenyAll, Arc::new(handle))
        .expect("root");
    let fs = FilesystemCapability::new(vec![root]).expect("fs");
    CapabilitySet::builder()
        .filesystem(fs)
        .expect("grant filesystem")
        .build()
}

/// `max_pending_bytes` だけを差し替えた audit budget。
fn queue_budget(max_pending_bytes: u64) -> AuditBudget {
    AuditBudget {
        max_pending_bytes,
        ..AuditBudget::default()
    }
}

fn engine_with(sink: Arc<dyn AuditSink>, budget: Option<AuditBudget>) -> EmbeddingEngine {
    let builder = EmbeddingEngine::builder().audit_sink(sink);
    let builder = match budget {
        Some(budget) => builder.audit_budget_for_test(budget),
        None => builder,
    };
    builder.build().expect("build")
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

fn request_with_clock(
    id: u128,
    capabilities: CapabilitySet,
    clock: Arc<dyn MonotonicClock>,
) -> EmbeddingRequest {
    let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
    EmbeddingRequest::new(budget, clock)
        .expect("standard budget は同 domain")
        .with_execution_id(ExecutionId::new(id.try_into().expect("nonzero")))
        .with_capabilities(capabilities)
}

/// script を監査付きで 1 回実行し、outcome と sink が受け取った envelope 列を返す。
fn run_with_clock(
    engine: &EmbeddingEngine,
    sink: &ScriptedAuditSink,
    script: &'static str,
    base: &std::path::Path,
    clock: Arc<dyn MonotonicClock>,
) -> (AuditedOutcome, Vec<AuditEnvelope>) {
    let compiled = engine
        .compile(
            Source::new(SourceId::new("m").expect("valid source id"), script),
            &CompileOptions {
                retain_source: true,
            },
        )
        .expect("compile");
    let linked = engine.link(&compiled, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(engine);
    let audited = engine.run_audited(
        &linked,
        &mut ctx,
        request_with_clock(7, fs_capabilities(base), clock),
    );
    (audited, sink.snapshot())
}

/// 常に Ack する sink と小さい queue 上限で script を実行する。
fn run_with_limit(
    script: &'static str,
    limit: Option<u64>,
    tag: &str,
) -> (AuditedOutcome, Vec<AuditEnvelope>) {
    let sink = Arc::new(ScriptedAuditSink::new([]));
    let engine = engine_with(sink.clone() as Arc<dyn AuditSink>, limit.map(queue_budget));
    let base = temp_dir(tag);
    let result = run_with_clock(&engine, &sink, script, &base, Arc::new(FakeClock::new()));
    let _ = std::fs::remove_dir_all(&base);
    result
}

fn assert_completed(audited: &AuditedOutcome) {
    assert!(
        matches!(audited.outcome(), Some(EmbeddingOutcome::Completed { .. })),
        "Completed を期待: {audited:?}"
    );
}

fn is_backpressure_yield(event: &AuditEvent) -> bool {
    matches!(
        event,
        AuditEvent::Yielded {
            reason: AuditYieldReason::AuditBackpressure,
            ..
        }
    )
}

fn backpressure_yield_count(envelopes: &[AuditEnvelope]) -> usize {
    envelopes
        .iter()
        .filter(|e| is_backpressure_yield(&e.event))
        .count()
}

/// sequence が 0 始まり gap・重複なし、先頭 Started・末尾 Terminal（§8 規則1/9）。
fn assert_sequence_integrity(envelopes: &[AuditEnvelope]) {
    assert!(matches!(
        envelopes.first().expect("non-empty").event,
        AuditEvent::ExecutionStarted { .. }
    ));
    for (i, env) in envelopes.iter().enumerate() {
        assert_eq!(env.sequence as usize, i, "sequence は 0 始まり gap なし");
    }
    assert!(matches!(
        envelopes.last().expect("non-empty").event,
        AuditEvent::Terminal { .. }
    ));
    let terminals = envelopes
        .iter()
        .filter(|e| matches!(e.event, AuditEvent::Terminal { .. }))
        .count();
    assert_eq!(terminals, 1, "Terminal はちょうど 1 件で最後（§8 規則9）");
}

/// host-call triple が `ITERATIONS` 組すべて揃い、operation_id が 0 始まりで連続する
/// （queue を捨てない・上書きしない、§10.2）。
fn assert_host_call_triples_complete(envelopes: &[AuditEnvelope]) {
    let mut started = Vec::new();
    let mut decisions = 0usize;
    let mut finished = Vec::new();
    for env in envelopes {
        match &env.event {
            AuditEvent::HostCallStarted { operation_id, .. } => started.push(*operation_id),
            AuditEvent::CapabilityDecision { .. } => decisions += 1,
            AuditEvent::HostCallFinished { operation_id, .. } => finished.push(*operation_id),
            _ => {}
        }
    }
    let expected: Vec<u64> = (0..ITERATIONS as u64).collect();
    assert_eq!(started, expected, "HostCallStarted が欠落・重複なく揃う");
    assert_eq!(finished, expected, "HostCallFinished が欠落・重複なく揃う");
    assert_eq!(decisions, ITERATIONS, "CapabilityDecision が欠落なく揃う");
}

/// Yielded/Resumed を LIFO で突き合わせ、pair が poll_index と reason を共有し、Yielded の
/// poll_index が厳密単調増加することを検証する（§8/§15.3）。queue 上限 yield の flush 中に sink
/// `Pending` が起きるとネストする（B-1 と同じ規律）。
fn assert_yield_pairs_lifo(envelopes: &[AuditEnvelope]) {
    let mut stack: Vec<(AuditYieldReason, u64)> = Vec::new();
    let mut last_poll_index: Option<u64> = None;
    for env in envelopes {
        match &env.event {
            AuditEvent::Yielded {
                reason, poll_index, ..
            } => {
                if let Some(prev) = last_poll_index {
                    assert!(*poll_index > prev, "Yielded の poll_index は厳密単調増加");
                }
                last_poll_index = Some(*poll_index);
                stack.push((reason.clone(), *poll_index));
            }
            AuditEvent::Resumed {
                previous_reason,
                poll_index,
                ..
            } => {
                let (reason, index) = stack.pop().expect("Resumed は対の Yielded の後に出る");
                assert_eq!(
                    previous_reason, &reason,
                    "Resumed.previous_reason は対の Yielded.reason と一致"
                );
                assert_eq!(*poll_index, index, "pair は poll_index を共有");
            }
            _ => {}
        }
    }
    assert!(stack.is_empty(), "全 Yielded が Resumed で閉じている");
}

// -----------------------------------------------------------------------------
// B2-AT-01: queue 上限で AuditBackpressure pair が出る（sink 即 Ack でも出る）
// -----------------------------------------------------------------------------

#[test]
fn b2_at_01_queue_limit_yields_backpressure_pair_even_with_immediate_ack() {
    let (audited, envelopes) = run_with_limit(TOP_LEVEL_LOOP, Some(SMALL_LIMIT), "at01");
    assert_completed(&audited);

    // sink は常に Ack するので、AuditBackpressure はすべて queue 上限由来。
    let yields = backpressure_yield_count(&envelopes);
    assert!(
        yields >= 2,
        "20 反復の途中で queue 上限に複数回達する: {yields} 回"
    );
    assert_yield_pairs_lifo(&envelopes);

    // sink 即 Ack ではネストしないので、各 Yielded の次の yield/resume event は対の Resumed。
    let yr: Vec<&AuditEvent> = envelopes
        .iter()
        .map(|e| &e.event)
        .filter(|e| matches!(e, AuditEvent::Yielded { .. } | AuditEvent::Resumed { .. }))
        .collect();
    for pair in yr.chunks(2) {
        match pair {
            [
                AuditEvent::Yielded {
                    reason: AuditYieldReason::AuditBackpressure,
                    poll_index: y,
                    ..
                },
                AuditEvent::Resumed {
                    previous_reason: AuditYieldReason::AuditBackpressure,
                    poll_index: r,
                    ..
                },
            ] => assert_eq!(y, r),
            other => panic!("Yielded/Resumed(AuditBackpressure) の交互を期待: {other:?}"),
        }
    }
}

// -----------------------------------------------------------------------------
// B2-AT-02: queue 上限到達時も event を捨てない・上書きしない
// -----------------------------------------------------------------------------

#[test]
fn b2_at_02_queue_limit_does_not_drop_or_overwrite_events() {
    let (audited, envelopes) = run_with_limit(TOP_LEVEL_LOOP, Some(SMALL_LIMIT), "at02");
    assert_completed(&audited);
    assert!(backpressure_yield_count(&envelopes) >= 1);
    assert_sequence_integrity(&envelopes);
    assert_host_call_triples_complete(&envelopes);
}

// -----------------------------------------------------------------------------
// B2-AT-03: §8 規則7/8/9
// -----------------------------------------------------------------------------

#[test]
fn b2_at_03_queue_yield_respects_rules_7_8_9() {
    let (audited, envelopes) = run_with_limit(TOP_LEVEL_LOOP, Some(SMALL_LIMIT), "at03");
    assert_completed(&audited);
    assert!(backpressure_yield_count(&envelopes) >= 1);
    // 規則9: Terminal が最後。
    assert_sequence_integrity(&envelopes);

    // 規則7: Yielded の後、対の Resumed までに BudgetCharged は出ない（この slice ぶんの
    // BudgetCharged は Yielded より前に flush 済み）。
    // 規則8: Yielded と対の Resumed の間に、次の work（host call）の event は出ない。
    let mut open_yield = false;
    for env in &envelopes {
        match &env.event {
            AuditEvent::Yielded { .. } => open_yield = true,
            AuditEvent::Resumed { .. } => open_yield = false,
            AuditEvent::BudgetCharged { .. } => {
                assert!(!open_yield, "規則7: BudgetCharged は Yielded より前に置く")
            }
            AuditEvent::HostCallStarted { .. }
            | AuditEvent::CapabilityDecision { .. }
            | AuditEvent::HostCallFinished { .. } => {
                assert!(!open_yield, "規則8: Resumed は次の work より前に置く")
            }
            _ => {}
        }
    }
}

// -----------------------------------------------------------------------------
// B2-AT-04: queue 上限 yield と sink Pending の重なり
// -----------------------------------------------------------------------------

#[test]
fn b2_at_04_queue_yield_and_sink_pending_nest_and_complete() {
    // 既定 slice fuel（u64::MAX）では最初の flush は queue 上限 yield の境界で起きる。
    // Started の次に配送される envelope（その flush の先頭）で sink が 2 回 Pending を返すので、
    // queue 由来の Yielded の内側に sink 由来の Yielded/Resumed がネストする。
    let sink = Arc::new(ScriptedAuditSink::new([
        ScriptedResponse::AckCorrect,
        ScriptedResponse::PendingThenAck { pending_times: 2 },
    ]));
    let engine = engine_with(
        sink.clone() as Arc<dyn AuditSink>,
        Some(queue_budget(SMALL_LIMIT)),
    );
    let base = temp_dir("at04");
    let (audited, envelopes) = run_with_clock(
        &engine,
        &sink,
        TOP_LEVEL_LOOP,
        &base,
        Arc::new(FakeClock::new()),
    );
    let _ = std::fs::remove_dir_all(&base);

    assert_completed(&audited);
    assert!(
        backpressure_yield_count(&envelopes) >= 2,
        "queue 由来と sink 由来の両方の AuditBackpressure yield が出る"
    );
    assert_yield_pairs_lifo(&envelopes);
    assert_sequence_integrity(&envelopes);
    assert_host_call_triples_complete(&envelopes);

    // ネストの確認: 先頭の 2 つの yield/resume event はどちらも AuditBackpressure の Yielded。
    let yr: Vec<&AuditEvent> = envelopes
        .iter()
        .map(|e| &e.event)
        .filter(|e| matches!(e, AuditEvent::Yielded { .. } | AuditEvent::Resumed { .. }))
        .collect();
    assert!(
        is_backpressure_yield(yr[0]) && is_backpressure_yield(yr[1]),
        "queue Yielded の内側に sink Yielded がネストする: {:?}",
        &yr[..2]
    );
}

// -----------------------------------------------------------------------------
// B2-AT-05: queue 上限 yield 中の永続 Pending → BackpressureDeadline
// -----------------------------------------------------------------------------

/// `now()` を呼ぶたびに `tick_ns` だけ進む clock（audit_backpressure.rs と同形）。
struct AutoAdvanceClock {
    clock_id: u64,
    now_ns: AtomicU64,
    tick_ns: u64,
}

impl AutoAdvanceClock {
    fn new(tick_ns: u64) -> Self {
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
fn b2_at_05_persistent_pending_during_queue_flush_is_backpressure_deadline() {
    let sink = Arc::new(ScriptedAuditSink::new([
        ScriptedResponse::AckCorrect,
        ScriptedResponse::PendingForever,
    ]));
    let engine = engine_with(
        sink.clone() as Arc<dyn AuditSink>,
        Some(queue_budget(SMALL_LIMIT)),
    );
    let base = temp_dir("at05");
    // 1 tick = 1 ms。script 実行中も clock は進むが、30 s deadline に達するのは永続 Pending の
    // 再送 loop の中になる（host call 20 回ぶんの now() では deadline に届かない）。
    let (audited, envelopes) = run_with_clock(
        &engine,
        &sink,
        TOP_LEVEL_LOOP,
        &base,
        Arc::new(AutoAdvanceClock::new(1_000_000)),
    );
    let _ = std::fs::remove_dir_all(&base);

    match &audited {
        AuditedOutcome::AuditFailed { failure, .. } => assert_eq!(
            *failure,
            AuditFailure::BackpressureDeadline,
            "永続 Pending は BackpressureDeadline（§10.2、script の Deadline より優先）"
        ),
        other => panic!("AuditFailed(BackpressureDeadline) を期待したが {other:?}"),
    }
    assert!(audited.outcome().is_none(), "success な outcome を持たない");
    // 永続 Pending に入ったのは queue 上限 yield の flush 中（それ以前に flush は起きない）。
    assert!(
        envelopes
            .iter()
            .all(|e| !matches!(e.event, AuditEvent::Terminal { .. })),
        "Terminal は配送されない"
    );
}

// -----------------------------------------------------------------------------
// B2-AT-06: 単一 envelope が上限を超える設定（0 を含む）でも進行不能にならない
// -----------------------------------------------------------------------------

#[test]
fn b2_at_06_tiny_or_zero_limit_makes_progress_without_livelock() {
    for limit in [0u64, 1, 64] {
        let (audited, envelopes) =
            run_with_limit(TOP_LEVEL_LOOP, Some(limit), &format!("at06-{limit}"));
        assert_completed(&audited);
        assert_sequence_integrity(&envelopes);
        assert_host_call_triples_complete(&envelopes);
        assert_yield_pairs_lifo(&envelopes);
        // 前進保証: queue 上限の判定は slice 内で 1 文以上（または 1 反復境界以上）進めた後だけ
        // なので、yield 回数は進めたトップレベルの文/反復境界の数で抑えられる（1 反復あたり
        // 本体 2 文 ＋ 本体 frame の出入り 2 ＝ 4、これに先頭の let・while・末尾を足した程度）。
        // yield/resume だけを繰り返す livelock なら有界にならず終了しない。
        let yields = backpressure_yield_count(&envelopes);
        assert!(
            (1..=5 * ITERATIONS).contains(&yields),
            "limit={limit}: yield 回数は有界: {yields}"
        );
    }
}

// -----------------------------------------------------------------------------
// B2-AT-07（非破壊の一部）: 既定 budget（1 MiB）では queue 上限に達しない
// -----------------------------------------------------------------------------

#[test]
fn b2_at_07_default_budget_does_not_emit_queue_backpressure() {
    let (audited, envelopes) = run_with_limit(TOP_LEVEL_LOOP, None, "at07");
    assert_completed(&audited);
    assert_eq!(
        backpressure_yield_count(&envelopes),
        0,
        "既定 1 MiB では 20 反復の host call で queue 上限に達しない"
    );
    assert!(
        envelopes
            .iter()
            .all(|e| !matches!(e.event, AuditEvent::Yielded { .. })),
        "既定 slice fuel・既定 budget では yield event が出ない（B-1 と同じ envelope 列）"
    );
    assert_sequence_integrity(&envelopes);
    assert_host_call_triples_complete(&envelopes);
}

// -----------------------------------------------------------------------------
// B2-AT-08: 決定性（同条件 100 回で yield 位置と envelope 列が一致）
// -----------------------------------------------------------------------------

#[test]
fn b2_at_08_queue_yield_positions_are_deterministic() {
    let (first_outcome, first) = run_with_limit(TOP_LEVEL_LOOP, Some(SMALL_LIMIT), "at08");
    assert_completed(&first_outcome);
    assert!(backpressure_yield_count(&first) >= 1);
    for round in 1..100 {
        let (outcome, envelopes) =
            run_with_limit(TOP_LEVEL_LOOP, Some(SMALL_LIMIT), &format!("at08-{round}"));
        assert_completed(&outcome);
        // `ExecutionStarted` は request budget の deadline に実行ごとの `FakeClock` の clock_id を
        // 載せるため、先頭（sequence 0）だけ種別の一致に留め、以降の event 列を完全一致で比べる。
        assert!(matches!(
            envelopes[0].event,
            AuditEvent::ExecutionStarted { .. }
        ));
        let events: Vec<&AuditEvent> = envelopes[1..].iter().map(|e| &e.event).collect();
        let expected: Vec<&AuditEvent> = first[1..].iter().map(|e| &e.event).collect();
        assert_eq!(events, expected, "round {round}: envelope 列が一致する");
    }
}

// -----------------------------------------------------------------------------
// B2-AT-09: 関数本体の中では yield せず、戻った後のトップレベル境界で yield する
// -----------------------------------------------------------------------------

#[test]
fn b2_at_09_function_body_is_soft_limited_until_return() {
    let (audited, envelopes) = run_with_limit(IN_FUNCTION_LOOP, Some(SMALL_LIMIT), "at09");
    assert_completed(&audited);
    assert_sequence_integrity(&envelopes);
    assert_host_call_triples_complete(&envelopes);
    assert_yield_pairs_lifo(&envelopes);

    // queue は work() の中で上限を大きく超えるが、yield はトップレベル境界まで起きない。
    // したがって最初の AuditBackpressure Yielded は、最後の HostCallFinished より後に出る。
    let first_yield = envelopes
        .iter()
        .position(|e| is_backpressure_yield(&e.event))
        .expect("work() から戻った後のトップレベル境界で queue 上限 yield が起きる");
    let last_finished = envelopes
        .iter()
        .rposition(|e| matches!(e.event, AuditEvent::HostCallFinished { .. }))
        .expect("host call が行われる");
    assert!(
        first_yield > last_finished,
        "関数内では yield しない（first_yield={first_yield}, last_finished={last_finished}）"
    );
    assert_eq!(
        backpressure_yield_count(&envelopes),
        1,
        "work() の 1 文が返った後の境界で 1 回だけ yield する"
    );
}

// -----------------------------------------------------------------------------
// 回帰（§8 規則8）: slice fuel yield と host call の組み合わせでも Resumed が次の work より前
// -----------------------------------------------------------------------------

/// B-2 以前は host-call triple と Yielded/Resumed を別 buffer から「host-call 群 → yield/resume 群」
/// の固定順で drain していたため、resume 後の slice で起きた host call が対の `Resumed` より前に
/// 配送されていた（queue 上限 yield に限らず SliceFuel yield でも起きる）。temporal 順の単一
/// buffer へ直したことを、queue 上限を使わない SliceFuel yield で固定する。
#[test]
fn slice_fuel_yield_with_host_calls_keeps_resumed_before_next_work() {
    let sink = Arc::new(ScriptedAuditSink::new([]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .audit_slice_fuel(4)
        .build()
        .expect("build");
    let base = temp_dir("rule8");
    let (audited, envelopes) = run_with_clock(
        &engine,
        &sink,
        TOP_LEVEL_LOOP,
        &base,
        Arc::new(FakeClock::new()),
    );
    let _ = std::fs::remove_dir_all(&base);
    assert_completed(&audited);
    assert_sequence_integrity(&envelopes);
    assert_host_call_triples_complete(&envelopes);
    assert_yield_pairs_lifo(&envelopes);
    assert!(
        envelopes.iter().any(|e| matches!(
            e.event,
            AuditEvent::Yielded {
                reason: AuditYieldReason::SliceFuelExhausted,
                ..
            }
        )),
        "小さい slice fuel で SliceFuel yield が起きる"
    );

    let mut open_yield = false;
    for env in &envelopes {
        match &env.event {
            AuditEvent::Yielded { .. } => open_yield = true,
            AuditEvent::Resumed { .. } => open_yield = false,
            AuditEvent::HostCallStarted { .. }
            | AuditEvent::CapabilityDecision { .. }
            | AuditEvent::HostCallFinished { .. } => {
                assert!(!open_yield, "規則8: Resumed は次の work より前に置く")
            }
            _ => {}
        }
    }
}
