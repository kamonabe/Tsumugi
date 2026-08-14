# Tsumugi — 設計ドキュメント

最終更新: 2026-08-14

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
- バイトコード VM は将来の発展形として残す

### 関数の戻り値: 明示的 return

- Ruby 風の「最後の式が暗黙の戻り値」も検討したが、読む側が戻り値の意図を把握しやすい明示的 return を採用

### 論理演算: and / or / not

- `&&` / `||` / `!` より英単語の方が直感的で覚えやすい

### null 表現: null

- nil (Ruby/Lua) や None (Python) ではなく、最も馴染みのある `null` を採用
- 「余計な用語を増やさない」方針

### エラーメッセージ: 行番号付き

- `Spanned` 構造体（`Token` + `line: usize`）でレキサーが行番号を追跡
- AST の各 `Stmt` バリアントにも `line` フィールドを保持
- パースエラー・ランタイムエラーの両方で「N行目: ...」形式のメッセージを表示
- REPL では入力ごとに1行目からカウント（ファイル実行時はファイル先頭から通し番号）

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

将来的にクロージャ/高階関数を実装した際に `filter()` を追加すれば、より簡潔に書けるようになる。

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

### パーサー (parser.rs)

- 再帰下降構文解析
- `Vec<Spanned>` を入力として受け取る
- 演算子の優先順位（低→高）: or → and → 比較 → 加減 → 乗除 → 単項 → 関数呼び出し
- ブロックは終端トークン（`end`, `else`）を目印に解析
- エラー発生時に `Spanned.line` を参照して「N行目: ...」メッセージを生成

### 評価器 (eval.rs)

- AST を再帰的にたどって Value を返す
- 環境（Env）で変数スコープをスタック管理
- 関数呼び出し時に新しいスコープを push、終了時に pop
- `return` 文は EvalResult::Return で早期脱出を表現
- ランタイムエラー（型エラー、未定義変数、ゼロ除算等）に `Stmt.line` を付与
- 組み込み関数の実装は `builtin.rs` に分離（eval.rs は制御フロー・式評価に専念）

### 環境 (env.rs)

- スコープのスタック（Vec<HashMap>）
- 変数検索は内側 → 外側の順
- `set()` は現在のスコープに変数を新規定義
- `update()` は内側→外側へ探索し、既存変数を更新（見つからなければエラー）
- `get_mut()` は可変参照で変数を取得（インデックス代入・push で使用）
- 関数定義はグローバルな HashMap で管理

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

## 文法仕様 (v0.2)

```
program        = stmt*
stmt           = let_stmt | assign_stmt | index_assign | return_stmt
               | if_stmt | while_stmt | for_stmt | break_stmt | continue_stmt
               | fn_def | expr_stmt
let_stmt       = "let" IDENT "=" expr NEWLINE
assign_stmt    = IDENT "=" expr NEWLINE
index_assign   = postfix "[" expr "]" "=" expr NEWLINE
return_stmt    = "return" expr NEWLINE
if_stmt        = "if" expr NEWLINE block ("elif" expr NEWLINE block)* ("else" NEWLINE block)? "end" NEWLINE
while_stmt     = "while" expr NEWLINE block "end" NEWLINE
for_stmt       = "for" IDENT "in" expr NEWLINE block "end" NEWLINE
break_stmt     = "break" NEWLINE
continue_stmt  = "continue" NEWLINE
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
list_literal   = "[" (expr ("," expr)* ","?)? "]"
dict_literal   = "{" (expr ":" expr ("," expr ":" expr)* ","?)? "}"
args           = expr ("," expr)*
```

## 今後の候補

| 優先度 | 項目 |
|---|---|
| 低 | モジュール / import |
| 低 | クロージャ / 高階関数 |
| 低 | クラス（継承なし・合成で拡張） |
| 発展 | バイトコード VM 化 |

## 参考資料

- 「Writing An Interpreter In Go」(Thorsten Ball)
- 「Crafting Interpreters」(Robert Nystrom) — https://craftinginterpreters.com/
- 「低レイヤを知りたい人のためのCコンパイラ作成入門」(Rui Ueyama)
