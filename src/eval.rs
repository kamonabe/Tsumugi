use crate::ast::*;
use crate::env::{Env, Function};
use crate::value::Value;

use std::collections::BTreeMap;
use std::fs;

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
            "pop" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: pop() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let var_name = match &args[0] {
                    Expr::Ident(name) => name.clone(),
                    _ => {
                        return Err(format!(
                            "{}行目: pop() の引数はリスト変数である必要があります",
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
                        if list.is_empty() {
                            return Err(format!("{}行目: 空のリストから pop できません", line));
                        }
                        let val = list.pop().unwrap();
                        return Ok(val);
                    }
                    _ => {
                        return Err(format!("{}行目: pop() はリストにのみ使用できます", line));
                    }
                }
            }
            "slice" => {
                if args.len() != 3 {
                    return Err(format!(
                        "{}行目: slice() は引数3個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let collection = self.eval_expr(&args[0], line)?;
                let start = match self.eval_expr(&args[1], line)? {
                    Value::Int(n) => n as usize,
                    _ => {
                        return Err(format!(
                            "{}行目: slice() の第2引数は整数である必要があります",
                            line
                        ));
                    }
                };
                let end = match self.eval_expr(&args[2], line)? {
                    Value::Int(n) => n as usize,
                    _ => {
                        return Err(format!(
                            "{}行目: slice() の第3引数は整数である必要があります",
                            line
                        ));
                    }
                };
                match collection {
                    Value::List(list) => {
                        let end = end.min(list.len());
                        let start = start.min(end);
                        return Ok(Value::List(list[start..end].to_vec()));
                    }
                    Value::Str(s) => {
                        let chars: Vec<char> = s.chars().collect();
                        let end = end.min(chars.len());
                        let start = start.min(end);
                        return Ok(Value::Str(chars[start..end].iter().collect()));
                    }
                    _ => {
                        return Err(format!(
                            "{}行目: slice() はリストまたは文字列にのみ使用できます",
                            line
                        ));
                    }
                }
            }
            "contains" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: contains() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let collection = self.eval_expr(&args[0], line)?;
                let target = self.eval_expr(&args[1], line)?;
                let result = match (&collection, &target) {
                    (Value::List(list), val) => list.contains(val),
                    (Value::Dict(map), Value::Str(key)) => map.contains_key(key),
                    (Value::Str(s), Value::Str(sub)) => s.contains(sub.as_str()),
                    _ => {
                        return Err(format!(
                            "{}行目: contains() はリスト・辞書・文字列にのみ使用できます",
                            line
                        ));
                    }
                };
                return Ok(Value::Bool(result));
            }
            "split" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: split() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let s = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: split() の第1引数は文字列である必要があります",
                            line
                        ));
                    }
                };
                let sep = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: split() の第2引数は文字列である必要があります",
                            line
                        ));
                    }
                };
                let parts: Vec<Value> = s.split(&sep).map(|p| Value::Str(p.to_string())).collect();
                return Ok(Value::List(parts));
            }
            "join" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: join() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let list = match self.eval_expr(&args[0], line)? {
                    Value::List(v) => v,
                    _ => {
                        return Err(format!(
                            "{}行目: join() の第1引数はリストである必要があります",
                            line
                        ));
                    }
                };
                let sep = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: join() の第2引数は文字列である必要があります",
                            line
                        ));
                    }
                };
                let parts: Vec<String> = list.iter().map(|v| v.to_string()).collect();
                return Ok(Value::Str(parts.join(&sep)));
            }
            "to_int" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: to_int() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let val = self.eval_expr(&args[0], line)?;
                let result = match &val {
                    Value::Int(n) => *n,
                    Value::Float(f) => *f as i64,
                    Value::Str(s) => s
                        .parse::<i64>()
                        .map_err(|_| format!("{}行目: to_int() 変換失敗: \"{}\"", line, s))?,
                    Value::Bool(b) => {
                        if *b {
                            1
                        } else {
                            0
                        }
                    }
                    _ => {
                        return Err(format!(
                            "{}行目: to_int() で変換できません: {:?}",
                            line, val
                        ));
                    }
                };
                return Ok(Value::Int(result));
            }
            "to_str" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: to_str() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let val = self.eval_expr(&args[0], line)?;
                return Ok(Value::Str(val.to_string()));
            }
            "range" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: range() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let start = match self.eval_expr(&args[0], line)? {
                    Value::Int(n) => n,
                    _ => {
                        return Err(format!(
                            "{}行目: range() の引数は整数である必要があります",
                            line
                        ));
                    }
                };
                let end = match self.eval_expr(&args[1], line)? {
                    Value::Int(n) => n,
                    _ => {
                        return Err(format!(
                            "{}行目: range() の引数は整数である必要があります",
                            line
                        ));
                    }
                };
                let list: Vec<Value> = (start..end).map(Value::Int).collect();
                return Ok(Value::List(list));
            }
            "read_file" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: read_file() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: read_file() の引数は文字列である必要があります",
                            line
                        ));
                    }
                };
                return Ok(match fs::read_to_string(&path) {
                    Ok(content) => Value::Str(content),
                    Err(_) => Value::Null,
                });
            }
            "read_lines" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: read_lines() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: read_lines() の引数は文字列である必要があります",
                            line
                        ));
                    }
                };
                return Ok(match fs::read_to_string(&path) {
                    Ok(content) => {
                        let lines: Vec<Value> =
                            content.lines().map(|l| Value::Str(l.to_string())).collect();
                        Value::List(lines)
                    }
                    Err(_) => Value::Null,
                });
            }
            "write_file" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: write_file() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: write_file() の第1引数は文字列である必要があります",
                            line
                        ));
                    }
                };
                let content = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    other => other.to_string(),
                };
                return Ok(Value::Bool(fs::write(&path, &content).is_ok()));
            }
            "append_file" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: append_file() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    ));
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: append_file() の第1引数は文字列である必要があります",
                            line
                        ));
                    }
                };
                let content = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    other => other.to_string(),
                };
                use std::fs::OpenOptions;
                use std::io::Write;
                let result = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .and_then(|mut f| f.write_all(content.as_bytes()));
                return Ok(Value::Bool(result.is_ok()));
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

    #[test]
    fn elif_basic() {
        run_program(
            "let x = 5\nif x == 1\n  print(1)\nelif x == 5\n  print(5)\nelse\n  print(0)\nend",
        )
        .unwrap();
    }

    #[test]
    fn elif_multiple() {
        run_program("let x = 3\nif x == 1\n  print(1)\nelif x == 2\n  print(2)\nelif x == 3\n  print(3)\nelse\n  print(0)\nend").unwrap();
    }

    #[test]
    fn elif_no_else() {
        run_program("let x = 2\nif x == 1\n  print(1)\nelif x == 2\n  print(2)\nend").unwrap();
    }

    #[test]
    fn builtin_pop() {
        run_program("let xs = [1, 2, 3]\nlet v = pop(xs)\nprint(v)\nprint(len(xs))").unwrap();
    }

    #[test]
    fn builtin_pop_empty_error() {
        let result = run_program("let xs = []\npop(xs)");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("空のリスト"));
    }

    #[test]
    fn builtin_slice_list() {
        run_program("let xs = [1, 2, 3, 4]\nlet s = slice(xs, 1, 3)\nprint(len(s))").unwrap();
    }

    #[test]
    fn builtin_slice_string() {
        run_program("let s = slice(\"hello\", 0, 3)\nprint(s)").unwrap();
    }

    #[test]
    fn builtin_contains_list() {
        run_program("print(contains([1, 2, 3], 2))").unwrap();
    }

    #[test]
    fn builtin_contains_string() {
        run_program("print(contains(\"hello\", \"ell\"))").unwrap();
    }

    #[test]
    fn builtin_contains_dict() {
        run_program("print(contains({\"a\": 1}, \"a\"))").unwrap();
    }

    #[test]
    fn builtin_split() {
        run_program("let parts = split(\"a,b,c\", \",\")\nprint(len(parts))").unwrap();
    }

    #[test]
    fn builtin_join() {
        run_program("let s = join([\"a\", \"b\"], \"-\")\nprint(s)").unwrap();
    }

    #[test]
    fn builtin_to_int() {
        run_program("print(to_int(\"42\"))\nprint(to_int(3.7))").unwrap();
    }

    #[test]
    fn builtin_to_int_error() {
        let result = run_program("to_int(\"abc\")");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("変換失敗"));
    }

    #[test]
    fn builtin_to_str() {
        run_program("let s = to_str(42)\nprint(s)").unwrap();
    }

    #[test]
    fn builtin_range() {
        run_program("let xs = range(0, 5)\nprint(len(xs))").unwrap();
    }

    #[test]
    fn builtin_range_in_for() {
        run_program("for i in range(1, 4)\n  print(i)\nend").unwrap();
    }

    #[test]
    fn builtin_write_and_read_file() {
        run_program(
            "write_file(\"/tmp/tsumugi_unit_test.txt\", \"hello\")\nlet c = read_file(\"/tmp/tsumugi_unit_test.txt\")\nprint(c)",
        )
        .unwrap();
        // cleanup
        std::fs::remove_file("/tmp/tsumugi_unit_test.txt").ok();
    }

    #[test]
    fn builtin_read_lines() {
        run_program(
            "write_file(\"/tmp/tsumugi_lines_test.txt\", \"a\\nb\\nc\")\nlet lines = read_lines(\"/tmp/tsumugi_lines_test.txt\")\nprint(len(lines))",
        )
        .unwrap();
        std::fs::remove_file("/tmp/tsumugi_lines_test.txt").ok();
    }

    #[test]
    fn builtin_append_file() {
        run_program(
            "write_file(\"/tmp/tsumugi_append_test.txt\", \"a\")\nappend_file(\"/tmp/tsumugi_append_test.txt\", \"b\")\nlet c = read_file(\"/tmp/tsumugi_append_test.txt\")\nprint(c)",
        )
        .unwrap();
        std::fs::remove_file("/tmp/tsumugi_append_test.txt").ok();
    }

    #[test]
    fn builtin_read_file_missing() {
        run_program("let x = read_file(\"/tmp/no_such_file_xyz.txt\")\nprint(x)").unwrap();
    }
}
