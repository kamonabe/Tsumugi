//! CAP-AT-27: core から ambient OS access が除去されたことを静的 + 動的に固定する（C10）。
//!
//! 静的: `src/` の production コード（`#[cfg(test)]` モジュールより前）で、`std::env::var*` /
//! `std::fs::*` / `std::io::stdin` / `std::io::stdout` / `std::process::exit` / `SystemTime::now`
//! の直接呼びが CLI adapter（`main.rs` / `cli_capability.rs`）と capability adapter
//! （`capability.rs`）以外に無いこと（import 解決の `module.rs` / `sandbox.rs`、budget env の
//! `budget.rs` は許可リストで除外）。
//!
//! 動的: safe 既定で fs/env/clock/stdin/exit を試み、未 grant で Denial（ambient へ漏れない）
//! ことを tree/VM で確認する。

use std::path::{Path, PathBuf};
use std::process::Command;

fn tsumugi_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsumugi")
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// production コードで ambient 直接呼びを許可するファイル（理由付き）。
///
/// - `main.rs` / `cli_capability.rs`: CLI adapter。profile から frozen set を組む境界で OS を触る。
/// - `capability.rs`: System* / OsDirectoryHandle adapter（OS 実装そのもの）。
/// - `module.rs` / `sandbox.rs`: import 解決（C6）。runtime fs ではない（§6.6）。C6/E7 で撤去予定。
/// - `budget.rs`: `from_legacy_env`（budget env）。C9/C10 対象外（Phase 3/REV-015）。
const ALLOWLIST: &[&str] = &[
    "main.rs",
    "cli_capability.rs",
    "capability.rs",
    "module.rs",
    "sandbox.rs",
    "budget.rs",
];

/// 検出する ambient パターン。
const PATTERNS: &[&str] = &[
    "std::env::var",
    "std::env::vars",
    "std::env::current_dir",
    "std::env::temp_dir",
    "std::fs::",
    "std::io::stdin",
    "std::io::stdout",
    "std::process::exit",
    "SystemTime::now",
];

#[test]
fn core_has_no_direct_ambient_os_access() {
    let dir = src_dir();
    let mut violations: Vec<String> = Vec::new();

    for entry in std::fs::read_dir(&dir).expect("read src dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        if ALLOWLIST.contains(&file_name.as_str()) {
            continue;
        }
        let content = std::fs::read_to_string(&path).expect("read file");
        for (lineno, line) in content.lines().enumerate() {
            // production コードのみを検査する。`#[cfg(test)]` 以降（テストモジュール）は除外する
            // ——このリポジトリの慣例でテストはファイル末尾の単一 `#[cfg(test)] mod tests` にある。
            if line.trim_start().starts_with("#[cfg(test)]") {
                break;
            }
            // コメント行は無視する。
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("*") {
                continue;
            }
            for pat in PATTERNS {
                if line.contains(pat) {
                    violations.push(format!("{}:{}: {}", file_name, lineno + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "core production コードに ambient 直接呼びが残っています（CLI/adapter/import/budget 以外）:\n{}",
        violations.join("\n")
    );
}

fn run_safe(use_vm: bool, source: &str) -> bool {
    let dir = std::env::temp_dir().join(format!(
        "tsumugi-cap27-{}-{}",
        std::process::id(),
        source.len()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let script = dir.join("s.tsg");
    std::fs::write(&script, source).expect("write");
    let mut cmd = Command::new(tsumugi_bin());
    if use_vm {
        cmd.arg("--vm");
    }
    cmd.arg(&script);
    // 旧 env を除去して safe 既定の deny を純粋に観測する。
    cmd.env_remove("TSUMUGI_SANDBOX");
    cmd.env_remove("TSUMUGI_ENV_ALLOW");
    let ok = cmd.output().expect("spawn").status.success();
    let _ = std::fs::remove_dir_all(&dir);
    ok
}

#[test]
fn safe_default_denies_all_ambient_operations() {
    for use_vm in [false, true] {
        // safe 既定で fs/env/clock/stdin/exit を試みると未 grant で Denial（ambient へ漏れない）。
        for script in [
            "read_file(\"/etc/hostname\")",
            "env(\"HOME\")",
            "now()",
            "input()",
            "exit(0)",
        ] {
            assert!(
                !run_safe(use_vm, script),
                "safe 既定で未 grant 操作は Denial になる [vm={use_vm}] script={script}"
            );
        }
    }
}
