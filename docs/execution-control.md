# Tsumugi 実行予算・協調実行仕様

最終更新: 2026-09-20

設計ステータス: **実装仕様確定・実装進行中**（第14節 Slice 1 実装済み。Slice 2 は完了（string / source / import accounting、heap accounting 基盤（`AllocationLedger`・§5.1 論理サイズ）、collection（`List`/`Dict`）の per-drop release（`Rc<Tracked<T>>` で生成時に課金し最後の参照 drop で release、tree/VM 両対応）、および String body の per-drop release（PR-b、`Value::Str` を `Rc<Tracked<RefCell<Value>>>` 化し `Value::Fn`/`VmFn` に header token を持たせる）、および AST / bytecode chunk / imported module record / rollback journal の per-drop 追跡（PR-d、`HeapToken = Tracked<()>` トークンで所有構造側から課金・release。tree は AST、VM は bytecode chunk を課金）、および string リテラル/連結/f-string 経路の課金（生成点で `track_result` を通し cumulative + live heap を課金、tree/VM 両対応）、および I-O accounting（input / output / host call の count + bytes を reserve/commit/refund で課金。stdio は host call として co-charge。host call 対象は filesystem read/write と stdio に限り、その他 host 境界 builtin は Phase 2）を実装済みで、これで Slice 2 は完了。VM の push/pop の full-clone や f-string リテラル部分の個別課金のため configured 上限で tree と live/peak・cumulative が食い違い得るが、charge trace の tree/VM 完全一致は第14節 Slice 6（VM が experimental の間）で解消する。Slice 3（explicit continuation）は blast radius が大きいため PR-a（公開 state-machine surface + poll-to-terminal core）→ PR-b（statement / block / loop の明示 frame）→ PR-c（関数呼び出し・try handler の明示 frame）→ PR-d（slice fuel + yield）へ分割する。PR-a（公開 state-machine surface + poll-to-terminal core）・PR-b（statement / block / loop の明示 frame stack。tree evaluator の `drive_body` driver ループ + `Frame`/cursor で `exec_program`/`exec_block`/ループ再帰を置換し、continuation_frame heap を配線）・PR-c（関数呼び出し・try handler の明示 frame。`eval_call` / callback の `call_fn_value` を `FrameKind::Call` を積む形へ変換し、共通 `run_driver` + `drive_call_body` で本体を駆動。VM の `CallFrame` をミラーし、`try` handler は PR-b の `FrameKind::Try` が VM の `TryHandler` をミラーする）を実装済み。次は PR-d（slice fuel + yield）。詳細は第14節 Slice 3 を参照）

## 1. 位置づけ

本文書は、[Tsumugi Manifesto](manifesto.md)と[ロードマップ](roadmap.md)のうち、マニフェスト実現ロードマップ Phase 3「包括的な実行予算」とPhase 4「協調実行と負荷制御」の実装仕様を定める。既存のstep上限、collection上限、call・AST・import深度上限を土台として再利用する。第14節 Slice 1（budget型・legacy adapter・共有BudgetLedger）は `src/budget.rs` に実装済みで、Slice 2 のうち string accounting サブスライス（per-item `SingleStringBytes`・cumulative `StringAllocations`/`StringBytes` の課金を共有 builtin handler へ tree/VM 共通で配線し、さらに string リテラル・`+` 連結・f-string の生成点でも `track_result` で課金）と source / import accounting サブスライス（per-item `SingleSourceBytes`・cumulative `SourceCount`/`SourceBytes`・`ImportCount`/`ImportBytes` を Link フェーズで root と import へ tree/VM 共通で課金）、heap accounting 基盤サブスライス（`AllocationId`・`AllocationLedger`・§5.1 論理サイズ関数・`HeapBytes` reserve/超過写像）、collection（`List`/`Dict`）の per-drop release サブスライス（`Rc<Tracked<T>>` で生成時に課金し最後の参照 drop で release、COW は delta 課金、tree/VM 両対応）、および String body の per-drop release サブスライス（PR-b、`Value::Str` を `Rc<Tracked<String>>` 化し、builtin 結果を dispatch 境界の `track_result` で live heap 課金・最後の参照 drop で release）、cell と tree/VM 関数 instance の per-drop release サブスライス（PR-c、cell 生成点で captured cell を課金し `Value::Fn`/`VmFn` の header token で function instance header を課金、drop で release）、および AST / bytecode chunk / imported module record / rollback journal の per-drop 追跡サブスライス（PR-d、所有構造側が `HeapToken = Tracked<()>` トークンで §5.1 論理サイズを課金し drop で release。§5.3「AST または bytecode」に従い tree は AST・VM は bytecode chunk を課金、import record は両 engine、rollback journal entry は両 engine で entry の固定 overhead を課金）、および string リテラル/連結/f-string 経路の課金サブスライス（生成点で `track_result` を通し cumulative + live heap を課金、tree/VM 両対応）、および I-O accounting サブスライス（input / output / host call の count + bytes を reserve/commit/refund（§7）で課金し、stdio を host call として co-charge。tree/VM の `print` / `input` / filesystem dispatch 境界へ共通配線）も実装済みで、Slice 2 は完了した。Slice 3 は PR-a（公開 state-machine surface + poll-to-terminal core）と PR-b（tree evaluator の statement / block / loop を明示 frame stack + driver ループへ変換し continuation_frame heap を配線）、PR-c（関数呼び出し・try handler の明示 frame。`eval_call` / `call_fn_value` を `FrameKind::Call` 化し、共通 `run_driver` + `drive_call_body` で本体を駆動。VM の `CallFrame` / `TryHandler` をミラー）を実装済みで、PR-d（slice fuel + yield）以降と cancel/pause、scheduler、VM charge parity は未実装である。

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

実装状況: ✅ 実装済み（`src/budget.rs`）。詳細は末尾の注記を参照。

- `BudgetConfig`、`BudgetUsage`、`BudgetExceeded`、checked arithmetic、固定優先順位を実装
- CLIだけに旧環境変数adapterを置き、libraryは明示configを要求
- fake monotonic clockを導入
- まだ同期実行だが、既存step/collection検査を共有BudgetLedger経由へ移す

> **Slice 1 実装注記（2026-09-11）**
>
> `src/budget.rs` に第3節の公開型（`BudgetConfig` / `BudgetCounters` / `BudgetPeaks` /
> `BudgetUsage` / `BudgetExceeded` / `BudgetUnit` / `BudgetResource` / `ExecutionPhase`）、
> `ControlStop`、`MonotonicClock` / `MonotonicInstant` / `FakeClock`、`CancellationToken`、
> `BudgetLedger` を実装した。`reserve_all` / `commit` / `refund` は第7節の checked
> arithmetic と第7.2節の固定優先順位（`BudgetResource::priority`）に従う。overflow は
> `requested = u64::MAX` の `BudgetExceeded` へ写像し wrap しない。`BudgetConfig::standard`
> は第3.1節の既定値を、`from_legacy_env` / `for_legacy` は `TSUMUGI_MAX_STEPS` /
> `TSUMUGI_MAX_COLLECTION_SIZE` を fuel / collection 上限へ写す legacy 入口を提供する。
>
> tree evaluator（`src/eval.rs`）と VM（`src/vm.rs`）の step 検査を `charge_fuel(1, Run)`
> へ、collection 検査を `check_collection_elements` / 共有 handler へ渡す
> `max_collection_elements` へ一本化した。`builtin_core` の process-global な `OnceLock`
> collection 上限は廃止し、上限値は各 execution の ledger config を単一の正本とする。
> `ControlStop` は Slice 1 では既存の `step_limit` / `collection_limit` エラーへ写像し、
> 観測挙動を変えない（既存 golden・回帰テストは不変で通過）。
>
> Slice 1 の範囲外（後続 slice）: fuel 全 charge point の展開・heap/string/source/I-O
> accounting は Slice 2。ledger charge 経路での deadline / cancel checkpoint は Slice 4
> （現状 `check_deadline` は no-op、cancel は `charge` 前確認のみ）。共有 builtin の
> per-item collection peak 反映（`note_collection_elements`）は未接続。library の
> 明示 config 要求は `Evaluator::with_budget` を用意済みだが、公開 `Engine` API への
> `ExecutionRequest` 配線は Slice 3。

### Slice 2: source・string・heap・I/O accounting

- string accounting（✅ 実装済み）: per-item `SingleStringBytes` と cumulative
  `StringAllocations`/`StringBytes` を `BudgetLedger::charge_string` /
  `charge_result_strings` で課金する。§7.2 の固定優先順位（`SingleStringBytes` <
  `StringAllocations` < `StringBytes`）に従い、cancel を charge 前に確認する。共有
  builtin handler（`builtin_core::dispatch`）が新規生成する String body（scalar /
  List / Dict key を含む）へ、tree（`builtin.rs`）と VM（`vm.rs`）の dispatch 呼び
  出し側から共通で配線する。`control_stop_to_error` を `budget` へ集約し両 engine の
  error 写像を一本化。legacy env `TSUMUGI_MAX_SINGLE_STRING_BYTES` /
  `TSUMUGI_MAX_STRING_ALLOCATIONS` / `TSUMUGI_MAX_STRING_BYTES` を追加。既定上限では
  観測挙動を変えない。この cumulative 会計は §5.3 のとおり解放しても減らさず、String body の
  live heap（`HeapBytes`）per-drop 追跡（PR-b、下記）とは独立した別会計である。string
  リテラル・`+` 連結・f-string 経路の課金（✅ 実装済み）: これらは tree/VM で dispatch を
  経由しないため、生成点で同じ `track_result`（cumulative `charge_string` ＋ live heap
  `new_str`）を通す。tree は `Evaluator::eval_expr` の `Expr::Str`・`BinOp`（`eval_binop`
  結果）・`Expr::FStr` で、VM は `OpCode::LoadConst`（String 定数のみ）・`OpCode::Add`
  （結果が String のとき）・`OpCode::FStrConcat` で課金する。既定上限では観測挙動を
  変えない。既知の tree/VM 差（第14節 Slice 6 で解消）: VM の f-string はリテラル部分を
  個別の String 定数として `LoadConst` するため、それぞれが 1 度課金される。tree は
  リテラル部分を `push_str` で結合してから最終 body だけを課金するため、リテラル部分を
  含む f-string の cumulative `StringAllocations`/`StringBytes` は VM の方が多い。VM が
  experimental の間の既知差として許容する。
- source / import accounting（✅ 実装済み）: per-item `SingleSourceBytes` と cumulative
  `SourceCount`/`SourceBytes`、`ImportCount`/`ImportBytes` を
  `BudgetLedger::charge_source` / `charge_import` で課金する。§5.3 のとおり root を
  `source_count` の 1 本目として数え、初めて解決した各 import module を `charge_source`
  （`source_bytes`）と `charge_import`（`import_bytes`）の両方へ課金する。§7.2 の固定
  優先順位（`SingleSourceBytes` < `SourceCount` < `SourceBytes`、`ImportCount` <
  `ImportBytes`）に従い、cancel を charge 前に確認する。課金は `Link` フェーズで最初の
  文を実行する前に行い、超過なら 1 文も実行しない。`ModuleLoader::link` が初めて解決した
  import の生 byte 長（`LoadedModule`）を返し、tree（`Evaluator::charge_link`）と
  VM（`Vm::charge_link`）が共通規則で課金する。root source byte 長は `CompiledScript`
  （tree）と CLI 入口（VM）から渡す。cache hit（解決済み module ID）は数えない。
  legacy env `TSUMUGI_MAX_SINGLE_SOURCE_BYTES` / `TSUMUGI_MAX_SOURCE_COUNT` /
  `TSUMUGI_MAX_SOURCE_BYTES` / `TSUMUGI_MAX_IMPORT_COUNT` / `TSUMUGI_MAX_IMPORT_BYTES`
  を追加。既定上限では観測挙動を変えない。
- I-O accounting（✅ 実装済み）: input / output / host call の count + bytes を
  reserve/commit/refund（§7）で課金する。`BudgetLedger::charge_input`（`InputCalls` +1・
  `InputBytes`）・`charge_output`（`OutputCalls` +1・`OutputBytes`）・`charge_host_call_request`
  （`HostCalls` +1・`HostRequestBytes`）・`charge_host_response_bytes`（`HostResponseBytes`）
  を追加した。§6.1 のとおり stdio（`print` / `input`）は host call でもあるため、input /
  output 固有 counter に加えて `HostCalls` / `HostRequestBytes` / `HostResponseBytes` /
  `HostCallBytes` へも co-charge する（input は request 0・response = payload、output は
  request = payload・response 0）。host call は request 側（count 含む）を境界へ入る前に、
  response 側を結果確定後に課金する 2 phase で、`HostCalls` は request 側で 1 回だけ数える。
  §7.2 の固定優先順位（`InputCalls` < `InputBytes` < `OutputCalls` < `OutputBytes` <
  `HostCalls` < `HostRequestBytes` < `HostResponseBytes` < `HostCallBytes`）に従い、cancel を
  charge 前に確認する。tree（`builtin.rs` の `print` / `input` と PureCore dispatch wrapper）と
  VM（`vm.rs` の `OpCode::Print`・`input` arm・`exec_builtin` dispatch wrapper）が同じ論理
  位置で共通に課金する。byte 数は host 境界で実際に受け渡す UTF-8 payload の長さ（`print` は
  join 後の出力・`input` は受け取った行・`write_file`/`append_file` は書き込み内容・
  `read_file`/`read_lines` は読み込み内容）で、Rust object の capacity や transport header は
  含めない（§6.1）。本 Slice の host call 対象は payload byte が明確な filesystem read/write
  builtin（`read_file`/`read_lines`/`write_file`/`append_file`、`builtin_core::is_host_call_builtin`）
  と stdio に限る。path 判定・env・clock 等その他の host 境界 builtin の課金は Phase 2 の
  capability / host function 配線で扱う（本 Slice では未対象）。エラー写像は
  `control_stop_to_error` に I-O resource の arm を追加し、`error.rs` の
  `input_calls_limit` / `input_bytes_limit` / `output_calls_limit` / `output_bytes_limit` /
  `host_calls_limit` / `host_request_bytes_limit` / `host_response_bytes_limit` /
  `host_call_bytes_limit`（`ErrorKind::IoLimit` = `io_limit`）へ tree/VM 共通で写す。legacy env
  `TSUMUGI_MAX_INPUT_CALLS` / `TSUMUGI_MAX_INPUT_BYTES` / `TSUMUGI_MAX_OUTPUT_CALLS` /
  `TSUMUGI_MAX_OUTPUT_BYTES` / `TSUMUGI_MAX_HOST_CALLS` / `TSUMUGI_MAX_HOST_REQUEST_BYTES` /
  `TSUMUGI_MAX_HOST_RESPONSE_BYTES` / `TSUMUGI_MAX_HOST_CALL_BYTES` を追加。既定上限では観測
  挙動を変えない。descriptor 上限・stream 途中停止・capability 拒否時の課金差
  （§6.1 後半・§6.2）は host function / capability 面が入る Phase 2/4 で扱う。
- heap accounting 基盤（✅ 実装済み）: `AllocationId(u64)` と per-execution
  `AllocationLedger` を `src/budget.rs` に導入した。§5.1 の論理サイズ表を `heap_size`
  純関数群として固定し（Value slot / String body / List・Dict body / tree・VM function
  instance / AST root・node / bytecode chunk / import record / continuation frame /
  handler / rollback journal entry）、`BudgetLedger::charge_heap` / `release_heap` が
  live heap を checked add / saturating sub で管理する。上限超過・overflow は §7.2 の
  固定優先順位（`HeapBytes` は Fuel の次）で `BudgetExceeded(HeapBytes)`、`AllocationId`
  の発行 overflow は 0 へ wrap せず `ControlStop::InternalFailure(AllocationIdExhausted)`
  にする。`usage()` が `live_heap_bytes` / `peak_heap_bytes` を反映する。legacy env
  `TSUMUGI_MAX_LIVE_HEAP_BYTES` を追加。§5.2 の context baseline 走査
  （`charge_context_baseline`、反復 worklist + `Rc` pointer visited set）は純関数として
  実装・単体テスト済みだが、collection の per-drop release 導入（下記）に伴い engine の
  `charge_link` からは呼ばない（tracked collection を二重計上するため）。非 collection の
  pre-existing context 状態（String / 関数 instance / cell）を baseline 課金する配線は、
  それらを per-drop 化する後続 PR で再導入する。
- collection（`List`/`Dict`）の per-drop release（✅ 実装済み、REV-015 案A PR-a）:
  `Value::List`/`Dict` の backing を `Rc<Tracked<T>>` にした。`Tracked<T>` は
  `AllocationId`・論理サイズ・heap 台帳への `Weak` を持ち、生成（`Value::new_list` /
  `new_dict`、`Tracked::new`）で §5.1 の body サイズを課金し、この backing を指す最後の
  `Rc` が drop した瞬間に `Drop` で release する（§5.2）。`Rc::clone`（共有）は無課金。
  COW mutation（index 代入・push/pop）は `detach`＋`retrack` で、単独所有なら in-place・
  共有なら新 `AllocationId` を発番して複製し、要素数変化ぶんの delta だけ課金/release する。
  compile 時の空リテラル定数は untracked（`Tracked::constant`）で、最初の mutation か
  dispatch 境界の `track_result` で tracked へ昇格する。builtin_core が返す collection は
  untracked で作られ、engine（tree=`builtin.rs`, VM=`vm.rs`）の dispatch 境界 `track_result`
  が tracked へ変換して課金する。tree/VM 両対応。未捕捉エラーの AUD-024 rollback では、
  巻き戻しで drop される tracked collection が `Drop` で自動 release される。既定上限では
  観測挙動を変えない。
  - 既知の tree/VM 差（第14節 Slice 6 で解消）: VM の `push`/`pop` は `CallBuiltin` +
    書き戻しで backing を full-clone してから `track_result` で丸ごと課金するため、tree の
    in-place delta 課金と異なり push のたびに peak_heap がスパイクする。VM の空リテラル
    `x=[]` は untracked 定数のまま変数へ渡り、最初の mutation まで body 24 byte を課金
    しない。いずれも既定上限では観測に影響しないが、configured 上限では tree と live/peak
    が食い違い得る。VM が experimental の間の既知差として許容する。
- 未実装（後続 PR）: AST / bytecode chunk / imported module record / rollback journal の
  per-drop 追跡（PR-d）。cell・tree/VM 関数 instance・String body は PR-b/PR-c で実装済み。
  - サブスライス分割（実装順）: collection per-drop（案A PR-a、✅ 済み）に続けて、
    依存の少ない順に切り出す。
    - **PR-b: String body の per-drop 追跡（✅ 実装済み）** — `Value::Str` の backing を
      `String` から `Rc<Tracked<String>>` へ変更し、§5.1 の String body（24 + byte 長）を
      生成時に live heap（`HeapBytes`）へ課金し、最後の参照 drop で release する。
      collection と同じ `Tracked<T>` + dispatch 境界 `track_result` パターンを横展開する。
      cumulative `StringAllocations`/`StringBytes`（§5.3、解放で減らさない累積値）は
      **据え置き**、live heap 課金を**追加**する。両会計は独立で、既定上限では観測挙動を
      変えない。`Value::Str` は他の per-drop 対象（cell / 関数 instance）と backing を
      共有しないため、単独で切り出せる。§5.3 の「substring が既存 body を共有する実装なら
      新規 allocation として数えない」共有最適化は本 PR の範囲外（`Rc<Tracked<String>>`
      は共有可能な backing だが、現行の連結・substring は依然 copy を作る）。
    - **PR-c: cell（`SharedValue`）＋ tree/VM 関数 instance の per-drop 追跡（✅ 実装済み）**
      — `SharedValue` の backing を `RefCell<Value>` から `Rc<Tracked<RefCell<Value>>>`
      へ変更し、cell 生成点（tree=`Env::set`、VM=`ensure_local_cell`）で §5.1 captured
      cell（32 byte）を live heap へ課金し、最後の参照 drop で release する。`Env` に heap
      台帳への `Weak` を持たせ `Env::set` を fallible 化した（呼び出し元へ `?` 伝播）。
      関数 instance は `Value::Fn` / `Value::VmFn` へ `header: Rc<Tracked<()>>` を持たせ、
      生成時（FnDef/Lambda 評価・`MakeClosure`）に §5.1 の header（tree: 64 + 16×captured
      / VM: 48 + 16×upvalue）を課金し drop で release する。captured / upvalue cell 実体は
      cell 側で別途課金するため header ぶんだけ計上し、二重計上しない。`Rc::clone`（共有・
      capture）は無課金。cell は関数 instance（captured / upvalue）の前提となるため一体で
      扱った。collection・String・cell・関数 header がすべて per-drop 追跡されたことで、
      同じ台帳を跨ぐ REPL/実行では pre-existing な live heap が自動的に持ち越され、Link
      境界での baseline 再走査は不要になった（`charge_link` は baseline 課金を行わない。
      `charge_context_baseline` は fresh ledger モデルの埋め込み API 用に温存）。既定上限
      では観測挙動を変えない。
    - **PR-d: AST / bytecode chunk / imported module record / rollback journal の per-drop
      追跡（✅ 実装済み）** — `Value` ツリーに現れず所有構造側が保持する heap object を、
      関数 header（PR-c）と同じ課金トークン（`HeapToken = Tracked<()>`、`Value::new_heap_token`
      で §5.1 論理サイズを課金し drop で release）で追跡する。§5.3「AST または bytecode」に
      従い、tree engine は AST を、VM は bytecode chunk を課金する（両者は同じ source の
      別 artifact で、engine ごとに一方だけを持つ）。
      - **imported module record**（両 engine）: `ModuleLoader` が canonical path をキーに
        record token（`imported_module_record` = 96 + module ID の UTF-8 byte 長）を保持し、
        `loaded` set と寿命を揃える。`charge_link` が Link フェーズで課金し、`forget`
        （未捕捉エラー rollback）または loader drop で release する。VM は loader を所有
        しないため、`Vm::charge_link` が token を返し、loader 所有者（CLI 入口）が登録する。
      - **AST**（tree のみ）: linked program（root + 全 Stmt / Expr node）の §5.1 論理サイズ
        （`AST program root` 64 + 各 node の `ast_node` = 64 + 所有 identifier / string literal
        の byte 長）を `Evaluator::charge_link` が Link フェーズで一括課金し、execution の
        寿命で token を保持する。REPL は入力ごとに前 AST token を release してから課金する。
      - **bytecode chunk**（VM のみ）: root chunk と全 prototype chunk（`MakeClosure` が
        `VmFn` へ clone 共有する `Rc<Chunk>`）を transitive に走査し、各 distinct chunk を
        1 回ずつ `bytecode_chunk`（64 + 16×opcode + 32×constant）で課金する（§5.2 の共有は
        1 回課金）。`Vm::run` は execution 寿命で保持し、`run_repl_chunk` は入力単位で
        入れ替えて release する。持続 closure が prototype chunk を retain する REPL ケースの
        parity 差は第14節 Slice 6（VM が experimental の間）で扱う。
      - **rollback journal entry**（両 engine、AUD-024）: undo journal（tree=`SubmissionJournal`、
        VM=`ReplStackCheckpoint`）が entry ごとに `rollback_journal_entry` の固定 overhead
        （48 byte）を課金するトークンを積み、submission の commit / rollback で journal ごと
        drop して release する（§10「work 量は journal entry 数で有限」）。保持する旧 value の
        到達 payload（List / Dict / String body）は、entry の握る `Rc` が対象 backing の
        `Tracked` を生かし続けるため既に live heap に計上されており、ここで再課金しない
        （§5.2 の共有は 1 回課金）。tree は journal 記録経路を fallible 化して超過を伝播し、
        VM は infallible な checkpoint 経路の超過を保留して次の per-instruction step 課金
        境界で surface する。既定上限では観測挙動を変えない。
  - `charge_context_baseline`（全 heap object を 1 回ずつ論理課金する純関数）は、collection・
    String・cell・関数 header・AST・chunk・import record・journal がすべて per-drop 追跡された
    ことで engine の `charge_link` からは呼ばない（tracked 分を二重計上するため）。execution
    ごとに台帳を作り直す埋め込み API（fresh ledger モデル、後続 Phase）用に温存し、単体
    テストで固定する。

### Slice 3: explicit continuation

- tree evaluatorの再帰的な実行状態を明示frame/cursorへ変換
- `ExecutionHandle`と全`ExecutionState`、`poll`、slice fuel、yieldを実装
- 同期`Engine::execute`はhandleをterminalまでpollする互換wrapperにする

Slice 3 は実行の「形」を変える最初の slice であり blast radius が大きいため、Slice 2 と
同様に依存の少ない順で 4 つの sub-PR へ分割する。各 PR は独立して緑（`cargo fmt --check`
/ `cargo clippy --all-targets -- -D warnings` / `cargo test`）にし、前 PR の受入基準を満たして
から次へ進む。VM の continuation / charge parity は第14節 Slice 6（VM が experimental の間）
で扱い、Slice 3 は tree evaluator を対象とする。cancel / pause / transaction（AUD-024 の
journal は既に一部実装済み）と admission / scheduler / host pending は Slice 4/5 の範囲で、
Slice 3 には含めない。

- **PR-a: 公開 state-machine surface + poll-to-terminal core（✅ 実装済み、最初の一手）** —
  §9 の公開型（`ExecutionHandle` / `ExecutionState`（`Created` / `Linked` / `Ready` /
  `Running` / `Yielded(YieldReason)` / `Paused(PausedState)` / `Terminal`）/ `PollSlice` /
  `PollResult` / `HandleError` / `ExecutionRequest`）と、拡張版 `ExecutionOutcome`（§9 の
  terminal payload）を `engine.rs` / `lib.rs` へ追加する。`Engine::create_execution`（Created
  から）/ `Engine::start`（Linked から）と `poll` を実装するが、**この PR の `poll` は初回に
  既存の再帰 `Evaluator::run` を一気に terminal まで回して `PollResult::Terminal` を返す**
  （continuation の書き換えはまだ行わない）。同期 `Engine::execute` は handle を terminal まで
  poll する互換 wrapper にする。§1.1 の統合規則（`ExecutionRequest.budget` は有限
  `BudgetConfig`、公開 `ExecutionHandle` は `!Send + !Sync`、state は §9 の enum だけ）を満たす。
  状態遷移（`Created→Linked→Ready→Running→Terminal`）・`HandleError::Terminal` /
  `InvalidState` の合法/不合法 matrix・terminal 後の再 poll 拒否をテストで固定する。
  allocation 形状を変えないため scaling / golden スイートに影響しない。後続 PR は、この
  安定した handle の裏側を差し替える形で進める。
  - 実装（`src/engine.rs` / `src/lib.rs`）: `ExecutionState`（`Created` / `Linked` /
    `Ready` / `Running` / `Yielded(YieldReason)` / `Paused(PausedState)` / `Terminal`）・
    `YieldReason`・`PauseReason`・`PausedState`・`ResumeState`・`PollSlice`・`PollResult`・
    `HandleError`・`ExecutionRequest`・`ExecutionHandle`・拡張 `ExecutionOutcome`
    （`Completed` / `RuntimeError { error }` / `LinkError { error }`）を追加。`Engine::create_execution`
    （Created から）/ `Engine::start`（Linked から）/ `ExecutionHandle::poll` を実装し、`poll` は
    `Created`/`Linked`/`Ready`/`Yielded` から `Running` を経て `run_phased`（Link/Run を区別する
    `Evaluator` の新 API）を terminal まで回し `PollResult::Terminal { outcome, usage }` を返す。
    Link 失敗を `LinkError`、実行中失敗（予算 limit 系を含む）を `RuntimeError`、正常完了を
    `Completed` へ写す。terminal 後の `poll`/`pause`/`resume` は `HandleError::Terminal`。
    `ExecutionHandle` は `PhantomData<*const ()>` で `!Send + !Sync`。`Engine::execute` /
    `execute_repl_submission` は handle を terminal まで poll する互換 wrapper で、戻り値契約
    （`Ok(Completed)` / `Err(TsumugiError)`）を維持する。最終 `BudgetUsage` は `PollResult::Terminal`
    の `usage` で観測し、outcome には二重に持たせない（§1.1）。
  - 本 PR で意図的に未実装（型を骨格に留めるか公開しない）: slice fuel での `Yielded`（PR-d）、
    `pause`/`resume` の実効化（Slice 4。terminal 以外では `InvalidState` を返す骨格）、`Exited`
    （REV-023）、`Denied`/`HostError`/`BudgetExceeded`/`DeadlineExceeded`/`Cancelled` などの構造化
    terminal（Slice 4/5・Phase 2。現状予算超過は `RuntimeError` 内）、scheduler / admission /
    backpressure（Slice 5）、`CapabilitySet` / `ExecutionId` / `LinkOptions`（Phase 2）。
    `ExecutionRequest` は budget/capability/cancellation を持たない最小骨格で、入口
    `ExecutionRequest::new()` を維持したまま後続で field を足す。
- **PR-b: statement / block / loop の明示 frame stack（✅ 実装済み）** —
  `exec_program` / `exec_block` / `exec_stmt` の Rust 再帰を、ヒープ上の frame stack + cursor
  と driver ループへ置き換える。`EvalResult`（`Return` / `Break` / `Continue`）で Rust
  スタックを巻き戻していた制御フローを、loop / block frame 上の保留 control-flow 状態へ移す
  （§9.3）。`eval_expr` は当面再帰のまま（式は浅く bounded）。§5.1 の `continuation_frame`
  （96 + local slot 32×count）heap 課金をここで配線する。
  - 実装（`src/eval.rs` / `src/env.rs` / `src/builtin.rs`）: `exec_stmt` の戻り値を
    `StmtStep`（`Val` / `Flow(FlowSignal)` / `Enter(Frame)`）化し、複合文（`if` / `while` /
    `for` / `try`）は子 `Frame` を組み立てて `Enter` を返す。`drive_body(root: &[Stmt])` が
    ヒープ上の `Vec<Frame>` + cursor を回す driver ループで、`exec_program` / 関数本体
    （`eval_call`）/ callback 本体（`builtin.rs`）/ `if`・`try`・ループ本体がすべてこれを通る。
    `Frame` は実行中の文列・cursor・`FrameKind`（`Block` / `Scoped` / `While` / `For` / `Try`）・
    `scope_base`（pop 時に `env.truncate_scopes` で巻き戻すスコープ深さ）・`heap_charge`
    （pop 時に release する continuation heap）を持つ。制御フローは Rust スタックの巻き戻し
    ではなく `unwind_flow` が frame stack を明示 unwind する（`Return` は関数活性境界まで、
    `Break` は最も近いループ controller frame ごと畳み、`Continue` は本体 Block frame だけ
    畳んで controller を残し次反復へ）。ループ本体は反復ごとに `push_scope` / continuation
    heap 課金 / `pop_frame` で release し、`for` は開始時に materialize した items と反復元
    collection を frame が保持して従来の live-heap 寿命を保つ。`while` / `for` の末尾
    `count_step`（fuel）は controller frame の `pending_step` で「本体 1 反復完了後・次反復
    判定前」に課金する（従来順序を保つ）。`try` 本体は `Try` frame で実行し、捕捉エラーは
    `handle_error` が内側 frame を畳んで try スコープを解放し、独立スコープの catch 本体を
    実行する `Scoped` frame へ差し替える（fuel/collection/heap を含む全 `Err` を捕捉する
    現行挙動を保つ）。continuation_frame heap は各 frame の push で課金し pop / unwind /
    catch 切替で release する。`env` に `scope_depth()` / `truncate_scopes(len)` を追加。
    関数呼び出し（`eval_call`）と式（`eval_expr`）は当面 Rust 再帰のまま（PR-c / 式は bounded）。
  - 観測挙動はほぼ不変で、次の 2 点だけ意図的に変える。(1) ループ外 break/continue の
    エラー行を offending 文の行へ揃える。従来の tree は関数呼び出し位置やトップレベル
    複合文の行を使い VM（compile 時に break/continue 文の行で検出）と食い違っていたが、
    frame stack 化に伴い `EvalResult::Break`/`Continue` に文の行を持たせ、両 engine を
    文の行へ収束させた（tree/VM parity の改善、roadmap の差異縮小方針に沿う）。既存の
    canonical error inventory はトップレベル bare break のみで両 engine 一致は保たれる。
    (2) `if` ブロックが continuation_frame（96 byte）を新たに課金するため、これに依存する
    heap rollback テストの上限を 7400→9600 へ調整した（release 性質は不変）。ループの反復
    進行中（2 回目以降の condition 再評価・末尾 count_step・反復スコープ束縛）に起きる
    エラーも、従来どおり囲む try/catch が捕捉する（advance 経路のエラーも `handle_error`
    を通す）。`tests/fixtures/try_catch` に 2 回目 condition 評価での除算エラーを catch する
    ケースを追加して両 engine で固定した。
- **PR-c: 関数呼び出し・try handler の明示 frame（✅ 実装済み）** —
  `eval_call`（`src/eval.rs`）と callback の `call_fn_value`（`src/builtin.rs`）を、Tsumugi の
  関数呼び出しがヒープ上の明示 call frame を積む形へ変換した。VM の `CallFrame` をミラーする
  `FrameKind::Call { saved_scopes }` を追加し、PR-b の driver ループを共通 `run_driver(frames)`
  へ抽出したうえで、関数本体を実行する `drive_call_body(def, saved_scopes)` を新設した。
  - 実装（`src/eval.rs` / `src/builtin.rs`）: `drive_call_body` は呼び出す関数の
    `Rc<FnDef>` をローカルに保持し、その `&def.body` を root frame（`FrameKind::Call`）の
    `stmts` として渡す。これにより関数本体とそこから派生する `if`/ループ/`try` の子 frame は、
    呼び出しの実行全体を通じて生きるこの本体 AST を借用でき、frame が自分の所有物を借用する
    自己参照を避ける（VM の `CallFrame` が `Rc<Chunk>` を保持し `ip` で index するのに対応する
    tree 側の等価な寿命管理）。呼び出し元（`eval_call` / `call_fn_value`）は従来どおり
    `env.push_call_frame()` でスコープを退避し、captured cell / self-binding / 引数を束縛し、
    call trace を積んでから、退避情報 `saved_scopes` を Call frame へ預けて `drive_call_body`
    を呼ぶ。Call frame の `pop_frame` が全終了経路（正常完了・`return` unwind・エラー unwind）で
    `env.pop_call_frame` と call trace の巻き戻しを一元的に行うため、呼び出し元側の終了経路ごとの
    手動 `pop_call_frame` を廃止した。`return` は `unwind_flow` が最も近い Call frame まで畳んで
    値を produce し、`break` / `continue` は Call 境界を越えられずそのまま surface して
    呼び出し側が「ループ外」エラーへ写す。エラー時の call trace は、frame を畳む前・発生時点の
    `call_stack` を snapshot する `attach_trace`（VM の `attach_trace` と同じ意味論。`with_trace`
    は既存トレースを上書きしないため最深トレースが保たれる）で付加する。
  - `try` handler は PR-b 時点で既に明示 frame stack 上の `FrameKind::Try`（driver の
    `handle_error` が最も近い Try frame へ unwind）として実装済みであり、これが VM の
    `TryHandler` stack をミラーする。PR-c の Call frame 化により、呼び出し境界を越えた
    try/catch の伝播は各呼び出しが独立した frame stack を持つ再入で自然に分離される
    （callee 内の未捕捉エラーは Call frame まで畳んで `Err` を返し、caller の driver が
    自分の Try frame で捕捉する）。
  - 観測挙動は不変（既存の全テストが緑、tree/VM の trace・catch・parity テストを含む）。
    再入モデルのため呼び出しごとに 1 段の Rust フレームは残り、`main.rs` の 8 MiB 実行
    スレッドと呼び出し深度上限 128 はそのまま維持する（式評価が Rust 再帰である以上、
    呼び出しごとの Rust フレーム除去には明示式評価スタックが必要で、それは範囲外。
    スレッド縮小は必須ではない）。
- **PR-d: slice fuel + yield（⬜ 未実装）** —
  `budget.remaining_fuel()` の上に slice-fuel accounting 層を足し、driver ループの charge 点で
  `PollSlice::max_fuel` を確認する。total 残量はあるが slice 残量を超える場合は charge せず
  `Yielded(SliceFuelExhausted)` を返し、`poll` が保存 frame 状態から再開できるようにする
  （§4.2）。`ExplicitYield` もここで扱う。`Engine::execute` は引き続き terminal まで poll する。

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
