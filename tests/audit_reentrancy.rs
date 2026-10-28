//! fix-audit-sink-reentrancy-guard: sink callback 中の同じ Engine への再入検出の統合テスト。
//!
//! 設計正本は `docs/determinism-and-audit.md` §10（thread-local reentrancy guard）・§15.3 と、
//! 同ファイルの「fix-audit-sink-reentrancy-guard 実装スライス」小節。受入基準 RG-AT-01〜05 を
//! tree backend で確かめる。crate root の公開型だけを使い、sink は `OnceLock<Arc<Engine>>` で
//! 後から Engine を受け取って callback の中から呼ぶ。

use std::collections::BTreeSet;
use std::num::NonZeroU128;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tsumugi::{
    AuditAck, AuditEnvelope, AuditEvent, AuditFailure, AuditSink, AuditSubmit, AuditWaker,
    AuditedOutcome, BudgetConfig, CapabilitySet, CompileDiagnosticCode, CompileOptions,
    EmbeddingCompiledScript, EmbeddingContext, EmbeddingEngine, EmbeddingOutcome, EmbeddingRequest,
    ExecutionId, FakeClock, FilesystemCapability, FilesystemRoot, FsOperation, LinkError,
    LinkRequest, LinkedScript, MonotonicClock, MountName, OsDirectoryHandle, Source, SourceId,
    SymlinkPolicy, TerminalOutcome,
};

// -----------------------------------------------------------------------------
// ヘルパ
// -----------------------------------------------------------------------------

/// host-call triple・BudgetCharged・Terminal の配送点をすべて通る script。
/// 反復があり、小さい slice fuel で `Yielded` / `Resumed` を配送する script。
const LOOP_SCRIPT: &str =
    "let total = 0\nlet i = 0\nwhile i < 30\n    total = total + i\n    i = i + 1\nend\n";

const SCRIPT: &str =
    "write_file(\"@data/out.txt\", \"hello\")\nlet c = read_file(\"@data/out.txt\")\n";

fn temp_dir(tag: &str) -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "tsg-audit-reentrancy-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

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
    CapabilitySet::builder()
        .filesystem(FilesystemCapability::new(vec![root]).expect("fs"))
        .expect("grant filesystem")
        .build()
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

fn run_request(id: u128, capabilities: CapabilitySet) -> EmbeddingRequest {
    let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
    let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
    EmbeddingRequest::new(budget, clock)
        .expect("standard budget は同 domain")
        .with_execution_id(ExecutionId::new(id.try_into().expect("nonzero")))
        .with_capabilities(capabilities)
}

fn compile(engine: &EmbeddingEngine, text: &'static str) -> EmbeddingCompiledScript {
    engine
        .compile(
            Source::new(SourceId::new("m").expect("source id"), text),
            &CompileOptions {
                retain_source: true,
            },
        )
        .expect("compile")
}

/// 失敗するまで時間がかかる deadlock を `recv_timeout` で検出する（RG-AT-05）。
/// 評価器は再帰するので 8 MiB の thread で回す。
fn within_timeout(body: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            body();
            let _ = tx.send(());
        })
        .expect("spawn");
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(()) => handle.join().expect("test thread"),
        Err(_) => {
            // thread が panic した場合は tx が drop されて Disconnected になる。
            match handle.join() {
                Err(payload) => std::panic::resume_unwind(payload),
                Ok(()) => panic!("10 秒以内に戻らなかった（deadlock の疑い）"),
            }
        }
    }
}

/// callback から呼ぶ closure を持つ記録用 sink。`on_submit` には 1 始まりの submit 回数を渡す。
struct FnSink {
    log: Mutex<Vec<AuditEnvelope>>,
    count: AtomicUsize,
    on_submit: Box<dyn Fn(usize) + Send + Sync>,
}

impl FnSink {
    fn new(on_submit: impl Fn(usize) + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            log: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
            on_submit: Box::new(on_submit),
        })
    }

    fn log(&self) -> Vec<AuditEnvelope> {
        self.log.lock().expect("log").clone()
    }

    fn submits(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl AuditSink for FnSink {
    fn submit(&self, batch: Arc<[AuditEnvelope]>, _waker: AuditWaker) -> AuditSubmit {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        self.log.lock().expect("log").extend(batch.iter().cloned());
        (self.on_submit)(n);
        let last = batch.last().expect("non-empty batch");
        AuditSubmit::Ack(AuditAck {
            execution_id: last.execution_id,
            through_sequence: last.sequence,
        })
    }
}

/// sink が後から受け取る Engine と、callback から使う script。
struct Target {
    engine: Arc<EmbeddingEngine>,
    compiled: EmbeddingCompiledScript,
    linked: LinkedScript,
    caps_dir: std::path::PathBuf,
}

type Slot = Arc<OnceLock<Arc<Target>>>;

/// `sink` を持つ Engine を作って slot に入れる。
fn build_target(slot: &Slot, sink: Arc<dyn AuditSink>, tag: &str) -> Arc<Target> {
    build_target_with(slot, sink, tag, SCRIPT, None)
}

/// script と `audit_slice_fuel` を指定して作る（`Yielded` / `Resumed` の配送点を通すため）。
fn build_target_with(
    slot: &Slot,
    sink: Arc<dyn AuditSink>,
    tag: &str,
    script: &'static str,
    slice_fuel: Option<u64>,
) -> Arc<Target> {
    let mut builder = EmbeddingEngine::builder().audit_sink(sink);
    if let Some(fuel) = slice_fuel {
        builder = builder.audit_slice_fuel(fuel);
    }
    let engine = Arc::new(builder.build().expect("build"));
    let compiled = compile(&engine, script);
    let linked = engine.link(&compiled, link_request()).expect("link");
    let target = Arc::new(Target {
        engine,
        compiled,
        linked,
        caps_dir: temp_dir(tag),
    });
    slot.set(Arc::clone(&target)).ok().expect("slot once");
    target
}

fn no_sink_engine() -> Arc<EmbeddingEngine> {
    Arc::new(EmbeddingEngine::builder().build().expect("build"))
}

/// `target` の 6 入口を callback の中から呼び、拒否の値が期待どおりでなかったものを返す。
/// 空なら全入口が RG-AT-02 の型で拒否され、渡した context は poison されていない。
fn probe_all_entries(target: &Target) -> Vec<String> {
    let mut bad = Vec::new();
    let engine = &*target.engine;

    match engine.compile(
        Source::new(SourceId::new("p").expect("source id"), "let x = 1\n"),
        &CompileOptions::default(),
    ) {
        Err(errors)
            if errors.diagnostics.len() == 1
                && errors.diagnostics[0].code == CompileDiagnosticCode::InternalFault => {}
        other => bad.push(format!("compile: {other:?}")),
    }
    match engine.link(&target.compiled, link_request()) {
        Err(LinkError::InternalFailure { .. }) => {}
        other => bad.push(format!("link: {other:?}")),
    }
    match engine.link_for_repl(&target.compiled, link_request()) {
        Err(LinkError::InternalFailure { .. }) => {}
        other => bad.push(format!("link_for_repl: {other:?}")),
    }

    let mut ctx = EmbeddingContext::new(engine);
    match engine.run(
        &target.linked,
        &mut ctx,
        run_request(900, fs_capabilities(&target.caps_dir)),
    ) {
        EmbeddingOutcome::InternalFailure { .. } => {}
        other => bad.push(format!("run: {other:?}")),
    }
    if ctx.is_poisoned() {
        bad.push("run: context が poison された".into());
    }

    let mut ctx = EmbeddingContext::new(engine);
    match engine.run_for_repl(
        &target.linked,
        &mut ctx,
        run_request(901, fs_capabilities(&target.caps_dir)),
    ) {
        EmbeddingOutcome::InternalFailure { .. } => {}
        other => bad.push(format!("run_for_repl: {other:?}")),
    }
    if ctx.is_poisoned() {
        bad.push("run_for_repl: context が poison された".into());
    }

    let mut ctx = EmbeddingContext::new(engine);
    match engine.run_audited(
        &target.linked,
        &mut ctx,
        run_request(902, fs_capabilities(&target.caps_dir)),
    ) {
        AuditedOutcome::Outcome(outcome)
            if matches!(*outcome, EmbeddingOutcome::InternalFailure { .. }) => {}
        other => bad.push(format!("run_audited: {other:?}")),
    }
    if ctx.is_poisoned() {
        bad.push("run_audited: context が poison された".into());
    }
    bad
}

/// 外側の実行で、script が実際に実行された証拠（output file）。
fn outer_wrote_file(target: &Target) -> bool {
    target.caps_dir.join("out.txt").exists()
}

fn run_outer(target: &Target, id: u128) -> AuditedOutcome {
    let mut ctx = EmbeddingContext::new(&target.engine);
    target.engine.run_audited(
        &target.linked,
        &mut ctx,
        run_request(id, fs_capabilities(&target.caps_dir)),
    )
}

fn is_audit_failed_sink(outcome: &AuditedOutcome) -> bool {
    matches!(
        outcome,
        AuditedOutcome::AuditFailed {
            failure: AuditFailure::Sink,
            ..
        }
    )
}

/// envelope 列を比較用の文字列にする。`FakeClock` の `clock_id` は clock を作るたびに変わる
/// （process 内の通し番号）ので、再入の有無と無関係なその値だけを落とす。
fn normalized(log: &[AuditEnvelope]) -> String {
    let text = format!("{log:?}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(pos) = rest.find("clock_id: ") {
        let (head, tail) = rest.split_at(pos + "clock_id: ".len());
        out.push_str(head);
        let digits = tail.chars().take_while(char::is_ascii_digit).count();
        out.push('N');
        rest = &tail[digits..];
    }
    out.push_str(rest);
    out
}

/// 再入しない基準の実行を 1 回行い、sink の envelope 列と submit 回数を返す。
fn baseline(tag: &str) -> (Vec<AuditEnvelope>, usize) {
    let slot: Slot = Arc::new(OnceLock::new());
    let sink = FnSink::new(|_| {});
    let target = build_target(&slot, sink.clone(), tag);
    let outcome = run_outer(&target, 7);
    assert!(
        matches!(outcome.outcome(), Some(EmbeddingOutcome::Completed { .. })),
        "baseline は Completed のはず: {:?}",
        outcome.outcome()
    );
    (sink.log(), sink.submits())
}

// -----------------------------------------------------------------------------
// RG-AT-01: Started の submit 中の再入
// -----------------------------------------------------------------------------

#[test]
fn rg_at_01_reentry_during_started_submit_fails_closed_without_running_script() {
    within_timeout(|| {
        let slot: Slot = Arc::new(OnceLock::new());
        let probe_failures: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
        let sink = {
            let slot = Arc::clone(&slot);
            let probe_failures = Arc::clone(&probe_failures);
            FnSink::new(move |n| {
                if n == 1 {
                    let target = slot.get().expect("target");
                    *probe_failures.lock().expect("probe") = Some(probe_all_entries(target));
                }
            })
        };
        let target = build_target(&slot, sink.clone(), "rg01");

        let outcome = run_outer(&target, 11);

        // 再入した 6 入口はすべて拒否された（fresh context の run_audited を含む）。
        let failures = probe_failures
            .lock()
            .expect("probe")
            .take()
            .expect("callback が呼ばれた");
        assert!(failures.is_empty(), "拒否の値が期待と違う: {failures:?}");
        // 再入した呼び出しは script を 1 文も実行しない（output file が無い）。
        assert!(!outer_wrote_file(&target));

        // 外側の実行は AuditFailed { Sink }（sink は Ack を返したが Reentrant として扱う）。
        assert!(is_audit_failed_sink(&outcome), "{outcome:?}");
        // sink が受け取るのは Started と Terminal(AuditFailure) だけ。
        let log = sink.log();
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(matches!(log[0].event, AuditEvent::ExecutionStarted { .. }));
        assert!(matches!(
            log[1].event,
            AuditEvent::Terminal {
                outcome: TerminalOutcome::AuditFailure,
                ..
            }
        ));
        assert_eq!(sink.submits(), 2, "再入した呼び出しは submit に届かない");
    });
}

// -----------------------------------------------------------------------------
// RG-AT-02: 全入口と全配送点
// -----------------------------------------------------------------------------

#[test]
fn rg_at_02_every_delivery_point_fails_closed_even_when_sink_acks() {
    within_timeout(|| {
        let (_, total_submits) = baseline("rg02-base");
        // Started + host-call triple（2 回分）+ BudgetCharged + Terminal を通る script。
        assert!(
            total_submits >= 4,
            "配送点が少なすぎる（{total_submits} 回）"
        );

        for reenter_at in 1..=total_submits {
            let slot: Slot = Arc::new(OnceLock::new());
            let probe_failures: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
            let sink = {
                let slot = Arc::clone(&slot);
                let probe_failures = Arc::clone(&probe_failures);
                FnSink::new(move |n| {
                    if n == reenter_at {
                        let target = slot.get().expect("target");
                        *probe_failures.lock().expect("probe") = Some(probe_all_entries(target));
                    }
                })
            };
            let target = build_target(&slot, sink.clone(), "rg02");

            let outcome = run_outer(&target, 20 + reenter_at as u128);

            let failures = probe_failures
                .lock()
                .expect("probe")
                .take()
                .unwrap_or_else(|| panic!("{reenter_at} 回目の submit で callback が呼ばれた"));
            assert!(
                failures.is_empty(),
                "{reenter_at} 回目の submit での拒否が期待と違う: {failures:?}"
            );
            assert!(
                is_audit_failed_sink(&outcome),
                "{reenter_at} 回目の submit で再入したのに fail-closed でない: {outcome:?}"
            );
        }
    });
}

#[test]
fn rg_at_02_yielded_and_resumed_deliveries_fail_closed() {
    within_timeout(|| {
        // 基準の実行で、Yielded / Resumed が配送される submit の回を数える。
        let slot: Slot = Arc::new(OnceLock::new());
        let sink = FnSink::new(|_| {});
        let target = build_target_with(&slot, sink.clone(), "rg02y-base", LOOP_SCRIPT, Some(16));
        assert!(
            matches!(
                run_outer(&target, 7).outcome(),
                Some(EmbeddingOutcome::Completed { .. })
            ),
            "基準の実行は Completed"
        );
        let log = sink.log();
        let yield_resume_batches: Vec<usize> = log
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                matches!(
                    e.event,
                    AuditEvent::Yielded { .. } | AuditEvent::Resumed { .. }
                )
            })
            .map(|(i, _)| i)
            .collect();
        assert!(
            !yield_resume_batches.is_empty(),
            "Yielded / Resumed が配送されない script では意味がない"
        );
        let total_submits = sink.submits();

        for reenter_at in 1..=total_submits {
            let slot: Slot = Arc::new(OnceLock::new());
            let probe_failures: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
            let sink = {
                let slot = Arc::clone(&slot);
                let probe_failures = Arc::clone(&probe_failures);
                FnSink::new(move |n| {
                    if n == reenter_at {
                        *probe_failures.lock().expect("probe") =
                            Some(probe_all_entries(slot.get().expect("target")));
                    }
                })
            };
            let target = build_target_with(&slot, sink.clone(), "rg02y", LOOP_SCRIPT, Some(16));
            let outcome = run_outer(&target, 70 + reenter_at as u128);
            let failures = probe_failures
                .lock()
                .expect("probe")
                .take()
                .unwrap_or_else(|| panic!("{reenter_at} 回目の submit で callback が呼ばれた"));
            assert!(failures.is_empty(), "{reenter_at} 回目: {failures:?}");
            assert!(
                is_audit_failed_sink(&outcome),
                "{reenter_at} 回目: {outcome:?}"
            );
        }
    });
}

/// `Pending` を返す sink。`pend_at` 回目の submit で `Pending` を返し、`reenter_on` 回目の
/// submit で再入する。`reenter_on == pend_at` なら、再入した submit が `Pending` を返す。
/// それ以外の submit は `Ack` を返す（`Pending` の次の submit は backpressure の再送）。
struct PendingSink {
    slot: Slot,
    count: AtomicUsize,
    pend_at: usize,
    reenter_on: usize,
    probe_failures: Mutex<Option<Vec<String>>>,
}

impl AuditSink for PendingSink {
    fn submit(&self, batch: Arc<[AuditEnvelope]>, _waker: AuditWaker) -> AuditSubmit {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.reenter_on {
            *self.probe_failures.lock().expect("probe") =
                Some(probe_all_entries(self.slot.get().expect("target")));
        }
        if n == self.pend_at {
            return AuditSubmit::Pending(tsumugi::AuditTicket::new());
        }
        let last = batch.last().expect("non-empty batch");
        AuditSubmit::Ack(AuditAck {
            execution_id: last.execution_id,
            through_sequence: last.sequence,
        })
    }
}

#[test]
fn rg_at_02_backpressure_resend_and_pending_return_fail_closed() {
    within_timeout(|| {
        // (pend_at, reenter_on): 再送の submit で再入する／再入した submit が Pending を返す。
        for (pend_at, reenter_on) in [(2, 3), (3, 4), (2, 2), (3, 3)] {
            let slot: Slot = Arc::new(OnceLock::new());
            let sink = Arc::new(PendingSink {
                slot: Arc::clone(&slot),
                count: AtomicUsize::new(0),
                pend_at,
                reenter_on,
                probe_failures: Mutex::new(None),
            });
            let target = build_target(&slot, sink.clone(), "rg02p");
            let outcome = run_outer(&target, 80 + pend_at as u128);
            let failures = sink
                .probe_failures
                .lock()
                .expect("probe")
                .take()
                .unwrap_or_else(|| panic!("({pend_at},{reenter_on}): callback が呼ばれた"));
            assert!(
                failures.is_empty(),
                "({pend_at},{reenter_on}): {failures:?}"
            );
            assert!(
                is_audit_failed_sink(&outcome),
                "({pend_at},{reenter_on}): {outcome:?}"
            );
        }
    });
}

// -----------------------------------------------------------------------------
// RG-AT-03: 別の Engine は許す
// -----------------------------------------------------------------------------

#[test]
fn rg_at_03_calling_another_engine_from_callback_is_allowed() {
    within_timeout(|| {
        let (baseline_log, _) = baseline("rg03-base");

        // sink なしの別 Engine と、sink ありの別 Engine。
        for with_sink in [false, true] {
            let other_engine = if with_sink {
                Arc::new(
                    EmbeddingEngine::builder()
                        .audit_sink(
                            Arc::new(tsumugi::InMemoryAuditSink::new()) as Arc<dyn AuditSink>
                        )
                        .build()
                        .expect("build"),
                )
            } else {
                no_sink_engine()
            };
            let other_dir = temp_dir("rg03-other");
            let inner_results: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));

            let sink = {
                let inner_results = Arc::clone(&inner_results);
                FnSink::new(move |n| {
                    if n != 1 {
                        return;
                    }
                    let compiled = compile(&other_engine, SCRIPT);
                    let linked = other_engine
                        .link(&compiled, link_request())
                        .expect("別 Engine の link は許される");
                    let mut ctx = EmbeddingContext::new(&other_engine);
                    let outcome = other_engine.run_audited(
                        &linked,
                        &mut ctx,
                        run_request(300, fs_capabilities(&other_dir)),
                    );
                    inner_results.lock().expect("inner").push(matches!(
                        outcome.outcome(),
                        Some(EmbeddingOutcome::Completed { .. })
                    ));
                })
            };
            let slot: Slot = Arc::new(OnceLock::new());
            let target = build_target(&slot, sink.clone(), "rg03");
            let outcome = run_outer(&target, 7);

            assert_eq!(
                *inner_results.lock().expect("inner"),
                vec![true],
                "別 Engine の実行は通常どおり Completed になる（with_sink={with_sink}）"
            );
            assert!(
                matches!(outcome.outcome(), Some(EmbeddingOutcome::Completed { .. })),
                "外側の実行も成功する: {outcome:?}"
            );
            assert_eq!(
                normalized(&sink.log()),
                normalized(&baseline_log),
                "envelope 列は再入しない場合と一致する（with_sink={with_sink}）"
            );
        }
    });
}

// -----------------------------------------------------------------------------
// RG-AT-04: guard の後始末と入れ子
// -----------------------------------------------------------------------------

/// callback の外から、同じ Engine の compile が通常どおり成功する（guard が残っていない）。
/// run_audited は各ケースが sink の挙動に合わせて確かめる。
fn assert_engine_usable_outside_callback(target: &Target) {
    let engine = &*target.engine;
    engine
        .compile(
            Source::new(SourceId::new("after").expect("source id"), "let x = 1\n"),
            &CompileOptions::default(),
        )
        .expect("callback の外の compile は成功する");
    let _ = std::fs::remove_file(target.caps_dir.join("out.txt"));
}

#[test]
fn rg_at_04_guard_is_released_after_reentry_failed_sink_and_panic() {
    within_timeout(|| {
        // 1. 再入で fail-closed になった実行の後。
        {
            let slot: Slot = Arc::new(OnceLock::new());
            let armed = Arc::new(AtomicUsize::new(1));
            let sink = {
                let slot = Arc::clone(&slot);
                let armed = Arc::clone(&armed);
                FnSink::new(move |n| {
                    if n == 1 && armed.load(Ordering::SeqCst) == 1 {
                        let _ = probe_all_entries(slot.get().expect("target"));
                    }
                })
            };
            let target = build_target(&slot, sink, "rg04-reenter");
            assert!(is_audit_failed_sink(&run_outer(&target, 41)));
            armed.store(0, Ordering::SeqCst);
            assert_engine_usable_outside_callback(&target);
            let outcome = run_outer(&target, 42);
            assert!(
                matches!(outcome.outcome(), Some(EmbeddingOutcome::Completed { .. })),
                "{outcome:?}"
            );
        }

        // 2. sink が Failed を返した実行の後。
        {
            struct FailingSink;
            impl AuditSink for FailingSink {
                fn submit(&self, _: Arc<[AuditEnvelope]>, _: AuditWaker) -> AuditSubmit {
                    AuditSubmit::Failed(tsumugi::AuditSinkError::Permanent)
                }
            }
            let slot: Slot = Arc::new(OnceLock::new());
            let target = build_target(&slot, Arc::new(FailingSink), "rg04-failed");
            assert!(is_audit_failed_sink(&run_outer(&target, 43)));
            assert_engine_usable_outside_callback(&target);
            // sink が常に失敗するので Completed にはならないが、guard が残っていれば
            // run_audited が `Outcome(InternalFailure)` で拒否される。そうではなく
            // 通常どおり sink まで届いて `AuditFailed { Sink }` になる。
            assert!(is_audit_failed_sink(&run_outer(&target, 45)));
        }

        // 3. sink が panic した実行の後（unwind で guard が外れる）。
        {
            let slot: Slot = Arc::new(OnceLock::new());
            let sink = FnSink::new(|n| {
                if n == 1 {
                    panic!("injected sink panic (test only)");
                }
            });
            let target = build_target(&slot, sink, "rg04-panic");
            let unwound = catch_unwind(AssertUnwindSafe(|| run_outer(&target, 44)));
            assert!(unwound.is_err(), "sink の panic は run_audited を貫通する");
            assert_engine_usable_outside_callback(&target);
            // panic は 1 回目の submit だけなので、続く run_audited は通常どおり Completed になる。
            let outcome = run_outer(&target, 46);
            assert!(
                matches!(outcome.outcome(), Some(EmbeddingOutcome::Completed { .. })),
                "{outcome:?}"
            );
        }
    });
}

#[test]
fn rg_at_04_nested_callbacks_fail_only_the_reentered_engine() {
    within_timeout(|| {
        let (baseline_b_log, _) = baseline("rg04-nested-base");

        let slot_a: Slot = Arc::new(OnceLock::new());
        let slot_b: Slot = Arc::new(OnceLock::new());
        let a_probe_in_b: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
        let a_probe_after_b: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
        let b_outcome_ok: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));

        // Engine B の sink: Started の submit で A へ再入する。
        let sink_b = {
            let slot_a = Arc::clone(&slot_a);
            let a_probe_in_b = Arc::clone(&a_probe_in_b);
            FnSink::new(move |n| {
                if n == 1 {
                    let a = slot_a.get().expect("A");
                    *a_probe_in_b.lock().expect("probe") = Some(probe_all_entries(a));
                }
            })
        };
        build_target(&slot_b, sink_b.clone(), "rg04-b");

        // Engine A の sink: Started の submit で B を実行し、戻ったあともう一度 A へ再入する。
        let sink_a = {
            let slot_b = Arc::clone(&slot_b);
            let slot_a = Arc::clone(&slot_a);
            let a_probe_after_b = Arc::clone(&a_probe_after_b);
            let b_outcome_ok = Arc::clone(&b_outcome_ok);
            FnSink::new(move |n| {
                if n == 1 {
                    let b = slot_b.get().expect("B");
                    let outcome = run_outer(b, 7);
                    *b_outcome_ok.lock().expect("b") = Some(matches!(
                        outcome.outcome(),
                        Some(EmbeddingOutcome::Completed { .. })
                    ));
                    // B の callback を出たあとも、A の callback の中では A への再入は拒否される。
                    *a_probe_after_b.lock().expect("probe") =
                        Some(probe_all_entries(slot_a.get().expect("A")));
                }
            })
        };
        let target_a = build_target(&slot_a, sink_a.clone(), "rg04-a");

        let outcome_a = run_outer(&target_a, 50);

        assert_eq!(
            a_probe_in_b
                .lock()
                .expect("probe")
                .take()
                .expect("B の callback"),
            Vec::<String>::new(),
            "B の callback からの A への呼び出しは拒否される"
        );
        assert_eq!(
            a_probe_after_b
                .lock()
                .expect("probe")
                .take()
                .expect("A の callback"),
            Vec::<String>::new(),
            "B を出たあとの A の callback でも A への再入は拒否される"
        );
        // B の実行は通常どおり完了し、B の sink の envelope 列は A を呼ばない場合と一致する。
        assert_eq!(*b_outcome_ok.lock().expect("b"), Some(true));
        assert_eq!(normalized(&sink_b.log()), normalized(&baseline_b_log));
        // A の実行だけが fail-closed になる。
        assert!(is_audit_failed_sink(&outcome_a), "{outcome_a:?}");
        // A の callback を出たあとは A も通常どおり使える。
        assert_engine_usable_outside_callback(&target_a);
        // sink の再入は 1 回目の submit だけなので、2 回目の実行は通常どおり成功する
        // （guard が残っていれば run 自体が拒否される）。
        let again = run_outer(&target_a, 51);
        assert!(
            matches!(again.outcome(), Some(EmbeddingOutcome::Completed { .. })),
            "{again:?}"
        );
    });
}

// -----------------------------------------------------------------------------
// RG-AT-05: deadlock しない・非破壊
// -----------------------------------------------------------------------------

#[test]
fn rg_at_05_non_reentrant_sink_and_no_sink_observations_are_unchanged() {
    within_timeout(|| {
        // 再入しない sink ありの経路: 2 回実行して envelope 列が一致する（guard の状態が残らない）。
        let (first, _) = baseline("rg05-a");
        let (second, _) = baseline("rg05-b");
        assert_eq!(normalized(&first), normalized(&second));

        // sink なしの経路: 同じ script が通常どおり Completed になる（guard は何もしない）。
        let engine = no_sink_engine();
        let compiled = compile(&engine, SCRIPT);
        let linked = engine.link(&compiled, link_request()).expect("link");
        let dir = temp_dir("rg05-nosink");
        let mut ctx = EmbeddingContext::new(&engine);
        let outcome = engine.run(&linked, &mut ctx, run_request(60, fs_capabilities(&dir)));
        assert!(
            matches!(outcome, EmbeddingOutcome::Completed { .. }),
            "{outcome:?}"
        );
        assert!(dir.join("out.txt").exists());
    });
}
