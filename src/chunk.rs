//! バイトコードのチャンク（命令列 + 定数テーブル）

use crate::opcode::OpCode;
use crate::value::Value;

/// コンパイル結果を保持する構造体
#[derive(Debug, Clone)]
pub struct Chunk {
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
}
