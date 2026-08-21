mod ast;
mod builtin_core;
mod chunk;
mod compiler;
mod env;
mod error;
mod eval;
mod lexer;
mod opcode;
mod parser;
mod sandbox;
mod token;
mod value;
mod vm;

use std::env as std_env;
use std::fs;
use std::io::{self, Write};

use compiler::Compiler;
use eval::Evaluator;
use lexer::Lexer;
use parser::Parser;
use token::Token;
use vm::Vm;

fn main() {
    // ツリーウォーク版の再帰がスタックを多く消費するため、
    // メインスレッド(Windows: 1MB)では不足する場合がある。
    // 十分なスタックサイズのスレッドで実行する。
    let builder = std::thread::Builder::new()
        .name("tsumugi-main".to_string())
        .stack_size(8 * 1024 * 1024); // 8MB
    let handler = builder.spawn(run).unwrap();
    if handler.join().is_err() {
        // パニック時（スタックオーバーフロー等）はそのまま異常終了
        std::process::exit(1);
    }
}

fn run() {
    let args: Vec<String> = std_env::args().collect();

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

    let mut evaluator = Evaluator::new();
    evaluator.set_base_dir(std::path::Path::new(path));

    if let Err(e) = execute(&source, &mut evaluator) {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

/// REPL（対話実行モード）
fn run_repl() {
    println!("Tsumugi v0.1.0 — 終了するには Ctrl+D");
    let mut evaluator = Evaluator::new();
    let mut input = String::new();

    loop {
        // プロンプト表示
        if input.is_empty() {
            print!("tsumugi> ");
        } else {
            print!("      .. ");
        }
        io::stdout().flush().unwrap();

        // 1行読み取り
        let mut line = String::new();
        let bytes = io::stdin().read_line(&mut line).unwrap();
        if bytes == 0 {
            // Ctrl+D (EOF)
            println!();
            break;
        }

        input.push_str(&line);

        // 入力が完結しているか判定（未閉じブロックがあれば継続入力）
        if is_incomplete(&input) {
            continue;
        }

        // 実行
        if let Err(e) = execute(&input, &mut evaluator) {
            eprintln!("  エラー: {}", e);
        }

        input.clear();
    }
}

/// ソースを実行する共通関数
fn execute(source: &str, evaluator: &mut Evaluator) -> Result<(), error::TsumugiError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();

    let mut parser = Parser::new(tokens);
    let program = parser.parse()?;

    evaluator.run(&program)?;
    Ok(())
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

    if let Err(e) = execute_vm_with_path(&source, path) {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

/// VMモードのREPL
fn run_repl_vm() {
    println!("Tsumugi v0.1.0 [VM mode] — 終了するには Ctrl+D");
    let mut input = String::new();

    loop {
        if input.is_empty() {
            print!("tsumugi:vm> ");
        } else {
            print!("         .. ");
        }
        io::stdout().flush().unwrap();

        let mut line = String::new();
        let bytes = io::stdin().read_line(&mut line).unwrap();
        if bytes == 0 {
            println!();
            break;
        }

        input.push_str(&line);

        if is_incomplete(&input) {
            continue;
        }

        if let Err(e) = execute_vm(&input) {
            eprintln!("  エラー: {}", e);
        }

        input.clear();
    }
}

/// VMモードの実行関数
fn execute_vm(source: &str) -> Result<(), error::TsumugiError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();

    let mut parser = Parser::new(tokens);
    let program = parser.parse()?;

    let chunk = Compiler::new().compile(&program)?;
    let mut vm = Vm::new(chunk);
    vm.run()
}

/// VMモードの実行関数（ファイルパス付き）
fn execute_vm_with_path(source: &str, path: &str) -> Result<(), error::TsumugiError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();

    let mut parser = Parser::new(tokens);
    let program = parser.parse()?;

    let mut compiler = Compiler::new();
    compiler.set_base_dir(std::path::Path::new(path));
    let chunk = compiler.compile(&program)?;
    let mut vm = Vm::new(chunk);
    vm.run()
}
