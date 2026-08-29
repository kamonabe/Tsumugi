use std::env as std_env;
use std::fs;
use std::io::{self, Write};

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

fn run() {
    // 非UTF-8のargvでもpanicさせず、診断して終了する（AUD-035）
    let args: Vec<String> = match std_env::args_os().map(|arg| arg.into_string()).collect() {
        Ok(args) => args,
        Err(invalid) => {
            eprintln!("エラー: 引数がUTF-8ではありません: {:?}", invalid);
            std::process::exit(1);
        }
    };

    // --vm フラグの検出
    let use_vm = args.iter().any(|a| a == "--vm");
    let file_args: Vec<&String> = args[1..].iter().filter(|a| *a != "--vm").collect();

    match file_args.len() {
        // 引数なし → REPL
        0 => {
            if use_vm {
                run_repl_vm();
            } else {
                run_repl();
            }
        }
        // 引数あり → ファイル実行
        1 => {
            if use_vm {
                run_file_vm(file_args[0]);
            } else {
                run_file(file_args[0]);
            }
        }
        _ => {
            eprintln!("使い方: tsumugi [--vm] [script.tsg]");
            std::process::exit(1);
        }
    }
}

/// ファイルを読み込んで実行
fn run_file(path: &str) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("エラー: ファイルを開けません: {} ({})", path, e);
            std::process::exit(1);
        }
    };

    let engine = Engine::new();
    let mut context = ExecutionContext::new();
    context.set_script_path(path);

    if let Err(errors) = execute(&engine, &source, &mut context) {
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
            // Ctrl+D (EOF)
            write_stdout("\n");
            break;
        }

        input.push_str(&line);

        // 入力が完結しているか判定（未閉じブロックがあれば継続入力）
        if is_incomplete(&input) {
            continue;
        }

        // 実行。ステップ予算はREPL入力ごとに独立させる。
        context.reset_step_budget();
        if let Err(errors) = execute(&engine, &input, &mut context) {
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

// =============================================
// VM モード
// =============================================

/// VMモードでファイルを実行
fn run_file_vm(path: &str) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("エラー: ファイルを開けません: {} ({})", path, e);
            std::process::exit(1);
        }
    };

    if let Err(errors) = execute_vm_with_path(&source, path) {
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
            write_stdout("\n");
            break;
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
                    Ok((linked, _)) => {
                        let linked_program = linked.as_ref().unwrap_or(&program);
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
fn execute_vm_with_path(source: &str, path: &str) -> Result<(), Vec<TsumugiError>> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();

    let mut parser = Parser::new(tokens);
    let program = parser.parse()?;

    // import は実行前に解決する（AUD-030）
    let mut loader = ModuleLoader::new();
    loader.set_base_dir(std::path::Path::new(path));
    let (linked, _) = loader.link(&program).map_err(|e| vec![e])?;
    let program = linked.as_ref().unwrap_or(&program);

    let compiler = Compiler::new();
    let chunk = compiler.compile(program).map_err(|e| vec![e])?;
    let mut vm = Vm::new(chunk);
    vm.run().map_err(|e| vec![e])
}
