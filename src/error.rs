//! Tsumugi のエラー型
//!
//! パースエラーとランタイムエラーを構造的に区別する。
//! Display 実装で「N行目: ...」形式のメッセージを生成する。
//! ランタイムエラーにはスタックトレース（呼び出し経路）を付加できる。

use crate::value::Value;
use std::fmt;

/// 値の型名を canonical error message 用に返す（AUD-019）
///
/// message には値そのものや秘密を埋め込まず、この型名だけを用いる。
/// 名称は [`docs/semantic-decisions.md`] 第3.3節の型名一覧に一致させる。
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::Str(_) => "Str",
        Value::Bool(_) => "Bool",
        Value::Null => "Null",
        Value::List(_) => "List",
        Value::Dict(_) => "Dict",
        Value::Fn { .. } | Value::VmFn { .. } => "Fn",
        Value::Error { .. } => "Error",
    }
}

/// 二項演算子の canonical 表記（AUD-019）
pub fn binop_symbol(op: crate::ast::BinOpKind) -> &'static str {
    use crate::ast::BinOpKind::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Mod => "%",
        Eq => "==",
        NotEq => "!=",
        Lt => "<",
        Gt => ">",
        LtEq => "<=",
        GtEq => ">=",
        And => "and",
        Or => "or",
    }
}

/// 単項演算子の canonical 表記（AUD-019）
pub fn unaryop_symbol(op: crate::ast::UnaryOpKind) -> &'static str {
    use crate::ast::UnaryOpKind::*;
    match op {
        Neg => "-",
        Not => "not",
    }
}

/// ランタイムエラーの種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// ゼロ除算
    ZeroDivision,
    /// 型エラー（二項演算・単項演算・比較の型不一致）
    Type,
    /// インデックス範囲外
    Index,
    /// 未定義の変数・関数・関数ではない値の呼び出し
    Name,
    /// ステップ上限到達
    StepLimit,
    /// スタックオーバーフロー（再帰深度超過）
    StackOverflow,
    /// サンドボックス違反
    Sandbox,
    /// import 失敗
    Import,
    /// 引数の数が合わない
    Argument,
    /// 整数オーバーフロー
    IntOverflow,
    /// break/continue のループ外使用
    ControlFlow,
    /// コレクションサイズ上限超過
    CollectionLimit,
    /// 型変換失敗（to_int / to_float）
    Conversion,
    /// 組み込み関数への不正な型の引数
    BuiltinType,
    /// イテレーション不可の値に対する for
    Iteration,
    /// 標準出力などのI/O失敗（broken pipe等）
    Io,
    /// VM 内部エラー（コンパイラバグ等）
    Internal,
    /// 上記に該当しないランタイムエラー
    Runtime,
}

impl ErrorKind {
    /// try/catch の `e["type"]` フィールドに使われる文字列を返す
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ZeroDivision => "zero_division",
            Self::Type => "type",
            Self::Index => "index",
            Self::Name => "name",
            Self::StepLimit => "limit",
            Self::StackOverflow => "overflow",
            Self::Sandbox => "sandbox",
            Self::Import => "import",
            Self::Argument => "argument",
            Self::IntOverflow => "int_overflow",
            Self::ControlFlow => "control_flow",
            Self::CollectionLimit => "collection_limit",
            Self::Conversion => "conversion",
            Self::BuiltinType => "builtin_type",
            Self::Iteration => "iteration",
            Self::Io => "io",
            Self::Internal => "internal",
            Self::Runtime => "runtime",
        }
    }
}

/// スタックトレースの1フレーム
#[derive(Debug, Clone, PartialEq)]
pub struct TraceFrame {
    /// 関数名（トップレベルなら "<main>"）
    pub name: String,
    /// 呼び出し元の行番号
    pub line: usize,
}

/// Tsumugi の構造化エラー型
#[derive(Debug, Clone, PartialEq)]
pub enum TsumugiError {
    /// パースエラー（構文解析時）
    Parse { line: usize, message: String },

    /// ランタイムエラー（実行時）
    Runtime {
        line: usize,
        message: String,
        kind: ErrorKind,
        trace: Vec<TraceFrame>,
    },
}

impl TsumugiError {
    /// パースエラーを生成する
    pub fn parse(line: usize, message: impl Into<String>) -> Self {
        Self::Parse {
            line,
            message: message.into(),
        }
    }

    /// ランタイムエラーを種別指定で生成する
    pub fn runtime_with_kind(line: usize, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::Runtime {
            line,
            message: message.into(),
            kind,
            trace: Vec::new(),
        }
    }

    /// エラーの行番号を取得する
    #[allow(dead_code)]
    pub fn line(&self) -> usize {
        match self {
            Self::Parse { line, .. } => *line,
            Self::Runtime { line, .. } => *line,
        }
    }

    /// エラーのメッセージ部分を取得する
    #[allow(dead_code)]
    pub fn message(&self) -> &str {
        match self {
            Self::Parse { message, .. } => message,
            Self::Runtime { message, .. } => message,
        }
    }

    /// エラーの種別を取得する
    #[allow(dead_code)]
    pub fn kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Runtime { kind, .. } => Some(*kind),
            Self::Parse { .. } => None,
        }
    }

    /// スタックトレースを付加する（既にトレースが付加されている場合は上書きしない）
    pub fn with_trace(self, trace: Vec<TraceFrame>) -> Self {
        match self {
            Self::Runtime {
                line,
                message,
                kind,
                trace: ref existing,
            } if existing.is_empty() => Self::Runtime {
                line,
                message,
                kind,
                trace,
            },
            other => other,
        }
    }

    /// エラーの種類を分類する（try/catch で Value::Error の type フィールドに使用）
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Parse { .. } => "parse",
            Self::Runtime { kind, .. } => kind.as_str(),
        }
    }
}

/// AUD-019 canonical error constructors
///
/// operation ごとの共通 constructor から kind・canonical message を生成する。
/// tree-walk 評価器と VM が同じ constructor を用いることで、両 backend の
/// kind/message が一致する。テンプレートは [`docs/semantic-decisions.md`] 第3.4節を正本とする。
///
/// message には値そのものを埋め込まず、[`type_name`] が返す型名だけを用いる。
impl TsumugiError {
    /// 変数 read・callee 名が未定義
    pub fn undefined_name(line: usize, name: &str) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Name,
            format!("未定義の変数または関数: {}", name),
        )
    }

    /// 未定義変数への通常代入
    pub fn assign_undefined(line: usize, name: &str) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Name,
            format!("未定義の変数に代入: {}", name),
        )
    }

    /// Int の 0 除算・剰余
    pub fn zero_division(line: usize) -> Self {
        Self::runtime_with_kind(line, ErrorKind::ZeroDivision, "ゼロ除算")
    }

    /// 整数演算 overflow
    pub fn int_overflow(line: usize, operation: &str) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::IntOverflow,
            format!("整数オーバーフロー: {}", operation),
        )
    }

    /// 算術演算の対象型不正
    pub fn arithmetic_type(
        line: usize,
        op: crate::ast::BinOpKind,
        left: &Value,
        right: &Value,
    ) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Type,
            format!(
                "演算子 {} は {} と {} に適用できません",
                binop_symbol(op),
                type_name(left),
                type_name(right)
            ),
        )
    }

    /// 単項演算の対象型不正
    pub fn unary_type(line: usize, op: crate::ast::UnaryOpKind, operand: &Value) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Type,
            format!(
                "演算子 {} は {} に適用できません",
                unaryop_symbol(op),
                type_name(operand)
            ),
        )
    }

    /// 大小比較の対象型不正
    pub fn comparison_type(
        line: usize,
        op: crate::ast::BinOpKind,
        left: &Value,
        right: &Value,
    ) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Type,
            format!(
                "比較演算子 {} は {} と {} に適用できません",
                binop_symbol(op),
                type_name(left),
                type_name(right)
            ),
        )
    }

    /// List index が Int でない
    pub fn list_index_type(line: usize, actual: &Value) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Type,
            format!(
                "List のインデックスは Int である必要があります: {}",
                type_name(actual)
            ),
        )
    }

    /// Str index が Int でない
    pub fn str_index_type(line: usize, actual: &Value) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Type,
            format!(
                "Str のインデックスは Int である必要があります: {}",
                type_name(actual)
            ),
        )
    }

    /// Dict key が Str でない
    pub fn dict_key_type(line: usize, actual: &Value) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Type,
            format!(
                "Dict のキーは Str である必要があります: {}",
                type_name(actual)
            ),
        )
    }

    /// List index 範囲外
    pub fn list_index_out_of_range(line: usize, index: i64, len: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Index,
            format!("List のインデックスが範囲外です: {} (長さ: {})", index, len),
        )
    }

    /// Str index 範囲外
    pub fn str_index_out_of_range(line: usize, index: i64, len: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Index,
            format!("Str のインデックスが範囲外です: {} (長さ: {})", index, len),
        )
    }

    /// index read 非対応型
    pub fn index_read_unsupported(line: usize, actual: &Value) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Type,
            format!("インデックスアクセスできない型です: {}", type_name(actual)),
        )
    }

    /// index assignment 非対応型
    pub fn index_assign_unsupported(line: usize, actual: &Value) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Type,
            format!("インデックス代入できない型です: {}", type_name(actual)),
        )
    }

    /// 反復非対応型
    pub fn not_iterable(line: usize, actual: &Value) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Iteration,
            format!("反復できない型です: {}", type_name(actual)),
        )
    }

    /// 非 callable の呼び出し
    pub fn not_callable(line: usize, actual: &Value) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Type,
            format!("呼び出せない型です: {}", type_name(actual)),
        )
    }

    /// user function / method の arity 不一致
    pub fn user_arity(line: usize, callable: &str, expected: usize, actual: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Argument,
            format!(
                "{} の引数個数が一致しません: 期待 {}, 実際 {}",
                callable, expected, actual
            ),
        )
    }

    /// builtin の arity 不一致
    pub fn builtin_arity(line: usize, builtin: &str, expected: usize, actual: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Argument,
            format!(
                "{} の引数個数が一致しません: 期待 {}, 実際 {}",
                builtin, expected, actual
            ),
        )
    }

    /// variadic builtin の最小 arity 不足
    pub fn builtin_min_arity(line: usize, builtin: &str, minimum: usize, actual: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Argument,
            format!(
                "{} の引数が不足しています: 最小 {}, 実際 {}",
                builtin, minimum, actual
            ),
        )
    }

    /// builtin argument の型不正
    pub fn builtin_arg_type(
        line: usize,
        builtin: &str,
        position: usize,
        expected: &str,
        actual: &Value,
    ) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::BuiltinType,
            format!(
                "{} の第 {} 引数は {} である必要があります: {}",
                builtin,
                position,
                expected,
                type_name(actual)
            ),
        )
    }

    /// callback が非 callable
    pub fn callback_not_callable(line: usize, builtin: &str, actual: &Value) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Type,
            format!(
                "{} のコールバックは呼び出し可能である必要があります: {}",
                builtin,
                type_name(actual)
            ),
        )
    }

    /// callback の arity 不一致
    pub fn callback_arity(line: usize, builtin: &str, actual: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Argument,
            format!(
                "{} のコールバック引数個数が一致しません: 期待 1, 実際 {}",
                builtin, actual
            ),
        )
    }

    /// push / pop の第1引数が変数でない
    pub fn mutation_target_not_variable(line: usize, builtin: &str) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::BuiltinType,
            format!("{} の第 1 引数には List 変数を指定してください", builtin),
        )
    }

    /// 空 List への pop
    pub fn pop_empty_list(line: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::BuiltinType,
            "pop は空の List には使用できません",
        )
    }

    /// builtin 固有の状態不正
    pub fn builtin_state(line: usize, builtin: &str, state: &str) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::BuiltinType,
            format!("{} は {} には使用できません", builtin, state),
        )
    }

    /// collection 上限超過
    pub fn collection_limit(line: usize, requested: usize, limit: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::CollectionLimit,
            format!(
                "コレクション要素数が上限を超えました: {} (上限: {})",
                requested, limit
            ),
        )
    }

    /// user function depth 上限
    pub fn call_depth_limit(line: usize, limit: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::StackOverflow,
            format!("スタックオーバーフロー: 再帰が深すぎます (上限: {})", limit),
        )
    }

    /// step/fuel 上限
    pub fn step_limit(line: usize, limit: u64) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::StepLimit,
            format!("ステップ上限に達しました (上限: {})", limit),
        )
    }

    /// loop 外の break
    pub fn break_outside_loop(line: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::ControlFlow,
            "break はループの中でのみ使用できます",
        )
    }

    /// loop 外の continue
    pub fn continue_outside_loop(line: usize) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::ControlFlow,
            "continue はループの中でのみ使用できます",
        )
    }

    /// 内部エラー（VM/Compiler の不変条件違反など）
    pub fn internal(line: usize, detail: impl Into<String>) -> Self {
        Self::runtime_with_kind(
            line,
            ErrorKind::Internal,
            format!("内部エラー: {}", detail.into()),
        )
    }
}

impl fmt::Display for TsumugiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { line, message } => write!(f, "{}行目: {}", line, message),
            Self::Runtime {
                line,
                message,
                trace,
                ..
            } => {
                write!(f, "{}行目: {}", line, message)?;
                for frame in trace {
                    write!(f, "\n  in {}() ({}行目)", frame.name, frame.line)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for TsumugiError {}
