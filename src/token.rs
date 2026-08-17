/// Tsumugi のトークン型
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // リテラル
    Int(i64),
    Float(f64),
    Str(String),
    True,
    False,
    Null,

    // 識別子・キーワード
    Ident(String),
    Let,
    Fn,
    Return,
    If,
    Else,
    Elif,
    While,
    For,
    In,
    Break,
    Continue,
    End,
    And,
    Or,
    Not,
    Print,
    Import,

    // 演算子
    Plus,    // +
    Minus,   // -
    Star,    // *
    Slash,   // /
    Percent, // %
    Assign,  // =
    Eq,      // ==
    NotEq,   // !=
    Lt,      // <
    Gt,      // >
    LtEq,    // <=
    GtEq,    // >=

    // 区切り
    LParen,   // (
    RParen,   // )
    LBracket, // [
    RBracket, // ]
    LBrace,   // {
    RBrace,   // }
    Comma,    // ,
    Colon,    // :

    // 制御
    Newline,
    Eof,

    // 不明な文字（レキサーが認識できない文字）
    Unknown(char),
}

/// 行番号付きトークン
#[derive(Debug, Clone, PartialEq)]
pub struct Spanned {
    pub token: Token,
    pub line: usize,
}

impl Spanned {
    pub fn new(token: Token, line: usize) -> Self {
        Self { token, line }
    }
}
