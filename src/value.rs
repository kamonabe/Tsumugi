use std::collections::BTreeMap;
use std::collections::HashMap;

use crate::ast::Stmt;
use crate::chunk::Chunk;

/// Tsumugi の実行時の値
#[derive(Debug, Clone, PartialEq)]
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
        captured: HashMap<String, Value>,
    },
    /// VM用関数値（コンパイル済みバイトコード）
    VmFn {
        name: String,
        arity: usize,
        chunk: Chunk,
        /// クロージャがキャプチャした値（値キャプチャ方式）
        upvalues: Vec<Value>,
    },
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
            Value::VmFn { name, arity, .. } => {
                write!(f, "<fn {}({} args)>", name, arity)
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
        assert_eq!(Value::Float(3.14).to_string(), "3.14");
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
