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

現在は**教育・実験用途のalpha版**であり、言語仕様・組み込みAPI・CLIの後方互換性は保証していない。crate root には、デフォルトのツリーウォークを利用する最小の埋め込み facade（`Engine`、`CompiledScript`、`ExecutionContext`、`ExecutionOutcome`）がある。これはCLIも利用する入口だが、stable Engine APIからのVM利用、host I/Oの注入、deny-by-default capability、包括的な実行予算、監査 event は未実装である。`--vm`は処理系比較のための実験的backendで、同一スコープでの`let`再宣言、捕捉のない関数値の同一性、error種別・メッセージ、未捕捉エラー後のREPL状態にデフォルト実行系との既知の差が残る。現在利用できる観測仕様は[言語仕様](docs/language-spec.md)、現行アーキテクチャは[設計ドキュメント](docs/design.md)を正本とする。Phase 0〜7の次期実装契約は後述の7設計正本で確定済みだが、設計確定は実装完了を意味しない。実装差の一覧と進捗は[ロードマップ](docs/roadmap.md)を参照すること。

組み込みのステップ上限やfilesystem制限はdefense-in-depthであり、非信頼コードを隔離するsecurity sandboxではない。また、現行CLIは`--help` / `--version`とスクリプトへの追加引数に未対応である。Cargo package / REPLの`0.1.0`と[言語仕様](docs/language-spec.md)の`0.12`は、それぞれ実装版と仕様revisionを表す独立した番号として管理している。

## 組み込み API（tree-walk）

`Engine::compile` はソースをパースして再利用可能な `CompiledScript` を返し、`Engine::execute` は `ExecutionContext` 上で実行する。構文エラーは `Vec<TsumugiError>`、実行時エラーは単一の `TsumugiError` として区別される。import は `ExecutionContext` の状態とスクリプトパスに依存するためcompile時には解決せず、`Engine::execute`開始時に最初のscript文を実行する前に全件を解決する。これは現行APIの説明であり、次期APIのcompile/link分離は[組み込みAPI仕様](docs/embedding-api.md)を正本とする。

```rust
use tsumugi::{Engine, ExecutionContext, ExecutionOutcome};

fn main() {
    let engine = Engine::new();
    let script = engine
        .compile("let greeting = \"hello\"\nprint(greeting)\n")
        .expect("valid script");
    let mut context = ExecutionContext::new();

    assert_eq!(
        engine.execute(&script, &mut context).expect("runtime success"),
        ExecutionOutcome::Completed,
    );
}
```

同じ context を再利用すると、変数・関数・解決済みimportが実行間で維持される。相対importを使うファイルは実行前に `context.set_script_path("/absolute/path/to/rules.tsg")` を呼ぶ。REPL相当の入力単位ごとにステップ予算を独立させる場合は `context.reset_step_budget()` を呼ぶ。

このAPIは caller のスレッド上で同期的にツリーウォーク評価を行う。`ExecutionContext` は別スレッドへ移動できないため、埋め込み先は十分なスタックを持つ同一スレッド内で context を生成・利用する（CLIは8 MiBの実行スレッドを使う）。`input` / `print` / `args` / `exit` はまだprocess-globalな既存挙動のままであり、特に `exit()` はホストプロセスを終了し得る。敵対的コードの隔離やホストI/Oの安全な注入には使わない。

## クイックスタート

```bash
# ビルド
cargo build

# ファイル実行
cargo run -- examples/hello.tsg

# バイトコードVM モードで実行
cargo run -- --vm examples/hello.tsg

# import のデモ（examples/math.tsg を読み込む）
cargo run -- examples/import_demo.tsg

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

`main` への push と PR で GitHub Actions が3つのジョブを並行実行する。設定は `.github/workflows/ci.yml`。

| ジョブ | 実行環境 | 内容 |
|---|---|---|
| `lint` | ubuntu-latest | `cargo fmt --check` と `cargo clippy -- -D warnings` |
| `test` | ubuntu-latest / macos-latest / windows-latest | `cargo test`（`fail-fast: false` で全OSの結果を得る） |
| `coverage` | ubuntu-latest | `cargo llvm-cov` で `lcov.info` を生成し artifact `lcov-report` として保存 |

ローカルで同じ検査をする場合は次を順に実行する。

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

Windows固有の挙動（`TSUMUGI_*` 環境変数のcase-insensitive保護など）は `test` ジョブのwindows-latestでのみ検証される。CIの `clippy` はデフォルトターゲットだけを見るため、テストやベンチも含めて検査する場合は `cargo clippy --all-targets -- -D warnings` をローカルで実行する。

## プロジェクト構成

```
src/
├── main.rs       # CLIエントリポイント（REPL / ファイル実行 / --vm 切り替え）
├── lib.rs        # library crateのルート（埋め込みAPIと各モジュールを公開）
├── engine.rs     # tree-walk向け埋め込み facade（compile / execute / context）
│
│  --- フロントエンド（両実行系で共有） ---
├── token.rs      # トークン型定義（Spanned: Token + 行番号）
├── lexer.rs      # レキサー（ソース → トークン列、行番号追跡）
├── ast.rs        # AST ノード定義（行番号保持・深度検査・参照名収集）
├── parser.rs     # パーサー（トークン列 → AST、エラー回復と行番号付与）
├── module.rs     # ModuleLoader（import を実行前に解決してリンク）
├── value.rs      # 実行時の値型
├── error.rs      # エラー型（TsumugiError: Parse / Runtime、ErrorKind 18種）
│
│  --- ツリーウォーク実行（デフォルト） ---
├── env.rs        # 環境（変数スコープとcall frame。関数も変数として保持）
├── eval.rs       # 評価器（AST → 実行）
├── builtin.rs    # コンテキスト依存の組み込み関数（ツリーウォーク版）
│
│  --- バイトコードVM実行（--vm） ---
├── opcode.rs     # バイトコード命令セット
├── chunk.rs      # 命令列 + 定数テーブル
├── compiler.rs   # コンパイラ（AST → Chunk、builtin判定はregistryから導出）
├── vm.rs         # スタックマシン VM + コンテキスト依存の組み込み関数
│
│  --- 実行時ガードレール（両実行系で共有） ---
├── builtin_core.rs     # 組み込み関数の共通実装（47個）
├── builtin_registry.rs # builtin名・arity・context/pure分類の単一正本（BuiltinSpec）
├── limits.rs           # 構造的上限（MAX_AST_DEPTH / MAX_IMPORT_DEPTH）
└── sandbox.rs          # ファイルI/Oと環境変数のallow-list検査

tests/
├── integration.rs    # 統合テスト（ゴールデンテスト + stdin駆動REPL）
├── scaling.rs        # 計算量オーダーと解放漏れの回帰ゲート
├── defensive_vm.rs   # 不正な Chunk を公開APIへ渡してもホストが落ちないこと
└── fixtures/         # テスト用 .tsg + 期待出力ファイル

benches/
└── interpreter.rs    # parse / compile / execute / end_to_end のフェーズ別計測

docs/
├── manifesto.md                     # プロジェクトの価値基準・設計原則・非目標
├── design.md                        # 現行実装の設計正本
├── language-spec.md                 # 現行実装の観測仕様（規範）
├── threat-model.md                  # Phase 0: 保証境界・責任分界
├── embedding-api.md                 # Phase 1/2: 組み込みAPI・terminal channel
├── capability-model.md              # Phase 2: deny-by-default認可
├── execution-control.md             # Phase 3/4: 実行予算・協調実行
├── determinism-and-audit.md          # Phase 5/6: 決定性・実行時監査
├── semantic-decisions.md             # 横断: 次期revisionの意味論・CLI決定
├── verification-release-operations.md # Phase 7: 検証・リリース・運用gate
└── roadmap.md                        # 設計trace・実装状態・実現順序

examples/
├── hello.tsg         # 基本構文（変数・関数・条件分岐・ループ）
├── math.tsg          # import される数学ユーティリティモジュール
└── import_demo.tsg   # math.tsg を import して使うデモ

LANG_GUIDE.md         # AIコード支援向けの言語ガイド（形式文法を含む）

.github/workflows/
└── ci.yml            # CI 設定（lint / test 3 OS / coverage）
```

実行予算とcapabilityのガードレールは `limits.rs`（コンパイル時定数）と `sandbox.rs`（環境変数ベースのallow-list）に集約している。コールフレーム深度上限は `limits.rs` の `MAX_USER_CALL_DEPTH` に一本化し、ツリーウォーク版・VM版とも root script frame を数えない active user frame 数で判定する（AUD-050 / AUD-017）。

## ドキュメント

現行利用者が依存できる仕様は[言語仕様](docs/language-spec.md)と[設計ドキュメント](docs/design.md)である。次期実装契約は以下の7設計正本で確定済みだが、[ロードマップ](docs/roadmap.md)の受入gateを満たすまでは未実装または部分実装として扱う。

- [Tsumugi Manifesto](docs/manifesto.md) — プロジェクトの価値基準・設計原則・非目標
- [設計ドキュメント](docs/design.md) — 現行実装のアーキテクチャと実装済み判断
- [言語仕様](docs/language-spec.md) — 現行実装から観測できる文法・意味論の規範
- [脅威モデル](docs/threat-model.md) — Phase 0の保証境界、非保証、責任分界
- [組み込みAPI仕様](docs/embedding-api.md) — Phase 1/2のEngine、compile/link、terminal channel
- [Capability Model仕様](docs/capability-model.md) — Phase 2のdeny-by-default認可とadapter境界
- [実行予算・協調実行仕様](docs/execution-control.md) — Phase 3/4の有限budget、transaction、協調実行
- [決定性・実行時監査仕様](docs/determinism-and-audit.md) — Phase 5/6の規範backend、決定性、audit
- [次期意味論・実装決定](docs/semantic-decisions.md) — 次期revisionの意味論、CLI、canonical error、class/HTTP判断
- [検証・リリース・運用設計](docs/verification-release-operations.md) — Phase 7の検証、配布、supply chain、運用gate
- [ロードマップ](docs/roadmap.md) — 設計正本へのtrace、実装状態、実現順序

## ライセンス

MIT
