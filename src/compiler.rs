//! コンパイラ: AST → バイトコード（Chunk）に変換

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::ast::{BinOpKind, Expr, FStrExprPart, Program, Stmt, UnaryOpKind};
use crate::chunk::Chunk;
use crate::error::TsumugiError;
use crate::opcode::OpCode;
use crate::value::Value;

/// ローカル変数の情報
#[derive(Debug, Clone)]
struct Local {
    name: String,
    /// スコープの深さ（0 = トップレベル）
    depth: usize,
}

/// ループのコンパイル状態（break/continue のパッチに使う）
#[derive(Debug)]
struct LoopState {
    /// continue の飛び先（while: 条件チェック先頭、for: インクリメント先頭）
    /// for では本体コンパイル後にパッチする
    continue_target: usize,
    /// break のジャンプ命令位置（ループ後にパッチする）
    breaks: Vec<usize>,
    /// continue のジャンプ命令位置（for ループ用: インクリメント位置にパッチ）
    continues: Vec<usize>,
    /// ループ開始時のローカル変数数（break/continue 時のスタック巻き戻しに使う）
    locals_count: usize,
    /// continue を固定先に飛ばすか（while=true）、パッチするか（for=false）
    continue_resolved: bool,
    /// ループ内で現在アクティブな try ブロックの深さ
    /// break/continue 時にこの数だけ TeardownTry を発行する
    try_depth: usize,
}

#[derive(Debug, Clone)]
struct Upvalue {
    /// 外側のコンパイラでのスロット位置
    /// is_local=true の場合: 親のローカル変数スロット
    /// is_local=false の場合: 親の upvalue インデックス
    slot: usize,
    /// 親のローカル変数を直接キャプチャするか（false なら親の upvalue を経由）
    is_local: bool,
    /// 変数名（デバッグ用）
    #[allow(dead_code)]
    name: String,
}

/// AST をバイトコードにコンパイルする
pub struct Compiler {
    chunk: Chunk,
    /// ローカル変数テーブル（スタック上の位置 = Vec のインデックス）
    locals: Vec<Local>,
    /// 現在のスコープの深さ
    scope_depth: usize,
    /// ループのネスト管理
    loops: Vec<LoopState>,
    /// この関数がキャプチャする upvalue のリスト
    upvalues: Vec<Upvalue>,
    /// 親コンパイラのローカル変数（クロージャ用）
    enclosing_locals: Option<Vec<Local>>,
    /// 親コンパイラの upvalue リスト（多段キャプチャ用）
    enclosing_upvalues: Option<Vec<Upvalue>>,
    /// 現在のスクリプトの基準ディレクトリ（import のパス解決に使用）
    base_dir: PathBuf,
    /// import 済みファイルの正規パス集合（循環 import 防止）
    imported: HashSet<PathBuf>,
}

impl Compiler {
    pub fn new() -> Self {
        Compiler {
            chunk: Chunk::new(),
            locals: Vec::new(),
            scope_depth: 0,
            loops: Vec::new(),
            upvalues: Vec::new(),
            enclosing_locals: None,
            enclosing_upvalues: None,
            base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            imported: HashSet::new(),
        }
    }

    /// 基準ディレクトリを設定する（ファイル実行時に呼ばれる）
    pub fn set_base_dir(&mut self, path: &Path) {
        if let Some(parent) = path.parent() {
            self.base_dir = parent.to_path_buf();
        }
        if let Ok(canonical) = std::fs::canonicalize(path) {
            self.imported.insert(canonical);
        }
    }

    /// 子コンパイラを作成（親のローカル変数と祖先のスコープを参照可能に）
    fn new_enclosed(enclosing_locals: Vec<Local>, enclosing_upvalues: Vec<Upvalue>) -> Self {
        Compiler {
            chunk: Chunk::new(),
            locals: Vec::new(),
            scope_depth: 0,
            loops: Vec::new(),
            upvalues: Vec::new(),
            enclosing_locals: Some(enclosing_locals),
            enclosing_upvalues: Some(enclosing_upvalues),
            base_dir: PathBuf::from("."),
            imported: HashSet::new(),
        }
    }

    /// プログラム全体をコンパイルして Chunk を返す
    pub fn compile(mut self, program: &Program) -> Result<Chunk, TsumugiError> {
        for stmt in program {
            self.compile_stmt(stmt)?;
        }
        self.chunk.emit(OpCode::Return, 0);
        Ok(self.chunk)
    }

    /// 文をコンパイル
    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), TsumugiError> {
        match stmt {
            Stmt::Let { name, value, line } => {
                self.compile_expr(value, *line)?;
                // 同じスコープに同名の変数が既にあれば上書き（ツリーウォーク版互換）
                if let Some(slot) = self.find_local_in_current_scope(name) {
                    self.chunk.emit(OpCode::SetLocal(slot), *line);
                    self.chunk.emit(OpCode::Pop, *line);
                } else {
                    self.add_local(name.clone());
                }
            }
            Stmt::Assign { name, value, line } => {
                self.compile_expr(value, *line)?;
                if let Ok(slot) = self.resolve_local(name, *line) {
                    self.chunk.emit(OpCode::SetLocal(slot), *line);
                } else if let Some(upvalue_idx) = self.resolve_upvalue(name) {
                    self.chunk.emit(OpCode::SetUpvalue(upvalue_idx), *line);
                } else {
                    return Err(TsumugiError::runtime_with_kind(
                        *line,
                        crate::error::ErrorKind::Name,
                        format!("未定義の変数に代入: {}", name),
                    ));
                }
                self.chunk.emit(OpCode::Pop, *line);
            }
            Stmt::IndexAssign {
                object,
                index,
                value,
                line,
            } => {
                // object が変数参照の場合、更新後の値を元のスロットに書き戻す
                if let Expr::Ident(name) = object {
                    let slot = self.resolve_local(name, *line)?;
                    self.chunk.emit(OpCode::GetLocal(slot), *line);
                    self.compile_expr(index, *line)?;
                    self.compile_expr(value, *line)?;
                    self.chunk.emit(OpCode::SetIndex, *line);
                    self.chunk.emit(OpCode::SetLocal(slot), *line);
                    self.chunk.emit(OpCode::Pop, *line);
                } else {
                    // ネストされたインデックス代入は未サポート
                    return Err(TsumugiError::runtime_with_kind(
                        *line,
                        crate::error::ErrorKind::Runtime,
                        "VM未対応: ネストされたインデックス代入",
                    ));
                }
            }
            Stmt::ExprStmt { expr, line } => {
                self.compile_expr(expr, *line)?;
                self.chunk.emit(OpCode::Pop, *line);
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
                line,
            } => {
                self.compile_if(condition, then_body, else_body, *line)?;
            }
            Stmt::While {
                condition,
                body,
                line,
            } => {
                self.compile_while(condition, body, *line)?;
            }
            Stmt::For {
                var,
                iter,
                body,
                line,
            } => {
                self.compile_for(var, iter, body, *line)?;
            }
            Stmt::Break { line } => {
                self.compile_break(*line)?;
            }
            Stmt::Continue { line } => {
                self.compile_continue(*line)?;
            }
            Stmt::FnDef {
                name,
                params,
                body,
                line,
            } => {
                self.compile_fn_def(name, params, body, *line)?;
            }
            Stmt::Return { value, line } => {
                self.compile_expr(value, *line)?;
                self.chunk.emit(OpCode::ReturnValue, *line);
            }
            Stmt::Import { path, line } => {
                self.compile_import(path, *line)?;
            }
            Stmt::TryCatch {
                try_body,
                var,
                catch_body,
                line,
            } => {
                self.compile_try_catch(try_body, var, catch_body, *line)?;
            }
        }
        Ok(())
    }

    // --- 制御フロー ---

    /// import 文のコンパイル（ファイルを読み込んでインラインでコンパイル）
    fn compile_import(&mut self, path: &str, line: usize) -> Result<(), TsumugiError> {
        use crate::lexer::Lexer;
        use crate::parser::Parser;

        // パス解決
        let resolved = self.base_dir.join(path);
        let canonical = std::fs::canonicalize(&resolved).map_err(|e| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Import,
                format!("import 失敗: ファイルが見つかりません: {} ({})", path, e),
            )
        })?;

        // サンドボックスチェック: import 先が許可範囲内か検証
        crate::sandbox::check_path(canonical.to_str().unwrap_or(""), line)?;

        // 循環 import チェック（既に import 済みならスキップ）
        if self.imported.contains(&canonical) {
            return Ok(());
        }
        self.imported.insert(canonical.clone());

        // ファイル読み込み
        let source = std::fs::read_to_string(&canonical).map_err(|e| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Import,
                format!("import 失敗: ファイルを読み込めません: {} ({})", path, e),
            )
        })?;

        // base_dir を一時的に切り替え
        let prev_base_dir = self.base_dir.clone();
        if let Some(parent) = canonical.parent() {
            self.base_dir = parent.to_path_buf();
        }

        // パース
        let mut lexer = Lexer::new(&source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().map_err(|errors| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Import,
                format!(
                    "import 失敗 ({}): {}",
                    path,
                    errors
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            )
        })?;

        // import 先の文をインラインでコンパイル（現在のチャンクに直接追加）
        for stmt in &program {
            self.compile_stmt(stmt)?;
        }

        // base_dir を復元
        self.base_dir = prev_base_dir;

        Ok(())
    }

    /// try / catch のコンパイル
    fn compile_try_catch(
        &mut self,
        try_body: &[Stmt],
        var: &str,
        catch_body: &[Stmt],
        line: usize,
    ) -> Result<(), TsumugiError> {
        // SetupTry: catch ブロックの先頭アドレスを後でパッチ
        let setup_offset = self.chunk.len();
        self.chunk.emit(OpCode::SetupTry(0), line);

        // ループ内の try_depth をインクリメント（break/continue の TeardownTry 発行用）
        if let Some(loop_state) = self.loops.last_mut() {
            loop_state.try_depth += 1;
        }

        // try ブロック
        self.begin_scope();
        for stmt in try_body {
            self.compile_stmt(stmt)?;
        }
        self.end_scope(line);

        // try 正常完了: ハンドラを解除して catch をスキップ
        self.chunk.emit(OpCode::TeardownTry, line);

        // ループ内の try_depth をデクリメント
        if let Some(loop_state) = self.loops.last_mut() {
            loop_state.try_depth -= 1;
        }

        let jump_over_catch = self.chunk.emit_jump(OpCode::Jump(0), line);

        // catch ブロックの先頭アドレスをパッチ
        let catch_start = self.chunk.len();
        if let OpCode::SetupTry(ref mut addr) = self.chunk.code[setup_offset] {
            *addr = catch_start;
        }

        // catch ブロック: エラーメッセージがスタックトップに積まれた状態で開始
        self.begin_scope();
        // エラーメッセージをローカル変数として登録
        self.add_local(var.to_string());
        for stmt in catch_body {
            self.compile_stmt(stmt)?;
        }
        self.end_scope(line);

        // catch 後への合流点
        self.chunk.patch_jump(jump_over_catch);

        Ok(())
    }

    /// if / elif / else のコンパイル
    fn compile_if(
        &mut self,
        condition: &Expr,
        then_body: &[Stmt],
        else_body: &[Stmt],
        line: usize,
    ) -> Result<(), TsumugiError> {
        // 条件式をコンパイル
        self.compile_expr(condition, line)?;
        // 偽なら else ブロックへジャンプ（飛び先は後でパッチ）
        let jump_to_else = self.chunk.emit_jump(OpCode::JumpIfFalse(0), line);

        // then ブロック
        self.begin_scope();
        for stmt in then_body {
            self.compile_stmt(stmt)?;
        }
        self.end_scope(line);

        // then の末尾で else の後ろへジャンプ（else がある場合のみ）
        if !else_body.is_empty() {
            let jump_over_else = self.chunk.emit_jump(OpCode::Jump(0), line);
            self.chunk.patch_jump(jump_to_else);

            // else ブロック（elif は Parser が再帰的に If ノードに変換済み）
            self.begin_scope();
            for stmt in else_body {
                self.compile_stmt(stmt)?;
            }
            self.end_scope(line);

            self.chunk.patch_jump(jump_over_else);
        } else {
            self.chunk.patch_jump(jump_to_else);
        }

        Ok(())
    }

    /// while ループのコンパイル
    fn compile_while(
        &mut self,
        condition: &Expr,
        body: &[Stmt],
        line: usize,
    ) -> Result<(), TsumugiError> {
        let loop_start = self.chunk.len();

        // ループ状態を push（while では continue = 条件チェック先頭）
        self.loops.push(LoopState {
            continue_target: loop_start,
            breaks: Vec::new(),
            continues: Vec::new(),
            locals_count: self.locals.len(),
            continue_resolved: true,
            try_depth: 0,
        });

        // 条件式
        self.compile_expr(condition, line)?;
        let exit_jump = self.chunk.emit_jump(OpCode::JumpIfFalse(0), line);

        // ループ本体（ブロックスコープあり）
        self.begin_scope();
        for stmt in body {
            self.compile_stmt(stmt)?;
        }
        self.end_scope(line);

        // ループ先頭に戻る
        self.chunk.emit(OpCode::Loop(loop_start), line);

        // ループ脱出先をパッチ
        self.chunk.patch_jump(exit_jump);

        // break のパッチ
        let loop_state = self.loops.pop().unwrap();
        for break_offset in loop_state.breaks {
            self.chunk.patch_jump(break_offset);
        }

        Ok(())
    }

    /// for ループのコンパイル
    /// `for item in collection` → コレクションを評価し、インデックス変数と共にwhile的に展開
    fn compile_for(
        &mut self,
        var: &str,
        iter: &Expr,
        body: &[Stmt],
        line: usize,
    ) -> Result<(), TsumugiError> {
        // スコープ開始（コレクション・インデックス・ループ変数が入る）
        self.begin_scope();

        // コレクションを評価してスタックに積む（隠しローカル変数として）
        self.compile_expr(iter, line)?;
        // 辞書/文字列をリストに変換（リストはそのまま）
        self.chunk.emit(OpCode::ToIterList, line);
        let collection_slot = self.locals.len();
        self.add_local("__collection__".to_string());

        // インデックスカウンタ (0) を積む
        self.chunk.emit_constant(Value::Int(0), line);
        let index_slot = self.locals.len();
        self.add_local("__index__".to_string());

        // ループ変数を null で初期化
        self.chunk.emit_constant(Value::Null, line);
        let var_slot = self.locals.len();
        self.add_local(var.to_string());

        // ループ先頭
        let loop_start = self.chunk.len();
        // for では continue_target を後で設定する（インクリメント位置）
        self.loops.push(LoopState {
            continue_target: 0, // 後でパッチ
            breaks: Vec::new(),
            continues: Vec::new(),
            locals_count: self.locals.len(),
            continue_resolved: false,
            try_depth: 0,
        });

        // 条件: index < len(collection)
        self.chunk.emit(OpCode::GetLocal(index_slot), line);
        self.chunk.emit(OpCode::GetLocal(collection_slot), line);
        self.chunk.emit(OpCode::Len, line);
        self.chunk.emit(OpCode::Lt, line);
        let exit_jump = self.chunk.emit_jump(OpCode::JumpIfFalse(0), line);

        // ループ変数 = collection[index]
        self.chunk.emit(OpCode::GetLocal(collection_slot), line);
        self.chunk.emit(OpCode::GetLocal(index_slot), line);
        self.chunk.emit(OpCode::Index, line);
        self.chunk.emit(OpCode::SetLocal(var_slot), line);
        self.chunk.emit(OpCode::Pop, line);

        // ループ本体（ブロックスコープあり）
        self.begin_scope();
        for stmt in body {
            self.compile_stmt(stmt)?;
        }
        self.end_scope(line);

        // continue の飛び先 = インクリメント処理の先頭
        let increment_start = self.chunk.len();
        // LoopState の continue_target をパッチ
        self.loops.last_mut().unwrap().continue_target = increment_start;

        // index = index + 1
        self.chunk.emit(OpCode::GetLocal(index_slot), line);
        self.chunk.emit_constant(Value::Int(1), line);
        self.chunk.emit(OpCode::Add, line);
        self.chunk.emit(OpCode::SetLocal(index_slot), line);
        self.chunk.emit(OpCode::Pop, line);

        // ループ先頭に戻る
        self.chunk.emit(OpCode::Loop(loop_start), line);

        // 脱出先パッチ
        self.chunk.patch_jump(exit_jump);

        // break / continue パッチ
        let loop_state = self.loops.pop().unwrap();
        for break_offset in loop_state.breaks {
            self.chunk.patch_jump(break_offset);
        }
        // continue のジャンプ先をインクリメント位置にパッチ
        for cont_offset in loop_state.continues {
            // Loop 命令のターゲットをパッチ
            if let OpCode::Loop(ref mut target) = self.chunk.code[cont_offset] {
                *target = increment_start;
            }
        }

        // for スコープ終了（collection, index, var を pop）
        self.end_scope(line);

        Ok(())
    }

    /// break のコンパイル
    fn compile_break(&mut self, line: usize) -> Result<(), TsumugiError> {
        let loop_state = self.loops.last().ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::ControlFlow,
                "break はループの中でのみ使用できます",
            )
        })?;

        // break 時にアクティブな try ハンドラを解除する
        let try_depth = loop_state.try_depth;
        for _ in 0..try_depth {
            self.chunk.emit(OpCode::TeardownTry, line);
        }

        // ループ内で宣言されたローカル変数をクリーンアップ
        let locals_to_pop = self.locals.len() - loop_state.locals_count;
        if locals_to_pop > 0 {
            self.chunk.emit(OpCode::PopN(locals_to_pop), line);
        }

        // ジャンプ命令を仮で発行（ループ終了後にパッチ）
        let jump_offset = self.chunk.emit_jump(OpCode::Jump(0), line);
        // loop_state を再度可変で取得（borrow checker 対策）
        self.loops.last_mut().unwrap().breaks.push(jump_offset);
        Ok(())
    }

    /// continue のコンパイル
    fn compile_continue(&mut self, line: usize) -> Result<(), TsumugiError> {
        let loop_state = self.loops.last().ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::ControlFlow,
                "continue はループの中でのみ使用できます",
            )
        })?;
        let continue_target = loop_state.continue_target;
        let continue_resolved = loop_state.continue_resolved;

        // continue 時にアクティブな try ハンドラを解除する
        let try_depth = loop_state.try_depth;
        for _ in 0..try_depth {
            self.chunk.emit(OpCode::TeardownTry, line);
        }

        // ループ内で宣言されたローカル変数をクリーンアップ
        let locals_to_pop = self.locals.len() - loop_state.locals_count;
        if locals_to_pop > 0 {
            self.chunk.emit(OpCode::PopN(locals_to_pop), line);
        }

        if continue_resolved {
            // while: 飛び先が確定している
            self.chunk.emit(OpCode::Loop(continue_target), line);
        } else {
            // for: 飛び先が未確定なので仮で発行して後でパッチ
            let offset = self.chunk.len();
            self.chunk.emit(OpCode::Loop(0), line);
            self.loops.last_mut().unwrap().continues.push(offset);
        }
        Ok(())
    }

    /// 式をコンパイル（結果はスタックトップに積まれる）
    fn compile_expr(&mut self, expr: &Expr, line: usize) -> Result<(), TsumugiError> {
        match expr {
            Expr::Int(n) => {
                self.chunk.emit_constant(Value::Int(*n), line);
            }
            Expr::Float(n) => {
                self.chunk.emit_constant(Value::Float(*n), line);
            }
            Expr::Str(s) => {
                self.chunk.emit_constant(Value::Str(s.clone()), line);
            }
            Expr::Bool(b) => {
                self.chunk.emit_constant(Value::Bool(*b), line);
            }
            Expr::Null => {
                self.chunk.emit_constant(Value::Null, line);
            }
            Expr::Ident(name) => {
                // ローカル変数を検索
                if let Ok(slot) = self.resolve_local(name, line) {
                    self.chunk.emit(OpCode::GetLocal(slot), line);
                } else if let Some(upvalue_idx) = self.resolve_upvalue(name) {
                    // upvalue（外側のスコープの変数）
                    self.chunk.emit(OpCode::GetUpvalue(upvalue_idx), line);
                } else {
                    return Err(TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Name,
                        format!("未定義の変数: {}", name),
                    ));
                }
            }
            Expr::BinOp { left, op, right } => {
                match op {
                    // and: 短絡評価（左辺が偽なら左辺の値を返す）
                    BinOpKind::And => {
                        self.compile_expr(left, line)?;
                        // 左辺が偽ならスタックに残したままジャンプ
                        let jump = self.chunk.emit_jump(OpCode::JumpIfFalseKeep(0), line);
                        // 左辺が真 → 左辺を捨てて右辺を評価
                        self.chunk.emit(OpCode::Pop, line);
                        self.compile_expr(right, line)?;
                        self.chunk.patch_jump(jump);
                    }
                    // or: 短絡評価（左辺が真なら左辺の値を返す）
                    BinOpKind::Or => {
                        self.compile_expr(left, line)?;
                        // 左辺が真ならスタックに残したままジャンプ
                        let jump = self.chunk.emit_jump(OpCode::JumpIfTrueKeep(0), line);
                        // 左辺が偽 → 左辺を捨てて右辺を評価
                        self.chunk.emit(OpCode::Pop, line);
                        self.compile_expr(right, line)?;
                        self.chunk.patch_jump(jump);
                    }
                    _ => {
                        self.compile_expr(left, line)?;
                        self.compile_expr(right, line)?;
                        let opcode = match op {
                            BinOpKind::Add => OpCode::Add,
                            BinOpKind::Sub => OpCode::Sub,
                            BinOpKind::Mul => OpCode::Mul,
                            BinOpKind::Div => OpCode::Div,
                            BinOpKind::Mod => OpCode::Mod,
                            BinOpKind::Eq => OpCode::Eq,
                            BinOpKind::NotEq => OpCode::NotEq,
                            BinOpKind::Lt => OpCode::Lt,
                            BinOpKind::Gt => OpCode::Gt,
                            BinOpKind::LtEq => OpCode::LtEq,
                            BinOpKind::GtEq => OpCode::GtEq,
                            BinOpKind::And | BinOpKind::Or => unreachable!(),
                        };
                        self.chunk.emit(opcode, line);
                    }
                }
            }
            Expr::UnaryOp { op, expr } => {
                self.compile_expr(expr, line)?;
                match op {
                    UnaryOpKind::Neg => self.chunk.emit(OpCode::Negate, line),
                    UnaryOpKind::Not => self.chunk.emit(OpCode::Not, line),
                }
            }
            Expr::Call { callee, args } => {
                // 組み込み関数かチェック
                if let Expr::Ident(name) = callee.as_ref() {
                    if name == "print" {
                        let arg_count = args.len();
                        for arg in args {
                            self.compile_expr(arg, line)?;
                        }
                        self.chunk.emit(OpCode::Print(arg_count), line);
                        self.chunk.emit_constant(Value::Null, line);
                        return Ok(());
                    }
                    if is_builtin(name) {
                        // push/pop は第一引数のリストを破壊的に変更する
                        // → 実行後に元の変数スロットを更新する
                        if (name == "push" || name == "pop")
                            && !args.is_empty()
                            && let Expr::Ident(var_name) = &args[0]
                        {
                            let slot = self.resolve_local(var_name, line)?;
                            let arg_count = args.len();
                            for arg in args {
                                self.compile_expr(arg, line)?;
                            }
                            let name_idx = self.chunk.add_constant(Value::Str(name.clone()));
                            self.chunk
                                .emit(OpCode::CallBuiltin(name_idx, arg_count), line);
                            if name == "push" {
                                self.chunk.emit(OpCode::SetLocal(slot), line);
                                self.chunk.emit(OpCode::Pop, line);
                                self.chunk.emit_constant(Value::Null, line);
                            }
                            if name == "pop" {
                                self.chunk.emit(OpCode::GetLocal(slot), line);
                                let pop_update_idx = self
                                    .chunk
                                    .add_constant(Value::Str("__pop_update".to_string()));
                                self.chunk
                                    .emit(OpCode::CallBuiltin(pop_update_idx, 1), line);
                                self.chunk.emit(OpCode::SetLocal(slot), line);
                                self.chunk.emit(OpCode::Pop, line);
                            }
                            return Ok(());
                        }
                        let arg_count = args.len();
                        for arg in args {
                            self.compile_expr(arg, line)?;
                        }
                        let name_idx = self.chunk.add_constant(Value::Str(name.clone()));
                        self.chunk
                            .emit(OpCode::CallBuiltin(name_idx, arg_count), line);
                        return Ok(());
                    }
                }
                // ユーザー定義関数呼び出し: callee を評価 → 引数を評価 → Call
                if let Expr::Ident(name) = callee.as_ref() {
                    // 関数呼び出しのコンテキストでは「未定義の関数」エラーを出す
                    if self.resolve_local(name, line).is_err()
                        && self.resolve_upvalue(name).is_none()
                    {
                        return Err(TsumugiError::runtime_with_kind(
                            line,
                            crate::error::ErrorKind::Name,
                            format!("未定義の関数: {}", name),
                        ));
                    }
                }
                self.compile_expr(callee, line)?;
                let arg_count = args.len();
                for arg in args {
                    self.compile_expr(arg, line)?;
                }
                self.chunk.emit(OpCode::Call(arg_count), line);
            }
            Expr::List(elements) => {
                self.chunk.emit_constant(Value::List(Vec::new()), line);
                for elem in elements {
                    self.compile_expr(elem, line)?;
                    self.chunk.emit(OpCode::ListPush, line);
                }
            }
            Expr::Dict(pairs) => {
                self.chunk
                    .emit_constant(Value::Dict(std::collections::BTreeMap::new()), line);
                for (key, val) in pairs {
                    self.compile_expr(key, line)?;
                    self.compile_expr(val, line)?;
                    self.chunk.emit(OpCode::DictInsert, line);
                }
            }
            Expr::Index { object, index } => {
                self.compile_expr(object, line)?;
                self.compile_expr(index, line)?;
                self.chunk.emit(OpCode::Index, line);
            }
            Expr::Lambda { params, body } => {
                self.compile_lambda(params, body, line)?;
            }
            Expr::FStr(parts) => {
                let part_count = parts.len();
                for part in parts {
                    match part {
                        FStrExprPart::Literal(s) => {
                            self.chunk.emit_constant(Value::Str(s.clone()), line);
                        }
                        FStrExprPart::Expr(expr) => {
                            self.compile_expr(expr, line)?;
                        }
                    }
                }
                self.chunk.emit(OpCode::FStrConcat(part_count), line);
            }
        }
        Ok(())
    }

    // --- スコープ管理 ---

    fn begin_scope(&mut self) {
        self.scope_depth += 1;
    }

    fn end_scope(&mut self, line: usize) {
        self.scope_depth -= 1;
        // 現在のスコープより深いローカル変数を削除
        let mut pop_count = 0;
        while let Some(local) = self.locals.last() {
            if local.depth <= self.scope_depth {
                break;
            }
            self.locals.pop();
            pop_count += 1;
        }
        if pop_count > 0 {
            self.chunk.emit(OpCode::PopN(pop_count), line);
        }
    }

    // --- ローカル変数管理 ---

    /// 関数定義のコンパイル
    fn compile_fn_def(
        &mut self,
        name: &str,
        params: &[String],
        body: &[Stmt],
        line: usize,
    ) -> Result<(), TsumugiError> {
        // 関数本体を別の Compiler でコンパイル（親のlocalsと祖先スコープを渡す）
        // enclosing_upvalues には親が到達可能な祖先変数を渡す
        // （親の enclosing_locals + 親の enclosing_upvalues を合成）
        let ancestor_vars = self.build_ancestor_vars();
        let mut fn_compiler = Compiler::new_enclosed(self.locals.clone(), ancestor_vars);
        fn_compiler.chunk.name = name.to_string();

        // 再帰呼び出し用に関数自身の名前を slot 0 として登録
        fn_compiler.add_local(name.to_string());

        // パラメータをローカル変数として登録（slot 1, 2, 3, ...）
        for param in params {
            fn_compiler.add_local(param.clone());
        }

        // 本体をコンパイル
        for stmt in body {
            fn_compiler.compile_stmt(stmt)?;
        }

        // 明示的な return がない場合のフォールバック: null を返す
        fn_compiler.chunk.emit_constant(Value::Null, line);
        fn_compiler.chunk.emit(OpCode::ReturnValue, line);

        let fn_chunk = fn_compiler.chunk;
        let mut upvalues = fn_compiler.upvalues;

        // 子の is_local=false upvalue を解決（親が中間キャプチャを行う）
        self.resolve_child_upvalues(&mut upvalues);

        // VmFn 値を定数テーブルに追加してロード
        let fn_value = Value::VmFn {
            name: name.to_string(),
            arity: params.len(),
            params: params.to_vec(),
            chunk: Rc::new(fn_chunk),
            upvalues: Vec::new(),
        };
        self.chunk.emit_constant(fn_value, line);

        if upvalues.is_empty() {
            // upvalue なし: そのまま関数値として使う
        } else {
            // upvalue あり: 外側の変数値をスタックに積んで MakeClosure
            for uv in &upvalues {
                if uv.is_local {
                    self.chunk.emit(OpCode::GetLocal(uv.slot), line);
                } else {
                    self.chunk.emit(OpCode::GetUpvalue(uv.slot), line);
                }
            }
            self.chunk.emit(OpCode::MakeClosure(upvalues.len()), line);
        }

        // 関数名をローカル変数として登録
        self.add_local(name.to_string());

        Ok(())
    }

    /// ラムダ（無名関数）のコンパイル
    fn compile_lambda(
        &mut self,
        params: &[String],
        body: &[Stmt],
        line: usize,
    ) -> Result<(), TsumugiError> {
        let ancestor_vars = self.build_ancestor_vars();
        let mut fn_compiler = Compiler::new_enclosed(self.locals.clone(), ancestor_vars);
        fn_compiler.chunk.name = "<lambda>".to_string();

        // ラムダ自身は slot 0（無名なので "__lambda__"）
        fn_compiler.add_local("__lambda__".to_string());

        for param in params {
            fn_compiler.add_local(param.clone());
        }

        for stmt in body {
            fn_compiler.compile_stmt(stmt)?;
        }

        fn_compiler.chunk.emit_constant(Value::Null, line);
        fn_compiler.chunk.emit(OpCode::ReturnValue, line);

        let fn_chunk = fn_compiler.chunk;
        let mut upvalues = fn_compiler.upvalues;

        // 子の is_local=false upvalue を解決（親が中間キャプチャを行う）
        self.resolve_child_upvalues(&mut upvalues);

        let fn_value = Value::VmFn {
            name: "<lambda>".to_string(),
            arity: params.len(),
            params: params.to_vec(),
            chunk: Rc::new(fn_chunk),
            upvalues: Vec::new(),
        };
        self.chunk.emit_constant(fn_value, line);

        if !upvalues.is_empty() {
            for uv in &upvalues {
                if uv.is_local {
                    self.chunk.emit(OpCode::GetLocal(uv.slot), line);
                } else {
                    self.chunk.emit(OpCode::GetUpvalue(uv.slot), line);
                }
            }
            self.chunk.emit(OpCode::MakeClosure(upvalues.len()), line);
        }

        Ok(())
    }

    fn add_local(&mut self, name: String) {
        self.locals.push(Local {
            name,
            depth: self.scope_depth,
        });
    }

    /// 同じスコープ深さに同名の変数があればそのスロットを返す
    fn find_local_in_current_scope(&self, name: &str) -> Option<usize> {
        for (i, local) in self.locals.iter().enumerate().rev() {
            if local.depth < self.scope_depth {
                break;
            }
            if local.name == name {
                return Some(i);
            }
        }
        None
    }

    fn resolve_local(&self, name: &str, line: usize) -> Result<usize, TsumugiError> {
        for (i, local) in self.locals.iter().enumerate().rev() {
            if local.name == name {
                return Ok(i);
            }
        }
        Err(TsumugiError::runtime_with_kind(
            line,
            crate::error::ErrorKind::Name,
            format!("未定義の変数: {}", name),
        ))
    }

    /// 外側のスコープから変数を探し、upvalue として登録する
    /// 見つかった場合は upvalue のインデックスを返す
    /// 多段キャプチャ: 親のローカル → 親の祖先変数の順に検索
    fn resolve_upvalue(&mut self, name: &str) -> Option<usize> {
        // まず親のローカル変数を検索（is_local=true）
        if let Some(enclosing) = &self.enclosing_locals {
            for (i, local) in enclosing.iter().enumerate().rev() {
                if local.name == name {
                    return Some(self.add_upvalue(i, true, name));
                }
            }
        }
        // 親の祖先変数を検索（is_local=false: 親経由で祖先の変数をキャプチャ）
        // ここでの slot は enclosing_upvalues 内のインデックス（後で親側で解決する）
        if let Some(enclosing_uvs) = &self.enclosing_upvalues {
            for (i, uv) in enclosing_uvs.iter().enumerate() {
                if uv.name == name {
                    return Some(self.add_upvalue(i, false, name));
                }
            }
        }
        None
    }

    /// upvalue を登録する（既に同じものがあれば既存のインデックスを返す）
    fn add_upvalue(&mut self, slot: usize, is_local: bool, name: &str) -> usize {
        // 既に同じ upvalue が登録されているか確認
        for (i, uv) in self.upvalues.iter().enumerate() {
            if uv.slot == slot && uv.is_local == is_local {
                return i;
            }
        }
        let index = self.upvalues.len();
        self.upvalues.push(Upvalue {
            slot,
            is_local,
            name: name.to_string(),
        });
        index
    }

    /// 子コンパイラに渡す祖先変数リストを構築する
    /// 自身の enclosing_locals + enclosing_upvalues を合成して、
    /// 子がさらに上の変数を参照できるようにする
    fn build_ancestor_vars(&self) -> Vec<Upvalue> {
        let mut vars = Vec::new();
        // 自身の enclosing_locals（自分の親のローカル変数）
        if let Some(enclosing) = &self.enclosing_locals {
            for local in enclosing {
                vars.push(Upvalue {
                    slot: 0, // 実際のスロットは resolve 時に決まる
                    is_local: true,
                    name: local.name.clone(),
                });
            }
        }
        // 自身の enclosing_upvalues（自分の祖先の変数）
        if let Some(uvs) = &self.enclosing_upvalues {
            for uv in uvs {
                // 既に同名がなければ追加
                if !vars.iter().any(|v| v.name == uv.name) {
                    vars.push(uv.clone());
                }
            }
        }
        vars
    }

    /// 子コンパイラの upvalue を解決する: is_local=false の upvalue について、
    /// 親（self）が実際にキャプチャを行い、正しい slot を設定する
    fn resolve_child_upvalues(&mut self, child_upvalues: &mut [Upvalue]) {
        for child_uv in child_upvalues.iter_mut() {
            if !child_uv.is_local {
                // 子が祖先変数を要求 → 親が自分の upvalue として確保する
                let parent_uv_index = self.ensure_upvalue_for_ancestor(&child_uv.name);
                child_uv.slot = parent_uv_index;
            }
        }
    }

    /// 指定名の変数を自身の upvalue として確保する（なければ追加）
    /// 戻り値は self.upvalues 内のインデックス
    fn ensure_upvalue_for_ancestor(&mut self, name: &str) -> usize {
        // 既に自分の upvalues にあるか
        for (i, uv) in self.upvalues.iter().enumerate() {
            if uv.name == name {
                return i;
            }
        }
        // 自分の enclosing_locals から探す
        if let Some(enclosing) = &self.enclosing_locals {
            for (i, local) in enclosing.iter().enumerate().rev() {
                if local.name == name {
                    return self.add_upvalue(i, true, name);
                }
            }
        }
        // 自分の enclosing_upvalues から探す
        if let Some(enclosing_uvs) = &self.enclosing_upvalues {
            for (i, uv) in enclosing_uvs.iter().enumerate() {
                if uv.name == name {
                    return self.add_upvalue(i, false, name);
                }
            }
        }
        // フォールバック（到達しないはず）
        self.add_upvalue(0, false, name)
    }
}

/// 組み込み関数かどうかを判定する
fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "len"
            | "push"
            | "pop"
            | "keys"
            | "values"
            | "has_key"
            | "type"
            | "slice"
            | "contains"
            | "split"
            | "join"
            | "to_int"
            | "to_str"
            | "to_float"
            | "range"
            | "map"
            | "filter"
            | "each"
            | "sort"
            | "reverse"
            | "trim"
            | "upper"
            | "lower"
            | "starts_with"
            | "ends_with"
            | "replace"
            | "abs"
            | "min"
            | "max"
            | "floor"
            | "ceil"
            | "round"
            | "input"
            | "exit"
            | "read_file"
            | "read_lines"
            | "write_file"
            | "append_file"
            | "env"
            | "args"
            | "now"
            | "format_time"
            | "path_exists"
            | "path_join"
            | "mkdir"
            | "remove"
            | "remove_dir"
            | "rename"
            | "list_dir"
            | "file_size"
            | "is_file"
            | "is_dir"
    )
}
