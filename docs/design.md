# Tsumugi — 設計ドキュメント

最終更新: 2026-08-13

## 目的

言語処理系の開発を「入り口レベル」で体験する。
世界に広める言語を作ることが目的ではなく、レキサー・パーサー・評価器の仕組みを理解すること自体が目標。

## 設計判断の経緯

### 実装言語: Rust

- `enum` + パターンマッチが AST 表現に適している
- exhaustive match によりノード処理漏れをコンパイル時に検出できる
- 所有権モデルを通じて「なぜGCが必要か」を体感できる
- Python でプロトタイプしてから Rust で再実装する案もあったが、最初から Rust で進めることにした

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
- 型: Int, Float, Str, Bool, Null

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

## アーキテクチャ

```
ソースコード (.tsg ファイル or REPL 入力)
    │
    ▼
┌──────────┐
│  Lexer   │  文字列 → トークン列
└──────────┘
    │
    ▼
┌──────────┐
│  Parser  │  トークン列 → AST
└──────────┘
    │
    ▼
┌──────────┐
│Evaluator │  AST → 実行
└──────────┘
    │
    ▼
  標準出力
```

### レキサー (lexer.rs)

- 1文字ずつ先読みしてトークンを生成
- 改行を `Newline` トークンとして保持（文の区切りとして使う）
- スペース/タブはスキップ、`#` 以降はコメントとしてスキップ
- 2文字演算子（`==`, `!=`, `<=`, `>=`）は先読みで判定

### パーサー (parser.rs)

- 再帰下降構文解析
- 演算子の優先順位（低→高）: or → and → 比較 → 加減 → 乗除 → 単項 → 関数呼び出し
- ブロックは終端トークン（`end`, `else`）を目印に解析

### 評価器 (eval.rs)

- AST を再帰的にたどって Value を返す
- 環境（Env）で変数スコープをスタック管理
- 関数呼び出し時に新しいスコープを push、終了時に pop
- `return` 文は EvalResult::Return で早期脱出を表現

### 環境 (env.rs)

- スコープのスタック（Vec<HashMap>）
- 変数検索は内側 → 外側の順
- 関数定義はグローバルな HashMap で管理

## 文法仕様 (v0.1)

```
program     = stmt*
stmt        = let_stmt | return_stmt | if_stmt | while_stmt | fn_def | expr_stmt
let_stmt    = "let" IDENT "=" expr NEWLINE
return_stmt = "return" expr NEWLINE
if_stmt     = "if" expr NEWLINE block ("else" NEWLINE block)? "end" NEWLINE
while_stmt  = "while" expr NEWLINE block "end" NEWLINE
fn_def      = "fn" IDENT "(" params? ")" NEWLINE block "end" NEWLINE
expr_stmt   = expr NEWLINE
block       = stmt*
params      = IDENT ("," IDENT)*
expr        = or_expr
or_expr     = and_expr ("or" and_expr)*
and_expr    = cmp_expr ("and" cmp_expr)*
cmp_expr    = add_expr (("==" | "!=" | "<" | ">" | "<=" | ">=") add_expr)*
add_expr    = mul_expr (("+" | "-") mul_expr)*
mul_expr    = unary_expr (("*" | "/") unary_expr)*
unary_expr  = ("not" | "-") unary_expr | call_expr
call_expr   = primary ("(" args? ")")?
primary     = INT | FLOAT | STRING | "true" | "false" | "null" | IDENT | "(" expr ")"
args        = expr ("," expr)*
```

## 今後の候補

| 優先度 | 項目 |
|---|---|
| 高 | エラーメッセージに行番号を付与 |
| 中 | 配列リテラル・インデックスアクセス |
| 中 | 組み込み関数の追加（len, type, to_string 等） |
| 低 | ハッシュマップ |
| 低 | for ループ（配列のイテレーション） |
| 低 | モジュール / import |
| 発展 | バイトコード VM 化 |

## 参考資料

- 「Writing An Interpreter In Go」(Thorsten Ball)
- 「Crafting Interpreters」(Robert Nystrom) — https://craftinginterpreters.com/
- 「低レイヤを知りたい人のためのCコンパイラ作成入門」(Rui Ueyama)
