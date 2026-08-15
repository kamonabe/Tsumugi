//! 仮想マシン: バイトコード（Chunk）を実行するスタックマシン

use crate::chunk::Chunk;
use crate::error::TsumugiError;
use crate::opcode::OpCode;
use crate::value::Value;

/// スタックベースの仮想マシン
pub struct Vm {
    /// 実行中のチャンク
    chunk: Chunk,

    /// 命令ポインタ（次に実行する命令のインデックス）
    ip: usize,

    /// 値スタック
    stack: Vec<Value>,
}

impl Vm {
    pub fn new(chunk: Chunk) -> Self {
        Vm {
            chunk,
            ip: 0,
            stack: Vec::with_capacity(256),
        }
    }

    /// チャンクを実行する
    pub fn run(&mut self) -> Result<(), TsumugiError> {
        loop {
            if self.ip >= self.chunk.code.len() {
                break;
            }

            let instruction = self.chunk.code[self.ip].clone();
            let line = self.chunk.lines[self.ip];
            self.ip += 1;

            match instruction {
                OpCode::LoadConst(idx) => {
                    let value = self.chunk.constants[idx].clone();
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

                OpCode::Print(arg_count) => {
                    // スタックから arg_count 個の値を取り出す（逆順に注意）
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
                message: format!(
                    "型エラー: {:?} Add {:?} は計算できません",
                    left, right
                ),
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
                message: format!(
                    "型エラー: {:?} Sub {:?} は計算できません",
                    left, right
                ),
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
                message: format!(
                    "型エラー: {:?} Mul {:?} は計算できません",
                    left, right
                ),
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
                message: format!(
                    "型エラー: {:?} Div {:?} は計算できません",
                    left, right
                ),
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
                message: format!(
                    "型エラー: {:?} Mod {:?} は計算できません",
                    left, right
                ),
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
                message: format!(
                    "型エラー: {:?} と {:?} は比較できません",
                    left, right
                ),
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
                message: format!(
                    "型エラー: {:?} と {:?} は比較できません",
                    left, right
                ),
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
                message: format!(
                    "型エラー: {:?} と {:?} は比較できません",
                    left, right
                ),
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
                message: format!(
                    "型エラー: {:?} と {:?} は比較できません",
                    left, right
                ),
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
}
