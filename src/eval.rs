use crate::ast::*;
use crate::env::{Env, Function};
use crate::value::Value;

/// 評価器の戻り値（通常の値 or return による早期脱出）
enum EvalResult {
    Val,
    Return(Value),
}

/// AST を評価して実行する
pub struct Evaluator {
    env: Env,
}

impl Evaluator {
    pub fn new() -> Self {
        Self { env: Env::new() }
    }

    /// プログラム全体を実行
    pub fn run(&mut self, program: &Program) -> Result<(), String> {
        for stmt in program {
            if let EvalResult::Return(_) = self.exec_stmt(stmt)? {
                // トップレベルの return は無視
                break;
            }
        }
        Ok(())
    }

    /// 文を実行
    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<EvalResult, String> {
        match stmt {
            Stmt::Let { name, value, line } => {
                let val = self.eval_expr(value, *line)?;
                self.env.set(name, val);
                Ok(EvalResult::Val)
            }

            Stmt::Return { value, line } => {
                let val = self.eval_expr(value, *line)?;
                Ok(EvalResult::Return(val))
            }

            Stmt::If {
                condition,
                then_body,
                else_body,
                line,
            } => {
                let cond = self.eval_expr(condition, *line)?;
                let body = if cond.is_truthy() {
                    then_body
                } else {
                    else_body
                };
                for s in body {
                    if let EvalResult::Return(v) = self.exec_stmt(s)? {
                        return Ok(EvalResult::Return(v));
                    }
                }
                Ok(EvalResult::Val)
            }

            Stmt::While {
                condition,
                body,
                line,
            } => {
                loop {
                    let cond = self.eval_expr(condition, *line)?;
                    if !cond.is_truthy() {
                        break;
                    }
                    for s in body {
                        if let EvalResult::Return(v) = self.exec_stmt(s)? {
                            return Ok(EvalResult::Return(v));
                        }
                    }
                }
                Ok(EvalResult::Val)
            }

            Stmt::FnDef {
                name, params, body, ..
            } => {
                let func = Function {
                    params: params.clone(),
                    body: body.clone(),
                };
                self.env.functions.insert(name.clone(), func);
                Ok(EvalResult::Val)
            }

            Stmt::ExprStmt { expr, line } => {
                self.eval_expr(expr, *line)?;
                Ok(EvalResult::Val)
            }
        }
    }

    /// 式を評価して値を返す（line は文の行番号をエラー表示に使う）
    fn eval_expr(&mut self, expr: &Expr, line: usize) -> Result<Value, String> {
        match expr {
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),

            Expr::Ident(name) => self
                .env
                .get(name)
                .cloned()
                .ok_or_else(|| format!("{}行目: 未定義の変数: {}", line, name)),

            Expr::BinOp { left, op, right } => {
                let l = self.eval_expr(left, line)?;
                let r = self.eval_expr(right, line)?;
                self.eval_binop(&l, op, &r, line)
            }

            Expr::UnaryOp { op, expr } => {
                let val = self.eval_expr(expr, line)?;
                self.eval_unary(op, &val, line)
            }

            Expr::Call { name, args } => self.eval_call(name, args, line),
        }
    }

    /// 二項演算
    fn eval_binop(
        &self,
        left: &Value,
        op: &BinOpKind,
        right: &Value,
        line: usize,
    ) -> Result<Value, String> {
        match (left, op, right) {
            // 整数同士の算術
            (Value::Int(l), BinOpKind::Add, Value::Int(r)) => Ok(Value::Int(l + r)),
            (Value::Int(l), BinOpKind::Sub, Value::Int(r)) => Ok(Value::Int(l - r)),
            (Value::Int(l), BinOpKind::Mul, Value::Int(r)) => Ok(Value::Int(l * r)),
            (Value::Int(l), BinOpKind::Div, Value::Int(r)) => {
                if *r == 0 {
                    Err(format!("{}行目: ゼロ除算", line))
                } else {
                    Ok(Value::Int(l / r))
                }
            }

            // 浮動小数点
            (Value::Float(l), BinOpKind::Add, Value::Float(r)) => Ok(Value::Float(l + r)),
            (Value::Float(l), BinOpKind::Sub, Value::Float(r)) => Ok(Value::Float(l - r)),
            (Value::Float(l), BinOpKind::Mul, Value::Float(r)) => Ok(Value::Float(l * r)),
            (Value::Float(l), BinOpKind::Div, Value::Float(r)) => Ok(Value::Float(l / r)),

            // Int と Float の混合
            (Value::Int(l), BinOpKind::Add, Value::Float(r)) => Ok(Value::Float(*l as f64 + r)),
            (Value::Float(l), BinOpKind::Add, Value::Int(r)) => Ok(Value::Float(l + *r as f64)),
            (Value::Int(l), BinOpKind::Sub, Value::Float(r)) => Ok(Value::Float(*l as f64 - r)),
            (Value::Float(l), BinOpKind::Sub, Value::Int(r)) => Ok(Value::Float(l - *r as f64)),
            (Value::Int(l), BinOpKind::Mul, Value::Float(r)) => Ok(Value::Float(*l as f64 * r)),
            (Value::Float(l), BinOpKind::Mul, Value::Int(r)) => Ok(Value::Float(l * *r as f64)),
            (Value::Int(l), BinOpKind::Div, Value::Float(r)) => Ok(Value::Float(*l as f64 / r)),
            (Value::Float(l), BinOpKind::Div, Value::Int(r)) => Ok(Value::Float(l / *r as f64)),

            // 文字列結合
            (Value::Str(l), BinOpKind::Add, Value::Str(r)) => Ok(Value::Str(format!("{}{}", l, r))),

            // 比較演算（整数）
            (Value::Int(l), BinOpKind::Eq, Value::Int(r)) => Ok(Value::Bool(l == r)),
            (Value::Int(l), BinOpKind::NotEq, Value::Int(r)) => Ok(Value::Bool(l != r)),
            (Value::Int(l), BinOpKind::Lt, Value::Int(r)) => Ok(Value::Bool(l < r)),
            (Value::Int(l), BinOpKind::Gt, Value::Int(r)) => Ok(Value::Bool(l > r)),
            (Value::Int(l), BinOpKind::LtEq, Value::Int(r)) => Ok(Value::Bool(l <= r)),
            (Value::Int(l), BinOpKind::GtEq, Value::Int(r)) => Ok(Value::Bool(l >= r)),

            // 比較演算（文字列）
            (Value::Str(l), BinOpKind::Eq, Value::Str(r)) => Ok(Value::Bool(l == r)),
            (Value::Str(l), BinOpKind::NotEq, Value::Str(r)) => Ok(Value::Bool(l != r)),

            // 論理演算
            (l, BinOpKind::And, r) => Ok(Value::Bool(l.is_truthy() && r.is_truthy())),
            (l, BinOpKind::Or, r) => Ok(Value::Bool(l.is_truthy() || r.is_truthy())),

            _ => Err(format!(
                "{}行目: 型エラー: {:?} {:?} {:?} は計算できません",
                line, left, op, right
            )),
        }
    }

    /// 単項演算
    fn eval_unary(&self, op: &UnaryOpKind, val: &Value, line: usize) -> Result<Value, String> {
        match (op, val) {
            (UnaryOpKind::Neg, Value::Int(n)) => Ok(Value::Int(-n)),
            (UnaryOpKind::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
            (UnaryOpKind::Not, v) => Ok(Value::Bool(!v.is_truthy())),
            _ => Err(format!(
                "{}行目: 型エラー: {:?} {:?} は計算できません",
                line, op, val
            )),
        }
    }

    /// 関数呼び出し
    fn eval_call(&mut self, name: &str, args: &[Expr], line: usize) -> Result<Value, String> {
        // 組み込み関数
        if name == "print" {
            let mut parts = Vec::new();
            for arg in args {
                let val = self.eval_expr(arg, line)?;
                parts.push(val.to_string());
            }
            println!("{}", parts.join(" "));
            return Ok(Value::Null);
        }

        // ユーザー定義関数
        let func = self
            .env
            .functions
            .get(name)
            .cloned()
            .ok_or_else(|| format!("{}行目: 未定義の関数: {}", line, name))?;

        if args.len() != func.params.len() {
            return Err(format!(
                "{}行目: 関数 {} は引数{}個ですが、{}個渡されました",
                line,
                name,
                func.params.len(),
                args.len()
            ));
        }

        // 引数を評価
        let mut arg_values = Vec::new();
        for arg in args {
            arg_values.push(self.eval_expr(arg, line)?);
        }

        // 新しいスコープを作って引数をバインド
        self.env.push_scope();
        for (param, val) in func.params.iter().zip(arg_values) {
            self.env.set(param, val);
        }

        // 関数本体を実行
        let mut result = Value::Null;
        for stmt in &func.body {
            match self.exec_stmt(stmt)? {
                EvalResult::Return(v) => {
                    result = v;
                    break;
                }
                EvalResult::Val => {}
            }
        }

        self.env.pop_scope();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn run_program(input: &str) -> Result<(), String> {
        let tokens = Lexer::new(input).tokenize();
        let program = Parser::new(tokens).parse()?;
        let mut eval = Evaluator::new();
        eval.run(&program)
    }

    #[test]
    fn arithmetic() {
        // Should not error
        run_program("let x = 1 + 2 * 3").unwrap();
    }

    #[test]
    fn function_call() {
        let src = "fn add(a, b)\n  return a + b\nend\nlet r = add(3, 4)";
        run_program(src).unwrap();
    }

    #[test]
    fn undefined_variable_error() {
        let result = run_program("let x = 10\nprint(y)");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("2行目"), "should mention line 2: {}", msg);
        assert!(msg.contains("未定義の変数"));
    }

    #[test]
    fn zero_division_error() {
        let result = run_program("let x = 10 / 0");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("1行目"));
        assert!(msg.contains("ゼロ除算"));
    }

    #[test]
    fn type_error() {
        let result = run_program("let x = \"hello\" + 1");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("型エラー"));
    }

    #[test]
    fn undefined_function_error() {
        let result = run_program("foo(1, 2)");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("未定義の関数"));
    }

    #[test]
    fn wrong_arg_count() {
        let src = "fn f(a)\n  return a\nend\nf(1, 2)";
        let result = run_program(src);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("引数"));
    }

    #[test]
    fn while_loop() {
        // Just confirm it doesn't panic or infinite loop
        let src = "let i = 3\nwhile i > 0\n  let i = i - 1\nend";
        run_program(src).unwrap();
    }

    #[test]
    fn if_else() {
        let src = "if false\n  print(1)\nelse\n  print(2)\nend";
        run_program(src).unwrap();
    }

    #[test]
    fn string_concat() {
        run_program("let s = \"hello\" + \" world\"").unwrap();
    }

    #[test]
    fn logical_ops() {
        run_program("let x = true and false\nlet y = true or false\nlet z = not true").unwrap();
    }
}
