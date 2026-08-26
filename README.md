# Tsumugi（紡ぎ）

Rust で作るミニプログラミング言語。言語処理系の学習を目的としたプロジェクト。

## 概要

Tsumugi は Ruby 風の文法を持つ動的型付けインタプリタ言語。
ソースコードを レキサー → パーサー → 評価器 の3段パイプラインで実行する。
バイトコードコンパイラ + スタックVM による実行モード（`--vm`）も備えている。

## クイックスタート

```bash
# ビルド
cargo build

# ファイル実行
cargo run -- examples/hello.tsg

# バイトコードVM モードで実行
cargo run -- --vm examples/hello.tsg

# REPL（対話モード）
cargo run
```

## サンプルコード

```
let name = "tsumugi"
print("hello, " + name)

# f-string（文字列補間）
let version = 1
print(f"running {name} v{version}")

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

# クロージャ
fn make_adder(n)
    return fn(x) return x + n end
end
let add5 = make_adder(5)
print(add5(3))    # 8

# 無名関数を引数に渡す
fn apply(func, val)
    return func(val)
end
print(apply(fn(x) x * x end, 6))  # 36

# モジュール import
import "math.tsg"
print(square(5))    # 25
```

## 対応機能

- 整数・浮動小数点・文字列・真偽値・null
- f-string（文字列補間: `f"hello, {expr}"`）
- リスト (`[1, 2, 3]`) と辞書 (`{"key": value}`)
- 変数束縛 (`let`) と再代入 (`x = expr`)
- インデックスアクセス (`xs[0]`, `d["key"]`) と代入 (`xs[0] = val`)
- 四則演算・剰余演算・文字列結合
- 比較演算 (`==`, `!=`, `<`, `>`, `<=`, `>=`)
- 論理演算 (`and`, `or`, `not`)
- 条件分岐 (`if` / `elif` / `else` / `end`)
- 関数定義・呼び出し (`fn` / `return` / `end`)
- 第一級関数（関数を変数に代入、引数として渡す）
- 無名関数 / ラムダ (`fn(x) x * 2 end`)
- クロージャ（外側の変数をキャプチャ）
- while ループ
- for ループ (`for item in collection ... end`)
- break / continue
- モジュール / import (`import "path.tsg"`)
- 組み込み関数 53個（文字列操作、リスト操作、高階関数、ファイルI/O、パス操作、環境変数、日時など）
- REPL（複数行入力対応）
- ファイル実行
- 行番号付きエラーメッセージ（パースエラー・ランタイムエラー両方）
- パースエラーの回復（複数のエラーを一度にまとめて報告）
- スタックトレース（関数呼び出し経路の表示）
- ステップ予算（無限ループ・無限再帰の強制停止、環境変数 `TSUMUGI_MAX_STEPS` で上限変更可能）
- コレクションサイズ上限（List/Dictの生成・拡張を制限、環境変数 `TSUMUGI_MAX_COLLECTION_SIZE` で変更可能）
- ファイルI/Oサンドボックス（環境変数 `TSUMUGI_SANDBOX` でアクセス許可パスを制限、import含む全ファイル操作が対象）
- 環境変数アクセス制御（環境変数 `TSUMUGI_ENV_ALLOW` で読み取り可能なキーを許可リスト制限）

## エラーメッセージ

エラー発生時に行番号が表示される。関数内のエラーにはスタックトレースが付加される。
構文エラーが複数ある場合は、1回の実行でまとめて報告される。

```
$ cat error.tsg
let x = "hello"
let y = x + 1

$ tsumugi error.tsg
2行目: 型エラー: Str("hello") Add Int(1) は計算できません
```

複数パースエラーの報告例:

```
$ cat multi_error.tsg
let = oops
let y = 1
let = bad

$ tsumugi multi_error.tsg
1行目: let の後に識別子が必要です。got: Assign
3行目: let の後に識別子が必要です。got: Assign
```

スタックトレース例:

```
$ cat trace.tsg
fn divide(a, b)
    return a / b
end

fn calc(x)
    return divide(x, 0)
end

calc(10)

$ tsumugi trace.tsg
2行目: ゼロ除算
  in divide() (6行目)
  in calc() (9行目)
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
| 統合テスト | `tests/integration.rs` | ファイル実行のゴールデンテスト + stdin駆動REPLの状態回復・資源上限テスト |
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
├── main.rs       # エントリポイント（REPL / ファイル実行 / --vm 切り替え）
├── token.rs      # トークン型定義（Spanned: Token + 行番号）
├── lexer.rs      # レキサー（ソース → トークン列、行番号追跡）
├── ast.rs        # AST ノード定義（各 Stmt に行番号を保持）
├── parser.rs     # パーサー（トークン列 → AST、エラーに行番号付与）
├── value.rs      # 実行時の値型
├── error.rs      # エラー型（TsumugiError: Parse / Runtime）
│
│  --- ツリーウォーク実行（デフォルト） ---
├── env.rs        # 環境（変数スコープ・関数テーブル）
├── eval.rs       # 評価器（AST → 実行）
├── builtin.rs    # 組み込み関数（ツリーウォーク版）
│
│  --- バイトコードVM実行（--vm） ---
├── opcode.rs     # バイトコード命令セット
├── chunk.rs      # 命令列 + 定数テーブル
├── compiler.rs   # コンパイラ（AST → Chunk）
└── vm.rs         # スタックマシン VM + 組み込み関数

tests/
├── integration.rs    # 統合テスト（ゴールデンテスト）
└── fixtures/         # テスト用 .tsg + 期待出力ファイル

docs/
├── design.md         # 設計ドキュメント
├── language-spec.md  # 言語仕様
└── roadmap.md        # ロードマップ・設計方針

examples/
└── hello.tsg         # サンプルスクリプト

.github/workflows/
└── ci.yml            # CI 設定
```

## ドキュメント

- [設計ドキュメント](docs/design.md) — 設計判断の経緯・アーキテクチャ・今後の候補
- [言語仕様](docs/language-spec.md) — 文法・データ型・演算子の一覧
- [ロードマップ](docs/roadmap.md) — 実装済み機能・設計方針・今後の検討事項

## ライセンス

MIT
