//! CAP-AT-25: 既定 profile（無指定 = safe）の構成差と legacy warning を golden 固定する（C9/C10）。
//!
//! N-1（legacy 既定）を飛ばし safe 既定（N 相当）を採るため、golden は release 間 default 差では
//! なく「無指定 = safe」vs「--profile legacy」の profile 選択差 2 点に縮退する（finding 9）。
//! CapabilitySetId は CLI から直接観測できないため、観測挙動（capability の有無）と warning 文言で
//! 固定する。

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn tsumugi_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsumugi")
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("tsumugi-cap25-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// §7.3 warning 文言 golden。
const WARN_EMPTY_SANDBOX: &str = "警告: TSUMUGI_SANDBOX が未設定のため、legacy profile はファイルシステム全体へのアクセスを許可します";
const WARN_EMPTY_ENV_ALLOW: &str = "警告: TSUMUGI_ENV_ALLOW が未設定のため、legacy profile は TSUMUGI_ 以外の全環境変数を公開します";

fn run(args: &[&str], script: &std::path::Path, remove_legacy_env: bool) -> (String, String, bool) {
    let mut cmd = Command::new(tsumugi_bin());
    for a in args {
        cmd.arg(a);
    }
    cmd.arg(script);
    if remove_legacy_env {
        cmd.env_remove("TSUMUGI_SANDBOX");
        cmd.env_remove("TSUMUGI_ENV_ALLOW");
    }
    let output = cmd.output().expect("spawn");
    (
        String::from_utf8_lossy(&output.stdout)
            .replace("\r\n", "\n")
            .trim_end()
            .to_string(),
        String::from_utf8_lossy(&output.stderr)
            .replace("\r\n", "\n")
            .to_string(),
        output.status.success(),
    )
}

#[test]
fn default_profile_equals_safe() {
    // 無指定と --profile safe が同じ観測挙動（print 成功・env deny・stderr 空）であることを固定する。
    let dir = TestDir::new();
    let script = dir.path.join("s.tsg");
    std::fs::write(&script, "print(\"ok\")").unwrap();

    let default = run(&[], &script, true);
    let explicit_safe = run(&["--profile", "safe"], &script, true);
    assert_eq!(default.0, "ok");
    assert_eq!(default.1, "", "safe 既定は stderr 空");
    assert!(default.2);
    assert_eq!(
        default, explicit_safe,
        "無指定 = --profile safe（観測挙動が一致）"
    );

    // env は safe 既定で deny（golden: 失敗）。
    let env_script = dir.path.join("e.tsg");
    std::fs::write(&env_script, "print(env(\"HOME\"))").unwrap();
    let (_, _, env_ok) = run(&[], &env_script, true);
    assert!(!env_ok, "safe 既定で env は deny");
}

#[test]
fn legacy_profile_warnings_are_golden() {
    // --profile legacy + 空 sandbox / 空 env-allow の warning 文言を golden 固定する。
    let dir = TestDir::new();
    let script = dir.path.join("s.tsg");
    std::fs::write(&script, "print(1)").unwrap();

    let (_, stderr, ok) = run(&["--profile", "legacy"], &script, true);
    assert!(ok, "legacy は実行成功");
    // 空 sandbox / 空 env-allow の両警告が 1 件ずつ出る。
    assert_eq!(
        stderr.lines().filter(|l| *l == WARN_EMPTY_SANDBOX).count(),
        1,
        "空 sandbox 警告 golden: {stderr}"
    );
    assert_eq!(
        stderr
            .lines()
            .filter(|l| *l == WARN_EMPTY_ENV_ALLOW)
            .count(),
        1,
        "空 env-allow 警告 golden: {stderr}"
    );
    // それ以外の行は無い（警告 2 行だけ）。
    assert_eq!(
        stderr.lines().filter(|l| !l.is_empty()).count(),
        2,
        "legacy 警告は 2 件だけ: {stderr}"
    );
}
