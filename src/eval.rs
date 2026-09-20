#[path = "builtin.rs"]
mod builtin;

use crate::ast::*;
use crate::budget::{BudgetConfig, BudgetLedger, ControlStop, ExecutionPhase};
use crate::env::Env;
use crate::error::{TraceFrame, TsumugiError};
use crate::limits::MAX_USER_CALL_DEPTH;
use crate::value::{FnDef, FunctionId, NumericOrder, Value};

use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
/// 評価器の戻り値（通常の値 or return / break / continue による制御フロー）
enum EvalResult {
    Val,
    Return(Value),
    /// `break`（ループ外で表面化したときのエラー行を保持する）。
    Break(usize),
    /// `continue`（同上）。
    Continue(usize),
}

/// `run_driver` が 1 slice 回した結果（REV-015 Slice 3 PR-d-2）。
enum DriveOutcome {
    /// この活性の本体を最後まで（または return/break/continue まで）実行し切った。
    Done(EvalResult),
    /// slice fuel を使い切って文/反復境界で協調停止した。`self.frames` に continuation が
    /// 残っており、次の `run_slice` が続きから再開する（§4.2 `Yielded(SliceFuelExhausted)`）。
    Yielded,
}

/// 明示 frame stack 上で 1 文を実行した結果、driver がどう遷移するか（REV-015 Slice 3 PR-b）。
///
/// 従来は `exec_block` / ループ本体が Rust 再帰でブロックへ降り、`EvalResult` を
/// Rust スタックの巻き戻しで伝播していた。PR-b ではその再帰をヒープ上の frame stack と
/// cursor + driver ループへ置き換える。`exec_stmt` は 1 文ぶんの「葉」の仕事だけを行い、
/// 複合文（`if` / `while` / `for` / `try`）に出会ったら子 frame の生成を driver へ指示する。
enum StmtStep {
    /// 通常完了。driver は同じ frame の次の文へ進む。
    Val,
    /// `return` / `break` / `continue`。driver は該当境界（call frame / loop）まで
    /// frame を巻き戻す。
    Flow(FlowSignal),
    /// 複合文が子 frame を要求した。driver はこの frame を push してそこから実行を続ける。
    /// `Frame` は PR-d で `'static`（AST 借用なし）になった。
    Enter(Frame),
}

/// 明示 frame stack を巻き戻す制御フロー信号（`EvalResult` の非 `Val` 部分に対応）。
enum FlowSignal {
    Return(Value),
    /// `break` とその文の行番号（ループ外エラー表示用）。
    Break(usize),
    /// `continue` とその文の行番号。
    Continue(usize),
}

/// 明示実行 frame。ヒープ上の frame stack の 1 要素で、Rust 再帰の 1 段に対応する
/// （REV-015 Slice 3 PR-b / PR-c / PR-d、§9.3）。式評価（`eval_expr`）は当面 Rust 再帰の
/// まま（式は浅く bounded）。
///
/// PR-d で frame は `'static`（AST 借用なし）になった。実行する文列は `stmts: Block`
/// （`Rc<[Stmt]>`）として所有し、loop / try / call の各種状態も borrow を持たない。これに
/// より frame stack を Evaluator が跨 `poll` で永続保持でき、slice fuel 枯渇時に suspend /
/// resume できる（§9.3「再帰する Rust call stack を continuation として使ってはならない」）。
/// `condition` / ループ変数名など個別ノードは、それを含む compound 文を `owner` Block +
/// `stmt_index` として保持し、必要時に `owner[stmt_index]` を match して読み出す（owner を
/// Rc で持つので寿命は保たれる）。
struct Frame {
    /// この frame が順に実行する文列（program 直下・関数本体・`if`/ループ/`try` 本体）。
    stmts: Block,
    /// 次に実行する文の index。
    cursor: usize,
    /// frame 種別ごとの状態（loop の反復状態・try handler・call 境界など）。
    kind: FrameKind,
    /// frame 生成時の `env.scope_depth()`。pop 時にここまでスコープを巻き戻す。
    scope_base: usize,
    /// この frame の継続を live heap へ課金したバイト数（§5.1 continuation_frame / HANDLER）。
    /// pop 時に同量を release する。
    heap_charge: u64,
}

impl Frame {
    /// この frame が `break` / `continue` を捕捉するループ frame か。
    fn is_loop(&self) -> bool {
        matches!(self.kind, FrameKind::While { .. } | FrameKind::For { .. })
    }

    /// この frame が関数呼び出しの境界（`return` の到達先）か。
    fn is_call(&self) -> bool {
        matches!(self.kind, FrameKind::Call { .. })
    }
}

/// `while` / `for` 制御 frame が参照する compound 文の位置（REV-015 Slice 3 PR-d）。
///
/// `condition`（while）やループ変数名（for）は、それを含む `Stmt::While` / `Stmt::For` の
/// フィールドである。frame が AST を借用せず `'static` になるよう、compound 文を含む
/// Block（`owner`、`Rc<[Stmt]>` を所有）とその index を保持し、必要時に `owner[stmt_index]`
/// を match して個別ノードを読み出す。owner を持つあいだノードは生き続ける。
struct LoopSite {
    /// この loop 文を含む Block（program / 関数本体 / 親ブロック）。
    owner: Block,
    /// `owner` 内でのこの loop 文の index。
    stmt_index: usize,
}

/// `advance_frame` がループ frame の借用を落とした後に実行する動作（driver 内部用）。
enum LoopAction {
    /// `while` frame: 必要なら fuel 課金し、condition 評価で次反復 or 畳む。
    While {
        site: LoopSite,
        body: Block,
        line: usize,
        charge_step: bool,
    },
    /// `for` frame: 必要なら fuel 課金し、次 item があれば本体を積む or 畳む。
    For {
        site: LoopSite,
        body: Block,
        line: usize,
        charge_step: bool,
        next_item: Option<Value>,
    },
    /// Block / Scoped / Try / Call: 文列を実行し切ったので frame を畳むだけ。
    PopPlain,
}

/// [`Frame`] の種別と反復・handler 状態（REV-015 Slice 3 PR-b / PR-d）。
enum FrameKind {
    /// 独立スコープを持たない素のブロック（program 直下・関数本体・ループ本体の 1 反復）。
    /// スコープ管理は生成側（loop frame や call）が行う。
    Block,
    /// 独立スコープを 1 つ push したブロック（`if` の分岐、`try` 本体、`catch` 本体）。
    /// pop 時に `scope_base` まで巻き戻す。
    Scoped,
    /// `while` ループ。condition を毎回評価し、本体 1 反復を子 Block frame として実行する。
    /// `pending_step` は「直前の反復本体を実行し終えた（次反復前に fuel 課金が要る）」状態。
    While {
        /// condition / line を読み出すための compound 文位置。
        site: LoopSite,
        /// 本体（`Rc<[Stmt]>` を所有）。反復ごとに clone（Rc bump）して Block frame にする。
        body: Block,
        line: usize,
        pending_step: bool,
    },
    /// `for` ループ。開始時に materialize した items を 1 個ずつ束縛して本体を実行する。
    /// `pending_step` は while と同じく反復完了後の fuel 課金待ちフラグ。
    For {
        /// ループ変数名 / line を読み出すための compound 文位置。
        site: LoopSite,
        /// 本体（`Rc<[Stmt]>` を所有）。
        body: Block,
        line: usize,
        items: Vec<Value>,
        index: usize,
        pending_step: bool,
        /// 反復元コレクション値を保持する。従来の tree 評価器は `for` の対象を局所変数
        /// `collection` としてループ実行中ずっと生かしていたため、その live heap 課金も
        /// ループ完了まで維持されていた。frame へ持たせて同じ寿命を保つ（挙動不変）。
        _collection: Value,
    },
    /// `try` 本体を実行中。本体で捕捉エラーが出たら `catch` 本体へ切り替える。
    Try {
        var: String,
        catch_body: Block,
        line: usize,
    },
    /// 関数呼び出しの境界（REV-015 Slice 3 PR-c）。VM の `CallFrame` をミラーする。
    /// この frame の `stmts` は呼び出す関数本体（`FnDef.body` の `Rc<[Stmt]>` を所有）で、
    /// 本体を実行し切るか `return` に達したら、退避した呼び出し元スコープ（`saved_scopes`）と
    /// call trace を復元して 1 個の戻り値を produce する。`break` / `continue` はこの境界を
    /// 越えられず、ここで「ループ外」エラーになる。
    Call {
        /// 呼び出し時に `env.push_call_frame()` が返した退避情報。pop 時に復元する。
        saved_scopes: crate::env::CallFrame,
    },
}

impl LoopSite {
    /// `while` 文の condition / line を読み出す。
    fn while_parts(&self) -> (&Expr, usize) {
        match &self.owner[self.stmt_index] {
            Stmt::While {
                condition, line, ..
            } => (condition, *line),
            _ => unreachable!("while LoopSite は Stmt::While を指す"),
        }
    }

    /// `for` 文のループ変数名 / line を読み出す。
    fn for_parts(&self) -> (&str, usize) {
        match &self.owner[self.stmt_index] {
            Stmt::For { var, line, .. } => (var, *line),
            _ => unreachable!("for LoopSite は Stmt::For を指す"),
        }
    }
}

/// 空の [`Block`]（`Rc<[Stmt]>`）を返す（REV-015 Slice 3 PR-d）。
///
/// `while` / `for` の controller frame は本体を持たない空 `stmts` を持ち、push 直後に
/// `cursor >= len` となって `advance_frame` へ入る（従来 `&[]` を使っていた設計をミラー）。
/// `Rc::from(Vec::new())` は要素 0 のヒープ確保のみで安価。
fn empty_block() -> Block {
    std::rc::Rc::from(Vec::<Stmt>::new())
}

/// [`Evaluator::run_phased`] が失敗フェーズを区別するための種別（REV-015 Slice 3 PR-a）。
///
/// 埋め込み handle が terminal outcome を `LinkError`（Link 中失敗）と
/// `RuntimeError`（実行中失敗）へ振り分けるために使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    /// import 解決・source/import/heap 課金など、最初の文を実行する前のフェーズ。
    Link,
    /// スクリプト本体の実行フェーズ。
    Run,
}

/// AST を評価して実行する
pub struct Evaluator {
    pub(crate) env: Env,
    /// 関数呼び出しのスタック（スタックトレース用）
    call_stack: Vec<TraceFrame>,
    /// 実行予算の課金台帳（REV-015 Slice 1）。
    /// step（fuel）と collection 要素数の検査をここへ一本化する。
    budget: BudgetLedger,
    /// import の解決状態（実行前にリンクする。AUD-030）
    loader: crate::module::ModuleLoader,
    /// 関数値へ発番する次の FunctionId（AUD-048）。単調増加し、
    /// REPL の失敗入力でも巻き戻さない。
    next_function_id: u64,
    /// `args()` が返すスクリプト引数の snapshot（AUD-018）。
    /// process argv ではなく実行 context に属し、埋め込み host が実行単位で注入する。
    script_args: Vec<String>,
    /// 実行対象 AST（linked program、root + node）の §5.1 論理サイズを live heap へ
    /// 課金するトークン（REV-015 PR-d）。AST は実行のあいだ生き続けるため execution の
    /// 寿命でトークンを保持する。REPL では入力ごとに charge_link で入れ替えて release し、
    /// 前入力の AST を持ち越さない。VM は AST を bytecode へ compile するため、代わりに
    /// bytecode chunk を課金する（§5.3「AST または bytecode」）。
    ast_token: Option<Rc<crate::value::HeapToken>>,
    /// 明示実行 frame の永続スタック（REV-015 Slice 3 PR-d）。従来は 1 回の実行ごとに
    /// `run_driver` のローカル `Vec<Frame>` を作っていたが、PR-d で `'static` になった frame を
    /// Evaluator が保持することで、跨 `poll` で continuation を維持し slice fuel 枯渇時に
    /// suspend / resume できる（PR-d-2）。関数呼び出しの再入（`drive_call_body`）はこの共有
    /// スタックへ Call frame を積み、`run_driver(stop_depth)` が自分より上だけを回す。
    frames: Vec<Frame>,
    /// この slice（1 回の `poll`）で消費した fuel（REV-015 Slice 3 PR-d-2、§4.2）。
    /// `count_step` が加算し、slice 開始時に 0 へ戻す。
    slice_fuel_used: u64,
    /// この slice で消費を許す fuel 上限（§4.2）。`None` は slice 制限なし（同期 `run` /
    /// テスト経路）で、この場合 yield しない。`Some(limit)` は `poll` が 1 slice ぶんを設定。
    slice_fuel_limit: Option<u64>,
    /// suspend / resume する実行セッション（REV-015 Slice 3 PR-d-2）。`begin_execution` で
    /// 張り、terminal で畳む。yield を跨いで module の解決マーカーと transaction 境界を保つ。
    session: Option<RunSession>,
}

/// suspend / resume できる 1 回の実行セッション（REV-015 Slice 3 PR-d-2）。
///
/// `begin_execution`（初回 poll 相当）で link と root frame push を済ませ、`run_slice` が
/// `self.frames` を 1 slice ぶん進める。yield すると `self.frames` と本 session が残り、次の
/// `run_slice` が続きから再開する。terminal（完了・エラー）で finalize する。
struct RunSession {
    /// この実行が回すべき frame stack の下限深度（root frame を積む前の `frames.len()`）。
    /// `run_driver` はここまで縮んだら本体完了とみなす。
    base_depth: usize,
    /// 実行完了しなかったとき解決マーカーを巻き戻す import module 群（AUD-030）。
    newly_loaded: Vec<crate::module::LoadedModule>,
    /// REPL transaction 経路か（未捕捉エラーで language-state を rollback する。AUD-024）。
    transactional: bool,
}

impl Evaluator {
    pub fn new() -> Self {
        Self::with_budget(BudgetConfig::from_legacy_env())
    }

    /// 明示 `BudgetConfig` で評価器を作る（埋め込み host 向け）。
    pub fn with_budget(config: BudgetConfig) -> Self {
        let budget = BudgetLedger::with_config(config);
        let mut env = Env::new();
        // cell 生成時の captured cell 課金のため、budget の heap 台帳を Env へ配る
        // （REV-015 PR-c）。
        env.set_heap_ledger(budget.heap_handle());
        Self {
            env,
            call_stack: Vec::new(),
            budget,
            loader: crate::module::ModuleLoader::new(),
            next_function_id: 0,
            script_args: Vec::new(),
            ast_token: None,
            frames: Vec::new(),
            slice_fuel_used: 0,
            slice_fuel_limit: None,
            session: None,
        }
    }

    /// `args()` が返すスクリプト引数の snapshot を設定する（AUD-018）。
    pub fn set_script_args(&mut self, args: Vec<String>) {
        self.script_args = args;
    }

    /// `args()` が返すスクリプト引数の snapshot を参照する（AUD-018）。
    pub(crate) fn script_args(&self) -> &[String] {
        &self.script_args
    }

    /// 関数値へ新しい FunctionId を発番する（AUD-048）。
    ///
    /// 関数式・関数定義を評価して値を生成するたびに呼ぶ。u64 を使い切った場合は
    /// binding が公開される前に internal error を返す（通常運用では到達不能）。
    fn allocate_function_id(&mut self, line: usize) -> Result<FunctionId, TsumugiError> {
        let id = self.next_function_id;
        self.next_function_id = self
            .next_function_id
            .checked_add(1)
            .ok_or_else(|| TsumugiError::internal(line, "FunctionId を割り当てできません"))?;
        Ok(FunctionId(id))
    }

    /// 基準ディレクトリを設定する（ファイル実行時に呼ばれる）
    pub fn set_base_dir(&mut self, path: &Path) {
        self.loader.set_base_dir(path);
    }

    /// ステップ（fuel）を 1 課金し、上限チェックする（REV-015 Slice 1）。
    ///
    /// 既存挙動どおり、total fuel 上限到達時は `step_limit` エラーを call trace 付きで返す。
    /// あわせて slice fuel 使用量（§4.2）を 1 加算する。slice 上限の判定は yield 可能な
    /// 文/反復境界（driver）で行うため、ここでは加算だけ行い yield はしない（式の途中で
    /// yield すると Rust 再帰スタックを保存できず resume 不能になるため）。
    fn count_step(&mut self, line: usize) -> Result<(), TsumugiError> {
        self.budget
            .charge_fuel(1, ExecutionPhase::Run)
            .map_err(|stop| self.control_stop_to_error(stop, line))?;
        self.slice_fuel_used = self.slice_fuel_used.saturating_add(1);
        Ok(())
    }

    /// この slice の fuel を使い切ったか（§4.2）。slice 上限が設定されていない同期実行
    /// （`run` / テスト）では常に false で、yield しない。
    fn slice_exhausted(&self) -> bool {
        match self.slice_fuel_limit {
            Some(limit) => self.slice_fuel_used >= limit,
            None => false,
        }
    }

    /// 変数 cell を作って現在スコープへ束縛する（REV-015 PR-c）。
    ///
    /// `Env::set` は cell の §5.1 captured cell（32 byte）を live heap へ課金するため
    /// fallible になった。超過 `ControlStop` を既存 `TsumugiError` へ写像する。
    fn env_set(&mut self, name: &str, value: Value, line: usize) -> Result<(), TsumugiError> {
        self.env
            .set(name, value)
            .map_err(|stop| self.control_stop_to_error(stop, line))
    }

    /// tree function instance header（§5.1 tree_function）を課金してトークンを作る
    /// （REV-015 PR-c）。captured cell 実体は cell 側で別途課金するため header ぶんだけ。
    fn new_fn_header(
        &self,
        captured_count: usize,
        line: usize,
    ) -> Result<Rc<crate::value::FnHeader>, TsumugiError> {
        Value::new_tree_fn_header(
            captured_count as u64,
            &self.budget.heap_handle(),
            ExecutionPhase::Run,
        )
        .map_err(|stop| self.control_stop_to_error(stop, line))
    }

    /// collection 要素数の per-item 検査（REV-015 Slice 1）。
    ///
    /// 既存の `check_collection_size_public` を置き換える入口。上限超過時は
    /// 既存挙動どおり `collection_limit` エラーを返す。
    fn check_collection(&mut self, size: usize, line: usize) -> Result<(), TsumugiError> {
        self.budget
            .check_collection_elements(size as u64, ExecutionPhase::Run)
            .map_err(|stop| self.control_stop_to_error(stop, line))
    }

    /// cell が保持する `Value::List` へ tracked backing 上で 1 要素 push する
    /// （delta 課金、REV-015 案A）。型・上限は呼び出し側で検査済み。
    fn budget_list_push(
        &mut self,
        cell: &crate::value::SharedValue,
        value: Value,
        line: usize,
    ) -> Result<(), TsumugiError> {
        let mut target = cell.borrow_mut();
        target
            .list_push_tracked(value, &mut self.budget, ExecutionPhase::Run)
            .map_err(|stop| crate::budget::control_stop_to_error(stop, 0, line))
    }

    /// cell が保持する `Value::List` の末尾を tracked backing 上で除く（delta release）。
    fn budget_list_pop(
        &mut self,
        cell: &crate::value::SharedValue,
        line: usize,
    ) -> Result<(), TsumugiError> {
        let mut target = cell.borrow_mut();
        target
            .list_pop_tracked(&mut self.budget, ExecutionPhase::Run)
            .map_err(|stop| crate::budget::control_stop_to_error(stop, 0, line))
    }

    /// budget の [`ControlStop`] を既存の [`TsumugiError`] へ写像する（Slice 1/2 互換）。
    /// resource → error kind/message の対応は tree/VM 共有の
    /// [`crate::budget::control_stop_to_error`] に集約し、trace だけ tree 側で付ける。
    ///
    /// Slice 1/2 で発生し得るのは fuel（= step）・collection・string 超過。cancel /
    /// deadline は ledger の charge 経路にまだ配線しておらず（Slice 4）、
    /// 到達した場合も安全側で step 上限として扱う。
    fn control_stop_to_error(&self, stop: ControlStop, line: usize) -> TsumugiError {
        let err =
            crate::budget::control_stop_to_error(stop, self.budget.usage().committed.fuel, line);
        if !self.call_stack.is_empty() {
            let mut trace = self.call_stack.clone();
            trace.reverse();
            return err.with_trace(trace);
        }
        err
    }

    /// REPLの新しい入力を開始する前にステップ予算をリセットする。
    pub fn reset_step_budget(&mut self) {
        self.budget.reset_fuel();
    }

    /// root source と import source を予算へ課金する（REV-015 Slice 2、§5.3）。
    ///
    /// `Link` フェーズの課金であり、最初の文を実行する前に行う。root（実行対象
    /// スクリプト）を 1 本の source として `charge_source` し、初めて解決した各 import
    /// module を `charge_source`（`source_bytes`）と `charge_import`（`import_bytes`）
    /// の両方へ課金する。超過は catch 不能 terminal として既存 error へ写像する。
    fn charge_link(
        &mut self,
        root_source_bytes: u64,
        loaded: &[crate::module::LoadedModule],
        linked_program: &Program,
    ) -> Result<(), TsumugiError> {
        // live heap は tracked backing の生成/drop で逐次維持する（REV-015 案A/PR-b/PR-c）。
        // collection・String・変数 cell・関数 instance header はすべて per-drop 追跡され、
        // 生成時に課金し最後の参照 drop で release する。よって同じ台帳を跨ぐ REPL 入力では
        // pre-existing な言語状態の live heap が自動的に持ち越され、Link 境界で baseline を
        // 再走査する必要がない（再走査するとむしろ tracked 分を二重計上する）。したがって
        // `charge_link` では baseline 課金を行わない。`charge_context_baseline`
        // （全 heap object を 1 回ずつ論理課金する純関数）は、execution ごとに台帳を作り直す
        // 埋め込み API（fresh ledger モデル、後続 Phase）用に残し、単体テストで固定する。
        self.budget
            .charge_source(root_source_bytes, ExecutionPhase::Link)
            .map_err(|stop| self.control_stop_to_error(stop, 0))?;
        for module in loaded {
            self.budget
                .charge_source(module.byte_len, ExecutionPhase::Link)
                .map_err(|stop| self.control_stop_to_error(stop, module.line))?;
            self.budget
                .charge_import(module.byte_len, ExecutionPhase::Link)
                .map_err(|stop| self.control_stop_to_error(stop, module.line))?;
            // imported module record を live heap へ課金する（REV-015 PR-d）。
            // module ID は normalized（canonical）path の UTF-8 byte 長で数える。token は
            // loader が `loaded` set と寿命を揃えて保持し、`forget`（rollback）または
            // loader drop で release する。
            let module_id_bytes = module.path.as_os_str().len() as u64;
            let token = Value::new_heap_token(
                crate::budget::heap_size::imported_module_record(module_id_bytes),
                &self.budget.heap_handle(),
                ExecutionPhase::Link,
            )
            .map_err(|stop| self.control_stop_to_error(stop, module.line))?;
            self.loader
                .register_record_token(module.path.clone(), token);
        }
        // 実行対象 AST（linked program）を live heap へ課金する（REV-015 PR-d）。前入力の
        // AST token を先に drop（release）してから今回ぶんを課金し、二重計上しない。tree
        // engine は AST を直接実行するため execution の寿命でトークンを保持する。
        self.ast_token = None;
        let ast_bytes = crate::ast::ast_heap_size(linked_program);
        let ast_token =
            Value::new_heap_token(ast_bytes, &self.budget.heap_handle(), ExecutionPhase::Link)
                .map_err(|stop| self.control_stop_to_error(stop, 0))?;
        self.ast_token = Some(ast_token);
        Ok(())
    }

    /// プログラム全体を実行
    ///
    /// import は実行前にリンクして解決する（AUD-030）。読み込み・パース・サンドボックス・
    /// 深度の失敗は、最初の文を実行する前に報告される。
    pub fn run(&mut self, program: &Program, root_source_bytes: u64) -> Result<(), TsumugiError> {
        let (linked, newly_loaded) = self.loader.link(program)?;
        let target = linked.as_ref().unwrap_or(program);
        // source/import 予算と AST heap を Link フェーズで課金する（REV-015 Slice 2 / PR-d）。
        // 超過なら 1 文も実行せず、解決済みマーカーを巻き戻す。
        if let Err(e) = self.charge_link(root_source_bytes, &newly_loaded, target) {
            self.loader.forget(&newly_loaded);
            return Err(e);
        }
        let result = self.exec_program(target);
        if result.is_err() {
            // 実行が完了しなかったmoduleは解決済みにしない（同じパスを再試行できる）
            self.loader.forget(&newly_loaded);
        }
        result
    }

    /// REPL の1入力をトランザクションとして実行する（AUD-024）。
    ///
    /// 未捕捉ランタイムエラーで終了した入力は、その入力が変更した全 language-state
    /// （binding、cell の値、index 代入、push/pop、import marker）を入力開始時点へ
    /// 巻き戻す。正常完了と、入力内で catch されて最終的に正常完了したエラーは commit
    /// する。stdout やファイル書き込みなどの外部効果は巻き戻さない。
    ///
    /// link 失敗（読み込み・parse・sandbox・深度）は最初の文を実行する前に報告され、
    /// この場合 language-state は変化していないため journal は空のまま破棄される。
    pub fn run_repl_submission(
        &mut self,
        program: &Program,
        root_source_bytes: u64,
    ) -> Result<(), TsumugiError> {
        self.env.begin_submission();
        let (linked, newly_loaded) = match self.loader.link(program) {
            Ok(linked) => linked,
            Err(e) => {
                // link はまだ language-state を変えていない。journal を破棄する。
                self.env.commit_submission();
                return Err(e);
            }
        };
        let target = linked.as_ref().unwrap_or(program);
        // source/import 予算と AST heap を Link フェーズで課金する（REV-015 Slice 2 / PR-d）。
        // 超過なら 1 文も実行せず、language-state も import marker も巻き戻す。
        if let Err(e) = self.charge_link(root_source_bytes, &newly_loaded, target) {
            self.env.rollback_submission();
            self.loader.forget(&newly_loaded);
            return Err(e);
        }
        let result = self.exec_program(target);
        if result.is_err() {
            // language-state を入力開始時点へ戻す。
            self.env.rollback_submission();
            // 実行が完了しなかったmoduleは解決済みにしない（同じパスを再試行できる）。
            self.loader.forget(&newly_loaded);
        } else {
            self.env.commit_submission();
        }
        result
    }

    /// 現在の予算使用量 snapshot を返す（REV-015 Slice 3 PR-a）。
    ///
    /// 埋め込み API の `poll` / terminal outcome が `BudgetUsage` を公開するために使う。
    pub fn budget_usage(&self) -> crate::budget::BudgetUsage {
        self.budget.usage()
    }

    /// 実行セッションを開始する（REV-015 Slice 3 PR-d-2、poll 経路の初回）。
    ///
    /// import を link し、source/import/AST heap を Link フェーズで課金し、REPL transaction を
    /// 開始し、root 文列を Block frame として共有 frame stack へ積む。まだ 1 文も実行しない。
    /// 成功後は [`Self::run_slice`] を繰り返し呼んで実行を進める。Link 失敗は
    /// `Err((RunPhase::Link, _))` を返し、セッションは張らない（language-state 不変）。
    pub fn begin_execution(
        &mut self,
        program: &Program,
        root_source_bytes: u64,
        transactional: bool,
    ) -> Result<(), (RunPhase, TsumugiError)> {
        if transactional {
            self.env.begin_submission();
        }
        let (linked, newly_loaded) = match self.loader.link(program) {
            Ok(linked) => linked,
            Err(e) => {
                if transactional {
                    // link はまだ language-state を変えていない。journal を破棄する。
                    self.env.commit_submission();
                }
                return Err((RunPhase::Link, e));
            }
        };
        let target = linked.as_ref().unwrap_or(program);
        if let Err(e) = self.charge_link(root_source_bytes, &newly_loaded, target) {
            if transactional {
                self.env.rollback_submission();
            }
            self.loader.forget(&newly_loaded);
            return Err((RunPhase::Link, e));
        }
        if let Err(e) = validate_program_depth(target) {
            if transactional {
                self.env.rollback_submission();
            }
            self.loader.forget(&newly_loaded);
            return Err((RunPhase::Run, e));
        }
        // root 文列を Block（Rc<[Stmt]>）へ写して frame へ所有させる。frame が Rc を持つので
        // linked（ローカル Program）は drop してよい。
        let root: Block = std::rc::Rc::from(target.as_slice());
        let base_depth = self.frames.len();
        self.frames.push(Frame {
            stmts: root,
            cursor: 0,
            kind: FrameKind::Block,
            scope_base: self.env.scope_depth(),
            heap_charge: 0,
        });
        self.session = Some(RunSession {
            base_depth,
            newly_loaded,
            transactional,
        });
        Ok(())
    }

    /// 進行中セッションを 1 slice ぶん進める（REV-015 Slice 3 PR-d-2、§4.2）。
    ///
    /// `slice_fuel` はこの slice で消費を許す fuel 上限。総 fuel はセッションを跨いで
    /// 単調に維持され、slice 上限は補充されない（実際に commit した fuel だけが total へ乗る）。
    /// 戻り値:
    /// - `Ok(None)`: slice fuel を使い切って文/反復境界で yield した。frame stack を保持し、
    ///   次の `run_slice` で続きから再開する。
    /// - `Ok(Some(Ok(())))`: 実行が正常完了した（terminal）。transaction を commit 済み。
    /// - `Ok(Some(Err(e)))`: 実行中に未捕捉エラーで終了した（terminal）。transaction を
    ///   rollback 済み、module 解決マーカーも巻き戻し済み。
    ///
    /// セッションが無い状態で呼ぶと `panic`（呼び出し側が begin_execution 済みを保証する）。
    /// `None` = yield（継続あり）、`Some(result)` = terminal。
    pub fn run_slice(&mut self, slice_fuel: u64) -> Option<Result<(), TsumugiError>> {
        let base_depth = self
            .session
            .as_ref()
            .expect("run_slice requires an active session")
            .base_depth;
        // この slice の fuel 予算を張り直す（slice ごとに 0 から数える）。
        self.slice_fuel_used = 0;
        self.slice_fuel_limit = Some(slice_fuel);

        let outcome = self.run_driver(base_depth, true);

        // slice 中だけ有効な制限を解除する（同期経路が影響を受けないように）。
        self.slice_fuel_limit = None;

        match outcome {
            Ok(DriveOutcome::Yielded) => None,
            Ok(DriveOutcome::Done(result)) => {
                // 本体完了。EvalResult を terminal 結果へ写し、transaction / module を finalize。
                let final_result = match result {
                    EvalResult::Return(_) | EvalResult::Val => Ok(()),
                    EvalResult::Break(line) => Err(TsumugiError::break_outside_loop(line)),
                    EvalResult::Continue(line) => Err(TsumugiError::continue_outside_loop(line)),
                };
                self.finalize_session(final_result.is_ok());
                Some(final_result)
            }
            Err(e) => {
                // 未捕捉エラーで terminal。unwind は run_driver 内で stop_depth まで済んでいる。
                self.finalize_session(false);
                Some(Err(e))
            }
        }
    }

    /// セッションを畳む（REV-015 Slice 3 PR-d-2）。commit なら transaction を確定、失敗なら
    /// language-state を rollback し、完了しなかった module の解決マーカーを巻き戻す。
    fn finalize_session(&mut self, committed: bool) {
        let Some(session) = self.session.take() else {
            return;
        };
        if committed {
            if session.transactional {
                self.env.commit_submission();
            }
        } else {
            if session.transactional {
                self.env.rollback_submission();
            }
            self.loader.forget(&session.newly_loaded);
        }
    }

    /// [`Self::run`] と同じ実行をしつつ、失敗が Link フェーズか実行フェーズかを返す
    /// （REV-015 Slice 3 PR-a）。
    ///
    /// 埋め込み handle が terminal outcome を `LinkError` と `RuntimeError` に振り分ける
    /// ために使う。観測挙動は [`Self::run`] と同一で、成功なら `Ok(())`、失敗なら
    /// 発生フェーズ（`RunPhase::Link` / `RunPhase::Run`）と error を返す。
    pub fn run_phased(
        &mut self,
        program: &Program,
        root_source_bytes: u64,
    ) -> Result<(), (RunPhase, TsumugiError)> {
        let (linked, newly_loaded) = match self.loader.link(program) {
            Ok(linked) => linked,
            Err(e) => return Err((RunPhase::Link, e)),
        };
        let target = linked.as_ref().unwrap_or(program);
        if let Err(e) = self.charge_link(root_source_bytes, &newly_loaded, target) {
            self.loader.forget(&newly_loaded);
            return Err((RunPhase::Link, e));
        }
        match self.exec_program(target) {
            Ok(()) => Ok(()),
            Err(e) => {
                // 実行が完了しなかった module は解決済みにしない（同じパスを再試行できる）。
                self.loader.forget(&newly_loaded);
                Err((RunPhase::Run, e))
            }
        }
    }

    fn exec_program(&mut self, program: &Program) -> Result<(), TsumugiError> {
        validate_program_depth(program)?;
        // Program は Vec<Stmt> のまま。root 文列を Block（Rc<[Stmt]>）へ写して frame へ所有
        // させる（PR-d）。`Rc::from(&[Stmt])` は要素を 1 度だけ複製し、以降の子 frame は
        // owner Rc を共有する。
        let root: Block = std::rc::Rc::from(program.as_slice());
        // 同期実行（`run` / `run_phased` / `run_repl_submission`）は 1 回で terminal まで
        // 回す（`can_yield = false`）。slice fuel での yield は poll 経路（begin_execution /
        // run_slice）だけで起きる。トップレベルの break/continue はその文の行番号でエラー化。
        match self.drive_body(root, false)? {
            DriveOutcome::Done(EvalResult::Return(_) | EvalResult::Val) => Ok(()),
            DriveOutcome::Done(EvalResult::Break(line)) => {
                Err(TsumugiError::break_outside_loop(line))
            }
            DriveOutcome::Done(EvalResult::Continue(line)) => {
                Err(TsumugiError::continue_outside_loop(line))
            }
            DriveOutcome::Yielded => {
                unreachable!("exec_program は can_yield=false なので yield しない")
            }
        }
    }

    /// 1 つの文列（program 直下・関数本体・`if`/`try`/ループ本体）を明示 frame stack で
    /// 実行する driver（REV-015 Slice 3 PR-b）。
    ///
    /// Rust 再帰でブロックへ降りていた `exec_block` / `exec_scoped_block` / ループ本体を、
    /// ヒープ上の frame stack + cursor + driver ループへ置き換える。制御フロー
    /// （return / break / continue）は Rust スタックの巻き戻しではなく、frame stack を
    /// 明示的に unwind して処理する（§9.3）。式評価（`eval_expr`）と関数呼び出し
    /// （`eval_call`）は当面 Rust 再帰のまま（PR-c / 式は bounded）。
    ///
    /// 戻り値の `EvalResult` は従来の `exec_block` と同一意味:
    /// - `Val`: 文列を最後まで実行した
    /// - `Return(v)`: この文列を含む関数活性が値 `v` で return した
    /// - `Break` / `Continue`: この文列を囲むループへ抜ける制御フロー
    ///
    /// エラーは従来どおり `Err` で伝播する。エラー・return / break / continue の全経路で、
    /// push 済み frame のスコープ解放と continuation heap の release を行う。
    fn drive_body(&mut self, root: Block, can_yield: bool) -> Result<DriveOutcome, TsumugiError> {
        // 共有 frame stack へ root 文列を素の Block frame として積む。root は呼び出し側
        // （exec_program / callback 本体）がスコープを管理するため、ここでは新しいスコープを
        // push しない（scope_base は現在の深さ）。`base` はこの活性が回すべき下限深度で、
        // run_driver は frames がここまで縮んだら「本体完了」として返す。
        let base = self.frames.len();
        self.frames.push(Frame {
            stmts: root,
            cursor: 0,
            kind: FrameKind::Block,
            scope_base: self.env.scope_depth(),
            heap_charge: 0,
        });
        self.run_driver(base, can_yield)
    }

    /// 関数本体を明示 call frame として driver で実行する（REV-015 Slice 3 PR-c）。
    ///
    /// VM の `run_frames` 相当で、呼び出し境界を [`FrameKind::Call`] としてヒープ frame stack
    /// の root に積む。`def`（`Rc<FnDef>`）はこの関数のローカルとして保持し、その `&def.body`
    /// を frame の `stmts`（`'p`）として渡す。関数本体とそこから派生する子 frame は
    /// `drive_call_body` の実行中ずっと生きるこの本体 AST を借用する（VM の `CallFrame` が
    /// `Rc<Chunk>` を保持するのに対応する、tree 側の等価な寿命管理）。呼び出し元は事前に
    /// `env.push_call_frame()` でスコープを退避し、その退避情報 `saved_scopes` をこの Call
    /// frame へ預ける。frame の pop（正常終了・return・エラー unwind）で `env.pop_call_frame`
    /// と call trace の巻き戻しが行われ、呼び出し元スコープを復元する。
    ///
    /// 戻り値は `drive_body` と同じ `EvalResult`。`Return(v)` は Call frame まで unwind して
    /// `v` を返し、`Break` / `Continue` は Call 境界を越えられずそのまま surface する
    /// （呼び出し側が「ループ外」エラーへ写す）。
    fn drive_call_body(
        &mut self,
        def: Rc<FnDef>,
        saved_scopes: crate::env::CallFrame,
    ) -> Result<EvalResult, TsumugiError> {
        // 関数本体 `def.body` は `Block`（Rc<[Stmt]>）。clone は Rc bump のみで、frame 自身が
        // 本体 AST を root する（VM の CallFrame が Rc<Chunk> を持つのに対応）。共有 frame stack
        // へ Call frame を積み、この活性の下限深度 `base` から上だけを run_driver が回す。
        let base = self.frames.len();
        self.frames.push(Frame {
            stmts: def.body.clone(),
            cursor: 0,
            kind: FrameKind::Call { saved_scopes },
            // Call frame 自身は呼び出し元がスコープ管理する（push_call_frame 済み）。関数用
            // ローカルスコープは push_call_frame が積み、pop_call_frame が畳むため、この frame
            // 自身は追加スコープを持たない（scope_base は現在の深さ、heap_charge は 0）。
            scope_base: self.env.scope_depth(),
            heap_charge: 0,
        });
        // 関数呼び出しは式の途中（Rust 再帰）から再入するため、この活性は yield せず
        // 完了させる（`can_yield = false`）。slice fuel の消費はここでも count_step で
        // 加算され、呼び出しから戻ったあとトップレベル driver の境界で yield 判定される。
        match self.run_driver(base, false)? {
            DriveOutcome::Done(result) => Ok(result),
            DriveOutcome::Yielded => {
                unreachable!("drive_call_body は can_yield=false なので yield しない")
            }
        }
    }

    /// frame stack を回す共通 driver ループ（REV-015 Slice 3 PR-b / PR-c）。
    ///
    /// root frame は `drive_body`（Block）または `drive_call_body`（Call）が積む。
    /// 共有 frame stack `self.frames` を回す driver（REV-015 Slice 3 PR-d）。
    ///
    /// `stop_depth` はこの活性が回すべき下限深度で、`drive_body` / `drive_call_body` が
    /// frame を積む前の `self.frames.len()`。`self.frames.len() <= stop_depth` になったら、
    /// この活性の本体を（return なしで）実行し切ったとして `Ok(EvalResult::Val)` を返す。
    /// VM の `run_frames(stop_depth)` をミラーする。return / break / continue / エラーの
    /// 巻き戻しはすべて `stop_depth` を下限に行い、外側の活性の frame を侵さない。
    fn run_driver(
        &mut self,
        stop_depth: usize,
        can_yield: bool,
    ) -> Result<DriveOutcome, TsumugiError> {
        loop {
            // frame がこの活性の下限まで縮んだ = 本体を最後まで実行した。
            if self.frames.len() <= stop_depth {
                return Ok(DriveOutcome::Done(EvalResult::Val));
            }

            // slice fuel を使い切っていれば、文/反復境界で協調停止する（§4.2）。ここは
            // exec_stmt / advance_frame を呼ぶ前＝Rust 再帰スタックが driver ループまで
            // 巻き戻った安全な境界なので、`self.frames` を残したまま resume できる。yield
            // できるのはトップレベル実行の driver だけ（`can_yield`）。関数呼び出しの再入
            // （`drive_call_body`、式の途中）では yield せず活性を完了させる。
            if can_yield && self.slice_exhausted() {
                return Ok(DriveOutcome::Yielded);
            }

            let top = self.frames.last_mut().expect("frames.len() > stop_depth");

            // ループ frame は cursor が本体末尾に達したら「1 反復完了」として扱い、
            // 次反復（condition 再評価 / 次 item）へ進む。advance 中のエラー（condition
            // 再評価・末尾 count_step・反復スコープ/束縛の課金）も、文実行中のエラーと同じく
            // 最も近い try/catch へ渡す。従来はループ全体が try 本体で実行されたため、これらの
            // エラーも catch されていた（tree/VM parity）。
            if top.cursor >= top.stmts.len() {
                match self.advance_frame() {
                    Ok(()) => {}
                    Err(e) => {
                        let e = self.attach_trace(e);
                        match self.handle_error(e, stop_depth) {
                            Ok(()) => {}
                            Err(e) => {
                                self.unwind_to(stop_depth);
                                return Err(e);
                            }
                        }
                    }
                }
                continue;
            }

            // 次の文を実行する。owner は最上位 frame の文列（Block = Rc<[Stmt]>）を clone した
            // ローカル（Rc bump のみ）。`stmt` はこのローカル `owner` を借用するので、
            // `exec_stmt` が `&mut self`（= self.frames を含む）を取っても借用が衝突しない。
            // owner + index は compound 文（while/for）の LoopSite にも使う。
            let owner = top.stmts.clone();
            let index = top.cursor;
            top.cursor += 1;
            let stmt = &owner[index];
            let step = self.exec_stmt(stmt, &owner, index);
            match step {
                Ok(StmtStep::Val) => {}
                Ok(StmtStep::Enter(child)) => self.frames.push(child),
                Ok(StmtStep::Flow(signal)) => {
                    if let Some(result) = self.unwind_flow(signal, stop_depth)? {
                        return Ok(DriveOutcome::Done(result));
                    }
                }
                Err(e) => {
                    // エラー発生時点（frame をまだ畳む前）の call_stack をトレースとして付加
                    // する。この後 handle_error / unwind_to が Call frame を畳んで call_stack
                    // を巻き戻すため、ここで snapshot しないと最内フレームが欠落する（PR-c）。
                    // `with_trace` は既にトレースがあれば上書きしないので、外側の再入 driver
                    // では no-op になり、最深トレースが保たれる。
                    let e = self.attach_trace(e);
                    // catch 可能なら最も近い try frame の catch へ切り替える。
                    match self.handle_error(e, stop_depth) {
                        Ok(()) => {}
                        Err(e) => {
                            self.unwind_to(stop_depth);
                            return Err(e);
                        }
                    }
                }
            }
        }
    }

    /// 最上位 frame の文列を末尾まで実行し終えたときの遷移（driver 内部用）。
    ///
    /// ループ frame なら次反復（`while` は condition 再評価、`for` は次 item）へ進み、
    /// それ以外の frame（Block / Scoped / Try）は 1 反復ぶんの意味を持たないので pop する。
    fn advance_frame(&mut self) -> Result<(), TsumugiError> {
        // 末尾に到達した最上位 frame の種別に応じて次反復判定を組み立てる。self.frames の
        // 借用は begin_*_iteration / pop_frame / eval_expr（&mut self）を呼ぶ前に必ず落とす
        // ため、まず「何をするか」を [`LoopAction`] へ落としてから実行する。LoopSite / body は
        // Rc（Block）を clone して frame から切り離す（Rc bump のみ）。condition / var は
        // LoopSite.owner（clone した Block ローカル）越しに読むので self.frames を借用しない。
        let top = self
            .frames
            .last_mut()
            .expect("advance_frame requires a frame");
        let action = match &mut top.kind {
            FrameKind::While {
                site,
                body,
                line,
                pending_step,
            } => {
                let site = LoopSite {
                    owner: site.owner.clone(),
                    stmt_index: site.stmt_index,
                };
                let body = body.clone();
                let line = *line;
                let charge_step = *pending_step;
                *pending_step = false;
                LoopAction::While {
                    site,
                    body,
                    line,
                    charge_step,
                }
            }
            FrameKind::For {
                site,
                body,
                line,
                items,
                index,
                pending_step,
                ..
            } => {
                let site = LoopSite {
                    owner: site.owner.clone(),
                    stmt_index: site.stmt_index,
                };
                let body = body.clone();
                let line = *line;
                let charge_step = *pending_step;
                *pending_step = false;
                let next_item = if *index < items.len() {
                    let item = items[*index].clone();
                    *index += 1;
                    Some(item)
                } else {
                    None
                };
                LoopAction::For {
                    site,
                    body,
                    line,
                    charge_step,
                    next_item,
                }
            }
            _ => LoopAction::PopPlain,
        };

        match action {
            LoopAction::While {
                site,
                body,
                line,
                charge_step,
            } => {
                // 直前反復の本体を実行し終えていれば、次反復判定の前に fuel を課金する
                // （従来 while の末尾 count_step と同じ順序・タイミング）。
                if charge_step {
                    self.count_step(line)?;
                }
                // condition は site.owner（clone した Block ローカル）越しに読む。この借用は
                // self.frames ではなくローカル `site` に紐づくので、eval_expr（&mut self）を
                // 呼べる。
                let (condition, _cond_line) = site.while_parts();
                let cond = self.eval_expr(condition, line)?;
                if cond.is_truthy() {
                    // 次反復の本体を積む前に、この While frame の pending_step を立てる。
                    if let FrameKind::While { pending_step, .. } =
                        &mut self.frames.last_mut().expect("while frame present").kind
                    {
                        *pending_step = true;
                    }
                    self.begin_loop_iteration(body, line)?;
                } else {
                    self.pop_frame();
                }
            }
            LoopAction::For {
                site,
                body,
                line,
                charge_step,
                next_item,
            } => {
                if charge_step {
                    self.count_step(line)?;
                }
                match next_item {
                    Some(item) => {
                        // var は site.owner（clone した Block ローカル）越しに読む。次反復本体を
                        // 積む前に pending_step を立てる。
                        let (var, _var_line) = site.for_parts();
                        let var = var.to_string();
                        if let FrameKind::For { pending_step, .. } =
                            &mut self.frames.last_mut().expect("for frame present").kind
                        {
                            *pending_step = true;
                        }
                        self.begin_for_iteration(&var, item, body, line)?;
                    }
                    None => self.pop_frame(),
                }
            }
            LoopAction::PopPlain => {
                // Block / Scoped / Try / Call: 文列を実行し切ったので frame を畳む。
                self.pop_frame();
            }
        }
        Ok(())
    }

    /// 制御フロー信号（return / break / continue）で frame stack を巻き戻す（driver 内部用）。
    ///
    /// - `Return(v)`: この文列を含む関数活性の境界（= root frame）まで全 frame を畳み、
    ///   `Some(EvalResult::Return(v))` を返す。呼び出し側（exec_program / eval_call）が
    ///   関数の戻り値へ写す。
    /// - `Break` / `Continue`: 最も近いループ frame まで畳む。ループ frame が無ければ
    ///   `Some(EvalResult::Break/Continue)` を返し、囲む文列（exec_program / eval_call）が
    ///   `break/continue outside loop` エラーへ写す。ループ frame が有れば、`Break` は
    ///   その frame を畳んで反復を終え、`Continue` は本体を残り分スキップして次反復の判定へ
    ///   進める。
    fn unwind_flow(
        &mut self,
        signal: FlowSignal,
        stop_depth: usize,
    ) -> Result<Option<EvalResult>, TsumugiError> {
        match signal {
            FlowSignal::Return(v) => {
                // 関数活性境界（Call frame）まで、なければこの活性の下限 `stop_depth` まで
                // 畳む。Call frame の pop で `env.pop_call_frame` と call trace の巻き戻しが
                // 行われる（PR-c）。program 直下（Call frame の無い drive_body）では
                // stop_depth まで畳んで Return を surface し、exec_program が「関数外 return」
                // ではなく通常完了として扱う。共有スタックのため stop_depth より下（外側の
                // 活性）は侵さない。
                while self.frames.len() > stop_depth {
                    let is_call = self.frames.last().expect("len > stop_depth").is_call();
                    self.pop_frame();
                    if is_call {
                        break;
                    }
                }
                Ok(Some(EvalResult::Return(v)))
            }
            FlowSignal::Break(line) => {
                loop {
                    if self.frames.len() <= stop_depth {
                        // この活性内にループ frame が無い = ループ外 break。囲む文列
                        // （exec_program / eval_call）がエラー化する。
                        return Ok(Some(EvalResult::Break(line)));
                    }
                    if self.frames.last().expect("len > stop_depth").is_loop() {
                        // ループ frame を畳んで反復を終える。本体 Block frame は既に
                        // pop 済みで、次に来るのがループ controller frame。break は
                        // controller ごと畳んでループを終える。
                        self.pop_frame();
                        return Ok(None);
                    }
                    self.pop_frame();
                }
            }
            FlowSignal::Continue(line) => {
                loop {
                    if self.frames.len() <= stop_depth {
                        return Ok(Some(EvalResult::Continue(line)));
                    }
                    if self.frames.last().expect("len > stop_depth").is_loop() {
                        // controller frame はそのまま残す。本体 Block frame は
                        // 下で既に畳んだので、controller が次に advance_frame へ入り、
                        // fuel 課金 + 次反復開始をする。
                        return Ok(None);
                    }
                    self.pop_frame();
                }
            }
        }
    }

    /// 実行中エラーを最も近い `try` frame の `catch` へ渡す（driver 内部用）。
    ///
    /// try frame より内側の frame を畳んでスコープと continuation heap を解放し、
    /// try 本体スコープも解放してから catch 本体スコープを push し、error 値を束縛して
    /// catch 本体を実行する Scoped frame へ差し替える。try frame が無ければ `Err(e)` を
    /// そのまま返し、呼び出し側で unwind する。従来の `TryCatch` と同じく、fuel/collection/
    /// heap など全種の `Err` を捕捉する（現行挙動を保つ）。
    fn handle_error(&mut self, error: TsumugiError, stop_depth: usize) -> Result<(), TsumugiError> {
        // この活性内（stop_depth より上）で最も近い try frame を探す。stop_depth より下
        // （外側の活性）の try は侵さない。見つからなければエラーを伝播し、呼び出し元の
        // run_driver が自分の frame を巻き戻す。
        while self.frames.len() > stop_depth {
            if matches!(
                self.frames.last().expect("len > stop_depth").kind,
                FrameKind::Try { .. }
            ) {
                break;
            }
            self.pop_frame();
        }
        if self.frames.len() <= stop_depth {
            // try frame が無い: エラーを伝播する。
            return Err(error);
        }
        // Try frame の状態を取り出す。var は String、catch_body は Block（どちらも move /
        // clone で取り出す）。scope_base / heap_charge も値でコピーし、self.frames の借用を
        // 落としてから env / budget 操作（&mut self）を行う。共有スタックのため、handle_error
        // 途中で self.frames を借用したまま &mut self を呼べない（PR-d 制約）。
        let top = self.frames.last_mut().expect("len > stop_depth");
        let scope_base = top.scope_base;
        let try_heap_charge = top.heap_charge;
        let (var, catch_body, line) = match &mut top.kind {
            FrameKind::Try {
                var,
                catch_body,
                line,
            } => (std::mem::take(var), catch_body.clone(), *line),
            _ => unreachable!("loop above stops only at a Try frame"),
        };
        // try 本体ぶんの continuation heap は下で release するので、frame の記録は先に 0 に
        // する。これをしないと、この後の charge 失敗や catch 本体の pop で二重 release になる。
        // ここで top 借用を確定させ、以降は self.frames の借用を持たない。
        top.heap_charge = 0;

        // try 本体で push したスコープと continuation heap を解放し、catch 用へ差し替える。
        self.env.truncate_scopes(scope_base);
        self.budget.release_heap(try_heap_charge);

        let err_line = error.line();
        let error_value = Value::Error {
            error_type: error.error_type().to_string(),
            message: error.message().to_string(),
            line: err_line,
        };

        // catch 本体は独立スコープ。従来の TryCatch と同じ順序で scope を push し、
        // continuation heap を課金してから error 値を束縛する。
        self.env.push_scope();
        let charge = crate::budget::heap_size::continuation_frame(1);
        if let Err(stop) = self.budget.charge_heap(charge, ExecutionPhase::Run) {
            self.env.truncate_scopes(scope_base);
            return Err(self.control_stop_to_error(stop, err_line));
        }
        if let Err(e) = self.env_set(&var, error_value, err_line) {
            self.env.truncate_scopes(scope_base);
            self.budget.release_heap(charge);
            return Err(e);
        }
        // Try frame を catch 本体を実行する Scoped frame へ差し替える。scope_base は try frame
        // 生成時と同じ（catch も同じ base へ戻す）ため据え置く。
        let top = self.frames.last_mut().expect("try frame still present");
        top.stmts = catch_body;
        top.cursor = 0;
        top.kind = FrameKind::Scoped;
        top.heap_charge = charge;
        let _ = line;
        Ok(())
    }

    /// ループ本体 1 反復ぶんの Block frame を積む（`while` 用、driver 内部用）。
    ///
    /// 従来の `while` は反復ごとに `push_scope` / `exec_block` / `pop_scope` していた。
    /// ここではスコープを push し continuation heap を課金してから本体 Block frame を積む。
    fn begin_loop_iteration(&mut self, body: Block, line: usize) -> Result<(), TsumugiError> {
        let scope_base = self.env.scope_depth();
        self.env.push_scope();
        let charge = crate::budget::heap_size::continuation_frame(0);
        if let Err(stop) = self.budget.charge_heap(charge, ExecutionPhase::Run) {
            self.env.truncate_scopes(scope_base);
            return Err(self.control_stop_to_error(stop, line));
        }
        self.frames.push(Frame {
            stmts: body,
            cursor: 0,
            kind: FrameKind::Block,
            scope_base,
            heap_charge: charge,
        });
        Ok(())
    }

    /// `for` ループ本体 1 反復ぶんのスコープを作り、ループ変数を束縛して Block frame を積む。
    fn begin_for_iteration(
        &mut self,
        var: &str,
        item: Value,
        body: Block,
        line: usize,
    ) -> Result<(), TsumugiError> {
        let scope_base = self.env.scope_depth();
        self.env.push_scope();
        let charge = crate::budget::heap_size::continuation_frame(1);
        if let Err(stop) = self.budget.charge_heap(charge, ExecutionPhase::Run) {
            self.env.truncate_scopes(scope_base);
            return Err(self.control_stop_to_error(stop, line));
        }
        // cell 課金の超過でも iteration scope / continuation heap を必ず解放する。
        if let Err(e) = self.env_set(var, item, line) {
            self.budget.release_heap(charge);
            self.env.truncate_scopes(scope_base);
            return Err(e);
        }
        self.frames.push(Frame {
            stmts: body,
            cursor: 0,
            kind: FrameKind::Block,
            scope_base,
            heap_charge: charge,
        });
        Ok(())
    }

    /// 最上位 frame を 1 つ畳み、スコープと continuation heap を解放する（driver 内部用）。
    ///
    /// Call frame は呼び出し境界なので、通常のスコープ巻き戻しではなく
    /// `env.pop_call_frame` で呼び出し元スコープ・`frame_base` を復元し、call trace も畳む。
    fn pop_frame(&mut self) {
        if let Some(frame) = self.frames.pop() {
            self.budget.release_heap(frame.heap_charge);
            match frame.kind {
                FrameKind::Call { saved_scopes, .. } => {
                    self.call_stack.pop();
                    self.env.pop_call_frame(saved_scopes);
                }
                _ => self.env.truncate_scopes(frame.scope_base),
            }
        }
    }

    /// この活性の frame を `stop_depth` まで畳んでスコープと continuation heap を解放する
    /// （エラー / return 時）。共有スタックのため stop_depth より下（外側の活性）は残す。
    fn unwind_to(&mut self, stop_depth: usize) {
        while self.frames.len() > stop_depth {
            self.pop_frame();
        }
    }

    /// エラー発生時点の call trace を error へ付加する（REV-015 Slice 3 PR-c）。
    ///
    /// PR-c 以前は各 `eval_call` / `call_fn_value` が自身の `Err` 経路で `with_trace` を
    /// 呼んでいた。Call frame を明示 frame stack へ移したことで、frame の pop（handle_error /
    /// unwind_to）で call_stack が巻き戻る。そのため frame を畳む前・エラー発生時点の
    /// call_stack を snapshot してトレースにする。`with_trace` は既にトレースがある error を
    /// 上書きしないため、最内の失敗が積んだ最深トレースが保たれ、外側の再入 driver では
    /// no-op になる（VM の `attach_trace` と同じ意味論）。
    fn attach_trace(&self, error: TsumugiError) -> TsumugiError {
        let mut trace = self.call_stack.clone();
        trace.reverse();
        error.with_trace(trace)
    }

    /// 1 文の「葉」を実行する（REV-015 Slice 3 PR-b で戻り値を [`StmtStep`] 化）。
    ///
    /// 単純文はここで完結して `StmtStep::Val` を返す。`return` / `break` / `continue` は
    /// `StmtStep::Flow` を返し、driver が frame stack を巻き戻す。複合文（`if` / `while` /
    /// `for` / `try`）は子 frame を組み立てて `StmtStep::Enter` を返し、driver がそれを
    /// push する。これによりブロック・ループの Rust 再帰がヒープ frame stack へ移る。
    fn exec_stmt(
        &mut self,
        stmt: &Stmt,
        owner: &Block,
        stmt_index: usize,
    ) -> Result<StmtStep, TsumugiError> {
        match stmt {
            Stmt::Let { name, value, line } => {
                let val = self.eval_expr(value, *line)?;
                self.env_set(name, val, *line)?;
                Ok(StmtStep::Val)
            }

            Stmt::Assign { name, value, line } => {
                let val = self.eval_expr(value, *line)?;
                match self.env.update(name, val) {
                    Ok(()) => {}
                    Err(crate::env::UpdateError::Undefined) => {
                        return Err(TsumugiError::assign_undefined(*line, name));
                    }
                    Err(crate::env::UpdateError::Budget(stop)) => {
                        return Err(self.control_stop_to_error(stop, *line));
                    }
                }
                Ok(StmtStep::Val)
            }

            Stmt::IndexAssign {
                name,
                index,
                value,
                line,
            } => {
                // 規範順序: target binding解決 → index → value → in-place更新。
                // bindingを先に解決するため、未定義変数はindex/valueの副作用より前に報告する。
                let cell = self
                    .env
                    .get_cell(name)
                    .ok_or_else(|| TsumugiError::undefined_name(*line, name))?;
                let idx = self.eval_expr(index, *line)?;
                let val = self.eval_expr(value, *line)?;

                // REPL transaction のため、cellのin-place変更前に元値を記録する（AUD-024）。
                self.env
                    .journal_cell(&cell)
                    .map_err(|stop| self.control_stop_to_error(stop, *line))?;
                // 更新はcellへのin-place代入。index/valueの評価中に同じbindingが
                // 変更されていても、その最新状態に対して書き込む。
                let max_collection = self.budget.max_collection_elements();
                crate::builtin_core::assign_index(
                    &mut cell.borrow_mut(),
                    &idx,
                    val,
                    max_collection,
                    &mut self.budget,
                    *line,
                )?;

                Ok(StmtStep::Val)
            }

            Stmt::Return { value, line } => {
                let val = self.eval_expr(value, *line)?;
                Ok(StmtStep::Flow(FlowSignal::Return(val)))
            }

            Stmt::If {
                condition,
                then_body,
                else_body,
                line,
            } => {
                let cond = self.eval_expr(condition, *line)?;
                // 選んだ分岐本体（Block）を clone（Rc bump）して独立スコープ frame へ渡す。
                let body = if cond.is_truthy() {
                    then_body.clone()
                } else {
                    else_body.clone()
                };
                // 従来の exec_scoped_block を独立スコープ frame として driver へ渡す。
                Ok(StmtStep::Enter(self.enter_scoped_block(body, *line)?))
            }

            Stmt::While {
                condition, line, ..
            } => {
                // condition を初回評価し、真なら While controller frame を driver へ渡す。偽なら
                // frame を積まず即完了する。condition / body は controller の LoopSite
                // （owner + stmt_index）越しに読むので、ここでは owner / index を enter_while へ
                // 渡す。
                let cond = self.eval_expr(condition, *line)?;
                if !cond.is_truthy() {
                    return Ok(StmtStep::Val);
                }
                Ok(StmtStep::Enter(self.enter_while(owner, stmt_index, *line)?))
            }

            Stmt::For { iter, line, .. } => {
                let collection = self.eval_expr(iter, *line)?;
                let items: Vec<Value> = match &collection {
                    Value::List(list) => {
                        self.check_collection(list.len(), *line)?;
                        // 開始時点の要素を snapshot する。ループ本体で元 binding を
                        // 変更しても make_mut が detach するため反復列は不変（AUD-047）。
                        (**list).clone()
                    }
                    Value::Dict(map) => {
                        self.check_collection(map.len(), *line)?;
                        map.keys().map(|k| Value::str_constant(k.clone())).collect()
                    }
                    Value::Str(s) => {
                        let size = s.chars().count();
                        self.check_collection(size, *line)?;
                        s.chars()
                            .map(|c| Value::str_constant(c.to_string()))
                            .collect()
                    }
                    _ => {
                        return Err(TsumugiError::not_iterable(*line, &collection));
                    }
                };
                if items.is_empty() {
                    return Ok(StmtStep::Val);
                }
                // var / body は controller の LoopSite（owner + stmt_index）越しに読むので、
                // ここでは owner / index を enter_for へ渡す。materialize 済み items と
                // 反復元 collection は controller frame が保持する（従来の局所変数と同じ寿命）。
                Ok(StmtStep::Enter(
                    self.enter_for(owner, stmt_index, items, collection, *line)?,
                ))
            }

            Stmt::FnDef {
                name,
                params,
                body,
                line,
            } => {
                // 関数を値として環境にセット
                // ネストされた関数定義の場合、定義時のスコープをキャプチャする
                // 捕捉するのは本体で言及される名前だけ（AUD-042）
                // 本体ASTの複製は定義時の一度だけで、以降の呼び出しはRcを共有する
                // FunctionId は定義を評価するたびに新規発番する（AUD-048）
                let id = self.allocate_function_id(*line)?;
                let captured = Rc::new(
                    self.env
                        .capture_referenced(&crate::ast::referenced_names(body)),
                );
                let header = self.new_fn_header(captured.len(), *line)?;
                self.env_set(
                    name,
                    Value::Fn {
                        id,
                        def: Rc::new(FnDef {
                            name: name.clone(),
                            params: params.clone(),
                            body: body.clone(),
                        }),
                        captured,
                        header,
                    },
                    *line,
                )?;
                Ok(StmtStep::Val)
            }

            Stmt::Break { line } => Ok(StmtStep::Flow(FlowSignal::Break(*line))),

            Stmt::Continue { line } => Ok(StmtStep::Flow(FlowSignal::Continue(*line))),

            // import はリンク時に解決済みなので、ここへは到達しない
            Stmt::Import { line, .. } => Err(TsumugiError::internal(
                *line,
                "import がリンクされていません",
            )),

            Stmt::TryCatch {
                try_body,
                var,
                catch_body,
                line,
            } => {
                // try と catch は別 scope。try 本体を実行する Try frame を積む。本体で
                // 捕捉エラーが出たら driver の handle_error が catch 本体へ切り替える。
                // Block（try_body / catch_body）は clone（Rc bump）、var は所有 String にして
                // frame が持つ。
                Ok(StmtStep::Enter(self.enter_try(
                    try_body.clone(),
                    var,
                    catch_body.clone(),
                    *line,
                )?))
            }

            Stmt::ExprStmt { expr, line } => {
                self.eval_expr(expr, *line)?;
                Ok(StmtStep::Val)
            }
        }
    }

    /// 独立スコープブロック（`if` 分岐・`try`/`catch` 本体）の frame を組み立てる。
    /// スコープを push し §5.1 continuation_frame を課金する（REV-015 Slice 3 PR-b）。
    fn enter_scoped_block(&mut self, stmts: Block, line: usize) -> Result<Frame, TsumugiError> {
        let scope_base = self.env.scope_depth();
        self.env.push_scope();
        let charge = crate::budget::heap_size::continuation_frame(0);
        if let Err(stop) = self.budget.charge_heap(charge, ExecutionPhase::Run) {
            self.env.truncate_scopes(scope_base);
            return Err(self.control_stop_to_error(stop, line));
        }
        Ok(Frame {
            stmts,
            cursor: 0,
            kind: FrameKind::Scoped,
            scope_base,
            heap_charge: charge,
        })
    }

    /// `while` の本体 1 反復ぶんのスコープを作り、本体 Block frame と While frame を
    /// まとめて返す（driver は返された frame を push し、以降 advance_frame が反復を回す）。
    fn enter_while(
        &mut self,
        owner: &Block,
        stmt_index: usize,
        line: usize,
    ) -> Result<Frame, TsumugiError> {
        // While frame（反復制御）自身はスコープ・heap を持たない空 stmts の controller。
        // driver が push した直後に cursor==0>=len==0 で advance_frame へ入り、そこで
        // condition を評価して反復本体（begin_loop_iteration）を積む。反復ごとのスコープ・
        // continuation heap は本体 Block frame 側が持つ。condition / body は owner[stmt_index]
        // （Stmt::While）から読み出す。body は controller に持たせて反復ごとに clone する。
        let body = match &owner[stmt_index] {
            Stmt::While { body, .. } => body.clone(),
            _ => unreachable!("enter_while は Stmt::While に対して呼ばれる"),
        };
        Ok(Frame {
            stmts: empty_block(),
            cursor: 0,
            kind: FrameKind::While {
                site: LoopSite {
                    owner: owner.clone(),
                    stmt_index,
                },
                body,
                line,
                pending_step: false,
            },
            scope_base: self.env.scope_depth(),
            heap_charge: 0,
        })
    }

    /// `for` の反復制御 frame を返す（items は materialize 済み）。
    fn enter_for(
        &mut self,
        owner: &Block,
        stmt_index: usize,
        items: Vec<Value>,
        collection: Value,
        line: usize,
    ) -> Result<Frame, TsumugiError> {
        // var / body は owner[stmt_index]（Stmt::For）から読み出す。body は controller に
        // 持たせて反復ごとに clone する。var は反復ごとに LoopSite 越しに読む。
        let body = match &owner[stmt_index] {
            Stmt::For { body, .. } => body.clone(),
            _ => unreachable!("enter_for は Stmt::For に対して呼ばれる"),
        };
        Ok(Frame {
            stmts: empty_block(),
            cursor: 0,
            kind: FrameKind::For {
                site: LoopSite {
                    owner: owner.clone(),
                    stmt_index,
                },
                body,
                line,
                items,
                index: 0,
                pending_step: false,
                _collection: collection,
            },
            scope_base: self.env.scope_depth(),
            heap_charge: 0,
        })
    }

    /// `try` 本体を実行する Try frame を組み立てる。スコープを push し continuation_frame を
    /// 課金する（catch 側は handle_error が別スコープで用意する）。
    fn enter_try(
        &mut self,
        try_body: Block,
        var: &str,
        catch_body: Block,
        line: usize,
    ) -> Result<Frame, TsumugiError> {
        let scope_base = self.env.scope_depth();
        self.env.push_scope();
        let charge = crate::budget::heap_size::continuation_frame(0);
        if let Err(stop) = self.budget.charge_heap(charge, ExecutionPhase::Run) {
            self.env.truncate_scopes(scope_base);
            return Err(self.control_stop_to_error(stop, line));
        }
        Ok(Frame {
            stmts: try_body,
            cursor: 0,
            kind: FrameKind::Try {
                var: var.to_string(),
                catch_body,
                line,
            },
            scope_base,
            heap_charge: charge,
        })
    }

    /// 式を評価して値を返す（line は文の行番号をエラー表示に使う）
    pub(crate) fn eval_expr(&mut self, expr: &Expr, line: usize) -> Result<Value, TsumugiError> {
        match expr {
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::Str(s) => {
                // 文字列リテラルは dispatch を経由しないため、ここで cumulative string
                // accounting と live heap を課金する（REV-015 Slice 2）。track_result は
                // untracked な String を charge_string + new_str で tracked へ昇格させる。
                self.budget
                    .track_result(Value::str_constant(s.clone()), ExecutionPhase::Run)
                    .map_err(|stop| self.control_stop_to_error(stop, line))
            }
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),

            Expr::List(items) => {
                let mut values = Vec::new();
                for item in items {
                    let value = self.eval_expr(item, line)?;
                    self.check_collection(values.len().saturating_add(1), line)?;
                    values.push(value);
                }
                // heap 課金付きで tracked backing を作る（REV-015 案A、§5.2）。
                Value::new_list(values, &mut self.budget, ExecutionPhase::Run)
                    .map_err(|stop| self.control_stop_to_error(stop, line))
            }

            Expr::Dict(pairs) => {
                let mut map = BTreeMap::new();
                for (key_expr, val_expr) in pairs {
                    let key = match self.eval_expr(key_expr, line)? {
                        Value::Str(s) => s.to_string(),
                        other => {
                            return Err(TsumugiError::dict_key_type(line, &other));
                        }
                    };
                    let val = self.eval_expr(val_expr, line)?;
                    if !map.contains_key(&key) {
                        self.check_collection(map.len().saturating_add(1), line)?;
                    }
                    map.insert(key, val);
                }
                Value::new_dict(map, &mut self.budget, ExecutionPhase::Run)
                    .map_err(|stop| self.control_stop_to_error(stop, line))
            }

            Expr::Ident(name) => self
                .env
                .get(name)
                .ok_or_else(|| TsumugiError::undefined_name(line, name)),

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
                        let result = self.eval_binop(&l, op, &r, line)?;
                        // 文字列結合（`+`）が新規生成する String body を課金する
                        // （REV-015 Slice 2）。track_result は untracked な String だけを
                        // charge_string + new_str で昇格させ、数値/真偽値など他の binop
                        // 結果はそのまま返す。
                        self.budget
                            .track_result(result, ExecutionPhase::Run)
                            .map_err(|stop| self.control_stop_to_error(stop, line))
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
                // FunctionId は lambda 式を評価するたびに新規発番する（AUD-048）
                let id = self.allocate_function_id(line)?;
                let captured = Rc::new(
                    self.env
                        .capture_referenced(&crate::ast::referenced_names(body)),
                );
                let header = self.new_fn_header(captured.len(), line)?;
                Ok(Value::Fn {
                    id,
                    def: Rc::new(FnDef {
                        name: "<lambda>".to_string(),
                        params: params.clone(),
                        body: body.clone(),
                    }),
                    captured,
                    header,
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
                // f-string の生成 body も dispatch を経由しないため課金する（REV-015 Slice 2）。
                self.budget
                    .track_result(Value::str_constant(result), ExecutionPhase::Run)
                    .map_err(|stop| self.control_stop_to_error(stop, line))
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
        match object {
            Value::List(list) => {
                let Value::Int(i) = index else {
                    return Err(TsumugiError::list_index_type(line, index));
                };
                let len = list.len() as i64;
                let actual = if *i < 0 { len + *i } else { *i };
                if actual < 0 || actual >= len {
                    return Err(TsumugiError::list_index_out_of_range(line, *i, list.len()));
                }
                Ok(list[actual as usize].clone())
            }
            Value::Str(s) => {
                let Value::Int(i) = index else {
                    return Err(TsumugiError::str_index_type(line, index));
                };
                let count = s.chars().count();
                let len = count as i64;
                let actual = if *i < 0 { len + *i } else { *i };
                if actual < 0 || actual >= len {
                    return Err(TsumugiError::str_index_out_of_range(line, *i, count));
                }
                let ch = s.chars().nth(actual as usize).unwrap();
                Ok(Value::str_constant(ch.to_string()))
            }
            Value::Dict(map) => {
                let Value::Str(key) = index else {
                    return Err(TsumugiError::dict_key_type(line, index));
                };
                Ok(map.get(key.as_str()).cloned().unwrap_or(Value::Null))
            }
            Value::Error {
                error_type,
                message,
                line: err_line,
            } => {
                let Value::Str(key) = index else {
                    return Err(TsumugiError::dict_key_type(line, index));
                };
                match key.as_str() {
                    "type" => Ok(Value::str_constant(error_type.clone())),
                    "message" => Ok(Value::str_constant(message.clone())),
                    "line" => Ok(Value::Int(*err_line as i64)),
                    _ => Ok(Value::Null),
                }
            }
            _ => Err(TsumugiError::index_read_unsupported(line, object)),
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
                .ok_or_else(|| TsumugiError::int_overflow(line, "加算")),
            (Value::Int(l), BinOpKind::Sub, Value::Int(r)) => l
                .checked_sub(*r)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::int_overflow(line, "減算")),
            (Value::Int(l), BinOpKind::Mul, Value::Int(r)) => l
                .checked_mul(*r)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::int_overflow(line, "乗算")),
            (Value::Int(l), BinOpKind::Div, Value::Int(r)) => {
                if *r == 0 {
                    Err(TsumugiError::zero_division(line))
                } else {
                    l.checked_div(*r)
                        .map(Value::Int)
                        .ok_or_else(|| TsumugiError::int_overflow(line, "除算"))
                }
            }
            (Value::Int(l), BinOpKind::Mod, Value::Int(r)) => {
                if *r == 0 {
                    Err(TsumugiError::zero_division(line))
                } else {
                    l.checked_rem(*r)
                        .map(Value::Int)
                        .ok_or_else(|| TsumugiError::int_overflow(line, "剰余"))
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
            (Value::Str(l), BinOpKind::Add, Value::Str(r)) => {
                Ok(Value::str_constant(format!("{}{}", l.as_str(), r.as_str())))
            }
            // 文字列 + Error（Error は Display で message を返す）
            (Value::Str(l), BinOpKind::Add, r @ Value::Error { .. }) => {
                Ok(Value::str_constant(format!("{}{}", l.as_str(), r)))
            }
            (l @ Value::Error { .. }, BinOpKind::Add, Value::Str(r)) => {
                Ok(Value::str_constant(format!("{}{}", l, r.as_str())))
            }

            // 大小比較は数値だけを対象にする。IntとFloatは跨いで厳密比較する
            // （REV-003）。判定は NumericOrder へ集約し、VMと同じ意味論にする。
            // NaN が絡む比較は UnorderedNaN として全演算子を false にする。
            (
                l @ (Value::Int(_) | Value::Float(_)),
                BinOpKind::Lt | BinOpKind::Gt | BinOpKind::LtEq | BinOpKind::GtEq,
                r @ (Value::Int(_) | Value::Float(_)),
            ) => {
                // 数値ペアなので compare_relational は必ず Some を返す。
                let ord =
                    NumericOrder::compare_relational(l, r).expect("Int/Float ペアは数値比較できる");
                let result = match op {
                    BinOpKind::Lt => ord.is_lt(),
                    BinOpKind::Gt => ord.is_gt(),
                    BinOpKind::LtEq => ord.is_le(),
                    BinOpKind::GtEq => ord.is_ge(),
                    _ => unreachable!(),
                };
                Ok(Value::Bool(result))
            }

            // 等価比較は全ての型の組み合わせで結果を返す（AUD-014）
            // 判定は `Value` の等価規則へ集約し、VMと同じ意味論にする
            (l, BinOpKind::Eq, r) => Ok(Value::Bool(l == r)),
            (l, BinOpKind::NotEq, r) => Ok(Value::Bool(l != r)),

            // 論理演算は eval_expr 側で短絡評価するため、ここには到達しない
            (_, BinOpKind::And, _) | (_, BinOpKind::Or, _) => unreachable!(),

            // 大小比較の対象型不正（数値以外・型混在）
            (l, BinOpKind::Lt | BinOpKind::Gt | BinOpKind::LtEq | BinOpKind::GtEq, r) => {
                Err(TsumugiError::comparison_type(line, *op, l, r))
            }

            // 算術演算の対象型不正
            (l, op, r) => Err(TsumugiError::arithmetic_type(line, *op, l, r)),
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
                .ok_or_else(|| TsumugiError::int_overflow(line, "符号反転")),
            (UnaryOpKind::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
            (UnaryOpKind::Not, v) => Ok(Value::Bool(!v.is_truthy())),
            _ => Err(TsumugiError::unary_type(line, *op, val)),
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
        if self.call_stack.len() >= MAX_USER_CALL_DEPTH {
            return Err(TsumugiError::call_depth_limit(line, MAX_USER_CALL_DEPTH));
        }

        // callee を評価して関数値を取得
        // 識別子の場合: 変数として検索
        let func_value = if let Expr::Ident(name) = callee {
            self.env
                .get(name)
                .ok_or_else(|| TsumugiError::undefined_name(line, name))?
        } else {
            // 識別子以外（式の評価結果を呼び出す）
            self.eval_expr(callee, line)?
        };

        let Value::Fn { def, captured, .. } = &func_value else {
            return Err(TsumugiError::not_callable(line, &func_value));
        };
        // Rcを複製して以降の借用から切り離す（値の複製は起きない）
        let def = Rc::clone(def);
        let captured = Rc::clone(captured);
        let func_name = def.name.as_str();
        let params = &def.params;

        if args.len() != params.len() {
            return Err(TsumugiError::user_arity(
                line,
                func_name,
                params.len(),
                args.len(),
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
            // journal entry の課金超過でも call frame を必ず解放してから返す（REV-015 PR-d）。
            if let Err(e) = self.env.set_shared(k, cell.clone()) {
                self.env.pop_call_frame(saved_scopes);
                return Err(self.control_stop_to_error(e, line));
            }
        }
        // 名前付き関数は呼び出し時の関数値を宣言名へ束縛する。
        // 定義時captureへ自身を入れず、Rc cycleを避ける。
        // cell 課金の超過でも call frame を必ず解放してから返す（REV-015 PR-c）。
        if func_name != "<lambda>"
            && let Err(e) = self.env.set(func_name, func_value.clone())
        {
            self.env.pop_call_frame(saved_scopes);
            return Err(self.control_stop_to_error(e, line));
        }
        // parameterはself-bindingと同名ならshadowする。
        for (param, val) in params.iter().zip(arg_values) {
            if let Err(e) = self.env.set(param, val) {
                self.env.pop_call_frame(saved_scopes);
                return Err(self.control_stop_to_error(e, line));
            }
        }

        // コールスタックに記録（Call frame の pop 時に対で pop される）。
        self.call_stack.push(TraceFrame {
            name: func_name.to_string(),
            line,
        });

        // 関数本体を明示 Call frame として driver で実行する（REV-015 Slice 3 PR-c）。
        // 呼び出し境界がヒープ frame stack 上の [`FrameKind::Call`] になり、スコープ退避
        // （`saved_scopes`）と call trace の巻き戻しは全終了経路で Call frame の pop_frame が
        // 行う。そのため eval_call 側では drive_call_body 後に env / trace を触らない
        // （終了経路ごとの手動 pop_call_frame は不要になった）。エラー時のトレース付加は
        // driver の attach_trace が担う。
        match self.drive_call_body(def, saved_scopes) {
            Ok(EvalResult::Return(v)) => Ok(v),
            Ok(EvalResult::Val) => Ok(Value::Null),
            // ループ外 break/continue は offending 文の行番号でエラー化する（VM と一致）。
            Ok(EvalResult::Break(err_line)) => Err(TsumugiError::break_outside_loop(err_line)),
            Ok(EvalResult::Continue(err_line)) => {
                Err(TsumugiError::continue_outside_loop(err_line))
            }
            Err(e) => Err(e),
        }
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
        eval.run(&program, input.len() as u64)
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
        assert!(msg.contains("演算子 + は Str と Int に適用できません"));
    }

    #[test]
    fn undefined_function_error() {
        let result = run_program("foo(1, 2)");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("未定義の変数または関数: foo"));
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
                .contains("List のインデックスが範囲外です")
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
                .contains("反復できない型です: Int")
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("pop は空の List には使用できません")
        );
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("to_int で Int に変換できません")
        );
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

    #[test]
    fn function_id_is_monotonic_and_starts_at_zero() {
        // AUD-048: allocate_function_id は 0 から単調増加する
        let mut eval = Evaluator::new();
        assert_eq!(eval.allocate_function_id(1).unwrap(), FunctionId(0));
        assert_eq!(eval.allocate_function_id(1).unwrap(), FunctionId(1));
        assert_eq!(eval.allocate_function_id(1).unwrap(), FunctionId(2));
    }

    #[test]
    fn function_id_overflow_reports_internal_error_before_binding() {
        // AUD-048: u64 を使い切ったら binding が公開される前に internal error を返す
        let mut eval = Evaluator::new();
        eval.next_function_id = u64::MAX - 1;
        // 最後の 1 個は割り当てられる
        assert_eq!(
            eval.allocate_function_id(3).unwrap(),
            FunctionId(u64::MAX - 1)
        );
        // 次はオーバーフローで internal error
        let err = eval.allocate_function_id(3).unwrap_err();
        match err {
            TsumugiError::Runtime {
                line,
                message,
                kind,
                ..
            } => {
                assert_eq!(line, 3);
                assert_eq!(kind, crate::error::ErrorKind::Internal);
                assert!(
                    message.contains("FunctionId を割り当てできません"),
                    "想定外のメッセージ: {message}"
                );
            }
            other => panic!("Runtime error を期待したが {other:?}"),
        }
    }
}
