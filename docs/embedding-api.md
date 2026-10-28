# Tsumugi — 組み込みAPI仕様

最終更新: 2026-10-20
設計ステータス: **実装仕様確定・実装進行中**（E1〜E6・E7〔tree REPL の Engine API 統合を含む〕・E8a・E11 実装済み。E9〔VM 経路統合、Phase 5〕のうち **VM file/stdin の import・resolver 統合（link 層 capability gate）は実装済み**で、残りの adapter/conformance（VM の charge parity・StableError・FunctionId・transaction 等）と experimental flag 除去は未実装＝**E9 は未完了**。E8b の capability profile/options 配線（`--profile safe|legacy` と全 capability option、safe/legacy の frozen `CapabilitySet` 構築・注入）は C9/C10/E7/E11 で統合済み、migration warning は §14.4 の N-1 スキップ採択により正式に descope 済み（不実装）、EMB-AT-10/11 は profile 経由 subprocess test で固定済み（下記実装 slice 表・第16節 Phase 2 の受入 ID ↔ test 対応を参照）＝**E8b 完了**。E10 は未実装。詳細は [ロードマップ](roadmap.md) Phase 1/2 を参照）

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
    V0_19,
    V0_20,
    V0_21,
}

impl LanguageRevision {
    pub const CURRENT: Self = Self::V0_21;
    pub const fn as_str(self) -> &'static str; // 例: V0_21 => "0.21"
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

### 4.1 実行用 AST の入手方針（案 A採用、案 B却下の記録）

`CompiledScript` と `LinkedScript` は `Send + Sync` 契約（EMB-AT-02）を持つ一方、実行入口（E3 tree backend adapter）は実行スレッド上で runnable な `Program`（AST）を必要とする。現行 AST は `ast::Block = Rc<[Stmt]>`（REV-015 Slice 3 PR-d の永続 continuation）で `!Send`/`!Sync` のため、この 2 つを両立する方針を確定する必要があった。以下は 2026-09-21 の設計判断の記録である。

**採用（案 A）: 実行時に保持 source を再 parse して `Program` を再構築する。**

- `CompiledScript` は identity・`source_hash`・`has_imports`・任意の保持 source だけを持ち、runnable AST を保持しない。実行入口は保持 source を実行スレッド上で `Lexer`→`Parser` で再 parse して `Program` を得る。parse は決定的なので再構築した AST は `source_hash` と必ず一致し、`Evaluator::begin_execution` が root を `Rc<[Stmt]>` フレームへ写した後は `Program` を drop できる。
- 実行するには `retain_source = true` で compile しておく必要がある（保持していない場合は `InternalFailure`）。これは欠点ではなく、原則2「権限・前提はホストが明示的に付与する（ambient を既定にしない）」に沿う明示契約である。
- 評価器・AST の中核に一切触れないため、変更が実行入口スライスに閉じる（原則8「小さく理解可能な中核」）。tree 評価器のホットパスの性能特性（AUD-038/040 でチューニング済み）も不変。

**却下（案 B）: `ast::Block` を `Rc<[Stmt]>` から `Arc<[Stmt]>` へ変え、AST を `Send + Sync` 化して handle に保持する。**

- 変更点自体は小さい（型 alias 1 + `Rc::from` 呼び出し 5 箇所）。AST は `String`/`Expr`/`Block`/`Vec` だけで `Value`/`Rc` を含まないため、`Arc` 化で `Program`/`CompiledScript` は真に `Send + Sync` になり、再 parse も source 保持も不要になる。
- しかし却下した。理由:
  - 実行そのものは `ExecutionContext` が `!Send + !Sync`（第9.1節の stable 契約）で単一スレッドに固定される。`Send + Sync` が必要なのは handle だけで、**AST を跨スレッド共有する能力は思想（単一スレッド実行）が要求していない**。案 B はその不要な能力を中核へ足す（原則8 に反する方向）。
  - tree 評価器はフレームごとに `Block` を clone するホットパスで、`Rc`（非アトミック）→`Arc`（アトミック参照カウント）はわずかに実行コストを増やす。速度は非目標だが「わざわざ遅くする」のは別で、根拠のない性能変更を避ける原則9・計測負債の観点で不利。
  - 「Send な AST が存在する」こと自体が、将来「実行を複数スレッドへ分ければ速い」という原則1（最悪時負荷の制御）・原則5（backend 差を作らない）に反する最適化への誘因・説明負債として中核に residue を残す。
- 将来性の評価: Phase 5（決定性・record/replay）は source を再現の第一級入力として扱うため、案 A の「実行時に source から再構築」は将来像と同じ方向を向く。逆に案 B の「Send な AST」は、`!Send` 実行という確定契約の下では将来も使い道がない。案 B が正当化されるのは Tsumugi が単一スレッド実行の stable 契約を撤回して真の並行 AST 共有へ舵を切る場合だけで、それはマニフェスト原則1・5・「目指さないもの（最大スループット競争）」に照らして現時点では起きない前提の変更であり、実行入口スライスで先取りする話ではない。

したがって、現状でも将来でも案 A を採る。案 B の再検討は、単一スレッド実行契約そのものを見直す意思決定とセットでのみ行う。

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
    ) -> Self;

    // cancellation注入の補助builder（additive）。既定は never-cancel token。
    pub fn with_cancellation(self, token: CancellationToken) -> Self;
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

`link`はEngine ID、language revision、backendの順に検証し、不一致なら対応する`LinkError`を返してresolverを呼ばない。Phase 2では全importを実行前にresolver capabilityで解決し、**循環・深度・module単体compile・hash**を検証する。static cross-module callable symbol/arityの検証はPhase 2のlink検証に含めず、export/symbolモデルが入る後続Phase（exportモデル導入スライス）へ移す。未解決のcross-module symbolはlinkで静的に落とさず、run時に`RuntimeError`（name error等）へ落ちる。resolver capabilityなしでimportがあればresolver call 0で terminal `Denied`とする。`cancellation`は`with_cancellation`で注入し、link開始前のcancelはresolver call 0で`Cancelled`。run中のdynamic importは禁止する。import 0件でも空graphを持つ。`FeatureUnavailable`はPhase 2最終形でも列挙に残すが、import解決がEngine APIへ配線された後はimportを理由に返されない（到達しない保持variant）。

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

`ExecutionContext`の`!Send + !Sync`はstable契約である。Phase 2同期adapterはcaller thread上で有限時間に`Ready`相当の結果を返す。Phase 4 cooperative adapterだけが`Pending` ticketを返し、別threadはresult格納とwakeだけを行う。同期callbackを暗黙worker threadへ移さない。同じcontext/handleへの再入は許さず`InternalFailure`とする。audit sink callbackの中から同じ`Engine`の入口を呼ぶ再入も許さない。`run`は`ExecutionOutcome::InternalFailure`、`compile`は`CompileDiagnosticCode::InternalFault`の診断1件の`CompileErrors`、`link`は`LinkError::InternalFailure`で、何も実行せずに拒否する。この拒否は実行を始める前に返すもので、渡されたcontextを変えずpoisonもしない（第10節規則4のpoisonは実行したexecutionのterminalに対するもので、この拒否には適用しない）。対象の入口の全体（REPL用の内部入口と監査付き入口を含む）、外側のexecutionの扱い、入れ子のcallbackは[決定性・実行時監査仕様](determinism-and-audit.md)第10節を正本とする。

## 10. 境界、状態遷移、transaction

| 段階 | 許可 | 禁止 | 失敗channel |
|---|---|---|---|
| compile | root lex/parse/backend compile/hash | import、OS、host callback | `CompileErrors` |
| Created/link | resolver import、module単体compile、cycle/depth/hash、source budget（static cross-module symbol/arityはPhase 2外・後続Phase） | script命令、runtime FS、stdio、clock | `ExecutionOutcome`またはpre-handle `LinkError` |
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
2. compile panicは`CompileDiagnosticCode::InternalFault`、link/start panicはそれぞれ`LinkError::InternalFailure` / `StartError::InternalFailure`、handle作成後のrun/poll panicはterminal `ExecutionOutcome::InternalFailure`とし、random nonzero fault IDを付ける。panic以外では、audit sink callbackからの同じEngineへの再入の拒否（第9節）にも`CompileDiagnosticCode::InternalFault` / `LinkError::InternalFailure` / `ExecutionOutcome::InternalFailure`を使う。
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
- **C9/C10時点のcapability注入経路**: CLIはprofileから組んだfrozen `CapabilitySet`を経路別に注入する——tree file/stdin（importの有無を問わず）はEngine API（`ExecutionRequest::with_capabilities`、link時は`LinkRequest.capabilities`）、tree REPLもE7でEngine API（session で1つ保持する`EmbeddingContext`を再利用し、各submissionの`ExecutionRequest::with_capabilities`へ同じfrozen setを載せる。REPLは`clear_user_state`やcapability clearを呼ばず持ち越す）、全VM経路は`Vm::set_capabilities`。tree file/stdinのimport解決はC6/E7でEngine resolverへ統合済み（`--allow-import-root`が`module_resolver`をgrant）。tree REPLはsession開始時に1回だけ注入した扱いを再現しつつ全submissionへ持ち越す。
  - **tree REPLのimport解決はresolver gateを通らない**: tree REPLは統合前から`--allow-import-root`無しでもCWD相対importを解決できる（評価器内蔵の`ModuleLoader`が解決し、固定のエラー文字列を出す）observable挙動を持つ。E7ではこの挙動を保全するため、REPL専用の内部入口（`link_for_repl`／`run_for_repl`、いずれも`#[doc(hidden)]`のcrate内部shim）がfile/stdinの`link`が課すresolver gateを通さず、import解決はrun側の評価器内蔵loaderに委ねる。これは新規のcapability緩和ではなく従来挙動の保全であり、file/stdin経路のresolver gate（未grant + importは terminal `Denied`）とは別物。REPLのimport gate迂回を将来capabilityで塞ぐならE8b/capability model側の課題。
  - **tree REPLのbudgetライフサイクル**: REPLはsession全体で1本のbudget ledgerを共有し、submissionごとにfuel(step)だけresetしてcumulative counter（source/output/host/string/heap等）はsession累積を温存する（`run_for_repl`）。これも統合前tree REPLのobservable挙動の保全で、file/stdinのone-shot run（呼び出しごとにbudget config再構築）とは意図的に別経路。
  - **VM file/stdinのimport gate**: E9でVM file/stdin（`execute_vm_with_path`）のimport解決を、link層のEngine resolver gate（`compile`→`link`、`LinkRequest.capabilities`の`module_resolver`。`--allow-import-root`がgrant）経由へ統合した。未grant + importはtreeと同じくterminal `Denied`、import 0件はresolver不問で通過、grant時は従来どおり解決・実行する。実際のimport展開とscript本体実行は依然`Compiler`+`Vm`（`Vm::set_capabilities`）のalpha経路で、run層のimport解決はtree file/stdinと同じく評価器内蔵`ModuleLoader`が担う（gate専用に`Backend::TreeWalk` engineをbuildする最小統合で、VM backend adapterは新設しない）。VM backend adapter本体（run経路のEngine統合）とconformanceはE9残。VM REPLは現状維持（tree REPLと対称にimport gateを迂回し評価器内蔵`ModuleLoader`に委ねる）。

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
| E8a | 1 | CLIがEngine APIだけを通る入口統合、基本引数転送（🟡 tree file/stdinはimportの有無を問わずEngine API経由で完了〔importはC6/E7〕。tree REPLもE7でEngine API統合済み〔import解決は評価器内蔵loaderに委ねる`link_for_repl`／`run_for_repl`経由〕。VM file/stdinのimport gateは統合済み〔E9の一片。link層resolver gate経由〕、VM backend adapter本体はE9残） | EMB-AT-15/16/17のPhase 1範囲、AUD-018 |
| E8b | 2 | capability profile/options、safe/legacy移行（✅ 完了。profile/options 配線は C9/C10/E7/E11 で統合済み。migration warning は[capability model](capability-model.md) §14.4 の N-1 スキップ・safe 既定採択により正式に descope（不実装）。EMB-AT-10/11 は profile 経由 subprocess test で固定済み——第16節 Phase 2 参照） | EMB-AT-10/11/16、CAP-AT-23〜26（migration warning は descope——§14.4 参照） |
| E9 | 5/7 | VM experimental adapter/conformance（🟡 VM file/stdinのimport・resolver gateはEngine resolver〔`compile`→`link`、`module_resolver` capability〕経由へ統合済み。残りはadapter/conformance〔charge parity・StableError・FunctionId・transaction 他〕とstable化） | 同API、差分0でstable化 |
| E10 | N-1/N | deprecation/migration | compile test |
| E11 | 3 | budget/deadline/runtime cancel（🟡 tree範囲で実装。`EngineConfig.budget`で有限`BudgetConfig`、`ExecutionRequest::with_deadline_clock`でdeadline clock、`ExecutionContext::cancellation_token()`で実行中cancel。`ExecutionOutcome`に`BudgetExceeded`/`DeadlineExceeded`追加、budget/deadline/cancelをcatch不能terminal化（`ErrorKind::is_uncatchable`をtree/VM共有）。実system monotonic clock（`SystemMonotonicClock`）と全terminal variantへの`usage: BudgetUsage`同梱まで実装済み。さらに最終形移行Slice 1として`ExecutionRequest`を`new(budget, clock) -> Result<_, ConfigError>`へ移し有限`BudgetConfig`とdeadline clockを必須所有（`with_deadline_clock`独立経路と`EngineConfig.budget`二重管理を解消、deadlineは`budget.deadline`のみ、domain/revision/deadline検証を構築時へ前倒し）、CLI既定をoption B＝import なし経路でstandard有限budgetへ変更。`CancellationToken`必須所有も実装済み（`ExecutionRequest`が`cancellation`を所有し`.cancellation(value)`で設定、`Engine::run`が実行直前にinstall）。REV-015最終形移行は完了） | terminal variant境界test |
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
| EMB-AT-05 | 1/2 | compile時resolver/OS/callback 0、link時script/stdio 0、run時resolver 0。Phase 2でimportはlink時にresolver capabilityで解決（resolver未grantは terminal `Denied`）。Phase 1の「importはFeatureUnavailable」はC6/E7で解消済み |
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
- **Phase 1:** E1〜E6・E8a、EMB-AT-01〜09・13・15〜17を完了条件とする。E8aでCLIのtree/VM両経路をEngine APIだけへ統合し、基本引数転送を行う。現行tree-only facadeだけでは不足する。E8aの実装は、CLIのtree file/stdin実行のうちimportなしrootをEngine API（`compile`→`link`→`run`）だけへ統合し、`ExecutionRequest.arguments`で引数転送・`ExecutionOutcome`→exit code変換を第12節どおりに行うところまで。importを含むtree file/stdin rootはC6/E7でEngine API（`compile`→`link`→`run`）へ統合済み（`link`がresolver capabilityでimportを解決し、resolver未grant + importは terminal `Denied`）。tree REPL（入力間の状態継続とimport解決が必要）もE7でEngine API（`compile`→`link_for_repl`→`run_for_repl`、`EmbeddingContext`再利用）へ統合済み。ただしREPLはimport解決を評価器内蔵loaderに委ね（resolver gateを通さず従来のCWD相対import・固定エラー文字列を保全）、budgetはsession累積・fuelのみresetする（いずれも統合前の観測挙動の保全。詳細は第12節）。VM file/stdinのimport gateはE9でlink層のEngine resolver gate経由へ統合済み（未grant + importはtreeと同じくterminal `Denied`）だが、VM backend adapter本体（run経路のEngine統合）はE9残。したがってEMB-AT-17はtree経路（file/stdin・REPL、importの有無を問わず）とVM file/stdinのimport側について満たし、残り（VM backend adapter本体の統合とconformance）はE9で満たす。
- **Phase 2:** E7・E8b、EMB-AT-10/11/16、CAP-AT-23〜26と[capability model](capability-model.md)のPhase 2基準を完了する。E8bでcapability profile/optionsとsafe/legacy移行を接続する。この profile/options 配線（`--profile safe|legacy`・全 capability option・safe/legacy の frozen `CapabilitySet` 構築と tree/VM 両経路への注入）はC9/C10/E7/E11で統合済みで、CAP-AT-23〜26・exit mapping（EMB-AT-16）・catch 規則（EMB-AT-10/11）に相当する実装・テストが存在する。E8b 受入条件のうち **migration warning は正式に descope する（不実装）**: deprecation / migration warning は本来 N-1 段階（legacy 既定・移行促進）のためのもので、[capability model](capability-model.md) §14.4 の「N-1 を飛ばし safe 既定（N 相当）を採る」採択により本 revision では役割がない。legacy 選択時の既存 security warning（全アクセス許可の警告、§14.3）で十分とする。したがって E8b の Phase 2 受入は EMB-AT-10/11/16・CAP-AT-23〜26 とし、migration warning は受入条件から外す。EMB-AT-10/11 は下記の受入 ID ↔ test 対応どおり profile 経由 subprocess test で固定済みで、**E8b は完了**（profile/options 配線は統合済み、migration warning は descope 済み、EMB-AT-10/11 を profile 経由で固定済み）。ただし Phase 2 全体の完了は[capability model](capability-model.md)側の Phase 2 基準（CAP-AT 等）にも依存するため、本行は embedding 側 E8b の完了を述べるにとどめる。

  - **EMB-AT-10/11 の受入 ID ↔ test 対応（トレーサビリティ）**: catch 規則・exit 写像・range 写像は outcome 種別レベルを tree 生 `CapabilitySet` 経由の embedding 層 unit test で、CLI safe profile が組む frozen set 経由の挙動を tree/VM 両 engine の subprocess test で、それぞれ固定する。両者は補完関係にある（unit test は outcome 種別の正本、subprocess test は profile 配線を通した観測挙動の正本）。
    - **EMB-AT-10a**（通常 runtime の capability-deny が script の try/catch で catch 可能）: unit = `src/embedding.rs` の `c7_exit_ungranted_is_catchable` / `c3_env_ungranted_is_catchable` / `c3_now_ungranted_is_catchable`（tree 生 set、catch して Completed）。profile 経由 = `tests/cli_safe_profile.rs::safe_capability_deny_is_catchable_via_profile`（tree/VM、exit/env/now を try/catch で囲み `e["type"]` が `capability` で正常終了）。
    - **EMB-AT-10b**（未捕捉の capability-deny が RuntimeError outcome）: unit = `c7_exit_ungranted_is_runtime_error` / `c3_env_ungranted_is_runtime_error` / `c3_now_ungranted_is_runtime_error`（`ErrorKind::Capability`）。profile 経由 = `tests/cli_safe_profile.rs::safe_default_grants_only_stdout`・`tests/ambient_access_audit.rs::safe_default_denies_all_ambient_operations`（tree/VM、未捕捉 deny → 非ゼロ exit）。
    - **EMB-AT-10c**（link/control-plane deny は catch 不可）: `tests/engine_api.rs::link_error_surfaces_as_link_error_outcome`、`tests/cli_safe_profile.rs::relative_import_tree_requires_import_root` / `relative_import_vm_requires_import_root`（terminal Denied、exit 1、tree/VM）。
    - **EMB-AT-11a**（exit(0/255) が Exited、granted exit code が実 process exit code へ伝播）: unit = `c7_exit_boundary_codes_are_exited` / `c7_exit_granted_maps_to_exited`（Exited outcome）。profile 経由 = `tests/cli_safe_profile.rs::allow_exit_propagates_exit_code_via_profile`（tree/VM、`--allow-exit` + exit(0/42/255) → 実 process exit code 0/42/255）。exit mapping は `main.rs::exit_code_for_outcome` の `Exited { code } => code as i32`。
    - **EMB-AT-11b**（exit(-1/256) が RuntimeError 写像）: unit = `c7_exit_out_of_range_is_runtime_error`（`ErrorKind::Argument`）。profile 経由 = `tests/cli_safe_profile.rs::allow_exit_out_of_range_maps_to_runtime_error_via_profile`（tree/VM、`--allow-exit` + exit(256)/exit(0 - 1) → exit 1、process は panic せず終了）。
    - **EMB-AT-11c**（exit capability なしは catch 可能な capability error）: unit = `c7_exit_ungranted_is_runtime_error` + `c7_exit_ungranted_is_catchable`。profile 経由 = `tests/cli_safe_profile.rs::safe_capability_deny_is_catchable_via_profile`（exit(0) を含む）。
    - **EMB-AT-11d**（library process 継続）: unit = `c7_exit_does_not_poison_context`。profile 経由 = `tests/cli_safe_profile.rs::safe_capability_deny_is_catchable_via_profile`（catch 後に `print("alive")` が走る）、REPL = `tests/integration.rs::repl_continues_session_after_uncaught_error_in_both_engines`。
- **AUD-018:** process argvをExecutionRequest snapshotへ置換し、複数script引数とCLI syntaxを固定した。E8a/C6-cでtree file/stdin経路（importの有無を問わず）の引数転送を`EmbeddingRequest::with_arguments`→`Engine::run`経由へ載せ替えた。tree REPLはE7でEngine API（`compile`→`link_for_repl`→`run_for_repl`、`EmbeddingContext`再利用）へ統合済み（REPLはscript引数を持たないため`with_arguments`は使わない）。
- **AUD-020:** Engineでpath文字列checkをせず、path-handle adapterへ委譲する。
- **AUD-049:** compile/link callable解決は単一catalogを使い、host registryを第4のbuiltin名一覧にしない。
