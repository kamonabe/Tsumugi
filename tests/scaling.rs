//! スケーリング回帰テスト: 入力量に対する計算量オーダーを固定する
//!
//! 実時間ではなく**確保バイト数**を計測する。実時間はCIランナーの負荷で揺れるが、
//! 確保量は決定的なので、O(n) と O(n^2) の区別を安定して検出できる。
//!
//! 対象は `for` ループのイテレーション。VMはかつてコレクションを反復ごとに
//! スタックへ複製しており、ループ全体がO(n^2)になっていた（AUD-038）。
//!
//! 注意: グローバルアロケータでプロセス全体の確保量を数えるため、
//! このテストバイナリには測定を行うテストを1つだけ置く。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use tsumugi::compiler::Compiler;
use tsumugi::eval::Evaluator;
use tsumugi::lexer::Lexer;
use tsumugi::parser::Parser;
use tsumugi::vm::Vm;

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

/// 確保バイト数を数えるだけのアロケータ（解放は数えない）
struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATED.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// クロージャ実行中に確保されたバイト数を返す
fn allocated_bytes(body: impl FnOnce()) -> usize {
    let before = ALLOCATED.load(Ordering::Relaxed);
    body();
    ALLOCATED.load(Ordering::Relaxed).saturating_sub(before)
}

fn for_loop_source(n: usize) -> String {
    format!("let sum = 0\nfor i in range(0, {n})\n    sum = sum + i\nend\n")
}

/// 実行フェーズだけの確保量を測る（parse / compile は測定対象外）
fn execute_bytes(source: &str, use_vm: bool) -> usize {
    let tokens = Lexer::new(source).tokenize();
    let program = Parser::new(tokens).parse().expect("パースに失敗");

    if use_vm {
        let chunk = Compiler::new().compile(&program).expect("コンパイルに失敗");
        allocated_bytes(|| {
            let mut vm = Vm::new(chunk);
            vm.run().expect("VM実行に失敗");
        })
    } else {
        allocated_bytes(|| {
            let mut evaluator = Evaluator::new();
            evaluator.run(&program).expect("ツリーウォーク実行に失敗");
        })
    }
}

#[test]
fn for_loop_allocation_stays_linear_in_both_engines() {
    // 入力を2倍にしたときの確保量の伸び。線形なら約2倍、二次なら約4倍になる。
    // ランタイム側の固定コストで比が下振れするため、上限は3.0とする。
    const LIMIT: f64 = 3.0;
    const SMALL: usize = 2_000;
    const LARGE: usize = 4_000;

    let small_source = for_loop_source(SMALL);
    let large_source = for_loop_source(LARGE);

    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree-walk" };
        let small = execute_bytes(&small_source, use_vm);
        let large = execute_bytes(&large_source, use_vm);
        assert!(
            small > 0,
            "{mode}: 確保量が計測できていません（アロケータが差し替わっていない可能性）"
        );

        let ratio = large as f64 / small as f64;
        assert!(
            ratio < LIMIT,
            "{mode}: forループの確保量が線形を超えて増えています。\
             n={SMALL}で{small}バイト, n={LARGE}で{large}バイト（比 {ratio:.2} >= {LIMIT}）。\
             反復ごとにコレクション全体を複製していないか確認してください"
        );
    }
}
