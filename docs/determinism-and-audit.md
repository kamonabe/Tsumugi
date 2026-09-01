# Tsumugi 決定性・実行時監査仕様

最終更新: 2026-08-31

設計ステータス: **実装仕様確定・未実装**

## 1. 位置づけ

本文書は、[Tsumugi Manifesto](manifesto.md)と[ロードマップ](roadmap.md)のうち、マニフェスト実現ロードマップ Phase 5「規範意味論と決定的境界」とPhase 6「実行時監査」の実装仕様を定める。

本文書は次の文書と一体で実装する。

- [組み込みAPI仕様](embedding-api.md): Phase 1/2の`Engine`、compile済みscript、実行context、構造化outcomeの先行契約
- [Capability Model仕様](capability-model.md): Phase 2のdeny-by-default policy、host function registry、各外部効果の境界
- [実行予算・協調実行仕様](execution-control.md): Phase 3/4の最終budget、continuation、yield、pause、cancel、scheduler、transaction
- [次期意味論・実装決定](semantic-decisions.md): 次期言語挙動、CLI、canonical error、catch可否
- [言語仕様](language-spec.md): 現行実装から観測できる意味論

監査は秘密情報の保存機能ではなく、hostが実行の事実・許可・外部操作・資源消費・終了理由を検証するためのcontrol-plane機能である。audit sinkはscriptから直接参照できず、host capabilityにも公開しない。

### 1.1 Phase 1/2文書との統合規則

Phase 5/6実装では、[組み込みAPI仕様](embedding-api.md)と[Capability Model仕様](capability-model.md)の先行契約を次の最終契約へ統合する。本文書がaudit eventとfailure policyの正本である。

- `EngineConfig`の暫定`BestEffortWithCounter`既定は廃止し、10節の`FailClosed`だけを既定かつ初期実装で唯一のpolicyとする。sink未指定はin-memory fallbackへ暗黙変更せず`ConfigError::AuditSinkRequired`とする。
- capability文書の`CapabilityAllowed` / `CapabilityDenied`暫定eventは、`AuditEvent::CapabilityDecision { decision: Allow | Deny }`へ統合する。別event名を残さない。
- Phase 2の同期adapterはPhase 4以前の契約である。Phase 4のcooperative extensionだけが`Pending`を返せ、Phase 5/6は両者を同じhost call correlationへ記録する。
- budget、deadline、yield/pause、terminal stateは[実行予算・協調実行仕様](execution-control.md)の最終型を使う。
- context commit/rollbackとfunction identityは次期意味論の`FunctionId`および全language-state transactionへ統一する。Phase 1のprefix commit記述を残さない。

これらは同じbreaking alpha releaseで更新し、同じbuildにbest-effort/fail-closedや旧/新event enumを併存させない。

## 2. 規範backendと適合方針

### 2.1 treeを規範backendとする

Phase 5のproduction backendはtree-walk evaluator（以下`tree`）だけとする。`tree`は[言語仕様](language-spec.md)を実行する規範backendであり、VMは全適合gateを通るまで`experimental`である。

新規6設計文書の規範優先順位は領域別に次のとおりとする。

1. 次期言語挙動、CLI、canonical error、catch可否は[次期意味論・実装決定](semantic-decisions.md)
2. Phase 3/4の公開API、状態機械、budget、AUD-024 transactionは[実行予算・協調実行仕様](execution-control.md)
3. Phase 5/6のaudit schema、record/replay、redaction、fail-closedは本文書
4. Phase 0の保証境界とhost/Tsumugi/OS責任は[脅威モデル](threat-model.md)
5. Phase 1/2の組み込み・capability契約は上記最終契約の内部subset
6. 規範fixtureとtreeの観測結果、実装内部の都合

現行releaseで利用者が依存できる挙動は[言語仕様](language-spec.md)に従うが、次期revisionの設計・実装ではsemantic-decisionsを先に満たし、完了後にlanguage-specへ統合する。仕様とtreeが矛盾する場合は該当revisionの仕様を正としてtreeを修正し、treeのbugを無条件に仕様へ昇格しない。

VMは次を満たすまで、embedding APIのproduction選択肢、record作成backend、監査保証対象にしない。

- 全規範fixtureでvalue、error、context commit、side effect、fuel、`normalize_for_conformance`後のaudit semantic eventがtreeと一致する
- AUD-016、AUD-017、AUD-019、AUD-024、AUD-048を含む既知非適合が0件である
- differential matrix、limit境界、REPL継続、pause/resume、record/replay、fuzz gateを通る

backend名は監査metadataには記録するが、backendは結果差を許すdeterminism inputではない。

## 3. 決定性の定義

同一のdeterminism input tupleから、次が同一になることを決定性と呼ぶ。

- scriptの最終valueまたは構造化terminal outcome
- catch可能errorの安定code、script-visible type/message/line
- `ExecutionContext`へcommitされるbinding、collection、function identity、module状態
- host call requestの順序、operation、引数、許可・拒否、外部効果の論理順序
- output byte列とinput消費順序
- budget charge列とterminal時`BudgetUsage`。同じpoll scheduleを与えるexecution-trace比較ではyield reasonも含む
- timestamp等の運用fieldを除いたaudit semantic event列

engine全体で同時に走る別execution間の外部効果順序は決定性の対象外である。1 execution内の順序だけを保証する。複数executionを跨ぐ順序が必要なhost functionは、host側でtransaction keyやserialize policyを提供する。

### 3.1 DeterminismInput tuple

```rust
pub struct DeterminismInput {
    pub language_revision: LanguageRevision,
    pub root_source: SourceBlob,
    pub modules: BTreeMap<ModuleId, SourceBlob>,
    pub initial_context: ContextSnapshot,
    pub input_events: Vec<InputEvent>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub semantic_clock: Vec<WallClockValue>,
    pub monotonic_clock: Vec<MonotonicTick>,
    pub capability_policy: FrozenCapabilityPolicy,
    pub budget: BudgetConfig,
    pub host_responses: Vec<RecordedHostResponse>,
    pub poll_schedule: Vec<PollSlice>,
    pub control_events: Vec<ControlEvent>,
    pub rules_revision: DeterminismRulesRevision,
}

pub struct ContextSnapshot {
    pub generation: u64,
    pub bindings: BTreeMap<String, RecordedValue>,
    pub loaded_modules: BTreeSet<ModuleId>,
    pub next_function_id: u64,
    pub next_class_id: u64,
}
```

各fieldの意味を次で固定する。

| field | 内容 |
|---|---|
| `language_revision` | 文法、Unicode data、float・error・path規則を含む言語仕様revision |
| `root_source` | 正規化前の有効UTF-8 source byte列とroot `ModuleId` |
| `modules` | import実行前解決に使うimmutableなmodule snapshot。key順は`BTreeMap` |
| `initial_context` | 実行開始前のbinding/cell/value、loaded module、rollbackで再利用しない単調identity counterのsnapshot。fresh contextは全map空・counter 0 |
| `input_events` | input payload、EOF、I/O errorの順序付き列 |
| `args` | hostが明示注入する引数。process argvを直接読まない |
| `env` | 許可済み環境snapshot。process environmentを直接読まない |
| `semantic_clock` | `now()`等、scriptから観測できるwall-clock response列 |
| `monotonic_clock` | deadline、host latency、scheduler control用の単調tick列 |
| `capability_policy` | execution作成時にfreezeしたgrant、deny rule、provider version |
| `budget` | 全上限、deadline、heap accounting revision |
| `host_responses` | host callごとのvalue/error/effect status/byte数/完了tick |
| `poll_schedule` | 呼出し順の`PollSlice.max_fuel`列。yield回数・位置を比較する場合は同一列を使う |
| `control_events` | pause、resume、cancelをどのcheckpointで観測させるかを示す列 |
| `rules_revision` | 本文書のbackend非依存規則revision。初期値1 |

`ContextSnapshot.bindings`の`RecordedValue`はtype tag付きDAG encodingを使い、List/Dict backing、cell、function code/captureをlocal object IDで1回だけ表現する。Dict/bindingはkey byte順、Listはindex順、functionは`FunctionId`・source hash・source node ID・capture cell IDを保存する。snapshot採取中はcontextを排他borrowし、秘密値を含み得るsnapshot/record storeはaudit redactionとは別に暗号化・access controlする。snapshotを復元できない型をhost valueとしてcontextへ保存してはならない。

`execution_id`、worker thread、OS process ID、実時間scheduler順はsemantic tupleへ含めない。audit byte列まで再現する場合は、注入`execution_id`、`AuditClock`、redaction keyもreplay metadataとして同じ値を渡す。

決定性を2層に分ける。どちらも同じ完全な`DeterminismInput`、したがって同じ`poll_schedule`と`control_events`を前提とする。**semantic determinism**はvalue/outcome/context/host effect/budget totalを比較し、yield eventを出力比較から除く。**execution-trace determinism**はyield/resume/poll index/audit sequenceまで比較し、tree/VM differential gateはこちらを使う。

slice量だけを変えるmetamorphic testは別入力間の強い追加性質であり、fake monotonic clockが全scheduler-only checkpointで同じtickを返し、terminalまでdeadline未到達、pauseなし、host response completion checkpoint固定の場合だけterminal/context/effect/budget totalの一致を要求する。production clockやdeadline到達可能な実行ではpoll scheduleが待ち時間とdeadline outcomeを変え得るため、異なるslice間の一致を保証しない。deadline check自体は各scheduleで[実行予算・協調実行仕様](execution-control.md)どおりqueue/yieldを含めて行う。

監査eventをbackend間比較する前に`normalize_for_conformance`を適用し、envelopeの`execution_id` / `timestamp`、ExecutionStartedの`engine_version` / `backend` / `mode`だけを除外する。`source_hash`、language revision、sequence、resource、decision、call ID、usage、error code等は除外しない。同じbackend・modeのrecord/replay byte一致試験では正規化せず比較する。

## 4. Backend非依存の規則

### 4.1 評価順とside effect順

1. statementはsource順に実行する。
2. unary operand、binaryのleft/right、callのcallee/arguments、list要素、dict key/valueは左から右へ評価する。
3. `and` / `or`は左を先に評価し、短絡した右辺は評価・課金・監査しない。
4. index assignmentはtarget解決、index、value、mutationの順とする。
5. call先の存在・arity・破壊対象妥当性を、現在の言語仕様で定めた位置に検査し、検査前に評価してはならないargumentを揃える。
6. 1 executionが同時に未完了にできるscript host callは1個だけとする。responseを受け取るまで次のscript statementへ進まない。
7. host call開始、capability decision、外部効果、結果公開の順序は監査順序と一致させる。
8. 最初の未捕捉error、budget、cancel、audit failureより後のscript side effectを実行しない。

### 4.2 importとsource

importは[言語仕様](language-spec.md)どおり、最初のscript statementより前に全件解決する。module resolverは[Capability仕様](capability-model.md)の注入adapterだけを使い、次を固定する。

- source内のimport specifierは正規化せずresolverへ渡す。filesystem resolverの相対path検査はCapability仕様の`RelativePath`規則に従う。
- resolverが返す`ModuleId`はopaqueで安定なUTF-8 identifierであり、同じlogical moduleには同じIDを返す。engineはpathとして再解釈・case fold・Unicode normalizeしない。
- `ModuleId`にabsolute local path、credential、query secretを含めない。
- 同一execution中のmodule graphはimmutable snapshotであり、host filesystemやresolver登録の後続変更を観測しない。
- 解決・parse・capability・budget失敗時は1文も実行しない。
- 正常時のmodule top-level statementはimport文の位置へ展開した順で実行する。

`source_hash`はroot sourceの正規化前byte列に対するSHA-256である。BOM除去、改行変換、Unicode normalizationを行わない。`import_graph_hash`は[組み込みAPI仕様](embedding-api.md)の`LinkedScript.import_graph().graph_hash()`を正本とし、同文書のbyte-level仕様をそのまま共有helperで呼ぶ。domain separatorは末尾NULを含むASCII `TSUMUGI-IMPORT-GRAPH-V1\0`、integerはu64 big-endian、stringは`length || UTF-8 bytes`であり、language revision、root hash、root import ID列、`ModuleId` byte順のnode列、各nodeのsource hashとsource順import ID列をencodeする。本文書側に別algorithmを実装しない。

### 4.3 path

scriptが扱うfilesystem pathは[Capability仕様](capability-model.md)の`RelativePath`とpath-handle adapter契約に従う。deterministic modeではOS固有separator、current directory、drive letter、case folding、symlink解決結果をscript意味論へ直接混入させない。filesystem providerのresponse、canonical resource label、errorはhost responseとしてrecordする。

pathの表示順・比較はproviderが返すpublic UTF-8 labelのbyte順とする。host pathをscriptへ公開しないproviderではopaque labelだけを使う。auditへraw pathを既定で記録しない。

### 4.4 localeとUnicode

- sourceとStringは有効UTF-8であり、内部の順序はUnicode scalar value順とする。
- 暗黙のNFC/NFD normalizationを行わない。見た目が同じでも異なるscalar列は異なる文字列である。
- Dictは`BTreeMap<String, Value>`を使い、key iteration、`keys`、`values`、Dict表示、deterministic serializationを同じ順序にする。
- process localeを参照しない。数値parse/formatの小数点は`.`、時刻formatはUTC、error codeはlocale非依存とする。
- 大文字・小文字変換等がUnicode tableへ依存する場合、`language_revision`へUnicode data revisionを固定する。Rust標準libraryのversion追従で無言に意味を変えない。

### 4.5 FloatとNaN

- FloatはIEEE 754 binary64で、fast-math、暗黙fused operation、backend別拡張精度を使わない。
- operationで生じたNaNはquiet NaN bit pattern `0x7ff8_0000_0000_0000`へcanonicalizeする。
- `NaN == NaN`は`false`、`NaN != NaN`は`true`とする。大小比較はすべて`false`とする。
- `-0.0 == 0.0`は`true`。deterministic binary encodingではsign bitを保持し、表示規則は言語仕様に従う。
- host responseからFloatを受け取る時もNaNをcanonicalizeし、Infinity・NaNをIntへ変換する規則は安定error codeで固定する。
- Floatのparse、format、serializeはlocale非依存の同一algorithmをtree/VM/record-replayで共有する。

### 4.6 error

内部Rust型名やdebug表示を規範errorへ埋め込まない。構造化errorは少なくとも次を持つ。

```rust
pub struct StableError {
    pub code: ErrorCode,
    pub script_type: String,
    pub message_id: ErrorMessageId,
    pub script_message: String,
    pub module_id: ModuleId,
    pub line: u32,
    pub column: Option<u32>,
    pub trace: Vec<StableFrame>,
}
```

`code`、`script_type`、`message_id`、source位置、trace frame順をbackend間で一致させる。`script_message`も`try` / `catch`から観測可能なため同じ日本語templateと値serializationを共有する。host向けlocalized displayは別層とし、determinism比較とaudit schemaへ使わない。

budget、deadline、cancel、audit failure、internal invariant、replay mismatchはhost control-plane terminalであり、script-visible `Value::Error`へ変換しない。

### 4.7 function identity

AUD-048は、規範言語仕様どおり「同じ動的function instanceだけが等しい」で解消する。

- function/lambda式を評価するたびに新しい`FunctionId`を発行する。
- 値clone、変数代入、collection格納、captureは同じIDを保持する。
- 同じsource位置・同じchunk・upvalueなしでも、別評価なら別IDとする。
- treeは現在のobject identityをこのIDへ明示化し、VMはclosure生成opcodeごとに新IDを付与する。
- IDはexecution内の比較専用で、scriptとaudit eventへ生値を公開しない。`ContextSnapshot`および暗号化・access controlされたrecord storeでは、同一executionのDAG identityを再現するlocal IDとして保存できるが、execution外の永続identityや認可tokenとして扱わない。

## 5. 注入hostとfake host

ambientなOS clock、process env/argv、stdin/stdout、filesystem、process exitへbackendから直接接続しない。[Capability仕様](capability-model.md)の個別adapterとcall contextを唯一の外部入口とする。以下の`HostBoundary`はそれらを束ねるengine内部dispatcherの擬似型であり、Capability仕様の公開trait群を置換したり第2のregistryを作ったりしない。Phase 2同期adapterは`Ready`へwrapし、Phase 4のcooperative extensionだけが`Pending`を返せる。

```rust
pub trait HostBoundary {
    fn resolve_module(&self, request: ModuleRequest, cx: HostCallContext)
        -> HostCallPoll<SourceBlob>;
    fn read_input(&self, request: InputRequest, cx: HostCallContext)
        -> HostCallPoll<InputEvent>;
    fn write_output(&self, request: OutputRequest, cx: HostCallContext)
        -> HostCallPoll<()>;
    fn wall_clock(&self, request: ClockRequest, cx: HostCallContext)
        -> HostCallPoll<WallClockValue>;
    fn call(&self, request: HostFunctionRequest, cx: HostCallContext)
        -> HostCallPoll<HostValue>;
}
```

`HostCallContext`はexecution ID、call ID、frozen capability、deadline、cancellation token、request/response残量、audit correlationを含む。providerはprocess-global stateを裏で参照してよいが、その観測結果をresponseとして明示し、record可能にする。

`FakeHost`は全providerをmemory上のqueue/mapで実装し、次を提供する。

- immutable module map、env、args
- scripted input/clock/host response/error列
- output、filesystem相当操作、業務operationのeffect log
- responseを返すmonotonic tickとcancel barrier
- requestの完全一致検査と未消費response検査

unit testとdifferential testは原則`FakeHost`を使い、実OS providerはadapter integration testだけで扱う。

## 6. Record / replay

record/replay logはaudit logと別物である。auditはredactedな説明記録であり、replayに必要なsecret/bodyを欠くため、audit streamからreplayしてはならない。

```rust
pub struct RecordIntent {
    pub index: u64,
    pub call_id: u64,
    pub operation: HostOperation,
    pub request_fingerprint: [u8; 32],
    pub prepared_at: MonotonicTick,
}

pub struct RecordCompletion {
    pub response: Option<RecordedHostResponse>,
    pub effect_status: EffectStatus,
    pub wall_timestamp: HostTimestamp,
    pub completed_at: MonotonicTick,
    pub disposition: RecordDisposition, // Finished | Cancelled | Detached
}

pub struct RecordToken { pub index: u64, pub nonce: u128 }

pub trait RecordStore: Send + Sync {
    fn prepare(&self, intent: &RecordIntent) -> Result<RecordToken, RecordStoreError>;
    fn finish(
        &self,
        token: &RecordToken,
        completion: &RecordCompletion,
    ) -> Result<(), RecordStoreError>;
}

pub struct ReplayEntry {
    pub intent: RecordIntent,
    pub completion: RecordCompletion,
}
```

`prepare`と`finish`はreturn前にdurableでなければならない。storeはindexを一意にし、同じtokenへの同一内容のretryをidempotent成功、異なる内容をprotocol errorとする。crash recoveryで`prepare`だけが残るentryは`effect_status = Unknown`相当であり、自動replayせずhost operatorの解決を要求する。

### 6.1 record

- host境界へ渡す直前のcanonical requestからfingerprintと`RecordIntent`を作り、`prepare`成功後だけ外部callを開始する。
- capability decision、raw response/error、effect status、byte数、clockを`RecordCompletion`へ保存し、`finish`成功後だけresponseをscriptへ公開する。
- cancelが外部効果開始前なら`Cancelled/None`、開始後に完了不明なら`Detached/Unknown`でfinishする。
- `prepare`失敗は外部call回数0の`RecordFailure`、外部call後の`finish`失敗は`RecordFailure { host_effects_may_remain: true }`でfail-closedにする。
- replayに必要なcredential/bodyを含み得るため、record storeはhostが暗号化・access control・retentionを実装する。

### 6.2 replay

- 実host effectを実行せず、次entryの`intent.operation` / `call_id` / `request_fingerprint`が一致した場合だけcompletionを返す。
- `Finished`以外、`effect_status = Unknown`、response必須operationのresponse欠落、不一致、entry不足、余剰entry、byte数不一致はcatch不能`ReplayMismatch` terminalとする。
- outputも実streamへ書かず、recordされたrequestと比較してeffect logへ追加する。
- wall/monotonic clockはentryから返す。scheduler sliceの違いでresponse順を変えない。
- replay中もcapabilityとbudgetを再評価し、record時より厳しいpolicy/budgetならその時点でdeny/terminalにする。recordを権限bypassに使わない。

同じexecution ID、AuditClock、redaction keyを注入すればaudit envelopeもbyte単位で再現できる。通常はそれらを変え、semantic event種別・payload・sequenceだけを比較する。

## 7. Audit schema

### 7.1 envelope

field名、型、意味をschema version 1として固定する。

```rust
pub struct AuditEnvelope {
    pub schema_version: u16,          // 常に1
    pub execution_id: ExecutionId,   // host供給128-bit ID
    pub source_hash: [u8; 32],        // root source SHA-256
    pub language_revision: String,
    pub sequence: u64,                // 0始まり、gapなし
    pub timestamp: HostTimestamp,     // 注入AuditClockのunix ns
    pub event: AuditEvent,
}

pub struct HostTimestamp {
    pub unix_nanoseconds: i128,
}
```

`timestamp`は注入host clockの値であり、順序の正本ではない。wall clockが後退しても書き換えず、`sequence`で順序を決める。replay時はrecord値を使う。`execution_id`はhostが一意に生成して渡し、engineがambient random sourceへ直接アクセスしない。

sequenceはStartedが0で、eventをlogical audit journalへappendする時にchecked incrementする。overflowは`AuditFailure::SequenceExhausted`でfail-closedとし、wrapしない。

### 7.2 event enum

```rust
pub enum AuditYieldReason {
    AdmissionQueued { resume_to: AdmissionPhase },
    SliceFuelExhausted,
    ExplicitYield,
    HostCallPending { call_id: u64 },
    AuditBackpressure,
    SchedulerPreempted,
    HostPaused,
}

pub enum AuditEvent {
    ExecutionStarted {
        engine_version: String,
        backend: Backend,                 // productionはTree
        rules_revision: u32,
        heap_accounting_revision: u32,
        budget: BudgetConfig,
        capability_policy_hash: [u8; 32],
        redaction_policy_id: String,
        mode: ExecutionMode,                // Live | Record | Replay
    },
    CapabilityDecision {
        operation_id: u64,
        call_id: Option<u64>,
        capability: String,
        action: String,
        resource: AuditPayload,
        decision: CapabilityDecisionKind,   // Allow | Deny
        rule_id: String,
    },
    HostCallStarted {
        operation_id: u64,
        call_id: u64,
        function: String,
        request: AuditPayload,
        request_bytes: u64,
    },
    HostCallFinished {
        operation_id: u64,
        call_id: u64,
        outcome: HostCallOutcome,
        response: AuditPayload,
        response_bytes: u64,
        error_code: Option<String>,
        effect_status: EffectStatus,        // None | Committed | Unknown
    },
    BudgetCharged {
        resource: BudgetResource,
        committed_delta: u64,
        released_delta: u64,
        usage_after: u64,
        peak_after: Option<u64>,
        charge_count: u64,
        reason: BudgetChargeReason,
    },
    Yielded {
        reason: AuditYieldReason,
        poll_index: u64,
        usage: BudgetUsage,
    },
    Resumed {
        previous_reason: AuditYieldReason,
        poll_index: u64,
        usage: BudgetUsage,
    },
    Terminal {
        outcome: TerminalOutcome,
        error: Option<AuditErrorPayload>,
        usage: BudgetUsage,
        import_graph_hash: Option<[u8; 32]>,
        context_committed: bool,
        host_effects_may_remain: bool,
    },
}
```

```rust
pub enum ExecutionMode { Live, Record, Replay }
pub enum CapabilityDecisionKind { Allow, Deny }
pub enum HostCallOutcome { Success, Denied, HostError, Cancelled, Detached }
pub enum EffectStatus { None, Committed, Unknown }
pub enum BudgetChargeReason {
    Statement,
    Expression,
    Operation,
    Function,
    Loop,
    HostCall,
    BulkElements,
    BulkBytes,
    Allocation,
    Source,
    Input,
    Output,
}
pub enum TerminalOutcome {
    Completed,
    Exited(u8),
    Denied,
    LinkError,
    RuntimeError,
    HostError,
    BudgetExceeded(BudgetResource),
    DeadlineExceeded,
    Cancelled,
    AuditFailure,
    InternalFailure,
    RecordFailure,
    ReplayMismatch,
}
pub struct AuditFrame {
    pub function: String,             // 最大256 UTF-8 bytes
    pub module_id: AuditField,        // Identifier/Path policy適用済み
    pub line: u32,
}
pub struct AuditError {
    pub code: String,                 // 最大128 UTF-8 bytes
    pub message_id: String,           // 最大128 UTF-8 bytes
    pub module_id: Option<AuditField>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub trace: Vec<AuditFrame>,       // 最大32 frames
    pub omitted_trace_frames: u32,
}
pub enum AuditErrorPayload {
    Full(AuditError),
    Emergency {
        code: String,                 // 最大128 UTF-8 bytes
        message_id: String,           // 最大128 UTF-8 bytes
        full_error_bytes: u64,
        full_error_sha256: [u8; 32],
    },
}
```

各normal audit eventのencoded長は最大60 KiBとする。redaction後payloadが超える場合、Body/Path/Identifierの順に`Omitted`へ縮退し、それでも超えるeventは`AuditFailure::EventTooLarge`とする。Terminalだけは失敗を再帰させず、`Full(AuditError)`を60 KiB以内にできない場合に`Emergency`へ置換する。Emergencyはredaction済みfull errorのbyte長とSHA-256だけを持ち、Terminal全体を4 KiB以下にする。したがって64 KiB Terminal reserveで必ずencodeできる。traceは先頭32 frameだけを保持し、残数を`omitted_trace_frames`へ記録する。

`ExecutionOutcome`と`TerminalOutcome`は上の全variantを1対1に対応させる。APIの`Exited { code, usage }`は`TerminalOutcome::Exited(code)`、`InternalFailure`は同名へ写像する。Phase 1/2内部bootstrapに別名の`InternalFault` terminal、Phase 3/4に別のusage型、Phase 6に別event enumを残さない。raw Rust error、backtrace、source本文をpayloadへ入れない。

operation IDとcall IDはexecution内で0から単調増加し、overflow時はfail-closedにする。1 host call requestは同じ`operation_id` / `call_id`を`HostCallStarted`、`CapabilityDecision`、`HostCallFinished`で使う。host call以外のmodule/path等のdecisionは`call_id = None`を許す。

### 7.3 BudgetChargedの集約

各statementごとのaudit eventで監査自体をDoSにしないよう、budget chargeはresourceごとに集約する。

- 同じpoll内の同resource・同reasonをaccumulatorへ足す。
- host call開始前、yield前、pause前、terminal前、または256 chargeごとにflushする。
- `charge_count`は集約した論理charge回数、`committed_delta`は合計、`usage_after`はflush後の累積値である。
- heap releaseは`released_delta`へ記録し、`usage_after`をlive heap、`peak_after`をpeak heapとする。
- Terminalのusageと全BudgetChargedのdeltaをcumulative/live resourceで突合し、per-item resourceは`BudgetPeaks`を突合できなければならない。

## 8. event順序と完全性

1 executionのlogical streamは次を満たす。

1. `ExecutionStarted`は必ずsequence 0に1件だけある。
2. root scriptのsemantic statementより前にStartedをjournalへappendし、sinkがackする。
3. import resolverを含むhost requestはlifecycle 3 event分をatomic reserveし、`HostCallStarted`、`CapabilityDecision`、`HostCallFinished`の順にする。
4. denyでもStarted/Finishedを両方出し、Finished outcomeを`Denied`、effectを`None`にする。
5. allowのdecisionがsinkにackされる前に外部効果を開始しない。
6. HostCallFinishedがackされる前にresponseをscriptへ公開しない。
7. budget accumulatorをflushしてからYielded/Terminalを出す。
8. resume後の最初のsemantic workより前にResumedを出す。
9. `Terminal`は1件だけで最後のeventである。Terminal append後はいかなるeventも発行しない。
10. Startedのあるexecutionには、sink delivery成否にかかわらずlogical journal上のTerminalが必ず1件ある。

compile自体がrootのparse errorを返し、`ExecutionHandle`を作成できなかった場合はexecution auditの対象外とする。handle作成後のlink/import/capability/budget失敗はStartedとTerminalを持つ。

pauseは`Yielded { reason: AuditYieldReason::HostPaused }`として記録し、resume時に`Resumed { previous_reason: HostPaused }`を出す。script executionの公開`YieldReason`へ`HostPaused`は追加しない。

## 9. Redaction policy

### 9.1 field class

```rust
pub enum AuditFieldClass {
    Public,
    Identifier,
    Path,
    Body,
    Secret,
}

pub enum RedactionMode {
    Reveal,
    HmacSha256,
    Omit,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub enum AuditDirection { Request, Response }

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct AuditFieldKey {
    pub operation: String,
    pub direction: AuditDirection,
    pub field: String,             // nameまたは0始まりargument index
}

pub enum AuditScalar {
    Null,
    Bool(bool),
    Int(i64),
    Float(u64),             // canonicalized binary64 bits
    Utf8(String),
    Bytes(Vec<u8>),
}

pub enum AuditValue {
    Scalar(AuditScalar),
    Sequence(Vec<AuditField>),
    Object(BTreeMap<String, AuditField>),
}

pub struct AuditPayload {
    pub fields: BTreeMap<String, AuditField>,
}

pub enum AuditField {
    Revealed {
        class: AuditFieldClass,
        value: AuditValue,
        original_bytes: u64,
    },
    HmacSha256 {
        class: AuditFieldClass,
        digest: [u8; 32],
        original_bytes: u64,
    },
    Omitted {
        class: AuditFieldClass,
        original_bytes: u64,
    },
}

pub struct RedactionPolicy {
    pub policy_id: String,
    pub identifier_mode: RedactionMode, // default Reveal
    pub path_mode: RedactionMode,       // default HmacSha256
    pub body_mode: RedactionMode,       // default Omit
    pub reveal_body_fields: BTreeSet<AuditFieldKey>,
    pub hmac_key: SecretBytes,
    pub max_revealed_body_bytes: u64,   // default 0
}
```

canonical CBORでは`AuditField`のvariantを`0=Revealed`、`1=HmacSha256`、`2=Omitted`、classを表の順のu8としてencodeする。`AuditPayload.fields`と`AuditValue::Object`はUTF-8 key byte順、Sequenceはargument/element順を維持する。host call引数は`fields["arguments"] = Sequence(...)`、operation固有metadataは別keyにする。各sequence element/object valueを独立した`AuditField`にするため、argument indexやnested fieldごとにredactionできる。nestingは64段までとし、それ以上のsubtreeはBody/Omittedへ縮退する。

`original_bytes`はredaction前のcanonical subtree payload長であり、omit/hash後も保持する。Secret classの親は子を走査・記録せず必ずsubtree全体を`Omitted`、Pathの既定は`HmacSha256`、Bodyは`body_mode == Reveal`かつkeyが`reveal_body_fields`にあり上限内の場合だけ`Revealed`となる。

| class | 例 | 既定 |
|---|---|---|
| Public | event種別、ID、hash、language revision、budget、count、error code | raw |
| Identifier | capability名、host function名、env key、rule ID、module logical name | raw。ただしhostがhash/omitへ変更可 |
| Path | filesystem path、module source location、URL resource identity | HMAC-SHA-256。rawを記録しない |
| Body | source、stdin/stdout、file/network body、host args/result、errorの自由文 | omitし、型・byte数・必要ならHMACだけ |
| Secret | credential、token、password、secret env value、redaction key | 常にomit。mode変更不可 |

`Secret<T>`を型で区別し、`Debug` / `Display` / generic serializerを実装しない。secretの存在とbyte数だけをPublic metadataとして記録できる。

BodyをRevealできるのは、policyのallow-listで特定host function fieldを`audit_safe`と明示し、`max_revealed_body_bytes > 0`の場合だけである。上限を超えたbodyはtruncationせず全体をomitし、`truncated = true`ではなく`omitted = true`とbyte数を記録する。prefixだけを残してsecretを漏らさない。

pathはproviderが認可に使ったcanonical resource labelのfield payload全体へHMAC-SHA-256を適用する。host absolute pathをlabelへ含めない。単純SHA-256は辞書攻撃に弱いため使わない。deny eventでもraw requested pathを出さない。

error messageはpath、argument、host responseを含み得るため、auditでは安定`ErrorCode`、`message_id`、source位置、redacted parameter summaryだけを保存する。script-visible messageをそのまま保存しない。

### 9.2 Canonical encodingとrequest fingerprint

schema version 1のbyte表現はRFC 8949 deterministic CBORの次のsubsetへ固定する。

- structはRust field宣言順のCBOR array、enumは`[u16 tag, field0, ...]`、`Option`は`[0]`または`[1, value]`でencodeする。mapとしてfield名をencodeしない。
- unsigned/signed integerは値を表せる最短幅、Stringは正規化しないUTF-8 text、byte列はCBOR byte string、bool/nullは標準primitiveとする。
- `HostTimestamp.unix_nanoseconds`は16 byte two's-complement big-endian byte string、Floatはcanonicalized binary64 bitsの8 byte big-endian byte stringとし、CBOR float型を使わない。
- `BTreeMap`は`[[key, value], ...]`としてUTF-8 key byte順、`BTreeSet`もsort済みarray、Vec/sequenceは元順でencodeする。
- `BudgetConfig`、`BudgetCounters`、`BudgetUsage`は[実行予算・協調実行仕様](execution-control.md)のfield宣言順を使う。unknown schema fieldを末尾へ追加せず、schema versionを上げる。
- `AuditEnvelope`は`[schema_version, execution_id(16 bytes), source_hash(32 bytes), language_revision, sequence, timestamp, event]`である。
- `AuditEvent` tagは`0=ExecutionStarted`、`1=CapabilityDecision`、`2=HostCallStarted`、`3=HostCallFinished`、`4=BudgetCharged`、`5=Yielded`、`6=Resumed`、`7=Terminal`とする。payload fieldは7.2節の宣言順である。
- `AuditField` / class / direction / nested valueのtagは9.1節の宣言順を0始まりで使う。HMAC対象と`original_bytes`はredaction前subtreeのこのencodingである。

record用`RecordedHostRequest`は、operationのstable ID、host function IDまたはcapability operation ID、language revision、capability set ID、型tag付きraw argument/resource payloadを持つ。ValueはNull/Bool/Int/Float/String/List/Dict/Errorの順を0始まりtagとし、Listは要素順、Dictはkey UTF-8 byte順、Floatは上記8 bytes、String/Bytesは長さ付きCBORでencodeする。execution ID、call ID、timestamp、worker、redaction結果は含めない。

```text
fingerprint_input = "TSUMUGI-HOST-REQUEST-V1\0"
                  || deterministic_cbor(RecordedHostRequest)
request_fingerprint = SHA-256(fingerprint_input)
```

domain separatorは末尾NULを含むASCII生byteである。record storeはraw canonical requestまたは再計算に必要な暗号化payloadを保持し、replayは同じ共有encoderでfingerprintを計算する。audit encoder、record encoder、予算byte counterが別実装のsize推定を持つことを禁止する。

## 10. Audit sink契約

```rust
pub trait AuditSink: Send + Sync {
    fn submit(
        &self,
        batch: Arc<[AuditEnvelope]>,
        waker: AuditWaker,
    ) -> AuditSubmit;
}

pub enum AuditSubmit {
    Ack(AuditAck),
    Pending(AuditTicket),
    Failed(AuditSinkError),
}

#[derive(Clone)]
pub struct AuditWaker { /* Arc<WakeState>, Send + Sync */ }
impl AuditWaker { pub fn wake(&self); }

pub struct AuditTicket { /* private, Send + Sync */ }

impl AuditTicket {
    pub fn register_waker(&self, waker: AuditWaker);
    pub fn try_take(&self) -> Option<Result<AuditAck, AuditSinkError>>;
    pub fn cancel(&self);
}

pub struct AuditAck {
    pub execution_id: ExecutionId,
    pub through_sequence: u64,
}
```

- `Pending(ticket)`ならexecutionを`Yielded(AuditBackpressure)`にする。sinkはack/errorをticketへ一度だけ格納して`AuditWaker`をwakeし、作成thread上の次回`ExecutionHandle::poll`が`try_take`して再開する。Pending batchの`Arc`はack/errorまで不変で、再送も同一byte列を使う。
- cancel/drop時はticketへ取消要求を送る。ただし未ack eventを捨てず、logical journalまたはbounded orphan-audit queueがdeliveryを引き継ぐ。
- sinkは`(execution_id, sequence)`をidempotency keyとして重複を除去する。
- ackは連続sequenceだけを認め、gap、別execution ID、未送信sequenceのackをsink protocol errorとする。
- deliveryはat-least-once、logical eventはexactly-onceである。ack喪失時は同じenvelopeを再送する。
- 1 execution内のbatchとevent順序を維持する。異なるexecution間の順序は保証しない。
- sink callback中に同じEngineのcompile/start/poll/pause/resume/cancel、host function、別audit emitを呼んではならない。thread-local reentrancy guardで検出し`AuditSinkError::Reentrant`にする。
- engine/scheduler/context lockを保持したままsinkへ入らない。

### 10.1 fail-closedを既定とする

安全性を優先し、初期実装のpolicyは`AuditFailurePolicy::FailClosed`だけとする。fail-open optionは提供しない。

engineはsinkの前にper-execution bounded logical journalへeventをappendする。Started時にTerminal 1件分のevent-count slotと64 KiB emergency byte slotを予約し、通常eventはこの領域を使えない。

- Startedがackされるまでscript/import host callを開始しない。
- permanent sink error、protocol error、audit budget超過では新しいscript workを停止する。
- pending host callを`Cancelled`または`Detached`のHostCallFinishedで論理的に閉じる。
- script内状態を[実行予算・協調実行仕様](execution-control.md)どおりrollbackする。
- emergency slotへ`Terminal(AuditFailure)`をappendし、その後のevent生成を禁止する。
- callerにはTerminalがjournalへappendされた後でのみ`AuditFailed`を返す。sinkが回復すればjournalを再送できる。

process OOM等で事前予約したTerminal領域すら使えない状況はhost process障害であり、言語level保証外である。通常のbudget/backpressure/sink error経路ではStart/Terminal各1件を必ず維持する。

### 10.2 backpressure

各executionのaudit drain queueは最大1 MiB、engine全体は最大16 MiBとする。上限へ達したらsemantic executionを進めずAuditBackpressureでyieldする。queueを捨てたり古いeventを上書きしない。

backpressure中もexecution deadlineとcancelを監視する。deadline/cancel時は通常event領域を使わず、未flush budgetを既存bufferへ可能な範囲で統合し、予約済みTerminalをappendする。sinkが永久に`Pending`のままdeadlineへ達した場合のoutcomeは`AuditFailure::BackpressureDeadline`とし、scriptのDeadlineより優先する。これにより「実行結果は成功だが監査が欠ける」状態を作らない。

## 11. 監査自体の予算

監査はscriptのfuel、output、host-call budgetへ課金しない。監査eventがさらにBudgetChargedを発生させる再帰を避ける。代わりに独立したcontrol-plane budgetを使う。

```rust
pub struct AuditBudget {
    pub max_events: u64,                    // default 100,000、4以上
    pub max_encoded_bytes: u64,             // default 16 MiB
    pub max_pending_bytes: u64,             // default 1 MiB / execution
    pub terminal_reserve_events: u64,       // fixed 1
    pub terminal_reserve_bytes: u64,        // fixed 64 KiB
    pub host_call_close_reserve_events: u64,// fixed 2
    pub host_call_close_reserve_bytes: u64, // fixed 128 KiB
}
```

encoded byte数は9.2節のdeterministic CBORでencodeした実byte長とする。encoding前に同じencoderのsize passで最大長をchecked arithmeticによりreserveし、write passの長さが一致しなければinternal audit failureとする。

- StartedとTerminalもevent count・encoded bytesへ含める。Started append時にevent 1件と64 KiBをTerminal専用にreserveし、通常eventは`committed_events + requested_events <= max_events - terminal_reserve_events - active_close_reserve_events`かつ通常byte残量内の場合だけappendできる。
- host call開始前に、`HostCallStarted`自身の通常1 event/最大60 KiBと、後続`CapabilityDecision` + `HostCallFinished`用の2 events/128 KiBをatomicにreserveする。予約できなければHostCallStartedも外部callも行わずAuditFailureにする。Decision/Finishedはclose reserveを消費し、pair完了時に未使用byteをrefundする。1 executionの未完了host callは1個だけなのでclose reserveも最大1組である。
- `max_events < 2`、`terminal_reserve_events != 1`、`terminal_reserve_bytes < 64 KiB`、`host_call_close_reserve_events != 2`、`host_call_close_reserve_bytes < 128 KiB`はconfig errorとする。Terminalは専用event/byte reserveだけを使い、通常上限を使い切っても必ずappendできる。低いmax_events/bytesによりhost lifecycle 3 eventsを予約できないconfigは構築可能だが、host callを効果開始前のAuditFailureにする。
- 上限超過は`AuditFailure::BudgetExceeded`でfail-closedにする。
- BudgetChargedを前節どおり集約し、監査event数をscript statement数へ単純比例させない。
- hashing、HMAC、encodingはhost control-plane workでfuelへ課金しないが、入力sizeはsource/I/O/host budgetで既に有限でなければならない。

## 12. Record/replayとauditの関係

| 項目 | audit | record/replay |
|---|---|---|
| 目的 | 誰が何を許可・実行し、どう終了したかの説明 | 外部responseを再現して同じ実行を得る |
| body | 既定omit/redacted | replayに必要なら暗号化して保存 |
| sink失敗 | fail-closed | record時fail-closed、replay時は事前読込失敗 |
| 順序key | execution ID + sequence | entry index + call ID |
| host effect | 要約とeffect status | request fingerprintとraw response |

record entryとaudit host callは同じcall IDを使う。`RecordStore::prepare`をdurableにし、auditのCapabilityDecision ackも完了してから外部効果を開始する。外部call終了後は`RecordStore::finish`をdurableにし、HostCallFinishedをackしてからscriptへ値を返す。

replayでは実外部効果を起こさず、監査に`mode = Replay`を記録する。auditはreplay entryから再生成し、record済みのredacted audit payloadを盲目的にコピーしない。現在のredaction policyを適用する。

## 13. 指定AUD項目との関係

### AUD-019: engine固有error kind/message

Phase 5のproduction gateで解消する。StableErrorのcode、script type、message ID、script-visible message、source位置、traceをtree/VMで完全一致させる。auditはraw messageではなくcode/message IDを使うためschemaは表示文変更から分離されるが、VMのexperimental解除にはmessageを含むscript観測も一致が必要である。

### AUD-022: differential・limit境界・fuzz

既存harnessのtimeout、完全一致golden、一時directory分離を維持し、determinism tuple、host effect log、budget charge、audit streamを比較対象へ追加する。型の組合せだけでなく、同一source位置から複数instanceを作る生成履歴、pause/resume、cancel race、record/replay、sink failureをmatrix/fuzzへ含める。

### AUD-024: import・REPLの状態commit

[実行予算・協調実行仕様](execution-control.md)で方針を確定した。Completed/Exitedで終了したexecutionだけscript stateをcommitし、`Denied`、未捕捉runtime error、`HostError`、budget、deadline、cancel、audit/record/replay/internal failureはexecution開始時点へrollbackする。execution中にcatchされ最終的にCompleted/Exitedへ到達した通常errorはcommit側である。既にcommitした外部host effectはrollbackせず、Terminalへ`host_effects_may_remain`を記録する。tree/VMは同じmutation journal契約を使う。

### AUD-048: function identity

本文書4.7の`FunctionId`で解消する。同じsource/chunkでも動的評価ごとに別instanceとし、VMへclosure生成単位のidentity tokenを追加する。treeをcompile chunk共有単位へ寄せて別instanceを等しくする案は、現行言語仕様に反するため採用しない。

## 14. 実装slice

各sliceは独立PRとし、前sliceの受入基準を満たしてから次へ進む。

### Slice 1: deterministic primitive

- `LanguageRevision`、`DeterminismRulesRevision`、StableError、ModuleId、source/import graph hashを実装
- locale/Unicode/Float/NaN/BTreeMap/path規則を共有helperへ集約
- AUD-048のFunctionIdをtreeへ導入

### Slice 2: HostBoundaryとFakeHost

- clock、env、args、stdio、module resolver、exit、filesystemをambient APIから注入hostへ移す
- FakeHostのscripted response/effect logを実装
- CLIは必要capabilityと実OS adapterを明示構築する

### Slice 3: tree規範化

- 全言語fixtureをFakeHost経由にし、value/error/effect/budgetを規範化
- AUD-019とAUD-024をtreeのStableError・transactionへ反映
- import実行前解決とmodule snapshot/hashを固定

### Slice 4: record/replay

- canonical request fingerprint、encrypted store interface、strict replay matcherを実装
- clock、input、output、host error、effect statusをrecord/replayする
- mismatchをcatch不能terminalへ接続

### Slice 5: audit core

- schema version 1、logical journal、sequence、BudgetCharged集約、redactionを実装
- nonblocking sink、ack/retry、reentrancy guard、fail-closed、emergency Terminal slotを実装
- execution-controlのyield/resume/terminalへeventを接続

### Slice 6: VM conformance

- Charge opcode、StableError、FunctionId、host boundary、transaction、audit semantic eventをtreeと一致させる
- differential matrix/fuzzを通し、既知非適合が0件になったcommitでのみexperimental flagを外す

## 15. 受入基準

Phase 5/6実装完了には次をすべて自動化する。

### 15.1 決定性

- same DeterminismInputを100回実行し、value/outcome、context、effect log、budget、semantic audit eventが一致する
- fresh contextだけでなく、binding/loaded module/FunctionId counterが異なる`ContextSnapshot`を復元し、同snapshotでは一致・1 field差では規定差を検出する
- 同じpoll scheduleではyield/resume/poll indexまで一致する。sliceだけを変えるmetamorphic testは、fake monotonic tick固定・deadline未到達・pauseなし・host completion checkpoint固定のprofileでterminal/context/effect/budget totalだけを比較する
- OS locale、timezone、current directory、environment、worker数を変えてもFakeHost実行結果が変わらない
- source hashはLF/CRLF、BOM、Unicode normalization差を区別し、同じbyte列では一致する
- module map挿入順を変えてもimport graph hashと実行結果が変わらない
- Dict iteration、nested serialization、path sortがBTreeMap/normalized byte順になる
- NaN payloadを変えて入力してもcanonical output/error/audit fingerprintになる
- slice fuel、worker割当、pause位置を変えても、control eventを同じsemantic checkpointへ正規化した場合のterminal結果とside effect順が一致する

### 15.2 fake host・record/replay

- clock/env/args/input/output/module/host functionがambient OSへアクセスしないことをdeny adapterで検証する
- recordした全fixtureをnetwork/filesystemなしでreplayし、terminal/context/effect/audit semantic streamが一致する
- RecordStoreのprepare/finish各失敗、idempotent retry、prepareだけ残るcrash recovery、Cancelled/Detached/Unknownを検証し、外部効果前後のfailure channelが規定どおりになる
- request順、引数、call ID、entry countを1つ変えると必ずReplayMismatchになる
- replayがcapabilityまたはbudgetをbypassしない
- record/audit store失敗時、外部効果開始前にfail-closedになる

### 15.3 audit完全性

- 全terminal pathでStartedがsequence 0に1件、Terminalが最後に1件だけある
- sequenceが0からgap・重複なく、再送してもsinkのlogical recordが重複しない
- 全host callにStarted/Finishedのpairがあり、call IDが一致する。deny、cancel、detach、host errorもpairを閉じる
- audit残量がHostCallStartedだけには足りるがlifecycle 3 eventには足りない境界で、Started/外部効果を0件のままAuditFailureになり、予約後のsink failureではclose reserveからDecision/Finishedを出す
- capability allow/denyが外部効果前にackされ、HostCallFinishedがscriptへの結果公開前にackされる
- BudgetChargedの合計とTerminal usageがcumulative/live resourceで一致し、SingleString/SingleSource/CollectionElementsはTerminal `BudgetPeaks`と実測最大candidateが一致する
- YieldedとResumedが交互に対応し、pause、host pending、audit backpressureを区別できる
- sink permanent error、protocol error、永続Pending、event/byte上限でscriptが成功扱いにならずAuditFailure terminalになる
- max_events/max_encoded_bytesを通常eventで使い切っても、予約済みevent 1件・64 KiBからTerminalを必ずappendできる
- Pending ticketのwake、spurious wake、ack/error一度だけ、cancel/drop後のorphan queue引継ぎでeventを欠落・重複しない
- sink callbackのEngine再入を拒否し、deadlockしない

### 15.4 redaction

- path、source、input/output body、host args/result、env value、credentialへ異なるcanary secretを入れ、audit encoded byteとsink diagnosticに平文が1 byteも現れない
- `AuditField`全variant/classをcanonical CBORでround-tripし、original byte count、HMAC digest、Omitted、Body allow-listをschemaどおり保持する
- deny/error/backtrace相当の経路でもraw path・body・secretが漏れない
- HMAC keyが違えばPath hashが変わり、同じkey/normalized pathなら一致する
- Secret型がDebug/Display/serializeできないcompile-fail testを持つ
- Body Reveal allow-listとbyte上限がない限り、hostがRevealを要求してもomitする

### 15.5 tree/VM differential

- stdout/stderrではなくFakeHost effect logをbyte単位・順序込みで比較する
- StableError全field、function identity生成履歴、context rollback、fuel/yield、`normalize_for_conformance`後のaudit payloadを完全一致させる
- AUD-019/024/048の専用fixtureと、AUD-022の生成matrix/fuzzで差分0件を確認する
- 差分が1件でもあるbuildではVM production featureを有効化できないCI gateを置く

## 16. Phase完了条件

Phase 5は、treeが唯一のproduction backendとなり、すべてのambient inputがHostBoundaryへ移り、DeterminismInputとbackend非依存規則が自動検証され、VMのproduction化に差分0件gateが設定された時点で完了とする。

Phase 6は、schema version 1の全event、redaction、bounded journal、fail-closed sink、host call correlation、budget集約、Started/Terminal完全性、record/replay連携が全terminal pathで検証された時点で完了とする。
