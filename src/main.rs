mod ast;
mod env;
mod eval;
mod lexer;
mod parser;
mod token;
mod value;

use std::env as std_env;
use std::fs;
use std::io::{self, Write};

use eval::Evaluator;
use lexer::Lexer;
use parser::Parser;
use token::Token;

fn main() {
    let args: Vec<String> = std_env::args().collect();

    match args.len() {
        // 引数なし → REPL
        1 => run_repl(),
        // 引数あり → ファイル実行
        2 => run_file(&args[1]),
        _ => {
            eprintln!("使い方: tsumugi [script.tsg]");
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

    if let Err(e) = execute(&source, &mut Evaluator::new()) {
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
fn execute(source: &str, evaluator: &mut Evaluator) -> Result<(), String> {
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
            Token::If | Token::Fn | Token::While | Token::For => depth += 1,
            Token::End => depth -= 1,
            _ => {}
        }
    }
    depth > 0
}
