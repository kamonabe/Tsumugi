use crate::token::{FStrPart, Spanned, Token};

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
                // f"..." → f-string
                if c == 'f' && self.peek_next() == Some('"') {
                    let tok = self.read_fstring();
                    Spanned::new(tok, line)
                } else {
                    let tok = self.read_ident_or_keyword();
                    Spanned::new(tok, line)
                }
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
            match s.parse::<f64>() {
                Ok(n) => Token::Float(n),
                Err(_) => Token::Error(format!("浮動小数点リテラルが不正です: {}", s)),
            }
        } else {
            match s.parse::<i64>() {
                Ok(n) => Token::Int(n),
                Err(_) => Token::Error(format!(
                    "整数リテラルが範囲外です (最大: {}): {}",
                    i64::MAX,
                    s
                )),
            }
        }
    }

    /// 文字列リテラル "..."
    fn read_string(&mut self) -> Token {
        self.advance(); // 開き "
        let mut s = String::new();
        let mut closed = false;

        loop {
            match self.peek() {
                None | Some('\n') => break, // 未閉じ
                Some('"') => {
                    self.advance(); // 閉じ "
                    closed = true;
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

        if closed {
            Token::Str(s)
        } else {
            Token::Error("文字列リテラルが閉じられていません".into())
        }
    }

    /// f-string リテラル f"...{expr}..."
    fn read_fstring(&mut self) -> Token {
        self.advance(); // 'f'
        self.advance(); // 開き '"'

        let mut parts: Vec<FStrPart> = Vec::new();
        let mut literal = String::new();
        let mut closed = false;

        loop {
            match self.peek() {
                None | Some('\n') => break, // 未閉じ
                Some('"') => {
                    self.advance(); // 閉じ '"'
                    closed = true;
                    break;
                }
                Some('{') => {
                    // {{ → エスケープされた '{' リテラル
                    if self.peek_next() == Some('{') {
                        self.advance();
                        self.advance();
                        literal.push('{');
                        continue;
                    }
                    // リテラル部分を保存
                    if !literal.is_empty() {
                        parts.push(FStrPart::Literal(literal.clone()));
                        literal.clear();
                    }
                    self.advance(); // '{' を消費

                    // {} の中身をトークン化する
                    let expr_tokens = self.read_fstring_expr();
                    match expr_tokens {
                        Ok(tokens) => parts.push(FStrPart::Expr(tokens)),
                        Err(msg) => return Token::Error(msg),
                    }
                }
                Some('}') => {
                    // }} → エスケープされた '}' リテラル
                    if self.peek_next() == Some('}') {
                        self.advance();
                        self.advance();
                        literal.push('}');
                        continue;
                    }
                    // 単独の '}' はエラー
                    return Token::Error(
                        "f-string: 対応しない '}' があります（'}}' でエスケープしてください）"
                            .into(),
                    );
                }
                Some('\\') => {
                    self.advance();
                    match self.peek() {
                        Some('n') => {
                            literal.push('\n');
                            self.advance();
                        }
                        Some('t') => {
                            literal.push('\t');
                            self.advance();
                        }
                        Some('\\') => {
                            literal.push('\\');
                            self.advance();
                        }
                        Some('"') => {
                            literal.push('"');
                            self.advance();
                        }
                        _ => literal.push('\\'),
                    }
                }
                Some(c) => {
                    literal.push(c);
                    self.advance();
                }
            }
        }

        if !closed {
            return Token::Error("f-stringが閉じられていません".into());
        }

        // 最後のリテラル部分を保存
        if !literal.is_empty() {
            parts.push(FStrPart::Literal(literal));
        }

        Token::FStr(parts)
    }

    /// f-string 内の {} 中の式をトークン列として読み取る
    /// '}' に到達するまでの文字をサブ Lexer でトークン化する
    fn read_fstring_expr(&mut self) -> Result<Vec<Spanned>, String> {
        let mut depth = 0; // ネストされた {} の追跡
        let mut expr_chars: Vec<char> = Vec::new();

        loop {
            match self.peek() {
                None | Some('\n') => {
                    return Err("f-string: 式が閉じられていません（'}' がありません）".into());
                }
                Some('{') => {
                    depth += 1;
                    expr_chars.push('{');
                    self.advance();
                }
                Some('}') => {
                    if depth == 0 {
                        self.advance(); // 閉じ '}' を消費
                        break;
                    }
                    depth -= 1;
                    expr_chars.push('}');
                    self.advance();
                }
                Some('"') => {
                    // 式中の文字列リテラルを丸ごと読み取る
                    expr_chars.push('"');
                    self.advance();
                    loop {
                        match self.peek() {
                            None | Some('\n') => {
                                return Err(
                                    "f-string: 式中の文字列リテラルが閉じられていません".into()
                                );
                            }
                            Some('\\') => {
                                expr_chars.push('\\');
                                self.advance();
                                if let Some(c) = self.peek() {
                                    expr_chars.push(c);
                                    self.advance();
                                }
                            }
                            Some('"') => {
                                expr_chars.push('"');
                                self.advance();
                                break;
                            }
                            Some(c) => {
                                expr_chars.push(c);
                                self.advance();
                            }
                        }
                    }
                }
                Some(c) => {
                    expr_chars.push(c);
                    self.advance();
                }
            }
        }

        // サブ Lexer で式部分をトークン化
        let expr_str: String = expr_chars.into_iter().collect();
        let mut sub_lexer = Lexer::new(&expr_str);
        let tokens: Vec<Spanned> = sub_lexer
            .tokenize()
            .into_iter()
            .filter(|s| s.token != Token::Eof && s.token != Token::Newline)
            .collect();

        if tokens.is_empty() {
            return Err("f-string: 空の式 '{}' は使えません".into());
        }

        Ok(tokens)
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
            "elif" => Token::Elif,
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
            "import" => Token::Import,
            "try" => Token::Try,
            "catch" => Token::Catch,
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
            _ => Token::Unknown(c), // 不明な文字はトークンとして記録
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
    fn unclosed_string_newline() {
        let tokens = tokens_only("\"hello\nworld");
        assert_eq!(
            tokens,
            vec![
                Token::Error("文字列リテラルが閉じられていません".into()),
                Token::Newline,
                Token::Ident("world".into()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn unclosed_string_eof() {
        let tokens = tokens_only("\"hello");
        assert_eq!(
            tokens,
            vec![
                Token::Error("文字列リテラルが閉じられていません".into()),
                Token::Eof
            ]
        );
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
            "if else elif while for in break continue fn end return let and or not true false null print import",
        );
        assert_eq!(
            tokens,
            vec![
                Token::If,
                Token::Else,
                Token::Elif,
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
                Token::Import,
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

    #[test]
    fn integer_overflow_returns_error_token() {
        let tokens = tokens_only("99999999999999999999");
        assert_eq!(tokens.len(), 2); // Error + Eof
        match &tokens[0] {
            Token::Error(msg) => {
                assert!(msg.contains("整数リテラルが範囲外です"));
                assert!(msg.contains("99999999999999999999"));
            }
            other => panic!("期待: Token::Error, 実際: {:?}", other),
        }
    }

    #[test]
    fn i64_max_is_valid() {
        let tokens = tokens_only("9223372036854775807");
        assert_eq!(tokens, vec![Token::Int(i64::MAX), Token::Eof]);
    }

    #[test]
    fn i64_max_plus_one_is_error() {
        let tokens = tokens_only("9223372036854775808");
        match &tokens[0] {
            Token::Error(msg) => {
                assert!(msg.contains("整数リテラルが範囲外です"));
            }
            other => panic!("期待: Token::Error, 実際: {:?}", other),
        }
    }

    #[test]
    fn fstring_simple_variable() {
        let tokens = tokens_only(r#"f"hello, {x}""#);
        match &tokens[0] {
            Token::FStr(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0], FStrPart::Literal("hello, ".into()));
                if let FStrPart::Expr(toks) = &parts[1] {
                    assert_eq!(toks.len(), 1);
                    assert_eq!(toks[0].token, Token::Ident("x".into()));
                } else {
                    panic!("期待: FStrPart::Expr");
                }
            }
            other => panic!("期待: Token::FStr, 実際: {:?}", other),
        }
    }

    #[test]
    fn fstring_expression() {
        let tokens = tokens_only(r#"f"{2 + 3}""#);
        match &tokens[0] {
            Token::FStr(parts) => {
                assert_eq!(parts.len(), 1);
                if let FStrPart::Expr(toks) = &parts[0] {
                    assert_eq!(toks.len(), 3); // 2 + 3
                    assert_eq!(toks[0].token, Token::Int(2));
                    assert_eq!(toks[1].token, Token::Plus);
                    assert_eq!(toks[2].token, Token::Int(3));
                } else {
                    panic!("期待: FStrPart::Expr");
                }
            }
            other => panic!("期待: Token::FStr, 実際: {:?}", other),
        }
    }

    #[test]
    fn fstring_escaped_braces() {
        let tokens = tokens_only(r#"f"{{hello}}""#);
        match &tokens[0] {
            Token::FStr(parts) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0], FStrPart::Literal("{hello}".into()));
            }
            other => panic!("期待: Token::FStr, 実際: {:?}", other),
        }
    }

    #[test]
    fn fstring_no_interpolation() {
        let tokens = tokens_only(r#"f"just text""#);
        match &tokens[0] {
            Token::FStr(parts) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0], FStrPart::Literal("just text".into()));
            }
            other => panic!("期待: Token::FStr, 実際: {:?}", other),
        }
    }

    #[test]
    fn fstring_unclosed() {
        let tokens = tokens_only(r#"f"hello"#);
        match &tokens[0] {
            Token::Error(msg) => {
                assert!(msg.contains("f-string"));
            }
            other => panic!("期待: Token::Error, 実際: {:?}", other),
        }
    }

    #[test]
    fn fstring_empty_expr_error() {
        let tokens = tokens_only(r#"f"hello {}""#);
        match &tokens[0] {
            Token::Error(msg) => {
                assert!(msg.contains("空の式"));
            }
            other => panic!("期待: Token::Error, 実際: {:?}", other),
        }
    }

    #[test]
    fn fstring_multiple_parts() {
        let tokens = tokens_only(r#"f"{a} and {b}""#);
        match &tokens[0] {
            Token::FStr(parts) => {
                assert_eq!(parts.len(), 3); // Expr("a"), Literal(" and "), Expr("b")
                if let FStrPart::Expr(toks) = &parts[0] {
                    assert_eq!(toks[0].token, Token::Ident("a".into()));
                } else {
                    panic!("期待: FStrPart::Expr");
                }
                assert_eq!(parts[1], FStrPart::Literal(" and ".into()));
                if let FStrPart::Expr(toks) = &parts[2] {
                    assert_eq!(toks[0].token, Token::Ident("b".into()));
                } else {
                    panic!("期待: FStrPart::Expr");
                }
            }
            other => panic!("期待: Token::FStr, 実際: {:?}", other),
        }
    }

    #[test]
    fn f_ident_not_fstring() {
        // "f" alone (not followed by '"') should be an identifier
        let tokens = tokens_only("f");
        assert_eq!(tokens, vec![Token::Ident("f".into()), Token::Eof]);
    }
}
