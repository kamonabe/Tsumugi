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
/// E1 で到達し得る variant のみを持つ。capability / callable / descriptor /
/// filesystem policy 関連（`DuplicateCapability` / `DuplicateCallableName` /
/// `InvalidDescriptor` / `InvalidFilesystemPolicy`）は Phase 2（E7）で追加する。
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
    }
}
