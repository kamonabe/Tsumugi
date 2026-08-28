//! スケーリング回帰テスト: 入力量に対する計算量オーダーを固定する
//!
//! 実時間ではなく**確保バイト数**を計測する。実時間はCIランナーの負荷で揺れるが、
//! 確保量は決定的なので、O(n) と O(n^2) の区別を安定して検出できる。
//!
//! 検証している性質:
//! - `for` の反復コストが要素数に線形であること（AUD-038で修正した退行）
//! - 関数呼び出しのコストが関数body長に依存しないこと（AUD-040で修正した退行）
//! - クロージャ定義のコストが可視bindingの数に依存しないこと（AUD-042）
//! - コレクションへ溜めたクロージャが解放されること（AUD-042の参照循環）
//! - 呼び出しのコストがtop-level bindingの数に依存しないこと（AUD-046）
//!
//! 解放漏れの検出には確保量ではなく生存量（確保 - 解放）を使う。
//!
//! 注意: グローバルアロケータでプロセス全体の確保量を数えるため、測定中は
//! `MEASURE_LOCK` を保持して他の測定と直列化する。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

/// 測定を直列化するロック（並列テストによる確保量の相互汚染を防ぐ）
static MEASURE_LOCK: Mutex<()> = Mutex::new(());

use tsumugi::compiler::Compiler;
use tsumugi::eval::Evaluator;
use tsumugi::lexer::Lexer;
use tsumugi::parser::Parser;
use tsumugi::vm::Vm;

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
/// 生存中のバイト数（確保で加算、解放で減算）。解放漏れの検出に使う。
static LIVE: AtomicIsize = AtomicIsize::new(0);

/// 確保バイト数と生存バイト数を数えるアロケータ
struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATED.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        LIVE.fetch_add(
            new_size as isize - layout.size() as isize,
            Ordering::Relaxed,
        );
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

/// 関数ローカルのコレクションへクロージャを溜め、そのまま関数を抜けるスクリプト
///
/// クロージャ本体は `i` だけを参照する。にもかかわらず定義時に見える `saved` まで
/// 捕捉すると、cell→list→closure→captured→cell の参照循環になり、関数を抜けて
/// `saved` がスコープから消えてもセルが解放されない（AUD-042）。
fn closure_container_source(n: usize) -> String {
    format!(
        "fn build(n)\n    let saved = []\n    for i in range(0, n)\n        push(saved, fn() i end)\n    end\n    return len(saved)\nend\nlet built = build({n})\n"
    )
}

/// 可視bindingを増やしながら、同じ本体のクロージャを繰り返し定義するスクリプト
///
/// クロージャ本体が参照するのは `i` と `x` だけなので、`var*` を増やしても
/// 定義コストは変わらないはず。定義時に見える全bindingを捕捉していると、
/// 可視bindingの数に比例して確保量が増える（AUD-042）。
fn closure_def_source(visible_bindings: usize, defs: usize) -> String {
    let mut source = String::new();
    for i in 0..visible_bindings {
        source.push_str(&format!("let var{i} = {i}\n"));
    }
    // 定義コストだけを測るため、作ったクロージャは呼び出さない
    // （呼び出すと、呼び出し側の固定コストが混ざる）
    source.push_str(&format!("for i in range(0, {defs})\n"));
    source.push_str("    let f = fn(x) return x + i end\nend\n");
    source
}

/// top-level bindingを増やしながら、同じ関数を同じ回数呼ぶスクリプト
///
/// 関数本体は引数だけを使う。にもかかわらず呼び出しごとにglobal scopeを
/// 複製すると、確保量がtop-level bindingの数に比例する（AUD-046）。
fn call_with_globals_source(globals: usize, calls: usize) -> String {
    let mut source = String::new();
    for i in 0..globals {
        source.push_str(&format!("let var{i} = {i}\n"));
    }
    source.push_str("fn identity(x)\n    return x\nend\nlet total = 0\n");
    source.push_str(&format!("for i in range(0, {calls})\n"));
    source.push_str("    total = total + identity(i)\nend\n");
    source
}

/// 実行が終わってengineを破棄した後も解放されずに残ったバイト数を返す
fn retained_bytes(source: &str, use_vm: bool) -> usize {
    let tokens = Lexer::new(source).tokenize();
    let program = Parser::new(tokens).parse().expect("パースに失敗");

    let before = LIVE.load(Ordering::Relaxed);
    if use_vm {
        let chunk = Compiler::new().compile(&program).expect("コンパイルに失敗");
        let mut vm = Vm::new(chunk);
        vm.run().expect("VM実行に失敗");
        drop(vm);
    } else {
        let mut evaluator = Evaluator::new();
        evaluator.run(&program).expect("ツリーウォーク実行に失敗");
        drop(evaluator);
    }
    LIVE.load(Ordering::Relaxed).saturating_sub(before).max(0) as usize
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

#[test]
fn closures_stored_in_a_container_are_released_in_both_engines() {
    // 実行後に残る量はクロージャ数に依存しないはず。参照循環があると
    // 1クロージャあたり数百バイト規模で残るため、n=400なら十数万バイトになる。
    // 固定コスト（初回のlazy初期化など）を吸収するため上限は16KiBとする。
    const LIMIT_BYTES: usize = 16 * 1024;
    const SMALL: usize = 200;
    const LARGE: usize = 400;

    // 他の測定が失敗してもロックを使い続けられるようにpoisonは無視する
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree-walk" };
        let small = retained_bytes(&closure_container_source(SMALL), use_vm);
        let large = retained_bytes(&closure_container_source(LARGE), use_vm);

        assert!(
            large < LIMIT_BYTES,
            "{mode}: クロージャを溜めたコレクションが解放されていません。\
             n={SMALL}で{small}バイト, n={LARGE}で{large}バイト残存（上限 {LIMIT_BYTES}）。\
             クロージャがコンテナ自体を捕捉して参照循環になっていないか確認してください"
        );
        assert!(
            large <= small + LIMIT_BYTES,
            "{mode}: 残存量がクロージャ数に比例しています。\
             n={SMALL}で{small}バイト, n={LARGE}で{large}バイト残存"
        );
    }
}

#[test]
fn closure_definition_allocation_is_independent_of_visible_bindings_in_both_engines() {
    // 可視bindingを20倍にしても、クロージャ本体が参照する名前が同じなら
    // 定義コストはほぼ変わらないはず。余分なbindingのlet自体の分だけ増える。
    const LIMIT: f64 = 2.0;
    const FEW: usize = 5;
    const MANY: usize = 100;
    const DEFS: usize = 2_000;

    // 他の測定が失敗してもロックを使い続けられるようにpoisonは無視する
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let few_source = closure_def_source(FEW, DEFS);
    let many_source = closure_def_source(MANY, DEFS);

    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree-walk" };
        let few = execute_bytes(&few_source, use_vm);
        let many = execute_bytes(&many_source, use_vm);
        assert!(few > 0, "{mode}: 確保量が計測できていません");

        let ratio = many as f64 / few as f64;
        assert!(
            ratio < LIMIT,
            "{mode}: クロージャ定義の確保量が可視bindingの数に比例しています。\
             binding {FEW}個で{few}バイト, {MANY}個で{many}バイト\
             （比 {ratio:.2} >= {LIMIT}）。\
             定義時に本体で言及されない binding まで捕捉していないか確認してください"
        );
    }
}

#[test]
fn call_allocation_is_independent_of_global_count_in_both_engines() {
    // top-level bindingを20倍にしても、呼び出し回数が同じなら確保量は
    // ほぼ変わらないはず。余分なbindingのlet自体の分だけ増える。
    const LIMIT: f64 = 2.0;
    const FEW: usize = 5;
    const MANY: usize = 100;
    const CALLS: usize = 2_000;

    // 他の測定が失敗してもロックを使い続けられるようにpoisonは無視する
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let few_source = call_with_globals_source(FEW, CALLS);
    let many_source = call_with_globals_source(MANY, CALLS);

    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree-walk" };
        let few = execute_bytes(&few_source, use_vm);
        let many = execute_bytes(&many_source, use_vm);
        assert!(few > 0, "{mode}: 確保量が計測できていません");

        let ratio = many as f64 / few as f64;
        assert!(
            ratio < LIMIT,
            "{mode}: 呼び出しの確保量がtop-level bindingの数に比例しています。\
             binding {FEW}個で{few}バイト, {MANY}個で{many}バイト\
             （比 {ratio:.2} >= {LIMIT}）。\
             呼び出しごとにglobal scopeを複製していないか確認してください"
        );
    }
}

/// ループ内でコレクションを読み取るスクリプト（AUD-041）
///
/// 読み取りのたびにコレクション全体を複製すると、確保量がO(n^2)になる。
/// index式は副作用のない形（識別子・演算）にしてある。
fn collection_read_sources(n: usize) -> [(&'static str, String); 3] {
    [
        (
            "list-index",
            format!(
                "let xs = range(0, {n})\nlet total = 0\nfor i in range(0, {n})\n    total = total + xs[i]\nend\n"
            ),
        ),
        (
            "dict-index",
            format!(
                "let d = {{}}\nfor i in range(0, {n})\n    d[to_str(i)] = i\nend\nlet ks = keys(d)\nlet total = 0\nfor k in ks\n    total = total + d[k]\nend\n"
            ),
        ),
        (
            "len",
            format!(
                "let xs = range(0, {n})\nlet total = 0\nfor i in range(0, {n})\n    total = total + len(xs)\nend\n"
            ),
        ),
    ]
}

#[test]
fn collection_read_allocation_stays_linear_in_both_engines() {
    // 入力を2倍にしたときの確保量の伸び。線形なら約2倍、二次なら約4倍になる。
    const LIMIT: f64 = 3.0;
    const SMALL: usize = 500;
    const LARGE: usize = 1_000;

    // 他の測定が失敗してもロックを使い続けられるようにpoisonは無視する
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let small_sources = collection_read_sources(SMALL);
    let large_sources = collection_read_sources(LARGE);

    for ((label, small_source), (_, large_source)) in small_sources.iter().zip(large_sources.iter())
    {
        for use_vm in [false, true] {
            let mode = if use_vm { "VM" } else { "tree-walk" };
            let small = execute_bytes(small_source, use_vm);
            let large = execute_bytes(large_source, use_vm);
            assert!(small > 0, "{mode}/{label}: 確保量が計測できていません");

            let ratio = large as f64 / small as f64;
            assert!(
                ratio < LIMIT,
                "{mode}/{label}: コレクション読み取りの確保量が線形を超えて増えています。\
                 n={SMALL}で{small}バイト, n={LARGE}で{large}バイト（比 {ratio:.2} >= {LIMIT}）。\
                 読み取りのたびにコレクション全体を複製していないか確認してください"
            );
        }
    }
}
