# Tsumugi — 設計ドキュメント

最終更新: 2026-08-26

## 目的

言語処理系の開発を「入り口レベル」で体験する。
世界に広める言語を作ることが目的ではなく、レキサー・パーサー・評価器の仕組みを理解すること自体が目標。

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
- インデックス代入は `Stmt::IndexAssign` で表現。`Env::get_mut()` で変数を可変参照で取得し直接更新する方式
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
- ブロックは終端トークン（`end`, `else`）を目印に解析
- エラー発生時に `Spanned.line` を参照して「N行目: ...」メッセージを生成
- エラー回復: 1つ目のエラーで停止せず、複数のパースエラーをまとめて報告する
  - `parse()` の戻り型は `Result<Program, Vec<TsumugiError>>`
  - 文のパースに失敗したら `synchronize()` で次の文境界までスキップし、パースを継続する
  - リカバリポイント: 改行 / EOF / 文の先頭キーワード（`let`, `fn`, `if`, `while`, `for`, `return`, `import`, `try`, `break`, `continue`）

### 評価器 (eval.rs)

- AST を再帰的にたどって Value を返す
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
- `get_mut()` は可変参照で変数を取得（インデックス代入・push で使用）
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

## 文法仕様 (v0.3)

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

**VM版:**
- `CallFrame` に `locals_cells: Vec<Option<SharedValue>>` を追加
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

- `PartialEq` / `Debug` は手動実装に移行。関数値の等価性は常に `false`（同じ変数に束縛されていても `f == f` は `false`）。多くの動的型付け言語（JavaScript, Python, Ruby）が関数の構造的等価性を定義しないのと同じ方針
- 循環参照: `Rc<RefCell>` のため理論的にはメモリリークの可能性があるが、Tsumugi には循環データ構造を作る手段がないため実質問題にならない
- パフォーマンス: `RefCell` の実行時借用チェックのオーバーヘッドは存在するが、学習用途では問題にならない水準

## モジュール / import の設計

### 概要

`import "path.tsg"` 構文で別ファイルの関数・変数を現在のスコープに取り込む。
ツリーウォーク版・VM版の両方で動作する。

### 設計判断

#### import をトップレベルに限定

`import` はプログラムのトップレベルでのみ有効とし、関数・条件分岐・ループ・`try` / `catch`・複数行ラムダのブロック内ではパースエラーにする。

ツリーウォーク版は import 文へ到達した実行時にファイルを読み込む一方、VM版はコンパイル時にASTをインライン展開する。非トップレベルを許可すると、false branchでの読込有無、loopでの実行回数、関数呼び出し時の読込、相対パス基準、エラー発生フェーズを同一にできない。VMへruntime import命令を追加する大規模な再設計や、tree版をcompile-time展開へ変更する意味論変更は行わず、非本質的な配置を構文上禁止して両engineを統一する。

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
- これにより A→B→A のような循環参照が安全に処理される
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

ツリーウォークインタプリタに加え、バイトコードコンパイラ + スタックVM を並行実装する。
`--vm` フラグで実行方式を切り替え可能。既存のテスト資産をそのまま回帰テストに使い、両方式で同じ言語仕様を満たすことを保証する。

### 動機

- ツリーウォークは「AST → 再帰で直接実行」。理解しやすいが、関数呼び出しオーバーヘッドが大きい
- バイトコードVMは「AST → 一次元の命令列に変換 → ループで1命令ずつ実行」。CPUの分岐予測と相性が良く高速
- 言語処理系の学習として、コード生成とVM実行ループは別の知見が得られる

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
- **引数は評価済み `&[Value]`** — 引数の評価方法がエンジンで異なるため（ツリーウォーク: `&[Expr]` を `eval_expr` で評価、VM: スタックから pop 済み）、共通モジュールは評価済みの値だけ受け取る
- **エンジン固有のビルトインは各モジュールに残す** — `push`/`pop`（ツリーウォークは変数を直接変更）、`map`/`filter`/`each`（クロージャ呼び出しがエンジン依存）、`print`/`input`/`exit`/`args`（I/O・プロセス操作）
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

信頼できないコードを安全に実行するためのサンドボックス機構。
全て環境変数で制御し、未設定時は制限なし（開発時のUXを阻害しない）。

### 制御機構一覧

| 環境変数 | 対象 | 動作 |
|---|---|---|
| `TSUMUGI_MAX_STEPS` | ループ反復 + 関数呼び出し | 上限超過でランタイムエラー |
| `TSUMUGI_MAX_COLLECTION_SIZE` | List/Dictの生成・拡張とList生成builtin | 要素数上限超過でランタイムエラー |
| `TSUMUGI_SANDBOX` | 全ファイル操作（read/write/import） | 許可パス外へのアクセスをブロック |
| `TSUMUGI_ENV_ALLOW` | `env()` 関数 | 許可リスト外のキーは null を返す |

### コールフレーム深度制限

定数 `MAX_CALL_DEPTH = 128` で関数呼び出しのネスト深度を制限する。超過すると「スタックオーバーフロー: 再帰が深すぎます」ランタイムエラー。

- ツリーウォーク版: `call_stack.len()` でチェック
- VM版: `frames.len()` でチェック
- main() は 8MB スタックのスレッドで実行するため、Windowsのデフォルトスタック(1MB)制限を回避済み

### TSUMUGI_* 環境変数の保護

`env()` 組み込み関数は `TSUMUGI_` プレフィックスで始まるキーへのアクセスを常に `null` で返す。`TSUMUGI_ENV_ALLOW` 許可リストの設定に関わらず適用される。

理由: `TSUMUGI_SANDBOX`, `TSUMUGI_MAX_STEPS`, `TSUMUGI_ENV_ALLOW` 等のランタイム制御用環境変数の値がスクリプトに漏洩すると、サンドボックスの許可パスリストや実行上限が攻撃者に見えてしまう。

### map/filter/each のステップカウント

VM版の `call_fn_value()`（map/filter/each のコールバック呼び出し）にもステップカウントを適用。巨大リストに対するコールバックでステップ予算をバイパスされる問題を防止。

REPLではtree版・VM版とも入力開始時にステップカウンタを0へ戻す。importは同じ評価呼び出し内で実行するため、1入力から到達したimport・関数・callbackは予算を共有する。

### REPL入力の状態transaction

REPLの未捕捉エラーは、外部I/Oを含む完全なACID transactionではない。ただし、次入力を安全に処理するための内部構造状態は入力開始時点へ戻す。

- Compiler: `locals`、scope/loop、import集合、base directoryをcheckpointし、compile errorまたはVM runtime errorで復元
- VM: value stack、call frame、try handler、step状態をcheckpointし、未捕捉runtime errorで復元
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

### 未対応の既知の制約

- ~~**スタック深度制限**: 深い再帰で Rust の実スタックが溢れるとプロセスごとクラッシュする。ステップ予算では防げない~~ → **解決済み**: `MAX_CALL_DEPTH = 128` + 8MB スタックスレッドで対策
- **シンボリックリンク**: サンドボックス内に外部を指す symlink が存在する場合、新規ファイル作成時に迂回の余地がある
- **総ヒープメモリ制限なし**: List/Dictの要素数上限はあるが、巨大文字列、要素自身のサイズ、全コレクション合計量にはglobal quotaがなくOOMの可能性が残る
- **input() の無制限読み込み**: 改行なしの巨大入力でOOM、入力なしで無限ブロック


## 変更履歴

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
