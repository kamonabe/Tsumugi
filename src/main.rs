use std::env as std_env;
use std::fs;
use std::io::{self, Read, Write};

use tsumugi::{
    CapabilitySet, CompileErrors, EmbeddingContext, EmbeddingEngine, EmbeddingOutcome,
    EmbeddingRequest, EmbeddingTraceFrame, Engine, ExecutionContext, ExecutionError, LinkError,
    LinkRequest, Source as EmbeddingSource, SourceId, compiler::Compiler,
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

/// 実行 backend（ツリーウォーク / VM）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Tree,
    Vm,
}

/// script source の取得元（AUD-018）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// 引数なし → REPL
    Repl,
    /// `-` → 標準入力から source 全体を読む
    Stdin,
    /// 通常の positional → ファイルパス
    File(String),
}

/// CLI 起動の確定結果（AUD-018, semantic-decisions §6.4 の E8a subset）。
///
/// capability profile / options（`--profile` / `--allow-*` / `--fs-*`）は Phase 2 (E8b)
/// で追加するため、ここでは扱わない。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CliInvocation {
    backend: Backend,
    source: Source,
    script_args: Vec<String>,
}

/// argv（program 名を除く）を CLI grammar に従って解析する（AUD-018）。
///
/// grammar（E8a subset）:
///
/// ```text
/// tsumugi [--vm] [SCRIPT [ARGS...]]
/// SCRIPT が `-` なら stdin から source を読む。`--` は option 解析を終了する。
/// ```
///
/// option 解析中の最初の positional を SCRIPT とし、それ以後の token は既知 option・
/// 未知 option・`--` を含めて一切再解釈せず、そのまま script args とする。`--vm` は
/// SCRIPT より前でのみ backend option として解釈する（複数回指定は idempotent）。
fn parse_cli(argv: &[String]) -> CliInvocation {
    let mut backend = Backend::Tree;
    let mut iter = argv.iter();

    // option 解析フェーズ: SCRIPT が確定するまで既知 option を処理する。
    let source = loop {
        match iter.next() {
            None => break Source::Repl,
            Some(token) if token == "--vm" => {
                backend = Backend::Vm;
            }
            Some(token) if token == "--" => {
                // option 解析を終了。次の token があれば SCRIPT。
                match iter.next() {
                    None => break Source::Repl,
                    Some(script) if script == "-" => break Source::Stdin,
                    Some(script) => break Source::File(script.clone()),
                }
            }
            Some(token) if token == "-" => break Source::Stdin,
            Some(token) => break Source::File(token.clone()),
        }
    };

    // SCRIPT 確定後の残り token はすべて verbatim に script args とする。
    let script_args: Vec<String> = iter.cloned().collect();

    CliInvocation {
        backend,
        source,
        script_args,
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

    let invocation = parse_cli(&argv);

    match invocation.source {
        Source::Repl => match invocation.backend {
            Backend::Tree => run_repl(),
            Backend::Vm => run_repl_vm(),
        },
        Source::Stdin => {
            let source = read_stdin_source();
            match invocation.backend {
                Backend::Tree => run_source(&source, "<stdin>", invocation.script_args),
                Backend::Vm => run_source_vm(&source, "<stdin>", invocation.script_args),
            }
        }
        Source::File(ref path) => {
            let source = read_source_file(path);
            match invocation.backend {
                Backend::Tree => run_source(&source, path, invocation.script_args),
                Backend::Vm => run_source_vm(&source, path, invocation.script_args),
            }
        }
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

/// ツリーウォーク版で source を実行する（ファイル / stdin 共通、E8a）。
///
/// import なし root は embedding Engine API（[`EmbeddingEngine`]）だけを通す（EMB-AT-17 の
/// Phase 1 範囲）。Phase 1 の embedding `link` は import 解決を持たず import 文を拒否するため
/// （import resolver は Phase 2/E7）、import を含む root は現行の alpha facade（`ModuleLoader`
/// 経由）へフォールバックする。この import ありの Engine API 統合は E7 で行う。
///
/// どちらの経路も同じ診断表示・exit code 契約（[組み込みAPI仕様] 第12節）を満たす。
fn run_source(source: &str, script_path: &str, script_args: Vec<String>) {
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

    let linked = match engine.link(&script, LinkRequest::new()) {
        Ok(linked) => linked,
        // import を含む root は Phase 1 embedding では link できない（module resolver は
        // Phase 2/E7）。この場合だけ alpha facade（`ModuleLoader` 経由）へフォールバックして
        // 従来どおり解決・実行する。import ありの Engine API 統合は E7 で行う。
        Err(LinkError::FeatureUnavailable {
            feature: "module_resolver",
        }) => {
            run_source_alpha(source, script_path, script_args);
            return;
        }
        Err(error) => {
            // import なし root のその他の link 失敗は engine/revision/backend 不一致のみ。
            // 単一 Engine を使う CLI では実際には到達しないが、防御的に診断して終了する。
            print_link_error(&error);
            std::process::exit(1);
        }
    };

    let mut context = EmbeddingContext::new(&engine);
    context.set_script_path(script_path);
    // C7（REV-023）: CLI は従来どおり `exit()` を許可する。Phase 2 の CLI safe/legacy profile
    // （C9）が capability option を導入するまでは、暫定的に ProcessExit だけを明示 grant する
    // （filesystem・env・stdio は現行の process-global 経路が担う）。
    let request = EmbeddingRequest::new()
        .with_arguments(script_args)
        .with_capabilities(cli_transition_capabilities());

    let exit_code = exit_code_for_outcome(engine.run(&linked, &mut context, request));
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

/// Phase 2 移行期の CLI capability 集合（C9 の safe/legacy profile が入るまでの暫定）。
///
/// 現状は `exit()` を structured terminal（`Exited`）へ写すために ProcessExit だけを grant する。
/// その他 authority（filesystem・env・stdio）は C3〜C5/C10 で置換するまで現行の process-global
/// 経路（sandbox / env allow-list）が担うため、この set には載せない。
fn cli_transition_capabilities() -> CapabilitySet {
    CapabilitySet::ambient_compat()
}

/// import を含む root を alpha facade（`ModuleLoader` 経由）で実行するフォールバック（E8a）。
///
/// Phase 1 の embedding `link` は import 解決を持たないため、import ありの tree 実行は現行の
/// alpha facade を使う。import ありでも Engine API へ統合するのは E7（Phase 2、capability /
/// import resolver）の作業である。
fn run_source_alpha(source: &str, script_path: &str, script_args: Vec<String>) {
    let engine = Engine::new();
    let mut context = ExecutionContext::new();
    context.set_script_path(script_path);
    context.set_script_args(script_args);

    if let Err(errors) = execute(&engine, source, &mut context) {
        // C7（REV-023）: exit() は structured terminal。error 表示せず、context に記録された
        // 終了コードで process を終了する。
        if errors
            .iter()
            .any(|e| matches!(e.kind(), Some(tsumugi::error::ErrorKind::ProcessExit)))
        {
            let code = context.take_pending_exit().unwrap_or(0);
            std::process::exit(code as i32);
        }
        for e in &errors {
            eprintln!("{}", e);
        }
        std::process::exit(1);
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
        EmbeddingOutcome::Completed => 0,
        // C7（REV-023）: exit(code) は structured terminal。CLI 境界で実際の exit code へ写す。
        EmbeddingOutcome::Exited { code } => code as i32,
        EmbeddingOutcome::RuntimeError { error } => {
            eprintln!("{}", format_execution_error(&error));
            1
        }
        EmbeddingOutcome::Cancelled => 130,
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

/// link 失敗を stderr へ出す。import なし root では engine/revision/backend 不一致のみ。
fn print_link_error(error: &LinkError) {
    eprintln!("リンクエラー: {:?}", error);
}

/// REPL（対話実行モード）。
///
/// REPL は入力をまたいで language-state を保持し、import も解決する必要があるため、Phase 1
/// では現行の alpha facade（`ModuleLoader` を持つ評価器）を使う。embedding Engine API は
/// import なし root の一括実行のみを提供する（状態継続・import 解決は Phase 2/E7）。REPL の
/// Engine API 統合は E7 で行う（E8a の Phase 1 範囲外）。
fn run_repl() {
    write_stdout("Tsumugi v0.1.0 — 終了するには Ctrl+D\n");
    let engine = Engine::new();
    let mut context = ExecutionContext::new();
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

/// ソースを実行する CLI 用のアダプター。
///
/// パースエラーは複数件、実行時エラーは1件という既存の表示契約を維持する。
fn execute(
    engine: &Engine,
    source: &str,
    context: &mut ExecutionContext,
) -> Result<(), Vec<TsumugiError>> {
    let script = engine.compile(source)?;
    engine
        .execute(&script, context)
        .map(|_| ())
        .map_err(|error| vec![error])
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
fn run_source_vm(source: &str, script_path: &str, script_args: Vec<String>) {
    match execute_vm_with_path(source, script_path, script_args) {
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
fn run_repl_vm() {
    write_stdout("Tsumugi v0.1.0 [VM mode] — 終了するには Ctrl+D\n");
    let mut input = String::new();
    let mut compiler = Compiler::new();
    let mut vm = Vm::new_repl();
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

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_starts_tree_repl() {
        let inv = parse_cli(&[]);
        assert_eq!(inv.backend, Backend::Tree);
        assert_eq!(inv.source, Source::Repl);
        assert!(inv.script_args.is_empty());
    }

    #[test]
    fn vm_flag_before_script_selects_vm_backend() {
        // tsumugi --vm app.tsg a --vm  → VMで実行、args() == ["a", "--vm"]
        let inv = parse_cli(&strs(&["--vm", "app.tsg", "a", "--vm"]));
        assert_eq!(inv.backend, Backend::Vm);
        assert_eq!(inv.source, Source::File("app.tsg".to_string()));
        assert_eq!(inv.script_args, strs(&["a", "--vm"]));
    }

    #[test]
    fn double_dash_ends_option_parsing() {
        // tsumugi -- app.tsg --help  → treeで実行、args() == ["--help"]
        let inv = parse_cli(&strs(&["--", "app.tsg", "--help"]));
        assert_eq!(inv.backend, Backend::Tree);
        assert_eq!(inv.source, Source::File("app.tsg".to_string()));
        assert_eq!(inv.script_args, strs(&["--help"]));
    }

    #[test]
    fn dash_reads_stdin_source() {
        // tsumugi - a b  → stdin script、args() == ["a", "b"]
        let inv = parse_cli(&strs(&["-", "a", "b"]));
        assert_eq!(inv.backend, Backend::Tree);
        assert_eq!(inv.source, Source::Stdin);
        assert_eq!(inv.script_args, strs(&["a", "b"]));
    }

    #[test]
    fn double_dash_only_starts_repl() {
        // tsumugi --  → SCRIPTなしなのでREPL
        let inv = parse_cli(&strs(&["--"]));
        assert_eq!(inv.source, Source::Repl);
        assert!(inv.script_args.is_empty());
    }

    #[test]
    fn script_after_double_dash_may_be_dash_stdin() {
        // `--` の後の `-` は stdin script として扱う
        let inv = parse_cli(&strs(&["--", "-", "x"]));
        assert_eq!(inv.source, Source::Stdin);
        assert_eq!(inv.script_args, strs(&["x"]));
    }

    #[test]
    fn tokens_after_script_are_verbatim() {
        // script 確定後の --vm は backend option ではなく script arg
        let inv = parse_cli(&strs(&["app.tsg", "--vm", "--", "-"]));
        assert_eq!(inv.backend, Backend::Tree);
        assert_eq!(inv.source, Source::File("app.tsg".to_string()));
        assert_eq!(inv.script_args, strs(&["--vm", "--", "-"]));
    }

    #[test]
    fn vm_flag_is_idempotent() {
        let inv = parse_cli(&strs(&["--vm", "--vm", "app.tsg"]));
        assert_eq!(inv.backend, Backend::Vm);
        assert_eq!(inv.source, Source::File("app.tsg".to_string()));
        assert!(inv.script_args.is_empty());
    }

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
        assert_eq!(exit_code_for_outcome(EmbeddingOutcome::Completed), 0);
    }

    #[test]
    fn runtime_error_outcome_maps_to_exit_one() {
        let outcome = EmbeddingOutcome::RuntimeError {
            error: exec_error(Some(1), "ゼロ除算", Vec::new()),
        };
        assert_eq!(exit_code_for_outcome(outcome), 1);
    }

    #[test]
    fn cancelled_outcome_maps_to_exit_130() {
        // 第12節: Cancelled は 130（現行 CLI では pre-cancel を発火しないが写像は固定する）。
        assert_eq!(exit_code_for_outcome(EmbeddingOutcome::Cancelled), 130);
    }

    #[test]
    fn internal_failure_outcome_maps_to_exit_70() {
        let outcome = EmbeddingOutcome::InternalFailure {
            fault_id: 1,
            safe_message: "内部障害が発生しました (fault_id=1)".to_string(),
        };
        assert_eq!(exit_code_for_outcome(outcome), 70);
    }
}
