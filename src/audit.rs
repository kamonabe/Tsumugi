//! Phase 6 実行時監査（audit）の schema v1 と logical journal（A-1 スライス）。
//!
//! 設計正本は [`docs/determinism-and-audit.md`](../docs/determinism-and-audit.md) §7（schema）/
//! §8（event 順序・完全性）/ §10（sink 契約・fail-closed）/ §11（監査自体の予算）である。
//! 本モジュールは A-1 の最小縦切りを実装する:
//!
//! - schema version 1 の型（§7.1 `AuditEnvelope` / §7.2 `AuditEvent` 全8 variant と
//!   `TerminalOutcome`）。ただし emission するのは `ExecutionStarted` と `Terminal` だけで、
//!   残り6 event と redaction payload 型は **型のみ**（構築配線なし）。
//! - per-execution bounded logical journal（§8/§11）。`sequence` は 0 始まり、gap/重複なし、
//!   checked increment（overflow は fail-closed）。Started 時に Terminal 専用の
//!   event slot(1) + 64 KiB emergency byte slot を予約し、通常 event はこれを使えない。
//! - `AuditSink` trait（§10）と、常に `Ack` を返す同期 in-process sink、
//!   `Mutex<Vec<AuditEnvelope>>` ベースの in-memory test sink。
//! - fail-closed（§10.1）: Started が ack される前に script work を開始しない。sink が `Failed`
//!   なら emergency slot へ `Terminal(AuditFailure)` を append し、success でない結果を返す。
//!
//! # A-1 で意図的に実装しない範囲（後続スライス）
//!
//! - `AuditBackpressure` の yield 配線（§10.2）と `engine.rs::YieldReason` 変種。
//! - `ExecutionStarted` / `Terminal` 以外6 event の emission。
//! - redaction policy 本体（§9）と strict deterministic CBOR（§9.2）。`encoded_bytes` は
//!   [`estimate_encoded_bytes`] の naive 推定で、後で §9.2 encoder へ差し替え可能に隔離する。
//! - record/replay（§6）、VM 変更、§1.1 の sink 必須ポリシー（opt-in のまま）。

use std::sync::Arc;
use std::sync::Mutex;

use crate::budget::{BudgetConfig, BudgetResource, BudgetUsage};
use crate::embedding::{Backend, ExecutionId, ExecutionOutcome};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

// =============================================================================
// §7.1 envelope
// =============================================================================

/// 監査 event 1 件を包む envelope（§7.1）。schema version 1 で field 名・型・意味を固定する。
#[derive(Clone, Debug, PartialEq)]
pub struct AuditEnvelope {
    /// 常に 1（schema version）。
    pub schema_version: u16,
    /// host が供給する 128-bit 実行 ID（§7.1、ambient random source へ engine は触れない）。
    pub execution_id: ExecutionId,
    /// root source の SHA-256。
    pub source_hash: [u8; 32],
    /// 言語 revision 文字列（例 `"0.20"`）。
    pub language_revision: String,
    /// 0 始まり・gap なしの logical sequence（§7.1/§8）。
    pub sequence: u64,
    /// 注入 host clock の unix ns。順序の正本ではない（順序は `sequence`）。
    pub timestamp: HostTimestamp,
    /// この envelope が運ぶ event。
    pub event: AuditEvent,
}

/// 注入 audit clock の unix ns（§7.1）。wall clock が後退しても書き換えない。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostTimestamp {
    /// epoch からの unix nanoseconds。
    pub unix_nanoseconds: i128,
}

// =============================================================================
// §7.2 event enum（全8 variant を型として定義。emission は Started/Terminal のみ）
// =============================================================================

/// yield 理由（§7.2）。A-1 では emission しない（`Yielded`/`Resumed` が型のみ）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditYieldReason {
    /// admission queue 待ち。
    AdmissionQueued {
        /// 再開先 phase。
        resume_to: crate::engine::AdmissionPhase,
    },
    /// slice fuel 消尽。
    SliceFuelExhausted,
    /// 明示 yield。
    ExplicitYield,
    /// host call pending。
    HostCallPending {
        /// 対象 call ID。
        call_id: u64,
    },
    /// audit backpressure（§10.2、A-1 では配線しない）。
    AuditBackpressure,
    /// scheduler による preempt。
    SchedulerPreempted,
    /// host による pause。
    HostPaused,
}

/// capability decision の種別（§7.2）。型のみ。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityDecisionKind {
    /// 許可。
    Allow,
    /// 拒否。
    Deny,
}

/// host call の結果種別（§7.2）。型のみ。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostCallOutcome {
    /// 成功。
    Success,
    /// capability により拒否。
    Denied,
    /// host error。
    HostError,
    /// cancel により論理的に閉じた。
    Cancelled,
    /// detach により論理的に閉じた。
    Detached,
}

/// host call の effect 状態（§7.2）。型のみ。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectStatus {
    /// 効果なし。
    None,
    /// 効果が commit された。
    Committed,
    /// 不明。
    Unknown,
}

/// budget charge の理由分類（§7.2）。型のみ。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetChargeReason {
    /// statement。
    Statement,
    /// expression。
    Expression,
    /// operation。
    Operation,
    /// function。
    Function,
    /// loop。
    Loop,
    /// host call。
    HostCall,
    /// bulk elements。
    BulkElements,
    /// bulk bytes。
    BulkBytes,
    /// allocation。
    Allocation,
    /// source。
    Source,
    /// input。
    Input,
    /// output。
    Output,
}

/// 実行モード（§7.2）。A-1 の emission は常に `Live`。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionMode {
    /// 通常実行。
    Live,
    /// record 実行（§6、A-1 範囲外）。
    Record,
    /// replay 実行（§6、A-1 範囲外）。
    Replay,
}

/// redaction 適用後の host-call payload（§9.1）。A-1 では型のみ（本体は未配線）。
///
/// A-1 は host-call lifecycle event を emission しないため、payload の実体は作らない。後続
/// スライスで §9.1 の `AuditField` ツリーへ差し替える。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditPayload {
    /// redaction 済み field 群（A-1 は空のまま）。将来 §9.1 の map へ拡張する。
    pub fields: Vec<(String, AuditPayloadField)>,
}

/// `AuditPayload` の field（§9.1 の `AuditField` の A-1 プレースホルダ、型のみ）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditPayloadField {
    /// 公開可能なスカラ（A-1 ではこの variant だけ型として用意）。
    Public {
        /// redaction 前の canonical byte 長。
        original_bytes: u64,
    },
    /// omit 済み（class 情報を残し値は捨てる）。
    Omitted {
        /// redaction 前の canonical byte 長。
        original_bytes: u64,
    },
}

/// 監査 trace の 1 フレーム（§7.2 `AuditFrame`）。型のみ。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditFrame {
    /// 関数名（最大 256 UTF-8 bytes）。
    pub function: String,
    /// module 識別子（redaction policy 適用済み。A-1 では plain string）。
    pub module_id: String,
    /// 行番号。
    pub line: u32,
}

/// 監査用の構造化 error（§7.2 `AuditError`）。型のみ。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditError {
    /// 安定 error code（最大 128 UTF-8 bytes）。
    pub code: String,
    /// message 識別子（最大 128 UTF-8 bytes）。
    pub message_id: String,
    /// module 識別子（redaction 適用済み）。
    pub module_id: Option<String>,
    /// 行番号。
    pub line: Option<u32>,
    /// 列番号。
    pub column: Option<u32>,
    /// trace（最大 32 frames）。
    pub trace: Vec<AuditFrame>,
    /// 省略した trace frame 数。
    pub omitted_trace_frames: u32,
}

/// Terminal event の error payload（§7.2 `AuditErrorPayload`）。
///
/// A-1 の error terminal では secret-free な [`AuditErrorPayload::Full`] を使う。`Emergency`
/// 置換（60 KiB 超過時）は strict CBOR が入る後続スライスで実配線する（型は用意する）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditErrorPayload {
    /// 完全な構造化 error。
    Full(AuditError),
    /// 60 KiB 超過時の縮退形（§7.2）。A-1 では構築しない。
    Emergency {
        /// 安定 error code（最大 128 UTF-8 bytes）。
        code: String,
        /// message 識別子（最大 128 UTF-8 bytes）。
        message_id: String,
        /// redaction 済み full error の byte 長。
        full_error_bytes: u64,
        /// redaction 済み full error の SHA-256。
        full_error_sha256: [u8; 32],
    },
}

/// 監査 event（§7.2）。全8 variant を型として持ち、A-1 で emission するのは
/// `ExecutionStarted` と `Terminal` だけ。他6 variant は型のみ（構築配線なし）。
#[derive(Clone, Debug, PartialEq)]
pub enum AuditEvent {
    /// 実行開始（§7.2、§8 規則1: sequence 0 に 1 件）。A-1 で emission する。
    ExecutionStarted {
        /// engine version 文字列。
        engine_version: String,
        /// 実行 backend（production は Tree）。
        backend: Backend,
        /// determinism rules revision。
        rules_revision: u32,
        /// heap accounting revision。
        heap_accounting_revision: u32,
        /// この実行の budget config。
        budget: BudgetConfig,
        /// capability policy の hash。
        capability_policy_hash: [u8; 32],
        /// redaction policy の識別子。
        redaction_policy_id: String,
        /// 実行モード（A-1 は `Live`）。
        mode: ExecutionMode,
    },
    /// capability 判定（§7.2）。型のみ（A-1 では emission しない）。
    CapabilityDecision {
        /// operation ID。
        operation_id: u64,
        /// call ID（module/path decision は `None` を許す）。
        call_id: Option<u64>,
        /// capability 名。
        capability: String,
        /// action。
        action: String,
        /// 対象 resource。
        resource: AuditPayload,
        /// 判定。
        decision: CapabilityDecisionKind,
        /// rule ID。
        rule_id: String,
    },
    /// host call 開始（§7.2）。型のみ。
    HostCallStarted {
        /// operation ID。
        operation_id: u64,
        /// call ID。
        call_id: u64,
        /// host function 名。
        function: String,
        /// request payload。
        request: AuditPayload,
        /// request byte 数。
        request_bytes: u64,
    },
    /// host call 完了（§7.2）。型のみ。
    HostCallFinished {
        /// operation ID。
        operation_id: u64,
        /// call ID。
        call_id: u64,
        /// 結果種別。
        outcome: HostCallOutcome,
        /// response payload。
        response: AuditPayload,
        /// response byte 数。
        response_bytes: u64,
        /// error code（あれば）。
        error_code: Option<String>,
        /// effect 状態。
        effect_status: EffectStatus,
    },
    /// budget charge の集約（§7.2/§7.3）。型のみ。
    BudgetCharged {
        /// charge した resource。
        resource: BudgetResource,
        /// commit した合計 delta。
        committed_delta: u64,
        /// release した合計 delta。
        released_delta: u64,
        /// flush 後の累積値。
        usage_after: u64,
        /// flush 後の peak（live resource のみ）。
        peak_after: Option<u64>,
        /// 集約した論理 charge 回数。
        charge_count: u64,
        /// charge 理由。
        reason: BudgetChargeReason,
    },
    /// yield（§7.2）。型のみ。
    Yielded {
        /// yield 理由。
        reason: AuditYieldReason,
        /// poll index。
        poll_index: u64,
        /// 時点の usage。
        usage: BudgetUsage,
    },
    /// resume（§7.2）。型のみ。
    Resumed {
        /// 直前の yield 理由。
        previous_reason: AuditYieldReason,
        /// poll index。
        poll_index: u64,
        /// 時点の usage。
        usage: BudgetUsage,
    },
    /// terminal（§7.2、§8 規則9: 最後の event で 1 件のみ）。A-1 で emission する。
    Terminal {
        /// terminal outcome。
        outcome: TerminalOutcome,
        /// error payload（error terminal のみ）。
        error: Option<AuditErrorPayload>,
        /// terminal 時点の予算使用量。
        usage: BudgetUsage,
        /// import graph の hash。
        import_graph_hash: Option<[u8; 32]>,
        /// language-state を commit したか（§13 AUD-024）。
        context_committed: bool,
        /// host effect が残り得るか（A-1 は常に false）。
        host_effects_may_remain: bool,
    },
}

/// terminal outcome（§7.2）。`ExecutionOutcome` と 1:1 対応する全 variant を持つ。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalOutcome {
    /// 正常完了。
    Completed,
    /// `exit(code)` で終了。
    Exited(u8),
    /// capability により拒否（A-1 では到達しない）。
    Denied,
    /// link error（A-1 では到達しない）。
    LinkError,
    /// 未捕捉 runtime error。
    RuntimeError,
    /// host error（A-1 では到達しない）。
    HostError,
    /// 予算超過（超過 resource を同梱）。
    BudgetExceeded(BudgetResource),
    /// deadline 超過。
    DeadlineExceeded,
    /// 協調 cancel。
    Cancelled,
    /// 監査自体の失敗（fail-closed、§10.1）。
    AuditFailure,
    /// 内部障害。
    InternalFailure,
    /// record 失敗（§6、A-1 範囲外）。
    RecordFailure,
    /// replay mismatch（§6、A-1 範囲外）。
    ReplayMismatch,
}

/// `ExecutionOutcome` を §7.2 の対応表どおり [`TerminalOutcome`] へ写す（1:1）。
///
/// `BudgetExceeded` の resource は、まず `exceeded_resource` 引数（engine が畳む前に退避した
/// 原本。§7.2 の細粒度 resource）を使う。`None`（原本を取れない）ときだけ
/// `ExecutionError` の `ErrorKind` から [`budget_resource_from_error_kind`] で粗く復元し、
/// それも `None` なら仕様の既定優先 resource である `Fuel` を使う（§7.2 の 1:1 を total に保つ
/// 明示フォールバック）。監査経路では原本を渡すので、`control_stop_to_error` の縮約で失われる
/// String/Source/I-O 系の細分 resource も正しく載る。
pub fn terminal_outcome_from(
    outcome: &ExecutionOutcome,
    exceeded_resource: Option<BudgetResource>,
) -> TerminalOutcome {
    match outcome {
        ExecutionOutcome::Completed { .. } => TerminalOutcome::Completed,
        ExecutionOutcome::Exited { code, .. } => TerminalOutcome::Exited(*code),
        ExecutionOutcome::RuntimeError { .. } => TerminalOutcome::RuntimeError,
        ExecutionOutcome::BudgetExceeded { error, .. } => {
            let resource = exceeded_resource
                .or_else(|| budget_resource_from_error_kind(error.code))
                .unwrap_or(BudgetResource::Fuel);
            TerminalOutcome::BudgetExceeded(resource)
        }
        ExecutionOutcome::DeadlineExceeded { .. } => TerminalOutcome::DeadlineExceeded,
        ExecutionOutcome::Cancelled { .. } => TerminalOutcome::Cancelled,
        ExecutionOutcome::InternalFailure { .. } => TerminalOutcome::InternalFailure,
    }
}

/// `BudgetExceeded` terminal の `ErrorKind` を [`BudgetResource`] へ訳す（§7.2 / D5）。
///
/// この写像は total ではなく、かつ **粗い**。`control_stop_to_error` が複数の細粒度 resource を
/// 1 つの `ErrorKind` へ縮約するため、ここで得られるのは各 family の代表値だけである（例:
/// `StringLimit → StringBytes`）。原本の細粒度 resource が必要な監査経路は
/// [`terminal_outcome_from`] の `exceeded_resource` 引数で原本を渡す。予算超過を表さない
/// `ErrorKind` は `None` を返す。
pub fn budget_resource_from_error_kind(kind: crate::error::ErrorKind) -> Option<BudgetResource> {
    use crate::error::ErrorKind;
    match kind {
        ErrorKind::StepLimit => Some(BudgetResource::Fuel),
        ErrorKind::CollectionLimit => Some(BudgetResource::CollectionElements),
        ErrorKind::StringLimit => Some(BudgetResource::StringBytes),
        ErrorKind::SourceLimit => Some(BudgetResource::SourceBytes),
        ErrorKind::HeapLimit => Some(BudgetResource::HeapBytes),
        ErrorKind::IoLimit => Some(BudgetResource::InputCalls),
        _ => None,
    }
}

// =============================================================================
// §11 監査自体の予算（AuditBudget）と config 検証
// =============================================================================

/// 監査 control-plane の予算（§11）。script の fuel/output/host-call budget へは課金しない。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuditBudget {
    /// 最大 event 数（既定 100,000、実効 4 以上。§11）。
    pub max_events: u64,
    /// 最大 encoded byte 数（既定 16 MiB）。
    pub max_encoded_bytes: u64,
    /// execution ごとの最大 pending byte 数（既定 1 MiB、A-1 では消費しない）。
    pub max_pending_bytes: u64,
    /// Terminal 専用に予約する event 数（fixed 1）。
    pub terminal_reserve_events: u64,
    /// Terminal 専用に予約する byte 数（fixed 64 KiB）。
    pub terminal_reserve_bytes: u64,
    /// host-call close 予約 event 数（fixed 2、A-1 では消費しない）。
    pub host_call_close_reserve_events: u64,
    /// host-call close 予約 byte 数（fixed 128 KiB、A-1 では消費しない）。
    pub host_call_close_reserve_bytes: u64,
}

impl Default for AuditBudget {
    fn default() -> Self {
        Self {
            max_events: 100_000,
            max_encoded_bytes: 16 * MIB,
            max_pending_bytes: MIB,
            terminal_reserve_events: 1,
            terminal_reserve_bytes: 64 * KIB,
            host_call_close_reserve_events: 2,
            host_call_close_reserve_bytes: 128 * KIB,
        }
    }
}

/// `AuditBudget` の config 検証エラー（§11）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditConfigError {
    /// `max_events < 2`。
    MaxEventsTooSmall {
        /// 設定値。
        max_events: u64,
    },
    /// `terminal_reserve_events != 1`。
    TerminalReserveEventsNotOne {
        /// 設定値。
        terminal_reserve_events: u64,
    },
    /// `terminal_reserve_bytes < 64 KiB`。
    TerminalReserveBytesTooSmall {
        /// 設定値。
        terminal_reserve_bytes: u64,
    },
    /// `host_call_close_reserve_events != 2`。
    HostCallCloseReserveEventsNotTwo {
        /// 設定値。
        host_call_close_reserve_events: u64,
    },
    /// `host_call_close_reserve_bytes < 128 KiB`。
    HostCallCloseReserveBytesTooSmall {
        /// 設定値。
        host_call_close_reserve_bytes: u64,
    },
}

impl AuditBudget {
    /// §11 の config 不変条件を検証する。execution 作成前に呼ぶ。
    pub fn validate(&self) -> Result<(), AuditConfigError> {
        if self.max_events < 2 {
            return Err(AuditConfigError::MaxEventsTooSmall {
                max_events: self.max_events,
            });
        }
        if self.terminal_reserve_events != 1 {
            return Err(AuditConfigError::TerminalReserveEventsNotOne {
                terminal_reserve_events: self.terminal_reserve_events,
            });
        }
        if self.terminal_reserve_bytes < 64 * KIB {
            return Err(AuditConfigError::TerminalReserveBytesTooSmall {
                terminal_reserve_bytes: self.terminal_reserve_bytes,
            });
        }
        // host-call reserve は A-1 で消費しないが、型の自己整合性のため §11 の固定値を検証する。
        if self.host_call_close_reserve_events != 2 {
            return Err(AuditConfigError::HostCallCloseReserveEventsNotTwo {
                host_call_close_reserve_events: self.host_call_close_reserve_events,
            });
        }
        if self.host_call_close_reserve_bytes < 128 * KIB {
            return Err(AuditConfigError::HostCallCloseReserveBytesTooSmall {
                host_call_close_reserve_bytes: self.host_call_close_reserve_bytes,
            });
        }
        Ok(())
    }
}

// =============================================================================
// D4 encoded_bytes の naive 推定（§9.2 strict CBOR は後続スライスで差し替え）
// =============================================================================

/// envelope の encoded byte 数の naive 推定（D4）。
///
/// A-1 では §9.2 の deterministic CBOR を実装しないため、journal の byte 予算が課金できる概算を
/// 返す。call site を 1 関数へ隔離し、後続スライスで §9.2 encoder へ差し替えられるようにする。
/// 推定は保守的（固定 overhead + 可変 field の byte 長）で、厳密な CBOR 長とは一致しない。
pub fn estimate_encoded_bytes(envelope: &AuditEnvelope) -> u64 {
    // envelope 固定部: schema_version(2) + execution_id(16) + source_hash(32) + sequence(8)
    // + timestamp(16) ＝ 74 bytes 程度。language_revision は可変。
    let mut bytes: u64 = 74;
    bytes = bytes.saturating_add(envelope.language_revision.len() as u64);
    bytes = bytes.saturating_add(estimate_event_bytes(&envelope.event));
    bytes
}

/// event 本体の naive 推定。
fn estimate_event_bytes(event: &AuditEvent) -> u64 {
    match event {
        AuditEvent::ExecutionStarted {
            engine_version,
            redaction_policy_id,
            ..
        } => {
            // 固定 field（backend/revision/budget/hash/mode）＋可変 string を概算。
            let fixed: u64 = 256;
            fixed
                .saturating_add(engine_version.len() as u64)
                .saturating_add(redaction_policy_id.len() as u64)
        }
        AuditEvent::Terminal { error, .. } => {
            let fixed: u64 = 128;
            let err_bytes = match error {
                None => 0,
                Some(AuditErrorPayload::Full(e)) => {
                    (e.code.len()
                        + e.message_id.len()
                        + e.trace.iter().map(|f| f.function.len() + 16).sum::<usize>())
                        as u64
                }
                Some(AuditErrorPayload::Emergency {
                    code, message_id, ..
                }) => (code.len() + message_id.len() + 40) as u64,
            };
            fixed.saturating_add(err_bytes)
        }
        // A-1 では emission しない event。概算のみ（journal には積まれない）。
        _ => 128,
    }
}

// =============================================================================
// §10.1 journal（per-execution bounded logical journal）
// =============================================================================

/// 監査 journal の失敗（§10.1/§11）。fail-closed の信号で、`AuditFailure` terminal に対応する。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditFailure {
    /// sequence が u64 を使い切った（§7.1、wrap せず fail-closed）。
    SequenceExhausted,
    /// 通常 event の event-count / byte 予算を超過した（§11）。
    BudgetExceeded,
    /// 単一 event が最大 encoded 長を超えた（§7.2 60 KiB / §11）。
    EventTooLarge,
    /// sink が永続 error / protocol error を返した（§10）。
    Sink,
}

/// per-execution の bounded logical journal（§8/§10.1/§11）。
///
/// `sequence` は 0 から始まり checked increment する。Started を append すると Terminal 専用の
/// event slot(1) と byte slot(64 KiB) を予約し、通常 event はこれを使えない。Terminal は予約
/// slot を使うため通常上限を使い切っても必ず append できる（§11）。
pub struct AuditJournal {
    budget: AuditBudget,
    /// 次に割り当てる sequence（Started で 0）。
    next_sequence: u64,
    /// commit 済み通常 event 数（Terminal を含まない）。
    committed_events: u64,
    /// commit 済み通常 encoded byte 合計（Terminal を含まない）。
    committed_bytes: u64,
    /// Started を append したか（Terminal 予約が有効か）。
    started: bool,
    /// Terminal を append したか（以後 append 禁止、§8 規則9）。
    terminated: bool,
}

impl AuditJournal {
    /// 検証済み budget から空の journal を開く。
    ///
    /// `budget` は呼び出し前に [`AuditBudget::validate`] を通していること。
    pub fn open(budget: AuditBudget) -> Self {
        Self {
            budget,
            next_sequence: 0,
            committed_events: 0,
            committed_bytes: 0,
            started: false,
            terminated: false,
        }
    }

    /// 次に割り当てられる sequence（主にテスト・診断用）。
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Started を append したか。
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Terminal を append したか。
    pub fn is_terminated(&self) -> bool {
        self.terminated
    }

    /// checked increment で次の sequence を払い出す（overflow は fail-closed、§7.1）。
    fn take_sequence(&mut self) -> Result<u64, AuditFailure> {
        let seq = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(AuditFailure::SequenceExhausted)?;
        Ok(seq)
    }

    /// 通常 event を append し、割り当てた sequence を返す（§8/§11）。
    ///
    /// - Terminal 後は append しない（§8 規則9）。
    /// - sequence は checked increment（overflow は `SequenceExhausted`）。
    /// - 単一 event の encoded 長が通常上限（`terminal_reserve_bytes` を除いた分）を超えると
    ///   `EventTooLarge`。
    /// - event 数が `max_events - terminal_reserve_events` を超える、または累積 byte が通常
    ///   上限を超えると `BudgetExceeded`。
    ///
    /// `ExecutionStarted` を初回に append すると Started フラグを立て、以後 Terminal 予約が
    /// 有効になる（§8 規則1: Started は sequence 0 に 1 件）。
    pub fn append_normal(&mut self, envelope: &AuditEnvelope) -> Result<u64, AuditFailure> {
        if self.terminated {
            // Terminal 後はいかなる event も発行しない（§8 規則9）。
            return Err(AuditFailure::BudgetExceeded);
        }

        let is_started_event = matches!(envelope.event, AuditEvent::ExecutionStarted { .. });
        if is_started_event {
            // Started は sequence 0 に 1 件だけ（§8 規則1）。
            if self.started || self.next_sequence != 0 {
                return Err(AuditFailure::BudgetExceeded);
            }
        } else {
            // Started より前に通常 event は来ない（A-1 では Started 以外の通常 event はないが、
            // 不変条件を守る）。
            if !self.started {
                return Err(AuditFailure::BudgetExceeded);
            }
        }

        let event_bytes = estimate_encoded_bytes(envelope);

        // 通常 event が使える event-count 上限（Terminal 予約を除く）。
        let normal_event_limit = self
            .budget
            .max_events
            .saturating_sub(self.budget.terminal_reserve_events);
        // 通常 event が使える byte 上限（Terminal 予約を除く）。
        let normal_byte_limit = self
            .budget
            .max_encoded_bytes
            .saturating_sub(self.budget.terminal_reserve_bytes);

        // 単一 event が通常 byte 上限そのものを超える場合は EventTooLarge。
        if event_bytes > normal_byte_limit {
            return Err(AuditFailure::EventTooLarge);
        }

        let projected_events = self
            .committed_events
            .checked_add(1)
            .ok_or(AuditFailure::BudgetExceeded)?;
        if projected_events > normal_event_limit {
            return Err(AuditFailure::BudgetExceeded);
        }
        let projected_bytes = self
            .committed_bytes
            .checked_add(event_bytes)
            .ok_or(AuditFailure::BudgetExceeded)?;
        if projected_bytes > normal_byte_limit {
            return Err(AuditFailure::BudgetExceeded);
        }

        // checked increment で sequence を払い出す（commit 直前）。
        let seq = self.take_sequence()?;
        self.committed_events = projected_events;
        self.committed_bytes = projected_bytes;
        if is_started_event {
            self.started = true;
        }
        Ok(seq)
    }

    /// Terminal を予約 slot を使って append し、割り当てた sequence を返す（§8 規則9/10）。
    ///
    /// Started 済みであれば通常上限を使い切っていても必ず成功する（Terminal 専用予約を使う）。
    /// Started 前・Terminal 後の呼び出しは不変条件違反で `AuditFailure` を返す。sequence の
    /// checked increment の overflow だけは fail-closed にする。
    pub fn append_terminal(&mut self, envelope: &AuditEnvelope) -> Result<u64, AuditFailure> {
        debug_assert!(matches!(envelope.event, AuditEvent::Terminal { .. }));
        if !self.started {
            // Started の無い execution に Terminal は無い（§8）。
            return Err(AuditFailure::BudgetExceeded);
        }
        if self.terminated {
            // Terminal は 1 件のみ（§8 規則9/10）。
            return Err(AuditFailure::BudgetExceeded);
        }
        // Terminal は専用予約を使うので通常 byte/event 残量は検査しない（§11）。
        // sequence の overflow だけは fail-closed（§7.1）。
        let seq = self.take_sequence()?;
        self.terminated = true;
        Ok(seq)
    }
}

// =============================================================================
// §10 sink 契約
// =============================================================================

/// sink の `submit` 結果（§10）。
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AuditSubmit {
    /// batch を ack した（through_sequence まで確定）。
    Ack(AuditAck),
    /// backpressure により pending（§10.2、A-1 では同期 sink が返さない）。
    Pending(AuditTicket),
    /// 永続 error / protocol error（§10.1、fail-closed）。
    Failed(AuditSinkError),
}

/// sink の ack（§10）。連続 sequence だけを認める。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuditAck {
    /// 対象 execution ID。
    pub execution_id: ExecutionId,
    /// ack した末尾 sequence。
    pub through_sequence: u64,
}

/// sink error（§10）。A-1 では type-only の最小形。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AuditSinkError {
    /// 永続的な配送失敗。
    Permanent,
    /// protocol 違反（gap / 別 execution / 未送信 sequence の ack 等）。
    Protocol,
    /// sink callback 中の再入（§10、A-1 では検出配線なし）。
    Reentrant,
}

/// backpressure 時の wake ハンドル（§10）。A-1 では型のみ（`wake` は no-op）。
#[derive(Clone)]
pub struct AuditWaker {
    _inner: Arc<()>,
}

impl AuditWaker {
    /// A-1 の同期経路用の no-op waker を作る。
    pub fn noop() -> Self {
        Self {
            _inner: Arc::new(()),
        }
    }

    /// pending の sink を wake する（§10.2）。A-1 では no-op。
    pub fn wake(&self) {}
}

impl std::fmt::Debug for AuditWaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditWaker").finish_non_exhaustive()
    }
}

/// pending batch の ticket（§10）。A-1 では型のみ（同期 sink は Pending を返さない）。
#[derive(Clone)]
pub struct AuditTicket {
    _inner: Arc<Mutex<Option<Result<AuditAck, AuditSinkError>>>>,
}

impl AuditTicket {
    /// 空の ticket を作る（A-1 では使われない）。
    pub fn new() -> Self {
        Self {
            _inner: Arc::new(Mutex::new(None)),
        }
    }

    /// ack/error 格納時に wake する waker を登録する（§10）。A-1 では no-op。
    pub fn register_waker(&self, _waker: AuditWaker) {}

    /// 格納済みの ack/error を取り出す（§10）。A-1 では常に `None`。
    pub fn try_take(&self) -> Option<Result<AuditAck, AuditSinkError>> {
        self._inner.lock().expect("audit ticket poisoned").take()
    }

    /// ticket を取り消す（§10）。A-1 では no-op。
    pub fn cancel(&self) {}
}

impl Default for AuditTicket {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for AuditTicket {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self._inner, &other._inner)
    }
}

impl Eq for AuditTicket {}

impl std::fmt::Debug for AuditTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditTicket").finish_non_exhaustive()
    }
}

/// 監査 sink（§10）。batch を submit し、ack/pending/failed を返す。`Send + Sync`。
pub trait AuditSink: Send + Sync {
    /// batch（1 execution 内の順序付き envelope 列）を submit する（§10）。
    fn submit(&self, batch: Arc<[AuditEnvelope]>, waker: AuditWaker) -> AuditSubmit;
}

/// 常に `Ack` を返す同期 in-process sink（§10）。A-1 の最小実装。
///
/// batch 末尾の sequence を through_sequence として ack する。envelope を保持しない
/// （記録・検査が必要なら [`InMemoryAuditSink`] を使う）。
#[derive(Debug, Default)]
pub struct SyncAuditSink;

impl SyncAuditSink {
    /// 新しい同期 sink を作る。
    pub fn new() -> Self {
        Self
    }
}

impl AuditSink for SyncAuditSink {
    fn submit(&self, batch: Arc<[AuditEnvelope]>, _waker: AuditWaker) -> AuditSubmit {
        match batch.last() {
            Some(last) => AuditSubmit::Ack(AuditAck {
                execution_id: last.execution_id,
                through_sequence: last.sequence,
            }),
            // 空 batch は protocol error（連続 sequence を認められない）。
            None => AuditSubmit::Failed(AuditSinkError::Protocol),
        }
    }
}

/// envelope を収集する in-memory test sink（§10）。`Mutex<Vec<_>>` で `Send + Sync`。
///
/// `fail_mode` を立てると `submit` が `Failed` を返し、fail-closed 経路を検証できる。
pub struct InMemoryAuditSink {
    envelopes: Mutex<Vec<AuditEnvelope>>,
    /// true の間 `submit` は常に `Failed` を返す（fail-closed テスト用）。
    fail: Mutex<bool>,
}

impl InMemoryAuditSink {
    /// 空の収集 sink を作る。
    pub fn new() -> Self {
        Self {
            envelopes: Mutex::new(Vec::new()),
            fail: Mutex::new(false),
        }
    }

    /// 最初の submit から常に `Failed` を返す sink を作る（fail-closed テスト用）。
    pub fn failing() -> Self {
        Self {
            envelopes: Mutex::new(Vec::new()),
            fail: Mutex::new(true),
        }
    }

    /// 収集済み envelope の snapshot を返す。
    pub fn snapshot(&self) -> Vec<AuditEnvelope> {
        self.envelopes.lock().expect("audit sink poisoned").clone()
    }

    /// これまでに収集した envelope 数。
    pub fn len(&self) -> usize {
        self.envelopes.lock().expect("audit sink poisoned").len()
    }

    /// 収集済み envelope が空か。
    pub fn is_empty(&self) -> bool {
        self.envelopes
            .lock()
            .expect("audit sink poisoned")
            .is_empty()
    }
}

impl Default for InMemoryAuditSink {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditSink for InMemoryAuditSink {
    fn submit(&self, batch: Arc<[AuditEnvelope]>, _waker: AuditWaker) -> AuditSubmit {
        if *self.fail.lock().expect("audit sink poisoned") {
            return AuditSubmit::Failed(AuditSinkError::Permanent);
        }
        let last = match batch.last() {
            Some(last) => (last.execution_id, last.sequence),
            None => return AuditSubmit::Failed(AuditSinkError::Protocol),
        };
        self.envelopes
            .lock()
            .expect("audit sink poisoned")
            .extend(batch.iter().cloned());
        AuditSubmit::Ack(AuditAck {
            execution_id: last.0,
            through_sequence: last.1,
        })
    }
}

/// submit ごとに応答を切り替えられるテスト用 sink（§10 の ack 規則・fail-closed 検証用）。
///
/// `plan` の先頭から 1 submit につき 1 つ [`ScriptedResponse`] を消費する。`plan` を使い切った
/// 以降は末尾の挙動（既定は `Ack`）を繰り返す。received envelope は記録する。
///
/// 用途:
/// - Started を Ack し Terminal を Fail させる（二度目失敗の fail-closed）。
/// - Started に別 execution の ack / 不正 sequence を返す（§10 protocol 違反）。
pub struct ScriptedAuditSink {
    plan: Mutex<std::collections::VecDeque<ScriptedResponse>>,
    tail: ScriptedResponse,
    envelopes: Mutex<Vec<AuditEnvelope>>,
}

/// [`ScriptedAuditSink`] の 1 submit あたりの応答指示。
#[derive(Clone, Copy, Debug)]
pub enum ScriptedResponse {
    /// 正しい（batch 末尾の execution_id / sequence を指す）Ack を返す。
    AckCorrect,
    /// `Failed(Permanent)` を返す。
    Fail,
    /// batch とは別の execution_id で Ack を返す（protocol 違反）。
    AckWrongId,
    /// batch 末尾 +1 の sequence で Ack を返す（未送信 sequence の ack、protocol 違反）。
    AckWrongSequence,
}

impl ScriptedAuditSink {
    /// 応答計画を与えて作る。計画消費後は `AckCorrect` を繰り返す。
    pub fn new(plan: impl IntoIterator<Item = ScriptedResponse>) -> Self {
        Self {
            plan: Mutex::new(plan.into_iter().collect()),
            tail: ScriptedResponse::AckCorrect,
            envelopes: Mutex::new(Vec::new()),
        }
    }

    /// 収集済み envelope の snapshot。
    pub fn snapshot(&self) -> Vec<AuditEnvelope> {
        self.envelopes.lock().expect("audit sink poisoned").clone()
    }

    /// 収集済み envelope 数。
    pub fn len(&self) -> usize {
        self.envelopes.lock().expect("audit sink poisoned").len()
    }

    /// 収集済み envelope が空か。
    pub fn is_empty(&self) -> bool {
        self.envelopes
            .lock()
            .expect("audit sink poisoned")
            .is_empty()
    }
}

impl AuditSink for ScriptedAuditSink {
    fn submit(&self, batch: Arc<[AuditEnvelope]>, _waker: AuditWaker) -> AuditSubmit {
        let response = self
            .plan
            .lock()
            .expect("audit sink poisoned")
            .pop_front()
            .unwrap_or(self.tail);
        let last = match batch.last() {
            Some(last) => (last.execution_id, last.sequence),
            None => return AuditSubmit::Failed(AuditSinkError::Protocol),
        };
        // 記録は Fail 以外で行う（Fail は配送なしとして扱い、何も残さない）。
        if !matches!(response, ScriptedResponse::Fail) {
            self.envelopes
                .lock()
                .expect("audit sink poisoned")
                .extend(batch.iter().cloned());
        }
        match response {
            ScriptedResponse::Fail => AuditSubmit::Failed(AuditSinkError::Permanent),
            ScriptedResponse::AckCorrect => AuditSubmit::Ack(AuditAck {
                execution_id: last.0,
                through_sequence: last.1,
            }),
            ScriptedResponse::AckWrongId => AuditSubmit::Ack(AuditAck {
                // 必ず batch の id と異なる値（nonzero 保証のため wrapping_add(1) を使う）。
                execution_id: wrong_execution_id(last.0),
                through_sequence: last.1,
            }),
            ScriptedResponse::AckWrongSequence => AuditSubmit::Ack(AuditAck {
                execution_id: last.0,
                through_sequence: last.1.wrapping_add(1),
            }),
        }
    }
}

/// batch の execution_id と必ず異なる id を作る（protocol 違反テスト用）。
fn wrong_execution_id(id: ExecutionId) -> ExecutionId {
    let raw = id.get().get();
    // 1 を足して衝突しない別値にする（u128::MAX のときは 1 へ回り込ませて 0 を避ける）。
    let bumped = raw.wrapping_add(1);
    let nonzero = std::num::NonZeroU128::new(bumped)
        .unwrap_or(std::num::NonZeroU128::new(1).expect("1 is nonzero"));
    ExecutionId::new(nonzero)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetUsage, FakeClock};
    use crate::error::ErrorKind;
    use std::num::NonZeroU128;

    fn exec_id(n: u128) -> ExecutionId {
        ExecutionId::new(NonZeroU128::new(n).expect("nonzero"))
    }

    fn standard_budget() -> BudgetConfig {
        let clock = FakeClock::new();
        BudgetConfig::standard(&clock).expect("standard budget")
    }

    fn started_envelope(id: ExecutionId, sequence: u64) -> AuditEnvelope {
        AuditEnvelope {
            schema_version: 1,
            execution_id: id,
            source_hash: [0u8; 32],
            language_revision: "0.20".to_string(),
            sequence,
            timestamp: HostTimestamp {
                unix_nanoseconds: 0,
            },
            event: AuditEvent::ExecutionStarted {
                engine_version: "0.1.0".to_string(),
                backend: Backend::TreeWalk,
                rules_revision: 1,
                heap_accounting_revision: 1,
                budget: standard_budget(),
                capability_policy_hash: [0u8; 32],
                redaction_policy_id: "default".to_string(),
                mode: ExecutionMode::Live,
            },
        }
    }

    fn terminal_envelope(
        id: ExecutionId,
        sequence: u64,
        outcome: TerminalOutcome,
    ) -> AuditEnvelope {
        AuditEnvelope {
            schema_version: 1,
            execution_id: id,
            source_hash: [0u8; 32],
            language_revision: "0.20".to_string(),
            sequence,
            timestamp: HostTimestamp {
                unix_nanoseconds: 0,
            },
            event: AuditEvent::Terminal {
                outcome,
                error: None,
                usage: BudgetUsage::default(),
                import_graph_hash: None,
                context_committed: true,
                host_effects_may_remain: false,
            },
        }
    }

    // --- (a) schema / mapping: ExecutionOutcome -> TerminalOutcome の 1:1 ---

    #[test]
    fn terminal_outcome_maps_every_execution_outcome_variant() {
        let usage = BudgetUsage::default();
        assert_eq!(
            terminal_outcome_from(&ExecutionOutcome::Completed { usage }, None),
            TerminalOutcome::Completed
        );
        assert_eq!(
            terminal_outcome_from(&ExecutionOutcome::Exited { code: 7, usage }, None),
            TerminalOutcome::Exited(7)
        );
        assert_eq!(
            terminal_outcome_from(
                &ExecutionOutcome::RuntimeError {
                    error: exec_error(ErrorKind::Name),
                    usage,
                },
                None
            ),
            TerminalOutcome::RuntimeError
        );
        // 原本 resource が無いときは ErrorKind から粗く復元する（family 代表値）。
        assert_eq!(
            terminal_outcome_from(
                &ExecutionOutcome::BudgetExceeded {
                    error: exec_error(ErrorKind::StepLimit),
                    usage,
                },
                None
            ),
            TerminalOutcome::BudgetExceeded(BudgetResource::Fuel)
        );
        assert_eq!(
            terminal_outcome_from(
                &ExecutionOutcome::BudgetExceeded {
                    error: exec_error(ErrorKind::HeapLimit),
                    usage,
                },
                None
            ),
            TerminalOutcome::BudgetExceeded(BudgetResource::HeapBytes)
        );
        assert_eq!(
            terminal_outcome_from(&ExecutionOutcome::DeadlineExceeded { usage }, None),
            TerminalOutcome::DeadlineExceeded
        );
        assert_eq!(
            terminal_outcome_from(&ExecutionOutcome::Cancelled { usage }, None),
            TerminalOutcome::Cancelled
        );
        assert_eq!(
            terminal_outcome_from(
                &ExecutionOutcome::InternalFailure {
                    fault_id: 1,
                    safe_message: "x".to_string(),
                    usage,
                },
                None
            ),
            TerminalOutcome::InternalFailure
        );
    }

    #[test]
    fn terminal_outcome_prefers_original_budget_resource() {
        // control_stop_to_error が ErrorKind::IoLimit へ畳む細分 resource（例 OutputBytes）は、
        // 原本を渡せば §7.2 どおり正しく載る。ErrorKind だけなら代表値 InputCalls に化ける。
        let usage = BudgetUsage::default();
        let outcome = ExecutionOutcome::BudgetExceeded {
            error: exec_error(ErrorKind::IoLimit),
            usage,
        };
        assert_eq!(
            terminal_outcome_from(&outcome, Some(BudgetResource::OutputBytes)),
            TerminalOutcome::BudgetExceeded(BudgetResource::OutputBytes)
        );
        // String family: 原本 StringAllocations は ErrorKind だけだと StringBytes になる。
        let outcome = ExecutionOutcome::BudgetExceeded {
            error: exec_error(ErrorKind::StringLimit),
            usage,
        };
        assert_eq!(
            terminal_outcome_from(&outcome, Some(BudgetResource::StringAllocations)),
            TerminalOutcome::BudgetExceeded(BudgetResource::StringAllocations)
        );
        // Source family: 原本 ImportBytes は ErrorKind だけだと SourceBytes になる。
        let outcome = ExecutionOutcome::BudgetExceeded {
            error: exec_error(ErrorKind::SourceLimit),
            usage,
        };
        assert_eq!(
            terminal_outcome_from(&outcome, Some(BudgetResource::ImportBytes)),
            TerminalOutcome::BudgetExceeded(BudgetResource::ImportBytes)
        );
    }

    #[test]
    fn budget_resource_mapping_handles_unmapped_kind() {
        // 予算超過を表さない ErrorKind は None（total でないことを明示）。
        assert_eq!(budget_resource_from_error_kind(ErrorKind::Name), None);
        assert_eq!(
            budget_resource_from_error_kind(ErrorKind::StepLimit),
            Some(BudgetResource::Fuel)
        );
    }

    fn exec_error(kind: ErrorKind) -> crate::embedding::ExecutionError {
        crate::embedding::ExecutionError {
            code: kind,
            safe_message: "msg".to_string(),
            line: None,
            trace: Vec::new(),
        }
    }

    // --- (e) AuditBudget config 検証 ---

    #[test]
    fn audit_budget_default_is_valid() {
        assert_eq!(AuditBudget::default().validate(), Ok(()));
    }

    #[test]
    fn audit_budget_rejects_small_max_events() {
        let b = AuditBudget {
            max_events: 1,
            ..AuditBudget::default()
        };
        assert_eq!(
            b.validate(),
            Err(AuditConfigError::MaxEventsTooSmall { max_events: 1 })
        );
    }

    #[test]
    fn audit_budget_rejects_bad_terminal_reserve_events() {
        let b = AuditBudget {
            terminal_reserve_events: 0,
            ..AuditBudget::default()
        };
        assert_eq!(
            b.validate(),
            Err(AuditConfigError::TerminalReserveEventsNotOne {
                terminal_reserve_events: 0
            })
        );
    }

    #[test]
    fn audit_budget_rejects_small_terminal_reserve_bytes() {
        let b = AuditBudget {
            terminal_reserve_bytes: 64 * KIB - 1,
            ..AuditBudget::default()
        };
        assert_eq!(
            b.validate(),
            Err(AuditConfigError::TerminalReserveBytesTooSmall {
                terminal_reserve_bytes: 64 * KIB - 1
            })
        );
    }

    #[test]
    fn audit_budget_rejects_bad_host_call_reserve() {
        let b = AuditBudget {
            host_call_close_reserve_events: 1,
            ..AuditBudget::default()
        };
        assert_eq!(
            b.validate(),
            Err(AuditConfigError::HostCallCloseReserveEventsNotTwo {
                host_call_close_reserve_events: 1
            })
        );
        let b = AuditBudget {
            host_call_close_reserve_bytes: 128 * KIB - 1,
            ..AuditBudget::default()
        };
        assert_eq!(
            b.validate(),
            Err(AuditConfigError::HostCallCloseReserveBytesTooSmall {
                host_call_close_reserve_bytes: 128 * KIB - 1
            })
        );
    }

    // --- (b) journal 完全性 ---

    #[test]
    fn journal_started_is_sequence_zero_then_terminal_is_last() {
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        let started = started_envelope(id, 0);
        assert_eq!(journal.append_normal(&started), Ok(0));
        assert!(journal.is_started());
        let terminal = terminal_envelope(id, 1, TerminalOutcome::Completed);
        assert_eq!(journal.append_terminal(&terminal), Ok(1));
        assert!(journal.is_terminated());
    }

    #[test]
    fn journal_rejects_append_after_terminal() {
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        journal
            .append_terminal(&terminal_envelope(id, 1, TerminalOutcome::Completed))
            .unwrap();
        // Terminal 後の通常 append は拒否（§8 規則9）。
        assert_eq!(
            journal.append_normal(&started_envelope(id, 2)),
            Err(AuditFailure::BudgetExceeded)
        );
        // Terminal 後の 2 回目 Terminal も拒否。
        assert_eq!(
            journal.append_terminal(&terminal_envelope(id, 2, TerminalOutcome::Completed)),
            Err(AuditFailure::BudgetExceeded)
        );
    }

    #[test]
    fn journal_started_only_once_at_sequence_zero() {
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        // 2 回目の Started は拒否。
        assert_eq!(
            journal.append_normal(&started_envelope(id, 1)),
            Err(AuditFailure::BudgetExceeded)
        );
    }

    #[test]
    fn journal_terminal_requires_started() {
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        // Started 無しの Terminal は不変条件違反。
        assert_eq!(
            journal.append_terminal(&terminal_envelope(id, 0, TerminalOutcome::Completed)),
            Err(AuditFailure::BudgetExceeded)
        );
    }

    #[test]
    fn journal_sequence_overflow_fails_closed() {
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        // sequence を u64::MAX 手前へ進め、Terminal の checked increment overflow を観測する。
        journal.next_sequence = u64::MAX;
        assert_eq!(
            journal.append_terminal(&terminal_envelope(id, 0, TerminalOutcome::Completed)),
            Err(AuditFailure::SequenceExhausted)
        );
        // wrap していない（fail-closed）。
        assert!(!journal.is_terminated());
    }

    #[test]
    fn journal_terminal_appends_even_when_normal_budget_exhausted() {
        // 通常 event 上限を使い切っても Terminal は予約 slot で必ず append できる（§11）。
        let id = exec_id(1);
        // max_events=2（最小有効値 >=2）、terminal_reserve_events=1 → 通常 event は 1 件のみ。
        let budget = AuditBudget {
            max_events: 2,
            ..AuditBudget::default()
        };
        assert_eq!(budget.validate(), Ok(()));
        let mut journal = AuditJournal::open(budget);
        // Started（通常 event 1 件）で通常枠を使い切る。
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        // Terminal は予約 slot で成功する。
        assert_eq!(
            journal.append_terminal(&terminal_envelope(id, 1, TerminalOutcome::Completed)),
            Ok(1)
        );
    }

    // --- (c)/(d) sink の挙動 ---

    #[test]
    fn sync_sink_always_acks() {
        let id = exec_id(1);
        let sink = SyncAuditSink::new();
        let batch: Arc<[AuditEnvelope]> = Arc::from(vec![started_envelope(id, 0)]);
        match sink.submit(batch, AuditWaker::noop()) {
            AuditSubmit::Ack(ack) => {
                assert_eq!(ack.execution_id, id);
                assert_eq!(ack.through_sequence, 0);
            }
            other => panic!("期待 Ack, 実際 {other:?}"),
        }
    }

    #[test]
    fn in_memory_sink_records_and_can_fail() {
        let id = exec_id(1);
        let sink = InMemoryAuditSink::new();
        let batch: Arc<[AuditEnvelope]> = Arc::from(vec![
            started_envelope(id, 0),
            terminal_envelope(id, 1, TerminalOutcome::Completed),
        ]);
        assert!(matches!(
            sink.submit(batch, AuditWaker::noop()),
            AuditSubmit::Ack(_)
        ));
        assert_eq!(sink.len(), 2);

        let failing = InMemoryAuditSink::failing();
        let batch: Arc<[AuditEnvelope]> = Arc::from(vec![started_envelope(id, 0)]);
        assert!(matches!(
            failing.submit(batch, AuditWaker::noop()),
            AuditSubmit::Failed(_)
        ));
        assert!(failing.is_empty());
    }
}
