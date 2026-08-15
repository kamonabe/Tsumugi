//! コンパイラ: AST → バイトコード（Chunk）に変換

use crate::ast::{BinOpKind, Expr, Program, Stmt, UnaryOpKind};
use crate::chunk::Chunk;
use crate::error::TsumugiError;
use crate::opcode::OpCode;
use crate::value::Value;

/// ローカル変数の情報
#[derive(Debug, Clone)]
struct Local {
    name: String,
    /// スコープの深さ（0 = トップレベル）— Phase 3 で使用
    #[allow(dead_code)]
    depth: usize,
}

/// AST をバイトコードにコンパイルする
pub struct Compiler {
    chunk: Chunk,
    /// ローカル変数テーブル（スタック上の位置 = Vec のインデックス）
    locals: Vec<Local>,
    /// 現在のスコープの深さ
    scope_depth: usize,
}

impl Compiler {
    pub fn new() -> Self {
        Compiler {
            chunk: Chunk::new(),
            locals: Vec::new(),
            scope_depth: 0,
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
            Stmt::Let { name, value, line } => {
                // 初期値をコンパイル → スタックトップに値が残る
                self.compile_expr(value, *line)?;
                // ローカル変数として登録（スタック上の現在位置に対応）
                self.add_local(name.clone());
                // let 文では Pop しない — 値がスタック上に変数のスロットとして残る
            }
            Stmt::Assign { name, value, line } => {
                // 新しい値をコンパイル
                self.compile_expr(value, *line)?;
                // 変数のスロットを探して SetLocal を発行
                let slot = self.resolve_local(name, *line)?;
                self.chunk.emit(OpCode::SetLocal(slot), *line);
                // 代入文は式文ではないが、SetLocal は値をスタックに残すので Pop する
                self.chunk.emit(OpCode::Pop, *line);
            }
            Stmt::ExprStmt { expr, line } => {
                self.compile_expr(expr, *line)?;
                // 式文の結果はスタックに残るので捨てる
                self.chunk.emit(OpCode::Pop, *line);
            }
            _ => {
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
            Expr::Ident(name) => {
                // 変数参照 → GetLocal
                let slot = self.resolve_local(name, line)?;
                self.chunk.emit(OpCode::GetLocal(slot), line);
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
                // Phase 1-2: print のみ対応
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

    // --- ローカル変数管理 ---

    /// ローカル変数を追加
    fn add_local(&mut self, name: String) {
        self.locals.push(Local {
            name,
            depth: self.scope_depth,
        });
    }

    /// 変数名からスタック上のスロット位置を検索（内側→外側）
    fn resolve_local(&self, name: &str, line: usize) -> Result<usize, TsumugiError> {
        // 後ろから探す（内側のスコープが優先）
        for (i, local) in self.locals.iter().enumerate().rev() {
            if local.name == name {
                return Ok(i);
            }
        }
        Err(TsumugiError::Runtime {
            line,
            message: format!("未定義の変数: {}", name),
        })
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
        assert_eq!(chunk.constants[0], Value::Int(42));
        assert_eq!(chunk.constants[1], Value::Null);
        assert_eq!(chunk.code[0], OpCode::LoadConst(0));
        assert_eq!(chunk.code[1], OpCode::Print(1));
        assert_eq!(chunk.code[2], OpCode::LoadConst(1));
        assert_eq!(chunk.code[3], OpCode::Pop);
        assert_eq!(chunk.code[4], OpCode::Return);
    }

    #[test]
    fn compile_arithmetic() {
        let chunk = compile_source("print(1 + 2)");
        assert_eq!(chunk.constants[0], Value::Int(1));
        assert_eq!(chunk.constants[1], Value::Int(2));
        assert_eq!(chunk.code[0], OpCode::LoadConst(0));
        assert_eq!(chunk.code[1], OpCode::LoadConst(1));
        assert_eq!(chunk.code[2], OpCode::Add);
        assert_eq!(chunk.code[3], OpCode::Print(1));
    }

    #[test]
    fn compile_let_and_get() {
        let chunk = compile_source("let x = 10\nprint(x)");
        // 定数: [10, null]
        assert_eq!(chunk.constants[0], Value::Int(10));
        // 命令: LoadConst(0) → x のスロット確保
        //       GetLocal(0), Print(1), LoadConst(1), Pop, Return
        assert_eq!(chunk.code[0], OpCode::LoadConst(0)); // let x = 10
        assert_eq!(chunk.code[1], OpCode::GetLocal(0)); // print(x) の x
        assert_eq!(chunk.code[2], OpCode::Print(1));
    }

    #[test]
    fn compile_let_and_assign() {
        let chunk = compile_source("let x = 1\nx = 2\nprint(x)");
        // LoadConst(0) → let x = 1 (slot 0)
        // LoadConst(1) → 2 (新しい値)
        // SetLocal(0)  → x に代入
        // Pop          → 代入文なので Pop
        // GetLocal(0)  → print(x) の x
        assert_eq!(chunk.code[0], OpCode::LoadConst(0));
        assert_eq!(chunk.code[1], OpCode::LoadConst(1));
        assert_eq!(chunk.code[2], OpCode::SetLocal(0));
        assert_eq!(chunk.code[3], OpCode::Pop);
        assert_eq!(chunk.code[4], OpCode::GetLocal(0));
        assert_eq!(chunk.code[5], OpCode::Print(1));
    }

    #[test]
    fn compile_multiple_vars() {
        let chunk = compile_source("let a = 1\nlet b = 2\nprint(a + b)");
        // LoadConst(0) → let a = 1 (slot 0)
        // LoadConst(1) → let b = 2 (slot 1)
        // GetLocal(0), GetLocal(1), Add, Print(1)
        assert_eq!(chunk.code[0], OpCode::LoadConst(0));
        assert_eq!(chunk.code[1], OpCode::LoadConst(1));
        assert_eq!(chunk.code[2], OpCode::GetLocal(0));
        assert_eq!(chunk.code[3], OpCode::GetLocal(1));
        assert_eq!(chunk.code[4], OpCode::Add);
        assert_eq!(chunk.code[5], OpCode::Print(1));
    }

    #[test]
    fn compile_undefined_var_error() {
        let mut lexer = Lexer::new("print(z)");
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        let result = Compiler::new().compile(&program);
        assert!(result.is_err());
    }
}
