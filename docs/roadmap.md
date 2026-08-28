# Tsumugi — ロードマップ

最終更新: 2026-08-28

## プロジェクトの方向性

Tsumugiは、学習用の言語処理系として得た知見を発展させ、実運用を見据えた、制御可能な組み込みスクリプト言語を目指す。価値基準と非目標の正本は[Tsumugi Manifesto](manifesto.md)とし、本ロードマップは現在地からその目標へ進む順序を管理する。

最短の実行時間よりホストの安定性を優先する。新しい言語機能を増やす前に、組み込み境界、明示的な権限、包括的な実行予算、規範意味論、監査可能性を整える。

## マニフェスト実現ロードマップ

以下は目標アーキテクチャへの移行順序である。現行のstep上限、collection上限、深度制限、filesystem/env allow-list、構造化エラー、差分テストは土台として再利用するが、それだけで各phaseが完了したとはみなさない。

| Phase | 目的 | 主な完了条件 | 状態 |
|---|---|---|---|
| 0 | 保証範囲と脅威モデル | マニフェスト、対象script作者、ホストとOS隔離の責務、用語を文書化 | 🟡 マニフェスト策定。詳細な保証境界は継続 |
| 1 | 安定した組み込みAPI | `Engine`、compile済みscript、実行context、構造化outcomeを公開し、CLIも同じAPIを利用 | ⬜ 未着手 |
| 2 | deny-by-default capability | filesystem、env、clock、stdin/stdout、process、host functionを実行単位で明示付与し、ambient accessを既定で禁止 | ⬜ 未着手 |
| 3 | 包括的な実行予算 | fuel、総heap、文字列・入出力、source/import、deadline、cancellationを扱い、超過後の再開可否を規定 | 🟡 step・collection・深度上限のみ実装済み |
| 4 | 協調実行と負荷制御 | 実行slice、yield、一時停止・再開、エンジン全体の同時実行上限、backpressureを提供 | ⬜ 未着手 |
| 5 | 規範意味論と決定的境界 | 正式backendを一つに定め、clock・env・I/O・module resolverをhost注入し、既知のengine差を解消 | 🟡 差分監査とpaired testを継続中 |
| 6 | 実行時監査 | script/source hash、version、capability、host call、deny、予算消費、終了理由をaudit sinkへ通知 | 🟡 構造化エラーのみ実装済み |
| 7 | 運用保証と検証 | 資源制約下の通常テスト、opt-in stress/fuzz、capability matrix、budget境界、audit完全性を継続検証 | 🟡 timeout・scaling・golden testを実装済み |

長時間処理について「確実に終わる」とは、任意のscriptの成功を保証することではない。ホストを不安定にせず、完了、停止、または失敗を観測可能な結果として扱い、有限の処理を設定された負荷の中で着実に進められることを目標とする。

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
| AUD-043 | トップレベル`return`の文脈を検証する | Parserが関数外の`return`を無条件に受理するため3つの症状になる。(1) VM REPLでtop-level変数がある状態で`return`を実行すると、`ReturnValue`がroot frameをpopしstackを`base`まで捨てる一方Compilerの`locals`は残るため、次入力の`GetLocal`が空stackを読み`src/vm.rs`の範囲外panicでhost abort（終了1）。`try`内・`for`内の`return`でも再現し、tree REPLは同入力で正常継続する。(2) file実行では両engineとも後続文を実行せず、エラーなしで終了コード0になる。(3) import先のトップレベル`return`は、treeがmodule実行だけ打ち切って呼び出し元を継続する一方、VMはinline展開された`ReturnValue`がroot script全体を終了させる。`break` / `continue`は両engineでエラー化されるが`return`だけ検査がない | ✅ 完了（Parserに関数本体の深度を持たせ、関数外の`return`を両engine共通のパースエラーへ。parser単体・error fixture・tree/VM REPLの回帰テスト追加） |

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
| AUD-038 | benchmarkをparse / compile / executeへ分離しVM退行を調査する | 現行Criterionは毎回parseし、VMはcompileも含む。aarch64 release実測でVMはfibが約2.77倍高速な一方、loop 5000回は約358倍低速で、単純な「VMは高速」という説明が成立しない | ✅ 4フェーズへ分離し退行の原因を特定・修正（VMのforが反復ごとにコレクションを複製しO(n^2)だった）。確保量ベースのスケーリングゲートを追加。副産物としてAUD-040 / AUD-041を検出 |
| AUD-039 | binaryからlibrary moduleを利用して二重コンパイルを解消する | `main.rs`がlibraryと同じmoduleを再宣言し、同一単体テスト139件がlib/binで重複実行される。ビルド時間・テスト件数の解釈を歪める | ⬜ 未着手 |
| AUD-040 | treeの名前付き関数self-bindingで`Value::Fn`の複製を避ける | AUD-037の呼び出し時self-bindingが毎回`Value::Fn`（body AST含む）をcloneし、呼び出しコストが関数body長に比例する。`fib(22)`で67.0ms（該当行を無効化すると42.2ms、約1.6倍） | ✅ `Value::Fn`を`Rc<FnDef>` + `Rc<captured>`へ変更（VmFnの`Rc<Chunk>`と同じ方針）。同一条件A/Bで`fib(22)` 64.5ms→21.2ms、確保量の比 15.89→1.06。確保量ベースの回帰ゲートを追加 |
| AUD-041 | VMのコレクション読み取りで全体複製を避ける | `GetLocal`が値を複製するため、ループ内の`d[k]` / `xs[i]`読み取りがコレクション全体をコピーする。forループの反復自体はAUD-038で解消したが、一般のindex読み取り経路は残る | ⬜ 未着手。index式を副作用のないもの（literal・識別子・それらの演算）に限れば、コレクションを後から参照で読んでも観測結果は同じで仕様判断は不要。global targetの未定義エラー順序はAUD-013の`RequireGlobal`をindexより前に置けば保てる。関数呼び出しを含むindex式は現行loweringを維持する |
| AUD-042 | treeのclosure捕捉範囲を自由変数へ絞る | treeは`capture_all()`で定義時に見える全bindingを捕捉するため、クロージャを保持するコンテナ（`push(saved, fn ...)`の`saved`等）まで捕捉し、cell→list→closure→captured→cellの参照循環でメモリが解放されない。200回×200個で51.8MB（循環しない書き方では2.19MB）。VMは自由変数だけをupvalue化するため発生しない。捕捉範囲の統一は生成コストの削減にもなる | ✅ 完了（本体で言及される名前だけを捕捉。生存量は400個で345,796→0バイト、定義コストは可視binding 100個で19,640,166→2,560,166バイト。生存量ベースと定義コストのscalingゲート、tree/VM両engineのfixtureを追加） |
| AUD-046 | treeの関数呼び出しでglobal scopeの複製を避ける | `push_call_frame`が`self.scopes[0].clone()`でglobal scopeのHashMapを呼び出しごとに複製するため、呼び出しコストがtop-level bindingの数に比例していた。global 5個と100個で同じ関数を2,000回呼ぶと確保量の比が3.67（AUD-042前は7.66）。VMは同条件で1.03。cellは`Rc`共有なので値は複製されないが、entry数ぶんのRc複製とHashMap確保が毎回発生していた | ✅ 完了（スコープスタックを差し替えず`frame_base`で探索範囲を限定。確保量は2,000回の呼び出しでglobal 100個のとき12,203,174→2,349,174バイト、比3.67→1.01。releaseの実時間は`fib(22)` 22→17 ms、global 100個×20,000回 56→8 ms。可視性の単体テストとscalingゲートを追加） |
| AUD-044 | 完了済み非適合と古い記述をREADME・規範仕様から除く | `README.md`は仕様revisionを`0.5`と書くが`language-spec.md`は0.6である。captured collectionへのindex代入はAUD-013で完了し、tree/VMとも`[99, 2]`で一致するのに、両文書の「既知非適合」に残る。構成図は`lib.rs` / `builtin_core.rs` / `limits.rs` / `sandbox.rs`を欠き、`env.rs`を「関数テーブル」と説明し、examplesを3件中1件しか挙げない。CI手順は矢印区切りでコピー実行できず、`cargo clippy`の`--`が欠け、3 OS matrixとcoverage jobを記載していない。組み込み関数53個とLICENSE(MIT)の記載は実測と一致する | 🟡 仕様revision表記はAUD-043で0.7へ揃えた（`README.md` / `design.md`）。完了済み非適合の掲載、構成図、CI手順は未着手 |
| AUD-045 | 配布・実行手順とtoolchainの下限を明示する | READMEのクイックスタートは`cargo build` / `cargo run`だけだが、エラーメッセージの例は`$ tsumugi file.tsg`を使う。`cargo install --path .`やPATH設定、再導入手順の記載がない。`Cargo.toml`はedition 2024を要求しながら`rust-version`を宣言せず、`rust-toolchain.toml`もないためCIはstable追従で、compiler版の下限を検証できない。release / install workflowとcommit SHA固定のaction参照もない | ⬜ 未着手 |

### 2026-08-27 検証スナップショット

対象はcommit `feb1cbd940b0243faaec91b1eb7cf017c43283ae`、aarch64 Linux、`rustc 1.97.1`。`cargo fmt --check`、Clippy `-D warnings`、全targetテスト、release build、tree/VMのhello smoke testはすべて成功した。単体テスト138件はlib/binで重複実行され、統合テストは150件。`cargo llvm-cov`のline coverageは全体83.55%、`vm.rs` 71.23%、`builtin.rs` 56.54%だった。

Criterionの平均値は次のとおり。各iterationにparseを含み、VMはcompileも含むため、一回実行のend-to-end latencyであり純粋なdispatch速度ではない。最新の測定値は次節「フェーズ別ベンチマーク」を参照する（この表はAUD-038前の記録として残す）。

| workload | tree | VM | 相対結果 |
|---|---:|---:|---|
| `fib_20` | 14.982 ms | 5.408 ms | VMが約2.77倍高速 |
| `dict_500` | 9.410 ms | 34.350 ms | VMが約3.65倍低速 |
| `fstr_300` | 89.535 µs | 1.047 ms | VMが約11.7倍低速 |
| `loop_5000` | 762.89 µs | 272.94 ms | VMが約358倍低速 |
| `higher_order_200` | 110.71 µs | 78.633 µs | VMが約1.41倍高速 |

### 2026-08-27 フェーズ別ベンチマーク（AUD-038）

対象はcommit `c0fd91f`＋AUD-038の変更、aarch64 Linux、`rustc 1.97.1`、Criterionの中央値。

`parse` は 1.85–4.19 µs、`compile` は 0.72–2.73 µs で、いずれも実行時間より3桁小さい。したがって旧スナップショットのend-to-end値は実質的に実行フェーズの値であり、engine差の原因はparse/compileではない。

| workload（execute） | tree | VM | 相対結果 |
|---|---:|---:|---|
| `fib_20` | 29.130 ms | 6.158 ms | VMが約4.7倍高速 |
| `dict_500` | 11.185 ms | 10.759 ms | ほぼ同等 |
| `fstr_300` | 99.77 µs | 155.21 µs | VMが約1.6倍低速 |
| `loop_5000` | 870.22 µs | 1.490 ms | VMが約1.7倍低速 |
| `while_5000` | 1.032 ms | 1.159 ms | VMが約1.1倍低速 |
| `higher_order_200` | 112.72 µs | 81.49 µs | VMが約1.4倍高速 |

`loop_5000`（コレクション反復）と `while_5000`（コレクションを介さない反復）を並べると、イテレーション処理の追加コストが分離できる。

旧スナップショットからのVM側の変化は次のとおり。原因はいずれもforループの反復ごとのコレクション複製で、AUD-038で解消した。

| workload | 旧VM | 新VM |
|---|---:|---:|
| `loop_5000` | 272.94 ms | 1.392 ms |
| `dict_500` | 34.350 ms | 10.587 ms |
| `fstr_300` | 1.047 ms | 154.20 µs |

tree側は旧スナップショットより遅くなっている（`fib_20` 14.982 ms → 28.349 ms）。原因はAUD-037の呼び出し時self-bindingによる`Value::Fn`の複製で、AUD-040で解消した（次節）。この表のtree列はAUD-040前の値である。

### 2026-08-27 AUD-040後の実行フェーズ

`Value::Fn`をRc共有にした後の`execute`フェーズ（`--sample-size 20 --measurement-time 2`、上の表とは測定設定・マシン状態が異なるため直接比較しない）。

| workload（execute） | tree | VM |
|---|---:|---:|
| `fib_20` | 7.599 ms | 4.995 ms |
| `dict_500` | 9.030 ms | 8.826 ms |
| `fstr_300` | 84.79 µs | 127.53 µs |
| `loop_5000` | 799.14 µs | 1.117 ms |
| `while_5000` | 942.73 µs | 922.95 µs |
| `higher_order_200` | 103.66 µs | 72.75 µs |
| `closure_def_200` | 144.99 µs | 112.19 µs |

修正の効果は同一マシン・連続実行のA/Bで確認した（`fib(22)`を7回実行した最小値）。

| workload | `93b7606` | AUD-040後 | 比 |
|---|---:|---:|---:|
| tree `fib(22)` | 64.5 ms | 21.2 ms | 0.33 |
| tree closureループ定義 | 10.0 ms | 7.8 ms | 0.78 |
| VM `fib(22)` | 14.1 ms | 13.2 ms | 0.94（誤差。`Value::Fn`はVMでは未使用） |

実時間はマシン状態に依存するため、確保バイト数も併記する。body 2文の関数を300回呼ぶと 909,821バイト → 452,645バイト、body 100文との比は 15.89 → 1.06 になった。

### 2026-08-28 追加監査スナップショット

対象はcommit `06dae8e`、aarch64 Linux、`rustc 1.97.1`。`cargo fmt --check`、Clippy `--all-targets -- -D warnings`、`cargo test`はすべて成功した。単体テスト139件はlib/binで重複実行され（AUD-039）、統合テストは156件、スケーリングテストは2件である。

この状態から、既存テストが捕捉していない不具合として **AUD-043**（トップレベル`return`の未検証）を最小入力で再現した。トップレベル`return`のfixtureもREPL回帰テストも存在しないため、緑のCIでは検出できない。`break` / `continue`の文脈エラーは両engineで期待どおり報告される。

文書照合では **AUD-044 / AUD-045** を追加した。Kubernetes / Helm / Kustomize / Dockerの資材は存在せず、追跡対象の設定ファイルは`.github/workflows/ci.yml`と`Cargo.toml`だけである。Windows固有挙動とsymlinkのTOCTOUは、従来どおり実環境確認が必要な項目として据え置く。

### 2026-08-28 AUD-042の測定

対象はaarch64 Linux、`rustc 1.97.1`。`tests/scaling.rs`のグローバルアロケータで、確保量と生存量（確保 - 解放）を測った。実時間ではないため測定は決定的である。

関数ローカルのリストへクロージャを溜めて関数を抜けた後の生存量。

| クロージャ数 | 修正前 | 修正後 | VM |
|---|---:|---:|---:|
| 200 | 173,596 バイト | 0 バイト | 0 バイト |
| 400 | 345,796 バイト | 0 バイト | 0 バイト |

クロージャを2,000回定義する際の確保量（定義のみ、呼び出しなし）。

| 可視binding | 修正前 | 修正後 |
|---|---:|---:|
| 5個 | 2,774,064 バイト | 2,538,064 バイト |
| 100個 | 19,640,166 バイト | 2,560,166 バイト |
| 比 | 7.08 | 1.01 |

同じ関数を2,000回呼ぶ際の確保量は、global 5個と100個の比が7.66→3.67へ下がったが比例は残った。捕捉範囲ではなく`push_call_frame`のglobal複製が原因で、AUD-046として分離し別途修正した（次節）。VMは同条件で1.02〜1.03と影響を受けない。

### 2026-08-28 AUD-046の測定

同条件での、同じ関数を2,000回呼ぶ際の確保量。

| top-level binding | 修正前 | 修正後 |
|---|---:|---:|
| 5個 | 3,321,072 バイト | 2,327,072 バイト |
| 100個 | 12,203,174 バイト | 2,349,174 バイト |
| 比 | 3.67 | 1.01 |

`fib(20)`の確保量は23,164,422→15,832,425バイト。releaseビルドの実時間（3回の最小値）は`fib(22)`が22→17 ms、global 100個で20,000回呼ぶ例が56→8 msだった。VMは確保量・比とも変化しない。

### 追加監査後の推奨改修順

1. **停止性・ホスト安定性:** AUD-026 / AUD-027 / AUD-028 / AUD-043は完了。timeout・深度境界・host abortなしを継続検証する。library利用者が不正な`Chunk`をVMへ渡す経路の防御はAUD-023で続ける。
2. **誤実行・安全境界:** AUD-012 / AUD-013 / AUD-029 / AUD-031 / AUD-037は完了。意味論選択が必要なAUD-014 / AUD-030は仕様決定後に実装する。AUD-043はトップレベル`return`をパースエラーへ統一し、panic・無言終了・import時のengine差を同時に解消した。
3. **品質基盤:** AUD-022のharness整備、AUD-038の測定分離、AUD-040のtree呼び出しコスト、AUD-042のclosure捕捉範囲、AUD-046のglobal複製は完了。次はAUD-041（VMのコレクション読み取り）を扱い、その後にP2境界とfuzzを拡充する。
4. **文書・配布:** AUD-021に続き、AUD-044でREADMEと規範仕様の古い記述を除き、AUD-045で実行手順とtoolchain下限を明示する。

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

## 設計方針: 言語中核とホスト機能の境界

### 目標: 小さな言語中核 + 明示的なhost capability

言語中核には、文字列、数値、List/Dict、型変換など、外部状態へ触れない純粋な計算を置く。
filesystem、環境変数、時刻、標準入出力、process、network、database、メール、業務操作などの外部効果は、原則としてホストが実行単位で明示的に付与するcapabilityまたはhost functionとして提供する。

「自プロセス + OSで完結するか」ではなく、「外部状態を観測・変更するか」「権限、予算、監査の対象になるか」を境界の判断基準とする。

### 現行実装からの移行

現行のbuiltinはCLI中心の学習用設計としてOS機能へ直接接続している。次の表は現在の挙動と目標を区別する。

| カテゴリ | 現在 | 目標 |
|---|---|---|
| 文字列・数値・List/Dict操作 | core builtin | core builtinを維持 |
| filesystem | `std::fs`へ直接接続 | read/write/delete等を分離した実行単位capability |
| 環境変数・引数 | process環境・argvへ直接接続 | hostが許可した値のsnapshotを注入 |
| 時刻 | OS clockを直接参照 | clock capabilityとして注入 |
| stdin/stdout | processのstreamへ直接接続 | host提供のinput/outputと出力量予算を使用 |
| `exit` | host processを終了 | processを終了せず構造化`Outcome`を返す |
| HTTP・DB・メール | 未実装 | coreへ追加せずhost function/moduleとして提供 |
| 業務操作 | 登録手段なし | host function registryから明示的に公開 |

この移行は、stable embedding APIと実行contextを先に設計してから行う。既存builtinをただ削除するのではなく、CLIが必要なcapabilityを明示的に付与する構造へ変え、同じengineを組み込み用途でも利用できるようにする。

現行の`import`はTsumugi sourceを読み込む機能であり、native host moduleやhost functionを登録する拡張境界ではない。host extension APIが成立するまで、「HTTPやDBを外部moduleで提供する器が完成した」とは扱わない。

### この境界を採用する理由

1. **最小権限** — scriptごとに必要な操作だけを付与し、ambient authorityを避けられる
2. **資源制御** — host callの時間、入出力量、同時実行数を実行予算へ含められる
3. **監査可能性** — 外部効果の許可、拒否、引数、結果をhost boundaryで観測できる
4. **予測可能性** — clock、env、filesystem等を注入し、同じ入力に対する再現性を高められる
5. **依存の分離** — HTTP clientやDB driverを言語本体へ固定せず、ホストが用途に応じて選べる
6. **テスト容易性** — 実OSや外部serviceを使わず、fake capabilityで境界動作を検証できる

### 外部機能を追加する順序

1. `Engine` / `ExecutionContext` / `ExecutionOutcome`を定義する
2. host function registryとcapability policyを定義する
3. clock、env、stdio、filesystem、process操作をhost境界へ移す
4. budget、deadline、cancellation、audit eventをhost callへ伝播する
5. 具体的なユースケースができた段階で、HTTPやDB等をホスト側adapterとして実装する

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
| ベンチマーク | `criterion` で parse / compile / execute / end_to_end を分離計測 | ✅ 完了（AUD-038） |
| 計算量オーダーの回帰ゲート | `tests/scaling.rs` が確保バイト数で `for` の線形性（AUD-038）、呼び出しコストのbody長非依存（AUD-040）、クロージャ定義コストの可視binding非依存（AUD-042）、呼び出しコストのtop-level binding非依存（AUD-046）を検査し、生存量でクロージャの解放（AUD-042）を検査（実時間に依存しない） | ✅ 完了 |

### コード構造

| 項目 | 詳細 | 状態 |
|---|---|---|
| `eval.rs` の `exec_stmt` 分割 | 巨大 match を独立メソッドに分離 | 未着手 |
| `Env::functions` の廃止 | 関数を変数として統合しスコープルールを一本化 | ✅ 完了 |

### 実行安全性

| 項目 | 詳細 | 状態 |
|---|---|---|
| ユーザー関数のコール深度制限 | 関数再帰を128フレームでRust実stack overflow前にエラー化 | ✅ 完了（境界差はAUD-017で継続） |
| 関数外 `return` の拒否 | 関数本体の外の`return`をパース時にエラー化し、VM REPLのhost panicと無言終了を防止 | ✅ 完了（AUD-043） |
| 構文・AST深度制限 | Parser生成時とCompiler/Evaluator入口でAST深度256を検査。nested f-stringにも親深度を継承 | ✅ 完了（AUD-027） |
| import chain深度制限 | rootを除くactive import chainをtree/VMとも128に制限 | ✅ 完了（AUD-028） |
| サンドボックスの `OnceLock` テスタビリティ | テスト時に環境変数を切り替え可能な設計にする | 未着手 |
| サンドボックスの中間シンボリックリンク迂回修正 | 新規書き込み時に親ディレクトリを `canonicalize()` してからチェック | ✅ 完了 |
| 整数オーバーフローのエラー化 | `checked_add` 等に置き換え、release ビルドでもサイレントラップを防止 | ✅ 完了 |
| メモリ DoS 対策（コレクションサイズ上限） | List/Dictの生成・拡張、List生成builtin、反復変換に上限ガード。`TSUMUGI_MAX_COLLECTION_SIZE` で変更可能 | ✅ 完了（総heap quotaは別課題） |
| ファジングテスト導入 | `cargo-fuzz` でレキサー/パーサー/評価器に無作為入力 | 未着手 |
| VM の `unwrap()` 除去 | コンパイラバグ時にパニックではなく構造化エラーを返す | 未着手 |
| エラー種別の enum 化 | `classify_runtime_error()` の `contains()` 判定を `ErrorKind` enum に移行 | ✅ 完了 |

## 検討事項: HTTPアクセス機能

### 方針: 言語中核へ組み込まない

HTTPはnetwork access、DNS、TLS、認証、redirect、response size、timeoutなど、権限・資源・監査の境界を伴う。特定のHTTP clientをTsumugi中核へ組み込まず、host functionまたはhost moduleとして提供する。

例えばホストが`http_get`を公開する場合も、scriptから任意のnetwork accessを許可するのではなく、次をhost policyで制御する。

- 接続先scheme・host・portのallow-list
- request/response byte上限
- connect/read/total deadline
- redirect回数
- 同時実行数とrate limit
- cancellation
- request開始、許可・拒否、終了理由のaudit event
- credentialとresponse bodyのredaction

具体的なRust HTTP clientと認証方式はホストアプリケーションが選択する。これにより、HTTP dependencyの更新周期を言語本体から分離し、Tsumugiを利用しないホストへ不要な依存を持ち込まない。

### 実装タイミング

- stable embedding API、host function registry、capability、budget、audit sinkの後に実装する
- 具体的な業務ユースケースと必要な権限境界が明確になった段階でhost adapterとして追加する
- core builtin化は既定の選択肢とせず、必要性と安全境界を改めて設計レビューする
