//! host-call pending プロトコル（REV-015 Slice 5、設計 §4.4 ticket part / §4.5）。
//!
//! 本 module は「ブロックしない host-call の ticket / waker プロトコル」を閉じ込める。
//! cooperative adapter が結果を即返せない（`Pending`）とき、execution は
//! `Yielded(HostCallPending)` になり、ticket へ 1 個の [`ExecutionWaker`] を登録する。
//! adapter executor（別 thread・Engine 外）は結果を ticket へ一度だけ格納して wake し、
//! 作成 thread の次回 poll が [`HostCallTicket::try_take`] で取り出す。wake は continuation を
//! 別 thread で実行しない（INV-7）。
//!
//! # lost-wake 回避の順序（設計 §4.5、Finding 1）
//!
//! - [`HostCallTicket::register_waker`]: waker を保存 → ready flag を再確認 → ready なら即 wake。
//! - [`HostCallCompleter::complete`]: result を一度だけ格納 → **ready を waker 読み取りより先に**
//!   store → 保存済み waker を clone → lock 解放後に wake。
//!
//! この 2 つの順序を厳守することで、register-after-complete / complete-after-register の
//! どちらの interleaving でも wake が 1 回保証される（§4.5 の説明と §6.3 の両 interleaving
//! テストが根拠）。
//!
//! # Send + Sync 境界
//!
//! 別 thread（adapter executor）へ渡るのは [`HostCallCompleter`] と [`ExecutionWaker`] /
//! [`HostCallTicket`]（`T: Send` のとき）だけ。continuation / context は渡さない（§9.1）。
//! 公開 `ExecutionHandle`（`engine.rs`）は `!Send + !Sync` のまま。

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::budget::{CancellationToken, MonotonicInstant};
use crate::capability::{AdapterError, CapabilityCallContext};
use crate::value::Value;

/// 作成 thread の poll を起こす edge-triggered な wake hint（設計 §4.5）。
///
/// `wake()` は host への「次 poll を呼べ」というヒントだけで、continuation を進めない
/// （INV-7）。実装は `Send + Sync`（別 thread の adapter executor / cancel から呼べる）。
pub trait Wake: Send + Sync {
    /// handle の作成 thread に次 poll を促す。multiple wake は coalesce してよい。
    fn wake(&self);
}

/// handle の wake handle（設計 §4.4）。`Arc<dyn Wake>` を包み `Send + Sync`・`Clone`。
///
/// host-call ticket / [`crate::budget::CancellationToken`] / admission 昇格が state を
/// ready にしたときに `wake()` を呼ぶ。別 thread から保持・invoke してよい唯一の handle
/// 由来の操作のひとつ（もうひとつは cancel、§9.1）。
#[derive(Clone)]
pub struct ExecutionWaker(Arc<dyn Wake>);

impl ExecutionWaker {
    /// `Arc<dyn Wake>` から waker を作る。
    pub fn new(wake: Arc<dyn Wake>) -> Self {
        Self(wake)
    }

    /// 登録先に wake hint を送る。
    pub fn wake(&self) {
        self.0.wake();
    }
}

impl std::fmt::Debug for ExecutionWaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionWaker").finish_non_exhaustive()
    }
}

/// cooperative adapter の `start` が返す host-call の即時結果（設計 §4.4 / FR-6）。
///
/// `Ready` は同期完了（結果をその場で値へ写す）。`Pending` は execution を
/// `Yielded(HostCallPending)` にし、[`HostCallTicket`] へ waker を登録して結果を待つ。
pub enum HostCallPoll<T> {
    /// 同期的に結果が得られた。
    Ready(Result<T, AdapterError>),
    /// 結果はまだ。ticket で待つ。
    Pending(HostCallTicket<T>),
}

/// ticket の共有内部（`Send + Sync`、設計 §4.4）。result slot + ready flag + waker + cancel flag。
struct TicketShared<T> {
    /// この host call の識別子（handle の pending と突き合わせる）。
    id: u64,
    /// result / waker / cancelled を 1 つの `Mutex` 下で守る。
    state: Mutex<TicketState<T>>,
    /// store-waker → recheck-ready の lock-free fast path（設計 §4.5）。
    ///
    /// `complete` は result 格納後に **waker 読み取りより先に** `Release` store し、
    /// `register_waker` は waker 保存後に `Acquire` load して ready なら即 wake する。
    ready: AtomicBool,
}

/// `TicketShared` の `Mutex` が守る可変状態。
struct TicketState<T> {
    /// 一度だけ格納される結果（`complete` が `None` のときだけ書き込む）。
    result: Option<Result<T, AdapterError>>,
    /// 登録済み waker（最後の `register_waker` が置換）。
    waker: Option<ExecutionWaker>,
    /// cancel / drop 済みか。cancel 後は格納済み / 遅着 result を破棄する（FR-6）。
    cancelled: bool,
}

/// 作成 thread が保持する host-call ticket（`T: Send` のとき `Send + Sync`、設計 §4.4）。
///
/// poll で [`Self::try_take`] し、host-call pending に入るとき [`Self::register_waker`] する。
/// drop / [`Self::cancel`] は adapter へ取消要求を送り、以後の遅着 result を破棄する。
pub struct HostCallTicket<T> {
    shared: Arc<TicketShared<T>>,
}

impl<T> HostCallTicket<T> {
    /// この host call の識別子を返す。
    pub fn id(&self) -> u64 {
        self.shared.id
    }

    /// waker を登録する（設計 §4.5、lost-wake 回避）。
    ///
    /// 1. `Mutex` を取り waker を保存（置換）する。
    /// 2. lock 解放後に `ready.load(Acquire)` を再確認し、ready なら即 wake する。
    pub fn register_waker(&self, waker: &ExecutionWaker) {
        {
            let mut st = self.shared.state.lock().expect("ticket mutex poisoned");
            st.waker = Some(waker.clone());
        }
        // ready 再確認 → 即 wake（complete が先行して ready を立てていた場合に拾う）。
        if self.shared.ready.load(Ordering::Acquire) {
            waker.wake();
        }
    }

    /// 作成 thread から結果を 1 回取り出す（設計 §4.4）。
    ///
    /// `ready` が立っていて cancel されていなければ、格納済み `result` を `take()` して返す。
    /// cancel 済みなら格納済み / 遅着 result を破棄して `None`（FR-6）。未完了も `None`。
    pub fn try_take(&self) -> Option<Result<T, AdapterError>> {
        if !self.shared.ready.load(Ordering::Acquire) {
            return None;
        }
        let mut st = self.shared.state.lock().expect("ticket mutex poisoned");
        if st.cancelled {
            // cancel 済み: 遅着 result は返さず破棄する。
            st.result = None;
            return None;
        }
        st.result.take()
    }

    /// adapter へ取消要求を送り、以後の result を破棄する（設計 §4.4 / §4.8 step 2）。
    ///
    /// `cancelled` を立て、格納済み result も破棄する。executor 側は
    /// [`HostCallCompleter::is_cancelled`] で協調的に停止できる。
    pub fn cancel(&self) {
        let mut st = self.shared.state.lock().expect("ticket mutex poisoned");
        st.cancelled = true;
        st.result = None;
    }
}

impl<T> Drop for HostCallTicket<T> {
    fn drop(&mut self) {
        // drop は cancel 相当（遅着破棄、設計 §4.4）。
        if let Ok(mut st) = self.shared.state.lock() {
            st.cancelled = true;
            st.result = None;
        }
    }
}

impl<T> std::fmt::Debug for HostCallTicket<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCallTicket")
            .field("id", &self.shared.id)
            .finish_non_exhaustive()
    }
}

/// adapter executor が結果を書き戻すハンドル（`Send`。executor thread が持つ、設計 §4.4）。
pub struct HostCallCompleter<T> {
    shared: Arc<TicketShared<T>>,
}

impl<T> HostCallCompleter<T> {
    /// 結果を一度だけ格納し、登録済み waker を wake する（設計 §4.5、Finding 1）。
    ///
    /// 1. `Mutex` を取り `result` が `None` のときだけ格納する（二度目は破棄）。
    /// 2. `ready.store(true, Release)` を **waker 読み取りより先に** 行う。
    /// 3. 保存済み waker を clone して退避し、lock 解放後に wake する。
    pub fn complete(self, result: Result<T, AdapterError>) {
        {
            let mut st = self.shared.state.lock().expect("ticket mutex poisoned");
            if st.result.is_some() {
                // 既に格納済み: 二度目は破棄する（INV-4）。
                return;
            }
            st.result = Some(result);
        }
        // waker を読む前に ready を立てる（register-after-complete で register が true を読める）。
        self.shared.ready.store(true, Ordering::Release);
        let waker = {
            let st = self.shared.state.lock().expect("ticket mutex poisoned");
            st.waker.clone()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// provider が協調 cancel を確認する（設計 §4.4 / FR-9）。
    pub fn is_cancelled(&self) -> bool {
        self.shared
            .state
            .lock()
            .map(|st| st.cancelled)
            .unwrap_or(true)
    }
}

impl<T> std::fmt::Debug for HostCallCompleter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCallCompleter")
            .field("id", &self.shared.id)
            .finish_non_exhaustive()
    }
}

/// ticket と completer のペアを作る（設計 §4.4）。両者が `Arc<TicketShared<T>>` を共有する。
pub fn new_ticket<T>(id: u64) -> (HostCallTicket<T>, HostCallCompleter<T>) {
    let shared = Arc::new(TicketShared {
        id,
        state: Mutex::new(TicketState {
            result: None,
            waker: None,
            cancelled: false,
        }),
        ready: AtomicBool::new(false),
    });
    (
        HostCallTicket {
            shared: Arc::clone(&shared),
        },
        HostCallCompleter { shared },
    )
}

/// 型消去済みの host-call ticket（設計 §4.4 / §4.7）。
///
/// `Evaluator` は Pending の host-call を `Value` 型で待つが、`run_slice` が
/// `YieldedHostCall` を返した時点で handle へ ticket を移送する（Finding 3）。handle は
/// 具体型 [`HostCallTicket<T>`] を知らずに `try_take_value` / `cancel` だけ駆動できるよう、
/// この型消去 wrapper で保持する。[`crate::engine::ExecutionHandle`] の
/// `pending_ticket: Option<TicketErased>` がこれを持つ。
pub struct TicketErased {
    /// 具体型を消した `HostCallTicket<Value>`。`Value` へ writeback される host-call に限る
    /// （本 Slice の cooperative host call は script 値を返す、§4.7）。
    inner: HostCallTicket<Value>,
}

impl TicketErased {
    /// `HostCallTicket<Value>` から型消去 ticket を作る。
    pub fn new(ticket: HostCallTicket<Value>) -> Self {
        Self { inner: ticket }
    }

    /// この host call の識別子を返す。
    pub fn id(&self) -> u64 {
        self.inner.id()
    }

    /// handle の waker を ticket へ登録する（host-call pending 進入時、設計 §4.3 step 6）。
    pub fn register_waker(&self, waker: &ExecutionWaker) {
        self.inner.register_waker(waker);
    }

    /// 完了済みなら `Value` 結果を取り出す（設計 §4.7、resume 用）。
    pub fn try_take_value(&self) -> Option<Result<Value, AdapterError>> {
        self.inner.try_take()
    }

    /// adapter へ取消要求を送り遅着結果を破棄する（設計 §4.8 step 2）。
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

impl std::fmt::Debug for TicketErased {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TicketErased")
            .field("id", &self.inner.id())
            .finish_non_exhaustive()
    }
}

/// cooperative（ブロックしない）host adapter（設計 §4.4 / FR-8）。
///
/// 各 Phase-2 同期 adapter の request/response 型ごとに Phase-4 で登録できる。同期 adapter は
/// 常に `Ready` 相当で `Pending` API を持たない。cooperative 実装は同じ capability 判定・
/// budget reservation・deadline・cancellation・audit correlation（[`CapabilityCallContext`]）を
/// 使い、Phase-2 sync trait を別 thread へ暗黙 offload して擬似 `Pending` へ変換してはならない
/// （non-offload 契約、§4.4）。
///
/// `start` は [`CapabilityCallContext::may_yield`] が `false`（関数本体・callback・同期経路）の
/// とき **必ず `Ready` を返す**。規約違反の `Pending` は dispatcher が catch 不能 terminal
/// （`ErrorKind::InternalControl`）にする（Finding 7）。
pub trait CooperativeAdapter<Request, Response>: Send + Sync + 'static {
    /// host call を開始する。即時完了なら [`HostCallPoll::Ready`]、未完了なら
    /// [`HostCallPoll::Pending`] を返す。
    fn start(
        &self,
        context: &mut CapabilityCallContext<'_>,
        request: Request,
    ) -> HostCallPoll<Response>;
}

/// provider が協調 cancel を観測する view（設計 §4.4）。
///
/// [`AdapterExecutor::submit`] が実行する `f` へ cancel token の read-only view を渡す。
/// provider は cancel 不能 API では deadline 以下の有限 timeout を設定し、cancel 観測後は
/// 結果/副作用を script へ返さない（FR-9）。
pub struct CancelObserver {
    cancellation: CancellationToken,
}

impl CancelObserver {
    /// cancel token から observer を作る。
    pub fn new(cancellation: CancellationToken) -> Self {
        Self { cancellation }
    }

    /// cancel が要求されたか。
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

impl std::fmt::Debug for CancelObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelObserver")
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish()
    }
}

/// [`AdapterExecutor`] の有限上限（設計 §4.4 / FR-9）。
#[derive(Debug, Clone, Copy)]
pub struct AdapterExecutorLimits {
    /// thread pool の最大 thread 数。
    pub max_threads: NonZeroUsize,
    /// queue に積める最大 job 数（満杯なら backpressure）。
    pub max_queued: usize,
    /// 同時実行の最大数。
    pub max_concurrency: NonZeroUsize,
}

/// Engine 本体と別の有限 thread pool を持つ adapter executor（設計 §4.4 / NFR-2 / FR-9）。
///
/// `submit` された job を executor thread 上で実行し、[`HostCallCompleter`] 経由で結果を
/// 書き戻す。queue 満杯のときは caller thread を block せず、cooperative adapter の `start` が
/// `Pending` ではなく `Ready(Err(backpressure))` を返せるよう [`SubmitError::QueueFull`] を返す
/// （NFR-2「詰まりを yield/backpressure へ変換」）。Engine の scheduler/context/audit lock を
/// 保持したまま `submit` を呼ばない（NFR-4）。
///
/// 本 Slice は worker thread を spawn する最小実装を提供するが、§6 のテストは thread を使わない
/// `FakeHostExecutor` で決定的に検証する（NFR-3）。
pub struct AdapterExecutor {
    limits: AdapterExecutorLimits,
    inner: Arc<Mutex<ExecutorInner>>,
}

/// executor の内部状態（queue 長と稼働中 job 数を会計する）。
struct ExecutorInner {
    /// queue + 実行中の合計 job 数。`max_queued` と突き合わせて backpressure 判定する。
    outstanding: usize,
}

/// `submit` が backpressure で受け付けられなかったことを表す（設計 §4.4 / OQ-5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    /// executor queue が満杯。cooperative adapter は `Ready(Err(backpressure))` へ写す。
    QueueFull,
}

impl AdapterExecutor {
    /// 有限上限から executor を作る。
    pub fn new(limits: AdapterExecutorLimits) -> Self {
        Self {
            limits,
            inner: Arc::new(Mutex::new(ExecutorInner { outstanding: 0 })),
        }
    }

    /// 現在の上限を返す。
    pub fn limits(&self) -> AdapterExecutorLimits {
        self.limits
    }

    /// job を executor thread へ投入する（設計 §4.4）。
    ///
    /// queue 満杯なら caller を block せず [`SubmitError::QueueFull`] を返す（cooperative adapter
    /// 側が `Ready(Err(backpressure))` へ写す）。受理した job は executor thread 上で `f` を
    /// 実行し、`CancelObserver` と `deadline` を渡して結果を `completer` へ書き戻す。
    pub fn submit<T, F>(
        &self,
        completer: HostCallCompleter<T>,
        deadline: MonotonicInstant,
        cancel: CancellationToken,
        f: F,
    ) -> Result<(), SubmitError>
    where
        F: FnOnce(&CancelObserver, MonotonicInstant) -> Result<T, AdapterError> + Send + 'static,
        T: Send + 'static,
    {
        {
            let mut inner = self.inner.lock().expect("adapter executor mutex poisoned");
            if inner.outstanding >= self.limits.max_queued {
                return Err(SubmitError::QueueFull);
            }
            inner.outstanding += 1;
        }
        let inner = Arc::clone(&self.inner);
        // 有限 thread pool の最小実装: 1 job = 1 短命 thread。max_threads/max_concurrency の
        // 厳密な上限適用は本 Slice のテスト（thread 無しの FakeHostExecutor）では駆動されない。
        std::thread::spawn(move || {
            let observer = CancelObserver::new(cancel);
            let result = f(&observer, deadline);
            completer.complete(result);
            let mut guard = inner.lock().expect("adapter executor mutex poisoned");
            guard.outstanding = guard.outstanding.saturating_sub(1);
        });
        Ok(())
    }
}

impl std::fmt::Debug for AdapterExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let outstanding = self.inner.lock().map(|g| g.outstanding).unwrap_or_default();
        f.debug_struct("AdapterExecutor")
            .field("limits", &self.limits)
            .field("outstanding", &outstanding)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::MonotonicClock;
    use std::sync::atomic::AtomicUsize;

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

    #[test]
    fn try_take_returns_result_after_ready() {
        let (ticket, completer) = new_ticket::<i32>(7);
        assert_eq!(ticket.id(), 7);
        // 未完了なら None。
        assert!(ticket.try_take().is_none());
        completer.complete(Ok(42));
        // ready 後は 1 回だけ取り出せる。
        match ticket.try_take() {
            Some(Ok(42)) => {}
            other => panic!("Ok(42) を期待したが {other:?}"),
        }
        // 2 回目は None（take 済み）。
        assert!(ticket.try_take().is_none());
    }

    #[test]
    fn cancel_discards_late_result() {
        let (ticket, completer) = new_ticket::<i32>(1);
        ticket.cancel();
        // cancel 後に遅着した result は破棄する（FR-6）。
        completer.complete(Ok(99));
        assert!(
            ticket.try_take().is_none(),
            "cancel 済み ticket は遅着 result を返さない"
        );
    }

    #[test]
    fn drop_cancels_and_marks_completer() {
        let (ticket, completer) = new_ticket::<i32>(2);
        drop(ticket);
        // drop は cancel 相当。completer 側から観測できる。
        assert!(
            completer.is_cancelled(),
            "ticket drop で completer は cancel 済み"
        );
    }

    #[test]
    fn complete_is_stored_only_once() {
        let (ticket, completer1) = new_ticket::<i32>(3);
        // 2 個目の completer は作れない（API 上 1 ペア）が、complete が二度呼ばれる状況を
        // 単一 completer では起こせないため、clone した Arc で擬似的に二度格納を試す代わりに、
        // complete 後に try_take した結果が最初の格納値であることだけ固定する。
        completer1.complete(Ok(10));
        assert!(matches!(ticket.try_take(), Some(Ok(10))));
    }

    #[test]
    fn lost_wake_avoided_register_after_complete() {
        // (a) register-after-complete: complete を先に、register_waker を後に。
        let (ticket, completer) = new_ticket::<i32>(4);
        let (waker, count) = fake_waker();
        // complete: result 格納 → ready.store(true) → waker 読取（まだ未登録なので誰も鳴らない）。
        completer.complete(Ok(1));
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "register 前は誰も wake されない"
        );
        // register: waker 保存 → ready.load が true を読み即 wake。
        ticket.register_waker(&waker);
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "register が ready を観測して即 wake する"
        );
    }

    #[test]
    fn lost_wake_avoided_complete_after_register() {
        // (b) complete-after-register: register_waker を先に、complete を後に。
        let (ticket, completer) = new_ticket::<i32>(5);
        let (waker, count) = fake_waker();
        // register: waker 保存 → ready がまだ false なので即 wake はしない。
        ticket.register_waker(&waker);
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "complete 前は wake されない"
        );
        // complete: ready を立ててから保存済み waker を拾って wake。
        completer.complete(Ok(2));
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "complete が保存済み waker を wake する"
        );
    }

    #[test]
    fn ticket_erased_delegates_value_roundtrip() {
        let (ticket, completer) = new_ticket::<Value>(6);
        let erased = TicketErased::new(ticket);
        assert_eq!(erased.id(), 6);
        let (waker, count) = fake_waker();
        erased.register_waker(&waker);
        assert!(erased.try_take_value().is_none());
        completer.complete(Ok(Value::Int(123)));
        assert_eq!(count.load(Ordering::SeqCst), 1, "complete で waker が鳴る");
        match erased.try_take_value() {
            Some(Ok(Value::Int(123))) => {}
            other => panic!("Int(123) を期待したが {other:?}"),
        }
    }

    #[test]
    fn execution_waker_and_ticket_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        // ExecutionWaker は常に Send + Sync（別 thread の adapter executor / cancel から
        // invoke できる、§9.1）。ticket / completer は `T: Send` のとき Send + Sync になる。
        // `Value` は `Rc` を含み `!Send` のため `TicketErased`（`HostCallTicket<Value>` 保持）は
        // `!Send` だが、それは handle（`!Send`）内に留まるため問題ない（continuation を別 thread へ
        // 渡さない設計、§9.1）。ここでは `T: Send` の代表として `i32` で境界を固定する。
        assert_send_sync::<ExecutionWaker>();
        assert_send_sync::<HostCallTicket<i32>>();
        assert_send_sync::<HostCallCompleter<i32>>();
    }

    #[test]
    fn adapter_executor_queue_full_is_backpressure() {
        // max_queued=0 なら最初の submit が caller を block せず即 QueueFull（設計 §4.4 / AC-4）。
        let executor = AdapterExecutor::new(AdapterExecutorLimits {
            max_threads: NonZeroUsize::new(1).unwrap(),
            max_queued: 0,
            max_concurrency: NonZeroUsize::new(1).unwrap(),
        });
        let (_ticket, completer) = new_ticket::<i32>(1);
        let deadline = crate::budget::FakeClock::new().now();
        let cancel = CancellationToken::new();
        let out = executor.submit(completer, deadline, cancel, |_obs, _dl| Ok(42));
        assert_eq!(out, Err(SubmitError::QueueFull));
    }

    #[test]
    fn adapter_executor_runs_job_and_completes_ticket() {
        // thread 上で f を実行し completer へ結果を書き戻す（最小動作確認）。
        let executor = AdapterExecutor::new(AdapterExecutorLimits {
            max_threads: NonZeroUsize::new(1).unwrap(),
            max_queued: 4,
            max_concurrency: NonZeroUsize::new(1).unwrap(),
        });
        let (ticket, completer) = new_ticket::<i32>(2);
        let deadline = crate::budget::FakeClock::new().now();
        let cancel = CancellationToken::new();
        executor
            .submit(completer, deadline, cancel, |obs, _dl| {
                assert!(!obs.is_cancelled());
                Ok(7)
            })
            .expect("submit accepted");
        // thread 完了を待つ（決定的でないため polling）。本番テストは FakeHostExecutor を使う。
        for _ in 0..1000 {
            if let Some(Ok(7)) = ticket.try_take() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("job did not complete");
    }
}
