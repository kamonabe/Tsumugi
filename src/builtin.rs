//! 組み込み関数の実装
//!
//! eval.rs から分離した組み込み関数群。
//! コンテキスト依存のビルトイン（push/pop/map/filter/each/print/input/exit/args）のみ残し、
//! 残りは builtin_core モジュールに委譲する。

use crate::ast::Expr;
use crate::budget::ExecutionPhase;
use crate::error::TsumugiError;
use crate::value::Value;

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
            "print" | "input" | "args" | "exit" | "env" | "now" => {
                self.builtin_io(name, args, line)
            }

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
                self.apply_pure_core_builtin(name, evaluated, line)
            }

            _ => Ok(None),
        }
    }

    /// 評価済みの引数から PureCore builtin の結果を作る（request 課金 → dispatch → response 課金 →
    /// `track_result`）。同期経路（`eval_builtin`）と式 frame（tree-expr-continuation-e3）で共有する。
    pub(super) fn apply_pure_core_builtin(
        &mut self,
        name: &str,
        evaluated: Vec<Value>,
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        // filesystem host call の count + request bytes（書き込み内容）を、境界へ
        // 入る（副作用が始まる）前に課金する（§6.1、REV-015 Slice 2）。read 系は
        // request 0 byte。
        let is_host_call = crate::builtin_core::is_host_call_builtin(name);
        // request byte 数は課金と、Phase 6 の host-call lifecycle 監査（HostCallStarted /
        // request payload）で共有する。非 host-call は 0。
        let request_bytes_for_audit = if is_host_call {
            let request_bytes = crate::builtin_core::host_call_request_bytes(name, &evaluated);
            self.budget
                .charge_host_call_request(request_bytes, ExecutionPhase::Run)
                .map_err(|stop| self.control_stop_to_error(stop, line))?;
            request_bytes
        } else {
            0
        };
        let max_collection = self.budget.max_collection_elements();
        let language_revision = self.language_revision();
        // REV-001: to_str / join は値を表示して文字列化するため、budget-aware な
        // render_display 経由（fuel + per-item byte 上限）で有限化する。budget を
        // 持てない builtin_core::dispatch へ入れず、境界で先に intercept する。
        let result = if name == "to_str" {
            Some(crate::builtin_core::builtin_to_str_budgeted(
                &evaluated,
                &mut self.budget,
                line,
            )?)
        } else if name == "join" {
            Some(crate::builtin_core::builtin_join_budgeted(
                &evaluated,
                &mut self.budget,
                line,
            )?)
        } else if name == "contains" {
            Some(crate::builtin_core::builtin_contains_budgeted(
                &evaluated,
                &mut self.budget,
                line,
            )?)
        } else if name == "sort" {
            Some(crate::builtin_core::builtin_sort_budgeted(
                &evaluated,
                &mut self.budget,
                language_revision,
                line,
            )?)
        } else if crate::builtin_core::is_filesystem_builtin(name) {
            // C10: filesystem builtin は grant 有無に関わらず常に capability 経路を通す。
            // 未 grant（filesystem() が None）なら dispatch_fs が adapter/OS call 0 で
            // 単一の sandbox denial を返す。ambient std::fs fallback へは落ちない。
            let dispatched_fs = crate::builtin_core::dispatch_fs(
                name,
                &evaluated,
                self.capabilities().filesystem(),
                max_collection,
                language_revision,
                line,
            );
            // Phase 6 R-1: dispatch_fs が返す canonical label（Deny は None）を triple へ渡す。
            let fs_label = dispatched_fs.label;
            let fs_result = dispatched_fs.outcome;
            // Phase 6（§7.2/§8）: filesystem は唯一の tree host-call 境界なので、ここで
            // host-call lifecycle triple（HostCallStarted → CapabilityDecision →
            // HostCallFinished）を監査バッファへ積む。buffer は監査付き facade が drain
            // するときだけ emission され、sink 無し経路は drain しない（opt-in、byte 不変）。
            // 既存の制御フロー（成功時 response byte 課金・error 伝播）は triple を積んだ後
            // も厳密に保つため、`fs_result` を `?` で畳む前に triple を構築・push する。
            if is_host_call {
                let response_bytes = match &fs_result {
                    Ok(value) => crate::builtin_core::host_call_response_bytes(name, value),
                    Err(_) => 0,
                };
                // Deny の original_bytes 用: 先頭の Str 引数（requested path）の UTF-8 byte
                // 長。path 文字列が無い（引数型エラー等）なら 0（§9.1 / R-1 スライス）。
                let requested_path_bytes = evaluated
                    .first()
                    .and_then(|v| match v {
                        crate::value::Value::Str(s) => Some(s.as_str().len() as u64),
                        _ => None,
                    })
                    .unwrap_or(0);
                let op_id = self.alloc_host_call_operation_id(line)?;
                let bodies = crate::builtin_core::host_call_body_values(
                    name,
                    &evaluated,
                    &fs_result,
                    request_bytes_for_audit,
                    response_bytes,
                    self.redaction_policy(),
                );
                let triple = crate::builtin_core::host_call_triple_with_bodies(
                    op_id,
                    name,
                    request_bytes_for_audit,
                    &fs_result,
                    response_bytes,
                    self.redaction_policy(),
                    fs_label.as_deref(),
                    requested_path_bytes,
                    bodies.request,
                    bodies.response,
                );
                self.push_host_call_events(triple);
            }
            // ここで初めて error を伝播する（triple は deny/host-error でも push 済み）。
            Some(fs_result?)
        } else {
            crate::builtin_core::dispatch(name, &evaluated, max_collection, line)?
        };
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

    /// `print` の引数 1 つを描画する（同期経路と式 frame で共有、tree-expr-continuation-e3）。
    pub(super) fn render_print_part(
        &mut self,
        val: &Value,
        line: usize,
    ) -> Result<String, TsumugiError> {
        // budget-aware に描画する（REV-001）。共有 DAG を to_string で指数サイズに
        // materialize する前に、各複合ノードの fuel と per-item byte 上限で打ち切る。
        self.budget
            .render_display(val, ExecutionPhase::Run)
            .map_err(|stop| self.control_stop_to_error(stop, line))
    }

    /// 描画済みの部分から `print` を完了する（output 課金 → Stdout authority → write）。
    pub(super) fn apply_print(
        &mut self,
        parts: &[String],
        line: usize,
    ) -> Result<Value, TsumugiError> {
        let payload = parts.join(" ");
        // output（stdio host call）を課金する（§6.1、REV-015 Slice 2、I-O accounting）。
        // payload を host へ渡す前に課金する。
        self.budget
            .charge_output(payload.len() as u64, ExecutionPhase::Run)
            .map_err(|stop| self.control_stop_to_error(stop, line))?;
        // C4（CAP-AT-07）: Stdout authority を consult してから adapter へ write する。
        // 未 grant は adapter call 0 の catch 可能な `capability` error。
        crate::builtin_core::resolve_print(self.capabilities().stdout(), &payload, line)?;
        Ok(Value::Null)
    }

    /// 評価済みの key から `env` を完了する（型検査 → Environment authority → `track_result`）。
    pub(super) fn apply_env(&mut self, key: Value, line: usize) -> Result<Value, TsumugiError> {
        let key = match key {
            Value::Str(key) => key,
            other => {
                return Err(TsumugiError::builtin_arg_type(
                    line, "env", 1, "Str", &other,
                ));
            }
        };
        let value = crate::builtin_core::resolve_env(
            self.capabilities().environment(),
            key.as_str(),
            line,
        )?;
        // env が生成した untracked String body を tracked 化しつつ heap 課金する
        // （PureCore builtin と同じ dispatch 境界の扱い、REV-015 案A / Slice 2）。
        self.budget
            .track_result(value, ExecutionPhase::Run)
            .map_err(|stop| self.control_stop_to_error(stop, line))
    }

    /// 評価済みの code から `exit` を完了する。成功時も catch 不可の `ProcessExit` 信号を `Err` で返す。
    pub(super) fn apply_exit(
        &mut self,
        code: Option<Value>,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        let code = match code {
            None => None,
            Some(Value::Int(n)) => Some(n),
            Some(other) => {
                return Err(TsumugiError::builtin_arg_type(
                    line, "exit", 1, "Int", &other,
                ));
            }
        };
        let has_exit = self.capabilities().process_exit().is_some();
        let code = crate::builtin_core::resolve_exit(code, has_exit, line)?;
        // record_exit は pending_exit を載せ、catch 不可の ProcessExit 信号を返す。
        Err(self.record_exit(code, line))
    }

    /// `push` の代入先 binding の cell を解決する（第 2 引数の評価より前。未定義名のエラーはここ）。
    pub(super) fn push_target_cell(
        &self,
        target: &Expr,
        line: usize,
    ) -> Result<crate::value::SharedValue, TsumugiError> {
        let Expr::Ident(var_name) = target else {
            unreachable!("push target was validated before argument evaluation")
        };
        self.env
            .get_cell(var_name)
            .ok_or_else(|| TsumugiError::undefined_name(line, var_name))
    }

    /// 解決済みの cell へ評価済みの値を push する（型・上限の検査 → journal → delta 課金付き push）。
    pub(super) fn apply_push(
        &mut self,
        cell: &crate::value::SharedValue,
        value: Value,
        line: usize,
    ) -> Result<Value, TsumugiError> {
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
            .journal_cell(cell)
            .map_err(|stop| self.control_stop_to_error(stop, line))?;
        // tracked backing へ delta 課金付きで push する（REV-015 案A）。
        self.budget_list_push(cell, value, line)?;
        Ok(Value::Null)
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
                    parts.push(self.render_print_part(&val, line)?);
                }
                self.apply_print(&parts, line).map(Some)
            }
            "input" => {
                if !args.is_empty() {
                    return Err(TsumugiError::builtin_arity(line, "input", 0, args.len()));
                }
                // C4（CAP-AT-08）: Stdin authority を consult する。未 grant は adapter call 0 の
                // catch 可能な `capability` error。EOF は null、host 失敗は catch 可能な `host` error。
                let value = crate::builtin_core::resolve_input(self.capabilities().stdin(), line)?;
                // input（stdio host call）を課金する（§6.1、REV-015 Slice 2）。受け取った
                // payload の byte 長を count と併せて課金する（EOF 時は 0 byte）。
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
            "env" => {
                // C3（CAP-AT-05）: 引数（arity 1・Str）を評価してから Environment authority を
                // consult する共通ロジックへ渡す。ambient read はせず snapshot だけを引く。
                crate::builtin_core::check_arity_count(name, args.len(), 1, line)?;
                let key = self.eval_expr(&args[0], line)?;
                self.apply_env(key, line).map(Some)
            }
            "now" => {
                // C3（CAP-AT-06）: Clock authority を consult する。system clock へは触れない。
                crate::builtin_core::check_arity_count(name, args.len(), 0, line)?;
                let value = crate::builtin_core::resolve_now(self.capabilities().clock(), line)?;
                Ok(Some(value))
            }
            "exit" => {
                if args.len() > 1 {
                    return Err(TsumugiError::runtime_with_message_id(
                        line,
                        crate::error::ErrorKind::Argument,
                        crate::stable_error::message_id::ARGUMENT_EXIT_ARITY,
                        format!("exit() は引数0〜1個ですが、{}個渡されました", args.len()),
                    ));
                }
                // C7（REV-023）: プロセスを終了せず structured terminal（Exited）へ写す。
                // 引数を評価し（型検査は Int 要求）、範囲・ProcessExit authority を共通ロジックで
                // 検証してから terminal 信号を返す。
                let code = if args.is_empty() {
                    None
                } else {
                    Some(self.eval_expr(&args[0], line)?)
                };
                self.apply_exit(code, line).map(Some)
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
                let cell = self.push_target_cell(&args[0], line)?;
                // 第2引数を評価してから、同じbindingへin-placeで push する。
                let value = self.eval_expr(&args[1], line)?;
                self.apply_push(&cell, value, line).map(Some)
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
                Value::check_elements_depth(result.iter(), line)?;
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
