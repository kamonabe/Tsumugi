# Tsumugi — ロードマップ

最終更新: 2026-09-20

## 現在の作業状況（サマリ）

> このセクションは「今どこにいて、次に何をやるか」を1か所に集約した早見表である。詳細・根拠は各 AUD / REV 項目と設計正本を参照する。**設計確定は実装完了ではない**（受入gate未達なら未完了扱い）。open 項目を増減したらこの表も更新する。

### 現在地

- **フェーズ:** 意味論基盤（下記「設計sliceに沿う推奨実装順」のステップ2）を進行中。ステップ2の完了済み項目は AUD-050/017・049・019・047・048・016・024・034 と bytecode検証・API封印（REV-006/004/005/018）。境界挙動ステップ1は AUD-036/018/033・REV-003 完了で完結（残りは AUD-036 の構造化`Exited`のみで、これは REV-023 と同基盤のためステップ3）。ステップ3の包括budget（REV-015/001/023）を進行中で、Slice 1 と Slice 2 の string accounting・source/import accounting・heap accounting 基盤（`AllocationLedger`・§5.1 論理サイズ）・collection（`List`/`Dict`）の per-drop release（`Rc<Tracked<T>>` で生成時課金＋最後の参照 drop で release、tree/VM 両対応）サブスライスと String body の per-drop release（PR-b、`Value::Str` を `Rc<Tracked<String>>` 化）と cell・tree/VM 関数 instance の per-drop release（PR-c、`SharedValue` を `Rc<Tracked<RefCell<Value>>>` 化し `Value::Fn`/`VmFn` に header token を持たせる）まで実装済み。さらに AST / bytecode chunk / imported module record / rollback journal の per-drop 追跡（PR-d、所有構造側が `HeapToken = Tracked<()>` トークンで §5.1 論理サイズを課金し drop で release。§5.3「AST または bytecode」に従い tree は AST・VM は bytecode chunk を課金、import record と rollback journal entry は両 engine）と string リテラル/連結/f-string 経路の課金（生成点で `track_result` を通し cumulative + live heap を課金、tree/VM 両対応）と I-O accounting（input / output / host call の count + bytes を reserve/commit/refund で課金。stdio を host call として co-charge し、host call 対象は filesystem read/write と stdio に限る。tree/VM 両対応）まで実装済みで、これで Slice 2 は完了。次は Slice 3（explicit continuation）で、実行の形を変える大改修のため PR-a（§9 公開 state-machine surface + poll-to-terminal core、再帰は温存）→ PR-b（statement / block / loop の明示 frame）→ PR-c（関数呼び出し・try handler の明示 frame）→ PR-d（slice fuel + yield）へ分割する（詳細は [実行制御仕様](execution-control.md) 第14節 Slice 3）。PR-a（§9 公開型 `ExecutionHandle`/`ExecutionState`/`PollSlice`/`PollResult`/`HandleError`/`ExecutionRequest`・拡張 `ExecutionOutcome` を `engine.rs`/`lib.rs` へ追加し、`poll` は `run_phased` を terminal まで回す。`Engine::execute` は poll-to-terminal 互換 wrapper。再帰は温存）実装済み。PR-b（statement / block / loop の明示 frame stack）も実装済み。tree evaluator の `exec_program`/`exec_block`/`exec_scoped_block` 再帰と `while`/`for` の Rust ループを、ヒープ上の `Vec<Frame>` + cursor + `drive_body` driver ループへ置換した。`exec_stmt` は 1 文の葉だけを実行して `StmtStep`（`Val`/`Flow`/`Enter`）を返し、複合文（`if`/`while`/`for`/`try`）は子 `Frame` を組み立てて driver へ渡す。`EvalResult`（`Return`/`Break`/`Continue`）による制御フローは Rust スタック巻き戻しではなく `unwind_flow` が frame stack を明示 unwind する。ループ本体は反復ごとにスコープ push/pop、末尾 `count_step` は controller frame の `pending_step` で従来順序を保ち、`for` は反復元 collection を frame が保持して live-heap 寿命を維持する。`try` 本体は `Try` frame で実行し捕捉エラーは `handle_error` が catch 用 `Scoped` frame へ差し替える。各 frame の push で §5.1 continuation_frame（96 + 32×locals）heap を課金し pop/unwind/catch 切替で release する。式（`eval_expr`）は当面 Rust 再帰のまま（式は bounded）。さらに PR-c（関数呼び出し・try handler の明示 frame）を実装済み。`eval_call` と callback の `call_fn_value` を、VM の `CallFrame` をミラーする `FrameKind::Call { saved_scopes }` を積む形へ変換し、PR-b の driver ループを共通 `run_driver(frames)` へ抽出したうえで、関数本体を `drive_call_body(def, saved_scopes)` で駆動する。`drive_call_body` は呼び出す関数の `Rc<FnDef>` をローカルに保持してその `&def.body` を Call frame の `stmts`（`'p`）として渡し、本体・子 frame が呼び出し全体を通じて生きる AST を借用できるようにする（VM が `CallFrame` に `Rc<Chunk>` を持つのと同型で、frame の自己参照を避ける）。スコープ退避（`env.push_call_frame`）・self-binding・引数束縛・call trace は呼び出し元が用意し、`saved_scopes` を Call frame へ預ける。Call frame の pop が全終了経路（正常完了・`return`・エラー unwind）で `env.pop_call_frame` と call trace の巻き戻しを一元化するため、呼び出し元側の手動 `pop_call_frame` を廃止した。`return` は `unwind_flow` が最も近い Call frame まで畳んで値を produce し、`break`/`continue` は Call 境界を越えられずそのまま surface する。エラー時の trace は発生時点の `call_stack` を snapshot する `attach_trace`（`with_trace` の idempotency で最深トレースを保持）で付加する。`try` handler は PR-b の `FrameKind::Try` が既に VM の `TryHandler` をミラーしており、Call frame 化で呼び出し境界を越えた try/catch の分離が再入で自然に成立する。観測挙動は不変（既存の全テスト緑、tree/VM の trace・catch・parity を含む）。再入モデルのため呼び出しごとに 1 段の Rust フレームと `main.rs` の 8 MiB 実行スレッド・呼び出し深度上限 128 は維持する（式評価が Rust 再帰である以上、呼び出しごとの Rust フレーム除去は範囲外）。さらに PR-d（slice fuel + yield）を PR-d-1 / PR-d-2 で実装済みで、Slice 3（explicit continuation）は完了した。PR-d-1 は continuation を Evaluator 所有の `'static` 永続 frame stack へ移す（AST ブロック本体・`FnDef.body`・`Lambda.body` を `Rc<[Stmt]>`＝`ast::Block` へ、`Frame`/`FrameKind` から借用を廃し、`run_driver(stop_depth)` が `self.frames` を回す。観測挙動不変）。PR-d-2 は slice-fuel accounting を足し、`count_step` が total fuel 課金に加え `slice_fuel_used` を数え、driver が文/反復境界で `PollSlice::max_fuel` 到達時に `Yielded(SliceFuelExhausted)` を返す（yield は式の途中では起きず、関数呼び出し・callback・同期実行 `run`/`run_phased`/`run_repl_submission` は `can_yield=false` で完了まで回す）。`Engine::poll` は初回 `begin_execution`（link のみ）→ 以降 `run_slice(max_fuel)` で永続 continuation を 1 slice ぶん resume し、terminal で transaction commit/rollback と未完了 module の解決マーカー巻き戻しを行う。`ExplicitYield` は enum のみで trigger 未配線（Slice 4）。小 slice の複数 poll が単一 slice と同じ terminal・同じ total fuel へ到達することを `tests/engine_api.rs` で固定（597 tests green）。次は残る包括budget（REV-001・REV-023・deadline/cancel/transaction は Slice 4 以降）。並行して Phase 1（embedding）へ着手し、スライス E1（[組み込みAPI仕様](embedding-api.md) 第3・8節の識別子・設定・エラー・terminal outcome 型と `EngineBuilder`）を `src/embedding.rs` に実装済み。`Backend`/`LanguageRevision`（現行 `V0_19`）/`EngineId`/`SourceId`/`SourceHash`（E1 は 32 byte 器のみ、SHA-256 計算は E2）/`ExecutionId`/`EngineConfig`+`Default`/`ConfigError`（E1 variant のみ、capability/callable 系は E7）/`EngineBuilder`（`allow_experimental_backend` で `VmExperimental` を gate。`host_functions` は E7 へ後回し）/`Engine`（`builder`/`id`/`config`）と、`ExecutionError`（`code` は `ErrorKind` を再利用）/`HostError`/`HostErrorCode`/`TraceFrame`/`ExecutionOutcome`（`#[non_exhaustive]`、Phase 1 到達 variant `Completed`/`RuntimeError`/`Cancelled`/`InternalFailure` のみ、`usage` は Phase 3）を含む。alpha facade（`engine`）と名前が衝突する `Engine`/`ExecutionOutcome`/`TraceFrame` は crate root で `EmbeddingEngine`/`EmbeddingOutcome`/`EmbeddingTraceFrame` として別名公開し、統合は E10 へ委ねる。secret-free `Debug`・`Send+Sync`・constructor 検証の unit test を追加。続けて E2（`Engine::compile`/`link`・`CompiledScript`/`LinkedScript`・import なし link・byte-level hash）を実装済み。`compile` は root source を lex/parse で検証し（import 先・OS・host は呼ばない）、生 UTF-8 bytes の SHA-256 を `source_hash` にする。`link` は engine ID→revision→backend の順に検証し、import が1件でもあれば `LinkError::FeatureUnavailable { feature: "module_resolver" }`、import 0 件なら空 graph の `LinkedScript` を作る。§5.1 の byte-level hash（domain separator `TSUMUGI-IMPORT-GRAPH-V1\0` / `TSUMUGI-LINKED-SCRIPT-V1\0`、u64 big-endian、`str = len || bytes`）で `graph_hash` / `script_hash` を計算する。SHA-256 は外部クレートを増やさず self-contained 実装（FIPS 180-4）を置き、既知テストベクタで検証。`retain_source` 既定は false。`CompiledScript`/`LinkedScript` は `Send+Sync` 契約（EMB-AT-02）を満たすため、`!Send`/`!Sync` な現行 AST（`Block=Rc<[Stmt]>`、Slice 3 PR-d）を handle へ保持せず、identity・hash・`has_imports`・任意保持 source のみを持つ（runnable AST の駆動は E3 で再 parse または AST の `Send+Sync` 化と併せて扱う）。EMB-AT-13 の golden hash を含む unit test を追加。続けて E3（tree backend adapter・実行入口）を実装済み。`Engine::run(&LinkedScript, &mut ExecutionContext, ExecutionRequest)` が tree 評価器の既存意味論で同期実行し、Phase 1 到達 outcome（`Completed` / `RuntimeError` / `InternalFailure`）を返す。runnable AST は E2 で先送りした `Send+Sync` 問題を **案 A** で解決した——`CompiledScript` は AST を持たず、実行入口が実行スレッド上で保持 source を再 parse して `Program` を再構築する（`retain_source=true` が実行の前提。設計判断の根拠と却下した案 B は embedding-api §4.1 に記録）。`ExecutionContext`（`!Send+!Sync`、`ExecutionContext::new(&Engine)`）と最小 `ExecutionRequest`（script 引数 snapshot）を追加し、`TsumugiError::Runtime` を `ExecutionError{code:ErrorKind, safe_message, line, trace}` へ写す。crate root では alpha facade と衝突する `ExecutionContext`/`ExecutionRequest` を `EmbeddingContext`/`EmbeddingRequest` として別名公開。Completed / RuntimeError（code・call trace）/ InternalFailure（source 非保持・engine 不一致）/ context 再利用の unit test を追加。続けて E4（Context/transaction 縦切り）を実装済み。`Engine::run` を transaction 全面適用（`transactional=true`）へ切り替え、tree 評価器の `begin_execution`/`run_slice` が持つ AUD-024 journal を通す。これにより `Completed` は変更した全 language-state（binding・cell・List/Dict・function・module marker）を commit し、`RuntimeError` は execution 開始時点へ rollback する（§10 規則5）ので、未捕捉エラーで終わった実行の副作用は次実行へ残らない（catch 済みで正常完了した場合は commit）。terminal 後の再利用可否は §10 規則4に従い、`InternalFailure` だけが context を poison して以後の実行・状態操作を拒否し、他 terminal は commit/rollback 完了後にそのまま再利用できる。poison 判定は全 InternalFailure 経路を単一内部関数（`run_inner`/`run_transactional`）へ閉じて一元化し、precondition guard（既 poison / running 再入）はそこに含めない。`ContextError`{`Busy`/`Poisoned`}・`ExecutionContext::is_poisoned`/`clear_user_state`（idle 時のみ user state 破棄）・再入防止の `running` フラグを追加し、crate root で `ContextError` を re-export。rollback/commit・catch 済み commit・poison と再利用拒否・clear の unit test（EMB-AT-06/07/08 の Phase 1 範囲）を追加。続けて E5（terminal channel）を実装済み。`ExecutionRequest::pre_cancelled()` を追加し、`Engine::run` は最初の poll より前に cancel 済みなら script 命令を1つも実行せず `ExecutionOutcome::Cancelled` を返す（EMB-AT-09/12 の Phase 1 範囲。"pre-cancel は命令0"）。命令0なので language-state は不変で context は poison されず再利用できる。terminal channel の他 variant（`Completed`/`RuntimeError`/`InternalFailure`）は E1〜E4 で既に構造化 outcome だけで返しており、library は `std::process::exit` を呼ばず host process を継続する。続けて E6（panic 隔離）を実装済み。lexer/parser/linker/評価器の unwind panic を各 host boundary で `catch_unwind(AssertUnwindSafe(...))`（共通 helper `catch_host_unwind`）で捕捉し、compile 中の panic は `CompileDiagnosticCode::InternalFault`、link 中は新設 `LinkError::InternalFailure { fault_id, safe_message }`、handle 作成後の run/poll 中は terminal `ExecutionOutcome::InternalFailure` へ写す（第11節 規則1/2）。いずれも相関用の非ゼロ fault ID を発番し、panic payload / native backtrace は公開エラーへ含めない（`internal_fault_message` は fault ID だけを載せる secret-free メッセージ、規則3）。run 中の panic 経路も他 InternalFailure と同様に context を poison し（規則4）、`running` フラグは panic 後も必ず降ろして再入検査の誤検出を防ぐ。`catch_host_unwind` の fault ID 発番・secret-free メッセージ・run 境界 panic→InternalFailure+poison・pre-cancel の命令0/非 poison/precondition 優先を unit test で固定（embedding lib 41 tests green）。`panic=abort`・OOM・stack overflow・FFI UB は捕捉不能で最終防御は別 process 隔離（脅威モデル）。**Phase 1 embedding は E1〜E6 完了**。続けて E8a を実装済み。CLI の tree file/stdin 実行のうち import なし root を embedding Engine API（`compile`→`link`→`run`）だけへ統合し、`EmbeddingRequest::with_arguments` で引数転送・`ExecutionOutcome`→exit code 変換（第12節: Completed→0 / RuntimeError→1 / Cancelled→130 / InternalFailure→70、compile/link error→1）を CLI 側の formatter（`format_execution_error` が現行 `TsumugiError` Display と byte 一致）で行う。import を含む root は Phase 1 embedding の `link` が `FeatureUnavailable { feature: "module_resolver" }` で拒否するため、その LinkError を検出して alpha facade（`ModuleLoader` 経由の `run_source_alpha`）へフォールバックする。REPL は入力間の状態継続と import 解決が必要なため alpha facade のまま維持する。import・REPL の Engine API 統合は import resolver が入る E7（Phase 2）、VM 経路の Engine API 統合は E9（Phase 5）で行う。**Phase 1 embedding は E1〜E6・E8a（Phase 1 範囲）完了**。続けて Phase 2 capability の slice C1 を実装済み。`src/capability.rs` に deny-by-default な `CapabilitySet`（`empty()` が唯一の library 既定値で全 8 authority を拒否・`builder`/`id`/`contains`）と `CapabilitySetBuilder`（authority 別 setter、同 kind 二重設定は `ConfigError::DuplicateCapability`、host function grant のみ別 ID 複数可、`build` で freeze）・`CapabilityKind`（Environment/Clock/Stdin/Stdout/Filesystem/ProcessExit/ModuleResolver/HostFunction）・`CapabilitySetId`（§3.1/§8.6 の byte-level encoding、domain `TSUMUGI-CAPSET-V1`、`embedding::hash::sha256` を crate 内公開して再利用）を追加した。adapter trait（`Clock`/`Input`/`Output`/`ModuleResolver` は C1 では `policy_id` のみ、`DirectoryHandle` は `policy_id`+`symlink_policy`）と、set 格納・ID 計算に必要な値型（`EnvironmentSnapshot`/`EnvironmentValue`/`DataClassification`、`FilesystemCapability`/`FilesystemRoot`/`MountName`/`FsOperation`/`SymlinkPolicy`、`ProcessExit`、`HostFunctionId`）を用意し、`EnvironmentValue`/`EnvironmentSnapshot`/`CapabilitySet` の `Debug` は値本文を出さない secret-free 実装にした。`ConfigError` へ Phase 2 variant（`DuplicateCapability` を C1 で使用、`DuplicateCallableName`/`InvalidDescriptor`/`InvalidFilesystemPolicy` を定義。filesystem 検証で一部使用）を追加。CAP-AT-01/02/03/28 相当（empty で全 authority deny、単一 grant 分離、clone の ID/権限一致、empty set ID の golden bytes、root 順に依存しない filesystem ID、secret-free `Debug`）の unit test を追加した。adapter の実行時メソッド接続と builtin/CLI 配線は後続 slice。続けて C2（`CallableCatalog` 統合、AUD-049 / CAP-AT-20）を実装済み。単一 `BuiltinSpec` registry（`src/builtin_registry.rs`）は既に存在し tree/VM/compiler が導出しているため（§13.8 の名前一覧撤去・`__pop_update` 非到達・contract test は既済）、C2 の残作業だった CAP-AT-20 の「generated docs 完全一致」を実装した。registry から生成 docs を描画する `render_reference()`（`Arity::describe`/`Execution::label` 付き）を追加し、`src/bin/gen_builtins_doc.rs` で `docs/generated/builtins.md` を生成、`tests/builtin_registry_contract.rs` に生成物と registry の byte 一致（ドリフト検出）・公開名/ID 重複なし・全 entry の行存在の contract test を追加した。§13 概念モデルの追加フィールド（`evaluation`/`mutation_target`/`lowering`/`handler`）と backend の残存 `match name`/arity 二重定義の吸収は CAP-AT-20 の gate ではなく §14 品質改善（14.1/14.2 dispatch 分割）扱いのため C2 では触れない。続けて C3（Environment/Clock）を実装済み。`env()`/`now()` を PureCore dispatch から context builtin へ移し、frozen `CapabilitySet` の Environment snapshot / Clock adapter を consult する共有ロジック（`resolve_env`/`resolve_now`、authority を引数・型検査より後に確定する `resolve_exit` と同じ precedence）を tree/VM 両 engine へ配線した。未 grant は adapter/trait call 0 のまま catch 可能な `capability` エラー（未捕捉→RuntimeError）で、grant 済みは env が snapshot だけを引き（missing key は null）、now は Clock adapter の `now_utc` を Unix 秒へ写す。`Clock` trait へ C3 最小形の `now_utc(&self) -> SystemTime`（`CapabilityCallContext`/`AdapterError` を取る第6節最終形は C4/C8 でそれらの型を導入するスライスへ委譲）と `SystemClock`（OS clock）・`FixedClock`（決定的 test/host utility）を追加。ambient 経路の唯一の process env 読み取りを `ambient_environment_snapshot()`（legacy `TSUMUGI_ENV_ALLOW` allow-list + `TSUMUGI_` 保護を適用した visible key を snapshot 化）へ集約し、`ambient_compat()` が Environment（この snapshot）と Clock（`SystemClock`）も grant するようにして CLI/REPL/alpha の観測挙動を保った（core builtin の ambient read 0）。CAP-AT-05（env snapshot 固定・process env 変更から隔離・missing=null・未 grant catch 可能）/ CAP-AT-06（fixed clock 決定的・未 grant catch 可能）相当の unit test を tree（embedding `Engine::run`）と VM（`exec_builtin` 直呼び）の両方へ追加。続けて C4（Stdin/Stdout）を実装済み。`input()`/`print` を frozen `CapabilitySet` の Stdin/Stdout adapter へ配線し、`Input`/`Output` trait へ C4 最小形（`read_line`→`InputLine`{`Line`/`Eof`}・`write_all`/`flush`。`CapabilityCallContext`/`ReadLimit`/有限 meter は Phase 3 へ委譲）と `AdapterError`{`Host`} を追加、OS 直読み/直書きを `SystemInput`/`SystemOutput` へ集約（従来 `write_stdout_line` の broken pipe 非 panic 挙動＝AUD-035 は adapter が引き継ぎ、`write_stdout_line` は撤去）。共有 `resolve_input`/`resolve_print` が authority を先に consult し、未 grant は adapter call 0 の catch 可能 `capability` エラー、`input` の EOF は null、host 失敗は catch 可能な canonical `host` エラー（新 `ErrorKind::Host`・`host_adapter_failed`、null/false へ潰さない）。budget の `charge_output`/`charge_input` は従来位置のまま。tree は `print`/`input` builtin、VM は `OpCode::Print`/`exec_builtin` を配線し、`ambient_compat()` に Stdin/Stdout を追加して CLI/REPL/alpha/VM の観測挙動を保った。CAP-AT-07/08 相当を tree（embedding）と VM（`exec_builtin`＋`OpCode::Print` chunk）で test。続けて C5（Filesystem adapter）に着手。規模が大きくセキュリティ上デリケートなため C5-a（path/mount routing 型 + trait 拡張）→ C5-b（secure OS adapter + symlink policy）→ C5-c（builtin 移設 + ambient + oracle）へ分割し、Phase 2 最小形（`CapabilityCallContext` なし）で進める（import resolver は sandbox のまま据え置き、C6 で移行）。C5-a を実装済み: `RelativePath`/`PathError`/`FilesystemTarget::parse`（`@MOUNT/comp` syntax・unqualified は `default` mount・完全一致 routing・絶対/`.`/`..`/空 component/backslash/drive/UNC/NUL 拒否、CAP-AT-10/29 の lexical 部分）、`DirectoryHandle` trait を open_file/create_dir/metadata/list/remove/rename へ拡張＋新 `FileHandle`（read_to_end/write_all/metadata、Phase 2 最小形で context/`max_bytes`/`max_entries` は器のみ）、value 型（`WriteMode`/`OpenFileRequest`/`EntryKind`/`PublicMetadata`/`DirectoryEntry`/`RemoveKind`）、`AdapterError::SecureResolutionUnsupported`、`FilesystemCapability::root`（完全一致 lookup）を追加。builtin/adapter 未配線のため観測挙動は不変。続けて C5-b（secure OS adapter）を実装済み: `std` のみで OS filesystem を backing にする `OsDirectoryHandle`/`OsFileHandle` を追加した。Unix では component ごとに `symlink_metadata`（lstat）で中間 component を検証し、`SymlinkPolicy` に従って symlink を拒否（`DenyAll`）または root 内へ拘束（`FollowWithinRoot`/`OperateOnFinalEntry` は `canonicalize`＋root 配下判定で拘束）し、final entry は open 前に lstat して symlink を拒否する（`O_NOFOLLOW` の flag 値は Linux で arch 依存＝libc 非依存では確定できないため、flag ではなく明示 lstat で検出する設計にした）。metadata（`follow_final` と root 拘束）・list（final symlink 拒否・非 UTF-8 名 skip）・create_dir・remove（`FileOrSymlink`/`EmptyDirectory`）・rename（同一 policy 相関 ID 内のみ、異種 backing は `SecureResolutionUnsupported`）を配線し、非 Unix は文字列 prefix へ fallback せず `SecureResolutionUnsupported` で fail closed する（契約3）。`PublicMetadata` は時刻・owner・host path を出さず、`OsDirectoryHandle` の `Debug` も host path を隠す（存在 oracle 防止）。TOCTOU race は AUD-020 / CAP-AT-11 stress gate へ委ねる。CAP-AT-11/12 相当の unit test（read/write roundtrip・create/list/remove・DenyAll の中間/final symlink 拒否・FollowWithinRoot の root 内追従と root 外拒否・CreateNew 既存拒否・OperateOnFinalEntry の lstat/delete・空 path=root・not-found の host error・Debug secret-free）を `#[cfg(unix)]` で追加。builtin 配線は C5-c。次は C5-c（builtin 移設 + ambient + oracle）。
- **仕様 revision:** language-spec 0.19（実装版 package は 0.1.0。番号体系は別管理）。
- **完了の中心:** 2026-08-26 深層監査（AUD-001〜050）の大半は実装済み。残る open は下表のとおり。
- **新規入力:** 2026-09-07 詳細レビュー（REV-001〜025）は全件が実装バックログ（設計は6件が §17 で確定、他は既存正本を参照）。

### 次に着手すべき順（先頭ほど優先）

「設計sliceに沿う推奨実装順」の未完了部分を抜き出したもの。詳細は同節を参照。

1. **境界挙動（意味論基盤の残り）:** ~~AUD-036 のchecked変換~~（✅ Float→Int/file_size完了、仕様revision 0.17）→ ~~AUD-018 のE8a（CLI script引数）~~（✅ 完了、仕様revision 0.18。capability profile/optionsはE8bで別追跡）→ ~~AUD-033（未完結REPL入力のEOF診断）~~（✅ 完了、tree/VM共有の `finish_repl_at_eof`）→ ~~REV-003（数値厳密比較）~~（✅ 完了、共通 `NumericOrder`・仕様revision 0.19）。AUD-036 の構造化`Exited`は REV-023 と同基盤のためステップ3へ移す。ステップ1はこれで完結。
2. ~~**bytecode検証・API封印（基盤・境界挙動より前に置く）:** REV-006（`VerifiedChunk`/verifier + per-instruction step 課金。§17.7 で実装詳細確定）を軸に、同一マイルストーンで REV-004（`patch_jump` fallible化）・REV-005（`MakeClosure` capture記述子化）・REV-018（internal module封印）。停止性はper-instruction課金（verifier非依存）で担保し、VM入口を `VerifiedChunk` へ限定する。~~（✅ 完了。`src/verifier.rs`の`VerifiedChunk`/`verify`（V1〜V9）、VMの全命令dispatch前per-instruction課金、`MakeClosure(proto_index)`+`FunctionPrototype`/`CaptureDesc`、`patch_jump`のfallible化、`unstable-bytecode` featureでのraw module封印。観測挙動はstep到達点以外不変）
3. **包括budget（P0 基盤）:** REV-015（source/string/heap/I-O budget・deadline・cancel）に REV-001（共有DAGの指数時間/出力）の受入条件を含める。REV-023（`exit()`のprocess終了廃止）も同基盤。**進行中**: [実行制御仕様](execution-control.md) 第14節 Slice 1（budget型・checked reserve/commit・固定優先順位・fake clock・共有 `BudgetLedger`）を `src/budget.rs` に実装済み。既存 step/collection 検査を ledger 経由へ一本化し、`builtin_core` の process-global `OnceLock` 上限を廃止。続けて Slice 2 の **string accounting サブスライス**（per-item `SingleStringBytes`・cumulative `StringAllocations`/`StringBytes` を `charge_string`/`charge_result_strings` で課金し、共有 builtin handler が新規生成する String body に tree/VM 共通で配線。`control_stop_to_error` を共有化して両 engine の error 写像を一本化。legacy env `TSUMUGI_MAX_SINGLE_STRING_BYTES`/`_STRING_ALLOCATIONS`/`_STRING_BYTES` を追加）と **source/import accounting サブスライス**（per-item `SingleSourceBytes`・cumulative `SourceCount`/`SourceBytes`・`ImportCount`/`ImportBytes` を `charge_source`/`charge_import` で `Link` フェーズに課金。root を source の 1 本目に数え、初めて解決した import は `source_bytes` と `import_bytes` の両方へ課金。`ModuleLoader::link` が import の生 byte 長を `LoadedModule` として返し、tree（`Evaluator::charge_link`）と VM（`Vm::charge_link`）が共通規則で課金。root source byte 長は `CompiledScript`（tree）と CLI 入口（VM）から渡す。legacy env `TSUMUGI_MAX_SINGLE_SOURCE_BYTES`/`_SOURCE_COUNT`/`_SOURCE_BYTES`/`_IMPORT_COUNT`/`_IMPORT_BYTES` を追加）を実装済み。さらに **heap accounting 基盤サブスライス**（`AllocationId`・per-execution `AllocationLedger`・§5.1 論理サイズ関数群（`heap_size`）・`HeapBytes` の checked 課金と §7.2 優先順位での超過写像・`AllocationId` overflow の `InternalFailure`・`usage()` の live/peak 反映。legacy env `TSUMUGI_MAX_LIVE_HEAP_BYTES` を追加）と、**collection（`List`/`Dict`）の per-drop release サブスライス**（`Value::List`/`Dict` の backing を `Rc<Tracked<T>>` にし、`Tracked::new` の生成時に §5.1 body サイズを課金、最後の `Rc` drop で `Drop` が release。`Rc::clone` 共有は無課金。COW mutation（index 代入・push/pop）は detach＋retrack で delta だけ課金/release。builtin_core は untracked（`Tracked::constant`）で生成し dispatch 境界の `track_result` が tracked 化・課金。AUD-024 rollback は drop 連鎖で自動 release。tree/VM 両対応）を実装済み。§5.2 の `charge_context_baseline` は tracked collection を二重計上するため engine の `charge_link` からは外した（純関数として単体テストは残す）。String body の per-drop release（PR-b、`Value::Str` を `Rc<Tracked<String>>` 化し、builtin 結果を dispatch 境界の `track_result` で live heap 課金・最後の参照 drop で release。cumulative `StringAllocations`/`StringBytes` 会計は据え置きの別会計、tree/VM 両対応）も実装済み。さらに cell・tree/VM 関数 instance の per-drop release（PR-c、`SharedValue` を `Rc<Tracked<RefCell<Value>>>` 化し cell 生成点で captured cell を課金、`Value::Fn`/`VmFn` の header token で function instance header を課金、drop で release）も実装済み。さらに AST / bytecode chunk / imported module record / rollback journal の per-drop 追跡（PR-d、所有構造側が `HeapToken = Tracked<()>` トークンで §5.1 論理サイズを課金し drop で release。§5.3「AST または bytecode」に従い tree は AST・VM は bytecode chunk を課金、import record は `ModuleLoader` が `loaded` set と寿命を揃えて保持、rollback journal entry は tree/VM とも entry の固定 overhead を課金）も実装済みで、全 per-drop 対象が揃ったため charge_link での baseline 再走査は不要。さらに string リテラル/連結/f-string 経路の課金（生成点で `track_result` を通し cumulative + live heap を課金、tree=`Expr::Str`/`BinOp`/`Expr::FStr`、VM=`LoadConst`(String)/`Add`/`FStrConcat`、両 engine 対応）も実装済み。さらに I-O accounting（input / output / host call の count + bytes を reserve/commit/refund（§7）で課金し、stdio を host call として co-charge。`charge_input`/`charge_output`/`charge_host_call_request`/`charge_host_response_bytes` を tree/VM の `print`/`input`/filesystem dispatch 境界へ共通配線。host call 対象は filesystem read/write と stdio に限り、その他 host 境界 builtin は Phase 2）も実装済みで、これで Slice 2 は完了。既定上限では観測挙動は不変。VM の push/pop full-clone 差・空リテラル差・f-string リテラル部分の個別課金差は Slice 6（VM charge parity、VM は experimental）で解消。続く Slice 3（explicit continuation）は PR-a（§9 公開 state-machine surface + poll-to-terminal core）と PR-b（tree evaluator の statement / block / loop を明示 frame stack + `drive_body` driver ループへ変換し、`exec_program`/`exec_block`/ループ再帰を `Vec<Frame>` + cursor へ置換、`EvalResult` 制御フローを `unwind_flow` で明示 unwind、§5.1 continuation_frame heap を各 frame の push/pop で課金/release。式は当面再帰）を実装済み。さらに PR-c（関数呼び出し・try handler の明示 frame）を実装済み。`eval_call` / callback の `call_fn_value` を `FrameKind::Call { saved_scopes }`（VM の `CallFrame` をミラー）を積む形へ変換し、driver ループを共通 `run_driver` へ抽出、関数本体は `drive_call_body`（`Rc<FnDef>` をローカル保持し `&def.body` を Call frame の `stmts` として渡す）で駆動する。Call frame の pop が全終了経路で `env.pop_call_frame` と call trace 巻き戻しを一元化し、`return` は最も近い Call frame まで unwind、`break`/`continue` は境界を越えず surface、エラー trace は発生時点 snapshot の `attach_trace` で付加する。`try` handler は PR-b の `FrameKind::Try`（= VM の `TryHandler` ミラー）を流用。観測挙動不変（tree/VM parity 維持）。さらに PR-d（slice fuel + yield）を実装済み。PR-d-1 で continuation を Evaluator 所有の `'static` 永続 frame stack へ移し（AST ブロック本体・`FnDef.body`・`Lambda.body` を `Rc<[Stmt]>` 化、`run_driver(stop_depth)` が `self.frames` を回す）、PR-d-2 で slice-fuel accounting を追加した。`count_step` は total fuel 課金に加え `slice_fuel_used` を数え、driver が文/反復境界で `PollSlice::max_fuel` 到達を見て `Yielded(SliceFuelExhausted)` を返す（yield は式の途中では起きず、関数呼び出し・callback・同期実行は `can_yield=false` で完了まで回す）。`Engine::poll` は初回 `begin_execution`（link のみ）→ `run_slice(max_fuel)` で永続 continuation を 1 slice ぶん resume し、`None`→Yielded / `Some`→Terminal（transaction commit/rollback 済み）へ写す。`ExplicitYield` は enum のみで trigger 未配線（Slice 4）。小 slice の複数 poll が単一 slice と同じ terminal・同じ total fuel へ到達することを `tests/engine_api.rs` で固定。**Slice 3（explicit continuation）は完了**。ステップ4（Phase 1 embedding）の E8a も完了（CLI の import なし tree file/stdin を embedding Engine API 経由へ統合、引数転送・exit code 変換を第12節どおり。import・REPL は E7、VM は E9 で統合）。続けてステップ5（Phase 2 capability の E7）へ着手し、slice C1（`CapabilitySet` 公開surface・deny-by-default・`CapabilitySetId` encoding）と C2（`CallableCatalog` 統合の残作業＝CAP-AT-20 の generated docs 完全一致。`render_reference()`＋`gen_builtins_doc` bin＋`docs/generated/builtins.md`＋ドリフト検出 contract test）と C7（ProcessExit、REV-023）を実装済み。C7 は `exit()` を `std::process::exit` からプロセス非終了の structured terminal `ExecutionOutcome::Exited { code }` へ置換した。frozen `CapabilitySet` を `ExecutionRequest`（`with_capabilities`、既定 empty＝deny-by-default）から `Evaluator`/`Vm` へ注入し、`exit` は ProcessExit authority を consult する: 未 grant は catch 可能な `capability` error（未捕捉→RuntimeError、プロセス継続）、grant 済み 0..=255 は catch 不可 terminal `Exited`（`Completed` と同じく language-state を commit）、範囲外/非 Int は catch 可能な `argument` error。`handle_error`（tree）と try handler（VM）は新 `ErrorKind::ProcessExit` を捕捉せず伝播する。alpha facade / CLI / REPL / VM は `CapabilitySet::ambient_compat()`（ProcessExit だけ grant）で従来挙動を保ち、CLI 境界が `Exited` を実際の exit code へ写す（tree/VM parity 確認済み）。CAP-AT-18 / EMB-AT-11 相当の unit test を追加。続けて C3（Environment/Clock）を実装済み。`env()`/`now()` を context builtin 化して frozen `CapabilitySet` の Environment snapshot / Clock adapter を consult し（共有 `resolve_env`/`resolve_now`、未 grant は adapter/trait call 0 の catch 可能 `capability` エラー、grant 済みは env=snapshot のみ引き missing=null・now=`now_utc`→Unix 秒）、`Clock` trait へ C3 最小形 `now_utc` と `SystemClock`/`FixedClock` を追加。ambient 経路の process env 読み取りを `ambient_environment_snapshot()` へ集約し `ambient_compat()` に Environment/Clock を載せて CLI/REPL/alpha を不変に保った。CAP-AT-05/06 相当を tree/VM 両 engine で test。続けて C4（Stdin/Stdout）を実装済み。`input()`/`print` を frozen `CapabilitySet` の Stdin/Stdout adapter へ配線し、`Input`/`Output` trait へ C4 最小形（`read_line`→`InputLine`・`write_all`/`flush`。context/`ReadLimit`/有限 meter は Phase 3 委譲）と `AdapterError`{`Host`} を追加、`SystemInput`/`SystemOutput` へ OS stdio を集約（AUD-035 の broken pipe 非 panic を adapter が継承、`write_stdout_line` 撤去）。共有 `resolve_input`/`resolve_print` が authority を先に consult し、未 grant は catch 可能 `capability` エラー、EOF は null、host 失敗は catch 可能 `host` エラー（新 `ErrorKind::Host`）。budget 課金は従来位置維持。tree=`print`/`input` builtin・VM=`OpCode::Print`/`exec_builtin` を配線、`ambient_compat()` に Stdin/Stdout 追加で観測挙動不変。CAP-AT-07/08 相当を tree/VM で test。続けて C5（Filesystem adapter）に着手し C5-a/b/c へ分割（Phase 2 最小形・import は sandbox 据え置き）。C5-a を実装済み: `RelativePath`/`PathError`/`FilesystemTarget::parse`（mount routing・lexical path 検証、CAP-AT-10/29）、`DirectoryHandle` trait 拡張＋新 `FileHandle`、value 型群、`AdapterError::SecureResolutionUnsupported`、`FilesystemCapability::root`。builtin/adapter 未配線で観測挙動不変。続けて C5-b（secure OS adapter）を実装済み: `std` のみの `OsDirectoryHandle`/`OsFileHandle` を追加し、Unix では component ごとの lstat で `SymlinkPolicy` を適用（`DenyAll` は symlink 拒否、`FollowWithinRoot`/`OperateOnFinalEntry` は `canonicalize`＋root 配下判定で拘束）、final entry は open 前 lstat で symlink 拒否（arch 依存の `O_NOFOLLOW` flag に依存せず明示 lstat で検出）。metadata/list/create_dir/remove/rename（同 policy 内のみ）を配線し、非 Unix は fail closed で `SecureResolutionUnsupported`（契約3、文字列 prefix fallback なし）。secret-free な `PublicMetadata`/`Debug`。CAP-AT-11/12 相当の `#[cfg(unix)]` unit test を追加。builtin 配線は C5-c。次は C5-c（builtin 移設 + ambient + oracle）。
4. **Phase 1 embedding:** [組み込みAPI仕様](embedding-api.md) ~~E1〜E6~~（✅ 完了）→ ~~**E8a**（CLI を Engine API だけへ統合、基本引数転送、AUD-018）~~（🟡 importなし tree file/stdin を Engine API 経由に統合し完了。import・REPL は E7、VM は E9 で統合）。REV-007/008/011/013/014/020 はこの Phase 1〜2 で解消する。
5. **Phase 2 capability:** E7 → E8b と Capability C1〜C10。**進行中**: C1（`CapabilitySet` 公開surface・deny-by-default・`CapabilitySetId` の byte-level encoding、`src/capability.rs`）と C2（`CallableCatalog` 統合。registry は既存のため残作業の CAP-AT-20「generated docs 完全一致」を実装＝`render_reference()`・`gen_builtins_doc` bin・`docs/generated/builtins.md`・ドリフト検出 contract test）と C7（ProcessExit、REV-023。`exit()` を structured `ExecutionOutcome::Exited { code }` へ置換し `std::process::exit` を library 経路から排除。`ExecutionRequest::with_capabilities` で frozen set を注入し ProcessExit を consult。CAP-AT-18 / EMB-AT-11 相当の test）を実装済み。さらに C3（Environment/Clock。`env()`/`now()` を context builtin 化し `resolve_env`/`resolve_now` で snapshot / Clock adapter を consult、`Clock::now_utc` 最小形 + `SystemClock`/`FixedClock`、ambient 経路の env 読み取りを `ambient_environment_snapshot()` へ集約し `ambient_compat()` へ Environment/Clock を追加。CAP-AT-05/06 相当を tree/VM で test）を実装済み。さらに C4（Stdin/Stdout。`input()`/`print` を frozen set の Stdin/Stdout adapter へ配線。`Input`/`Output` trait の C4 最小形＋`AdapterError`{`Host`}＋`InputLine`、`SystemInput`/`SystemOutput` へ OS stdio 集約、共有 `resolve_input`/`resolve_print`＝authority 先行 consult・未 grant は catch 可能 `capability`・EOF は null・host 失敗は catch 可能 `host`（新 `ErrorKind::Host`）、budget 課金は従来位置維持、`ambient_compat()` へ Stdin/Stdout 追加で観測挙動不変。CAP-AT-07/08 相当を tree/VM で test）を実装済み。さらに C5（Filesystem adapter）へ着手し C5-a/b/c へ分割（Phase 2 最小形・import は sandbox 据え置き）。C5-a（`RelativePath`/`PathError`/`FilesystemTarget::parse` の mount routing・lexical 検証、`DirectoryHandle` 拡張＋`FileHandle`、value 型群、`AdapterError::SecureResolutionUnsupported`、`FilesystemCapability::root`。builtin 未配線で観測挙動不変）を実装済み。さらに C5-b（secure OS adapter。`std` のみの `OsDirectoryHandle`/`OsFileHandle`。Unix は component ごとの lstat で `SymlinkPolicy` を適用＝`DenyAll` 拒否・`FollowWithinRoot`/`OperateOnFinalEntry` は `canonicalize`＋root 配下判定で拘束、final entry は open 前 lstat で symlink 拒否＝arch 依存の `O_NOFOLLOW` flag に依存しない設計。metadata/list/create_dir/remove/rename＝同 policy 内のみ。非 Unix は fail closed `SecureResolutionUnsupported`＝文字列 prefix fallback なし。secret-free な `PublicMetadata`/`Debug`。CAP-AT-11/12 相当の `#[cfg(unix)]` test。builtin 配線は C5-c）を実装済み。次は C5-c（builtin 移設 + ambient + oracle）→ C6（stream 後）→ C8（BuiltinSpec 衝突検査後）→ C9/C10（最後）の順（capability-model §18）。REV-023 は C7 で解消。REV-002/009/019/021/022 はこの capability 面で実装（sandbox の process-global を ExecutionContext へ移す）。

### open 項目一覧（未完了のみ・優先度順）

未完了（🟡 部分実装 / ⬜ 未実装）だけを AUD・REV 横断で並べたもの。✅ 完了項目は各バックログ節を参照。

| 優先度 | ID | 概要 | 状態 | 詳細 |
|---|---|---|---|---|
| P0 | REV-001 | 共有DAGの比較・表示が指数時間／出力 | ⬜ | REV表 P0 |
| P0 | REV-002 | 非UTF-8 canonical import pathでsandbox認可がすり替わる | ⬜ | REV表 P0 |
| ~~P0~~ | ~~REV-006~~ | ~~未検証bytecodeでstep/call課金を迂回し無期限実行~~ | ✅ | 完了（§17.7、per-instruction課金+verifier+`VerifiedChunk`） |
| P0 | REV-015 | source/string/heap/I-O/bulk workが未有限化 | 🟡 | Slice 1（budget型・`BudgetLedger`・legacy adapter）と Slice 2 の string accounting・source/import accounting・heap accounting 基盤（`AllocationLedger`・§5.1 論理サイズ・`HeapBytes` 課金/超過写像。legacy env `TSUMUGI_MAX_LIVE_HEAP_BYTES`）・collection（`List`/`Dict`）の per-drop release（`Rc<Tracked<T>>`＝生成時課金＋最後の参照 drop で release、COW は delta 課金、tree/VM 両対応）と String body の per-drop release（PR-b、`Value::Str` を `Rc<Tracked<String>>` 化、`track_result` で live heap 課金）と cell・tree/VM 関数 instance の per-drop release（PR-c、`SharedValue` を `Rc<Tracked<RefCell<Value>>>` 化、`Value::Fn`/`VmFn` に header token）と AST / bytecode chunk / imported module record / rollback journal の per-drop 追跡（PR-d、`HeapToken = Tracked<()>` トークン。tree は AST・VM は bytecode chunk、import record と journal entry は両 engine）と string リテラル/連結/f-string 経路の課金（生成点で `track_result` を通し cumulative + live heap を課金、tree/VM 両対応）と I-O accounting（input / output / host call の count + bytes を reserve/commit/refund で課金。stdio を host call として co-charge。host call 対象は filesystem read/write と stdio）実装済みで Slice 2 完了。Slice 3 は PR-a（公開 state-machine surface + poll-to-terminal core）・PR-b（tree の statement / block / loop を明示 frame stack + driver ループへ変換し continuation_frame heap を課金/release）・PR-c（関数呼び出し・try handler の明示 frame。`eval_call` / `call_fn_value` を `FrameKind::Call` 化し共通 `run_driver` + `drive_call_body` で駆動。VM の `CallFrame` / `TryHandler` をミラー）を実装済み。全 I/O/host budget の残り（descriptor 上限・stream 途中停止・capability 拒否差）と deadline/cancel/transaction は後続 Slice。Slice 3 は PR-a〜PR-d 完了（PR-d-1 で continuation を Evaluator 所有の `'static` 永続 frame stack へ、PR-d-2 で slice-fuel yield/resume）。VM push/pop の full-clone 差・f-string リテラル部分の個別課金差は Slice 6。REV表 P0 |
| ~~P0~~ | ~~REV-023~~ | ~~`exit()`がホストプロセスを終了する~~ | ✅ | 完了（Phase 2 C7。`exit()` を structured `ExecutionOutcome::Exited { code }` へ置換し library 経路から `std::process::exit` を排除。ProcessExit capability を consult し、未 grant は catch 可能 `capability` error、grant 済み 0..=255 は catch 不可 terminal、範囲外は `argument` error。CLI 境界が Exited を exit code へ写す。CAP-AT-18 / EMB-AT-11） |
| ~~P1~~ | ~~REV-003~~ | ~~Int–Float比較が2^53超で誤り、`==`が非推移的~~ | ✅ | 完了（§17.1、仕様revision 0.19。下記REV表 P1参照） |
| ~~P1~~ | ~~REV-004~~ | ~~公開`Chunk::patch_jump`がpanic~~ | ✅ | 完了（§17.2、`patch_jump`をfallible化しbuilderエラーをcompiler internal errorへ） |
| ~~P1~~ | ~~REV-005~~ | ~~不正`MakeClosure` descriptorをNull captureで黙認~~ | ✅ | 完了（§17.3、`MakeClosure(proto_index)`+明示`CaptureDesc`、Nullフォールバック廃止） |
| P1 | REV-007 | `ExecutionContext`がsession stateとrun meterを混在 | ⬜ | REV表 P1 |
| P1 | REV-008 | stable `Engine::execute`がtransactionでない | 🟡 | REV表 P1（REPLのみ実装） |
| P1 | REV-011 | revision・engine差・実装statusが文書drift | ⬜ | REV表 P1 |
| P1 | REV-012 | call評価順のstatus/doc drift（意味論正本はAUD-017） | 🟡 | REV表 P1（§17.5 設計確定） |
| P1 | REV-013 | `args()`がhost process argvを読む | ⬜ | REV表 P1 |
| P1 | REV-014 | sandbox/env/limits/stdio/clockがprocess-global | ⬜ | REV表 P1 |
| ~~P1~~ | ~~REV-018~~ | ~~internal module／raw bytecode公開が安全境界を弱める~~ | ✅ | 完了（`chunk`/`compiler`/`opcode`/`verifier`/`vm`を`unstable-bytecode` feature下でのみ公開、安定surfaceは`Engine`系） |
| P1 | REV-020 | import先parse errorの原因を捨てる | ⬜ | REV表 P1 |
| P1 | REV-021 | `remove_dir`の再帰削除とcapabilityの不整合 | ⬜ | REV表 P1（§17.6 設計確定） |
| P1 | AUD-018 | CLIからscript引数を渡せない（capability profile/optionsはE8bで別追跡） | 🟡 | AUD crosswalk（E8a完了。importなしtree file/stdinは`EmbeddingRequest::with_arguments`→`Engine::run`経由。import・REPLの引数転送はE7で統合するまでalpha facade。E8b残） |
| P1 | AUD-021 | language-spec/LANG_GUIDE/design drift継続 | 🟡 | AUD P2表（意味論確定後に継続更新） |
| P2 | REV-009 | `list_dir`の部分失敗黙殺と非UTF-8名衝突 | ⬜ | REV表 P2（§17.4 設計確定） |
| P2 | REV-010 | 32-bitで`i64 as usize`がwrap | ⬜ | REV表 P2 |
| P2 | REV-016 | Rc cycleと長寿命contextのheap残留 | ⬜ | REV表 P2 |
| P2 | REV-017 | 表示・repr・sort orderの結合 | ⬜ | REV表 P2 |
| P2 | REV-019 | `now()`のepoch前/error 0化と未検査cast | ⬜ | REV表 P2 |
| P2 | REV-022 | EOF/I-O error/permissionをNull/falseへ畳む | ⬜ | REV表 P2 |
| P2 | REV-024 | CIがrolling stable・fuzz/stress/MSRVなし | 🟡 | REV表 P2 |
| P2 | REV-025 | parse diagnostic件数に上限がない | ⬜ | REV表 P2 |
| P2 | AUD-020 | sandbox TOCTOU/path-handle未実装 | 🟡 | AUD crosswalk |
| P2 | AUD-022 | fuzz/stress/matrix未実装 | 🟡 | AUD crosswalk |
| P2 | AUD-036 | lossy数値・OS境界変換の検証（残: `Exited`の`usage`付き完全形＝Phase 3/E11） | 🟡 | AUD crosswalk（§10 設計確定・Float→Int/file_size・`exit`の構造化`Exited`＝Phase 2 C7/REV-023 実装済み） |
| P2 | AUD-045 | MSRV固定は完了。release/install/OCI未実装 | 🟡 | AUD crosswalk（toolchain/MSRV は VRO Slice 1 で実装済み） |

## プロジェクトの方向性

Tsumugiは、学習用の言語処理系として得た知見を発展させ、実運用を見据えた、制御可能な組み込みスクリプト言語を目指す。価値基準と非目標の正本は[Tsumugi Manifesto](manifesto.md)とし、本ロードマップは現在地からその目標へ進む順序を管理する。

最短の実行時間よりホストの安定性を優先する。新しい言語機能を増やす前に、組み込み境界、明示的な権限、包括的な実行予算、規範意味論、監査可能性を整える。

## マニフェスト実現ロードマップ

以下は目標アーキテクチャへの移行順序である。現行のstep上限、collection上限、深度制限、filesystem/env allow-list、構造化エラー、差分テストは土台として再利用するが、それだけで各phaseが完了したとはみなさない。

| Phase | 目的 | 設計正本 | 設計状態 | 実装状態 | 完了gate |
|---|---|---|---|---|---|
| 0 | 保証範囲と脅威モデル | [脅威モデル](threat-model.md) | ✅ 確定済み | 🟡 文書化は完了。OS隔離guideとrelease導線は未実装 | 第11節Phase 0、TM-AT-07・TM-AT-12（host部分） |
| 1 | 安定した組み込みAPI | [組み込みAPI仕様](embedding-api.md) | ✅ 確定済み | 🟡 tree向け最小facadeとCLI入口に加え、E1（識別子・設定・エラー・terminal outcome 型と `EngineBuilder`）・E2（`compile`/`link`・`CompiledScript`/`LinkedScript`・import なし link・self-contained SHA-256 の byte-level hash・`retain_source` 既定 false）・E3（`Engine::run`・`ExecutionContext`(`!Send+!Sync`)・tree 評価器での同期実行・`ExecutionError` 写像。runnable AST は案 A＝実行時に保持 source を再 parse、§4.1 記録）・E4（Context/transaction 縦切り。`Engine::run` を transaction 全面適用（`transactional=true`）へ切替え、`Completed` は全 language-state を commit・`RuntimeError` は開始時点へ rollback（AUD-024・§10 規則5）。`InternalFailure` だけが context を poison（§10 規則4）し以後の実行・状態操作を拒否、他 terminal は再利用可。`ContextError`{`Busy`/`Poisoned`}・`ExecutionContext::is_poisoned`/`clear_user_state`・再入防止 running フラグを追加）を実装（`src/embedding.rs`）。さらに E5（terminal channel。`ExecutionRequest::pre_cancelled()` で最初の poll より前の cancel を命令0の `Cancelled` terminal へ落とし、context を poison しない。他 terminal は E1〜E4 で構造化 outcome のみ・host process 継続）と E6（compile/link/run の各 host boundary を `catch_host_unwind`＝`catch_unwind(AssertUnwindSafe)` で囲い、panic を `CompileDiagnosticCode::InternalFault` / `LinkError::InternalFailure` / `ExecutionOutcome::InternalFailure` へ非ゼロ fault ID 付きで写す。payload/backtrace は非公開、run panic は context を poison、`running` は必ず降ろす）を実装。**E1〜E6 完了**。加えて E8a（CLI の import なし tree file/stdin を embedding Engine API 経由へ統合。`EmbeddingRequest::with_arguments` で引数転送、`ExecutionOutcome`→exit code 変換を第12節どおり、診断は現行 `TsumugiError` Display と byte 一致。import root は `link` の `FeatureUnavailable` を検出して alpha facade へフォールバック、REPL は状態継続・import 解決のため alpha 維持。import・REPL は E7、VM は E9 で統合）を実装 | 第14〜15節 E1〜E6・E8a、EMB-AT-01〜09・13・15〜17 |
| 2 | deny-by-default capability | [Capability Model仕様](capability-model.md)・[組み込みAPI仕様](embedding-api.md) | ✅ 確定済み | 🟡 C1（`CapabilitySet` 公開surface）を実装。`src/capability.rs` に `CapabilityKind`（8 authority）・`CapabilitySet`（`empty()` deny-by-default・`builder`・`id`・`contains`）・`CapabilitySetBuilder`（authority 別 setter、同 kind 二重設定は `ConfigError::DuplicateCapability`、host function grant のみ別 ID 複数可）・`CapabilitySetId`（§3.1/§8.6 の byte-level encoding、domain `TSUMUGI-CAPSET-V1`、`embedding::hash::sha256` を再利用）と、adapter trait（`Clock`/`Input`/`Output`/`ModuleResolver`/`DirectoryHandle` は C1 では `policy_id`/`symlink_policy` のみ）・値型（`EnvironmentSnapshot`/`EnvironmentValue`/`DataClassification`・`FilesystemCapability`/`FilesystemRoot`/`MountName`/`FsOperation`/`SymlinkPolicy`・`ProcessExit`・`HostFunctionId`）を用意。`ConfigError` に Phase 2 variant（`DuplicateCapability`/`DuplicateCallableName`/`InvalidDescriptor`/`InvalidFilesystemPolicy`）を追加。CAP-AT-01/02/03/28 相当の unit test（empty で全 8 authority deny、単一 grant 分離、clone の ID/権限一致、empty set ID の golden bytes、secret-free `Debug`）を追加。C2（CallableCatalog 統合の残作業＝CAP-AT-20 generated docs 完全一致）と C7（ProcessExit、REV-023）も実装済み。さらに C3（Environment/Clock）を実装：`env()`/`now()` を PureCore dispatch から context builtin へ移し、共有 `resolve_env`/`resolve_now`（authority を引数・型検査より後に確定）で frozen `CapabilitySet` の Environment snapshot / Clock adapter を consult する形へ tree/VM 両 engine を配線。未 grant は adapter/trait call 0 のまま catch 可能 `capability` エラー（未捕捉→RuntimeError）、grant 済みは env=snapshot のみ（missing=null）・now=`now_utc`→Unix 秒。`Clock` trait へ C3 最小形 `now_utc(&self) -> SystemTime`（第6節最終形の `CapabilityCallContext`/`AdapterError` 引数は C4/C8 で導入するスライスへ委譲）と `SystemClock`/`FixedClock` を追加。ambient 経路の唯一の process env 読み取りを `ambient_environment_snapshot()`（legacy `TSUMUGI_ENV_ALLOW`+`TSUMUGI_` 保護適用の snapshot 化）へ集約し、`ambient_compat()` が Environment/Clock も grant するようにして CLI/REPL/alpha を不変に保った（core builtin の ambient read 0）。CAP-AT-05/06 相当を tree（embedding `Engine::run`）と VM（`exec_builtin` 直呼び）の両方で test。さらに C5-a（path/mount routing 型・trait 拡張）と C5-b（secure OS adapter＝`std` のみの `OsDirectoryHandle`/`OsFileHandle`、Unix は lstat ベースで `SymlinkPolicy` を適用し root 拘束、非 Unix は fail closed `SecureResolutionUnsupported`）を実装。**builtin 配線は C5-c**。現行 sandbox（filesystem）allow-list（process-global `OnceLock`）は fail-open の defense-in-depth のまま残る | Capability第18〜19節 C1〜C10・Phase 2 CAP-AT、Embedding E7・E8b |
| 3 | 包括的な実行予算 | [実行予算・協調実行仕様](execution-control.md) | ✅ 確定済み | 🟡 step・collection・AST/import/call深度・string/source/import budget・heap基盤（`AllocationLedger`・§5.1論理サイズ・§5.2 context baseline）・全heap objectのper-drop release（collection/String/cell/関数/AST/chunk/import record/journal）・string リテラル/連結/f-string 経路の課金・I-O accounting（input/output/host call の count+bytes を reserve/commit/refund で課金。stdio を host call として co-charge。host call 対象は filesystem read/write と stdio）を部分実装。加えて Slice 3 PR-a（公開 state-machine surface）・PR-b（tree の statement/block/loop を明示 frame stack + driver ループ化し continuation_frame heap を配線）・PR-c（関数呼び出し・try handler の明示 frame。`eval_call`/`call_fn_value` を `FrameKind::Call` 化し共通 `run_driver`+`drive_call_body` で駆動。VM の `CallFrame`/`TryHandler` をミラー）を実装。残りのI/O/host budget（descriptor上限・stream途中停止・capability拒否差）、deadline、runtime cancel、transaction は未実装。Slice 3 は完了（PR-a〜PR-d。PR-d-1 で continuation を Evaluator 所有の `'static` 永続 frame stack へ、PR-d-2 で slice-fuel yield/resume を実装） | 第3〜8・10・15.1〜15.3・16節、EMB-AT-21 |
| 4 | 協調実行と負荷制御 | [実行予算・協調実行仕様](execution-control.md) | ✅ 確定済み | ⬜ 未実装 | 第6.2・9・11〜12・15.3〜15.5・16節、EMB-AT-22 |
| 5 | 規範意味論と決定的境界 | [決定性・実行時監査仕様](determinism-and-audit.md)・[次期意味論・実装決定](semantic-decisions.md) | ✅ 確定済み | 🟡 paired testはあるが既知のbackend差とHostBoundary/FunctionId/record-replayが未実装 | Determinism第2〜6・14節Slice 1〜4/6・15.1/15.2/15.5・16節 |
| 6 | 実行時監査 | [決定性・実行時監査仕様](determinism-and-audit.md) | ✅ 確定済み | 🟡 構造化errorのみ。canonical 8 event、sink、sequence、redaction、bounded journal、fail-closedは未実装 | 第7〜12・14節Slice 5・15.3/15.4・16節、EMB-AT-23・CAP-AT-30 |
| 7 | 運用保証と検証 | [検証・リリース・運用設計](verification-release-operations.md) | ✅ 確定済み | 🟡 timeout・golden・scaling・defensive test、3 OS CI、coverage artifactのみ実装。MSRV/release/fuzz/stress/OCI/運用は未実装 | 第6節matrix、第17節VRO-AT-01〜15、第18節 |

長時間処理について「確実に終わる」とは、任意のscriptの成功を保証することではない。ホストを不安定にせず、完了、停止、または失敗を観測可能な結果として扱い、有限の処理を設定された負荷の中で着実に進められることを目標とする。

## 2026-08-26 深層監査バックログ

ツリーウォーク版とVM版を、REPL継続実行・失敗時状態・スコープ・クロージャ・import・全組み込み関数・資源上限・既存仕様の観点で横断監査した。既存テストは全件成功したが、REPL入力間の状態回復や実行系差を検出できない空白がある。優先度は、ホストプロセス停止／メモリ枯渇につながるものを **P0**、誤実行・状態漏洩・主要仕様差を **P1**、診断性・境界値・文書不整合を **P2** とする。

追加監査では `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo build`、`cargo test` がすべて成功した状態から、隔離した最小入力で既存テスト外の不具合を再現した。再現済み項目は実装状況欄に明記し、Windows固有挙動やsymlink操作など実環境確認が必要な項目はコード監査結果として区別する。

> **状態の読み方:** 以下のAUD表の最終列はコード・test・workflow・配布物の**実装状況**であり、設計状態ではない。次期設計正本で判断が確定済みでも、受入gateを満たす実装がなければ完了扱いにしない。

### P0 — Critical

| ID | 項目 | 再現・影響 | 実装状況 |
|---|---|---|---|
| AUD-001 | VM REPLのコンパイル失敗をtransactionalにする | ブロック内でlocal追加後にcompile error（当時は未解決index assignment target、現在はループ外`break`等）を置くと、Compilerだけが更新され、次入力の`GetLocal`でRustの範囲外panic。stale loopから`Jump(0)`生成にも到達可能 | ✅ 完了（REPL回帰テスト追加） |
| AUD-002 | VM REPLの未捕捉runtime error後にstack/frame/handler/compilerを復元する | 一時値・callee frame・未実行bindingが次入力へ残り、誤値参照、古い関数の再開、二次panicを起こす | ✅ 完了（REPL回帰テスト追加） |
| AUD-003 | コレクション上限を全生成経路へ一貫適用する | VMのlist/dict literal、`push`、`map`/`filter`、keys/values等で`TSUMUGI_MAX_COLLECTION_SIZE`を迂回でき、メモリDoS防止の完了記載と矛盾 | ✅ 言語から到達する生成・拡張経路を修正。総heap quotaは対象外 |
| AUD-026 | `format_time`の極端なtimestampを定数時間で処理する | `format_time(9223372036854775807, "%Y")`は1970年から1年ずつ進むため実用上停止せず、step予算も消費しない。tree/VMとも2秒以内に完了せず、timeout（終了124）で強制停止 | ✅ 完了（400年周期化・両engineのi64極値timeout回帰テスト追加） |
| AUD-027 | parser・compiler・evaluatorの全再帰経路へ深度制限を適用する | 10万個の`not`連鎖で旧`MAX_PARSE_DEPTH`を迂回し、Rust stack overflowでabort（終了134）。`elif`直再帰や左深BinOp ASTにも同種の経路があった | ✅ 完了（`MAX_AST_DEPTH=256`、生成時検査・子f-string深度継承・実行前の非再帰preflight） |
| AUD-028 | 非循環import chainの深度を制限する | treeのimport実行とVMのinline compileが、旧実装では深い非循環chainでhost stack overflowに到達し得た | ✅ 完了（rootを除くactive chainを128に制限、tree/VM共通エラー） |
| AUD-043 | トップレベル`return`の文脈を検証する | Parserが関数外の`return`を無条件に受理するため3つの症状になる。(1) VM REPLでtop-level変数がある状態で`return`を実行すると、`ReturnValue`がroot frameをpopしstackを`base`まで捨てる一方Compilerの`locals`は残るため、次入力の`GetLocal`が空stackを読み`src/vm.rs`の範囲外panicでhost abort（終了1）。`try`内・`for`内の`return`でも再現し、tree REPLは同入力で正常継続する。(2) file実行では両engineとも後続文を実行せず、エラーなしで終了コード0になる。(3) import先のトップレベル`return`は、treeがmodule実行だけ打ち切って呼び出し元を継続する一方、VMはinline展開された`ReturnValue`がroot script全体を終了させる。`break` / `continue`は両engineでエラー化されるが`return`だけ検査がない | ✅ 完了（Parserに関数本体の深度を持たせ、関数外の`return`を両engine共通のパースエラーへ。parser単体・error fixture・tree/VM REPLの回帰テスト追加） |

### P1 — High

| ID | 項目 | 再現・影響 | 実装状況 |
|---|---|---|---|
| AUD-004 | VMの`locals_cells`をREPL入力・try unwindで正しく保存／復元する | 入力ごとにtop-level cell対応が消え、closureと変数が別値になる。try内localのcellがcatch変数slotと衝突する | ✅ 完了（cell同一性・catch回帰テスト追加） |
| AUD-005 | treeのwhile/forでエラー時もscopeを必ず解放する | ループ内エラーをcatchすると反復localが後続処理・次REPL入力から見える | ✅ 完了（caught error回帰テスト追加） |
| AUD-006 | import失敗時の`base_dir`・loading/loaded marker・compiler状態を復元する | 同一fileの再試行がsilent skip。VMでは次の相対import基準やlocalsも汚染する | ✅ 失敗rollback完了。import・REPLの状態commit方針もAUD-024で完了（未捕捉errorで全language-stateをrollback、正常・catch済み完了はcommit） |
| AUD-007 | 非トップレベルimportの意味論を統一する | VMはcompile-time inlineのためfalse branchでもloaded扱い、loopでは複数実行、関数内relative path/control-flowもtreeと異なる | ✅ トップレベル限定として統一（全ネスト構文のparserテスト・tree/VM回帰テスト追加） |
| AUD-008 | `if` / `try` / `catch`のscope仕様を確定し両engineを統一する | treeではblock内`let`が外から可視、VMではcompile error。公開ガイドの「ifはscopeを作らない」とVMが不一致 | ✅ 独立block scopeへ統一（shadowing・error/control-flow・closure・REPL回帰テスト追加） |
| AUD-009 | tree REPLのstep予算を入力単位でresetする | step数がセッション全体で累積し、一度上限に達すると以後の入力も失敗。VMと不一致 | ✅ 完了（入力間回帰テスト追加） |
| AUD-010 | for変数のclosure bindingを反復単位で統一する | `[1,2,3]`で作ったclosureがtreeは`1,2,3`、VMは全て`3` | ✅ 反復ごとのfresh cellへ統一（closure・control-flow・REPL slot再利用回帰テスト追加） |
| AUD-011 | VMのcompile-time name resolution差を仕様化／縮小する | dead branchの未定義名、global forward reference、引数評価順がtreeと異なる | ✅ call validation順とruntime global fallbackを統一（dead code・forward read/write・mutual recursion・REPL/import回帰テスト追加） |
| AUD-012 | context依存builtinの契約を統一する | `input(side())`等の不正arityでtreeは引数を評価せず、VMは副作用後に拒否する。`push`/`pop`はupvalue・一時List・error kindにも差がある | ✅ builtin選択後のarity・破壊対象を引数評価前に検査。一時List拒否、left-to-right snapshot/writeback、local/upvalue/runtime global更新、collection error kindをtree/VMで統一 |
| AUD-013 | VM index assignmentのupvalue対応と評価順を統一する | captured listへ代入不可。object取得順の違いで副作用後に古いlistを書き戻す | ✅ target解決→index→value→in-place更新へ統一。local/upvalue/runtime global対応、未定義targetの先行報告、共有`assign_index`によるメッセージ・境界判定一致（golden pair・両engine REPL回帰テスト追加） |
| AUD-014 | equality / relational comparisonの対象型を統一する | List/Dict/Function/Error、Int×Floatでtreeはtype error、VMはboolを返す場合がある。486ケース（9型×9型×6演算子）の網羅比較で128件の差分を確認した | ✅ 完了（等価比較を全型で成立させ、Int×Floatを数値比較、List/Dict/Errorを構造比較、関数値を`Rc::ptr_eq`の同一性比較へ統一。大小比較は数値のみ（混在可）。判定を`Value::PartialEq`へ集約し、486ケースの差分が0件。付随して型エラーの種別をkind明示へ変え、被演算子の値による誤分類も解消。仕様revisionを0.9へ）。網羅行列は型の組み合わせを対象としたため、同一sourceから複数の関数値を作る形は含まれず、同一性の粒度差をAUD-048として後から検出した |
| AUD-029 | 複数行lambdaの終端`end`を必須検証する | `let f = fn(x)\n return x`をtree/VMとも構文エラーにせず終了コード0で受理する。EOFを`end`として無条件消費している | ✅ Parserで`End`を必須検証し、tree/VM共通でEOFを構文エラー化 |
| AUD-030 | top-level importの評価時点を統一／仕様化する | `print("BEFORE")`後の失敗importでtreeだけ先行出力する。実行中に生成したmoduleもtreeだけimport可能で、VMのcompile-time inlineと観測可能な差がある。9ケースの検証で4つの観測差を確認した（失敗import前の副作用、構文エラーmodule、import前の実行時エラー、実行中に生成／削除したmodule） | ✅ 完了（実行前解決へ統一。`src/module.rs`の`ModuleLoader`へ解決処理を集約し、treeは`run`でリンク、VMはリンク済みプログラムをcompileする。`exec_import` / `compile_import`を削除し、9ケースすべてで両engine一致。仕様revisionを0.10へ） |
| AUD-031 | Windowsで`TSUMUGI_*`環境変数保護をcase-insensitiveにする | Windowsの環境変数検索は大文字小文字を区別しないがprefix検査は区別するため、`env("tsumugi_sandbox")`等で保護値を読める可能性がある | ✅ Unicode uppercase後のprefix保護を実装。tree/VM、allow-list未設定/全許可、ASCII大小文字・Unicode case alias・secret非漏洩をWindows実OS CIで確認 |
| AUD-032 | 破壊的ファイル操作のfinal symlink意味論を修正する | 旧`check_path`は最終symlinkまでcanonicalizeし、`remove`/`remove_dir`/`rename`がlink自体ではなくlink先を削除・移動していた | ✅ 完了（中間componentのみ解決し、final directory entryを操作） |
| AUD-037 | ローカル名前付き関数のself-bindingを両engineで統一する | 関数内で定義した再帰関数がtreeでは自身を捕捉できず`未定義の関数`、VMでは正常完了する。`factorial(5)`相当でtree失敗／VM `120`を再現 | ✅ 呼び出し時self-bindingとuser binding優先のbuiltin fallbackをtree/VMで統一。匿名lambdaの内部slot名も非公開化 |

### P2 — Medium / Quality

| ID | 項目 | 再現・影響 | 実装状況 |
|---|---|---|---|
| AUD-015 | callback内`break`/`continue`を通常関数と同じくエラー化する | treeのmap/filter/eachだけ`break`を暗黙`null`として扱い、VMはcompile error | ✅ 完了（control-flow回帰テスト追加） |
| AUD-016 | 同一scopeの`let`再宣言時のbinding identityを仕様化する | 既存closureがtreeでは旧cell、VMでは更新済みcellを参照 | ✅ 完了（VM Compilerの`Stmt::Let`再宣言でのslot再利用を廃止し、全scopeで新slot（=新cell）を割り当て。treeの既存fresh-cell挙動を規範として統一。`let`/`fn`再宣言のtop-level/function/block scope・REPL入力間rollbackを両engineで固定するgolden fixture `let_redeclaration_fresh_cell`とREPL回帰テストを追加。観測挙動が変わるため仕様revisionを0.14へ） |
| AUD-017 | call-depth境界を統一する | 上限128にtop-level frameを含めるVMだけ、許容user frame数が1少ない | ✅ 完了（AUD-050と一体で解消。VMを`active_user_frame_count() = frames.len() - 1`によるroot除外計数へ変更し、tree/VMとも128 user frameを許可・129個目を拒否。境界値127/128/129・相互再帰・lambda・callbackの回帰テストと、両engine一致のstack overflow traceテストを追加） |
| AUD-018 | CLIからscript引数を渡せるようにする | `args()`を公開しているがCLIが2個目以降の非flag引数をusage errorにする | 🟡 設計確定・未実装（同第6節、Embedding E8a。capability profile/optionsとsafe/legacy移行はE8bで別追跡） |
| AUD-019 | engine固有error kind/messageを統一する | iteration/index/callback等でkind・messageが異なる。push/pop/map/filter/eachの主要なkindはAUD-012で統一したが、callback messageやtrace差は残る。具体例として、コレクション以外へのindex（`let n = 42` に対する `n[0]`）はtreeが`runtime` / 「インデックスアクセスできません: Int(42)[Int(0)]」、VMが`type` / 「型エラー: Int(42) に対して Int(0) でインデックスアクセスできません」になっていた | ✅ 完了（`src/error.rs`にoperation別canonical constructorを新設し、tree/VM/compiler/module/builtin_coreの全runtime error生成をそれへ移行。値埋め込みを型名へ置換し、message文字列からのkind推測（`classify_runtime_error`）と`runtime()`を削除。`n[0]`は両engineとも`type` / 「インデックスアクセスできない型です: Int」に統一し、`index_read_lowering.expected.vm`を削除。`tests/canonical_error_inventory.rs`で28 operation行をtree/VM両engine実行し(kind, message, line)完全一致とRuntime catch-all非生成を検証。仕様revisionを0.12へ。traceのcanonical化はcall trace統一済みで、host adapter traceはPhase 2で別追跡） |
| AUD-020 | sandboxの脅威モデルとTOCTOU制約を明記する | checkとI/O間のsymlink race、sandbox検査前のcanonicalizeによる許可外path存在oracle、dangling final symlink経由の新規write/append、空設定のfail-open意味論が未整理 | 🟡 設計確定・部分実装（現行制約の文書化のみ完了。[脅威モデル](threat-model.md) TM-002〜004と[Capability Model仕様](capability-model.md) CAP-AT-10〜14のpath-handle実装は未完了） |
| AUD-021 | language-spec / LANG_GUIDE / designのdriftを解消する | engine parity・Float完全一致・全module unit test・coverage/benchmark gate等の記載が現実装やAUD残件と矛盾する | 🟡 規範仕様と既知非適合、VMの実験的位置付け、sandbox制約、予約語、循環参照を更新。意味論確定後の更新は継続 |
| AUD-022 | REPL・differential・limit境界・defensive VMテストを追加する | subprocess timeoutなし、error goldenが部分一致、fixture登録が手動、tree/VMが固定`/tmp`を共有して並列raceする。厳密なstderr/stdout副作用比較も不足 | 🟡 設計確定・部分実装（harness、timeout、完全一致、temp分離は完了。[検証・リリース・運用設計](verification-release-operations.md)のmatrix/fuzz/stressは未実装） |
| AUD-023 | VMのunchecked index/`unwrap()`を構造化internal errorへ置換する | compiler/VM invariantが崩れるとhost panic。AUD-001/002でユーザー入力から到達可能だった。`Vm::new` / `run_repl_chunk`は任意の`Chunk`を受け取るため、library利用では範囲外のslot・定数・upvalue・行番号表の不足・operandのunderflowでindex panicへ到達した | ✅ 完了（frame/stack/upvalue/定数/命令参照を検査付きヘルパー経由にし、`unreachable!`も含め本番コードから`unwrap`を排除。公開APIだけで書いた`tests/defensive_vm.rs`で8ケースを固定し、修正前はindex panicで失敗することを確認） |
| AUD-024 | import・REPLの状態commit方針を明文化する | 未捕捉error前の代入/list mutation/upvalue更新を保持するかrollbackするか未定義。外部I/Oはrollback不能 | ✅ 完了（tree/VMとも未捕捉errorで全language-state（binding・cell値・index代入・push/pop・import marker）を入力開始時点へrollback。正常完了とcatch済み完了はcommit。外部効果はrollbackしない。first-write undo logで実装し記録量は変更箇所数に比例。仕様revisionを0.15へ。deadline/budget/cancel由来のrollbackはPhase 3/4で別追跡） |
| AUD-025 | VM REPL checkpointの複製コストを削減する | 入力ごとの`stack.clone()`が保持中List/Dictをdeep cloneし、時間・一時メモリがREPL状態量に比例する | ✅ 完了（stack全体cloneを初期長と変更済みslotのmutation logへ置換。通常入力は保持中List/Dictを複製せず、既存slotの書換・削除時だけrollback用の元値を記録して未捕捉エラー時に復元） |
| AUD-033 | 未完結REPL入力のEOFを診断する | `if true`等の継続入力中にEOFを送ると、tree/VMとも構文エラーを出さずbufferを破棄して終了コード0になる | ✅ 完了（tree/VM共有の `finish_repl_at_eof` が非空bufferの継続入力EOFで実Parserのparse診断をstderrへ出し終了コード1。空bufferは0。未閉じブロックは canonical `入力が未完結です: end が必要です`。`tests/integration.rs` の `aud033_*` でtree/VM一致を固定。[次期意味論・実装決定](semantic-decisions.md)第8節） |
| AUD-034 | `path_join`の引数型契約を厳格化する | `path_join("a", 123, "b")`が型エラーにならず`a/b`を返し、非文字列argumentを無言で欠落させる | ✅ 完了（全引数を左から右へStr検査し、最初の非Strで`builtin_type` / 「path_join の第 {position} 引数は Str である必要があります: {型名}」を返し結合を開始しない。tree/VMはAUD-049の共有registry/handlerを使うため差はなく、`tests/canonical_error_inventory.rs`にtree/VM一致の非Strケースを追加。正常系は`tests/path_join_contract.rs`でOS依存の期待値をRust `PathBuf`から構築して照合。観測挙動が変わるため仕様revisionを0.16へ） |
| AUD-035 | CLI・標準I/Oのhost panic経路を構造化する | REPLのthread spawn・stdout flush・stdin readに`unwrap()`があり、broken pipe/I/O障害でpanicする。`print`も`println!`のため`tsumugi script.tsg \| head -1`でpanicした。Unixの非UTF-8 argvは`std::env::args()`でもpanicし得る | ✅ 完了（`ErrorKind::Io`を追加し`print`の出力失敗を構造化エラーへ。CLIのbanner・prompt・stdin・spawn・argv検証は診断＋終了コード1へ。パイプ切断と非UTF-8 argvの回帰テストを追加） |
| AUD-036 | lossyな数値・OS境界変換を検証する | `exit`のi64→i32、`file_size`のu64→i64、NaN/Infを含む`to_int`/`floor`/`ceil`/`round`がwrap・飽和・0化し得る | 🟡 部分実装。Float→Int（`to_int`/`floor`/`ceil`/`round`）と`file_size`を共通checked helper（`checked_float_to_i64`/`checked_file_size_to_i64`）へ集約し、NaN/±Infinity/i64範囲外を`conversion`/`int_overflow`へ。tree/VM共有handlerで一致し、`canonical_error_inventory`・`checked_conversion_contract`・`builtin_core::aud_036_tests`で固定。仕様revision 0.17。`exit`の構造化`Exited`は Phase 2 C7（REV-023）で実装完了（`ExecutionOutcome::Exited { code }`、範囲外は`argument` error）。`usage`付き完全形はPhase 3/E11 |
| AUD-038 | benchmarkをparse / compile / executeへ分離しVM退行を調査する | 現行Criterionは毎回parseし、VMはcompileも含む。aarch64 release実測でVMはfibが約2.77倍高速な一方、loop 5000回は約358倍低速で、単純な「VMは高速」という説明が成立しない | ✅ 4フェーズへ分離し退行の原因を特定・修正（VMのforが反復ごとにコレクションを複製しO(n^2)だった）。確保量ベースのスケーリングゲートを追加。副産物としてAUD-040 / AUD-041を検出 |
| AUD-039 | binaryからlibrary moduleを利用して二重コンパイルを解消する | `main.rs`が`lib.rs`と同じ16モジュールを再宣言し、`use tsumugi::`を一切使わないため、同一ソースがlib targetとbin targetで2回コンパイルされる。単体テストも両方に取り込まれ、`cargo test`が同じテストを2回実行する（2026-08-28時点で各152件）。ビルド時間・テスト件数の解釈を歪める | ✅ 完了（binaryのローカルmodule宣言を削除し、tree-walk CLIを`Engine` facade、VM CLIをlibrary moduleの型へ移行。単体テストはlib targetで一度だけ実行） |
| AUD-040 | treeの名前付き関数self-bindingで`Value::Fn`の複製を避ける | AUD-037の呼び出し時self-bindingが毎回`Value::Fn`（body AST含む）をcloneし、呼び出しコストが関数body長に比例する。`fib(22)`で67.0ms（該当行を無効化すると42.2ms、約1.6倍） | ✅ `Value::Fn`を`Rc<FnDef>` + `Rc<captured>`へ変更（VmFnの`Rc<Chunk>`と同じ方針）。同一条件A/Bで`fib(22)` 64.5ms→21.2ms、確保量の比 15.89→1.06。確保量ベースの回帰ゲートを追加 |
| AUD-041 | コレクション読み取りで全体複製を避ける | `GetLocal`が値を複製するため、ループ内の`d[k]` / `xs[i]`読み取りがコレクション全体をコピーする。forループの反復自体はAUD-038で解消したが、一般のindex読み取り経路は残っていた。実測ではtreeの`Env::get`も同じく全体を複製し、`len`も同じ経路だった（確保量の比は両engineとも約4.0） | ✅ 完了（副作用のないindex式に限り参照読みへ。VMは`IndexLocal` / `LenLocal`へlowering、treeは変数セルを`borrow()`して読む。比は`xs[i]`が3.98→1.99（tree）/3.99→1.79（VM）、`d[k]`が4.00→2.00/4.01→1.95、`len(xs)`が3.97→1.99/3.98→1.79。goldenフィクスチャとscalingゲートを追加） |
| AUD-047 | コレクションをcopy-on-writeにして複製コストを構造的に下げる | AUD-041は副作用のないindex式だけを参照読みにしたため、`d[to_str(i)]`のように関数呼び出しを含むindex式は評価前のコレクションを読む意味論を保つために複製が残る（確保量の比4.00）。`Value::List` / `Value::Dict`を`Rc<Vec>` / `Rc<BTreeMap>`にして書き込み時に複製すれば、意味論を変えずに複製を減らせる。upvalue経由の読み取り（`GetUpvalue`のclone）も同時に解消できる | ✅ 完了（`Value::List`を`Rc<Vec<Value>>`、`Value::Dict`を`Rc<BTreeMap<String, Value>>`へ変更。全mutation（`assign_index`・push/pop/`__pop_update`・VMの`ListPush`/`DictInsert`）を`Rc::make_mut`経由にし、検査後にdetachする。cloneはハンドル共有O(1)で、書き込み時だけbackingを複製する。観測挙動は不変。alias分離・引数/返り値/ネスト/closure共有cell・equalityを両engineで固定するgolden fixture `collection_cow_alias`と、`d[to_str(i)]`・upvalue経由`xs[i]`の確保量が線形に収まるscaling gate `cow_read_allocation_stays_linear_in_both_engines`を追加） |
| AUD-042 | treeのclosure捕捉範囲を自由変数へ絞る | treeは`capture_all()`で定義時に見える全bindingを捕捉するため、クロージャを保持するコンテナ（`push(saved, fn ...)`の`saved`等）まで捕捉し、cell→list→closure→captured→cellの参照循環でメモリが解放されない。200回×200個で51.8MB（循環しない書き方では2.19MB）。VMは自由変数だけをupvalue化するため発生しない。捕捉範囲の統一は生成コストの削減にもなる | ✅ 完了（本体で言及される名前だけを捕捉。生存量は400個で345,796→0バイト、定義コストは可視binding 100個で19,640,166→2,560,166バイト。生存量ベースと定義コストのscalingゲート、tree/VM両engineのfixtureを追加） |
| AUD-046 | treeの関数呼び出しでglobal scopeの複製を避ける | `push_call_frame`が`self.scopes[0].clone()`でglobal scopeのHashMapを呼び出しごとに複製するため、呼び出しコストがtop-level bindingの数に比例していた。global 5個と100個で同じ関数を2,000回呼ぶと確保量の比が3.67（AUD-042前は7.66）。VMは同条件で1.03。cellは`Rc`共有なので値は複製されないが、entry数ぶんのRc複製とHashMap確保が毎回発生していた | ✅ 完了（スコープスタックを差し替えず`frame_base`で探索範囲を限定。確保量は2,000回の呼び出しでglobal 100個のとき12,203,174→2,349,174バイト、比3.67→1.01。releaseの実時間は`fib(22)` 22→17 ms、global 100個×20,000回 56→8 ms。可視性の単体テストとscalingゲートを追加） |
| AUD-044 | 完了済み非適合と古い記述をREADME・規範仕様から除く | 仕様revision表記の不一致（当時`README.md` 0.5 / `language-spec.md` 0.6）、完了済み非適合（captured collectionへのindex代入）の掲載、`README.md`の構成図が`lib.rs` / `builtin_core.rs` / `limits.rs` / `sandbox.rs` / `module.rs` / `tests/defensive_vm.rs`を欠くこと、`env.rs`を「関数テーブル」と説明すること、examplesを3件中1件しか挙げないこと、CI手順が矢印区切りでコピー実行できず`cargo clippy`の`--`を欠き3 OS matrixとcoverage jobを記載していないこと。組み込み関数53個とLICENSE(MIT)の記載は実測と一致する | ✅ 完了（revision表記はAUD-043で揃え、以後0.10まで追随。完了済み非適合は`language-spec.md`から除去しID付きの表へ置換。構成図は`src/*.rs` 19件が実ファイルと1対1で一致することを確認し、examplesも3件掲載。CI手順は3ジョブの表とコピー可能なコマンド列へ置換） |
| AUD-045 | 配布・実行手順とtoolchainの下限を明示する | READMEのクイックスタートは`cargo build` / `cargo run`だけだが、エラーメッセージの例は`$ tsumugi file.tsg`を使う。`cargo install --path .`やPATH設定、再導入手順の記載がない。`Cargo.toml`はedition 2024を要求しながら`rust-version`を宣言せず、`rust-toolchain.toml`もないためCIはstable追従で、compiler版の下限を検証できない。release / install workflowとcommit SHA固定のaction参照もない | 🟡 設計確定・未実装（[検証・リリース・運用設計](verification-release-operations.md): MSRV Rust 1.97、install/release、artifact/OCI/運用gate。現行workflow・manifestは未変更） |
| AUD-048 | 捕捉のない関数値の同一性判定を統一する | AUD-014は「関数値は同一の関数値とだけ等しい」を規範としたが、同一性の粒度がengine間で揃っていない。`fn make() return fn(x) x end end` に対する `make() == make()` がtreeで`false`、VMで`true`になる。捕捉なし名前付き関数の2回生成、ループ内で同じlambda式を2回評価した2値の比較でも同じ差が出る。原因は`src/value.rs`の`PartialEq`で、treeの`Value::Fn`は定義式の評価ごとに新しい`Rc<FnDef>`を作るのに対し、VMの`Value::VmFn`はcompile時に共有される`Rc<Chunk>`とupvalue cellで比較するため、`upvalues`が空だと`all`が真になる。upvalueを持つ関数値、および別々に書いた同形lambdaの比較は両engineで一致する。AUD-014の486ケース網羅比較は型の組み合わせを対象としたため、同一sourceから複数インスタンスを作る形を含んでおらず検出できなかった | ✅ 完了（`src/value.rs`に`FunctionId(u64)`を新設し、`Value::Fn` / `Value::VmFn`へ`id`を追加、`PartialEq`をID比較だけに統一。treeは`Evaluator`、VMは`Vm`に単調増加counterを持ち、FnDef/Lambda評価・`MakeClosure`実行のたびに発番する。VMは定数テーブルにid未確定のプロトタイプ`VmFn`を置き、capture 0件でも必ず`MakeClosure`を通して実行時に一意IDを付与する。IDはrollbackしても再利用しない。`make()==make()`はtree/VMとも`false`に統一し、backend別期待ファイル`comparison_semantics.expected.vm`を削除。REPL跨ぎのID非再利用回帰テスト（tree/VM）とcounter overflowのfault injection unit test（tree/VM）を追加。仕様revisionを0.13へ） |
| AUD-049 | builtin名一覧の3重管理を解消する | スクリプトから呼べるbuiltin名が3か所に分散している。`builtin_core.rs`の`dispatch`（46名。`__pop_update`は内部専用）、`builtin.rs`の`match name`（53名、`_ => Ok(None)`で終わる）、`compiler.rs`の`is_builtin()`（52名。`print`は予約tokenのため別扱い）。現状は3リストが整合しているが、追加時に1か所でも漏らすと「treeでは呼べるがVMでは呼べない」状態になり、compile errorではなく実行時の`name`エラーとして現れる。`design.md`が「`builtin_core.rs` + dispatchへの登録のみで両engineに反映される」と書いていたのはこの構造と矛盾していた | ✅ 完了（`src/builtin_registry.rs`に単一`BuiltinSpec` registry（`PUBLIC_BUILTINS`）を新設し、tree委譲判定・Compilerの`is_builtin`・context判定・VM dispatchをすべてregistryから導出。手書き名一覧を3か所とも除去。VM `CallBuiltin` opcodeを名前文字列から`BuiltinId`へ変更しtypoをcompile時検出。内部`__pop_update`はpublic registryから外し`pop`書き戻し専用の`OpCode::PopUpdate`へ隔離してsourceから到達不能化。registry内unit test 9件と`tests/builtin_registry_contract.rs` 3件で一意性・往復・tree/VM名前解決・`__pop_update`非到達を自動検査。HostFunction registry連携（[Capability Model仕様](capability-model.md)）はPhase 2で別途） |
| AUD-050 | `MAX_CALL_DEPTH`を`limits.rs`へ集約する | 構造的上限のうち`MAX_AST_DEPTH`と`MAX_IMPORT_DEPTH`は`src/limits.rs`にあるが、`MAX_CALL_DEPTH = 128`だけ`eval.rs`と`vm.rs`に同じdoc commentで二重定義されている。値とエラー文面は一致しているが、境界の数え方が揃っておらず、tree（`call_stack`が空開始）とVM（`frames: vec![frame]`でtop-level込み）で到達できる再帰深度が1段ずれる（AUD-017）。定義が分かれていることがこのズレを見つけにくくしている | ✅ 完了（`MAX_USER_CALL_DEPTH = 128`を`src/limits.rs`へdoc contract付きで集約。`eval.rs`/`builtin.rs`/`vm.rs`のローカル定義を削除し共有定数を参照。VMは`active_user_frame_count()`でroot frameを除いて数え、AUD-017の1段ズレも同時に解消） |

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

### 2026-08-28 ドキュメント整合監査

対象はcommit `4396ffa`、aarch64 Linux。`README.md` / `LANG_GUIDE.md` / `docs/*.md` の記述を実装と実行結果に突き合わせた。テスト件数は次のとおりで、これ以前のスナップショットの件数は当時のcommitに対する記録として残す。

| 種別 | 件数 |
|---|---:|
| 単体テスト（lib / bin でそれぞれ実行、AUD-039） | 152 |
| 統合テスト（`tests/integration.rs`） | 172 |
| 防御的テスト（`tests/defensive_vm.rs`） | 8 |
| スケーリングテスト（`tests/scaling.rs`） | 6 |

検出した記述の誤りは3種類に分かれた。

**規範仕様に反するもの:** `LANG_GUIDE.md`のClosures節と`design.md`の既知のトレードオフが、AUD-014で廃止した「関数値は常に等しくない」という旧方針を残していた。`LANG_GUIDE.md`は同じファイルのOperators節と矛盾していた。`README.md`と`design.md`はengine差の残存領域として「比較・index代入・builtin・import」を挙げていたが、これらはAUD-014 / AUD-013 / AUD-012 / AUD-030で統一済みで、実際に残る差を1件も挙げていなかった。`language-spec.md`の既知非適合にはAUD-017が載っていなかった。

**実装に追随していないもの:** `design.md`の`env.rs`節が`Env::functions`（廃止済み）と`capture_all`（AUD-042で`capture_referenced`へ置換）を残し、`frame_base`（AUD-046）に触れていなかった。ユニットテスト表は8モジュールのうち5つしか挙げず、存在しない`get_mut`を観点に挙げていた。スケーリングテスト節は6性質のうち2つしか説明していなかった（`tests/scaling.rs`のmodule docもAUD-041の1件を欠いていた）。`builtin_core.rs`の説明は「~45個の純粋関数」だが実数は`builtin_*`が47個で、うち15個はfilesystem・env・clockに触る。CI手順は3ジョブ・3 OS matrix・coverage jobを反映していなかった。

**文書間で重複し乖離したもの:** 形式文法が`LANG_GUIDE.md`と`design.md`に二重に置かれ、`index_assign`（両方がAUD-013前の`postfix`）と`dict_literal`（`LANG_GUIDE.md`だけが実装と異なる`STRING`キー）で食い違っていた。文法は`LANG_GUIDE.md`の1か所に集約し、`design.md`はv0.3からの差分だけを残した。

この監査で新たに再現・記録した項目は **AUD-048**（捕捉のない関数値の同一性がengine間で異なる）、**AUD-049**（builtin名一覧の3重管理）、**AUD-050**（`MAX_CALL_DEPTH`が`limits.rs`外に二重定義）である。AUD-048はgolden fixtureで差分を固定した。

仕様revisionは0.11へ上げた。0.9（AUD-014）と0.10（AUD-030）は観測挙動の変更に伴う更新だったが、今回は**観測挙動を一切変えていない**。それまで文書化していなかった制約（`return`の式必須、辞書キーの順序、`contains`の辞書での対象、`slice`の負値、`has_key`のキー型、`format_time`の型とUTC、`to_int`の文字列受付範囲）を規範仕様へ明記したため、規範として拘束する内容が増えたことを示す更新である。

`language-spec.md`の組み込み関数表では、実行して確認した挙動と説明が食い違う箇所を修正した。`contains`が辞書ではキーを見ること、`keys` / `values` / `for`の辞書反復が「アルファベット順」ではなくコードポイント順（`"Z" < "a"`）であること、`slice`の負値が0クランプでindexアクセスの末尾参照とは非対称であること、`has_key`のキーがStr限定であること、`format_time`がInt限定・UTC固定であること、`to_int`が`"3.5"`や`" 42"`を受けないこと、`return`に式が必須であることを明記した。

## 2026-09-07 詳細レビューバックログ（REV）

commit `092da35d0a01c6e3f403123df8416ec0819746d7` を対象に、production source・test・全設計文書を横断した詳細レビューを実施し、25件の指摘を **REV-001〜REV-025** として記録した。レビュー報告書の正本は [`semantic-review/Tsumugi-detailed-review-20260907.md`](../semantic-review/Tsumugi-detailed-review-20260907.md) である。

このレビューの環境では `rustc` / `cargo` が使えず、`cargo build` / `cargo test` / `cargo clippy` / fuzz / sanitizer は未実施である。「確認」とあるものは、明示しない限りコード経路を静的に追跡して成立を確認したものである。

指摘のうち、レビュー時点で**設計が不足していた（設計欄が △ または ×）6件**（REV-003 / 004 / 005 / 009 / 012 / 021）は、次期実装仕様を [次期意味論・実装決定](semantic-decisions.md)第17節に追加済みである。残りの指摘は既存の各設計文書と該当節を正本とし、設計を新規追加せず実装バックログへ直結させる。REV-012 は既存の第5節（AUD-017）が正本であり、意味論変更ではなく status / doc drift の解消のみを扱う。REV-006 は §17.2 / §17.3 が範囲検証を委譲する検証層であり、実装レベルの検査項目（V1〜V9）と 2 層モデル（per-instruction step 課金 + verifier）を §17.7 として後から確定した。

> **状態の読み方:** 下表の最終列はコード・test・workflow・配布物の**実装状況**であり、設計状態ではない。設計正本で判断が確定していても、受入gateを満たす実装がなければ完了扱いにしない。REV は既存 AUD 番号を再割り当てせず、独立した追跡項目として扱う。

### P0 — Critical

| ID | 項目 | 到達範囲 | 設計状態 | 実装状況 |
|---|---|---|---|---|
| REV-001 | 共有DAGの構造比較・表示・join・sortが指数時間／指数出力になる。value graph traversalへnode/depth/fuel/bytes上限とvisited pair管理を導入し、表示をHumanDisplay/CanonicalRepr/TotalOrderKeyへ分離する | 通常script | 検討○・設計○（[実行予算・協調実行](execution-control.md)、REV-015受入条件へ追加） | ⬜ 未実装 |
| REV-002 | 非UTF-8 canonical import pathが空文字へ変換され、sandbox認可対象とI/O対象が分離する。認可APIを`&Path`／認可済みhandleへ変更する | Unix + import | 設計○（次期path-handle方式、[capability-model](capability-model.md)） | ⬜ 未実装 |
| REV-006 | 未検証bytecodeの`Jump(0)`等でstep課金を迂回し無期限実行できる。raw `Call`もcall課金を迂回する。`VerifiedChunk`とverifierで検証済みbytecodeだけをVMへ渡す。停止性はper-instruction step課金（全命令dispatchごとに1、verifier非依存）で担保し、verifierは早期拒否・不変条件確立・defense-in-depth | 公開raw bytecode | ✅ 確定済み（[次期意味論・実装決定](semantic-decisions.md)§17.7。実装レベルの検査項目V1〜V9・2層モデルを確定） | ✅ 完了。`src/verifier.rs`に`VerifiedChunk`（生成は`verify`か信頼済み`from_trusted`のみ）と`verify`（V1〜V9をプロトタイプ木へ再帰適用、最初の違反を`ChunkVerifyError`で返し`internal`へ写像）を新設。VMの`run_frames`は全命令dispatch直前に無条件`count_step`（層1）へ変更し、`Loop`/`PrepareCall`/`call_fn_value`の個別課金を廃止（深度上限は`PrepareCall`と`Call`の両方で検査）。`Vm::new`/`run_repl_chunk`は`VerifiedChunk`だけを受け取り、compiler出力は`from_trusted`で昇格。停止性はper-instruction課金でverifier非依存。`tests/defensive_vm.rs`に`Jump(0)`/`Loop(0)`自己ループと`PrepareCall`なしraw `Call`再帰の有限停止を追加、verifier unit 13件でV1〜V9拒否と正規出力通過を固定。観測挙動はstep到達点以外不変（`error_step_limit`等のgoldenは不変で通過） |
| REV-015 | source・string・heap・I/O・bulk workが包括的に有限化されていない。reserve-before-allocate等でsource/string/heap/I-O budgetとdeadline/cancelを実装する。REV-001のDAG増幅も受入条件へ含める | 通常script | 検討◎・設計◎（[実行予算・協調実行](execution-control.md)、[capability-model](capability-model.md)） | 🟡 部分実装。Slice 1（budget型・`BudgetLedger`・checked reserve/commit・固定優先順位・fake clock・legacy adapter）と Slice 2 の string accounting（per-item `SingleStringBytes`・cumulative `StringAllocations`/`StringBytes` を共有 builtin handler 経由で tree/VM 共通課金）、source/import accounting（per-item `SingleSourceBytes`・cumulative `SourceCount`/`SourceBytes`・`ImportCount`/`ImportBytes` を `charge_source`/`charge_import` で `Link` フェーズに課金。root を source の 1 本目に数え、初めて解決した import は `source_bytes` と `import_bytes` の両方へ課金。`ModuleLoader::link` が import の生 byte 長を `LoadedModule` で返し、tree（`Evaluator::charge_link`）と VM（`Vm::charge_link`）が共通規則で課金。legacy env `TSUMUGI_MAX_SINGLE_SOURCE_BYTES`/`_SOURCE_COUNT`/`_SOURCE_BYTES`/`_IMPORT_COUNT`/`_IMPORT_BYTES` を追加）、heap accounting 基盤（`AllocationId`・per-execution `AllocationLedger`・§5.1 論理サイズ関数群・`HeapBytes` の checked 課金と §7.2 優先順位での超過写像・`AllocationId` overflow の `InternalFailure`・`usage()` の live/peak 反映。legacy env `TSUMUGI_MAX_LIVE_HEAP_BYTES`）、collection（`List`/`Dict`）の per-drop release（`Rc<Tracked<T>>` で生成時に §5.1 body サイズを課金し最後の参照 drop で release、`Rc::clone` 共有は無課金、COW mutation は detach＋retrack の delta 課金、builtin_core は untracked 生成＋dispatch 境界 `track_result` で tracked 化、AUD-024 rollback は drop 連鎖で自動 release、tree/VM 両対応）を実装。`charge_context_baseline` は二重計上回避のため `charge_link` から外した（純関数として単体テストのみ）。String body の per-drop release（PR-b、`Value::Str` を `Rc<Tracked<String>>` 化し、builtin 結果を dispatch 境界の `track_result` で live heap 課金・最後の参照 drop で release。cumulative `StringAllocations`/`StringBytes` は据え置きの別会計、tree/VM 両対応）も実装済み。さらに cell・tree/VM 関数 instance の per-drop release（PR-c、`SharedValue` を `Rc<Tracked<RefCell<Value>>>` 化し cell 生成点で captured cell を課金、`Value::Fn`/`VmFn` の header token で function instance header を課金）も実装済み。さらに AST / bytecode chunk / imported module record / rollback journal の per-drop 追跡（PR-d、所有構造側が `HeapToken = Tracked<()>` トークンで §5.1 論理サイズを課金し drop で release。tree は AST・VM は bytecode chunk、import record は `ModuleLoader` が `loaded` set と寿命を揃えて保持し `forget`/loader drop で release、rollback journal entry は tree/VM とも entry の固定 overhead 48 byte を課金し submission commit/rollback で release）も実装済みで、全 per-drop 対象が揃い charge_link の baseline 再走査は不要。既定上限では観測挙動は不変。残りは I-O accounting、string リテラル/連結/f-string 経路、deadline/cancel、REV-001 の DAG 増幅受入条件。VM の push/pop full-clone 差・空リテラル差は Slice 6（VM charge parity）で解消 |
| REV-023 | script `exit()`がホストプロセスを終了する。process-globalなexitをやめ、構造化`Exited` outcomeを返す | embedding | 検討◎・設計◎（[実行予算・協調実行](execution-control.md)、AUD-036と同基盤） | ✅ 完了（Phase 2 C7。`ExecutionOutcome::Exited { code }`・ProcessExit capability・CLI 境界での exit code 写像。`usage: BudgetUsage` field は Phase 3/E11） |

### P1 — High

| ID | 項目 | 到達範囲 | 設計状態 | 実装状況 |
|---|---|---|---|---|
| REV-003 | Int–Float比較が2^53超で誤り、`==`が非推移的になる。整数を丸めずFloatのbit表現から数学的に正確に比較する共通`NumericOrder`を導入し、`min`/`max`は選択したoperandを元の型で返す | 通常script | ✅ 確定済み（[次期意味論・実装決定](semantic-decisions.md)§17.1） | ✅ 完了。`src/value.rs`に共通`NumericOrder`（Int を`f64`へ丸めず`trunc`/`fract`分離+`i128` widenで厳密比較、`±Inf`/`NaN`対応）を新設し、`PartialEq`のInt×Float、tree(`eval.rs`)・VM(`vm.rs`)の関係演算子、`min`/`max`をこれへ集約。`contains`は`PartialEq`経由で自動追随。`min`/`max`は選択operandを元の型で返し同値時は第1引数・NaN入力はcanonical NaN。`sort`は現行仕様どおり文字列表現比較のまま（別課題）。value.rs unit 11件・golden fixture `numeric_strict_comparison`（tree/VM一致）を追加。仕様revision 0.19 |
| REV-004 | 公開`Chunk::patch_jump`が範囲外／非jump offsetでpanicする。`patch_jump`をfallible化し、builderを`pub(crate)`／feature gateへ封印する | 公開low-level API | ✅ 確定済み（[次期意味論・実装決定](semantic-decisions.md)§17.2） | ✅ 完了。`patch_jump`を`Result<(), ChunkBuildError>`（`BadOffset`/`NotAJump`、エラー時は`code`を書き換えず部分破損を残さない）へ変更。compilerは`self.patch_jump(offset)?`で内部エラーへ写像し全9箇所を`?`伝播。raw builder（`chunk` module）はREV-018と共通で`unstable-bytecode` feature下へ封印 |
| REV-005 | 不正`MakeClosure` descriptorをNull captureとして黙認する。capture記述子を明示化し、不整合は`internal` errorにする（REV-006と同一マイルストーン） | 公開raw bytecode | ✅ 確定済み（[次期意味論・実装決定](semantic-decisions.md)§17.3） | ✅ 完了。`MakeClosure`のoperandを「upvalue数」から「プロトタイプindex」へ変更し、`Chunk.prototypes: Vec<FunctionPrototype>`と明示`CaptureDesc`（`Local`/`Upvalue`）を導入。compilerは隣接`GetLocal`/`GetUpvalue`列の暗黙契約を廃止しプロトタイプへcapture記述子を格納、VMは記述子からcellを解決し`usize::MAX`/Null cellフォールバックを削除（不正記述子は`internal`。範囲はverifierのV5で拒否）。REV-006と同一build |
| REV-007 | `ExecutionContext`がsession stateとrun meterを混在し、`execute`間でstepを累積する。meterをrequestごとに分離する | stable facade | 設計◎（[実行予算・協調実行](execution-control.md)、`ExecutionRequest`） | ⬜ 未実装 |
| REV-008 | 通常`Engine::execute`がtransactionでなく、エラー前のstate mutationを保持する。全stable executionへtransactionを適用する（AUD-024はREPL限定で完了） | stable facade | 設計◎（[組み込みAPI](embedding-api.md)、[実行予算・協調実行](execution-control.md)、AUD-024） | 🟡 REPLのみ実装。全execution transactionは未実装 |
| REV-011 | language revision・engine差・実装statusが複数文書でdriftしている。機械可読な単一正本（project-metadata）から生成し、CIでstale literalを検査する | 文書・release | 設計◎（報告書に受入条件、§17との連携） | ⬜ 未実装 |
| REV-012 | call評価順が現行仕様・実装（callee評価前検査）と次期仕様・code comment（callee先行）で矛盾し、完了表示も不整合。意味論の正本は第5節（AUD-017）で、statusとdoc driftのみ解消する | call semantics | ✅ 確定済み（[次期意味論・実装決定](semantic-decisions.md)§17.5、第5節が正本） | 🟡 depth-counting実装済み。callee-precedence切替は未実装 |
| REV-013 | `args()`がhost process argvを読み、tree/VMで解析規則も異なる。runtime coreから`std::env::args_os()`を除去し、`ExecutionRequest.arguments`のみを公開する | embedding | 設計◎（[組み込みAPI](embedding-api.md)、AUD-018） | ✅ 完了（AUD-018 E8aと一体）。`src/builtin.rs`（tree）・`src/vm.rs`（VM）の`args()`から`std::env::args_os()`を除去し、`Evaluator` / `Vm`のscript_args snapshotを返すよう統一。tree/VMで解析規則差（skip 2 vs skip 1+`--vm` filter）も解消。CLIのみが`std::env::args_os()`をargv取得に使い、runtime coreからは参照しない |
| REV-014 | sandbox/env/limits/stdio/clockがprocess-globalまたはfirst-use global。`EngineConfig`／`ExecutionRequest`へ移し、library coreが`std::env`等を直接参照しないようにする | embedding | 設計◎（[capability-model](capability-model.md)、[組み込みAPI](embedding-api.md)、AUD-014と関連） | ⬜ 未実装 |
| REV-018 | internal module／raw bytecodeの公開が安全境界とstable surfaceを弱める。stable rootをEngine等へ限定し、internalsを`pub(crate)`にする（REV-004〜006の根因） | 公開API | 設計◎（[組み込みAPI](embedding-api.md)、§17.2と共通の封印作業） | ✅ 完了。`chunk`/`compiler`/`opcode`/`verifier`/`vm`を`unstable-bytecode` feature（`default`で有効）でのみ`pub`にし、feature無効時は`pub(crate)`。安定利用者は`default-features = false`でraw bytecode surfaceを封印できる。安定embedding surfaceは`Engine`系のみ。`Vm::new`はREV-006により`VerifiedChunk`だけを受け取るため、feature有効でもraw `Chunk`を直接VMへ渡せない |
| REV-020 | import先parse errorの原因を捨て、wrapper messageだけを返す。原因診断を保持する | import diagnostics | 設計○（[組み込みAPI](embedding-api.md) error契約） | ⬜ 未実装 |
| REV-021 | 現行`remove_dir`は再帰削除だが次期capabilityは`EmptyDirectory`へ割当て。`remove_dir`を空のみへ変更し、再帰削除を`remove_tree`（`RecursiveDelete`）へ分離する | filesystem capability | ✅ 確定済み（[次期意味論・実装決定](semantic-decisions.md)§17.6） | ⬜ 未実装 |

### P2 — Medium / Quality

| ID | 項目 | 到達範囲 | 設計状態 | 実装状況 |
|---|---|---|---|---|
| REV-009 | `list_dir`がentry errorを黙殺し、非UTF-8名をlossy変換して衝突させる。個別entry errorをstructured host errorにし、非UTF-8名を`invalid_encoding`にする | filesystem | ✅ 確定済み（[次期意味論・実装決定](semantic-decisions.md)§17.4、[capability-model](capability-model.md)） | ⬜ 未実装 |
| REV-010 | 32-bit targetで`i64 as usize`がwrapし、limit迂回・巨大collectを起こし得る。`try_from`とchecked arithmeticへ置換する | 32-bit target | 設計○（[実行予算・協調実行](execution-control.md) budget受入） | ⬜ 未実装 |
| REV-016 | scriptからRc cycleを作れ、context再利用で回収不能heapが累積する。baseline heap課金・`clear_user_state()`・tenant跨ぎ再利用禁止で近期対応し、中期はarena+GCを検討する（AUD-042は偶発cycle削減で完了済み） | 長寿命context | 検討◎・設計○（[設計](design.md)、AUD-042） | ⬜ 未実装 |
| REV-017 | human Displayが非escapeで、sort key・repr・outputが同じ表現へ結合されている。HumanDisplay/CanonicalRepr/TotalOrderを分離する（REV-001と連動） | 通常script | 設計△（sort仕様は記載、分離設計は不足） | ⬜ 未実装 |
| REV-019 | `now()`がepoch前／clock errorを0にし、`u64 as i64`も未検査。clock capabilityとchecked変換で扱う | clock | 設計◎（[capability-model](capability-model.md)、[実行予算・協調実行](execution-control.md)） | ⬜ 未実装 |
| REV-022 | EOF・I/O error・permission等をNull/falseへ畳み、原因とdenialを区別できない。safe profileでdenialとOS失敗を区別する | I/O API | 設計◎（[capability-model](capability-model.md)、[組み込みAPI](embedding-api.md)） | ⬜ 未実装 |
| REV-024 | 現行CIはrolling stable・mutable action・`--locked`なし・fuzz/stress/MSRVなし。MSRV固定・action pin・release/fuzz/stress gateを段階導入する | CI／release | 設計◎（[検証・リリース・運用設計](verification-release-operations.md)、AUD-024/045と関連） | 🟡 3 OS CI・golden・scaling・defensive testのみ実装 |
| REV-025 | source/token制限に加え、parse diagnostic件数にも上限がない。診断件数上限を設ける | compile | 設計△（source/token上限は記載、diagnostic件数は未記載） | ⬜ 未実装 |

REV 由来項目の実装順は、[次期意味論・実装決定](semantic-decisions.md)第17節「移行順」と、後述の「設計sliceに沿う推奨実装順」に統合済みである。REV-001 / 015（包括budget）と REV-002 / 006 / 018（bytecode検証・API封印）は基盤に属するため、境界挙動より前に置く。

### 設計決定crosswalk

次の表は、監査で未決定または未完了として記録された論点を、設計正本と現行実装へ対応付ける。**設計状態が確定でも、実装状況が未実装・部分実装ならAUD完了ではない。**

| AUD | 設計状態 | 決定概要 | 正本 | 実装状態 |
|---|---|---|---|---|
| AUD-018 | 確定済み | CLIは`tsumugi [OPTIONS] [SCRIPT [ARGS...]]`とし、script引数を`ExecutionRequest.arguments` snapshotへ渡す。入口統合をE8a、profile/options移行をE8bに分離 | [次期意味論・実装決定](semantic-decisions.md)第6節、[組み込みAPI仕様](embedding-api.md)第12・14〜16節 | 🟡 E8a完了。CLIの`--vm` / `--` / `-`(stdin) / SCRIPT / ARGS grammar subsetを実装し、script引数を注入。`args()`はtree/VMともprocess argvを読まずcontext snapshotを返す。非UTF-8 argvは`エラー: コマンドライン引数はUTF-8で指定してください`+終了1。parse_cli単体8件・engine_api契約2件・統合4件で固定。仕様revision 0.18。E8aでCLIのimportなしtree file/stdin実行を embedding Engine API（`compile`→`link`→`run`）だけへ統合し、引数転送を`EmbeddingRequest::with_arguments`→`Engine::run`経由へ、`ExecutionOutcome`→exit code変換を第12節どおりに載せ替えた（診断は`format_execution_error`で現行`TsumugiError` Displayとbyte一致、exit code mapping/formatterのunit test 7件追加）。import root は`link`の`FeatureUnavailable { feature: "module_resolver" }`を検出して alpha facade（`run_source_alpha`）へフォールバック、REPLは状態継続・import解決のため alpha 維持。import・REPLのEngine API統合はE7、VMはE9。capability profile/options（`--profile` / `--allow-*` / `--fs-*`）と`--help` / `--version`のterminal action、safe/legacy移行はE8b（Phase 2）で別追跡（REV-013と同基盤） |
| AUD-019 | 確定済み | operation別の単一constructorからcanonical kind/message/line/traceを生成し、backend固有診断とmessage推測を廃止 | [次期意味論・実装決定](semantic-decisions.md)第3節 | ✅ 完了（language core分）。operation別constructorへ全移行、`classify_runtime_error`削除、tree/VM完全一致をinventory/pairedテストで固定。host adapter error（HostErrorSpec）はPhase 2で別追跡 |
| AUD-020 | 確定済み | Tsumugi単体をsecurity boundaryとせず、filesystemはportable path-handleで認可と利用をbindし、TOCTOU・oracle・dangling symlinkを受入試験化 | [脅威モデル](threat-model.md) TM-002〜004・第11節、[Capability Model仕様](capability-model.md)第8節・CAP-AT-10〜14 | 部分実装。現行制約の文書化のみ完了、path-handle未実装 |
| AUD-022 | 確定済み | timeout/golden/differential/limit/defensive matrixに加え、fuzz・stress・failure injection・資源制約gateを段階導入 | [検証・リリース・運用設計](verification-release-operations.md)第4〜6・17節 | 部分実装。harness、timeout、完全一致、temp分離は完了。matrix/fuzz/stressは未実装 |
| AUD-024 | 確定済み | `Completed` / `Exited`だけ全language-stateをcommitし、その他terminalはexecution開始時点へrollbackする。catch済みerror後に最終完了した実行はcommitする。stdout/filesystem/network/DB/host function等の完了済み外部効果はrollbackしない | [次期意味論・実装決定](semantic-decisions.md)第7節、[実行予算・協調実行仕様](execution-control.md)第10節、[組み込みAPI仕様](embedding-api.md)第10節 | ✅ 完了（REPL submissionの未捕捉errorで全language-stateをrollback、正常完了・catch済み完了はcommit、外部効果はrollbackしない。first-write undo logで記録量は変更箇所数に比例。deadline/budget/cancel terminalはPhase 3/4で別追跡） |
| AUD-034 | 確定済み | `path_join`は全argumentをStrとして検査し、非Strを無言で欠落させない | [次期意味論・実装決定](semantic-decisions.md)第9節 | ✅ 完了（`builtin_path_join`で全引数を左から右へStr検査し、最初の非Strで`builtin_type`エラーを返す。tree/VMは共有handlerで一致。error inventoryと`path_join_contract`テストを追加。仕様revision 0.16） |
| AUD-036 | 確定済み | `exit`、file size、Float→Intのlossy変換を共通checked helperで拒否し、valid `exit`は構造化`Exited`にする | [次期意味論・実装決定](semantic-decisions.md)第10節 | 🟡 ほぼ完了。Float→Intとfile_sizeのchecked変換（仕様revision 0.17）、valid `exit`の構造化`Exited`（Phase 2 C7 / REV-023）を実装済み。残るは`Exited`の`usage: BudgetUsage` field（Phase 3/E11） |
| AUD-045 | 確定済み | MSRVをRust 1.97とし、stable/MSRV CI、install/release、6 platform artifact、署名・SBOM・OCI、参照用Kubernetes Jobの順序とgateを固定 | [検証・リリース・運用設計](verification-release-operations.md)第3〜10・17〜18節 | 🟡 部分実装（VRO Slice 1）。`Cargo.toml` に `rust-version = "1.97"`、CI に 1.97.0 pin の `msrv` job（`cargo test --all-features --locked`）を追加し、clippy を `--all-targets --all-features`、test を `--all-features --locked` へ。これで CI が検証に使う toolchain が固定され、rolling stable の新 lint による差分すり抜けと独立に MSRV を担保する。install/release workflow・6 platform artifact・署名・SBOM・OCI・Kubernetes manifest と README install 節、coverage fail-under・docs job（VRO Slice 2 以降）は未実装 |
| AUD-048 | 確定済み | function/lambda式の**動的評価ごと**にfresh `FunctionId`を発行し、clone/captureは同じIDを保持、rollback後もIDを再利用しない | [次期意味論・実装決定](semantic-decisions.md)第12節、[決定性・実行時監査仕様](determinism-and-audit.md)第4.7節 | ✅ 完了。tree/VMとも関数等価性を`FunctionId`比較へ統一。VMはcapture 0件でも`MakeClosure`で実行時発番し、backend別期待ファイルを削除。REPL rollback非再利用・overflow fault injectionのテストを追加 |
| AUD-049 | 確定済み | 単一`BuiltinSpec` / callable catalogからtree、VM、compiler、arity、context metadata、生成文書を導出。HostFunction registryは別registryだが共通resolverで衝突検査 | [次期意味論・実装決定](semantic-decisions.md)第13節、[Capability Model仕様](capability-model.md)第17節・CAP-AT-20 | ✅ 完了（language core分。`src/builtin_registry.rs`の`PUBLIC_BUILTINS`をtree/VM/compiler/arity/context metadataの正本にし、`CallBuiltin`をBuiltinId化、`__pop_update`を`OpCode::PopUpdate`へ隔離。HostFunction registryとの共通resolver衝突検査はPhase 2で別追跡） |
| AUD-050 | 確定済み | `MAX_USER_CALL_DEPTH = 128`を`limits.rs`へ集約し、root frameを数えずactive user call数で統一。128個目を許可し129個目の直前で拒否 | [次期意味論・実装決定](semantic-decisions.md)第5節 | ✅ 完了（AUD-017と一体）。`limits.rs`へ集約、VMは`active_user_frame_count()`でroot除外計数。tree/VMとも128 user frameを許可し129個目を同じline/message/traceで拒否。境界値・相互再帰・lambda・callbackの回帰テスト追加 |

### 設計sliceに沿う推奨実装順

1. **基準固定:** [次期意味論・実装決定](semantic-decisions.md)第17節の基準固定と、現行非適合fixtureを維持する。文書の設計確定を実装完了として扱わない。
2. **意味論基盤:** 内部refactor → ~~AUD-050/017深度統合~~（✅ 完了） → ~~AUD-049単一BuiltinSpec~~（✅ 完了） → ~~AUD-019 canonical error~~（✅ 完了） → ~~AUD-047 COW~~（✅ 完了）/~~AUD-048 FunctionId~~（✅ 完了） → ~~AUD-016 binding~~（✅ 完了）/~~AUD-024 transaction~~（✅ 完了） → ~~AUD-034 path_join~~（✅ 完了）/AUD-036境界挙動（🟡 Float→Int/file_size完了、`exit`はREV-023へ）/~~AUD-018 E8a~~（✅ 完了）/033の順で進める。
3. **Phase 1 embedding:** [組み込みAPI仕様](embedding-api.md) ~~E1~~（✅ 完了、`src/embedding.rs`）→~~E2~~（✅ 完了、compile / import なし link / byte-level hash）→~~E3~~（✅ 完了、tree backend adapter・実行入口。runnable AST は案 A＝実行時に保持 source を再 parse、§4.1 に記録）→E4〜E6→E8a。最終terminal型のsubsetを使い、先行公開型を作らない。
4. **Phase 2 capability:** E7→E8bと[Capability Model仕様](capability-model.md) C1→C2、C3/C4/C5/C7、C6、C8、最後にC9/C10。現行ambient accessを削除するまで完了扱いにしない。
5. **Phase 3/4 control:** E11/C11で有限budget・transactionを完成し、その後E12/C12でcooperative state machine、yield/pause/resume、admission/backpressureを実装する。
6. **Phase 5/6 determinism/audit:** Determinism Slice 1〜4→Audit Slice 5→VM conformance Slice 6。E13/C13はSlice 5へ統合し、別event実装を作らない。
7. **Phase 7 verification/release:** VRO-AT-01〜15を満たし、MSRV、release/install、artifact、OCI、運用資材は前段Phaseの受入を再検証してから導入する。
8. **将来機能:** classは[次期意味論・実装決定](semantic-decisions.md)第15節で**設計済み・低優先度**。基盤と総heap budgetの後に扱う。HTTPは同第16節で**設計済み・着手禁止（具体ユースケース承認待ち）**であり、Phase 1〜6完了と着手gate承認後だけ別計画を作る。

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
- [x] bytecode検証・API封印（REV-006/004/005/018）— `VerifiedChunk`/verifier（V1〜V9）、per-instruction step課金、`MakeClosure(proto_index)`+明示`CaptureDesc`、`patch_jump`のfallible化、`unstable-bytecode` featureでのraw module封印。VM入口を検証済みchunkへ限定
- [x] スタックトレース（関数呼び出し経路のエラー表示、ツリーウォーク版/VM版両対応）
- [x] ステップ予算（ループ反復 + 関数呼び出しのカウント制限、無限ループ/無限再帰を防止）
- [x] ファイルI/Oサンドボックス（環境変数 `TSUMUGI_SANDBOX` でアクセス許可パスを制限）
- [x] モジュール / import（ファイル分割、循環import検出、ネストimport対応、実行前解決の共有ローダーでツリーウォーク版/VM版を統一）
- [x] エラー処理 / try/catch（ランタイムエラーの捕捉、ネスト対応、ツリーウォーク版/VM版両対応）
- [x] `From<String>` 廃止（全エラー生成箇所を `TsumugiError::runtime()` に統一、文字列再パース除去）
- [x] import のサンドボックス対応（`TSUMUGI_SANDBOX` 設定時に import 先パスも検証、ツリーウォーク版/VM版両対応）
- [x] 環境変数アクセス制御（`TSUMUGI_ENV_ALLOW` で env() の読み取り可能キーを許可リスト制限）
- [x] 浮動小数点 IEEE 754 の基本挙動（VMのFloatゼロ除算をinf/NaNに修正、ツリーウォークにFloat比較armを追加。異種型・複合値を含む比較parityはAUD-014で解消）
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

この移行は、設計確定済みのstable embedding APIと実行contextを先に実装してから行う。既存builtinをただ削除するのではなく、CLIが必要なcapabilityを明示的に付与する構造へ変え、同じengineを組み込み用途でも利用できるようにする。

現行の`import`はTsumugi sourceを読み込む機能であり、native host moduleやhost functionを登録する拡張境界ではない。host extension APIが成立するまで、「HTTPやDBを外部moduleで提供する器が完成した」とは扱わない。

### この境界を採用する理由

1. **最小権限** — scriptごとに必要な操作だけを付与し、ambient authorityを避けられる
2. **資源制御** — host callの時間、入出力量、同時実行数を実行予算へ含められる
3. **監査可能性** — 外部効果の許可、拒否、引数、結果をhost boundaryで観測できる
4. **予測可能性** — clock、env、filesystem等を注入し、同じ入力に対する再現性を高められる
5. **依存の分離** — HTTP clientやDB driverを言語本体へ固定せず、ホストが用途に応じて選べる
6. **テスト容易性** — 実OSや外部serviceを使わず、fake capabilityで境界動作を検証できる

### 外部機能を追加する順序

1. [組み込みAPI仕様](embedding-api.md)のE1〜E6・E8aでstable Engine入口を作る
2. [Capability Model仕様](capability-model.md)の単一callable catalog、host function registry、deny-by-default policyを実装する
3. clock、env、stdio、filesystem、process操作をhost境界へ移し、E7・E8bでsafe/legacy profileを接続する
4. [実行予算・協調実行仕様](execution-control.md)のbudget、deadline、cancellation、backpressureと、[決定性・実行時監査仕様](determinism-and-audit.md)のauditをhost callへ伝播する
5. HTTPは[次期意味論・実装決定](semantic-decisions.md)第16節の着手gateを満たし、具体的ユースケースが承認された場合だけhost adapterとして別計画を作る

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
| 低 | クラス（継承なし） | [次期意味論・実装決定](semantic-decisions.md)第15節で設計済み。基盤と総heap budget後の低優先度実装候補 |

## 設計済み・低優先度: クラス

### 背景

クラスの規範的なgrammar、identity、construction、error、受入基準は[次期意味論・実装決定](semantic-decisions.md)第15節で確定済みである。以下は判断に至った背景と候補記録として残し、実装時は同正本を優先する。

### 方針: クラスは設計確定済みとし、継承は採用しない

- **クラス構文自体**: 設計確定済み。`class ... end` でデータと操作をまとめる低優先度の実装候補とする
- **クラス継承（スーパークラス/サブクラス）**: 採用しない。理由は後述
- **合成（composition）**: 採用する。部品を「持つ」方式でクラス間の機能共有を実現する

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
# 設計確定済みクラス構文の非規範例
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
| `OpCode::CallBuiltin` の String 除去 | 関数名を定数テーブルへ移し、AUD-049 で `CallBuiltin(BuiltinId, usize)` へ更に前進（typo を compile 時に検出） | ✅ 完了 |
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
| 計算量オーダーの回帰ゲート | `tests/scaling.rs` が確保バイト数で `for` の線形性（AUD-038）、呼び出しコストのbody長非依存（AUD-040）、クロージャ定義コストの可視binding非依存（AUD-042）、呼び出しコストのtop-level binding非依存（AUD-046）、コレクション読み取りの線形性（AUD-041）を検査し、生存量でクロージャの解放（AUD-042）を検査（実時間に依存しない） | ✅ 完了 |

### コード構造

| 項目 | 詳細 | 状態 |
|---|---|---|
| `eval.rs` の `exec_stmt` 分割 | 巨大 match を独立メソッドに分離 | 未着手 |
| `Env::functions` の廃止 | 関数を変数として統合しスコープルールを一本化 | ✅ 完了 |

### 実行安全性

| 項目 | 詳細 | 状態 |
|---|---|---|
| ユーザー関数のコール深度制限 | 関数再帰を128フレームでRust実stack overflow前にエラー化。root除外のactive user frame数で数え、tree/VM境界を統一（`MAX_USER_CALL_DEPTH`を`limits.rs`へ集約） | ✅ 完了（AUD-050 / AUD-017） |
| 関数外 `return` の拒否 | 関数本体の外の`return`をパース時にエラー化し、VM REPLのhost panicと無言終了を防止 | ✅ 完了（AUD-043） |
| 構文・AST深度制限 | Parser生成時とCompiler/Evaluator入口でAST深度256を検査。nested f-stringにも親深度を継承 | ✅ 完了（AUD-027） |
| import chain深度制限 | rootを除くactive import chainをtree/VMとも128に制限 | ✅ 完了（AUD-028） |
| サンドボックスの `OnceLock` テスタビリティ | テスト時に環境変数を切り替え可能な設計にする | 未着手 |
| サンドボックスの中間シンボリックリンク迂回修正 | 新規書き込み時に親ディレクトリを `canonicalize()` してからチェック | ✅ 完了 |
| 整数オーバーフローのエラー化 | `checked_add` 等に置き換え、release ビルドでもサイレントラップを防止 | ✅ 完了 |
| メモリ DoS 対策（コレクションサイズ上限） | List/Dictの生成・拡張、List生成builtin、反復変換に上限ガード。`TSUMUGI_MAX_COLLECTION_SIZE` で変更可能 | ✅ 完了（総heap quotaは別課題） |
| ファジングテスト導入 | `cargo-fuzz` でレキサー/パーサー/評価器に無作為入力 | 未着手 |
| VM の `unwrap()` 除去 | コンパイラバグ時にパニックではなく構造化エラーを返す | ✅ 完了（AUD-023） |
| エラー種別の enum 化 | `classify_runtime_error()` の `contains()` 判定を `ErrorKind` enum に移行 | ✅ 完了 |

## 設計済み・着手禁止: HTTPアクセス機能

### 方針: 言語中核へ組み込まない

HTTP adapterの着手gate、capability、SSRF/DNS/TLS/redirect、budget、audit、error契約は[次期意味論・実装決定](semantic-decisions.md)第16節で設計済みである。ただしPhase 1〜6完了と具体的ユースケースの設計レビュー承認までは、依存追加・実装・DNS接続testを開始しない。

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

- **着手禁止（具体ユースケース承認待ち）**。Phase 1〜6の受入gateと[次期意味論・実装決定](semantic-decisions.md)第16.1節の全条件を満たすまで開始しない
- 承認後もcore builtinではなくhost adapterとして別計画を作る
- 具体的なRust HTTP clientと認証方式は承認ユースケースとhost責任に合わせて選び、coreへ固定しない

## 検討済み・現時点で見送り: エラーメッセージの多言語化（ロケール別出力）

### 経緯

エラーメッセージは現在すべて日本語で固定している（`src/error.rs` の operation 別 canonical constructor がテンプレートを持ち、`Display` が `N行目: ...` を組み立てて CLI が stderr へ出す）。利用者の環境に合わせて出力言語を切り替えたいという発想から、「実行ホストの timezone を見て JST なら日本語・それ以外は英語で出力し、将来は他ロケールへも拡張する」というアイデアを検討した。

### 結論: 現時点では見送り

技術的には実現可能だが、現在の設計正本と正面から衝突するため、現時点では実装しない。アイデア自体は破棄せず、下記の条件が整った段階で「表示レイヤ限定の i18n」として再検討する。

### 見送りの理由

1. **canonical error 契約（AUD-019 / [次期意味論・実装決定](semantic-decisions.md)第3章）と衝突する。** runtime error は operation ごとの共通 constructor から `kind` / canonical message / line を生成し、その message テンプレートを「完全一致の正本」として扱う。tree/VM 両 backend で stdout・stderr・kind・message・line・trace が完全一致することを `tests/canonical_error_inventory.rs` で検証している。出力言語が環境で変わると、この完全一致 golden test が環境依存になって壊れる。
2. **「規範出力を locale 差で変えない」という既存の設計判断に反する。** 第3.2節は「OS エラー文字列を canonical message へ埋め込む」案を、OS・locale 差が規範出力になることを理由に却下している。timezone による言語切り替えは、まさにこの却下理由に該当する。
3. **timezone から言語を推定するのは筋が悪い。** UTC 運用のサーバー上の日本語話者、JST 環境の英語話者など、timezone と希望言語は一致しない。マニフェスト原則2「時刻も含む外部効果は capability として明示的に付与する」の思想とも噛み合わず、環境からの暗黙推定は避けたい方向である。
4. **今は canonical message 自体がまだ動く時期である。** Phase 3/4 で `capability` / `budget` / `timeout` / `cancelled` / `host` などの kind 追加が控えている（第3.3節）。message の正本が固まる前に翻訳レイヤを作ると二重メンテになる。
5. **優先度が低い。** i18n は利用者体験の改善であり、マニフェストの中核価値（安定性・予算・capability・監査・予測可能性）のいずれの牽引役でもない。

### 再検討する場合の方向性（メモ）

将来やるなら、素朴な「timezone で自動判定」ではなく次の切り分けにする。

- **canonical message（機械可読・規範）は 1 言語に固定したまま変えない。** テスト・監査・host 側分岐の基盤なので、環境で揺らさない。
- **人間向けの表示レイヤ（CLI 出力）だけを翻訳可能にする。** `ErrorKind` + placeholder は既に構造化されているので、「kind + パラメータ → ローカライズ文字列」のカタログを表示直前に噛ませる。
- **言語選択は timezone ではなく明示設定にする。** 環境変数（`LANG` / `LC_MESSAGES` / `TSUMUGI_LANG`）や CLI オプションなど、ホストが明示的に与える形にする。
- **前提として、監査ログ・canonical message の正本を何語で持つか（日本語のままか英語へ寄せるか）を先に決めておく**と、表示レイヤの議論が単純になる。

### 着手タイミングの目安

canonical error の正本が固まり `language-spec.md` へ統合され、Phase 2 で CLI（`--help` 等を含む）が整備された後の、表示レイヤ改善としての nice-to-have。Phase 7（運用保証・検証）近辺での検討が自然で、それより前に優先しない。
