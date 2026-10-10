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

/// C6-c: `@foo/...` qualified import は両 engine で exit 1 になる（理由は engine で異なる）。
///
/// tree 経路では link 層の resolver が `@foo/bar.tsg` を mount `foo` へ routing して**解決は成功**する
/// が、実行時の import 展開は依然 評価器内 `ModuleLoader`（ambient、`@mount` routing 未配線）が担う
/// ため、`@foo/bar.tsg` をリテラル相対 path として扱い run 時 import error になる（link と run で
/// resolver が別経路という既知の Phase 2 ギャップ。E9/E7 で統合）。VM 経路も ModuleLoader が
/// `@foo/...` をリテラル扱いして未解決 import error になる。どちらも exit 1。
#[test]
fn at_syntax_import_fails_in_both_engines() {
    for use_vm in [false, true] {
        let dir = TestDir::new("imp");
        std::fs::write(dir.path.join("bar.tsg"), "let v = 1\n").unwrap();
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
            "@foo/... は両 engine で import error [vm={use_vm}]"
        );
    }
}

/// C6-c（Q2 = Denied all-in）: tree 経路の相対 import は `--allow-import-root` が必須になった。
///
/// 従来は safe profile で `--allow-import-root` 無しの相対 import（ambient fs）が動いていたが、
/// C6-c で tree 経路は Engine resolver（capability）経由になり、resolver 未 grant + import は
/// terminal `Denied`（exit 1）になる（設計 §4.5/§8、マニフェスト原則2 明示的 capability）。
/// import root を渡せば `@default/...` routing で従来どおり解決・実行できる。
/// VM 経路は E9/Phase 5 まで ModuleLoader に残るため従来どおり import root 無しで動く（意図的な
/// tree/VM 既知差、設計 §4.4）。
#[test]
fn relative_import_tree_requires_import_root() {
    let dir = TestDir::new("relimp-tree");
    std::fs::write(dir.path.join("lib.tsg"), "let helper = 42\n").unwrap();
    let main = dir.path.join("main.tsg");
    std::fs::write(&main, "import \"lib.tsg\"\nprint(helper)").unwrap();

    // import root 無し → terminal Denied（exit 1、actionable 診断）。
    let output = Command::new(tsumugi_bin())
        .arg(&main)
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "tree 相対 import は resolver 未 grant で拒否される"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--allow-import-root"),
        "Denied 診断が対処方法を示す: {stderr}"
    );

    // import root 指定 → default routing で解決・実行できる。
    let output = Command::new(tsumugi_bin())
        .arg("--allow-import-root")
        .arg(format!("default={}", dir.as_str()))
        .arg(&main)
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    assert!(
        output.status.success(),
        "import root 指定で tree 相対 import は動く: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("42"), "import した値: {stdout}");
}

/// VM 経路は E9/Phase 5 まで ModuleLoader に残るため、相対 import が従来どおり動く（import root 不要）。
#[test]
fn relative_import_vm_still_works_without_import_root() {
    let dir = TestDir::new("relimp-vm");
    std::fs::write(dir.path.join("lib.tsg"), "let helper = 42\n").unwrap();
    let main = dir.path.join("main.tsg");
    std::fs::write(&main, "import \"lib.tsg\"\nprint(helper)").unwrap();
    let output = Command::new(tsumugi_bin())
        .arg("--vm")
        .arg(&main)
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    assert!(
        output.status.success(),
        "VM 相対 import は従来どおり動く: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("42"), "import した値: {stdout}");
}
