/// Tsumugi の抽象構文木（AST）

/// プログラム全体 = 文のリスト
pub type Program = Vec<Stmt>;

/// 文（Statement）
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// let x = expr
    Let { name: String, value: Expr },

    /// return expr
    Return { value: Expr },

    /// if cond ... else ... end
    If {
        condition: Expr,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    },

    /// while cond ... end
    While {
        condition: Expr,
        body: Vec<Stmt>,
    },

    /// fn name(params) ... end
    FnDef {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
    },

    /// 式文（print(x) や add(1,2) など、式だけの行）
    ExprStmt { expr: Expr },
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

    /// 変数参照
    Ident(String),

    /// 二項演算: left op right
    BinOp {
        left: Box<Expr>,
        op: BinOpKind,
        right: Box<Expr>,
    },

    /// 単項演算: not expr, -expr
    UnaryOp {
        op: UnaryOpKind,
        expr: Box<Expr>,
    },

    /// 関数呼び出し: name(args)
    Call {
        name: String,
        args: Vec<Expr>,
    },
}

/// 二項演算子の種類
#[derive(Debug, Clone, PartialEq)]
pub enum BinOpKind {
    Add,    // +
    Sub,    // -
    Mul,    // *
    Div,    // /
    Eq,     // ==
    NotEq,  // !=
    Lt,     // <
    Gt,     // >
    LtEq,   // <=
    GtEq,   // >=
    And,    // and
    Or,     // or
}

/// 単項演算子の種類
#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOpKind {
    Neg, // -
    Not, // not
}
