use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::Stmt;
use crate::chunk::Chunk;

/// 共有可能な変数セル（参照キャプチャ用）
pub type SharedValue = Rc<RefCell<Value>>;

/// Tsumugi の実行時の値
#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    List(Vec<Value>),
    Dict(BTreeMap<String, Value>),
    /// 関数値（ツリーウォーク用: ユーザー定義関数を値として扱う）
    Fn {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
        captured: HashMap<String, SharedValue>,
    },
    /// VM用関数値（コンパイル済みバイトコード）
    /// Rc<Chunk> により関数呼び出し・クロージャ生成時のディープコピーを回避
    VmFn {
        name: String,
        arity: usize,
        params: Vec<String>,
        chunk: Rc<Chunk>,
        /// クロージャがキャプチャした値（参照キャプチャ方式）
        upvalues: Vec<SharedValue>,
    },
    /// 構造化エラー値（try/catch で捕捉したエラー）
    /// Display では message を返すため、既存の文字列結合と互換性がある。
    /// インデックスアクセスで "type" / "message" / "line" を取得可能。
    Error {
        error_type: String,
        message: String,
        line: usize,
    },
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Dict(a), Value::Dict(b)) => a == b,
            (Value::Fn { .. }, Value::Fn { .. }) => false,
            (Value::VmFn { .. }, Value::VmFn { .. }) => false,
            (
                Value::Error {
                    error_type: t1,
                    message: m1,
                    line: l1,
                },
                Value::Error {
                    error_type: t2,
                    message: m2,
                    line: l2,
                },
            ) => t1 == t2 && m1 == m2 && l1 == l2,
            _ => false,
        }
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "Int({})", n),
            Value::Float(n) => write!(f, "Float({})", n),
            Value::Str(s) => write!(f, "Str({:?})", s),
            Value::Bool(b) => write!(f, "Bool({})", b),
            Value::Null => write!(f, "Null"),
            Value::List(items) => write!(f, "List({:?})", items),
            Value::Dict(map) => write!(f, "Dict({:?})", map),
            Value::Fn { name, params, .. } => {
                write!(f, "Fn({}, params={:?})", name, params)
            }
            Value::VmFn { name, arity, .. } => {
                write!(f, "VmFn({}, arity={})", name, arity)
            }
            Value::Error {
                error_type,
                message,
                line,
            } => {
                write!(f, "Error({}: {} at line {})", error_type, message, line)
            }
        }
    }
}

impl Value {
    /// 真偽判定（if / while の条件で使う）
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Null => false,
            Value::Int(0) => false,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(v) => !v.is_empty(),
            Value::Dict(m) => !m.is_empty(),
            Value::Fn { .. } => true,
            Value::VmFn { .. } => true,
            Value::Error { .. } => true,
            _ => true,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{}", n),
            Value::Float(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{}", s),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Null => write!(f, "null"),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(format_value_repr).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Dict(map) => {
                let parts: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("\"{}\": {}", k, format_value_repr(v)))
                    .collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
            Value::Fn { name, params, .. } => {
                write!(f, "<fn {}({})>", name, params.join(", "))
            }
            Value::VmFn { name, params, .. } => {
                write!(f, "<fn {}({})>", name, params.join(", "))
            }
            Value::Error { message, .. } => {
                write!(f, "{}", message)
            }
        }
    }
}

/// Display 用に値を repr 形式（文字列はクォート付き）で表示する
fn format_value_repr(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("\"{}\"", s),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_values() {
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Int(1).is_truthy());
        assert!(Value::Int(-1).is_truthy());
        assert!(Value::Float(0.1).is_truthy());
        assert!(Value::Str("hello".to_string()).is_truthy());
        assert!(Value::List(vec![Value::Int(1)]).is_truthy());
        assert!(Value::Dict(BTreeMap::from([("a".into(), Value::Int(1))])).is_truthy());
    }

    #[test]
    fn falsy_values() {
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Null.is_truthy());
        assert!(!Value::Int(0).is_truthy());
        assert!(!Value::Float(0.0).is_truthy());
        assert!(!Value::Str("".to_string()).is_truthy());
        assert!(!Value::List(vec![]).is_truthy());
        assert!(!Value::Dict(BTreeMap::new()).is_truthy());
    }

    #[test]
    fn display() {
        assert_eq!(Value::Int(42).to_string(), "42");
        assert_eq!(Value::Float(2.5).to_string(), "2.5");
        assert_eq!(Value::Str("hi".to_string()).to_string(), "hi");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Null.to_string(), "null");
        assert_eq!(
            Value::List(vec![Value::Int(1), Value::Str("a".into())]).to_string(),
            "[1, \"a\"]"
        );
        assert_eq!(
            Value::Dict(BTreeMap::from([("x".into(), Value::Int(10))])).to_string(),
            "{\"x\": 10}"
        );
    }
}
