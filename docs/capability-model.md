# Tsumugi — Capability Model仕様

最終更新: 2026-08-31
設計ステータス: **実装仕様確定・未実装**

## 1. 目的と規範範囲

本文書は、[ロードマップ](roadmap.md) Phase 2のdeny-by-default capability modelを固定する先行規範仕様である。外部状態へ触れる操作を実行単位で明示grantし、process environment、argv、clock、stdio、filesystem、processからambient authorityを除去する。

次期言語挙動、CLI、canonical errorとscript catch可否は[次期意味論・実装決定](semantic-decisions.md)、Phase 3/4の公開API・budget・state・transactionは[実行予算・協調実行仕様](execution-control.md)、Phase 5/6のaudit schemaとfail-closedは[決定性・実行時監査仕様](determinism-and-audit.md)、Phase 0の保証境界は[脅威モデル](threat-model.md)に従う。本書のPhase 2先行型はそれら最終契約の内部subsetとし、同じbuildへ旧型・旧event・旧adapter契約を併存させない。

## 2. Phase境界と原則

| Phase | 本文書と後続正本の境界 |
|---|---|
| 2 | empty default、grant/freeze、operation分離、同期Ready adapter、事前deny、path-handle認可、snapshot、host registry metadata、CLI profile、ambient実装削除 |
| 3 | [実行予算・協調実行仕様](execution-control.md)の有限budget、reservation、deadline、実行中cancelをadapterへ伝播 |
| 4 | 同文書のcooperative adapter extension、`Pending` ticket/waker、backpressure。同期traitの意味は変更しない |
| 6 | [決定性・実行時監査仕様](determinism-and-audit.md)のcanonical event、sequence、redaction、fail-closed sink |
| 7 | capability matrix、race/stress、budget/audit完全性の継続gate |

原則:

1. `CapabilitySet::empty()`が唯一のlibrary既定値で、全外部操作を拒否する。CLI profileはこのempty setから必要なadapterを明示grantして構築する。
2. grantはhostだけが行う。scriptはcapabilityを生成、列挙、複製、grant、revokeできない。
3. setはstart時freezeされ、実行中のgrant/revokeはない。取消はexecution全体をcancelし、新setで再実行する。
4. denialは対象OS API、resolver、host callbackより前に成立する。script実行中のoperation denialは[次期意味論・実装決定](semantic-decisions.md)第3節のcatch可能なcanonical `capability` / `sandbox` errorで、未捕捉時は`RuntimeError`となる。link/import等handler開始前のdenialだけがcatch不可terminal `Denied`となる。budget超過、deadline、cancel、audit fail-closed、`InternalFailure`等の制御・基盤terminalはscriptからcatchできない。
5. Environment、Clock、Stdin、Stdout、Filesystem各操作、ProcessExit、ModuleResolver、HostFunctionを別authorityとし、一方から他方を導出しない。
6. adapterは`Send + Sync`である。Phase 2同期traitはcaller thread上で有限時間にReady相当の結果を返す。Phase 4 cooperative extensionだけが`Pending`を返し、同期traitを暗黙に別threadへ移さない。HTTP/DB/mail/queueはcoreへ入れずhost adapterとする。

## 3. 共通公開型とconstructor

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum CapabilityKind {
    Environment,
    Clock,
    Stdin,
    Stdout,
    Filesystem,
    ProcessExit,
    ModuleResolver,
    HostFunction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct CapabilitySetId([u8; 32]);

impl CapabilitySetId {
    pub const fn as_bytes(&self) -> &[u8; 32];
}

#[derive(Clone)]
pub struct CapabilitySet(Arc<FrozenCapabilities>);

impl CapabilitySet {
    pub fn empty() -> Self;
    pub fn builder() -> CapabilitySetBuilder;
    pub fn id(&self) -> CapabilitySetId;
    pub fn contains(&self, kind: CapabilityKind) -> bool;
}

pub struct CapabilitySetBuilder { /* mutable, !Clone */ }

impl CapabilitySetBuilder {
    pub fn environment(self, value: EnvironmentSnapshot) -> Result<Self, ConfigError>;
    pub fn clock(self, value: Arc<dyn Clock>) -> Result<Self, ConfigError>;
    pub fn stdin(self, value: Arc<dyn Input>) -> Result<Self, ConfigError>;
    pub fn stdout(self, value: Arc<dyn Output>) -> Result<Self, ConfigError>;
    pub fn filesystem(self, value: FilesystemCapability) -> Result<Self, ConfigError>;
    pub fn process_exit(self, value: ProcessExit) -> Result<Self, ConfigError>;
    pub fn module_resolver(self, value: Arc<dyn ModuleResolver>) -> Result<Self, ConfigError>;
    pub fn grant_host_function(self, id: HostFunctionId) -> Result<Self, ConfigError>;
    pub fn build(self) -> CapabilitySet;
}
```

同kind二重設定は`ConfigError::DuplicateCapability`で、後勝ちにしない。host function grantだけは異なるIDを複数追加でき、同ID重複はconfiguration error。setからadapterを取り出すpublic getterはない。

`clone()`は同じimmutable authorityへのshallow cloneで、set IDと権限は同一。revoke APIは存在しない。

### 3.1 CapabilitySetId encoding

IDは認可tokenではなくpolicy相関IDである。SHA-256入力の全整数はunsigned big-endian、u128 policy/function IDは16 byte big-endian、文字列は`u64(length) || UTF-8 bytes`とする。

固定tag:

| 型 | tag |
|---|---|
| CapabilityKind | Environment=`0x01`, Clock=`0x02`, Stdin=`0x03`, Stdout=`0x04`, Filesystem=`0x05`, ProcessExit=`0x06`, ModuleResolver=`0x07`, HostFunction=`0x08` |
| DataClassification | Public=`0x00`, Sensitive=`0x01`, Secret=`0x02` |
| SymlinkPolicy | DenyAll=`0x00`, FollowWithinRoot=`0x01`, OperateOnFinalEntry=`0x02` |
| FsOperation | Read=`0x01`, Write=`0x02`, Create=`0x03`, Delete=`0x04`, Metadata=`0x05`, List=`0x06`, Import=`0x07` |

`entry_count`は存在するCapabilityKind group数で、host functionが複数でも1 groupと数える。groupはCapabilityKind tag昇順、environment key/mountはUTF-8 byte列昇順、host function IDは16 byte値昇順。empty setも同じdomainと`entry_count=0`でhashする。

```text
"TSUMUGI-CAPSET-V1\0"
|| u64(entry_count)
|| each group:
   capability_kind_tag
   Environment: u64(key_count) || each str(key) || classification_tag
                // value本文は含めない。policy IDでありinput identityではない
   Clock: clock_policy_id[16]
   Stdin: input_policy_id[16]
   Stdout: output_policy_id[16]
   Filesystem: filesystem policy encoding（第8.6節）
   ProcessExit: process_exit_policy_id[16]
   ModuleResolver: resolver_policy_id[16]
   HostFunction: u64(count) || sorted host_function_id[16]
```

adapter policy IDはhostが構成内容ごとに変える非zero u128で、pointer addressを使わない。

## 4. Adapter controlとerror

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdapterError {
    Host(HostError),
    Control(ControlStop),
    SecureResolutionUnsupported,
}

pub struct CapabilityCallContext<'a> { /* field private */ }

impl CapabilityCallContext<'_> {
    pub fn execution_id(&self) -> ExecutionId;
    pub fn deadline(&self) -> MonotonicInstant;
    pub fn cancellation(&self) -> &CancellationToken;
    pub fn check_control(&mut self) -> Result<(), ControlStop>;
    pub fn reserve(&mut self, request: BudgetRequest)
        -> Result<BudgetReservation, ControlStop>;
}
```

`ControlStop`、`BudgetRequest`、reservationのreserve/commit/refund、`MonotonicInstant`は[実行予算・協調実行仕様](execution-control.md)第3・7節を唯一の正本とする。独立した`BudgetFailure`、optional deadline、未計測時に成功するpublic `charge` APIを作らない。adapterはcall前、bounded block中に可能な地点、return前に`check_control`し、外部効果またはallocation前にatomic reservationを得る。

error projectionはvariantごとに分離する。script操作中の`AdapterError::Host`だけをsanitized canonical `host` errorとしてcatch可能にし、`SecureResolutionUnsupported`は第8.4節のfilesystem操作に限って同じchannelへ投影する。`AdapterError::Control`はcatch可能なlanguage errorへ変換せず、`ControlStop`のvariantに応じてterminal `BudgetExceeded` / `DeadlineExceeded` / `Cancelled`へだけ投影する。link/control-plane中のhost failureはterminal `HostError`とする。

script操作中のnon-filesystem capability denialは、[次期意味論・実装決定](semantic-decisions.md)第3.4節の「host function capability拒否」を共通のcanonical projectionとして使う。adapter-backed builtinの`{name}`は閉じた集合`env` / `now` / `input` / `print` / `exit`から選び、登録host functionではcatalogに固定した公開名を使う。filesystem denialだけは同節の`sandbox` projectionを使う。これはadapter-backed builtinをnative host functionとみなす分類ではなく、公開error kind/messageを一意にする互換境界である。

Phase 2の各同期traitは`may_block = false`で、caller thread上で有限時間に完了する場合だけ登録できる。Phase 4では同文書第6.2節の`CooperativeAdapter<Request, Response>` / `HostCallPoll::{Ready, Pending}` / `HostCallTicket`を対応request/response型へ追加できる。`Pending`はcooperative extensionだけが返し、同期traitのsignatureや意味を変更しない。両実装は同じcapability decision、budget、deadline、cancellation、call ID、audit lifecycleを共有する。

Phase 6のaudit emitterはprivate fieldとしてcontextへ追加するが、adapterから任意event名を発行するAPIは公開しない。core dispatcherだけがcanonical eventを生成する。

## 5. Environment snapshotとarguments

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum DataClassification { Public, Sensitive, Secret }

#[derive(Clone)]
pub struct EnvironmentValue {
    value: String,
    classification: DataClassification,
}

impl EnvironmentValue {
    pub fn new(value: impl Into<String>, classification: DataClassification)
        -> Result<Self, ConfigError>;
    pub fn expose_to_script(&self) -> &str;
    pub fn classification(&self) -> DataClassification;
}

#[derive(Clone)]
pub struct EnvironmentSnapshot(Arc<BTreeMap<String, EnvironmentValue>>);

impl EnvironmentSnapshot {
    pub fn empty() -> Self;
    pub fn from_entries(
        entries: impl IntoIterator<Item = (String, EnvironmentValue)>,
    ) -> Result<Self, ConfigError>;
    pub fn keys(&self) -> impl Iterator<Item = &str>;
    pub(crate) fn get(&self, key: &str) -> Option<&EnvironmentValue>;
}
```

key/valueはUTF-8、NULなし。keyは1..=256 bytes、重複keyはerror。snapshotはstart前に完成し、run中にprocess environmentを再読しない。

- Environment capabilityなしの`env()`はenvironment adapter call 0で内部`Denial`を生成し、script実行中はsanitized canonical `capability` errorとしてcatch可能にする。未捕捉時は`RuntimeError`であり、terminal `Denied`にはしない。
- capabilityあり・keyなしは`null`。
- safe profileはprotected runtime keyをsnapshotへ入れない。CLIの`--allow-env`とlegacy profileにも同じ判定を使う。
- protected判定はWindowsではkeyをUnicode uppercase化して`TSUMUGI_` prefixと比較し、その他OSではcase-sensitiveに`TSUMUGI_` prefixと比較する。これにより`tsumugi_*`とUnicode case aliasもWindowsで拒否する。
- custom embedding hostがprotected keyを明示投入する場合は`Secret`必須であり、CLI互換profileの保護とは別のtrusted-host操作とする。
- argumentsはEnvironmentではなく`ExecutionRequest.arguments`。空snapshotが既定でprocess argvへfallbackしない。

## 6. Clock

```rust
pub trait Clock: Send + Sync + 'static {
    fn policy_id(&self) -> NonZeroU128;
    fn now_utc(
        &self,
        context: &mut CapabilityCallContext<'_>,
    ) -> Result<SystemTime, AdapterError>;
}
```

`now()`はこのtraitだけを使う。deadlineはscript用ClockでなくEngineのmonotonic clockを使う。test utilityとして`FixedClock`を提供する。Clockなしの`now()`はtrait call 0で内部`Denial`を生成し、script実行中はcatch可能なcanonical `capability` error、未捕捉時は`RuntimeError`とする。

## 7. Stdin / Stdout

```rust
pub trait Input: Send + Sync + 'static {
    fn policy_id(&self) -> NonZeroU128;
    fn read_line(
        &self,
        context: &mut CapabilityCallContext<'_>,
        limit: ReadLimit,
    ) -> Result<InputLine, AdapterError>;
}

#[derive(Clone, Copy, Debug)]
pub struct ReadLimit { pub max_bytes: Option<NonZeroU64> }

pub enum InputLine { Line(String), Eof }

pub trait Output: Send + Sync + 'static {
    fn policy_id(&self) -> NonZeroU128;
    fn write_all(
        &self,
        context: &mut CapabilityCallContext<'_>,
        bytes: &[u8],
    ) -> Result<(), AdapterError>;
    fn flush(
        &self,
        context: &mut CapabilityCallContext<'_>,
    ) -> Result<(), AdapterError>;
}
```

- Inputなしの`input()`はInput adapter call 0で内部`Denial`を生成し、script実行中はcatch可能なcanonical `capability` error、未捕捉時は`RuntimeError`とする。EOFは`null`、`AdapterError::Host`はcatch可能なcanonical `host` errorで、`null`へ潰さない。`AdapterError::Control`は第4節どおりterminal outcomeへ投影する。
- Input adapterはincremental readし、finite limit NならN+1 byteを蓄積する前に`AdapterError::Control(ControlStop::BudgetExceeded(_))`。改行なしも同じ。
- Outputなしの`print`はOutput call 0で内部`Denial`を生成し、script実行中はcatch可能なcanonical `capability` error、未捕捉時は`RuntimeError`とする。grant時はUTF-8 bytes+改行をlogical write前に一括chargeする。
- 残量NならN bytes成功、N+1は`Output::write_all` call 0でBudgetExceeded。
- Phase 2はtraitとdeny/error分離まで、有限meterとN境界はPhase 3。

## 8. Filesystem

### 8.1 操作粒度

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum FsOperation {
    Read,
    Write,    // 既存file overwrite/append
    Create,   // 新規file/directory/destination entry
    Delete,
    Metadata,
    List,
    Import,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum SymlinkPolicy {
    DenyAll,
    FollowWithinRoot,
    OperateOnFinalEntry,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MountName(String);

impl MountName {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError>;
    pub fn as_str(&self) -> &str;
}

#[derive(Clone)]
pub struct FilesystemRoot {
    pub mount: MountName,
    pub policy_id: NonZeroU128,
    pub operations: BTreeSet<FsOperation>,
    pub symlink_policy: SymlinkPolicy,
    pub adapter: Arc<dyn DirectoryHandle>,
}

impl FilesystemRoot {
    pub fn new(
        mount: MountName,
        policy_id: NonZeroU128,
        operations: BTreeSet<FsOperation>,
        symlink_policy: SymlinkPolicy,
        adapter: Arc<dyn DirectoryHandle>,
    ) -> Result<Self, ConfigError>;
}

#[derive(Clone)]
pub struct FilesystemCapability { roots: Arc<[FilesystemRoot]> }

impl FilesystemCapability {
    pub fn new(roots: impl IntoIterator<Item = FilesystemRoot>)
        -> Result<Self, ConfigError>;
    pub fn roots(&self) -> impl Iterator<Item = &FilesystemRoot>;
}
```

`MountName`はASCII `[A-Za-z][A-Za-z0-9_-]{0,31}`。mount重複と空operationsはconfiguration error。同じpolicy IDは、同じadapter Arc・operations・symlink policyへ別mount aliasを付ける場合だけ許可する。`FilesystemRoot::new`は`adapter.policy_id()`と`adapter.symlink_policy()`が指定値と一致することを検証する。script filesystem operationでmountまたはoperationが不足する場合はadapter/OS call 0で内部`Denial`を生成し、catch可能なcanonical `sandbox` errorへ変換する。未捕捉時は`RuntimeError`であり、handler開始前のlink/importだけがterminal `Denied`となる。

### 8.2 Script pathとrouting

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelativePath { components: Arc<[String]> }

impl RelativePath {
    pub fn components(&self) -> impl Iterator<Item = &str>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathError {
    Empty,
    ContainsNul,
    Absolute,
    DotComponent,
    ParentComponent,
    EmptyComponent,
    BackslashSeparator,
    InvalidMountName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemTarget {
    pub mount: MountName,
    pub path: RelativePath,
}

impl FilesystemTarget {
    pub fn parse(script_path: &str) -> Result<Self, PathError>;
}
```

規範syntaxは`@MOUNT/component/component`。unqualified `component/component`はmount名`default`へparseする。mountは完全一致し、prefix一致や登録順fallbackをしない。

relative componentはUTF-8、1 byte以上、NULなし。`/`だけをseparatorとし、absolute `/`、`.`、`..`、空component、backslash、drive prefix、UNCを拒否する。host platform pathへ変換する前に検証する。`PathError`はcapabilityの有無を調べる前にcatch可能な`RuntimeErrorCode::Argument`へ変換するため、malformed pathとauthority不足のchannelは混在しない。parse後に該当mountがない場合はadapter/OS call 0で内部`Denial { code: ResourceNotGranted }`を生成し、script filesystem operationならcatch可能なcanonical `sandbox` errorへ変換する。

### 8.3 Portable path-handle契約

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteMode { Truncate, Append }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenFileRequest {
    ReadExisting,
    WriteExisting { mode: WriteMode },
    CreateNew { mode: WriteMode },
    Upsert { mode: WriteMode },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind { File, Directory, Symlink, Other }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicMetadata {
    pub kind: EntryKind,
    pub size_bytes: u64,
    pub readonly: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    pub name: String, // 単一の検証済みcomponent。path separatorなし
    pub kind: EntryKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoveKind { FileOrSymlink, EmptyDirectory }

pub trait DirectoryHandle: Send + Sync + 'static {
    fn policy_id(&self) -> NonZeroU128;
    fn symlink_policy(&self) -> SymlinkPolicy;

    fn open_file(
        &self,
        context: &mut CapabilityCallContext<'_>,
        path: &RelativePath,
        request: OpenFileRequest,
    ) -> Result<Box<dyn FileHandle>, AdapterError>;
    fn create_dir(
        &self,
        context: &mut CapabilityCallContext<'_>,
        path: &RelativePath,
    ) -> Result<(), AdapterError>;
    fn metadata(
        &self,
        context: &mut CapabilityCallContext<'_>,
        path: &RelativePath,
        follow_final: bool,
    ) -> Result<PublicMetadata, AdapterError>;
    fn list(
        &self,
        context: &mut CapabilityCallContext<'_>,
        path: &RelativePath,
        max_entries: Option<NonZeroU64>,
    ) -> Result<Vec<DirectoryEntry>, AdapterError>;
    fn remove(
        &self,
        context: &mut CapabilityCallContext<'_>,
        path: &RelativePath,
        kind: RemoveKind,
    ) -> Result<(), AdapterError>;
    fn rename(
        &self,
        context: &mut CapabilityCallContext<'_>,
        from: &RelativePath,
        to_directory: &dyn DirectoryHandle,
        to: &RelativePath,
        replace: bool,
    ) -> Result<(), AdapterError>;
}

pub trait FileHandle: Send + 'static {
    fn read_to_end(
        &mut self,
        context: &mut CapabilityCallContext<'_>,
        max_bytes: Option<NonZeroU64>,
    ) -> Result<Vec<u8>, AdapterError>;
    fn write_all(
        &mut self,
        context: &mut CapabilityCallContext<'_>,
        bytes: &[u8],
    ) -> Result<(), AdapterError>;
    fn metadata(
        &self,
        context: &mut CapabilityCallContext<'_>,
    ) -> Result<PublicMetadata, AdapterError>;
}
```

adapter契約:

1. root内判定と利用を同じdirectory/file handleへbindする。`canonicalize`でcheck後に元pathを`std::fs`へ渡す実装は禁止。
2. Unix `openat`だけに依存しない。Windows handle、capability filesystem、in-memory adapter等で同じ契約を実現できる。
3. platformがroot拘束とsymlink policyを保証できなければ`SecureResolutionUnsupported`。文字列prefix checkへfallbackしない。script filesystem操作ではcanonical `host` error（code `secure_resolution_unsupported`）へ変換してcatch可能とし、未捕捉なら`RuntimeError`となる。link中のresolverで発生してscript handlerが存在しない場合だけterminal `HostError`へ変換する。対象fileのread/write/deleteは行わない。
4. `DenyAll`は途中/final symlinkを拒否。`FollowWithinRoot`は解決先を同じroot handleへ拘束。`OperateOnFinalEntry`は中間symlinkをroot内へ拘束し、delete/renameおよび`follow_final=false`のmetadataだけがfinal symlink entry自体を扱える。Read/Write/Create/Listはfinal symlinkを拒否する。
5. create/write/appendはdangling final symlinkを追従しない。targetをsecureにbindできなければ拒否。
6. renameはsource `Delete`、destination `Create`、replace時はdestination `Delete`も必要。両handle認可後、単一rename call。
7. case/Unicode aliasはadapterがfilesystem規則でroot内拘束する。

### 8.4 Operation matrix

| Script operation | Read | Write | Create | Delete | Metadata | List | Import |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| `read_file`, `read_lines` | ✓ |  |  |  |  |  |  |
| `write_file`, `append_file` |  | ✓ | ✓ |  |  |  |  |
| `mkdir` |  |  | ✓ |  |  |  |  |
| `remove`, `remove_dir` |  |  |  | ✓ |  |  |  |
| exists/type/size |  |  |  |  | ✓ |  |  |
| `list_dir` |  |  |  |  | ✓ | ✓ |  |
| `rename` no replace |  |  | ✓(to) | ✓(from) |  |  |  |
| `rename` replace |  |  | ✓(to) | ✓(from+to) |  |  |  |
| Tsumugi import |  |  |  |  |  |  | ✓ |

builtin mappingは固定する。`read_file`/`read_lines`は`ReadExisting`、`write_file`は`Upsert { Truncate }`、`append_file`は`Upsert { Append }`を使う。upsertはfileの存在を調べずWrite+Createを事前要求する。現行言語に`WriteExisting`/`CreateNew`専用builtinはなく、これらは将来またはhost adapter用である。`remove`は`FileOrSymlink`、`remove_dir`は`EmptyDirectory`を使う。`PublicMetadata`は時刻、owner、absolute pathを公開せず、`DirectoryEntry.name`が空、`.`、`..`、separator含有ならadapter contract violationとしてHostErrorにする。

`Import`はruntime `Read`と独立する。filesystem resolverが内部でImport rootを使う。Readだけでimportできず、Importだけで`read_file`できない。

### 8.5 Denialと存在oracle

lexical path syntaxはcapability lookupより先に検証し、`PathError`をcatch可能な`RuntimeErrorCode::Argument`へ変換する。syntaxが正しい場合だけcapability/mount/operationを調べ、不足時はadapter/OS call 0で内部`Denial`を生成する。script filesystem operationではsafe fieldだけからcatch可能なcanonical `sandbox` errorへ変換し、未捕捉なら`RuntimeError`とする。handler開始前のimport resolver denialだけはcatch不可terminal `Denied`である。存在path/不存在pathを同じpublic denial code/messageにし、absolute host path、symlink、permissionを含めない。grant済みroot内のnot-found等だけがcatch可能なcanonical `host` errorまたは既存言語意味論の`null`/`false`になれる。denyを`null`/`false`へ変換しない。

### 8.6 Filesystem policy encoding

CapabilitySetIdにはmount UTF-8 byte順で次をencodeする。

```text
u64(root_count)
|| each root:
   str(mount) || policy_id[16] || symlink_policy_u8
   || u64(operation_count) || sorted operation_u8
```

host OS pathやcredentialはencodeしない。policy内容が変わるとhostはpolicy IDを変えなければならない。

## 9. ProcessExit

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessExit { policy_id: NonZeroU128 }

impl ProcessExit {
    pub const fn new(policy_id: NonZeroU128) -> Self;
    pub const fn policy_id(self) -> NonZeroU128;
}
```

OS process終了権限ではなくexecutionを`ExecutionOutcome::Exited { code, usage }`にする権限。grant済み0..=255はcatch不可terminal `Exited`、未grantはadapter/process/OS call 0で内部`Denial`を生成し、script実行中はcatch可能なcanonical `capability` error、未捕捉時は`RuntimeError`とする。library/callback/adapterは`std::process::exit`を呼ばない。

## 10. ModuleResolverとbounded source

```rust
pub trait ModuleSource: Send + 'static {
    fn read_chunk(
        &mut self,
        context: &mut CapabilityCallContext<'_>,
        max_bytes: NonZeroUsize,
    ) -> Result<ModuleChunk, AdapterError>;
}

pub enum ModuleChunk {
    Bytes(Vec<u8>), // lenは1..=max_bytes
    Eof,
}

pub trait ModuleResolver: Send + Sync + 'static {
    fn policy_id(&self) -> NonZeroU128;
    fn resolve(
        &self,
        context: &mut CapabilityCallContext<'_>,
        request: ResolveRequest<'_>,
    ) -> Result<ResolvedModule, AdapterError>;
}

pub struct ResolveRequest<'a> {
    pub importer: Option<&'a ModuleId>,
    pub specifier: &'a str,
    pub language_revision: LanguageRevision,
}

pub struct ResolvedModule {
    pub id: ModuleId,
    pub source: Box<dyn ModuleSource>,
    pub classification: DataClassification,
}
```

resolverはlink時だけ呼ぶ。Engineはsourceを64 KiB以下のchunkで読み、Phase 3のmodule/total limit NならN+1を蓄積する前に停止する。chunkがmaxを超えたadapterは`HostErrorCode="resolver_contract_violation"`。完了bytesをUTF-8検証し、invalidならmodule compile diagnostic。

ModuleIdは同じlogical moduleへ同じIDを返す。同ID/異hashはlink error。HTTP resolverをcoreへ内蔵しない。host adapterがnetwork/TLS/redirect/credential/timeout/size/cache/supply-chainを担う。

## 11. HostFunctionRegistry

### 11.1 型とconstructor

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct HostFunctionId(NonZeroU128);

impl HostFunctionId {
    pub const fn new(value: NonZeroU128) -> Self;
    pub const fn get(self) -> NonZeroU128;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Arity {
    Exact(u16),
    Range { min: u16, max: u16 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostCost {
    pub base_fuel: u64,
    pub per_argument_fuel: u64,
    pub per_value_unit_fuel: u64,
    pub max_result_bytes: Option<NonZeroU64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditValuePolicy { Omit, TypeOnly, LengthOnly }

#[derive(Clone, Debug)]
pub struct HostFunctionDescriptor {
    pub id: HostFunctionId,
    pub name: String,
    pub arity: Arity,
    pub cost: HostCost,
    pub argument_audit: Vec<AuditValuePolicy>,
    pub result_audit: AuditValuePolicy,
    pub may_block: bool,
}

impl HostFunctionDescriptor {
    pub fn validate(&self) -> Result<(), ConfigError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostCallError {
    Host(HostError),
    Control(ControlStop),
}

pub struct HostCallContext<'a> { inner: CapabilityCallContext<'a> }

impl HostCallContext<'_> {
    pub fn check_control(&mut self) -> Result<(), HostCallError>;
    pub fn charge_external(&mut self, units: u64) -> Result<(), HostCallError>;
}

pub trait HostFunction: Send + Sync + 'static {
    fn descriptor(&self) -> &HostFunctionDescriptor;
    fn call(
        &self,
        context: &mut HostCallContext<'_>,
        arguments: &[Value],
    ) -> Result<Value, HostCallError>;
}

pub struct HostFunctionRegistryBuilder { /* private */ }
pub struct HostFunctionRegistry { /* immutable */ }

impl HostFunctionRegistry {
    pub fn builder() -> HostFunctionRegistryBuilder;
    pub fn get(&self, id: HostFunctionId) -> Option<&Arc<dyn HostFunction>>;
}

impl HostFunctionRegistryBuilder {
    pub fn register(self, function: Arc<dyn HostFunction>) -> Result<Self, ConfigError>;
    pub fn build(self) -> Result<HostFunctionRegistry, ConfigError>;
}
```

nameは既存identifier grammarを満たす1..=64 UTF-8 bytesとする。`host_` prefixは必須にしない。core keyword、`print`、core/context builtin、他host functionとの重複はbuild error。これにより`http_request`のような承認済み業務名も登録できる。user bindingはcall siteでbuiltin/host fallbackより優先する。

`validate()`は次を強制する。`Arity::Range`は`min <= max`。`argument_audit`が空なら全indexを`Omit`とする。空でない場合、`Exact(n)`では長さがn、`Range { max, .. }`では長さがmaxでなければerror。実callのindex `i < actual_arity`にはvector[i]を使い、actualより後のpolicyは無視する。`max`は1024以下、`name`は64 bytes以下、host function IDはregistry内で一意でなければならない。

registry登録とexecution grantは別。link時にname→IDを固定し、run中のname lookupやregistry差替えをしない。registered-but-not-grantedはcallback call 0で内部`Denial`を生成し、script call中はcatch可能なcanonical `capability` error、未捕捉時は`RuntimeError`とする。unknown nameは通常language name error。handler開始前に同じ判断が必要なcontrol-plane処理だけがterminal `Denied`となる。

### 11.2 Arity、cost、result

1. arityはargument式評価前に検証。不一致はcatch可能な通常argument error。
2. Phase 2はcost metadataのvalidationとcatalog格納まで。Phase 3でgrant後・callback前にfuel/host-callを課金する。
3. value unitは各Value nodeにつき1、Stringは加えてUTF-8 byte数、Listは要素、Dictはkey/valueを再帰加算する。checked u64加算しoverflowはBudgetExceeded。Function/Errorはnode 1だけ。現行非循環Valueを前提とし、循環導入時はvisited identityで二重計上しない。
4. resultはPhase 3で`max_result_bytes`とheap budgetをscriptへ渡す前に検査する。超過時、callback副作用はrollbackしないが全language-stateはterminal規則どおりrollbackし、auditの`host_effects_may_remain`へ反映する。
5. script call中の`HostCallError::Host`はcanonical `host` runtime errorへ変換してcatch可能とし、未捕捉なら`RuntimeError`となる。`HostCallError::Control(BudgetExceeded | DeadlineExceeded | Cancelled)`はcatch不可terminalである。link/control-plane中でscript handlerが存在しないhost failureだけはterminal `HostError`となる。業務not-found等は明示Valueまたは登録済みcanonical script errorで返す。

### 11.3 Redaction、panic、再入

Phase 2はdescriptorに`Omit`、`TypeOnly`、`LengthOnly`だけを許可し、runtime value本文を記録するpolicyを設けない。したがって完全taint trackingなしでもcore auditへargument/result本文は出ない。defaultは`Omit`。`LengthOnly`は型と長さだけ、`TypeOnly`は型だけで、内容hashも記録しない。

Phase 6のaudit sinkはこのmetadataを使う。secretを扱うfunctionは`Omit`を必須とし、host review事項とする。`HostError.safe_message`、panic eventへ本文、credential、absolute path、backtraceを入れない。

callback unwind panicは`InternalFailure`、context poison。callbackへEngine/ExecutionContext参照を渡さず同execution再入を禁止する。panic=abort、FFI UB、process exit/abort、無期限blockは捕捉不能。

## 12. Capability × operation matrix

| Script operation | Env | Clock | Stdin | Stdout | FS op | Exit | Resolver | HostFn grant |
|---|:---:|:---:|:---:|:---:|---|:---:|:---:|:---:|
| `args()` | — | — | — | — | — | — | — | — |
| `env()` | ✓ | — | — | — | — | — | — | — |
| `now()` | — | ✓ | — | — | — | — | — | — |
| `input()` | — | — | ✓ | — | — | — | — | — |
| `print` | — | — | — | ✓ | — | — | — | — |
| `read_file` | — | — | — | — | Read | — | — | — |
| `write_file` upsert | — | — | — | — | Write+Create | — | — | — |
| metadata | — | — | — | — | Metadata | — | — | — |
| `list_dir` | — | — | — | — | Metadata+List | — | — | — |
| remove | — | — | — | — | Delete | — | — | — |
| import | — | — | — | — | optional resolver内部Import | — | ✓ | — |
| `exit()` | — | — | — | — | — | ✓ | — | — |
| registered host function | — | — | — | — | — | — | — | ✓ |

`args()`はrequest snapshotで、capabilityではない。

## 13. Denial、error、audit schema

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DenialCode {
    CapabilityNotGranted,
    OperationNotGranted,
    ResourceNotGranted,
    HostFunctionNotGranted,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct OperationId(String); // crate-defined fixed ASCII identifier

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ResourceLabel(String); // hostがPublicと宣言したlabelだけ

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Denial {
    pub code: DenialCode,
    pub capability: CapabilityKind,
    pub operation: OperationId,
    pub public_resource: Option<ResourceLabel>,
}
```

invalid pathはhost API misuseでなく、grantの有無を調べる前にcatch可能なcanonical argument errorとする。capability/mount/operation不足はOS/adapter call 0の`Denial`を生成する。script operationではそのsafe fieldだけからcanonical capability errorを作りcatch可能、link/control-planeではterminal `Denied`にする。いずれも存在、absolute path、environment value、callback detailを含めない。

Phase 6 event schemaは[決定性・実行時監査仕様](determinism-and-audit.md)第7節の`ExecutionStarted`、`CapabilityDecision`、`HostCallStarted`、`HostCallFinished`、`BudgetCharged`、`Yielded`、`Resumed`、`Terminal`だけを使う。`CapabilityAllowed`、`CapabilityDenied`、`ExecutionTerminated`等の別event名を定義しない。host call denyも同じoperation ID/call IDで`HostCallStarted`→`CapabilityDecision(Deny)`→`HostCallFinished(Denied)`とし、adapter/OS call 0でpairを閉じる。Phase 2はdescriptor/redaction metadataとeventへの写像を固定するがeventを発行せず、Phase 6でbounded journal、sequence、ack、fail-closedを有効化する。

## 14. CLI profileとoption grammar

### 14.1 共通grammar

唯一のCLI文法は[次期意味論・実装決定](semantic-decisions.md)第6節である。本節はその`OPTIONS`に属するprofile/capability optionの値検証とauthority構築だけを定める。

```text
tsumugi [OPTIONS] [SCRIPT [ARGS...]]

OPTIONS:
  --vm
  --profile safe|legacy
  --allow-env KEY
  --allow-clock
  --allow-script-stdin
  --allow-exit
  --deny-stdout
  --fs-root NAME=PATH
  --fs-op NAME=OP[,OP...]
  --allow-import-root NAME=PATH
  --help
  --version
  --
```

- option解析中の最初のpositionalをSCRIPTとし、それ以後の全tokenはoption風でも再解釈せずscript argumentへ順序どおり渡す。`--`はoption terminatorであり、後続tokenがなければSCRIPTなしとしてREPLを起動する。
- SCRIPT `-`はstdin source、SCRIPTなしはREPLである。capability optionは各REPL executionへ同じfrozen profileとして適用する。
- `--vm`はidempotent。profile/boolean option重複、unknown option、option値欠落、同NAME・同KEY重複、profileとの不正組合せ、非UTF-8は副作用前にstderrへ診断しexit 1とする。CLI usage errorにexit 2を使わない。
- `NAME`はMountName grammar。`OP`は`read|write|create|delete|metadata|list`。`import`は`--fs-op`では受理せずresolver optionだけで付与する。
- `--fs-root NAME=PATH`と`--fs-op NAME=...`は1対1必須。CLIはroot directory handleをexecution作成前に開き、safe profileのsymlink policyを常に`DenyAll`とする。
- scriptは`@NAME/...`でrootを選ぶ。unqualified pathにはNAME `default`が必要である。
- `--allow-import-root NAME=PATH`はspecifier `@NAME/...`だけを解決するfilesystem `ModuleResolver`を作り、runtime filesystemをgrantしない。
- capability optionはsafe profileだけで受理し、legacyとの同時指定はusage error 1とする。
- `--allow-env KEY`はCLI execution作成時にprocess envのOS上のkey同一性で完全一致する値をsnapshotする。不存在はsnapshotへ入れずwarningなし。protected runtime keyは第5節のOS-aware判定でusage error 1とする。
- `--allow-clock`はsystem clock、`--allow-script-stdin`はscript Input、`--allow-exit`はProcessExitをprofile builderへ明示grantする。
- safe file/stdin/REPL profileはstdout adapterを`CapabilitySetBuilder::stdout`で明示grantする既定構成で、ambient stdio accessではない。`--deny-stdout`はそのgrantを除去する。REPL prompt出力はscript capability外のCLI I/Oである。
- 公式CLIはhost function registryを持たないためhost function grant optionを提供しない。embedding hostが登録とexecution grantを別々に行う。

### 14.2 Safe profile

`--profile safe`はRelease N以降の既定。root sourceはCLIがfileまたはstdinから明示読込みしてEngineへ渡し、script filesystem capabilityへ暗黙追加しない。profile builderはstdout adapterを明示grantし、arguments snapshotをrequestへ設定する。environment、clock、script stdin、filesystem、exit、resolver、host functionはoptionなしでは付与しない。このstdout既定は`CapabilitySet`に実在する明示authorityであり、coreがprocess stdoutへambient接続することを許さない。

### 14.3 Legacy profile

`--profile legacy`だけが互換用authorityを明示構築する。

- process envをstart前snapshotし`TSUMUGI_`を除外。
- system clock、script stdin/stdout、ProcessExitをgrant。
- CLI adapterだけが`TSUMUGI_SANDBOX`と`TSUMUGI_ENV_ALLOW`を1回読む。core evaluator/builtinは読まない。
- sandbox設定あり: comma-separated host rootsをstart時CWD基準でabsolute化してdirectory handleとして開き、`legacy0`, `legacy1`, ... mountへ順序固定する。rootが1つなら`default` aliasも同じhandle/policy IDへ付ける。旧absolute pathはOS lexical componentで各rootへのrelative candidateを作り、含むrootのうちcomponent数が最長のものを選ぶ。同じ長さの候補が複数ならprofile構築error。旧relative pathはsnapshot CWDを含む最長rootへrouteし、該当rootがなければadapter/OS call 0の内部`Denial`からcatch可能なcanonical `sandbox` errorへ変換する。候補選択後の解決とI/Oはroot handleだけを使う。
- sandbox未設定/空: legacy専用`LegacyFilesystemTranslator`へunrestricted filesystem namespace authorityを明示grantしsecurity warningをstderrへ1件出す。Unixは`/`とsnapshot CWD handle、Windowsは要求されたvolume rootとsnapshot CWD handleへ安全なhandle操作を使う。authority自体がunrestrictedなだけでcheck/use文字列fallbackは使わない。
- env allow設定あり: comma-separated exact keyまたは末尾`*` prefixだけsnapshot。未設定/空は`TSUMUGI_`以外を全snapshotしwarning 1件。
- translatorは旧absolute pathをnamespace root handle、旧relative pathをstart時CWD handleへ変換する。symlink操作はhandle adapter契約に従う。secure handleを実装できないplatformではfilesystem capability構築を拒否。
- legacy authorityは`ExecutionStarted.capability_policy_hash`と通常のcanonical decision/host-call eventで観測する。warning専用の別audit eventを追加しない。Phase 2完了条件はstderr warning、Phase 6では同じ8 event schemaとfail-closedを使う。

### 14.4 Release移行

| Release | default | 旧環境変数 |
|---|---|---|
| N-1 | legacy | 読む。起動時deprecation warning。safeを選択可 |
| N | safe | safeでは無視。legacyは明示flag必須 |
| N+1 | safe | coreから直接参照削除済み。legacy adapter継続可否は別判断 |

`OnceLock` process-global policyをper-execution capabilityへ再利用しない。

## 15. Clone、revoke、context再利用

1. builderはbuildで消費、setはimmutable。
2. cloneは同じset ID/adapter Arc。clone片方だけのrevokeなし。
3. start後のbuilder、registry、process env、legacy env var変更は実行へ反映しない。
4. 緊急取消はexecution全体の`CancellationToken`。個別authorityを途中revokeしてresumeしない。
5. contextは前回setを保存しない。次requestがemptyならdeny-by-default。
6. scriptがenv/argumentsをglobal Valueへ保存した場合、保存したexecutionが`Completed` / `Exited`でcommitしたときだけ残り得る。他terminalではAUD-024により開始時点へrollbackする。別tenant再利用前に`clear_user_state`または新contextを使う。

## 16. HTTP / DB / 外部service

HTTP client、DB driver、SMTP、cloud SDKをcoreへ追加しない。hostは任意URLの汎用関数より`host_orders_lookup(order_id)`のような最小業務操作をhost functionとして公開する。

credentialはhost内部保持。destination allow-list、TLS、redirect、timeout、request/response size、transaction、rate limitはhost adapter責任。外部service failureはscript call中ならcatch可能canonical `host` error、handler開始前ならterminal `HostError`、業務not-foundは明示Valueとする。

## 17. AUD-049とBuiltinSpec / HostFunctionRegistry

core/context builtinは[次期意味論・実装決定](semantic-decisions.md)第13節の単一`BuiltinSpec` registryを正本とし、tree dispatch、VM/compiler認識、arity、context metadata、generated documentationをそこから導出する。`builtin_core.rs`、`builtin.rs`、`compiler.rs`へ独立した公開builtin名一覧を残さない。

`HostFunctionRegistry`はhostがruntimeに構築する別registryであり、`BuiltinSpec`へhost descriptorを混在させない。ただしEngine build時にkeyword、`BuiltinSpec`、既登録host functionとの名前衝突を一括検査し、link時にname→`HostFunctionId`を固定する。tree/VM/compilerのcallable resolutionは「user binding → public builtin → host registry」の共通resolverを使い、host registryを第4の手書き名前一覧としてbackendごとに複製しない。

AUD-049のcontract testは、BuiltinSpec全entryのtree/VM/compiler/generated docs一致、HostFunctionRegistryの衝突検査、同じregistry snapshotからのhost name resolution、unknown/registered/ungranted/grantedの4組合せを検証する。

## 18. 実装slice

| Slice | Phase | 内容 | 完了条件 |
|---|---:|---|---|
| C1 | 2 | CapabilitySet/Denial/dispatcher/empty default | adapter call前deny |
| C2 | 2 | CallableCatalog統合 | AUD-049 |
| C3 | 2 | Environment/args/Clock | ambient process read 0 |
| C4 | 2 | Input/Output trait | deny/EOF/HostError分離 |
| C5 | 2 | mount routing/DirectoryHandle/FileHandle | path/symlink/oracle |
| C6 | 2 | ModuleResolver/stream/link接続 | runtime resolver 0 |
| C7 | 2 | ProcessExit | structured Exited |
| C8 | 2 | HostFunctionRegistry metadata/grant | name/arity/redaction/panic |
| C9 | 2 | CLI safe/legacy | AUD-018、migration warning |
| C10 | 2 | ambient実装削除 | core直接OS access 0 |
| C11 | 3 | finite meter/deadline/runtime cancel/cost | N境界、reservation |
| C12 | 4 | cooperative adapter/ticket/waker | Ready/Pending、backpressure、cancel |
| C13 | 6 | canonical audit sink/event/sequence | 8 event、fail-closed、correlation |

C1→C2、C3/C4/C5/C7を並行、C6はstream後、C8はBuiltinSpec衝突検査後、C9/C10を最後に行う。C11〜C13をPhase 2完了条件へ含めないが、Phase 2のpublic signatureは後続最終型と競合させない。

## 19. 受入基準

| ID | Phase | 基準 |
|---|---:|---|
| CAP-AT-01 | 2 | emptyでenv/clock/stdin/stdout/FS 7操作/exit/resolver/host functionを各1回試しadapter/callback/OS call 0。script操作はcatch可能capability error、link/control-planeはterminal Denied |
| CAP-AT-02 | 2 | grantした1操作だけ成功し隣接操作deny |
| CAP-AT-03 | 2 | clone ID/権限一致、変更/revoke APIなし、start後snapshot不変 |
| CAP-AT-04 | 2/3 | cancel後、新setで別実行可。Phase 3で旧実行だけCancelled |
| CAP-AT-05 | 2 | env snapshot後process env変更が不変、missing=null。capability不足はenvironment adapter call 0のcatch可能capability errorで、未捕捉時RuntimeError |
| CAP-AT-06 | 2 | fixed clockで同結果。Clockなしはtrait call 0のcatch可能capability errorで、未捕捉時RuntimeError |
| CAP-AT-07 | 3 | input N-1/N成功、N+1は蓄積前BudgetExceeded。改行なし同じ |
| CAP-AT-08 | 3 | output N-1/N成功、N+1はOutput call 0 |
| CAP-AT-09 | 2/3 | EOF / `AdapterError::Host` / authority不足 / `AdapterError::Control`を、順に`null` / catch可能host error / catch可能capability error / terminal control outcomeへ分離する。第4節のcanonical name全件をexact matchし、link前failureはterminal channelへ分離 |
| CAP-AT-10 | 2 | path absolute/dot/dotdot/NUL/empty/backslash/drive/UNC/prefix衝突を拒否 |
| CAP-AT-11 | 2/7 | symlink途中/final/dangling/renameを全policyで検証しroot外変更0。raceをstress gate化 |
| CAP-AT-12 | 2 | 許可外存在/不存在はadapter call 0、同denial。root内だけnot-found可 |
| CAP-AT-13 | 2 | FS operation matrix全組合せで1 authority欠落ごとI/O前に内部Denialを生成。script operationはcatch可能sandbox error、未捕捉時RuntimeError |
| CAP-AT-14 | 2 | secure resolution不能adapterはfail closed、文字列fallback 0。script operationはcatch可能host error、未捕捉時RuntimeError |
| CAP-AT-15 | 3 | file read/write N-1/N成功、N+1はallocation/write前BudgetExceeded |
| CAP-AT-16 | 2 | importありresolverなしはresolver call 0のterminal Denied。grant時linkだけ、run 0 |
| CAP-AT-17 | 3 | source N-1/N成功、N+1はchunk蓄積前拒否 |
| CAP-AT-18 | 2 | exit capabilityなしはprocess/OS call 0のcatch可能capability error（未捕捉時RuntimeError）、あり0/255 Exited、-1/256 RuntimeError、process継続 |
| CAP-AT-19 | 2/3 | Phase 2でregistered/granted 4組合せとarity。ungrantedはcallback 0のcatch可能capability error（未捕捉時RuntimeError）。Phase 3でfuel N-1/N/N+1 |
| CAP-AT-20 | 2 | catalog/tree/VM/compiler/generated docsの名前・arity・metadata完全一致、重複build error |
| CAP-AT-21 | 2/3 | callback success/catch可能host error/panicを分離。Phase 3でresult N+1 BudgetExceeded。link前host failureはterminal HostError |
| CAP-AT-22 | 2/6 | Omit/TypeOnly/LengthOnly serializerにfake secret本文0。Phase 6でsink eventも同じ |
| CAP-AT-23 | 2 | safe profileでroot source読込みとprofile builderが明示grantしたstdout以外ambient call 0。`--deny-stdout`時はstdout call 0で、全option mapping一致 |
| CAP-AT-24 | 2/6 | legacy互換、empty sandbox/env allowはstderr warning各1。Phase 6でも別eventを追加せずcanonical schemaとfail-closedを維持 |
| CAP-AT-25 | N-1/N | default profile差とwarningをgolden固定 |
| CAP-AT-26 | 2 | safeで旧環境変数変更がset ID/挙動へ影響0 |
| CAP-AT-27 | 2 | adapter/CLI以外のprocess env、runtime fs、stdio、process exit、ambient clock直接利用0 |
| CAP-AT-28 | 2 | CapabilitySetId/filesystem encodingのgolden bytes/hash固定 |
| CAP-AT-29 | 2 | 複数mount qualified/default/missing/duplicateのroutingを完全一致検証 |
| CAP-AT-30 | 6 | `ExecutionStarted`、`CapabilityDecision`、host call pair、budget/yield/resume、最後の`Terminal`に欠番・重複がなく、sink failureでfail-closed |

## 20. ロードマップ・監査項目との関係

- **Phase 0:** security boundaryではないこと、OS責任、residual riskは[脅威モデル](threat-model.md)。
- **Phase 1:** `CapabilitySet`は[実行予算・協調実行仕様](execution-control.md)のfinite `ExecutionRequest`へmoveし、terminal outcomeは[組み込みAPI仕様](embedding-api.md)、catch規則は[次期意味論・実装決定](semantic-decisions.md)に従う。
- **Phase 2:** C1〜C10とPhase 2のCAP-ATを完了条件とする。環境変数allow-listや現行sandboxだけでは完了でない。
- **Phase 3/4:** finite budget/controlとcooperative `Pending`は[実行予算・協調実行仕様](execution-control.md)を正本とし、Phase 2同期traitを置換しない。
- **Phase 6:** event enum、redaction、bounded journal、sink failureは[決定性・実行時監査仕様](determinism-and-audit.md)を正本とし、`FailClosed`以外や別event名を追加しない。
- **AUD-020:** canonicalize/check/useをportable path-handleへ置換し、dangling symlink、TOCTOU、存在oracleをCAP-AT-10〜14で検証。
- **AUD-018:** `args()`はrequest snapshotだけを読み、CLIが複数script引数を渡す。
- **AUD-049:** 単一catalogとCAP-AT-20を完了条件とする。
