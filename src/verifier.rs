//! バイトコード検証（REV-006）
//!
//! VM が実行する bytecode を、検証を通過した [`VerifiedChunk`] に限定するための検証層。
//! ホストが構築・改変した bytecode の構造的健全性を compile / link 時に一度だけ検査し、
//! VM 実行時の防御的分岐（AUD-023 の `require_*` ヘルパー）が「起こり得るが正常運用では
//! 到達しない」経路として残る前提を確立する。
//!
//! 停止性（step 予算での必ずの終了）の正本は VM の per-instruction step 課金であり、
//! この verifier ではない。verifier は早期拒否・不変条件の明文化・defense-in-depth を担う。

use crate::chunk::Chunk;
use crate::opcode::{CaptureDesc, MutationTarget, OpCode};

/// 検証を通過した [`Chunk`]（REV-006）。
///
/// 生成は [`verify`] 経由のみ（`inner` は非公開）。VM の公開入口はこの型だけを受け取る。
/// `Clone` は検証状態を保つ（再検証不要）。`PartialEq` は `inner` の等価で定義する。
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedChunk {
    inner: Chunk,
}

impl VerifiedChunk {
    /// 検証済みの `Chunk` への読み取り専用アクセサ。
    ///
    /// VM は code / constants / lines / prototypes をこのアクセサ経由で参照する。
    pub fn chunk(&self) -> &Chunk {
        &self.inner
    }

    /// 検証済み `Chunk` を取り出す（VM が root frame を構築する際に使う）。
    pub fn into_inner(self) -> Chunk {
        self.inner
    }

    /// 検証を skip して `VerifiedChunk` を直接構築する（検証をバイパスする）。
    ///
    /// 正規 compiler 出力（`compile` / `compile_repl_line` の戻り値）は構造的に健全なので、
    /// verify の再走を避けるためこの経路を使う。それ以外（ホスト構築 bytecode）は
    /// 通常 [`verify`] を通す。
    ///
    /// この関数は検証を行わないため、防御的テストや raw bytecode 実験のための
    /// unstable な経路である（`verifier` module 自体が `unstable-bytecode` feature 下でのみ
    /// 公開される）。未検証の chunk を渡しても VM は host panic せず、per-instruction step
    /// 課金と `require_*` 防御により有限停止する（REV-006 層1）。
    pub fn from_trusted(chunk: Chunk) -> Self {
        VerifiedChunk { inner: chunk }
    }
}

/// bytecode 検証の失敗種別（REV-006）。
///
/// 生値・オフセットは保持せず、種別と位置種別のみを表す。script からは到達しないため、
/// エラー文面には operand の生値やスタック内容を含めない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkVerifyError {
    /// 行番号表の長さが命令列と一致しない（V1）。
    BadLineTable,
    /// 定数参照 index が範囲外（V2）。
    BadConstant,
    /// local slot が宣言 local 数の範囲外（V3）。
    BadLocalSlot,
    /// upvalue index が capture 数の範囲外（V4）。
    BadUpvalue,
    /// capture 記述子が親プロトタイプの範囲外（V5）。
    BadCapture,
    /// jump / loop / try target が範囲外（V6）。
    BadJumpTarget,
    /// CallBuiltin の builtin id が registry に存在しない（V7）。
    UnknownBuiltin,
    /// stack operand（arg 数など）が変換不能（V8）。
    BadOperand,
    /// try 構造（SetupTry / TeardownTry の対応）が不正（V9）。
    BadTryStructure,
}

impl ChunkVerifyError {
    /// 内部エラーへ載せる安定した固定文字列（生値を含めない）。
    ///
    /// `internal(line, detail)` の `detail` として使う。第3.4節の
    /// 「VM/Compiler 不変条件違反」に対応する。
    pub fn stable_detail(&self) -> &'static str {
        match self {
            ChunkVerifyError::BadLineTable => {
                "bytecode 検証に失敗しました: 行番号表が命令列と一致しません"
            }
            ChunkVerifyError::BadConstant => "bytecode 検証に失敗しました: 定数参照が範囲外です",
            ChunkVerifyError::BadLocalSlot => {
                "bytecode 検証に失敗しました: local slot が範囲外です"
            }
            ChunkVerifyError::BadUpvalue => {
                "bytecode 検証に失敗しました: upvalue index が範囲外です"
            }
            ChunkVerifyError::BadCapture => {
                "bytecode 検証に失敗しました: capture 記述子が範囲外です"
            }
            ChunkVerifyError::BadJumpTarget => {
                "bytecode 検証に失敗しました: jump target が範囲外です"
            }
            ChunkVerifyError::UnknownBuiltin => {
                "bytecode 検証に失敗しました: 未登録の builtin id です"
            }
            ChunkVerifyError::BadOperand => "bytecode 検証に失敗しました: operand が不正です",
            ChunkVerifyError::BadTryStructure => "bytecode 検証に失敗しました: try 構造が不正です",
        }
    }
}

/// `Chunk` を検証して `VerifiedChunk` へ昇格させる（REV-006）。
///
/// 関数プロトタイプを含め chunk 木の全 code に対して V1〜V9 を検査する。検査は一度だけ行い、
/// 成功なら `VerifiedChunk`、失敗なら最初の違反を `ChunkVerifyError` で返す。
pub fn verify(chunk: Chunk) -> Result<VerifiedChunk, ChunkVerifyError> {
    // top-level chunk は upvalue を持たない。
    verify_chunk(&chunk, 0)?;
    Ok(VerifiedChunk { inner: chunk })
}

/// 1 つの chunk（および子プロトタイプ）を再帰的に検査する。
///
/// `upvalue_count` は、この chunk を参照する親プロトタイプの capture 数（top-level は 0）。
/// 関数本体の `GetUpvalue` / `SetUpvalue` / `SetIndex(Upvalue)` はこの数で範囲検査する（V4）。
fn verify_chunk(chunk: &Chunk, upvalue_count: usize) -> Result<(), ChunkVerifyError> {
    let code_len = chunk.code.len();

    // (V1) 行番号表整合
    if chunk.lines.len() != code_len {
        return Err(ChunkVerifyError::BadLineTable);
    }

    // jump target は「末尾（= 暗黙 return 位置）」を許すため 0..=code_len を有効とする。
    let jump_target_ok = |target: usize| target <= code_len;

    for op in &chunk.code {
        match op {
            // (V2) 定数参照
            OpCode::LoadConst(i) => {
                if *i >= chunk.constants.len() {
                    return Err(ChunkVerifyError::BadConstant);
                }
            }

            // (V3) local slot
            OpCode::GetLocal(s)
            | OpCode::SetLocal(s)
            | OpCode::LenLocal(s)
            | OpCode::IndexLocal(s) => {
                if *s >= chunk.max_locals {
                    return Err(ChunkVerifyError::BadLocalSlot);
                }
            }
            OpCode::RegisterGlobal(_, slot) => {
                if *slot >= chunk.max_locals {
                    return Err(ChunkVerifyError::BadLocalSlot);
                }
            }

            // (V4) upvalue index
            OpCode::GetUpvalue(i) | OpCode::SetUpvalue(i) => {
                if *i >= upvalue_count {
                    return Err(ChunkVerifyError::BadUpvalue);
                }
            }

            // (V3)/(V4) SetIndex は対象 binding 種別で分岐
            OpCode::SetIndex(target) => match target {
                MutationTarget::Local(s) => {
                    if *s >= chunk.max_locals {
                        return Err(ChunkVerifyError::BadLocalSlot);
                    }
                }
                MutationTarget::Upvalue(i) => {
                    if *i >= upvalue_count {
                        return Err(ChunkVerifyError::BadUpvalue);
                    }
                }
                MutationTarget::Global(_) => {}
            },

            // (V6) jump target 範囲
            OpCode::Jump(t)
            | OpCode::JumpIfFalse(t)
            | OpCode::JumpIfFalseKeep(t)
            | OpCode::JumpIfTrueKeep(t)
            | OpCode::Loop(t)
            | OpCode::JumpIfGlobalDefined(_, t) => {
                if !jump_target_ok(*t) {
                    return Err(ChunkVerifyError::BadJumpTarget);
                }
            }

            // (V9) try 構造: SetupTry の catch target が範囲内であること。
            // TeardownTry の対応は、break/continue が余分な TeardownTry を発行し得るため
            // 線形カウントでは検査せず（valid 出力を誤拒否しない）、VM 側は空の
            // try_handlers への TeardownTry を安全に無視する（host panic しない）。
            OpCode::SetupTry(t) => {
                if !jump_target_ok(*t) {
                    return Err(ChunkVerifyError::BadJumpTarget);
                }
            }

            // (V7) builtin id
            OpCode::CallBuiltin(id, _) => {
                if !crate::builtin_registry::is_registered(*id) {
                    return Err(ChunkVerifyError::UnknownBuiltin);
                }
            }

            // (V5) capture 記述子は MakeClosure が参照するプロトタイプ側で検査する。
            // ここでは proto index の範囲を確認する。
            OpCode::MakeClosure(proto_index) => {
                if *proto_index >= chunk.prototypes.len() {
                    return Err(ChunkVerifyError::BadCapture);
                }
            }

            // (V8) stack operand。operand は usize のため型で非負が保証され、
            // 32-bit target でも usize=u32 で表現可能。REV-010 の i64→usize 迂回は
            // ここではなく数値変換側で扱う。現状は追加拒否条件を持たない。
            OpCode::PopN(_)
            | OpCode::Print(_)
            | OpCode::FStrConcat(_)
            | OpCode::Call(_)
            | OpCode::ValidateCall(_) => {}

            // 残りの命令は operand に検証対象の index を持たない。
            _ => {}
        }
    }

    // (V5) 各プロトタイプの capture 記述子が親（この chunk）の範囲内であること。
    // Local(slot) は親の宣言 local 数、Upvalue(index) は親の upvalue 数で検査する。
    for proto in &chunk.prototypes {
        for cap in &proto.captures {
            match cap {
                CaptureDesc::Local(slot) => {
                    if *slot >= chunk.max_locals {
                        return Err(ChunkVerifyError::BadCapture);
                    }
                }
                CaptureDesc::Upvalue(index) => {
                    if *index >= upvalue_count {
                        return Err(ChunkVerifyError::BadCapture);
                    }
                }
            }
        }
        // プロトタイプ木を再帰検査する。子の upvalue 数はこのプロトタイプの capture 数。
        verify_chunk(&proto.chunk, proto.captures.len())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunk, FunctionPrototype};
    use crate::compiler::Compiler;
    use crate::lexer::Lexer;
    use crate::opcode::OpCode;
    use crate::parser::Parser;
    use crate::value::Value;
    use std::rc::Rc;

    /// source を compile して raw Chunk を得る（compiler は VerifiedChunk を返すため中身を取り出す）。
    fn compile(source: &str) -> Chunk {
        let tokens = Lexer::new(source).tokenize();
        let program = Parser::new(tokens).parse().expect("parse に失敗");
        Compiler::new()
            .compile(&program)
            .expect("compile に失敗")
            .into_inner()
    }

    /// V1〜V9 を通る正規 compiler 出力（if/while/for/関数/closure/try/f-string/builtin）。
    #[test]
    fn accepts_normal_compiler_output() {
        let source = r#"
            let total = 0
            for i in range(3)
                if i > 0
                    total = total + i
                end
            end
            fn make_adder(n)
                return fn(x) x + n end
            end
            let add5 = make_adder(5)
            let r = add5(total)
            let name = "count"
            let msg = f"{name}={r}"
            let xs = [1, 2, 3]
            push(xs, 4)
            try
                let bad = xs[100]
            catch e
                print(e)
            end
            while total > 0
                total = total - 1
            end
        "#;
        let chunk = compile(source);
        assert!(
            verify(chunk).is_ok(),
            "正規 compiler 出力は verify を通るべき"
        );
    }

    #[test]
    fn rejects_bad_line_table() {
        let mut chunk = Chunk::new();
        chunk.emit(OpCode::Return, 1);
        chunk.lines.clear();
        assert_eq!(verify(chunk).unwrap_err(), ChunkVerifyError::BadLineTable);
    }

    #[test]
    fn rejects_bad_constant() {
        let mut chunk = Chunk::new();
        chunk.emit(OpCode::LoadConst(0), 1); // constants は空
        chunk.emit(OpCode::Return, 1);
        assert_eq!(verify(chunk).unwrap_err(), ChunkVerifyError::BadConstant);
    }

    #[test]
    fn rejects_bad_local_slot() {
        let mut chunk = Chunk::new();
        chunk.max_locals = 1;
        chunk.emit(OpCode::GetLocal(5), 1);
        chunk.emit(OpCode::Return, 1);
        assert_eq!(verify(chunk).unwrap_err(), ChunkVerifyError::BadLocalSlot);
    }

    #[test]
    fn rejects_bad_upvalue() {
        // upvalue を持たない top-level chunk で GetUpvalue を参照する。
        let mut chunk = Chunk::new();
        chunk.emit(OpCode::GetUpvalue(0), 1);
        chunk.emit(OpCode::Return, 1);
        assert_eq!(verify(chunk).unwrap_err(), ChunkVerifyError::BadUpvalue);
    }

    #[test]
    fn rejects_bad_jump_target() {
        let mut chunk = Chunk::new();
        chunk.emit(OpCode::Jump(999), 1); // code.len() を超える
        chunk.emit(OpCode::Return, 1);
        assert_eq!(verify(chunk).unwrap_err(), ChunkVerifyError::BadJumpTarget);
    }

    #[test]
    fn allows_jump_to_end_of_code() {
        // 末尾（= 暗黙 return 位置）へのジャンプは許可する。
        let mut chunk = Chunk::new();
        chunk.emit(OpCode::Jump(1), 1); // target == code.len()
        assert!(verify(chunk).is_ok());
    }

    #[test]
    fn rejects_bad_capture_proto_index() {
        let mut chunk = Chunk::new();
        chunk.emit(OpCode::MakeClosure(0), 1); // prototypes は空
        chunk.emit(OpCode::Return, 1);
        assert_eq!(verify(chunk).unwrap_err(), ChunkVerifyError::BadCapture);
    }

    #[test]
    fn rejects_bad_capture_descriptor_out_of_range() {
        // capture 記述子 Local(9) が親の max_locals を超える。
        let mut body = Chunk::new();
        body.emit(OpCode::Return, 1);
        let proto = FunctionPrototype {
            name: "f".to_string(),
            arity: 0,
            params: Vec::new(),
            chunk: Rc::new(body),
            captures: vec![CaptureDesc::Local(9)],
        };
        let mut chunk = Chunk::new();
        chunk.max_locals = 1;
        chunk.add_prototype(proto);
        chunk.emit(OpCode::MakeClosure(0), 1);
        chunk.emit(OpCode::Return, 1);
        assert_eq!(verify(chunk).unwrap_err(), ChunkVerifyError::BadCapture);
    }

    #[test]
    fn recurses_into_prototype_bodies() {
        // 子プロトタイプ本体に不正な定数参照があれば検出する。
        let mut body = Chunk::new();
        body.emit(OpCode::LoadConst(0), 1); // 子 chunk の constants は空
        body.emit(OpCode::Return, 1);
        let proto = FunctionPrototype {
            name: "f".to_string(),
            arity: 0,
            params: Vec::new(),
            chunk: Rc::new(body),
            captures: Vec::new(),
        };
        let mut chunk = Chunk::new();
        chunk.add_prototype(proto);
        chunk.emit(OpCode::MakeClosure(0), 1);
        chunk.emit(OpCode::Return, 1);
        assert_eq!(verify(chunk).unwrap_err(), ChunkVerifyError::BadConstant);
    }

    #[test]
    fn child_upvalue_bounded_by_prototype_capture_count() {
        // 子本体の GetUpvalue(0) は、親プロトタイプの capture 数 1 で許可される。
        let mut body = Chunk::new();
        body.emit(OpCode::GetUpvalue(0), 1);
        body.emit(OpCode::ReturnValue, 1);
        let proto = FunctionPrototype {
            name: "f".to_string(),
            arity: 0,
            params: Vec::new(),
            chunk: Rc::new(body),
            captures: vec![CaptureDesc::Local(0)],
        };
        let mut chunk = Chunk::new();
        chunk.max_locals = 1;
        chunk.add_prototype(proto);
        chunk.emit(OpCode::MakeClosure(0), 1);
        chunk.emit(OpCode::Return, 1);
        assert!(verify(chunk).is_ok());
    }

    #[test]
    fn constant_containing_string_verifies() {
        // 定数に文字列を載せた LoadConst は範囲内なら通る。
        let mut chunk = Chunk::new();
        let idx = chunk.add_constant(Value::Str("ok".to_string()));
        chunk.emit(OpCode::LoadConst(idx), 1);
        chunk.emit(OpCode::Pop, 1);
        chunk.emit(OpCode::Return, 1);
        assert!(verify(chunk).is_ok());
    }
}
