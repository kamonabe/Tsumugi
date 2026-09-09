//! AUD-036: lossy な数値・OS 境界変換の検証（semantic-decisions.md §10）。
//!
//! Float→Int 変換（`to_int` / `floor` / `ceil` / `round`）と `file_size` の u64→i64
//! 変換で、NaN・±Infinity・i64 範囲外・境界値を lossy な `as` cast に頼らず検査する。
//! tree/VM はどちらも AUD-049 の共有 handler（`builtin_core`）へ委譲するため、変換
//! 意味論はこの共有 handler のユニットテストで両 engine を覆う。error の tree/VM 一致は
//! `canonical_error_inventory` で別途固定する。
//!
//! Float の丸めモード（§10.1）:
//! - `to_int`: 0 方向へ切り捨て
//! - `floor`:  負の無限大方向
//! - `ceil`:   正の無限大方向
//! - `round`:  最も近い整数、中間は 0 から遠い側

use tsumugi::builtin_core::{
    builtin_ceil, builtin_file_size, builtin_floor, builtin_round, builtin_to_int,
};
use tsumugi::error::ErrorKind;
use tsumugi::value::Value;

type Handler = fn(&[Value], usize) -> Result<Value, tsumugi::error::TsumugiError>;

fn call_int(handler: Handler, value: f64) -> i64 {
    match handler(&[Value::Float(value)], 1) {
        Ok(Value::Int(n)) => n,
        other => panic!("Int が返るはず: {other:?}"),
    }
}

fn expect_conversion_error(handler: Handler, value: f64, expected_message: &str) {
    let error = handler(&[Value::Float(value)], 3).expect_err("conversion エラーになるはず");
    assert_eq!(error.kind(), Some(ErrorKind::Conversion));
    assert_eq!(error.message(), expected_message);
    assert_eq!(error.line(), 3, "line は call 式の行");
}

// -----------------------------------------------------------------------------
// 正常系: 各丸めモード
// -----------------------------------------------------------------------------

#[test]
fn to_int_truncates_toward_zero() {
    assert_eq!(call_int(builtin_to_int, 0.0), 0);
    assert_eq!(call_int(builtin_to_int, 0.5), 0);
    assert_eq!(call_int(builtin_to_int, -0.5), 0);
    assert_eq!(call_int(builtin_to_int, 1.5), 1);
    assert_eq!(call_int(builtin_to_int, -1.5), -1);
    assert_eq!(call_int(builtin_to_int, 2.9), 2);
    assert_eq!(call_int(builtin_to_int, -2.9), -2);
}

#[test]
fn negative_zero_becomes_zero() {
    assert_eq!(call_int(builtin_to_int, -0.0), 0);
    assert_eq!(call_int(builtin_floor, -0.0), 0);
    assert_eq!(call_int(builtin_ceil, -0.0), 0);
    assert_eq!(call_int(builtin_round, -0.0), 0);
}

#[test]
fn floor_rounds_toward_negative_infinity() {
    assert_eq!(call_int(builtin_floor, 0.5), 0);
    assert_eq!(call_int(builtin_floor, 1.5), 1);
    assert_eq!(call_int(builtin_floor, -0.5), -1);
    assert_eq!(call_int(builtin_floor, -1.5), -2);
}

#[test]
fn ceil_rounds_toward_positive_infinity() {
    assert_eq!(call_int(builtin_ceil, 0.5), 1);
    assert_eq!(call_int(builtin_ceil, 1.5), 2);
    assert_eq!(call_int(builtin_ceil, -0.5), 0);
    assert_eq!(call_int(builtin_ceil, -1.5), -1);
}

#[test]
fn round_ties_away_from_zero() {
    assert_eq!(call_int(builtin_round, 0.5), 1);
    assert_eq!(call_int(builtin_round, 1.5), 2);
    assert_eq!(call_int(builtin_round, 2.5), 3);
    assert_eq!(call_int(builtin_round, -0.5), -1);
    assert_eq!(call_int(builtin_round, -1.5), -2);
    assert_eq!(call_int(builtin_round, -2.5), -3);
}

// -----------------------------------------------------------------------------
// 正常系: i64 境界
// -----------------------------------------------------------------------------

#[test]
fn accepts_i64_min_which_is_exactly_representable() {
    // -2^63 は f64 で正確に表現でき、半開区間 [-2^63, 2^63) の下端として受理する。
    let min = i64::MIN as f64;
    assert_eq!(call_int(builtin_to_int, min), i64::MIN);
    assert_eq!(call_int(builtin_floor, min), i64::MIN);
    assert_eq!(call_int(builtin_ceil, min), i64::MIN);
    assert_eq!(call_int(builtin_round, min), i64::MIN);
}

#[test]
fn accepts_largest_representable_below_two_pow_63() {
    // 2^63 直前の f64 表現可能値（2^63 - 1024 = 9223372036854774784）は受理する。
    let below = 9_223_372_036_854_774_784.0_f64;
    let expected = below as i64;
    assert_eq!(call_int(builtin_to_int, below), expected);
    assert_eq!(call_int(builtin_floor, below), expected);
    assert_eq!(call_int(builtin_ceil, below), expected);
    assert_eq!(call_int(builtin_round, below), expected);
    assert!(expected < i64::MAX, "2^63 直前の値は i64::MAX 未満");
}

// -----------------------------------------------------------------------------
// エラー系: NaN / Infinity / 範囲外
// -----------------------------------------------------------------------------

#[test]
fn nan_is_conversion_error_for_all_modes() {
    expect_conversion_error(
        builtin_to_int,
        f64::NAN,
        "to_int で Int に変換できません: NaN",
    );
    expect_conversion_error(
        builtin_floor,
        f64::NAN,
        "floor で Int に変換できません: NaN",
    );
    expect_conversion_error(builtin_ceil, f64::NAN, "ceil で Int に変換できません: NaN");
    expect_conversion_error(
        builtin_round,
        f64::NAN,
        "round で Int に変換できません: NaN",
    );
}

#[test]
fn infinity_is_conversion_error_for_all_modes() {
    expect_conversion_error(
        builtin_to_int,
        f64::INFINITY,
        "to_int で Int に変換できません: 非有限値",
    );
    expect_conversion_error(
        builtin_floor,
        f64::NEG_INFINITY,
        "floor で Int に変換できません: 非有限値",
    );
    expect_conversion_error(
        builtin_ceil,
        f64::INFINITY,
        "ceil で Int に変換できません: 非有限値",
    );
    expect_conversion_error(
        builtin_round,
        f64::NEG_INFINITY,
        "round で Int に変換できません: 非有限値",
    );
}

#[test]
fn positive_two_pow_63_is_out_of_range() {
    // 上端 2^63 は i64 で表現できないため受理しない（i64::MAX as f64 も 2^63 へ丸む）。
    let two_pow_63 = 9_223_372_036_854_775_808.0_f64;
    expect_conversion_error(
        builtin_to_int,
        two_pow_63,
        "to_int で Int に変換できません: i64 範囲外",
    );
    expect_conversion_error(
        builtin_round,
        two_pow_63,
        "round で Int に変換できません: i64 範囲外",
    );
}

#[test]
fn i64_max_as_f64_is_out_of_range() {
    // i64::MAX (2^63 - 1) は f64 では 2^63 へ丸められるため受理しない。
    let value = i64::MAX as f64;
    expect_conversion_error(
        builtin_to_int,
        value,
        "to_int で Int に変換できません: i64 範囲外",
    );
}

#[test]
fn large_magnitude_values_are_out_of_range() {
    expect_conversion_error(
        builtin_to_int,
        1e19,
        "to_int で Int に変換できません: i64 範囲外",
    );
    expect_conversion_error(
        builtin_to_int,
        -1e19,
        "to_int で Int に変換できません: i64 範囲外",
    );
    expect_conversion_error(
        builtin_ceil,
        1e300,
        "ceil で Int に変換できません: i64 範囲外",
    );
    expect_conversion_error(
        builtin_floor,
        -1e300,
        "floor で Int に変換できません: i64 範囲外",
    );
}

// -----------------------------------------------------------------------------
// file_size: u64 → i64 境界
// -----------------------------------------------------------------------------

#[test]
fn file_size_of_real_file_is_positive_int() {
    let path = std::env::temp_dir().join("tsg_aud036_file_size.txt");
    std::fs::write(&path, b"hello").expect("一時ファイル作成");
    let path_str = path.to_string_lossy().to_string();

    // sandbox 未設定時は allow-all（fail-open）なので読める。
    match builtin_file_size(&[Value::Str(path_str)], 1) {
        Ok(Value::Int(n)) => assert_eq!(n, 5, "file_size はバイト数を返す"),
        other => panic!("Int が返るはず: {other:?}"),
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn missing_file_returns_null() {
    let path = std::env::temp_dir().join("tsg_aud036_missing_file_xyz.txt");
    let _ = std::fs::remove_file(&path);
    let path_str = path.to_string_lossy().to_string();
    match builtin_file_size(&[Value::Str(path_str)], 1) {
        Ok(Value::Null) => {}
        other => panic!("存在しないファイルは Null: {other:?}"),
    }
}
