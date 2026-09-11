//! 防御的テスト: 不正な `Chunk` を公開APIへ渡してもホストを落とさない（AUD-023）
//!
//! `Vm::new` / `Vm::run_repl_chunk` は任意の `Chunk` を受け取れるため、
//! compilerが生成しない命令列でもRustのindex panicやunwrapへ到達してはいけない。
//! 期待する結果は `internal` 種別の構造化エラーである。
//!
//! ライブラリの公開APIだけを使うため、VM側の実装を差し戻してもこのテストは残る。

use tsumugi::chunk::Chunk;
use tsumugi::opcode::OpCode;
use tsumugi::value::Value;
use tsumugi::verifier::VerifiedChunk;
use tsumugi::vm::Vm;

/// 未検証の raw chunk を防御的経路で VM へ渡すためのヘルパー（REV-006）。
///
/// `VerifiedChunk::from_trusted` は検証をバイパスするため、compiler が生成しない
/// 不正命令列も VM へ届けられる。これらが host panic せず internal error / limit で
/// 有限停止することを検査するのがこのテストの目的である。
fn unverified(chunk: Chunk) -> VerifiedChunk {
    VerifiedChunk::from_trusted(chunk)
}

/// 不正なChunkを実行し、`internal` エラーのメッセージを返す
fn run_expecting_internal_error(label: &str, chunk: Chunk) -> String {
    let error = match Vm::new(unverified(chunk)).run() {
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
        (
            "CallBuiltin",
            OpCode::CallBuiltin(tsumugi::builtin_registry::BuiltinId::Len, 3),
        ),
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

/// `MakeClosure` のプロトタイプ index が範囲外でも panic せず内部エラーになる（REV-005）。
///
/// capture を隣接 opcode 列から逆算する旧方式を廃止し、プロトタイプ表の index を
/// operand とする。空のプロトタイプ表に対する index はここで防御的に拒否する。
#[test]
fn make_closure_with_out_of_range_prototype_returns_internal_error() {
    let mut chunk = Chunk::new();
    // prototypes が空の状態で index 0 を参照する。
    chunk.emit(OpCode::MakeClosure(0), 1);
    chunk.emit(OpCode::Return, 1);

    let message = run_expecting_internal_error("範囲外プロトタイプのMakeClosure", chunk);
    assert!(
        message.contains("MakeClosure のプロトタイプ index が範囲外です"),
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
    let _ = Vm::new(unverified(chunk)).run();
}

#[test]
fn repl_rolls_back_malformed_chunk_and_recovers() {
    let mut vm = Vm::new_repl();

    // LoadConstで一時値を積んだ後に失敗させ、入力開始時の空stackへ戻ることを検証する。
    let mut malformed = Chunk::new();
    malformed.emit_constant(Value::Int(1), 1);
    malformed.emit(OpCode::SetLocal(999), 1);
    malformed.emit(OpCode::Return, 1);

    let error = vm
        .run_repl_chunk(unverified(malformed))
        .expect_err("不正なREPL chunkが成功しました");
    assert_eq!(error.error_type(), "internal");
    assert!(
        error.message().contains("local slotが不正です"),
        "想定外のメッセージ: {}",
        error.message()
    );

    // rollbackされていれば、前の入力が積んだ値をPopできない。
    let mut stack_probe = Chunk::new();
    stack_probe.emit(OpCode::Pop, 2);
    stack_probe.emit(OpCode::Return, 2);
    let error = vm
        .run_repl_chunk(unverified(stack_probe))
        .expect_err("失敗した入力のstack値が次の入力へ漏洩しています");
    assert_eq!(error.error_type(), "internal");

    // 防御エラーが続いても、後続の正常な入力を受け付けられる。
    let mut valid = Chunk::new();
    valid.emit(OpCode::Return, 3);
    vm.run_repl_chunk(unverified(valid))
        .expect("不正な入力の後にREPL VMが回復しませんでした");
}
