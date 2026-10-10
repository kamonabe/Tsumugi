mod cli_capability;

use std::env as std_env;
use std::fs;
use std::io::{self, Read, Write};
use std::sync::Arc;

use cli_capability::{
    Backend, CliInvocation, CliOutcome, Profile, Source, USAGE, build_legacy_capabilities,
    build_safe_capabilities, parse_cli,
};
use tsumugi::{
    BudgetConfig, CancellationToken, CapabilitySet, CompileErrors, EmbeddingContext,
    EmbeddingEngine, EmbeddingOutcome, EmbeddingRequest, EmbeddingTraceFrame, Engine,
    ExecutionContext, ExecutionError, ExecutionId, LinkError, LinkRequest,
    Source as EmbeddingSource, SourceId, SystemMonotonicClock, compiler::Compiler,
    embedding::CompileOptions, error::TsumugiError, lexer::Lexer, module::ModuleLoader,
    parser::Parser, token::Token, vm::Vm,
};

fn main() {
    // ツリーウォーク版の再帰がスタックを多く消費するため、
    // メインスレッド(Windows: 1MB)では不足する場合がある。
    // 十分なスタックサイズのスレッドで実行する。
    let builder = std::thread::Builder::new()
        .name("tsumugi-main".to_string())
        .stack_size(8 * 1024 * 1024); // 8MB
    let handler = match builder.spawn(run) {
        Ok(handler) => handler,
        Err(error) => {
            eprintln!("エラー: 実行スレッドを作成できません: {}", error);
            std::process::exit(1);
        }
    };
    if handler.join().is_err() {
        // パニック時（スタックオーバーフロー等）はそのまま異常終了
        std::process::exit(1);
    }
}

/// 標準出力へ書き出す。書き込めない場合は診断を出して終了する（AUD-035）
///
/// CLI自身のbannerとpromptに使う。スクリプトの`print`は構造化エラーを返すため、
/// この関数ではなくC4の`builtin_core::resolve_print`経由のStdout adapterを通る。
fn write_stdout(text: &str) {
    let mut out = io::stdout().lock();
    if let Err(error) = out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        eprintln!("エラー: 標準出力へ書き込めません: {}", error);
        std::process::exit(1);
    }
}

/// 標準入力から1行読む。読み取れない場合は診断を出して終了する（AUD-035）
///
/// 戻り値は読み取ったバイト数。0はEOF（Ctrl+D）を表す。
fn read_stdin_line(line: &mut String) -> usize {
    match io::stdin().read_line(line) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("エラー: 標準入力から読み取れません: {}", error);
            std::process::exit(1);
        }
    }
}

fn run() {
    // 非UTF-8のargvでもpanicさせず、診断して終了する（AUD-018 / AUD-035）
    let argv: Vec<String> = match std_env::args_os()
        .skip(1)
        .map(|arg| arg.into_string())
        .collect()
    {
        Ok(argv) => argv,
        Err(_) => {
            eprintln!("エラー: コマンドライン引数はUTF-8で指定してください");
            std::process::exit(1);
        }
    };

    let invocation = match parse_cli(&argv) {
        Ok(CliOutcome::Run(inv)) => inv,
        Ok(CliOutcome::Help) => {
            write_stdout(USAGE);
            write_stdout("\n");
            std::process::exit(0);
        }
        Ok(CliOutcome::Version) => {
            write_stdout(&format!("tsumugi {}\n", env!("CARGO_PKG_VERSION")));
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("{}", error);
            eprintln!("{}", USAGE);
            std::process::exit(1);
        }
    };

    // profile / options から frozen capability set を構築する（§3/§4）。構築 error（legacy の
    // secure handle 不能・root 選択曖昧など）は usage error 相当で exit 1。
    let frozen = build_capabilities(&invocation);

    match invocation.source {
        Source::Repl => match invocation.backend {
            Backend::Tree => run_repl(frozen),
            Backend::Vm => run_repl_vm(frozen),
        },
        Source::Stdin => {
            let source = read_stdin_source();
            match invocation.backend {
                Backend::Tree => run_source(&source, "<stdin>", invocation.script_args, frozen),
                Backend::Vm => run_source_vm(&source, "<stdin>", invocation.script_args, frozen),
            }
        }
        Source::File(ref path) => {
            let source = read_source_file(path);
            match invocation.backend {
                Backend::Tree => run_source(&source, path, invocation.script_args, frozen),
                Backend::Vm => run_source_vm(&source, path, invocation.script_args, frozen),
            }
        }
    }
}

/// profile と options から frozen `CapabilitySet` を構築する。legacy は warning を stderr へ出す。
///
/// 構築に失敗した場合（legacy の secure handle 不能など）は診断を stderr へ出して exit 1。
fn build_capabilities(invocation: &CliInvocation) -> CapabilitySet {
    match invocation.profile {
        Profile::Safe => match build_safe_capabilities(&invocation.capability_options) {
            Ok(set) => set,
            Err(error) => {
                eprintln!("{}", error);
                std::process::exit(1);
            }
        },
        Profile::Legacy => match build_legacy_capabilities() {
            Ok(build) => {
                for warning in &build.warnings {
                    eprintln!("{}", warning);
                }
                build.capabilities
            }
            Err(error) => {
                eprintln!("{}", error);
                std::process::exit(1);
            }
        },
    }
}

/// ファイルから source を読む。開けない場合は診断を出して終了する。
fn read_source_file(path: &str) -> String {
    match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("エラー: ファイルを開けません: {} ({})", path, e);
            std::process::exit(1);
        }
    }
}

/// 標準入力から source 全体を読む（`-` SCRIPT）。読めない場合は診断を出して終了する。
fn read_stdin_source() -> String {
    let mut source = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut source) {
        eprintln!("エラー: 標準入力から読み取れません: {}", e);
        std::process::exit(1);
    }
    source
}

/// ツリーウォーク版で source を実行する（ファイル / stdin 共通、C6-c = E7-import）。
///
/// import の有無によらず embedding Engine API（[`EmbeddingEngine`]）を通す。import があれば
/// `link` が frozen capability の `module_resolver`（CLI が `--allow-import-root` から grant）で
/// 解決する。resolver 未 grant + import あり は terminal `Denied`（exit 1、actionable 診断）。
/// REPL は依然 alpha facade（`run_source_alpha`）を使う（Engine 統合は E7 残）。
fn run_source(source: &str, script_path: &str, script_args: Vec<String>, frozen: CapabilitySet) {
    let engine = EmbeddingEngine::builder()
        .build()
        .expect("既定 backend の Engine build は失敗しない");

    // source を保持して compile する。embedding の `run` は実行スレッド上で保持 source を
    // 再 parse して Program を得る（案 A）ため、`retain_source=true` が実行の前提になる。
    let source_id = source_id_for(script_path);
    let options = CompileOptions {
        retain_source: true,
    };
    let script = match engine.compile(EmbeddingSource::new(source_id, source), &options) {
        Ok(script) => script,
        Err(errors) => {
            print_compile_errors(&errors);
            std::process::exit(1);
        }
    };

    // REV-015 最終形移行 Slice 1（option B）: CLI 経路の既定を有限 budget にする。
    // 単一の SystemMonotonicClock を 1 個だけ作り、BudgetConfig::standard の deadline 生成と
    // link/run request への clock 注入で同一 instance を共有する（clock_id を一致させ
    // ForeignClock を避ける）。
    let clock: Arc<dyn tsumugi::MonotonicClock> = Arc::new(SystemMonotonicClock::new());
    let budget = BudgetConfig::standard(clock.as_ref())
        .expect("standard budget の生成は overflow しない限り成功する");
    // 実行 1 回ぶんの cancellation token を 1 個生成し、link と run の両 request へ同じ clone を
    // 渡す（設計 §4.6。link 開始前 cancel が実 token で機能する）。CLI は別スレッド cancel を
    // 使わないが、link/run で同一計数系を共有する契約に揃える。
    let cancellation = CancellationToken::new();
    // CLI 固定の operation_id（resolver 相関用、監査 sink 無しなので値は固定で足りる）。
    let operation_id = ExecutionId::new(std::num::NonZeroU128::new(1).expect("1 は非ゼロ"));

    // import の有無によらず link を通す。import あり + resolver 未 grant は terminal Denied。
    let link_request = LinkRequest::new(operation_id, frozen.clone(), budget)
        .with_cancellation(cancellation.clone());
    let linked = match engine.link(&script, link_request) {
        Ok(linked) => linked,
        Err(error) => {
            print_link_error(&error);
            std::process::exit(1);
        }
    };

    let mut context = EmbeddingContext::new(&engine);
    context.set_script_path(script_path);
    // C9/C10: CLI が profile（safe/legacy）から組んだ frozen capability set を注入する。
    let request = EmbeddingRequest::new(budget, Arc::clone(&clock))
        .expect("standard budget は自身を生成した clock と同 domain なので検証を通る")
        .with_arguments(script_args)
        .with_capabilities(frozen)
        .cancellation(cancellation);

    let exit_code = exit_code_for_outcome(engine.run(&linked, &mut context, request));
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

/// CLI の script 識別子（[`SourceId`]）を作る。表示専用で secret を含めない。
///
/// script path は 1..=256 byte・NUL なしの制約に収まらない場合があるため、収まらないときは
/// 固定の表示名へフォールバックする（識別子は診断の見出しにのみ使う）。
fn source_id_for(script_path: &str) -> SourceId {
    SourceId::new(script_path)
        .unwrap_or_else(|_| SourceId::new("<script>").expect("固定 fallback 識別子は常に妥当"))
}

/// embedding の terminal outcome を CLI の exit code へ写す（[組み込みAPI仕様] 第12節）。
///
/// `Completed` は 0、`RuntimeError` は 1、`Cancelled` は 130、`InternalFailure` は 70。
/// エラー系はいずれも診断を stderr へ出してから code を返す。
fn exit_code_for_outcome(outcome: EmbeddingOutcome) -> i32 {
    match outcome {
        EmbeddingOutcome::Completed { .. } => 0,
        // C7（REV-023）: exit(code) は structured terminal。CLI 境界で実際の exit code へ写す。
        EmbeddingOutcome::Exited { code, .. } => code as i32,
        EmbeddingOutcome::RuntimeError { error, .. } => {
            eprintln!("{}", format_execution_error(&error));
            1
        }
        // 予算超過（REV-015 E11）: 診断を出して exit 1（第12節の terminal channel 表）。
        // 既存 CLI の step 上限メッセージと byte 一致させるため RuntimeError と同じ formatter。
        EmbeddingOutcome::BudgetExceeded { error, .. } => {
            eprintln!("{}", format_execution_error(&error));
            1
        }
        // deadline 超過（REV-015 E11）: exit 1（第12節）。CLI は deadline clock を注入しないため
        // 通常この経路には到達しないが、写像は固定しておく。
        EmbeddingOutcome::DeadlineExceeded { .. } => {
            eprintln!("実行 deadline を超過しました");
            1
        }
        EmbeddingOutcome::Cancelled { .. } => 130,
        EmbeddingOutcome::InternalFailure { safe_message, .. } => {
            eprintln!("{}", safe_message);
            70
        }
        // `EmbeddingOutcome` は `#[non_exhaustive]`。Phase 1 で到達し得るのは上記4種だが、
        // 将来 variant が増えても未処理を internal failure と同じ扱い（code 70）で拾う。
        other => {
            eprintln!("内部エラー: 未対応の実行結果です: {:?}", other);
            70
        }
    }
}

/// [`ExecutionError`] を現行 CLI の診断形式へ整形する。
///
/// 現行の `TsumugiError` Display（`"{line}行目: {message}"` と、trace 各行の
/// `"\n  in {name}() ({line}行目)"`）と byte 単位で一致させ、ゴールデンエラー fixture を
/// 維持する。
fn format_execution_error(error: &ExecutionError) -> String {
    let mut out = format_line_message(error.line, &error.safe_message);
    for frame in &error.trace {
        out.push_str(&format_trace_frame(frame));
    }
    out
}

/// trace の1フレームを `"\n  in {name}() ({line}行目)"` 形式へ整形する。
fn format_trace_frame(frame: &EmbeddingTraceFrame) -> String {
    match frame.line {
        Some(line) => format!("\n  in {}() ({}行目)", frame.function, line),
        None => format!("\n  in {}()", frame.function),
    }
}

/// `"{line}行目: {message}"` を作る。行番号が不明なら message だけを返す。
fn format_line_message(line: Option<u32>, message: &str) -> String {
    match line {
        Some(line) => format!("{}行目: {}", line, message),
        None => message.to_string(),
    }
}

/// compile 診断を stderr へ出す。source 位置順に1件以上あり、各行を現行形式で表示する。
fn print_compile_errors(errors: &CompileErrors) {
    for diagnostic in &errors.diagnostics {
        eprintln!(
            "{}",
            format_line_message(diagnostic.line, &diagnostic.safe_message)
        );
    }
}

/// link 失敗を stderr へ出す（exit 1）。
///
/// import あり + resolver 未 grant（`Denied`）は原因と対処を明示する actionable 診断にする
/// （Q2、設計 §8.5 の存在 oracle 回避のため絶対 path・specifier 原文は漏らさない）。
fn print_link_error(error: &LinkError) {
    match error {
        LinkError::Denied(_) => {
            eprintln!(
                "エラー: import がありますが module resolver が許可されていません。\
                 `--allow-import-root NAME=PATH` で import root を指定してください。"
            );
        }
        LinkError::Resolve(host) => {
            // code は固定の安全文字列。specifier 原文・絶対 path は含めない（§8.5）。
            eprintln!("import 解決エラー: {}", host.code.as_str());
        }
        LinkError::InvalidModule { diagnostics, .. } => {
            eprintln!("import モジュールが不正です:");
            print_compile_errors(diagnostics);
        }
        LinkError::Cycle { chain } => {
            let path: Vec<&str> = chain.iter().map(|m| m.as_str()).collect();
            eprintln!("import の循環を検出しました: {}", path.join(" -> "));
        }
        LinkError::DepthExceeded { limit } => {
            eprintln!("import のネストが深すぎます (上限: {})", limit);
        }
        LinkError::Cancelled => {
            eprintln!("link がキャンセルされました");
        }
        LinkError::DeadlineExceeded => {
            eprintln!("link の deadline を超過しました");
        }
        other => {
            // engine/revision/backend 不一致・budget・backend・internal は防御的に表示する。
            eprintln!("リンクエラー: {:?}", other);
        }
    }
}

/// REPL（対話実行モード）。
///
/// REPL は入力をまたいで language-state を保持し、import も解決する必要があるため、Phase 1
/// では現行の alpha facade（`ModuleLoader` を持つ評価器）を使う。embedding Engine API は
/// import なし root の一括実行のみを提供する（状態継続・import 解決は Phase 2/E7）。REPL の
/// Engine API 統合は E7 で行う（E8a の Phase 1 範囲外）。
fn run_repl(frozen: CapabilitySet) {
    write_stdout(&format!(
        "Tsumugi v{} — 終了するには Ctrl+D\n",
        env!("CARGO_PKG_VERSION")
    ));
    let engine = Engine::new();
    let mut context = ExecutionContext::new();
    // C9/C10: session 開始時に frozen set を 1 回注入する。各 submission は同じ frozen set を
    // 共有し（§1(f)）、REPL は clear_capabilities を呼ばないので持ち越される。
    context.set_capabilities(frozen);
    let mut input = String::new();

    loop {
        // プロンプト表示
        write_stdout(if input.is_empty() {
            "tsumugi> "
        } else {
            "      .. "
        });

        // 1行読み取り
        let mut line = String::new();
        let bytes = read_stdin_line(&mut line);
        if bytes == 0 {
            // Ctrl+D (EOF)。継続入力 buffer が残っていれば診断して終了する（AUD-033）。
            finish_repl_at_eof(&input);
        }

        input.push_str(&line);

        // 入力が完結しているか判定（未閉じブロックがあれば継続入力）
        if is_incomplete(&input) {
            continue;
        }

        // 実行。ステップ予算はREPL入力ごとに独立させる。
        // 未捕捉エラーは入力が変更した language-state を巻き戻す（AUD-024）。
        context.reset_step_budget();
        if let Err(errors) = execute_repl(&engine, &input, &mut context) {
            // C7（REV-023）: REPL 入力の exit() は REPL を終了コード付きで終える。
            if errors
                .iter()
                .any(|e| matches!(e.kind(), Some(tsumugi::error::ErrorKind::ProcessExit)))
            {
                let code = context.take_pending_exit().unwrap_or(0);
                std::process::exit(code as i32);
            }
            for e in &errors {
                eprintln!("  エラー: {}", e);
            }
        }

        input.clear();
    }
}

/// REPL の1入力を実行する CLI 用アダプター（AUD-024）。
///
/// 未捕捉ランタイムエラーで終了した入力は、その入力が加えた language-state の変更を
/// 入力開始時点へ巻き戻す。外部効果（stdout・ファイル書き込み等）は巻き戻さない。
fn execute_repl(
    engine: &Engine,
    source: &str,
    context: &mut ExecutionContext,
) -> Result<(), Vec<TsumugiError>> {
    let script = engine.compile(source)?;
    engine
        .execute_repl_submission(&script, context)
        .map(|_| ())
        .map_err(|error| vec![error])
}

/// 入力が未完結か判定（if/fn/while/for が end で閉じられていない）
/// レキサーを通してトークン列で判定するため、文字列リテラル内の "if" や
/// コメント中の "end" に影響されない。
fn is_incomplete(input: &str) -> bool {
    let mut lexer = Lexer::new(input);
    let tokens = lexer.tokenize();
    let mut depth: i32 = 0;
    for spanned in &tokens {
        match &spanned.token {
            Token::If | Token::Fn | Token::While | Token::For | Token::Try => depth += 1,
            Token::End => depth -= 1,
            _ => {}
        }
    }
    depth > 0
}

/// REPL で EOF（Ctrl+D）を受けたときの終了処理（AUD-033, semantic-decisions §8）。
///
/// tree / VM の両 REPL loop で共通に使う。継続入力 buffer が空でなければ、
/// buffer を破棄して正常終了せず、実 Lexer/Parser へ渡した parse 診断を stderr へ出して
/// 終了コード 1 で終了する。buffer が空の EOF だけを正常終了（0）とする。
///
/// `is_incomplete` の判定だけで message を合成せず、実 Parser の結果を表示する。
fn finish_repl_at_eof(buffer: &str) -> ! {
    // プロンプト行から改行して診断・シェルへ戻す。
    write_stdout("\n");

    if buffer.trim().is_empty() {
        // 空 buffer（未完結の実体がない）は正常終了。
        std::process::exit(0);
    }

    let tokens = Lexer::new(buffer).tokenize();
    match Parser::new(tokens).parse() {
        Ok(_) => {
            // is_incomplete が継続と判定したが Parser は完結として受理した場合。
            // buffer を黙って捨てないよう、診断を出して失敗扱いにする。
            eprintln!("  エラー: 入力が未完結です");
            std::process::exit(1);
        }
        Err(errors) => {
            for e in &errors {
                eprintln!("  エラー: {}", e);
            }
            std::process::exit(1);
        }
    }
}

// =============================================
// VM モード
// =============================================

/// VMモードで source を実行する（ファイル / stdin 共通）。
fn run_source_vm(source: &str, script_path: &str, script_args: Vec<String>, frozen: CapabilitySet) {
    match execute_vm_with_path(source, script_path, script_args, frozen) {
        Ok(0) => {}
        // C7（REV-023）: VM の exit() は structured terminal。CLI 境界で実際の exit code へ写す。
        Ok(code) => std::process::exit(code),
        Err(errors) => {
            for e in &errors {
                eprintln!("{}", e);
            }
            std::process::exit(1);
        }
    }
}

/// VMモードのREPL
fn run_repl_vm(frozen: CapabilitySet) {
    write_stdout(&format!(
        "Tsumugi v{} [VM mode] — 終了するには Ctrl+D\n",
        env!("CARGO_PKG_VERSION")
    ));
    let mut input = String::new();
    let mut compiler = Compiler::new();
    let mut vm = Vm::new_repl();
    // C9/C10: session 開始時に frozen set を 1 回注入する（§1(f)）。
    vm.set_capabilities(frozen);
    let mut loader = ModuleLoader::new();

    loop {
        write_stdout(if input.is_empty() {
            "tsumugi:vm> "
        } else {
            "         .. "
        });

        let mut line = String::new();
        let bytes = read_stdin_line(&mut line);
        if bytes == 0 {
            // Ctrl+D (EOF)。継続入力 buffer が残っていれば診断して終了する（AUD-033）。
            finish_repl_at_eof(&input);
        }

        input.push_str(&line);

        if is_incomplete(&input) {
            continue;
        }

        // パース
        let mut lexer = Lexer::new(&input);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        match parser.parse() {
            Ok(program) => {
                // Compiler・VM・ModuleLoaderを1つのREPL transactionとして扱う。
                // compile成功後でもruntime errorなら、未実行のbinding/import情報を残さない。
                let compiler_checkpoint = compiler.clone();
                let loader_checkpoint = loader.clone();
                // import は実行前に解決する（AUD-030）
                match loader.link(&program) {
                    Ok((linked, loaded)) => {
                        let linked_program = linked.as_ref().unwrap_or(&program);
                        // source/import 予算を Link フェーズで課金する（REV-015 Slice 2）。
                        // 超過なら compile も実行もせず、loader を巻き戻す。
                        match vm.charge_link(input.len() as u64, &loaded) {
                            Ok(record_tokens) => {
                                // imported module record token を loader へ登録し、
                                // `loaded` set と寿命を揃える（REV-015 PR-d）。runtime error
                                // 時の `loader = loader_checkpoint` で checkpoint 以降の
                                // token が drop され live heap が release される。
                                for (path, token) in record_tokens {
                                    loader.register_record_token(path, token);
                                }
                            }
                            Err(e) => {
                                loader = loader_checkpoint;
                                eprintln!("  エラー: {}", e);
                                input.clear();
                                continue;
                            }
                        }
                        match compiler.compile_repl_line(linked_program) {
                            Ok(chunk) => {
                                if let Err(e) = vm.run_repl_chunk(chunk) {
                                    // C7（REV-023）: exit() は REPL を終了コード付きで終える。
                                    if matches!(
                                        e.kind(),
                                        Some(tsumugi::error::ErrorKind::ProcessExit)
                                    ) {
                                        let code = vm.take_pending_exit().unwrap_or(0);
                                        std::process::exit(code as i32);
                                    }
                                    compiler = compiler_checkpoint;
                                    loader = loader_checkpoint;
                                    eprintln!("  エラー: {}", e);
                                }
                            }
                            // compile_repl_line自身もrollbackするが、ここでも入力開始時の
                            // checkpointを保持することでtransaction境界を明示する。
                            Err(e) => {
                                compiler = compiler_checkpoint;
                                loader = loader_checkpoint;
                                eprintln!("  エラー: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        loader = loader_checkpoint;
                        eprintln!("  エラー: {}", e);
                    }
                }
            }
            Err(errors) => {
                for e in &errors {
                    eprintln!("  エラー: {}", e);
                }
            }
        }

        input.clear();
    }
}

/// VMモードの実行関数（ファイルパス付き）
fn execute_vm_with_path(
    source: &str,
    path: &str,
    script_args: Vec<String>,
    frozen: CapabilitySet,
) -> Result<i32, Vec<TsumugiError>> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();

    let mut parser = Parser::new(tokens);
    let program = parser.parse()?;

    // import は実行前に解決する（AUD-030）
    let mut loader = ModuleLoader::new();
    loader.set_base_dir(std::path::Path::new(path));
    let (linked, loaded) = loader.link(&program).map_err(|e| vec![e])?;
    let linked_program = linked.as_ref().unwrap_or(&program);

    let compiler = Compiler::new();
    let chunk = compiler.compile(linked_program).map_err(|e| vec![e])?;
    let mut vm = Vm::new(chunk);
    // C9/C10: VM 経路へ frozen set を注入する（Vm::set_capabilities）。
    vm.set_capabilities(frozen);
    vm.set_script_args(script_args);
    // source/import 予算を Link フェーズで課金する（REV-015 Slice 2）。
    // imported module record token（REV-015 PR-d）は loader へ登録し、実行のあいだ
    // 生かしておく（loader は関数終了で drop され live heap も release される）。
    let record_tokens = vm
        .charge_link(source.len() as u64, &loaded)
        .map_err(|e| vec![e])?;
    for (path, token) in record_tokens {
        loader.register_record_token(path, token);
    }
    match vm.run() {
        Ok(()) => Ok(0),
        Err(error) => {
            // C7（REV-023）: exit() は structured terminal。error として表示せず exit code へ写す。
            if matches!(error.kind(), Some(tsumugi::error::ErrorKind::ProcessExit)) {
                Ok(vm.take_pending_exit().unwrap_or(0) as i32)
            } else {
                Err(vec![error])
            }
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    // parse_cli / CLI grammar の単体テストは cli_capability モジュール（§9）が持つ。
    // ここでは E8a の embedding outcome → CLI 診断・exit code 写像だけを固定する。

    // --- E8a: embedding outcome → CLI 診断・exit code の写像 ---

    use tsumugi::error::ErrorKind;

    fn exec_error(
        line: Option<u32>,
        message: &str,
        trace: Vec<EmbeddingTraceFrame>,
    ) -> ExecutionError {
        ExecutionError {
            code: ErrorKind::Runtime,
            safe_message: message.to_string(),
            line,
            trace,
        }
    }

    #[test]
    fn runtime_error_without_trace_matches_display_format() {
        // 現行 TsumugiError Display の `"{line}行目: {message}"` と一致する。
        let error = exec_error(Some(1), "ゼロ除算", Vec::new());
        assert_eq!(format_execution_error(&error), "1行目: ゼロ除算");
    }

    #[test]
    fn runtime_error_with_trace_matches_display_format() {
        // trace 各行は `"\n  in {name}() ({line}行目)"`。error_stack_trace fixture と同形式。
        let error = exec_error(
            Some(2),
            "ゼロ除算",
            vec![
                EmbeddingTraceFrame {
                    function: "divide".to_string(),
                    line: Some(6),
                },
                EmbeddingTraceFrame {
                    function: "calc".to_string(),
                    line: Some(9),
                },
            ],
        );
        assert_eq!(
            format_execution_error(&error),
            "2行目: ゼロ除算\n  in divide() (6行目)\n  in calc() (9行目)"
        );
    }

    #[test]
    fn line_message_falls_back_to_message_when_line_unknown() {
        assert_eq!(format_line_message(None, "詳細不明"), "詳細不明");
        assert_eq!(format_line_message(Some(3), "x"), "3行目: x");
    }

    #[test]
    fn completed_outcome_maps_to_exit_zero() {
        assert_eq!(
            exit_code_for_outcome(EmbeddingOutcome::Completed {
                usage: Default::default(),
            }),
            0
        );
    }

    #[test]
    fn runtime_error_outcome_maps_to_exit_one() {
        let outcome = EmbeddingOutcome::RuntimeError {
            error: exec_error(Some(1), "ゼロ除算", Vec::new()),
            usage: Default::default(),
        };
        assert_eq!(exit_code_for_outcome(outcome), 1);
    }

    #[test]
    fn cancelled_outcome_maps_to_exit_130() {
        // 第12節: Cancelled は 130（現行 CLI では pre-cancel を発火しないが写像は固定する）。
        assert_eq!(
            exit_code_for_outcome(EmbeddingOutcome::Cancelled {
                usage: Default::default(),
            }),
            130
        );
    }

    #[test]
    fn internal_failure_outcome_maps_to_exit_70() {
        let outcome = EmbeddingOutcome::InternalFailure {
            fault_id: 1,
            safe_message: "内部障害が発生しました (fault_id=1)".to_string(),
            usage: Default::default(),
        };
        assert_eq!(exit_code_for_outcome(outcome), 70);
    }
}
