# Tsumugi — 次期意味論・実装決定

最終更新: 2026-08-31

設計ステータス: **次期仕様確定・未実装**

## 1. 本書の位置づけ

現行の [`language-spec.md`](language-spec.md) は、実装済みの観測可能な挙動を規定する正本である。本書はそれを書き換えるものではなく、監査バックログで未決定だった意味論、品質改善、将来機能を実装するための**次revisionの実装仕様**である。

本書と現行実装が異なる間は、利用者が実際に依存できる挙動は `language-spec.md` に従う。本書の各受入基準を満たし、tree-walk版とVM版のpaired testが通過した時点で、該当事項を `language-spec.md` へ統合し、仕様revisionを更新する。部分実装を「次revision対応」と表示してはならない。

新規6設計文書間では、本書が次期言語挙動、CLI文法、canonical errorとscript catch可否の正本である。Phase 3/4の公開API・状態機械・budget・transactionは[実行予算・協調実行仕様](execution-control.md)、Phase 5/6のaudit schemaとfail-closed契約は[決定性・実行時監査仕様](determinism-and-audit.md)、Phase 0の責任境界は[脅威モデル](threat-model.md)を正本とする。[組み込みAPI仕様](embedding-api.md)と[Capability Model仕様](capability-model.md)のPhase 1/2先行実装はこれらの最終契約の内部subsetとして実装し、同じbuildへ旧型・旧event・旧状態機械を併存させない。

本書で用いる規範語は次の意味を持つ。

- **MUST / 必須**: 実装・テスト・文書のすべてで満たす。
- **MUST NOT / 禁止**: 実装してはならない。
- **SHOULD / 原則**: 例外には設計記録と回帰テストを必要とする。
- **観測可能**: 戻り値、標準出力・標準エラー、終了コード、error kind/message/line/trace、外部効果、REPL次入力から見える状態を含む。

対象backendはデフォルトのtree-walk版と `--vm` 版である。明示的に「内部リファクタ」とした項目を除き、両backendは同じ観測可能な結果を返さなければならない。

## 2. 次期revisionの共通不変条件

1. **規範意味論は一つ**: backend固有の期待ファイルは、次期revisionで明示的に残す非適合を除き作らない。本書の対象事項については差を残さない。
2. **実行context単位の状態**: 変数、closure cell、import marker、FunctionId、capability、budget、audit識別子は `ExecutionContext` に属する。process-globalな可変意味論を増やさない。
3. **失敗は構造化する**: script入力からhost panic、abort、unchecked cast、index panicへ到達させない。
4. **左から右の評価**: 本書に別の優先順位を定義しない限り、receiver/callee、引数、index、代入値はsource順に一度だけ評価する。
5. **lineとtraceを意味論に含める**: kindとmessageだけ一致していても、lineまたはtraceが異なればbackend非互換とする。
6. **外部効果はlanguage-stateと分離する**: language-stateをrollbackしても、filesystem、stdio、process、network等の完了済み外部効果は戻らない。外部効果の有無はauditで観測可能にする。
7. **IDを再利用しない**: FunctionId等の観測可能な同一性に使う単調増加IDは、rollback後も再利用しない。
8. **上限は境界を含めて定義する**: 「128」のような値だけでなく、何を数え、どの時点で拒否するかを共有定数のdoc contractに含める。

## 3. Canonical error契約（AUD-019）

### 3.1 採用判断

runtime errorは、operationごとの共通constructorから `kind`、canonical message、source lineを生成する。tree/VMが個別にmessageを組み立てる方式と、message文字列からkindを推測する方式を廃止する。`classify_runtime_error()` は互換移行中のprivate fallbackに限定し、次期revision対象経路から呼んではならない。

### 3.2 却下案

- **backendごとの自然な診断を許す**: golden testとhost側分岐がbackend依存になるため却下する。
- **messageの部分一致だけを保証する**: 値や型による誤分類、trace欠落、余分な副作用を検出できないため却下する。
- **すべてを `runtime` kindへ集約する**: `try/catch` とembedding hostが失敗原因を安全に識別できないため却下する。
- **OSエラー文字列をcanonical messageへ埋め込む**: OS・locale差が規範出力になるため却下する。詳細は非規範のcauseまたはaudit metadataへ保持する。

### 3.3 errorデータモデル

runtime errorは次の論理構造を持つ。

```text
RuntimeError {
    kind: ErrorKind,
    message: String,
    line: usize,
    trace: Vec<TraceFrame>,
    cause: Option<HostCause>,       # scriptへは非公開、audit/host API向け
}

TraceFrame {
    name: String,
    line: usize,                    # 呼び出し元のcall-site行
}
```

既存kindに加え、host boundary実装時に `capability`、`budget`、`timeout`、`cancelled`、`host` を追加する。`budget`、`timeout`、`cancelled` は停止要求でありscriptの `try/catch` では捕捉できない。その他のruntime errorは原則として捕捉できる。

message内の型名は `Int`、`Float`、`Str`、`Bool`、`Null`、`List`、`Dict`、`Fn`、`Error`、`Class`、`Instance`、`BoundMethod` を用いる。`{actual}` へ値全体やsecretを埋め込まず、型または安全な整数境界だけを表示する。

### 3.4 operation別canonical kind/message

以下のテンプレートを完全一致の正本とする。波括弧は実行時に置換する部分である。

| operation / 条件 | kind | canonical message |
|---|---|---|
| 変数read・callee名が未定義 | `name` | `未定義の変数または関数: {name}` |
| 未定義変数への通常代入 | `name` | `未定義の変数に代入: {name}` |
| `let` / 関数値 / class値のID割当不能 | `internal` | `内部エラー: {id_kind} を割り当てできません` |
| Intの0除算・剰余 | `zero_division` | `ゼロ除算` |
| 算術演算の対象型不正 | `type` | `演算子 {op} は {left_type} と {right_type} に適用できません` |
| 大小比較の対象型不正 | `type` | `比較演算子 {op} は {left_type} と {right_type} に適用できません` |
| List indexがIntでない | `type` | `List のインデックスは Int である必要があります: {actual_type}` |
| Str indexがIntでない | `type` | `Str のインデックスは Int である必要があります: {actual_type}` |
| Dict keyがStrでない | `type` | `Dict のキーは Str である必要があります: {actual_type}` |
| List index範囲外 | `index` | `List のインデックスが範囲外です: {index} (長さ: {len})` |
| Str index範囲外 | `index` | `Str のインデックスが範囲外です: {index} (長さ: {len})` |
| index read非対応型 | `type` | `インデックスアクセスできない型です: {actual_type}` |
| index assignment非対応型 | `type` | `インデックス代入できない型です: {actual_type}` |
| 反復非対応型 | `iteration` | `反復できない型です: {actual_type}` |
| 非callableの呼び出し | `type` | `呼び出せない型です: {actual_type}` |
| user function / method arity | `argument` | `{callable} の引数個数が一致しません: 期待 {expected}, 実際 {actual}` |
| builtin arity | `argument` | `{builtin} の引数個数が一致しません: 期待 {expected}, 実際 {actual}` |
| variadic builtin最小arity | `argument` | `{builtin} の引数が不足しています: 最小 {minimum}, 実際 {actual}` |
| builtin argument型 | `builtin_type` | `{builtin} の第 {position} 引数は {expected} である必要があります: {actual_type}` |
| callbackが非callable | `type` | `{builtin} のコールバックは呼び出し可能である必要があります: {actual_type}` |
| callback arity | `argument` | `{builtin} のコールバック引数個数が一致しません: 期待 1, 実際 {actual}` |
| `push` / `pop` の第1引数が変数でない | `builtin_type` | `{builtin} の第 1 引数には List 変数を指定してください` |
| 空Listへの`pop` | `builtin_type` | `pop は空の List には使用できません` |
| builtin固有の状態不正 | `builtin_type` | `{builtin} は {state} には使用できません` |
| collection上限 | `collection_limit` | `コレクション要素数が上限を超えました: {requested} (上限: {limit})` |
| user function depth上限 | `overflow` | `スタックオーバーフロー: 再帰が深すぎます (上限: 128)` |
| AST深度上限 | `overflow` | `AST のネストが深すぎます (上限: 256)` |
| import chain深度上限 | `import` | `import 失敗: ネストが深すぎます (上限: 128)` |
| import先が存在しない・読めない | `import` | `import に失敗しました: モジュールを読み込めません: {module}` |
| import先のparse失敗 | `import` | `import に失敗しました: モジュールの構文が不正です: {module}` |
| loop外の`break` | `control_flow` | `break はループの中でのみ使用できます` |
| loop外の`continue` | `control_flow` | `continue はループの中でのみ使用できます` |
| step/fuel上限 | `limit` | `ステップ上限に達しました (上限: {limit})` |
| 整数演算overflow | `int_overflow` | `整数オーバーフロー: {operation}` |
| 文字列等からIntへ変換不能 | `conversion` | `to_int で Int に変換できません: {reason}` |
| Float→Int不能 | `conversion` | `{builtin} で Int に変換できません: {reason}` |
| file sizeがi64範囲外 | `int_overflow` | `ファイルサイズを Int で表現できません` |
| `exit` code範囲外 | `argument` | `exit の終了コードは 0 から 255 の範囲で指定してください: {code}` |
| `path_join`非Str | `builtin_type` | `path_join の第 {position} 引数は Str である必要があります: {actual_type}` |
| property read対象がInstanceでない | `type` | `プロパティアクセスできない型です: {actual_type}` |
| property write対象がInstanceでない | `type` | `プロパティ代入できない型です: {actual_type}` |
| property不存在 | `name` | `プロパティが見つかりません: {class_name}.{property}` |
| `init`がNull以外をreturn | `type` | `{class_name}.init は Null 以外を返せません: {actual_type}` |
| filesystem capability拒否 | `sandbox` | `ファイル操作が許可されていません: {operation}` |
| host function capability拒否 | `capability` | `host function の実行が許可されていません: {name}` |
| host adapter失敗 | `host` | `host function の実行に失敗しました: {name} ({category})` |
| deadline超過 | `timeout` | `実行期限を超過しました` |
| cancellation | `cancelled` | `実行がキャンセルされました` |
| host/heap/I/O budget超過 | `budget` | `{budget_name} の上限を超えました (上限: {limit})` |
| stdout書込失敗 | `io` | `標準出力への書き込みに失敗しました` |
| VM/Compiler不変条件違反 | `internal` | `内部エラー: {stable_detail}` |

`{state}`、`{reason}`、`{operation}`、`{category}` は各BuiltinSpecまたはoperation constructorが列挙型から選び、任意のOS文字列を入れない。import先のparse error詳細はcauseとして保持し、CLIはwrapperに続けて同じcanonical parse diagnosticsをsource順に表示する。

この表は言語core runtime error constructorの全inventoryである。新しいcore constructorを追加するPRは、既存行へ対応付けるか新しいcanonical行を追加しなければCIを通過してはならない。core外のHostFunction adapterは、登録時に静的 `HostErrorSpec` extension table（kind、message template、placeholder列挙、catch可否）を同時登録し、同じinventory testへ連結する。HTTP adapterのextension正本は16.12節であり、未登録adapterのerror constructorは実行できない。filesystem builtinのうち仕様上Null/Falseを返すOS失敗はruntime error constructorではないため表の対象外だが、capability拒否は必ず表の`filesystem capability拒否`を使う。

`expected` が複数候補の場合はregistryに保持した文字列（例: `1 または 2`）を用いる。callbackで実際のuser function bodyへ入った後に発生したエラーは、callback専用messageへ包み直さず、元のkind/message/lineを保持する。

### 3.5 line規則

| 失敗位置 | canonical line |
|---|---|
| parse error | 不正tokenまたはEOF tokenの行 |
| 二項・単項演算 | 演算子を含む式の開始行 |
| index read | `[` を含むindex式の開始行 |
| index assignment | assignment文の開始行 |
| callee/arity/capability検査 | call式の開始行 |
| 引数式内部 | その引数で実際に失敗した式の行 |
| builtin本体の型・境界検査 | call式の開始行 |
| callback body | callback内で実際に失敗した文・式の行 |
| property read/write | `.` を含む式または代入文の開始行 |
| `init`の不正return | `return` 文の行 |
| import解決 | rootから見た `import` 文の行 |
| host adapter内部 | host functionのcall式の行 |
| defensive VMのline table不正 | 安全に取得できれば該当命令行、取得不能なら1 |

### 3.6 trace規則

- traceはエラー発生地点に最も近いuser function/methodから外側へ並べる。
- 各frameのlineは、そのframeを呼び出したcaller側call-siteの行とする。root script frameは含めない。
- call validationがframe作成前に失敗した場合、不正calleeのframeは追加しない。
- callbackへ入った場合はcallbackのuser function frameを通常関数と同じように追加する。`map` 等のbuiltin自体はframeに追加しない。
- bound methodのframe名は `{Class}.{method}`、`init`は `{Class}.init` とする。
- host adapter内部frame、Rust関数名、VM instruction offsetをscript traceへ出さない。
- catchされたError値の `line` はorigin lineを保持する。次期revisionではError値へtraceを公開しないが、再throw相当の仕組みを将来追加してもorigin traceを破棄しない。

### 3.7 error precedence

| operation | 検査・評価順 |
|---|---|
| user function / method call | callee評価 → callable分類 → user callableならstep・user frame深度 → callable/arity → 引数を左から右 → frame作成 → body |
| context builtin (`input/args/exit/push/pop/map/filter/each`) | builtin選択 → arity → mutation target構文条件 → 引数を左から右 → 型・状態 → 実行 |
| pure/core builtin | callee選択 → 引数を左から右 → registry arity → 型・境界 → 実行 |
| host function | symbol/arity → host functionの粗粒度capability → host-call回数budget/deadline/cancel → 引数を左から右 → 型・shape → 引数値依存policy（URL/宛先等）→ audit start → adapter外部処理 |
| index read | object → index → object型 → index/key型 → 境界 → read |
| index assignment | target binding存在 → index → value → targetの最新値の型 → index/key型 → 境界/size → mutation |
| property read | receiver → Instance検査 → field lookup → method lookup → missing error |
| property assignment | receiver → Instance検査 → value → field mutation |
| class construction | class/`init` arity → 引数を左から右 → Instance生成 → `init` user frame → Instance返却 |
| callback builtin | outer引数を左から右 → collection型 → 要素ごとにcallback callable/arity → callback body |

### 3.8 tree / VM変更箇所

- `src/error.rs`: operation別constructor、追加kind、canonical rendererを定義する。
- `src/eval.rs`, `src/builtin.rs`: `runtime()`のmessage推測をやめ、共通constructorとline規則を使う。
- `src/vm.rs`: `eval_index`、call/callback、iteration、trace生成を共通constructorへ移す。
- `src/builtin_core.rs`: builtin型・境界errorをregistry metadataと共通constructorへ統合する。
- `src/compiler.rs`: call-site、property、callbackのsource lineをopcodeへ欠落なく保持する。
- CLIは `Display` のcanonical出力だけをstderrへ書く。backend名を追加しない。

### 3.9 移行

error messageやkindを文字列比較するscript・hostは更新が必要になる。移行期間に旧messageを併記してはならない。release noteに operation→旧/新kind/messageの対応表を掲載し、`e["type"]` を使うことを推奨する。

### 3.10 テスト行列

| 軸 | ケース |
|---|---|
| backend | tree / VM |
| phase | parse / link / call validation / argument / body / host boundary |
| call | direct / closure / callback / bound method / constructor |
| index | List / Dict / Str / 非対応型 / 負index / 範囲外 |
| trace | top-level / 1段 / 多段 / callback / method→function→method |
| catch | 未捕捉 / 同一frame catch / caller catch |
| line | 単行 / 複数行 / import先 / f-string内 |

全ケースで終了コード、stdout、stderr、kind、message、line、traceを完全一致比較する。`index_read_lowering.expected.vm` のような本項目由来のbackend別期待値は削除する。

### 3.11 受入基準

- 代表例ではなくoperation表の全行に少なくとも1つのpaired testがある。
- runtime error constructor inventory testが、表または登録済みHostErrorSpec extensionに対応しないconstructorと任意文字列からのkind推測を拒否する。
- error生成の対象経路で `classify_runtime_error()` を呼ばない。
- tree/VMのstdout・stderr・終了コード・Error値が完全一致する。
- callback、import、REPL、defensive Chunkを含めhost panicがない。

## 4. `let`再宣言のbinding identity（AUD-016）

### 4.1 採用判断

同一scopeで同名を `let` 再宣言した場合も、**必ず新しいcellを作る**。そのscopeの名前解決は新cellへ切り替わるが、再宣言前に作られたclosureは旧cellを保持し、旧値を見る。現行tree-walk版へ統一する。

```tsg
let x = 1
let before = fn() x end
let x = 2
let after = fn() x end
print(before())  # 1
print(after())   # 2
print(x)         # 2
```

関数定義・class定義も名前を現在scopeへbindingする宣言であり、同じfresh-cell規則を使う。

### 4.2 却下案

- **既存cellの値だけを更新する**: `let` と通常代入の区別が失われ、過去のclosureの意味が再宣言で変わるため却下する。
- **同一scope再宣言を構文エラーにする**: 現行sourceとの互換性を不必要に壊すため却下する。
- **closureだけ旧値をsnapshotする**: 参照captureモデルと矛盾するため却下する。

### 4.3 データモデル / 状態遷移

scopeは `name -> CellId/SharedValue` のmapである。

```text
Declare(name, value):
  old = scope[name]             # あってもよい
  fresh = new Cell(value)
  scope[name] = fresh
  oldは既存closureが参照している限り生存

Assign(name, value):
  visible scopeを検索
  解決した既存Cellの内容を更新
```

REPL transactionが未捕捉errorでrollbackした場合、scope mapは入力開始時のcellへ戻し、入力中に作ったfresh cellを公開しない。FunctionIdは再利用しない。

### 4.4 tree / VM変更箇所

- tree: `Env::set` の現行fresh-cell挙動を規範として維持し、unit testを追加する。
- VM: `Compiler::compile_stmt(Stmt::Let/FnDef/ClassDef)` は `find_local_in_current_scope` によるslot再利用をやめ、同一scopeでも新slotを割り当てる。name resolverとtop-level global registryだけを新slotへ付け替える。
- VM: 旧slotと `locals_cells` をpopしてはならない。escaping closureが旧cellを保持する。
- REPL: 正常commit後は最新slotを次入力の名前解決対象にし、rollback時は旧slot mappingへ戻す。

### 4.5 error

再宣言自体はerrorにしない。cell/slot割当不能は `internal` / `内部エラー: binding cell を割り当てできません` とし、宣言行をlineにする。

### 4.6 移行

VMで「再宣言後に過去のclosureも新値を見る」挙動へ依存したsourceは変わる。共有更新が必要な場合は `let` ではなく通常代入を使う。

### 4.7 テスト行列

| scope | 宣言種別 | closure作成位置 | 実行形態 |
|---|---|---|---|
| top-level / function / block / REPL跨ぎ | `let` / `fn` / `class` | 再宣言前 / 後 / 両方 | direct / callback / import |

旧closure、最新binding、通常代入、shadowing、未捕捉error rollback、cell化前後をtree/VMで比較する。

### 4.8 受入基準

- すべてのscopeで再宣言がfresh cellになる。
- 既存closureは旧cell、再宣言後のreadと新closureは新cellを見る。
- VMのslot再利用による差がなく、REPL失敗時にstale slotが残らない。

## 5. user function frame上限（AUD-017 / AUD-050）

### 5.1 採用判断

`MAX_USER_CALL_DEPTH = 128` を `src/limits.rs` に一度だけ定義する。数えるのは**root frameを除く、現在activeなuser定義関数・lambda・method・initのframe**である。通常callと `map/filter/each` callbackは同じ規則を使う。host functionとcore builtin自体はuser frameへ数えない。

128個目のuser frameは実行可能で、129個目を作る直前に拒否する。動的calleeは先に一度だけ評価・分類し、user callable（Fn、BoundMethod、`init`を持つClass）の場合だけ、引数評価より前にstepと深度を検査する。builtin、host function、`init`なしClassにはuser-frame深度検査を適用しない。constructor allocationはframeに数えず、実際に実行する `init` は1 user frameとして数える。host functionがhost内部で行うRust callやHTTP redirectも数えないが、別のhost-call budgetで制御する。

### 5.2 却下案

- **VM root frameを1つとして数える**: backendの実装詳細がsourceの許容深度へ漏れるため却下する。
- **callbackだけ別上限にする**: `map` 経由で上限を迂回できるため却下する。
- **host functionもuser frameに数える**: scriptが制御できないadapter実装深度で結果が変わるため却下する。

### 5.3 データモデル / 状態遷移

```text
active_user_frames: usize
BeforeUserCall:
  if active_user_frames >= MAX_USER_CALL_DEPTH: error
  active_user_frames += 1
AfterUserCall / unwind:
  active_user_frames -= 1
```

unwind、catch、return、cancellationの全経路で減算する。VMは `frames.len() - root_frame_count` を直接各所で計算せず、`active_user_frame_count()` を唯一の計数APIにする。

### 5.4 tree / VM変更箇所

- `src/limits.rs`: `MAX_USER_CALL_DEPTH` と本節のdoc contractを追加する。
- `src/eval.rs`: ローカル定数を削除し、通常callで共有定数を使う。
- `src/builtin.rs`: callback callで同じ計数APIを使う。
- `src/vm.rs`: ローカル定数を削除し、root frameを除くhelperを `PrepareCall`、defensive `Call`、callbackへ適用する。
- class実装時はmethod/initも同じhelperを通す。

### 5.5 error

上限超過は `overflow`、messageは `スタックオーバーフロー: 再帰が深すぎます (上限: 128)`。lineは拒否された129個目のcall式。traceには既にactiveな128 user frameだけを内側から外側へ出す。

### 5.6 移行

VMで許容されるuser frameが127から128へ1つ増える。また深度上限到達時もcallee式の評価までは行われ、その結果をuser callableと分類した後、引数評価前に拒否する。callee式の副作用順に依存する場合はこの規則へ移行する。tree/VMともbuiltin・host function呼び出しはuser depth 128でも拒否しない。`MAX_CALL_DEPTH` を直接参照していたlibrary利用コードは、公開する場合 `MAX_USER_CALL_DEPTH` へ移行する。

### 5.7 テスト行列

- direct recursion 127 / 128 / 129
- mutual recursion
- lambda recursion
- `map/filter/each` callback recursion
- method/init recursion
- user→host→user再入が可能になった場合のuser frame計数
- REPLでcatch済み上限error後の回復
- defensive Chunkが `PrepareCall` を迂回した場合

### 5.8 受入基準

- tree/VMとも128 user frameを許可し、129個目を同じline/message/traceで拒否する。
- callback・method・initで迂回できない。
- root/host/core builtin frameは数えない。
- `MAX_USER_CALL_DEPTH` の定義は `limits.rs` の1か所だけに存在する。

## 6. CLI grammarとscript引数（AUD-018）

### 6.1 採用判断

CLI grammarを次で固定する。Capability/profile optionもすべて`OPTIONS`に属し、別文書に独立したgrammarを定義しない。

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

複数回指定できるoptionは`--allow-env`、`--fs-root`、`--fs-op`、`--allow-import-root`だけであり、詳細な値検証とsafe/legacy profileの構築規則は[Capability Model仕様](capability-model.md)第14節に従う。safe profileはprofile builderがstdout capabilityを明示grantする既定構成であり、process stdioへのambient accessではない。`--deny-stdout`はその明示grantを除去する。REPL promptはscript capability外のCLI I/Oである。

option解析中の最初のpositionalを `SCRIPT` とし、それ以後のtokenは既知option、未知option、`--`を含めて一切再解釈せず、そのままscript argsとする。`--` はoption解析を終了し、次のtokenがあればSCRIPTとして扱う。`--`だけでEOFならSCRIPTなしとしてREPLを起動する。`-` はstdinからscript source全体を読む特別なSCRIPTである。SCRIPTがなければREPLを起動する。

例:

```text
tsumugi --vm app.tsg a --vm       # VMで実行、args() == ["a", "--vm"]
tsumugi -- app.tsg --help         # treeで実行、args() == ["--help"]
tsumugi - a b                     # stdin script、args() == ["a", "b"]
tsumugi --                        # SCRIPTなしなのでREPL
```

### 6.2 却下案

- **argv全体から `--vm` をfilterする**: scriptへ同名文字列を渡せないため却下する。
- **2個目以降のpositionalをusage errorにする**: `args()` の公開契約を満たせないため却下する。
- **process-global argvをbuiltinから読む**: embedding APIとテストが実行単位で引数を注入できないため却下する。
- **`-` をREPLとして扱う**: pipeされた完全scriptと対話入力を区別できないため却下する。

### 6.3 parser状態遷移

```text
Options --(--vm / profile / capability option)--> Options(updated configuration)
Options --(--)-----> ExpectScript
Options --(-)------> Script(stdin), Args
Options --(other positional)--> Script(path), Args
Args --(any UTF-8 token)-------> append verbatim
Options/ExpectScript --(EOF)---> REPL
```

`--help` と `--version` はOptions状態でのみterminal actionとなり、表示して0で終了する。`--vm` は複数回指定してもidempotent。profile/boolean option重複、unknown option、同NAME・同KEY重複、option値欠落、profileとの不正な組合せは、いずれも診断とusageをstderrへ出し1で終了する。SCRIPT確定後は同じtokenを含めてすべてArgsへ渡す。

全argvを副作用前にUTF-8へ変換する。SCRIPTまたはARGSを含む1つでも非UTF-8なら `エラー: コマンドライン引数はUTF-8で指定してください` をstderrへ出し、終了コード1とする。

### 6.4 データモデル

```text
CliInvocation {
    backend: Tree | Vm,
    source: Repl | Stdin | File(PathBuf),
    script_args: Vec<String>,
    profile: Safe | Legacy,
    capability_options: CliCapabilityOptions,
}
```

`script_args` は `ExecutionRequest.arguments` へsnapshotとして注入し、`args()` はそのcloneを返す。tree/VMとも `std::env::args*()` を実行中に読まない。stdin scriptのsource nameは `<stdin>`、相対import基準はCLI起動時cwdとする。profileとcapability optionはSCRIPT確定前にfreezeし、safe profileのstdoutを含む全grantを`CapabilitySetBuilder`で明示構築する。

### 6.5 tree / VM変更箇所

- `src/main.rs`: argv parsingを単一関数へ分離し、tree/VMへ同じ `CliInvocation` を渡す。stdin scriptとREPLを分離する。
- `src/engine.rs`: `ExecutionContext` のhost inputへscript argsを追加する。
- `src/builtin.rs`: `args()` をcontext snapshotから返す。
- `src/vm.rs`: process argv参照を削除し、VM execution contextから返す。
- help/usageを新grammarへ更新する。

### 6.6 error / exit

CLI parse errorはTsumugi runtime errorではなく、traceなしのCLI診断である。help/versionはstdout・0、unknown option、missing file、非UTF-8、stdin read失敗はstderr・1。stdin scriptのparse/runtime errorは通常scriptと同じcanonical errorをstderrへ出し1で終了する。

### 6.7 移行

従来usage errorだった追加positionalがscript argsになる。script path後の `--vm` はbackend optionではなくなるため、VM指定はSCRIPTより前へ移す。引数を取りたいembedding hostはprocess argvではなくcontextへ明示注入する。

### 6.8 テスト行列

| source | option位置 | args | backend |
|---|---|---|---|
| file / stdin / REPL | 前 / `--`後 / script後 | 空 / 通常 / `--vm` / `--help` / `-` / Unicode | tree / VM |

help、version、unknown option、missing file、stdin read error、非UTF-8が実行前に副作用0で終了することも検証する。

### 6.9 受入基準

- grammar表の全例でtree/VMの `args()` が一致する。
- script後の全tokenが順序・内容を保つ。
- `-` は非対話script、scriptなしはREPLになる。
- runtime中にprocess-global argvを参照しない。
- 非UTF-8でpanicせず診断+1になる。

## 7. REPL submission transaction（AUD-024）

### 7.1 採用判断

未捕捉runtime errorで終了したREPL入力は、その入力が変更した**全language-stateを入力開始時点へrollback**する。正常完了と、入力内でcatchされ最終的に正常完了したerrorはcommitする。外部I/Oはrollbackせず、rollback後も「partial effectsあり」とauditへ記録する。

### 7.2 却下案

- **errorまでのprefixをcommitする**: Compiler/Envの宣言状態と値の公開状態を一貫させにくく、backend差を残すため却下する。
- **stack slotだけrollbackする**: captured cell、upvalue、List/Dict、Instance field、import markerが漏れるため却下する。
- **外部I/Oもtransaction化する**: stdout、任意filesystem、HTTP等に一般的なrollbackを提供できないため却下する。
- **context全体をdeep cloneする**: REPL状態量に比例する時間・一時メモリを毎入力で必要とするため却下する。

### 7.3 transaction状態遷移

```text
Idle
  -> Begin(submission_id)
  -> Link/Compile
      parse/link/compile failure -> AbortWithoutExecution -> Idle
  -> Execute
      completed / Exited -> Commit -> Idle
      caught error then completed -> Commit -> Idle
      uncaught runtime error -> RollbackLanguageState -> AuditPartialEffects -> Idle
      cancellation/deadline/budget abort -> RollbackLanguageState -> AuditAbort -> Idle
```

rollback対象は次を含む。

- scopeのname→cell mapping、新規・再宣言binding
- 既存cellの値、captured cell、upvalue
- List/Dictの要素・キー、nested collection
- class導入後のInstance field
- VM stack slot、locals_cells、global name→slot、frame、handler
- Compiler locals/scope/loop状態とModuleLoaderのloaded/loading marker、base directory
- import先が作成・変更したlanguage binding
- step/fuelの入力内消費状態（次入力は新予算）

rollback対象外は次である。

- FunctionId、将来のClassId等の単調増加identity counter
- stdout/stderr/input消費
- filesystem/process/network/host functionの完了済み効果
- audit event自体
- hostがcontext外で管理するrate limit、circuit breaker、secret使用回数

### 7.4 journalデータモデル

transactionはcopy-on-write journalを使う。

```text
SubmissionJournal {
    undo_log: Vec<UndoEntry>,
    seen_scope_entries: Set<(ScopeId, Name)>,
    seen_cells: Set<CellId>,
    seen_instance_fields: Set<(InstanceIdentity, FieldName)>,
    compiler_checkpoint,
    loader_checkpoint,
    vm_checkpoint,
    external_effect_count,
}

UndoEntry =
    ScopeEntry { scope, name, original: Option<Cell> }
  | CellValue { cell, original: Value }
  | InstanceField { instance, field, original: Option<Value> }
```

同じscope entry、cell、instance fieldを1入力で複数回変更しても元値は最初の1回だけ `undo_log` へ追加する。InstanceはFieldMap全体ではなく変更field単位で、不存在も `None` として記録する。rollbackは `undo_log` の逆順、commitはjournal破棄で行う。AUD-047のList/Dict COWにより `Value` journal cloneはO(1)のhandle cloneになる。これによりrollbackの記録量は変更したentry/cell/field数に比例し、Instance全field数には比例しない。

### 7.5 tree / VM変更箇所

- tree: `ExecutionContext` がsubmission transactionを開始し、`Env::set/update`、index mutation、push/pop、upvalue cell、将来のproperty mutationをjournal経由にする。
- tree: `Evaluator::run` のmodule marker rollbackと同じtransactionへ統合する。
- VM: 現行 `ReplStackCheckpoint` を拡張し、`SharedValue`への `SetLocal/SetGlobal/SetUpvalue` とindex mutationもfirst-write journalへ記録する。
- VM: Compiler/ModuleLoader checkpointをsubmission objectへ統合し、値rollbackと名前解決rollbackを同時に行う。
- host boundary: 外部操作開始/完了時にsubmission idとeffect countをauditへ通知する。

### 7.6 error / audit

元のruntime errorのkind/message/line/traceを変更しない。rollback自体に失敗した場合は元errorをcauseとして保持し、`internal` / `内部エラー: REPL state を復元できません` を返してcontextをpoison状態にする。poison状態のcontextは再利用禁止。

外部効果が1件以上完了してからrollbackした場合、[決定性・実行時監査仕様](determinism-and-audit.md)の最終`AuditEvent::Terminal`へ`context_committed = false`、`host_effects_may_remain = true`と終了理由を記録する。別名のcompletion fieldや独自eventを追加せず、script stderrへ追加warningも出さない。

### 7.7 移行

未捕捉error後に途中代入やList変更が残ることへ依存した対話操作は変わる。変更を残したい場合は `try/catch` で処理し、入力を正常完了させる。

### 7.8 テスト行列

- binding: 新規let / 再宣言 / 通常代入 / fn / import
- cell: local / global / captured / 多段upvalue / cell化前後
- collection: List / Dict / nested / push / pop / index assignment
- control: direct error / function / callback / try内で未捕捉 / caught
- external: print / file write / fake host call後のerror
- backend: tree / VM、同一processで次入力から確認
- failure: rollback中internal failureをfault injection

### 7.9 受入基準

- 未捕捉runtime error後の全language-stateが入力開始時と観測上同一になる。
- caught errorと正常入力はcommitされる。
- import再試行、closure identity、最新binding mappingが壊れない。
- 外部効果は戻さず、auditがpartial effectsを欠落なく示す。
- rollbackコストは変更量に比例し、保持中全state量には比例しない。

## 8. 未完結REPL入力でのEOF（AUD-033）

### 8.1 採用判断

継続入力bufferが空でない状態でEOFを受けた場合、bufferを破棄して正常終了してはならない。通常のLexer/ParserへEOFを渡し、parse診断をstderrへ出して終了コード1でREPLを終了する。tree/VM共通である。

bufferが空のEOFだけを正常終了（0）とする。

### 8.2 却下案

- **無言でbufferを破棄する**: typoやpipe切断を成功と誤認するため却下する。
- **EOF後もpromptを出し続ける**: 入力sourceが閉じているため進行不能になる。
- **runtime errorとして扱う**: 実行開始前の構文不完結なのでparse errorである。

### 8.3 状態遷移

```text
Reading(buffer="") + EOF -> Completed(0)
Reading(buffer!="") + EOF -> Parse(buffer + EOF) -> Diagnostic -> Failed(1)
```

lineはLexerが生成するEOF tokenの行。未閉じblockのcanonical messageは `入力が未完結です: end が必要です`。未閉じ文字列、f-string、括弧は各parser/lexerの既存canonical parse messageを使う。

### 8.4 tree / VM変更箇所

`src/main.rs` のtree/VM REPL loopで共通 `finish_repl_at_eof(buffer)` を使う。`is_incomplete` の判定だけでmessageを合成せず、実Parserの結果を表示する。

### 8.5 移行

未完結入力をpipeしていた自動処理の終了コードが0から1へ変わる。

### 8.6 テスト行列

`if/fn/while/for/try/class`、複数行lambda、括弧、List/Dict、string、f-string、commentのみ、空bufferをtree/VMのstdin subprocessで確認する。

### 8.7 受入基準

- 非空未完結buffer+EOFはparse診断・stderr・1。
- 空buffer+EOFは出力を壊さず0。
- tree/VMで診断全文と終了コードが一致する。

## 9. `path_join` 型契約（AUD-034）

### 9.1 採用判断

`path_join` は可変長引数で、**全引数がStrでなければならない**。

- 0引数: `""`
- 1引数: Rust `PathBuf`へその要素をpushした結果
- N引数: 空の `PathBuf` へ左から順に `PathBuf::push` した結果
- filesystemへのアクセス、canonicalize、存在確認は行わない
- separator、absolute component、prefixの扱いは実行OSのRust `PathBuf` と同じ

全引数の型を先に左から右へ検査し、非Strを1つでも見つけたら結果を返さない。

### 9.2 却下案

- **非Strを無言でskipする**: typoとデータ欠落を成功扱いするため却下する。
- **`to_str`で暗黙変換する**: pathに意図しない値表現が入るため却下する。
- **常に `/` で結合する**: host filesystem APIとしてWindowsの意味論とずれるため却下する。

### 9.3 データモデル / tree / VM変更箇所

`BuiltinSpec(path_join)` は `arity=Variadic(0)`、`argument_contract=All(Str)` を持つ。`src/builtin_core.rs` の実装は検査済み `&[Value]` からのみPathBufを構築する。tree/VMはAUD-049の同じregistry/handlerを使い、固有処理を持たない。

### 9.4 error

最初の非Str引数について `builtin_type`、`path_join の第 {position} 引数は Str である必要があります: {actual_type}`。lineはcall式。後続引数は既に言語式として評価済みだが、結合処理は開始しない。

### 9.5 移行

非Strが無視される挙動に依存したsourceはerrorになる。明示的に `to_str` を呼ぶ。

### 9.6 テスト行列

0/1/N、空文字、absolute component、`.`/`..`、Unicode、separatorを含むcomponent、各positionの非StrをOS別に確認する。OS依存期待値はRust `PathBuf`でtest側も構築し、固定separator文字列にしない。

### 9.7 受入基準

- 全正常caseがPathBuf相当となる。
- 非Strを一切skipしない。
- tree/VMのkind/message/lineが一致する。

## 10. lossy変換境界（AUD-036）

### 10.1 採用判断

#### Float→Int

Float入力はfiniteで、各演算後の数学値が `[-2^63, 2^63)` に入る場合だけIntへ変換する。

- `to_int`: 0方向へ切り捨て
- `floor`: 負の無限大方向
- `ceil`: 正の無限大方向
- `round`: 最も近い整数、ちょうど中間は0から遠い側

NaN、±Infinity、演算後にi64範囲外となる値は `conversion` error。`-0.0` は0になる。Float以外に対する各builtinの既存受付型は変えない。

#### file_size

OSの `u64` sizeを `i64::try_from` 相当で検査し、`i64::MAX` を超えた場合は `int_overflow` error。負値やwrapを返さない。

#### exit

`exit` は0個または1個の引数を受ける。0引数の `exit()` は終了code 0、1引数ではIntだけを受け、値域は `0..=255`。2個以上はcanonical arity error、1引数が非Intなら `builtin_type`、範囲外Intは `argument` error。coreはprocessを直接終了せず `ExecutionOutcome::Exited { code: u8, usage: BudgetUsage }` を返し、CLIだけが対応するprocess exit codeへ変換する。

### 10.2 却下案

- **Rust `as` castの飽和・0化・wrapを仕様化する**: 入力ミスと境界超過が成功値になるため却下する。
- **すべて `int_overflow` にする**: 値表現変換失敗、filesystem境界、引数範囲の区別を失うため却下する。
- **exitをi32全域にする**: OS間で観測終了コードが一致しないため却下する。
- **embedding時にもprocess exitする**: host processの安定性に反するため却下する。

### 10.3 データモデル / 状態遷移

共通helper `checked_float_to_i64(value, mode)` がfinite検査→丸め→半開区間検査→変換を行う。`i64::MAX as f64` は2^63へ丸められるため上端として受理しない。file sizeは共通checked helperへ分離し、sparse fileなしでもunit test可能にする。

`exit(valid)` はruntime errorではなくterminal outcomeであり、REPLではlanguage-stateをcommitして該当sessionを終了する。`exit(invalid)` は通常runtime errorで、未捕捉ならAUD-024に従いrollbackする。

### 10.4 tree / VM変更箇所

- `src/builtin_core.rs`: checked Float helper、file size helperを共有する。
- `src/builtin.rs`, `src/vm.rs`: exitの重複cast/process exitを削除し、同じoutcomeへ変換する。
- `src/engine.rs`: `ExecutionOutcome::Exited { code: u8, usage: BudgetUsage }` を追加する。
- `src/main.rs`: outcomeをCLI exit codeへ変換する。

### 10.5 error

- NaN: `conversion` / `{builtin} で Int に変換できません: NaN`
- Infinity: `conversion` / `{builtin} で Int に変換できません: 非有限値`
- 範囲外: `conversion` / `{builtin} で Int に変換できません: i64 範囲外`
- file size: `int_overflow` / `ファイルサイズを Int で表現できません`
- exit非Int: AUD-019のbuiltin argument型
- exit範囲外: `argument` / `exit の終了コードは 0 から 255 の範囲で指定してください: {code}`

### 10.6 移行

NaN/Infinity/範囲外が0や端値になっていたsourceはerrorになる。`exit()` は従来どおり0を要求するためsource移行不要。範囲外exit codeに依存したshell scriptは0..255へ修正する。embedding hostは `Exited` を明示処理する。

### 10.7 テスト行列

各Float builtinについて0、±0.5、±1.5、i64最小、2^63直前の表現可能値、±2^63、NaN、±Infinityを確認する。file size helperは `i64::MAX` と `i64::MAX+1`、exitは0引数、型違い、2引数、-1、0、255、256、i64極値を確認する。

### 10.8 受入基準

- 対象経路にlossy `as i64/as i32` がない。
- tree/VMの値またはerrorが全境界で一致する。
- `exit` がlibrary host processを終了しない。

## 11. List/Dict copy-on-write（AUD-047）

### 11.1 採用判断

Value表現を次へ変更する。

```text
List = Rc<Vec<Value>>
Dict = Rc<BTreeMap<String, Value>>
```

全mutationは `Rc::make_mut` を通す。cloneはhandle共有、書き込み時だけbacking collectionを複製する。言語の観測可能な**値snapshot意味論を維持**し、参照alias意味論へ変えない。

### 11.2 規範挙動

- `let b = a` でList/Dictを代入した直後は内部backingを共有してよい。
- `b[0] = x`、`push(b, x)` 等でbを変更するとdetachし、aは変化しない。
- 同じbinding cellをclosureが共有している場合、そのcellへのmutationは全closureから見える。
- forは開始時のRc snapshotを保持する。loop中に元bindingを変更するとmutation側がdetachし、反復列は開始時の要素を維持する。
- nested collectionも各階層で同じ値意味論を持つ。
- equality、display、iteration order、collection limitは変えない。

### 11.3 却下案

- **List/Dictを共有可変objectにする**: `let b=a` 後のmutationがaへ漏れ、現行意味論を破壊するため却下する。
- **AUD-041の専用opcodeだけを増やす**: callを含むindex、upvalue、REPL journal等のdeep cloneが残るため却下する。
- **unsafeなinterior mutation**: snapshotとrollbackを破壊するため禁止する。

### 11.4 データモデル / mutation遷移

```text
Read/Clone: Rc::clone(backing)
Write(binding):
  value = bindingの現在Value
  vec/map = Rc::make_mut(backing)   # strong_count > 1ならdetach
  検査後にmutation
  同じbindingへValueを書き戻す、またはcell内で置換
```

collection上限とindex/key検査はmutation前に完了し、error時にdetach済みの同値backingを残しても観測結果は変えない。可能なら検査後に `make_mut` して不要copyを避ける。

### 11.5 tree / VM変更箇所

- `src/value.rs`: variant、Clone/PartialEq/Display/Debug/truthiness/typeを更新する。
- `src/builtin_core.rs`: `assign_index`、push/pop、sort/reverse等の全mutationを `make_mut` 化する。
- `src/eval.rs`, `src/builtin.rs`: pattern matchとfor snapshot、index readを更新する。
- `src/vm.rs`: constant/local/upvalue clone、index、collection opcode、REPL journalを更新する。
- `src/compiler.rs`: AUD-041 opcodeは正当な高速pathとして残せるが、意味論の正しさを専用opcodeへ依存させない。
- heap budget実装時は同じbacking allocationを参照数分重複計上せず、detach時に新規allocationを課金する。

### 11.6 error

新しいerrorは追加しない。allocation failureをcatch可能なscript errorへ変換できないRust allocator環境では、総heap budgetでallocation前に拒否することをclass/HTTPより先に実装する。

### 11.7 移行

sourceの観測挙動は変えない。libraryで `Value::List(Vec<_>)` / `Dict(BTreeMap<...>)` を直接構築・matchするコードはRc表現へ追従する。

### 11.8 テスト行列

- alias分離: List/Dict/nested、function argument、return、closure、global
- mutation: index、push/pop、新規Dict key、既存key、callback内
- snapshot: for中の再代入・push/pop・index write
- equality/display: shared/detachedで同値
- rollback: 未捕捉error前のmutation
- scaling: `d[to_str(i)]`、upvalue read、REPL checkpointの確保量が線形

### 11.9 受入基準

- 全mutation経路が `Rc::make_mut` またはそれを包む共通helperを通る。
- alias分離とfor snapshotが現行規範どおり。
- AUD-041で残った複雑index/upvalueの二次確保を解消する。
- paired goldenとallocation scaling gateが通る。

## 12. FunctionIdによる関数同一性（AUD-048）

### 12.1 採用判断

関数値は**関数式・関数定義を実行して値を生成するたび**に、ExecutionContext内で単調増加する `FunctionId(u64)` を1つ割り当てる。clone、変数代入、引数渡し、collection格納は同じIDを保持する。tree/VMの関数等価性はFunctionIdだけで判定する。

同じsource上の `fn` をloopやfactoryで複数回評価した値は、captureの有無に関係なく異なる。aliasは等しい。

### 12.2 却下案

- **AST/Chunk pointer identity**: VMのcaptureなし関数で生成instanceを区別できないため却下する。
- **capture cell列も比較する**: closure identityと環境の構造比較を混同するため却下する。
- **関数は常に不等**: `f == f` の反射律を壊すため却下する。
- **rollback時にcounterを戻す**: 失敗入力とauditで一度使ったIDを再利用するため却下する。

### 12.3 データモデル / 状態遷移

```text
ExecutionContext {
    next_function_id: u64,
}

FunctionValue {
    id: FunctionId,
    code: TreeFnDef | VmChunk,
    captures: ...,
}

InstantiateFunction(prototype, captures):
  id = allocate_monotonic()
  if overflow: internal error before binding公開
  return FunctionValue(id, ...)
```

VM constant tableにはID付き関数値ではなく `FunctionPrototype` を置く。captureが0件でも実行時に必ず `InstantiateFunction` / `MakeClosure(0)` を通す。class methodもclass定義実行時にsource順でIDを割り当てる。rollbackしても `next_function_id` は戻さない。

### 12.4 tree / VM変更箇所

- `src/value.rs`: tree/VM関数値へ共通FunctionIdを追加し、PartialEqをID比較だけにする。
- `src/engine.rs`: allocatorをExecutionContextへ置く。
- `src/eval.rs`: FnDef/Lambda評価時に割り当てる。
- `src/compiler.rs`: function constantをprototype化し、captureなしでもruntime instantiate opcodeを生成する。
- `src/opcode.rs`, `src/vm.rs`: FunctionId allocator参照とinstantiate処理を追加する。

### 12.5 error

u64を割り当てた後さらに必要になりoverflowする場合、`internal` / `内部エラー: FunctionId を割り当てできません`。lineは値を生成しようとした関数式/定義行。通常運用で到達不能でもfault injection testを持つ。

### 12.6 移行

VMでcaptureなしの同じfn式を複数回評価した値が `true` だった非適合は `false` になる。`comparison_semantics.expected.vm` の本件差分を削除する。

### 12.7 テスト行列

named/lambda、capture有/無、factory、loop、import、REPL、clone/alias、List contains、rollback後の再生成、counter overflow、tree/VMを確認する。

### 12.8 受入基準

- 関数等価性の実装はFunctionId比較だけである。
- captureなしVM関数も評価ごとに新IDを得る。
- cloneはIDを保持し、rollbackでIDを再利用しない。
- AUD-048のbackend別期待ファイルが不要になる。

## 13. 単一BuiltinSpec registry（AUD-049）

### 13.1 採用判断

language-visible builtinを単一の静的 `BuiltinSpec` registryで定義し、treeの名前解決・dispatch、Compilerのbuiltin判定/lowering、VM dispatch、arity、context metadataをすべてそこから導出する。

概念モデル:

```text
BuiltinSpec {
    id: BuiltinId,
    name: &'static str,
    arity: Exact | OneOf | Variadic { min },
    execution: PureCore | Context,
    evaluation: EvaluateThenValidate | ValidateBeforeArgs,
    mutation_target: None | FirstArgIdentifier,
    lowering: Generic | Print | Push | Pop | HigherOrder | Input | Args | Exit,
    handler: CoreHandler | ContextHandler,
}
```

`print` はlexer上の予約tokenであっても同じregistry entryへ解決する。内部命令 `__pop_update` はpublic registryへ置かず、別の `InternalBuiltinSpec` または専用opcodeへ置く。

### 13.2 却下案

- **macroで3つのmatchを生成するだけ**: metadataとdispatchが再び分離しやすいため却下する。
- **string名をVM opcodeへ保持する**: typoをruntimeまで持ち越すため却下する。opcodeはBuiltinIdを持つ。
- **内部builtinもpublic registryへ混在**: scriptから到達可能になる危険があるため却下する。

### 13.3 データモデル / 状態遷移

compile/name resolutionは `lookup_public(name) -> Option<&BuiltinSpec>` を唯一の入口にする。runtimeは `BuiltinId` からspec/handlerを引く。arityとpre-evaluation validationはspecが決め、handlerが独自に別messageを作らない。

HostFunction registryは別物であり、BuiltinSpecへHTTP等を追加しない。名前解決はuser binding → public builtin → host registryの優先順を別途固定する。host登録名がbuiltinと衝突した場合は登録時errorとする。

### 13.4 tree / VM変更箇所

- `src/builtin_core.rs` または新規 `src/builtin_registry.rs`: registryとBuiltinIdを定義。
- `src/builtin.rs`: 公開名matchを削除しspec dispatchを使う。
- `src/compiler.rs`: `is_builtin()` の名前列挙を削除しregistry lookupを使う。
- `src/opcode.rs`, `src/vm.rs`: `CallBuiltin` 等をBuiltinIdベースにする。
- internal pop更新はprivate registry/専用opcodeへ移す。

### 13.5 error

arity/type messageはAUD-019のregistry由来テンプレートを使う。registry重複、ID重複、公開名に `__` prefix、handler欠落はbuild/test時に失敗させ、runtimeなら `internal` とする。

### 13.6 移行

source意味論は変えない。新builtin追加手順は「handler実装+BuiltinSpec 1 entry+tests」に一本化する。

### 13.7 テスト行列

全public specを列挙し、treeで名前解決、VMでcompile、正しいarity metadata、handler到達、user binding優先、print予約token、internal名非到達、重複なしを自動検査する。

### 13.8 受入基準

- public builtin名の正本が1つだけである。
- compiler/tree/VMに手書きの公開名一覧が残らない。
- `__pop_update` をsourceから呼べない。
- registry全entryの自動contract testがある。

## 14. 品質改善の実装契約

以下4件は原則として観測可能な意味論を変えない。意味論変更が必要になった場合は、該当AUDまたは別のsemantic decisionとして先に承認する。

### 14.1 VM dispatch分割

**ステータス:** 実装契約確定・未実装。

**採用判断:** `run_frames` を命令fetch、line検査、IP進行、unwindのownerとして残し、巨大な `dispatch` をカテゴリ別private handlerへ分ける。第一段階では制御型を変えず、外側のexhaustive matchから次へ委譲する。

- arithmetic/comparison
- binding/upvalue/global
- control-flow
- call/closure/class
- stack/constant
- collection/property
- builtin/host boundary

**却下案:** 一度にdirect-threaded VMへ変更、unsafe dispatch table、return/tryと全opcodeの同時再設計は、意味論退行の切り分けを困難にするため却下する。

**状態・不変条件:** IPは命令ごとに一度だけ進める。stack effect、line、try handler、call frame、REPL journal、step count、defensive Chunk検査を変えない。各handlerは不正variantを `unreachable!()` へ送らず、外側matchがvariantを一意に分類する。

**変更箇所:** `src/vm.rs` 内helper化を先行し、安定後に `src/vm/dispatch/*.rs` へ物理分割する。`src/opcode.rs` は意味論変更時以外触らない。

**error:** kind/message/line/traceはbyte-for-byte不変。内部不変条件違反は従来どおり構造化 `internal`。

**移行:** public API/source変更なし。

**テスト行列:** 全Opcodeの正常stack effect、不足stack、範囲外operand、line table欠落、try/return、callback再入、REPL rollbackをtable-drivenで検証する。

**受入基準:** `Vm::dispatch` の単一巨大matchをカテゴリへ分割し、`tests/defensive_vm.rs`、全paired fixture、scaling testが無変更で通る。新panic経路がない。

### 14.2 tree `exec_stmt` 分割

**ステータス:** 実装契約確定・未実装。

**採用判断:** `exec_stmt` をexhaustive routerとして残し、binding、index/property assignment、if、while、for、fn/class、try/catchへprivate helperを分離する。最初は同じファイル内で分割し、visibilityが安定してからmodule分割する。

**却下案:** AST visitor frameworkへの全面置換、Compilerとの共通HIR導入を同じPRで行う案は、scope/control-flow退行の原因を増やすため却下する。

**状態・不変条件:** `EvalResult::{Val,Return,Break,Continue}` とruntime errorを全経路で保持し、scopeは正常/error/return/break/continueで必ずpopする。index assignmentの評価順、loop step count、import internal guard、closure captureを変えない。

**変更箇所:** `src/eval.rs` のhelper化。必要なら後続で `src/eval/stmt.rs`。AST/valueは変更しない。

**error / 移行:** 観測出力とpublic APIは不変。

**テスト行列:** statement variant×正常/return/break/continue/error、nested scope、caught error、closure escape、REPLをtree単体とpaired fixtureで検証する。

**受入基準:** router以外に巨大なStmt matchを複製せず、全scope cleanup testとgoldenが無変更で通る。

### 14.3 sandbox OnceLockのExecutionContext capability化

**ステータス:** 実装契約確定・未実装。Phase 2 capabilityの先行縦切り。

**採用判断:** process-global `OnceLock` を廃止し、immutableな `FilesystemCapability` を実行requestへ注入する。libraryの`CapabilitySet::empty()`はfilesystem deny-by-defaultであり、CLIもsafe/legacy profile builderが必要な`DirectoryHandle` / `FileHandle` authorityを明示構築する。`ExecutionContext`は前executionのcapabilityを保持しない。

operation対応は[Capability Model仕様](capability-model.md)第8.4節を唯一の正本とする。要点は次のとおりである。

| source operation | 必要capability |
|---|---|
| source `import` | resolverに付与した`Import`。runtime `Read`から導出しない |
| `read_file`, `read_lines` | `Read` |
| `write_file`, `append_file` | `Write` + `Create` |
| `mkdir` | `Create` |
| `list_dir` | `Metadata` + `List` |
| `path_exists`, `file_size`, `is_file`, `is_dir` | `Metadata` |
| `remove`, `remove_dir` | `Delete` |
| `rename` | sourceの`Delete` + destinationの`Create`、置換時destinationの`Delete` |
| `path_join` | 不要（filesystemへ触れない） |

root scriptはhostが選択して読む入力であり、script filesystem capabilityへ暗黙追加しない。script pathは`@MOUNT/component`または`default` mountへrouteする検証済み`RelativePath`だけを受理し、absolute path、`.`、`..`、空component、backslash、drive prefix、UNCをhost pathへ変換する前に拒否する。

path認可とI/Oは同じroot-bound directory/file handleへbindする。`canonicalize`や文字列prefixで検査した後に元pathを`std::fs`へ渡すcheck/use分離は禁止する。中間・final・dangling symlinkの扱いは`SymlinkPolicy`ごとにadapterが原子的に保証し、platformが保証できなければ`SecureResolutionUnsupported`としてfail closedにする。存在path/不存在pathを許可外rootのoracleにせず、mount・operation不足ではOS call前に同じcapability denialを返す。renameは両endpointのauthorityを検査してから単一adapter callを行う。

CLI互換profileは`TSUMUGI_SANDBOX`等をCLI起動時に一度だけ読み、coreからambient参照しない。safe profileは明示optionで指定されたrootだけを`DenyAll` policyのhandleとしてgrantする。legacy profileが広いauthorityを構築する場合も、文字列fallbackではなく同じhandle契約を使い、security warningをstderrへ出す。secure handleを提供できないplatformではprofile構築を拒否する。

**却下案:** OnceLockをreset可能にする、thread-localへ移す、環境変数をI/Oごとに読む、`canonicalize`後に元pathを再利用する案は、実行単位のauthorityまたはTOCTOU耐性を満たさないため却下する。

**状態・不変条件:** 同一processの2 executionが異なるpolicyを持てる。capabilityはstart時にfreezeし、cancel以外の途中revokeはない。read/writeとfinal directory entry操作のsymlink意味論を混同せず、secure resolution不能時に文字列checkへfallbackしない。

**tree / VM変更箇所:** `src/sandbox.rs` のprocess-global stateを廃止し、`src/engine.rs`のrequest、`src/eval.rs`/`builtin.rs`、`src/builtin_core.rs`、`src/module.rs`、`src/vm.rs`、`src/main.rs`へ同じfrozen capabilityを伝播する。詳細型をこれらの箇所で再定義せず、capability文書のadapterを共有する。

**error:** lexical path不正はcatch可能なcanonical `argument` error、script操作中のauthority不足とsecure adapter失敗は第3節のcatch可能な`capability` / `host` errorとする。link中にhandlerが存在しないresolver denial/host failureだけは構造化terminal `Denied` / `HostError`となる。path全文やhost absolute pathをmessageへ出さない。

**移行:** embeddingの既定requestはfilesystem denyとなる。CLI safe profileは明示optionだけをgrantし、legacy互換もcoreの環境参照や文字列path検査を復活させない。

**テスト行列:** 同一processでallow/deny policyを交互実行し、各operation、qualified/default/missing mount、absolute/`.`/`..`/separator/drive/UNC、存在/不存在target、intermediate/final/dangling symlink、全`SymlinkPolicy`、rename両endpointと置換、import専用authority、tree/VM、並列executionを検証する。secure resolution不能adapterとsymlink交換raceでroot外変更0を確認する。

**受入基準:** `SANDBOX_PATHS`/OnceLockがなく、core filesystem/import経路が明示capabilityなしにadapter/OSを呼ばない。全path操作がroot-bound handle契約を通り、文字列check/use fallbackが0件である。

### 14.4 cargo-fuzz

**ステータス:** 実装契約確定・未実装。

**採用判断:** 通常testと分離した `fuzz/` workspaceを作り、次のtargetを段階導入する。

1. `frontend`: bounded UTF-8 bytes → Lexer → Parser
2. `compiler`: parse成功Program → Compiler
3. `vm_chunk`: bounded arbitrary Chunk → VM
4. `differential_pure`: 副作用なし生成Programのtree/VM比較
5. `evaluator`: capability deny、有限budget、fake I/O下での実行

依存は導入時にexact versionでpinし、PR smokeとschedule/manual long runを分ける。crash artifactは最小化し、通常unit/integration regressionへ昇格してからartifactを整理する。

**却下案:** 無制限sourceをそのままprocess I/O付きCLIでfuzzする、PRごとに長時間fuzzする、crash corpusだけをCI artifactに残して回帰testへしない案は却下する。

**状態・安全契約:** input bytes、AST node、instruction、constant、collection、step、wall time、memoryをboundedにする。`exit/input/filesystem/network` はfakeまたはdeny。panic、abort、hang、OOMを失敗とし、成功または構造化errorだけを許す。

**変更箇所:** 新規 `fuzz/Cargo.toml`、`fuzz/fuzz_targets/*`、seed corpus、CIの独立job。root packageの通常runtime依存へfuzzer依存を入れない。

**error / 移行:** 言語error契約は変えない。既知の意図的差はallowlistではなく、本書対象の差を先に解消してからdifferential targetを有効にする。

**テスト行列:** random token/UTF-8、深いunary/elif/f-string、巨大operand、欠損line table、不正jump、try/call/closure、limit境界をseedに含める。

**受入基準:** short smokeがCIで決定時間内に通り、長時間jobを手動/schedule実行できる。発見したcrashは再現可能な通常testへ昇格する。外部I/Oやprocess exitへ到達しない。

## 15. クラス（継承なし）の完全設計

設計ステータス: **設計済み・低優先度**

### 15.1 採用判断

データと操作を束ねる最小クラスを導入する。単一・多重を問わず継承、super、override chain、metaclass、static/class methodは導入しない。機能共有はfieldへ別instanceを持つ**合成**で行う。

### 15.2 grammar

```ebnf
class_def       = "class" IDENT NEWLINE class_member* "end" NEWLINE ;
class_member    = method_def ;
method_def      = "fn" IDENT "(" [ params ] ")" NEWLINE block "end" NEWLINE ;

primary         = ... | "self" ;
postfix         = primary { call_suffix | index_suffix | property_suffix } ;
property_suffix = "." IDENT ;
assignment_stmt = postfix "=" expr NEWLINE ;
```

Parserは左辺postfixを最後まで構築した後、次tokenが `=` なら完成したASTをassignable targetとして検査する。root nodeが `Expr::GetProperty { object, name }` なら `Stmt::PropertyAssign` へ分解する。これにより `a.b = v` はreceiver `a`、name `b`、`a.b.c = v` はreceiver `a.b`、name `c` と一意に決まる。rootがGetPropertyでない場合は、既存のIdent assignmentまたはIdent直下のIndexAssign規則だけを許し、それ以外をparse errorにする。

- class bodyに置けるのはmethod定義、空行、commentだけ。field宣言や実行文は置けない。
- `class` と `self` は予約語。`init` は通常のidentifierだがmethod名として特別なconstructor契約を持つ。
- method parameterに `self` を明記しない。暗黙bindingとして提供する。
- `self` はmethod本体と、その内側で定義されたclosureから利用できる。class外ではparse error。
- `self = value` はparse error。`self.field = value` は有効。
- `a.b.c`、`a.b().c`、`a[i].b` を左結合postfixとして許す。
- `a.b = value` のreceiver `a` は任意のpostfix式でよいが、一度だけ評価する。
- `class Child < Parent`、`class Child(Parent)` 等の継承構文は `継承はサポートされていません` というparse errorにする。

### 15.3 AST

```text
Stmt::ClassDef {
    name: String,
    methods: Vec<MethodDef>,
    line: usize,
}

Stmt::PropertyAssign {
    object: Expr,
    name: String,
    value: Expr,
    line: usize,
}

Expr::SelfRef { line }
Expr::GetProperty { object: Box<Expr>, name: String, line }

MethodDef {
    name: String,
    params: Vec<String>,
    body: Vec<Stmt>,
    line: usize,
}
```

`Stmt::line`、AST depth preflight、`referenced_names`、`is_side_effect_free`、f-string子Parserを全variantへ対応させる。`SelfRef` は外側Env名としてcaptureせず、method call時の特別localを参照する。method内closureはそのlocal cellを通常upvalueとしてcaptureできる。

同一class内のmethod名重複は2つ目の定義行でparse error。`init` は0個または1個だけ。

### 15.4 runtimeデータモデル

```text
Class {
    name: String,
    methods: BTreeMap<String, FunctionValue>,
}

Instance {
    inner: Rc<RefCell<InstanceData>>,
}

InstanceData {
    class: Rc<Class>,
    fields: BTreeMap<String, Value>,
}

BoundMethod {
    receiver: Instance,
    method: FunctionValue,
}
```

- Classは定義実行ごとに新しいidentityを持つimmutable値。
- Instanceはreference semantics。`let b = a; b.x = 1` はaからも見える。
- BoundMethodはreceiverをstrong参照し、元のinstance変数がscopeを抜けても呼べる。
- ClassとInstanceは同じidentityだけ等しい。BoundMethodは同じInstance identityかつ同じmethod FunctionIdの場合だけ等しい。
- Class/Instanceを構造比較しない。
- truthinessはすべてtrue。
- `type()` はそれぞれ `"class"`、`"instance"`、`"bound_method"`。
- displayは `<class {Name}>`、`<{Name} instance>`、`<bound method {Name}.{method}>`。field値を自動展開しない。

BoundMethodまたはselfをinstance自身のfield/containerへ保存するとRc cycleを構成し得る。cycle collector導入までは既知の保持として扱い、class実装のproduction有効化前に総heap budgetを必須とする。receiverをWeakにしてbound methodの寿命を壊す案は採らない。

### 15.5 class定義とcapture

ClassDef実行は宣言全体をatomicに扱う。最初に現在scopeへAUD-016のfresh class cellを予約し、値を内部非公開の `UninitializedClass` sentinelにする。次にmethodをsource順で評価して各methodへAUD-048のFunctionIdを割り当てる。methodは予約済みclass cellを含む、class定義地点のlexical bindingを通常関数と同じ規則でcaptureする。`self` はcapture対象外。全methodからClassを構築できた後、同じ予約cellをClass値で置換して宣言を公開する。

method生成、FunctionId、BuildClassのいずれかが失敗した場合は、通常file実行を含めscope mappingをClassDef開始前へ戻し、sentinelを外部へ公開しない。再宣言では旧class cellを置換せず、新しい予約cellを作るため、旧classのmethodは旧cell、新classのmethodは新cellを参照する。これによりmethod内の宣言class名参照と再宣言identityが一意になる。VMは新slotを先にcell化してsentinelを置き、method closureをそのcellへcaptureさせ、`BuildClass`成功後に同じcellへ書く。top-level global registryへの公開は成功後に行う。

import先のClassDefは既存source importと同様にcaller top-levelへ展開される。native moduleやnamespaceをclass importに便乗して導入しない。

### 15.6 construction / `init`

Class値はcallableである。

1. classに `init` がなければ引数0個だけを許す。
2. `init` があればsource上のparameter数をconstructor arityとする。暗黙selfは数えない。
3. arity検査後、引数を左から右へ評価する。
4. 空fieldsのInstanceを生成する。
5. `init` をBoundMethodとして1 user frameで呼ぶ。
6. `init` が暗黙終了または `return null` なら、class callはInstanceを返す。
7. Null以外をreturnした場合はtype errorとし、Instanceを返さない。

`obj.init(...)` を通常methodとして直接呼ぶこともでき、その戻り値はNullである。これにより `return null` をearly returnへ使える。

### 15.7 field / method lookup

property readは次の順序。

1. receiverを評価しInstanceであることを検査。
2. fieldsに同名があればfield値を返す。
3. Class methodsに同名があればfresh BoundMethodを返す。
4. なければ `name` error。

fieldはmethodをshadowできる。property assignmentは存在しないfieldも作成でき、method名と同名でも許す。field削除は提供しない。Class値自体へのproperty access、static field、methodの差替えは提供しない。

property assignmentはreceiver評価→Instance検査→value評価→first-write journal→field更新の順。receiverが不正ならvalueを評価しない。AUD-024のREPL rollback対象である。

### 15.8 callable / self-binding

既存の「名前付き関数が宣言名で自身を参照するself-binding」と、予約語 `self` は別である。

- methodのslot/binding 0: 現在のBoundMethod（宣言名での再帰用。receiverを保持）
- methodのslot/binding 1: slot 0と同じreceiverの暗黙self
- 以後: source parameter

parameter名で `self` は使えない。treeはmethod frameへ宣言名→現在のBoundMethodをbindingし、VMはBoundMethodをslot 0へ置いて、そのunderlying FunctionValueのcodeを実行する。したがってmethod bodyから宣言名を呼んでもreceiverを失わず、通常のBoundMethod callとしてslot 1へ同じselfが入る。methodをfieldから取り出したBoundMethodはreceiverを保持し、後で呼んでも同じselfを使う。`self.method()` は通常のproperty lookupを行うため、同名fieldがmethodをshadowしていればそのfieldをcallしようとする。

### 15.9 tree変更箇所

- `src/lexer.rs`: Class/Self/Dot token。
- `src/parser.rs`: class body、method context、postfix dot、PropertyAssign、重複method、継承拒否。
- `src/ast.rs`: 新variantと全非再帰traversal。
- `src/value.rs`: Class/Instance/BoundMethod、equality/display/type/truthiness。
- `src/eval.rs`: ClassDef、GetProperty、PropertyAssign、汎用callable dispatch。
- `src/env.rs`: self special bindingは通常cellとしてframeへ注入し、closure capture可能にする。
- `src/main.rs`: `is_incomplete` がclass/endを数える。

### 15.10 VM変更箇所 / opcode

CompilerはmethodをFunctionPrototypeとしてcompileし、class定義実行時に全methodをinstantiateする。

```text
BuildClass { name, method_names }  # stack上のN methodをpopしClassをpush
GetProperty { name }               # receiverをpopしfield/BoundMethodをpush
SetProperty { name }               # receiver,valueをpopしmutation、valueをpush
```

ClassDef statementはfresh slot/cellへ `UninitializedClass` を置き、method closure生成と `BuildClass` に成功した後で同じcellへClassを書き、通常宣言として公開する。失敗時はslot/global mappingを宣言前へ戻す。PropertyAssign statementは `SetProperty` 後のvalueをPopする。

`ValidateCall` / `Call` はFnに加えてClass/BoundMethodを扱う。BoundMethod callは物理frameを `[bound_method, receiver, args...]` とし、CallFrameは `bound_method.method` のcodeを実行する。これによりcompilerのslot 0/1規則と宣言名再帰を同時に満たす。Class callはInstanceを生成し、`init` BoundMethod frameに `ReturnMode::Constructor(instance)` を設定する。initの正常Null return後はcallerへInstanceをpushする。try unwind、trace、depth、step、REPL journalは通常user callと同じ経路を使う。

不正ChunkのBuildClass method数、property name constant、stack不足はpanicせず `internal` error。

### 15.11 class error契約

| 条件 | kind | message / line |
|---|---|---|
| class外のself | parse | `self はメソッドの中でのみ使用できます` / self行 |
| selfへの代入 | parse | `self には代入できません` / assignment行 |
| class bodyの非method | parse | `クラス本体にはメソッドのみ定義できます` / 該当行 |
| method重複 | parse | `メソッドが重複しています: {name}` / 2つ目 |
| 継承構文 | parse | `継承はサポートされていません` / class行 |
| 非Instance property read/write | AUD-019の`type` | dot/assignment行 |
| missing property | `name` | `プロパティが見つかりません: {Class}.{name}` / dot行 |
| constructor/method arity | `argument` | AUD-019 callable template / call行 |
| init non-Null return | `type` | `{Class}.init は Null 以外を返せません: {type}` / return行 |
| `BuildClass`のmethod stack不足 | `internal` | `内部エラー: BuildClass のメソッドスタックが不正です` / opcode行 |
| `BuildClass`へ非関数method値 | `internal` | `内部エラー: BuildClass に関数以外のメソッド値が渡されました` / opcode行 |
| property名constant不正 | `internal` | `内部エラー: プロパティ名定数が不正です` / opcode行 |
| constructor frame戻り状態不正 | `internal` | `内部エラー: constructor frame の戻り状態が不正です` / call行 |
| Instance生成前のlogical heap budget不足 | `budget` | `heap の上限を超えました (上限: {limit})` / class call行 |

trace frame名は `{Class}.{method}`。constructor arity失敗ではinit frameを追加しない。

### 15.12 移行

`class` と `self` が予約語になるため、既存の同名変数・関数はrenameが必要。`.` は従来Unknown tokenだったため既存の有効sourceとは衝突しない。List/Dictは値意味論、Instanceはreference意味論であることをmigration guideへ明記する。

### 15.13 テスト行列

- parser: 空class、複数method、重複、禁止body、継承拒否、self文脈、dot chain、property assignment
- construction: init有無、0/N args、implicit/`return null`、non-Null return
- fields: create/read/update、method shadow、missing、receiver/value評価順
- methods: direct、extracted BoundMethod、receiver生存、宣言名によるdirect/extracted method再帰、method同士、closure capture、self capture
- identity: Class/Instance/BoundMethod alias・別instance、FunctionIdとの関係、contains
- scope: class名のmethod内参照、class再宣言時の旧/new class cell、class構築失敗時の予約cell rollback、block、REPL、import
- control: try/catch、return、callback、depth128、stack trace
- VM defensive: stack不足、invalid method table/property constant
- lifecycle: intentional cycleがheap budgetで停止し、host OOMへ至らない

### 15.14 受入基準

- grammar、AST、tree、Compiler/VM、error、REPL、importが同一PR系列で完成し、片backendだけをreleaseしない。
- paired matrixでstdout/stderr/value/identity/traceが一致する。
- 継承に関するruntime fieldやopcodeを持たない。
- BoundMethodを取り出してもreceiverが生存する。
- Instance mutationがAUD-024でrollbackでき、総heap budgetでcycle保持を制御できる。

## 16. HTTP host adapter

設計ステータス: **設計済み・着手禁止**

### 16.1 着手gate

次をすべて満たし、具体的ユースケースが設計レビューで承認されるまで、HTTP client依存追加、adapter実装、DNS接続testを開始してはならない。

1. Roadmap Phase 1: tree/VMを包含するstable Engine/CompiledScript/ExecutionContext/Outcome。
2. Phase 2: deny-by-default capabilityとHostFunction registry。
3. Phase 3: host call数、request/response bytes、総heap、deadlineを含む実行budget。
4. Phase 4: cancellation、同時実行上限、backpressure。
5. Phase 5: host注入resolver/clock/I/Oと正式backend境界。
6. Phase 6: [決定性・実行時監査仕様](determinism-and-audit.md)の`ExecutionStarted`、`CapabilityDecision`、`HostCallStarted`、`HostCallFinished`、`BudgetCharged`、`Yielded`、`Resumed`、`Terminal`とfail-closed sinkを実装済みである。HTTP専用の別event enumを定義しない。
7. 承認対象ユースケースについて、宛先、method、data分類、secret owner、rate、SLO、失敗時処理、運用責任者が文書化されている。
8. threat reviewでSSRF、DNS rebinding、redirect、secret漏洩、response size、timeoutが承認されている。

Phase 7のcapability/budget/audit/fuzz matrix通過はrelease gateとする。

### 16.2 採用判断

HTTPは言語core builtinでもsource `import` moduleでもなく、core外のoptional host adapterとして提供する。coreはHTTP client crateへ依存しない。hostがadapterのHostFunctionをregistryへ登録し、ExecutionContextへ対応capabilityを付与した場合だけ利用できる。登録と権限付与は別の二鍵とする。

canonical adapter surfaceは1引数のhost functionとする。

```tsg
let response = http_request({
    "method": "GET",
    "url": "https://example.invalid/data",
    "headers": {"accept": "application/json"},
    "body": null
})
```

request Dict:

| key | 型 | 必須 | 契約 |
|---|---|---|---|
| `method` | Str | yes | ASCII token、uppercase正規化後policy検査 |
| `url` | Str | yes | absolute URL |
| `headers` | Dict<Str,Str> | no | default `{}`、禁止headerあり |
| `body` | Str または Null | no | UTF-8 bytes、default Null |

unknown keyは `argument` error。responseは次のDict。

```text
{
  "status": Int,
  "headers": Dict<Str, List<Str>>,
  "body": Str,
  "url": Str
}
```

header名はASCII lowercase、重複値は受信順List、`url` は最終URL。response bodyが有効UTF-8でなければ `host` errorとし、lossy変換しない。binary bodyは次期Bytes型または別adapterまで対象外。JSONの自動parseは行わない。

### 16.3 却下案

- **`http_get` をbuiltin_coreへ追加**: capability、budget、auditを迂回し依存をcoreへ固定するため却下する。
- **現行 `import "http"` をnative moduleに流用**: filesystem source importとhost registryの責務が混ざるため却下する。
- **URL allow-listだけでSSRF対策完了とする**: DNS解決後IP、redirect、proxyで迂回できるため却下する。
- **TLS検証無効化optionをscriptへ渡す**: secretと通信内容を保護できないため禁止する。
- **responseを上限確認後に一括readする**: header詐称・chunked responseでOOMし得るため却下する。
- **自動retry**: 非冪等requestの重複外部効果を起こすため初版では行わない。

### 16.4 adapter境界

```text
HostFunction dispatcher
  -> resolve symbol / validate arity
  -> reserve host-call count / deadline / cancellation
  -> evaluate the single argument
  -> validate request shape/type and header syntax
  -> normalize URL and evaluate coarse/target capability
  -> reserve request-header/body, redirect budget, audit lifecycle 3 events
  -> append+ack HostCallStarted
  -> append+ack CapabilityDecision(Allow | Deny)
  -> Denyならadapter call 0でHostCallFinished(Denied)をappend+ackしcanonical capability error
  -> Allowなら最初のDNS query以前に外部処理を開始
  -> resolve DNS via injected resolver
  -> validate every resolved address
  -> execute through injected transport
  -> validate every redirect hop
  -> stream/charge headers and body
  -> append+ack HostCallFinished(Success | HostError | Cancelled | Detached)
  -> return Value or canonical error
```

symbol/arity、host-call回数budgetは引数評価より前に検査する。URL等の引数値に依存するpolicyはshape/type検査後、外部処理前に検査する。粗粒度またはURL policyの拒否でも、同じcall IDの`HostCallStarted`、`CapabilityDecision(Deny)`、`HostCallFinished(Denied)`を順に記録し、adapter/DNS/transport callを0回にする。allowでは`CapabilityDecision(Allow)`のsink ack後だけ外部効果を開始し、DNS failure/timeout/cancelを含むすべての終了を`HostCallFinished`の対応outcomeで1回だけ閉じる。

adapterはEvaluator、Env、VM stackへアクセスしない。共通dispatcherが認可・引数評価・shape検査・audit開始を担当し、adapterへ渡すのは認可済みrequestと狭いcontrolだけである。概念interfaceは次。

```text
HttpAdapter::execute(
    approved_request,
    HostCallControl { deadline, cancellation, budgets, audit_context }
) -> Result<HttpResponse, SanitizedHostError>
```

resolver、clock、transport、rate limiterを注入可能にし、unit testで実networkを使わない。

### 16.5 capability

`HttpCapability` は少なくとも次を持つ。

- 許可method
- 許可scheme（defaultは`https`のみ、`http`は明示grant）
- IDNA/ASCII正規化後host allow-list
- port allow-list
- 任意のpath prefix
- redirect許可と最大hop
- request/response byte上限
- connect/read/total timeout上限
- secret injection policy
- proxy policy

URLはabsoluteのみ。userinfoとfragmentを拒否する。曖昧なIPv4表記、zone id、scheme-relative URLを拒否する。host名比較は末尾dotとcaseを正規化し、suffix文字列一致ではなくlabel境界で行う。

### 16.6 SSRF / DNS

- hostnameはinjected resolverでA/AAAA解決し、**返った全address**を検査する。
- loopback、private、link-local、unspecified、multicast、carrier-grade NAT、benchmark/documentation、cloud metadataとしてpolicyが禁止するrangeをdefault denyする。
- IP literalも同じ検査を通す。
- 許可済みSocketAddrへconnect先をpinし、TLS SNI/hostname検証にはcanonical hostnameを使う。再解決した未検査addressへtransportが接続してはならない。
- 接続retryで別addressを使う場合も、事前検査済み集合内だけにする。
- proxyはdefault無効。hostが明示構成する場合もproxy先と最終宛先を別々に認可し、環境変数proxyをambientに読まない。

### 16.7 TLS

- certificate chainとhostname検証を必須とする。
- scriptからverification disable、任意CA、client certificate pathを指定できない。
- custom trust storeとmTLS secretはhost設定だけで注入し、対象originへbindする。
- TLS error詳細はsecret/pathを除去して `host (... tls)` に正規化する。

### 16.8 redirect

redirectはdefault無効。許可時も各hopでscheme/host/port/path、DNS/IP、secret、budgetを最初から再検査する。

- 最大hopはcapabilityとbudgetの小さい方。
- 303はGETへ変更しbodyを破棄。
- 307/308はmethod/bodyを維持。
- 301/302でGET/HEAD以外を別methodへ暗黙変更せず、`host (... redirect_policy)` error。
- originを跨ぐhopへAuthorization、Cookie、host secretを転送しない。
- redirect loopは `host (... redirect_loop)`。

### 16.9 secret / header

request header名はASCII case-insensitiveで扱い、検査前にlowercaseへ正規化する。nameはRFC token相当のASCIIだけを許し、valueはCR、LF、NULとその他の禁止control characterを拒否する。正規化後のAuthorization、Proxy-Authorization、Cookie、Host、Content-Length、Transfer-Encoding、Connection等のsensitive/hop-by-hop headerはargument-dependent capability denialとし、外部処理前に`CapabilityDecision(Deny)`と`HostCallFinished(Denied)`を記録してcanonical `capability` errorを返す。header name/valueのsyntax不正は `argument` errorであり、host call lifecycle開始前に判定する。検査順はheader名のcodepoint順とし、各entryでname syntax→forbidden name→value syntaxの順に最初の失敗を返す。

host secretはpolicyにより特定origin・path・methodへbindし、adapterがtransport直前に注入する。secret値はTsumugi Value、error、trace、audit、debug logへ入れない。

responseの `set-cookie` は初版ではscriptへ返さない。response headerに禁止control characterがあれば、同じcall IDの`HostCallFinished(HostError)`を記録して `host (... protocol)` errorとする。

### 16.10 body / budget / timeout / cancellation / rate

- request header bytes、request body bytes、response status/header count/header bytes/body bytesを個別に制限する。
- responseはstreamし、chunk受信ごとにbudgetを消費する。上限超過時は接続を中断し、残りを読み続けない。
- effective total deadlineはexecution deadline、capability上限、request policyの最小値。
- connect/read timeoutはtotal deadlineを超えない。
- cancellation tokenをDNS、connect、TLS、write、readの各待機へ伝播し、可能な限りtransportをabortする。
- host/tenant/origin単位のtoken bucketと同時実行semaphoreをhostが提供する。rate waitがdeadlineを超える場合は待たずにbudget/timeout error。
- 自動retryは初版なし。将来追加する場合はmethod idempotency、retry回数、追加budget、auditを別決定する。

### 16.11 audit / REPL transaction

HTTPは独自eventを定義せず、[決定性・実行時監査仕様](determinism-and-audit.md)第7〜11節のhost call lifecycleを使う。各requestは同じoperation ID/call IDの`HostCallStarted`、`CapabilityDecision(Allow | Deny)`、`HostCallFinished(Success | Denied | HostError | Cancelled | Detached)`をこの順で持ち、denyを含め必ずpairを閉じる。allow decisionがackされる前にDNS・transport等の外部効果を開始せず、finishedがackされる前にresponseをscriptへ公開しない。`ExecutionStarted`、集約`BudgetCharged`、必要な`Yielded`/`Resumed`、最後の`Terminal`も同じschemaを使い、sink failureは`FailClosed`とする。

eventはexecution ID、script hash、host function名、capability policy、method、redacted origin/path、redirect数、request/response byte、duration、終了categoryを既存fieldへ写像する。query、userinfo、Authorization/Cookie、request/response body、secretは既定で記録しない。

HTTP request完了後に同じexecutionが未捕捉errorでrollbackしてもHTTP効果は戻らない。AUD-024に従い、最終`AuditEvent::Terminal`へ`context_committed = false`、`host_effects_may_remain = true`を記録する。host call completionへ独自のlanguage-state fieldを追加しない。

### 16.12 error契約

| 条件 | kind | canonical message |
|---|---|---|
| adapter未登録 | `name` | `未定義の変数または関数: http_request` |
| coarse/target capability拒否 | `capability` | `host function の実行が許可されていません: http_request` |
| 第1引数がDictでない | `builtin_type` | `http_request の第 1 引数は Dict である必要があります: {actual_type}` |
| 必須key `method` / `url` がない | `argument` | `http_request の必須キーがありません: {key}` |
| unknown request key | `argument` | `http_request に未対応のキーがあります: {key}` |
| `method` / `url` がStrでない | `builtin_type` | `http_request.{key} は Str である必要があります: {actual_type}` |
| `headers` がDictでない | `builtin_type` | `http_request.headers は Dict である必要があります: {actual_type}` |
| header valueがStrでない | `builtin_type` | `http_request.headers の値は Str である必要があります: {actual_type}` |
| request header name syntax不正 | `argument` | `http_request.headers の名前が不正です: {header}` |
| request header valueに禁止文字 | `argument` | `http_request.headers の値が不正です: {header}` |
| sensitive/hop-by-hop header | `capability` | `host function の実行が許可されていません: http_request` |
| `body` がStr/Nullでない | `builtin_type` | `http_request.body は Str または Null である必要があります: {actual_type}` |
| method token / URL syntax不正 | `argument` | `http_request.{key} が不正です: {reason}` |
| DNS/IP/redirect/TLS/protocol/UTF-8/transport | `host` | `host function の実行に失敗しました: http_request ({category})` |
| byte/call/concurrency budget | `budget` | `{budget_name} の上限を超えました (上限: {limit})` |
| deadline | `timeout` | `実行期限を超過しました` |
| cancellation | `cancelled` | `実行がキャンセルされました` |

`{reason}` は `invalid_method`、`invalid_url` のいずれか、`{category}` は `dns`、`address_policy`、`redirect_policy`、`redirect_loop`、`tls`、`protocol`、`utf8`、`transport` のいずれかに限定する。OS/libraryの生messageを埋め込まない。request Dictの検査順は `method`存在→`url`存在→unknown keyのcodepoint順→method型→url型→headers型/各値のStr型→body型→method token→URL syntax→header名のcodepoint順にname syntax/forbidden name/value syntax→method/scheme/host/port/path policy とし、最初の失敗だけを返す。`{header}` はlowercase正規化後のheader名で、control characterを含む未正規化値をmessageへ出さない。

lineは `http_request(...)` call式。host内部traceは出さない。`budget/timeout/cancelled` はscriptでcatch不能、`capability/host/argument/builtin_type` はcatch可能とする。

### 16.13 移行

HTTPは新規optional機能であり、adapter未登録contextでは名前未定義のまま。core package、CLI、default ExecutionContextがnetwork権限を暗黙付与してはならない。利用hostはcapability、budget、audit、secret policyを明示設定する。

### 16.14 テスト行列

- registry/capability: 未登録、登録deny、grant、名前衝突、deny時の引数副作用なし
- request shape: 非Dict、必須key欠落、unknown key、各key/nested header/body型、validation precedence
- URL: scheme、userinfo、IDNA、末尾dot、IP literal、曖昧IP、port、path
- DNS: public/private混在、rebind、複数A/AAAA、resolver failure/timeout/cancelと`HostCallStarted`→`CapabilityDecision`→`HostCallFinished`のcorrelation
- redirect: same/cross origin、loop、hop上限、301/302/303/307/308、secret除去
- TLS: valid、expired、hostname mismatch、unknown CA、mTLS host injection
- header/secret: forbidden request header、control char、set-cookie除外、audit redaction
- body: content-length有無、chunked、exact limit、limit+1、invalid UTF-8
- control: connect/read/total timeout、cancel各phase、rate、concurrency、deadline
- semantics: caught host error、uncatchable timeout、REPL rollback後partial effect audit
- backend: fake adapterでtree/VMの引数順、error、audit一致

実network統合testは隔離したlocal test server/resolverで行い、public Internetへ依存しない。

### 16.15 受入基準

- 着手gateの承認記録が存在するまでHTTP依存・実装がrepositoryへ入らない。
- core crateはHTTP clientへ依存しない。
- 全接続先がURL policyとDNS後IP policyの両方を通る。
- redirect全hop、secret、stream body、deadline/cancel/rateがfake transportで決定的に検証される。
- canonical host call lifecycle、capability decision、terminalのaudit欠落がなく、sink failure時にfail-closedとなる。
- default context/CLIからnetworkへ到達できない。

## 17. 実装順

次期revisionは以下の順で進める。各段階でfmt、Clippy、全test、paired golden、defensive testを通し、意味論変更と内部リファクタを同じcommitへ混在させない。

1. **基準固定**: 本書のerror templates・line/trace・評価順をtest helperへ定義する。現行非適合fixtureを明示する。
2. **内部リファクタ**: VM dispatch分割、tree `exec_stmt` 分割。観測挙動は変えない。
3. **上限統合**: AUD-050の `limits.rs` 集約とAUD-017の128 user frame統一。
4. **Builtin/error基盤**: AUD-049単一registry、AUD-019共通error constructorと完全一致test。
5. **値表現**: AUD-047 List/Dict COW、続けてAUD-048 FunctionId。scaling/identity testを先に追加する。
6. **binding/transaction**: AUD-016 VM fresh cell、AUD-024全language-state REPL journal。FunctionId counterをrollback対象外に固定する。
7. **境界挙動**: AUD-034 `path_join`、AUD-036 checked変換/Exited、AUD-018 CLI args/stdin、AUD-033 EOF診断。
8. **capability縦切り**: sandbox OnceLockをExecutionContext FilesystemCapabilityへ移し、tree/VM/importを同じpolicyへ接続する。
9. **検証基盤**: cargo-fuzzのfrontend/compiler/vm_chunk、続いてcapability完成後にevaluator/differential target。
10. **次期仕様反映**: 全受入基準通過後にだけ `language-spec.md`、`LANG_GUIDE.md`、設計文書、revisionを更新する。
11. **クラス**: 上記基盤と総heap budget完成後、低優先度機能としてlexer→AST→tree→VM→paired testの順で実装する。継承は含めない。
12. **HTTP**: 現在の実装順には含めない。Phase 1–6完了と具体ユースケース承認後にgateを再評価し、承認された場合だけ別計画を作る。

## 18. 文書全体の最終受入基準

- AUD-016/017/018/019/024/033/034/036/047/048/049/050の採用判断、却下案、規範挙動、状態、変更箇所、error、移行、test、受入基準が実装PRから追跡できる。
- tree/VMで意図しないbackend別期待値が残らない。
- 現行 `language-spec.md` は実装完了まで変更せず、現行挙動の正本として維持する。
- クラスは「設計済み・低優先度」、HTTPは「設計済み・着手禁止」として扱われる。
- 本書の承認だけをもってコード実装済み、security sandbox完成、HTTP利用可能とは表示しない。
