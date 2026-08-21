//! 仮想マシン: バイトコード（Chunk）を実行するスタックマシン

use std::rc::Rc;

use crate::chunk::Chunk;
use crate::error::TsumugiError;
use crate::opcode::OpCode;
use crate::value::Value;

/// コールフレーム: 関数呼び出しの状態を保存する
#[derive(Debug)]
struct CallFrame {
    /// この関数の Chunk（Rc で共有）
    chunk: Rc<Chunk>,
    /// 命令ポインタ（この関数内の次に実行する命令のインデックス）
    ip: usize,
    /// スタック上のベース位置（この関数のローカル変数 slot 0 に対応）
    base: usize,
    /// この関数がキャプチャした値（クロージャ用）
    upvalues: Vec<Value>,
}

/// デフォルトのステップ上限（100万）
const DEFAULT_MAX_STEPS: u64 = 1_000_000;

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
        };
        Vm {
            frames: vec![frame],
            stack: Vec::with_capacity(256),
            steps: 0,
            max_steps: vm_resolve_max_steps(),
            try_handlers: Vec::new(),
        }
    }

    /// チャンクを実行する
    pub fn run(&mut self) -> Result<(), TsumugiError> {
        loop {
            let frame = self.frames.last().unwrap();
            if frame.ip >= frame.chunk.code.len() {
                break;
            }

            let instruction = frame.chunk.code[frame.ip].clone();
            let line = frame.chunk.lines[frame.ip];
            self.frames.last_mut().unwrap().ip += 1;

            let result = match &instruction {
                OpCode::ReturnValue => {
                    let return_value = self.pop(line)?;
                    let frame = self.frames.pop().unwrap();
                    self.stack.truncate(frame.base);
                    if self.frames.is_empty() {
                        return Ok(());
                    }
                    self.stack.push(return_value);
                    Ok(())
                }
                OpCode::Return => {
                    return Ok(());
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
                    // フレームを巻き戻す
                    self.frames.truncate(handler.frame_depth);
                    // スタックを巻き戻す
                    self.stack.truncate(handler.stack_depth);
                    // エラーメッセージをスタックに積む
                    let error_msg = e.message().to_string();
                    self.stack.push(Value::Str(error_msg));
                    // catch ブロックへジャンプ
                    self.frames.last_mut().unwrap().ip = handler.catch_ip;
                } else {
                    return Err(self.attach_trace(e));
                }
            }
        }
        Ok(())
    }

    /// ステップカウンタを進め、上限チェックする
    fn count_step(&mut self, line: usize) -> Result<(), TsumugiError> {
        self.steps += 1;
        if self.steps > self.max_steps {
            return Err(TsumugiError::Runtime {
                line,
                message: format!("ステップ上限に達しました (上限: {})", self.max_steps),
                trace: Vec::new(),
            });
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
        // frames: [<main>, calc, divide] のとき、エラーは divide で発生
        // 表示:
        //   "in divide() (6行目)"  ← divide が呼ばれた行（calc内のCall命令の行）
        //   "in calc() (9行目)"    ← calc が呼ばれた行（<main>内のCall命令の行）
        //
        // frames[i+1] の関数名と、frames[i] の ip-1 の行番号を組み合わせる
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
                    Value::Int(n) => Value::Int(-n),
                    Value::Float(n) => Value::Float(-n),
                    other => {
                        return Err(TsumugiError::Runtime {
                            line,
                            message: format!("型エラー: -{} は計算できません", type_name(&other)),
                            trace: Vec::new(),
                        });
                    }
                };
                self.stack.push(result);
            }
            OpCode::GetLocal(slot) => {
                let base = self.frames.last().unwrap().base;
                let value = self.stack[base + slot].clone();
                self.stack.push(value);
            }
            OpCode::SetLocal(slot) => {
                let base = self.frames.last().unwrap().base;
                let value = self
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| TsumugiError::Runtime {
                        line,
                        message: "内部エラー: スタックが空です".to_string(),
                        trace: Vec::new(),
                    })?;
                self.stack[base + slot] = value;
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
            OpCode::Loop(target) => {
                self.count_step(line)?;
                self.frames.last_mut().unwrap().ip = target;
            }
            OpCode::GetUpvalue(index) => {
                let value = self.frames.last().unwrap().upvalues[index].clone();
                self.stack.push(value);
            }
            OpCode::MakeClosure(upvalue_count) => {
                let mut upvalues = Vec::with_capacity(upvalue_count);
                for _ in 0..upvalue_count {
                    upvalues.push(self.pop(line)?);
                }
                upvalues.reverse();
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
                        chunk, // Rc::clone は自動（Value の Clone 実装経由）
                        upvalues,
                    });
                } else {
                    return Err(TsumugiError::Runtime {
                        line,
                        message: "内部エラー: MakeClosure の対象が VmFn ではありません".to_string(),
                        trace: Vec::new(),
                    });
                }
            }
            OpCode::Call(arg_count) => {
                self.count_step(line)?;
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
                        return Err(TsumugiError::Runtime {
                            line,
                            message: format!(
                                "関数 {} は引数{}個ですが、{}個渡されました",
                                name, arity, arg_count
                            ),
                            trace: Vec::new(),
                        });
                    }
                    let base = fn_pos;
                    self.frames.push(CallFrame {
                        chunk, // Rc<Chunk> — ポインタコピーのみ
                        ip: 0,
                        base,
                        upvalues,
                    });
                } else {
                    return Err(TsumugiError::Runtime {
                        line,
                        message: format!("関数ではない値を呼び出そうとしました: {:?}", fn_value),
                        trace: Vec::new(),
                    });
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
                self.pop(line)?;
            }
            OpCode::PopN(count) => {
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
                        return Err(TsumugiError::Runtime {
                            line,
                            message: format!(
                                "型エラー: {} の長さは取得できません",
                                type_name(&value)
                            ),
                            trace: Vec::new(),
                        });
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
                let list = self.stack.last_mut().ok_or_else(|| TsumugiError::Runtime {
                    line,
                    message: "内部エラー: スタックが空です".to_string(),
                    trace: Vec::new(),
                })?;
                if let Value::List(v) = list {
                    v.push(value);
                } else {
                    return Err(TsumugiError::Runtime {
                        line,
                        message: "内部エラー: ListPush の対象がリストではありません".to_string(),
                        trace: Vec::new(),
                    });
                }
            }
            OpCode::DictInsert => {
                let value = self.pop(line)?;
                let key = self.pop(line)?;
                let dict = self.stack.last_mut().ok_or_else(|| TsumugiError::Runtime {
                    line,
                    message: "内部エラー: スタックが空です".to_string(),
                    trace: Vec::new(),
                })?;
                if let Value::Dict(map) = dict {
                    if let Value::Str(k) = key {
                        map.insert(k, value);
                    } else {
                        return Err(TsumugiError::Runtime {
                            line,
                            message: "辞書のキーは文字列である必要があります".to_string(),
                            trace: Vec::new(),
                        });
                    }
                } else {
                    return Err(TsumugiError::Runtime {
                        line,
                        message: "内部エラー: DictInsert の対象が辞書ではありません".to_string(),
                        trace: Vec::new(),
                    });
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
                        return Err(TsumugiError::Runtime {
                            line,
                            message: format!("型エラー: {:?} はイテレートできません", value),
                            trace: Vec::new(),
                        });
                    }
                };
                self.stack.push(list);
            }
            OpCode::CallBuiltin(ref name, arg_count) => {
                let mut args = Vec::with_capacity(arg_count);
                for _ in 0..arg_count {
                    args.push(self.pop(line)?);
                }
                args.reverse();
                let result = self.exec_builtin(name, args, line)?;
                self.stack.push(result);
            }
            OpCode::ReturnValue | OpCode::Return => {
                // これらは run() / call_fn_value で処理済み、ここに来ない
                unreachable!()
            }
            OpCode::SetupTry(_) | OpCode::TeardownTry => {
                // これらは run() で処理済み、ここに来ない
                unreachable!()
            }
        }
        Ok(())
    }

    /// スタックからpop
    fn pop(&mut self, line: usize) -> Result<Value, TsumugiError> {
        self.stack.pop().ok_or_else(|| TsumugiError::Runtime {
            line,
            message: "内部エラー: スタックが空です".to_string(),
            trace: Vec::new(),
        })
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
                list.get(idx).cloned().ok_or_else(|| TsumugiError::Runtime {
                    line,
                    message: format!("インデックス範囲外: {} (長さ: {})", i, list.len()),
                    trace: Vec::new(),
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
                    .ok_or_else(|| TsumugiError::Runtime {
                        line,
                        message: format!("インデックス範囲外: {} (長さ: {})", i, chars.len()),
                        trace: Vec::new(),
                    })
            }
            (Value::Dict(map), Value::Str(key)) => Ok(map.get(key).cloned().unwrap_or(Value::Null)),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!(
                    "型エラー: {:?} に対して {:?} でインデックスアクセスできません",
                    collection, index
                ),
                trace: Vec::new(),
            }),
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
                    return Err(TsumugiError::Runtime {
                        line,
                        message: format!("インデックス範囲外: {} (長さ: {})", i, list.len()),
                        trace: Vec::new(),
                    });
                }
                list[idx] = value;
                Ok(collection)
            }
            (Value::Dict(map), Value::Str(key)) => {
                map.insert(key.clone(), value);
                Ok(collection)
            }
            _ => Err(TsumugiError::Runtime {
                line,
                message: "辞書のキーは文字列である必要があります".to_string(),
                trace: Vec::new(),
            }),
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
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("未定義の組み込み関数: {}", name),
                trace: Vec::new(),
            }),
        }
    }

    /// 関数値を呼び出すヘルパー（map/filter/each 用）
    fn call_fn_value(
        &mut self,
        func: Value,
        args: Vec<Value>,
        line: usize,
    ) -> Result<Value, TsumugiError> {
        if let Value::VmFn {
            arity,
            chunk,
            upvalues,
            ..
        } = func
        {
            if args.len() != arity {
                return Err(TsumugiError::Runtime {
                    line,
                    message: format!(
                        "引数の数が合いません: {}個必要ですが{}個渡されました",
                        arity,
                        args.len()
                    ),
                    trace: Vec::new(),
                });
            }
            // 関数自身をスタックに積む（slot 0）
            let base = self.stack.len();
            self.stack.push(Value::Null); // slot 0 placeholder
            for arg in args {
                self.stack.push(arg);
            }
            let target_depth = self.frames.len();
            self.frames.push(CallFrame {
                chunk, // Rc<Chunk> — ポインタコピーのみ
                ip: 0,
                base,
                upvalues,
            });
            // メインループと同じ構造で実行（ReturnValue でフレームが target_depth に戻ったら終了）
            loop {
                let frame = self.frames.last().unwrap();
                if frame.ip >= frame.chunk.code.len() {
                    // フレームの命令が尽きた = 暗黙 null return
                    let f = self.frames.pop().unwrap();
                    self.stack.truncate(f.base);
                    if self.frames.len() <= target_depth {
                        return Ok(Value::Null);
                    }
                    self.stack.push(Value::Null);
                    continue;
                }

                let instruction = frame.chunk.code[frame.ip].clone();
                let instr_line = frame.chunk.lines[frame.ip];
                self.frames.last_mut().unwrap().ip += 1;

                match &instruction {
                    OpCode::ReturnValue => {
                        let return_value = self.pop(instr_line)?;
                        let f = self.frames.pop().unwrap();
                        self.stack.truncate(f.base);
                        if self.frames.len() <= target_depth {
                            return Ok(return_value);
                        }
                        self.stack.push(return_value);
                    }
                    OpCode::Return => {
                        // トップレベル Return = 通常は起きないがガード
                        let f = self.frames.pop().unwrap();
                        self.stack.truncate(f.base);
                        return Ok(Value::Null);
                    }
                    _ => {
                        self.dispatch(instruction, instr_line)?;
                    }
                }
            }
        } else {
            Err(TsumugiError::Runtime {
                line,
                message: "関数ではない値を呼び出そうとしました".to_string(),
                trace: Vec::new(),
            })
        }
    }

    // --- 算術演算 ---

    fn binary_add(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
            (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{}{}", a, b))),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} Add {:?} は計算できません", left, right),
                trace: Vec::new(),
            }),
        }
    }

    fn binary_sub(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 - b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - *b as f64)),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} Sub {:?} は計算できません", left, right),
                trace: Vec::new(),
            }),
        }
    }

    fn binary_mul(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 * b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * *b as f64)),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} Mul {:?} は計算できません", left, right),
                trace: Vec::new(),
            }),
        }
    }

    fn binary_div(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(_), Value::Int(0)) => Err(TsumugiError::Runtime {
                line,
                message: "ゼロ除算".to_string(),
                trace: Vec::new(),
            }),
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a / b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 / b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a / *b as f64)),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} Div {:?} は計算できません", left, right),
                trace: Vec::new(),
            }),
        }
    }

    fn binary_mod(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(_), Value::Int(0)) => Err(TsumugiError::Runtime {
                line,
                message: "ゼロ除算".to_string(),
                trace: Vec::new(),
            }),
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a % b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 % b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a % *b as f64)),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} Mod {:?} は計算できません", left, right),
                trace: Vec::new(),
            }),
        }
    }

    // --- 比較演算 ---

    fn compare_lt(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool((*a as f64) < *b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a < *b as f64)),
            (Value::Str(a), Value::Str(b)) => Ok(Value::Bool(a < b)),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} と {:?} は比較できません", left, right),
                trace: Vec::new(),
            }),
        }
    }

    fn compare_gt(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool(*a as f64 > *b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a > *b as f64)),
            (Value::Str(a), Value::Str(b)) => Ok(Value::Bool(a > b)),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} と {:?} は比較できません", left, right),
                trace: Vec::new(),
            }),
        }
    }

    fn compare_lteq(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool(*a as f64 <= *b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a <= *b as f64)),
            (Value::Str(a), Value::Str(b)) => Ok(Value::Bool(a <= b)),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} と {:?} は比較できません", left, right),
                trace: Vec::new(),
            }),
        }
    }

    fn compare_gteq(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool(*a as f64 >= *b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a >= *b as f64)),
            (Value::Str(a), Value::Str(b)) => Ok(Value::Bool(a >= b)),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} と {:?} は比較できません", left, right),
                trace: Vec::new(),
            }),
        }
    }
}

/// 型名を返すヘルパー
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::Str(_) => "Str",
        Value::Bool(_) => "Bool",
        Value::Null => "Null",
        Value::List(_) => "List",
        Value::Dict(_) => "Dict",
        Value::Fn { .. } => "Fn",
        Value::VmFn { .. } => "Fn",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Compiler;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    /// ソースをコンパイル → VM実行（print出力はキャプチャできないので、
    /// エラーなく完走することを確認する）
    fn run_vm(source: &str) -> Result<(), TsumugiError> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse()?;
        let chunk = Compiler::new().compile(&program)?;
        let mut vm = Vm::new(chunk);
        vm.run()
    }

    #[test]
    fn vm_print_int() {
        assert!(run_vm("print(42)").is_ok());
    }

    #[test]
    fn vm_arithmetic() {
        assert!(run_vm("print(1 + 2)").is_ok());
        assert!(run_vm("print(10 - 3)").is_ok());
        assert!(run_vm("print(4 * 5)").is_ok());
        assert!(run_vm("print(10 / 3)").is_ok());
        assert!(run_vm("print(10 % 3)").is_ok());
    }

    #[test]
    fn vm_string_concat() {
        assert!(run_vm(r#"print("hello, " + "world")"#).is_ok());
    }

    #[test]
    fn vm_nested_arithmetic() {
        assert!(run_vm("print((1 + 2) * (3 + 4))").is_ok());
    }

    #[test]
    fn vm_negate() {
        assert!(run_vm("print(-42)").is_ok());
    }

    #[test]
    fn vm_comparison() {
        assert!(run_vm("print(1 < 2)").is_ok());
        assert!(run_vm("print(3 > 1)").is_ok());
        assert!(run_vm("print(1 == 1)").is_ok());
        assert!(run_vm("print(1 != 2)").is_ok());
    }

    #[test]
    fn vm_zero_division_error() {
        let result = run_vm("print(1 / 0)");
        assert!(result.is_err());
    }

    #[test]
    fn vm_type_error() {
        let result = run_vm(r#"print("hello" + 1)"#);
        assert!(result.is_err());
    }

    #[test]
    fn vm_multiple_prints() {
        assert!(run_vm("print(1)\nprint(2)\nprint(3)").is_ok());
    }

    #[test]
    fn vm_print_multiple_args() {
        assert!(run_vm(r#"print("hello", "world")"#).is_ok());
    }

    // --- Phase 2: 変数 ---

    #[test]
    fn vm_let_and_print() {
        assert!(run_vm("let x = 42\nprint(x)").is_ok());
    }

    #[test]
    fn vm_let_multiple_vars() {
        assert!(run_vm("let a = 10\nlet b = 20\nprint(a + b)").is_ok());
    }

    #[test]
    fn vm_assign() {
        assert!(run_vm("let x = 1\nx = 99\nprint(x)").is_ok());
    }

    #[test]
    fn vm_assign_arithmetic() {
        assert!(run_vm("let x = 10\nx = x + 5\nprint(x)").is_ok());
    }

    #[test]
    fn vm_multiple_assigns() {
        assert!(run_vm("let a = 1\nlet b = 2\na = a + b\nb = a * 2\nprint(a, b)").is_ok());
    }

    #[test]
    fn vm_undefined_var_error() {
        let result = run_vm("print(z)");
        assert!(result.is_err());
    }

    // --- Phase 3: 制御フロー ---

    #[test]
    fn vm_if_true() {
        assert!(run_vm("let x = 10\nif x > 5\n    print(x)\nend").is_ok());
    }

    #[test]
    fn vm_if_else() {
        assert!(
            run_vm("let x = 3\nif x > 5\n    print(\"big\")\nelse\n    print(\"small\")\nend")
                .is_ok()
        );
    }

    #[test]
    fn vm_if_elif() {
        assert!(run_vm("let x = 5\nif x > 10\n    print(\"a\")\nelif x > 3\n    print(\"b\")\nelse\n    print(\"c\")\nend").is_ok());
    }

    #[test]
    fn vm_while_loop() {
        assert!(run_vm("let i = 3\nwhile i > 0\n    print(i)\n    i = i - 1\nend").is_ok());
    }

    #[test]
    fn vm_while_break() {
        assert!(run_vm("let i = 0\nwhile true\n    i = i + 1\n    if i == 3\n        break\n    end\nend\nprint(i)").is_ok());
    }

    #[test]
    fn vm_while_continue() {
        assert!(run_vm("let i = 0\nwhile i < 5\n    i = i + 1\n    if i == 3\n        continue\n    end\n    print(i)\nend").is_ok());
    }

    #[test]
    fn vm_for_loop() {
        assert!(run_vm("for x in [1, 2, 3]\n    print(x)\nend").is_ok());
    }

    #[test]
    fn vm_for_break() {
        assert!(
            run_vm(
                "for x in [1, 2, 3, 4, 5]\n    if x == 3\n        break\n    end\n    print(x)\nend"
            )
            .is_ok()
        );
    }

    #[test]
    fn vm_for_continue() {
        assert!(run_vm("for x in [1, 2, 3, 4, 5]\n    if x == 3\n        continue\n    end\n    print(x)\nend").is_ok());
    }

    #[test]
    fn vm_and_or() {
        assert!(run_vm("print(true and false)\nprint(true or false)").is_ok());
    }

    #[test]
    fn vm_nested_loops() {
        assert!(
            run_vm("for i in [1, 2]\n    for j in [10, 20]\n        print(i, j)\n    end\nend")
                .is_ok()
        );
    }

    // --- Phase 4: 関数 ---

    #[test]
    fn vm_fn_basic() {
        assert!(run_vm("fn add(a, b)\n    return a + b\nend\nprint(add(3, 4))").is_ok());
    }

    #[test]
    fn vm_fn_no_return() {
        assert!(run_vm("fn greet()\n    print(\"hello\")\nend\ngreet()").is_ok());
    }

    #[test]
    fn vm_fn_recursive() {
        assert!(
            run_vm("fn fib(n)\n    if n <= 1\n        return n\n    end\n    return fib(n - 1) + fib(n - 2)\nend\nprint(fib(10))").is_ok()
        );
    }

    #[test]
    fn vm_fn_multiple_calls() {
        assert!(
            run_vm("fn double(x)\n    return x * 2\nend\nprint(double(3))\nprint(double(5))")
                .is_ok()
        );
    }

    #[test]
    fn vm_fn_wrong_arity() {
        let result = run_vm("fn add(a, b)\n    return a + b\nend\nadd(1)");
        assert!(result.is_err());
    }

    #[test]
    fn vm_fn_call_non_function() {
        let result = run_vm("let x = 42\nx()");
        assert!(result.is_err());
    }

    // --- Phase 5: クロージャ ---

    #[test]
    fn vm_closure_make_adder() {
        assert!(run_vm(
            "fn make_adder(n)\n    return fn(x) return x + n end\nend\nlet add5 = make_adder(5)\nprint(add5(3))"
        ).is_ok());
    }

    #[test]
    fn vm_closure_value_capture() {
        // 値キャプチャ: 定義後に元の変数を変更してもクロージャには影響しない
        assert!(
            run_vm(
                "let base = 10\nlet adder = fn(x) return x + base end\nbase = 999\nprint(adder(1))"
            )
            .is_ok()
        );
    }

    #[test]
    fn vm_lambda_inline() {
        assert!(
            run_vm(
                "fn apply(func, val)\n    return func(val)\nend\nprint(apply(fn(x) x * x end, 6))"
            )
            .is_ok()
        );
    }

    #[test]
    fn vm_closure_multiple_calls() {
        assert!(run_vm(
            "fn make_adder(n)\n    return fn(x) return x + n end\nend\nlet add3 = make_adder(3)\nlet add7 = make_adder(7)\nprint(add3(1))\nprint(add7(1))"
        ).is_ok());
    }
}
