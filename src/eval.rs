#[path = "builtin.rs"]
mod builtin;

use crate::ast::*;
use crate::env::Env;
use crate::error::{TraceFrame, TsumugiError};
use crate::limits::MAX_IMPORT_DEPTH;
use crate::value::{FnDef, Value};

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
/// 評価器の戻り値（通常の値 or return / break / continue による制御フロー）
enum EvalResult {
    Val,
    Return(Value),
    Break,
    Continue,
}

/// デフォルトのステップ上限（100万）
const DEFAULT_MAX_STEPS: u64 = 1_000_000;

/// コールフレーム深度の上限（スタックオーバーフロー防止）
const MAX_CALL_DEPTH: usize = 128;

/// 環境変数からステップ上限を読み取る
fn resolve_max_steps() -> u64 {
    std::env::var("TSUMUGI_MAX_STEPS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_STEPS)
}

/// AST を評価して実行する
pub struct Evaluator {
    pub(crate) env: Env,
    /// 関数呼び出しのスタック（スタックトレース用）
    call_stack: Vec<TraceFrame>,
    /// 実行ステップカウンタ（ループ反復 + 関数呼び出し）
    steps: u64,
    /// ステップ上限
    max_steps: u64,
    /// 現在のスクリプトの基準ディレクトリ（import のパス解決に使用）
    base_dir: PathBuf,
    /// import 済みファイルの正規パス集合（循環 import 防止）
    imported: HashSet<PathBuf>,
    /// 現在処理中のactive import chain深度（root scriptは0）。
    import_depth: usize,
}

impl Evaluator {
    pub fn new() -> Self {
        Self {
            env: Env::new(),
            call_stack: Vec::new(),
            steps: 0,
            max_steps: resolve_max_steps(),
            base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            imported: HashSet::new(),
            import_depth: 0,
        }
    }

    /// 基準ディレクトリを設定する（ファイル実行時に呼ばれる）
    pub fn set_base_dir(&mut self, path: &Path) {
        if let Some(parent) = path.parent() {
            self.base_dir = parent.to_path_buf();
        }
        // 実行されたファイル自体も imported に追加（自分自身の import を防ぐ）
        if let Ok(canonical) = std::fs::canonicalize(path) {
            self.imported.insert(canonical);
        }
    }

    /// ステップカウンタを進め、上限チェックする
    fn count_step(&mut self, line: usize) -> Result<(), TsumugiError> {
        self.steps += 1;
        if self.steps > self.max_steps {
            let mut err = TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::StepLimit,
                format!("ステップ上限に達しました (上限: {})", self.max_steps),
            );
            if !self.call_stack.is_empty() {
                let mut trace = self.call_stack.clone();
                trace.reverse();
                err = err.with_trace(trace);
            }
            return Err(err);
        }
        Ok(())
    }

    /// REPLの新しい入力を開始する前にステップ予算をリセットする。
    /// importは同じ`run`を再帰利用するため、`run`自身ではリセットしない。
    pub fn reset_step_budget(&mut self) {
        self.steps = 0;
    }

    /// プログラム全体を実行
    pub fn run(&mut self, program: &Program) -> Result<(), TsumugiError> {
        validate_program_depth(program)?;
        for stmt in program {
            match self.exec_stmt(stmt)? {
                EvalResult::Return(_) => break,
                EvalResult::Break => {
                    return Err(TsumugiError::runtime(
                        stmt.line(),
                        "break はループの中でのみ使用できます",
                    ));
                }
                EvalResult::Continue => {
                    return Err(TsumugiError::runtime(
                        stmt.line(),
                        "continue はループの中でのみ使用できます",
                    ));
                }
                EvalResult::Val => {}
            }
        }
        Ok(())
    }

    /// import 文を実行する
    fn exec_import(&mut self, path: &str, line: usize) -> Result<(), TsumugiError> {
        use crate::lexer::Lexer;
        use crate::parser::Parser;

        // パス解決: base_dir からの相対パス
        let resolved = self.base_dir.join(path);
        let canonical = std::fs::canonicalize(&resolved).map_err(|e| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Import,
                format!("import 失敗: ファイルが見つかりません: {} ({})", path, e),
            )
        })?;

        // サンドボックスチェック: import 先が許可範囲内か検証
        let _ = crate::sandbox::check_path(canonical.to_str().unwrap_or(""), line)?;

        // 循環 import は従来どおり成功扱いにし、active chainの深度を消費しない。
        if self.imported.contains(&canonical) {
            return Ok(());
        }
        if self.import_depth >= MAX_IMPORT_DEPTH {
            return Err(TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Import,
                format!(
                    "import 失敗: ネストが深すぎます (上限: {})",
                    MAX_IMPORT_DEPTH
                ),
            ));
        }
        self.imported.insert(canonical.clone());

        let previous_depth = self.import_depth;
        let previous_base_dir = self.base_dir.clone();
        self.import_depth += 1;

        let result = (|| -> Result<(), TsumugiError> {
            let source = std::fs::read_to_string(&canonical).map_err(|error| {
                TsumugiError::runtime_with_kind(
                    line,
                    crate::error::ErrorKind::Import,
                    format!(
                        "import 失敗: ファイルを読み込めません: {} ({})",
                        path, error
                    ),
                )
            })?;

            // base_dir を一時的に import 先のディレクトリに切り替え（ネスト import 対応）
            if let Some(parent) = canonical.parent() {
                self.base_dir = parent.to_path_buf();
            }

            // パース & 実行
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

            self.run(&program)
        })();

        // 成否にかかわらずactive chainの状態を呼び出し元へ戻す。
        self.base_dir = previous_base_dir;
        self.import_depth = previous_depth;
        if result.is_err() {
            // runtime errorで完了しなかったmoduleもloaded扱いにしない。
            // 既に発生した代入や外部I/OのrollbackはAUD-024で別途仕様化する。
            self.imported.remove(&canonical);
        }

        result
    }

    /// 文を実行
    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<EvalResult, TsumugiError> {
        match stmt {
            Stmt::Let { name, value, line } => {
                let val = self.eval_expr(value, *line)?;
                self.env.set(name, val);
                Ok(EvalResult::Val)
            }

            Stmt::Assign { name, value, line } => {
                let val = self.eval_expr(value, *line)?;
                if self.env.update(name, val).is_err() {
                    return Err(TsumugiError::runtime(
                        *line,
                        format!("未定義の変数に代入: {}", name),
                    ));
                }
                Ok(EvalResult::Val)
            }

            Stmt::IndexAssign {
                name,
                index,
                value,
                line,
            } => {
                // 規範順序: target binding解決 → index → value → in-place更新。
                // bindingを先に解決するため、未定義変数はindex/valueの副作用より前に報告する。
                let cell = self.env.get_cell(name).ok_or_else(|| {
                    TsumugiError::runtime(*line, format!("未定義の変数: {}", name))
                })?;
                let idx = self.eval_expr(index, *line)?;
                let val = self.eval_expr(value, *line)?;

                // 更新はcellへのin-place代入。index/valueの評価中に同じbindingが
                // 変更されていても、その最新状態に対して書き込む。
                crate::builtin_core::assign_index(&mut cell.borrow_mut(), &idx, val, *line)?;

                Ok(EvalResult::Val)
            }

            Stmt::Return { value, line } => {
                let val = self.eval_expr(value, *line)?;
                Ok(EvalResult::Return(val))
            }

            Stmt::If {
                condition,
                then_body,
                else_body,
                line,
            } => {
                let cond = self.eval_expr(condition, *line)?;
                let body = if cond.is_truthy() {
                    then_body
                } else {
                    else_body
                };
                self.exec_scoped_block(body)
            }

            Stmt::While {
                condition,
                body,
                line,
            } => {
                loop {
                    let cond = self.eval_expr(condition, *line)?;
                    if !cond.is_truthy() {
                        break;
                    }
                    self.env.push_scope();
                    let body_result = self.exec_block(body);
                    // body内のruntime errorも含め、全ての経路で反復scopeを解放してから
                    // 制御フローまたはエラーを伝播する。
                    self.env.pop_scope();
                    match body_result? {
                        EvalResult::Return(v) => return Ok(EvalResult::Return(v)),
                        EvalResult::Break => break,
                        EvalResult::Continue | EvalResult::Val => {}
                    }
                    self.count_step(*line)?;
                }
                Ok(EvalResult::Val)
            }

            Stmt::For {
                var,
                iter,
                body,
                line,
            } => {
                let collection = self.eval_expr(iter, *line)?;
                let items: Vec<Value> = match &collection {
                    Value::List(list) => {
                        crate::builtin_core::check_collection_size_public(list.len(), *line)?;
                        list.clone()
                    }
                    Value::Dict(map) => {
                        crate::builtin_core::check_collection_size_public(map.len(), *line)?;
                        map.keys().map(|k| Value::Str(k.clone())).collect()
                    }
                    Value::Str(s) => {
                        let size = s.chars().count();
                        crate::builtin_core::check_collection_size_public(size, *line)?;
                        s.chars().map(|c| Value::Str(c.to_string())).collect()
                    }
                    _ => {
                        return Err(TsumugiError::runtime(
                            *line,
                            format!("for で反復できません: {:?}", collection),
                        ));
                    }
                };

                for item in items {
                    self.env.push_scope();
                    self.env.set(var, item);
                    let body_result = self.exec_block(body);
                    // whileと同様に、エラー・return・break・continueの全経路で
                    // iteration scopeを先に解放する。
                    self.env.pop_scope();
                    match body_result? {
                        EvalResult::Return(v) => return Ok(EvalResult::Return(v)),
                        EvalResult::Break => break,
                        EvalResult::Continue | EvalResult::Val => {}
                    }
                    self.count_step(*line)?;
                }
                Ok(EvalResult::Val)
            }

            Stmt::FnDef {
                name, params, body, ..
            } => {
                // 関数を値として環境にセット
                // ネストされた関数定義の場合、定義時のスコープをキャプチャする
                // 捕捉するのは本体で言及される名前だけ（AUD-042）
                // 本体ASTの複製は定義時の一度だけで、以降の呼び出しはRcを共有する
                let captured = Rc::new(
                    self.env
                        .capture_referenced(&crate::ast::referenced_names(body)),
                );
                self.env.set(
                    name,
                    Value::Fn {
                        def: Rc::new(FnDef {
                            name: name.clone(),
                            params: params.clone(),
                            body: body.clone(),
                        }),
                        captured,
                    },
                );
                Ok(EvalResult::Val)
            }

            Stmt::Break { .. } => Ok(EvalResult::Break),

            Stmt::Continue { .. } => Ok(EvalResult::Continue),

            Stmt::Import { path, line } => {
                self.exec_import(path, *line)?;
                Ok(EvalResult::Val)
            }

            Stmt::TryCatch {
                try_body,
                var,
                catch_body,
                line: _,
            } => {
                // tryとcatchは別scope。outerへの代入は保持し、block localだけを破棄する。
                match self.exec_scoped_block(try_body) {
                    Ok(result) => Ok(result),
                    Err(e) => {
                        let error_value = Value::Error {
                            error_type: e.error_type().to_string(),
                            message: e.message().to_string(),
                            line: e.line(),
                        };

                        self.env.push_scope();
                        self.env.set(var, error_value);
                        let catch_result = self.exec_block(catch_body);
                        // error・return・break・continueの全経路でcatch scopeを先に解放する。
                        self.env.pop_scope();
                        catch_result
                    }
                }
            }

            Stmt::ExprStmt { expr, line } => {
                self.eval_expr(expr, *line)?;
                Ok(EvalResult::Val)
            }
        }
    }

    /// ブロック（文のリスト）を実行する。
    fn exec_block(&mut self, stmts: &[Stmt]) -> Result<EvalResult, TsumugiError> {
        for s in stmts {
            match self.exec_stmt(s)? {
                EvalResult::Val => {}
                other => return Ok(other),
            }
        }
        Ok(EvalResult::Val)
    }

    /// 独立scopeでブロックを実行し、全ての終了経路でscopeを解放する。
    fn exec_scoped_block(&mut self, stmts: &[Stmt]) -> Result<EvalResult, TsumugiError> {
        self.env.push_scope();
        let result = self.exec_block(stmts);
        self.env.pop_scope();
        result
    }

    /// 式を評価して値を返す（line は文の行番号をエラー表示に使う）
    pub(crate) fn eval_expr(&mut self, expr: &Expr, line: usize) -> Result<Value, TsumugiError> {
        match expr {
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),

            Expr::List(items) => {
                let mut values = Vec::new();
                for item in items {
                    let value = self.eval_expr(item, line)?;
                    crate::builtin_core::check_collection_size_public(
                        values.len().saturating_add(1),
                        line,
                    )?;
                    values.push(value);
                }
                Ok(Value::List(values))
            }

            Expr::Dict(pairs) => {
                let mut map = BTreeMap::new();
                for (key_expr, val_expr) in pairs {
                    let key = match self.eval_expr(key_expr, line)? {
                        Value::Str(s) => s,
                        other => {
                            return Err(TsumugiError::runtime(
                                line,
                                format!("辞書のキーは文字列である必要があります。got: {:?}", other),
                            ));
                        }
                    };
                    let val = self.eval_expr(val_expr, line)?;
                    if !map.contains_key(&key) {
                        crate::builtin_core::check_collection_size_public(
                            map.len().saturating_add(1),
                            line,
                        )?;
                    }
                    map.insert(key, val);
                }
                Ok(Value::Dict(map))
            }

            Expr::Ident(name) => self
                .env
                .get(name)
                .ok_or_else(|| TsumugiError::runtime(line, format!("未定義の変数: {}", name))),

            Expr::BinOp { left, op, right } => {
                // and/or は短絡評価（右辺を常に評価しない）
                match op {
                    BinOpKind::And => {
                        let l = self.eval_expr(left, line)?;
                        if !l.is_truthy() {
                            return Ok(l);
                        }
                        self.eval_expr(right, line)
                    }
                    BinOpKind::Or => {
                        let l = self.eval_expr(left, line)?;
                        if l.is_truthy() {
                            return Ok(l);
                        }
                        self.eval_expr(right, line)
                    }
                    _ => {
                        let l = self.eval_expr(left, line)?;
                        let r = self.eval_expr(right, line)?;
                        self.eval_binop(&l, op, &r, line)
                    }
                }
            }

            Expr::UnaryOp { op, expr } => {
                let val = self.eval_expr(expr, line)?;
                self.eval_unary(op, &val, line)
            }

            Expr::Call { callee, args } => self.eval_call(callee, args, line),

            Expr::Lambda { params, body } => {
                // 無名関数: 定義時のスコープの変数セルを共有してキャプチャ
                // 捕捉するのは本体で言及される名前だけ（AUD-042）
                let captured = Rc::new(
                    self.env
                        .capture_referenced(&crate::ast::referenced_names(body)),
                );
                Ok(Value::Fn {
                    def: Rc::new(FnDef {
                        name: "<lambda>".to_string(),
                        params: params.clone(),
                        body: body.clone(),
                    }),
                    captured,
                })
            }

            Expr::Index { object, index } => {
                // 副作用のないindex式なら、コレクションを複製せず
                // 変数セルから参照で読む（AUD-041）。
                if let Expr::Ident(name) = object.as_ref()
                    && crate::ast::is_side_effect_free(index)
                    && let Some(cell) = self.env.get_cell(name)
                {
                    let idx = self.eval_expr(index, line)?;
                    let collection = cell.borrow();
                    return self.eval_index(&collection, &idx, line);
                }
                let obj = self.eval_expr(object, line)?;
                let idx = self.eval_expr(index, line)?;
                self.eval_index(&obj, &idx, line)
            }

            Expr::FStr(parts) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        FStrExprPart::Literal(s) => result.push_str(s),
                        FStrExprPart::Expr(expr) => {
                            let val = self.eval_expr(expr, line)?;
                            result.push_str(&val.to_string());
                        }
                    }
                }
                Ok(Value::Str(result))
            }
        }
    }

    /// インデックスアクセスの評価
    fn eval_index(
        &self,
        object: &Value,
        index: &Value,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        match (object, index) {
            (Value::List(list), Value::Int(i)) => {
                let len = list.len() as i64;
                let actual = if *i < 0 { len + *i } else { *i };
                if actual < 0 || actual >= len {
                    return Err(TsumugiError::runtime(
                        line,
                        format!("インデックス範囲外: {} (長さ: {})", i, len),
                    ));
                }
                Ok(list[actual as usize].clone())
            }
            (Value::Dict(map), Value::Str(key)) => Ok(map.get(key).cloned().unwrap_or(Value::Null)),
            (
                Value::Error {
                    error_type,
                    message,
                    line: err_line,
                },
                Value::Str(key),
            ) => match key.as_str() {
                "type" => Ok(Value::Str(error_type.clone())),
                "message" => Ok(Value::Str(message.clone())),
                "line" => Ok(Value::Int(*err_line as i64)),
                _ => Ok(Value::Null),
            },
            (Value::Str(s), Value::Int(i)) => {
                let len = s.chars().count() as i64;
                let actual = if *i < 0 { len + *i } else { *i };
                if actual < 0 || actual >= len {
                    return Err(TsumugiError::runtime(
                        line,
                        format!("インデックス範囲外: {} (長さ: {})", i, len),
                    ));
                }
                let ch = s.chars().nth(actual as usize).unwrap();
                Ok(Value::Str(ch.to_string()))
            }
            _ => Err(TsumugiError::runtime(
                line,
                format!("インデックスアクセスできません: {:?}[{:?}]", object, index),
            )),
        }
    }

    /// 二項演算
    fn eval_binop(
        &self,
        left: &Value,
        op: &BinOpKind,
        right: &Value,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        match (left, op, right) {
            // 整数同士の算術
            (Value::Int(l), BinOpKind::Add, Value::Int(r)) => l
                .checked_add(*r)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー")),
            (Value::Int(l), BinOpKind::Sub, Value::Int(r)) => l
                .checked_sub(*r)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー")),
            (Value::Int(l), BinOpKind::Mul, Value::Int(r)) => l
                .checked_mul(*r)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー")),
            (Value::Int(l), BinOpKind::Div, Value::Int(r)) => {
                if *r == 0 {
                    Err(TsumugiError::runtime(line, "ゼロ除算"))
                } else {
                    l.checked_div(*r)
                        .map(Value::Int)
                        .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー"))
                }
            }
            (Value::Int(l), BinOpKind::Mod, Value::Int(r)) => {
                if *r == 0 {
                    Err(TsumugiError::runtime(line, "ゼロ除算"))
                } else {
                    l.checked_rem(*r)
                        .map(Value::Int)
                        .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー"))
                }
            }

            // 浮動小数点
            (Value::Float(l), BinOpKind::Add, Value::Float(r)) => Ok(Value::Float(l + r)),
            (Value::Float(l), BinOpKind::Sub, Value::Float(r)) => Ok(Value::Float(l - r)),
            (Value::Float(l), BinOpKind::Mul, Value::Float(r)) => Ok(Value::Float(l * r)),
            (Value::Float(l), BinOpKind::Div, Value::Float(r)) => Ok(Value::Float(l / r)),
            (Value::Float(l), BinOpKind::Mod, Value::Float(r)) => Ok(Value::Float(l % r)),

            // Int と Float の混合
            (Value::Int(l), BinOpKind::Add, Value::Float(r)) => Ok(Value::Float(*l as f64 + r)),
            (Value::Float(l), BinOpKind::Add, Value::Int(r)) => Ok(Value::Float(l + *r as f64)),
            (Value::Int(l), BinOpKind::Sub, Value::Float(r)) => Ok(Value::Float(*l as f64 - r)),
            (Value::Float(l), BinOpKind::Sub, Value::Int(r)) => Ok(Value::Float(l - *r as f64)),
            (Value::Int(l), BinOpKind::Mul, Value::Float(r)) => Ok(Value::Float(*l as f64 * r)),
            (Value::Float(l), BinOpKind::Mul, Value::Int(r)) => Ok(Value::Float(l * *r as f64)),
            (Value::Int(l), BinOpKind::Div, Value::Float(r)) => Ok(Value::Float(*l as f64 / r)),
            (Value::Float(l), BinOpKind::Div, Value::Int(r)) => Ok(Value::Float(l / *r as f64)),
            (Value::Int(l), BinOpKind::Mod, Value::Float(r)) => Ok(Value::Float(*l as f64 % r)),
            (Value::Float(l), BinOpKind::Mod, Value::Int(r)) => Ok(Value::Float(l % *r as f64)),

            // 文字列結合
            (Value::Str(l), BinOpKind::Add, Value::Str(r)) => Ok(Value::Str(format!("{}{}", l, r))),
            // 文字列 + Error（Error は Display で message を返す）
            (Value::Str(l), BinOpKind::Add, r @ Value::Error { .. }) => {
                Ok(Value::Str(format!("{}{}", l, r)))
            }
            (l @ Value::Error { .. }, BinOpKind::Add, Value::Str(r)) => {
                Ok(Value::Str(format!("{}{}", l, r)))
            }

            // 大小比較は数値だけを対象にする。IntとFloatは跨いで比較できる（AUD-014）
            (Value::Int(l), BinOpKind::Lt, Value::Int(r)) => Ok(Value::Bool(l < r)),
            (Value::Int(l), BinOpKind::Gt, Value::Int(r)) => Ok(Value::Bool(l > r)),
            (Value::Int(l), BinOpKind::LtEq, Value::Int(r)) => Ok(Value::Bool(l <= r)),
            (Value::Int(l), BinOpKind::GtEq, Value::Int(r)) => Ok(Value::Bool(l >= r)),
            (Value::Float(l), BinOpKind::Lt, Value::Float(r)) => Ok(Value::Bool(l < r)),
            (Value::Float(l), BinOpKind::Gt, Value::Float(r)) => Ok(Value::Bool(l > r)),
            (Value::Float(l), BinOpKind::LtEq, Value::Float(r)) => Ok(Value::Bool(l <= r)),
            (Value::Float(l), BinOpKind::GtEq, Value::Float(r)) => Ok(Value::Bool(l >= r)),
            (Value::Int(l), BinOpKind::Lt, Value::Float(r)) => Ok(Value::Bool((*l as f64) < *r)),
            (Value::Int(l), BinOpKind::Gt, Value::Float(r)) => Ok(Value::Bool((*l as f64) > *r)),
            (Value::Int(l), BinOpKind::LtEq, Value::Float(r)) => Ok(Value::Bool((*l as f64) <= *r)),
            (Value::Int(l), BinOpKind::GtEq, Value::Float(r)) => Ok(Value::Bool((*l as f64) >= *r)),
            (Value::Float(l), BinOpKind::Lt, Value::Int(r)) => Ok(Value::Bool(*l < (*r as f64))),
            (Value::Float(l), BinOpKind::Gt, Value::Int(r)) => Ok(Value::Bool(*l > (*r as f64))),
            (Value::Float(l), BinOpKind::LtEq, Value::Int(r)) => Ok(Value::Bool(*l <= (*r as f64))),
            (Value::Float(l), BinOpKind::GtEq, Value::Int(r)) => Ok(Value::Bool(*l >= (*r as f64))),

            // 等価比較は全ての型の組み合わせで結果を返す（AUD-014）
            // 判定は `Value` の等価規則へ集約し、VMと同じ意味論にする
            (l, BinOpKind::Eq, r) => Ok(Value::Bool(l == r)),
            (l, BinOpKind::NotEq, r) => Ok(Value::Bool(l != r)),

            // 論理演算は eval_expr 側で短絡評価するため、ここには到達しない
            (_, BinOpKind::And, _) | (_, BinOpKind::Or, _) => unreachable!(),

            // 被演算子の値がメッセージに入っても種別がぶれないよう、kindを明示する
            _ => Err(TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Type,
                format!("型エラー: {:?} {:?} {:?} は計算できません", left, op, right),
            )),
        }
    }

    /// 単項演算
    fn eval_unary(
        &self,
        op: &UnaryOpKind,
        val: &Value,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        match (op, val) {
            (UnaryOpKind::Neg, Value::Int(n)) => n
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー")),
            (UnaryOpKind::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
            (UnaryOpKind::Not, v) => Ok(Value::Bool(!v.is_truthy())),
            _ => Err(TsumugiError::runtime(
                line,
                format!("型エラー: {:?} {:?} は計算できません", op, val),
            )),
        }
    }

    /// 関数呼び出し
    fn eval_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        line: usize,
    ) -> Result<Value, TsumugiError> {
        // 識別子calleeはuser bindingを優先し、未定義の場合だけbuiltinへfallbackする。
        // printは予約tokenのため、通常どおりbindingなしでbuiltinへ到達する。
        if let Expr::Ident(name) = callee
            && self.env.get_cell(name).is_none()
            && let Some(value) = self.eval_builtin(name, args, line)?
        {
            return Ok(value);
        }

        // ユーザー定義関数の呼び出し: ステップカウント + 深度チェック
        self.count_step(line)?;
        if self.call_stack.len() >= MAX_CALL_DEPTH {
            return Err(TsumugiError::runtime(
                line,
                format!(
                    "スタックオーバーフロー: 再帰が深すぎます (上限: {})",
                    MAX_CALL_DEPTH
                ),
            ));
        }

        // callee を評価して関数値を取得
        // 識別子の場合: 変数として検索
        let func_value = if let Expr::Ident(name) = callee {
            self.env
                .get(name)
                .ok_or_else(|| TsumugiError::runtime(line, format!("未定義の関数: {}", name)))?
        } else {
            // 識別子以外（式の評価結果を呼び出す）
            self.eval_expr(callee, line)?
        };

        let Value::Fn { def, captured } = &func_value else {
            return Err(TsumugiError::runtime(
                line,
                format!("関数ではない値を呼び出そうとしました: {}", func_value),
            ));
        };
        // Rcを複製して以降の借用から切り離す（値の複製は起きない）
        let def = Rc::clone(def);
        let captured = Rc::clone(captured);
        let func_name = def.name.as_str();
        let params = &def.params;

        if args.len() != params.len() {
            return Err(TsumugiError::runtime(
                line,
                format!(
                    "関数 {} は引数{}個ですが、{}個渡されました",
                    func_name,
                    params.len(),
                    args.len()
                ),
            ));
        }

        // 引数を評価
        let mut arg_values = Vec::new();
        for arg in args {
            arg_values.push(self.eval_expr(arg, line)?);
        }

        // レキシカルスコープ: 呼び出し元のスコープを退避し、独立環境で実行
        let saved_scopes = self.env.push_call_frame();
        for (k, cell) in captured.iter() {
            self.env.set_shared(k, cell.clone());
        }
        // 名前付き関数は呼び出し時の関数値を宣言名へ束縛する。
        // 定義時captureへ自身を入れず、Rc cycleを避ける。
        if func_name != "<lambda>" {
            self.env.set(func_name, func_value.clone());
        }
        // parameterはself-bindingと同名ならshadowする。
        for (param, val) in params.iter().zip(arg_values) {
            self.env.set(param, val);
        }

        // コールスタックに記録
        self.call_stack.push(TraceFrame {
            name: func_name.to_string(),
            line,
        });

        // 関数本体を実行
        let mut result = Value::Null;
        for stmt in &def.body {
            match self.exec_stmt(stmt) {
                Ok(EvalResult::Return(v)) => {
                    result = v;
                    break;
                }
                Ok(EvalResult::Break) => {
                    self.call_stack.pop();
                    self.env.pop_call_frame(saved_scopes);
                    return Err(TsumugiError::runtime(
                        line,
                        "break はループの中でのみ使用できます",
                    ));
                }
                Ok(EvalResult::Continue) => {
                    self.call_stack.pop();
                    self.env.pop_call_frame(saved_scopes);
                    return Err(TsumugiError::runtime(
                        line,
                        "continue はループの中でのみ使用できます",
                    ));
                }
                Ok(EvalResult::Val) => {}
                Err(e) => {
                    let mut trace = self.call_stack.clone();
                    trace.reverse();
                    self.call_stack.pop();
                    self.env.pop_call_frame(saved_scopes);
                    return Err(e.with_trace(trace));
                }
            }
        }

        self.call_stack.pop();
        self.env.pop_call_frame(saved_scopes);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn run_program(input: &str) -> Result<(), TsumugiError> {
        let tokens = Lexer::new(input).tokenize();
        let program = Parser::new(tokens)
            .parse()
            .map_err(|errors| errors.into_iter().next().unwrap())?;
        let mut eval = Evaluator::new();
        eval.run(&program)
    }

    #[test]
    fn arithmetic() {
        // Should not error
        run_program("let x = 1 + 2 * 3").unwrap();
    }

    #[test]
    fn function_call() {
        let src = "fn add(a, b)\n  return a + b\nend\nlet r = add(3, 4)";
        run_program(src).unwrap();
    }

    #[test]
    fn undefined_variable_error() {
        let result = run_program("let x = 10\nprint(y)");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("2行目"), "should mention line 2: {}", msg);
        assert!(msg.contains("未定義の変数"));
    }

    #[test]
    fn zero_division_error() {
        let result = run_program("let x = 10 / 0");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("1行目"));
        assert!(msg.contains("ゼロ除算"));
    }

    #[test]
    fn type_error() {
        let result = run_program("let x = \"hello\" + 1");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("型エラー"));
    }

    #[test]
    fn undefined_function_error() {
        let result = run_program("foo(1, 2)");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("未定義の関数"));
    }

    #[test]
    fn wrong_arg_count() {
        let src = "fn f(a)\n  return a\nend\nf(1, 2)";
        let result = run_program(src);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("引数"));
    }

    #[test]
    fn while_loop() {
        // Just confirm it doesn't panic or infinite loop
        let src = "let i = 3\nwhile i > 0\n  i = i - 1\nend";
        run_program(src).unwrap();
    }

    #[test]
    fn assign_variable() {
        let src = "let x = 1\nx = 2\nprint(x)";
        run_program(src).unwrap();
    }

    #[test]
    fn assign_in_while_loop() {
        let src = "let count = 3\nwhile count > 0\n  count = count - 1\nend\nprint(count)";
        run_program(src).unwrap();
    }

    #[test]
    fn assign_undefined_variable_error() {
        let result = run_program("x = 42");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("1行目"), "should mention line 1: {}", msg);
        assert!(msg.contains("未定義の変数に代入"));
    }

    #[test]
    fn assign_updates_outer_scope() {
        // 関数内から引数を再代入して、関数内で反映されることを確認
        let src = "fn countdown(n)\n  while n > 0\n    print(n)\n    n = n - 1\n  end\n  return n\nend\nlet r = countdown(3)\nprint(r)";
        run_program(src).unwrap();
    }

    #[test]
    fn if_else() {
        let src = "if false\n  print(1)\nelse\n  print(2)\nend";
        run_program(src).unwrap();
    }

    #[test]
    fn string_concat() {
        run_program("let s = \"hello\" + \" world\"").unwrap();
    }

    #[test]
    fn logical_ops() {
        run_program("let x = true and false\nlet y = true or false\nlet z = not true").unwrap();
    }

    #[test]
    fn list_literal_and_index() {
        run_program("let xs = [1, 2, 3]\nprint(xs[0])\nprint(xs[-1])").unwrap();
    }

    #[test]
    fn list_index_assign() {
        run_program("let xs = [1, 2, 3]\nxs[1] = 99\nprint(xs[1])").unwrap();
    }

    #[test]
    fn dict_literal_and_access() {
        run_program("let d = {\"a\": 1, \"b\": 2}\nprint(d[\"a\"])").unwrap();
    }

    #[test]
    fn dict_index_assign() {
        run_program("let d = {}\nd[\"x\"] = 42\nprint(d[\"x\"])").unwrap();
    }

    #[test]
    fn builtin_len() {
        run_program("let xs = [1, 2, 3]\nprint(len(xs))\nprint(len(\"hello\"))").unwrap();
    }

    #[test]
    fn builtin_push() {
        run_program("let xs = []\npush(xs, 1)\npush(xs, 2)\nprint(len(xs))").unwrap();
    }

    #[test]
    fn builtin_keys() {
        run_program("let d = {\"a\": 1}\nlet ks = keys(d)\nprint(len(ks))").unwrap();
    }

    #[test]
    fn builtin_type() {
        run_program("print(type(42))\nprint(type([]))\nprint(type({}))").unwrap();
    }

    #[test]
    fn index_out_of_bounds() {
        let result = run_program("let xs = [1, 2]\nprint(xs[5])");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("インデックス範囲外")
        );
    }

    #[test]
    fn for_loop_list() {
        run_program("let xs = [1, 2, 3]\nfor x in xs\n  print(x)\nend").unwrap();
    }

    #[test]
    fn for_loop_dict() {
        run_program("let d = {\"a\": 1}\nfor k in d\n  print(k)\nend").unwrap();
    }

    #[test]
    fn for_loop_string() {
        run_program("for ch in \"hi\"\n  print(ch)\nend").unwrap();
    }

    #[test]
    fn for_loop_accumulate() {
        run_program("let total = 0\nfor n in [1, 2, 3]\n  total = total + n\nend\nprint(total)")
            .unwrap();
    }

    #[test]
    fn for_loop_non_iterable_error() {
        let result = run_program("for x in 42\n  print(x)\nend");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("for で反復できません")
        );
    }

    #[test]
    fn break_in_while() {
        run_program("let i = 0\nwhile true\n  if i == 3\n    break\n  end\n  i = i + 1\nend")
            .unwrap();
    }

    #[test]
    fn break_in_for() {
        run_program("for n in [1, 2, 3, 4, 5]\n  if n == 3\n    break\n  end\n  print(n)\nend")
            .unwrap();
    }

    #[test]
    fn continue_in_for() {
        run_program("for n in [1, 2, 3, 4, 5]\n  if n == 3\n    continue\n  end\n  print(n)\nend")
            .unwrap();
    }

    #[test]
    fn break_outside_loop_error() {
        let result = run_program("break");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("break はループの中でのみ")
        );
    }

    #[test]
    fn continue_outside_loop_error() {
        let result = run_program("continue");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("continue はループの中でのみ")
        );
    }

    #[test]
    fn modulo_operator() {
        run_program("let x = 10 % 3\nprint(x)").unwrap();
    }

    #[test]
    fn modulo_zero_error() {
        let result = run_program("let x = 10 % 0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ゼロ除算"));
    }

    #[test]
    fn elif_basic() {
        run_program(
            "let x = 5\nif x == 1\n  print(1)\nelif x == 5\n  print(5)\nelse\n  print(0)\nend",
        )
        .unwrap();
    }

    #[test]
    fn elif_multiple() {
        run_program("let x = 3\nif x == 1\n  print(1)\nelif x == 2\n  print(2)\nelif x == 3\n  print(3)\nelse\n  print(0)\nend").unwrap();
    }

    #[test]
    fn elif_no_else() {
        run_program("let x = 2\nif x == 1\n  print(1)\nelif x == 2\n  print(2)\nend").unwrap();
    }

    #[test]
    fn builtin_pop() {
        run_program("let xs = [1, 2, 3]\nlet v = pop(xs)\nprint(v)\nprint(len(xs))").unwrap();
    }

    #[test]
    fn builtin_pop_empty_error() {
        let result = run_program("let xs = []\npop(xs)");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("空のリスト"));
    }

    #[test]
    fn builtin_slice_list() {
        run_program("let xs = [1, 2, 3, 4]\nlet s = slice(xs, 1, 3)\nprint(len(s))").unwrap();
    }

    #[test]
    fn builtin_slice_string() {
        run_program("let s = slice(\"hello\", 0, 3)\nprint(s)").unwrap();
    }

    #[test]
    fn builtin_contains_list() {
        run_program("print(contains([1, 2, 3], 2))").unwrap();
    }

    #[test]
    fn builtin_contains_string() {
        run_program("print(contains(\"hello\", \"ell\"))").unwrap();
    }

    #[test]
    fn builtin_contains_dict() {
        run_program("print(contains({\"a\": 1}, \"a\"))").unwrap();
    }

    #[test]
    fn builtin_split() {
        run_program("let parts = split(\"a,b,c\", \",\")\nprint(len(parts))").unwrap();
    }

    #[test]
    fn builtin_join() {
        run_program("let s = join([\"a\", \"b\"], \"-\")\nprint(s)").unwrap();
    }

    #[test]
    fn builtin_to_int() {
        run_program("print(to_int(\"42\"))\nprint(to_int(3.7))").unwrap();
    }

    #[test]
    fn builtin_to_int_error() {
        let result = run_program("to_int(\"abc\")");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("変換失敗"));
    }

    #[test]
    fn builtin_to_str() {
        run_program("let s = to_str(42)\nprint(s)").unwrap();
    }

    #[test]
    fn builtin_range() {
        run_program("let xs = range(0, 5)\nprint(len(xs))").unwrap();
    }

    #[test]
    fn builtin_range_in_for() {
        run_program("for i in range(1, 4)\n  print(i)\nend").unwrap();
    }

    #[test]
    fn builtin_write_and_read_file() {
        run_program(
            "write_file(\"/tmp/tsumugi_unit_test.txt\", \"hello\")\nlet c = read_file(\"/tmp/tsumugi_unit_test.txt\")\nprint(c)",
        )
        .unwrap();
        // cleanup
        std::fs::remove_file("/tmp/tsumugi_unit_test.txt").ok();
    }

    #[test]
    fn builtin_read_lines() {
        run_program(
            "write_file(\"/tmp/tsumugi_lines_test.txt\", \"a\\nb\\nc\")\nlet lines = read_lines(\"/tmp/tsumugi_lines_test.txt\")\nprint(len(lines))",
        )
        .unwrap();
        std::fs::remove_file("/tmp/tsumugi_lines_test.txt").ok();
    }

    #[test]
    fn builtin_append_file() {
        run_program(
            "write_file(\"/tmp/tsumugi_append_test.txt\", \"a\")\nappend_file(\"/tmp/tsumugi_append_test.txt\", \"b\")\nlet c = read_file(\"/tmp/tsumugi_append_test.txt\")\nprint(c)",
        )
        .unwrap();
        std::fs::remove_file("/tmp/tsumugi_append_test.txt").ok();
    }

    #[test]
    fn builtin_read_file_missing() {
        run_program("let x = read_file(\"/tmp/no_such_file_xyz.txt\")\nprint(x)").unwrap();
    }

    #[test]
    fn builtin_env() {
        // HOME should always be set
        run_program("let h = env(\"HOME\")\nprint(h != null)").unwrap();
    }

    #[test]
    fn builtin_env_missing() {
        run_program("let x = env(\"NONEXISTENT_TSG_VAR\")\nprint(x)").unwrap();
    }

    #[test]
    fn builtin_args() {
        run_program("let a = args()\nprint(type(a))").unwrap();
    }

    #[test]
    fn builtin_now() {
        run_program("let ts = now()\nprint(ts > 0)").unwrap();
    }

    #[test]
    fn builtin_format_time() {
        // 2026-01-01 00:00:00 UTC = 1767225600
        run_program("let s = format_time(1767225600, \"%Y-%m-%d\")\nprint(s)").unwrap();
    }

    #[test]
    fn builtin_path_exists() {
        run_program("print(path_exists(\"/tmp\"))").unwrap();
    }

    #[test]
    fn builtin_path_exists_missing() {
        run_program("print(path_exists(\"/no_such_dir_xyz\"))").unwrap();
    }

    #[test]
    fn builtin_path_join() {
        run_program("let p = path_join(\"/home\", \"user\", \"file.txt\")\nprint(p)").unwrap();
    }

    #[test]
    fn builtin_mkdir_and_remove_dir() {
        run_program(
            "mkdir(\"/tmp/tsg_test_mkdir\")\nprint(path_exists(\"/tmp/tsg_test_mkdir\"))\nremove_dir(\"/tmp/tsg_test_mkdir\")\nprint(path_exists(\"/tmp/tsg_test_mkdir\"))",
        )
        .unwrap();
    }

    #[test]
    fn builtin_rename() {
        run_program(
            "write_file(\"/tmp/tsg_rename_src.txt\", \"x\")\nrename(\"/tmp/tsg_rename_src.txt\", \"/tmp/tsg_rename_dst.txt\")\nprint(path_exists(\"/tmp/tsg_rename_dst.txt\"))",
        )
        .unwrap();
        std::fs::remove_file("/tmp/tsg_rename_dst.txt").ok();
    }

    #[test]
    fn builtin_list_dir() {
        run_program(
            "mkdir(\"/tmp/tsg_list_test\")\nwrite_file(\"/tmp/tsg_list_test/a.txt\", \"\")\nlet entries = list_dir(\"/tmp/tsg_list_test\")\nprint(len(entries))\nremove_dir(\"/tmp/tsg_list_test\")",
        )
        .unwrap();
    }

    #[test]
    fn builtin_file_size() {
        run_program(
            "write_file(\"/tmp/tsg_size_test.txt\", \"hello\")\nlet s = file_size(\"/tmp/tsg_size_test.txt\")\nprint(s)",
        )
        .unwrap();
        std::fs::remove_file("/tmp/tsg_size_test.txt").ok();
    }

    #[test]
    fn builtin_remove_file() {
        run_program(
            "write_file(\"/tmp/tsg_remove_test.txt\", \"x\")\nprint(remove(\"/tmp/tsg_remove_test.txt\"))\nprint(path_exists(\"/tmp/tsg_remove_test.txt\"))",
        )
        .unwrap();
    }

    #[test]
    fn builtin_trim() {
        run_program("print(trim(\"  hello  \"))").unwrap();
    }

    #[test]
    fn builtin_starts_with() {
        run_program("print(starts_with(\"hello\", \"hel\"))").unwrap();
    }

    #[test]
    fn builtin_ends_with() {
        run_program("print(ends_with(\"file.txt\", \".txt\"))").unwrap();
    }

    #[test]
    fn builtin_replace() {
        run_program("print(replace(\"aabbcc\", \"bb\", \"XX\"))").unwrap();
    }

    #[test]
    fn builtin_upper_lower() {
        run_program("print(upper(\"hello\"))\nprint(lower(\"WORLD\"))").unwrap();
    }

    #[test]
    fn builtin_to_float() {
        run_program("print(to_float(\"3.14\"))\nprint(to_float(42))").unwrap();
    }

    #[test]
    fn builtin_abs() {
        run_program("print(abs(-5))\nprint(abs(3))").unwrap();
    }

    #[test]
    fn builtin_min_max() {
        run_program("print(min(10, 3))\nprint(max(10, 3))").unwrap();
    }

    #[test]
    fn builtin_sort() {
        run_program("print(sort([3, 1, 2]))").unwrap();
    }

    #[test]
    fn builtin_reverse() {
        run_program("print(reverse([1, 2, 3]))\nprint(reverse(\"abc\"))").unwrap();
    }

    #[test]
    fn builtin_is_file_is_dir() {
        run_program("print(is_dir(\"/tmp\"))\nprint(is_file(\"/tmp\"))").unwrap();
    }
}
