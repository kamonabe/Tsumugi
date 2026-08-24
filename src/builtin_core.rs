//! 組み込み関数の共通ロジック
//!
//! ツリーウォーク評価器 (builtin.rs) と VM (vm.rs) の両方から呼び出される。
//! 引数は評価済みの `&[Value]` で受け取り、副作用のない純粋な値変換を行う。
//!
//! 注意: I/O・クロージャ呼び出し（map/filter/each）・ファイル操作など
//! 実行コンテキストに依存するビルトインは各エンジン側に残す。

use crate::error::TsumugiError;
use crate::value::Value;

// =============================================================================
// ユーティリティ
// =============================================================================

pub fn check_arity(
    name: &str,
    args: &[Value],
    expected: usize,
    line: usize,
) -> Result<(), TsumugiError> {
    if args.len() != expected {
        Err(TsumugiError::Runtime {
            line,
            message: format!(
                "{}: 引数の数が合いません: {}個必要ですが{}個渡されました",
                name,
                expected,
                args.len()
            ),
            trace: Vec::new(),
        })
    } else {
        Ok(())
    }
}

pub fn type_error(line: usize, msg: &str) -> TsumugiError {
    TsumugiError::Runtime {
        line,
        message: msg.to_string(),
        trace: Vec::new(),
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
        _ => Err(type_error(
            line,
            "len は List/Str/Dict に対してのみ使えます",
        )),
    }
}

pub fn builtin_push(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("push", args, 2, line)?;
    let mut list = args[0].clone();
    if let Value::List(ref mut v) = list {
        v.push(args[1].clone());
        Ok(list)
    } else {
        Err(type_error(line, "push はリストに対してのみ使えます"))
    }
}

pub fn builtin_pop(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("pop", args, 1, line)?;
    let mut list = args[0].clone();
    if let Value::List(ref mut v) = list {
        if v.is_empty() {
            Err(TsumugiError::Runtime {
                line,
                message: "pop: 空のリストからは取り出せません".to_string(),
                trace: Vec::new(),
            })
        } else {
            Ok(v.pop().unwrap())
        }
    } else {
        Err(type_error(line, "pop はリストに対してのみ使えます"))
    }
}

pub fn builtin_pop_update(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("__pop_update", args, 1, line)?;
    let mut list = args[0].clone();
    if let Value::List(ref mut v) = list {
        if !v.is_empty() {
            v.pop();
        }
        Ok(list)
    } else {
        Ok(args[0].clone())
    }
}

pub fn builtin_keys(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("keys", args, 1, line)?;
    if let Value::Dict(map) = &args[0] {
        let keys: Vec<Value> = map.keys().map(|k| Value::Str(k.clone())).collect();
        Ok(Value::List(keys))
    } else {
        Err(type_error(line, "keys は辞書に対してのみ使えます"))
    }
}

pub fn builtin_values(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("values", args, 1, line)?;
    if let Value::Dict(map) = &args[0] {
        let vals: Vec<Value> = map.values().cloned().collect();
        Ok(Value::List(vals))
    } else {
        Err(type_error(line, "values は辞書に対してのみ使えます"))
    }
}

pub fn builtin_has_key(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("has_key", args, 2, line)?;
    if let (Value::Dict(map), Value::Str(key)) = (&args[0], &args[1]) {
        Ok(Value::Bool(map.contains_key(key)))
    } else {
        Err(type_error(line, "has_key(dict, str) の形式で使います"))
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
    Ok(Value::Str(t.to_string()))
}

pub fn builtin_slice(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("slice", args, 3, line)?;
    let (Value::Int(start), Value::Int(end)) = (&args[1], &args[2]) else {
        return Err(type_error(line, "slice の開始・終了は整数で指定します"));
    };
    let start = *start as usize;
    let end = *end as usize;
    match &args[0] {
        Value::List(v) => {
            let s = start.min(v.len());
            let e = end.min(v.len());
            Ok(Value::List(v[s..e].to_vec()))
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let st = start.min(chars.len());
            let en = end.min(chars.len());
            Ok(Value::Str(chars[st..en].iter().collect()))
        }
        _ => Err(type_error(line, "slice は List/Str に対してのみ使えます")),
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
                Ok(Value::Bool(map.contains_key(key)))
            } else {
                Ok(Value::Bool(false))
            }
        }
        _ => Err(type_error(
            line,
            "contains は List/Str/Dict に対してのみ使えます",
        )),
    }
}

pub fn builtin_sort(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("sort", args, 1, line)?;
    if let Value::List(list) = &args[0] {
        let mut sorted = list.clone();
        sorted.sort_by_key(|a| a.to_string());
        Ok(Value::List(sorted))
    } else {
        Err(type_error(line, "sort はリストに対してのみ使えます"))
    }
}

pub fn builtin_reverse(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("reverse", args, 1, line)?;
    match &args[0] {
        Value::List(list) => {
            let mut rev = list.clone();
            rev.reverse();
            Ok(Value::List(rev))
        }
        Value::Str(s) => Ok(Value::Str(s.chars().rev().collect())),
        _ => Err(type_error(line, "reverse は List/Str に対してのみ使えます")),
    }
}

pub fn builtin_range(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("range", args, 2, line)?;
    if let (Value::Int(start), Value::Int(end)) = (&args[0], &args[1]) {
        let list: Vec<Value> = (*start..*end).map(Value::Int).collect();
        Ok(Value::List(list))
    } else {
        Err(type_error(line, "range(int, int) の形式で使います"))
    }
}

// =============================================================================
// 文字列操作系
// =============================================================================

pub fn builtin_split(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("split", args, 2, line)?;
    if let (Value::Str(s), Value::Str(sep)) = (&args[0], &args[1]) {
        let parts: Vec<Value> = s
            .split(sep.as_str())
            .map(|p| Value::Str(p.to_string()))
            .collect();
        Ok(Value::List(parts))
    } else {
        Err(type_error(line, "split(str, str) の形式で使います"))
    }
}

pub fn builtin_join(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("join", args, 2, line)?;
    if let (Value::List(list), Value::Str(sep)) = (&args[0], &args[1]) {
        let parts: Vec<String> = list.iter().map(|v| v.to_string()).collect();
        Ok(Value::Str(parts.join(sep)))
    } else {
        Err(type_error(line, "join(list, str) の形式で使います"))
    }
}

pub fn builtin_trim(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("trim", args, 1, line)?;
    if let Value::Str(s) = &args[0] {
        Ok(Value::Str(s.trim().to_string()))
    } else {
        Err(type_error(line, "trim は文字列に対してのみ使えます"))
    }
}

pub fn builtin_upper(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("upper", args, 1, line)?;
    if let Value::Str(s) = &args[0] {
        Ok(Value::Str(s.to_uppercase()))
    } else {
        Err(type_error(line, "upper は文字列に対してのみ使えます"))
    }
}

pub fn builtin_lower(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("lower", args, 1, line)?;
    if let Value::Str(s) = &args[0] {
        Ok(Value::Str(s.to_lowercase()))
    } else {
        Err(type_error(line, "lower は文字列に対してのみ使えます"))
    }
}

pub fn builtin_starts_with(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("starts_with", args, 2, line)?;
    if let (Value::Str(s), Value::Str(prefix)) = (&args[0], &args[1]) {
        Ok(Value::Bool(s.starts_with(prefix.as_str())))
    } else {
        Err(type_error(line, "starts_with(str, str) の形式で使います"))
    }
}

pub fn builtin_ends_with(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("ends_with", args, 2, line)?;
    if let (Value::Str(s), Value::Str(suffix)) = (&args[0], &args[1]) {
        Ok(Value::Bool(s.ends_with(suffix.as_str())))
    } else {
        Err(type_error(line, "ends_with(str, str) の形式で使います"))
    }
}

pub fn builtin_replace(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("replace", args, 3, line)?;
    if let (Value::Str(s), Value::Str(old), Value::Str(new)) = (&args[0], &args[1], &args[2]) {
        Ok(Value::Str(s.replace(old.as_str(), new.as_str())))
    } else {
        Err(type_error(line, "replace(str, str, str) の形式で使います"))
    }
}

// =============================================================================
// 型変換・数値系
// =============================================================================

pub fn builtin_to_int(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("to_int", args, 1, line)?;
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(f) => Ok(Value::Int(*f as i64)),
        Value::Bool(b) => Ok(Value::Int(if *b { 1 } else { 0 })),
        Value::Str(s) => s
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| TsumugiError::Runtime {
                line,
                message: format!("to_int: 変換失敗: \"{}\"", s),
                trace: Vec::new(),
            }),
        _ => Err(type_error(line, "to_int: 変換できない型です")),
    }
}

pub fn builtin_to_str(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("to_str", args, 1, line)?;
    Ok(Value::Str(args[0].to_string()))
}

pub fn builtin_to_float(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("to_float", args, 1, line)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Int(n) => Ok(Value::Float(*n as f64)),
        Value::Str(s) => s
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| TsumugiError::Runtime {
                line,
                message: format!("to_float: 変換失敗: \"{}\"", s),
                trace: Vec::new(),
            }),
        _ => Err(type_error(line, "to_float: 変換できない型です")),
    }
}

pub fn builtin_abs(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("abs", args, 1, line)?;
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(n.abs())),
        Value::Float(f) => Ok(Value::Float(f.abs())),
        _ => Err(type_error(line, "abs は数値に対してのみ使えます")),
    }
}

pub fn builtin_min(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("min", args, 2, line)?;
    match (&args[0], &args[1]) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(*a.min(b))),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.min(*b))),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float((*a as f64).min(*b))),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a.min(*b as f64))),
        _ => Err(type_error(line, "min は数値に対してのみ使えます")),
    }
}

pub fn builtin_max(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("max", args, 2, line)?;
    match (&args[0], &args[1]) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(*a.max(b))),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a.max(*b))),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float((*a as f64).max(*b))),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a.max(*b as f64))),
        _ => Err(type_error(line, "max は数値に対してのみ使えます")),
    }
}

pub fn builtin_floor(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("floor", args, 1, line)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Int(f.floor() as i64)),
        Value::Int(n) => Ok(Value::Int(*n)),
        _ => Err(type_error(line, "floor は数値に対してのみ使えます")),
    }
}

pub fn builtin_ceil(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("ceil", args, 1, line)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Int(f.ceil() as i64)),
        Value::Int(n) => Ok(Value::Int(*n)),
        _ => Err(type_error(line, "ceil は数値に対してのみ使えます")),
    }
}

pub fn builtin_round(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("round", args, 1, line)?;
    match &args[0] {
        Value::Float(f) => Ok(Value::Int(f.round() as i64)),
        Value::Int(n) => Ok(Value::Int(*n)),
        _ => Err(type_error(line, "round は数値に対してのみ使えます")),
    }
}

// =============================================================================
// 日時系
// =============================================================================

pub fn builtin_now(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("now", args, 0, line)?;
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    Ok(Value::Int(secs))
}

pub fn builtin_format_time(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("format_time", args, 2, line)?;
    if let (Value::Int(ts), Value::Str(fmt)) = (&args[0], &args[1]) {
        Ok(Value::Str(format_unix_timestamp(*ts, fmt)))
    } else {
        Err(type_error(line, "format_time(int, str) の形式で使います"))
    }
}

// =============================================================================
// ファイルI/O系（サンドボックスチェック付き）
// =============================================================================

pub fn builtin_read_file(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("read_file", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        match std::fs::read_to_string(path) {
            Ok(content) => Ok(Value::Str(content)),
            Err(_) => Ok(Value::Null),
        }
    } else {
        Err(type_error(line, "read_file(str) の形式で使います"))
    }
}

pub fn builtin_read_lines(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("read_lines", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let lines: Vec<Value> =
                    content.lines().map(|l| Value::Str(l.to_string())).collect();
                Ok(Value::List(lines))
            }
            Err(_) => Ok(Value::Null),
        }
    } else {
        Err(type_error(line, "read_lines(str) の形式で使います"))
    }
}

pub fn builtin_write_file(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("write_file", args, 2, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        let content = match &args[1] {
            Value::Str(s) => s.clone(),
            other => other.to_string(),
        };
        Ok(Value::Bool(std::fs::write(path, &content).is_ok()))
    } else {
        Err(type_error(
            line,
            "write_file(str, content) の形式で使います",
        ))
    }
}

pub fn builtin_append_file(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("append_file", args, 2, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        let content = match &args[1] {
            Value::Str(s) => s.clone(),
            other => other.to_string(),
        };
        use std::fs::OpenOptions;
        use std::io::Write;
        let result = OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .and_then(|mut f| f.write_all(content.as_bytes()));
        Ok(Value::Bool(result.is_ok()))
    } else {
        Err(type_error(
            line,
            "append_file(str, content) の形式で使います",
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

pub fn builtin_env(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("env", args, 1, line)?;
    if let Value::Str(key) = &args[0] {
        // ランタイム制御用の環境変数はスクリプトからアクセス不可
        if key.starts_with("TSUMUGI_") {
            return Ok(Value::Null);
        }
        if !is_env_key_allowed(key) {
            // 許可リスト外のキーへのアクセスは null を返す（エラーにはしない）
            return Ok(Value::Null);
        }
        match std::env::var(key) {
            Ok(val) => Ok(Value::Str(val)),
            Err(_) => Ok(Value::Null),
        }
    } else {
        Err(type_error(line, "env(str) の形式で使います"))
    }
}

// =============================================================================
// パス・ファイルシステム系
// =============================================================================

pub fn builtin_path_exists(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("path_exists", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        Ok(Value::Bool(std::path::Path::new(path).exists()))
    } else {
        Err(type_error(line, "path_exists(str) の形式で使います"))
    }
}

pub fn builtin_path_join(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    let _ = line;
    let mut path = std::path::PathBuf::new();
    for arg in args {
        if let Value::Str(s) = arg {
            path.push(s);
        }
    }
    Ok(Value::Str(path.to_string_lossy().to_string()))
}

pub fn builtin_mkdir(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("mkdir", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        Ok(Value::Bool(std::fs::create_dir_all(path).is_ok()))
    } else {
        Err(type_error(line, "mkdir(str) の形式で使います"))
    }
}

pub fn builtin_remove(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("remove", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        let p = std::path::Path::new(path);
        let result = if p.is_dir() {
            std::fs::remove_dir(path)
        } else {
            std::fs::remove_file(path)
        };
        Ok(Value::Bool(result.is_ok()))
    } else {
        Err(type_error(line, "remove(str) の形式で使います"))
    }
}

pub fn builtin_remove_dir(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("remove_dir", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        Ok(Value::Bool(std::fs::remove_dir_all(path).is_ok()))
    } else {
        Err(type_error(line, "remove_dir(str) の形式で使います"))
    }
}

pub fn builtin_rename(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("rename", args, 2, line)?;
    if let (Value::Str(from), Value::Str(to)) = (&args[0], &args[1]) {
        crate::sandbox::check_path(from, line)?;
        crate::sandbox::check_path(to, line)?;
        Ok(Value::Bool(std::fs::rename(from, to).is_ok()))
    } else {
        Err(type_error(line, "rename(str, str) の形式で使います"))
    }
}

pub fn builtin_list_dir(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("list_dir", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        match std::fs::read_dir(path) {
            Ok(entries) => {
                let mut names: Vec<Value> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| Value::Str(e.file_name().to_string_lossy().to_string()))
                    .collect();
                names.sort_by_key(|v| v.to_string());
                Ok(Value::List(names))
            }
            Err(_) => Ok(Value::Null),
        }
    } else {
        Err(type_error(line, "list_dir(str) の形式で使います"))
    }
}

pub fn builtin_file_size(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("file_size", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        match std::fs::metadata(path) {
            Ok(meta) => Ok(Value::Int(meta.len() as i64)),
            Err(_) => Ok(Value::Null),
        }
    } else {
        Err(type_error(line, "file_size(str) の形式で使います"))
    }
}

pub fn builtin_is_file(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("is_file", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        Ok(Value::Bool(std::path::Path::new(path).is_file()))
    } else {
        Err(type_error(line, "is_file(str) の形式で使います"))
    }
}

pub fn builtin_is_dir(args: &[Value], line: usize) -> Result<Value, TsumugiError> {
    check_arity("is_dir", args, 1, line)?;
    if let Value::Str(path) = &args[0] {
        crate::sandbox::check_path(path, line)?;
        Ok(Value::Bool(std::path::Path::new(path).is_dir()))
    } else {
        Err(type_error(line, "is_dir(str) の形式で使います"))
    }
}

// =============================================================================
// ディスパッチ関数
// =============================================================================

/// 共通化されたビルトインを名前で呼び出す。
/// 該当すれば Ok(Some(value))、該当しなければ Ok(None) を返す。
/// map/filter/each/print/input/exit/args はコンテキスト依存のため含まない。
pub fn dispatch(name: &str, args: &[Value], line: usize) -> Result<Option<Value>, TsumugiError> {
    let result = match name {
        "len" => builtin_len(args, line)?,
        "push" => builtin_push(args, line)?,
        "pop" => builtin_pop(args, line)?,
        "__pop_update" => builtin_pop_update(args, line)?,
        "keys" => builtin_keys(args, line)?,
        "values" => builtin_values(args, line)?,
        "has_key" => builtin_has_key(args, line)?,
        "type" => builtin_type(args, line)?,
        "slice" => builtin_slice(args, line)?,
        "contains" => builtin_contains(args, line)?,
        "sort" => builtin_sort(args, line)?,
        "reverse" => builtin_reverse(args, line)?,
        "range" => builtin_range(args, line)?,
        "split" => builtin_split(args, line)?,
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
        "now" => builtin_now(args, line)?,
        "format_time" => builtin_format_time(args, line)?,
        "read_file" => builtin_read_file(args, line)?,
        "read_lines" => builtin_read_lines(args, line)?,
        "write_file" => builtin_write_file(args, line)?,
        "append_file" => builtin_append_file(args, line)?,
        "env" => builtin_env(args, line)?,
        "path_exists" => builtin_path_exists(args, line)?,
        "path_join" => builtin_path_join(args, line)?,
        "mkdir" => builtin_mkdir(args, line)?,
        "remove" => builtin_remove(args, line)?,
        "remove_dir" => builtin_remove_dir(args, line)?,
        "rename" => builtin_rename(args, line)?,
        "list_dir" => builtin_list_dir(args, line)?,
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
    let mut days = timestamp / secs_per_day;
    let day_secs = (timestamp % secs_per_day) as u32;
    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    let seconds = day_secs % 60;

    let mut year: i64 = 1970;
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
