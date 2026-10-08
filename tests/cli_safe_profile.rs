//! CAP-AT-23: CLI safe profile（既定）の capability 挙動を tree/VM 両 engine で固定する（C9/C10）。
//!
//! safe 既定では stdout 以外の ambient call が 0。各 capability option を個別に付けたときだけ該当
//! 操作が成功し、隣接操作は deny。`--fs-op` の 6 トークン mapping、`remove_tree` が常に deny、
//! unqualified path の `default` 依存、`--allow-import-root` の無効果を固定する。

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn tsumugi_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsumugi")
}

/// 一意な一時ディレクトリ。
struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "tsumugi-cap23-{}-{}-{}",
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

/// safe profile（既定・profile 無指定）で script を実行する。`extra` は追加 CLI option。
fn run_safe(use_vm: bool, extra: &[&str], source: &str) -> Run {
    let dir = TestDir::new("script");
    let script = dir.path.join("s.tsg");
    let mut f = std::fs::File::create(&script).expect("write script");
    f.write_all(source.as_bytes()).expect("write");
    drop(f);

    let mut cmd = Command::new(tsumugi_bin());
    if use_vm {
        cmd.arg("--vm");
    }
    for a in extra {
        cmd.arg(a);
    }
    cmd.arg(&script);
    let output = cmd.output().expect("spawn tsumugi");
    Run {
        status_ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout)
            .replace("\r\n", "\n")
            .trim_end()
            .to_string(),
        stderr: String::from_utf8_lossy(&output.stderr)
            .replace("\r\n", "\n")
            .trim_end()
            .to_string(),
    }
}

#[test]
fn safe_default_grants_only_stdout() {
    for use_vm in [false, true] {
        // print は safe 既定で成功（stdout grant 済み）。
        let r = run_safe(use_vm, &[], "print(\"hi\")");
        assert!(r.status_ok, "safe print 成功 [vm={use_vm}]: {}", r.stderr);
        assert_eq!(r.stdout, "hi");

        // env/now/input/read_file/exit は未 grant → catch 可能 capability/sandbox error。
        // 未捕捉なら exit 1。
        for script in [
            "print(env(\"HOME\"))",
            "print(now())",
            "print(read_file(\"/etc/hostname\"))",
        ] {
            let r = run_safe(use_vm, &[], script);
            assert!(
                !r.status_ok,
                "safe 既定で未 grant 操作は失敗する [vm={use_vm}] script={script}: {}",
                r.stdout
            );
        }
    }
}

#[test]
fn deny_stdout_blocks_print() {
    for use_vm in [false, true] {
        let r = run_safe(use_vm, &["--deny-stdout"], "print(\"hi\")");
        assert!(
            !r.status_ok,
            "--deny-stdout で print は capability error [vm={use_vm}]"
        );
        assert_eq!(r.stdout, "", "stdout call 0: {}", r.stdout);
    }
}

#[test]
fn allow_clock_grants_now_only() {
    for use_vm in [false, true] {
        let r = run_safe(use_vm, &["--allow-clock"], "print(now() > 0)");
        assert!(
            r.status_ok,
            "--allow-clock で now 成功 [vm={use_vm}]: {}",
            r.stderr
        );
        assert_eq!(r.stdout, "true");
        // clock を付けても env は依然 deny。
        let r = run_safe(use_vm, &["--allow-clock"], "print(env(\"HOME\"))");
        assert!(
            !r.status_ok,
            "clock grant は env を許可しない [vm={use_vm}]"
        );
    }
}

#[test]
fn allow_env_grants_requested_key_only() {
    for use_vm in [false, true] {
        // TSG_CAP23 を allow して env で読む。subprocess 側で env を設定する。
        let dir = TestDir::new("env");
        let script = dir.path.join("s.tsg");
        std::fs::write(
            &script,
            "print(env(\"TSG_CAP23\"))\nprint(env(\"TSG_OTHER\"))",
        )
        .unwrap();
        let mut cmd = Command::new(tsumugi_bin());
        if use_vm {
            cmd.arg("--vm");
        }
        cmd.arg("--allow-env")
            .arg("TSG_CAP23")
            .arg(&script)
            .env("TSG_CAP23", "visible")
            .env("TSG_OTHER", "hidden");
        let output = cmd.output().expect("spawn");
        let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
        assert!(output.status.success(), "allow-env 成功 [vm={use_vm}]");
        // 許可 key は値、未許可 key は null（missing）。
        assert!(
            stdout.contains("visible"),
            "許可 key の値が見える: {stdout}"
        );
        assert!(stdout.contains("null"), "未許可 key は null: {stdout}");
        assert!(
            !stdout.contains("hidden"),
            "未許可 key の値は漏れない: {stdout}"
        );
    }
}

// safe profile の fs grant 成功（read/metadata）は Unix / Windows の両方で検証する。両 OS
// とも secure resolution を実装するため成功を assert できる（capability-model §8.3 契約3）。
// option mapping・adjacent deny の網羅検証も全 OS で走らせる。
#[test]
fn fs_op_mapping_and_adjacent_deny() {
    for use_vm in [false, true] {
        let dir = TestDir::new("fs");
        std::fs::write(dir.path.join("a.txt"), b"hello").unwrap();
        let root = format!("data={}", dir.as_str());

        // read だけ grant → read_file 成功、write は deny。
        let r = run_safe(
            use_vm,
            &["--fs-root", &root, "--fs-op", "data=read"],
            "print(read_file(\"@data/a.txt\"))",
        );
        assert!(
            r.status_ok,
            "read grant で read 成功 [vm={use_vm}]: {}",
            r.stderr
        );
        assert_eq!(r.stdout, "hello");

        let r = run_safe(
            use_vm,
            &["--fs-root", &root, "--fs-op", "data=read"],
            "write_file(\"@data/b.txt\", \"x\")",
        );
        assert!(
            !r.status_ok,
            "read のみ grant で write は deny [vm={use_vm}]"
        );

        // metadata トークンは path_exists/file_size/is_file/is_dir の 4 builtin を認可。
        let r = run_safe(
            use_vm,
            &["--fs-root", &root, "--fs-op", "data=metadata"],
            "print(path_exists(\"@data/a.txt\"))\nprint(is_file(\"@data/a.txt\"))\nprint(file_size(\"@data/a.txt\"))",
        );
        assert!(
            r.status_ok,
            "metadata grant で metadata 系成功 [vm={use_vm}]: {}",
            r.stderr
        );
        assert_eq!(r.stdout, "true\ntrue\n5");
    }
}

#[test]
fn remove_tree_is_never_granted_in_safe() {
    for use_vm in [false, true] {
        let dir = TestDir::new("rmtree");
        std::fs::create_dir(dir.path.join("sub")).unwrap();
        let root = format!("data={}", dir.as_str());
        // 6 トークン全部付けても RecursiveDelete は付与されず remove_tree は deny（方針 A）。
        let r = run_safe(
            use_vm,
            &[
                "--fs-root",
                &root,
                "--fs-op",
                "data=read,write,create,delete,metadata,list",
            ],
            "remove_tree(\"@data/sub\")",
        );
        assert!(
            !r.status_ok,
            "remove_tree はどの --fs-op トークンでも deny [vm={use_vm}]: {}",
            r.stdout
        );
        // sub は削除されない。
        assert!(dir.path.join("sub").exists(), "remove_tree は実行されない");
    }
}

// default mount の unqualified 解決成功は実 fs read を伴う。Unix / Windows とも secure
// resolution を実装するため全 OS で走らせる（§8.3 契約3）。
#[test]
fn unqualified_path_requires_default_mount() {
    for use_vm in [false, true] {
        let dir = TestDir::new("unq");
        std::fs::write(dir.path.join("a.txt"), b"x").unwrap();

        // mount 名 data（default でない）で unqualified path を使うと default 不在で deny。
        let root = format!("data={}", dir.as_str());
        let r = run_safe(
            use_vm,
            &["--fs-root", &root, "--fs-op", "data=read"],
            "print(read_file(\"a.txt\"))",
        );
        assert!(
            !r.status_ok,
            "unqualified + default 未指定は deny（fallback しない）[vm={use_vm}]"
        );

        // default mount を明示すると unqualified が解決する。
        let root = format!("default={}", dir.as_str());
        let r = run_safe(
            use_vm,
            &["--fs-root", &root, "--fs-op", "default=read"],
            "print(read_file(\"a.txt\"))",
        );
        assert!(
            r.status_ok,
            "default 明示で unqualified 解決 [vm={use_vm}]: {}",
            r.stderr
        );
        assert_eq!(r.stdout, "x");
    }
}

#[test]
fn allow_import_root_has_no_effect_on_at_syntax() {
    for use_vm in [false, true] {
        // --allow-import-root を付けても @foo/... は alpha ModuleLoader がリテラル相対として解決し
        // import error になる（従来どおり未解決）。import error は safe/legacy 問わず起きる。
        let dir = TestDir::new("imp");
        let script = dir.path.join("s.tsg");
        std::fs::write(&script, "import \"@foo/bar.tsg\"\nprint(1)").unwrap();
        let mut cmd = Command::new(tsumugi_bin());
        if use_vm {
            cmd.arg("--vm");
        }
        cmd.arg("--allow-import-root")
            .arg(format!("foo={}", dir.as_str()))
            .arg(&script);
        let output = cmd.output().expect("spawn");
        assert!(
            !output.status.success(),
            "@foo/... は未解決 import error [vm={use_vm}]"
        );
    }
}

#[test]
fn relative_import_still_works_under_safe() {
    for use_vm in [false, true] {
        // 相対パス import は従来どおり動く（import 解決は C6 の sandbox 経路、profile 非依存）。
        let dir = TestDir::new("relimp");
        std::fs::write(dir.path.join("lib.tsg"), "let helper = 42\n").unwrap();
        let main = dir.path.join("main.tsg");
        std::fs::write(&main, "import \"lib.tsg\"\nprint(helper)").unwrap();
        let mut cmd = Command::new(tsumugi_bin());
        if use_vm {
            cmd.arg("--vm");
        }
        cmd.arg(&main);
        let output = cmd.output().expect("spawn");
        let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
        assert!(
            output.status.success(),
            "相対 import は safe でも動く [vm={use_vm}]: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("42"), "import した値: {stdout}");
    }
}
