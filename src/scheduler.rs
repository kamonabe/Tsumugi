//! Engine 全体で共有する協調制御の共有状態（REV-015 Slice 5、設計 §4.1/§4.2/§5.1-§5.3）。
//!
//! 本 module は「Engine が所有する `Send + Sync` な協調制御層」を閉じ込める。continuation
//! （frame stack / session / transaction journal）は含めず、slot トークンと wake handle だけが
//! thread 間を渡る。公開 `ExecutionHandle`（`engine.rs`）は `!Send + !Sync` のまま。
//!
//! # 本 module が持つもの
//!
//! - [`SchedulerShared`]: `Arc` で Engine が保持する共有状態。単一 `Mutex<SchedulerInner>` で
//!   admission（有限 active/queue slot の会計）と run-turn FIFO の線形化を担う（設計 §4.1 Finding 5）。
//! - [`EngineLimits`]: active/queue 上限と既定 slice fuel（設計 §4.1）。
//! - [`AdmissionSlot`]: handle が所有する RAII 解放トークン。drop で所有 slot を 1 個だけ解放する。
//! - [`SlotState`]: handle が poll 時に lock なしで「自分が Active へ昇格したか」を観測する
//!   read-only ミラー（書き込みは必ず `SchedulerShared` の Mutex クリティカルセクション内、Finding 5）。
//! - [`StartError`]: admission の失敗（backpressure / config / 同期 run の多重実行）。
//!
//! # host pending との境界（FEAT-002 で差し替え）
//!
//! [`QueuedEntry::waker`] は昇格通知のための waker を保持する field だが、`ExecutionWaker`
//! 型は host pending protocol（FEAT-002、`src/host_pending.rs`）で導入する。本 FEAT では
//! `Option<()>` の placeholder とし、昇格時の wake 配線は FEAT-002 が担う。

use std::collections::VecDeque;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::budget::ConfigError;

/// Engine 全体で共有する協調制御の共有状態（`Send + Sync`、`Arc` で保持する）。
///
/// continuation は含めない（ID と slot トークンだけ）。すべての会計操作は内部の単一
/// `Mutex<SchedulerInner>` を取り、slot 種別遷移とカウント増減を同一クリティカルセクションで
/// 直列化する（設計 §4.1 Finding 5、INV-6）。
pub struct SchedulerShared {
    inner: Mutex<SchedulerInner>,
}

struct SchedulerInner {
    /// この Engine の上限設定（設計 §4.1）。
    limits: EngineLimits,
    /// 占有中の active slot 数（active 会計の唯一の正本、INV-6）。
    active: usize,
    /// `AdmissionQueued` の FIFO 待ち行列。
    admission_queue: VecDeque<QueuedEntry>,
    /// Ready execution の run-turn FIFO。
    run_turn: VecDeque<ExecutionSlotId>,
    /// 次に発行する slot id。
    next_slot_id: u64,
}

/// admission queue の 1 エントリ。
struct QueuedEntry {
    slot_id: ExecutionSlotId,
    #[allow(dead_code)]
    resume_to: AdmissionPhase,
    /// 昇格通知の waker。`ExecutionWaker` は FEAT-002（host pending）で導入するため、本 FEAT
    /// では placeholder（`Option<()>`）とする。FEAT-002 でこれを `Option<ExecutionWaker>` に
    /// 差し替え、昇格時の wake を配線する。
    waker: Option<()>,
    /// この entry と handle が共有する slot 種別ミラー（昇格を store する）。
    slot_state: std::sync::Arc<SlotState>,
}

/// run-turn / admission で execution を識別する slot id（設計 §4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExecutionSlotId(u64);

impl ExecutionSlotId {
    /// 内部値を返す（test / debug 用）。
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// slot の種別（handle が poll 時に lock なしで読む read-only ミラー、設計 §4.1 Finding 5）。
///
/// `kind` の書き込みは必ず [`SchedulerShared`] の release/admit/昇格 method の Mutex
/// クリティカルセクション内でのみ行う。handle 側（別 thread 含む）は read-only で load する。
/// active カウントの正本ではない（正本は `SchedulerInner.active`、Mutex 下）。
#[derive(Debug)]
pub struct SlotState {
    kind: AtomicU8,
}

const SLOT_QUEUED: u8 = 0;
const SLOT_ACTIVE: u8 = 1;
const SLOT_RELEASED: u8 = 2;

impl SlotState {
    fn new(kind: u8) -> Self {
        Self {
            kind: AtomicU8::new(kind),
        }
    }

    fn load(&self) -> u8 {
        self.kind.load(Ordering::Acquire)
    }

    fn store(&self, kind: u8) {
        self.kind.store(kind, Ordering::Release);
    }

    /// handle 側の観測 API: Active へ昇格済みなら true（設計 §4.3 step 3）。
    pub fn is_active(&self) -> bool {
        self.load() == SLOT_ACTIVE
    }

    /// handle 側の観測 API: queue 待ち中なら true。
    pub fn is_queued(&self) -> bool {
        self.load() == SLOT_QUEUED
    }

    /// handle 側の観測 API: 既に解放済みなら true。
    pub fn is_released(&self) -> bool {
        self.load() == SLOT_RELEASED
    }
}

/// admission の phase（queue から戻る先、設計 §4.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionPhase {
    Created,
    Linked,
}

/// Engine 全体の上限設定（設計 §4.1 / §12）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineLimits {
    /// 同時に active にできる execution の上限（0 不可、型で保証）。
    /// 既定は `min(available_parallelism, 64)`、取得失敗時は 1。
    pub max_active_executions: NonZeroUsize,
    /// admission queue の上限（0 許可 = queue 無しで即 backpressure）。既定 256。
    pub max_queued_executions: usize,
    /// 既定の slice fuel（設計 §4.2、既定 10,000）。
    pub default_slice_fuel: NonZeroU64,
}

impl Default for EngineLimits {
    fn default() -> Self {
        let max_active = std::thread::available_parallelism()
            .map(|n| n.get().min(64))
            .unwrap_or(1);
        Self {
            // available_parallelism は 1 以上、min(_,64) も 1 以上なので unwrap は安全。
            max_active_executions: NonZeroUsize::new(max_active).unwrap_or(NonZeroUsize::MIN),
            max_queued_executions: 256,
            default_slice_fuel: NonZeroU64::new(10_000).expect("10_000 は非ゼロ"),
        }
    }
}

/// handle が所有する slot 解放トークン（RAII、設計 §4.1）。
///
/// 会計カウンタではなく「drop 時に正本から 1 個引く権利」に徹する（INV-6）。`Active` /
/// `Queued` のどちらかを 1 個だけ所有し、[`Drop`] で [`SchedulerShared::release_active`] /
/// [`SchedulerShared::release_queued`] を Mutex 経由で呼んで 1 回だけ解放する。二重解放は
/// `SlotState` の Released 判定で no-op になる。
pub struct AdmissionSlot {
    shared: std::sync::Arc<SchedulerShared>,
    kind: AdmissionSlotKind,
    slot_id: ExecutionSlotId,
    slot_state: std::sync::Arc<SlotState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionSlotKind {
    Active,
    Queued,
}

impl AdmissionSlot {
    /// この slot の id を返す。
    pub fn slot_id(&self) -> ExecutionSlotId {
        self.slot_id
    }

    /// この slot が現在 active を占めているか（昇格済みを含む）。
    pub fn is_active(&self) -> bool {
        self.slot_state.is_active()
    }

    /// この slot と共有する [`SlotState`] ミラーの `Arc` clone を返す（設計 §4.1）。
    ///
    /// handle はこれを保持し、poll 時に lock なしで「自分が Active へ昇格したか」を観測する。
    pub fn slot_state_arc(&self) -> std::sync::Arc<SlotState> {
        std::sync::Arc::clone(&self.slot_state)
    }
}

impl std::fmt::Debug for AdmissionSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionSlot")
            .field("kind", &self.kind)
            .field("slot_id", &self.slot_id)
            .field("slot_state", &self.slot_state.load())
            .finish()
    }
}

impl Drop for AdmissionSlot {
    fn drop(&mut self) {
        match self.kind {
            AdmissionSlotKind::Active => self.shared.release_active(self.slot_id, &self.slot_state),
            AdmissionSlotKind::Queued => {
                // queue 中に昇格していれば release_active 相当で扱う。SlotState が Active を
                // 指していれば active slot を解放する（昇格後の drop）。それ以外は queue 解放。
                if self.slot_state.is_active() {
                    self.shared.release_active(self.slot_id, &self.slot_state);
                } else {
                    self.shared.release_queued(self.slot_id, &self.slot_state);
                }
            }
        }
    }
}

/// admission / 同期 run の失敗（設計 §5.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// active / queue が共に満杯。handle を作らず context を変更しない（AC-1）。
    Backpressure {
        active: usize,
        queued: usize,
        limit: usize,
    },
    /// 設定ミス（foreign clock 等）。
    Config(ConfigError),
    /// 同期 `Engine::run` が他に非 terminal handle があるのに呼ばれた（設計 §5.1 Finding 9）。
    ///
    /// 本 FEAT では variant を定義するが検出ロジックは配線しない（単一 execution の同期入口は
    /// 構造的にこの状況を起こさない）。複数 active を同期入口から起こせる将来 Phase で配線する。
    ConcurrentRunRequiresPolling,
}

impl SchedulerShared {
    /// 既定上限で共有状態を作る。
    pub fn new() -> Self {
        Self::with_limits(EngineLimits::default())
    }

    /// 指定した上限で共有状態を作る。
    pub fn with_limits(limits: EngineLimits) -> Self {
        Self {
            inner: Mutex::new(SchedulerInner {
                limits,
                active: 0,
                admission_queue: VecDeque::new(),
                run_turn: VecDeque::new(),
                next_slot_id: 0,
            }),
        }
    }

    /// この Engine の上限設定を返す。
    pub fn limits(&self) -> EngineLimits {
        self.inner.lock().expect("scheduler mutex poisoned").limits
    }

    /// admission 予約（設計 §4.1）。1 回の Mutex ロック内で:
    /// 1. `active < max_active` なら `active += 1` し `AdmissionSlot::Active` を返す。
    /// 2. そうでなく `queue.len() < max_queued` なら queue へ push し `AdmissionSlot::Queued` を返す。
    /// 3. どちらも満杯なら `Err(StartError::Backpressure)`。
    pub fn admit(
        self: &std::sync::Arc<Self>,
        resume_to: AdmissionPhase,
    ) -> Result<AdmissionSlot, StartError> {
        let mut inner = self.inner.lock().expect("scheduler mutex poisoned");
        let slot_id = ExecutionSlotId(inner.next_slot_id);
        inner.next_slot_id += 1;

        if inner.active < inner.limits.max_active_executions.get() {
            inner.active += 1;
            let slot_state = std::sync::Arc::new(SlotState::new(SLOT_ACTIVE));
            Ok(AdmissionSlot {
                shared: std::sync::Arc::clone(self),
                kind: AdmissionSlotKind::Active,
                slot_id,
                slot_state,
            })
        } else if inner.admission_queue.len() < inner.limits.max_queued_executions {
            let slot_state = std::sync::Arc::new(SlotState::new(SLOT_QUEUED));
            inner.admission_queue.push_back(QueuedEntry {
                slot_id,
                resume_to,
                waker: None,
                slot_state: std::sync::Arc::clone(&slot_state),
            });
            Ok(AdmissionSlot {
                shared: std::sync::Arc::clone(self),
                kind: AdmissionSlotKind::Queued,
                slot_id,
                slot_state,
            })
        } else {
            Err(StartError::Backpressure {
                active: inner.active,
                queued: inner.admission_queue.len(),
                limit: inner.limits.max_active_executions.get(),
            })
        }
    }

    /// active slot を解放し、FIFO 先頭を昇格する（設計 §4.1 Finding 5）。
    ///
    /// 既に Released なら no-op（二重解放ガード）。まだ Active なら `SlotState → Released` に
    /// store し `active -= 1`、続けて同じ lock 下で FIFO 昇格（queue 先頭を pop、`active += 1`、
    /// 昇格 entry の `SlotState → Active`、waker 退避）する。退避 waker は lock 解放後に wake する。
    fn release_active(&self, slot_id: ExecutionSlotId, slot_state: &SlotState) {
        let promoted_waker = {
            let mut inner = self.inner.lock().expect("scheduler mutex poisoned");
            if slot_state.is_released() {
                return;
            }
            slot_state.store(SLOT_RELEASED);
            inner.active -= 1;
            // active が空いたので run-turn からも外す（terminal/drop 整合）。
            if let Some(pos) = inner.run_turn.iter().position(|id| *id == slot_id) {
                inner.run_turn.remove(pos);
            }

            // FIFO 昇格: queue 先頭を 1 件取り出し active へ付け替える。active 純増は 1 回だけ。
            if let Some(entry) = inner.admission_queue.pop_front() {
                inner.active += 1;
                entry.slot_state.store(SLOT_ACTIVE);
                entry.waker
            } else {
                None
            }
        };
        // lock 解放後に退避 waker を wake する（設計 §4.1）。FEAT-002 で ExecutionWaker を配線する。
        if let Some(()) = promoted_waker {
            // placeholder: FEAT-002 が waker.wake() を行う。
        }
    }

    /// queue slot を解放する（設計 §4.1 Finding 5）。Released なら no-op。まだ Queued なら
    /// queue から entry を除去し `SlotState → Released`（`active` は不変）。
    fn release_queued(&self, slot_id: ExecutionSlotId, slot_state: &SlotState) {
        let mut inner = self.inner.lock().expect("scheduler mutex poisoned");
        if slot_state.is_released() {
            return;
        }
        if let Some(pos) = inner
            .admission_queue
            .iter()
            .position(|e| e.slot_id == slot_id)
        {
            inner.admission_queue.remove(pos);
        }
        slot_state.store(SLOT_RELEASED);
    }

    /// run-turn FIFO 末尾へ登録する（設計 §4.1、§9.2 の Linked→Ready）。既に登録済みなら no-op。
    pub fn run_turn_push_back(&self, slot_id: ExecutionSlotId) {
        let mut inner = self.inner.lock().expect("scheduler mutex poisoned");
        if !inner.run_turn.iter().any(|id| *id == slot_id) {
            inner.run_turn.push_back(slot_id);
        }
    }

    /// run-turn の先頭が `slot_id` のとき true（設計 §4.1 Finding 4）。
    ///
    /// `slot_id` が run-turn に存在しない場合は false（未登録 slot に対する戻りを明示）。
    pub fn is_head(&self, slot_id: ExecutionSlotId) -> bool {
        let inner = self.inner.lock().expect("scheduler mutex poisoned");
        inner.run_turn.front() == Some(&slot_id)
    }

    /// run-turn の先頭を末尾へ回す（slice yield 時、設計 §4.1 / §12.1 規則 2/3）。
    pub fn run_turn_rotate(&self, slot_id: ExecutionSlotId) {
        let mut inner = self.inner.lock().expect("scheduler mutex poisoned");
        if inner.run_turn.front() == Some(&slot_id) {
            let id = inner.run_turn.pop_front().expect("front が存在する");
            inner.run_turn.push_back(id);
        }
    }

    /// run-turn から外す（terminal / host-call pending 進入時、設計 §4.1）。
    pub fn run_turn_remove(&self, slot_id: ExecutionSlotId) {
        let mut inner = self.inner.lock().expect("scheduler mutex poisoned");
        if let Some(pos) = inner.run_turn.iter().position(|id| *id == slot_id) {
            inner.run_turn.remove(pos);
        }
    }
}

impl Default for SchedulerShared {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn limits(active: usize, queued: usize) -> EngineLimits {
        EngineLimits {
            max_active_executions: NonZeroUsize::new(active).expect("active 上限は非ゼロ"),
            max_queued_executions: queued,
            default_slice_fuel: NonZeroU64::new(10_000).unwrap(),
        }
    }

    #[test]
    fn admit_at_exact_active_and_queue_limits_then_plus_one_backpressure() {
        let shared = Arc::new(SchedulerShared::with_limits(limits(2, 1)));
        // active 上限ちょうど（2 個）を受理。
        let a1 = shared.admit(AdmissionPhase::Created).expect("1 個目");
        let a2 = shared.admit(AdmissionPhase::Created).expect("2 個目");
        assert!(a1.is_active());
        assert!(a2.is_active());
        // queue 上限ちょうど（1 個）を受理。
        let q1 = shared.admit(AdmissionPhase::Created).expect("queue 1 個目");
        assert!(!q1.is_active());
        // +1 は即時 Backpressure（handle 無し）。
        match shared.admit(AdmissionPhase::Created) {
            Err(StartError::Backpressure {
                active,
                queued,
                limit,
            }) => {
                assert_eq!(active, 2);
                assert_eq!(queued, 1);
                assert_eq!(limit, 2);
            }
            other => panic!("Backpressure を期待したが {other:?}"),
        }
        drop((a1, a2, q1));
    }

    #[test]
    fn release_active_promotes_fifo_head_and_keeps_active_single_sourced() {
        let shared = Arc::new(SchedulerShared::with_limits(limits(1, 4)));
        let a = shared.admit(AdmissionPhase::Created).expect("active");
        let q1 = shared.admit(AdmissionPhase::Created).expect("queue 1");
        let q2 = shared.admit(AdmissionPhase::Created).expect("queue 2");
        assert!(a.is_active());
        assert!(!q1.is_active());
        assert!(!q2.is_active());

        // active を解放すると FIFO 先頭（q1）が昇格する。active は常に単一正本で 1 のまま。
        drop(a);
        assert!(q1.is_active(), "FIFO 先頭 q1 が Active へ昇格する");
        assert!(!q2.is_active(), "q2 はまだ queue 待ち");
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(inner.active, 1, "active は単一正本で 1（昇格は付け替え）");
            assert_eq!(inner.admission_queue.len(), 1, "q2 のみ queue に残る");
        }

        // 次の解放で q2 が昇格する。
        drop(q1);
        assert!(q2.is_active());
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(inner.active, 1);
            assert_eq!(inner.admission_queue.len(), 0);
        }
        drop(q2);
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(inner.active, 0);
        }
    }

    #[test]
    fn release_queued_leaves_active_unchanged() {
        let shared = Arc::new(SchedulerShared::with_limits(limits(1, 4)));
        let a = shared.admit(AdmissionPhase::Created).expect("active");
        let q1 = shared.admit(AdmissionPhase::Created).expect("queue 1");
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(inner.active, 1);
            assert_eq!(inner.admission_queue.len(), 1);
        }
        // queue slot を drop しても active は不変、queue のみ減る。
        drop(q1);
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(inner.active, 1, "queue 解放で active は変わらない");
            assert_eq!(inner.admission_queue.len(), 0);
        }
        drop(a);
    }

    #[test]
    fn admission_slot_double_release_is_noop() {
        let shared = Arc::new(SchedulerShared::with_limits(limits(1, 0)));
        let a = shared.admit(AdmissionPhase::Created).expect("active");
        // 明示的に release_active を 2 回呼んでも active は 1 回しか減らない（Released ガード）。
        let slot_id = a.slot_id();
        // AdmissionSlot の内部 SlotState を取り出すため、clone 不可なので手動で検証する。
        // 1 回目: 直接 release。
        shared.release_active(slot_id, &a.slot_state);
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(inner.active, 0);
        }
        // 2 回目（a の drop 相当）: Released なので no-op。
        shared.release_active(slot_id, &a.slot_state);
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(inner.active, 0, "二重解放は no-op");
        }
        // a の実 drop もさらに no-op。
        drop(a);
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(inner.active, 0);
        }
    }

    #[test]
    fn run_turn_fifo_order_push_is_head_rotate_remove() {
        let shared = Arc::new(SchedulerShared::with_limits(limits(8, 0)));
        let a = shared.admit(AdmissionPhase::Created).unwrap();
        let b = shared.admit(AdmissionPhase::Created).unwrap();
        let c = shared.admit(AdmissionPhase::Created).unwrap();
        let (ida, idb, idc) = (a.slot_id(), b.slot_id(), c.slot_id());

        shared.run_turn_push_back(ida);
        shared.run_turn_push_back(idb);
        shared.run_turn_push_back(idc);
        // 二重登録は no-op。
        shared.run_turn_push_back(ida);

        assert!(shared.is_head(ida));
        assert!(!shared.is_head(idb));

        // rotate: 先頭 a を末尾へ回すと b が head。
        shared.run_turn_rotate(ida);
        assert!(shared.is_head(idb));
        shared.run_turn_rotate(idb);
        assert!(shared.is_head(idc));
        shared.run_turn_rotate(idc);
        assert!(shared.is_head(ida), "一巡して a が再び head");

        // remove: head を外すと次が head。
        shared.run_turn_remove(ida);
        assert!(shared.is_head(idb));
        shared.run_turn_remove(idb);
        shared.run_turn_remove(idc);
        assert!(!shared.is_head(ida));

        drop((a, b, c));
    }

    #[test]
    fn is_head_false_for_unregistered_slot() {
        let shared = Arc::new(SchedulerShared::with_limits(limits(4, 0)));
        let a = shared.admit(AdmissionPhase::Created).unwrap();
        // 未登録 slot に対しては false（設計 §4.1 Finding 4）。
        assert!(!shared.is_head(a.slot_id()));
        drop(a);
    }
}
