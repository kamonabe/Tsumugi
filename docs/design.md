# Tsumugi — 設計ドキュメント

最終更新: 2026-08-27

## 目的

言語処理系の開発を「入り口レベル」で体験する。
世界に広める言語を作ることが目的ではなく、レキサー・パーサー・評価器の仕組みを理解すること自体が目標。

## 日時変換の停止性

`format_time` は Unix timestamp をUTCの先発Gregorian暦へ変換する。年の算出ではGregorian暦の400年周期（146,097日）を先に除去し、残りを年単位で走査する。これにより年ループはtimestampの大きさによらず最大399回となり、`i64`の全範囲を有限の一定時間で処理できる。ツリーウォーク版とVM版は同じ組み込み関数実装を共有する。

## 設計判断の経緯

### 実装言語: Rust

- `enum` + パターンマッチが AST 表現に適している
- exhaustive match によりノード処理漏れをコンパイル時に検出できる
- 所有権モデルを通じて「なぜGCが必要か」を体感できる
- Python でプロトタイプしてから Rust で再実装する案もあったが、最初から Rust で進めることにした
- **Edition 2024** を採用。`let` chains（`if let ... && ...`）が安定化されており、パーサーの `parse_call` で活用している。CI の toolchain は `stable` 指定で問題なく動作する

### 文法スタイル: Ruby 風（end キーワード）

検討した選択肢:
1. C 系（波括弧 + セミコロン） — 馴染み深いが、セミコロンが煩わしい
2. Rust 風（式ベース + 波括弧） — 面白いがパーサーが複雑
3. Python 風（インデント） — 見た目は良いがレキサーの INDENT/DEDENT 処理が面倒
4. **Ruby 風（end）** — 採用。波括弧なし・セミコロンなし・パーサーもシンプル
5. Lisp 系（S 式） — パーサーは簡単だが読み書きの好みが分かれる
6. ML 風 — 関数型の雰囲気だが馴染みが薄い

決め手: Ruby の経験があり `if ... end` に馴染みがあった。インデント処理も不要でパーサーの実装が素直。

### 型システム: 動的型付け

- 入門レベルでは型チェッカーを作るコストが大きい
- 動的型付けなら実行時にエラーを出せば済むので実装が楽
- 型: Int, Float, Str, Bool, Null, List, Dict

### 実行方式: ツリーウォークインタプリタ

- AST を再帰的にたどって直接実行する最もシンプルな方式
- バイトコード VM も実装済み（`--vm` フラグで切り替え）

### 関数の戻り値: 明示的 return

- Ruby 風の「最後の式が暗黙の戻り値」も検討したが、読む側が戻り値の意図を把握しやすい明示的 return を採用

### 論理演算: and / or / not

- `&&` / `||` / `!` より英単語の方が直感的で覚えやすい

### null 表現: null

- nil (Ruby/Lua) や None (Python) ではなく、最も馴染みのある `null` を採用
- 「余計な用語を増やさない」方針

### エラーメッセージ: 行番号付き + スタックトレース

- `Spanned` 構造体（`Token` + `line: usize`）でレキサーが行番号を追跡
- AST の各 `Stmt` バリアントにも `line` フィールドを保持
- パースエラー・ランタイムエラーの両方で「N行目: ...」形式のメッセージを表示
- REPL では入力ごとに1行目からカウント（ファイル実行時はファイル先頭から通し番号）
- 関数内で発生したエラーには呼び出し経路（スタックトレース）を付加する
- トレースは内側（エラー発生地点に近い関数）→ 外側（呼び出し元）の順で表示
- `TraceFrame` 構造体（関数名 + 呼び出し元行番号）のベクタとしてエラーに保持
- ツリーウォーク版: `Evaluator.call_stack` で呼び出し経路を追跡、エラー時にコピーして付加
- VM版: エラー発生時に `self.frames` から呼び出し経路を収集して付加

### 変数の再代入: `let` なしの代入文

- `let x = 1` で宣言、`x = 2` で再代入の2段階方式を採用
- 未宣言の変数への代入はランタイムエラーとする（typo 防止）
- 再代入時はスコープを内側→外側へ探索し、最初に見つかった変数を更新する（クロージャ的なセマンティクス）
- これにより `while` ループ内でカウンタ変数を自然に更新できるようになった
- 代替案: Python のように代入文を変数宣言と区別しない方式もあったが、「明示的な宣言を必須とする」方が学習者に変数のライフサイクルを意識させやすいと判断

### リスト・辞書: コレクション型

設計判断:
- リストは `Vec<Value>` で実装。型混在を許容する（動的型付けと整合）
- 辞書は `BTreeMap<String, Value>` で実装。キーを文字列に限定することでシンプルさを維持
- `HashMap` ではなく `BTreeMap` を採用した理由: `print()` 時の出力がキー順で安定し、テストが書きやすい
- インデックスアクセスは `expr[expr]` 構文でリスト・辞書・文字列に統一的に使える
- インデックス代入は `Stmt::IndexAssign` で表現。対象は識別子に固定し、変数セルを直接更新する方式（AUD-013）
- 負のインデックス対応（Python 風）: `-1` で末尾要素にアクセス可能
- 辞書の存在しないキーへのアクセスは `null` を返す（エラーにしない）— 存在チェックは `== null` で判定する想定

### for ループのイテレーション: スナップショット方式

for ループはイテレーション対象のコレクションを **開始時にクローン（コピー）** して、コピーに対して走査する。

```rust
let items: Vec<Value> = match &collection {
    Value::List(list) => list.clone(),  // ← 全要素のコピー
    ...
};
```

検討した選択肢:
1. **スナップショット方式（clone）** — 採用。ループ開始時の状態を保持して走査する
2. **参照方式（借用）** — ループ中にリストを変更できなくなる（Rust の借用ルール制約）。push/pop が使えないのは不便
3. **破壊的方式（オリジナルを直接走査）** — ループ中に要素を削除するとイテレーション回数が変わり、直感に反する動作になる

スナップショット方式の挙動:

```
let test = ["hoge", "piyo", "kani"]
for item in test
    if item == "hoge"
        pop(test)        # オリジナルから "kani" を削除
    end
    print(item)
end
# → "hoge", "piyo", "kani" の3回ループする（開始時の状態で走査）
# → ループ後の test は ["hoge", "piyo"]（pop の効果はオリジナルに反映）
```

なぜこの方式か:
- **安全性**: ループ中にリストを変更してもイテレータが壊れない。「ループが何回回るか」は開始時点で確定する
- **直感性**: 「3要素あるリストを for で回したら3回回る」が常に成り立つ。途中で要素が消えて2回で終わる、という驚きがない
- **トレードオフ**: コレクション全体のメモリコピーが発生するため、巨大なリスト（数万要素〜）では余分なメモリを消費する。現時点では許容範囲と判断

「ループ中に不要な要素を除外したい」ユースケースは、新しいリストを組み立てるパターンで対応する:

```
let result = []
for item in test
    if item != "kani"
        push(result, item)
    end
end
test = result
```

高階関数 `filter()` を使えば、より簡潔に書ける:

```
let result = filter(test, fn(item) item != "kani" end)
```

### 組み込み関数の拡充

- `print` に加えて `len` / `push` / `keys` / `type` を追加
- `push` は破壊的操作として設計（`Env::get_mut()` で直接リストを変更）
- 関数的スタイル（新しいリストを返す `append`）も検討したが、入門レベルでは破壊的操作の方が直感的と判断
- `type()` はデバッグや型チェック用途。動的型付け言語では実行時に型を確認したい場面が多い

## アーキテクチャ

```
ソースコード (.tsg ファイル or REPL 入力)
    │
    ▼
┌──────────┐
│  Lexer   │  文字列 → Spanned トークン列（行番号付き）
└──────────┘
    │
    ▼
┌──────────┐
│  Parser  │  Spanned トークン列 → AST（行番号付き Stmt）
└──────────┘
    │
    ▼
┌──────────┐
│Evaluator │  AST → 実行（エラー時に行番号参照）
└──────────┘
    │
    ▼
  標準出力
```

### レキサー (lexer.rs)

- 1文字ずつ先読みしてトークンを生成
- `line` カウンタを保持し、`\n` を消費するたびにインクリメント
- 各トークンの生成時に現在の行番号を `Spanned` に記録
- 改行を `Newline` トークンとして保持（文の区切りとして使う）
- スペース/タブはスキップ、`#` 以降はコメントとしてスキップ
- 2文字演算子（`==`, `!=`, `<=`, `>=`）は先読みで判定
- 数値リテラルのオーバーフロー（`i64` 範囲外）は `Token::Error` として記録し、パーサーがパースエラーとして報告する

### パーサー (parser.rs)

- 再帰下降構文解析
- `Vec<Spanned>` を入力として受け取る
- 演算子の優先順位（低→高）: or → and → 比較 → 加減 → 乗除 → 単項 → 関数呼び出し
- `MAX_AST_DEPTH = 256`をblock・括弧・unary・`elif`などの再帰edgeへ共通適用し、nested f-stringの子Lexer/Parserにも親深度を引き継ぐ
- 左結合BinOpと連続call/indexは複合ノードの構築直後に実AST深度を非再帰検査し、最初の上限超過ノード（深度257）で停止して、それ以上の深い連鎖を蓄積しない
- ブロックは終端トークン（`end`, `else`）を目印に解析
- エラー発生時に `Spanned.line` を参照して「N行目: ...」メッセージを生成
- エラー回復: 1つ目のエラーで停止せず、複数のパースエラーをまとめて報告する
  - `parse()` の戻り型は `Result<Program, Vec<TsumugiError>>`
  - 文のパースに失敗したら `synchronize()` で次の文境界までスキップし、パースを継続する
  - リカバリポイント: 改行 / EOF / 文の先頭キーワード（`let`, `fn`, `if`, `while`, `for`, `return`, `import`, `try`, `break`, `continue`）

### 評価器 (eval.rs)

- AST を再帰的にたどって Value を返す
- `run()`入口ではProgramを非再帰worklistでpreflightし、Parserを迂回した公開ASTも深度256超過なら`StackOverflow`として拒否する
- 環境（Env）で変数スコープをスタック管理
- 関数呼び出し時に新しいスコープを push、終了時に pop
- `return` 文は EvalResult::Return で早期脱出を表現
- ランタイムエラー（型エラー、未定義変数、ゼロ除算等）に `Stmt.line` を付与
- 組み込み関数の実装は `builtin.rs` に分離（eval.rs は制御フロー・式評価に専念）
- `builtin.rs` はコンテキスト依存のビルトイン（push/pop/map/filter/each/print/input/exit/args）を処理し、それ以外は `builtin_core.rs` に委譲

### 環境 (env.rs)

- スコープのスタック（Vec<HashMap>）
- 変数検索は内側 → 外側の順
- `set()` は現在のスコープに変数を新規定義
- `update()` は内側→外側へ探索し、既存変数を更新（見つからなければエラー）
- `get_cell()` は変数の `Rc<RefCell<Value>>` セルを返す（インデックス代入・push の破壊的更新で使用）
- 関数定義はグローバルな HashMap で管理

### エラー型 (error.rs)

- `TsumugiError` enum でパースエラー（`Parse`）とランタイムエラー（`Runtime`）を構造的に区別
- 各バリアントに `line: usize` と `message: String` を保持
- `Runtime` バリアントには `kind: ErrorKind` フィールドを持ち、エラーの種別を構造的に表現する
- `ErrorKind` enum（17バリアント）: `ZeroDivision`, `Type`, `Index`, `Name`, `StepLimit`, `StackOverflow`, `Sandbox`, `Import`, `Argument`, `IntOverflow`, `ControlFlow`, `CollectionLimit`, `Conversion`, `BuiltinType`, `Iteration`, `Internal`, `Runtime`
- `error_type()` メソッドは `kind.as_str()` を返す — try/catch で `e["type"]` として利用される
- `Display` 実装で「N行目: メッセージ」形式の出力を生成（従来と同じ形式を維持）
- 全箇所で `TsumugiError::runtime(line, msg)`（メッセージから kind を自動推定）/ `TsumugiError::runtime_with_kind(line, kind, msg)`（明示指定）/ `TsumugiError::parse(line, msg)` コンストラクタを使用
- `From<String>` は廃止済み — 文字列の再パースに頼らず、行番号を構造として保持する方式に統一
- `classify_runtime_error()` はメッセージ文字列から kind を推定するフォールバック関数として残存（`runtime()` 経由の既存コード向け後方互換）

## テスト設計

### ユニットテスト

各モジュールに `#[cfg(test)] mod tests` で配置。

| モジュール | テスト観点 |
|---|---|
| `value.rs` | `is_truthy` 判定（List/Dict含む）、`Display` 表示 |
| `env.rs` | 変数 set/get/get_mut、スコープ shadowing、外側スコープ参照、update（再代入） |
| `lexer.rs` | トークン化、行番号付与、エスケープ、演算子、キーワード、コメント |
| `parser.rs` | 各文のAST生成、リスト/辞書リテラル、インデックスアクセス/代入、エラー時の行番号含有 |
| `eval.rs` | 算術・比較・論理、関数呼び出し、リスト/辞書操作、組み込み関数、エラーケース全般 |

### 統合テスト（ゴールデンテスト）

`tests/integration.rs` で `.tsg` ファイルをバイナリ実行し、出力を期待値と比較する。

- 正常系: `tests/fixtures/<name>.tsg` + `<name>.expected`
- エラー系: `tests/fixtures/<name>.tsg` + `<name>.expected_err`

テストデータ追加時は `.tsg` と `.expected` / `.expected_err` ペアを置いて `integration.rs` にテスト関数を追加するだけ。

### CI

GitHub Actions (`.github/workflows/ci.yml`) で push / PR 時に自動実行:

1. `cargo fmt --check` — フォーマット整合性
2. `cargo clippy -- -D warnings` — 静的解析
3. `cargo test` — 全テスト実行

## 設計時の文法スナップショット（revision v0.3）

この番号は設計履歴上の文法revisionであり、規範となる最新仕様は[`language-spec.md`](language-spec.md)のversion 0.5、実装packageは0.1.0である。構文を変更する場合は、まず規範仕様と`LANG_GUIDE.md`を更新し、この節は設計履歴として扱う。

```
program        = top_level_stmt*
top_level_stmt = import_stmt | stmt
stmt           = let_stmt | assign_stmt | index_assign | return_stmt
               | if_stmt | while_stmt | for_stmt | break_stmt | continue_stmt
               | fn_def | try_catch_stmt | expr_stmt
let_stmt       = "let" IDENT "=" expr NEWLINE
assign_stmt    = IDENT "=" expr NEWLINE
index_assign   = postfix "[" expr "]" "=" expr NEWLINE
return_stmt    = "return" expr NEWLINE
if_stmt        = "if" expr NEWLINE block ("elif" expr NEWLINE block)* ("else" NEWLINE block)? "end" NEWLINE
while_stmt     = "while" expr NEWLINE block "end" NEWLINE
for_stmt       = "for" IDENT "in" expr NEWLINE block "end" NEWLINE
break_stmt     = "break" NEWLINE
continue_stmt  = "continue" NEWLINE
import_stmt    = "import" STRING NEWLINE
try_catch_stmt = "try" NEWLINE block "catch" IDENT NEWLINE block "end" NEWLINE
fn_def         = "fn" IDENT "(" params? ")" NEWLINE block "end" NEWLINE
expr_stmt      = expr NEWLINE
block          = stmt*
params         = IDENT ("," IDENT)*
expr           = or_expr
or_expr        = and_expr ("or" and_expr)*
and_expr       = cmp_expr ("and" cmp_expr)*
cmp_expr       = add_expr (("==" | "!=" | "<" | ">" | "<=" | ">=") add_expr)*
add_expr       = mul_expr (("+" | "-") mul_expr)*
mul_expr       = unary_expr (("*" | "/" | "%") unary_expr)*
unary_expr     = ("not" | "-") unary_expr | postfix
postfix        = primary ( "(" args? ")" | "[" expr "]" )*
primary        = INT | FLOAT | STRING | "true" | "false" | "null"
               | IDENT | "(" expr ")" | list_literal | dict_literal
               | lambda
list_literal   = "[" (expr ("," expr)* ","?)? "]"
dict_literal   = "{" (expr ":" expr ("," expr ":" expr)* ","?)? "}"
lambda         = "fn" "(" params? ")" NEWLINE block "end"
               | "fn" "(" params? ")" expr "end"
args           = expr ("," expr)*
```

## 今後の候補

| 優先度 | 項目 |
|---|---|
| 低 | クラス（継承なし・合成で拡張） |

## クロージャ・無名関数の設計判断

### 実装状況（実施済み）

Phase 1 で `Expr::Call { callee: Box<Expr> }` への構造変更と `Value::Fn` の追加を行い、Phase 2 で無名関数（ラムダ）とクロージャ（値キャプチャ方式）を実装した。Phase 3 で参照キャプチャ（`Rc<RefCell>`）に移行し、カウンターパターンをサポートした。

### AST 変更（実施済み）

```rust
// Call: 任意の式を呼び出し対象にできる
Expr::Call { callee: Box<Expr>, args: Vec<Expr> }

// Lambda: 無名関数式
Expr::Lambda { params: Vec<String>, body: Vec<Stmt> }
```

### 参照キャプチャ方式（現行）

検討した選択肢:

| 方式 | 挙動 | カウンター | 採用 |
|---|---|---|---|
| 値キャプチャ（clone） | 定義時の値をコピー | ❌ | — (旧方式) |
| 参照キャプチャ（`Rc<RefCell>`） | 定義元と値を共有 | ✅ | ✅ 採用 |

当初は値キャプチャで実装していたが、以下の理由で参照キャプチャへ移行した:
- カウンターパターン（状態を保持するクロージャ）が動かないのは、学習用であっても表現力の不足が大きい
- `Rc<RefCell>` 導入で `Value` の `PartialEq` / `Clone` の derive が壊れるが、手動実装のコストは限定的だった
- 所有権モデルの限界を `Rc<RefCell>` で突破する体験自体が「なぜGCが必要か」の学習に直結する

### 実装方式

**ツリーウォーク版:**
- `Env` の各スコープが `HashMap<String, Rc<RefCell<Value>>>` で変数を保持
- 関数定義時（`FnDef` / `Lambda`）に `env.capture_all()` で全変数の `Rc` を共有コピー
- クロージャ呼び出し時は `set_shared()` でセルを新スコープに注入し、書き込みがセル経由で定義元に伝播
- 名前付き関数は呼び出しframe構築時に宣言名へ現在の関数値をself-bindする。定義時captureへ自身を含めないため、全関数に恒久的な`cell → function → captured → cell`循環を作らない
- self-bindingはcaptured bindingの後、parameterの前に設定する。同名parameterはself-bindingをshadowする

**VM版:**
- `CallFrame` に `locals_cells: Vec<Option<SharedValue>>` を追加
- 名前付き関数のlocal slot 0にcallee自身を置き、ツリーウォーク版と同じself-bindingを実現
- 無名lambdaもstack layout用にslot 0を持つが、resolver上の名前はsource identifierにならない`<lambda>`とし、暗黙self名として公開しない
- `MakeClosure` 時に対象ローカル変数を `Rc<RefCell<Value>>` セルに昇格（`ensure_local_cell`）
- `GetLocal` / `SetLocal` はセル経由の読み書きにフォールバック（非キャプチャ変数は従来通りスタック直接）
- `SetUpvalue` オペコードを追加し、クロージャ内から外部変数への書き込みをサポート

### 1行ラムダの構文判断

```
# 複数行
let f = fn(x)
    return x * 2
end

# 1行（return 省略可）
let f = fn(x) x * 2 end
```

1行ラムダでは `fn(params)` の後に改行がなければ式を1つだけ読み、暗黙的に `return` 扱いとする。`return` を明示してもよい。

### 既知のトレードオフ

- 関数値の等価性は仕様上常に`false`とする方針だが、現行treeは型エラー、VMは`false`となるためAUD-014で統一する
- 循環参照: `Rc<RefCell>`にはcycle collectorがなく、捕捉変数のListへその変数を捕捉したclosureを`push`すると、`cell → List → closure → cell`の循環を言語コードから構成できる。短命なscriptでは影響が限定的でも、REPLや長時間実行では解放されないメモリが累積し得る
- 捕捉範囲: treeは定義時に見える全bindingを共有し、VMは自由変数だけをupvalue化する。treeではclosure生成コストと不要な値の生存期間が可視binding数に比例する
- 意味論の重複: 共通Resolver/HIRはなく、scope・名前解決・call・比較・mutation・importの規則をEvaluatorとCompiler/VMが別々に実装する。拡張時はdifferential testで両engineの観測可能な挙動を固定する必要がある
- パフォーマンス: `RefCell`の実行時借用チェックに加え、値cloneやVMのstack/cell管理コストがある。VMが常に高速とは限らず、現行Criterionではworkloadごとに優劣が大きく異なる

## モジュール / import の設計

### 概要

`import "path.tsg"` 構文で別ファイルの関数・変数を現在のスコープに取り込む。
ツリーウォーク版・VM版の両方で動作する。

### 設計判断

#### import をトップレベルに限定

`import` はプログラムのトップレベルでのみ有効とし、関数・条件分岐・ループ・`try` / `catch`・複数行ラムダのブロック内ではパースエラーにする。

ツリーウォーク版はimport文へ到達した実行時にファイルを読み込む一方、VM版はコンパイル時にASTをインライン展開する。非トップレベルを許可すると、false branchでの読込有無、loopでの実行回数、関数呼び出し時の読込、相対パス基準、エラー発生フェーズの差がさらに広がるため、配置をトップレベルへ限定した。ただし、importより前の出力、実行中に生成したmodule、失敗時の副作用順など、トップレベルでも評価時点の違いは観測可能であり、完全な統一には至っていない（AUD-030）。

Parserはプログラム直下とblock内の文を区別し、block内のimportをファイルI/O前に `import はトップレベルでのみ使用できます` で拒否する。これによりtree版・VM版でエラーフェーズとメッセージが一致し、到達不能branch内でもimport先へアクセスしない。

#### フラットなインジェクション方式を採用

検討した選択肢:
1. **フラットインジェクション（全展開）** — 採用。import 先のファイルを実行し、定義された名前をすべて現在のスコープに注入する
2. **名前空間分離（`module.func()` 形式）** — 不採用。ドット演算子のパース追加が必要で、クラスの前準備として後回し
3. **選択的 import（`import { add, sub } from "math.tsg"`）** — 不採用。入門レベルでは過剰

フラットインジェクションを選んだ理由:
- 実装が素直（import 先を評価/コンパイルして現在の環境に追加するだけ）
- 名前衝突は「後から定義した方が勝つ」で解決（Python と同じ）
- 将来名前空間が欲しくなったら、クラス実装時にドット演算子を追加すれば自然に対応できる

#### パス解決: 実行中スクリプトからの相対パス

- import を書いたファイルの親ディレクトリを基準に解決する
- `std::fs::canonicalize()` で正規化し、循環 import 検出に使う
- REPL では `cwd` を基準にする

#### 循環 import: サイレントスキップ

- 正規化パスの `HashSet` で管理
- 正常に完了したimportの2回目以降は何もせずスキップ（エラーにしない）
- 読み込み・パース・実行に失敗した場合はmarkerと一時的な`base_dir`を復元し、同じpathを再試行可能にする
- A→B→Aのように同じ正規化pathへ戻る循環は、深度検査前の既訪問判定で従来どおり停止する
- `Evaluator`と`Compiler`はloaded集合と分離した`import_depth`でactive chainだけを数え、root scriptを除く128ファイルまで許可する。129段目は`ErrorKind::Import`で拒否し、marker挿入前に停止する
- active depthと一時的な`base_dir`はinner処理の成否にかかわらず呼び出し元の値へ復元する
- Python の挙動に近い（部分的に実行済みのモジュールオブジェクトを返す）

#### VM版: コンパイル時にインライン展開

- VM ではファイルを読み込んでパースし、得られた AST を現在の `Compiler` でそのままコンパイルする
- ランタイムの新しい OpCode は不要（コンパイル時に解決される）
- import先ファイルからの再帰的なimportに対応するため、コンパイル中に `base_dir` を一時切り替えする

### 制約・トレードオフ

- 名前空間が分離されないため、大規模なプロジェクトでは名前衝突のリスクがある
- ファイルのトップレベルで副作用のあるコード（print 等）がある場合、import 時に実行される
- 将来的に名前空間分離が必要になったら、クラス + ドット演算子の実装と合わせて対応する

## 参考資料

- 「Writing An Interpreter In Go」(Thorsten Ball)
- 「Crafting Interpreters」(Robert Nystrom) — https://craftinginterpreters.com/
- 「低レイヤを知りたい人のためのCコンパイラ作成入門」(Rui Ueyama)

## バイトコード VM の設計

### 概要

ツリーウォークインタプリタに加え、バイトコードコンパイラ + スタックVMを並行実装する。
`--vm`フラグで実行方式を切り替え可能。Lexer・Parser・ASTは共有し、両方式が同じ規範仕様を満たすことを目標とする。ただし、意味解析以降は別実装であり、比較、index代入、builtin、importなどに既知の差が残る。現時点のVMは互換性・性能ともに実験的backendとして扱い、非適合は`roadmap.md`で管理する。

### 動機

- ツリーウォークは「AST → 再帰で直接実行」で、言語意味論と実装の対応を追いやすい
- バイトコードVMは「AST → 一次元の命令列 → dispatch loop」という別方式を学べる。再帰関数や高階関数で高速な場合がある一方、辞書、f-string、単純loopではtreeより遅い現行workloadもあり、一律の高速化を保証しない
- 言語処理系の学習として、コード生成、stack frame、upvalue、例外unwind、VM実行loopはツリーウォークとは異なる知見を得られる
- 共通Resolver/HIRがないため、既存テストの流用だけでなく、stdout・stderr・終了コード・副作用順を比較する厳密なdifferential testを互換性条件とする

### アーキテクチャ

```
ソースコード
    │
    ▼
┌──────────┐
│  Lexer   │  （既存、共有）
└──────────┘
    │
    ▼
┌──────────┐
│  Parser  │  （既存、共有）
└──────────┘
    │
    ▼ AST
    ├──────────────────────────────────┐
    ▼                                  ▼
┌──────────┐                    ┌──────────────┐
│Evaluator │ (--vm なし)        │   Compiler   │ (--vm あり)
│ツリーウォーク│                │ AST → Chunk  │
└──────────┘                    └──────────────┘
    │                                  │
    ▼                                  ▼ Chunk（命令列 + 定数テーブル）
  標準出力                        ┌──────────┐
                                  │    VM    │ スタックマシン
                                  └──────────┘
                                       │
                                       ▼
                                     標準出力
```

### 構成ファイル

| ファイル | 役割 |
|---|---|
| `src/opcode.rs` | OpCode enum — VM が実行する命令の種類 |
| `src/chunk.rs` | Chunk — 命令列（`Vec<OpCode>`）+ 定数テーブル（`Vec<Value>`）+ 行番号 |
| `src/compiler.rs` | Compiler — AST を走査して Chunk を生成する |
| `src/vm.rs` | Vm — Chunk をスタックマシンとして実行する |
| `src/builtin_core.rs` | 組み込み関数の共通ロジック — VM/ツリーウォーク両方から呼ばれる |

### スタックマシンの動作原理

全ての計算が「スタックから値を取り出す → 演算する → 結果をスタックに戻す」で表現される。

例: `print(1 + 2)` のコンパイル結果と実行:

```
定数テーブル: [1, 2, null]
命令列:
  0: LoadConst(0)   → スタック: [1]
  1: LoadConst(1)   → スタック: [1, 2]
  2: Add            → スタック: [3]
  3: Print(1)       → "3" を出力、スタック: []
  4: LoadConst(2)   → スタック: [null]   ← print の戻り値
  5: Pop            → スタック: []        ← 式文なので捨てる
  6: Return         → 実行終了
```

### 変数名のhybrid解決

Compilerは識別子を、現在のlocal slot、lexical upvalueの順にstatic解決する。どちらにも見つからない通常read/callee/plain assignmentはcompile errorにせず、`GetGlobal` / `GetGlobalForCall` / `SetGlobal`として実行時へ残す。

top-levelの`let` / `fn`は`RegisterGlobal(name, slot)`で宣言実行時に公開される。VMの`globals`は値ではなく`name → top-level slot`だけを保持し、read/write時はtop-level frameのstackまたは`locals_cells`へ到達する。したがってstatic upvalueとruntime globalは同じbinding/cellを共有し、別のglobal value storeは持たない。

### 実装フェーズ

| Phase | 内容 | 状態 |
|---|---|---|
| 0+1 | OpCode定義 + Chunk + Compiler + VM骨格 + 定数 + 算術 + Print | ✅ 完了 |
| 2 | 変数（let / 再代入 / 参照） | ✅ 完了 |
| 3 | 比較 + 条件ジャンプ（if / while / for） | ✅ 完了 |
| 4 | 関数定義・呼び出し（コールフレーム） | ✅ 完了 |
| 5 | クロージャ（upvalue） | ✅ 完了 |
| 6 | 組み込み関数（53個） | ✅ 完了 |
| 7 | 互換性修正（min/max混合型・remove・write_file） | ✅ 完了 |
| 8 | 浮動小数点 IEEE 754 統一（VM ゼロ除算→inf/NaN、ツリーウォーク Float 比較追加） | ✅ 完了 |

### Rc\<Chunk\> による関数値の共有

`Value::VmFn` の `chunk` フィールドは `Rc<Chunk>` で保持する。

```rust
VmFn {
    name: String,
    arity: usize,
    params: Vec<String>,
    chunk: Rc<Chunk>,     // ← ポインタコピーで共有
    upvalues: Vec<Value>,
}
```

採用理由:
- `Value` は `Clone` を要求される（スタック操作・クロージャ生成で頻繁にコピーが走る）
- `Chunk` は命令列（`Vec<OpCode>`）+ 定数テーブル（`Vec<Value>`）を持つため、ディープコピーはコストが高い
- `Rc<Chunk>` にすることで、`Value::clone()` 時はポインタコピー（参照カウント+1）だけで済む
- 関数の `Chunk` は immutable（実行中に書き換わらない）のため `Rc` で十分。`RefCell` は不要

### 組み込み関数の共通化（builtin_core.rs）

ツリーウォーク評価器（`builtin.rs`）と VM（`vm.rs`）で重複していた組み込み関数ロジックを `builtin_core.rs` に集約した。

```
builtin_core.rs
├── dispatch(name, &[Value], line) → Result<Option<Value>>
├── builtin_len, builtin_push, ...（~45個の純粋関数）
└── format_unix_timestamp, is_leap_year（ヘルパー）
```

設計方針:
- **引数は評価済み `&[Value]`** — 引数評価自体は各engineが担当し、共通モジュールは評価済みの値だけ受け取る
- **context builtinは評価前に共通検証** — `input` / `args` / `exit` / `push` / `pop` / `map` / `filter` / `each`は共通validatorでarityを検査する。`push` / `pop`は第1引数が識別子かも検査し、失敗時は引数を評価しない。VMはbuiltin branch内の`ValidateBuiltinCall` opcodeで実行時に検査する
- **エンジン固有のビルトインは各モジュールに残す** — `push`/`pop`（binding更新）、`map`/`filter`/`each`（クロージャ呼び出し）、`print`/`input`/`exit`/`args`（I/O・プロセス操作）
- **user bindingをbuiltinより優先する** — `print`以外の識別子calleeはlocal/upvalue/runtime globalを先に探し、bindingがない場合だけbuiltinへfallbackする。VMは`JumpIfGlobalDefined`で実行時のglobal登録状態を分岐し、builtin branchとuser-call branchの評価順を混在させない
- **破壊的List操作はbindingへ書き戻す** — builtin `push`/`pop`が選ばれた場合、第1引数はlocal/upvalue/runtime globalの識別子bindingに限定する。第1引数の値を先にsnapshotし、更新後のListを同じbindingへ書き戻す。一時Listは永続化先がないため拒否する
- **新規ビルトイン追加は `builtin_core.rs` + dispatch テーブルへの登録のみ** — 両エンジンに自動的に反映される

### 設計判断

#### なぜツリーウォークを残すのか

- 既存の全機能が動く実装を壊さない安全弁
- VM が段階的に育つ間、ユーザーは `--vm` なしで従来通り使える
- 両方式の出力を比較するテストが書ける（将来）

#### OpCode の粒度

- 1命令 = 1つの操作（LoadConst, Add, Print 等）。複合命令は作らない
- 理由: デバッグしやすい。命令列を読んだときに動作が自明
- 将来の最適化（例: LoadConst + Add → AddConst）は後から追加可能

#### Print を専用 OpCode にした理由

- Phase 1 では汎用的な関数呼び出し機構がまだない
- print は出力の確認に必須なので、最小構成で動かすために専用命令とした
- Phase 4 で関数呼び出しを実装した後、`CallBuiltin` OpCode + `builtin_core.rs` 方式に移行した

## 実行安全性の設計

誤操作と過剰な資源消費を抑えるdefense-in-depth機構。これは敵対的コードを隔離するsecurity sandboxではなく、非信頼コードにはOS・コンテナ側の権限、CPU、メモリ、filesystem、実行時間制限を併用する。
実行量・collection・I/Oの制御は環境変数で設定し、構文・AST・call frame・import chainの構造的上限はコンパイル時定数で固定する。環境変数方式は未設定時に既定値または制限なしのfail-openとなる（開発時のUXを阻害しない）。

### 制御機構一覧

| 設定 / 定数 | 対象 | 動作 |
|---|---|---|
| `TSUMUGI_MAX_STEPS` | ループ反復 + 関数呼び出し | 上限超過でランタイムエラー |
| `TSUMUGI_MAX_COLLECTION_SIZE` | List/Dictの生成・拡張とList生成builtin | 要素数上限超過でランタイムエラー |
| `TSUMUGI_SANDBOX` | 全ファイル操作（read/write/import） | 許可パス外へのアクセスをブロック |
| `TSUMUGI_ENV_ALLOW` | `env()` 関数 | 許可リスト外のキーは null を返す |
| `MAX_CALL_DEPTH = 128` | ユーザー関数のcall frame | 上限超過で`StackOverflow` |
| `MAX_AST_DEPTH = 256` | Parser生成物と公開AST | Parser生成時とCompiler/Evaluator入口で拒否 |
| `MAX_IMPORT_DEPTH = 128` | rootを除くactive import chain | 129段目を`Import`エラーで拒否 |

### コールフレーム深度制限

定数 `MAX_CALL_DEPTH = 128` でユーザー関数呼び出しのネスト深度を制限する。超過すると「スタックオーバーフロー: 再帰が深すぎます」ランタイムエラー。

- ツリーウォーク版: `call_stack.len()` でチェック
- VM版: `frames.len()` でチェック
- main() は8MB stackのthreadで実行し、通常の評価再帰に余裕を持たせる

### 構文・AST深度制限

`src/limits.rs`の`MAX_AST_DEPTH = 256`をLexer・Parser・AST・Compiler・Evaluatorで共有する。

- Parserの再帰counterはblock、括弧、unary、`elif`、nested f-string間でリセットせず継承する
- 左深BinOpとpostfix chainは各ノード構築直後に非再帰worklistで実深度を検査し、最初の深度257ノードで停止してそれ以上を蓄積しない
- Parserが完成した各文はbody・lambdaを含めて再検査する
- `Compiler::compile`、`compile_repl_line`、`Evaluator::run`はProgram全体を非再帰preflightし、外部コードが公開ASTを手組みしてParserを迂回しても処理系自身の再帰走査前に`ErrorKind::StackOverflow`を返す。このAPIはProgramを借用するため、制限を大幅に超えるcaller所有ASTの破棄までstack-safeにするものではない

### import chain深度制限

`MAX_IMPORT_DEPTH = 128`でroot scriptを除くactive import chainを制限する。loaded module総数とは分離したcounterを使うため、REPLで正常importが累積しても許容量は減らない。canonicalize・sandbox・既訪問skipの後、marker挿入前に深度を検査し、129段目は`ErrorKind::Import`で拒否する。

### TSUMUGI_* 環境変数の保護

`env()` 組み込み関数は `TSUMUGI_` プレフィックスで始まるキーへのアクセスを常に `null` で返す。`TSUMUGI_ENV_ALLOW` 許可リストの設定に関わらず適用される。WindowsではキーをUnicode uppercaseへ変換してからprefixを照合し、ASCII大小文字違いとASCII名へ別名解決され得るUnicode case variantを保護する。その他のOSではcase-sensitiveに照合する。

理由: `TSUMUGI_SANDBOX`, `TSUMUGI_MAX_STEPS`, `TSUMUGI_ENV_ALLOW` 等のランタイム制御用環境変数の値がスクリプトに漏洩すると、サンドボックスの許可パスリストや実行上限が攻撃者に見えてしまう。

### map/filter/each のステップカウント

VM版の `call_fn_value()`（map/filter/each のコールバック呼び出し）にもステップカウントを適用。巨大リストに対するコールバックでステップ予算をバイパスされる問題を防止。

REPLではtree版・VM版とも入力開始時にステップカウンタを0へ戻す。importは同じ評価呼び出し内で実行するため、1入力から到達したimport・関数・callbackは予算を共有する。

### REPL入力の状態transaction

REPLの未捕捉エラーは、外部I/Oを含む完全なACID transactionではない。ただし、次入力を安全に処理するための内部構造状態は入力開始時点へ戻す。

- Compiler: `locals`、scope/loop、import集合、base directoryをcheckpointし、compile errorまたはVM runtime errorで復元
- VM: value stack、call frame、try handler、step状態、runtime global registryのname→slot対応をcheckpointし、未捕捉runtime errorで復元
- top-levelの`locals_cells`は正常入力間で引き継ぎ、既存closureとの参照同一性を維持
- try/catchは開始時の有効local slot数を保存し、unwind時はtry-local cellだけを破棄する。既存localがtry中に初めてcell化された場合は昇格を維持する
- catch済みエラーは通常の制御フローとしてcommitする
- エラー前の外部I/Oや共有cellへの代入をどこまでrollbackするかは、完全な意味論を今後仕様化する

### コレクションサイズ上限

`TSUMUGI_MAX_COLLECTION_SIZE`（デフォルト1,000,000）を、言語から到達するList/Dictの主要な生成・拡張経路へ共通適用する。チェックは可能な限り追加前に行い、上限超過時は`ErrorKind::CollectionLimit`を返す。

対象にはliteral、`push`、辞書の新規キー、`range`、`split`、`read_lines`、`keys`/`values`、`map`/`filter`、`list_dir`、for用変換を含む。文字列のbyte数や全Valueの総メモリ量を追跡するglobal heap quotaではない。

### 設計判断

- **エラーにするか null を返すか**: ファイルI/O のサンドボックス違反はランタイムエラー（意図せぬアクセスを即座に検出するため）。env() の許可リスト外は null を返す（「存在しないキー」と同じ挙動にして、スクリプト側に漏洩情報を与えない）
- **OnceLock による初期化**: 許可リストはプロセス起動時に一度だけ読み込む。テスト時に動的に切り替えたい場合の課題が残っている（ロードマップ「サンドボックスの OnceLock テスタビリティ」参照）
- **import のサンドボックス対応**: canonicalize 後に check_path を呼ぶ。ファイルが存在しない場合は canonicalize が先に失敗するため、存在しないパスへの import は「ファイルが見つかりません」エラーになる（サンドボックス違反ではない）
- **破壊的操作のdirectory entry検査**: `remove` / `remove_dir` / `rename`は中間componentをcanonicalizeしつつfinal componentを保持する`check_entry_path`を使う。final symlinkが許可範囲内ならlink entry自体だけを操作し、targetの場所は認可・操作対象にしない。中間symlinkは従来どおり解決し、許可範囲外への迂回を拒否する

### 未対応の既知の制約

- **caller所有ASTの破棄**: 公開AST APIのpreflightはCompiler/Evaluator自身の再帰走査を防ぐが、借用元がParserを使わず任意深度の再帰ASTを構築した場合、そのASTの通常Dropはcaller側のhost stackを使用する
- **シンボリックリンクとsandbox制約**: 破壊的操作がfinal symlinkを決定論的にtargetへ置換する問題はAUD-032で修正した。ただしtargetが未作成のdangling final symlinkを通じた`write_file` / `append_file`は、fallback正規化がlink entryを許可範囲内と判定した後にOSが許可範囲外のtargetを作成し得る。またcheckと実I/Oの間のsymlink差し替えraceは防げず、許可外pathの存在情報がcanonicalize結果から観測できる場合もある（AUD-020）
- **総ヒープメモリ制限なし**: List/Dictの要素数上限はあるが、巨大文字列、要素自身のサイズ、全コレクション合計量にはglobal quotaがなくOOMの可能性が残る
- **input() の無制限読み込み**: 改行なしの巨大入力でOOM、入力なしで無限ブロック


## 変更履歴

### 2026-08-27: index assignmentの対象bindingと評価順の統一（AUD-013）

**問題:** VM Compilerは`xs[i] = v`の対象を`resolve_local`だけで解決していたため、closureがキャプチャした変数やfunction内から見たtop-level変数への代入がcompile errorになり、プログラム全体が実行されなかった。さらにVMは対象の値を`GetLocal`でsnapshotしてからindex/valueを評価し、更新結果を`SetLocal`でbinding全体へ書き戻すため、index/valueの評価中に同じbindingへ加えられた変更を消していた。treeはindex/valueを評価してから対象を解決するため、未定義変数の報告時点も異なった。`set_index`の型エラーメッセージもtreeの3種に対しVMは1種だった。

**決定:** `target[index] = value`を「target binding解決 → index評価 → value評価 → in-place更新」の左から右の順に統一する。targetはlocal・upvalue・runtime globalのいずれでもよく、未定義bindingはindex/valueの副作用より前にruntime Name errorとして報告する。更新はbinding全体の書き戻しではなくコレクションへの直接代入とし、index/valueの評価中に生じた変更を上書きしない。境界判定も更新時点の最新状態に対して行う。targetの事前検証はAUD-012の`push` / `pop`と同じleft-to-right規範に揃える。

**実装:** `Stmt::IndexAssign`の対象を`Expr`から識別子名へ変更し、Parserが`ident[...] =`の形だけを受理する事実をASTへ反映した。これにより両engineにあった到達不能な分岐（treeのruntime error、VMの「ネストされたインデックス代入」compile error）を削除した。境界判定・コレクション上限・エラーメッセージは`builtin_core::assign_index`へ集約し、tree evaluatorとVMが同じ関数を呼ぶ。

Compilerは`MutationTarget`（`opcode`へ移動し`push` / `pop`と共有）で対象を解決し、runtime globalの場合だけ`RequireGlobal(name)`をindex/valueより前に生成する。`SetIndex`は`SetIndex(MutationTarget)`となり、popした`[index, value]`だけを受け取る。VMは`resolve_binding_storage`でcell化済みなら`Rc<RefCell<Value>>`、未cell化ならstack slotを特定し、そこへ直接代入する。storage解決はindex/valueの評価後に行うため、評価中にcell昇格が起きても書き込み先を誤らない。

**不採用案:** VM旧仕様のsnapshot/writebackにtreeを合わせる案は、`xs[f()] = g()`で`f` / `g`が同じbindingを更新した場合にlost updateを仕様化することになるため不採用とした。対象解決をindex/value評価後に遅延する案は、未定義bindingの報告がAUD-012の「破壊対象を引数評価前に検査」と非対称になる。`GetGlobal`で存在確認する案は検証のためだけにコレクション全体をcloneするため、値を積まない`RequireGlobal`を追加した。ネストしたインデックス代入（`xs[0][1] = val`）のサポートは言語機能の追加であり本変更の対象外とする。

**互換性と境界:** VMでcompile errorだったcaptured/global targetへの代入は実行可能になり、未定義targetは`try` / `catch`可能なruntime errorになる。VMでindex/value評価中の同一binding更新が消えていた挙動は、変更が保持される側へ変わる。VMの型エラーメッセージはtreeの3種（リストのインデックス型、辞書のキー型、非コレクション）に統一される。エラーkindは`runtime` / `index`のまま維持し、`type`への再分類はAUD-019で扱う。ネストindex代入の構文エラーは両engineで従来どおり同一である。

**回帰テスト:** paired golden testでcaptured list/dict、多段クロージャ、逃げたclosureの独立状態、function内からのglobal代入、宣言前targetのcatch可能なName error、関数引数・forループ変数、負index、新規キー追加、target解決がindex/valueの副作用に先行すること、index/valueの左から右評価、value評価中の変更保持、index評価でのlist伸張、value評価でのlist縮小後の境界判定、型・境界エラーメッセージを検証する。両engineのREPLテストでは失敗入力後の回復と入力をまたいだ同一bindingへの書き込みを検証する。ParserのunitテストでAST上のtargetが識別子であることと、識別子以外の左辺を受理しないことを固定する。

### 2026-08-27: context依存builtin契約の統一（AUD-012）

**問題:** `input`等の不正arityでVMだけ引数の副作用を実行し、`push` / `pop`は一時ListをVMだけ受理していた。さらにtreeは`push`の第2引数を評価してからtargetを読み、VMはtargetを先にsnapshotするため、引数内で同じbindingを再代入した場合の書戻し結果が異なった。collection系の型・空Listエラーも`runtime`と`builtin_type`に分かれていた。

**決定:** context builtinはuser/builtin選択後、arityを引数評価前に検査する。`push` / `pop`は第1引数のsource式が識別子かも事前検査し、local・captured・runtime globalのList bindingだけを破壊的更新対象とする。正当な引数は左から右へ評価し、`push`は第1引数の値を先にsnapshotして第2引数評価後に同じbindingへ書き戻す。`map` / `filter` / `each`は外側の両引数を評価してからList型を検査する。

**実装:** `builtin_core`へ共通validatorを追加し、treeはbuiltin dispatch直後に呼び出す。Compilerはruntime globalのuser bindingが存在しないbuiltin branch内だけに`ValidateBuiltinCall`を生成し、VMは引数bytecodeより前に実行する。treeの`push` / `pop`も共通coreで更新値・戻り値・`builtin_type`を生成し、local/upvalue/runtime globalへ同じsnapshot/writeback規則を適用する。

**不採用案:** 一時Listを許可すると、永続化先のない破壊的操作となり、identifier版では`null`、旧VMのtemporary版では更新済みListという二重の戻り値契約が残るため不採用とした。Compilerで静的エラーにするとdead branchでも失敗し、`try` / `catch`可能なruntime validationを壊すため、専用opcodeで到達時に検査する。

**互換性と境界:** VMで不正context builtinの引数副作用やtemporary `push` / `pop`に依存したコードは変化する。treeで`push`の第2引数から同じtargetを変更していた場合も、先に読んだsnapshotを書き戻す結果へ変わる。callbackのnon-callable・arity messageや他のengine固有診断の完全一致はAUD-019で継続する。

**検証:** arity副作用抑止、temporary拒否、target再代入、higher-order builtinの評価順・error kindをtree/VMの同一smoke入力で比較し、既存の全target test・lint・buildを実行する。

### 2026-08-26: runtime global name resolutionの導入（AUD-011 後半）

**問題:** VM Compilerは全ASTを実行前に走査し、識別子をその時点で既知のlocal/upvalue slotへだけ解決していた。そのため、dead branch・短絡された式・未呼出し関数内の未定義名もcompile errorとなり、先行I/Oを実行せず、`try` / `catch`でも捕捉できなかった。また、関数定義後に宣言されるtop-level globalや後続関数をfunction bodyから参照できず、tree evaluatorが持つlive global scopeと不一致だった。

**決定:** 既知のlocal/upvalueは従来どおりstatic slot/cellへ解決し、そこで未解決の通常read、callee、単純代入だけをruntime global lookupへfallbackするhybrid方式を採用する。名前の存在は式・代入を実行した時点で検査し、到達しないコードでは検査しない。top-level bindingは宣言の実行時に公開し、hoist/predeclareはしないため、宣言前の直接参照は従来どおりruntime Name errorとなる。関数定義時に未解決だった名前は、呼出し時点のglobalを参照する。

**実装:** Compilerは`compile_name_read`で`GetLocal → GetUpvalue → GetGlobal`の順にloweringし、識別子calleeの未解決時は診断を保つ`GetGlobalForCall`、単純代入の未解決時は`SetGlobal`を生成する。script top-levelの新規`let` / `fn`は値がstack slotへ置かれた直後に`RegisterGlobal(name, slot)`を実行する。importは同じroot Compilerへinline展開されるため、import先のtop-level宣言も同じ手順で登録される。

VMのregistryは`HashMap<String, usize>`として**名前からtop-level slotだけ**を保持し、値やcellを複製しない。global read/writeはtop-level `locals_cells[slot]`があればその`Rc<RefCell<Value>>`を、なければ同じstack slotを使う。これにより、後からclosure captureでcell化されてもstatic upvalueとdynamic globalが同じbindingを見る。すべてのglobalを一律cell化せず、既存のREPL値commit挙動も変更しない。`run_repl_chunk`は未捕捉error時にregistryのname→slot対応もcheckpointへ戻し、失敗入力で登録されたstale slotを次入力へ残さない。

**不採用案:** tree evaluatorへ全面static resolverを追加する案はdead code、catch可能なName error、live globalを破壊する。top-level名を事前登録する案は未初期化状態を導入し、宣言到達前の可視性とAUD-016を巻き込む。全名前をdynamic化する案は既存local/upvalue slotの性能とlexical bindingを不要に変える。globalごとに別cellを作る案はtop-level slot・upvalueとのidentityを二重化し、AUD-004で修正したclosure共有とAUD-024のrollback境界を壊すため採用しない。

**互換性と境界:** VMでcompile errorだった未定義名は、実際に到達した場合だけruntime Name errorになる。このためエラー前のprint/I/Oは実行され、同一実行内の`try` / `catch`で捕捉可能になる。これはtree semanticsへの意図的な統一である。同一scopeの`let`再宣言identityはAUD-016、未捕捉REPL入力の値mutation commit/rollbackはAUD-024、error message/traceの完全一致はAUD-019として維持する。`push` / `pop`の識別子更新はAUD-012、index assignmentのupvalue・評価順はAUD-013でそれぞれ統一済みである。

**回帰テスト:** paired golden testでdead `if` / loop / catch、short-circuit、未呼出しfunction/lambda、`return`後、catch可能なName error、未定義calleeの引数非評価、invalid call validation優先、forward global read/write、nested closure、後続cell化、top-level mutual recursion、block-local非公開を検証する。import fixtureではimport内の後続関数とcaller側の後続globalを、両engineのREPLテストでは入力間forward referenceを検証する。VM REPLでは失敗import後にregistry entryとslotがrollbackされ、同名globalを安全に再定義できることも検証する。

### 2026-08-26: user function call validation順序の統一（AUD-011 前半）

**問題:** ツリーウォーク版はuser function callでstep/depth、callee、callable/arityを検査してから引数を評価していた。一方、VMはcalleeと全引数を評価した後の`Call`で検査していたため、wrong arityやnon-callableでも引数のprint・collection mutation・I/OがVMだけ実行された。引数内のruntime errorとcall validation errorの優先順もengine間で異なり得た。

**決定:** user function callを「step予算とcall depth → callee評価 → callable/arity検査 → 引数を左から右 → body」の順に統一する。callee評価やvalidationに失敗した場合は引数を評価せず、引数評価中に失敗した場合は残りの引数とbodyを実行しない。既存tree semanticsを正とする。

**実装:** `PrepareCall`でstep/depthをcallee評価前に、`ValidateCall(arg_count)`でcallable/arityをcallee評価後かつargs評価前に検査する。Compilerは`PrepareCall → callee → ValidateCall → args → Call`を生成し、`Call`はframe構築を担当する。不正bytecodeへの防御として`Call`側でもframe depth、stack shape、callable/arityを再検査するが、stepは二重countしない。

**不採用案:** treeをVM旧仕様のargs-firstへ合わせる案は、invalid callで新たに外部副作用を発生させ、既存treeコードのerror precedenceを壊すため不採用。Compilerで静的にarityを検査するだけの案はfirst-class functionやcallee式を扱えず、non-callableも解決しない。

**互換性と境界:** VMでinvalid callの引数副作用に依存していたコードは変化する。必要な副作用はcall引数の外で明示的に実行する。builtin固有の引数・callback契約はAUD-012で統一済みであり、non-callableを含むerror messageの完全一致はAUD-019で扱う。dead codeの未定義名、global forward reference、未定義argの実行前拒否はcall validationとは別のname resolution問題であり、AUD-011後半（前節）のruntime global fallbackで対応する。

**回帰テスト:** paired golden testで成功時のcallee→arg1→arg2→body、wrong arity/non-callable時の引数非評価、callee runtime errorの優先、引数の左から右評価とarg error時のbody非実行を検証する。tree/VM REPLテストではstep上限をcallee評価前に検査し、caught `limit` error後もcallee副作用が発生しないことを検証する。

### 2026-08-26: for変数のclosure bindingを反復単位へ統一（AUD-010）

**問題:** ツリーウォーク版は`for`の各反復でloop変数に新しいcellを作るため、`[1, 2, 3]`から作ったclosureはそれぞれ`1`、`2`、`3`を返した。一方、Compiler/VMはloop全体で同じlocal slotと昇格済み`locals_cells`を更新していたため、すべてのclosureが最終値`3`を返した。

**決定:** `for`のloop変数は各反復開始時にfresh cellへbindする。異なる反復のclosureはcellを共有せず、同一反復内の複数closureと通常代入は同じcellを共有する。各iterationを独立lexical scopeとする既存仕様およびtree版の挙動を正とする。

**実装:** `Compiler::compile_for`のstatic slot layoutは維持し、現在要素を積む前に前反復のloop-var slotを`Pop`する。VMの`Pop`が該当`locals_cells` mappingを解除した後、`Index`の結果が同じslot位置を占めるため、次にcaptureされた際は新しいcellへ昇格する。escaping closureが保持する旧`Rc`は生存する。body scope、`LoopState.locals_count`、break/continue target、try unwind、REPL checkpoint、VM/opcode、tree evaluatorは変更しない。

**不採用案:** tree版をloop全体で単一cellを共有する仕様へ変更する案は、既存のobservable behaviorとiteration scope仕様を壊し、AUD-005で修正したerror/control-flow cleanupを再び複雑化するため不採用。loop変数をcompile-time body scopeへ移す案はbreak/continue cleanupとshadowingを組み替え、同一scope再宣言のAUD-016を巻き込む。専用opcode追加も既存`Pop`で必要なcell detachを表現できるため採用しない。

**互換性と境界:** VMでloop変数を直接captureしたclosureがすべて最終値を返すことに依存したコードは、反復ごとの値を返すように変わる。意図的に単一cellを共有する場合はloop外で変数を宣言し、各反復でそのouter bindingへ代入してcaptureする。同一scopeの`let`再宣言identityはAUD-016、未捕捉REPL入力の値commit/rollbackはAUD-024として分離する。

**回帰テスト:** paired golden testでdirect capture、同一反復内代入、named function、List/Dict/Unicode文字列、nested loop、shadowing、`continue`、`break`、caught error、`return`をtree/VM双方で検証する。VM REPLテストではloop終了後にcollection/index/loop-var相当slotを再利用してcellへ再昇格・更新しても、旧closureのcellが変化しないことを検証する。

### 2026-08-26: if・try・catchのブロックスコープ統一（AUD-008）

**問題:** ツリーウォーク版は`if`、`try`、`catch`を現在scopeで実行していたため、block内の`let`やcatch変数が外へ漏れた。一方、Compiler/VMは各bodyをlexical scopeとして扱い、終了時またはtry unwind時にlocal slotを破棄していた。

**決定:** 実行対象に選ばれた各`if` / `elif` / `else` body、`try` body、`catch` bodyを独立scopeとする。`try`と`catch`は相互にlocalを共有せず、catch変数はcatch内限定とする。`let`は現在scopeでshadowし、通常代入は最寄りの外側bindingを更新する。escaping closureはblock localのcellを保持できる。

**実装:** tree evaluatorに、bodyの結果を保持してscopeをpopしてから伝播する`exec_scoped_block`を追加。正常終了、runtime error、`return`、`break`、`continue`の全経路でscopeを解放する。catchは専用scopeへError値をbindし、同様に結果保存後にpopする。Compiler/VMは既にこの契約を満たすため変更しない。

**不採用案:** VMをtree版の旧非scope仕様へ合わせる案は、条件分岐ごとのlocal slot差、未初期化値、try途中失敗時の部分宣言、REPL stack/checkpointを再設計する必要があり、AUD-001/004で修正したinvariantを再び危険にするため不採用。

**互換性と境界:** block内で宣言して外へ値を渡していたコードは、外側で宣言してblock内では代入する形へ移行する。正常終了または同一実行内でcatchされたエラーでは、scope解放自体はrollbackを行わずouter assignment、collection mutation、外部I/Oを保持する。未捕捉REPL入力のcommit/rollbackはengine間で未統一のためAUD-024で継続する。dead branchのname resolution（AUD-011）、同一scope再宣言のcell identity（AUD-016）も別課題とする。

**回帰テスト:** paired golden testでshadowing、outer assignment、caught error前のcollection mutation/I/O、catch lifetime、error・break・continue後も生存するescaping closure、returnを検証する。tree/VM REPLテストではif/try/catch localが次入力へ漏れないことに加え、解放slotの再利用とclosure保持cellの分離を検証する。

### 2026-08-26: importのトップレベル限定（AUD-007）

ツリーウォーク版のruntime importとVM版のcompile-time inlineで意味論が一致しないため、`import`をプログラムのトップレベルだけで有効な構文に確定した。Parserがプログラム直下とblock内を区別し、非トップレベルのimportをファイルI/O前の共通パースエラーとして拒否する。if/while/for/関数/try/catch/複数行ラムダのparserテストと、tree/VM共通のintegrationテストを追加した。

### 2026-08-26: 深層監査修正（REPL transaction・scope回復・collection上限）

#### VM REPLの失敗時rollback

**問題:** インクリメンタルCompilerとVMが、compile/runtime error後の部分状態を次入力へ持ち越していた。Compilerだけにlocal slotが残ると`GetLocal`の範囲外indexでRust panic、stale loopが残ると未patchの`Jump(0)`、callee frameや一時値が残ると古い処理の再開・誤値参照へ到達できた。

**修正:** `compile_repl_line`をcheckpoint付きにし、compile errorではCompiler全状態を復元。compile成功後のruntime errorでは`main`がCompiler checkpointを復元し、`Vm::run_repl_chunk`もframe/stack/handlerを入力開始状態へ戻す。正常入力ではtop-level `locals_cells`を引き継ぐ。

#### try/catchとtree scopeの回復

- `TryHandler`がtry開始時の有効local slot数を保存。unwind時はtry-local cellだけを破棄し、catch変数slotとの衝突を防ぎつつ既存localの新しいcell昇格を保持
- tree版while/forはbody結果を一旦保持し、error/return/break/continueの全経路でscopeをpopしてから伝播
- tree REPLのstep予算を入力単位でreset
- 失敗importは`base_dir`とimport markerを復元し、同じpathを再試行可能に変更

#### コレクション・builtin契約

- List/Dict literal、`push`、辞書新規キー、map/filter、keys/values、list_dir、args、for変換などへ共通collection limitを適用
- VMの`exit`/`args` arity・type検査と`input` CRLF除去をtree版へ整合
- callback内break/continueをtree版でもcontrol-flow error化
- VM callbackのslot 0に関数自身を置き、direct named callbackの自己再帰を修正

#### 回帰テスト

`tests/integration.rs`にstdin駆動のREPL subprocessテストを追加。compile/runtime error後の継続、panic非再発、frame再開防止、top-level/try cell、loop scope、step reset、import retry、collection limit、callback/exit契約を同一process内の連続入力で検証する。

### 2026-08-25: 安全性バグ修正（パニック防止・ハンドラリーク）

外部レビューで発見された「ユーザー入力だけでホスト言語がパニックする」問題と「VM内部状態が壊れる」問題を修正。

#### 修正内容

| 対象 | 問題 | 修正 |
|------|------|------|
| `slice()` | 負数の `i64` を `usize` に変換するとラップアラウンド。`start > end` で Rust パニック | 負数は 0 にクランプ。`s > e` なら空を返す |
| `abs()` | `i64::MIN` の `abs()` が表現不能でパニック | `checked_abs` を使い、失敗時は `IntOverflow` エラーを返す |
| `range()` | `end - start` が `i64` オーバーフロー（例: `MIN` と `MAX` の差） | `checked_sub` でオーバーフロー検出、エラーを返す |
| VM `ReturnValue` | try 内で return すると `TeardownTry` を通らず `try_handlers` にハンドラが残留 | フレーム pop 後に `try_handlers.retain()` で古いハンドラを除去 |
| VM `break`/`continue` | try 内で break/continue してもハンドラが残り、後続のエラーが誤キャッチされる | `LoopState` に `try_depth` を追加し、break/continue 時に必要数の `TeardownTry` を発行 |

#### 設計判断

- **slice の負数**: Python の負数インデックス（末尾から数える）の導入は見送り、0 クランプとした。将来仕様として追加する場合は別途設計する
- **try ハンドラの二重防御**: VM 側の `retain()` とコンパイラ側の `TeardownTry` 発行の両方を入れた。コンパイラ側だけでも動作するが、VM 側にも安全弁を残すことで return 経路の漏れに対するフォールバックとした
- **abs/range のエラー**: `try/catch` で捕捉可能な Tsumugi エラーとして返す。パニックさせない

#### リグレッションテスト

- `tests/fixtures/slice_edge.tsg` — 負数・逆転範囲・範囲外
- `tests/fixtures/try_break_continue.tsg` — try 内 break/continue/return
- `tests/fixtures/overflow_edge.tsg` — abs(i64::MIN)、range オーバーフロー


### 2026-08-25: 仕様統一（and/or 意味論 + ループブロックスコープ）

AST評価器とVM間で動作が異なっていた2つの仕様を統一。

#### and/or の意味論統一

**変更前:**
- AST版: 両辺を常に評価、結果は常に `Bool`
- VM版: 短絡評価、左辺が偽なら `false` / 真なら右辺の値をそのまま返す

**変更後（両エンジン共通）:**
- Python/JS風の短絡評価 + 値返し
- `and`: 左辺が falsy なら左辺を返す。truthy なら右辺を評価して返す
- `or`: 左辺が truthy なら左辺を返す。falsy なら右辺を評価して返す

**実装:**
- AST版: `eval_binop` から `And`/`Or` を除去し、`Expr::BinOp` 処理内で短絡評価
- VM版: `JumpIfFalseKeep` / `JumpIfTrueKeep` 新オペコードを追加（値を pop せずにジャンプ判定）

**判断理由:** 動的型付け言語として最も一般的な動作。`let x = config or default` イディオムが使える。

#### ループブロックスコープ導入

**変更前:**
- AST版: while/for 本体にスコープなし。ループ内 `let` がループ外から参照可能
- VM版: コメントで「ツリーウォーク版に合わせてスコープを開始しない」と明記

**変更後（両エンジン共通）:**
- while/for の本体は反復ごとに新しいブロックスコープを作成
- ループ内で `let` した変数はそのイテレーション終了時に破棄される
- 外側の変数を変更するには代入（`x = x + 1`）を使う

**破壊的変更:**
- `let x = x - 1` パターンでループカウンタを更新していたコードは `x = x - 1` に書き換えが必要
- 既存テスト `control_flow.tsg` を修正

**判断理由:** ほぼすべての現代言語がブロックスコープ。VM のスタック膨張問題を根本解決。バグ防止。


### 2026-08-25: 高優先残件修正（再帰制限・多段クロージャ・import復元）

#### map/filter/each 再帰制限の追加

**問題:** `call_fn_value()`（AST版・VM版）にコールスタック深度チェックがなく、コールバック内の再帰で Rust プロセスのスタックが溢れる可能性があった。

**修正:**
- AST版 (`builtin.rs`): `count_step()` + `MAX_CALL_DEPTH` チェックを `call_fn_value` 冒頭に追加
- VM版 (`vm.rs`): `MAX_CALL_DEPTH` チェックを `call_fn_value` 冒頭に追加（`count_step` は既存）

#### 多段クロージャキャプチャ

**問題:** `resolve_upvalue` が直近親のローカル変数しか検索せず、3段以上のネスト（`outer → middle → inner`）で `inner` が `outer` の変数を直接キャプチャできなかった。

**修正:**
- `Upvalue` 構造体に `is_local: bool` フィールドを追加（`true` = 親ローカル、`false` = 親upvalue経由）
- `Compiler` に `enclosing_upvalues` フィールドを追加し、祖先の変数チェーンを伝搬
- `build_ancestor_vars()` で親の enclosing_locals + enclosing_upvalues を合成して子に渡す
- `resolve_child_upvalues()` で子の `is_local=false` upvalue に対し、親が自動的に中間キャプチャを登録
- VM の `MakeClosure` を `GetLocal`/`GetUpvalue` 両方に対応するよう拡張

**設計判断:** Crafting Interpreters の「upvalue-of-upvalue」方式を採用。中間関数が変数を直接使わなくても、子孫のために自動キャプチャする。

#### import 失敗時の base_dir 復元

**問題:** `exec_import` で `base_dir` 変更後にパース失敗すると `?` で早期リターンし、`base_dir` が復元されなかった。`try/catch` でキャッチして続行すると以降の相対 import が壊れる。

**修正:** `parser.parse().map_err(...)? ` を `match` に書き換え、エラー時に `self.base_dir = prev_base_dir` を復元してからエラーを返す。

#### リグレッションテスト

- `tests/fixtures/deep_closure.tsg` — 3段ネスト、ラムダ多段、ミュータブル多段キャプチャ
- `tests/fixtures/map_recursion_limit.tsg` — map 経由再帰のスタックオーバーフロー検出


### 2026-08-25: 中〜低優先残件修正

#### mkdir() サンドボックス: 祖先遡上 canonicalize

**問題:** `normalize_path` が直接の親ディレクトリだけを canonicalize し、それも存在しない場合は文字列上の `.`/`..` 解決のみで落ちていた。中間にシンボリックリンクがある場合、`create_dir_all` がリンク先にディレクトリを作成してサンドボックスを脱出できた。

**修正:** 存在する最も近い祖先ディレクトリまで遡って `canonicalize` し、残りのパスコンポーネントを join する方式に変更。これにより中間のシンボリックリンクが確実に解決される。

#### f-string パーサの余剰トークン検査

**問題:** `f"{1 2}"` のような入力で最初の式 `1` だけを受理し、残りの `2` を無視していた。

**修正:** `parse_fstr` でサブパーサーの `parse_expr()` 後に `is_at_end()` を確認し、余剰トークンがあればパースエラーを返す。

#### 負の Unix 時刻処理

**問題:** `format_unix_timestamp` で負のタイムスタンプを処理する際、`timestamp % secs_per_day` が負になり、`as u32` でラップアラウンドして時分秒が不正になっていた。年計算も1970年以降にしか対応していなかった。

**修正:** `div_euclid` / `rem_euclid` を使って日数と秒を正しく計算。年計算で `days < 0` の場合は1970年から逆方向に遡るループを追加。

#### ゴールデンテストの終了ステータス確認

**問題:** 正常系テストが stdout 比較のみで、プロセスがパニックやエラー終了しても stdout が一致すれば通過していた。

**修正:** `run_golden_test_mode` に `output.status.success()` の assert を追加。異常終了時は stderr を表示して失敗する。

#### VM版 REPL の状態非保持（未対応・既知の制約）

入力ごとに新規 Compiler + VM を生成するため、前の入力で宣言した変数が保持されない。修正にはコンパイラのインクリメンタル対応またはアーキテクチャ変更が必要。ロードマップに記録。


### 2026-08-25: VM版 REPL インクリメンタルコンパイル対応

**問題:** VM版 REPL が入力ごとに新規 `Compiler` + `Vm` を生成していたため、前の入力で宣言した変数や関数が保持されなかった。

**修正（案A: インクリメンタルコンパイル）:**
- `Compiler::compile_repl_line(&mut self, ...)` を追加。`self` を消費せず、既存の `locals` テーブルを保持したまま新しいステートメントだけをコンパイルする
- `Vm::new_repl()` を追加。空のフレームスタックで VM を生成
- `Vm::run_repl_chunk(&mut self, chunk)` を追加。既存スタック上に新しいトップレベルフレームを差し替えて実行する
- `main.rs` の `run_repl_vm` を `Compiler` と `Vm` をループ外で保持する方式に書き換え

**動作:**
```
tsumugi:vm> let x = 10       ← コンパイル → スタック[10]
tsumugi:vm> print(x)         ← GetLocal(0) → スタック[10] から読める
tsumugi:vm> fn f() return x end  ← 関数定義が保持される
```

**設計判断:**
- チャンクは入力ごとに新規作成（コンパイル位置はリセット）だが、`locals` テーブルは蓄積される
- スタック上の値は保持され、新しいチャンクのスロット番号は前回の続きを参照する
- ステップカウンタは入力ごとにリセット（各入力で予算全額を使える）
- 不要になった `execute_vm()` 関数を削除


### 2026-08-25: 深層設計修正（locals_cells リーク・レキシカルスコープ・サンドボックス TOCTOU）

#### VM locals_cells リーク修正

**問題:** `PopN`/`Pop` がスタック値を削除するだけで `locals_cells` のエントリを残していた。スコープ終了後に同じスロット番号を別変数に再利用すると、古いセルから値を読み取ってしまう。

**修正:** `Pop`/`PopN` 実行時に、削除対象のスロットに対応する `locals_cells[slot]` を `None` にクリア。クロージャが保持する `Rc<RefCell<Value>>` への参照は影響なし（参照カウントで生存）。

#### AST 評価器のレキシカルスコープ化

**問題:** `Env` がフラットなスコープスタックで、関数呼び出し時に `push_scope` するだけだったため、呼び出し元のローカル変数がスコープ検索で到達可能だった（動的スコープ）。

**修正:** `Env` に `push_call_frame()` / `pop_call_frame()` を追加。関数実行時にグローバルスコープ以外を退避し、キャプチャ変数+引数だけの独立環境で実行。終了後に復元。

**設計判断:** グローバルスコープ（`scopes[0]`）は関数内からも参照可能とした。これはトップレベルで定義した関数や変数を関数内から使えるようにするため。キャプチャは定義時に`capture_all()`で到達可能な変数セルを記録し、その`Rc<RefCell<Value>>`を共有するため、レキシカルスコープと参照キャプチャの要件を満たす。

#### サンドボックス TOCTOU 修正

**問題:** `check_path()` が `Result<(), TsumugiError>` を返していたため、検証で正規化したパスが破棄され、実際のファイル操作は元の生文字列パスで実行されていた。`..` を含むパスで検証と操作の対象が乖離する。

**修正:** `check_path()` の戻り値を `Result<PathBuf, TsumugiError>` に変更。全ファイル操作関数が返された正規化済みパスを使用する。検証対象 = 操作対象 を保証。

#### リグレッションテスト

- `tests/fixtures/scope_isolation.tsg` — レキシカルスコープ（呼び出し元の変数非参照）、locals_cells リーク防止、ループ後のスロット再利用
