//! 仮想マシン: バイトコード（Chunk）を実行するスタックマシン

use std::collections::HashMap;
use std::rc::Rc;

use crate::budget::{BudgetConfig, BudgetLedger, ControlStop, ExecutionPhase};
use crate::chunk::Chunk;
use crate::error::TsumugiError;
use crate::limits::MAX_USER_CALL_DEPTH;
use crate::opcode::{CaptureDesc, MutationTarget, OpCode};
use crate::value::{FunctionId, NumericOrder, SharedValue, Value};
use crate::verifier::VerifiedChunk;

/// 演算・比較の型エラーを作る（AUD-014）
///
/// 内部不変条件の破れを構造化エラーにする（AUD-023）
///
/// libraryから不正な`Chunk`を渡された場合でもhost panicさせず、
/// `internal`種別のランタイムエラーとして返すために使う。
fn internal_error(line: usize, message: impl Into<String>) -> TsumugiError {
    TsumugiError::runtime_with_kind(line, crate::error::ErrorKind::Internal, message)
}

/// コールフレーム: 関数呼び出しの状態を保存する
#[derive(Debug, Clone)]
struct CallFrame {
    /// この関数の Chunk（Rc で共有）
    chunk: Rc<Chunk>,
    /// 命令ポインタ（この関数内の次に実行する命令のインデックス）
    ip: usize,
    /// スタック上のベース位置（この関数のローカル変数 slot 0 に対応）
    base: usize,
    /// この関数がキャプチャした upvalue セル（参照キャプチャ方式）
    upvalues: Vec<SharedValue>,
    /// ローカル変数のうちキャプチャされたもののセル
    /// locals_cells[slot] が Some のとき、その変数はヒープ上のセルで管理される
    locals_cells: Vec<Option<SharedValue>>,
}

/// 例外ハンドラ: try/catch のスタック状態を保持
#[derive(Debug, Clone)]
struct TryHandler {
    /// catch ブロックの先頭命令アドレス
    catch_ip: usize,
    /// try 開始時のスタック深さ（エラー時にスタックを巻き戻す）
    stack_depth: usize,
    /// try 開始時のフレーム深さ
    frame_depth: usize,
    /// try 開始時に対象フレームで有効だったローカル変数数
    ///
    /// unwind時はこの境界より後ろのtry-local cellだけを破棄する。境界内の既存localが
    /// try中に初めてcell化された場合、その昇格はcatch後も維持する。
    locals_count: usize,
}

/// binding の値が置かれている場所
///
/// cell化済みならヒープ上のセル、未cell化ならスタックスロットを指す。
/// 破壊的更新をbinding全体の書き戻しなしで適用するために使う。
enum BindingStorage {
    Cell(SharedValue),
    Stack(usize),
}

/// REPL入力の開始時点にあった言語状態を、変更された箇所だけ記録するjournal（AUD-024）。
///
/// 通常の入力は既存のtop-level bindingを読むだけなので、保持中のList/Dictを
/// 入力ごとに深く複製しない。既存slotの書換・削除、cellの書換、stack slotの
/// cell昇格が発生した場合だけ、rollback用にその時点の情報を記録する。
struct ReplStackCheckpoint {
    /// 入力開始時点のvalue stack長。
    stack_len: usize,
    /// 書換・削除された既存stack slotの、最初の値。
    originals: HashMap<usize, Value>,
    /// 書換されたcellの、最初の値。cellのRcポインタ同一性で1回だけ記録する。
    ///
    /// cell（`Rc<RefCell<Value>>`）は既存closureやtop-level変数と共有され、
    /// `frames` checkpointを戻してもcellの中身は戻らない。破壊的更新の前に
    /// 元値を記録し、未捕捉エラー時に復元する（AUD-024）。
    cell_originals: HashMap<usize, (SharedValue, Value)>,
    /// 各 journal entry（stack slot / cell の退避）の rollback journal entry（§5.1）を
    /// live heap へ課金するトークン（REV-015 PR-d）。entry と 1:1 で積み、checkpoint が
    /// drop（commit=`take`、rollback=`restore`）されるときにまとめて release される。
    /// entry 数に比例した有限量だけを課金する（§10）。
    entry_tokens: Vec<Rc<crate::value::HeapToken>>,
}

/// スタックベースの仮想マシン
pub struct Vm {
    /// コールフレームスタック
    frames: Vec<CallFrame>,

    /// 値スタック
    stack: Vec<Value>,

    /// REPL実行中だけ有効な、変更済みstack slotのrollback journal。
    repl_stack_checkpoint: Option<ReplStackCheckpoint>,

    /// 実行済みtop-level宣言の名前からstack slotへの対応。
    /// 値自体はstack/locals_cellsをsource of truthとし、bindingを複製しない。
    globals: HashMap<String, usize>,

    /// 実行予算の課金台帳（REV-015 Slice 1）。
    /// per-instruction の fuel 課金と collection 要素数検査をここへ一本化する。
    budget: BudgetLedger,

    /// 例外ハンドラスタック（try/catch）
    try_handlers: Vec<TryHandler>,

    /// rollback journal entry の heap 課金が上限を超えたとき、次の per-instruction step
    /// 課金境界で surface する保留エラー（REV-015 PR-d）。checkpoint 記録は `pop` など
    /// 多数の infallible 経路から呼ばれるため、その場で `Result` を返さず、ここへ退避し
    /// て `count_step` で `BudgetExceeded(HeapBytes)` を返す。既定上限では発生しない。
    pending_heap_stop: Option<ControlStop>,

    /// 実行中に adopt した bytecode chunk 木（root + 全 prototype chunk）の §5.1
    /// bytecode chunk を live heap へ課金するトークン（REV-015 PR-d）。chunk は実行の
    /// あいだ生き続ける（root frame・prototype・`VmFn` が `Rc<Chunk>` を共有する）ため、
    /// VM が execution の寿命でトークンを保持し、VM drop で release する。REPL では入力
    /// ごとの chunk を毎回 adopt し直すため、`run_repl_chunk` が入力の完了時に前回分を
    /// 入れ替えて release する。
    chunk_tokens: Vec<Rc<crate::value::HeapToken>>,

    /// 関数値へ発番する次の FunctionId（AUD-048）。単調増加し、
    /// REPL の失敗入力でも巻き戻さない。
    next_function_id: u64,

    /// `args()` が返すスクリプト引数の snapshot（AUD-018）。
    /// process argv ではなく実行 context に属する。
    script_args: Vec<String>,

    /// この実行に付与された capability 集合（Phase 2 C1〜）。VM 経路の埋め込み統合は
    /// E9（Phase 5）のため、現状は ambient 互換の既定 set（ProcessExit を grant）を使う。
    capabilities: crate::capability::CapabilitySet,

    /// `exit()` の terminal 終了コード（Phase 2 C7、REV-023）。granted な `exit(code)` が
    /// `Some(code)` を載せ catch 不可の `ProcessExit` 信号を伝播する。CLI が terminal で
    /// 読み取り実際の exit code へ写す。
    pending_exit: Option<u8>,
}

impl Vm {
    pub fn new(chunk: VerifiedChunk) -> Self {
        let frame = CallFrame {
            chunk: Rc::new(chunk.into_inner()),
            ip: 0,
            base: 0,
            upvalues: Vec::new(),
            locals_cells: Vec::new(),
        };
        Vm {
            frames: vec![frame],
            stack: Vec::with_capacity(256),
            repl_stack_checkpoint: None,
            globals: HashMap::new(),
            budget: BudgetLedger::with_config(BudgetConfig::from_legacy_env()),
            try_handlers: Vec::new(),
            pending_heap_stop: None,
            chunk_tokens: Vec::new(),
            next_function_id: 0,
            script_args: Vec::new(),
            capabilities: crate::capability::CapabilitySet::ambient_compat(),
            pending_exit: None,
        }
    }

    /// REPL 用: 空のスタックで VM を生成（最初の run_repl_chunk で使用）
    pub fn new_repl() -> Self {
        Vm {
            frames: Vec::new(),
            stack: Vec::with_capacity(256),
            repl_stack_checkpoint: None,
            globals: HashMap::new(),
            budget: BudgetLedger::with_config(BudgetConfig::from_legacy_env()),
            try_handlers: Vec::new(),
            pending_heap_stop: None,
            chunk_tokens: Vec::new(),
            next_function_id: 0,
            script_args: Vec::new(),
            capabilities: crate::capability::CapabilitySet::ambient_compat(),
            pending_exit: None,
        }
    }

    /// `args()` が返すスクリプト引数の snapshot を設定する（AUD-018）。
    pub fn set_script_args(&mut self, args: Vec<String>) {
        self.script_args = args;
    }

    /// terminal で記録済み `exit` コードを取り出す（C7、REV-023）。CLI が実際の exit code へ写す。
    pub fn take_pending_exit(&mut self) -> Option<u8> {
        self.pending_exit.take()
    }

    /// step（fuel）上限を明示的に設定する。
    ///
    /// 既定は legacy 環境変数（`TSUMUGI_MAX_STEPS`）由来。process-global な env に
    /// 依存せず停止性を検証したいテストや、ホストが実行単位で予算を与える場合に使う。
    pub fn set_max_steps(&mut self, max_steps: u64) {
        self.budget.set_total_fuel(max_steps);
    }

    /// 関数値へ新しい FunctionId を発番する（AUD-048）。
    ///
    /// MakeClosure 実行のたびに呼ぶ。u64 を使い切った場合は internal error を返す
    /// （通常運用では到達不能）。
    fn allocate_function_id(&mut self, line: usize) -> Result<FunctionId, TsumugiError> {
        let id = self.next_function_id;
        self.next_function_id = self
            .next_function_id
            .checked_add(1)
            .ok_or_else(|| TsumugiError::internal(line, "FunctionId を割り当てできません"))?;
        Ok(FunctionId(id))
    }

    /// root script frame を除いた、現在 active な user 定義呼び出しフレーム数。
    ///
    /// VM は `run()` / `run_repl_chunk()` の実行中は常に 1 つの root frame を base に
    /// 持つ。深度上限（[`MAX_USER_CALL_DEPTH`]）はこの root を数えないため、判定・doc
    /// 契約はこの値だけを唯一の計数として用いる。tree-walk 版の `call_stack.len()` と
    /// 同じ意味になる。
    fn active_user_frame_count(&self) -> usize {
        self.frames.len().saturating_sub(1)
    }

    /// root source と import source を予算へ課金する（REV-015 Slice 2、§5.3）。
    ///
    /// `Link` フェーズの課金であり、`run` の前に呼ぶ。tree evaluator の
    /// `Evaluator::charge_link` と同じ規則で、root を 1 本の source として `charge_source`
    /// し、初めて解決した各 import module を `charge_source`（`source_bytes`）と
    /// `charge_import`（`import_bytes`）の両方へ課金する。超過は catch 不能 terminal
    /// として既存 error へ写像する。両 engine で観測挙動を一致させる。
    pub fn charge_link(
        &mut self,
        root_source_bytes: u64,
        loaded: &[crate::module::LoadedModule],
    ) -> Result<Vec<(std::path::PathBuf, Rc<crate::value::HeapToken>)>, TsumugiError> {
        // live heap は tracked backing の生成/drop で逐次維持する（REV-015 案A/PR-b/PR-c）。
        // collection・String・cell・関数 instance header はすべて per-drop 追跡されるため、
        // 同じ台帳を跨ぐ実行では pre-existing な live heap が自動的に持ち越され、Link 境界で
        // baseline を再走査する必要がない（再走査すると tracked 分を二重計上する）。よって
        // baseline 再課金は行わない（tree engine の `Evaluator::charge_link` と対称）。
        // `charge_context_baseline` は fresh ledger モデルの埋め込み API 用に残す。
        //
        // imported module record の live heap token は呼び出し側（loader 所有者）が
        // `loaded` set と寿命を揃えて保持できるよう戻り値で返す（REV-015 PR-d）。
        let mut record_tokens: Vec<(std::path::PathBuf, Rc<crate::value::HeapToken>)> = Vec::new();
        self.budget
            .charge_source(root_source_bytes, ExecutionPhase::Link)
            .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, 0))?;
        for module in loaded {
            self.budget
                .charge_source(module.byte_len, ExecutionPhase::Link)
                .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, module.line))?;
            self.budget
                .charge_import(module.byte_len, ExecutionPhase::Link)
                .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, module.line))?;
            // imported module record を live heap へ課金する（REV-015 PR-d）。tree engine
            // の `Evaluator::charge_link` と同じ規則（canonical path の UTF-8 byte 長）で
            // 課金し、token を呼び出し側（`ModuleLoader` を所有する main.rs）へ返して
            // `loaded` set と寿命を揃えて保持させる。VM は loader を所有しないため、
            // tree engine のように内部で登録できない。
            let module_id_bytes = module.path.as_os_str().len() as u64;
            let token = Value::new_heap_token(
                crate::budget::heap_size::imported_module_record(module_id_bytes),
                &self.budget.heap_handle(),
                ExecutionPhase::Link,
            )
            .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, module.line))?;
            record_tokens.push((module.path.clone(), token));
        }
        Ok(record_tokens)
    }

    /// bytecode chunk 木（`chunk` を root とし、全 prototype chunk を transitive に含む）の
    /// §5.1 bytecode chunk を live heap へ課金し、トークンを返す（REV-015 PR-d）。
    ///
    /// 各 distinct chunk を 1 回ずつ課金する（§5.2）。prototype の `Rc<Chunk>` は
    /// `MakeClosure` で `VmFn` へ clone 共有されるが、それは同じ backing の共有なので
    /// 追加課金しない。charge は `Compile` フェーズ相当（adopt 時）に行う。再帰は
    /// prototype 木の深さに比例するが、深さは compile 時の関数ネストで有限。
    fn charge_chunk_tree(
        &mut self,
        chunk: &Chunk,
    ) -> Result<Vec<Rc<crate::value::HeapToken>>, TsumugiError> {
        let mut tokens = Vec::new();
        self.charge_chunk_tree_into(chunk, &mut tokens)?;
        Ok(tokens)
    }

    fn charge_chunk_tree_into(
        &mut self,
        chunk: &Chunk,
        tokens: &mut Vec<Rc<crate::value::HeapToken>>,
    ) -> Result<(), TsumugiError> {
        let bytes = crate::budget::heap_size::bytecode_chunk(
            chunk.code.len() as u64,
            chunk.constants.len() as u64,
        );
        let token =
            Value::new_heap_token(bytes, &self.budget.heap_handle(), ExecutionPhase::Compile)
                .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, 0))?;
        tokens.push(token);
        for prototype in &chunk.prototypes {
            self.charge_chunk_tree_into(&prototype.chunk, tokens)?;
        }
        Ok(())
    }

    /// チャンクを実行する
    pub fn run(&mut self) -> Result<(), TsumugiError> {
        // adopt 済み root chunk 木を live heap へ課金する（REV-015 PR-d）。root frame は
        // `Vm::new` で設定済みなので、その chunk を辿る。token は execution の寿命で保持し
        // VM drop で release する。
        if self.chunk_tokens.is_empty()
            && let Some(root) = self.frames.first()
        {
            let root_chunk = Rc::clone(&root.chunk);
            let tokens = self.charge_chunk_tree(&root_chunk)?;
            self.chunk_tokens = tokens;
        }
        self.run_frames(0)?;
        Ok(())
    }

    /// REPL 用: 既存のスタック（ローカル変数）を保持したまま新しいチャンクを実行する。
    /// 前回のフレームを差し替えて実行し、終了後もスタック上の値を保持する。
    pub fn run_repl_chunk(&mut self, chunk: VerifiedChunk) -> Result<(), TsumugiError> {
        let chunk = chunk.into_inner();
        // 未捕捉エラー時に、入力途中の一時値・callee frame・try handlerを
        // 次の入力へ持ち越さないための構造状態checkpoint。
        // stackはList/Dictを深く複製せず、既存slotの書換・削除時だけjournalへ退避する。
        let frames_checkpoint = self.frames.clone();
        let globals_checkpoint = self.globals.clone();
        let handlers_checkpoint = self.try_handlers.clone();
        let steps_checkpoint = self.budget.committed_fuel();
        debug_assert!(self.repl_stack_checkpoint.is_none());
        // 新しい transaction 開始時に保留 heap 超過をクリアする（REV-015 PR-d）。
        self.pending_heap_stop = None;
        self.repl_stack_checkpoint = Some(ReplStackCheckpoint {
            stack_len: self.stack.len(),
            originals: HashMap::new(),
            cell_originals: HashMap::new(),
            entry_tokens: Vec::new(),
        });

        // top-levelでcell化された変数は入力間でも同じcellを使う。
        // これを空にすると既存closureとtop-level変数の参照先が分離する。
        let locals_cells = self
            .frames
            .first()
            .map(|frame| frame.locals_cells.clone())
            .unwrap_or_default();
        let root_chunk = Rc::new(chunk);
        let frame = CallFrame {
            chunk: Rc::clone(&root_chunk),
            ip: 0,
            base: 0,
            upvalues: Vec::new(),
            locals_cells,
        };
        if self.frames.is_empty() {
            self.frames.push(frame);
        } else {
            // 正常時は常にtop-level frameだけだが、防御的に古いcalleeを除去する。
            self.frames.truncate(1);
            self.frames[0] = frame;
        }
        // fuel 予算はリセット（各入力で予算を全額使えるように）
        self.budget.reset_fuel();

        // 入力の bytecode chunk 木を live heap へ課金する（REV-015 PR-d）。REPL の
        // top-level chunk は入力完了後に言語状態へ retain されない（結果 binding だけが
        // 残る）ため、入力単位で保持・release する。前回入力の chunk token を先に drop
        // （release）してから今回ぶんを課金し、両者を同時に live 計上して二重課金しない。
        // 持続 closure が prototype chunk を retain するケースは VM の既知 heap parity
        // gap（第14節 Slice 6）として扱う。
        self.chunk_tokens.clear();
        let chunk_tokens = match self.charge_chunk_tree(&root_chunk) {
            Ok(tokens) => tokens,
            Err(error) => {
                // 課金超過なら 1 命令も実行せず、checkpoint を破棄して state を巻き戻す。
                self.repl_stack_checkpoint = None;
                self.frames = frames_checkpoint;
                self.globals = globals_checkpoint;
                self.try_handlers = handlers_checkpoint;
                self.budget.restore_fuel(steps_checkpoint);
                return Err(error);
            }
        };
        self.chunk_tokens = chunk_tokens;

        match self.run_frames(0) {
            Ok(_) => {
                self.repl_stack_checkpoint = None;
                Ok(())
            }
            Err(error) => {
                let stack_checkpoint = self.repl_stack_checkpoint.take();
                self.frames = frames_checkpoint;
                if let Some(stack_checkpoint) = stack_checkpoint {
                    self.restore_repl_stack(stack_checkpoint);
                }
                self.globals = globals_checkpoint;
                self.try_handlers = handlers_checkpoint;
                self.budget.restore_fuel(steps_checkpoint);
                Err(error)
            }
        }
    }

    /// フレーム実行ループ（共通エンジン）
    ///
    /// `stop_depth` より深いフレームを実行し、`ReturnValue` で `stop_depth` まで
    /// 戻ったら戻り値を返す。トップレベル（`stop_depth == 0`）で命令が尽きた場合は
    /// `Value::Null` を返す。
    ///
    /// try/catch ハンドラもこのループ内で処理するため、map/filter/each 経由で
    /// 呼ばれた関数内の try/catch も正しく動作する。
    fn run_frames(&mut self, stop_depth: usize) -> Result<Value, TsumugiError> {
        loop {
            let frame = self.frame(0)?;
            if frame.ip >= frame.chunk.code.len() {
                // フレームの命令が尽きた = 暗黙 null return
                if self.frames.len() <= stop_depth + 1 {
                    // トップレベルまたは stop_depth に戻った
                    break;
                }
                // ネストされた関数が暗黙 null return で終わった場合
                let f = self.take_frame(0)?;
                self.truncate_stack(f.base);
                // 暗黙 return 時にこのフレーム内の try ハンドラを除去する
                self.try_handlers
                    .retain(|h| h.frame_depth <= self.frames.len());
                self.stack.push(Value::Null);
                continue;
            }

            let instruction = frame
                .chunk
                .code
                .get(frame.ip)
                .cloned()
                .ok_or_else(|| internal_error(0, "命令の参照が不正です"))?;
            // 行番号表が命令列と対応していないChunkでもpanicさせない
            let line = frame
                .chunk
                .lines
                .get(frame.ip)
                .copied()
                .ok_or_else(|| internal_error(0, "命令に対応する行番号がありません"))?;
            self.frame_mut(line)?.ip += 1;

            // per-instruction step 課金（REV-006 層1）。
            // fetch した全命令の dispatch 直前に無条件で 1 step 課金する。これにより、
            // 検証を通っていない chunk が防御的に届いても、任意の命令列は有限 step で
            // `limit` error になる（`Jump(0)` 自己ループも 1 周ごとに課金される）。
            // step 上限到達エラーは、既存挙動どおり try/catch handler 経路を通す。
            let result = match self.count_step(line) {
                Err(step_error) => Err(step_error),
                Ok(()) => match &instruction {
                    OpCode::ReturnValue => {
                        let return_value = self.pop(line)?;
                        let frame = self.take_frame(line)?;
                        self.truncate_stack(frame.base);
                        // return 時にこのフレーム内の try ハンドラを除去する
                        let current_depth = self.frames.len();
                        self.try_handlers.retain(|h| h.frame_depth <= current_depth);
                        if current_depth <= stop_depth {
                            return Ok(return_value);
                        }
                        self.stack.push(return_value);
                        Ok(())
                    }
                    OpCode::Return => {
                        if self.frames.len() <= stop_depth + 1 {
                            return Ok(Value::Null);
                        }
                        // ネストされたフレーム内の Return（通常は起きないがガード）
                        let f = self.take_frame(line)?;
                        self.truncate_stack(f.base);
                        // return 時にこのフレーム内の try ハンドラを除去する
                        self.try_handlers
                            .retain(|h| h.frame_depth <= self.frames.len());
                        self.stack.push(Value::Null);
                        Ok(())
                    }
                    OpCode::SetupTry(catch_ip) => {
                        let catch_ip = *catch_ip;
                        let locals_count = self
                            .frames
                            .last()
                            .map(|frame| self.stack.len().saturating_sub(frame.base))
                            .unwrap_or(0);
                        self.try_handlers.push(TryHandler {
                            catch_ip,
                            stack_depth: self.stack.len(),
                            frame_depth: self.frames.len(),
                            locals_count,
                        });
                        Ok(())
                    }
                    OpCode::TeardownTry => {
                        self.try_handlers.pop();
                        Ok(())
                    }
                    _ => self.dispatch(instruction, line),
                },
            };

            if let Err(e) = result {
                // exit() の structured terminal 信号（C7、REV-023）は script から catch でき
                // ない。try handler を探さずそのまま伝播し、CLI が Exited terminal へ写す。
                if matches!(e.kind(), Some(crate::error::ErrorKind::ProcessExit)) {
                    return Err(self.attach_trace(e));
                }
                if let Some(handler) = self.try_handlers.pop() {
                    // try ハンドラが stop_depth より深い場合のみ処理する
                    // (stop_depth 以下のハンドラは呼び出し元の管轄)
                    if handler.frame_depth > stop_depth {
                        // フレームを巻き戻す
                        self.frames.truncate(handler.frame_depth);
                        // try開始時から有効だったslotのcell昇格は維持し、try内で
                        // 追加されたlocalのcell対応だけを破棄してcatch slotとの衝突を防ぐ。
                        if let Some(frame) = self.frames.last_mut() {
                            frame.locals_cells.truncate(handler.locals_count);
                        }
                        // スタックを巻き戻す
                        self.truncate_stack(handler.stack_depth);
                        // 構造化エラーをスタックに積む
                        let error_value = Value::Error {
                            error_type: e.error_type().to_string(),
                            message: e.message().to_string(),
                            line: e.line(),
                        };
                        self.stack.push(error_value);
                        // catch ブロックへジャンプ
                        self.set_ip(handler.catch_ip, line)?;
                    } else {
                        // このハンドラは呼び出し元のもの → 戻してからエラーを伝播
                        self.try_handlers.push(handler);
                        return Err(self.attach_trace(e));
                    }
                } else {
                    return Err(self.attach_trace(e));
                }
            }
        }
        Ok(Value::Null)
    }

    /// ステップ（fuel）を 1 課金し、上限チェックする（REV-015 Slice 1）。
    fn count_step(&mut self, line: usize) -> Result<(), TsumugiError> {
        // 直前の checkpoint 記録で rollback journal entry の heap 課金が上限を超えていた
        // 場合、ここで surface する（REV-015 PR-d）。既定上限では発生しない。
        if let Some(stop) = self.pending_heap_stop.take() {
            return Err(Self::control_stop_to_error(&self.budget, stop, line));
        }
        self.budget
            .charge_fuel(1, ExecutionPhase::Run)
            .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))
    }

    /// collection 要素数の per-item 検査（REV-015 Slice 1）。
    fn check_collection(&mut self, size: usize, line: usize) -> Result<(), TsumugiError> {
        self.budget
            .check_collection_elements(size as u64, ExecutionPhase::Run)
            .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))
    }

    /// budget の [`ControlStop`] を既存の [`TsumugiError`] へ写像する（Slice 1/2 互換）。
    /// 実体は tree/VM 共有の [`crate::budget::control_stop_to_error`]。
    ///
    /// Slice 1/2 で発生し得るのは fuel（= step）・collection・string 超過。cancel /
    /// deadline は ledger の charge 経路にまだ配線しておらず（Slice 4）、
    /// 到達した場合も安全側で step 上限として扱う。
    fn control_stop_to_error(
        budget: &BudgetLedger,
        stop: ControlStop,
        line: usize,
    ) -> TsumugiError {
        crate::budget::control_stop_to_error(stop, budget.usage().committed.fuel, line)
    }

    // --- 内部不変条件の検査（AUD-023） ---
    //
    // 公開APIは `Vm::new` / `Vm::run_repl_chunk` で任意の `Chunk` を受け取れるため、
    // compiler が生成しない命令列でも panic せず internal error を返す。

    /// 実行中のframeを取得する
    fn frame(&self, line: usize) -> Result<&CallFrame, TsumugiError> {
        self.frames
            .last()
            .ok_or_else(|| internal_error(line, "実行中のframeがありません"))
    }

    /// 実行中のframeを可変で取得する
    fn frame_mut(&mut self, line: usize) -> Result<&mut CallFrame, TsumugiError> {
        self.frames
            .last_mut()
            .ok_or_else(|| internal_error(line, "実行中のframeがありません"))
    }

    /// 現在のframeを取り出す（return・暗黙returnでの復帰用）
    fn take_frame(&mut self, line: usize) -> Result<CallFrame, TsumugiError> {
        self.frames
            .pop()
            .ok_or_else(|| internal_error(line, "戻り先のframeがありません"))
    }

    /// 命令ポインタを移動する
    fn set_ip(&mut self, target: usize, line: usize) -> Result<(), TsumugiError> {
        self.frame_mut(line)?.ip = target;
        Ok(())
    }

    /// 定数表から値を取り出す
    fn constant(&self, index: usize, line: usize) -> Result<Value, TsumugiError> {
        self.frame(line)?
            .chunk
            .constants
            .get(index)
            .cloned()
            .ok_or_else(|| internal_error(line, format!("定数表の参照が不正です: {}", index)))
    }

    /// upvalueのセルを取り出す
    fn upvalue_cell(&self, index: usize, line: usize) -> Result<SharedValue, TsumugiError> {
        self.frame(line)?
            .upvalues
            .get(index)
            .map(Rc::clone)
            .ok_or_else(|| internal_error(line, format!("upvalueの参照が不正です: {}", index)))
    }

    /// local slotに対応するstack位置を検査付きで求める
    fn local_stack_index(
        &self,
        base: usize,
        slot: usize,
        line: usize,
    ) -> Result<usize, TsumugiError> {
        let at = base
            .checked_add(slot)
            .ok_or_else(|| internal_error(line, "local slotの計算がオーバーフローしました"))?;
        if at >= self.stack.len() {
            return Err(internal_error(
                line,
                format!("local slotが不正です: {}", slot),
            ));
        }
        Ok(at)
    }

    /// stackから取り出す個数が足りているか検査する（大きすぎるoperandの確保も防ぐ）
    fn require_stack_len(&self, count: usize, line: usize) -> Result<(), TsumugiError> {
        if count > self.stack.len() {
            return Err(internal_error(
                line,
                format!(
                    "スタックの要素数が不足しています (要求: {}, 実際: {})",
                    count,
                    self.stack.len()
                ),
            ));
        }
        Ok(())
    }

    /// エラーにスタックトレース情報を付加する
    fn attach_trace(&self, error: TsumugiError) -> TsumugiError {
        use crate::error::TraceFrame;

        if self.frames.len() <= 1 {
            return error;
        }

        let mut trace = Vec::new();
        for i in (0..self.frames.len() - 1).rev() {
            let caller = &self.frames[i];
            let callee = &self.frames[i + 1];
            // trace生成はエラー整形の途中なので、行番号が引けない場合も panic させない
            let call_line = caller
                .ip
                .checked_sub(1)
                .and_then(|at| caller.chunk.lines.get(at).copied())
                .unwrap_or(1);
            trace.push(TraceFrame {
                name: callee.chunk.name.clone(),
                line: call_line,
            });
        }

        error.with_trace(trace)
    }

    /// ローカル変数を読み取る（セル経由の場合はセルから読む）
    fn get_local(&self, slot: usize, line: usize) -> Result<Value, TsumugiError> {
        let frame = self.frame(line)?;
        // locals_cells にセルがあればそこから読む
        if let Some(Some(cell)) = frame.locals_cells.get(slot) {
            return Ok(cell.borrow().clone());
        }
        // 通常のスタック読み取り
        let at = self.local_stack_index(frame.base, slot, line)?;
        Ok(self.stack[at].clone())
    }

    /// ローカル変数を参照のまま読む。
    ///
    /// `get_local` は値を複製するため、コレクションでは要素数に比例したコストになる。
    /// 長さ取得やインデックスアクセスのように結果だけが必要な場合はこちらを使う。
    fn with_local_ref<R>(
        &self,
        slot: usize,
        line: usize,
        read: impl FnOnce(&Value) -> Result<R, TsumugiError>,
    ) -> Result<R, TsumugiError> {
        let frame = self.frames.last().ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                "local参照用のframeがありません",
            )
        })?;
        // cell化済みならcell、未cell化ならstack slotをそのまま参照する
        if let Some(Some(cell)) = frame.locals_cells.get(slot) {
            let cell = Rc::clone(cell);
            return read(&cell.borrow());
        }
        let at = frame.base.checked_add(slot).ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                "local slotの計算がオーバーフローしました",
            )
        })?;
        let value = self.stack.get(at).ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                "local slotが不正です",
            )
        })?;
        read(value)
    }

    /// ローカル変数を書き込む（セル経由の場合はセルに書く）
    fn set_local(&mut self, slot: usize, value: Value, line: usize) -> Result<(), TsumugiError> {
        let base = {
            let frame = self.frame(line)?;
            // locals_cells にセルがあればそこに書く
            if let Some(Some(cell)) = frame.locals_cells.get(slot) {
                let cell = Rc::clone(cell);
                self.checkpoint_cell(&cell);
                *cell.borrow_mut() = value;
                return Ok(());
            }
            frame.base
        };
        let at = self.local_stack_index(base, slot, line)?;
        self.checkpoint_stack_slot(at);
        self.stack[at] = value;
        Ok(())
    }

    /// ローカル変数をキャプチャ用セルに昇格させる
    /// 既にセルがあればそれを返す。なければスタックの値からセルを作成し、登録して返す
    fn ensure_local_cell(&mut self, slot: usize, line: usize) -> Result<SharedValue, TsumugiError> {
        let base = self.frame(line)?.base;
        let at = self.local_stack_index(base, slot, line)?;
        {
            let frame = self.frame_mut(line)?;
            // locals_cells を必要なサイズに拡張
            while frame.locals_cells.len() <= slot {
                frame.locals_cells.push(None);
            }
            if let Some(Some(cell)) = frame.locals_cells.get(slot) {
                return Ok(Rc::clone(cell));
            }
        }
        // スタックから現在の値を取り出してセルを作成する。§5.1 captured cell（32 byte）を
        // live heap へ課金し、最後の参照 drop で release する（REV-015 PR-c）。
        let value = self.stack[at].clone();
        let cell =
            crate::value::Value::new_cell(value, &self.budget.heap_handle(), ExecutionPhase::Run)
                .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
        if let Some(entry) = self.frame_mut(line)?.locals_cells.get_mut(slot) {
            *entry = Some(Rc::clone(&cell));
        }
        Ok(cell)
    }

    /// runtime globalを読み取る。registryはtop-level slotだけを保持し、
    /// cell化済みなら同じSharedValue、未cell化なら同じstack slotから値を得る。
    fn get_global(&self, name: &str, line: usize) -> Result<Value, TsumugiError> {
        let slot = self
            .globals
            .get(name)
            .copied()
            .ok_or_else(|| TsumugiError::undefined_name(line, name))?;
        let frame = self.frames.first().ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                "global参照用のtop-level frameがありません",
            )
        })?;
        if let Some(Some(cell)) = frame.locals_cells.get(slot) {
            return Ok(cell.borrow().clone());
        }
        let stack_index = frame.base.checked_add(slot).ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                "global slotの計算がオーバーフローしました",
            )
        })?;
        self.stack.get(stack_index).cloned().ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                format!("global registryのslotが不正です: {}", name),
            )
        })
    }

    /// runtime globalが定義済みかだけを検査する（値は読み出さない）。
    /// 破壊的更新の対象bindingを、他の被演算子の評価前に検証するために使う。
    fn require_global(&self, name: &str, line: usize) -> Result<(), TsumugiError> {
        if self.globals.contains_key(name) {
            return Ok(());
        }
        Err(TsumugiError::undefined_name(line, name))
    }

    /// binding の値が置かれている場所を解決する（値は複製しない）。
    fn resolve_binding_storage(
        &self,
        target: &MutationTarget,
        line: usize,
    ) -> Result<BindingStorage, TsumugiError> {
        match target {
            MutationTarget::Local(slot) => {
                let frame = self.frames.last().ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Internal,
                        "local参照用のframeがありません",
                    )
                })?;
                if let Some(Some(cell)) = frame.locals_cells.get(*slot) {
                    return Ok(BindingStorage::Cell(Rc::clone(cell)));
                }
                let stack_index = frame.base.checked_add(*slot).ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Internal,
                        "local slotの計算がオーバーフローしました",
                    )
                })?;
                Ok(BindingStorage::Stack(stack_index))
            }
            MutationTarget::Upvalue(index) => {
                let frame = self.frames.last().ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Internal,
                        "upvalue参照用のframeがありません",
                    )
                })?;
                let cell = frame.upvalues.get(*index).ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Internal,
                        format!("upvalue indexが不正です: {}", index),
                    )
                })?;
                Ok(BindingStorage::Cell(Rc::clone(cell)))
            }
            MutationTarget::Global(name) => {
                let slot = self
                    .globals
                    .get(name)
                    .copied()
                    .ok_or_else(|| TsumugiError::undefined_name(line, name))?;
                let frame = self.frames.first().ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Internal,
                        "global参照用のtop-level frameがありません",
                    )
                })?;
                if let Some(Some(cell)) = frame.locals_cells.get(slot) {
                    return Ok(BindingStorage::Cell(Rc::clone(cell)));
                }
                let stack_index = frame.base.checked_add(slot).ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Internal,
                        "global slotの計算がオーバーフローしました",
                    )
                })?;
                Ok(BindingStorage::Stack(stack_index))
            }
        }
    }

    /// runtime globalを更新する。既存cellがあればcell、なければtop-level stackへ書く。
    fn set_global(&mut self, name: &str, value: Value, line: usize) -> Result<(), TsumugiError> {
        let slot = self
            .globals
            .get(name)
            .copied()
            .ok_or_else(|| TsumugiError::assign_undefined(line, name))?;
        let (base, cell) = {
            let frame = self.frames.first().ok_or_else(|| {
                TsumugiError::runtime_with_kind(
                    line,
                    crate::error::ErrorKind::Internal,
                    "global更新用のtop-level frameがありません",
                )
            })?;
            (
                frame.base,
                frame.locals_cells.get(slot).and_then(|entry| entry.clone()),
            )
        };
        if let Some(cell) = cell {
            self.checkpoint_cell(&cell);
            *cell.borrow_mut() = value;
            return Ok(());
        }
        let stack_index = base.checked_add(slot).ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                "global slotの計算がオーバーフローしました",
            )
        })?;
        self.checkpoint_stack_slot(stack_index);
        let target = self.stack.get_mut(stack_index).ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                format!("global registryのslotが不正です: {}", name),
            )
        })?;
        *target = value;
        Ok(())
    }

    /// 実行済みtop-level宣言をglobal registryへ公開する。
    fn register_global(
        &mut self,
        name: String,
        slot: usize,
        line: usize,
    ) -> Result<(), TsumugiError> {
        if self.frames.len() != 1 {
            return Err(TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                "関数frameからglobalを登録しようとしました",
            ));
        }
        let frame = self.frames.first().ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                "global登録用のtop-level frameがありません",
            )
        })?;
        let stack_index = frame.base.checked_add(slot).ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                "global slotの計算がオーバーフローしました",
            )
        })?;
        if stack_index >= self.stack.len() {
            return Err(TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Internal,
                format!("global登録対象のslotが不正です: {}", name),
            ));
        }
        self.globals.insert(name, slot);
        Ok(())
    }

    /// 命令をディスパッチ（ReturnValue / Return 以外）
    fn dispatch(&mut self, instruction: OpCode, line: usize) -> Result<(), TsumugiError> {
        match instruction {
            OpCode::LoadConst(idx) => {
                let value = self.constant(idx, line)?;
                // 文字列リテラルは dispatch を経由しないため、materialize 時に課金する
                // （REV-015 Slice 2）。定数表の String は untracked なので track_result が
                // cumulative string accounting と live heap（new_str）へ昇格させる。Int /
                // Float / Bool や空 collection 定数は untracked のまま素通しする（後者の
                // 課金差は Slice 6 の VM charge parity で扱う既知差）。
                let value = if matches!(value, Value::Str(_)) {
                    self.budget
                        .track_result(value, ExecutionPhase::Run)
                        .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?
                } else {
                    value
                };
                self.stack.push(value);
            }
            OpCode::Add => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                let result = self.binary_add(left, right, line)?;
                // 文字列結合（`+`）が新規生成する String body を課金する
                // （REV-015 Slice 2）。track_result は untracked な String だけを昇格させ、
                // 数値など他の結果はそのまま返す。
                let result = self
                    .budget
                    .track_result(result, ExecutionPhase::Run)
                    .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
                self.stack.push(result);
            }
            OpCode::Sub => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                let result = self.binary_sub(left, right, line)?;
                self.stack.push(result);
            }
            OpCode::Mul => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                let result = self.binary_mul(left, right, line)?;
                self.stack.push(result);
            }
            OpCode::Div => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                let result = self.binary_div(left, right, line)?;
                self.stack.push(result);
            }
            OpCode::Mod => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                let result = self.binary_mod(left, right, line)?;
                self.stack.push(result);
            }
            OpCode::Eq => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                self.stack.push(Value::Bool(left == right));
            }
            OpCode::NotEq => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                self.stack.push(Value::Bool(left != right));
            }
            OpCode::Lt => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                let result = self.compare_lt(left, right, line)?;
                self.stack.push(result);
            }
            OpCode::Gt => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                let result = self.compare_gt(left, right, line)?;
                self.stack.push(result);
            }
            OpCode::LtEq => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                let result = self.compare_lteq(left, right, line)?;
                self.stack.push(result);
            }
            OpCode::GtEq => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                let result = self.compare_gteq(left, right, line)?;
                self.stack.push(result);
            }
            OpCode::Not => {
                let value = self.pop(line)?;
                self.stack.push(Value::Bool(!value.is_truthy()));
            }
            OpCode::Negate => {
                let value = self.pop(line)?;
                let result = match value {
                    Value::Int(n) => n
                        .checked_neg()
                        .map(Value::Int)
                        .ok_or_else(|| TsumugiError::int_overflow(line, "符号反転"))?,
                    Value::Float(n) => Value::Float(-n),
                    other => {
                        return Err(TsumugiError::unary_type(
                            line,
                            crate::ast::UnaryOpKind::Neg,
                            &other,
                        ));
                    }
                };
                self.stack.push(result);
            }
            OpCode::GetLocal(slot) => {
                let value = self.get_local(slot, line)?;
                self.stack.push(value);
            }
            OpCode::SetLocal(slot) => {
                let value = self
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| internal_error(line, "内部エラー: スタックが空です"))?;
                self.set_local(slot, value, line)?;
            }
            OpCode::GetGlobal(name) => {
                let value = self.get_global(&name, line)?;
                self.stack.push(value);
            }
            OpCode::GetGlobalForCall(name) => {
                let value = self.get_global(&name, line)?;
                self.stack.push(value);
            }
            OpCode::SetGlobal(name) => {
                let value = self.stack.last().cloned().ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Internal,
                        "SetGlobalの値がスタックにありません",
                    )
                })?;
                self.set_global(&name, value, line)?;
            }
            OpCode::RegisterGlobal(name, slot) => {
                self.register_global(name, slot, line)?;
            }
            OpCode::JumpIfGlobalDefined(name, target) => {
                if self.globals.contains_key(&name) {
                    self.set_ip(target, line)?;
                }
            }
            OpCode::RequireGlobal(name) => {
                self.require_global(&name, line)?;
            }
            OpCode::Jump(target) => {
                self.set_ip(target, line)?;
            }
            OpCode::JumpIfFalse(target) => {
                let value = self.pop(line)?;
                if !value.is_truthy() {
                    self.set_ip(target, line)?;
                }
            }
            OpCode::JumpIfFalseKeep(target) => {
                let value = self.stack.last().ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Internal,
                        "スタックが空です",
                    )
                })?;
                if !value.is_truthy() {
                    self.set_ip(target, line)?;
                }
            }
            OpCode::JumpIfTrueKeep(target) => {
                let value = self.stack.last().ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Internal,
                        "スタックが空です",
                    )
                })?;
                if value.is_truthy() {
                    self.set_ip(target, line)?;
                }
            }
            OpCode::Loop(target) => {
                // step 課金はメインループの per-instruction 課金へ一本化した（REV-006）。
                self.set_ip(target, line)?;
            }
            OpCode::GetUpvalue(index) => {
                let value = self.upvalue_cell(index, line)?.borrow().clone();
                self.stack.push(value);
            }
            OpCode::SetUpvalue(index) => {
                let value = self
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| internal_error(line, "内部エラー: スタックが空です"))?;
                let cell = self.upvalue_cell(index, line)?;
                self.checkpoint_cell(&cell);
                *cell.borrow_mut() = value;
            }
            OpCode::MakeClosure(proto_index) => {
                // capture は隣接 opcode 列ではなくプロトタイプの明示記述子から解釈する（REV-005）。
                // operand はプロトタイプ index。範囲は verifier 済みだが、防御的に再検査する。
                let frame = self.frame(line)?;
                let prototype = frame
                    .chunk
                    .prototypes
                    .get(proto_index)
                    .cloned()
                    .ok_or_else(|| {
                        internal_error(line, "MakeClosure のプロトタイプ index が範囲外です")
                    })?;

                // 各 capture 記述子をセルへ解決する。Null フォールバックは持たない。
                let mut upvalue_cells = Vec::with_capacity(prototype.captures.len());
                for cap in &prototype.captures {
                    let cell = match cap {
                        CaptureDesc::Local(slot) => self.ensure_local_cell(*slot, line)?,
                        CaptureDesc::Upvalue(index) => self.upvalue_cell(*index, line)?,
                    };
                    upvalue_cells.push(cell);
                }

                // 関数式の評価ごとに一意な FunctionId を発番する（AUD-048）。
                let id = self.allocate_function_id(line)?;
                // VM function instance header（§5.1 vm_function = 48 + 16×upvalue）を
                // live heap へ課金する（REV-015 PR-c）。upvalue cell 実体は
                // ensure_local_cell / upvalue_cell 側で課金済みなので header ぶんだけ。
                let header = crate::value::Value::new_vm_fn_header(
                    upvalue_cells.len() as u64,
                    &self.budget.heap_handle(),
                    ExecutionPhase::Run,
                )
                .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
                self.stack.push(Value::VmFn {
                    id,
                    name: prototype.name.clone(),
                    arity: prototype.arity,
                    params: prototype.params.clone(),
                    chunk: prototype.chunk.clone(),
                    upvalues: upvalue_cells,
                    header,
                });
            }
            OpCode::PrepareCall => {
                // step 課金は per-instruction 課金へ一本化した（REV-006）。深度上限は
                // PrepareCall と Call の両方で検査する（raw Call でも迂回させない）。
                if self.active_user_frame_count() >= MAX_USER_CALL_DEPTH {
                    return Err(TsumugiError::call_depth_limit(line, MAX_USER_CALL_DEPTH));
                }
            }
            OpCode::ValidateCall(arg_count) => {
                let fn_value = self
                    .stack
                    .last()
                    .ok_or_else(|| internal_error(line, "ValidateCall のcalleeがありません"))?;
                if let Value::VmFn { name, arity, .. } = fn_value {
                    if arg_count != *arity {
                        return Err(TsumugiError::user_arity(line, name, *arity, arg_count));
                    }
                } else {
                    return Err(TsumugiError::not_callable(line, fn_value));
                }
            }
            OpCode::Call(arg_count) => {
                // PrepareCallを経由しない不正bytecodeでもframe上限を迂回させない。
                // stepはPrepareCallだけで数え、ここでは二重countしない。
                if self.active_user_frame_count() >= MAX_USER_CALL_DEPTH {
                    return Err(TsumugiError::call_depth_limit(line, MAX_USER_CALL_DEPTH));
                }
                let required = arg_count
                    .checked_add(1)
                    .ok_or_else(|| internal_error(line, "Call の引数数が不正です"))?;
                let fn_pos =
                    self.stack.len().checked_sub(required).ok_or_else(|| {
                        internal_error(line, "Call のスタック要素が不足しています")
                    })?;
                let fn_value = self.stack[fn_pos].clone();
                if let Value::VmFn {
                    name,
                    arity,
                    chunk,
                    upvalues,
                    ..
                } = fn_value
                {
                    // ValidateCall後にcalleeが変化しないことを前提とするが、
                    // 不正bytecodeに対する防御として再検査する。
                    if arg_count != arity {
                        return Err(TsumugiError::user_arity(line, &name, arity, arg_count));
                    }
                    let base = fn_pos;
                    self.frames.push(CallFrame {
                        chunk,
                        ip: 0,
                        base,
                        upvalues,
                        locals_cells: Vec::new(),
                    });
                } else {
                    return Err(TsumugiError::not_callable(line, &fn_value));
                }
            }
            OpCode::Print(arg_count) => {
                self.require_stack_len(arg_count, line)?;
                let mut values = Vec::with_capacity(arg_count);
                for _ in 0..arg_count {
                    values.push(self.pop(line)?);
                }
                values.reverse();
                let output: Vec<String> = values.iter().map(|v| v.to_string()).collect();
                let payload = output.join(" ");
                // output（stdio host call）を課金する（§6.1、REV-015 Slice 2、I-O accounting）。
                // tree engine（builtin.rs の print）と同じ論理位置で payload を host へ渡す
                // 前に課金する。
                self.budget
                    .charge_output(payload.len() as u64, ExecutionPhase::Run)
                    .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
                crate::builtin_core::write_stdout_line(&payload, line)?;
            }
            OpCode::Pop => {
                // 単一 pop 時もセルをクリア
                let stack_top = self.stack.len();
                let frame = self.frame_mut(line)?;
                if let Some(slot) = stack_top
                    .checked_sub(1)
                    .and_then(|top| top.checked_sub(frame.base))
                    && slot < frame.locals_cells.len()
                {
                    frame.locals_cells[slot] = None;
                }
                self.pop(line)?;
            }
            OpCode::PopN(count) => {
                // スコープ終了: 対応する locals_cells をクリアしてからスタックを削除
                self.require_stack_len(count, line)?;
                let stack_top = self.stack.len();
                let frame = self.frame_mut(line)?;
                for i in 0..count {
                    if let Some(slot) = stack_top
                        .checked_sub(1)
                        .and_then(|top| top.checked_sub(i))
                        .and_then(|top| top.checked_sub(frame.base))
                        && slot < frame.locals_cells.len()
                    {
                        frame.locals_cells[slot] = None;
                    }
                }
                for _ in 0..count {
                    self.pop(line)?;
                }
            }
            OpCode::LenLocal(slot) => {
                // 判定とエラーは `len` builtin と共有し、engine間で差が出ないようにする
                let length = self.with_local_ref(slot, line, |value| {
                    crate::builtin_core::builtin_len(std::slice::from_ref(value), line)
                })?;
                self.stack.push(length);
            }
            OpCode::Index => {
                let index = self.pop(line)?;
                let collection = self.pop(line)?;
                let result = self.eval_index(&collection, &index, line)?;
                self.stack.push(result);
            }
            OpCode::IndexLocal(slot) => {
                let index = self.pop(line)?;
                let result = self.with_local_ref(slot, line, |collection| {
                    self.eval_index(collection, &index, line)
                })?;
                self.stack.push(result);
            }
            OpCode::ListPush => {
                let value = self.pop(line)?;
                let stack_index = self.stack.len().saturating_sub(1);
                self.checkpoint_stack_slot(stack_index);
                // 候補サイズを先に確定してから collection 検査する（self への
                // mutable borrow を stack 要素 borrow と両立させるため）。
                let candidate = match self.stack.last() {
                    Some(Value::List(v)) => v.len().saturating_add(1),
                    Some(_) => {
                        return Err(internal_error(
                            line,
                            "内部エラー: ListPush の対象がリストではありません",
                        ));
                    }
                    None => return Err(internal_error(line, "内部エラー: スタックが空です")),
                };
                self.check_collection(candidate, line)?;
                // stack 上のリテラル backing へ tracked push する（delta 課金、REV-015 案A）。
                // budget と stack の同時可変借用を避けるため、slot を一旦取り出して戻す。
                let idx = self.stack.len().saturating_sub(1);
                let mut collection = std::mem::replace(&mut self.stack[idx], Value::Null);
                let result =
                    collection.list_push_tracked(value, &mut self.budget, ExecutionPhase::Run);
                self.stack[idx] = collection;
                result.map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
            }
            OpCode::DictInsert => {
                let value = self.pop(line)?;
                let key = self.pop(line)?;
                let stack_index = self.stack.len().saturating_sub(1);
                self.checkpoint_stack_slot(stack_index);
                let k = match key {
                    Value::Str(k) => k.to_string(),
                    other => return Err(TsumugiError::dict_key_type(line, &other)),
                };
                // 新規 key のときだけ候補サイズを確定して collection 検査する。
                let candidate = match self.stack.last() {
                    Some(Value::Dict(map)) => {
                        if map.contains_key(&k) {
                            None
                        } else {
                            Some(map.len().saturating_add(1))
                        }
                    }
                    Some(_) => {
                        return Err(internal_error(
                            line,
                            "内部エラー: DictInsert の対象が辞書ではありません",
                        ));
                    }
                    None => return Err(internal_error(line, "内部エラー: スタックが空です")),
                };
                if let Some(candidate) = candidate {
                    self.check_collection(candidate, line)?;
                }
                // stack 上のリテラル backing へ tracked insert する（delta 課金）。
                let idx = self.stack.len().saturating_sub(1);
                let mut collection = std::mem::replace(&mut self.stack[idx], Value::Null);
                let result = collection.index_set_tracked(
                    crate::value::IndexTarget::DictKey(k),
                    value,
                    &mut self.budget,
                    ExecutionPhase::Run,
                );
                self.stack[idx] = collection;
                result.map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
            }
            OpCode::SetIndex(target) => {
                let value = self.pop(line)?;
                let index = self.pop(line)?;
                self.assign_index_binding(&target, &index, value, line)?;
            }
            OpCode::ToIterList => {
                let value = self.pop(line)?;
                let list = match value {
                    Value::List(ref values) => {
                        let size = values.len();
                        self.check_collection(size, line)?;
                        value
                    }
                    Value::Dict(ref map) => {
                        let size = map.len();
                        let keys: Vec<Value> =
                            map.keys().map(|k| Value::str_constant(k.clone())).collect();
                        self.check_collection(size, line)?;
                        Value::new_list(keys, &mut self.budget, ExecutionPhase::Run)
                            .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?
                    }
                    Value::Str(ref s) => {
                        let size = s.chars().count();
                        let chars: Vec<Value> = s
                            .chars()
                            .map(|c| Value::str_constant(c.to_string()))
                            .collect();
                        self.check_collection(size, line)?;
                        Value::new_list(chars, &mut self.budget, ExecutionPhase::Run)
                            .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?
                    }
                    _ => {
                        return Err(TsumugiError::not_iterable(line, &value));
                    }
                };
                self.stack.push(list);
            }
            OpCode::ValidateBuiltinCall(name, arg_count, first_arg_is_identifier) => {
                crate::builtin_core::validate_context_builtin_call(
                    &name,
                    arg_count,
                    first_arg_is_identifier,
                    line,
                )?;
            }
            OpCode::CallBuiltin(builtin_id, arg_count) => {
                let name = crate::builtin_registry::name_of(builtin_id);
                self.require_stack_len(arg_count, line)?;
                let mut args = Vec::with_capacity(arg_count);
                for _ in 0..arg_count {
                    args.push(self.pop(line)?);
                }
                args.reverse();
                let result = self.exec_builtin(name, args, line)?;
                self.stack.push(result);
            }
            OpCode::PopUpdate => {
                // pop の書き戻し専用の内部命令。スタックトップの List から末尾を
                // 取り除く（source から到達不能）。tracked backing 上で delta release
                // しながら in-place で更新する（REV-015 案A）。
                self.require_stack_len(1, line)?;
                let idx = self.stack.len().saturating_sub(1);
                self.checkpoint_stack_slot(idx);
                let mut collection = std::mem::replace(&mut self.stack[idx], Value::Null);
                let result = collection.list_pop_tracked(&mut self.budget, ExecutionPhase::Run);
                self.stack[idx] = collection;
                result.map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
            }
            OpCode::FStrConcat(count) => {
                // スタックから count 個の値を取り出して文字列に連結
                self.require_stack_len(count, line)?;
                let start = self.stack.len() - count;
                self.checkpoint_stack_range(start, self.stack.len());
                let parts: Vec<Value> = self.stack.drain(start..).collect();
                let mut result = String::new();
                for val in parts {
                    result.push_str(&val.to_string());
                }
                // f-string の生成 body も dispatch を経由しないため課金する（REV-015 Slice 2）。
                let value = self
                    .budget
                    .track_result(Value::str_constant(result), ExecutionPhase::Run)
                    .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
                self.stack.push(value);
            }
            OpCode::ReturnValue | OpCode::Return => {
                // 通常は run_frames() が処理する。不正な呼び出しでもpanicさせない。
                return Err(internal_error(
                    line,
                    "内部エラー: return命令がdispatchへ到達しました",
                ));
            }
            OpCode::SetupTry(_) | OpCode::TeardownTry => {
                // 通常は run_frames() が処理する。不正な呼び出しでもpanicさせない。
                return Err(internal_error(
                    line,
                    "内部エラー: try命令がdispatchへ到達しました",
                ));
            }
        }
        Ok(())
    }

    /// rollback journal entry 1 件の §5.1 論理サイズ（固定 overhead 48 byte）を live heap
    /// へ課金するトークンを作る（REV-015 PR-d）。
    ///
    /// checkpoint 記録は `pop` など多数の infallible 経路から呼ばれるため、その場で
    /// `Result` を返さず、超過時は `pending_heap_stop` へ退避して次の per-instruction
    /// step 課金境界（[`Vm::count_step`]）で surface する。保持する旧 value の到達
    /// payload（List/Dict/String body）は shared backing の `Tracked` が entry の握る
    /// `Rc` で生き続けるため既に live heap に計上されており、ここで再課金しない（§5.2）。
    /// 台帳を持たない文脈では untracked（無課金）で包む。
    fn charge_journal_entry(&mut self) -> Option<Rc<crate::value::HeapToken>> {
        match Value::new_heap_token(
            crate::budget::heap_size::rollback_journal_entry(0),
            &self.budget.heap_handle(),
            ExecutionPhase::Run,
        ) {
            Ok(token) => Some(token),
            Err(stop) => {
                if self.pending_heap_stop.is_none() {
                    self.pending_heap_stop = Some(stop);
                }
                None
            }
        }
    }

    /// REPL checkpointに含まれる既存stack slotを、最初の書換・削除時だけ記録する。
    fn checkpoint_stack_slot(&mut self, index: usize) {
        let should_record = self
            .repl_stack_checkpoint
            .as_ref()
            .is_some_and(|checkpoint| {
                index < checkpoint.stack_len && !checkpoint.originals.contains_key(&index)
            });
        if !should_record {
            return;
        }

        let Some(value) = self.stack.get(index).cloned() else {
            return;
        };
        // journal entry の live heap を課金する（REV-015 PR-d）。超過は
        // `pending_heap_stop` に退避し、次の step 課金境界で surface する。
        let token = self.charge_journal_entry();
        if let Some(checkpoint) = &mut self.repl_stack_checkpoint
            && checkpoint.originals.insert(index, value).is_none()
            && let Some(token) = token
        {
            checkpoint.entry_tokens.push(token);
        }
    }

    /// REPL開始時点に存在したstack範囲を、削除前にjournalへ退避する。
    fn checkpoint_stack_range(&mut self, start: usize, end: usize) {
        let end = end.min(self.stack.len());
        for index in start.min(end)..end {
            self.checkpoint_stack_slot(index);
        }
    }

    /// stackを短縮する。REPL transaction中は削除される既存slotを記録する。
    fn truncate_stack(&mut self, len: usize) {
        let current_len = self.stack.len();
        if len < current_len {
            self.checkpoint_stack_range(len, current_len);
        }
        self.stack.truncate(len);
    }

    /// 変更済みslot・cellを復元し、REPL開始後に積まれた一時値を破棄する。
    fn restore_repl_stack(&mut self, checkpoint: ReplStackCheckpoint) {
        self.stack.truncate(checkpoint.stack_len);
        if self.stack.len() < checkpoint.stack_len {
            self.stack.resize(checkpoint.stack_len, Value::Null);
        }
        for (index, value) in checkpoint.originals {
            self.stack[index] = value;
        }
        // cellの中身を入力開始時点へ戻す（AUD-024）。frames checkpointでは
        // 戻せない、共有cellの破壊的更新を復元する。
        for (_, (cell, original)) in checkpoint.cell_originals {
            *cell.borrow_mut() = original;
        }
    }

    /// cellの中身を書き換える前に、その元値をREPL journalへ記録する（AUD-024）。
    ///
    /// cellのRcポインタ同一性で判定し、1入力につき最初の1回だけ記録する。
    /// REPL transaction中でなければ何もしない。
    fn checkpoint_cell(&mut self, cell: &SharedValue) {
        let id = Rc::as_ptr(cell) as usize;
        let already_recorded = self
            .repl_stack_checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.cell_originals.contains_key(&id));
        if self.repl_stack_checkpoint.is_none() || already_recorded {
            return;
        }
        let original = cell.borrow().clone();
        // journal entry の live heap を課金する（REV-015 PR-d）。超過は
        // `pending_heap_stop` に退避し、次の step 課金境界で surface する。
        let token = self.charge_journal_entry();
        if let Some(checkpoint) = self.repl_stack_checkpoint.as_mut() {
            checkpoint
                .cell_originals
                .insert(id, (Rc::clone(cell), original));
            if let Some(token) = token {
                checkpoint.entry_tokens.push(token);
            }
        }
    }

    /// スタックからpop
    fn pop(&mut self, line: usize) -> Result<Value, TsumugiError> {
        if let Some(index) = self.stack.len().checked_sub(1) {
            self.checkpoint_stack_slot(index);
        }
        self.stack
            .pop()
            .ok_or_else(|| internal_error(line, "内部エラー: スタックが空です"))
    }

    /// インデックスアクセス（コレクションは参照で受け取り複製しない）
    fn eval_index(
        &self,
        collection: &Value,
        index: &Value,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        match collection {
            Value::List(list) => {
                let Value::Int(i) = index else {
                    return Err(TsumugiError::list_index_type(line, index));
                };
                let idx = if *i < 0 {
                    (list.len() as i64 + i) as usize
                } else {
                    *i as usize
                };
                list.get(idx)
                    .cloned()
                    .ok_or_else(|| TsumugiError::list_index_out_of_range(line, *i, list.len()))
            }
            Value::Str(s) => {
                let Value::Int(i) = index else {
                    return Err(TsumugiError::str_index_type(line, index));
                };
                let chars: Vec<char> = s.chars().collect();
                let idx = if *i < 0 {
                    (chars.len() as i64 + i) as usize
                } else {
                    *i as usize
                };
                chars
                    .get(idx)
                    .map(|c| Value::str_constant(c.to_string()))
                    .ok_or_else(|| TsumugiError::str_index_out_of_range(line, *i, chars.len()))
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
            _ => Err(TsumugiError::index_read_unsupported(line, collection)),
        }
    }

    /// インデックス代入を対象bindingへin-placeで適用する。
    ///
    /// binding全体を書き戻さないため、index/valueの評価中に同じbindingへ
    /// 加えられた変更を上書きしない。境界判定とエラーメッセージは
    /// `builtin_core::assign_index` に集約し、tree evaluatorと共有する。
    fn assign_index_binding(
        &mut self,
        target: &MutationTarget,
        index: &Value,
        value: Value,
        line: usize,
    ) -> Result<(), TsumugiError> {
        let max_collection = self.budget.max_collection_elements();
        match self.resolve_binding_storage(target, line)? {
            BindingStorage::Cell(cell) => {
                self.checkpoint_cell(&cell);
                // cell は self とは別の Rc なので、cell の借用と &mut self.budget は両立する。
                crate::builtin_core::assign_index(
                    &mut cell.borrow_mut(),
                    index,
                    value,
                    max_collection,
                    &mut self.budget,
                    line,
                )
            }
            BindingStorage::Stack(stack_index) => {
                self.checkpoint_stack_slot(stack_index);
                if stack_index >= self.stack.len() {
                    return Err(internal_error(line, "インデックス代入の対象slotが不正です"));
                }
                // self.stack と self.budget を同時可変借用できないため、slot を一旦
                // 取り出して assign_index へ渡し、書き戻す。
                let mut slot = std::mem::replace(&mut self.stack[stack_index], Value::Null);
                let result = crate::builtin_core::assign_index(
                    &mut slot,
                    index,
                    value,
                    max_collection,
                    &mut self.budget,
                    line,
                );
                self.stack[stack_index] = slot;
                result
            }
        }
    }

    // --- 組み込み関数 ---

    fn exec_builtin(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        // filesystem host call の count + request bytes を、境界へ入る（副作用が始まる）
        // 前に課金する（§6.1、REV-015 Slice 2、I-O accounting）。tree engine（builtin.rs
        // の PureCore wrapper）と同じ論理位置・同じ規則で課金する。read 系は request 0 byte。
        let is_host_call = crate::builtin_core::is_host_call_builtin(name);
        if is_host_call {
            let request_bytes = crate::builtin_core::host_call_request_bytes(name, &args);
            self.budget
                .charge_host_call_request(request_bytes, ExecutionPhase::Run)
                .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
        }
        // まず共通モジュールで処理を試みる
        let max_collection = self.budget.max_collection_elements();
        if let Some(result) = crate::builtin_core::dispatch(name, &args, max_collection, line)? {
            // filesystem host call の response bytes（読み込み内容）を、結果が確定した後に
            // 課金する（§6.1）。write 系は response 0 byte。
            if is_host_call {
                let response_bytes = crate::builtin_core::host_call_response_bytes(name, &result);
                self.budget
                    .charge_host_response_bytes(response_bytes, ExecutionPhase::Run)
                    .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
            }
            // builtin が生成した untracked collection を tracked 化しつつ heap 課金し、
            // 新規 String body も課金する（REV-015 案A / Slice 2）。
            let tracked = self
                .budget
                .track_result(result, ExecutionPhase::Run)
                .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
            return Ok(tracked);
        }

        // コンテキスト依存のビルトイン（VM固有の実装が必要なもの）
        match name {
            "input" => {
                crate::builtin_core::check_arity(name, &args, 0, line)?;
                let mut buf = String::new();
                let value = match std::io::stdin().read_line(&mut buf) {
                    Ok(0) => Value::Null,
                    Ok(_) => {
                        if buf.ends_with('\n') {
                            buf.pop();
                            if buf.ends_with('\r') {
                                buf.pop();
                            }
                        }
                        Value::str_constant(buf)
                    }
                    Err(_) => Value::Null,
                };
                // input（stdio host call）を課金する（§6.1、REV-015 Slice 2）。tree engine
                // と同じく受け取った payload の byte 長を count と併せて課金する。
                let payload_bytes = match &value {
                    Value::Str(s) => s.len() as u64,
                    _ => 0,
                };
                self.budget
                    .charge_input(payload_bytes, ExecutionPhase::Run)
                    .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))?;
                Ok(value)
            }
            "exit" => {
                if args.len() > 1 {
                    return Err(TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Argument,
                        format!("exit() は引数0〜1個ですが、{}個渡されました", args.len()),
                    ));
                }
                // C7（REV-023）: プロセスを終了せず structured terminal（Exited）へ写す。
                let code = match args.first() {
                    None => None,
                    Some(Value::Int(n)) => Some(*n),
                    Some(other) => {
                        return Err(TsumugiError::builtin_arg_type(
                            line, "exit", 1, "Int", other,
                        ));
                    }
                };
                let has_exit = self.capabilities.process_exit().is_some();
                let code = crate::builtin_core::resolve_exit(code, has_exit, line)?;
                // pending_exit を載せ、catch 不可の ProcessExit 信号を返す。
                self.pending_exit = Some(code);
                Err(TsumugiError::process_exit_signal(line))
            }
            "args" => {
                crate::builtin_core::check_arity(name, &args, 0, line)?;
                // process argv ではなく実行 context の snapshot を返す（AUD-018）
                let argv: Vec<Value> = self
                    .script_args
                    .iter()
                    .map(|arg| Value::str_constant(arg.clone()))
                    .collect();
                self.check_collection(argv.len(), line)?;
                Value::new_list(argv, &mut self.budget, ExecutionPhase::Run)
                    .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))
            }
            "map" => {
                crate::builtin_core::check_arity(name, &args, 2, line)?;
                if let Value::List(list) = &args[0] {
                    let func = args[1].clone();
                    let mut result = Vec::new();
                    for item in list.iter() {
                        let value =
                            self.call_fn_value("map", func.clone(), vec![item.clone()], line)?;
                        self.check_collection(result.len().saturating_add(1), line)?;
                        result.push(value);
                    }
                    Value::new_list(result, &mut self.budget, ExecutionPhase::Run)
                        .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))
                } else {
                    Err(TsumugiError::builtin_arg_type(
                        line, "map", 1, "List", &args[0],
                    ))
                }
            }
            "filter" => {
                crate::builtin_core::check_arity(name, &args, 2, line)?;
                if let Value::List(list) = &args[0] {
                    let func = args[1].clone();
                    let mut result = Vec::new();
                    for item in list.iter() {
                        let cond =
                            self.call_fn_value("filter", func.clone(), vec![item.clone()], line)?;
                        if cond.is_truthy() {
                            self.check_collection(result.len().saturating_add(1), line)?;
                            result.push(item.clone());
                        }
                    }
                    Value::new_list(result, &mut self.budget, ExecutionPhase::Run)
                        .map_err(|stop| Self::control_stop_to_error(&self.budget, stop, line))
                } else {
                    Err(TsumugiError::builtin_arg_type(
                        line, "filter", 1, "List", &args[0],
                    ))
                }
            }
            "each" => {
                crate::builtin_core::check_arity(name, &args, 2, line)?;
                if let Value::List(list) = &args[0] {
                    let func = args[1].clone();
                    for item in list.iter() {
                        self.call_fn_value("each", func.clone(), vec![item.clone()], line)?;
                    }
                    Ok(Value::Null)
                } else {
                    Err(TsumugiError::builtin_arg_type(
                        line, "each", 1, "List", &args[0],
                    ))
                }
            }
            _ => Err(internal_error(
                line,
                format!("未定義の組み込み関数: {}", name),
            )),
        }
    }

    /// 関数値を呼び出すヘルパー（map/filter/each 用）
    fn call_fn_value(
        &mut self,
        builtin: &str,
        func: Value,
        args: Vec<Value>,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        // step 課金は per-instruction 課金へ一本化した（REV-006）。callback 本体の
        // 各命令は run_frames で課金される。深度上限のみここで検査する。
        if self.active_user_frame_count() >= MAX_USER_CALL_DEPTH {
            return Err(TsumugiError::call_depth_limit(line, MAX_USER_CALL_DEPTH));
        }
        let self_value = func.clone();
        if let Value::VmFn {
            arity,
            chunk,
            upvalues,
            ..
        } = func
        {
            // callbackは常に1引数で呼ぶため、arity不一致はcallback専用messageで報告する。
            if args.len() != arity {
                return Err(TsumugiError::callback_arity(line, builtin, arity));
            }
            // 関数自身をスタックに積む（slot 0）。direct callback内の自己再帰でも
            // 通常のOpCode::Callと同じself bindingを参照できるようにする。
            let base = self.stack.len();
            self.stack.push(self_value);
            for arg in args {
                self.stack.push(arg);
            }
            let target_depth = self.frames.len();
            self.frames.push(CallFrame {
                chunk,
                ip: 0,
                base,
                upvalues,
                locals_cells: Vec::new(),
            });
            // run_frames で実行し、target_depth まで戻ったら値を返す
            self.run_frames(target_depth)
        } else {
            Err(TsumugiError::callback_not_callable(
                line,
                builtin,
                &self_value,
            ))
        }
    }

    // --- 算術演算 ---

    fn binary_add(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => a
                .checked_add(*b)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::int_overflow(line, "加算")),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
            (Value::Str(a), Value::Str(b)) => {
                Ok(Value::str_constant(format!("{}{}", a.as_str(), b.as_str())))
            }
            (Value::Str(a), Value::Error { .. }) => {
                Ok(Value::str_constant(format!("{}{}", a.as_str(), right)))
            }
            (Value::Error { .. }, Value::Str(b)) => {
                Ok(Value::str_constant(format!("{}{}", left, b.as_str())))
            }
            _ => Err(TsumugiError::arithmetic_type(
                line,
                crate::ast::BinOpKind::Add,
                &left,
                &right,
            )),
        }
    }

    fn binary_sub(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => a
                .checked_sub(*b)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::int_overflow(line, "減算")),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 - b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - *b as f64)),
            _ => Err(TsumugiError::arithmetic_type(
                line,
                crate::ast::BinOpKind::Sub,
                &left,
                &right,
            )),
        }
    }

    fn binary_mul(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => a
                .checked_mul(*b)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::int_overflow(line, "乗算")),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 * b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * *b as f64)),
            _ => Err(TsumugiError::arithmetic_type(
                line,
                crate::ast::BinOpKind::Mul,
                &left,
                &right,
            )),
        }
    }

    fn binary_div(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(TsumugiError::zero_division(line))
                } else {
                    a.checked_div(*b)
                        .map(Value::Int)
                        .ok_or_else(|| TsumugiError::int_overflow(line, "除算"))
                }
            }
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 / b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a / *b as f64)),
            _ => Err(TsumugiError::arithmetic_type(
                line,
                crate::ast::BinOpKind::Div,
                &left,
                &right,
            )),
        }
    }

    fn binary_mod(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(TsumugiError::zero_division(line))
                } else {
                    a.checked_rem(*b)
                        .map(Value::Int)
                        .ok_or_else(|| TsumugiError::int_overflow(line, "剰余"))
                }
            }
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a % b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 % b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a % *b as f64)),
            _ => Err(TsumugiError::arithmetic_type(
                line,
                crate::ast::BinOpKind::Mod,
                &left,
                &right,
            )),
        }
    }

    fn compare_lt(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        // Int/Float は跨いで厳密比較する（REV-003）。tree と同じ NumericOrder を経由。
        match NumericOrder::compare_relational(&left, &right) {
            Some(ord) => Ok(Value::Bool(ord.is_lt())),
            None => Err(TsumugiError::comparison_type(
                line,
                crate::ast::BinOpKind::Lt,
                &left,
                &right,
            )),
        }
    }

    fn compare_gt(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match NumericOrder::compare_relational(&left, &right) {
            Some(ord) => Ok(Value::Bool(ord.is_gt())),
            None => Err(TsumugiError::comparison_type(
                line,
                crate::ast::BinOpKind::Gt,
                &left,
                &right,
            )),
        }
    }

    fn compare_lteq(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match NumericOrder::compare_relational(&left, &right) {
            Some(ord) => Ok(Value::Bool(ord.is_le())),
            None => Err(TsumugiError::comparison_type(
                line,
                crate::ast::BinOpKind::LtEq,
                &left,
                &right,
            )),
        }
    }

    fn compare_gteq(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match NumericOrder::compare_relational(&left, &right) {
            Some(ord) => Ok(Value::Bool(ord.is_ge())),
            None => Err(TsumugiError::comparison_type(
                line,
                crate::ast::BinOpKind::GtEq,
                &left,
                &right,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Chunk;
    use crate::opcode::OpCode;
    use std::rc::Rc;

    #[test]
    fn call_without_prepare_still_enforces_depth_limit() {
        let mut recursive = Chunk::new();
        recursive.name = "malformed_recursive".to_string();
        recursive.emit(OpCode::GetLocal(0), 1);
        recursive.emit(OpCode::Call(0), 1);
        recursive.emit(OpCode::ReturnValue, 1);

        let function = Value::VmFn {
            id: FunctionId(0),
            name: "malformed_recursive".to_string(),
            arity: 0,
            params: Vec::new(),
            chunk: Rc::new(recursive),
            upvalues: Vec::new(),
            header: Value::fn_header_untracked(),
        };
        let mut main = Chunk::new();
        main.emit_constant(function, 1);
        main.emit(OpCode::Call(0), 1);
        main.emit(OpCode::Return, 1);

        let error = Vm::new(VerifiedChunk::from_trusted(main))
            .run()
            .expect_err("PrepareCallなしの再帰Callが成功しました");
        assert_eq!(error.error_type(), "overflow");
        assert!(error.message().contains("スタックオーバーフロー"));
    }

    #[test]
    fn malformed_call_with_missing_stack_returns_internal_error() {
        let mut chunk = Chunk::new();
        chunk.emit(OpCode::Call(0), 1);
        chunk.emit(OpCode::Return, 1);

        let error = Vm::new(VerifiedChunk::from_trusted(chunk))
            .run()
            .expect_err("calleeのないCallが成功しました");
        assert!(error.message().contains("Call のスタック要素が不足"));
    }

    #[test]
    fn malformed_call_with_overflowing_arg_count_returns_internal_error() {
        let mut chunk = Chunk::new();
        chunk.emit(OpCode::Call(usize::MAX), 1);
        chunk.emit(OpCode::Return, 1);

        let error = Vm::new(VerifiedChunk::from_trusted(chunk))
            .run()
            .expect_err("overflowする引数数のCallが成功しました");
        assert!(error.message().contains("Call の引数数が不正"));
    }

    /// AUD-023: compilerが生成しない命令列でもhost panicさせず internal error を返す。
    ///
    /// 網羅ケースは公開APIだけで書ける `tests/defensive_vm.rs` にある。
    /// ここでは内部実装と一緒に読める最小のケースだけ残す。
    #[test]
    fn out_of_range_local_slot_returns_internal_error() {
        let mut chunk = Chunk::new();
        chunk.emit(OpCode::GetLocal(999), 1);
        chunk.emit(OpCode::Return, 1);

        let error = Vm::new(VerifiedChunk::from_trusted(chunk))
            .run()
            .expect_err("範囲外のlocal読み取りが成功しました");
        assert_eq!(error.error_type(), "internal");
        assert!(
            error.message().contains("local slotが不正です"),
            "想定外のメッセージ: {}",
            error.message()
        );
    }

    #[test]
    fn function_id_is_monotonic_and_starts_at_zero() {
        // AUD-048: allocate_function_id は 0 から単調増加する
        let mut vm = Vm::new(VerifiedChunk::from_trusted(Chunk::new()));
        assert_eq!(vm.allocate_function_id(1).unwrap(), FunctionId(0));
        assert_eq!(vm.allocate_function_id(1).unwrap(), FunctionId(1));
        assert_eq!(vm.allocate_function_id(1).unwrap(), FunctionId(2));
    }

    #[test]
    fn function_id_overflow_reports_internal_error() {
        // AUD-048: u64 を使い切ったら internal error を返す
        let mut vm = Vm::new(VerifiedChunk::from_trusted(Chunk::new()));
        vm.next_function_id = u64::MAX - 1;
        assert_eq!(
            vm.allocate_function_id(7).unwrap(),
            FunctionId(u64::MAX - 1)
        );
        let error = vm
            .allocate_function_id(7)
            .expect_err("FunctionId overflow が成功しました");
        assert_eq!(error.error_type(), "internal");
        assert_eq!(error.line(), 7);
        assert!(
            error.message().contains("FunctionId を割り当てできません"),
            "想定外のメッセージ: {}",
            error.message()
        );
    }
}
