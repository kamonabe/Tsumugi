//! Phase 1 embedding API（スライス E1）。
//!
//! [組み込みAPI仕様](../docs/embedding-api.md) 第3・8節が定義する識別子・設定・エラー・
//! terminal outcome 型と [`EngineBuilder`] を、追加判断なしに最終契約へ接続できる形で導入する。
//!
//! # スライス境界（E1）
//!
//! 本モジュールは E1 の完了条件（constructor・default・secret-free `Debug`）だけを満たす。
//! `compile` / `link` / `run` などの実行入口は後続スライス（E2〜E8a）で追加する。現行の
//! alpha facade（[`crate::engine`]）はそのまま残し、本モジュールと名前衝突しないよう
//! crate root では別名で公開する（仕様第13節の移行計画に従い、統合は E10 で行う）。
//!
//! # 意図的に E1 へ含めない範囲
//!
//! - [`EngineBuilder::host_functions`]（Phase 2、E7）は実装済み。build 済みの
//!   [`crate::host_function::HostFunctionRegistry`] を engine へ格納し、`Engine::run` が
//!   実行前に evaluator へ注入・実行後にクリアする（登録は engine 単位、grant は
//!   [`ExecutionRequest::with_capabilities`] で実行単位）。名前衝突 validation（[`ConfigError`]
//!   の `DuplicateCapability` / `DuplicateCallableName`）は registry build 時に
//!   [`crate::host_function::HostFunctionRegistryBuilder`] で済むため、engine builder は再検証
//!   しない。
//! - import graph 解決（`module_resolver`）と、それに紐づく `ExecutionOutcome::Denied` /
//!   `HostError` の terminal variant は引き続き保留（C6、ブロック中）。run 時の host function
//!   denial は catch 可能な `capability` error（未捕捉→`RuntimeError`）として正しく表出する。
//! - `SourceHash` の実際の計算（SHA-256）は root source を扱う `compile`（E2）で行う。E1 は
//!   32 byte の値型と constructor だけを用意し、ハッシュ依存クレートを持ち込まない。
//! - `BudgetUsage` を伴う terminal outcome の `usage` field は、有限 budget を公開する
//!   Phase 3（E11）で最終形にする。E1 の [`ExecutionOutcome`] は Phase 1 で到達し得る
//!   variant のみを持ち、`#[non_exhaustive]` で後方互換に将来拡張する。

use std::num::NonZeroU128;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::ErrorKind;

/// 実行 backend の選択（仕様第3節 `Backend`）。
///
/// `TreeWalk` が既定かつ規範 backend。`VmExperimental` は実験的で、
/// [`EngineBuilder::allow_experimental_backend`] で明示的に許可しない限り
/// [`EngineBuilder::build`] が [`ConfigError::ExperimentalBackendNotEnabled`] を返す。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Backend {
    /// ツリーウォーク評価器（既定・規範）。
    TreeWalk,
    /// バイトコード VM（実験的。conformance 完了まで stable backend ではない）。
    VmExperimental,
}

/// 言語仕様 revision（仕様第3節 `LanguageRevision`）。
///
/// 仕様文書の擬似コードは執筆時点の `V0_11` を例示するが、実装の現行 revision は 0.20 で
/// あるため、実装は現行の `V0_20` を持つ。番号体系は Cargo package version（0.1.0）とは
/// 独立に管理する（`docs/roadmap.md`「バージョン番号の扱い」）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum LanguageRevision {
    /// language-spec revision 0.19。
    V0_19,
    /// language-spec revision 0.20（現行）。`remove_dir` を空 directory のみへ変更し、再帰削除を
    /// 新規 builtin `remove_tree`（capability `RecursiveDelete`）へ分離した破壊的変更（REV-021）。
    V0_20,
}

impl LanguageRevision {
    /// 現行の言語 revision。
    pub const CURRENT: Self = Self::V0_20;

    /// revision の文字列表現（例: `"0.20"`）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V0_19 => "0.19",
            Self::V0_20 => "0.20",
        }
    }
}

/// process 内で衝突しない Engine 識別子（仕様第3節 `EngineId`）。
///
/// build ごとに非ゼロのランダム相当値を割り当てる。永続 identity には使わない。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct EngineId(u128);

impl EngineId {
    /// 内部の生値を返す（診断・照合用）。
    pub const fn get(self) -> u128 {
        self.0
    }

    /// process 内で単調増加する非ゼロ ID を発番する。
    ///
    /// 永続化・secret には使わない表示専用 identity である。process をまたいで
    /// 一意性を要求しない（仕様: 永続 identity には使わない）。
    fn allocate() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // 上位 64bit に固定の nonce を混ぜ、0 を避けつつ 128bit 空間へ広げる。
        Self(((0x5375_6d75_6769_0001u128) << 64) | (n as u128))
    }
}

/// 表示用の source 識別子（仕様第3節 `SourceId`）。
///
/// 1..=256 UTF-8 bytes、NUL なしを受理する。secret path や credential を入れてはならない。
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SourceId(String);

impl SourceId {
    /// source 識別子を検証して作る。
    ///
    /// 空・256 byte 超過・NUL 含みは [`ConfigError`] で拒否する。
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        validate_identifier("source_id", &value, 256)?;
        Ok(Self(value))
    }

    /// 識別子文字列を返す。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// source の内容ハッシュ（仕様第3節 `SourceHash`、SHA-256 の 32 bytes）。
///
/// E1 では 32 byte 値の器と constructor だけを用意する。実際の SHA-256 計算は
/// root source を扱う `compile`（E2）で行う。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SourceHash([u8; 32]);

impl SourceHash {
    /// 生の 32 byte ハッシュから作る。
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// 生の 32 byte 表現を返す。
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// 実行の識別子（仕様第3節 `ExecutionId`、非ゼロ）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ExecutionId(NonZeroU128);

impl ExecutionId {
    /// 非ゼロ値から作る。
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    /// 内部の非ゼロ値を返す。
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }
}

/// Engine の不変設定（仕様第3節 `EngineConfig`）。
///
/// `Debug` は手書きで、`audit_sink` の有無だけを出す（sink 本体は secret を保持し得るため
/// Debug へ出さない。`AuditSink` に `Debug` を要求しない設計、A-1 D2）。
#[derive(Clone)]
pub struct EngineConfig {
    /// 実行 backend。
    pub backend: Backend,
    /// 言語 revision。
    pub language_revision: LanguageRevision,
    /// 監査 sink（Phase 6 A-1、opt-in）。既定は `None`。
    ///
    /// `None` のときは監査を一切 emission せず、既存挙動と bit-identical。`Some` を
    /// [`EngineBuilder::audit_sink`] で設定すると、[`Engine::run`] が `ExecutionStarted` と
    /// `Terminal` を journal 経由で emission する（§8/§10）。§1.1 の sink 必須ポリシーは
    /// 後続スライスへ延期する（opt-in の逸脱、`docs/determinism-and-audit.md` §14 参照）。
    pub audit_sink: Option<Arc<dyn crate::audit::AuditSink>>,
}

impl Default for EngineConfig {
    /// 既定は `TreeWalk` + 現行 revision、監査 sink なし（opt-in）。
    ///
    /// budget は engine 単位ではなく [`ExecutionRequest`] が必須所有する（REV-015 最終形移行
    /// Slice 1、[実行制御仕様](../docs/execution-control.md) §2 不変条件6 / §3）。
    fn default() -> Self {
        Self {
            backend: Backend::TreeWalk,
            language_revision: LanguageRevision::CURRENT,
            audit_sink: None,
        }
    }
}

impl std::fmt::Debug for EngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // sink 本体は Debug へ出さない（secret-free）。有無だけを出す。
        f.debug_struct("EngineConfig")
            .field("backend", &self.backend)
            .field("language_revision", &self.language_revision)
            .field(
                "audit_sink",
                &if self.audit_sink.is_some() {
                    "configured"
                } else {
                    "none"
                },
            )
            .finish()
    }
}

/// Engine 構築・設定の検証エラー（仕様第3節 `ConfigError`）。
///
/// E1 の variant に加え、Phase 2（E7、slice C1〜）で使う capability / callable /
/// descriptor / filesystem policy 関連 variant を持つ。C1 では `DuplicateCapability` を
/// 使い、残る 3 variant は後続 slice（C5/C8）で使う。
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConfigError {
    /// 識別子が空。
    EmptyIdentifier {
        /// 対象 field 名。
        field: &'static str,
    },
    /// 識別子が最大 byte 数を超えた。
    IdentifierTooLong {
        /// 対象 field 名。
        field: &'static str,
        /// 許容する最大 byte 数。
        max_bytes: u16,
    },
    /// 識別子に NUL を含む。
    IdentifierContainsNul {
        /// 対象 field 名。
        field: &'static str,
    },
    /// 実験 backend が許可されていない。
    ExperimentalBackendNotEnabled,
    /// 監査 sink と `VmExperimental` backend を同時に設定した（Phase 6 A-1）。
    ///
    /// A-1 の run 経路には backend dispatch が無く、常に tree evaluator で実行する。
    /// この状態で `VmExperimental` label を監査へ載せると、tree 実行を VM 実行として
    /// 誤報告してしまう（A-1 の tree-only 境界違反）。VM の監査 wiring が入るまで、
    /// この組み合わせは build 時に拒否する。
    AuditSinkWithExperimentalBackend,
    /// 同一 [`CapabilityKind`](crate::capability::CapabilityKind) を二重に設定した
    /// （後勝ちにしない。仕様第3節）。host function grant は別 ID なら複数許可する。
    DuplicateCapability {
        /// 重複した authority 種別。
        kind: crate::capability::CapabilityKind,
    },
    /// callable 名が keyword / builtin / 既登録 host function と衝突する（AUD-049）。
    ///
    /// C8（`HostFunctionRegistry`）で使う。
    DuplicateCallableName {
        /// 衝突した公開名。
        name: String,
    },
    /// descriptor field が不正（arity / audit policy 等）。C8 で使う。
    InvalidDescriptor {
        /// 対象 field 名。
        field: &'static str,
        /// 機械可読の理由コード。
        code: &'static str,
    },
    /// filesystem policy が不正（mount / operation / symlink policy 等）。C5 で使う。
    InvalidFilesystemPolicy {
        /// 機械可読の理由コード。
        code: &'static str,
    },
}

/// 識別子（`SourceId` / `ModuleId` / `HostErrorCode` の共通制約）を検証する。
///
/// 空・最大 byte 超過・NUL 含みをそれぞれ対応する [`ConfigError`] で拒否する。
fn validate_identifier(
    field: &'static str,
    value: &str,
    max_bytes: u16,
) -> Result<(), ConfigError> {
    if value.is_empty() {
        return Err(ConfigError::EmptyIdentifier { field });
    }
    if value.contains('\0') {
        return Err(ConfigError::IdentifierContainsNul { field });
    }
    if value.len() > max_bytes as usize {
        return Err(ConfigError::IdentifierTooLong { field, max_bytes });
    }
    Ok(())
}

/// [`Engine`] を構築する builder（仕様第3節 `EngineBuilder`）。
///
/// E1 では config と実験 backend 許可だけを扱っていた。E7 で [`Self::host_functions`] を公開し、
/// build 済みの [`crate::host_function::HostFunctionRegistry`] を engine へ格納する。登録と grant
/// は別で（capability-model 第11.1節）、grant は実行ごとに
/// [`ExecutionRequest::with_capabilities`] で渡す。
#[derive(Debug)]
pub struct EngineBuilder {
    config: EngineConfig,
    allow_experimental_backend: bool,
    host_registry: std::sync::Arc<crate::host_function::HostFunctionRegistry>,
}

impl Default for EngineBuilder {
    /// [`Self::new`] と同じ既定の builder（`host_registry` が
    /// [`Arc<HostFunctionRegistry>`] のため derive できない）。
    fn default() -> Self {
        Self::new()
    }
}

impl EngineBuilder {
    /// 既定設定の builder を作る。
    pub fn new() -> Self {
        Self {
            config: EngineConfig::default(),
            allow_experimental_backend: false,
            // 既定は空 registry（host function なし）。run-path 挙動は従来と同一。
            host_registry: std::sync::Arc::new(crate::host_function::HostFunctionRegistry::empty()),
        }
    }

    /// Engine 設定を指定する。
    pub fn config(mut self, config: EngineConfig) -> Self {
        self.config = config;
        self
    }

    /// 実験 backend（`VmExperimental`）の使用を許可する。
    ///
    /// 許可しないまま `VmExperimental` を build すると
    /// [`ConfigError::ExperimentalBackendNotEnabled`] を返す。
    pub fn allow_experimental_backend(mut self, allow: bool) -> Self {
        self.allow_experimental_backend = allow;
        self
    }

    /// 監査 sink を設定する（Phase 6 A-1、opt-in、§10）。
    ///
    /// 設定すると [`Engine::run`] が `ExecutionStarted`（最初の semantic work の前）と
    /// `Terminal`（各 terminal commit 点）を journal 経由で emission する。設定しなければ
    /// 監査は一切起きず、既存挙動と bit-identical（§1.1 の sink 必須は後続スライスへ延期）。
    ///
    /// A-1 の run 経路は tree-only のため、`VmExperimental` backend と同時に設定したまま
    /// [`Self::build`] すると [`ConfigError::AuditSinkWithExperimentalBackend`] を返す。
    pub fn audit_sink(mut self, sink: Arc<dyn crate::audit::AuditSink>) -> Self {
        self.config.audit_sink = Some(sink);
        self
    }

    /// build 済みの host function registry を engine へ登録する（Phase 2、E7、第11.1節）。
    ///
    /// registry は engine 単位の build-time 設定（link 時に name→ID を固定する）であり、実行ごとに
    /// 変わる grant（[`crate::capability::CapabilitySet`]）とは別の軸である。名前衝突・descriptor
    /// validation（[`ConfigError::DuplicateCallableName`] を含む）は
    /// [`crate::host_function::HostFunctionRegistryBuilder`] の `register`/`build` で既に済んでいる
    /// ため、ここでは再検証しない（したがって [`Self::build`] の signature は不変）。
    pub fn host_functions(mut self, registry: crate::host_function::HostFunctionRegistry) -> Self {
        self.host_registry = std::sync::Arc::new(registry);
        self
    }

    /// 設定を検証して [`Engine`] を build する。
    ///
    /// `VmExperimental` を選びつつ許可していない場合は
    /// [`ConfigError::ExperimentalBackendNotEnabled`] を返す。監査 sink を `VmExperimental`
    /// backend と同時に設定した場合は [`ConfigError::AuditSinkWithExperimentalBackend`] を返す
    /// （A-1 の run 経路は tree-only のため。§7.1）。host function registry の validation は
    /// registry build 時に済んでいるため、ここでは再検証しない。
    pub fn build(self) -> Result<Engine, ConfigError> {
        if self.config.backend == Backend::VmExperimental && !self.allow_experimental_backend {
            return Err(ConfigError::ExperimentalBackendNotEnabled);
        }
        // Phase 6 A-1: 監査 sink は tree-only の run 経路だけが対象。VmExperimental と組むと
        // tree 実行を VM として監査報告してしまうため、VM の監査 wiring が入るまで拒否する。
        if self.config.audit_sink.is_some() && self.config.backend == Backend::VmExperimental {
            return Err(ConfigError::AuditSinkWithExperimentalBackend);
        }
        Ok(Engine {
            id: EngineId::allocate(),
            config: self.config,
            host_registry: self.host_registry,
        })
    }
}

/// Phase 1 embedding の Engine（仕様第3節 `Engine`）。
///
/// build 後は不変。E1 では identity と config だけを公開し、`compile` / `link` などの
/// 実行入口は後続スライス（E2〜）で追加する。現行 alpha facade の
/// [`crate::engine::Engine`] とは別型で、crate root では [`crate::EmbeddingEngine`] として
/// 公開する。
#[derive(Debug)]
pub struct Engine {
    id: EngineId,
    config: EngineConfig,
    /// [`EngineBuilder::host_functions`] で登録した registry（既定は空、E7）。実行時に
    /// `Engine::run` が evaluator へ注入する。
    host_registry: std::sync::Arc<crate::host_function::HostFunctionRegistry>,
}

impl Engine {
    /// builder を作る（`Engine::builder().build()`）。
    pub fn builder() -> EngineBuilder {
        EngineBuilder::new()
    }

    /// この Engine の process 内一意 ID を返す。
    pub fn id(&self) -> EngineId {
        self.id
    }

    /// この Engine の不変設定を返す。
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }
}

/// スタックトレースの1フレーム（仕様第8節 `TraceFrame`）。
///
/// crate 内部の [`crate::error::TraceFrame`]（`name: String` / `line: usize`）とは別の、
/// 公開 embedding surface 用の型。`line` は不明なら `None`。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceFrame {
    /// 関数名（トップレベルは実装の慣習に従う）。
    pub function: String,
    /// 呼び出し元の行番号。不明なら `None`。
    pub line: Option<u32>,
}

/// スクリプト実行中の runtime error（仕様第8節 `ExecutionError`）。
///
/// `code` は [`crate::error::ErrorKind`] を唯一の正本として再利用する（backend ごとの
/// 縮小 enum を作らない）。`safe_message` には secret・絶対パス・native backtrace・
/// panic payload を入れない。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionError {
    /// canonical なエラー種別（正本は [`ErrorKind`]）。
    pub code: ErrorKind,
    /// secret を含まない表示用メッセージ。
    pub safe_message: String,
    /// 発生行。不明なら `None`。
    pub line: Option<u32>,
    /// スタックトレース。
    pub trace: Vec<TraceFrame>,
}

/// host error の code（仕様第8節 `HostErrorCode`）。
///
/// ASCII lowercase `[a-z][a-z0-9_.-]{0,63}` だけを受理する。
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct HostErrorCode(String);

impl HostErrorCode {
    /// code を検証して作る。
    ///
    /// 先頭が ASCII lowercase 英字で、以降が `[a-z0-9_.-]`、全体 1..=64 byte の場合のみ受理する。
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        let field = "host_error_code";
        if value.is_empty() {
            return Err(ConfigError::EmptyIdentifier { field });
        }
        if value.len() > 64 {
            return Err(ConfigError::IdentifierTooLong {
                field,
                max_bytes: 64,
            });
        }
        let mut chars = value.chars();
        let first = chars.next().expect("non-empty checked above");
        let head_ok = first.is_ascii_lowercase();
        let tail_ok = chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'));
        if !head_ok || !tail_ok {
            return Err(ConfigError::IdentifierContainsNul { field });
        }
        Ok(Self(value))
    }

    /// code 文字列を返す。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// host boundary の失敗（仕様第8節 `HostError`）。
///
/// `safe_message` に secret・絶対パス・backtrace を入れない。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostError {
    /// host error の code。
    pub code: HostErrorCode,
    /// secret を含まない表示用メッセージ。
    pub safe_message: String,
    /// 再試行可能か。
    pub retryable: bool,
}

/// スクリプト実行の terminal outcome（仕様第8節 `ExecutionOutcome`）。
///
/// Phase 1〜Phase 3（E11）で到達し得る variant を持つ。各 variant は仕様第6・12節どおり
/// `usage: BudgetUsage` を同梱し、terminal 時点の予算使用量 snapshot を公開する。仕様の他
/// terminal（`Denied` / `HostError` / `AuditFailure` / `RecordFailure` / `ReplayMismatch` /
/// `LinkError`）は、対応する機構が入る後続 Phase で追加する。`#[non_exhaustive]` により
/// variant 追加を breaking にしない。
///
/// `usage` はいずれの経路でも commit / rollback 後の値を載せる（total budget は単調で、
/// live heap だけ解放で減少し得る。仕様第2節 不変条件）。実行を1命令も試みない terminal
/// （precondition 違反による `InternalFailure` や pre-run cancel）は既定の空 usage を持つ。
///
/// 現行 alpha facade の [`crate::engine::ExecutionOutcome`] とは別型で、crate root では
/// [`crate::EmbeddingOutcome`] として公開する。
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ExecutionOutcome {
    /// スクリプトが最後まで実行された。v1 の返値は常に空（top-level 返値は将来拡張）。
    Completed {
        /// terminal 時点の予算使用量 snapshot（仕様第6・12節）。
        usage: BudgetUsage,
    },
    /// スクリプトが `exit(code)` で終了した（Phase 2 C7、REV-023）。
    ///
    /// ProcessExit capability を grant された実行だけがこの terminal に到達する。`Completed`
    /// と同じく全 language-state を commit する（仕様第10節 規則5）。script からは catch でき
    /// ない。
    Exited {
        /// スクリプトが指定した終了コード（0..=255）。
        code: u8,
        /// terminal 時点の予算使用量 snapshot（仕様第6・12節）。
        usage: BudgetUsage,
    },
    /// スクリプト実行中に未捕捉 runtime error で終了した。
    RuntimeError {
        /// canonical な runtime error。
        error: ExecutionError,
        /// terminal 時点の予算使用量 snapshot（仕様第6・12節）。
        usage: BudgetUsage,
    },
    /// 実行予算（fuel / heap / string / source / I-O 等）の上限を超過して停止した
    /// （REV-015 E11、仕様第8節）。
    ///
    /// script からは catch できない catch 不能 terminal で、`RuntimeError` には畳まない。
    /// 超過した resource は [`ExecutionError::code`] の `ErrorKind`（`StepLimit` /
    /// `CollectionLimit` / `StringLimit` / `SourceLimit` / `HeapLimit` / `IoLimit`）で分かる。
    /// language-state は開始時点へ rollback される。`failure` の構造化 `BudgetExceeded` 形は
    /// 後続で追加する。
    BudgetExceeded {
        /// 超過した予算 resource を示す canonical error。
        error: ExecutionError,
        /// terminal 時点の予算使用量 snapshot（仕様第6・12節）。
        usage: BudgetUsage,
    },
    /// 実行 deadline を超過して停止した（REV-015 E11、仕様第8節）。
    ///
    /// [`ExecutionRequest::new`] に渡した clock が、その request が所有する `budget.deadline` に
    /// 達したときに到達する catch 不能 terminal。script からは catch できず、language-state は
    /// 開始時点へ rollback される。
    DeadlineExceeded {
        /// terminal 時点の予算使用量 snapshot（仕様第6・12節）。
        usage: BudgetUsage,
    },
    /// 実行が協調的に cancel された（REV-015 E11、仕様第7・8節）。
    ///
    /// [`ExecutionRequest`] が所有する [`CancellationToken`](crate::budget::CancellationToken)
    /// （[`ExecutionRequest::cancellation`] で設定）を実行中に
    /// [`CancellationToken::cancel`](crate::budget::CancellationToken::cancel) した場合、または
    /// 実行前に cancel 済みの token を載せた場合（pre-run cancel、命令0）に到達する catch 不能
    /// terminal。language-state は開始時点へ rollback される。
    Cancelled {
        /// terminal 時点の予算使用量 snapshot（仕様第6・12節）。
        usage: BudgetUsage,
    },
    /// engine / host callback の panic を捕捉した内部障害（secret を含まない）。
    InternalFailure {
        /// 相関用の非ゼロ fault ID。
        fault_id: u128,
        /// secret を含まない表示用メッセージ。
        safe_message: String,
        /// terminal 時点の予算使用量 snapshot（仕様第6・12節）。
        usage: BudgetUsage,
    },
}

// ===========================================================================
// スライス E2: compile / import なし link / byte-level hash
// ===========================================================================

use std::panic::{AssertUnwindSafe, UnwindSafe};
use std::sync::Arc;

use crate::ast::{Program, Stmt};
use crate::error::TsumugiError;
use crate::lexer::Lexer;
use crate::parser::Parser;

/// compile 対象の root source（仕様第4節 `Source`）。
pub struct Source<'a> {
    /// 表示用 source 識別子。
    pub id: SourceId,
    /// source 本文（UTF-8）。
    pub text: &'a str,
}

impl<'a> Source<'a> {
    /// root source を作る。
    pub fn new(id: SourceId, text: &'a str) -> Self {
        Self { id, text }
    }
}

/// compile の挙動オプション（仕様第4節 `CompileOptions`）。
///
/// `Default` の `retain_source` は `false`（line map と診断に必要な位置以外は source 本文を
/// 保持しない）。
#[derive(Clone, Debug, Default)]
pub struct CompileOptions {
    /// source 本文を保持するか。既定は `false`。
    pub retain_source: bool,
}

/// compile 診断の分類（仕様第4節 `CompileDiagnosticCode`）。
///
/// 現行 pipeline は lex エラーを parser が `Parse` として surface するため、E2 では
/// AST 深度超過を [`Self::AstDepth`]、それ以外の lex/parse エラーを [`Self::Parse`] に
/// 分類する。`Backend`（VM compile）と `InternalFault`（compile 中 panic）は後続スライスで
/// 使う。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileDiagnosticCode {
    /// 字句解析エラー。
    Lex,
    /// 構文解析エラー。
    Parse,
    /// AST ネスト深度の超過。
    AstDepth,
    /// backend compile（VM）エラー。
    Backend,
    /// compile 中の内部障害。
    InternalFault,
}

/// 単一の compile 診断（仕様第4節 `CompileDiagnostic`）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompileDiagnostic {
    /// 診断の分類。
    pub code: CompileDiagnosticCode,
    /// 発生行。不明なら `None`。
    pub line: Option<u32>,
    /// 発生列。現行 parser は列を追跡しないため常に `None`。
    pub column: Option<u32>,
    /// secret を含まない表示用メッセージ。
    pub safe_message: String,
}

/// compile 失敗（仕様第4節 `CompileErrors`）。診断は常に1件以上、source 位置順。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompileErrors {
    /// 1件以上の診断（source 位置順）。
    pub diagnostics: Vec<CompileDiagnostic>,
}

/// パース済みで実行可能な Tsumugi スクリプト（仕様第4節 `CompiledScript`）。
///
/// `Arc` 共有で `Send + Sync`。同 engine / revision / backend で再利用できる。E2 では
/// `retain_source=false` のとき source 本文を保持しない（AST と source hash のみ持つ）。
#[derive(Clone)]
pub struct CompiledScript(Arc<CompiledScriptInner>);

struct CompiledScriptInner {
    engine_id: EngineId,
    source_id: SourceId,
    source_hash: SourceHash,
    language_revision: LanguageRevision,
    backend: Backend,
    /// root source に top-level import 文が1件以上あるか（Phase 1 の link 判定に使う）。
    ///
    /// [`CompiledScript`] は `Send + Sync` 契約（EMB-AT-02）を満たす必要がある。現行 AST は
    /// `Block = Rc<[Stmt]>`（REV-015 Slice 3 PR-d）で `!Send`/`!Sync` のため、runnable AST を
    /// この handle へ保持しない。E3 の実行入口は、実行スレッド上で保持 source を再 parse して
    /// runnable な `Program` を再構築する（案 A。設計判断の根拠と却下した案 B は
    /// [組み込みAPI仕様](../docs/embedding-api.md) 第4.1節を正本とする）。import の有無だけは
    /// link 判定に必要なので `bool` に畳んで持つ。
    has_imports: bool,
    /// root source の top-level import specifier を出現順に収集したもの（specifier と行番号）。
    ///
    /// compile 時に AST の `Stmt::Import { path, line }` を走査して収集する（再 parse /
    /// `retain_source` 非依存、設計 §4.3 step 2）。link 層（C6-c）が module resolver へ渡す
    /// root import 列として使う。
    import_specifiers: Vec<(String, usize)>,
    /// `retain_source=true` のとき保持する source 本文。
    ///
    /// E3 の実行入口はこの本文を再 parse して実行する。したがって
    /// [`CompiledScript`] を実行するには `retain_source=true` で compile しておく必要がある
    /// （案 A の明示契約。原則2「明示 > 暗黙」に沿う）。
    retained_source: Option<String>,
}

impl CompiledScript {
    /// この script を作った engine の ID。
    pub fn engine_id(&self) -> EngineId {
        self.0.engine_id
    }

    /// source 識別子。
    pub fn source_id(&self) -> &SourceId {
        &self.0.source_id
    }

    /// source 内容ハッシュ（SHA-256）。
    pub fn source_hash(&self) -> SourceHash {
        self.0.source_hash
    }

    /// 言語 revision。
    pub fn language_revision(&self) -> LanguageRevision {
        self.0.language_revision
    }

    /// backend。
    pub fn backend(&self) -> Backend {
        self.0.backend
    }

    /// 保持している source 本文（`retain_source=true` のときのみ `Some`）。
    pub fn retained_source(&self) -> Option<&str> {
        self.0.retained_source.as_deref()
    }

    /// root source に top-level import 文があるか（link 判定用、crate 内部）。
    pub(crate) fn has_imports(&self) -> bool {
        self.0.has_imports
    }

    /// root source の top-level import specifier（出現順、crate 内部）。link 層が解決に使う。
    pub(crate) fn import_specifiers(&self) -> &[(String, usize)] {
        &self.0.import_specifiers
    }
}

impl std::fmt::Debug for CompiledScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // source 本文・AST を Debug へ出さない（secret-free）。identity だけを見せる。
        f.debug_struct("CompiledScript")
            .field("engine_id", &self.0.engine_id)
            .field("source_id", &self.0.source_id)
            .field("source_hash", &self.0.source_hash)
            .field("language_revision", &self.0.language_revision)
            .field("backend", &self.0.backend)
            .finish_non_exhaustive()
    }
}

/// import module の識別子（仕様第5節 `ModuleId`）。1..=1024 UTF-8 bytes、NUL なし。
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ModuleId(String);

impl ModuleId {
    /// module 識別子を検証して作る。
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        validate_identifier("module_id", &value, 1024)?;
        Ok(Self(value))
    }

    /// 識別子文字列を返す。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// import graph の1ノード（仕様第5節 `ImportNode`）。
#[derive(Clone, Debug)]
pub struct ImportNode {
    /// module ID。
    pub module_id: ModuleId,
    /// この module の source hash。
    pub source_hash: SourceHash,
    /// この module が import する module（source 内の出現順）。
    pub imports: Vec<ModuleId>,
}

/// import graph（仕様第5節 `ImportGraph`）。
#[derive(Clone, Debug)]
pub struct ImportGraph {
    /// root source の hash。
    pub root: SourceHash,
    /// root source が import する module（出現順）。
    pub root_imports: Vec<ModuleId>,
    /// 各ノード（module_id の UTF-8 byte 列昇順）。
    pub nodes: Vec<ImportNode>,
    /// graph の byte-level hash（§5.1）。
    pub graph_hash: SourceHash,
}

/// link 済みで実行可能なスクリプト（仕様第5節 `LinkedScript`）。
#[derive(Clone)]
pub struct LinkedScript(Arc<LinkedScriptInner>);

struct LinkedScriptInner {
    root: CompiledScript,
    import_graph: ImportGraph,
    script_hash: SourceHash,
}

impl LinkedScript {
    /// root の `CompiledScript`。
    pub fn root(&self) -> &CompiledScript {
        &self.0.root
    }

    /// import graph。
    pub fn import_graph(&self) -> &ImportGraph {
        &self.0.import_graph
    }

    /// linked script の byte-level hash（§5.1）。
    pub fn script_hash(&self) -> SourceHash {
        self.0.script_hash
    }
}

impl std::fmt::Debug for LinkedScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkedScript")
            .field("root", &self.0.root)
            .field("script_hash", &self.0.script_hash)
            .finish_non_exhaustive()
    }
}

/// link 要求（仕様第5節 `LinkRequest`）。
///
/// Phase 2（C6/E7）で import 解決を Engine API へ配線した最終形。resolver 相関用の
/// `operation_id`、import 解決の authority を持つ `capabilities`、link 時 budget 器
/// （`budget`）、link 開始前 cancel 観測用の `cancellation` を持つ。
#[derive(Clone)]
pub struct LinkRequest {
    /// resolver へ渡す相関 ID。
    pub operation_id: ExecutionId,
    /// import 解決の authority（`module_resolver` を含み得る）。
    pub capabilities: crate::capability::CapabilitySet,
    /// link 時 budget 器（Phase 2 は remaining-bytes getter の器のみ、N 境界は Phase 3）。
    pub budget: crate::budget::BudgetConfig,
    /// link 開始前・各 resolve 前に観測する cancel token（§4.3 step 0）。
    pub cancellation: crate::budget::CancellationToken,
}

impl LinkRequest {
    /// link 要求を作る（仕様第5節の 3 引数 `new`）。cancellation は never-cancel token を既定とする。
    pub fn new(
        operation_id: ExecutionId,
        capabilities: crate::capability::CapabilitySet,
        budget: crate::budget::BudgetConfig,
    ) -> Self {
        Self {
            operation_id,
            capabilities,
            budget,
            cancellation: crate::budget::CancellationToken::new(),
        }
    }

    /// cancellation token を差し替える（補助 builder、設計 §4.1）。
    ///
    /// 仕様第5節の 3 引数 `new` に無い additive な public API。CLI/test はこの builder で
    /// cancellation を注入し、`run` 経路と同じ token インスタンスを共有させる（§4.6）。
    pub fn with_cancellation(mut self, token: crate::budget::CancellationToken) -> Self {
        self.cancellation = token;
        self
    }
}

impl std::fmt::Debug for LinkRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // capability 本体・budget 値は出さず identity と有無だけを見せる（secret-free）。
        f.debug_struct("LinkRequest")
            .field("operation_id", &self.operation_id)
            .field("capabilities", &self.capabilities.id())
            .finish_non_exhaustive()
    }
}

/// link 失敗（仕様第5節 `LinkError`）。
///
/// Phase 2（C6/E7）で import 解決を配線した最終形の全 variant を持つ。`#[non_exhaustive]`
/// により variant 追加を breaking にしない。
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LinkError {
    /// 別 engine で作られた `CompiledScript` を link しようとした。
    EngineMismatch,
    /// revision が一致しない。
    RevisionMismatch,
    /// backend が一致しない。
    BackendMismatch,
    /// 現在の Phase で未提供の機能を要求した。
    ///
    /// C6-c 後は import を理由にこの variant を返すことはない（import あり + resolver 未 grant は
    /// [`LinkError::Denied`]、engine/revision/backend 不一致は各 Mismatch）。最終形にも保持するが
    /// 実質的に到達しない保持 variant（設計 §4.2・§4.6）。
    FeatureUnavailable {
        /// 未提供の機能名。
        feature: &'static str,
    },
    /// link 中に host boundary で panic を捕捉した内部障害（第11節）。
    ///
    /// panic payload / native backtrace は含まず、相関用の非ゼロ fault ID と secret を
    /// 含まない表示用メッセージだけを持つ。
    InternalFailure {
        /// 相関用の非ゼロ fault ID。
        fault_id: u128,
        /// secret を含まない表示用メッセージ。
        safe_message: String,
    },
    /// import があるのに authority（`module_resolver`）が無い等、capability 不足で拒否した
    /// terminal（§4.5、capability-model §13）。script からは catch できない。
    Denied(crate::capability::Denial),
    /// resolver が解決に失敗した／malformed specifier／内部 resolver fault 等の host boundary 失敗。
    Resolve(HostError),
    /// 解決済み module の UTF-8 不正 / 単体 compile 失敗（§4.4）。
    InvalidModule {
        /// 不正だった module の ID。
        module: ModuleId,
        /// module compile 診断（1 件以上、source 位置順）。
        diagnostics: CompileErrors,
    },
    /// import の循環を検出した。`chain` は循環の閉路（起点を末尾に再掲、§4.4）。
    Cycle {
        /// 循環を構成する module ID 列（起点を末尾に再掲）。
        chain: Vec<ModuleId>,
    },
    /// import の深度が上限を超えた（§4.3 step 6）。
    DepthExceeded {
        /// 深度上限（`MAX_IMPORT_DEPTH`）。
        limit: u32,
    },
    /// link 時 budget 超過（Phase 3 で実効化。器としての variant）。
    BudgetExceeded(crate::budget::BudgetExceeded),
    /// link 時 deadline 超過。
    DeadlineExceeded,
    /// link 開始前・解決中に cancel された（§4.3 step 0）。
    Cancelled,
    /// backend 固有の実行失敗。
    Backend(ExecutionError),
}

impl Engine {
    /// root source を lex / parse / backend compile して再利用可能な [`CompiledScript`] を作る
    /// （仕様第4節）。
    ///
    /// import 先・filesystem・environment・clock・stdio・host function を一切呼ばない。
    /// 失敗時は source 位置順に1件以上の [`CompileDiagnostic`] を返す。
    pub fn compile(
        &self,
        source: Source<'_>,
        options: &CompileOptions,
    ) -> Result<CompiledScript, CompileErrors> {
        // lexer / parser / backend の unwind panic を host boundary で捕捉する（第11節 規則1）。
        // compile 中の panic は `CompileDiagnosticCode::InternalFault` へ写す（規則2）。panic
        // payload / native backtrace は公開診断へ含めない（規則3）。E7 以降 `&Engine` は
        // `Arc<dyn HostFunction>` を含み自動 `UnwindSafe` ではないため、panic を上記のとおり
        // 内部診断へ写して漏らさないことで `AssertUnwindSafe` を正当化する。
        catch_host_unwind(AssertUnwindSafe(|| self.compile_inner(source, options))).unwrap_or_else(
            |fault_id| {
                Err(CompileErrors {
                    diagnostics: vec![CompileDiagnostic {
                        code: CompileDiagnosticCode::InternalFault,
                        line: None,
                        column: None,
                        safe_message: internal_fault_message(fault_id),
                    }],
                })
            },
        )
    }

    fn compile_inner(
        &self,
        source: Source<'_>,
        options: &CompileOptions,
    ) -> Result<CompiledScript, CompileErrors> {
        let tokens = Lexer::new(source.text).tokenize();
        let program = Parser::new(tokens)
            .parse()
            .map_err(|errors| CompileErrors {
                diagnostics: errors.iter().map(compile_diagnostic_from).collect(),
            })?;

        let source_hash = SourceHash::from_bytes(hash::sha256(source.text.as_bytes()));
        // top-level import specifier を出現順に収集する（設計 §4.3 step 2、再 parse 非依存）。
        let import_specifiers: Vec<(String, usize)> = program
            .iter()
            .filter_map(|stmt| match stmt {
                Stmt::Import { path, line } => Some((path.clone(), *line)),
                _ => None,
            })
            .collect();
        let has_imports = !import_specifiers.is_empty();

        Ok(CompiledScript(Arc::new(CompiledScriptInner {
            engine_id: self.id(),
            source_id: source.id.clone(),
            source_hash,
            language_revision: self.config().language_revision,
            backend: self.config().backend,
            has_imports,
            import_specifiers,
            retained_source: options.retain_source.then(|| source.text.to_string()),
        })))
    }

    /// [`CompiledScript`] を link して [`LinkedScript`] を作る（仕様第5節）。
    ///
    /// engine ID → revision → backend の順に検証し、不一致なら対応する [`LinkError`] を返す。
    /// import があれば `request.capabilities` の `module_resolver` で解決する（Phase 2、C6/E7）。
    /// resolver 未 grant + import あり は resolver call 0 の terminal [`LinkError::Denied`]。
    /// import 0 件は空 graph の [`LinkedScript`] を返す。
    pub fn link(
        &self,
        script: &CompiledScript,
        request: LinkRequest,
    ) -> Result<LinkedScript, LinkError> {
        // linker の unwind panic を host boundary で捕捉する（第11節 規則1）。link 中の panic は
        // `LinkError::InternalFailure` へ写す（規則2）。payload / backtrace は含めない（規則3）。
        // E7 で `Engine` が `Arc<dyn HostFunction>` を保持したため `&Engine` は自動 `UnwindSafe`
        // ではなくなった。panic は上記のとおり InternalFailure へ写して以後の実行へ漏らさないので、
        // run 入口と同じく `AssertUnwindSafe` で正当化する。
        catch_host_unwind(AssertUnwindSafe(|| self.link_inner(script, request))).unwrap_or_else(
            |fault_id| {
                Err(LinkError::InternalFailure {
                    fault_id,
                    safe_message: internal_fault_message(fault_id),
                })
            },
        )
    }

    fn link_inner(
        &self,
        script: &CompiledScript,
        request: LinkRequest,
    ) -> Result<LinkedScript, LinkError> {
        if script.engine_id() != self.id() {
            return Err(LinkError::EngineMismatch);
        }
        if script.language_revision() != self.config().language_revision {
            return Err(LinkError::RevisionMismatch);
        }
        if script.backend() != self.config().backend {
            return Err(LinkError::BackendMismatch);
        }

        // step 0: engine/revision/backend 検証直後・resolver 取得より前に cancel を観測する
        // （resolver call 0 契約、設計 §4.3 step 0）。
        if request.cancellation.is_cancelled() {
            return Err(LinkError::Cancelled);
        }

        let root_hash = script.source_hash();

        // step 1: resolver capability を取得する。import があるのに resolver 未 grant なら
        // resolver を一度も呼ばず terminal Denied（CAP-AT-16、設計 §4.5）。
        let resolver = match request.capabilities.module_resolver() {
            Some(resolver) => resolver,
            None => {
                if script.has_imports() {
                    return Err(LinkError::Denied(resolver_absent_denial()));
                }
                // import 0 件 + resolver 無しは従来どおり空 graph。
                return Ok(self.empty_linked_script(script, root_hash));
            }
        };

        // import 0 件なら resolver があっても空 graph（resolver は呼ばない）。
        if !script.has_imports() {
            return Ok(self.empty_linked_script(script, root_hash));
        }

        // step 2〜8: import を解決して graph を構築する。
        let resolution = self.resolve_import_graph(script, resolver.as_ref(), &request)?;

        let revision = script.language_revision();
        let root_imports_str: Vec<&str> = resolution
            .root_imports
            .iter()
            .map(ModuleId::as_str)
            .collect();
        let nodes_encoded: Vec<(&str, SourceHash, Vec<&str>)> = resolution
            .nodes
            .iter()
            .map(|node| {
                (
                    node.module_id.as_str(),
                    node.source_hash,
                    node.imports.iter().map(ModuleId::as_str).collect(),
                )
            })
            .collect();
        let nodes_ref: Vec<(&str, SourceHash, &[&str])> = nodes_encoded
            .iter()
            .map(|(id, hash, imports)| (*id, *hash, imports.as_slice()))
            .collect();
        let graph_hash = SourceHash::from_bytes(hash::import_graph_hash(
            revision,
            root_hash,
            &root_imports_str,
            &nodes_ref,
        ));

        let import_graph = ImportGraph {
            root: root_hash,
            root_imports: resolution.root_imports,
            nodes: resolution.nodes,
            graph_hash,
        };

        let script_hash =
            SourceHash::from_bytes(hash::linked_script_hash(revision, root_hash, graph_hash));

        Ok(LinkedScript(Arc::new(LinkedScriptInner {
            root: script.clone(),
            import_graph,
            script_hash,
        })))
    }

    /// import 0 件の空 graph を持つ [`LinkedScript`] を作る。
    fn empty_linked_script(&self, script: &CompiledScript, root_hash: SourceHash) -> LinkedScript {
        let revision = script.language_revision();
        let graph_hash =
            SourceHash::from_bytes(hash::import_graph_hash(revision, root_hash, &[], &[]));
        let import_graph = ImportGraph {
            root: root_hash,
            root_imports: Vec::new(),
            nodes: Vec::new(),
            graph_hash,
        };
        let script_hash =
            SourceHash::from_bytes(hash::linked_script_hash(revision, root_hash, graph_hash));
        LinkedScript(Arc::new(LinkedScriptInner {
            root: script.clone(),
            import_graph,
            script_hash,
        }))
    }

    /// root から BFS/DFS で import を解決し、`root_imports` と `nodes` を構築する
    /// （設計 §4.3 step 3〜8）。cycle / depth / 同 ID・異 hash / UTF-8 / module compile を検証する。
    fn resolve_import_graph(
        &self,
        script: &CompiledScript,
        resolver: &dyn crate::capability::ModuleResolver,
        request: &LinkRequest,
    ) -> Result<ResolvedGraph, LinkError> {
        use std::collections::HashMap;

        let revision = script.language_revision();
        let mut call_id: u64 = 0;

        // 解決済み module → source_hash（二重展開防止と同 ID/異 hash 判定、§4.4）。
        let mut resolved: HashMap<ModuleId, SourceHash> = HashMap::new();
        // 構築済み node（module_id をキーに重複構築を避ける）。
        let mut node_map: HashMap<ModuleId, ImportNode> = HashMap::new();

        // root の import を正規化して root_imports を作る（§4.3 step 2 の収集 + 正規化）。
        let mut root_imports: Vec<ModuleId> = Vec::new();
        // DFS フレーム: (解決する module ID, その specifier, depth)。root import は importer=None。
        // active スタック上の循環検出のため、再帰を手続きの明示 stack で表す。
        self.resolve_module_imports(
            resolver,
            request,
            revision,
            None,
            script.import_specifiers(),
            0,
            &mut call_id,
            &mut resolved,
            &mut node_map,
            &mut Vec::new(),
            &mut root_imports,
        )?;

        // nodes を module_id の UTF-8 昇順で並べる（§4.3 step 8、hash encoding 契約）。
        let mut nodes: Vec<ImportNode> = node_map.into_values().collect();
        nodes.sort_by(|a, b| a.module_id.as_str().cmp(b.module_id.as_str()));

        Ok(ResolvedGraph {
            root_imports,
            nodes,
        })
    }

    /// `importer` が出した import 群（specifier 列）を順に解決する再帰ヘルパ。
    ///
    /// `active` は現在 DFS で訪問中の module ID スタック（循環検出用）。`out_imports` には、
    /// この呼び出しが解決した各 import の正規化済み `ModuleId` を出現順に積む（importer の
    /// `imports` 列 / root の `root_imports` 列になる）。
    #[allow(clippy::too_many_arguments)]
    fn resolve_module_imports(
        &self,
        resolver: &dyn crate::capability::ModuleResolver,
        request: &LinkRequest,
        revision: LanguageRevision,
        importer: Option<&ModuleId>,
        specifiers: &[(String, usize)],
        depth: usize,
        call_id: &mut u64,
        resolved: &mut std::collections::HashMap<ModuleId, SourceHash>,
        node_map: &mut std::collections::HashMap<ModuleId, ImportNode>,
        active: &mut Vec<ModuleId>,
        out_imports: &mut Vec<ModuleId>,
    ) -> Result<(), LinkError> {
        for (specifier, _line) in specifiers {
            // 各 resolve 前に cancel を観測する（§4.3 step 0 の loop 内確認）。
            if request.cancellation.is_cancelled() {
                return Err(LinkError::Cancelled);
            }

            // specifier を parse / 正規化する（malformed / mount は 2 channel 分離）。
            let (module_id, normalized) = self.normalize_specifier(specifier, resolver)?;

            // この import の ID を importer / root の imports 列へ記録する（出現順）。
            out_imports.push(module_id.clone());

            // 循環検出: active に既在なら Cycle（起点を末尾に再掲、§4.4）。
            if let Some(pos) = active.iter().position(|m| m == &module_id) {
                let mut chain: Vec<ModuleId> = active[pos..].to_vec();
                chain.push(module_id.clone());
                return Err(LinkError::Cycle { chain });
            }

            // 深度境界（§4.3 step 6）。import を depth 段降りた時点が MAX に達したら拒否する。
            // 既解決の dedup は resolve 後（同 ID/異 hash 判定と同じ地点）で行う（§4.3 step 4）。
            // ただし既解決 module は再展開しない契約なので、深度は未解決 module へ降りるときだけ
            // 消費させる判断が要る。ここでは「resolve → hash 判定 → 既解決なら skip」の順で統一し、
            // 深度境界は未解決 module の子を降りる再帰呼び出し（depth+1）で評価される。
            if depth >= crate::limits::MAX_IMPORT_DEPTH {
                return Err(LinkError::DepthExceeded {
                    limit: crate::limits::MAX_IMPORT_DEPTH as u32,
                });
            }

            // resolver を呼ぶ（§4.3 step 3）。
            *call_id += 1;
            let resolved_module = {
                let mut context = self.link_call_context(request, *call_id);
                resolver
                    .resolve(
                        &mut context,
                        crate::capability::ResolveRequest {
                            importer,
                            specifier: &normalized,
                            language_revision: revision,
                        },
                    )
                    .map_err(adapter_error_to_link_error)?
            };

            // resolver が返す ID は正規化済み specifier と一致する契約（§3.2）。防御的に
            // 不一致は内部 fault として扱う。
            if resolved_module.id != module_id {
                return Err(LinkError::Resolve(host_error("internal_resolver_fault")));
            }

            // source を 64 KiB chunk で Eof まで読み、bytes を連結する（§4.3 step 3）。
            let bytes = self.read_module_source(resolved_module.source, request, call_id)?;

            // source_hash を連結後の生 bytes から算出する（§4.3 step 4 / LOW-2）。
            let source_hash = SourceHash::from_bytes(hash::sha256(&bytes));

            // 同 ID / 異 hash は Resolve（embedding-api §5.1、後勝ちにしない、§4.4）。
            if let Some(existing) = resolved.get(&module_id) {
                if *existing != source_hash {
                    return Err(LinkError::Resolve(host_error("module_hash_mismatch")));
                }
                continue;
            }

            // UTF-8 検証（§4.3 step 3）。invalid なら InvalidModule。
            let text = match std::str::from_utf8(&bytes) {
                Ok(text) => text,
                Err(_) => {
                    return Err(LinkError::InvalidModule {
                        module: module_id.clone(),
                        diagnostics: CompileErrors {
                            diagnostics: vec![CompileDiagnostic {
                                code: CompileDiagnosticCode::Lex,
                                line: None,
                                column: None,
                                safe_message: "module source が UTF-8 ではありません".to_string(),
                            }],
                        },
                    });
                }
            };

            // module 単体 compile（parse まで）。失敗は InvalidModule（§4.3 step 3）。
            let module_program = {
                let tokens = Lexer::new(text).tokenize();
                Parser::new(tokens)
                    .parse()
                    .map_err(|errors| LinkError::InvalidModule {
                        module: module_id.clone(),
                        diagnostics: CompileErrors {
                            diagnostics: errors.iter().map(compile_diagnostic_from).collect(),
                        },
                    })?
            };

            // この module の import specifier を出現順に収集する。
            let module_specifiers: Vec<(String, usize)> = module_program
                .iter()
                .filter_map(|stmt| match stmt {
                    Stmt::Import { path, line } => Some((path.clone(), *line)),
                    _ => None,
                })
                .collect();

            // 解決済みに登録し、active へ push して子 import を降りる（§4.4）。
            resolved.insert(module_id.clone(), source_hash);
            active.push(module_id.clone());
            let mut child_imports: Vec<ModuleId> = Vec::new();
            self.resolve_module_imports(
                resolver,
                request,
                revision,
                Some(&module_id),
                &module_specifiers,
                depth + 1,
                call_id,
                resolved,
                node_map,
                active,
                &mut child_imports,
            )?;
            active.pop();

            node_map.insert(
                module_id.clone(),
                ImportNode {
                    module_id,
                    source_hash,
                    imports: child_imports,
                },
            );
        }
        Ok(())
    }

    /// specifier を parse・正規化し、正規形 `ModuleId` と正規化済み specifier 文字列を返す
    /// （§3.2「`ModuleId` 正規形」、2 channel 分離の HIGH-2）。
    fn normalize_specifier(
        &self,
        specifier: &str,
        resolver: &dyn crate::capability::ModuleResolver,
    ) -> Result<(ModuleId, String), LinkError> {
        use crate::capability::FilesystemTarget;

        // malformed specifier（PathError）→ Resolve(code "invalid_import_specifier")。
        let target = FilesystemTarget::parse(specifier)
            .map_err(|_| LinkError::Resolve(host_error("invalid_import_specifier")))?;

        // 正規形 `@mount/comp/comp/...` を組み立てる（unqualified は @default/... へ正規化）。
        let mut normalized = String::from("@");
        normalized.push_str(target.mount.as_str());
        for component in target.path.components() {
            normalized.push('/');
            normalized.push_str(component);
        }

        // mount 未登録は terminal Denied（authority 不足、§3.2 の 2 channel 分離）。resolver が
        // 知らない mount は resolve まで来ず link 層で Denied にする。
        if !resolver.knows_mount(target.mount.as_str()) {
            return Err(LinkError::Denied(resolver_absent_denial()));
        }

        let module_id = ModuleId::new(normalized.clone())
            .map_err(|_| LinkError::Resolve(host_error("invalid_import_specifier")))?;
        Ok((module_id, normalized))
    }

    /// resolver が返す source を 64 KiB chunk で Eof まで読み、bytes を連結する（§4.3 step 3）。
    fn read_module_source(
        &self,
        mut source: Box<dyn crate::capability::ModuleSource>,
        request: &LinkRequest,
        call_id: &mut u64,
    ) -> Result<Vec<u8>, LinkError> {
        use std::num::NonZeroUsize;
        const CHUNK: usize = 64 * 1024;
        let max_bytes = NonZeroUsize::new(CHUNK).expect("64 KiB は非ゼロ");
        let mut bytes = Vec::new();
        loop {
            // chunk 取得前にも cancel を観測する（§4.3 step 0）。
            if request.cancellation.is_cancelled() {
                return Err(LinkError::Cancelled);
            }
            *call_id += 1;
            let mut context = self.link_call_context(request, *call_id);
            let chunk = source
                .read_chunk(&mut context, max_bytes)
                .map_err(adapter_error_to_link_error)?;
            match chunk {
                crate::capability::ModuleChunk::Bytes(chunk) => {
                    // chunk 長 > max_bytes は adapter 契約違反（§4.3 step 3）。
                    if chunk.len() > max_bytes.get() {
                        return Err(LinkError::Resolve(host_error(
                            "resolver_contract_violation",
                        )));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                crate::capability::ModuleChunk::Eof => break,
            }
        }
        Ok(bytes)
    }

    /// link 時 resolver / source 呼び出し用の [`CapabilityCallContext`] を組む。
    ///
    /// deadline / cancellation は `request` の budget・token を共有する。remaining-bytes は
    /// Phase 2 では器のみ（import byte 上限を上限適用せず getter 値として渡す。CAP-AT-17 は
    /// Phase 3）。`may_yield` は link 層では false（link は top-level driver 文脈でない）。
    fn link_call_context<'a>(
        &self,
        request: &'a LinkRequest,
        call_id: u64,
    ) -> crate::capability::CapabilityCallContext<'a> {
        crate::capability::CapabilityCallContext::new(
            &request.capabilities,
            request.budget.deadline,
            request.cancellation.clone(),
            request.budget.max_import_bytes,
            request.budget.max_import_bytes,
            false,
            call_id,
        )
    }
}

/// 解決済み import graph の中間結果（root_imports と nodes）。
struct ResolvedGraph {
    root_imports: Vec<ModuleId>,
    nodes: Vec<ImportNode>,
}

/// resolver 不足時に返す固定の [`Denial`]（設計 §4.2・§4.5）。
fn resolver_absent_denial() -> crate::capability::Denial {
    crate::capability::Denial {
        code: crate::capability::DenialCode::ResourceNotGranted,
        capability: crate::capability::CapabilityKind::ModuleResolver,
        operation: crate::capability::OperationId::new("module.resolve"),
        public_resource: None,
    }
}

/// 固定 code の [`HostError`]（link 層の `Resolve` 用）。`safe_message` に specifier 原文・
/// 絶対 path を入れない（§8.5 存在 oracle 回避）。
fn host_error(code: &str) -> HostError {
    HostError {
        code: HostErrorCode::new(code).expect("固定 host error code は常に妥当"),
        safe_message: String::new(),
        retryable: false,
    }
}

/// adapter の失敗を link 層の [`LinkError`] へ写す（設計 §4.3 step 3）。
fn adapter_error_to_link_error(error: crate::capability::AdapterError) -> LinkError {
    use crate::budget::ControlStop;
    use crate::capability::AdapterError;
    match error {
        AdapterError::Host(_) => LinkError::Resolve(host_error("resolver_failed")),
        AdapterError::SecureResolutionUnsupported => {
            LinkError::Resolve(host_error("secure_resolution_unsupported"))
        }
        AdapterError::DirectoryReadFailed => LinkError::Resolve(host_error("resolver_failed")),
        AdapterError::NonUtf8EntryName => LinkError::Resolve(host_error("resolver_failed")),
        AdapterError::Control(stop) => match stop {
            ControlStop::Cancelled => LinkError::Cancelled,
            ControlStop::DeadlineExceeded { .. } => LinkError::DeadlineExceeded,
            ControlStop::BudgetExceeded(exceeded) => LinkError::BudgetExceeded(exceeded),
            ControlStop::InternalFailure(_) => {
                LinkError::Resolve(host_error("internal_resolver_fault"))
            }
        },
    }
}

// ===========================================================================
// スライス E3: tree backend adapter（実行入口）
// スライス E4: Context/Handle cleanup と transaction journal の縦切り
//   （再利用・poison・全 language-state rollback）
// ===========================================================================

use crate::budget::BudgetUsage;
use crate::eval::{Evaluator, RunPhase, SliceOutcome};

/// 1 回の実行に対する不変の設定（仕様第6節 `ExecutionRequest`）。
///
/// request は有限 [`BudgetConfig`](crate::budget::BudgetConfig) を**必須所有**し、deadline は
/// `budget.deadline` だけに存在する（REV-015 最終形移行 Slice 1、[実行制御仕様](../docs/execution-control.md)
/// §3 / §3.1、[組み込みAPI仕様](../docs/embedding-api.md) §6）。deadline を効かせる
/// monotonic clock も必須所有し、`new(budget, clock)` が構築時に domain / accounting revision /
/// deadline(>now) を検証する。cancellation token も必須所有し、`.cancellation(value)` builder で
/// 設定する（既定は未 cancel の新 token）。REV-015 最終形移行（budget + cancellation 必須所有）は
/// 本スライスで完了する。alpha facade（[`crate::engine::ExecutionRequest`]）とは別型。
#[derive(Clone)]
pub struct ExecutionRequest {
    /// `args()` が返すスクリプト引数 snapshot（binary 名・script path・CLI flag を含まない）。
    arguments: Vec<String>,
    /// この実行に付与する frozen capability 集合（Phase 2 C1〜、deny-by-default）。
    ///
    /// 既定は [`CapabilitySet::empty`]（全 authority 拒否）。host が `with_capabilities` で
    /// 明示 grant した authority だけを許可する。実行後に評価器から clear され、reusable な
    /// context へ持ち越さない（仕様第15節 規則5・6）。
    capabilities: crate::capability::CapabilitySet,
    /// この実行が所有する有限 budget（REV-015 最終形移行、必須）。
    ///
    /// deadline は `budget.deadline` だけに存在する（別経路の optional deadline は持たない）。
    budget: crate::budget::BudgetConfig,
    /// `budget.deadline` を効かせる monotonic clock（必須）。
    ///
    /// `budget.deadline` と同じ domain（`clock_id` 一致）でなければならず、`new` が構築時に
    /// 検証する。実行中に `clock.now() >= budget.deadline` へ達すると
    /// [`ExecutionOutcome::DeadlineExceeded`] terminal で停止する（script からは catch できない）。
    clock: Arc<dyn crate::budget::MonotonicClock>,
    /// この実行が所有する協調的 cancel token（REV-015 最終形移行、必須）。
    ///
    /// cancel は request が所有するこの token 経由で行う（`.cancellation(..)` で設定）。host は
    /// この token の clone を別スレッドで握り、[`CancellationToken::cancel`](crate::budget::CancellationToken::cancel)
    /// を呼ぶことで実行中の script を協調停止できる。実行前に cancel 済みの token を渡せば
    /// 命令を1つも実行せず [`ExecutionOutcome::Cancelled`] になる（pre-cancel は命令0、EMB-AT-12）。
    /// 既定（`.cancellation(..)` 未指定）は未 cancel の新 token。
    cancellation: crate::budget::CancellationToken,
    /// host 供給の実行 ID（Phase 6 A-1、監査 envelope の `execution_id`、§7.1）。
    ///
    /// 監査 sink を設定した engine で実行する場合に必須（`.with_execution_id(..)` で設定）。
    /// engine は ambient random source へ触れないため、未設定で sink がある場合は script work
    /// 前に fail-closed にする（§7.1 / §10.1）。sink 未設定なら無視する。
    execution_id: Option<ExecutionId>,
}

impl std::fmt::Debug for ExecutionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 引数値や capability 本文・budget 値を Debug へ出さない（secret-free）。件数・有無だけを
        // 見せる。budget は常に所有するので「有無」を出す意味はない。cancel 済みか否かの真偽は
        // 秘密ではないので is_cancelled として出す。
        f.debug_struct("ExecutionRequest")
            .field("argument_count", &self.arguments.len())
            .field("is_cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl ExecutionRequest {
    /// 有限 budget と、その deadline を効かせる clock を必須で受け取って実行リクエストを作る
    /// （REV-015 最終形移行 Slice 1、仕様第6節）。
    ///
    /// `clock` は `budget.deadline` を生成した clock と同じ domain（`clock_id` 一致）でなければ
    /// ならない。構築時に [`BudgetConfig::validate`](crate::budget::BudgetConfig::validate) を
    /// 呼び、clock domain 不一致を [`BudgetConfigError::ForeignClock`](crate::budget::ConfigError::ForeignClock)、
    /// `heap_accounting_revision != 1` を
    /// [`BudgetConfigError::UnsupportedAccountingRevision`](crate::budget::ConfigError::UnsupportedAccountingRevision)、
    /// deadline が作成時点以前なら
    /// [`BudgetConfigError::DeadlineNotInFuture`](crate::budget::ConfigError::DeadlineNotInFuture)
    /// で弾く。capability は既定で empty（deny-by-default）、引数は空で、`with_arguments` /
    /// `with_capabilities` で設定する。
    pub fn new(
        budget: crate::budget::BudgetConfig,
        clock: Arc<dyn crate::budget::MonotonicClock>,
    ) -> Result<Self, crate::budget::ConfigError> {
        budget.validate(clock.as_ref())?;
        Ok(Self {
            arguments: Vec::new(),
            capabilities: crate::capability::CapabilitySet::empty(),
            budget,
            clock,
            cancellation: crate::budget::CancellationToken::new(),
            execution_id: None,
        })
    }

    /// スクリプト引数 snapshot を設定する（AUD-018、仕様第6節）。
    pub fn with_arguments(mut self, arguments: Vec<String>) -> Self {
        self.arguments = arguments;
        self
    }

    /// この実行の frozen capability 集合を設定する（Phase 2、仕様第6節）。
    ///
    /// 設定しなければ deny-by-default（[`CapabilitySet::empty`]）。
    pub fn with_capabilities(mut self, capabilities: crate::capability::CapabilitySet) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// この実行の cancellation token を設定する（REV-015 最終形移行 Slice 2、仕様第3節/第7節）。
    ///
    /// host はこの token の clone を別スレッドで握り、[`CancellationToken::cancel`](crate::budget::CancellationToken::cancel)
    /// を呼ぶことで実行中の script を協調停止できる。[`Engine::run`] は実行直前にこの token を
    /// 評価器へ install するので、host clone と install token は同一 `Arc` を共有する。
    ///
    /// 実行前に cancel 済みの token を渡せば、script 命令を1つも実行せずに
    /// [`ExecutionOutcome::Cancelled`] を返す（pre-cancel は命令0、EMB-AT-12）。この場合
    /// language-state は開始時点へ rollback され（第10節 規則5）、context は poison されないので
    /// 再利用できる。
    ///
    /// 設定しなければ既定の未 cancel token が使われ、決して cancel されない。
    pub fn cancellation(mut self, value: crate::budget::CancellationToken) -> Self {
        self.cancellation = value;
        self
    }

    /// host 供給の実行 ID を設定する（Phase 6 A-1、監査 envelope の `execution_id`、§7.1）。
    ///
    /// 監査 sink を設定した engine で実行する場合に必須。engine は ambient random source へ
    /// 触れないため、host が一意な ID を生成して渡す。sink 未設定の engine では無視される。
    pub fn with_execution_id(mut self, id: ExecutionId) -> Self {
        self.execution_id = Some(id);
        self
    }
}

/// [`ExecutionContext`] の状態操作エラー（仕様第6節 `ContextError`）。
///
/// E4 で到達し得る variant のみを持つ。`Busy` は実行中の context を再入・状態操作した場合、
/// `Poisoned` は直前の実行が [`ExecutionOutcome::InternalFailure`] で context を poison した
/// 場合に返す。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextError {
    /// context が実行中で、再入または状態操作できない。
    Busy,
    /// 直前の実行が InternalFailure で context を poison した。再利用不可。
    Poisoned,
}

/// 実行間で維持する Tsumugi の状態（仕様第6節 `ExecutionContext`）。
///
/// 単一スレッドの評価状態（変数・関数・import 解決）を保持し、`!Send + !Sync`（第9.1節）。
/// これは stable 契約であり、別スレッドへ move できない。同 engine の実行で再利用すると
/// binding を保持する。
///
/// # transaction と再利用（E4、仕様第10節）
///
/// [`Engine::run`] は AUD-024 の transaction を全 execution へ適用する（第10節 規則5）。
/// `Completed`（将来は `Exited`）だけが変更した全 language-state（binding・cell・List/Dict・
/// function・module marker）を commit し、それ以外の terminal（`RuntimeError` 等）は execution
/// 開始時点へ rollback する。したがって未捕捉エラーで終わった execution の副作用は次の実行に
/// 残らない。
///
/// terminal 後の再利用可否は第10節 規則4に従う。`InternalFailure` だけが context を poison し、
/// 以後の実行・状態操作を [`ContextError::Poisoned`] で拒否する。他の terminal は commit /
/// rollback 完了後にそのまま再利用できる。
///
/// alpha facade（[`crate::engine::ExecutionContext`]）とは別型で、crate root では
/// [`crate::EmbeddingContext`] として公開する。
pub struct ExecutionContext {
    engine_id: EngineId,
    evaluator: Evaluator,
    /// InternalFailure で poison されたか（第10節 規則4）。true の間は実行・状態操作を拒否する。
    poisoned: bool,
    /// 実行中フラグ（第10節 規則: 同 context への再入は許さない）。
    ///
    /// `Engine::run` は入口で立て、terminal で必ず降ろす。再入すると [`ContextError::Busy`]。
    running: bool,
    /// test 専用: 次の run で評価器境界を panic させる注入フラグ（E6 の panic 隔離検証用）。
    #[cfg(test)]
    panic_in_run_for_test: bool,
    /// `!Send + !Sync` を保証する（第9.1節）。
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ExecutionContext {
    /// 指定した engine 用の実行コンテキストを作る（仕様第6節 `ExecutionContext::new`）。
    ///
    /// budget は [`ExecutionRequest`] が必須所有し、[`Engine::run`] が実行直前に評価器へ据え直す
    /// （REV-015 最終形移行 Slice 1）。context 生成時点では budget を確定できないため、評価器は
    /// bootstrap 用の既定（legacy env 由来）で作る。実際の実行で効く budget は常に request の
    /// ものになる。cancellation token も [`ExecutionRequest`] が必須所有し、[`Engine::run`] が
    /// 実行直前に評価器へ install する（REV-015 最終形移行 Slice 2）。context 生成時点の評価器
    /// token は次 run が install するまでの bootstrap 値で、実際の実行で効く token は常に request
    /// のものになる。
    pub fn new(engine: &Engine) -> Self {
        let evaluator = Evaluator::new();
        Self {
            engine_id: engine.id(),
            evaluator,
            poisoned: false,
            running: false,
            #[cfg(test)]
            panic_in_run_for_test: false,
            _not_send: std::marker::PhantomData,
        }
    }

    /// test 専用: 次の [`Engine::run`] で評価器境界を panic させる（E6 の panic 隔離検証用）。
    #[cfg(test)]
    fn inject_run_panic_for_test(&mut self) {
        self.panic_in_run_for_test = true;
    }

    /// このコンテキストを作った engine の ID。
    pub fn engine_id(&self) -> EngineId {
        self.engine_id
    }

    /// InternalFailure により poison されているか（仕様第6節 `is_poisoned`、第10節 規則4）。
    ///
    /// poison された context は実行・状態操作を拒否する。新しい context を作り直すこと。
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// script が定義した全 user state（binding・関数・import 解決）を破棄する
    /// （仕様第6節 `clear_user_state`）。
    ///
    /// 実行中は [`ContextError::Busy`]、poison 済みは [`ContextError::Poisoned`] を返し、
    /// どちらでもなければ評価状態を初期化して `Ok(())` を返す。engine 対応と `!Send` 契約は
    /// 維持する。
    pub fn clear_user_state(&mut self) -> Result<(), ContextError> {
        if self.running {
            return Err(ContextError::Busy);
        }
        if self.poisoned {
            return Err(ContextError::Poisoned);
        }
        // 評価状態を作り直して全 user state を捨てる（budget config は new と同じ既定）。
        self.evaluator = Evaluator::new();
        Ok(())
    }

    /// 相対 import 解決・自己再 import 防止に使う script path を設定する。
    ///
    /// Phase 1 は import なし root のみ実行するため実効はないが、CLI 同一入口（E8a）で
    /// 使えるよう用意する。
    pub fn set_script_path(&mut self, path: impl AsRef<std::path::Path>) {
        self.evaluator.set_base_dir(path.as_ref());
    }

    /// 現在の予算使用量 snapshot を返す（仕様第6節 `BudgetUsage`）。
    pub fn budget_usage(&self) -> BudgetUsage {
        self.evaluator.budget_usage()
    }
}

impl std::fmt::Debug for ExecutionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 評価状態（binding・値）を Debug へ出さない（secret-free）。
        f.debug_struct("ExecutionContext")
            .field("engine_id", &self.engine_id)
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// [`LinkedScript`] を実行コンテキスト内で同期実行し、terminal outcome を返す（仕様第2・8節）。
    ///
    /// E3（tree backend adapter）の実行入口。caller スレッドを terminal まで占有する
    /// convenience method で、worker thread を暗黙生成しない（仕様第2節）。tree 評価器の
    /// 既存意味論をそのまま使い、Phase 1 で到達し得る [`ExecutionOutcome`]
    /// （`Completed` / `RuntimeError` / `InternalFailure`）を返す。
    ///
    /// # 実行用 AST の再構築（案 A）
    ///
    /// [`CompiledScript`] は `Send + Sync` 契約のため runnable AST を保持しない。本メソッドは
    /// 実行スレッド上で `retain_source` の保持 source を再 parse して `Program` を得る。
    /// したがって **`retain_source=true` で compile した script だけが実行可能**である。
    /// source を保持していない場合は [`ExecutionOutcome::InternalFailure`] を返す（host の
    /// 前提条件違反）。設計判断の根拠と却下した案 B は
    /// [組み込みAPI仕様](../docs/embedding-api.md) 第4.1節を正本とする。
    ///
    /// # スレッドとスタック
    ///
    /// [`ExecutionContext`] は `!Send` で、caller スレッド上で再帰評価する。埋め込み host は
    /// 十分なスタックを持つスレッドで context を生成・利用する（CLI は 8 MiB スレッドを使う）。
    ///
    /// # transaction・poison・再利用（E4、仕様第10節）
    ///
    /// 全 execution へ AUD-024 の transaction を適用する（規則5）。`Completed` は変更した全
    /// language-state を commit し、`RuntimeError` は execution 開始時点へ rollback する。
    /// `InternalFailure` だけが context を poison し（規則4）、以後の実行・状態操作を
    /// [`ContextError::Poisoned`] で拒否する。poison 済み context を渡すと本メソッドは
    /// `InternalFailure` を返す。同 context への再入（`running` 中の呼び出し）も
    /// `InternalFailure` とする（第9.1節）。
    pub fn run(
        &self,
        linked: &LinkedScript,
        context: &mut ExecutionContext,
        request: ExecutionRequest,
    ) -> ExecutionOutcome {
        // poison 済み context は実行しない（第10節 規則4）。frame は clean のままで、poison は
        // 解除しない。この経路は poison を「新たに」設定しない（既に true）。
        if context.poisoned {
            return internal_failure("poison 済みの実行コンテキストは再利用できません");
        }
        // 同 context への再入は許さない（第9.1節）。!Send かつ &mut 借用のため通常は起きないが、
        // 防御的に検査する。既に別実行が running なので、この失敗では poison を設定しない
        // （その実行の terminal 側が状態を確定する）。
        if context.running {
            return internal_failure("実行コンテキストが実行中です（再入は許可されません）");
        }
        // 実行中は running を立てる（第9.1節 再入 bookkeeping）。両入口で対称に保つ
        // （[`Self::run_audited`] も同様に立てる）。
        context.running = true;

        // 監査 sink が設定されていなければ、従来どおりの実行経路（監査 wiring なし。opt-in の
        // ため挙動は bit-identical、§1.1 の sink 必須は後続スライス）。設定されていれば監査付き
        // 経路へ委譲し、audit 成功時は本来の outcome を、失敗時は fail-closed の非成功
        // outcome（下記 D1）を `ExecutionOutcome` へ写して返す。audit 失敗を観測したい caller は
        // [`Self::run_audited`] を使う（`ExecutionOutcome` は `AuditFailure` を表現できないため、
        // 既存の `InternalFailure` には畳まず別 surface で返す）。
        if self.config.audit_sink.is_none() {
            let outcome = self.run_guarded(linked, context, request);
            context.running = false;
            if matches!(outcome, ExecutionOutcome::InternalFailure { .. }) {
                context.poisoned = true;
            }
            return outcome;
        }

        // 監査付き経路。audit 失敗でない限り本来の outcome を返す。
        let audited = self.run_audited_inner(linked, context, request);
        context.running = false;
        let outcome = audited.outcome_for_poison();
        if matches!(outcome, ExecutionOutcome::InternalFailure { .. }) {
            context.poisoned = true;
        }
        audited.into_execution_outcome()
    }

    /// 監査 sink を設定した engine 向けの実行入口（Phase 6 A-1、§8/§10）。
    ///
    /// 返す [`AuditedOutcome`] は、通常の terminal を運ぶ [`AuditedOutcome::Outcome`] と、
    /// fail-closed（sink が `Failed` 等）で監査が失敗したことを運ぶ
    /// [`AuditedOutcome::AuditFailed`] の 2 系統を持つ。いずれの `AuditFailed` でも実行結果は
    /// success ではない。journal 上の Terminal は失敗 phase によって異なる（詳細は
    /// [`AuditedOutcome::AuditFailed`] 参照）:
    ///
    /// - **Started ack 失敗**（script work 開始前）: emergency slot へ `Terminal(AuditFailure)` が
    ///   append 済み。language-state は未変更。
    /// - **最終 Terminal 配送失敗**（§14、A-1 延期）: journal 上の唯一の Terminal は確定済みの
    ///   `Completed`/`Exited`（`AuditFailure` ではない）。`Completed`/`Exited` の language-state は
    ///   既に commit 済みで rollback しない。caller はこの context を再利用してはならない。
    ///
    /// sink が未設定の engine で呼ぶと監査は起きず、本来の outcome をそのまま
    /// [`AuditedOutcome::Outcome`] で返す（opt-in）。
    pub fn run_audited(
        &self,
        linked: &LinkedScript,
        context: &mut ExecutionContext,
        request: ExecutionRequest,
    ) -> AuditedOutcome {
        if context.poisoned {
            return AuditedOutcome::Outcome(Box::new(internal_failure(
                "poison 済みの実行コンテキストは再利用できません",
            )));
        }
        if context.running {
            return AuditedOutcome::Outcome(Box::new(internal_failure(
                "実行コンテキストが実行中です（再入は許可されません）",
            )));
        }
        context.running = true;
        let audited = if self.config.audit_sink.is_none() {
            AuditedOutcome::Outcome(Box::new(self.run_guarded(linked, context, request)))
        } else {
            self.run_audited_inner(linked, context, request)
        };
        context.running = false;
        if matches!(
            audited.outcome_for_poison(),
            ExecutionOutcome::InternalFailure { .. }
        ) {
            context.poisoned = true;
        }
        audited
    }

    /// panic 捕捉付きで `run_inner` を回す（監査 wiring なしの従来経路）。
    ///
    /// run / poll 中の unwind panic を host boundary で捕捉する（第11節 規則1）。捕捉した
    /// panic は terminal `ExecutionOutcome::InternalFailure` へ写す（規則2）。payload /
    /// backtrace は公開しない（規則3）。`AssertUnwindSafe` は、panic 後に context を
    /// poison して以後の再利用を拒否する（規則4）ことで正当化する。
    fn run_guarded(
        &self,
        linked: &LinkedScript,
        context: &mut ExecutionContext,
        request: ExecutionRequest,
    ) -> ExecutionOutcome {
        match catch_host_unwind(AssertUnwindSafe(|| {
            self.run_inner(linked, context, request)
        })) {
            Ok(outcome) => outcome,
            Err(fault_id) => ExecutionOutcome::InternalFailure {
                fault_id,
                safe_message: internal_fault_message(fault_id),
                // panic 捕捉時点の usage snapshot（counter の read は panic 後も安全）。
                usage: context.evaluator.budget_usage(),
            },
        }
    }

    /// 監査付きの実行本体（§8/§10/§10.1）。sink が設定済みの前提で呼ぶ。
    ///
    /// 手順:
    /// 1. precondition 検証と Program 再構築（失敗は Started 前の precondition 違反。監査上は
    ///    「開始していない execution」で Started を発行しない。§8）。
    /// 2. `execution_id` の存在を確認（§7.1、engine は ambient random source へ触れない）。
    ///    未指定なら script work 前に fail-closed。
    /// 3. journal を開き、`ExecutionStarted` を sequence 0 へ append → sink へ submit → Ack 待ち。
    ///    Started が ack されるまで script/import work を開始しない（§10.1）。sink が `Failed`
    ///    なら emergency slot へ `Terminal(AuditFailure)` を append し `AuditFailed` を返す。
    /// 4. 実行本体（panic 捕捉付き）を回して terminal outcome を得る。
    /// 5. outcome を `TerminalOutcome` へ写し、`Terminal` を予約 slot へ append → submit。
    ///    Terminal の submit が `Failed` でも journal 上の Terminal は確定済み（§8 規則10）。
    fn run_audited_inner(
        &self,
        linked: &LinkedScript,
        context: &mut ExecutionContext,
        request: ExecutionRequest,
    ) -> AuditedOutcome {
        use crate::audit::{
            AuditBudget, AuditEnvelope, AuditEvent, AuditFailure, AuditJournal, AuditSubmit,
            AuditWaker, ExecutionMode, HostTimestamp, TerminalOutcome,
        };

        let sink = match &self.config.audit_sink {
            Some(sink) => Arc::clone(sink),
            // 呼び出し規約違反（sink 未設定で呼ばれた）。従来経路へフォールバック。
            None => {
                return AuditedOutcome::Outcome(Box::new(
                    self.run_guarded(linked, context, request),
                ));
            }
        };

        // 1. precondition 検証と Program 再構築（Started 前。監査対象外の precondition 違反）。
        // 再構築（保持 source の再 parse）は host boundary の panic 隔離対象（第11節 規則1）。
        // no-sink 経路（run_guarded）が prepare_program を catch 内で回すのと対称に、監査経路でも
        // catch_host_unwind で包み、再 parse の panic が Engine::run を貫通せず InternalFailure へ
        // 写る（＝呼び出し元が context を poison する）ようにする。
        let prepared =
            match catch_host_unwind(AssertUnwindSafe(|| self.prepare_program(linked, context))) {
                Ok(result) => result,
                Err(fault_id) => {
                    // 再 parse 中の panic。Started 未発行なので監査 execution は無い（§8）。
                    return AuditedOutcome::Outcome(Box::new(ExecutionOutcome::InternalFailure {
                        fault_id,
                        safe_message: internal_fault_message(fault_id),
                        usage: context.evaluator.budget_usage(),
                    }));
                }
            };
        let (program, root_source_bytes) = match prepared {
            Ok(prepared) => prepared,
            Err(outcome) => return AuditedOutcome::Outcome(outcome),
        };

        // 2. execution_id の存在確認（§7.1）。engine は random source へ触れない。
        let Some(execution_id) = request.execution_id else {
            // Started を発行する前の fail-closed（script work は一切行っていない）。
            return AuditedOutcome::Outcome(Box::new(internal_failure(
                "監査 sink 設定時は ExecutionRequest::with_execution_id が必須です（§7.1）",
            )));
        };

        let source_hash = *linked.root().source_hash().as_bytes();
        let language_revision = linked.root().language_revision().as_str().to_string();
        let import_graph_hash = *linked.import_graph().graph_hash.as_bytes();
        let budget = request.budget;
        // 実行を認可した frozen policy の相関 ID（§7.1）。request がまだ execute へ move される
        // 前に、評価器へ install されるのと同じ CapabilitySet から決定的な 32 byte を取り出す。
        // これにより deny-by-default と stdout/filesystem/exit 等を grant した実行を監査上区別できる。
        let capability_policy_hash = *request.capabilities.id().as_bytes();
        // timestamp は注入 clock の ns を unix ns として載せる（A-1 は request の monotonic clock
        // の ns を流用する。順序の正本は sequence、§7.1）。全 envelope で同一値を使う。
        let timestamp = HostTimestamp {
            unix_nanoseconds: i128::from(request.clock.now().as_nanos()),
        };
        // envelope を組む小ヘルパ（borrow を避けるため値 clone で閉じる）。
        let make_envelope = |sequence: u64, event: AuditEvent| AuditEnvelope {
            schema_version: 1,
            execution_id,
            source_hash,
            language_revision: language_revision.clone(),
            sequence,
            timestamp,
            event,
        };

        let mut journal = AuditJournal::open(AuditBudget::default());

        // 3. ExecutionStarted を sequence 0 へ append。
        let started_event = AuditEvent::ExecutionStarted {
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            backend: self.config.backend,
            rules_revision: DETERMINISM_RULES_REVISION,
            heap_accounting_revision: crate::budget::HEAP_ACCOUNTING_REVISION,
            budget,
            // capability policy hash は実行を認可した frozen set の相関 ID（上で算出）。
            // redaction policy は A-1 では未配線のため安定の既定値を載せる。
            capability_policy_hash,
            redaction_policy_id: "default".to_string(),
            mode: ExecutionMode::Live,
        };
        let started_seq = match journal.append_normal(&make_envelope(0, started_event.clone())) {
            Ok(seq) => seq,
            // journal 自体が Started を受け付けない（予算等）→ fail-closed（Terminal も出せない）。
            Err(failure) => {
                return AuditedOutcome::AuditFailed {
                    withheld: TerminalOutcome::InternalFailure,
                    failure,
                };
            }
        };

        // sink へ submit し Ack を待つ（§10.1: Started が ack されるまで script work しない）。
        // §10 の ack 規則どおり、ack は submit した exact execution と連続 sequence を指すこと。
        // 別 execution の ack / gap / 未送信 sequence の ack は protocol 違反として fail-closed。
        let started_batch: Arc<[AuditEnvelope]> =
            Arc::from(vec![make_envelope(started_seq, started_event)]);
        let started_ok = match sink.submit(started_batch, AuditWaker::noop()) {
            AuditSubmit::Ack(ack) => {
                ack.execution_id == execution_id && ack.through_sequence == started_seq
            }
            // Failed / Pending（A-1 では Pending は起きない）は未 ack 扱い。
            _ => false,
        };
        if !started_ok {
            // Started の ack 失敗（Failed / 不正 ack、または A-1 では起きない Pending）→ fail-closed。
            // emergency slot へ Terminal(AuditFailure) を append し、script work は行わない（§10.1）。
            let terminal_event = AuditEvent::Terminal {
                outcome: TerminalOutcome::AuditFailure,
                error: None,
                usage: crate::budget::BudgetUsage::default(),
                import_graph_hash: None,
                context_committed: false,
                host_effects_may_remain: false,
            };
            let failure = match journal.append_terminal(&make_envelope(
                journal.next_sequence(),
                terminal_event.clone(),
            )) {
                Ok(seq) => {
                    let batch: Arc<[AuditEnvelope]> =
                        Arc::from(vec![make_envelope(seq, terminal_event)]);
                    let _ = sink.submit(batch, AuditWaker::noop());
                    AuditFailure::Sink
                }
                Err(failure) => failure,
            };
            // script は 1 命令も走っていないので「保留された本来の terminal」は無い。参考情報
            // として、監査自体が壊れたことを表す AuditFailure を載せる（Completed の捏造を避ける）。
            return AuditedOutcome::AuditFailed {
                withheld: TerminalOutcome::AuditFailure,
                failure,
            };
        }

        // 4. 実行本体（panic 捕捉付き）。Started 発行後なので pre-cancel も Started+Terminal を持つ。
        let outcome = match catch_host_unwind(AssertUnwindSafe(|| {
            #[cfg(test)]
            if context.panic_in_run_for_test {
                panic!("injected run-boundary panic (test only)");
            }
            self.execute_program(context, &program, root_source_bytes, request)
        })) {
            Ok(outcome) => outcome,
            Err(fault_id) => ExecutionOutcome::InternalFailure {
                fault_id,
                safe_message: internal_fault_message(fault_id),
                usage: context.evaluator.budget_usage(),
            },
        };

        // 5. Terminal を予約 slot へ append → submit（§8 規則9/10）。
        // BudgetExceeded の resource は、評価器が畳む前に退避した原本を優先する（§7.2。
        // control_stop_to_error は細粒度 resource を粗い ErrorKind へ縮約するため、原本が無いと
        // String/Source/I-O 系が代表値へ化ける）。原本が無い場合のみ ErrorKind から復元する。
        let exceeded_resource = if matches!(outcome, ExecutionOutcome::BudgetExceeded { .. }) {
            context.evaluator.take_pending_budget_resource()
        } else {
            None
        };
        let terminal_outcome = crate::audit::terminal_outcome_from(&outcome, exceeded_resource);
        let context_committed = matches!(
            outcome,
            ExecutionOutcome::Completed { .. } | ExecutionOutcome::Exited { .. }
        );
        let terminal_event = AuditEvent::Terminal {
            outcome: terminal_outcome.clone(),
            error: audit_error_payload_from(&outcome),
            usage: outcome_usage(&outcome),
            import_graph_hash: Some(import_graph_hash),
            context_committed,
            host_effects_may_remain: false,
        };
        match journal.append_terminal(&make_envelope(
            journal.next_sequence(),
            terminal_event.clone(),
        )) {
            Ok(terminal_seq) => {
                let batch: Arc<[AuditEnvelope]> =
                    Arc::from(vec![make_envelope(terminal_seq, terminal_event)]);
                // Terminal の配送結果を評価する（§10.1）。配送が成立し（Ack）、かつ ack が
                // この execution の Terminal sequence を正しく指しているときだけ監査成立とする。
                // 配送失敗（Failed / 不正 ack / Pending）は fail-closed: journal 上の Terminal は
                // 確定済み（§8 規則10）だが、監査が成立していないので success な outcome は返さず
                // AuditFailed を返す（§10.1: 監査が壊れた実行を成功として報告しない）。
                let delivered = matches!(
                    sink.submit(batch, AuditWaker::noop()),
                    AuditSubmit::Ack(ack)
                        if ack.execution_id == execution_id
                            && ack.through_sequence == terminal_seq
                );
                if delivered {
                    AuditedOutcome::Outcome(Box::new(outcome))
                } else {
                    AuditedOutcome::AuditFailed {
                        withheld: terminal_outcome,
                        failure: AuditFailure::Sink,
                    }
                }
            }
            // sequence overflow 等で Terminal すら append できない → fail-closed。
            Err(failure) => AuditedOutcome::AuditFailed {
                withheld: terminal_outcome,
                failure,
            },
        }
    }

    /// [`Self::run`] の本体（precondition guard の後）。ここから返る InternalFailure は
    /// すべて呼び出し元が context を poison する（第10節 規則4）。
    fn run_inner(
        &self,
        linked: &LinkedScript,
        context: &mut ExecutionContext,
        request: ExecutionRequest,
    ) -> ExecutionOutcome {
        // 実行前の整合検証と Program 再構築（案 A）。失敗は precondition 由来の InternalFailure。
        let (program, root_source_bytes) = match self.prepare_program(linked, context) {
            Ok(prepared) => prepared,
            Err(outcome) => return *outcome,
        };

        // test 専用: 評価器境界での panic を模擬し、E6 の catch_unwind 隔離を検証する。
        // この panic は run の catch_host_unwind 内で捕捉され InternalFailure へ写る。
        #[cfg(test)]
        if context.panic_in_run_for_test {
            panic!("injected run-boundary panic (test only)");
        }

        self.execute_program(context, &program, root_source_bytes, request)
    }

    /// 実行前の engine/context 整合検証と、保持 source からの Program 再構築（案 A）。
    ///
    /// 成功で runnable な [`Program`] を返す。失敗は precondition 違反の
    /// [`ExecutionOutcome::InternalFailure`]（呼び出し元が context を poison する）。この経路は
    /// 監査上「開始していない execution」であり、Started を発行しない（§8）。
    fn prepare_program(
        &self,
        linked: &LinkedScript,
        context: &ExecutionContext,
    ) -> Result<(Program, u64), Box<ExecutionOutcome>> {
        // engine / context の整合を検証する（別 engine の context は実行しない）。
        if context.engine_id() != self.id() {
            return Err(Box::new(internal_failure(
                "実行コンテキストの engine が一致しません",
            )));
        }
        let root = linked.root();
        if root.engine_id() != self.id() {
            return Err(Box::new(internal_failure(
                "LinkedScript の engine が一致しません",
            )));
        }

        // 実行用 Program を保持 source から再構築する（案 A）。
        let Some(source_text) = root.retained_source() else {
            return Err(Box::new(internal_failure(
                "実行には source の保持が必要です: retain_source=true で compile してください",
            )));
        };
        let root_source_bytes = source_text.len() as u64;
        match Parser::new(Lexer::new(source_text).tokenize()).parse() {
            Ok(program) => Ok((program, root_source_bytes)),
            // compile 済みの script を再 parse して失敗するのは内部不整合（決定的なはず）。
            Err(errors) => {
                let detail = errors
                    .first()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "詳細不明".to_string());
                Err(Box::new(internal_failure(format!(
                    "保持 source の再 parse に失敗しました: {detail}"
                ))))
            }
        }
    }

    /// 再構築済み Program を評価器へ据えて terminal まで実行する（budget install → pre-cancel
    /// 判定 → transaction）。監査 wiring は呼び出し元が担い、本体は従来挙動を保つ。
    fn execute_program(
        &self,
        context: &mut ExecutionContext,
        program: &Program,
        root_source_bytes: u64,
        request: ExecutionRequest,
    ) -> ExecutionOutcome {
        // request は有限 budget・deadline clock・cancellation token を必須所有する
        // （REV-015 最終形移行 Slice 2）。実行直前に 3 つとも評価器へ install する。
        // domain / accounting revision / deadline(>now) は ExecutionRequest::new が構築時に
        // 検証済みなので、ここで InternalFailure へ落ちる domain-mismatch 経路は存在しない。
        // reset_budget は request が所有する token を ledger へ install（継承ではなく置換）する
        // ため、host が run 前に `.cancellation(token.clone())` で載せた token は同一 Arc を共有し、
        // reset をまたいでも実行中に cancel が観測される（finding 1）。pre-run cancel 判定より前に
        // 置くことで、pre-cancel で返す usage も「この request の budget」に対するものになる。
        context
            .evaluator
            .reset_budget(request.budget, request.clock, request.cancellation.clone());

        // pre-run cancel（EMB-AT-12）: install 済み token が最初の poll より前に cancel 済みなら、
        // script 命令を1つも実行せずに Cancelled terminal を返す（"pre-cancel は命令0"）。
        // language-state は何も変更していないので rollback は自明に成立し、context は poison
        // されない（第10節 規則4・5）。判定は install 直後・baseline 課金前の順序を保つことで、
        // install した token の cancel 状態を見る。監査 wiring では Started 発行後にこの判定へ
        // 到達するので、pre-cancel も Started+Terminal(Cancelled) を持つ（§8）。
        if request.cancellation.is_cancelled() {
            // 命令0なので usage は空（reset_budget 直後・baseline 課金前。committed/reserved/live
            // とも 0）。finding 3。
            return ExecutionOutcome::Cancelled {
                usage: context.evaluator.budget_usage(),
            };
        }

        // 引数 snapshot と frozen capability 集合を評価器へ注入する（AUD-018 / Phase 2 C1〜）。
        context.evaluator.set_script_args(request.arguments);
        context.evaluator.set_capabilities(request.capabilities);
        // engine 単位の host function registry を実行前に注入する（E7、capability-model 第11.1節）。
        // 登録は engine 単位、grant は上の capability 集合（実行単位）という分離を保つ。
        context
            .evaluator
            .set_host_registry(std::sync::Arc::clone(&self.host_registry));

        let outcome = Self::run_transactional(context, program, root_source_bytes);

        // capability 集合も host function registry も reusable context に持ち越さない（仕様第15節
        // 規則5・6）。次 request の empty set か再注入まで、ambient 互換の既定へ戻す。
        context.evaluator.clear_capabilities();
        context.evaluator.clear_host_registry();
        outcome
    }

    /// begin_execution → run_slice を terminal まで回す（transaction 適用、E4）。
    ///
    /// transaction は評価器側で処理する（`transactional=true`）。`Completed` は commit、
    /// `RuntimeError` は rollback 済みで戻る。poison 判定は呼び出し元 [`Self::run`] が行う。
    fn run_transactional(
        context: &mut ExecutionContext,
        program: &Program,
        root_source_bytes: u64,
    ) -> ExecutionOutcome {
        // begin_execution → run_slice を terminal まで回す（engine.rs の poll と同じ手順）。
        // E4: transaction を全面適用する（transactional=true。AUD-024 / 仕様第10節 規則5）。
        if let Err((phase, error)) =
            context
                .evaluator
                .begin_execution(program, root_source_bytes, true)
        {
            // Phase 1 の import なし root では Link 失敗は起きないが、防御的に写像する。
            // link/control-plane の LinkError terminal は Phase 2（E7）で扱う。Link フェーズでも
            // charge（source/import/heap）で budget 超過・deadline・cancel が起き得るので、
            // 実行フェーズと同じ専用 terminal へ写す（REV-015 E11、§8）。
            return match phase {
                RunPhase::Link | RunPhase::Run => terminal_from_run_error(context, error),
            };
        }

        loop {
            // 大きな slice で 1 回ずつ回す（協調 yield の実効化は Phase 4）。同期実行なので
            // terminal まで回し切る。
            // run_slice は SliceOutcome を返す（REV-015 Slice 5、設計 §4.7）。同期経路は
            // slice=u64::MAX のため yield は実質起きず、host-call pending も Ready フォールバック
            // のため起きないが、型に合わせて全 yield 変種を continue で回し切る（観測挙動不変、§7）。
            match context.evaluator.run_slice(u64::MAX) {
                SliceOutcome::YieldedSliceFuel
                | SliceOutcome::YieldedHostCall { .. }
                | SliceOutcome::YieldedExplicit => continue,
                SliceOutcome::Terminal(Ok(())) => {
                    // commit 済みの usage を同梱する（仕様第6・12節）。
                    let usage = context.evaluator.budget_usage();
                    return ExecutionOutcome::Completed { usage };
                }
                SliceOutcome::Terminal(Err(error)) => {
                    // exit() の structured terminal（C7、REV-023）を Exited outcome へ写す。
                    // run_slice は既に language-state を commit 済み（Exited は Completed と同じ
                    // 規則5 commit）。cancel / deadline / budget 超過は catch 不能 terminal 信号
                    // なので専用 outcome へ写し、RuntimeError に埋もれさせない（REV-015 E11、
                    // 仕様第8節）。それ以外の未捕捉エラーは RuntimeError（rollback 済み）。
                    return terminal_from_run_error(context, error);
                }
            }
        }
    }
}

/// 内部 [`TsumugiError`] を [`ExecutionOutcome::RuntimeError`] へ写す。
fn runtime_error_outcome(error: TsumugiError, usage: BudgetUsage) -> ExecutionOutcome {
    ExecutionOutcome::RuntimeError {
        error: execution_error_from(&error),
        usage,
    }
}

/// 実行（Link / Run フェーズ）で発生した内部 [`TsumugiError`] を terminal outcome へ写す
/// （REV-015 E11、仕様第8節）。
///
/// catch 不能 terminal 信号（`exit` / cancel / deadline / budget 超過）は専用 outcome へ写し、
/// それ以外の未捕捉 runtime error は [`ExecutionOutcome::RuntimeError`] とする。いずれの経路も
/// `run_slice` / `begin_execution` が既に language-state を commit / rollback 済みで戻る
/// （`Exited` は commit、他は rollback）。
fn terminal_from_run_error(
    context: &mut ExecutionContext,
    error: TsumugiError,
) -> ExecutionOutcome {
    use crate::error::ErrorKind;
    // usage snapshot は commit / rollback 後（run_slice / begin_execution が確定済み）の値。
    let usage = context.evaluator.budget_usage();
    match error.kind() {
        // exit() の structured terminal（C7、REV-023）。commit 済み。
        Some(ErrorKind::ProcessExit) => {
            let code = context.evaluator.take_pending_exit().unwrap_or(0);
            ExecutionOutcome::Exited { code, usage }
        }
        // 協調 cancel（§8）。catch 不能 terminal。
        Some(ErrorKind::Cancelled) => ExecutionOutcome::Cancelled { usage },
        // deadline 超過（§8）。catch 不能 terminal。
        Some(ErrorKind::DeadlineExceeded) => ExecutionOutcome::DeadlineExceeded { usage },
        // 予算超過（fuel / collection / string / source / heap / I-O）。catch 不能 terminal で
        // RuntimeError には畳まない（§8）。
        Some(
            ErrorKind::StepLimit
            | ErrorKind::CollectionLimit
            | ErrorKind::StringLimit
            | ErrorKind::SourceLimit
            | ErrorKind::HeapLimit
            | ErrorKind::IoLimit,
        ) => ExecutionOutcome::BudgetExceeded {
            error: execution_error_from(&error),
            usage,
        },
        // それ以外の未捕捉 runtime error（rollback 済み）。
        _ => runtime_error_outcome(error, usage),
    }
}

/// determinism rules revision（§3 `DeterminismRulesRevision`）。A-1 では固定 1。
///
/// Phase 6 で rules を跨いだ差分を検出するための revision。本スライスは監査 envelope の
/// `ExecutionStarted.rules_revision` に載せる固定値だけを持つ（配線は後続スライス）。
const DETERMINISM_RULES_REVISION: u32 = 1;

/// 監査付き実行（[`Engine::run_audited`]）の結果（Phase 6 A-1、§8/§10.1）。
///
/// 通常の terminal を運ぶ [`AuditedOutcome::Outcome`] と、fail-closed で監査が失敗したことを
/// 運ぶ [`AuditedOutcome::AuditFailed`] の 2 系統を持つ。`ExecutionOutcome` enum は
/// `AuditFailure` terminal を表現できず（かつ既存の `InternalFailure` へ畳まない方針）、監査
/// 失敗は本型でのみ surface する（additive。既存 enum を壊さない。D1）。
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AuditedOutcome {
    /// 監査が成立した通常の terminal。`ExecutionOutcome` を運ぶ（enum サイズ平準化のため box）。
    Outcome(Box<ExecutionOutcome>),
    /// 監査が fail-closed で失敗した（§10.1）。実行結果は success ではない。`withheld` は、
    /// 監査が成立していれば報告されたはずの terminal 種別。
    ///
    /// journal 上の Terminal は失敗した phase によって異なる:
    ///
    /// - **Started ack 失敗**（script work 開始前）: emergency slot へ `Terminal(AuditFailure)` が
    ///   append 済み（sequence overflow など append すら不能な場合を除く）。`withheld` は
    ///   `AuditFailure`。language-state は未変更。
    /// - **最終 Terminal 配送失敗**（§14、A-1 延期）: journal 上の唯一の Terminal は確定済みの
    ///   `Completed`/`Exited` であり、`AuditFailure` ではない。`withheld` はその保留された本来の
    ///   terminal 種別。`Completed`/`Exited` の language-state は既に commit 済みで A-1 では
    ///   rollback しない。context は poison されないが、caller は再利用してはならない。
    AuditFailed {
        /// 監査が成立していれば報告されたはずの terminal 種別（参考情報）。
        withheld: crate::audit::TerminalOutcome,
        /// 監査失敗の理由（§10.1/§11）。
        failure: crate::audit::AuditFailure,
    },
}

impl AuditedOutcome {
    /// poison 判定用に、この結果が表す `ExecutionOutcome` を参照で返す。
    ///
    /// `AuditFailed` は script 実行の内部障害ではないため、poison はしない（Started 前の
    /// precondition 違反だけが `Outcome(InternalFailure)` として poison 対象になる）。
    fn outcome_for_poison(&self) -> ExecutionOutcome {
        match self {
            AuditedOutcome::Outcome(outcome) => (**outcome).clone(),
            // 監査失敗自体は context を poison しない（空 usage の非成功 sentinel を返す）。
            AuditedOutcome::AuditFailed { .. } => ExecutionOutcome::Cancelled {
                usage: BudgetUsage::default(),
            },
        }
    }

    /// `Engine::run`（`ExecutionOutcome` を返す入口）向けの写像。
    ///
    /// 監査成功時は本来の outcome。監査失敗時は、`ExecutionOutcome` が `AuditFailure` を
    /// 表現できないため、success ではない保守的な terminal として `Cancelled`（空 usage）を
    /// 返す。監査失敗を正確に観測したい caller は [`Engine::run_audited`] を使う。
    fn into_execution_outcome(self) -> ExecutionOutcome {
        match self {
            AuditedOutcome::Outcome(outcome) => *outcome,
            AuditedOutcome::AuditFailed { .. } => ExecutionOutcome::Cancelled {
                usage: BudgetUsage::default(),
            },
        }
    }

    /// 通常 terminal を運ぶ [`AuditedOutcome::Outcome`] の参照。監査失敗時は `None`。
    pub fn outcome(&self) -> Option<&ExecutionOutcome> {
        match self {
            AuditedOutcome::Outcome(outcome) => Some(outcome),
            AuditedOutcome::AuditFailed { .. } => None,
        }
    }

    /// 監査が成立したか（`Outcome` なら true）。
    pub fn is_audited_ok(&self) -> bool {
        matches!(self, AuditedOutcome::Outcome(_))
    }
}

/// terminal outcome から公開 `usage` snapshot を取り出す（監査 Terminal 用）。
fn outcome_usage(outcome: &ExecutionOutcome) -> BudgetUsage {
    match outcome {
        ExecutionOutcome::Completed { usage }
        | ExecutionOutcome::Exited { usage, .. }
        | ExecutionOutcome::RuntimeError { usage, .. }
        | ExecutionOutcome::BudgetExceeded { usage, .. }
        | ExecutionOutcome::DeadlineExceeded { usage }
        | ExecutionOutcome::Cancelled { usage }
        | ExecutionOutcome::InternalFailure { usage, .. } => *usage,
    }
}

/// error terminal から secret-free な監査 error payload を作る（§7.2 `AuditErrorPayload::Full`）。
///
/// A-1 では redaction policy 本体を実装しないため、`ExecutionError` の既存 secret-free field
/// （`code` の安定文字列・行番号・trace の関数名）だけを写す。source 本文・host response・
/// 自由文の生 message は載せない（§9）。success 系 terminal は `None`。
fn audit_error_payload_from(outcome: &ExecutionOutcome) -> Option<crate::audit::AuditErrorPayload> {
    use crate::audit::{AuditError, AuditErrorPayload, AuditFrame};
    let error = match outcome {
        ExecutionOutcome::RuntimeError { error, .. }
        | ExecutionOutcome::BudgetExceeded { error, .. } => error,
        // Completed / Exited / Deadline / Cancelled / Internal は error payload を持たない
        // （§7.2: error は error terminal のみ。InternalFailure の safe_message は載せない）。
        _ => return None,
    };
    let trace = error
        .trace
        .iter()
        .take(32)
        .map(|frame| AuditFrame {
            function: frame.function.clone(),
            module_id: String::new(),
            line: frame.line.unwrap_or(0),
        })
        .collect::<Vec<_>>();
    let omitted_trace_frames = u32::try_from(error.trace.len().saturating_sub(32)).unwrap_or(0);
    Some(AuditErrorPayload::Full(AuditError {
        // 安定 error code（ErrorKind の機械可読名）。生 message は載せない（§9）。
        code: error.code.as_str().to_string(),
        // message_id は A-1 では error code を流用する（安定 ID 体系は後続スライス）。
        message_id: error.code.as_str().to_string(),
        module_id: None,
        line: error.line,
        column: None,
        trace,
        omitted_trace_frames,
    }))
}

/// secret を含まない [`ExecutionOutcome::InternalFailure`] を作る。
///
/// 実行を1命令も試みない precondition 違反経路で使うため、`usage` は空（既定）とする。
fn internal_failure(safe_message: impl Into<String>) -> ExecutionOutcome {
    ExecutionOutcome::InternalFailure {
        fault_id: next_fault_id(),
        safe_message: safe_message.into(),
        usage: BudgetUsage::default(),
    }
}

/// 相関用の非ゼロ fault ID を発番する。
fn next_fault_id() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed) as u128
}

/// fault ID を埋め込んだ secret-free な内部障害メッセージ（第11節 規則3）。
///
/// panic payload / native backtrace は決して含めない。相関のための fault ID だけを載せる。
fn internal_fault_message(fault_id: u128) -> String {
    format!("内部障害が発生しました (fault_id={fault_id})")
}

/// host boundary の unwind panic を捕捉する（第11節 規則1）。
///
/// panic を捕まえたら発番済みの非ゼロ fault ID を `Err` で返す。panic payload / native
/// backtrace は呼び出し元へ渡さない（規則3）。`std::process::abort`（`panic=abort`）・OOM・
/// stack overflow abort は捕捉できない（規則4。最終防御は別 process 隔離）。
fn catch_host_unwind<F, T>(f: F) -> Result<T, u128>
where
    F: FnOnce() -> T + UnwindSafe,
{
    std::panic::catch_unwind(f).map_err(|_payload| next_fault_id())
}

/// 内部 [`TsumugiError`] を公開 [`ExecutionError`] へ写す（仕様第8節）。
///
/// `Runtime` は `kind`・`trace` を持つ。`Parse` は実行フェーズでは発生しない想定だが、
/// 防御的に `Internal` として写す（message は AUD-019 により secret-free）。
fn execution_error_from(error: &TsumugiError) -> ExecutionError {
    match error {
        TsumugiError::Runtime {
            line,
            message,
            kind,
            trace,
        } => ExecutionError {
            code: *kind,
            safe_message: message.clone(),
            line: u32::try_from(*line).ok(),
            trace: trace
                .iter()
                .map(|frame| TraceFrame {
                    function: frame.name.clone(),
                    line: u32::try_from(frame.line).ok(),
                })
                .collect(),
        },
        TsumugiError::Parse { line, message } => ExecutionError {
            code: ErrorKind::Internal,
            safe_message: message.clone(),
            line: u32::try_from(*line).ok(),
            trace: Vec::new(),
        },
    }
}

/// 内部 [`TsumugiError`]（compile フェーズ）を [`CompileDiagnostic`] へ写す。
fn compile_diagnostic_from(error: &TsumugiError) -> CompileDiagnostic {
    let (line, message) = match error {
        TsumugiError::Parse { line, message } => (*line, message.clone()),
        // compile フェーズは Parse のみを生成するが、防御的に他種も message を拾う。
        TsumugiError::Runtime { line, message, .. } => (*line, message.clone()),
    };
    let code = if message.contains("ネストが深すぎます") {
        CompileDiagnosticCode::AstDepth
    } else {
        CompileDiagnosticCode::Parse
    };
    CompileDiagnostic {
        code,
        line: u32::try_from(line).ok(),
        column: None,
        safe_message: message,
    }
}

/// SHA-256（FIPS 180-4）と §5.1 の byte-level hash encoding。
///
/// 外部クレートを持ち込まないため self-contained に実装する。既知テストベクタで検証する。
///
/// `CapabilitySetId`（[`crate::capability`]）も同じ self-contained SHA-256 を再利用するため
/// crate 内へ公開する（外部 SHA-256 実装を二重に持ち込まない）。
pub(crate) mod hash {
    use super::{LanguageRevision, SourceHash};

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    /// SHA-256 ダイジェスト（32 bytes）を計算する。
    pub fn sha256(data: &[u8]) -> [u8; 32] {
        let mut h = H0;

        // padding: 0x80、64bit 境界の 56 まで 0、その後 64bit big-endian の bit 長。
        let bit_len = (data.len() as u64).wrapping_mul(8);
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());

        // padding 後は必ず 64 の倍数長。`as_chunks` で 64 byte 固定長ブロックへ分ける。
        let (blocks, rest) = msg.as_chunks::<64>();
        debug_assert!(rest.is_empty());
        for chunk in blocks {
            let mut w = [0u32; 64];
            for (i, word) in w.iter_mut().enumerate().take(16) {
                let j = i * 4;
                *word = u32::from_be_bytes([chunk[j], chunk[j + 1], chunk[j + 2], chunk[j + 3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }

            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }

        let mut out = [0u8; 32];
        for (i, word) in h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// §5.1 の `str = u64(len) || UTF-8 bytes` を buffer へ書く。
    fn push_str_field(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(&(s.len() as u64).to_be_bytes());
        buf.extend_from_slice(s.as_bytes());
    }

    /// import graph の byte-level hash（§5.1、`TSUMUGI-IMPORT-GRAPH-V1`）。
    ///
    /// `nodes` は module_id の UTF-8 byte 列昇順で渡す前提。各 node は
    /// `(module_id, source_hash, imports)` を持つ。E2 では import 0 件のため空 slice を渡す。
    pub fn import_graph_hash(
        revision: LanguageRevision,
        root_hash: SourceHash,
        root_imports: &[&str],
        nodes: &[(&str, SourceHash, &[&str])],
    ) -> [u8; 32] {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"TSUMUGI-IMPORT-GRAPH-V1\0");
        push_str_field(&mut buf, revision.as_str());
        buf.extend_from_slice(root_hash.as_bytes());
        buf.extend_from_slice(&(root_imports.len() as u64).to_be_bytes());
        for id in root_imports {
            push_str_field(&mut buf, id);
        }
        buf.extend_from_slice(&(nodes.len() as u64).to_be_bytes());
        for (module_id, source_hash, imports) in nodes {
            push_str_field(&mut buf, module_id);
            buf.extend_from_slice(source_hash.as_bytes());
            buf.extend_from_slice(&(imports.len() as u64).to_be_bytes());
            for imp in *imports {
                push_str_field(&mut buf, imp);
            }
        }
        sha256(&buf)
    }

    /// linked script の byte-level hash（§5.1、`TSUMUGI-LINKED-SCRIPT-V1`）。
    pub fn linked_script_hash(
        revision: LanguageRevision,
        root_hash: SourceHash,
        graph_hash: SourceHash,
    ) -> [u8; 32] {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"TSUMUGI-LINKED-SCRIPT-V1\0");
        push_str_field(&mut buf, revision.as_str());
        buf.extend_from_slice(root_hash.as_bytes());
        buf.extend_from_slice(graph_hash.as_bytes());
        sha256(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 決定的 test 用の standard budget + 共有 clock から [`ExecutionRequest`] を作る helper。
    ///
    /// 単一の [`FakeClock`](crate::budget::FakeClock) を 1 個作り、`standard` budget の deadline
    /// 生成と request への clock 注入で同一 instance を共有する（CLI の option B と同じ構成）。
    /// deadline は `now + 30s` の遠い未来なので、FakeClock を進めない限り到達しない。
    fn standard_request() -> ExecutionRequest {
        let clock: Arc<dyn crate::budget::MonotonicClock> =
            Arc::new(crate::budget::FakeClock::new());
        let budget = crate::budget::BudgetConfig::standard(clock.as_ref())
            .expect("standard budget の生成は成功する");
        ExecutionRequest::new(budget, clock)
            .expect("standard budget は自身を生成した clock と同 domain なので検証を通る")
    }

    /// `standard_request()` に実行前 cancel 済みの token を載せた [`ExecutionRequest`] を作る
    /// helper（pre-cancel 系テスト用、REV-015 Slice 2）。`.cancellation(..)` 経由で表現する。
    fn standard_request_cancelled() -> ExecutionRequest {
        let token = crate::budget::CancellationToken::new();
        token.cancel();
        standard_request().cancellation(token)
    }

    /// import なし link テスト用の既定 [`LinkRequest`]（empty capabilities + standard budget）。
    ///
    /// C6-c で `LinkRequest::new` が 3 引数化したため、import 0 件を link するテストはこの helper で
    /// 既定要求を作る（operation_id は固定、capabilities は empty = resolver 未 grant、budget は
    /// standard）。import 0 件は resolver を呼ばず空 graph を返すので empty で足りる。
    fn link_request() -> LinkRequest {
        let clock = crate::budget::FakeClock::new();
        let budget = crate::budget::BudgetConfig::standard(&clock)
            .expect("standard budget の生成は成功する");
        LinkRequest::new(
            ExecutionId::new(std::num::NonZeroU128::new(1).expect("1 は非ゼロ")),
            crate::capability::CapabilitySet::empty(),
            budget,
        )
    }

    #[test]
    fn default_config_is_tree_walk_current() {
        let config = EngineConfig::default();
        assert_eq!(config.backend, Backend::TreeWalk);
        assert_eq!(config.language_revision, LanguageRevision::CURRENT);
        assert_eq!(config.language_revision.as_str(), "0.20");
    }

    #[test]
    fn builder_default_builds_tree_walk_engine() {
        let engine = Engine::builder().build().expect("default build");
        assert_eq!(engine.config().backend, Backend::TreeWalk);
    }

    #[test]
    fn engine_ids_are_distinct_and_nonzero() {
        let a = Engine::builder().build().unwrap();
        let b = Engine::builder().build().unwrap();
        assert_ne!(a.id(), b.id());
        assert_ne!(a.id().get(), 0);
    }

    #[test]
    fn vm_experimental_requires_opt_in() {
        let config = EngineConfig {
            backend: Backend::VmExperimental,
            language_revision: LanguageRevision::CURRENT,
            audit_sink: None,
        };
        let err = Engine::builder()
            .config(config.clone())
            .build()
            .expect_err("must reject without opt-in");
        assert_eq!(err, ConfigError::ExperimentalBackendNotEnabled);

        let engine = Engine::builder()
            .config(config)
            .allow_experimental_backend(true)
            .build()
            .expect("opt-in build");
        assert_eq!(engine.config().backend, Backend::VmExperimental);
    }

    /// Phase 6 A-1: 監査 sink + `VmExperimental` backend は build 時に拒否する。
    /// A-1 の run 経路は tree-only なので、VM label を監査へ載せると tree 実行を VM として
    /// 誤報告してしまう。VM の監査 wiring が入るまで、この組み合わせを使えないよう固定する。
    #[test]
    fn audit_sink_with_experimental_backend_is_rejected() {
        let sink = Arc::new(crate::audit::InMemoryAuditSink::new());
        let config = EngineConfig {
            backend: Backend::VmExperimental,
            language_revision: LanguageRevision::CURRENT,
            audit_sink: None,
        };
        let err = Engine::builder()
            .config(config)
            // experimental backend 自体は許可済みにして、残る拒否理由を sink 併用だけに絞る。
            .allow_experimental_backend(true)
            .audit_sink(sink as Arc<dyn crate::audit::AuditSink>)
            .build()
            .expect_err("audit_sink + VmExperimental は拒否される");
        assert_eq!(err, ConfigError::AuditSinkWithExperimentalBackend);
    }

    /// 対照: 監査 sink は既定の tree-walk backend では問題なく build できる。
    #[test]
    fn audit_sink_with_tree_walk_backend_builds() {
        let sink = Arc::new(crate::audit::InMemoryAuditSink::new());
        let engine = Engine::builder()
            .audit_sink(sink as Arc<dyn crate::audit::AuditSink>)
            .build()
            .expect("audit_sink + TreeWalk は build できる");
        assert_eq!(engine.config().backend, Backend::TreeWalk);
    }

    #[test]
    fn source_id_validation() {
        assert_eq!(
            SourceId::new(""),
            Err(ConfigError::EmptyIdentifier { field: "source_id" })
        );
        assert_eq!(
            SourceId::new("a\0b"),
            Err(ConfigError::IdentifierContainsNul { field: "source_id" })
        );
        let too_long = "a".repeat(257);
        assert_eq!(
            SourceId::new(too_long),
            Err(ConfigError::IdentifierTooLong {
                field: "source_id",
                max_bytes: 256
            })
        );
        let ok = SourceId::new("main.tsg").expect("valid id");
        assert_eq!(ok.as_str(), "main.tsg");
        // 境界: ちょうど 256 byte は受理する。
        assert!(SourceId::new("a".repeat(256)).is_ok());
    }

    #[test]
    fn host_error_code_validation() {
        assert!(HostErrorCode::new("fs.read_failed").is_ok());
        assert!(HostErrorCode::new("io").is_ok());
        // 先頭が英字でない。
        assert!(HostErrorCode::new("1abc").is_err());
        // 大文字は不可。
        assert!(HostErrorCode::new("Abc").is_err());
        // 空は不可。
        assert!(HostErrorCode::new("").is_err());
        // 64 byte 超過は不可。
        assert!(HostErrorCode::new(format!("a{}", "b".repeat(64))).is_err());
    }

    #[test]
    fn execution_id_roundtrip() {
        let value = NonZeroU128::new(42).unwrap();
        let id = ExecutionId::new(value);
        assert_eq!(id.get(), value);
    }

    #[test]
    fn source_hash_roundtrip() {
        let bytes = [7u8; 32];
        let hash = SourceHash::from_bytes(bytes);
        assert_eq!(hash.as_bytes(), &bytes);
    }

    /// secret-free Debug: エラー・outcome の Debug 出力に埋め込んだ秘密が現れないこと。
    #[test]
    fn debug_output_does_not_leak_injected_secret() {
        const SECRET: &str = "SUPER_SECRET_TOKEN";

        // ExecutionError / HostError / outcome の Debug は、開発者が safe_message へ入れない
        // 限り secret を含まない。ここでは safe_message に secret を入れない構築を検証する。
        let err = ExecutionError {
            code: ErrorKind::Type,
            safe_message: "型エラー".to_string(),
            line: Some(3),
            trace: vec![TraceFrame {
                function: "<main>".to_string(),
                line: Some(3),
            }],
        };
        let outcome = ExecutionOutcome::RuntimeError {
            error: err,
            usage: BudgetUsage::default(),
        };
        let rendered = format!("{outcome:?}");
        assert!(!rendered.contains(SECRET));

        let internal = ExecutionOutcome::InternalFailure {
            fault_id: 12345,
            safe_message: "内部エラー".to_string(),
            usage: BudgetUsage::default(),
        };
        assert!(!format!("{internal:?}").contains(SECRET));
    }

    /// Send + Sync が要求される公開型のコンパイル時アサーション。
    #[test]
    fn config_and_outcome_types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Engine>();
        assert_send_sync::<EngineConfig>();
        assert_send_sync::<ExecutionOutcome>();
        assert_send_sync::<ExecutionError>();
        assert_send_sync::<HostError>();
        assert_send_sync::<SourceId>();
        assert_send_sync::<SourceHash>();
        assert_send_sync::<ExecutionId>();
        assert_send_sync::<EngineId>();
        // E2: compile/link 成果物も再利用のため Send + Sync。
        assert_send_sync::<CompiledScript>();
        assert_send_sync::<LinkedScript>();
    }

    // --- E2: compile / link / hash ---

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn src(id: &str, text: &str) -> Source<'static> {
        // テスト用に text を leak して 'static にする（測定に影響しない）。
        Source::new(
            SourceId::new(id).unwrap(),
            Box::leak(text.to_string().into_boxed_str()),
        )
    }

    /// SHA-256 の既知テストベクタ（FIPS 180-4）で self-contained 実装を検証する。
    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            hex(&hash::sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&hash::sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&hash::sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn compile_produces_source_hash_over_raw_bytes() {
        let engine = Engine::builder().build().unwrap();
        let text = "let x = 1\n";
        let script = engine
            .compile(src("main.tsg", text), &CompileOptions::default())
            .expect("compile");
        assert_eq!(script.engine_id(), engine.id());
        assert_eq!(script.source_id().as_str(), "main.tsg");
        assert_eq!(script.language_revision(), LanguageRevision::CURRENT);
        assert_eq!(script.backend(), Backend::TreeWalk);
        // SourceHash は生 UTF-8 bytes の SHA-256。
        assert_eq!(
            script.source_hash().as_bytes(),
            &hash::sha256(text.as_bytes())
        );
    }

    #[test]
    fn compile_does_not_retain_source_by_default() {
        let engine = Engine::builder().build().unwrap();
        let script = engine
            .compile(src("m", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        assert_eq!(script.retained_source(), None);

        let retained = engine
            .compile(
                src("m", "let x = 1\n"),
                &CompileOptions {
                    retain_source: true,
                },
            )
            .unwrap();
        assert_eq!(retained.retained_source(), Some("let x = 1\n"));
    }

    #[test]
    fn compile_reports_parse_diagnostics() {
        let engine = Engine::builder().build().unwrap();
        let err = engine
            .compile(src("bad", "let = = ="), &CompileOptions::default())
            .expect_err("must fail");
        assert!(!err.diagnostics.is_empty());
        assert!(
            err.diagnostics
                .iter()
                .all(|d| d.code == CompileDiagnosticCode::Parse)
        );
    }

    #[test]
    fn compile_diagnostic_debug_is_secret_free() {
        // 診断 message は開発者が入れない限り secret を含まない。
        let engine = Engine::builder().build().unwrap();
        let err = engine
            .compile(src("bad", "@@@"), &CompileOptions::default())
            .expect_err("must fail");
        let rendered = format!("{err:?}");
        assert!(!rendered.contains("SECRET"));
    }

    #[test]
    fn source_hash_changes_with_one_byte_diff() {
        let engine = Engine::builder().build().unwrap();
        let a = engine
            .compile(src("m", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        let b = engine
            .compile(src("m", "let x = 2\n"), &CompileOptions::default())
            .unwrap();
        assert_ne!(a.source_hash(), b.source_hash());
    }

    #[test]
    fn link_import_less_produces_empty_graph_and_hashes() {
        let engine = Engine::builder().build().unwrap();
        let script = engine
            .compile(src("m", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        let linked = engine.link(&script, link_request()).expect("link");

        let graph = linked.import_graph();
        assert!(graph.root_imports.is_empty());
        assert!(graph.nodes.is_empty());
        assert_eq!(graph.root, script.source_hash());
        assert_eq!(linked.root().source_hash(), script.source_hash());

        // graph_hash / script_hash は §5.1 の byte-level encoding と一致する。
        let expected_graph =
            hash::import_graph_hash(LanguageRevision::CURRENT, script.source_hash(), &[], &[]);
        assert_eq!(graph.graph_hash.as_bytes(), &expected_graph);
        let expected_script = hash::linked_script_hash(
            LanguageRevision::CURRENT,
            script.source_hash(),
            graph.graph_hash,
        );
        assert_eq!(linked.script_hash().as_bytes(), &expected_script);
    }

    /// C6-c（CAP-AT-16）: import があるのに resolver を grant していない link は、resolver を一度も
    /// 呼ばず terminal `Denied(ResourceNotGranted / ModuleResolver / "module.resolve" / None)` を返す
    /// （Q2 = Denied all-in、設計 §4.5）。Phase 1 の `FeatureUnavailable` は返さない。
    #[test]
    fn link_denies_imports_without_resolver() {
        use crate::capability::{CapabilityKind, DenialCode};
        let engine = Engine::builder().build().unwrap();
        let script = engine
            .compile(
                src("m", "import \"other\"\nlet x = 1\n"),
                &CompileOptions::default(),
            )
            .unwrap();
        // link_request() は empty capabilities（resolver 未 grant）。
        let err = engine
            .link(&script, link_request())
            .expect_err("import は resolver 未 grant で Denied");
        match err {
            LinkError::Denied(denial) => {
                assert_eq!(denial.code, DenialCode::ResourceNotGranted);
                assert_eq!(denial.capability, CapabilityKind::ModuleResolver);
                assert_eq!(denial.operation.as_str(), "module.resolve");
                assert_eq!(denial.public_resource, None);
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn link_rejects_foreign_engine() {
        let engine_a = Engine::builder().build().unwrap();
        let engine_b = Engine::builder().build().unwrap();
        let script = engine_a
            .compile(src("m", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        let err = engine_b
            .link(&script, link_request())
            .expect_err("foreign engine");
        assert_eq!(err, LinkError::EngineMismatch);
    }

    // --- E3: tree backend adapter（実行入口）---

    fn retained(id: &str, text: &str) -> Source<'static> {
        src(id, text)
    }

    /// compile(retain) → link → run が Completed を返す。
    #[test]
    fn run_completes_import_less_script() {
        let engine = Engine::builder().build().unwrap();
        let script = engine
            .compile(
                retained("m", "let x = 1\nlet y = x + 2\n"),
                &CompileOptions {
                    retain_source: true,
                },
            )
            .unwrap();
        let linked = engine.link(&script, link_request()).unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let outcome = engine.run(&linked, &mut ctx, standard_request());
        assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
    }

    /// 未捕捉 runtime error は RuntimeError outcome へ写り、code / trace が付く。
    #[test]
    fn run_surfaces_runtime_error_with_code_and_trace() {
        let engine = Engine::builder().build().unwrap();
        // 未定義変数の参照 → Name エラー。
        let script = engine
            .compile(
                retained("m", "let x = undefined_name\n"),
                &CompileOptions {
                    retain_source: true,
                },
            )
            .unwrap();
        let linked = engine.link(&script, link_request()).unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        match engine.run(&linked, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, ErrorKind::Name);
                assert_eq!(error.line, Some(1));
            }
            other => panic!("期待: RuntimeError, 実際: {other:?}"),
        }
    }

    /// 関数内で発生したエラーは trace に呼び出し経路を持つ。
    #[test]
    fn run_runtime_error_includes_call_trace() {
        let engine = Engine::builder().build().unwrap();
        let source = "fn boom()\n  return missing\nend\nlet r = boom()\n";
        let script = engine
            .compile(
                retained("m", source),
                &CompileOptions {
                    retain_source: true,
                },
            )
            .unwrap();
        let linked = engine.link(&script, link_request()).unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        match engine.run(&linked, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, ErrorKind::Name);
                assert!(
                    error.trace.iter().any(|f| f.function == "boom"),
                    "trace に boom を含むべき: {:?}",
                    error.trace
                );
            }
            other => panic!("期待: RuntimeError, 実際: {other:?}"),
        }
    }

    /// retain_source=false の script を実行すると InternalFailure（host 前提条件違反）。
    #[test]
    fn run_without_retained_source_is_internal_failure() {
        let engine = Engine::builder().build().unwrap();
        let script = engine
            .compile(retained("m", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        let linked = engine.link(&script, link_request()).unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        match engine.run(&linked, &mut ctx, standard_request()) {
            ExecutionOutcome::InternalFailure { fault_id, .. } => {
                assert_ne!(fault_id, 0);
            }
            other => panic!("期待: InternalFailure, 実際: {other:?}"),
        }
    }

    /// 別 engine の context で実行すると InternalFailure（engine 不一致）。
    #[test]
    fn run_rejects_foreign_context() {
        let engine_a = Engine::builder().build().unwrap();
        let engine_b = Engine::builder().build().unwrap();
        let script = engine_a
            .compile(
                retained("m", "let x = 1\n"),
                &CompileOptions {
                    retain_source: true,
                },
            )
            .unwrap();
        let linked = engine_a.link(&script, link_request()).unwrap();
        let mut foreign_ctx = ExecutionContext::new(&engine_b);
        assert!(matches!(
            engine_a.run(&linked, &mut foreign_ctx, standard_request()),
            ExecutionOutcome::InternalFailure { .. }
        ));
    }

    /// 同一 context の再利用で binding が保持される。
    #[test]
    fn run_reuses_context_bindings() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let first = engine
            .compile(
                retained("m", "let saved = 41\n"),
                &CompileOptions {
                    retain_source: true,
                },
            )
            .unwrap();
        let linked1 = engine.link(&first, link_request()).unwrap();
        assert!(matches!(
            engine.run(&linked1, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));

        // 直前の実行で定義した saved を参照できる（同一 context）。
        let second = engine
            .compile(
                retained("m", "let doubled = saved + 1\n"),
                &CompileOptions {
                    retain_source: true,
                },
            )
            .unwrap();
        let linked2 = engine.link(&second, link_request()).unwrap();
        assert!(matches!(
            engine.run(&linked2, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// ExecutionContext は !Send / !Sync（stable 契約、第9.1節）であることを型で示す。
    /// （コンパイルが通ること自体が確認。ここでは Send/Sync を要求しない用途で使う。）
    #[test]
    fn execution_context_is_usable_single_threaded() {
        let engine = Engine::builder().build().unwrap();
        let ctx = ExecutionContext::new(&engine);
        assert_eq!(ctx.engine_id(), engine.id());
    }

    /// EMB-AT-13: root hash / graph hash の golden 値を固定し、1 byte 変更で差が出る。
    #[test]
    fn golden_hashes_are_stable() {
        let engine = Engine::builder().build().unwrap();
        let script = engine
            .compile(src("golden", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        // root source hash の golden（"let x = 1\n" の SHA-256）。
        assert_eq!(
            hex(script.source_hash().as_bytes()),
            hex(&hash::sha256(b"let x = 1\n"))
        );
        let linked = engine.link(&script, link_request()).unwrap();
        // graph_hash と script_hash が決定的であること（同一入力で不変）。
        let script2 = engine
            .compile(src("golden", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        let linked2 = engine.link(&script2, link_request()).unwrap();
        assert_eq!(
            linked.import_graph().graph_hash,
            linked2.import_graph().graph_hash
        );
        assert_eq!(linked.script_hash(), linked2.script_hash());
    }

    // --- E4: Context cleanup / transaction / poison / reuse ---

    /// retain_source=true の linked script を作る小さなヘルパ。
    fn compile_link(engine: &Engine, id: &str, text: &str) -> LinkedScript {
        let script = engine
            .compile(
                retained(id, text),
                &CompileOptions {
                    retain_source: true,
                },
            )
            .unwrap();
        engine.link(&script, link_request()).unwrap()
    }

    /// EMB-AT-08: 未捕捉 runtime error は execution 開始時点まで全 language-state を rollback し、
    /// 副作用は次実行へ残らない（AUD-024・第10節 規則5）。
    #[test]
    fn e4_runtime_error_rolls_back_language_state() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        // 変数を代入した直後に未定義参照でエラー化する。rollback されれば committed は残らない。
        let linked = compile_link(&engine, "m", "let saved = 7\nlet boom = undefined_name\n");
        match engine.run(&linked, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => assert_eq!(error.code, ErrorKind::Name),
            other => panic!("期待: RuntimeError, 実際: {other:?}"),
        }

        // rollback 済みなので saved は次実行から見えない（見えれば Name エラーで判別できる）。
        let probe = compile_link(&engine, "m", "let echo = saved\n");
        match engine.run(&probe, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => assert_eq!(error.code, ErrorKind::Name),
            other => panic!("saved が rollback されず残った: {other:?}"),
        }
    }

    /// EMB-AT-08: Completed は全 language-state を commit し、次実行へ binding が残る。
    #[test]
    fn e4_completed_commits_language_state() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let first = compile_link(&engine, "m", "let saved = 41\n");
        assert!(matches!(
            engine.run(&first, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
        // commit 済みなので次実行から saved を参照できる。
        let second = compile_link(&engine, "m", "let doubled = saved + 1\n");
        assert!(matches!(
            engine.run(&second, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// EMB-AT-08: script 内で catch して正常完了したエラーは commit する（rollback しない）。
    #[test]
    fn e4_caught_error_then_completed_commits() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let linked = compile_link(
            &engine,
            "m",
            "let saved = 0\ntry\n  let x = undefined_name\ncatch e\n  saved = 5\nend\n",
        );
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
        // catch 後に代入した saved=5 は commit され、次実行から見える。
        let probe = compile_link(&engine, "m", "let echo = saved + 1\n");
        assert!(matches!(
            engine.run(&probe, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// EMB-AT-07: RuntimeError で終わっても context は poison されず、再利用できる。
    #[test]
    fn e4_runtime_error_does_not_poison() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let bad = compile_link(&engine, "m", "let x = undefined_name\n");
        assert!(matches!(
            engine.run(&bad, &mut ctx, standard_request()),
            ExecutionOutcome::RuntimeError { .. }
        ));
        assert!(!ctx.is_poisoned(), "RuntimeError は poison しない");

        // そのまま再利用して正常実行できる。
        let ok = compile_link(&engine, "m", "let y = 1\n");
        assert!(matches!(
            engine.run(&ok, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// EMB-AT-07: InternalFailure は context を poison し、以後の実行を拒否する。
    #[test]
    fn e4_internal_failure_poisons_and_blocks_reuse() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        // retain_source=false → 実行入口の前提条件違反で InternalFailure（案 A）。
        let script = engine
            .compile(retained("m", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        let linked = engine.link(&script, link_request()).unwrap();
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request()),
            ExecutionOutcome::InternalFailure { .. }
        ));
        assert!(ctx.is_poisoned(), "InternalFailure は poison する");

        // poison 済み context は以後 InternalFailure を返す（正しい retained script でも）。
        let ok = compile_link(&engine, "m", "let y = 1\n");
        assert!(matches!(
            engine.run(&ok, &mut ctx, standard_request()),
            ExecutionOutcome::InternalFailure { .. }
        ));
        // 状態操作も Poisoned で拒否する。
        assert_eq!(ctx.clear_user_state(), Err(ContextError::Poisoned));
    }

    /// clear_user_state は user state を捨て、以後の binding 参照を消す。
    #[test]
    fn e4_clear_user_state_discards_bindings() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let seed = compile_link(&engine, "m", "let saved = 9\n");
        assert!(matches!(
            engine.run(&seed, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
        ctx.clear_user_state().expect("clear on idle context");

        // clear 後は saved が見えない（見えれば commit されている）。
        let probe = compile_link(&engine, "m", "let echo = saved\n");
        match engine.run(&probe, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => assert_eq!(error.code, ErrorKind::Name),
            other => panic!("clear 後も saved が残った: {other:?}"),
        }
    }

    /// ContextError は Send + Sync（診断値として host が保持しやすいように）。
    #[test]
    fn e4_context_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ContextError>();
    }

    // =======================================================================
    // スライス E5: terminal channel（pre-run Cancelled、catch 規則）
    // =======================================================================

    /// EMB-AT-09 / EMB-AT-12: pre-run cancel は命令を1つも実行せず Cancelled を返す。
    #[test]
    fn e5_pre_run_cancel_returns_cancelled_without_running() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        // pre-cancel。副作用（binding）が起きれば後段の probe で検出できる。
        let linked = compile_link(&engine, "m", "let saved = 123\n");
        let outcome = engine.run(&linked, &mut ctx, standard_request_cancelled());
        assert!(matches!(outcome, ExecutionOutcome::Cancelled { .. }));

        // 命令0なので saved は commit されない（見えれば Name エラーで判別できる）。
        let probe = compile_link(&engine, "m", "let echo = saved\n");
        match engine.run(&probe, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => assert_eq!(error.code, ErrorKind::Name),
            other => panic!("pre-cancel が副作用を残した: {other:?}"),
        }
    }

    /// EMB-AT-07: pre-run cancel は context を poison せず、再利用できる。
    #[test]
    fn e5_pre_run_cancel_does_not_poison() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let linked = compile_link(&engine, "m", "let x = 1\n");
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request_cancelled()),
            ExecutionOutcome::Cancelled { .. }
        ));
        assert!(!ctx.is_poisoned(), "Cancelled は poison しない");

        // そのまま再利用して正常実行できる。
        let ok = compile_link(&engine, "m", "let y = 2\n");
        assert!(matches!(
            engine.run(&ok, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// pre-run cancel は poison 済み context では実行前提条件が優先される（InternalFailure）。
    #[test]
    fn e5_pre_run_cancel_respects_poison_precondition() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        // retain_source=false で poison させる。
        let script = engine
            .compile(retained("m", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        let linked = engine.link(&script, link_request()).unwrap();
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request()),
            ExecutionOutcome::InternalFailure { .. }
        ));

        // poison 後は pre-cancel でも InternalFailure（precondition 優先、第10節 規則4）。
        let ok = compile_link(&engine, "m", "let y = 1\n");
        assert!(matches!(
            engine.run(&ok, &mut ctx, standard_request_cancelled()),
            ExecutionOutcome::InternalFailure { .. }
        ));
    }

    // =======================================================================
    // スライス E6: compile / link / run panic 隔離（fault ID）
    // =======================================================================

    /// panic hook を一時的に無音化して panic を誘発するテストを実行する。
    fn with_silent_panic_hook<T>(f: impl FnOnce() -> T) -> T {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = f();
        std::panic::set_hook(previous);
        result
    }

    /// catch_host_unwind は panic を捕捉し、非ゼロ fault ID を返す（payload を漏らさない）。
    #[test]
    fn e6_catch_host_unwind_captures_panic_as_fault_id() {
        // 正常経路は値をそのまま返す。
        assert_eq!(catch_host_unwind(|| 7), Ok(7));

        // panic 経路は Err(fault_id)。fault_id は非ゼロ。
        let fault = with_silent_panic_hook(|| {
            catch_host_unwind(|| panic!("secret internal detail")).unwrap_err()
        });
        assert_ne!(fault, 0);
    }

    /// internal_fault_message は fault ID を載せるが panic payload は載せない（第11節 規則3）。
    #[test]
    fn e6_internal_fault_message_is_secret_free() {
        let msg = internal_fault_message(42);
        assert!(msg.contains("42"), "fault ID を相関できること");
        // panic payload に使った文字列は含まれない。
        assert!(!msg.contains("secret"));
    }

    /// run 中に評価器が panic すると terminal InternalFailure（非ゼロ fault ID）で、
    /// context は poison される（第11節 規則2/4）。
    #[test]
    fn e6_run_panic_maps_to_internal_failure_and_poisons() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        // 実行スレッド上で評価器を panic させる注入 hook（test 専用）。
        ctx.inject_run_panic_for_test();
        let linked = compile_link(&engine, "m", "let x = 1\n");
        let outcome = with_silent_panic_hook(|| engine.run(&linked, &mut ctx, standard_request()));
        match outcome {
            ExecutionOutcome::InternalFailure {
                fault_id,
                safe_message,
                ..
            } => {
                assert_ne!(fault_id, 0);
                assert!(!safe_message.contains("injected"));
            }
            other => panic!("期待: InternalFailure, 実際: {other:?}"),
        }
        // panic 経路も context を poison する。
        assert!(ctx.is_poisoned(), "run panic は context を poison する");
        // running フラグは panic 後も解除され、再入検査で誤検出しない。
        let ok = compile_link(&engine, "m", "let y = 1\n");
        assert!(matches!(
            engine.run(&ok, &mut ctx, standard_request()),
            ExecutionOutcome::InternalFailure { .. }
        ));
    }

    // --- Phase 6 A-1: audited terminal coverage（各 terminal 種別が Terminal を 1 件出す） ---

    /// 監査 execution_id 付きの standard request を作る helper。
    fn audited_request(id: u128) -> ExecutionRequest {
        standard_request().with_execution_id(ExecutionId::new(id.try_into().expect("nonzero")))
    }

    /// sink の収集 envelope から最後の Terminal event の outcome を取り出す helper。
    fn terminal_outcome_of(
        sink: &crate::audit::InMemoryAuditSink,
    ) -> crate::audit::TerminalOutcome {
        let envelopes = sink.snapshot();
        assert_eq!(
            envelopes.len(),
            2,
            "Started + Terminal の 2 件のはず: {envelopes:?}"
        );
        assert_eq!(envelopes[0].sequence, 0);
        assert!(matches!(
            envelopes[0].event,
            crate::audit::AuditEvent::ExecutionStarted { .. }
        ));
        assert_eq!(envelopes[1].sequence, 1);
        match &envelopes[1].event {
            crate::audit::AuditEvent::Terminal { outcome, .. } => outcome.clone(),
            other => panic!("期待 Terminal, 実際 {other:?}"),
        }
    }

    /// sink の収集 envelope から ExecutionStarted の capability_policy_hash を取り出す helper。
    fn started_capability_policy_hash(sink: &crate::audit::InMemoryAuditSink) -> [u8; 32] {
        let envelopes = sink.snapshot();
        assert_eq!(envelopes[0].sequence, 0);
        match &envelopes[0].event {
            crate::audit::AuditEvent::ExecutionStarted {
                capability_policy_hash,
                ..
            } => *capability_policy_hash,
            other => panic!("期待 ExecutionStarted, 実際 {other:?}"),
        }
    }

    /// ExecutionStarted.capability_policy_hash は実行を認可した frozen policy の相関 ID であり、
    /// 権限内容が異なれば異なる値を emit する（deny-by-default と grant 済みを監査上区別できる）。
    #[test]
    fn audited_started_records_capability_policy_hash() {
        // (1) deny-by-default（空 set）。
        let empty_sink = Arc::new(crate::audit::InMemoryAuditSink::new());
        let engine = Engine::builder()
            .audit_sink(empty_sink.clone() as Arc<dyn crate::audit::AuditSink>)
            .build()
            .unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "let x = 1\n");
        let _ = engine.run_audited(&linked, &mut ctx, audited_request(201));
        let empty_hash = started_capability_policy_hash(&empty_sink);

        // (2) exit を grant した set。
        let exit_sink = Arc::new(crate::audit::InMemoryAuditSink::new());
        let engine2 = Engine::builder()
            .audit_sink(exit_sink.clone() as Arc<dyn crate::audit::AuditSink>)
            .build()
            .unwrap();
        let mut ctx2 = ExecutionContext::new(&engine2);
        let linked2 = compile_link(&engine2, "m", "exit(0)\n");
        let _ = engine2.run_audited(
            &linked2,
            &mut ctx2,
            audited_request(202).with_capabilities(exit_granted()),
        );
        let exit_hash = started_capability_policy_hash(&exit_sink);

        // hash は決定的に CapabilitySet::id() から導かれ、空値ではなく、内容差で異なる。
        assert_eq!(
            empty_hash,
            *crate::capability::CapabilitySet::empty().id().as_bytes(),
            "deny-by-default の hash は空 set の CapabilitySetId に一致する"
        );
        assert_eq!(
            exit_hash,
            *exit_granted().id().as_bytes(),
            "grant 済みの hash は対応する CapabilitySetId に一致する"
        );
        assert_ne!(
            empty_hash, exit_hash,
            "権限内容が異なれば capability_policy_hash も異なる"
        );
        assert_ne!(exit_hash, [0u8; 32], "grant 済みの hash は全 0 ではない");
    }

    /// granted な exit(code) は audited 経路で Terminal(Exited) を 1 件出す。
    #[test]
    fn audited_exit_emits_exited_terminal() {
        let sink = Arc::new(crate::audit::InMemoryAuditSink::new());
        let engine = Engine::builder()
            .audit_sink(sink.clone() as Arc<dyn crate::audit::AuditSink>)
            .build()
            .unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "exit(7)\n");
        let audited = engine.run_audited(
            &linked,
            &mut ctx,
            audited_request(101).with_capabilities(exit_granted()),
        );
        assert!(matches!(
            audited.outcome(),
            Some(ExecutionOutcome::Exited { code: 7, .. })
        ));
        assert_eq!(
            terminal_outcome_of(&sink),
            crate::audit::TerminalOutcome::Exited(7)
        );
    }

    /// fuel を使い切る実行は audited 経路で Terminal(BudgetExceeded(Fuel)) を 1 件出す。
    #[test]
    fn audited_budget_exceeded_emits_budget_terminal_with_resource() {
        let clock: Arc<dyn crate::budget::MonotonicClock> =
            Arc::new(crate::budget::FakeClock::new());
        let mut budget = crate::budget::BudgetConfig::standard(clock.as_ref()).unwrap();
        budget.total_fuel = 5; // ループ反復で step 上限に達する極小 fuel。
        let request = ExecutionRequest::new(budget, clock)
            .expect("同 domain")
            .with_execution_id(ExecutionId::new(102u128.try_into().unwrap()));

        let sink = Arc::new(crate::audit::InMemoryAuditSink::new());
        let engine = Engine::builder()
            .audit_sink(sink.clone() as Arc<dyn crate::audit::AuditSink>)
            .build()
            .unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(
            &engine,
            "m",
            "let i = 0\nwhile i < 1000\n  i = i + 1\nend\n",
        );
        let audited = engine.run_audited(&linked, &mut ctx, request);
        assert!(matches!(
            audited.outcome(),
            Some(ExecutionOutcome::BudgetExceeded { .. })
        ));
        // 原本 resource（Fuel）が §7.2 どおり載る。
        assert_eq!(
            terminal_outcome_of(&sink),
            crate::audit::TerminalOutcome::BudgetExceeded(crate::budget::BudgetResource::Fuel)
        );
    }

    /// deadline 到達は audited 経路で Terminal(DeadlineExceeded) を 1 件出す。
    #[test]
    fn audited_deadline_emits_deadline_terminal() {
        let clock = Arc::new(crate::budget::FakeClock::new());
        let budget = crate::budget::BudgetConfig::standard(clock.as_ref()).unwrap();
        let request = ExecutionRequest::new(
            budget,
            clock.clone() as Arc<dyn crate::budget::MonotonicClock>,
        )
        .expect("同 domain")
        .with_execution_id(ExecutionId::new(103u128.try_into().unwrap()));
        clock.set(budget.deadline.as_nanos());

        let sink = Arc::new(crate::audit::InMemoryAuditSink::new());
        let engine = Engine::builder()
            .audit_sink(sink.clone() as Arc<dyn crate::audit::AuditSink>)
            .build()
            .unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "let x = 1\nlet y = 2\n");
        let audited = engine.run_audited(&linked, &mut ctx, request);
        assert!(matches!(
            audited.outcome(),
            Some(ExecutionOutcome::DeadlineExceeded { .. })
        ));
        assert_eq!(
            terminal_outcome_of(&sink),
            crate::audit::TerminalOutcome::DeadlineExceeded
        );
    }

    /// run 中の panic は audited 経路でも Terminal(InternalFailure) を 1 件出し、context を poison。
    #[test]
    fn audited_panic_emits_internal_failure_terminal() {
        let sink = Arc::new(crate::audit::InMemoryAuditSink::new());
        let engine = Engine::builder()
            .audit_sink(sink.clone() as Arc<dyn crate::audit::AuditSink>)
            .build()
            .unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        ctx.inject_run_panic_for_test();
        let linked = compile_link(&engine, "m", "let x = 1\n");
        let audited =
            with_silent_panic_hook(|| engine.run_audited(&linked, &mut ctx, audited_request(104)));
        match audited.outcome() {
            Some(ExecutionOutcome::InternalFailure { fault_id, .. }) => assert_ne!(*fault_id, 0),
            other => panic!("期待 InternalFailure, 実際 {other:?}"),
        }
        assert_eq!(
            terminal_outcome_of(&sink),
            crate::audit::TerminalOutcome::InternalFailure
        );
        // audited 経路でも panic は context を poison する。
        assert!(ctx.is_poisoned());
    }

    /// no-sink の lifecycle regression: run は実行中 running を立て、戻ると下ろす。再入は弾く。
    #[test]
    fn no_sink_run_sets_and_clears_running_flag() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        assert!(!ctx.running, "実行前は running=false");
        let linked = compile_link(&engine, "m", "let x = 1\n");
        let outcome = engine.run(&linked, &mut ctx, standard_request());
        assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
        // 戻ると running は確実に下りている（次の実行が再入検査で誤検出しない）。
        assert!(!ctx.running, "実行後は running=false へ戻る");
        let again = engine.run(&linked, &mut ctx, standard_request());
        assert!(matches!(again, ExecutionOutcome::Completed { .. }));
    }

    /// LinkError::InternalFailure は Send + Sync な診断値として保持できる。
    #[test]
    fn e6_link_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LinkError>();
    }

    // --- C7: ProcessExit（exit → Exited terminal、REV-023）---

    /// ProcessExit を grant した capability 集合を作る。
    fn exit_granted() -> crate::capability::CapabilitySet {
        use std::num::NonZeroU128;
        crate::capability::CapabilitySet::builder()
            .process_exit(crate::capability::ProcessExit::new(
                NonZeroU128::new(1).unwrap(),
            ))
            .unwrap()
            .build()
    }

    /// granted な exit(code) は Exited terminal になる（CAP-AT-18 / EMB-AT-11）。
    #[test]
    fn c7_exit_granted_maps_to_exited() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "exit(7)\n");
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(exit_granted()),
        );
        assert!(matches!(outcome, ExecutionOutcome::Exited { code: c, .. } if c == (7)));
    }

    /// exit(0) / exit(255) は境界値として Exited（EMB-AT-11）。
    #[test]
    fn c7_exit_boundary_codes_are_exited() {
        let engine = Engine::builder().build().unwrap();
        for (src_text, code) in [("exit(0)\n", 0u8), ("exit(255)\n", 255u8)] {
            let mut ctx = ExecutionContext::new(&engine);
            let linked = compile_link(&engine, "m", src_text);
            let outcome = engine.run(
                &linked,
                &mut ctx,
                standard_request().with_capabilities(exit_granted()),
            );
            assert!(matches!(outcome, ExecutionOutcome::Exited { code: got, .. } if got == code));
        }
    }

    /// Exited は Completed と同じく直前までの language-state を commit する（第10節 規則5）。
    #[test]
    fn c7_exit_commits_language_state() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let first = compile_link(&engine, "m", "let saved = 41\nexit(0)\n");
        assert!(matches!(
            engine.run(
                &first,
                &mut ctx,
                standard_request().with_capabilities(exit_granted())
            ),
            ExecutionOutcome::Exited { code: 0, .. }
        ));
        // 次の実行で saved が見える（commit されている）。
        let probe = compile_link(&engine, "m", "let echo = saved\n");
        assert!(matches!(
            engine.run(
                &probe,
                &mut ctx,
                standard_request().with_capabilities(exit_granted())
            ),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// ProcessExit 未 grant の exit() は catch 可能な capability error（未捕捉→RuntimeError）。
    /// OS/process は継続する（EMB-AT-11 / CAP-AT-18）。
    #[test]
    fn c7_exit_ungranted_is_runtime_error() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        // 既定 request は deny-by-default（capability なし）。
        let linked = compile_link(&engine, "m", "exit(0)\n");
        match engine.run(&linked, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, crate::error::ErrorKind::Capability);
            }
            other => panic!("期待: RuntimeError(capability), 実際: {other:?}"),
        }
    }

    /// 未 grant の exit() は script から catch できる（catchable capability error）。
    #[test]
    fn c7_exit_ungranted_is_catchable() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(
            &engine,
            "m",
            "try\n  exit(0)\ncatch e\n  let caught = e[\"type\"]\nend\n",
        );
        // catch されれば正常完了する。
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// granted でも 0..=255 の範囲外は catch 可能な argument error（EMB-AT-11）。
    #[test]
    fn c7_exit_out_of_range_is_runtime_error() {
        let engine = Engine::builder().build().unwrap();
        for src_text in ["exit(256)\n", "exit(0 - 1)\n"] {
            let mut ctx = ExecutionContext::new(&engine);
            let linked = compile_link(&engine, "m", src_text);
            match engine.run(
                &linked,
                &mut ctx,
                standard_request().with_capabilities(exit_granted()),
            ) {
                ExecutionOutcome::RuntimeError { error, .. } => {
                    assert_eq!(error.code, crate::error::ErrorKind::Argument);
                }
                other => panic!("期待: RuntimeError(argument), 実際: {other:?}"),
            }
        }
    }

    /// granted な exit() は script の try/catch で捕捉できない uncatchable terminal（規則4）。
    #[test]
    fn c7_granted_exit_is_uncatchable() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(
            &engine,
            "m",
            "try\n  exit(3)\ncatch e\n  print(\"caught\")\nend\n",
        );
        // catch を素通りして Exited terminal になる。
        assert!(matches!(
            engine.run(
                &linked,
                &mut ctx,
                standard_request().with_capabilities(exit_granted())
            ),
            ExecutionOutcome::Exited { code: 3, .. }
        ));
    }

    /// exit terminal 後も context は poison されず再利用できる（規則4）。
    #[test]
    fn c7_exit_does_not_poison_context() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "exit(2)\n");
        engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(exit_granted()),
        );
        assert!(!ctx.is_poisoned());
        // 再利用できる。
        let ok = compile_link(&engine, "m", "let x = 1\n");
        assert!(matches!(
            engine.run(&ok, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    // --- C3: Environment / Clock（env / now capability、CAP-AT-05 / CAP-AT-06）---

    use std::num::NonZeroU128;
    use std::sync::Arc;

    /// 固定 policy_id の ProcessExit を作る（結果観測のため env/clock テストへ相乗り）。
    fn process_exit() -> crate::capability::ProcessExit {
        crate::capability::ProcessExit::new(NonZeroU128::new(1).unwrap())
    }

    /// 指定 env snapshot（＋結果観測用 ProcessExit）を grant した set。
    fn env_granted(entries: &[(&str, &str)]) -> crate::capability::CapabilitySet {
        use crate::capability::{DataClassification, EnvironmentSnapshot, EnvironmentValue};
        let snapshot = EnvironmentSnapshot::from_entries(entries.iter().map(|(k, v)| {
            (
                (*k).to_string(),
                EnvironmentValue::new(*v, DataClassification::Public).unwrap(),
            )
        }))
        .unwrap();
        crate::capability::CapabilitySet::builder()
            .environment(snapshot)
            .unwrap()
            .process_exit(process_exit())
            .unwrap()
            .build()
    }

    /// 指定 Unix 秒の FixedClock（＋結果観測用 ProcessExit）を grant した set。
    fn clock_granted(secs: u64) -> crate::capability::CapabilitySet {
        use crate::capability::FixedClock;
        use std::time::{Duration, UNIX_EPOCH};
        let instant = UNIX_EPOCH + Duration::from_secs(secs);
        crate::capability::CapabilitySet::builder()
            .clock(Arc::new(FixedClock::new(
                NonZeroU128::new(1).unwrap(),
                instant,
            )))
            .unwrap()
            .process_exit(process_exit())
            .unwrap()
            .build()
    }

    /// Environment grant 済みなら env(key) は snapshot の値を返す（CAP-AT-05）。
    /// 値 "7" を exit code へ写して観測する。
    #[test]
    fn c3_env_granted_reads_snapshot() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "exit(to_int(env(\"CODE\")))\n");
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(env_granted(&[("CODE", "7")])),
        );
        assert!(matches!(outcome, ExecutionOutcome::Exited { code: c, .. } if c == (7)));
    }

    /// Environment grant 済みでも key が snapshot に無ければ null（error にしない、CAP-AT-05）。
    #[test]
    fn c3_env_granted_missing_key_is_null() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        // env("MISSING") == null なら exit(5)、そうでなければ exit(9)。
        let linked = compile_link(
            &engine,
            "m",
            "if env(\"MISSING\") == null\n  exit(5)\nelse\n  exit(9)\nend\n",
        );
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(env_granted(&[("CODE", "7")])),
        );
        assert!(matches!(outcome, ExecutionOutcome::Exited { code: c, .. } if c == (5)));
    }

    /// Environment 未 grant の env() は catch 可能な capability error（未捕捉→RuntimeError、
    /// CAP-AT-05）。process env へは触れない。
    #[test]
    fn c3_env_ungranted_is_runtime_error() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        // 既定 request は deny-by-default（Environment なし）。
        let linked = compile_link(&engine, "m", "let x = env(\"CODE\")\n");
        match engine.run(&linked, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, crate::error::ErrorKind::Capability);
            }
            other => panic!("期待: RuntimeError(capability), 実際: {other:?}"),
        }
    }

    /// 未 grant の env() は script から catch できる（catchable capability error）。
    #[test]
    fn c3_env_ungranted_is_catchable() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(
            &engine,
            "m",
            "try\n  let x = env(\"CODE\")\ncatch e\n  let caught = e[\"type\"]\nend\n",
        );
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// snapshot は start 前に固定され、実行中に process env を再読しない（CAP-AT-05）。
    /// grant した snapshot に無い実 process env の key は見えない。
    #[test]
    fn c3_env_snapshot_is_fixed_and_isolated_from_process_env() {
        // 実 process env を汚しても snapshot 経由では見えないことを確認する。
        // safety: テスト専用の一時 key。
        unsafe {
            std::env::set_var("TSG_C3_PROBE", "leak");
        }
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(
            &engine,
            "m",
            "if env(\"TSG_C3_PROBE\") == null\n  exit(1)\nelse\n  exit(2)\nend\n",
        );
        // grant した snapshot は TSG_C3_PROBE を含まない → null → exit(1)。
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(env_granted(&[("CODE", "7")])),
        );
        unsafe {
            std::env::remove_var("TSG_C3_PROBE");
        }
        assert!(matches!(outcome, ExecutionOutcome::Exited { code: c, .. } if c == (1)));
    }

    /// Clock grant 済みなら now() は FixedClock の時刻を Unix 秒で返す（CAP-AT-06）。
    #[test]
    fn c3_now_granted_reads_fixed_clock() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        // now() を exit code へ写す（0..=255 に収まる固定秒）。
        let linked = compile_link(&engine, "m", "exit(now())\n");
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(clock_granted(42)),
        );
        assert!(matches!(outcome, ExecutionOutcome::Exited { code: c, .. } if c == (42)));
    }

    /// 同じ FixedClock は同じ結果を返す（決定的、CAP-AT-06）。
    #[test]
    fn c3_now_fixed_clock_is_deterministic() {
        let engine = Engine::builder().build().unwrap();
        for _ in 0..3 {
            let mut ctx = ExecutionContext::new(&engine);
            let linked = compile_link(&engine, "m", "exit(now())\n");
            let outcome = engine.run(
                &linked,
                &mut ctx,
                standard_request().with_capabilities(clock_granted(100)),
            );
            assert!(matches!(outcome, ExecutionOutcome::Exited { code: c, .. } if c == (100)));
        }
    }

    /// Clock 未 grant の now() は catch 可能な capability error（未捕捉→RuntimeError、
    /// CAP-AT-06）。system clock へは触れない。
    #[test]
    fn c3_now_ungranted_is_runtime_error() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "let t = now()\n");
        match engine.run(&linked, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, crate::error::ErrorKind::Capability);
            }
            other => panic!("期待: RuntimeError(capability), 実際: {other:?}"),
        }
    }

    /// 未 grant の now() は script から catch できる（catchable capability error）。
    #[test]
    fn c3_now_ungranted_is_catchable() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(
            &engine,
            "m",
            "try\n  let t = now()\ncatch e\n  let caught = e[\"type\"]\nend\n",
        );
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    // --- C4: Stdin / Stdout（input / print capability、CAP-AT-07 / CAP-AT-08）---

    use std::sync::Mutex;

    /// 書き込まれた bytes を蓄積する観測用 Output double（FixedClock の stdout 版）。
    struct CapturingOutput {
        policy_id: NonZeroU128,
        sink: Arc<Mutex<Vec<u8>>>,
    }

    impl crate::capability::Output for CapturingOutput {
        fn policy_id(&self) -> NonZeroU128 {
            self.policy_id
        }
        fn write_all(&self, bytes: &[u8]) -> Result<(), crate::capability::AdapterError> {
            self.sink.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
        fn flush(&self) -> Result<(), crate::capability::AdapterError> {
            Ok(())
        }
    }

    /// 常に host 失敗を返す Output double（`host` error 経路の観測用）。
    struct FailingOutput(NonZeroU128);
    impl crate::capability::Output for FailingOutput {
        fn policy_id(&self) -> NonZeroU128 {
            self.0
        }
        fn write_all(&self, _bytes: &[u8]) -> Result<(), crate::capability::AdapterError> {
            Err(crate::capability::AdapterError::Host("boom".into()))
        }
        fn flush(&self) -> Result<(), crate::capability::AdapterError> {
            Ok(())
        }
    }

    /// 事前設定した行を順に返す観測用 Input double（決定的、FixedClock の stdin 版）。
    struct ScriptedInput {
        policy_id: NonZeroU128,
        lines: Mutex<std::collections::VecDeque<String>>,
    }

    impl crate::capability::Input for ScriptedInput {
        fn policy_id(&self) -> NonZeroU128 {
            self.policy_id
        }
        fn read_line(
            &self,
        ) -> Result<crate::capability::InputLine, crate::capability::AdapterError> {
            match self.lines.lock().unwrap().pop_front() {
                Some(line) => Ok(crate::capability::InputLine::Line(line)),
                None => Ok(crate::capability::InputLine::Eof),
            }
        }
    }

    /// 指定 sink の Output（＋結果観測用 ProcessExit）を grant した set。
    fn stdout_granted(sink: Arc<Mutex<Vec<u8>>>) -> crate::capability::CapabilitySet {
        crate::capability::CapabilitySet::builder()
            .stdout(Arc::new(CapturingOutput {
                policy_id: NonZeroU128::new(1).unwrap(),
                sink,
            }))
            .unwrap()
            .process_exit(process_exit())
            .unwrap()
            .build()
    }

    /// 指定行を返す Input（＋結果観測用 ProcessExit）を grant した set。
    fn stdin_granted(lines: &[&str]) -> crate::capability::CapabilitySet {
        let queue = lines.iter().map(|s| (*s).to_string()).collect();
        crate::capability::CapabilitySet::builder()
            .stdin(Arc::new(ScriptedInput {
                policy_id: NonZeroU128::new(1).unwrap(),
                lines: Mutex::new(queue),
            }))
            .unwrap()
            .process_exit(process_exit())
            .unwrap()
            .build()
    }

    /// Stdout grant 済みなら print は adapter へ書き出す（CAP-AT-07）。
    #[test]
    fn c4_print_granted_writes_to_adapter() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let sink = Arc::new(Mutex::new(Vec::new()));
        let linked = compile_link(&engine, "m", "print(\"hello\")\n");
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(stdout_granted(sink.clone())),
        );
        assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
        assert_eq!(sink.lock().unwrap().as_slice(), b"hello\n");
    }

    /// Stdout 未 grant の print は catch 可能な capability error（未捕捉→RuntimeError、CAP-AT-07）。
    #[test]
    fn c4_print_ungranted_is_runtime_error() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        // 既定 request は deny-by-default（Stdout なし）。
        let linked = compile_link(&engine, "m", "print(\"x\")\n");
        match engine.run(&linked, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, crate::error::ErrorKind::Capability);
            }
            other => panic!("期待: RuntimeError(capability), 実際: {other:?}"),
        }
    }

    /// 未 grant の print は script から catch できる（catchable capability error）。
    #[test]
    fn c4_print_ungranted_is_catchable() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(
            &engine,
            "m",
            "try\n  print(\"x\")\ncatch e\n  let caught = e[\"type\"]\nend\n",
        );
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// print の host adapter 失敗は catch 可能な `host` error で、null へ潰さない（CAP-AT-07）。
    #[test]
    fn c4_print_host_failure_is_host_error() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let set = crate::capability::CapabilitySet::builder()
            .stdout(Arc::new(FailingOutput(NonZeroU128::new(1).unwrap())))
            .unwrap()
            .process_exit(process_exit())
            .unwrap()
            .build();
        let linked = compile_link(&engine, "m", "print(\"x\")\n");
        match engine.run(&linked, &mut ctx, standard_request().with_capabilities(set)) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, crate::error::ErrorKind::Host);
            }
            other => panic!("期待: RuntimeError(host), 実際: {other:?}"),
        }
    }

    /// Stdin grant 済みなら input は adapter の行を返す（CAP-AT-08）。値 "8" を exit code へ写す。
    #[test]
    fn c4_input_granted_reads_from_adapter() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "exit(to_int(input()))\n");
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(stdin_granted(&["8"])),
        );
        assert!(matches!(outcome, ExecutionOutcome::Exited { code: c, .. } if c == (8)));
    }

    /// Stdin grant 済みで入力が尽きたら input は null（EOF、CAP-AT-08）。
    #[test]
    fn c4_input_granted_eof_is_null() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        // 入力なし（空 queue）→ EOF → null → exit(3)。
        let linked = compile_link(
            &engine,
            "m",
            "if input() == null\n  exit(3)\nelse\n  exit(9)\nend\n",
        );
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(stdin_granted(&[])),
        );
        assert!(matches!(outcome, ExecutionOutcome::Exited { code: c, .. } if c == (3)));
    }

    /// Stdin 未 grant の input は catch 可能な capability error（未捕捉→RuntimeError、CAP-AT-08）。
    #[test]
    fn c4_input_ungranted_is_runtime_error() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "let x = input()\n");
        match engine.run(&linked, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, crate::error::ErrorKind::Capability);
            }
            other => panic!("期待: RuntimeError(capability), 実際: {other:?}"),
        }
    }

    /// 未 grant の input は script から catch できる（catchable capability error）。
    #[test]
    fn c4_input_ungranted_is_catchable() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(
            &engine,
            "m",
            "try\n  let x = input()\ncatch e\n  let caught = e[\"type\"]\nend\n",
        );
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    // =======================================================================
    // スライス E11: budget / deadline / runtime cancel の公開 API 露出
    //   （EMB-AT-21 の Phase 3・tree 範囲）
    // =======================================================================

    use crate::budget::ConfigError as BudgetConfigError;
    use crate::budget::{BudgetConfig, CancellationToken, FakeClock};

    /// budget + clock から新 API の [`ExecutionRequest`] を作る helper（REV-015 最終形移行）。
    fn request_with(
        budget: BudgetConfig,
        clock: Arc<dyn crate::budget::MonotonicClock>,
    ) -> ExecutionRequest {
        ExecutionRequest::new(budget, clock).expect("budget は clock と同 domain なので検証を通る")
    }

    /// fuel を極小に絞った budget と、その deadline と同 domain の clock を返す helper。
    ///
    /// `for_legacy` の deadline は実在 clock に紐づかない（`clock_id` が合わず request 構築が
    /// `ForeignClock` で失敗する）ため、standard budget を基に `total_fuel` だけ差し替えて
    /// deadline domain の整合を保つ。deadline は遠い未来（now+30s）なので fuel 超過側だけを
    /// 踏む。
    fn small_fuel_budget(fuel: u64) -> (BudgetConfig, Arc<dyn crate::budget::MonotonicClock>) {
        let clock: Arc<dyn crate::budget::MonotonicClock> = Arc::new(FakeClock::new());
        let mut budget = BudgetConfig::standard(clock.as_ref()).unwrap();
        budget.total_fuel = fuel;
        (budget, clock)
    }

    /// EMB-AT-21: deadline に達すると DeadlineExceeded terminal で停止し、RuntimeError にしない。
    #[test]
    fn e11_deadline_exceeded_is_terminal() {
        let clock = Arc::new(FakeClock::new());
        // 30s 先の deadline を持つ既定 budget。
        let budget = BudgetConfig::standard(clock.as_ref()).unwrap();
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let request = request_with(
            budget,
            clock.clone() as Arc<dyn crate::budget::MonotonicClock>,
        );
        // clock を deadline ちょうどへ進めておく（charge 前 checkpoint で観測される）。request は
        // 構築済みなので検証（deadline>now）は進める前に通っている。
        clock.set(budget.deadline.as_nanos());

        let linked = compile_link(&engine, "m", "let x = 1\nlet y = 2\n");
        let outcome = engine.run(&linked, &mut ctx, request);
        assert!(matches!(outcome, ExecutionOutcome::DeadlineExceeded { .. }));
        // catch 不能 terminal なので poison しない・再利用できる。
        assert!(!ctx.is_poisoned());
    }

    /// EMB-AT-21 / EMB-AT-10: deadline は try/catch で捕捉できない（catch body に入らない）。
    #[test]
    fn e11_deadline_is_uncatchable() {
        let clock = Arc::new(FakeClock::new());
        let budget = BudgetConfig::standard(clock.as_ref()).unwrap();
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let request = request_with(
            budget,
            clock.clone() as Arc<dyn crate::budget::MonotonicClock>,
        );
        clock.set(budget.deadline.as_nanos());

        // catch body で binding を作っても、deadline は catch されず terminal になる。
        let linked = compile_link(
            &engine,
            "m",
            "try\n  let a = 1\ncatch e\n  let caught = 1\nend\n",
        );
        let outcome = engine.run(&linked, &mut ctx, request);
        assert!(matches!(outcome, ExecutionOutcome::DeadlineExceeded { .. }));
    }

    /// deadline 未達なら通常どおり Completed（clock を進めない）。
    #[test]
    fn e11_deadline_not_reached_completes() {
        let clock = Arc::new(FakeClock::new());
        let budget = BudgetConfig::standard(clock.as_ref()).unwrap();
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let linked = compile_link(&engine, "m", "let x = 1\nlet y = x + 2\n");
        let outcome = engine.run(
            &linked,
            &mut ctx,
            request_with(
                budget,
                clock.clone() as Arc<dyn crate::budget::MonotonicClock>,
            ),
        );
        assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
    }

    /// clock の domain（clock_id）が budget deadline と一致しないと、request 構築時に
    /// ForeignClock で弾かれる（run まで到達しない。REV-015 最終形移行 §11.1）。
    #[test]
    fn e11_foreign_deadline_clock_rejected_at_build() {
        let config_clock = FakeClock::new();
        let budget = BudgetConfig::standard(&config_clock).unwrap();

        // 別 clock（別 clock_id）を渡すと new が ForeignClock を返す。
        let foreign: Arc<dyn crate::budget::MonotonicClock> = Arc::new(FakeClock::new());
        let err = ExecutionRequest::new(budget, foreign)
            .expect_err("別 domain の clock は構築時に弾かれる");
        assert_eq!(err, BudgetConfigError::ForeignClock);
    }

    /// EMB-AT-21: request が所有する cancel 済み token の実行は Cancelled terminal で停止する。
    ///
    /// host は token を作って run 前に cancel し、`.cancellation(..)` で request に載せる。
    /// install 直後の pre-run 分岐で命令実行前に Cancelled になる（§8、§8.2 の役割分担で
    /// embedding 層は pre-run 分岐の固定に専念し、runtime checkpoint の catch 不能性は eval.rs の
    /// `cancel_after_install_is_uncatchable_terminal` が担保する）。
    #[test]
    fn e11_runtime_cancel_is_terminal() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let token = CancellationToken::new();
        assert!(token.cancel(), "最初の cancel は true を返す");

        let linked = compile_link(&engine, "m", "let saved = 7\nlet z = saved + 1\n");
        // host が握る token の clone を request に載せる。Engine::run が install するので、
        // host clone と install token は同一 Arc を共有する（finding 1 の回帰保証）。
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().cancellation(token.clone()),
        );
        assert!(matches!(outcome, ExecutionOutcome::Cancelled { .. }));
        assert!(!ctx.is_poisoned(), "Cancelled は poison しない");

        // cancel された実行は language-state を rollback する（saved は残らない）。
        let probe = compile_link(&engine, "m", "let echo = saved\n");
        match engine.run(&probe, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => assert_eq!(error.code, ErrorKind::Name),
            other => panic!("cancel が副作用を残した: {other:?}"),
        }
    }

    /// EMB-AT-10: request が所有する cancel 済み token は try/catch で捕捉できない。
    #[test]
    fn e11_runtime_cancel_is_uncatchable() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let token = CancellationToken::new();
        token.cancel();

        let linked = compile_link(
            &engine,
            "m",
            "try\n  let a = 1\ncatch e\n  let caught = 1\nend\n",
        );
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request().cancellation(token)),
            ExecutionOutcome::Cancelled { .. }
        ));
    }

    /// EMB-AT-21: fuel 上限を超えると BudgetExceeded terminal になり、RuntimeError にはしない。
    ///
    /// `small_fuel_budget` で fuel を極小に絞り、ループ反復で超過させる
    /// （単純な `let` の羅列は count_step を呼ばないためループを使う）。
    #[test]
    fn e11_budget_exceeded_is_terminal_not_runtime_error() {
        // fuel=5 の極小 budget。ループ反復で step 上限に達する。
        let (budget, clock) = small_fuel_budget(5);
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let linked = compile_link(
            &engine,
            "m",
            "let i = 0\nwhile i < 1000\n  i = i + 1\nend\n",
        );
        match engine.run(&linked, &mut ctx, request_with(budget, clock)) {
            ExecutionOutcome::BudgetExceeded { error, .. } => {
                // fuel 超過は StepLimit（canonical code "limit"）。
                assert_eq!(error.code, ErrorKind::StepLimit);
            }
            other => panic!("期待: BudgetExceeded, 実際: {other:?}"),
        }
        // catch 不能 terminal なので poison しない。
        assert!(!ctx.is_poisoned());
    }

    /// EMB-AT-21 / EMB-AT-10: budget 超過は try/catch で捕捉できない。
    #[test]
    fn e11_budget_exceeded_is_uncatchable() {
        let (budget, clock) = small_fuel_budget(5);
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let linked = compile_link(
            &engine,
            "m",
            "try\n  let i = 0\n  while i < 1000\n    i = i + 1\n  end\ncatch e\n  let caught = 1\nend\n",
        );
        match engine.run(&linked, &mut ctx, request_with(budget, clock)) {
            ExecutionOutcome::BudgetExceeded { .. } => {}
            other => panic!("期待: BudgetExceeded, 実際: {other:?}"),
        }
    }

    /// terminal outcome は commit 済みの `usage` を同梱する（仕様第6・12節）。
    ///
    /// ループを回す Completed は fuel を消費しているので、`usage.committed.fuel` が
    /// 0 より大きいことを確認する（usage が空のまま返っていないことの回帰固定）。
    #[test]
    fn e11_completed_carries_committed_usage() {
        let (budget, clock) = small_fuel_budget(1_000_000);
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let linked = compile_link(&engine, "m", "let i = 0\nwhile i < 10\n  i = i + 1\nend\n");
        match engine.run(&linked, &mut ctx, request_with(budget, clock)) {
            ExecutionOutcome::Completed { usage } => {
                assert!(
                    usage.committed.fuel > 0,
                    "Completed の usage に fuel が反映されていない: {usage:?}"
                );
            }
            other => panic!("期待: Completed, 実際: {other:?}"),
        }
    }

    /// budget 超過 terminal も `usage` を同梱し、peak が上限に達している（仕様第6・12節）。
    #[test]
    fn e11_budget_exceeded_carries_usage() {
        let (budget, clock) = small_fuel_budget(5);
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let linked = compile_link(
            &engine,
            "m",
            "let i = 0\nwhile i < 1000\n  i = i + 1\nend\n",
        );
        match engine.run(&linked, &mut ctx, request_with(budget, clock)) {
            ExecutionOutcome::BudgetExceeded { usage, .. } => {
                // fuel 上限 5 に達して停止したので、commit 済み fuel は上限以下で非ゼロ。
                assert!(
                    usage.committed.fuel > 0 && usage.committed.fuel <= 5,
                    "BudgetExceeded の usage が想定外: {usage:?}"
                );
            }
            other => panic!("期待: BudgetExceeded, 実際: {other:?}"),
        }
    }

    /// 既定 engine + standard budget の request で、軽量 script が従来どおり Completed する。
    ///
    /// REV-015 最終形移行で `EngineConfig.budget` は消え、budget は request 必須所有になった。
    /// standard budget（30s deadline + §3.1 標準上限）の request で軽量 script が通常終了する
    /// ことを固定する。
    #[test]
    fn e11_standard_budget_request_completes() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "let a = 1\nlet b = a + 2\n");
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// ExecutionRequest の Debug は secret（引数値・capability 本文）を出さない（EMB-AT-14）。
    #[test]
    fn e11_execution_request_debug_is_secret_free() {
        let req = standard_request().with_arguments(vec!["s3cr3t-token".to_string()]);
        let dbg = format!("{req:?}");
        assert!(
            !dbg.contains("s3cr3t-token"),
            "Debug に引数値を出さない: {dbg}"
        );
        assert!(dbg.contains("argument_count"));
        // cancel 済みか否かの真偽は秘密ではないので出す（field 名は is_cancelled）。
        assert!(dbg.contains("is_cancelled"));
    }

    /// 本番 clock（SystemMonotonicClock）で有限 budget + 未来 deadline を設定した実行が
    /// 通常どおり Completed する（clock domain 整合・deadline 未達）。実運用の deadline 経路が
    /// 公開 API だけで組めることを固定する（REV-015 E11）。
    #[test]
    fn e11_system_clock_deadline_not_reached_completes() {
        use crate::budget::{BudgetConfig, SystemMonotonicClock};

        let clock: Arc<dyn crate::budget::MonotonicClock> = Arc::new(SystemMonotonicClock::new());
        // 同じ clock で deadline（now + 30 s）を計算し、同じ clock を実行へ注入する（同一 domain）。
        let budget = BudgetConfig::standard(clock.as_ref()).unwrap();
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let linked = compile_link(&engine, "m", "let x = 1\nlet y = x + 2\n");
        let outcome = engine.run(&linked, &mut ctx, request_with(budget, clock));
        assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
    }

    /// `ExecutionRequest::new` の検証: 正しい clock なら Ok（REV-015 最終形移行 §11.2）。
    #[test]
    fn e11_request_new_ok_with_matching_clock() {
        let clock: Arc<dyn crate::budget::MonotonicClock> = Arc::new(FakeClock::new());
        let budget = BudgetConfig::standard(clock.as_ref()).unwrap();
        assert!(ExecutionRequest::new(budget, clock).is_ok());
    }

    /// `ExecutionRequest::new` の検証: 別 domain の clock は ForeignClock（§11.2）。
    #[test]
    fn e11_request_new_rejects_foreign_clock() {
        let config_clock = FakeClock::new();
        let budget = BudgetConfig::standard(&config_clock).unwrap();
        let foreign: Arc<dyn crate::budget::MonotonicClock> = Arc::new(FakeClock::new());
        assert_eq!(
            ExecutionRequest::new(budget, foreign).unwrap_err(),
            BudgetConfigError::ForeignClock
        );
    }

    /// `ExecutionRequest::new` の検証: 未サポートの accounting revision は拒否する（§11.2）。
    #[test]
    fn e11_request_new_rejects_unsupported_revision() {
        let clock: Arc<dyn crate::budget::MonotonicClock> = Arc::new(FakeClock::new());
        let mut budget = BudgetConfig::standard(clock.as_ref()).unwrap();
        budget.heap_accounting_revision = 2;
        assert_eq!(
            ExecutionRequest::new(budget, clock).unwrap_err(),
            BudgetConfigError::UnsupportedAccountingRevision(2)
        );
    }

    /// `ExecutionRequest::new` の検証: 過去 deadline は DeadlineNotInFuture（§11.2）。
    #[test]
    fn e11_request_new_rejects_past_deadline() {
        let clock = Arc::new(FakeClock::new());
        // now=0 のとき now+30s の deadline を作ってから、clock を deadline より先へ進める。
        let budget = BudgetConfig::standard(clock.as_ref()).unwrap();
        clock.set(budget.deadline.as_nanos() + 1);
        let clock_dyn: Arc<dyn crate::budget::MonotonicClock> = clock;
        assert_eq!(
            ExecutionRequest::new(budget, clock_dyn).unwrap_err(),
            BudgetConfigError::DeadlineNotInFuture
        );
    }

    /// 同一 context で連続実行しても budget 残量が前 run に汚染されない（reset_budget が
    /// counters を作り直す。REV-015 最終形移行 §11.2）。
    #[test]
    fn e11_reset_budget_isolates_fuel_between_runs() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        // 1 回目: fuel をある程度消費するループ。
        let (budget1, clock1) = small_fuel_budget(1_000_000);
        let heavy = compile_link(&engine, "m", "let i = 0\nwhile i < 100\n  i = i + 1\nend\n");
        assert!(matches!(
            engine.run(&heavy, &mut ctx, request_with(budget1, clock1)),
            ExecutionOutcome::Completed { .. }
        ));

        // 2 回目: 少量 fuel の別 budget。1 回目の残量に汚染されず、独立した上限で Completed。
        let (budget2, clock2) = small_fuel_budget(1_000);
        let light = compile_link(&engine, "m", "let a = 1\nlet b = a + 2\n");
        match engine.run(&light, &mut ctx, request_with(budget2, clock2)) {
            ExecutionOutcome::Completed { usage } => {
                assert!(
                    usage.committed.fuel <= 1_000,
                    "2 回目の fuel が 1 回目に汚染された: {usage:?}"
                );
            }
            other => panic!("期待: Completed, 実際: {other:?}"),
        }
    }

    /// finding 1 回帰テスト: request が所有する cancellation token が reset_budget をまたいでも
    /// 有効であり続け、cancel すると実行が Cancelled へ落ちる（§3.2・finding 1）。
    ///
    /// host は run の前に token を作り `.cancellation(token.clone())` で request に載せる。
    /// `run_inner` は冒頭で `reset_budget` を呼び、request の token を ledger へ install
    /// （継承ではなく置換）する。host clone と install token は同一 `Arc` を共有するので、
    /// run 前に握った token の `cancel()` が reset をまたいで実行で観測される。
    #[test]
    fn e11_cancellation_token_survives_reset_budget() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        // run の前に token を作り、clone を保持しつつ request に載せる。
        let token = CancellationToken::new();

        // standard_request() は内部で新しい budget/clock を作り、run_inner が reset_budget で
        // request token を install する。その install（reset）をまたいで、保持 token の cancel が
        // 観測されることを確認する。
        assert!(
            token.cancel(),
            "run 前に握った token の最初の cancel は true"
        );

        let linked = compile_link(
            &engine,
            "m",
            "let i = 0\nwhile i < 1000\n  i = i + 1\nend\n",
        );
        assert!(
            matches!(
                engine.run(
                    &linked,
                    &mut ctx,
                    standard_request().cancellation(token.clone()),
                ),
                ExecutionOutcome::Cancelled { .. }
            ),
            "reset_budget をまたいで request-owned token の cancel が観測されるべき"
        );
    }

    /// pre-cancel が返す usage は空（reset_budget 直後・baseline 課金前。finding 3）。
    #[test]
    fn e11_pre_cancel_usage_is_empty() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "let a = 1\nlet b = a + 2\n");
        match engine.run(&linked, &mut ctx, standard_request_cancelled()) {
            ExecutionOutcome::Cancelled { usage } => {
                assert_eq!(usage.committed, crate::budget::BudgetCounters::default());
                assert_eq!(usage.reserved, crate::budget::BudgetCounters::default());
                assert_eq!(usage.live_heap_bytes, 0);
            }
            other => panic!("期待: Cancelled, 実際: {other:?}"),
        }
    }

    // =======================================================================
    // スライス S2: request が CancellationToken を必須所有する（REV-015 最終形移行）
    // =======================================================================

    /// `ExecutionRequest::new` の既定 cancellation token は未 cancel で、通常の script は
    /// Completed する（`.cancellation(..)` 未指定の既定挙動、設計 §5.1）。
    #[test]
    fn s2_request_owns_cancellation_default_is_not_cancelled() {
        let request = standard_request();
        assert!(
            !format!("{request:?}").contains("is_cancelled: true"),
            "既定 token は未 cancel"
        );

        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "let a = 1\nlet b = a + 2\n");
        assert!(matches!(
            engine.run(&linked, &mut ctx, request),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// `.cancellation(token)` で載せた token の clone を host が cancel すると、pre-run で
    /// Cancelled になる（命令0、EMB-AT-12）。host が握る clone と ledger install token が同一
    /// `Arc` を共有することを「host 側 clone を cancel すると run が止まる」で固定する（設計 §8.3）。
    #[test]
    fn s2_cancellation_builder_sets_token() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        // host は token を作り、clone を request に載せ、自分の clone を cancel する。
        let token = CancellationToken::new();
        let request = standard_request().cancellation(token.clone());
        token.cancel();

        let linked = compile_link(&engine, "m", "let saved = 7\nlet z = saved + 1\n");
        assert!(matches!(
            engine.run(&linked, &mut ctx, request),
            ExecutionOutcome::Cancelled { .. }
        ));
        assert!(!ctx.is_poisoned(), "Cancelled は poison しない");

        // 命令0なので saved は commit されない（見えれば Name エラーで判別できる）。
        let probe = compile_link(&engine, "m", "let echo = saved\n");
        match engine.run(&probe, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => assert_eq!(error.code, ErrorKind::Name),
            other => panic!("pre-cancel が副作用を残した: {other:?}"),
        }
    }

    /// request の Debug は引数値・budget 値・token 内部を出さず、`argument_count` と
    /// `is_cancelled` の真偽だけを出す（EMB-AT-14 系、設計 §8.3）。
    #[test]
    fn s2_request_debug_is_secret_free() {
        let token = CancellationToken::new();
        token.cancel();
        let request = standard_request()
            .with_arguments(vec!["s3cr3t".to_string()])
            .cancellation(token);
        let dbg = format!("{request:?}");

        assert!(!dbg.contains("s3cr3t"), "引数値を出さない: {dbg}");
        assert!(dbg.contains("argument_count"));
        assert!(
            dbg.contains("is_cancelled: true"),
            "cancel 済みの真偽を出す: {dbg}"
        );
    }

    // --- E7: host function embedding registration + grant injection ---
    //
    // `EngineBuilder::host_functions` で登録した registry が `Engine::run` 経由で evaluator へ
    // 注入され、grant（`ExecutionRequest::with_capabilities`）と組み合わさって projection する
    // ことを Engine API レベルで固定する（登録と grant の分離、capability-model 第11.1・11.2節）。
    // projection 自体は eval 側で既に検証済み（C8）。ここは installation/injection の配線確認。

    use crate::host_function::{
        Arity as HostArity, AuditValuePolicy, HostCallError, HostCost, HostFunction,
        HostFunctionDescriptor, HostFunctionRegistry,
    };
    use crate::value::Value;

    /// host function id を作る小ヘルパ。
    fn host_id(n: u128) -> crate::capability::HostFunctionId {
        crate::capability::HostFunctionId::new(NonZeroU128::new(n).unwrap())
    }

    /// 指定 id の host function を grant した set（＋結果観測用 ProcessExit）。`exit(...)` で
    /// 結果を terminal code として観測できるようにする。
    fn host_fn_granted(id: u128) -> crate::capability::CapabilitySet {
        crate::capability::CapabilitySet::builder()
            .grant_host_function(host_id(id))
            .unwrap()
            .process_exit(crate::capability::ProcessExit::new(
                NonZeroU128::new(1).unwrap(),
            ))
            .unwrap()
            .build()
    }

    /// テスト用 host function（echo か host error）。descriptor と挙動を持つ。
    struct E7TestFn {
        descriptor: HostFunctionDescriptor,
        host_error: bool,
    }
    impl HostFunction for E7TestFn {
        fn descriptor(&self) -> &HostFunctionDescriptor {
            &self.descriptor
        }
        fn call(&self, arguments: &[Value]) -> Result<Value, HostCallError> {
            if self.host_error {
                return Err(HostCallError::Host {
                    category: "lookup".to_string(),
                });
            }
            // echo: 先頭引数を返す（なければ null）。
            Ok(arguments.first().cloned().unwrap_or(Value::Null))
        }
    }

    fn e7_descriptor(id: u128, name: &str, arity: HostArity) -> HostFunctionDescriptor {
        HostFunctionDescriptor {
            id: host_id(id),
            name: name.to_string(),
            arity,
            cost: HostCost::default(),
            argument_audit: Vec::new(),
            result_audit: AuditValuePolicy::Omit,
            may_block: false,
        }
    }

    /// `lookup` という echo host function 1件だけの registry を作る（id=7, arity Exact(1)）。
    fn lookup_registry() -> HostFunctionRegistry {
        HostFunctionRegistry::builder()
            .register(std::sync::Arc::new(E7TestFn {
                descriptor: e7_descriptor(7, "lookup", HostArity::Exact(1)),
                host_error: false,
            }))
            .unwrap()
            .build()
            .unwrap()
    }

    /// host error を返す `lookup` 1件（id=7, arity Exact(0)）の registry。
    fn host_error_registry() -> HostFunctionRegistry {
        HostFunctionRegistry::builder()
            .register(std::sync::Arc::new(E7TestFn {
                descriptor: e7_descriptor(7, "lookup", HostArity::Exact(0)),
                host_error: true,
            }))
            .unwrap()
            .build()
            .unwrap()
    }

    fn engine_with_lookup(registry: HostFunctionRegistry) -> Engine {
        Engine::builder().host_functions(registry).build().unwrap()
    }

    /// 登録 + grant された host fn は `Engine::run` から呼べて期待値を返す（Completed/Exited）。
    #[test]
    fn e7_registered_and_granted_is_invokable() {
        let engine = engine_with_lookup(lookup_registry());
        let mut ctx = ExecutionContext::new(&engine);
        // echo(42) の結果を exit code として観測する（host callback が引数を受け取った証拠）。
        let linked = compile_link(&engine, "m", "exit(lookup(42))\n");
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(host_fn_granted(7)),
        );
        assert!(matches!(outcome, ExecutionOutcome::Exited { code: 42, .. }));
    }

    /// 登録 + grant された host fn の結果を binding に束ねて Completed する。
    #[test]
    fn e7_registered_and_granted_completes() {
        let engine = engine_with_lookup(lookup_registry());
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "let r = lookup(1)\n");
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(host_fn_granted(7)),
        );
        assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
    }

    /// 登録済みだが未 grant → 未捕捉で capability RuntimeError（terminal Denied ではない）。
    #[test]
    fn e7_registered_but_ungranted_is_capability_runtime_error() {
        let engine = engine_with_lookup(lookup_registry());
        let mut ctx = ExecutionContext::new(&engine);
        // 既定 request は deny-by-default（capability なし）。
        let linked = compile_link(&engine, "m", "let r = lookup(1)\n");
        match engine.run(&linked, &mut ctx, standard_request()) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, ErrorKind::Capability);
            }
            other => panic!("期待: RuntimeError(capability), 実際: {other:?}"),
        }
    }

    /// 未 grant の host fn 呼び出しは script から catch できる（catchable capability error）。
    #[test]
    fn e7_registered_but_ungranted_is_catchable() {
        let engine = engine_with_lookup(lookup_registry());
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(
            &engine,
            "m",
            "try\n  let r = lookup(1)\ncatch e\n  let caught = e[\"type\"]\nend\n",
        );
        assert!(matches!(
            engine.run(&linked, &mut ctx, standard_request()),
            ExecutionOutcome::Completed { .. }
        ));
    }

    /// grant 済み host fn の arity mismatch → 未捕捉で argument RuntimeError。
    #[test]
    fn e7_granted_arity_mismatch_is_argument_runtime_error() {
        let engine = engine_with_lookup(lookup_registry());
        let mut ctx = ExecutionContext::new(&engine);
        // lookup は Exact(1)。引数0個で呼ぶ。
        let linked = compile_link(&engine, "m", "let r = lookup()\n");
        match engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(host_fn_granted(7)),
        ) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, ErrorKind::Argument);
            }
            other => panic!("期待: RuntimeError(argument), 実際: {other:?}"),
        }
    }

    /// `HostCallError::Host` を返す host fn → 未捕捉で host RuntimeError。
    #[test]
    fn e7_host_failure_is_host_runtime_error() {
        let engine = engine_with_lookup(host_error_registry());
        let mut ctx = ExecutionContext::new(&engine);
        let linked = compile_link(&engine, "m", "let r = lookup()\n");
        match engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(host_fn_granted(7)),
        ) {
            ExecutionOutcome::RuntimeError { error, .. } => {
                assert_eq!(error.code, ErrorKind::Host);
            }
            other => panic!("期待: RuntimeError(host), 実際: {other:?}"),
        }
    }

    /// 同名の user binding は host function を shadow する（user 定義が優先）。
    #[test]
    fn e7_user_binding_shadows_host_function() {
        let engine = engine_with_lookup(lookup_registry());
        let mut ctx = ExecutionContext::new(&engine);
        // user の lookup は常に 99 を返す。grant 済みでも user 定義が優先される。
        let linked = compile_link(&engine, "m", "let lookup = fn(x) 99 end\nexit(lookup(1))\n");
        let outcome = engine.run(
            &linked,
            &mut ctx,
            standard_request().with_capabilities(host_fn_granted(7)),
        );
        assert!(matches!(outcome, ExecutionOutcome::Exited { code: 99, .. }));
    }

    /// 名前衝突 registry は registry build 時に拒否される（Engine builder はその検証に依存し、
    /// 衝突した registry を受け取らない）。
    #[test]
    fn e7_colliding_registry_rejected_at_registry_build() {
        // `len` は core builtin。登録は registry build でエラー。
        let err = HostFunctionRegistry::builder()
            .register(std::sync::Arc::new(E7TestFn {
                descriptor: e7_descriptor(1, "len", HostArity::Exact(1)),
                host_error: false,
            }))
            .err()
            .expect("colliding name must fail at registry build");
        assert!(matches!(err, ConfigError::DuplicateCallableName { .. }));
    }
}

// ===========================================================================
// C6-c: link 時 import 解決の Engine API 配線テスト（設計 §4.7）。
//
// link 層が resolver を使って import graph を構築・検証する経路を直接固定する。resolver
// 呼び出し回数カウンタ付きの fake resolver で CAP-AT-16（resolver call 0）/ cycle / depth /
// 同 ID 異 hash / UTF-8 / 契約違反 / pre-cancel / 未登録 mount / malformed / ImportGraph golden /
// EMB-AT-05（run 時 resolver 0）を検証する。
// ===========================================================================
#[cfg(test)]
mod c6c_tests {
    use super::*;
    use crate::budget::{BudgetConfig, CancellationToken, FakeClock};
    use crate::capability::{
        AdapterError, CapabilityCallContext, CapabilityKind, CapabilitySet, DenialCode,
        ModuleChunk, ModuleResolver, ModuleSource, ResolveRequest, ResolvedModule,
    };
    use std::collections::HashMap;
    use std::num::{NonZeroU128, NonZeroUsize};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn pid(n: u128) -> NonZeroU128 {
        NonZeroU128::new(n).expect("non-zero")
    }

    /// in-memory な [`ModuleSource`]。`bytes` を `max_bytes` 単位で切り出して返す。
    struct InMemorySource {
        bytes: Vec<u8>,
        offset: usize,
    }
    impl ModuleSource for InMemorySource {
        fn read_chunk(
            &mut self,
            _context: &mut CapabilityCallContext<'_>,
            max_bytes: NonZeroUsize,
        ) -> Result<ModuleChunk, AdapterError> {
            if self.offset >= self.bytes.len() {
                return Ok(ModuleChunk::Eof);
            }
            let end = (self.offset + max_bytes.get()).min(self.bytes.len());
            let chunk = self.bytes[self.offset..end].to_vec();
            self.offset = end;
            Ok(ModuleChunk::Bytes(chunk))
        }
    }

    /// `max_bytes` を 1 byte 超過する chunk を返す契約違反 source。
    struct OversizedSource {
        emitted: bool,
    }
    impl ModuleSource for OversizedSource {
        fn read_chunk(
            &mut self,
            _context: &mut CapabilityCallContext<'_>,
            max_bytes: NonZeroUsize,
        ) -> Result<ModuleChunk, AdapterError> {
            if self.emitted {
                return Ok(ModuleChunk::Eof);
            }
            self.emitted = true;
            Ok(ModuleChunk::Bytes(vec![b'x'; max_bytes.get() + 1]))
        }
    }

    /// 呼び出し回数カウンタ付きの fake resolver。
    ///
    /// `sources`: 正規化済み specifier（`@default/a` 等）→ module の生 bytes。resolve ごとに
    /// `calls` を +1 する。`mount` 既定は `default`（`knows_mount` が `default` を知っていると返す）。
    struct CountingResolver {
        sources: HashMap<String, Vec<u8>>,
        calls: Arc<AtomicUsize>,
        mounts: Vec<String>,
    }
    impl CountingResolver {
        fn new(sources: &[(&str, &str)]) -> (Self, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            let map = sources
                .iter()
                .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
                .collect();
            (
                Self {
                    sources: map,
                    calls: Arc::clone(&calls),
                    mounts: vec!["default".to_string()],
                },
                calls,
            )
        }
        fn with_bytes(sources: Vec<(&str, Vec<u8>)>) -> (Self, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            let map = sources
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            (
                Self {
                    sources: map,
                    calls: Arc::clone(&calls),
                    mounts: vec!["default".to_string()],
                },
                calls,
            )
        }
    }
    impl ModuleResolver for CountingResolver {
        fn policy_id(&self) -> NonZeroU128 {
            pid(99)
        }
        fn knows_mount(&self, mount: &str) -> bool {
            self.mounts.iter().any(|m| m == mount)
        }
        fn resolve(
            &self,
            _context: &mut CapabilityCallContext<'_>,
            request: ResolveRequest<'_>,
        ) -> Result<ResolvedModule, AdapterError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let id = ModuleId::new(request.specifier)
                .map_err(|e| AdapterError::Host(format!("bad id: {e:?}")))?;
            let bytes = self
                .sources
                .get(request.specifier)
                .cloned()
                .ok_or_else(|| AdapterError::Host("not found".to_string()))?;
            Ok(ResolvedModule {
                id,
                source: Box::new(InMemorySource { bytes, offset: 0 }),
                classification: crate::capability::DataClassification::Public,
            })
        }
    }

    /// 契約違反 source を返す resolver。
    struct OversizedResolver {
        calls: Arc<AtomicUsize>,
    }
    impl ModuleResolver for OversizedResolver {
        fn policy_id(&self) -> NonZeroU128 {
            pid(98)
        }
        fn resolve(
            &self,
            _context: &mut CapabilityCallContext<'_>,
            request: ResolveRequest<'_>,
        ) -> Result<ResolvedModule, AdapterError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ResolvedModule {
                id: ModuleId::new(request.specifier).expect("id"),
                source: Box::new(OversizedSource { emitted: false }),
                classification: crate::capability::DataClassification::Public,
            })
        }
    }

    fn budget() -> BudgetConfig {
        let clock = FakeClock::new();
        BudgetConfig::standard(&clock).expect("standard budget")
    }

    fn op_id() -> ExecutionId {
        ExecutionId::new(pid(1))
    }

    fn caps_with(resolver: Arc<dyn ModuleResolver>) -> CapabilitySet {
        CapabilitySet::builder()
            .module_resolver(resolver)
            .expect("grant resolver")
            .build()
    }

    fn compile_root(engine: &Engine, text: &str) -> CompiledScript {
        engine
            .compile(
                Source::new(SourceId::new("root").expect("id"), text),
                &CompileOptions::default(),
            )
            .expect("compile root")
    }

    // --- CAP-AT-16: resolver 未 grant + import → resolver call 0 の Denied ---
    #[test]
    fn cap_at_16_import_without_resolver_is_denied_call_0() {
        let engine = Engine::builder().build().unwrap();
        let (resolver, calls) = CountingResolver::new(&[("@default/a", "let a = 1\n")]);
        // resolver を作るが grant しない（empty capabilities）。
        drop(resolver);
        let script = compile_root(&engine, "import \"a\"\n");
        let req = LinkRequest::new(op_id(), CapabilitySet::empty(), budget());
        let err = engine.link(&script, req).expect_err("denied");
        match err {
            LinkError::Denied(d) => {
                assert_eq!(d.code, DenialCode::ResourceNotGranted);
                assert_eq!(d.capability, CapabilityKind::ModuleResolver);
                assert_eq!(d.operation.as_str(), "module.resolve");
                assert_eq!(d.public_resource, None);
            }
            other => panic!("expected Denied, got {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0, "resolver は呼ばれない");
    }

    // --- CAP-AT-16(b): resolver grant + import → link 成功、解決は link 内のみ ---
    #[test]
    fn cap_at_16_import_with_resolver_resolves_at_link() {
        let engine = Engine::builder().build().unwrap();
        let (resolver, calls) = CountingResolver::new(&[("@default/a", "let a = 1\n")]);
        let script = compile_root(&engine, "import \"a\"\n");
        let req = LinkRequest::new(op_id(), caps_with(Arc::new(resolver)), budget());
        let linked = engine.link(&script, req).expect("link ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a を 1 回解決");
        let graph = linked.import_graph();
        assert_eq!(graph.root_imports.len(), 1);
        assert_eq!(graph.root_imports[0].as_str(), "@default/a");
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].module_id.as_str(), "@default/a");
    }

    // --- 循環: a → b → a（chain は起点を末尾に再掲）---
    #[test]
    fn cycle_a_b_a_reports_chain() {
        let engine = Engine::builder().build().unwrap();
        let (resolver, _calls) = CountingResolver::new(&[
            ("@default/a", "import \"b\"\n"),
            ("@default/b", "import \"a\"\n"),
        ]);
        let script = compile_root(&engine, "import \"a\"\n");
        let req = LinkRequest::new(op_id(), caps_with(Arc::new(resolver)), budget());
        let err = engine.link(&script, req).expect_err("cycle");
        match err {
            LinkError::Cycle { chain } => {
                let got: Vec<&str> = chain.iter().map(ModuleId::as_str).collect();
                assert_eq!(got, vec!["@default/a", "@default/b", "@default/a"]);
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    // --- 自己循環: a → a（chain == [@default/a, @default/a]）---
    #[test]
    fn self_cycle_a_a_reports_chain() {
        let engine = Engine::builder().build().unwrap();
        let (resolver, _calls) = CountingResolver::new(&[("@default/a", "import \"a\"\n")]);
        let script = compile_root(&engine, "import \"a\"\n");
        let req = LinkRequest::new(op_id(), caps_with(Arc::new(resolver)), budget());
        let err = engine.link(&script, req).expect_err("self cycle");
        match err {
            LinkError::Cycle { chain } => {
                let got: Vec<&str> = chain.iter().map(ModuleId::as_str).collect();
                assert_eq!(got, vec!["@default/a", "@default/a"]);
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    // --- 深度: MAX_IMPORT_DEPTH を超える chain で DepthExceeded ---
    #[test]
    fn depth_exceeded_reports_limit() {
        let engine = Engine::builder().build().unwrap();
        // m0 → m1 → ... と各 module が次を import する深い鎖を作る。
        let depth = crate::limits::MAX_IMPORT_DEPTH + 5;
        let mut sources: Vec<(String, String)> = Vec::new();
        for i in 0..depth {
            sources.push((format!("@default/m{i}"), format!("import \"m{}\"\n", i + 1)));
        }
        let src_refs: Vec<(&str, &str)> = sources
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let (resolver, _calls) = CountingResolver::new(&src_refs);
        let script = compile_root(&engine, "import \"m0\"\n");
        let req = LinkRequest::new(op_id(), caps_with(Arc::new(resolver)), budget());
        let err = engine.link(&script, req).expect_err("depth");
        match err {
            LinkError::DepthExceeded { limit } => {
                assert_eq!(limit, crate::limits::MAX_IMPORT_DEPTH as u32);
            }
            other => panic!("expected DepthExceeded, got {other:?}"),
        }
    }

    // --- 未登録 mount → terminal Denied（resolver は呼ばれない）---
    #[test]
    fn unregistered_mount_is_denied() {
        let engine = Engine::builder().build().unwrap();
        let (resolver, calls) = CountingResolver::new(&[("@default/a", "let a = 1\n")]);
        // CountingResolver は default のみ知る。@none は未登録。
        let script = compile_root(&engine, "import \"@none/a\"\n");
        let req = LinkRequest::new(op_id(), caps_with(Arc::new(resolver)), budget());
        let err = engine.link(&script, req).expect_err("denied");
        assert!(matches!(err, LinkError::Denied(_)), "got {err:?}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "未登録 mount で resolver は呼ばない"
        );
    }

    // --- malformed specifier → Resolve(invalid_import_specifier)（InvalidModule ではない）---
    #[test]
    fn malformed_specifier_is_resolve_error() {
        let engine = Engine::builder().build().unwrap();
        for bad in ["../escape", "/abs/mod"] {
            let (resolver, _calls) = CountingResolver::new(&[]);
            let script = compile_root(&engine, &format!("import \"{bad}\"\n"));
            let req = LinkRequest::new(op_id(), caps_with(Arc::new(resolver)), budget());
            let err = engine.link(&script, req).expect_err("resolve err");
            match err {
                LinkError::Resolve(h) => {
                    assert_eq!(h.code.as_str(), "invalid_import_specifier");
                }
                other => panic!("malformed `{bad}` expected Resolve, got {other:?}"),
            }
        }
    }

    // --- pre-link cancel → Cancelled、resolver call 0 ---
    #[test]
    fn pre_link_cancel_is_cancelled_call_0() {
        let engine = Engine::builder().build().unwrap();
        let (resolver, calls) = CountingResolver::new(&[("@default/a", "let a = 1\n")]);
        let script = compile_root(&engine, "import \"a\"\n");
        let token = CancellationToken::new();
        token.cancel();
        let req = LinkRequest::new(op_id(), caps_with(Arc::new(resolver)), budget())
            .with_cancellation(token);
        let err = engine.link(&script, req).expect_err("cancelled");
        assert!(matches!(err, LinkError::Cancelled), "got {err:?}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "cancel 済みで resolver は呼ばない"
        );
    }

    // --- 同 ID / 異 hash → Resolve ---
    #[test]
    fn same_id_different_hash_is_resolve_error() {
        // root が同じ specifier `a` を 2 回 import するが、resolver は呼ばれるたびに別内容を返す。
        struct FlakyResolver {
            counter: AtomicUsize,
        }
        impl ModuleResolver for FlakyResolver {
            fn policy_id(&self) -> NonZeroU128 {
                pid(97)
            }
            fn resolve(
                &self,
                _c: &mut CapabilityCallContext<'_>,
                request: ResolveRequest<'_>,
            ) -> Result<ResolvedModule, AdapterError> {
                let n = self.counter.fetch_add(1, Ordering::SeqCst);
                let bytes = format!("let v = {n}\n").into_bytes();
                Ok(ResolvedModule {
                    id: ModuleId::new(request.specifier).expect("id"),
                    source: Box::new(InMemorySource { bytes, offset: 0 }),
                    classification: crate::capability::DataClassification::Public,
                })
            }
        }
        let engine = Engine::builder().build().unwrap();
        let resolver = Arc::new(FlakyResolver {
            counter: AtomicUsize::new(0),
        });
        // root に同じ import を 2 回書く。1 回目で resolved へ登録、2 回目は同 ID だが別 hash。
        let script = compile_root(&engine, "import \"a\"\nimport \"a\"\n");
        let req = LinkRequest::new(op_id(), caps_with(resolver), budget());
        let err = engine.link(&script, req).expect_err("hash mismatch");
        match err {
            LinkError::Resolve(h) => assert_eq!(h.code.as_str(), "module_hash_mismatch"),
            other => panic!("expected Resolve(module_hash_mismatch), got {other:?}"),
        }
    }

    // --- 非 UTF-8 module → InvalidModule ---
    #[test]
    fn non_utf8_module_is_invalid_module() {
        let engine = Engine::builder().build().unwrap();
        let (resolver, _calls) =
            CountingResolver::with_bytes(vec![("@default/a", vec![0xff, 0xfe, 0x00])]);
        let script = compile_root(&engine, "import \"a\"\n");
        let req = LinkRequest::new(op_id(), caps_with(Arc::new(resolver)), budget());
        let err = engine.link(&script, req).expect_err("invalid module");
        match err {
            LinkError::InvalidModule { module, .. } => {
                assert_eq!(module.as_str(), "@default/a");
            }
            other => panic!("expected InvalidModule, got {other:?}"),
        }
    }

    // --- resolver_contract_violation: max_bytes 超過 chunk → Resolve ---
    #[test]
    fn resolver_contract_violation_is_resolve_error() {
        let engine = Engine::builder().build().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = Arc::new(OversizedResolver {
            calls: Arc::clone(&calls),
        });
        let script = compile_root(&engine, "import \"a\"\n");
        let req = LinkRequest::new(op_id(), caps_with(resolver), budget());
        let err = engine.link(&script, req).expect_err("contract violation");
        match err {
            LinkError::Resolve(h) => assert_eq!(h.code.as_str(), "resolver_contract_violation"),
            other => panic!("expected Resolve(resolver_contract_violation), got {other:?}"),
        }
    }

    // --- ImportGraph golden: 実 node が入り graph_hash が決定的 ---
    #[test]
    fn import_graph_is_populated_and_hash_deterministic() {
        let engine = Engine::builder().build().unwrap();
        let build = || {
            let (resolver, _c) = CountingResolver::new(&[
                ("@default/a", "import \"b\"\nlet a = 1\n"),
                ("@default/b", "let b = 2\n"),
            ]);
            let script = compile_root(&engine, "import \"a\"\n");
            let req = LinkRequest::new(op_id(), caps_with(Arc::new(resolver)), budget());
            engine.link(&script, req).expect("link")
        };
        let g1 = build();
        let g2 = build();
        let graph = g1.import_graph();
        // root → a、a → b の実 node。
        assert_eq!(graph.root_imports.len(), 1);
        let ids: Vec<&str> = graph.nodes.iter().map(|n| n.module_id.as_str()).collect();
        assert_eq!(ids, vec!["@default/a", "@default/b"], "nodes は id 昇順");
        // a は b を import する node を持つ。
        let a = graph
            .nodes
            .iter()
            .find(|n| n.module_id.as_str() == "@default/a")
            .expect("a node");
        assert_eq!(a.imports.len(), 1);
        assert_eq!(a.imports[0].as_str(), "@default/b");
        // graph_hash / script_hash は同一入力で決定的。
        assert_eq!(graph.graph_hash, g2.import_graph().graph_hash);
        assert_eq!(g1.script_hash(), g2.script_hash());
    }

    // --- EMB-AT-05: run 時 resolver call 0（解決は link で済み、run は resolver を触らない）---
    #[test]
    fn emb_at_05_run_does_not_call_resolver() {
        let engine = Engine::builder().build().unwrap();
        // run するため root を retain=true で compile し、import は default routing で解決する。
        // root 本文は import した名前を使わない（評価器の ambient ModuleLoader は tempdir を持たない
        // ため、ここでは import 解決を link 層のみで観測し、run は root の素の文だけ実行する）。
        let (resolver, calls) = CountingResolver::new(&[("@default/a", "let a = 1\n")]);
        let script = engine
            .compile(
                Source::new(SourceId::new("root").expect("id"), "let x = 1\n"),
                &CompileOptions {
                    retain_source: true,
                },
            )
            .expect("compile");
        let req = LinkRequest::new(op_id(), caps_with(Arc::new(resolver)), budget());
        let linked = engine.link(&script, req).expect("link");
        // import 0 件 root なので link でも resolver は呼ばれない（空 graph）。
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // run して resolver call が増えないことを固定（run は resolver を一切呼ばない）。
        let mut context = ExecutionContext::new(&engine);
        let clock: Arc<dyn crate::budget::MonotonicClock> =
            Arc::new(crate::budget::SystemMonotonicClock::new());
        let run_budget = BudgetConfig::standard(clock.as_ref()).expect("budget");
        let run_req = ExecutionRequest::new(run_budget, clock).expect("req");
        let outcome = engine.run(&linked, &mut context, run_req);
        assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "run 中 resolver call 0");
    }
}
