/// Tsumugi の実行時の値
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
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
        }
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
    }

    #[test]
    fn falsy_values() {
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Null.is_truthy());
        assert!(!Value::Int(0).is_truthy());
        assert!(!Value::Float(0.0).is_truthy());
        assert!(!Value::Str("".to_string()).is_truthy());
    }

    #[test]
    fn display() {
        assert_eq!(Value::Int(42).to_string(), "42");
        assert_eq!(Value::Float(3.14).to_string(), "3.14");
        assert_eq!(Value::Str("hi".to_string()).to_string(), "hi");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Null.to_string(), "null");
    }
}
