//! 仮想マシン: バイトコード（Chunk）を実行するスタックマシン

use crate::chunk::Chunk;
use crate::error::TsumugiError;
use crate::opcode::OpCode;
use crate::value::Value;

/// コールフレーム: 関数呼び出しの状態を保存する
#[derive(Debug)]
struct CallFrame {
    /// この関数の Chunk
    chunk: Chunk,
    /// 命令ポインタ（この関数内の次に実行する命令のインデックス）
    ip: usize,
    /// スタック上のベース位置（この関数のローカル変数 slot 0 に対応）
    base: usize,
}

/// スタックベースの仮想マシン
pub struct Vm {
    /// コールフレームスタック
    frames: Vec<CallFrame>,

    /// 値スタック
    stack: Vec<Value>,
}

impl Vm {
    pub fn new(chunk: Chunk) -> Self {
        let frame = CallFrame {
            chunk,
            ip: 0,
            base: 0,
        };
        Vm {
            frames: vec![frame],
            stack: Vec::with_capacity(256),
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
            // ip を進める（frames の可変参照）
            self.frames.last_mut().unwrap().ip += 1;

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
                                message: format!(
                                    "型エラー: -{} は計算できません",
                                    type_name(&other)
                                ),
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
                    let value =
                        self.stack
                            .last()
                            .cloned()
                            .ok_or_else(|| TsumugiError::Runtime {
                                line,
                                message: "内部エラー: スタックが空です".to_string(),
                            })?;
                    self.stack[base + slot] = value;
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
                    self.frames.last_mut().unwrap().ip = target;
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
                    // スタックトップがリスト
                    let list = self.stack.last_mut().ok_or_else(|| TsumugiError::Runtime {
                        line,
                        message: "内部エラー: スタックが空です".to_string(),
                    })?;
                    if let Value::List(v) = list {
                        v.push(value);
                    } else {
                        return Err(TsumugiError::Runtime {
                            line,
                            message: "内部エラー: ListPush の対象がリストではありません"
                                .to_string(),
                        });
                    }
                }

                OpCode::Call(arg_count) => {
                    // スタック: [..., fn_value, arg0, arg1, ...]
                    let fn_pos = self.stack.len() - 1 - arg_count;
                    let fn_value = self.stack[fn_pos].clone();

                    if let Value::VmFn { arity, chunk, .. } = fn_value {
                        if arg_count != arity {
                            return Err(TsumugiError::Runtime {
                                line,
                                message: format!(
                                    "引数の数が合いません: {}個必要ですが{}個渡されました",
                                    arity, arg_count
                                ),
                            });
                        }
                        // コールフレームを push
                        // base = fn_pos（fn_value 自体が slot 0 = 自己参照用）
                        // 引数は slot 1, 2, ... に対応
                        let base = fn_pos;

                        self.frames.push(CallFrame { chunk, ip: 0, base });
                    } else {
                        return Err(TsumugiError::Runtime {
                            line,
                            message: format!(
                                "関数ではない値を呼び出そうとしました: {:?}",
                                fn_value
                            ),
                        });
                    }
                }

                OpCode::ReturnValue => {
                    // 戻り値をスタックトップから取得
                    let return_value = self.pop(line)?;

                    // 現在のフレームを pop
                    let frame = self.frames.pop().unwrap();

                    // フレームのローカル変数をスタックからクリーンアップ
                    self.stack.truncate(frame.base);

                    // トップレベルからの ReturnValue なら終了
                    if self.frames.is_empty() {
                        return Ok(());
                    }

                    // 戻り値をスタックに積む
                    self.stack.push(return_value);
                }

                OpCode::Return => {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// スタックからpop
    fn pop(&mut self, line: usize) -> Result<Value, TsumugiError> {
        self.stack.pop().ok_or_else(|| TsumugiError::Runtime {
            line,
            message: "内部エラー: スタックが空です".to_string(),
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
                    })
            }
            (Value::Dict(map), Value::Str(key)) => Ok(map.get(key).cloned().unwrap_or(Value::Null)),
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!(
                    "型エラー: {:?} に対して {:?} でインデックスアクセスできません",
                    collection, index
                ),
            }),
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
            }),
        }
    }

    fn binary_div(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(_), Value::Int(0)) => Err(TsumugiError::Runtime {
                line,
                message: "ゼロ除算".to_string(),
            }),
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a / b)),
            (Value::Float(a), Value::Float(b)) => {
                if *b == 0.0 {
                    return Err(TsumugiError::Runtime {
                        line,
                        message: "ゼロ除算".to_string(),
                    });
                }
                Ok(Value::Float(a / b))
            }
            (Value::Int(a), Value::Float(b)) => {
                if *b == 0.0 {
                    return Err(TsumugiError::Runtime {
                        line,
                        message: "ゼロ除算".to_string(),
                    });
                }
                Ok(Value::Float(*a as f64 / b))
            }
            (Value::Float(a), Value::Int(b)) => {
                if *b == 0 {
                    return Err(TsumugiError::Runtime {
                        line,
                        message: "ゼロ除算".to_string(),
                    });
                }
                Ok(Value::Float(a / *b as f64))
            }
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} Div {:?} は計算できません", left, right),
            }),
        }
    }

    fn binary_mod(&self, left: Value, right: Value, line: usize) -> Result<Value, TsumugiError> {
        match (&left, &right) {
            (Value::Int(_), Value::Int(0)) => Err(TsumugiError::Runtime {
                line,
                message: "ゼロ除算".to_string(),
            }),
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
            (Value::Float(a), Value::Float(b)) => {
                if *b == 0.0 {
                    return Err(TsumugiError::Runtime {
                        line,
                        message: "ゼロ除算".to_string(),
                    });
                }
                Ok(Value::Float(a % b))
            }
            (Value::Int(a), Value::Float(b)) => {
                if *b == 0.0 {
                    return Err(TsumugiError::Runtime {
                        line,
                        message: "ゼロ除算".to_string(),
                    });
                }
                Ok(Value::Float(*a as f64 % b))
            }
            (Value::Float(a), Value::Int(b)) => {
                if *b == 0 {
                    return Err(TsumugiError::Runtime {
                        line,
                        message: "ゼロ除算".to_string(),
                    });
                }
                Ok(Value::Float(a % *b as f64))
            }
            _ => Err(TsumugiError::Runtime {
                line,
                message: format!("型エラー: {:?} Mod {:?} は計算できません", left, right),
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
}
