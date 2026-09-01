# Tsumugi — 組み込みAPI仕様

最終更新: 2026-08-31
設計ステータス: **実装仕様確定・未実装**

## 1. 目的と規範範囲

本文書は、[ロードマップ](roadmap.md) Phase 1/2のembedding API先行実装を、Rust実装者が追加判断せず最終契約へ接続できる型、所有権、error、CLI mappingまで固定する規範仕様である。次期言語挙動とCLIは[次期意味論・実装決定](semantic-decisions.md)、Phase 3/4の公開budget・state・handle・poll・transactionは[実行予算・協調実行仕様](execution-control.md)、Phase 5/6のaudit schemaとfail-closedは[決定性・実行時監査仕様](determinism-and-audit.md)、security前提は[脅威モデル](threat-model.md)、外部効果の認可は[Capability Model仕様](capability-model.md)に従う。

以下のRustは、compile/link/source identity等のPhase 1/2公開APIと、後続正本から参照するterminal型の擬似コードである。Phase 3/4型を本書で再定義せず、先行実装専用型をpublic stable surfaceへ出さない。同じbuildへ旧型と最終型を併存させない。

## 2. Phase境界

| Phase | 本書と後続正本の境界 |
|---|---|
| 1 | tree規範backend、importなしrootのcompile/link、source identity、構造化terminal channel、runtime/internal error、CLI一本化。execution-controlの最終型を使う内部縦切り |
| 2 | `CapabilitySet`接続、import resolver、`Denied`、`Exited`、host function、safe/legacy profile。capabilityの最終catch規則を使う内部縦切り |
| 3 | [実行予算・協調実行仕様](execution-control.md)の有限`BudgetConfig` / `BudgetUsage`、deadline、実行中cancellation、`create_execution`を完全実装 |
| 4 | 同文書の`ExecutionState`、`ExecutionHandle`、`PollSlice` / `PollResult`、yield/pause、cooperative adapterを完全実装 |
| 5/7 | VM conformance完了後のstable化 |
| 6 | [決定性・実行時監査仕様](determinism-and-audit.md)のaudit sink、canonical event列、fail-closed、usage監査を実装 |

Phase 1/2の内部縦切りでも`ExecutionRequest`は有限`BudgetConfig`を必須とする。meter未接続部分を内部bootstrapとして段階実装できるが、無制限budget、optional usage、`Suspended`、旧poll/resume等を公開契約にしない。Phase 3/4でbreakingな二重APIを追加せず、最終型を完成させる。

同期`run`はcaller threadをterminal outcomeまで占有するconvenience methodである。Tsumugi coreはworker threadを暗黙生成しない。

## 3. 識別子、設定、構築

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Backend {
    TreeWalk,
    VmExperimental,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum LanguageRevision {
    V0_11,
}

impl LanguageRevision {
    pub const CURRENT: Self = Self::V0_11;
    pub const fn as_str(self) -> &'static str; // "0.11"
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct EngineId(u128); // Engineだけが生成

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError>;
    pub fn as_str(&self) -> &str;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SourceHash([u8; 32]);

impl SourceHash {
    pub const fn as_bytes(&self) -> &[u8; 32];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ExecutionId(NonZeroU128);

impl ExecutionId {
    pub const fn new(value: NonZeroU128) -> Self;
    pub const fn get(self) -> NonZeroU128;
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub backend: Backend,
    pub language_revision: LanguageRevision,
}

impl Default for EngineConfig {
    fn default() -> Self; // TreeWalk + CURRENT
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    EmptyIdentifier { field: &'static str },
    IdentifierTooLong { field: &'static str, max_bytes: u16 },
    IdentifierContainsNul { field: &'static str },
    ExperimentalBackendNotEnabled,
    DuplicateCapability { kind: CapabilityKind },
    DuplicateCallableName { name: String },
    InvalidDescriptor { field: &'static str, code: &'static str },
    InvalidFilesystemPolicy { code: &'static str },
}

pub struct EngineBuilder { /* private */ }

impl EngineBuilder {
    pub fn new() -> Self;
    pub fn config(self, config: EngineConfig) -> Self;
    pub fn allow_experimental_backend(self, allow: bool) -> Self;
    pub fn host_functions(self, registry: HostFunctionRegistry) -> Self;
    pub fn build(self) -> Result<Engine, ConfigError>;
}

pub struct Engine { /* immutable after build */ }

impl Engine {
    pub fn builder() -> EngineBuilder;
    pub fn id(&self) -> EngineId;
    pub fn config(&self) -> &EngineConfig;
}
```

`SourceId::new`は1..=256 UTF-8 bytes、NULなしを受理する。表示用IDであり、hostはsecret pathやcredentialを入れてはならない。`EngineId`はprocess内で衝突しないrandom nonzero u128とし、永続identityには使わない。

`VmExperimental`は`allow_experimental_backend(true)`がなければbuild errorである。Phase 1/2内部縦切りではaudit sinkをまだ接続しない。Phase 6統合buildではbuilderへ`audit_sink`を追加し、[決定性・実行時監査仕様](determinism-and-audit.md)どおりsink未指定を`ConfigError::AuditSinkRequired`で拒否し、`AuditFailurePolicy::FailClosed`だけを受理する。同じbuildにsinkなしfallbackやbest-effort policyを併存させない。

## 4. Compileとsource identity

```rust
pub struct Source<'a> {
    pub id: SourceId,
    pub text: &'a str,
}

impl<'a> Source<'a> {
    pub fn new(id: SourceId, text: &'a str) -> Self;
}

#[derive(Clone, Debug)]
pub struct CompileOptions {
    pub retain_source: bool,
}

impl Default for CompileOptions {
    fn default() -> Self; // retain_source=false
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileDiagnosticCode {
    Lex,
    Parse,
    AstDepth,
    Backend,
    InternalFault,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompileDiagnostic {
    pub code: CompileDiagnosticCode,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub safe_message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompileErrors {
    pub diagnostics: Vec<CompileDiagnostic>, // 常に1件以上、source位置順
}

#[derive(Clone)]
pub struct CompiledScript(Arc<CompiledScriptInner>);

impl CompiledScript {
    pub fn engine_id(&self) -> EngineId;
    pub fn source_id(&self) -> &SourceId;
    pub fn source_hash(&self) -> SourceHash;
    pub fn language_revision(&self) -> LanguageRevision;
    pub fn backend(&self) -> Backend;
}

impl Engine {
    pub fn compile(
        &self,
        source: Source<'_>,
        options: &CompileOptions,
    ) -> Result<CompiledScript, CompileErrors>;
}
```

`compile`はroot sourceだけをlex/parse/backend compileする。import先、filesystem、environment、clock、stdio、host functionを呼ばない。`retain_source=false`ではline mapとdiagnosticに必要なtoken位置以外のsource本文を保持しない。

`SourceHash`はsourceのUTF-8 bytesそのものに対するSHA-256で、BOM除去、改行変換、Unicode正規化をしない。

## 5. Link、Module graph、link request

```rust
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ModuleId(String);

impl ModuleId {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError>;
    pub fn as_str(&self) -> &str;
}

#[derive(Clone, Debug)]
pub struct ImportNode {
    pub module_id: ModuleId,
    pub source_hash: SourceHash,
    pub imports: Vec<ModuleId>, // source内の出現順
}

#[derive(Clone, Debug)]
pub struct ImportGraph {
    pub root: SourceHash,
    pub root_imports: Vec<ModuleId>, // root source内の出現順
    pub nodes: Vec<ImportNode>, // ModuleId UTF-8 byte列の昇順
    pub graph_hash: SourceHash,
}

#[derive(Clone)]
pub struct LinkedScript(Arc<LinkedScriptInner>);

impl LinkedScript {
    pub fn root(&self) -> &CompiledScript;
    pub fn import_graph(&self) -> &ImportGraph;
    pub fn script_hash(&self) -> SourceHash;
}

#[derive(Clone)]
pub struct LinkRequest {
    pub operation_id: ExecutionId,
    pub capabilities: CapabilitySet,
    pub budget: BudgetConfig,
    pub cancellation: CancellationToken,
}

impl LinkRequest {
    pub fn new(
        operation_id: ExecutionId,
        capabilities: CapabilitySet,
        budget: BudgetConfig,
    ) -> Result<Self, ConfigError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkError {
    EngineMismatch,
    RevisionMismatch,
    BackendMismatch,
    FeatureUnavailable { feature: &'static str },
    Denied(Denial),
    Resolve(HostError),
    InvalidModule { module: ModuleId, diagnostics: CompileErrors },
    Cycle { chain: Vec<ModuleId> },
    DepthExceeded { limit: u32 },
    BudgetExceeded(BudgetExceeded),
    DeadlineExceeded,
    Cancelled,
    Backend(ExecutionError),
    InternalFailure { fault_id: u128, safe_message: String },
}

impl Engine {
    pub fn link(
        &self,
        script: &CompiledScript,
        request: LinkRequest,
    ) -> Result<LinkedScript, LinkError>;
}
```

`ModuleId::new`は1..=1024 UTF-8 bytes、NULなしを受理する。resolverはcredential、query secret、絶対local pathをIDへ含めてはならない。

`link`はEngine ID、language revision、backendの順に検証し、不一致なら対応する`LinkError`を返してresolverを呼ばない。Phase 1はimport 0件だけをlinkでき、import文が1件以上あれば`FeatureUnavailable { feature: "module_resolver" }`とする。Phase 2では全importを実行前にresolver capabilityで解決し、循環・深度・module compile・callable symbol/arityを検証する。resolver capabilityなしならresolver call 0で`Denied`。run中のdynamic importは禁止する。import 0件でも空graphを持つ。

`LinkRequest`はresolverへ渡すoperation ID、capability、[実行予算・協調実行仕様](execution-control.md)の有限`BudgetConfig`、cancellationを所有する。deadlineは`BudgetConfig.deadline`を使い、source/module/import count・bytesを同文書の`BudgetUsage`へ課金する。Phase 1/2のmeter実装を内部縦切りにしても、無制限limitや独立optional deadlineをpublic APIへ置かない。`create_execution`からlinkする場合も同じrequest budget/controlを共有し、別の計数系を作らない。

### 5.1 Hashのbyte-level仕様

全整数はunsigned 64-bit big-endian、文字列は`length || UTF-8 bytes`、hashは生32 bytesでencodeする。domain separatorは末尾NULを含むASCII bytesである。

```text
graph = "TSUMUGI-IMPORT-GRAPH-V1\0"
      || str(language_revision.as_str())
      || root_hash
      || u64(root_imports.len)
      || each root import in source order: str(import_module_id)
      || u64(nodes.len)
      || each node sorted by module_id UTF-8 bytes:
           str(module_id) || source_hash || u64(imports.len)
           || each import in source order: str(import_module_id)

graph_hash = SHA-256(graph)

script = "TSUMUGI-LINKED-SCRIPT-V1\0"
       || str(language_revision.as_str())
       || root_hash || graph_hash
script_hash = SHA-256(script)
```

同一`ModuleId`が異なるsource hashで2回解決された場合は`LinkError::Resolve`で拒否し、後勝ちにしない。

## 6. Execution request、budget、context

`BudgetConfig`、`BudgetUsage`、`BudgetExceeded`、`BudgetResource`、`ExecutionRequest`のfieldとconstructorは[実行予算・協調実行仕様](execution-control.md)第3節を唯一の正本とする。`ExecutionRequest`は`execution_id`、frozen `CapabilitySet`、script引数snapshot、有限`BudgetConfig`、`CancellationToken`を所有する。独立したoptional deadline、無制限sentinel、未計測fieldを`Option`で表すusage型を定義しない。

```rust
pub struct ExecutionContext { /* private, !Send, !Sync */ }

impl ExecutionContext {
    pub fn new(engine: &Engine) -> Self;
    pub fn engine_id(&self) -> EngineId;
    pub fn clear_user_state(&mut self) -> Result<(), ContextError>;
    pub fn is_poisoned(&self) -> bool;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextError {
    Busy,
    Poisoned,
}
```

`ExecutionRequest.arguments`はbinary名、script path、CLI flagを含まないscript引数のUTF-8 snapshotである。run中にprocess argvを再読しない。全executionは作成時から有限budgetを持ち、使用量は全fieldが明示値を持つ`BudgetUsage`で返す。Phase 1/2の内部縦切りで未接続meterがあっても、optional usageや無制限APIを公開せず、Phase 3完了までproduction保証を主張しない。

## 7. Cancellation

```rust
#[derive(Clone, Debug)]
pub struct CancellationToken(Arc<CancellationState>); // Send + Sync

impl CancellationToken {
    pub fn new() -> Self;
    pub fn cancel(&self) -> bool; // 最初のfalse→trueだけtrue
    pub fn is_cancelled(&self) -> bool;
}

impl Default for CancellationToken {
    fn default() -> Self; // new()
}
```

`cancel()`はrequestでありthreadをkillしない。link開始前のcancelはresolver call 0で`LinkError::Cancelled`。execution作成後の確認点とraceは[実行予算・協調実行仕様](execution-control.md)第8節に従い、最初の`poll`でscript命令、callback、OS操作より前に確認する。loop中、bulk chunk間、adapter待機、host call前後、commit前でも確認し、terminal `Cancelled`はscript catch不可で全language-stateをrollbackする。同tokenを複数requestへ渡した場合、一度のcancelで全該当requestがcancel対象になる。

## 8. Runtime errorとterminal outcome

runtime errorのkind、canonical message、line、trace、評価順、catch可否は[次期意味論・実装決定](semantic-decisions.md)第3節の`ErrorKind` inventoryを唯一の正本とする。backendごとの縮小enumやmessage推測を作らない。

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceFrame {
    pub function: String,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionError {
    pub code: ErrorKind,
    pub safe_message: String,
    pub line: Option<u32>,
    pub trace: Vec<TraceFrame>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct HostErrorCode(String);

impl HostErrorCode {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError>;
    pub fn as_str(&self) -> &str;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostError {
    pub code: HostErrorCode,
    pub safe_message: String,
    pub retryable: bool,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ExecutionOutcome {
    Completed { value: Value, usage: BudgetUsage },
    Exited { code: u8, usage: BudgetUsage },
    Denied { denial: Denial, usage: BudgetUsage },
    LinkError { error: LinkError, usage: BudgetUsage },
    RuntimeError { error: ExecutionError, usage: BudgetUsage },
    HostError { error: HostError, usage: BudgetUsage },
    BudgetExceeded { failure: BudgetExceeded, usage: BudgetUsage },
    DeadlineExceeded { deadline: MonotonicInstant, usage: BudgetUsage },
    Cancelled { usage: BudgetUsage },
    AuditFailure { error: AuditFailure, usage: BudgetUsage },
    InternalFailure { fault_id: u128, safe_message: String, usage: BudgetUsage },
    RecordFailure { error: RecordFailure, usage: BudgetUsage },
    ReplayMismatch { error: ReplayMismatch, usage: BudgetUsage },
}
```

`HostErrorCode::new`はASCII lowercase `[a-z][a-z0-9_.-]{0,63}`だけを受理する。`safe_message`、`Debug`、`Display`へsecret、absolute path、native backtrace、panic payloadを入れない。

v1の`Completed.value`は`Value::Null`。top-level返値を将来追加してもvariantは変えない。

### 8.1 script catch可否

| 事象 | script内catch | terminal outcome / Phase |
|---|---|---|
| 型、index、name、argument等の通常runtime error | 可 | 未捕捉時`RuntimeError` / Phase 1 |
| script操作中のcapability denial | 可 | canonical `capability` / `sandbox` error。未捕捉時`RuntimeError` / Phase 2 |
| script操作中のsanitized adapter/host function failure | 可 | canonical `host` error。未捕捉時`RuntimeError` / Phase 2 |
| link/import等、script handler開始前のcapability denial | 不可 | `Denied` / Phase 2 |
| link/import等、script handler開始前のhost failure | 不可 | `HostError` / Phase 2 |
| fuel/heap/I/O/source budget | 不可 | `BudgetExceeded` / Phase 3 |
| deadline | 不可 | `DeadlineExceeded` / Phase 3 |
| cancellation | 不可 | `Cancelled` / Phase 3 |
| `exit(code)` | 不可 | grantあり`Exited` / Phase 2。grantなしはcatch可能capability error |
| engine/host callback panic | 不可 | `InternalFailure` / Phase 1/2 |
| audit/record/replay control failure | 不可 | 対応するterminal outcome / Phase 6 |

capability/host errorをcatch可能にしても、denyは対象adapter/OS/callbackより前に成立し、catchによる権限取得はできない。budget、deadline、cancel、audit/internal/record/replay failure、valid `exit`をcatch可能なscript errorへ変換してはならない。`exit`は整数1個または省略時0、0..=255だけを受理する。範囲外・型不一致はcatch可能な`RuntimeError`で、libraryは`std::process::exit`を呼ばない。`Denied` / `HostError` terminalはscript handlerが存在しないlink/control-plane channelに限定する。

## 9. Handle、lifetime、Send/Sync、再利用

`ExecutionState`、`ExecutionHandle`、`PollSlice`、`PollResult`、`HandleError`、`Engine::create_execution` / `start`、`poll` / `pause` / `resume` / `set_waker` / `usage` / `outcome`の最終公開定義は[実行予算・協調実行仕様](execution-control.md)第9節を唯一の正本とする。本書では`Ready/Running/Suspended`等の別stateや`run()`をhandle固有の唯一drive APIとして再定義しない。同期`Engine::run`は、同文書のstate machineをterminalまでpollする制約付きconvenience methodである。

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartError {
    EngineMismatch,
    RevisionMismatch,
    ContextBusy,
    ContextPoisoned,
    InvalidRequest { field: &'static str, code: &'static str },
    Backpressure { active: usize, queued: usize, limit: usize },
    ConcurrentRunRequiresPolling,
    AuditUnavailable,
    InternalFailure { fault_id: u128, safe_message: String },
}
```

開始前のAPI/config mismatchだけが`StartError`、handle作成後は全て`PollResult`と保存済み`ExecutionOutcome`である。`start(&LinkedScript, ...)`はLinkedから、`create_execution(&CompiledScript, ...)`はCreatedから開始する。

| 型 | Clone | Send | Sync | 契約 |
|---|---:|---:|---:|---|
| `Engine` | `Arc`で共有 | yes | yes | build後immutable |
| `CompiledScript` | shallow | yes | yes | 同engine/revision/backendで再利用可 |
| `LinkedScript` | shallow | yes | yes | immutable graphを再利用可 |
| `ExecutionContext` | no | **no** | **no** | 作成threadのみ、同時実行不可 |
| `ExecutionRequest` | yes | yes | yes | create/startでhandleへmove、finite budgetを所有 |
| `ExecutionHandle` | no | no | no | 作成threadでpoll |
| `CancellationToken` | yes | yes | yes | 任意threadからcancel可 |
| `ExecutionWaker` | yes | yes | yes | 別threadはwakeだけ可能 |

`ExecutionContext`の`!Send + !Sync`はstable契約である。Phase 2同期adapterはcaller thread上で有限時間に`Ready`相当の結果を返す。Phase 4 cooperative adapterだけが`Pending` ticketを返し、別threadはresult格納とwakeだけを行う。同期callbackを暗黙worker threadへ移さない。同じcontext/handleへの再入は許さず`InternalFailure`とする。

## 10. 境界、状態遷移、transaction

| 段階 | 許可 | 禁止 | 失敗channel |
|---|---|---|---|
| compile | root lex/parse/backend compile/hash | import、OS、host callback | `CompileErrors` |
| Created/link | resolver import、module compile、cycle/depth/symbol/hash、source budget | script命令、runtime FS、stdio、clock | `ExecutionOutcome`またはpre-handle `LinkError` |
| start/create | engine/context/request整合、finite budget検証、capability freeze、admission | script命令、host callback | `StartError` |
| poll/run | control確認後にscript、capability、host callback、audit | process exit、ambient access、runtime import | `PollResult` / `ExecutionOutcome` |

状態遷移は[実行予算・協調実行仕様](execution-control.md)第9節の`Created → Linked → Ready → Running → Yielded/Paused/Terminal`だけを使う。`poll`だけがsemantic workを進め、`resume()`はPausedを保存済みstateへ戻すだけで、実作業は次の`poll(PollSlice)`が行う。terminal outcomeは1回だけ保存し、terminal後にscriptを再実行しない。

規則:

1. handle作成前のmismatch/backpressureだけを`StartError`にし、作成後のlink/control/runtime結果はterminal `ExecutionOutcome`にする。
2. nonterminal handleのdropはcancelをlinearizeし、pending host callを`Detached`で閉じ、全language-stateをrollbackし、fail-closed audit journalへ最後の`Terminal(Cancelled)`をappendする。dropはblocking I/Oやpanicを行わない。
3. unwind中も内部frame/handlerをtop-levelへ復元し、再unwindしない。
4. `InternalFailure`だけcontextをpoisonする。他terminal後はrollback/commit完了後に再利用できる。
5. AUD-024は全executionに適用する。`Completed` / `Exited`だけが全language-stateをcommitし、catch済みerror後に最終`Completed` / `Exited`ならcatch前後をcommitする。それ以外のterminalはexecution開始時点までbinding、cell、List/Dict、function、module marker等をrollbackする。
6. stdout、filesystem、network、database、process、host function等の完了済み外部効果はrollbackしない。最終`AuditEvent::Terminal`は`context_committed`と`host_effects_may_remain`で結果を表す。
7. capability、arguments、environment、stdio、tokenはcontextへ保存せずterminal後に破棄する。scriptがglobalへ保存した通常Valueが次executionへ残るのは、保存したexecutionが`Completed` / `Exited`でcommitした場合だけである。

## 11. Host panic、abort、exit

1. lexer/parser/linker/backend/host functionのunwind panicを各host boundaryで`catch_unwind(AssertUnwindSafe(...))`する。
2. compile panicは`CompileDiagnosticCode::InternalFault`、link/start panicはそれぞれ`LinkError::InternalFailure` / `StartError::InternalFailure`、handle作成後のrun/poll panicはterminal `ExecutionOutcome::InternalFailure`とし、random nonzero fault IDを付ける。
3. panic payload/native backtraceはpublic errorへ含めず、host-only diagnostic hookがある場合だけ渡す。
4. `panic=abort`、OOM、stack overflow abort、FFI UB、callback内process exit/abortは捕捉不能。[脅威モデル](threat-model.md)どおり別process隔離が最終防御である。
5. 公式host function/adapterはpanic、abort、process exitを行ってはならない。

## 12. CLI adapter

CLIはroot file/stdin読込み、UTF-8検証、profile/capability構築、Engine API、outcome→OS code変換だけを行う。parser/compiler/evaluator/VMを直接呼ばない。唯一の文法は[次期意味論・実装決定](semantic-decisions.md)第6節である。

```text
tsumugi [OPTIONS] [SCRIPT [ARGS...]]
```

- OPTIONSには`--vm`、`--profile safe|legacy`、capability option、`--help`、`--version`、`--`だけを置く。`--backend`は定義しない。
- option解析中の最初のpositionalをSCRIPTとし、それ以後は`--vm`、`--help`、`--`、未知option風tokenを含め一切再解釈せず、`ExecutionRequest.arguments`へ順序どおり渡す。
- `-`はstdin script、scriptなしと`tsumugi --`はREPLである。
- `args()`はbinary名、script path、CLI flagを含めない。
- 非UTF-8、unknown option、option値欠落、profile/capability usage errorはscript開始前にstderrへ診断しexit 1とする。help/versionだけstdout・0である。
- profile/capability optionの値検証とgrant構築は[Capability Model仕様](capability-model.md)第14節に従う。safe profileのstdout既定はbuilderによる明示`Stdout` grantでありambient accessではない。

| 結果 | file/stdin mode | REPL |
|---|---:|---|
| `Completed` | 0 | 次入力へ |
| `Exited { code, .. }` | code | session終了、codeを返す |
| `RuntimeError` / `Denied` / `LinkError` / `HostError` / `BudgetExceeded` / `DeadlineExceeded` | 1 | 表示し次入力へ |
| `Cancelled` | 130 | 現在入力だけcancelしpromptへ戻る |
| `AuditFailure` / `InternalFailure` / `RecordFailure` / `ReplayMismatch` | 70 | session終了、context破棄 |
| CLI usage / UTF-8 / file / stdin read error | 1 | 該当なし |
| compile error | 1 | REPL chunkなら表示し次入力へ |

`tree`が既定・規範backend。`--vm`はexperimental VMを選択しwarningを要求する。VMも`Engine::compile`と同じexecution-control APIを通り、conformance完了まで別のstable backend名や`--backend` optionを公開しない。

## 13. 既存API移行

現行`Engine::new()`、`compile(&str)`、`execute(...)`、引数なし`ExecutionContext::new()`はalpha APIである。

### Release N-1

1. 最終型を追加する。名前衝突methodは一時的に`compile_v1`、`link_v1`、`run_v1`で提供する。
2. 旧APIへ`#[deprecated]`。
3. 旧wrapperはambient権限を付与しない。外部効果は`Denied`とし新APIへの移行を示す。
4. crate rootから新型をre-export。low-level moduleはstable surface外と明記する。

### Release N

1. breaking alpha releaseとして旧methodを削除し、本書の`compile/link/start/run`へ改名する。
2. `ExecutionContext::new(&Engine)`だけを残す。
3. CLI tree/VM両経路を新APIへ統一する。
4. 旧`TsumugiError`は内部型として残せるがterminal APIから返さない。

compatibility shimもprocess exitやambient accessを復活させず、script操作中のcapability/host errorをcatch可能とするcanonical契約と、link/control-planeのterminal契約を変更してはならない。

## 14. 実装slice

| Slice | Phase | 内容 | 完了条件 |
|---|---:|---|---|
| E1 | 1 | ID/config/error/outcome/EngineBuilder | constructor、default、secret-free Debug |
| E2 | 1 | CompiledScript、importなしLinkedScript、source/root hash | byte-level hash、source非保持default |
| E3 | 1 | tree backend adapter | 既存tree意味論、CLI同一入口 |
| E4 | 1/3 | Context/Handle cleanupとtransaction journalの縦切り | 再利用、poison、drop、全language-state rollback |
| E5 | 1 | Completed/Runtime/Internal/Cancelled(pre-run) channel | process継続、catch規則 |
| E6 | 1 | compile/link/run panic隔離 | unwind test、fault ID |
| E7 | 2 | capability/import graph/Denied/Exited/HostError/host function接続 | capability Phase 2基準 |
| E8a | 1 | CLIがEngine APIだけを通る入口統合、基本引数転送 | EMB-AT-15/16/17のPhase 1範囲、AUD-018 |
| E8b | 2 | capability profile/options、safe/legacy移行 | EMB-AT-10/11/16、CAP-AT-23〜26、migration warning |
| E9 | 5/7 | VM experimental adapter/conformance | 同API、差分0でstable化 |
| E10 | N-1/N | deprecation/migration | compile test |
| E11 | 3 | budget/deadline/runtime cancel | terminal variant境界test |
| E12 | 4 | final state machineとpoll/pause/resume | `PollSlice` / `PollResult`、yield/pause/state test |
| E13 | 6 | canonical audit sink/event/usage完全性 | 8 event、fail-closed、`BudgetUsage` test |

E1→E2→E3→E4→E5→E6→E8a、次にE7→E8b。E11以降をPhase 1/2完了条件へ前倒ししない。

## 15. 受入基準

| ID | Phase | 基準 |
|---|---:|---|
| EMB-AT-01 | 1 | importなしpublic exampleがlow-level moduleなしでcompile→link→runできる |
| EMB-AT-02 | 1 | Compiled/Linkedは`Send+Sync`、Context/Handleは`!Send+!Sync`のcompile-time assertion |
| EMB-AT-03 | 1 | foreign CompiledScriptをlink時、foreign LinkedScript/contextをstart時に対応するmismatchで拒否しresolver/命令0 |
| EMB-AT-04 | 1 | 同contextの同時startをborrowまたはContextBusyで拒否 |
| EMB-AT-05 | 1/2 | compile時resolver/OS/callback 0、link時script/stdio 0、run時resolver 0。Phase 1 importはFeatureUnavailable |
| EMB-AT-06 | 1 | Ready dropは命令0、context再利用可 |
| EMB-AT-07 | 1〜4 | InternalFailure以外のterminal後はframe cleanでcommit/rollback規則どおり再利用可、InternalFailureだけpoison |
| EMB-AT-08 | 1〜4 | Completed/Exitedだけ全language-state commit、caught error後のCompleted/Exitedもcommit、その他terminalは開始時点へrollback。完了済み外部I/Oは戻さずauditへ記録しtree/VM一致 |
| EMB-AT-09 | 1/2 | Phase 1でCompleted/Runtime/InternalFailure/pre-run Cancelled、Phase 2でExitedとlink/control-plane Denied/HostErrorを構造化terminal channelだけで返す |
| EMB-AT-10 | 2〜6 | 通常runtimeとscript操作中のcapability/host errorはcatch可。link/control-plane deny/host、budget/deadline/cancel/exit/panic/audit/record/replayはcatch不可 |
| EMB-AT-11 | 2 | exit(-1/256)はRuntimeError、0/255はExited、capabilityなしはcatch可能capability error、library process継続 |
| EMB-AT-12 | 1/3 | pre-cancelは命令0。Phase 3でloop/host call境界cancelを各1 terminalにする |
| EMB-AT-13 | 1/2 | Phase 1でroot hash、Phase 2でimport graph hashのgolden bytes/SHA-256を固定し1 byte変更で差が出る |
| EMB-AT-14 | 1/2 | fake secretがpublic error/Debugへ現れない |
| EMB-AT-15 | 1 | CLI引数0/1/複数/`--`/非UTF-8がAUD-018契約どおりで、Engine APIの`ExecutionRequest.arguments`へ転送される |
| EMB-AT-16 | 1〜3 | 実装済みoutcomeのCLI exit mappingをsubprocess完全一致検証 |
| EMB-AT-17 | 1/5 | tree/VM CLIがEngine APIだけを通る |
| EMB-AT-18 | N-1 | 旧sampleはdeprecation warning、新sampleはwarningなし |
| EMB-AT-19 | N | 旧sampleはcompile失敗、migration後成功 |
| EMB-AT-20 | N-1/N | compatibility wrapperでambient権限が復活しない |
| EMB-AT-21 | 3 | budget N-1/N成功、N+1 BudgetExceeded。deadline/cancelもterminal channelのみ |
| EMB-AT-22 | 4 | final `ExecutionState`全状態のpoll/pause/resume合法・不正matrix、slice N-1/N/N+1を検証 |
| EMB-AT-23 | 6 | `ExecutionStarted`から`Terminal`までのcanonical sequence、fail-closed、`BudgetUsage`完全性を検証 |

## 16. ロードマップ・監査項目との関係

- **Phase 0:** fault非保証、panic/abort責任分界は[脅威モデル](threat-model.md)。
- **Phase 1:** E1〜E6・E8a、EMB-AT-01〜09・13・15〜17を完了条件とする。E8aでCLIのtree/VM両経路をEngine APIだけへ統合し、基本引数転送を行う。現行tree-only facadeだけでは不足する。
- **Phase 2:** E7・E8b、EMB-AT-10/11/16、CAP-AT-23〜26と[capability model](capability-model.md)のPhase 2基準を完了する。E8bでcapability profile/optionsとsafe/legacy移行を接続する。
- **AUD-018:** process argvをExecutionRequest snapshotへ置換し、複数script引数とCLI syntaxを固定した。
- **AUD-020:** Engineでpath文字列checkをせず、path-handle adapterへ委譲する。
- **AUD-049:** compile/link callable解決は単一catalogを使い、host registryを第4のbuiltin名一覧にしない。
