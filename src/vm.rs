//! 仮想マシン: バイトコード（Chunk）を実行するスタックマシン

use std::cell::RefCell;
use std::rc::Rc;

use crate::chunk::Chunk;
use crate::error::TsumugiError;
use crate::opcode::OpCode;
use crate::value::{SharedValue, Value};

/// コールフレーム: 関数呼び出しの状態を保存する
#[derive(Debug)]
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
#[derive(Debug)]
struct TryHandler {
    /// catch ブロックの先頭命令アドレス
    catch_ip: usize,
    /// try 開始時のスタック深さ（エラー時にスタックを巻き戻す）
    stack_depth: usize,
    /// try 開始時のフレーム深さ
    frame_depth: usize,
}

/// スタックベースの仮想マシン
pub struct Vm {
    /// コールフレームスタック
    frames: Vec<CallFrame>,

    /// 値スタック
    stack: Vec<Value>,

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
        // トップレベルフレームを新しいチャンクに差し替える
        // （スタックはそのまま保持 — 前回の locals が残っている）
        let frame = CallFrame {
            chunk: Rc::new(chunk),
            ip: 0,
            base: 0,
            upvalues: Vec::new(),
            locals_cells: Vec::new(),
        };
        // 前のトップレベルフレームがあれば差し替え、なければ追加
        if self.frames.is_empty() {
            self.frames.push(frame);
        } else {
            self.frames[0] = frame;
        }
        // ステップカウンタはリセット（各入力で予算を全額使えるように）
        self.steps = 0;
        self.run_frames(0)?;
        Ok(())
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
                    self.try_handlers.push(TryHandler {
                        catch_ip,
                        stack_depth: self.stack.len(),
                        frame_depth: self.frames.len(),
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
            OpCode::Call(arg_count) => {
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
                let fn_pos = self.stack.len() - 1 - arg_count;
                let fn_value = self.stack[fn_pos].clone();
                if let Value::VmFn {
                    name,
                    arity,
                    chunk,
                    upvalues,
                    ..
                } = fn_value
                {
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
            OpCode::Len => {
                let value = self.pop(line)?;
                let length = match &value {
                    Value::List(v) => v.len() as i64,
                    Value::Str(s) => s.chars().count() as i64,
                    Value::Dict(m) => m.len() as i64,
                    _ => {
                        return Err(TsumugiError::runtime(
                            line,
                            format!("型エラー: {} の長さは取得できません", type_name(&value)),
                        ));
                    }
                };
                self.stack.push(Value::Int(length));
            }
            OpCode::Index => {
                let index = self.pop(line)?;
                let collection = self.pop(line)?;
                let result = self.eval_index(collection, index, line)?;
                self.stack.push(result);
            }
            OpCode::ListPush => {
                let value = self.pop(line)?;
                let list = self
                    .stack
                    .last_mut()
                    .ok_or_else(|| TsumugiError::runtime(line, "内部エラー: スタックが空です"))?;
                if let Value::List(v) = list {
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
            OpCode::SetIndex => {
                let value = self.pop(line)?;
                let index = self.pop(line)?;
                let collection = self.pop(line)?;
                let updated = self.set_index(collection, index, value, line)?;
                self.stack.push(updated);
            }
            OpCode::ToIterList => {
                let value = self.pop(line)?;
                let list = match value {
                    Value::List(_) => value,
                    Value::Dict(ref map) => {
                        Value::List(map.keys().map(|k| Value::Str(k.clone())).collect())
                    }
                    Value::Str(ref s) => {
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

    /// インデックスアクセス
    fn eval_index(
        &self,
        collection: Value,
        index: Value,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        match (&collection, &index) {
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

    /// インデックス代入
    fn set_index(
        &self,
        mut collection: Value,
        index: Value,
        value: Value,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        match (&mut collection, &index) {
            (Value::List(list), Value::Int(i)) => {
                let idx = if *i < 0 {
                    (list.len() as i64 + i) as usize
                } else {
                    *i as usize
                };
                if idx >= list.len() {
                    return Err(TsumugiError::runtime(
                        line,
                        format!("インデックス範囲外: {} (長さ: {})", i, list.len()),
                    ));
                }
                list[idx] = value;
                Ok(collection)
            }
            (Value::Dict(map), Value::Str(key)) => {
                map.insert(key.clone(), value);
                Ok(collection)
            }
            _ => Err(TsumugiError::runtime(
                line,
                "辞書のキーは文字列である必要があります",
            )),
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
                    Ok(_) => Ok(Value::Str(buf.trim_end_matches('\n').to_string())),
                    Err(_) => Ok(Value::Null),
                }
            }
            "exit" => {
                let code = if args.is_empty() {
                    0
                } else if let Value::Int(n) = &args[0] {
                    *n as i32
                } else {
                    0
                };
                std::process::exit(code);
            }
            "args" => {
                let argv: Vec<Value> = std::env::args()
                    .skip(1)
                    .filter(|a| a != "--vm")
                    .skip(1) // スクリプトパスをスキップ
                    .map(Value::Str)
                    .collect();
                Ok(Value::List(argv))
            }
            "map" => {
                crate::builtin_core::check_arity(name, &args, 2, line)?;
                if let Value::List(list) = &args[0] {
                    let func = args[1].clone();
                    let mut result = Vec::new();
                    for item in list {
                        result.push(self.call_fn_value(func.clone(), vec![item.clone()], line)?);
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
            // 関数自身をスタックに積む（slot 0）
            let base = self.stack.len();
            self.stack.push(Value::Null); // slot 0 placeholder
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
