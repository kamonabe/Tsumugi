//! 防御的テスト: 不正な `Chunk` を公開APIへ渡してもホストを落とさない（AUD-023）
//!
//! `Vm::new` / `Vm::run_repl_chunk` は任意の `Chunk` を受け取れるため、
//! compilerが生成しない命令列でもRustのindex panicやunwrapへ到達してはいけない。
//! 期待する結果は `internal` 種別の構造化エラーである。
//!
//! ライブラリの公開APIだけを使うため、VM側の実装を差し戻してもこのテストは残る。

use std::rc::Rc;

use tsumugi::chunk::Chunk;
use tsumugi::opcode::OpCode;
use tsumugi::value::Value;
use tsumugi::vm::Vm;

/// 不正なChunkを実行し、`internal` エラーのメッセージを返す
fn run_expecting_internal_error(label: &str, chunk: Chunk) -> String {
    let error = match Vm::new(chunk).run() {
        Ok(()) => panic!("{label}: 不正な命令列が成功しました"),
        Err(error) => error,
    };
    assert_eq!(
        error.error_type(),
        "internal",
        "{label}: 種別が internal ではありません: {}",
        error.message()
    );
    error.message().to_string()
}

#[test]
fn out_of_range_local_slot_read_returns_internal_error() {
    let mut chunk = Chunk::new();
    chunk.emit(OpCode::GetLocal(999), 1);
    chunk.emit(OpCode::Return, 1);

    let message = run_expecting_internal_error("範囲外のlocal読み取り", chunk);
    assert!(
        message.contains("local slotが不正です"),
        "想定外のメッセージ: {message}"
    );
}

#[test]
fn out_of_range_local_slot_write_returns_internal_error() {
    let mut chunk = Chunk::new();
    chunk.emit_constant(Value::Int(1), 1);
    chunk.emit(OpCode::SetLocal(999), 1);
    chunk.emit(OpCode::Return, 1);

    let message = run_expecting_internal_error("範囲外のlocal書き込み", chunk);
    assert!(
        message.contains("local slotが不正です"),
        "想定外のメッセージ: {message}"
    );
}

#[test]
fn out_of_range_constant_returns_internal_error() {
    let mut chunk = Chunk::new();
    chunk.emit(OpCode::LoadConst(999), 1);
    chunk.emit(OpCode::Return, 1);

    let message = run_expecting_internal_error("範囲外の定数参照", chunk);
    assert!(
        message.contains("定数表の参照が不正です"),
        "想定外のメッセージ: {message}"
    );
}

#[test]
fn out_of_range_upvalue_returns_internal_error() {
    let mut chunk = Chunk::new();
    chunk.emit(OpCode::GetUpvalue(0), 1);
    chunk.emit(OpCode::Return, 1);

    let message = run_expecting_internal_error("upvalueを持たないframeでのGetUpvalue", chunk);
    assert!(
        message.contains("upvalueの参照が不正です"),
        "想定外のメッセージ: {message}"
    );
}

#[test]
fn stack_hungry_operands_return_internal_errors() {
    let cases: [(&str, OpCode); 4] = [
        ("FStrConcat", OpCode::FStrConcat(5)),
        ("PopN", OpCode::PopN(10)),
        ("Print", OpCode::Print(3)),
        ("CallBuiltin", OpCode::CallBuiltin(999, 0)),
    ];

    for (label, op) in cases {
        let mut chunk = Chunk::new();
        chunk.emit(op, 1);
        chunk.emit(OpCode::Return, 1);

        let message = run_expecting_internal_error(label, chunk);
        assert!(
            message.contains("スタックの要素数が不足しています")
                || message.contains("定数表の参照が不正です"),
            "{label}: 想定外のメッセージ: {message}"
        );
    }
}

/// 関数の先頭にMakeClosureがあると、直前の命令を数える計算がunderflowし得る。
///
/// 呼び出し先frameでは `ip` が0から始まる一方、stackには呼び出し元の値が
/// 残っているため、要素数の検査だけでは防げない経路になる。
#[test]
fn make_closure_at_function_start_returns_internal_error() {
    let mut body = Chunk::new();
    body.name = "malformed_closure".to_string();
    body.emit(OpCode::MakeClosure(1), 1);
    body.emit(OpCode::Return, 1);

    let function = Value::VmFn {
        name: "malformed_closure".to_string(),
        arity: 0,
        params: Vec::new(),
        chunk: Rc::new(body),
        upvalues: Vec::new(),
    };
    let mut main = Chunk::new();
    main.emit_constant(function, 1);
    main.emit(OpCode::Call(0), 1);
    main.emit(OpCode::Return, 1);

    let message = run_expecting_internal_error("関数先頭のMakeClosure", main);
    assert!(
        message.contains("MakeClosure の直前にupvalue命令がありません"),
        "想定外のメッセージ: {message}"
    );
}

/// 行番号表が命令列と対応していないChunkでもpanicしない
#[test]
fn missing_line_table_returns_internal_error() {
    let mut chunk = Chunk::new();
    chunk.emit(OpCode::Return, 1);
    chunk.lines.clear();

    let message = run_expecting_internal_error("行番号のないChunk", chunk);
    assert!(message.contains("行番号"), "想定外のメッセージ: {message}");
}

/// `try` 命令が `dispatch` へ直接到達してもpanicしない
#[test]
fn try_opcode_reaching_dispatch_does_not_panic() {
    let mut chunk = Chunk::new();
    chunk.emit(OpCode::SetupTry(0), 1);
    chunk.emit(OpCode::GetLocal(999), 1);
    chunk.emit(OpCode::Return, 1);

    // SetupTry自体はrun_framesが処理する。ハンドラ登録後の不正命令でも
    // catch経路へ入り、host panicにはならない（結果の成否は問わない）。
    let _ = Vm::new(chunk).run();
}
