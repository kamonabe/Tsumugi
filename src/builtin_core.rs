//! 組み込み関数の共通ロジック
//!
//! ツリーウォーク評価器 (builtin.rs) と VM (vm.rs) の両方から呼び出される。
//! 引数は評価済みの `&[Value]` で受け取るため、引数評価の順序と副作用は各engineの責務。
//!
//! `builtin_*` の多くは副作用のない値変換で、一部は filesystem に触る（`read_file` /
//! `write_file` / `mkdir` / `remove` / `rename` / `list_dir` など）。これらは
//! `sandbox` の認可を通すロジックも両engineで共有したいため、ここに置く。
//!
//! 各engine側に残すのは、実行コンテキスト（capability set・stdio・argv・変数 binding・
//! closure）そのものを必要とするものだけ:
//! - `print` / `input` / `exit` / `args` — process の stdio・argv・終了
//! - `env` / `now` — Environment / Clock capability を consult する（Phase 2 C3）
//! - `push` / `pop` — 変数bindingへの書き戻し
//! - `map` / `filter` / `each` — クロージャ呼び出し
//!
//! `env` / `now` の型・arity 検査後の解決ロジックは [`resolve_env`] / [`resolve_now`] に
//! 共有として置き、capability set は各 engine が [`crate::capability::CapabilitySet`] から
//! 渡す（`exit` の [`resolve_exit`] と同じ形）。
//!
//! language-visible builtin 名の正本は [`crate::builtin_registry`] の
//! `PUBLIC_BUILTINS` 1か所だけである（AUD-049）。本モジュールの `dispatch` は
//! PureCore builtin の handler 正本であり、名前の可否判定は registry から導出する。
//! 内部命令 `__pop_update` は public registry へ置かず、Compiler の pop lowering
//! 専用 opcode として実装するため source から到達できない。

use crate::error::TsumugiError;
use crate::value::{IndexTarget, NumericOrder, NumericOrdering, Tracked, Value};

// collection を返す純粋 builtin は untracked backing（`Tracked::constant`）で生成し、
// 呼び出し側（eval/vm）の `track_result` が dispatch 境界で課金・tracked 化する。
// in-place mutation（index 代入・push/pop）は ledger を受け取り、tracked mutation
// primitive で delta 課金する（REV-015 案A、§5.2）。

// =============================================================================
// コレクションサイズ上限（メモリ DoS 対策 / REV-015 Slice 1）
// =============================================================================
//
// 上限値の正本は各 engine が保持する [`crate::budget::BudgetLedger`] の
// `max_collection_elements` である。共有 builtin handler（両 engine から dispatch
// される）は engine の ledger へ直接触れないため、上限値を引数で受け取る。
// これにより、旧実装の process-global な `OnceLock` を廃止し、上限を execution
// 単位の config へ一本化する（execution-control.md §13）。

/// コレクションサイズが上限を超えていないかチェックする（上限は呼び出し側が渡す）。
fn check_collection_size(size: usize, limit: u64, line: usize) -> Result<(), TsumugiError> {
    if size as u64 > limit {
        return Err(TsumugiError::collection_limit(line, size, limit as usize));
    }
    Ok(())
}

// =============================================================================
// ユーティリティ
// =============================================================================

pub fn check_arity_count(
    name: &str,
    actual: usize,
    expected: usize,
    line: usize,
) -> Result<(), TsumugiError> {
    if actual != expected {
        Err(TsumugiError::builtin_arity(line, name, expected, actual))
    } else {
        Ok(())
    }
}

pub fn check_arity(
    name: &str,
    args: &[Value],
    expected: usize,
    line: usize,
) -> Result<(), TsumugiError> {
    check_arity_count(name, args.len(), expected, line)
}

pub fn is_context_builtin(name: &str) -> bool {
    // 名前一覧は単一の BuiltinSpec registry から導出する（AUD-049）。
    // `print` は Compiler が予約 token として直接 lowering するため、context
    // builtin だが本 registry 判定の対象にはならない（ValidateBuiltinCall を出さない）。
    crate::builtin_registry::is_context_builtin(name) && !matches!(name, "print")
}

pub fn validate_context_builtin_call(
    name: &str,
    arg_count: usize,
    first_arg_is_identifier: bool,
    line: usize,
) -> Result<(), TsumugiError> {
    match name {
        "input" | "args" => check_arity_count(name, arg_count, 0, line),
        "exit" => {
            if arg_count > 1 {
                Err(TsumugiError::runtime_with_message_id(
                    line,
                    crate::error::ErrorKind::Argument,
                    crate::stable_error::message_id::ARGUMENT_EXIT_ARITY,
                    format!("exit() は引数0〜1個ですが、{}個渡されました", arg_count),
                ))
            } else {
                Ok(())
            }
        }
        "push" | "pop" => {
            let expected = if name == "push" { 2 } else { 1 };
            check_arity_count(name, arg_count, expected, line)?;
            if first_arg_is_identifier {
                Ok(())
            } else {
                Err(TsumugiError::mutation_target_not_variable(line, name))
            }
        }
        "map" | "filter" | "each" => check_arity_count(name, arg_count, 2, line),
        _ => Ok(()),
    }
}

/// `exit(code)` の code を検証する共通ロジック（Phase 2 C7、REV-023）。tree/VM 両 engine が使う。
///
/// - arity（0〜1）は呼び出し側が既に検査済みで、ここは評価済み code を受け取る。
/// - `code` が `0..=255` の範囲外なら catch 可能な `argument` エラー（第3.4節「`exit` code範囲外」）。
/// - `has_process_exit` が false（ProcessExit 未 grant）なら catch 可能な `capability` エラー
///   （第3.4節「host function capability拒否」、`{name}` = `exit`）。OS/process へは触れない。
/// - 両方通れば `Ok(u8)` を返す。呼び出し側が `Evaluator`/`Vm` の `record_exit` で terminal 信号へ写す。
///
/// `code` の `None` は引数なし呼び出し（`exit()`）で、終了コード 0 を意味する。
pub fn resolve_exit(
    code: Option<i64>,
    has_process_exit: bool,
    line: usize,
) -> Result<u8, TsumugiError> {
    let code = code.unwrap_or(0);
    // 範囲検査を capability 検査より先に行う（第3.7節 error precedence: 引数・型を authority
    // より先に確定する。malformed 引数と authority 不足の channel を混在させない）。
    let code = u8::try_from(code).map_err(|_| TsumugiError::exit_code_out_of_range(line, code))?;
    if !has_process_exit {
        return Err(TsumugiError::capability_denied(line, "exit"));
    }
    Ok(code)
}

/// builtin 引数が Str であることを要求し、そうでなければ canonical な引数型エラーを返す。
fn require_str<'a>(
    value: &'a Value,
    builtin: &str,
    position: usize,
    line: usize,
) -> Result<&'a String, TsumugiError> {
    match value {
        Value::Str(s) => Ok(s),
        other => Err(TsumugiError::builtin_arg_type(
            line, builtin, position, "Str", other,
        )),
    }
}

// =============================================================================
// コレクション操作系
// =============================================================================

pub fn builtin_len(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("len", args, 1, line)?;
    match &args[0] {
        Value::List(v) => Ok(Value::Int(v.len() as i64)),
        Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
        Value::Dict(m) => Ok(Value::Int(m.len() as i64)),
        other => Err(TsumugiError::builtin_arg_type(
            line,
            "len",
            1,
            "List/Str/Dict",
            other,
        )),
    }
}

/// `target[index] = value` を対象コレクションへ in-place で適用する。
///
/// ツリーウォーク評価器と VM の双方がこの関数だけを使うため、境界判定・
/// コレクション上限・エラーメッセージは両エンジンで同一になる。
/// 呼び出し側は index と value を評価し終えた後に呼ぶこと（規範評価順）。
pub fn assign_index(
    target: &mut Value,
    index: &Value,
    value: Value,
    max_collection: u64,
    budget: &mut crate::budget::BudgetLedger,
    line: usize,
) -> Result<(), TsumugiError> {
    // ターゲット種別ごとに index/key と上限を検査し、正規化した IndexTarget を作る。
    // 実際の書き込みと heap の delta 課金は tracked mutation primitive に集約する
    // （REV-015 案A、§5.2）。両エンジンが同じ経路を通り parity を保つ。
    let normalized = match target {
        Value::List(list) => {
            let i = match index {
                Value::Int(n) => *n,
                other => {
                    return Err(TsumugiError::list_index_type(line, other));
                }
            };
            let len = list.len() as i64;
            let actual_idx = if i < 0 { len + i } else { i };
            if actual_idx < 0 || actual_idx >= len {
                return Err(TsumugiError::list_index_out_of_range(line, i, list.len()));
            }
            IndexTarget::ListIndex(actual_idx as usize)
        }
        Value::Dict(map) => {
            let key = match index {
                Value::Str(s) => s.to_string(),
                other => {
                    return Err(TsumugiError::dict_key_type(line, other));
                }
            };
            if !map.contains_key(&key) {
                check_collection_size(map.len().saturating_add(1), max_collection, line)?;
            }
            IndexTarget::DictKey(key)
        }
        other => return Err(TsumugiError::index_assign_unsupported(line, other)),
    };
    target
        .index_set_tracked(
            normalized,
            value,
            budget,
            crate::budget::ExecutionPhase::Run,
        )
        .map_err(|stop| crate::budget::control_stop_to_error(stop, budget.live_heap_bytes(), line))
}

pub fn builtin_push(
    args: &[Value],
    max_collection: u64,
    line: usize,
) -> Result<Value, TsumugiError> {
    check_arity("push", args, 2, line)?;
    if let Value::List(v) = &args[0] {
        check_collection_size(v.len().saturating_add(1), max_collection, line)?;
        // COW: backing を複製して 1 要素追加し、untracked で包む。heap 課金は
        // 呼び出し側の `track_result` で行う（REV-015 案A）。
        let mut data: Vec<Value> = (**v).clone();
        data.push(args[1].clone());
        Ok(Value::List(Tracked::constant(data)))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line, "push", 1, "List", &args[0],
        ))
    }
}

pub fn builtin_pop(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("pop", args, 1, line)?;
    if let Value::List(v) = &args[0] {
        // pop は末尾要素（値）を返す。list 本体の書き戻しは __pop_update が行う。
        match v.last() {
            Some(last) => Ok(last.clone()),
            None => Err(TsumugiError::pop_empty_list(line)),
        }
    } else {
        Err(TsumugiError::builtin_arg_type(
            line, "pop", 1, "List", &args[0],
        ))
    }
}

pub fn builtin_pop_update(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("__pop_update", args, 1, line)?;
    if let Value::List(v) = &args[0] {
        if v.is_empty() {
            return Ok(args[0].clone());
        }
        // COW: backing を複製して末尾を除き、untracked で包む。heap 課金は
        // 呼び出し側の `track_result` で行う（REV-015 案A）。
        let mut data: Vec<Value> = (**v).clone();
        data.pop();
        Ok(Value::List(Tracked::constant(data)))
    } else {
        Ok(args[0].clone())
    }
}

pub fn builtin_keys(
    args: &[Value],
    max_collection: u64,
    line: usize,
) -> Result<Value, TsumugiError> {
    check_arity("keys", args, 1, line)?;
    if let Value::Dict(map) = &args[0] {
        check_collection_size(map.len(), max_collection, line)?;
        let keys: Vec<Value> = map.keys().map(|k| Value::str_constant(k.clone())).collect();
        Ok(Value::List(Tracked::constant(keys)))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line, "keys", 1, "Dict", &args[0],
        ))
    }
}

pub fn builtin_values(
    args: &[Value],
    max_collection: u64,
    line: usize,
) -> Result<Value, TsumugiError> {
    check_arity("values", args, 1, line)?;
    if let Value::Dict(map) = &args[0] {
        check_collection_size(map.len(), max_collection, line)?;
        let vals: Vec<Value> = map.values().cloned().collect();
        Ok(Value::List(Tracked::constant(vals)))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line, "values", 1, "Dict", &args[0],
        ))
    }
}

pub fn builtin_has_key(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("has_key", args, 2, line)?;
    match (&args[0], &args[1]) {
        (Value::Dict(map), Value::Str(key)) => Ok(Value::Bool(map.contains_key(key.as_str()))),
        (Value::Dict(_), other) => Err(TsumugiError::builtin_arg_type(
            line, "has_key", 2, "Str", other,
        )),
        (other, _) => Err(TsumugiError::builtin_arg_type(
            line, "has_key", 1, "Dict", other,
        )),
    }
}

pub fn builtin_type(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("type", args, 1, line)?;
    let t = match &args[0] {
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "str",
        Value::Bool(_) => "bool",
        Value::Null => "null",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        Value::Fn { .. } | Value::VmFn { .. } => "fn",
        Value::Error { .. } => "error",
    };
    Ok(Value::str_from(t))
}

pub fn builtin_slice(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("slice", args, 3, line)?;
    let start = match &args[1] {
        Value::Int(n) => n,
        other => {
            return Err(TsumugiError::builtin_arg_type(
                line, "slice", 2, "Int", other,
            ));
        }
    };
    let end = match &args[2] {
        Value::Int(n) => n,
        other => {
            return Err(TsumugiError::builtin_arg_type(
                line, "slice", 3, "Int", other,
            ));
        }
    };
    // 負数は 0 にクランプ
    let start = if *start < 0 { 0usize } else { *start as usize };
    let end = if *end < 0 { 0usize } else { *end as usize };
    match &args[0] {
        Value::List(v) => {
            let s = start.min(v.len());
            let e = end.min(v.len());
            // start > end の場合は空リストを返す（パニックしない）
            if s > e {
                return Ok(Value::List(Tracked::constant(Vec::new())));
            }
            Ok(Value::List(Tracked::constant(v[s..e].to_vec())))
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let st = start.min(chars.len());
            let en = end.min(chars.len());
            // start > end の場合は空文字列を返す（パニックしない）
            if st > en {
                return Ok(Value::str_constant(String::new()));
            }
            Ok(Value::str_constant(
                chars[st..en].iter().collect::<String>(),
            ))
        }
        other => Err(TsumugiError::builtin_arg_type(
            line, "slice", 1, "List/Str", other,
        )),
    }
}

pub fn builtin_contains(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("contains", args, 2, line)?;
    match &args[0] {
        Value::List(v) => Ok(Value::Bool(v.contains(&args[1]))),
        Value::Str(s) => {
            if let Value::Str(sub) = &args[1] {
                Ok(Value::Bool(s.contains(sub.as_str())))
            } else {
                Ok(Value::Bool(false))
            }
        }
        Value::Dict(map) => {
            if let Value::Str(key) = &args[1] {
                Ok(Value::Bool(map.contains_key(key.as_str())))
            } else {
                Ok(Value::Bool(false))
            }
        }
        other => Err(TsumugiError::builtin_arg_type(
            line,
            "contains",
            1,
            "List/Str/Dict",
            other,
        )),
    }
}

/// `sort` の budget-aware 版（REV-001 / REV-017）。
///
/// revision 0.21 以降（§17.8）は全値型を覆う単一の全順序 [`crate::value::TotalOrder`] で
/// 安定ソートする。描画文字列キーを作らず、値どうしを直接比較する（`sort([3, 10, 2])` が
/// 数値順の `[2, 3, 10]` になる）。per-element 走査課金は charge table の `1 / element`
/// （execution-control.md）を維持するため明示的に行う。文字列描画をやめることで `render_display`
/// の per-item byte 課金が sort 経路から外れ、同じ入力でも fuel 消費が 0.20 より減る
/// （§17.8.5 / §17.8.7 の観測差。charge table の値は不変）。
///
/// revision 0.20 以下は従来どおり **文字列表現比較**（`str` の Unicode コードポイント順）を
/// 維持する。各要素を `BudgetLedger::render_display` で描画し、共有 DAG の指数時間／出力を
/// fuel + per-item byte 上限で有限化してから `sort_by_key(to_string)` 相当の安定ソートを行う。
///
/// どちらの revision でも tree/VM の dispatch 境界が同じこの関数を呼び、backend 固有分岐を
/// 作らない。安定性（同順位要素は元順序を保つ）は両 revision で維持する。
pub fn builtin_sort_budgeted(
    args: &[Value],
    budget: &mut crate::budget::BudgetLedger,
    revision: crate::embedding::LanguageRevision,
    line: usize,
) -> Result<Value, TsumugiError> {
    check_arity("sort", args, 1, line)?;
    let Value::List(list) = &args[0] else {
        return Err(TsumugiError::builtin_arg_type(
            line, "sort", 1, "List", &args[0],
        ));
    };

    if revision >= crate::embedding::LanguageRevision::V0_21 {
        // REV-017: 全順序 TotalOrder で値どうしを直接比較する（描画キーを作らない）。
        // 比較ごとに budget 課金する `compare_budgeted` を使うことで、per-element 走査課金
        // （charge table の 1 / element、値は不変）を比較走査へ適用する。これにより共有 DAG
        // 要素を含む List の比較も走査課金が有限に積み上がり budget 超過で停止する（§17.8.5）。
        //
        // `sort_by` の comparator は失敗を返せないため、最初に起きた `ControlStop` を捕捉して
        // 以降は `Equal` を返し、ソート後に取り出して error へ写す。安定ソート（sort_by は
        // stable）で同順位要素は元順序を保つ。
        use std::cmp::Ordering;
        let phase = crate::budget::ExecutionPhase::Run;
        // まず要素数ぶんの走査課金を明示的に行う（§17.8.5 の 1 / element）。render_display が
        // 担っていた per-node fuel の代替で、要素数 n に対して有限に積む（単調増加）。
        for _ in list.iter() {
            budget
                .charge_fuel(1, phase)
                .map_err(|stop| crate::budget::control_stop_to_error(stop, 0, line))?;
        }
        let mut stop: Option<crate::budget::ControlStop> = None;
        let mut sorted: Vec<Value> = (**list).clone();
        sorted.sort_by(|a, b| {
            if stop.is_some() {
                return Ordering::Equal;
            }
            match crate::value::TotalOrder::compare_budgeted(a, b, budget, phase) {
                Ok(ord) => ord,
                Err(s) => {
                    stop = Some(s);
                    Ordering::Equal
                }
            }
        });
        if let Some(s) = stop {
            return Err(crate::budget::control_stop_to_error(s, 0, line));
        }
        return Ok(Value::List(Tracked::constant(sorted)));
    }

    // revision <= 0.20: 従来の文字列表現比較を温存する。
    // 各要素の描画キーを budget-aware に先に生成する（materialize 前に打ち切る）。
    let mut keyed: Vec<(String, Value)> = Vec::with_capacity(list.len());
    for v in list.iter() {
        let key = budget
            .render_display(v, crate::budget::ExecutionPhase::Run)
            .map_err(|stop| crate::budget::control_stop_to_error(stop, 0, line))?;
        keyed.push((key, v.clone()));
    }
    // 安定ソート（sort_by は stable）。キーが同じ要素は元の順序を保ち、
    // sort_by_key(to_string) と完全に同一の並びになる。
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let sorted: Vec<Value> = keyed.into_iter().map(|(_, v)| v).collect();
    Ok(Value::List(Tracked::constant(sorted)))
}

pub fn builtin_reverse(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("reverse", args, 1, line)?;
    match &args[0] {
        Value::List(list) => {
            let mut rev = (**list).clone();
            rev.reverse();
            Ok(Value::List(Tracked::constant(rev)))
        }
        Value::Str(s) => Ok(Value::str_constant(s.chars().rev().collect::<String>())),
        other => Err(TsumugiError::builtin_arg_type(
            line, "reverse", 1, "List/Str", other,
        )),
    }
}

pub fn builtin_range(
    args: &[Value],
    max_collection: u64,
    line: usize,
) -> Result<Value, TsumugiError> {
    check_arity("range", args, 2, line)?;
    let start = match &args[0] {
        Value::Int(n) => *n,
        other => {
            return Err(TsumugiError::builtin_arg_type(
                line, "range", 1, "Int", other,
            ));
        }
    };
    let end = match &args[1] {
        Value::Int(n) => *n,
        other => {
            return Err(TsumugiError::builtin_arg_type(
                line, "range", 2, "Int", other,
            ));
        }
    };
    let size = if end > start {
        // checked_sub でオーバーフローを防ぐ
        let diff = end
            .checked_sub(start)
            .ok_or_else(|| TsumugiError::int_overflow(line, "range の範囲が大きすぎます"))?;
        diff as usize
    } else {
        0
    };
    check_collection_size(size, max_collection, line)?;
    let list: Vec<Value> = (start..end).map(Value::Int).collect();
    Ok(Value::List(Tracked::constant(list)))
}

// =============================================================================
// 文字列操作系
// =============================================================================

pub fn builtin_split(
    args: &[Value],
    max_collection: u64,
    line: usize,
) -> Result<Value, TsumugiError> {
    check_arity("split", args, 2, line)?;
    let s = require_str(&args[0], "split", 1, line)?;
    let sep = require_str(&args[1], "split", 2, line)?;
    let mut parts = Vec::new();
    for part in s.split(sep.as_str()) {
        check_collection_size(parts.len().saturating_add(1), max_collection, line)?;
        parts.push(Value::str_constant(part.to_string()));
    }
    Ok(Value::List(Tracked::constant(parts)))
}

/// `join` の budget-aware 版（REV-001）。要素を `to_string` で materialize する代わりに
/// `BudgetLedger::render_display` で描画し、共有 DAG の指数時間／出力を fuel + per-item
/// byte 上限で有限化する。tree/VM の dispatch 境界が budget を渡して呼ぶ。
pub fn builtin_join_budgeted(
    args: &[Value],
    budget: &mut crate::budget::BudgetLedger,
    line: usize,
) -> Result<Value, TsumugiError> {
    check_arity("join", args, 2, line)?;
    let Value::List(list) = &args[0] else {
        return Err(TsumugiError::builtin_arg_type(
            line, "join", 1, "List", &args[0],
        ));
    };
    let sep = require_str(&args[1], "join", 2, line)?;
    let mut parts: Vec<String> = Vec::with_capacity(list.len());
    for v in list.iter() {
        let rendered = budget
            .render_display(v, crate::budget::ExecutionPhase::Run)
            .map_err(|stop| crate::budget::control_stop_to_error(stop, 0, line))?;
        parts.push(rendered);
    }
    Ok(Value::str_constant(parts.join(sep)))
}

/// `to_str` の budget-aware 版（REV-001）。`to_string` の代わりに
/// `BudgetLedger::render_display` で描画し、共有 DAG の指数時間／出力を有限化する。
pub fn builtin_to_str_budgeted(
    args: &[Value],
    budget: &mut crate::budget::BudgetLedger,
    line: usize,
) -> Result<Value, TsumugiError> {
    check_arity("to_str", args, 1, line)?;
    let rendered = budget
        .render_display(&args[0], crate::budget::ExecutionPhase::Run)
        .map_err(|stop| crate::budget::control_stop_to_error(stop, 0, line))?;
    Ok(Value::str_constant(rendered))
}

pub fn builtin_trim(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("trim", args, 1, line)?;
    let s = require_str(&args[0], "trim", 1, line)?;
    Ok(Value::str_constant(s.trim().to_string()))
}

pub fn builtin_upper(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("upper", args, 1, line)?;
    let s = require_str(&args[0], "upper", 1, line)?;
    Ok(Value::str_constant(s.to_uppercase()))
}

pub fn builtin_lower(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("lower", args, 1, line)?;
    let s = require_str(&args[0], "lower", 1, line)?;
    Ok(Value::str_constant(s.to_lowercase()))
}

pub fn builtin_starts_with(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("starts_with", args, 2, line)?;
    let s = require_str(&args[0], "starts_with", 1, line)?;
    let prefix = require_str(&args[1], "starts_with", 2, line)?;
    Ok(Value::Bool(s.starts_with(prefix.as_str())))
}

pub fn builtin_ends_with(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("ends_with", args, 2, line)?;
    let s = require_str(&args[0], "ends_with", 1, line)?;
    let suffix = require_str(&args[1], "ends_with", 2, line)?;
    Ok(Value::Bool(s.ends_with(suffix.as_str())))
}

pub fn builtin_replace(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("replace", args, 3, line)?;
    let s = require_str(&args[0], "replace", 1, line)?;
    let old = require_str(&args[1], "replace", 2, line)?;
    let new = require_str(&args[2], "replace", 3, line)?;
    Ok(Value::str_constant(s.replace(old.as_str(), new.as_str())))
}

// =============================================================================
// 型変換・数値系
// =============================================================================

/// Float→Int 変換の丸めモード（AUD-036）。
#[derive(Clone, Copy)]
enum RoundMode {
    /// 0 方向へ切り捨て（`to_int`）
    TowardZero,
    /// 負の無限大方向（`floor`）
    Floor,
    /// 正の無限大方向（`ceil`）
    Ceil,
    /// 最も近い整数、中間は 0 から遠い側（`round`）
    Nearest,
}

/// finite な Float を丸めて i64 へ変換する。NaN・±Infinity・i64 範囲外は
/// `conversion` エラーにする（AUD-036）。lossy な `as i64` を使わず、丸め後の
/// 数学値が半開区間 `[-2^63, 2^63)` に入ることを検査してから変換する。
///
/// `i64::MAX as f64` は 2^63 へ丸められるため、上端 2^63 は表現できず受理しない。
/// 下端 `-2^63`（= `i64::MIN`）は f64 で正確に表現できるため受理する。
///
/// これは Float→Int 変換の検査層であり、Float の bit canonicalization
/// （[`crate::determinism::canonicalize_float`]）とは別層。入力 NaN は canonical かどうかに
/// 関わらず一律にエラーへ落とすため、挙動は本 Slice で不変。
fn checked_float_to_i64(
    value: f64,
    mode: RoundMode,
    builtin: &str,
    line: usize,
) -> Result<i64, TsumugiError> {
    if value.is_nan() {
        return Err(TsumugiError::runtime_with_message_id(
            line,
            crate::error::ErrorKind::Conversion,
            crate::stable_error::message_id::CONVERSION_FLOAT_TO_INT,
            format!("{builtin} で Int に変換できません: NaN"),
        ));
    }
    if value.is_infinite() {
        return Err(TsumugiError::runtime_with_message_id(
            line,
            crate::error::ErrorKind::Conversion,
            crate::stable_error::message_id::CONVERSION_FLOAT_TO_INT,
            format!("{builtin} で Int に変換できません: 非有限値"),
        ));
    }

    let rounded = match mode {
        RoundMode::TowardZero => value.trunc(),
        RoundMode::Floor => value.floor(),
        RoundMode::Ceil => value.ceil(),
        RoundMode::Nearest => value.round(),
    };

    // 半開区間 [-2^63, 2^63) を境界で判定する。上端 2^63 は i64 で表現できない。
    // 定数は f64 で正確に表現できる 2 の冪なので比較は厳密。
    const MIN: f64 = -9_223_372_036_854_775_808.0; // -2^63 = i64::MIN
    const LIMIT: f64 = 9_223_372_036_854_775_808.0; // 2^63（受理しない上端）
    if !(MIN..LIMIT).contains(&rounded) {
        return Err(TsumugiError::runtime_with_message_id(
            line,
            crate::error::ErrorKind::Conversion,
            crate::stable_error::message_id::CONVERSION_FLOAT_TO_INT,
            format!("{builtin} で Int に変換できません: i64 範囲外"),
        ));
    }

    Ok(rounded as i64)
}

/// OS が返す `u64` のファイルサイズを i64 へ変換する。`i64::MAX` を超える場合は
/// wrap・負値を返さず `int_overflow` エラーにする（AUD-036）。
fn checked_file_size_to_i64(size: u64, line: usize) -> Result<i64, TsumugiError> {
    i64::try_from(size).map_err(|_| {
        TsumugiError::runtime_with_message_id(
            line,
            crate::error::ErrorKind::IntOverflow,
            crate::stable_error::message_id::INT_OVERFLOW_FILE_SIZE,
            "ファイルサイズを Int で表現できません",
        )
    })
}

pub fn builtin_to_int(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("to_int", args, 1, line)?;
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(f) => Ok(Value::Int(checked_float_to_i64(
            *f,
            RoundMode::TowardZero,
            "to_int",
            line,
        )?)),
        Value::Bool(b) => Ok(Value::Int(if *b { 1 } else { 0 })),
        Value::Str(s) => s.parse::<i64>().map(Value::Int).map_err(|_| {
            TsumugiError::runtime_with_message_id(
                line,
                crate::error::ErrorKind::Conversion,
                crate::stable_error::message_id::CONVERSION_TO_INT,
                "to_int で Int に変換できません: 数値として解釈できません",
            )
        }),
        other => Err(TsumugiError::builtin_arg_type(
            line,
            "to_int",
            1,
            "Int/Float/Bool/Str",
            other,
        )),
    }
}

pub fn builtin_to_float(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("to_float", args, 1, line)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Float(crate::determinism::canonicalize_float(*f))),
        Value::Int(n) => Ok(Value::Float(crate::determinism::canonicalize_float(
            *n as f64,
        ))),
        Value::Str(s) => crate::determinism::parse_float(s)
            .map(Value::Float)
            .map_err(|_| {
                TsumugiError::runtime_with_message_id(
                    line,
                    crate::error::ErrorKind::Conversion,
                    crate::stable_error::message_id::CONVERSION_TO_FLOAT,
                    "to_float で Float に変換できません: 数値として解釈できません",
                )
            }),
        other => Err(TsumugiError::builtin_arg_type(
            line,
            "to_float",
            1,
            "Int/Float/Str",
            other,
        )),
    }
}

pub fn builtin_abs(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("abs", args, 1, line)?;
    match &args[0] {
        Value::Int(n) => n
            .checked_abs()
            .map(|v| Ok(Value::Int(v)))
            .unwrap_or_else(|| {
                Err(TsumugiError::int_overflow(
                    line,
                    "abs の結果が表現できません",
                ))
            }),
        Value::Float(f) => Ok(Value::Float(crate::determinism::canonicalize_float(
            f.abs(),
        ))),
        other => Err(TsumugiError::builtin_arg_type(
            line,
            "abs",
            1,
            "Int/Float",
            other,
        )),
    }
}

/// min / max の共通実装（REV-003）。
///
/// NumericOrder で厳密比較し、選択した operand を**元の型のまま**返す
/// （Int を渡せば Int が返る）。同値時は第 1 引数を返す。いずれかが NaN の
/// ときは canonical NaN（[`crate::determinism::canonical_nan`]）を返す。
fn builtin_min_max(
    name: &str,
    args: &[Value],
    line: usize,
    want_min: bool,
) -> Result<Value, TsumugiError> {
    check_arity(name, args, 2, line)?;
    // 型検査は第1・第2引数の順で行い、非数値を早期に拒否する。
    if !matches!(args[0], Value::Int(_) | Value::Float(_)) {
        return Err(TsumugiError::builtin_arg_type(
            line,
            name,
            1,
            "Int/Float",
            &args[0],
        ));
    }
    if !matches!(args[1], Value::Int(_) | Value::Float(_)) {
        return Err(TsumugiError::builtin_arg_type(
            line,
            name,
            2,
            "Int/Float",
            &args[1],
        ));
    }

    match NumericOrder::compare(&args[0], &args[1]) {
        // NaN が絡む場合は canonical NaN を返す。
        Some(NumericOrdering::UnorderedNaN) | None => {
            Ok(Value::Float(crate::determinism::canonical_nan()))
        }
        Some(NumericOrdering::Equal) => Ok(args[0].clone()),
        Some(NumericOrdering::Less) => {
            // args[0] < args[1]
            Ok(if want_min {
                args[0].clone()
            } else {
                args[1].clone()
            })
        }
        Some(NumericOrdering::Greater) => {
            // args[0] > args[1]
            Ok(if want_min {
                args[1].clone()
            } else {
                args[0].clone()
            })
        }
    }
}

pub fn builtin_min(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    builtin_min_max("min", args, line, true)
}

pub fn builtin_max(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    builtin_min_max("max", args, line, false)
}

pub fn builtin_floor(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("floor", args, 1, line)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Int(checked_float_to_i64(
            *f,
            RoundMode::Floor,
            "floor",
            line,
        )?)),
        Value::Int(n) => Ok(Value::Int(*n)),
        other => Err(TsumugiError::builtin_arg_type(
            line,
            "floor",
            1,
            "Int/Float",
            other,
        )),
    }
}

pub fn builtin_ceil(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("ceil", args, 1, line)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Int(checked_float_to_i64(
            *f,
            RoundMode::Ceil,
            "ceil",
            line,
        )?)),
        Value::Int(n) => Ok(Value::Int(*n)),
        other => Err(TsumugiError::builtin_arg_type(
            line,
            "ceil",
            1,
            "Int/Float",
            other,
        )),
    }
}

pub fn builtin_round(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("round", args, 1, line)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Int(checked_float_to_i64(
            *f,
            RoundMode::Nearest,
            "round",
            line,
        )?)),
        Value::Int(n) => Ok(Value::Int(*n)),
        other => Err(TsumugiError::builtin_arg_type(
            line,
            "round",
            1,
            "Int/Float",
            other,
        )),
    }
}

// =============================================================================
// 日時系
// =============================================================================

pub fn builtin_format_time(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("format_time", args, 2, line)?;
    let Value::Int(ts) = &args[0] else {
        return Err(TsumugiError::builtin_arg_type(
            line,
            "format_time",
            1,
            "Int",
            &args[0],
        ));
    };
    let fmt = require_str(&args[1], "format_time", 2, line)?;
    Ok(Value::str_constant(format_unix_timestamp(*ts, fmt)))
}

// =============================================================================
// 標準出力
// =============================================================================

// script の `print` による標準出力書き込みは C4（Phase 2）で [`resolve_print`] 経由の
// Stdout adapter（[`crate::capability::SystemOutput`] 等）へ移した。broken pipe を panic
// させず構造化エラー化する挙動（AUD-035）は adapter の `write_all` が引き継ぐ。

// =============================================================================
// ファイルI/O系（サンドボックスチェック付き）
// =============================================================================

/// この builtin が host 境界（filesystem 等）を越える host call かどうか（§6.1、
/// REV-015 Slice 2、I-O accounting）。
///
/// stdio（`print` / `input`）は各 engine が固有実装を持ち、`charge_output` /
/// `charge_input` で別途課金するためここには含めない。本 Slice では読み書きの payload
/// byte が明確な filesystem read/write builtin を host call として課金対象にする。
/// path 判定・env・clock 等その他の host 境界 builtin の課金は Phase 2 の capability /
/// host function 配線で扱う。
pub fn is_host_call_builtin(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "read_lines" | "write_file" | "append_file"
    )
}

/// 監査 host-call lifecycle triple（§7.2/§8）を emission する filesystem builtin か（Phase 6 R-1）。
///
/// [`is_host_call_builtin`]（I-O byte 課金対象）に `rename` を加えた集合。`rename` は書き込み/
/// 読み込み payload byte を持たず I-O byte 課金対象ではない（[`is_host_call_builtin`] に含めない）が、
/// capability decision を伴う filesystem 境界なので監査 triple は emission する（§9.1 / R-1 スライス・
/// 5 周目レビュー反映。`rename` は resource を source_path / destination_path の 2 field に分ける）。
/// triple の push は budget・意味論・外部効果に影響しない（監査付き facade が drain するときだけ
/// emission される）ため、`rename` を監査対象へ加えても byte 会計・I-O call count は不変。
pub fn is_audited_host_call(name: &str) -> bool {
    is_host_call_builtin(name) || name == "rename"
}

/// host call の request payload byte 長（host へ渡す内容）。§6.1、REV-015 Slice 2。
///
/// write/append は書き込む内容の byte 長、read 系は request payload を持たないため 0。
/// path 引数の byte は本 Slice では request に含めない（write 内容だけを payload とする）。
pub fn host_call_request_bytes(name: &str, args: &[Value]) -> u64 {
    match name {
        "write_file" | "append_file" => match args.get(1) {
            Some(Value::Str(s)) => s.len() as u64,
            Some(other) => other.to_string().len() as u64,
            None => 0,
        },
        _ => 0,
    }
}

/// host call の response payload byte 長（host から受け取る内容）。§6.1、REV-015 Slice 2。
///
/// read_file は読み込んだ文字列の byte 長、read_lines は各行の byte 長の合計。write /
/// append は response payload を持たないため 0（返り値の `Bool` は payload ではない）。
pub fn host_call_response_bytes(name: &str, result: &Value) -> u64 {
    match name {
        "read_file" => match result {
            Value::Str(s) => s.len() as u64,
            _ => 0,
        },
        "read_lines" => match result {
            Value::List(items) => items
                .iter()
                .map(|v| match v {
                    Value::Str(s) => s.len() as u64,
                    _ => 0,
                })
                .fold(0u64, |acc, n| acc.saturating_add(n)),
            _ => 0,
        },
        _ => 0,
    }
}

/// host call の redaction で Deny/label 不在時に使う requested path の UTF-8 byte 長（§9.1 / R-1）。
///
/// 単一 path 操作は `primary` だけを使う。`rename` は `primary` を source、`destination` を
/// destination の requested path 長に使う（2 field 化、5 周目レビュー反映）。
pub struct RequestedPathBytes {
    /// 第 1 path（単一操作の path、rename の source）の requested path 長。
    pub primary: u64,
    /// rename の destination の requested path 長（単一操作では未使用）。
    pub destination: u64,
}

/// Identifier class（capability 名・host function 名・rule ID 等）を既定 policy で畳んだ
/// `AuditField` を作る（§9.1 / ユーザー判断 A＝(イ)）。Reveal 既定では UTF-8 text を `Revealed`、
/// policy が omit/hash 設定なら `Omitted` に畳む（[`crate::audit::fold_audit_field`] が一元管理）。
fn identifier_field(
    policy: &crate::audit::RedactionPolicy,
    value: &str,
) -> crate::audit::AuditField {
    use crate::audit::{AuditFieldClass, AuditScalar, AuditValue, fold_audit_field};
    fold_audit_field(
        policy,
        AuditFieldClass::Identifier,
        value.len() as u64,
        None,
        Some(AuditValue::Scalar(AuditScalar::Utf8(value.to_string()))),
    )
}

/// filesystem host call 1 件を監査 lifecycle triple（§7.2/§8）へ分類する純関数。
///
/// 設計正本は [`docs/determinism-and-audit.md`](../docs/determinism-and-audit.md) §7.2（event
/// schema・`effect_status` 意味）/ §8（emission 順序）/ §9（redaction 既定）である。tree backend の
/// host-call 境界（[`dispatch_fs`]）が返した `result` を受け取り、`HostCallStarted` →
/// `CapabilityDecision` → `HostCallFinished` の順に 3 event を構築して返す。VM へは配線しない。
///
/// `id` は 1 host call へ発番した単調 operation id で、`operation_id` と `call_id`（fs 操作は
/// host call なので `Some`）に同値で使う（§7.2 の triple ID 再利用）。
///
/// # 分類（§7.2）
///
/// - **Deny**: `dispatch_fs` が `filesystem_denied`（`ErrorKind::Sandbox`）を返した場合、または
///   capability lookup 前の path lexical error（`ErrorKind::Argument`）。外部効果は発生していない。
///   `CapabilityDecision.decision = Deny`、`HostCallFinished { outcome: Denied, effect_status: None }`。
/// - **HostError**: adapter/OS call が失敗（`ErrorKind::Host`）。`decision = Allow`（認可は通り
///   adapter まで到達した）、`HostCallFinished.outcome = HostError`。`effect_status` は方向依存:
///     - read 系（`read_file` / `read_lines`）は mutation しないので `None`。
///     - write 系（`write_file` / `append_file`）は部分適用があり得るため `Unknown`（§7.2
///       「crash recovery で prepare だけ残る entry は Unknown 相当」の方針に合わせ、失敗時に
///       副作用が残り得る方向を Unknown とする）。
/// - **Success**: `Ok`。`decision = Allow`、`outcome = Success`。`effect_status` は方向依存:
///     - read 系は外部 mutation がないので `None`。
///     - write 系は commit 済みなので `Committed`。
///
/// # payload（§9.1 redaction・R-1a 既定 policy）
///
/// 既定 policy を [`crate::audit::fold_audit_field`] で適用して §9.1 の `AuditField` ツリーへ
/// 畳む。path は Path class なので Allow では canonical label の `HmacSha256`（生 path を漏らさ
/// ない）・Deny では `Omitted`（label の有無を問わず raw/label 非露出）。request/response body は
/// Body class で常に `Omitted`（R-1 は allow-list Reveal 未実装）。`arguments` は外側
/// `Revealed{class:Public, Sequence}`（個数と並びだけ開示）で各要素を Path/Body へ畳む。
/// `function` / `capability` / `action` / `rule_id` は Identifier class（既定 Reveal）で載せる
/// （ユーザー判断 A＝(イ)）。`rename` だけ resource を `source_path` / `destination_path` の 2
/// field に分ける。`error_code` は安定 `ErrorCode` 文字列（`ErrorKind::as_str`）で script 可視
/// message は載せない（§9 意図）。
// R-1 で redaction 用の 3 引数（policy / labels / requested_path_bytes）が加わり 8 引数になる。
// これらは §9.1 の payload 構築に不可欠で、まとめる自然な struct も無い（id/name/bytes/result は
// 呼び出し側の別経路で確定する）ため許容する。
#[allow(clippy::too_many_arguments)]
pub fn host_call_triple(
    id: u64,
    name: &str,
    request_bytes: u64,
    result: &Result<Value, TsumugiError>,
    response_bytes: u64,
    policy: &crate::audit::RedactionPolicy,
    labels: &AuditResourceLabels,
    requested_path_bytes: &RequestedPathBytes,
) -> [crate::audit::AuditEvent; 3] {
    use crate::audit::{
        AuditEvent, AuditField, AuditFieldClass, AuditPayload, AuditValue, CapabilityDecisionKind,
        EffectStatus, HostCallOutcome, fold_audit_field,
    };
    use crate::error::ErrorKind;
    use std::collections::BTreeMap;

    let is_write = matches!(name, "write_file" | "append_file");

    // decision / outcome / effect_status / error_code / rule_id を result から分類する。
    let (decision, outcome, effect_status, error_code, rule_id) = match result {
        Ok(_) => (
            CapabilityDecisionKind::Allow,
            HostCallOutcome::Success,
            if is_write {
                EffectStatus::Committed
            } else {
                EffectStatus::None
            },
            None,
            "mount-granted".to_string(),
        ),
        Err(error) => match error.kind() {
            // adapter/OS 失敗。認可は通っており（Allow）、外部効果が write 系では残り得る。
            Some(ErrorKind::Host) => (
                CapabilityDecisionKind::Allow,
                HostCallOutcome::HostError,
                if is_write {
                    EffectStatus::Unknown
                } else {
                    EffectStatus::None
                },
                Some(ErrorKind::Host.as_str().to_string()),
                "mount-granted".to_string(),
            ),
            // path lexical error（capability lookup 前）。外部効果なし＝Deny 扱い。
            Some(kind @ ErrorKind::Argument) => (
                CapabilityDecisionKind::Deny,
                HostCallOutcome::Denied,
                EffectStatus::None,
                Some(kind.as_str().to_string()),
                "invalid-path".to_string(),
            ),
            // filesystem_denied（mount 未登録・op 未 grant・sandbox 範囲外）。既定の deny。
            Some(kind) => (
                CapabilityDecisionKind::Deny,
                HostCallOutcome::Denied,
                EffectStatus::None,
                Some(kind.as_str().to_string()),
                "deny-by-default".to_string(),
            ),
            // kind なし（parse error 等）は到達しない想定だが、fail-safe に deny 扱い。
            None => (
                CapabilityDecisionKind::Deny,
                HostCallOutcome::Denied,
                EffectStatus::None,
                None,
                "deny-by-default".to_string(),
            ),
        },
    };

    // §9.1 redaction tree（既定 policy）を適用して payload を組む。畳み規則はすべて
    // `fold_audit_field` へ集約する（Secret は常に Omit、Path は HMAC mode だけ digest ほか、
    // R-1 スライス）。R-1 は body allow-list Reveal を実装しないため、Body は `body_mode` に
    // 関わらず常に Omit へ畳む（R-1b で `reveal_body_fields` を参照する Reveal 経路を有効化する）。
    let body_field = |original_bytes: u64| -> AuditField {
        fold_audit_field(policy, AuditFieldClass::Body, original_bytes, None, None)
    };
    // decision が Allow のとき Path を HMAC、Deny のとき Omitted に畳む helper。label の有無・
    // original_bytes を渡し、HMAC 入力・raw 非露出は `fold_audit_field` が一元管理する
    // （§9.1 / R-AT-01・R-AT-02）。
    let path_field = |label: Option<&str>, requested_path_bytes: u64| -> AuditField {
        match decision {
            // Allow は label 確定済み（Success/HostError 両方）。label 文字列の UTF-8 byte 長を
            // original_bytes にする。label 不在の異常時は requested path 長で代用し Omitted へ畳む。
            CapabilityDecisionKind::Allow => {
                let original_bytes = label
                    .map(|l| l.len() as u64)
                    .unwrap_or(requested_path_bytes);
                fold_audit_field(policy, AuditFieldClass::Path, original_bytes, label, None)
            }
            // Deny は label を渡さない（手元にあっても漏らさない）。original_bytes は requested
            // path 長（label 長を漏らさない）。引数型エラーで path が無いときは 0。
            CapabilityDecisionKind::Deny => fold_audit_field(
                policy,
                AuditFieldClass::Path,
                requested_path_bytes,
                None,
                None,
            ),
        }
    };

    // request payload: body（原本 byte 長）＋ arguments。
    let mut request_fields: BTreeMap<String, AuditField> = BTreeMap::new();
    request_fields.insert("body".to_string(), body_field(request_bytes));
    // arguments は §9.1 どおり外側を Revealed{class:Public, Sequence([..])} とし、開示するのは
    // 引数の個数と並びだけにする。各要素は引数の種類に応じて Path（path 引数）/ Body（書き込み
    // 内容などの自由文）へ畳むため raw path も自由文も露出しない。path の HMAC は resource 側だけが
    // 持ち arguments 側では HMAC しない（各要素 Path は label を渡さず Omitted）。各要素の
    // original_bytes はその引数の UTF-8 byte 長（R-1 の代用規則、§9.1 / R-1 スライス）。
    let mut argument_elements: Vec<AuditField> = Vec::new();
    // 第 1 引数は常に path（read/write/append/read_lines すべて arg[0] が path）。要素側は
    // label を渡さず Omitted{class:Path} に畳む（HMAC は resource 側だけ）。
    argument_elements.push(fold_audit_field(
        policy,
        AuditFieldClass::Path,
        requested_path_bytes.primary,
        None,
        None,
    ));
    // write/append は第 2 引数が書き込み内容（自由文）＝ Body。original_bytes は内容 byte 長
    // （= request_bytes）。read 系は第 2 引数を持たない。
    if is_write {
        argument_elements.push(body_field(request_bytes));
    }
    let arguments_original_bytes =
        requested_path_bytes
            .primary
            .saturating_add(if is_write { request_bytes } else { 0 });
    request_fields.insert(
        "arguments".to_string(),
        AuditField::Revealed {
            class: AuditFieldClass::Public,
            value: AuditValue::Sequence(argument_elements),
            original_bytes: arguments_original_bytes,
        },
    );
    request_fields.insert("function".to_string(), identifier_field(policy, name));
    let request_payload = AuditPayload {
        fields: request_fields,
    };

    // resource payload: path field。単一 path 操作は `path`、rename は `source_path` /
    // `destination_path` の 2 field（§9.1 / R-1 スライス・5 周目レビュー反映）。
    let mut resource_fields: BTreeMap<String, AuditField> = BTreeMap::new();
    match labels {
        AuditResourceLabels::Single(label) => {
            resource_fields.insert(
                "path".to_string(),
                path_field(label.as_deref(), requested_path_bytes.primary),
            );
        }
        AuditResourceLabels::Rename {
            source,
            destination,
        } => {
            resource_fields.insert(
                "source_path".to_string(),
                path_field(source.as_deref(), requested_path_bytes.primary),
            );
            resource_fields.insert(
                "destination_path".to_string(),
                path_field(destination.as_deref(), requested_path_bytes.destination),
            );
        }
    }
    // capability 名・rule ID は Identifier class（既定 Reveal）で payload にも載せる（§9.1 /
    // ユーザー判断 A＝(イ)）。これにより slice 文面の「既定 Reveal」と実装が一致する。
    resource_fields.insert(
        "capability".to_string(),
        identifier_field(policy, "filesystem"),
    );
    resource_fields.insert("rule_id".to_string(), identifier_field(policy, &rule_id));
    resource_fields.insert("action".to_string(), identifier_field(policy, name));
    let resource_payload = AuditPayload {
        fields: resource_fields,
    };

    // response payload: body（原本 byte 長）。
    let mut response_fields: BTreeMap<String, AuditField> = BTreeMap::new();
    response_fields.insert("body".to_string(), body_field(response_bytes));
    let response_payload = AuditPayload {
        fields: response_fields,
    };

    [
        AuditEvent::HostCallStarted {
            operation_id: id,
            call_id: id,
            function: name.to_string(),
            request: request_payload,
            request_bytes,
        },
        AuditEvent::CapabilityDecision {
            operation_id: id,
            call_id: Some(id),
            capability: "filesystem".to_string(),
            action: name.to_string(),
            resource: resource_payload,
            decision,
            rule_id,
        },
        AuditEvent::HostCallFinished {
            operation_id: id,
            call_id: id,
            outcome,
            response: response_payload,
            response_bytes,
            error_code,
            effect_status,
        },
    ]
}

// C10: ambient fs builtin（read_file/read_lines/write_file/append_file ほか 14 名）の
// `sandbox::check_path` + 生 `std::fs` 実装は削除した。fs builtin は tree/VM の dispatch 境界で
// 必ず capability 経路（[`dispatch_fs`] → [`dispatch_filesystem_capability`]）を通る。

// =============================================================================
// 環境系
// =============================================================================

// C10: `ENV_ALLOW` OnceLock / `env_allowed_keys` / `is_env_key_allowed` /
// `ambient_environment_snapshot` / `is_protected_env_key` は削除した。process env の走査・
// allow-list 適用（`TSUMUGI_ENV_ALLOW`）と `TSUMUGI_` 保護は CLI の safe/legacy profile builder
// （binary crate 側 `cli_capability`）がその場で 1 回行い、frozen `EnvironmentSnapshot` として
// 注入する。`env()` builtin（`resolve_env`）は snapshot だけを読む。
//
// `normalize_ambient_env_key` は `resolve_env` の lookup 正規化が参照するため残す。

/// 環境変数 key を OS 規則で正規化する（`resolve_env` lookup 用）。
///
/// Windows の process env lookup は case-insensitive なので、snapshot 構築時・lookup 時とも
/// key を大文字化してこの ambient 挙動を保つ（legacy の `std::env::var` 相当）。他 OS は
/// case-sensitive なのでそのまま返す。`is_protected_env_key` の Windows 大文字化と整合する。
pub(crate) fn normalize_ambient_env_key(key: &str) -> String {
    #[cfg(windows)]
    {
        key.to_uppercase()
    }
    #[cfg(not(windows))]
    {
        key.to_string()
    }
}

/// `env(key)` を解決する共通ロジック（Phase 2 C3、CAP-AT-05）。tree/VM 両 engine が使う。
///
/// - arity（1）と型（Str）は呼び出し側が検査済みで、ここは評価済み key と Environment
///   snapshot の有無を受け取る。
/// - `environment` が `None`（Environment 未 grant）なら environment adapter call 0 のまま
///   catch 可能な `capability` エラーを返す（第5節、`{name}` = `env`）。process env へは触れない。
/// - grant 済みなら snapshot だけを引く。missing key は `null`（error にしない）。
///
/// snapshot は start 前に固定され、実行中に process env を再読しない（CAP-AT-05）。
pub fn resolve_env(
    environment: Option<&crate::capability::EnvironmentSnapshot>,
    key: &str,
    line: usize,
) -> Result<Value, TsumugiError> {
    // authority 検査を snapshot lookup より先に行う（第3.7節 error precedence）。
    let Some(snapshot) = environment else {
        return Err(TsumugiError::capability_denied(line, "env"));
    };
    // Windows は case-insensitive な OS env に合わせ key を正規化して引く（ambient 経路の
    // snapshot も同じ正規化で構築される）。他 OS は case-sensitive の exact match。
    let lookup_key = normalize_ambient_env_key(key);
    match snapshot.get(&lookup_key) {
        Some(value) => Ok(Value::str_constant(value.expose_to_script().to_string())),
        None => Ok(Value::Null),
    }
}

/// `now()` を解決する共通ロジック（Phase 2 C3、CAP-AT-06）。tree/VM 両 engine が使う。
///
/// - arity（0）は呼び出し側が検査済み。
/// - `clock` が `None`（Clock 未 grant）なら trait call 0 のまま catch 可能な `capability`
///   エラーを返す（第6節、`{name}` = `now`）。system clock へは触れない。
/// - grant 済みなら Clock adapter の `now_utc` を Unix 秒（`Int`）へ写す。UNIX epoch より前や
///   極端な時刻は従来どおり秒へ丸め、負値も許す（AUD-036 の完全な checked 変換は別追跡）。
pub fn resolve_now(
    clock: Option<&std::sync::Arc<dyn crate::capability::Clock>>,
    line: usize,
) -> Result<Value, TsumugiError> {
    use std::time::UNIX_EPOCH;
    let Some(clock) = clock else {
        return Err(TsumugiError::capability_denied(line, "now"));
    };
    let now = clock.now_utc();
    let secs = match now.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        // epoch より前は負の秒数として表現する（従来の unwrap_or_default は 0 化していたが、
        // fixed clock で epoch 前を設定できるため符号を保つ）。
        Err(e) => -(e.duration().as_secs() as i64),
    };
    Ok(Value::Int(secs))
}

/// `input()` を解決する共通ロジック（Phase 2 C4、CAP-AT-08）。tree/VM 両 engine が使う。
///
/// - arity（0）は呼び出し側が検査済み。
/// - `stdin` が `None`（Stdin 未 grant）なら Input adapter call 0 のまま catch 可能な
///   `capability` エラーを返す（第7節、`{name}` = `input`）。process stdin へは触れない。
/// - grant 済みなら Input adapter の `read_line` を呼ぶ。EOF は `null` へ写し、
///   `AdapterError::Host` は catch 可能な canonical `host` エラーへ写す（`null` へ潰さない）。
pub fn resolve_input(
    stdin: Option<&std::sync::Arc<dyn crate::capability::Input>>,
    line: usize,
) -> Result<Value, TsumugiError> {
    use crate::capability::InputLine;
    // authority 検査を adapter call より先に行う（第3.7節 error precedence）。
    let Some(stdin) = stdin else {
        return Err(TsumugiError::capability_denied(line, "input"));
    };
    match stdin.read_line() {
        Ok(InputLine::Line(text)) => Ok(Value::str_constant(text)),
        Ok(InputLine::Eof) => Ok(Value::Null),
        // host 起因の失敗は null へ潰さず catch 可能な `host` エラーにする（第7節）。
        // stdio に secure resolution はないが、`AdapterError` は non_exhaustive のため
        // 他 variant も host 失敗として安全側に写す。
        Err(_) => Err(TsumugiError::host_adapter_failed(line, "input", "stdin")),
    }
}

/// `print` を解決する共通ロジック（Phase 2 C4、CAP-AT-07）。tree/VM 両 engine が使う。
///
/// - 引数の評価・join・budget 課金は呼び出し側が済ませ、logical write 対象の `payload` を渡す。
/// - `stdout` が `None`（Stdout 未 grant）なら Output adapter call 0 のまま catch 可能な
///   `capability` エラーを返す（第7節、`{name}` = `print`）。process stdout へは触れない。
/// - grant 済みなら UTF-8 bytes + 改行を一括 write する。`AdapterError::Host`（broken pipe
///   等）は catch 可能な canonical `host` エラーへ写す（従来 `write_stdout_line` は `Io` error
///   にしていたが、C4 では adapter 経由の `host` error に統一する）。
pub fn resolve_print(
    stdout: Option<&std::sync::Arc<dyn crate::capability::Output>>,
    payload: &str,
    line: usize,
) -> Result<(), TsumugiError> {
    // authority 検査を adapter call より先に行う（第3.7節 error precedence）。
    let Some(stdout) = stdout else {
        return Err(TsumugiError::capability_denied(line, "print"));
    };
    // UTF-8 bytes + 改行を一括 write する（第7節「logical write 前に一括 charge」に対応する
    // 単一 write）。改行を含めて 1 回の write_all で渡す。
    let mut bytes = payload.as_bytes().to_vec();
    bytes.push(b'\n');
    match stdout.write_all(&bytes) {
        Ok(()) => Ok(()),
        // stdio に secure resolution はないが、`AdapterError` は non_exhaustive のため
        // 他 variant も host 失敗として安全側に写す。
        Err(_) => Err(TsumugiError::host_adapter_failed(line, "print", "stdout")),
    }
}

// =============================================================================
// filesystem capability dispatch（Phase 2 C5-c、CAP-AT-12/13/14）
// =============================================================================

/// filesystem authority を consult する builtin か（tree/VM が capability 経路へ振り分ける）。
///
/// `path_join` は純粋なので含めない。`import` は resolver（C6）が扱うため含めない。
pub fn is_filesystem_builtin(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "read_lines"
            | "write_file"
            | "append_file"
            | "path_exists"
            | "mkdir"
            | "remove"
            | "remove_dir"
            | "remove_tree"
            | "rename"
            | "list_dir"
            | "file_size"
            | "is_file"
            | "is_dir"
    )
}

/// filesystem builtin を frozen [`FilesystemCapability`] 経由で dispatch する薄い wrapper
/// （C10、tree/VM 共通）。
///
/// `filesystem` が `Some`（Filesystem authority grant 済み）なら
/// [`dispatch_filesystem_capability`] へ委譲する。`None`（未 grant）なら adapter/OS call を
/// 一切行わず、単一の catch 可能な `sandbox` denial（[`TsumugiError::filesystem_denied`]）を
/// 返す。これにより fs builtin は grant 有無に関わらず常に capability 経路を通り、ambient
/// `std::fs` fallback へは決して落ちない（CAP-AT-27）。
pub fn dispatch_fs(
    name: &str,
    args: &[Value],
    filesystem: Option<&crate::capability::FilesystemCapability>,
    max_collection: u64,
    revision: crate::embedding::LanguageRevision,
    line: usize,
) -> FsDispatch {
    let mut labels = AuditResourceLabels::for_operation(name);
    let outcome = match filesystem {
        Some(fs) => dispatch_filesystem_capability(
            name,
            args,
            fs,
            max_collection,
            revision,
            line,
            &mut labels,
        ),
        // 未 grant（Deny、label 確定前）。labels は初期値（全 None）のまま。
        None => Err(TsumugiError::filesystem_denied(line)),
    };
    FsDispatch { outcome, labels }
}

/// [`dispatch_fs`] の戻り値（Phase 6 R-1、§9.1）。outcome に加えて canonical resource label を運ぶ。
///
/// `labels` は capability decision が `Allow`（成功・host error の両方）のとき確定した canonical
/// label を保持し、`Deny`（未 grant・path/argument エラー・mount/op denial・Upsert の Create 不足）
/// では該当 label が `None` になる。label の有無は routing が通ったか（＝ decision）で決まり、その後の
/// adapter 成否とは独立する（host error でも label は保持する、R-1 スライス）。非監査経路（sink なし）も
/// 常にこの型を受け取るが labels を無視し、triple 構築側だけが使う（観測挙動は不変）。
pub struct FsDispatch {
    /// filesystem 操作の結果。
    pub outcome: Result<Value, TsumugiError>,
    /// 認可に使った canonical resource label。単一 path 操作は 1 つ、`rename` は 2 つ（§9.1 / R-1）。
    pub labels: AuditResourceLabels,
}

/// host-call が認可に使った canonical resource label（§9.1 / R-1 スライス）。
///
/// 単一 path を扱う操作（read/write/mkdir/remove/list 等）は 1 つの label（[`Self::Single`]）、
/// `rename` は source / destination の 2 つ（[`Self::Rename`]）を運ぶ。単一 `Option<String>` では
/// `rename` の destination が source を上書きしてしまうため、2 label を保持できるこの形に広げた
/// （5 周目レビュー反映。単一操作は従来どおり 1 label のまま）。
///
/// label は decision が `Allow` のとき `Some`、`Deny` のとき `None`。
pub enum AuditResourceLabels {
    /// 単一 path 操作の canonical label。
    Single(Option<String>),
    /// `rename` の source / destination の canonical label。
    Rename {
        /// 移動元の canonical label（source の route が通ったとき `Some`）。
        source: Option<String>,
        /// 移動先の canonical label（destination の route が通ったとき `Some`）。
        destination: Option<String>,
    },
}

impl AuditResourceLabels {
    /// operation 名から初期値（全 `None`）を作る。`rename` だけ 2 label 形、他は単一形。
    fn for_operation(name: &str) -> Self {
        if name == "rename" {
            AuditResourceLabels::Rename {
                source: None,
                destination: None,
            }
        } else {
            AuditResourceLabels::Single(None)
        }
    }
}

/// routing 後の [`FilesystemTarget`] を canonical resource label 文字列へ組む（§9.1 / R-1 スライス）。
///
/// 形式は `@<mount>/<component>/...`。host absolute path・CWD・symlink 解決結果は含めない。
/// 同じ mount・同じ mount 相対 path は同じ label（＝同じ digest）になる。
fn canonical_resource_label(target: &crate::capability::FilesystemTarget) -> String {
    let mut label = String::from("@");
    label.push_str(target.mount.as_str());
    for component in target.path.components() {
        label.push('/');
        label.push_str(component);
    }
    label
}

/// filesystem builtin を frozen [`FilesystemCapability`] 経由で実行する（C5-c、案 B の grant 経路）。
///
/// tree/VM 両 engine が、filesystem authority が grant されているときにこの関数へ振り分ける
/// （未 grant の ambient 経路は従来の [`dispatch`]＝process-global sandbox のまま）。
///
/// # error 分離（capability-model 第8.5節）
///
/// - 引数の arity/型（`Str`）は adapter/authority 検査より先に検査する。
/// - script path の lexical 検証（[`crate::capability::FilesystemTarget::parse`]）は
///   capability lookup より先に行い、[`crate::capability::PathError`] は catch 可能な
///   `argument` error へ写す（authority channel と混在させない）。
/// - mount 未登録・operation 未 grant・許可外の存在/不存在は adapter/OS call 0 で単一の
///   catch 可能な `sandbox` denial（[`TsumugiError::filesystem_denied`]）にする。`null`/`false`
///   へ潰さない（存在 oracle 防止）。
/// - adapter 失敗（[`crate::capability::AdapterError`]、`SecureResolutionUnsupported` を含む）は
///   catch 可能な canonical `host` error（[`TsumugiError::host_adapter_failed`]）へ写す。
pub fn dispatch_filesystem_capability(
    name: &str,
    args: &[Value],
    filesystem: &crate::capability::FilesystemCapability,
    max_collection: u64,
    revision: crate::embedding::LanguageRevision,
    line: usize,
    labels: &mut AuditResourceLabels,
) -> Result<Value, TsumugiError> {
    use crate::capability::{FsOperation, OpenFileRequest, RemoveKind, WriteMode};

    // 単一 path 操作用の label slot。`rename` 以外はこの 1 つを helper へ渡す。`rename` は
    // 自分の 2 label を直接 `labels` へ書くため、この slot は使わない。
    //
    // arm 内の `?` が関数全体を抜けて `*labels` 書き込みを飛ばさないよう、単一 path の dispatch は
    // 内側クロージャに包み、`?` をクロージャ内で閉じる（label は host error でも確定済みを保持する）。
    let mut label: Option<String> = None;
    let label = &mut label;

    let result = (|| match name {
        "read_file" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let mut file = fs_open(
                name,
                filesystem,
                path,
                OpenFileRequest::ReadExisting,
                line,
                label,
            )?;
            let bytes = file
                .read_to_end(None)
                .map_err(|_| fs_host_error(name, line))?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            Ok(Value::str_constant(text))
        }
        "read_lines" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let mut file = fs_open(
                name,
                filesystem,
                path,
                OpenFileRequest::ReadExisting,
                line,
                label,
            )?;
            let bytes = file
                .read_to_end(None)
                .map_err(|_| fs_host_error(name, line))?;
            let content = String::from_utf8_lossy(&bytes).into_owned();
            let mut lines = Vec::new();
            for content_line in content.lines() {
                check_collection_size(lines.len().saturating_add(1), max_collection, line)?;
                lines.push(Value::str_constant(content_line.to_string()));
            }
            Ok(Value::List(Tracked::constant(lines)))
        }
        "write_file" => {
            check_arity(name, args, 2, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let content = fs_content_arg(&args[1]);
            let mut file = fs_open(
                name,
                filesystem,
                path,
                OpenFileRequest::Upsert {
                    mode: WriteMode::Truncate,
                },
                line,
                label,
            )?;
            file.write_all(content.as_bytes())
                .map_err(|_| fs_host_error(name, line))?;
            Ok(Value::Bool(true))
        }
        "append_file" => {
            check_arity(name, args, 2, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let content = fs_content_arg(&args[1]);
            let mut file = fs_open(
                name,
                filesystem,
                path,
                OpenFileRequest::Upsert {
                    mode: WriteMode::Append,
                },
                line,
                label,
            )?;
            file.write_all(content.as_bytes())
                .map_err(|_| fs_host_error(name, line))?;
            Ok(Value::Bool(true))
        }
        "mkdir" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let (root, target) =
                fs_route(name, filesystem, path, FsOperation::Create, line, label)?;
            root.adapter
                .create_dir(&target.path)
                .map_err(|_| fs_host_error(name, line))?;
            Ok(Value::Bool(true))
        }
        "remove" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let (root, target) =
                fs_route(name, filesystem, path, FsOperation::Delete, line, label)?;
            root.adapter
                .remove(&target.path, RemoveKind::FileOrSymlink)
                .map_err(|_| fs_host_error(name, line))?;
            Ok(Value::Bool(true))
        }
        "remove_dir" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            // remove_dir は空 directory のみ（`EmptyDirectory`）。非空は最も一般的な失敗要因の
            // ため、capability 経路の失敗を `directory_not_empty` category の host error へ写す
            // （REV-021 §17.6.5）。再帰削除は remove_tree（`RecursiveDelete`）を使う。
            let (root, target) =
                fs_route(name, filesystem, path, FsOperation::Delete, line, label)?;
            root.adapter
                .remove(&target.path, RemoveKind::EmptyDirectory)
                .map_err(|_| {
                    TsumugiError::host_adapter_failed(line, name, "directory_not_empty")
                })?;
            Ok(Value::Bool(true))
        }
        "remove_tree" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            // 再帰削除は専用 capability `RecursiveDelete` を要求する（`EmptyDirectory` から
            // 暗黙昇格させない、REV-021 §17.6）。symlink は adapter がリンク自体を削除する。
            let (root, target) = fs_route(
                name,
                filesystem,
                path,
                FsOperation::RecursiveDelete,
                line,
                label,
            )?;
            root.adapter
                .remove(&target.path, RemoveKind::Tree)
                .map_err(|_| fs_host_error(name, line))?;
            Ok(Value::Bool(true))
        }
        "rename" => {
            check_arity(name, args, 2, line)?;
            let from = require_str(&args[0], name, 1, line)?;
            let to = require_str(&args[1], name, 2, line)?;
            // rename は source `Delete` と destination `Create` を要求する（第8.4節）。source/dest
            // で 2 回 route するため、label も source/destination で別々に確定する（§9.1 / R-1）。
            // source だけ route が通った後に destination で Deny しても、source の label は保持する
            // （decision が Deny でも payload 側で Omitted に畳む）。
            let mut source_label: Option<String> = None;
            let mut destination_label: Option<String> = None;
            let route_result = (|| {
                let (from_root, from_target) = fs_route(
                    name,
                    filesystem,
                    from,
                    FsOperation::Delete,
                    line,
                    &mut source_label,
                )?;
                let (to_root, to_target) = fs_route(
                    name,
                    filesystem,
                    to,
                    FsOperation::Create,
                    line,
                    &mut destination_label,
                )?;
                from_root
                    .adapter
                    .rename(
                        &from_target.path,
                        to_root.adapter.as_ref(),
                        &to_target.path,
                        true,
                    )
                    .map_err(|_| fs_host_error(name, line))?;
                Ok(Value::Bool(true))
            })();
            // route で確定した 2 label を結果に反映する（host error でも保持、Deny では None のまま）。
            *labels = AuditResourceLabels::Rename {
                source: source_label,
                destination: destination_label,
            };
            route_result
        }
        "list_dir" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let (root, target) = fs_route(name, filesystem, path, FsOperation::List, line, label)?;
            // REV-009 §17.4（safe profile）: 個別 entry 取得失敗・非 UTF-8 名を黙殺せず、
            // それぞれ `directory_read` / `invalid_encoding` category の catch 可能な `host`
            // error へ写す（部分結果を成功 List として返さない）。
            let entries = root
                .adapter
                .list(&target.path, None)
                .map_err(|e| fs_list_error(name, e, line))?;
            let mut names = Vec::new();
            for entry in entries {
                check_collection_size(names.len().saturating_add(1), max_collection, line)?;
                names.push(Value::str_constant(entry.name));
            }
            // REV-017: 整列キーを全順序 TotalOrder へ一本化する。list_dir の要素は常に Str
            // なので実効順序はコードポイント順で両 revision 同一だが、キー経路を全順序へ
            // 寄せて backend 差・将来の型拡張差を作らない（§17.8.4）。整列はエラーにならな
            // かった名前集合に対してのみ行う（REV-009 の per-entry 処理は不変）。
            if revision >= crate::embedding::LanguageRevision::V0_21 {
                names.sort_by(crate::value::TotalOrder::compare);
            } else {
                names.sort_by_key(|v| v.to_string());
            }
            Ok(Value::List(Tracked::constant(names)))
        }
        "file_size" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let (root, target) =
                fs_route(name, filesystem, path, FsOperation::Metadata, line, label)?;
            let meta = root
                .adapter
                .metadata(&target.path, true)
                .map_err(|_| fs_host_error(name, line))?;
            Ok(Value::Int(checked_file_size_to_i64(meta.size_bytes, line)?))
        }
        "is_file" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let kind = fs_metadata_kind(name, filesystem, path, line, label)?;
            Ok(Value::Bool(matches!(
                kind,
                Some(crate::capability::EntryKind::File)
            )))
        }
        "is_dir" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let kind = fs_metadata_kind(name, filesystem, path, line, label)?;
            Ok(Value::Bool(matches!(
                kind,
                Some(crate::capability::EntryKind::Directory)
            )))
        }
        "path_exists" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let kind = fs_metadata_kind(name, filesystem, path, line, label)?;
            Ok(Value::Bool(kind.is_some()))
        }
        // is_filesystem_builtin と本 match は同じ名前集合を持つ。ここへ来ない。
        _ => Err(TsumugiError::filesystem_denied(line)),
    })();
    // 単一 path 操作で確定した label を結果へ反映する。`rename` はクロージャ内で `*labels` を
    // 2 label 形（`Rename`）へ書き済みなので、ここで `Single` へ上書きしない。
    if name != "rename" {
        *labels = AuditResourceLabels::Single(std::mem::take(label));
    }
    result
}

/// write/append の content 引数を文字列化する（`Str` はそのまま、他は `to_string`）。
fn fs_content_arg(value: &Value) -> String {
    match value {
        Value::Str(s) => s.to_string(),
        other => other.to_string(),
    }
}

/// filesystem 操作失敗を canonical `host` error（category `filesystem`）へ写す。
fn fs_host_error(name: &str, line: usize) -> TsumugiError {
    TsumugiError::host_adapter_failed(line, name, "filesystem")
}

/// `list_dir` の adapter 失敗を category 付きの `host` error へ写す（REV-009 §17.4）。
///
/// 個別 entry 取得失敗は `directory_read`、非 UTF-8 名は `invalid_encoding`、その他は
/// 一般の `filesystem` category とする。
fn fs_list_error(name: &str, error: crate::capability::AdapterError, line: usize) -> TsumugiError {
    let category = match error {
        crate::capability::AdapterError::DirectoryReadFailed => "directory_read",
        crate::capability::AdapterError::NonUtf8EntryName => "invalid_encoding",
        _ => "filesystem",
    };
    TsumugiError::host_adapter_failed(line, name, category)
}

/// script path を parse し、mount routing と operation 認可を行う（第8.2・8.4・8.5節）。
///
/// lexical error は `argument`、mount 未登録・operation 未 grant は単一の `sandbox` denial。
fn fs_route<'a>(
    name: &str,
    filesystem: &'a crate::capability::FilesystemCapability,
    path: &str,
    operation: crate::capability::FsOperation,
    line: usize,
    label: &mut Option<String>,
) -> Result<
    (
        &'a crate::capability::FilesystemRoot,
        crate::capability::FilesystemTarget,
    ),
    TsumugiError,
> {
    let _ = name;
    // legacy profile は host path（絶対/相対）を登録 root 最長一致で mount 相対へ翻訳する
    // （§4.2/§5）。safe / 埋め込み host（translator なし）は従来どおり `@mount/...` 構文を
    // `FilesystemTarget::parse` で解釈し、絶対パスは `PathError::Absolute` で拒否する。
    let target = match filesystem.legacy_translator() {
        Some(translator) => match translator.translate(path) {
            // route 先 root 無し（sandbox 範囲外）は単一の sandbox denial（存在 oracle 防止）。
            Ok(None) => return Err(TsumugiError::filesystem_denied(line)),
            Ok(Some(target)) => target,
            Err(_) => {
                return Err(TsumugiError::runtime_with_message_id(
                    line,
                    crate::error::ErrorKind::Argument,
                    crate::stable_error::message_id::ARGUMENT_INVALID_PATH,
                    "ファイルパスの形式が不正です",
                ));
            }
        },
        None => crate::capability::FilesystemTarget::parse(path).map_err(|_| {
            // lexical 検証を capability lookup より先に行う（PathError は catch 可能な argument
            // error。authority channel と分離する、第8.5節）。message に host path を含めない。
            TsumugiError::runtime_with_message_id(
                line,
                crate::error::ErrorKind::Argument,
                crate::stable_error::message_id::ARGUMENT_INVALID_PATH,
                "ファイルパスの形式が不正です",
            )
        })?,
    };
    // mount 完全一致で root を引く。未登録は単一の sandbox denial（存在 oracle 防止）。
    let Some(root) = filesystem.root(&target.mount) else {
        return Err(TsumugiError::filesystem_denied(line));
    };
    // operation が grant されていなければ adapter call 0 で denial。
    if !root.operations.contains(&operation) {
        return Err(TsumugiError::filesystem_denied(line));
    }
    // routing と operation 認可が通った時点で canonical resource label を確定する（§9.1 / R-1）。
    // この後 adapter が失敗（host error）しても label は保持する。
    *label = Some(canonical_resource_label(&target));
    Ok((root, target))
}

/// path を route してから file を open する（read/write 系）。
fn fs_open(
    name: &str,
    filesystem: &crate::capability::FilesystemCapability,
    path: &str,
    request: crate::capability::OpenFileRequest,
    line: usize,
    label: &mut Option<String>,
) -> Result<Box<dyn crate::capability::FileHandle>, TsumugiError> {
    use crate::capability::{FsOperation, OpenFileRequest};
    // open request に対応する FsOperation を確定する（第8.4節 operation matrix）。
    let operation = match request {
        OpenFileRequest::ReadExisting => FsOperation::Read,
        OpenFileRequest::WriteExisting { .. } => FsOperation::Write,
        OpenFileRequest::CreateNew { .. } => FsOperation::Create,
        // Upsert は Write+Create を事前要求する（第8.4節）。ここでは Write を代表 op として
        // route し、Create の可否は下で追加検査する。
        OpenFileRequest::Upsert { .. } => FsOperation::Write,
    };
    let (root, target) = fs_route(name, filesystem, path, operation, line, label)?;
    // Upsert は Write+Create を事前要求する（第8.4節）。route では Write を検査済みなので、
    // 追加で Create の grant を確認する（不足なら adapter call 0 の sandbox denial）。
    if matches!(request, OpenFileRequest::Upsert { .. })
        && !root.operations.contains(&FsOperation::Create)
    {
        // Upsert の Create 不足は route 成功後の Deny（§9.1 / R-AT-02d）。label は fs_route で
        // 立っているが decision が Deny なので None へ戻し、Path を Omitted にする。
        *label = None;
        return Err(TsumugiError::filesystem_denied(line));
    }
    root.adapter
        .open_file(&target.path, request)
        .map_err(|_| fs_host_error(name, line))
}

/// path を route して metadata を取得し、[`EntryKind`] を返す（exists/is_file/is_dir 用）。
///
/// not-found は `None`（許可 root 内の benign な不存在。存在 oracle は route/認可で担保済み）。
/// route 失敗（mount/op 不足）は `sandbox` denial、adapter 失敗は `host` error。
fn fs_metadata_kind(
    name: &str,
    filesystem: &crate::capability::FilesystemCapability,
    path: &str,
    line: usize,
    label: &mut Option<String>,
) -> Result<Option<crate::capability::EntryKind>, TsumugiError> {
    use crate::capability::FsOperation;
    let (root, target) = fs_route(name, filesystem, path, FsOperation::Metadata, line, label)?;
    match root.adapter.metadata(&target.path, true) {
        Ok(meta) => Ok(Some(meta.kind)),
        // 許可 root 内の not-found は benign（false へ写す）。それ以外の adapter 失敗も
        // exists/type 系は従来 bool を返すため、ここでは None（=不存在扱い）に畳む。
        Err(_) => Ok(None),
    }
}

// =============================================================================
// パス・ファイルシステム系
// =============================================================================

pub fn builtin_path_join(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    // 全引数を左から右へ Str として検査し、非 Str を1つでも見つけたら結合を開始しない。
    // 非 Str を無言で欠落させない（AUD-034, semantic-decisions.md §9）。
    for (index, arg) in args.iter().enumerate() {
        if !matches!(arg, Value::Str(_)) {
            return Err(TsumugiError::builtin_arg_type(
                line,
                "path_join",
                index + 1,
                "Str",
                arg,
            ));
        }
    }
    let mut path = std::path::PathBuf::new();
    for arg in args {
        if let Value::Str(s) = arg {
            path.push(s.as_str());
        }
    }
    Ok(Value::str_constant(path.to_string_lossy().to_string()))
}

// C10: ambient fs builtin（mkdir/remove/remove_dir/remove_tree/rename/list_dir/file_size/
// is_file/is_dir と remove_symlink_entry helper）の `sandbox::check_*` + 生 `std::fs` 実装は
// 削除した。これらは tree/VM の dispatch 境界で必ず capability 経路（[`dispatch_fs`] →
// [`dispatch_filesystem_capability`]）を通る。`path_join` は純関数なので残す。

// =============================================================================
// ディスパッチ関数
// =============================================================================

/// PureCore builtin と、両engineが値だけで実行できる `push` / `pop` を名前で呼び出す。
/// 該当すれば Ok(Some(value))、該当しなければ Ok(None) を返す。
/// map/filter/each/print/input/exit/args は実行コンテキスト依存のため含まない。
/// 内部命令 `__pop_update` は public 名として持たず、[`builtin_pop_update`] を
/// VM の [`crate::opcode::OpCode::PopUpdate`] と tree の pop 実装が直接呼ぶ（AUD-049）。
pub fn dispatch(
    name: &str,
    args: &[Value],
    max_collection: u64,
    line: usize,
) -> Result<Option<Value>, TsumugiError> {
    let result = match name {
        "len" => builtin_len(args, line)?,
        "push" => builtin_push(args, max_collection, line)?,
        "pop" => builtin_pop(args, line)?,
        "keys" => builtin_keys(args, max_collection, line)?,
        "values" => builtin_values(args, max_collection, line)?,
        "has_key" => builtin_has_key(args, line)?,
        "type" => builtin_type(args, line)?,
        "slice" => builtin_slice(args, line)?,
        "contains" => builtin_contains(args, line)?,
        "reverse" => builtin_reverse(args, line)?,
        "range" => builtin_range(args, max_collection, line)?,
        "split" => builtin_split(args, max_collection, line)?,
        // sort / join / to_str は REV-001 で budget-aware 版（builtin_sort_budgeted /
        // builtin_join_budgeted / builtin_to_str_budgeted）へ移し、tree/VM の dispatch
        // 境界が intercept する。ここへは到達しない（budget を持てない dispatch では
        // 共有 DAG の指数時間／出力を有限化できない）。
        "trim" => builtin_trim(args, line)?,
        "upper" => builtin_upper(args, line)?,
        "lower" => builtin_lower(args, line)?,
        "starts_with" => builtin_starts_with(args, line)?,
        "ends_with" => builtin_ends_with(args, line)?,
        "replace" => builtin_replace(args, line)?,
        "to_int" => builtin_to_int(args, line)?,
        "to_float" => builtin_to_float(args, line)?,
        "abs" => builtin_abs(args, line)?,
        "min" => builtin_min(args, line)?,
        "max" => builtin_max(args, line)?,
        "floor" => builtin_floor(args, line)?,
        "ceil" => builtin_ceil(args, line)?,
        "round" => builtin_round(args, line)?,
        "format_time" => builtin_format_time(args, line)?,
        // C10: fs builtin（read_file/.../is_dir の 14 名）は dispatch へ来ない。tree/VM の
        // 境界が `is_filesystem_builtin` で intercept し capability 経路（dispatch_fs）へ回す。
        // `path_join` は純関数なので dispatch に残す。
        "path_join" => builtin_path_join(args, line)?,
        _ => return Ok(None),
    };
    Ok(Some(result))
}

// =============================================================================
// ヘルパー関数
// =============================================================================

pub fn format_unix_timestamp(timestamp: i64, format: &str) -> String {
    let secs_per_day: i64 = 86400;

    // days と day_secs を正しく計算（負のタイムスタンプ対応）
    // Rust の % は truncated division なので、負の場合は補正する
    let mut days = timestamp.div_euclid(secs_per_day);
    let day_secs = timestamp.rem_euclid(secs_per_day) as u32;
    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    let seconds = day_secs % 60;

    // Gregorian暦は400年ごとに曜日・うるう年の並びが繰り返される。
    // 先に周期分を移動しておくことで、年ごとの反復をtimestampによらず最大399回に制限する。
    const YEARS_PER_GREGORIAN_CYCLE: i64 = 400;
    const DAYS_PER_GREGORIAN_CYCLE: i64 = 146_097;
    let cycles = days.div_euclid(DAYS_PER_GREGORIAN_CYCLE);
    days = days.rem_euclid(DAYS_PER_GREGORIAN_CYCLE);
    let mut year = 1970 + cycles * YEARS_PER_GREGORIAN_CYCLE;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let month_days = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month: u32 = 1;
    for md in month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    let day = days + 1;

    format
        .replace("%Y", &format!("{:04}", year))
        .replace("%m", &format!("{:02}", month))
        .replace("%d", &format!("{:02}", day))
        .replace("%H", &format!("{:02}", hours))
        .replace("%M", &format!("{:02}", minutes))
        .replace("%S", &format!("{:02}", seconds))
        // UTC 固定のタイムゾーンマーカー（semantic-decisions.md §20）。
        // 置換値は定数リテラルで、chrono・システム TZ・ロケール・システムクロックに依存しない。
        .replace("%z", "+0000")
        .replace("%Z", "UTC")
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod host_call_triple_tests {
    //! `host_call_triple` の分類ユニットテスト（§7.2/§8）。full run を回さずに、deny/allow/
    //! host-error × read/write の分類と triple の ID 再利用・emission 順序を固定する。
    use super::{AuditResourceLabels, RequestedPathBytes, host_call_triple};
    use crate::audit::{
        AuditEvent, AuditField, AuditFieldClass, CapabilityDecisionKind, EffectStatus,
        HostCallOutcome, RedactionPolicy,
    };
    use crate::error::{ErrorKind, TsumugiError};
    use crate::value::Value;

    /// 既定 policy・sample label でのテスト用 triple 構築（Allow は label あり、Deny は None を
    /// 呼び出し側で与える）。単一 path 操作用に `Single` label を組む。requested_path_bytes は
    /// テストでは 0 既定（primary のみ使用）。
    fn triple(
        id: u64,
        name: &str,
        request_bytes: u64,
        result: &Result<Value, TsumugiError>,
        response_bytes: u64,
        label: Option<&str>,
        requested_path_bytes: u64,
    ) -> [AuditEvent; 3] {
        host_call_triple(
            id,
            name,
            request_bytes,
            result,
            response_bytes,
            &RedactionPolicy::default(),
            &AuditResourceLabels::Single(label.map(|l| l.to_string())),
            &RequestedPathBytes {
                primary: requested_path_bytes,
                destination: 0,
            },
        )
    }

    /// triple[1]（CapabilityDecision）の resource.path field を取り出す。
    fn resource_path(triple: &[AuditEvent; 3]) -> &AuditField {
        match &triple[1] {
            AuditEvent::CapabilityDecision { resource, .. } => resource
                .fields
                .get("path")
                .expect("resource に path field がある"),
            other => panic!("triple[1] は CapabilityDecision のはず: {other:?}"),
        }
    }

    /// triple を (operation_id, call_id, decision, outcome, effect_status, error_code) へ畳む。
    #[allow(clippy::type_complexity)]
    fn parts(
        triple: &[AuditEvent; 3],
    ) -> (
        u64,
        Option<u64>,
        u64,
        CapabilityDecisionKind,
        HostCallOutcome,
        EffectStatus,
        Option<String>,
    ) {
        let (started_op, started_call) = match &triple[0] {
            AuditEvent::HostCallStarted {
                operation_id,
                call_id,
                ..
            } => (*operation_id, *call_id),
            other => panic!("期待 HostCallStarted, 実際 {other:?}"),
        };
        let (decision, dec_op, dec_call) = match &triple[1] {
            AuditEvent::CapabilityDecision {
                operation_id,
                call_id,
                decision,
                ..
            } => (*decision, *operation_id, *call_id),
            other => panic!("期待 CapabilityDecision, 実際 {other:?}"),
        };
        let (outcome, effect_status, error_code, fin_op, fin_call) = match &triple[2] {
            AuditEvent::HostCallFinished {
                outcome,
                effect_status,
                error_code,
                operation_id,
                call_id,
                ..
            } => (
                *outcome,
                *effect_status,
                error_code.clone(),
                *operation_id,
                *call_id,
            ),
            other => panic!("期待 HostCallFinished, 実際 {other:?}"),
        };
        // ID はすべて triple 内で同値（§7.2 の再利用）。
        assert_eq!(started_op, started_call);
        assert_eq!(dec_op, started_op);
        assert_eq!(dec_call, Some(started_op));
        assert_eq!(fin_op, started_op);
        assert_eq!(fin_call, started_op);
        (
            started_op,
            dec_call,
            started_call,
            decision,
            outcome,
            effect_status,
            error_code,
        )
    }

    #[test]
    fn read_success_is_allow_success_none() {
        let result = Ok(Value::str_constant("hello".to_string()));
        let triple = triple(0, "read_file", 0, &result, 5, Some("@default/a.txt"), 0);
        let (_, _, _, decision, outcome, effect, code) = parts(&triple);
        assert_eq!(decision, CapabilityDecisionKind::Allow);
        assert_eq!(outcome, HostCallOutcome::Success);
        assert_eq!(effect, EffectStatus::None);
        assert_eq!(code, None);
        // Allow の Path は HmacSha256（§9.1 / R-AT-01）。
        assert!(matches!(
            resource_path(&triple),
            AuditField::HmacSha256 {
                class: AuditFieldClass::Path,
                ..
            }
        ));
    }

    #[test]
    fn write_success_is_allow_success_committed() {
        let result = Ok(Value::Bool(true));
        let triple = triple(3, "write_file", 11, &result, 0, Some("@default/out.txt"), 0);
        let (op, call, _, decision, outcome, effect, code) = parts(&triple);
        assert_eq!(op, 3);
        assert_eq!(call, Some(3));
        assert_eq!(decision, CapabilityDecisionKind::Allow);
        assert_eq!(outcome, HostCallOutcome::Success);
        assert_eq!(effect, EffectStatus::Committed);
        assert_eq!(code, None);
    }

    #[test]
    fn denied_is_deny_denied_none_with_pair_and_triple() {
        // filesystem_denied（ErrorKind::Sandbox）。Deny/Denied/None の 3 event が揃う（§8 規則4）。
        let result: Result<Value, TsumugiError> = Err(TsumugiError::filesystem_denied(1));
        let triple = triple(0, "read_file", 0, &result, 0, None, 10);
        // 順序は Started → Decision → Finished。
        assert!(matches!(triple[0], AuditEvent::HostCallStarted { .. }));
        assert!(matches!(triple[1], AuditEvent::CapabilityDecision { .. }));
        assert!(matches!(triple[2], AuditEvent::HostCallFinished { .. }));
        let (_, _, _, decision, outcome, effect, code) = parts(&triple);
        assert_eq!(decision, CapabilityDecisionKind::Deny);
        assert_eq!(outcome, HostCallOutcome::Denied);
        assert_eq!(effect, EffectStatus::None);
        assert_eq!(code, Some(ErrorKind::Sandbox.as_str().to_string()));
        // Deny の Path は Omitted{class:Path}、original_bytes は requested path 長（§9.1 / R-AT-02）。
        assert!(matches!(
            resource_path(&triple),
            AuditField::Omitted {
                class: AuditFieldClass::Path,
                original_bytes: 10,
            }
        ));
    }

    #[test]
    fn host_error_write_is_allow_hosterror_unknown() {
        let result: Result<Value, TsumugiError> = Err(TsumugiError::host_adapter_failed(
            1,
            "write_file",
            "filesystem",
        ));
        // Allow/HostError: route 成功後に adapter 失敗。label は確定済み。
        let triple = triple(1, "write_file", 4, &result, 0, Some("@default/out.txt"), 0);
        let (_, _, _, decision, outcome, effect, code) = parts(&triple);
        assert_eq!(decision, CapabilityDecisionKind::Allow);
        assert_eq!(outcome, HostCallOutcome::HostError);
        assert_eq!(effect, EffectStatus::Unknown);
        assert_eq!(code, Some(ErrorKind::Host.as_str().to_string()));
        // Allow/HostError でも Path は HmacSha256（§9.1 / R-AT-01）。
        assert!(matches!(
            resource_path(&triple),
            AuditField::HmacSha256 {
                class: AuditFieldClass::Path,
                ..
            }
        ));
    }

    #[test]
    fn host_error_read_is_allow_hosterror_none() {
        let result: Result<Value, TsumugiError> = Err(TsumugiError::host_adapter_failed(
            1,
            "read_file",
            "filesystem",
        ));
        let triple = triple(2, "read_file", 0, &result, 0, Some("@default/a.txt"), 0);
        let (_, _, _, decision, outcome, effect, _) = parts(&triple);
        assert_eq!(decision, CapabilityDecisionKind::Allow);
        assert_eq!(outcome, HostCallOutcome::HostError);
        assert_eq!(effect, EffectStatus::None);
        // Allow/HostError（read）でも Path は HmacSha256。
        assert!(matches!(
            resource_path(&triple),
            AuditField::HmacSha256 {
                class: AuditFieldClass::Path,
                ..
            }
        ));
    }
}

#[cfg(test)]
mod aud_036_tests {
    //! AUD-036: checked 変換 helper のユニットテスト。file_size の u64→i64 境界は
    //! `i64::MAX` 超のファイルを実際に作れないため、helper を直接検査する（§10.3）。
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn file_size_accepts_i64_max() {
        let size = i64::MAX as u64;
        assert_eq!(checked_file_size_to_i64(size, 1).unwrap(), i64::MAX);
    }

    #[test]
    fn file_size_rejects_i64_max_plus_one() {
        let size = (i64::MAX as u64) + 1;
        let error = checked_file_size_to_i64(size, 5).expect_err("i64::MAX+1 は範囲外");
        assert_eq!(error.kind(), Some(ErrorKind::IntOverflow));
        assert_eq!(error.message(), "ファイルサイズを Int で表現できません");
        assert_eq!(error.line(), 5);
    }

    #[test]
    fn file_size_rejects_u64_max() {
        let error = checked_file_size_to_i64(u64::MAX, 1).expect_err("u64::MAX は範囲外");
        assert_eq!(error.kind(), Some(ErrorKind::IntOverflow));
    }

    #[test]
    fn file_size_accepts_zero() {
        assert_eq!(checked_file_size_to_i64(0, 1).unwrap(), 0);
    }

    #[test]
    fn checked_float_accepts_i64_min() {
        let v = checked_float_to_i64(i64::MIN as f64, RoundMode::TowardZero, "to_int", 1).unwrap();
        assert_eq!(v, i64::MIN);
    }

    #[test]
    fn checked_float_rejects_two_pow_63() {
        // i64::MAX as f64 は 2^63 へ丸められるため上端として受理しない。
        let error = checked_float_to_i64(i64::MAX as f64, RoundMode::TowardZero, "to_int", 1)
            .expect_err("2^63 は範囲外");
        assert_eq!(error.kind(), Some(ErrorKind::Conversion));
        assert_eq!(
            error.message(),
            "to_int で Int に変換できません: i64 範囲外"
        );
    }
}
