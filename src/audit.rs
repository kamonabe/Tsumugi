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

use std::collections::BTreeMap;
use std::collections::BTreeSet;
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

// =============================================================================
// §9.1 redaction field tree（R-1 実装スライス）
// =============================================================================

/// field class（§9.1）。redaction の既定モードをこの class から決める。
///
/// canonical CBOR では宣言順を 0 始まり u8 で encode する（§9.2）。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AuditFieldClass {
    /// event 種別・ID・hash・count・error code 等。既定 raw。
    Public,
    /// capability 名・host function 名・rule ID・module logical name 等。既定 raw。
    Identifier,
    /// filesystem path・module source location・URL resource identity 等。既定 HMAC-SHA-256。
    Path,
    /// source・stdin/stdout・file/network body・host args/result・error の自由文。既定 omit。
    Body,
    /// credential・token・password・secret env value・redaction key。常に omit（mode 変更不可）。
    Secret,
}

/// redaction mode（§9.1）。policy の class 別既定を表す。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedactionMode {
    /// 値をそのまま記録する。
    Reveal,
    /// canonical label に HMAC-SHA-256 を適用して digest だけ記録する。
    HmacSha256,
    /// 値を捨て class と byte 数だけ記録する。
    Omit,
}

/// audit field の方向（§9.1）。request / response を区別する。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AuditDirection {
    /// host-call request 側。
    Request,
    /// host-call response 側。
    Response,
}

/// body allow-list のキー（§9.1）。operation + 方向 + field 名/argument index で特定する。
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AuditFieldKey {
    /// operation（host function 名など）。
    pub operation: String,
    /// request / response。
    pub direction: AuditDirection,
    /// field 名または 0 始まり argument index の文字列。
    pub field: String,
}

/// redaction 前の scalar 値（§9.1）。canonical CBOR では宣言順を 0 始まり tag で encode する。
#[derive(Clone, Debug, PartialEq)]
pub enum AuditScalar {
    /// null。
    Null,
    /// bool。
    Bool(bool),
    /// 符号付き整数。
    Int(i64),
    /// canonicalized binary64 bits（§9.2）。
    Float(u64),
    /// 正規化しない UTF-8 text。
    Utf8(String),
    /// byte 列。
    Bytes(Vec<u8>),
}

/// redaction 前の field 値ツリー（§9.1）。scalar / sequence / object を持つ。
///
/// `Sequence` は argument/element 順を維持し、`Object` は UTF-8 key byte 順（`BTreeMap`）を保つ。
/// nesting は 64 段までとし、それ以上の subtree は呼び出し側が `Body`/`Omitted` へ縮退させる。
#[derive(Clone, Debug, PartialEq)]
pub enum AuditValue {
    /// scalar。
    Scalar(AuditScalar),
    /// 順序を保つ列（argument/element 順）。各要素は独立した `AuditField`。
    Sequence(Vec<AuditField>),
    /// UTF-8 key byte 順の map。各 value は独立した `AuditField`。
    Object(BTreeMap<String, AuditField>),
}

/// redaction 適用後の host-call payload（§9.1）。
///
/// `fields` は UTF-8 key byte 順（`BTreeMap`）。host call 引数は `fields["arguments"]`
/// （`AuditValue::Sequence`）へ、operation 固有 metadata は別 key へ入れる。
#[derive(Clone, Debug, PartialEq)]
pub struct AuditPayload {
    /// redaction 済み field 群（UTF-8 key byte 順）。
    pub fields: BTreeMap<String, AuditField>,
}

/// redaction 適用後の 1 field（§9.1）。canonical CBOR tag は 0=Revealed / 1=HmacSha256 /
/// 2=Omitted（§9.2 で予約済み）。`original_bytes` は redaction 前の subtree payload 長で、
/// omit/hash 後も保持する（R-1 では値の UTF-8 byte 長で代用、§9.1 / R-1 スライス）。
#[derive(Clone, Debug, PartialEq)]
pub enum AuditField {
    /// 値をそのまま記録する（tag 0）。
    Revealed {
        /// field class。
        class: AuditFieldClass,
        /// 記録する値ツリー。
        value: AuditValue,
        /// redaction 前の byte 長。
        original_bytes: u64,
    },
    /// canonical label の HMAC-SHA-256 digest を記録する（tag 1）。raw は記録しない。
    HmacSha256 {
        /// field class（Path 等）。
        class: AuditFieldClass,
        /// HMAC-SHA-256 digest（32 bytes）。
        digest: [u8; 32],
        /// redaction 前の byte 長。
        original_bytes: u64,
    },
    /// 値を捨て class と byte 数だけ記録する（tag 2）。
    Omitted {
        /// field class。
        class: AuditFieldClass,
        /// redaction 前の byte 長。
        original_bytes: u64,
    },
}

/// secret 値の型マーカー（§9.1）。`Debug` / `Display` / generic serializer を **実装しない**。
///
/// 存在と byte 数だけを Public metadata として記録できる。R-1 の filesystem 境界には Secret
/// field は現れないが、型と畳み規則をこのスライスで入れる（§9.1 / R-1 スライス）。
///
/// `Debug`/`Display`/serialize を導出・実装しないことは R-AT-07 の compile-fail doctest で固定する。
///
/// # R-AT-07: `Secret<T>` が `Debug` を実装しないことの compile-fail 固定（§15.4）
///
/// ```compile_fail
/// // (1) compile_fail 版: Secret を Debug で表示しようとすると compile しない。
/// let secret = tsumugi::Secret::new(42u32);
/// let _ = format!("{:?}", secret); // Secret<T> は Debug を実装しない
/// ```
///
/// ```
/// // (2) 通常版: 同じ型で Debug 以外（Clone）は compile・実行できる。
/// let secret = tsumugi::Secret::new(42u32);
/// let _cloned = secret.clone();
/// ```
#[derive(Clone)]
pub struct Secret<T> {
    inner: T,
}

impl<T> Secret<T> {
    /// secret を包む。
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    /// 包んだ値への参照（crate 内部でだけ使う。audit payload へは漏らさない）。
    pub(crate) fn expose(&self) -> &T {
        &self.inner
    }
}

/// secret な byte 列（§9.1）。HMAC key 等を保持する。`Debug` / `Display` / serialize を実装しない。
///
/// 存在と byte 数（[`Self::len`]）だけを Public metadata として公開する。
///
/// # R-AT-07: `Debug` を実装しないことの compile-fail 固定（§15.4）
///
/// `SecretBytes` は `Debug` を実装しないため、公開経路（`tsumugi::RedactionPolicy`）経由で
/// `hmac_key` を `{:?}` で表示しようとすると compile しない。これを compile_fail doctest で固定する。
/// 偽陽性（型が見えないだけで落ちる）を防ぐため、下の通常 doctest と 1 ペアにする。
///
/// ```compile_fail
/// // (1) compile_fail 版: 公開経路から SecretBytes を参照し Debug を要求 → compile しない。
/// let policy = tsumugi::RedactionPolicy::default();
/// let key: &tsumugi::SecretBytes = &policy.hmac_key;
/// let _ = format!("{:?}", key); // SecretBytes は Debug を実装しない
/// ```
///
/// ```
/// // (2) 通常版: 同じ公開経路で Debug 以外（byte 長・Clone）を使う → compile・実行できる。
/// // これにより (1) の失敗が「型が見えない」ではなく「Debug が無い」由来だと保証する。
/// let policy = tsumugi::RedactionPolicy::default();
/// let key: &tsumugi::SecretBytes = &policy.hmac_key;
/// assert!(key.len() > 0);
/// let _cloned = key.clone();
/// ```
#[derive(Clone)]
pub struct SecretBytes {
    bytes: Secret<Vec<u8>>,
}

impl SecretBytes {
    /// byte 列を包む。
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Secret::new(bytes),
        }
    }

    /// 保持している byte 数（Public metadata）。値そのものは公開しない。
    pub fn len(&self) -> usize {
        self.bytes.expose().len()
    }

    /// 空かどうか（clippy の `len_without_is_empty` 対応・Public metadata）。
    pub fn is_empty(&self) -> bool {
        self.bytes.expose().is_empty()
    }

    /// crate 内部でだけ raw bytes を参照する（HMAC 計算用。audit payload へは漏らさない）。
    pub(crate) fn expose(&self) -> &[u8] {
        self.bytes.expose()
    }
}

/// R-1 既定の固定 HMAC key（alpha 用・秘匿性なし、§7.1 の ambient 乱数禁止と R-AT-06 決定性のため
/// 乱数生成しない）。raw path を audit へそのまま載せないことだけを目的とする。key は公開されている
/// ため digest からの推測復元には耐えない（秘匿性のある host 注入経路は sink 必須化スライスへ延期）。
const DEFAULT_HMAC_KEY: &[u8] = b"tsumugi-r1-default-hmac-key-v1";

/// SHA-256 の block size（HMAC の ipad/opad padding に使う）。
const HMAC_BLOCK_SIZE: usize = 64;

/// redaction policy（§9.1）。R-1 の既定は path=HMAC・body=omit・identifier=reveal。
///
/// `hmac_key` は [`SecretBytes`] で保持し `Debug`/`Display` で生 key を出さない。この型自身の
/// `Debug` は手書きで key を `"<redacted>"` に伏せる（`Engine`/`EngineBuilder` の `#[derive(Debug)]`
/// を壊さないため）。
#[derive(Clone)]
pub struct RedactionPolicy {
    /// host が付ける安定ラベル（内容ハッシュではない、§9.1）。既定 `"default-r1"`。
    pub policy_id: String,
    /// identifier class の mode（既定 Reveal）。
    pub identifier_mode: RedactionMode,
    /// path class の mode（既定 HmacSha256）。
    pub path_mode: RedactionMode,
    /// body class の mode（既定 Omit）。
    pub body_mode: RedactionMode,
    /// body allow-list（R-1 では空・常に Omit）。
    pub reveal_body_fields: BTreeSet<AuditFieldKey>,
    /// path HMAC に使う秘密鍵。
    pub hmac_key: SecretBytes,
    /// Reveal できる body の上限 byte 数（R-1 既定 0）。
    pub max_revealed_body_bytes: u64,
}

impl Default for RedactionPolicy {
    /// R-1 の既定 policy（`policy_id = "default-r1"`、path=HMAC・body=omit・identifier=reveal、
    /// 空 allow-list、固定 `hmac_key`）。
    fn default() -> Self {
        Self {
            policy_id: "default-r1".to_string(),
            identifier_mode: RedactionMode::Reveal,
            path_mode: RedactionMode::HmacSha256,
            body_mode: RedactionMode::Omit,
            reveal_body_fields: BTreeSet::new(),
            hmac_key: SecretBytes::new(DEFAULT_HMAC_KEY.to_vec()),
            max_revealed_body_bytes: 0,
        }
    }
}

impl std::fmt::Debug for RedactionPolicy {
    /// `hmac_key` を `"<redacted>"` に伏せた手書き `Debug`（§9.1 / R-1 スライス）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedactionPolicy")
            .field("policy_id", &self.policy_id)
            .field("identifier_mode", &self.identifier_mode)
            .field("path_mode", &self.path_mode)
            .field("body_mode", &self.body_mode)
            .field("reveal_body_fields", &self.reveal_body_fields)
            .field("hmac_key", &"<redacted>")
            .field("max_revealed_body_bytes", &self.max_revealed_body_bytes)
            .finish()
    }
}

impl RedactionPolicy {
    /// canonical resource label へ HMAC-SHA-256 を適用して digest を返す（§9.1）。
    ///
    /// `crate::embedding::hash::sha256`（block size 64）の上に ipad 0x36 / opad 0x5c で
    /// HMAC-SHA-256 を組む。同じ key・同じ label なら同じ digest、別 label なら別 digest。
    pub fn hmac_path(&self, label: &str) -> [u8; 32] {
        hmac_sha256(self.hmac_key.expose(), label.as_bytes())
    }
}

/// HMAC-SHA-256（RFC 2104）。`crate::embedding::hash::sha256` を内部 hash に使う。
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use crate::embedding::hash::sha256;

    // key を block size へ正規化する: 長い key は hash、短い key は 0 padding。
    let mut block = [0u8; HMAC_BLOCK_SIZE];
    if key.len() > HMAC_BLOCK_SIZE {
        let digest = sha256(key);
        block[..digest.len()].copy_from_slice(&digest);
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; HMAC_BLOCK_SIZE];
    let mut opad = [0x5cu8; HMAC_BLOCK_SIZE];
    for i in 0..HMAC_BLOCK_SIZE {
        ipad[i] ^= block[i];
        opad[i] ^= block[i];
    }

    // inner = SHA-256(ipad || message)
    let mut inner_input = Vec::with_capacity(HMAC_BLOCK_SIZE + message.len());
    inner_input.extend_from_slice(&ipad);
    inner_input.extend_from_slice(message);
    let inner = sha256(&inner_input);

    // outer = SHA-256(opad || inner)
    let mut outer_input = Vec::with_capacity(HMAC_BLOCK_SIZE + inner.len());
    outer_input.extend_from_slice(&opad);
    outer_input.extend_from_slice(&inner);
    sha256(&outer_input)
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
// §7.3 BudgetCharged の集約（accumulator）
// =============================================================================

/// `BudgetCharged`（§7.2 CBOR tag=4）を §7.3 どおり集約するアキュムレータ。
///
/// 同じ poll 内の同 `resource`・同 `reason` の charge/release を 1 グループへ足し、flush で
/// 各グループを **既存の** [`AuditEvent::BudgetCharged`]（新 event 型は作らない）1 件へ畳む。
/// 加算は常に走ってよい軽量操作で、観測挙動を変えない。emission するのは embedding facade の
/// 監査経路（sink 設定時）だけで、[`Self::flush`] の結果を journal へ `append_normal` する。
///
/// # flush 点（§7.3）
///
/// 仕様の flush 点（host call 前・yield 前・pause 前・terminal 前・256 charge ごと）のうち、
/// 「256 charge ごと」はグループ内部で [`Self::note_charge`] が確定 buffer へ切り出すことで、
/// 残りの点は呼び出し側が [`Self::flush`] を呼ぶことで満たす。A-1 同期経路では embedding が
/// Terminal 直前に 1 回 flush する（§8 規則 7）。
///
/// # 決定論的順序
///
/// flush の出力は `(resource 優先順位, reason 宣言順)` の安定キー順にソートし、確定 buffer を
/// 先に、その後に残り進行中グループを置く。これにより backend 非依存の決定的 audit 列になる。
#[derive(Debug, Default)]
pub struct BudgetChargeAccumulator {
    /// 進行中グループ: (resource, reason) → 集約中の running 値。
    groups: Vec<ChargeGroup>,
    /// 256 charge 到達等で確定した `BudgetCharged` レコード（emission 待ち）。
    finalized: Vec<AuditEvent>,
    /// `finalized` の各 event の [`estimate_event_bytes`] 合計（Phase 6 B-2、§10.2）。
    ///
    /// 確定済み `BudgetCharged` は audit drain queue の一部として数える（未確定の進行中グループは
    /// (resource, reason) ごとに 1 件で有界なので数えない）。envelope 固定部は呼び出し側が件数から
    /// 足す（[`Self::finalized_queue_bytes`]）。[`Self::flush`] で 0 へ戻る。
    finalized_event_bytes: u64,
}

/// 進行中の 1 (resource, reason) グループ（§7.3）。
#[derive(Debug)]
struct ChargeGroup {
    resource: BudgetResource,
    reason: BudgetChargeReason,
    committed_delta: u64,
    released_delta: u64,
    usage_after: u64,
    peak_after: Option<u64>,
    charge_count: u64,
}

/// §7.3「256 charge ごとに flush」の閾値。
const CHARGE_FLUSH_THRESHOLD: u64 = 256;

impl ChargeGroup {
    fn into_event(self) -> AuditEvent {
        AuditEvent::BudgetCharged {
            resource: self.resource,
            committed_delta: self.committed_delta,
            released_delta: self.released_delta,
            usage_after: self.usage_after,
            peak_after: self.peak_after,
            charge_count: self.charge_count,
            reason: self.reason,
        }
    }
}

impl BudgetChargeAccumulator {
    /// 空のアキュムレータを作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// 進行中・確定済みが共に空か（主にテスト・診断用）。
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.finalized.is_empty()
    }

    /// 進行中グループの index を (resource, reason) で引く。
    fn position(&self, resource: BudgetResource, reason: BudgetChargeReason) -> Option<usize> {
        self.groups
            .iter()
            .position(|g| g.resource == resource && g.reason == reason)
    }

    /// cumulative charge を 1 件通知する（§7.3）。
    ///
    /// 同 `(resource, reason)` グループへ `committed_delta` を加算し `charge_count` を +1、
    /// `usage_after` を最新 committed 値で上書きする。グループの `charge_count` が
    /// [`CHARGE_FLUSH_THRESHOLD`] に達したらそのグループを確定 buffer へ切り出す
    /// （§7.3「256 charge ごと flush」）。
    pub fn note_charge(
        &mut self,
        resource: BudgetResource,
        reason: BudgetChargeReason,
        committed_delta: u64,
        usage_after: u64,
    ) {
        match self.position(resource, reason) {
            Some(idx) => {
                let g = &mut self.groups[idx];
                g.committed_delta = g.committed_delta.saturating_add(committed_delta);
                g.usage_after = usage_after;
                g.charge_count = g.charge_count.saturating_add(1);
                if g.charge_count >= CHARGE_FLUSH_THRESHOLD {
                    let group = self.groups.remove(idx);
                    self.push_finalized(group.into_event());
                }
            }
            None => {
                self.groups.push(ChargeGroup {
                    resource,
                    reason,
                    committed_delta,
                    released_delta: 0,
                    usage_after,
                    peak_after: None,
                    charge_count: 1,
                });
            }
        }
    }

    /// heap release を 1 件通知する（§7.3）。
    ///
    /// 同 `(resource, reason)` グループへ `released_delta` を加算し、`usage_after` を live、
    /// `peak_after` を `Some(peak)` で記録する（live heap 系）。release も 1 論理 charge として
    /// `charge_count` を +1 する。
    pub fn note_release(
        &mut self,
        resource: BudgetResource,
        reason: BudgetChargeReason,
        released_delta: u64,
        usage_after: u64,
        peak_after: u64,
    ) {
        match self.position(resource, reason) {
            Some(idx) => {
                let g = &mut self.groups[idx];
                g.released_delta = g.released_delta.saturating_add(released_delta);
                g.usage_after = usage_after;
                g.peak_after = Some(peak_after);
                g.charge_count = g.charge_count.saturating_add(1);
                if g.charge_count >= CHARGE_FLUSH_THRESHOLD {
                    let group = self.groups.remove(idx);
                    self.push_finalized(group.into_event());
                }
            }
            None => {
                self.groups.push(ChargeGroup {
                    resource,
                    reason,
                    committed_delta: 0,
                    released_delta,
                    usage_after,
                    peak_after: Some(peak_after),
                    charge_count: 1,
                });
            }
        }
    }

    /// 確定 buffer へ 1 件積み、audit drain queue 用の byte 合計を更新する（B-2）。
    fn push_finalized(&mut self, event: AuditEvent) {
        self.finalized_event_bytes = self
            .finalized_event_bytes
            .saturating_add(estimate_event_bytes(&event));
        self.finalized.push(event);
    }

    /// 確定済み（emission 待ち）`BudgetCharged` が audit drain queue に占める推定 byte 数
    /// （Phase 6 B-2、§10.2）。
    ///
    /// `envelope_overhead` は [`estimate_envelope_overhead_bytes`] の値で、確定済み 1 件ごとに
    /// envelope 固定部として足す。journal が append 時に計上する [`estimate_encoded_bytes`] と
    /// 同じ部品の和になる（§9.2: 別実装の size 推定を持たない）。
    pub(crate) fn finalized_queue_bytes(&self, envelope_overhead: u64) -> u64 {
        let count = self.finalized.len() as u64;
        self.finalized_event_bytes
            .saturating_add(count.saturating_mul(envelope_overhead))
    }

    /// 全進行中グループを畳み、確定 buffer と結合して決定的順序で返す（§7.3 / §8 規則 7）。
    ///
    /// 確定 buffer（256 charge 到達で先に切り出された分）を元の確定順で先頭に置き、その後に
    /// 残り進行中グループを `(resource 優先順位, reason 宣言順)` の安定キー順で並べる。返した後は
    /// 内部状態を空にする。
    pub fn flush(&mut self) -> Vec<AuditEvent> {
        let mut events = std::mem::take(&mut self.finalized);
        self.finalized_event_bytes = 0;
        let mut groups = std::mem::take(&mut self.groups);
        groups.sort_by_key(|g| (resource_sort_key(g.resource), reason_sort_key(g.reason)));
        events.extend(groups.into_iter().map(ChargeGroup::into_event));
        events
    }
}

/// resource の安定ソートキー（§7.2 の宣言順＝固定優先順位に沿う）。
fn resource_sort_key(resource: BudgetResource) -> u8 {
    match resource {
        BudgetResource::Fuel => 0,
        BudgetResource::HeapBytes => 1,
        BudgetResource::SingleStringBytes => 2,
        BudgetResource::StringAllocations => 3,
        BudgetResource::StringBytes => 4,
        BudgetResource::SingleSourceBytes => 5,
        BudgetResource::SourceCount => 6,
        BudgetResource::SourceBytes => 7,
        BudgetResource::ImportCount => 8,
        BudgetResource::ImportBytes => 9,
        BudgetResource::CollectionElements => 10,
        BudgetResource::InputCalls => 11,
        BudgetResource::InputBytes => 12,
        BudgetResource::OutputCalls => 13,
        BudgetResource::OutputBytes => 14,
        BudgetResource::HostCalls => 15,
        BudgetResource::HostRequestBytes => 16,
        BudgetResource::HostResponseBytes => 17,
        BudgetResource::HostCallBytes => 18,
    }
}

/// reason の安定ソートキー（§7.2 の宣言順）。
fn reason_sort_key(reason: BudgetChargeReason) -> u8 {
    match reason {
        BudgetChargeReason::Statement => 0,
        BudgetChargeReason::Expression => 1,
        BudgetChargeReason::Operation => 2,
        BudgetChargeReason::Function => 3,
        BudgetChargeReason::Loop => 4,
        BudgetChargeReason::HostCall => 5,
        BudgetChargeReason::BulkElements => 6,
        BudgetChargeReason::BulkBytes => 7,
        BudgetChargeReason::Allocation => 8,
        BudgetChargeReason::Source => 9,
        BudgetChargeReason::Input => 10,
        BudgetChargeReason::Output => 11,
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
    /// execution ごとの最大 pending byte 数（既定 1 MiB）。audit drain queue（生成済み・未 ack の
    /// 通常 event）がこの値に達すると、トップレベルの文/反復境界で `AuditBackpressure` yield する
    /// （§10.2、Phase 6 B-2。engine 全体の上限は未実装）。
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
///
/// 「envelope 固定部（`estimate_envelope_overhead_bytes`）＋ event 本体
/// （`estimate_event_bytes`）」の和。Phase 6 B-2 の audit drain queue（§10.2）は envelope 化前
/// （sequence 未確定）の event を同じ 2 部品で計上するため、journal と queue が同一の推定を
/// 共有する（§9.2: 予算 byte counter が別実装の size 推定を持つことを禁止）。
pub fn estimate_encoded_bytes(envelope: &AuditEnvelope) -> u64 {
    estimate_envelope_overhead_bytes(&envelope.language_revision)
        .saturating_add(estimate_event_bytes(&envelope.event))
}

/// envelope 固定部の naive 推定（Phase 6 B-2 で [`estimate_encoded_bytes`] から分離）。
///
/// schema_version(2) + execution_id(16) + source_hash(32) + sequence(8) + timestamp(16)
/// ＝ 74 bytes 程度に可変長の language_revision を足す。sequence に依存しないので、sequence
/// 確定前の event にも適用できる。§9.2 strict CBOR へ差し替えると sequence/timestamp の encode
/// 長が値依存になるため、その時点では sequence を最大長とみなした上限値を返す。
pub(crate) fn estimate_envelope_overhead_bytes(language_revision: &str) -> u64 {
    74u64.saturating_add(language_revision.len() as u64)
}

/// event 本体の naive 推定（envelope 固定部を除く）。
pub(crate) fn estimate_event_bytes(event: &AuditEvent) -> u64 {
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
    /// 呼び出し元が渡した envelope が journal 不変条件に違反した（§8）。
    ///
    /// append 時に期待する `sequence`（journal が次に払い出す値）と envelope の `sequence` が
    /// 食い違う、または append 口（normal/terminal）と event 種別が一致しない場合に返る。
    /// journal は §8 の no-gap / no-duplicate / exactly-one-Terminal を自ら強制するため、
    /// debug/release を問わず（debug_assert ではなく）この失敗で fail-closed にする。
    InvariantViolation,
    /// backpressure 中に sink が永久に `Pending` のまま execution deadline へ達した（§10.2）。
    ///
    /// `Yielded(AuditBackpressure)` で待機中に deadline/cancel checkpoint が立ち、ack が
    /// 取れなかった場合の fail-closed 信号。§10.2 の規定どおり script の `Deadline` より優先し、
    /// 「実行結果は成功だが監査が欠ける」状態を作らない。
    BackpressureDeadline,
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

        // 呼び出し元の envelope を信用せず、journal 自身が sequence/種別を検証する（§8）。
        // Terminal を通常 event として受けない（append_terminal 専用）。
        if matches!(envelope.event, AuditEvent::Terminal { .. }) {
            return Err(AuditFailure::InvariantViolation);
        }
        // envelope.sequence は journal が次に払い出す値と一致しなければならない（no-gap/no-dup）。
        if envelope.sequence != self.next_sequence {
            return Err(AuditFailure::InvariantViolation);
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

    /// 次の host-call lifecycle triple を通常枠へ収められるかを事前判定する（§11 reserve）。
    ///
    /// §11 は host call 開始前に、`HostCallStarted`（通常 1 event / 最大 60 KiB）と、後続
    /// `CapabilityDecision` + `HostCallFinished` の close reserve（2 events / 128 KiB）を atomic に
    /// reserve することを要求する。A-1 は triple を post-run に drain する（§14）ため、ここでは
    /// 「あと `host_call_close_reserve_events + 1` 件・`host_call_close_reserve_bytes + 60 KiB`
    /// 相当を通常枠（Terminal 予約を除いた分）へ収められるか」の容量チェックとして実装する。
    ///
    /// A-1 では faithful な pre-effect reservation（reserve 失敗時に外部効果自体を拒否する）は
    /// retry/backpressure スライスへ延期し、ここは drain 時の容量チェックとして機能する。収まら
    /// なければ監査付き facade は triple を 1 件も出さず `AuditFailure::BudgetExceeded` へ落ちる。
    pub fn can_reserve_host_call(&self) -> bool {
        if self.terminated {
            return false;
        }
        // 通常 event が使える event-count 上限（Terminal 予約を除く）。
        let normal_event_limit = self
            .budget
            .max_events
            .saturating_sub(self.budget.terminal_reserve_events);
        // triple は Started 1 件 + close reserve 分（Decision + Finished）。
        let needed_events = self.budget.host_call_close_reserve_events.saturating_add(1);
        let projected_events = self.committed_events.saturating_add(needed_events);
        if projected_events > normal_event_limit {
            return false;
        }

        // 通常 byte 上限（Terminal 予約を除く）。
        let normal_byte_limit = self
            .budget
            .max_encoded_bytes
            .saturating_sub(self.budget.terminal_reserve_bytes);
        // §11 の share: HostCallStarted ≤60 KiB + close reserve byte（既定 128 KiB）。
        const HOST_CALL_STARTED_MAX_BYTES: u64 = 60 * KIB;
        let needed_bytes = self
            .budget
            .host_call_close_reserve_bytes
            .saturating_add(HOST_CALL_STARTED_MAX_BYTES);
        let projected_bytes = self.committed_bytes.saturating_add(needed_bytes);
        if projected_bytes > normal_byte_limit {
            return false;
        }
        true
    }

    /// Terminal を予約 slot を使って append し、割り当てた sequence を返す（§8 規則9/10）。
    ///
    /// Started 済みであれば通常上限を使い切っていても必ず成功する（Terminal 専用予約を使う）。
    /// Started 前・Terminal 後の呼び出しは不変条件違反で `AuditFailure` を返す。sequence の
    /// checked increment の overflow だけは fail-closed にする。
    pub fn append_terminal(&mut self, envelope: &AuditEnvelope) -> Result<u64, AuditFailure> {
        // 呼び出し元の envelope を信用せず、journal 自身が種別/ sequence を検証する（§8）。
        // event 種別の検査は debug_assert ではなく全ビルドで有効な fail-closed にする。
        if !matches!(envelope.event, AuditEvent::Terminal { .. }) {
            return Err(AuditFailure::InvariantViolation);
        }
        if !self.started {
            // Started の無い execution に Terminal は無い（§8）。
            return Err(AuditFailure::BudgetExceeded);
        }
        if self.terminated {
            // Terminal は 1 件のみ（§8 規則9/10）。
            return Err(AuditFailure::BudgetExceeded);
        }
        // envelope.sequence は journal が次に払い出す値と一致しなければならない（no-gap/no-dup）。
        if envelope.sequence != self.next_sequence {
            return Err(AuditFailure::InvariantViolation);
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

/// [`AuditTicket`] の共有内部（§10）。result slot + 登録 waker + cancel flag を 1 つの
/// `Mutex` 下で守り、lock-free fast path 用に `ready` を別持ちする。
///
/// 雛形は [`crate::host_pending`] の `TicketShared`（host-call ticket）。lost-wake を避けるため、
/// `complete` は result 格納後に `ready` を waker 読み取りより先に store し、`register_waker` は
/// waker 保存後に `ready` を再確認する（§10 / Finding 1）。
struct AuditTicketShared {
    /// result / waker / cancelled を 1 つの `Mutex` 下で守る。
    state: Mutex<AuditTicketState>,
    /// store-waker → recheck-ready の lock-free fast path（§10）。
    ready: std::sync::atomic::AtomicBool,
}

/// [`AuditTicketShared`] の `Mutex` が守る可変状態。
struct AuditTicketState {
    /// 一度だけ格納される ack/error（`complete` が `None` のときだけ書き込む）。
    result: Option<Result<AuditAck, AuditSinkError>>,
    /// 登録済み waker（最後の `register_waker` が置換）。
    waker: Option<AuditWaker>,
    /// cancel / drop 済みか。cancel 後は格納済み / 遅着 result を破棄する。
    cancelled: bool,
}

/// backpressure 時の wake ハンドル（§10）。`Arc<dyn Wake>` を包み `Send + Sync`・`Clone`。
///
/// sink の completer が ack/error を [`AuditTicket`] へ格納したとき `wake()` を呼び、作成 thread
/// の次回 poll を促す。A-1 の同期 driver では poll が `register_waker` 後に `try_take` を
/// ポーリングするため wake が無くても前進できるが（scope 3）、将来の非同期 sink 連携のため
/// [`crate::host_pending::Wake`] 雛形の実配線を持つ。
#[derive(Clone)]
pub struct AuditWaker {
    inner: Arc<dyn crate::host_pending::Wake>,
}

impl AuditWaker {
    /// `Arc<dyn Wake>` から waker を作る。
    pub fn new(wake: Arc<dyn crate::host_pending::Wake>) -> Self {
        Self { inner: wake }
    }

    /// 何もしない waker を作る（A-1 同期 driver は `try_take` ポーリングで足りる）。
    pub fn noop() -> Self {
        Self {
            inner: Arc::new(NoopWake),
        }
    }

    /// pending の sink を wake する（§10.2）。登録先 `Wake` へ委譲する。
    pub fn wake(&self) {
        self.inner.wake();
    }
}

/// 何もしない [`Wake`](crate::host_pending::Wake)（[`AuditWaker::noop`] 用）。
struct NoopWake;

impl crate::host_pending::Wake for NoopWake {
    fn wake(&self) {}
}

impl std::fmt::Debug for AuditWaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditWaker").finish_non_exhaustive()
    }
}

/// pending batch の ticket（§10）。sink が `Pending` を返したとき facade driver が保持する。
///
/// 雛形は [`crate::host_pending::HostCallTicket`]。facade driver は backpressure 進入時に
/// [`Self::register_waker`] し、各 poll ステップで [`Self::try_take`] を駆動する。sink 側は
/// 対になる [`AuditCompleter`] で ack/error を一度だけ格納する。
#[derive(Clone)]
pub struct AuditTicket {
    shared: Arc<AuditTicketShared>,
}

impl AuditTicket {
    /// 空の ticket を作る（completer を伴わない孤立 ticket。主に型の互換用）。
    pub fn new() -> Self {
        Self {
            shared: Arc::new(AuditTicketShared {
                state: Mutex::new(AuditTicketState {
                    result: None,
                    waker: None,
                    cancelled: false,
                }),
                ready: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// ack/error 格納時に wake する waker を登録する（§10、lost-wake 回避）。
    ///
    /// 1. `Mutex` を取り waker を保存（置換）する。
    /// 2. lock 解放後に `ready` を再確認し、ready なら即 wake する（complete が先行していた場合）。
    pub fn register_waker(&self, waker: AuditWaker) {
        {
            let mut st = self.shared.state.lock().expect("audit ticket poisoned");
            st.waker = Some(waker.clone());
        }
        if self.shared.ready.load(std::sync::atomic::Ordering::Acquire) {
            waker.wake();
        }
    }

    /// 格納済みの ack/error を 1 回取り出す（§10）。
    ///
    /// `ready` が立っていて cancel されていなければ、格納済み `result` を `take()` して返す。
    /// cancel 済みなら格納済み / 遅着 result を破棄して `None`。未 ready も `None`。
    pub fn try_take(&self) -> Option<Result<AuditAck, AuditSinkError>> {
        if !self.shared.ready.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        let mut st = self.shared.state.lock().expect("audit ticket poisoned");
        if st.cancelled {
            st.result = None;
            return None;
        }
        st.result.take()
    }

    /// ticket を取り消し、以後の result を破棄する（§10）。
    pub fn cancel(&self) {
        let mut st = self.shared.state.lock().expect("audit ticket poisoned");
        st.cancelled = true;
        st.result = None;
    }
}

impl Default for AuditTicket {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for AuditTicket {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }
}

impl Eq for AuditTicket {}

impl std::fmt::Debug for AuditTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditTicket").finish_non_exhaustive()
    }
}

/// sink が ack/error を [`AuditTicket`] へ書き戻すハンドル（§10）。
///
/// 雛形は [`crate::host_pending::HostCallCompleter`]。sink 実装（例: [`ScriptedAuditSink`]）が
/// `Pending` を返すとき、対の completer を保持し、後続の同一 batch 再送で ack を格納する。
pub struct AuditCompleter {
    shared: Arc<AuditTicketShared>,
}

impl AuditCompleter {
    /// ack/error を一度だけ格納し、登録済み waker を wake する（§10、lost-wake 回避）。
    ///
    /// 1. `Mutex` を取り `result` が `None` のときだけ格納する（二度目は破棄）。
    /// 2. `ready.store(true, Release)` を **waker 読み取りより先に** 行う。
    /// 3. 保存済み waker を退避し、lock 解放後に wake する。
    pub fn complete(self, result: Result<AuditAck, AuditSinkError>) {
        {
            let mut st = self.shared.state.lock().expect("audit ticket poisoned");
            if st.result.is_some() {
                return;
            }
            st.result = Some(result);
        }
        self.shared
            .ready
            .store(true, std::sync::atomic::Ordering::Release);
        let waker = {
            let st = self.shared.state.lock().expect("audit ticket poisoned");
            st.waker.clone()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// cancel 済みか（sink 側が協調停止を観測する）。
    pub fn is_cancelled(&self) -> bool {
        self.shared
            .state
            .lock()
            .map(|st| st.cancelled)
            .unwrap_or(true)
    }
}

impl std::fmt::Debug for AuditCompleter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditCompleter").finish_non_exhaustive()
    }
}

/// ticket と completer のペアを作る（§10）。両者が内部状態を共有する。
pub fn new_audit_ticket() -> (AuditTicket, AuditCompleter) {
    let shared = Arc::new(AuditTicketShared {
        state: Mutex::new(AuditTicketState {
            result: None,
            waker: None,
            cancelled: false,
        }),
        ready: std::sync::atomic::AtomicBool::new(false),
    });
    (
        AuditTicket {
            shared: Arc::clone(&shared),
        },
        AuditCompleter { shared },
    )
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
    /// 進行中の backpressure episode（§10.2 テスト用）。`Some` の間は plan を進めず、
    /// 同一 batch の再送に対して `remaining` 回だけ `Pending` を返し続ける。
    pending: Mutex<PendingEpisode>,
}

/// [`ScriptedAuditSink`] の進行中 backpressure episode（決定的テスト用）。
#[derive(Default)]
struct PendingEpisode {
    /// 残りの `Pending` 回数（`Some(0)` は「次の submit で Ack」）。`None` は episode 無し。
    remaining: Option<u32>,
    /// `PendingForever`（ack を決して格納しない、deadline テスト用）。
    forever: bool,
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
    /// `pending_times` 回だけ `Pending` を返し、その後（同一 batch の再送で）正しい Ack を
    /// 返す（§10.2 backpressure テスト用）。`pending_times == 0` は即 Ack と等価。
    PendingThenAck {
        /// `Pending` を返す回数。
        pending_times: u32,
    },
    /// 決して Ack を格納せず常に `Pending` を返す（§10.2 永続 Pending→BackpressureDeadline
    /// テスト用）。
    PendingForever,
}

impl ScriptedAuditSink {
    /// 応答計画を与えて作る。計画消費後は `AckCorrect` を繰り返す。
    pub fn new(plan: impl IntoIterator<Item = ScriptedResponse>) -> Self {
        Self {
            plan: Mutex::new(plan.into_iter().collect()),
            tail: ScriptedResponse::AckCorrect,
            envelopes: Mutex::new(Vec::new()),
            pending: Mutex::new(PendingEpisode::default()),
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

impl ScriptedAuditSink {
    /// batch を 1 回だけ記録する（同一 batch の再送では重複記録しない）。Ack 成立時に呼ぶ。
    fn record(&self, batch: &Arc<[AuditEnvelope]>) {
        self.envelopes
            .lock()
            .expect("audit sink poisoned")
            .extend(batch.iter().cloned());
    }
}

impl AuditSink for ScriptedAuditSink {
    fn submit(&self, batch: Arc<[AuditEnvelope]>, _waker: AuditWaker) -> AuditSubmit {
        let last = match batch.last() {
            Some(last) => (last.execution_id, last.sequence),
            None => return AuditSubmit::Failed(AuditSinkError::Protocol),
        };
        let ack = AuditSubmit::Ack(AuditAck {
            execution_id: last.0,
            through_sequence: last.1,
        });

        // 進行中の backpressure episode を先に処理する（plan は進めない、同一 batch 再送）。
        {
            let mut episode = self.pending.lock().expect("audit sink poisoned");
            if episode.forever {
                // 永続 Pending: 決して ack せず、記録もしない。
                return AuditSubmit::Pending(AuditTicket::new());
            }
            if let Some(remaining) = episode.remaining {
                if remaining > 0 {
                    episode.remaining = Some(remaining - 1);
                    return AuditSubmit::Pending(AuditTicket::new());
                }
                // 残り 0: episode 終了 → この再送で Ack（初回の 1 回だけ記録）。
                episode.remaining = None;
                drop(episode);
                self.record(&batch);
                return ack;
            }
        }

        // episode 無し: plan から次の応答を取り出す。
        let response = self
            .plan
            .lock()
            .expect("audit sink poisoned")
            .pop_front()
            .unwrap_or(self.tail);
        match response {
            ScriptedResponse::Fail => AuditSubmit::Failed(AuditSinkError::Permanent),
            ScriptedResponse::AckCorrect => {
                self.record(&batch);
                ack
            }
            ScriptedResponse::AckWrongId => {
                self.record(&batch);
                AuditSubmit::Ack(AuditAck {
                    // 必ず batch の id と異なる値（nonzero 保証のため wrapping_add(1) を使う）。
                    execution_id: wrong_execution_id(last.0),
                    through_sequence: last.1,
                })
            }
            ScriptedResponse::AckWrongSequence => {
                self.record(&batch);
                AuditSubmit::Ack(AuditAck {
                    execution_id: last.0,
                    through_sequence: last.1.wrapping_add(1),
                })
            }
            ScriptedResponse::PendingThenAck { pending_times } => {
                if pending_times == 0 {
                    // Pending 0 回 = 即 Ack。
                    self.record(&batch);
                    ack
                } else {
                    // episode 開始: 残り pending_times 回 Pending を返す。
                    let mut episode = self.pending.lock().expect("audit sink poisoned");
                    episode.remaining = Some(pending_times - 1);
                    AuditSubmit::Pending(AuditTicket::new())
                }
            }
            ScriptedResponse::PendingForever => {
                let mut episode = self.pending.lock().expect("audit sink poisoned");
                episode.forever = true;
                AuditSubmit::Pending(AuditTicket::new())
            }
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

    // --- B-2（§10.2・§9.2）: audit drain queue と journal が同じ byte 推定を共有する ---

    /// B2-AT-10: envelope 化前の event を「envelope 固定部 ＋ event 本体」で計上した値が、同じ
    /// event を journal へ append したときの `committed_bytes` の増分と一致する。
    #[test]
    fn b2_at_10_queue_estimate_matches_journal_committed_bytes() {
        let id = exec_id(42);
        let mut acc = BudgetChargeAccumulator::new();
        acc.note_charge(BudgetResource::Fuel, BudgetChargeReason::Operation, 3, 3);
        let charged = acc.flush().remove(0);
        let events = vec![
            AuditEvent::HostCallStarted {
                operation_id: 0,
                call_id: 0,
                function: "write_file".to_string(),
                request: AuditPayload {
                    fields: BTreeMap::new(),
                },
                request_bytes: 5,
            },
            AuditEvent::Yielded {
                reason: AuditYieldReason::AuditBackpressure,
                poll_index: 0,
                usage: BudgetUsage::default(),
            },
            AuditEvent::Resumed {
                previous_reason: AuditYieldReason::AuditBackpressure,
                poll_index: 0,
                usage: BudgetUsage::default(),
            },
            charged,
        ];

        let mut journal = AuditJournal::open(AuditBudget::default());
        journal
            .append_normal(&started_envelope(id, 0))
            .expect("started");
        for event in events {
            let envelope = AuditEnvelope {
                event: event.clone(),
                sequence: journal.next_sequence(),
                ..started_envelope(id, 0)
            };
            let queued = estimate_envelope_overhead_bytes(&envelope.language_revision)
                .saturating_add(estimate_event_bytes(&event));
            let before = journal.committed_bytes;
            journal.append_normal(&envelope).expect("append");
            assert_eq!(
                journal.committed_bytes - before,
                queued,
                "queue の計上値と journal の committed_bytes 増分が一致する: {event:?}"
            );
            assert_eq!(estimate_encoded_bytes(&envelope), queued);
        }
    }

    /// accumulator の確定済み（256-charge cut）分だけが queue byte に数えられ、flush で 0 に戻る。
    #[test]
    fn b2_finalized_charges_count_toward_queue_until_flush() {
        let overhead = estimate_envelope_overhead_bytes("0.21");
        let mut acc = BudgetChargeAccumulator::new();
        // 進行中グループ（未確定）は数えない。
        acc.note_charge(BudgetResource::Fuel, BudgetChargeReason::Operation, 1, 1);
        assert_eq!(acc.finalized_queue_bytes(overhead), 0);
        // 256 charge で確定 buffer へ切り出された 1 件を数える。
        for n in 2..=CHARGE_FLUSH_THRESHOLD {
            acc.note_charge(BudgetResource::Fuel, BudgetChargeReason::Operation, 1, n);
        }
        let finalized = acc.finalized.clone();
        assert_eq!(finalized.len(), 1);
        assert_eq!(
            acc.finalized_queue_bytes(overhead),
            overhead + estimate_event_bytes(&finalized[0])
        );
        let _ = acc.flush();
        assert_eq!(acc.finalized_queue_bytes(overhead), 0);
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
        // sequence を u64::MAX へ進め、Terminal の checked increment overflow を観測する。
        // envelope.sequence も next_sequence に揃えて渡し、sequence 検証を通して overflow 経路へ入る。
        journal.next_sequence = u64::MAX;
        assert_eq!(
            journal.append_terminal(&terminal_envelope(id, u64::MAX, TerminalOutcome::Completed)),
            Err(AuditFailure::SequenceExhausted)
        );
        // wrap していない（fail-closed）。
        assert!(!journal.is_terminated());
    }

    #[test]
    fn journal_rejects_wrong_sequence_on_normal_append() {
        // envelope.sequence が journal の次 sequence と食い違うと fail-closed（§8 no-gap/no-dup）。
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        // 初回 Started に sequence 99 を渡す（期待は 0）。
        assert_eq!(
            journal.append_normal(&started_envelope(id, 99)),
            Err(AuditFailure::InvariantViolation)
        );
        // journal 状態は進んでいない（Started 未登録）。
        assert!(!journal.is_started());
        assert_eq!(journal.next_sequence(), 0);
    }

    #[test]
    fn journal_rejects_terminal_event_via_append_normal() {
        // Terminal event を通常 append 口へ渡しても受け付けない（append_terminal 専用）。
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        assert_eq!(
            journal.append_normal(&terminal_envelope(id, 1, TerminalOutcome::Completed)),
            Err(AuditFailure::InvariantViolation)
        );
        // Terminal 化していない（通常 append では terminate しない）。
        assert!(!journal.is_terminated());
    }

    #[test]
    fn journal_rejects_non_terminal_event_via_append_terminal() {
        // 非 Terminal event を terminal append 口へ渡すと全ビルドで fail-closed（debug_assert ではない）。
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        assert_eq!(
            journal.append_terminal(&started_envelope(id, 1)),
            Err(AuditFailure::InvariantViolation)
        );
        assert!(!journal.is_terminated());
    }

    #[test]
    fn journal_rejects_wrong_sequence_on_terminal_append() {
        // Terminal の envelope.sequence が次 sequence と食い違うと fail-closed。
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        // 期待は 1 だが 5 を渡す。
        assert_eq!(
            journal.append_terminal(&terminal_envelope(id, 5, TerminalOutcome::Completed)),
            Err(AuditFailure::InvariantViolation)
        );
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

    // --- §11 host-call reserve 容量チェック（can_reserve_host_call） ---

    #[test]
    fn can_reserve_host_call_true_with_default_budget() {
        // 既定予算は 100,000 event / 16 MiB。Started 直後は triple を余裕で収められる。
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        assert!(journal.can_reserve_host_call());
    }

    #[test]
    fn can_reserve_host_call_false_when_only_started_fits_but_not_triple() {
        // §15.3 境界: Started 1 件だけは収まるが triple（Started + close reserve 2 件）は収まらない
        // event 数を作る。max_events=4, terminal_reserve=1 → 通常枠は 3 件。triple は 1+2=3 件
        // 必要なので Started(1 件 commit) 後は残り 2 件で足りない → false。
        let id = exec_id(1);
        let budget = AuditBudget {
            max_events: 4,
            ..AuditBudget::default()
        };
        assert_eq!(budget.validate(), Ok(()));
        let mut journal = AuditJournal::open(budget);
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        // committed_events=1、通常枠=3。triple に必要な needed_events=3 → 1+3=4 > 3 で false。
        assert!(!journal.can_reserve_host_call());
    }

    #[test]
    fn can_reserve_host_call_false_when_byte_budget_too_small() {
        // byte 枠が triple share（Started 60 KiB + close reserve 128 KiB）に満たない境界。
        // terminal_reserve_bytes=64 KiB は固定なので、max_encoded_bytes を share ちょうど下回る値に。
        let id = exec_id(1);
        let budget = AuditBudget {
            // 通常 byte 枠 = max_encoded_bytes - 64 KiB。share = 128 KiB + 60 KiB = 188 KiB。
            // max_encoded_bytes = 64 KiB + 188 KiB - 1 にすると通常枠が share を 1 byte 下回る。
            max_encoded_bytes: 64 * KIB + 188 * KIB - 1,
            ..AuditBudget::default()
        };
        assert_eq!(budget.validate(), Ok(()));
        let mut journal = AuditJournal::open(budget);
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        assert!(!journal.can_reserve_host_call());
    }

    #[test]
    fn can_reserve_host_call_false_after_terminal() {
        let id = exec_id(1);
        let mut journal = AuditJournal::open(AuditBudget::default());
        journal.append_normal(&started_envelope(id, 0)).unwrap();
        journal
            .append_terminal(&terminal_envelope(id, 1, TerminalOutcome::Completed))
            .unwrap();
        assert!(!journal.can_reserve_host_call());
    }

    // --- §7.3 BudgetChargeAccumulator ---

    /// `BudgetCharged` から集約フィールドを取り出すテストヘルパ。
    fn as_charged(
        event: &AuditEvent,
    ) -> (
        BudgetResource,
        BudgetChargeReason,
        u64,
        u64,
        u64,
        Option<u64>,
        u64,
    ) {
        match event {
            AuditEvent::BudgetCharged {
                resource,
                committed_delta,
                released_delta,
                usage_after,
                peak_after,
                charge_count,
                reason,
            } => (
                *resource,
                *reason,
                *committed_delta,
                *released_delta,
                *usage_after,
                *peak_after,
                *charge_count,
            ),
            other => panic!("期待 BudgetCharged, 実際 {other:?}"),
        }
    }

    #[test]
    fn accumulator_aggregates_same_resource_and_reason() {
        let mut acc = BudgetChargeAccumulator::new();
        // 同 (Fuel, Operation) を 3 回。usage_after は最新値で上書き。
        acc.note_charge(BudgetResource::Fuel, BudgetChargeReason::Operation, 1, 1);
        acc.note_charge(BudgetResource::Fuel, BudgetChargeReason::Operation, 2, 3);
        acc.note_charge(BudgetResource::Fuel, BudgetChargeReason::Operation, 4, 7);
        let events = acc.flush();
        assert_eq!(events.len(), 1);
        let (resource, reason, committed, released, usage_after, peak, count) =
            as_charged(&events[0]);
        assert_eq!(resource, BudgetResource::Fuel);
        assert_eq!(reason, BudgetChargeReason::Operation);
        assert_eq!(committed, 7); // 1 + 2 + 4
        assert_eq!(released, 0);
        assert_eq!(usage_after, 7); // 最新値
        assert_eq!(peak, None); // cumulative は peak なし
        assert_eq!(count, 3);
        // flush で空になる。
        assert!(acc.is_empty());
    }

    #[test]
    fn accumulator_separates_distinct_reason_and_resource() {
        let mut acc = BudgetChargeAccumulator::new();
        acc.note_charge(BudgetResource::Fuel, BudgetChargeReason::Operation, 1, 1);
        acc.note_charge(
            BudgetResource::StringBytes,
            BudgetChargeReason::BulkBytes,
            10,
            10,
        );
        // 同 resource でも reason が違えば別グループ。
        acc.note_charge(BudgetResource::Fuel, BudgetChargeReason::Statement, 2, 3);
        let events = acc.flush();
        assert_eq!(events.len(), 3);
        // 決定的順序: Fuel(0) が先。Fuel 内は reason 宣言順（Statement(0) < Operation(2)）。
        let (r0, reason0, ..) = as_charged(&events[0]);
        let (r1, reason1, ..) = as_charged(&events[1]);
        let (r2, ..) = as_charged(&events[2]);
        assert_eq!(r0, BudgetResource::Fuel);
        assert_eq!(reason0, BudgetChargeReason::Statement);
        assert_eq!(r1, BudgetResource::Fuel);
        assert_eq!(reason1, BudgetChargeReason::Operation);
        assert_eq!(r2, BudgetResource::StringBytes);
    }

    #[test]
    fn accumulator_flushes_group_every_256_charges() {
        let mut acc = BudgetChargeAccumulator::new();
        // 256 回で確定 buffer へ切り出し、257 回目は新グループ（進行中）。
        for i in 0..257u64 {
            acc.note_charge(
                BudgetResource::Fuel,
                BudgetChargeReason::Operation,
                1,
                i + 1,
            );
        }
        let events = acc.flush();
        // 確定分 1 件（256 charge）+ 残り進行中 1 件（1 charge）。
        assert_eq!(events.len(), 2);
        let (_, _, committed0, _, _, _, count0) = as_charged(&events[0]);
        assert_eq!(count0, 256);
        assert_eq!(committed0, 256);
        let (_, _, committed1, _, _, _, count1) = as_charged(&events[1]);
        assert_eq!(count1, 1);
        assert_eq!(committed1, 1);
    }

    #[test]
    fn accumulator_records_release_with_peak() {
        let mut acc = BudgetChargeAccumulator::new();
        acc.note_release(
            BudgetResource::HeapBytes,
            BudgetChargeReason::Allocation,
            64,
            128,
            256,
        );
        let events = acc.flush();
        assert_eq!(events.len(), 1);
        let (resource, reason, committed, released, usage_after, peak, count) =
            as_charged(&events[0]);
        assert_eq!(resource, BudgetResource::HeapBytes);
        assert_eq!(reason, BudgetChargeReason::Allocation);
        assert_eq!(committed, 0);
        assert_eq!(released, 64);
        assert_eq!(usage_after, 128); // live
        assert_eq!(peak, Some(256)); // live heap は peak を載せる
        assert_eq!(count, 1);
    }

    #[test]
    fn accumulator_flush_order_is_deterministic() {
        let mut acc = BudgetChargeAccumulator::new();
        // 投入順をわざと resource 優先順位と逆にする。
        acc.note_charge(
            BudgetResource::OutputBytes,
            BudgetChargeReason::Output,
            1,
            1,
        );
        acc.note_charge(
            BudgetResource::StringBytes,
            BudgetChargeReason::BulkBytes,
            1,
            1,
        );
        acc.note_charge(BudgetResource::Fuel, BudgetChargeReason::Operation, 1, 1);
        let events = acc.flush();
        let resources: Vec<BudgetResource> = events.iter().map(|e| as_charged(e).0).collect();
        // resource 優先順位順: Fuel < StringBytes < OutputBytes。
        assert_eq!(
            resources,
            vec![
                BudgetResource::Fuel,
                BudgetResource::StringBytes,
                BudgetResource::OutputBytes,
            ]
        );
    }

    #[test]
    fn accumulator_empty_flush_returns_nothing() {
        let mut acc = BudgetChargeAccumulator::new();
        assert!(acc.flush().is_empty());
        assert!(acc.is_empty());
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

    // --- AuditTicket / AuditCompleter（host_pending パターン、§10 / B-1）---

    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    /// wake 回数を数える fake `Wake`（決定的に lost-wake 回避を検証する）。
    struct CountingWake {
        count: Arc<AtomicUsize>,
    }

    impl crate::host_pending::Wake for CountingWake {
        fn wake(&self) {
            self.count.fetch_add(1, AtomicOrdering::SeqCst);
        }
    }

    fn counting_waker() -> (AuditWaker, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let waker = AuditWaker::new(Arc::new(CountingWake {
            count: Arc::clone(&count),
        }));
        (waker, count)
    }

    fn sample_ack() -> AuditAck {
        AuditAck {
            execution_id: exec_id(7),
            through_sequence: 3,
        }
    }

    #[test]
    fn audit_ticket_try_take_returns_result_after_ready() {
        let (ticket, completer) = new_audit_ticket();
        assert!(ticket.try_take().is_none(), "未完了は None");
        completer.complete(Ok(sample_ack()));
        match ticket.try_take() {
            Some(Ok(ack)) => assert_eq!(ack, sample_ack()),
            other => panic!("Ok(ack) を期待したが {other:?}"),
        }
        assert!(ticket.try_take().is_none(), "2 回目は take 済みで None");
    }

    #[test]
    fn audit_ticket_cancel_discards_late_result() {
        let (ticket, completer) = new_audit_ticket();
        ticket.cancel();
        completer.complete(Ok(sample_ack()));
        assert!(
            ticket.try_take().is_none(),
            "cancel 済み ticket は遅着 result を返さない"
        );
    }

    #[test]
    fn audit_ticket_lost_wake_avoided_register_after_complete() {
        // (a) register-after-complete: complete を先に、register_waker を後に。
        let (ticket, completer) = new_audit_ticket();
        let (waker, count) = counting_waker();
        completer.complete(Ok(sample_ack()));
        assert_eq!(
            count.load(AtomicOrdering::SeqCst),
            0,
            "register 前は wake されない"
        );
        ticket.register_waker(waker);
        assert_eq!(
            count.load(AtomicOrdering::SeqCst),
            1,
            "register が ready を観測して即 wake する"
        );
    }

    #[test]
    fn audit_ticket_lost_wake_avoided_complete_after_register() {
        // (b) complete-after-register: register_waker を先に、complete を後に。
        let (ticket, completer) = new_audit_ticket();
        let (waker, count) = counting_waker();
        ticket.register_waker(waker);
        assert_eq!(
            count.load(AtomicOrdering::SeqCst),
            0,
            "complete 前は wake されない"
        );
        completer.complete(Ok(sample_ack()));
        assert_eq!(
            count.load(AtomicOrdering::SeqCst),
            1,
            "complete が保存済み waker を wake する"
        );
    }

    #[test]
    fn audit_ticket_complete_is_stored_only_once() {
        let (ticket, completer) = new_audit_ticket();
        completer.complete(Ok(sample_ack()));
        assert!(matches!(ticket.try_take(), Some(Ok(_))));
    }

    #[test]
    fn audit_waker_and_ticket_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AuditWaker>();
        assert_send_sync::<AuditTicket>();
        assert_send_sync::<AuditCompleter>();
    }

    // --- ScriptedAuditSink の Pending 系応答（§10.2 テスト facade）---

    #[test]
    fn scripted_pending_then_ack_pends_then_acks_on_resend() {
        let id = exec_id(9);
        let sink = ScriptedAuditSink::new([ScriptedResponse::PendingThenAck { pending_times: 2 }]);
        let batch: Arc<[AuditEnvelope]> = Arc::from(vec![started_envelope(id, 0)]);
        // 1 回目・2 回目は Pending（同一 batch の再送）。
        assert!(matches!(
            sink.submit(Arc::clone(&batch), AuditWaker::noop()),
            AuditSubmit::Pending(_)
        ));
        assert!(matches!(
            sink.submit(Arc::clone(&batch), AuditWaker::noop()),
            AuditSubmit::Pending(_)
        ));
        // 3 回目の再送で Ack。記録はこの 1 回だけ（Pending 中は記録しない）。
        match sink.submit(Arc::clone(&batch), AuditWaker::noop()) {
            AuditSubmit::Ack(ack) => {
                assert_eq!(ack.execution_id, id);
                assert_eq!(ack.through_sequence, 0);
            }
            other => panic!("Ack を期待したが {other:?}"),
        }
        assert_eq!(sink.len(), 1, "Ack 成立時に 1 回だけ記録");
    }

    #[test]
    fn scripted_pending_forever_never_acks() {
        let id = exec_id(9);
        let sink = ScriptedAuditSink::new([ScriptedResponse::PendingForever]);
        let batch: Arc<[AuditEnvelope]> = Arc::from(vec![started_envelope(id, 0)]);
        for _ in 0..5 {
            assert!(matches!(
                sink.submit(Arc::clone(&batch), AuditWaker::noop()),
                AuditSubmit::Pending(_)
            ));
        }
        assert!(sink.is_empty(), "永続 Pending は決して記録しない");
    }

    // --- R-1（§9.1）: HMAC-SHA-256 の既知応答・決定性と byte 推定据え置き ---

    /// RFC 4231 Test Case 2（key="Jefe", data="what do ya want for nothing?"）で HMAC-SHA-256 の
    /// 実装正しさを固定する。
    #[test]
    fn hmac_sha256_rfc4231_test_case_2() {
        let key = b"Jefe";
        let data = b"what do ya want for nothing?";
        let expected = [
            0x5b, 0xdc, 0xc1, 0x46, 0xbf, 0x60, 0x75, 0x4e, 0x6a, 0x04, 0x24, 0x26, 0x08, 0x95,
            0x75, 0xc7, 0x5a, 0x00, 0x3f, 0x08, 0x9d, 0x27, 0x39, 0x83, 0x9d, 0xec, 0x58, 0xb9,
            0x64, 0xec, 0x38, 0x43,
        ];
        assert_eq!(hmac_sha256(key, data), expected);
    }

    /// RFC 4231 Test Case 1（20-byte 0x0b key, data="Hi There")。短い key の 0 padding 経路。
    #[test]
    fn hmac_sha256_rfc4231_test_case_1() {
        let key = [0x0bu8; 20];
        let data = b"Hi There";
        let expected = [
            0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b,
            0xf1, 0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c,
            0x2e, 0x32, 0xcf, 0xf7,
        ];
        assert_eq!(hmac_sha256(&key, data), expected);
    }

    /// RFC 4231 Test Case 6（131-byte key、block size 超で key を hash する経路）。
    #[test]
    fn hmac_sha256_rfc4231_test_case_6_long_key() {
        let key = [0xaau8; 131];
        let data = b"Test Using Larger Than Block-Size Key - Hash Key First";
        let expected = [
            0x60, 0xe4, 0x31, 0x59, 0x1e, 0xe0, 0xb6, 0x7f, 0x0d, 0x8a, 0x26, 0xaa, 0xcb, 0xf5,
            0xb7, 0x7f, 0x8e, 0x0b, 0xc6, 0x21, 0x37, 0x28, 0xc5, 0x14, 0x05, 0x46, 0x04, 0x0f,
            0x0e, 0xe3, 0x7f, 0x54,
        ];
        assert_eq!(hmac_sha256(&key, data), expected);
    }

    /// R-AT-06 の下敷き: 同 key・同 label は同 digest、別 label は別 digest（決定性）。
    #[test]
    fn hmac_path_same_label_same_digest_different_label_differs() {
        let policy = RedactionPolicy::default();
        let a1 = policy.hmac_path("@default/a.txt");
        let a2 = policy.hmac_path("@default/a.txt");
        let b = policy.hmac_path("@default/b.txt");
        assert_eq!(a1, a2, "同じ label は同じ digest");
        assert_ne!(a1, b, "別 label は別 digest");
    }

    /// 既定 key が固定であることを確認（§7.1 乱数禁止・R-AT-06 の前提）。別インスタンス間で一致する。
    #[test]
    fn default_policy_hmac_key_is_fixed() {
        let p1 = RedactionPolicy::default();
        let p2 = RedactionPolicy::default();
        assert_eq!(p1.hmac_path("@default/x"), p2.hmac_path("@default/x"));
        assert_eq!(p1.policy_id, "default-r1");
    }

    /// R-AT-08: §9.1 の payload ツリーを載せても host-call lifecycle event の `estimate_event_bytes`
    /// が payload 非依存の 128 固定のまま据え置かれる（B-2 queue 会計の不変条件）。
    #[test]
    fn r_at_08_estimate_event_bytes_frozen_for_host_call_events() {
        let mut big_fields = BTreeMap::new();
        big_fields.insert(
            "path".to_string(),
            AuditField::HmacSha256 {
                class: AuditFieldClass::Path,
                digest: [0xabu8; 32],
                original_bytes: 4096,
            },
        );
        big_fields.insert(
            "arguments".to_string(),
            AuditField::Revealed {
                class: AuditFieldClass::Identifier,
                value: AuditValue::Sequence(vec![AuditField::Omitted {
                    class: AuditFieldClass::Body,
                    original_bytes: 9999,
                }]),
                original_bytes: 9999,
            },
        );
        let big_payload = AuditPayload { fields: big_fields };

        let started = AuditEvent::HostCallStarted {
            operation_id: 0,
            call_id: 0,
            function: "write_file".to_string(),
            request: big_payload.clone(),
            request_bytes: 4096,
        };
        let decision = AuditEvent::CapabilityDecision {
            operation_id: 0,
            call_id: Some(0),
            capability: "filesystem".to_string(),
            action: "write_file".to_string(),
            resource: big_payload.clone(),
            decision: CapabilityDecisionKind::Allow,
            rule_id: "mount-granted".to_string(),
        };
        let finished = AuditEvent::HostCallFinished {
            operation_id: 0,
            call_id: 0,
            outcome: HostCallOutcome::Success,
            response: big_payload,
            response_bytes: 0,
            error_code: None,
            effect_status: EffectStatus::Committed,
        };

        // host-call lifecycle 3 event は `_ => 128` 分岐で payload に依存せず 128 固定。
        assert_eq!(estimate_event_bytes(&started), 128);
        assert_eq!(estimate_event_bytes(&decision), 128);
        assert_eq!(estimate_event_bytes(&finished), 128);
    }
}
