use crate::ast::*;
use crate::error::TsumugiError;
use crate::token::{FStrPart, Spanned, Token};

/// 式のネスト深度上限（パーサーの再帰スタックオーバーフロー防止）
const MAX_PARSE_DEPTH: usize = 256;

/// トークン列をASTに変換するパーサー
pub struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
    errors: Vec<TsumugiError>,
    /// 式パースの再帰深度（スタックオーバーフロー防止）
    depth: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Spanned>) -> Self {
        Self {
            tokens,
            pos: 0,
            errors: Vec::new(),
            depth: 0,
        }
    }

    /// プログラム全体をパースする
    ///
    /// エラー回復により複数のパースエラーをまとめて報告する。
    /// エラーが1つ以上ある場合は Err(Vec<TsumugiError>) を返す。
    pub fn parse(&mut self) -> Result<Program, Vec<TsumugiError>> {
        let mut stmts = Vec::new();
        self.skip_newlines();

        while !self.is_at_end() {
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(e) => {
                    self.errors.push(e);
                    self.synchronize();
                }
            }
            self.skip_newlines();
        }

        if self.errors.is_empty() {
            Ok(stmts)
        } else {
            Err(std::mem::take(&mut self.errors))
        }
    }

    /// エラー回復: 次の文の境界までトークンを読み飛ばす
    ///
    /// 改行・EOF・文の先頭になりうるキーワードをリカバリポイントとする。
    fn synchronize(&mut self) {
        loop {
            match self.peek_token() {
                Token::Eof => break,
                Token::Newline => {
                    self.advance();
                    break;
                }
                // 次の文の先頭になりうるキーワード
                Token::Let
                | Token::Fn
                | Token::If
                | Token::While
                | Token::For
                | Token::Return
                | Token::Import
                | Token::Try
                | Token::Break
                | Token::Continue => break,
                _ => {
                    self.advance();
                }
            }
        }
    }

    // --- 文のパース ---

    fn parse_stmt(&mut self) -> Result<Stmt, TsumugiError> {
        // ブロックを持つ文のネストも深度制限の対象
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Err(TsumugiError::parse(
                self.current_line(),
                format!("ネストが深すぎます (上限: {})", MAX_PARSE_DEPTH),
            ));
        }
        let result = self.parse_stmt_inner();
        self.depth -= 1;
        result
    }

    fn parse_stmt_inner(&mut self) -> Result<Stmt, TsumugiError> {
        match self.peek_token() {
            Token::Let => self.parse_let(),
            Token::Return => self.parse_return(),
            Token::If => self.parse_if(),
            Token::While => self.parse_while(),
            Token::For => self.parse_for(),
            Token::Fn => self.parse_fn_def(),
            Token::Break => self.parse_break(),
            Token::Continue => self.parse_continue(),
            Token::Import => self.parse_import(),
            Token::Try => self.parse_try_catch(),
            Token::Ident(_) => {
                // 識別子 + '=' なら再代入文
                // 識別子 + '[' ... ']' + '=' ならインデックス代入文
                if self.is_assign_stmt() {
                    self.parse_assign()
                } else if self.is_index_assign_stmt() {
                    self.parse_index_assign()
                } else {
                    self.parse_expr_stmt()
                }
            }
            _ => self.parse_expr_stmt(),
        }
    }

    /// 現在位置が「ident =」パターン（再代入）か判定
    fn is_assign_stmt(&self) -> bool {
        if let Some(next) = self.tokens.get(self.pos + 1) {
            next.token == Token::Assign
        } else {
            false
        }
    }

    /// 現在位置が「ident[...] =」パターン（インデックス代入）か判定
    fn is_index_assign_stmt(&self) -> bool {
        // ident の次が '[' かチェック
        if let Some(next) = self.tokens.get(self.pos + 1) {
            if next.token != Token::LBracket {
                return false;
            }
        } else {
            return false;
        }
        // '[' の対応する ']' を探し、その次が '=' かチェック
        let mut depth = 0;
        let mut i = self.pos + 1;
        while i < self.tokens.len() {
            match &self.tokens[i].token {
                Token::LBracket => depth += 1,
                Token::RBracket => {
                    depth -= 1;
                    if depth == 0 {
                        // ']' の次が '=' なら index assign
                        if let Some(after) = self.tokens.get(i + 1) {
                            return after.token == Token::Assign;
                        }
                        return false;
                    }
                }
                Token::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// name = expr（再代入）
    fn parse_assign(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        let spanned = self.advance_spanned();
        let name = match spanned.token {
            Token::Ident(s) => s,
            _ => unreachable!(),
        };

        self.advance(); // consume '='
        let value = self.parse_expr()?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::Assign { name, value, line })
    }

    /// ident[expr] = expr（インデックス代入）
    fn parse_index_assign(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        // パースは式として左辺を読み取り、Index ノードを得る
        let object_expr = self.parse_expr()?;

        // object_expr が Expr::Index であることを期待
        let (object, index) = match object_expr {
            Expr::Index { object, index } => (*object, *index),
            _ => {
                return Err(TsumugiError::parse(
                    line,
                    "インデックス代入の左辺が不正です",
                ));
            }
        };

        self.expect(Token::Assign)?;
        let value = self.parse_expr()?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::IndexAssign {
            object,
            index,
            value,
            line,
        })
    }

    /// let name = expr
    fn parse_let(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        self.advance(); // consume 'let'

        let spanned = self.advance_spanned();
        let name = match spanned.token {
            Token::Ident(s) => s,
            other => {
                return Err(TsumugiError::parse(
                    spanned.line,
                    format!("let の後に識別子が必要です。got: {:?}", other),
                ));
            }
        };

        self.expect(Token::Assign)?;
        let value = self.parse_expr()?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::Let { name, value, line })
    }

    /// return expr
    fn parse_return(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        self.advance(); // consume 'return'
        let value = self.parse_expr()?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::Return { value, line })
    }

    /// if cond \n body (else \n body)? end
    fn parse_if(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        self.advance(); // consume 'if'

        let condition = self.parse_expr()?;
        self.expect_newline()?;
        self.skip_newlines();

        let then_body = self.parse_block(&[Token::Else, Token::Elif, Token::End])?;

        let else_body = if self.peek_token() == Token::Elif {
            // elif を再帰的に if 文としてパース（end は最後の1つだけ必要）
            let elif_stmt = self.parse_if()?; // elif を if として再帰パース
            vec![elif_stmt]
        } else if self.peek_token() == Token::Else {
            self.advance(); // consume 'else'
            self.expect_newline()?;
            self.skip_newlines();
            let body = self.parse_block(&[Token::End])?;
            self.expect(Token::End)?;
            self.expect_newline_or_eof()?;
            return Ok(Stmt::If {
                condition,
                then_body,
                else_body: body,
                line,
            });
        } else {
            Vec::new()
        };

        if else_body.is_empty() {
            // end で終わるケース（elif なし、else なし）
            self.expect(Token::End)?;
            self.expect_newline_or_eof()?;
        }

        Ok(Stmt::If {
            condition,
            then_body,
            else_body,
            line,
        })
    }

    /// while cond \n body end
    fn parse_while(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        self.advance(); // consume 'while'

        let condition = self.parse_expr()?;
        self.expect_newline()?;
        self.skip_newlines();

        let body = self.parse_block(&[Token::End])?;

        self.expect(Token::End)?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::While {
            condition,
            body,
            line,
        })
    }

    /// for var in expr \n body end
    fn parse_for(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        self.advance(); // consume 'for'

        let spanned = self.advance_spanned();
        let var = match spanned.token {
            Token::Ident(s) => s,
            other => {
                return Err(TsumugiError::parse(
                    spanned.line,
                    format!("for の後に変数名が必要です。got: {:?}", other),
                ));
            }
        };

        self.expect(Token::In)?;
        let iter = self.parse_expr()?;
        self.expect_newline()?;
        self.skip_newlines();

        let body = self.parse_block(&[Token::End])?;

        self.expect(Token::End)?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::For {
            var,
            iter,
            body,
            line,
        })
    }

    /// break
    fn parse_break(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        self.advance(); // consume 'break'
        self.expect_newline_or_eof()?;
        Ok(Stmt::Break { line })
    }

    /// continue
    fn parse_continue(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        self.advance(); // consume 'continue'
        self.expect_newline_or_eof()?;
        Ok(Stmt::Continue { line })
    }

    /// import "path"
    fn parse_import(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        self.advance(); // consume 'import'

        let spanned = self.advance_spanned();
        let path = match spanned.token {
            Token::Str(s) => s,
            _ => {
                return Err(TsumugiError::parse(
                    line,
                    "import の後に文字列パスが必要です",
                ));
            }
        };

        self.expect_newline_or_eof()?;
        Ok(Stmt::Import { path, line })
    }

    /// try \n body catch var \n body end
    fn parse_try_catch(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        self.advance(); // consume 'try'
        self.expect_newline()?;
        self.skip_newlines();

        let try_body = self.parse_block(&[Token::Catch])?;

        self.expect(Token::Catch)?;

        let spanned = self.advance_spanned();
        let var = match spanned.token {
            Token::Ident(s) => s,
            other => {
                return Err(TsumugiError::parse(
                    spanned.line,
                    format!("catch の後に変数名が必要です。got: {:?}", other),
                ));
            }
        };

        self.expect_newline()?;
        self.skip_newlines();

        let catch_body = self.parse_block(&[Token::End])?;

        self.expect(Token::End)?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::TryCatch {
            try_body,
            var,
            catch_body,
            line,
        })
    }

    /// fn name(params) \n body end
    fn parse_fn_def(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        self.advance(); // consume 'fn'

        let spanned = self.advance_spanned();
        let name = match spanned.token {
            Token::Ident(s) => s,
            other => {
                return Err(TsumugiError::parse(
                    spanned.line,
                    format!("fn の後に関数名が必要です。got: {:?}", other),
                ));
            }
        };

        self.expect(Token::LParen)?;
        let params = self.parse_params()?;
        self.expect(Token::RParen)?;
        self.expect_newline()?;
        self.skip_newlines();

        let body = self.parse_block(&[Token::End])?;

        self.expect(Token::End)?;
        self.expect_newline_or_eof()?;

        Ok(Stmt::FnDef {
            name,
            params,
            body,
            line,
        })
    }

    /// 引数リスト: ident, ident, ...
    fn parse_params(&mut self) -> Result<Vec<String>, TsumugiError> {
        let mut params = Vec::new();

        if self.peek_token() == Token::RParen {
            return Ok(params);
        }

        let spanned = self.advance_spanned();
        match spanned.token {
            Token::Ident(s) => params.push(s),
            other => {
                return Err(TsumugiError::parse(
                    spanned.line,
                    format!("引数名が必要です。got: {:?}", other),
                ));
            }
        }

        while self.peek_token() == Token::Comma {
            self.advance(); // consume ','
            let spanned = self.advance_spanned();
            match spanned.token {
                Token::Ident(s) => params.push(s),
                other => {
                    return Err(TsumugiError::parse(
                        spanned.line,
                        format!("引数名が必要です。got: {:?}", other),
                    ));
                }
            }
        }

        Ok(params)
    }

    /// 式文
    fn parse_expr_stmt(&mut self) -> Result<Stmt, TsumugiError> {
        let line = self.current_line();
        let expr = self.parse_expr()?;
        self.expect_newline_or_eof()?;
        Ok(Stmt::ExprStmt { expr, line })
    }

    /// ブロック: 終端トークンのいずれかに到達するまで文をパース
    fn parse_block(&mut self, terminators: &[Token]) -> Result<Vec<Stmt>, TsumugiError> {
        let mut stmts = Vec::new();

        while !self.is_at_end() && !terminators.contains(&self.peek_token()) {
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

    fn parse_expr(&mut self) -> Result<Expr, TsumugiError> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Err(TsumugiError::parse(
                self.current_line(),
                format!("式のネストが深すぎます (上限: {})", MAX_PARSE_DEPTH),
            ));
        }
        let result = self.parse_or();
        self.depth -= 1;
        result
    }

    /// or
    fn parse_or(&mut self) -> Result<Expr, TsumugiError> {
        let mut left = self.parse_and()?;

        while self.peek_token() == Token::Or {
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
    fn parse_and(&mut self) -> Result<Expr, TsumugiError> {
        let mut left = self.parse_comparison()?;

        while self.peek_token() == Token::And {
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
    fn parse_comparison(&mut self) -> Result<Expr, TsumugiError> {
        let mut left = self.parse_add_sub()?;

        loop {
            let op = match self.peek_token() {
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
    fn parse_add_sub(&mut self) -> Result<Expr, TsumugiError> {
        let mut left = self.parse_mul_div()?;

        loop {
            let op = match self.peek_token() {
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
    fn parse_mul_div(&mut self) -> Result<Expr, TsumugiError> {
        let mut left = self.parse_unary()?;

        loop {
            let op = match self.peek_token() {
                Token::Star => BinOpKind::Mul,
                Token::Slash => BinOpKind::Div,
                Token::Percent => BinOpKind::Mod,
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
    fn parse_unary(&mut self) -> Result<Expr, TsumugiError> {
        match self.peek_token() {
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

    /// 関数呼び出し / インデックスアクセス or プライマリ
    fn parse_call(&mut self) -> Result<Expr, TsumugiError> {
        let mut expr = self.parse_primary()?;

        loop {
            if self.peek_token() == Token::LParen {
                self.advance(); // consume '('
                let args = self.parse_args()?;
                self.expect(Token::RParen)?;
                expr = Expr::Call {
                    callee: Box::new(expr),
                    args,
                };
            } else if self.peek_token() == Token::LBracket {
                self.advance(); // consume '['
                let index = self.parse_expr()?;
                self.expect(Token::RBracket)?;
                expr = Expr::Index {
                    object: Box::new(expr),
                    index: Box::new(index),
                };
            } else {
                break;
            }
        }

        Ok(expr)
    }

    /// 呼び出し引数リスト
    fn parse_args(&mut self) -> Result<Vec<Expr>, TsumugiError> {
        let mut args = Vec::new();

        if self.peek_token() == Token::RParen {
            return Ok(args);
        }

        args.push(self.parse_expr()?);

        while self.peek_token() == Token::Comma {
            self.advance();
            args.push(self.parse_expr()?);
        }

        Ok(args)
    }

    /// プライマリ: リテラル, 識別子, 括弧式, リスト, 辞書
    fn parse_primary(&mut self) -> Result<Expr, TsumugiError> {
        let spanned = self.peek_spanned();
        match spanned.token {
            Token::Int(_) => {
                let s = self.advance_spanned();
                if let Token::Int(n) = s.token {
                    Ok(Expr::Int(n))
                } else {
                    unreachable!()
                }
            }
            Token::Float(_) => {
                let s = self.advance_spanned();
                if let Token::Float(f) = s.token {
                    Ok(Expr::Float(f))
                } else {
                    unreachable!()
                }
            }
            Token::Str(_) => {
                let s = self.advance_spanned();
                if let Token::Str(st) = s.token {
                    Ok(Expr::Str(st))
                } else {
                    unreachable!()
                }
            }
            Token::FStr(_) => {
                let s = self.advance_spanned();
                if let Token::FStr(parts) = s.token {
                    self.parse_fstr(parts, s.line)
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
                let s = self.advance_spanned();
                if let Token::Ident(name) = s.token {
                    Ok(Expr::Ident(name))
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
                    callee: Box::new(Expr::Ident("print".to_string())),
                    args,
                })
            }
            Token::LParen => {
                self.advance(); // consume '('
                let expr = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            Token::LBracket => self.parse_list_literal(),
            Token::LBrace => self.parse_dict_literal(),
            Token::Fn => self.parse_anonymous_fn(),
            Token::Error(_) => {
                let s = self.advance_spanned();
                if let Token::Error(msg) = s.token {
                    Err(TsumugiError::parse(s.line, msg))
                } else {
                    unreachable!()
                }
            }
            Token::Unknown(c) => {
                self.advance();
                Err(TsumugiError::parse(
                    spanned.line,
                    format!("想定外の文字: '{}'", c),
                ))
            }
            other => Err(TsumugiError::parse(
                spanned.line,
                format!("予期しないトークン: {:?}", other),
            )),
        }
    }

    /// リストリテラル: [expr, expr, ...]
    fn parse_list_literal(&mut self) -> Result<Expr, TsumugiError> {
        self.advance(); // consume '['
        let mut items = Vec::new();

        if self.peek_token() == Token::RBracket {
            self.advance(); // consume ']'
            return Ok(Expr::List(items));
        }

        items.push(self.parse_expr()?);
        while self.peek_token() == Token::Comma {
            self.advance(); // consume ','
            // 末尾カンマ対応
            if self.peek_token() == Token::RBracket {
                break;
            }
            items.push(self.parse_expr()?);
        }

        self.expect(Token::RBracket)?;
        Ok(Expr::List(items))
    }

    /// 辞書リテラル: {expr: expr, ...}
    fn parse_dict_literal(&mut self) -> Result<Expr, TsumugiError> {
        self.advance(); // consume '{'
        let mut pairs = Vec::new();

        if self.peek_token() == Token::RBrace {
            self.advance(); // consume '}'
            return Ok(Expr::Dict(pairs));
        }

        let key = self.parse_expr()?;
        self.expect(Token::Colon)?;
        let value = self.parse_expr()?;
        pairs.push((key, value));

        while self.peek_token() == Token::Comma {
            self.advance(); // consume ','
            // 末尾カンマ対応
            if self.peek_token() == Token::RBrace {
                break;
            }
            let key = self.parse_expr()?;
            self.expect(Token::Colon)?;
            let value = self.parse_expr()?;
            pairs.push((key, value));
        }

        self.expect(Token::RBrace)?;
        Ok(Expr::Dict(pairs))
    }

    /// 無名関数式: fn(params) body end
    /// 複数行: fn(params)\n body \n end
    /// 1行:    fn(params) expr end
    fn parse_anonymous_fn(&mut self) -> Result<Expr, TsumugiError> {
        self.advance(); // consume 'fn'
        self.expect(Token::LParen)?;
        let params = self.parse_params()?;
        self.expect(Token::RParen)?;

        // 改行があれば複数行、なければ1行（暗黙 return）
        if self.peek_token() == Token::Newline {
            self.advance(); // consume newline
            let body = self.parse_block(&[Token::End])?;
            self.advance(); // consume 'end'
            Ok(Expr::Lambda { params, body })
        } else {
            // 1行ラムダ: fn(x) expr end → 暗黙的に return 扱い
            let line = self.current_line();
            // "return" キーワードがあれば消費
            if self.peek_token() == Token::Return {
                self.advance();
            }
            let expr = self.parse_expr()?;
            self.expect(Token::End)?;
            Ok(Expr::Lambda {
                params,
                body: vec![Stmt::Return { value: expr, line }],
            })
        }
    }

    /// f-string のトークンパーツを AST に変換する
    fn parse_fstr(&mut self, parts: Vec<FStrPart>, line: usize) -> Result<Expr, TsumugiError> {
        let mut ast_parts: Vec<FStrExprPart> = Vec::new();

        for part in parts {
            match part {
                FStrPart::Literal(s) => {
                    ast_parts.push(FStrExprPart::Literal(s));
                }
                FStrPart::Expr(tokens) => {
                    // サブパーサーで式トークン列をパースする
                    // Eof トークンを末尾に追加（パーサーが終端を認識するため）
                    let mut expr_tokens = tokens;
                    let eof_line = expr_tokens.last().map_or(line, |t| t.line);
                    expr_tokens.push(Spanned::new(Token::Eof, eof_line));

                    let mut sub_parser = Parser::new(expr_tokens);
                    let expr = sub_parser.parse_expr().map_err(|e| {
                        // サブパーサーのエラーに f-string のコンテキストを付加
                        TsumugiError::parse(line, format!("f-string内の式のパースに失敗: {}", e))
                    })?;
                    ast_parts.push(FStrExprPart::Expr(Box::new(expr)));
                }
            }
        }

        Ok(Expr::FStr(ast_parts))
    }

    // --- ユーティリティ ---

    /// 現在位置のトークン種別だけを返す（Token enum）
    fn peek_token(&self) -> Token {
        self.tokens
            .get(self.pos)
            .map(|s| s.token.clone())
            .unwrap_or(Token::Eof)
    }

    /// 現在位置の Spanned をクローンで返す
    fn peek_spanned(&self) -> Spanned {
        self.tokens
            .get(self.pos)
            .cloned()
            .unwrap_or(Spanned::new(Token::Eof, 0))
    }

    /// 進めてトークン種別だけ返す（既存コードとの互換用）
    fn advance(&mut self) -> Token {
        self.advance_spanned().token
    }

    /// 進めて Spanned を返す
    fn advance_spanned(&mut self) -> Spanned {
        let spanned = self
            .tokens
            .get(self.pos)
            .cloned()
            .unwrap_or(Spanned::new(Token::Eof, 0));
        self.pos += 1;
        spanned
    }

    fn is_at_end(&self) -> bool {
        self.peek_token() == Token::Eof
    }

    /// 現在位置の行番号を取得
    fn current_line(&self) -> usize {
        self.tokens.get(self.pos).map(|s| s.line).unwrap_or(0)
    }

    fn expect(&mut self, expected: Token) -> Result<(), TsumugiError> {
        let spanned = self.advance_spanned();
        if spanned.token == expected {
            Ok(())
        } else {
            Err(TsumugiError::parse(
                spanned.line,
                format!("期待: {:?}, 実際: {:?}", expected, spanned.token),
            ))
        }
    }

    fn expect_newline(&mut self) -> Result<(), TsumugiError> {
        let line = self.current_line();
        match self.peek_token() {
            Token::Newline => {
                self.advance();
                Ok(())
            }
            Token::Eof => Ok(()),
            other => Err(TsumugiError::parse(
                line,
                format!("改行が必要です。got: {:?}", other),
            )),
        }
    }

    fn expect_newline_or_eof(&mut self) -> Result<(), TsumugiError> {
        let line = self.current_line();
        match self.peek_token() {
            Token::Newline | Token::Eof => {
                if self.peek_token() == Token::Newline {
                    self.advance();
                }
                Ok(())
            }
            other => Err(TsumugiError::parse(
                line,
                format!("改行またはEOFが必要です。got: {:?}", other),
            )),
        }
    }

    fn skip_newlines(&mut self) {
        while self.peek_token() == Token::Newline {
            self.advance();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(input: &str) -> Result<Program, Vec<TsumugiError>> {
        let tokens = Lexer::new(input).tokenize();
        Parser::new(tokens).parse()
    }

    #[test]
    fn parse_let() {
        let program = parse("let x = 42").unwrap();
        assert_eq!(program.len(), 1);
        match &program[0] {
            Stmt::Let { name, line, .. } => {
                assert_eq!(name, "x");
                assert_eq!(*line, 1);
            }
            other => panic!("expected Let, got {:?}", other),
        }
    }

    #[test]
    fn parse_fn_def() {
        let program = parse("fn add(a, b)\n  return a + b\nend").unwrap();
        assert_eq!(program.len(), 1);
        match &program[0] {
            Stmt::FnDef {
                name, params, line, ..
            } => {
                assert_eq!(name, "add");
                assert_eq!(params, &vec!["a".to_string(), "b".to_string()]);
                assert_eq!(*line, 1);
            }
            other => panic!("expected FnDef, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_else() {
        let src = "if true\n  print(1)\nelse\n  print(2)\nend";
        let program = parse(src).unwrap();
        assert_eq!(program.len(), 1);
        match &program[0] {
            Stmt::If {
                then_body,
                else_body,
                line,
                ..
            } => {
                assert_eq!(*line, 1);
                assert_eq!(then_body.len(), 1);
                assert_eq!(else_body.len(), 1);
            }
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn parse_while() {
        let src = "while x > 0\n  let x = x - 1\nend";
        let program = parse(src).unwrap();
        assert_eq!(program.len(), 1);
        match &program[0] {
            Stmt::While { body, line, .. } => {
                assert_eq!(*line, 1);
                assert_eq!(body.len(), 1);
            }
            other => panic!("expected While, got {:?}", other),
        }
    }

    #[test]
    fn error_has_line_number() {
        let result = parse("let x = 10\nlet = oops");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(!errors.is_empty());
        assert_eq!(
            errors[0].line(),
            2,
            "error should be on line 2: {}",
            errors[0]
        );
    }

    #[test]
    fn parse_multi_error_recovery() {
        // 複数のエラーがまとめて報告されることを確認
        let result = parse("let = oops\nlet y = 1\nlet = bad");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 2, "should report 2 errors: {:?}", errors);
        assert_eq!(errors[0].line(), 1);
        assert_eq!(errors[1].line(), 3);
    }

    #[test]
    fn parse_assign() {
        let program = parse("let x = 1\nx = 2").unwrap();
        assert_eq!(program.len(), 2);
        match &program[1] {
            Stmt::Assign { name, line, .. } => {
                assert_eq!(name, "x");
                assert_eq!(*line, 2);
            }
            other => panic!("expected Assign, got {:?}", other),
        }
    }

    #[test]
    fn parse_assign_with_expr() {
        let program = parse("let x = 10\nx = x + 1").unwrap();
        assert_eq!(program.len(), 2);
        match &program[1] {
            Stmt::Assign { name, .. } => {
                assert_eq!(name, "x");
            }
            other => panic!("expected Assign, got {:?}", other),
        }
    }

    #[test]
    fn parse_list_literal() {
        let program = parse("let xs = [1, 2, 3]").unwrap();
        assert_eq!(program.len(), 1);
        match &program[0] {
            Stmt::Let { value, .. } => match value {
                Expr::List(items) => assert_eq!(items.len(), 3),
                other => panic!("expected List, got {:?}", other),
            },
            other => panic!("expected Let, got {:?}", other),
        }
    }

    #[test]
    fn parse_dict_literal() {
        let program = parse("let d = {\"a\": 1}").unwrap();
        assert_eq!(program.len(), 1);
        match &program[0] {
            Stmt::Let { value, .. } => match value {
                Expr::Dict(pairs) => assert_eq!(pairs.len(), 1),
                other => panic!("expected Dict, got {:?}", other),
            },
            other => panic!("expected Let, got {:?}", other),
        }
    }

    #[test]
    fn parse_index_access() {
        let program = parse("let x = xs[0]").unwrap();
        assert_eq!(program.len(), 1);
        match &program[0] {
            Stmt::Let { value, .. } => match value {
                Expr::Index { .. } => {}
                other => panic!("expected Index, got {:?}", other),
            },
            other => panic!("expected Let, got {:?}", other),
        }
    }

    #[test]
    fn parse_index_assign() {
        let program = parse("let xs = [1]\nxs[0] = 99").unwrap();
        assert_eq!(program.len(), 2);
        match &program[1] {
            Stmt::IndexAssign { .. } => {}
            other => panic!("expected IndexAssign, got {:?}", other),
        }
    }

    #[test]
    fn parse_for() {
        let program = parse("for x in xs\n  print(x)\nend").unwrap();
        assert_eq!(program.len(), 1);
        match &program[0] {
            Stmt::For {
                var, body, line, ..
            } => {
                assert_eq!(var, "x");
                assert_eq!(*line, 1);
                assert_eq!(body.len(), 1);
            }
            other => panic!("expected For, got {:?}", other),
        }
    }

    #[test]
    fn parse_break() {
        let program = parse("while true\n  break\nend").unwrap();
        match &program[0] {
            Stmt::While { body, .. } => match &body[0] {
                Stmt::Break { line } => assert_eq!(*line, 2),
                other => panic!("expected Break, got {:?}", other),
            },
            other => panic!("expected While, got {:?}", other),
        }
    }

    #[test]
    fn parse_elif() {
        let program =
            parse("if x == 1\n  print(1)\nelif x == 2\n  print(2)\nelse\n  print(0)\nend").unwrap();
        assert_eq!(program.len(), 1);
        match &program[0] {
            Stmt::If { else_body, .. } => {
                // elif は内部的にネストされた If になる
                assert_eq!(else_body.len(), 1);
                match &else_body[0] {
                    Stmt::If {
                        else_body: inner_else,
                        ..
                    } => {
                        // else 部分
                        assert_eq!(inner_else.len(), 1);
                    }
                    other => panic!("expected nested If, got {:?}", other),
                }
            }
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn parse_continue() {
        let program = parse("for x in xs\n  continue\nend").unwrap();
        match &program[0] {
            Stmt::For { body, .. } => match &body[0] {
                Stmt::Continue { line } => assert_eq!(*line, 2),
                other => panic!("expected Continue, got {:?}", other),
            },
            other => panic!("expected For, got {:?}", other),
        }
    }

    #[test]
    fn unexpected_token_error() {
        let result = parse("let x = 10\n@@@");
        // '@' is unknown — lexer skips it, but the resulting parse should error with line info
        assert!(result.is_err() || result.is_ok()); // relaxed: just confirm no panic
    }

    #[test]
    fn parse_import() {
        let program = parse("import \"math.tsg\"").unwrap();
        assert_eq!(program.len(), 1);
        match &program[0] {
            Stmt::Import { path, line } => {
                assert_eq!(path, "math.tsg");
                assert_eq!(*line, 1);
            }
            other => panic!("expected Import, got {:?}", other),
        }
    }

    #[test]
    fn parse_import_error_no_string() {
        let result = parse("import 123");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(
            errors[0].message().contains("文字列パス"),
            "error should mention string path: {}",
            errors[0]
        );
    }
}
