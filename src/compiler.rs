//! コンパイラ: AST → バイトコード（Chunk）に変換

use std::rc::Rc;

use crate::ast::{
    BinOpKind, Expr, FStrExprPart, Program, Stmt, UnaryOpKind, validate_program_depth,
};
use crate::chunk::Chunk;
use crate::error::TsumugiError;
use crate::opcode::{MutationTarget, OpCode};
use crate::value::{FunctionId, Value};

/// ローカル変数の情報
#[derive(Debug, Clone)]
struct Local {
    name: String,
    /// スコープの深さ（0 = トップレベル）
    depth: usize,
}

/// ループのコンパイル状態（break/continue のパッチに使う）
#[derive(Debug, Clone)]
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
#[derive(Clone)]
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
        }
    }

    /// プログラム全体をコンパイルして Chunk を返す
    pub fn compile(mut self, program: &Program) -> Result<Chunk, TsumugiError> {
        validate_program_depth(program)?;
        for stmt in program {
            self.compile_stmt(stmt)?;
        }
        self.chunk.emit(OpCode::Return, 0);
        Ok(self.chunk)
    }

    /// REPL 用インクリメンタルコンパイル: 既存のローカル変数を保持したまま
    /// 新しいステートメントだけをコンパイルして Chunk を返す。
    /// self は消費されず次の入力に再利用される。
    pub fn compile_repl_line(&mut self, program: &Program) -> Result<Chunk, TsumugiError> {
        validate_program_depth(program)?;
        // コンパイル途中で失敗しても、永続する locals / scope / loop / import 状態を
        // 次の入力へ持ち越さない。VM 側は失敗チャンクを実行しないため、Compiler も
        // 入力開始時点へ戻す必要がある。
        let checkpoint = self.clone();
        let result = (|| -> Result<Chunk, TsumugiError> {
            // 新しいチャンクを作成（前のチャンクは捨てる）
            let prev_chunk = std::mem::replace(&mut self.chunk, Chunk::new());
            // チャンク名を引き継ぐ
            self.chunk.name = prev_chunk.name;

            for stmt in program {
                self.compile_stmt(stmt)?;
            }
            self.chunk.emit(OpCode::Return, 0);

            // 今回のチャンクを取り出して返す（次回用に空チャンクをセット）
            Ok(std::mem::replace(&mut self.chunk, Chunk::new()))
        })();

        if result.is_err() {
            *self = checkpoint;
        }
        result
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
                    self.declare_local(name.clone(), *line);
                }
            }
            Stmt::Assign { name, value, line } => {
                self.compile_expr(value, *line)?;
                if let Ok(slot) = self.resolve_local(name, *line) {
                    self.chunk.emit(OpCode::SetLocal(slot), *line);
                } else if let Some(upvalue_idx) = self.resolve_upvalue(name) {
                    self.chunk.emit(OpCode::SetUpvalue(upvalue_idx), *line);
                } else {
                    // tree evaluatorと同様、未解決名の存在確認は代入の実行時まで遅延する。
                    self.chunk.emit(OpCode::SetGlobal(name.clone()), *line);
                }
                self.chunk.emit(OpCode::Pop, *line);
            }
            Stmt::IndexAssign {
                name,
                index,
                value,
                line,
            } => {
                // 規範順序: target binding解決 → index → value → in-place更新。
                // targetはlocal/upvalue/globalのいずれでもよく、引数評価前に
                // 解決してupvalueキャプチャを確定させる（push/popと同じ扱い）。
                let target = self.resolve_mutation_target(name, *line);
                if let MutationTarget::Global(global) = &target {
                    // 未定義bindingはindex/valueの副作用より先に報告する
                    // （treeのget_cell検査と同じ順序）。
                    self.chunk
                        .emit(OpCode::RequireGlobal(global.clone()), *line);
                }
                self.compile_expr(index, *line)?;
                self.compile_expr(value, *line)?;
                self.chunk.emit(OpCode::SetIndex(target), *line);
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
            // import はリンク時に解決済みなので、ここへは到達しない
            Stmt::Import { line, .. } => {
                return Err(TsumugiError::internal(
                    *line,
                    "import がリンクされていません",
                ));
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

        // ループ変数をnullで初期化
        self.chunk.emit_constant(Value::Null, line);
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
        // コレクションはslotから直接参照する。スタックへ複製すると
        // 反復ごとに全要素をコピーし、ループ全体がO(n^2)になる。
        self.chunk.emit(OpCode::GetLocal(index_slot), line);
        self.chunk.emit(OpCode::LenLocal(collection_slot), line);
        self.chunk.emit(OpCode::Lt, line);
        let exit_jump = self.chunk.emit_jump(OpCode::JumpIfFalse(0), line);

        // ループ変数を反復ごとに新しいcellへ再bindする。
        // 前回値をpopするとlocals_cellsの対応も解除され、escaping closureが
        // 保持する旧cellと次の反復で作られるcellが分離される。
        self.chunk.emit(OpCode::Pop, line);
        self.chunk.emit(OpCode::GetLocal(index_slot), line);
        self.chunk.emit(OpCode::IndexLocal(collection_slot), line);

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
        let loop_state = self
            .loops
            .last()
            .ok_or_else(|| TsumugiError::break_outside_loop(line))?;

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
        let loop_state = self
            .loops
            .last()
            .ok_or_else(|| TsumugiError::continue_outside_loop(line))?;
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
                self.compile_name_read(name, line, false);
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
                // printは予約tokenのため常にbuiltinへ直接dispatchする。
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
                        // lexical bindingがあればuser callとしてcompileする。
                        // 未解決時はruntime globalの有無でuser/builtinを選び、
                        // 後続宣言・import・REPLでも実行時のbindingを優先する。
                        let has_static_binding = self.resolve_local(name, line).is_ok()
                            || self.resolve_upvalue(name).is_some();
                        if !has_static_binding {
                            let user_jump = self
                                .chunk
                                .emit_jump(OpCode::JumpIfGlobalDefined(name.clone(), 0), line);
                            self.compile_builtin_call(name, args, line)?;
                            let end_jump = self.chunk.emit_jump(OpCode::Jump(0), line);

                            self.chunk.patch_jump(user_jump);
                            self.compile_user_call(callee, args, line)?;
                            self.chunk.patch_jump(end_jump);
                            return Ok(());
                        }
                    }
                }

                self.compile_user_call(callee, args, line)?;
            }
            Expr::List(elements) => {
                self.chunk
                    .emit_constant(Value::List(Rc::new(Vec::new())), line);
                for elem in elements {
                    self.compile_expr(elem, line)?;
                    self.chunk.emit(OpCode::ListPush, line);
                }
            }
            Expr::Dict(pairs) => {
                self.chunk.emit_constant(
                    Value::Dict(Rc::new(std::collections::BTreeMap::new())),
                    line,
                );
                for (key, val) in pairs {
                    self.compile_expr(key, line)?;
                    self.compile_expr(val, line)?;
                    self.chunk.emit(OpCode::DictInsert, line);
                }
            }
            Expr::Index { object, index } => {
                // 副作用のないindex式なら、コレクションをスタックへ複製せず
                // ローカルslotから参照で読む（AUD-041）。
                if let Expr::Ident(name) = object.as_ref()
                    && crate::ast::is_side_effect_free(index)
                    && let Ok(slot) = self.resolve_local(name, line)
                {
                    self.compile_expr(index, line)?;
                    self.chunk.emit(OpCode::IndexLocal(slot), line);
                } else {
                    self.compile_expr(object, line)?;
                    self.compile_expr(index, line)?;
                    self.chunk.emit(OpCode::Index, line);
                }
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

    /// builtin callを現在のengine固有契約でcompileする。
    fn compile_builtin_call(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<(), TsumugiError> {
        if crate::builtin_core::is_context_builtin(name) {
            self.chunk.emit(
                OpCode::ValidateBuiltinCall(
                    name.to_string(),
                    args.len(),
                    matches!(args.first(), Some(Expr::Ident(_))),
                ),
                line,
            );
        }

        // len(識別子) はコレクションを複製せず長さだけ読む（AUD-041）
        if name == "len"
            && args.len() == 1
            && let Expr::Ident(var_name) = &args[0]
            && let Ok(slot) = self.resolve_local(var_name, line)
        {
            self.chunk.emit(OpCode::LenLocal(slot), line);
            return Ok(());
        }

        // builtin ID は registry から引く。compile_builtin_call は is_builtin(name)
        // を満たした名前だけに到達するため、ここでの欠落は内部不整合である（AUD-049）。
        let builtin_id = self.resolve_builtin_id(name, line)?;

        // push/pop は第一引数のリストを破壊的に変更する
        // → 実行後に元の変数スロットを更新する
        if (name == "push" || name == "pop")
            && !args.is_empty()
            && let Expr::Ident(var_name) = &args[0]
        {
            let target = self.resolve_mutation_target(var_name, line);
            let arg_count = args.len();
            for arg in args {
                self.compile_expr(arg, line)?;
            }
            self.chunk
                .emit(OpCode::CallBuiltin(builtin_id, arg_count), line);
            if name == "push" {
                self.emit_set_mutation_target(&target, line);
                self.chunk.emit(OpCode::Pop, line);
                self.chunk.emit_constant(Value::Null, line);
            }
            if name == "pop" {
                // pop の戻り値（取り出した要素）はスタックに残したまま、変数側の
                // List を PopUpdate で更新する。内部命令なので source から呼べない。
                self.emit_get_mutation_target(&target, line);
                self.chunk.emit(OpCode::PopUpdate, line);
                self.emit_set_mutation_target(&target, line);
                self.chunk.emit(OpCode::Pop, line);
            }
            return Ok(());
        }

        let arg_count = args.len();
        for arg in args {
            self.compile_expr(arg, line)?;
        }
        self.chunk
            .emit(OpCode::CallBuiltin(builtin_id, arg_count), line);
        Ok(())
    }

    /// builtin 名から [`crate::builtin_registry::BuiltinId`] を引く。
    /// registry に無ければ内部不整合として構造化エラーにする（source から到達不能）。
    fn resolve_builtin_id(
        &self,
        name: &str,
        line: usize,
    ) -> Result<crate::builtin_registry::BuiltinId, TsumugiError> {
        crate::builtin_registry::id_of(name).ok_or_else(|| {
            TsumugiError::internal(
                line,
                format!("未登録の builtin をコンパイルしようとしました: {}", name),
            )
        })
    }

    fn resolve_mutation_target(&mut self, name: &str, line: usize) -> MutationTarget {
        if let Ok(slot) = self.resolve_local(name, line) {
            MutationTarget::Local(slot)
        } else if let Some(index) = self.resolve_upvalue(name) {
            MutationTarget::Upvalue(index)
        } else {
            MutationTarget::Global(name.to_string())
        }
    }

    fn emit_get_mutation_target(&mut self, target: &MutationTarget, line: usize) {
        match target {
            MutationTarget::Local(slot) => self.chunk.emit(OpCode::GetLocal(*slot), line),
            MutationTarget::Upvalue(index) => self.chunk.emit(OpCode::GetUpvalue(*index), line),
            MutationTarget::Global(name) => {
                self.chunk.emit(OpCode::GetGlobal(name.clone()), line);
            }
        }
    }

    fn emit_set_mutation_target(&mut self, target: &MutationTarget, line: usize) {
        match target {
            MutationTarget::Local(slot) => self.chunk.emit(OpCode::SetLocal(*slot), line),
            MutationTarget::Upvalue(index) => self.chunk.emit(OpCode::SetUpvalue(*index), line),
            MutationTarget::Global(name) => {
                self.chunk.emit(OpCode::SetGlobal(name.clone()), line);
            }
        }
    }

    /// user function callを規範順序でcompileする。
    fn compile_user_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        line: usize,
    ) -> Result<(), TsumugiError> {
        // step/depth検査 → callee評価 → callable/arity検査 → 引数評価 → Call
        let arg_count = args.len();
        self.chunk.emit(OpCode::PrepareCall, line);
        if let Expr::Ident(name) = callee {
            // 未解決calleeもcompile errorにせずruntime global lookupへ落とす。
            // 存在しない場合の診断はtree evaluatorと同じ canonical な
            // 「未定義の変数または関数」にする（AUD-019）。
            self.compile_name_read(name, line, true);
        } else {
            self.compile_expr(callee, line)?;
        }
        self.chunk.emit(OpCode::ValidateCall(arg_count), line);
        for arg in args {
            self.compile_expr(arg, line)?;
        }
        self.chunk.emit(OpCode::Call(arg_count), line);
        Ok(())
    }

    /// 識別子読み取りをstatic slot優先でloweringし、未解決名だけをruntime globalへ落とす。
    fn compile_name_read(&mut self, name: &str, line: usize, for_call: bool) {
        if let Ok(slot) = self.resolve_local(name, line) {
            self.chunk.emit(OpCode::GetLocal(slot), line);
        } else if let Some(upvalue_idx) = self.resolve_upvalue(name) {
            self.chunk.emit(OpCode::GetUpvalue(upvalue_idx), line);
        } else if for_call {
            self.chunk
                .emit(OpCode::GetGlobalForCall(name.to_string()), line);
        } else {
            self.chunk.emit(OpCode::GetGlobal(name.to_string()), line);
        }
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

        // VmFn プロトタイプを定数テーブルに追加してロードする。
        // 定数の id は placeholder（FunctionId(0)）で、実行時に MakeClosure が
        // 新しい FunctionId を発番して差し替える（AUD-048）。
        let fn_value = Value::VmFn {
            id: FunctionId(0),
            name: name.to_string(),
            arity: params.len(),
            params: params.to_vec(),
            chunk: Rc::new(fn_chunk),
            upvalues: Vec::new(),
        };
        self.chunk.emit_constant(fn_value, line);

        // upvalue の有無に関わらず必ず MakeClosure を通す（AUD-048）。
        // capture 0 件でも実行時に一意な FunctionId を持つ新インスタンスを生成し、
        // 同じ fn 式を複数回評価した値が別物になるようにする。
        for uv in &upvalues {
            if uv.is_local {
                self.chunk.emit(OpCode::GetLocal(uv.slot), line);
            } else {
                self.chunk.emit(OpCode::GetUpvalue(uv.slot), line);
            }
        }
        self.chunk.emit(OpCode::MakeClosure(upvalues.len()), line);

        // 関数名を現在のscopeへ登録する。script top-levelならruntime globalにも公開する。
        self.declare_local(name.to_string(), line);

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

        // ラムダ自身は物理slot 0を使うが、sourceから参照できない内部名にする。
        fn_compiler.add_local("<lambda>".to_string());

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

        // VmFn プロトタイプ（id は placeholder。実行時に MakeClosure が発番する。AUD-048）
        let fn_value = Value::VmFn {
            id: FunctionId(0),
            name: "<lambda>".to_string(),
            arity: params.len(),
            params: params.to_vec(),
            chunk: Rc::new(fn_chunk),
            upvalues: Vec::new(),
        };
        self.chunk.emit_constant(fn_value, line);

        // capture 0 件でも必ず MakeClosure を通し、評価ごとに一意な FunctionId を得る（AUD-048）
        for uv in &upvalues {
            if uv.is_local {
                self.chunk.emit(OpCode::GetLocal(uv.slot), line);
            } else {
                self.chunk.emit(OpCode::GetUpvalue(uv.slot), line);
            }
        }
        self.chunk.emit(OpCode::MakeClosure(upvalues.len()), line);

        Ok(())
    }

    /// 言語上のbindingを現在scopeへ追加する。
    /// script top-levelだけは、宣言が実際に実行された時点で同じslotをglobal公開する。
    fn declare_local(&mut self, name: String, line: usize) {
        let slot = self.locals.len();
        let is_script_top_level = self.scope_depth == 0 && self.enclosing_locals.is_none();
        self.add_local(name.clone());
        if is_script_top_level {
            self.chunk.emit(OpCode::RegisterGlobal(name, slot), line);
        }
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
        Err(TsumugiError::undefined_name(line, name))
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

/// 組み込み関数かどうかを判定する。
///
/// 名前一覧は単一の BuiltinSpec registry から導出する（AUD-049）。ここへ手書きの
/// 名前列挙を復活させない。`print` は呼び出し元が予約 token として先に直接
/// lowering するため、この判定へは到達しない。
fn is_builtin(name: &str) -> bool {
    crate::builtin_registry::is_public_builtin(name)
}
