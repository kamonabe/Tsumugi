//! 組み込み関数の実装
//!
//! eval.rs から分離した組み込み関数群。
//! コンテキスト依存のビルトイン（push/pop/map/filter/each/print/input/exit/args）のみ残し、
//! 残りは builtin_core モジュールに委譲する。

use crate::ast::Expr;
use crate::budget::ExecutionPhase;
use crate::error::TsumugiError;
use crate::value::Value;

use std::io::{self, BufRead};

use super::Evaluator;

impl Evaluator {
    /// Value::Fn を呼び出すヘルパー（map/filter/each 用）
    fn call_fn_value(
        &mut self,
        builtin: &str,
        func: &Value,
        arg_values: Vec<Value>,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        // 再帰制限チェック（通常の関数呼び出しと同じガードを適用）
        self.count_step(line)?;
        if self.call_stack.len() >= super::MAX_USER_CALL_DEPTH {
            return Err(TsumugiError::call_depth_limit(
                line,
                super::MAX_USER_CALL_DEPTH,
            ));
        }
        match func {
            Value::Fn { def, captured, .. } => {
                // Rcを複製して以降の借用から切り離す（値の複製は起きない）
                let def = std::rc::Rc::clone(def);
                let captured = std::rc::Rc::clone(captured);
                let name = def.name.as_str();
                let params = &def.params;

                // callbackは常に1引数で呼ぶため、arity不一致はcallback専用messageで報告する。
                if arg_values.len() != params.len() {
                    return Err(TsumugiError::callback_arity(line, builtin, params.len()));
                }
                let saved_scopes = self.env.push_call_frame();
                for (k, cell) in captured.iter() {
                    // journal entry の課金超過でも call frame を解放してから返す（REV-015 PR-d）。
                    if let Err(e) = self.env.set_shared(k, cell.clone()) {
                        self.env.pop_call_frame(saved_scopes);
                        return Err(self.control_stop_to_error(e, line));
                    }
                }
                // 通常callと同じく、名前付き関数を宣言名へself-bindする。
                // cell 課金の超過でも call frame を解放してから返す（REV-015 PR-c）。
                if name != "<lambda>"
                    && let Err(e) = self.env.set(name, func.clone())
                {
                    self.env.pop_call_frame(saved_scopes);
                    return Err(self.control_stop_to_error(e, line));
                }
                // parameterはself-bindingと同名ならshadowする。
                for (param, val) in params.iter().zip(arg_values) {
                    if let Err(e) = self.env.set(param, val) {
                        self.env.pop_call_frame(saved_scopes);
                        return Err(self.control_stop_to_error(e, line));
                    }
                }

                use crate::error::TraceFrame;
                self.call_stack.push(TraceFrame {
                    name: name.to_string(),
                    line,
                });

                // callback 本体も通常の関数呼び出しと同じく明示 Call frame として driver で
                // 実行する（REV-015 Slice 3 PR-c）。スコープ退避（`saved_scopes`）と call trace
                // の巻き戻しは全終了経路で Call frame の pop_frame が行い、エラー時のトレース
                // 付加は driver の attach_trace が担うため、ここでは終了後に env /
                // trace を触らない。ループ外 break/continue はその文の行番号でエラー化する。
                match self.drive_call_body(def, saved_scopes) {
                    Ok(super::EvalResult::Return(v)) => Ok(v),
                    Ok(super::EvalResult::Val) => Ok(Value::Null),
                    Ok(super::EvalResult::Break(err_line)) => {
                        Err(TsumugiError::break_outside_loop(err_line))
                    }
                    Ok(super::EvalResult::Continue(err_line)) => {
                        Err(TsumugiError::continue_outside_loop(err_line))
                    }
                    Err(e) => Err(e),
                }
            }
            _ => Err(TsumugiError::callback_not_callable(line, builtin, func)),
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

            // --- PureCore builtin は引数を評価してから builtin_core::dispatch へ委譲 ---
            // 名前一覧は単一の BuiltinSpec registry から導出する（AUD-049）。
            // ここへ手書きの名前列挙を復活させない。
            _ if crate::builtin_registry::is_pure_core_builtin(name) => {
                let mut evaluated = Vec::with_capacity(args.len());
                for arg in args {
                    evaluated.push(self.eval_expr(arg, line)?);
                }
                // filesystem host call の count + request bytes（書き込み内容）を、境界へ
                // 入る（副作用が始まる）前に課金する（§6.1、REV-015 Slice 2）。read 系は
                // request 0 byte。
                let is_host_call = crate::builtin_core::is_host_call_builtin(name);
                if is_host_call {
                    let request_bytes =
                        crate::builtin_core::host_call_request_bytes(name, &evaluated);
                    self.budget
                        .charge_host_call_request(request_bytes, ExecutionPhase::Run)
                        .map_err(|stop| self.control_stop_to_error(stop, line))?;
                }
                let max_collection = self.budget.max_collection_elements();
                let result = crate::builtin_core::dispatch(name, &evaluated, max_collection, line)?;
                match result {
                    Some(value) => {
                        // filesystem host call の response bytes（読み込み内容）を、結果が
                        // 確定した後に課金する（§6.1）。write 系は response 0 byte。
                        if is_host_call {
                            let response_bytes =
                                crate::builtin_core::host_call_response_bytes(name, &value);
                            self.budget
                                .charge_host_response_bytes(response_bytes, ExecutionPhase::Run)
                                .map_err(|stop| self.control_stop_to_error(stop, line))?;
                        }
                        // builtin が生成した untracked collection を tracked 化しつつ heap
                        // 課金し、新規 String body も課金する（REV-015 案A / Slice 2）。
                        let tracked = self
                            .budget
                            .track_result(value, ExecutionPhase::Run)
                            .map_err(|stop| self.control_stop_to_error(stop, line))?;
                        Ok(Some(tracked))
                    }
                    None => Ok(None),
                }
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
                let payload = parts.join(" ");
                // output（stdio host call）を課金する（§6.1、REV-015 Slice 2、I-O accounting）。
                // payload を host へ渡す前に課金する。
                self.budget
                    .charge_output(payload.len() as u64, ExecutionPhase::Run)
                    .map_err(|stop| self.control_stop_to_error(stop, line))?;
                crate::builtin_core::write_stdout_line(&payload, line)?;
                Ok(Some(Value::Null))
            }
            "input" => {
                if !args.is_empty() {
                    return Err(TsumugiError::builtin_arity(line, "input", 0, args.len()));
                }
                let stdin = io::stdin();
                let mut line_buf = String::new();
                let value = match stdin.lock().read_line(&mut line_buf) {
                    Ok(0) => Value::Null,
                    Ok(_) => {
                        if line_buf.ends_with('\n') {
                            line_buf.pop();
                            if line_buf.ends_with('\r') {
                                line_buf.pop();
                            }
                        }
                        Value::str_constant(line_buf)
                    }
                    Err(_) => Value::Null,
                };
                // input（stdio host call）を課金する（§6.1、REV-015 Slice 2）。受け取った
                // payload の byte 長を count と併せて課金する（EOF / error 時は 0 byte）。
                let payload_bytes = match &value {
                    Value::Str(s) => s.len() as u64,
                    _ => 0,
                };
                self.budget
                    .charge_input(payload_bytes, ExecutionPhase::Run)
                    .map_err(|stop| self.control_stop_to_error(stop, line))?;
                Ok(Some(value))
            }
            "args" => {
                if !args.is_empty() {
                    return Err(TsumugiError::builtin_arity(line, "args", 0, args.len()));
                }
                // process argv ではなく実行 context の snapshot を返す（AUD-018）
                let argv: Vec<Value> = self
                    .script_args()
                    .iter()
                    .map(|arg| Value::str_constant(arg.clone()))
                    .collect();
                self.check_collection(argv.len(), line)?;
                let listed = Value::new_list(argv, &mut self.budget, ExecutionPhase::Run)
                    .map_err(|stop| self.control_stop_to_error(stop, line))?;
                Ok(Some(listed))
            }
            "exit" => {
                if args.len() > 1 {
                    return Err(TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Argument,
                        format!("exit() は引数0〜1個ですが、{}個渡されました", args.len()),
                    ));
                }
                // C7（REV-023）: プロセスを終了せず structured terminal（Exited）へ写す。
                // 引数を評価し（型検査は Int 要求）、範囲・ProcessExit authority を共通ロジックで
                // 検証してから terminal 信号を返す。
                let code = if args.is_empty() {
                    None
                } else {
                    match self.eval_expr(&args[0], line)? {
                        Value::Int(n) => Some(n),
                        other => {
                            return Err(TsumugiError::builtin_arg_type(
                                line, "exit", 1, "Int", &other,
                            ));
                        }
                    }
                };
                let has_exit = self.capabilities().process_exit().is_some();
                let code = crate::builtin_core::resolve_exit(code, has_exit, line)?;
                // record_exit は pending_exit を載せ、catch 不可の ProcessExit 信号を返す。
                Err(self.record_exit(code, line))
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
                let cell = self
                    .env
                    .get_cell(var_name)
                    .ok_or_else(|| TsumugiError::undefined_name(line, var_name))?;

                // 第2引数を評価してから、同じbindingへin-placeで push する。
                let value = self.eval_expr(&args[1], line)?;
                // 型・上限を検査する（値は借用せず長さだけ見る）。
                {
                    let target = cell.borrow();
                    let Value::List(v) = &*target else {
                        return Err(TsumugiError::builtin_arg_type(
                            line, "push", 1, "List", &target,
                        ));
                    };
                    self.check_collection(v.len().saturating_add(1), line)?;
                }
                // 書き戻し前に元値を記録する（AUD-024）。
                self.env
                    .journal_cell(&cell)
                    .map_err(|stop| self.control_stop_to_error(stop, line))?;
                // tracked backing へ delta 課金付きで push する（REV-015 案A）。
                self.budget_list_push(&cell, value, line)?;
                Ok(Some(Value::Null))
            }
            "pop" => {
                let Expr::Ident(var_name) = &args[0] else {
                    unreachable!("pop target was validated before argument evaluation")
                };
                let cell = self
                    .env
                    .get_cell(var_name)
                    .ok_or_else(|| TsumugiError::undefined_name(line, var_name))?;
                // 末尾要素を取り出す（型検査と空チェックを含む）。
                let popped = {
                    let target = cell.borrow();
                    let Value::List(v) = &*target else {
                        return Err(TsumugiError::builtin_arg_type(
                            line, "pop", 1, "List", &target,
                        ));
                    };
                    match v.last() {
                        Some(last) => last.clone(),
                        None => return Err(TsumugiError::pop_empty_list(line)),
                    }
                };
                // 書き戻し前に元値を記録する（AUD-024）。
                self.env
                    .journal_cell(&cell)
                    .map_err(|stop| self.control_stop_to_error(stop, line))?;
                // tracked backing から delta release 付きで末尾を除く（REV-015 案A）。
                self.budget_list_pop(&cell, line)?;
                Ok(Some(popped))
            }
            "map" => {
                let list_value = self.eval_expr(&args[0], line)?;
                let func = self.eval_expr(&args[1], line)?;
                let list = match list_value {
                    Value::List(v) => v,
                    other => {
                        return Err(TsumugiError::builtin_arg_type(
                            line, "map", 1, "List", &other,
                        ));
                    }
                };
                let mut result = Vec::new();
                for item in list.iter().cloned() {
                    let val = self.call_fn_value("map", &func, vec![item], line)?;
                    self.check_collection(result.len().saturating_add(1), line)?;
                    result.push(val);
                }
                let listed = Value::new_list(result, &mut self.budget, ExecutionPhase::Run)
                    .map_err(|stop| self.control_stop_to_error(stop, line))?;
                Ok(Some(listed))
            }
            "filter" => {
                let list_value = self.eval_expr(&args[0], line)?;
                let func = self.eval_expr(&args[1], line)?;
                let list = match list_value {
                    Value::List(v) => v,
                    other => {
                        return Err(TsumugiError::builtin_arg_type(
                            line, "filter", 1, "List", &other,
                        ));
                    }
                };
                let mut result = Vec::new();
                for item in list.iter().cloned() {
                    let val = self.call_fn_value("filter", &func, vec![item.clone()], line)?;
                    if val.is_truthy() {
                        self.check_collection(result.len().saturating_add(1), line)?;
                        result.push(item);
                    }
                }
                let listed = Value::new_list(result, &mut self.budget, ExecutionPhase::Run)
                    .map_err(|stop| self.control_stop_to_error(stop, line))?;
                Ok(Some(listed))
            }
            "each" => {
                let list_value = self.eval_expr(&args[0], line)?;
                let func = self.eval_expr(&args[1], line)?;
                let list = match list_value {
                    Value::List(v) => v,
                    other => {
                        return Err(TsumugiError::builtin_arg_type(
                            line, "each", 1, "List", &other,
                        ));
                    }
                };
                for item in list.iter().cloned() {
                    self.call_fn_value("each", &func, vec![item], line)?;
                }
                Ok(Some(Value::Null))
            }
            _ => Ok(None),
        }
    }
}
