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
            Stmt::Let { name, value } => {
                let val = self.eval_expr(value)?;
                self.env.set(name, val);
                Ok(EvalResult::Val)
            }

            Stmt::Return { value } => {
                let val = self.eval_expr(value)?;
                Ok(EvalResult::Return(val))
            }

            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.eval_expr(condition)?;
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

            Stmt::While { condition, body } => {
                loop {
                    let cond = self.eval_expr(condition)?;
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

            Stmt::FnDef { name, params, body } => {
                let func = Function {
                    params: params.clone(),
                    body: body.clone(),
                };
                self.env.functions.insert(name.clone(), func);
                Ok(EvalResult::Val)
            }

            Stmt::ExprStmt { expr } => {
                self.eval_expr(expr)?;
                Ok(EvalResult::Val)
            }
        }
    }

    /// 式を評価して値を返す
    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, String> {
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
                .ok_or_else(|| format!("未定義の変数: {}", name)),

            Expr::BinOp { left, op, right } => {
                let l = self.eval_expr(left)?;
                let r = self.eval_expr(right)?;
                self.eval_binop(&l, op, &r)
            }

            Expr::UnaryOp { op, expr } => {
                let val = self.eval_expr(expr)?;
                self.eval_unary(op, &val)
            }

            Expr::Call { name, args } => self.eval_call(name, args),
        }
    }

    /// 二項演算
    fn eval_binop(&self, left: &Value, op: &BinOpKind, right: &Value) -> Result<Value, String> {
        match (left, op, right) {
            // 整数同士の算術
            (Value::Int(l), BinOpKind::Add, Value::Int(r)) => Ok(Value::Int(l + r)),
            (Value::Int(l), BinOpKind::Sub, Value::Int(r)) => Ok(Value::Int(l - r)),
            (Value::Int(l), BinOpKind::Mul, Value::Int(r)) => Ok(Value::Int(l * r)),
            (Value::Int(l), BinOpKind::Div, Value::Int(r)) => {
                if *r == 0 {
                    Err("ゼロ除算".to_string())
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
            (Value::Str(l), BinOpKind::Add, Value::Str(r)) => {
                Ok(Value::Str(format!("{}{}", l, r)))
            }

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
                "型エラー: {:?} {:?} {:?} は計算できません",
                left, op, right
            )),
        }
    }

    /// 単項演算
    fn eval_unary(&self, op: &UnaryOpKind, val: &Value) -> Result<Value, String> {
        match (op, val) {
            (UnaryOpKind::Neg, Value::Int(n)) => Ok(Value::Int(-n)),
            (UnaryOpKind::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
            (UnaryOpKind::Not, v) => Ok(Value::Bool(!v.is_truthy())),
            _ => Err(format!("型エラー: {:?} {:?} は計算できません", op, val)),
        }
    }

    /// 関数呼び出し
    fn eval_call(&mut self, name: &str, args: &[Expr]) -> Result<Value, String> {
        // 組み込み関数
        if name == "print" {
            let mut parts = Vec::new();
            for arg in args {
                let val = self.eval_expr(arg)?;
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
            .ok_or_else(|| format!("未定義の関数: {}", name))?;

        if args.len() != func.params.len() {
            return Err(format!(
                "関数 {} は引数{}個ですが、{}個渡されました",
                name,
                func.params.len(),
                args.len()
            ));
        }

        // 引数を評価
        let mut arg_values = Vec::new();
        for arg in args {
            arg_values.push(self.eval_expr(arg)?);
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
