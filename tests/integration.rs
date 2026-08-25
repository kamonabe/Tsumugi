//! 統合テスト: .tsg ファイルを実行して期待出力と比較するゴールデンテスト
//!
//! ツリーウォーク版（デフォルト）と VM 版（--vm）の両方で同じ fixture を回し、
//! 両実行方式で同じ言語仕様を満たすことを保証する。

use std::path::Path;
use std::process::Command;

/// テストバイナリのパスを取得（Cargo が提供する環境変数を使用）
fn tsumugi_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsumugi")
}

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// 正常系: .tsg を実行して stdout が .expected と一致することを確認
fn run_golden_test(name: &str) {
    run_golden_test_mode(name, false);
}

/// 正常系（VM版）: --vm フラグ付きで実行
fn run_golden_test_vm(name: &str) {
    run_golden_test_mode(name, true);
}

/// 正常系の共通実装
fn run_golden_test_mode(name: &str, use_vm: bool) {
    let dir = fixtures_dir();
    let script = dir.join(format!("{}.tsg", name));
    let expected_file = dir.join(format!("{}.expected", name));

    let expected = std::fs::read_to_string(&expected_file)
        .unwrap_or_else(|_| panic!("期待出力ファイルが読めません: {:?}", expected_file));

    let mut cmd = Command::new(tsumugi_bin());
    if use_vm {
        cmd.arg("--vm");
    }
    cmd.arg(script.to_str().unwrap());

    let output = cmd.output().expect("tsumugi バイナリの実行に失敗");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mode = if use_vm { "VM" } else { "tree-walk" };

    // Windows の CRLF を LF に正規化して比較
    let actual = stdout.replace("\r\n", "\n");
    let expect = expected.replace("\r\n", "\n");

    assert_eq!(
        actual.trim_end(),
        expect.trim_end(),
        "ゴールデンテスト失敗 [{}]: {}\n--- 実際の出力 ---\n{}\n--- 期待出力 ---\n{}",
        mode,
        name,
        actual,
        expect
    );
}

/// エラー系: .tsg を実行して stderr に期待メッセージが含まれることを確認
fn run_error_test(name: &str) {
    run_error_test_mode(name, false);
}

/// エラー系（VM版）
fn run_error_test_vm(name: &str) {
    run_error_test_mode(name, true);
}

/// エラー系の共通実装
fn run_error_test_mode(name: &str, use_vm: bool) {
    let dir = fixtures_dir();
    let script = dir.join(format!("{}.tsg", name));
    let expected_err_file = dir.join(format!("{}.expected_err", name));

    let expected_err = std::fs::read_to_string(&expected_err_file)
        .unwrap_or_else(|_| panic!("期待エラーファイルが読めません: {:?}", expected_err_file));

    let mut cmd = Command::new(tsumugi_bin());
    if use_vm {
        cmd.arg("--vm");
    }
    cmd.arg(script.to_str().unwrap());

    let output = cmd.output().expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n");
    let mode = if use_vm { "VM" } else { "tree-walk" };

    for line in expected_err.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        assert!(
            stderr.contains(line),
            "エラーテスト失敗 [{}]: {}\nstderr に期待文字列が含まれません: {:?}\n--- stderr ---\n{}",
            mode,
            name,
            line,
            stderr
        );
    }

    assert!(
        !output.status.success(),
        "エラーケースなのに終了コード0で終了しました [{}]: {}",
        mode,
        name
    );
}

// =============================================================
// 正常系ゴールデンテスト（ツリーウォーク）
// =============================================================

#[test]
fn golden_hello() {
    run_golden_test("hello");
}

#[test]
fn golden_arithmetic() {
    run_golden_test("arithmetic");
}

#[test]
fn golden_control_flow() {
    run_golden_test("control_flow");
}

#[test]
fn golden_logic() {
    run_golden_test("logic");
}

#[test]
fn golden_assign() {
    run_golden_test("assign");
}

#[test]
fn golden_list_dict() {
    run_golden_test("list_dict");
}

#[test]
fn golden_for_loop() {
    run_golden_test("for_loop");
}

#[test]
fn golden_break_continue() {
    run_golden_test("break_continue");
}

#[test]
fn golden_fizzbuzz() {
    run_golden_test("fizzbuzz");
}

#[test]
fn golden_builtins() {
    run_golden_test("builtins");
}

#[test]
fn golden_file_io() {
    run_golden_test("file_io");
}

#[test]
fn golden_local_utils() {
    run_golden_test("local_utils");
}

#[test]
fn golden_filesystem() {
    run_golden_test("filesystem");
}

#[test]
fn golden_string_utils() {
    run_golden_test("string_utils");
}

#[test]
fn golden_first_class_fn() {
    run_golden_test("first_class_fn");
}

#[test]
fn golden_closure() {
    run_golden_test("closure");
}

#[test]
fn golden_higher_order() {
    run_golden_test("higher_order");
}

#[test]
fn golden_numeric_utils() {
    run_golden_test("numeric_utils");
}

#[test]
fn golden_dict_utils() {
    run_golden_test("dict_utils");
}

// =============================================================
// 正常系ゴールデンテスト（VM）
// =============================================================

#[test]
fn vm_golden_hello() {
    run_golden_test_vm("hello");
}

#[test]
fn vm_golden_arithmetic() {
    run_golden_test_vm("arithmetic");
}

#[test]
fn vm_golden_control_flow() {
    run_golden_test_vm("control_flow");
}

#[test]
fn vm_golden_logic() {
    run_golden_test_vm("logic");
}

#[test]
fn vm_golden_assign() {
    run_golden_test_vm("assign");
}

#[test]
fn vm_golden_list_dict() {
    run_golden_test_vm("list_dict");
}

#[test]
fn vm_golden_for_loop() {
    run_golden_test_vm("for_loop");
}

#[test]
fn vm_golden_break_continue() {
    run_golden_test_vm("break_continue");
}

#[test]
fn vm_golden_fizzbuzz() {
    run_golden_test_vm("fizzbuzz");
}

#[test]
fn vm_golden_builtins() {
    run_golden_test_vm("builtins");
}

#[test]
fn vm_golden_file_io() {
    run_golden_test_vm("file_io");
}

#[test]
fn vm_golden_local_utils() {
    run_golden_test_vm("local_utils");
}

#[test]
fn vm_golden_filesystem() {
    run_golden_test_vm("filesystem");
}

#[test]
fn vm_golden_string_utils() {
    run_golden_test_vm("string_utils");
}

#[test]
fn vm_golden_first_class_fn() {
    run_golden_test_vm("first_class_fn");
}

#[test]
fn vm_golden_closure() {
    run_golden_test_vm("closure");
}

#[test]
fn vm_golden_higher_order() {
    run_golden_test_vm("higher_order");
}

#[test]
fn vm_golden_numeric_utils() {
    run_golden_test_vm("numeric_utils");
}

#[test]
fn vm_golden_dict_utils() {
    run_golden_test_vm("dict_utils");
}

// =============================================================
// エラー系テスト（ツリーウォーク）
// =============================================================

#[test]
fn error_undefined_var() {
    run_error_test("error_undefined_var");
}

#[test]
fn error_assign_undefined() {
    run_error_test("error_assign_undefined");
}

#[test]
fn error_parse() {
    let dir = fixtures_dir();
    let script = dir.join("error_parse.tsg");

    let output = Command::new(tsumugi_bin())
        .arg(script.to_str().unwrap())
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("2行目"),
        "parse error should mention line 2: {}",
        stderr
    );
    assert!(!output.status.success());
}

#[test]
fn error_parse_multi() {
    run_error_test("error_parse_multi");
}

#[test]
fn error_type() {
    let dir = fixtures_dir();
    let script = dir.join("error_type.tsg");

    let output = Command::new(tsumugi_bin())
        .arg(script.to_str().unwrap())
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("型エラー"),
        "should contain type error: {}",
        stderr
    );
    assert!(!output.status.success());
}

#[test]
fn error_zero_division() {
    run_error_test("error_zero_division");
}

#[test]
fn error_wrong_arg_count() {
    run_error_test("error_wrong_arg_count");
}

#[test]
fn error_undefined_fn() {
    run_error_test("error_undefined_fn");
}

#[test]
fn error_break_outside_loop() {
    run_error_test("error_break_outside_loop");
}

#[test]
fn error_continue_outside_loop() {
    run_error_test("error_continue_outside_loop");
}

#[test]
fn error_index_out_of_bounds() {
    run_error_test("error_index_out_of_bounds");
}

#[test]
fn error_dict_key_type() {
    run_error_test("error_dict_key_type");
}

#[test]
fn error_unknown_char() {
    run_error_test("error_unknown_char");
}

#[test]
fn error_stack_trace() {
    run_error_test("error_stack_trace");
}

#[test]
fn error_step_limit() {
    let dir = fixtures_dir();
    let script = dir.join("error_step_limit.tsg");

    let output = Command::new(tsumugi_bin())
        .arg(script.to_str().unwrap())
        .env("TSUMUGI_MAX_STEPS", "100")
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ステップ上限に達しました"),
        "ステップ予算エラーが出ません: {}",
        stderr
    );
    assert!(!output.status.success());
}

#[test]
fn vm_error_step_limit() {
    let dir = fixtures_dir();
    let script = dir.join("error_step_limit.tsg");

    let output = Command::new(tsumugi_bin())
        .arg("--vm")
        .arg(script.to_str().unwrap())
        .env("TSUMUGI_MAX_STEPS", "100")
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ステップ上限に達しました"),
        "[VM] ステップ予算エラーが出ません: {}",
        stderr
    );
    assert!(!output.status.success());
}

// =============================================================
// スタックオーバーフロー（コールフレーム深度制限）テスト
// =============================================================

#[test]
fn error_stack_overflow() {
    run_error_test("error_stack_overflow");
}

#[test]
fn vm_error_stack_overflow() {
    run_error_test_vm("error_stack_overflow");
}

#[test]
fn error_sandbox() {
    let dir = fixtures_dir();
    let script = dir.join("error_sandbox.tsg");

    let output = Command::new(tsumugi_bin())
        .arg(script.to_str().unwrap())
        .env("TSUMUGI_SANDBOX", "/tmp")
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("サンドボックス違反"),
        "サンドボックスエラーが出ません: {}",
        stderr
    );
    assert!(!output.status.success());
}

#[test]
fn vm_error_sandbox() {
    let dir = fixtures_dir();
    let script = dir.join("error_sandbox.tsg");

    let output = Command::new(tsumugi_bin())
        .arg("--vm")
        .arg(script.to_str().unwrap())
        .env("TSUMUGI_SANDBOX", "/tmp")
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("サンドボックス違反"),
        "[VM] サンドボックスエラーが出ません: {}",
        stderr
    );
    assert!(!output.status.success());
}

#[test]
fn error_sandbox_import() {
    // サンドボックス外の実在ファイルを用意してテスト
    let temp_dir = std::env::temp_dir();
    let outside_file = temp_dir.join("tsg_sandbox_import_test.tsg");
    std::fs::write(&outside_file, "print(\"leaked\")").unwrap();

    // スクリプト本体を一時ディレクトリに作成（import先はプロジェクトディレクトリ外）
    let script = temp_dir.join("tsg_sandbox_import_main.tsg");
    std::fs::write(
        &script,
        format!(
            "import \"{}\"",
            outside_file.to_str().unwrap().replace('\\', "/")
        ),
    )
    .unwrap();

    // サンドボックスを fixtures ディレクトリに設定 → 一時ディレクトリの import 先はブロック
    let sandbox_path = fixtures_dir().to_str().unwrap().to_string();

    let output = Command::new(tsumugi_bin())
        .arg(script.to_str().unwrap())
        .env("TSUMUGI_SANDBOX", &sandbox_path)
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("サンドボックス違反"),
        "import のサンドボックスエラーが出ません: {}",
        stderr
    );
    assert!(!output.status.success());

    // cleanup
    std::fs::remove_file(&outside_file).ok();
    std::fs::remove_file(&script).ok();
}

#[test]
fn vm_error_sandbox_import() {
    // サンドボックス外の実在ファイルを用意してテスト
    let temp_dir = std::env::temp_dir();
    let outside_file = temp_dir.join("tsg_sandbox_import_test_vm.tsg");
    std::fs::write(&outside_file, "print(\"leaked\")").unwrap();

    let script = temp_dir.join("tsg_sandbox_import_main_vm.tsg");
    std::fs::write(
        &script,
        format!(
            "import \"{}\"",
            outside_file.to_str().unwrap().replace('\\', "/")
        ),
    )
    .unwrap();

    let sandbox_path = fixtures_dir().to_str().unwrap().to_string();

    let output = Command::new(tsumugi_bin())
        .arg("--vm")
        .arg(script.to_str().unwrap())
        .env("TSUMUGI_SANDBOX", &sandbox_path)
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("サンドボックス違反"),
        "[VM] import のサンドボックスエラーが出ません: {}",
        stderr
    );
    assert!(!output.status.success());

    // cleanup
    std::fs::remove_file(&outside_file).ok();
    std::fs::remove_file(&script).ok();
}

#[test]
fn env_allow_list() {
    let dir = fixtures_dir();
    let script = dir.join("env_allow.tsg");

    // テスト側で制御可能な環境変数を設定して検証
    let output = Command::new(tsumugi_bin())
        .arg(script.to_str().unwrap())
        .env("TSUMUGI_ENV_ALLOW", "TSG_TEST_ALLOWED,TSUMUGI_*")
        .env("TSG_TEST_ALLOWED", "visible_value")
        .env("SECRET_DB_PASS", "hunter2")
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    assert!(
        stdout.contains("allowed_ok: true"),
        "TSG_TEST_ALLOWED should be accessible: {}",
        stdout
    );
    assert!(
        stdout.contains("secret_blocked: true"),
        "SECRET_DB_PASS should be blocked: {}",
        stdout
    );
    assert!(output.status.success());
}

// =============================================================
// エラー系テスト（VM）
// =============================================================

#[test]
fn vm_error_undefined_var() {
    run_error_test_vm("error_undefined_var");
}

#[test]
fn vm_error_assign_undefined() {
    run_error_test_vm("error_assign_undefined");
}

#[test]
fn vm_error_parse() {
    let dir = fixtures_dir();
    let script = dir.join("error_parse.tsg");

    let output = Command::new(tsumugi_bin())
        .arg("--vm")
        .arg(script.to_str().unwrap())
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("2行目"),
        "[VM] parse error should mention line 2: {}",
        stderr
    );
    assert!(!output.status.success());
}

#[test]
fn vm_error_parse_multi() {
    run_error_test_vm("error_parse_multi");
}

#[test]
fn vm_error_type() {
    let dir = fixtures_dir();
    let script = dir.join("error_type.tsg");

    let output = Command::new(tsumugi_bin())
        .arg("--vm")
        .arg(script.to_str().unwrap())
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("型エラー"),
        "[VM] should contain type error: {}",
        stderr
    );
    assert!(!output.status.success());
}

#[test]
fn vm_error_zero_division() {
    run_error_test_vm("error_zero_division");
}

#[test]
fn vm_error_wrong_arg_count() {
    run_error_test_vm("error_wrong_arg_count");
}

#[test]
fn vm_error_undefined_fn() {
    run_error_test_vm("error_undefined_fn");
}

#[test]
fn vm_error_break_outside_loop() {
    run_error_test_vm("error_break_outside_loop");
}

#[test]
fn vm_error_continue_outside_loop() {
    run_error_test_vm("error_continue_outside_loop");
}

#[test]
fn vm_error_index_out_of_bounds() {
    run_error_test_vm("error_index_out_of_bounds");
}

#[test]
fn vm_error_dict_key_type() {
    run_error_test_vm("error_dict_key_type");
}

#[test]
fn vm_error_unknown_char() {
    run_error_test_vm("error_unknown_char");
}

#[test]
fn vm_error_stack_trace() {
    run_error_test_vm("error_stack_trace");
}

// =============================================================
// import テスト（ツリーウォーク）
// =============================================================

#[test]
fn golden_import_basic() {
    run_golden_test("import_basic");
}

#[test]
fn golden_import_nested() {
    run_golden_test("import_nested");
}

#[test]
fn golden_import_circular() {
    run_golden_test("import_circular");
}

#[test]
fn error_import_not_found() {
    run_error_test("error_import_not_found");
}

// =============================================================
// import テスト（VM）
// =============================================================

#[test]
fn vm_golden_import_basic() {
    run_golden_test_vm("import_basic");
}

#[test]
fn vm_golden_import_nested() {
    run_golden_test_vm("import_nested");
}

#[test]
fn vm_golden_import_circular() {
    run_golden_test_vm("import_circular");
}

#[test]
fn vm_error_import_not_found() {
    run_error_test_vm("error_import_not_found");
}

// =============================================================
// try/catch テスト
// =============================================================

#[test]
fn golden_try_catch() {
    run_golden_test("try_catch");
}

#[test]
fn vm_golden_try_catch() {
    run_golden_test_vm("try_catch");
}

// =============================================================
// クロージャ × try/catch 複合テスト
// =============================================================

#[test]
fn golden_closure_try_catch() {
    run_golden_test("closure_try_catch");
}

#[test]
fn vm_golden_closure_try_catch() {
    run_golden_test_vm("closure_try_catch");
}

// =============================================================
// sort() 数値ソート挙動テスト
// =============================================================

#[test]
fn golden_sort_numeric() {
    run_golden_test("sort_numeric");
}

#[test]
fn vm_golden_sort_numeric() {
    run_golden_test_vm("sort_numeric");
}

// =============================================================
// 浮動小数点特殊値テスト
// =============================================================

#[test]
fn golden_float_special() {
    run_golden_test("float_special");
}

#[test]
fn vm_golden_float_special() {
    run_golden_test_vm("float_special");
}

// =============================================================
// f-string テスト
// =============================================================

#[test]
fn golden_fstring() {
    run_golden_test("fstring");
}

#[test]
fn vm_golden_fstring() {
    run_golden_test_vm("fstring");
}

// =============================================================
// 整数オーバーフロー テスト
// =============================================================

#[test]
fn error_integer_overflow() {
    run_error_test("error_integer_overflow");
}

#[test]
fn vm_error_integer_overflow() {
    run_error_test_vm("error_integer_overflow");
}

// =============================================================
// 未閉じ文字列テスト
// =============================================================

#[test]
fn error_unclosed_string() {
    run_error_test("error_unclosed_string");
}

#[test]
fn vm_error_unclosed_string() {
    run_error_test_vm("error_unclosed_string");
}

#[test]
fn error_unclosed_string_eof() {
    run_error_test("error_unclosed_string_eof");
}

#[test]
fn vm_error_unclosed_string_eof() {
    run_error_test_vm("error_unclosed_string_eof");
}

// =============================================================
// 構造化エラーテスト
// =============================================================

#[test]
fn golden_error_structured() {
    run_golden_test("error_structured");
}

#[test]
fn vm_golden_error_structured() {
    run_golden_test_vm("error_structured");
}

// =============================================================
// クロージャ参照キャプチャ（カウンタパターン）テスト
// =============================================================

#[test]
fn golden_closure_counter() {
    run_golden_test("closure_counter");
}

#[test]
fn vm_golden_closure_counter() {
    run_golden_test_vm("closure_counter");
}

// =============================================================
// リグレッションテスト: slice 境界値
// =============================================================

#[test]
fn golden_slice_edge() {
    run_golden_test("slice_edge");
}

#[test]
fn vm_golden_slice_edge() {
    run_golden_test_vm("slice_edge");
}

// =============================================================
// リグレッションテスト: try 内 break/continue/return
// =============================================================

#[test]
fn golden_try_break_continue() {
    run_golden_test("try_break_continue");
}

#[test]
fn vm_golden_try_break_continue() {
    run_golden_test_vm("try_break_continue");
}

// =============================================================
// リグレッションテスト: abs/range オーバーフロー境界値
// =============================================================

#[test]
fn golden_overflow_edge() {
    run_golden_test("overflow_edge");
}

#[test]
fn vm_golden_overflow_edge() {
    run_golden_test_vm("overflow_edge");
}
