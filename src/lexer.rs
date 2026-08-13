use crate::token::Token;

/// ソースコードをトークン列に変換するレキサー
pub struct Lexer {
    input: Vec<char>,
    pos: usize,
}

impl Lexer {
    pub fn new(input: &str) -> Self {
        Self {
            input: input.chars().collect(),
            pos: 0,
        }
    }

    /// 全トークンを一括で返す
    pub fn tokenize(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token();
            if tok == Token::Eof {
                tokens.push(tok);
                break;
            }
            tokens.push(tok);
        }
        tokens
    }

    /// 次のトークンを1つ読み取る
    fn next_token(&mut self) -> Token {
        self.skip_spaces();

        // コメントをスキップ
        if self.peek() == Some('#') {
            self.skip_comment();
            return self.next_token();
        }

        match self.peek() {
            None => Token::Eof,
            Some('\n') => {
                self.advance();
                Token::Newline
            }
            Some('"') => self.read_string(),
            Some(c) if c.is_ascii_digit() => self.read_number(),
            Some(c) if c.is_ascii_alphabetic() || c == '_' => self.read_ident_or_keyword(),
            Some(c) => self.read_symbol(c),
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
            '(' => Token::LParen,
            ')' => Token::RParen,
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
                    // 不明な文字として再帰
                    self.next_token()
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
            _ => self.next_token(), // 不明な文字はスキップ
        }
    }
}
