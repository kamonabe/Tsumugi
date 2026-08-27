# Tsumugi — ロードマップ

最終更新: 2026-08-27

## 2026-08-26 深層監査バックログ

ツリーウォーク版とVM版を、REPL継続実行・失敗時状態・スコープ・クロージャ・import・全組み込み関数・資源上限・既存仕様の観点で横断監査した。既存テストは全件成功したが、REPL入力間の状態回復や実行系差を検出できない空白がある。優先度は、ホストプロセス停止／メモリ枯渇につながるものを **P0**、誤実行・状態漏洩・主要仕様差を **P1**、診断性・境界値・文書不整合を **P2** とする。

追加監査では `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo build`、`cargo test` がすべて成功した状態から、隔離した最小入力で既存テスト外の不具合を再現した。再現済み項目は状態欄に明記し、Windows固有挙動やsymlink操作など実環境確認が必要な項目はコード監査結果として区別する。

### P0 — Critical

| ID | 項目 | 再現・影響 | 状態 |
|---|---|---|---|
| AUD-001 | VM REPLのコンパイル失敗をtransactionalにする | ブロック内でlocal追加後にcompile error（当時は未解決index assignment target、現在はループ外`break`等）を置くと、Compilerだけが更新され、次入力の`GetLocal`でRustの範囲外panic。stale loopから`Jump(0)`生成にも到達可能 | ✅ 完了（REPL回帰テスト追加） |
| AUD-002 | VM REPLの未捕捉runtime error後にstack/frame/handler/compilerを復元する | 一時値・callee frame・未実行bindingが次入力へ残り、誤値参照、古い関数の再開、二次panicを起こす | ✅ 完了（REPL回帰テスト追加） |
| AUD-003 | コレクション上限を全生成経路へ一貫適用する | VMのlist/dict literal、`push`、`map`/`filter`、keys/values等で`TSUMUGI_MAX_COLLECTION_SIZE`を迂回でき、メモリDoS防止の完了記載と矛盾 | ✅ 言語から到達する生成・拡張経路を修正。総heap quotaは対象外 |
| AUD-026 | `format_time`の極端なtimestampを定数時間で処理する | `format_time(9223372036854775807, "%Y")`は1970年から1年ずつ進むため実用上停止せず、step予算も消費しない。tree/VMとも2秒以内に完了せず、timeout（終了124）で強制停止 | ✅ 完了（400年周期化・両engineのi64極値timeout回帰テスト追加） |
| AUD-027 | parser・compiler・evaluatorの全再帰経路へ深度制限を適用する | 10万個の`not`連鎖で旧`MAX_PARSE_DEPTH`を迂回し、Rust stack overflowでabort（終了134）。`elif`直再帰や左深BinOp ASTにも同種の経路があった | ✅ 完了（`MAX_AST_DEPTH=256`、生成時検査・子f-string深度継承・実行前の非再帰preflight） |
| AUD-028 | 非循環import chainの深度を制限する | treeのimport実行とVMのinline compileが、旧実装では深い非循環chainでhost stack overflowに到達し得た | ✅ 完了（rootを除くactive chainを128に制限、tree/VM共通エラー） |

### P1 — High

| ID | 項目 | 再現・影響 | 状態 |
|---|---|---|---|
| AUD-004 | VMの`locals_cells`をREPL入力・try unwindで正しく保存／復元する | 入力ごとにtop-level cell対応が消え、closureと変数が別値になる。try内localのcellがcatch変数slotと衝突する | ✅ 完了（cell同一性・catch回帰テスト追加） |
| AUD-005 | treeのwhile/forでエラー時もscopeを必ず解放する | ループ内エラーをcatchすると反復localが後続処理・次REPL入力から見える | ✅ 完了（caught error回帰テスト追加） |
| AUD-006 | import失敗時の`base_dir`・loading/loaded marker・compiler状態を復元する | 同一fileの再試行がsilent skip。VMでは次の相対import基準やlocalsも汚染する | ✅ 失敗rollback完了。import・REPLの状態commit方針はAUD-024で継続 |
| AUD-007 | 非トップレベルimportの意味論を統一する | VMはcompile-time inlineのためfalse branchでもloaded扱い、loopでは複数実行、関数内relative path/control-flowもtreeと異なる | ✅ トップレベル限定として統一（全ネスト構文のparserテスト・tree/VM回帰テスト追加） |
| AUD-008 | `if` / `try` / `catch`のscope仕様を確定し両engineを統一する | treeではblock内`let`が外から可視、VMではcompile error。公開ガイドの「ifはscopeを作らない」とVMが不一致 | ✅ 独立block scopeへ統一（shadowing・error/control-flow・closure・REPL回帰テスト追加） |
| AUD-009 | tree REPLのstep予算を入力単位でresetする | step数がセッション全体で累積し、一度上限に達すると以後の入力も失敗。VMと不一致 | ✅ 完了（入力間回帰テスト追加） |
| AUD-010 | for変数のclosure bindingを反復単位で統一する | `[1,2,3]`で作ったclosureがtreeは`1,2,3`、VMは全て`3` | ✅ 反復ごとのfresh cellへ統一（closure・control-flow・REPL slot再利用回帰テスト追加） |
| AUD-011 | VMのcompile-time name resolution差を仕様化／縮小する | dead branchの未定義名、global forward reference、引数評価順がtreeと異なる | ✅ call validation順とruntime global fallbackを統一（dead code・forward read/write・mutual recursion・REPL/import回帰テスト追加） |
| AUD-012 | context依存builtinの契約を統一する | `input(side())`等の不正arityでtreeは引数を評価せず、VMは副作用後に拒否する。`push`/`pop`はupvalue・一時List・error kindにも差がある | ✅ builtin選択後のarity・破壊対象を引数評価前に検査。一時List拒否、left-to-right snapshot/writeback、local/upvalue/runtime global更新、collection error kindをtree/VMで統一 |
| AUD-013 | VM index assignmentのupvalue対応と評価順を統一する | captured listへ代入不可。object取得順の違いで副作用後に古いlistを書き戻す | ✅ target解決→index→value→in-place更新へ統一。local/upvalue/runtime global対応、未定義targetの先行報告、共有`assign_index`によるメッセージ・境界判定一致（golden pair・両engine REPL回帰テスト追加） |
| AUD-014 | equality / relational comparisonの対象型を統一する | List/Dict/Function/Error、Int×Floatでtreeはtype error、VMはboolを返す場合がある | ⬜ 仕様確定待ち |
| AUD-029 | 複数行lambdaの終端`end`を必須検証する | `let f = fn(x)\n return x`をtree/VMとも構文エラーにせず終了コード0で受理する。EOFを`end`として無条件消費している | ✅ Parserで`End`を必須検証し、tree/VM共通でEOFを構文エラー化 |
| AUD-030 | top-level importの評価時点を統一／仕様化する | `print("BEFORE")`後の失敗importでtreeだけ先行出力する。実行中に生成したmoduleもtreeだけimport可能で、VMのcompile-time inlineと観測可能な差がある | ⬜ 仕様確定待ち（両engine差を再現済み） |
| AUD-031 | Windowsで`TSUMUGI_*`環境変数保護をcase-insensitiveにする | Windowsの環境変数検索は大文字小文字を区別しないがprefix検査は区別するため、`env("tsumugi_sandbox")`等で保護値を読める可能性がある | ✅ Unicode uppercase後のprefix保護を実装。tree/VM、allow-list未設定/全許可、ASCII大小文字・Unicode case alias・secret非漏洩をWindows実OS CIで確認 |
| AUD-032 | 破壊的ファイル操作のfinal symlink意味論を修正する | 旧`check_path`は最終symlinkまでcanonicalizeし、`remove`/`remove_dir`/`rename`がlink自体ではなくlink先を削除・移動していた | ✅ 完了（中間componentのみ解決し、final directory entryを操作） |
| AUD-037 | ローカル名前付き関数のself-bindingを両engineで統一する | 関数内で定義した再帰関数がtreeでは自身を捕捉できず`未定義の関数`、VMでは正常完了する。`factorial(5)`相当でtree失敗／VM `120`を再現 | ✅ 呼び出し時self-bindingとuser binding優先のbuiltin fallbackをtree/VMで統一。匿名lambdaの内部slot名も非公開化 |

### P2 — Medium / Quality

| ID | 項目 | 再現・影響 | 状態 |
|---|---|---|---|
| AUD-015 | callback内`break`/`continue`を通常関数と同じくエラー化する | treeのmap/filter/eachだけ`break`を暗黙`null`として扱い、VMはcompile error | ✅ 完了（control-flow回帰テスト追加） |
| AUD-016 | 同一scopeの`let`再宣言時のbinding identityを仕様化する | 既存closureがtreeでは旧cell、VMでは更新済みcellを参照 | ⬜ 未着手 |
| AUD-017 | call-depth境界を統一する | 上限128にtop-level frameを含めるVMだけ、許容user frame数が1少ない | ⬜ 未着手 |
| AUD-018 | CLIからscript引数を渡せるようにする | `args()`を公開しているがCLIが2個目以降の非flag引数をusage errorにする | ⬜ 未着手 |
| AUD-019 | engine固有error kind/messageを統一する | iteration/index/callback等でkind・messageが異なる。push/pop/map/filter/eachの主要なkindはAUD-012で統一したが、callback messageやtrace差は残る | ⬜ 未着手 |
| AUD-020 | sandboxの脅威モデルとTOCTOU制約を明記する | checkとI/O間のsymlink race、sandbox検査前のcanonicalizeによる許可外path存在oracle、dangling final symlink経由の新規write/append、空設定のfail-open意味論が未整理 | 🟡 security boundaryではないこと、fail-open、未保護資源、symlink/TOCTOU制約を文書化。dangling linkを含む実装修正と隔離環境ガイドは継続 |
| AUD-021 | language-spec / LANG_GUIDE / designのdriftを解消する | engine parity・Float完全一致・全module unit test・coverage/benchmark gate等の記載が現実装やAUD残件と矛盾する | 🟡 規範仕様と既知非適合、VMの実験的位置付け、sandbox制約、予約語、循環参照を更新。意味論確定後の更新は継続 |
| AUD-022 | REPL・differential・limit境界・defensive VMテストを追加する | subprocess timeoutなし、error goldenが部分一致、fixture登録が手動、tree/VMが固定`/tmp`を共有して並列raceする。厳密なstderr/stdout副作用比較も不足 | 🟡 harnessを整備（全子プロセスにtimeout、期待出力を完全一致化、`fixture_tests!`宣言とディレクトリ整合検査、実行ごとの一時ディレクトリ分離）。網羅matrixとfuzzは継続 |
| AUD-023 | VMのunchecked index/`unwrap()`を構造化internal errorへ置換する | compiler/VM invariantが崩れるとhost panic。AUD-001/002でユーザー入力から到達可能だった | ⬜ transaction修正後も防御的に継続 |
| AUD-024 | import・REPLの状態commit方針を明文化する | 未捕捉error前の代入/list mutation/upvalue更新を保持するかrollbackするか未定義。外部I/Oはrollback不能 | ⬜ 設計判断が必要 |
| AUD-025 | VM REPL checkpointの複製コストを削減する | 入力ごとの`stack.clone()`が保持中List/Dictをdeep cloneし、時間・一時メモリがREPL状態量に比例する | ⬜ 計測後にCOW/mutation logを検討 |
| AUD-033 | 未完結REPL入力のEOFを診断する | `if true`等の継続入力中にEOFを送ると、tree/VMとも構文エラーを出さずbufferを破棄して終了コード0になる | ⬜ 未着手（両engineで再現済み） |
| AUD-034 | `path_join`の引数型契約を厳格化する | `path_join("a", 123, "b")`が型エラーにならず`a/b`を返し、非文字列argumentを無言で欠落させる | ⬜ 仕様確定待ち（両engine共通で再現済み） |
| AUD-035 | CLI・標準I/Oのhost panic経路を構造化する | REPLのthread spawn・stdout flush・stdin readに`unwrap()`があり、broken pipe/I/O障害でpanicする。Unixの非UTF-8 argvは`std::env::args()`でもpanicし得る | ⬜ 未着手 |
| AUD-036 | lossyな数値・OS境界変換を検証する | `exit`のi64→i32、`file_size`のu64→i64、NaN/Infを含む`to_int`/`floor`/`ceil`/`round`がwrap・飽和・0化し得る | ⬜ 仕様確定待ち（境界回帰テストが必要） |
| AUD-038 | benchmarkをparse / compile / executeへ分離しVM退行を調査する | 現行Criterionは毎回parseし、VMはcompileも含む。aarch64 release実測でVMはfibが約2.77倍高速な一方、loop 5000回は約358倍低速で、単純な「VMは高速」という説明が成立しない | ⬜ workload別profile・測定分離・回帰gateが必要 |
| AUD-039 | binaryからlibrary moduleを利用して二重コンパイルを解消する | `main.rs`がlibraryと同じmoduleを再宣言し、同一単体テスト138件がlib/binで重複実行される。ビルド時間・テスト件数の解釈を歪める | ⬜ 未着手 |

### 2026-08-27 検証スナップショット

対象はcommit `feb1cbd940b0243faaec91b1eb7cf017c43283ae`、aarch64 Linux、`rustc 1.97.1`。`cargo fmt --check`、Clippy `-D warnings`、全targetテスト、release build、tree/VMのhello smoke testはすべて成功した。単体テスト138件はlib/binで重複実行され、統合テストは150件。`cargo llvm-cov`のline coverageは全体83.55%、`vm.rs` 71.23%、`builtin.rs` 56.54%だった。

Criterionの平均値は次のとおり。各iterationにparseを含み、VMはcompileも含むため、一回実行のend-to-end latencyであり純粋なdispatch速度ではない。

| workload | tree | VM | 相対結果 |
|---|---:|---:|---|
| `fib_20` | 14.982 ms | 5.408 ms | VMが約2.77倍高速 |
| `dict_500` | 9.410 ms | 34.350 ms | VMが約3.65倍低速 |
| `fstr_300` | 89.535 µs | 1.047 ms | VMが約11.7倍低速 |
| `loop_5000` | 762.89 µs | 272.94 ms | VMが約358倍低速 |
| `higher_order_200` | 110.71 µs | 78.633 µs | VMが約1.41倍高速 |

### 追加監査後の推奨改修順

1. **停止性:** AUD-026 / AUD-027 / AUD-028は完了。timeout・深度境界・host abortなしを継続検証する。
2. **誤実行・安全境界:** AUD-012 / AUD-013 / AUD-029 / AUD-031 / AUD-037は完了。意味論選択が必要なAUD-014 / AUD-030は仕様決定後に実装する。
3. **品質基盤:** AUD-022でtimeout・一時directory分離・厳密differential harnessを整備し、AUD-038でbenchmarkを分解してVM loop退行をprofileしてから、P2境界とfuzzを拡充する。

### 初回監査の改修境界（記録）

ユーザー入力だけでホストpanic／状態破損へ到達していた **AUD-001 / AUD-002** を最優先で解消した。同じ状態境界に属する **AUD-004 / AUD-006**、独立して安全に修正できた **AUD-003（主要生成経路）/ AUD-005 / AUD-009 / AUD-012（一部）/ AUD-015 / AUD-022** までを回帰テスト付きで扱い、言語仕様の選択を伴う項目はバックログに残した。

## 実装済み

- [x] 基本型（Int, Float, Str, Bool, Null）
- [x] 変数宣言（let）と再代入
- [x] 四則演算 + 剰余演算子（%）
- [x] 比較演算・論理演算（and / or / not）
- [x] 条件分岐（if / elif / else / end）
- [x] while ループ
- [x] for ループ（リスト・辞書・文字列のイテレーション）
- [x] break / continue
- [x] 関数定義・呼び出し（fn / return / end）
- [x] 第一級関数（関数を変数に代入・引数として渡す）
- [x] 無名関数 / ラムダ（`fn(x) expr end`）
- [x] クロージャ（変数セルの参照キャプチャ・状態共有）
- [x] リスト・辞書
- [x] インデックスアクセス・代入
- [x] 組み込み関数（print, len, push, pop, keys, type, slice, contains, split, join, to_int, to_str, range）
- [x] ファイルI/O（read_file, read_lines, write_file, append_file）
- [x] REPL（複数行入力対応）
- [x] 行番号付きエラーメッセージ
- [x] CI（fmt + clippy + test）
- [x] REPL の is_incomplete をレキサー経由に修正（文字列/コメント内の誤判定解消）
- [x] eval.rs の分割（組み込み関数を builtin.rs に切り出し）
- [x] エラー型の構造化（TsumugiError enum: Parse / Runtime）
- [x] builtin.rs のカテゴリ別分割（I/O・コレクション・文字列・数値・ファイル・パス・日時）
- [x] 高階関数（map / filter / each）
- [x] バイトコード VM: Phase 1（OpCode + Chunk + Compiler + VM + 算術 + Print）
- [x] バイトコード VM: Phase 2（変数 — let / 再代入 / GetLocal / SetLocal）
- [x] バイトコード VM: Phase 3（制御フロー — if/elif/else / while / for / break / continue / and / or）
- [x] バイトコード VM: Phase 4（関数 — FnDef / Call / ReturnValue / 再帰対応）
- [x] バイトコード VM: Phase 5（クロージャ — upvalue / MakeClosure / Lambda）
- [x] バイトコード VM: Phase 6（組み込み関数 — 53個対応）
- [x] バイトコード VM: Phase 7（互換性修正 — min/max Int×Float混合、remove ファイル/ディレクトリ判定、write_file/append_file 型変換）
- [x] スタックトレース（関数呼び出し経路のエラー表示、ツリーウォーク版/VM版両対応）
- [x] ステップ予算（ループ反復 + 関数呼び出しのカウント制限、無限ループ/無限再帰を防止）
- [x] ファイルI/Oサンドボックス（環境変数 `TSUMUGI_SANDBOX` でアクセス許可パスを制限）
- [x] モジュール / import（ファイル分割、循環import検出、ネストimport対応、ツリーウォーク版/VM版両対応）
- [x] エラー処理 / try/catch（ランタイムエラーの捕捉、ネスト対応、ツリーウォーク版/VM版両対応）
- [x] `From<String>` 廃止（全エラー生成箇所を `TsumugiError::runtime()` に統一、文字列再パース除去）
- [x] import のサンドボックス対応（`TSUMUGI_SANDBOX` 設定時に import 先パスも検証、ツリーウォーク版/VM版両対応）
- [x] 環境変数アクセス制御（`TSUMUGI_ENV_ALLOW` で env() の読み取り可能キーを許可リスト制限）
- [x] 浮動小数点 IEEE 754 の基本挙動（VMのFloatゼロ除算をinf/NaNに修正、ツリーウォークにFloat比較armを追加。異種型・複合値を含む比較parityはAUD-014で継続）
- [x] セキュリティ強化（コールフレーム深度制限 MAX_CALL_DEPTH=128、map/filter/each ステップカウント修正、TSUMUGI_* 環境変数ブロック）
- [x] f-string（文字列補間）— `f"hello, {expr}"` 構文。レキサー/パーサー/評価器/VM全対応
- [x] 構造化エラー — try/catch で `Value::Error` を返す。`e["type"]` / `e["message"]` / `e["line"]` でアクセス可能。既存の文字列結合との互換性を維持
- [x] 参照キャプチャ — クロージャが `Rc<RefCell<Value>>` で変数セルを共有。カウンターパターン（状態を保持するクロージャ）をサポート。`Value` の `PartialEq`/`Debug` を手動実装に移行。VM版は `SetUpvalue` オペコード + `locals_cells` でローカル変数のセル昇格を実装

## 設計方針: 組み込み関数の境界線

### 原則: 「自プロセス + OS で完結するか」で判断する

組み込み関数としてベースに入れるもの:
- OS の syscall 1段で完結する処理（ファイルI/O、パス操作、プロセスのメタデータ）
- プロセス内のメモリ操作で完結する処理（文字列操作、リスト操作、型変換）

外だし（将来のモジュール/import）にすべきもの:
- 別ノード・別プロセスとの対話が必要な処理（HTTP、DB接続、メール送信）
- プロトコル交渉・認証・バージョン差異が絡む処理
- 外部ネイティブライブラリ（libmysqlclient 等）への依存が発生する処理

### なぜこの線引きか

1. **変化速度の分離** — DB プロトコルや HTTP の認証方式は言語本体より速く変わる。組み込みにすると、言語本体のリリースサイクルがボトルネックになる
2. **選択肢の多様性** — MySQL / PostgreSQL / SQLite のように「正解が1つに定まらない」ものを1つ選んで焼き込むと、他を使う人にとって死んだ重量になる
3. **不可逆性の回避** — 一度組み込みに入れたものは後から抜けない（Python の urllib 問題）。ユーザーが自分1人の今は自由に出し入れできるが、将来を見据えて「外だし前提」の意識を持っておく

### 現在の組み込み関数が全てこの原則に沿っていることの確認

| カテゴリ | 境界 | 判定 |
|---|---|---|
| 文字列操作（trim, split, replace 等） | プロセス内メモリ操作 | ✅ 組み込み |
| リスト操作（push, sort, reverse 等） | プロセス内メモリ操作 | ✅ 組み込み |
| 高階関数（map, filter, each） | プロセス内メモリ操作 | ✅ 組み込み |
| ファイルI/O（read_file, write_file, mkdir 等） | OS syscall | ✅ 組み込み |
| パス操作（path_exists, is_dir 等） | OS syscall | ✅ 組み込み |
| 環境変数・引数（env, args） | OS プロセスのメタデータ | ✅ 組み込み |
| 時刻（now, format_time） | OS クロック | ✅ 組み込み |
| HTTP（http_get 等） | 外部ノードとの対話 | ❌ 外だし |
| DB接続（query 等） | 外部ノードとの対話 | ❌ 外だし |

### グレーゾーンのケース

| 機能 | 分析 | 判定 |
|---|---|---|
| プロセス起動（exec） | OS syscall だが、起動先が何をするかは制御外。「外部への窓」に近い | 慎重に（入れるなら制限付き） |
| DNS 解決 | 一見 OS のローカル処理だが、実体は外部 DNS サーバーとの通信 | 外だし寄り |
| タイムゾーン処理 | OS クロック + tz データベース参照。データベースの更新が外部依存 | 簡易実装なら組み込み可（現在の format_time は UTC 固定で回避済み） |

### この方針を見直すタイミング

- モジュール / import は実装済み。HTTP や DB を外部モジュールとして提供する「器」は整っている
- ユーザーが増えて「組み込みで http_get が欲しい」という声が出たとき → urllib の教訓を踏まえ、本当に入れるか再検討する

## 次の候補（バイトコード VM）

| Phase | 内容 | 状態 |
|---|---|---|
| 0+1 | OpCode + Chunk + Compiler + VM + 定数 + 算術 + Print | ✅ 完了 |
| 2 | 変数（let / 再代入 / 参照） | ✅ 完了 |
| 3 | 比較 + 条件ジャンプ（if / while / for） + break/continue | ✅ 完了 |
| 4 | 関数定義・呼び出し（コールフレーム） | ✅ 完了 |
| 5 | クロージャ（upvalue） | ✅ 完了 |
| 6 | 組み込み関数（len, push, pop 等） | ✅ 完了 |
| 7 | VM互換性修正 — min/max混合型・remove・write_file | ✅ 完了 |
| 8 | 浮動小数点 IEEE 754 統一 — VMゼロ除算→inf/NaN、Float比較arm追加 | ✅ 完了 |

## 次の候補（言語機能）

| 優先度 | 項目 | メモ |
|---|---|---|
| 低 | クラス（継承なし） | データと操作の束ね方。合成で拡張する方針 |

## 検討事項: クラス

### 背景

現状の Tsumugi には辞書 + 関数で「オブジェクト的なもの」を表現する方法がある。
しかし「データと操作の紐付け」が慣例（第一引数に辞書を渡す）に依存しており、構造が大きくなると見通しが悪い。

### 方針: クラスは検討するが継承はスコープ外

- **クラス構文自体**: 検討スコープ内。`class ... end` でデータと操作をまとめられると便利な場面がある
- **クラス継承（スーパークラス/サブクラス）**: 2026-08 現在スコープ外。理由は後述
- **合成（composition）**: 検討スコープ内。部品を「持つ」方式でクラス間の機能共有を実現する

### 継承をスコープ外とする理由

1. **認知負荷が高い** — 多段継承・メソッドオーバーライドの挙動追跡はプログラミング経験が浅い人にとって大きなハードル。Tsumugi の「入り口レベルで学ぶ」目的と矛盾する
2. **現代的な設計思想との整合** — Go は継承を意図的に持たない。Rust はトレイトで代替する。「継承より合成」が定石として定着している
3. **一度入れたら抜けない** — 継承のメソッド解決順序（MRO）は言語の根幹に影響する。後から設計を変えるのが極めて難しい

### 継承なしで困らない理由

合成パターンで大半のユースケースに対応できる:

```
# 部品
fn create_battery()
    return {"level": 100}
end

fn charge(battery)
    battery["level"] = 100
end

# ロボット犬 = 部品を組み合わせ
fn create_robot_dog(name)
    return {"name": name, "battery": create_battery()}
end

fn recharge(dog)
    charge(dog["battery"])
end
```

クラス構文を入れる場合も同様に「フィールドに別のオブジェクトを持つ」ことで機能を共有する:

```
# 将来のクラス構文（仮）
class RobotDog
    fn init(name)
        self.name = name
        self.battery = Battery()
    end

    fn recharge()
        self.battery.charge()
    end
end
```

### この方針を見直すタイミング

- 「継承がないと書けないプログラム」の具体的なユースケースが明確になったとき
- ただしその場合もまずインターフェース（trait 的な仕組み）で代替可能か検討する

## 検討事項: 実行安全性（ステップ予算） — 実装済み

### 実装内容

- **カウント対象**: ループ先頭への戻り（while/for）+ ユーザー定義関数呼び出し
- **デフォルト上限**: 1,000,000（百万ステップ）
- **上限変更**: 環境変数 `TSUMUGI_MAX_STEPS` で指定（例: `TSUMUGI_MAX_STEPS=5000000`）
- **超過時**: ランタイムエラー `"ステップ上限に達しました (上限: N)"` + スタックトレース
- **ツリーウォーク版・VM版の両方で同じ動作**

### 背景

Tsumugi にはファイルI/O やサンドボックス機能が既に実装されているが、
信頼できないコードの暴走（無限ループ・無限再帰）を防ぐためにステップ予算を導入した。

### 案: ループ反復 + 関数呼び出しのカウント制限

- while / for がループ先頭に戻るタイミングでカウント +1
- 関数呼び出し時にカウント +1
- ループ内の let / if / 代入はカウントしない
- 上限（例: 1,000,000）に達したら強制停止

### この方式の利点

- ユーザーの感覚と一致する（「100万回ループしたら止まる」はわかりやすい）
- ループ内の処理量に左右されない（if が何段あってもカウントに影響しない）
- 書き方に制約を加えない（while true も書ける。止まるなら止まる）
- 無限再帰も検知できる

### 解決済みの事項

- 上限値: デフォルト 1,000,000（百万）。環境変数 `TSUMUGI_MAX_STEPS` で変更可能
- カウント方式: ループ反復 + 関数呼び出しのみカウント（ユーザーにとって予測しやすい方式を採用）
- 実装タイミング: ファイルI/O 実装と同時に導入済み

### 参考

- Dhall: チューリング不完全にすることで全プログラムの停止を保証
- Deno: 権限付与モデル（--allow-net 等）
- Go/Rust Playground: タイムアウトによる暴走防止
- Lua: debug.sethook による命令数コールバック

## 品質改善候補（機能追加ではなく処理改善）

既存の実装を壊さずに品質・堅牢性・開発体験を底上げする改善項目。

### エラーメッセージの改善

| 項目 | 詳細 | 状態 |
|---|---|---|
| 整数リテラルのオーバーフロー検出 | `read_number` で `i64::MAX` 超の入力がパニックする → パースエラーにする | ✅ 完了 |
| 未閉じ文字列の明示エラー | レキサーが `\n` や EOF で打ち切った未閉じ文字列を明示的にエラー報告する | ✅ 完了 |
| `From<String>` の段階的廃止 | `"N行目: ..."` を再パースする脆い変換を構造化エラーに逐次移行 | ✅ 完了 |
| パースエラーの回復 | 最初の1エラーで停止する代わりに複数エラーをまとめて報告 | ✅ 完了 |

### VM 実行性能

| 項目 | 詳細 | 状態 |
|---|---|---|
| `OpCode::CallBuiltin` の String 除去 | 関数名を定数テーブルに移し `CallBuiltin(usize, usize)` にする | ✅ 完了 |
| `dispatch` の match 分割 | 算術・比較・制御フローをメソッドに分けて可読性向上 | 未着手 |
| `call_fn_value` ループ統一 | `run_frames(stop_depth)` を抽出し run() と共有、try/catch 対応を統一 | ✅ 完了 |

### レキサー / パーサーの堅牢性

| 項目 | 詳細 | 状態 |
|---|---|---|
| `Token::Unknown` のエラーメッセージ改善 | パーサーに流れた際に文字名を含む親切なメッセージにする | ✅ 完了 |
| `!` 単体の処理 | 再帰で不明文字が消える問題を修正 | ✅ 完了 |

### テスト品質

| 項目 | 詳細 | 状態 |
|---|---|---|
| カバレッジ可視化 | `cargo llvm-cov` で未到達パスを特定しテスト拡充 | ✅ 完了 |
| ベンチマーク | `criterion` でリグレッション検出 | ✅ 完了 |

### コード構造

| 項目 | 詳細 | 状態 |
|---|---|---|
| `eval.rs` の `exec_stmt` 分割 | 巨大 match を独立メソッドに分離 | 未着手 |
| `Env::functions` の廃止 | 関数を変数として統合しスコープルールを一本化 | ✅ 完了 |

### 実行安全性

| 項目 | 詳細 | 状態 |
|---|---|---|
| ユーザー関数のコール深度制限 | 関数再帰を128フレームでRust実stack overflow前にエラー化 | ✅ 完了（境界差はAUD-017で継続） |
| 構文・AST深度制限 | Parser生成時とCompiler/Evaluator入口でAST深度256を検査。nested f-stringにも親深度を継承 | ✅ 完了（AUD-027） |
| import chain深度制限 | rootを除くactive import chainをtree/VMとも128に制限 | ✅ 完了（AUD-028） |
| サンドボックスの `OnceLock` テスタビリティ | テスト時に環境変数を切り替え可能な設計にする | 未着手 |
| サンドボックスの中間シンボリックリンク迂回修正 | 新規書き込み時に親ディレクトリを `canonicalize()` してからチェック | ✅ 完了 |
| 整数オーバーフローのエラー化 | `checked_add` 等に置き換え、release ビルドでもサイレントラップを防止 | ✅ 完了 |
| メモリ DoS 対策（コレクションサイズ上限） | List/Dictの生成・拡張、List生成builtin、反復変換に上限ガード。`TSUMUGI_MAX_COLLECTION_SIZE` で変更可能 | ✅ 完了（総heap quotaは別課題） |
| ファジングテスト導入 | `cargo-fuzz` でレキサー/パーサー/評価器に無作為入力 | 未着手 |
| VM の `unwrap()` 除去 | コンパイラバグ時にパニックではなく構造化エラーを返す | 未着手 |
| エラー種別の enum 化 | `classify_runtime_error()` の `contains()` 判定を `ErrorKind` enum に移行 | ✅ 完了 |

## 検討事項: HTTP アクセス機能

### 背景

`requests.get(url)` のような HTTP クライアント機能があれば、API呼び出しやWebスクレイピング的な処理が可能になる。
ただし Rust の標準ライブラリには HTTP クライアントがないため、外部 crate の追加が必要。

### 方針: ureq crate を使う

- `ureq` は同期的な HTTP クライアントで依存が比較的少ない
- curl コマンド呼び出し方式も検討したが、Windows 非対応になるため却下
- Tsumugi の「依存ゼロ」は崩れるが、クロスプラットフォーム対応を優先する

### 想定する組み込み関数

```
let resp = http_get("https://example.com/api")
let resp = http_post("https://example.com/api", body)
```

- 成功時: レスポンスボディを文字列で返す
- 失敗時: null を返す（ファイルI/O と同じ方針）

### 実装タイミング

- 「HTTP が本当に必要なユースケースが明確になったとき」に入れる
- 現時点ではファイルI/O だけで十分な範囲をカバーできている
- 入れる場合は cargo-audit の CI 追加も同時に行う（外部依存が初めて入るため）
