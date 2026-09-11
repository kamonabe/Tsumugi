//! バイトコードのチャンク（命令列 + 定数テーブル）

use crate::opcode::{CaptureDesc, OpCode};
use crate::value::Value;
use std::rc::Rc;

/// 関数プロトタイプ（REV-005）。
///
/// `MakeClosure(proto_index)` が参照する、closure 生成の元になる不変情報。
/// capture を隣接 opcode 列ではなく明示記述子で保持する。`chunk` は関数本体の
/// バイトコードで、REV-006 の verifier がプロトタイプ木を再帰的に検査する。
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionPrototype {
    /// 関数名（無名関数は `<lambda>`）。
    pub name: String,
    /// 引数の数。
    pub arity: usize,
    /// 引数名（診断・表示用）。
    pub params: Vec<String>,
    /// 関数本体のバイトコード。
    pub chunk: Rc<Chunk>,
    /// capture する変数セルの明示記述子（隣接 opcode からの逆算を廃止）。
    pub captures: Vec<CaptureDesc>,
}

/// builder（`Chunk` の可変組み立て API）が返すエラー（REV-004）。
///
/// `patch_jump` の不正 offset / 非 jump opcode を panic ではなく `Result` で表す。
/// script からは到達せず、呼び出し元 compiler が内部エラーとして扱う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkBuildError {
    /// 指定 offset が命令列の範囲外だった。
    BadOffset,
    /// 指定 offset の命令が jump 系 opcode ではなかった。
    NotAJump,
}

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

    /// 関数プロトタイプ表（REV-005）。`MakeClosure(proto_index)` が参照する。
    pub prototypes: Vec<FunctionPrototype>,
}

impl Chunk {
    pub fn new() -> Self {
        Chunk {
            name: "<main>".to_string(),
            code: Vec::new(),
            constants: Vec::new(),
            lines: Vec::new(),
            prototypes: Vec::new(),
        }
    }

    /// 関数プロトタイプを追加し、その index を返す（REV-005）。
    pub fn add_prototype(&mut self, prototype: FunctionPrototype) -> usize {
        self.prototypes.push(prototype);
        self.prototypes.len() - 1
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

    /// 仮で発行したジャンプ命令の飛び先を現在位置にパッチする（REV-004）。
    ///
    /// 範囲外 offset は [`ChunkBuildError::BadOffset`]、jump 系でない opcode は
    /// [`ChunkBuildError::NotAJump`] を返す。panic はしない。エラー時は `code` を
    /// 書き換えないため、部分破損を残さない。
    pub fn patch_jump(&mut self, offset: usize) -> Result<(), ChunkBuildError> {
        let target = self.code.len();
        let op = self
            .code
            .get_mut(offset)
            .ok_or(ChunkBuildError::BadOffset)?;
        match op {
            OpCode::Jump(addr)
            | OpCode::JumpIfFalse(addr)
            | OpCode::JumpIfFalseKeep(addr)
            | OpCode::JumpIfTrueKeep(addr)
            | OpCode::JumpIfGlobalDefined(_, addr) => {
                *addr = target;
                Ok(())
            }
            _ => Err(ChunkBuildError::NotAJump),
        }
    }
}
