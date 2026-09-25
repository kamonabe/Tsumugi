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
    /// C5-a では routing 基盤として用意する。builtin からの consult は C5-c で配線する。
    #[allow(dead_code)]
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

    /// filesystem authority（crate 内部限定。C1 では未配線、C5 で使う）。
    #[allow(dead_code)]
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
}
