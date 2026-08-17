//! 組み込み関数の実装
//!
//! eval.rs から分離した組み込み関数群。
//! コンテキスト依存のビルトイン（push/pop/map/filter/each/print/input/exit/args）のみ残し、
//! 残りは builtin_core モジュールに委譲する。

use crate::ast::Expr;
use crate::error::TsumugiError;
use crate::value::Value;

use std::io::{self, BufRead};

use super::Evaluator;

impl Evaluator {
    /// Value::Fn を呼び出すヘルパー（map/filter/each 用）
    fn call_fn_value(
        &mut self,
        func: &Value,
        arg_values: Vec<Value>,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        match func {
            Value::Fn {
                name,
                params,
                body,
                captured,
            } => {
                if arg_values.len() != params.len() {
                    return Err(format!(
                        "{}行目: 関数 {} は引数{}個ですが、{}個渡されました",
                        line,
                        name,
                        params.len(),
                        arg_values.len()
                    )
                    .into());
                }
                self.env.push_scope();
                for (k, v) in captured {
                    self.env.set(k, v.clone());
                }
                for (param, val) in params.iter().zip(arg_values) {
                    self.env.set(param, val);
                }

                use crate::error::TraceFrame;
                self.call_stack.push(TraceFrame {
                    name: name.clone(),
                    line,
                });

                let mut result = Value::Null;
                for stmt in body {
                    match self.exec_stmt(stmt) {
                        Ok(super::EvalResult::Return(v)) => {
                            result = v;
                            break;
                        }
                        Ok(super::EvalResult::Break | super::EvalResult::Continue) => break,
                        Ok(super::EvalResult::Val) => {}
                        Err(e) => {
                            let mut trace = self.call_stack.clone();
                            trace.reverse();
                            self.call_stack.pop();
                            self.env.pop_scope();
                            return Err(e.with_trace(trace));
                        }
                    }
                }
                self.call_stack.pop();
                self.env.pop_scope();
                Ok(result)
            }
            _ => Err(format!(
                "{}行目: 関数ではない値を呼び出そうとしました: {}",
                line, func
            )
            .into()),
        }
    }

    /// 組み込み関数を評価する。
    /// 該当する組み込み関数があれば Ok(Some(value))、なければ Ok(None)、エラーなら Err を返す。
    pub(crate) fn eval_builtin(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        match name {
            // --- コンテキスト依存（ツリーウォーク固有の実装が必要） ---
            "print" | "input" | "args" | "exit" => self.builtin_io(name, args, line),

            // push/pop はツリーウォークでは変数を直接変更するため固有実装
            "push" | "pop" => self.builtin_collection(name, args, line),

            // map/filter/each はクロージャ呼び出しが必要
            "map" | "filter" | "each" => self.builtin_collection(name, args, line),

            // --- 共通モジュールに委譲可能なビルトイン ---
            // 引数を評価してから builtin_core::dispatch に委譲
            "len" | "keys" | "values" | "has_key" | "type" | "slice" | "contains" | "sort"
            | "reverse" | "range" | "split" | "join" | "trim" | "starts_with" | "ends_with"
            | "replace" | "upper" | "lower" | "to_int" | "to_str" | "to_float" | "abs" | "min"
            | "max" | "floor" | "ceil" | "round" | "now" | "format_time" | "env" | "read_file"
            | "read_lines" | "write_file" | "append_file" | "path_exists" | "path_join"
            | "mkdir" | "remove" | "remove_dir" | "rename" | "list_dir" | "file_size"
            | "is_file" | "is_dir" => {
                let mut evaluated = Vec::with_capacity(args.len());
                for arg in args {
                    evaluated.push(self.eval_expr(arg, line)?);
                }
                crate::builtin_core::dispatch(name, &evaluated, line)
            }

            _ => Ok(None),
        }
    }

    // =========================================================================
    // I/O・環境系: print, input, env, args, exit
    // =========================================================================

    fn builtin_io(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        match name {
            "print" => {
                let mut parts = Vec::new();
                for arg in args {
                    let val = self.eval_expr(arg, line)?;
                    parts.push(val.to_string());
                }
                println!("{}", parts.join(" "));
                Ok(Some(Value::Null))
            }
            "input" => {
                if !args.is_empty() {
                    return Err(format!(
                        "{}行目: input() は引数0個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let stdin = io::stdin();
                let mut line_buf = String::new();
                Ok(Some(match stdin.lock().read_line(&mut line_buf) {
                    Ok(0) => Value::Null,
                    Ok(_) => {
                        if line_buf.ends_with('\n') {
                            line_buf.pop();
                            if line_buf.ends_with('\r') {
                                line_buf.pop();
                            }
                        }
                        Value::Str(line_buf)
                    }
                    Err(_) => Value::Null,
                }))
            }
            "args" => {
                if !args.is_empty() {
                    return Err(format!(
                        "{}行目: args() は引数0個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let argv: Vec<Value> = std::env::args().skip(2).map(Value::Str).collect();
                Ok(Some(Value::List(argv)))
            }
            "exit" => {
                if args.len() > 1 {
                    return Err(format!(
                        "{}行目: exit() は引数0〜1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let code = if args.is_empty() {
                    0
                } else {
                    match self.eval_expr(&args[0], line)? {
                        Value::Int(n) => n as i32,
                        _ => {
                            return Err(format!(
                                "{}行目: exit() の引数は整数である必要があります",
                                line
                            )
                            .into());
                        }
                    }
                };
                std::process::exit(code);
            }
            _ => Ok(None),
        }
    }

    // =========================================================================
    // コレクション操作系（ツリーウォーク固有: push, pop, map, filter, each）
    // =========================================================================

    fn builtin_collection(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        match name {
            "push" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: push() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let var_name = match &args[0] {
                    Expr::Ident(name) => name.clone(),
                    _ => {
                        return Err(format!(
                            "{}行目: push() の第1引数はリスト変数である必要があります",
                            line
                        )
                        .into());
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
                        return Err(
                            format!("{}行目: push() はリストにのみ使用できます", line).into()
                        );
                    }
                }
                Ok(Some(Value::Null))
            }
            "pop" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: pop() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let var_name = match &args[0] {
                    Expr::Ident(name) => name.clone(),
                    _ => {
                        return Err(format!(
                            "{}行目: pop() の引数はリスト変数である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let target = self
                    .env
                    .get_mut(&var_name)
                    .ok_or_else(|| format!("{}行目: 未定義の変数: {}", line, var_name))?;
                match target {
                    Value::List(list) => {
                        if list.is_empty() {
                            return Err(
                                format!("{}行目: 空のリストから pop できません", line).into()
                            );
                        }
                        let val = list.pop().unwrap();
                        Ok(Some(val))
                    }
                    _ => Err(format!("{}行目: pop() はリストにのみ使用できます", line).into()),
                }
            }
            "map" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: map() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let list = match self.eval_expr(&args[0], line)? {
                    Value::List(v) => v,
                    _ => {
                        return Err(format!(
                            "{}行目: map() の第1引数はリストである必要があります",
                            line
                        )
                        .into());
                    }
                };
                let func = self.eval_expr(&args[1], line)?;
                let mut result = Vec::new();
                for item in list {
                    let val = self.call_fn_value(&func, vec![item], line)?;
                    result.push(val);
                }
                Ok(Some(Value::List(result)))
            }
            "filter" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: filter() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let list = match self.eval_expr(&args[0], line)? {
                    Value::List(v) => v,
                    _ => {
                        return Err(format!(
                            "{}行目: filter() の第1引数はリストである必要があります",
                            line
                        )
                        .into());
                    }
                };
                let func = self.eval_expr(&args[1], line)?;
                let mut result = Vec::new();
                for item in list {
                    let val = self.call_fn_value(&func, vec![item.clone()], line)?;
                    if val.is_truthy() {
                        result.push(item);
                    }
                }
                Ok(Some(Value::List(result)))
            }
            "each" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: each() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let list = match self.eval_expr(&args[0], line)? {
                    Value::List(v) => v,
                    _ => {
                        return Err(format!(
                            "{}行目: each() の第1引数はリストである必要があります",
                            line
                        )
                        .into());
                    }
                };
                let func = self.eval_expr(&args[1], line)?;
                for item in list {
                    self.call_fn_value(&func, vec![item], line)?;
                }
                Ok(Some(Value::Null))
            }
            _ => Ok(None),
        }
    }
}
