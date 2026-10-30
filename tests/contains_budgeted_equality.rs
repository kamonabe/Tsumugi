//! fix-contains-budgeted-equality（issue #290）: `contains` の等価判定を `==` と同じ予算付きの
//! `values_equal` にする。受入基準 CBE-AT-01..04（docs/execution-control.md の同名スライス小節）。

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tsumugi::budget::BudgetConfig;
use tsumugi::compiler::Compiler;
use tsumugi::eval::Evaluator;
use tsumugi::lexer::Lexer;
use tsumugi::parser::Parser;
use tsumugi::vm::Vm;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("tsumugi-cbe-{}-{}", std::process::id(), n));
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

struct CliResult {
    stdout: String,
    stderr: String,
    success: bool,
    elapsed: Duration,
}

/// CLI で script を実行する。`limit` を超えて終わらなければ kill して panic する。
fn run_cli(source: &str, use_vm: bool, envs: &[(&str, &str)], limit: Duration) -> CliResult {
    let dir = TestDir::new();
    let script = dir.path.join("s.tsg");
    std::fs::write(&script, source).expect("write script");
    let mut command = Command::new(env!("CARGO_BIN_EXE_tsumugi"));
    if use_vm {
        command.arg("--vm");
    }
    command
        .arg(&script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let start = Instant::now();
    let mut child = command.spawn().expect("spawn");
    let mut out = child.stdout.take().expect("stdout");
    let mut err = child.stderr.take().expect("stderr");
    let out_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{limit:?} 以内に終わらなかった（vm={use_vm}）");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    CliResult {
        stdout: out_thread.join().expect("join").replace("\r\n", "\n"),
        stderr: err_thread.join().expect("join").replace("\r\n", "\n"),
        success: status.success(),
        elapsed: start.elapsed(),
    }
}

/// CBE-AT-01: `contains([a], b)` が `a == b` と同じ真偽を返す（Int/Float 跨ぎ、NaN、入れ子、
/// 関数値の同一性、エラー値、型違い）。tree-walk 版と VM 版で同じ。
#[test]
fn cbe_at_01_contains_matches_equality_in_both_engines() {
    let prelude = concat!(
        "let nan = 0.0 / 0.0\n",
        "let f = fn() 1 end\n",
        "let g = fn() 1 end\n",
        "let e1 = null\n",
        "let e2 = null\n",
        "try\n  let z = 1 / 0\ncatch err\n  e1 = err\nend\n",
        "try\n  let z = 1 / 0\ncatch err\n  e2 = err\nend\n",
    );
    let pairs: &[(&str, &str)] = &[
        ("1", "1"),
        ("1", "1.0"),
        ("1.0", "1"),
        ("1", "2"),
        ("nan", "nan"),
        ("[nan]", "[nan]"),
        ("[1]", "[1.0]"),
        ("[1, [2, 3]]", "[1.0, [2, 3.0]]"),
        ("[1, 2]", "[1, 3]"),
        ("[1, 2]", "[1]"),
        ("{\"a\": [1]}", "{\"a\": [1.0]}"),
        ("{\"a\": 1}", "{\"b\": 1}"),
        ("{\"a\": 1}", "{\"a\": 2}"),
        ("f", "f"),
        ("f", "g"),
        ("[f]", "[f]"),
        ("e1", "e1"),
        ("e1", "e2"),
        ("null", "null"),
        ("null", "false"),
        ("1", "true"),
        ("\"a\"", "\"a\""),
        ("\"a\"", "1"),
        ("[1]", "1"),
        ("[]", "[]"),
        ("{}", "[]"),
    ];
    let mut source = String::from(prelude);
    for (a, b) in pairs {
        source.push_str(&format!("print(contains([{a}], {b}))\nprint({a} == {b})\n"));
    }
    // 見つかる位置が先頭・途中・末尾・無しのどれでも `==` と一致する。
    source.push_str("print(contains([0, 1.0, 2], 1))\nprint(contains([0, 5, 1], 1.0))\n");
    source.push_str("print(contains([], 1))\n");

    let mut outputs = Vec::new();
    for use_vm in [false, true] {
        let r = run_cli(&source, use_vm, &[], Duration::from_secs(30));
        assert!(r.success, "[vm={use_vm}] 失敗: {}", r.stderr);
        let lines: Vec<&str> = r.stdout.lines().collect();
        for (i, (a, b)) in pairs.iter().enumerate() {
            assert_eq!(
                lines[2 * i],
                lines[2 * i + 1],
                "[vm={use_vm}] contains([{a}], {b}) が {a} == {b} と違う"
            );
        }
        let tail = &lines[2 * pairs.len()..];
        assert_eq!(tail, ["true", "true", "false"], "[vm={use_vm}]");
        outputs.push(r.stdout);
    }
    assert_eq!(outputs[0], outputs[1], "tree と VM の出力が違う");
}

/// CBE-AT-02: issue #290 の再現 2（共有 DAG を 60 回入れ子にした値どうし）の `contains` が
/// 既定の budget で有限時間に `true` を返す。
#[test]
fn cbe_at_02_shared_dag_contains_finishes_in_bounded_time() {
    let source = concat!(
        "let a = [1]\n",
        "let b = [1]\n",
        "let i = 0\n",
        "while i < 60\n",
        "  a = [a, a]\n",
        "  b = [b, b]\n",
        "  i = i + 1\n",
        "end\n",
        "print(contains([a], b))\n",
    );
    for use_vm in [false, true] {
        let r = run_cli(source, use_vm, &[], Duration::from_secs(10));
        assert!(r.success, "[vm={use_vm}] 失敗: {}", r.stderr);
        assert_eq!(r.stdout.trim_end(), "true", "[vm={use_vm}]");
        assert!(r.elapsed < Duration::from_secs(10));
    }
}

/// tree-walk を in-process で `fuel` 上限付きで実行し、成功したかを返す。
fn tree_succeeds(source: &str, fuel: u64) -> bool {
    let tokens = Lexer::new(source).tokenize();
    let program = Parser::new(tokens).parse().expect("parse");
    let mut evaluator = Evaluator::with_budget(BudgetConfig::for_legacy(fuel, 1_000_000));
    evaluator.run(&program, source.len() as u64).is_ok()
}

/// VM を in-process で `fuel` 上限付きで実行し、成功したかを返す。
fn vm_succeeds(source: &str, fuel: u64) -> bool {
    let tokens = Lexer::new(source).tokenize();
    let program = Parser::new(tokens).parse().expect("parse");
    let chunk = Compiler::new().compile(&program).expect("compile");
    let mut vm = Vm::new(chunk);
    vm.set_max_steps(fuel);
    vm.run().is_ok()
}

/// 成功する最小の fuel 上限を二分探索で求める（成功は fuel について単調）。
fn min_fuel(source: &str, use_vm: bool) -> u64 {
    let ok = |fuel: u64| {
        if use_vm {
            vm_succeeds(source, fuel)
        } else {
            tree_succeeds(source, fuel)
        }
    };
    let (mut lo, mut hi) = (0u64, 1_000_000u64);
    assert!(ok(hi), "上限 {hi} でも成功しない");
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if ok(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// 共有しない入れ子（2 分木）を作る関数と、同じ形の 2 つの値。
const TREE_PRELUDE: &str = concat!(
    "fn mk(n)\n",
    "  if n == 0\n",
    "    return [1]\n",
    "  end\n",
    "  return [mk(n - 1), mk(n - 1), {\"k\": [n]}]\n",
    "end\n",
    "let x = mk(5)\n",
    "let y = mk(5)\n",
);

/// CBE-AT-03: 比較が fuel を使い切る `contains` は `==` と同じ catch できない
/// `BudgetExceeded(Fuel)` で止まり、`try` / `catch` で捕捉されない。
#[test]
fn cbe_at_03_fuel_exhaustion_in_contains_is_terminal() {
    // in-process 実行には print の capability が無いので、fuel の測定用は print を含めない。
    let measured =
        |stmt: &str| format!("{TREE_PRELUDE}try\n  let r = {stmt}\ncatch e\n  let c = 1\nend\n");
    // CLI 用は catch 節と後続で print する。
    let observed = |stmt: &str| {
        format!(
            "{TREE_PRELUDE}try\n  let r = {stmt}\ncatch e\n  print(\"caught\")\nend\nprint(\"after\")\n"
        )
    };
    for use_vm in [false, true] {
        // 比較の手前までは通り、比較（木の複合ノードごとに 1 fuel）で尽きる上限。
        let limit = min_fuel(&measured("1"), use_vm) + 5;
        assert!(
            min_fuel(&measured("contains([x], y)"), use_vm) > limit + 20,
            "[vm={use_vm}] contains が比較ぶんの fuel を使っていない"
        );
        let limit_text = limit.to_string();
        let envs = [("TSUMUGI_MAX_STEPS", limit_text.as_str())];
        for (label, stmt) in [("contains", "contains([x], y)"), ("==", "x == y")] {
            let r = run_cli(&observed(stmt), use_vm, &envs, Duration::from_secs(30));
            assert!(
                r.stderr.contains("ステップ上限に達しました"),
                "[vm={use_vm}] {label}: fuel 超過で止まっていない: stdout={} stderr={}",
                r.stdout,
                r.stderr
            );
            assert!(!r.success, "[vm={use_vm}] {label}: 成功してしまった");
            assert!(
                !r.stdout.contains("caught") && !r.stdout.contains("after"),
                "[vm={use_vm}] {label}: fuel 超過が catch された: {}",
                r.stdout
            );
        }
    }
}

/// CBE-AT-04: List / Dict を要素に持つ `contains([x], y)` の fuel の増分は `x == y` の増分と一致する。
/// スカラーだけの List への `contains` の fuel は変更前の値で固定する。
#[test]
fn cbe_at_04_contains_charges_like_equality() {
    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree" };
        let fuel = |body: &str| min_fuel(&format!("{TREE_PRELUDE}{body}"), use_vm);
        // 増分 = 比較を含む script の最小 fuel - 同じ形でスカラーどうしの script の最小 fuel。
        let contains_delta =
            fuel("let r = contains([x], y)\n") - fuel("let r = contains([1], 1)\n");
        let eq_delta = fuel("let r = x == y\n") - fuel("let r = 1 == 1\n");
        assert!(eq_delta > 0, "[{mode}] == が複合ノードを課金していない");
        assert_eq!(
            contains_delta, eq_delta,
            "[{mode}] contains と == の課金が違う"
        );

        // 要素の位置が違っても、見つかるまでに比べたぶんだけ課金する（先頭で一致すれば残りは比べない）。
        let first_hit =
            fuel("let r = contains([x, y, y], y)\n") - fuel("let r = contains([1, 1, 1], 1)\n");
        assert_eq!(first_hit, eq_delta, "[{mode}] 先頭一致で残りを比較した");

        // スカラーだけの List への contains の fuel は変更前と同じ（TREE_PRELUDE を除いた script）。
        let scalar = |body: &str| min_fuel(body, use_vm);
        let scalar_fuel = scalar("let r = contains([1, 2.5, \"a\", null, true], 2.5)\n");
        let expected_before_change = if use_vm {
            SCALAR_CONTAINS_FUEL.1
        } else {
            SCALAR_CONTAINS_FUEL.0
        };
        assert_eq!(
            scalar_fuel, expected_before_change,
            "[{mode}] スカラー List の contains の fuel が変わった"
        );
    }
}

/// 変更前（`Vec::contains`）の実装で測った、スカラーだけの List への `contains` の最小 fuel
/// （tree-walk, VM）。
const SCALAR_CONTAINS_FUEL: (u64, u64) = (0, 0);
