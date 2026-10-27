# Tsumugi 決定性・実行時監査仕様

最終更新: 2026-10-27

設計ステータス: **到達目標の確定仕様（§1〜§13）＋ 実装の現在地（下記の逸脱一覧）**

本文書の §1〜§13 は **到達目標の確定仕様**であり、現在の実装状態ではない。現在の実装が目標仕様からどう
逸脱しているか・何が未実装かは、下の **逸脱一覧** と **未実装範囲** の 2 表が唯一の正本である（状態の正本は
この 2 表に置き、§14 のスライス小節は再掲しない）。進捗の唯一の正本は [ロードマップ](roadmap.md)。

2 表は現在の逸脱と未実装範囲だけを持つ。done にする実装 PR は、この 2 表に新しい逸脱・未実装範囲を追加し、
解消した行を削除する（どのスライスで解消したかはスライス小節と git 履歴に任せる。[ドキュメント運用ガイド](documentation-guide.md)
「逸脱一覧の持ち方」）。

## 逸脱一覧（目標仕様 §1〜§13 に対する現在の逸脱）

| 逸脱内容 | 対象の節(§) | 発生スライス |
|---|---|---|
| 監査は `EngineBuilder::audit_sink` 設定時だけ有効（opt-in）。sink 未設定時は event を emission せず既存挙動と bit-identical。§1.1 の sink 必須（`ConfigError::AuditSinkRequired`）は未適用 | §1.1 | A-1 |
| Terminal 配送失敗後に完了済み script の language-state を rollback しない（commit が Terminal 配送より前に確定するため）。host は `AuditFailed` を受けた context を再利用してはならない | §10.1 | A-1 |
| envelope の `timestamp.unix_nanoseconds` に wall/unix `AuditClock` でなく request の monotonic tick を流用（順序の正本は `sequence` なので影響限定） | §7.1 | A-1 |
| host-call triple の §8 規則5/6 の timing（allow ack 前に外部効果を止める／`HostCallFinished` ack 前に response を露出させない）を満たさない。外部効果は `dispatch_fs` 内で同期完結し、triple は評価器 buffer から後で drain する。満たすのは triple の論理順序（規則3/4） | §8/§11 | A-1 |
| §11 の host call 開始前 atomic reserve を pre-effect reservation でなく drain 時の容量チェックで代用。外部効果自体を reserve 失敗で止める faithful 実装は未了 | §11 | A-1 |
| `AdmissionQueued` / `SchedulerPreempted` / `HostPaused` / `HostCallPending` の yield は engine-poll 経路（`engine.rs::poll`、sink 無し）でのみ生じるため、写像は定義するが監査付き経路では emission しない。sink-bearing な scheduler/pause/cooperative host-call 経路の配線が未了 | §7.2/§8 | A-1 |
| audit triple / yield/resume の emission が tree-only。VM への配線は未了（監査 sink と `VmExperimental` の同時設定は build 時に `ConfigError::AuditSinkWithExperimentalBackend` で拒否。既知の受容済み tree/VM 差分、Slice 6 で解消予定） | §2.1/§7.1 | A-1 |
| §10 の「次回 `ExecutionHandle::poll` が `try_take` して再開」を、facade 内の単一 execution poll driver（`run_audited_driver`）が sink round-trip を担う形で代用。scheduler engine（`engine.rs::poll`）は sink を持たず `YieldReason::AuditBackpressure` を返さない（E10 統合後） | §10/§10.2 | B-1 |
| audit drain queue byte 上限の判定がトップレベル境界だけの soft limit。関数・callback 本体の中では効かず、queue は上限を超えて溜まり得る（超過量は host-call 回数 budget で有界） | §10.2 | B-2 |
| host-call triple を emission するのは fs builtin のうち I/O byte 課金対象の 4 つ（`read_file` / `read_lines` / `write_file` / `append_file`、`src/builtin_core.rs::is_host_call_builtin`）だけ。`rename` / `mkdir` / `remove` 系 / `list_dir` / `file_size` / `is_file` / `is_dir` / `path_exists` など capability 判定を通る他の fs builtin は監査対象外（「fs builtin 監査範囲」スライスへ延期） | §7.2/§8 | R-1a |
| **（セキュリティ）** 既定 `hmac_key` が crate 内の固定値（`src/audit.rs` の `DEFAULT_HMAC_KEY`）で秘匿性がない。raw path を直接載せないことだけを担保し、key が公開されているため digest から候補 path を総当たりで推測・復元できる（§9.1 の辞書攻撃耐性は得られない）。秘匿性のある key を host から渡す経路は sink 必須化スライスへ延期 | §9.1 | R-1a |
| host-call lifecycle event（および `Yielded` / `Resumed` / `BudgetCharged`）の byte 見積もりが payload 非依存の 128 固定（`estimate_event_bytes` の `_ => 128`）。上限保証ではなく、HMAC digest や Identifier、Reveal された body（最大 `max_revealed_body_bytes`）を含む実 encode 長を過小評価し得るため、journal の byte 予算・B-2 の queue 会計・§11 の判定は概算依存（strict CBOR スライスへ延期） | §9.2/§11 | R-1a |
| `AuditField` の `original_bytes` を §9.1 の canonical subtree 長（§9.2 の encoding で測る）ではなく値の UTF-8 byte 長で代用（Path は canonical label または requested path の長さ、引数型エラーは 0）。strict CBOR スライスで揃える | §9.1 | R-1a |
| Path の HMAC-SHA-256 の入力が canonical label の UTF-8 生 byte（`src/audit.rs::RedactionPolicy::hmac_path`）。§9.1/§9.2 の「redaction 前 subtree の canonical encoding」（label を値とする `AuditValue` の deterministic CBOR）ではないため、digest 値が目標仕様と異なる（秘匿性・決定性は保たれる）。strict CBOR スライスで揃える | §9.1/§9.2 | R-1a |
| `AuditFrame.module_id` / `AuditError.module_id` が §7.2 の `AuditField`（Identifier/Path policy 適用済み）ではなく plain `String`（`Option<String>`）。現在は常に空文字列 / `None` で埋めており（`src/embedding.rs` の Terminal error 構築）module 情報を記録しない。§9.2 の encoding も `AuditField` ではなく text / `Option` になる | §7.2/§9.1 | A-1 |
| 公開 `ExecutionOutcome`（`src/embedding.rs`、crate root では `EmbeddingOutcome`）は `Completed` / `Exited` / `RuntimeError` / `BudgetExceeded` / `DeadlineExceeded` / `Cancelled` / `InternalFailure` の 7 variant だけで、§7.2 の `TerminalOutcome` と 1 対 1 に対応しない。link 時の拒否と resolver の host 失敗は `Engine::link` の `Result<_, LinkError>`（`LinkError::Denied` / `LinkError::Resolve(HostError)` ほか）で返し、run 中の capability 拒否と host function の失敗は `RuntimeError`（`ErrorKind::Capability` / `ErrorKind::Host`）に含める。監査失敗は監査付き経路だけの別型 `AuditedOutcome::AuditFailed` で返す。`RecordFailure` / `ReplayMismatch` は record/replay が未実装のため無い | §7.2 | A-1（7 variant の構成は [組み込みAPI仕様](embedding-api.md) の E 系スライス由来） |
| `AuditSinkError::Reentrant` は型だけで（`src/audit.rs`）、§10 の thread-local reentrancy guard が無い。sink callback から同じ Engine の compile/start/poll/pause/resume/cancel・host function・別の audit emit を呼んでも検出せず、`Reentrant` を生成する箇所も無い（§15.3 の「sink callbackのEngine再入を拒否し、deadlockしない」は未達）。検出を入れるのは `fix-audit-sink-reentrancy-guard` スライス | §10 | A-1 |

上の表の `ExecutionOutcome` が 7 variant だけである行（D-8）と、`AuditSinkError::Reentrant` が型だけである行（D-9）は、loop:drift #263 を受けて載せた。どちらも本文（§7.2 の 1 対 1 対応、§10 の再入検出）を実装に合わせて後退させず、逸脱として記録する案を選んだ（D-8 は issue #263 Q1=A、D-9 は Q2=A。いずれも返信なしのため推奨案で確定、[開発ループ手順書](loop-runbook.md) §7.5）。理由: spec が実装の正本であり、現在の逸脱は事実を確かめて逸脱一覧に置く（[ドキュメント運用ガイド](documentation-guide.md)「逸脱一覧の持ち方」）。D-8 の `ExecutionOutcome` は `#[non_exhaustive]` で、後続 Phase で variant を足す前提になっている。D-9 の再入検出は §15.3 の受入基準で、[マニフェスト](manifesto.md)原則 7（観測・監査可能にする）にかかわり、どのスライスも担当していなかったため、`fix` スライスを `todo` で [`slices.toml`](slices.toml) に足した。[組み込みAPI仕様](embedding-api.md) の `ExecutionOutcome`（13 variant）との差は、同 spec が逸脱一覧を作るときに載せる（ドキュメント運用ガイドの移行中注記）。

## 未実装範囲（目標仕様 §1〜§13 のうち未着手・延期中）

| 未実装範囲 | 対象の節(§) | 備考 |
|---|---|---|
| sink 必須化（§1.1、`ConfigError::AuditSinkRequired`。opt-in を既定から外す） | §1.1 | CLI 側の sink 構築が整う後続スライスへ延期 |
| 監査設定の公開 setter（`EngineBuilder::audit_redaction_policy` / `EngineBuilder::audit_budget`）と `ConfigError` への `AuditConfigError` 接続。現状は `#[doc(hidden)]` のテスト用 shim（`audit_redaction_policy_for_test` / `audit_budget_for_test`、不正 budget は panic）だけ | §1.1/§9.1/§10.2 | sink 必須化スライスへ延期 |
| strict deterministic CBOR。現 `encoded_bytes` は naive 推定で後で差し替え可能に隔離 | §9.2 | — |
| 60 KiB 縮退規則（normal event の最大 60 KiB、超過時 Body → Path → Identifier の順で `Omitted` へ縮退、なお超過で `EventTooLarge`）。`AuditFailure::EventTooLarge` は型としてあるが、判定は単一 event が通常 byte 枠全体（`max_encoded_bytes` − Terminal 予約）を超える場合だけで、60 KiB 判定と縮退は無い | §7.2/§9 | 正確な byte 推定が前提のため strict CBOR スライスへ延期 |
| record/replay | §6 | — |
| record/replay での redaction 再適用（record 済み redacted payload を盲目コピーせず現行 policy を適用） | §12 | record/replay スライスへ延期 |
| VM の audit triple 対応（監査 wiring） | §2.1 | Slice 6（VM conformance）へ延期 |
| engine 全体の audit drain queue byte 上限（16 MiB）。per-execution 上限（`max_pending_bytes`）は B-2 で実装済み | §10.2 | 複数 execution 共有の queue 会計と wake 経路が scheduler（E10 統合）と不可分なため延期 |

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
- resolverが返す`ModuleId`はopaqueで安定なUTF-8 identifierであり、同じlogical moduleには同じIDを返す。engineはpathとして再解釈・case fold・Unicode normalizeしない。このno-normalization契約（§4.4のlocale/Unicode非正規化と一致）はSlice 1でshared primitiveの実装doc comment（`ModuleId`）に明文化済み。
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
- 上記のNaN canonicalization・parse・format・deterministic bit化はSlice 1でshared helper（`src/determinism.rs`）へ集約済みであり、意味論・表示規則は不変のまま実装位置だけを1本化している。

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

> 実装メモ（Slice 1）: この`StableError`は`src/stable_error.rs`に実装している。`ErrorCode`は独立enumを作らず`error::ErrorKind`を正本として型エイリアス再利用し（`embedding::ExecutionError.code`の前例に倣う）、`ErrorMessageId`はper-template（canonical messageテンプレート1個に1 ID）で採番する。射影は内部`TsumugiError`/`ErrorKind`から単一関数`StableError::project`で行い、安定型文字列・canonical message・traceを二重管理しない。uncatchable terminalは`ErrorKind::is_uncatchable`を唯一の判定正本として射影しない（`None`）。これらは§4.6の意味を変えない実装上の決定であり、normative structは上記のまま。

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

各normal audit eventのencoded長は最大60 KiBとする。redaction後payloadが超える場合、Body/Path/Identifierの順に`Omitted`へ縮退し（classごとにまとめて行う。まずevent内のBodyをすべて`Omitted`にして測り直し、なお超えればPathをすべて、次にIdentifierをすべて`Omitted`にする。§14「audit-strict-cbor-c4 実装スライス」の判断Q4）、それでも超えるeventは`AuditFailure::EventTooLarge`とする。Terminalだけは失敗を再帰させず、`Full(AuditError)`を60 KiB以内にできない場合に`Emergency`へ置換する。Emergencyはredaction済みfull errorのbyte長とSHA-256だけを持ち、Terminal全体を4 KiB以下にする。したがって64 KiB Terminal reserveで必ずencodeできる。traceは先頭32 frameだけを保持し、残数を`omitted_trace_frames`へ記録する。

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

canonical CBORでは`AuditField`のvariantを`0=Revealed`、`1=HmacSha256`、`2=Omitted`、classは表の順を0始まりのtagとし、fieldを持たないenumとして9.2節の一般規則どおり`[tag]`でencodeする（bare integerにしない。`AuditField`のvariantはfieldを持つので常に`[tag, field...]`であり、ここで定めるのはtagの値だけ。§14「audit-strict-cbor-c1 実装スライス」の判断Q1）。`AuditPayload.fields`と`AuditValue::Object`はUTF-8 key byte順、Sequenceはargument/element順を維持する。host call引数は`fields["arguments"] = Sequence(...)`、operation固有metadataは別keyにする。各sequence element/object valueを独立した`AuditField`にするため、argument indexやnested fieldごとにredactionできる。nestingは64段までとし、それ以上のsubtreeはBody/Omittedへ縮退する。

`original_bytes`はredaction前のcanonical subtree payload長であり、omit/hash後も保持する。redaction前subtreeは、子fieldをすべて`Revealed`（各子のclassはそのまま）とし、子の`original_bytes`も同じ規則で下から再帰的に求めた値として9.2節のencodingで表し、その長さを測る。ただし`AuditValue`へ写せない子は`Omitted{class, original_bytes: 0}`のまま置く（要素は落とさず、Sequenceの長さとargument indexを保つ）。host call引数のうち`AuditValue`へ写せない値（型エラーで文字列でない引数など）は`original_bytes = 0`の`Omitted`とする（§14「audit-strict-cbor-c3 実装スライス」の判断Q2・Q3）。Secret classの親は子を走査・記録せず必ずsubtree全体を`Omitted`、Pathの既定は`HmacSha256`、Bodyは`body_mode == Reveal`かつkeyが`reveal_body_fields`にあり上限内の場合だけ`Revealed`となる。

| class | 例 | 既定 |
|---|---|---|
| Public | event種別、ID、hash、language revision、budget、count、error code | raw |
| Identifier | capability名、host function名、env key、rule ID、module logical name | raw。ただしhostがhash/omitへ変更可 |
| Path | filesystem path、module source location、URL resource identity | HMAC-SHA-256。rawを記録しない |
| Body | source、stdin/stdout、file/network body、host args/result、errorの自由文 | omitし、型・byte数・必要ならHMACだけ |
| Secret | credential、token、password、secret env value、redaction key | 常にomit。mode変更不可 |

`Secret<T>`を型で区別し、`Debug` / `Display` / generic serializerを実装しない。secretの存在とbyte数だけをPublic metadataとして記録できる。

BodyをRevealできるのは、policyのallow-listで特定host function fieldを`audit_safe`と明示し、`max_revealed_body_bytes > 0`の場合だけである。上限と比べる長さは`original_bytes`（CBOR header込みのencode長）とする。上限を超えたbodyはtruncationせず全体をomitし、`truncated = true`ではなく`omitted = true`とbyte数を記録する。prefixだけを残してsecretを漏らさない。

pathはproviderが認可に使ったcanonical resource labelのfield payload全体（labelを値とする`AuditValue::Scalar(AuditScalar::Utf8(label))`の9.2節のencoding）へHMAC-SHA-256を適用する。host absolute pathをlabelへ含めない。単純SHA-256は辞書攻撃に弱いため使わない。deny eventでもraw requested pathを出さない。

error messageはpath、argument、host responseを含み得るため、auditでは安定`ErrorCode`、`message_id`、source位置、redacted parameter summaryだけを保存する。script-visible messageをそのまま保存しない。

### 9.2 Canonical encodingとrequest fingerprint

schema version 1のbyte表現はRFC 8949 deterministic CBORの次のsubsetへ固定する。

- structはRust field宣言順のCBOR array、enumは`[u16 tag, field0, ...]`、`Option`は`[0]`または`[1, value]`でencodeする。mapとしてfield名をencodeしない。
- unsigned/signed integerは値を表せる最短幅、Stringは正規化しないUTF-8 text、byte列はCBOR byte string、bool/nullは標準primitiveとする。Rustの符号なし整数（u8〜u64）はmajor type 0、符号付き整数（i64）は0以上ならmajor type 0、負ならmajor type 1とし、宣言型の幅ではなく値で最短幅を選ぶ。array・text・byte stringは常に定長（definite length）で、indefinite-length encodingを使わない。CBOR tag（major type 6）は使わない。
- enumのvariantは、fieldなしなら`[tag]`、tuple variantなら`[tag, field0, ...]`、struct variantなら`[tag, field...]`（7.2節・9.1節の宣言順）とする。tagは各enumの宣言順を0始まりで振る。
- `HostTimestamp.unix_nanoseconds`は16 byte two's-complement big-endian byte string、Floatはcanonicalized binary64 bitsの8 byte big-endian byte stringとし、CBOR float型を使わない。
- `BTreeMap`は`[[key, value], ...]`としてUTF-8 key byte順、`BTreeSet`もsort済みarray、Vec/sequenceは元順でencodeする。
- `BudgetConfig`、`BudgetCounters`、`BudgetUsage`は[実行予算・協調実行仕様](execution-control.md)のfield宣言順を使う。unknown schema fieldを末尾へ追加せず、schema versionを上げる。
- `AuditEnvelope`は`[schema_version, execution_id(16 bytes), source_hash(32 bytes), language_revision, sequence, timestamp, event]`である。`execution_id`はu128を16 byte big-endianのbyte string、`[u8; 32]`のhash/digestは32 byteのbyte stringとする。
- `MonotonicInstant`（`BudgetConfig.deadline`）は[実行予算・協調実行仕様](execution-control.md)のとおりopaqueなu64 nanosecond tickとしてu64でencodeする。clock domainを区別するための実装内部の識別子（clock instanceごとに異なる値）はencodeしない。実行ごとに変わる値をencodeすると§15.1の決定性を壊すため。
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
  - 対象の入口: start/poll/pause/resume/cancelはschedulerの`ExecutionHandle`の操作を指す。sinkを持つfacadeの`Engine`では、`compile`・`link`・`link_for_repl`・`run`・`run_for_repl`・`run_audited`の6入口を対象にする（startとpollはrun系3入口の中にあり、pause/resumeの入口は無い）。host functionと別audit emitはrun系の入口を通らなければ呼べないため、この6入口で覆う。`CancellationToken::cancel`は任意threadから呼べるflag（[組み込みAPI仕様](embedding-api.md)第7節・第9節）でEngineの入口ではないため、callbackから呼んでよい。`ExecutionHandle`の操作で拒否に返す値は、sinkをscheduler経路へ配線するとき（E10統合）に決める。
  - 再入した呼び出しへ返す値: 何も実行せず、既存のInternalFailure系で拒否する。公開型は増やさない。`run`・`run_for_repl`は`ExecutionOutcome::InternalFailure`、`run_audited`は`AuditedOutcome::Outcome(InternalFailure)`、`compile`は`CompileDiagnosticCode::InternalFault`の診断1件の`CompileErrors`、`link`・`link_for_repl`は`LinkError::InternalFailure`を返す。渡されたcontextは変えず、poisonもしない。
  - 外側のexecution: guardは「このthreadで今sink callbackの中にいるEngineの列」を持ち、再入の印を列の要素ごとに持つ。再入を拒否したら、再入されたEngineの要素にだけ印を立てる。`submit`から戻ったとき自分の要素に印があれば、sinkが返した値（`Ack`を含む）を捨てて`Failed(AuditSinkError::Reentrant)`として扱い、§10.1のとおり`AuditFailure::Sink`でfail-closedにする（`AuditFailure`にvariantは足さない）。
  - 入れ子: Engine Aのcallbackの中で別のEngine Bを実行し、Bのcallbackから Engine Aへ再入した場合、fail-closedになるのはAのexecutionだけで、Bのexecutionは通常どおり進む。Aへの再入は、Aのcallbackを出るまで（Bのcallbackを出たあとも）拒否する。別のEngineをcallbackから呼ぶこと自体は許す。
  - 検出はthread-localであり、別threadからの呼び出しは対象外とする。engine lockを保持したままsinkへ入らない（次項）ため、別threadからの呼び出しでdeadlockしない。
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
    pub max_events: u64,                    // default 100,000、2以上
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
- `max_events < 2`、`terminal_reserve_events != 1`、`terminal_reserve_bytes < 64 KiB`、`host_call_close_reserve_events != 2`、`host_call_close_reserve_bytes < 128 KiB`はconfig errorとする。Terminalは専用event/byte reserveだけを使い、通常上限を使い切っても必ずappendできる。低いmax_events/bytesによりhost lifecycle 3 eventsを予約できないconfigは構築可能だが、host callを効果開始前のAuditFailureにする。`max_events`の下限は、どのexecutionも必ず持つStartedとTerminalの2件である（上の`AuditBudget`定義のコメントが「4以上」としていたのを「2以上」へ直した。loop:drift #263 D-10。この節の`max_events < 2`と実装〔`AuditBudget::validate`〕を正にする案〔Q3=A〕は返信なしのため推奨案で確定、[開発ループ手順書](loop-runbook.md) §7.5）。理由: 3 events を予約できない低い`max_events`もhost callをAuditFailureにする形で扱いが決まっており、下限を4にする理由が無い。
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

各スライス小節はそのスライスを実装した時点の記録であり、書き換えない。現在の逸脱・未実装範囲は冒頭の逸脱一覧を見ること。

各sliceは独立PRとし、前sliceの受入基準を満たしてから次へ進む。

### Slice 1: deterministic primitive

- `LanguageRevision`、`DeterminismRulesRevision`、StableError、ModuleId、source/import graph hashを実装
- locale/Unicode/Float/NaN/BTreeMap/path規則を共有helperへ集約
- AUD-048のFunctionIdをtreeへ導入

Float/NaNのcanonicalization・parse・formatとserialize用bit化は`src/determinism.rs`へ、`DeterminismRulesRevision`固定値と`ModuleId`のno-normalization契約は`src/embedding.rs`のshared primitiveへ集約済み。StableErrorは`src/stable_error.rs`へ実装済み（内部`TsumugiError`/`ErrorKind`からtree backendで決定的に射影する。VM parity・FakeHost gate・audit normalizeは後続slice）。これでSlice 1のdeterministic primitiveは一通り揃った。状態は[ロードマップ](roadmap.md)「ステータス一覧」を参照する。

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

#### A-1 実装スライス（tree engine、opt-in）

Slice 5 の最小縦切り（A-1）を tree engine にのみ実装した時点の、仕様に対する逸脱と
未実装範囲を記録する。これは一時的な実装状態であり、仕様（本章 §1〜§13）が正本である。

- **opt-in（§1.1 からの逸脱）**: A-1 では監査 sink を `EngineBuilder::audit_sink` で設定した
  ときだけ監査が有効になる。sink 未設定時は監査 event を一切 emission せず、既存挙動と
  bit-identical。§1.1 が規定する sink 必須ポリシー（`ConfigError::AuditSinkRequired`）は、
  CLI 側の sink 構築が整う後続スライスへ延期する。
- **emission する event**: `ExecutionStarted`（sequence 0、最初の semantic work の前に ack 待ち）、
  `Terminal`（各 terminal commit 点で 1 件、最後の event）、`BudgetCharged`（§7.3 の集約、
  Started と Terminal の間）、および host-call lifecycle の 3 event（`HostCallStarted` /
  `CapabilityDecision` / `HostCallFinished`、§7.2/§8）、および yield/resume lifecycle の 2 event
  （`Yielded` / `Resumed`、§7.2 tag 5/6）。host-call 3 event は filesystem builtin
  （`read_file` / `read_lines` / `write_file` / `append_file`、tree 唯一の host-call 境界）で
  emission する。`Yielded` / `Resumed` は tree evaluator の slice 境界で emission する（下記
  「yield/resume lifecycle 2 event を tree backend で emission」）。backpressure 由来の yield も
  B-1 で facade の poll driver に配線済み（下記「`AuditBackpressure`-yield 配線」）。これで §7.2 の
  8 event 型が tree backend で emission される。
- **`capability_policy_hash` を実配線（§7.1）**: `ExecutionStarted.capability_policy_hash` には、
  実行を認可した frozen `CapabilitySet` の決定的な相関 ID（`CapabilitySet::id()` の 32 byte）を
  載せる。これにより deny-by-default（空 set）と stdout/filesystem/exit 等を grant した実行を監査上
  区別できる。`redaction_policy_id` は redaction 本体が未実装のため A-1 では安定の既定値のまま。
- **tree-only backend 境界（§2.1/§7.1）**: A-1 の run 経路には backend dispatch が無く、常に tree
  evaluator で実行する。`ExecutionStarted.backend` が実行実体と食い違わないよう、監査 sink と
  `Backend::VmExperimental` を同時に設定した engine の build は `ConfigError::AuditSinkWithExperimentalBackend`
  で拒否する。VM の監査 wiring は Slice 6（VM conformance）へ延期する。
- **fail-closed（§10.1）**: Started が ack されるまで script/import work を開始しない。ack は
  §10 の規則どおり、submit した exact な `execution_id` と連続 `sequence`（Started なら 0）を指す
  ものだけを受理し、別 execution の ack / gap / 未送信 sequence の ack は protocol 違反として
  fail-closed にする。Started の ack 失敗（`Failed` / 不正 ack / A-1 では起きない `Pending`）では
  emergency slot へ `Terminal(AuditFailure)` を append し、script work を一切行わない。実行後の
  `Terminal` の配送失敗（`Failed` / 不正 ack）も fail-closed にし、journal 上の Terminal は確定
  済み（§8 規則10）でも監査は成立していないため success な outcome を返さない。いずれの場合も
  監査付き入口 `Engine::run_audited` は success ではない `AuditedOutcome::AuditFailed` を返す。
  既存の `ExecutionOutcome` enum は変更せず、`AuditFailure` を `InternalFailure` へ畳まない
  （監査失敗は追加型 `AuditedOutcome` でのみ surface する）。
- **Terminal 配送失敗後の language-state は rollback しない（§10.1 からの逸脱、A-1）**:
  tree engine では Completed/Exited の language-state commit が評価器内（`run_slice` →
  `finalize_session(true)` → `commit_submission`）で、Terminal を sink へ配送する前に確定する。
  このため実行後の `Terminal` 配送が失敗して `AuditedOutcome::AuditFailed` を返したときでも、
  **完了済み script の context 変更（binding 等）は commit されたまま残り、roll back されない**。
  journal 上の唯一の Terminal も `AuditFailure`/`context_committed: false` へは書き換えず、確定済みの
  `Completed`/`context_committed: true` のまま（§8 規則10: started execution は配送結果に関わらず
  確定した Terminal を 1 件だけ持ち、§10 のモデルはその実 Terminal の再配送であって `AuditFailure`
  への書き換えではない）。fail-closed の要件（success を返さない）は満たすが、host は
  `AuditFailed` を受け取った context を**再利用してはならない**（完了 script の変更が観測され得る）。
  監査確定を跨いで言語トランザクションを rollback 可能に保つ §10.1 本来の挙動は、retry/backpressure
  基盤が入る後続スライスへ延期する。
- **timestamp は monotonic tick（§7.1 の簡略）**: A-1 では envelope の `timestamp.unix_nanoseconds`
  に、注入された wall/unix `AuditClock` ではなく request の monotonic clock（`request.clock.now()`）の
  ns を流用する。順序の正本は `sequence` であり `timestamp` は非正本（§7.1）なので影響は限定的だが、
  値は unix 時刻ではなく monotonic tick である。専用 `AuditClock` の注入は後続スライスへ延期する。
- **BudgetExceeded resource は原本を保持（§7.2）**: `control_stop_to_error` が細粒度の
  `BudgetResource` を粗い `ErrorKind` へ縮約するため、監査付き経路では評価器が縮約前に退避した
  原本 resource を `Terminal(BudgetExceeded(resource))` に載せる（原本が無い場合のみ `ErrorKind`
  から粗く復元する）。これにより String/Source/I-O 系の細分 resource も §7.2 どおり記録される。
- **host-call lifecycle 3 event を tree backend で emission（§7.2/§8/§11、A-1 の逸脱を明記）**:
  filesystem host-call 境界（tree 唯一の host-call）で `HostCallStarted` → `CapabilityDecision`
  → `HostCallFinished` の triple を emission する。実装上の逸脱は次のとおり。
  - **deferred post-run drain**: triple は `BudgetCharged` と同じく実行中に評価器へバッファし、
    監査付き facade（`run_audited_inner`）が Started と Terminal の間で drain して emission する。
    A-1 facade は 1 回の `execute_program` 内で script 全体を同期実行し、mid-execution の sink
    round-trip 配線を持たないため、`BudgetCharged` が確立した deferred モデルに合わせる。
  - **§8 規則5/6 の timing を延期**: 外部 filesystem 効果は `dispatch_fs` 内で同期完結し、drain
    時点では効果も response も既に確定している。したがって allow decision ack 前に外部効果を
    止める（規則5）・`HostCallFinished` ack 前に response を script へ露出させない（規則6）という
    timing は A-1 では満たせず、retry/backpressure スライスへ延期する。A-1 が満たすのは triple の
    論理順序（規則3）、deny-path の pair/triple（規則4、Deny/Denied/`effect_status=None`）、
    operation_id/call_id の 0 始まり単調・triple 内再利用、journal/sink の fail-closed である。
  - **§11 reserve は drain 時の容量チェック**: host call 開始前の atomic reserve（規則8 相当）は
    A-1 では pre-effect reservation ではなく、drain 時に `AuditJournal::can_reserve_host_call()`
    で「`HostCallStarted`（1 event / ≤60 KiB）＋ close reserve（2 events / 128 KiB）」が通常枠へ
    収まるかを triple ごとに確認し、収まらなければ triple を 1 件も出さず
    `AuditFailure::BudgetExceeded` にする。外部効果自体を reserve 失敗で拒否する faithful 実装は
    規則5/6 と同じ後続スライスへ延期する。1 execution の未完了 host call は tree の同期実行で
    最大 1 個なので close reserve も最大 1 組（§11）。
  - **tree-only / payload は leak-free プレースホルダ**: VM への配線は Slice 6 へ延期し、
    tree/VM 差は既知の受容済み差分とする。payload（`request` / `resource` / `response`）は
    redaction 本体未実装のため原本 byte 長だけを `Omitted` で載せ、生 path/body は漏らさない
    （§9 既定）。`error_code` は安定 `ErrorCode` 文字列であり script 可視 message は載せない。
- **yield/resume lifecycle 2 event を tree backend で emission（§7.2/§8 規則7/8/9、A-1 の逸脱を明記）**:
  tree evaluator の slice 境界（`run_slice`）で `Yielded { reason, poll_index, usage }`（tag 5）と
  `Resumed { previous_reason, poll_index, usage }`（tag 6）を emission する。実装上の決定・逸脱は
  次のとおり。
  - **per-step drain（B-1 で poll driver 化、旧 deferred post-run drain から移行）**: `Yielded`/
    `Resumed` は `BudgetCharged`・host-call triple と同じく実行中に評価器へ buffer する。B-1 で
    監査付き driver を同期 loop（`run_transactional`）から **facade 内の単一 execution poll driver**
    （`run_audited_driver`）へ移行し、1 poll ステップ（`run_slice` 1 回）ごとに評価器 buffer を
    従来 drain 順で `AuditDelivery` 経由で submit する（§10.2 の sink round-trip を実行中に行うため）。
    drain 順は「host-call triple・`Yielded`/`Resumed`・slice 境界で flush した `BudgetCharged` を
    発生した temporal 順に並べた 1 本の列」→ accumulator に残る `BudgetCharged`（per-slice flush）→
    `Terminal`。B-1 時点では host-call triple と `Yielded`/`Resumed` を別 buffer から「host-call 群 →
    yield/resume 群」の固定順で drain しており、resume 後の slice で起きた host call が対の `Resumed`
    より前に配送されて §8 規則8 に反していた（yield と host call が同じ実行で起きる場合だけ）。B-2 で
    評価器 buffer を 1 本にまとめて修正した（下記「B-2 実装スライス」）。backpressure が一度も起きない実行では、per-step drain の envelope
    列は旧 post-run 一括 drain と完全一致する（評価器 buffer を同じ順で引くため。既存
    `tests/audit_yield_resume.rs` / `tests/audit_core.rs` を無改変で緑に保つ回帰条件で固定）。
  - **実 yield を起こす slice fuel の橋渡し（D3）**: 監査付き run 経路（poll driver）は
    従来 `run_slice(u64::MAX)` で単一 slice だったため監査下で実 yield が起きなかった。
    `EngineBuilder::audit_slice_fuel` で小さい slice fuel を与えると監査付き実行が複数 slice へ
    分かれ、文/反復境界で実 `Yielded(SliceFuelExhausted)` が起きて 2 event が流れる。既定（未設定＝
    `u64::MAX`）は従来挙動そのもので、観測結果・出力 byte 列・課金は不変。sink 無し経路
    （`run_guarded`）は常に `u64::MAX` で、この knob の影響を受けない（§1.1 opt-in・非破壊）。
  - **§8 規則7/8/9 を満たす**: 規則7（flush-before-Yielded）は各 yield 境界で budget accumulator を
    flush し、その slice ぶんの `BudgetCharged` を同 slice の `Yielded` より前へ確定させる。規則8
    （Resumed-before-work）は次 slice 先頭で、`run_driver` を呼ぶ前（まだ 1 文も実行していない）に
    `Resumed` を buffer する。規則9（Terminal last）は従来どおり facade が全 deferred buffer を
    drain した後に `Terminal` を append する。`poll_index` は execution ごとに 0 始まり単調
    （`checked_add` で overflow fail-closed）で、1 回の yield/resume pair の `Yielded` と `Resumed`
    は同じ値を共有し、`Resumed.previous_reason` は対の `Yielded.reason` と一致する。
  - **yield-cause → `AuditYieldReason` 写像**: `SliceFuelExhausted`（主経路・テストで実際に起きる）/
    `HostCallPending { call_id }`（cooperative host-call、A-1 同期経路では Ready フォールバックのため
    実際には起きないが写像は定義）/ `ExplicitYield`（型として写像、未 trigger）を純粋関数で写す。
    `AdmissionQueued` / `SchedulerPreempted` / `HostPaused` は engine-poll 経路（sink を持たない）
    でのみ生じるため、写像は定義するが A-1 監査付き facade では emission しない（下記）。
    host pause は `Yielded { reason: HostPaused }`・resume は `Resumed { previous_reason: HostPaused }`
    へ写すが、`HostPaused` を script 可視の公開 `YieldReason` へは追加しない（§8）。
  - **tree-only / VM 未配線**: VM には scheduler/pause 経路が無く、yield/resume emission は tree-only。
    tree/VM 差は既知の受容済み差分（Slice 6 で解消予定）。
  - **mapped-but-not-emitted（engine-poll 経路）**: `AdmissionQueued` / `SchedulerPreempted`
    / `HostPaused` の yield は `engine.rs::poll`（sink を持たない alpha facade）でのみ生じる。
    監査付き driver（facade の poll driver）は slice-fuel yield と backpressure yield を起こすが、
    これら 3 reason は起こさないため emission されない。sink-bearing な scheduler/pause 経路の配線は
    後続スライスへ延期する。
- **`AuditBackpressure`-yield 配線（§10/§10.2、B-1、実装済み）**: audit sink が batch を
  `AuditSubmit::Pending` にしたとき、facade の poll driver（`AuditDelivery::deliver`）が execution を
  実際に `Yielded(AuditBackpressure)` へ遷移させ、ack（同一 `Arc` batch の再送で Ack される、または
  `AuditTicket::try_take`）で `Resumed{previous_reason: AuditBackpressure}` へ resume する。§10 の
  「次回 `ExecutionHandle::poll` が `try_take` して再開」は、A-1（tree-only・facade 内 driver）では
  **facade の poll driver が round-trip を担う**形で実現する（scheduler engine は sink を持たない。
  公開 `YieldReason::AuditBackpressure` は facade driver 経由で returnable な公開 variant になり、
  scheduler engine 側で返すのは E10 統合後）。backpressure は sink 応答由来で評価器の意味論
  （`SliceOutcome`）を跨がず、`Yielded`/`Resumed` の poll_index 発番・pair 共有は評価器の単一
  `poll_index` カウンタに乗る（driver 独自カウンタなし）。backpressure yield が SliceFuel yield の
  event 配送中に起きるとネスト（外側 SliceFuel の Resumed はまだ）するため、外側 yield の pair 情報を
  退避し backpressure Resumed 後に復元する。各 poll ステップ先頭で cancel→deadline を `try_take` より
  **先に** checkpoint し（§4.6 linearization）、永続 `Pending` のまま deadline へ達した場合は
  `AuditFailure::BackpressureDeadline` の terminal にして script の `Deadline` より優先する（§10.2、
  「実行結果は成功だが監査が欠ける」状態を作らない）。公開 API の破壊的変更は
  `YieldReason::AuditBackpressure` / `AuditFailure::BackpressureDeadline` の追加（additive）で、
  `language_revision` bump は無い（既存 tag 5/6 + 既存 `AuditYieldReason::AuditBackpressure`）。§10.2 の
  audit drain queue byte 会計（per-execution 1 MiB / engine 全体 16 MiB・`AuditBudget.max_pending_bytes`
  消費）は B-2 へ分離（sink-Pending 由来の backpressure だけ先行）。これにより §15.3 の受入基準
  「永続 Pending で script が成功扱いにならず AuditFailure terminal になる」が sink-Pending 経路で
  満たされ、§15.3「Yielded と Resumed が交互に対応し audit backpressure を区別できる」も
  `AuditYieldReason::AuditBackpressure` の Yielded/Resumed pair で満たされる（`tests/audit_backpressure.rs`）。
- **未実装（意図的に含めない）**: redaction policy 本体のうち body allow-list Reveal（§9、R-1b。既定 policy の
  redaction tree R-1a は下記「R-1 実装スライス」で実装済み）、
  strict deterministic CBOR（§9.2、A-1 の
  `encoded_bytes` は naive 推定で後で差し替え可能に隔離）、record/replay（§6）、VM の変更、§10.2 の
  audit drain queue byte 会計のうち engine 全体の上限（per-execution 上限は B-2 で実装済み。下記「B-2 実装スライス」）。`AdmissionQueued` / `SchedulerPreempted` / `HostPaused`
  の yield emission（engine-poll 経路に sink が無い、上記）も後続スライスへ延期する。

#### B-2 実装スライス（tree engine、opt-in、audit drain queue byte 会計）

> 状態: **実装済み**（2026-10-20、tree backend・opt-in）。実装上の注記は本小節末尾「実装上の注記」を参照。

B-1 は sink の `Pending` 応答に由来する backpressure だけを配線し、§10.2 の
「audit drain queue の byte 上限」は消費していない（`AuditBudget.max_pending_bytes` は未使用）。
B-2 はこの byte 会計を tree engine の監査付き poll driver へ入れる。仕様（§10.2・§11）が正本であり、
以下は B-2 時点の実装範囲と逸脱の記録である。

- **drain queue の定義**: execution の audit drain queue を「生成済みだが sink の ack を
  まだ得ていない通常 event の encoded byte 合計」と定義する。A-1/B-1 の実装では次の 2 つの和になる。
  1. 評価器 buffer（host-call triple・`Yielded`/`Resumed`・確定済み `BudgetCharged`）にあり、
     journal へまだ append していない event。
  2. journal へ append 済みで、sink の ack をまだ得ていない in-flight batch（現在は 1 envelope/batch
     の同期 deliver なので高々 1 batch）。

  既定 slice fuel（`u64::MAX`）では script 全体が 1 slice で走り、評価器 buffer は terminal まで
  flush されない。B-2 の主目的は、この buffer の無制限な成長を §10.2 の上限で抑えることである。
  `ExecutionStarted` と `Terminal` は queue 会計の対象外とする（Started は最初の work の前に ack 待ち、
  Terminal は §10.1 の専用 reserve を使うため）。`BudgetCharged` は accumulator 内の未確定集約
  （256-charge cut 前）を数えず、確定 event として buffer へ移った時点で数える（未確定集約は
  (resource, reason) ごとに 1 件で有界）。

- **上限判定の位置**: 評価器は event を buffer へ積むたびに queue byte を加算し、既存の slice fuel
  判定と同じ **トップレベル driver の文/反復境界**（`run_driver` の `can_yield && …` 判定と同じ位置）で
  `queue_bytes >= max_pending_bytes` を判定する。達していれば slice を終えて crate 内部の
  `SliceOutcome::YieldedAuditQueue` を返す。式の途中、および関数呼び出し・callback の本体
  （`drive_call_body`、`can_yield = false`）の中では yield しない（tree は Rust 再帰スタックを
  保存できないため。§4.2・[実行予算・協調実行仕様](execution-control.md) Slice 3 PR-d と同じ制約）。
  したがって上限は「トップレベル境界で判定する soft limit」である（下記「逸脱」）。

- **前進保証**: 判定は **その slice で 1 文以上実行した後** にだけ行う。resume 直後（`Resumed` を
  buffer した直後）の境界では判定しない。これにより `max_pending_bytes` がどんな値（0 や
  `Resumed` 1 件より小さい値を含む）でも、yield と resume だけを繰り返して 1 文も進まない livelock は
  起きない。`max_pending_bytes = 0` は「トップレベルの 1 文ごとに flush する」設定として有効であり、
  config error にはしない。

- **yield と event 順序**: queue 上限による yield は §10.2 どおり
  `Yielded { reason: AuditBackpressure }` として記録する（新しい `AuditYieldReason` や
  CBOR tag は追加しない）。sink が即 Ack する場合も必ず `Yielded`/`Resumed` の pair を出す
  （§8: yield は必ず監査される）。順序は SliceFuel yield と同じ規律に従う。
  1. §8 規則7: budget accumulator を flush し、その slice ぶんの `BudgetCharged` を `Yielded` より前に置く。
  2. `Yielded(AuditBackpressure)` を評価器の単一 `poll_index` カウンタで発番する（driver 独自カウンタなし）。
  3. driver は `flush_events` で buffer を全量 deliver する（sink が `Pending` なら B-1 の round-trip に入る）。
     flush 完了時に queue は 0 へ戻る。
  4. 次の `run_slice` 先頭で `Resumed { previous_reason: AuditBackpressure }` を buffer する（§8 規則8）。

  queue 由来か sink `Pending` 由来かは event 上は区別しない（どちらも `AuditBackpressure`）。

- **単一 event が上限を超える場合**: 特別扱いしない。flush 完了で queue は必ず 0 へ戻るため、
  `max_pending_bytes` を超える event が 1 件あっても、次のトップレベル境界で yield・flush して前進する
  （進行不能にはならない）。event 単体の size 検査は従来どおり journal 側（`EventTooLarge` /
  `BudgetExceeded`、§11）に任せ、`EventTooLarge` を queue 上限の意味では使わない。

- **byte の測り方**: queue byte は journal の会計と同じ推定関数で測る（§9.2「予算 byte counter が
  別実装の size 推定を持つことを禁止」）。評価器 buffer の event はまだ envelope ではない（sequence 未確定）
  ため、`audit::estimate_encoded_bytes` を「envelope 固定部（`language_revision` に依存、sequence に
  非依存）＋ `estimate_event_bytes(event)`」の 2 部品へ分解し、journal と queue の双方がこの同じ部品を
  使う。現行 naive 推定は sequence に依存しないので、push 時の計上値は append 時の journal 計上値と
  一致する。

- **engine 全体 16 MiB の延期**: B-2 は **per-execution の上限（`max_pending_bytes`）だけ**を
  実装する。engine 全体の 16 MiB 上限は、複数 execution で共有する queue 会計の持ち主と、上限で
  yield した execution を起こす wake 経路が scheduler（E10 統合）と不可分なため、後続スライスへ延期する。

- **設定経路**: facade は既定で `AuditBudget::default()`（`max_pending_bytes` = 1 MiB）を使う。
  公開 builder setter は追加しない。境界テストのため、`EngineBuilder` に `#[doc(hidden)] pub` の
  shim `audit_budget_for_test(AuditBudget)` を置く（`link_for_repl` / `run_for_repl` と同じ扱いで、
  公開契約は増やさない）。shim は内部で `AuditBudget::validate()` を呼び、不正値なら panic する
  （テスト専用のため）。注入された budget は facade の `AuditJournal::open` と queue 判定の双方に使う。

- **opt-in・非破壊**: sink なし経路（`run_guarded`）は queue 会計を一切行わず、観測結果・出力 byte・
  exit code は不変。sink あり経路では、queue が一度も上限に達しない実行の envelope 列は B-1 と
  完全一致する。新しい公開型・CBOR tag・`language_revision` の変更はない。
  **sink あり経路での挙動変化**: 既定 budget（1 MiB）でも、トップレベル境界までに 1 MiB 超の event を
  溜める script では、envelope 列の途中に `AuditBackpressure` の `Yielded`/`Resumed` pair が入る
  （queue 上限到達は backpressure の一種）。opt-in 経路内の変化であり alpha として受容する。

- **逸脱（B-2 時点で残るもの）**:
  - **関数呼び出し・callback の中では上限が効かない**。判定はトップレベル境界だけなので、例えば
    `main()` の 1 文の中で多数の host call を行うと、`main()` から戻るまで queue は上限を超えて
    溜まり続ける。超過量は host-call 回数 budget（`BudgetConfig::standard` で `max_host_calls`
    = 10,000、event 最大約 3 万件）で有界だが、1 MiB を大きく超え得る。根本対策（関数内 yield、
    または §11 の host call 開始前 reserve）は B-2 の範囲外。
  - engine 全体 16 MiB の未実装。同一 `Engine`（`Send + Sync`、`run_audited(&self, …)`）を複数 thread
    から同時に `run_audited` した場合の合計は束縛しない。
  - byte 数は naive 推定。§9.2 strict CBOR へ差し替えると sequence/timestamp の encode 長が値依存に
    なり、push 時点（sequence 未確定）では正確な長さが分からない。その時点では §11 の size pass と
    同じく「sequence を最大長とみなした上限値」で queue へ計上する。差し替えで yield が起きる位置が
    変わり得るため、回帰テストの期待値を更新する。
  - 公開 setter（`EngineBuilder::audit_budget`）と `ConfigError` への `AuditConfigError` 接続は、
    sink 必須化（§1.1）のスライスへ延期する。

- **変更箇所**:
  - `src/eval.rs`: audit drain queue 計器（`AuditQueueMeter`。buffer への push 時に加算、sink の ack 時に
    減算）、`max_pending_bytes` の保持（`set_audit_queue`、計器が無い経路では計上・判定なし）、
    トップレベル文/反復境界での判定（slice 内で 1 文以上実行済みの場合のみ）、
    `SliceOutcome::YieldedAuditQueue`、`audit_yield_reason_from` の写像追加
    （→ `AuditYieldReason::AuditBackpressure`）。host-call triple 用と yield/resume 用の 2 本の buffer を
    temporal 順の 1 本（`audit_events`）へ統合（下記「実装上の注記」）。
  - `src/builtin.rs`: 変更なし（`push_host_call_events` 経由で計上される）。
  - `src/budget.rs` / `src/audit.rs`: `BudgetChargeAccumulator` が確定済み（256-charge cut 済み）分の
    event byte 合計を保持し、`BudgetLedger::audit_finalized_charge_bytes` で queue 判定へ渡す。
  - `src/audit.rs`: `estimate_encoded_bytes` を envelope 固定部（`estimate_envelope_overhead_bytes`）＋
    event 本体（`estimate_event_bytes`、`pub(crate)`）へ分解。`max_pending_bytes` の doc コメントを更新。
  - `src/embedding.rs`: `run_audited_driver` で計器を張り、`YieldedAuditQueue` を `continue` 扱いにする。
    `flush_events` は temporal 順の 1 本の列を deliver し、各 event を ack 時に queue から外す（in-flight を
    含む）。flush 後に queue が 0 に戻ることを debug assert。`EngineBuilder::audit_budget_for_test` shim と、
    注入 budget の `AuditJournal::open` への受け渡し。
  - `src/engine.rs`: engine-poll 経路の `SliceOutcome` 写像に `YieldedAuditQueue` を追加（計器を張らない
    ので到達しないが、到達時は通常の slice 境界 yield と同じ扱い）。

- **受入基準（B-2）**:
  - **B2-AT-01**: `max_pending_bytes` を小さく注入し、**トップレベルのループから** host call を多数行う
    script を監査付きで実行すると、上限到達の境界で `Yielded{AuditBackpressure}` →
    `Resumed{AuditBackpressure}` の pair が出る。sink が即 Ack する場合も pair が出る。pair は
    poll_index を共有し、`Resumed.previous_reason` は対の `Yielded.reason` と一致する（§15.3）。
  - **B2-AT-02**: 上限到達時に queue の event が捨てられたり上書きされたりしない。sequence は 0 から
    gap・重複がなく、host-call の triple がすべて揃う（§10.2「queueを捨てない」・§15.3）。
  - **B2-AT-03**: §8 規則7（`BudgetCharged` が `Yielded` より前）・規則8（`Resumed` が次の work より前）・
    規則9（Terminal が最後）を満たす。
  - **B2-AT-04**: queue 上限 yield と sink `Pending` が重なる場合（queue 上限で flush 中に sink が
    `PendingThenAck` を返す場合）も、pair が交互に正しく対応し、最終的に Completed になる。
  - **B2-AT-05**: queue 上限 yield 中の flush で sink が `PendingForever` を返し deadline に達した場合は、
    `AuditFailure::BackpressureDeadline` になる（script の Deadline より優先、§10.2）。
  - **B2-AT-06**: 単一 envelope が `max_pending_bytes` を超える設定（`max_pending_bytes = 0` を含む）でも、
    進行不能にならず各トップレベル境界で yield・flush して Completed になる。yield/resume だけを
    繰り返す livelock が起きない（前進保証）。
  - **B2-AT-07（非破壊）**: 既定 budget（1 MiB）で既存の `tests/audit_core.rs` /
    `tests/audit_hostcall.rs` / `tests/audit_yield_resume.rs` / `tests/audit_backpressure.rs` が
    無改変で緑。sink なし経路の出力 byte・exit code が不変。既存テストが上限に到達して期待値が
    変わる場合は、実装を止めて報告する。
  - **B2-AT-08（決定性）**: 同じ script・同じ注入 budget・同じ sink 応答列では、yield が起きる位置と
    envelope 列が 100 回の実行で一致する（§15.1 の部分適用）。
  - **B2-AT-09（関数内 soft limit）**: 関数本体の中で多数の host call を行う script では、関数の中では
    yield せず、関数から戻った後のトップレベル境界で yield する。その間 event は欠落せず、
    sequence・triple の完全性（B2-AT-02 と同条件）を満たす。
  - **B2-AT-10（byte 計上の一致）**: push 時に queue へ計上した byte の総和が、同じ event を journal へ
    append したときの `committed_bytes` の増分と一致する（推定関数の共有）。
  - VM は対象外（監査 sink と `VmExperimental` の同時設定は従来どおり build 時に拒否）。

- **テスト配置**: `tests/audit_queue_bytes.rs`（B2-AT-01〜09、および §8 規則8 の回帰
  `slice_fuel_yield_with_host_calls_keeps_resumed_before_next_work`）。B2-AT-10 は `src/audit.rs`
  （`b2_at_10_queue_estimate_matches_journal_committed_bytes` ほか）/ `src/eval.rs`（`audit_queue_meter`）の
  unit test、B2-AT-07 は既存 `tests/audit_core.rs` / `tests/audit_hostcall.rs` /
  `tests/audit_yield_resume.rs` / `tests/audit_backpressure.rs` の無改変緑で担保する。

- **実装上の注記（B-2 実装時に判明・決定したもの）**:
  - **§8 規則8 違反の修正（B-1 からの既存不具合）**: B-1 までは host-call triple と `Yielded`/`Resumed` を
    評価器の別 buffer に積み、flush で「host-call 群 → yield/resume 群」の固定順に drain していた。この
    ため yield（SliceFuel を含む）と host call が同じ実行で起きると、resume 後の slice の host call が対の
    `Resumed` より前に配送されていた（既存テストは yield と host call を同時に起こさないため未検出）。
    B2-AT-03 で顕在化したため、buffer を temporal 順の 1 本に統合して修正した。yield と host call が
    同時に起きない実行の envelope 列は不変（既存テスト無改変で緑）。
  - **`SliceOutcome` への variant 追加**: `SliceOutcome` は `pub mod eval` の公開 enum だが、crate 文書
    （`src/lib.rs`）どおり個別モジュールはベンチ・テストツール互換のための公開で埋め込み API としての
    安定性を保証しない。埋め込み入口（crate root re-export）には現れないため、`YieldedAuditQueue` の
    追加は安定公開 API の破壊的変更として扱わない。
  - **test 用 budget の保持場所**: `EngineConfig` は全 field 公開の公開構造体で、field 追加は外部の
    literal 構築を壊すため、注入した `AuditBudget` は field 非公開の `EngineBuilder` / `Engine` に持たせる。
  - **accumulator から直接 drain する `BudgetCharged`**: flush 時に accumulator から drain した分は、
    drain で accumulator 側の合計から外れるため、その時点で queue へ計上し直し、ack 時に外す。queue の
    判定は評価器の境界（flush 中ではない）で行うので、判定時点の queue に in-flight 分は現れない。

#### R-1 実装スライス（tree engine、opt-in、redaction policy 本体）

> 状態: **実装済み（R-1a、tree backend・opt-in）**。本小節は A-1 が残した redaction プレースホルダ
> （payload は leak-free の `Omitted`、`redaction_policy_id` は固定値）を §9.1/§9.2 の redaction policy
> 本体へ差し替える最小縦切り（R-1a）の slice 定義である。仕様（本章 §9）が正本であり、以下は R-1 で
> 実装した範囲と意図的に延期する逸脱の記録である。実装は本定義とは別 PR（実装 PR #238）で行った。
> 公開型 `AuditPayload`/`AuditField` 差し替えは破壊的変更だが alpha として受容し、`language_revision`
> の bump は行わない（根拠は [ロードマップ](roadmap.md) 明記）。残る strict CBOR・record/replay・VM
> audit triple 対応・sink 必須化・body allow-list Reveal（R-1b）は未実装。

A-1 が残していた redaction プレースホルダ（host-call lifecycle 3 event〔§7.2/§8〕の payload を
crate 内簡略型 `AuditPayloadField::Omitted` で leak-free に載せ、`redaction_policy_id` を固定値
`"default"` にしていた）は R-1a で §9.1 の `AuditField` ツリーへ差し替え済みである（構築は
`src/builtin_core.rs::host_call_triple`）。R-1a は既定 redaction policy を tree engine の監査付き
経路へ配線し、`request`/`response` の `body` は `Omitted{class:Body}`、`resource` の path は Allow で
canonical label の `HmacSha256`・Deny で `Omitted{class:Path}`（raw path/label は非露出）とし、
`arguments` は `Revealed{class:Public, Sequence}` で各要素を Path/Body へ畳む。`ExecutionStarted` の
`redaction_policy_id` は注入 policy の `policy_id`（既定 `"default-r1"`）を反映する。

> このスライス定義は semantic review（本小節末尾「R-1 レビュー反映の記録」）を複数周通し、初版から
> 次の点を改めた: (1) encoded 長の非破壊根拠が成り立たないため **byte 推定（`estimate_event_bytes`）を
> R-1 では据え置く**ことにした、(2) 現状 payload が「空」という事実誤認と `src/builtin.rs` の誤記を正した、
> (3) 既定 `hmac_key` のジレンマを設計判断として明記した、(4) 公開型 `AuditPayload`/`AuditPayloadField`
> 差し替えが破壊的変更であることを明記した。採用・不採用の理由は末尾の記録を参照。

- **粒度は R-1a（既定 policy のみ）**: 既定 policy（`identifier_mode = Reveal` / `path_mode =
  HmacSha256` / `body_mode = Omit`）だけを実装し、filesystem host-call（tree 唯一の host-call 境界）の
  実 payload を §9.1 の `AuditField` ツリーへ載せる。body の allow-list Reveal 経路
  （`reveal_body_fields` と `max_revealed_body_bytes > 0` による `audit_safe` field の明示開示）は
  型として用意するが R-1 では常に `Omitted` に畳み、R-1b（後続スライス）へ延期する。

- **型の差し替え（§9.1）**: crate 内簡略型 `AuditPayload`（`Vec<(String, AuditPayloadField)>`）と
  `AuditPayloadField`（`Public` / `Omitted`）を §9.1 の型へ差し替える。`AuditField`（`Revealed` /
  `HmacSha256` / `Omitted`、tag 0/1/2）・`AuditValue`（`Scalar` / `Sequence` / `Object`）・
  `AuditScalar`・`AuditFieldClass`（`Public` / `Identifier` / `Path` / `Body` / `Secret`）・
  `RedactionMode`（`Reveal` / `HmacSha256` / `Omit`）・`RedactionPolicy` を新設する。`AuditPayload.fields`
  は §9.1 どおり `BTreeMap<String, AuditField>`（UTF-8 key byte 順）とし、`Sequence` は argument/element
  順を維持する。host call 引数は `fields["arguments"] = Sequence(...)` へ、operation 固有 metadata は
  別 key へ入れる。nesting は 64 段までとし、それ以上の subtree は `Body`/`Omitted` へ縮退する。

- **field class 判定と既定モード（§9.1）**: filesystem host-call について次を適用する。
  - **Path**: provider が認可に使った canonical resource label に HMAC-SHA-256 を適用し、
    `HmacSha256 { digest, original_bytes }` で記録する。**canonical resource label の定義**: routing 後の
    `FilesystemTarget`（`src/capability.rs`）＝ **mount 名（`MountName`）＋ mount 相対 path
    （`RelativePath` の component 列）**を、`@<mount>/<component>/...` の正規形へ組んだ文字列とする
    （host absolute path・CWD・symlink 解決結果は含めない）。同じ mount・同じ mount 相対 path は同じ label
    ＝同じ digest になり（R-AT-01 の「同じ label」の意味）、別 mount または別相対 path は別 digest になる。
    host absolute path は label へ含めない。raw path は `Revealed` へ出さない。deny event でも raw
    requested path を出さない。単純 SHA-256 は使わない。この配線のため、評価器は現状 0 固定の
    `path.original_bytes` を実長へ改め、canonical resource label を `host_call_triple` へ渡す
    （下記「変更箇所」）。
    - **Path を HMAC するかは decision で決める（label の有無ではなく判定で分岐）**: HMAC 対象は「provider が
      認可に使った canonical resource label」で、これは `fs_route`（`src/builtin_core.rs`）が
      `FilesystemTarget` を返した**時点**（＝mount routing と operation 認可が通った時点）で確定する。
      `fs_open` 内の `adapter.open_file` 失敗はこの後に起きるので、その host error でも label は既にある。
      **R-1 の確定規則は「`host_call_triple` の `CapabilityDecisionKind` が `Allow` なら Path を
      `HmacSha256`、`Deny` なら label が手元にあっても `Omitted { class: Path }`」**とし、label の有無では
      なく decision で分岐させる（label 有無を基準にすると「label があるなら HMAC してよい」と読み違える
      恐れがあるため）。decision 別の内訳:
      - **Allow / `Success`**（正常完了）→ Path は `HmacSha256`。
      - **Allow / `HostError`**（route は成功して label 確定済み、その後 `adapter.open_file` /
        `read_to_end` / `write_all` / `create_dir` / `remove` 等が失敗。`fs_result` は
        `Err(ErrorKind::Host)`）→ label はあるので Path は `HmacSha256`。`fs_result` が `Err` でも label を
        落とさないことが要件（下記「変更箇所」の戻り値型）。
      - **Deny / `Denied`** → Path は `Omitted { class: Path }`。大半は label 確定前の拒否だが、
        **route 成功後に拒否される経路もある**: Upsert（`write_file`/`append_file`）が Write では通り
        Create 権限が無いとき、`fs_route`（Write 代表 op）成功で target＝label が決まった後に
        `filesystem_denied` になる。この経路でも decision は `Deny` なので label を HMAC せず `Omitted`。
        その他の Deny は label 確定前: (a) filesystem 未 grant（`filesystem()` が `None`）、
        (b) path lexical/argument エラー（`FilesystemTarget::parse` の `PathError`）、
        (c) mount 未登録・operation 未 grant・許可外（provider 問い合わせ前の `sandbox` denial）。

      Deny では HMAC せず raw requested path も `Revealed`/HMAC 入力に出さない（raw path を代用 HMAC する
      のは host absolute path を label へ含めない §9.1 方針に反し、3 OS の path 表現差が digest に漏れる
      ため採らない）。まとめると **Path は `Allow`（Success/HostError 両方）で `HmacSha256`、`Deny` で
      `Omitted`（label の有無を問わず）、raw は常に非露出**（R-AT-01/02 で明記）。
      - **`original_bytes` の取り方**: `Allow` は canonical label の UTF-8 byte 長。`Deny` で label が
        手元にある場合（Upsert の Create 不足）も、`Deny` は label を payload へ出さないため
        `original_bytes` は requested path 文字列の UTF-8 byte 長で統一する（label 由来の長さを漏らさない）。
        `Deny` の (a)/(c) も requested path の UTF-8 byte 長。(b) のうち **引数の型エラー（`require_str`
        失敗で path 文字列すら取れない）**は requested path が無いため `original_bytes = 0` とする。
  - **Body**: file 内容・stdin/stdout・host args/result の自由文は `body_mode = Omit` により
    `Omitted { class: Body, original_bytes }` にする。`original_bytes` は redaction 前の canonical
    subtree 長。R-1 では allow-list Reveal を行わない。
  - **Identifier**: host function 名・capability 名・rule ID・module logical name は既定 `Reveal`
    （`Revealed { class: Identifier, value, original_bytes }`）。
  - **Public**: event 種別・ID・hash・count・error code は `Revealed { class: Public, .. }`。
  - **Secret**: credential/token/password/secret env value/redaction key は常に subtree 全体を
    `Omitted`、mode 変更不可。`Secret<T>` を型で区別し `Debug` / `Display` / generic serializer を
    実装しない。存在と byte 数だけを Public metadata として記録する。R-1 の filesystem 境界には
    Secret field は現れないが、型と畳み規則は本スライスで入れる。

- **`original_bytes` の定義（§9.1）と R-1 での代用**: §9.1 では `original_bytes` は redaction 前の
  canonical subtree payload 長（§9.2 の canonical encoding で測る）と定義される。ただし R-1 は strict CBOR
  （§9.2）を実装せず byte 推定を据え置くため、**strict CBOR スライスまでは `original_bytes` を値の UTF-8
  byte 長で代用する**（scalar は UTF-8 文字列の byte 長、Path は canonical label または requested path の
  UTF-8 byte 長、body は原本文字列の byte 長）。canonical subtree 長との差は strict CBOR 導入時に揃える。
  この代用値は payload の**値**であって event の encode 推定長（128 固定）とは独立で、B-2 の queue 会計には
  影響しない（上記「byte 推定は据え置く」）。

- **`redaction_policy_id` の実配線（§7.1/§7.2）**: `ExecutionStarted.redaction_policy_id` に注入された
  `RedactionPolicy.policy_id` を**そのまま**載せる。`policy_id` は §9.1 の型どおり **host が付ける
  `String`**（内容ハッシュではない。host が policy を識別するための安定ラベル）。A-1 の固定値 `"default"`
  から、注入 policy の `policy_id` 値へ変える。既定 policy は `policy_id = "default-r1"` とし、A-1 の
  `"default"` と区別できる識別子にする（後続スライスで policy が増えたとき既定と区別するため）。

- **byte 推定は R-1 では据え置く（§9.2、非破壊の前提）**: R-1 は `estimate_event_bytes`
  （`src/audit.rs`）を**変更しない**。現状 host-call lifecycle event（`HostCallStarted` /
  `CapabilityDecision` / `HostCallFinished`）・`Yielded` / `Resumed` / `BudgetCharged` は naive 推定の
  **固定 128 byte**（`_ => 128` 分岐）で見積もられ、journal/queue の byte 会計はこの値で動く。R-1 は
  payload の**中身**を §9.1 の `AuditField` ツリーへ差し替える（leak-free を強化する）が、見積もり長は
  128 固定のまま据え置く。これにより host-call event の encoded 推定長・sequence・yield 位置・B-2 の
  queue byte 会計が A-1/B-2 と完全に不変になり、R-AT-05 の非破壊が実際に成立する。
  - **なぜ据え置くか**: payload を実サイズで見積もるように変えると、host-call event の見積もり長は
    128 から payload 依存の値へ動き、B-2 の `max_pending_bytes` queue 会計で yield が起きる位置が変わる。
    これは §9.2 strict CBOR スライス（naive 推定全体を正確な encode 長へ差し替える別スライス、既に延期中）が
    扱う範囲であり、そちらに寄せる方が B-2 の回帰を 1 回で済ませられる。R-1 で見積もりに手を入れると、
    初版定義が自己矛盾していた「`original_bytes`（field の値）を保てば encode 長も不変」という成り立たない
    根拠に逆戻りする（`original_bytes` は payload 中の値であって encode 後の長さそのものではない）。
  - **60 KiB 縮退規則も strict 推定に依存するため R-1 では範囲外**: §9 の「normal event は最大 60 KiB、
    超過時 Body → Path → Identifier の順で `Omitted` へ縮退、なお超過で `EventTooLarge`」は、event の
    実 encode 長に対する判定なので、正確な byte 推定が前提になる。128 固定見積もりのままでは faithful に
    実装できないため、縮退規則は strict CBOR スライスへ延期する（R-1 の受入基準から R-AT-09 を外す）。
    Terminal の `Emergency` 置換は A-1 で実装済みの範囲を踏襲し、R-1 では触らない。

- **policy 注入経路**: facade は既定で `RedactionPolicy::default()`（上記既定モード・空
  `reveal_body_fields`・`max_revealed_body_bytes = 0`・`policy_id = "default-r1"`・固定 `hmac_key`）を
  使う。公開 builder setter は追加しない（sink 必須化 §1.1 のスライスへ延期）。境界テスト用に
  `EngineBuilder` へ `#[doc(hidden)] pub` の shim（`audit_redaction_policy_for_test` 等、
  `audit_budget_for_test` と同じ扱い）を置き、注入 policy を journal と payload 構築の双方へ渡す。
  `hmac_key` は `SecretBytes` で保持し、`Debug` / `Display` を実装せず生 key を出さない。

- **既定 `hmac_key` の性質（設計判断・逸脱として明記）**: R-1 では公開 setter を作らないため、既定
  policy の利用者は既定 `hmac_key` しか使えない。この key の扱いには相反する制約がある。
  - 実行ごとに乱数で生成すると §7.1「engine は ambient な乱数源に触らない」に反し、R-AT-06（決定性・
    HMAC が key 固定で決定的）も壊れる。したがって **R-1 は key を乱数生成しない**。
  - 一方、コードに固定 key を埋め込むと key が公開されているのと同義で、§9.1 が「単純 SHA-256 は辞書攻撃に
    弱いので使わない」と定めた path hashing の秘匿性はほぼ得られない。
  - **採る方針**: R-1 の既定 `hmac_key` は **crate 内の固定値（alpha 用・秘匿性なし）**とし、「audit に
    **raw path をそのまま載せない**」ことだけを目的とする。固定 key では同一 label が同一 digest になるため
    label の相関は残り（これは R-AT-01 の決定性に必要）、かつ **key が公開されている以上、digest から
    候補 path を総当たりで推測・復元できる**（辞書攻撃耐性は得られない）。したがって R-1 が守るのは
    「平文 path の直接露出を防ぐ」ところまでで、推測復元に耐える秘匿性は守れないと明記する。推測復元にも
    耐える秘匿性のある key を host から渡させる公開経路（`EngineBuilder` の key 必須 setter、または sink が
    あるとき key 必須）は **sink 必須化（§1.1）スライス**へ延期する。R-1 の固定 key は §7.1（乱数源に
    触らない）と R-AT-06（決定性）を満たし、秘匿性だけを後続へ送る割り切りである。

- **opt-in・非破壊**: sink なし経路（`run_guarded`）は redaction を一切行わず、観測結果・出力 byte・
  exit code は不変。sink あり経路では、上記「byte 推定は据え置く」により host-call event の encoded
  推定長は 128 固定のまま変わらないため、sequence・yield 位置・B-2 の queue byte 会計が A-1/B-2 と一致する。
  新しい CBOR tag・`language_revision` の変更はない（`AuditField` の tag は §9.2 で既に予約済み）。
  変わるのは **payload の中身**だけである: `request`/`resource`/`response` の `fields` が A-1 の簡略型
  `AuditPayloadField::Omitted`（`body`/`path`）から §9.1 の `AuditField` ツリー（path は Allow 時
  `HmacSha256`・Deny 時 `Omitted{class:Path}`、body は `Omitted{class:Body}`、function 名等は
  `Revealed{class:Identifier}`）へ変わり、`redaction_policy_id` が `"default"` から `"default-r1"` へ
  変わり、`resource` の `path.original_bytes` が 0 固定から実長（Allow は canonical label の UTF-8 byte 長、
  Deny は requested path の UTF-8 byte 長〔Upsert の Create 不足で label があっても requested path 長で
  統一〕、引数型エラーは 0）へ変わる。したがって
  payload 構造・`redaction_policy_id`・`original_bytes` を検査する既存テストの期待値更新が要る
  （下記 R-AT-05）。

- **公開 API の破壊的変更（alpha・受容、`language_revision` bump なし）**: `AuditPayload` と
  `AuditPayloadField` は crate root（`src/lib.rs`）で `pub use` 公開されている。R-1 は簡略
  `AuditPayloadField` を撤去し `AuditPayload.fields` を §9.1 の `BTreeMap<String, AuditField>` へ
  差し替えるため、これらの型に依存する外部コードは壊れる。B-1（`YieldReason::AuditBackpressure` 追加）と
  同じく、alpha（後方互換無保証）につき roadmap 明記を条件に**破壊的変更を受容**する。CBOR tag 表・
  charge table・script 観測値は不変なので `language_revision` bump は無い（既存 tag 1/2/3 の payload 中身が
  簡略型から §9.1 ツリーへ変わるだけ。`AuditField` の tag 0/1/2 は §9.2 で予約済み）。

- **逸脱（R-1 時点で残るもの、後続スライスへ延期）**:
  - **fs builtin 全体の監査範囲**は別 slice で決める（6 周目レビュー反映）。R-1 で host-call triple を
    emission するのは I/O byte 課金対象の 4 builtin（`read_file` / `read_lines` / `write_file` /
    `append_file`）のみで、`rename` / `mkdir` / `remove` 系 / `list_dir` / `file_size` / `is_file` /
    `is_dir` / `path_exists` など capability 判定を通る他の fs builtin は監査対象外のまま据え置く。これらを
    監査対象へ広げると emission 範囲（event 件数・sequence・queue byte 会計）が変わり §7.2/§8 の扱いを要する
    ため、「fs builtin 監査範囲」slice で一括して決める。その slice で `rename`（2 path 操作）を対象化する
    ときは、resource を `source_path` / `destination_path` の 2 field に分け、Allow は 2 つとも canonical
    label の `HmacSha256`、Deny は（移動元の route だけ通った場合も含め）2 つとも `Omitted { class: Path }`、
    `original_bytes` はそれぞれの requested path の UTF-8 byte 長とする（PR #238 でユーザー合意済みの設計を
    引き継ぐ）。
  - body の allow-list Reveal（`reveal_body_fields` / `max_revealed_body_bytes > 0` による
    `audit_safe` 開示）は型のみで常に Omit（R-1b）。
  - **byte 推定の正確化**（`estimate_event_bytes` を payload 依存の実 encode 長へ差し替え）は R-1 の
    範囲外。host-call event は 128 固定見積もりのまま据え置く（上記「byte 推定は据え置く」）。strict
    deterministic CBOR（§9.2）スライスで naive 推定全体をまとめて差し替える。
    - **128 固定は概算であり上限保証ではない**: 現状のコメントは 128 を保守的な上限扱いにしているが、
      R-1 で payload が HMAC digest（32 byte）や Identifier の実データを含むと、実 encode 長が 128 を
      超えうる。R-1 は見積もりを据え置くため、§11 の `max_encoded_bytes` / 60 KiB 判定は実長ではなく
      128 概算に対して行われ、**strict CBOR スライスまで不正確**（過小評価がありうる）。R-1 の host-call
      payload は path=digest・body=Omit・識別子のみで数百 byte 規模に収まり 60 KiB 判定に実害は無いが、
      判定が概算依存である事実を逸脱として残す。
  - **compile-fail に `trybuild` を使わない**: R-AT-07 の compile-fail は、dev-dependency を増やさず
    `compile_fail` doctest で固定する。現状 dev-deps は `criterion` のみで、`trybuild` の stderr
    snapshot は 3 OS（ubuntu/macos/windows）CI で壊れやすい。doctest は error 文言に依存せず「compile
    しない」ことだけを固定できるため、OS 差に強く依存も増えない。`trybuild` 導入は必要になった時点で
    別途判断する。
  - **60 KiB 縮退規則**（§9 の Body → Path → Identifier 縮退・`EventTooLarge`）は正確な byte 推定が
    前提のため strict CBOR スライスへ延期する。
  - 秘匿性のある `hmac_key` の host 注入経路（公開 key 必須 setter、sink 時 key 必須）は sink 必須化
    （§1.1）スライスへ延期する。R-1 の既定 key は固定値（alpha 用・秘匿性なし。raw path を直接載せない
    ことだけを担保し、key 公開ゆえ digest からの推測復元には耐えない）。
  - VM 対応（Slice 6）。監査 sink と `VmExperimental` の同時設定は従来どおり build 時に拒否する。
  - record/replay の redaction 再適用（§12「record 済み redacted payload を盲目コピーせず現行 policy を
    適用」）は record/replay スライス（§6）へ延期する。
  - 公開 setter（`EngineBuilder::audit_redaction_policy`）と `ConfigError` 接続は sink 必須化（§1.1）
    のスライスへ延期する。

- **変更箇所（見込み）**:
  - `src/audit.rs`: §9.1 の型（`AuditField` / `AuditValue` / `AuditScalar` / `AuditFieldClass` /
    `RedactionMode` / `RedactionPolicy` / `Secret` / `SecretBytes`）新設、簡略 `AuditPayloadField` 撤去、
    `AuditPayload.fields` を `BTreeMap<String, AuditField>` 化。**`estimate_event_bytes` は変更しない**
    （128 固定見積もりを据え置く。上記「byte 推定は据え置く」）。60 KiB 縮退規則は実装しない（延期）。
  - `src/builtin_core.rs`（**初版の `src/builtin.rs` は誤記**）: `host_call_triple` の payload 構築を、
    現状の `AuditPayloadField::Omitted`（`body`/`path`）から既定 policy 適用の `AuditField` ツリーへ
    置き換える。path は allow 時に canonical resource label を HMAC・deny 時は `Omitted{class:Path}`、
    body は `Omitted{class:Body}`、function 名は `Revealed{class:Identifier}`。現状 0 固定の
    `path.original_bytes` を実長（allow は label 長、deny は requested path 長）へ改める。
    - **canonical label を triple まで運ぶ配線が必要（host error でも label を落とさない）**: label は現在
      `dispatch_fs` → `dispatch_filesystem_capability` の**内側**、具体的には `fs_route` が
      `FilesystemTarget` を返した時点（`fs_open` 内の `adapter.open_file` 成否より前）で確定し、外へ返って
      いない。実装では label を `fs_route` の直後に受け取り、open/adapter 失敗（HostError）でも保持する。一方 triple は `src/builtin.rs` 側で `dispatch_fs` が返った **後**に
      `fs_result: Result<Value, TsumugiError>` から組み立てている（label は手元に無い）。label の有無は
      `host_call_triple` の分類どおり **decision で決まり outcome の成否と独立**なので、戻り値型は
      **`Ok` でも `Err` でも label を運べる形**にする。具体的には `dispatch_fs` の戻り値を
      `Result<Value, TsumugiError>` から「`{ outcome: Result<Value, TsumugiError>, label: Option<ResourceLabel> }`」
      相当（label は Allow＝`Success`/`HostError` 両方で `Some`、Deny で `None`）へ拡張する。
      **特に host error（route 成功後に `read_to_end`/`write_all` 等が失敗）では `outcome` が `Err` でも
      `label` は `Some`** にする（ここが「allow なら Some」だけでは取りこぼす経路）。`src/builtin.rs` の
      呼び出し側はこの `label` を `host_call_triple` へ渡す。`dispatch_fs` は非監査経路（sink なし）でも
      通るため、label 返却は監査の有無に関わらず常に行い、triple 構築側だけが使う（非監査経路は label を
      無視し観測挙動不変）。
  - `src/builtin.rs`: filesystem builtin dispatch で `dispatch_fs` が返した canonical label（または
    その不在）を `host_call_triple` へ渡す（上記配線の呼び出し側）。
  - `src/embedding.rs`: `EngineBuilder`/`Engine` に `RedactionPolicy` を保持（field 非公開）、
    journal と payload 構築へ受け渡し、`audit_redaction_policy_for_test` shim、`ExecutionStarted` の
    `redaction_policy_id` 実配線（既定 `"default-r1"`）。
    - **`#[derive(Debug)]` との整合**: `EngineBuilder` と `Engine` は現在 `#[derive(Debug)]`。
      `RedactionPolicy`（`hmac_key: SecretBytes` を含む）を field に持たせると、`SecretBytes` が `Debug` を
      実装しない方針（§9.1・R-AT-07）のため derive が壊れる。対応は `SecretBytes` 以外を自動 derive しつつ、
      `RedactionPolicy` に **key を伏せた手書き `Debug`**（`hmac_key` を `"<redacted>"` 等で出す）を実装し、
      `Engine`/`EngineBuilder` の derive はそのまま使えるようにする。`SecretBytes` 自体は `Debug`/`Display`/
      generic serialize を実装しない（R-AT-07 の compile-fail で固定）。
  - `src/eval.rs`: payload 構築 helper が policy と canonical label を参照できるよう受け渡し（host-call
    buffer へ積む前に redaction 適用、temporal 順 1 本 buffer の不変は B-2 のまま）。

- **受入基準（R-1）**:
  - **R-AT-01（Allow 時の Path、Success と HostError 両方）**: 認可が通った filesystem host-call の path が
    raw ではなく `HmacSha256`（32 byte digest）として記録され、同じ canonical label は同じ digest、異なる
    label は異なる digest になる（§9.1）。**正常完了（`Success`）だけでなく、route/open 成功後に adapter が
    失敗した host error（Allow/`HostError`、`fs_result` は `Err`）でも Path は `HmacSha256`**（label は失敗
    前に確定しているため）。host error ケースを明示的にテスト対象へ含める。（`rename` は R-1 の監査対象外
    ＝triple を emission するのは I/O byte 課金対象の 4 builtin〔`read_file`/`read_lines`/`write_file`/
    `append_file`〕のみ。2 path 操作の resource 分割は後続「fs builtin 監査範囲」slice へ延期。下記「逸脱」
    参照。）
  - **R-AT-02（Deny 時の Path と raw 非露出）**: decision が `Deny` の host-call は **label が手元にある
    場合でも** Path field が `Omitted { class: Path }` になる。対象経路——(a) filesystem 未 grant、
    (b) path lexical/argument エラー、(c) mount 未登録・operation 未 grant・許可外、(d) **Upsert の
    Create 権限不足**（`fs_route` 成功で label 確定済みだが `Deny`）——いずれでも Path は `Omitted`。
    そのうえで **全経路で raw requested path も canonical label も payload（`resource`/`request`/error）に
    1 byte も現れない**（`Revealed` にも HMAC 入力にも出ない。§9.1）。(d) を「label があるのに Omitted」の
    テストケースとして明示的に含める。R-1 の不変条件は「Path は `Allow`（Success/HostError）で
    `HmacSha256`・`Deny` で `Omitted`（label の有無を問わず）、raw/label は Deny で常に非露出」。
    （`rename` は R-1 の監査対象外のため R-AT-02 の対象にも含めない。下記「逸脱」参照。）
  - **R-AT-03**: body（file 内容・args の自由文）が `Omitted { class: Body }` で、`original_bytes` に
    redaction 前の canonical byte 長が入る（prefix も残さない。§9.1）。
  - **R-AT-04**: `ExecutionStarted.redaction_policy_id` が注入 policy の `policy_id`（§9.1 の host 指定
    `String`）を**そのまま**反映する。既定 policy では `"default-r1"`、別 `policy_id` を注入すると
    その値になる（§7.1。内容ハッシュではなく host ラベルの反映）。
  - **R-AT-05（非破壊）**: sink なし経路の出力 byte・exit code が不変。既定 policy で既存
    `tests/audit_core.rs` / `tests/audit_hostcall.rs` / `tests/audit_yield_resume.rs` /
    `tests/audit_backpressure.rs` / `tests/audit_queue_bytes.rs` の **sequence・yield 位置・encoded
    推定長（128 固定）**が不変。これは R-1 が `estimate_event_bytes` を据え置くことから機械的に従う
    （payload 中身の型だけが変わり、見積もり長は payload 非依存の 128 固定のまま）。payload 構造・
    `redaction_policy_id`・`path.original_bytes` を検査する assertion は §9.1 ツリーへ期待値を更新する。
    万一 yield 位置が動く差分が出たら、見積もり据え置きが崩れている兆候なので実装を止めて報告する。
  - **R-AT-06（決定性）**: 同じ script・同じ `hmac_key`・同じ注入 policy では、redaction 結果と envelope
    列が 100 回の実行で一致する（§15.1）。既定 key は固定値で乱数生成しない（§7.1）ため HMAC は決定的。
  - **R-AT-07（Secret・§15.4 の compile-fail を含む）**: Secret class の field は存在と byte 数だけが
    記録され、値が payload・`Debug`/`Display` 出力・error message のいずれにも漏れない。加えて §15.4 の
    要求どおり `Secret` / `SecretBytes` が `Debug` / `Display` / generic serialize を**実装しない**ことを
    compile-fail で固定する。**枠組みは `trybuild` を使わず `compile_fail` doctest にする**（下記「逸脱」）。
  - **R-AT-08（byte 計上の不変）**: R-1 は見積もりを据え置くため、host-call event の push 時計上 byte
    （128 固定 + envelope 固定部）が A-1/B-2 と変わらず、journal の `committed_bytes` 増分とも一致し続ける
    （B2-AT-10 の不変条件を維持）。redaction 後 field を実サイズで測る正確化は strict CBOR スライスへ延期。
  - **R-AT-09（error message）**: Terminal/error の payload に安定 `ErrorCode` / `message_id` / source
    位置 / redacted parameter summary だけが入り、script-visible message がそのまま入らない（§9.1）。
  - VM は対象外（監査 sink と `VmExperimental` の同時設定は build 時に拒否）。
  - **範囲外（初版 R-AT-09「60 KiB 縮退」を削除）**: 60 KiB 縮退・`EventTooLarge` は正確な byte 推定が
    前提のため strict CBOR スライスへ延期し、R-1 の受入基準から外した（上記「逸脱」）。

- **テスト配置**: `tests/audit_redaction.rs`（R-AT-01〜04・06・09）。R-AT-05 は既存
  `tests/audit_*.rs` の sequence/yield 位置/encoded 推定長の不変 ＋ payload 期待値更新で担保。R-AT-07 の
  compile-fail は `src/audit.rs`（または `src/lib.rs`）の `compile_fail` doctest。R-AT-08 は
  `src/audit.rs` の unit test（見積もり据え置きの確認）。
  - **compile-fail doctest の偽陽性対策**: `compile_fail` doctest は「どんな理由で compile が落ちても成功」
    になるため、型が参照できない（非公開・パス誤記）だけで通ってしまう。これを防ぐため doctest は次の 2 本を
    1 ペアにする: (1) `compile_fail` 版——**公開経路から `SecretBytes` を参照**し（例えば
    `tsumugi::RedactionPolicy` 経由、または `SecretBytes` を公開 export して参照）、その値に
    `format!("{:?}", ..)` 等 `Debug` を要求して**落ちる**ことを固定する。(2) 通常 doctest 版——**同じ参照
    経路で、通るべき操作**（`Clone` や byte 長取得など `Debug` 以外）を実際に compile・実行し、「型には
    手が届くが `Debug`/`Display`/serialize だけが無い」ことを担保する。(2) が通ることで (1) の失敗が
    「型が見えない」ではなく「`Debug` が無い」由来であることを保証する。

- **R-1 レビュー反映の記録（採用・不採用とその理由）**: 初版の R-1 定義に対する semantic review を
  1 周通し、現行コード（`src/audit.rs` / `src/builtin_core.rs` / `src/embedding.rs` / `src/eval.rs`）と
  仕様 §7・§9〜§11・§15 を突き合わせた指摘を次のとおり処理した。将来この slice を実装・改訂する際の
  判断履歴として残す。

  **採用した指摘（定義を改めた）**
  - **byte 推定の非破壊根拠が成り立たない（最重要）**: 初版は「`original_bytes` を保てば encoded 長も
    yield 位置も不変」としたが、`estimate_event_bytes` は host-call 系を payload 非依存の 128 固定で
    見積もっており（`src/audit.rs` の `_ => 128`）、`original_bytes` は payload 中の値であって encode 後の
    長さではない。見積もりを payload 依存に変えると B-2 の queue 会計で yield 位置が動く。**対応**: R-1 では
    見積もりを**据え置く**（payload の中身だけ §9.1 ツリーへ差し替え、見積もり長は 128 固定のまま）。
    **据え置きを選んだ理由**: レビューは「書き直す／範囲から外す」の 2 択を許したが、naive 推定全体の
    正確化は既に延期済みの strict CBOR スライスが扱う領域で、そこへ寄せれば B-2 の回帰更新を 1 回で済ませ、
    R-AT-05 の非破壊が本当に成立する。見積もりを R-1 で変えると初版の自己矛盾に逆戻りする。
  - **「現状 payload は空」という事実誤認**: 実際は `host_call_triple`（`src/builtin_core.rs`）が `body`/
    `path` を `Omitted` にした field を既に積んでおり、空 `fields` は `eval.rs` のユニットテストだけ。
    **対応**: 冒頭記述を実態（`Omitted` プレースホルダ・`path.original_bytes` は 0 固定）へ修正。
  - **変更箇所 `src/builtin.rs` は `src/builtin_core.rs` の誤記**: **対応**: 修正。あわせて canonical
    resource label を `host_call_triple` へ渡す新規配線と、`path.original_bytes` を実 label 長へ改める
    作業を変更箇所へ明記（初版は欠落）。
  - **既定 `hmac_key` のジレンマ（セキュリティ判断）**: 固定埋め込みは公開鍵同然で §9.1 の辞書攻撃耐性が
    消え、乱数生成は §7.1（ambient 乱数源に触らない）と R-AT-06（決定性）を壊す。**対応**: R-1 の既定 key は
    固定値（alpha 用・秘匿性なし。raw path を直接載せないことだけを担保し、key 公開ゆえ推測復元には
    耐えない）と割り切り、秘匿性のある host 注入経路を sink 必須化スライスへ延期、と逸脱に明記。
    **この割り切りを選んだ理由**: R-1 の主目的は raw path をそのまま載せないことであり、固定 key でもそれは
    達成できる。推測復元に耐える秘匿性は公開 API（key 注入経路）の設計を伴うため sink 必須化と同じスライスに
    まとめる方が設計が一貫する（「相関除去」という当初表現は 3 周目で不正確と認め、下記のとおり言い直した）。
  - **公開 API の破壊的変更に触れていない**: `AuditPayload`/`AuditPayloadField` は `src/lib.rs` で公開
    export 済み。**対応**: 差し替えが破壊的変更であること、B-1 と同じく alpha として roadmap 明記の上
    受容し `language_revision` bump は無いことを明記。
  - **invalid-path 時の HMAC 対象が未定義**: **対応**: canonical label が無い場合は Path field を
    `Omitted{class:Path}` にし HMAC しない、を確定規則として追加（raw path は leak させない）。
  - **`policy_id` の決め方が曖昧**: **対応**: §9.1 の型どおり host 指定 `String`（内容ハッシュでない）と
    明記し、既定を `"default-r1"` に。R-AT-04 を「注入値をそのまま反映」に統一。
  - **R-AT-07 が §15.4 より弱い**: 初版は「値が漏れない」だけだった。**対応**: §15.4 の compile-fail
    （`Secret`/`SecretBytes` が `Debug`/`Display`/serialize 不可）を R-AT-07 に含め強度を揃えた。
  - **ロードマップの一貫性**: 初版は「直近完了」に未マージの slice 定義を入れ、「次のタスク」に既定モードの
    詳細を書いていた。**対応**: F-1 の docs PR に倣い「次のタスク」中心の更新へ寄せ、詳細は spec 参照に留める
    （本コミットで roadmap を再修正）。

  **不採用・範囲外にした指摘（理由つき）**
  - **60 KiB 縮退規則（初版 R-AT-09）を R-1 で実装する**: 縮退判定は event の実 encode 長に対する判定で、
    正確な byte 推定が前提。R-1 は見積もりを据え置くため faithful に実装できない。**不採用の理由**: 見積もり
    据え置きと両立しないため strict CBOR スライスへ延期し、受入基準から外した（レビューの指摘 1 対応の
    論理的帰結）。
  - **見積もり関数を「変わる前提に書き直す」案（指摘 1 の第一選択肢）**: レビューが許した 2 択のうち
    採らなかった方。**不採用の理由**: 上記「据え置きを選んだ理由」のとおり、strict CBOR スライスへ寄せる方が
    B-2 回帰を 1 回で済ませられ非破壊が成立するため。

  **2 周目レビューで採用した指摘（上記定義へ反映済み）**
  - **R-AT-02 と invalid-path ルールの矛盾**: 初版改訂で invalid-path を `Omitted` にしたのに R-AT-02 は
    「deny でも Path は HmacSha256」のままだった。コード（`dispatch_fs`/`dispatch_filesystem_capability`）を
    確認すると label が無い deny は invalid-path だけでなく「未 grant」「mount 未登録・未 grant・許可外」も
    該当。**対応**: canonical label が得られるのは allow 時だけと一般化し、R-AT-01（allow=HmacSha256）と
    R-AT-02（deny 3 経路=Omitted・raw 非露出）に書き分けた。
  - **label の渡し方がコード構造と不整合**: triple は `dispatch_fs` が返った後に `fs_result` から組み立てて
    おり、label は `dispatch_fs` 内に閉じている。**対応**: `dispatch_fs` の戻り値を label 同伴型へ拡張し
    呼び出し側が triple へ渡す配線を変更箇所に明記（非監査経路は label を無視し観測挙動不変）。
  - **trybuild ハーネスが存在しない**: dev-deps は criterion のみ。3 OS CI で trybuild の stderr snapshot は
    壊れやすい。**対応**: R-AT-07 の compile-fail を `trybuild` ではなく `compile_fail` doctest で固定する
    方針に変更（依存を増やさず OS 差に強い）。
  - **`SecretBytes` の Debug 未実装で `Engine`/`EngineBuilder` の `#[derive(Debug)]` が壊れる**: **対応**:
    `RedactionPolicy` に key を伏せた手書き `Debug` を実装し derive を保つ、と変更箇所に明記。`SecretBytes`
    自体は `Debug`/`Display`/serialize 非実装（R-AT-07 の compile-fail で固定）。
  - **128 固定見積もりは上限保証でない**: payload が HMAC digest/Identifier の実データを含むと 128 を
    超えうる。**対応**: `max_encoded_bytes`/60 KiB 判定が strict CBOR まで概算依存（過小評価あり）である旨を
    逸脱に明記（R-1 の payload 規模では実害なし）。
  - **「相関除去」は不正確**: 固定 key では同一 label→同一 digest で相関は残り、key 公開なので path は推測
    復元されうる。**対応**: 既定 key の目的を「raw path をそのまま載せない」に正し、推測復元に耐える秘匿性は
    得られないと明記。
  - **記録場所の表現ミス**: 導入文の「§14 末尾」を「本小節末尾」に修正。

  **3 周目レビューで採用した指摘（上記定義へ反映済み）**
  - **host error 経路で label を取りこぼす**: 「allow なら `Some`、deny なら `None`」では
    `HostCallOutcome::HostError`（route/open 成功後に adapter が失敗。`host_call_triple` の分類で
    Allow/`HostError`、`fs_result` は `Err`）が抜ける。コード（`builtin_core.rs` の fs operation は
    `fs_open`/`fs_route` 成功後に `read_to_end`/`write_all` 等が失敗し `fs_host_error`）で確認。
    **対応**: label の有無は decision で決まり outcome の成否と独立と整理し、`dispatch_fs` 戻り値を
    `Err` でも label を運ぶ型へ拡張、Path は Allow（Success/HostError 両方）で `HmacSha256`。R-AT-01 に
    host error ケースを明示的にテスト対象として追加。
  - **compile_fail doctest の偽陽性**: 型が参照できない（非公開・パス誤記）だけで compile が落ちても
    `compile_fail` は成功になる。**対応**: (1) 公開経路から `SecretBytes` を参照して `Debug` 要求で落ちる
    `compile_fail` 版と、(2) 同じ経路で `Debug` 以外（`Clone` 等）が通る通常 doctest 版を 1 ペアにし、
    失敗が「型が見えない」でなく「`Debug` が無い」由来であることを担保する、とテスト配置に明記。
  - **直し漏れ・不整合**: (a)「逸脱」節の `hmac_key` に「相関除去のみ」が残っていた → 「raw path を直接
    載せない／推測復元には耐えない」に統一。(b)「opt-in・非破壊」節の Path 記述が旧ルール（常に
    `HmacSha256`）のまま → Allow=`HmacSha256`/Deny=`Omitted` に、`path.original_bytes` も Allow/Deny/
    引数型エラー別の実長に修正。(c) `original_bytes` の定義ぶれ（§9.1 の canonical subtree 長 vs Path の
    label/path byte 長）→ strict CBOR までは UTF-8 byte 長で代用すると一文で固定。(d) 引数型エラー
    （`require_str` 失敗で path 文字列が無い）の `original_bytes` 未定義 → `0` と定義。
  - **canonical resource label の中身（好みの範囲・採用）**: R-AT-01 の「同じ label」を明確にするため、
    label を routing 後の `FilesystemTarget`（mount 名＋mount 相対 path を `@<mount>/...` 正規形へ）と
    一文で固定した。host absolute path・CWD・symlink 解決結果は含めない。

  **4 周目レビューで採用した指摘（上記定義へ反映済み、いずれも書き方の精密化）**
  - **label 確定は「route 成功時点」であって「open の後」ではない**: `fs_route` が `FilesystemTarget` を
    返した時点で label が決まり、`fs_open` 内の `adapter.open_file` 失敗はその後に起きる（`builtin_core.rs`）。
    **対応**: field class 判定と変更箇所を「route 成功時点で label 確定、open 失敗を含む host error でも
    label あり」に直し、実装は label を `fs_route` 直後に受け取ると明記。
  - **「Deny は label 確定前に拒否」は全経路には当てはまらない**: Upsert（`write_file`/`append_file`）の
    Create 権限不足は、`fs_route`（Write 代表 op）成功で label が決まった**後**に `filesystem_denied` に
    なる（`builtin_core.rs` の `fs_open`）。**対応**: Path のルールを「label の有無」ではなく
    「**decision が `Deny` なら label が手元にあっても `Omitted`**」という判定基準に書き直し（「label が
    あれば HMAC してよい」と読み違えるのを防ぐ）。R-AT-02 に Upsert 経路 (d) を「label があるのに Omitted」の
    テストケースとして追加し、`original_bytes` は Deny では requested path 長で統一（label 長を漏らさない）と
    定義。

  **5 周目レビューで採用した指摘（実装 PR #238 のレビュー反映、上記定義へ反映済み）**
  - **`arguments` field の形**: 初版実装は `arguments` を `Revealed { class: Body, value: Sequence([]) }`
    にしていたが、§9.1 で Body が `Revealed` になるのは allow-list に載り上限内のときだけであり、形の上で
    「Body を開示した」と読めてしまう（値は空なので leak はない）。**対応（R-1 の確定規則）**: host-call
    引数の `fields["arguments"]` は外側を `Revealed { class: Public, value: Sequence([...]) }`（開示するのは
    引数の個数と並びだけ）とし、各要素は引数の種類に応じて `Omitted { class: Path }`（path 引数）または
    `Omitted { class: Body }`（書き込み内容などの自由文）に畳む。path の HMAC は `resource` 側だけで持ち、
    `arguments` 側では HMAC しない。各要素の `original_bytes` はその引数の UTF-8 byte 長（R-1 の代用規則）。
    これにより R-1b の allow-list Reveal を要素単位で差し込める。
  - **`rename` の resource 2 field は R-1 から外し後続 slice へ延期**（6 周目で撤回・下記参照）: 5 周目で
    `rename` の resource を `source_path` / `destination_path` の 2 field にすると定義したが、これは
    「`rename` も監査 triple を出している」という誤前提に基づいていた。実際は triple を emission するのは
    I/O byte 課金対象の 4 builtin（`read_file` / `read_lines` / `write_file` / `append_file`）だけで、
    `rename` は初版（R-1 着手前）から監査対象外である。`rename` を監査対象へ追加すると emission 範囲
    （event 件数・sequence・queue byte 会計）が変わり、その変更が §7.2/§8/§14 に未記載になるうえ、
    capability で判定される他の fs builtin（`mkdir` / `remove` 系 / `list_dir` / `file_size` / `is_file` /
    `is_dir` / `path_exists`）が監査対象外のまま `rename` だけ対象になる非対称を生む。**対応**: `rename` の
    2 field 規則は R-1 の受入基準から外し、下記「逸脱」の「fs builtin 全体の監査範囲」slice へ延期する
    （その slice で `rename` を対象化するときの 2 field 設計は合意済みとして引き継ぐ）。
  - **Secret class の畳み規則を本スライスで入れる**: 初版実装は `AuditFieldClass::Secret` を扱う構築経路が
    無く、`AuditField::Revealed { class: Secret, .. }` を作れてしまった。R-AT-07 は「Secret class の field は
    存在と byte 数だけが記録される」を要求する。**対応**: class と mode から `AuditField` を組む小さな helper
    を `audit.rs` に置き、Secret は mode に関係なく常に `Omitted` にする。`host_call_triple` の body/path も
    この helper を経由させ、畳み規則を 1 か所へ集約する。unit test を添える。
  - **`identifier_mode` を payload へ反映する（ユーザー判断 A＝(イ)）**: slice 定義の「function 名・capability
    名・rule ID は既定 `Reveal`（`Revealed { class: Identifier, .. }`）」を実装にも反映する。初版は
    function/capability を event の素の String のままにしていた（payload の Identifier 二重化なし）。**対応**:
    host-call payload 側にも既定 policy の `identifier_mode` を適用した `Revealed { class: Identifier, value,
    original_bytes }` を載せ、slice 文面と実装を一致させる。
  - **test shim の `debug_assert!(reveal_body_fields.is_empty())` を撤去**: allow-list 非空 policy を注入
    すると debug build で panic し、ホスト安定性原則に反する。**対応**: assert を外し、「R-1 は allow-list を
    無視して常に Omit する」ことをコメントで明記する（R-1b で Reveal 経路を有効化する）。
  - **公開面の整理**: `RedactionPolicy::hmac_path` は spec に無い HMAC 計算の口なので `pub(crate)` にする。
    `path_mode` が HMAC 以外のとき `Omitted` へ畳む fail-safe 挙動を `RedactionPolicy` の doc に明記する。

  **6 周目レビューで採用した指摘（実装 PR #238 の 2 周目レビュー反映、上記定義へ反映済み）**
  - **`rename` の監査対象化を R-1 から外す（1 周目指摘 4 の誤前提の訂正）**: 1 周目レビューの指摘 4
    （`rename` の resource を 2 field に）は「`rename` も監査 triple を出している」という前提だったが、これは
    誤りだった。初版（R-1 着手前）から triple を emission するのは I/O byte 課金対象の 4 builtin
    （`read_file` / `read_lines` / `write_file` / `append_file`）のみで、`rename` は監査対象外である。
    2 周目修正で `is_audited_host_call` を新設して `rename` を監査対象へ追加したが、これは R-1 の範囲
    （既存 payload の中身を §9.1 ツリーへ差し替える最小縦切り）を超える emission 範囲の変更で、§7.2/§8/§14
    に未記載であり、他の capability 判定 fs builtin が対象外のまま `rename` だけ対象になる非対称も生む。
    **対応（ユーザー決定）**: `is_audited_host_call` を撤去し `is_host_call` の元判定へ戻す。`rename` 専用に
    入れた label 運搬（`AuditResourceLabels::Rename` など）と `rename` テストも R-1 から外す（未使用コードを
    残さない。単一 path 操作の label 運搬は R-1 に必要な形へ整理する）。5 周目で R-AT-01/02・本記録に書いた
    `rename` 2 field 規則は受入基準から外し、下記「逸脱」の「fs builtin 全体の監査範囲」slice へ移す。
  - **`arguments` と Identifier field のテスト追加**: `arguments` の構造（外側 `Revealed { class: Public }`・
    各要素の class・`original_bytes`）と payload に載った Identifier field（request の `function`、resource の
    `capability` / `rule_id` / `action`）を検査するテストが無かった。**対応**: `src/builtin_core.rs` の
    `host_call_triple_tests` に read 系（要素 1 つ）と write 系（Path + Body の 2 要素）のケースを
    追加し、raw path や書き込み内容が要素に出ないことも確認する。

#### R-1b 実装スライス（tree engine、opt-in、body allow-list Reveal）

> 状態: **未実装**（設計のみ）。本小節は R-1a が型だけ用意して常に `Omitted` に畳んでいる body
> allow-list Reveal 経路（`reveal_body_fields` と `max_revealed_body_bytes > 0` による `audit_safe`
> field の明示開示）を有効化する最小縦切り（R-1b）の slice 定義である。仕様（本章 §9/§9.1）が正本で
> あり、以下は R-1b で実装する範囲と意図的に延期する逸脱の記録である。R-1a 小節はその時点の記録として
> 触らない（過去のスライス小節を後から直さない）。現在の逸脱・未実装範囲は冒頭の逸脱一覧が正本であり、
> R-1b の done 実装 PR で body allow-list Reveal を未実装範囲から外す。

R-1a は §9.1 の `AuditField` ツリーと `RedactionPolicy`（`reveal_body_fields` /
`max_revealed_body_bytes` を含む）を型として用意したが、body を扱う畳み規則
（`src/audit.rs::fold_audit_field` の `AuditFieldClass::Body` 分岐）は `body_mode` に関わらず
**常に `Omitted`** にしており、allow-list を無視している（R-1a の確定規則・コメントで明記済み）。
R-1b はこの 1 か所の畳み規則を §9.1 の Body Reveal 条件へ差し替え、allow-list に載った body field を
上限内でだけ `Revealed` にする。R-1a と同じ opt-in（sink あり経路）・tree backend だけを対象とし、
新しい公開型・CBOR tag・`language_revision` の変更は行わない。

- **粒度は R-1b（body Reveal 経路の有効化のみ）**: R-1a が常に `Omitted` に畳んでいた Body class を、
  §9.1 の条件——`body_mode == Reveal` **かつ** field の `AuditFieldKey` が `reveal_body_fields` に
  ある **かつ** `original_bytes <= max_revealed_body_bytes`（`max_revealed_body_bytes > 0`）——を
  すべて満たすときだけ `Revealed { class: Body, value, original_bytes }` にする。1 つでも満たさなければ
  R-1a と同じく `Omitted { class: Body, original_bytes }` のままとする。filesystem host-call
  （tree 唯一の監査対象境界、R-1a と同じ 4 builtin〔`read_file` / `read_lines` / `write_file` /
  `append_file`〕）の `request` body・`response` body・`arguments` 各要素（自由文の書き込み内容）を対象に
  する。path 引数（`AuditFieldClass::Path`）・Secret・Identifier・Public の畳み規則は R-1a のまま一切
  変えない。

- **allow-list のキー（§9.1 どおり、新設しない）**: Reveal 可否は §9.1 の `AuditFieldKey`
  （`{ operation: String, direction: AuditDirection, field: String }`）で引く。R-1b はこの型を
  そのまま使い、新しいキー型を作らない。各単位の割り当ては次のとおり固定する。
  - `operation` = host function 名（R-1a が `request.function` / `resource.action` に載せている
    Identifier と同じ文字列。`read_file` / `read_lines` / `write_file` / `append_file`）。
  - `direction` = その body がどちらの payload に現れるかで決める。`request` body と `request` の
    `arguments` 要素は `AuditDirection::Request`、`response` body は `AuditDirection::Response`。
  - `field` = payload の key。`request`/`response` の自由文 body は `"body"`、`arguments` の各要素は
    §9.1 の「name または 0 始まり argument index」どおり **0 始まりの index の 10 進文字列**
    （`"0"`、`"1"`、…）とする。R-1a の `arguments` は arg[0]=path・arg[1]=書き込み内容なので、Body の
    Reveal 対象になり得るのは write/append の `"1"` だけである（arg[0] は Path class で対象外）。
  - **根拠**: §9.1 は `AuditFieldKey` を allow-list の要素型として既に定め、`field` を「name または
    0 始まり argument index」と規定している。R-1b はこの規定を host-call payload の実際の key 名
    （`"body"`）と `arguments` の index 文字列へ機械的に対応づけるだけで、新たな識別子体系を導入しない。
    operation に host function 名を使うのは、R-1a が同じ名前を payload の Identifier として既に露出して
    いるため、host が allow-list を書くときに参照できる安定ラベルになるからである。

- **`max_revealed_body_bytes` の比較対象と上限超過時の表現（§9.1）**: 比較対象は当該 field の
  `original_bytes`、すなわち **redaction 前の canonical subtree 長**（R-1a の代用規則どおり、strict CBOR
  スライスまでは値の UTF-8 byte 長で代用する。body は原本文字列の byte 長）とする。`original_bytes >
  max_revealed_body_bytes` の body は §9.1 どおり **truncation せず subtree 全体を `Omitted { class:
  Body, original_bytes }`** にする。prefix も 1 byte も残さない（prefix を残すと secret が漏れうるため）。
  §9.1 の「`truncated = true` ではなく `omitted = true` と byte 数を記録する」は、R-1a の `AuditField`
  表現では「variant が `Omitted` であること（truncation variant を持たない）」と「`original_bytes` に
  原本 byte 長が入ること」で満たす。`max_revealed_body_bytes` の既定は 0 なので、既定 policy では
  `original_bytes <= 0` を満たす body が無く（空 body の `original_bytes` も 0 だが、空 body は
  allow-list に載っても Reveal する値が無い）、実質すべて `Omitted` のままになる。
  - **根拠**: §9.1 が「上限を超えた body は truncation せず全体を omit」「prefix だけを残して secret を
    漏らさない」と明記している。比較対象を `original_bytes`（redaction 前の subtree 長）にするのは、
    §9.1 が Body Reveal の条件を「上限内」と定め、`original_bytes` を「redaction 前の canonical subtree
    payload 長」と定義しているため、両者の突合が仕様の意図どおりになる。

- **class 間の優先順位（§9.1、allow-list より class 規則が優先）**: Reveal 条件を満たしても、次の
  class 規則が allow-list より優先し、body Reveal には **到達しない**。
  - **Secret は常に `Omitted`**（mode 変更不可・走査しない、§9.1）。allow-list に載っていても Secret
    class の field は `fold_audit_field` の Secret 分岐で先に `Omitted` になる（R-1a の畳み順を変えない）。
  - **Path は decision 由来の規則が優先**（Allow=`HmacSha256` / Deny=`Omitted`、R-1a の R-AT-01/02）。
    `arguments` の path 引数（arg[0]）は Path class なので、allow-list に `"0"` を載せても body Reveal の
    対象にならない（Path 分岐で処理され Body 分岐へ来ない）。
  - **64 段を超える nesting は `Body`/`Omitted` へ縮退**（§9.1）。縮退した subtree は Body class の
    `Omitted` になり、allow-list に載っていても Reveal しない（縮退先が `Omitted` 固定であることは §9.1 の
    既定であり、R-1b はこれを Reveal へ昇格させない）。R-1a の filesystem payload は `arguments` が 1 段
    （`Sequence` の要素はスカラ）なので 64 段には達しないが、縮退規則と allow-list の優先関係を本スライスで
    確定しておく。
  - 優先順位をまとめると **Secret > Path（decision 規則）> 64 段縮退 > body allow-list Reveal** であり、
    body Reveal は「Body class として `fold_audit_field` の Body 分岐に到達し、かつ §9.1 の 3 条件を満たす」
    ときだけ起きる。
  - **根拠**: §9.1 は「Secret の親は必ず subtree 全体を `Omitted`」「Body は `body_mode == Reveal` かつ
    key が allow-list にあり上限内の場合だけ `Revealed`」「64 段を超える subtree は Body/Omitted へ縮退」
    と、class 規則を Body Reveal 条件の前段として規定している。R-1b はこの順序をそのまま実装し、allow-list
    を class 規則の後ろに置く。

- **`redaction_policy_id` の扱い（R-1a の実配線を踏襲）**: `ExecutionStarted.redaction_policy_id` は
  R-1a どおり注入 policy の `policy_id`（§9.1 の host 指定 `String`）を**そのまま**反映する。allow-list が
  非空の policy でも R-1b は `policy_id` の決め方・既定値・format に手を入れない（既定 policy は allow-list
  が空のままなので `"default-r1"` を維持する）。allow-list 非空 policy に特定の `policy_id` を要求するか、
  allow-list の内容を `policy_id` へ反映するか（例: 内容ハッシュを含める）は §9.1 の「`policy_id` は host が
  付ける安定ラベル・内容ハッシュではない」に反し得る API/format の約束になるため、本スライスでは決めず
  PR 本文の「判断待ち」へ回す。
  - **根拠**: §9.1 が `policy_id` を「host が付ける安定ラベル（内容ハッシュではない）」と定義し、R-1a の
    R-AT-04 が「注入値をそのまま反映」を確定している。R-1b は Body の畳み規則だけを変え、この契約は維持する。

- **policy 注入経路は R-1a のまま（公開 setter を新設しない）**: R-1b は facade の既定 policy
  （`RedactionPolicy::default()`、allow-list 空・`max_revealed_body_bytes = 0`）を変えない。allow-list を
  非空にした policy を注入する経路は、R-1a が置いた `#[doc(hidden)] pub` の test shim
  （`EngineBuilder::audit_redaction_policy_for_test`）をそのまま使い、受入基準のテストもこの shim 経由で
  allow-list 入り policy を注入する。host が production で allow-list 付き policy を設定する**公開 API**
  （`EngineBuilder::audit_redaction_policy` などの安定 setter）は、選択肢があり公開契約を増やすため、
  R-1a と同じく sink 必須化（§1.1）スライスへ延期し、R-1b では決めない（詳細は PR 本文「判断待ち」）。
  - **根拠**: R-1a の slice 定義が「公開 setter は追加しない・sink 必須化スライスへ延期」「test shim で
    注入」を確定している。R-1b は Body Reveal の意味論を有効化する縦切りであり、公開 API の設計を伴わない。

- **opt-in・非破壊（既定 policy で R-1a と 1 byte も変わらない）**: sink なし経路（`run_guarded`）は
  redaction を一切行わず、観測結果・出力 byte・exit code は不変（R-1a のまま）。sink あり経路でも、既定
  policy（allow-list 空・`max_revealed_body_bytes = 0`）では §9.1 の Reveal 条件を満たす body が存在せず、
  `fold_audit_field` の Body 分岐は R-1a と同じ `Omitted` を返す。したがって既定 policy での payload・
  `redaction_policy_id`・`original_bytes`・envelope 列は R-1a と **byte 単位で完全一致**する（R1b-AT-06 で
  固定）。挙動が変わるのは allow-list を非空にした policy を test shim 経由で注入したときだけで、その場合も
  R-1a の byte 推定据え置き（host-call event は 128 固定見積もり）を維持するため、sequence・yield 位置・
  B-2 の queue byte 会計は不変である（payload の**中身**だけが `Omitted` から `Revealed` へ変わる）。

- **byte 推定は R-1b でも据え置く（§9.2、非破壊の前提）**: R-1b は `estimate_event_bytes`
  （`src/audit.rs`）を**変更しない**（R-1a と同じ理由。host-call lifecycle event は payload 非依存の
  128 固定見積もりのまま）。Body を Reveal しても見積もり長は 128 固定で動かないため、B-2 の queue byte
  会計・yield 位置は allow-list の有無に関わらず不変になる。payload を実サイズで見積もる正確化は strict
  deterministic CBOR（§9.2）スライスへ延期する（R-1a と同じ割り切り）。

- **逸脱（R-1b 時点で残るもの、後続スライスへ延期）**:
  - **公開 setter**（`EngineBuilder::audit_redaction_policy`）と `ConfigError` への接続は sink 必須化
    （§1.1）スライスへ延期する（R-1a と同じ）。R-1b の allow-list 注入は test shim 経由だけ。
  - **allow-list 非空 policy の `policy_id` 契約**（特定値の要求・内容反映の有無）は決めず、PR 本文の
    「判断待ち」へ回す。R-1b は R-1a の「注入値をそのまま反映」を維持する。
  - **byte 推定の正確化**（`estimate_event_bytes` を payload 依存の実 encode 長へ）は strict CBOR
    （§9.2）スライスへ延期する。Reveal された body が 128 を超える encode 長になっても、R-1b は見積もりを
    据え置くため §11 の `max_encoded_bytes` / 60 KiB 判定は 128 概算に対して行われ、strict CBOR まで
    不正確なまま（R-1a と同じ逸脱）。
  - **60 KiB 縮退規則**（§9 の Body → Path → Identifier 縮退・`EventTooLarge`）は正確な byte 推定が
    前提のため strict CBOR スライスへ延期する（R-1a と同じ）。Reveal した body が大きい場合でも R-1b は
    `max_revealed_body_bytes` 上限（超過時は全体 `Omitted`）で有界化し、縮退規則には依存しない。
  - **fs builtin 全体の監査範囲**（`rename` ほか非課金 fs builtin への監査 triple 拡張）は別 slice
    （R-1a の逸脱と同じ「fs builtin 監査範囲」slice）。R-1b は R-1a と同じ 4 builtin だけを対象とする。
  - **VM 対応**（Slice 6）。監査 sink と `VmExperimental` の同時設定は従来どおり build 時に拒否する。
  - **record/replay の redaction 再適用**（§12）は record/replay スライス（§6）へ延期する。

- **変更箇所（見込み）**:
  - `src/audit.rs`: `fold_audit_field` の `AuditFieldClass::Body` 分岐を、§9.1 の 3 条件
    （`body_mode == Reveal` ∧ key ∈ `reveal_body_fields` ∧ `original_bytes <= max_revealed_body_bytes`
    かつ `max_revealed_body_bytes > 0`）を満たすとき `Revealed { class: Body, value, original_bytes }`、
    満たさなければ `Omitted { class: Body, original_bytes }` にする。allow-list 判定に必要な
    `AuditFieldKey`（`operation` / `direction` / `field`）と Reveal する `value` を呼び出し側から
    受け取れるよう、Body 用の畳み口（`fold_audit_field` の引数追加または Body 専用 helper）を整える。
    Secret / Path / 縮退の畳み規則は変えない。`RedactionPolicy` の Body Reveal 条件を doc に明記する。
  - `src/builtin_core.rs`: `host_call_triple` の `body_field` helper が、`request`/`response`/`arguments`
    の各 body について `AuditFieldKey`（host function 名・direction・key 名または index 文字列）と
    原本の `AuditValue`（Reveal される場合に載せる自由文の `AuditScalar::Utf8` 等）を組んで Body の
    畳み口へ渡すようにする。allow-list に無い・上限超過・既定 policy のときは従来どおり `Omitted`。
    path 引数（Path class）・Identifier・Public の経路は変えない。
  - `src/embedding.rs` / `src/eval.rs`: 変更なし（policy は R-1a の受け渡し経路をそのまま使う。
    test shim・`redaction_policy_id` 配線は R-1a のまま）。

- **受入基準（R-1b）**:
  - **R1b-AT-01（allow-list + 上限内で Reveal）**: `body_mode = Reveal`・当該 body の `AuditFieldKey` を
    `reveal_body_fields` に含め・`max_revealed_body_bytes` を body 長以上にした policy を注入すると、その
    body field が `Revealed { class: Body, value, original_bytes }` になり、`value` に原本の自由文が、
    `original_bytes` に redaction 前の byte 長（R-1b の代用規則＝UTF-8 byte 長）が入る（§9.1）。
  - **R1b-AT-02（上限超過は全体 Omit・truncation しない）**: `original_bytes > max_revealed_body_bytes`
    の body は、allow-list に載り `body_mode = Reveal` でも `Omitted { class: Body, original_bytes }` に
    なる。prefix を含め原本の byte が payload に 1 byte も現れず、`original_bytes` に原本長が入る（§9.1
    「truncation せず全体 omit・prefix を残さない」）。
  - **R1b-AT-03（allow-list 外は Omit）**: `AuditFieldKey` が `reveal_body_fields` に無い body は、
    `body_mode = Reveal`・上限内でも `Omitted { class: Body }` のままになる。`direction` 違い・`field`
    違い・`operation`（host function 名）違いのいずれでもキーが一致しなければ Reveal しない（§9.1）。
  - **R1b-AT-04（class 優先順位）**: allow-list に載っていても (a) Secret class は常に `Omitted`、
    (b) `arguments` の path 引数（arg[0]＝Path class）は decision 規則（Allow=`HmacSha256`/Deny=`Omitted`）に
    従い body Reveal に到達しない、(c) 64 段を超える nesting で Body/Omitted へ縮退した subtree は Reveal
    しない。優先順位 Secret > Path > 64 段縮退 > body allow-list Reveal を満たす（§9.1）。
  - **R1b-AT-05（`body_mode != Reveal` では Reveal しない）**: allow-list に載り上限内でも、
    `body_mode = Omit`（既定）または `body_mode = HmacSha256` の policy では body が `Revealed` にならない
    （§9.1 の 3 条件は `body_mode == Reveal` を含む）。既定 policy（`body_mode = Omit`）がこのケースを兼ねる。
  - **R1b-AT-06（既定 policy で R-1a と byte 一致・非破壊）**: 既定 policy（allow-list 空・
    `max_revealed_body_bytes = 0`・`body_mode = Omit`）では、payload・`redaction_policy_id`・
    `original_bytes`・envelope 列・sequence・yield 位置が R-1a と **1 byte も変わらない**。既存
    `tests/audit_redaction.rs` / `tests/audit_core.rs` / `tests/audit_hostcall.rs` /
    `tests/audit_yield_resume.rs` / `tests/audit_backpressure.rs` / `tests/audit_queue_bytes.rs` が
    無改変で緑。sink なし経路の出力 byte・exit code が不変。既存テストの期待値が変わる場合は実装を止めて
    報告する。
  - **R1b-AT-07（決定性）**: 同じ script・同じ `hmac_key`・同じ allow-list 入り policy では、Reveal 結果と
    envelope 列が 100 回の実行で一致する（§15.1）。Reveal は原本値をそのまま載せるだけで乱数源に触れない。
  - **R1b-AT-08（byte 計上の不変）**: R-1b は `estimate_event_bytes` を据え置くため、allow-list の有無・
    Reveal の有無に関わらず host-call event の push 時計上 byte（128 固定 + envelope 固定部）が R-1a/B-2 と
    変わらず、journal の `committed_bytes` 増分とも一致し続ける（B2-AT-10 の不変条件を維持）。
  - VM は対象外（監査 sink と `VmExperimental` の同時設定は build 時に拒否）。

- **テスト配置**: `tests/audit_redaction.rs`（R1b-AT-01〜05・07 を allow-list 入り policy の test shim
  注入で追加）。R1b-AT-06 は既存 `tests/audit_*.rs` の無改変緑 ＋ 既定 policy payload の期待値不変で担保。
  R1b-AT-08 は `src/audit.rs` の unit test（見積もり据え置きと Body 畳み規則）で担保。allow-list 判定・
  class 優先順位の畳み規則は `src/audit.rs::fold_audit_field` と `src/builtin_core.rs::host_call_triple_tests`
  の unit test に Reveal/Omit の分岐ケースを追加する。

#### audit-strict-cbor の sub-slice 分割

> 状態: 設計のみ（本小節以下 4 小節は設計時点の記録）。現在の逸脱・未実装範囲は冒頭の逸脱一覧が正本である。

`slices.toml` の `audit-strict-cbor`（strict deterministic CBOR、§9.2）は、次の 4 つを含む。

1. §9.2 の encoder 本体（schema v1 の全型の deterministic CBOR と、§11 の size pass / write pass）
2. journal・audit drain queue の byte 計測を naive 推定（`estimate_encoded_bytes` / `estimate_event_bytes`、host-call 系は 128 固定）から encoder の実長へ差し替えること
3. `original_bytes` と Path の HMAC 入力を §9.1/§9.2 の「redaction 前 subtree の canonical encoding」へ揃えること
4. §7.2 の 60 KiB 縮退規則（Body → Path → Identifier の `Omitted` 化、`EventTooLarge`、Terminal の `Emergency` 置換）

まとめると `src/` の差分は 700 行を超え、触る仕組みも encoder・journal/queue 会計・redaction・縮退の 4 つになる。[開発ループ手順書](loop-runbook.md)「スライスの大きさの目安」（400 行・仕組み 1 つ・受入基準 5 項目）に収まらないため、c1〜c4 の 4 スライスに分ける。元の id `audit-strict-cbor` は置き換えて残さない。「strict CBOR スライス」と書かれた既存の延期先（R-1a/R-1b/B-2 小節、逸脱一覧）は c1〜c4 全体を指す。

| スライス | 仕組み | 依存 | 受入基準 |
|---|---|---|---|
| audit-strict-cbor-c1 | encoder 本体（`src/audit.rs` 内、crate 内部） | A-1 | SC-AT-01..05 |
| audit-strict-cbor-c2 | journal・queue の byte 計測の差し替え | c1 | SC-AT-06..10 |
| audit-strict-cbor-c3 | `original_bytes`・HMAC 入力の canonical encoding 化（redaction） | c1 | SC-AT-11..15 |
| audit-strict-cbor-c4 | 60 KiB 縮退規則と Terminal `Emergency` | c2・c3 | SC-AT-16..20 |

c1 は byte 計測にも redaction にも配線しないため、c1 だけでは観測できる挙動は変わらない。挙動（`committed_bytes`・yield 位置・digest・`original_bytes`）が変わるのは c2 以降である。

#### audit-strict-cbor-c1 実装スライス（§9.2 encoder 本体と size pass）

> 状態: **未実装**（設計のみ）。仕様（§9.1・§9.2・§11）が正本であり、以下は c1 で実装する範囲と延期する範囲の記録である。

- **実装範囲**:
  - §9.2 の deterministic CBOR encoder を `src/audit.rs` の中に置く（新しい依存 crate は足さない。SHA-256/HMAC と同じく自前実装）。primitive（unsigned/negative integer の最短幅、定長 text/byte string、array header、bool/null）と、§9.2 の合成規則（struct = 宣言順 array、enum = `[tag, ...]`、`Option` = `[0]` / `[1, v]`、`BTreeMap` = `[[k, v], ...]`、Vec = 元順、`i128` = 16 byte BE byte string、Float = 8 byte BE byte string、u128 の `execution_id` = 16 byte BE byte string、`MonotonicInstant` = u64 tick のみ）を実装する。`HostTimestamp` は field 1 つの struct なので、§9.2 の一般規則どおり 1 要素の array `[bstr(16)]`（array header 1 + byte string header 1 + 16 = 18 byte 固定）として encode する。
  - schema v1 の全型に encode を与える: `AuditEnvelope`・`HostTimestamp`・`AuditEvent`（8 variant、tag 0〜7）・`AuditYieldReason`（`AdmissionPhase` を含む）・`Backend`・`ExecutionMode`・`CapabilityDecisionKind`・`HostCallOutcome`・`EffectStatus`・`BudgetChargeReason`・`BudgetResource`・`TerminalOutcome`・`BudgetConfig`・`BudgetUsage`・`BudgetCounters`・`BudgetPeaks`・`AuditErrorPayload`・`AuditError`・`AuditFrame`・`AuditPayload`・`AuditField`・`AuditFieldClass`・`AuditValue`・`AuditScalar`。tag は各 enum の Rust 宣言順（§7.2・§9.1 の宣言順と同じ）を 0 始まりで振る。fieldless enum（`AuditFieldClass` など。field を持つ variant は常に `[tag, field...]`）は §9.2 の一般規則どおり `[tag]` で encode する（下記の判断 Q1 = A）。`BudgetConfig` 系の field 順は [実行予算・協調実行仕様](execution-control.md) の宣言順（現行 `src/budget.rs` と一致）を使う。
  - §11 の 2 pass を 1 つの encoder で持つ。同じ走査を「長さだけ数える出力」（size pass）と「`Vec<u8>` へ書く出力」（write pass）に通す形にし、size pass は checked arithmetic で長さを返す（overflow は `None` / error）。別実装の size 推定を持たない（§9.2 末尾）。
  - 入口は crate 内部（`pub(crate)`）の 2 関数程度に絞る（例: envelope を encode して bytes を返す関数、event 本体と envelope 固定部の長さを返す関数）。c1 時点では本番経路から呼ばれないため、未使用警告を c1 の間だけ局所的に許す（`#[cfg_attr(not(test), expect(dead_code))]` など。c2 で使われた時点で外れる書き方にする）。
- **範囲外（逸脱・延期）**:
  - journal・queue の byte 計測の差し替えは c2。c1 の間は逸脱一覧の「128 固定見積もり」行と未実装範囲の「strict deterministic CBOR」行はそのまま残る（c1 の done PR では逸脱一覧を変えない）。
  - `original_bytes`・HMAC 入力は c3、60 KiB 縮退と `Emergency` は c4。
  - **encoder の公開**（host が envelope の canonical bytes を得る公開関数、sink へ bytes を渡す API）は行わない。`AuditSink` は従来どおり `Arc<[AuditEnvelope]>` を受け取る。公開面は sink 必須化（§1.1）または record/replay（§6）のスライスで、record store の fingerprint（§9.2 `RecordedHostRequest`）とあわせて決める。`RecordedHostRequest` の encode も c1 の範囲外（record/replay スライス）。
  - `AuditFrame.module_id` / `AuditError.module_id` は現行の型（`String` / `Option<String>`）のまま encode する（逸脱一覧の該当行）。`AuditField` への型変更は c1 の範囲外。
  - VM は対象外。
- **判断 Q1（kamonabe の選択: A）**: §9.1 の class（`AuditFieldClass` などの field を持たない enum。`AuditField` の variant は field を持つので常に `[tag, field...]` で、Q1 の対象外）を、§9.2 の一般規則どおり `[tag]`（array、例 `0x81 0x00`、2 byte）で encode するか、§9.1 の文言どおり bare u8（例 `0x00`、1 byte）で encode するか。変更前の spec は 2 通りに読め、どちらを採るかで全 `AuditField` の byte 列・`original_bytes`・digest・journal の byte 会計が変わる。
  - A（推奨）: 一般規則どおり `[tag]`。例外の無い 1 つの規則で済み、encoder も検証も単純になる。根拠: manifesto 原則 8（小さく理解可能な中核）・原則 5（予測可能性）、§9.2 の一般規則。
  - B: §9.1 の文言どおり bare u8。`AuditField` 1 つあたり 1 byte 小さいが、fieldless enum の中で class だけが例外になる。根拠: §9.1 の文言。
  - **選択: A**（kamonabe の選択、2026-10-24、PR #256）。§9.1 と本小節の実装範囲へ反映した。
- **受入基準（c1）**:
  - **SC-AT-01（primitive の golden）**: unsigned integer の幅の境界（0・23・24・255・256・65535・65536・2^32−1・2^32・u64::MAX）、負の i64（−1・−24・−25・i64::MIN）、空と非空の text / byte string、`Option` の `None` / `Some`、`i128`（0・−1・i128::MIN・i128::MAX）の 16 byte BE、Float bits の 8 byte BE が、手で組んだ期待 byte 列と一致する（RFC 8949 §4.2.1 の最短幅・定長）。
  - **SC-AT-02（schema v1 の全型の golden）**: `AuditEvent` の 8 variant と、上記の全 enum の全 variant について、§9.2 の tag と field 順で組んだ期待 byte 列と一致する。`AuditEnvelope` は `[schema_version, execution_id(16), source_hash(32), language_revision, sequence, timestamp, event]` の順になる。
  - **SC-AT-03（size pass と write pass の一致）**: SC-AT-01/02 の全ケースと、極値（全 u64 field が u64::MAX、長い text、64 段 nesting の `AuditValue`）で、size pass の長さが write pass の byte 長と一致する。size pass の加算は checked で、overflow を検出できる（unit test で overflow 経路を直接呼ぶ）。
  - **SC-AT-04（決定性）**: 同じ値は 100 回 encode しても同じ byte 列になる。`BTreeMap` / `AuditValue::Object` は挿入順に関係なく UTF-8 key byte 順で出る。clock instance だけが異なる（tick は同じ）`MonotonicInstant` を含む `BudgetConfig` は同じ byte 列になる（§9.2・§15.1）。
  - **SC-AT-05（非破壊）**: 既存の `tests/audit_*.rs` が無改変で緑。sink なし経路・sink あり経路とも観測結果（envelope 列・`committed_bytes`・yield 位置・出力 byte・exit code）が変わらない（c1 は計測にも redaction にも配線しない）。
- **想定の変更規模**: `src/audit.rs` に約 350〜400 行（primitive と 2 pass で約 100 行、残りは型ごとの定型の encode）。テストは `src/audit.rs` の unit test（golden の期待 byte 列）。他の `src/` ファイルは変えない。仕組みは encoder 1 つ、受入基準は 5 項目。
- **テスト配置**: SC-AT-01〜04 は `src/audit.rs` の unit test（`cbor_` 接頭辞）。SC-AT-05 は既存の `tests/audit_*.rs` の無改変緑。

#### audit-strict-cbor-c2 実装スライス（journal・audit drain queue の byte 計測を encoder の実長へ差し替え）

> 状態: **未実装**（設計のみ）。c1 が done になってから着手する。

- **実装範囲**:
  - journal（`AuditJournal::append_normal` / `append_terminal`）が計上する byte を、c1 の encoder で測った envelope の実 encode 長にする（§11「encoded byte数は9.2節のdeterministic CBORでencodeした実byte長」）。append の前に size pass で長さを求めて budget 判定に使い、write pass を行う場合はその長さと一致しなければ `AuditFailure::InvariantViolation`（internal audit failure、fail-closed）とする。
  - audit drain queue（B-2、`AuditQueueMeter`）は、sequence 確定前の event を「envelope 固定部の上限値（sequence を u64 の最大幅 9 byte とみなす。timestamp は `[bstr(16)]` の 18 byte 固定、`language_revision` は execution 内で不変）＋ event 本体の実 encode 長」で計上する（B-2 小節「逸脱」に書いた方針どおり）。ack 時は push 時に足した値と同じ値を引く。
  - `estimate_encoded_bytes` / `estimate_envelope_overhead_bytes` / `estimate_event_bytes` を encoder の size pass へ置き換える（関数名を残すかどうかは実装者に任せる。naive 推定の実装は削除する）。
  - `can_reserve_host_call` の 60 KiB / close reserve の判定は現行どおり（定数による上限値）とし、変えない。
- **範囲外（逸脱・延期）**: `original_bytes`・HMAC は c3、60 KiB 縮退は c4。§11 の pre-effect reservation（逸脱一覧の該当行）と関数内の queue soft limit（B-2 の逸脱）は変えない。VM は対象外。
- **c2 の done PR で行う逸脱一覧の更新**: 「128 固定見積もり」の行と未実装範囲の「strict deterministic CBOR」行を削除する。
- **受入基準（c2）**:
  - **SC-AT-06（journal の実長）**: Started・host-call triple・`BudgetCharged`・`Yielded`/`Resumed`・Terminal を含む実行で、journal の `committed_bytes`（Terminal は reserve 側）が各 envelope の c1 encoder による byte 長の和と一致する。Reveal された body を含む host-call event は payload に応じた長さで計上される（128 固定でない）。
  - **SC-AT-07（queue の上限値計上）**: B2-AT-10 を置き換える。各 event の push 時計上値は、同じ event を journal へ append したときの増分以上で、その差は sequence の encode 幅の差（0〜8 byte）に限られる。flush 後に queue は 0 へ戻る。
  - **SC-AT-08（実長での上限判定）**: `max_encoded_bytes` を小さく注入した実行で、実長の合計が通常枠を超えた時点で `AuditFailure::BudgetExceeded`、単一 event の実長が通常枠を超えると `EventTooLarge` になり、Terminal は予約枠で必ず append される（§10.1・§11）。
  - **SC-AT-09（回帰の更新範囲）**: 既存の `tests/audit_*.rs` は、byte 値に依存する期待値（`committed_bytes` の値、`tests/audit_queue_bytes.rs` の yield が起きる位置）だけを更新し、それ以外（event の中身・sequence・triple の完全性・§8 の順序）は無改変で緑。sink なし経路は不変。
  - **SC-AT-10（決定性）**: 同じ script・同じ注入 budget・同じ sink 応答列で、`committed_bytes` と yield 位置と envelope 列が 100 回一致する（§15.1）。
- **想定の変更規模**: `src/audit.rs`・`src/eval.rs`・`src/embedding.rs`・`src/budget.rs` で約 80〜150 行（推定関数の置き換えと呼び出し側）。仕組みは監査の byte 会計 1 つ。

#### audit-strict-cbor-c3 実装スライス（`original_bytes` と Path HMAC 入力を canonical encoding へ）

> 状態: **未実装**（設計のみ）。c1 が done になってから着手する。判断 Q2・Q3 は kamonabe の選択（Q2 = A、Q3 = 3a・3b・3c とも A）で決まり、以下の実装範囲はその選択で書いている（§9.1 にも反映済み）。

- **実装範囲**:
  - `AuditField` の `original_bytes` を、その field の redaction 前の値（`AuditValue`）の §9.2 encoding の byte 長にする（Q3a = A）。入れ子の値は（Q3b = A）、子 field をすべて `Revealed`（各子の class はそのまま、子自身の `original_bytes` も同じ規則で下から求めた値）とした redaction 前 subtree を encode して測る。`AuditValue` へ写せない子は `Omitted { class, original_bytes: 0 }` のまま置く（要素は落とさず、Sequence の長さと argument index を保つ）。対象は R-1a/R-1b の host-call payload（Path・Body・`arguments` の各要素と外側の `Sequence`）。
  - Path の HMAC-SHA-256 の入力を、canonical label の生 byte から「label を値とする `AuditValue::Scalar(AuditScalar::Utf8(label))` の §9.2 encoding」へ変える（§9.1「field payload全体」・§9.2 末尾。Q3a = A）。Deny で label を使わない規則（R-1a）は変えない。
  - R-1b の `max_revealed_body_bytes` との比較は新しい `original_bytes`（CBOR の header を含む長さ）で行う（Q3c = A）。
  - 引数の型エラーで `AuditValue` へ写せない引数（文字列でない引数など）は、現行どおり `original_bytes = 0` の `Omitted` とする（Q2 = A、§9.1）。
- **範囲外（逸脱・延期）**: 60 KiB 縮退は c4。`AuditFrame.module_id` の型は変えない。VM は対象外。
- **c3 の done PR で行う逸脱一覧の更新**: 「`original_bytes` を UTF-8 byte 長で代用」の行と「HMAC 入力が label の生 byte」の行を削除する。
- **判断 Q2（kamonabe の選択: A）**: 引数の型エラーで引数が文字列でないとき（現在は `original_bytes = 0`）、`original_bytes` をどうするか。script の値（Int・List・関数など）を `AuditValue` へ写す規則は spec に無い。
  - A（推奨）: 0 のままとし、§9.1 に「`AuditValue` へ写せない引数（型エラー）は `original_bytes = 0` の `Omitted`」と明記する。型エラーの引数は Path/Body として意味を持たず、写像の規則を足さずに済む。根拠: manifesto 原則 8・原則 5、§9.1。
  - B: script の値を `AuditValue` へ写す規則（Null/Bool/Int/Float/String → Scalar、List → Sequence、Dict → Object、関数などは型名）を §9.1 に足し、その encode 長を測る。§9.1 の定義を文字どおり満たすが、record 用の Value encoding（§9.2 `RecordedHostRequest`）と別の写像が増える。
- **判断 Q3（kamonabe の選択: 3a・3b・3c とも A）**: redaction 前 subtree の encoding の定義（§9.1「field payload全体」・§9.2「redaction前subtree」はどちらにも読める。`AuditField::Revealed` 全体は `original_bytes` を含み循環するので候補から外す）。
  - **3a. Path の HMAC 入力**: A（推奨）`AuditValue::Scalar(AuditScalar::Utf8(label))` の encoding（field の値として他の field と同じ規則で測れ、`original_bytes` と同じ入力になる。根拠: §9.1「field payload全体」、manifesto 原則 8）。B: label の CBOR text 単体（`AuditValue`/`AuditScalar` の tag を含まない。§9.1「canonical resource label」に近い）。どちらでも digest の値は現行から変わる。
  - **3b. 入れ子の `original_bytes`**: A（推奨）子 field をすべて `Revealed`（各子の class はそのまま）にし、子の `original_bytes` も同じ規則で下から求めた subtree の encode 長（子を単独で測った値と親の中の子の値が一致し、決定的。根拠: §9.1「redaction前のcanonical subtree payload長」、manifesto 原則 5）。B: 子の `original_bytes` を 0 とみなして測る（再帰が無く単純だが、親の中の子の表現が実際の Revealed 子と一致しない）。
  - **3c. R-1b の上限比較**: A（推奨）CBOR header を含む新しい `original_bytes` と比較する（§9.1 の「上限内」と `original_bytes` が同じ量を指し、比較対象が 1 つで済む。Reveal の境界が header 分ずれる。根拠: §9.1、R-1b 小節の「比較対象は `original_bytes`」）。B: 原本の UTF-8 byte 長と比較する（host が設定する上限の意味が R-1b のまま変わらない。比較用の長さを `original_bytes` と別に持つ）。
- **受入基準（c3）**:
  - **SC-AT-11**: Path（Allow / Deny）・Body（write/append の内容、read の response）・`arguments` の各要素の `original_bytes` が、redaction 前の値の §9.2 encoding の長さと一致する（例: 5 byte の ASCII 文字列は `Scalar(Utf8)` の encode 長）。
  - **SC-AT-12**: Path の digest が `HMAC-SHA-256(hmac_key, deterministic_cbor(Scalar(Utf8(label))))` と一致する（既知の key・label による test vector）。同じ key・label なら一致し、key か label が違えば変わる（§15.4）。
  - **SC-AT-13**: R-1b の上限判定が新しい `original_bytes` で行われる。上限と等しい body は Reveal され、1 byte 超えると全体 `Omitted` になる（境界が CBOR header 分ずれることを固定する）。
  - **SC-AT-14**: canary を入れた path・body が encode 後の byte 列に平文で現れない（§15.4）。同じ入力では 100 回一致する。
  - **SC-AT-15**: 外側の `arguments`（`Sequence`）の `original_bytes` が、子をすべて `Revealed` とした redaction 前 subtree の encode 長と一致する。型エラーの引数（`AuditValue` へ写せない引数）を含む `arguments` では、その子を `Omitted { class, original_bytes: 0 }` として測った長さと一致する。
- **想定の変更規模**: `src/audit.rs`・`src/builtin_core.rs` で約 60〜120 行。既存の R-1a/R-1b のテストの `original_bytes` と digest の期待値を更新する。仕組みは redaction 1 つ。

#### audit-strict-cbor-c4 実装スライス（60 KiB 縮退規則と Terminal `Emergency`）

> 状態: **未実装**（設計のみ）。c2・c3 が done になってから着手する。判断 Q4 は kamonabe の選択（A: class ごと）で決まり、以下の実装範囲はその選択で書いている（§7.2 にも反映済み）。

- **実装範囲**:
  - 通常 event の実 encode 長が 60 KiB を超える場合、§7.2 のとおり Body → Path → Identifier の順に、class ごとにまとめて（Q4 = A。その class の `AuditField` をすべて）`Omitted { class, original_bytes }` へ縮退して測り直し、それでも超える event は `AuditFailure::EventTooLarge`（fail-closed）とする。縮退は event を評価器 buffer へ積む前（queue 計上の前）に行い、queue と journal は縮退後の長さを使う。Secret と、`AuditField` でない field（関数名・capability 名・error code など）は縮退の対象にしない。
  - Terminal は、`Full(AuditError)` を含む Terminal 全体の実長が 60 KiB を超える場合に `Emergency { code, message_id, full_error_bytes, full_error_sha256 }` へ置き換える。`full_error_bytes` と `full_error_sha256` は redaction 済み `AuditError` の §9.2 encoding に対して求める。Emergency の Terminal 全体は 4 KiB 以下になる（code / message_id は各 128 byte まで）。
- **範囲外**: `max_revealed_body_bytes` の既定や allow-list の規則は変えない。VM は対象外。
- **c4 の done PR で行う逸脱一覧の更新**: 未実装範囲の「60 KiB 縮退規則」行を削除する。
- **判断 Q4（kamonabe の選択: A）**: §7.2「Body/Path/Identifierの順にOmittedへ縮退」をどの粒度で行うか。
  - A（推奨）: class ごと。event 内の Body をすべて `Omitted` にして測り直し、まだ超えれば Path をすべて、次に Identifier をすべて `Omitted` にする。段は最大 3 つで、結果が入力から一意に決まり、実装・検証が単純。根拠: manifesto 原則 5・原則 8、§7.2 の文言の素直な読み方。
  - B: field ごと。同じ class の中で大きい field から 1 つずつ `Omitted` にし、60 KiB 以下になったら止める。残る情報は多いが、同じ長さのときの順番などの規則を新しく決める必要があり、測り直しの回数も増える。
- **受入基準（c4）**:
  - **SC-AT-16**: Reveal で 60 KiB を超える Body を持つ event が、Body の `Omitted` 化で 60 KiB 以下に収まり、`original_bytes` は保持される。
  - **SC-AT-17**: Body を縮退しても超える場合は Path、次に Identifier を縮退する。全部縮退しても超える event は `EventTooLarge` で AuditFailure terminal になる（§10.1）。
  - **SC-AT-18**: Terminal の `Full` が 60 KiB を超える場合に `Emergency` になり、`full_error_bytes`・`full_error_sha256` が `AuditError` の encoding と一致し、Terminal 全体が 4 KiB 以下になる。
  - **SC-AT-19**: 60 KiB 以下の event は縮退されず、c3 時点と byte 単位で一致する（既存テストは無改変で緑）。
  - **SC-AT-20**: 同じ入力では縮退結果と envelope 列が 100 回一致する（§15.1）。
- **想定の変更規模**: `src/audit.rs`・`src/eval.rs`（または event を積む箇所）・`src/embedding.rs` で約 100〜180 行。仕組みは縮退 1 つ。

#### fix-audit-sink-reentrancy-guard 実装スライス（sink callback 中の同じ Engine への再入検出）

> 状態: **未実装**（設計のみ）。loop:drift #263 D-9（issue #263 の Q2=A、返信なしのため推奨案で確定）で足した `fix` スライス。§10 の「sink callback中に同じEngineの…を呼んではならない。thread-local reentrancy guardで検出し`AuditSinkError::Reentrant`にする」と §15.3 の「sink callbackのEngine再入を拒否し、deadlockしない」を満たす。この設計 PR（#281）の判断待ち 2 つ、PR #281 の Q1（再入した呼び出しへ何を返すか・どの入口を対象にするか）と PR #281 の Q2（入れ子の callback で再入の印をどの実行に付けるか。レビュー指摘 2）は、返信なしのため推奨案で確定した（[開発ループ手順書](loop-runbook.md) §7.5）: Q1=A・Q2=A。以下の実装範囲と受入基準はこの選択で書いており、選んだ内容は下の「本文への反映」のとおり目標の章へ書いた。

設計時点（main `b7ce903`）の事実:

- `AuditSinkError::Reentrant` は型だけで、生成する箇所が無い（`src/audit.rs` の `AuditSinkError`）。
- facade の `Engine`（`src/embedding.rs`）は `Send + Sync`（同ファイルのコンパイル時アサーション）で、`Send + Sync` の部品（`Arc<dyn AuditSink>`・`Arc<HostFunctionRegistry>`）だけを持ち、内部に lock を持たない。sink は `EngineBuilder::audit_sink` で渡すため、sink が `Arc<Engine>` を後から受け取れば、callback の中から同じ Engine の入口を呼べる。
- sink の `submit` を呼ぶのは次の 3 か所で、どれも run を呼んだ thread の上で同期に呼ぶ: `run_audited_inner` の Started の submit と、Started 失敗時の Terminal の submit、`AuditDelivery::deliver`（host-call triple・`Yielded`/`Resumed`・`BudgetCharged`・Terminal の配送と、backpressure 中の同じ batch の再送）。
- facade の公開入口は `compile`・`link`・`link_for_repl`・`run`・`run_for_repl`・`run_audited` の 6 つである。§10 が挙げる start/poll/pause/resume/cancel は scheduler の `ExecutionHandle`（`src/engine.rs`）の操作で、scheduler の `Engine` は sink を持たない（逸脱一覧の B-1 の行）。facade では start と poll は run 系 3 入口の中にあり、pause/resume の入口は無い。cancel は `CancellationToken`（`Send + Sync` の flag）で、Engine の入口ではない。host function と別の audit emit は、run 系の入口を通らなければ呼べない。

- **実装範囲**:
  - `src/audit.rs` に thread-local の guard を置く。中身は「この thread で今 sink callback の中にいる Engine の列」で、列の要素ごとに `EngineId` と「その Engine への再入が試みられたか」の印を持つ（PR #281 の Q2=A。印を thread で 1 つにはしない）。sink の `submit` を呼ぶ 3 か所を、guard に入って（自分の要素を積んで）`submit` を呼び、出るときに自分の要素の印を確かめる 1 つの helper に通す。guard から出る処理は RAII（`Drop`）で行い、sink が panic して unwind した場合も列から外れるようにする。別の Engine の sink callback が入れ子になった場合も、入った順に積み、出るときに自分の要素だけを外す。同じ Engine の要素は列に高々 1 つである（その Engine の callback の中では同じ Engine の実行が拒否され、2 つ目の要素を積む submit に届かないため）。
  - facade の 6 入口は、最初に「この thread で、この Engine（`self.id`）の要素が列にあるか」（列の先頭だけでなく、入れ子の下の要素も含む）を確かめる。あれば、何も実行せずに拒否し、その要素にだけ印を立てる。拒否の返し方（PR #281 の Q1=A）: `run`・`run_for_repl` は `ExecutionOutcome::InternalFailure`、`run_audited` は `AuditedOutcome::Outcome(InternalFailure)`、`compile` は `CompileDiagnosticCode::InternalFault` の診断 1 件の `CompileErrors`、`link`・`link_for_repl` は `LinkError::InternalFailure`。この検査は poison・running の検査より前に行い、渡された context の `running`・`poisoned`・binding を変えない（同じ context への再入を拒否する既存の検査と同じく、拒否で context を poison しない）。
  - `submit` から戻ったとき自分の要素に印が立っていれば、sink が返した値（`Ack` でも）を捨てて `AuditSubmit::Failed(AuditSinkError::Reentrant)` として扱う。以降は既存の `Failed` と同じ経路を通る: Started なら script work を行わず `Terminal(AuditFailure)` を emergency slot へ append して `AuditFailed { failure: AuditFailure::Sink }`、実行途中・Terminal の配送なら `AuditFailure::Sink` で fail-closed（§10.1）。`AuditFailure` に variant は足さない（`AuditSinkError` の他の variant と同じく `Sink` に畳む）。入れ子で、Engine A の callback の中で実行した別の Engine B の callback から A へ再入した場合、印が立つのは A の要素だけなので、B の `submit` は sink が返した値どおりに扱い（B の実行は通常どおり進む）、A の `submit` から戻ったときに A の実行だけが fail-closed になる。
  - `AuditSinkError::Reentrant` の doc comment から「A-1 では検出配線なし」を外す。
- **範囲外**:
  - 別の thread からの呼び出しは thread-local guard の対象外であり、検出しない（§10 の「thread-local reentrancy guard」どおり）。sink が別 thread で同じ Engine を使っても、facade の `Engine` は lock を持たないため deadlock はしない。
  - scheduler の `Engine`（`src/engine.rs`）と `ExecutionHandle` は sink を持たないため対象外。sink を持つ scheduler 経路は E10 統合のスライスで扱う。
  - `CancellationToken::cancel` は止めない（別 thread からの cancel を受けるための flag で、execution-control の契約どおり callback からも呼べる）。
  - `AuditWaker`・`AuditCompleter::complete` から呼ばれる waker の中の再入（[実行予算・協調実行仕様](execution-control.md) の waker 契約）は扱わない。
  - sink の panic の捕捉方法（Started の submit が `catch_host_unwind` の外にあること）は変えない。guard が unwind で外れることだけを保つ。
  - VM は対象外（監査 sink と VM の同時設定は build で拒否される）。
- **本文への反映（この設計 PR のマージ前に行う）**: 再入した呼び出しが何を返すか・どの入口を対象にするかは公開 API の挙動なので、スライス小節だけに置かず目標の章に書く。判断待ち（PR #281 の Q1 と Q2）が確定したら（[開発ループ手順書](loop-runbook.md) §7.3 の 4、返信が無い場合は §7.5）、同じ PR に追加の commit で次を書く。判断待ちの間は本文を変えない（未確定の選択を確定として書かないため）。Q1=A・Q2=A の確定（§7.5）を受けて、同じ PR の追加の commit で次を書いた。
  - (a) §10 の再入の規則: 対象の入口と、再入した呼び出しへ返す値。§10 の「start/poll/pause/resume/cancel」が scheduler の `ExecutionHandle` の操作を指すこと、facade の入口との対応、`CancellationToken::cancel` を callback から呼べるかどうか（小節の範囲外の扱いを本文に写す）。入れ子の callback でどの実行を fail-closed にするか（Q2=A: 再入された Engine の実行だけ）。`ExecutionHandle` の操作で拒否に返す値は、Q1 の選択肢が facade の 6 入口だけを扱ったため決めず、sink を scheduler 経路へ配線するとき（E10 統合）に決めると書いた。
  - (b) [組み込みAPI仕様](embedding-api.md) の再入の規則（§9 の「同じcontext/handleへの再入は許さず`InternalFailure`とする」の段落）と §11 の規則 2: `compile`・`link` の拒否に使う値（Q1=A なので `CompileDiagnosticCode::InternalFault`・`LinkError::InternalFailure`。§11 の規則 2 はこれらを panic の写像としてだけ定義していた）。拒否で context を poison しないことと、§10 の規則 4 との関係も書いた。
  - Q1・Q2 とも推奨案 A になったため、本小節の実装範囲と受入基準は Q1 の選択肢を変えずに保ち、Q2=A に合わせて guard の印の持ち方（実装範囲の 1 項目め・3 項目め）と RG-AT-04（入れ子の A・B それぞれの実行結果）を確定させた。
- **done PR で行う逸脱一覧の更新**: 逸脱一覧の「`AuditSinkError::Reentrant` は型だけで…」の行を削除し、表の下の段落から D-9 についての記述（「`AuditSinkError::Reentrant` が型だけである行（D-9）」と、D-9 の理由の文）を削除する。D-8 についての記述は残す。
- **判断 Q1（PR #281 の Q1。返信なしのため推奨案 A で確定、[開発ループ手順書](loop-runbook.md) §7.5）**: §10 は「呼んではならない」「guard で検出し `AuditSinkError::Reentrant` にする」とだけ書き、再入した呼び出しそのものに何を返すか、facade のどの入口を対象にするかを決めていない。どれを選ぶかで公開 API の挙動（と、B では公開型）が決まる。
  - A（推奨）: facade の 6 入口すべてを対象にし、既存の InternalFailure 系（上記）で拒否する。公開型を増やさない。根拠: [組み込みAPI仕様](embedding-api.md)「同じcontext/handleへの再入は許さず`InternalFailure`とする」、[Capability Model仕様](capability-model.md) §11.3 の host callback の再入禁止と同じ扱いになる。§10 の列挙に compile が含まれる。外側の execution は `Reentrant` で fail-closed になり、host は `AuditFailed` で契約違反を観測できる。manifesto 原則 7（監査可能）・原則 8（小さく理解可能な中核）。
  - B: A と同じ入口を対象にし、拒否を表す新しい公開 variant（`ContextError`／`LinkError`／`CompileDiagnosticCode` に `Reentrant` 相当）を足す。再入した呼び出しの側でも理由を区別できるが、公開 API が 3 つ増え、外側の `AuditFailed` と情報が重なる。
  - C: run 系の 3 入口だけを対象にし、`compile`・`link`・`link_for_repl` は許す（audit を emit せず、execution の状態を持たないため）。変更は小さいが、§10 の列挙（compile）を直す必要がある。
- **判断 Q2（PR #281 の Q2。設計レビューの選択肢ありの指摘 2。返信なしのため推奨案 A で確定、[開発ループ手順書](loop-runbook.md) §7.5。issue #263 の Q2 とは別）**: Engine A の callback の中で別の Engine B を実行し、B の callback が A へ再入したとき、どの実行を fail-closed にするか。印が thread で 1 つか、列の要素ごとかで決まり、公開の挙動（どちらの実行が `AuditFailed` を返すか）にかかわる。
  - A（推奨）: 印を「callback 中の Engine の列」の要素ごとに持ち、再入された Engine（A）の要素にだけ立てる。A の `submit` だけが `Reentrant` になり、B は通常どおり進む。根拠: RG-AT-03「別の Engine は許す」と同じ扱いになり、B は自分の sink 契約に違反していないため失敗させない。§10 の「同じEngine」への再入だけを禁じる文言どおり。manifesto 原則 6（失敗を制御可能な結果にする: 失敗するのは契約に違反した Engine の実行だけで、host がどの実行が失敗したかを特定できる）・原則 7（監査可能）。
  - B: 印を thread で 1 つ持つ。最初に戻る B の `submit` が `Reentrant` になり、A も失敗させるかを別に決める必要がある。
- **受入基準**:
  - **RG-AT-01（Started の submit 中の再入）**: sink の callback から同じ Engine の `run_audited` を新しい context で呼ぶと、その呼び出しは `AuditedOutcome::Outcome(InternalFailure)` を返し、script を 1 文も実行しない（stdout 出力・host call が 0 件、渡した context は poison されない）。外側の実行は `AuditFailed { failure: AuditFailure::Sink }` になり、sink が受け取る envelope 列は Started と `Terminal(AuditFailure)` だけである。確かめ方: `tests/audit_reentrancy.rs` で、`OnceLock<Arc<Engine>>` を持つ記録用 sink を使う。
  - **RG-AT-02（全入口と全配送点）**: callback の中から `compile`・`link`・`link_for_repl`・`run`・`run_for_repl`・`run_audited` を呼ぶと、それぞれ上記の型で拒否される。再入が host-call triple・`Yielded`/`Resumed`・`BudgetCharged`・Terminal の配送で起きた場合も、sink が `Ack` を返していても外側は `AuditFailure::Sink` で fail-closed になる。確かめ方: 入口ごと・「何回目の submit で再入するか」ごとの table-driven test。
  - **RG-AT-03（別の Engine は許す）**: callback の中から別の Engine（sink なし・sink あり）を呼ぶと通常どおり実行され、外側の実行も成功する（envelope 列は再入しない場合と一致する）。確かめ方: 2 つの Engine を作る統合テスト。
  - **RG-AT-04（guard の後始末）**: 再入で fail-closed になった実行・sink が `Failed` を返した実行・sink が panic した実行の後で、同じ thread から同じ Engine の `compile`・`run_audited` を（callback の外で）呼ぶと通常どおり成功する。2 つの Engine の callback が入れ子になった場合（PR #281 の Q2=A）: Engine A（sink あり）の callback の中で Engine B（sink あり）の `run_audited` を呼び、B の callback から A の `run_audited` を呼ぶと、A への呼び出しは `AuditedOutcome::Outcome(InternalFailure)` で拒否される。B の実行は通常どおり完了し（B の sink が受け取る envelope 列は、B の callback が A を呼ばない場合と一致する）、A の実行は `AuditFailed { failure: AuditFailure::Sink }` になる。B の callback を出たあとも A の callback の中では A への再入は引き続き拒否され、A の callback を出たあとは許される。確かめ方: 統合テストと `src/audit.rs` の guard の unit test（列の要素ごとの印、入れ子の下の要素への印、RAII での取り外し）。
  - **RG-AT-05（deadlock しない・非破壊）**: RG-AT-01〜04 の各テストを別 thread で回し、上限時間（例: 10 秒）以内に戻ることを `recv_timeout` で確かめる。既存の `tests/audit_*.rs` は無改変で緑。sink なしの経路と、再入しない sink ありの経路は観測結果（envelope 列・`committed_bytes`・yield 位置・出力 byte・exit code）が変わらない。
- **想定の変更規模**: `src/audit.rs` に約 50〜70 行（thread-local の guard・RAII・入口用の判定関数・doc comment）、`src/embedding.rs` に約 50〜80 行（submit 3 か所を helper へ通す・6 入口の先頭の検査と拒否の値）。合わせて約 100〜150 行。テストは新しい `tests/audit_reentrancy.rs`（約 250〜350 行）と `src/audit.rs` の unit test。触る仕組みは監査の sink 配送の境界 1 つ、受入基準は 5 項目。

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
- §3の「terminal時`BudgetUsage`のbackend一致」に従い、charge traceの突合はREPL持続closureがretainするcode（tree=本体AST／VM=prototype chunk）の課金も含める。[実行予算・協調実行仕様](execution-control.md) §5.3のA-1モデル（closure寿命トークンで課金）により、retained closureのcodeがreachableな限り両engineとも課金し続ける（従来の片側だけrelease-時に落ちるunder-chargeが解消する）ことを突合する。突合対象は**課金の有無とrelease寿命が両engineで一致すること**であり、課金額はengineごとのartifact単位（tree=`ast_node`／VM=`bytecode_chunk`）のままで、§5.3「ASTまたはbytecode」どおりcode artifactの`live_heap`/`peak_heap`数値はengine固有であってよい（byte単位の数値一致まで求めるA-2は採らない）
- AUD-019/024/048の専用fixtureと、AUD-022の生成matrix/fuzzで差分0件を確認する
- 差分が1件でもあるbuildではVM production featureを有効化できないCI gateを置く

> **PR-6c 線引き（REV-015 Slice 6）**: この§15.5のうち、**charge/budget parity**（budget usageの
> engine間不変な軸・terminal reason=`TsumugiError`全field・stdout/stderrのtree/VM一致）は
> REV-015 Slice 6 PR-6cで`tests/differential.rs`に自動化済みである。比較は軸別ポリシーを用い、
> engine間不変な軸（I/O counter・string/source/import counts・per-item peak・terminal reasonの
> kind/message）はexact一致を、fuel countとabsolute heap（`live_heap`/`peak_heap`）は向き・符号・
> release寿命の一致を突合する（byte-exact一致は§5.3のA-1どおりbackend間で成立しないため要求しない。
> fuel枯渇のterminal `line`もstep粒度差でengine間に差があり、fuel parityはStepLimit terminalの
> observable一致で担保する）。
> **（2026-10-20 注記）** 両engineとも上限に達するscriptだけを比べていたため、「treeでは成功しVMでは
> step上限」というobservable差を検出できなかった。fuelは[実行予算・協調実行仕様](execution-control.md)
> §14「F-1 実装スライス」でtreeの課金点に揃え、`committed.fuel`のexact一致で突合する。
>
> 一方、**FakeHost effect logのbyte/順序比較・StableError生成履歴・`normalize_for_conformance`後の
> audit payload・yield/fuelの完全一致・AUD-019/024/048 fixture・AUD-022生成matrix/fuzz**は
> Phase 5/6のdifferential gateの管轄で、PR-6cの射程外である（基盤が未実装）。
> **pause/resume・cancel・host pending**はVMがscheduler非経由（Readyフォールバック・`vm.rs`不変）で
> 実行経路を持たないため**VM対象外**（treeは`tests/scheduler_api.rs`で担保）。
>
> **PR-6c-min完了はVMのexperimental卒業ではない。** 卒業gateは本節§16のconformance（differential
> matrix/fuzz差分0件・差分0件CI gate）であり後続フェーズの管轄である。

## 16. Phase完了条件

Phase 5は、treeが唯一のproduction backendとなり、すべてのambient inputがHostBoundaryへ移り、DeterminismInputとbackend非依存規則が自動検証され、VMのproduction化に差分0件gateが設定された時点で完了とする。

Phase 6は、schema version 1の全event、redaction、bounded journal、fail-closed sink、host call correlation、budget集約、Started/Terminal完全性、record/replay連携が全terminal pathで検証された時点で完了とする。
