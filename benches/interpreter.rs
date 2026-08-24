//! ベンチマーク: ツリーウォーク版とVM版の代表的なワークロードを計測する

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use tsumugi::compiler::Compiler;
use tsumugi::eval::Evaluator;
use tsumugi::lexer::Lexer;
use tsumugi::parser::Parser;
use tsumugi::vm::Vm;

/// ソースコードをパースしてASTを返すヘルパー
fn parse(source: &str) -> tsumugi::ast::Program {
    let tokens = Lexer::new(source).tokenize();
    Parser::new(tokens).parse().unwrap()
}

/// ツリーウォーク版で実行
fn run_tree_walk(source: &str) {
    let program = parse(source);
    let mut eval = Evaluator::new();
    eval.run(&program).unwrap();
}

/// VM版で実行
fn run_vm(source: &str) {
    let program = parse(source);
    let compiler = Compiler::new();
    let chunk = compiler.compile(&program).unwrap();
    let mut vm = Vm::new(chunk);
    vm.run().unwrap();
}

// ---------------------------------------------------------------------------
// ベンチマーク用スクリプト
// ---------------------------------------------------------------------------

/// フィボナッチ (再帰) — 関数呼び出しのオーバーヘッド計測
const FIB_SCRIPT: &str = r#"
fn fib(n)
  if n < 2
    return n
  end
  return fib(n - 1) + fib(n - 2)
end
let result = fib(20)
"#;

/// 辞書操作 — コレクション生成・アクセスの計測
const DICT_SCRIPT: &str = r#"
let d = {}
for i in range(0, 500)
  d[to_str(i)] = i * 2
end
let total = 0
for k in d
  total = total + d[k]
end
"#;

/// f-string 連結 — 文字列補間の計測
const FSTR_SCRIPT: &str = r#"
let result = ""
for i in range(0, 300)
  result = f"{result}{i},"
end
"#;

/// ループ + 算術 — 基本的なループ性能の計測
const LOOP_SCRIPT: &str = r#"
let sum = 0
for i in range(0, 5000)
  sum = sum + i
end
"#;

/// 高階関数 (map/filter) — クロージャ呼び出しの計測
const HIGHER_ORDER_SCRIPT: &str = r#"
let nums = range(0, 200)
let doubled = map(nums, fn(x) return x * 2 end)
let evens = filter(doubled, fn(x) return x % 4 == 0 end)
"#;

// ---------------------------------------------------------------------------
// ベンチマーク定義
// ---------------------------------------------------------------------------

fn bench_fib(c: &mut Criterion) {
    let mut group = c.benchmark_group("fib_20");
    group.bench_function("tree_walk", |b| {
        b.iter(|| run_tree_walk(black_box(FIB_SCRIPT)))
    });
    group.bench_function("vm", |b| b.iter(|| run_vm(black_box(FIB_SCRIPT))));
    group.finish();
}

fn bench_dict(c: &mut Criterion) {
    let mut group = c.benchmark_group("dict_500");
    group.bench_function("tree_walk", |b| {
        b.iter(|| run_tree_walk(black_box(DICT_SCRIPT)))
    });
    group.bench_function("vm", |b| b.iter(|| run_vm(black_box(DICT_SCRIPT))));
    group.finish();
}

fn bench_fstr(c: &mut Criterion) {
    let mut group = c.benchmark_group("fstr_300");
    group.bench_function("tree_walk", |b| {
        b.iter(|| run_tree_walk(black_box(FSTR_SCRIPT)))
    });
    group.bench_function("vm", |b| b.iter(|| run_vm(black_box(FSTR_SCRIPT))));
    group.finish();
}

fn bench_loop(c: &mut Criterion) {
    let mut group = c.benchmark_group("loop_5000");
    group.bench_function("tree_walk", |b| {
        b.iter(|| run_tree_walk(black_box(LOOP_SCRIPT)))
    });
    group.bench_function("vm", |b| b.iter(|| run_vm(black_box(LOOP_SCRIPT))));
    group.finish();
}

fn bench_higher_order(c: &mut Criterion) {
    let mut group = c.benchmark_group("higher_order_200");
    group.bench_function("tree_walk", |b| {
        b.iter(|| run_tree_walk(black_box(HIGHER_ORDER_SCRIPT)))
    });
    group.bench_function("vm", |b| b.iter(|| run_vm(black_box(HIGHER_ORDER_SCRIPT))));
    group.finish();
}

criterion_group!(
    benches,
    bench_fib,
    bench_dict,
    bench_fstr,
    bench_loop,
    bench_higher_order
);
criterion_main!(benches);
