# Tsumugi（紡ぎ）

Rustで実装する、制御可能な組み込みスクリプト言語を目指すプロジェクト。

## プロジェクトの方向性

Tsumugiは、プログラミング言語処理系への理解を深めるための個人プロジェクトとして始まった。その学びを継続しながら、実運用を見据えた組み込みスクリプト言語の研究・開発へ進む。

> Tsumugiは、サーバーアプリケーションや業務システムに組み込み、ホストが明示的に付与した権限と実行予算の範囲内で、業務ルールや拡張ロジックを予測可能かつ監査可能に実行するためのスクリプト言語を目指す。

瞬間的な実行速度よりもホストの安定性を優先し、制御された負荷の中で処理を着実に進める。設計上の原則と非目標は[Tsumugi Manifesto](docs/manifesto.md)を参照すること。

> [!IMPORTANT]
> マニフェストはTsumugiが目指す方向を示すものであり、現在の実装がすべての性質を保証していることを意味しない。

## 概要

現在のTsumugiはRuby風の文法を持つ動的型付け言語である。Lexer・Parser・ASTを共有し、デフォルトのツリーウォーク評価器、またはバイトコードコンパイラ + スタックVM（`--vm`）で実行する。

現在は**教育・実験用途のalpha版**であり、言語仕様・組み込みAPI・CLIの後方互換性は保証していない。安定した組み込みAPI、実行単位のdeny-by-default capability、包括的な実行予算、実行時audit eventは今後の設計・実装対象である。`--vm`は処理系比較のための実験的backendで、比較、index代入、builtin、importなど一部の境界動作はデフォルト実行系と一致していない。現在地と修正状況は[ロードマップ](docs/roadmap.md)を参照すること。

組み込みのステップ上限やfilesystem制限はdefense-in-depthであり、非信頼コードを隔離するsecurity sandboxではない。また、現行CLIは`--help` / `--version`とスクリプトへの追加引数に未対応である。Cargo package / REPLの`0.1.0`と[言語仕様](docs/language-spec.md)の`0.10`は、それぞれ実装版と仕様revisionを表す独立した番号として管理している。

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

# 資源の限られた環境でbuild・testの並列度を抑える
cargo test -j 1 -- --test-threads=1

# 特定モジュールのみ
cargo test lexer::
cargo test eval::
cargo test parser::
```

低並列実行は瞬間的なCPU・メモリ負荷を抑えるが、hard limitを設定するものではない。必要に応じてcgroupやコンテナの資源制限と組み合わせる。

### テスト構成

| 種別 | 場所 | 内容 |
|---|---|---|
| ユニットテスト | `src/*.rs` 内 `#[cfg(test)]` | 各モジュールの個別ロジック検証 |
| 統合テスト | `tests/integration.rs` | ファイル実行のゴールデンテスト + stdin駆動REPLの状態回復・資源上限テスト |
| スケーリングテスト | `tests/scaling.rs` | 確保バイト数と生存バイト数で計算量オーダーと解放漏れを固定 |
| 防御的テスト | `tests/defensive_vm.rs` | 不正な `Chunk` を公開APIへ渡してもホストを落とさないことを検証 |
| テストデータ | `tests/fixtures/` | 正常系 `.tsg` + `.expected` / エラー系 `.tsg` + `.expected_err` |

fixture は `fixture_tests!` へ1行宣言するとツリーウォーク版 / VM版の両方のテストが生成される。判定は完全一致で、子プロセスは制限時間付きで実行する。詳細は `docs/design.md` の「統合テスト」を参照。

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
├── scaling.rs        # 計算量オーダーの回帰テスト
└── fixtures/         # テスト用 .tsg + 期待出力ファイル

benches/
└── interpreter.rs    # parse / compile / execute / end_to_end のフェーズ別計測

docs/
├── manifesto.md      # プロジェクトの方向性・設計原則・非目標
├── design.md         # 設計ドキュメント
├── language-spec.md  # 言語仕様
└── roadmap.md        # ロードマップ・設計方針

examples/
└── hello.tsg         # サンプルスクリプト

.github/workflows/
└── ci.yml            # CI 設定
```

## ドキュメント

- [Tsumugi Manifesto](docs/manifesto.md) — プロジェクトの方向性・設計原則・非目標
- [設計ドキュメント](docs/design.md) — 設計判断の経緯・アーキテクチャ・今後の候補
- [言語仕様](docs/language-spec.md) — 文法・データ型・演算子の一覧
- [ロードマップ](docs/roadmap.md) — 実装済み機能・設計方針・今後の検討事項

## ライセンス

MIT
