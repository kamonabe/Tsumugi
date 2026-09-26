//! Phase 2 capability model（スライス C1）。
//!
//! [Capability Model 仕様](../docs/capability-model.md) 第3・8・13節が定義する
//! deny-by-default な [`CapabilitySet`] の公開 surface を導入する。
//!
//! # スライス境界（C1）
//!
//! 本モジュールは C1 の完了条件——[`CapabilitySet::empty`] を唯一の library 既定値とし、
//! adapter を呼ぶ前に authority の有無で拒否できること——を満たすための型と、
//! policy 相関 ID（[`CapabilitySetId`]、CAP-AT-28 の golden bytes）を用意する。
//!
//! adapter trait の**実行時メソッド**（`now_utc` / `read_line` / `open_file` など、
//! [`CapabilityCallContext`] や `AdapterError` を引数に取るもの）は、それぞれの authority を
//! 配線する後続スライス（C3 Environment/Clock、C4 Stdin/Stdout、C5 Filesystem、
//! C6 ModuleResolver、C8 HostFunction）で追加する。C1 の trait は set への格納と
//! [`CapabilitySetId`] 計算に必要な `policy_id()`（および [`DirectoryHandle::symlink_policy`]）
//! だけを持つ。
//!
//! # deny-by-default（仕様第2節 原則1）
//!
//! [`CapabilitySet::empty`] は全 8 authority を拒否する。host だけが grant でき、script は
//! capability を生成・列挙・複製・grant・revoke できない。set は start 時 freeze され、
//! 実行中の grant/revoke はない（原則2・3）。

use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroU64, NonZeroU128};
use std::sync::Arc;
use std::time::SystemTime;

use crate::embedding::ConfigError;

/// capability の種別（仕様第3節 `CapabilityKind`）。
///
/// 8 個の独立した authority で、一方から他方を導出しない（原則5）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum CapabilityKind {
    /// 環境変数 snapshot 読み取り（`env()`）。
    Environment,
    /// 時刻取得（`now()`）。
    Clock,
    /// 標準入力（`input()`）。
    Stdin,
    /// 標準出力（`print`）。
    Stdout,
    /// ファイルシステム操作。
    Filesystem,
    /// `exit()` による structured terminal。
    ProcessExit,
    /// import 解決。
    ModuleResolver,
    /// 登録 host function の呼び出し。
    HostFunction,
}

impl CapabilityKind {
    /// [`CapabilitySetId`] encoding 用の固定 tag（仕様第3.1節）。
    const fn tag(self) -> u8 {
        match self {
            Self::Environment => 0x01,
            Self::Clock => 0x02,
            Self::Stdin => 0x03,
            Self::Stdout => 0x04,
            Self::Filesystem => 0x05,
            Self::ProcessExit => 0x06,
            Self::ModuleResolver => 0x07,
            Self::HostFunction => 0x08,
        }
    }
}

/// 環境変数値の機密分類（仕様第5節 `DataClassification`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum DataClassification {
    /// 公開値。
    Public,
    /// 機微値。
    Sensitive,
    /// 秘密値。
    Secret,
}

impl DataClassification {
    /// [`CapabilitySetId`] encoding 用の固定 tag（仕様第3.1節）。
    const fn tag(self) -> u8 {
        match self {
            Self::Public => 0x00,
            Self::Sensitive => 0x01,
            Self::Secret => 0x02,
        }
    }
}

/// 環境変数の値と分類（仕様第5節 `EnvironmentValue`）。
///
/// `Debug` は本文値を出さず、CAP-AT-14/22 のとおり secret を漏らさない。
#[derive(Clone)]
pub struct EnvironmentValue {
    value: String,
    classification: DataClassification,
}

impl EnvironmentValue {
    /// 値と分類から作る。値は UTF-8・NUL なし。
    pub fn new(
        value: impl Into<String>,
        classification: DataClassification,
    ) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.as_bytes().contains(&0) {
            return Err(ConfigError::IdentifierContainsNul {
                field: "environment_value",
            });
        }
        Ok(Self {
            value,
            classification,
        })
    }

    /// script へ公開する生の値。
    pub fn expose_to_script(&self) -> &str {
        &self.value
    }

    /// 機密分類。
    pub fn classification(&self) -> DataClassification {
        self.classification
    }
}

impl std::fmt::Debug for EnvironmentValue {
    /// 本文値を出さず、分類だけを表示する（secret-free）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentValue")
            .field("classification", &self.classification)
            .finish_non_exhaustive()
    }
}

/// 環境変数 snapshot（仕様第5節 `EnvironmentSnapshot`）。
///
/// start 前に完成し、run 中に process environment を再読しない。key は UTF-8・NUL なし・
/// 1..=256 bytes。重複 key は error。
#[derive(Clone)]
pub struct EnvironmentSnapshot(Arc<BTreeMap<String, EnvironmentValue>>);

impl EnvironmentSnapshot {
    /// 空 snapshot。
    pub fn empty() -> Self {
        Self(Arc::new(BTreeMap::new()))
    }

    /// key/value の集合から作る。重複 key は [`ConfigError`]。
    pub fn from_entries(
        entries: impl IntoIterator<Item = (String, EnvironmentValue)>,
    ) -> Result<Self, ConfigError> {
        let mut map = BTreeMap::new();
        for (key, value) in entries {
            validate_env_key(&key)?;
            if map.insert(key.clone(), value).is_some() {
                return Err(ConfigError::DuplicateCallableName { name: key });
            }
        }
        Ok(Self(Arc::new(map)))
    }

    /// snapshot 内の key を昇順で返す。
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// key に対応する値を返す（crate 内部限定、仕様第5節の `pub(crate) get`）。
    ///
    /// Environment authority を builtin へ接続する C3 の `env()` が使う。
    pub(crate) fn get(&self, key: &str) -> Option<&EnvironmentValue> {
        self.0.get(key)
    }
}

impl std::fmt::Debug for EnvironmentSnapshot {
    /// 値本文を出さず、key と分類のみ表示する（secret-free）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|(k, v)| (k, v.classification())))
            .finish()
    }
}

/// 環境変数 key を検証する（UTF-8・NUL なし・1..=256 bytes）。
fn validate_env_key(key: &str) -> Result<(), ConfigError> {
    if key.is_empty() {
        return Err(ConfigError::EmptyIdentifier {
            field: "environment_key",
        });
    }
    if key.len() > 256 {
        return Err(ConfigError::IdentifierTooLong {
            field: "environment_key",
            max_bytes: 256,
        });
    }
    if key.as_bytes().contains(&0) {
        return Err(ConfigError::IdentifierContainsNul {
            field: "environment_key",
        });
    }
    Ok(())
}

/// 時刻 authority（仕様第6節 `Clock`）。
///
/// `now()` はこの trait だけを使う。deadline は script 用 Clock ではなく Engine の
/// monotonic clock を使う（仕様第6節）。
///
/// # スライス境界（C3）
///
/// 仕様第6節の最終形は `now_utc(&mut CapabilityCallContext) -> Result<SystemTime, AdapterError>`
/// だが、`CapabilityCallContext` / `AdapterError` は host call 境界を配線する後続スライス
/// （C4 Stdin/Stdout・C8 HostFunction）で導入する横断型である。C3 はそれらを待たずに
/// Clock authority を `now()` へ接続するため、context/error を取らない最小形の
/// [`now_utc`](Clock::now_utc) を提供する。context 引数と `AdapterError` への拡張は、
/// それらの型を導入するスライスで行う（C1 が実行時メソッドを後続へ委ねたのと同じ方針）。
pub trait Clock: Send + Sync + 'static {
    /// policy 相関 ID（構成内容ごとに変える非ゼロ値。pointer address 不可）。
    fn policy_id(&self) -> NonZeroU128;

    /// 現在の UTC 時刻を返す。
    ///
    /// C3 の最小形。host adapter は自身の時刻源から `SystemTime` を返す。
    fn now_utc(&self) -> SystemTime;
}

/// OS の system clock を使う [`Clock`]（ambient 互換経路と CLI legacy profile 用）。
///
/// [`CapabilitySet::ambient_compat`] が使う既定 clock。埋め込み host は自前の
/// [`Clock`] 実装を grant できる。
pub struct SystemClock {
    policy_id: NonZeroU128,
}

impl SystemClock {
    /// 指定 policy 相関 ID で作る。
    pub const fn new(policy_id: NonZeroU128) -> Self {
        Self { policy_id }
    }
}

impl Clock for SystemClock {
    fn policy_id(&self) -> NonZeroU128 {
        self.policy_id
    }

    fn now_utc(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// 決定的な固定時刻を返す test/host utility の [`Clock`]（仕様第6節 `FixedClock`）。
///
/// 同じ設定で常に同じ時刻を返すため、`now()` を含む script の結果を再現できる
/// （CAP-AT-06）。
pub struct FixedClock {
    policy_id: NonZeroU128,
    instant: SystemTime,
}

impl FixedClock {
    /// policy 相関 ID と固定時刻から作る。
    pub const fn new(policy_id: NonZeroU128, instant: SystemTime) -> Self {
        Self { policy_id, instant }
    }
}

impl Clock for FixedClock {
    fn policy_id(&self) -> NonZeroU128 {
        self.policy_id
    }

    fn now_utc(&self) -> SystemTime {
        self.instant
    }
}

/// adapter が返すエラー（仕様第4節 `AdapterError`）。
///
/// C4 で host 起因の失敗（`Host`）、C5 で filesystem 用の `SecureResolutionUnsupported`
/// を持つ。budget/deadline/cancel を運ぶ `Control` variant は、その制御・予約 context を
/// 導入する後続スライス（Phase 3/4）で追加する（C1/C3 が実行時 context を後続へ委ねたのと
/// 同じ方針）。
#[derive(Debug)]
#[non_exhaustive]
pub enum AdapterError {
    /// host 実装内部で起きた失敗。script 操作中は sanitized な canonical `host` error として
    /// catch 可能にし、`null`/`false` へ潰さない（仕様第4・7節）。
    Host(String),
    /// platform が root 拘束・symlink policy を保証できない（仕様第8.3節 契約3）。
    ///
    /// 文字列 prefix check へ fallback せず fail closed する。script filesystem 操作中は
    /// code `secure_resolution_unsupported` の canonical `host` error（catch 可能）へ写す。
    SecureResolutionUnsupported,
    /// directory 列挙中の個別 entry 取得失敗（REV-009 §17.4）。safe profile では部分結果を
    /// 成功として返さず、category `directory_read` の catch 可能な `host` error へ写す。
    DirectoryReadFailed,
    /// directory entry 名が非 UTF-8（REV-009 §17.4）。safe profile では lossy 変換で同一視せず、
    /// category `invalid_encoding` の catch 可能な `host` error へ写す。
    NonUtf8EntryName,
}

/// [`Input::read_line`] の1行読み取り結果（仕様第7節 `InputLine`）。
#[derive(Debug)]
pub enum InputLine {
    /// 改行を含まない1行分のテキスト（末尾改行は adapter が除去済み）。
    Line(String),
    /// 入力終端。script では `null` へ写す。
    Eof,
}

/// 標準入力 authority（仕様第7節 `Input`）。
///
/// C4 の最小形は `read_line` を context 引数・`ReadLimit` なしで提供する。有限 meter と
/// N 境界（`ReadLimit`）・`CapabilityCallContext` は Phase 3 で追加する（仕様第7節末尾）。
pub trait Input: Send + Sync + 'static {
    /// policy 相関 ID。
    fn policy_id(&self) -> NonZeroU128;

    /// 1行読み取る。EOF は [`InputLine::Eof`]、host 起因の失敗は [`AdapterError::Host`]。
    fn read_line(&self) -> Result<InputLine, AdapterError>;
}

/// 標準出力 authority（仕様第7節 `Output`）。
///
/// C4 の最小形は `write_all`/`flush` を context 引数なしで提供する。残量 N 境界と
/// `CapabilityCallContext` は Phase 3 で追加する（仕様第7節末尾）。
pub trait Output: Send + Sync + 'static {
    /// policy 相関 ID。
    fn policy_id(&self) -> NonZeroU128;

    /// UTF-8 bytes を書き出す。host 起因の失敗は [`AdapterError::Host`]。
    fn write_all(&self, bytes: &[u8]) -> Result<(), AdapterError>;

    /// バッファを flush する。host 起因の失敗は [`AdapterError::Host`]。
    fn flush(&self) -> Result<(), AdapterError>;
}

/// OS の標準入力を読む [`Input`]（ambient 互換経路と CLI legacy profile 用）。
///
/// [`CapabilitySet::ambient_compat`] が使う既定 stdin。従来 `input()` が直接読んでいた
/// `std::io::stdin()` をこの adapter 内へ集約する。
pub struct SystemInput {
    policy_id: NonZeroU128,
}

impl SystemInput {
    /// 指定 policy 相関 ID で作る。
    pub const fn new(policy_id: NonZeroU128) -> Self {
        Self { policy_id }
    }
}

impl Input for SystemInput {
    fn policy_id(&self) -> NonZeroU128 {
        self.policy_id
    }

    fn read_line(&self) -> Result<InputLine, AdapterError> {
        use std::io::BufRead;
        let mut buf = String::new();
        match std::io::stdin().lock().read_line(&mut buf) {
            Ok(0) => Ok(InputLine::Eof),
            Ok(_) => {
                if buf.ends_with('\n') {
                    buf.pop();
                    if buf.ends_with('\r') {
                        buf.pop();
                    }
                }
                Ok(InputLine::Line(buf))
            }
            Err(e) => Err(AdapterError::Host(e.to_string())),
        }
    }
}

/// OS の標準出力へ書き出す [`Output`]（ambient 互換経路と CLI legacy profile 用）。
///
/// [`CapabilitySet::ambient_compat`] が使う既定 stdout。broken pipe でも panic せず
/// [`AdapterError::Host`] へ写す（従来 `write_stdout_line` が持っていた挙動、AUD-035）。
pub struct SystemOutput {
    policy_id: NonZeroU128,
}

impl SystemOutput {
    /// 指定 policy 相関 ID で作る。
    pub const fn new(policy_id: NonZeroU128) -> Self {
        Self { policy_id }
    }
}

impl Output for SystemOutput {
    fn policy_id(&self) -> NonZeroU128 {
        self.policy_id
    }

    fn write_all(&self, bytes: &[u8]) -> Result<(), AdapterError> {
        use std::io::Write;
        std::io::stdout()
            .lock()
            .write_all(bytes)
            .map_err(|e| AdapterError::Host(e.to_string()))
    }

    fn flush(&self) -> Result<(), AdapterError> {
        use std::io::Write;
        std::io::stdout()
            .lock()
            .flush()
            .map_err(|e| AdapterError::Host(e.to_string()))
    }
}

/// import 解決 authority（仕様第10節 `ModuleResolver`）。C1 は `policy_id` のみ。解決は C6。
pub trait ModuleResolver: Send + Sync + 'static {
    /// policy 相関 ID。
    fn policy_id(&self) -> NonZeroU128;
}

/// filesystem 操作粒度（仕様第8.1節 `FsOperation`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum FsOperation {
    /// 既存 file の読み取り。
    Read,
    /// 既存 file の overwrite/append。
    Write,
    /// 新規 file/directory/destination entry。
    Create,
    /// 削除。
    Delete,
    /// メタデータ取得。
    Metadata,
    /// directory 列挙。
    List,
    /// import（runtime Read と独立）。
    Import,
    /// 再帰削除（`remove_tree`。`Delete`/`EmptyDirectory` から暗黙昇格させない、REV-021 §17.6）。
    ///
    /// `Ord` の導出は宣言順に従うため、`BTreeSet<FsOperation>` の走査順（§8.6 encoding）と
    /// tag 昇順を一致させるべく、本 variant は必ず末尾（最大 tag）へ置く。
    RecursiveDelete,
}

impl FsOperation {
    /// [`CapabilitySetId`] encoding 用の固定 tag（仕様第3.1節）。
    const fn tag(self) -> u8 {
        match self {
            Self::Read => 0x01,
            Self::Write => 0x02,
            Self::Create => 0x03,
            Self::Delete => 0x04,
            Self::Metadata => 0x05,
            Self::List => 0x06,
            Self::Import => 0x07,
            Self::RecursiveDelete => 0x08,
        }
    }
}

/// symlink 追従 policy（仕様第8.1節 `SymlinkPolicy`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum SymlinkPolicy {
    /// 途中/final symlink をすべて拒否。
    DenyAll,
    /// 解決先を同じ root handle へ拘束。
    FollowWithinRoot,
    /// final entry 自体を操作対象にできる（delete/rename/metadata の一部）。
    OperateOnFinalEntry,
}

impl SymlinkPolicy {
    /// [`CapabilitySetId`] encoding 用の固定 tag（仕様第3.1節）。
    const fn tag(self) -> u8 {
        match self {
            Self::DenyAll => 0x00,
            Self::FollowWithinRoot => 0x01,
            Self::OperateOnFinalEntry => 0x02,
        }
    }
}

/// mount 名（仕様第8.1節 `MountName`）。ASCII `[A-Za-z][A-Za-z0-9_-]{0,31}`。
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MountName(String);

impl MountName {
    /// mount 名を検証して作る。
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = matches!(bytes.first(), Some(b) if b.is_ascii_alphabetic())
            && bytes.len() <= 32
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-');
        if !valid {
            return Err(ConfigError::InvalidFilesystemPolicy {
                code: "invalid_mount_name",
            });
        }
        Ok(Self(value))
    }

    /// mount 名文字列。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// portable path-handle（仕様第8.3節 `DirectoryHandle`）。
///
/// C5 の最小形は open/create_dir/metadata/list/remove/rename を `CapabilityCallContext`
/// なしで提供する（C3/C4 と同じく Phase 3 の制御・予約 context は後続へ委譲する）。有限
/// budget（`max_bytes`/`max_entries`）は `Option<NonZeroU64>` の器だけ用意し、上限適用は
/// Phase 3 で adapter へ渡す。
///
/// adapter は root 拘束と symlink policy を単一 handle へ bind して保証する（契約1〜7）。
/// `canonicalize` でチェック後に元 path を `std::fs` へ渡す実装は禁止。platform が保証
/// できなければ [`AdapterError::SecureResolutionUnsupported`] を返し、文字列 prefix へ
/// fallback しない。
pub trait DirectoryHandle: Send + Sync + 'static {
    /// policy 相関 ID。
    fn policy_id(&self) -> NonZeroU128;
    /// この handle の symlink policy。
    fn symlink_policy(&self) -> SymlinkPolicy;

    /// file を open する。root 内へ secure に bind できなければ拒否する。
    fn open_file(
        &self,
        path: &RelativePath,
        request: OpenFileRequest,
    ) -> Result<Box<dyn FileHandle>, AdapterError>;

    /// directory を作る（親は既存前提。`Create` operation）。
    fn create_dir(&self, path: &RelativePath) -> Result<(), AdapterError>;

    /// メタデータを取得する。`follow_final=false` は final symlink entry 自体を対象にする。
    fn metadata(
        &self,
        path: &RelativePath,
        follow_final: bool,
    ) -> Result<PublicMetadata, AdapterError>;

    /// directory を列挙する。`max_entries` は Phase 3 の budget 器（C5 では None 相当）。
    fn list(
        &self,
        path: &RelativePath,
        max_entries: Option<NonZeroU64>,
    ) -> Result<Vec<DirectoryEntry>, AdapterError>;

    /// entry を削除する（`FileOrSymlink` / `EmptyDirectory`）。
    fn remove(&self, path: &RelativePath, kind: RemoveKind) -> Result<(), AdapterError>;

    /// entry を rename する。両 handle 認可後に単一 rename call を行う（契約6）。
    fn rename(
        &self,
        from: &RelativePath,
        to_directory: &dyn DirectoryHandle,
        to: &RelativePath,
        replace: bool,
    ) -> Result<(), AdapterError>;
}

/// open 済み file への path-handle（仕様第8.3節 `FileHandle`）。
///
/// C5 の最小形は read/write/metadata を `CapabilityCallContext` なしで提供する。`max_bytes`
/// は Phase 3 の budget 器で、C5 では未適用（`None` 相当）。`Send` だが `Sync` は要求しない
/// （仕様の signature に合わせる）。
pub trait FileHandle: Send + 'static {
    /// 全 byte を読み取る。`max_bytes` は Phase 3 の上限器（C5 では None 相当）。
    fn read_to_end(&mut self, max_bytes: Option<NonZeroU64>) -> Result<Vec<u8>, AdapterError>;

    /// bytes を書き出す。
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), AdapterError>;

    /// メタデータを取得する。
    fn metadata(&self) -> Result<PublicMetadata, AdapterError>;
}

/// 1 つの mount root（仕様第8.1節 `FilesystemRoot`）。
#[derive(Clone)]
pub struct FilesystemRoot {
    /// mount 名。
    pub mount: MountName,
    /// policy 相関 ID。
    pub policy_id: NonZeroU128,
    /// 許可する操作集合。
    pub operations: BTreeSet<FsOperation>,
    /// symlink policy。
    pub symlink_policy: SymlinkPolicy,
    /// path-handle adapter。
    pub adapter: Arc<dyn DirectoryHandle>,
}

impl FilesystemRoot {
    /// root を検証して作る。
    ///
    /// 空 operations は configuration error。`adapter` の `policy_id`/`symlink_policy` が
    /// 指定値と一致することを検証する（仕様第8.1節）。
    pub fn new(
        mount: MountName,
        policy_id: NonZeroU128,
        operations: BTreeSet<FsOperation>,
        symlink_policy: SymlinkPolicy,
        adapter: Arc<dyn DirectoryHandle>,
    ) -> Result<Self, ConfigError> {
        if operations.is_empty() {
            return Err(ConfigError::InvalidFilesystemPolicy {
                code: "empty_operations",
            });
        }
        if adapter.policy_id() != policy_id {
            return Err(ConfigError::InvalidFilesystemPolicy {
                code: "adapter_policy_id_mismatch",
            });
        }
        if adapter.symlink_policy() != symlink_policy {
            return Err(ConfigError::InvalidFilesystemPolicy {
                code: "adapter_symlink_policy_mismatch",
            });
        }
        Ok(Self {
            mount,
            policy_id,
            operations,
            symlink_policy,
            adapter,
        })
    }
}

impl std::fmt::Debug for FilesystemRoot {
    /// adapter 本体を出さず、policy 情報だけを表示する。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesystemRoot")
            .field("mount", &self.mount)
            .field("policy_id", &self.policy_id)
            .field("operations", &self.operations)
            .field("symlink_policy", &self.symlink_policy)
            .finish_non_exhaustive()
    }
}

/// filesystem authority 全体（仕様第8.1節 `FilesystemCapability`）。
#[derive(Clone)]
pub struct FilesystemCapability {
    roots: Arc<[FilesystemRoot]>,
}

impl FilesystemCapability {
    /// root 集合から作る。空・mount 重複は configuration error。
    ///
    /// 同じ policy ID は、同じ adapter Arc・operations・symlink policy へ別 mount alias を
    /// 付ける場合だけ許可する（仕様第8.1節）。
    pub fn new(roots: impl IntoIterator<Item = FilesystemRoot>) -> Result<Self, ConfigError> {
        let roots: Vec<FilesystemRoot> = roots.into_iter().collect();
        if roots.is_empty() {
            return Err(ConfigError::InvalidFilesystemPolicy { code: "no_roots" });
        }
        let mut seen_mounts = BTreeSet::new();
        for root in &roots {
            if !seen_mounts.insert(root.mount.clone()) {
                return Err(ConfigError::InvalidFilesystemPolicy {
                    code: "duplicate_mount",
                });
            }
        }
        Ok(Self {
            roots: roots.into(),
        })
    }

    /// root を返す。
    pub fn roots(&self) -> impl Iterator<Item = &FilesystemRoot> {
        self.roots.iter()
    }
}

impl std::fmt::Debug for FilesystemCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesystemCapability")
            .field("roots", &self.roots)
            .finish()
    }
}

impl FilesystemCapability {
    /// 指定 mount の root を返す（完全一致。prefix 一致や登録順 fallback はしない、仕様第8.2節）。
    ///
    /// C5-c の fs builtin dispatch が routing に使う。
    pub(crate) fn root(&self, mount: &MountName) -> Option<&FilesystemRoot> {
        self.roots.iter().find(|r| &r.mount == mount)
    }
}

/// script が指定する mount 相対 path（仕様第8.2節 `RelativePath`）。
///
/// component は UTF-8・1 byte 以上・NUL なし。`.`/`..`/空 component/絶対 path/backslash は
/// [`FilesystemTarget::parse`] が事前に排除するため、ここへ来る component はすべて検証済み。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelativePath {
    components: Arc<[String]>,
}

impl RelativePath {
    /// 検証済み component を順に返す。
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.components.iter().map(String::as_str)
    }
}

/// script path の lexical エラー（仕様第8.2節 `PathError`）。
///
/// capability lookup より先に検出し、catch 可能な `argument` error へ写す（authority 不足の
/// channel とは混在させない、仕様第8.5節）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathError {
    /// path が空。
    Empty,
    /// NUL を含む。
    ContainsNul,
    /// 絶対 path（先頭 `/` や drive prefix）。
    Absolute,
    /// `.` component。
    DotComponent,
    /// `..` component。
    ParentComponent,
    /// 空 component（`//` 等）。
    EmptyComponent,
    /// backslash separator を含む。
    BackslashSeparator,
    /// mount 名が不正。
    InvalidMountName,
}

/// routing 後の filesystem 操作対象（仕様第8.2節 `FilesystemTarget`）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemTarget {
    /// mount 名。
    pub mount: MountName,
    /// mount 相対 path。
    pub path: RelativePath,
}

impl FilesystemTarget {
    /// script path を parse する（規範 syntax `@MOUNT/component/...`、仕様第8.2節）。
    ///
    /// unqualified `component/...` は mount 名 `default` へ parse する。mount は完全一致。
    /// 絶対 path・`.`・`..`・空 component・backslash・drive prefix・UNC・NUL を拒否する
    /// （host platform path へ変換する前に検証、CAP-AT-10）。
    pub fn parse(script_path: &str) -> Result<Self, PathError> {
        if script_path.is_empty() {
            return Err(PathError::Empty);
        }
        if script_path.contains('\0') {
            return Err(PathError::ContainsNul);
        }
        if script_path.contains('\\') {
            return Err(PathError::BackslashSeparator);
        }

        // mount と body を分離する。`@MOUNT/...` は qualified、それ以外は `default` mount。
        let (mount_str, body) = if let Some(rest) = script_path.strip_prefix('@') {
            match rest.split_once('/') {
                Some((mount, body)) => (mount, body),
                // `@MOUNT` だけ（body なし）は空 path とみなす。
                None => (rest, ""),
            }
        } else {
            ("default", script_path)
        };

        let mount = MountName::new(mount_str).map_err(|_| PathError::InvalidMountName)?;

        // 絶対 path（先頭 `/`）と Windows drive prefix（`C:`）・UNC（`//` は先頭空 component
        // として EmptyComponent で捕捉される）を排除する。
        if body.starts_with('/') {
            return Err(PathError::Absolute);
        }
        if has_drive_prefix(body) {
            return Err(PathError::Absolute);
        }

        let mut components = Vec::new();
        // body が空（`@mount` だけ、または unqualified の空文字は上で Empty 済み）は
        // root 自身を指す空 component 列とする。それ以外は `/` で分割して各 component を検証。
        if !body.is_empty() {
            for component in body.split('/') {
                match component {
                    "" => return Err(PathError::EmptyComponent),
                    "." => return Err(PathError::DotComponent),
                    ".." => return Err(PathError::ParentComponent),
                    c => components.push(c.to_string()),
                }
            }
        }

        Ok(Self {
            mount,
            path: RelativePath {
                components: components.into(),
            },
        })
    }
}

/// Windows drive prefix（`C:` / `C:/...`）か。UNC はここでは判定せず separator で捕捉する。
fn has_drive_prefix(body: &str) -> bool {
    let bytes = body.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// file open 時の書き込み mode（仕様第8.3節 `WriteMode`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteMode {
    /// 既存内容を切り詰める。
    Truncate,
    /// 末尾へ追記する。
    Append,
}

/// file open request（仕様第8.3節 `OpenFileRequest`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenFileRequest {
    /// 既存 file の読み取り。
    ReadExisting,
    /// 既存 file への書き込み。
    WriteExisting {
        /// 書き込み mode。
        mode: WriteMode,
    },
    /// 新規 file の作成。
    CreateNew {
        /// 書き込み mode。
        mode: WriteMode,
    },
    /// 存在を問わず Write+Create を事前要求する（`write_file`/`append_file`）。
    Upsert {
        /// 書き込み mode。
        mode: WriteMode,
    },
}

/// entry 種別（仕様第8.3節 `EntryKind`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// 通常 file。
    File,
    /// directory。
    Directory,
    /// symlink。
    Symlink,
    /// その他。
    Other,
}

/// 公開メタデータ（仕様第8.3節 `PublicMetadata`）。時刻・owner・absolute path は含めない。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicMetadata {
    /// entry 種別。
    pub kind: EntryKind,
    /// byte サイズ。
    pub size_bytes: u64,
    /// 読み取り専用か。
    pub readonly: bool,
}

/// directory entry（仕様第8.3節 `DirectoryEntry`）。`name` は検証済みの単一 component。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    /// entry 名（path separator を含まない単一 component）。
    pub name: String,
    /// entry 種別。
    pub kind: EntryKind,
}

/// 削除対象種別（仕様第8.3節 `RemoveKind`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoveKind {
    /// file または symlink。
    FileOrSymlink,
    /// 空 directory。
    EmptyDirectory,
    /// directory を再帰削除する（`remove_tree`、REV-021 §17.6）。中間・final symlink は
    /// リンクを辿らずリンク自体を削除する。専用 capability `RecursiveDelete` を要求する。
    Tree,
}

/// `exit()` を structured terminal にする authority（仕様第9節 `ProcessExit`）。
///
/// OS process 終了権限ではなく、execution を `Exited { code }` にする権限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessExit {
    policy_id: NonZeroU128,
}

impl ProcessExit {
    /// policy 相関 ID から作る。
    pub const fn new(policy_id: NonZeroU128) -> Self {
        Self { policy_id }
    }

    /// policy 相関 ID。
    pub const fn policy_id(self) -> NonZeroU128 {
        self.policy_id
    }
}

/// 登録 host function の識別子（仕様第11節 `HostFunctionId`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct HostFunctionId(NonZeroU128);

impl HostFunctionId {
    /// 非ゼロ値から作る。
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    /// 内部の非ゼロ値。
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }
}

/// policy 相関 ID（仕様第3節 `CapabilitySetId`）。
///
/// 認可 token ではなく policy 相関 ID である。同じ policy 内容は同じ ID を持ち、
/// 内容が変われば ID が変わる（CAP-AT-28）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct CapabilitySetId([u8; 32]);

impl CapabilitySetId {
    /// 生の 32 byte 表現を返す。
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// freeze された capability 群（内部保持）。
struct FrozenCapabilities {
    environment: Option<EnvironmentSnapshot>,
    clock: Option<Arc<dyn Clock>>,
    stdin: Option<Arc<dyn Input>>,
    stdout: Option<Arc<dyn Output>>,
    filesystem: Option<FilesystemCapability>,
    process_exit: Option<ProcessExit>,
    module_resolver: Option<Arc<dyn ModuleResolver>>,
    host_functions: BTreeSet<HostFunctionId>,
    id: CapabilitySetId,
}

/// deny-by-default な capability 集合（仕様第3節 `CapabilitySet`）。
///
/// [`empty`](CapabilitySet::empty) が唯一の library 既定値で全操作を拒否する。`clone` は
/// 同じ immutable authority への shallow clone で、set ID と権限は同一。revoke API はない。
#[derive(Clone)]
pub struct CapabilitySet(Arc<FrozenCapabilities>);

impl CapabilitySet {
    /// 全 authority を拒否する空 set。
    pub fn empty() -> Self {
        CapabilitySetBuilder::new().build()
    }

    /// builder を作る。
    pub fn builder() -> CapabilitySetBuilder {
        CapabilitySetBuilder::new()
    }

    /// ambient 互換の既定 set（Phase 2 移行用）。
    ///
    /// alpha facade / CLI / REPL / VM 経路は Phase 2 の CLI profile（C9）が入るまで、
    /// 従来どおりの挙動へ到達できる必要がある。そのため次を grant した set を既定にする。
    ///
    /// - **ProcessExit**（C7）: `exit()` を structured `Exited` terminal にする。
    /// - **Environment**（C3）: `env()` 用の snapshot。process env を 1 度だけ読み、legacy の
    ///   allow-list（`TSUMUGI_ENV_ALLOW`）と `TSUMUGI_` 保護を適用した visible key だけを載せる。
    ///   ambient 経路の唯一の process env 読み取りをこの構築時点へ集約し、`env()` builtin 側は
    ///   snapshot だけを読む（core builtin の ambient read 0）。
    /// - **Clock**（C3）: OS system clock（[`SystemClock`]）。`now()` が使う。
    /// - **Stdin**（C4）: OS 標準入力（[`SystemInput`]）。`input()` が使う。
    /// - **Stdout**（C4）: OS 標準出力（[`SystemOutput`]）。`print` が使う。
    ///
    /// filesystem 等は従来の process-global 経路（sandbox）が引き続き担うため、この set
    /// には載せない（C5/C10 で置換する）。
    ///
    /// deny-by-default の唯一の library 既定値は [`Self::empty`] であり、埋め込み host は
    /// そちらから明示 grant する。本 set は移行期の内部利用に限る。
    pub fn ambient_compat() -> Self {
        // ambient 経路は policy 相関 ID を区別しないため固定の非ゼロ policy_id を使う。
        let policy_id = NonZeroU128::new(1).expect("non-zero");
        CapabilitySetBuilder::new()
            .process_exit(ProcessExit::new(policy_id))
            .expect("single grant never duplicates")
            .environment(crate::builtin_core::ambient_environment_snapshot())
            .expect("single grant never duplicates")
            .clock(Arc::new(SystemClock::new(policy_id)))
            .expect("single grant never duplicates")
            .stdin(Arc::new(SystemInput::new(policy_id)))
            .expect("single grant never duplicates")
            .stdout(Arc::new(SystemOutput::new(policy_id)))
            .expect("single grant never duplicates")
            .build()
    }

    /// policy 相関 ID。
    pub fn id(&self) -> CapabilitySetId {
        self.0.id
    }

    /// 指定 authority を持つか。
    pub fn contains(&self, kind: CapabilityKind) -> bool {
        match kind {
            CapabilityKind::Environment => self.0.environment.is_some(),
            CapabilityKind::Clock => self.0.clock.is_some(),
            CapabilityKind::Stdin => self.0.stdin.is_some(),
            CapabilityKind::Stdout => self.0.stdout.is_some(),
            CapabilityKind::Filesystem => self.0.filesystem.is_some(),
            CapabilityKind::ProcessExit => self.0.process_exit.is_some(),
            CapabilityKind::ModuleResolver => self.0.module_resolver.is_some(),
            CapabilityKind::HostFunction => !self.0.host_functions.is_empty(),
        }
    }

    /// 指定 host function が grant されているか。
    pub fn contains_host_function(&self, id: HostFunctionId) -> bool {
        self.0.host_functions.contains(&id)
    }

    /// 環境変数 snapshot（crate 内部限定。C3 で `env()` が使う）。
    pub(crate) fn environment(&self) -> Option<&EnvironmentSnapshot> {
        self.0.environment.as_ref()
    }

    /// clock authority（crate 内部限定。C3 で `now()` が使う）。
    pub(crate) fn clock(&self) -> Option<&Arc<dyn Clock>> {
        self.0.clock.as_ref()
    }

    /// stdin authority（crate 内部限定。C4 で `input()` が使う）。
    pub(crate) fn stdin(&self) -> Option<&Arc<dyn Input>> {
        self.0.stdin.as_ref()
    }

    /// stdout authority（crate 内部限定。C4 で `print` が使う）。
    pub(crate) fn stdout(&self) -> Option<&Arc<dyn Output>> {
        self.0.stdout.as_ref()
    }

    /// filesystem authority（crate 内部限定。C5-c で fs builtin が consult する）。
    pub(crate) fn filesystem(&self) -> Option<&FilesystemCapability> {
        self.0.filesystem.as_ref()
    }

    /// process exit authority（crate 内部限定。C1 では未配線、C7 で使う）。
    #[allow(dead_code)]
    pub(crate) fn process_exit(&self) -> Option<ProcessExit> {
        self.0.process_exit
    }
}

impl Default for CapabilitySet {
    /// deny-by-default（[`CapabilitySet::empty`]）。
    fn default() -> Self {
        Self::empty()
    }
}

impl std::fmt::Debug for CapabilitySet {
    /// authority の有無と ID だけを出し、adapter 本体や env 値は出さない（secret-free）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilitySet")
            .field("id", &self.0.id)
            .field("environment", &self.0.environment.is_some())
            .field("clock", &self.0.clock.is_some())
            .field("stdin", &self.0.stdin.is_some())
            .field("stdout", &self.0.stdout.is_some())
            .field("filesystem", &self.0.filesystem.is_some())
            .field("process_exit", &self.0.process_exit.is_some())
            .field("module_resolver", &self.0.module_resolver.is_some())
            .field("host_function_count", &self.0.host_functions.len())
            .finish()
    }
}

/// [`CapabilitySet`] を組み立てる mutable builder（仕様第3節）。
///
/// 同 kind 二重設定は [`ConfigError::DuplicateCapability`] で後勝ちにしない。host function
/// grant だけは異なる ID を複数追加でき、同 ID 重複は configuration error。`build` で消費する。
pub struct CapabilitySetBuilder {
    environment: Option<EnvironmentSnapshot>,
    clock: Option<Arc<dyn Clock>>,
    stdin: Option<Arc<dyn Input>>,
    stdout: Option<Arc<dyn Output>>,
    filesystem: Option<FilesystemCapability>,
    process_exit: Option<ProcessExit>,
    module_resolver: Option<Arc<dyn ModuleResolver>>,
    host_functions: BTreeSet<HostFunctionId>,
}

impl CapabilitySetBuilder {
    fn new() -> Self {
        Self {
            environment: None,
            clock: None,
            stdin: None,
            stdout: None,
            filesystem: None,
            process_exit: None,
            module_resolver: None,
            host_functions: BTreeSet::new(),
        }
    }

    /// Environment authority を grant する。二重設定は error。
    pub fn environment(mut self, value: EnvironmentSnapshot) -> Result<Self, ConfigError> {
        set_once(&mut self.environment, value, CapabilityKind::Environment)?;
        Ok(self)
    }

    /// Clock authority を grant する。二重設定は error。
    pub fn clock(mut self, value: Arc<dyn Clock>) -> Result<Self, ConfigError> {
        set_once(&mut self.clock, value, CapabilityKind::Clock)?;
        Ok(self)
    }

    /// Stdin authority を grant する。二重設定は error。
    pub fn stdin(mut self, value: Arc<dyn Input>) -> Result<Self, ConfigError> {
        set_once(&mut self.stdin, value, CapabilityKind::Stdin)?;
        Ok(self)
    }

    /// Stdout authority を grant する。二重設定は error。
    pub fn stdout(mut self, value: Arc<dyn Output>) -> Result<Self, ConfigError> {
        set_once(&mut self.stdout, value, CapabilityKind::Stdout)?;
        Ok(self)
    }

    /// Filesystem authority を grant する。二重設定は error。
    pub fn filesystem(mut self, value: FilesystemCapability) -> Result<Self, ConfigError> {
        set_once(&mut self.filesystem, value, CapabilityKind::Filesystem)?;
        Ok(self)
    }

    /// ProcessExit authority を grant する。二重設定は error。
    pub fn process_exit(mut self, value: ProcessExit) -> Result<Self, ConfigError> {
        set_once(&mut self.process_exit, value, CapabilityKind::ProcessExit)?;
        Ok(self)
    }

    /// ModuleResolver authority を grant する。二重設定は error。
    pub fn module_resolver(mut self, value: Arc<dyn ModuleResolver>) -> Result<Self, ConfigError> {
        set_once(
            &mut self.module_resolver,
            value,
            CapabilityKind::ModuleResolver,
        )?;
        Ok(self)
    }

    /// host function を 1 つ grant する。異なる ID は複数可、同 ID 重複は error。
    pub fn grant_host_function(mut self, id: HostFunctionId) -> Result<Self, ConfigError> {
        if !self.host_functions.insert(id) {
            return Err(ConfigError::DuplicateCapability {
                kind: CapabilityKind::HostFunction,
            });
        }
        Ok(self)
    }

    /// set を freeze して [`CapabilitySet`] を作る。
    pub fn build(self) -> CapabilitySet {
        let id = compute_id(&self);
        CapabilitySet(Arc::new(FrozenCapabilities {
            environment: self.environment,
            clock: self.clock,
            stdin: self.stdin,
            stdout: self.stdout,
            filesystem: self.filesystem,
            process_exit: self.process_exit,
            module_resolver: self.module_resolver,
            host_functions: self.host_functions,
            id,
        }))
    }
}

/// authority slot を 1 度だけ設定する。既に設定済みなら [`ConfigError::DuplicateCapability`]。
fn set_once<T>(slot: &mut Option<T>, value: T, kind: CapabilityKind) -> Result<(), ConfigError> {
    if slot.is_some() {
        return Err(ConfigError::DuplicateCapability { kind });
    }
    *slot = Some(value);
    Ok(())
}

/// §5.1 の `str = u64(len) || UTF-8 bytes` を buffer へ書く。
fn push_str_field(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u64).to_be_bytes());
    buf.extend_from_slice(s.as_bytes());
}

/// [`CapabilitySetId`] を計算する（仕様第3.1・8.6節）。
///
/// domain `TSUMUGI-CAPSET-V1\0`。group は CapabilityKind tag 昇順、環境 key は UTF-8 byte 順、
/// host function ID は 16 byte 値昇順。empty set も同じ domain と `entry_count=0` で hash する。
fn compute_id(b: &CapabilitySetBuilder) -> CapabilitySetId {
    // 存在する CapabilityKind group を tag 昇順で列挙する（host function は複数でも 1 group）。
    let mut groups: Vec<CapabilityKind> = Vec::new();
    if b.environment.is_some() {
        groups.push(CapabilityKind::Environment);
    }
    if b.clock.is_some() {
        groups.push(CapabilityKind::Clock);
    }
    if b.stdin.is_some() {
        groups.push(CapabilityKind::Stdin);
    }
    if b.stdout.is_some() {
        groups.push(CapabilityKind::Stdout);
    }
    if b.filesystem.is_some() {
        groups.push(CapabilityKind::Filesystem);
    }
    if b.process_exit.is_some() {
        groups.push(CapabilityKind::ProcessExit);
    }
    if b.module_resolver.is_some() {
        groups.push(CapabilityKind::ModuleResolver);
    }
    if !b.host_functions.is_empty() {
        groups.push(CapabilityKind::HostFunction);
    }
    groups.sort_by_key(|k| k.tag());

    let mut buf = Vec::new();
    buf.extend_from_slice(b"TSUMUGI-CAPSET-V1\0");
    buf.extend_from_slice(&(groups.len() as u64).to_be_bytes());
    for kind in groups {
        buf.push(kind.tag());
        match kind {
            CapabilityKind::Environment => {
                let env = b.environment.as_ref().expect("group present");
                let entries: Vec<(&str, &EnvironmentValue)> =
                    env.0.iter().map(|(k, v)| (k.as_str(), v)).collect();
                buf.extend_from_slice(&(entries.len() as u64).to_be_bytes());
                for (key, value) in entries {
                    push_str_field(&mut buf, key);
                    buf.push(value.classification().tag());
                }
            }
            CapabilityKind::Clock => push_policy_id(
                &mut buf,
                b.clock.as_ref().expect("group present").policy_id(),
            ),
            CapabilityKind::Stdin => push_policy_id(
                &mut buf,
                b.stdin.as_ref().expect("group present").policy_id(),
            ),
            CapabilityKind::Stdout => push_policy_id(
                &mut buf,
                b.stdout.as_ref().expect("group present").policy_id(),
            ),
            CapabilityKind::Filesystem => {
                encode_filesystem(&mut buf, b.filesystem.as_ref().expect("group present"));
            }
            CapabilityKind::ProcessExit => {
                push_policy_id(&mut buf, b.process_exit.expect("group present").policy_id())
            }
            CapabilityKind::ModuleResolver => push_policy_id(
                &mut buf,
                b.module_resolver
                    .as_ref()
                    .expect("group present")
                    .policy_id(),
            ),
            CapabilityKind::HostFunction => {
                // BTreeSet<HostFunctionId> は 16 byte 値昇順（NonZeroU128 の Ord）で走査する。
                buf.extend_from_slice(&(b.host_functions.len() as u64).to_be_bytes());
                for id in &b.host_functions {
                    buf.extend_from_slice(&id.get().get().to_be_bytes());
                }
            }
        }
    }

    CapabilitySetId(crate::embedding::hash::sha256(&buf))
}

/// policy ID を 16 byte big-endian で書く（仕様第3.1節）。
fn push_policy_id(buf: &mut Vec<u8>, id: NonZeroU128) {
    buf.extend_from_slice(&id.get().to_be_bytes());
}

/// filesystem policy を encode する（仕様第8.6節）。mount UTF-8 byte 順。
fn encode_filesystem(buf: &mut Vec<u8>, fs: &FilesystemCapability) {
    // roots を mount 名昇順に並べ替えて encode する（登録順に依存しない）。
    let mut roots: Vec<&FilesystemRoot> = fs.roots().collect();
    roots.sort_by(|a, b| a.mount.as_str().cmp(b.mount.as_str()));
    buf.extend_from_slice(&(roots.len() as u64).to_be_bytes());
    for root in roots {
        push_str_field(buf, root.mount.as_str());
        buf.extend_from_slice(&root.policy_id.get().to_be_bytes());
        buf.push(root.symlink_policy.tag());
        // operations は BTreeSet なので FsOperation の Ord（宣言順）昇順。tag 昇順と一致する。
        buf.extend_from_slice(&(root.operations.len() as u64).to_be_bytes());
        for op in &root.operations {
            buf.push(op.tag());
        }
    }
}

// ---------------------------------------------------------------------------
// C5-b: secure OS adapter（仕様第8.3節 契約1〜7）
// ---------------------------------------------------------------------------
//
// OS filesystem を backing にする [`DirectoryHandle`] / [`FileHandle`] 実装。root
// directory を 1 つの base path へ bind し、script が指定する mount 相対 [`RelativePath`]
// を **component ごとに** 解決する。各中間 component は `symlink_metadata`（lstat 相当）で
// 種別を確認してから降り、[`SymlinkPolicy`] に従って symlink を拒否/拘束する。final entry の
// open は `O_NOFOLLOW` を付けて symlink を追従しない（契約4・5）。
//
// # platform サポート（契約2・3）
//
// secure な component 解決は Unix でだけ提供する。`O_NOFOLLOW` と lstat による symlink 検出は
// Unix `std::os::unix` surface（外部 crate 非依存）で表現できる。root 拘束・symlink policy を
// 同じ保証で満たせない platform（非 Unix）では、文字列 prefix check へ fallback せず
// [`AdapterError::SecureResolutionUnsupported`] を返して fail closed する（契約3）。これは
// script filesystem 操作では code `secure_resolution_unsupported` の canonical `host` error
// （catch 可能）へ写る（C5-c で配線）。
//
// # 本スライスの範囲
//
// TOCTOU race（component 解決中に symlink が差し替わる）は本 slice の対象外で、AUD-020
// （path-handle TOCTOU、P2）と CAP-AT-11 の stress gate で扱う。ここでは lstat→種別判定→
// `O_NOFOLLOW` open という契約1（canonicalize せず handle/path を bind）の形を確立する。

/// OS filesystem を backing にする secure な [`DirectoryHandle`]（仕様第8.3節）。
///
/// [`OsDirectoryHandle::new`] で root base path・policy 相関 ID・[`SymlinkPolicy`] を束ね、
/// 以降の操作は root 相対 [`RelativePath`] だけを受け取る。base path 自体の外へ出る解決は
/// symlink policy と component 検証で防ぐ。
pub struct OsDirectoryHandle {
    /// root directory の絶対 path（host が open 前に確定する）。
    base: std::path::PathBuf,
    policy_id: NonZeroU128,
    symlink_policy: SymlinkPolicy,
}

impl OsDirectoryHandle {
    /// root base path・policy 相関 ID・symlink policy から作る。
    ///
    /// `base` は host が execution 作成前に用意する root directory の path。adapter は
    /// この path より外を解決しない。
    pub fn new(
        base: impl Into<std::path::PathBuf>,
        policy_id: NonZeroU128,
        symlink_policy: SymlinkPolicy,
    ) -> Self {
        Self {
            base: base.into(),
            policy_id,
            symlink_policy,
        }
    }
}

impl std::fmt::Debug for OsDirectoryHandle {
    /// host path を出さない（存在 oracle / 情報漏洩防止、仕様第8.5節）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OsDirectoryHandle")
            .field("policy_id", &self.policy_id)
            .field("symlink_policy", &self.symlink_policy)
            .finish_non_exhaustive()
    }
}

/// `std::fs::Metadata` の種別を公開 [`EntryKind`] へ写す。
fn entry_kind_of(meta: &std::fs::Metadata) -> EntryKind {
    let ft = meta.file_type();
    if ft.is_file() {
        EntryKind::File
    } else if ft.is_dir() {
        EntryKind::Directory
    } else if ft.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }
}

/// `std::fs::Metadata` から secret-free な [`PublicMetadata`] を作る（時刻・owner・path 非公開）。
fn public_metadata_of(meta: &std::fs::Metadata) -> PublicMetadata {
    PublicMetadata {
        kind: entry_kind_of(meta),
        size_bytes: meta.len(),
        readonly: meta.permissions().readonly(),
    }
}

/// host I/O error を [`AdapterError::Host`] へ写す。message は OS 由来の文字列のみで、
/// absolute host path・symlink・permission の詳細は含めない（呼び出し側が sanitize 済み前提）。
fn host_err(context: &str) -> AdapterError {
    AdapterError::Host(context.to_string())
}

#[cfg(unix)]
mod os_secure {
    //! Unix 上の secure component 解決（契約1〜5）。
    //!
    //! `symlink_metadata`（lstat）で各 component の種別を確認し、[`SymlinkPolicy`] に従って
    //! 中間/final symlink を拒否または拘束する。final entry は open 前に lstat して policy を
    //! 適用する（`O_NOFOLLOW` の flag 値は Linux で arch 依存＝libc 非依存では確定できないため、
    //! flag ではなく明示 lstat で symlink を検出する。TOCTOU race は AUD-020 / CAP-AT-11 stress
    //! gate で扱う）。

    use super::{
        AdapterError, OpenFileRequest, OsDirectoryHandle, RelativePath, SymlinkPolicy, WriteMode,
        entry_kind_of, host_err,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    /// 解決結果の host path と、その parent directory（rename/remove 等が使う）。
    pub(super) struct Resolved {
        /// final entry の host path（base 配下）。
        pub path: PathBuf,
    }

    /// root base から `rel` を component ごとに secure に解決する。
    ///
    /// - 各 **中間** component を lstat し、directory でなければ拒否。symlink は policy に従う
    ///   （`DenyAll` は拒否、`FollowWithinRoot`/`OperateOnFinalEntry` は解決先を root 内へ拘束）。
    /// - final component は lstat せず path を組み立てて返す（open/metadata 側が lstat で
    ///   symlink を検出し policy を適用する）。空 component 列（root 自身）は base を返す。
    pub(super) fn resolve(
        handle: &OsDirectoryHandle,
        rel: &RelativePath,
    ) -> Result<Resolved, AdapterError> {
        let policy = handle.symlink_policy;
        let mut current = handle.base.clone();
        let comps: Vec<&str> = rel.components().collect();
        // 最後の 1 つを除く中間 component を解決する。
        let intermediate = comps.len().saturating_sub(1);
        for name in comps.iter().take(intermediate) {
            current.push(name);
            let meta = fs::symlink_metadata(&current)
                .map_err(|_| host_err("intermediate component not accessible"))?;
            let ft = meta.file_type();
            if ft.is_symlink() {
                match policy {
                    SymlinkPolicy::DenyAll => {
                        return Err(host_err("symlink component denied by policy"));
                    }
                    SymlinkPolicy::FollowWithinRoot | SymlinkPolicy::OperateOnFinalEntry => {
                        // 中間 symlink は解決先を root 内へ拘束する。canonicalize の結果が
                        // base 配下に収まることだけを確認し（契約4）、収まらなければ拒否。
                        let resolved = fs::canonicalize(&current)
                            .map_err(|_| host_err("symlink target not resolvable"))?;
                        if !within_base(&handle.base, &resolved) {
                            return Err(host_err("symlink escapes root"));
                        }
                        if !resolved.is_dir() {
                            return Err(host_err("intermediate component is not a directory"));
                        }
                        current = resolved;
                    }
                }
            } else if !ft.is_dir() {
                return Err(host_err("intermediate component is not a directory"));
            }
        }
        if let Some(last) = comps.last() {
            current.push(last);
        }
        Ok(Resolved { path: current })
    }

    /// `candidate` が `base` 配下（base 自身を含む）か。両者とも canonical 前提。
    pub(super) fn within_base(base: &Path, candidate: &Path) -> bool {
        // base を canonicalize してから比較する（base は既存 directory 前提）。
        let base = match fs::canonicalize(base) {
            Ok(b) => b,
            Err(_) => return false,
        };
        candidate.starts_with(&base)
    }

    /// final entry を lstat して symlink policy を適用したうえで open する。
    ///
    /// Read/Write/Create は final symlink を拒否する（契約4）。open 前に final entry を lstat
    /// し、symlink なら追従せず拒否する（契約5、dangling final symlink も追従しない）。
    pub(super) fn open_file(
        handle: &OsDirectoryHandle,
        rel: &RelativePath,
        request: OpenFileRequest,
    ) -> Result<fs::File, AdapterError> {
        let resolved = resolve(handle, rel)?;
        let path = resolved.path;

        // final entry が既存 symlink なら Read/Write/Create すべて拒否する（契約4・5）。
        // 存在しない場合（CreateNew/Upsert の新規作成）は symlink 検出不要。
        if let Ok(lmeta) = fs::symlink_metadata(&path)
            && lmeta.file_type().is_symlink()
        {
            return Err(host_err("final symlink denied by policy"));
        }

        let mut opts = fs::OpenOptions::new();
        match request {
            OpenFileRequest::ReadExisting => {
                opts.read(true);
            }
            OpenFileRequest::WriteExisting { mode } => {
                apply_write_mode(&mut opts, mode);
            }
            OpenFileRequest::CreateNew { mode } => {
                opts.create_new(true);
                apply_write_mode(&mut opts, mode);
            }
            OpenFileRequest::Upsert { mode } => {
                opts.create(true);
                apply_write_mode(&mut opts, mode);
            }
        }
        opts.open(&path).map_err(|_| host_err("open failed"))
    }

    /// [`WriteMode`] を `OpenOptions` へ適用する。
    fn apply_write_mode(opts: &mut fs::OpenOptions, mode: WriteMode) {
        match mode {
            WriteMode::Truncate => {
                opts.write(true).truncate(true);
            }
            WriteMode::Append => {
                opts.append(true);
            }
        }
    }

    /// final entry の [`super::PublicMetadata`] を取得する。
    ///
    /// `follow_final=false` は lstat（symlink 自体）、`true` は stat（追従）を使う。ただし
    /// `follow_final=true` で symlink を追従する場合、追従先が root 内に留まることを確認する。
    pub(super) fn metadata(
        handle: &OsDirectoryHandle,
        rel: &RelativePath,
        follow_final: bool,
    ) -> Result<super::PublicMetadata, AdapterError> {
        let resolved = resolve(handle, rel)?;
        let path = resolved.path;
        let lmeta =
            fs::symlink_metadata(&path).map_err(|_| host_err("metadata target not accessible"))?;
        if follow_final && lmeta.file_type().is_symlink() {
            // 追従する場合は解決先が root 内であることを確認する（契約4）。
            let resolved =
                fs::canonicalize(&path).map_err(|_| host_err("symlink target not resolvable"))?;
            if !within_base(&handle.base, &resolved) {
                return Err(host_err("symlink escapes root"));
            }
            let meta =
                fs::metadata(&resolved).map_err(|_| host_err("metadata target not accessible"))?;
            Ok(super::public_metadata_of(&meta))
        } else {
            Ok(super::public_metadata_of(&lmeta))
        }
    }

    /// directory を列挙する。各 entry を lstat して [`EntryKind`] を確定する。
    pub(super) fn list(
        handle: &OsDirectoryHandle,
        rel: &RelativePath,
        max_entries: Option<std::num::NonZeroU64>,
    ) -> Result<Vec<super::DirectoryEntry>, AdapterError> {
        let resolved = resolve(handle, rel)?;
        let path = resolved.path;
        // final entry が symlink の場合は List が拒否する（契約4）。
        let lmeta =
            fs::symlink_metadata(&path).map_err(|_| host_err("list target not accessible"))?;
        if lmeta.file_type().is_symlink() {
            return Err(host_err("list on symlink denied by policy"));
        }
        let mut out = Vec::new();
        let iter = fs::read_dir(&path).map_err(|_| host_err("read_dir failed"))?;
        for entry in iter {
            // REV-009: 個別 entry 取得失敗を黙殺しない（safe profile は directory_read へ写す）。
            let entry = entry.map_err(|_| AdapterError::DirectoryReadFailed)?;
            // REV-009: 非 UTF-8 名を lossy 変換で同一視しない（safe profile は invalid_encoding）。
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| AdapterError::NonUtf8EntryName)?;
            let meta = entry
                .path()
                .symlink_metadata()
                .map_err(|_| AdapterError::DirectoryReadFailed)?;
            out.push(super::DirectoryEntry {
                name,
                kind: entry_kind_of(&meta),
            });
            if let Some(max) = max_entries
                && out.len() as u64 >= max.get()
            {
                break;
            }
        }
        Ok(out)
    }

    /// directory を作る（親は既存前提、`Create`）。
    pub(super) fn create_dir(
        handle: &OsDirectoryHandle,
        rel: &RelativePath,
    ) -> Result<(), AdapterError> {
        let resolved = resolve(handle, rel)?;
        fs::create_dir(&resolved.path).map_err(|_| host_err("create_dir failed"))
    }

    /// entry を削除する。`FileOrSymlink` は file/symlink、`EmptyDirectory` は空 dir、
    /// `Tree` は directory の再帰削除（REV-021 §17.6）。
    pub(super) fn remove(
        handle: &OsDirectoryHandle,
        rel: &RelativePath,
        kind: super::RemoveKind,
    ) -> Result<(), AdapterError> {
        let resolved = resolve(handle, rel)?;
        let path = resolved.path;
        match kind {
            super::RemoveKind::FileOrSymlink => {
                // final symlink 自体を消す（追従しない）。DenyAll でも「entry の削除」は
                // 対象 file の read/write を伴わないため許可する（契約4 の delete 例外）。
                fs::remove_file(&path).map_err(|_| host_err("remove failed"))
            }
            super::RemoveKind::EmptyDirectory => {
                // 空 directory のみ削除する（非空は OS error → host error）。
                fs::remove_dir(&path).map_err(|_| host_err("remove_dir failed"))
            }
            super::RemoveKind::Tree => {
                // 再帰削除（REV-021）。final entry が symlink なら追従せずリンク自体を消す。
                // `remove_dir_all` は directory 内の symlink もリンクとして扱い、リンク先を
                // 辿って削除しない。
                let lmeta = fs::symlink_metadata(&path)
                    .map_err(|_| host_err("remove_tree target not accessible"))?;
                if lmeta.file_type().is_symlink() {
                    return fs::remove_file(&path).map_err(|_| host_err("remove_tree failed"));
                }
                fs::remove_dir_all(&path).map_err(|_| host_err("remove_tree failed"))
            }
        }
    }

    /// entry を rename する。両 handle 認可後の単一 rename call（契約6）。
    ///
    /// destination handle も [`OsDirectoryHandle`] であることを要求する（異種 backing 間の
    /// rename は本 slice では非対応で `SecureResolutionUnsupported`）。
    pub(super) fn rename(
        from_handle: &OsDirectoryHandle,
        from: &RelativePath,
        to_handle: &OsDirectoryHandle,
        to: &RelativePath,
        replace: bool,
    ) -> Result<(), AdapterError> {
        let src = resolve(from_handle, from)?.path;
        let dst = resolve(to_handle, to)?.path;
        if !replace && dst.exists() {
            return Err(host_err("destination exists"));
        }
        fs::rename(&src, &dst).map_err(|_| host_err("rename failed"))
    }
}

impl DirectoryHandle for OsDirectoryHandle {
    fn policy_id(&self) -> NonZeroU128 {
        self.policy_id
    }

    fn symlink_policy(&self) -> SymlinkPolicy {
        self.symlink_policy
    }

    #[cfg(unix)]
    fn open_file(
        &self,
        path: &RelativePath,
        request: OpenFileRequest,
    ) -> Result<Box<dyn FileHandle>, AdapterError> {
        let file = os_secure::open_file(self, path, request)?;
        Ok(Box::new(OsFileHandle { file }))
    }

    #[cfg(unix)]
    fn create_dir(&self, path: &RelativePath) -> Result<(), AdapterError> {
        os_secure::create_dir(self, path)
    }

    #[cfg(unix)]
    fn metadata(
        &self,
        path: &RelativePath,
        follow_final: bool,
    ) -> Result<PublicMetadata, AdapterError> {
        os_secure::metadata(self, path, follow_final)
    }

    #[cfg(unix)]
    fn list(
        &self,
        path: &RelativePath,
        max_entries: Option<NonZeroU64>,
    ) -> Result<Vec<DirectoryEntry>, AdapterError> {
        os_secure::list(self, path, max_entries)
    }

    #[cfg(unix)]
    fn remove(&self, path: &RelativePath, kind: RemoveKind) -> Result<(), AdapterError> {
        os_secure::remove(self, path, kind)
    }

    #[cfg(unix)]
    fn rename(
        &self,
        from: &RelativePath,
        to_directory: &dyn DirectoryHandle,
        to: &RelativePath,
        replace: bool,
    ) -> Result<(), AdapterError> {
        // 本 slice では同一 backing root（同 policy 相関 ID）内の rename だけを扱う。trait
        // object から具象型へ downcast できないため、destination handle が同じ policy ID を
        // 持つ場合に限り self の base を両 path に使う。mount を越える rename（別 root handle）
        // は C5-c で mount routing を配線する際に拡張する。それまでは fail closed（契約6）。
        if to_directory.policy_id() != self.policy_id {
            return Err(AdapterError::SecureResolutionUnsupported);
        }
        os_secure::rename(self, from, self, to, replace)
    }

    // --- 非 Unix: secure resolution 非対応（fail closed、契約3）---

    #[cfg(not(unix))]
    fn open_file(
        &self,
        _path: &RelativePath,
        _request: OpenFileRequest,
    ) -> Result<Box<dyn FileHandle>, AdapterError> {
        Err(AdapterError::SecureResolutionUnsupported)
    }

    #[cfg(not(unix))]
    fn create_dir(&self, _path: &RelativePath) -> Result<(), AdapterError> {
        Err(AdapterError::SecureResolutionUnsupported)
    }

    #[cfg(not(unix))]
    fn metadata(
        &self,
        _path: &RelativePath,
        _follow_final: bool,
    ) -> Result<PublicMetadata, AdapterError> {
        Err(AdapterError::SecureResolutionUnsupported)
    }

    #[cfg(not(unix))]
    fn list(
        &self,
        _path: &RelativePath,
        _max_entries: Option<NonZeroU64>,
    ) -> Result<Vec<DirectoryEntry>, AdapterError> {
        Err(AdapterError::SecureResolutionUnsupported)
    }

    #[cfg(not(unix))]
    fn remove(&self, _path: &RelativePath, _kind: RemoveKind) -> Result<(), AdapterError> {
        Err(AdapterError::SecureResolutionUnsupported)
    }

    #[cfg(not(unix))]
    fn rename(
        &self,
        _from: &RelativePath,
        _to_directory: &dyn DirectoryHandle,
        _to: &RelativePath,
        _replace: bool,
    ) -> Result<(), AdapterError> {
        Err(AdapterError::SecureResolutionUnsupported)
    }
}

/// OS file を backing にする [`FileHandle`]（Unix）。open は [`OsDirectoryHandle`] が行う。
#[cfg(unix)]
pub struct OsFileHandle {
    file: std::fs::File,
}

#[cfg(unix)]
impl FileHandle for OsFileHandle {
    fn read_to_end(&mut self, max_bytes: Option<NonZeroU64>) -> Result<Vec<u8>, AdapterError> {
        use std::io::Read;
        let mut buf = Vec::new();
        match max_bytes {
            // Phase 3 で有限 meter を配線する。C5-b では上限が指定された場合のみ
            // その byte 数まで読む（超過検出は Phase 3 の budget 経路で行う）。
            Some(max) => {
                let limit = max.get();
                let mut limited = (&self.file).take(limit);
                limited
                    .read_to_end(&mut buf)
                    .map_err(|_| host_err("read failed"))?;
            }
            None => {
                self.file
                    .read_to_end(&mut buf)
                    .map_err(|_| host_err("read failed"))?;
            }
        }
        Ok(buf)
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), AdapterError> {
        use std::io::Write;
        self.file
            .write_all(bytes)
            .map_err(|_| host_err("write failed"))
    }

    fn metadata(&self) -> Result<PublicMetadata, AdapterError> {
        let meta = self
            .file
            .metadata()
            .map_err(|_| host_err("metadata not accessible"))?;
        Ok(public_metadata_of(&meta))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用 policy_id を作る。
    fn pid(n: u128) -> NonZeroU128 {
        NonZeroU128::new(n).expect("non-zero")
    }

    struct FakeClock(NonZeroU128);
    impl Clock for FakeClock {
        fn policy_id(&self) -> NonZeroU128 {
            self.0
        }
        fn now_utc(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }
    }

    struct FakeInput(NonZeroU128);
    impl Input for FakeInput {
        fn policy_id(&self) -> NonZeroU128 {
            self.0
        }
        fn read_line(&self) -> Result<InputLine, AdapterError> {
            Ok(InputLine::Eof)
        }
    }

    struct FakeOutput(NonZeroU128);
    impl Output for FakeOutput {
        fn policy_id(&self) -> NonZeroU128 {
            self.0
        }
        fn write_all(&self, _bytes: &[u8]) -> Result<(), AdapterError> {
            Ok(())
        }
        fn flush(&self) -> Result<(), AdapterError> {
            Ok(())
        }
    }

    struct FakeResolver(NonZeroU128);
    impl ModuleResolver for FakeResolver {
        fn policy_id(&self) -> NonZeroU128 {
            self.0
        }
    }

    struct FakeDir(NonZeroU128, SymlinkPolicy);
    impl DirectoryHandle for FakeDir {
        fn policy_id(&self) -> NonZeroU128 {
            self.0
        }
        fn symlink_policy(&self) -> SymlinkPolicy {
            self.1
        }
        // ID 計算・検証テスト専用の double。実操作は使わない（C5-b の OS adapter が本実装）。
        fn open_file(
            &self,
            _path: &RelativePath,
            _request: OpenFileRequest,
        ) -> Result<Box<dyn FileHandle>, AdapterError> {
            Err(AdapterError::SecureResolutionUnsupported)
        }
        fn create_dir(&self, _path: &RelativePath) -> Result<(), AdapterError> {
            Err(AdapterError::SecureResolutionUnsupported)
        }
        fn metadata(
            &self,
            _path: &RelativePath,
            _follow_final: bool,
        ) -> Result<PublicMetadata, AdapterError> {
            Err(AdapterError::SecureResolutionUnsupported)
        }
        fn list(
            &self,
            _path: &RelativePath,
            _max_entries: Option<NonZeroU64>,
        ) -> Result<Vec<DirectoryEntry>, AdapterError> {
            Err(AdapterError::SecureResolutionUnsupported)
        }
        fn remove(&self, _path: &RelativePath, _kind: RemoveKind) -> Result<(), AdapterError> {
            Err(AdapterError::SecureResolutionUnsupported)
        }
        fn rename(
            &self,
            _from: &RelativePath,
            _to_directory: &dyn DirectoryHandle,
            _to: &RelativePath,
            _replace: bool,
        ) -> Result<(), AdapterError> {
            Err(AdapterError::SecureResolutionUnsupported)
        }
    }

    fn all_kinds() -> [CapabilityKind; 8] {
        [
            CapabilityKind::Environment,
            CapabilityKind::Clock,
            CapabilityKind::Stdin,
            CapabilityKind::Stdout,
            CapabilityKind::Filesystem,
            CapabilityKind::ProcessExit,
            CapabilityKind::ModuleResolver,
            CapabilityKind::HostFunction,
        ]
    }

    #[test]
    fn empty_denies_all_authorities() {
        // CAP-AT-01: empty set は全 8 authority を拒否する。
        let set = CapabilitySet::empty();
        for kind in all_kinds() {
            assert!(!set.contains(kind), "empty must deny {kind:?}");
        }
        assert!(!set.contains_host_function(HostFunctionId::new(pid(1))));
    }

    #[test]
    fn single_grant_isolated() {
        // CAP-AT-02: grant した 1 authority だけ true、隣接は false。
        let set = CapabilitySet::builder()
            .clock(Arc::new(FakeClock(pid(7))))
            .expect("grant clock")
            .build();
        assert!(set.contains(CapabilityKind::Clock));
        for kind in all_kinds() {
            if kind != CapabilityKind::Clock {
                assert!(
                    !set.contains(kind),
                    "only Clock should be granted, got {kind:?}"
                );
            }
        }
    }

    #[test]
    fn host_function_grant_multiple_distinct() {
        let set = CapabilitySet::builder()
            .grant_host_function(HostFunctionId::new(pid(10)))
            .expect("grant 10")
            .grant_host_function(HostFunctionId::new(pid(20)))
            .expect("grant 20")
            .build();
        assert!(set.contains(CapabilityKind::HostFunction));
        assert!(set.contains_host_function(HostFunctionId::new(pid(10))));
        assert!(set.contains_host_function(HostFunctionId::new(pid(20))));
        assert!(!set.contains_host_function(HostFunctionId::new(pid(30))));
    }

    #[test]
    fn duplicate_same_kind_is_error() {
        let builder = CapabilitySet::builder()
            .clock(Arc::new(FakeClock(pid(1))))
            .expect("first clock");
        match builder.clock(Arc::new(FakeClock(pid(2)))) {
            Err(ConfigError::DuplicateCapability {
                kind: CapabilityKind::Clock,
            }) => {}
            Ok(_) => panic!("second clock must fail"),
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn duplicate_host_function_id_is_error() {
        let builder = CapabilitySet::builder()
            .grant_host_function(HostFunctionId::new(pid(5)))
            .expect("first");
        match builder.grant_host_function(HostFunctionId::new(pid(5))) {
            Err(ConfigError::DuplicateCapability {
                kind: CapabilityKind::HostFunction,
            }) => {}
            Ok(_) => panic!("duplicate id must fail"),
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn clone_has_equal_id_and_permissions() {
        // CAP-AT-03: clone は set ID / 権限が同一。
        let set = CapabilitySet::builder()
            .stdout(Arc::new(FakeOutput(pid(3))))
            .expect("grant stdout")
            .build();
        let cloned = set.clone();
        assert_eq!(set.id(), cloned.id());
        for kind in all_kinds() {
            assert_eq!(set.contains(kind), cloned.contains(kind));
        }
    }

    #[test]
    fn id_is_deterministic_and_content_addressed() {
        // 同じ内容は同じ ID、内容が変われば ID が変わる（CAP-AT-28 の趣旨）。
        let a = CapabilitySet::builder()
            .process_exit(ProcessExit::new(pid(9)))
            .expect("exit")
            .build();
        let b = CapabilitySet::builder()
            .process_exit(ProcessExit::new(pid(9)))
            .expect("exit")
            .build();
        let c = CapabilitySet::builder()
            .process_exit(ProcessExit::new(pid(10)))
            .expect("exit")
            .build();
        assert_eq!(a.id(), b.id());
        assert_ne!(a.id(), c.id());
    }

    #[test]
    fn empty_id_golden_bytes() {
        // CAP-AT-28: empty set の CapabilitySetId を golden bytes として固定する。
        // domain "TSUMUGI-CAPSET-V1\0" || u64(0) の SHA-256。実装が encoding を変えると壊れる。
        let set = CapabilitySet::empty();
        assert_eq!(set.id().as_bytes(), &EMPTY_SET_ID_GOLDEN);
        // encoding の自己整合性も確認する。
        assert_eq!(set.id().as_bytes(), &super_expected_empty());
    }

    #[test]
    fn stdin_grant_isolated() {
        // stdin だけを grant し、FakeInput を配線する。
        let set = CapabilitySet::builder()
            .stdin(Arc::new(FakeInput(pid(11))))
            .expect("grant stdin")
            .build();
        assert!(set.contains(CapabilityKind::Stdin));
        assert!(!set.contains(CapabilityKind::Stdout));
        assert_ne!(set.id(), CapabilitySet::empty().id());
    }

    #[test]
    fn resolver_grant_changes_id() {
        // ModuleResolver group が ID へ寄与し、FakeResolver も配線されることを確認する。
        let set = CapabilitySet::builder()
            .module_resolver(Arc::new(FakeResolver(pid(42))))
            .expect("resolver")
            .build();
        assert!(set.contains(CapabilityKind::ModuleResolver));
        assert_ne!(set.id(), CapabilitySet::empty().id());
        // policy_id が違えば ID も違う。
        let other = CapabilitySet::builder()
            .module_resolver(Arc::new(FakeResolver(pid(43))))
            .expect("resolver")
            .build();
        assert_ne!(set.id(), other.id());
    }

    /// empty set の CapabilitySetId golden bytes（CAP-AT-28）。
    ///
    /// domain `TSUMUGI-CAPSET-V1\0` に entry_count 0 を連結した SHA-256。encoding を
    /// 変更するとこの値が壊れる。
    const EMPTY_SET_ID_GOLDEN: [u8; 32] = [
        92, 117, 206, 78, 163, 208, 129, 118, 222, 53, 113, 138, 188, 116, 81, 141, 17, 230, 13,
        234, 29, 107, 99, 75, 137, 57, 71, 229, 46, 87, 112, 15,
    ];

    #[test]
    fn filesystem_root_validates_adapter_policy() {
        let mount = MountName::new("default").expect("mount");
        let mut ops = BTreeSet::new();
        ops.insert(FsOperation::Read);
        // adapter policy_id 不一致は error。
        let err = FilesystemRoot::new(
            mount.clone(),
            pid(1),
            ops.clone(),
            SymlinkPolicy::DenyAll,
            Arc::new(FakeDir(pid(2), SymlinkPolicy::DenyAll)),
        )
        .expect_err("policy id mismatch");
        assert_eq!(
            err,
            ConfigError::InvalidFilesystemPolicy {
                code: "adapter_policy_id_mismatch"
            }
        );
        // 一致すれば OK。
        FilesystemRoot::new(
            mount,
            pid(1),
            ops,
            SymlinkPolicy::DenyAll,
            Arc::new(FakeDir(pid(1), SymlinkPolicy::DenyAll)),
        )
        .expect("valid root");
    }

    #[test]
    fn empty_operations_rejected() {
        let mount = MountName::new("default").expect("mount");
        let err = FilesystemRoot::new(
            mount,
            pid(1),
            BTreeSet::new(),
            SymlinkPolicy::DenyAll,
            Arc::new(FakeDir(pid(1), SymlinkPolicy::DenyAll)),
        )
        .expect_err("empty ops");
        assert_eq!(
            err,
            ConfigError::InvalidFilesystemPolicy {
                code: "empty_operations"
            }
        );
    }

    #[test]
    fn duplicate_mount_rejected() {
        let mk_root = |name: &str| {
            let mut ops = BTreeSet::new();
            ops.insert(FsOperation::Read);
            FilesystemRoot::new(
                MountName::new(name).expect("mount"),
                pid(1),
                ops,
                SymlinkPolicy::DenyAll,
                Arc::new(FakeDir(pid(1), SymlinkPolicy::DenyAll)),
            )
            .expect("root")
        };
        let err = FilesystemCapability::new([mk_root("default"), mk_root("default")])
            .expect_err("dup mount");
        assert_eq!(
            err,
            ConfigError::InvalidFilesystemPolicy {
                code: "duplicate_mount"
            }
        );
    }

    #[test]
    fn mount_name_grammar() {
        assert!(MountName::new("default").is_ok());
        assert!(MountName::new("a1_-").is_ok());
        assert!(MountName::new("").is_err());
        assert!(MountName::new("1abc").is_err());
        assert!(MountName::new("has space").is_err());
        assert!(MountName::new("a".repeat(33)).is_err());
    }

    #[test]
    fn filesystem_id_independent_of_root_order() {
        let mk = |name: &str, id: u128| {
            let mut ops = BTreeSet::new();
            ops.insert(FsOperation::Read);
            FilesystemRoot::new(
                MountName::new(name).expect("mount"),
                pid(id),
                ops,
                SymlinkPolicy::DenyAll,
                Arc::new(FakeDir(pid(id), SymlinkPolicy::DenyAll)),
            )
            .expect("root")
        };
        let fs1 = FilesystemCapability::new([mk("alpha", 1), mk("beta", 2)]).expect("fs");
        let fs2 = FilesystemCapability::new([mk("beta", 2), mk("alpha", 1)]).expect("fs");
        let id1 = CapabilitySet::builder()
            .filesystem(fs1)
            .expect("fs")
            .build()
            .id();
        let id2 = CapabilitySet::builder()
            .filesystem(fs2)
            .expect("fs")
            .build()
            .id();
        assert_eq!(id1, id2, "root order must not affect id");
    }

    // --- C5-a: FilesystemTarget::parse（mount routing / lexical path、CAP-AT-10/29）---

    #[test]
    fn fs_target_parse_qualified_mount() {
        let t = FilesystemTarget::parse("@data/dir/file.txt").expect("parse");
        assert_eq!(t.mount.as_str(), "data");
        let comps: Vec<&str> = t.path.components().collect();
        assert_eq!(comps, vec!["dir", "file.txt"]);
    }

    #[test]
    fn fs_target_parse_unqualified_uses_default_mount() {
        let t = FilesystemTarget::parse("dir/file.txt").expect("parse");
        assert_eq!(t.mount.as_str(), "default");
        let comps: Vec<&str> = t.path.components().collect();
        assert_eq!(comps, vec!["dir", "file.txt"]);
    }

    #[test]
    fn fs_target_parse_single_component() {
        let t = FilesystemTarget::parse("file.txt").expect("parse");
        assert_eq!(t.mount.as_str(), "default");
        let comps: Vec<&str> = t.path.components().collect();
        assert_eq!(comps, vec!["file.txt"]);
    }

    #[test]
    fn fs_target_parse_qualified_empty_body_is_empty_path() {
        // `@mount` だけは body なし → 空 component 列（root 自身）。
        let t = FilesystemTarget::parse("@data").expect("parse");
        assert_eq!(t.mount.as_str(), "data");
        assert_eq!(t.path.components().count(), 0);
    }

    #[test]
    fn fs_target_parse_rejects_lexical_errors() {
        // CAP-AT-10: absolute / dot / dotdot / NUL / empty component / backslash / drive / UNC。
        assert_eq!(FilesystemTarget::parse(""), Err(PathError::Empty));
        assert_eq!(
            FilesystemTarget::parse("dir\0/x"),
            Err(PathError::ContainsNul)
        );
        assert_eq!(
            FilesystemTarget::parse("/etc/passwd"),
            Err(PathError::Absolute)
        );
        assert_eq!(
            FilesystemTarget::parse("@data/./x"),
            Err(PathError::DotComponent)
        );
        assert_eq!(
            FilesystemTarget::parse("@data/../x"),
            Err(PathError::ParentComponent)
        );
        assert_eq!(
            FilesystemTarget::parse("a//b"),
            Err(PathError::EmptyComponent)
        );
        assert_eq!(
            FilesystemTarget::parse("dir\\file"),
            Err(PathError::BackslashSeparator)
        );
        // Windows drive prefix と UNC。UNC(`//srv`) は先頭空 component として捕捉する。
        assert_eq!(FilesystemTarget::parse("C:/x"), Err(PathError::Absolute));
        assert_eq!(
            FilesystemTarget::parse("//server/share"),
            Err(PathError::Absolute)
        );
        // 不正な mount 名。
        assert_eq!(
            FilesystemTarget::parse("@1bad/x"),
            Err(PathError::InvalidMountName)
        );
    }

    #[test]
    fn fs_capability_root_lookup_is_exact_match() {
        // CAP-AT-29: mount は完全一致で引く（prefix 一致や登録順 fallback をしない）。
        let mut ops = BTreeSet::new();
        ops.insert(FsOperation::Read);
        let root = FilesystemRoot::new(
            MountName::new("data").expect("mount"),
            pid(1),
            ops,
            SymlinkPolicy::DenyAll,
            Arc::new(FakeDir(pid(1), SymlinkPolicy::DenyAll)),
        )
        .expect("root");
        let fs = FilesystemCapability::new([root]).expect("fs");
        assert!(fs.root(&MountName::new("data").expect("mount")).is_some());
        assert!(
            fs.root(&MountName::new("default").expect("mount"))
                .is_none()
        );
        assert!(fs.root(&MountName::new("dat").expect("mount")).is_none());
    }

    #[test]
    fn debug_is_secret_free() {
        // CAP-AT-14/22: env 値本文が Debug へ現れない。
        let value = EnvironmentValue::new("s3cr3t-token", DataClassification::Secret).expect("val");
        let dbg = format!("{value:?}");
        assert!(!dbg.contains("s3cr3t-token"), "value leaked: {dbg}");

        let env = EnvironmentSnapshot::from_entries([(
            "API_KEY".to_string(),
            EnvironmentValue::new("s3cr3t-token", DataClassification::Secret).expect("val"),
        )])
        .expect("snapshot");
        let set = CapabilitySet::builder()
            .environment(env)
            .expect("env")
            .build();
        let dbg = format!("{set:?}");
        assert!(!dbg.contains("s3cr3t-token"), "value leaked: {dbg}");
    }

    /// empty set の期待 CapabilitySetId bytes を encoding から再計算する（自己整合性確認用）。
    fn super_expected_empty() -> [u8; 32] {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"TSUMUGI-CAPSET-V1\0");
        buf.extend_from_slice(&0u64.to_be_bytes());
        crate::embedding::hash::sha256(&buf)
    }

    #[test]
    fn env_snapshot_get_and_keys() {
        let env = EnvironmentSnapshot::from_entries([
            (
                "B".to_string(),
                EnvironmentValue::new("2", DataClassification::Public).expect("v"),
            ),
            (
                "A".to_string(),
                EnvironmentValue::new("1", DataClassification::Public).expect("v"),
            ),
        ])
        .expect("snapshot");
        let keys: Vec<&str> = env.keys().collect();
        assert_eq!(keys, vec!["A", "B"], "keys must be sorted");
        assert_eq!(env.get("A").expect("A").expose_to_script(), "1");
        assert!(env.get("missing").is_none());
    }

    #[test]
    fn duplicate_env_key_rejected() {
        let err = EnvironmentSnapshot::from_entries([
            (
                "K".to_string(),
                EnvironmentValue::new("1", DataClassification::Public).expect("v"),
            ),
            (
                "K".to_string(),
                EnvironmentValue::new("2", DataClassification::Public).expect("v"),
            ),
        ])
        .expect_err("dup key");
        assert_eq!(err, ConfigError::DuplicateCallableName { name: "K".into() });
    }

    // --- C5-b: secure OS adapter（契約1〜5、CAP-AT-11/12 の Phase 2 範囲）---

    #[cfg(unix)]
    mod os_adapter {
        use super::*;
        use std::fs;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU64, Ordering};

        /// テスト用の一意な一時 directory を作る（後始末は best-effort）。
        struct TempRoot {
            path: PathBuf,
        }

        impl TempRoot {
            fn new() -> Self {
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                let mut path = std::env::temp_dir();
                path.push(format!("tsumugi-c5b-{}-{}", std::process::id(), n));
                fs::create_dir_all(&path).expect("create temp root");
                Self { path }
            }
        }

        impl Drop for TempRoot {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.path);
            }
        }

        /// component 列から [`RelativePath`] を作る（module 内なので private field へアクセス可）。
        fn rel(components: &[&str]) -> RelativePath {
            RelativePath {
                components: components.iter().map(|s| s.to_string()).collect(),
            }
        }

        fn handle(root: &TempRoot, policy: SymlinkPolicy) -> OsDirectoryHandle {
            OsDirectoryHandle::new(root.path.clone(), pid(100), policy)
        }

        #[test]
        fn read_write_roundtrip_within_root() {
            let root = TempRoot::new();
            let h = handle(&root, SymlinkPolicy::DenyAll);
            // 書き込み（Upsert/Truncate）。
            {
                let mut f = h
                    .open_file(
                        &rel(&["a.txt"]),
                        OpenFileRequest::Upsert {
                            mode: WriteMode::Truncate,
                        },
                    )
                    .expect("open write");
                f.write_all(b"hello").expect("write");
            }
            // 読み取り。
            let mut f = h
                .open_file(&rel(&["a.txt"]), OpenFileRequest::ReadExisting)
                .expect("open read");
            let bytes = f.read_to_end(None).expect("read");
            assert_eq!(bytes, b"hello");
            // metadata。
            let meta = h.metadata(&rel(&["a.txt"]), true).expect("metadata");
            assert_eq!(meta.kind, EntryKind::File);
            assert_eq!(meta.size_bytes, 5);
        }

        #[test]
        fn create_dir_list_and_remove() {
            let root = TempRoot::new();
            let h = handle(&root, SymlinkPolicy::DenyAll);
            h.create_dir(&rel(&["sub"])).expect("mkdir");
            {
                let mut f = h
                    .open_file(
                        &rel(&["sub", "x.txt"]),
                        OpenFileRequest::Upsert {
                            mode: WriteMode::Truncate,
                        },
                    )
                    .expect("open");
                f.write_all(b"z").expect("write");
            }
            let entries = h.list(&rel(&["sub"]), None).expect("list");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "x.txt");
            assert_eq!(entries[0].kind, EntryKind::File);
            // remove file, then empty dir。
            h.remove(&rel(&["sub", "x.txt"]), RemoveKind::FileOrSymlink)
                .expect("remove file");
            h.remove(&rel(&["sub"]), RemoveKind::EmptyDirectory)
                .expect("remove dir");
        }

        #[test]
        fn deny_all_rejects_final_symlink_open() {
            // 契約4: DenyAll は final symlink の read を拒否する（O_NOFOLLOW）。
            let root = TempRoot::new();
            // root 内に実体 file と、それを指す symlink を作る。
            fs::write(root.path.join("real.txt"), b"data").expect("write real");
            std::os::unix::fs::symlink(root.path.join("real.txt"), root.path.join("link.txt"))
                .expect("symlink");
            let h = handle(&root, SymlinkPolicy::DenyAll);
            // 実体はそのまま読める。
            let mut f = h
                .open_file(&rel(&["real.txt"]), OpenFileRequest::ReadExisting)
                .expect("open real");
            assert_eq!(f.read_to_end(None).expect("read"), b"data");
            // symlink 経由の open は拒否（O_NOFOLLOW により host error）。
            let res = h.open_file(&rel(&["link.txt"]), OpenFileRequest::ReadExisting);
            assert!(
                matches!(res, Err(AdapterError::Host(_))),
                "symlink open must fail"
            );
        }

        #[test]
        fn intermediate_symlink_denied_under_deny_all() {
            // 契約4: DenyAll は中間 symlink component を拒否する。
            let root = TempRoot::new();
            fs::create_dir(root.path.join("realdir")).expect("mkdir realdir");
            fs::write(root.path.join("realdir").join("f.txt"), b"x").expect("write");
            std::os::unix::fs::symlink(root.path.join("realdir"), root.path.join("linkdir"))
                .expect("symlink dir");
            let h = handle(&root, SymlinkPolicy::DenyAll);
            let res = h.open_file(&rel(&["linkdir", "f.txt"]), OpenFileRequest::ReadExisting);
            assert!(
                matches!(res, Err(AdapterError::Host(_))),
                "intermediate symlink must be denied"
            );
        }

        #[test]
        fn intermediate_symlink_within_root_followed() {
            // 契約4: FollowWithinRoot は root 内へ解決する中間 symlink を許可する。
            let root = TempRoot::new();
            fs::create_dir(root.path.join("realdir")).expect("mkdir realdir");
            fs::write(root.path.join("realdir").join("f.txt"), b"ok").expect("write");
            std::os::unix::fs::symlink(root.path.join("realdir"), root.path.join("linkdir"))
                .expect("symlink dir");
            let h = handle(&root, SymlinkPolicy::FollowWithinRoot);
            let mut f = h
                .open_file(&rel(&["linkdir", "f.txt"]), OpenFileRequest::ReadExisting)
                .expect("within-root symlink allowed");
            assert_eq!(f.read_to_end(None).expect("read"), b"ok");
        }

        #[test]
        fn intermediate_symlink_escaping_root_denied() {
            // 契約4: FollowWithinRoot でも root 外へ出る symlink は拒否する。
            let root = TempRoot::new();
            let outside = TempRoot::new();
            fs::write(outside.path.join("secret.txt"), b"top-secret").expect("write outside");
            // root 内から root 外 directory を指す symlink。
            std::os::unix::fs::symlink(&outside.path, root.path.join("escape"))
                .expect("symlink escape");
            let h = handle(&root, SymlinkPolicy::FollowWithinRoot);
            let res = h.open_file(
                &rel(&["escape", "secret.txt"]),
                OpenFileRequest::ReadExisting,
            );
            assert!(
                matches!(res, Err(AdapterError::Host(_))),
                "escaping symlink must be denied"
            );
        }

        #[test]
        fn create_new_rejects_existing() {
            let root = TempRoot::new();
            fs::write(root.path.join("exists.txt"), b"1").expect("write");
            let h = handle(&root, SymlinkPolicy::DenyAll);
            let res = h.open_file(
                &rel(&["exists.txt"]),
                OpenFileRequest::CreateNew {
                    mode: WriteMode::Truncate,
                },
            );
            assert!(
                matches!(res, Err(AdapterError::Host(_))),
                "create_new on existing must fail"
            );
        }

        #[test]
        fn metadata_follow_final_false_sees_symlink_kind() {
            // 契約4: OperateOnFinalEntry + follow_final=false は final symlink 自体を対象にできる。
            let root = TempRoot::new();
            fs::write(root.path.join("real.txt"), b"d").expect("write");
            std::os::unix::fs::symlink(root.path.join("real.txt"), root.path.join("link.txt"))
                .expect("symlink");
            let h = handle(&root, SymlinkPolicy::OperateOnFinalEntry);
            let meta = h.metadata(&rel(&["link.txt"]), false).expect("lstat");
            assert_eq!(meta.kind, EntryKind::Symlink);
        }

        #[test]
        fn remove_final_symlink_does_not_touch_target() {
            // 契約4: final symlink の delete は entry 自体を消し、対象 file を残す。
            let root = TempRoot::new();
            fs::write(root.path.join("real.txt"), b"keep").expect("write");
            std::os::unix::fs::symlink(root.path.join("real.txt"), root.path.join("link.txt"))
                .expect("symlink");
            let h = handle(&root, SymlinkPolicy::OperateOnFinalEntry);
            h.remove(&rel(&["link.txt"]), RemoveKind::FileOrSymlink)
                .expect("remove symlink");
            // 対象 file は残る。
            assert!(root.path.join("real.txt").exists());
            assert!(!root.path.join("link.txt").exists());
        }

        #[test]
        fn empty_relative_path_targets_root_metadata() {
            // 空 component 列（`@mount` だけ）は root 自身を指す。
            let root = TempRoot::new();
            let h = handle(&root, SymlinkPolicy::DenyAll);
            let meta = h.metadata(&rel(&[]), true).expect("root metadata");
            assert_eq!(meta.kind, EntryKind::Directory);
        }

        #[test]
        fn missing_file_maps_to_host_error() {
            // grant 済み root 内の not-found は catch 可能な host error（null/false へ潰さない）。
            let root = TempRoot::new();
            let h = handle(&root, SymlinkPolicy::DenyAll);
            let res = h.open_file(&rel(&["nope.txt"]), OpenFileRequest::ReadExisting);
            assert!(
                matches!(res, Err(AdapterError::Host(_))),
                "missing must be host error"
            );
        }

        #[test]
        fn debug_does_not_leak_host_path() {
            // 存在 oracle 防止: Debug に host path を出さない。
            let root = TempRoot::new();
            let h = handle(&root, SymlinkPolicy::DenyAll);
            let dbg = format!("{h:?}");
            assert!(
                !dbg.contains(&root.path.to_string_lossy().to_string()),
                "host path leaked: {dbg}"
            );
        }
    }
}
