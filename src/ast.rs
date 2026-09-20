//! Tsumugi の抽象構文木（AST）

/// プログラム全体 = 文のリスト
pub type Program = Vec<Stmt>;

/// ブロック本体（文列）。`Rc<[Stmt]>` で共有する（REV-015 Slice 3 PR-d）。
///
/// PR-d 以前は各ブロック本体を `Vec<Stmt>` で所有していたが、実行を suspend/resume できる
/// 永続 continuation では、frame が自分の実行する文列を「所有」して AST の寿命を保つ必要が
/// ある（呼び出し側の借用 `'p` に依存できない）。関数本体（`FnDef.body`）が既に `Rc<FnDef>`
/// 経由で共有されているのと同様に、全ブロック本体を `Rc<[Stmt]>` にして cheap に共有・保持
/// できるようにする。`Deref` で `&[Stmt]` として読めるため、`.iter()` / `.len()` / index など
/// の読み取り側は不変。定義時の clone は Rc の参照カウント bump のみで、本体 AST は複製しない。
pub type Block = std::rc::Rc<[Stmt]>;

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

    /// name[expr] = expr（インデックス代入）
    ///
    /// 代入対象は識別子に限る。パーサーが `ident[...] =` の形だけを
    /// この文として受理するため、対象が変数でない状態は表現できない。
    IndexAssign {
        name: String,
        index: Expr,
        value: Expr,
        line: usize,
    },

    /// return expr
    Return { value: Expr, line: usize },

    /// if cond ... else ... end
    If {
        condition: Expr,
        then_body: Block,
        else_body: Block,
        line: usize,
    },

    /// while cond ... end
    While {
        condition: Expr,
        body: Block,
        line: usize,
    },

    /// for item in collection ... end
    For {
        var: String,
        iter: Expr,
        body: Block,
        line: usize,
    },

    /// fn name(params) ... end
    FnDef {
        name: String,
        params: Vec<String>,
        body: Block,
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
        try_body: Block,
        var: String,
        catch_body: Block,
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
    Lambda { params: Vec<String>, body: Block },

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
#[derive(Debug, Clone, Copy, PartialEq)]
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
                Stmt::IndexAssign { index, value, .. } => {
                    worklist.push((AstNode::Expr(value), child_depth, line));
                    worklist.push((AstNode::Expr(index), child_depth, line));
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

/// 副作用のない式か判定する（AUD-041のコレクション読み取り最適化に使う）。
///
/// リテラル、識別子、それらの演算、およびそれらのindex参照だけを「副作用なし」とみなす。
/// 関数呼び出し・ラムダ・f-string・コレクションリテラルは含めない。判定がtrueなら、
/// 式の評価がコレクションを変更しないため、コレクションを後から参照で読んでも
/// 観測結果は変わらない。識別子の未定義エラーは評価順に関係なく同じになる。
///
/// AST深度は`MAX_AST_DEPTH`で制限されているため再帰で走査する。
pub(crate) fn is_side_effect_free(expr: &Expr) -> bool {
    match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Ident(_) => true,
        Expr::BinOp { left, right, .. } => is_side_effect_free(left) && is_side_effect_free(right),
        Expr::UnaryOp { expr, .. } => is_side_effect_free(expr),
        Expr::Index { object, index } => is_side_effect_free(object) && is_side_effect_free(index),
        // 呼び出しは任意の副作用を持つ。他は評価コストが読み取り最適化の対象外。
        Expr::Call { .. } | Expr::Lambda { .. } | Expr::List(_) | Expr::Dict(_) | Expr::FStr(_) => {
            false
        }
    }
}

/// クロージャ捕捉用に、本体で言及される識別子名を集める（非再帰）。
///
/// 自由変数の保守的な近似である。`let`で束縛される名前、parameter、`for`変数、
/// `catch`変数も含めるため、「`let`より前で外側の同名bindingを読む」といった
/// 既存の観測挙動を変えない。本体で一度も言及されない名前だけを捕捉対象から外す。
/// ネストした関数・ラムダの本体も辿るので、内側のクロージャが必要とする名前は
/// 外側の関数値が保持する。
pub(crate) fn referenced_names(body: &[Stmt]) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut worklist: Vec<AstNode<'_>> = body.iter().map(AstNode::Stmt).collect();

    while let Some(node) = worklist.pop() {
        match node {
            AstNode::Stmt(stmt) => match stmt {
                Stmt::Let { name, value, .. } | Stmt::Assign { name, value, .. } => {
                    names.insert(name.clone());
                    worklist.push(AstNode::Expr(value));
                }
                Stmt::IndexAssign {
                    name, index, value, ..
                } => {
                    names.insert(name.clone());
                    worklist.push(AstNode::Expr(index));
                    worklist.push(AstNode::Expr(value));
                }
                Stmt::Return { value: expr, .. } | Stmt::ExprStmt { expr, .. } => {
                    worklist.push(AstNode::Expr(expr));
                }
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                    ..
                } => {
                    worklist.push(AstNode::Expr(condition));
                    worklist.extend(then_body.iter().map(AstNode::Stmt));
                    worklist.extend(else_body.iter().map(AstNode::Stmt));
                }
                Stmt::While {
                    condition, body, ..
                } => {
                    worklist.push(AstNode::Expr(condition));
                    worklist.extend(body.iter().map(AstNode::Stmt));
                }
                Stmt::For {
                    var, iter, body, ..
                } => {
                    names.insert(var.clone());
                    worklist.push(AstNode::Expr(iter));
                    worklist.extend(body.iter().map(AstNode::Stmt));
                }
                Stmt::FnDef {
                    name, params, body, ..
                } => {
                    names.insert(name.clone());
                    names.extend(params.iter().cloned());
                    worklist.extend(body.iter().map(AstNode::Stmt));
                }
                Stmt::TryCatch {
                    try_body,
                    var,
                    catch_body,
                    ..
                } => {
                    names.insert(var.clone());
                    worklist.extend(try_body.iter().map(AstNode::Stmt));
                    worklist.extend(catch_body.iter().map(AstNode::Stmt));
                }
                Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Import { .. } => {}
            },
            AstNode::Expr(expr) => match expr {
                Expr::Ident(name) => {
                    names.insert(name.clone());
                }
                Expr::List(items) => worklist.extend(items.iter().map(AstNode::Expr)),
                Expr::Dict(pairs) => {
                    for (key, value) in pairs {
                        worklist.push(AstNode::Expr(key));
                        worklist.push(AstNode::Expr(value));
                    }
                }
                Expr::BinOp { left, right, .. } => {
                    worklist.push(AstNode::Expr(left));
                    worklist.push(AstNode::Expr(right));
                }
                Expr::UnaryOp { expr, .. } => worklist.push(AstNode::Expr(expr)),
                Expr::Call { callee, args } => {
                    worklist.push(AstNode::Expr(callee));
                    worklist.extend(args.iter().map(AstNode::Expr));
                }
                Expr::Lambda { params, body } => {
                    names.extend(params.iter().cloned());
                    worklist.extend(body.iter().map(AstNode::Stmt));
                }
                Expr::Index { object, index } => {
                    worklist.push(AstNode::Expr(object));
                    worklist.push(AstNode::Expr(index));
                }
                Expr::FStr(parts) => {
                    for part in parts {
                        if let FStrExprPart::Expr(child) = part {
                            worklist.push(AstNode::Expr(child));
                        }
                    }
                }
                Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null => {}
            },
        }
    }

    names
}

/// AST program（root + 全 node）の §5.1 論理サイズ合計を計算する（REV-015 PR-d）。
///
/// `AST program root`（64）に、各 Stmt / Expr node の `ast_node(owned_bytes)`
/// （= 64 + その node が所有する identifier / string literal の UTF-8 byte 長）を足す。
/// owned bytes は node が直接保持する `String`（変数名・関数名・parameter 名・import
/// path・文字列リテラル・f-string の literal 部分）の byte 長で、子 node が持つ文字列は
/// その子 node 側で数える（二重計上しない）。深度は `MAX_AST_DEPTH` で有限に抑えられて
/// いるが、ここでは再帰を使わず worklist で走査して host stack を消費しない。
///
/// tree evaluator は AST を直接実行するため、この論理サイズを Link フェーズで live heap
/// へ課金する。VM は AST を bytecode へ compile するため、代わりに bytecode chunk を
/// 課金する（§5.3「AST または bytecode」）。
pub(crate) fn ast_heap_size(program: &Program) -> u64 {
    use crate::budget::heap_size;

    let mut total = heap_size::AST_ROOT;
    let mut worklist: Vec<AstNode<'_>> = program.iter().map(AstNode::Stmt).collect();

    // 文字列 slice の byte 長を saturating で足し込むヘルパ。
    fn owned(bytes: &mut u64, s: &str) {
        *bytes = bytes.saturating_add(s.len() as u64);
    }

    while let Some(node) = worklist.pop() {
        let mut owned_bytes = 0u64;
        match node {
            AstNode::Stmt(stmt) => match stmt {
                Stmt::Let { name, value, .. } | Stmt::Assign { name, value, .. } => {
                    owned(&mut owned_bytes, name);
                    worklist.push(AstNode::Expr(value));
                }
                Stmt::IndexAssign {
                    name, index, value, ..
                } => {
                    owned(&mut owned_bytes, name);
                    worklist.push(AstNode::Expr(index));
                    worklist.push(AstNode::Expr(value));
                }
                Stmt::Return { value, .. } | Stmt::ExprStmt { expr: value, .. } => {
                    worklist.push(AstNode::Expr(value));
                }
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                    ..
                } => {
                    worklist.push(AstNode::Expr(condition));
                    worklist.extend(then_body.iter().map(AstNode::Stmt));
                    worklist.extend(else_body.iter().map(AstNode::Stmt));
                }
                Stmt::While {
                    condition, body, ..
                } => {
                    worklist.push(AstNode::Expr(condition));
                    worklist.extend(body.iter().map(AstNode::Stmt));
                }
                Stmt::For {
                    var, iter, body, ..
                } => {
                    owned(&mut owned_bytes, var);
                    worklist.push(AstNode::Expr(iter));
                    worklist.extend(body.iter().map(AstNode::Stmt));
                }
                Stmt::FnDef {
                    name, params, body, ..
                } => {
                    owned(&mut owned_bytes, name);
                    for p in params {
                        owned(&mut owned_bytes, p);
                    }
                    worklist.extend(body.iter().map(AstNode::Stmt));
                }
                Stmt::Import { path, .. } => owned(&mut owned_bytes, path),
                Stmt::TryCatch {
                    try_body,
                    var,
                    catch_body,
                    ..
                } => {
                    owned(&mut owned_bytes, var);
                    worklist.extend(try_body.iter().map(AstNode::Stmt));
                    worklist.extend(catch_body.iter().map(AstNode::Stmt));
                }
                Stmt::Break { .. } | Stmt::Continue { .. } => {}
            },
            AstNode::Expr(expr) => match expr {
                Expr::Str(s) => owned(&mut owned_bytes, s),
                Expr::Ident(name) => owned(&mut owned_bytes, name),
                Expr::List(items) => worklist.extend(items.iter().map(AstNode::Expr)),
                Expr::Dict(pairs) => {
                    for (key, value) in pairs {
                        worklist.push(AstNode::Expr(key));
                        worklist.push(AstNode::Expr(value));
                    }
                }
                Expr::BinOp { left, right, .. } => {
                    worklist.push(AstNode::Expr(left));
                    worklist.push(AstNode::Expr(right));
                }
                Expr::UnaryOp { expr, .. } => worklist.push(AstNode::Expr(expr)),
                Expr::Call { callee, args } => {
                    worklist.push(AstNode::Expr(callee));
                    worklist.extend(args.iter().map(AstNode::Expr));
                }
                Expr::Lambda { params, body } => {
                    for p in params {
                        owned(&mut owned_bytes, p);
                    }
                    worklist.extend(body.iter().map(AstNode::Stmt));
                }
                Expr::Index { object, index } => {
                    worklist.push(AstNode::Expr(object));
                    worklist.push(AstNode::Expr(index));
                }
                Expr::FStr(parts) => {
                    for part in parts {
                        match part {
                            FStrExprPart::Literal(s) => owned(&mut owned_bytes, s),
                            FStrExprPart::Expr(child) => worklist.push(AstNode::Expr(child)),
                        }
                    }
                }
                Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Null => {}
            },
        }
        total = total.saturating_add(heap_size::ast_node(owned_bytes));
    }

    total
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

#[cfg(test)]
mod tests {
    use super::*;

    /// sourceの最初の関数定義の本体から、捕捉対象の名前を集める
    fn names_of_first_fn_body(source: &str) -> std::collections::HashSet<String> {
        let tokens = crate::lexer::Lexer::new(source).tokenize();
        let program = crate::parser::Parser::new(tokens)
            .parse()
            .expect("パースに失敗");
        for stmt in &program {
            if let Stmt::FnDef { body, .. } = stmt {
                return referenced_names(body);
            }
        }
        panic!("関数定義が見つかりません");
    }

    #[test]
    fn referenced_names_collects_reads_writes_and_callees() {
        let names = names_of_first_fn_body(
            "fn f(p)\n  let local = outer + p\n  assigned = local\n  target[0] = 1\n  let shown = helper(local)\n  return f\"{shown}\"\nend",
        );

        for expected in [
            "p", "local", "outer", "assigned", "target", "helper", "shown",
        ] {
            assert!(
                names.contains(expected),
                "{expected} が集められていない: {names:?}"
            );
        }
        assert!(!names.contains("unrelated"));
    }

    fn parse_program(source: &str) -> Program {
        let tokens = crate::lexer::Lexer::new(source).tokenize();
        crate::parser::Parser::new(tokens)
            .parse()
            .expect("パースに失敗")
    }

    #[test]
    fn ast_heap_size_counts_root_nodes_and_owned_bytes() {
        use crate::budget::heap_size;

        // 単一の式文 `1`: root(64) + ExprStmt node(64) + Int node(64、owned 0)。
        let program = parse_program("1\n");
        assert_eq!(
            ast_heap_size(&program),
            heap_size::AST_ROOT + heap_size::ast_node(0) + heap_size::ast_node(0)
        );

        // `let x = "ab"`: root + Let node(owned "x"=1) + Str node(owned "ab"=2)。
        let program = parse_program("let x = \"ab\"\n");
        assert_eq!(
            ast_heap_size(&program),
            heap_size::AST_ROOT + heap_size::ast_node(1) + heap_size::ast_node(2)
        );

        // 識別子参照は名前の byte 長を owned として数える。
        // `let name = value`: root + Let("name"=4) + Ident("value"=5)。
        let program = parse_program("let name = value\n");
        assert_eq!(
            ast_heap_size(&program),
            heap_size::AST_ROOT + heap_size::ast_node(4) + heap_size::ast_node(5)
        );
    }

    #[test]
    fn ast_heap_size_is_monotonic_in_program_size() {
        // 文を増やすほど論理サイズは単調増加する（root は 1 回だけ）。
        let small = ast_heap_size(&parse_program("let a = 1\n"));
        let large = ast_heap_size(&parse_program("let a = 1\nlet b = 2\nlet c = 3\n"));
        assert!(large > small, "AST が大きいほど論理サイズも大きいはず");
    }

    #[test]
    fn referenced_names_includes_nested_bodies() {
        let names = names_of_first_fn_body(
            "fn f()\n  let g = fn()\n    return fn() deep end\n  end\n  for item in items\n    try\n      let doubled = item\n    catch err\n      let kind = err\n    end\n  end\n  return 1 + 2\nend",
        );

        for expected in ["g", "deep", "item", "items", "err"] {
            assert!(
                names.contains(expected),
                "{expected} が集められていない: {names:?}"
            );
        }
    }
}
