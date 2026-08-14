//! 統合テスト: .tsg ファイルを実行して期待出力と比較するゴールデンテスト

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
    let dir = fixtures_dir();
    let script = dir.join(format!("{}.tsg", name));
    let expected_file = dir.join(format!("{}.expected", name));

    let expected = std::fs::read_to_string(&expected_file)
        .unwrap_or_else(|_| panic!("期待出力ファイルが読めません: {:?}", expected_file));

    let output = Command::new(tsumugi_bin())
        .arg(script.to_str().unwrap())
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        stdout.trim_end(),
        expected.trim_end(),
        "ゴールデンテスト失敗: {}\n--- 実際の出力 ---\n{}\n--- 期待出力 ---\n{}",
        name,
        stdout,
        expected
    );
}

/// エラー系: .tsg を実行して stderr に期待メッセージが含まれることを確認
fn run_error_test(name: &str) {
    let dir = fixtures_dir();
    let script = dir.join(format!("{}.tsg", name));
    let expected_err_file = dir.join(format!("{}.expected_err", name));

    let expected_err = std::fs::read_to_string(&expected_err_file)
        .unwrap_or_else(|_| panic!("期待エラーファイルが読めません: {:?}", expected_err_file));

    let output = Command::new(tsumugi_bin())
        .arg(script.to_str().unwrap())
        .output()
        .expect("tsumugi バイナリの実行に失敗");

    let stderr = String::from_utf8_lossy(&output.stderr);

    for line in expected_err.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        assert!(
            stderr.contains(line),
            "エラーテスト失敗: {}\nstderr に期待文字列が含まれません: {:?}\n--- stderr ---\n{}",
            name,
            line,
            stderr
        );
    }

    assert!(
        !output.status.success(),
        "エラーケースなのに終了コード0で終了しました: {}",
        name
    );
}

// --- 正常系ゴールデンテスト ---

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

// --- エラー系テスト ---

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
fn golden_first_class_fn() {
    run_golden_test("first_class_fn");
}
