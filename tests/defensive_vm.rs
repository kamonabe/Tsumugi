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

/// REV-006 層1: `Jump(0)` 自己ループが per-instruction 課金で有限 step 停止する。
///
/// 旧モデルでは Jump に課金がなく、`Jump(0)` の自己ループは step 予算を迂回して
/// 無期限実行できた。per-instruction 課金では 1 周ごとに 1 命令以上 dispatch するため
/// 必ず課金され、`limit` error で停止する。
#[test]
fn jump_self_loop_halts_with_limit() {
    let mut chunk = Chunk::new();
    chunk.emit(OpCode::Jump(0), 1); // index 0 へ無条件ジャンプ（自己ループ）
    let mut vm = Vm::new(unverified(chunk));
    vm.set_max_steps(1000);
    let error = vm
        .run()
        .expect_err("Jump(0) 自己ループが停止しませんでした");
    assert_eq!(
        error.error_type(),
        "limit",
        "想定外の種別: {}",
        error.message()
    );
    assert!(
        error.message().contains("ステップ上限に達しました"),
        "想定外のメッセージ: {}",
        error.message()
    );
}

/// REV-006 層1: 後方 `Loop(0)` 迂回ループが per-instruction 課金で有限 step 停止する。
#[test]
fn backward_loop_halts_with_limit() {
    let mut chunk = Chunk::new();
    chunk.emit(OpCode::Loop(0), 1); // index 0 へ後方ジャンプ（自己ループ）
    let mut vm = Vm::new(unverified(chunk));
    vm.set_max_steps(1000);
    let error = vm
        .run()
        .expect_err("Loop(0) 迂回ループが停止しませんでした");
    assert_eq!(
        error.error_type(),
        "limit",
        "想定外の種別: {}",
        error.message()
    );
}

/// REV-006 層1: `PrepareCall` を省いた raw `Call` の再帰ループが有限停止する。
///
/// per-instruction 課金では `Call` 命令自体が 1 step 課金されるため、`PrepareCall` を
/// 迂回しても課金を回避できない。深度上限（先に到達）または step 上限のいずれかで
/// 必ず有限停止し、host panic しない。
#[test]
fn prepare_call_less_recursion_halts() {
    let mut recursive = Chunk::new();
    recursive.name = "raw_recursive".to_string();
    recursive.emit(OpCode::GetLocal(0), 1); // 自身（slot 0）を積む
    recursive.emit(OpCode::Call(0), 1); // PrepareCall なしで自己呼び出し
    recursive.emit(OpCode::ReturnValue, 1);
    recursive.max_locals = 1;

    let mut body = Chunk::new();
    let proto = tsumugi::chunk::FunctionPrototype {
        name: "raw_recursive".to_string(),
        arity: 0,
        params: Vec::new(),
        chunk: std::rc::Rc::new(recursive),
        captures: Vec::new(),
    };
    body.add_prototype(proto);
    body.emit(OpCode::MakeClosure(0), 1);
    body.emit(OpCode::Call(0), 1);
    body.emit(OpCode::Return, 1);
    body.max_locals = 0;

    let mut vm = Vm::new(unverified(body));
    vm.set_max_steps(100_000);
    let error = vm
        .run()
        .expect_err("raw Call の再帰ループが停止しませんでした");
    // 深度上限（overflow）または step 上限（limit）のいずれかで有限停止する。
    assert!(
        matches!(error.error_type(), "overflow" | "limit"),
        "想定外の種別: {} / {}",
        error.error_type(),
        error.message()
    );
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
