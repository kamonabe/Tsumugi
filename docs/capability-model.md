# Tsumugi — Capability Model仕様

最終更新: 2026-10-31
設計ステータス: **実装仕様確定・実装進行中**（C1〜C10 実装済み。C6〔ModuleResolver／import 解決〕はPhase 2最小形としてtree経路のlink時解決を実装済み〔VM file/stdinのimportはC6-c/E9の一片としてlink層のEngine resolver gateへ統合済み——未grant + importはtreeと同じくterminal `Denied`、run層のimport展開は依然評価器内蔵`ModuleLoader`。VM backend adapter本体のconformanceはE9残。tree REPLはE7でEngine API統合済みだがimport解決は評価器内蔵`ModuleLoader`に委ねresolver gateを迂回する——従来のCWD相対import・固定エラー文字列を保全する既知の設計で、VM REPLはtree REPLと対称にalpha `ModuleLoader`を保全（gate迂回）。REPLのimport gateを塞ぐならE8b以降の課題、budget N境界はC11/Phase 3〕。詳細は [ロードマップ](roadmap.md) Phase 2 を参照）

本文書の §1〜§17・§19・§20 は **到達目標の確定仕様**であり、現在の実装状態ではない。現在の実装が目標仕様からどう
逸脱しているかは、下の **逸脱一覧** が唯一の正本である（[ドキュメント運用ガイド](documentation-guide.md)
「逸脱一覧の持ち方」）。表は現在の逸脱だけを持ち、解消した行は削除する。実装スライスの進捗の正本は
[`slices.toml`](slices.toml)。

逸脱一覧は loop:drift #263 の D-3 を受けて新設した。本文（目標）を実装に合わせて後退させず、実装との差を
逸脱として載せる案（Q2=A）は、返信なしのため推奨案で確定した（[開発ループ手順書](loop-runbook.md) §7.5、
issue #263 Q2=A）。理由: spec が実装の正本であり、adapter の context 引数・`Control` は Phase 3/4 の予算・協調実行
（[マニフェスト](manifesto.md) 原則 3・4）を受ける目標の形である。本文を実装に合わせるとその受け皿が spec から消える。載せたのは 2026-10-25 の整合点検で §3〜§11.1 を実装と照合して確かめた差であり、
照合していない節（§12・§14.2 以降）の逸脱は、事実を確かめられたものから順に載せる。表は現時点で #263 D-3 の (a)〜(d) と、issue #291 の設計で事実を確かめた 3 行（file/stdin の run 層の import 展開、scheduler の handle 経路の import、区切り文字だけの `TSUMUGI_SANDBOX`）を持つ（(e) は fix-host-function-id-unique で解消した）。上の設計ステータス段落にある現在の状態（§10 の範囲の REPL の resolver gate 迂回・run 層の import 展開・E9 の残りなど）と、§18「実装slice」の状態注記は、まだ表へ移していない。

## 逸脱一覧（目標仕様に対する現在の逸脱）

| 逸脱内容 | 対象の節(§) | 発生スライス |
|---|---|---|
| `AdapterError` の形が目標と違う。目標は `Host(HostError)`・`Control(ControlStop)`・`SecureResolutionUnsupported` の 3 variant で `Clone`/`Eq`/`PartialEq` を derive するが、実装（`src/capability.rs` の `AdapterError`）は `Host(String)`（`HostError` でなく文字列）で、目標に無い `DirectoryReadFailed`・`NonUtf8EntryName` を持ち、`#[non_exhaustive]` 付き、derive は `Debug` だけである | §4 | C4（`Host(String)`・`#[non_exhaustive]`・derive）、REV-009 §17.4（`DirectoryReadFailed`・`NonUtf8EntryName`） |
| `CapabilityCallContext` に `execution_id`・`check_control`・`reserve` が無い。実装（`src/capability.rs` の `CapabilityCallContext`）が持つのは `capabilities`・`deadline`・`cancellation`・`remaining_request_bytes`・`remaining_response_bytes`・`may_yield`・`call_id` である。渡されるのは cooperative adapter（`src/host_pending.rs` の `CooperativeAdapter::start`）・cooperative host function（`src/host_function.rs` の `CooperativeHostFunction::start`）・`ModuleResolver::resolve`・`ModuleSource::read_chunk`（`src/capability.rs`）で、下の行の同期 adapter と同期 `HostFunction::call` には渡されない | §4 | C12 / exec-control Slice 5 |
| 同期 adapter のメソッドが `&mut CapabilityCallContext` を取らない。実装（`src/capability.rs`）の `Clock::now_utc(&self)` は `Result<SystemTime, AdapterError>` でなく `SystemTime` を返し、`Input::read_line(&self)`・`Output::write_all(&self, bytes)`・`Output::flush(&self)`・`DirectoryHandle` と `FileHandle` の各メソッドも context 引数を持たない。`ReadLimit` 型は無く、`read_line` は上限引数を取らない | §6・§7・§8.3 | C3（Clock）、C4（Input/Output）、C5（DirectoryHandle/FileHandle） |
| host function の呼び出し境界が目標と違う。実装（`src/host_function.rs`）に `HostCallContext` と `charge_external` は無く、`HostFunction::call(&self, arguments)` は context を取らない。`HostCallError` は `Host { category: String }` の 1 variant だけで（`#[non_exhaustive]`、derive は `Debug`）、`Control(ControlStop)` が無い | §11.1 | C8 |
| file/stdin の run 層の import 展開が、link が resolver で確かめた source を使わない。tree（`Engine::run` → `Evaluator::begin_execution`）も VM（`src/main.rs` の `execute_vm_with_path`）も `Engine::link` の結果の `import_graph` を捨て、評価器内蔵 `ModuleLoader`（`src/module.rs`）が import 元スクリプトのディレクトリを基準にファイルを読み直す。このため link で確かめたファイルと実行するファイルが別物になりうる（import root の外の同名ファイルが実行される）。読み直したファイルの中の import は resolver を通らず、`TSUMUGI_SANDBOX`（`src/sandbox.rs`）の検査だけを受ける。`@NAME/...` は run 層で literal な相対 path として扱われ失敗する。imported module record の heap 課金は `ModuleId` でなく canonical path の byte 長で数える。issue #291 の再現 1 | §10・§14.1 | C6（tree）、E9（VM） |
| scheduler の handle 経路（`src/engine.rs` の `Engine::compile` / `create_execution` / `start`）は `Engine::link` と resolver を経ない。初回 poll の `Evaluator::begin_execution` で評価器内蔵 `ModuleLoader` が `ExecutionContext::set_script_path` のディレクトリ（未設定なら CWD）を基準にファイルを読み、import に `module_resolver` の grant を要さない（`TSUMUGI_SANDBOX` の検査だけを受ける）。`&LinkedScript` を取らない差は [実行予算・協調実行仕様](execution-control.md) の逸脱一覧（§9.1）にある | §10 | exec-control Slice 3 PR-a |
| legacy profile で `TSUMUGI_SANDBOX` が区切り文字と空白だけの値（`,`・` , ` など）のとき、CLI（`src/cli_capability.rs` の `build_legacy_sandbox_fs`）は host root が 0 個なのを未設定と同じに扱い、unrestricted filesystem を security warning なしで grant する。同じ値を `src/sandbox.rs` は空の許可リスト（全部拒否）と解釈する。issue #291 の再現 2 | §14.3 | C9/C10 |

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
| FsOperation | Read=`0x01`, Write=`0x02`, Create=`0x03`, Delete=`0x04`, Metadata=`0x05`, List=`0x06`, Import=`0x07`, RecursiveDelete=`0x08` |

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
    RecursiveDelete, // 再帰削除（`remove_tree`）。Delete から暗黙昇格しない
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

`FsOperation`の並びと名前は§3.1のtag（`RecursiveDelete`=`0x08`が末尾）・§8.4・§12と一致させる。`RecursiveDelete`はこの列挙から漏れていたため足した（loop:drift #263 D-4。§3.1・§8.4・§12に合わせる案〔Q3=A〕は返信なしのため推奨案で確定、[開発ループ手順書](loop-runbook.md) §7.5、issue #263 Q3=A）。理由: §3.1・§8.4・§12と実装はすでに`RecursiveDelete`を独立した操作として持ち、再帰削除を`Delete`と別に明示grantする形が[マニフェスト](manifesto.md)原則2に合う。

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
    ColonInComponent,
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

relative componentはUTF-8、1 byte以上、NULなし。`/`だけをseparatorとし、absolute `/`、`.`、`..`、空component、backslash、drive prefix、UNCを拒否する。あわせてcomponent内のASCII `:`（Windows ADS / stream指定）を`PathError::ColonInComponent`で拒否する。`@mount`経路（`FilesystemTarget::parse`）とlegacy translator（`LegacyFsTranslator::translate`のComponent::Normal）の両方で適用する。これは先頭drive prefix（`X:`）を表す`PathError::Absolute`とは別概念である（ADSは絶対pathではない）。host platform pathへ変換する前に検証する。`PathError`はcapabilityの有無を調べる前にcatch可能な`RuntimeErrorCode::Argument`へ変換するため、malformed pathとauthority不足のchannelは混在しない。parse後に該当mountがない場合はadapter/OS call 0で内部`Denial { code: ResourceNotGranted }`を生成し、script filesystem operationならcatch可能なcanonical `sandbox` errorへ変換する。

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
pub enum RemoveKind { FileOrSymlink, EmptyDirectory, Tree }

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
3. platformがroot拘束とsymlink policyを保証できなければ`SecureResolutionUnsupported`。文字列prefix checkへfallbackしない。script filesystem操作ではcanonical `host` error（code `secure_resolution_unsupported`）へ変換してcatch可能とし、未捕捉なら`RuntimeError`となる。link中のresolverで発生してscript handlerが存在しない場合だけterminal `HostError`へ変換する。対象fileのread/write/deleteは行わない。UnixとWindowsはsecure handleを実装する。Windowsではjunction / mount point等「stdがsymlinkと認識しないreparse point」を含むpathをfail closed（拒否、`AdapterError::Host("reparse point denied")`）し、安全を保証できる通常file/dir/symlinkのみ操作する。この拒否はsymlink policy判定より先に、中間component・open_file（finalが既存のとき）/ metadata / list / remove / renameのsrc・既存dstのいずれでも行う（将来、厳密なreparse tag判定でroot内junction許可を検討）。
4. `DenyAll`は途中/final symlinkを拒否。`FollowWithinRoot`は解決先を同じroot handleへ拘束。`OperateOnFinalEntry`は中間symlinkをroot内へ拘束し、delete/renameおよび`follow_final=false`のmetadataだけがfinal symlink entry自体を扱える。Read/Write/Create/Listはfinal symlinkを拒否する。
5. create/write/appendはdangling final symlinkを追従しない。targetをsecureにbindできなければ拒否。
6. renameはsource `Delete`、destination `Create`、replace時はdestination `Delete`も必要。両handle認可後、単一rename call。
7. case/Unicode aliasはadapterがfilesystem規則でroot内拘束する。Windowsのcase-insensitive aliasと8.3 short nameは`canonicalize`による実体収束でroot内拘束される（別名表記はroot脱出に使えない）。

### 8.4 Operation matrix

| Script operation | Read | Write | Create | Delete | Metadata | List | Import | RecursiveDelete |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| `read_file`, `read_lines` | ✓ |  |  |  |  |  |  |  |
| `write_file`, `append_file` |  | ✓ | ✓ |  |  |  |  |  |
| `mkdir` |  |  | ✓ |  |  |  |  |  |
| `remove`, `remove_dir` |  |  |  | ✓ |  |  |  |  |
| `remove_tree` |  |  |  |  |  |  |  | ✓ |
| exists/type/size |  |  |  |  | ✓ |  |  |  |
| `list_dir` |  |  |  |  | ✓ | ✓ |  |  |
| `rename` no replace |  |  | ✓(to) | ✓(from) |  |  |  |  |
| `rename` replace |  |  | ✓(to) | ✓(from+to) |  |  |  |  |
| Tsumugi import |  |  |  |  |  |  | ✓ |  |

builtin mappingは固定する。`read_file`/`read_lines`は`ReadExisting`、`write_file`は`Upsert { Truncate }`、`append_file`は`Upsert { Append }`を使う。upsertはfileの存在を調べずWrite+Createを事前要求する。現行言語に`WriteExisting`/`CreateNew`専用builtinはなく、これらは将来またはhost adapter用である。`remove`は`FileOrSymlink`、`remove_dir`は`EmptyDirectory`（空directoryのみ。非空は削除しない）を使う。再帰削除は`remove_tree`が`RemoveKind::Tree`＋専用`RecursiveDelete`権限で行い、`EmptyDirectory`から暗黙昇格しない（REV-021 §17.6）。final/中間symlinkはリンク先を辿らずリンク自体を削除する。`PublicMetadata`は時刻、owner、absolute pathを公開せず、`DirectoryEntry.name`が空、`.`、`..`、separator含有ならadapter contract violationとしてHostErrorにする。

`Import`はruntime `Read`と独立する。filesystem resolverが内部でImport rootを使う。Readだけでimportできず、Importだけで`read_file`できない。

### 8.5 Denialと存在oracle

lexical path syntaxはcapability lookupより先に検証し、`PathError`をcatch可能な`RuntimeErrorCode::Argument`へ変換する。syntaxが正しい場合だけcapability/mount/operationを調べ、不足時はadapter/OS call 0で内部`Denial`を生成する。script filesystem operationではsafe fieldだけからcatch可能なcanonical `sandbox` errorへ変換し、未捕捉なら`RuntimeError`とする。handler開始前のimport resolver denialだけはcatch不可terminal `Denied`である。存在path/不存在pathを同じpublic denial code/messageにし、absolute host path、symlink、permissionを含めない。grant済みroot内のnot-found等だけがcatch可能なcanonical `host` errorまたは既存言語意味論の`null`/`false`になれる。denyを`null`/`false`へ変換しない。

このargument error規定はscript runtimeのfilesystem operation用である。link層のmalformed import specifierにはscript handlerもcatchable channelも無いため適用せず、terminal `LinkError::Resolve(code: "invalid_import_specifier")`を採る（mount未登録はterminal `Denied`。§13も参照）。

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
    // link層がresolveより先にmount存在を確認するためのhook（未登録mount→terminal Denied）。
    // 既定trueで、mountの概念を持たないresolverは全specifierを自前で解決する前提。
    fn knows_mount(&self, _mount: &str) -> bool { true }
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

resolverはlink時だけ呼ぶ（C6-cで実装済み）。`CapabilityCallContext`はC3〜C5と同じPhase-2最小のgetter流儀（`deadline()`/`cancellation()`/`remaining_*_bytes()`等）で、§4/§6の`check_control`/`reserve`最終signatureはPhase 3で導入する。Engineはsourceを64 KiB以下のchunkで読み、Phase 3のmodule/total limit NならN+1を蓄積する前に停止する（Phase 2は器のみ・上限非適用）。chunkがmaxを超えたadapterは`HostErrorCode="resolver_contract_violation"`。完了bytesをUTF-8検証し、invalidなら`LinkError::InvalidModule`。

ModuleIdは同じlogical moduleへ同じIDを返す。同ID/異hashのlink errorは、owning docである[組み込みAPI仕様](embedding-api.md) §5.1のとおり`LinkError::Resolve`で拒否し後勝ちにしない。HTTP resolverをcoreへ内蔵しない。host adapterがnetwork/TLS/redirect/credential/timeout/size/cache/supply-chainを担う。

**runはlinkが読んだsourceだけを実行する。** `LinkedScript`は、linkがresolverから読みUTF-8検証とmodule単体compileを通した各moduleのsourceを、`ModuleId`ごとに内部で保持する（公開型`ImportGraph`/`ImportNode`のfieldは増やさない）。runはroot sourceと同じく保持したmodule sourceを実行スレッド上で再parseし（[組み込みAPI仕様](embedding-api.md) §4.1 案A）、`import`文をその内容で展開する。run層はfilesystem・CWD・importerのディレクトリからmoduleを読み直さず、resolverも呼ばない（EMB-AT-05）。linkで確かめたmoduleと実行するmoduleを1経路にすることで、確認と使用の間の差し替え（TOCTOU、[脅威モデル](threat-model.md) TM-003）が入り込む余地を無くす（[マニフェスト](manifesto.md) 原則2「権限はホストが明示的に付与する」、原則8「小さく理解可能な中核を保つ」）。

- module内の`import`もroot sourceの`import`と同じ規則でlink層が正規化しresolverへ渡す。specifierはimporterのディレクトリを基準にしない。unqualified pathはmount `default`のroot、`@NAME/...`はmount `NAME`のrootを基準にし、`..`・絶対pathはmalformed specifierとして`LinkError::Resolve`（code `invalid_import_specifier`）で拒否する（§14.1、§8.2の`FilesystemTarget`）。
- 展開の規則は[言語仕様](language-spec.md)「モジュール / import」に従う。`import`文の位置にmoduleのtop-level文を置き、同じ`ModuleId`は1つの`ExecutionContext`の中で（contextの再利用をまたいでも）1回だけ展開する。実行が完了しなかったexecutionが新しく展開したmoduleは未展開に戻し、次のexecutionで再び展開できる。
- source・importの本数とbytes、imported module recordのheapは[実行予算・協調実行仕様](execution-control.md) §5.1・§5.3のとおり`ModuleId`単位で課金する（module recordは96 + `ModuleId`のUTF-8 byte長）。課金はこれまでどおりrunの`Link`フェーズ（最初の文を実行する前）で行う。
- 展開中のどの`import`（rootにあるものもmoduleの中にあるものも）でも、specifierの正規化に失敗するか、`ModuleId`が`LinkedScript`の保持sourceに無いのは、linkを経ていない`LinkedScript`（例: `link_for_repl`の空graph）をrunへ渡した内部契約違反であり、scriptから見えるimport errorにせず`InternalFailure`にする。
- REPLのimportは本段落の対象外で、評価器内蔵`ModuleLoader`が解決する既知の設計のまま（冒頭の設計ステータス段落、[組み込みAPI仕様](embedding-api.md) §12）。

この段落はissue #291 のQ1=A（run層のimportはlinkで確認したmodule graphから読み、入れ子のimportもlink時にresolverの許可を通す。run層で`ModuleLoader`がファイルを読み直す経路をfile/stdinから無くす。REPLは範囲外）による。返信なしのため推奨案で確定した（[開発ループ手順書](loop-runbook.md) §7.5）。採らなかった案B（`ModuleLoader`を残しrun層にもresolverと同じroot検査を足す）は、確認と読み込みが2経路のまま残り、確認後の差し替えを防げないため採らない。

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
| `remove`, `remove_dir` | — | — | — | — | Delete | — | — | — |
| `remove_tree` | — | — | — | — | RecursiveDelete | — | — | — |
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

C6-cで上記4型（`Denial`/`DenialCode`/`OperationId`/`ResourceLabel`）を`src/capability.rs`に実装し、crate rootへre-exportした。`LinkError::Denied`と`ExecutionOutcome::Denied`は同一の`Denial`型を共有する（独自structを作らない）。import resolver不足時の固定値は`code: ResourceNotGranted` / `capability: ModuleResolver` / `operation: OperationId::new("module.resolve")` / `public_resource: None`で、`"module.resolve"`がcrate初出のfixed identifierである。

invalid pathはhost API misuseでなく、grantの有無を調べる前にcatch可能なcanonical argument errorとする。capability/mount/operation不足はOS/adapter call 0の`Denial`を生成する。script operationではそのsafe fieldだけからcanonical capability errorを作りcatch可能、link/control-planeではterminal `Denied`にする。いずれも存在、absolute path、environment value、callback detailを含めない。

この「invalid path → catch可能なargument error」規定はscript runtimeのfilesystem operation用であり、script handlerが存在しないlink層のmalformed import specifierには適用しない。link層はcatchable channel（scriptがtry/catchできる経路）を持たないため、malformed import specifierはargument errorへ落とさずterminal `LinkError::Resolve(code: "invalid_import_specifier")`を採る（mount未登録はterminal `Denied`。2 channelを混在させない）。

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
- `--allow-import-root NAME=PATH`はC6-cでfilesystem `ModuleResolver`を構築してsafe profileへgrantする（各NAMEをmount root、`SymlinkPolicy::DenyAll`で`OsDirectoryHandle`化）。これによりtree file/stdin経路のimportはlink時にEngine resolverで解決され、`@NAME/...`が実効化される（unqualified pathはmount `default`へrouting）。grantされるため`--allow-import-root`付きsafe setのCapabilitySetIdは`module_resolver` groupを含むように変わる。importがあるのに`--allow-import-root`を1つも渡さないtree実行は、resolver未grantでterminal `Denied`（exit 1、`--allow-import-root NAME=PATH`を促すactionable診断）になる——ambient相対パスimportは廃止した（原則2 明示的capability）。E9でVM file/stdinも同じlink層のEngine resolver gateへ統合され、未grant + importはtreeと同じくterminal `Denied`になる（`--allow-import-root`が`module_resolver`をgrantする経路もtree/VM共通）。run層はtree/VMともlinkが読んだsourceからimportを展開し、ファイルを読み直さない（§10）。したがって`@NAME/...`もrunまでend-to-endで効く。VM REPLは従来どおりalpha `ModuleLoader`（import root不要）でtree REPLと対称。tree/VMの「既知差」はfile/stdinについては解消され、REPLにのみ残る。
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
- sandbox未設定/空: legacy専用`LegacyFilesystemTranslator`へunrestricted filesystem namespace authorityを明示grantしsecurity warningをstderrへ1件出す。Unixは`/`とsnapshot CWD handle、Windowsは要求されたvolume rootとsnapshot CWD handleへ安全なhandle操作を使う。authority自体がunrestrictedなだけでcheck/use文字列fallbackは使わない。ここでの「空」は値が空文字列（0 byte）のときだけを指す。
- sandbox設定ありでhost rootが0個（実質空）: 値は空文字列でないが、comma区切りの各要素をtrimすると空しか残らないもの（`,`・` , `・空白だけなど）は、未設定とみなさず**空の許可リスト**として扱う。filesystem capabilityをgrantせず、script filesystem操作はすべてI/O前に§8.5の単一denial（catch可能なcanonical `sandbox` error）になる。security warningをstderrへ1件出す（文言は「全体を許可」でなく「すべて拒否」を告げるもの）。REPLのimportが通る`sandbox.rs`も同じ値を空の許可リストと解釈するので、CLIのfilesystemとREPLのimportで意味が一致する。値を書いたのに全開放になる驚きを残さず、明示的に付与しない限り権限は無い（[マニフェスト](manifesto.md) 原則2、fail-closed）。issue #291 のQ2=A（返信なしのため推奨案で確定、[開発ループ手順書](loop-runbook.md) §7.5）による。採らなかった案B（未設定と同じunrestrictedにし警告だけ必ず出す）は、値を書いたのに全開放になる驚きが残るため採らない。
- env allow設定あり: comma-separated exact keyまたは末尾`*` prefixだけsnapshot。未設定/空は`TSUMUGI_`以外を全snapshotしwarning 1件。
- translatorは旧absolute pathをnamespace root handle、旧relative pathをstart時CWD handleへ変換する。legacy filesystemは`OsDirectoryHandle` + `SymlinkPolicy::FollowWithinRoot`で構築し、旧`sandbox.rs`のcanonicalizeベースsymlink解決をroot拘束handle追従へ具体化する（symlink追従自体は互換維持。中間component symlinkはroot内へ拘束して追従し、root外へ逃げるものと final entry symlinkはhandle契約どおり拒否する）。safe profileは常に`DenyAll`、legacy profileは`FollowWithinRoot`とsymlink policyが異なる。root選択（candidate生成・component数比較）はCWD基準で絶対化したlexical path（symlink非追従）で行いcanonicalizeしない。Unix / Windowsはsecure handleを実装済みで、Windowsは§8.3契約3のreparse fail-closed（junction / mount point拒否）を伴う。secure handleを実装できないplatform（非Unix・非Windows）ではfilesystem capability構築を拒否。
- legacy authorityは`ExecutionStarted.capability_policy_hash`と通常のcanonical decision/host-call eventで観測する。warning専用の別audit eventを追加しない。Phase 2完了条件はstderr warning、Phase 6では同じ8 event schemaとfail-closedを使う。

### 14.4 Release移行

| Release | default | 旧環境変数 |
|---|---|---|
| N-1 | legacy | 読む。起動時deprecation warning。safeを選択可 |
| N | safe | safeでは無視。legacyは明示flag必須 |
| N+1 | safe | coreから直接参照削除済み。legacy adapter継続可否は別判断 |

`OnceLock` process-global policyをper-execution capabilityへ再利用しない。

**本revision（C9/C10一体実装）の採択**: ambient読みをcoreから全除去したうえでCLIを構築するため、N-1（旧envをcore/CLIが既定で読む段階）には戻れない。Tsumugiはalphaで後方互換を保証しないため、N-1を飛ばして**safe既定（N相当）**を採る。legacyは`--profile legacy`指定時のみCLI adapterが旧envをその場で1回読む（process-global OnceLock不使用）。この結果、CAP-AT-25のgoldenはrelease間default差ではなく「無指定=safe」vs「`--profile legacy`」の**profile選択差2点**に縮退する（N-1不在のため）。

**migration warning（deprecation warning 段階）の正式 descope**: 上記のとおりN-1段階を飛ばすため、N-1でのみ意味を持つ起動時 deprecation / migration warning（legacy 既定を前提に safe への移行を促すメッセージ）は本 revision では役割がない。したがって migration warning は**実装しない（正式に descope する）**。これは[組み込みAPI仕様](embedding-api.md) 第14〜16節の E8b 受入項目「migration warning」の descope に対応し、E8b の Phase 2 受入条件は EMB-AT-10/11/16・CAP-AT-23〜26 へ縮退する（migration warning を外す）。legacy 選択時に移行を示す別警告は追加せず、legacy profile が既に出す全アクセス許可の security warning（§14.3、空 sandbox / 空 env-allow の各 1 件）で十分とする。legacy は safe が既定となった本 revision でも `--profile legacy` 明示時の互換 authority として機能し続けるため、「将来削除予定」を告げる deprecation は現時点では過剰である。

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

各スライス小節はそのスライスを実装した時点の記録であり、書き換えない。現在の逸脱・未実装範囲は冒頭の逸脱一覧を見ること。

| Slice | Phase | 内容 | 完了条件 |
|---|---:|---|---|
| C1 | 2 | CapabilitySet/Denial/dispatcher/empty default | adapter call前deny |
| C2 | 2 | CallableCatalog統合 | AUD-049 |
| C3 | 2 | Environment/args/Clock | ambient process read 0 |
| C4 | 2 | Input/Output trait | deny/EOF/HostError分離 |
| C5 | 2 | mount routing/DirectoryHandle/FileHandle | path/symlink/oracle |
| C6 | 2 | ModuleResolver/stream/link接続（実装済み。tree経路のimportをlink時resolverで解決、runtime resolver 0） | runtime resolver 0 |
| C7 | 2 | ProcessExit | structured Exited |
| C8 | 2 | HostFunctionRegistry metadata/grant | name/arity/redaction/panic |
| C9 | 2 | CLI safe/legacy | AUD-018（migration warning は §14.4 の N-1 スキップ採択により正式に descope——不実装） |
| C10 | 2 | ambient実装削除 | core直接OS access 0 |
| C11 | 3 | finite meter/deadline/runtime cancel/cost | N境界、reservation |
| C12 | 4 | cooperative adapter/ticket/waker | Ready/Pending、backpressure、cancel |
| C13 | 6 | canonical audit sink/event/sequence | 8 event、fail-closed、correlation |

C1→C2、C3/C4/C5/C7を並行、C6はstream後、C8はBuiltinSpec衝突検査後、C9/C10を最後に行う。C11〜C13をPhase 2完了条件へ含めないが、Phase 2のpublic signatureは後続最終型と競合させない。

### fix-host-function-id-unique 実装スライス（cooperative 登録での host function ID 一意性検査）

§11.1 は「host function ID は registry 内で一意でなければならない」と定め、違反は build error とする。
C12（[実行予算・協調実行仕様](execution-control.md) Slice 5）が入れた `HostFunctionRegistryBuilder::register_cooperative`
（`src/host_function.rs`）は名前の衝突だけを検査し、ID の重複を検査しない（逸脱一覧の該当行、loop:drift #263 の D-3 (e)）。
cooperative host function の grant 検査は descriptor の ID で行う（`resolve_cooperative_call` の
`capabilities.contains_host_function(descriptor.id)`）ため、ID が重複すると 1 つの grant で別名の function まで
呼べてしまう。これは §2 原則 5（HostFunction ごとに別 authority）と [マニフェスト](manifesto.md) 原則 2
（権限はホストが明示的に付与する）に反する。本スライスはこの検査漏れを直す。

- **実装範囲**:
  1. `register_cooperative` は、名前の検査（keyword・core/context builtin・同期／cooperative の登録名との衝突）の
     **後に**、descriptor の ID が登録済みの同期 function・cooperative function のどちらかと重複していれば
     build error を返す。
  2. `register` の ID 重複検査を、登録済みの同期 function に加えて登録済みの cooperative function の ID にも広げる。
     これで登録の順序（同期が先か cooperative が先か）によらず、registry 内の ID がすべて一意になる。
  3. error は既存の同期同士の ID 重複と同じ `ConfigError::DuplicateCapability { kind: CapabilityKind::HostFunction }`
     とする（新しい variant を足さない。[組み込みAPI仕様](embedding-api.md) の `ConfigError` は変えない）。
     名前と ID の両方が重複する場合は、既存の `register` と同じく名前の検査が先に効き
     `ConfigError::DuplicateCallableName` を返す。
  4. cooperative の ID は builder の内部だけで持つ（例: builder の private な `BTreeSet<HostFunctionId>`）。
     `HostFunctionRegistry` の公開 API・格納構造・lookup の挙動は変えない。
  5. `register`・`register_cooperative` の doc comment を、ID 一意性を同期・cooperative の両方で検査することに合わせて直す。
- **範囲外（逸脱として残すもの）**:
  - `HostFunctionRegistry::get(id)` で cooperative function を引けるようにすること（§11.1 の `get` は同期 `HostFunction` を返す型で、
    cooperative を ID で引く API は spec に無い）。
  - 逸脱一覧の他の行（`HostCallContext`・`HostCallError::Control`・`CapabilityCallContext` の不足フィールドなど）。
  - §17 の Engine build 時の一括衝突検査・VM 経路（VM は cooperative を解決しない）。
  - grant 側（`CapabilitySetBuilder::grant_host_function`）の検査は既に同 ID の二重 grant を拒否しており変えない。
- **観測される変化**: これまで登録できていた「ID が重複する cooperative 登録」が build error になる。
  ID が一意な登録（既存のテスト・CLI の経路はすべてこれ）の挙動は変えない。言語挙動・fuel・heap・監査の値は変えない。
- **受入基準（fix-host-function-id-unique）**:
  - **HFID-AT-01**（cooperative 同士）: ID が同じで名前が異なる cooperative function を 2 つ登録すると、2 つ目の
    `register_cooperative` が `ConfigError::DuplicateCapability { kind: HostFunction }` を返す。
    確かめ方: `src/host_function.rs` の unit test で、Ready だけを返す最小の `CooperativeHostFunction` を 2 つ登録し、`err()` の variant を照合する。
  - **HFID-AT-02**（同期 → cooperative）: 同期 function を登録した後、同じ ID・別名の cooperative function を登録すると
    `register_cooperative` が HFID-AT-01 と同じ error を返す。
    確かめ方: 同じ unit test 群で、既存の `echo_fn` の後に同 ID の cooperative を登録して照合する。
  - **HFID-AT-03**（cooperative → 同期）: cooperative function を登録した後、同じ ID・別名の同期 function を登録すると
    `register` が HFID-AT-01 と同じ error を返す。
    確かめ方: HFID-AT-02 の逆順の unit test。
  - **HFID-AT-04**（優先順位と正常系）: 名前と ID の両方が重複する cooperative 登録は `ConfigError::DuplicateCallableName` を返す。
    ID も名前も異なる同期・cooperative の混在登録は成功し、`build()` 後に両方の名前が解決できる
    （`contains_name`・`lookup_by_name`・`lookup_cooperative`）。
    確かめ方: unit test で、名前・ID 二重重複の error variant と、混在登録後の lookup 結果を照合する。
  - 既存のテスト（`register_rejects_duplicate_name_and_id`、`tests/engine_api.rs`・`tests/scheduler_api.rs` の cooperative 登録）は期待値を変えずに通る。
- **想定の変更規模**: `src/host_function.rs` だけ。本体（`#[cfg(test)]` を除く）は +20〜30 行程度
  （builder の field と初期化、`register` の ID 検査の拡張、`register_cooperative` の ID 検査と記録、doc comment）。
  テストは `src/host_function.rs` の `#[cfg(test)]` に +80〜120 行程度（最小の cooperative stub と HFID-AT-01〜04）。
  触る仕組みは host function registry の builder 1 つ、受入基準は 4 項目で、[開発ループ手順書](loop-runbook.md) §11.1 に収まる。
  done にする実装 PR で、逸脱一覧の「host function ID の registry 内一意性を cooperative 登録で検査しない」行を削除する。

### fix-import-run-from-link-graph 実装スライス（run 層の import を link で確かめた graph から展開）

issue #291 の再現 1 を直す。§10 は「resolver は link 時だけ呼ぶ」とし、[組み込みAPI仕様](embedding-api.md) §5 は
全 import を実行前に resolver capability で解決するとする。ところが file/stdin の run 層は `Engine::link` の結果の
`import_graph` を捨て、評価器内蔵 `ModuleLoader`（`src/module.rs`）が import 元スクリプトのディレクトリを基準に
ファイルを読み直す（逸脱一覧の該当行）。このため `--allow-import-root default=/libs` で `/libs/a.tsg` を確かめたのに、
スクリプトの隣の `a.tsg` が実行され、その中の `import "../secret/s.tsg"` は resolver を通らずに読まれる。
§2 原則 4・5 と [マニフェスト](manifesto.md) 原則 2 に反する。直し方は issue #291 の Q1=A（§10 の「run は link が読んだ
source だけを実行する」の段落）で確定している。本スライスは run 層の読み込み元を link の graph に差し替える。
展開・重複除去・巻き戻し・課金の規則は変えない。

- **実装範囲**:
  1. `LinkedScript` の内部（`LinkedScriptInner`）に、link が resolver から読み UTF-8 検証と module 単体 compile を通した
     各 module の source を `ModuleId` ごとに保持する（例: `BTreeMap<ModuleId, Arc<str>>`。`Send + Sync` を保つ）。
     `resolve_module_imports` が既に持っている `text` を入れる。公開型 `ImportGraph`・`ImportNode` の field、
     `graph_hash`・`script_hash` の計算（§5.1 の byte 列）は変えない。
  2. root の `Program` と 1 の保持 source から、`import` 文を展開した linked program を作る関数を置く（`src/module.rs`）。
     specifier の正規化は link 層（`Engine::normalize_specifier` の `FilesystemTarget::parse` と `@mount/...` の組み立て）と
     同じ関数を共有し、2 か所に別実装を置かない。module source は実行スレッド上で再 parse する（embedding-api §4.1 案 A）。
     展開の規則は `ModuleLoader::link_into` と同じ（`import` 文の位置に module の top-level 文を置く、展開済みの
     `ModuleId` は skip、展開した module の中の `import` も同じ関数で再帰的に展開）。展開済みの集合は `ModuleId` を
     キーにし、`ExecutionContext` の評価器が持つ（context の再利用をまたいで保持し、`clear_user_state` で捨てる）。
     展開中のどの `import`（root にあるものも module の中にあるものも）でも、specifier の正規化に失敗するか、`ModuleId` が
     保持 source に無いときは `InternalFailure` にする（§10。例: `link_for_repl` の空 graph を `Engine::run` に渡したとき）。
  3. tree の `Engine::run`・`run_audited`（`src/embedding.rs` から `Evaluator::begin_execution` を呼ぶ経路）は、
     2 の linked program を使い、評価器の `ModuleLoader` を通さない。`run_for_repl` と、scheduler の handle 経路
     （`src/engine.rs` の `ExecutionHandle::poll` → `drive_one_slice` から呼ぶ `begin_execution`）は、従来どおり `ModuleLoader` を使い、
     観測挙動を変えない（`begin_execution` に読み込み元を選ぶ引数か別入口を足す。どちらにするかは実装の判断でよい）。
     scheduler の handle 経路の import を確かめるテストは `tests/scheduler_api.rs` に無いので、`begin_execution` を
     その場で差し替えると壊れても検出されない。既定の読み込み元を `ModuleLoader` のままにし、graph を使うのは
     `Engine::run`・`run_audited` が明示的に選んだときだけにする。
  4. VM file/stdin（`src/main.rs` の `execute_vm_with_path`）は `engine.link` の結果を捨てずに持ち、2 の関数で展開した
     program を `Compiler` に渡す。`ModuleLoader` をこの経路から外す。binary（別 crate）から 2 の関数と保持 source を
     使うための入口は、`link_for_repl` と同じく `#[doc(hidden)]` の CLI 内部 shim とし、外部 embedder 向けの公開契約を増やさない。
  5. 課金（`Evaluator::charge_link`・`Vm::charge_link`）は、展開で新しく展開した module ごとに、保持 source の byte 長で
     `charge_source`・`charge_import` を行い、imported module record を 96 + `ModuleId` の UTF-8 byte 長で heap に課金する
     （[実行予算・協調実行仕様](execution-control.md) §5.1・§5.3）。tree は record token を展開済みの集合と同じ寿命で持ち、
     実行が完了しなかったときに展開済みの集合から外す（`ModuleLoader::forget` と同じ巻き戻し）。VM は main.rs の
     実行の寿命で持つ。REPL の `ModuleLoader` 経路の課金（canonical path の byte 長）は変えない。
  6. doc comment を新しい経路に合わせて直す（`src/sandbox.rs` 冒頭の「run 層の実 import 展開は依然 ModuleLoader」、
     `src/module.rs` 冒頭、`Engine::link_for_repl` の「`Engine::run` は `import_graph` を参照せず」、`main.rs` の
     「gate 通過の確認だけに使い、LinkedScript は破棄する」、`ExecutionContext::set_script_path`）。`set_script_path` は
     公開 API なので残す（`Engine::run` の import には効かなくなったことを doc に書く）。
- **範囲外（逸脱として残すもの）**:
  - tree REPL・VM REPL の import（評価器内蔵 `ModuleLoader`・CWD 相対・`sandbox.rs` / `TSUMUGI_SANDBOX` を読む。§10、
    設計ステータス段落）。REPL 用に `ModuleLoader` と `sandbox.rs` は残す。
  - scheduler の handle 経路（`src/engine.rs` の `Engine::create_execution` / `start`）。`&CompiledScript` を取り link を経ないため、
    展開できる graph が無い（逸脱一覧の scheduler の handle 経路の行。[実行予算・協調実行仕様](execution-control.md) §9.1 の
    `&LinkedScript` を取る形へ移すときに合わせて直す）。
  - link の `Cycle` と [言語仕様](language-spec.md)「循環 import」（skip して成功）の食い違い。link は既に循環を `LinkError::Cycle` で
    拒否しており（`tests/fixtures/import_circular.expected_err`）、本スライスは link の判定を変えない。
  - 課金を run の `Link` フェーズから `Engine::link` の `LinkRequest.budget` へ移すこと（embedding-api §5 の目標）。
  - VM backend adapter 本体（E9 残）。
- **観測される変化**:
  - file/stdin の import は、link が import root の下で読んだ内容だけを実行する。スクリプトの隣にある同名ファイルは読まれない。
  - import root の下にだけあるモジュール（スクリプトのディレクトリから同じ相対 path で見えないもの）が読めるようになる。
    `@NAME/...` の import がrunまで通る（`tests/cli_safe_profile.rs::at_syntax_import_fails_in_both_engines` の期待値を
    成功へ反転し、名前を `at_syntax_import_runs_in_both_engines` に変える。`tests/differential.rs` のこのテストへの参照コメントも直す）。
  - module の中の `import "b.tsg"` は、その module のディレクトリでなく mount `default` の root を基準にする（link が既に
    そう解決している。run が読む内容が link と一致するようになる）。
  - safe profile の file/stdin の import は `TSUMUGI_SANDBOX` を読まない。
  - imported module record の heap 課金が canonical path の byte 長から `ModuleId` の byte 長になり、import を含む実行の
    `heap_bytes` の値が変わる（§5.1 の目標に合わせる変化）。source/import の本数と bytes は同じファイル内容なら変わらない。
  - 上記以外の既存テストの期待値は変えない。変える必要が出たら実装を止めて判断待ちにする。
- **受入基準（fix-import-run-from-link-graph）**:
  - **ILG-AT-01**（issue の再現）: issue #291 の再現 1 の配置（`libs/a.tsg`、`evil/a.tsg`、`secret/s.tsg`、`evil/main.tsg`）で
    `--allow-import-root default=<libs>` を付けて `evil/main.tsg` を実行すると、`libs/a.tsg` の `who()` の結果が出力され、
    `evil/a.tsg` と `secret/s.tsg` の内容は実行されない。tree と `--vm` の両方。
    確かめ方: `tests/cli_safe_profile.rs` の subprocess test で、stdout が libs 側の文字列だけであること、`EVIL`・`SECRET` を含まないことを照合する。
  - **ILG-AT-02**（読み直さない・差し替えに強い）: (a) on-disk に無い module を返す in-memory resolver で link した `LinkedScript` を
    `Engine::run` すると、root が import した名前を使えて `Completed` になり、run の間の resolver call は 0。
    (b) `FilesystemModuleResolver` で tempdir の module を link した後、run の前にそのファイルを別内容へ書き換えても（削除しても）、
    run は link 時の内容を実行する。
    確かめ方: `src/embedding.rs` の unit test（既存の `CountingResolver` を使う (a) と、tempdir を使う (b)）で、評価結果の
    binding（または capture した stdout）と resolver の call 数を照合する。
  - **ILG-AT-03**（path の規則と展開の規則）: (a) `default` root の `sub/a.tsg` が `import "b.tsg"` を持つとき、root の
    `b.tsg` が展開され、`sub/b.tsg` は読まれない。(b) `@NAME/...` の import が tree・VM ともに成功し、名前を使える。
    (c) 2 つの module が同じ module を import する菱形で、共有 module の top-level 文は 1 回だけ、最初の `import` の位置で実行される。
    (d) 同じ `ExecutionContext` で同じ module を import する root を 2 回 run すると、2 回目は展開しない。1 回目が
    `RuntimeError` で終わった場合は 2 回目に再び展開する。
    確かめ方: (a)(b)(c) は `tests/cli_safe_profile.rs` の subprocess test（tree / `--vm`）で stdout を照合する。(d) は
    `src/embedding.rs` の unit test で、module の top-level の `print`（capture した stdout）の回数を照合する。
  - **ILG-AT-04**（`TSUMUGI_SANDBOX` を読まない、REPL は不変）: safe profile で `TSUMUGI_SANDBOX` を import root と関係の無い
    ディレクトリに設定しても、file/stdin の import は成功する（tree / `--vm`）。REPL の import の既存テスト
    （`tree_repl_retries_failed_import`・`repl_imported_names_resolve_across_submissions_in_both_engines` ほか）は期待値を変えずに通る。
    確かめ方: `tests/cli_safe_ignores_legacy_env.rs`（または `tests/cli_safe_profile.rs`）の subprocess test で exit code と stdout を照合し、REPL は既存テストで確かめる。
  - **ILG-AT-05**（課金）: import を 2 本含む graph で、`Engine::run` 後の `BudgetUsage` の `source_count`・`source_bytes`・
    `import_count`・`import_bytes` が root と保持 source の byte 長どおりで、imported module record の heap 課金が
    96 + `ModuleId` の byte 長になる。`TSUMUGI_MAX_IMPORT_COUNT=0` などの N+1 拒否は 1 文も実行せずに起きる（既存の
    `import_count_limit_gates_file_execution` ほかが期待値を変えずに通る）。tree と VM で source/import の値が一致する。
    確かめ方: `src/embedding.rs` の unit test で `BudgetUsage` の各値を期待値と照合し、heap は record 1 件分の差を
    `ModuleId` の長さの違う 2 つの graph で比べる。tree/VM の一致は `tests/differential.rs` の既存 `cap_at_17_source_count_charge_parity` で確かめる。
- **想定の変更規模**: 本体（`#[cfg(test)]` を除く `src/`）は +250〜450 行・−50〜100 行程度。内訳の目安は
  `src/module.rs`（graph からの展開関数と正規化の共有、+100〜150）、`src/embedding.rs`（保持 source、run 経路の差し替え、
  doc(hidden) の入口、doc comment、+60〜120）、`src/eval.rs`（`begin_execution` の読み込み元の切り替え、`ModuleId` キーの
  展開済み集合と record token、+50〜100）、`src/vm.rs`（`charge_link` の入力を `ModuleId` 版に、+10〜30）、`src/main.rs`
  （VM 経路、+15〜30・−15）、`src/sandbox.rs`（doc comment）。テストは unit / subprocess を合わせて +300〜450 行程度。
  触る仕組みは run 層の import 展開 1 つ（tree と VM の両入口を同じ関数で揃える）、受入基準は 5 項目で、
  [開発ループ手順書](loop-runbook.md) §11.1 に収まる。
  done にする実装 PR で、逸脱一覧の「file/stdin の run 層の import 展開が、link が resolver で確かめた source を使わない」行を削除し、
  冒頭の設計ステータス段落の「run層のimport展開は依然評価器内蔵`ModuleLoader`」を REPL と scheduler の handle 経路だけに直す。
  [言語仕様](language-spec.md)「現在の既知非適合」の issue #291 の項目を削除し、`TSUMUGI_SANDBOX` の項目を REPL だけに直す
  （同「ファイルI/Oサンドボックス」の本文は、この設計 PR で file/stdin の import が `TSUMUGI_SANDBOX` を読まない目標の記述に直してある）。
  REPL の imported module record が canonical path の byte 長で課金される差は、[実行予算・協調実行仕様](execution-control.md) の
  逸脱一覧に事実を確かめて行を足す。

### fix-legacy-sandbox-empty-list 実装スライス（区切り文字だけの `TSUMUGI_SANDBOX` を空の許可リストにする）

issue #291 の再現 2 を直す。legacy profile で `TSUMUGI_SANDBOX=","` のように区切り文字と空白だけを書くと、CLI
（`src/cli_capability.rs` の `build_legacy_sandbox_fs`）は host root が 0 個なのを未設定と同じに扱い、ファイルシステム全体を
security warning なしで許可する（逸脱一覧の該当行）。同じ値を `src/sandbox.rs` は空の許可リスト（全部拒否）と解釈する。
直し方は issue #291 の Q2=A（§14.3 の「sandbox設定ありでhost rootが0個」の項目）で確定している。

- **実装範囲**:
  1. `build_legacy_capabilities`（`src/cli_capability.rs`）で、`TSUMUGI_SANDBOX` が空文字列でなく、comma 区切りの各要素を trim すると
     空しか残らないときは、filesystem capability を grant しない（`builder.filesystem(...)` を呼ばない）。
     `build_legacy_sandbox_fs` の「`hosts.is_empty()` なら `build_legacy_unrestricted_fs()`」の分岐は削除する
     （呼び出し側で 0 個を先に分けるので到達しない。到達したら profile 構築 error にしてよい）。
  2. 1 のとき、security warning を stderr へ 1 件出す。文言は次の定数として置く（`WARN_EMPTY_SANDBOX` と並べて公開する）。
     `警告: TSUMUGI_SANDBOX にパスが含まれていないため、legacy profile はファイルシステム操作をすべて拒否します`
     未設定・空文字列のときの `WARN_EMPTY_SANDBOX` と unrestricted の grant は変えない。
  3. `TSUMUGI_SANDBOX` を読むのは従来どおり CLI adapter が 1 回だけ（§14.3）。`src/sandbox.rs` の解釈は変えない
     （既に空の許可リストとして全部拒否する）。
- **範囲外（逸脱として残すもの）**:
  - 空文字列の `TSUMUGI_SANDBOX`（未設定と同じ unrestricted + warning、§14.3 の「sandbox未設定/空」）。
  - `TSUMUGI_ENV_ALLOW` の区切り文字だけの値の扱い（issue #291 の対象外。spec にも判断が無い）。
  - REPL の import が `sandbox.rs`（process 全体の `OnceLock`）を読む設計（fix-import-run-from-link-graph の範囲外と同じ）。
- **観測される変化**: legacy profile で区切り文字と空白だけの `TSUMUGI_SANDBOX` を書くと、script の filesystem 操作が
  すべて catch 可能な `sandbox` error（`ファイルシステム操作が許可されていません`）になり、stderr に 2 の警告が 1 行出る。
  filesystem を grant しないため、その場合の legacy の `CapabilitySetId` は filesystem group を含まない。
  それ以外の値（未設定・空文字列・path を 1 つ以上含む値）の挙動、safe profile の挙動は変えない。
- **受入基準（fix-legacy-sandbox-empty-list）**:
  - **LSE-AT-01**（全部拒否）: `--profile legacy` と `TSUMUGI_SANDBOX` = `,`・` , `・` `（空白 1 つ）のそれぞれで、
    実在するファイルの `read_file` が `ファイルシステム操作が許可されていません` で終わり（exit 1、ファイルの内容は出ない）、
    同じ呼び出しを `try` / `catch` で囲むと catch でき、後続の `print` が実行される。tree と `--vm` の両方。
    確かめ方: `tests/cli_legacy_profile.rs` の subprocess test で stdout・stderr・exit code を照合する。
  - **LSE-AT-02**（警告）: LSE-AT-01 の各値で、stderr に 2 の警告がちょうど 1 行出て、`WARN_EMPTY_SANDBOX` は出ない。
    未設定と空文字列では従来どおり `WARN_EMPTY_SANDBOX` が 1 行出て、filesystem が unrestricted のまま（既存テストが期待値を変えずに通る）。
    確かめ方: 同じ subprocess test で警告行を数え、既存の `tests/cli_legacy_profile.rs`・`tests/cli_default_profile_golden.rs` を通す。
  - **LSE-AT-03**（CLI の判定）: `TSUMUGI_SANDBOX` の値から legacy の filesystem grant と sandbox 側の警告を決める部分を、
    値を引数で受け取る内部関数（例: `legacy_filesystem_for(spec: Option<&str>)`）に切り出し、`,`・` , `・` ` では
    「grant 無し + 2 の警告 1 件」、未設定・空文字列では「unrestricted + `WARN_EMPTY_SANDBOX` 1 件」、path を含む値では
    「mount 構築 + 警告無し」を返す。
    確かめ方: `src/cli_capability.rs` の unit test で各値の戻り値を照合する。process env を書き換えるテストにしない（並列テストに敵対的なため）。
- **想定の変更規模**: 本体（`#[cfg(test)]` を除く `src/`）は `src/cli_capability.rs` だけで +20〜40 行・−5〜10 行程度
  （0 個の判定、警告の定数、`build_legacy_sandbox_fs` の分岐の削除、テストで値を注入できる内部関数への切り出し）。
  テストは `tests/cli_legacy_profile.rs` と `src/cli_capability.rs` の `#[cfg(test)]` に +80〜140 行程度。触る仕組みは legacy profile の
  filesystem 構築 1 つ、受入基準は 3 項目で、[開発ループ手順書](loop-runbook.md) §11.1 に収まる。
  done にする実装 PR で、逸脱一覧の「legacy profile で `TSUMUGI_SANDBOX` が区切り文字と空白だけの値のとき」の行を削除する。

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
| CAP-AT-16 | 2 | importありresolverなしはresolver call 0のterminal Denied。grant時linkだけ、run 0（C6-cで実装・テスト済み: `cap_at_16_*` / `emb_at_05_run_does_not_call_resolver`） |
| CAP-AT-17 | 3 | source N-1/N成功、N+1はchunk蓄積前拒否 |
| CAP-AT-18 | 2 | exit capabilityなしはprocess/OS call 0のcatch可能capability error（未捕捉時RuntimeError）、あり0/255 Exited、-1/256 RuntimeError、process継続 |
| CAP-AT-19 | 2/3 | Phase 2でregistered/granted 4組合せとarity。ungrantedはcallback 0のcatch可能capability error（未捕捉時RuntimeError）。Phase 3でfuel N-1/N/N+1 |
| CAP-AT-20 | 2 | catalog/tree/VM/compiler/generated docsの名前・arity・metadata完全一致、重複build error |
| CAP-AT-21 | 2/3 | callback success/catch可能host error/panicを分離。Phase 3でresult N+1 BudgetExceeded。link前host failureはterminal HostError |
| CAP-AT-22 | 2/6 | Omit/TypeOnly/LengthOnly serializerにfake secret本文0。Phase 6でsink eventも同じ |
| CAP-AT-23 | 2 | safe profileでroot source読込みとprofile builderが明示grantしたstdout以外ambient call 0。`--deny-stdout`時はstdout call 0。`--fs-op`は6トークン対応操作のみmapping一致（RecursiveDelete/ImportはCLI非対象で`remove_tree`は常にdeny）。C6-cで`--allow-import-root`は`module_resolver`をgrantするためset IDが変化し、tree経路の`@NAME/...`がlink層で実効化される（unqualifiedは`default`へrouting。run層はlink層が読んだsourceを展開するので、`@NAME/...`のimportはrunまでend-to-endで実行できる。§10）。E9で同じgrant経路はVM file/stdinにも効く（VM safe setも`--allow-import-root`で`module_resolver` groupを含むset IDになる） |
| CAP-AT-24 | 2/6 | legacy互換、empty sandbox/env allowはstderr warning各1。区切り文字と空白だけの`TSUMUGI_SANDBOX`（実質空）はfilesystem grant 0で全filesystem操作がcatch可能sandbox error、stderr warning 1（§14.3）。legacy symlinkは`FollowWithinRoot`、root選択曖昧はprofile構築error（exit 1）。Phase 6でも別eventを追加せずcanonical schemaとfail-closedを維持 |
| CAP-AT-25 | N | 本revisionはN-1を飛ばしsafe既定（N相当）を採るため、golden は「無指定=safe」vs「`--profile legacy`」のprofile選択差2点へ縮退して固定する |
| CAP-AT-26 | 2 | safeで旧環境変数変更がset ID/挙動へ影響0。射程はruntime fs/env/clock/stdin/exit/stdoutのcapability経路に限る。import解決はlink層（capability gate）とrun層（実import展開）の2層で動く。C6-cでtree file/stdinの、E9でVM file/stdinの**link層**import gateがEngine resolver（capability）へ移行し`sandbox.rs`/`TSUMUGI_SANDBOX`非依存になった。**run層**もlink層が読んだsourceから展開する（§10）ので、tree/VM file/stdinのimportは`sandbox.rs`/`TSUMUGI_SANDBOX`に依存しない。`sandbox.rs`/`TSUMUGI_SANDBOX`依存はtree REPL・VM REPLの`ModuleLoader`経路だけに残る（既知の設計、設計ステータス段落）。本fixtureは引き続きimportを含めない |
| CAP-AT-27 | 2 | adapter/CLI以外のprocess env、runtime fs、stdio、process exit、ambient clock直接利用0 |
| CAP-AT-28 | 2 | CapabilitySetId/filesystem encodingのgolden bytes/hash固定 |
| CAP-AT-29 | 2 | 複数mount qualified/default/missing/duplicateのroutingを完全一致検証 |
| CAP-AT-30 | 6 | `ExecutionStarted`、`CapabilityDecision`、host call pair、budget/yield/resume、最後の`Terminal`に欠番・重複がなく、sink failureでfail-closed |

## 20. ロードマップ・監査項目との関係

- **Phase 0:** security boundaryではないこと、OS責任、residual riskは[脅威モデル](threat-model.md)。
- **Phase 1:** `CapabilitySet`は[実行予算・協調実行仕様](execution-control.md)のfinite `ExecutionRequest`へmoveし、terminal outcomeは[組み込みAPI仕様](embedding-api.md)、catch規則は[次期意味論・実装決定](semantic-decisions.md)に従う。
- **Phase 2:** C1〜C10とPhase 2のCAP-ATを完了条件とする。環境変数allow-listや現行sandboxだけでは完了でない。C10でruntime fs builtinはcapability経路へ全面移行し、`sandbox.rs`はimport解決（C6）専用に縮退した（runtime fsのambient `std::fs`直呼びはcore/builtinから0）。budget env（`TSUMUGI_MAX_*`）はC9/C10対象外（Phase 3/REV-015）。2026-10-20以降はCLI adapter（`main.rs`）だけが読み、`budget.rs`はCAP-AT-27の静的auditの除外リストから外れた（[実行予算・協調実行仕様](execution-control.md)第13節）。
- **Phase 3/4:** finite budget/controlとcooperative `Pending`は[実行予算・協調実行仕様](execution-control.md)を正本とし、Phase 2同期traitを置換しない。
- **Phase 6:** event enum、redaction、bounded journal、sink failureは[決定性・実行時監査仕様](determinism-and-audit.md)を正本とし、`FailClosed`以外や別event名を追加しない。
- **AUD-020:** canonicalize/check/useをportable path-handleへ置換し、dangling symlink、TOCTOU、存在oracleをCAP-AT-10〜14で検証。
- **AUD-018:** `args()`はrequest snapshotだけを読み、CLIが複数script引数を渡す。
- **AUD-049:** 単一catalogとCAP-AT-20を完了条件とする。
