# Tsumugi — 設計ドキュメント

最終更新: 2026-08-29

## 目的と位置づけ

Tsumugiは、言語処理系の開発を入り口から理解するための個人プロジェクトとして始まった。レキサー、パーサー、評価器、コンパイラ、VMを自ら実装し、その設計判断を学ぶ目的は今後も継続する。

そのうえで、プロジェクトの次の段階として、サーバーアプリケーションや業務システムに組み込める、制御可能なスクリプト言語を目指す。ホストが明示的に付与した権限と実行予算の範囲内で、業務ルールや拡張ロジックを予測可能かつ監査可能に実行し、瞬間的な性能よりもホストの安定性を優先する。

設計上の価値基準と非目標の正本は[Tsumugi Manifesto](manifesto.md)とする。本書は、その原則を現在のアーキテクチャと具体的な技術判断へ落とし込む。

## 設計目標

新しい方向性では、次を主要な設計目標とする。

- **安定した組み込み境界:** sourceのcompile、入力、実行、結果、停止理由を高水準APIとして提供する
- **明示的な権限:** filesystem、環境変数、時刻、標準入出力、外部サービス、業務操作を実行単位のcapabilityとしてdeny-by-defaultで付与する
- **包括的な資源制御:** 実行単位のfuel・メモリ・時間・入出力予算に加え、エンジン全体の同時実行数と総負荷を制御する
- **協調的な実行:** 小さな実行単位でホストへ制御を返し、yield、backpressure、キャンセル、一時停止・再開を扱えるようにする
- **単一の規範意味論:** backendによって評価順、結果、副作用、エラーが変わらないことを目指す
- **観測・監査可能性:** script identity、言語version、付与権限、予算消費、外部効果、終了理由を構造化してホストへ通知する
- **小さく理解可能な中核:** 純粋な計算を中核に保ち、外部効果をhost boundaryの後ろへ移す

これらは目標であり、現在のalpha実装に対する保証ではない。Phase 1 の最初の縦切りとして、`src/engine.rs` はツリーウォーク用の `Engine`、パース済み `CompiledScript`、状態保持用 `ExecutionContext`、`ExecutionOutcome` を crate root から公開し、CLIもこの入口を利用する。compile は構文解析までで、import は context に依存して execute 時に解決する。実行は caller の同一スレッドで同期的に行われるため、context は十分なスタックを持つスレッド内で生成・利用する必要がある。

現行の環境変数ベースのstep・collection・filesystem・env制限はdefense-in-depthで、実行単位のdeny-by-default capabilityや総heap quotaをまだ提供していない。VM、host I/Oの注入、キャンセル、`exit()`を構造化outcomeとして返す契約も未実装である。現在地と実現順序は[ロードマップ](roadmap.md)で管理する。

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
- `push` は破壊的操作として設計。当初は `Env::get_mut()` で直接リストを変更していたが、現在は `Env::get_cell()` で変数セルを取り、値をsnapshotしてから第2引数を評価し、更新後のListを同じセルへ書き戻す（AUD-012で評価順を規範化）
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

- スコープのスタック（`Vec<HashMap<String, SharedValue>>`）。値はすべて `Rc<RefCell<Value>>` セルで保持し、クロージャと共有できる
- 関数専用のテーブルは持たない。関数は `Value::Fn` として通常の変数と同じスコープに入る（`Env::functions` はロードマップの「`Env::functions` の廃止」で削除済み）
- `frame_base` が現在の call frame の開始位置を持ち、`visible_scopes()` が「フレーム内のスコープを内側から」→「グローバルスコープ（index 0）」の順に返す。間にある呼び出し元のローカルスコープは見えない（AUD-046）
- `push_call_frame()` は関数用スコープを積んで `frame_base` をそこへ移し、復元情報 `CallFrame { previous_base, scope_len }` を返す。`pop_call_frame()` は `truncate` と `frame_base` の復帰だけを行う。グローバルスコープは複製せず共有する
- `set()` は現在のスコープに新しいセルを作って変数を定義
- `set_shared()` は既存のセルを現在のスコープへ直接挿入（クロージャの参照共有用）
- `update()` は `get_cell()` で解決してからセルへ書くため、共有しているクロージャにも反映される。見つからなければ `Err`
- `get()` / `get_cell()` は `visible_scopes()` を辿る。`get_cell()` はセルそのものを返す（インデックス代入・push の破壊的更新で使用）
- `capture_referenced(&HashSet<String>)` は、指定された名前のうち現在見えているセルだけを取る。本体で言及されない名前を捕捉しないことで参照循環を避ける（AUD-042）

### エラー型 (error.rs)

- `TsumugiError` enum でパースエラー（`Parse`）とランタイムエラー（`Runtime`）を構造的に区別
- 各バリアントに `line: usize` と `message: String` を保持
- `Runtime` バリアントには `kind: ErrorKind` フィールドを持ち、エラーの種別を構造的に表現する
- `ErrorKind` enum（18バリアント）: `ZeroDivision`, `Type`, `Index`, `Name`, `StepLimit`, `StackOverflow`, `Sandbox`, `Import`, `Argument`, `IntOverflow`, `ControlFlow`, `CollectionLimit`, `Conversion`, `BuiltinType`, `Iteration`, `Io`, `Internal`, `Runtime`
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
| `env.rs` | 変数 set/get/get_cell、スコープ shadowing、外側スコープ参照、update（再代入）、call frameの可視性（呼び出し元ローカルの非可視・globalの可視・多段frameの分離・frame内block scope）、`capture_referenced` の対象選択と内側優先 |
| `ast.rs` | `referenced_names` が読み・書き・calleeを集めること、ネストした関数・ラムダ本体も辿ること |
| `lexer.rs` | トークン化、行番号付与、エスケープ、演算子、キーワード、コメント |
| `parser.rs` | 各文のAST生成、リスト/辞書リテラル、インデックスアクセス、インデックス代入の対象が識別子であること、非トップレベル`import`の拒否、関数外`return`の拒否と全件報告、エラー回復と行番号含有 |
| `eval.rs` | 算術・比較・論理、関数呼び出し、リスト/辞書操作、組み込み関数、エラーケース全般 |
| `vm.rs` | 内部不変条件の破れをinternal errorとして返すこと（範囲外local slot、stack不足の`Call`、operand overflow、`PrepareCall`を経由しない呼び出しでの深度上限） |
| `sandbox.rs` | `.` / `..` を含むパスの正規化 |

`vm.rs` の単体テストは内部実装と一緒に読める最小ケースだけを置き、公開APIだけで書いた網羅ケースは `tests/defensive_vm.rs` に分ける（AUD-023）。

`src/main.rs` はlibrary crateの公開APIを利用するため、処理系モジュールとその単体テストはlib targetで一度だけコンパイルされる（AUD-039）。binary targetはCLI adapterだけを持ち、CLIの振る舞いは統合テストで検証する。

### 統合テスト（ゴールデンテスト）

`tests/integration.rs` で `.tsg` ファイルをバイナリ実行し、出力を期待値と比較する。

- 正常系: `tests/fixtures/<name>.tsg` + `<name>.expected`（終了コード0・stderr空・stdout完全一致）
- エラー系: `tests/fixtures/<name>.tsg` + `<name>.expected_err`（終了コード非0・stderr完全一致・stdoutは `<name>.expected_out`。既定は空）

fixture 追加手順は、`.tsg` と期待ファイルを置き、`fixture_tests!` へ名前を1行宣言する。宣言から tree / VM 両方のテスト（`<name>::tree` / `<name>::vm`）が生成されるため、片側の登録漏れが起きない。`fixture_declarations_match_directory` が宣言とディレクトリの整合を検査するので、未宣言の fixture はテスト失敗として検出される。

- OS・ロケール依存の文字列は期待ファイル側で `{*}` に逃がす（それ以外は完全一致）
- engine 間に差が残る箇所は、期待ファイルを `<name>.<ext>.vm` で上書きして明示する（`<name>.expected.vm` / `<name>.expected_err.vm`）。`.vm` が無ければ共通の期待ファイルを使うため、差がある fixture だけが `.vm` を持つ。現在は `index_read_lowering`（AUD-019）と `comparison_semantics`（AUD-048）が該当する
- 期待ファイルを持たず専用テストから実行する fixture は `CUSTOM_FIXTURES`、import 先の補助ファイルは `HELPER_FIXTURES` に分類する
- ファイルを触る fixture は `env("TSG_TEST_DIR")` で実行ごとの一時ディレクトリを受け取る（固定パスを共有しない）
- 子プロセスは必ず制限時間付きで待つため、停止しないコードは失敗として検出される

### スケーリングテスト

`tests/scaling.rs` は計算量オーダーを固定する。実時間はCIランナーの負荷で揺れるため、カウントアロケータで**確保バイト数**を測る。解放漏れの検出だけは確保量ではなく生存量（確保 - 解放）を使う。検証している性質は次の6つで、いずれも両engineで確認する。

| 性質 | 測り方 | 由来 |
|---|---|---|
| `for` の反復コストが要素数に線形 | 入力2倍で確保量が約2倍（二次なら約4倍） | AUD-038 |
| コレクション読み取りのコストが要素数に線形 | `xs[i]` / `d[k]` / `len(xs)` を n=500 と n=1000 で比較 | AUD-041 |
| 関数呼び出しのコストが関数body長に依存しない | 到達しない文でbodyだけを膨らませ、同じ回数呼び出して比較 | AUD-040 |
| 関数呼び出しのコストがtop-level bindingの数に依存しない | global 5個と100個で同じ関数を2,000回呼んで比較 | AUD-046 |
| クロージャ定義のコストが可視bindingの数に依存しない | 可視binding 5個と100個でクロージャを2,000回定義して比較 | AUD-042 |
| コレクションへ溜めたクロージャが解放される | 関数を抜けた後の生存量を見る（参照循環の検出） | AUD-042 |

グローバルアロケータでプロセス全体の確保量を数えるため、測定中は `MEASURE_LOCK` を保持して直列化する。ロックのpoisonは無視し、ある測定が失敗しても他の測定が続行できるようにする。

### 防御的テスト

`tests/defensive_vm.rs` は、library利用者が不正な `Chunk` を公開APIへ渡してもホストプロセスが落ちないことを検証する（AUD-023）。`tsumugi::` の公開APIだけを使い、`Vm::new` / `Vm::run_repl_chunk` に手組みのbytecodeを渡す。

固定しているのは、範囲外のlocal slot読み書き・定数参照・upvalue参照、stackが足りない `FStrConcat` / `PopN` / `Print` / `CallBuiltin`、関数先頭の `MakeClosure`、行番号表が空のChunk、`dispatch` へ到達する `try` 命令の8ケース。いずれも`internal`種別の構造化エラーになることを期待値とし、VM実装を修正前へ戻すと `index out of bounds` のpanicで失敗する。

### CI

GitHub Actions (`.github/workflows/ci.yml`) が `main` への push と PR で3つのジョブを実行する。

| ジョブ | 実行環境 | 内容 |
|---|---|---|
| `lint` | ubuntu-latest | `cargo fmt --check`（フォーマット整合性）、`cargo clippy -- -D warnings`（静的解析） |
| `test` | ubuntu-latest / macos-latest / windows-latest | `cargo test`。`fail-fast: false` で1つのOSが落ちても他の結果を得る |
| `coverage` | ubuntu-latest | `cargo llvm-cov --all-features --lcov` で `lcov.info` を生成し、artifact `lcov-report` として保存する |

3 OS matrixを持つ理由は、filesystemとsymlinkの意味論、および `TSUMUGI_*` 環境変数のcase-insensitive保護（AUD-031）がOSごとに異なるためである。これらは実OSで動かさないと検証できない。

toolchainは全ジョブで `dtolnay/rust-toolchain@stable` を使う。`Cargo.toml` に `rust-version` がなく `rust-toolchain.toml` も置いていないため、CIはstableに追従し、compiler版の下限は検証していない（AUD-045）。`clippy` はデフォルトターゲットだけを検査するため、テスト・ベンチを含む `--all-targets` はCIの対象外である。

## 文法定義の置き場所

形式文法は `LANG_GUIDE.md` の「Grammar (Formal)」に1つだけ置く。規範となる意味論は[`language-spec.md`](language-spec.md)（version 0.11）、実装packageは0.1.0である。構文を変更する場合は、規範仕様と`LANG_GUIDE.md`を更新する。

本書はかつて「設計時の文法スナップショット（revision v0.3）」として文法を複製していたが、2つの複製が別々に古くなり、`index_assign` と `dict_literal` で互いに食い違う状態になった。複製をやめ、v0.3から現在までの構文上の変更点だけを設計履歴として残す。v0.3時点の全文はgit履歴で参照できる。

### v0.3 からの構文変更

| 変更 | 内容 | 由来 |
|---|---|---|
| f-string | `primary` に `FSTRING` を追加。`f"...{expr}..."` を補間付き文字列リテラルとして扱う | — |
| `index_assign` の対象 | `postfix "[" expr "]"` から `IDENT "[" expr "]"` へ狭めた。`xs[0][1] = v` や `g()[0] = v` はパースエラー | AUD-013 |
| `import` の位置 | `top_level_stmt` だけが `import_stmt` を含む。block内はファイルI/O前にパースエラー | AUD-007 |
| `return` の文脈 | 文法上は `stmt` だが、関数定義と複数行lambdaの本体でのみ受理する。それ以外はパースエラー。式は省略できない（`return` 単体は不可） | AUD-043 |
| 複数行lambdaの終端 | `end` を必須検証する。EOFを`end`として消費しない | AUD-029 |
| `try` / `catch` | v0.3以降に追加。`try_catch_stmt = "try" NEWLINE block "catch" IDENT NEWLINE block "end" NEWLINE` | — |
| ネスト深度 | 文法では表現しないが、Parserが `MAX_AST_DEPTH = 256` を再帰edgeへ適用する | AUD-027 |

`dict_literal` のキーは文法上は任意の式（`expr ":" expr`）で、Strへ評価されない場合は実行時エラーになる。この非対称は当時から変わっていない。

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
- 関数定義時（`FnDef` / `Lambda`）に `ast::referenced_names()` で本体が言及する名前を集め、`env.capture_referenced()` でその名前のセルだけ `Rc` を共有コピーする（AUD-042。以前は `capture_all()` で可視binding全部を捕捉していた）
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

- 関数値の等価性は同一性比較（`Rc::ptr_eq`）とし、反射律（`f == f`が`true`）を保つ。同じ`fn`から別々に作ったクロージャは等しくない。構造比較や呼び出し結果の比較は行わない（AUD-014で確定）
- 同一性の粒度がengine間で揃っていない: treeは`Value::Fn`の`def: Rc<FnDef>`を定義式の評価ごとに作るため別インスタンスは常に不等になる。一方VMの`Value::VmFn`はcompile時に共有される`Rc<Chunk>`とupvalue cellで比較するため、upvalueを持たない関数値では同じ`fn`式から生成した別インスタンスが等しくなる（AUD-048）
- 循環参照: `Rc<RefCell>`にはcycle collectorがなく、捕捉変数のListへその変数を捕捉したclosureを`push`すると、`cell → List → closure → cell`の循環を言語コードから構成できる。短命なscriptでは影響が限定的でも、REPLや長時間実行では解放されないメモリが累積し得る
- 捕捉範囲: treeは本体が言及する名前のセルを共有し、VMは自由変数だけをupvalue化する（AUD-042で可視binding全捕捉から変更）。treeの判定は厳密な自由変数解析ではなく保守的な近似で、`let`・parameter・`for`変数・`catch`変数のように本体で束縛される名前も集める。そのため同名の外側bindingがある場合、本体がそれを読まなくてもセルを保持し、VMのupvalue集合より広くなることがある。観測可能な挙動は変わらないが、生存量ではこの差が残る
- 意味論の重複: 共通Resolver/HIRはなく、scope・名前解決・call・比較・mutation・importの規則をEvaluatorとCompiler/VMが別々に実装する。拡張時はdifferential testで両engineの観測可能な挙動を固定する必要がある
- パフォーマンス: `RefCell`の実行時借用チェックに加え、値cloneやVMのstack/cell管理コストがある。VMが常に高速とは限らず、workloadごとに優劣が異なる。判断はフェーズ別ベンチマーク（`parse` / `compile` / `execute` / `end_to_end`）の実測に基づき、最新値は `docs/roadmap.md` のスナップショットに記録する

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
`--vm`フラグで実行方式を切り替え可能。Lexer・Parser・ASTは共有し、両方式が同じ規範仕様を満たすことを目標とする。比較（AUD-014）、index代入（AUD-013）、context依存builtin（AUD-012）、import解決時点（AUD-030）はいずれも統一済みである。ただし意味解析以降は別実装のため、同一scopeの`let`再宣言でのcell identity（AUD-016）、捕捉のない関数値の同一性（AUD-048）、call frame深度の境界（AUD-017）、error kind・message（AUD-019）、未捕捉エラー後のREPL状態（AUD-024）に既知の差が残る。現時点のVMは互換性・性能ともに実験的backendとして扱い、非適合は`roadmap.md`で管理する。

### 動機

- ツリーウォークは「AST → 再帰で直接実行」で、言語意味論と実装の対応を追いやすい
- バイトコードVMは「AST → 一次元の命令列 → dispatch loop」という別方式を学べる。ただし一律の高速化は保証しない。AUD-040後の`execute`フェーズ実測では、再帰（`fib_20`）と高階関数・クロージャ生成でVMが速く、f-stringとループでtreeが速く、辞書操作はほぼ同等である。最新値は`docs/roadmap.md`のスナップショットを参照する
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
├── dispatch(name, &[Value], line) → Result<Option<Value>>   # 47アーム
├── builtin_len, builtin_push, ...（builtin_* が47個）
│   ├── 副作用なしの値変換: 32個
│   └── filesystem / env / clock に触るもの: 15個
│       （read_file, read_lines, write_file, append_file, path_exists,
│         mkdir, remove, remove_dir, rename, list_dir, file_size,
│         is_file, is_dir, env, now）
├── check_arity, check_arity_count, check_collection_size_public,
│   is_context_builtin, type_error, assign_index, write_stdout_line（共通検査・共有処理）
└── format_unix_timestamp, is_leap_year, remove_symlink_entry（ヘルパー）
```

設計方針:
- **引数は評価済み `&[Value]`** — 引数評価自体は各engineが担当し、共通モジュールは評価済みの値だけ受け取る
- **context builtinは評価前に共通検証** — `input` / `args` / `exit` / `push` / `pop` / `map` / `filter` / `each`は共通validatorでarityを検査する。`push` / `pop`は第1引数が識別子かも検査し、失敗時は引数を評価しない。VMはbuiltin branch内の`ValidateBuiltinCall` opcodeで実行時に検査する
- **エンジン固有のビルトインは各モジュールに残す** — `push`/`pop`（binding更新）、`map`/`filter`/`each`（クロージャ呼び出し）、`print`/`input`/`exit`/`args`（I/O・プロセス操作）
- **user bindingをbuiltinより優先する** — `print`以外の識別子calleeはlocal/upvalue/runtime globalを先に探し、bindingがない場合だけbuiltinへfallbackする。VMは`JumpIfGlobalDefined`で実行時のglobal登録状態を分岐し、builtin branchとuser-call branchの評価順を混在させない
- **破壊的List操作はbindingへ書き戻す** — builtin `push`/`pop`が選ばれた場合、第1引数はlocal/upvalue/runtime globalの識別子bindingに限定する。第1引数の値を先にsnapshotし、更新後のListを同じbindingへ書き戻す。一時Listは永続化先がないため拒否する
- **新規ビルトイン追加は3か所への登録が必要** — 実装と `dispatch` アームを `builtin_core.rs` へ、委譲名を `builtin.rs` の `match name` へ、VM側の名前判定を `compiler.rs` の `is_builtin()` へ登録する。`builtin.rs` の `match` は `_ => Ok(None)` で終わるため、`builtin_core.rs` だけに足しても両engineから呼べない。名前一覧が3か所に分散していることは既知の構造的リスクで、片側だけ足すと「treeでは呼べるがVMでは呼べない」状態になる（AUD-049）

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

この節は、現在のalpha実装が提供するdefense-in-depth機構を記録する。マニフェストが目標とする実行単位のcapability、包括的な実行予算、エンジン全体の負荷制御とは区別する。

現行機構は誤操作と過剰な資源消費を抑えるが、敵対的コードを隔離するsecurity sandboxではない。非信頼コードにはOS・コンテナ側の権限、CPU、メモリ、filesystem、実行時間制限を併用する。
実行量・collection・I/Oの制御は環境変数で設定し、構文・AST・call frame・import chainの構造的上限はコンパイル時定数で固定する。環境変数方式は未設定時に既定値または制限なしのfail-openとなる（開発時のUXを阻害しない）。deny-by-defaultへの移行は[ロードマップ](roadmap.md)で管理する。

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

### 2026-08-28: import を実行前解決へ統一（AUD-030）

**問題:** treeは`import`文に到達した時点で読み込み、VMはコンパイル時にインライン展開していたため、9ケースの検証で4つの観測差が出た。存在しないモジュールや構文エラーのあるモジュールでは、treeだけ手前の`print`を出力してから失敗した。importより前に実行時エラーがある場合、treeはその実行時エラー、VMはimportエラーを報告した。実行中に`write_file`で生成したモジュールはtreeだけimportでき、逆に実行中に削除したモジュールはVMだけ成功した。副作用の順序、sandbox違反、深度上限、循環importは既に一致していた。

**決定:** VM側の「実行前解決」を規範とする。importはプログラム開始前にすべて解決し、読み込み・パース・sandbox検査・深度検査を実行前に終える。モジュールのトップレベル文は`import`文があった位置で実行するため、正常系の観測結果は変わらない。実行中に生成したファイルのimportは使えなくなる。

根拠は3つある。失敗が副作用より前に必ず出るため、途中まで出力してから落ちる状態がなくなる。実行前にモジュールグラフが確定するため、Phase 5で計画しているhost注入のmodule resolverの受け皿になる。そして自己書き換え的なimportは、`import`がトップレベル限定である制約と合わせて、予測可能性を優先する方針に反する。

**実装:** `src/module.rs`に`ModuleLoader`を追加し、canonicalize・sandbox検査・読み込み・パース・循環と深度の判定を1か所へ集約した。`link`は`import`文をモジュールの文へ置き換えた「リンク済みプログラム」を返す。importが無ければ`None`を返し、AST全体を複製しない。treeの`Evaluator::run`はリンクしてから実行し、VMは`main`がリンク済みプログラムをCompilerへ渡す。これに伴い、treeの`exec_import`とCompilerの`compile_import`、Compilerの`base_dir` / `imported` / `import_depth`を削除した。両engineの`Stmt::Import`は到達しない経路になったため、internal errorを返す。実行が完了しなかったモジュールは`forget`で未解決へ戻し、同じパスを再importできる状態を保つ（AUD-006）。VM REPLではloaderもcompilerと同じcheckpointでrollbackする。

**不採用案:** tree側に合わせてVMも実行時解決にする案は、VMがimport先を現在のchunkへインライン展開してローカルslotを割り当てているため、実行中のslot割り当てとglobal登録が必要になり、AUD-001で整えたREPLのtransaction境界にも影響する。差を仕様として許容する案は、engine間で観測結果が変わる状態を残すため、規範仕様を一つに定める方針と両立しない。

**互換性と境界:** デフォルトのtreeで観測挙動が変わる。実行中にモジュールを生成してimportしていたコードは動かなくなるため、生成が必要な場合は将来のhost resolverで扱う。エラーメッセージと行番号は従来と同じ（`5行目: import 失敗: ...`）である。importより前の実行時エラーは、importの解決が先に走るためimportエラーとして報告される。

**回帰テスト:** golden fixture `import_static_resolution`で正常系の順序・二重importのスキップ・ネスト解決・importした定義の可視性を両engineで固定した。error fixture `error_import_before_side_effects`は、失敗時に手前の`print`とファイル作成が起きないことを固定する。REPLテストでは、実行が完了しなかったモジュールを再importするとtree/VMとも再実行され、観測結果が一致することを検証した。9ケースの手動検証でも全て一致を確認した。

### 2026-08-28: 比較演算の対象型を統一（AUD-014）

**問題:** 9種類の値×9種類×6演算子の486ケースを両engineで実行したところ、128ケースで結果が違った。等価比較（120ケース）は、treeが型の違う値を`type`エラーにし、同じ型でもList / Dict / Fn / Errorをエラーにする一方、VMは真偽値を返していた。大小比較（8ケース）はIntとFloatの混在でtreeだけエラーになった。`null`との比較だけは両engineが一致していた。さらに付随して、型エラーのメッセージへ被演算子の値を埋め込んでいるため、`1 < "ゼロ除算"`のように値が分類キーワードを含むと`classify_runtime_error`の部分文字列判定が誤り、`type`ではなく`zero_division`になっていた。Error値を比較したときの種別ずれも同じ原因だった。

**決定:** 等価比較を全型で成立させ、大小比較は数値に限定する。`==` / `!=`は型エラーにせず、型が違えば等しくないとする。数値はIntとFloatを跨いで比較し、List / Dict / Errorは構造で、関数値は同一性で比較する。大小比較はInt / Floatだけを対象とし、混在を許す。文字列の順序比較は現状どちらのengineも提供していないため、本件では追加しない。

判断の根拠は3つある。第一に、`x == null`が以前から両engineで`false`を返しており、等価比較を全型で成立させる方向が既存挙動と整合する。第二に、`if x == "done"`のような番兵比較が想定外の型で停止しないほうが予測しやすく、マニフェストの予測可能性に沿う。第三に、`min` / `max`がPhase 7でInt×Floatの混在を許しているため、比較でも数値を跨げるほうが一貫する。関数値は反射律（`f == f`が`true`）を保つため同一性比較を採用した。

**実装:** 判定を`Value`の`PartialEq`へ集約した。Int×Floatの数値比較を追加し、`Value::Fn`は`def`と`captured`、`Value::VmFn`は`chunk`とupvalueセルの`Rc::ptr_eq`で同一性を見る。treeの`eval_binop`は型ごとの等価armを削除し、`(l, Eq, r) => l == r`の総称armへ置き換えた。大小比較はInt×Floatの16armを追加した。VMは`OpCode::Eq` / `NotEq`が既に`Value::eq`を使い、`compare_*`が混在を許していたため、意味論の変更は`PartialEq`経由で反映される。エラー種別は、treeの`eval_binop`とVMの算術・比較9か所を`ErrorKind::Type`明示（`internal_type_error`）へ変え、被演算子の値でぶれないようにした。

**不採用案:** treeに合わせて等価比較を型制限する案は、`type(x) == "str"`のような前置きを常に書かせることになり、既にVMとREPLで動いているコードを壊す。関数値を常に`false`とする現行VM案は反射律が崩れ、`contains`でクロージャを探せない。文字列の順序比較を同時に導入する案は、engine差の解消ではなく新機能であり、`sort`の文字列変換比較との関係も含めて別途判断する。

**互換性と境界:** デフォルトのtreeで観測挙動が変わる破壊的変更である。`==`の型エラーに依存して型検査を代替していたコードは、`type(x)`による明示的な判定へ移行する必要がある。VMでも`1 == 1.0`が`false`から`true`へ、`f == f`が`false`から`true`へ変わる。`contains`など等価判定を使う組み込み関数も同じ規則に従う。コレクション以外へのindex（`n[0]`）のerror kindがtreeとVMで異なる問題は本件の対象外で、AUD-019として残る。

**回帰テスト:** golden fixture `comparison_semantics`をtree/VM両方で実行し、同型・異型・数値混在・NaN・ネスト構造・関数の同一性・`contains`との共有・大小比較の型エラー・被演算子による種別ぶれを固定した。加えて486ケースの網羅行列を両engineで実行し、差分が0件であること、エラー種別がすべて`type`になることを確認した。

### 2026-08-28: コレクション読み取りの複製を除去（AUD-041）

**問題:** `xs[i]` / `d[k]` / `len(xs)` は、読み取りのたびにコレクション全体を複製していた。VMは`GetLocal`が値をcloneし、treeも`Env::get`が`cell.borrow().clone()`するため、ループ内の読み取りがO(n^2)になる。確保量で測ると、n=500とn=1000の比が両engineとも約4.0だった。AUD-041はVMの課題として記録していたが、実測ではtreeも同じ欠陥を持っていた。`len`も同じ経路で全体を複製していた。

**決定:** index式が副作用を持たない場合に限り、コレクションを複製せず参照で読む。副作用のない式（リテラル・識別子・それらの演算・それらのindex参照）では、読み取りの時点でコレクションが変化しないため、複製しても参照でも観測結果は同じである。関数呼び出しを含むindex式は、評価前のコレクションを読む現行の意味論を保つため従来のloweringを維持する。treeも同じ規則で最適化し、engine間で観測結果が分かれないようにする。

**実装:** `ast.rs`に`is_side_effect_free`を追加し、両engineで同じ判定を共有する。VMのCompilerは`Expr::Index`のobjectが識別子でローカルに解決でき、index式が副作用なしなら`IndexLocal(slot)`へloweringする（indexを積んでからローカルを参照読みする既存opcode）。`len(識別子)`も同様に`LenLocal(slot)`へ落とす。treeは`Expr::Index`で変数セルを取得し、index式を評価してからセルを`borrow()`して読む。`len`も同じく識別子ならセルを参照して長さだけ取る。`LenLocal`の判定とエラーは`builtin_core::builtin_len`へ委譲し、VM専用だった`value_len`を削除した。これで`len(42)`のエラーは両engineで従来どおり`builtin_type`のまま一致する。

**不採用案:** index式の種類を問わず参照読みへ寄せる案は、`xs[grow()]`のようにindex式がコレクションを変更する場合に読む対象が変わるため不採用。コレクションを`Rc<Vec>` / `Rc<BTreeMap>`のcopy-on-writeにする案は、関数呼び出しを含むindex式も含めて構造的に複製を減らせるが、`Value`の表現と破壊的更新の実装に広く影響するため、AUD-047として分離した。upvalue経由の読み取り（`GetUpvalue`のclone）は今回の対象外で、専用opcodeが必要になる。

**互換性と境界:** 観測可能な挙動は変わらない。goldenフィクスチャで、list/dict/strの読み取り、負index、範囲外・型エラー、shadowing、変更の反映、captured collection、parameter・global経由の読み取り、副作用を持つindex式の順序、`len`の各型とエラーを両engineで固定した。`n[0]`のようにコレクション以外へindexした場合のerror kindは、tree（`runtime`）とVM（`type`）で以前から異なる。本変更の前後で同一であることを確認し、期待値ファイルを分けて明示した。解消はAUD-019で扱う。

**測定:** n=500とn=1000の確保量比（`tests/scaling.rs`のアロケータ）。

| workload | tree 修正前 | tree 修正後 | VM 修正前 | VM 修正後 |
|---|---:|---:|---:|---:|
| `xs[i]` | 3.98 | 1.99 | 3.99 | 1.79 |
| `d[k]` | 4.00 | 2.00 | 4.01 | 1.95 |
| `len(xs)` | 3.97 | 1.99 | 3.98 | 1.79 |

n=1000の絶対値では、`xs[i]`がtree 88,518,619→518,619バイト、VM 88,199,231→199,231バイト。`d[k]`はtree 199,894,216→1,452,216バイト、VM 199,245,586→803,586バイト。関数呼び出しを含む`d[to_str(i)]`は設計どおり比4.00のままである。

### 2026-08-28: VMの内部不変条件をinternal errorへ置換（AUD-023）

**問題:** VMは`self.frames.last().unwrap()`や`chunk.constants[idx]`のように、compilerが正しい命令列を作ることを前提とした無検査アクセスを多く持っていた。公開APIの`Vm::new` / `Vm::run_repl_chunk`は任意の`Chunk`を受け取るため、library利用者が手組みしたbytecodeではこれらがRustのindex panicやunwrap panicになり、ホストプロセスが落ちた。実測では、範囲外のlocal slot・定数index・upvalue index、行番号表が命令列より短いChunk、関数先頭の`MakeClosure`によるoperand計算のunderflowが、それぞれ`src/vm.rs`のindexアクセスでpanicした。`dispatch`の`unreachable!()`も同種の到達点だった。

**決定:** 内部不変条件の破れを`internal`種別の構造化エラーとして返す。エラーメッセージは原因（どの参照が不正か）を示し、行番号は命令に対応するものを使う。scriptの書き方で防ぐのではなく、VM側で必ず検査する。`Chunk::patch_jump`はbytecodeを組み立てる側のAPIなので今回の対象に含めず、compilerの不変条件として残す。

**実装:** `internal_error(line, message)`を追加し、`frame` / `frame_mut` / `take_frame` / `set_ip` / `constant` / `upvalue_cell` / `local_stack_index` / `require_stack_len`を通して参照する形へ統一した。`get_local` / `set_local` / `ensure_local_cell`は`Result`を返すよう変更し、呼び出し側で`?`を伝播する。`Print` / `PopN` / `FStrConcat` / `CallBuiltin` / `MakeClosure`は取り出す個数を先に検査するため、巨大なoperandで`Vec::with_capacity`が過大な確保を行うこともない。`Pop` / `PopN`のslot計算と`MakeClosure`の命令位置計算は`checked_sub`にした。`ReturnValue` / `Return` / `SetupTry` / `TeardownTry`が`dispatch`へ到達した場合は`unreachable!()`ではなくinternal errorを返す。これで本番コードから`unwrap()`・`expect()`・`unreachable!()`はなくなった。

**不採用案:** `debug_assert!`で開発時だけ検査する案は、releaseビルドのライブラリ利用者を守れない。`Chunk`を検証してから実行するvalidator方式は、jump先の到達可能性やstack効果の静的検証が必要で、今回の目的（panicを構造化エラーに変える）に対して過大である。将来bytecodeを外部から読み込む機能を入れる場合に改めて検討する。

**互換性と境界:** compilerが生成した正しいChunkでは挙動が変わらない。golden fixtureのtree/VM出力一致で確認した。ライブラリ利用者から見ると、これまでpanicだった状況が`internal`種別のランタイムエラーになる。`internal`はscriptから`try` / `catch`で捕捉できるが、これは処理系のバグまたは不正なbytecodeを示すため、捕捉して継続することは推奨しない。

**回帰テスト:** 公開APIだけを使う`tests/defensive_vm.rs`を追加し、範囲外のlocal読み書き・定数参照・upvalue参照、stackが足りない`FStrConcat` / `PopN` / `Print` / `CallBuiltin`、関数先頭の`MakeClosure`、行番号表が空のChunk、`dispatch`へ到達する`try`命令の8ケースを検証する。VM実装を修正前へ戻すと、これらは`index out of bounds`のpanicで失敗する。`src/vm.rs`側には内部実装と一緒に読める最小ケースだけ残した。

### 2026-08-28: CLI・標準I/Oのhost panic経路を構造化（AUD-035）

**問題:** 標準I/Oの失敗がpanicへ直結していた。`print`は`println!`を使うため、`tsumugi script.tsg | head -1`のような通常のパイプ利用でbroken pipeになり、tree/VMとも`failed printing to stdout`でpanicした。REPLも`io::stdout().flush().unwrap()`と`io::stdin().read_line().unwrap()`を持ち、出力先が閉じた状態で起動するだけでpanicした。さらにUnixの非UTF-8 argvは`std::env::args()`が内部でunwrapするため、`tsumugi $'\xff'`でpanicした。実行スレッドの`spawn().unwrap()`も同様である。いずれもscriptの結果ではなくホストの異常終了になり、「失敗を観測可能な結果として扱う」というマニフェストの方針と衝突していた。

**決定:** 失敗の扱いを層で分ける。scriptの`print`はランタイムエラー（`io`種別）として返し、`try` / `catch`で捕捉でき、捕捉しなければ通常のエラー表示と終了コード1になる。CLI自身のbanner・prompt・stdin読み取り・スレッド生成・argv検証はscriptのエラーではないため、stderrへ診断を出して終了コード1で終える。broken pipeを終了コード0の暗黙成功にはしない。パイプ切断は観測できる失敗として扱う方が、監査可能性を優先する方針と整合する。

**実装:** `ErrorKind::Io`（`e["type"]`は`io`）を追加し、`builtin_core::write_stdout_line`でロック済みstdoutへ`writeln!`して失敗を構造化エラーへ変換する。tree版`print`（`builtin.rs`）とVMの`OpCode::Print`（`vm.rs`）はこの関数を共有する。`main.rs`には`write_stdout` / `read_stdin_line`を置き、両REPLのbanner・prompt・EOF改行・行読み取りをこれらに置き換えた。argvは`args_os()`で受けて`into_string()`で検証し、非UTF-8なら診断して終了する。`args()` builtinも`args_os()`＋lossy変換にしてlibrary利用時のpanicを避けた。

**不採用案:** broken pipeを静かに終了コード0で終わらせる案はUnixの慣習に近いが、出力が途中で失われたことをホストから観測できない。SIGPIPEを既定動作へ戻す案（`signal(SIGPIPE, SIG_DFL)`）はプロセス全体の挙動を変え、組み込み用途でホストの設定を壊す。`print`の失敗を無視して`null`を返す案は、書き込み失敗を検知できないまま処理が進む。

**互換性と境界:** 出力先が正常なら挙動は変わらない。パイプ切断時は、panic出力の代わりに`N行目: 標準出力への書き込みに失敗しました: ...`が出て終了コード1になる。`io`種別が増えたため、`e["type"]`を網羅的に分岐しているscriptは追従が必要である。`input()`は従来どおり読み取り失敗を`null`にする。スクリプトが`io`エラーを捕捉して`print`を繰り返す場合は失敗し続けるが、ステップ予算で停止する。

**回帰テスト:** 統合テストで、100,000行を出力するscriptの1行目だけ読んでからパイプを閉じ、tree/VM双方でhost panicが出ないこと、終了コードが非0であること、構造化された出力エラーが報告されることを検証する。Unixでは非UTF-8 argvを渡し、panicせず終了コード1でUTF-8に関する診断が出ることを検証する。修正前のコードでは両テストがpanic検出で失敗する。

### 2026-08-28: tree呼び出し時のglobal scope複製を除去（AUD-046）

**問題:** `Env::push_call_frame`が`self.scopes[0].clone()`でglobal scopeのHashMapを複製し、`vec![global]`へ差し替えていた。cell自体は`Rc`共有なので値は複製されないが、entry数ぶんのRc複製とHashMap確保が呼び出しごとに発生する。そのため、tree版の関数呼び出しコストがtop-level bindingの数に比例していた。global 5個と100個で同じ関数を2,000回呼ぶと確保量が3,321,072バイトと12,203,174バイト（比3.67）になり、global 100個で20,000回呼ぶreleaseビルドの実測は56 msだった。AUD-042の捕捉範囲修正で比は7.66から3.67へ下がったが、比例そのものは残っていた。

**決定:** スコープスタックを差し替えるのをやめ、「現在のcall frameがどこから始まるか」だけを覚える。探索対象をフレーム内のスコープとglobal scope（index 0）に限定すれば、呼び出し元のローカルスコープはスタック上に残したままでも見えない。global scopeは複製せず共有する。

**実装:** `Env`に`frame_base`を持たせ、`visible_scopes()`が「フレーム内のスコープを内側から」→「global scope」の順に返す。`get` / `get_cell` / `capture_referenced`はこのイテレータを使う。`update`は`get_cell`で解決してからセルへ書くため、可変イテレータを持たずに同じ意味論を保つ。`push_call_frame`は関数用スコープを積んで`frame_base`をそこへ移し、復元情報として`CallFrame { previous_base, scope_len }`を返す。`pop_call_frame`は`truncate`と`frame_base`の復帰だけを行う。`HashMap::new()`は最初の挿入まで確保しないため、引数のない呼び出しでは確保が消える。

**不採用案:** global scopeを`Rc<RefCell<HashMap>>`にする案は、他のスコープと型が分かれて`visible_scopes`が複雑になり、借用の生存期間管理が増える。呼び出しのたびにglobalへの参照だけをフレームへ渡す案は、`Env`のAPIを呼び出し側へ露出させる。`frame_base`方式はデータ構造を変えずに探索範囲だけを絞れる。

**互換性と境界:** 可視性の規則は変わらない。関数内からglobalは見え、呼び出し元のローカルは見えず、フレーム内のblock scopeは見える。`set` / `set_shared`は常に最内スコープへ書くため、呼び出し中にglobal scopeへentryが増えることはなく、複製をやめても差が出ない。スコープスタックが呼び出し深さぶん伸びるが、`MAX_CALL_DEPTH=128`で抑えられている。VMは別実装なので影響を受けない。

**回帰テスト:** `env.rs`の単体テストで、呼び出し元ローカルの不可視、globalの可視、共有セル経由のglobal更新、フレーム内ローカルの非漏洩、多段フレームの分離、フレーム内block scopeの可視を検証する。`tests/scaling.rs`には確保量ゲートを追加し、top-level bindingを5個から100個へ増やしても2,000回の呼び出しコストが比2.0未満に収まることを両engineで検証する。修正前は比3.67で失敗する。全goldenフィクスチャのtree/VM出力一致も確認した。

**測定:** 2,000回の呼び出しの確保量は、global 5個で3,321,072→2,327,072バイト、100個で12,203,174→2,349,174バイト（比3.67→1.01）。`fib(20)`の確保量は23,164,422→15,832,425バイト。releaseビルドの実時間（3回の最小値）は`fib(22)`が22→17 ms、global 100個で20,000回呼ぶ例が56→8 ms。VMは同条件で変化しない。

### 2026-08-28: treeのclosure捕捉範囲を本体の参照名へ限定（AUD-042）

**問題:** treeは`Env::capture_all()`で定義時に見えるbindingを全部捕捉していた。そのため、クロージャを保持するコレクション自体（`push(saved, fn() i end)`の`saved`）まで捕捉し、cell→list→closure→captured→cellの参照循環になった。`Rc`にcycle collectorはないため、`saved`がスコープから消えても解放されない。関数ローカルのリストへクロージャを溜めて関数を抜けるスクリプトで、実行後の生存量が200個で173,596バイト、400個で345,796バイトとクロージャ数に比例して残った。VMは自由変数だけをupvalue化するため0バイトで、engine間の差にもなっていた。捕捉が広いことは定義コストにも出ており、可視binding 100個の環境でクロージャを2,000回定義すると19,640,166バイトを確保していた（binding 5個なら2,774,064バイト、比7.08）。

**決定:** 捕捉対象を「本体で言及される名前」に限定する。厳密な自由変数解析ではなく保守的な近似を使い、`let`で束縛される名前、parameter、`for`変数、`catch`変数も集める。treeでは`let`より前の参照が外側のbindingを読むため、束縛名を機械的に除くと観測挙動が変わる。近似に留めることで、AUD-042の目的（参照循環の解消と定義コストの削減）を満たしながら意味論を一切動かさない。

**実装:** `ast.rs`に非再帰の`referenced_names(body)`を追加し、既存の深度検査と同じworklist方式でStmt/Exprを辿る。ネストした関数・ラムダの本体も辿るため、内側のクロージャが必要とする名前は外側の関数値が保持する。callee側の識別子も集めるので、builtinと同名のユーザーbindingを呼ぶ場合の優先順位も保たれる。`env.rs`の`capture_all`は`capture_referenced(&HashSet<String>)`へ置き換え、`get_cell`と同じ内側優先の探索で名前ごとにセルを取る。`eval.rs`の`Stmt::FnDef`と`Expr::Lambda`はこの2つを組み合わせて捕捉する。

**不採用案:** 厳密な自由変数解析（束縛名をスコープ単位で除去する）案は、`let`前の外側参照とblock scopeの相互作用を再現する必要があり、VMのcompile時解決と同じ複雑さをtreeへ持ち込む。`Weak`参照で循環を切る案は、クロージャが正当にコレクションを参照するケースで値が消える。cycle collectorの導入は処理系全体の所有権設計に影響するため、この課題の範囲を超える。

**互換性と境界:** 観測可能な挙動は変わらない。本体で言及されない名前はそもそも本体から読めないため、捕捉しなくても差が出ない。top-level bindingは`push_call_frame`がglobal scopeを保持するため、捕捉の有無に関わらず関数内から見える。クロージャが本体で実際にコレクションを参照する場合（`fn() push(saved, 1) end`）の循環は残る。これはVMのupvalue方式でも同じで、`Rc`の性質上避けられない。

**回帰テスト:** golden fixture `closure_capture_scope`をtree/VM両方で実行し、`let`前の外側参照、builtin同名bindingの呼び出し、最内クロージャだけが参照する名前、カウンターの参照共有、捕捉コレクションへのindex代入、f-string、`for` / `catch`変数、コンテナへ溜めたクロージャの後からの呼び出しを固定する。`tests/scaling.rs`には生存量（確保 - 解放）ベースのゲートを追加し、クロージャを溜めたコレクションが解放されること（400個で16KiB未満）と、定義コストが可視bindingの数に比例しないこと（binding 5個と100個の比が2.0未満）を検証する。修正前は前者が345,796バイト、後者が比7.08で両方とも失敗する。

**測定:** 生存量は200個で173,596→0バイト、400個で345,796→0バイト。定義コストは可視binding 100個で19,640,166→2,560,166バイト、binding 5個との比は7.08→1.01になった。一方、関数呼び出しの確保量はglobal数への比が7.66→3.71に下がっただけで比例が残る。原因は捕捉ではなく`push_call_frame`がglobal scopeのHashMapを呼び出しごとに複製することで、AUD-046として分離した。

### 2026-08-28: 関数外`return`の構文エラー化（AUD-043）

**問題:** Parserが関数の外の`return`を無条件に受理していた。`import`は配置をパース時に検査し、`break` / `continue`はループ外で実行時エラーになるのに対し、`return`だけ文脈検査がなかった。結果として3つの症状が出ていた。(1) VM REPLでtop-level変数がある状態の`return`は、`ReturnValue`がtop-level frameをpopしstackを`base`まで捨てる一方でCompilerの`locals`が残り、次入力の`GetLocal`が空stackを読んでhost panicになった（`try`内・`for`内でも再現）。(2) file実行では両engineとも後続文を実行せず、診断なしで終了コード0になった。(3) import先のトップレベル`return`は、treeがmodule実行だけ打ち切って呼び出し元を継続する一方、VMはinline展開された`ReturnValue`がroot script全体を終了させた。

**決定:** `return`は関数定義と複数行ラムダの本体でのみ有効とし、それ以外の位置はパースエラーにする。Lexer・Parser・ASTは両engineで共有するため、パース時に拒否すれば実行系を触らずにengine差ごと解消できる。`break` / `continue`と同じ「文脈が違えばエラー」という既存の直感に合わせ、メッセージも同じ形にする。

**実装:** Parserに関数本体のネスト深度`fn_depth`を持たせ、`with_fn_context`で`parse_fn_def`と複数行ラムダの本体パースだけ深度を上げる。`parse_stmt_inner`は`fn_depth > 0`のときだけ`return`を文として受理し、それ以外は`reject_non_function_return`でエラーにする。`parse_block`でも`fn_depth == 0`の`return`を`import`と同じ方式で先に処理し、行を捨ててから回復するため、囲みの構文を巻き込まず1件だけ報告して停止もしない。1行ラムダの明示`return`は`parse_anonymous_fn`が直接消費するため従来どおり通る。

**不採用案:** 実行時エラーにする案は、tree（`EvalResult::Return`をtop-levelで無視）とVM（frameを畳む）の両方に別々の対処が必要で、REPLのrollback契約にも手が入る。Parserを触らずVMの`get_local`だけ構造化エラーへ変える案は、panicは避けられても無言終了とimport時のengine差が残る。トップレベル`return`をscript終了として仕様化する案は、`exit()`と役割が重なり、import先で「どこまで終了するか」を新たに定義する必要がある。

**互換性と境界:** 関数外の`return`に依存したスクリプトはパースエラーになる。早期終了が目的なら`exit()`、値を返すなら関数にまとめる。既存fixtureとexamplesに該当箇所はなかった。import先の`return`は両engineで同じ`import`エラーになるが、tree側だけ失敗より前のtop-level出力が残る点は評価時点の差（AUD-030）であり、本変更の対象外である。library利用者が不正な`Chunk`を直接VMへ渡す経路のpanic面はAUD-023として残る。

**回帰テスト:** Parserの単体テストで、トップレベル・`if` / `elif` / `else` / `while` / `for` / `try` / `catch`の各ブロックでの拒否、関数・多段ネスト・複数行ラムダ・1行ラムダ・関数内関数での受理、関数本体を抜けた後の再拒否（`fn_depth`の戻し漏れ検出）、複数箇所の全件報告と進行保証を検証する。統合テストではfixture `error_return_outside_fn`をtree/VM両方で実行し、2件のパースエラーとstdout副作用なしを完全一致で固定する。REPLテストは`bare` / `try`内 / `for`内の3形をtree/VM双方で流し、host panicが出ないこと、配置エラーが1件であること、失敗入力の後もtop-level bindingを読めることを検証する。

### 2026-08-27: tree関数値のRc共有化（AUD-040）

**問題:** AUD-038でフェーズ別に計測したところ、treeの`fib_20`が旧スナップショットの14.982 msから28.349 msへ遅くなっていた。二分探索の結果、AUD-037で入れた呼び出し時self-bindingが原因で、`self.env.set(func_name, func_value.clone())`が毎回`Value::Fn`を複製していた。`Value::Fn`は`body: Vec<Stmt>`を値で持つため、呼び出しごとに関数本体のASTを深くコピーしていた。`env.get`による取得時の複製と合わせて1回の呼び出しで2回、さらに`captured`のHashMapも同数複製していた。body 2文の関数を300回呼ぶと909,821バイト、body 100文なら14,457,505バイト（比15.89）を確保していた。

**決定:** 関数値の不変部分を`Rc`で共有する。VM側の`VmFn`が`Rc<Chunk>`で同じ問題を避けているため、新しい方針ではなくtreeへの適用である。`captured`は定義時に作って以後読むだけで、中のセルは元から共有参照なので、マップ自体も`Rc`で共有して差し支えない。AUD-037が定めた「呼び出し時にself-bindingする」意味論は変えず、複製を安くするだけに留める。

**実装:** `value.rs`に`FnDef { name, params, body }`を追加し、`Value::Fn { def: Rc<FnDef>, captured: Rc<HashMap<String, SharedValue>> }`とした。`eval_call`と`call_fn_value`（map/filter/each経路）は`Rc::clone`で借用から切り離してから本体を実行する。定義時（`Stmt::FnDef` / `Expr::Lambda`）はASTから本体を一度複製して`Rc`へ入れる。

**不採用案:** self-bindingでbinding cellを共有する案（`set_shared`）は、本体内で関数名へ再代入した場合に外側のbindingまで書き換わるため、意味論が変わる。`body`だけを`Rc`にする案は`captured`と`params`の複製が残る。AST側の本体まで共有して定義時の複製もなくす案は、`ast.rs`とparser・compilerへ波及するため、効果を`closure_def_200`で測ってから判断する。

**互換性と境界:** 言語の観測可能な挙動は変わらない。`Value::Fn`は`def`と`captured`の2フィールドになるため、パターンマッチしている箇所は追従が必要。定義時の本体複製は残るため、ループ内でクロージャを生成する場合のコストは本体長に比例する。treeの捕捉範囲が広いことによる参照循環（AUD-042）は本変更の対象外で、`93b7606`と同じ挙動である。

**回帰テスト:** `tests/scaling.rs`に、到達しない文で本体だけを膨らませた関数を同じ回数呼び出し、確保量が本体長に比例しないことを検証する測定を追加した（比15.89 → 1.06）。グローバルアロケータを共有するため、測定は`MEASURE_LOCK`で直列化する。関数の意味論（再帰、関数内named関数の自己参照、相互再帰、第一級関数、クロージャの状態共有、self-bindingのparameter shadow、再定義、map/filter/each経由のcallback、arity・非関数呼び出し・深い再帰のエラー）はtree/VMの差分比較で一致を確認した。

### 2026-08-27: ベンチマークのフェーズ分離とVM forループの計算量修正（AUD-038）

**問題:** Criterionが1 iterationごとにparseし、VMはcompileも含めていたため、測定値はend-to-end latencyであり、engine差がどのフェーズに由来するのか分からなかった。実測ではVMの`loop_5000`がtreeの約358倍低速で、「VMは高速」という説明が成立していなかった。

**調査:** `while`ループはtree/VMがほぼ同速（比1.0〜1.1）で、入力を倍にしても線形に伸びる。一方`for i in range(0, n)`はVMだけ入力を倍にすると時間が4倍になり、O(n^2)だった。原因はCompilerのforループloweringで、反復ごとに`GetLocal(collection_slot)`を2回発行していた点にある（条件の`Len`と要素取得の`Index`）。`get_local`は値を複製するため、反復ごとにコレクション全体をコピーしていた。確保バイト数で見るとn=2000で960 MB、n=4000で3.84 GBだった。

**決定:** ベンチマークを`parse` / `compile` / `execute` / `end_to_end`の4グループへ分ける。`execute`は`iter_batched`で初期化とChunk複製をセットアップへ追い出し、engine比較は同じフェーズ同士で行う。コレクションを介さない`while_5000`を追加し、イテレーション処理の追加コストを分離できるようにする。forループのコレクションはスタックへ複製せず、slotから参照で読む。

**実装:** `OpCode::Len`を`LenLocal(usize)`へ置き換え、`IndexLocal(usize)`を追加した。どちらもVMの`with_local_ref`でcell化済みならcell、未cell化ならstack slotを参照し、長さまたは要素だけをpushする。`eval_index`はコレクションを参照で受け取る形に変え、`value_len`を共通ヘルパーへ切り出した。イテレーション対象は`ToIterList`が開始時にスナップショットする既存仕様のままで、意味論は変わらない。

**不採用案:** `Value::List` / `Dict`の内部を`Rc`共有にする案は、複製が安くなる一方で値意味論とエイリアシングを変えるため、言語仕様の変更になる。一般の`xs[i]`読み取りまで参照読みへ広げる案は、index式が同じbindingを変更した場合の評価順がtreeと変わるため、AUD-013と同種の仕様判断としてAUD-041へ分離した。実時間で回帰ゲートを組む案はCIランナーの負荷で揺れるため採用しない。

**互換性と境界:** 言語の観測可能な挙動は変わらない。ベンチマークのグループ名が変わるため、既存のCriterionベースラインとは比較できない。`for`の反復コストは要素数に線形になったが、ループ内での`d[k]`のような読み取りは依然コレクションを複製する（AUD-041）。tree側の呼び出しコストはAUD-037のself-bindingによる`Value::Fn`複製で増えており、AUD-040として追跡する。

**回帰テスト:** `tests/scaling.rs`がカウントアロケータで確保バイト数を測り、入力を2倍にしたときの伸びが3倍未満であることを両engineで検証する（線形なら約2倍、二次なら約4倍）。実時間に依存しないため決定的で、旧loweringへ戻すと比4.00で失敗することを確認した。paired golden fixture `for_iteration_snapshot`では、list/dict/str/空コレクションの反復、ネスト、break/continue、反復中の再代入と破壊的更新に対するスナップショット維持、反復ごとのfresh cell、ループ変数のcell昇格経路、負index・範囲外を検証する。

### 2026-08-27: 統合テストharnessの整備（AUD-022 harness分）

**問題:** fixtureを実行する子プロセスに制限時間がなく、停止しないコードを踏むとCIがハングした。エラー系の期待ファイルは行ごとの部分一致だったため、stack traceの行番号やimportエラーのファイル名を検証できず、エラー前後のstdout副作用も未検証だった。fixtureのtree/VM登録は手書きの`#[test]`を2つ並べる方式で、片側の登録漏れを検出できなかった。さらにファイルI/O系fixtureが`/tmp`の固定パスを共有し、tree/VMの並列実行やテスト間で同じパスを読み書きしていた。

**決定:** 判定は完全一致を既定とし、逃がす範囲を期待ファイル上で明示する。OS・ロケール依存の文字列だけ`{*}`ワイルドカードを許可し、engine間で意図的に残る差は`<name>.expected_err.vm`として可視化する。fixtureは1行の宣言からtree/VM両方のテストを生成し、宣言とfixtureディレクトリの整合をテストで検査する。ファイルを触るfixtureには実行ごとに専用の一時ディレクトリを渡し、固定パスを共有しない。子プロセスは必ず制限時間付きで待つ。

**実装:** `wait_with_timeout`を共通ヘルパーへ移し、fixture実行・bespokeスクリプト実行・REPL実行のすべてを既定30秒で待つ。停止性を検証する`format_time_extreme`だけ2秒に短縮する。`run_fixture`が正常系（終了コード0・stderr空・stdout一致）とエラー系（終了コード非0・stderr一致・stdout一致）を判定し、`fixture_tests!`が`<fixture>::tree` / `<fixture>::vm`を生成する。`fixture_declarations_match_directory`が期待ファイル・宣言テーブル・`.tsg`の全数を突き合わせ、専用テストで実行するfixtureは`CUSTOM_FIXTURES`、import先の補助ファイルは`HELPER_FIXTURES`として分類を明示する。

一時ディレクトリは`TestDir`がテスト・engine・プロセス・連番ごとに作成し、`Drop`で削除する。パスは`TSG_TEST_DIR`でスクリプトへ渡す（`TSUMUGI_`始まりは処理系が保護して`env()`から読めないため別prefix）。ファイルI/O系fixtureは`TSUMUGI_SANDBOX`もその一時ディレクトリだけに限定する。REPLのプロンプト除去は`repl_visible_lines`へ集約した。

**不採用案:** 全fixtureを1つのテストでループする案はテスト名から失敗箇所が分からず、並列実行の粒度も失う。`concat_idents`相当のためにpasteクレートを追加する案は依存を増やすため、宣言側でテスト名を兼ねるモジュール名を書く方式にした。`error_stack_overflow`に129行の期待ファイルを置く案は、AUD-017のframe数差（tree 128 / VM 127）が大量の同一行に埋もれるため、frame数を数値で明示する専用テストに置き換えた。部分一致を残す案は、どこを検証していないかが期待ファイルから読み取れないため採用しない。

**互換性と境界:** テスト名が`golden_<name>` / `vm_golden_<name>`から`<name>::tree` / `<name>::vm`へ変わる。fixtureを追加する際は`fixture_tests!`への宣言が必須になり、宣言しないとテストが失敗する。ファイルを触るfixtureは`/tmp`の固定パスではなく`env("TSG_TEST_DIR")`を使う。網羅matrixの拡充とfuzz導入はAUD-022の残件として継続する。

**回帰テスト:** harness自身の検出力を、未登録fixtureの追加、期待stderrの1文字変更、正常系へのstderr混入、無限ループfixtureのtimeout、`{*}`を含む期待文の別メッセージ化で確認した。並列2プロセスで同じファイルI/O fixtureを実行しても競合しないこと、実行後に一時ディレクトリが残らないことも確認した。

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
