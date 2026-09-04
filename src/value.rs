use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::Stmt;
use crate::chunk::Chunk;

/// 共有可能な変数セル（参照キャプチャ用）
pub type SharedValue = Rc<RefCell<Value>>;

/// 関数値の同一性を表す識別子（AUD-048）
///
/// 関数式・関数定義を実行して値を生成するたびに、ExecutionContext 内で
/// 単調増加する値を 1 つ割り当てる。clone・変数代入・引数渡し・collection 格納では
/// 同じ ID を保持する。tree/VM の関数等価性は FunctionId だけで判定する。
/// ID は実行内の比較専用で、script や audit event へ生値を公開しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionId(pub u64);

/// ツリーウォーク用関数の不変部分
///
/// 呼び出しごとに本体ASTを複製しないよう `Rc` で共有する。
/// VM側の `VmFn` が `Rc<Chunk>` で同じ問題を避けているのと同じ方針。
#[derive(Debug)]
pub struct FnDef {
    /// 関数名（無名関数は `<lambda>`）
    pub name: String,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
}

/// Tsumugi の実行時の値
#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    /// リスト。copy-on-write（AUD-047）。clone はハンドル共有で O(1)、
    /// mutation は `Rc::make_mut` を通して書き込み時だけ backing を複製する。
    List(Rc<Vec<Value>>),
    /// 辞書。copy-on-write（AUD-047）。List と同じく mutation は `Rc::make_mut` 経由。
    Dict(Rc<BTreeMap<String, Value>>),
    /// 関数値（ツリーウォーク用: ユーザー定義関数を値として扱う）
    /// `Rc` により関数呼び出し・self-binding・クロージャ生成時のディープコピーを回避
    Fn {
        /// 関数値の同一性（AUD-048）。生成のたびに新規発番、clone では保持
        id: FunctionId,
        /// 定義時に確定する不変部分（名前・引数・本体）
        def: Rc<FnDef>,
        /// 定義時にキャプチャした変数セル。セル自体は参照共有される
        captured: Rc<HashMap<String, SharedValue>>,
    },
    /// VM用関数値（コンパイル済みバイトコード）
    /// Rc<Chunk> により関数呼び出し・クロージャ生成時のディープコピーを回避
    VmFn {
        /// 関数値の同一性（AUD-048）。生成のたびに新規発番、clone では保持
        id: FunctionId,
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
    /// 規範仕様の等価比較（AUD-014）
    ///
    /// 全ての型の組み合わせで結果を返し、型エラーにしない。型が違う値は等しくない。
    /// 数値だけは例外で、IntとFloatを数値として比較する（`1 == 1.0` は true）。
    /// List / Dict / Error は構造で比較し、関数値は同一の関数値とだけ等しい。
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            // 数値はIntとFloatを跨いで比較する（NaNはIEEE 754どおり不一致）
            (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
            (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            // 共有 backing（同じ Rc）なら要素比較を省く。分離済みでも要素で比較する。
            (Value::List(a), Value::List(b)) => Rc::ptr_eq(a, b) || a == b,
            (Value::Dict(a), Value::Dict(b)) => Rc::ptr_eq(a, b) || a == b,
            // 関数値は FunctionId だけで同一性を判定する（AUD-048）。
            // ID は関数式・関数定義を評価して値を生成するたびに新規発番され、
            // clone・代入・引数渡し・collection 格納では保持される。
            // capture の有無や backend（tree/VM）に関係なく、同じ ID の値だけが等しい。
            (Value::Fn { id: id_a, .. }, Value::Fn { id: id_b, .. }) => id_a == id_b,
            (Value::VmFn { id: id_a, .. }, Value::VmFn { id: id_b, .. }) => id_a == id_b,
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
            Value::Fn { def, .. } => {
                write!(f, "Fn({}, params={:?})", def.name, def.params)
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
            Value::Fn { def, .. } => {
                write!(f, "<fn {}({})>", def.name, def.params.join(", "))
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
        assert!(Value::List(Rc::new(vec![Value::Int(1)])).is_truthy());
        assert!(Value::Dict(Rc::new(BTreeMap::from([("a".into(), Value::Int(1))]))).is_truthy());
    }

    #[test]
    fn falsy_values() {
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Null.is_truthy());
        assert!(!Value::Int(0).is_truthy());
        assert!(!Value::Float(0.0).is_truthy());
        assert!(!Value::Str("".to_string()).is_truthy());
        assert!(!Value::List(Rc::new(vec![])).is_truthy());
        assert!(!Value::Dict(Rc::new(BTreeMap::new())).is_truthy());
    }

    #[test]
    fn display() {
        assert_eq!(Value::Int(42).to_string(), "42");
        assert_eq!(Value::Float(2.5).to_string(), "2.5");
        assert_eq!(Value::Str("hi".to_string()).to_string(), "hi");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Null.to_string(), "null");
        assert_eq!(
            Value::List(Rc::new(vec![Value::Int(1), Value::Str("a".into())])).to_string(),
            "[1, \"a\"]"
        );
        assert_eq!(
            Value::Dict(Rc::new(BTreeMap::from([("x".into(), Value::Int(10))]))).to_string(),
            "{\"x\": 10}"
        );
    }
}
