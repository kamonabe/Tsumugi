//! 統合テスト: .tsg ファイルを実行して期待出力と比較するゴールデンテスト
//!
//! ツリーウォーク版（デフォルト）と VM 版（--vm）の両方で同じ fixture を回し、
//! 両実行方式で同じ言語仕様を満たすことを保証する。
//!
//! ハーネスの規約:
//! - fixture は `fixture_tests!` に1行宣言すると tree/VM 両方のテストが生成される。
//!   `fixture_declarations_match_directory` がディレクトリとの整合を検査するため、
//!   宣言漏れの fixture は残らない。
//! - 判定は完全一致。正常系は stdout（stderrは空）、エラー系は stderr と stdout の
//!   両方を検証する。OS依存の文字列だけ期待ファイル側で `{*}` に逃がせる。
//! - engine間で意図的に差が残る箇所は `<name>.expected_err.vm` で明示する。
//! - 子プロセスは必ず制限時間付きで待つ。ハングはテスト失敗として検出する。
//! - ファイルを触る fixture には実行ごとに専用の一時ディレクトリを渡す
//!   （`TSG_TEST_DIR`）。固定パスを共有しないため並列・連続実行で競合しない。

use std::path::Path;
use std::process::Command;

/// テストバイナリのパスを取得（Cargo が提供する環境変数を使用）
fn tsumugi_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsumugi")
}

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// 子プロセスの既定実行上限。ハングをテスト失敗として検出する。
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// fixture がスクリプトへ渡す一時ディレクトリのキー。
///
/// `TSUMUGI_` 始まりは処理系が保護して `env()` から読めないため、別prefixを使う。
const TEST_DIR_ENV: &str = "TSG_TEST_DIR";

/// fixture の判定方式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixtureKind {
    /// 終了コード0・stderr空・stdoutが `.expected` と完全一致
    Golden,
    /// 終了コード非0・stderrが `.expected_err` と完全一致・stdoutが `.expected_out`（既定は空）と一致
    Error,
}

/// 子プロセスの完了を待つ。制限時間を超えたら kill して失敗させる。
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: std::time::Duration,
    context: &str,
) -> std::process::Output {
    let deadline = std::time::Instant::now() + timeout;

    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.wait_with_output().unwrap_or_else(|error| {
                    panic!("{context}: プロセスの出力取得に失敗: {error}")
                });
            }
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{context}: {}秒以内に完了しませんでした", timeout.as_secs());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{context}: プロセス状態の取得に失敗: {error}");
            }
        }
    }
}

/// テスト専用の一時ディレクトリ。tree/VM・テストごとに別パスを使い、
/// 並列実行や連続実行で同じパスを共有しないようにする。
struct TestDir {
    path: std::path::PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "tsumugi-it-{}-{}-{}",
            label,
            std::process::id(),
            unique
        ));
        std::fs::remove_dir_all(&path).ok();
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|error| panic!("一時ディレクトリの作成に失敗 {path:?}: {error}"));
        Self { path }
    }

    fn as_str(&self) -> &str {
        self.path
            .to_str()
            .expect("一時ディレクトリのパスがUTF-8ではありません")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).ok();
    }
}

/// fixture 固有の追加環境変数。fixture 名からここだけで決める。
fn fixture_envs(name: &str, test_dir: &str) -> Vec<(String, String)> {
    let mut envs = vec![(TEST_DIR_ENV.to_string(), test_dir.to_string())];
    match name {
        // ファイルI/O系はテスト専用ディレクトリだけを許可する。
        // error_sandbox はその範囲外（/etc/hostname）への読み取りを拒否させる。
        "file_io" | "filesystem" | "string_utils" | "error_sandbox" => {
            envs.push(("TSUMUGI_SANDBOX".to_string(), test_dir.to_string()));
        }
        // 無限ループを短い予算で止める
        "error_step_limit" => {
            envs.push(("TSUMUGI_MAX_STEPS".to_string(), "100".to_string()));
        }
        _ => {}
    }
    envs
}

/// fixture 固有の実行上限。既定より短くするのは停止性を検証する fixture だけ。
fn fixture_timeout(name: &str) -> std::time::Duration {
    match name {
        // AUD-026: i64極値のtimestampでも定数時間で完了することの検証
        "format_time_extreme" => std::time::Duration::from_secs(2),
        _ => DEFAULT_TIMEOUT,
    }
}

/// 期待ファイルを読む。`<name>.<ext>.vm` があれば VM 版だけそちらを優先する
/// （engine間で意図的に差が残っている箇所を明示するため）。
fn read_expected(name: &str, ext: &str, use_vm: bool) -> Option<String> {
    let dir = fixtures_dir();
    if use_vm {
        let vm_specific = dir.join(format!("{}.{}.vm", name, ext));
        if vm_specific.exists() {
            return Some(normalize(
                &std::fs::read_to_string(&vm_specific)
                    .unwrap_or_else(|e| panic!("期待ファイルが読めません {vm_specific:?}: {e}")),
            ));
        }
    }
    let shared = dir.join(format!("{}.{}", name, ext));
    if !shared.exists() {
        return None;
    }
    Some(normalize(&std::fs::read_to_string(&shared).unwrap_or_else(
        |e| panic!("期待ファイルが読めません {shared:?}: {e}"),
    )))
}

/// 改行を LF に揃え、末尾の空白を落とす
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end().to_string()
}

/// 期待テキストと実出力を比較する。
///
/// 既定は完全一致。期待側の `{*}` だけはワイルドカードとして扱い、
/// OS・ロケール依存の文字列（`io::Error` のメッセージ等）を逃がす。
/// 逃がす範囲を期待ファイル上で明示するため、部分一致は導入しない。
fn matches_expected(actual: &str, expected: &str) -> bool {
    const WILDCARD: &str = "{*}";
    if !expected.contains(WILDCARD) {
        return actual == expected;
    }

    let parts: Vec<&str> = expected.split(WILDCARD).collect();
    let last = parts.len() - 1;
    let mut rest = actual;
    for (index, part) in parts.iter().enumerate() {
        if index == 0 {
            let Some(tail) = rest.strip_prefix(part) else {
                return false;
            };
            rest = tail;
        } else if index == last {
            if rest.len() < part.len() || !rest.ends_with(part) {
                return false;
            }
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    true
}

/// fixture を1つ実行して stdout / stderr / 終了コードを検証する
fn run_fixture(name: &str, kind: FixtureKind, use_vm: bool) {
    let mode = if use_vm { "VM" } else { "tree-walk" };
    let label = format!("{}-{}", name, if use_vm { "vm" } else { "tree" });
    let test_dir = TestDir::new(&label);
    let script = fixtures_dir().join(format!("{}.tsg", name));
    assert!(script.exists(), "fixtureがありません: {script:?}");

    let mut command = Command::new(tsumugi_bin());
    if use_vm {
        command.arg("--vm");
    }
    command
        .arg(&script)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in fixture_envs(name, test_dir.as_str()) {
        command.env(key, value);
    }

    let context = format!("{name} [{mode}]");
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("{context}: tsumugi バイナリの起動に失敗: {error}"));
    let output = wait_with_timeout(child, fixture_timeout(name), &context);

    let stdout = normalize(&String::from_utf8_lossy(&output.stdout));
    let stderr = normalize(&String::from_utf8_lossy(&output.stderr));
    assert!(
        !stderr.contains("panicked at"),
        "{context}: host panicが発生しました\n--- stderr ---\n{stderr}"
    );

    match kind {
        FixtureKind::Golden => {
            let expected = read_expected(name, "expected", use_vm)
                .unwrap_or_else(|| panic!("{context}: .expected がありません"));
            assert!(
                output.status.success(),
                "{context}: 正常系が異常終了しました\n--- stderr ---\n{stderr}"
            );
            assert_eq!(
                stderr, "",
                "{context}: 正常系で診断が出力されました\n--- stderr ---\n{stderr}"
            );
            assert!(
                matches_expected(&stdout, &expected),
                "{context}: stdoutが期待出力と一致しません\n--- 実際 ---\n{stdout}\n--- 期待 ---\n{expected}"
            );
        }
        FixtureKind::Error => {
            let expected_err = read_expected(name, "expected_err", use_vm)
                .unwrap_or_else(|| panic!("{context}: .expected_err がありません"));
            let expected_out = read_expected(name, "expected_out", use_vm).unwrap_or_default();
            assert!(
                !output.status.success(),
                "{context}: エラー系が終了コード0で終了しました\n--- stdout ---\n{stdout}"
            );
            assert!(
                matches_expected(&stderr, &expected_err),
                "{context}: stderrが期待エラーと一致しません\n--- 実際 ---\n{stderr}\n--- 期待 ---\n{expected_err}"
            );
            assert!(
                matches_expected(&stdout, &expected_out),
                "{context}: エラー前後のstdout副作用が期待と一致しません\n--- 実際 ---\n{stdout}\n--- 期待 ---\n{expected_out}"
            );
        }
    }
}

/// fixture 以外の任意スクリプトを timeout 付きで実行する（bespokeテスト用）
fn run_script_process(
    script: &std::path::Path,
    use_vm: bool,
    envs: &[(&str, &str)],
    context: &str,
) -> std::process::Output {
    let mut command = Command::new(tsumugi_bin());
    if use_vm {
        command.arg("--vm");
    }
    command
        .arg(script)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("{context}: tsumugi バイナリの起動に失敗: {error}"));
    wait_with_timeout(child, DEFAULT_TIMEOUT, context)
}

/// fixture 宣言から tree/VM 両方の `#[test]` を生成する。
///
/// 1行の宣言で必ず両engine分が作られるため、片側の登録漏れが起きない。
/// 生成されるテスト名は `<fixture>::tree` / `<fixture>::vm`。
macro_rules! fixture_tests {
    ($table:ident, $kind:expr, [ $($name:ident),* $(,)? ]) => {
        /// このテーブルに宣言された fixture 名（ディレクトリとの整合を検査する）
        const $table: &[&str] = &[ $(stringify!($name)),* ];

        $(
            mod $name {
                #[test]
                fn tree() {
                    super::run_fixture(stringify!($name), $kind, false);
                }

                #[test]
                fn vm() {
                    super::run_fixture(stringify!($name), $kind, true);
                }
            }
        )*
    };
}

// =============================================================
// fixture 宣言（tree/VM 両方が自動生成される）
// =============================================================

fixture_tests!(
    GOLDEN_FIXTURES,
    crate::FixtureKind::Golden,
    [
        and_or_scope,
        arithmetic,
        assign,
        block_scope_semantics,
        break_continue,
        builtins,
        call_validation_order,
        closure,
        closure_capture_scope,
        closure_counter,
        closure_try_catch,
        comparison_semantics,
        control_flow,
        deep_closure,
        dict_utils,
        edge_cases,
        error_structured,
        file_io,
        filesystem,
        first_class_fn,
        fizzbuzz,
        float_special,
        for_closure_binding,
        for_iteration_snapshot,
        for_loop,
        format_time_extreme,
        fstring,
        hello,
        higher_order,
        import_basic,
        import_circular,
        import_nested,
        import_static_resolution,
        index_assign_binding,
        index_read_lowering,
        list_dict,
        local_utils,
        logic,
        map_recursion_limit,
        numeric_utils,
        overflow_edge,
        runtime_global_import,
        runtime_global_name_resolution,
        scope_isolation,
        slice_edge,
        sort_numeric,
        string_utils,
        try_break_continue,
        try_catch,
    ]
);

fixture_tests!(
    ERROR_FIXTURES,
    crate::FixtureKind::Error,
    [
        error_assign_undefined,
        error_break_outside_loop,
        error_continue_outside_loop,
        error_dict_key_type,
        error_fstring_extra,
        error_import_before_side_effects,
        error_import_non_top_level,
        error_import_not_found,
        error_index_out_of_bounds,
        error_integer_overflow,
        error_parse,
        error_parse_multi,
        error_return_outside_fn,
        error_sandbox,
        error_stack_trace,
        error_step_limit,
        error_type,
        error_unclosed_string,
        error_unclosed_string_eof,
        error_undefined_fn,
        error_undefined_var,
        error_unknown_char,
        error_wrong_arg_count,
        error_zero_division,
    ]
);

/// 期待ファイルを持たず、専用テストから実行する fixture。
const CUSTOM_FIXTURES: &[&str] = &[
    // 環境変数の組み合わせを複数回すため専用テストで実行する
    "env_allow",
    "env_protected_windows",
    // engine間でtrace frame数が異なる（AUD-017）ため専用テストで検証する
    "error_stack_overflow",
];

/// 他の fixture から import される補助ファイル（単体では実行しない）。
const HELPER_FIXTURES: &[&str] = &[
    "import_bad_syntax",
    "import_circular_a",
    "import_circular_b",
    "import_lib",
    "import_nested_base",
    "import_nested_mid",
    "runtime_global_failed_import",
    "runtime_global_import_lib",
];

/// 期待ファイルを持つ fixture が必ず宣言テーブルに載っていることを検査する。
///
/// 宣言漏れの fixture が誰にも実行されないまま残る状態を防ぐ。
#[test]
fn fixture_declarations_match_directory() {
    let dir = fixtures_dir();
    let entries = std::fs::read_dir(&dir).expect("fixtureディレクトリが読めません");

    let mut golden_files = Vec::new();
    let mut error_files = Vec::new();
    for entry in entries {
        let path = entry.expect("fixtureエントリが読めません").path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // `.vm` / `.expected_out` は補助ファイルなので一覧の対象にしない
        if let Some(stem) = file_name.strip_suffix(".expected") {
            golden_files.push(stem.to_string());
        } else if let Some(stem) = file_name.strip_suffix(".expected_err") {
            error_files.push(stem.to_string());
        }
    }
    golden_files.sort();
    error_files.sort();

    for (label, files, declared) in [
        ("正常系", &golden_files, GOLDEN_FIXTURES),
        ("エラー系", &error_files, ERROR_FIXTURES),
    ] {
        for file in files {
            assert!(
                declared.contains(&file.as_str()),
                "{label} fixture `{file}` が宣言テーブルに登録されていません。\
                 fixture_tests! へ追加してください"
            );
        }
        for name in declared {
            assert!(
                files.iter().any(|file| file == name),
                "{label} fixture `{name}` が宣言されていますが期待ファイルがありません"
            );
            assert!(
                dir.join(format!("{}.tsg", name)).exists(),
                "{label} fixture `{name}` の .tsg がありません"
            );
        }
    }

    // すべての .tsg がいずれかの分類に属していること（放置された fixture の検出）
    for entry in std::fs::read_dir(&dir).expect("fixtureディレクトリが読めません") {
        let path = entry.expect("fixtureエントリが読めません").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".tsg") else {
            continue;
        };
        let classified = GOLDEN_FIXTURES.contains(&stem)
            || ERROR_FIXTURES.contains(&stem)
            || CUSTOM_FIXTURES.contains(&stem)
            || HELPER_FIXTURES.contains(&stem);
        assert!(
            classified,
            "fixture `{stem}.tsg` がどのテストからも実行されていません。\
             fixture_tests! / CUSTOM_FIXTURES / HELPER_FIXTURES のいずれかへ登録してください"
        );
    }
    for name in CUSTOM_FIXTURES.iter().chain(HELPER_FIXTURES) {
        assert!(
            dir.join(format!("{}.tsg", name)).exists(),
            "宣言された fixture `{name}.tsg` がありません"
        );
    }
}
// =============================================================
// スタックオーバーフロー（コールフレーム深度制限）テスト
// =============================================================

/// 深度上限エラーとスタックトレースを厳密に検証する。
///
/// trace frame数だけは engine 間で異なる（VMは上限128にtop-level frameを含めるため
/// 許容user frameが1少ない = AUD-017）。129行の期待ファイルを置く代わりに、
/// 差分を数値として明示し、AUD-017の修正時にこのテストが落ちるようにしている。
#[test]
fn stack_overflow_reports_depth_limit_with_trace() {
    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree-walk" };
        let context = format!("error_stack_overflow [{mode}]");
        let script = fixtures_dir().join("error_stack_overflow.tsg");
        let output = run_script_process(&script, use_vm, &[], &context);
        let stdout = normalize(&String::from_utf8_lossy(&output.stdout));
        let stderr = normalize(&String::from_utf8_lossy(&output.stderr));
        let lines: Vec<&str> = stderr.lines().collect();

        assert!(
            !output.status.success(),
            "{context}: 終了コード0で終了しました"
        );
        assert_eq!(stdout, "", "{context}: 予期しないstdout: {stdout}");
        assert_eq!(
            lines.first().copied(),
            Some("3行目: スタックオーバーフロー: 再帰が深すぎます (上限: 128)"),
            "{context}: 先頭のエラー行が一致しません: {stderr}"
        );

        let expected_frames = if use_vm { 127 } else { 128 };
        assert_eq!(
            lines.len(),
            expected_frames + 1,
            "{context}: trace行数が期待と異なります: {}",
            lines.len()
        );
        for line in &lines[1..expected_frames] {
            assert_eq!(
                *line, "  in recurse() (3行目)",
                "{context}: 再帰フレームの表示が一致しません: {line}"
            );
        }
        assert_eq!(
            lines.last().copied(),
            Some("  in recurse() (6行目)"),
            "{context}: 最外のcall元フレームが一致しません: {stderr}"
        );
    }
}

// =============================================================
// サンドボックス境界テスト（import 先の検証）
// =============================================================

#[test]
fn import_outside_sandbox_is_blocked_in_both_engines() {
    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree-walk" };
        let label = format!("sandbox-import-{}", if use_vm { "vm" } else { "tree" });
        let dir = TestDir::new(&label);

        // サンドボックス外の実在ファイルと、それをimportするスクリプトを用意する
        let outside = dir.path.join("outside.tsg");
        std::fs::write(&outside, "print(\"leaked\")").expect("import先の作成に失敗");
        let script = dir.path.join("main.tsg");
        std::fs::write(
            &script,
            format!(
                "import \"{}\"",
                outside
                    .to_str()
                    .expect("パスがUTF-8ではありません")
                    .replace('\\', "/")
            ),
        )
        .expect("スクリプトの作成に失敗");

        // サンドボックスは fixtures ディレクトリのみ → 一時ディレクトリのimport先は許可外
        let sandbox = fixtures_dir();
        let context = format!("sandbox import [{mode}]");
        let output = run_script_process(
            &script,
            use_vm,
            &[(
                "TSUMUGI_SANDBOX",
                sandbox.to_str().expect("パスがUTF-8ではありません"),
            )],
            &context,
        );
        let stdout = normalize(&String::from_utf8_lossy(&output.stdout));
        let stderr = normalize(&String::from_utf8_lossy(&output.stderr));

        assert!(
            !output.status.success(),
            "{context}: 終了コード0で終了しました\n--- stderr ---\n{stderr}"
        );
        assert!(
            stderr.contains("サンドボックス違反"),
            "{context}: importのサンドボックスエラーが出ません\n--- stderr ---\n{stderr}"
        );
        assert_eq!(
            stdout, "",
            "{context}: 許可外のimport先が実行されました\n--- stdout ---\n{stdout}"
        );
        assert!(
            !stderr.contains("leaked"),
            "{context}: import先の出力が漏洩しました\n--- stderr ---\n{stderr}"
        );
    }
}

#[test]
fn env_allow_list() {
    let dir = fixtures_dir();
    let script = dir.join("env_allow.tsg");

    // テスト側で制御可能な環境変数を設定して検証
    let output = run_script_process(
        &script,
        false,
        &[
            ("TSUMUGI_ENV_ALLOW", "TSG_TEST_ALLOWED,TSUMUGI_*"),
            ("TSG_TEST_ALLOWED", "visible_value"),
            ("SECRET_DB_PASS", "hunter2"),
        ],
        "env allow list",
    );

    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    assert!(
        stdout.contains("allowed_ok: true"),
        "TSG_TEST_ALLOWED should be accessible: {}",
        stdout
    );
    assert!(
        stdout.contains("secret_blocked: true"),
        "SECRET_DB_PASS should be blocked: {}",
        stdout
    );
    assert!(output.status.success());
}

#[cfg_attr(not(windows), allow(dead_code))]
fn run_windows_protected_env_keys(use_vm: bool) {
    const SECRET_MARKER: &str = "AUD031_WINDOWS_SECRET_MUST_NOT_LEAK";

    let dir = fixtures_dir();
    let script = dir.join("env_protected_windows.tsg");
    let expected = [
        "uppercase: null",
        "lowercase: null",
        "mixed_case: null",
        "long_s: null",
        "dotless_i: null",
        "sandbox_lowercase: null",
    ]
    .join("\n");

    for allow_all in [false, true] {
        let mut cmd = Command::new(tsumugi_bin());
        if use_vm {
            cmd.arg("--vm");
        }
        cmd.arg(script.to_str().unwrap())
            .env("TSUMUGI_SANDBOX", &dir);

        for key in [
            "TSUMUGI_AUD031_SECRET",
            "tsumugi_aud031_secret",
            "TsUmUgI_AuD031_SeCrEt",
            "TſUMUGI_AUD031_SECRET",
            "TSUMUGı_AUD031_SECRET",
        ] {
            cmd.env(key, SECRET_MARKER);
        }

        if allow_all {
            cmd.env("TSUMUGI_ENV_ALLOW", "*");
        } else {
            cmd.env_remove("TSUMUGI_ENV_ALLOW");
        }

        let output = cmd.output().expect("tsumugi バイナリの実行に失敗");
        let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
        let stderr = String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n");
        let mode = if use_vm { "VM" } else { "tree-walk" };
        let allow_mode = if allow_all {
            "allow-list=*"
        } else {
            "allow-list未設定"
        };

        assert!(
            output.status.success(),
            "Windows環境変数保護テストが異常終了しました [{mode}, {allow_mode}]\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        );
        assert!(
            !stdout.contains(SECRET_MARKER),
            "保護対象の環境変数値がstdoutへ漏洩しました [{mode}, {allow_mode}]"
        );
        assert!(
            !stderr.contains(SECRET_MARKER),
            "保護対象の環境変数値がstderrへ漏洩しました [{mode}, {allow_mode}]"
        );
        assert_eq!(
            stdout.trim_end(),
            expected,
            "Windows環境変数保護テスト失敗 [{mode}, {allow_mode}]\n--- stdout ---\n{stdout}"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_protected_env_keys_tree_walk() {
    run_windows_protected_env_keys(false);
}

#[cfg(windows)]
#[test]
fn windows_protected_env_keys_vm() {
    run_windows_protected_env_keys(true);
}

// =============================================================
// リグレッションテスト: user function call validation順序
// =============================================================

#[test]
fn call_budget_is_checked_before_callee_in_both_engines() {
    let rejected_source = concat!(
        "fn make_step_callee()\n",
        "    print(\"step-callee-ran\")\n",
        "    return fn() null end\n",
        "end\n",
        "try\n",
        "    let unused = make_step_callee()()\n",
        "catch step_error\n",
        "    print(step_error[\"type\"])\n",
        "end\n",
    );
    let exact_once_source = concat!(
        "fn once()\n",
        "    print(\"once-body\")\n",
        "    return 1\n",
        "end\n",
        "print(once())\n",
    );

    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree" };
        let rejected = run_repl_process(rejected_source, use_vm, &[("TSUMUGI_MAX_STEPS", "0")]);
        let (rejected_stdout, rejected_stderr) = output_text(&rejected);

        assert!(
            rejected.status.success(),
            "{mode} REPLが異常終了: {rejected_stderr}"
        );
        assert!(
            rejected_stderr.is_empty(),
            "{mode}で捕捉外の診断: {rejected_stderr}"
        );
        assert!(
            !rejected_stdout.contains("step-callee-ran"),
            "{mode}でstep検査前にcalleeを評価: {rejected_stdout}"
        );
        assert_eq!(
            rejected_stdout.matches("limit\n").count(),
            1,
            "{mode}でstep errorの捕捉結果が不正: {rejected_stdout}"
        );

        let exact_once = run_repl_process(exact_once_source, use_vm, &[("TSUMUGI_MAX_STEPS", "1")]);
        let (exact_once_stdout, exact_once_stderr) = output_text(&exact_once);
        assert!(
            exact_once.status.success(),
            "{mode} REPLが異常終了: {exact_once_stderr}"
        );
        assert!(
            exact_once_stderr.is_empty(),
            "{mode}でcall stepを二重count: {exact_once_stderr}"
        );
        assert_eq!(
            exact_once_stdout.matches("once-body\n").count(),
            1,
            "{mode}でcallを正確に1 stepとして実行していない: {exact_once_stdout}"
        );
    }
}

#[test]
fn invalid_call_repl_recovers_without_argument_effects() {
    let source = concat!(
        "fn zero()\n",
        "    return 0\n",
        "end\n",
        "zero(print(\"invalid-arg-ran\"))\n",
        "let recovered = \"alive\"\n",
        "fn read_recovered()\n",
        "    return recovered\n",
        "end\n",
        "print(read_recovered())\n",
    );

    for use_vm in [false, true] {
        let output = run_repl_process(source, use_vm, &[]);
        let (stdout, stderr) = output_text(&output);
        let mode = if use_vm { "VM" } else { "tree" };

        assert!(output.status.success(), "{mode} REPLが異常終了: {stderr}");
        assert!(
            stderr.contains("引数"),
            "{mode}でarity errorがない: {stderr}"
        );
        assert!(
            !stdout.contains("invalid-arg-ran"),
            "{mode}でinvalid callの引数副作用が発生: {stdout}"
        );
        assert!(
            stdout.contains("alive\n"),
            "{mode}でvalidation error後の入力を実行できない: {stdout}"
        );
        assert!(
            !stderr.contains("panicked at"),
            "{mode}でhost panic: {stderr}"
        );
    }
}

// =============================================================
// リグレッションテスト: runtime global name visibility
// =============================================================

#[test]
fn runtime_globals_resolve_across_repl_submissions() {
    let source = concat!(
        "fn repl_read()\n",
        "    return repl_later\n",
        "end\n",
        "fn repl_write(value)\n",
        "    repl_mutable = value\n",
        "end\n",
        "fn repl_even(n)\n",
        "    if n == 0\n",
        "        return true\n",
        "    end\n",
        "    return repl_odd(n - 1)\n",
        "end\n",
        "fn repl_odd(n)\n",
        "    if n == 0\n",
        "        return false\n",
        "    end\n",
        "    return repl_even(n - 1)\n",
        "end\n",
        "let repl_later = \"repl-later\"\n",
        "let repl_mutable = \"before\"\n",
        "print(repl_read())\n",
        "repl_write(\"repl-after\")\n",
        "print(repl_mutable)\n",
        "print(repl_even(6))\n",
    );

    for use_vm in [false, true] {
        let output = run_repl_process(source, use_vm, &[]);
        let (stdout, stderr) = output_text(&output);
        let mode = if use_vm { "VM" } else { "tree" };
        let visible_output = repl_visible_lines(&stdout, use_vm);

        assert!(output.status.success(), "{mode} REPLが異常終了: {stderr}");
        assert!(stderr.is_empty(), "{mode} REPLで予期しない診断: {stderr}");
        assert_eq!(
            visible_output,
            ["repl-later", "repl-after", "true"],
            "{mode}で入力間forward globalまたはmutual recursionが不正: {stdout}"
        );
    }
}

#[test]
fn vm_repl_rolls_back_runtime_global_registry_after_error() {
    let source = concat!(
        "import \"tests/fixtures/runtime_global_failed_import.tsg\"\n",
        "print(failed_global)\n",
        "let failed_global = \"recovered\"\n",
        "print(failed_global)\n",
    );
    let output = run_repl_process(source, true, &[]);
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success(), "VM REPLが異常終了: {stderr}");
    assert_eq!(
        stderr
            .matches("未定義の変数: missing_in_failed_import")
            .count(),
        1,
        "import内の元のruntime errorが不正: {stderr}"
    );
    assert_eq!(
        stderr.matches("未定義の変数: failed_global").count(),
        1,
        "失敗入力のglobal registry entryが残留: {stderr}"
    );
    assert!(
        stdout.contains("recovered\n"),
        "rollback後に同名globalを再定義できない: {stdout}"
    );
    assert!(
        !stderr.contains("global registryのslotが不正"),
        "rollback後にstale global slotを参照: {stderr}"
    );
    assert!(!stderr.contains("panicked at"), "host panic: {stderr}");
}

// =============================================================
// リグレッションテスト: レキシカルスコープ・locals_cells リーク防止
// =============================================================

#[test]
fn vm_repl_for_closure_cells_survive_slot_reuse() {
    let source = concat!(
        "let saved = []\n",
        "for i in [1, 2, 3]\n",
        "    push(saved, fn() i end)\n",
        "end\n",
        "let reused_collection = \"reused-collection\"\n",
        "let reused_index = \"reused-index\"\n",
        "let reused_var = \"reused-var\"\n",
        "let reused_reader = fn() reused_var end\n",
        "print(reused_reader())\n",
        "print(saved[0]())\n",
        "print(saved[1]())\n",
        "print(saved[2]())\n",
        "reused_var = \"changed-var\"\n",
        "print(reused_reader())\n",
        "print(saved[0]())\n",
        "print(saved[1]())\n",
        "print(saved[2]())\n",
    );
    let output = run_repl_process(source, true, &[]);
    let (stdout, stderr) = output_text(&output);
    let visible_output = repl_visible_lines(&stdout, true);

    assert!(output.status.success(), "VM REPLが異常終了: {stderr}");
    assert!(stderr.is_empty(), "VM REPLで予期しない診断: {stderr}");
    assert_eq!(
        visible_output,
        ["reused-var", "1", "2", "3", "changed-var", "1", "2", "3"],
        "loop slotの再利用でescaping closureのcellが変化: {stdout}"
    );
}

#[test]
fn vm_repl_recovers_structure_after_for_closure_error() {
    let source = concat!(
        "for i in [1]\n",
        "    let doomed = fn() i end\n",
        "    let failed = 1 / 0\n",
        "end\n",
        "let recovered_collection = \"unused-collection-slot\"\n",
        "let recovered_index = \"unused-index-slot\"\n",
        "let recovered_var = \"recovered\"\n",
        "let recovered_reader = fn() recovered_var end\n",
        "print(recovered_reader())\n",
        "print(\"alive\")\n",
    );
    let output = run_repl_process(source, true, &[]);
    let (stdout, stderr) = output_text(&output);
    let visible_output = repl_visible_lines(&stdout, true);

    assert!(output.status.success(), "VM REPLが異常終了: {stderr}");
    assert_eq!(
        stderr.matches("ゼロ除算").count(),
        1,
        "元のruntime errorが正確に報告されていない: {stderr}"
    );
    assert_eq!(
        visible_output,
        ["recovered", "alive"],
        "失敗したforのstack/cell状態が後続入力へ残留: {stdout}"
    );
    assert!(!stderr.contains("panicked at"), "host panic: {stderr}");
}

#[test]
fn repl_control_flow_block_locals_do_not_leak() {
    let source = "let escaped_if = null\n\
                  if true\n    let if_local = \"if-cell\"\n    escaped_if = fn() if_local end\nend\n\
                  let reused_if = \"after-if\"\n\
                  print(reused_if)\n\
                  print(escaped_if())\n\
                  print(if_local)\n\
                  let escaped_try = null\n\
                  try\n    let try_local = \"try-cell\"\n    escaped_try = fn() try_local end\ncatch unused\n    print(\"unexpected-normal-catch\")\nend\n\
                  let reused_try = \"after-try\"\n\
                  print(reused_try)\n\
                  print(escaped_try())\n\
                  print(try_local)\n\
                  let escaped_catch = null\n\
                  try\n    let failed = 1 / 0\ncatch catch_error\n    let catch_local = \"catch-cell\"\n    escaped_catch = fn() catch_local + \":\" + catch_error[\"type\"] end\nend\n\
                  let reused_catch = \"after-catch\"\n\
                  print(reused_catch)\n\
                  print(escaped_catch())\n\
                  print(catch_error)\n\
                  print(catch_local)\n\
                  try\n    let try_only = 4\n    let failed = 1 / 0\ncatch separated\n    print(try_only)\nend\n\
                  let reused_failed = \"after-failed\"\n\
                  print(reused_failed)\n\
                  print(separated)\n\
                  print(\"alive\")\n";
    let expected_output = [
        "after-if",
        "if-cell",
        "after-try",
        "try-cell",
        "after-catch",
        "catch-cell:zero_division",
        "after-failed",
        "alive",
    ];

    for use_vm in [false, true] {
        let output = run_repl_process(source, use_vm, &[]);
        let (stdout, stderr) = output_text(&output);
        let mode = if use_vm { "VM" } else { "tree" };
        let visible_output = repl_visible_lines(&stdout, use_vm);
        let diagnostics: Vec<_> = stderr
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();

        assert!(output.status.success(), "{mode} REPLが異常終了: {stderr}");
        assert_eq!(
            visible_output, expected_output,
            "{mode}でslot再利用・escaping closure・後続実行が不正: {stdout}"
        );
        assert_eq!(
            diagnostics.len(),
            6,
            "{mode}で予期しない診断が発生: {stderr}"
        );
        for name in [
            "if_local",
            "try_local",
            "catch_error",
            "catch_local",
            "try_only",
            "separated",
        ] {
            let expected = format!("未定義の変数: {name}");
            assert_eq!(
                stderr.matches(&expected).count(),
                1,
                "{mode}で{name}のscopeまたは診断が不正: {stderr}"
            );
        }
        assert!(!stdout.contains("unexpected-normal-catch"));
        assert!(
            !stderr.contains("panicked at"),
            "{mode}でhost panic: {stderr}"
        );
    }
}

// =============================================================
// 深層監査リグレッション: REPL transaction / 状態回復 / 資源上限
// =============================================================

fn run_repl_process(source: &str, use_vm: bool, envs: &[(&str, &str)]) -> std::process::Output {
    use std::io::Write as _;

    let mut command = Command::new(tsumugi_bin());
    if use_vm {
        command.arg("--vm");
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }

    let mut child = command.spawn().expect("REPLプロセスの起動に失敗");
    let mut stdin = child.stdin.take().expect("REPL stdinの取得に失敗");
    stdin
        .write_all(source.as_bytes())
        .expect("REPL stdinへの書き込みに失敗");
    drop(stdin); // EOFを送り、REPLを終了させる

    let mode = if use_vm { "VM" } else { "tree-walk" };
    wait_with_timeout(child, DEFAULT_TIMEOUT, &format!("REPL [{mode}]"))
}

/// REPLのstdoutからプロンプトを除いた出力行だけを取り出す。
///
/// tree/VMでプロンプト文字列が異なるため、行の比較にはこの正規化を使う。
fn repl_visible_lines(stdout: &str, use_vm: bool) -> Vec<&str> {
    let prompt = if use_vm { "tsumugi:vm> " } else { "tsumugi> " };
    stdout
        .lines()
        .filter_map(|line| {
            line.rsplit_once(prompt)
                .map(|(_, value)| value)
                .filter(|value| !value.is_empty())
        })
        .collect()
}

fn output_text(output: &std::process::Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n"),
    )
}

#[test]
fn vm_repl_recovers_after_compile_error() {
    // ブロック内でlocalを追加した後にcompile errorへ到達させ、Compilerだけが
    // 更新された状態を次入力へ持ち越さないことを検証する。
    let output = run_repl_process(
        "if true\n    let ghost = 1\n    break\nend\n\
         let live = 2\nprint(live)\nprint(ghost)\n",
        true,
        &[],
    );
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success(), "VM REPLが異常終了: {stderr}");
    assert!(
        stdout.contains("2\n"),
        "正常な次入力が実行されていない: {stdout}"
    );
    assert!(
        stderr.contains("break はループの中でのみ使用できます"),
        "元のcompile errorがない: {stderr}"
    );
    assert!(
        stderr.contains("未定義の変数: ghost"),
        "失敗入力のlocalが残留: {stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "host panicが再発: {stderr}"
    );
}

/// AUD-043: トップレベル`return`を構文エラーとして拒否し、次入力の状態を壊さない。
///
/// 旧実装のVMでは`ReturnValue`がtop-level frameをpopしstackを`base`まで捨てる一方、
/// Compilerの`locals`が残るため、次入力の`GetLocal`が空stackを読みhost panicへ到達した。
/// treeは同じ入力で継続していたため、engine間の挙動差も併せて固定する。
#[test]
fn repl_rejects_top_level_return_without_host_panic() {
    let cases = [
        ("bare", "let x = 1\nreturn 0\nprint(x)\n"),
        (
            "in-try",
            "let x = 1\ntry\n    return 0\ncatch e\n    print(e[\"type\"])\nend\nprint(x)\n",
        ),
        (
            "in-for",
            "let x = 1\nfor i in [1, 2]\n    return 0\nend\nprint(x)\n",
        ),
    ];

    for (label, source) in cases {
        for use_vm in [false, true] {
            let output = run_repl_process(source, use_vm, &[]);
            let (stdout, stderr) = output_text(&output);
            let mode = if use_vm { "VM" } else { "tree" };

            assert!(
                !stderr.contains("panicked at"),
                "{mode}/{label}でhost panicが発生: {stderr}"
            );
            assert!(
                output.status.success(),
                "{mode}/{label}でREPLが異常終了: {stderr}"
            );
            assert_eq!(
                stderr
                    .matches("return は関数の中でのみ使用できます")
                    .count(),
                1,
                "{mode}/{label}でreturnの配置エラーが1件報告されていない: {stderr}"
            );
            assert!(
                repl_visible_lines(&stdout, use_vm).contains(&"1"),
                "{mode}/{label}で失敗入力後にtop-level bindingを読めない: {stdout}"
            );
        }
    }
}

#[test]
fn vm_repl_recovers_after_runtime_error() {
    let output = run_repl_process(
        "fn boom()\n    let temp = 1\n    let bad = 1 / 0\n    print(\"SHOULD_NOT_RUN\")\nend\n\
         boom()\nlet live = 222\nprint(live)\n",
        true,
        &[],
    );
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success(), "VM REPLが異常終了: {stderr}");
    assert!(
        stderr.contains("ゼロ除算"),
        "runtime errorが報告されていない: {stderr}"
    );
    assert!(
        !stdout.contains("SHOULD_NOT_RUN"),
        "失敗したcalleeが次入力で再開された: {stdout}"
    );
    assert!(
        stdout.contains("222\n"),
        "rollback後のlocal値が不正: {stdout}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "host panicが再発: {stderr}"
    );
}

#[test]
fn vm_repl_preserves_top_level_and_try_cells() {
    let output = run_repl_process(
        "let x = 1\n\
         fn get()\n    return x\nend\n\
         x = 2\nprint(get())\n\
         try\n    let local = 7\n    let capture = fn() local end\n    let bad = 1 / 0\ncatch e\n    print(e[\"type\"])\nend\n",
        true,
        &[],
    );
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success(), "VM REPLが異常終了: {stderr}");
    assert!(stderr.is_empty(), "捕捉済みエラー以外が発生: {stderr}");
    assert!(
        stdout.contains("2\n"),
        "top-level cellとclosureが分離: {stdout}"
    );
    assert!(
        stdout.contains("zero_division\n"),
        "catch変数slotがtry local cellと衝突: {stdout}"
    );
}

#[test]
fn tree_repl_cleans_loop_scope_after_caught_error() {
    let output = run_repl_process(
        "try\n    while true\n        let leaked = 42\n        print(1 / 0)\n    end\ncatch e\n    print(\"caught\")\nend\n\
         print(leaked)\n",
        false,
        &[],
    );
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success(), "tree REPLが異常終了: {stderr}");
    assert!(
        stdout.contains("caught\n"),
        "エラーがcatchされていない: {stdout}"
    );
    assert!(!stdout.contains("42\n"), "loop localがREPLへ漏洩: {stdout}");
    assert!(
        stderr.contains("未定義の変数: leaked"),
        "漏洩検査が期待どおり失敗しない: {stderr}"
    );
}

#[test]
fn tree_repl_resets_step_budget_per_submission() {
    let output = run_repl_process(
        "fn one()\n    return 1\nend\nprint(one())\nprint(one())\n",
        false,
        &[("TSUMUGI_MAX_STEPS", "1")],
    );
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success(), "tree REPLが異常終了: {stderr}");
    assert!(stderr.is_empty(), "入力間でstep予算が累積: {stderr}");
    assert_eq!(
        stdout.matches("1\n").count(),
        2,
        "各入力が独立予算で実行されていない: {stdout}"
    );
}

#[test]
fn tree_repl_retries_failed_import() {
    let output = run_repl_process(
        "import \"tests/fixtures/import_bad_syntax.tsg\"\n\
         import \"tests/fixtures/import_bad_syntax.tsg\"\n\
         import \"tests/fixtures/import_lib.tsg\"\n\
         print(add(3, 4))\n",
        false,
        &[],
    );
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success(), "tree REPLが異常終了: {stderr}");
    assert_eq!(
        stderr
            .matches("import 失敗 (tests/fixtures/import_bad_syntax.tsg)")
            .count(),
        2,
        "失敗したimportがloaded扱いになったか、base_dirが復元されていない: {stderr}"
    );
    assert!(
        stdout.contains("7\n"),
        "失敗後の相対importまたは後続実行に失敗: {stdout}\n--- stderr ---\n{stderr}"
    );
}

#[test]
fn collection_limit_is_consistent_in_both_engines() {
    let source = "print([1, 2, 3])\n\
                  print({\"a\": 1, \"b\": 2, \"c\": 3})\n\
                  let xs = []\npush(xs, 1)\npush(xs, 2)\npush(xs, 3)\nprint(xs)\n";

    for use_vm in [false, true] {
        let output = run_repl_process(source, use_vm, &[("TSUMUGI_MAX_COLLECTION_SIZE", "2")]);
        let (stdout, stderr) = output_text(&output);
        let mode = if use_vm { "VM" } else { "tree" };

        assert!(output.status.success(), "{mode} REPLが異常終了: {stderr}");
        assert_eq!(
            stderr.matches("コレクションサイズ上限超過").count(),
            3,
            "{mode}でliteral/pushの上限適用が不一致: {stderr}"
        );
        assert!(
            stdout.contains("[1, 2]\n"),
            "{mode}で失敗したpushが部分commit: {stdout}"
        );
    }
}

#[test]
fn index_assign_recovers_and_writes_across_inputs_in_both_engines() {
    // 未定義targetはcompile errorではなくcatch可能なruntime errorとして扱い、
    // 入力をまたいでも同じbindingへ書き込めること。
    let source = "let shared = [1, 2]\n\
                  missing_target[0] = 1\n\
                  shared[0] = 9\nprint(shared)\n\
                  fn write_shared()\n    shared[1] = 8\nend\n\
                  write_shared()\nprint(shared)\n\
                  let capture = fn()\n    shared[0] = 7\nend\n\
                  capture()\nprint(shared)\n";

    for use_vm in [false, true] {
        let output = run_repl_process(source, use_vm, &[]);
        let (stdout, stderr) = output_text(&output);
        let mode = if use_vm { "VM" } else { "tree" };

        assert!(output.status.success(), "{mode} REPLが異常終了: {stderr}");
        assert!(
            stderr.contains("未定義の変数: missing_target"),
            "{mode}で未定義targetが報告されていない: {stderr}"
        );
        assert!(
            stdout.contains("[9, 2]\n"),
            "{mode}で失敗入力後のindex代入が実行されていない: {stdout}"
        );
        assert!(
            stdout.contains("[9, 8]\n"),
            "{mode}で関数内からのglobal index代入が反映されていない: {stdout}"
        );
        assert!(
            stdout.contains("[7, 8]\n"),
            "{mode}でclosureが同じbindingを更新していない: {stdout}"
        );
        assert!(
            !stderr.contains("panicked at"),
            "{mode}でhost panic: {stderr}"
        );
    }
}

#[test]
fn context_builtins_reject_invalid_control_flow_and_exit_type() {
    let tree = run_repl_process(
        "fn stop(x)\n    break\nend\nprint(map([1], stop))\n",
        false,
        &[],
    );
    let (_, tree_stderr) = output_text(&tree);
    assert!(tree.status.success(), "tree REPLが異常終了: {tree_stderr}");
    assert!(
        tree_stderr.contains("break はループの中でのみ使用できます"),
        "callback内breakが暗黙nullになった: {tree_stderr}"
    );

    let vm = run_repl_process("exit(\"bad\")\nprint(\"alive\")\n", true, &[]);
    let (vm_stdout, vm_stderr) = output_text(&vm);
    assert!(vm.status.success(), "VM REPLが異常終了: {vm_stderr}");
    assert!(vm_stderr.contains("exit() の引数は整数である必要があります"));
    assert!(
        vm_stdout.contains("alive\n"),
        "不正なexitがprocessを終了した: {vm_stdout}"
    );
}

#[test]
fn vm_direct_recursive_callback_keeps_self_binding() {
    let output = run_repl_process(
        "fn down(n)\n    if n == 0\n        return 0\n    end\n    return down(n - 1)\nend\n\
         print(map([2], down))\n",
        true,
        &[],
    );
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success(), "VM REPLが異常終了: {stderr}");
    assert!(
        stderr.is_empty(),
        "direct callbackの自己再帰に失敗: {stderr}"
    );
    assert!(stdout.contains("[0]\n"), "callback結果が不正: {stdout}");
}

#[test]
fn vm_try_unwind_preserves_existing_local_cell_promotion() {
    let output = run_repl_process(
        "fn demo()\n    let x = 1\n    let holder = null\n    try\n        holder = fn() x end\n        let bad = 1 / 0\n    catch e\n        print(e[\"type\"])\n    end\n    x = 2\n    return holder()\nend\n\
         print(demo())\n",
        true,
        &[],
    );
    let (stdout, stderr) = output_text(&output);

    assert!(output.status.success(), "VM REPLが異常終了: {stderr}");
    assert!(stderr.is_empty(), "catch済みエラー以外が発生: {stderr}");
    assert!(
        stdout.contains("zero_division\n"),
        "try内エラーがcatchされていない: {stdout}"
    );
    assert!(
        stdout.contains("2\n"),
        "try内で初めてcell化した既存localとescape closureが分離: {stdout}"
    );
}

// =============================================================
// リグレッションテスト: CLI・標準I/Oのhost panic経路（AUD-035）
// =============================================================

/// 出力先が閉じた状態（`tsumugi script.tsg | head -1` 相当）でも
/// `println!`のpanicでホストを落とさず、構造化エラーとして報告する。
#[test]
fn print_reports_closed_output_without_host_panic() {
    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree" };
        let dir = TestDir::new(&format!(
            "closed-output-{}",
            if use_vm { "vm" } else { "tree" }
        ));
        let script = std::path::Path::new(dir.as_str()).join("many_prints.tsg");
        std::fs::write(&script, "for i in range(0, 100000)\n    print(i)\nend\n")
            .unwrap_or_else(|error| panic!("{mode}: スクリプトを書けません: {error}"));

        let mut command = Command::new(tsumugi_bin());
        if use_vm {
            command.arg("--vm");
        }
        command
            .arg(&script)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command
            .spawn()
            .unwrap_or_else(|error| panic!("{mode}: 起動に失敗: {error}"));

        // 1行だけ読み、残りを読まずに読み取り側を閉じる（パイプ切断）
        {
            use std::io::BufRead as _;
            let stdout = child
                .stdout
                .take()
                .unwrap_or_else(|| panic!("{mode}: stdoutを取得できません"));
            let mut reader = std::io::BufReader::new(stdout);
            let mut first_line = String::new();
            reader.read_line(&mut first_line).ok();
        }

        let output = wait_with_timeout(child, DEFAULT_TIMEOUT, &format!("{mode} closed output"));
        let stderr = normalize(&String::from_utf8_lossy(&output.stderr));

        assert!(
            !stderr.contains("panicked at"),
            "{mode}: 出力先の切断でhost panicが発生: {stderr}"
        );
        assert!(
            !output.status.success(),
            "{mode}: 出力失敗が成功扱いになっています: {stderr}"
        );
        assert!(
            stderr.contains("標準出力への書き込みに失敗しました"),
            "{mode}: 構造化された出力エラーが報告されていません: {stderr}"
        );
    }
}

/// 非UTF-8のargvで`std::env::args()`がpanicせず、診断して終了する。
#[cfg(unix)]
#[test]
fn rejects_non_utf8_argument_without_host_panic() {
    use std::os::unix::ffi::OsStrExt as _;

    let invalid = std::ffi::OsStr::from_bytes(&[0xff, 0xfe]);
    let child = Command::new(tsumugi_bin())
        .arg(invalid)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("起動に失敗: {error}"));

    let output = wait_with_timeout(child, DEFAULT_TIMEOUT, "non-utf8 argv");
    let stderr = normalize(&String::from_utf8_lossy(&output.stderr));

    assert!(
        !stderr.contains("panicked at"),
        "非UTF-8のargvでhost panicが発生: {stderr}"
    );
    assert!(
        !output.status.success(),
        "非UTF-8のargvが成功扱いになっています: {stderr}"
    );
    assert!(
        stderr.contains("UTF-8"),
        "UTF-8でないことを説明する診断がありません: {stderr}"
    );
}

// =============================================================
// リグレッションテスト: import の評価時点（AUD-030）
// =============================================================

/// 実行前に解決するため、失敗した import の手前で副作用が起きない。
/// また、実行が完了しなかったモジュールは未解決へ戻り、再度 import できる。
#[test]
fn repl_resolves_imports_before_execution_in_both_engines() {
    let dir = TestDir::new("import-timing");
    let module = std::path::Path::new(dir.as_str()).join("boom.tsg");
    std::fs::write(&module, "print(\"MOD-TOP\")\nlet boom = 1 / 0\n")
        .unwrap_or_else(|error| panic!("モジュールを書けません: {error}"));
    let module_path = module
        .to_str()
        .unwrap_or_else(|| panic!("パスがUTF-8ではありません"))
        .replace('\\', "/");

    let source = format!(
        "print(\"BEFORE\")\nimport \"{module_path}\"\nimport \"{module_path}\"\nprint(\"AFTER\")\n"
    );

    let mut outputs = Vec::new();
    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree" };
        let output = run_repl_process(&source, use_vm, &[]);
        let (stdout, stderr) = output_text(&output);
        let visible = repl_visible_lines(&stdout, use_vm);

        assert!(
            !stderr.contains("panicked at"),
            "{mode}: host panicが発生: {stderr}"
        );
        // 失敗したimportは未解決へ戻るため、2回目のimportで再実行される
        assert_eq!(
            visible.iter().filter(|line| **line == "MOD-TOP").count(),
            2,
            "{mode}: 実行が完了しなかったmoduleを再importできていません: {stdout}"
        );
        assert_eq!(
            stderr.matches("ゼロ除算").count(),
            2,
            "{mode}: 2回目のimportでmoduleが再実行されていません: {stderr}"
        );
        outputs.push(visible.join("\n"));
    }

    assert_eq!(
        outputs[0], outputs[1],
        "treeとVMでimportの観測結果が異なります"
    );
}
