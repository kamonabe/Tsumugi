//! バイトコードのチャンク（命令列 + 定数テーブル）

use crate::opcode::OpCode;
use crate::value::Value;

/// コンパイル結果を保持する構造体
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// この Chunk に対応する関数名（トップレベルなら "<main>"）
    pub name: String,

    /// 命令列
    pub code: Vec<OpCode>,

    /// 定数テーブル（リテラル値を格納）
    pub constants: Vec<Value>,

    /// 各命令に対応するソース行番号（デバッグ・エラー表示用）
    pub lines: Vec<usize>,
}

impl Chunk {
    pub fn new() -> Self {
        Chunk {
            name: "<main>".to_string(),
            code: Vec::new(),
            constants: Vec::new(),
            lines: Vec::new(),
        }
    }

    /// 命令を追加する
    pub fn emit(&mut self, op: OpCode, line: usize) {
        self.code.push(op);
        self.lines.push(line);
    }

    /// 定数テーブルに値を追加し、そのインデックスを返す
    pub fn add_constant(&mut self, value: Value) -> usize {
        self.constants.push(value);
        self.constants.len() - 1
    }

    /// 定数を追加して LoadConst 命令を発行する便利メソッド
    pub fn emit_constant(&mut self, value: Value, line: usize) {
        let idx = self.add_constant(value);
        self.emit(OpCode::LoadConst(idx), line);
    }

    /// 現在の命令列の長さ（次の命令のインデックス）を返す
    pub fn len(&self) -> usize {
        self.code.len()
    }

    /// ジャンプ命令を仮の値（0）で発行し、その命令のインデックスを返す（後でパッチする）
    pub fn emit_jump(&mut self, op: OpCode, line: usize) -> usize {
        let offset = self.code.len();
        self.emit(op, line);
        offset
    }

    /// 仮で発行したジャンプ命令の飛び先を現在位置にパッチする
    pub fn patch_jump(&mut self, offset: usize) {
        let target = self.code.len();
        match &mut self.code[offset] {
            OpCode::Jump(addr)
            | OpCode::JumpIfFalse(addr)
            | OpCode::JumpIfFalseKeep(addr)
            | OpCode::JumpIfTrueKeep(addr) => {
                *addr = target;
            }
            _ => panic!("patch_jump: ジャンプ命令ではないオフセットが指定されました"),
        }
    }
}
