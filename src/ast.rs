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

    /// 関数呼び出し: name(args)
    Call { name: String, args: Vec<Expr> },

    /// インデックスアクセス: expr[expr]
    Index { object: Box<Expr>, index: Box<Expr> },
}

/// 二項演算子の種類
#[derive(Debug, Clone, PartialEq)]
pub enum BinOpKind {
    Add,   // +
    Sub,   // -
    Mul,   // *
    Div,   // /
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
