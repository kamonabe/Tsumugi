//! CAP-AT-24: CLI legacy profile の互換挙動と warning を tree/VM 両 engine で固定する（C9/C10）。
//!
//! legacy は env/clock/stdin/stdout/exit を grant し、`TSUMUGI_SANDBOX` を host path 翻訳表として
//! filesystem を構築する。空 sandbox / 空 env-allow は stderr warning 各 1 件。root 内 symlink は
//! `FollowWithinRoot` で追従し、root 外へ逃げる symlink と範囲外 path は deny。

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
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "tsumugi-cap24-{}-{}-{}",
            label,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn as_str(&self) -> &str {
        self.path.to_str().expect("UTF-8 path")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct Run {
    status_ok: bool,
    stdout: String,
    stderr: String,
}

/// legacy profile で script を実行する。`envs` は環境変数、`src_dir` は script を置く dir。
fn run_legacy(use_vm: bool, src_dir: &std::path::Path, source: &str, envs: &[(&str, &str)]) -> Run {
    let script = src_dir.join("s.tsg");
    std::fs::write(&script, source).expect("write script");
    let mut cmd = Command::new(tsumugi_bin());
    if use_vm {
        cmd.arg("--vm");
    }
    cmd.arg("--profile").arg("legacy").arg(&script);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // テストプロセスから継承する TSUMUGI_* を打ち消し、各ケースの前提を固定する。
    if !envs.iter().any(|(k, _)| *k == "TSUMUGI_SANDBOX") {
        cmd.env_remove("TSUMUGI_SANDBOX");
    }
    if !envs.iter().any(|(k, _)| *k == "TSUMUGI_ENV_ALLOW") {
        cmd.env_remove("TSUMUGI_ENV_ALLOW");
    }
    let output = cmd.output().expect("spawn tsumugi");
    Run {
        status_ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout)
            .replace("\r\n", "\n")
            .trim_end()
            .to_string(),
        stderr: String::from_utf8_lossy(&output.stderr)
            .replace("\r\n", "\n")
            .to_string(),
    }
}

const WARN_EMPTY_SANDBOX: &str = "警告: TSUMUGI_SANDBOX が未設定のため、legacy profile はファイルシステム全体へのアクセスを許可します";
const WARN_EMPTY_ENV_ALLOW: &str = "警告: TSUMUGI_ENV_ALLOW が未設定のため、legacy profile は TSUMUGI_ 以外の全環境変数を公開します";

#[test]
fn legacy_grants_env_clock_stdout() {
    for use_vm in [false, true] {
        let dir = TestDir::new("grants");
        let r = run_legacy(
            use_vm,
            &dir.path,
            "print(now() > 0)\nprint(env(\"TSG_CAP24\") == \"v\")",
            &[
                ("TSUMUGI_SANDBOX", dir.as_str()),
                ("TSUMUGI_ENV_ALLOW", "*"),
                ("TSG_CAP24", "v"),
            ],
        );
        assert!(
            r.status_ok,
            "legacy で env/clock/stdout 動作 [vm={use_vm}]: {}",
            r.stderr
        );
        assert_eq!(r.stdout, "true\ntrue");
    }
}

#[test]
fn empty_sandbox_emits_single_warning() {
    for use_vm in [false, true] {
        let dir = TestDir::new("emptysb");
        // TSUMUGI_SANDBOX 未設定（env-allow は設定して env 警告を切り分ける）。
        let r = run_legacy(use_vm, &dir.path, "print(1)", &[("TSUMUGI_ENV_ALLOW", "*")]);
        assert!(
            r.status_ok,
            "空 sandbox でも実行は成功 [vm={use_vm}]: {}",
            r.stderr
        );
        let count = r
            .stderr
            .lines()
            .filter(|l| *l == WARN_EMPTY_SANDBOX)
            .count();
        assert_eq!(
            count, 1,
            "空 sandbox 警告は 1 件 [vm={use_vm}]: {}",
            r.stderr
        );
    }
}

#[test]
fn empty_env_allow_emits_single_warning() {
    for use_vm in [false, true] {
        let dir = TestDir::new("emptyenv");
        // TSUMUGI_SANDBOX は設定して sandbox 警告を切り分け、env-allow 未設定で env 警告を見る。
        let r = run_legacy(
            use_vm,
            &dir.path,
            "print(1)",
            &[("TSUMUGI_SANDBOX", dir.as_str())],
        );
        assert!(
            r.status_ok,
            "空 env-allow でも実行成功 [vm={use_vm}]: {}",
            r.stderr
        );
        let count = r
            .stderr
            .lines()
            .filter(|l| *l == WARN_EMPTY_ENV_ALLOW)
            .count();
        assert_eq!(
            count, 1,
            "空 env-allow 警告は 1 件 [vm={use_vm}]: {}",
            r.stderr
        );
    }
}

// legacy の実 fs route 成功は Unix / Windows の両方で検証する。両 OS とも secure resolution
// を実装するため成功を assert できる（capability-model §8.3 契約3）。symlink 生成に依存する
// legacy_symlink_* は std::os::unix::fs::symlink を使うため #[cfg(unix)] のまま残す。
#[test]
fn sandbox_routes_old_absolute_path() {
    for use_vm in [false, true] {
        let dir = TestDir::new("route");
        std::fs::write(dir.path.join("in.txt"), b"data").unwrap();
        let target = dir.path.join("in.txt");
        let target_str = target.to_str().unwrap().replace('\\', "/");
        // 旧スクリプト流儀の絶対 path が translator 経由で mount へ route され読める。
        let r = run_legacy(
            use_vm,
            &dir.path,
            &format!("print(read_file(\"{}\"))", target_str),
            &[
                ("TSUMUGI_SANDBOX", dir.as_str()),
                ("TSUMUGI_ENV_ALLOW", "*"),
            ],
        );
        assert!(
            r.status_ok,
            "sandbox 内絶対 path は route されて読める [vm={use_vm}]: {}",
            r.stderr
        );
        assert_eq!(r.stdout, "data");
    }
}

#[test]
fn out_of_sandbox_path_is_denied() {
    for use_vm in [false, true] {
        let dir = TestDir::new("oob");
        // sandbox 外（/etc/hostname）は route 先 root 無し → sandbox deny。
        let r = run_legacy(
            use_vm,
            &dir.path,
            "print(read_file(\"/etc/hostname\"))",
            &[
                ("TSUMUGI_SANDBOX", dir.as_str()),
                ("TSUMUGI_ENV_ALLOW", "*"),
            ],
        );
        assert!(
            !r.status_ok,
            "sandbox 外 path は deny [vm={use_vm}]: {}",
            r.stdout
        );
    }
}

#[test]
fn ambiguous_root_selection_is_profile_error() {
    for use_vm in [false, true] {
        let dir = TestDir::new("ambig");
        // 同一 host root を 2 回指定すると、最長一致が曖昧になり profile 構築 error（exit 1、§7.4）。
        let sandbox = format!("{d},{d}", d = dir.as_str());
        let r = run_legacy(
            use_vm,
            &dir.path,
            "print(1)",
            &[("TSUMUGI_SANDBOX", &sandbox), ("TSUMUGI_ENV_ALLOW", "*")],
        );
        assert!(!r.status_ok, "曖昧な root 選択は exit 1 [vm={use_vm}]");
        assert!(
            r.stderr.contains("複数の root に同じ深さで一致します"),
            "§7.4 文言 [vm={use_vm}]: {}",
            r.stderr
        );
    }
}

#[cfg(unix)]
#[test]
fn legacy_symlink_follow_within_root_intermediate() {
    for use_vm in [false, true] {
        let dir = TestDir::new("symlink");
        // root 内に実 directory とファイルを作り、それを指す root 内 **中間** symlink を作る。
        // C5 handle 契約: final entry symlink は policy に関わらず拒否する（契約4/5）が、
        // 中間 component の symlink は FollowWithinRoot で root 内に拘束したうえで追従する。
        std::fs::create_dir(dir.path.join("realdir")).unwrap();
        std::fs::write(dir.path.join("realdir").join("file.txt"), b"linked").unwrap();
        std::os::unix::fs::symlink(dir.path.join("realdir"), dir.path.join("linkdir")).unwrap();
        let via_link = dir.path.join("linkdir").join("file.txt");
        let via_link_str = via_link.to_str().unwrap();
        // FollowWithinRoot: 中間 symlink（linkdir）を辿って root 内のファイルを read 成功。
        let r = run_legacy(
            use_vm,
            &dir.path,
            &format!("print(read_file(\"{}\"))", via_link_str),
            &[
                ("TSUMUGI_SANDBOX", dir.as_str()),
                ("TSUMUGI_ENV_ALLOW", "*"),
            ],
        );
        assert!(
            r.status_ok,
            "root 内の中間 symlink を辿った read は成功 [vm={use_vm}]: {}",
            r.stderr
        );
        assert_eq!(r.stdout, "linked");
    }
}

#[cfg(unix)]
#[test]
fn legacy_symlink_escaping_root_is_denied() {
    for use_vm in [false, true] {
        let outside = TestDir::new("outside");
        std::fs::create_dir(outside.path.join("secretdir")).unwrap();
        std::fs::write(outside.path.join("secretdir").join("s.txt"), b"secret").unwrap();
        let dir = TestDir::new("escape");
        // root 内に、root 外の directory を指す **中間** symlink を作る。FollowWithinRoot は
        // 解決先が root 内に収まることを要求するため、root 外へ逃げる symlink は拒否される。
        std::os::unix::fs::symlink(outside.path.join("secretdir"), dir.path.join("escdir"))
            .unwrap();
        let via = dir.path.join("escdir").join("s.txt");
        let via_str = via.to_str().unwrap();
        let r = run_legacy(
            use_vm,
            &dir.path,
            &format!("print(read_file(\"{}\"))", via_str),
            &[
                ("TSUMUGI_SANDBOX", dir.as_str()),
                ("TSUMUGI_ENV_ALLOW", "*"),
            ],
        );
        assert!(
            !r.status_ok,
            "root 外へ逃げる中間 symlink は拒否される [vm={use_vm}]: {}",
            r.stdout
        );
    }
}
