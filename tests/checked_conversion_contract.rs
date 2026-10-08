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

use tsumugi::builtin_core::{builtin_ceil, builtin_floor, builtin_round, builtin_to_int};
use tsumugi::value::Value;

// capability 経路の fs テスト（file_size）は Unix 限定（§8.3 契約3の fail-closed）。
// 関連 import とヘルパーも Unix でのみ使うため同じ cfg でガードし、非 Unix の未使用 import
// warning（clippy -D warnings）を避ける。
#[cfg(unix)]
use std::collections::BTreeSet;
#[cfg(unix)]
use std::num::NonZeroU128;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use tsumugi::builtin_core::dispatch_filesystem_capability;
#[cfg(unix)]
use tsumugi::error::ErrorKind;
#[cfg(unix)]
use tsumugi::{
    FilesystemCapability, FilesystemRoot, FsOperation, MountName, OsDirectoryHandle, SymlinkPolicy,
};

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
// file_size: u64 → i64 境界（C10 後は capability 経路で検証する）
// -----------------------------------------------------------------------------

/// 一時 dir を root に Metadata を grant した filesystem capability を作る。
#[cfg(unix)]
fn metadata_fs(base: &std::path::Path) -> FilesystemCapability {
    let pid = NonZeroU128::new(1).expect("non-zero");
    let mut ops = BTreeSet::new();
    ops.insert(FsOperation::Metadata);
    let mount = MountName::new("data").expect("mount");
    let handle = OsDirectoryHandle::new(base.to_path_buf(), pid, SymlinkPolicy::DenyAll);
    let root = FilesystemRoot::new(mount, pid, ops, SymlinkPolicy::DenyAll, Arc::new(handle))
        .expect("root");
    FilesystemCapability::new(vec![root]).expect("fs")
}

/// 一意な一時 dir を作る。
#[cfg(unix)]
fn temp_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!("tsg-aud036-{}-{}-{}", tag, std::process::id(), n));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

// C10/C5-b: capability 経路の実 fs 解決は Unix でのみ提供し、非 Unix は
// SecureResolutionUnsupported で fail closed する（capability-model §14.3・§8.3 契約3）。
// success/missing の意味的検証は Unix でのみ成立するため Unix 限定にする（非 Unix では
// どちらも fail closed で区別がつかず、意図と違う理由でグリーンになるのを避ける）。
// 非 Unix の fail-closed 挙動は capability.rs の非 Unix adapter が担保する。
#[cfg(unix)]
#[test]
fn file_size_of_real_file_is_positive_int() {
    // C10: capability 経路（@mount/rel 構文）で file_size を検証する。
    let base = temp_dir("file_size");
    std::fs::write(base.join("f.txt"), b"hello").expect("一時ファイル作成");
    let fs = metadata_fs(&base);

    match dispatch_filesystem_capability(
        "file_size",
        &[Value::str_constant("@data/f.txt".to_string())],
        &fs,
        1024,
        1,
    ) {
        Ok(Value::Int(n)) => assert_eq!(n, 5, "file_size はバイト数を返す"),
        other => panic!("Int が返るはず: {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn missing_file_is_host_error() {
    // C10: capability 経路の file_size は、許可 root 内でも missing file を `host` error（catch
    // 可能）へ写す（ambient 時代の Null 返しは廃止。存在 oracle 防止は route/認可で担保済み）。
    let base = temp_dir("missing");
    let fs = metadata_fs(&base);
    let result = dispatch_filesystem_capability(
        "file_size",
        &[Value::str_constant("@data/no_such.txt".to_string())],
        &fs,
        1024,
        1,
    );
    let error = result.expect_err("存在しない file の file_size は host error");
    assert_eq!(error.kind(), Some(ErrorKind::Host));
    let _ = std::fs::remove_dir_all(&base);
}
