# Tsumugi — 次期意味論・実装決定

最終更新: 2026-10-20

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

### 3.12 表示レイヤのi18n（ロケール別出力）— 現時点で見送り

canonical error契約の延長として、エラーメッセージ多言語化（ロケール別出力）の見送り判断をここに記録する。

#### 経緯

エラーメッセージは現在すべて日本語で固定している（`src/error.rs` の operation 別 canonical constructor がテンプレートを持ち、`Display` が `N行目: ...` を組み立てて CLI が stderr へ出す）。利用者の環境に合わせて出力言語を切り替えたいという発想から、「実行ホストの timezone を見て JST なら日本語・それ以外は英語で出力し、将来は他ロケールへも拡張する」というアイデアを検討した。

#### 結論: 現時点では見送り

技術的には実現可能だが、現在の設計正本と正面から衝突するため、現時点では実装しない。アイデア自体は破棄せず、下記の条件が整った段階で「表示レイヤ限定の i18n」として再検討する。

#### 見送りの理由

1. **canonical error 契約（AUD-019 / 本章）と衝突する。** runtime error は operation ごとの共通 constructor から `kind` / canonical message / line を生成し、その message テンプレートを「完全一致の正本」として扱う。tree/VM 両 backend で stdout・stderr・kind・message・line・trace が完全一致することを `tests/canonical_error_inventory.rs` で検証している。出力言語が環境で変わると、この完全一致 golden test が環境依存になって壊れる。
2. **「規範出力を locale 差で変えない」という既存の設計判断に反する。** 第3.2節は「OS エラー文字列を canonical message へ埋め込む」案を、OS・locale 差が規範出力になることを理由に却下している。timezone による言語切り替えは、まさにこの却下理由に該当する。
3. **timezone から言語を推定するのは筋が悪い。** UTC 運用のサーバー上の日本語話者、JST 環境の英語話者など、timezone と希望言語は一致しない。マニフェスト原則2「時刻も含む外部効果は capability として明示的に付与する」の思想とも噛み合わず、環境からの暗黙推定は避けたい方向である。
4. **今は canonical message 自体がまだ動く時期である。** Phase 3/4 で `capability` / `budget` / `timeout` / `cancelled` / `host` などの kind 追加が控えている（第3.3節）。message の正本が固まる前に翻訳レイヤを作ると二重メンテになる。
5. **優先度が低い。** i18n は利用者体験の改善であり、マニフェストの中核価値（安定性・予算・capability・監査・予測可能性）のいずれの牽引役でもない。

#### 再検討する場合の方向性（メモ）

将来やるなら、素朴な「timezone で自動判定」ではなく次の切り分けにする。

- **canonical message（機械可読・規範）は 1 言語に固定したまま変えない。** テスト・監査・host 側分岐の基盤なので、環境で揺らさない。
- **人間向けの表示レイヤ（CLI 出力）だけを翻訳可能にする。** `ErrorKind` + placeholder は既に構造化されているので、「kind + パラメータ → ローカライズ文字列」のカタログを表示直前に噛ませる。
- **言語選択は timezone ではなく明示設定にする。** 環境変数（`LANG` / `LC_MESSAGES` / `TSUMUGI_LANG`）や CLI オプションなど、ホストが明示的に与える形にする。
- **前提として、監査ログ・canonical message の正本を何語で持つか（日本語のままか英語へ寄せるか）を先に決めておく**と、表示レイヤの議論が単純になる。

#### 着手タイミングの目安

canonical error の正本が固まり `language-spec.md` へ統合され、Phase 2 で CLI（`--help` 等を含む）が整備された後の、表示レイヤ改善としての nice-to-have。Phase 7（運用保証・検証）近辺での検討が自然で、それより前に優先しない。

## 4. `let`再宣言のbinding identity（AUD-016）

> **実装状況（2026-09-06）:** ✅ 実装完了。VM Compilerの`Stmt::Let`再宣言でのslot再利用を廃止し、全scopeで新slotを割り当てるよう統一した。観測挙動を変更したため`language-spec.md`へ規範として反映し、仕様revisionを0.14へ上げた。

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

> **実装状況（2026-09-07）:** ✅ 実装完了。tree/VM両engineで、未捕捉ランタイムエラーで
> 終了したREPL入力が変更した language-state（binding、cell の値、index 代入、push/pop、
> import marker）を入力開始時点へ巻き戻すようにした。正常完了と catch されて完了した入力は
> commit する。stdout・ファイル書き込み等の外部効果は巻き戻さない。FunctionId は rollback
> しても再利用しない（AUD-048）。observable挙動が変わるため `language-spec.md` へ反映し、
> 仕様revisionを0.15へ上げた。7.5節の journal 方式（first-write undo log、cell は Rc
> ポインタ同一性で1回だけ記録）で実装し、記録量は変更した箇所の数に比例する。deadline/
> budget/cancellation は未実装のため、それらに伴う rollback は本 phase の対象外。

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

> 実装状況: ✅ 実装完了。`src/main.rs` の `finish_repl_at_eof` を tree / VM 両 REPL loop が共有し、継続入力 buffer が非空のまま EOF を受けたら実 Lexer/Parser の parse 診断を stderr へ出して終了コード 1 で終了する。空 buffer の EOF は 0。未閉じブロック（`if`/`fn`/`while`/`for`/`try` と複数行 lambda）は `src/parser.rs` の `expect` が canonical message `入力が未完結です: end が必要です` を返す。回帰は `tests/integration.rs` の `aud033_*`（tree/VM subprocess）と `src/parser.rs` の `unclosed_block_eof_reports_incomplete_input` で固定した。

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

> 実装状況: ✅ 実装完了（仕様revision 0.16で `language-spec.md` へ統合済み）。`src/builtin_core.rs` の `builtin_path_join` が全引数を左から右へStr検査し、最初の非Strで `builtin_type` エラーを返す。tree/VMはAUD-049の共有registry/handlerを使うため差はない。`tests/canonical_error_inventory.rs`（tree/VM一致）と `tests/path_join_contract.rs`（OS依存期待値をRust `PathBuf` から構築）で固定した。

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

> **実装状況（2026-09-09）:** 🟡 部分実装。**Float→Int 変換**（`to_int` / `floor` /
> `ceil` / `round`）と **file_size** の u64→i64 変換を checked helper へ集約し、`NaN` /
> `±Infinity` / i64 範囲外を lossy な `as` cast に頼らず `conversion` / `int_overflow`
> エラーにした。`src/builtin_core.rs` の `checked_float_to_i64`（`RoundMode`）と
> `checked_file_size_to_i64` を tree/VM 共有 handler として実装し、観測挙動が変わるため
> `language-spec.md` へ反映し仕様revisionを0.17へ上げた。paired test は
> `tests/canonical_error_inventory.rs`（tree/VM 一致）・`tests/checked_conversion_contract.rs`
> （丸めモード・境界）・`builtin_core` の `aud_036_tests`（private file_size helper の
> `i64::MAX+1` / `u64::MAX`）で固定した。
>
> **未実装（本節の残り）:** `exit` の構造化 `ExecutionOutcome::Exited { code, usage }` 化。
> これは `BudgetUsage` と REV-023（`exit()` の process 終了廃止・Phase 3 budget 基盤）に
> 依存するため、本節の他項目とは切り離し、REV-023 と同一マイルストーンで実装する。
> それまで `exit` は従来どおり process を終了する（`language-spec.md` の記述は据え置き）。

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

### 11.10 実装状況

✅ 実装済み。`src/value.rs` の `Value::List` を `Rc<Vec<Value>>`、`Value::Dict` を
`Rc<BTreeMap<String, Value>>` へ変更した。全 mutation 経路（`builtin_core::assign_index`・
`builtin_push` / `builtin_pop` / `builtin_pop_update`、VM の `ListPush` / `DictInsert`
opcode）は検査後に `Rc::make_mut` で detach する。`PartialEq` に `Rc::ptr_eq` の高速パスを
足したが等価性の結果は変えていない。for は `(**list).clone()` で開始時点の要素を snapshot
する。観測挙動は不変で、既存の paired golden・error inventory・defensive test はすべて通る。
受入基準の alias 分離と snapshot は golden fixture `collection_cow_alias`（代入・引数・
返り値・nested・closure 共有 cell・equality）と既存の `for_iteration_snapshot` で固定し、
`d[to_str(i)]`（関数呼び出しを含む index 読み）と upvalue 経由 `xs[i]` の確保量が線形に
収まることを `tests/scaling.rs` の `cow_read_allocation_stays_linear_in_both_engines` で
固定した。総 heap budget の allocation 課金統合は Phase 3（[実行予算・協調実行仕様](execution-control.md)）で別途扱う。

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

## 17. 詳細レビュー由来の追加設計（REV-003 / 004 / 005 / 006 / 009 / 012 / 017 / 021）

本節は [`semantic-review/Tsumugi-detailed-review-20260907.md`](../semantic-review/Tsumugi-detailed-review-20260907.md) の指摘のうち、レビュー時点で**設計が不足していた（設計欄が △ または ×）6件**の次期実装仕様を定める。レビューが「検討◎/設計◎、実装のみ未」とした指摘（REV-001/002/006/007/013/014/015/016/017/018/019/020/022/023/024/025）は、既存の各設計文書と本書の該当節を正本とし、実装バックログへ直結させる。それらへ本節で新しい決定を追加しない。

レビュー第9.3節の方針に従い、既存 AUD 番号を再割り当てせず、本節の各項目を REV 由来の新規追跡項目として扱う。REV-012 だけは既存の第5節（AUD-017）が正本であり、本節は第5節との整合と status/doc drift の解消のみを扱う。

### 17.1 数値の厳密比較（REV-003）

#### 17.1.1 採用判断

Int–Float の比較と等価判定は、Int を `f64` へ丸めず、**数学的に厳密な順序**を返す。tree・VM・`PartialEq`・関係演算子・`contains`・`min`・`max`・sort key 生成が、共通の 1 実装 `NumericOrder` を経由する。

```text
NumericOrder::compare(&Value, &Value) -> Result<NumericOrdering, TypeError>
NumericOrdering = Less | Equal | Greater | UnorderedNaN
```

規範挙動は次のとおり。

- `Int(a)` と `Int(b)` は整数として比較する。
- `Float(a)` と `Float(b)` は IEEE 754 binary64 として比較し、NaN が絡む比較は `UnorderedNaN`。
- `Int(a)` と `Float(b)` は、Float を「符号・指数・仮数」へ分解して、丸めなしで整数側と厳密比較する。有限 Float は数学的実数値として、`±Infinity` は全ての有限 Int より大小が確定、NaN は `UnorderedNaN`。
- 等価は `compare` が `Equal` を返す場合だけ true。`UnorderedNaN` は等価でも大小でもない。
- `min` / `max` は比較後に**選択された元の operand を、その型のまま**返す（Int を Float へ変換して返さない）。同値時は第 1 引数を返す。入力に NaN を含む場合は canonical NaN（`Float(f64::NAN)`）を返す。

これにより `2^53` 近傍で `a == f` かつ `f == b` だが `a != b` となる非推移性（レビュー REV-003 の例）が解消し、等価は対称・推移的、比較は反対称になる。

#### 17.1.2 却下案

- **現行どおり `i64 as f64` で比較する**: `2^53` 超で桁落ちし、等価の推移性と比較の反対称性が崩れるため却下する。
- **Int を常に Float 化した値で `min/max` の結果も返す**: 正確な Int operand の値を失うため却下する。
- **`f64 as i64` で Float 側を丸めてから整数比較する**: 小数部・範囲外・NaN/Inf の順序が壊れるため却下する。

#### 17.1.3 データモデル / アルゴリズム

`i64` と `f64` の厳密比較は次で行う。丸め・浮動小数演算を経由しない。

```text
compare_int_float(i: i64, f: f64) -> NumericOrdering:
  f が NaN               -> UnorderedNaN
  f が +Infinity         -> Less        # i < +inf
  f が -Infinity         -> Greater     # i > -inf
  f を (sign, mantissa_int, exponent) へ分解し、
  f == trunc(f) でなければ、trunc(f) と i を整数比較して、
    等しければ小数部の符号で決着（正の小数部なら f>trunc、負なら f<trunc）
  f == trunc(f) なら trunc(f) を i128/u128 magnitude で表現し i と厳密比較
```

実装は `f64::to_bits()` から `sign / exponent / significand` を取り出し、`f.fract()` と `f.trunc()` を使って整数部と小数部を分離してよい（丸めではなく分離であり厳密）。`i64` は `i128` へ widen して比較する。中間に `f64` へ戻す経路を持たない。

#### 17.1.4 tree / VM 変更箇所

- `src/value.rs`: `PartialEq` の `Int×Float` / `Float×Int` 分岐を `NumericOrder::compare` の `Equal` 判定へ置換する。
- 新規 `NumericOrder`（`src/value.rs` もしくは専用 module）: tree・VM・builtin から参照する単一実装。
- `src/eval.rs`: 関係演算子（`<`,`<=`,`>`,`>=`）の混合数値比較を共通 helper へ。
- `src/vm.rs`: relational opcode の混合数値比較を同じ helper へ。
- `src/builtin_core.rs`: `contains` の要素一致、`min` / `max` の比較と戻り値選択、sort key 生成の数値比較を共通 helper へ。

#### 17.1.5 error

比較不能な型組合せ（数値でない型どうしの大小比較など）は既存の第3.4節「大小比較の対象型不正」の `type` error を維持する。等価比較は従来どおり型エラーにせず、異型は非等価。NaN 比較は error ではなく `false`（大小・等価とも）。

#### 17.1.6 移行

`2^53` を超える Int と Float を等価・大小比較していた source は結果が変わる（従来 true だったものが false になり得る）。厳密値へ移行する。`min` / `max` の戻り値型が「Int を渡せば Int のまま」返るようになる点も移行対象。

#### 17.1.7 テスト行列

- 値境界: `±(2^53-1)`, `±2^53`, `±(2^53+1)`, `i64::MIN`, `i64::MAX`
- 各整数近傍の `next_up` / `next_down` 相当 Float
- `-0.0`, `0.0`, NaN, `±Infinity`
- property: 等価の対称性・推移性、comparator の反対称性
- `min`/`max` の戻り値型と tie 規則、NaN 入力
- tree / VM / `PartialEq` / relational / contains / min / max / sort で結果一致

#### 17.1.8 受入基準

- 対象経路に `i64 as f64` / `f64 as i64` の混合比較 cast がない。
- 等価が対称・推移的、比較が反対称であることを property test が示す。
- tree / VM の値・error・sort 順が全境界で一致する。

#### 17.1.9 実装状況

✅ 実装済み（仕様 revision 0.19）。`src/value.rs` に `NumericOrder` / `NumericOrdering` を新設し、`Int×Float` を `trunc` / `fract` 分離 + `i128` widen で丸めなし比較する（`±Infinity` は有限 Int と大小確定、`NaN` は `UnorderedNaN`、`i128` 範囲外の巨大 Float は magnitude で決着）。`PartialEq` の `Int×Float` / `Float×Int`、tree（`src/eval.rs`）と VM（`src/vm.rs`）の関係演算子、`min` / `max`（`src/builtin_core.rs`）がこの単一実装を経由する。`contains` は `PartialEq` 経由で自動的に追随する。`min` / `max` は選択した operand を元の型のまま返し、同値時は第 1 引数、NaN 入力は canonical NaN を返す。`sort` は本 REV（revision 0.19）の時点では文字列表現比較を維持した（本節の sort 数値順化は対象外）。sort / `list_dir` の全順序化（数値順を含む）は §17.8（REV-017）で実装済み（revision 0.21）となり、`NumericOrder` を数値帯で再利用する。`value.rs` の unit test 11 件と golden fixture `numeric_strict_comparison`（tree/VM 一致）で境界値・推移性・対称性・min/max 戻り型を固定する。

### 17.2 `Chunk::patch_jump` の fallible 化と builder 封印（REV-004）

> 設計ステータス: ✅ 実装済み。`patch_jump` を `Result<(), ChunkBuildError>`（`BadOffset`/`NotAJump`）へ変更し、エラー時は `code` を書き換えず部分破損を残さない。compiler は内部エラーへ写像して `?` 伝播する。raw builder（`chunk` module）は REV-018 と共通で `unstable-bytecode` feature 下へ封印した。

#### 17.2.1 採用判断

`Chunk` の可変 builder API（`emit` / `emit_jump` / `patch_jump` / `add_constant` 等）を公開 panic 面から外す。二段構えとする。

1. **最終形**: `ChunkBuilder` を `pub(crate)` にし、公開する `Chunk` は immutable かつ検証済み（REV-005/006 の `VerifiedChunk` へ接続）とする。raw builder は `unstable-bytecode` feature でのみ公開する。
2. **最小修正（先行可）**: `patch_jump` を `panic!` しない `Result` へ変更する。

```text
patch_jump(&mut self, offset: usize) -> Result<(), ChunkBuildError>
  code.get_mut(offset) が None            -> Err(BadOffset)
  対象が Jump 系 opcode でない            -> Err(NotAJump)
  それ以外                                -> addr を現在位置へ設定して Ok
```

`ChunkBuildError` は `BadOffset` / `NotAJump` を持つ列挙型とし、builder エラーは呼び出し元 compiler が内部エラーとして扱う（script からは到達しない）。

#### 17.2.2 却下案

- **`patch_jump` の panic を維持する**: 公開 API から host を panic させられ、`tests/defensive_vm.rs` の「公開 API へ任意 Chunk を渡しても host panic しない」方針に反するため却下する。
- **`offset` を無検査 index のまま assert だけ足す**: release build で assert が消え未定義寄りの挙動になるため却下する。

#### 17.2.3 tree / VM 変更箇所

- `src/chunk.rs`: `patch_jump` を `Result` 化。builder 系メソッドの可視性を段階的に `pub(crate)` へ縮小する。
- `src/compiler.rs`: `patch_jump` の呼び出しを `?` 伝播へ変更し、builder エラーを compiler 内部エラーへ写像する。
- `src/lib.rs`: `chunk` module の公開範囲を縮小し、raw builder は feature gate 下へ移す（REV-018 と共通の封印作業）。

#### 17.2.4 error

builder エラーが VM 実行時まで漏れた場合（防御的経路）は第3.4節「VM/Compiler 不変条件違反」の `internal` / `内部エラー: {stable_detail}` を用いる。`{stable_detail}` に offset 生値を入れない。

#### 17.2.5 テスト行列

- `offset == len`, `usize::MAX`, 通常 opcode, 全 jump variant
- `catch_unwind` で panic が発生しないこと
- builder error 発生時に `code` / `lines` が部分破損しないこと

#### 17.2.6 受入基準

- 公開経路から `patch_jump` で panic しない。
- 不正 offset / 非 jump opcode が `Result::Err` になる。
- builder 縮小後、通常 compile 経路の観測挙動が変わらない。

### 17.3 `MakeClosure` capture 記述子の明示化（REV-005）

> 設計ステータス: ✅ 実装済み。`MakeClosure` の operand を「upvalue 数」から `Chunk.prototypes` の「プロトタイプ index」へ変更し、`FunctionPrototype`（`chunk` / `captures`）と明示 `CaptureDesc`（`Local`/`Upvalue`）を導入。compiler は隣接 `GetLocal`/`GetUpvalue` 列の暗黙契約を廃止し、VM は記述子から cell を解決する（`usize::MAX`/Null cell フォールバックを削除。不正記述子は `internal`、範囲は verifier の V5 で拒否）。REV-006 と同一 build で実施した。

#### 17.3.1 採用判断

closure の capture を、`MakeClosure` 直前の隣接 opcode 列から逆算する現行方式をやめ、**関数プロトタイプが capture 記述子を明示的に持つ**方式へ変更する。

```text
FunctionPrototype {
    chunk: Rc<VerifiedChunk>,
    captures: Box<[CaptureDesc]>,
}
CaptureDesc = Local(u32) | Upvalue(u32)
```

`MakeClosure` はプロトタイプ index だけを operand に持ち、capture の解釈は記述子から直接行う。現行の `(true, usize::MAX)` フォールバック（不正 operand を Null cell として黙認する経路）を廃止し、記述子が local/upvalue 範囲外・不整合なら即 `internal` error とする。範囲検証は REV-006 の verifier が担い、VM 実行時は defense-in-depth として再検査する。

#### 17.3.2 却下案

- **隣接 opcode からの逆算を維持し、不一致だけ error にする**: 命令列の並びに依存する暗黙契約が残り、verifier と二重管理になるため却下する。
- **不正記述子を現行どおり Null cell で握りつぶす**: 壊れた capture を持つ closure が「成功」してしまい、観測挙動が未定義化するため却下する。

#### 17.3.3 データモデル / 状態遷移

compiler は各関数定義について capture 集合を確定し、`FunctionPrototype.captures` へ格納する。VM は `MakeClosure(proto_index)` で:

```text
proto = current_chunk.prototypes[proto_index]   # 範囲は verifier 済み
for cap in proto.captures:
  Local(slot)    -> 親フレームの local cell を共有（slot は verifier 済み）
  Upvalue(index) -> 親フレームの upvalue cell を共有（index は verifier 済み）
Null フォールバックは持たない
```

#### 17.3.4 tree / VM 変更箇所

- `src/opcode.rs`: `MakeClosure` の operand を「upvalue 数」から「プロトタイプ index」へ変更する。
- `src/value.rs`（VmFn 表現）: 関数値がプロトタイプ / capture 記述子を参照する形へ変更する。
- `src/compiler.rs`: capture 記述子を明示生成し、`GetLocal`/`GetUpvalue` 列の暗黙契約を廃止する。
- `src/vm.rs`: `MakeClosure` 処理を記述子ベースへ置換し、`usize::MAX` フォールバックと Null cell 生成を削除する。tree 版 closure 生成と観測挙動を一致させる。

これは opcode と関数値表現の変更を伴うため、REV-006（verifier / VerifiedChunk）と同一マイルストーンで実施する。

#### 17.3.5 error

記述子不正が実行時まで到達した場合は第3.4節「VM/Compiler 不変条件違反」の `internal` を用いる。capture 範囲外は verifier が compile/link 段で拒否する。

#### 17.3.6 テスト行列

- 正常系: 0 capture / 単一 local / 単一 upvalue / 多段 upvalue / 複数 capture
- 異常系（raw bytecode）: 記述子の local/upvalue 範囲外、記述子順序不正、対応する cell が無い
- 不正列が「成功」も「Null 補完」もされないこと
- tree / VM で closure の観測挙動一致

#### 17.3.7 受入基準

- capture が隣接 opcode 列でなく明示記述子で決まる。
- 不正記述子が Null cell へ黙殺されず、verifier または内部 error で止まる。
- 正常な多段 capture が tree / VM 一致する。

### 17.4 `list_dir` の部分失敗と非 UTF-8 名の扱い（REV-009）

#### 17.4.1 採用判断

`list_dir` は、ディレクトリ列挙開始後の**個別 entry 取得失敗を黙殺しない**。safe profile では entry error が 1 件でもあれば、部分結果を成功 List として返さず、構造化した host error にする。非 UTF-8 の entry 名を lossy 変換して同一視しない。

- 個別 entry 取得失敗: `host` error（category は `directory_read`）。
- 非 UTF-8 の entry 名: safe profile では entry 名を String として返せないため、`host` error（category は `invalid_encoding`）とする。将来 opaque entry ID / Bytes 型を導入する場合はそちらで表現する。
- legacy profile では現行互換（失敗時 Null、lossy 名）を adapter 層で提供してよいが、capability denial を Null へ畳まない。
- 列挙は snapshot とし、ordering・件数・bytes budget を adapter 契約に含める（budget は REV-015 / execution-control 側の実装に接続）。

これは次期 Capability Model の `DirectoryEntry.name`（検証済み String, `capability-model.md`）とも整合させる。

#### 17.4.2 却下案

- **`entries.flatten()` のまま部分結果を返す**: 「存在しない」という誤認を成功として返すため却下する。
- **非 UTF-8 名を `to_string_lossy` で返し続ける**: 異なる byte 名が同一表示へ写像され、path identity・audit・replay が壊れるため却下する。
- **部分結果を `{ entries, incomplete }` として通常 List に見せる**: 通常 List への偽装であり、業務処理が incomplete を見落とすため却下する（構造化する場合は明示型で返す）。

#### 17.4.3 データモデル / 状態遷移

```text
list_dir(path):
  authorize(path)                 # capability 拒否は sandbox/capability error（Null にしない）
  read_dir 失敗                    -> safe: host(directory_read) / legacy: Null
  各 entry:
    取得失敗                       -> safe: host(directory_read) / legacy: skip
    file_name が非 UTF-8           -> safe: host(invalid_encoding) / legacy: lossy 文字列
    件数/bytes budget 超過         -> collection_limit / budget（既存契約）
  snapshot を確定し ordering 済み List を返す
```

#### 17.4.4 tree / VM 変更箇所

- `src/builtin_core.rs`: `builtin_list_dir` の `entries.flatten()` を、各 entry の `Result` を検査する形へ変更する。`to_string_lossy` を廃し、非 UTF-8 は profile に応じて error か legacy 変換にする。
- profile 判定は capability / ExecutionContext 側の safe/legacy に接続する（REV-014 の request 化と同一基盤）。
- tree / VM は同じ registry / handler（AUD-049）を共有し固有処理を持たない。

#### 17.4.5 error

- entry 取得失敗: `host` / `host function の実行に失敗しました: list_dir (directory_read)`
- 非 UTF-8 名（safe）: `host` / `host function の実行に失敗しました: list_dir (invalid_encoding)`
- line は `list_dir(...)` call 式。OS の生 message を埋め込まない。

第3.4節の host error inventory へ上記 category を追記する。

#### 17.4.6 移行

「一部 entry が欠けた List が成功で返る」挙動、および非 UTF-8 名が置換文字列で返る挙動に依存した source は変わる。legacy profile で当面の互換を取る。

#### 17.4.7 テスト行列

- 個別 entry error を 2 件目だけ注入（adapter fake）
- Unix の異なる invalid-byte 名が同じ lossy 文字列になる case
- partial success を成功 List として返さないこと
- ordering と budget N / N+1
- safe / legacy profile 差
- tree / VM 一致

#### 17.4.8 受入基準

- entry error が 1 件でもあれば safe profile で成功 List を返さない。
- 非 UTF-8 名を lossy 変換して同一視しない。
- capability denial を Null へ畳まない。
- tree / VM の結果・error・ordering が一致する。

### 17.5 user call 評価順の整合と status drift 解消（REV-012）

#### 17.5.1 位置づけ

user call の最終評価順の正本は第5節（AUD-017）§5.1 および第3.7節 error precedence 表であり、本節はそれらを**新しく変更しない**。REV-012 が指摘するのは実装ではなく、次の drift である。

1. 現行 `language-spec.md`・現行実装は「step/depth 検査 → callee 評価」（callee 評価前に検査）である。
2. `src/limits.rs` の doc comment と本書 §5.1/§5.6・第3.7節は「callee を先に評価・分類し、user callable だけ step/depth 検査」（callee 先行）を次期契約とする。
3. `roadmap.md` は AUD-017 / AUD-050 を「完了」と表示するが、これは深度計数値（127→128）の統一を指し、error precedence の callee 先行化は未実装である。

つまり「境界値の統一は実装済み」「callee 先行への precedence 変更は未実装」の 2 つが 1 つの完了表示に畳まれていることが問題である。

#### 17.5.2 決定

- 最終意味論は第5節どおり **callee 先行**を維持する（builtin/host function を user 深度で拒否せず、動的 callee の種類を確定してから適切な budget を選べるため）。これは callee の副作用が limit error より前に発生し得る破壊的変更であり、`LanguageRevision` を上げて実施する。
- `src/limits.rs` の doc comment は「current（callee 評価前検査）」と「target（callee 先行）」を混在させず、どちらの revision の契約かを明記する。実装が現行のうちは current を、切替 commit で target へ更新する。
- roadmap の AUD-017 / AUD-050 status を、単一の「完了」から次の 2 sub-status へ分割する。
  - `depth-counting`（128 user frame 統一）: verified
  - `callee-precedence`（callee 先行への precedence 変更）: planned
- tree / compiler / VM / callback の評価順切替は同一 commit で行い、backend 差を残さない。

status 表現は §19（文書全体の最終受入基準）および REV-011（メタデータ単一正本、`roadmap.md`）の機械可読 status で管理し、手作業同期の再 drift を避ける。

#### 17.5.3 tree / VM 変更箇所

- `src/limits.rs`: doc comment を current/target 明記へ修正する。
- `src/eval.rs` / `src/compiler.rs` / `src/vm.rs`: callee 先行化を実施する commit で、callee 評価 → callable 分類 → user callable だけ step/depth → arity → args 左→右 → frame/body の順へ揃える。
- `docs/roadmap.md`: AUD-017 / AUD-050 の status を `depth-counting=verified` / `callee-precedence=planned` へ分割する。

#### 17.5.4 error

precedence 変更後も error kind/message/line は第3.4・3.5・3.7節どおり。深度上限は `overflow` / `スタックオーバーフロー: 再帰が深すぎます (上限: 128)`、line は拒否された call 式。

#### 17.5.5 移行

callee の副作用が limit error より前に発生する（従来は検査で先に止まっていた）。callee 式の副作用順に依存する source はこの規則へ移行する。builtin / host function 呼び出しは user depth 128 でも拒否されない点も §5.6 のとおり。

#### 17.5.6 テスト行列

- callee に counter 更新を持つ式（budget 0 / depth 128 で callee 副作用の有無）
- non-callable / wrong arity で argument 非評価
- builtin / host function / callback の分類
- tree / VM の stdout・state・error kind/message/line 完全一致

#### 17.5.7 受入基準

- tree / VM が callee 先行の同一順序で評価する。
- `limits.rs` doc comment が current/target を混在させない。
- roadmap の AUD-017 / AUD-050 status が 2 sub-status へ分かれ、`callee-precedence` が verified になるまで「完了」と表示しない。

### 17.6 `remove_dir` の再帰削除と capability の整合（REV-021）

#### 17.6.1 採用判断

現行 `remove_dir` は `std::fs::remove_dir_all`（中身ごと再帰削除）だが、次期 Capability Model は `remove_dir` を `RemoveKind::EmptyDirectory` へ割り当てている。この不整合を、`EmptyDirectory` 権限で再帰削除を許してしまう（過大権限）／非空で突然失敗する（契約違反）のどちらにもしないため、**操作を分割する**。

- 次期 revision で `remove_dir(path)` を**空ディレクトリのみ**削除へ変更する（`std::fs::remove_dir` 相当）。非空なら error。必要 capability は `EmptyDirectory`。
- 再帰削除は新規 builtin `remove_tree(path)` として分離し、専用 capability `RecursiveDelete` を要求する。`remove_dir` へ `EmptyDirectory` から暗黙昇格させない。
- `remove_tree` は entry 件数 / depth / bytes / fuel / deadline / cancel / audit を持つ（budget・cancel は REV-015 / execution-control 側に接続）。final symlink と中間 symlink の扱いを明示する（symlink 自体を削除し、リンク先を辿って再帰削除しない）。

互換を優先する代替として「`RemoveKind::RecursiveTree` を明示追加して `remove_dir` へ割り当てる」案もあるが、`remove_dir` の名前と再帰削除の危険度が一致しないため、分割案を第一候補とする。

#### 17.6.2 却下案

- **現行 `remove_dir_all` のまま `EmptyDirectory` capability へ接続する**: 空ディレクトリ権限で再帰削除を許す過大権限になるため却下する。
- **`remove_dir` を空のみへ変えるが再帰削除手段を用意しない**: 既存の再帰削除ユースケースを失うため、`remove_tree` を同時に用意する。
- **`EmptyDirectory` へ暗黙で再帰権限を含める**: capability の最小付与原則に反するため却下する。

#### 17.6.3 データモデル / 状態遷移

```text
remove_dir(path):
  authorize(path, EmptyDirectory)
  std::fs::remove_dir(safe_path)      # 非空なら OS error -> 契約に従い false もしくは host error
  symlink は remove_dir で扱わず remove（FileOrSymlink）側の責務

remove_tree(path):
  authorize(path, RecursiveDelete)
  walk しながら entry 件数/depth/bytes/fuel/deadline/cancel を消費
  中間・final symlink はリンクを辿らずリンク自体を削除
  audit に削除対象数と partial effect を記録
```

戻り値契約（Null/false vs 構造化 error）は REV-022 の profile 方針に従い、safe profile では denial と OS 失敗を区別する。

#### 17.6.4 tree / VM 変更箇所

- `src/builtin_core.rs`: `builtin_remove_dir` を `remove_dir_all` から `remove_dir`（空のみ）へ変更する。新規 `builtin_remove_tree` を追加する。
- capability 層（次期 `FilesystemCapability`）: `remove_dir`→`EmptyDirectory`、`remove_tree`→`RecursiveDelete` を割り当てる。
- `docs/capability-model.md` / `docs/language-spec.md`: `remove_dir` の意味論変更と `remove_tree` 追加を反映する（実装完了・受入通過後）。
- tree / VM は同じ registry / handler を共有する。

#### 17.6.5 error

- 非空ディレクトリへの `remove_dir`: safe profile では `host`（category `directory_not_empty`）。legacy では現行どおり false。
- capability 拒否: 第3.4節「filesystem capability 拒否」の `sandbox`。
- `remove_tree` の budget / cancel: `budget` / `cancelled`（catch 不能）。

第3.4節の host error inventory へ `directory_not_empty` category を追記する。

#### 17.6.6 移行

`remove_dir` が非空ディレクトリを再帰削除しなくなる。再帰削除が必要な source は `remove_tree` へ移行し、host は `RecursiveDelete` capability を明示付与する。これは破壊的変更のため `LanguageRevision` を上げる。

#### 17.6.7 テスト行列

- 空 / 非空ディレクトリ
- nested tree（`remove_tree`）
- final symlink と中間 symlink
- partial failure / permission change / cancel（`remove_tree`）
- capability 隣接操作の deny（`EmptyDirectory` で `remove_tree` 不可、その逆も）
- audit で削除対象数と partial effect を記録
- tree / VM 一致

#### 17.6.8 受入基準

- `remove_dir` が空ディレクトリのみを削除し、`EmptyDirectory` capability で再帰削除できない。
- `remove_tree` が `RecursiveDelete` capability を要求し、budget / cancel / audit を持つ。
- symlink をリンク先まで辿って削除しない。
- tree / VM の結果・error が一致する。

### 17.7 bytecode 検証と `VerifiedChunk`（REV-006）

> 設計ステータス: ✅ 実装済み。§17.2（REV-004）・§17.3（REV-005）・REV-018 と同一マイルストーンで実装した。`src/verifier.rs` に `VerifiedChunk`（生成は `verify` か信頼済み `from_trusted` のみ）と `verify`（V1〜V9 をプロトタイプ木へ再帰適用し、最初の違反を `ChunkVerifyError` として返す。`From<ChunkVerifyError> for TsumugiError` と `verify_or_internal_error` で `internal` へ写像）を新設。VM の `run_frames` は全命令 dispatch 直前で無条件に `count_step` する層1課金へ変更し、`Loop`/`PrepareCall`/`call_fn_value` の個別課金を廃止（深度上限は `PrepareCall` と `Call` の両方で検査）。`Vm::new` / `run_repl_chunk` は `VerifiedChunk` だけを受け取り、compiler 出力は `from_trusted` で昇格する。停止性は per-instruction 課金で verifier 非依存。本節はそれらが「範囲検証は verifier が担う」と委譲している検証層の正本である。
>
> **2026-10-20 改訂（2 メーター化）**: 層1 を「全命令で fuel（step）を課金する」から「fuel とは独立した VM instruction watchdog」へ改める。per-instruction の fuel 課金は、[実行予算・協調実行仕様](execution-control.md) §2・§4.1 の「fuel は backend 非依存の論理量で、VM opcode 数そのものではない」と矛盾し、同じ script が tree では成功して VM では step 上限に達する（ループで約 15 倍、再帰で約 11 倍の差）という観測差を生んでいた。改訂後は、**fuel は論理量として tree と同じ課金点で数え、停止性は watchdog が形状非依存に保証する**。層2（verifier・`VerifiedChunk`・API 封印）は変更しない。改訂部分（§17.7.1 層1・§17.7.3・§17.7.5〜§17.7.8）は未実装で、[実行予算・協調実行仕様](execution-control.md) §14「F-1 実装スライス」で実装する。

#### 17.7.1 採用判断

VM が実行する bytecode を、**検証を通過した `VerifiedChunk` に限定する**。あわせて、任意の bytecode に対して停止性（予算での必ずの終了）が成り立つよう、**fuel とは独立した instruction watchdog** を置く（2026-10-20 改訂。改訂前は「step 課金を 1 命令 dispatch ごとに 1 へ変更する」だった）。この 2 層で REV-006 の 2 つの穴（`Jump(0)` 等による step 課金迂回、`PrepareCall` を省いた raw `Call` による call 課金迂回）を塞ぐ。

- **層1（durable guarantee・bytecode 形状に非依存）＝ instruction watchdog**: VM は execution ごとに、fuel・`BudgetUsage`・audit・slice yield のいずれにも現れない内部カウンタ（watchdog）を持つ。watchdog は命令の種別ではなく、次の 2 つの**一般的な条件**で 1 tick 進む。
  - **(W1) 後退・停滞**: 命令を 1 つ dispatch した後、同じ frame のまま ip が dispatch 前の ip 以下になった。
  - **(W2) frame の追加**: call frame が 1 つ積まれた（命令の種類や callback 経由かを問わず、frame を積む唯一の経路で数える）。

  tick が上限に達したら実行を止める（§17.7.5）。停止性の論証: tick が起きない区間では、各 frame の ip は単調増加し、frame は減る一方なので、区間内に dispatch できる命令数は「frame 深さ上限 × chunk 木の最大 code 長」で有界になる。したがって watchdog の上限が有限なら、検証を通っていない chunk（`Jump(0)` の自己ループ、`PrepareCall` を省いた raw `Call` の再帰など）でも、命令列の形状によらず必ず有限で止まる。
- **fuel（論理量）**: VM の fuel は、tree と同じ論理的な課金点で数える（§17.7.3）。per-instruction の fuel 課金は廃止する。
- **層2（gate・早期拒否と不変条件の確立）**: `Chunk` を `VerifiedChunk` へ昇格させる際に static verifier を通す。verifier はホストが構築・改変した bytecode の構造的健全性を compile/link 時に一度だけ検査し、VM 実行時の防御的分岐（AUD-023 の `require_*` ヘルパー）が「起こり得るが正常運用では到達しない」経路として残る前提を確立する。

VM の公開入口は `VerifiedChunk` だけを受け取る。raw `Chunk` から VM を直接動かす現行の公開経路（`tsumugi::vm::Vm::new(chunk)` / `run_repl_chunk(chunk)` と `pub mod chunk/opcode/vm`）は封印する（REV-018 と共通作業）。

停止性の正本は層1（instruction watchdog）とする。verifier は「早期拒否・不変条件の明文化・defense-in-depth」であって、停止性を verifier だけに依存させない。verifier を通していない chunk が防御的に実行されても、ホストは panic せず有限 step で停止しなければならない。

#### 17.7.2 却下案

- **verifier だけで停止性を保証し、課金は現状（Loop/PrepareCall のみ）を維持する**: verifier に単一の見落としがあると無期限実行へ直結する。停止性を静的解析の完全性へ依存させるのは脆いため却下する。層1の無条件課金を正本にする。
- **後方ジャンプの到達可能性を解析して「課金経路を必ず通る」ことを保証する**: 一般の bytecode で「必ず charged 命令を通る」ことの静的証明は制御フロー解析が重く、生成器の変更に脆い。per-instruction 課金なら形状に依存せず自明に停止するため却下する。
- **`Jump` にだけ課金を追加する**: `Jump` 以外の後方遷移（不正な `Loop` operand 等）や、分岐で自己参照する構成を取りこぼす。命令種別ごとの課金は網羅が難しいため却下する。
- **raw `Chunk` 経路を公開したまま実行時チェックだけ足す**: 未検証 bytecode が VM 内部不変条件へ到達し続け、verifier で確立したい「VM は検証済み前提」を崩す。公開入口を `VerifiedChunk` へ限定する。
- **1 命令 = 1 step で既存 fixture の step 予算が変わることを避けるため、命令に重み付けする**: 重み表が opcode 追加ごとに保守対象になり、観測挙動（`limit` 到達点）の説明が複雑化する。等重み（1 命令 1 step）とし、必要なら `TSUMUGI_MAX_STEPS` の既定値だけを調整する。（2026-10-20 改訂で per-instruction の fuel 課金自体を廃止したため、この却下理由は watchdog にだけ当てはまる。watchdog は命令の種別で重みを変えない）
- **（2026-10-20 追加）1 つのカウンタで fuel と停止性を兼ねる（改訂前の層1）**: fuel を「VM opcode 数」にすると、[実行予算・協調実行仕様](execution-control.md) §2・§4.1 の「fuel は backend 非依存の論理量」に反し、同じ script の成功・失敗が backend で変わる。逆に fuel を論理量にして停止性も fuel に任せると、停止性が compiler の課金点配置（＝生成器の正しさ）に依存し、本節の「停止性を静的な正しさに依存させない」に反する。目的の違う 2 つを 1 つのカウンタで満たせないため、fuel と watchdog を分ける。
- **（2026-10-20 追加）watchdog を「後方ジャンプ命令」だけで数える**: 上の「`Jump` にだけ課金を追加する」と同じく、命令種別の列挙は取りこぼしやすい。(W1)(W2) は ip と frame 数の変化だけを見るため、命令の追加・変更に影響されない。

#### 17.7.3 データモデル / 状態遷移

```text
VerifiedChunk(inner: Chunk)          # 生成は verify() 経由のみ。inner は非公開。
  - VM が参照する code / constants / lines / prototypes への読み取り専用アクセサを持つ
  - Clone は検証状態を保つ（再検証不要）
  - PartialEq は inner の等価で定義（test 用）

verify(chunk: Chunk) -> Result<VerifiedChunk, ChunkVerifyError>
  以下を、関数プロトタイプを含め chunk 木の全 code に対して検査する:

  (V1) 行番号表整合:      lines.len() == code.len()
  (V2) 定数参照:          LoadConst(i) の i < constants.len()
  (V3) local slot:        Get/SetLocal(s) / LenLocal(s) / IndexLocal(s) /
                          SetIndex(Local(s)) の s が、その chunk の宣言 local 数の上限内
  (V4) upvalue index:     Get/SetUpvalue(i) / SetIndex(Upvalue(i)) の i が
                          プロトタイプの capture 数（§17.3 の captures.len()）内
  (V5) capture 記述子:    §17.3 の CaptureDesc の Local(slot)/Upvalue(index) が
                          親プロトタイプの範囲内（REV-005 の範囲検証をここで実施）
  (V6) jump target 範囲:  Jump / JumpIfFalse / JumpIfFalseKeep / JumpIfTrueKeep /
                          Loop / SetupTry / JumpIfGlobalDefined の target が
                          0 ..= code.len()（= 末尾＝暗黙 return 位置を許可）
  (V7) builtin id:        CallBuiltin(id, _) の id が registry に存在
                          （AUD-049 の BuiltinId は型で保証されるが、防御的に再確認）
  (V8) stack operand:     PopN(n) / Print(n) / FStrConcat(n) / CallBuiltin(_, n) /
                          Call(n) / ValidateCall(n) の n が u32→usize 変換可能
                          （32-bit 迂回は REV-010 と整合。負の到達は型で排除）
  (V9) try 構造:          SetupTry の target が catch 先頭として範囲内、
                          TeardownTry が対応する SetupTry を持つ入れ子であること
  検査は一度だけ。成功なら VerifiedChunk、失敗なら最初の違反を ChunkVerifyError で返す。

ChunkVerifyError {
    kind: BadLineTable | BadConstant | BadLocalSlot | BadUpvalue
        | BadCapture | BadJumpTarget | UnknownBuiltin | BadOperand | BadTryStructure,
    // 生値・オフセットは stable_detail へ含めず、種別と位置種別のみ保持する
}
```

停止性（層1）と fuel の状態遷移（2026-10-20 改訂）:

```text
run_frames dispatch loop:
  (frame_before, ip_before) = (現在の frame, ip)
  fetch instruction at ip
  ip += 1
  dispatch(instruction)    # fuel は下記の論理課金点でだけ課金する
  if 同じ frame のまま ip <= ip_before:
      watchdog_tick(line)? # (W1) 後退・停滞
push_frame(...):           # frame を積む唯一の経路（Call / callback / REPL root）
  watchdog_tick(line)?     # (W2) frame の追加

fuel の論理課金点（tree の現行課金点と同じ。各 1）:
  (F1) ループの back-edge  : `Loop` 命令（`Loop` は back-edge 専用にする。`for` の
                             `continue` は前方 `Jump` で increment へ飛ばす）
  (F2) user function 呼出し: `PrepareCall`（呼び出し位置の line。深度検査の前）
  (F3) builtin の callback : `call_fn_value`（builtin 呼び出しの line）
```

`count_step` / `max_steps` / `TSUMUGI_MAX_STEPS` / `step_limit` error の定義は変更しない。fuel を課金するのは (F1)〜(F3) だけで、tree の課金点（ループ反復・user function 呼び出し・callback 呼び出し）と 1 対 1 に対応する（VM は host function に対応しないため、tree の host call 課金点に当たるものはない）。深度上限（`MAX_USER_CALL_DEPTH`）は現行どおり `PrepareCall` と `Call` の両方で検査する。`PrepareCall` を省いた raw `Call` は fuel を課金しないが、(W2) で watchdog が進むため停止性は保たれる。

watchdog の上限は `total_fuel × 2 + 16` とする。正規 compiler 出力では、(W1) の後退は (F1) の back-edge、(W2) の frame 追加は (F2)・(F3) か root frame に必ず伴うため、tick 数は「fuel 課金数 + root frame 数」を超えない。よって正規 compiler 出力では常に fuel の上限が先に効き、watchdog の上限は検証を通っていない chunk に対してだけ働く（この不変条件はテストで固定する）。

`VerifiedChunk` は VM・関数プロトタイプ・REPL 差し替えの各所で `Chunk` を置き換える。VM 内部の `require_*` ヘルパー（AUD-023）は削除せず defense-in-depth として残す。verifier を通した chunk では発火しないことをテストで確認する。

#### 17.7.4 tree / VM 変更箇所

- 新規 `src/verifier.rs`（または `src/chunk.rs` 内 `verify`）: `verify(Chunk) -> Result<VerifiedChunk, ChunkVerifyError>` と `VerifiedChunk` を定義する。プロトタイプ木を再帰的に検査する。
- `src/chunk.rs`: `VerifiedChunk` の読み取り専用アクセサを定義し、builder（`emit` / `patch_jump` 等）は §17.2 に従い `pub(crate)` へ縮小する。
- `src/opcode.rs`: §17.3 の `MakeClosure(proto_index)` と `FunctionPrototype` を導入する（同一マイルストーン）。
- `src/vm.rs`:
  - メインループの `count_step` を全命令 dispatch 前の 1 回へ移す。`Loop` / `PrepareCall` / `call_fn_value` の個別 `count_step` を削除する。（2026-10-20 改訂で反転: 全命令の `count_step` を廃止し、(F1)〜(F3) の個別 `count_step` と (W1)(W2) の watchdog を置く。詳細は [実行予算・協調実行仕様](execution-control.md) §14「F-1 実装スライス」）
  - `Vm::new` / `run_repl_chunk` の受け取りを `VerifiedChunk` へ変更する。VM 内部が参照する code / constants / lines / prototypes を `VerifiedChunk` のアクセサ経由にする。
  - `require_*` ヘルパーは残す（防御的経路）。
- `src/compiler.rs`: `compile` / `compile_repl_line` の戻り値を `VerifiedChunk` にする（生成直後に `verify` を通す）か、compiler が生成する chunk は不変条件を満たすため verify を skip して `VerifiedChunk` を直接構築する `pub(crate)` 経路を用意する。どちらでも「VM へ渡る型は `VerifiedChunk`」を守る。
- `src/lib.rs`: `chunk` / `opcode` / `vm` module の raw 公開を封印する（REV-018 と共通）。公開 facade は `Engine` 系のみ。raw builder / raw VM 経路は `unstable-bytecode` feature 下でだけ露出する。
- `src/module.rs` / `src/engine.rs`: link 済みプログラムを compile→verify→VM の順に流す経路を `VerifiedChunk` で通す。

#### 17.7.5 error

- verifier 拒否（compile/link 段）: 第3.4節「VM/Compiler 不変条件違反」の `internal` / `内部エラー: {stable_detail}` を用いる。`{stable_detail}` は `ChunkVerifyError.kind` に対応する固定文字列（例: `bytecode 検証に失敗しました: jump target が範囲外です`）とし、オフセットや operand の生値・スタック内容を含めない。script からは到達しないため trace は付けない。
- 防御的に未検証 chunk が VM 実行時の `require_*` へ到達した場合: 従来どおり `internal`（AUD-023 の各メッセージ）を維持し、host panic させない。
- step 上限到達: 既存の `limit` / `ステップ上限に達しました (上限: {limit})` を変更しない。（2026-10-20 改訂）fuel の到達点・line・trace は tree と一致する（tree と同じ課金点・同じ line で課金するため）。
- watchdog 上限到達（2026-10-20 追加）: fuel の上限到達と**同じ** terminal（kind `limit`、`ステップ上限に達しました (上限: {total_fuel})`、catch 不可、line は上限に達した命令の line）に写す。正規 compiler 出力では到達しないため、新しい error 種別を script・host へ見せない。

#### 17.7.6 移行

- `TSUMUGI_MAX_STEPS` の既定値と、step 数に依存する fixture（`error_step_limit` 等）は、per-instruction 課金で到達点が変わるため再調整する。観測される kind・message は変えない。（2026-10-20 改訂: 既定値は調整されなかった。改訂後は VM の fuel が tree と同じ点で数えられるため既定値の再調整は不要になり、VM で step 上限に達していた script の一部は成功するようになる＝制限の緩和）
- ホストが `tsumugi::chunk::Chunk` を raw 構築して `tsumugi::vm::Vm` へ渡していた利用は、封印により `unstable-bytecode` feature が必要になる。安定利用は `Engine` facade へ移行する。
- `tests/defensive_vm.rs` は「未検証 chunk を防御的経路（`unstable-bytecode` またはテスト内部 API）で VM へ渡しても host panic しない」方針を維持する。加えて REV-006 の 2 ケース（`Jump(0)` 自己ループ・`PrepareCall` なし raw `Call` ループ）を追加し、いずれも `limit` error で有限停止することを固定する。

#### 17.7.7 テスト行列

| 軸 | ケース |
|---|---|
| 停止性（層1） | `Jump(0)` 自己ループ / 後方 `Loop` 迂回 / `PrepareCall` なし raw `Call` ループ / 巨大 operand なし純命令ループ。いずれも `limit` で有限停止（2026-10-20 改訂後は watchdog によって） |
| fuel parity（2026-10-20 追加） | ループ反復・再帰・callback・`for`+`continue`・`while`+`continue`・try 内ループで、tree と VM の `committed.fuel` が完全一致し、上限 N に対して成功・失敗の境界、line、trace が一致する |
| watchdog の非干渉（2026-10-20 追加） | 正規 compiler 出力では、どの上限値でも fuel の上限が watchdog より先に効く |
| verifier 拒否（層2） | V1〜V9 各違反を最低 1 ケース。最初の違反種別が `ChunkVerifyError` になる |
| verifier 通過 | 正規 compiler 出力（if/while/for/関数/closure/try/f-string/builtin）が verify を通り、観測挙動が現行と一致 |
| defense-in-depth | verify を通した chunk では `require_*` が発火しないこと（fault injection でのみ発火） |
| backend | per-instruction 課金後の step 到達点を tree（該当する範囲）と VM で説明可能にし、VM の `limit` error が message/line 一致（2026-10-20 改訂で「fuel parity」行へ強化） |
| 公開境界 | `unstable-bytecode` feature 無効時に raw `Chunk`→VM 経路がコンパイル不能／到達不能であること |

全ケースで終了コード・stdout・stderr・kind・message・line を検査する。既存 golden の step 到達点変化は fixture 側で吸収し、backend 別期待値を新設しない。

#### 17.7.8 受入基準

- VM の公開入口が `VerifiedChunk` だけを受け取り、安定 API から raw `Chunk` を VM へ渡せない。
- 全命令 dispatch ごとに 1 step 課金され、`Jump(0)` 自己ループ・`PrepareCall` なし raw `Call` ループが有限 step で `limit` error になる。（2026-10-20 改訂: 「全命令 dispatch ごとに 1 step 課金」は「(W1)(W2) で watchdog が進み、上限で `limit` error」へ置き換える。fuel は (F1)〜(F3) でだけ課金し、tree と `committed.fuel` が一致する）
- verifier が V1〜V9 の構造違反を compile/link 段で `internal` として拒否する。
- verifier を通していない chunk が防御的に実行されても host が panic せず有限停止する（停止性は verifier に依存しない）。
- 正規 compiler 出力の観測挙動（step 到達点を除く）が変わらず、tree / VM で kind・message・line が一致する。
- §17.2（REV-004）builder 封印・§17.3（REV-005）capture 記述子と同一 build へ統合し、旧 API・旧フォールバックを併存させない。

### 17.8 HumanDisplay / CanonicalRepr / TotalOrder の分離と全順序 sort（REV-017）

> 設計ステータス: ✅ 実装済み（仕様 revision 0.21）。本節は sort / `list_dir` の並び順を「文字列表現比較」から**全値型を覆う単一の全順序 `TotalOrder`** へ移す仕様（revision 0.21）を定める。REV-003（§17.1）が確立した `NumericOrder` を数値帯で再利用し、観測挙動が変わる破壊的変更として扱う。REV-001（HumanDisplay / CanonicalRepr / TotalOrder の 3 層分離）と連動する。

#### 17.8.1 採用判断

sort（builtin）と `list_dir`（名前の整列）の順序を、各要素を人間向け表示文字列へ描画してその Unicode コードポイント順に並べる現行方式から、**全ての `Value` を覆う決定的な全順序 `TotalOrder`** による比較へ置き換える。これにより `sort([3, 10, 2])` は現行の `[10, 2, 3]`（`"10" < "2" < "3"`）ではなく **`[2, 3, 10]`**（数値順）になる。

値の「表示」「正規化表現」「順序」という 3 つの関心を、次の 3 層として明示的に分離する（REV-001 連動）。

- **HumanDisplay**: 人間向けの描画（`print` / `Display` が見せるもの）。**挙動は現行のまま変えない**。`src/determinism.rs` の `format_float`（canonical NaN 畳み込み・小数描画）を含む既存経路がこれにあたる。
- **CanonicalRepr**: 正規化表現。等価判定・ハッシュ・監査正規化で用いる「同値を同一表現へ畳む」層。既存の正規化（canonical quiet NaN への畳み込み §4.5、`PartialEq` の数値跨ぎ等価 §17.1、Dict key の BTreeMap コードポイント順）がこれにあたり、本 REV で新しい正規化規則は追加しない。
- **TotalOrder**: 全ての値に対する決定的な全順序。sort と `list_dir` の整列が参照する**本 REV の新規規範成果物**。tree・VM の両 backend で**同一**でなければならない（manifesto 原則5「同じ結果・同じ副作用順序」）。

数値要素は HumanDisplay（文字列描画）ではなく `NumericOrder`（REV-003）で順序づける。異種混在リストでも決定的に並ぶよう、型帯（type band）をまたぐ順序も小さく明示的な規則で定義し、「何でも並べられる」という現行の利便性は全順序として保ったまま維持する（manifesto 原則8「小さく理解可能な中核」）。

#### 17.8.2 却下案

- **現行どおり文字列表現比較を維持する**: `sort([3,10,2])` が `[10,2,3]` になる数値の桁違い問題が残り、REV-003 で `<` / `==` / `min` / `max` を厳密数値順にした一方で sort だけ文字列順という backend 内の不整合が残るため却下する。
- **数値帯だけ数値順にし、異種混在は従来どおり文字列順へフォールバックする**: 比較規則が「同型なら数値順・異型なら文字列順」と二枚看板になり、全順序の推移性が壊れる（`Int(2) < Str("10")` の判定が描画依存になる）ため却下する。全型を 1 本の全順序で覆う。
- **異種混在リストの sort を型エラーにする**: 現行の「何でも並べられる」利便性を失い、既存 script を広く壊すため却下する。型帯順で決定的に並べる。
- **NaN を sort から除外する／エラーにする**: REV-003 で NaN は既に canonical 化され値として流通するため、除外は不整合。数値帯の末尾という固定位置を与える。

#### 17.8.3 データモデル / アルゴリズム

全順序 `TotalOrder` を次で定義する。tree・VM・builtin が参照する**単一実装**とし、backend 間で結果が食い違わないようにする（REV-003 の `NumericOrder` と同じ集約方針）。

```text
TotalOrder::compare(&Value, &Value) -> std::cmp::Ordering   // 常に Less/Equal/Greater を返す（Unordered を作らない）
```

**型帯（type band）順**。まず値の型帯を次の全順序で比較する。帯が異なればこの順で決着する。

```text
Null < Bool < 数値(Int/Float 統合) < Str < List < Dict < Fn(Fn/VmFn) < Error
```

`src/value.rs` の `Value` enum に実在する全 variant（`Int` / `Float` / `Str` / `Bool` / `Null` / `List` / `Dict` / `Fn` / `VmFn` / `Error`）へそれぞれ帯を割り当てる。帯の割り当ては `src/error.rs` の `type_name`（canonical 型名）と整合させ、`Fn` と `VmFn` は同一帯「Fn」とする（`type_name` が両者を `"Fn"` へ畳むのと一致）。

> 注記（brief との差分）: 本 REV の着手指示は Value variant として `Class` / `Instance` / `BoundMethod` を想定していたが、現行 `src/value.rs` にこれらの variant は**存在しない**（クラスは §18 実装順 12 の低優先度機能として未着手）。実在する variant は上記 10 種で、帯割り当てはこの 10 種に対して閉じている。クラス系 variant が将来追加されたときは、本節へ帯を追記するまで `TotalOrder` の対象に含めない（追加は別 REV）。なお本 REV は着手指示（brief）で「§17.6」と参照されたが、doc 上の実体は本節 **§17.8**（§17.6 は REV-021 `remove_dir`）であり、節番号のずれは brief 側の参照ずれで doc の内容には影響しない。

**同一帯内の順序**を帯ごとに次で定義する。

- **Null**: 単一値。常に `Equal`。
- **Bool**: `false < true`。
- **数値（Int / Float 統合）**: `NumericOrder::compare`（REV-003 §17.1）で順序づける。Int と Float は 1 本の帯として統合し、`2^53` 近傍も丸めなしで厳密に並ぶ。`NumericOrder` が `UnorderedNaN` を返す組（NaN が絡む比較）は全順序では決着が要るため、**NaN を数値帯の末尾**（`+Infinity` の後ろ）へ固定配置する。すなわち数値帯内は `-Infinity < 有限値（厳密順） < +Infinity < NaN` とし、NaN どうしは `Equal`（REV-003 で canonical quiet NaN へ畳み済みなので 1 種類）。実装は `NumericOrder::compare` が `Less/Equal/Greater` を返せばそれを採り、`UnorderedNaN` のときだけ「NaN は非 NaN より大、NaN 同士は等しい」規則で `Ordering` へ写す。
- **Str**: Unicode コードポイント順（`str` の `Ord`、すなわち UTF-8 バイト順＝コードポイント順）。Dict key が BTreeMap のコードポイント順で並ぶのと一貫させる（language-spec の `values(dict)`「キーのコードポイント順」と同じ規則）。
- **List**: 要素ごとに**同じ `TotalOrder` で再帰的**に比較する辞書式（lexicographic）順序。先頭要素から順に比較し、最初に差が出た位置で決着する。全要素が等しく一方が他方の prefix なら**短い方を小さい**とする（`[] < [1] < [1,2]`）。空リストは任意の非空リストより小さい。ネストした List も同じ規則で再帰する。
- **Dict**: key でソート済みの `(key, value)` 列として辞書式比較する。Dict backing は `BTreeMap<String, Value>` なので iteration は既に key のコードポイント昇順であり、この順の `(key, value)` 列を要素とみなして List と同じ辞書式規則を適用する。各ペアはまず key を Str 規則で比較し、key が等しければ value を `TotalOrder` で再帰比較する。全ペアが等しく一方が他方の prefix なら短い方（＝エントリ数が少ない方）を小さいとする。空 Dict は任意の非空 Dict より小さい。
- **Fn（Fn / VmFn）**: 関数値は内容順序を持たないため、**帯内では全て `Equal`** とする（安定ソートにより元の相対順序が保たれる）。`FunctionId`（AUD-048）は実行内の同一性比較専用で順序の意味を持たず、script や audit へ生値を公開しないため sort key にしない。tree の `Fn` と VM の `VmFn` は同一帯・帯内 `Equal` で、backend 差を順序へ漏らさない。
- **Error**: 構造化エラー値も内容順序を持たせず、**帯内では全て `Equal`** とする（`error_type` / `message` / `line` を順序キーにしない。message には description が入り安定順序の根拠に乏しいため）。安定ソートで元の相対順序を保つ。

この定義で `TotalOrder` は全順序の 3 性質（完全性・反対称性・推移性）を満たす。帯順は固定全順序、数値帯は `NumericOrder`（REV-003 で推移性・反対称性を保証済み）＋ NaN 末尾固定、Str/List/Dict はコードポイント順と辞書式の合成で、いずれも全順序を保存する。Fn / Error 帯内の `Equal` は反射的で推移性を壊さない。

#### 17.8.4 tree / VM 変更箇所

- 新規 `TotalOrder`（`src/value.rs` もしくは専用 module）: 上記全順序の単一実装。`NumericOrder` を数値帯で呼び出す。tree・VM・builtin が共通に参照する。
- `src/builtin_core.rs` `builtin_sort_budgeted`: 要素を `render_display` で文字列キー化して `a.0.cmp(&b.0)` で安定ソートする現行実装を、`TotalOrder::compare` を comparator とする安定ソート（`sort_by`）へ置換する。描画文字列キーの事前生成はやめ、比較時に `TotalOrder` を適用する。安定性は維持（同順位要素は元順序を保つ）。
- `src/builtin_core.rs` `list_dir`: `names.sort_by_key(|v| v.to_string())` を、名前（`Value::Str`）に対する `TotalOrder::compare`（＝ Str 帯のコードポイント順）での安定ソートへ置換する。list_dir の要素は常に Str なので実効順序はコードポイント順で、現行の `to_string()` キー（Str の表示は内容そのもの）と同一順になる想定だが、キー経路を全順序へ一本化して backend 差・将来の型拡張差を作らない。
- `src/eval.rs` / `src/vm.rs`: sort / list_dir は builtin dispatch 経由で共通実装を呼ぶため、両 backend が同一の `TotalOrder` を通ることを保証する（新たな backend 固有分岐を作らない）。

既存の `builtin_sort_budgeted` doc comment にある「全順序 TotalOrderKey への分離は REV-017 で扱う」の記述が本節で確定する。

#### 17.8.5 budget 課金

sort / 比較の per-element 課金は **execution-control.md の charge table（`collection element を走査・copy・生成 | 1 / element`、`map`/`filter`/`sort`/比較/serialize 等）が正本**で、本 REV はその値を変更しない。現行 `builtin_sort_budgeted` は要素ごとに `render_display`（fuel + per-item byte 上限で有限化）で描画キーを作り課金していた。全順序化後は描画キーを作らないため、課金は次のとおりとする。

- **要素あたり 1 の走査課金**は維持する（charge table の `1 / element`）。sort は要素数 n に対して有限（`O(n log n)` 比較）で、charge table の per-element 課金で上限が効く。
- 文字列描画をやめることで `render_display` の per-item byte 課金（UTF-8 payload scan 課金）が sort 経路から**減る**（HumanDisplay への依存が切れる）。これは課金値の引き下げではなく、不要になった描画作業の除去であり、charge table の既存値はそのまま。
- `TotalOrder::compare` は値どうしの直接比較で、List / Dict の再帰比較も要素走査として `1 / element` の範囲に収まる。深いネストは要素走査課金が自然に積み上がり有限で停止する。
- **新しい課金概念が必要かどうか**（例: 比較 1 回あたりの明示課金を sort の `n log n` 比較へ課すべきか）は execution-control.md を正本とする設計項目として同書側で確定する。本節では「charge table の per-element 課金で有限性は担保され、現行の charge 値を変更しない」ことだけを固定し、charge table の数値編集は行わない。

#### 17.8.6 error

- sort / list_dir は型エラーを増やさない。異種混在リストも型帯順で決定的に並べ、エラーにしない（却下案参照）。
- budget 超過（fuel / deadline / cancel）は現行どおり `ControlStop` → 既存の budget error へ写す。全順序化で新しい error category は追加しない。
- list_dir の非 UTF-8 名・個別 entry 失敗は REV-009（§17.4）の `invalid_encoding` / `directory_read` host error が引き続き正本で、整列はエラーにならなかった名前の集合に対して行う（本 REV は順序だけを変える）。

#### 17.8.7 移行

破壊的変更。仕様 revision を **0.20 → 0.21** へ上げる（`src/embedding.rs` の `LanguageRevision` に `V0_21` を追加し `CURRENT` を更新する実装は未実装側で行う）。

影響する script:

- **数値を含むリストを sort している script**: `sort([3, 10, 2])` が `[10, 2, 3]` から `[2, 3, 10]` へ変わる。桁数の異なる整数・Int と Float の混在で特に観測差が出る。
- **異種混在リストを sort している script**: 従来は全要素の文字列描画のコードポイント順だったが、型帯順＋帯内全順序へ変わる（例: `Bool` と数値の相対位置が描画文字列順から帯順へ変わる）。
- **list_dir の結果順に依存している script**: 名前は Str なので実効順序はコードポイント順のままで、観測差は想定しない（キー経路のみ全順序へ一本化）。
- **sort の budget / fuel 消費に依存している script**: 全順序化で要素の文字列描画（`render_display` の per-item byte 課金）が sort 経路から外れ、代わりに全順序比較の再帰的要素走査へ charge table の per-element 課金（`1 / element`、値は不変）が載る。このため同じ入力でも sort の fuel / budget 消費が revision 境界で変わる。消費差の「向き」は入力の形で決まる（実装注記）: スカラ列の sort では 0.20 は複合ノードを持たず描画 fuel を積まないため 0.21 の per-element 走査課金のぶん 0.21 の方が多くなり得る一方、複合要素（ネスト List / Dict）を含む sort では 0.20 の描画が要素木を全走査するため 0.20 の方が多くなり得る。したがって本節は「0.21 で一律に減る」ことではなく「revision 境界で消費が決定的に変わり、各 revision 内では決定的」ことを観測差として固定する（§17.8.9 の消費差ピン）。いずれの revision でも消費量は charge table の per-element 課金で有限に上限される。budget / fuel 消費は観測可能・監査対象（manifesto 原則5・原則7）であり、この消費差も順序差と同じく revision 境界で説明される観測差として扱う。revision 0.21 を要求する script にのみ適用される。

移行方針: revision 0.21 を要求する script は新しい全順序で、revision 0.20 以下を要求する script は従来の文字列表現順で動く（revision は link 時に検証される §3）。language-spec は実装が入るまで現行挙動（文字列表現比較）を記述し続け、本節への drift note で予告する（部分実装を『次 revision 対応』と表示しない運用規則に従う）。

#### 17.8.8 テスト行列

DESIGN ONLY（本節はテストを記述しない。意図するテストを記述する）。

- 数値順: `sort([3, 10, 2]) == [2, 3, 10]`、Int/Float 混在（`[2, 1.5, 10]` → `[1.5, 2, 10]`）、`2^53` 近傍の Int/Float、`-0.0`/`0.0`、`±Infinity`、NaN が末尾へ来ること。
- Str: コードポイント順。Dict key 順との一貫性。
- 型帯順: 各帯の代表値を 1 リストへ混ぜ、`Null < Bool < 数値 < Str < List < Dict < Fn < Error` の順に並ぶこと。
- List 再帰: `[] < [1] < [1,2] < [2]`、ネストした List、prefix 規則（短い方が小）。
- Dict: 空 Dict、key ソート済み `(key,value)` 列の辞書式、value 再帰、prefix 規則。
- Fn / Error: 帯内で安定（元順序保持）であること。tree の `Fn` と VM の `VmFn` が同一帯・同順位であること。
- 安定性: 同順位要素が元の相対順序を保つこと。
- budget: 大きなリスト・深いネストで per-element 課金により有限停止すること。
- **tree / VM parity**: §17.1.9 の `numeric_strict_comparison` golden fixture と同様に、全順序 sort / list_dir の golden fixture（例: `total_order_sort`）を追加し、tree と VM で順序・budget 消費・error が一致することを固定する。加えて `value.rs` に `TotalOrder` の unit test（全順序 3 性質・帯順・再帰・NaN 末尾）を置く。

#### 17.8.9 受入基準

- sort / list_dir が全ての経路で単一の `TotalOrder` を通り、tree / VM で順序が一致する。
- 数値要素が `NumericOrder`（REV-003）で並び、`sort([3,10,2]) == [2,3,10]`。NaN は数値帯の末尾。
- `src/value.rs` の全 `Value` variant（10 種）に帯が定義され、未定義 variant がない。
- List / Dict が再帰的辞書式順（空・ネスト・prefix 規則を含む）で並ぶ。
- 全順序の 3 性質（完全性・反対称性・推移性）を property / unit test が示す。
- 安定ソートで同順位要素の元順序が保たれる。
- charge table（execution-control.md）の課金値を変更していない。budget 超過で有限停止する。sort の per-element 課金は不変で、全順序比較の再帰的要素走査にも適用する（共有 DAG 要素の比較も per-element 走査課金で有限停止する）。描画 byte 課金が sort 経路から外れ比較走査課金が載ることによる revision 境界の budget / fuel 消費差は、tree / VM 間 fuel が byte-exact 一致しない（`tests/differential.rs` 不変量）ため、同一 backend（embedding tree engine）の 0.20 vs 0.21 比較で固定する（`tests/total_order_budget.rs`）。parity golden fixture（`total_order_sort`）は順序・error の tree / VM 観測一致を担保する。
- revision が 0.20 → 0.21 へ上がり、language-spec は実装完了まで現行挙動を記述し続ける（drift note で予告）。

#### 17.8.10 実装状況

✅ 実装済み（仕様 revision 0.21）。`src/value.rs` に全 10 variant を覆う単一の全順序 `TotalOrder`（純比較版 `compare` と per-element 課金版 `compare_budgeted`）を新設し、`builtin_sort_budgeted`・`list_dir` が revision で経路を分岐する（`>= V0_21` で全順序、`<= V0_20` は従来の描画キー順を温存）。`src/embedding.rs` に `LanguageRevision::V0_21`（`CURRENT=V0_21`、`as_str()=>"0.21"`）を追加し、tree（`src/eval.rs`）/ VM（`src/vm.rs`）に対称な `language_revision` フィールドと set/get を足して run-path で注入する。帯順は §17.8.3 どおり Null < Bool < 数値 < Str < List < Dict < Fn < Error、数値帯は `NumericOrder` 再利用で NaN を末尾固定、List / Dict は再帰辞書式＋prefix 規則、Fn / Error は帯内 Equal。`value.rs` の unit test 12 件（全順序 3 性質・帯順・再帰・prefix・NaN 末尾・安定性・大規模/深ネスト停止）、golden fixture `total_order_sort`（tree / VM parity）、`tests/total_order_budget.rs`（同一 backend 0.20 vs 0.21 の消費差・単調性・完了）で固定する。charge table（execution-control.md）の課金値は不変。REV-001 の 3 層分離（HumanDisplay / CanonicalRepr / TotalOrder）と連動し、`TotalOrder` が規範成果物となる。

#### 17.8.11 設計レビューへの対応（revision 1）

本節は設計レビュー（CHANGES_REQUESTED, MEDIUM 1 / NIT 1）を受けた改訂版。各 finding への対応は次のとおり。

- **F1（MEDIUM, 移行節に budget / fuel 消費の観測差が未記載）**: 対応。§17.8.7 移行の影響 script 列挙へ「sort の budget / fuel 消費に依存している script」を追加し、描画 byte 課金が sort 経路から外れることで消費が減る観測差（manifesto 原則5・原則7 の観測可能性）を revision 0.21 の境界事項として明記した。あわせて §17.8.9 受入基準へ、この消費差を golden fixture の budget 消費比較で固定する 1 項を追加し C7（parity fixture）/ C8（budget 課金）と地続きにした。
- **F2（NIT, brief ↔ doc の節番号ずれ）**: 対応（任意修正を採用）。§17.8.3 の注記へ「brief は §17.6 と参照したが doc 実体は §17.8（§17.6 は REV-021）」の 1 文を追加した。doc の内容妥当性には元から影響しないが、追跡時の混乱を避けるため明文化した。

## 18. 実装順

次期revisionは以下の順で進める。各段階でfmt、Clippy、全test、paired golden、defensive testを通し、意味論変更と内部リファクタを同じcommitへ混在させない。

1. **基準固定**: 本書のerror templates・line/trace・評価順をtest helperへ定義する。現行非適合fixtureを明示する。
2. **内部リファクタ**: VM dispatch分割、tree `exec_stmt` 分割。観測挙動は変えない。
3. **上限統合**: AUD-050の `limits.rs` 集約とAUD-017の128 user frame統一。§17.5（REV-012）のcallee先行precedenceへの切替と `limits.rs` doc comment のcurrent/target明記、roadmap statusの2分割もここへ含める。
4. **Builtin/error基盤**: AUD-049単一registry、AUD-019共通error constructorと完全一致test。§17.1（REV-003）の `NumericOrder` 集約、および §17.4（REV-009）・§17.6（REV-021）で追加するhost error category（`directory_read` / `invalid_encoding` / `directory_not_empty`）のinventory登録もここで行う。
5. **値表現**: AUD-047 List/Dict COW、続けてAUD-048 FunctionId。scaling/identity testを先に追加する。
6. **binding/transaction**: AUD-016 VM fresh cell、AUD-024全language-state REPL journal。FunctionId counterをrollback対象外に固定する。
7. ✅ **bytecode検証面（実装済み）**: §17.7（REV-006）の `VerifiedChunk`/verifier と per-instruction step 課金を軸に、§17.2（REV-004）の `patch_jump` fallible化とbuilder封印、§17.3（REV-005）の `MakeClosure` capture記述子化、REV-018のraw module封印を同一マイルストーンで実装した。VM入口を `VerifiedChunk` へ限定し、停止性はper-instruction課金（verifier非依存）で担保する。観測挙動はstep到達点以外不変。
8. **境界挙動**: ~~AUD-034 `path_join`~~（✅ 完了、revision 0.16）、AUD-036 checked変換/Exited、~~AUD-018 CLI args/stdin~~（✅ 完了、revision 0.18）、~~AUD-033 EOF診断~~（✅ 完了）、~~§17.1（REV-003）の混合数値比較~~（✅ 完了、revision 0.19）。§20の `format_time` `%z`/`%Z` 追加も独立性が高い純粋フォーマット拡張としてこの面で実装できる（capability非依存）。
9. **capability縦切り**: sandbox OnceLockをExecutionContext FilesystemCapabilityへ移し、tree/VM/importを同じpolicyへ接続する。§17.4（REV-009）の `list_dir` 部分失敗/非UTF-8、§17.6（REV-021）の `remove_dir`（空のみ）/`remove_tree`（`RecursiveDelete`）分割をこのcapability面で実装する。
10. **検証基盤**: cargo-fuzzのfrontend/compiler/vm_chunk、続いてcapability完成後にevaluator/differential target。
11. **次期仕様反映**: 全受入基準通過後にだけ `language-spec.md`、`LANG_GUIDE.md`、設計文書、revisionを更新する。
12. **クラス**: 上記基盤と総heap budget完成後、低優先度機能としてlexer→AST→tree→VM→paired testの順で実装する。継承は含めない。
13. **HTTP**: 現在の実装順には含めない。Phase 1–6完了と具体ユースケース承認後にgateを再評価し、承認された場合だけ別計画を作る。

## 19. 文書全体の最終受入基準

- AUD-016/017/018/019/024/033/034/036/047/048/049/050の採用判断、却下案、規範挙動、状態、変更箇所、error、移行、test、受入基準が実装PRから追跡できる。
- §17のREV-003/004/005/006/009/012/017/021についても、同じ8観点（採用判断・却下案・データモデル・変更箇所・error・移行・test・受入基準）が実装PRから追跡できる。REV-017（§17.8）は実装済み（revision 0.21）で、sort / `list_dir` の全順序化を定める。REV-012は既存の第5節（AUD-017）を正本とし、本書で新しい意味論を追加しない。REV-006（§17.7）は停止性をper-instruction課金で担保し、verifierを唯一の停止性根拠にしない。
- §20（`format_time` の `%z`/`%Z` 追加）の採用判断・採用理由・却下案・OS非依存条件・バックエンド非差分・後方互換・変更箇所・残論点・受入基準が実装PRから追跡できる。
- tree/VMで意図しないbackend別期待値が残らない。
- 現行 `language-spec.md` は実装完了まで変更せず、現行挙動の正本として維持する。
- クラスは「設計済み・低優先度」、HTTPは「設計済み・着手禁止」として扱われる。
- 本書の承認だけをもってコード実装済み、security sandbox完成、HTTP利用可能とは表示しない。

## 20. `format_time` のタイムゾーンマーカー（`%z` / `%Z`）

設計ステータス: **設計確定・未実装**

本節は組み込み関数 `format_time(timestamp, format)` に、UTC を示すタイムゾーンマーカー用のフォーマット記号 `%z` と `%Z` を追加する決定を定める。既存6記号（`%Y %m %d %H %M %S`）の挙動は変えず、記号を追加するのみである。

### 20.1 背景・課題

Tsumugi の時刻は内部表現も出力基準も UTC 固定である。`now()` は Clock capability の `now_utc()` を UNIX 秒（`Int`）へ写し、`format_time` の実体 `format_unix_timestamp`（`src/builtin_core.rs`）はその秒数を先発グレゴリオ暦で分解してフォーマット文字列へ展開する。オフセット加算は一切行わない。UTC 固定という設計判断自体は、実行環境のローカルタイムゾーンに結果を依存させない点で本書・マニフェストの決定性原則と整合しており、変更しない。

課題は出力の**自己記述性**にある。現行の `format_time` が置換する記号は日付・時刻の数値成分（`%Y %m %d %H %M %S`）のみで、出力文字列に「この時刻が UTC である」ことを示すマーカーが一切付かない。そのため:

- `format_time(ts, "%Y-%m-%dT%H:%M:%S")` の出力 `2026-10-16T12:33:00` を受け取った側は、文字列だけ見てタイムゾーンを判断できない。
- 判断するには language-spec の「UTC でフォーマット」という記述を参照する必要があり、利用者の手間とローカル時刻への誤認リスクになる。

利用者が UTC を明示したい場合に、出力へ UTC マーカー（`+0000` / `UTC` 等）を含められる手段を提供することで、この自己記述性を回復する。

### 20.2 採用判断

`format_unix_timestamp` の記号置換に、UTC 固定の定数を返す次の2記号を追加する。

| 記号 | 出力（UTC 固定の定数） | 由来 |
|---|---|---|
| `%z` | `+0000` | chrono / C strftime の `%z`（基本形式、コロンなし4桁） |
| `%Z` | `UTC` | chrono / C strftime の `%Z`（タイムゾーン名・略称） |

- 返り値はいずれも UTC 固定なので**環境・ロケール・タイムゾーン設定・入力に依存しない定数**である。
- スコープは `%z` と `%Z` の2記号に限定する。拡張形式 `+00:00`（chrono の `%:z`）や zulu 指示子 `Z` は本決定では追加しない（§20.6 却下案を参照）。
- 既存6記号の挙動・暦計算部分は変更しない。追加は置換チェーンへの定数置換の付加のみである。

### 20.3 採用理由（思想・設計との整合）

1. **決定性・予測可能性（本書第2節、マニフェスト原則）**: UTC 固定のため `%z` は常に `+0000`、`%Z` は常に `UTC` という定数を返すだけで、同じ入力から常に同じ結果になる。環境依存を一切持ち込まない。
2. **自己記述性による監査可能性の向上**: 出力に UTC マーカーが付くことで、監査ログ等に記録された時刻文字列が単体で UTC だと読める。監査性をむしろ高める方向である。
3. **既存記号セットとの一貫性（chrono strftime 互換）**: 現行の `%Y %m %d %H %M %S` は chrono/C strftime の綴りを採用している。追加記号も同じ体系に揃えることで、「tsumugi の `%z`/`%Z` は chrono と同じ」という形で説明が完結し、学習コストと設計書参照の手間を増やさない。
4. **ロケール/timezone 推定を避ける既存判断（§3.12 と同方向）**: §3.12 は「timezone からメッセージ言語を推定する」案を、環境からの暗黙推定を避ける観点で却下している。本決定は環境のローカルタイムゾーンを一切参照せず UTC 定数を返すだけであり、同じ「環境非依存」の方向にある。両者を混同して本決定を「timezone 絡みは見送り済み」と誤って却下しないこと。

### 20.4 OS 非依存性（必須不変条件）

`%z` / `%Z` の出力は、OS の `strftime` / libc、環境変数 `TZ`、ロケール、システムクロックのいずれにも依存してはならない（MUST NOT）。これは次の実装制約で担保する。

- 置換値は定数リテラル（`"+0000"` / `"UTC"`）とする。`chrono::Utc` 等の外部ライブラリやシステムタイムゾーンから値を取得しない。
- 既存の `format_unix_timestamp` が chrono を使わず自前の先発グレゴリオ暦計算で `%Y`〜`%S` を出しているのと同じ理屈で、本記号も OS 差を構造的に持たない。
- 「chrono 互換」とは記号の綴りと出力文字列の見た目を chrono に合わせるという意味であり、実行時に chrono の挙動へ依存することを意味しない。

この不変条件により、ubuntu / macos / windows の CI マトリクスで同一出力になる。

### 20.5 バックエンド差（tree-walk / VM）

挙動差は生じない。`format_time` は `src/builtin_registry.rs` で `execution: PureCore` に分類され、`builtin_core::dispatch` を通じて tree-walk 版と VM 版が同一の `builtin_format_time` → `format_unix_timestamp` 実装を共有する。VM 固有の `format_time` 実装は存在しない。したがって `format_unix_timestamp` に記号を1箇所追加すれば両バックエンドへ同時に反映され、新たな backend 別期待値は発生しない。本書第2節の「規範意味論は一つ」に適合する。

### 20.6 却下案

- **ISO 8601 / RFC 3339 表記へ寄せる（`%z` → `+00:00`、`%Z` → `Z`）**: 記号の綴りは chrono なのに出力が chrono と異なる状態になり、「strftime 互換」という既存の説明と齟齬を生む。利用者が「これは何規格か」を設計書で確かめる手間を別の形で再発させるため、本決定では却下する。拡張形式が必要になった場合は、chrono に実在する別記号 `%:z`（`+00:00`）を将来追加する形で対応する（§20.9 残論点）。
- **`%:z` を本決定に同時追加する**: 最小スコープで始める方針（本書の段階実装の考え方）に反する。`+00:00` を直接出したい実需が確認された時点で追加するのが妥当で、初回スコープには含めない。
- **ISO 8601 専用の新規組み込み関数を追加する（例 `format_iso8601(ts)`）**: フォーマット記号で制御する既存の `format_time` 設計と二重化する。記号追加で目的を達成できるため、新 API は作らない。
- **実装は変えず language-spec の注意書きのみで啓蒙する**: 利用者が手でフォーマット文字列へ `Z` 等を書く運用は、処理系が UTC を保証して付けたマーカーではなく「たまたま置いた文字」にすぎず、自己記述性の本質的な解決にならないため却下する。

### 20.7 後方互換

現行の `format_unix_timestamp` は既知6記号のみを順次 `str::replace` で置換し、それ以外の文字列（`%z` / `%Z` を含む任意の未知記号）は入力にそのまま残す。したがって現状 `format_time(0, "%Y %z")` は `"1970 %z"` を返す。

本決定で `%z` / `%Z` に意味を与えると、**フォーマット文字列中に文字列 `%z` / `%Z` を含め、未置換のまま出力に残ることを期待していた既存スクリプト**の出力のみが変化する（例: `"%Y%z"` が `"1970%z"` から `"1970+0000"` へ）。このリスクは小さいと評価する。

- Tsumugi は alpha 版であり後方互換性を保証対象にしていない（マニフェスト）。
- リポジトリ内のテスト・fixture・examples を調査した時点で `%z` / `%Z` の使用箇所は皆無であり、実利用は6記号のみである。
- `%` は strftime 系で記号導入に使う予約的文字であり、`%z` という並びをそのまま出力したい利用は不自然である。
- 既存6記号の挙動は不変のため、6記号のみを使う既存スクリプトは一切影響を受けない。

### 20.8 変更箇所（実装フェーズの入力）

本節は設計確定・未実装である。実装時には以下へ手を入れる。

**ソースコード**
- `src/builtin_core.rs` の `format_unix_timestamp`: 既存の `.replace("%Y", ...)` チェーンへ `.replace("%z", "+0000")` と `.replace("%Z", "UTC")` を追加する。暦計算部分は不変。`builtin_format_time` 自体は引数検査のみで変更不要。

**ドキュメント**
- `docs/language-spec.md` の組み込み関数表 `format_time` 行: サポート記号へ `%z` / `%Z` を追加し、「タイムゾーン指定は提供しない」という記述を「UTC 固定であり、`%z`（`+0000`）/ `%Z`（`UTC`）で UTC マーカーを出力できる」趣旨へ改訂する。あわせて仕様 revision を次版へ更新する。この統合は本決定の受入基準（paired test 通過）を満たした時点で行い、それまで language-spec を先行変更しない。
- `docs/design.md` の「日時変換の停止性」節: 置換記号が増える点を反映する（推奨）。

**テスト**
- `%z` / `%Z` を含む fixture を `tests/fixtures/` に追加する（`fixture_tests!` が tree/VM 両方を自動生成するため、1 fixture で両バックエンド検証になる）。既存 `local_utils.tsg` / `edge_cases.tsg` への追記でも可。
- 未置換が維持されるべき未知記号（例 `%q`）がそのまま残ることを固定する回帰テストを添え、置換追加による想定外の副作用を検出できるようにする。

### 20.9 残論点

- **zulu 指示子 `Z` を出力する手段**: 利用者が例示した `2026-10-16T12:33:00Z` 形式は、`%Z` → `UTC` を採用した本決定では直接作れない。`Z` を出したい実需が確認された場合に、`%Z` の意味を変えるのではなく別手段（専用記号の追加等）で対応するか検討する。本決定では未対応とする。
- **拡張形式 `+00:00`**: 必要になれば chrono に実在する `%:z` を将来追加する。本決定のスコープ外。
- **逐次 `str::replace` 方式の既存制約**: 現行方式は strftime の `%%` エスケープを扱えない既存の技術的負債を持つ。本決定の `%z` / `%Z` 追加はこの負債を悪化させないが、将来さらに記号を増やす場合はフォーマット文字列を1パスで走査するパーサ化を検討すべきである。本決定の前提ではない。

### 20.10 受入基準

- `%z` が `+0000`、`%Z` が `UTC` を UTC 固定の定数として出力し、入力・環境・ロケール・OS に依存しない。
- 既存6記号の出力と、`%z` / `%Z` 以外の未知記号がそのまま残る挙動が不変である。
- tree-walk / VM で同一出力になり、backend 別期待値を追加しない。
- language-spec への統合と仕様 revision 更新が、paired test 通過を条件に行われている。
