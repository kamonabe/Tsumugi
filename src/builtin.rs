//! 組み込み関数の実装
//!
//! eval.rs から分離した組み込み関数群。
//! カテゴリ別にヘルパーメソッドへ分割し、eval_builtin がディスパッチする。

use crate::ast::Expr;
use crate::error::TsumugiError;
use crate::value::Value;

use std::fs;
use std::io::{self, BufRead};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::Evaluator;

impl Evaluator {
    /// 組み込み関数を評価する。
    /// 該当する組み込み関数があれば Ok(Some(value))、なければ Ok(None)、エラーなら Err を返す。
    pub(crate) fn eval_builtin(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        match name {
            // I/O・環境系
            "print" | "input" | "env" | "args" | "exit" => self.builtin_io(name, args, line),

            // コレクション操作系
            "len" | "push" | "pop" | "keys" | "values" | "has_key" | "type" | "slice"
            | "contains" | "sort" | "reverse" | "range" => {
                self.builtin_collection(name, args, line)
            }

            // 文字列操作系
            "split" | "join" | "trim" | "starts_with" | "ends_with" | "replace" | "upper"
            | "lower" => self.builtin_string(name, args, line),

            // 型変換・数値系
            "to_int" | "to_str" | "to_float" | "abs" | "min" | "max" | "floor" | "ceil"
            | "round" => self.builtin_numeric(name, args, line),

            // ファイルI/O系
            "read_file" | "read_lines" | "write_file" | "append_file" => {
                self.builtin_file_io(name, args, line)
            }

            // パス・ファイルシステム系
            "path_exists" | "path_join" | "mkdir" | "remove" | "remove_dir" | "rename"
            | "list_dir" | "file_size" | "is_file" | "is_dir" => {
                self.builtin_filesystem(name, args, line)
            }

            // 日時系
            "now" | "format_time" => self.builtin_time(name, args, line),

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
            "env" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: env() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let name = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: env() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(match std::env::var(&name) {
                    Ok(val) => Value::Str(val),
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
    // コレクション操作系: len, push, pop, keys, values, has_key, type, slice,
    //                     contains, sort, reverse, range
    // =========================================================================

    fn builtin_collection(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        match name {
            "len" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: len() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
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
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Int(length)))
            }
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
            "keys" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: keys() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let val = self.eval_expr(&args[0], line)?;
                match val {
                    Value::Dict(map) => {
                        let key_list: Vec<Value> =
                            map.keys().map(|k| Value::Str(k.clone())).collect();
                        Ok(Some(Value::List(key_list)))
                    }
                    _ => Err(format!("{}行目: keys() は辞書にのみ使用できます", line).into()),
                }
            }
            "values" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: values() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let val = self.eval_expr(&args[0], line)?;
                match val {
                    Value::Dict(map) => {
                        let value_list: Vec<Value> = map.into_values().collect();
                        Ok(Some(Value::List(value_list)))
                    }
                    _ => Err(format!("{}行目: values() は辞書にのみ使用できます", line).into()),
                }
            }
            "has_key" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: has_key() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let dict = self.eval_expr(&args[0], line)?;
                let key = self.eval_expr(&args[1], line)?;
                match (&dict, &key) {
                    (Value::Dict(map), Value::Str(k)) => Ok(Some(Value::Bool(map.contains_key(k)))),
                    (Value::Dict(_), _) => Err(format!(
                        "{}行目: has_key() の第2引数は文字列である必要があります",
                        line
                    )
                    .into()),
                    _ => Err(format!(
                        "{}行目: has_key() の第1引数は辞書である必要があります",
                        line
                    )
                    .into()),
                }
            }
            "type" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: type() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
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
                Ok(Some(Value::Str(type_name.to_string())))
            }
            "slice" => {
                if args.len() != 3 {
                    return Err(format!(
                        "{}行目: slice() は引数3個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let collection = self.eval_expr(&args[0], line)?;
                let start = match self.eval_expr(&args[1], line)? {
                    Value::Int(n) => n as usize,
                    _ => {
                        return Err(format!(
                            "{}行目: slice() の第2引数は整数である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let end = match self.eval_expr(&args[2], line)? {
                    Value::Int(n) => n as usize,
                    _ => {
                        return Err(format!(
                            "{}行目: slice() の第3引数は整数である必要があります",
                            line
                        )
                        .into());
                    }
                };
                match collection {
                    Value::List(list) => {
                        let end = end.min(list.len());
                        let start = start.min(end);
                        Ok(Some(Value::List(list[start..end].to_vec())))
                    }
                    Value::Str(s) => {
                        let chars: Vec<char> = s.chars().collect();
                        let end = end.min(chars.len());
                        let start = start.min(end);
                        Ok(Some(Value::Str(chars[start..end].iter().collect())))
                    }
                    _ => Err(format!(
                        "{}行目: slice() はリストまたは文字列にのみ使用できます",
                        line
                    )
                    .into()),
                }
            }
            "contains" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: contains() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
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
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Bool(result)))
            }
            "sort" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: sort() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let val = self.eval_expr(&args[0], line)?;
                match val {
                    Value::List(mut list) => {
                        list.sort_by_key(|a| a.to_string());
                        Ok(Some(Value::List(list)))
                    }
                    _ => Err(format!("{}行目: sort() はリストにのみ使用できます", line).into()),
                }
            }
            "reverse" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: reverse() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let val = self.eval_expr(&args[0], line)?;
                match val {
                    Value::List(mut list) => {
                        list.reverse();
                        Ok(Some(Value::List(list)))
                    }
                    Value::Str(s) => Ok(Some(Value::Str(s.chars().rev().collect()))),
                    _ => Err(format!(
                        "{}行目: reverse() はリストまたは文字列にのみ使用できます",
                        line
                    )
                    .into()),
                }
            }
            "range" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: range() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let start = match self.eval_expr(&args[0], line)? {
                    Value::Int(n) => n,
                    _ => {
                        return Err(format!(
                            "{}行目: range() の引数は整数である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let end = match self.eval_expr(&args[1], line)? {
                    Value::Int(n) => n,
                    _ => {
                        return Err(format!(
                            "{}行目: range() の引数は整数である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let list: Vec<Value> = (start..end).map(Value::Int).collect();
                Ok(Some(Value::List(list)))
            }
            _ => Ok(None),
        }
    }

    // =========================================================================
    // 文字列操作系: split, join, trim, starts_with, ends_with, replace,
    //              upper, lower
    // =========================================================================

    fn builtin_string(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        match name {
            "split" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: split() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let s = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: split() の第1引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let sep = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: split() の第2引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let parts: Vec<Value> = s.split(&sep).map(|p| Value::Str(p.to_string())).collect();
                Ok(Some(Value::List(parts)))
            }
            "join" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: join() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let list = match self.eval_expr(&args[0], line)? {
                    Value::List(v) => v,
                    _ => {
                        return Err(format!(
                            "{}行目: join() の第1引数はリストである必要があります",
                            line
                        )
                        .into());
                    }
                };
                let sep = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: join() の第2引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let parts: Vec<String> = list.iter().map(|v| v.to_string()).collect();
                Ok(Some(Value::Str(parts.join(&sep))))
            }
            "trim" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: trim() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let s = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: trim() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Str(s.trim().to_string())))
            }
            "starts_with" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: starts_with() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let s = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: starts_with() の第1引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let prefix = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: starts_with() の第2引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Bool(s.starts_with(&prefix))))
            }
            "ends_with" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: ends_with() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let s = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: ends_with() の第1引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let suffix = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: ends_with() の第2引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Bool(s.ends_with(&suffix))))
            }
            "replace" => {
                if args.len() != 3 {
                    return Err(format!(
                        "{}行目: replace() は引数3個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let s = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: replace() の第1引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let old = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: replace() の第2引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let new = match self.eval_expr(&args[2], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: replace() の第3引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Str(s.replace(&old, &new))))
            }
            "upper" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: upper() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let s = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: upper() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Str(s.to_uppercase())))
            }
            "lower" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: lower() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let s = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: lower() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Str(s.to_lowercase())))
            }
            _ => Ok(None),
        }
    }

    // =========================================================================
    // 型変換・数値系: to_int, to_str, to_float, abs, min, max, floor, ceil, round
    // =========================================================================

    fn builtin_numeric(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        match name {
            "to_int" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: to_int() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
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
                        return Err(
                            format!("{}行目: to_int() で変換できません: {:?}", line, val).into(),
                        );
                    }
                };
                Ok(Some(Value::Int(result)))
            }
            "to_str" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: to_str() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let val = self.eval_expr(&args[0], line)?;
                Ok(Some(Value::Str(val.to_string())))
            }
            "to_float" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: to_float() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let val = self.eval_expr(&args[0], line)?;
                let result = match &val {
                    Value::Float(f) => *f,
                    Value::Int(n) => *n as f64,
                    Value::Str(s) => s
                        .parse::<f64>()
                        .map_err(|_| format!("{}行目: to_float() 変換失敗: \"{}\"", line, s))?,
                    _ => {
                        return Err(format!(
                            "{}行目: to_float() で変換できません: {:?}",
                            line, val
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Float(result)))
            }
            "abs" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: abs() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let val = self.eval_expr(&args[0], line)?;
                match val {
                    Value::Int(n) => Ok(Some(Value::Int(n.abs()))),
                    Value::Float(f) => Ok(Some(Value::Float(f.abs()))),
                    _ => Err(format!("{}行目: abs() は数値にのみ使用できます", line).into()),
                }
            }
            "min" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: min() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let a = self.eval_expr(&args[0], line)?;
                let b = self.eval_expr(&args[1], line)?;
                match (&a, &b) {
                    (Value::Int(x), Value::Int(y)) => Ok(Some(Value::Int(*x.min(y)))),
                    (Value::Float(x), Value::Float(y)) => Ok(Some(Value::Float(x.min(*y)))),
                    (Value::Int(x), Value::Float(y)) => Ok(Some(Value::Float((*x as f64).min(*y)))),
                    (Value::Float(x), Value::Int(y)) => Ok(Some(Value::Float(x.min(*y as f64)))),
                    _ => Err(format!("{}行目: min() は数値にのみ使用できます", line).into()),
                }
            }
            "max" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: max() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let a = self.eval_expr(&args[0], line)?;
                let b = self.eval_expr(&args[1], line)?;
                match (&a, &b) {
                    (Value::Int(x), Value::Int(y)) => Ok(Some(Value::Int(*x.max(y)))),
                    (Value::Float(x), Value::Float(y)) => Ok(Some(Value::Float(x.max(*y)))),
                    (Value::Int(x), Value::Float(y)) => Ok(Some(Value::Float((*x as f64).max(*y)))),
                    (Value::Float(x), Value::Int(y)) => Ok(Some(Value::Float(x.max(*y as f64)))),
                    _ => Err(format!("{}行目: max() は数値にのみ使用できます", line).into()),
                }
            }
            "floor" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: floor() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let val = self.eval_expr(&args[0], line)?;
                match val {
                    Value::Float(f) => Ok(Some(Value::Int(f.floor() as i64))),
                    Value::Int(n) => Ok(Some(Value::Int(n))),
                    _ => Err(format!("{}行目: floor() は数値にのみ使用できます", line).into()),
                }
            }
            "ceil" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: ceil() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let val = self.eval_expr(&args[0], line)?;
                match val {
                    Value::Float(f) => Ok(Some(Value::Int(f.ceil() as i64))),
                    Value::Int(n) => Ok(Some(Value::Int(n))),
                    _ => Err(format!("{}行目: ceil() は数値にのみ使用できます", line).into()),
                }
            }
            "round" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: round() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let val = self.eval_expr(&args[0], line)?;
                match val {
                    Value::Float(f) => Ok(Some(Value::Int(f.round() as i64))),
                    Value::Int(n) => Ok(Some(Value::Int(n))),
                    _ => Err(format!("{}行目: round() は数値にのみ使用できます", line).into()),
                }
            }
            _ => Ok(None),
        }
    }

    // =========================================================================
    // ファイルI/O系: read_file, read_lines, write_file, append_file
    // =========================================================================

    fn builtin_file_io(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        match name {
            "read_file" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: read_file() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: read_file() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(match fs::read_to_string(&path) {
                    Ok(content) => Value::Str(content),
                    Err(_) => Value::Null,
                }))
            }
            "read_lines" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: read_lines() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: read_lines() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(match fs::read_to_string(&path) {
                    Ok(content) => {
                        let lines: Vec<Value> =
                            content.lines().map(|l| Value::Str(l.to_string())).collect();
                        Value::List(lines)
                    }
                    Err(_) => Value::Null,
                }))
            }
            "write_file" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: write_file() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: write_file() の第1引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let content = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    other => other.to_string(),
                };
                Ok(Some(Value::Bool(fs::write(&path, &content).is_ok())))
            }
            "append_file" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: append_file() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: append_file() の第1引数は文字列である必要があります",
                            line
                        )
                        .into());
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
                Ok(Some(Value::Bool(result.is_ok())))
            }
            _ => Ok(None),
        }
    }

    // =========================================================================
    // パス・ファイルシステム系: path_exists, path_join, mkdir, remove,
    //                          remove_dir, rename, list_dir, file_size,
    //                          is_file, is_dir
    // =========================================================================

    fn builtin_filesystem(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        match name {
            "path_exists" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: path_exists() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: path_exists() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Bool(Path::new(&path).exists())))
            }
            "path_join" => {
                if args.is_empty() {
                    return Err(format!("{}行目: path_join() は引数が1個以上必要です", line).into());
                }
                let mut path = std::path::PathBuf::new();
                for arg in args {
                    let part = match self.eval_expr(arg, line)? {
                        Value::Str(s) => s,
                        _ => {
                            return Err(format!(
                                "{}行目: path_join() の引数は文字列である必要があります",
                                line
                            )
                            .into());
                        }
                    };
                    path.push(part);
                }
                Ok(Some(Value::Str(path.to_string_lossy().to_string())))
            }
            "mkdir" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: mkdir() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: mkdir() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Bool(fs::create_dir_all(&path).is_ok())))
            }
            "remove" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: remove() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: remove() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let p = Path::new(&path);
                let result = if p.is_dir() {
                    fs::remove_dir(&path)
                } else {
                    fs::remove_file(&path)
                };
                Ok(Some(Value::Bool(result.is_ok())))
            }
            "remove_dir" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: remove_dir() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: remove_dir() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Bool(fs::remove_dir_all(&path).is_ok())))
            }
            "rename" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: rename() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let from = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: rename() の第1引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let to = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: rename() の第2引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Bool(fs::rename(&from, &to).is_ok())))
            }
            "list_dir" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: list_dir() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: list_dir() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(match fs::read_dir(&path) {
                    Ok(entries) => {
                        let mut names: Vec<Value> = Vec::new();
                        for entry in entries.flatten() {
                            if let Some(name) = entry.file_name().to_str() {
                                names.push(Value::Str(name.to_string()));
                            }
                        }
                        names.sort_by_key(|a| a.to_string());
                        Value::List(names)
                    }
                    Err(_) => Value::Null,
                }))
            }
            "file_size" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: file_size() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: file_size() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(match fs::metadata(&path) {
                    Ok(meta) => Value::Int(meta.len() as i64),
                    Err(_) => Value::Null,
                }))
            }
            "is_file" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: is_file() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: is_file() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Bool(Path::new(&path).is_file())))
            }
            "is_dir" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{}行目: is_dir() は引数1個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let path = match self.eval_expr(&args[0], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: is_dir() の引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                Ok(Some(Value::Bool(Path::new(&path).is_dir())))
            }
            _ => Ok(None),
        }
    }

    // =========================================================================
    // 日時系: now, format_time
    // =========================================================================

    fn builtin_time(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Value>, TsumugiError> {
        match name {
            "now" => {
                if !args.is_empty() {
                    return Err(format!(
                        "{}行目: now() は引数0個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                Ok(Some(Value::Int(timestamp)))
            }
            "format_time" => {
                if args.len() != 2 {
                    return Err(format!(
                        "{}行目: format_time() は引数2個ですが、{}個渡されました",
                        line,
                        args.len()
                    )
                    .into());
                }
                let timestamp = match self.eval_expr(&args[0], line)? {
                    Value::Int(n) => n,
                    _ => {
                        return Err(format!(
                            "{}行目: format_time() の第1引数は整数である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let format = match self.eval_expr(&args[1], line)? {
                    Value::Str(s) => s,
                    _ => {
                        return Err(format!(
                            "{}行目: format_time() の第2引数は文字列である必要があります",
                            line
                        )
                        .into());
                    }
                };
                let formatted = format_unix_timestamp(timestamp, &format);
                Ok(Some(Value::Str(formatted)))
            }
            _ => Ok(None),
        }
    }
}

// =============================================================================
// ヘルパー関数（モジュールレベル）
// =============================================================================

fn format_unix_timestamp(timestamp: i64, format: &str) -> String {
    let mut days = timestamp / 86400;
    let day_secs = timestamp % 86400;
    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    let seconds = day_secs % 60;

    let mut year = 1970i64;
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

    let mut month = 1;
    for &md in &month_days {
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
