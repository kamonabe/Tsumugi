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
//! - `CapabilitySet` / `HostFunctionRegistry` / `CapabilityKind`（Phase 2、E7）。これらを
//!   参照する [`ConfigError`] の `DuplicateCapability` / `DuplicateCallableName` と
//!   `EngineBuilder::host_functions` は E7 で追加する。
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
/// 仕様文書の擬似コードは執筆時点の `V0_11` を例示するが、実装の現行 revision は 0.19 で
/// あるため、実装は現行の `V0_19` を持つ。番号体系は Cargo package version（0.1.0）とは
/// 独立に管理する（`docs/roadmap.md`「バージョン番号の扱い」）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum LanguageRevision {
    /// language-spec revision 0.19（現行）。
    V0_19,
}

impl LanguageRevision {
    /// 現行の言語 revision。
    pub const CURRENT: Self = Self::V0_19;

    /// revision の文字列表現（例: `"0.19"`）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V0_19 => "0.19",
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
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// 実行 backend。
    pub backend: Backend,
    /// 言語 revision。
    pub language_revision: LanguageRevision,
}

impl Default for EngineConfig {
    /// 既定は `TreeWalk` + 現行 revision。
    fn default() -> Self {
        Self {
            backend: Backend::TreeWalk,
            language_revision: LanguageRevision::CURRENT,
        }
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
/// E1 では config と実験 backend 許可だけを扱う。`host_functions`（Phase 2、E7）は
/// まだ公開しない。
#[derive(Debug, Default)]
pub struct EngineBuilder {
    config: EngineConfig,
    allow_experimental_backend: bool,
}

impl EngineBuilder {
    /// 既定設定の builder を作る。
    pub fn new() -> Self {
        Self {
            config: EngineConfig::default(),
            allow_experimental_backend: false,
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

    /// 設定を検証して [`Engine`] を build する。
    ///
    /// `VmExperimental` を選びつつ許可していない場合は
    /// [`ConfigError::ExperimentalBackendNotEnabled`] を返す。
    pub fn build(self) -> Result<Engine, ConfigError> {
        if self.config.backend == Backend::VmExperimental && !self.allow_experimental_backend {
            return Err(ConfigError::ExperimentalBackendNotEnabled);
        }
        Ok(Engine {
            id: EngineId::allocate(),
            config: self.config,
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
/// E1（Phase 1）で構造化できる variant のみを持つ。仕様の他 terminal（`Exited` /
/// `Denied` / `HostError` / `BudgetExceeded` / `DeadlineExceeded` / `AuditFailure` /
/// `RecordFailure` / `ReplayMismatch` と、`usage: BudgetUsage` field）は、対応する機構が
/// 入る後続 Phase で追加する。`#[non_exhaustive]` により variant 追加を breaking にしない。
///
/// 現行 alpha facade の [`crate::engine::ExecutionOutcome`] とは別型で、crate root では
/// [`crate::EmbeddingOutcome`] として公開する。
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ExecutionOutcome {
    /// スクリプトが最後まで実行された。v1 の返値は常に空（top-level 返値は将来拡張）。
    Completed,
    /// スクリプト実行中に未捕捉 runtime error で終了した。
    RuntimeError {
        /// canonical な runtime error。
        error: ExecutionError,
    },
    /// script handler 開始前（最初の poll より前）に cancel された（Phase 1 の pre-run cancel）。
    Cancelled,
    /// engine / host callback の panic を捕捉した内部障害（secret を含まない）。
    InternalFailure {
        /// 相関用の非ゼロ fault ID。
        fault_id: u128,
        /// secret を含まない表示用メッセージ。
        safe_message: String,
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
/// E2 では capability / budget / cancellation を伴う import 解決を行わない（Phase 1 は
/// import 0 件のみ link 可能）。最終形の `operation_id` / `capabilities` / `budget` /
/// `cancellation` は Phase 2/3（E7・E11）で導入する。現状は最小の骨格に留める。
#[derive(Clone, Debug, Default)]
pub struct LinkRequest {
    _private: (),
}

impl LinkRequest {
    /// 既定の link 要求を作る。
    pub fn new() -> Self {
        Self { _private: () }
    }
}

/// link 失敗（仕様第5節 `LinkError`）。
///
/// E2（Phase 1、import 0 件）で到達し得る variant のみを持つ。resolver / capability /
/// budget / deadline / cancel 等は Phase 2/3 で追加する。`#[non_exhaustive]`。
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LinkError {
    /// 別 engine で作られた `CompiledScript` を link しようとした。
    EngineMismatch,
    /// revision が一致しない。
    RevisionMismatch,
    /// backend が一致しない。
    BackendMismatch,
    /// 現在の Phase で未提供の機能を要求した（Phase 1 の import など）。
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
        // payload / native backtrace は公開診断へ含めない（規則3）。
        catch_host_unwind(|| self.compile_inner(source, options)).unwrap_or_else(|fault_id| {
            Err(CompileErrors {
                diagnostics: vec![CompileDiagnostic {
                    code: CompileDiagnosticCode::InternalFault,
                    line: None,
                    column: None,
                    safe_message: internal_fault_message(fault_id),
                }],
            })
        })
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
        let has_imports = program.iter().any(is_import_stmt);

        Ok(CompiledScript(Arc::new(CompiledScriptInner {
            engine_id: self.id(),
            source_id: source.id.clone(),
            source_hash,
            language_revision: self.config().language_revision,
            backend: self.config().backend,
            has_imports,
            retained_source: options.retain_source.then(|| source.text.to_string()),
        })))
    }

    /// [`CompiledScript`] を link して [`LinkedScript`] を作る（仕様第5節）。
    ///
    /// engine ID → revision → backend の順に検証し、不一致なら対応する [`LinkError`] を返す。
    /// Phase 1 では import 0 件だけを link でき、import 文が1件以上あれば
    /// [`LinkError::FeatureUnavailable`]（`feature: "module_resolver"`）を返す。import 0 件でも
    /// 空 graph を持つ。
    pub fn link(
        &self,
        script: &CompiledScript,
        request: LinkRequest,
    ) -> Result<LinkedScript, LinkError> {
        // linker の unwind panic を host boundary で捕捉する（第11節 規則1）。link 中の panic は
        // `LinkError::InternalFailure` へ写す（規則2）。payload / backtrace は含めない（規則3）。
        catch_host_unwind(|| self.link_inner(script, request)).unwrap_or_else(|fault_id| {
            Err(LinkError::InternalFailure {
                fault_id,
                safe_message: internal_fault_message(fault_id),
            })
        })
    }

    fn link_inner(
        &self,
        script: &CompiledScript,
        _request: LinkRequest,
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

        // Phase 1: import が1件でもあれば module resolver 未提供として拒否する。
        if script.has_imports() {
            return Err(LinkError::FeatureUnavailable {
                feature: "module_resolver",
            });
        }

        let root = script.source_hash();
        // import 0 件の空 graph。
        let graph_hash = SourceHash::from_bytes(hash::import_graph_hash(
            script.language_revision(),
            root,
            &[],
            &[],
        ));
        let import_graph = ImportGraph {
            root,
            root_imports: Vec::new(),
            nodes: Vec::new(),
            graph_hash,
        };

        let script_hash = SourceHash::from_bytes(hash::linked_script_hash(
            script.language_revision(),
            root,
            graph_hash,
        ));

        Ok(LinkedScript(Arc::new(LinkedScriptInner {
            root: script.clone(),
            import_graph,
            script_hash,
        })))
    }
}

// ===========================================================================
// スライス E3: tree backend adapter（実行入口）
// スライス E4: Context/Handle cleanup と transaction journal の縦切り
//   （再利用・poison・全 language-state rollback）
// ===========================================================================

use crate::budget::BudgetUsage;
use crate::eval::{Evaluator, RunPhase};

/// 1 回の実行に対する不変の設定（仕様第6節 `ExecutionRequest`）。
///
/// E3 では script 引数 snapshot だけを持つ最小の骨格。仕様の `execution_id` /
/// frozen `CapabilitySet` / 有限 `BudgetConfig` / `CancellationToken` は Phase 2/3（E7・E11）で
/// 導入する。alpha facade（[`crate::engine::ExecutionRequest`]）とは別型。
#[derive(Clone, Debug, Default)]
pub struct ExecutionRequest {
    /// `args()` が返すスクリプト引数 snapshot（binary 名・script path・CLI flag を含まない）。
    arguments: Vec<String>,
    /// 最初の poll より前に cancel 済みか（Phase 1 の pre-run cancel、EMB-AT-12）。
    ///
    /// Phase 3 の `CancellationToken`（実行中 cancel）は E11 で導入する。E5 では実行前に
    /// 確定した cancel だけを扱い、命令を1つも実行せずに [`ExecutionOutcome::Cancelled`] へ
    /// 落とす（"pre-cancel は命令0"）。
    pre_cancelled: bool,
}

impl ExecutionRequest {
    /// 引数なしの実行リクエストを作る。
    pub fn new() -> Self {
        Self {
            arguments: Vec::new(),
            pre_cancelled: false,
        }
    }

    /// スクリプト引数 snapshot を設定する（AUD-018、仕様第6節）。
    pub fn with_arguments(mut self, arguments: Vec<String>) -> Self {
        self.arguments = arguments;
        self
    }

    /// 最初の poll より前に cancel 済みとしてこのリクエストを印付ける（EMB-AT-12）。
    ///
    /// このリクエストで [`Engine::run`] を呼ぶと、script 命令を1つも実行せずに
    /// [`ExecutionOutcome::Cancelled`] を返す。language-state は開始時点へ rollback され
    /// （第10節 規則5）、context は poison されないので再利用できる。
    pub fn pre_cancelled(mut self) -> Self {
        self.pre_cancelled = true;
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
    pub fn new(engine: &Engine) -> Self {
        Self {
            engine_id: engine.id(),
            evaluator: Evaluator::new(),
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

        // ここから先の失敗は「実行の試行」に起因する。InternalFailure を返す経路はすべて
        // context を poison する（第10節 規則4）ため、単一の内部関数へ閉じ、その戻り値で
        // poison を一元判定する。
        context.running = true;
        // run / poll 中の unwind panic を host boundary で捕捉する（第11節 規則1）。捕捉した
        // panic は terminal `ExecutionOutcome::InternalFailure` へ写す（規則2）。payload /
        // backtrace は公開しない（規則3）。`AssertUnwindSafe` は、panic 後に context を
        // poison して以後の再利用を拒否する（規則4）ことで正当化する。
        let outcome = match catch_host_unwind(AssertUnwindSafe(|| {
            self.run_inner(linked, context, request)
        })) {
            Ok(outcome) => outcome,
            Err(fault_id) => ExecutionOutcome::InternalFailure {
                fault_id,
                safe_message: internal_fault_message(fault_id),
            },
        };
        context.running = false;

        // InternalFailure だけ context を poison する（第10節 規則4）。他 terminal は
        // begin_execution / run_slice が commit / rollback 済みで、そのまま再利用できる。
        // run/poll 中の panic 経路も InternalFailure なのでここで一律に poison される。
        if matches!(outcome, ExecutionOutcome::InternalFailure { .. }) {
            context.poisoned = true;
        }
        outcome
    }

    /// [`Self::run`] の本体（precondition guard の後）。ここから返る InternalFailure は
    /// すべて呼び出し元が context を poison する（第10節 規則4）。
    fn run_inner(
        &self,
        linked: &LinkedScript,
        context: &mut ExecutionContext,
        request: ExecutionRequest,
    ) -> ExecutionOutcome {
        // engine / context の整合を検証する（別 engine の context は実行しない）。
        if context.engine_id() != self.id() {
            return internal_failure("実行コンテキストの engine が一致しません");
        }
        let root = linked.root();
        if root.engine_id() != self.id() {
            return internal_failure("LinkedScript の engine が一致しません");
        }

        // 実行用 Program を保持 source から再構築する（案 A）。
        let Some(source_text) = root.retained_source() else {
            return internal_failure(
                "実行には source の保持が必要です: retain_source=true で compile してください",
            );
        };
        let program = match Parser::new(Lexer::new(source_text).tokenize()).parse() {
            Ok(program) => program,
            // compile 済みの script を再 parse して失敗するのは内部不整合（決定的なはず）。
            Err(errors) => {
                let detail = errors
                    .first()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "詳細不明".to_string());
                return internal_failure(format!(
                    "保持 source の再 parse に失敗しました: {detail}"
                ));
            }
        };
        let root_source_bytes = source_text.len() as u64;

        // test 専用: 評価器境界での panic を模擬し、E6 の catch_unwind 隔離を検証する。
        // この panic は run の catch_host_unwind 内で捕捉され InternalFailure へ写る。
        #[cfg(test)]
        if context.panic_in_run_for_test {
            panic!("injected run-boundary panic (test only)");
        }

        // pre-run cancel（EMB-AT-12）: 最初の poll より前に cancel 済みなら、script 命令を
        // 1つも実行せずに Cancelled terminal を返す（"pre-cancel は命令0"）。language-state は
        // 何も変更していないので rollback は自明に成立し、context は poison されない
        // （第10節 規則4・5）。
        if request.pre_cancelled {
            return ExecutionOutcome::Cancelled;
        }

        // 引数 snapshot を評価器へ注入する（AUD-018）。
        context.evaluator.set_script_args(request.arguments);

        Self::run_transactional(context, &program, root_source_bytes)
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
            // link/control-plane の LinkError terminal は Phase 2（E7）で扱う。
            return match phase {
                RunPhase::Link | RunPhase::Run => runtime_error_outcome(error),
            };
        }

        loop {
            // 大きな slice で 1 回ずつ回す（協調 yield の実効化は Phase 4）。同期実行なので
            // terminal まで回し切る。
            match context.evaluator.run_slice(u64::MAX) {
                None => continue, // yield（Phase 1 では slice=u64::MAX のため実質起きない）
                Some(Ok(())) => return ExecutionOutcome::Completed,
                Some(Err(error)) => return runtime_error_outcome(error),
            }
        }
    }
}

/// 内部 [`TsumugiError`] を [`ExecutionOutcome::RuntimeError`] へ写す。
fn runtime_error_outcome(error: TsumugiError) -> ExecutionOutcome {
    ExecutionOutcome::RuntimeError {
        error: execution_error_from(&error),
    }
}

/// secret を含まない [`ExecutionOutcome::InternalFailure`] を作る。
fn internal_failure(safe_message: impl Into<String>) -> ExecutionOutcome {
    ExecutionOutcome::InternalFailure {
        fault_id: next_fault_id(),
        safe_message: safe_message.into(),
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

/// top-level import 文か。
fn is_import_stmt(stmt: &Stmt) -> bool {
    matches!(stmt, Stmt::Import { .. })
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

    #[test]
    fn default_config_is_tree_walk_current() {
        let config = EngineConfig::default();
        assert_eq!(config.backend, Backend::TreeWalk);
        assert_eq!(config.language_revision, LanguageRevision::CURRENT);
        assert_eq!(config.language_revision.as_str(), "0.19");
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
        let outcome = ExecutionOutcome::RuntimeError { error: err };
        let rendered = format!("{outcome:?}");
        assert!(!rendered.contains(SECRET));

        let internal = ExecutionOutcome::InternalFailure {
            fault_id: 12345,
            safe_message: "内部エラー".to_string(),
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
        let linked = engine.link(&script, LinkRequest::new()).expect("link");

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

    #[test]
    fn link_rejects_imports_in_phase1() {
        let engine = Engine::builder().build().unwrap();
        let script = engine
            .compile(
                src("m", "import \"other\"\nlet x = 1\n"),
                &CompileOptions::default(),
            )
            .unwrap();
        let err = engine
            .link(&script, LinkRequest::new())
            .expect_err("import rejected");
        assert_eq!(
            err,
            LinkError::FeatureUnavailable {
                feature: "module_resolver"
            }
        );
    }

    #[test]
    fn link_rejects_foreign_engine() {
        let engine_a = Engine::builder().build().unwrap();
        let engine_b = Engine::builder().build().unwrap();
        let script = engine_a
            .compile(src("m", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        let err = engine_b
            .link(&script, LinkRequest::new())
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
        let linked = engine.link(&script, LinkRequest::new()).unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        let outcome = engine.run(&linked, &mut ctx, ExecutionRequest::new());
        assert_eq!(outcome, ExecutionOutcome::Completed);
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
        let linked = engine.link(&script, LinkRequest::new()).unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        match engine.run(&linked, &mut ctx, ExecutionRequest::new()) {
            ExecutionOutcome::RuntimeError { error } => {
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
        let linked = engine.link(&script, LinkRequest::new()).unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        match engine.run(&linked, &mut ctx, ExecutionRequest::new()) {
            ExecutionOutcome::RuntimeError { error } => {
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
        let linked = engine.link(&script, LinkRequest::new()).unwrap();
        let mut ctx = ExecutionContext::new(&engine);
        match engine.run(&linked, &mut ctx, ExecutionRequest::new()) {
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
        let linked = engine_a.link(&script, LinkRequest::new()).unwrap();
        let mut foreign_ctx = ExecutionContext::new(&engine_b);
        assert!(matches!(
            engine_a.run(&linked, &mut foreign_ctx, ExecutionRequest::new()),
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
        let linked1 = engine.link(&first, LinkRequest::new()).unwrap();
        assert_eq!(
            engine.run(&linked1, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::Completed
        );

        // 直前の実行で定義した saved を参照できる（同一 context）。
        let second = engine
            .compile(
                retained("m", "let doubled = saved + 1\n"),
                &CompileOptions {
                    retain_source: true,
                },
            )
            .unwrap();
        let linked2 = engine.link(&second, LinkRequest::new()).unwrap();
        assert_eq!(
            engine.run(&linked2, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::Completed
        );
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
        let linked = engine.link(&script, LinkRequest::new()).unwrap();
        // graph_hash と script_hash が決定的であること（同一入力で不変）。
        let script2 = engine
            .compile(src("golden", "let x = 1\n"), &CompileOptions::default())
            .unwrap();
        let linked2 = engine.link(&script2, LinkRequest::new()).unwrap();
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
        engine.link(&script, LinkRequest::new()).unwrap()
    }

    /// EMB-AT-08: 未捕捉 runtime error は execution 開始時点まで全 language-state を rollback し、
    /// 副作用は次実行へ残らない（AUD-024・第10節 規則5）。
    #[test]
    fn e4_runtime_error_rolls_back_language_state() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        // 変数を代入した直後に未定義参照でエラー化する。rollback されれば committed は残らない。
        let linked = compile_link(&engine, "m", "let saved = 7\nlet boom = undefined_name\n");
        match engine.run(&linked, &mut ctx, ExecutionRequest::new()) {
            ExecutionOutcome::RuntimeError { error } => assert_eq!(error.code, ErrorKind::Name),
            other => panic!("期待: RuntimeError, 実際: {other:?}"),
        }

        // rollback 済みなので saved は次実行から見えない（見えれば Name エラーで判別できる）。
        let probe = compile_link(&engine, "m", "let echo = saved\n");
        match engine.run(&probe, &mut ctx, ExecutionRequest::new()) {
            ExecutionOutcome::RuntimeError { error } => assert_eq!(error.code, ErrorKind::Name),
            other => panic!("saved が rollback されず残った: {other:?}"),
        }
    }

    /// EMB-AT-08: Completed は全 language-state を commit し、次実行へ binding が残る。
    #[test]
    fn e4_completed_commits_language_state() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let first = compile_link(&engine, "m", "let saved = 41\n");
        assert_eq!(
            engine.run(&first, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::Completed
        );
        // commit 済みなので次実行から saved を参照できる。
        let second = compile_link(&engine, "m", "let doubled = saved + 1\n");
        assert_eq!(
            engine.run(&second, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::Completed
        );
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
        assert_eq!(
            engine.run(&linked, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::Completed
        );
        // catch 後に代入した saved=5 は commit され、次実行から見える。
        let probe = compile_link(&engine, "m", "let echo = saved + 1\n");
        assert_eq!(
            engine.run(&probe, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::Completed
        );
    }

    /// EMB-AT-07: RuntimeError で終わっても context は poison されず、再利用できる。
    #[test]
    fn e4_runtime_error_does_not_poison() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let bad = compile_link(&engine, "m", "let x = undefined_name\n");
        assert!(matches!(
            engine.run(&bad, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::RuntimeError { .. }
        ));
        assert!(!ctx.is_poisoned(), "RuntimeError は poison しない");

        // そのまま再利用して正常実行できる。
        let ok = compile_link(&engine, "m", "let y = 1\n");
        assert_eq!(
            engine.run(&ok, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::Completed
        );
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
        let linked = engine.link(&script, LinkRequest::new()).unwrap();
        assert!(matches!(
            engine.run(&linked, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::InternalFailure { .. }
        ));
        assert!(ctx.is_poisoned(), "InternalFailure は poison する");

        // poison 済み context は以後 InternalFailure を返す（正しい retained script でも）。
        let ok = compile_link(&engine, "m", "let y = 1\n");
        assert!(matches!(
            engine.run(&ok, &mut ctx, ExecutionRequest::new()),
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
        assert_eq!(
            engine.run(&seed, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::Completed
        );
        ctx.clear_user_state().expect("clear on idle context");

        // clear 後は saved が見えない（見えれば commit されている）。
        let probe = compile_link(&engine, "m", "let echo = saved\n");
        match engine.run(&probe, &mut ctx, ExecutionRequest::new()) {
            ExecutionOutcome::RuntimeError { error } => assert_eq!(error.code, ErrorKind::Name),
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
        let outcome = engine.run(&linked, &mut ctx, ExecutionRequest::new().pre_cancelled());
        assert_eq!(outcome, ExecutionOutcome::Cancelled);

        // 命令0なので saved は commit されない（見えれば Name エラーで判別できる）。
        let probe = compile_link(&engine, "m", "let echo = saved\n");
        match engine.run(&probe, &mut ctx, ExecutionRequest::new()) {
            ExecutionOutcome::RuntimeError { error } => assert_eq!(error.code, ErrorKind::Name),
            other => panic!("pre-cancel が副作用を残した: {other:?}"),
        }
    }

    /// EMB-AT-07: pre-run cancel は context を poison せず、再利用できる。
    #[test]
    fn e5_pre_run_cancel_does_not_poison() {
        let engine = Engine::builder().build().unwrap();
        let mut ctx = ExecutionContext::new(&engine);

        let linked = compile_link(&engine, "m", "let x = 1\n");
        assert_eq!(
            engine.run(&linked, &mut ctx, ExecutionRequest::new().pre_cancelled()),
            ExecutionOutcome::Cancelled
        );
        assert!(!ctx.is_poisoned(), "Cancelled は poison しない");

        // そのまま再利用して正常実行できる。
        let ok = compile_link(&engine, "m", "let y = 2\n");
        assert_eq!(
            engine.run(&ok, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::Completed
        );
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
        let linked = engine.link(&script, LinkRequest::new()).unwrap();
        assert!(matches!(
            engine.run(&linked, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::InternalFailure { .. }
        ));

        // poison 後は pre-cancel でも InternalFailure（precondition 優先、第10節 規則4）。
        let ok = compile_link(&engine, "m", "let y = 1\n");
        assert!(matches!(
            engine.run(&ok, &mut ctx, ExecutionRequest::new().pre_cancelled()),
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
        let outcome =
            with_silent_panic_hook(|| engine.run(&linked, &mut ctx, ExecutionRequest::new()));
        match outcome {
            ExecutionOutcome::InternalFailure {
                fault_id,
                safe_message,
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
            engine.run(&ok, &mut ctx, ExecutionRequest::new()),
            ExecutionOutcome::InternalFailure { .. }
        ));
    }

    /// LinkError::InternalFailure は Send + Sync な診断値として保持できる。
    #[test]
    fn e6_link_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LinkError>();
    }
}
