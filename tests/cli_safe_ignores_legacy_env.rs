//! CAP-AT-26: safe 既定で旧環境変数（TSUMUGI_SANDBOX / TSUMUGI_ENV_ALLOW）を変更しても、
//! capability 挙動が不変であることを tree/VM で固定する（C10）。
//!
//! 射程（finding 2）: 「挙動不変」は runtime fs builtin / env / clock / stdin / exit / stdout の
//! capability 経路に限る。import 解決は C6 の sandbox 依存を継続するため対象外——fixture には
//! import を含めない。

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
        path.push(format!("tsumugi-cap26-{}-{}", std::process::id(), n));
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

/// safe 既定（profile 無指定）で script を実行し、(stdout, stderr, success) を返す。
fn run_safe_with_env(
    use_vm: bool,
    script: &std::path::Path,
    envs: &[(&str, &str)],
    remove: &[&str],
) -> (String, String, bool) {
    let mut cmd = Command::new(tsumugi_bin());
    if use_vm {
        cmd.arg("--vm");
    }
    cmd.arg(script);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    for k in remove {
        cmd.env_remove(k);
    }
    let output = cmd.output().expect("spawn tsumugi");
    (
        String::from_utf8_lossy(&output.stdout)
            .replace("\r\n", "\n")
            .trim_end()
            .to_string(),
        String::from_utf8_lossy(&output.stderr)
            .replace("\r\n", "\n")
            .trim_end()
            .to_string(),
        output.status.success(),
    )
}

#[test]
fn safe_ignores_legacy_env_for_env_and_fs() {
    for use_vm in [false, true] {
        let dir = TestDir::new();
        let script = dir.path.join("s.tsg");
        // safe 既定では env/fs は未 grant。旧 env を変えても結果（失敗・stdout 空）が不変であること。
        // import は含めない（C6 sandbox 依存のため CAP-AT-26 対象外）。
        std::fs::write(&script, "print(env(\"TSG_CAP26\"))").unwrap();

        // 旧 env を「設定」した run。
        let with_env = run_safe_with_env(
            use_vm,
            &script,
            &[
                ("TSUMUGI_SANDBOX", dir.path.to_str().unwrap()),
                ("TSUMUGI_ENV_ALLOW", "*"),
                ("TSG_CAP26", "value"),
            ],
            &[],
        );
        // 旧 env を「除去」した run。
        let without_env = run_safe_with_env(
            use_vm,
            &script,
            &[("TSG_CAP26", "value")],
            &["TSUMUGI_SANDBOX", "TSUMUGI_ENV_ALLOW"],
        );

        // safe 既定では env 未 grant → どちらも同じ結果（env は capability error で失敗）。
        assert_eq!(
            (with_env.0.clone(), with_env.2),
            (without_env.0.clone(), without_env.2),
            "safe 既定で旧 env 変更は env 挙動へ影響しない [vm={use_vm}]: with={:?} without={:?}",
            with_env,
            without_env
        );
        assert!(
            !with_env.2,
            "safe 既定で env は未 grant（失敗する）[vm={use_vm}]"
        );
    }
}

#[test]
fn safe_stdout_behavior_is_stable_across_legacy_env() {
    for use_vm in [false, true] {
        let dir = TestDir::new();
        let script = dir.path.join("s.tsg");
        std::fs::write(&script, "print(\"stable\")").unwrap();

        let a = run_safe_with_env(
            use_vm,
            &script,
            &[
                ("TSUMUGI_SANDBOX", "/some/where"),
                ("TSUMUGI_ENV_ALLOW", "FOO,BAR"),
            ],
            &[],
        );
        let b = run_safe_with_env(
            use_vm,
            &script,
            &[],
            &["TSUMUGI_SANDBOX", "TSUMUGI_ENV_ALLOW"],
        );
        assert_eq!(a.0, "stable");
        assert_eq!(a, b, "safe の print 挙動は旧 env に不変 [vm={use_vm}]");
    }
}
