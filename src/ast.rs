//! Tsumugi の抽象構文木（AST）

/// プログラム全体 = 文のリスト
pub type Program = Vec<Stmt>;

/// 文（Statement）
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// let x = expr
    Let {
        name: String,
        value: Expr,
        line: usize,
    },

    /// x = expr（再代入）
    Assign {
        name: String,
        value: Expr,
        line: usize,
    },

    /// expr[expr] = expr（インデックス代入）
    IndexAssign {
        object: Expr,
        index: Expr,
        value: Expr,
        line: usize,
    },

    /// return expr
    Return { value: Expr, line: usize },

    /// if cond ... else ... end
    If {
        condition: Expr,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
        line: usize,
    },

    /// while cond ... end
    While {
        condition: Expr,
        body: Vec<Stmt>,
        line: usize,
    },

    /// for item in collection ... end
    For {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
        line: usize,
    },

    /// fn name(params) ... end
    FnDef {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
        line: usize,
    },

    /// break
    Break { line: usize },

    /// continue
    Continue { line: usize },

    /// import "path"
    Import { path: String, line: usize },

    /// try ... catch var ... end
    TryCatch {
        try_body: Vec<Stmt>,
        var: String,
        catch_body: Vec<Stmt>,
        line: usize,
    },

    /// 式文（print(x) や add(1,2) など、式だけの行）
    #[allow(clippy::enum_variant_names)]
    ExprStmt { expr: Expr, line: usize },
}

impl Stmt {
    /// 文の行番号を取得
    #[allow(dead_code)]
    pub fn line(&self) -> usize {
        match self {
            Stmt::Let { line, .. } => *line,
            Stmt::Assign { line, .. } => *line,
            Stmt::IndexAssign { line, .. } => *line,
            Stmt::Return { line, .. } => *line,
            Stmt::If { line, .. } => *line,
            Stmt::While { line, .. } => *line,
            Stmt::For { line, .. } => *line,
            Stmt::FnDef { line, .. } => *line,
            Stmt::Break { line } => *line,
            Stmt::Continue { line } => *line,
            Stmt::Import { line, .. } => *line,
            Stmt::TryCatch { line, .. } => *line,
            Stmt::ExprStmt { line, .. } => *line,
        }
    }
}

/// 式（Expression）
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// 整数リテラル
    Int(i64),

    /// 浮動小数点リテラル
    Float(f64),

    /// 文字列リテラル
    Str(String),

    /// 真偽値
    Bool(bool),

    /// null
    Null,

    /// リストリテラル: [expr, expr, ...]
    List(Vec<Expr>),

    /// 辞書リテラル: {"key": expr, ...}
    Dict(Vec<(Expr, Expr)>),

    /// 変数参照
    Ident(String),

    /// 二項演算: left op right
    BinOp {
        left: Box<Expr>,
        op: BinOpKind,
        right: Box<Expr>,
    },

    /// 単項演算: not expr, -expr
    UnaryOp { op: UnaryOpKind, expr: Box<Expr> },

    /// 関数呼び出し: expr(args)
    Call { callee: Box<Expr>, args: Vec<Expr> },

    /// 無名関数（ラムダ）: fn(params) body end
    Lambda {
        params: Vec<String>,
        body: Vec<Stmt>,
    },

    /// インデックスアクセス: expr[expr]
    Index { object: Box<Expr>, index: Box<Expr> },

    /// f-string: f"hello, {expr}"
    /// 各パートはリテラル文字列か式
    FStr(Vec<FStrExprPart>),
}

/// f-string の AST パート
#[derive(Debug, Clone, PartialEq)]
pub enum FStrExprPart {
    /// リテラル部分
    Literal(String),
    /// 式部分（評価して文字列化する）
    Expr(Box<Expr>),
}

/// 二項演算子の種類
#[derive(Debug, Clone, PartialEq)]
pub enum BinOpKind {
    Add,   // +
    Sub,   // -
    Mul,   // *
    Div,   // /
    Mod,   // %
    Eq,    // ==
    NotEq, // !=
    Lt,    // <
    Gt,    // >
    LtEq,  // <=
    GtEq,  // >=
    And,   // and
    Or,    // or
}

/// 単項演算子の種類
#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOpKind {
    Neg, // -
    Not, // not
}

/// 深度検証用の非再帰work item。
enum AstNode<'a> {
    Stmt(&'a Stmt),
    Expr(&'a Expr),
}

/// ASTを非再帰で走査し、上限を超えた最初のノードの行番号を返す。
fn excessive_depth_line(mut worklist: Vec<(AstNode<'_>, usize, usize)>) -> Option<usize> {
    use crate::limits::MAX_AST_DEPTH;

    while let Some((node, depth, line)) = worklist.pop() {
        if depth > MAX_AST_DEPTH {
            return Some(line);
        }

        let child_depth = depth + 1;
        match node {
            AstNode::Stmt(stmt) => match stmt {
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::Return { value, .. } => {
                    worklist.push((AstNode::Expr(value), child_depth, line));
                }
                Stmt::IndexAssign {
                    object,
                    index,
                    value,
                    ..
                } => {
                    worklist.push((AstNode::Expr(value), child_depth, line));
                    worklist.push((AstNode::Expr(index), child_depth, line));
                    worklist.push((AstNode::Expr(object), child_depth, line));
                }
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                    ..
                } => {
                    for child in else_body.iter().rev() {
                        worklist.push((AstNode::Stmt(child), child_depth, child.line()));
                    }
                    for child in then_body.iter().rev() {
                        worklist.push((AstNode::Stmt(child), child_depth, child.line()));
                    }
                    worklist.push((AstNode::Expr(condition), child_depth, line));
                }
                Stmt::While {
                    condition, body, ..
                } => {
                    for child in body.iter().rev() {
                        worklist.push((AstNode::Stmt(child), child_depth, child.line()));
                    }
                    worklist.push((AstNode::Expr(condition), child_depth, line));
                }
                Stmt::For { iter, body, .. } => {
                    for child in body.iter().rev() {
                        worklist.push((AstNode::Stmt(child), child_depth, child.line()));
                    }
                    worklist.push((AstNode::Expr(iter), child_depth, line));
                }
                Stmt::FnDef { body, .. } => {
                    for child in body.iter().rev() {
                        worklist.push((AstNode::Stmt(child), child_depth, child.line()));
                    }
                }
                Stmt::TryCatch {
                    try_body,
                    catch_body,
                    ..
                } => {
                    for child in catch_body.iter().rev() {
                        worklist.push((AstNode::Stmt(child), child_depth, child.line()));
                    }
                    for child in try_body.iter().rev() {
                        worklist.push((AstNode::Stmt(child), child_depth, child.line()));
                    }
                }
                Stmt::ExprStmt { expr, .. } => {
                    worklist.push((AstNode::Expr(expr), child_depth, line));
                }
                Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Import { .. } => {}
            },
            AstNode::Expr(expr) => match expr {
                Expr::List(items) => {
                    for child in items.iter().rev() {
                        worklist.push((AstNode::Expr(child), child_depth, line));
                    }
                }
                Expr::Dict(pairs) => {
                    for (key, value) in pairs.iter().rev() {
                        worklist.push((AstNode::Expr(value), child_depth, line));
                        worklist.push((AstNode::Expr(key), child_depth, line));
                    }
                }
                Expr::BinOp { left, right, .. } => {
                    worklist.push((AstNode::Expr(right), child_depth, line));
                    worklist.push((AstNode::Expr(left), child_depth, line));
                }
                Expr::UnaryOp { expr, .. } => {
                    worklist.push((AstNode::Expr(expr), child_depth, line));
                }
                Expr::Call { callee, args } => {
                    for child in args.iter().rev() {
                        worklist.push((AstNode::Expr(child), child_depth, line));
                    }
                    worklist.push((AstNode::Expr(callee), child_depth, line));
                }
                Expr::Lambda { body, .. } => {
                    for child in body.iter().rev() {
                        worklist.push((AstNode::Stmt(child), child_depth, child.line()));
                    }
                }
                Expr::Index { object, index } => {
                    worklist.push((AstNode::Expr(index), child_depth, line));
                    worklist.push((AstNode::Expr(object), child_depth, line));
                }
                Expr::FStr(parts) => {
                    for part in parts.iter().rev() {
                        if let FStrExprPart::Expr(child) = part {
                            worklist.push((AstNode::Expr(child), child_depth, line));
                        }
                    }
                }
                Expr::Int(_)
                | Expr::Float(_)
                | Expr::Str(_)
                | Expr::Bool(_)
                | Expr::Null
                | Expr::Ident(_) => {}
            },
        }
    }

    None
}

/// Parserが複合式を構築するたびに、危険な深さへ到達していないか確認する。
pub(crate) fn expr_depth_exceeds_limit(expr: &Expr) -> bool {
    excessive_depth_line(vec![(AstNode::Expr(expr), 1, 0)]).is_some()
}

/// Parserが文を構築した時点で、文と配下の式・blockをまとめて確認する。
pub(crate) fn stmt_depth_exceeds_limit(stmt: &Stmt) -> bool {
    excessive_depth_line(vec![(AstNode::Stmt(stmt), 1, stmt.line())]).is_some()
}

/// Compiler/Evaluatorへ渡されたProgramを、再帰処理へ入る前に検証する。
pub(crate) fn validate_program_depth(program: &Program) -> Result<(), crate::error::TsumugiError> {
    use crate::error::{ErrorKind, TsumugiError};
    use crate::limits::MAX_AST_DEPTH;

    let worklist = program
        .iter()
        .rev()
        .map(|stmt| (AstNode::Stmt(stmt), 1, stmt.line()))
        .collect();

    if let Some(line) = excessive_depth_line(worklist) {
        return Err(TsumugiError::runtime_with_kind(
            line,
            ErrorKind::StackOverflow,
            format!("ASTのネストが深すぎます (上限: {})", MAX_AST_DEPTH),
        ));
    }

    Ok(())
}
