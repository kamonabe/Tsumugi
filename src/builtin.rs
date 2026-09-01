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
        // 再帰制限チェック（通常の関数呼び出しと同じガードを適用）
        self.count_step(line)?;
        if self.call_stack.len() >= super::MAX_USER_CALL_DEPTH {
            return Err(TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::StackOverflow,
                format!(
                    "スタックオーバーフロー: 再帰が深すぎます (上限: {})",
                    super::MAX_USER_CALL_DEPTH
                ),
            ));
        }
        match func {
            Value::Fn { def, captured } => {
                // Rcを複製して以降の借用から切り離す（値の複製は起きない）
                let def = std::rc::Rc::clone(def);
                let captured = std::rc::Rc::clone(captured);
                let name = def.name.as_str();
                let params = &def.params;

                if arg_values.len() != params.len() {
                    return Err(TsumugiError::runtime(
                        line,
                        format!(
                            "関数 {} は引数{}個ですが、{}個渡されました",
                            name,
                            params.len(),
                            arg_values.len()
                        ),
                    ));
                }
                let saved_scopes = self.env.push_call_frame();
                for (k, cell) in captured.iter() {
                    self.env.set_shared(k, cell.clone());
                }
                // 通常callと同じく、名前付き関数を宣言名へself-bindする。
                if name != "<lambda>" {
                    self.env.set(name, func.clone());
                }
                // parameterはself-bindingと同名ならshadowする。
                for (param, val) in params.iter().zip(arg_values) {
                    self.env.set(param, val);
                }

                use crate::error::TraceFrame;
                self.call_stack.push(TraceFrame {
                    name: name.to_string(),
                    line,
                });

                let mut result = Value::Null;
                for stmt in &def.body {
                    match self.exec_stmt(stmt) {
                        Ok(super::EvalResult::Return(v)) => {
                            result = v;
                            break;
                        }
                        Ok(super::EvalResult::Break) => {
                            let mut trace = self.call_stack.clone();
                            trace.reverse();
                            self.call_stack.pop();
                            self.env.pop_call_frame(saved_scopes);
                            return Err(TsumugiError::runtime(
                                stmt.line(),
                                "break はループの中でのみ使用できます",
                            )
                            .with_trace(trace));
                        }
                        Ok(super::EvalResult::Continue) => {
                            let mut trace = self.call_stack.clone();
                            trace.reverse();
                            self.call_stack.pop();
                            self.env.pop_call_frame(saved_scopes);
                            return Err(TsumugiError::runtime(
                                stmt.line(),
                                "continue はループの中でのみ使用できます",
                            )
                            .with_trace(trace));
                        }
                        Ok(super::EvalResult::Val) => {}
                        Err(e) => {
                            let mut trace = self.call_stack.clone();
                            trace.reverse();
                            self.call_stack.pop();
                            self.env.pop_call_frame(saved_scopes);
                            return Err(e.with_trace(trace));
                        }
                    }
                }
                self.call_stack.pop();
                self.env.pop_call_frame(saved_scopes);
                Ok(result)
            }
            _ => Err(TsumugiError::runtime(
                line,
                format!("関数ではない値を呼び出そうとしました: {}", func),
            )),
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
        crate::builtin_core::validate_context_builtin_call(
            name,
            args.len(),
            matches!(args.first(), Some(Expr::Ident(_))),
            line,
        )?;

        match name {
            // --- コンテキスト依存（ツリーウォーク固有の実装が必要） ---
            "print" | "input" | "args" | "exit" => self.builtin_io(name, args, line),

            // push/pop はツリーウォークでは変数を直接変更するため固有実装
            "push" | "pop" => self.builtin_collection(name, args, line),

            // map/filter/each はクロージャ呼び出しが必要
            "map" | "filter" | "each" => self.builtin_collection(name, args, line),

            // --- 共通モジュールに委譲可能なビルトイン ---
            // 引数を評価してから builtin_core::dispatch に委譲
            // len(識別子) はコレクションを複製せず長さだけ読む（AUD-041）
            "len"
                if args.len() == 1
                    && matches!(args.first(), Some(Expr::Ident(name)) if self.env.get_cell(name).is_some()) =>
            {
                let Some(Expr::Ident(name)) = args.first() else {
                    return Ok(None);
                };
                let Some(cell) = self.env.get_cell(name) else {
                    return Ok(None);
                };
                let collection = cell.borrow();
                crate::builtin_core::builtin_len(std::slice::from_ref(&collection), line).map(Some)
            }

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
                crate::builtin_core::write_stdout_line(&parts.join(" "), line)?;
                Ok(Some(Value::Null))
            }
            "input" => {
                if !args.is_empty() {
                    return Err(TsumugiError::runtime(
                        line,
                        format!("input() は引数0個ですが、{}個渡されました", args.len()),
                    ));
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
                    return Err(TsumugiError::runtime(
                        line,
                        format!("args() は引数0個ですが、{}個渡されました", args.len()),
                    ));
                }
                // 非UTF-8のargvでもpanicさせない（AUD-035）
                let argv: Vec<Value> = std::env::args_os()
                    .skip(2)
                    .map(|arg| Value::Str(arg.to_string_lossy().into_owned()))
                    .collect();
                crate::builtin_core::check_collection_size_public(argv.len(), line)?;
                Ok(Some(Value::List(argv)))
            }
            "exit" => {
                if args.len() > 1 {
                    return Err(TsumugiError::runtime(
                        line,
                        format!("exit() は引数0〜1個ですが、{}個渡されました", args.len()),
                    ));
                }
                let code = if args.is_empty() {
                    0
                } else {
                    match self.eval_expr(&args[0], line)? {
                        Value::Int(n) => n as i32,
                        _ => {
                            return Err(TsumugiError::runtime(
                                line,
                                "exit() の引数は整数である必要があります",
                            ));
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
                let Expr::Ident(var_name) = &args[0] else {
                    unreachable!("push target was validated before argument evaluation")
                };
                let cell = self.env.get_cell(var_name).ok_or_else(|| {
                    TsumugiError::runtime(line, format!("未定義の変数: {}", var_name))
                })?;

                // 第1引数を先にsnapshotし、第2引数の評価後に同じbindingへ書き戻す。
                let target_value = cell.borrow().clone();
                let value = self.eval_expr(&args[1], line)?;
                let updated = crate::builtin_core::builtin_push(&[target_value, value], line)?;
                *cell.borrow_mut() = updated;
                Ok(Some(Value::Null))
            }
            "pop" => {
                let Expr::Ident(var_name) = &args[0] else {
                    unreachable!("pop target was validated before argument evaluation")
                };
                let cell = self.env.get_cell(var_name).ok_or_else(|| {
                    TsumugiError::runtime(line, format!("未定義の変数: {}", var_name))
                })?;
                let target_value = cell.borrow().clone();
                let value =
                    crate::builtin_core::builtin_pop(std::slice::from_ref(&target_value), line)?;
                let updated = crate::builtin_core::builtin_pop_update(&[target_value], line)?;
                *cell.borrow_mut() = updated;
                Ok(Some(value))
            }
            "map" => {
                let list_value = self.eval_expr(&args[0], line)?;
                let func = self.eval_expr(&args[1], line)?;
                let list = match list_value {
                    Value::List(v) => v,
                    _ => {
                        return Err(crate::builtin_core::type_error(
                            line,
                            "map(list, fn) の形式で使います",
                        ));
                    }
                };
                let mut result = Vec::new();
                for item in list {
                    let val = self.call_fn_value(&func, vec![item], line)?;
                    crate::builtin_core::check_collection_size_public(
                        result.len().saturating_add(1),
                        line,
                    )?;
                    result.push(val);
                }
                Ok(Some(Value::List(result)))
            }
            "filter" => {
                let list_value = self.eval_expr(&args[0], line)?;
                let func = self.eval_expr(&args[1], line)?;
                let list = match list_value {
                    Value::List(v) => v,
                    _ => {
                        return Err(crate::builtin_core::type_error(
                            line,
                            "filter(list, fn) の形式で使います",
                        ));
                    }
                };
                let mut result = Vec::new();
                for item in list {
                    let val = self.call_fn_value(&func, vec![item.clone()], line)?;
                    if val.is_truthy() {
                        crate::builtin_core::check_collection_size_public(
                            result.len().saturating_add(1),
                            line,
                        )?;
                        result.push(item);
                    }
                }
                Ok(Some(Value::List(result)))
            }
            "each" => {
                let list_value = self.eval_expr(&args[0], line)?;
                let func = self.eval_expr(&args[1], line)?;
                let list = match list_value {
                    Value::List(v) => v,
                    _ => {
                        return Err(crate::builtin_core::type_error(
                            line,
                            "each(list, fn) の形式で使います",
                        ));
                    }
                };
                for item in list {
                    self.call_fn_value(&func, vec![item], line)?;
                }
                Ok(Some(Value::Null))
            }
            _ => Ok(None),
        }
    }
}
