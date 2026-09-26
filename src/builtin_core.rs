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
                Err(TsumugiError::runtime_with_kind(
                    line,
                    crate::error::ErrorKind::Argument,
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

pub fn builtin_sort(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("sort", args, 1, line)?;
    if let Value::List(list) = &args[0] {
        let mut sorted = (**list).clone();
        sorted.sort_by_key(|a| a.to_string());
        Ok(Value::List(Tracked::constant(sorted)))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line, "sort", 1, "List", &args[0],
        ))
    }
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

pub fn builtin_join(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("join", args, 2, line)?;
    let Value::List(list) = &args[0] else {
        return Err(TsumugiError::builtin_arg_type(
            line, "join", 1, "List", &args[0],
        ));
    };
    let sep = require_str(&args[1], "join", 2, line)?;
    let parts: Vec<String> = list.iter().map(|v| v.to_string()).collect();
    Ok(Value::str_constant(parts.join(sep)))
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
fn checked_float_to_i64(
    value: f64,
    mode: RoundMode,
    builtin: &str,
    line: usize,
) -> Result<i64, TsumugiError> {
    if value.is_nan() {
        return Err(TsumugiError::runtime_with_kind(
            line,
            crate::error::ErrorKind::Conversion,
            format!("{builtin} で Int に変換できません: NaN"),
        ));
    }
    if value.is_infinite() {
        return Err(TsumugiError::runtime_with_kind(
            line,
            crate::error::ErrorKind::Conversion,
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
        return Err(TsumugiError::runtime_with_kind(
            line,
            crate::error::ErrorKind::Conversion,
            format!("{builtin} で Int に変換できません: i64 範囲外"),
        ));
    }

    Ok(rounded as i64)
}

/// OS が返す `u64` のファイルサイズを i64 へ変換する。`i64::MAX` を超える場合は
/// wrap・負値を返さず `int_overflow` エラーにする（AUD-036）。
fn checked_file_size_to_i64(size: u64, line: usize) -> Result<i64, TsumugiError> {
    i64::try_from(size).map_err(|_| {
        TsumugiError::runtime_with_kind(
            line,
            crate::error::ErrorKind::IntOverflow,
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
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Conversion,
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

pub fn builtin_to_str(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("to_str", args, 1, line)?;
    Ok(Value::str_constant(args[0].to_string()))
}

pub fn builtin_to_float(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("to_float", args, 1, line)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Int(n) => Ok(Value::Float(*n as f64)),
        Value::Str(s) => s.parse::<f64>().map(Value::Float).map_err(|_| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Conversion,
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
        Value::Float(f) => Ok(Value::Float(f.abs())),
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
/// ときは canonical NaN（`Float(f64::NAN)`）を返す。
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
        Some(NumericOrdering::UnorderedNaN) | None => Ok(Value::Float(f64::NAN)),
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

pub fn builtin_read_file(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("read_file", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_path(path, line)?;
        match std::fs::read_to_string(&safe_path) {
            Ok(content) => Ok(Value::str_constant(content)),
            Err(_) => Ok(Value::Null),
        }
    } else {
        Err(TsumugiError::builtin_arg_type(
            line,
            "read_file",
            1,
            "Str",
            &args[0],
        ))
    }
}

pub fn builtin_read_lines(
    args: &[Value],
    max_collection: u64,
    line: usize,
) -> Result<Value, TsumugiError> {
    check_arity("read_lines", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_path(path, line)?;
        match std::fs::read_to_string(&safe_path) {
            Ok(content) => {
                let mut lines = Vec::new();
                for content_line in content.lines() {
                    check_collection_size(lines.len().saturating_add(1), max_collection, line)?;
                    lines.push(Value::str_constant(content_line.to_string()));
                }
                Ok(Value::List(Tracked::constant(lines)))
            }
            Err(_) => Ok(Value::Null),
        }
    } else {
        Err(TsumugiError::builtin_arg_type(
            line,
            "read_lines",
            1,
            "Str",
            &args[0],
        ))
    }
}

pub fn builtin_write_file(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("write_file", args, 2, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_path(path, line)?;
        let content = match &args[1] {
            Value::Str(s) => s.to_string(),
            other => other.to_string(),
        };
        Ok(Value::Bool(std::fs::write(&safe_path, &content).is_ok()))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line,
            "write_file",
            1,
            "Str",
            &args[0],
        ))
    }
}

pub fn builtin_append_file(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("append_file", args, 2, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_path(path, line)?;
        let content = match &args[1] {
            Value::Str(s) => s.to_string(),
            other => other.to_string(),
        };
        use std::fs::OpenOptions;
        use std::io::Write;
        let result = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&safe_path)
            .and_then(|mut f| f.write_all(content.as_bytes()));
        Ok(Value::Bool(result.is_ok()))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line,
            "append_file",
            1,
            "Str",
            &args[0],
        ))
    }
}

// =============================================================================
// 環境系
// =============================================================================

/// 環境変数アクセス許可リスト（`TSUMUGI_ENV_ALLOW` で制御）
/// 未設定 → 全キー許可、設定 → リスト内のキーのみ許可
static ENV_ALLOW: std::sync::OnceLock<Option<Vec<String>>> = std::sync::OnceLock::new();

fn env_allowed_keys() -> &'static Option<Vec<String>> {
    ENV_ALLOW.get_or_init(|| {
        let val = std::env::var("TSUMUGI_ENV_ALLOW").ok()?;
        if val.is_empty() {
            return None;
        }
        let keys: Vec<String> = val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Some(keys)
    })
}

fn is_env_key_allowed(key: &str) -> bool {
    let Some(allowed) = env_allowed_keys() else {
        // 許可リスト未設定 → 全キー許可
        return true;
    };
    for pattern in allowed {
        if pattern.ends_with('*') {
            // プレフィックスマッチ（例: "TSUMUGI_*"）
            let prefix = &pattern[..pattern.len() - 1];
            if key.starts_with(prefix) {
                return true;
            }
        } else if pattern == key {
            return true;
        }
    }
    false
}

fn is_protected_env_key(key: &str) -> bool {
    const PREFIX: &str = "TSUMUGI_";

    #[cfg(windows)]
    {
        // Windowsのcase-insensitive lookupでASCII名へ別名解決され得る
        // Unicode文字（long s、dotless i等）も保護側へ倒す。
        key.to_uppercase().starts_with(PREFIX)
    }

    #[cfg(not(windows))]
    {
        key.starts_with(PREFIX)
    }
}

/// ambient 互換経路（[`crate::capability::CapabilitySet::ambient_compat`]）用の
/// 環境変数 snapshot を作る（Phase 2 C3）。
///
/// ambient 経路の唯一の process env 読み取りをここへ集約する。process env を 1 度だけ走査し、
/// legacy の allow-list（`TSUMUGI_ENV_ALLOW`）と `TSUMUGI_` 保護を適用した visible key だけを
/// [`EnvironmentSnapshot`] に載せる。これにより `env()` builtin 自体は snapshot だけを読み、
/// core builtin からの ambient read を 0 にする（CAP-AT-27）。
///
/// snapshot key は UTF-8・1..=256 bytes・NUL なしが要件のため、これを満たさない key は
/// 従来の live-read でも `env()` へ渡せない（key は script 由来の Str）ので落として問題ない。
/// value 側の分類は ambient 互換なので [`DataClassification::Public`] とする。
pub fn ambient_environment_snapshot() -> crate::capability::EnvironmentSnapshot {
    use crate::capability::{DataClassification, EnvironmentSnapshot, EnvironmentValue};

    let entries = std::env::vars().filter_map(|(key, value)| {
        if is_protected_env_key(&key) || !is_env_key_allowed(&key) {
            return None;
        }
        // Windows の process env lookup は case-insensitive で、legacy の `env()` は
        // `std::env::var` 経由で `env("PATH")` から OS の `Path` を引けた。exact-match の
        // snapshot でこの ambient 挙動を保つため、Windows では key を大文字へ正規化する
        // （script 側の key も後段で同様に正規化する）。他 OS は case-sensitive のまま。
        let key = normalize_ambient_env_key(&key);
        // key/value 検証（長さ・NUL）を満たさないものは snapshot から落とす。
        if key.is_empty() || key.len() > 256 || key.as_bytes().contains(&0) {
            return None;
        }
        let value = EnvironmentValue::new(value, DataClassification::Public).ok()?;
        Some((key, value))
    });
    // from_entries は重複 key で error になるが、process env の key は一意なので握り潰す。
    EnvironmentSnapshot::from_entries(entries).unwrap_or_else(|_| EnvironmentSnapshot::empty())
}

/// ambient 経路の環境変数 key を OS 規則で正規化する。
///
/// Windows の process env lookup は case-insensitive なので、snapshot 構築時・lookup 時とも
/// key を大文字化してこの ambient 挙動を保つ（legacy の `std::env::var` 相当）。他 OS は
/// case-sensitive なのでそのまま返す。`is_protected_env_key` の Windows 大文字化と整合する。
fn normalize_ambient_env_key(key: &str) -> String {
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
    line: usize,
) -> Result<Value, TsumugiError> {
    use crate::capability::{FsOperation, OpenFileRequest, RemoveKind, WriteMode};

    match name {
        "read_file" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let mut file = fs_open(name, filesystem, path, OpenFileRequest::ReadExisting, line)?;
            let bytes = file
                .read_to_end(None)
                .map_err(|_| fs_host_error(name, line))?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            Ok(Value::str_constant(text))
        }
        "read_lines" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let mut file = fs_open(name, filesystem, path, OpenFileRequest::ReadExisting, line)?;
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
            )?;
            file.write_all(content.as_bytes())
                .map_err(|_| fs_host_error(name, line))?;
            Ok(Value::Bool(true))
        }
        "mkdir" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let (root, target) = fs_route(name, filesystem, path, FsOperation::Create, line)?;
            root.adapter
                .create_dir(&target.path)
                .map_err(|_| fs_host_error(name, line))?;
            Ok(Value::Bool(true))
        }
        "remove" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let (root, target) = fs_route(name, filesystem, path, FsOperation::Delete, line)?;
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
            let (root, target) = fs_route(name, filesystem, path, FsOperation::Delete, line)?;
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
            let (root, target) =
                fs_route(name, filesystem, path, FsOperation::RecursiveDelete, line)?;
            root.adapter
                .remove(&target.path, RemoveKind::Tree)
                .map_err(|_| fs_host_error(name, line))?;
            Ok(Value::Bool(true))
        }
        "rename" => {
            check_arity(name, args, 2, line)?;
            let from = require_str(&args[0], name, 1, line)?;
            let to = require_str(&args[1], name, 2, line)?;
            // rename は source `Delete` と destination `Create` を要求する（第8.4節）。
            let (from_root, from_target) =
                fs_route(name, filesystem, from, FsOperation::Delete, line)?;
            let (to_root, to_target) = fs_route(name, filesystem, to, FsOperation::Create, line)?;
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
        }
        "list_dir" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let (root, target) = fs_route(name, filesystem, path, FsOperation::List, line)?;
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
            names.sort_by_key(|v| v.to_string());
            Ok(Value::List(Tracked::constant(names)))
        }
        "file_size" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let (root, target) = fs_route(name, filesystem, path, FsOperation::Metadata, line)?;
            let meta = root
                .adapter
                .metadata(&target.path, true)
                .map_err(|_| fs_host_error(name, line))?;
            Ok(Value::Int(checked_file_size_to_i64(meta.size_bytes, line)?))
        }
        "is_file" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let kind = fs_metadata_kind(name, filesystem, path, line)?;
            Ok(Value::Bool(matches!(
                kind,
                Some(crate::capability::EntryKind::File)
            )))
        }
        "is_dir" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let kind = fs_metadata_kind(name, filesystem, path, line)?;
            Ok(Value::Bool(matches!(
                kind,
                Some(crate::capability::EntryKind::Directory)
            )))
        }
        "path_exists" => {
            check_arity(name, args, 1, line)?;
            let path = require_str(&args[0], name, 1, line)?;
            let kind = fs_metadata_kind(name, filesystem, path, line)?;
            Ok(Value::Bool(kind.is_some()))
        }
        // is_filesystem_builtin と本 match は同じ名前集合を持つ。ここへ来ない。
        _ => Err(TsumugiError::filesystem_denied(line)),
    }
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
) -> Result<
    (
        &'a crate::capability::FilesystemRoot,
        crate::capability::FilesystemTarget,
    ),
    TsumugiError,
> {
    let _ = name;
    // lexical 検証を capability lookup より先に行う（PathError は catch 可能な argument error。
    // authority channel と分離する、第8.5節）。message に host path を含めない。
    let target = crate::capability::FilesystemTarget::parse(path).map_err(|_| {
        TsumugiError::runtime_with_kind(
            line,
            crate::error::ErrorKind::Argument,
            "ファイルパスの形式が不正です",
        )
    })?;
    // mount 完全一致で root を引く。未登録は単一の sandbox denial（存在 oracle 防止）。
    let Some(root) = filesystem.root(&target.mount) else {
        return Err(TsumugiError::filesystem_denied(line));
    };
    // operation が grant されていなければ adapter call 0 で denial。
    if !root.operations.contains(&operation) {
        return Err(TsumugiError::filesystem_denied(line));
    }
    Ok((root, target))
}

/// path を route してから file を open する（read/write 系）。
fn fs_open(
    name: &str,
    filesystem: &crate::capability::FilesystemCapability,
    path: &str,
    request: crate::capability::OpenFileRequest,
    line: usize,
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
    let (root, target) = fs_route(name, filesystem, path, operation, line)?;
    // Upsert は Write+Create を事前要求する（第8.4節）。route では Write を検査済みなので、
    // 追加で Create の grant を確認する（不足なら adapter call 0 の sandbox denial）。
    if matches!(request, OpenFileRequest::Upsert { .. })
        && !root.operations.contains(&FsOperation::Create)
    {
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
) -> Result<Option<crate::capability::EntryKind>, TsumugiError> {
    use crate::capability::FsOperation;
    let (root, target) = fs_route(name, filesystem, path, FsOperation::Metadata, line)?;
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

pub fn builtin_path_exists(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("path_exists", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_path(path, line)?;
        Ok(Value::Bool(safe_path.exists()))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line,
            "path_exists",
            1,
            "Str",
            &args[0],
        ))
    }
}

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

pub fn builtin_mkdir(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("mkdir", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_path(path, line)?;
        Ok(Value::Bool(std::fs::create_dir_all(&safe_path).is_ok()))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line, "mkdir", 1, "Str", &args[0],
        ))
    }
}

fn remove_symlink_entry(path: &std::path::Path) -> std::io::Result<()> {
    // Unixではremove_file、Windowsのdirectory symlink/junctionではremove_dirが必要な場合がある。
    // どちらもfinal entry pathへ適用し、link targetは操作しない。
    std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))
}

pub fn builtin_remove(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("remove", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_entry_path(path, line)?;
        let result = match std::fs::symlink_metadata(&safe_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => remove_symlink_entry(&safe_path),
            Ok(metadata) if metadata.is_dir() => std::fs::remove_dir(&safe_path),
            _ => std::fs::remove_file(&safe_path),
        };
        Ok(Value::Bool(result.is_ok()))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line, "remove", 1, "Str", &args[0],
        ))
    }
}

/// `remove_dir(path)`: **空 directory のみ**を削除する（REV-021 §17.6）。
///
/// 非空 directory は削除しない（従来の再帰削除は `remove_tree` へ分離した）。symlink の削除は
/// `remove`（`FileOrSymlink`）の責務で、`remove_dir` は扱わない。ambient（legacy）経路では
/// 従来どおり成否を `Bool` で返す（非空・不存在は `false`）。capability 経路は
/// `EmptyDirectory` を要求し、非空は catch 可能な `host` error（category `directory_not_empty`）
/// へ写す（[`dispatch_filesystem_capability`]）。
pub fn builtin_remove_dir(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("remove_dir", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_entry_path(path, line)?;
        // 空 directory のみ削除する（非空は OS error → false）。
        Ok(Value::Bool(std::fs::remove_dir(&safe_path).is_ok()))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line,
            "remove_dir",
            1,
            "Str",
            &args[0],
        ))
    }
}

/// `remove_tree(path)`: directory を**再帰削除**する（REV-021 §17.6 で `remove_dir` から分離）。
///
/// final entry が symlink ならリンク自体を削除し、リンク先を辿って再帰削除しない。ambient
/// （legacy）経路では成否を `Bool` で返す。capability 経路は専用 capability `RecursiveDelete`
/// を要求する（`EmptyDirectory` から暗黙昇格させない）。budget / cancel / audit は Phase 3/4/6。
pub fn builtin_remove_tree(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("remove_tree", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_entry_path(path, line)?;
        let result = match std::fs::symlink_metadata(&safe_path) {
            // final symlink はリンク自体を削除する（リンク先を辿らない）。
            Ok(metadata) if metadata.file_type().is_symlink() => remove_symlink_entry(&safe_path),
            _ => std::fs::remove_dir_all(&safe_path),
        };
        Ok(Value::Bool(result.is_ok()))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line,
            "remove_tree",
            1,
            "Str",
            &args[0],
        ))
    }
}

pub fn builtin_rename(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("rename", args, 2, line)?;
    let from = require_str(&args[0], "rename", 1, line)?;
    let to = require_str(&args[1], "rename", 2, line)?;
    let safe_from = crate::sandbox::check_entry_path(from, line)?;
    let safe_to = crate::sandbox::check_entry_path(to, line)?;
    Ok(Value::Bool(std::fs::rename(&safe_from, &safe_to).is_ok()))
}

/// `list_dir(path)`: ambient（legacy）経路の directory 列挙。
///
/// REV-009 §17.4 の **safe** 挙動（個別 entry 取得失敗→`directory_read` host error、非 UTF-8 名
/// →`invalid_encoding` host error、部分結果を成功 List にしない）は capability 経路
/// （[`dispatch_filesystem_capability`]）で提供する。この ambient 経路は legacy 互換のまま——
/// 個別 entry 失敗は skip（`flatten`）、非 UTF-8 名は lossy 変換、`read_dir` 失敗は `null`。
/// capability denial は sandbox 側で処理し `null` へ畳まない。
pub fn builtin_list_dir(
    args: &[Value],
    max_collection: u64,
    line: usize,
) -> Result<Value, TsumugiError> {
    check_arity("list_dir", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_path(path, line)?;
        match std::fs::read_dir(&safe_path) {
            Ok(entries) => {
                let mut names = Vec::new();
                // legacy: 個別 entry 失敗は skip（safe 経路は directory_read へ写す、REV-009）。
                for entry in entries.flatten() {
                    check_collection_size(names.len().saturating_add(1), max_collection, line)?;
                    names.push(Value::str_constant(
                        entry.file_name().to_string_lossy().to_string(),
                    ));
                }
                names.sort_by_key(|v| v.to_string());
                Ok(Value::List(Tracked::constant(names)))
            }
            Err(_) => Ok(Value::Null),
        }
    } else {
        Err(TsumugiError::builtin_arg_type(
            line, "list_dir", 1, "Str", &args[0],
        ))
    }
}

pub fn builtin_file_size(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("file_size", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_path(path, line)?;
        match std::fs::metadata(&safe_path) {
            Ok(meta) => Ok(Value::Int(checked_file_size_to_i64(meta.len(), line)?)),
            Err(_) => Ok(Value::Null),
        }
    } else {
        Err(TsumugiError::builtin_arg_type(
            line,
            "file_size",
            1,
            "Str",
            &args[0],
        ))
    }
}

pub fn builtin_is_file(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("is_file", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_path(path, line)?;
        Ok(Value::Bool(safe_path.is_file()))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line, "is_file", 1, "Str", &args[0],
        ))
    }
}

pub fn builtin_is_dir(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("is_dir", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        let safe_path = crate::sandbox::check_path(path, line)?;
        Ok(Value::Bool(safe_path.is_dir()))
    } else {
        Err(TsumugiError::builtin_arg_type(
            line, "is_dir", 1, "Str", &args[0],
        ))
    }
}

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
        "sort" => builtin_sort(args, line)?,
        "reverse" => builtin_reverse(args, line)?,
        "range" => builtin_range(args, max_collection, line)?,
        "split" => builtin_split(args, max_collection, line)?,
        "join" => builtin_join(args, line)?,
        "trim" => builtin_trim(args, line)?,
        "upper" => builtin_upper(args, line)?,
        "lower" => builtin_lower(args, line)?,
        "starts_with" => builtin_starts_with(args, line)?,
        "ends_with" => builtin_ends_with(args, line)?,
        "replace" => builtin_replace(args, line)?,
        "to_int" => builtin_to_int(args, line)?,
        "to_str" => builtin_to_str(args, line)?,
        "to_float" => builtin_to_float(args, line)?,
        "abs" => builtin_abs(args, line)?,
        "min" => builtin_min(args, line)?,
        "max" => builtin_max(args, line)?,
        "floor" => builtin_floor(args, line)?,
        "ceil" => builtin_ceil(args, line)?,
        "round" => builtin_round(args, line)?,
        "format_time" => builtin_format_time(args, line)?,
        "read_file" => builtin_read_file(args, line)?,
        "read_lines" => builtin_read_lines(args, max_collection, line)?,
        "write_file" => builtin_write_file(args, line)?,
        "append_file" => builtin_append_file(args, line)?,
        "path_exists" => builtin_path_exists(args, line)?,
        "path_join" => builtin_path_join(args, line)?,
        "mkdir" => builtin_mkdir(args, line)?,
        "remove" => builtin_remove(args, line)?,
        "remove_dir" => builtin_remove_dir(args, line)?,
        "remove_tree" => builtin_remove_tree(args, line)?,
        "rename" => builtin_rename(args, line)?,
        "list_dir" => builtin_list_dir(args, max_collection, line)?,
        "file_size" => builtin_file_size(args, line)?,
        "is_file" => builtin_is_file(args, line)?,
        "is_dir" => builtin_is_dir(args, line)?,
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
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
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
