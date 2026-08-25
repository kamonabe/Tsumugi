//! Tsumugi のエラー型
//!
//! パースエラーとランタイムエラーを構造的に区別する。
//! Display 実装で「N行目: ...」形式のメッセージを生成する。
//! ランタイムエラーにはスタックトレース（呼び出し経路）を付加できる。

use std::fmt;

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

    /// ランタイムエラーを生成する（種別はメッセージから推定）
    pub fn runtime(line: usize, message: impl Into<String>) -> Self {
        let msg = message.into();
        let kind = classify_runtime_error(&msg);
        Self::Runtime {
            line,
            message: msg,
            kind,
            trace: Vec::new(),
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

/// ランタイムエラーのメッセージからエラー種別を推定する（後方互換: runtime() 経由の既存コード用）
fn classify_runtime_error(message: &str) -> ErrorKind {
    if message.contains("ゼロ除算") {
        ErrorKind::ZeroDivision
    } else if message.contains("型エラー") {
        ErrorKind::Type
    } else if message.contains("インデックス範囲外") {
        ErrorKind::Index
    } else if message.contains("未定義の変数")
        || message.contains("未定義の関数")
        || message.contains("未定義の組み込み関数")
        || message.contains("関数ではない値")
    {
        ErrorKind::Name
    } else if message.contains("ステップ上限") {
        ErrorKind::StepLimit
    } else if message.contains("スタックオーバーフロー") {
        ErrorKind::StackOverflow
    } else if message.contains("サンドボックス") {
        ErrorKind::Sandbox
    } else if message.contains("import 失敗") {
        ErrorKind::Import
    } else if message.contains("引数") && message.contains("個") {
        ErrorKind::Argument
    } else if message.contains("整数オーバーフロー") {
        ErrorKind::IntOverflow
    } else if message.contains("break") || message.contains("continue") {
        ErrorKind::ControlFlow
    } else if message.contains("コレクションサイズ上限超過") {
        ErrorKind::CollectionLimit
    } else if message.contains("変換失敗") || message.contains("変換できない型") {
        ErrorKind::Conversion
    } else if message.contains("のみ使えます")
        || message.contains("の形式で使います")
        || message.contains("に対してのみ")
    {
        ErrorKind::BuiltinType
    } else if message.contains("反復できません") || message.contains("イテレートできません") {
        ErrorKind::Iteration
    } else if message.contains("内部エラー") {
        ErrorKind::Internal
    } else {
        ErrorKind::Runtime
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
