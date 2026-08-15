//! コンパイラ: AST → バイトコード（Chunk）に変換

use crate::ast::{BinOpKind, Expr, Program, Stmt, UnaryOpKind};
use crate::chunk::Chunk;
use crate::error::TsumugiError;
use crate::opcode::OpCode;
use crate::value::Value;

/// AST をバイトコードにコンパイルする
pub struct Compiler {
    chunk: Chunk,
}

impl Compiler {
    pub fn new() -> Self {
        Compiler {
            chunk: Chunk::new(),
        }
    }

    /// プログラム全体をコンパイルして Chunk を返す
    pub fn compile(mut self, program: &Program) -> Result<Chunk, TsumugiError> {
        for stmt in program {
            self.compile_stmt(stmt)?;
        }
        // 最後に Return を追加（VM の実行終了を示す）
        self.chunk.emit(OpCode::Return, 0);
        Ok(self.chunk)
    }

    /// 文をコンパイル
    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), TsumugiError> {
        match stmt {
            Stmt::ExprStmt { expr, line } => {
                self.compile_expr(expr, *line)?;
                // 式文の結果はスタックに残るので捨てる
                self.chunk.emit(OpCode::Pop, *line);
            }
            _ => {
                // Phase 1 では ExprStmt（print呼び出し含む）のみ対応
                let line = stmt.line();
                return Err(TsumugiError::Runtime {
                    line,
                    message: "VM未対応の文です".to_string(),
                });
            }
        }
        Ok(())
    }

    /// 式をコンパイル（結果はスタックトップに積まれる）
    fn compile_expr(&mut self, expr: &Expr, line: usize) -> Result<(), TsumugiError> {
        match expr {
            Expr::Int(n) => {
                self.chunk.emit_constant(Value::Int(*n), line);
            }
            Expr::Float(n) => {
                self.chunk.emit_constant(Value::Float(*n), line);
            }
            Expr::Str(s) => {
                self.chunk.emit_constant(Value::Str(s.clone()), line);
            }
            Expr::Bool(b) => {
                self.chunk.emit_constant(Value::Bool(*b), line);
            }
            Expr::Null => {
                self.chunk.emit_constant(Value::Null, line);
            }
            Expr::BinOp { left, op, right } => {
                // 左辺 → 右辺の順にコンパイル（スタックに積む）
                self.compile_expr(left, line)?;
                self.compile_expr(right, line)?;
                // 演算命令を発行
                let opcode = match op {
                    BinOpKind::Add => OpCode::Add,
                    BinOpKind::Sub => OpCode::Sub,
                    BinOpKind::Mul => OpCode::Mul,
                    BinOpKind::Div => OpCode::Div,
                    BinOpKind::Mod => OpCode::Mod,
                    BinOpKind::Eq => OpCode::Eq,
                    BinOpKind::NotEq => OpCode::NotEq,
                    BinOpKind::Lt => OpCode::Lt,
                    BinOpKind::Gt => OpCode::Gt,
                    BinOpKind::LtEq => OpCode::LtEq,
                    BinOpKind::GtEq => OpCode::GtEq,
                    BinOpKind::And => OpCode::Add, // TODO: Phase 3 で短絡評価に対応
                    BinOpKind::Or => OpCode::Add,  // TODO: Phase 3 で短絡評価に対応
                };
                self.chunk.emit(opcode, line);
            }
            Expr::UnaryOp { op, expr } => {
                self.compile_expr(expr, line)?;
                match op {
                    UnaryOpKind::Neg => self.chunk.emit(OpCode::Negate, line),
                    UnaryOpKind::Not => self.chunk.emit(OpCode::Not, line),
                }
            }
            Expr::Call { callee, args } => {
                // Phase 1: print のみ対応
                if let Expr::Ident(name) = callee.as_ref()
                    && name == "print"
                {
                    let arg_count = args.len();
                    for arg in args {
                        self.compile_expr(arg, line)?;
                    }
                    self.chunk.emit(OpCode::Print(arg_count), line);
                    // print は null を返す（スタックに null を積む）
                    self.chunk.emit_constant(Value::Null, line);
                    return Ok(());
                }
                return Err(TsumugiError::Runtime {
                    line,
                    message: "VM未対応: print以外の関数呼び出し".to_string(),
                });
            }
            _ => {
                return Err(TsumugiError::Runtime {
                    line,
                    message: "VM未対応の式です".to_string(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn compile_source(source: &str) -> Chunk {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        Compiler::new().compile(&program).unwrap()
    }

    #[test]
    fn compile_print_int() {
        let chunk = compile_source("print(42)");
        // 定数テーブル: [42, null]
        assert_eq!(chunk.constants[0], Value::Int(42));
        assert_eq!(chunk.constants[1], Value::Null);
        // 命令: LoadConst(0), Print(1), LoadConst(1), Pop, Return
        assert_eq!(chunk.code[0], OpCode::LoadConst(0));
        assert_eq!(chunk.code[1], OpCode::Print(1));
        assert_eq!(chunk.code[2], OpCode::LoadConst(1));
        assert_eq!(chunk.code[3], OpCode::Pop);
        assert_eq!(chunk.code[4], OpCode::Return);
    }

    #[test]
    fn compile_arithmetic() {
        let chunk = compile_source("print(1 + 2)");
        // 定数テーブル: [1, 2, null]
        assert_eq!(chunk.constants[0], Value::Int(1));
        assert_eq!(chunk.constants[1], Value::Int(2));
        // 命令: LoadConst(0), LoadConst(1), Add, Print(1), LoadConst(2), Pop, Return
        assert_eq!(chunk.code[0], OpCode::LoadConst(0));
        assert_eq!(chunk.code[1], OpCode::LoadConst(1));
        assert_eq!(chunk.code[2], OpCode::Add);
        assert_eq!(chunk.code[3], OpCode::Print(1));
    }
}
