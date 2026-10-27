# Tsumugi 実行予算・協調実行仕様

最終更新: 2026-10-27

設計ステータス: **到達目標の確定仕様（§1〜§13）＋ 実装の現在地（下記の逸脱一覧）**

本文書の §1〜§13 は **到達目標の確定仕様**であり、現在の実装状態ではない。現在の実装が目標仕様からどう
逸脱しているか・何が未実装かは、下の **逸脱一覧** と **未実装範囲** の 2 表が唯一の正本である（状態の正本は
この 2 表に置き、§14 のスライス小節は再掲しない）。実装スライスの進捗の正本は [`slices.toml`](slices.toml)。

2 表は現在の逸脱と未実装範囲だけを持つ。done にする実装 PR は、この 2 表に新しい逸脱・未実装範囲を追加し、
解消した行を削除する（どのスライスで解消したかはスライス小節と git 履歴に任せる。[ドキュメント運用ガイド](documentation-guide.md)
「逸脱一覧の持ち方」）。2 表は tree-expr-continuation-e1 の実装 PR で新設した。それ以前に完了したスライスの
逸脱・未実装範囲は、事実を確かめられたものから順に載せる。

## 逸脱一覧（目標仕様 §1〜§13 に対する現在の逸脱）

| 逸脱内容 | 対象の節(§) | 発生スライス |
|---|---|---|
| tree evaluator の slice fuel の使用量は `count_step`（文・反復・呼び出しの fuel）だけを数え、builtin が台帳へ直接課金する per-element fuel（`sort` の走査・比較課金など）を数えない。total fuel は正しいが、1 回の poll で実行する fuel が「slice + 区間内の bulk builtin 1 回分」を超えうる（bulk builtin を含む文が 1 slice に複数回あると、その回数ぶん超える） | §4.2 | Slice 3 PR-d |

## 未実装範囲（目標仕様 §1〜§13 のうち未着手・延期中）

| 未実装範囲 | 対象の節(§) | 備考 |
|---|---|---|
| tree evaluator の index・list / dict リテラル・f-string の段階評価と、`if` / `while` の条件・`for` の反復元・index 代入の index と値の式スロット。これらは式 frame で段階評価せず、呼び出しを含んでいても従来どおり `eval_expr` の Rust 再帰（`can_yield = false`）で 1 つの値として完了まで実行する（その内側では yield しない＝§4.2 の超過として扱う）。式 frame で段階評価するのは、文の値の 4 位置（式文・`let` / 再代入の右辺・`return` の値）の式に現れる call（callee が識別子以外のもの、または user binding を持つ識別子のもの）・二項演算・単項演算だけで、その call 引数・operand の境界と、呼んだ関数本体の文/反復境界で yield する | §4.2/§9.3 | tree-expr-continuation-e2b |
| builtin・登録 host function・cooperative host function の引数に現れる呼び出しの引数境界での slice yield（その呼び出しは Rust 再帰で完了まで実行する。callee が識別子でその時点で user binding を持たない call は、式 frame の中でも call 全体を従来の `eval_call` で評価する） | §4.2/§9.3 | tree-expr-continuation-e3 |
| `map` / `filter` / `each` の要素境界と callback 本体の文/反復境界での slice yield（callback 本体は `can_yield = false` で完了まで実行する） | §4.2/§9.3 | tree-expr-continuation-e4 |
| 関数本体・callback 本体の中での cooperative host-call の Pending yield と audit drain queue 上限の yield。どちらもトップレベル（driver 上の呼び出しの外）の文/反復境界だけで判定し、関数本体の中で Pending を返す adapter は `may_yield = false` の規約違反（catch 不能 `InternalControl`）になる | §4.2/§6.2 | §14「tree-expr-continuation 実装スライス」の逸脱（audit drain queue は [決定性・実行時監査仕様](determinism-and-audit.md) の逸脱一覧にも B-2 として記載） |
| callback なし bulk builtin（`sort` / `join` / 文字列繰り返し / 大 collection コピー等）の chunk 境界での slice yield。現状は bulk builtin 1 回分を §4.2 の超過として扱い、chunk 前の確認は deadline / cancel だけ | §4.1/§4.2 | fuel-bulk-chunk-yield |
| VM の式レベル continuation（VM の slice yield と continuation parity） | §9.3 | Slice 6（VM が experimental の間） |
| script / host の明示 yield（`YieldReason::ExplicitYield`）。型はあるが trigger を配線していない | §9 | — |
| §4.1 課金表の主要 charge point（statement / expression / operation 単位）への移行。現状の fuel は文・反復・呼び出しと一部 builtin の per-element だけ（`fuel_accounting_revision = 1`） | §4.1 | F-2 |

## 1. 位置づけ

本文書は、[Tsumugi Manifesto](manifesto.md)と[ロードマップ](roadmap.md)のうち、マニフェスト実現ロードマップ Phase 3「包括的な実行予算」とPhase 4「協調実行と負荷制御」の実装仕様を定める。既存のstep上限、collection上限、call・AST・import深度上限を土台として再利用する。第14節 Slice 1（budget型・legacy adapter・共有BudgetLedger）は `src/budget.rs` に実装済みで、Slice 2 のうち string accounting サブスライス（per-item `SingleStringBytes`・cumulative `StringAllocations`/`StringBytes` の課金を共有 builtin handler へ tree/VM 共通で配線し、さらに string リテラル・`+` 連結・f-string の生成点でも `track_result` で課金）と source / import accounting サブスライス（per-item `SingleSourceBytes`・cumulative `SourceCount`/`SourceBytes`・`ImportCount`/`ImportBytes` を Link フェーズで root と import へ tree/VM 共通で課金）、heap accounting 基盤サブスライス（`AllocationId`・`AllocationLedger`・§5.1 論理サイズ関数・`HeapBytes` reserve/超過写像）、collection（`List`/`Dict`）の per-drop release サブスライス（`Rc<Tracked<T>>` で生成時に課金し最後の参照 drop で release、COW は delta 課金、tree/VM 両対応）、および String body の per-drop release サブスライス（PR-b、`Value::Str` を `Rc<Tracked<String>>` 化し、builtin 結果を dispatch 境界の `track_result` で live heap 課金・最後の参照 drop で release）、cell と tree/VM 関数 instance の per-drop release サブスライス（PR-c、cell 生成点で captured cell を課金し `Value::Fn`/`VmFn` の header token で function instance header を課金、drop で release）、および AST / bytecode chunk / imported module record / rollback journal の per-drop 追跡サブスライス（PR-d、所有構造側が `HeapToken = Tracked<()>` トークンで §5.1 論理サイズを課金し drop で release。§5.3「AST または bytecode」に従い tree は AST・VM は bytecode chunk を課金、import record は両 engine、rollback journal entry は両 engine で entry の固定 overhead を課金）、および string リテラル/連結/f-string 経路の課金サブスライス（生成点で `track_result` を通し cumulative + live heap を課金、tree/VM 両対応）、および I-O accounting サブスライス（input / output / host call の count + bytes を reserve/commit/refund（§7）で課金し、stdio を host call として co-charge。tree/VM の `print` / `input` / filesystem dispatch 境界へ共通配線）も実装済みで、Slice 2 は完了した。Slice 3 は PR-a（公開 state-machine surface + poll-to-terminal core）と PR-b（tree evaluator の statement / block / loop を明示 frame stack + driver ループへ変換し continuation_frame heap を配線）、PR-c（関数呼び出し・try handler の明示 frame。`eval_call` / `call_fn_value` を `FrameKind::Call` 化し、共通 `run_driver` + `drive_call_body` で本体を駆動。VM の `CallFrame` / `TryHandler` をミラー）、PR-d（slice fuel + yield。PR-d-1 で continuation を Evaluator 所有の `'static` 永続 frame stack へ移し、PR-d-2 で slice-fuel accounting と文/反復境界での `Yielded(SliceFuelExhausted)`・`begin_execution` / `run_slice` resume を実装）まで実装済み。PR-e（式の途中での slice yield。§14「tree-expr-continuation 実装スライス」）の現在の実装範囲は冒頭の逸脱一覧を見ること。cancel/pause（Slice 4）と scheduler・host pending（Slice 5）は実装済みである（Slice 5 は `SchedulerShared`/`AdmissionController` の bounded admission・run-turn FIFO backpressure、nonblocking host-call ticket〔`HostCallTicket`/`ExecutionWaker`/`HostCallPoll`〕と wake・queue 待ち/pause/host 待ち中の deadline/cancel 伝播、独立上限の `AdapterExecutor` + `CooperativeAdapter`/`CapabilityCallContext.may_yield`。`AuditBackpressure`-yield（sink-Pending 由来）は Phase 6 B-1 で facade の監査付き poll driver に配線済み〔§10.2、[決定性・実行時監査仕様](determinism-and-audit.md) §14〕。audit drain queue byte 会計〔§10.2〕は B-2 で per-execution 上限を同 driver に配線済み、engine 全体の上限は E10 統合後）。VM charge parity（Slice 6）も heap・string・I-O の会計について完了済み（fuel の課金差は第14節「F-1 実装スライス」で扱う）で、最終形移行のうち `BudgetConfig` 必須所有（Slice 1: `ExecutionRequest::new(budget, clock)`・deadline を `BudgetConfig.deadline` へ統合・`EngineConfig.budget` collapse・CLI 既定の有限化＝option B）は実装済みである。最終形の `CancellationToken` 必須所有（Slice 2: `ExecutionRequest` が `cancellation: CancellationToken` を必須所有し `.cancellation(value)` builder で設定、`Engine::run` が実行直前に request-owned token を評価器へ install）も実装済みで、REV-015 最終形移行（budget + cancellation 必須所有）は完了した。残りは Phase 6（audit）である。

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
| `fuel_accounting_revision` | 1 |
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

`fuel_accounting_revision`は§4.1の課金表（charge pointと課金値）の版である。`heap_accounting_revision`が§5.1の論理サイズ表の版であるのと対称で、`BudgetConfig`が`u32`で保持し、`BudgetConfig::standard`は既定を現行版にする。課金表を変える変更（charge pointの追加・課金値の変更）はこの値を1つ上げ、同時に受理する版を新版だけへ更新する（`heap_accounting_revision`と同じく、受理しない版はexecution作成前に`ConfigError`で拒否する）。会計版の不正は、どのfield（heapかfuelか）が不正かを呼び出し側が区別できる形のconfig errorで返す（現行の単一variant `UnsupportedAccountingRevision(u32)`をheap/fuelで区別できる形へ改める。詳細は§14「F-2 実装スライス」の変更箇所）。旧版の課金を再現する互換経路は持たない（alphaにつき後方互換無保証、旧版指定は拒否されるのであって旧課金で動くのではない）。fuelの会計版をこの専用fieldに分離するのは、`language_revision`（§17.8の全順序化などscriptから観測できる言語挙動の版）と会計の版を混ぜないためである。したがってfuel課金の変更で`language_revision`はbumpしない。初期版は`1`（F-1までの課金。F-2が§4.1の表へ移行するとき`2`へ上げる）。

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

#### yield 可能点に挟まれた区間の slice 超過（§14 Slice 3 PR-e）

tree evaluator は yield 可能点でだけ slice 境界を確認する。関数呼び出しと反復は明示 frame 上で
段階実行し、その文/反復境界は必ず yield 可能点にする（§14 Slice 3 PR-e）。したがって yield 可能点に
挟まれて途切れずに実行される区間は、**呼び出しも反復も含まない区間**に限られる。この区間の中で
slice 残量を使い切った場合は、区間の途中では yield せず、**区間を抜けた最初の yield 可能点で
`Yielded(SliceFuelExhausted)` を返す**（それまでは slice を超過して実行してよい）。これは
「charge せずに yield する」原則の、呼び出しも反復も含まない有界区間に限った例外である。

- **超過量の上限**: この区間 1 つで超過しうる fuel は、次の 2 つの和で上限が決まる。
  1. **区間が含む AST node 数**（呼び出しも反復も含まない部分式の大きさ。`MAX_AST_DEPTH` と
     source/collection 上限で有界）。
  2. **区間内の callback を持たない bulk builtin 1 回分**（`sort` / `join` / 文字列の繰り返し /
     大きな collection のコピー等）の fuel。入力サイズが `max_collection_elements`（既定 1,000,000）・
     `max_single_string_bytes` 等の **collection / string 上限で有界**なので、1 回分の fuel も有限値に
     なる。ただし **fuel 量そのものは入力要素数 n を超えうる**（`sort` は要素数ぶんの走査課金 n に
     加えて比較ごとの per-element 課金で O(n log n)、`join` 等の表示系も per-node 課金）。既定 slice
     （例 10,000）の数十〜数百倍になりうるが、config の collection / string 上限が入力サイズを抑える
     ことで最悪の fuel 量が有限に決まる。
  呼び出し・反復はこの区間に入らないため、再帰やループで超過量が無制限に積み上がることはない。
- **bulk builtin の chunk 境界での yield は本スライスの範囲外**: §4.1 は bulk work を 256 element /
  16 KiB の chunk に分けると定めるが、**本スライス（Slice 3 PR-e）の時点では chunk 前で確認するのは
  deadline / cancel だけ**であり、chunk 境界で slice yield はしない。bulk builtin を chunk 単位で
  中断・再開する slice yield は後続スライス（`fuel-bulk-chunk-yield`、[`slices.toml`](slices.toml) に
  `todo` で登録）へ延期する。それまでは bulk builtin 1 回分を上記 2 の有界な超過として扱う。
- **根拠（manifesto 原則 1・4）**: slice yield の目的は「ホストへ制御が戻るまでの最悪時間を有界に
  保つ」ことである。呼び出し・反復を必ず yield 可能点にすれば、最悪時間は「1 区間の有界な超過
  （AST 分 + bulk builtin 1 回分）+ slice」で抑えられる。区間の内部まで細かく中断できることは要件
  ではないため、Rust 再帰を一部残すハイブリッド実装（§14 Slice 3 PR-e の 1b）を許す。

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

continuation frameの課金対象は、スコープを作るframe（96 + local slot 32 × count）だけである。Call frame・ループ制御frame・式frameは、同期経路のRust再帰が持つ状態をheapへ移したものなので課金しない（0）。

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

#### closure が retain する code の課金（A-1、REV-015 Slice 6）

`charge_link` が課金する artifact token（AST / bytecode chunk）は **REPL 入力（artifact）の
寿命**に結びつく。しかし closure を定義した入力が終わっても、その closure が生き残る限り、
closure が握る code（tree=`FnDef.body` の `Rc<[Stmt]>`、VM=`VmFn.chunk` の prototype
`Rc<Chunk>`）はメモリ上に残る。従来はこの retained code が次入力の artifact release で
課金から外れていた（両 engine 対称の under-charge）。これを **A-1 モデル**で閉じる。両 engine
を「reachable な code は課金し続ける」挙動へ揃え、retained closure の code を reachable な限り
両 engine とも課金し続ける（従来の release-時に落ちる under-charge を解消する）。**課金の有無と
release 寿命は両 engine で一致する**が、課金額は engine ごとの artifact 単位（tree=`ast_node`、
VM=`bytecode_chunk`）のままで、§5.3 の「AST または bytecode」どおり数値は engine 固有であって
よい（A-2 のような engine 共通の単一基準へ寄せる＝ byte 単位で `live_heap` / `peak_heap` を
一致させることは**しない**）。これにより resource 消費の backend 差（＝「片方だけ落ちる」
非対称）を解消し、[決定性・監査仕様](determinism-and-audit.md) §3 の決定性要件（terminal 時
`BudgetUsage` の backend 一致＝課金の有無・寿命が揃うこと）および manifesto 原則 5 を満たす。

A-1 の設計原則は 1 つに尽きる: **retained-code token は closure instance ではなく、課金対象の
distinct code object（`Rc` の backing 同一性）に紐づける**。PR-c の header token は function
instance 固有（instance ごとに別物で、複数 instance 間で共有されない）だったが、retained code
は複数の closure instance が同一 `Rc` を共有しうる（下記の生成経路を参照）ため、header と同じ
instance 単位では N 個の instance が同一 code を N 回課金してしまう。したがって retained-code
token は **code object 単位で 1 つ**とし、instance はそれを `Rc::clone`（無課金）で共有する。
この一点で、以下に挙げる instance 間共有・ネスト closure・dead branch のいずれの穴も塞がる。

- **課金単位は distinct code object（§5.2「共有は 1 回課金」の token 層での具体化）**: closure が
  retain する code の §5.1 論理サイズは、その code object（tree=`FnDef.body` の `Rc<[Stmt]>`、
  VM=prototype `Rc<Chunk>`）に対して **Link/adopt 時（tree=`charge_link` の Link、
  VM=`charge_chunk_tree` の adopt＝`ExecutionPhase::Compile`）に 1 本の token で 1 回だけ**
  課金する（課金の timing と token 保持者の移譲は下記「ownership transfer」で定義）。
  同じ `Rc` を共有する
  2 個目以降の closure instance は、既存トークンへの `Rc::clone` だけを持ち、追加課金しない。
  - **実現機構（設計として確定・実装者が選ぶ余地を残さない）**: retained-code token を
    **code object（distinct `Rc<[Stmt]>` / `Rc<Chunk>`）ごとに 1 本の `Rc<HeapToken>`
    （`HeapToken = Tracked<()>`）として立て、closure instance はそれを `Rc::clone` で共有する**
    方式を採る。この `Rc<HeapToken>` は Link/adopt 時に課金して立て（下記「ownership transfer」）、
    生成前は artifact 側が code object との対応で保持し、生成時に closure instance
    （tree=`Value::Fn`、VM=`Value::VmFn`）へ `Rc::clone` 共有する。同じ code object を共有する
    2 個目以降の instance も同じ `Rc<HeapToken>` を `Rc::clone` するだけで追加課金しない。
    - **token と code object の対応付け（正準方式・tree / VM 対称の side-table）**:
      生成時に「いま instantiate しようとしている body / prototype」から「その事前課金済み
      token」を引くために、**両 engine とも `Rc::as_ptr` を key にした side-table で引く**。
      当初案（VM だけ `FunctionPrototype` に `retain_token` slot を足す）は却下する。理由は
      次のとおり現行コードでは実装不能だからである: `FunctionPrototype` は `Rc<Chunk>` の
      中に共有保持され（`src/chunk.rs:53`、`CallFrame.chunk: Rc<Chunk>` `src/vm.rs:28`）、
      `charge_chunk_tree_into` は `&Chunk` 共有参照で走査する（`src/vm.rs:299-317`）。
      interior mutability なしに prototype 内の slot へは書き込めず、REPL 経路では adopt 前に
      既に `Rc` の strong ref が複数本あり `Rc::get_mut` も不可（`src/vm.rs:363-388`）。
      したがって slot 書き込み方式は成立しない。両 engine を次の side-table 方式に統一する。
      - **VM**: `Vm` が `HashMap<*const Chunk, Weak<HeapToken>>` を 1 本持つ。key は
        `Rc::as_ptr(&prototype.chunk)`（`FunctionPrototype.chunk` は `Rc<Chunk>`、
        `src/chunk.rs:22`）。adopt 時の `charge_chunk_tree_into` 走査で各 distinct prototype
        chunk を課金し、立てた strong `Rc<HeapToken>` は `chunk_tokens` へ（artifact 保持）、
        同じ token の `Weak` を side-table へ登録する。`MakeClosure`（`src/vm.rs:1273` 近傍）は
        手元の `prototype.chunk`（および下記の transitive な子孫 prototype chunk）を
        `Rc::as_ptr` で side-table から引き、`Weak::upgrade` して得た strong を `Rc::clone` で
        `VmFn` の retain-token 集合へ載せる。`FunctionPrototype` / `Chunk` へのフィールド追加は
        不要（`Chunk` を可変にせず、`PartialEq`/`Clone` 派生〔`src/chunk.rs:11,41`〕も変えない）。
      - **tree**: `Evaluator` が `HashMap<*const [Stmt], Weak<HeapToken>>` を 1 本持つ。key は
        `Rc::as_ptr(&body)`（`FnDef.body` / `Lambda.body` は `Block = Rc<[Stmt]>`、
        `src/ast.rs:14`）。Link 時の per-body 課金で立てた strong `Rc<HeapToken>` は
        `ast_token` 相当の artifact 保持側へ、`Weak` を side-table へ登録する。closure 生成点
        （`src/eval.rs` の `Stmt::FnDef` / `Expr::Lambda` 評価）は、生成する closure の body
        （および下記 transitive な内側 body）を `Rc::as_ptr` で引き、`Weak::upgrade` して得た
        strong を `Rc::clone` で `Value::Fn` の retain-token 集合へ載せる。
    - **side-table は `Weak` の index であり token 寿命を一切縛らない（跨入力の鍵）**:
      side-table が持つのは `Weak<HeapToken>` だけなので、side-table 自身は token を生かさない。
      token の寿命は常に strong 参照（artifact 保持側の `chunk_tokens`/`ast_token` 相当、または
      生成済み closure instance が握る `Rc::clone`）の参照カウントだけが決める。これにより
      side-table は **`Vm` / `Evaluator` の寿命を通じて持ち続けてよい**（入力ごとに破棄しない）。
      REPL 入力をまたいで生成される closure（下記不変量）でも `Weak::upgrade` が成立する限り
      token を引けるためである。upgrade が失敗する `Weak`（token が既に release 済み）は
      dead entry であり、次回同 key 挿入時に上書きするか、入力入れ替え時に掃除してよい
      （掃除は性能最適化であって budget 不変量には影響しない）。
    - **跨入力生成の不変量（finding 2・両 engine 共通）**: closure 生成（tree=`FnDef`/`Lambda`
      評価、VM=`MakeClosure`）は、その code object を adopt/Link した入力と**必ずしも同一
      入力ではない**。top-level closure は定義入力内で生成されるが、ネストした内側 closure は
      外側 closure が後続入力で call されて初めて生成されうる（例: 入力 N で `fn outer() { fn
      inner()... }` を定義し、入力 N+1 で `outer()` を呼ぶと `inner` の生成は入力 N+1）。
      この跨ぎを安全にするため、本設計は **(1) side-table を `Weak` の長命 index にする**（直前
      項）ことと、**(2) closure 生成時にその closure が reachable に握る code object subtree
      全体（自分の code object + transitive な子孫 code object）の token を一括で `Rc::clone`
      して instance へ移譲する**こと（下記「生成時はトークンを移譲する」で定義）で担保する。
      (2) により、外側 closure が生き残れば内側 prototype/body の token も外側 instance の
      strong 参照で生き続け、後続入力の内側生成時に `Weak::upgrade` が成立する。raw-pointer
      key の跨入力 ABA（drop 済みアドレスの再利用）は、upgrade 可否が token の生死で判定される
      ため実害を生まない（dead な `Weak` は upgrade 失敗で弾かれ、生きている token だけが引ける）。
    - **token 保持構造自体は無課金**: 両 engine の side-table（`HashMap<_, Weak<HeapToken>>`）・
      artifact 保持側の token 集合・closure instance 側の retain-token 集合は、いずれも
      **budget の内部簿記であってそれ自体は課金しない**（現行の
      `chunk_tokens: Vec<Rc<HeapToken>>` / `ast_token` を課金対象にしていないのと同じ扱い）。
      これらは課金対象の code object 本体ではなく token への参照を保持するだけの bookkeeping で
      あるため、`live_heap` の「課金の有無と寿命が backend 間で一致」不変量には影響しない。
  - **tree**: `FnDef.body`（`ast_heap_size` と同じ `ast_node` 単位＝64 + 所有 identifier /
    string literal の byte 長）を本体 AST の論理サイズとして、その body `Rc<[Stmt]>` に紐づく
    token で 1 回だけ課金する。
  - **VM**: closure が握る prototype chunk を `charge_chunk_tree` と同じ `bytecode_chunk`
    （64 + 16×opcode + 32×constant）で課金する。**その prototype が transitive に抱える
    子・孫 prototype も含めて**、各 distinct chunk に紐づく token で 1 回ずつ課金する
    （§5.2 の共有は 1 回課金）。
- **ネスト closure の transitive 規則（tree / VM 対称）**: `FnDef.body` / `Lambda.body` には
  さらにネストした `FnDef` / `Lambda` を書け、`ast_heap_size` は body へ再帰展開する。したがって
  tree 側も VM の prototype 走査と対称に、**各 distinct body AST slice はちょうど 1 つの token が
  課金する**ものとして扱う。
  - **tree**: 外側 closure の token が `FnDef.body` を課金する際、body 内にネストした
    `FnDef` / `Lambda` の body（＝内側の distinct `Rc<[Stmt]>`）は、**その内側 closure の token
    が課金する責務**とし、外側 token の走査対象から除外する。どの body がどの token に属するかは
    code object（`Rc<[Stmt]>` の backing 同一性）で判定する。これにより外側と内側が同じ内側
    body を二重課金しない。tree の各 `FnDef` / `Lambda` ノードの body は原則 distinct な `Rc`
    だが、仮にパーサ/linker が同一 `Rc<[Stmt]>` を複数ノードへ共有させても、backing 同一性
    （`Rc::as_ptr`）で 1 token に畳まれるため §5.2「共有は 1 回課金」と整合する。
  - **VM**: 同様に、prototype が抱える子孫 prototype は、それ自体が別の `MakeClosure` の対象なら
    その内側 closure の token が課金する責務とし、外側の走査対象から除外する。
- **二重課金回避境界（最重要）と ownership transfer の発火条件**: artifact token（入力単位）と
  retained-code token（code object 寿命）が **同じ code object を同時に課金してはならない**。
  ただし責務移譲の発火点は「linked program の走査（closure 生成**前**）」ではなく
  「**closure が実際に生成された時点**」である。この時間差を取り違えると、評価されない
  `FnDef` / `Lambda`（dead branch の定義、束縛も返却もされず捨てられる lambda）で body が
  どちらの token にも載らない uncharged 状態になり、§5.2「reachable な code は課金」と矛盾する。
  現行の token は 1 個の `Tracked<()>` が生成時 `bytes` を課金し `Drop` でちょうどその `bytes`
  を release する（partial release / 途中での再配賦 API は持たない。tree の `ast_token` は
  `ast_heap_size(linked_program)` を 1 本の lump token で課金している）。この性質を前提に、
  lump token からの差し引きではなく **Link 時の per-code-object 分割 + 生成時のトークン移譲**で
  境界を実現する。
  - **Link/adopt 時に artifact 課金を「非 closure 部」と「code object ごと」に分割して積む**:
    Link/adopt（tree=`charge_link` の Link、VM=`charge_chunk_tree` の adopt）で artifact を課金
    する際、(a) closure body / prototype に該当しない top-level code は従来どおり artifact token
    で課金し、(b) 各 closure body / prototype（distinct `Rc<[Stmt]>` / `Rc<Chunk>`）は **それぞれ
    独立した `Rc<HeapToken>` を 1 本ずつ立てて課金する**。この per-code-object token の strong
    参照は生成前は **artifact 保持側（tree=`ast_token` 相当、VM=`chunk_tokens`）が保持し**、
    その `Weak` を side-table（上記「token と code object の対応付け」）へ index として登録して、
    closure が生成されるまでは artifact の寿命で生かす。
    - **全ネスト深度を eager に列挙すること（tree / VM 対称・最重要）**: この分割走査は
      **linked program 全体をネスト深度に関わらず走り、どんな深さにある distinct body / prototype
      に対しても 1 本ずつ token を立てなければならない**。top-level の `FnDef` / `Lambda` だけを
      列挙して body 内にネストした closure を取りこぼすと、その内側 body が uncharged になり本
      設計が塞ごうとしている穴が再発する。
      - **VM**: 列挙は既存挙動そのものを使える。`charge_chunk_tree_into`（`src/vm.rs:299-317`）が
        `for prototype in &chunk.prototypes` を **transitive に再帰**し、到達されない深いネスト
        prototype も含め全 distinct chunk を adopt 時に 1 回ずつ課金する。各 prototype を課金した
        strong token を `chunk_tokens` へ積み、その `Weak` を side-table
        （key=`Rc::as_ptr(&prototype.chunk)`）へ登録するだけで、列挙漏れは起きない。prototype は
        compile 時に distinct に作られるため backing 同一性での重複判定が素直に成立する。
      - **tree**: tree は VM と違い **既存の `ast_heap_size`（`src/ast.rs:489`）の走査を
        そのまま再利用できない**。現行 `ast_heap_size` は 1 本の flat worklist で、`FnDef` /
        `Lambda` に当たると body を個々の `Stmt` へ平坦展開して push し
        （`src/ast.rs:548-549`, `:585-586` 近傍）、どの `Stmt` がどの body slice に属するか・
        その body の論理サイズ小計を保持しない。したがって per-code-object 課金では、
        **body（`Rc<[Stmt]>`）を単位に小計を区切る走査へ再構成する**: `FnDef` / `Lambda` に
        当たったら body を worklist へ平坦展開せず、その body を 1 つの課金単位として小計を
        計算し、body 内にさらにネストした `FnDef` / `Lambda` の body は再帰的に別単位へ切り出す。
        外側 token が内側 body を二重課金しないよう、外側小計からは内側 body slice ぶんを除外
        する（上記「ネスト closure の transitive 規則」の tree 項と同じ境界）。任意のネスト深度の
        distinct body を列挙し、各 slice に 1 本ずつ token を立てて side-table
        （key=`Rc::as_ptr(&body)`）へ `Weak` を登録する。top-level だけを列挙する実装は禁止。
        「`ast_heap_size` をそのまま再利用」は per-body 小計を保持しない以上成立しないため、
        この走査構造の改造は本設計の必須作業である（「再利用」という語で過小評価しない）。
    これにより dead branch の body やネストした未到達 lambda も Link/adopt 時点で必ずどこかの
    token に載り、uncharged 状態を作らない（§5.2「reachable な code は課金」を満たす）。
  - **生成時はトークンを「移譲」する（再課金も差し引きもしない・subtree 一括）**: closure が
    実際に生成された瞬間（tree=`FnDef` / `Lambda` 評価、VM=`MakeClosure`）に、その closure が
    reachable に握る code object subtree 全体 — **自分の code object（tree=`FnDef.body`、
    VM=`VmFn.chunk`）に加えて、そこから transitive に辿れる子孫 code object（内側 body /
    子孫 prototype chunk）すべて** — の per-code-object token を、side-table から `Rc::as_ptr`
    で引き `Weak::upgrade` して得た strong を `Rc::clone` し、closure instance（tree=`Value::Fn`、
    VM=`Value::VmFn`）の retain-token 集合へ載せる。追加課金は発生しない（既に Link/adopt 時に
    1 回課金済み）。同じ code object を共有する 2 個目以降の instance も同じ `Rc<HeapToken>` を
    `Rc::clone` するだけ。subtree 一括で移譲するのは、内側 closure が後続入力で初めて生成される
    場合でも（上記「跨入力生成の不変量」）、外側 instance が内側 token の strong を握り続けて
    side-table の `Weak::upgrade` を成立させ続けるためである。こうして「生成時に責務が
    artifact 保持から closure instance 保持へ移る」。lump token からの byte 差し引きは行わない
    ため、`Tracked` の partial release 不在と矛盾しない。
  - **移譲は `Rc::clone`（共有）であり token の move-out ではない（課金 phase と生成 phase の
    時間差を跨ぐ鍵）**: per-code-object token は Link/adopt 時（VM は `ExecutionPhase::Compile`、
    `src/vm.rs:310` 近傍）に立って artifact 保持側（VM=`chunk_tokens`、tree=`ast_token` 相当）へ
    strong が入り、side-table には `Weak` だけが入る。生成（VM=`MakeClosure`、tree=`FnDef` /
    `Lambda` 評価）はずっと後に、場合によっては**複数入力をまたいで**走る。この時間差を安全に
    跨ぐため、移譲は **保持側から token を取り出して move するのではなく、side-table 経由で
    `Weak::upgrade` した strong を `Rc::clone` して共有を増やす**操作と定義する。生成時点で
    closure instance が独立した強参照（`Rc` clone）を握る。
  - **artifact release との接続（両 engine とも自分の clone を無条件 release、生存は instance の
    独立 `Rc` が担保）**: REPL が次入力で artifact を入れ替えるとき、VM は
    **`chunk_tokens.clear()` で自分が持つ strong を無条件に（移譲済みの prototype 分も含めて）
    drop する**（`src/vm.rs:383` 近傍）。tree も対称に、`ast_token = None` と artifact 保持側の
    per-body token 集合を無条件に drop する。「移譲されたものだけ残す」フィルタリングは両 engine
    とも不要かつ行わない。移譲済み code object の課金が生き残るのは、生きている closure instance
    が握る**独立した `Rc` clone** が last-ref ではないため `Tracked::Drop`
    （`src/value.rs:180-186`、last-ref でのみ release）が発火しないから、という一点のみで担保
    される。side-table はこの release に関与しない（`Weak` のみ保持し token 寿命を縛らない。
    上記「side-table は `Weak` の index」）。生成済み closure 側に `Rc::clone` の強参照が残る
    ため、その token は last-ref になるまで release されず、closure が生きている限り課金が残る。
    これが A-1 の核心である。
    - **帰結（取り違え防止）**: 移譲を「保持側コレクションからの move-out」とモデル化しては
      ならない。move-out にすると、入力 N で生成した closure がまだ生きているのに入力 N+1 の
      無条件 clear が「残った未移譲分だけ」を落とそうとして実装が複雑化し、かつ clone 共有の
      意味論から外れる。正準モデルは「side-table 経由の `Weak::upgrade` → `Rc::clone` 共有 +
      保持側の無条件 clear + last-ref release」であり、未移譲 only のフィルタは不要。dead branch
      等「一度も移譲されなかった」token も、この無条件 clear で artifact 保持側の strong が落ち、
      他に強参照がないので last-ref として正しく release される（side-table の `Weak` は
      dangling になるだけで token を生かさない）。
  - 要約すると、各 distinct code object は常に **ちょうど 1 つの `Rc<HeapToken>`** に課金され、
    その token の保持者が「artifact 側（生成前・未生成）」から「closure instance 側（生成後）」へ
    移るだけで、課金額は Link 時の 1 回から変化しない。§5.2 の `AllocationId` visited-set と同じ
    「同じ object は 1 回だけ課金」の不変量を token 層でも守る。
- **release 寿命（retained-code token を retain する最後の `Rc` が drop された時点で落とす）**:
  retained-code token は、その code object を retain する **最後の `Rc<HeapToken>` が drop された
  時点**で `Tracked` の `Drop`（`src/value.rs:180-186`、last strong ref の drop 時に発火）が
  live heap を release する。生成後は token の保持者が closure instance（および
  同じ code object を共有する他 instance）だけになるため、これは「その code object を retain
  する最後の closure instance が drop した瞬間」と一致する。closure instance が複数共有されて
  いても 1 個の drop では release せず、最後の 1 個まで retained code を live heap に残す
  （§5.2「到達可能性が残る object は release しない」）。生成されずに artifact 側へ残った token は
  artifact release 時（REPL 入れ替え）に drop されて release される。これは PR-c の関数
  header per-drop 追跡（`Value::Fn` / `Value::VmFn` の `header: Rc<Tracked<()>>` を生成時課金・
  drop で release）と**同じ per-drop パターンの横展開**だが、header が instance 固有トークン
  なのに対し retained-code は **code object 単位の共有トークン**である点が異なる。header token
  （instance の固定 overhead）と retained-code token（本体 AST / prototype chunk の論理サイズ）
  は別トークンとして並置する。
- **§5.3「AST または bytecode」原則との整合**: tree は AST を、VM は bytecode chunk を課金する
  という engine ごとの別 artifact の建付けは A-1 でも維持する（A-2 のように engine 共通の
  単一基準へ寄せない）。したがって retained closure の課金額も engine ごとに（tree=`ast_node` /
  VM=`bytecode_chunk`）異なる数値になってよく、A-1 が backend 間で揃えるのは **課金の有無と
  release 寿命**であって `live_heap` / `peak_heap` の byte 単位の数値一致ではない。
  [決定性・監査仕様](determinism-and-audit.md) §15.5 differential は retained closure を含めて
  charge trace を突合するが、その突合対象は
  「reachable な限り課金され続けること・release 寿命が一致すること」および code artifact に
  既に認められている engine 固有サイズ差であり、code artifact の byte 単位数値一致ではない。

> **設計レビュー対応メモ（REV-015 Slice 6 PR-6b, 2 巡目）**: 本 A-1 設計はレビュー指摘を次の
> とおり反映した。
> - *[HIGH] VM の `FunctionPrototype.retain_token` slot 方式が実装不能*: `Chunk` は `Rc<Chunk>`
>   共有・`charge_chunk_tree_into` は `&Chunk` 走査で interior mutability なしに slot へ書けない
>   （`src/vm.rs:299-317`, `:363-388`, `src/chunk.rs:53`）。→ **slot 方式を却下し、VM も tree と
>   対称な `Rc::as_ptr` key の side-table 方式へ統一**した（上記「token と code object の対応付け」）。
> - *[MEDIUM] side-table 寿命と跨入力 ABA*: closure 生成は adopt/Link 入力と別入力になりうる
>   （VM の `MakeClosure` は複数 `run_repl_chunk` を跨ぐ、tree の内側 closure も外側 call 入力で
>   初生成）。→ **side-table を `Weak` の長命 index にし、生成時に subtree 全体の token を一括
>   `Rc::clone` 移譲**する不変量を明記（「side-table は `Weak` の index」「跨入力生成の不変量」
>   「生成時はトークンを移譲する（subtree 一括）」）。ABA は upgrade 可否が token の生死で判定
>   されるため実害なしと固定。
> - *[MEDIUM] tree の per-body 分割が `ast_heap_size` の flat worklist では成立しない*: 現行走査は
>   body を個々の `Stmt` へ平坦展開し per-body 小計を保持しない（`src/ast.rs:548-549`, `:585-586`）。
>   → **「走査を body 単位の小計へ再構成する必須作業」である旨を明記**し「再利用」表現を撤回
>   （「全ネスト深度を eager に列挙」tree 項）。
> - *[NIT] body の `Rc` 共有有無*: → tree の body は原則 distinct `Rc`、共有されても backing
>   同一性で 1 token に畳まれ §5.2 と整合、の一文を追加（「ネスト closure の transitive 規則」tree 項）。

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

Slice 2で追加した上限のlegacy key（`TSUMUGI_MAX_SINGLE_STRING_BYTES` / `TSUMUGI_MAX_STRING_ALLOCATIONS` / `TSUMUGI_MAX_STRING_BYTES` / `TSUMUGI_MAX_SINGLE_SOURCE_BYTES` / `TSUMUGI_MAX_SOURCE_COUNT` / `TSUMUGI_MAX_SOURCE_BYTES` / `TSUMUGI_MAX_IMPORT_COUNT` / `TSUMUGI_MAX_IMPORT_BYTES` / `TSUMUGI_MAX_LIVE_HEAP_BYTES` / `TSUMUGI_MAX_INPUT_CALLS` / `TSUMUGI_MAX_INPUT_BYTES` / `TSUMUGI_MAX_OUTPUT_CALLS` / `TSUMUGI_MAX_OUTPUT_BYTES` / `TSUMUGI_MAX_HOST_CALLS` / `TSUMUGI_MAX_HOST_REQUEST_BYTES` / `TSUMUGI_MAX_HOST_RESPONSE_BYTES` / `TSUMUGI_MAX_HOST_CALL_BYTES`）も、上表と同じ移行規則で扱う（2026-10-20決定）。各keyは同名の`BudgetConfig`上限fieldへ写し、数値として解釈できない値は無視して既定を残す。

新fuelはstatement・expression等も課金するため、同じ数値でも旧step limitより早く到達し得る。互換mappingは安全側であり、旧実行量の完全再現を保証しない。

> **§13 実装注記（2026-10-20）**
>
> legacy envの入口はCLI adapter（`src/main.rs`の`LegacyBudgetEnv`）だけに置いた。起動時に1回だけ
> process envを読み、tree/VMのfile・stdin・REPLの全経路で同じ上書きを適用する（tree/VMは
> `BudgetConfig::standard`、clockを持たないVM実行は`for_legacy(1_000_000, 1_000_000)`を土台にする）。
> 上書きが1つでも効くと、stderrへ1回
> 「警告: 環境変数 KEY[, KEY...] による実行予算の上書きは非推奨であり、将来のバージョンで削除されます」を出す。
> libraryは env を読まない: `BudgetConfig::from_legacy_env`を削除し、env非依存の
> `BudgetConfig::apply_legacy_overrides(lookup)`と`LEGACY_OVERRIDE_KEYS`へ置き換えた。
> `Evaluator::new` / `Vm::new` / `Vm::new_repl`（alpha facadeの`ExecutionContext::new`を含む）は
> §3.1既定の上限を使う。VMへ明示configを渡す`Vm::with_budget` / `Vm::new_repl_with_budget`を追加した。
> これにより、option B（最終形移行Slice 1）以降に残っていた「tree file/stdinだけがenvを無視する」
> tree/VM差を解消した。削除予定versionは、最初のreleaseを切る時点で決める（未決）。`ExecutionContext::reset_step_budget()`は削除し、executionごとに新しい`BudgetUsage`を作る。REPLの各入力は1 executionとし、contextだけを引き継ぐ。

`MAX_AST_DEPTH`、`MAX_IMPORT_DEPTH`、`MAX_USER_CALL_DEPTH`は環境変数化せず、language revisionに紐づく構造上限として維持する。将来configurableにする場合もbudgetと同じ不変configへ置き、paused中変更は許さない。

## 14. 実装slice

各スライス小節はそのスライスを実装した時点の記録であり、書き換えない。現在の逸脱・未実装範囲は冒頭の逸脱一覧を見ること。

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
> `TSUMUGI_MAX_COLLECTION_SIZE` を fuel / collection 上限へ写す legacy 入口を提供する
> （2026-10-20: `from_legacy_env` は削除し、env の読み込みは CLI adapter へ移した。§13 実装注記）。
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
  変えない。tree/VM の f-string リテラル部分の課金差は第14節 Slice 6 で解消済み: VM は
  f-string のリテラル部分を課金しない `LoadConstRaw`（定数表からの非課金 load）で積み、
  tree と同じく結合後の最終 body だけを `FStrConcat` が課金する。これにより tree は
  リテラルを `push_str` で結合し個別課金しない会計と一致し、リテラル部分を含む f-string の
  cumulative `StringAllocations`/`StringBytes` が両 engine で等しくなる。
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
  - push/pop の full-clone 差（✅ Slice 6 で解消済み）: 以前は VM の `push`/`pop` が
    `CallBuiltin` + 書き戻しで backing を full-clone してから `track_result` で丸ごと課金
    したため、tree の in-place delta 課金と異なり操作のたびに新旧 backing が一瞬同時に
    live になり peak_heap がスパイクしていた。`pop` は `OpCode::PopUpdate`（→
    `list_pop_tracked`）、`push` は `OpCode::ListPushBinding`（→ `list_push_tracked`）で、
    どちらも対象 binding storage を直接書き換える in-place 経路へ切り替え、tree engine と
    同じ primitive を通すようにして解消した。`ListPushBinding` は `SetIndex` と同じ binding
    storage 経路で、backing の `Rc` を operand stack へ複製しないため、一意所有では COW
    複製なしの delta（+32 byte）だけを課金する（共有 backing は tree と同じく `detach_list`
    が COW 複製する）。`builtin_push` / `builtin_pop` は builtin registry 契約・エラー
    メッセージ parity のため残置する。paired test は `tests/scaling.rs` の
    `push_peak_heap_has_no_full_clone_spike_across_engines`（push 後に
    `peak_heap == live_heap` となり full-clone スパイクが消えたことを両 engine で固定）・
    `push_to_shared_backing_has_no_spike_across_engines`（共有 backing への push も
    スパイクしないことを固定）・`push_live_heap_is_linear_not_quadratic_across_engines`
    （live heap が O(N) に収まる回帰ゲート）。
  - 空リテラル差（✅ Slice 6 で解消済み）: 以前は VM の空リテラル `x=[]` / `x={}` が
    untracked 定数のまま変数へ渡り、最初の mutation まで body 24 byte を課金しなかった。
    `OpCode::LoadConst` が String だけでなく List / Dict も `track_result` に通すように
    して、空 collection も materialize 時に body を課金し、tree engine（`eval.rs` の
    `Expr::List`/`Expr::Dict` が `new_list`/`new_dict` で生成時課金）と揃えた。非空リテラル
    は `track_result` が空 body を 1 度課金したあと `ListPush`/`DictInsert` が tracked
    backing へ delta 課金するため合計は tree と同じで二重課金しない。paired test は
    `tests/scaling.rs` の `empty_collection_literal_live_heap_matches_across_engines`
    （スカラー変数の増分を基準に、空 List/Dict の増分がちょうど body ぶん多いことを両
    engine で固定）。
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
        parity 差は第14節 Slice 6 で closure-retain 課金（下記「closure が retain する code の
        課金（A-1）」）として扱う（VM が experimental の間）。
      - **rollback journal entry**（両 engine、AUD-024）: undo journal（tree=`SubmissionJournal`、
        VM=`ReplStackCheckpoint`）が entry ごとに `rollback_journal_entry` の固定 overhead
        （48 byte）を課金するトークンを積み、submission の commit / rollback で journal ごと
        drop して release する（§10「work 量は journal entry 数で有限」）。保持する旧 value の
        到達 payload（List / Dict / String body）は、entry の握る `Rc` が対象 backing の
        `Tracked` を生かし続けるため既に live heap に計上されており、ここで再課金しない
        （§5.2 の共有は 1 回課金）。tree は journal 記録経路を fallible 化して超過を伝播し、
        VM は infallible な checkpoint 経路の超過を保留して次の命令の dispatch 前に
        surface する（F-1 以降は fuel 課金と切り離した全命令の `surface_pending_heap_stop`）。既定上限では観測挙動を変えない。
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
- **PR-d: slice fuel + yield（✅ 実装済み）** —
  slice-fuel accounting 層を足し、driver ループの文/反復境界で `PollSlice::max_fuel` を確認する。
  total 残量はあるが slice 残量を超える場合は、その境界で `Yielded(SliceFuelExhausted)` を返し、
  `poll` が評価器の永続 frame stack（PR-d-1）から再開できるようにする（§4.2）。PR-d-1 で
  continuation を Evaluator 所有の `'static` frame stack へ移したことが前提になる。
  - 実装（`src/eval.rs`）: `Evaluator` に `slice_fuel_used` / `slice_fuel_limit` と実行
    セッション `session: Option<RunSession>` を追加。`count_step` は total fuel を課金しつつ
    `slice_fuel_used` を加算する（accounting のみ。yield 判定はしない）。`run_driver` は
    `(stop_depth, can_yield)` を取り、ループ先頭（= 文/反復ディスパッチ前の安全な境界）で
    `can_yield && slice_exhausted()` なら `DriveOutcome::Yielded` を返す。`slice fuel` は total を
    補充せず、実際に commit した fuel だけが total usage へ乗る（§4.2）。
  - **yield は文/反復境界だけ**（式評価は当面 Rust 再帰のままで、途中では resume 不能）。
    そのため関数呼び出し（`drive_call_body`）と callback、および同期実行
    （`run` / `run_phased` / `run_repl_submission` → `exec_program`）は `can_yield = false` で
    活性を完了まで回す。yield できるのは poll 経路のトップレベル driver だけ。呼び出し中に
    消費した slice fuel は、呼び出しから戻った後トップレベル境界で yield 判定される。
  - resume 契約: `Engine::create_execution` / `start` の初回 `poll` が `Evaluator::begin_execution`
    （link + Link フェーズ課金 + 深度検証 + REPL transaction 開始 + root frame push。実行しない）
    を呼び、以降の `poll` が `Evaluator::run_slice(max_fuel)` で永続 frame stack を 1 slice ぶん
    進める。`run_slice` は `None`（yield）/ `Some(Ok/Err)`（terminal）を返し、terminal で
    transaction を commit/rollback し未完了 module の解決マーカーを巻き戻す（AUD-024 / AUD-030）。
    `ExecutionHandle::poll` は `None` を `PollResult::Yielded { SliceFuelExhausted }` ＋
    `ExecutionState::Yielded(..)` へ、`Some` を `PollResult::Terminal` へ写す。`Engine::execute`
    は引き続き handle を terminal まで poll する互換 wrapper（`drive_to_outcome` が `Yielded` を
    ループで回して terminal まで駆動）。
  - `ExplicitYield`（`YieldReason`）は enum として定義済みだが、script / host の明示 yield surface が
    まだ無いため trigger は未配線（host 要求 pause と同じく Slice 4 で実効化する）。
  - 受入: `tests/engine_api.rs` に、小さな `max_fuel` で複数 slice に分かれて yield しても、大きな
    slice 1 回と同じ terminal かつ同じ total committed fuel へ到達すること、yield 後の resume と
    terminal 後 poll 拒否、slice 分割を跨いだ runtime error terminal を固定した。観測挙動は
    既定 slice（同期経路）では不変（597 tests green）。

#### tree-expr-continuation 実装スライス（Slice 3 PR-e：式の途中での slice yield）

> 状態: **未実装**（設計のみ）。本小節は Slice 3 PR-a〜d が文・反復・関数呼び出し・try を明示
> frame 化し slice fuel/yield を入れた後に残る最後のピース——**式評価（`eval_expr`）を Rust 再帰から
> 明示 continuation へ移し、式の途中でも slice yield/resume できるようにする**——の slice 定義で
> ある。§9.3「continuation へ保存するもの」と §4.2 の slice yield が正本であり、以下は本スライスで
> 実装する範囲・観測挙動・意図的に延期する逸脱の記録である。進捗の唯一の正本は
> [`slices.toml`](slices.toml) と [ロードマップ](roadmap.md)。
>
> **sub-slice への分割（issue #248）**: 本スライスは 1 晩・1 PR に収まらないため、下記
> 「tree-expr-continuation の sub-slice 分割」で **e1〜e4 の 4 つの実装スライス**
> （`tree-expr-continuation-e1` 〜 `-e4`）へ割った。本小節の本文（yield 要件・境界規則・保存する状態・
> heap 課金・観測挙動・逸脱・受入基準 TEC-AT-01〜07）は 4 つに共通の設計であり、各 sub-slice の範囲・
> 担当する受入基準の部分・完了条件はそれぞれの小節に書く。他の小節・スライスが
> 「`tree-expr-continuation` が done」と書くときは **e1〜e4 がすべて done** を意味する。
>
> **後続スライスとの関係**: 本スライスは F-2（§4.1 課金表の主要 charge point への移行）の前段依存
> である。F-2 で charge point が statement / expression / operation 単位になると式評価中に slice
> fuel を使い切りうるが、現状 tree は文/反復境界でしか yield できない（PR-d「yield は文/反復境界
> だけ（式評価は当面 Rust 再帰のままで、途中では resume 不能）」）。本スライスはこの制約を外す。
> **本スライスは課金値を変えない**（課金点・課金値の移行は F-2）。現状の fuel 課金のまま「式の途中で
> yield できる」ようにするだけである。

PR-c の実装注記が明言するとおり、現状は「式評価が Rust 再帰である以上、呼び出しごとの Rust フレーム
除去には明示式評価スタックが必要で、それは範囲外」として式を Rust 再帰（`eval_expr`）のまま残した。
また PR-d では関数本体（`drive_call_body`）と callback（`call_fn_value`）を `can_yield = false` で
完了まで回しており、**yield できるのは poll 経路のトップレベル driver だけ**である。本スライスは
(1) 式評価に明示式評価スタックを導入して式の途中でも yield でき、(2) 関数本体・callback 本体の
文/反復境界も yield できるようにする。

- **yield 要件（manifesto 原則 1・4、§4.2「yield 可能点に挟まれた区間の slice 超過」）**: slice yield の
  目的は「ホストへ制御が戻るまでの最悪時間を有界に保つ」ことである。これを満たすため、**呼び出しと
  反復は必ず yield 可能点にする**。具体的には次を満たす。
  - **関数本体・callback 本体の文/反復境界を必ず yield 可能にする**。現状 `drive_call_body` /
    `call_fn_value` は `can_yield = false` なので、本スライスで yield 可能化する（下記「can_yield の
    伝播」）。再帰やループが何段あっても、各段の文/反復境界で yield できる。
  - **callback を呼ぶ builtin（`map` / `filter` / `each` の 3 つ）も、callback 本体の途中で
    中断・再開できる**ようにする（これらは要素ごとにユーザー関数を呼ぶ。`sort` は callback を取らない
    bulk work なので対象外＝下記「逸脱」と §4.2）。
  - **yield できなくてよいのは「呼び出しも反復も含まない区間」だけ**。その区間は §4.2 の例外（slice を
    超過して実行し、区間を抜けた最初の yield 可能点で yield。超過量は区間の AST 分 + callback なし
    bulk builtin 1 回分で有界）に従う。この区間に限り Rust 再帰を残すハイブリッド（下記 1b）を許す。

- **方針＝ハイブリッド（1b）＋明示側に置く境界規則**: `eval_expr` の全式種を一括で明示スタック化する
  （Rust 再帰を式から完全排除する）案（1a）は blast radius が過大なため採らない。代わりに **yield 要件を
  満たすのに必要な部分を明示側に置き、残りは Rust 再帰を許す**。境界規則は上の yield 要件から次のように
  固定する（実装者が線引きを独自判断しない）。
  - **呼び出し（call 式）と反復を含む部分式は、必ず明示 frame 側に置く**（operand stack 上で段階評価し、
    各境界で yield 可能）。これにより「呼び出し・反復を必ず yield 可能点にする」要件を式レベルでも守る。
  - **呼び出しも反復も含まない部分式（純粋な算術・比較・index・リテラル等）は Rust 再帰を残してよい**。
    その区間は §4.2 の超過例外で有界。
  - **維持する防御**: 深度上限 `MAX_USER_CALL_DEPTH = 128`（`src/limits.rs`）と `main.rs` の 8 MiB
    実行スレッドは**変えない**。呼び出しごとの Rust フレームは残り、停止性・stack overflow 防御として
    引き続き機能する。
  - **過大なら分割してよい**: Slice 3 が PR-a〜d に割れたのと同様、sub-PR へ割ってよい。特に
    **`map` / `filter` / `each` の callback 段階実行化は sub-PR に分けてよいが、本スライスの完了条件に
    含める**（yield 要件の一部であり、別スライスへ外さない）。分割する場合は `slices.toml` を更新し
    本小節を割る。→ 4 つの sub-slice へ分割した（下記「tree-expr-continuation の sub-slice 分割」）。

- **continuation へ保存する式の評価状態（§9.3 の具体化）**: §9.3 は「AST cursor・value stack・
  保留中 operand」を heap 上の明示状態として保存することを要求する。本スライスが式について保存する
  のは次である（PR-b/c が導入した `Frame` / `FrameKind` と同じ continuation heap に載せる）。
  - **operand stack**: 評価済みだがまだ親式に消費されていない中間値（例: 二項演算 `a + b` で `a` を
    評価し終え `b` を評価する間の `a` の値）。
  - **評価中の式 cursor**: 親式のどの operand まで評価したか（二項/単項の左右、index の `obj[idx]` の
    どちら、call の callee と各引数のどこまで）。
  - **callee 引数の部分評価結果**: call 式 `f(e0, e1, …)` で、評価済みの引数値と次に評価する引数の
    index。関数へ入る前（引数評価の途中）で yield できるようにする。
  - **callback builtin の反復状態**: `map` / `filter` / `each`（callback を取る 3 builtin）で、
    処理済み要素・次の要素 index・部分結果。段階実行化（下記）でこれを frame に載せる。
  - **保留中の二項/単項/index 演算**: operand を揃えた後に適用する演算の種別（左右の値が揃うまで
    frame に保持し、揃ったら適用）。

- **式 frame の heap 課金（確定。e2a 判断 Q1 = A で改訂）**: 式評価用の frame は**課金しない**
  （`heap_charge` は 0。Call frame・`while` / `for` の制御 frame と同じ）。式 frame は同期経路の Rust 再帰（未課金）が
  持つ評価状態を heap へ移しただけで、新しい値は作らない。operand stack に積む中間値は **既に別所で課金済みの
  `Value`（§5.1 の Value slot ほか）を参照保持するだけ**とし、operand を二重課金しない（§5.2「共有は 1 回課金」に従う）。
  当初は式 frame を 96 byte 固定で課金すると確定していたが、e1 判断 Q1 = A（terminal まで poll する入口は観測値を
  変えない）と両立しないため改めた（理由は e2a 小節の判断の記録）。
  - **同期経路でも式 frame を積むか**: **積まない**。同期経路（`can_yield = false`）は従来どおり
    `eval_expr` の Rust 再帰で評価し、式 frame を作らない。式 frame を作る（明示段階評価に入る）のは
    **poll 経路（`can_yield = true`）で、かつ呼び出し・反復を含む部分式**に限る。これにより同期経路の
    `live_heap` / `peak_heap` は現状と 1 byte も変わらず、TEC-AT-03（観測不変）と衝突しない。VM との
    heap 差も広げない。
  - **poll 経路の heap 観測**: 式 frame は課金しないので、poll 経路で式 frame を積んでも `live_heap` /
    `peak_heap` は同期経路と一致する。

- **yield 可能点（どの境界で slice 残量を確認するか）**: §4.2「次の論理 charge が slice 残量を
  超える場合は charge せず `Yielded(SliceFuelExhausted)` を返す」を次の境界に適用する。
  - **関数本体・callback 本体の文/反復境界**（新規。PR-d のトップレベル driver と同じ判定を、
    `can_yield = true` を伝えた `drive_call_body` / callback 本体でも行う）。
  - call 式の各引数評価の境界（引数 i を評価後・引数 i+1 を評価前、全引数評価後・本体へ入る前）。
  - 呼び出し・反復を含む部分式の operand 境界（二項/単項/index で、その operand が呼び出し・反復を
    含む場合）。
  - callback builtin の各要素処理の境界（`map`/`filter`/`each` の 1 要素ごと。`sort` は callback を
    取らないため対象外＝下記「`sort` は対象外」）。
  - **呼び出しも反復も含まない区間は yield 可能点を置かない**（§4.2 の超過例外で有界）。
  - **現状課金では一部境界が 0 fuel になりうる**（課金値の付与は F-2）。本スライスは「境界で slice
    残量を確認し超過なら yield する機構」を置き、F-2 がそこへ課金値を載せる。課金 0 の間でも機構が
    resume 可能であることは、下記の強制 yield フックを使った受入基準で固定する。

- **can_yield の伝播（関数本体・callback を yield 可能にする）**: 現状 `drive_call_body` /
  `call_fn_value` は `run_driver(base, false)` で回す。本スライスはトップレベル driver から再入した
  活性にも `can_yield = true` を伝える。Call frame（`FrameKind::Call`）に「この活性が yield 可能か」を
  持たせ、resume 時にトップレベル driver が永続 frame stack を根から回し直す形にする（Rust 再帰で
  再入した活性の途中状態を Call frame 経由で復元する）。
  - **新しい can_yield が効くのは slice fuel の yield（と下記の強制 yield フック）だけ**とする
    （issue #248 の分割設計で確定。設計 PR で kamonabe の判断を仰いだ点）。次の 2 つは本スライスでは
    従来どおり **トップレベル driver の文/反復境界だけ**で判定し、関数本体・callback 本体の中では
    判定しない。
    - cooperative host-call の `may_yield`（`can_yield_ctx`）。関数本体内で Pending を返す adapter は
      従来どおり `may_yield = false` の規約違反（catch 不能 `InternalControl`）になる
      （`tests/scheduler_api.rs` の `may_yield_false_requires_ready_else_internal_terminal` を無改変で
      緑に保つ。関数本体内の host-call Pending yield は resume 時の「同じ文の再評価」規約〔§4.7〕を
      関数本体の文へ広げる別設計が要るため、本スライスの範囲外＝下記「逸脱」）。
    - audit drain queue 上限の yield（[決定性・実行時監査仕様](determinism-and-audit.md) §10.2・B-2 の
      「トップレベル境界で判定する soft limit」。B2-AT-09 を無改変で緑に保つ）。
    - 実装上は「slice fuel を確認してよいか」と「トップレベル（関数本体・callback 本体の外）か」を
      別の可否として持つ。前者は本スライスで関数本体・callback 本体へ伝え、後者は伝えない。

- **callback builtin の段階実行化（`map` / `filter` / `each`）**: callback を取る 3 builtin は
  要素ループを明示 frame の反復に載せ、1 要素ごと（callback 呼び出しの前後）に yield 可能点を置く。
  callback 本体は呼び出しなので、本体の文/反復境界でも yield できる（上記 can_yield 伝播）。
  per-element 課金・エラー伝播・結果の順序は現状維持。この段階実行化は sub-PR に割ってよいが本スライスの
  完了条件に含む（上記）。
  - **`sort` は対象外**: `sort` は callback（ユーザー関数）を取らない bulk work で、`sort_by` の
    comparator は値比較と per-element 課金（`compare_budgeted`）だけを行う。これを中断可能ソートへ
    書き換えると比較回数・比較順・同期経路の fuel（比較ごとの per-element 課金）が変わり TEC-AT-03
    （fuel 値不変）を破るため、**書き換えない**。`sort` を含む callback なし bulk builtin は §4.2 の
    超過例外（1 回分を collection / string 上限で有界な超過として扱う）に従い、chunk 境界 yield は
    後続スライス `fuel-bulk-chunk-yield` へ延期する。

- **変更箇所**（Slice 3 PR-b/c と同じ粒度で列挙）:
  - `src/eval.rs`: `eval_expr` のうち呼び出し・反復を含む部分を、yield 可能点を持つ段階評価
    （operand stack と式 cursor を `Frame` に載せる）へ分解する。`FrameKind` に式評価用 variant
    （仮称 `Expr { … }`：評価中の式・operand stack・cursor・保留演算）を追加。`drive_call_body` /
    callback 本体を `can_yield = true` で回せるようにし（Call frame に yield 可否を持たせ、resume を
    トップレベル driver から駆動）、`run_driver` のループで新境界の slice 確認を行う。呼び出しも反復も
    含まない部分式は Rust 再帰を残す。
  - `src/builtin.rs` / `src/builtin_core.rs`: `map` / `filter` / `each` の callback 呼び出しを
    段階実行へ書き換える（要素ループを明示反復に載せ要素境界で yield 可能に）。per-element 課金・
    エラー伝播・結果順序は現状維持。`sort` は書き換えない（上記）。
  - `src/env.rs`: 式 frame の一時値スコープ管理が要る場合は PR-b の `scope_depth()` /
    `truncate_scopes(len)` 規律に合わせる（新 API は増やさない想定）。
  - continuation heap 課金: 式 frame は課金しない（`heap_charge` 0、上記）。poll 経路でだけ積む。
  - docs: 本小節、§4.2（超過例外・追記済み）、§14 冒頭・第1節の「Slice 3 完了」記述（PR-e 追加に更新
    済み）、roadmap。VM は対象外（下記逸脱）。

- **観測挙動（原則不変・やむを得ない差は明記）**: 既定 slice（同期経路）では観測挙動を不変に保つ
  （fuel 値・成否・エラー行・trace・stdout・exit code・heap が現状と一致。同期経路は式 frame を積まない）。
  原則として明示化は evaluation 順・課金順・エラー発生点を変えない。ただし Slice 3 PR-b が「ループ外
  break/continue のエラー行」を tree/VM parity のため意図的に変えた前例があるとおり、避けられない差が
  出る場合は F-1/PR-b と同じく**意図的な差として本小節と実装 PR 本文に明記し、tree/VM parity を壊さ
  ない方向でのみ許容する**。既存テスト群（597 tests green を含む）は無改変で緑を回帰条件とする。
  期待値が説明できない形で変わる場合は実装を止めて報告する。

- **逸脱（本スライス時点で残るもの、後続スライスへ延期）**:
  - **式の charge point と課金値**（statement/expression/operation への移行）は **F-2**。本スライスは
    yield 機構だけを置き、課金は現状のまま。
  - **VM の式レベル continuation**は Slice 6（VM が experimental の間）。本スライスは tree evaluator
    だけを対象とする。VM の continuation/charge parity は Slice 6 で扱う。
  - **Rust 再帰の完全排除・深度上限や 8 MiB スレッドの見直し**（1a）は本スライスの範囲外。呼び出しも
    反復も含まない部分式は Rust 再帰を残し、既存の防御（128 / 8 MiB）を維持する。
  - **callback なし bulk builtin（`sort` / `join` / 文字列繰り返し / 大 collection コピー等）の
    chunk 境界 slice yield** は本スライスの範囲外。§4.2 の超過例外で 1 回分を有界な超過として扱い、
    chunk（256 element / 16 KiB）単位での中断・再開は後続スライス `fuel-bulk-chunk-yield`
    （[`slices.toml`](slices.toml) に `todo` 登録）へ延期する。本スライス時点では bulk の chunk 前
    確認は deadline / cancel のみ（slice yield はしない）。
  - **cancel / deadline / pause の新境界 checkpoint**: Slice 4 の cancel/deadline checkpoint は現状
    文/反復境界にある。本スライスで増えた callback 本体の文/反復境界には同じ checkpoint が自然に乗る
    （同じ driver ループ境界のため）。式の operand 境界にまで cancel/deadline checkpoint を広げるかは
    本スライスでは行わず、必要になれば Slice 4 側の課題として扱う（yield 要件は呼び出し・反復境界で
    満たされるため、最悪時間の有界性には影響しない）。
  - **関数本体・callback 本体の中での cooperative host-call Pending yield と audit drain queue 上限
    yield** は本スライスの範囲外（上記「can_yield の伝播」）。どちらも従来どおりトップレベル境界だけ。

- **受入基準（tree-expr-continuation）**:
  - **TEC-AT-01（yield 可能点を跨ぐ実行が同期経路 `run` と一致）**: 下記の強制 yield フックを有効に
    して、全 yield 可能点（関数本体・callback 本体の文/反復境界・call 引数境界・呼び出し/反復を含む
    operand 境界）を跨ぐ script を実行したとき、複数 slice に分かれて yield/resume した結果が、
    **同期経路（`can_yield = false` の `Evaluator::run` 系。式 frame を積まない Rust 再帰経路）の結果と一致する**。一致を検査するのは
    **stdout・`committed.fuel`・terminal 種別、および非 `Yielded`/`Resumed` の audit event 列**
    （`Yielded`/`Resumed` は同期経路には現れないため列全体の一致は求めない。対・順序は TEC-AT-06 で
    別途検査する）。`map` / `filter` / `each` の段階実行版についても、同期経路 `run` と同じ結果・同じ
    副作用順序・同じ `committed.fuel` になることを含める（manifesto 原則 5：2 経路が同じ結果を出す）。
  - **TEC-AT-02（resume の忠実性）**: operand stack・式 cursor・callee 引数の部分評価結果・callback
    反復状態が yield を跨いで保存され、resume 後に続きから評価される。深い式・入れ子 call・引数途中・
    callback 途中での yield を強制 yield フックで起こし、resume 後の最終値と副作用列が **同期経路
    （`Evaluator::run` 系）と一致**することを固定する。
  - **TEC-AT-03（観測不変・既定 slice）**: 既定 slice（同期経路と、terminal まで poll する入口）で、既存の全テスト（`tests/`・
    unit test、PR-d の 597 tests green を含む）が無改変で緑。fuel 値・エラー行・trace・stdout・
    exit code・`live_heap`/`peak_heap` が現状と一致する（式 frame は課金しないため heap 不変）。
    terminal まで poll する入口（`Engine::run`〔CLI を含む〕・alpha `execute` / `execute_repl_submission`・
    `run_for_repl`）は driver 経路を通ってよいが、観測値は現状と一致させる（e1 判断 Q1 = A）。
    意図的に変えた差があれば本基準の例外として明記し、tree/VM parity を壊さないことを別途固定する。
  - **TEC-AT-04（深度・停止性の維持）**: `MAX_USER_CALL_DEPTH = 128` と stack overflow 防御（8 MiB
    スレッド）が従来どおり機能する。深い再帰・深い式が従来と同じ `call_depth_limit` / 有限停止で止まる。
  - **TEC-AT-05（transaction / 境界の不変）**: yield は transaction 境界ではない（§10 規則7）。
    関数本体・callback 途中・式途中の yield を跨いでも submission journal が保持され、terminal でだけ
    commit/rollback される（PR-d の transaction 不変条件を新境界 yield でも維持）。
  - **TEC-AT-06（超過量の上限）**: 関数本体のループや callback 本体のループを持つ script を最小 slice
    （`PollSlice::max_fuel = 16`）で実行したとき、1 回の poll で実際に実行される fuel が
    **slice + 規定の超過上限以内**に収まる。超過上限はテストで計算できる形で次と定める（§4.2）。
    - **現行課金（`fuel_accounting_revision = 1`、F-1）下では式に fuel がかからない**ため、呼び出しも
      反復も含まない区間の AST 分の超過は **0**。したがって本スライス時点のテストは実質「超過 ≒
      bulk builtin 1 回分」を見る（式起因の超過が乗るのは F-2 で式を課金してから＝そのとき上限に
      「1 区間の node 数」が加わる）。
    - **callback なし bulk builtin 1 回分**の超過は、その builtin を**同じ入力で単独実行したときの
      fuel**（テストで事前に測る実測値）を上限とする。`sort` は要素数ぶんの走査課金（n）に加えて
      比較ごとの `compare_budgeted` per-element 課金（比較回数 O(n log n)）が乗るため、1 回分の fuel は
      n を超えうる（「collection 上限以内」ではない）。`join` など表示系も per-node 課金で同様。入力
      サイズ自体は `max_collection_elements`・`max_single_string_bytes` 等の config 上限で有界なので、
      1 回分の fuel はそれらから決まる有限値になる。テストは「bulk builtin を単独実行した fuel」を測り、
      その bulk を含む区間を最小 slice で回したときの 1 poll 実行量が「slice + 測った単独 fuel + 区間の
      AST 分」以内であることを exact に確認する。
    - 再帰・ループが何段あっても 1 poll の実行量がこの上限を超えない（呼び出し・反復が必ず yield
      可能点だから）。audit の `Yielded`/`Resumed` が対になり順序が単調であることも併せて検査する。
  - **TEC-AT-07（callback 中断の resume 一致）**: `map` / `filter` / `each`（callback を取る 3
    builtin）の callback の途中で yield/resume しても、同期経路 `run` と結果・副作用順序・
    `committed.fuel` が一致する。要素途中・callback 本体途中の両方で強制 yield を起こして固定する。
    （`sort` は callback を取らないため本基準の対象外。§4.2 の超過例外で扱う。）

- **テスト専用の強制 yield フック**: 現状課金では新 yield 可能点の多くが 0 fuel になり、最小 slice
  fuel でも yield しない。そこで **`unstable-bytecode`（または `cfg(test)`）限定で「全 yield 可能点で
  必ず yield する」モード**を追加し、TEC-AT-01/02/06/07 はこのフックで全境界を通す。production 既定
  経路には影響しない（PR-d の `EngineBuilder::audit_slice_fuel` と同じ、テスト専用の内部 surface）。

- **テスト配置**: yield 可能点を跨ぐ一致・resume 忠実性・超過上限・callback 中断（TEC-AT-01/02/06/07）
  と transaction 不変（TEC-AT-05）は `tests/engine_api.rs`（PR-d の slice 分割テストと同じ場所。
  強制 yield フック使用）、観測不変（TEC-AT-03）は既存 `tests/` の無改変緑、深度・停止性（TEC-AT-04）は
  `tests/defensive*.rs`、式 frame の push/pop・operand stack・callback 反復状態の不変条件は
  `src/eval.rs` / `src/builtin*.rs` の unit test。

#### tree-expr-continuation の sub-slice 分割

> 状態: **未実装**（設計のみ。issue #248 で分割を決定）。各 sub-slice の進捗は [`slices.toml`](slices.toml)
> の `tree-expr-continuation-e1` 〜 `-e4` を正とする。共通の設計（yield 要件・境界規則・保存する状態・
> heap 課金・観測挙動・逸脱・受入基準 TEC-AT-01〜07 の本文）は上の「tree-expr-continuation 実装スライス」
> 小節にあり、ここでは割り方と各 sub-slice の範囲・完了条件だけを書く。

**割り方の考え方**: yield を Rust 再帰の内側から外へ出すには、yield する活性とトップレベル driver の間に
Rust 再帰（`eval_expr` / `eval_call` / builtin の要素ループ）が 1 段も挟まっていてはならない。そこで
「どの呼び出し位置を driver の frame 上で実行するか」を段階的に広げる。

- **e1** は、式の途中を保存しなくてよい唯一の位置＝**文の値そのものがユーザー関数呼び出し**である場合だけを
  driver 上で実行し、関数本体の文/反復境界を yield 可能にする（式 frame は作らない）。
- **e2** は式 frame（`FrameKind::Expr`）を入れ、**ユーザー関数呼び出しを含む任意の部分式**へ広げる。
- **e3** は **builtin / host function / cooperative host function の呼び出しで、引数が呼び出しを含む場合**の
  引数境界へ広げる。
- **e4** は **`map` / `filter` / `each` の要素ループ**を明示反復にし、callback 本体の文/反復境界と要素境界を
  yield 可能にする。

各 sub-slice の完了時点で、まだ driver 上へ移していない呼び出し位置は**従来どおりの Rust 再帰（その内側は
`can_yield = false` で完了まで実行）**のまま残す。これは sub-slice 間の過渡状態で、その位置の yield は
§4.2 の超過として後続 sub-slice が done になるまで残る（各 sub-slice の「範囲外」に列挙する）。
過渡状態でも観測挙動は壊れない。同期経路は全 sub-slice を通じて一切変えず、poll 経路は「yield できる位置が
増える」以外の観測（結果・副作用順序・`committed.fuel`・エラー行・trace・catch 先）を同期経路と一致させる。

**依存順**: e1 → e2 → e3 → e4 の直列とする（e2 は e2a → e2b の 2 つに割った。下の「e2 実装スライス」の注記）。e2・e3・e4 はいずれも e1 の「呼び出しを driver 上の Call frame で
実行し、終了時に値を受け渡し先へ届ける」機構の上に載る。e3 は e2 の式 frame（引数の operand stack）を使う。
e4 の要素ループ自体は e1 だけに依存するが、`map` が式の中や他の呼び出しの引数に現れる組み合わせ
（例: `len(map(xs, f))`、`map(xs, f) + ys`）は e2・e3 の上でしか yield 可能にならず、TEC-AT-01 の全境界を
跨ぐ組み合わせテストを置く場所を 1 つに決めるため、e4 を最後に置く。ループは実装 PR を同時に 1 本しか
開かないため、直列化による待ちは生じない。

**呼び出しを含む式（全 sub-slice 共通の定義）**: 部分式のどこかに `Expr::Call` を含む式をいう（静的に判定する）。
`Expr::Lambda` の本体は定義時に評価されないので数えない。式の中で反復を起こすのは呼び出し（`map` / `filter` /
`each` を含む）だけなので、「呼び出し・反復を含む部分式」はこの定義と一致する。callee が実行時にユーザー関数・
builtin・host のどれに解決されるかは静的には決まらない（user binding が builtin より優先する）ため、判定は
「`Expr::Call` を含むか」で保守的に行い、実行時に callee を解決してから各 sub-slice の経路へ振り分ける。

**全 sub-slice 共通の完了条件**:

- `cargo fmt --check` / `cargo clippy --all-targets --all-features -- -D warnings` / `cargo test --all-features`
  が緑。既存テストは無改変（TEC-AT-03 のその sub-slice 分）。
- 同期経路（`can_yield = false` の `Evaluator::run` / `run_phased` / `run_repl_submission`）のコードパスを変えない。
  式 frame を積まず、fuel・heap（`live_heap` / `peak_heap`）・エラー行・trace・stdout・exit code が現状と一致する。
  terminal まで poll する入口（`Engine::run`〔CLI を含む〕・alpha `execute` / `execute_repl_submission`・`run_for_repl`）は
  コードパスではなく観測値を変えない（e1 判断 Q1 = A。e1 小節「判断の記録」）。
- その sub-slice で driver 上へ移した呼び出し位置について、強制 yield フックで全境界を yield させた poll 経路の
  結果が同期経路と一致することを `tests/engine_api.rs` で固定する（TEC-AT-01 のその sub-slice 分）。
- documentation-guide の Definition of Done に従い、done にする実装 PR で本 spec の逸脱一覧を更新する
  （`execution-control.md` は逸脱一覧をまだ持たないため、最初に done を出す sub-slice（e1）の実装 PR で
  逸脱一覧を新設する＝documentation-guide「逸脱一覧の持ち方」の移行注記）。各 sub-slice の「範囲外」に
  書いた過渡的な未実装範囲は、その sub-slice が done になった時点の逸脱一覧に行として載せ、後続 sub-slice
  が解消したときに削除する。

##### tree-expr-continuation-e1 実装スライス（文レベルの関数呼び出しを driver 上で実行・関数本体の yield・強制 yield フック）

- **範囲**:
  - **文レベルのユーザー関数呼び出しを driver 上で実行する（poll 経路だけ）**。対象は、`can_yield = true` の
    driver が実行する文のうち、次の 4 つの値の位置にある式が **それ自体 `Expr::Call` で、callee と全引数が
    呼び出しを含まない**ものに限る。
    - 式文（`Stmt::ExprStmt`）、`let`（`Stmt::Let`）の右辺、再代入（`Stmt::Assign`）の右辺、`return` の値
  - その呼び出しの callee が実行時にユーザー関数（`Value::Fn`）へ解決されたときだけ、`eval_call` と
    **同じ順序**（builtin / cooperative / host の fallback 判定 → `count_step` → 深度検査 → callee 評価 →
    callable 検査 → arity 検査 → 引数評価 → `push_call_frame` → captured / self-binding / 引数の束縛 →
    call trace の push）で前処理を行い、`drive_call_body` の Rust 再帰に入る代わりに Call frame を
    `StmtStep::Enter` で driver へ渡す。builtin / cooperative / host に解決された場合や、callee・引数が
    呼び出しを含む場合は従来の `eval_expr` 経路（その内側は `can_yield = false`）のまま。
  - Call frame（`FrameKind::Call`）に **「この活性が driver 上で実行されている（yield 可能）か」** と
    **戻り値の受け渡し先**（捨てる / `let` で束縛する名前 / 再代入する名前 / 呼び出し元の `return`）を持たせる。
    Call frame が pop するとき（本体完了・`return`）に値を受け渡し先へ届け、`let` / 再代入は同期経路と同じ
    `env_set` / `env.update` とエラー写像を使う。`return` の受け渡し先は呼び出し元の活性で
    `FlowSignal::Return` として unwind する。
  - 終了経路を同期経路（`eval_call` → `drive_call_body` の再入）と一致させる。callee 内で未捕捉のエラーは
    呼び出し元側の最も近い `try` が捕捉し、`break` / `continue` が Call 境界に達したら同期経路と同じ行・同じ
    trace で「ループ外」エラーになる。エラー時の call trace も同期経路と同じ内容にする（Call frame を畳む
    前後のどちらで snapshot するかを含めて揃える）。
  - **関数本体の文/反復境界での slice yield**: driver 上で実行される関数本体（入れ子の文レベル呼び出しを
    含む）の文/反復境界で、トップレベルと同じ slice fuel 判定を行う。resume は `run_slice` がトップレベル
    driver から永続 frame stack を回し直すだけで、関数本体の途中から続く。
  - cooperative host-call の `may_yield` と audit drain queue 上限の判定は、トップレベル（Call frame の外）
    だけのまま（上の「can_yield の伝播」）。`can_yield_ctx` を「slice fuel を確認してよいか」と
    「トップレベルか」に分けて持つ。
  - **テスト専用の強制 yield フック**（上の小節の定義どおり、`unstable-bytecode` または `cfg(test)` 限定）を
    追加し、e1 時点の全 yield 可能点（トップレベルと driver 上の関数本体の文/反復境界）で必ず yield させる。
    e2〜e4 は同じフックを自分の新境界へ広げる。
  - 深度上限 `MAX_USER_CALL_DEPTH = 128` の検査は driver 上の呼び出しでも同期経路と同じ位置・同じエラーにする。
- **範囲外（後続 sub-slice）**: 式の途中にある呼び出し・callee や引数が呼び出しを含む呼び出し・`if` / `while`
  の条件・`for` の反復元・index 代入の index と値に現れる呼び出し（e2）。builtin / host / cooperative の
  引数に現れる呼び出し（e3）。`map` / `filter` / `each` の callback 本体と要素境界（e4）。これらの内側は
  e1 時点では従来どおり `can_yield = false` で完了まで実行し、§4.2 の超過として扱う。
- **heap**: Call frame の `heap_charge` は従来どおり 0。e1 は式 frame を作らないため、poll 経路の
  continuation heap も増えない。
- **受入基準（e1 が満たす部分）**:
  - **TEC-AT-01（e1 分）**: 強制 yield フックで全境界を yield させ、関数本体の文/反復境界を跨ぐ script
    （4 つの値の位置それぞれ・入れ子の文レベル呼び出し・関数本体のループ・再帰）が、同期経路
    `Engine::run` と stdout・`committed.fuel`・terminal 種別・非 `Yielded`/`Resumed` の audit event 列で
    一致する。終了経路（callee 内エラーを呼び出し元の `try` が捕捉する・callee 内エラーが未捕捉で terminal に
    なる・関数内の `break`/`continue` がループ外エラーになる）についても、エラー種別・行・trace が一致する。
  - **TEC-AT-03（e1 分）**: 既存テスト無改変で緑（`may_yield_false_requires_ready_else_internal_terminal` と
    B2-AT-09 を含む）。同期経路の観測値不変。
  - **TEC-AT-04（e1 分）**: poll 経路で `return f(n - 1)` 形の深い再帰が、同期経路と同じ
    `call_depth_limit`（同じ行）で止まる。既存 `tests/defensive*.rs` は無改変で緑。
  - **TEC-AT-05（e1 分）**: 関数本体の途中での yield を跨いでも submission journal が保持され、terminal で
    だけ commit / rollback される（REPL transaction の成功・失敗の両方）。
  - **TEC-AT-06（e1 分）**: 文レベルで呼んだ関数の本体にループを持つ script を `PollSlice::max_fuel = 16`
    で実行したとき、1 回の poll で実行される fuel が「slice + 区間内の callback なし bulk builtin 1 回分の
    単独実測 fuel」以内（式起因の超過は現行課金で 0）。`Yielded`/`Resumed` が対で順序が単調。
- **検証**: 全 sub-slice 共通の完了条件。テストは `tests/engine_api.rs`（TEC-AT-01/05/06）、
  `tests/defensive*.rs` への追加または既存の無改変緑（TEC-AT-04）、Call frame の受け渡し先と終了経路の
  unit test は `src/eval.rs`。
- **実装（e1 実装 PR の時点の記録）**: `src/eval.rs` の `exec_stmt` が、yield 可能な driver
  （`can_yield_ctx`）で 4 つの値の位置の式が上記の文レベル呼び出しのときだけ `exec_driver_call_stmt` を通す。
  前処理は `eval_call` から分離した `prepare_call` を同期経路と共有し（同じ順序）、ユーザー関数なら
  `FrameKind::Call { driver: Some(DriverCall { dest, .. }) }` を `StmtStep::Enter` で積む。`DriverCall` は
  受け渡し先（`CallDest`：捨てる / `let` / 再代入 / `return`）と関数値（同期経路のローカル `func_value` と
  同じ寿命で live heap を揃える）を持つ。本体完了・`return` は `finish_driver_call` が Call frame を畳んでから
  受け渡し先へ届け、`break` / `continue` は Call frame を畳んでから「ループ外」エラーを呼び出し元の文のエラー
  として捕捉処理する（`drive_flow` / `recover_error`）。callee 内のエラーは `handle_error` が driver 上の
  Call frame を越えて呼び出し元の `try` を探す。`may_yield` と audit drain queue 上限は `in_driver_call()`
  が偽のときだけ判定する。強制 yield フックは `Evaluator::force_yield_every_boundary`（テスト専用 surface は
  `EmbeddingContext::force_yield_at_every_boundary_for_test`、`unstable-bytecode` 限定・`#[doc(hidden)]`）で、
  slice で 1 文・1 反復以上進めた後の全境界で yield する。`abort_session` は yield 中の driver 上の呼び出しの
  呼び出し元スコープと call trace を rollback より前に戻す。同期経路（`Evaluator::run` / `run_phased` /
  `run_repl_submission`、`can_yield = false`）は従来の分岐だけを通り、再帰 1 段あたりの Rust stack 使用量も
  増やさない（新しい処理は `#[inline(never)]` の別関数に置く）。
  - 注記: 本小節と共通の完了条件は「同期経路」を `Engine::run` / `execute` / REPL と書くが、実装では
    埋め込み `Engine::run`（CLI を含む）と alpha `Engine::execute` / `execute_repl_submission` は
    `begin_execution` / `run_slice`（`can_yield = true`）を通る。したがって e1 の driver 上の呼び出しはこれらの
    入口でも有効になる（slice 上限が大きいので通常は yield しない）。観測値は既存テストの無改変緑に加え、
    TEC-AT-01 のテストで stdout・fuel・エラー行・trace を `can_yield = false` の `Evaluator::run` と、
    audit event 列と terminal（usage を含む）を強制 yield なしの実行と、`src/eval.rs` の unit test で
    `live_heap` / `peak_heap` を `Evaluator::run` と比べて一致を確かめた。
- **判断の記録（e1 実装 PR #254 のレビューで判断待ちにしたもの。kamonabe が 2026-10-24 に選択）**:
  - **「同期経路」の扱い（Q1 = A）**: 実装はこのまま受け入れる。tree-expr-continuation の各小節と
    sub-slice 共通の完了条件でいう「同期経路」は、`can_yield = false` の `Evaluator::run` 系
    （`Evaluator::run` / `run_phased` / `run_repl_submission`）と読み替える。`Engine::run`（CLI を含む）・
    alpha `execute` / `execute_repl_submission`・`run_for_repl` のように terminal まで poll する入口は、
    コードパスを変えないことではなく**観測値を変えないこと**を条件にする。
    理由: §9.2 で `Engine::run` は同じ handle を terminal まで poll する入口と定めており、入口ごとに
    実行経路を分けると production に同じ文を実行する経路が 2 つでき（`audit_slice_fuel` を使う
    `Engine::run` は結局 driver 経路を通るため、分けても文言どおりにはならない）、e2〜e4 でさらに広がる。
    観測値の一致は比較テストで固定しており、マニフェストの予測可能性を保てる。
    共通の完了条件と TEC-AT-01/03 の文言（「同期経路 `Engine::run`」など）は、e2 の前の PR で直す。
  - **slice fuel の既存逸脱と TEC-AT-06（Q2 = A）**: e1 はこのまま受け入れる。slice の使用量が
    `count_step` だけを数え、builtin の per-element fuel を数えない挙動（Slice 3 PR-d から）は冒頭の
    逸脱一覧の行で記録し、解消は後続に回す（`fuel-bulk-chunk-yield` の設計範囲に含めるか、
    `needs-design` issue を立てる）。TEC-AT-06（e1 分）は bulk builtin を 1 回だけ呼ぶ script で確かめる。
    理由: 直すと yield の位置（poll の粒度）が変わり既存の slice 系テストに響きうるため、e1 の範囲
    （課金と観測を変えない）とスライスの大きさの目安（仕組みは 1 つまで、[開発ループ手順書](loop-runbook.md)
    「1 回分の上限」）を越える。現状は逸脱一覧で正しく記録する（[ドキュメント運用ガイド](documentation-guide.md)
    「逸脱一覧の持ち方」）。

##### tree-expr-continuation-e2 実装スライス（式 frame：ユーザー関数呼び出しを含む部分式・call 引数境界）

> **e2a / e2b への分割（e2 の設計 PR）**: e2 は 1 PR に収まらない（e1 の実装 PR #254 で `src/` の差分がテストを除いて
> +478 / -76 行だった〔`src/eval.rs` 434・`src/ast.rs` 29・`src/embedding.rs` 15。`mod tests` は数えない〕。e2 はその機構の上に式 frame・7 つの式種・4 つの文の式スロットを足すため、
> [開発ループ手順書](loop-runbook.md)「スライスの大きさの目安」の 400 行を大きく超える。受入基準も 7 項目あり、5 を超える）。そこで下の
> **e2a（式 frame の導入と、e1 の 4 位置での call・二項・単項）** と **e2b（index・list / dict リテラル・
> f-string と、文の式スロット）** の 2 つの実装スライスへ割った。本小節は e2 全体の設計の記録であり、
> 実装範囲・受入基準・完了条件は e2a / e2b の小節を正とする。「e2 が done」は e2a と e2b がともに done を
> 意味する。式 frame の heap は、e2a の判断 Q1 = A で課金なし（0）に確定した（本小節の元の記述は 96 byte 固定だった）。

- **範囲**:
  - `FrameKind::Expr` を追加する。保存するのは上の小節「continuation へ保存する式の評価状態」のうち
    operand stack・式 cursor・callee 引数の部分評価結果・保留中の演算（callback 反復状態は e4）。
  - poll 経路で、呼び出しを含む式を式 frame で段階評価する。対象の式種は二項演算（`and` / `or` の短絡を含む）・
    単項演算・index・list / dict リテラル・f-string・call（callee と引数）。呼び出しを含まない operand は
    従来どおり `eval_expr` の Rust 再帰で 1 つの値として評価してよい（境界規則）。
  - 式 frame の operand として現れた call の callee がユーザー関数に解決されたら、e1 と同じ前処理順序で
    Call frame を driver 上に積み、戻り値の受け渡し先を「親の式 frame の operand stack」とする。builtin /
    host / cooperative に解決され、かつ引数が呼び出しを含む場合は、e2 時点ではその call 全体を従来の
    `eval_expr` 経路で評価する（e3 の範囲）。
  - 文の式スロットを式 frame の対象に広げる: e1 の 4 位置（値が呼び出しを含む任意の式になる）に加え、
    `if` / `while` の条件（`while` は反復ごとの再評価を含む）、`for` の反復元、index 代入の index と値。
    評価順序（index 代入は binding 解決 → index → value の規範順序）とエラー行は同期経路と一致させる。
  - yield 可能点: call の各引数評価の境界（引数 i の評価後・引数 i+1 の評価前、全引数評価後・本体へ入る前）と、
    呼び出しを含む operand の境界。強制 yield フックをこれらへ広げる。
  - heap: 式 frame は課金しない（`heap_charge` 0。operand は二重課金しない。e2a の判断 Q1 = A）。
  - 式 frame の途中で起きたエラーの捕捉（最も近い `try`）、`break` / `continue` / `return` による unwind で
    式 frame を正しく畳み、heap を release する。
- **範囲外（後続 sub-slice）**: builtin / host / cooperative の呼び出しで引数が呼び出しを含む場合（e3）。
  `map` / `filter` / `each` の callback 本体と要素境界（e4）。
- **受入基準（e2 が満たす部分）**:
  - **TEC-AT-01（e2 分）**: 強制 yield フックで、call 引数境界・呼び出しを含む operand 境界・文の式スロット
    （条件・反復元・index 代入）を跨ぐ script が同期経路と一致する（検査項目は TEC-AT-01 本文どおり）。
  - **TEC-AT-02（e2 分）**: 深い式・入れ子 call・引数評価の途中・短絡評価の途中で yield させ、resume 後の
    最終値と副作用列が同期経路と一致する。operand stack・式 cursor・部分評価済み引数が保存されることを
    unit test で固定する。
  - **TEC-AT-03（e2 分）**: 既存テスト無改変で緑。同期経路の `live_heap` / `peak_heap` が不変
    （同期経路は式 frame を積まず、poll 経路の式 frame も課金しない）。
  - **TEC-AT-04（e2 分）**: poll 経路で `f(n - 1) + 1` 形（式の途中の再帰）が同期経路と同じ
    `call_depth_limit` で止まり、深い呼び出しを含まない式は従来どおり有限停止する。
  - **TEC-AT-05（e2 分）**: 式の途中の yield を跨いでも submission journal が保持され、terminal でだけ
    commit / rollback される。
  - **TEC-AT-06（e2 分）**: 式の中で呼んだ関数の本体にループを持つ script を最小 slice で実行し、1 poll の
    実行量が e1 と同じ上限以内。
  - 式 frame の heap: 式 frame は課金されず、push・pop・unwind・エラー捕捉のどこでも `live_heap` が動かない。
    poll 経路の `live_heap` / `peak_heap` が同じ script の同期経路と一致する（unit test）。
- **検証**: 全 sub-slice 共通の完了条件。テストは `tests/engine_api.rs`（TEC-AT-01/02/05/06）、
  `tests/defensive*.rs`（TEC-AT-04）、式 frame の不変条件と heap は `src/eval.rs` の unit test。

###### tree-expr-continuation-e2a 実装スライス（式 frame の導入：e1 の 4 位置での call・二項・単項）

- **範囲**:
  - **式 frame（`FrameKind::Expr`）を追加する**。段階評価中の式ノード 1 つにつき式 frame を 1 個積む。保存するのは
    上の小節「continuation へ保存する式の評価状態」のうち、評価中のノード・式 cursor（次に評価する operand）・
    評価済みの operand（operand stack）・callee 引数の部分評価結果・保留中の演算（callback 反復状態は e4）。
    - Frame は AST を借用しない（PR-d の `'static` 規律）。式ノードは `LoopSite` と同じく **所有する Block と文の
      index**、文の中の式スロット、そこから評価中のノードまでの **子の位置の経路** で辿る。AST の clone・unsafe な
      参照は使わない。
  - **対象の位置**: poll 経路（`can_yield_ctx`）で、e1 の 4 位置（式文・`let` の右辺・再代入の右辺・`return` の値）の
    式が呼び出しを含むとき。ただし e1 の形（式がそれ自体 `Expr::Call` で、callee と全引数が呼び出しを含まない）は
    e1 の経路のままにし、式 frame を作らない。
  - **段階評価する式種（e2a）**: call・二項演算（`and` / `or` の短絡を含む）・単項演算。式 frame の operand のうち
    次のものは従来どおり `eval_expr`（Rust 再帰、その内側は `can_yield = false`）で 1 つの値として評価する。
    - 呼び出しを含まない operand（境界規則）。
    - e2a で段階評価しない式種（index・list / dict リテラル・f-string。e2b）のノード。呼び出しを含んでいても
      まるごと 1 つの operand として評価する。
    - callee が識別子で、その時点で user binding を持たない call（builtin / cooperative host / 登録 host function、
      または未定義名）。call 全体を従来の `eval_call` 経路で評価する（builtin / host / cooperative は `CallPrep::Done`、
      未定義名は `count_step` → callee の評価で未定義名エラー。引数の段階評価は e3）。
  - **call の段階評価**: 同期経路の `prepare_call` と同じ順序で進める。`count_step` → 深度検査 → callee 評価
    （呼び出しを含むなら段階評価）→ callable 検査 → arity 検査（AST の引数の個数で行い、引数評価より前）→ 引数を
    左から 1 つずつ評価（呼び出しを含むものは段階評価）→ `push_call_frame` → captured / self-binding / 引数の束縛 →
    call trace の push → e1 と同じく `FrameKind::Call { driver: Some(DriverCall { .. }) }` を積む。前処理の各段は
    同期経路と共有する関数に分け、同期経路の順序とエラーを変えない。戻り値の受け渡し先 `CallDest` に「親の式 frame の
    operand stack」を加える。
  - **二項・単項の段階評価**: operand を左から評価し、揃ったら同期経路と同じ関数（`eval_binop` + `track_result`、
    `eval_unary`）で適用する。`and` / `or` は左辺の真偽で右辺を評価しない（同期経路と同じ値を返す）。
  - **式 frame の完了**: 根の式 frame が値を出したら、e1 の `deliver_call_result` と同じ処理で文の受け渡し先
    （捨てる / `let` / 再代入 / `return`）へ届ける。エラー写像と行は同期経路と同じ。
  - **yield 可能点**: 式 frame が最上位にある driver ループの境界（operand i の評価後・operand i+1 の評価前、call の
    全引数評価後・本体へ入る前）。ここでは **slice fuel の判定と強制 yield フックだけ** を行い、次の 2 つは従来どおり
    文/反復境界だけで行う（式の operand 境界へは広げない。上の小節「逸脱」の cancel / deadline と同じ扱い）。
    - audit drain queue 上限の yield（B-2。`audit_queue_progress` の意味も変えない）。
    - cancel / deadline の checkpoint。
  - **強制 yield フック**: e1 のフックを上の境界へ広げる。resume 直後に何も進めず再 yield しないよう、前進の条件に
    「この slice で operand を 1 つ以上評価した」を加える。前進は audit drain queue の前進フラグとは別に持つ。
  - **終了経路**: 式 frame の途中のエラーは、`attach_trace` の後に `handle_error` が式 frame を畳んで（heap があれば
    release して）最も近い `try` へ渡す。`return` / `break` / `continue` による unwind、`abort_session` でも式 frame を
    畳む。エラー行・trace は同期経路と同じにする。
  - **cooperative host-call の Pending**: トップレベルの文の式 frame の中で cooperative host function が Pending を
    返した場合（`may_yield` は e1 と同じく driver 上の Call frame の外で真）、その文の式 frame をすべて畳み、文の
    cursor を戻して yield する。resume では文全体を最初から評価し直す（現行 §4.7「同じ文の再評価」と同じ観測。
    式の途中状態からは再開しない）。関数本体の中は e1 と同じく `may_yield = false`。
  - **heap**: 式 frame の `heap_charge` は Call frame・`while` / `for` の制御 frame と同じく **0** とする（下記の判断 Q1 = A）。
    operand は課金済みの `Value` を参照するだけで二重課金しない。
- **範囲外（後続）**: index・list / dict リテラル・f-string の段階評価と、`if` / `while` の条件・`for` の反復元・index
  代入の index と値（e2b）。builtin / host / cooperative の引数に現れる呼び出し（e3）。`map` / `filter` / `each`（e4）。
  これらの内側は e2a 時点では `can_yield = false` で完了まで実行し、§4.2 の超過として扱う。
- **想定の変更規模**: `src/eval.rs` がおおむね 400 行（式 frame の型と経路の辿り方 50、call の段階評価と
  `prepare_call` の段分け 110、二項・単項 50、4 位置からの入口と受け渡し先 80、終了経路・Pending・`abort_session`
  70、境界の判定と強制 yield フック 40）。`src/ast.rs` は経路で子を辿る補助が要れば 30 行以内。触る仕組みは tree
  evaluator の driver 1 つ。受入基準は下の 5 項目。実装中に 400 行を大きく超えそうなら、短絡（`and` / `or`）と単項の
  段階評価を e2b へ回してよい（そのときは実装 PR を止めて、本小節と e2b 小節を直す設計 PR を先に出す）。
- **受入基準（e2a が満たす部分）**:
  - **TEC-AT-01（e2a 分）**: 強制 yield フックで全境界を yield させ、4 位置それぞれで call の引数・callee・二項・単項・
    短絡に呼び出しを含む script（例: `let y = f(x) + g(x)`、`return h(f(x), 1)`、`f(x)(y)`、`not f(x)`、
    `f(x) and g(x)`、e2a の式種の中に index などの e2b の式種が入る組み合わせ）が、同期経路（`Evaluator::run`）と
    stdout・`committed.fuel`・terminal 種別・非 `Yielded`/`Resumed` の audit event 列で一致する。callee 内エラーの
    `try` 捕捉・未捕捉 terminal・callable / arity エラー・未定義名についても、エラー種別・行・trace が一致する。
  - **TEC-AT-02（e2a 分）**: 深い式・入れ子 call・引数評価の途中・短絡評価の途中で yield させ、resume 後の最終値と
    副作用列が同期経路と一致する。operand stack・式 cursor・部分評価済み引数が yield を跨いで保存されることを
    unit test で固定する。
  - **TEC-AT-03（e2a 分）**: 既存テスト無改変で緑（`may_yield_false_requires_ready_else_internal_terminal` と B2-AT-09、
    cooperative host-call の Pending を含む既存テストを含む）。同期経路と、terminal まで poll する入口の観測値が不変。
    poll 経路の `live_heap` / `peak_heap` が同じ script の `Evaluator::run` と一致する（unit test）。
  - **TEC-AT-04（e2a 分）**: poll 経路で `f(n - 1) + 1` 形（式の途中の再帰）が同期経路と同じ `call_depth_limit`
    （同じ行）で止まる。既存 `tests/defensive*.rs` は無改変で緑。
  - **TEC-AT-05（e2a 分）**: 式の途中の yield を跨いでも submission journal が保持され、terminal でだけ commit /
    rollback される（REPL transaction の成功・失敗の両方）。
- **検証**: 全 sub-slice 共通の完了条件。テストは `tests/engine_api.rs`（TEC-AT-01/02/05）、`tests/defensive*.rs` への
  追加または既存の無改変緑（TEC-AT-04）、式 frame の保存状態・終了経路・heap の unit test は `src/eval.rs`。
- **判断の記録（返信なしのため推奨案で確定）**:
  - **Q1. 式 frame の heap 課金: A（式 frame の `heap_charge` を 0 にする）で確定。** kamonabe の返信が 2 時間無かったため、
    [開発ループ手順書](loop-runbook.md) §7.5 により推奨案で確定した（[Slack スレッド](https://kamo-dev.slack.com/archives/C0CB1H2HHJB/p1792869955313449)）。
    上の小節「式 frame の heap 課金」は「同期経路は式 frame を積まないので `live_heap` / `peak_heap` は変わらない」を前提に
    96 byte 固定を課金すると決めていた。ところが e1 判断 Q1 = A により `Engine::run`（CLI を含む）と alpha `execute` は
    driver 経路を通り、それらは観測値を変えないことが条件になった。96 byte を課金すると、これらの入口の `peak_heap` と
    heap 上限で止まる位置が変わる。そこで式 frame を課金しないことにした（Call frame・`while` / `for` の制御 frame と同じ）。
    - **理由**: (i) e1 判断 Q1 = A（terminal まで poll する入口は観測値を変えない）。(ii) manifesto 原則 5「同じ言語
      version、スクリプト、入力…から、同じ結果…が得られる」: 課金すると同じ script が `Evaluator::run` と `Engine::run` で
      heap 上限に達して止まる位置が変わる。(iii) 原則 5「意味論、評価順、エラー、資源消費が実行backendごとに変わる
      ことを許容しない」と上の小節「VM との heap 差も広げない」: 課金すると tree だけが式 frame を課金し、VM（value stack は
      未課金）との heap 差が広がる。(iv) §5.2（共有は 1 回課金）。式 frame は同期経路では Rust 再帰（未課金）が持つ評価状態を
      heap へ移しただけで、新しい値は作らない。operand は課金済みの `Value` を参照するだけ。
    - **原則 3（すべての実行に予算を設ける）との比較**: 式 frame の個数は `MAX_AST_DEPTH`（256）× 呼び出し深度 128 で
      有界（最大で約 3.3 万個）。同期経路では同じ深さの評価状態を Rust stack が持ち、これも課金していない。式 frame は
      その状態を heap へ移しただけなので、課金しなくても「予算の外で増えるメモリ」は同期経路から増えない
      （Rust stack 側の上限は 8 MiB スレッドと深度上限が引き続き抑える）。
    - **§5.1 との関係**: §5.1 の表は continuation frame を種類で区別せず「96 + local slot 32 × count」としていたが、
      実装はすでにスコープを作る frame だけに「96 + local slot 32 × count」を課金し（`while` の反復・`if` の分岐・`try`
      本体は 96、`for` の反復・`catch` 本体は 128）、program 直下の Block・Call・`while` / `for` の制御 frame は 0 である。
      §5.1 の表に「課金するのはスコープを作る frame（96 + local slot 32 × count）。Call・ループ制御・式 frame は課金しない」と
      注記して目標仕様を実装に合わせた（逸脱一覧には足さない）。
    - **反映した箇所**: §5.1 の表の注記、上の小節（式 frame の heap 課金・poll 経路の heap 観測・continuation heap 課金・
      TEC-AT-03）、e2 小節の範囲と受入基準、e2a の heap、e4 の反復 frame の heap。
    - **採らなかった案 B**（確定どおり 96 byte を課金する）: 根拠は §5.1 の課金表を文字どおり適用することと manifesto 原則 3。
      ただし e1 判断 Q1 = A と食い違い、TEC-AT-03 と共通の完了条件に例外を足して既存の heap 系テストの期待値を変える
      必要があり、VM との heap 差も広がるため採らなかった。

- **実装（e2a 実装 PR の時点の記録）**: `src/eval.rs` に `FrameKind::Expr(Box<ExprFrame>)` を足した。`ExprFrame` は
  所有する Block・文の index・式スロットから評価中のノードまでの子の位置の経路（`ast::expr_child` の index 列）・文の行・
  operand stack・call の前処理の段（`started` / `checked`）・受け渡し先（`CallDest`）を持ち、AST を借用しない。
  `exec_driver_call_stmt` は 4 位置の式が e1 の形でなく `needs_expr_frame`（呼び出しを含む call〔callee が user binding を
  持たない識別子のものを除く〕・二項・単項）なら根の式 frame を `StmtStep::Enter` で積む。driver ループは最上位が式 frame の
  とき、slice fuel と強制 yield フックの判定だけをしてから `drive_expr_step` → `step_expr` で 1 段（operand を 1 つ評価する
  か、子の式 frame を積むか、演算を適用して値を届ける）進め、audit drain queue 上限と cancel / deadline の checkpoint は
  文/反復境界だけで行う。call の前処理は `prepare_call` を `begin_user_call`（`count_step` → 深度検査）・
  `check_user_callee`（callable → arity）・`bind_user_call`（`push_call_frame` → 束縛 → call trace）に分けて同期経路と
  共有し、本体は `CallDest::ExprResult` を受け渡し先にした driver 上の Call frame で実行する。子の式 frame の値は
  `CallDest::Operand` で親の operand stack へ積む。強制 yield フックの前進条件に `expr_progress`（この slice で式 frame を
  1 段以上進めた）を足した。cooperative host-call の Pending は、その文の式 frame をすべて畳んで文の cursor を戻して
  yield する。式 frame は Call frame と同じく `abort_session` の巻き戻しで畳まれる（特別な処理は要らない）。

###### tree-expr-continuation-e2b 実装スライス（index・list / dict・f-string と、文の式スロット）

> **設計の更新（e2b を `ready` にした設計 PR）**: 範囲・範囲外・受入基準 ID は e2 分割の設計 PR #262 のまま変えていない。
> e2a の実装 PR #268 で式 frame の形（`ExprFrame` が文の式スロットから子の位置の経路でノードを辿る・1 段ずつ進める・
> `CallDest::Operand` / `ExprResult`）が決まったので、それに合わせて「保存する状態」「式スロットの指し方と入口」
> 「受け渡し先」「受入基準の確かめ方」を書き足し、「想定の変更規模」を e2a の実績で見直した。

- **範囲**:
  - **段階評価する式種を広げる**: e2a の式 frame に index・list リテラル・dict リテラル・f-string を加える（e2a の 4 位置を
    含む、式 frame で評価するすべての位置に効く）。対象は呼び出しを含むノードだけで、呼び出しを含まない子は従来どおり
    `eval_expr` で 1 つの値にする（境界規則）。評価順・課金・エラーの位置は同期経路の `eval_expr` と同じにする。
    - index: object → index の順に評価し、同期経路と同じ `eval_index` で適用する。AUD-041 の参照読み
      （`識別子[副作用のない式]`）は呼び出しを含まないので式 frame を作らない（従来どおり）。
    - list: 要素を左から評価し、要素ごとに `check_collection` を行ってから `Value::new_list` で作る。
    - dict: key → key の型検査 → value → `check_collection`（新しい key のとき）の順を entry ごとに行い、
      `Value::new_dict` で作る。重複 key は同期経路と同じく後の値で上書きする。
    - f-string: 式の部分を評価するたびに `render_display` で描画して結果へ足し（fuel と per-item byte 上限の課金は
      その時点）、リテラル部分はそのまま足し、最後に `track_result` する。
  - **式 frame に保存する状態の追加**（上の小節「continuation へ保存する式の評価状態」の具体化）: e2a の operand stack・
    式 cursor に加え、次を式 frame に持たせる。どれも同期経路では `eval_expr` / `exec_stmt` のローカル変数が持っていたもので、
    新しい値は作らない（heap は e2a と同じく課金しない）。
    - dict: 作りかけの map（key の型検査と `check_collection` を済ませた entry）と、評価済みで value を待つ key。
    - f-string: 描画済みの部分を連結した作りかけの文字列。
    - index 代入: 先に解決した代入先 binding の cell（同期経路の `exec_stmt` がローカルに持つ `cell` と同じもの）と、
      評価済みの index の値。
  - **文の式スロットを式 frame の対象に広げる**: e2a の 4 位置に加え、`if` の条件、`while` の条件（反復ごとの再評価を
    含む。反復完了後の `count_step` は従来どおり条件の評価より前）、`for` の反復元、index 代入の index と値。
    - **式スロットの指し方**: 式 frame の根は「所有する Block・文の index・その文のどの式スロットか」で指す（e2a の
      `stmt_value_expr` が返す値の位置に、条件・反復元・index 代入の index / value を加える）。経路で子を辿る規則は e2a と同じ。
    - **入口**: 新しい式スロットは、式が `needs_expr_frame`（e2b の式種を加えたもの）に当たるとき根の式 frame で評価する。
      e1 の形（それ自体 `Expr::Call` で callee と全引数が呼び出しを含まない）の式も、新しい式スロットでは根の式 frame で
      評価する（e1 の Call frame へ直接入る経路は e1 の 4 位置だけに残す。式 frame の call も同じ前処理順序を通るので
      観測は変わらない）。
    - **受け渡し先**: 根の式 frame の値は、同期経路の `exec_stmt` / `advance_frame` がその値に対して行っていた処理へ届ける。
      `if`: 真偽で分岐を選び `enter_scoped_block` の frame を積む。`while`（最初の評価）: 偽なら完了、真なら `enter_while` の
      frame を積む。`while`（反復ごとの再評価）: `advance_frame` と同じく真なら `pending_step` を立てて次反復の本体を積み、
      偽なら While frame を畳む。`for`: 同期経路と同じ materialize（list / dict / 文字列ごとの `check_collection`、
      反復できない値のエラー）をして `enter_for` の frame を積む。同期経路と共有できる処理は関数に分けて共有し、
      同期経路の順序・エラーを変えない。
    - **index 代入**: binding の解決（未定義名のエラーは index / value の副作用より前）→ index → value の規範順序で評価し、
      `journal_cell` → `assign_index` は同期経路と同じ関数で行う。index と value の一方だけが呼び出しを含む場合も、
      もう一方は同じ順序の位置で `eval_expr` で評価する。
  - `while` の条件と `for` の反復元の評価のエラーは、反復境界（`advance_frame`）でのエラーと同じく最も近い `try` へ渡す。
    エラー行は同期経路と同じく文の行。
  - **cooperative host-call の Pending**: 文の式スロットの式 frame の中で Pending を返した場合は e2a と同じく式 frame を
    畳んで文の評価を最初からやり直す（index 代入は binding の解決からやり直す。現行 §4.7「同じ文の再評価」と同じ観測）。
    `while` の条件の再評価（反復境界）での Pending は現行の反復境界の扱い（`advance_frame` の Pending をそのまま yield し、
    resume で While frame から条件を再評価する。反復完了後の `count_step` は再評価の前に済んでいるので二重に課金しない）と
    同じ観測にする。
  - yield 可能点・強制 yield フック・heap・終了経路は e2a と同じ規則を新しい式種と式スロットへ当てる（式 frame の 1 段の
    間が yield 可能点。slice fuel の判定と強制 yield フックだけを行い、audit drain queue 上限と cancel / deadline の
    checkpoint は文/反復境界だけ）。
- **範囲外（後続）**: builtin / host / cooperative の引数に現れる呼び出し（e3）。`map` / `filter` / `each`（e4）。
- **想定の変更規模**: `src/eval.rs` がおおむね 360 行（式スロットの指し方と根の入口 40、index・list・dict・f-string の
  段階評価と保存する状態 130、`if` / `for` の受け渡し先と materialize の共有 70、`while` の最初の評価と反復ごとの再評価 70、
  index 代入 50）。`src/ast.rs` は `expr_child` を 4 つの式種へ広げる 25 行。合わせて約 385 行で、[開発ループ手順書](loop-runbook.md)
  「スライスの大きさの目安」の 800 行に収まる（e2a は見積もり 約 430 行に対し実績 458 行。同じ比率で上振れしても 420 行前後）。
  触る仕組みは tree evaluator の driver 1 つ。受入基準は下の 4 項目。
- **TEC-AT-04 / 05 を持たない理由**: 深度検査と journal の扱いは、e2a が段階評価に入れた call の前処理（`prepare_call` と
  共有する段）と driver 上の Call frame の中にあり、e2b は呼び出しの経路を新しく足さない（足すのは式種と、式 frame へ
  入る文の位置だけ）。式 frame の途中の yield が transaction 境界でないことも e2a の TEC-AT-05 で固定済みで、e2b の
  式 frame は同じ frame 種別である。したがって e2a の TEC-AT-04 / 05 がそのまま e2b の位置にも効く。
- **受入基準（e2b が満たす部分）**:
  - **TEC-AT-01（e2b 分）**: 強制 yield フックで、index・list / dict リテラル・f-string の中の呼び出しと、`if` / `while` の
    条件・`for` の反復元・index 代入の index と値に呼び出しを含む script が、同期経路と一致する（検査項目は
    TEC-AT-01 本文どおり）。dict key の型エラー・collection 上限・index エラーの行と trace も一致する。
    - 確かめ方: `tests/engine_api.rs` に、式種・式スロットごとの script（例: `let y = xs[f(i)]`、`let a = [f(1), g(2)]`、
      `let d = {k(): f(x)}`、`let s = f"{f(x)}-{g(x)}"`、`if f(x) > 0`、`while step() < 3`、`for v in f(x)`、`xs[f(i)] = g(i)`。
      呼び出しを builtin の引数に置く形は e3 なので使わない）と
      エラー経路（dict key が文字列でない・`max_collection_elements` 超過・範囲外 index）の script を置き、強制 yield フックありの
      poll 経路と `can_yield = false` の `Evaluator::run` で stdout・`committed.fuel`・terminal 種別・エラー種別・行・trace・
      非 `Yielded`/`Resumed` の audit event 列を比べる。
  - **TEC-AT-02（e2b 分）**: list / dict の要素の途中・f-string の部分の途中・`while` の条件の途中で yield させ、resume 後の
    最終値と副作用列が同期経路と一致する。
    - 確かめ方: `tests/engine_api.rs` で、要素・部分・条件の各評価が副作用（`print`）を持つ script を強制 yield フックで
      1 境界ずつ yield させ、resume 後の値と出力順が同期経路と一致することを見る。`src/eval.rs` の unit test で、dict の
      作りかけの map・f-string の作りかけの文字列・index 代入の解決済み cell と index が yield を跨いで保存されることを固定する。
  - **TEC-AT-03（e2b 分）**: 既存テスト無改変で緑。同期経路と、terminal まで poll する入口の観測値が不変。
    - 確かめ方: 既存の `tests/`・unit test を無改変で通す（`cargo test --all-features`）。加えて `src/eval.rs` の unit test で、
      e2b の式種・式スロットを含む script の poll 経路の `live_heap` / `peak_heap` が `Evaluator::run` と一致することを見る。
  - **TEC-AT-06（e2a・e2b 分）**: e2a / e2b の式種と式スロットの中で呼んだ関数の本体にループを持つ script（例:
    `let y = f(x) + 1`、`return h(f(x), 1)`、`[f(x)]`、`while f(x) > 0`、`for v in f(x)`）を
    `PollSlice::max_fuel = 16` で実行し、1 回の poll の実行量が e1 と同じ上限以内。`Yielded`/`Resumed` が対で順序が単調。
    - 確かめ方: `tests/engine_api.rs` で上の各 script を最小 slice で poll し、poll ごとの fuel 増分が「slice + 区間内の
      callback なし bulk builtin 1 回分の単独実測 fuel」以内であることと、audit の `Yielded`/`Resumed` の対と順序を見る
      （e1 の TEC-AT-06 のテストと同じ測り方）。
- **検証**: 全 sub-slice 共通の完了条件。テストは `tests/engine_api.rs`（TEC-AT-01/02/06）、式種ごとの保存状態と heap の
  unit test は `src/eval.rs`。

##### tree-expr-continuation-e3 実装スライス（builtin・host function の呼び出し引数境界）

- **範囲**:
  - poll 経路で、callee が builtin（`map` / `filter` / `each` を除く）・登録 host function・cooperative host
    function に解決され、引数が呼び出しを含む call を、e2 の式 frame で引数を段階評価してから dispatch する。
    引数は従来どおり左から右へ評価し、`count_step`・host call 課金（`charge_host_call_request`）・capability /
    arity 検査・audit の host-call triple の順序と位置を同期経路と一致させる。
  - 引数の AST を直接読む builtin（`push` / `pop` の対象識別子、`len(識別子)` の最適化、
    `validate_context_builtin_call` の「第 1 引数が識別子か」判定）は、AST を読む部分の意味を変えない。
    呼び出しを含む引数だけを式 frame で評価し、それ以外の引数の扱いは同期経路と同じにする。
  - cooperative host function は、引数評価後の `start` 呼び出しと Pending 時の扱い（`may_yield` は
    トップレベルだけ＝e1）を同期経路の規約どおりに保つ。
  - yield 可能点: これらの call の引数境界（e2 の call 引数境界と同じ）。強制 yield フックをここへ広げる。
  - builtin 本体の実行（bulk work を含む）は yield しない（§4.2 の超過、chunk 境界 yield は
    `fuel-bulk-chunk-yield`）。
- **範囲外（後続 sub-slice）**: `map` / `filter` / `each`（e4）。
- **受入基準（e3 が満たす部分）**:
  - **TEC-AT-01（e3 分）**: 強制 yield フックで、builtin・host function・cooperative host function の引数に
    ユーザー関数呼び出しを含む script（`print(f(x))`、`push(xs, f(x))`、登録 host function の引数に呼び出し）
    を跨いで yield/resume した結果が同期経路と一致する。host-call triple を含む audit event 列（非
    `Yielded`/`Resumed`）も一致する。
  - **TEC-AT-02（e3 分）**: builtin の引数評価の途中で yield させ、部分評価済みの引数が保存されて resume 後に
    同期経路と同じ値・副作用列になる。
  - **TEC-AT-03（e3 分）**: 既存テスト無改変で緑（host function・cooperative・audit 系のテストを含む）。
- **検証**: 全 sub-slice 共通の完了条件。テストは `tests/engine_api.rs`（TEC-AT-01/02）と、host function・
  cooperative の組み合わせが要る場合は既存の配置（`tests/scheduler_api.rs` 等）に新規テストとして追加する。

##### tree-expr-continuation-e4 実装スライス（`map` / `filter` / `each` の段階実行）

- **範囲**:
  - poll 経路で、`map` / `filter` / `each` の要素ループを明示 frame の反復に載せる（上の小節
    「callback builtin の段階実行化」）。frame には callback 反復状態（処理済み要素・次の要素 index・部分結果）を
    保存する。callback 本体は Call frame として driver 上で実行し（e1 の機構）、戻り値の受け渡し先を
    「この反復 frame」とする。
  - これらの呼び出しが文レベル（e1 の 4 位置）・式の operand（e2）・他の builtin / host の引数（e3）の
    どこに現れても、driver 上の段階実行にする。
  - per-element 課金・callback の arity / 非 callable エラー・エラー伝播・結果の順序は同期経路
    （`call_fn_value` の現行実装）と一致させる。`sort` は書き換えない。
  - yield 可能点: 各要素の処理の境界（callback 呼び出しの前後）と callback 本体の文/反復境界。強制 yield
    フックをここへ広げる。
  - heap: 反復 frame は式 frame と同じく課金しない（`heap_charge` 0。e2a の判断 Q1 = A）。部分結果の
    collection は同期経路と同じ課金規律で作る（同期経路と terminal 時点の `live_heap` が一致する）。
- **受入基準（e4 が満たす部分。e4 の done で TEC-AT-01〜07 がすべて満たされる）**:
  - **TEC-AT-07**: 本文どおり。要素の途中・callback 本体の途中の両方で強制 yield を起こし、`map` / `filter` /
    `each` の結果・副作用順序・`committed.fuel` が同期経路と一致する。
  - **TEC-AT-01（全体）**: 強制 yield フックで、e1〜e4 の全 yield 可能点を 1 つの script の中で跨ぐ組み合わせ
    （例: 式の operand や builtin の引数に置いた `map`、`map` の callback の中の入れ子呼び出しとループ）を
    含めて、同期経路と一致する。
  - **TEC-AT-02（e4 分）**: callback 反復状態が yield を跨いで保存されることを unit test と resume 一致で固定する。
  - **TEC-AT-05（e4 分）**: callback 途中の yield を跨いでも submission journal が保持される。
  - **TEC-AT-06（全体）**: callback 本体にループを持つ script を最小 slice で実行し、1 poll の実行量が上限
    以内（e1 と同じ上限式）。再帰・ループ・callback が何段入れ子でも上限を超えない。
  - **TEC-AT-03（e4 分）**: 既存テスト無改変で緑。
- **検証**: 全 sub-slice 共通の完了条件。テストは `tests/engine_api.rs`（TEC-AT-01/02/05/06/07）、callback
  反復状態の不変条件は `src/builtin*.rs` の unit test。

### Slice 4: cancellation・pause・transaction

- `CancellationToken`、pause/resume、checkpointを実装
- mutation journalでAUD-024のrollback規則を実装
- terminal後resume不可とraceのlinearizationを固定

### Slice 5: scheduler・host pending

- bounded active/queue、FIFO round-robin、backpressureを実装済み（`SchedulerShared` + `AdmissionController` が単一 Mutex 下で active/queue slot を atomic 会計し、`create_execution`/`start` が handle 生成前に slot を予約・両満杯は即時 `StartError::Backpressure { active, queued, limit }` で handle を作らず context も変えない。queue slot を得た handle は `Yielded(AdmissionQueued { resume_to })` で semantic work を一切行わず、active が空くと FIFO 先頭を active へ atomic 変換し waker で通知する。Engine 全体の run-turn FIFO queue が先頭 handle の poll だけを 1 slice 進め、非先頭の Ready handle は semantic work なしで `Yielded(SchedulerPreempted)`（reason のみ、`self.state` 不変）を返す。Engine は worker thread を spawn しない）
- nonblocking host call ticket、wake、deadline/cancel伝播を実装済み（host adapter が `HostCallPoll<T> { Ready(Result<T, AdapterError>), Pending(HostCallTicket<T>) }` を返し、`Pending` は execution を `Yielded(HostCallPending { call_id })` にして ticket へ 1 個の `ExecutionWaker` を登録する。登録は「waker 保存 → ready flag 再確認 → ready なら即 wake」の lost-wake-safe 順序で、`ExecutionHandle::set_waker(Some(w))`/`set_waker(None)` が作成 thread から waker を置換・解除する。adapter executor が結果を ticket へ一度だけ格納して wake し、作成 thread の次回 poll が `try_take` する（wake は continuation を別 thread で実行しない）。`CancellationToken::register_waker` が cancel 時に登録 waker を wake し、queue 待ち・pause・host 待ちのいずれでも cancel/deadline の checkpoint が発火する。cancel/deadline terminal は `budget::control_stop_to_error` 経由で catch 不能化し、`CooperativeAdapterYieldInSyncContext` 違反は `ErrorKind::InternalControl`（`is_uncatchable`）の catch 不能 terminal になる。ticket drop/cancel は遅着 result を破棄する）
- adapter executorにも独立したconcurrency/queue上限を設定済み（`CooperativeAdapter<Request, Response>` を Phase-4 でだけ登録でき、`CapabilityCallContext`（`may_yield`・deadline・cancellation・残 request/response bytes・call_id）を介して同期 adapter と同じ capability/budget/deadline/cancellation を使う。Engine 本体と別の有限 thread pool `AdapterExecutor`（`AdapterExecutorLimits { max_threads, max_queued, max_concurrency }`）が結果を駆動し、queue 満杯は caller を block せず `SubmitError::QueueFull`＝cooperative start は `Ready(Err(..))` へ落として backpressure へ変換する。scheduler/context/audit lock を保持したまま adapter を呼ばない）

VM charge parity（Slice 6）はこの slice の対象外で VM は cooperative にせず Ready にフォールバックする（`vm.rs` 不変）。cancel で host call を `Cancelled`/`Detached` に閉じる構造は用意する。audit event の実発行と `AuditBackpressure` yield は本 Slice 5（engine-poll 経路）では据え置いたが、Phase 6 B-1 で **facade（`embedding.rs`）の監査付き poll driver** 側に配線した（sink の `AuditSubmit::Pending` を観測して `Yielded(AuditBackpressure)` へ遷移・ack で resume、永続 Pending→`AuditFailure::BackpressureDeadline`）。scheduler engine の poll が `YieldReason::AuditBackpressure` を返すのは E10 統合後のままで、sink を持たない engine-poll 経路は本 slice どおり変えない。詳細は [決定性・実行時監査仕様](determinism-and-audit.md) §10/§10.2/§14。`ExecutionHandle` は `!Send + !Sync` のままで、`Send + Sync` にするのは `SchedulerShared`/`AdmissionController`/run-turn queue/`ExecutionWaker`/`HostCallTicket`/`CancellationToken` だけ。既定/legacy config の同期経路（CLI/REPL/alpha facade・`embedding::Engine::run`）は観測挙動不変で、scheduler/admission/host-pending surface は新 API で opt-in。

### Slice 6: VM charge parity

- VMへ`Charge` opcodeと同じcontinuation/outcome契約を実装
- treeとのcharge trace、terminal boundaryをpaired testで一致させる
- 一致するまでVMはexperimentalであり、production schedulerへadmitしない

既知差の解消状況（順次 paired test で固定していく）:

- **f-string のリテラル部分の課金差（✅ 解消済み）**: VM は f-string のリテラル部分を
  課金しない `LoadConstRaw`（定数表からの非課金 load）で積み、tree と同じく結合後の最終
  body だけを `FStrConcat` が課金する。これで tree の「リテラルを `push_str` で結合し
  個別課金しない」会計と一致し、cumulative `StringAllocations`/`StringBytes` と live heap が
  両 engine で等しくなる。paired test は `tests/integration.rs` の
  `fstring_literal_parts_not_charged_in_both_engines`（リテラルの余分課金がないこと）と
  `fstring_final_body_is_charged_in_both_engines`（最終 body は両 engine で課金すること）。

- **空 collection リテラルの課金差（✅ 解消済み）**: VM の `x=[]` / `x={}` を materialize 時に
  body 課金し、tree と揃えた（詳細は第5節 collection per-drop の既知差リストを参照）。paired
  test は `tests/scaling.rs` の `empty_collection_literal_live_heap_matches_across_engines`。

- **`pop` の full-clone 差（✅ 解消済み）**: VM の `pop` を `OpCode::PopUpdate`（→
  `list_pop_tracked`）で in-place delta release 化し、tree の `budget_list_pop` と揃えた。

- **`push` の full-clone 差（✅ 解消済み）**: 以前は VM の `push` だけが
  `CallBuiltin(builtin_push)` 経路で、builtin が backing を full-clone した untracked List を
  返し、dispatch 境界の `track_result` が list_body(N+1) を丸ごと課金していた。push 済みの旧
  backing もまだ live なので、push のたびに新旧が一瞬同時に live になり `peak_heap` が
  `live_heap` を上回ってスパイクしていた（tree は `list_push_tracked` の in-place delta 課金で
  スパイクしない）。`pop` の `PopUpdate` と対称に、VM の `push` を対象 binding storage を直接
  書き換える in-place 命令 `OpCode::ListPushBinding`（`SetIndex` と同じ binding storage 経路で
  `list_push_tracked` を呼ぶ）へ切り替えて解消した。backing の `Rc` を operand stack へ複製
  しないため、一意所有では COW 複製なしの delta（+32 byte）だけを課金し、tree と `peak_heap`
  が一致する（共有 backing は tree と同じく `detach_list` が COW 複製する）。`builtin_push` は
  builtin registry 契約・エラーメッセージ parity のため残置する。paired test は
  `tests/scaling.rs` の `push_peak_heap_has_no_full_clone_spike_across_engines`（push 後に
  `peak_heap == live_heap` となりスパイクが消えたことを両 engine で固定）・
  `push_to_shared_backing_has_no_spike_across_engines`（共有 backing への push もスパイク
  しないこと）・`push_live_heap_is_linear_not_quadratic_across_engines`（live heap が O(N) に
  収まる回帰ゲート）。

- **REPL 持続 closure が retain する code の課金差（設計確定 A-1・実装は後続）**: closure を
  定義した入力の次の REPL 入力で、両 engine とも前入力の artifact 課金を丸ごと release する
  （tree=`ast_token = None`、VM=`chunk_tokens.clear()`）。しかし生き残った closure が握る code
  （tree=`FnDef.body` の `Rc<[Stmt]>`、VM=`VmFn.chunk` の prototype `Rc<Chunk>`）はメモリ上に
  残るのに**どちらの engine でも再課金されず**、両 engine が対称に under-charge する。release
  で落ちる論理サイズが engine ごとに違う（tree=AST body / VM=prototype chunk）ため、入力を
  またいだ `live_heap_bytes` / `peak_heap_bytes` が backend 間でズレる。この差は push/pop と
  違い「VM だけが誤り」ではなく「両 engine が retain を取りこぼす」非対称であり、§5.3 の
  **A-1「reachable な code は課金し続ける」モデル**（closure 寿命トークンで課金）で両 engine
  を揃えて閉じる。これにより reachable code の**課金の有無と release 寿命が backend 間で一致**
  する（従来の「次入力で落ちる」under-charge が両 engine から消える）。ただし課金額は engine
  ごとの artifact 単位（tree=`ast_node` / VM=`bytecode_chunk`）のままで、§5.3 の「AST または
  bytecode」どおり `live_heap` / `peak_heap` の数値そのものは engine 固有であってよい（byte
  単位の数値一致まで求める A-2 は採らない）。これにより [決定性・監査仕様](determinism-and-audit.md)
  §3 の「terminal 時 `BudgetUsage` の backend 一致」要件（課金の有無・寿命が揃うこと）および
  manifesto 原則 5（resource 消費の backend 差＝「片方だけ落ちる」非対称を許容しない）を満たす。
  差を仕様として受容する案（A-3）は採らない。**実装済み（PR-6b, commit `5be0ea2`）**で
  tree / VM 双方へ closure 寿命トークンを配線し、`tests/closure_retain.rs` の paired test で
  固定した（PR-6c で differential matrix `tests/differential.rs` へ集約し `closure_retain.rs` は
  harness へ載せ替えた）。実装詳細の課金規則・二重課金回避境界・release 寿命は §5.3
  「AST または bytecode」で定義する。
  closure retain 課金は PR-6b で実装済みであり、**Slice 6 の残作業は §15.5 charge/budget
  parity matrix の自動化のみ**である（下記 §15.5 の PR-6c 線引きを参照）。

- **fuel 課金差（✅ 解消済み：下記「F-1 実装スライス」）**: Slice 6 で揃えたのは heap・string・
  I-O の会計で、fuel は揃っていない（2026-10-20 判明）。tree はループ反復・user function
  呼び出し・callback 呼び出しごとに 1、VM は REV-006（[次期意味論・実装決定](semantic-decisions.md)
  §17.7）以降、全命令ごとに 1 を課金しており、同じ script が tree では成功して VM では step
  上限に達する（既定 1,000,000 で、2 文ループの最大反復数は tree 1,000,000 / VM 66,665、
  再帰 fib が通る最大 n は tree 27 / VM 22）。§15.5 の PR-6c 線引きは「fuel parity は StepLimit
  terminal の observable 一致で担保する」としていたが、differential が両 engine とも上限に
  達する script だけを比べていたため、この差を検出できなかった。§4.1 の課金表は tree・VM の
  どちらにもまだ実装されていない（Slice 1 注記の「fuel 全 charge point の展開は Slice 2」は、
  Slice 2 の項目に含まれないまま Slice 2 完了とされた）。

#### F-1 実装スライス（fuel parity：VM を tree の現行課金点へ、2 メーター化）

> 状態: **実装済み**（2026-10-20）。実装上の注記は本小節末尾を参照。§4.1 の課金表そのものの実装は後続の F-2（未定義）で扱う。

fuel を「論理量」と「停止性の安全網」の 2 つのメーターに分ける（[次期意味論・実装決定](semantic-decisions.md)
§17.7 の 2026-10-20 改訂）。F-1 は、VM の fuel を tree の**現行の**課金点に合わせて tree/VM の
fuel parity を回復し、停止性を fuel から独立した instruction watchdog へ移す。§4.1 の課金表
（statement / expression / operation 単位の課金）への移行は、両 engine の fuel が一致した状態から
F-2 で行う（F-2 は既定 `total_fuel` の見直しと `language_revision` の扱いを含むため別 slice とする）。

- **fuel の課金点（tree は変更しない）**: VM は次の 3 点でだけ fuel を 1 課金し、全命令での課金を
  やめる。いずれも tree の課金点と同じ順序・同じ line で課金する。
  1. **(F1) ループの back-edge**: `OpCode::Loop`。line は loop 文の line（compiler が `Loop` に
     付ける line）。tree の「本体 1 反復の完了後・次反復の判定前」に対応する。
  2. **(F2) user function 呼び出し**: `OpCode::PrepareCall`。line は呼び出し位置。深度検査より前に
     課金する（tree の `count_step` → 深度検査の順と同じ）。
  3. **(F3) builtin の callback 呼び出し**: `Vm::call_fn_value`。line は builtin 呼び出しの line。
     深度検査より前に課金する。

  VM は host function に対応しないため、tree の host / cooperative host 呼び出しの課金点に当たる
  ものはない。sort・比較・表示の per-element 課金は共有コードのため変更しない。
- **`Loop` を back-edge 専用にする**: 現在の compiler は `for` の `continue` を `Loop` で
  increment 位置（前方）へ飛ばしている。このままだと `for` で `continue` した反復が VM だけ 2 回
  課金されるため、`for` の `continue` は前方 `Jump`（`break` と同じ patch 方式）へ変える。`while` の
  `continue` は loop 先頭への後方 `Loop` のままでよい（1 反復につき back-edge は 1 回だけ実行される）。
- **instruction watchdog**: [§17.7](semantic-decisions.md) の (W1)(W2) で tick する VM 内部カウンタ。
  - fuel・`BudgetUsage`・audit・slice yield のどれにも現れない。
  - 上限は `total_fuel × 2 + 16`（saturating）。tick のたびに現在の `total_fuel` から計算する
    （`set_max_steps` や REPL の再設定に追従する）。
  - REPL では fuel と同じく submission ごとに 0 へ戻す。callback の入れ子 `run_frames` を含め、
    1 execution（REPL では 1 submission）で 1 本のカウンタを共有する。
  - 上限に達したら、fuel の上限到達と同じ terminal（kind `limit`、`ステップ上限に達しました
    (上限: {total_fuel})`、catch 不可）へ写す。
- **観測される変化（VM のみ）**: fuel の消費量が tree と同じになり、VM で step 上限に達していた
  script の一部が成功するようになる（制限の緩和）。step 上限エラーの line と trace が tree と
  一致する（例: 呼び出しで上限に達したとき、VM も呼び出し位置の line で止まり、callee の trace は
  付かない）。tree・既定経路の観測挙動は変えない。`language_revision` は bump しない
  （language-spec は既に「tree と `--vm` は同じ観測挙動」と定めており、F-1 はその非適合の修正）。
- **変更箇所**:
  - `src/vm.rs`: メインループの全命令 `count_step` を削除し、(F1)〜(F3) で `count_step` する。
    watchdog（counter・上限計算・(W1) の ip 比較・(W2) を数える frame push の一本化）を追加し、
    REPL submission の境界で fuel と一緒に reset する。
  - `src/compiler.rs`: `for` の `continue` を前方 `Jump` に変える。
  - `src/verifier.rs`: 変更なし（`Loop` の target 範囲検査は従来どおり）。
  - tests: 下記。
  - docs: §17.7（改訂済み）、本小節、§15.5 の PR-6c 注記、[決定性・監査仕様](determinism-and-audit.md)
    §15.5 の同注記、roadmap。
- **受入基準（F-1）**:
  - **F1-AT-01**（fuel 一致・ループ）: `while`・`for`・入れ子ループ・`while`+`continue`・
    `for`+`continue`・`break` で、両 engine の `committed.fuel` が完全一致する（differential の
    in-process 層で exact 比較）。
  - **F1-AT-02**（fuel 一致・呼び出し）: 再帰・相互再帰・closure 呼び出し・`map`/`filter`/`sort`
    の callback で、`committed.fuel` が完全一致する。
  - **F1-AT-03**（上限の境界）: F1-AT-01/02 の各 script について、上限 N = 必要 fuel では両 engine
    とも成功し、N − 1 では両 engine とも step 上限で失敗する。stdout・stderr（line・trace を含む）・
    exit code が一致する。
  - **F1-AT-04**（既定上限での一致）: 既定 1,000,000 で、2 文ループ 1,000,000 反復が両 engine で
    成功し、1,000,001 反復が両 engine で失敗する。fib(27) が両 engine で成功する。
  - **F1-AT-05**（呼び出しの 1 課金）: 上限 1 で `once()` を 1 回呼ぶ script が両 engine で成功し、
    本体がちょうど 1 回実行される（REV-006 で弱めた `call_budget_is_checked_before_callee_in_both_engines`
    の「call = ちょうど 1 step」検査を復元する）。
  - **F1-AT-06**（watchdog による停止性）: `tests/defensive_vm.rs` の `Jump(0)` 自己ループ・後方 `Loop`
    迂回・`PrepareCall` なし raw `Call` 再帰が、従来どおり `limit` で有限停止する。
  - **F1-AT-07**（watchdog の非干渉）: F1-AT-01〜04 の全 script と既存の全 fixture で、watchdog の
    上限に達する前に必ず fuel の上限に達する（テスト用に watchdog の tick 数を観測する内部 API で、
    「tick 数 ≤ fuel 課金数 + root frame 数」を確認する）。
  - **F1-AT-08**（REPL）: VM REPL で submission ごとに fuel と watchdog が reset され、
    tree REPL と同じ入力列で同じ出力になる。
  - **F1-AT-09**（catch 不可）: fuel・watchdog のどちらの上限到達も try/catch で捕捉できない
    （両 engine）。
  - 既存テストのうち、VM の per-instruction 課金を前提にした期待値（例: `call_budget_is_checked_before_callee_in_both_engines`
    の弱めた検査、differential の「向き・符号」比較）は、F-1 の exact parity へ置き換える。
- **テスト配置**: 境界・CLI は `tests/integration.rs`、exact fuel 比較は `tests/differential.rs`、
  停止性は `tests/defensive_vm.rs`、watchdog の内部不変条件は `src/vm.rs` の unit test。
- **F-1 の範囲外**: §4.1 の課金表（F-2）、VM の deadline clock 注入、VM の audit、VM の
  cooperative 実行（いずれも既存の後続項目）。
- **実装上の注記（2026-10-20）**:
  - **消費 fuel の突合方法**: tree は成功 path で `committed.fuel` を 0 として snapshot する
    （入力ごとに reset）ため、F1-AT-01/02 は usage の直接比較ではなく「成功する最小の fuel 上限 N」
    を二分探索で求めて exact 突合する（`tests/differential.rs` の `assert_fuel_parity`）。同じ関数で
    N−1 の terminal 全 field（F1-AT-03）と watchdog の tick 数 ≤ N + 1（F1-AT-07）も検査する。
  - **`while` の `continue` の line**: `continue` が出す back-edge `Loop` には、`continue` 文ではなく
    ループ文の line を付ける（`LoopState.line`）。tree はループ文の line で課金するため、付けないと
    `continue` 直後に上限へ達したときの line が tree/VM で食い違う（F1-AT-03 で検出）。
  - **保留中の heap 超過**: 改訂前の全命令 `count_step` は、rollback journal の heap 超過
    （`pending_heap_stop`）を次の命令で表に出す役目も兼ねていた。fuel の課金点を減らしてもこの
    検査は全命令で残す（`surface_pending_heap_stop`）。
  - **watchdog**: `Vm::watchdog_ticks()` を診断・テスト用に公開した（`unstable-bytecode` の `vm`
    module の一部）。frame を積む経路は `push_call_frame` に一本化した（`Call` と callback。REPL の
    root frame は submission 開始時に watchdog と一緒に reset するため数えない）。
  - **テスト配置**: F1-AT-01/02/03/07 は `tests/differential.rs`、F1-AT-04/05/08/09 は
    `tests/integration.rs`（F1-AT-05 は `call_budget_is_checked_before_callee_in_both_engines` を
    「呼び出し 1 回＝ちょうど 1 fuel」へ復元）、F1-AT-06/09（watchdog 側）は `tests/defensive_vm.rs`。

#### F-2 実装スライス（§4.1 課金表への移行：主要 charge point・既定 total_fuel 見直し・fuel 会計 revision）

> 状態: **未実装**（設計のみ）。本小節は §4.1 の課金表（既に規範として定義済み）の主要 charge point
> へ fuel 課金を揃える縦切り（F-2）の slice 定義である。§4.1〜4.2 の課金表・fuel 意味論が正本で
> あり、以下は F-2 で実装する範囲・観測挙動の変化・意図的に延期する逸脱の記録である。進捗の唯一の
> 正本は [`slices.toml`](slices.toml) と [ロードマップ](roadmap.md)。
>
> **依存（前段スライス）**: F-2 は、slice の yield を式の途中でも行える tree evaluator の continuation
> 拡張（実装スライス `tree-expr-continuation`）が **done** であることを前提にする。理由は本小節
> 「式の途中での slice yield は前段スライスで解決済みとする」を参照。`tree-expr-continuation` は
> F-2 とは独立に先行できるため、別スライスへ切り出して `depends_on` にした。

F-1 は VM の fuel を tree の**現行の**課金点（ループ back-edge 1・user function 呼び出し 1・builtin
callback 1）へ揃えて fuel parity を回復し、停止性を instruction watchdog（§17.7）へ分離した。ただし
F-1 の課金点と課金値は §4.1 の課金表そのものとは一致していない（F-1 小節「F-1 の範囲外」に明記）。
F-2 は、両 engine の fuel が一致した F-1 の状態を起点に、§4.1 の表のうち**主要 charge point**
（statement / expression / operation / function / loop / host）の課金点と課金値を §4.1 どおりへ移行する。

§4.1 の主要 charge point は F-1 の課金粒度より細かく、同じ script の消費 fuel が増える。したがって
F-2 は既定 `total_fuel` の見直しと、課金表の版である `fuel_accounting_revision`（§3.1）の bump を
含む（観測挙動が変わる）。これらは設計判断を要するため F-1 とは別 slice とする（F-1 小節で予告済み）。

- **課金点を §4.1 の主要 charge point どおりへ張り直す（tree/VM 両方）**: 下記の charge point を
  両 engine で同一の順序・同一の line で課金する。VM は §4.1 の要求どおり compiler が statement /
  expression / operation / function / loop の境界へ `Charge` opcode を挿入し、tree evaluator は
  同じ共有 `FuelSchedule` API を呼ぶ。対応は次のとおり。
  - **statement へ入る = 1**: 到達した statement の実行開始時。到達しない statement は課金しない。
  - **expression へ入る = 1**: literal・変数参照・call 式を含む各 expression の評価開始時。
  - **unary / binary / compare / index / assignment の論理 operation = 1**: operand の expression
    charge とは別に、その operation 1 個につき 1。
  - **language-level function invocation = 5**: user function・lambda・core builtin・callback で
    共通。F-1 の「呼び出し 1 課金」を 5 へ置き換える。line は呼び出し位置（F-1 と同じ）。
  - **loop iteration 開始 = 2**: condition・iterable・body の charge とは別に、1 反復につき 2。
    F-1 の back-edge 1 課金を、§4.1 の「iteration 開始 2」へ置き換える（課金のタイミングと値の
    両方が変わる。line はループ文の line で F-1 と同じ）。
  - **host call attempt = 10**: 許可・拒否・host error のいずれでも課金。tree の host-call 境界
    （filesystem builtin）にだけ現れる。VM は host function に対応しないため該当なし（F-1 と同じ）。
  - **VM `Charge` dispatch = 0 / その他 VM 内部 opcode = 0**: lowering 差を script の fuel 差に
    しないため、§4.1 どおり課金しない。compiler optimization で charge point を削除・併合しない。

- **既に §4.1 どおりで F-2 が変えない per-element 課金（再掲のみ）**: collection element の
  per-element fuel 課金（1 / element）が現状あるのは **比較・表示・`sort`** の経路だけである
  （`src/builtin_core.rs` の sort 走査課金＝`render_display` の per-node fuel 代替、`src/value.rs`
  の比較走査、`src/budget.rs` の表示課金）。これらは F-1 以前から fuel メーターへ課金しており §4.1
  と一致するため、F-2 は値も課金点も変えない。**`map` / `filter` / serialize の per-element 課金は
  §4.1 の規範だが未実装**（現状は callback 呼び出しごとの課金しかない）であり、UTF-8 payload・AST
  node と同じく後続スライス `fuel-bulk-charge` へ延期する（下記「逸脱」）。bulk work の 256 element
  / 16 KiB chunk 分割（§4.1）は既存経路でそのまま維持する。

- **変更箇所**（F-1 小節と同じ粒度で列挙）:
  - `src/budget.rs`: `BudgetConfig` に `fuel_accounting_revision: u32` を追加（既定 `1` → F-2 で
    `2`）。受理版の検査を `heap_accounting_revision` と同じ入口（`BudgetConfig::validate` ／
    execution 作成前）へ足す。現行の `ConfigError::UnsupportedAccountingRevision(u32)` は
    `heap_accounting_revision` 専用で、fuel と共用すると**どちらの版が不正か区別できない**。F-2 は
    **どの会計 field が不正かを区別できる形**にする（`UnsupportedHeapAccountingRevision(u32)` /
    `UnsupportedFuelAccountingRevision(u32)` へ分ける、または種別 enum を持たせる）。既存の
    `UnsupportedAccountingRevision` を参照する箇所（`src/embedding.rs` の doc・`src/budget.rs`
    `validate`・両者の unit test）を新形へ更新する。課金表の charge point / 課金値を一元提供する
    共有 API（仮称 `FuelSchedule`：statement=1 / expression=1 / operation=1 / invocation=5 /
    loop iteration=2 / host call=10 の定数と課金ヘルパ）を新設する。
  - `src/audit.rs` と [決定性・監査仕様](determinism-and-audit.md): `AuditEvent::ExecutionStarted`
    は `heap_accounting_revision: u32` と `budget: BudgetConfig` を持つ。`BudgetConfig` に
    `fuel_accounting_revision` を足すと監査 event の内容が変わるため、`ExecutionStarted` の field
    定義を更新する。**`heap_accounting_revision` と対称に、`ExecutionStarted` へ独立 field
    `fuel_accounting_revision: u32` を追加する**（`budget` の中だけで持たせる案は採らない。会計版は
    監査の第一級の属性で、heap と対称に top-level で引ける方が replay/検証時に扱いやすいため）。
    R-1a/B-2 と同じく opt-in・tree-only の監査経路のみに影響し、CBOR tag 表・charge table は変えない。
    determinism-and-audit.md の §7.2 `ExecutionStarted` の記述も `heap_accounting_revision` の隣へ
    `fuel_accounting_revision` を並べて更新する。
  - `src/compiler.rs`: statement / expression / operation / function / loop の各境界へ fuel 課金を
    encode する `Charge` opcode を挿入する。既存の最適化で charge point を削除・併合しない。
  - `src/opcode.rs` / `src/chunk.rs`: `Charge` opcode（§4.1 の「`Charge` opcode dispatch = 0、payload に
    論理 charge を encode」）を追加する。
  - `src/verifier.rs`: `Charge` payload（非負・既知の charge 種別）の検証を追加する。
  - `src/vm.rs`: F-1 で `OpCode::Loop`（back-edge）・`PrepareCall`・`call_fn_value` に置いた現行の
    `count_step` を外し、`Charge` opcode の dispatch で `FuelSchedule` を呼ぶ。instruction watchdog
    （§17.7、停止性）は fuel とは独立なので tick 機構はそのまま残す。
  - `src/eval.rs`（tree）: 各境界で `FuelSchedule` の課金ヘルパを呼ぶ。F-1 の `count_step` 呼び出し
    （5 か所）を新 API へ置き換える。式途中の yield は前段スライス `tree-expr-continuation` で
    入った continuation 機構に乗せる（F-2 は課金点を足すだけで continuation 自体は作らない）。
  - `unstable-bytecode` feature の公開面: `Charge` opcode を公開列挙へ追加するなど、既存の封印
    方針（REV-006）に沿って更新する。
  - docs: 本小節、§3.1（`fuel_accounting_revision` 追記済み）、roadmap。

- **既定 `total_fuel` の見直し（測定して決める）**: 課金が細かくなり、同じ script の消費 fuel が
  F-1 比で数倍〜十数倍に増える。現在の既定 1,000,000 fuel（§3.1）のままでは、F-1 まで通っていた
  代表 script の多くが上限に達する。F-2 は既定値を、代表 script 群が F-1 までと同程度に通る水準へ
  引き上げる。**具体値は実装時にベンチ（`benches/interpreter.rs` の workload と下記「代表 script
  集合」）で新課金の消費量を測って確定する**。設計では「旧挙動の代表 script が通る最小の切りの良い
  値を測定で決める」ことと測定対象（下記の固定集合）だけを固定し、本小節に数値を書かない（数値を
  先に決め打ちすると、実測とずれたとき spec/実装が drift する）。
  - **代表 script 集合（設計時点で固定・測定対象）**: `examples/*.tsg`（`hello` / `math` /
    `import_demo`）・`benches/interpreter.rs` の各 workload・F1-AT-04 相当の「2 文ループを既定上限
    近くまで回す script」と `fib(27)`。この集合が新課金・新既定 `total_fuel` で両 engine とも成功
    することを確認し、既定値決定の根拠（各 script の新消費 fuel）を実装 PR 本文へ記録する。
  - **根拠**: §4.2 は `total_fuel` を「terminal までの上限」と定め、具体値は config 事項として
    §3.1 の既定表に置く設計になっている。F-2 は課金粒度の変更に合わせて既定表の 1 値を更新する
    変更であり、新しい意味論を導入しない。

- **`fuel_accounting_revision` を 1 → 2 へ bump する（`language_revision` は触らない）**: F-2 は
  §4.1 の課金値へ移行する会計の変更であり、§3.1 で新設した `fuel_accounting_revision` を `1` から
  `2` へ上げ、受理版を `2` だけへ更新する。**旧版（`1`）の旧課金を再現する互換経路は持たない**
  （`fuel_accounting_revision = 1` は `ConfigError::UnsupportedAccountingRevision` で拒否されるので
  あって、旧課金で動くのではない）。`heap_accounting_revision` が §5.1 の論理サイズ表の版である
  のと完全に対称で、課金表を変えるたびにこの値だけを上げる。
  - **`language_revision` を使わない理由**: `language_revision`（REV-017 §17.8）は sort / `list_dir`
    の全順序化のように **script から観測できる言語挙動**の版であり、その doc は「0.20 以下は従来
    整列順を温存」と revision ごとの挙動温存を約束している。fuel 課金は言語挙動ではなく**資源会計**
    であり、§3.1 で既に `heap_accounting_revision` が会計版を言語 revision と分離する前例を確立して
    いる。fuel 課金の変更を `language_revision` bump に乗せると、(a) revision ごとに挙動を温存する
    約束と「旧 fuel 課金は温存しない」方針が食い違い、(b) 言語挙動と会計という別種の変更が 1 つの
    版番号に混ざる。そこで F-2 は `language_revision` を **bump しない**。この方針（fuel は会計
    revision で管理し言語 revision で分岐しない）は §3.1 の `fuel_accounting_revision` 定義にも
    明記した。
  - **根拠**: §3.1 の `heap_accounting_revision` が「会計表の版を専用 field で持ち、受理しない版を
    execution 作成前に拒否する」型を確立済み。F-2 はその型に fuel 用の版 field を 1 つ対称に足す
    だけで、新しい revision 機構を導入しない。旧版を温存しない判断は alpha の後方互換無保証
    （[ロードマップ](roadmap.md)・README の alpha 注記）に基づく。

- **式の途中での slice yield は前段スライスで解決済みとする**: 課金点が expression / operation 単位
  になると、式の評価の途中で slice fuel を使い切りうる。§4.2 は「次の論理 charge が slice 残量を
  超える場合は charge せず `Yielded(SliceFuelExhausted)` を返す」と定めるため、式の途中でも yield /
  resume できる必要がある。現行 tree evaluator は文・反復の粒度でしか continuation を持たず（式は
  `eval_expr` の Rust 再帰）、§9.3 が求める「保留中 operand を保存し Rust call stack を continuation
  にしない」を式へ適用できていない。この拡張は F-2 の課金点張り直しとは独立に先行でき、かつ重いので、
  **前段スライス `tree-expr-continuation` として切り出し、F-2 の `depends_on` に置く**（本小節冒頭の
  依存注記）。F-2 は `tree-expr-continuation` が done である前提で、新しい charge point を既存の
  continuation 機構に乗せるだけとし、continuation 自体は設計・実装しない。

- **観測される変化**: 同じ script の `committed.fuel` が §4.1 の主要 charge point どおりに増える。
  F-1 まで既定上限で通っていた script の一部が、既定 `total_fuel` を引き上げてもなお新課金で上限に
  達しうる（上限到達 script の集合と、上限到達時の line が変わる）。tree・VM の両方で同じ値・同じ
  成否・同じ line になる（F-1 の exact parity を維持する）。watchdog（停止性の安全網、§17.7）は
  fuel とは独立なので F-2 で意味論を変えない。ただし watchdog の上限 `total_fuel × 2 + 16` は既定
  `total_fuel` に追従するため、既定値を大きく上げると VM が暴走を止めるまでに許す instruction 数も
  同じ倍率で増える（停止はするが到達までが延びる）。これは資源会計の精度を上げるための許容範囲と
  する（停止性自体は保たれる）。

- **逸脱（F-2 時点で残るもの、後続スライスへ延期）**:
  - **UTF-8 payload（`ceil(bytes/64)`）と AST node（1 / node、compile・import link 中）の fuel
    課金**は F-2 の範囲外とし、後続スライス（`fuel-bulk-charge` 仮称）へ延期する。現状これらは
    fuel メーターへ課金されておらず（`src/budget.rs` 冒頭の注記「fuel の全 charge point 展開…は
    Slice 2 の残りサブスライス」）、新規に課金点を足す作業になるため、主要 charge point の移行とは
    別スライスに分ける。延期中も §4.1 の表はこれらを規範として残す（実装が表に追いつく形）。
  - **VM の deadline clock 注入・VM audit・VM cooperative 実行**は F-1 と同じく範囲外（既存の後続
    項目）。
  - **heap / string / count の課金**（§5）は F-2 の対象外。F-2 は fuel メーターだけを扱う。
  - **`PollSlice::max_fuel`（slice fuel）の既定値**（§4.2 の公平性の量子）は F-2 で変えない。
    F-2 が見直すのは `total_fuel`（terminal までの上限）だけ。

- **受入基準（F-2）**:
  - **F2-AT-01（主要 charge point が §4.1 どおり・tree/VM 一致）**: statement・expression・各論理
    operation・function invocation（5）・loop iteration（2）を含む代表 script 群で、両 engine の
    必要 fuel が §4.1 の表から手計算した期待値と一致する。tree は成功時に `committed.fuel` を 0 で
    snapshot する（入力ごとに reset）ため、usage の直接比較ではなく **F-1 と同じく「成功する最小の
    fuel 上限 N を二分探索で求めて exact 突合」**する（`tests/differential.rs` の `assert_fuel_parity`
    を新課金値へ更新）。**host call（10）は VM が host function に対応しないため tree/VM 一致検査に
    含めない**（下記 F2-AT-01b で tree 単独で検査する）。
  - **F2-AT-01b（host call = 10・tree 単独）**: filesystem builtin（`read_file` / `read_lines` /
    `write_file` / `append_file`）の host call 1 回につき 10 fuel 課金される。**builtin を呼ぶ文を
    1 つだけ増やした** 2 script（例: 同じ `read_file(path)` 文が 1 行 vs 2 行。path 引数は同一リテラル）
    の必要 fuel N の差を取る。増やした 1 文の内訳は host call 10 + builtin invocation 5 + その文の
    statement 1 + call 式の expression 1 + 引数リテラルの expression 1 = **18**（path 引数が変数参照
    1 つの単純形の場合。実際の期待値は選んだ script の式構造から §4.1 の表で手計算し、テストに定数で
    埋める）。この差のうち host call 由来が 10 であることを、invocation のみの差分（F2-AT-02）と
    突き合わせて確認する（差 − invocation 5 − statement/expression 分 = 10）。許可（Allow）・
    拒否（Denied）・host error の **3 経路それぞれ**で host call 10 が課金されることを別々に検査する
    （§4.1「許可・拒否・host error のいずれでも課金」。Denied は capability 未付与、host error は
    存在しないパス等で起こす）。tree 単独で、VM は対象外。
  - **F2-AT-02（function invocation = 5 の差分検査）**: 本体が同一で**呼び出し回数だけが 1 回と 2 回で
    異なる 2 本の script**について、成功する最小の fuel 上限 N の差がちょうど 5 になる（§4.1 の
    invocation = 5）。両 engine で一致する。1 回呼ぶ script の絶対 N を上限に使う検査（旧 F2-AT-02 の
    「上限 5 で成功」）は、statement / 変数参照 / 本体の課金が別途乗り成立しないため採らない。
  - **F2-AT-03（loop iteration = 2 の差分検査）**: body が同一で**反復回数だけが異なる**2 本の script
    について、成功する最小の fuel 上限 N の差が「反復回数差 × (2 + 1 反復分の condition / iterable
    評価の charge + body 1 反復分の charge)」になり、うち iteration 開始分が反復ごとに 2 である
    （`while` は condition の expression / operation charge、`for` は iterable 要素取り出しの charge が
    反復ごとに別途乗る）。`while` / `for` / 入れ子 / `continue` / `break` で両 engine の必要 fuel が
    一致する（F-1 の `continue` 二重課金回避を維持）。
  - **F2-AT-04（境界）**: F2-AT-01〜03 の各 script で、上限 N = 必要 fuel では両 engine とも成功、
    N − 1 では両 engine とも上限で失敗し、stdout・stderr（line・trace を含む）・exit code が一致する。
  - **F2-AT-05（新既定 total_fuel）**: 更新後の既定 `total_fuel` で、上記「代表 script 集合」が
    両 engine で成功する。既定値の決定根拠（各 script の新消費 fuel）が実装 PR 本文に記録される。
  - **F2-AT-06（fuel 会計 revision = 2）**: `BudgetConfig::standard` の `fuel_accounting_revision`
    が `2`。`2` を指定した実行と既定の実行が同じ fuel・同じ成否になる。`fuel_accounting_revision = 1`
    （旧版）を指定すると execution 作成前に `ConfigError::UnsupportedAccountingRevision` で拒否される
    （旧課金で動くのではない）。`language_revision` は F-2 前後で変わらない。
  - **F2-AT-07（watchdog 非干渉）**: 課金粒度を細かくしても、`tests/defensive_vm.rs` の停止性ケース
    （`Jump(0)` 自己ループ・後方 `Loop` 迂回・raw `Call` 再帰）が従来どおり `limit` で有限停止する。
    新課金でも watchdog 上限に達する前に fuel 上限へ達する（F1-AT-07 の不変条件を新課金で再確認）。
  - **F2-AT-08（既存テストの更新）**: VM/tree の fuel 期待値を持つ既存テスト（**F1-AT-04/05 を含む**
    F1-AT-* の N 値・differential の fuel 比較・integration の境界）を §4.1 の新課金値へ F-2 で
    再定義し、両 engine で緑。特に F1-AT-04（既定上限での 2 文ループ境界・fib(27)）と F1-AT-05
    （呼び出し = ちょうど 1 fuel）は新既定 `total_fuel`・invocation = 5 に合わせて数値を更新する。
    期待値が説明できない形で変わる場合は実装を止めて報告する。

- **テスト配置**: exact fuel 比較（F2-AT-01/02/03）は `tests/differential.rs`、host call tree 単独
  （F2-AT-01b）と境界・既定上限・revision（F2-AT-04/05/06）は `tests/integration.rs`、watchdog 非干渉
  （F2-AT-07）は `tests/defensive_vm.rs`、課金点挿入・`Charge` payload・`FuelSchedule` 定数・
  `ConfigError` の会計 field 区別の単体不変条件は `src/vm.rs` / `src/compiler.rs` / `src/budget.rs`
  の unit test。（F2-AT-01b は F2-AT-01 の host call 部分を tree 単独へ切り出した補助基準で、
  `slices.toml` の `acceptance` に明示する。）

- **スライス分割の指針**: F-2 は「主要 charge point の張り直し（実装が重い）」と「既定 `total_fuel`
  見直し ＋ `fuel_accounting_revision` bump」を 1 PR に載せる前提で設計した。`tree-expr-continuation`
  （式途中 yield）と `fuel-bulk-charge`（UTF-8・AST node 課金）は既に別スライスへ切り出してあるため、
  F-2 本体は主要 charge point の移行に絞られている。それでも実装着手時に 1 PR が過大と判断した場合は
  更に分割してよいが、`fuel_accounting_revision` の bump と既定 `total_fuel` の更新は**同一 PR で
  行う**（会計版と既定値を別 PR に割ると、中間状態で「新課金なのに旧既定上限」で代表 script が落ちる
  不整合が main に出るため）。分割する場合は `slices.toml` を更新し、本小節を割る。

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

> **PR-6c 線引き（REV-015 Slice 6）**: PR-6c-min の射程は、同一 fixture を tree/VM で走らせて
> **budget usage（engine 間不変な軸）+ terminal reason（`TsumugiError` 全 field）+ stdout/stderr の
> tree/VM parity** を突合する differential matrix（`tests/differential.rs`）までとする。以下は defer:
> - **(1) execution-trace determinism**（yield 位置 / host-effect 順序ログ / audit payload /
>   charge 系列 trace の完全一致）→ Phase 5/6 の differential gate。基盤（`DeterminismInput` /
>   `normalize_for_conformance` / effect log 比較 / charge trace 記録 API）が未実装のため Slice 6
>   単独では成立しない。
> - **(2) pause/resume・cancel・host pending**（上記 2 番目の箇条書き）→ **VM 対象外**。VM は
>   scheduler 非経由（Ready フォールバック・`vm.rs` 不変、本節 Slice 5 本文）で該当実行経路を
>   持たず differential が原理的に成立しない。tree 側は `tests/scheduler_api.rs` で担保済み。
> - **(3) fuzz/stress/生成 matrix**（上記 3 番目の箇条書き）→ AUD-022 / REV-024 / Phase 7
>   （[検証・リリース・運用設計](verification-release-operations.md) Slice 3）。
>
> **比較は軸別ポリシーを用いる**: engine 間不変な軸（I/O counter・string/source/import counts・
> per-item peak・terminal reason の kind/message）は exact 一致を要求し、**fuel count と absolute
> heap（`live_heap`/`peak_heap`）は向き・符号・release 寿命の一致**で突合する（byte-exact 一致は
> §5.3 の A-1 どおり backend 間で成立しないため要求しない。fuel 枯渇の terminal `line` も step
> 粒度差で engine 間に差があり、fuel parity は StepLimit terminal の observable 一致で担保する）。
> **（2026-10-20 注記）** この線引きは不十分だった: differential が両 engine とも上限に達する
> script だけを比べていたため、「tree では成功し VM では step 上限」という observable 差を検出
> できなかった。fuel は §14「F-1 実装スライス」で tree の課金点に揃え、`committed.fuel` の
> exact 一致と上限 N−1/N の境界一致で突合する。
>
> **PR-6c-min 完了 = Slice 6 charge-parity-matrix 自動化の完了であって、VM の experimental 卒業
> ではない。** 卒業 gate は determinism/audit conformance（§16・[決定性・監査仕様](determinism-and-audit.md)
> Slice 6）であり後続フェーズの管轄である。

## 16. 完了条件

Phase 3は、全`BudgetConfig` fieldが全生成・I/O・host経路へ適用され、超過がcatch不能terminalとなり、logical heapとdeadline/cancelを含む境界テストが通った時点で完了とする。

Phase 4は、treeの全制御状態がcontinuationへ保存され、slice実行、yield、pause/resume、bounded admission、FIFO fairness、nonblocking host call、backpressure、race testが通った時点で完了とする。VMは[決定性・監査仕様](determinism-and-audit.md)の適合gateを通るまでexperimentalのままとする。
