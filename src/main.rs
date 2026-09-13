use std::env as std_env;
use std::fs;
use std::io::{self, Read, Write};

use tsumugi::{
    Engine, ExecutionContext, compiler::Compiler, error::TsumugiError, lexer::Lexer,
    module::ModuleLoader, parser::Parser, token::Token, vm::Vm,
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
/// この関数ではなく`builtin_core::write_stdout_line`を通る。
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

/// ツリーウォーク版で source を実行する（ファイル / stdin 共通）。
fn run_source(source: &str, script_path: &str, script_args: Vec<String>) {
    let engine = Engine::new();
    let mut context = ExecutionContext::new();
    context.set_script_path(script_path);
    context.set_script_args(script_args);

    if let Err(errors) = execute(&engine, source, &mut context) {
        for e in &errors {
            eprintln!("{}", e);
        }
        std::process::exit(1);
    }
}

/// REPL（対話実行モード）
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
    if let Err(errors) = execute_vm_with_path(source, script_path, script_args) {
        for e in &errors {
            eprintln!("{}", e);
        }
        std::process::exit(1);
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
                        if let Err(e) = vm.charge_link(input.len() as u64, &loaded) {
                            loader = loader_checkpoint;
                            eprintln!("  エラー: {}", e);
                            input.clear();
                            continue;
                        }
                        match compiler.compile_repl_line(linked_program) {
                            Ok(chunk) => {
                                if let Err(e) = vm.run_repl_chunk(chunk) {
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
) -> Result<(), Vec<TsumugiError>> {
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
    vm.charge_link(source.len() as u64, &loaded)
        .map_err(|e| vec![e])?;
    vm.run().map_err(|e| vec![e])
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
}
