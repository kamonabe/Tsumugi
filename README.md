# Tsumugi（紡ぎ）

Rust で作るミニプログラミング言語。言語処理系の学習を目的としたプロジェクト。

## 概要

Tsumugi は Ruby 風の文法を持つ動的型付けインタプリタ言語。
ソースコードを レキサー → パーサー → 評価器 の3段パイプラインで実行する。

## クイックスタート

```bash
# ビルド
cargo build

# ファイル実行
cargo run -- examples/hello.tsg

# REPL（対話モード）
cargo run
```

## サンプルコード

```
let name = "tsumugi"
print("hello, " + name)

fn add(a, b)
    return a + b
end

print(add(3, 4))

let count = 3
while count > 0
    print(count)
    count = count - 1
end

# リスト
let fruits = ["apple", "banana", "cherry"]
print(fruits[0])
push(fruits, "date")
print(len(fruits))

# 辞書
let config = {"host": "localhost", "port": 8080}
print(config["host"])
config["debug"] = true
print(keys(config))
```

## 対応機能

- 整数・浮動小数点・文字列・真偽値・null
- リスト (`[1, 2, 3]`) と辞書 (`{"key": value}`)
- 変数束縛 (`let`) と再代入 (`x = expr`)
- インデックスアクセス (`xs[0]`, `d["key"]`) と代入 (`xs[0] = val`)
- 四則演算・剰余演算・文字列結合
- 比較演算 (`==`, `!=`, `<`, `>`, `<=`, `>=`)
- 論理演算 (`and`, `or`, `not`)
- 条件分岐 (`if` / `elif` / `else` / `end`)
- 関数定義・呼び出し (`fn` / `return` / `end`)
- while ループ
- for ループ (`for item in collection ... end`)
- break / continue
- 組み込み関数 44個（文字列操作、リスト操作、ファイルI/O、パス操作、環境変数、日時など）
- REPL（複数行入力対応）
- ファイル実行
- 行番号付きエラーメッセージ（パースエラー・ランタイムエラー両方）

## エラーメッセージ

エラー発生時に行番号が表示される。

```
$ cat error.tsg
let x = "hello"
let y = x + 1

$ tsumugi error.tsg
2行目: 型エラー: Str("hello") Add Int(1) は計算できません
```

## テスト

```bash
# 全テスト実行（ユニットテスト + 統合テスト）
cargo test

# 特定モジュールのみ
cargo test lexer::
cargo test eval::
cargo test parser::
```

### テスト構成

| 種別 | 場所 | 内容 |
|---|---|---|
| ユニットテスト | `src/*.rs` 内 `#[cfg(test)]` | 各モジュールの個別ロジック検証 |
| 統合テスト | `tests/integration.rs` | `.tsg` ファイル実行 → 期待出力と比較（ゴールデンテスト） |
| テストデータ | `tests/fixtures/` | 正常系 `.tsg` + `.expected` / エラー系 `.tsg` + `.expected_err` |

## CI

GitHub Actions で push / PR 時に自動実行。

```
cargo fmt --check → cargo clippy -D warnings → cargo test
```

設定: `.github/workflows/ci.yml`

## プロジェクト構成

```
src/
├── main.rs     # エントリポイント（REPL / ファイル実行）
├── token.rs    # トークン型定義（Spanned: Token + 行番号）
├── lexer.rs    # レキサー（ソース → トークン列、行番号追跡）
├── ast.rs      # AST ノード定義（各 Stmt に行番号を保持）
├── parser.rs   # パーサー（トークン列 → AST、エラーに行番号付与）
├── value.rs    # 実行時の値型
├── env.rs      # 環境（変数スコープ・関数テーブル）
└── eval.rs     # 評価器（AST → 実行、エラーに行番号付与）

tests/
├── integration.rs    # 統合テスト（ゴールデンテスト）
└── fixtures/         # テスト用 .tsg + 期待出力ファイル

docs/
├── design.md         # 設計ドキュメント
└── language-spec.md  # 言語仕様

examples/
└── hello.tsg         # サンプルスクリプト

.github/workflows/
└── ci.yml            # CI 設定
```

## ドキュメント

- [設計ドキュメント](docs/design.md) — 設計判断の経緯・アーキテクチャ・今後の候補
- [言語仕様](docs/language-spec.md) — 文法・データ型・演算子の一覧

## ライセンス

MIT
