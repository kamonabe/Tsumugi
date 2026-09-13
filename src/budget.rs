//! 実行予算（REV-015 / Phase 3）の公開型と課金台帳。
//!
//! 設計正本は [`docs/execution-control.md`](../docs/execution-control.md) 第3〜7節である。
//! 本モジュールは Slice 1（budget型と legacy adapter）と、Slice 2 のうち string
//! accounting サブスライスを実装する。すなわち:
//!
//! - `BudgetConfig` / `BudgetUsage` / `BudgetExceeded` などの公開型（§3）
//! - `BudgetConfig::standard` の既定値（§3.1）
//! - checked arithmetic による reserve / commit / refund（§7）と、複数超過時の
//!   固定優先順位（§7.2）
//! - 既存の step / collection 検査を一本化する [`BudgetLedger`]
//! - string accounting（§5.3）: per-item `SingleStringBytes` と cumulative
//!   `StringAllocations` / `StringBytes` を [`BudgetLedger::charge_string`] /
//!   [`BudgetLedger::charge_result_strings`] で課金する。現行の共有 builtin handler
//!   が新規生成する String body に配線済み。
//!
//! Slice 1 の時点では実行系は同期のままで、`ExecutionHandle` / `poll` / scheduler
//! （Slice 3 以降）は実装しない。fuel の全 charge point 展開と heap/source/I-O
//! accounting、および string リテラル・連結・f-string 経路の課金は Slice 2 の
//! 残りサブスライスとして本モジュールへ積み増す。
//!
//! # 不変条件（§2）
//!
//! - すべての execution は有限の `BudgetConfig` を持つ。無制限 sentinel は提供しない。
//! - usage は単調に維持する（live heap の release だけが減少できる。Slice 2 で導入）。
//! - budget 超過は script の `try` / `catch` から捕捉できない。本モジュールは
//!   [`ControlStop`] を返すだけで、engine 側が catch 不能な terminal として扱う。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// heap 論理サイズ表の revision。初期実装は 1 だけを受理する（§3.1 / §5.1）。
pub const HEAP_ACCOUNTING_REVISION: u32 = 1;

const MIB: u64 = 1_048_576;

// =============================================================================
// clock（§3）
// =============================================================================

/// 注入した [`MonotonicClock`] だけが生成する、単調増加の ns tick（§3）。
///
/// clock domain を型で縛るため、`MonotonicInstant` は `clock_id` を保持する。
/// 異なる clock instance から作られた instant を混ぜると
/// [`ConfigError::ForeignClock`] で拒否できる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonotonicInstant {
    clock_id: u64,
    ns: u64,
}

impl MonotonicInstant {
    /// この instant が属する clock domain の識別子。
    pub fn clock_id(&self) -> u64 {
        self.clock_id
    }

    /// epoch からの ns tick。
    pub fn as_nanos(&self) -> u64 {
        self.ns
    }
}

/// deadline 用の単調増加 clock（§3）。
///
/// 各 instance は一意な `clock_id` を持ち、生成する [`MonotonicInstant`] にそれを刻む。
/// テストでは [`FakeClock`] を使い、任意時刻へ進める。
pub trait MonotonicClock {
    /// この clock の domain 識別子。生成する instant はこの id を持つ。
    fn clock_id(&self) -> u64;
    /// 現在時刻を返す。
    fn now(&self) -> MonotonicInstant;
    /// この clock domain の ns tick から instant を組み立てる。
    fn instant_at(&self, ns: u64) -> MonotonicInstant {
        MonotonicInstant {
            clock_id: self.clock_id(),
            ns,
        }
    }
}

/// clock_id を配る process-global counter。0 は「未設定 / 任意」を表さない
/// （id は 1 から始める）。
fn next_clock_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// テスト用の手動 clock（§15.1 の「fake clock」）。
///
/// `advance` で任意 ns 進められ、deadline の境界挙動を決定的に検証できる。
pub struct FakeClock {
    clock_id: u64,
    now_ns: AtomicU64,
}

impl FakeClock {
    /// ns=0 から始まる新しい fake clock を作る。
    pub fn new() -> Self {
        Self {
            clock_id: next_clock_id(),
            now_ns: AtomicU64::new(0),
        }
    }

    /// 現在時刻を `delta_ns` だけ進める。overflow は saturating（テスト補助）。
    pub fn advance(&self, delta_ns: u64) {
        self.now_ns.fetch_add(delta_ns, Ordering::Relaxed);
    }

    /// 現在時刻を絶対 ns へ設定する。
    pub fn set(&self, ns: u64) {
        self.now_ns.store(ns, Ordering::Relaxed);
    }
}

impl MonotonicClock for FakeClock {
    fn clock_id(&self) -> u64 {
        self.clock_id
    }
    fn now(&self) -> MonotonicInstant {
        MonotonicInstant {
            clock_id: self.clock_id,
            ns: self.now_ns.load(Ordering::Relaxed),
        }
    }
}

// =============================================================================
// config（§3 / §3.1）
// =============================================================================

/// execution 作成時の設定エラー（§3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `heap_accounting_revision` が 1 以外。
    UnsupportedAccountingRevision(u32),
    /// deadline が別 clock domain の instant。
    ForeignClock,
    /// deadline が作成時点以前。
    DeadlineNotInFuture,
    /// 既定値生成時の addition overflow など。
    Overflow,
}

/// execution 全体の資源上限（§3）。field 名と単位は公開契約である。
///
/// 無制限を表す sentinel は持たない。すべての上限は 0 を許し、該当操作を最初から
/// 禁止できる。deadline だけは作成時点より後でなければならない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetConfig {
    /// 論理 accounting 契約。初期実装は 1 だけを受理する。
    pub heap_accounting_revision: u32,

    /// 論理 fuel unit。
    pub total_fuel: u64,

    /// logical byte / count。
    pub max_live_heap_bytes: u64,
    pub max_string_allocations: u64,
    pub max_string_bytes: u64,
    pub max_single_string_bytes: u64,
    pub max_source_count: u32,
    pub max_source_bytes: u64,
    pub max_single_source_bytes: u64,
    pub max_import_count: u32,
    pub max_import_bytes: u64,
    pub max_collection_elements: u64,

    /// call count / payload byte。
    pub max_input_calls: u64,
    pub max_input_bytes: u64,
    pub max_output_calls: u64,
    pub max_output_bytes: u64,
    pub max_host_calls: u64,
    pub max_host_request_bytes: u64,
    pub max_host_response_bytes: u64,
    pub max_host_call_bytes: u64,

    /// 注入 clock と同じ domain の絶対時刻。単位は ns。
    pub deadline: MonotonicInstant,
}

impl BudgetConfig {
    /// 既定の予算を作る（§3.1）。deadline は `clock.now() + 30 s`。
    ///
    /// addition overflow または deadline が作成時点以前になる場合は
    /// [`ConfigError::Overflow`] / [`ConfigError::DeadlineNotInFuture`] を返す。
    pub fn standard(clock: &dyn MonotonicClock) -> Result<Self, ConfigError> {
        let now = clock.now();
        let deadline_ns = now
            .as_nanos()
            .checked_add(30 * 1_000_000_000)
            .ok_or(ConfigError::Overflow)?;
        let deadline = clock.instant_at(deadline_ns);
        Ok(Self {
            heap_accounting_revision: HEAP_ACCOUNTING_REVISION,
            total_fuel: 1_000_000,
            max_live_heap_bytes: 64 * MIB,
            max_string_allocations: 1_000_000,
            max_string_bytes: 64 * MIB,
            max_single_string_bytes: 8 * MIB,
            max_source_count: 1_025,
            max_source_bytes: 16 * MIB,
            max_single_source_bytes: 2 * MIB,
            max_import_count: 1_024,
            max_import_bytes: 16 * MIB,
            max_collection_elements: 1_000_000,
            max_input_calls: 10_000,
            max_input_bytes: 8 * MIB,
            max_output_calls: 10_000,
            max_output_bytes: 8 * MIB,
            max_host_calls: 10_000,
            max_host_request_bytes: 8 * MIB,
            max_host_response_bytes: 16 * MIB,
            max_host_call_bytes: 24 * MIB,
            deadline,
        })
    }

    /// legacy 環境変数（`TSUMUGI_MAX_STEPS` / `TSUMUGI_MAX_COLLECTION_SIZE`）から
    /// Slice 1 用の config を組み立てる（§13 の移行入口）。
    ///
    /// Slice 1 で実際に課金するのは fuel（= step）と collection 要素数だけなので、
    /// その 2 field を引数で受け取り、他の field は既定値相当を置く。deadline は
    /// Slice 1 では ledger の charge 経路で確認しないため、内部 clock の遠い未来を置く。
    /// deadline checkpoint の配線は Slice 4 で行う。
    pub fn for_legacy(total_fuel: u64, max_collection_elements: u64) -> Self {
        // 内部 clock 由来の遠い未来 deadline。Slice 1 では ledger が参照しない。
        let deadline = MonotonicInstant {
            clock_id: next_clock_id(),
            ns: u64::MAX,
        };
        Self {
            heap_accounting_revision: HEAP_ACCOUNTING_REVISION,
            total_fuel,
            max_live_heap_bytes: 64 * MIB,
            max_string_allocations: 1_000_000,
            max_string_bytes: 64 * MIB,
            max_single_string_bytes: 8 * MIB,
            max_source_count: 1_025,
            max_source_bytes: 16 * MIB,
            max_single_source_bytes: 2 * MIB,
            max_import_count: 1_024,
            max_import_bytes: 16 * MIB,
            max_collection_elements,
            max_input_calls: 10_000,
            max_input_bytes: 8 * MIB,
            max_output_calls: 10_000,
            max_output_bytes: 8 * MIB,
            max_host_calls: 10_000,
            max_host_request_bytes: 8 * MIB,
            max_host_response_bytes: 16 * MIB,
            max_host_call_bytes: 24 * MIB,
            deadline,
        }
    }

    /// legacy 環境変数から config を組み立てる（§13）。
    ///
    /// `TSUMUGI_MAX_STEPS` を `total_fuel`、`TSUMUGI_MAX_COLLECTION_SIZE` を
    /// `max_collection_elements` に写す。未設定・不正値は既定（各 1,000,000）。
    /// process-global な互換入口であり、library 埋め込みは明示 `BudgetConfig` を使う。
    pub fn from_legacy_env() -> Self {
        let total_fuel = std::env::var("TSUMUGI_MAX_STEPS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1_000_000);
        let max_collection_elements = std::env::var("TSUMUGI_MAX_COLLECTION_SIZE")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1_000_000);
        let mut config = Self::for_legacy(total_fuel, max_collection_elements);
        // string accounting（REV-015 Slice 2）の legacy 上限入口。未設定は §3.1 の既定値。
        if let Some(v) = std::env::var("TSUMUGI_MAX_SINGLE_STRING_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            config.max_single_string_bytes = v;
        }
        if let Some(v) = std::env::var("TSUMUGI_MAX_STRING_ALLOCATIONS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            config.max_string_allocations = v;
        }
        if let Some(v) = std::env::var("TSUMUGI_MAX_STRING_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            config.max_string_bytes = v;
        }
        // source accounting（REV-015 Slice 2）の legacy 上限入口。未設定は §3.1 の既定値。
        if let Some(v) = std::env::var("TSUMUGI_MAX_SINGLE_SOURCE_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            config.max_single_source_bytes = v;
        }
        if let Some(v) = std::env::var("TSUMUGI_MAX_SOURCE_COUNT")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
        {
            config.max_source_count = v;
        }
        if let Some(v) = std::env::var("TSUMUGI_MAX_SOURCE_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            config.max_source_bytes = v;
        }
        if let Some(v) = std::env::var("TSUMUGI_MAX_IMPORT_COUNT")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
        {
            config.max_import_count = v;
        }
        if let Some(v) = std::env::var("TSUMUGI_MAX_IMPORT_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            config.max_import_bytes = v;
        }
        config
    }

    /// config の妥当性を検証する（§3）。execution 作成前に呼ぶ。
    ///
    /// - `heap_accounting_revision` が 1 以外なら拒否。
    /// - deadline が `clock` と別 domain なら拒否。
    /// - deadline が `clock.now()` 以前なら拒否。
    pub fn validate(&self, clock: &dyn MonotonicClock) -> Result<(), ConfigError> {
        if self.heap_accounting_revision != HEAP_ACCOUNTING_REVISION {
            return Err(ConfigError::UnsupportedAccountingRevision(
                self.heap_accounting_revision,
            ));
        }
        if self.deadline.clock_id() != clock.clock_id() {
            return Err(ConfigError::ForeignClock);
        }
        if self.deadline.as_nanos() <= clock.now().as_nanos() {
            return Err(ConfigError::DeadlineNotInFuture);
        }
        Ok(())
    }
}

// =============================================================================
// usage / counters（§3）
// =============================================================================

/// cumulative resource の累積値（§3）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetCounters {
    pub fuel: u64,
    pub string_allocations: u64,
    pub string_bytes: u64,
    pub source_count: u32,
    pub source_bytes: u64,
    pub import_count: u32,
    pub import_bytes: u64,
    pub input_calls: u64,
    pub input_bytes: u64,
    pub output_calls: u64,
    pub output_bytes: u64,
    pub host_calls: u64,
    pub host_request_bytes: u64,
    pub host_response_bytes: u64,
    pub host_call_bytes: u64,
}

/// per-item resource の最大値（§3）。累積せず `max(old, candidate)` で更新する。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetPeaks {
    pub single_string_bytes: u64,
    pub single_source_bytes: u64,
    pub collection_elements: u64,
}

/// execution の観測可能な予算使用量（§3）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetUsage {
    pub committed: BudgetCounters,
    pub reserved: BudgetCounters,
    pub live_heap_bytes: u64,
    pub reserved_heap_bytes: u64,
    pub peak_heap_bytes: u64,
    pub peaks: BudgetPeaks,
}

// =============================================================================
// 超過（§3 / §7）
// =============================================================================

/// 課金の単位（§3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetUnit {
    Fuel,
    Bytes,
    Count,
    Elements,
}

/// 予算 resource の種別（§3）。§7.2 の固定優先順位に対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetResource {
    Fuel,
    HeapBytes,
    SingleStringBytes,
    StringAllocations,
    StringBytes,
    SingleSourceBytes,
    SourceCount,
    SourceBytes,
    ImportCount,
    ImportBytes,
    CollectionElements,
    InputCalls,
    InputBytes,
    OutputCalls,
    OutputBytes,
    HostCalls,
    HostRequestBytes,
    HostResponseBytes,
    HostCallBytes,
}

impl BudgetResource {
    /// §7.2 の固定優先順位（小さいほど優先。複数超過時の primary 選択に使う）。
    fn priority(self) -> u8 {
        match self {
            BudgetResource::Fuel => 1,
            BudgetResource::HeapBytes => 2,
            BudgetResource::SingleStringBytes => 3,
            BudgetResource::StringAllocations => 4,
            BudgetResource::StringBytes => 5,
            BudgetResource::SingleSourceBytes => 6,
            BudgetResource::SourceCount => 7,
            BudgetResource::SourceBytes => 8,
            BudgetResource::ImportCount => 9,
            BudgetResource::ImportBytes => 10,
            BudgetResource::CollectionElements => 11,
            BudgetResource::InputCalls => 12,
            BudgetResource::InputBytes => 13,
            BudgetResource::OutputCalls => 14,
            BudgetResource::OutputBytes => 15,
            BudgetResource::HostCalls => 16,
            BudgetResource::HostRequestBytes => 17,
            BudgetResource::HostResponseBytes => 18,
            BudgetResource::HostCallBytes => 19,
        }
    }

    /// この resource の課金単位。
    fn unit(self) -> BudgetUnit {
        match self {
            BudgetResource::Fuel => BudgetUnit::Fuel,
            BudgetResource::SourceCount
            | BudgetResource::ImportCount
            | BudgetResource::StringAllocations
            | BudgetResource::InputCalls
            | BudgetResource::OutputCalls
            | BudgetResource::HostCalls => BudgetUnit::Count,
            BudgetResource::CollectionElements => BudgetUnit::Elements,
            _ => BudgetUnit::Bytes,
        }
    }
}

/// 予算超過の詳細（§3）。terminal outcome の payload になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded {
    pub resource: BudgetResource,
    pub limit: u64,
    pub used: u64,
    pub reserved: u64,
    pub requested: u64,
    pub unit: BudgetUnit,
    pub phase: ExecutionPhase,
}

/// 課金が発生した実行フェーズ（§3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPhase {
    Compile,
    Link,
    Run,
    HostCall,
    Commit,
}

/// reserve / charge が返す停止理由（§7）。
///
/// budget 超過・deadline・cancel はいずれも script から捕捉できない。engine は
/// これを catch 不能な terminal へ写像する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlStop {
    Cancelled,
    DeadlineExceeded {
        deadline: MonotonicInstant,
        observed: MonotonicInstant,
    },
    BudgetExceeded(BudgetExceeded),
}

// =============================================================================
// cancel（§8。Slice 1 では token 型だけ用意し、確認点は Slice 4 で拡張）
// =============================================================================

/// 協調的キャンセル token（§8）。
///
/// `cancel()` は idempotent で、最初の `false -> true` を linearization point とする。
/// Slice 1 では [`BudgetLedger`] の charge 前確認だけで使い、waker 連携は Slice 4。
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    flag: Arc<std::sync::atomic::AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// キャンセルを要求する。既に要求済みなら何もしない。
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// キャンセル済みか。
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

// =============================================================================
// charge の要求（§7）
// =============================================================================

/// cumulative resource への 1 回の課金要求（§7）。
///
/// Slice 1 で実際に配線するのは `Fuel` だけだが、reserve/commit の枠組みは
/// 全 cumulative resource で共有できるよう resource を明示的に受け取る。
#[derive(Debug, Clone, Copy)]
pub struct CumulativeRequest {
    pub resource: BudgetResource,
    pub amount: u64,
}

// =============================================================================
// 課金台帳（BudgetLedger）
// =============================================================================

/// execution ごとの課金台帳。
///
/// 既存の step 課金（fuel）と collection 要素数検査（per-item）を一本化する入口。
/// Slice 2 以降で string / source / heap / I-O の課金をここへ積み増す。
///
/// # reserve / commit / refund（§7）
///
/// cumulative resource は「reserve でまず `used + reserved + requested` を上限と比較し、
/// 超えなければ `reserved` へ積む」→「commit で `reserved` から `actual` を減らし
/// `committed` へ移す」→「refund で未使用分を戻す」の流れで課金する。Slice 1 の
/// fuel charge は `amount` を確定値として reserve→commit を 1 回で行う薄いヘルパ
/// [`BudgetLedger::charge_fuel`] を提供する。
pub struct BudgetLedger {
    config: BudgetConfig,
    committed: BudgetCounters,
    reserved: BudgetCounters,
    peaks: BudgetPeaks,
    cancellation: CancellationToken,
}

impl BudgetLedger {
    /// config と cancellation token から台帳を作る。
    pub fn new(config: BudgetConfig, cancellation: CancellationToken) -> Self {
        Self {
            config,
            committed: BudgetCounters::default(),
            reserved: BudgetCounters::default(),
            peaks: BudgetPeaks::default(),
            cancellation,
        }
    }

    /// cancellation token を共有せず、単体で使う台帳を作る（テスト補助）。
    pub fn with_config(config: BudgetConfig) -> Self {
        Self::new(config, CancellationToken::new())
    }

    /// 現在の使用量 snapshot（§3）。
    pub fn usage(&self) -> BudgetUsage {
        BudgetUsage {
            committed: self.committed,
            reserved: self.reserved,
            live_heap_bytes: 0,
            reserved_heap_bytes: 0,
            peak_heap_bytes: 0,
            peaks: self.peaks,
        }
    }

    /// この resource の現在の committed 値。
    fn committed_of(&self, resource: BudgetResource) -> u64 {
        match resource {
            BudgetResource::Fuel => self.committed.fuel,
            BudgetResource::StringAllocations => self.committed.string_allocations,
            BudgetResource::StringBytes => self.committed.string_bytes,
            BudgetResource::SourceCount => self.committed.source_count as u64,
            BudgetResource::SourceBytes => self.committed.source_bytes,
            BudgetResource::ImportCount => self.committed.import_count as u64,
            BudgetResource::ImportBytes => self.committed.import_bytes,
            BudgetResource::InputCalls => self.committed.input_calls,
            BudgetResource::InputBytes => self.committed.input_bytes,
            BudgetResource::OutputCalls => self.committed.output_calls,
            BudgetResource::OutputBytes => self.committed.output_bytes,
            BudgetResource::HostCalls => self.committed.host_calls,
            BudgetResource::HostRequestBytes => self.committed.host_request_bytes,
            BudgetResource::HostResponseBytes => self.committed.host_response_bytes,
            BudgetResource::HostCallBytes => self.committed.host_call_bytes,
            // live / per-item は cumulative 経路では扱わない。
            BudgetResource::HeapBytes
            | BudgetResource::SingleStringBytes
            | BudgetResource::SingleSourceBytes
            | BudgetResource::CollectionElements => 0,
        }
    }

    /// この resource の現在の reserved 値。
    fn reserved_of(&self, resource: BudgetResource) -> u64 {
        match resource {
            BudgetResource::Fuel => self.reserved.fuel,
            BudgetResource::StringAllocations => self.reserved.string_allocations,
            BudgetResource::StringBytes => self.reserved.string_bytes,
            BudgetResource::SourceCount => self.reserved.source_count as u64,
            BudgetResource::SourceBytes => self.reserved.source_bytes,
            BudgetResource::ImportCount => self.reserved.import_count as u64,
            BudgetResource::ImportBytes => self.reserved.import_bytes,
            BudgetResource::InputCalls => self.reserved.input_calls,
            BudgetResource::InputBytes => self.reserved.input_bytes,
            BudgetResource::OutputCalls => self.reserved.output_calls,
            BudgetResource::OutputBytes => self.reserved.output_bytes,
            BudgetResource::HostCalls => self.reserved.host_calls,
            BudgetResource::HostRequestBytes => self.reserved.host_request_bytes,
            BudgetResource::HostResponseBytes => self.reserved.host_response_bytes,
            BudgetResource::HostCallBytes => self.reserved.host_call_bytes,
            BudgetResource::HeapBytes
            | BudgetResource::SingleStringBytes
            | BudgetResource::SingleSourceBytes
            | BudgetResource::CollectionElements => 0,
        }
    }

    /// この resource の上限。
    fn limit_of(&self, resource: BudgetResource) -> u64 {
        match resource {
            BudgetResource::Fuel => self.config.total_fuel,
            BudgetResource::HeapBytes => self.config.max_live_heap_bytes,
            BudgetResource::SingleStringBytes => self.config.max_single_string_bytes,
            BudgetResource::StringAllocations => self.config.max_string_allocations,
            BudgetResource::StringBytes => self.config.max_string_bytes,
            BudgetResource::SingleSourceBytes => self.config.max_single_source_bytes,
            BudgetResource::SourceCount => self.config.max_source_count as u64,
            BudgetResource::SourceBytes => self.config.max_source_bytes,
            BudgetResource::ImportCount => self.config.max_import_count as u64,
            BudgetResource::ImportBytes => self.config.max_import_bytes,
            BudgetResource::CollectionElements => self.config.max_collection_elements,
            BudgetResource::InputCalls => self.config.max_input_calls,
            BudgetResource::InputBytes => self.config.max_input_bytes,
            BudgetResource::OutputCalls => self.config.max_output_calls,
            BudgetResource::OutputBytes => self.config.max_output_bytes,
            BudgetResource::HostCalls => self.config.max_host_calls,
            BudgetResource::HostRequestBytes => self.config.max_host_request_bytes,
            BudgetResource::HostResponseBytes => self.config.max_host_response_bytes,
            BudgetResource::HostCallBytes => self.config.max_host_call_bytes,
        }
    }

    /// reserved を加算する（reserve 成功時）。
    fn add_reserved(&mut self, resource: BudgetResource, amount: u64) {
        match resource {
            BudgetResource::Fuel => self.reserved.fuel += amount,
            BudgetResource::StringAllocations => self.reserved.string_allocations += amount,
            BudgetResource::StringBytes => self.reserved.string_bytes += amount,
            BudgetResource::SourceCount => self.reserved.source_count += amount as u32,
            BudgetResource::SourceBytes => self.reserved.source_bytes += amount,
            BudgetResource::ImportCount => self.reserved.import_count += amount as u32,
            BudgetResource::ImportBytes => self.reserved.import_bytes += amount,
            BudgetResource::InputCalls => self.reserved.input_calls += amount,
            BudgetResource::InputBytes => self.reserved.input_bytes += amount,
            BudgetResource::OutputCalls => self.reserved.output_calls += amount,
            BudgetResource::OutputBytes => self.reserved.output_bytes += amount,
            BudgetResource::HostCalls => self.reserved.host_calls += amount,
            BudgetResource::HostRequestBytes => self.reserved.host_request_bytes += amount,
            BudgetResource::HostResponseBytes => self.reserved.host_response_bytes += amount,
            BudgetResource::HostCallBytes => self.reserved.host_call_bytes += amount,
            BudgetResource::HeapBytes
            | BudgetResource::SingleStringBytes
            | BudgetResource::SingleSourceBytes
            | BudgetResource::CollectionElements => {}
        }
    }

    /// reserved から commit へ移す（commit 時）。`actual <= reserved` は呼び出し側が保証する。
    fn move_to_committed(&mut self, resource: BudgetResource, reserved_amount: u64, actual: u64) {
        // reserved を戻し、committed へ actual を積む。
        match resource {
            BudgetResource::Fuel => {
                self.reserved.fuel -= reserved_amount;
                self.committed.fuel += actual;
            }
            BudgetResource::StringAllocations => {
                self.reserved.string_allocations -= reserved_amount;
                self.committed.string_allocations += actual;
            }
            BudgetResource::StringBytes => {
                self.reserved.string_bytes -= reserved_amount;
                self.committed.string_bytes += actual;
            }
            BudgetResource::SourceCount => {
                self.reserved.source_count -= reserved_amount as u32;
                self.committed.source_count += actual as u32;
            }
            BudgetResource::SourceBytes => {
                self.reserved.source_bytes -= reserved_amount;
                self.committed.source_bytes += actual;
            }
            BudgetResource::ImportCount => {
                self.reserved.import_count -= reserved_amount as u32;
                self.committed.import_count += actual as u32;
            }
            BudgetResource::ImportBytes => {
                self.reserved.import_bytes -= reserved_amount;
                self.committed.import_bytes += actual;
            }
            BudgetResource::InputCalls => {
                self.reserved.input_calls -= reserved_amount;
                self.committed.input_calls += actual;
            }
            BudgetResource::InputBytes => {
                self.reserved.input_bytes -= reserved_amount;
                self.committed.input_bytes += actual;
            }
            BudgetResource::OutputCalls => {
                self.reserved.output_calls -= reserved_amount;
                self.committed.output_calls += actual;
            }
            BudgetResource::OutputBytes => {
                self.reserved.output_bytes -= reserved_amount;
                self.committed.output_bytes += actual;
            }
            BudgetResource::HostCalls => {
                self.reserved.host_calls -= reserved_amount;
                self.committed.host_calls += actual;
            }
            BudgetResource::HostRequestBytes => {
                self.reserved.host_request_bytes -= reserved_amount;
                self.committed.host_request_bytes += actual;
            }
            BudgetResource::HostResponseBytes => {
                self.reserved.host_response_bytes -= reserved_amount;
                self.committed.host_response_bytes += actual;
            }
            BudgetResource::HostCallBytes => {
                self.reserved.host_call_bytes -= reserved_amount;
                self.committed.host_call_bytes += actual;
            }
            BudgetResource::HeapBytes
            | BudgetResource::SingleStringBytes
            | BudgetResource::SingleSourceBytes
            | BudgetResource::CollectionElements => {}
        }
    }

    /// cancel を確認する（§7-1: reserve は cancel/deadline を先に確認する）。
    fn check_cancel(&self) -> Result<(), ControlStop> {
        if self.cancellation.is_cancelled() {
            return Err(ControlStop::Cancelled);
        }
        Ok(())
    }

    /// deadline を確認する（§7-1）。Slice 1 では clock を保持しないため、呼び出し側が
    /// 確認済みであることを前提に no-op とする。deadline checkpoint の配線は Slice 4。
    fn check_deadline(&self) -> Result<(), ControlStop> {
        Ok(())
    }

    /// cumulative resource を 1 回 reserve する（§7-1, §7-2）。
    ///
    /// `used + reserved + requested` を checked add で計算し、上限を超えるか overflow
    /// する場合は該当 resource の [`BudgetExceeded`] を返す。単一要求のため、複数超過の
    /// 優先順位は複合要求 [`Self::reserve_all`] で扱う。
    fn reserve_one(
        &self,
        req: CumulativeRequest,
        phase: ExecutionPhase,
    ) -> Result<(), BudgetExceeded> {
        let limit = self.limit_of(req.resource);
        let used = self.committed_of(req.resource);
        let reserved = self.reserved_of(req.resource);
        // used + reserved + requested を overflow なしで計算する（§7.1）。
        let projected = used
            .checked_add(reserved)
            .and_then(|v| v.checked_add(req.amount));
        match projected {
            Some(total) if total <= limit => Ok(()),
            Some(_) => Err(BudgetExceeded {
                resource: req.resource,
                limit,
                used,
                reserved,
                requested: req.amount,
                unit: req.resource.unit(),
                phase,
            }),
            None => Err(BudgetExceeded {
                resource: req.resource,
                limit,
                used,
                reserved,
                requested: u64::MAX,
                unit: req.resource.unit(),
                phase,
            }),
        }
    }

    /// 複数 cumulative resource を 1 個の atomic reservation として reserve する（§7）。
    ///
    /// どれか 1 つでも超える場合は何も reserve せず、§7.2 の固定優先順位で先頭の
    /// [`BudgetExceeded`] を返す（部分成功しない）。cancel / deadline を先に確認する。
    pub fn reserve_all(
        &mut self,
        requests: &[CumulativeRequest],
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        self.check_cancel()?;
        self.check_deadline()?;

        // 全要求を検査し、超過を集める（部分 reserve はしない）。
        let mut worst: Option<BudgetExceeded> = None;
        for req in requests {
            if let Err(exceeded) = self.reserve_one(*req, phase) {
                let replace = match &worst {
                    None => true,
                    Some(cur) => exceeded.resource.priority() < cur.resource.priority(),
                };
                if replace {
                    worst = Some(exceeded);
                }
            }
        }
        if let Some(exceeded) = worst {
            return Err(ControlStop::BudgetExceeded(exceeded));
        }

        // 全要求が通ったので reserve を確定する。
        for req in requests {
            self.add_reserved(req.resource, req.amount);
        }
        Ok(())
    }

    /// reserve 済み量を commit する（§7-3）。`actual <= reserved_amount` を要求する。
    pub fn commit(&mut self, resource: BudgetResource, reserved_amount: u64, actual: u64) {
        debug_assert!(actual <= reserved_amount, "commit actual は reserved 以下");
        self.move_to_committed(resource, reserved_amount, actual.min(reserved_amount));
    }

    /// reserve 済み量を全額 refund する（§7-4）。operation 未開始時に使う。
    pub fn refund(&mut self, resource: BudgetResource, reserved_amount: u64) {
        self.move_to_committed(resource, reserved_amount, 0);
    }

    // -------------------------------------------------------------------------
    // Slice 1 の配線ヘルパ（既存 step / collection 検査の一本化）
    // -------------------------------------------------------------------------

    /// fuel を `amount` だけ確定課金する（reserve→commit を 1 回で行う）。
    ///
    /// 既存の step 課金を置き換える入口。上限超過・overflow・cancel はいずれも
    /// [`ControlStop`] として返る。amount が確定しているため commit は reserved と
    /// 同額になる。
    pub fn charge_fuel(&mut self, amount: u64, phase: ExecutionPhase) -> Result<(), ControlStop> {
        let req = CumulativeRequest {
            resource: BudgetResource::Fuel,
            amount,
        };
        self.reserve_all(&[req], phase)?;
        self.commit(BudgetResource::Fuel, amount, amount);
        Ok(())
    }

    /// per-item の collection 要素数を検査する（§3 / §5.3）。
    ///
    /// candidate cardinality を上限と比較し、成功時は累積せず peak を更新する。
    /// 超過時は `used = reserved = 0`、`requested = candidate` とする（§3）。
    pub fn check_collection_elements(
        &mut self,
        candidate: u64,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        self.check_cancel()?;
        let limit = self.config.max_collection_elements;
        if candidate > limit {
            return Err(ControlStop::BudgetExceeded(BudgetExceeded {
                resource: BudgetResource::CollectionElements,
                limit,
                used: 0,
                reserved: 0,
                requested: candidate,
                unit: BudgetUnit::Elements,
                phase,
            }));
        }
        if candidate > self.peaks.collection_elements {
            self.peaks.collection_elements = candidate;
        }
        Ok(())
    }

    /// 新規 String body 1 個の生成を課金する（§5.3 / §7、REV-015 Slice 2）。
    ///
    /// `byte_len` は UTF-8 payload の byte 長（String header は含めない）。1 個の新規
    /// String body を作る前に、次を 1 個の atomic reservation として扱う。
    ///
    /// 1. per-item `SingleStringBytes`（§5.1 per-item）: `byte_len > max_single_string_bytes`
    ///    なら `used = reserved = 0`、`requested = byte_len` の [`BudgetExceeded`] を返す。
    /// 2. cumulative `StringAllocations`（+1）と `StringBytes`（+`byte_len`）を
    ///    [`Self::reserve_all`] で予約し、成功時に確定 commit する（§5.3）。
    ///
    /// per-item と cumulative の複数超過は §7.2 の固定優先順位（`SingleStringBytes` <
    /// `StringAllocations` < `StringBytes`）で先頭 1 件を primary にする。cancel /
    /// deadline は [`Self::reserve_all`] が charge 前に確認する。成功時は
    /// `peaks.single_string_bytes` を `max` 更新する。substring が既存 body を共有する
    /// 実装なら新規 allocation として数えないが、現行の `Value::Str(String)` は常に
    /// 新規 copy なので生成のたびに課金する（§5.3）。
    pub fn charge_string(
        &mut self,
        byte_len: u64,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        // per-item を先に検査する（§7.2 で SingleStringBytes が cumulative より優先）。
        let single_limit = self.config.max_single_string_bytes;
        if byte_len > single_limit {
            self.check_cancel()?;
            return Err(ControlStop::BudgetExceeded(BudgetExceeded {
                resource: BudgetResource::SingleStringBytes,
                limit: single_limit,
                used: 0,
                reserved: 0,
                requested: byte_len,
                unit: BudgetUnit::Bytes,
                phase,
            }));
        }
        // cumulative を 1 個の atomic reservation として予約する。
        let requests = [
            CumulativeRequest {
                resource: BudgetResource::StringAllocations,
                amount: 1,
            },
            CumulativeRequest {
                resource: BudgetResource::StringBytes,
                amount: byte_len,
            },
        ];
        self.reserve_all(&requests, phase)?;
        // amount 確定のため reserved と同額を commit する。
        self.commit(BudgetResource::StringAllocations, 1, 1);
        self.commit(BudgetResource::StringBytes, byte_len, byte_len);
        // per-item peak を更新する（§5.1 per-item）。
        if byte_len > self.peaks.single_string_bytes {
            self.peaks.single_string_bytes = byte_len;
        }
        Ok(())
    }

    /// 単一文字列 payload の上限（config 値）。共有 builtin handler へ渡す。
    pub fn max_single_string_bytes(&self) -> u64 {
        self.config.max_single_string_bytes
    }

    /// 読み込んだ source 1 本を課金する（§5.3 / §7、REV-015 Slice 2）。
    ///
    /// `byte_len` は BOM・改行を正規化しない生 UTF-8 byte 長で、hash 対象と同じ byte
    /// 列を数える。root（実行対象スクリプト）も import module も 1 本の source として
    /// `SourceCount += 1` / `SourceBytes += byte_len` を課金し、per-item
    /// `SingleSourceBytes` を検査する。import module の byte は別途
    /// [`Self::charge_import`] で `ImportCount` / `ImportBytes` にも課金する（§5.3）。
    ///
    /// 1 本の source を受理する前に次を 1 個の atomic reservation として扱う。
    ///
    /// 1. per-item `SingleSourceBytes`（§5.1 per-item）: `byte_len > max_single_source_bytes`
    ///    なら `used = reserved = 0`、`requested = byte_len` の [`BudgetExceeded`] を返す。
    /// 2. cumulative `SourceCount`（+1）と `SourceBytes`（+`byte_len`）を
    ///    [`Self::reserve_all`] で予約し、成功時に確定 commit する（§5.3）。
    ///
    /// per-item と cumulative の複数超過は §7.2 の固定優先順位（`SingleSourceBytes` <
    /// `SourceCount` < `SourceBytes`）で先頭 1 件を primary にする。cancel / deadline は
    /// [`Self::reserve_all`] が charge 前に確認する。成功時は `peaks.single_source_bytes`
    /// を `max` 更新する。同一 normalized module ID の cache hit は呼び出し側が除外し、
    /// ここでは受理した 1 本だけを数える（§5.3）。
    pub fn charge_source(
        &mut self,
        byte_len: u64,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        // per-item を先に検査する（§7.2 で SingleSourceBytes が cumulative より優先）。
        let single_limit = self.config.max_single_source_bytes;
        if byte_len > single_limit {
            self.check_cancel()?;
            return Err(ControlStop::BudgetExceeded(BudgetExceeded {
                resource: BudgetResource::SingleSourceBytes,
                limit: single_limit,
                used: 0,
                reserved: 0,
                requested: byte_len,
                unit: BudgetUnit::Bytes,
                phase,
            }));
        }
        // cumulative を 1 個の atomic reservation として予約する。
        let requests = [
            CumulativeRequest {
                resource: BudgetResource::SourceCount,
                amount: 1,
            },
            CumulativeRequest {
                resource: BudgetResource::SourceBytes,
                amount: byte_len,
            },
        ];
        self.reserve_all(&requests, phase)?;
        self.commit(BudgetResource::SourceCount, 1, 1);
        self.commit(BudgetResource::SourceBytes, byte_len, byte_len);
        if byte_len > self.peaks.single_source_bytes {
            self.peaks.single_source_bytes = byte_len;
        }
        Ok(())
    }

    /// 初めて解決した import module 1 本を課金する（§5.3 / §7、REV-015 Slice 2）。
    ///
    /// `import_count` は root を含まず、初めて解決した normalized module ID ごとに
    /// 1 増やす。`import_bytes` は import source の生 byte 長である。したがって import
    /// source は [`Self::charge_source`]（`source_bytes`）と本メソッド（`import_bytes`）
    /// の両方へ意図的に課金する（§5.3）。`ImportCount`（+1）と `ImportBytes`
    /// （+`byte_len`）を 1 個の atomic reservation として予約し、成功時に確定 commit
    /// する。複数超過は §7.2 の固定優先順位（`ImportCount` < `ImportBytes`）で先頭 1 件
    /// を primary にする。cancel / deadline は charge 前に確認する。
    pub fn charge_import(
        &mut self,
        byte_len: u64,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        let requests = [
            CumulativeRequest {
                resource: BudgetResource::ImportCount,
                amount: 1,
            },
            CumulativeRequest {
                resource: BudgetResource::ImportBytes,
                amount: byte_len,
            },
        ];
        self.reserve_all(&requests, phase)?;
        self.commit(BudgetResource::ImportCount, 1, 1);
        self.commit(BudgetResource::ImportBytes, byte_len, byte_len);
        Ok(())
    }

    /// dispatch 結果の `Value` から到達する新規 String body をすべて課金する
    /// （REV-015 Slice 2、string accounting）。
    ///
    /// 現行の `Value::Str(String)` は共有 backing を持たず、builtin が返す String は
    /// 必ず新規 copy なので、結果内の各 String body を [`Self::charge_string`] で
    /// 課金する。List / Dict の backing byte は heap accounting（別サブスライス）の
    /// 対象で、ここでは走査して内部の String body だけを課金する。Dict の key も
    /// String body なので課金する。再帰は使わず worklist で走査し、深い構造でも
    /// host stack を消費しない。
    ///
    /// per-item / cumulative のいずれかを超えた時点で `ControlStop` を返し、以降の
    /// 課金は行わない（部分的に committed が進むが、超過は catch 不能 terminal なので
    /// engine 側が実行を止める）。
    pub fn charge_result_strings(
        &mut self,
        value: &crate::value::Value,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        use crate::value::Value;
        let mut worklist: Vec<&Value> = vec![value];
        while let Some(v) = worklist.pop() {
            match v {
                Value::Str(s) => {
                    self.charge_string(s.len() as u64, phase)?;
                }
                Value::List(items) => {
                    for item in items.iter() {
                        worklist.push(item);
                    }
                }
                Value::Dict(map) => {
                    for (key, item) in map.iter() {
                        // Dict key も新規 String body として課金する（§5.1）。
                        self.charge_string(key.len() as u64, phase)?;
                        worklist.push(item);
                    }
                }
                // Int / Float / Bool / Null / Fn / VmFn / Error は String body を
                // 新規生成しない（Error の message は既存 body の写像で別サブスライス）。
                _ => {}
            }
        }
        Ok(())
    }

    /// fuel の committed / reserved を 0 に戻す（REPL 入力ごとの予算リセット）。
    ///
    /// 各 REPL 入力で fuel 予算を全額使えるようにするための Slice 1 互換操作。
    /// per-item peak と他 counter は据え置く。
    pub fn reset_fuel(&mut self) {
        self.committed.fuel = 0;
        self.reserved.fuel = 0;
    }

    /// 現在の committed fuel（REPL rollback の snapshot 用）。
    pub fn committed_fuel(&self) -> u64 {
        self.committed.fuel
    }

    /// committed fuel を指定値へ復元する（REPL 未捕捉エラー時の rollback 用）。
    /// reserved は 0 に戻す。
    pub fn restore_fuel(&mut self, committed_fuel: u64) {
        self.committed.fuel = committed_fuel;
        self.reserved.fuel = 0;
    }

    /// total fuel 上限を再設定する（`set_max_steps` 互換）。
    pub fn set_total_fuel(&mut self, total_fuel: u64) {
        self.config.total_fuel = total_fuel;
    }

    /// collection 要素数上限（config 値）。共有 builtin handler へ渡す。
    pub fn max_collection_elements(&self) -> u64 {
        self.config.max_collection_elements
    }

    /// candidate が collection 上限を超えるなら peak を更新せず true を返す（内部判定）。
    /// 上限内なら peak を `max` 更新して false を返す。
    /// 共有 builtin handler が [`check_collection_elements`](Self::check_collection_elements)
    /// を経由できない場合の per-item peak 反映に使う。
    pub fn note_collection_elements(&mut self, candidate: u64) {
        if candidate <= self.config.max_collection_elements
            && candidate > self.peaks.collection_elements
        {
            self.peaks.collection_elements = candidate;
        }
    }

    /// fuel の残量（total - committed - reserved）。診断・テスト補助。
    pub fn remaining_fuel(&self) -> u64 {
        self.config
            .total_fuel
            .saturating_sub(self.committed.fuel)
            .saturating_sub(self.reserved.fuel)
    }
}

/// budget の [`ControlStop`] を既存の [`TsumugiError`] へ写像する（Slice 1/2 互換）。
///
/// tree evaluator と VM の両方から呼び、resource → error kind/message の対応を
/// 1 箇所へ集約して parity を保証する。trace は各 engine が呼び出し側で付ける。
///
/// Slice 1/2 で発生し得るのは fuel（= step）・collection・string 超過。cancel /
/// deadline は ledger の charge 経路にまだ配線しておらず（Slice 4）、到達した場合も
/// 安全側で step 上限として扱う。`committed_fuel` は cancel/deadline fallback の
/// 上限表示に使う。
pub fn control_stop_to_error(
    stop: ControlStop,
    committed_fuel: u64,
    line: usize,
) -> crate::error::TsumugiError {
    use crate::error::TsumugiError;
    match stop {
        ControlStop::BudgetExceeded(e) => match e.resource {
            BudgetResource::CollectionElements => {
                TsumugiError::collection_limit(line, e.requested as usize, e.limit as usize)
            }
            BudgetResource::SingleStringBytes => {
                TsumugiError::single_string_limit(line, e.requested, e.limit)
            }
            BudgetResource::StringAllocations => {
                TsumugiError::string_allocation_limit(line, e.limit)
            }
            BudgetResource::StringBytes => TsumugiError::string_bytes_limit(line, e.limit),
            BudgetResource::SingleSourceBytes => {
                TsumugiError::single_source_limit(line, e.requested, e.limit)
            }
            BudgetResource::SourceCount => TsumugiError::source_count_limit(line, e.limit),
            BudgetResource::SourceBytes => TsumugiError::source_bytes_limit(line, e.limit),
            BudgetResource::ImportCount => TsumugiError::import_count_limit(line, e.limit),
            BudgetResource::ImportBytes => TsumugiError::import_bytes_limit(line, e.limit),
            _ => TsumugiError::step_limit(line, e.limit),
        },
        ControlStop::Cancelled | ControlStop::DeadlineExceeded { .. } => {
            TsumugiError::step_limit(line, committed_fuel)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用に、全上限が 0 の config を fake clock 由来の遠い未来 deadline で作る。
    fn zero_config() -> BudgetConfig {
        BudgetConfig::for_legacy(0, 0)
    }

    fn ledger(config: BudgetConfig) -> BudgetLedger {
        BudgetLedger::with_config(config)
    }

    fn req(resource: BudgetResource, amount: u64) -> CumulativeRequest {
        CumulativeRequest { resource, amount }
    }

    // -------------------------------------------------------------------------
    // config / clock（§3, §3.1）
    // -------------------------------------------------------------------------

    #[test]
    fn standard_config_sets_documented_defaults() {
        let clock = FakeClock::new();
        let config = BudgetConfig::standard(&clock).unwrap();
        assert_eq!(config.heap_accounting_revision, 1);
        assert_eq!(config.total_fuel, 1_000_000);
        assert_eq!(config.max_live_heap_bytes, 64 * 1_048_576);
        assert_eq!(config.max_single_string_bytes, 8 * 1_048_576);
        assert_eq!(config.max_source_count, 1_025);
        assert_eq!(config.max_import_count, 1_024);
        assert_eq!(config.max_collection_elements, 1_000_000);
        assert_eq!(config.max_host_call_bytes, 24 * 1_048_576);
        // deadline は now + 30 s。
        assert_eq!(config.deadline.as_nanos(), 30 * 1_000_000_000);
    }

    #[test]
    fn validate_rejects_unsupported_accounting_revision() {
        let clock = FakeClock::new();
        let mut config = BudgetConfig::standard(&clock).unwrap();
        config.heap_accounting_revision = 2;
        assert_eq!(
            config.validate(&clock),
            Err(ConfigError::UnsupportedAccountingRevision(2))
        );
    }

    #[test]
    fn validate_rejects_foreign_clock() {
        let clock_a = FakeClock::new();
        let clock_b = FakeClock::new();
        let config = BudgetConfig::standard(&clock_a).unwrap();
        // 別 domain の clock で検証すると ForeignClock。
        assert_eq!(config.validate(&clock_b), Err(ConfigError::ForeignClock));
    }

    #[test]
    fn validate_rejects_deadline_not_in_future() {
        let clock = FakeClock::new();
        let config = BudgetConfig::standard(&clock).unwrap();
        // deadline（now+30s）を過ぎた時刻まで進めると拒否。
        clock.set(30 * 1_000_000_000);
        assert_eq!(
            config.validate(&clock),
            Err(ConfigError::DeadlineNotInFuture)
        );
    }

    #[test]
    fn standard_config_overflow_is_rejected() {
        let clock = FakeClock::new();
        clock.set(u64::MAX - 1);
        assert_eq!(BudgetConfig::standard(&clock), Err(ConfigError::Overflow));
    }

    // -------------------------------------------------------------------------
    // limit ちょうど成功 / +1 超過（§15.1）
    // -------------------------------------------------------------------------

    #[test]
    fn fuel_at_limit_succeeds_and_plus_one_exceeds() {
        let mut l = ledger(BudgetConfig::for_legacy(3, 0));
        // 3 まではちょうど成功。
        assert!(l.charge_fuel(1, ExecutionPhase::Run).is_ok());
        assert!(l.charge_fuel(1, ExecutionPhase::Run).is_ok());
        assert!(l.charge_fuel(1, ExecutionPhase::Run).is_ok());
        // 4 個目は Fuel 超過。
        let err = l.charge_fuel(1, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::Fuel);
                assert_eq!(e.limit, 3);
                assert_eq!(e.used, 3);
                assert_eq!(e.requested, 1);
                assert_eq!(e.unit, BudgetUnit::Fuel);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    #[test]
    fn zero_fuel_limit_forbids_first_charge() {
        let mut l = ledger(zero_config());
        let err = l.charge_fuel(1, ExecutionPhase::Run).unwrap_err();
        assert!(matches!(
            err,
            ControlStop::BudgetExceeded(BudgetExceeded {
                resource: BudgetResource::Fuel,
                ..
            })
        ));
    }

    #[test]
    fn collection_at_limit_succeeds_and_plus_one_exceeds() {
        let mut l = ledger(BudgetConfig::for_legacy(1_000_000, 2));
        // 2 要素はちょうど成功し、peak が 2 になる。
        assert!(l.check_collection_elements(2, ExecutionPhase::Run).is_ok());
        assert_eq!(l.usage().peaks.collection_elements, 2);
        // 3 要素は超過。used/reserved は 0、requested は candidate。
        let err = l
            .check_collection_elements(3, ExecutionPhase::Run)
            .unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::CollectionElements);
                assert_eq!(e.limit, 2);
                assert_eq!(e.used, 0);
                assert_eq!(e.reserved, 0);
                assert_eq!(e.requested, 3);
                assert_eq!(e.unit, BudgetUnit::Elements);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
        // 超過検査は peak を更新しない。
        assert_eq!(l.usage().peaks.collection_elements, 2);
    }

    // -------------------------------------------------------------------------
    // string accounting（§5.3, §15.1）: single/allocations/bytes
    // -------------------------------------------------------------------------

    #[test]
    fn string_at_single_limit_succeeds_and_plus_one_exceeds() {
        // single 上限 4 byte、cumulative は十分。
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_single_string_bytes = 4;
        config.max_string_allocations = 1_000;
        config.max_string_bytes = 1_000;
        let mut l = ledger(config);
        // ちょうど 4 byte は成功し、peak と cumulative が積まれる。
        assert!(l.charge_string(4, ExecutionPhase::Run).is_ok());
        assert_eq!(l.usage().peaks.single_string_bytes, 4);
        assert_eq!(l.usage().committed.string_allocations, 1);
        assert_eq!(l.usage().committed.string_bytes, 4);
        // 5 byte は SingleStringBytes 超過。used/reserved は 0、requested は byte 長。
        let err = l.charge_string(5, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::SingleStringBytes);
                assert_eq!(e.limit, 4);
                assert_eq!(e.used, 0);
                assert_eq!(e.reserved, 0);
                assert_eq!(e.requested, 5);
                assert_eq!(e.unit, BudgetUnit::Bytes);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
        // 超過検査は cumulative も peak も更新しない。
        assert_eq!(l.usage().committed.string_allocations, 1);
        assert_eq!(l.usage().committed.string_bytes, 4);
        assert_eq!(l.usage().peaks.single_string_bytes, 4);
    }

    #[test]
    fn string_bytes_accumulate_across_allocations() {
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_single_string_bytes = 100;
        config.max_string_allocations = 1_000;
        config.max_string_bytes = 10;
        let mut l = ledger(config);
        // 4 + 4 = 8 byte までは成功。allocations は 2。
        assert!(l.charge_string(4, ExecutionPhase::Run).is_ok());
        assert!(l.charge_string(4, ExecutionPhase::Run).is_ok());
        assert_eq!(l.usage().committed.string_allocations, 2);
        assert_eq!(l.usage().committed.string_bytes, 8);
        // さらに 4 byte で累積 12 > 10。StringBytes 超過。
        let err = l.charge_string(4, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::StringBytes);
                assert_eq!(e.limit, 10);
                assert_eq!(e.used, 8);
                assert_eq!(e.requested, 4);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
        // 失敗は何も commit しない。
        assert_eq!(l.usage().committed.string_allocations, 2);
        assert_eq!(l.usage().committed.string_bytes, 8);
    }

    #[test]
    fn string_allocations_count_limit_is_enforced() {
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_single_string_bytes = 100;
        config.max_string_allocations = 2;
        config.max_string_bytes = 1_000;
        let mut l = ledger(config);
        // 空文字列（0 byte）でも allocation は 1 個として数える。
        assert!(l.charge_string(0, ExecutionPhase::Run).is_ok());
        assert!(l.charge_string(1, ExecutionPhase::Run).is_ok());
        assert_eq!(l.usage().committed.string_allocations, 2);
        // 3 個目は StringAllocations 超過。
        let err = l.charge_string(1, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::StringAllocations);
                assert_eq!(e.limit, 2);
                assert_eq!(e.used, 2);
                assert_eq!(e.requested, 1);
                assert_eq!(e.unit, BudgetUnit::Count);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    #[test]
    fn zero_single_string_limit_forbids_nonempty_but_allows_empty() {
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_single_string_bytes = 0;
        config.max_string_allocations = 1_000;
        config.max_string_bytes = 1_000;
        let mut l = ledger(config);
        // 0 byte は single 上限 0 ちょうどで成功。
        assert!(l.charge_string(0, ExecutionPhase::Run).is_ok());
        // 1 byte は SingleStringBytes 超過。
        let err = l.charge_string(1, ExecutionPhase::Run).unwrap_err();
        assert!(matches!(
            err,
            ControlStop::BudgetExceeded(BudgetExceeded {
                resource: BudgetResource::SingleStringBytes,
                ..
            })
        ));
    }

    #[test]
    fn string_single_and_cumulative_both_exceed_prefers_single() {
        // single も cumulative も 0 上限 → §7.2 で SingleStringBytes が優先。
        let config = BudgetConfig::for_legacy(1_000_000, 0);
        let mut config = config;
        config.max_single_string_bytes = 0;
        config.max_string_allocations = 0;
        config.max_string_bytes = 0;
        let mut l = ledger(config);
        let err = l.charge_string(1, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::SingleStringBytes)
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    #[test]
    fn string_cancel_is_checked_before_charge() {
        let token = CancellationToken::new();
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_single_string_bytes = 100;
        config.max_string_allocations = 100;
        config.max_string_bytes = 100;
        let mut l = BudgetLedger::new(config, token.clone());
        token.cancel();
        assert_eq!(
            l.charge_string(1, ExecutionPhase::Run),
            Err(ControlStop::Cancelled)
        );
        // per-item 超過より cancel が先。
        assert_eq!(
            l.charge_string(u64::MAX, ExecutionPhase::Run),
            Err(ControlStop::Cancelled)
        );
    }

    // -------------------------------------------------------------------------
    // source accounting（§5.3, §15.1）
    // -------------------------------------------------------------------------

    #[test]
    fn source_at_single_limit_succeeds_and_plus_one_exceeds() {
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_single_source_bytes = 4;
        config.max_source_count = 1_000;
        config.max_source_bytes = 1_000;
        let mut l = ledger(config);
        // ちょうど 4 byte は成功し、peak と cumulative が積まれる。
        assert!(l.charge_source(4, ExecutionPhase::Run).is_ok());
        assert_eq!(l.usage().peaks.single_source_bytes, 4);
        assert_eq!(l.usage().committed.source_count, 1);
        assert_eq!(l.usage().committed.source_bytes, 4);
        // 5 byte は SingleSourceBytes 超過。used/reserved は 0、requested は byte 長。
        let err = l.charge_source(5, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::SingleSourceBytes);
                assert_eq!(e.requested, 5);
                assert_eq!(e.used, 0);
                assert_eq!(e.reserved, 0);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    #[test]
    fn source_bytes_accumulate_across_sources() {
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_single_source_bytes = 100;
        config.max_source_count = 1_000;
        config.max_source_bytes = 10;
        let mut l = ledger(config);
        assert!(l.charge_source(4, ExecutionPhase::Run).is_ok());
        assert!(l.charge_source(4, ExecutionPhase::Run).is_ok());
        assert_eq!(l.usage().committed.source_count, 2);
        assert_eq!(l.usage().committed.source_bytes, 8);
        // さらに 4 byte で累積 12 > 10。SourceBytes 超過。
        let err = l.charge_source(4, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::SourceBytes);
                assert_eq!(e.limit, 10);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    #[test]
    fn source_count_limit_is_enforced() {
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_single_source_bytes = 100;
        config.max_source_count = 2;
        config.max_source_bytes = 1_000;
        let mut l = ledger(config);
        // 空 source（0 byte）でも 1 本として数える。
        assert!(l.charge_source(0, ExecutionPhase::Run).is_ok());
        assert!(l.charge_source(1, ExecutionPhase::Run).is_ok());
        assert_eq!(l.usage().committed.source_count, 2);
        // 3 本目は SourceCount 超過。
        let err = l.charge_source(1, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::SourceCount);
                assert_eq!(e.limit, 2);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    #[test]
    fn source_single_and_cumulative_both_exceed_prefers_single() {
        // single も cumulative も 0 上限 → §7.2 で SingleSourceBytes が優先。
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_single_source_bytes = 0;
        config.max_source_count = 0;
        config.max_source_bytes = 0;
        let mut l = ledger(config);
        let err = l.charge_source(1, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::SingleSourceBytes)
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    #[test]
    fn source_cancel_is_checked_before_charge() {
        let token = CancellationToken::new();
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_single_source_bytes = 100;
        config.max_source_count = 100;
        config.max_source_bytes = 100;
        let mut l = BudgetLedger::new(config, token.clone());
        token.cancel();
        assert_eq!(
            l.charge_source(1, ExecutionPhase::Run),
            Err(ControlStop::Cancelled)
        );
        // per-item 超過より cancel が先。
        assert_eq!(
            l.charge_source(u64::MAX, ExecutionPhase::Run),
            Err(ControlStop::Cancelled)
        );
    }

    // -------------------------------------------------------------------------
    // import accounting（§5.3, §15.1）
    // -------------------------------------------------------------------------

    #[test]
    fn import_count_and_bytes_accumulate() {
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_import_count = 2;
        config.max_import_bytes = 10;
        let mut l = ledger(config);
        assert!(l.charge_import(4, ExecutionPhase::Run).is_ok());
        assert!(l.charge_import(4, ExecutionPhase::Run).is_ok());
        assert_eq!(l.usage().committed.import_count, 2);
        assert_eq!(l.usage().committed.import_bytes, 8);
        // 3 本目は ImportCount 超過（count が bytes より優先）。
        let err = l.charge_import(1, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::ImportCount);
                assert_eq!(e.limit, 2);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    #[test]
    fn import_bytes_limit_is_enforced() {
        let mut config = BudgetConfig::for_legacy(1_000_000, 0);
        config.max_import_count = 1_000;
        config.max_import_bytes = 5;
        let mut l = ledger(config);
        let err = l.charge_import(6, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::ImportBytes);
                assert_eq!(e.limit, 5);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    // -------------------------------------------------------------------------
    // overflow は wrap せず超過扱い（§7.1, §15.1）
    // -------------------------------------------------------------------------

    #[test]
    fn near_u64_max_addition_does_not_wrap() {
        // used + reserved + requested が u64 を超えると requested = u64::MAX。
        let mut config = BudgetConfig::for_legacy(u64::MAX, 0);
        config.total_fuel = u64::MAX;
        let mut l = ledger(config);
        // 大きな fuel を先に commit しておく。
        assert!(l.charge_fuel(u64::MAX - 1, ExecutionPhase::Run).is_ok());
        // さらに 10 課金すると used(=u64::MAX-1)+10 が overflow。
        let err = l.charge_fuel(10, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::Fuel);
                assert_eq!(e.requested, u64::MAX);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    // -------------------------------------------------------------------------
    // 複合 reservation は部分成功せず固定優先順位を返す（§7.2, §15.1）
    // -------------------------------------------------------------------------

    #[test]
    fn composite_reservation_returns_highest_priority_resource() {
        // OutputBytes と Fuel の両方を超える要求。Fuel の方が優先順位が高い。
        let config = BudgetConfig::for_legacy(0, 0);
        let mut l = ledger(config);
        let requests = [
            req(BudgetResource::OutputBytes, 1),
            req(BudgetResource::Fuel, 1),
        ];
        let err = l.reserve_all(&requests, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => assert_eq!(e.resource, BudgetResource::Fuel),
            other => panic!("unexpected stop: {other:?}"),
        }
        // 部分成功していない: 何も reserve されていない。
        assert_eq!(l.usage().reserved.fuel, 0);
        assert_eq!(l.usage().reserved.output_bytes, 0);
    }

    #[test]
    fn composite_reservation_does_not_partially_reserve_on_success_mix() {
        // Fuel は通るが OutputCalls は 0 上限で超過 → 全体失敗、Fuel も reserve しない。
        let mut config = BudgetConfig::for_legacy(10, 0);
        config.max_output_calls = 0;
        let mut l = ledger(config);
        let requests = [
            req(BudgetResource::Fuel, 1),
            req(BudgetResource::OutputCalls, 1),
        ];
        assert!(l.reserve_all(&requests, ExecutionPhase::Run).is_err());
        assert_eq!(l.usage().reserved.fuel, 0);
    }

    #[test]
    fn reserve_all_reserves_every_resource_when_all_fit() {
        let mut config = BudgetConfig::for_legacy(10, 0);
        config.max_output_calls = 10;
        let mut l = ledger(config);
        let requests = [
            req(BudgetResource::Fuel, 2),
            req(BudgetResource::OutputCalls, 3),
        ];
        assert!(l.reserve_all(&requests, ExecutionPhase::Run).is_ok());
        assert_eq!(l.usage().reserved.fuel, 2);
        assert_eq!(l.usage().reserved.output_calls, 3);
    }

    // -------------------------------------------------------------------------
    // refund / commit（§7-3, §7-4, §15.1）
    // -------------------------------------------------------------------------

    #[test]
    fn refund_returns_reserved_amount_fully() {
        let mut config = BudgetConfig::for_legacy(10, 0);
        config.max_output_bytes = 10;
        let mut l = ledger(config);
        l.reserve_all(&[req(BudgetResource::OutputBytes, 4)], ExecutionPhase::Run)
            .unwrap();
        assert_eq!(l.usage().reserved.output_bytes, 4);
        // operation 未開始なら全額 refund。
        l.refund(BudgetResource::OutputBytes, 4);
        assert_eq!(l.usage().reserved.output_bytes, 0);
        assert_eq!(l.usage().committed.output_bytes, 0);
    }

    #[test]
    fn commit_moves_actual_and_refunds_the_difference() {
        let mut config = BudgetConfig::for_legacy(10, 0);
        config.max_output_bytes = 10;
        let mut l = ledger(config);
        l.reserve_all(&[req(BudgetResource::OutputBytes, 8)], ExecutionPhase::Run)
            .unwrap();
        // actual=5 を commit すると committed=5、reserved は 0（差分 refund）。
        l.commit(BudgetResource::OutputBytes, 8, 5);
        assert_eq!(l.usage().committed.output_bytes, 5);
        assert_eq!(l.usage().reserved.output_bytes, 0);
    }

    #[test]
    fn charge_fuel_commits_exact_amount() {
        let mut l = ledger(BudgetConfig::for_legacy(100, 0));
        l.charge_fuel(7, ExecutionPhase::Run).unwrap();
        assert_eq!(l.usage().committed.fuel, 7);
        assert_eq!(l.usage().reserved.fuel, 0);
        assert_eq!(l.remaining_fuel(), 93);
    }

    // -------------------------------------------------------------------------
    // REPL 互換の fuel reset / restore
    // -------------------------------------------------------------------------

    #[test]
    fn reset_fuel_clears_committed_and_reserved() {
        let mut l = ledger(BudgetConfig::for_legacy(100, 0));
        l.charge_fuel(10, ExecutionPhase::Run).unwrap();
        l.reset_fuel();
        assert_eq!(l.usage().committed.fuel, 0);
        assert_eq!(l.remaining_fuel(), 100);
    }

    #[test]
    fn restore_fuel_sets_committed_and_clears_reserved() {
        let mut l = ledger(BudgetConfig::for_legacy(100, 0));
        l.charge_fuel(10, ExecutionPhase::Run).unwrap();
        let snapshot = l.committed_fuel();
        l.reset_fuel();
        l.charge_fuel(5, ExecutionPhase::Run).unwrap();
        l.restore_fuel(snapshot);
        assert_eq!(l.usage().committed.fuel, 10);
        assert_eq!(l.usage().reserved.fuel, 0);
    }

    // -------------------------------------------------------------------------
    // cancel（§8）: charge 前に確認される
    // -------------------------------------------------------------------------

    #[test]
    fn cancelled_token_stops_charge_before_budget_check() {
        let token = CancellationToken::new();
        let mut l = BudgetLedger::new(BudgetConfig::for_legacy(100, 100), token.clone());
        token.cancel();
        assert_eq!(
            l.charge_fuel(1, ExecutionPhase::Run),
            Err(ControlStop::Cancelled)
        );
        assert_eq!(
            l.check_collection_elements(1, ExecutionPhase::Run),
            Err(ControlStop::Cancelled)
        );
    }

    // -------------------------------------------------------------------------
    // 固定優先順位表が §7.2 の 19 段と一致する
    // -------------------------------------------------------------------------

    #[test]
    fn resource_priority_matches_spec_order() {
        let order = [
            BudgetResource::Fuel,
            BudgetResource::HeapBytes,
            BudgetResource::SingleStringBytes,
            BudgetResource::StringAllocations,
            BudgetResource::StringBytes,
            BudgetResource::SingleSourceBytes,
            BudgetResource::SourceCount,
            BudgetResource::SourceBytes,
            BudgetResource::ImportCount,
            BudgetResource::ImportBytes,
            BudgetResource::CollectionElements,
            BudgetResource::InputCalls,
            BudgetResource::InputBytes,
            BudgetResource::OutputCalls,
            BudgetResource::OutputBytes,
            BudgetResource::HostCalls,
            BudgetResource::HostRequestBytes,
            BudgetResource::HostResponseBytes,
            BudgetResource::HostCallBytes,
        ];
        for (i, r) in order.iter().enumerate() {
            assert_eq!(r.priority() as usize, i + 1, "{r:?}");
        }
    }
}
