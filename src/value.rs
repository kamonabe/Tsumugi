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

/// 数値の厳密比較の結果（REV-003）
///
/// `Less` / `Equal` / `Greater` は全順序上の位置を表す。`UnorderedNaN` は
/// NaN が絡む比較で、大小でも等価でもないことを表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericOrdering {
    Less,
    Equal,
    Greater,
    UnorderedNaN,
}

impl NumericOrdering {
    /// `<` の結果。`UnorderedNaN`（NaN 比較）は false。
    pub fn is_lt(self) -> bool {
        matches!(self, NumericOrdering::Less)
    }
    /// `<=` の結果。`UnorderedNaN` は false。
    pub fn is_le(self) -> bool {
        matches!(self, NumericOrdering::Less | NumericOrdering::Equal)
    }
    /// `>` の結果。`UnorderedNaN` は false。
    pub fn is_gt(self) -> bool {
        matches!(self, NumericOrdering::Greater)
    }
    /// `>=` の結果。`UnorderedNaN` は false。
    pub fn is_ge(self) -> bool {
        matches!(self, NumericOrdering::Greater | NumericOrdering::Equal)
    }
}

/// Int / Float の厳密比較を担う単一実装（REV-003）
///
/// tree・VM・`PartialEq`・関係演算子・`min`・`max` がすべてこの実装を経由し、
/// backend 間で結果が食い違わないようにする。
///
/// Int×Float は Int を `f64` へ丸めず、Float を整数部と小数部へ分離して
/// 数学的に厳密な順序を返す。これにより `2^53` 近傍での等価の非推移性
/// （`a == f` かつ `f == b` だが `a != b`）が解消する。
pub struct NumericOrder;

impl NumericOrder {
    /// 2 つの値を数値として比較する。
    ///
    /// 両者が数値（Int / Float）でない場合は `None` を返す。呼び出し側が
    /// 型エラーにするか非等価として扱うかを決める。
    pub fn compare(a: &Value, b: &Value) -> Option<NumericOrdering> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Some(Self::from_ord(x.cmp(y))),
            (Value::Float(x), Value::Float(y)) => Some(Self::compare_float_float(*x, *y)),
            (Value::Int(x), Value::Float(y)) => Some(Self::compare_int_float(*x, *y)),
            (Value::Float(x), Value::Int(y)) => Some(Self::flip(Self::compare_int_float(*y, *x))),
            _ => None,
        }
    }

    /// 数値として等価なら true。NaN が絡む場合と非数値は false。
    pub fn numeric_eq(a: &Value, b: &Value) -> bool {
        matches!(Self::compare(a, b), Some(NumericOrdering::Equal))
    }

    /// 数値の大小比較。両者が数値なら `Some(NumericOrdering)`、非数値なら `None`。
    /// NaN が絡む比較は `Some(UnorderedNaN)` を返し、呼び出し側で `<`/`>` などを
    /// すべて false にする。
    pub fn compare_relational(a: &Value, b: &Value) -> Option<NumericOrdering> {
        Self::compare(a, b)
    }

    fn from_ord(o: std::cmp::Ordering) -> NumericOrdering {
        match o {
            std::cmp::Ordering::Less => NumericOrdering::Less,
            std::cmp::Ordering::Equal => NumericOrdering::Equal,
            std::cmp::Ordering::Greater => NumericOrdering::Greater,
        }
    }

    fn flip(o: NumericOrdering) -> NumericOrdering {
        match o {
            NumericOrdering::Less => NumericOrdering::Greater,
            NumericOrdering::Greater => NumericOrdering::Less,
            other => other,
        }
    }

    fn compare_float_float(x: f64, y: f64) -> NumericOrdering {
        match x.partial_cmp(&y) {
            Some(o) => Self::from_ord(o),
            None => NumericOrdering::UnorderedNaN,
        }
    }

    /// `i64` と `f64` の厳密比較（丸め・浮動小数演算を経由しない）。
    ///
    /// アルゴリズム（semantic-decisions §17.1.3）:
    /// - NaN は `UnorderedNaN`
    /// - `±Infinity` は全ての有限 Int より大小が確定
    /// - 有限 Float は `trunc` で整数部を取り、`i128` へ widen して Int と比較する。
    ///   整数部が等しいときは小数部（`fract`）の符号で決着させる。
    ///
    /// `f.trunc()` は絶対値が `2^53` 以上の f64 では小数部を持たないため、
    /// `trunc` 値は常に f64 で正確に表現でき、`i128` へロスなく変換できる。
    fn compare_int_float(i: i64, f: f64) -> NumericOrdering {
        if f.is_nan() {
            return NumericOrdering::UnorderedNaN;
        }
        if f == f64::INFINITY {
            return NumericOrdering::Less; // i < +inf
        }
        if f == f64::NEG_INFINITY {
            return NumericOrdering::Greater; // i > -inf
        }

        // 有限 Float。整数部と小数部へ分離する（丸めではなく分離なので厳密）。
        let trunc = f.trunc(); // 小数部を切り捨てた整数値（f64 で正確）
        let fract = f - trunc; // 小数部（-1 < fract < 1）

        // i64 の全値は i128 に収まる。Float の整数部は `1e300` のように
        // i128 範囲を超え得るため、範囲外は Float の magnitude が Int を上回る
        // として符号で決着する（`trunc as i128` の飽和には依存しない）。
        const I128_MAX_AS_F64: f64 = i128::MAX as f64;
        const I128_MIN_AS_F64: f64 = i128::MIN as f64;
        if trunc >= I128_MAX_AS_F64 {
            return NumericOrdering::Less; // i < +巨大 Float
        }
        if trunc <= I128_MIN_AS_F64 {
            return NumericOrdering::Greater; // i > -巨大 Float
        }

        let trunc_i128 = trunc as i128;
        let i_i128 = i as i128;

        match i_i128.cmp(&trunc_i128) {
            std::cmp::Ordering::Less => NumericOrdering::Less,
            std::cmp::Ordering::Greater => NumericOrdering::Greater,
            std::cmp::Ordering::Equal => {
                // 整数部が一致。小数部の符号で決着する。
                if fract > 0.0 {
                    // f = trunc + fract > i
                    NumericOrdering::Less
                } else if fract < 0.0 {
                    // f = trunc + fract < i
                    NumericOrdering::Greater
                } else {
                    NumericOrdering::Equal
                }
            }
        }
    }
}

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
            // 数値はIntとFloatを跨いで厳密比較する（REV-003）。
            // Int を f64 へ丸めず NumericOrder を経由するため、2^53 超でも
            // 等価が対称・推移的になる。NaN が絡む比較は非等価。
            (Value::Int(_), Value::Float(_)) | (Value::Float(_), Value::Int(_)) => {
                NumericOrder::numeric_eq(self, other)
            }
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

    // ---- REV-003: NumericOrder 厳密比較 ----

    fn int(i: i64) -> Value {
        Value::Int(i)
    }
    fn flt(f: f64) -> Value {
        Value::Float(f)
    }

    #[test]
    fn numeric_order_int_int() {
        assert_eq!(
            NumericOrder::compare(&int(1), &int(2)),
            Some(NumericOrdering::Less)
        );
        assert_eq!(
            NumericOrder::compare(&int(2), &int(2)),
            Some(NumericOrdering::Equal)
        );
        assert_eq!(
            NumericOrder::compare(&int(3), &int(2)),
            Some(NumericOrdering::Greater)
        );
    }

    #[test]
    fn numeric_order_float_float_and_nan() {
        assert_eq!(
            NumericOrder::compare(&flt(1.0), &flt(2.0)),
            Some(NumericOrdering::Less)
        );
        assert_eq!(
            NumericOrder::compare(&flt(2.0), &flt(2.0)),
            Some(NumericOrdering::Equal)
        );
        // -0.0 == 0.0
        assert_eq!(
            NumericOrder::compare(&flt(-0.0), &flt(0.0)),
            Some(NumericOrdering::Equal)
        );
        assert_eq!(
            NumericOrder::compare(&flt(f64::NAN), &flt(1.0)),
            Some(NumericOrdering::UnorderedNaN)
        );
    }

    #[test]
    fn numeric_order_non_numeric_is_none() {
        assert_eq!(
            NumericOrder::compare(&int(1), &Value::Str("a".into())),
            None
        );
        assert_eq!(NumericOrder::compare(&Value::Bool(true), &int(1)), None);
    }

    #[test]
    fn numeric_order_int_float_fractional() {
        // 1 < 1.5 < 2
        assert_eq!(
            NumericOrder::compare(&int(1), &flt(1.5)),
            Some(NumericOrdering::Less)
        );
        assert_eq!(
            NumericOrder::compare(&int(2), &flt(1.5)),
            Some(NumericOrdering::Greater)
        );
        // 負の小数部
        assert_eq!(
            NumericOrder::compare(&int(-1), &flt(-1.5)),
            Some(NumericOrdering::Greater)
        );
        assert_eq!(
            NumericOrder::compare(&int(-2), &flt(-1.5)),
            Some(NumericOrdering::Less)
        );
        // 整数値どうし（小数部 0）
        assert_eq!(
            NumericOrder::compare(&int(3), &flt(3.0)),
            Some(NumericOrdering::Equal)
        );
    }

    #[test]
    fn numeric_order_int_float_infinity() {
        assert_eq!(
            NumericOrder::compare(&int(i64::MAX), &flt(f64::INFINITY)),
            Some(NumericOrdering::Less)
        );
        assert_eq!(
            NumericOrder::compare(&int(i64::MIN), &flt(f64::NEG_INFINITY)),
            Some(NumericOrdering::Greater)
        );
        assert_eq!(
            NumericOrder::compare(&flt(f64::INFINITY), &int(0)),
            Some(NumericOrdering::Greater)
        );
    }

    #[test]
    fn numeric_order_int_float_nan() {
        assert_eq!(
            NumericOrder::compare(&int(1), &flt(f64::NAN)),
            Some(NumericOrdering::UnorderedNaN)
        );
        assert_eq!(
            NumericOrder::compare(&flt(f64::NAN), &int(1)),
            Some(NumericOrdering::UnorderedNaN)
        );
        // NaN は等価でも大小でもない
        assert!(!NumericOrder::numeric_eq(&int(1), &flt(f64::NAN)));
    }

    /// 2^53 近傍で丸めずに厳密比較できる（REV-003 の核心）。
    #[test]
    fn numeric_order_exact_beyond_2_pow_53() {
        let two53 = 1i64 << 53; // 9_007_199_254_740_992
        // f64 で 2^53 は正確に表現できる。2^53 と 2^53+1 は f64 では同じ値になるが、
        // Int 側は区別できるため、厳密比較では 2^53+1 > (2^53 as f64) になる。
        let f_two53 = two53 as f64;
        assert_eq!(
            NumericOrder::compare(&int(two53), &flt(f_two53)),
            Some(NumericOrdering::Equal)
        );
        assert_eq!(
            NumericOrder::compare(&int(two53 + 1), &flt(f_two53)),
            Some(NumericOrdering::Greater)
        );
        assert_eq!(
            NumericOrder::compare(&int(two53 - 1), &flt(f_two53)),
            Some(NumericOrdering::Less)
        );
    }

    /// 従来 `i64 as f64` では桁落ちで等価になっていた非推移性が解消する。
    /// a = 2^53, f = 2^53 as f64, b = 2^53 + 1。旧実装では a==f かつ f==b だが a!=b。
    #[test]
    fn equality_is_transitive_near_2_pow_53() {
        let a = int(1i64 << 53);
        let b = int((1i64 << 53) + 1);
        let f = flt((1i64 << 53) as f64);
        // 厳密比較: a == f は true、f == b は false（旧実装では両方 true だった）
        assert!(a == f);
        assert!(!(f == b));
        // 推移性: a == f かつ f == b なら a == b でなければならない。
        // f == b が false なので前件が成り立たず、非推移性は起きない。
        assert!(!(a == b));
    }

    #[test]
    fn equality_is_symmetric() {
        let cases = [
            (int(5), flt(5.0)),
            (int(1i64 << 53), flt((1i64 << 53) as f64)),
            (int(0), flt(-0.0)),
        ];
        for (a, b) in cases {
            assert_eq!(a == b, b == a, "対称性: {:?} vs {:?}", a, b);
        }
    }

    /// 巨大 Float（i128 範囲外）は Int より大きい／小さいと確定する。
    #[test]
    fn numeric_order_huge_float() {
        assert_eq!(
            NumericOrder::compare(&int(i64::MAX), &flt(1e300)),
            Some(NumericOrdering::Less)
        );
        assert_eq!(
            NumericOrder::compare(&int(i64::MIN), &flt(-1e300)),
            Some(NumericOrdering::Greater)
        );
    }

    #[test]
    fn ordering_helpers() {
        assert!(NumericOrdering::Less.is_lt());
        assert!(NumericOrdering::Less.is_le());
        assert!(!NumericOrdering::Less.is_gt());
        assert!(NumericOrdering::Equal.is_le());
        assert!(NumericOrdering::Equal.is_ge());
        assert!(!NumericOrdering::Equal.is_lt());
        assert!(NumericOrdering::Greater.is_gt());
        assert!(NumericOrdering::Greater.is_ge());
        // NaN は全比較で false
        assert!(!NumericOrdering::UnorderedNaN.is_lt());
        assert!(!NumericOrdering::UnorderedNaN.is_le());
        assert!(!NumericOrdering::UnorderedNaN.is_gt());
        assert!(!NumericOrdering::UnorderedNaN.is_ge());
    }
}
