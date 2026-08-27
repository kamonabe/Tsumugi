//! 仮想マシン: バイトコード（Chunk）を実行するスタックマシン

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::chunk::Chunk;
use crate::error::TsumugiError;
use crate::opcode::{MutationTarget, OpCode};
use crate::value::{SharedValue, Value};

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

/// デフォルトのステップ上限（100万）
const DEFAULT_MAX_STEPS: u64 = 1_000_000;

/// コールフレーム深度の上限（スタックオーバーフロー防止）
const MAX_CALL_DEPTH: usize = 128;

/// 環境変数からステップ上限を読み取る
fn vm_resolve_max_steps() -> u64 {
    std::env::var("TSUMUGI_MAX_STEPS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_STEPS)
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

/// スタックベースの仮想マシン
pub struct Vm {
    /// コールフレームスタック
    frames: Vec<CallFrame>,

    /// 値スタック
    stack: Vec<Value>,

    /// 実行済みtop-level宣言の名前からstack slotへの対応。
    /// 値自体はstack/locals_cellsをsource of truthとし、bindingを複製しない。
    globals: HashMap<String, usize>,

    /// 実行ステップカウンタ（ループ反復 + 関数呼び出し）
    steps: u64,

    /// ステップ上限
    max_steps: u64,

    /// 例外ハンドラスタック（try/catch）
    try_handlers: Vec<TryHandler>,
}

impl Vm {
    pub fn new(chunk: Chunk) -> Self {
        let frame = CallFrame {
            chunk: Rc::new(chunk),
            ip: 0,
            base: 0,
            upvalues: Vec::new(),
            locals_cells: Vec::new(),
        };
        Vm {
            frames: vec![frame],
            stack: Vec::with_capacity(256),
            globals: HashMap::new(),
            steps: 0,
            max_steps: vm_resolve_max_steps(),
            try_handlers: Vec::new(),
        }
    }

    /// REPL 用: 空のスタックで VM を生成（最初の run_repl_chunk で使用）
    pub fn new_repl() -> Self {
        Vm {
            frames: Vec::new(),
            stack: Vec::with_capacity(256),
            globals: HashMap::new(),
            steps: 0,
            max_steps: vm_resolve_max_steps(),
            try_handlers: Vec::new(),
        }
    }

    /// チャンクを実行する
    pub fn run(&mut self) -> Result<(), TsumugiError> {
        self.run_frames(0)?;
        Ok(())
    }

    /// REPL 用: 既存のスタック（ローカル変数）を保持したまま新しいチャンクを実行する。
    /// 前回のフレームを差し替えて実行し、終了後もスタック上の値を保持する。
    pub fn run_repl_chunk(&mut self, chunk: Chunk) -> Result<(), TsumugiError> {
        // 未捕捉エラー時に、入力途中の一時値・callee frame・try handlerを
        // 次の入力へ持ち越さないための構造状態checkpoint。
        let frames_checkpoint = self.frames.clone();
        let stack_checkpoint = self.stack.clone();
        let globals_checkpoint = self.globals.clone();
        let handlers_checkpoint = self.try_handlers.clone();
        let steps_checkpoint = self.steps;

        // top-levelでcell化された変数は入力間でも同じcellを使う。
        // これを空にすると既存closureとtop-level変数の参照先が分離する。
        let locals_cells = self
            .frames
            .first()
            .map(|frame| frame.locals_cells.clone())
            .unwrap_or_default();
        let frame = CallFrame {
            chunk: Rc::new(chunk),
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
        // ステップカウンタはリセット（各入力で予算を全額使えるように）
        self.steps = 0;

        match self.run_frames(0) {
            Ok(_) => Ok(()),
            Err(error) => {
                self.frames = frames_checkpoint;
                self.stack = stack_checkpoint;
                self.globals = globals_checkpoint;
                self.try_handlers = handlers_checkpoint;
                self.steps = steps_checkpoint;
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
            let frame = self.frames.last().unwrap();
            if frame.ip >= frame.chunk.code.len() {
                // フレームの命令が尽きた = 暗黙 null return
                if self.frames.len() <= stop_depth + 1 {
                    // トップレベルまたは stop_depth に戻った
                    break;
                }
                // ネストされた関数が暗黙 null return で終わった場合
                let f = self.frames.pop().unwrap();
                self.stack.truncate(f.base);
                // 暗黙 return 時にこのフレーム内の try ハンドラを除去する
                self.try_handlers
                    .retain(|h| h.frame_depth <= self.frames.len());
                self.stack.push(Value::Null);
                continue;
            }

            let instruction = frame.chunk.code[frame.ip].clone();
            let line = frame.chunk.lines[frame.ip];
            self.frames.last_mut().unwrap().ip += 1;

            let result = match &instruction {
                OpCode::ReturnValue => {
                    let return_value = self.pop(line)?;
                    let frame = self.frames.pop().unwrap();
                    self.stack.truncate(frame.base);
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
                    let f = self.frames.pop().unwrap();
                    self.stack.truncate(f.base);
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
            };

            if let Err(e) = result {
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
                        self.stack.truncate(handler.stack_depth);
                        // 構造化エラーをスタックに積む
                        let error_value = Value::Error {
                            error_type: e.error_type().to_string(),
                            message: e.message().to_string(),
                            line: e.line(),
                        };
                        self.stack.push(error_value);
                        // catch ブロックへジャンプ
                        self.frames.last_mut().unwrap().ip = handler.catch_ip;
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

    /// ステップカウンタを進め、上限チェックする
    fn count_step(&mut self, line: usize) -> Result<(), TsumugiError> {
        self.steps += 1;
        if self.steps > self.max_steps {
            return Err(TsumugiError::runtime(
                line,
                format!("ステップ上限に達しました (上限: {})", self.max_steps),
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
            let call_line = if caller.ip > 0 {
                caller.chunk.lines[caller.ip - 1]
            } else {
                1
            };
            trace.push(TraceFrame {
                name: callee.chunk.name.clone(),
                line: call_line,
            });
        }

        error.with_trace(trace)
    }

    /// ローカル変数を読み取る（セル経由の場合はセルから読む）
    fn get_local(&self, slot: usize) -> Value {
        let frame = self.frames.last().unwrap();
        // locals_cells にセルがあればそこから読む
        if slot < frame.locals_cells.len()
            && let Some(ref cell) = frame.locals_cells[slot]
        {
            return cell.borrow().clone();
        }
        // 通常のスタック読み取り
        self.stack[frame.base + slot].clone()
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
    fn set_local(&mut self, slot: usize, value: Value) {
        let frame = self.frames.last().unwrap();
        // locals_cells にセルがあればそこに書く
        if slot < frame.locals_cells.len()
            && let Some(ref cell) = frame.locals_cells[slot]
        {
            *cell.borrow_mut() = value;
            return;
        }
        let base = frame.base;
        self.stack[base + slot] = value;
    }

    /// ローカル変数をキャプチャ用セルに昇格させる
    /// 既にセルがあればそれを返す。なければスタックの値からセルを作成し、登録して返す
    fn ensure_local_cell(&mut self, slot: usize) -> SharedValue {
        let frame = self.frames.last_mut().unwrap();
        // locals_cells を必要なサイズに拡張
        while frame.locals_cells.len() <= slot {
            frame.locals_cells.push(None);
        }
        if let Some(ref cell) = frame.locals_cells[slot] {
            return Rc::clone(cell);
        }
        // スタックから現在の値を取り出してセルを作成
        let value = self.stack[frame.base + slot].clone();
        let cell = Rc::new(RefCell::new(value));
        frame.locals_cells[slot] = Some(Rc::clone(&cell));
        cell
    }

    /// runtime globalを読み取る。registryはtop-level slotだけを保持し、
    /// cell化済みなら同じSharedValue、未cell化なら同じstack slotから値を得る。
    fn get_global(&self, name: &str, line: usize, for_call: bool) -> Result<Value, TsumugiError> {
        let slot = self.globals.get(name).copied().ok_or_else(|| {
            let message = if for_call {
                format!("未定義の関数: {}", name)
            } else {
                format!("未定義の変数: {}", name)
            };
            TsumugiError::runtime_with_kind(line, crate::error::ErrorKind::Name, message)
        })?;
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
        Err(TsumugiError::runtime_with_kind(
            line,
            crate::error::ErrorKind::Name,
            format!("未定義の変数: {}", name),
        ))
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
                let slot = self.globals.get(name).copied().ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Name,
                        format!("未定義の変数: {}", name),
                    )
                })?;
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
        let slot = self.globals.get(name).copied().ok_or_else(|| {
            TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::Name,
                format!("未定義の変数に代入: {}", name),
            )
        })?;
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
                let value = self.frames.last().unwrap().chunk.constants[idx].clone();
                self.stack.push(value);
            }
            OpCode::Add => {
                let right = self.pop(line)?;
                let left = self.pop(line)?;
                let result = self.binary_add(left, right, line)?;
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
                        .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー"))?,
                    Value::Float(n) => Value::Float(-n),
                    other => {
                        return Err(TsumugiError::runtime(
                            line,
                            format!("型エラー: -{} は計算できません", type_name(&other)),
                        ));
                    }
                };
                self.stack.push(result);
            }
            OpCode::GetLocal(slot) => {
                let value = self.get_local(slot);
                self.stack.push(value);
            }
            OpCode::SetLocal(slot) => {
                let value =
                    self.stack.last().cloned().ok_or_else(|| {
                        TsumugiError::runtime(line, "内部エラー: スタックが空です")
                    })?;
                self.set_local(slot, value);
            }
            OpCode::GetGlobal(name) => {
                let value = self.get_global(&name, line, false)?;
                self.stack.push(value);
            }
            OpCode::GetGlobalForCall(name) => {
                let value = self.get_global(&name, line, true)?;
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
                    self.frames.last_mut().unwrap().ip = target;
                }
            }
            OpCode::RequireGlobal(name) => {
                self.require_global(&name, line)?;
            }
            OpCode::Jump(target) => {
                self.frames.last_mut().unwrap().ip = target;
            }
            OpCode::JumpIfFalse(target) => {
                let value = self.pop(line)?;
                if !value.is_truthy() {
                    self.frames.last_mut().unwrap().ip = target;
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
                    self.frames.last_mut().unwrap().ip = target;
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
                    self.frames.last_mut().unwrap().ip = target;
                }
            }
            OpCode::Loop(target) => {
                self.count_step(line)?;
                self.frames.last_mut().unwrap().ip = target;
            }
            OpCode::GetUpvalue(index) => {
                let value = self.frames.last().unwrap().upvalues[index].borrow().clone();
                self.stack.push(value);
            }
            OpCode::SetUpvalue(index) => {
                let value =
                    self.stack.last().cloned().ok_or_else(|| {
                        TsumugiError::runtime(line, "内部エラー: スタックが空です")
                    })?;
                let cell = self.frames.last().unwrap().upvalues[index].clone();
                *cell.borrow_mut() = value;
            }
            OpCode::MakeClosure(upvalue_count) => {
                // upvalue_count 個の値がスタックに積まれている
                // コンパイラは MakeClosure(N) の直前に N 個の GetLocal/GetUpvalue を emit する
                // GetLocal → 親のローカル変数セルを共有
                // GetUpvalue → 親の upvalue セルを共有（多段キャプチャ）
                let frame = self.frames.last().unwrap();
                let make_closure_ip = frame.ip - 1;

                let mut upvalue_sources = Vec::with_capacity(upvalue_count);
                for i in 0..upvalue_count {
                    let instr_ip = make_closure_ip - upvalue_count + i;
                    match &frame.chunk.code[instr_ip] {
                        OpCode::GetLocal(slot) => {
                            upvalue_sources.push((true, *slot)); // is_local, slot
                        }
                        OpCode::GetUpvalue(index) => {
                            upvalue_sources.push((false, *index)); // is_upvalue, index
                        }
                        _ => {
                            upvalue_sources.push((true, usize::MAX)); // フォールバック
                        }
                    }
                }

                // スタックから積まれた値を pop
                for _ in 0..upvalue_count {
                    self.pop(line)?;
                }

                // 各 upvalue についてセルを取得/作成
                let mut upvalue_cells = Vec::with_capacity(upvalue_count);
                for (is_local, slot) in upvalue_sources {
                    if is_local {
                        if slot == usize::MAX {
                            upvalue_cells.push(Rc::new(RefCell::new(Value::Null)));
                        } else {
                            let cell = self.ensure_local_cell(slot);
                            upvalue_cells.push(cell);
                        }
                    } else {
                        // 親の upvalue セルを直接共有（多段キャプチャ）
                        let cell = self.frames.last().unwrap().upvalues[slot].clone();
                        upvalue_cells.push(cell);
                    }
                }

                let fn_value = self.pop(line)?;
                if let Value::VmFn {
                    name,
                    arity,
                    params,
                    chunk,
                    ..
                } = fn_value
                {
                    self.stack.push(Value::VmFn {
                        name,
                        arity,
                        params,
                        chunk,
                        upvalues: upvalue_cells,
                    });
                } else {
                    return Err(TsumugiError::runtime(
                        line,
                        "内部エラー: MakeClosure の対象が VmFn ではありません",
                    ));
                }
            }
            OpCode::PrepareCall => {
                self.count_step(line)?;
                if self.frames.len() >= MAX_CALL_DEPTH {
                    return Err(TsumugiError::runtime(
                        line,
                        format!(
                            "スタックオーバーフロー: 再帰が深すぎます (上限: {})",
                            MAX_CALL_DEPTH
                        ),
                    ));
                }
            }
            OpCode::ValidateCall(arg_count) => {
                let fn_value = self.stack.last().ok_or_else(|| {
                    TsumugiError::runtime(line, "内部エラー: ValidateCall のcalleeがありません")
                })?;
                if let Value::VmFn { name, arity, .. } = fn_value {
                    if arg_count != *arity {
                        return Err(TsumugiError::runtime(
                            line,
                            format!(
                                "関数 {} は引数{}個ですが、{}個渡されました",
                                name, arity, arg_count
                            ),
                        ));
                    }
                } else {
                    return Err(TsumugiError::runtime(
                        line,
                        format!("関数ではない値を呼び出そうとしました: {:?}", fn_value),
                    ));
                }
            }
            OpCode::Call(arg_count) => {
                // PrepareCallを経由しない不正bytecodeでもframe上限を迂回させない。
                // stepはPrepareCallだけで数え、ここでは二重countしない。
                if self.frames.len() >= MAX_CALL_DEPTH {
                    return Err(TsumugiError::runtime(
                        line,
                        format!(
                            "スタックオーバーフロー: 再帰が深すぎます (上限: {})",
                            MAX_CALL_DEPTH
                        ),
                    ));
                }
                let required = arg_count.checked_add(1).ok_or_else(|| {
                    TsumugiError::runtime(line, "内部エラー: Call の引数数が不正です")
                })?;
                let fn_pos = self.stack.len().checked_sub(required).ok_or_else(|| {
                    TsumugiError::runtime(line, "内部エラー: Call のスタック要素が不足しています")
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
                        return Err(TsumugiError::runtime(
                            line,
                            format!(
                                "関数 {} は引数{}個ですが、{}個渡されました",
                                name, arity, arg_count
                            ),
                        ));
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
                    return Err(TsumugiError::runtime(
                        line,
                        format!("関数ではない値を呼び出そうとしました: {:?}", fn_value),
                    ));
                }
            }
            OpCode::Print(arg_count) => {
                let mut values = Vec::with_capacity(arg_count);
                for _ in 0..arg_count {
                    values.push(self.pop(line)?);
                }
                values.reverse();
                let output: Vec<String> = values.iter().map(|v| v.to_string()).collect();
                println!("{}", output.join(" "));
            }
            OpCode::Pop => {
                // 単一 pop 時もセルをクリア
                let frame = self.frames.last_mut().unwrap();
                let slot = self.stack.len() - 1 - frame.base;
                if slot < frame.locals_cells.len() {
                    frame.locals_cells[slot] = None;
                }
                self.pop(line)?;
            }
            OpCode::PopN(count) => {
                // スコープ終了: 対応する locals_cells をクリアしてからスタックを削除
                let frame = self.frames.last_mut().unwrap();
                let stack_top = self.stack.len();
                for i in 0..count {
                    let slot = stack_top - 1 - i - frame.base;
                    if slot < frame.locals_cells.len() {
                        frame.locals_cells[slot] = None;
                    }
                }
                for _ in 0..count {
                    self.pop(line)?;
                }
            }
            OpCode::LenLocal(slot) => {
                let length = self.with_local_ref(slot, line, |value| value_len(value, line))?;
                self.stack.push(Value::Int(length));
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
                let list = self
                    .stack
                    .last_mut()
                    .ok_or_else(|| TsumugiError::runtime(line, "内部エラー: スタックが空です"))?;
                if let Value::List(v) = list {
                    crate::builtin_core::check_collection_size_public(
                        v.len().saturating_add(1),
                        line,
                    )?;
                    v.push(value);
                } else {
                    return Err(TsumugiError::runtime(
                        line,
                        "内部エラー: ListPush の対象がリストではありません",
                    ));
                }
            }
            OpCode::DictInsert => {
                let value = self.pop(line)?;
                let key = self.pop(line)?;
                let dict = self
                    .stack
                    .last_mut()
                    .ok_or_else(|| TsumugiError::runtime(line, "内部エラー: スタックが空です"))?;
                if let Value::Dict(map) = dict {
                    if let Value::Str(k) = key {
                        if !map.contains_key(&k) {
                            crate::builtin_core::check_collection_size_public(
                                map.len().saturating_add(1),
                                line,
                            )?;
                        }
                        map.insert(k, value);
                    } else {
                        return Err(TsumugiError::runtime(
                            line,
                            "辞書のキーは文字列である必要があります",
                        ));
                    }
                } else {
                    return Err(TsumugiError::runtime(
                        line,
                        "内部エラー: DictInsert の対象が辞書ではありません",
                    ));
                }
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
                        crate::builtin_core::check_collection_size_public(values.len(), line)?;
                        value
                    }
                    Value::Dict(ref map) => {
                        crate::builtin_core::check_collection_size_public(map.len(), line)?;
                        Value::List(map.keys().map(|k| Value::Str(k.clone())).collect())
                    }
                    Value::Str(ref s) => {
                        let size = s.chars().count();
                        crate::builtin_core::check_collection_size_public(size, line)?;
                        Value::List(s.chars().map(|c| Value::Str(c.to_string())).collect())
                    }
                    _ => {
                        return Err(TsumugiError::runtime(
                            line,
                            format!("型エラー: {:?} はイテレートできません", value),
                        ));
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
            OpCode::CallBuiltin(name_idx, arg_count) => {
                let name = match &self.frames.last().unwrap().chunk.constants[name_idx] {
                    Value::Str(s) => s.clone(),
                    _ => {
                        return Err(TsumugiError::runtime(
                            line,
                            "内部エラー: CallBuiltin の関数名が文字列ではありません",
                        ));
                    }
                };
                let mut args = Vec::with_capacity(arg_count);
                for _ in 0..arg_count {
                    args.push(self.pop(line)?);
                }
                args.reverse();
                let result = self.exec_builtin(&name, args, line)?;
                self.stack.push(result);
            }
            OpCode::FStrConcat(count) => {
                // スタックから count 個の値を取り出して文字列に連結
                let start = self.stack.len() - count;
                let parts: Vec<Value> = self.stack.drain(start..).collect();
                let mut result = String::new();
                for val in parts {
                    result.push_str(&val.to_string());
                }
                self.stack.push(Value::Str(result));
            }
            OpCode::ReturnValue | OpCode::Return => {
                // これらは run_frames() で処理済み、ここに来ない
                unreachable!()
            }
            OpCode::SetupTry(_) | OpCode::TeardownTry => {
                // これらは run_frames() で処理済み、ここに来ない
                unreachable!()
            }
        }
        Ok(())
    }

    /// スタックからpop
    fn pop(&mut self, line: usize) -> Result<Value, TsumugiError> {
        self.stack
            .pop()
            .ok_or_else(|| TsumugiError::runtime(line, "内部エラー: スタックが空です"))
    }

    /// インデックスアクセス（コレクションは参照で受け取り複製しない）
    fn eval_index(
        &self,
        collection: &Value,
        index: &Value,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        match (collection, index) {
            (Value::List(list), Value::Int(i)) => {
                let idx = if *i < 0 {
                    (list.len() as i64 + i) as usize
                } else {
                    *i as usize
                };
                list.get(idx).cloned().ok_or_else(|| {
                    TsumugiError::runtime(
                        line,
                        format!("インデックス範囲外: {} (長さ: {})", i, list.len()),
                    )
                })
            }
            (Value::Str(s), Value::Int(i)) => {
                let chars: Vec<char> = s.chars().collect();
                let idx = if *i < 0 {
                    (chars.len() as i64 + i) as usize
                } else {
                    *i as usize
                };
                chars
                    .get(idx)
                    .map(|c| Value::Str(c.to_string()))
                    .ok_or_else(|| {
                        TsumugiError::runtime(
                            line,
                            format!("インデックス範囲外: {} (長さ: {})", i, chars.len()),
                        )
                    })
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
            _ => Err(TsumugiError::runtime(
                line,
                format!(
                    "型エラー: {:?} に対して {:?} でインデックスアクセスできません",
                    collection, index
                ),
            )),
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
        match self.resolve_binding_storage(target, line)? {
            BindingStorage::Cell(cell) => {
                crate::builtin_core::assign_index(&mut cell.borrow_mut(), index, value, line)
            }
            BindingStorage::Stack(stack_index) => {
                let slot = self.stack.get_mut(stack_index).ok_or_else(|| {
                    TsumugiError::runtime_with_kind(
                        line,
                        crate::error::ErrorKind::Internal,
                        "インデックス代入の対象slotが不正です",
                    )
                })?;
                crate::builtin_core::assign_index(slot, index, value, line)
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
        // まず共通モジュールで処理を試みる
        if let Some(result) = crate::builtin_core::dispatch(name, &args, line)? {
            return Ok(result);
        }

        // コンテキスト依存のビルトイン（VM固有の実装が必要なもの）
        match name {
            "input" => {
                crate::builtin_core::check_arity(name, &args, 0, line)?;
                let mut buf = String::new();
                match std::io::stdin().read_line(&mut buf) {
                    Ok(0) => Ok(Value::Null),
                    Ok(_) => {
                        if buf.ends_with('\n') {
                            buf.pop();
                            if buf.ends_with('\r') {
                                buf.pop();
                            }
                        }
                        Ok(Value::Str(buf))
                    }
                    Err(_) => Ok(Value::Null),
                }
            }
            "exit" => {
                if args.len() > 1 {
                    return Err(TsumugiError::runtime(
                        line,
                        format!("exit() は引数0〜1個ですが、{}個渡されました", args.len()),
                    ));
                }
                let code = match args.first() {
                    None => 0,
                    Some(Value::Int(n)) => *n as i32,
                    Some(_) => {
                        return Err(TsumugiError::runtime(
                            line,
                            "exit() の引数は整数である必要があります",
                        ));
                    }
                };
                std::process::exit(code);
            }
            "args" => {
                crate::builtin_core::check_arity(name, &args, 0, line)?;
                let argv: Vec<Value> = std::env::args()
                    .skip(1)
                    .filter(|a| a != "--vm")
                    .skip(1) // スクリプトパスをスキップ
                    .map(Value::Str)
                    .collect();
                crate::builtin_core::check_collection_size_public(argv.len(), line)?;
                Ok(Value::List(argv))
            }
            "map" => {
                crate::builtin_core::check_arity(name, &args, 2, line)?;
                if let Value::List(list) = &args[0] {
                    let func = args[1].clone();
                    let mut result = Vec::new();
                    for item in list {
                        let value = self.call_fn_value(func.clone(), vec![item.clone()], line)?;
                        crate::builtin_core::check_collection_size_public(
                            result.len().saturating_add(1),
                            line,
                        )?;
                        result.push(value);
                    }
                    Ok(Value::List(result))
                } else {
                    Err(crate::builtin_core::type_error(
                        line,
                        "map(list, fn) の形式で使います",
                    ))
                }
            }
            "filter" => {
                crate::builtin_core::check_arity(name, &args, 2, line)?;
                if let Value::List(list) = &args[0] {
                    let func = args[1].clone();
                    let mut result = Vec::new();
                    for item in list {
                        let cond = self.call_fn_value(func.clone(), vec![item.clone()], line)?;
                        if cond.is_truthy() {
                            crate::builtin_core::check_collection_size_public(
                                result.len().saturating_add(1),
                                line,
                            )?;
                            result.push(item.clone());
                        }
                    }
                    Ok(Value::List(result))
                } else {
                    Err(crate::builtin_core::type_error(
                        line,
                        "filter(list, fn) の形式で使います",
                    ))
                }
            }
            "each" => {
                crate::builtin_core::check_arity(name, &args, 2, line)?;
                if let Value::List(list) = &args[0] {
                    let func = args[1].clone();
                    for item in list {
                        self.call_fn_value(func.clone(), vec![item.clone()], line)?;
                    }
                    Ok(Value::Null)
                } else {
                    Err(crate::builtin_core::type_error(
                        line,
                        "each(list, fn) の形式で使います",
                    ))
                }
            }
            _ => Err(TsumugiError::runtime(
                line,
                format!("未定義の組み込み関数: {}", name),
            )),
        }
    }

    /// 関数値を呼び出すヘルパー（map/filter/each 用）
    fn call_fn_value(
        &mut self,
        func: Value,
        args: Vec<Value>,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        self.count_step(line)?;
        // 再帰制限チェック（OpCode::Call と同じガードを適用）
        if self.frames.len() >= MAX_CALL_DEPTH {
            return Err(TsumugiError::runtime_with_kind(
                line,
                crate::error::ErrorKind::StackOverflow,
                format!(
                    "スタックオーバーフロー: 再帰が深すぎます (上限: {})",
                    MAX_CALL_DEPTH
                ),
            ));
        }
        let self_value = func.clone();
        if let Value::VmFn {
            arity,
            chunk,
            upvalues,
            ..
        } = func
        {
            if args.len() != arity {
                return Err(TsumugiError::runtime(
                    line,
                    format!(
                        "引数の数が合いません: {}個必要ですが{}個渡されました",
                        arity,
                        args.len()
                    ),
                ));
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
            Err(TsumugiError::runtime(
                line,
                "関数ではない値を呼び出そうとしました",
            ))
        }
    }

    // --- 算術演算 ---

    fn binary_add(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => a
                .checked_add(*b)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー")),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
            (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{}{}", a, b))),
            (Value::Str(a), Value::Error { .. }) => Ok(Value::Str(format!("{}{}", a, right))),
            (Value::Error { .. }, Value::Str(b)) => Ok(Value::Str(format!("{}{}", left, b))),
            _ => Err(TsumugiError::runtime(
                line,
                format!("型エラー: {:?} Add {:?} は計算できません", left, right),
            )),
        }
    }

    fn binary_sub(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => a
                .checked_sub(*b)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー")),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 - b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - *b as f64)),
            _ => Err(TsumugiError::runtime(
                line,
                format!("型エラー: {:?} Sub {:?} は計算できません", left, right),
            )),
        }
    }

    fn binary_mul(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => a
                .checked_mul(*b)
                .map(Value::Int)
                .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー")),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 * b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * *b as f64)),
            _ => Err(TsumugiError::runtime(
                line,
                format!("型エラー: {:?} Mul {:?} は計算できません", left, right),
            )),
        }
    }

    fn binary_div(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(TsumugiError::runtime(line, "ゼロ除算"))
                } else {
                    a.checked_div(*b)
                        .map(Value::Int)
                        .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー"))
                }
            }
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 / b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a / *b as f64)),
            _ => Err(TsumugiError::runtime(
                line,
                format!("型エラー: {:?} Div {:?} は計算できません", left, right),
            )),
        }
    }

    fn binary_mod(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(TsumugiError::runtime(line, "ゼロ除算"))
                } else {
                    a.checked_rem(*b)
                        .map(Value::Int)
                        .ok_or_else(|| TsumugiError::runtime(line, "整数オーバーフロー"))
                }
            }
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a % b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 % b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a % *b as f64)),
            _ => Err(TsumugiError::runtime(
                line,
                format!("型エラー: {:?} Mod {:?} は計算できません", left, right),
            )),
        }
    }

    fn compare_lt(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool((*a as f64) < *b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a < (*b as f64))),
            _ => Err(TsumugiError::runtime(
                line,
                format!("型エラー: {:?} < {:?} は比較できません", left, right),
            )),
        }
    }

    fn compare_gt(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool((*a as f64) > *b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a > (*b as f64))),
            _ => Err(TsumugiError::runtime(
                line,
                format!("型エラー: {:?} > {:?} は比較できません", left, right),
            )),
        }
    }

    fn compare_lteq(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool((*a as f64) <= *b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a <= (*b as f64))),
            _ => Err(TsumugiError::runtime(
                line,
                format!("型エラー: {:?} <= {:?} は比較できません", left, right),
            )),
        }
    }

    fn compare_gteq(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool((*a as f64) >= *b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a >= (*b as f64))),
            _ => Err(TsumugiError::runtime(
                line,
                format!("型エラー: {:?} >= {:?} は比較できません", left, right),
            )),
        }
    }
}

/// コレクションの長さを返す（値は複製しない）
fn value_len(value: &Value, line: usize) -> Result<i64, TsumugiError> {
    match value {
        Value::List(v) => Ok(v.len() as i64),
        Value::Str(s) => Ok(s.chars().count() as i64),
        Value::Dict(m) => Ok(m.len() as i64),
        _ => Err(TsumugiError::runtime(
            line,
            format!("型エラー: {} の長さは取得できません", type_name(value)),
        )),
    }
}

/// 型名を返すヘルパー
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::Str(_) => "Str",
        Value::Bool(_) => "Bool",
        Value::Null => "Null",
        Value::List(_) => "List",
        Value::Dict(_) => "Dict",
        Value::Fn { .. } => "Fn",
        Value::VmFn { .. } => "Fn",
        Value::Error { .. } => "Error",
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
            name: "malformed_recursive".to_string(),
            arity: 0,
            params: Vec::new(),
            chunk: Rc::new(recursive),
            upvalues: Vec::new(),
        };
        let mut main = Chunk::new();
        main.emit_constant(function, 1);
        main.emit(OpCode::Call(0), 1);
        main.emit(OpCode::Return, 1);

        let error = Vm::new(main)
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

        let error = Vm::new(chunk)
            .run()
            .expect_err("calleeのないCallが成功しました");
        assert!(error.message().contains("Call のスタック要素が不足"));
    }

    #[test]
    fn malformed_call_with_overflowing_arg_count_returns_internal_error() {
        let mut chunk = Chunk::new();
        chunk.emit(OpCode::Call(usize::MAX), 1);
        chunk.emit(OpCode::Return, 1);

        let error = Vm::new(chunk)
            .run()
            .expect_err("overflowする引数数のCallが成功しました");
        assert!(error.message().contains("Call の引数数が不正"));
    }
}
