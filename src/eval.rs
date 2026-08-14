use crate::ast::*;
use crate::env::{Env, Function};
use crate::value::Value;

use std::collections::BTreeMap;

/// 評価器の戻り値（通常の値 or return / break / continue による制御フロー）
enum EvalResult {
    Val,
    Return(Value),
    Break,
    Continue,
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
            match self.exec_stmt(stmt)? {
                EvalResult::Return(_) => break,
                EvalResult::Break => {
                    return Err(format!(
                        "{}行目: break はループの中でのみ使用できます",
                        stmt.line()
                    ));
                }
                EvalResult::Continue => {
                    return Err(format!(
                        "{}行目: continue はループの中でのみ使用できます",
                        stmt.line()
                    ));
                }
                EvalResult::Val => {}
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

            Stmt::Assign { name, value, line } => {
                let val = self.eval_expr(value, *line)?;
                if self.env.update(name, val).is_err() {
                    return Err(format!("{}行目: 未定義の変数に代入: {}", line, name));
                }
                Ok(EvalResult::Val)
            }

            Stmt::IndexAssign {
                object,
                index,
                value,
                line,
            } => {
                let idx = self.eval_expr(index, *line)?;
                let val = self.eval_expr(value, *line)?;

                // object は変数参照であるはず
                let var_name = match object {
                    Expr::Ident(name) => name.clone(),
                    _ => {
                        return Err(format!(
                            "{}行目: インデックス代入の対象は変数である必要があります",
                            line
                        ));
                    }
                };

                let target = self
                    .env
                    .get_mut(&var_name)
                    .ok_or_else(|| format!("{}行目: 未定義の変数: {}", line, var_name))?;

                match target {
                    Value::List(list) => {
                        let i = match &idx {
                            Value::Int(n) => *n,
                            _ => {
                                return Err(format!(
                                    "{}行目: リストのインデックスは整数である必要があります",
                                    line
                                ));
                            }
                        };
                        let len = list.len() as i64;
                        let actual_idx = if i < 0 { len + i } else { i };
                        if actual_idx < 0 || actual_idx >= len {
                            return Err(format!(
                                "{}行目: インデックス範囲外: {} (長さ: {})",
                                line, i, len
                            ));
                        }
                        list[actual_idx as usize] = val;
                    }
                    Value::Dict(map) => {
                        let key = match &idx {
                            Value::Str(s) => s.clone(),
                            _ => {
                                return Err(format!(
                                    "{}行目: 辞書のキーは文字列である必要があります",
                                    line
                                ));
                            }
                        };
                        map.insert(key, val);
                    }
                    _ => {
                        return Err(format!(
                            "{}行目: インデックス代入はリストまたは辞書にのみ使用できます",
                            line
                        ));
                    }
                }

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
                    match self.exec_stmt(s)? {
                        EvalResult::Val => {}
                        other => return Ok(other),
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
                    let mut should_break = false;
                    for s in body {
                        match self.exec_stmt(s)? {
                            EvalResult::Return(v) => return Ok(EvalResult::Return(v)),
                            EvalResult::Break => {
                                should_break = true;
                                break;
                            }
                            EvalResult::Continue => break,
                            EvalResult::Val => {}
                        }
                    }
                    if should_break {
                        break;
                    }
                }
                Ok(EvalResult::Val)
            }

            Stmt::For {
                var,
                iter,
                body,
                line,
            } => {
                let collection = self.eval_expr(iter, *line)?;
                let items: Vec<Value> = match &collection {
                    Value::List(list) => list.clone(),
                    Value::Dict(map) => map.keys().map(|k| Value::Str(k.clone())).collect(),
                    Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string())).collect(),
                    _ => {
                        return Err(format!(
                            "{}行目: for で反復できません: {:?}",
                            line, collection
                        ));
                    }
                };

                for item in items {
                    self.env.set(var, item);
                    let mut should_break = false;
                    for s in body {
                        match self.exec_stmt(s)? {
                            EvalResult::Return(v) => return Ok(EvalResult::Return(v)),
                            EvalResult::Break => {
                                should_break = true;
                                break;
                            }
                            EvalResult::Continue => break,
                            EvalResult::Val => {}
                        }
                    }
                    if should_break {
                        break;
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

            Stmt::Break { .. } => Ok(EvalResult::Break),

            Stmt::Continue { .. } => Ok(EvalResult::Continue),

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

            Expr::List(items) => {
                let mut values = Vec::new();
                for item in items {
                    values.push(self.eval_expr(item, line)?);
                }
                Ok(Value::List(values))
            }

            Expr::Dict(pairs) => {
                let mut map = BTreeMap::new();
                for (key_expr, val_expr) in pairs {
                    let key = match self.eval_expr(key_expr, line)? {
                        Value::Str(s) => s,
                        other => {
                            return Err(format!(
                                "{}行目: 辞書のキーは文字列である必要があります。got: {:?}",
                                line, other
                            ));
                        }
                    };
                    let val = self.eval_expr(val_expr, line)?;
                    map.insert(key, val);
                }
                Ok(Value::Dict(map))
            }

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

            Expr::Index { object, index } => {
                let obj = self.eval_expr(object, line)?;
                let idx = self.eval_expr(index, line)?;
                self.eval_index(&obj, &idx, line)
            }
        }
    }

    /// インデックスアクセスの評価
    fn eval_index(&self, object: &Value, index: &Value, line: usize) -> Result<Value, String> {
        match (object, index) {
            (Value::List(list), Value::Int(i)) => {
                let len = list.len() as i64;
                let actual = if *i < 0 { len + *i } else { *i };
                if actual < 0 || actual >= len {
                    return Err(format!(
                        "{}行目: インデックス範囲外: {} (長さ: {})",
                        line, i, len
                    ));
                }
                Ok(list[actual as usize].clone())
            }
            (Value::Dict(map), Value::Str(key)) => Ok(map.get(key).cloned().unwrap_or(Value::Null)),
            (Value::Str(s), Value::Int(i)) => {
                let len = s.chars().count() as i64;
                let actual = if *i < 0 { len + *i } else { *i };
                if actual < 0 || actual >= len {
                    return Err(format!(
                        "{}行目: インデックス範囲外: {} (長さ: {})",
                        line, i, len
                    ));
                }
                let ch = s.chars().nth(actual as usize).unwrap();
                Ok(Value::Str(ch.to_string()))
            }
            _ => Err(format!(
                "{}行目: インデックスアクセスできません: {:?}[{:?}]",
                line, object, index
            )),
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
            (Value::Int(l), BinOpKind::Mod, Value::Int(r)) => {
                if *r == 0 {
                    Err(format!("{}行目: ゼロ除算", line))
                } else {
                    Ok(Value::Int(l % r))
                }
            }

            // 浮動小数点
            (Value::Float(l), BinOpKind::Add, Value::Float(r)) => Ok(Value::Float(l + r)),
            (Value::Float(l), BinOpKind::Sub, Value::Float(r)) => Ok(Value::Float(l - r)),
            (Value::Float(l), BinOpKind::Mul, Value::Float(r)) => Ok(Value::Float(l * r)),
            (Value::Float(l), BinOpKind::Div, Value::Float(r)) => Ok(Value::Float(l / r)),
            (Value::Float(l), BinOpKind::Mod, Value::Float(r)) => Ok(Value::Float(l % r)),

            // Int と Float の混合
            (Value::Int(l), BinOpKind::Add, Value::Float(r)) => Ok(Value::Float(*l as f64 + r)),
            (Value::Float(l), BinOpKind::Add, Value::Int(r)) => Ok(Value::Float(l + *r as f64)),
            (Value::Int(l), BinOpKind::Sub, Value::Float(r)) => Ok(Value::Float(*l as f64 - r)),
            (Value::Float(l), BinOpKind::Sub, Value::Int(r)) => Ok(Value::Float(l - *r as f64)),
            (Value::Int(l), BinOpKind::Mul, Value::Float(r)) => Ok(Value::Float(*l as f64 * r)),
            (Value::Float(l), BinOpKind::Mul, Value::Int(r)) => Ok(Value::Float(l * *r as f64)),
            (Value::Int(l), BinOpKind::Div, Value::Float(r)) => Ok(Value::Float(*l as f64 / r)),
            (Value::Float(l), BinOpKind::Div, Value::Int(r)) => Ok(Value::Float(l / *r as f64)),
            (Value::Int(l), BinOpKind::Mod, Value::Float(r)) => Ok(Value::Float(*l as f64 % r)),
            (Value::Float(l), BinOpKind::Mod, Value::Int(r)) => Ok(Value::Float(l % *r as f64)),

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
        match name {
            "print" => {
                let mut parts = Vec::new();
                for arg in args {
                    let val = self.eval_expr(arg, line)?;
                    parts.push(val.to_string());
                }
                println!("{}", parts.join(" "));
                return Ok(Value::Null);
            }
            "len" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: len() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let val = self.eval_expr(&args[0], line)?;
                let length = match &val {
                    Value::Str(s) => s.chars().count() as i64,
                    Value::List(v) => v.len() as i64,
                    Value::Dict(m) => m.len() as i64,
                    _ => {
                        return Err(format!(
                            "{}行目: len() は文字列・リスト・辞書にのみ使用できます",
                            line
                        ));
                    }
                };
                return Ok(Value::Int(length));
            }
            "push" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: push() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                // 第1引数は変数名（リストへの参照）
                let var_name = match &args[0] {
                    Expr::Ident(name) => name.clone(),
                    _ => {
                        return Err(format!(
                            "{}行目: push() の第1引数はリスト変数である必要があります",
                            line
                        ));
                    }
                };
                let val = self.eval_expr(&args[1], line)?;
                let target = self
                    .env
                    .get_mut(&var_name)
                    .ok_or_else(|| format!("{}行目: 未定義の変数: {}", line, var_name))?;
                match target {
                    Value::List(list) => {
                        list.push(val);
                    }
                    _ => {
                        return Err(format!("{}行目: push() はリストにのみ使用できます", line));
                    }
                }
                return Ok(Value::Null);
            }
            "keys" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: keys() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let val = self.eval_expr(&args[0], line)?;
                match val {
                    Value::Dict(map) => {
                        let key_list: Vec<Value> =
                            map.keys().map(|k| Value::Str(k.clone())).collect();
                        return Ok(Value::List(key_list));
                    }
                    _ => {
                        return Err(format!("{}行目: keys() は辞書にのみ使用できます", line));
                    }
                }
            }
            "type" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: type() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let val = self.eval_expr(&args[0], line)?;
                let type_name = match &val {
                    Value::Int(_) => "int",
                    Value::Float(_) => "float",
                    Value::Str(_) => "str",
                    Value::Bool(_) => "bool",
                    Value::Null => "null",
                    Value::List(_) => "list",
                    Value::Dict(_) => "dict",
                };
                return Ok(Value::Str(type_name.to_string()));
            }
            _ => {}
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
                EvalResult::Break => {
                    self.env.pop_scope();
                    return Err(format!(
                        "{}行目: break はループの中でのみ使用できます",
                        line
                    ));
                }
                EvalResult::Continue => {
                    self.env.pop_scope();
                    return Err(format!(
                        "{}行目: continue はループの中でのみ使用できます",
                        line
                    ));
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
    fn assign_variable() {
        let src = "let x = 1\nx = 2\nprint(x)";
        run_program(src).unwrap();
    }

    #[test]
    fn assign_in_while_loop() {
        let src = "let count = 3\nwhile count > 0\n  count = count - 1\nend\nprint(count)";
        run_program(src).unwrap();
    }

    #[test]
    fn assign_undefined_variable_error() {
        let result = run_program("x = 42");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("1行目"), "should mention line 1: {}", msg);
        assert!(msg.contains("未定義の変数に代入"));
    }

    #[test]
    fn assign_updates_outer_scope() {
        // 関数内から引数を再代入して、関数内で反映されることを確認
        let src = "fn countdown(n)\n  while n > 0\n    print(n)\n    n = n - 1\n  end\n  return n\nend\nlet r = countdown(3)\nprint(r)";
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

    #[test]
    fn list_literal_and_index() {
        run_program("let xs = [1, 2, 3]\nprint(xs[0])\nprint(xs[-1])").unwrap();
    }

    #[test]
    fn list_index_assign() {
        run_program("let xs = [1, 2, 3]\nxs[1] = 99\nprint(xs[1])").unwrap();
    }

    #[test]
    fn dict_literal_and_access() {
        run_program("let d = {\"a\": 1, \"b\": 2}\nprint(d[\"a\"])").unwrap();
    }

    #[test]
    fn dict_index_assign() {
        run_program("let d = {}\nd[\"x\"] = 42\nprint(d[\"x\"])").unwrap();
    }

    #[test]
    fn builtin_len() {
        run_program("let xs = [1, 2, 3]\nprint(len(xs))\nprint(len(\"hello\"))").unwrap();
    }

    #[test]
    fn builtin_push() {
        run_program("let xs = []\npush(xs, 1)\npush(xs, 2)\nprint(len(xs))").unwrap();
    }

    #[test]
    fn builtin_keys() {
        run_program("let d = {\"a\": 1}\nlet ks = keys(d)\nprint(len(ks))").unwrap();
    }

    #[test]
    fn builtin_type() {
        run_program("print(type(42))\nprint(type([]))\nprint(type({}))").unwrap();
    }

    #[test]
    fn index_out_of_bounds() {
        let result = run_program("let xs = [1, 2]\nprint(xs[5])");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("インデックス範囲外"));
    }

    #[test]
    fn for_loop_list() {
        run_program("let xs = [1, 2, 3]\nfor x in xs\n  print(x)\nend").unwrap();
    }

    #[test]
    fn for_loop_dict() {
        run_program("let d = {\"a\": 1}\nfor k in d\n  print(k)\nend").unwrap();
    }

    #[test]
    fn for_loop_string() {
        run_program("for ch in \"hi\"\n  print(ch)\nend").unwrap();
    }

    #[test]
    fn for_loop_accumulate() {
        run_program("let total = 0\nfor n in [1, 2, 3]\n  total = total + n\nend\nprint(total)")
            .unwrap();
    }

    #[test]
    fn for_loop_non_iterable_error() {
        let result = run_program("for x in 42\n  print(x)\nend");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("for で反復できません"));
    }

    #[test]
    fn break_in_while() {
        run_program("let i = 0\nwhile true\n  if i == 3\n    break\n  end\n  i = i + 1\nend")
            .unwrap();
    }

    #[test]
    fn break_in_for() {
        run_program("for n in [1, 2, 3, 4, 5]\n  if n == 3\n    break\n  end\n  print(n)\nend")
            .unwrap();
    }

    #[test]
    fn continue_in_for() {
        run_program("for n in [1, 2, 3, 4, 5]\n  if n == 3\n    continue\n  end\n  print(n)\nend")
            .unwrap();
    }

    #[test]
    fn break_outside_loop_error() {
        let result = run_program("break");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("break はループの中でのみ"));
    }

    #[test]
    fn continue_outside_loop_error() {
        let result = run_program("continue");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("continue はループの中でのみ"));
    }

    #[test]
    fn modulo_operator() {
        run_program("let x = 10 % 3\nprint(x)").unwrap();
    }

    #[test]
    fn modulo_zero_error() {
        let result = run_program("let x = 10 % 0");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ゼロ除算"));
    }
}
