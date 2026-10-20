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

/// `run_safe` と同じ起動だが、プロセスの実 exit code（`None` はシグナル終了）も返す。
/// EMB-AT-11a の Exited→実 process exit code 伝播の観測に使う。
fn run_safe_code(use_vm: bool, extra: &[&str], source: &str) -> (Option<i32>, String, String) {
    let dir = TestDir::new("script-code");
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
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout)
            .replace("\r\n", "\n")
            .trim_end()
            .to_string(),
        String::from_utf8_lossy(&output.stderr)
            .replace("\r\n", "\n")
            .trim_end()
            .to_string(),
    )
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
/// resolver が別経路という既知の Phase 2 ギャップ）。E9 で VM 経路も link 層 gate を通る（resolver
/// grant 済みなので link は成功する）が、run 層 ambient `ModuleLoader` が `@foo/...` をリテラル扱い
/// して未解決 import error になる。どちらも exit 1（run 層の挙動が tree/VM で一致）。
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
/// E9 で VM file/stdin も同じ link 層 gate を通すようになり、この要件は tree/VM 対称になった
/// （VM 版は `relative_import_vm_requires_import_root`）。
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

/// E9: VM file/stdin も tree と対称に `--allow-import-root` が必須になった。
///
/// E9 で VM file/stdin（`execute_vm_with_path`）は link 層の Engine resolver gate へ統合された。
/// 旧挙動（VM は ModuleLoader に残り import root 無しで相対 import が動く、`relative_import_vm_
/// still_works_without_import_root`）は**意図的に反転**し、resolver 未 grant + import は tree と同じく
/// terminal `Denied`（exit 1、actionable 診断）になる。import root を grant すれば従来どおり
/// `@default/...` routing で解決・実行できる（run 層は依然 ambient `ModuleLoader`）。
/// tree 版 `relative_import_tree_requires_import_root` と対称。
#[test]
fn relative_import_vm_requires_import_root() {
    let dir = TestDir::new("relimp-vm");
    std::fs::write(dir.path.join("lib.tsg"), "let helper = 42\n").unwrap();
    let main = dir.path.join("main.tsg");
    std::fs::write(&main, "import \"lib.tsg\"\nprint(helper)").unwrap();

    // import root 無し → terminal Denied（exit 1、actionable 診断）。
    let output = Command::new(tsumugi_bin())
        .arg("--vm")
        .arg(&main)
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "VM 相対 import は resolver 未 grant で拒否される"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--allow-import-root"),
        "Denied 診断が対処方法を示す: {stderr}"
    );

    // import root 指定 → default routing で解決・実行できる。
    let output = Command::new(tsumugi_bin())
        .arg("--vm")
        .arg("--allow-import-root")
        .arg(format!("default={}", dir.as_str()))
        .arg(&main)
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    assert!(
        output.status.success(),
        "import root 指定で VM 相対 import は動く: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("42"), "import した値: {stdout}");
}

/// E9: import 文の無い VM script は import root 無しでも gate を通過して実行できる。
///
/// link 層 gate は import の有無で分岐せず、import 0 件は resolver 不問で空 graph を返す。
/// したがって import の無い VM file/stdin は `--allow-import-root` 無しでも従来どおり実行される
/// （gate が import 0 件を通すことの回帰防止）。
#[test]
fn import_free_vm_runs_without_import_root() {
    let dir = TestDir::new("import-free-vm");
    let main = dir.path.join("main.tsg");
    std::fs::write(&main, "print(7)").unwrap();
    let output = Command::new(tsumugi_bin())
        .arg("--vm")
        .arg(&main)
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    assert!(
        output.status.success(),
        "import の無い VM script は import root 無しで動く: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("7"), "出力: {stdout}");
}

// =============================================================================
// EMB-AT-10 / EMB-AT-11: profile（safe）経由の capability-deny catch 可能性・exit 写像
//
// 既存の embedding.rs 単体テスト（c7_*/c3_*）は「生 CapabilitySet + TreeWalk」経由で
// outcome 種別レベルを固定する。下記は E8b 観点として、CLI safe profile が組む frozen
// set 経由で tree/VM 両 engine の subprocess 挙動を固定する:
//   - 追補1: 未 grant の capability-deny が script の try/catch で catch 可能であること
//            （EMB-AT-10a / EMB-AT-11c）、catch 後に後続処理が走ること（EMB-AT-11d）。
//   - 追補2: `--allow-exit` 経由で exit(N) が実 process exit code N に伝播すること
//            （EMB-AT-11a）、範囲外 exit が RuntimeError 写像（exit 1）になり process が
//            panic せず終了すること（EMB-AT-11b）。
// 既存の tree 生 set unit test は outcome 種別の正本として残す（本テストは補完）。
// =============================================================================

/// EMB-AT-10a / EMB-AT-11c / EMB-AT-11d: safe profile 経由で未 grant の capability-deny が
/// script の try/catch で catch 可能で、catch 後も後続処理（library process）が継続する。
///
/// safe 既定では exit / env / clock が未 grant。それぞれ try/catch で囲むと catch 可能な
/// `capability` error になり、catch 節で `e["type"]` を出力してから `print("alive")` が走る。
/// 正常終了（status_ok）と stdout（`capability` と `alive`）の両方を tree/VM で固定する。
#[test]
fn safe_capability_deny_is_catchable_via_profile() {
    for use_vm in [false, true] {
        // 未 grant 操作（exit / env / now）を try/catch で囲む。catch 節は error type を
        // 出力し、その後に後続処理（library 継続）が走ることを "alive" で示す。
        for deny_op in ["exit(0)", "env(\"HOME\")", "now()"] {
            let source =
                format!("try\n  {deny_op}\ncatch e\n  print(e[\"type\"])\nend\nprint(\"alive\")\n");
            let r = run_safe(use_vm, &[], &source);
            assert!(
                r.status_ok,
                "safe 既定で capability-deny を catch すれば正常終了する [vm={use_vm}] op={deny_op}: {}",
                r.stderr
            );
            // catch 節が capability error を捕捉し、後続の print が走る（library 継続）。
            assert_eq!(
                r.stdout, "capability\nalive",
                "catch で capability error type を得て後続処理が継続する [vm={use_vm}] op={deny_op}"
            );
        }
    }
}

/// EMB-AT-11a: `--allow-exit` 経由の exit(N) が実 process exit code N へ伝播する（Exited 写像）。
///
/// `exit_code_for_outcome`（main.rs）は `Exited { code } => code as i32`。CLI safe profile で
/// `--allow-exit` を付けて exit(N) を呼ぶと、process の実 exit code が N になることを tree/VM で固定。
/// 境界値 0 / 255 と中間値 42 を確認する。
#[test]
fn allow_exit_propagates_exit_code_via_profile() {
    for use_vm in [false, true] {
        for code in [0u8, 42, 255] {
            let source = format!("exit({code})\n");
            let (got, _stdout, stderr) = run_safe_code(use_vm, &["--allow-exit"], &source);
            assert_eq!(
                got,
                Some(i32::from(code)),
                "--allow-exit で exit({code}) は実 process exit code {code} へ伝播する [vm={use_vm}]: {stderr}"
            );
        }
    }
}

/// EMB-AT-11b: `--allow-exit` 経由でも範囲外 exit は RuntimeError 写像（exit 1）になり、
/// process は panic せず正常にプロセス終了する。
///
/// `resolve_exit`（builtin_core.rs）は `0..=255` の範囲外を catch 可能な `argument` error にし、
/// 未捕捉なら RuntimeError outcome → `exit_code_for_outcome` で exit 1。`exit(256)` と
/// `exit(0 - 1)`（= -1）の両方で exit code が Some(1)（＝正常終了・シグナルでない）であることを
/// tree/VM で固定する。
#[test]
fn allow_exit_out_of_range_maps_to_runtime_error_via_profile() {
    for use_vm in [false, true] {
        for source in ["exit(256)\n", "exit(0 - 1)\n"] {
            let (got, _stdout, _stderr) = run_safe_code(use_vm, &["--allow-exit"], source);
            assert_eq!(
                got,
                Some(1),
                "範囲外 exit は RuntimeError 写像（exit 1）で正常終了する [vm={use_vm}] src={source:?}"
            );
        }
    }
}
