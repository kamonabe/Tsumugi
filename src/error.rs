//! Tsumugi のエラー型
//!
//! パースエラーとランタイムエラーを構造的に区別する。
//! Display 実装で「N行目: ...」形式のメッセージを生成する。

use std::fmt;

/// Tsumugi の構造化エラー型
#[derive(Debug, Clone, PartialEq)]
pub enum TsumugiError {
    /// パースエラー（構文解析時）
    Parse { line: usize, message: String },

    /// ランタイムエラー（実行時）
    Runtime { line: usize, message: String },
}

impl TsumugiError {
    /// パースエラーを生成する
    pub fn parse(line: usize, message: impl Into<String>) -> Self {
        Self::Parse {
            line,
            message: message.into(),
        }
    }

    /// ランタイムエラーを生成する
    #[allow(dead_code)]
    pub fn runtime(line: usize, message: impl Into<String>) -> Self {
        Self::Runtime {
            line,
            message: message.into(),
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
}

impl fmt::Display for TsumugiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { line, message } => write!(f, "{}行目: {}", line, message),
            Self::Runtime { line, message } => write!(f, "{}行目: {}", line, message),
        }
    }
}

impl std::error::Error for TsumugiError {}

/// 「N行目: メッセージ」形式の文字列から TsumugiError::Runtime に変換する。
/// パターンに一致しない場合は行番号0のランタイムエラーとする。
impl From<String> for TsumugiError {
    fn from(s: String) -> Self {
        // "N行目: message" パターンを解析
        if let Some(idx) = s.find("行目: ")
            && let Ok(line) = s[..idx].parse::<usize>()
        {
            let message = s[idx + "行目: ".len()..].to_string();
            return Self::Runtime { line, message };
        }
        Self::Runtime {
            line: 0,
            message: s,
        }
    }
}
