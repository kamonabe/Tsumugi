//! REV-009（§17.4）: ambient（legacy）経路の `list_dir` 挙動を固定する。
//!
//! safe（capability）経路の挙動——個別 entry 失敗→`directory_read` host error、非 UTF-8 名
//! →`invalid_encoding` host error——は `src/vm.rs` の `#[cfg(unix)]` unit test で覆う。
//! この file は legacy 互換（skip / lossy 変換）が維持されることを固定する。sandbox 未設定時は
//! allow-all（fail-open）なので一時ディレクトリを列挙できる。

#![cfg(unix)]

use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicU64, Ordering};

use tsumugi::builtin_core::builtin_list_dir;
use tsumugi::value::Value;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "tsumugi-rev009-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

#[test]
fn legacy_list_dir_keeps_non_utf8_name_via_lossy_conversion() {
    // legacy 経路（ambient）は非 UTF-8 名を lossy 変換して List に含める（error にしない）。
    let dir = temp_dir("legacy_nonutf8");
    std::fs::write(dir.join("good.txt"), b"g").unwrap();
    let bad = std::ffi::OsStr::from_bytes(b"bad\x80name");
    // 非 UTF-8 の file 名を許さない filesystem（macOS/APFS 等は EILSEQ で拒否する）では
    // この legacy lossy ケースを検証できないため、作成に失敗したら skip する。
    if std::fs::write(dir.join(bad), b"x").is_err() {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let arg = Value::str_constant(dir.to_string_lossy().to_string());
    // max_collection は十分大きく取る（budget 検査は別契約）。
    let result = builtin_list_dir(&[arg], 1024, 1).expect("legacy list_dir");
    match result {
        Value::List(items) => {
            let names: Vec<String> = items.iter().map(|v| v.to_string()).collect();
            assert_eq!(
                names.len(),
                2,
                "非 UTF-8 名も skip されず含まれる: {names:?}"
            );
            assert!(names.iter().any(|n| n == "good.txt"));
            // 非 UTF-8 名は lossy 置換文字（U+FFFD）を含む形で返る（error にはならない）。
            assert!(
                names.iter().any(|n| n != "good.txt"),
                "lossy 変換された名前が存在する: {names:?}"
            );
        }
        other => panic!("list はリストを返す: {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}
