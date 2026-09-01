# Tsumugi 実行予算・協調実行仕様

最終更新: 2026-08-31

設計ステータス: **実装仕様確定・未実装**

## 1. 位置づけ

本文書は、[Tsumugi Manifesto](manifesto.md)と[ロードマップ](roadmap.md)のうち、マニフェスト実現ロードマップ Phase 3「包括的な実行予算」とPhase 4「協調実行と負荷制御」の実装仕様を定める。既存のstep上限、collection上限、call・AST・import深度上限を土台として再利用するが、本文書の型と状態機械はまだ実装されていない。

本文書は次の既存仕様と一体で実装する。

- [組み込みAPI仕様](embedding-api.md): Phase 1/2のEngine、compile/link、source identity、terminal channelの先行契約
- [Capability Model仕様](capability-model.md): Phase 2のfilesystem、env、clock、stdio、process、host functionのdeny-by-default権限境界
- [決定性・実行時監査仕様](determinism-and-audit.md): Phase 5/6の規範backend、注入host、audit event、record/replay
- [次期意味論・実装決定](semantic-decisions.md): 次期言語挙動、CLI、canonical error、catch可否

本文書のbudgetはsecurity sandboxではない。敵対的scriptを扱う場合は、別process、container、cgroup、OSのCPU・memory・time制限を併用する。

### 1.1 Phase 1/2先行実装との統合規則

Phase 1/2は本書の最終公開型の内部subsetとして実装する。先行実装専用の公開budget/state/poll型を作らず、本文書がPhase 3/4の最終契約である。

- `ExecutionRequest.budget`は最初から本書の有限`BudgetConfig`とし、独立したoptional deadline fieldや無制限sentinelを設けない。Phase 1/2でmeter未接続部分を内部bootstrapとして段階実装しても、その型をpublic stable APIにしない。
- `ExecutionState`は本書の`Created` / `Linked` / `Ready` / `Running` / `Yielded(YieldReason)` / `Paused(PausedState)` / `Terminal`だけを公開する。`Suspended`等の先行enumを作らない。
- `ExecutionOutcome`はterminal payloadの正本として維持し、`ExecutionState::Terminal`から同じoutcomeを参照する。stateとoutcomeでterminal理由を二重定義しない。poll中のlink失敗を表す`ExecutionOutcome::LinkError { error: LinkError, usage: BudgetUsage }`を使う。resolver denial/host error、link中budget/deadline/cancelはそれぞれ`Denied` / `HostError` / `BudgetExceeded` / `DeadlineExceeded` / `Cancelled`へmapし、module parse/compile、cycle、depth、symbol/arity mismatchだけを`LinkError`にする。
- Phase 1の`Engine::start(&LinkedScript, ...)`はLinkedから開始する互換入口として残せる。source/import予算を含む入口は`Engine::create_execution(&CompiledScript, ...)`でCreatedから開始し、linkを同じhandleで進める。
- Phase 1のcontext/handleに対する`!Send + !Sync`保証を維持する。engine内部scheduler stateとwake handleだけを`Send + Sync`にし、公開`ExecutionHandle`を別threadへ移動しない。
- AUD-024は本文書第10節の全language-state transactionを最初から最終契約とする。prefix commitや「terminal failureでもrollbackしない」という先行公開契約を設けない。

これらは同一実装系列で切り替え、同じbuildへ旧型と最終型、旧状態機械と最終状態機械を併存させない。

## 2. 用語と不変条件

| 用語 | 定義 |
|---|---|
| execution | 1個のroot scriptを、1個の`ExecutionContext`と不変の設定でterminalまで進める単位 |
| total budget | execution全体で消費できる資源上限。yield・pause・host call待ちを跨いで補充しない |
| slice | 1回の`poll`で実行を許可する量。公平性のための量子であり、total budgetではない |
| fuel | backend非依存の論理実行量。実時間やVM opcode数そのものではない |
| heap | executionから到達可能な論理allocationのlive byte数。OS allocatorの実測値ではない |
| reservation | 副作用・allocation前に上限内の枠を確保した状態 |
| terminal | `Completed`、`Exited`、`Denied`、`LinkError`、`RuntimeError`、`HostError`、`BudgetExceeded`、`DeadlineExceeded`、`Cancelled`、`AuditFailure`、`InternalFailure`、`RecordFailure`、`ReplayMismatch`のいずれか。terminal後は再開できない |
| yield | 非terminalの協調停止。continuationを保持し、再度`poll`できる |
| pause | hostの明示要求による非terminal停止。`resume`までschedulerへ戻さない |
| host call | capability境界を越えるclock、env、stdio、filesystem、process、登録host function等の呼出し |

次を全実装の不変条件とする。

1. すべてのexecutionは有限の`BudgetConfig`を持つ。無制限値を表すsentinelは提供しない。
2. total budgetのusageはyield・pause・resumeを跨いで単調に維持する。live heapだけは解放により減少できる。
3. budget超過、deadline、cancel、audit失敗はscriptの`try` / `catch`から捕捉できない。
4. terminal遷移は1回だけであり、terminal後の`poll`、`resume`、context変更は`HandleError::Terminal`を返す。
5. backend内部の実装量ではなく、本文書の論理charge pointを課金する。treeとVMで同じscript・同じ入力・同じbudgetなら同じcharge列になる。
6. capability、budget上限、deadline、注入host、redaction policy、backend、root source、linked importはexecution作成後に変更しない。

## 3. 公開データ型

以下は実装時にそのままRustの公開型へ落とす擬似定義である。field名と単位は公開契約とする。

```rust
pub struct BudgetConfig {
    // 論理accounting契約。初期実装は1だけを受理する。
    pub heap_accounting_revision: u32,

    // 論理fuel unit
    pub total_fuel: u64,

    // logical byte / count
    pub max_live_heap_bytes: u64,
    pub max_string_allocations: u64,
    pub max_string_bytes: u64,
    pub max_single_string_bytes: u64,
    pub max_source_count: u32,
    pub max_source_bytes: u64,
    pub max_single_source_bytes: u64,
    pub max_import_count: u32,
    pub max_import_bytes: u64,
    pub max_collection_elements: u64,

    // call count / payload byte
    pub max_input_calls: u64,
    pub max_input_bytes: u64,
    pub max_output_calls: u64,
    pub max_output_bytes: u64,
    pub max_host_calls: u64,
    pub max_host_request_bytes: u64,
    pub max_host_response_bytes: u64,
    pub max_host_call_bytes: u64,

    // 注入MonotonicClockと同じclock domainの絶対時刻。単位はns。
    pub deadline: MonotonicInstant,
}

pub struct BudgetCounters {
    pub fuel: u64,
    pub string_allocations: u64,
    pub string_bytes: u64,
    pub source_count: u32,
    pub source_bytes: u64,
    pub import_count: u32,
    pub import_bytes: u64,
    pub input_calls: u64,
    pub input_bytes: u64,
    pub output_calls: u64,
    pub output_bytes: u64,
    pub host_calls: u64,
    pub host_request_bytes: u64,
    pub host_response_bytes: u64,
    pub host_call_bytes: u64,
}

pub struct BudgetPeaks {
    pub single_string_bytes: u64,
    pub single_source_bytes: u64,
    pub collection_elements: u64,
}

pub struct BudgetUsage {
    pub committed: BudgetCounters,
    pub reserved: BudgetCounters,
    pub live_heap_bytes: u64,
    pub reserved_heap_bytes: u64,
    pub peak_heap_bytes: u64,
    pub peaks: BudgetPeaks,
}

pub struct BudgetExceeded {
    pub resource: BudgetResource,
    pub limit: u64,
    pub used: u64,
    pub reserved: u64,
    pub requested: u64,
    pub unit: BudgetUnit,
    pub phase: ExecutionPhase,
}

pub enum BudgetUnit {
    Fuel,
    Bytes,
    Count,
    Elements,
}

pub enum BudgetResource {
    Fuel,
    HeapBytes,
    SingleStringBytes,
    StringAllocations,
    StringBytes,
    SingleSourceBytes,
    SourceCount,
    SourceBytes,
    ImportCount,
    ImportBytes,
    CollectionElements,
    InputCalls,
    InputBytes,
    OutputCalls,
    OutputBytes,
    HostCalls,
    HostRequestBytes,
    HostResponseBytes,
    HostCallBytes,
}

pub enum ExecutionPhase {
    Compile,
    Link,
    Run,
    HostCall,
    Commit,
}

#[derive(Clone)]
pub struct ExecutionRequest {
    pub execution_id: ExecutionId,
    pub capabilities: CapabilitySet,
    pub arguments: Arc<[String]>,
    pub budget: BudgetConfig,
    pub cancellation: CancellationToken,
}

impl ExecutionRequest {
    pub fn new(
        execution_id: ExecutionId,
        capabilities: CapabilitySet,
        budget: BudgetConfig,
    ) -> Result<Self, ConfigError>;
    pub fn arguments(self, values: impl Into<Arc<[String]>>) -> Self;
    pub fn cancellation(self, value: CancellationToken) -> Self;
}
```

`ExecutionRequest`は必ず有限の`BudgetConfig`を所有する。deadlineは`BudgetConfig.deadline`だけに存在し、requestに独立したoptional fieldを持たせない。constructorはclock domain、accounting revision、deadlineが作成時点より後であることを検証する。

resourceを3種類に分類する。

- **cumulative**: Fuel、StringAllocations/Bytes、SourceCount/Bytes、ImportCount/Bytes、Input/Output/Host各count/bytes。成功時に`committed`へ加算し、通常は減らさない。
- **live**: HeapBytes。reserve/commit/releaseと`live_heap_bytes` / `peak_heap_bytes`で管理する。
- **per-item**: SingleStringBytes、SingleSourceBytes、CollectionElements。各1 objectのcandidate size/cardinalityだけを上限と比較し、成功時は累積加算せず`BudgetPeaks`を`max(old, candidate)`で更新する。文字列・sourceは同時に対応するcumulative bytesへ、collection backingはheapへ課金する。

`max_collection_elements`は**1個のListまたはDictの要素数**上限であり、execution内で生成した全要素数の累積ではない。List/Dict literal、push、新規Dict key、map/filter等の結果を変更前のcandidate cardinalityで検査する。Stringはcollection扱いせずsingle/string bytesで制御する。

per-item超過時の`BudgetExceeded`は`used = 0`、`reserved = 0`、`requested = candidate size/cardinality`、`limit = per-item limit`とする。成功したper-item検査は`BudgetCharged`を発行せず、Terminal/Yieldedの`BudgetUsage.peaks`で観測する。したがって監査delta総和との一致要件はcumulativeとlive resourceに適用し、per-itemはpeak一致を検査する。

`MonotonicInstant`は注入した`MonotonicClock`だけが生成するopaqueな`u64` nanosecond tickである。異なるclock instanceから作られたdeadlineを渡した場合、execution作成を`ConfigError::ForeignClock`で拒否する。`now >= deadline`をdeadline到達とする。pause、admission queue、host call待ちの時間もdeadlineに含める。

### 3.1 既定値

`BudgetConfig::standard(clock)`は`clock.now() + 30 s`をdeadlineとし、次を設定する。addition overflowまたはclock errorならconfig生成を失敗させる。

| field | 既定値 |
|---|---:|
| `heap_accounting_revision` | 1 |
| `total_fuel` | 1,000,000 fuel |
| `max_live_heap_bytes` | 64 MiB |
| `max_string_allocations` | 1,000,000 |
| `max_string_bytes` | 64 MiB |
| `max_single_string_bytes` | 8 MiB |
| `max_source_count` | 1,025（root 1 + import 1,024） |
| `max_source_bytes` | 16 MiB |
| `max_single_source_bytes` | 2 MiB |
| `max_import_count` | 1,024 |
| `max_import_bytes` | 16 MiB |
| `max_collection_elements` | 1,000,000 |
| `max_input_calls` | 10,000 |
| `max_input_bytes` | 8 MiB |
| `max_output_calls` | 10,000 |
| `max_output_bytes` | 8 MiB |
| `max_host_calls` | 10,000 |
| `max_host_request_bytes` | 8 MiB |
| `max_host_response_bytes` | 16 MiB |
| `max_host_call_bytes` | 24 MiB |

`MiB`は1,048,576 byteである。`heap_accounting_revision != 1`は`ConfigError::UnsupportedAccountingRevision`でexecution作成前に拒否する。すべての上限は0を許し、該当操作を最初から禁止できる。deadlineだけは作成時点より後でなければならない。

## 4. Fuel課金

### 4.1 課金表

| charge point | charge | 備考 |
|---|---:|---|
| statementへ入る | 1 | 到達しないstatementは課金しない |
| expressionへ入る | 1 | literal、変数参照、call式を含む |
| unary / binary / compare / index / assignmentの論理operation | 1 | operandのexpression chargeとは別 |
| language-level function invocation | 5 | user function、lambda、core builtin、callbackで共通 |
| loop iteration開始 | 2 | condition・iterable・bodyのchargeとは別 |
| host call attempt | 10 | 許可・拒否・host errorのいずれでも課金する |
| collection elementを走査・copy・生成 | 1 / element | `map`、`filter`、`sort`、比較、serialize等 |
| UTF-8 payloadをscan・copy・encode/decode | `ceil(bytes / 64)` | 0 byteは0。string、source、I/O、host payloadに適用 |
| AST node生成またはlinked node複製 | 1 / node | compile・import link中 |
| VM `Charge` opcode dispatch | 0 | opcode payloadに上記論理chargeをencodeする |
| その他のVM内部opcode dispatch | 0 | lowering差をscriptのfuel差にしない |

VM compilerはstatement、expression、operation、function、loop、bulk workの境界へ`Charge` opcodeを挿入する。tree evaluatorは同じ共有`FuelSchedule` APIを呼ぶ。compiler optimizationでcharge pointを削除・併合してはならない。VM固有のstack操作やjump数は課金しない。

`ceil(bytes / 64)`は`bytes == 0 ? 0 : 1 + (bytes - 1) / 64`でoverflowなしに計算する。collection/string builtinは長い処理を最大256 elementsまたは16 KiBごとの小chunkへ分け、chunk前にfuelとcancel/deadlineを確認する。

### 4.2 total fuelとslice fuel

`total_fuel`はterminalまでの上限であり、`poll`ごとの`PollSlice::max_fuel`は公平性の量子である。

- 次の論理chargeがtotal残量を超える場合は`BudgetExceeded(Fuel)`へterminal遷移する。
- total残量はあるがslice残量を超える場合はchargeせず`Yielded(SliceFuelExhausted)`を返す。
- 1個の固定chargeを分割しない。`PollSlice::max_fuel`は16以上とし、bulk workは前述のchunkへ分割する。
- slice終了でtotal fuelは補充しない。slice内で実際にcommitしたfuelだけをtotal usageへ加える。

## 5. Heap accounting

### 5.1 論理サイズ

heap quotaはallocator、platform、Rust compilerに依存しない論理サイズを使う。以下の定数を`HEAP_ACCOUNTING_REVISION = 1`として固定する。

| allocation | logical bytes |
|---|---:|
| `Value` slot / captured cell | 32 |
| UTF-8 `String` body | 24 + byte length |
| `List` body | 24 + 32 × element count |
| `Dict` body | 24 + 64 × entry count + 各keyのUTF-8 byte length |
| tree function instance | 64 + 16 × captured cell reference count |
| VM function instance | 48 + 16 × upvalue reference count |
| AST program root | 64 |
| AST node | 64 + nodeが所有するidentifier/string literalのbyte length |
| bytecode chunk | 64 + 16 × opcode count + 32 × constant slot count |
| imported module record | 96 + normalized module IDのUTF-8 byte length |
| continuation frame | 96 + frameが所有するlocal slot 32 × count |
| exception handler / loop handler | 32 |
| rollback journal entry | 48 + 保持する旧valueの到達payload |

`Value::Str`はValue slotとString body、`Value::List`はValue slotとList body、そこから初めて到達する子payloadを課金する。Dict keyはentryの64 bytesにString header 24 bytesを含むため、追加するのはkey payload byteだけである。

### 5.2 `Rc`共有と二重計上回避

すべてのheap-owned objectへ単調増加する`AllocationId(u64)`を付け、executionごとに`AllocationLedger`を持つ。

- 同一execution内で同じ`AllocationId`へ複数の`Rc`参照があっても1回だけ課金する。
- `Rc::clone`、closure capture、変数代入で既存objectを共有するだけなら追加課金しない。
- copy-on-write、string連結、collection拡張で新objectを作る場合は新しい`AllocationId`を発行し、allocation前に全logical bytesをreserveする。
- objectへの最後のexecution内参照が消えた時点でlive heapをreleaseする。cycle等により到達可能性が残るobjectはreleaseしない。
- 物理artifactを複数executionが共有しても、各executionは自分のquotaへ1回ずつ論理サイズを課金する。一方のexecutionのbudgetを他方が肩代わりしない。
- `AllocationId`の発行overflowは`InternalFailure(AllocationIdExhausted)`であり、0へwrapしない。

`ExecutionContext`に以前から残る変数・closure・collectionは、新executionを`Linked`へ進めるときに到達graphを反復worklistで走査し、baseline live heapとして課金する。baselineが上限を超える場合はscriptを1文も実行せず`BudgetExceeded(HeapBytes)`となる。再帰走査は使わず、同じ`AllocationId`をvisited setで除外する。

### 5.3 string、source、import

- `string_allocations`と`string_bytes`は成功した新規String bodyの累積値で、解放しても減らさない。substringが既存bodyを共有する実装なら新規allocationとして数えず、新bodyをcopyすれば数える。
- `max_single_string_bytes`はUTF-8 payload 1個の長さに適用する。headerは含めない。
- `source_count`はrootを1として、読み込んだimport sourceごとに1増やす。同一normalized module IDのcache hitは増やさない。
- `source_bytes`はrootとimportの生UTF-8 byte長の合計である。BOM・改行を正規化せず、hash対象と同じbyte列を数える。
- `import_count`はrootを含まず、初めて解決したnormalized module IDごとに1増やす。
- `import_bytes`はimport sourceの生byte長である。したがってimport sourceは`source_bytes`と`import_bytes`の両方へ意図的に課金する。
- imported module record、ASTまたはbytecode、module IDはheapにも課金する。
- 既存の`MAX_AST_DEPTH`、`MAX_IMPORT_DEPTH`、`MAX_USER_CALL_DEPTH`は構造的上限として残し、byte/count budgetとは独立に先に検査する。

## 6. Input、output、host callの課金

### 6.1 共通規則

byte数はhost境界で実際に受け渡すUTF-8またはbinary payloadの長さであり、Rust objectのcapacityやtransport headerは含めない。serialize形式は[Capability仕様](capability-model.md)で操作ごとに固定する。

| 操作 | count | bytes |
|---|---|---|
| `input` request | dispatch開始時に`input_calls += 1` | 受け取ったpayloadを`input_bytes`へcommit |
| output request | dispatch開始時に`output_calls += 1` | hostへ渡すpayloadを`output_bytes`へcommit |
| 任意host call | capability判定前に`host_calls += 1` | requestを`host_request_bytes`、responseを`host_response_bytes`、双方を`host_call_bytes`へcommit |

stdioもhost callであるため、input/output固有counterに加えてhost call counterとrequest/response byteへ課金する。capability拒否でもcountとrequest bytesは課金し、response bytesは0とする。

単一callのrequest/response最大値は、host function descriptorの上限とexecution残量の最小値で決まる。providerへ`max_response_bytes`を渡し、providerは超過payloadを作成・読み込み切る前に内部signal `HostAdapterLimit::ResponseBytes`で停止する。dispatcherはこれをpublic `HostError`へ変換せず、`BudgetExceeded { resource: HostResponseBytes, ... }`へ変換する。descriptor上限とexecution累積上限を同時に超える場合は小さい上限を`limit`とし、同値ならexecution累積上限をprimaryとする。先に全bodyを無制限に読み込んでから拒否してはならない。

### 6.2 blocking host call契約

[Capability仕様](capability-model.md)のPhase 2同期adapterは、descriptorが`may_block = false`であり、その場で有限時間に完了する場合だけ`Ready`へwrapする。Phase 4では、同期traitの意味を変えず、blocking可能なadapter向けに次のcooperative extension traitを追加する。Phase 2の「v1にasync callbackを入れない」という制約はPhase 4まで維持され、`Pending`対応後も既存同期callbackを暗黙に別threadへ移さない。

`poll`を呼ぶthreadで、時間上限が証明できないI/O、lock待ち、DNS、network、process待ちを行ってはならない。cooperative host adapterは次のいずれかを返す。

```rust
pub enum HostCallPoll<T> {
    Ready(Result<T, AdapterError>),
    Pending(HostCallTicket<T>),
}

pub trait CooperativeAdapter<Request, Response>: Send + Sync + 'static {
    fn start(
        &self,
        context: &mut CapabilityCallContext<'_>,
        request: Request,
    ) -> HostCallPoll<Response>;
}

pub trait Wake: Send + Sync { fn wake(&self); }
#[derive(Clone)]
pub struct ExecutionWaker(Arc<dyn Wake>);
impl ExecutionWaker {
    pub fn new(wake: Arc<dyn Wake>) -> Self;
    pub fn wake(&self);
}

pub struct HostCallTicket<T> { /* private, Send + Sync */ }

impl<T> HostCallTicket<T> {
    pub fn id(&self) -> u64;
    pub fn register_waker(&self, waker: &ExecutionWaker);
    pub fn try_take(&self) -> Option<Result<T, AdapterError>>;
    pub fn cancel(&self);
}
```

各Phase 2同期traitのrequest/response型に対して、Phase 4でだけ対応する`CooperativeAdapter<Request, Response>`実装を登録できる。同期実装は常に`Ready`相当で完了し`Pending`を返すAPIを持たない。cooperative実装は同じcapability判定、budget reservation、deadline、cancellation、audit correlationを使用し、Phase 2 traitを別threadへ暗黙offloadして擬似的に`Pending`へ変換してはならない。

`Pending`ではexecutionを`Yielded(HostCallPending { call_id })`にし、ticketへexecutionの`ExecutionWaker`を1個登録する。adapter executorはresultをticketへ一度だけ格納してwakeし、作成thread上の次回`poll`が`try_take`する。wakeはcontinuationを別threadで実行しない。ticket dropまたはcancelはadapterへ取消要求を送り、遅着resultは破棄する。executorはEngine本体と別の有限thread/concurrency/queue上限を持ち、deadline、`CancellationToken`、request/response残量を受け取る。Engineのscheduler lock、context lock、audit sink lockを保持したままadapterを呼ばない。

providerはcancelを協調的に処理する。基盤APIがcancel不能なら、adapterはexecution deadline以下の有限timeoutを必ず設定し、cancel後の結果と副作用をscriptへ返さない。cancel時は監査上のhost callを`Cancelled`または`Detached`で閉じてからexecutionをterminalにし、遅着callbackは破棄する。

## 7. reserve / commit / refund

複数resourceを使う操作は、外部効果またはallocationの前に1個のatomic reservationとして処理する。

```rust
pub enum ControlStop {
    Cancelled,
    DeadlineExceeded { deadline: MonotonicInstant, observed: MonotonicInstant },
    BudgetExceeded(BudgetExceeded),
}

let reservation = budget.reserve(BudgetRequest { ... })?; // Result<_, ControlStop>
let actual = perform_bounded_operation(reservation.limits())?;
reservation.commit(actual)?; // actual <= reserved
// operation未開始なら reservation.refund()
```

1. `reserve`はcancelとdeadlineを先に確認し、該当時は独立した`ControlStop`を返す。その後`used + reserved + requested`を全resourceでchecked additionする。
2. 1つでも超える場合は何もreserveせず、固定優先順位の`BudgetExceeded`を返す。
3. `commit(actual)`は`actual <= reserved`だけを許し、reservedからactualを引いてcommittedへ足し、差分をrefundする。
4. operation未開始、allocation失敗、capability判定前の内部失敗では全額refundする。
5. 外部へrequestを渡した後、count、request bytes、fuelはrefundしない。responseが不明なままadapterをdetachした場合はresponse予約を全額commitする。
6. heap objectのdropによるlive byte減少は`release`であり、累積string/source/I/O counterのrefundではない。
7. reservation objectを未settleでdropした場合、debug buildだけでpanicするのではなく、productionでも`InternalFailure(UnsettledReservation)`へ遷移する。外部効果開始前なら自動refund、開始後なら全額commitする。

### 7.1 overflow

全加算・乗算・byte長変換は`checked_*`を使う。演算overflowは「実質無制限」とせず、そのresourceの`BudgetExceeded`として扱い、`requested = u64::MAX`を記録する。`usize`から`u64`へ変換できないplatformでは同様に超過とする。usage、sequence、AllocationIdをsaturating/wrapping更新してはならない。

### 7.2 複数超過の優先順位

同一atomic reservationで複数上限を超える場合は、次の先頭1件をprimary `BudgetExceeded`とする。deadlineはbudget resourceではなく独立した`ExecutionOutcome::DeadlineExceeded`であり、この一覧へ含めない。監査には同時に超えたresource一覧を補助fieldとして記録できるが、outcomeは1件だけである。

1. `Fuel`
2. `HeapBytes`
3. `SingleStringBytes`
4. `StringAllocations`
5. `StringBytes`
6. `SingleSourceBytes`
7. `SourceCount`
8. `SourceBytes`
9. `ImportCount`
10. `ImportBytes`
11. `CollectionElements`
12. `InputCalls`
13. `InputBytes`
14. `OutputCalls`
15. `OutputBytes`
16. `HostCalls`
17. `HostRequestBytes`
18. `HostResponseBytes`
19. `HostCallBytes`

同じcheckpointでcancel、deadline、budget超過が同時に観測された場合は`ExecutionOutcome::Cancelled`、`ExecutionOutcome::DeadlineExceeded`、`ExecutionOutcome::BudgetExceeded`の順にする。既にterminal遷移が完了していれば、そのterminal結果を変更しない。

## 8. CancellationToken

```rust
#[derive(Clone)]
pub struct CancellationToken { /* Arc<AtomicBool> + wake registration */ }

impl CancellationToken {
    pub fn cancel(&self) -> bool;      // falseなら既にcancel済み
    pub fn is_cancelled(&self) -> bool;
}
```

cancelはidempotentかつthread-safeで、最初の`false -> true`をlinearization pointとする。tokenはReady/Yielded/Paused/host call待ちのhandleをwakeする。

確認点は、各fuel charge前、bulk chunk間、host call開始前後、yield/resume、queue admission、commit直前である。Running中にcancelされた場合は次の確認点まで現在のbounded atomic operationを終えられる。host callの外部効果が既に始まっている場合、その効果のrollbackは保証しない。

cancelと正常完了が競合した場合、terminal stateのcompare-and-setに先に成功した側を採用する。正常結果がterminalへcommitした後のcancelは結果を変えない。cancelが先に観測され`Cancelled`へ遷移した後の値・host responseは破棄する。

## 9. ExecutionHandleと状態機械

### 9.1 公開型

```rust
pub struct ExecutionHandle<'engine, 'script, 'context> {
    /* Engine/Script共有借用、ExecutionContext排他借用、!Send + !Sync */
}

pub enum ExecutionState {
    Created,
    Linked,
    Ready,
    Running,
    Yielded(YieldReason),
    Paused(PausedState),
    Terminal,
}

pub struct PausedState {
    pub reason: PauseReason,
    pub resume_to: ResumeState,
}

pub enum ResumeState {
    Created,
    Linked,
    Ready,
    Yielded(YieldReason),
}

pub enum AdmissionPhase { Created, Linked }

pub enum YieldReason {
    AdmissionQueued { resume_to: AdmissionPhase },
    SliceFuelExhausted,
    ExplicitYield,
    HostCallPending { call_id: u64 },
    AuditBackpressure,
    SchedulerPreempted,
}

pub enum PauseReason {
    HostRequested,
}

pub struct PollSlice {
    pub max_fuel: NonZeroU64, // 16以上。既定10,000
}

pub enum PollResult {
    Yielded { reason: YieldReason, usage: BudgetUsage },
    Paused { state: PausedState, usage: BudgetUsage },
    Terminal { outcome: ExecutionOutcome, usage: BudgetUsage },
}

pub enum HandleError {
    WrongThread,
    InvalidState { operation: &'static str, state: ExecutionState },
    Terminal,
}

impl Engine {
    pub fn create_execution<'e, 's, 'c>(
        &'e self,
        script: &'s CompiledScript,
        context: &'c mut ExecutionContext,
        request: ExecutionRequest,
        link_options: LinkOptions,
    ) -> Result<ExecutionHandle<'e, 's, 'c>, StartError>;

    pub fn start<'e, 's, 'c>(
        &'e self,
        script: &'s LinkedScript,
        context: &'c mut ExecutionContext,
        request: ExecutionRequest,
    ) -> Result<ExecutionHandle<'e, 's, 'c>, StartError>;
}

impl ExecutionHandle<'_, '_, '_> {
    pub fn state(&self) -> ExecutionState;
    pub fn usage(&self) -> BudgetUsage;
    pub fn poll(&mut self, slice: PollSlice) -> Result<PollResult, HandleError>;
    pub fn pause(&mut self) -> Result<(), HandleError>;
    pub fn resume(&mut self) -> Result<(), HandleError>;
    pub fn set_waker(&mut self, waker: Option<ExecutionWaker>) -> Result<(), HandleError>;
    pub fn cancellation_token(&self) -> CancellationToken;
    pub fn outcome(&self) -> Option<&ExecutionOutcome>;
}
```

`create_execution`と`start`はhandle作成前に、active slotが空いていればactiveを、なければadmission queue slotを1個だけatomicに予約する。両方が満杯なら`StartError::Backpressure`を返し、handleを作らずcontextを変更しない。`create_execution`はphase Created、`start`はphase Linkedから始まる。queue slotを得たhandleの公開stateは`Yielded(AdmissionQueued { resume_to: Created | Linked })`で、linkを含むworkを一切行わない。FIFO先頭でactiveが空くとqueue slotをactive slotへatomic変換し、`resume_to`のCreated/Linkedへ戻してwakerで通知する。link/terminal/drop時は所有するslotを必ず1個だけ解放する。

public `ExecutionHandle`は`!Send + !Sync`で、作成したthread上からだけ操作する。`poll`だけがReady/YieldedをRunningへ進め、1 slice以内に戻る。`pause`はCreated/Linked/Ready/Yieldedでだけ成功し、`resume`はPausedでだけ成功する。Running中はmutable borrow中なので別操作できず、terminalでは`HandleError::Terminal`を返す。

`set_waker(Some(w))`は作成threadから呼び、以前のwakerを置換する。admission取得、host/audit ticket完了、cancel、deadline timerは状態をreadyにした後でwakerを呼ぶ。wakeはedge-triggered hintで複数通知を1回へcoalesceでき、waker callbackはhandleへ再入せずhost event loopへ通知するだけとする。lost wakeを避けるため、登録は「waker保存→ready flag再確認→readyなら即wake」の順に行う。`set_waker(None)`で解除でき、waker自体は`Send + Sync`で別threadから呼べる。hostはwake後に作成threadで`poll`する。busy-pollは不要である。

別threadから許可する操作は`CancellationToken::cancel()`とwaker invocationだけであり、continuationやcontextへのmutable referenceをhostへ公開しない。

Engineはworker threadを生成しない。`Engine`内の`AdmissionController`とrun-turn queueだけを`Send + Sync`とし、continuationは各handleの作成threadに留める。Ready execution IDをengine-wide FIFOへ置き、queue先頭のhandleだけがsemantic workを1 slice進められる。先頭でないhandleの`poll`はworkを行わず`Yielded(SchedulerPreempted)`を返す。hostはwakeされたhandleをpollする責任を持ち、先頭handleをpollしなければ全体の進行は停止するが、上限超過や順序飛越は起きない。

非terminal handleのdropはcancelをlinearizeし、pending host callを`Detached`で閉じ、language-stateをrollbackし、logical `Terminal(Cancelled)`をappendする。sinkがPendingならjournalをEngineのbounded orphan-audit queueへmoveしてからcontext borrowを解放する。orphan queue満杯時はdrop前に予約済みTerminal slotを使い、以降の新規executionを`AuditUnavailable`でfail-closedにする。Dropはblocking I/Oやpanicを行わない。

### 9.2 状態遷移

```text
create_execution --active slot予約---------------------------> Created
start --active slot予約--------------------------------------> Linked
create_execution/start --queue slot予約----------------------> Yielded(AdmissionQueued { resume_to })
Yielded(AdmissionQueued { resume_to }) --slot変換-------------> resume_to
Created --pollでlink成功--> Linked --pollでrun-turn登録-------> Ready --poll--> Running
Created/Linked --失敗----------------------------------------> Terminal
Running --slice/host/audit待ち--> Yielded --再queue----------> Ready
Created/Linked/Ready/Yielded --pause-------------------------> Paused
Paused --resume----------------------------------------------> resume_to
Created/Linked/Ready/Running/Yielded/Paused --cancel等-------> Terminal
```

- `Created`: rootの`CompiledScript`、context、config、tokenを所有し、active slotを取得済み。script文は未実行。
- `Linked`: import graphを実行前解決し、source/import/compiled heapとcontext baselineを課金済みで、active slotを取得済み。
- `Ready`: link済みでrun-turn queueにいる。
- `Running`: 作成threadで`poll`しているcallerだけがcontinuationを進めている。
- `Yielded`: continuationを保持し、理由の解消後にrun-turn queue末尾へ戻る。`AdmissionQueued`だけはactiveではなくqueue slotを所有し、link workをまだ始めない。
- `Paused`: hostが明示resumeするまでrun-turn queueへ戻らない。`PausedState.resume_to`に直前のCreated/Linked/Ready/Yieldedを保持し、その状態へだけ戻る。pause前にactive slotを取得済みならactiveを、未取得ならqueue slotを消費し続け、deadlineも進む。
- `Terminal`: active/queue slot、pending reservation、continuationを解放する。context commit/rollback後の状態と`ExecutionOutcome`だけをcallerへ公開する。

`poll`はrun-turn queue先頭のReadyまたは再開可能なYieldedだけをRunningへ遷移させる。同期`Engine::run`はEngineに他のnonterminal handleがない場合だけ同じhandleをterminalまでpollし、存在する場合はscriptを開始せず`StartError::ConcurrentRunRequiresPolling`を返す。複数executionは各作成threadが公開`poll`をdriveする。

### 9.3 continuationへ保存するもの

再帰するRust call stackをcontinuationとして使ってはならない。少なくとも次をheap上の明示状態として保存する。

- AST cursorまたはprogram counter、value stack、保留中operand
- call frame、lexical scope、local/global slot、captured cell/upvalue
- loop frame、`return` / `break` / `continue`の保留control flow
- `try` handler stack、unwind位置、catch対象
- linked module table、import実行位置、loaded/loading marker
- `ExecutionContext`へのtransaction journalとbaseline generation
- `BudgetUsage`、reservation、AllocationLedger、slice残量
- cancellation/pause要求、poll index
- pending host callのcall ID、ticket、予約量、結果受渡し位置
- audit sequence、未flush event、redaction context

Paused/Yielded中にこれらを外部serializeしない。process終了を跨ぐ永続化、別Engineへの移送、version間resumeは非対応である。

## 10. Contextのcommit / rollback（AUD-024）

AUD-024の方針を次で確定する。

- `Completed`とscript要求の`Exited`だけが、execution中のscript-visible stateを`ExecutionContext`へcommitする。最終`AuditEvent::Terminal`は`context_committed = true`とする。
- catch済みruntime error（script操作中のcanonical `capability` / `host` errorを含む）は通常の制御フローであり、その後executionがCompleted/Exitedならcatchより前後の変更をcommit対象として維持する。
- `Denied`、未捕捉`RuntimeError`、`HostError`、`LinkError`、budget/deadline、cancel、audit/record/replay失敗、internal failureは、execution開始時点まで**全language-stateをrollback**し、最終`AuditEvent::Terminal`を`context_committed = false`とする。`Denied` / `HostError` terminalはscript handlerがまだ存在しないlink/control-plane失敗に使い、script実行中のcatch可能errorと混同しない。
- rollback対象はbindingの追加・削除・代入、List/Dict mutation、captured cell/upvalue、function定義、loaded module markerである。
- root parse/link失敗はscriptを実行していないためcontextを変更しない。失敗moduleはloaded扱いにしない。
- stdout、filesystem、network、database、process、host function等、既に境界外でcommitした効果はrollbackしない。監査のterminal eventへ`host_effects_may_remain = true`を記録する。
- yieldとpauseはtransaction境界ではなく、journalを保持する。resume後も同じexecution transactionを続ける。

実装はmutation journalまたはcopy-on-write generationを使い、context全体のdeep cloneを禁止する。journal自身と旧valueの保持はheap budgetへ課金する。rollback処理用に追加fuelを要求せず、terminal処理のhost control-plane workとして扱うが、work量はjournal entry数で有限でなければならない。

## 11. pause中の不変性

Paused中に許可する操作は`state`、`usage`のread、`resume`、`cancel`、handle dropだけである。次は変更不可とする。

- `BudgetConfig`とdeadline
- capability grant/policy
- host provider、clock、input、output、module resolver
- root source、linked import、language revision、backend
- `ExecutionContext`内の値
- audit sink、redaction、record/replay mode

budget追加、deadline延長、capability追加を行うAPIは提供しない。別設定で続けるにはterminal後に新executionを作る必要があり、terminal continuationは再利用できない。

## 12. Engine-wide負荷制御

```rust
pub struct EngineLimits {
    pub max_active_executions: NonZeroUsize, // 既定: available_parallelism、上限64
    pub max_queued_executions: usize,        // 既定: 256
    pub default_slice_fuel: NonZeroU64,      // 既定: 10,000
}
```

`available_parallelism`取得失敗時は1とし、`max_active_executions`の既定値は`min(available_parallelism, 64)`である。activeにはactive slotを予約したCreated、Linked、Ready、Running、通常Yielded、Paused、host call/audit待ちを含む。`Yielded(AdmissionQueued { resume_to })`だけはqueue slotへ数える。Pausedや外部I/O待ちをslot外へ出して無制限handleを抱えない。

handle作成時にactive slotが空いていればCreated/Linkedへ進め、空いていなければFIFO admission queueへ入れる。active slot解放時は最古のAdmissionQueued handleのqueue slotをactiveへatomic変換する。queue満杯なら待たずに次を返し、handleを作らない。

```rust
StartError::Backpressure {
    active: usize,
    queued: usize,
    limit: usize,
}
```

queue中もdeadlineとcancelを監視する。slot解放時は最古の`Yielded(AdmissionQueued { resume_to })`だけを対応するCreated/Linkedへ進める。link失敗、cancel、drop時もqueue/active slotを1回だけ解放する。priority APIは初期実装で提供しない。

### 12.1 fairness

run-turn queueはEngine全体でFIFO round-robinとし、continuation自体ではなくexecution IDとwakerだけを保持する。

1. Readyになったexecution IDをqueue末尾へ置く。
2. queue先頭のhandleによる1回の`poll`で最大1 sliceだけ実行する。
3. sliceでyieldしたexecutionを、同時点でReadyなexecutionより前へ挿入しない。
4. host call/audit完了でwakeしたexecutionも末尾へ置く。
5. 同じexecutionが連続してsemantic workを行えるのは、turn取得時点で他にReadyがない場合だけとする。

公平性は実行回数ではなくslice機会に対して保証する。Engineはhost threadをspawnせず、hostがqueue先頭handleをpollしない場合の進捗は保証しない。host adapterとaudit sinkは別のbounded queueを持ち、その詰まりをcaller threadへblocking伝播させずyield/backpressureへ変換する。

## 13. 既存環境変数からの移行

現行の`TSUMUGI_MAX_STEPS`と`TSUMUGI_MAX_COLLECTION_SIZE`はprocess-globalな互換入口として1 release cycleだけ残し、その後削除する。

| 旧設定 | 新設定 | 移行規則 |
|---|---|---|
| `TSUMUGI_MAX_STEPS` | `BudgetConfig.total_fuel` | embedding側の明示値が最優先。CLIで明示値がない場合だけ起動時に1回読み、同値へ設定してdeprecation warningを出す |
| `TSUMUGI_MAX_COLLECTION_SIZE` | `BudgetConfig.max_collection_elements` | 同上。`OnceLock`を使わず、executionごとのconfigへcopyする |

新fuelはstatement・expression等も課金するため、同じ数値でも旧step limitより早く到達し得る。互換mappingは安全側であり、旧実行量の完全再現を保証しない。`ExecutionContext::reset_step_budget()`は削除し、executionごとに新しい`BudgetUsage`を作る。REPLの各入力は1 executionとし、contextだけを引き継ぐ。

`MAX_AST_DEPTH`、`MAX_IMPORT_DEPTH`、`MAX_USER_CALL_DEPTH`は環境変数化せず、language revisionに紐づく構造上限として維持する。将来configurableにする場合もbudgetと同じ不変configへ置き、paused中変更は許さない。

## 14. 実装slice

各sliceは独立PRとし、前sliceの受入基準を満たしてから次へ進む。

### Slice 1: budget型とlegacy adapter

- `BudgetConfig`、`BudgetUsage`、`BudgetExceeded`、checked arithmetic、固定優先順位を実装
- CLIだけに旧環境変数adapterを置き、libraryは明示configを要求
- fake monotonic clockを導入
- まだ同期実行だが、既存step/collection検査を共有BudgetLedger経由へ移す

### Slice 2: source・string・heap・I/O accounting

- `AllocationId`とper-execution ledgerを導入
- Value、String、List、Dict、function、AST/chunk、import、journalを論理heapへ接続
- source/import/input/output/host count+bytesとreserve/commit/refundを実装
- baseline context graphの反復走査を実装

### Slice 3: explicit continuation

- tree evaluatorの再帰的な実行状態を明示frame/cursorへ変換
- `ExecutionHandle`と全`ExecutionState`、`poll`、slice fuel、yieldを実装
- 同期`Engine::execute`はhandleをterminalまでpollする互換wrapperにする

### Slice 4: cancellation・pause・transaction

- `CancellationToken`、pause/resume、checkpointを実装
- mutation journalでAUD-024のrollback規則を実装
- terminal後resume不可とraceのlinearizationを固定

### Slice 5: scheduler・host pending

- bounded active/queue、FIFO round-robin、backpressureを実装
- nonblocking host call ticket、wake、deadline/cancel伝播を実装
- adapter executorにも独立したconcurrency/queue上限を設定

### Slice 6: VM charge parity

- VMへ`Charge` opcodeと同じcontinuation/outcome契約を実装
- treeとのcharge trace、terminal boundaryをpaired testで一致させる
- 一致するまでVMはexperimentalであり、production schedulerへadmitしない

## 15. 境界受入テスト

実装完了には、通常unit testに加えて次をすべて自動化する。

### 15.1 budget

- 全fieldについてlimitちょうどが成功し、同一操作の`+1`が対応する`BudgetExceeded`になる
- 0上限、`u64::MAX`近傍、`usize`変換、加算・乗算overflowでwrapしない
- 複合reservationが部分的に成功せず、固定優先順位のresourceを返す
- operation開始前の失敗は全額refundし、開始後は規則どおりcommitする
- deadline直前は実行でき、fake clockがdeadlineと等しい時点で停止する
- budget超過を`try` / `catch`で囲んでもcatch bodyを実行しない
- host responseのdescriptor上限・execution累積上限のN/N+1と同時超過を試し、いずれもHostErrorではなく`BudgetExceeded(HostResponseBytes)`になる

### 15.2 heap

- 同じ`Rc`を複数変数・List・closureから参照しても1回だけ課金する
- copy-on-writeで新AllocationId分を課金し、最後の参照dropでlive heapをreleaseする
- root AST、VM chunk、function、import record、rollback journalが表の論理サイズと一致する
- context baselineがlimitちょうどならLinkedになり、`+1`ならscriptを実行せず失敗する
- 大量のallocate/freeはlive heapを回復してもstring allocation/countの累積上限を迂回できない

### 15.3 state・race

- 各非terminal stateからcancelでき、terminal eventは1回だけになる
- completion対cancel、deadline対cancel、host response対cancelをbarrierで同時発生させ、規定linearizationになる
- terminal後のpoll/resume/config変更をすべて拒否する
- yield/pause/resume後もstack、frame、handler、import、context、budget usageが失われない
- `Denied`、未捕捉error、HostError、budget/deadline、cancel、audit/record/replay/internal failureでbinding/List/upvalue/import markerがrollbackし、最終outcomeがCompleted/Exitedのときだけcatch済みerrorを含む変更をcommitする
- 全state×`poll`/`pause`/`resume`の合法・不合法matrixが`HandleError`表どおりで、terminal後はoutcome参照以外を拒否する
- active/queue満杯時のcreate/startがhandleを返さずcontextを変更しない。非terminal dropがrollback、Detached close、Terminal appendをblockingなしで完了する
- pause/resumeがCreated/Linked/Ready/Yieldedの元`resume_to`へ戻る
- rollback不能なfake host effectは残り、その事実がterminal監査へ反映される

### 15.4 fairness・backpressure・host

- 常にReadyなN executionが各1 sliceずつFIFOで進み、1 executionが連続独占しない
- active上限とqueue上限ちょうどを受理し、`+1`を即時Backpressureにする
- Pausedとhost待ちもactive slotを消費し、無制限admissionにならない
- queue待ち・pause・host待ち中にもdeadline/cancelが機能する
- blocking fake hostを使ってもcaller threadが無期限blockせず、run-turn queue上の別executionが進む
- response byte上限より大きいstreamを全量bufferせず途中で停止する

### 15.5 differential

- 同一fixtureのtree/VMで、charge trace、usage、yield位置、terminal reason、context commit結果、host effect順序が完全一致する
- AUD-022の網羅matrixへ各budget境界、REPL継続、pause/resume、cancel、host pendingを追加する
- subprocess timeout付きstress/fuzzでpanic、abort、OOM前の無制限allocation、terminal後実行がない

## 16. 完了条件

Phase 3は、全`BudgetConfig` fieldが全生成・I/O・host経路へ適用され、超過がcatch不能terminalとなり、logical heapとdeadline/cancelを含む境界テストが通った時点で完了とする。

Phase 4は、treeの全制御状態がcontinuationへ保存され、slice実行、yield、pause/resume、bounded admission、FIFO fairness、nonblocking host call、backpressure、race testが通った時点で完了とする。VMは[決定性・監査仕様](determinism-and-audit.md)の適合gateを通るまでexperimentalのままとする。
