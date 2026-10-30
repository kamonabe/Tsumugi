//! fix-value-nesting-depth-limit（issue #290）: List / Dict の入れ子の深さを 256 に制限し、
//! 超過を catch できる `overflow` にする。受入基準 VND-AT-01..05
//! （docs/execution-control.md の同名スライス小節、規範は docs/semantic-decisions.md §21）。

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tsumugi::budget::BudgetUsage;
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
        path.push(format!("tsumugi-vnd-{}-{}", std::process::id(), n));
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
    code: Option<i32>,
}

/// CLI（debug ビルド、8 MiB の実行スレッド）で script を実行する。`limit` を超えたら kill して panic する。
fn run_cli(source: &str, use_vm: bool, limit: Duration) -> CliResult {
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
        code: status.code(),
    }
}

const MESSAGE: &str = "値のネストが深すぎます (上限: 256)";

/// 深度 `depth` の List（`kind = "list"`）または Dict（`"dict"`）を `name` に作る script 断片。
/// 空の List / Dict が深度 1、1 段包むごとに 1 増える。
fn nest(name: &str, kind: &str, depth: usize) -> String {
    let (empty, wrap) = match kind {
        "list" => ("[]", format!("{name} = [{name}]")),
        _ => ("{}", format!("{name} = {{\"k\": {name}}}")),
    };
    format!(
        "let {name} = {empty}\nlet {name}_i = 1\nwhile {name}_i < {depth}\n  {wrap}\n  {name}_i = {name}_i + 1\nend\n"
    )
}

/// VND-AT-01: 深度 256 の List / Dict を各経路で作れ、257 になる同じ操作が `overflow` で拒否される。
/// 拒否は catch でき、`push` / index 代入の対象は変わらない。tree-walk 版と VM 版で
/// type / message / line が一致する。
#[test]
fn vnd_at_01_boundary_and_catch_in_both_engines() {
    for kind in ["list", "dict"] {
        // e255: 深度 255、e256: 深度 256（作れる）。入れ物にすると 256 / 257。
        let mut lines: Vec<String> = Vec::new();
        let mut expected: Vec<String> = Vec::new();
        let prelude = format!(
            "{}let e256 = {}\nlet sink = null\nlet t = [1]\nlet dt = {{\"a\": 1}}\n",
            nest("e255", kind, 255),
            if kind == "list" {
                "[e255]"
            } else {
                "{\"k\": e255}"
            }
        );
        lines.extend(prelude.lines().map(String::from));

        // (名前, 作れる操作, 拒否される操作)
        let cases: [(&str, &str, &str); 7] = [
            ("list-literal", "sink = [e255]", "sink = [e256]"),
            (
                "dict-literal",
                "sink = {\"k\": e255}",
                "sink = {\"k\": e256}",
            ),
            ("push", "push(t, e255)", "push(t, e256)"),
            ("list-index-assign", "t[0] = e255", "t[0] = e256"),
            ("dict-index-assign", "dt[\"a\"] = e255", "dt[\"a\"] = e256"),
            ("dict-new-key", "dt[\"b\"] = e255", "dt[\"c\"] = e256"),
            (
                "map",
                "sink = map([1], fn(x) e255 end)",
                "sink = map([1], fn(x) e256 end)",
            ),
        ];
        for (name, ok_stmt, bad_stmt) in cases {
            // 作れる側
            lines.push(ok_stmt.to_string());
            lines.push(format!("print(\"{name}:ok\")"));
            expected.push(format!("{name}:ok"));
            // 拒否される側（try の中の bad 文の行番号が e.line になる）
            lines.push("try".to_string());
            lines.push(bad_stmt.to_string());
            let bad_line = lines.len();
            lines.push("  print(\"NOT REJECTED\")".to_string());
            lines.push("catch e".to_string());
            lines.push("  print(e[\"type\"])".to_string());
            lines.push("  print(e[\"message\"])".to_string());
            lines.push("  print(e[\"line\"])".to_string());
            lines.push("end".to_string());
            expected.push("overflow".to_string());
            expected.push(MESSAGE.to_string());
            expected.push(bad_line.to_string());
        }
        // 拒否された push / index 代入は対象を変えていない。
        lines.push("print(len(t))".to_string());
        lines.push("print(t[0] == e255)".to_string());
        lines.push("print(len(dt))".to_string());
        lines.push("print(dt[\"a\"] == e255)".to_string());
        // t は push の 1 回で 2 要素、dt は新 key \"b\" の 1 回で 2 要素。拒否された操作は何も足していない。
        expected.push("2".to_string());
        expected.push("true".to_string());
        expected.push("2".to_string());
        expected.push("true".to_string());

        let source = lines.join("\n") + "\n";
        let want = expected.join("\n") + "\n";
        let mut outputs = Vec::new();
        for use_vm in [false, true] {
            let r = run_cli(&source, use_vm, Duration::from_secs(60));
            assert_eq!(
                r.code,
                Some(0),
                "[kind={kind} vm={use_vm}] 失敗: {}",
                r.stderr
            );
            assert_eq!(r.stdout, want, "[kind={kind} vm={use_vm}]");
            outputs.push(r.stdout);
        }
        assert_eq!(outputs[0], outputs[1], "tree と VM の出力が違う");
    }
}

/// VND-AT-02: issue #290 の再現 1（List / Dict を 20 万回入れ子にする）が abort せず、
/// `overflow` の診断を出して exit 1 で終わる。
#[test]
fn vnd_at_02_repro_does_not_abort() {
    for (label, wrap, init) in [("list", "x = [x]", "[]"), ("dict", "x = {\"a\": x}", "{}")] {
        let source = format!(
            "let x = {init}\nlet i = 0\nwhile i < 200000\n  {wrap}\n  i = i + 1\nend\nprint(i)\n"
        );
        for use_vm in [false, true] {
            let r = run_cli(&source, use_vm, Duration::from_secs(60));
            assert_eq!(
                r.code,
                Some(1),
                "[{label} vm={use_vm}] exit code（abort なら 134 / シグナル）。stderr: {}",
                r.stderr
            );
            assert!(
                r.stderr.contains(MESSAGE),
                "[{label} vm={use_vm}] stderr: {}",
                r.stderr
            );
            assert_eq!(
                r.stdout, "",
                "[{label} vm={use_vm}] 途中で print していない"
            );
        }
    }
}

/// VND-AT-03: 深い要素を index 代入・`pop` で取り除いた List / Dict は再び入れ子にできる（深度は正確）。
/// 最大深度の要素が複数あるあいだは深いまま。
#[test]
fn vnd_at_03_depth_is_exact_after_removal() {
    let source = format!(
        "{}{}",
        nest("e255", "list", 255),
        r#"
let a = [e255]
a[0] = 1
let a2 = [a]
print("index-assign:" + to_str(len(a2)))

let b = [e255, e255]
b[0] = 1
try
  let r = [b]
  print("two-deep:wrongly ok")
catch e
  print("two-deep:still deep")
end
b[1] = 1
let b2 = [b]
print("two-deep:removed")

let c = [1, e255]
let popped = pop(c)
let c2 = [c]
print("pop:" + to_str(len(c2)))

let d = {"x": e255, "y": 2}
d["x"] = 1
let d2 = [d]
print("dict:" + to_str(len(d2)))

let f = [e255, 1]
f[0] = [1]
let g = [f]
print("replaced:" + to_str(len(g)))
"#
    );
    let want = "index-assign:1\ntwo-deep:still deep\ntwo-deep:removed\npop:1\ndict:1\nreplaced:1\n";
    for use_vm in [false, true] {
        let r = run_cli(&source, use_vm, Duration::from_secs(60));
        assert_eq!(r.code, Some(0), "[vm={use_vm}] 失敗: {}", r.stderr);
        assert_eq!(r.stdout, want, "[vm={use_vm}]");
    }
}

/// VND-AT-03（時間）: 深い要素を並べた List（要素数 N）の全要素をスカラーへ index 代入するループが、
/// 深度の読み直しを要しても要素数の 2 乗にならず壁時計の上限以内に終わる。
#[test]
fn vnd_at_03_reassign_loop_is_not_quadratic() {
    let n = 20_000;
    let source = format!(
        "{}let xs = []\nlet k = 0\nwhile k < {n}\n  push(xs, e255)\n  k = k + 1\nend\nk = 0\nwhile k < {n}\n  xs[k] = 0\n  k = k + 1\nend\nlet ok = [xs]\nprint(len(ok))\n",
        nest("e255", "list", 255)
    );
    for use_vm in [false, true] {
        let start = Instant::now();
        let r = run_cli(&source, use_vm, Duration::from_secs(10));
        assert_eq!(r.code, Some(0), "[vm={use_vm}] 失敗: {}", r.stderr);
        assert_eq!(r.stdout, "1\n", "[vm={use_vm}]");
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}

/// VND-AT-04: 深度 256 の値を call depth 128 の再帰の内側で表示・比較・`sort`・破棄しても、
/// CLI の 8 MiB の実行スレッド（debug ビルド）で abort しない。
#[test]
fn vnd_at_04_limit_depth_fits_in_stack_under_max_call_depth() {
    // a1 / b1 は深度 255。a = [a1] / b = [b1] が深度 256 の値、[a1, b1] も深度 256。
    let source = format!(
        "{}{}{}",
        nest("a1", "list", 255),
        nest("b1", "list", 255),
        r#"
let a = [a1]
let b = [b1]
fn rec(n)
  if n == 0
    print(len(to_str(a)))
    print(a == b)
    print(len(sort([a1, b1])))
    print(contains([a1], b1))
    let tmp = []
    let k = 1
    while k < 256
      tmp = [tmp]
      k = k + 1
    end
    tmp = null
    return 0
  end
  return rec(n - 1)
end
print(rec(127))
"#
    );
    let mut outputs = Vec::new();
    for use_vm in [false, true] {
        let r = run_cli(&source, use_vm, Duration::from_secs(60));
        assert_eq!(r.code, Some(0), "[vm={use_vm}] 失敗: {}", r.stderr);
        // 深度 256 の List の表示は "[" × 256 + "]" × 256 = 512 文字。
        assert_eq!(r.stdout, "512\ntrue\n2\ntrue\n0\n", "[vm={use_vm}]");
        outputs.push(r.stdout);
    }
    assert_eq!(outputs[0], outputs[1]);
}

/// 深度 256 以下の値だけを使う代表 script。usage を変更前の値で固定する。
const USAGE_SCRIPT: &str = r#"
let xs = []
let i = 0
while i < 20
  push(xs, [i, {"k": [i, [i]]}])
  i = i + 1
end
xs[3] = {"a": [1, [2, [3]]]}
let m = map(xs, fn(x) [x] end)
let p = pop(xs)
let s = sort([3, 1, 2])
let deep = []
let j = 1
while j < 200
  deep = [deep]
  j = j + 1
end
let d2 = [deep, deep]
"#;

fn tree_usage(source: &str) -> BudgetUsage {
    let tokens = Lexer::new(source).tokenize();
    let program = Parser::new(tokens).parse().expect("parse");
    let mut evaluator = Evaluator::new();
    evaluator.run(&program, source.len() as u64).expect("run");
    evaluator.budget_usage()
}

fn vm_usage(source: &str) -> BudgetUsage {
    let tokens = Lexer::new(source).tokenize();
    let program = Parser::new(tokens).parse().expect("parse");
    let chunk = Compiler::new().compile(&program).expect("compile");
    let mut vm = Vm::new(chunk);
    vm.run().expect("run");
    vm.budget_usage()
}

/// VND-AT-05: 上限内の値だけを使う script の fuel・heap は、変更前（main）と同じ値のまま
/// （深度の計算は課金しない）。
#[test]
fn vnd_at_05_usage_unchanged_within_limit() {
    let t = tree_usage(USAGE_SCRIPT);
    let v = vm_usage(USAGE_SCRIPT);
    // 変更前（fix-contains-budgeted-equality 直後の main、650eeab）で測定した値。
    assert_eq!(
        (t.committed.fuel, t.live_heap_bytes, t.peak_heap_bytes),
        TREE_EXPECTED,
        "tree の usage（fuel, live_heap, peak_heap）"
    );
    assert_eq!(
        (v.committed.fuel, v.live_heap_bytes, v.peak_heap_bytes),
        VM_EXPECTED,
        "VM の usage（fuel, live_heap, peak_heap）"
    );
}

const TREE_EXPECTED: (u64, u64, u64) = (242, 25204, 25204);
const VM_EXPECTED: (u64, u64, u64) = (242, 24260, 24260);
