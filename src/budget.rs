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

use std::cell::RefCell;
use std::rc::{Rc, Weak};
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
        // heap accounting（REV-015 Slice 2）の legacy 上限入口。未設定は §3.1 の既定値。
        if let Some(v) = std::env::var("TSUMUGI_MAX_LIVE_HEAP_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            config.max_live_heap_bytes = v;
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
    /// 内部不変条件の破れ（§5.2）。現状は `AllocationId` 発行の overflow だけを
    /// 表す。budget 超過と同じく catch 不能 terminal であり、0 へ wrap しない。
    InternalFailure(InternalFailure),
}

/// 内部不変条件の破れ（§5.2 / §7.1）。catch 不能 terminal として扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalFailure {
    /// `AllocationId(u64)` を使い切った。0 へ wrap せず terminal にする。
    AllocationIdExhausted,
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
// heap accounting（§5.1 論理サイズ / §5.2 AllocationLedger）
// =============================================================================

/// heap-owned object 1 個へ発番する識別子（§5.2）。
///
/// execution ごとに 1 から単調増加する。同一 object を複数の `Rc` から参照しても、
/// この ID が同じである限り [`AllocationLedger`] は 1 回だけ課金する。発番の overflow
/// は 0 へ wrap せず [`InternalFailure::AllocationIdExhausted`] にする（§5.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AllocationId(pub u64);

/// §5.1 の論理サイズ表。allocator・platform・Rust compiler に依存しない固定値を
/// `HEAP_ACCOUNTING_REVISION = 1` として返す純関数群。
///
/// header 定数は byte 長・要素数から独立しており、object の種別だけで決まる。
/// payload（String body の byte 長、collection の要素数、Dict key の byte 長など）は
/// 各関数の引数で受け取る。すべて `u64` で計算し、overflow は呼び出し側の
/// `checked_*` 課金経路（[`AllocationLedger::charge`]）で `HeapBytes` 超過へ写像する。
pub mod heap_size {
    /// `Value` slot または captured cell 1 個（§5.1）。
    pub const VALUE_SLOT: u64 = 32;
    /// captured cell（`Rc<RefCell<Value>>`）1 個。§5.1 では Value slot と同じ 32。
    pub const CAPTURED_CELL: u64 = 32;

    /// UTF-8 `String` body（header 24 + byte 長）。header に payload を含めない。
    pub fn string_body(byte_len: u64) -> u64 {
        24u64.saturating_add(byte_len)
    }

    /// `List` body（header 24 + 32 × 要素数）。
    pub fn list_body(element_count: u64) -> u64 {
        24u64.saturating_add(element_count.saturating_mul(32))
    }

    /// `Dict` body（header 24 + 64 × entry 数 + 各 key の UTF-8 byte 長）。
    /// key payload の合計 `key_bytes_total` は呼び出し側が合算して渡す。
    pub fn dict_body(entry_count: u64, key_bytes_total: u64) -> u64 {
        24u64
            .saturating_add(entry_count.saturating_mul(64))
            .saturating_add(key_bytes_total)
    }

    /// tree function instance（64 + 16 × captured cell reference 数）。
    pub fn tree_function(captured_count: u64) -> u64 {
        64u64.saturating_add(captured_count.saturating_mul(16))
    }

    /// VM function instance（48 + 16 × upvalue reference 数）。
    pub fn vm_function(upvalue_count: u64) -> u64 {
        48u64.saturating_add(upvalue_count.saturating_mul(16))
    }

    /// AST program root（64 固定）。
    pub const AST_ROOT: u64 = 64;

    /// AST node（64 + node が所有する identifier / string literal の byte 長）。
    pub fn ast_node(owned_bytes: u64) -> u64 {
        64u64.saturating_add(owned_bytes)
    }

    /// bytecode chunk（64 + 16 × opcode 数 + 32 × constant slot 数）。
    pub fn bytecode_chunk(opcode_count: u64, constant_count: u64) -> u64 {
        64u64
            .saturating_add(opcode_count.saturating_mul(16))
            .saturating_add(constant_count.saturating_mul(32))
    }

    /// imported module record（96 + normalized module ID の UTF-8 byte 長）。
    pub fn imported_module_record(module_id_bytes: u64) -> u64 {
        96u64.saturating_add(module_id_bytes)
    }

    /// continuation frame（96 + 32 × frame が所有する local slot 数）。
    pub fn continuation_frame(local_count: u64) -> u64 {
        96u64.saturating_add(local_count.saturating_mul(32))
    }

    /// exception handler / loop handler（32 固定）。
    pub const HANDLER: u64 = 32;

    /// rollback journal entry（48 + 保持する旧 value の到達 payload）。
    pub fn rollback_journal_entry(retained_payload: u64) -> u64 {
        48u64.saturating_add(retained_payload)
    }
}

/// execution ごとの heap allocation 台帳（§5.2）。
///
/// 単調増加する [`AllocationId`] を配り、同じ ID を二度課金しない。live heap byte 数を
/// 管理し、新規 allocation 前に `HeapBytes` 上限を checked add で検査する。object への
/// 最後の execution 内参照が消えたら [`AllocationLedger::release`] で live heap を戻す
/// （cumulative の string/source/I-O counter は減らさない。§7-6）。
///
/// # PR 分割メモ（REV-015 Slice 2）
///
/// 本 PR（heap 基盤）は「生成時に論理サイズを課金し、境界で live heap を突き合わせる」
/// 方式で `charge` / `release` / `peak` を提供する。`Value` へ [`AllocationId`] を持たせ
/// drop 通知で正確に per-drop release する忠実版は後続 PR で載せる。したがって本台帳の
/// `charge`/`release` は呼び出し側（engine）が allocation site と boundary reconciliation
/// から明示的に driveする。
pub struct AllocationLedger {
    /// 次に配る AllocationId。1 から単調増加する（0 は「未割り当て」を表す）。
    next_id: u64,
    /// 現在 live な論理 heap byte 数。
    live_bytes: u64,
    /// これまでに観測した live_bytes の最大値（§3 peak）。
    peak_bytes: u64,
    /// live 上限（`BudgetConfig::max_live_heap_bytes`）。
    limit: u64,
}

impl AllocationLedger {
    /// live 上限から空の台帳を作る。
    pub fn new(limit: u64) -> Self {
        Self {
            next_id: 1,
            live_bytes: 0,
            peak_bytes: 0,
            limit,
        }
    }

    /// 新しい [`AllocationId`] を 1 個発番する（§5.2）。
    ///
    /// overflow は 0 へ wrap せず [`InternalFailure::AllocationIdExhausted`] を返す。
    pub fn allocate_id(&mut self) -> Result<AllocationId, ControlStop> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ControlStop::InternalFailure(
                InternalFailure::AllocationIdExhausted,
            ))?;
        Ok(AllocationId(id))
    }

    /// 新規 object 1 個の論理サイズ `bytes` を live heap へ課金する（§5.2）。
    ///
    /// `live_bytes + bytes` を checked add で計算し、上限超過 / overflow は
    /// `BudgetExceeded(HeapBytes)` を返す（§7.1: overflow も超過として扱い
    /// `requested = u64::MAX`）。成功時は live_bytes を増やし peak を `max` 更新する。
    pub fn charge(&mut self, bytes: u64, phase: ExecutionPhase) -> Result<(), ControlStop> {
        let projected = self.live_bytes.checked_add(bytes);
        match projected {
            Some(total) if total <= self.limit => {
                self.live_bytes = total;
                if total > self.peak_bytes {
                    self.peak_bytes = total;
                }
                Ok(())
            }
            Some(_) => Err(ControlStop::BudgetExceeded(BudgetExceeded {
                resource: BudgetResource::HeapBytes,
                limit: self.limit,
                used: self.live_bytes,
                reserved: 0,
                requested: bytes,
                unit: BudgetUnit::Bytes,
                phase,
            })),
            None => Err(ControlStop::BudgetExceeded(BudgetExceeded {
                resource: BudgetResource::HeapBytes,
                limit: self.limit,
                used: self.live_bytes,
                reserved: 0,
                requested: u64::MAX,
                unit: BudgetUnit::Bytes,
                phase,
            })),
        }
    }

    /// object の最後の execution 内参照が消えたときに live heap を戻す（§5.2 / §7-6）。
    ///
    /// live byte の減少であり、cumulative counter の refund ではない。二重 release で
    /// 0 未満へ回らないよう saturating で引く（§7.1 の禁止は saturating *更新* による
    /// 上限迂回であり、release の 0 下限 clamp は許される）。peak は据え置く。
    pub fn release(&mut self, bytes: u64) {
        self.live_bytes = self.live_bytes.saturating_sub(bytes);
    }

    /// 現在の live heap byte 数。
    pub fn live_bytes(&self) -> u64 {
        self.live_bytes
    }

    /// これまでの live heap byte 数の最大値（§3 peak）。
    pub fn peak_bytes(&self) -> u64 {
        self.peak_bytes
    }

    /// live 上限を再設定する（`set_max_live_heap_bytes` 互換の内部入口）。
    pub fn set_limit(&mut self, limit: u64) {
        self.limit = limit;
    }

    /// live heap byte 数を指定値へ復元する（REPL 未捕捉エラー時の rollback 用）。
    /// peak は据え置く。
    pub fn restore_live_bytes(&mut self, live_bytes: u64) {
        self.live_bytes = live_bytes;
    }
}

// =============================================================================
// 共有 heap 台帳ハンドル（per-drop release 用、REV-015 案A PR-a）
// =============================================================================

/// [`AllocationLedger`] への強参照ハンドル。
///
/// `BudgetLedger` が 1 本保持し、`Tracked` collection には [`HeapLedgerWeak`] を配る。
/// per-drop release は tracked collection の `Drop` から `Weak::upgrade` して
/// `release` を呼ぶため、台帳を単一 owner の field から `Rc<RefCell<>>` の共有へ移す。
pub type HeapLedger = Rc<RefCell<AllocationLedger>>;

/// [`AllocationLedger`] への弱参照。tracked collection が保持する。
///
/// `Weak` にすることで (1) tracked value が engine（＝台帳）より長生きしても台帳を
/// 生かし続けず、(2) 台帳 → value → 台帳 の循環を作らない。engine drop 後の遅延
/// release は `upgrade` が `None` を返し no-op になる（その時点で live 集計は不要）。
pub type HeapLedgerWeak = Weak<RefCell<AllocationLedger>>;

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
    /// live heap accounting（§5.2、REV-015 Slice 2）。
    ///
    /// per-drop release（案A）のため `Rc<RefCell<>>` 共有ハンドルにする。tracked
    /// collection へ [`HeapLedgerWeak`] を配り、その `Drop` から release させる。
    heap: HeapLedger,
}

impl BudgetLedger {
    /// config と cancellation token から台帳を作る。
    pub fn new(config: BudgetConfig, cancellation: CancellationToken) -> Self {
        Self {
            heap: Rc::new(RefCell::new(AllocationLedger::new(
                config.max_live_heap_bytes,
            ))),
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
            live_heap_bytes: self.heap.borrow().live_bytes(),
            // reserved_heap は per-drop release 導入（後続 PR）まで常に 0。本 PR の
            // heap 課金は生成時に live へ直接 commit する方式で reserve を保持しない。
            reserved_heap_bytes: 0,
            peak_heap_bytes: self.heap.borrow().peak_bytes(),
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
    /// 実装なら新規 allocation として数えないが、現行の連結・substring は依然 body を
    /// copy するので生成のたびに課金する（§5.3）。この cumulative 会計は解放しても
    /// 減らさず、String body の live heap per-drop 追跡（`Value::new_str` /
    /// `track_result`、REV-015 PR-b）とは独立した別会計である。
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

    /// dispatch 結果の `Value` から到達する新規 String body の cumulative 会計を課金する
    /// （REV-015 Slice 2、string accounting）。
    ///
    /// 結果内の各 String body を [`Self::charge_string`] で cumulative 課金する。List /
    /// Dict の backing byte と String body の live heap（per-drop）は
    /// [`Self::track_result`] が担うため、engine の dispatch 境界では `track_result` を
    /// 使う。本メソッドは cumulative 会計だけを行う補助関数として残す（live heap は
    /// 触らない）。Dict の key も String body なので課金する。再帰は使わず worklist で
    /// 走査し、深い構造でも host stack を消費しない。
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

    /// 生成・変更された `Value` を走査し、untracked な collection backing を tracked へ
    /// 変換しつつ heap 課金する（REV-015 案A、per-drop release）。
    ///
    /// `builtin_core` は heap 台帳を持たないため collection を untracked
    /// （`Tracked::constant`、`AllocationId(0)`）で生成する。engine 側は builtin dispatch
    /// や push/pop/index 代入の直後にこの pass を通し、untracked backing を（子要素を
    /// 先に処理してから）tracked backing へ包み直して §5.1 の body サイズを課金する。
    /// 既に tracked（`id != 0`）な backing は再課金せず据え置く（共有・再利用の二重課金
    /// 防止、§5.2）。String body の課金も併せて行い、[`Self::charge_result_strings`] の
    /// 役割を包含する。
    ///
    /// 値を所有権ごと受け取り、tracked 化した値を返す。上限超過・overflow は
    /// `ControlStop`（catch 不能 terminal）。深い構造でも再帰は最小限（child を処理して
    /// から親を包む後順走査）に留める。
    pub fn track_result(
        &mut self,
        value: crate::value::Value,
        phase: ExecutionPhase,
    ) -> Result<crate::value::Value, ControlStop> {
        use crate::value::Value;
        match value {
            Value::Str(s) => {
                if s.alloc_id().0 != 0 {
                    // 既に tracked（live heap 課金済み）。cumulative 会計も生成時に
                    // 済んでいるので据え置く（§5.2 共有二重課金防止）。
                    return Ok(Value::Str(s));
                }
                // untracked（builtin_core / literal などが untracked で作った String）。
                // cumulative の string accounting（§5.3、解放で減らない累積値）を課金し、
                // さらに live heap（§5.1 String body）へ tracked backing として課金する。
                // untracked backing の Drop は no-op なので clone_data で中身を取り出す。
                let text: String = s.clone_data();
                drop(s);
                self.charge_string(text.len() as u64, phase)?;
                Value::new_str(text, self, phase)
            }
            Value::List(list) => {
                if list.alloc_id().0 != 0 {
                    // 既に tracked。子は生成時に処理済みなので据え置く。
                    return Ok(Value::List(list));
                }
                // untracked。子を先に track し、その後 backing を tracked で包む。
                // untracked backing の Drop は no-op なので clone で中身を取り出してよい。
                let data: Vec<Value> = list.clone_data();
                drop(list);
                let mut tracked_items = Vec::with_capacity(data.len());
                for item in data {
                    tracked_items.push(self.track_result(item, phase)?);
                }
                Value::new_list(tracked_items, self, phase)
            }
            Value::Dict(dict) => {
                if dict.alloc_id().0 != 0 {
                    return Ok(Value::Dict(dict));
                }
                let data: std::collections::BTreeMap<String, Value> = dict.clone_data();
                drop(dict);
                let mut tracked_map = std::collections::BTreeMap::new();
                for (k, item) in data {
                    tracked_map.insert(k, self.track_result(item, phase)?);
                }
                Value::new_dict(tracked_map, self, phase)
            }
            // scalar / Fn / VmFn / Error は collection backing を持たない。
            // Fn/VmFn/Error の内部 String は既存 body の写像で本 PR の対象外。
            other => Ok(other),
        }
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

    // -------------------------------------------------------------------------
    // heap accounting（§5.2、REV-015 Slice 2）
    // -------------------------------------------------------------------------

    /// tracked collection へ配る heap 台帳への弱参照（案A PR-a）。
    ///
    /// `Tracked` collection はこの [`HeapLedgerWeak`] を保持し、`Drop` で `upgrade`
    /// して `release` を呼ぶ。engine（＝台帳の強参照 owner）が drop 済みなら upgrade は
    /// `None` になり release は no-op（その時点で live 集計は不要）。
    pub fn heap_handle(&self) -> HeapLedgerWeak {
        Rc::downgrade(&self.heap)
    }

    /// heap 台帳を borrow して 1 回課金する内部ヘルパ（baseline 走査などで使う）。
    fn heap_charge_raw(&self, bytes: u64, phase: ExecutionPhase) -> Result<(), ControlStop> {
        self.heap.borrow_mut().charge(bytes, phase)
    }

    /// 新しい [`AllocationId`] を 1 個発番する（§5.2）。
    ///
    /// overflow は [`ControlStop::InternalFailure`]（`AllocationIdExhausted`）。
    pub fn allocate_heap_id(&mut self) -> Result<AllocationId, ControlStop> {
        self.heap.borrow_mut().allocate_id()
    }

    /// 新規 heap object 1 個の論理サイズ `bytes` を live heap へ課金する（§5.2）。
    ///
    /// [`heap_size`] の各関数で算出した論理サイズを渡す。上限超過 / overflow は
    /// `BudgetExceeded(HeapBytes)`。cancel を charge 前に確認する（§7-1）。
    pub fn charge_heap(&mut self, bytes: u64, phase: ExecutionPhase) -> Result<(), ControlStop> {
        self.check_cancel()?;
        self.check_deadline()?;
        self.heap.borrow_mut().charge(bytes, phase)
    }

    /// object の最後の execution 内参照が消えたときに live heap を戻す（§5.2 / §7-6）。
    ///
    /// cumulative counter は減らさない。二重 release でも 0 未満へ回らない。
    pub fn release_heap(&mut self, bytes: u64) {
        self.heap.borrow_mut().release(bytes);
    }

    /// 現在の live heap byte 数（REPL rollback の snapshot 用）。
    pub fn live_heap_bytes(&self) -> u64 {
        self.heap.borrow().live_bytes()
    }

    /// live heap byte 数を指定値へ復元する（REPL 未捕捉エラー時の rollback 用）。
    /// peak は据え置く。
    pub fn restore_live_heap_bytes(&mut self, live_heap_bytes: u64) {
        self.heap.borrow_mut().restore_live_bytes(live_heap_bytes);
    }

    /// live heap 上限を再設定する。
    pub fn set_max_live_heap_bytes(&mut self, limit: u64) {
        self.config.max_live_heap_bytes = limit;
        self.heap.borrow_mut().set_limit(limit);
    }

    /// `ExecutionContext` に既存の変数・closure・collection を、新 execution の
    /// baseline live heap として課金する（§5.2 末尾 / §15.2 「context baseline」）。
    ///
    /// `Link` フェーズで最初の文を実行する前に呼ぶ。root として渡された `Value` 群から
    /// 到達する heap object を反復 worklist で走査し、§5.1 の論理サイズで live heap へ
    /// 課金する。再帰走査は使わず、同じ heap object（`Rc` pointer 同一性）を visited set
    /// で 1 回だけ数える。baseline が上限を超える場合は `BudgetExceeded(HeapBytes)` を
    /// 返し、呼び出し側は 1 文も実行しない。
    ///
    /// # 走査の根と clone について
    ///
    /// tree engine は各変数 cell が保持する `Value`、VM は globals / stack slot が保持する
    /// `Value` を root として渡す。root は所有 `Value` として受け取る。`List` / `Dict` /
    /// `Fn.captured` / cell は `Rc` 共有なので clone は handle 複製の O(1) であり、実体は
    /// 複製しない（String body だけは実 copy だが baseline 走査は 1 回きり）。共有 backing
    /// は pointer 同一性で dedup するため、同じ object を複数 root から指しても 1 回だけ
    /// 課金する（§5.2）。
    pub fn charge_context_baseline<I>(
        &mut self,
        roots: I,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop>
    where
        I: IntoIterator<Item = crate::value::Value>,
    {
        self.check_cancel()?;
        self.check_deadline()?;

        use crate::value::Value;
        use std::collections::HashSet;

        // 訪問済み heap object を pointer 同一性で除外する（§5.2 visited set）。
        // String / List / Dict / captured cell / VmFn upvalue cell / Fn captured map は
        // Rc 共有され得るため、backing の pointer で dedup し、共有 backing は 1 回だけ
        // 課金する（§5.2）。String backing は `Rc<TrackedStr>`（REV-015 PR-b）。
        let mut visited: HashSet<usize> = HashSet::new();
        let mut worklist: Vec<Value> = roots.into_iter().collect();

        while let Some(v) = worklist.pop() {
            // この Value slot 自体を課金する（§5.1 Value slot = 32）。
            self.heap_charge_raw(heap_size::VALUE_SLOT, phase)?;
            match v {
                Value::Str(s) => {
                    let ptr = Rc::as_ptr(&s) as usize;
                    if !visited.insert(ptr) {
                        continue;
                    }
                    self.heap_charge_raw(heap_size::string_body(s.len() as u64), phase)?;
                }
                Value::List(items) => {
                    let ptr = Rc::as_ptr(&items) as usize;
                    if !visited.insert(ptr) {
                        continue;
                    }
                    self.heap_charge_raw(heap_size::list_body(items.len() as u64), phase)?;
                    for item in items.iter() {
                        worklist.push(item.clone());
                    }
                }
                Value::Dict(map) => {
                    let ptr = Rc::as_ptr(&map) as usize;
                    if !visited.insert(ptr) {
                        continue;
                    }
                    let key_bytes_total: u64 = map
                        .keys()
                        .map(|k| k.len() as u64)
                        .fold(0u64, u64::saturating_add);
                    self.heap_charge_raw(
                        heap_size::dict_body(map.len() as u64, key_bytes_total),
                        phase,
                    )?;
                    for item in map.values() {
                        worklist.push(item.clone());
                    }
                }
                Value::Fn { captured, .. } => {
                    let ptr = Rc::as_ptr(&captured) as usize;
                    if !visited.insert(ptr) {
                        continue;
                    }
                    // tree function instance（64 + 16 × captured cell 数、§5.1）。
                    self.heap_charge_raw(heap_size::tree_function(captured.len() as u64), phase)?;
                    // captured cell 1 個ずつ（§5.1 captured cell = 32）と、その中身を辿る。
                    for cell in captured.values() {
                        let cell_ptr = Rc::as_ptr(cell) as usize;
                        if visited.insert(cell_ptr) {
                            self.heap_charge_raw(heap_size::CAPTURED_CELL, phase)?;
                            worklist.push(cell.borrow().clone());
                        }
                    }
                }
                Value::VmFn { upvalues, .. } => {
                    // VM function instance（48 + 16 × upvalue 数、§5.1）。VmFn 自体は Rc
                    // 共有されないため body ごとに課金する。
                    self.heap_charge_raw(heap_size::vm_function(upvalues.len() as u64), phase)?;
                    for cell in upvalues.iter() {
                        let cell_ptr = Rc::as_ptr(cell) as usize;
                        if visited.insert(cell_ptr) {
                            self.heap_charge_raw(heap_size::CAPTURED_CELL, phase)?;
                            worklist.push(cell.borrow().clone());
                        }
                    }
                }
                Value::Error { message, .. } => {
                    // Error の message を live な String body として 1 個数える（§5.1）。
                    self.heap_charge_raw(heap_size::string_body(message.len() as u64), phase)?;
                }
                // Int / Float / Bool / Null は Value slot だけで、追加 payload なし。
                _ => {}
            }
        }
        Ok(())
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
            BudgetResource::HeapBytes => TsumugiError::heap_limit(line, e.limit),
            _ => TsumugiError::step_limit(line, e.limit),
        },
        ControlStop::Cancelled | ControlStop::DeadlineExceeded { .. } => {
            TsumugiError::step_limit(line, committed_fuel)
        }
        ControlStop::InternalFailure(InternalFailure::AllocationIdExhausted) => {
            TsumugiError::internal(line, "AllocationId を割り当てできません")
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

    // -------------------------------------------------------------------------
    // heap accounting（§5.1 論理サイズ / §5.2 AllocationLedger、§15.2 heap）
    // -------------------------------------------------------------------------

    #[test]
    fn heap_size_table_matches_spec_5_1() {
        // §5.1 の論理サイズ表の値をそのまま固定する。
        assert_eq!(heap_size::VALUE_SLOT, 32);
        assert_eq!(heap_size::CAPTURED_CELL, 32);
        assert_eq!(heap_size::string_body(0), 24);
        assert_eq!(heap_size::string_body(10), 34);
        assert_eq!(heap_size::list_body(0), 24);
        assert_eq!(heap_size::list_body(3), 24 + 32 * 3);
        assert_eq!(heap_size::dict_body(0, 0), 24);
        // entry 2 個、key 合計 5 byte。
        assert_eq!(heap_size::dict_body(2, 5), 24 + 64 * 2 + 5);
        assert_eq!(heap_size::tree_function(0), 64);
        assert_eq!(heap_size::tree_function(2), 64 + 16 * 2);
        assert_eq!(heap_size::vm_function(0), 48);
        assert_eq!(heap_size::vm_function(3), 48 + 16 * 3);
        assert_eq!(heap_size::AST_ROOT, 64);
        assert_eq!(heap_size::ast_node(0), 64);
        assert_eq!(heap_size::ast_node(7), 71);
        assert_eq!(heap_size::bytecode_chunk(0, 0), 64);
        assert_eq!(heap_size::bytecode_chunk(4, 2), 64 + 16 * 4 + 32 * 2);
        assert_eq!(heap_size::imported_module_record(0), 96);
        assert_eq!(heap_size::imported_module_record(8), 104);
        assert_eq!(heap_size::continuation_frame(0), 96);
        assert_eq!(heap_size::continuation_frame(2), 96 + 32 * 2);
        assert_eq!(heap_size::HANDLER, 32);
        assert_eq!(heap_size::rollback_journal_entry(0), 48);
        assert_eq!(heap_size::rollback_journal_entry(16), 64);
    }

    #[test]
    fn heap_size_helpers_saturate_on_overflow() {
        // overflow は panic せず u64::MAX へ飽和する（課金側で HeapBytes 超過へ写像）。
        assert_eq!(heap_size::list_body(u64::MAX), u64::MAX);
        assert_eq!(heap_size::string_body(u64::MAX), u64::MAX);
        assert_eq!(heap_size::dict_body(u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn allocation_id_is_monotonic_from_one() {
        let mut ledger = AllocationLedger::new(1_000);
        assert_eq!(ledger.allocate_id().unwrap(), AllocationId(1));
        assert_eq!(ledger.allocate_id().unwrap(), AllocationId(2));
        assert_eq!(ledger.allocate_id().unwrap(), AllocationId(3));
    }

    #[test]
    fn allocation_id_overflow_is_internal_failure_not_wrap() {
        let mut ledger = AllocationLedger::new(0);
        // next_id を最後の 1 個へ寄せる。allocate_id は id を返した後 next_id を
        // checked_add(1) するため、配れる最後の id は u64::MAX - 1。
        ledger.next_id = u64::MAX - 1;
        assert_eq!(ledger.allocate_id().unwrap(), AllocationId(u64::MAX - 1));
        // 次は next_id == u64::MAX で checked_add(1) が None。0 へ wrap せず InternalFailure。
        assert_eq!(
            ledger.allocate_id(),
            Err(ControlStop::InternalFailure(
                InternalFailure::AllocationIdExhausted
            ))
        );
    }

    #[test]
    fn heap_charge_at_limit_succeeds_and_plus_one_exceeds() {
        let mut ledger = AllocationLedger::new(100);
        assert!(ledger.charge(60, ExecutionPhase::Run).is_ok());
        assert!(ledger.charge(40, ExecutionPhase::Run).is_ok());
        assert_eq!(ledger.live_bytes(), 100);
        assert_eq!(ledger.peak_bytes(), 100);
        // +1 は HeapBytes 超過。used は現在の live、requested は要求量。
        let err = ledger.charge(1, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::HeapBytes);
                assert_eq!(e.limit, 100);
                assert_eq!(e.used, 100);
                assert_eq!(e.requested, 1);
                assert_eq!(e.unit, BudgetUnit::Bytes);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
        // 超過は live を変えない。
        assert_eq!(ledger.live_bytes(), 100);
    }

    #[test]
    fn heap_charge_overflow_reports_requested_u64_max() {
        let mut ledger = AllocationLedger::new(u64::MAX);
        assert!(ledger.charge(10, ExecutionPhase::Run).is_ok());
        // live(10) + u64::MAX は overflow。§7.1: requested = u64::MAX。
        let err = ledger.charge(u64::MAX, ExecutionPhase::Run).unwrap_err();
        match err {
            ControlStop::BudgetExceeded(e) => {
                assert_eq!(e.resource, BudgetResource::HeapBytes);
                assert_eq!(e.requested, u64::MAX);
            }
            other => panic!("unexpected stop: {other:?}"),
        }
    }

    #[test]
    fn heap_release_recovers_live_but_not_peak() {
        let mut ledger = AllocationLedger::new(100);
        assert!(ledger.charge(80, ExecutionPhase::Run).is_ok());
        assert_eq!(ledger.live_bytes(), 80);
        assert_eq!(ledger.peak_bytes(), 80);
        ledger.release(50);
        assert_eq!(ledger.live_bytes(), 30);
        // peak は据え置き。
        assert_eq!(ledger.peak_bytes(), 80);
        // release で空いた分を再度確保できる（mass allocate/free 相当）。
        assert!(ledger.charge(70, ExecutionPhase::Run).is_ok());
        assert_eq!(ledger.live_bytes(), 100);
        assert_eq!(ledger.peak_bytes(), 100);
    }

    #[test]
    fn heap_release_saturates_at_zero() {
        let mut ledger = AllocationLedger::new(100);
        assert!(ledger.charge(10, ExecutionPhase::Run).is_ok());
        // 二重 release でも 0 未満へ回らない。
        ledger.release(50);
        assert_eq!(ledger.live_bytes(), 0);
    }

    #[test]
    fn zero_heap_limit_forbids_first_nonzero_charge() {
        let mut ledger = AllocationLedger::new(0);
        // 0 byte 課金は上限 0 ちょうどで成功。
        assert!(ledger.charge(0, ExecutionPhase::Run).is_ok());
        // 1 byte は超過。
        assert!(matches!(
            ledger.charge(1, ExecutionPhase::Run),
            Err(ControlStop::BudgetExceeded(BudgetExceeded {
                resource: BudgetResource::HeapBytes,
                ..
            }))
        ));
    }

    #[test]
    fn budget_ledger_usage_reflects_live_and_peak_heap() {
        let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
        config.max_live_heap_bytes = 1_000;
        let mut l = ledger(config);
        assert!(l.charge_heap(300, ExecutionPhase::Run).is_ok());
        assert!(l.charge_heap(200, ExecutionPhase::Run).is_ok());
        assert_eq!(l.usage().live_heap_bytes, 500);
        assert_eq!(l.usage().peak_heap_bytes, 500);
        l.release_heap(400);
        assert_eq!(l.usage().live_heap_bytes, 100);
        // peak は据え置き。
        assert_eq!(l.usage().peak_heap_bytes, 500);
    }

    #[test]
    fn heap_charge_is_cancelled_before_charge() {
        let token = CancellationToken::new();
        let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
        config.max_live_heap_bytes = 1_000;
        let mut l = BudgetLedger::new(config, token.clone());
        token.cancel();
        assert_eq!(
            l.charge_heap(10, ExecutionPhase::Run),
            Err(ControlStop::Cancelled)
        );
    }

    // ---- context baseline 走査（§5.2 / §15.2） ----

    use crate::value::{FunctionId, Tracked, Value};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc as StdRc;

    fn heap_ledger(limit: u64) -> BudgetLedger {
        let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
        config.max_live_heap_bytes = limit;
        ledger(config)
    }

    #[test]
    fn baseline_charges_scalar_value_slot_only() {
        let mut l = heap_ledger(1_000);
        l.charge_context_baseline([Value::Int(7)], ExecutionPhase::Link)
            .unwrap();
        // Value slot だけ（32）。
        assert_eq!(l.usage().live_heap_bytes, heap_size::VALUE_SLOT);
    }

    #[test]
    fn baseline_charges_string_body() {
        let mut l = heap_ledger(1_000);
        l.charge_context_baseline(
            [Value::str_constant("hello".to_string())],
            ExecutionPhase::Link,
        )
        .unwrap();
        // Value slot 32 + String body(24 + 5)。
        assert_eq!(
            l.usage().live_heap_bytes,
            heap_size::VALUE_SLOT + heap_size::string_body(5)
        );
    }

    #[test]
    fn baseline_charges_list_and_elements() {
        let mut l = heap_ledger(10_000);
        let list = Value::List(Tracked::constant(vec![Value::Int(1), Value::Int(2)]));
        l.charge_context_baseline([list], ExecutionPhase::Link)
            .unwrap();
        // list Value slot 32 + list body(24 + 32*2) + 各要素 Value slot 32*2。
        let expected = heap_size::VALUE_SLOT + heap_size::list_body(2) + heap_size::VALUE_SLOT * 2;
        assert_eq!(l.usage().live_heap_bytes, expected);
    }

    #[test]
    fn baseline_charges_dict_with_key_bytes() {
        let mut l = heap_ledger(10_000);
        let mut map = BTreeMap::new();
        map.insert("ab".to_string(), Value::Int(1));
        map.insert("cde".to_string(), Value::Int(2));
        let dict = Value::Dict(Tracked::constant(map));
        l.charge_context_baseline([dict], ExecutionPhase::Link)
            .unwrap();
        // dict Value slot 32 + dict body(24 + 64*2 + key bytes(2+3)) + 各値 Value slot 32*2。
        let expected =
            heap_size::VALUE_SLOT + heap_size::dict_body(2, 5) + heap_size::VALUE_SLOT * 2;
        assert_eq!(l.usage().live_heap_bytes, expected);
    }

    #[test]
    fn baseline_shared_rc_charged_once() {
        // 同じ List backing を 2 つの root から指しても 1 回だけ課金する（§5.2 visited）。
        let mut l_shared = heap_ledger(100_000);
        let shared = Tracked::constant(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let a = Value::List(StdRc::clone(&shared));
        let b = Value::List(StdRc::clone(&shared));
        l_shared
            .charge_context_baseline([a, b], ExecutionPhase::Link)
            .unwrap();

        // 別 backing の等価な 2 List だと body と要素が 2 回課金される。
        let mut l_distinct = heap_ledger(100_000);
        let c = Value::List(Tracked::constant(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]));
        let d = Value::List(Tracked::constant(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]));
        l_distinct
            .charge_context_baseline([c, d], ExecutionPhase::Link)
            .unwrap();

        // shared は body+要素を 1 回だけ、distinct は 2 回課金するので厳密に少ない。
        assert!(l_shared.usage().live_heap_bytes < l_distinct.usage().live_heap_bytes);
        // shared: 2 root Value slot + body 1 回 + 要素 3 個の Value slot。
        let shared_expected =
            heap_size::VALUE_SLOT * 2 + heap_size::list_body(3) + heap_size::VALUE_SLOT * 3;
        assert_eq!(l_shared.usage().live_heap_bytes, shared_expected);
    }

    #[test]
    fn baseline_charges_tree_function_and_captured_cell() {
        let mut l = heap_ledger(10_000);
        // captured cell 1 個（中身は Int）を持つ tree 関数値。
        let cell: crate::value::SharedValue = StdRc::new(RefCell::new(Value::Int(5)));
        let mut captured = std::collections::HashMap::new();
        captured.insert("x".to_string(), cell);
        let func = Value::Fn {
            id: FunctionId(1),
            def: StdRc::new(crate::value::FnDef {
                name: "f".to_string(),
                params: vec![],
                body: vec![],
            }),
            captured: StdRc::new(captured),
        };
        l.charge_context_baseline([func], ExecutionPhase::Link)
            .unwrap();
        // fn Value slot 32 + tree_function(1) + captured cell 32 + cell 内 Int の Value slot 32。
        let expected = heap_size::VALUE_SLOT
            + heap_size::tree_function(1)
            + heap_size::CAPTURED_CELL
            + heap_size::VALUE_SLOT;
        assert_eq!(l.usage().live_heap_bytes, expected);
    }

    #[test]
    fn baseline_overflow_reports_heap_bytes() {
        // 上限を root 1 個ぶんに満たない値にすると HeapBytes 超過になる。
        let mut l = heap_ledger(10);
        let err = l
            .charge_context_baseline([Value::Int(1)], ExecutionPhase::Link)
            .unwrap_err();
        assert!(matches!(
            err,
            ControlStop::BudgetExceeded(BudgetExceeded {
                resource: BudgetResource::HeapBytes,
                ..
            })
        ));
    }

    // ---- track_result: untracked collection の tracked 化と課金（REV-015 案A）----

    #[test]
    fn track_result_charges_untracked_collection_once_and_releases_on_drop() {
        let mut l = heap_ledger(1_000_000);
        // untracked（Tracked::constant）な List を dispatch 境界で tracked 化する。
        let untracked = Value::List(Tracked::constant(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(l.usage().live_heap_bytes, 0);
        let tracked = l.track_result(untracked, ExecutionPhase::Run).unwrap();
        // list body(2) が 1 回だけ課金される。
        assert_eq!(l.usage().live_heap_bytes, heap_size::list_body(2));
        drop(tracked);
        // per-drop release で 0 へ戻る。
        assert_eq!(l.usage().live_heap_bytes, 0);
    }

    #[test]
    fn track_result_leaves_already_tracked_collection_uncharged_again() {
        let mut l = heap_ledger(1_000_000);
        // 既に tracked な List（new_list 経由）。
        let tracked = Value::new_list(vec![Value::Int(1)], &mut l, ExecutionPhase::Run).unwrap();
        let before = l.usage().live_heap_bytes;
        assert_eq!(before, heap_size::list_body(1));
        // track_result は tracked（id != 0）を再課金しない。
        let again = l.track_result(tracked, ExecutionPhase::Run).unwrap();
        assert_eq!(l.usage().live_heap_bytes, before);
        drop(again);
        assert_eq!(l.usage().live_heap_bytes, 0);
    }

    #[test]
    fn track_result_charges_nested_untracked_collections() {
        let mut l = heap_ledger(1_000_000);
        // untracked な外側 List に untracked な内側 List を格納。
        let inner = Value::List(Tracked::constant(vec![Value::Int(1)]));
        let outer = Value::List(Tracked::constant(vec![inner]));
        let tracked = l.track_result(outer, ExecutionPhase::Run).unwrap();
        // 外側 body(1) + 内側 body(1) の両方が課金される。
        assert_eq!(l.usage().live_heap_bytes, heap_size::list_body(1) * 2);
        drop(tracked);
        assert_eq!(l.usage().live_heap_bytes, 0);
    }

    #[test]
    fn track_result_charges_strings_like_before() {
        let mut l = heap_ledger(1_000_000);
        // track_result は string accounting（cumulative）も包含する。
        let v = Value::str_constant("hello".to_string());
        let tracked = l.track_result(v, ExecutionPhase::Run).unwrap();
        assert_eq!(l.usage().committed.string_allocations, 1);
        assert_eq!(l.usage().committed.string_bytes, 5);
        // さらに live heap（§5.1 String body = 24 + 5）へ tracked backing として課金する。
        assert_eq!(l.usage().live_heap_bytes, heap_size::string_body(5));
        // 最後の参照 drop で live heap が戻る（§5.2）。cumulative は据え置き（§5.3）。
        drop(tracked);
        assert_eq!(l.usage().live_heap_bytes, 0);
        assert_eq!(l.usage().committed.string_allocations, 1);
        assert_eq!(l.usage().committed.string_bytes, 5);
    }

    #[test]
    fn track_result_leaves_already_tracked_string_uncharged_again() {
        let mut l = heap_ledger(1_000_000);
        // 既に tracked な String（new_str 経由）。
        let tracked = Value::new_str("hi".to_string(), &mut l, ExecutionPhase::Run).unwrap();
        let live_before = l.usage().live_heap_bytes;
        let alloc_before = l.usage().committed.string_allocations;
        assert_eq!(live_before, heap_size::string_body(2));
        // track_result は tracked（id != 0）を再課金しない（live も cumulative も）。
        let again = l.track_result(tracked, ExecutionPhase::Run).unwrap();
        assert_eq!(l.usage().live_heap_bytes, live_before);
        assert_eq!(l.usage().committed.string_allocations, alloc_before);
        drop(again);
        assert_eq!(l.usage().live_heap_bytes, 0);
    }
}
