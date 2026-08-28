//! スケーリング回帰テスト: 入力量に対する計算量オーダーを固定する
//!
//! 実時間ではなく**確保バイト数**を計測する。実時間はCIランナーの負荷で揺れるが、
//! 確保量は決定的なので、O(n) と O(n^2) の区別を安定して検出できる。
//!
//! 検証している性質:
//! - `for` の反復コストが要素数に線形であること（AUD-038で修正した退行）
//! - 関数呼び出しのコストが関数body長に依存しないこと（AUD-040で修正した退行）
//!
//! 注意: グローバルアロケータでプロセス全体の確保量を数えるため、測定中は
//! `MEASURE_LOCK` を保持して他の測定と直列化する。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 測定を直列化するロック（並列テストによる確保量の相互汚染を防ぐ）
static MEASURE_LOCK: Mutex<()> = Mutex::new(());

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

/// 到達しない文でbodyだけを膨らませた関数を、指定回数呼び出すスクリプト
///
/// `return` 以降は実行されないため、1回の呼び出しで行う仕事量はbody長に依存しない。
/// それでも確保量がbody長に比例するなら、呼び出しごとにbodyを複製している。
fn call_source(body_statements: usize, calls: usize) -> String {
    let mut source = String::from("fn target(n)\n    return n\n");
    for i in 0..body_statements {
        source.push_str(&format!("    let dead{i} = {i} + 1\n"));
    }
    source.push_str("end\nlet total = 0\n");
    source.push_str(&format!("for i in range(0, {calls})\n"));
    source.push_str("    total = total + target(i)\nend\n");
    source
}

#[test]
fn call_allocation_is_independent_of_body_size_in_both_engines() {
    // bodyを50倍にしても、呼び出し回数が同じなら確保量はほぼ変わらないはず。
    // 定義時に一度だけbodyを複製する分だけ増えるため、上限は2.0とする。
    const LIMIT: f64 = 2.0;
    const CALLS: usize = 300;
    const SMALL_BODY: usize = 2;
    const LARGE_BODY: usize = 100;

    // 他の測定が失敗してもロックを使い続けられるようにpoisonは無視する
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let small_source = call_source(SMALL_BODY, CALLS);
    let large_source = call_source(LARGE_BODY, CALLS);

    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree-walk" };
        let small = execute_bytes(&small_source, use_vm);
        let large = execute_bytes(&large_source, use_vm);
        assert!(small > 0, "{mode}: 確保量が計測できていません");

        let ratio = large as f64 / small as f64;
        assert!(
            ratio < LIMIT,
            "{mode}: 呼び出しの確保量が関数body長に比例しています。\
             body {SMALL_BODY}文で{small}バイト, body {LARGE_BODY}文で{large}バイト\
             （比 {ratio:.2} >= {LIMIT}）。\
             呼び出しごとに関数値のbodyを複製していないか確認してください"
        );
    }
}

#[test]
fn for_loop_allocation_stays_linear_in_both_engines() {
    // 入力を2倍にしたときの確保量の伸び。線形なら約2倍、二次なら約4倍になる。
    // ランタイム側の固定コストで比が下振れするため、上限は3.0とする。
    const LIMIT: f64 = 3.0;
    const SMALL: usize = 2_000;
    const LARGE: usize = 4_000;

    // 他の測定が失敗してもロックを使い続けられるようにpoisonは無視する
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
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
