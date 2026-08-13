use crate::ast::*;
use crate::token::Token;

/// トークン列をASTに変換するパーサー
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    /// プログラム全体をパースする
    pub fn parse(&mut self) -> Result<Program, String> {
        let mut stmts = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() {
            let stmt = self.parse_stmt()?;
            stmts.push(stmt);
            self.skip_newlines();
        }

        Ok(stmts)
    }

    // --- 文のパース ---

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        match self.peek() {
            Token::Let => self.parse_let(),
            Token::Return => self.parse_return(),
            Token::If => self.parse_if(),
            Token::While => self.parse_while(),
            Token::Fn => self.parse_fn_def(),
            _ => self.parse_expr_stmt(),
        }
    }

    /// let name = expr
    fn parse_let(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'let'

        let name = match self.advance() {
            Token::Ident(s) => s,
            other => return Err(format!("let の後に識別子が必要です。got: {:?}", other)),
        };

        self.expect(Token::Assign)?;
        let value = self.parse_expr()?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::Let { name, value })
    }

    /// return expr
    fn parse_return(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'return'
        let value = self.parse_expr()?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::Return { value })
    }

    /// if cond \n body (else \n body)? end
    fn parse_if(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'if'

        let condition = self.parse_expr()?;
        self.expect_newline()?;
        self.skip_newlines();

        let then_body = self.parse_block(&[Token::Else, Token::End])?;

        let else_body = if self.peek() == Token::Else {
            self.advance(); // consume 'else'
            self.expect_newline()?;
            self.skip_newlines();
            self.parse_block(&[Token::End])?
        } else {
            Vec::new()
        };

        self.expect(Token::End)?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::If {
            condition,
            then_body,
            else_body,
        })
    }

    /// while cond \n body end
    fn parse_while(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'while'

        let condition = self.parse_expr()?;
        self.expect_newline()?;
        self.skip_newlines();

        let body = self.parse_block(&[Token::End])?;

        self.expect(Token::End)?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::While { condition, body })
    }

    /// fn name(params) \n body end
    fn parse_fn_def(&mut self) -> Result<Stmt, String> {
        self.advance(); // consume 'fn'

        let name = match self.advance() {
            Token::Ident(s) => s,
            other => return Err(format!("fn の後に関数名が必要です。got: {:?}", other)),
        };

        self.expect(Token::LParen)?;
        let params = self.parse_params()?;
        self.expect(Token::RParen)?;
        self.expect_newline()?;
        self.skip_newlines();

        let body = self.parse_block(&[Token::End])?;

        self.expect(Token::End)?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::FnDef { name, params, body })
    }

    /// 引数リスト: ident, ident, ...
    fn parse_params(&mut self) -> Result<Vec<String>, String> {
        let mut params = Vec::new();

        if self.peek() == Token::RParen {
            return Ok(params);
        }

        match self.advance() {
            Token::Ident(s) => params.push(s),
            other => return Err(format!("引数名が必要です。got: {:?}", other)),
        }

        while self.peek() == Token::Comma {
            self.advance(); // consume ','
            match self.advance() {
                Token::Ident(s) => params.push(s),
                other => return Err(format!("引数名が必要です。got: {:?}", other)),
            }
        }

        Ok(params)
    }

    /// 式文
    fn parse_expr_stmt(&mut self) -> Result<Stmt, String> {
        let expr = self.parse_expr()?;
        self.expect_newline_or_eof()?;
        Ok(Stmt::ExprStmt { expr })
    }

    /// ブロック: 終端トークンのいずれかに到達するまで文をパース
    fn parse_block(&mut self, terminators: &[Token]) -> Result<Vec<Stmt>, String> {
        let mut stmts = Vec::new();

        while !self.is_at_end() && !terminators.contains(&self.peek()) {
            let stmt = self.parse_stmt()?;
            stmts.push(stmt);
            self.skip_newlines();
        }

        Ok(stmts)
    }

    // --- 式のパース（優先順位付き再帰下降） ---
    //
    // 優先順位（低→高）:
    //   or
    //   and
    //   == != < > <= >=
    //   + -
    //   * /
    //   not -（単項）
    //   関数呼び出し・リテラル・括弧

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_or()
    }

    /// or
    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;

        while self.peek() == Token::Or {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::BinOp {
                left: Box::new(left),
                op: BinOpKind::Or,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    /// and
    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_comparison()?;

        while self.peek() == Token::And {
            self.advance();
            let right = self.parse_comparison()?;
            left = Expr::BinOp {
                left: Box::new(left),
                op: BinOpKind::And,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    /// == != < > <= >=
    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_add_sub()?;

        loop {
            let op = match self.peek() {
                Token::Eq => BinOpKind::Eq,
                Token::NotEq => BinOpKind::NotEq,
                Token::Lt => BinOpKind::Lt,
                Token::Gt => BinOpKind::Gt,
                Token::LtEq => BinOpKind::LtEq,
                Token::GtEq => BinOpKind::GtEq,
                _ => break,
            };
            self.advance();
            let right = self.parse_add_sub()?;
            left = Expr::BinOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    /// + -
    fn parse_add_sub(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_mul_div()?;

        loop {
            let op = match self.peek() {
                Token::Plus => BinOpKind::Add,
                Token::Minus => BinOpKind::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_mul_div()?;
            left = Expr::BinOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    /// * /
    fn parse_mul_div(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;

        loop {
            let op = match self.peek() {
                Token::Star => BinOpKind::Mul,
                Token::Slash => BinOpKind::Div,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Expr::BinOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    /// 単項: not, -
    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Token::Not => {
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp {
                    op: UnaryOpKind::Not,
                    expr: Box::new(expr),
                })
            }
            Token::Minus => {
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp {
                    op: UnaryOpKind::Neg,
                    expr: Box::new(expr),
                })
            }
            _ => self.parse_call(),
        }
    }

    /// 関数呼び出し or プライマリ
    fn parse_call(&mut self) -> Result<Expr, String> {
        let expr = self.parse_primary()?;

        // ident(...) パターンの場合
        if let Expr::Ident(ref name) = expr {
            if self.peek() == Token::LParen {
                let name = name.clone();
                self.advance(); // consume '('
                let args = self.parse_args()?;
                self.expect(Token::RParen)?;
                return Ok(Expr::Call { name, args });
            }
        }

        Ok(expr)
    }

    /// 呼び出し引数リスト
    fn parse_args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();

        if self.peek() == Token::RParen {
            return Ok(args);
        }

        args.push(self.parse_expr()?);

        while self.peek() == Token::Comma {
            self.advance();
            args.push(self.parse_expr()?);
        }

        Ok(args)
    }

    /// プライマリ: リテラル, 識別子, 括弧式
    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Token::Int(_) => {
                if let Token::Int(n) = self.advance() {
                    Ok(Expr::Int(n))
                } else {
                    unreachable!()
                }
            }
            Token::Float(_) => {
                if let Token::Float(f) = self.advance() {
                    Ok(Expr::Float(f))
                } else {
                    unreachable!()
                }
            }
            Token::Str(_) => {
                if let Token::Str(s) = self.advance() {
                    Ok(Expr::Str(s))
                } else {
                    unreachable!()
                }
            }
            Token::True => {
                self.advance();
                Ok(Expr::Bool(true))
            }
            Token::False => {
                self.advance();
                Ok(Expr::Bool(false))
            }
            Token::Null => {
                self.advance();
                Ok(Expr::Null)
            }
            Token::Ident(_) => {
                if let Token::Ident(s) = self.advance() {
                    Ok(Expr::Ident(s))
                } else {
                    unreachable!()
                }
            }
            Token::Print => {
                // print を関数呼び出しとして扱う
                self.advance();
                self.expect(Token::LParen)?;
                let args = self.parse_args()?;
                self.expect(Token::RParen)?;
                Ok(Expr::Call {
                    name: "print".to_string(),
                    args,
                })
            }
            Token::LParen => {
                self.advance(); // consume '('
                let expr = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            other => Err(format!("予期しないトークン: {:?}", other)),
        }
    }

    // --- ユーティリティ ---

    fn peek(&self) -> Token {
        self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        self.pos += 1;
        tok
    }

    fn is_at_end(&self) -> bool {
        self.peek() == Token::Eof
    }

    fn expect(&mut self, expected: Token) -> Result<(), String> {
        let tok = self.advance();
        if tok == expected {
            Ok(())
        } else {
            Err(format!("期待: {:?}, 実際: {:?}", expected, tok))
        }
    }

    fn expect_newline(&mut self) -> Result<(), String> {
        match self.peek() {
            Token::Newline => {
                self.advance();
                Ok(())
            }
            Token::Eof => Ok(()),
            other => Err(format!("改行が必要です。got: {:?}", other)),
        }
    }

    fn expect_newline_or_eof(&mut self) -> Result<(), String> {
        match self.peek() {
            Token::Newline | Token::Eof => {
                if self.peek() == Token::Newline {
                    self.advance();
                }
                Ok(())
            }
            other => Err(format!("改行またはEOFが必要です。got: {:?}", other)),
        }
    }

    fn skip_newlines(&mut self) {
        while self.peek() == Token::Newline {
            self.advance();
        }
    }
}
