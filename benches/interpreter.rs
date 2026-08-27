//! ベンチマーク: ツリーウォーク版とVM版を、フェーズごとに分けて計測する
//!
//! 1回のend-to-end実行は parse → (compile) → execute の合計であり、
//! フェーズをまとめて測るとどこが効いているのか分からない。そのため
//! 次の4グループに分ける。
//!
//! - `parse/<workload>`: 字句解析 + 構文解析（両engine共通）
//! - `compile/<workload>`: AST → Chunk（VMのみ）
//! - `execute/<workload>`: 実行のみ（parse・compile済みを再利用）
//! - `end_to_end/<workload>`: 実際の1回実行に相当する合計
//!
//! `execute` は `iter_batched` を使い、`Evaluator::new()` や `Chunk` の複製を
//! セットアップとして測定対象から外す。engine間の比較は同じフェーズ同士で行う。

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};

use tsumugi::ast::Program;
use tsumugi::chunk::Chunk;
use tsumugi::compiler::Compiler;
use tsumugi::eval::Evaluator;
use tsumugi::lexer::Lexer;
use tsumugi::parser::Parser;
use tsumugi::vm::Vm;

// ---------------------------------------------------------------------------
// ヘルパー
// ---------------------------------------------------------------------------

fn parse(source: &str) -> Program {
    let tokens = Lexer::new(source).tokenize();
    Parser::new(tokens).parse().expect("ベンチのパースに失敗")
}

fn compile(program: &Program) -> Chunk {
    Compiler::new()
        .compile(program)
        .expect("ベンチのコンパイルに失敗")
}

fn execute_tree_walk(program: &Program) {
    let mut evaluator = Evaluator::new();
    evaluator.run(program).expect("ベンチのtree実行に失敗");
}

fn execute_vm(chunk: Chunk) {
    let mut vm = Vm::new(chunk);
    vm.run().expect("ベンチのVM実行に失敗");
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

/// for ループ + 算術 — コレクション反復の計測
const LOOP_SCRIPT: &str = r#"
let sum = 0
for i in range(0, 5000)
  sum = sum + i
end
"#;

/// while ループ + 算術 — コレクションを介さないループの計測
/// （`loop_5000` との差がイテレーション処理のコストになる）
const WHILE_SCRIPT: &str = r#"
let sum = 0
let i = 0
while i < 5000
  sum = sum + i
  i = i + 1
end
"#;

/// 高階関数 (map/filter) — クロージャ呼び出しの計測
const HIGHER_ORDER_SCRIPT: &str = r#"
let nums = range(0, 200)
let doubled = map(nums, fn(x) return x * 2 end)
let evens = filter(doubled, fn(x) return x % 4 == 0 end)
"#;

/// 計測対象のワークロード一覧
const WORKLOADS: &[(&str, &str)] = &[
    ("fib_20", FIB_SCRIPT),
    ("dict_500", DICT_SCRIPT),
    ("fstr_300", FSTR_SCRIPT),
    ("loop_5000", LOOP_SCRIPT),
    ("while_5000", WHILE_SCRIPT),
    ("higher_order_200", HIGHER_ORDER_SCRIPT),
];

// ---------------------------------------------------------------------------
// フェーズ別ベンチマーク
// ---------------------------------------------------------------------------

/// parse: 字句解析 + 構文解析（両engine共通のコスト）
fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");
    for (name, source) in WORKLOADS {
        group.bench_function(*name, |b| b.iter(|| parse(black_box(source))));
    }
    group.finish();
}

/// compile: AST → Chunk（VMのみが払うコスト）
fn bench_compile(c: &mut Criterion) {
    let mut group = c.benchmark_group("compile");
    for (name, source) in WORKLOADS {
        let program = parse(source);
        group.bench_function(*name, |b| b.iter(|| compile(black_box(&program))));
    }
    group.finish();
}

/// execute: 実行のみ。parse・compile と初期化コストは測定対象から外す
fn bench_execute(c: &mut Criterion) {
    for (name, source) in WORKLOADS {
        let mut group = c.benchmark_group(format!("execute/{}", name));
        let program = parse(source);
        let chunk = compile(&program);

        group.bench_function("tree_walk", |b| {
            b.iter(|| execute_tree_walk(black_box(&program)))
        });
        group.bench_function("vm", |b| {
            b.iter_batched(
                || chunk.clone(),
                |chunk| execute_vm(black_box(chunk)),
                BatchSize::SmallInput,
            )
        });
        group.finish();
    }
}

/// end_to_end: 実際の1回実行に相当する合計（parse + compile + execute）
fn bench_end_to_end(c: &mut Criterion) {
    for (name, source) in WORKLOADS {
        let mut group = c.benchmark_group(format!("end_to_end/{}", name));

        group.bench_function("tree_walk", |b| {
            b.iter(|| {
                let program = parse(black_box(source));
                execute_tree_walk(&program);
            })
        });
        group.bench_function("vm", |b| {
            b.iter(|| {
                let program = parse(black_box(source));
                execute_vm(compile(&program));
            })
        });
        group.finish();
    }
}

criterion_group!(
    benches,
    bench_parse,
    bench_compile,
    bench_execute,
    bench_end_to_end
);
criterion_main!(benches);
