use crate::token::{Spanned, Token};

/// ソースコードをトークン列に変換するレキサー
pub struct Lexer {
    input: Vec<char>,
    pos: usize,
    line: usize,
}

impl Lexer {
    pub fn new(input: &str) -> Self {
        Self {
            input: input.chars().collect(),
            pos: 0,
            line: 1,
        }
    }

    /// 全トークンを一括で返す
    pub fn tokenize(&mut self) -> Vec<Spanned> {
        let mut tokens = Vec::new();
        loop {
            let spanned = self.next_token();
            let is_eof = spanned.token == Token::Eof;
            tokens.push(spanned);
            if is_eof {
                break;
            }
        }
        tokens
    }

    /// 次のトークンを1つ読み取る
    fn next_token(&mut self) -> Spanned {
        self.skip_spaces();

        // コメントをスキップ
        if self.peek() == Some('#') {
            self.skip_comment();
            return self.next_token();
        }

        let line = self.line;

        match self.peek() {
            None => Spanned::new(Token::Eof, line),
            Some('\n') => {
                self.advance();
                self.line += 1;
                Spanned::new(Token::Newline, line)
            }
            Some('"') => {
                let tok = self.read_string();
                Spanned::new(tok, line)
            }
            Some(c) if c.is_ascii_digit() => {
                let tok = self.read_number();
                Spanned::new(tok, line)
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let tok = self.read_ident_or_keyword();
                Spanned::new(tok, line)
            }
            Some(c) => {
                let tok = self.read_symbol(c);
                Spanned::new(tok, line)
            }
        }
    }

    // --- ヘルパー ---

    fn peek(&self) -> Option<char> {
        self.input.get(self.pos).copied()
    }

    fn peek_next(&self) -> Option<char> {
        self.input.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.input.get(self.pos).copied();
        self.pos += 1;
        c
    }

    /// スペースとタブだけスキップ（改行はトークンとして残す）
    fn skip_spaces(&mut self) {
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' || c == '\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    /// # から行末までスキップ
    fn skip_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.advance();
        }
    }

    /// 数値リテラル（整数 or 浮動小数点）
    fn read_number(&mut self) -> Token {
        let mut s = String::new();
        let mut is_float = false;

        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.advance();
            } else if c == '.' && !is_float {
                // 次が数字なら小数点
                if let Some(next) = self.peek_next() {
                    if next.is_ascii_digit() {
                        is_float = true;
                        s.push(c);
                        self.advance();
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        if is_float {
            Token::Float(s.parse().unwrap())
        } else {
            Token::Int(s.parse().unwrap())
        }
    }

    /// 文字列リテラル "..."
    fn read_string(&mut self) -> Token {
        self.advance(); // 開き "
        let mut s = String::new();

        loop {
            match self.peek() {
                None | Some('\n') => break, // 未閉じ→そのまま返す
                Some('"') => {
                    self.advance(); // 閉じ "
                    break;
                }
                Some('\\') => {
                    self.advance();
                    match self.peek() {
                        Some('n') => {
                            s.push('\n');
                            self.advance();
                        }
                        Some('t') => {
                            s.push('\t');
                            self.advance();
                        }
                        Some('\\') => {
                            s.push('\\');
                            self.advance();
                        }
                        Some('"') => {
                            s.push('"');
                            self.advance();
                        }
                        _ => s.push('\\'),
                    }
                }
                Some(c) => {
                    s.push(c);
                    self.advance();
                }
            }
        }

        Token::Str(s)
    }

    /// 識別子 or キーワード
    fn read_ident_or_keyword(&mut self) -> Token {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }

        match s.as_str() {
            "let" => Token::Let,
            "fn" => Token::Fn,
            "return" => Token::Return,
            "if" => Token::If,
            "else" => Token::Else,
            "while" => Token::While,
            "for" => Token::For,
            "in" => Token::In,
            "break" => Token::Break,
            "continue" => Token::Continue,
            "end" => Token::End,
            "and" => Token::And,
            "or" => Token::Or,
            "not" => Token::Not,
            "true" => Token::True,
            "false" => Token::False,
            "null" => Token::Null,
            "print" => Token::Print,
            _ => Token::Ident(s),
        }
    }

    /// 記号トークン
    fn read_symbol(&mut self, c: char) -> Token {
        self.advance();
        match c {
            '+' => Token::Plus,
            '-' => Token::Minus,
            '*' => Token::Star,
            '/' => Token::Slash,
            '%' => Token::Percent,
            '(' => Token::LParen,
            ')' => Token::RParen,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            '{' => Token::LBrace,
            '}' => Token::RBrace,
            ':' => Token::Colon,
            ',' => Token::Comma,
            '=' => {
                if self.peek() == Some('=') {
                    self.advance();
                    Token::Eq
                } else {
                    Token::Assign
                }
            }
            '!' => {
                if self.peek() == Some('=') {
                    self.advance();
                    Token::NotEq
                } else {
                    // '!' 単体は Tsumugi では使わない（not を使う）
                    // 不明な文字として再帰 — line は呼び出し元で記録済み
                    self.next_token().token
                }
            }
            '<' => {
                if self.peek() == Some('=') {
                    self.advance();
                    Token::LtEq
                } else {
                    Token::Lt
                }
            }
            '>' => {
                if self.peek() == Some('=') {
                    self.advance();
                    Token::GtEq
                } else {
                    Token::Gt
                }
            }
            _ => self.next_token().token, // 不明な文字はスキップ
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Token;

    fn tokenize(input: &str) -> Vec<Spanned> {
        Lexer::new(input).tokenize()
    }

    fn tokens_only(input: &str) -> Vec<Token> {
        tokenize(input).into_iter().map(|s| s.token).collect()
    }

    #[test]
    fn simple_let() {
        let tokens = tokens_only("let x = 42");
        assert_eq!(
            tokens,
            vec![
                Token::Let,
                Token::Ident("x".into()),
                Token::Assign,
                Token::Int(42),
                Token::Eof
            ]
        );
    }

    #[test]
    fn line_numbers() {
        let spanned = tokenize("let a = 1\nlet b = 2\n");
        // "let" on line 1
        assert_eq!(spanned[0].line, 1);
        assert_eq!(spanned[0].token, Token::Let);
        // newline token is on line 1 (it ends line 1)
        assert_eq!(spanned[3].token, Token::Int(1));
        assert_eq!(spanned[3].line, 1);
        // "let" on line 2
        let second_let = spanned
            .iter()
            .skip(5)
            .find(|s| s.token == Token::Let)
            .unwrap();
        assert_eq!(second_let.line, 2);
    }

    #[test]
    fn string_with_escapes() {
        let tokens = tokens_only(r#""hello\nworld""#);
        assert_eq!(tokens, vec![Token::Str("hello\nworld".into()), Token::Eof]);
    }

    #[test]
    fn float_literal() {
        let tokens = tokens_only("3.14");
        assert_eq!(tokens, vec![Token::Float(3.14), Token::Eof]);
    }

    #[test]
    fn operators() {
        let tokens = tokens_only("== != <= >= < >");
        assert_eq!(
            tokens,
            vec![
                Token::Eq,
                Token::NotEq,
                Token::LtEq,
                Token::GtEq,
                Token::Lt,
                Token::Gt,
                Token::Eof
            ]
        );
    }

    #[test]
    fn keywords() {
        let tokens = tokens_only(
            "if else while for in break continue fn end return let and or not true false null print",
        );
        assert_eq!(
            tokens,
            vec![
                Token::If,
                Token::Else,
                Token::While,
                Token::For,
                Token::In,
                Token::Break,
                Token::Continue,
                Token::Fn,
                Token::End,
                Token::Return,
                Token::Let,
                Token::And,
                Token::Or,
                Token::Not,
                Token::True,
                Token::False,
                Token::Null,
                Token::Print,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn comment_skipped() {
        let tokens = tokens_only("# this is a comment\nlet x = 1");
        assert_eq!(
            tokens,
            vec![
                Token::Newline,
                Token::Let,
                Token::Ident("x".into()),
                Token::Assign,
                Token::Int(1),
                Token::Eof
            ]
        );
    }
}
