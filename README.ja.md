[English](README.md) | **日本語**

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

現在は**教育・実験用途のalpha版**であり、言語仕様・組み込みAPI・CLIの後方互換性は保証していない。Phase 0〜7の次期実装契約は後述の7設計正本で確定済みだが、設計確定は実装完了を意味しない。実装状況の概略は次のとおりで、各項目の正確な状態は[ロードマップ](docs/roadmap.md)の「ステータス一覧」を唯一の正本とする。

- **CLI:** capability profile（既定 `safe` / 互換 `legacy`）と `--allow-*` / `--fs-*` / `--allow-import-root` などのオプションを持つ。`safe` 既定では標準出力以外の外部効果（ファイル操作・環境変数・`now()`・`input()`・`exit()`・import）をすべて拒否し、必要なものだけをオプションで付与する。
- **組み込みAPI:** 2系統が併存する。CLIの入口でもある最小の alpha facade（`Engine`、`CompiledScript`、`ExecutionContext`、`ExecutionOutcome`）と、Phase 1/2 の組み込みAPI（`compile`→`link`→`run`。統合（E10）までは crate root で `EmbeddingEngine` / `EmbeddingContext` などの別名で公開）である。後者は capability set・host function registry・実行予算・audit sink を構成でき、`VmExperimental` backend も明示 opt-in で選べる。
- **capability（Phase 2）:** deny-by-default の `CapabilitySet`（filesystem root・環境変数 snapshot・clock・stdin/stdout・process exit・module resolver）と host function registry を実装済み。Phase 2 の受入基準は一部未完了。
- **実行予算・協調実行（Phase 3/4）:** fuel・collection・文字列・source/import 本数などの包括 budget と deadline・cancel は tree/VM の両方で動く。yield/resume・協調スケジューラ・cooperative host call は部分実装。
- **実行時監査（Phase 6）:** 組み込みAPIで audit sink を設定した場合だけ（opt-in）、schema v1 の audit event を出す。対応は tree backend のみで、VM backend との併用は構成時に拒否される。CLI は audit を出さない。

`--vm`は処理系比較のための実験的backendで、未捕捉エラー後のREPL状態にデフォルト実行系との既知の差が残る。現在利用できる観測仕様は[言語仕様](docs/language-spec.md)、現行アーキテクチャは[設計ドキュメント](docs/design.md)を正本とする。

組み込みのステップ上限やcapabilityによる制限はdefense-in-depthであり、非信頼コードを隔離するsecurity sandboxではない。CLIは`tsumugi [オプション] [SCRIPT [ARGS...]]`の形でスクリプト引数を受け取り、`args()`から参照できる（`SCRIPT`が`-`なら標準入力からソースを読む。AUD-018 E8a）。オプションの一覧は `tsumugi --help` で確認できる。Cargo package / REPLの`0.1.0`と[言語仕様](docs/language-spec.md)の`0.21`は、それぞれ実装版と仕様revisionを表す独立した番号として管理している。

## 組み込み API（alpha facade）

`Engine::compile` はソースをパースして再利用可能な `CompiledScript` を返し、`Engine::execute` は `ExecutionContext` 上で実行する。構文エラーは `Vec<TsumugiError>`、実行時エラーは単一の `TsumugiError` として区別される。import は `ExecutionContext` の状態とスクリプトパスに依存するためcompile時には解決せず、`Engine::execute`開始時に最初のscript文を実行する前に全件を解決する。compile/link を分離した Phase 1/2 の組み込みAPI（`EmbeddingEngine` ほか）は[組み込みAPI仕様](docs/embedding-api.md)を正本とする。

`ExecutionContext` の capability set は既定で空（deny-by-default）であり、`print` を含むすべての外部効果が拒否される。スクリプトに許す操作は `context.set_capabilities(...)` で明示的に付与する。

```rust
use std::num::NonZeroU128;
use std::sync::Arc;

use tsumugi::{CapabilitySet, Engine, ExecutionContext, ExecutionOutcome, SystemOutput};

fn main() {
    let engine = Engine::new();
    let script = engine
        .compile("let greeting = \"hello\"\nprint(greeting)\n")
        .expect("valid script");
    let mut context = ExecutionContext::new();

    // print（stdout）だけを許可する。
    let capabilities = CapabilitySet::builder()
        .stdout(Arc::new(SystemOutput::new(NonZeroU128::new(1).unwrap())))
        .expect("stdout grant")
        .build();
    context.set_capabilities(capabilities);

    assert_eq!(
        engine.execute(&script, &mut context).expect("runtime success"),
        ExecutionOutcome::Completed,
    );
}
```

同じ context を再利用すると、変数・関数・解決済みimportが実行間で維持される。相対importを使うファイルは実行前に `context.set_script_path("/absolute/path/to/rules.tsg")` を呼ぶ。REPL相当の入力単位ごとにステップ予算を独立させる場合は `context.reset_step_budget()` を呼ぶ。

このAPIは caller のスレッド上で同期的にツリーウォーク評価を行う。`ExecutionContext` は別スレッドへ移動できないため、埋め込み先は十分なスタックを持つ同一スレッド内で context を生成・利用する（CLIは8 MiBの実行スレッドを使う）。`exit()` はホストプロセスを終了させない。process exit capability を付与した場合も実行が終わるだけで、終了コードは `context.take_pending_exit()` で取り出す。capability はdefense-in-depthであり、敵対的コードの隔離には使わない。

## クイックスタート

```bash
# ビルド
cargo build

# ファイル実行
cargo run -- examples/hello.tsg

# バイトコードVM モードで実行
cargo run -- --vm examples/hello.tsg

# import のデモ（examples/math.tsg を読み込む。import には import root の付与が必要）
cargo run -- --allow-import-root default=examples examples/import_demo.tsg

# 環境変数・時刻などを個別に許可して実行（safe profile 既定では拒否される）
cargo run -- --allow-env HOME --allow-clock script.tsg

# ディレクトリを mount して読み取りだけを許可（スクリプトからは "@data/a.txt" の形で参照する）
cargo run -- --fs-root data=./data --fs-op data=read,list script.tsg

# オプション一覧
cargo run -- --help

# REPL（対話モード）
cargo run
```

`--profile legacy` は旧来の環境変数（`TSUMUGI_SANDBOX` / `TSUMUGI_ENV_ALLOW`）に基づく互換挙動で、未設定時は広い権限を与える旨の警告を出す。新規の利用には safe profile（既定）を使う。

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
- 組み込み関数 54個（文字列操作、リスト操作、高階関数、ファイルI/O、パス操作、環境変数、日時など。一覧は[生成リファレンス](docs/generated/builtins.md)）
- REPL（複数行入力対応）
- ファイル実行
- 行番号付きエラーメッセージ（パースエラー・ランタイムエラー両方）
- パースエラーの回復（複数のエラーを一度にまとめて報告）
- スタックトレース（関数呼び出し経路の表示）
- 実行予算（ステップ・コレクション要素数・文字列・source/import 本数・deadline など。無限ループ・無限再帰も強制停止する）。CLI は既定の上限（ステップ 1,000,000 など）を使う。`TSUMUGI_MAX_STEPS` / `TSUMUGI_MAX_COLLECTION_SIZE` などの `TSUMUGI_MAX_*` 環境変数で上限を変えられるが、非推奨の互換入口であり、設定すると起動時に警告が出る（組み込みAPIはこれらを読まない）
- capability による外部効果の制御（deny-by-default。CLI では `--allow-*` / `--fs-*` / `--allow-import-root` で付与し、組み込みAPIでは `CapabilitySet` で付与）
- legacy profile の互換制御（`--profile legacy` 時のみ、環境変数 `TSUMUGI_SANDBOX` / `TSUMUGI_ENV_ALLOW` でファイル操作と環境変数の許可リストを指定）
- 実行時監査（組み込みAPIで audit sink を設定した場合のみ。tree backend のみ対応）

## エラーメッセージ

エラー発生時に行番号が表示される。関数内のエラーにはスタックトレースが付加される。
構文エラーが複数ある場合は、1回の実行でまとめて報告される。

```
$ cat error.tsg
let x = "hello"
let y = x + 1

$ tsumugi error.tsg
2行目: 演算子 + は Str と Int に適用できません
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
| 埋め込みAPIテスト | `tests/engine_api.rs` | crate root の `Engine` / `ExecutionContext` 公開APIの契約を固定 |
| canonical errorテスト | `tests/canonical_error_inventory.rs` | operation別canonical errorのkind/message/lineをtree/VMで完全一致検証（AUD-019）+ trace/catch/line/importの軸 |
| builtin registryテスト | `tests/builtin_registry_contract.rs` | 単一BuiltinSpec registry（AUD-049）の正本性をクロスエンジンで固定 |
| スケーリングテスト | `tests/scaling.rs` | 確保バイト数と生存バイト数で計算量オーダーと解放漏れを固定 |
| 防御的テスト | `tests/defensive_vm.rs` | 不正な `Chunk` を公開APIへ渡してもホストを落とさないことを検証 |
| テストデータ | `tests/fixtures/` | 正常系 `.tsg` + `.expected` / エラー系 `.tsg` + `.expected_err` |

fixture は `fixture_tests!` へ1行宣言するとツリーウォーク版 / VM版の両方のテストが生成される。判定は完全一致で、子プロセスは制限時間付きで実行する。詳細は `docs/design.md` の「統合テスト」を参照。

## CI

`main` への push と PR で GitHub Actions が `lint` / `test`（3 OS）/ `msrv` / `coverage` の4つのジョブを並行実行する。各ジョブの実行環境と正確なコマンドは[設計ドキュメント](docs/design.md)の「CI」節を正本とし、設定の実体は `.github/workflows/ci.yml` にある。

ローカルで主要な検査を再現する場合は次を順に実行する。

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Windows固有の挙動（`TSUMUGI_*` 環境変数のcase-insensitive保護など）は `test` ジョブのwindows-latestでのみ検証される。MSRV 検証・release workflow・fuzz/stress など Phase 7 の運用ゲートの全体像は[検証・リリース・運用設計](docs/verification-release-operations.md)を正本とする。

## プロジェクト構成

```
src/
├── main.rs       # CLIエントリポイント（REPL / ファイル実行 / --vm 切り替え）
├── cli_capability.rs # CLIの capability profile（safe / legacy）とオプション解析
├── lib.rs        # library crateのルート（埋め込みAPIと各モジュールを公開）
├── engine.rs     # tree-walk向け埋め込み facade（compile / execute / context）
├── embedding.rs  # Phase 1 埋め込みAPI（Engine / compile / link / run、crate rootへEmbedding*で公開）
├── bin/gen_builtins_doc.rs # docs/generated/builtins.md の生成ツール
│
│  --- フロントエンド（両実行系で共有） ---
├── token.rs      # トークン型定義（Spanned: Token + 行番号）
├── lexer.rs      # レキサー（ソース → トークン列、行番号追跡）
├── ast.rs        # AST ノード定義（行番号保持・深度検査・参照名収集）
├── parser.rs     # パーサー（トークン列 → AST、エラー回復と行番号付与）
├── module.rs     # ModuleLoader（import を実行前に解決してリンク）
├── value.rs      # 実行時の値型
├── error.rs      # エラー型（TsumugiError: Parse / Runtime、ErrorKind 22種）
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
├── verifier.rs   # bytecode検証（VerifiedChunk、V1〜V9の検査でVM入口を検証済みchunkへ限定）
├── vm.rs         # スタックマシン VM + コンテキスト依存の組み込み関数
│
│  --- 実行時ガードレール（両実行系で共有） ---
├── builtin_core.rs     # 組み込み関数の共通実装
├── builtin_registry.rs # builtin名・arity・context/pure分類の単一正本（BuiltinSpec）
├── capability.rs       # deny-by-default の CapabilitySet・filesystem root・module resolver（Phase 2）
├── host_function.rs    # host function registry（Phase 2）
├── budget.rs           # 実行予算（BudgetLedger、reserve/commitによるstep等の一元管理）
├── scheduler.rs        # Engine 全体の協調制御（admission・同時実行上限）
├── host_pending.rs     # ブロックしない host call の ticket / waker プロトコル
├── audit.rs            # 実行時監査（schema v1 の event・sink・監査予算、Phase 6）
├── determinism.rs      # Float / NaN の決定的な共有 helper
├── stable_error.rs     # backend 非依存の構造化 error（StableError）
├── limits.rs           # 構造的上限（MAX_AST_DEPTH / MAX_IMPORT_DEPTH / MAX_USER_CALL_DEPTH）
└── sandbox.rs          # import 展開用のパス検査（TSUMUGI_SANDBOX、ModuleLoader 経路専用）

tests/
├── integration.rs                # 統合テスト（ゴールデンテスト + stdin駆動REPL）
├── engine_api.rs                 # 埋め込みAPI（Engine / ExecutionContext）の契約
├── canonical_error_inventory.rs  # canonical error（AUD-019）のtree/VM一致・inventory
├── builtin_registry_contract.rs  # 単一BuiltinSpec registry（AUD-049）の正本性
├── scaling.rs                    # 計算量オーダーと解放漏れの回帰ゲート
├── defensive_vm.rs               # 不正な Chunk を公開APIへ渡してもホストが落ちないこと
├── cli_*.rs                      # CLI の safe / legacy profile と capability オプションの挙動
├── scheduler_api.rs              # 協調スケジューラ・host call pending の公開API
├── audit_*.rs                    # 実行時監査（event・host call・backpressure・queue byte 会計）
└── fixtures/                     # テスト用 .tsg + 期待出力ファイル

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
└── ci.yml            # CI 設定（lint / test 3 OS / msrv / coverage）
```

外部効果の許可は `capability.rs`（実行単位の `CapabilitySet`）、実行予算は `budget.rs`、構造的上限は `limits.rs`（コンパイル時定数）が担う。コールフレーム深度上限は `limits.rs` の `MAX_USER_CALL_DEPTH` に一本化し、ツリーウォーク版・VM版とも root script frame を数えない active user frame 数で判定する（AUD-050 / AUD-017）。

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
