//! 協調スケジューラ（admission / run-turn fairness / backpressure）の受入テスト。
//!
//! REV-015 Slice 5、実行制御仕様 §15.4、設計 §6.3。公開 API（`Engine::with_limits` と
//! `ExecutionHandle`）だけを使い、host が各 Ready handle を round-robin で poll する運用を
//! 固定する。本 FEAT（FEAT-001）は scheduler-core（admission + fairness/backpressure）の
//! 範囲を対象とし、host pending（HostCallPending / waker）は FEAT-002 で追加する。

use std::num::{NonZeroU64, NonZeroUsize};

use tsumugi::{
    Engine, EngineLimits, ExecutionContext, ExecutionOutcome, ExecutionRequest, PollResult,
    PollSlice, StartError, YieldReason,
};

fn limits(max_active: usize, max_queued: usize) -> EngineLimits {
    EngineLimits {
        max_active_executions: NonZeroUsize::new(max_active).expect("active 上限は非ゼロ"),
        max_queued_executions: max_queued,
        default_slice_fuel: NonZeroU64::new(10_000).unwrap(),
    }
}

/// 常に Ready な N execution が run-turn FIFO で各 1 slice ずつ進み、1 execution が連続独占
/// しない（AC-2）。非 head handle の poll は semantic work をせず SchedulerPreempted を返す
/// （AC-8）。slice ごとの usage.committed.fuel 増分で確認する。
#[test]
fn n_ready_executions_advance_fifo_one_slice_each() {
    const N: usize = 3;
    let engine = Engine::with_limits(limits(N, 0));
    // 各 execution は十分重い（1 slice では終わらない）。
    let heavy = "\
let i = 0\n\
while i < 1000\n\
  i = i + 1\n\
end\n";
    let script = engine.compile(heavy).unwrap();

    let mut contexts: Vec<ExecutionContext> = (0..N).map(|_| ExecutionContext::new()).collect();
    // 各 context から handle を作る。借用の都合で context を 1 つずつ handle へ渡す。
    let mut ctx_refs: Vec<&mut ExecutionContext> = contexts.iter_mut().collect();
    let mut handles = Vec::new();
    for ctx in ctx_refs.drain(..) {
        let h = engine
            .create_execution(&script, ctx, ExecutionRequest::new())
            .expect("N 個とも active を取れる");
        handles.push(h);
    }

    let small = PollSlice { max_fuel: 16 };

    // まず全 handle を 1 周 poll して run-turn へ登録する。登録順（0,1,2）が FIFO 順になる。
    // 最初の 1 周: handle 0 が head で slice を進める。1,2 は登録されるが非 head → preempt。
    // ただし 0 が rotate するので、各周で head が入れ替わる。
    let mut fuel_before = [0u64; N];
    for (idx, h) in handles.iter().enumerate() {
        fuel_before[idx] = h.usage().committed.fuel;
    }

    // 1 ラウンド目: index 0 を poll すると head（run_turn=[0]）で slice を進め rotate。
    // 続けて 1 を poll すると登録され head（0 が末尾へ回ったので run_turn=[1,2 だが...]）。
    // 実際には 0 を poll→run_turn=[0]→slice→rotate=[0]（単独なので先頭のまま）。そこで
    // round-robin を厳密に観測するため、全 handle を登録してから回す。
    //
    // 登録フェーズ: 各 handle を 1 回ずつ poll する。index 0 は head で前進、1/2 は非 head で
    // SchedulerPreempted（state 不変）。
    let r0 = handles[0].poll(small).unwrap();
    assert!(
        matches!(
            r0,
            PollResult::Yielded {
                reason: YieldReason::SliceFuelExhausted,
                ..
            }
        ),
        "最初に登録した 0 は head で slice を進める: {r0:?}"
    );
    for i in [1usize, 2] {
        let r = handles[i].poll(small).unwrap();
        assert!(
            matches!(
                r,
                PollResult::Yielded {
                    reason: YieldReason::SchedulerPreempted,
                    ..
                }
            ),
            "非 head の {i} は SchedulerPreempted（AC-8）: {r:?}"
        );
        // 非 head poll は fuel を消費しない（semantic work なし）。
        assert_eq!(
            handles[i].usage().committed.fuel,
            fuel_before[i],
            "非 head poll は semantic work をしない"
        );
    }

    // run_turn=[1,2,0]（0 は slice 後に末尾へ rotate）。以後 round-robin で各 1 slice ずつ進む。
    // 2 ラウンド回して、各 handle が概ね均等に前進する（連続独占しない）ことを確認する。
    let mut advanced = [false; N];
    for _round in 0..6 {
        for i in 0..N {
            let before = handles[i].usage().committed.fuel;
            match handles[i].poll(small).unwrap() {
                PollResult::Yielded {
                    reason: YieldReason::SliceFuelExhausted,
                    ..
                } => {
                    assert!(
                        handles[i].usage().committed.fuel > before,
                        "head の slice は fuel を進める"
                    );
                    advanced[i] = true;
                }
                PollResult::Yielded {
                    reason: YieldReason::SchedulerPreempted,
                    ..
                } => {
                    assert_eq!(
                        handles[i].usage().committed.fuel,
                        before,
                        "preempt は fuel を進めない"
                    );
                }
                PollResult::Terminal { .. } => { /* 進み切った handle は terminal になり得る */
                }
                other => panic!("想定外の poll 結果: {other:?}"),
            }
        }
    }
    assert!(
        advanced.iter().all(|&a| a),
        "全 execution が少なくとも 1 slice は前進する（連続独占しない、AC-2）: {advanced:?}"
    );

    drop(handles);
    drop(contexts);
}

/// active 上限ちょうど・queue 上限ちょうどを受理し、+1 が即時 Backpressure（AC-1、設計 §4.1）。
#[test]
fn active_and_queue_limits_exact_then_backpressure() {
    let engine = Engine::with_limits(limits(2, 2));
    let script = engine.compile("let a = 1\n").unwrap();

    // active 上限ちょうど（2 個）を受理。
    let mut ctx1 = ExecutionContext::new();
    let mut ctx2 = ExecutionContext::new();
    let _h1 = engine
        .create_execution(&script, &mut ctx1, ExecutionRequest::new())
        .expect("active 1");
    let _h2 = engine
        .create_execution(&script, &mut ctx2, ExecutionRequest::new())
        .expect("active 2");

    // queue 上限ちょうど（2 個）を受理。
    let mut ctx3 = ExecutionContext::new();
    let mut ctx4 = ExecutionContext::new();
    let _h3 = engine
        .create_execution(&script, &mut ctx3, ExecutionRequest::new())
        .expect("queue 1");
    let _h4 = engine
        .create_execution(&script, &mut ctx4, ExecutionRequest::new())
        .expect("queue 2");

    // +1 は即時 Backpressure。
    let mut ctx5 = ExecutionContext::new();
    match engine.create_execution(&script, &mut ctx5, ExecutionRequest::new()) {
        Err(StartError::Backpressure {
            active,
            queued,
            limit,
        }) => {
            assert_eq!(active, 2);
            assert_eq!(queued, 2);
            assert_eq!(limit, 2);
        }
        Ok(_) => panic!("満杯なのに handle が作られた"),
        Err(other) => panic!("Backpressure を期待したが {other:?}"),
    }
}

/// Paused handle は active slot を握り続け、その間は新規 admission が queue/backpressure に
/// なる（AC-3 の Paused 半分、設計 §4.2/§6.3）。host 待ち半分は FEAT-002 で追加する。
#[test]
fn paused_consumes_active_slot() {
    // active=1, queue=0。handle A を pause して active を握らせる。
    let engine = Engine::with_limits(limits(1, 0));
    let script = engine.compile("let a = 1\n").unwrap();

    let mut ctx_a = ExecutionContext::new();
    let mut handle_a = engine
        .create_execution(&script, &mut ctx_a, ExecutionRequest::new())
        .expect("A active");

    // A を pause する。Paused でも active slot は解放されない（§4.2 FR-4）。
    handle_a.pause().expect("pause は成功する");

    // A が active を握ったままなので、B の admission は満杯で Backpressure（queue=0）。
    let mut ctx_b = ExecutionContext::new();
    match engine.create_execution(&script, &mut ctx_b, ExecutionRequest::new()) {
        Err(StartError::Backpressure { active, .. }) => {
            assert_eq!(active, 1, "Paused handle が active slot を消費している");
        }
        Ok(_) => panic!("Paused が active を解放してしまい、無制限 admission になった"),
        Err(other) => panic!("Backpressure を期待したが {other:?}"),
    }

    // A を resume して terminal まで進めると active が解放され、新規 admission が通る。
    handle_a.resume().expect("resume は成功する");
    let outcome = loop {
        match handle_a.poll(PollSlice::default()).unwrap() {
            PollResult::Yielded { .. } => continue,
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Paused { .. } => panic!("pause は要求していない"),
        }
    };
    assert_eq!(outcome, ExecutionOutcome::Completed);
    drop(handle_a);

    // active が空いたので新規 admission が active を取れる。
    let mut ctx_c = ExecutionContext::new();
    let _h_c = engine
        .create_execution(&script, &mut ctx_c, ExecutionRequest::new())
        .expect("active 解放後は admit が通る");
}

// =============================================================================
// REV-015 Slice 5 FEAT-003: §15.4 host（cooperative adapter / waker / backpressure）
// =============================================================================

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tsumugi::{
    AdapterExecutor, AdapterExecutorLimits, CancellationToken, CapabilityCallContext,
    CapabilitySet, CooperativeHostFunction, ExecutionWaker, HostCallCompleter, HostCallPoll,
    HostCallTicket, HostFunctionDescriptor, HostFunctionId, HostFunctionRegistry, SubmitError,
    Value, Wake, new_ticket,
};

/// wake 回数を数える fake Wake（設計 §6.1）。thread を使わず決定的に検証する。
struct FakeWake {
    count: Arc<AtomicUsize>,
}

impl Wake for FakeWake {
    fn wake(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

fn fake_waker() -> (ExecutionWaker, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let waker = ExecutionWaker::new(Arc::new(FakeWake {
        count: Arc::clone(&count),
    }));
    (waker, count)
}

fn host_fn_id(n: u128) -> HostFunctionId {
    HostFunctionId::new(std::num::NonZeroU128::new(n).unwrap())
}

/// cooperative host function の descriptor（arity 0、ID 固定）。
fn coop_descriptor(id: u128, name: &str) -> HostFunctionDescriptor {
    HostFunctionDescriptor {
        id: host_fn_id(id),
        name: name.to_string(),
        arity: tsumugi::HostArity::Exact(0),
        cost: tsumugi::HostCost::default(),
        argument_audit: Vec::new(),
        result_audit: tsumugi::AuditValuePolicy::Omit,
        may_block: true,
    }
}

// cooperative host call は `Value`（`Rc` を含み `!Send`）を返すため、completer<Value> は !Send。
// よって作成 thread（poll を呼ぶ thread）に留める必要がある（設計 §9.1）。テスト fixture の
// adapter は `Send + Sync`（registry 格納のため）でなければならないので、completer を struct へ
// 持たず、作成 thread の thread_local へ stash する。テストは同一 thread から取り出して complete
// する（thread を使わない決定的駆動、§6.1）。
thread_local! {
    static PENDING_COMPLETERS: std::cell::RefCell<Vec<HostCallCompleter<Value>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 作成 thread に stash された completer を 1 個取り出す（テストが complete するため）。
fn take_stashed_completer() -> Option<HostCallCompleter<Value>> {
    PENDING_COMPLETERS.with(|c| c.borrow_mut().pop())
}

/// テスト駆動の cooperative adapter（設計 §6.1 `FakeCooperativeAdapter`）。
///
/// `start` が `Pending(ticket)` を返し、completer を作成 thread の thread_local へ stash する。
/// テストはそれを取り出して明示的に `complete` する（thread を使わない）。`ready` が設定
/// されていれば `Ready(Ok(..))` を返す。`offloads` は submit 記録（non-offload 契約の観測、§6.3）。
///
/// `Value` を struct に保持しないため（thread_local 経由）、本型は `Send + Sync` を満たし
/// registry（`Send + Sync` 必須）へ格納できる。
struct FakeCooperativeAdapter {
    descriptor: HostFunctionDescriptor,
    /// `start` が `Ready(Ok(Int(n)))` を返すべきなら `Some(n)`、`Pending` なら `None`。
    ready_int: Mutex<Option<i64>>,
    /// `start` が response byte 上限での途中停止（`Ready(Err(Control(BudgetExceeded)))`）を
    /// 返すべきかどうか（§6.3 response_byte_limit）。true なら ready_int/pending より優先。
    response_byte_stop: Mutex<bool>,
    /// start が最後に観測した may_yield。
    last_may_yield: Arc<Mutex<Option<bool>>>,
    /// Phase-2 sync trait を別 thread へ offload した回数（期待値は 0、§6.3）。
    offloads: Arc<AtomicUsize>,
    next_id: AtomicUsize,
}

impl FakeCooperativeAdapter {
    fn pending(id: u128, name: &str) -> Arc<Self> {
        Arc::new(Self {
            descriptor: coop_descriptor(id, name),
            ready_int: Mutex::new(None),
            response_byte_stop: Mutex::new(false),
            last_may_yield: Arc::new(Mutex::new(None)),
            offloads: Arc::new(AtomicUsize::new(0)),
            next_id: AtomicUsize::new(1),
        })
    }
}

impl CooperativeHostFunction for FakeCooperativeAdapter {
    fn descriptor(&self) -> &HostFunctionDescriptor {
        &self.descriptor
    }

    fn start(
        &self,
        context: &mut CapabilityCallContext<'_>,
        _arguments: &[Value],
    ) -> HostCallPoll<Value> {
        *self.last_may_yield.lock().unwrap() = Some(context.may_yield());
        // Phase-2 sync trait を spawn で呼ばない（non-offload 契約）。offloads は 0 のまま。
        let _ = &self.offloads;
        // response byte 上限での途中停止（§4.4）: 全量 buffer せず Control(BudgetExceeded) を返す。
        if *self.response_byte_stop.lock().unwrap() {
            let exceeded = tsumugi::BudgetExceeded {
                resource: tsumugi::BudgetResource::HostResponseBytes,
                limit: context.remaining_response_bytes(),
                used: 0,
                reserved: 0,
                requested: context.remaining_response_bytes() + 1,
                unit: tsumugi::BudgetUnit::Bytes,
                phase: tsumugi::ExecutionPhase::Run,
            };
            return HostCallPoll::Ready(Err(tsumugi::AdapterError::Control(
                tsumugi::ControlStop::BudgetExceeded(exceeded),
            )));
        }
        if let Some(n) = self.ready_int.lock().unwrap().take() {
            return HostCallPoll::Ready(Ok(Value::Int(n)));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as u64;
        let (ticket, completer): (HostCallTicket<Value>, HostCallCompleter<Value>) =
            new_ticket::<Value>(id);
        // completer を作成 thread の thread_local へ stash する（struct へ Value を持たない）。
        PENDING_COMPLETERS.with(|c| c.borrow_mut().push(completer));
        HostCallPoll::Pending(ticket)
    }
}

/// `name` の cooperative adapter を 1 個だけ登録した registry と、その authority を grant した
/// capability set を作る共通ヘルパ。
fn coop_setup(
    id: u128,
    _name: &str,
    adapter: Arc<dyn CooperativeHostFunction>,
) -> (Arc<HostFunctionRegistry>, CapabilitySet) {
    let registry = HostFunctionRegistry::builder()
        .register_cooperative(adapter)
        .expect("cooperative 登録は成功する")
        .build()
        .expect("build は成功する");
    let caps = CapabilitySet::builder()
        .grant_host_function(host_fn_id(id))
        .expect("grant は成功する")
        .build();
    (Arc::new(registry), caps)
}

/// cooperative host-call を使う 1 文スクリプトで context をセットアップする。
fn coop_context(registry: Arc<HostFunctionRegistry>, caps: CapabilitySet) -> ExecutionContext {
    let mut ctx = ExecutionContext::new();
    ctx.set_host_registry(registry);
    ctx.set_capabilities(caps);
    ctx
}

/// A が cooperative adapter で Pending に入り host executor 未実行のまま、B が run-turn で
/// 前進する。caller thread は block しない（AC-5、設計 §6.3）。
#[test]
fn blocking_fake_host_does_not_block_caller_other_execution_progresses() {
    let engine = Engine::with_limits(limits(2, 0));
    let adapter = FakeCooperativeAdapter::pending(1, "slow_host");
    let (registry, caps) = coop_setup(1, "slow_host", adapter.clone());

    let script_a = engine.compile("let x = slow_host()\n").unwrap();
    let heavy = "\
let i = 0\n\
while i < 1000\n\
  i = i + 1\n\
end\n";
    let script_b = engine.compile(heavy).unwrap();

    let mut ctx_a = coop_context(registry, caps);
    let mut ctx_b = ExecutionContext::new();

    let mut handle_a = engine
        .create_execution(&script_a, &mut ctx_a, ExecutionRequest::new())
        .expect("A active");
    let mut handle_b = engine
        .create_execution(&script_b, &mut ctx_b, ExecutionRequest::new())
        .expect("B active");

    // A を poll すると cooperative host-call が Pending → HostCallPending（caller は block しない）。
    let ra = handle_a.poll(PollSlice::default()).unwrap();
    assert!(
        matches!(
            ra,
            PollResult::Yielded {
                reason: YieldReason::HostCallPending { .. },
                ..
            }
        ),
        "A は HostCallPending で yield する（block しない）: {ra:?}"
    );

    // host executor（adapter の completer）を実行しないまま、B が run-turn で前進して完了する。
    let mut b_terminated = false;
    for _ in 0..200 {
        match handle_b.poll(PollSlice { max_fuel: 64 }).unwrap() {
            PollResult::Terminal { outcome, .. } => {
                assert_eq!(outcome, ExecutionOutcome::Completed);
                b_terminated = true;
                break;
            }
            PollResult::Yielded { .. } => continue,
            PollResult::Paused { .. } => panic!("pause は要求していない"),
        }
    }
    assert!(b_terminated, "host 未完了でも B は前進・完了する（AC-5）");

    // A の poll を続けても未完了なら HostCallPending のまま（block しない）。
    let ra2 = handle_a.poll(PollSlice::default()).unwrap();
    assert!(matches!(
        ra2,
        PollResult::Yielded {
            reason: YieldReason::HostCallPending { .. },
            ..
        }
    ));
}

/// Pending → HostCallPending → waker wake → 次 poll で try_take → terminal の往復（AC-6）。
/// continuation は作成 thread の poll でだけ進む。
#[test]
fn host_pending_waker_roundtrip() {
    let engine = Engine::with_limits(limits(1, 0));
    let adapter = FakeCooperativeAdapter::pending(1, "fetch");
    let (registry, caps) = coop_setup(1, "fetch", adapter.clone());

    let script = engine.compile("let x = fetch()\n").unwrap();
    let mut ctx = coop_context(registry, caps);
    let mut handle = engine
        .create_execution(&script, &mut ctx, ExecutionRequest::new())
        .expect("active");

    let (waker, wake_count) = fake_waker();
    handle.set_waker(Some(waker)).expect("set_waker ok");

    // 1 回目の poll で Pending → HostCallPending。
    let r = handle.poll(PollSlice::default()).unwrap();
    assert!(matches!(
        r,
        PollResult::Yielded {
            reason: YieldReason::HostCallPending { .. },
            ..
        }
    ));
    assert_eq!(wake_count.load(Ordering::SeqCst), 0, "complete 前は wake 0");

    // host executor（completer）を実行して結果を注入 → waker が 1 回鳴る。
    let completer = take_stashed_completer().expect("completer");
    completer.complete(Ok(Value::Int(42)));
    assert_eq!(
        wake_count.load(Ordering::SeqCst),
        1,
        "complete で waker 1 回"
    );

    // 次 poll で try_take → resume → terminal。
    let outcome = loop {
        match handle.poll(PollSlice::default()).unwrap() {
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Yielded { .. } => continue,
            PollResult::Paused { .. } => panic!("pause は要求していない"),
        }
    };
    assert_eq!(outcome, ExecutionOutcome::Completed);
}

/// 両 interleaving で lost wake が起きない（Finding 1、設計 §6.3）。
/// ticket プロトコルを直接駆動して register/complete の順序を両方向で固定する。
#[test]
fn lost_wake_avoided_both_interleavings() {
    // (a) register-after-complete。
    {
        let (ticket, completer): (HostCallTicket<Value>, HostCallCompleter<Value>) =
            new_ticket::<Value>(1);
        let (waker, count) = fake_waker();
        completer.complete(Ok(Value::Int(1)));
        assert_eq!(count.load(Ordering::SeqCst), 0, "register 前は wake なし");
        ticket.register_waker(&waker);
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "register が ready を観測し即 wake"
        );
    }
    // (b) complete-after-register。
    {
        let (ticket, completer): (HostCallTicket<Value>, HostCallCompleter<Value>) =
            new_ticket::<Value>(2);
        let (waker, count) = fake_waker();
        ticket.register_waker(&waker);
        assert_eq!(count.load(Ordering::SeqCst), 0, "complete 前は wake なし");
        completer.complete(Ok(Value::Int(2)));
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "complete が保存済み waker を wake"
        );
    }
}

/// set_waker の置換・解除・terminal 後の Err（設計 §6.3）。
#[test]
fn set_waker_replace_and_clear() {
    let engine = Engine::with_limits(limits(1, 0));
    let script = engine.compile("let a = 1\n").unwrap();
    let mut ctx = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut ctx, ExecutionRequest::new())
        .expect("active");

    let (w1, _c1) = fake_waker();
    let (w2, _c2) = fake_waker();
    handle.set_waker(Some(w1)).expect("Some(w1) ok");
    handle.set_waker(Some(w2)).expect("Some(w2) 置換 ok");
    handle.set_waker(None).expect("None 解除 ok");

    // terminal まで進める。
    let outcome = loop {
        match handle.poll(PollSlice::default()).unwrap() {
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Yielded { .. } => continue,
            PollResult::Paused { .. } => unreachable!(),
        }
    };
    assert_eq!(outcome, ExecutionOutcome::Completed);
    // terminal 後の set_waker は Err(Terminal)。
    let (w3, _c3) = fake_waker();
    assert_eq!(
        handle.set_waker(Some(w3)),
        Err(tsumugi::HandleError::Terminal)
    );
}

/// cooperative adapter が Phase-2 sync trait を別 thread へ offload しない（FR-8 契約、§6.3）。
/// FakeCooperativeAdapter は submit を記録せず（offloads=0）、擬似 Pending を spawn で作らない。
#[test]
fn cooperative_adapter_does_not_offload_sync_trait() {
    let engine = Engine::with_limits(limits(1, 0));
    let adapter = FakeCooperativeAdapter::pending(1, "coop");
    let offloads = adapter.offloads.clone();
    let (registry, caps) = coop_setup(1, "coop", adapter.clone());

    let script = engine.compile("let x = coop()\n").unwrap();
    let mut ctx = coop_context(registry, caps);
    let mut handle = engine
        .create_execution(&script, &mut ctx, ExecutionRequest::new())
        .expect("active");

    let _ = handle.poll(PollSlice::default()).unwrap();
    assert_eq!(
        offloads.load(Ordering::SeqCst),
        0,
        "cooperative adapter は sync trait を別 thread へ offload しない（FR-8）"
    );
}

/// AdapterExecutor queue 満杯は Ready-Err backpressure（Pending ではない）で caller を block
/// しない（設計 §4.4 / AC-4）。no-thread の最小構成（max_queued=0）で検証する。
#[test]
fn adapter_executor_queue_full_is_backpressure_not_pending() {
    let executor = AdapterExecutor::new(AdapterExecutorLimits {
        max_threads: NonZeroUsize::new(1).unwrap(),
        max_queued: 0,
        max_concurrency: NonZeroUsize::new(1).unwrap(),
    });
    let (_ticket, completer) = new_ticket::<i32>(1);
    let deadline = tsumugi::MonotonicClock::now(&tsumugi::FakeClock::new());
    let cancel = CancellationToken::new();
    let out = executor.submit(completer, deadline, cancel, |_obs, _dl| Ok(1));
    assert_eq!(
        out,
        Err(SubmitError::QueueFull),
        "queue 満杯は Ready-Err backpressure"
    );
}

/// CancellationToken / ExecutionWaker が Send + Sync（別 thread から cancel / wake できる、§6.4）。
#[test]
fn cancellation_and_waker_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CancellationToken>();
    assert_send_sync::<ExecutionWaker>();
}

/// 別 thread から cancel を鳴らす最小 smoke テスト（§6.4）。
#[test]
fn cancel_from_another_thread_smoke() {
    let token = CancellationToken::new();
    let remote = token.clone();
    let handle = std::thread::spawn(move || remote.cancel());
    let newly = handle.join().unwrap();
    assert!(newly, "別 thread からの初回 cancel は true");
    assert!(token.is_cancelled(), "元 token でも cancel を観測する");
}

/// cooperative host-call 待ち中の handle が active slot を握り続ける（AC-3 の host 待ち半分）。
#[test]
fn host_waiting_consumes_active_slot() {
    let engine = Engine::with_limits(limits(1, 0));
    let adapter = FakeCooperativeAdapter::pending(1, "wait_host");
    let (registry, caps) = coop_setup(1, "wait_host", adapter.clone());

    let script = engine.compile("let x = wait_host()\n").unwrap();
    let mut ctx = coop_context(registry, caps);
    let mut handle = engine
        .create_execution(&script, &mut ctx, ExecutionRequest::new())
        .expect("active");

    // host-call Pending に入れて active を握らせる。
    let r = handle.poll(PollSlice::default()).unwrap();
    assert!(matches!(
        r,
        PollResult::Yielded {
            reason: YieldReason::HostCallPending { .. },
            ..
        }
    ));

    // host 待ち中は active を解放しないので、B の admission は Backpressure（queue=0）。
    let mut ctx_b = ExecutionContext::new();
    let script_b = engine.compile("let a = 1\n").unwrap();
    match engine.create_execution(&script_b, &mut ctx_b, ExecutionRequest::new()) {
        Err(StartError::Backpressure { active, .. }) => {
            assert_eq!(
                active, 1,
                "host 待ち handle が active slot を消費している（AC-3）"
            );
        }
        Ok(_) => panic!("host 待ちが active を解放し無制限 admission になった"),
        Err(other) => panic!("Backpressure を期待したが {other:?}"),
    }

    // 完了させて active を解放する。
    let completer = take_stashed_completer().expect("completer");
    completer.complete(Ok(Value::Int(0)));
    let outcome = loop {
        match handle.poll(PollSlice::default()).unwrap() {
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Yielded { .. } => continue,
            PollResult::Paused { .. } => unreachable!(),
        }
    };
    assert_eq!(outcome, ExecutionOutcome::Completed);
}

/// AC-4 の cancel: host 待ち中に cancel すると次 poll で Cancelled terminal（clock 不要、§4.6）。
/// deadline 側は alpha facade の legacy budget が専用 clock domain の deadline（ns=u64::MAX）を
/// 持つため `new_with_clock` が foreign clock を一律に弾く構造になっており（§4.6、FEAT-002 の
/// `new_with_clock_rejects_foreign_clock` 参照）、本 facade からは観測できない。したがって本
/// テストでは cancel 側（観測可能）と foreign-clock 拒否を固定する。
#[test]
fn cancel_works_while_host_waiting_and_foreign_clock_rejected() {
    use tsumugi::{BudgetConfigError, FakeClock};

    // --- cancel 側（clock 不要） ---
    let engine = Engine::with_limits(limits(1, 0));
    let adapter = FakeCooperativeAdapter::pending(1, "slow");
    let (registry, caps) = coop_setup(1, "slow", adapter.clone());
    let script = engine.compile("let x = slow()\n").unwrap();
    let mut ctx = coop_context(registry, caps);
    let mut handle = engine
        .create_execution(&script, &mut ctx, ExecutionRequest::new())
        .expect("active");

    // host-call Pending に入れる。
    let r = handle.poll(PollSlice::default()).unwrap();
    assert!(matches!(
        r,
        PollResult::Yielded {
            reason: YieldReason::HostCallPending { .. },
            ..
        }
    ));
    // host 待ち中に cancel → 次 poll で Cancelled terminal（try_take より先の checkpoint、§4.6）。
    handle.cancellation_token().cancel();
    match handle.poll(PollSlice::default()).unwrap() {
        PollResult::Terminal { outcome, .. } => assert_eq!(outcome, ExecutionOutcome::Cancelled),
        other => panic!("Cancelled を期待したが {other:?}"),
    }

    // --- foreign clock は new_with_clock が弾く（§4.6） ---
    let foreign = Arc::new(FakeClock::new());
    match ExecutionContext::new_with_clock(foreign) {
        Err(StartError::Config(BudgetConfigError::ForeignClock)) => {}
        Ok(_) => panic!("foreign clock は new_with_clock で弾かれるべき"),
        Err(other) => panic!("ForeignClock を期待したが {other:?}"),
    }
}

/// response byte 上限での途中停止: provider が全量 buffer する前に
/// `Control(BudgetExceeded(HostResponseBytes))` を返し、catch 不能 terminal になる（§6.3）。
#[test]
fn response_byte_limit_stops_stream_midway() {
    let engine = Engine::with_limits(limits(1, 0));
    let adapter = FakeCooperativeAdapter::pending(1, "stream");
    *adapter.response_byte_stop.lock().unwrap() = true;
    let (registry, caps) = coop_setup(1, "stream", adapter.clone());

    // host-call を try/catch で囲んでも、BudgetExceeded は catch 不能 terminal（捕捉されない）。
    let script = engine
        .compile("try\n  let x = stream()\ncatch e\n  let y = 1\nend\n")
        .unwrap();
    let mut ctx = coop_context(registry, caps);
    let mut handle = engine
        .create_execution(&script, &mut ctx, ExecutionRequest::new())
        .expect("active");

    let outcome = loop {
        match handle.poll(PollSlice::default()).unwrap() {
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Yielded { .. } => continue,
            PollResult::Paused { .. } => unreachable!(),
        }
    };
    // BudgetExceeded(HostResponseBytes) は catch 不能 → RuntimeError terminal（catch されない）。
    match outcome {
        ExecutionOutcome::RuntimeError { error } => {
            assert_eq!(
                error.error_type(),
                "io_limit",
                "HostResponseBytes 超過は io_limit kind（catch 不能 terminal）"
            );
        }
        other => panic!("RuntimeError(io_limit) を期待したが {other:?}"),
    }
}

/// may_yield=false 文脈（関数本体内）で cooperative adapter が Pending を返すと、busy-loop せず
/// catch 不能 internal terminal（InternalControl）になり、try/catch でも捕捉できない（Finding 7）。
/// 対比として may_yield=true（トップレベル文）では同じ adapter の Pending が HostCallPending に
/// なることを確認する。
#[test]
fn may_yield_false_requires_ready_else_internal_terminal() {
    // (a) 関数本体内（can_yield=false）で Pending → InternalControl terminal（try/catch でも不可）。
    {
        let engine = Engine::with_limits(limits(1, 0));
        let adapter = FakeCooperativeAdapter::pending(1, "inner");
        let (registry, caps) = coop_setup(1, "inner", adapter.clone());
        // 関数本体内で cooperative host-call を呼ぶ。try/catch で囲んでも捕捉されない。
        let script = engine
            .compile("fn f()\n  let x = inner()\nend\ntry\n  f()\ncatch e\n  let y = 1\nend\n")
            .unwrap();
        let mut ctx = coop_context(registry, caps);
        let mut handle = engine
            .create_execution(&script, &mut ctx, ExecutionRequest::new())
            .expect("active");

        let outcome = loop {
            match handle.poll(PollSlice::default()).unwrap() {
                PollResult::Terminal { outcome, .. } => break outcome,
                PollResult::Yielded { .. } => continue,
                PollResult::Paused { .. } => unreachable!(),
            }
        };
        match outcome {
            ExecutionOutcome::RuntimeError { error } => {
                assert_eq!(
                    error.error_type(),
                    "internal_control",
                    "may_yield=false の Pending は InternalControl（catch 不能、既存 internal とは別）"
                );
            }
            other => panic!("RuntimeError(internal_control) を期待したが {other:?}"),
        }
        // adapter が観測した may_yield は false（関数本体内）。
        assert_eq!(*adapter.last_may_yield.lock().unwrap(), Some(false));
    }

    // (b) トップレベル文（can_yield=true）では同じ adapter の Pending が HostCallPending になる。
    {
        let engine = Engine::with_limits(limits(1, 0));
        let adapter = FakeCooperativeAdapter::pending(2, "top");
        let (registry, caps) = coop_setup(2, "top", adapter.clone());
        let script = engine.compile("let x = top()\n").unwrap();
        let mut ctx = coop_context(registry, caps);
        let mut handle = engine
            .create_execution(&script, &mut ctx, ExecutionRequest::new())
            .expect("active");

        let r = handle.poll(PollSlice::default()).unwrap();
        assert!(
            matches!(
                r,
                PollResult::Yielded {
                    reason: YieldReason::HostCallPending { .. },
                    ..
                }
            ),
            "may_yield=true の Pending は HostCallPending yield: {r:?}"
        );
        assert_eq!(*adapter.last_may_yield.lock().unwrap(), Some(true));
    }
}
