mod ast;
mod env;
mod eval;
mod lexer;
mod parser;
mod token;
mod value;

use eval::Evaluator;
use lexer::Lexer;
use parser::Parser;

fn main() {
    let source = r#"
# Tsumugi サンプルプログラム
let x = 10
let name = "tsumugi"

print("hello, " + name)
print(x + 3)

if x > 5
    print("x is big")
else
    print("x is small")
end

fn add(a, b)
    return a + b
end

let result = add(3, 4)
print(result)

# while ループ
let count = 3
while count > 0
    print(count)
    let count = count - 1
end

print("done!")
"#;

    // レキサー → パーサー → 評価器
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();

    let mut parser = Parser::new(tokens);
    match parser.parse() {
        Ok(program) => {
            let mut evaluator = Evaluator::new();
            if let Err(e) = evaluator.run(&program) {
                eprintln!("実行エラー: {}", e);
            }
        }
        Err(e) => {
            eprintln!("パースエラー: {}", e);
        }
    }
}
