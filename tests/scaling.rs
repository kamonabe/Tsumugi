//! スケーリング回帰テスト: 入力量に対する計算量オーダーを固定する
//!
//! 実時間ではなく**確保バイト数**を計測する。実時間はCIランナーの負荷で揺れるが、
//! 確保量は決定的なので、O(n) と O(n^2) の区別を安定して検出できる。
//!
//! 検証している性質:
//! - `for` の反復コストが要素数に線形であること（AUD-038で修正した退行）
//! - コレクション読み取りのコストが要素数に線形であること（AUD-041で修正した退行）
//! - 関数呼び出しを含むindex読み取り・upvalue経由の読み取りが線形であること（AUD-047のCOW）
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
        evaluator
            .run(&program, source.len() as u64)
            .expect("ツリーウォーク実行に失敗");
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
            evaluator
                .run(&program, source.len() as u64)
                .expect("ツリーウォーク実行に失敗");
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

/// AUD-041 の参照読みが効かない読み取り経路（AUD-047 の COW が担当する）
///
/// AUD-041 は「副作用のない識別子 index」だけを参照読みにしたため、次の2経路は
/// 従来コレクション全体を複製し O(n^2) になっていた。COW 化で読み取りは共有 Rc の
/// ハンドル clone（O(1)）＋要素 clone だけになり、線形に収まる。
/// - `d[to_str(i)]`: index 式に関数呼び出しを含むため参照読みの対象外だった
/// - `xs[i]`（upvalue 経由）: capture したコレクションの読み取りは `GetUpvalue` の
///   clone でコレクション全体を複製していた
fn cow_read_sources(n: usize) -> [(&'static str, String); 2] {
    [
        (
            "dict-index-with-call",
            format!(
                "let d = {{}}\nfor i in range(0, {n})\n    d[to_str(i)] = i\nend\nlet total = 0\nfor i in range(0, {n})\n    total = total + d[to_str(i)]\nend\n"
            ),
        ),
        (
            "upvalue-list-index",
            format!(
                "let xs = range(0, {n})\nlet read = fn(i) xs[i] end\nlet total = 0\nfor i in range(0, {n})\n    total = total + read(i)\nend\n"
            ),
        ),
    ]
}

#[test]
fn cow_read_allocation_stays_linear_in_both_engines() {
    // 入力を2倍にしたときの確保量の伸び。線形なら約2倍、二次なら約4倍になる。
    // COW 前はコレクション全体の複製で約4倍（O(n^2)）だった。
    const LIMIT: f64 = 3.0;
    const SMALL: usize = 500;
    const LARGE: usize = 1_000;

    // 他の測定が失敗してもロックを使い続けられるようにpoisonは無視する
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let small_sources = cow_read_sources(SMALL);
    let large_sources = cow_read_sources(LARGE);

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
                "{mode}/{label}: COW 対象の読み取りの確保量が線形を超えて増えています。\
                 n={SMALL}で{small}バイト, n={LARGE}で{large}バイト（比 {ratio:.2} >= {LIMIT}）。\
                 読み取りのたびにコレクション全体を複製していないか確認してください"
            );
        }
    }
}

/// REV-015 Slice 2（§5.2 context baseline / §15.2 heap）:
/// 論理 baseline heap が要素数に線形であることを、公開 `BudgetLedger` API で固定する。
///
/// 実アロケータではなく論理台帳を測るため決定的で、`MEASURE_LOCK` は不要。
/// List backing は要素数 n に対し `24 + 32*n` + 要素ごとの Value slot 32 なので、
/// baseline は n に対して線形に増える（O(n)）。
#[test]
fn context_baseline_heap_is_linear_in_element_count() {
    use tsumugi::budget::{BudgetConfig, BudgetLedger, ExecutionPhase};
    use tsumugi::value::Value;

    fn baseline_for(n: usize) -> u64 {
        let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
        config.max_live_heap_bytes = u64::MAX;
        let mut ledger = BudgetLedger::with_config(config);
        let list = Value::List(tsumugi::value::Tracked::constant(
            (0..n as i64).map(Value::Int).collect::<Vec<_>>(),
        ));
        ledger
            .charge_context_baseline([list], ExecutionPhase::Link)
            .expect("baseline 課金に失敗");
        ledger.usage().live_heap_bytes
    }

    // n を 10 倍にしたら baseline も概ね 10 倍（固定 header 分だけ超える）に収まる。
    let small = baseline_for(100);
    let large = baseline_for(1_000);
    assert!(small > 0, "baseline が計測できていません");

    let ratio = large as f64 / small as f64;
    assert!(
        (9.0..=11.0).contains(&ratio),
        "baseline heap が要素数に線形ではありません: n=100 で {small} バイト, \
         n=1000 で {large} バイト（比 {ratio:.2}、期待 9.0..=11.0）"
    );
}

/// §5.2 visited set: 同じ backing を複数 root から指しても baseline は 1 回だけ課金する。
/// 共有した場合と分離した場合で、共有側が厳密に小さいことを固定する。
#[test]
fn context_baseline_dedups_shared_backing() {
    use std::rc::Rc;
    use tsumugi::budget::{BudgetConfig, BudgetLedger, ExecutionPhase};
    use tsumugi::value::Value;

    fn ledger() -> BudgetLedger {
        let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
        config.max_live_heap_bytes = u64::MAX;
        BudgetLedger::with_config(config)
    }

    let shared_backing =
        tsumugi::value::Tracked::constant((0..200i64).map(Value::Int).collect::<Vec<_>>());
    let mut shared = ledger();
    shared
        .charge_context_baseline(
            [
                Value::List(Rc::clone(&shared_backing)),
                Value::List(Rc::clone(&shared_backing)),
            ],
            ExecutionPhase::Link,
        )
        .expect("baseline 課金に失敗");

    let mut distinct = ledger();
    distinct
        .charge_context_baseline(
            [
                Value::List(tsumugi::value::Tracked::constant(
                    (0..200i64).map(Value::Int).collect::<Vec<_>>(),
                )),
                Value::List(tsumugi::value::Tracked::constant(
                    (0..200i64).map(Value::Int).collect::<Vec<_>>(),
                )),
            ],
            ExecutionPhase::Link,
        )
        .expect("baseline 課金に失敗");

    assert!(
        shared.usage().live_heap_bytes < distinct.usage().live_heap_bytes,
        "共有 backing が dedup されていません: shared={} distinct={}",
        shared.usage().live_heap_bytes,
        distinct.usage().live_heap_bytes
    );
}

/// REV-015 案A / §15.2: per-drop release で live heap が回復する。
/// 同じ論理サイズの List を N 回作っては drop すると、live heap は N に依存せず
/// 1 個ぶんに収まる（O(1)）。全体を保持し続ければ O(N) になるのと対比する。
#[test]
fn live_heap_is_bounded_across_allocate_and_free() {
    use tsumugi::budget::{BudgetConfig, BudgetLedger, ExecutionPhase};
    use tsumugi::value::Value;

    fn ledger() -> BudgetLedger {
        let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
        config.max_live_heap_bytes = u64::MAX;
        BudgetLedger::with_config(config)
    }

    // 作っては即 drop（変数への再代入相当）を N 回。live heap は常に高々 1 個ぶん。
    fn peak_holding_one(n: usize) -> u64 {
        let mut budget = ledger();
        let mut held: Option<Value> = None;
        for _ in 0..n {
            let list = Value::new_list(
                (0..50i64).map(Value::Int).collect::<Vec<_>>(),
                &mut budget,
                ExecutionPhase::Run,
            )
            .unwrap();
            // 直前の held を上書き → drop → release。
            held = Some(list);
        }
        drop(held);
        budget.live_heap_bytes()
    }

    // 全部保持すると live は N に比例する（対照）。
    fn holding_all(n: usize) -> u64 {
        let mut budget = ledger();
        let mut all = Vec::new();
        for _ in 0..n {
            all.push(
                Value::new_list(
                    (0..50i64).map(Value::Int).collect::<Vec<_>>(),
                    &mut budget,
                    ExecutionPhase::Run,
                )
                .unwrap(),
            );
        }
        let live = budget.live_heap_bytes();
        drop(all);
        live
    }

    // 作っては捨てると、最後に held を drop したので live は 0。N=10 と N=1000 で不変。
    assert_eq!(peak_holding_one(10), 0);
    assert_eq!(peak_holding_one(1000), 0);

    // 全保持は N に比例（10 個ぶん vs 1000 個ぶん）。release と対照的に線形増加する。
    let ten = holding_all(10);
    let thousand = holding_all(1000);
    assert!(ten > 0);
    let ratio = thousand as f64 / ten as f64;
    assert!(
        (95.0..=105.0).contains(&ratio),
        "全保持 live heap が N に線形でない: N=10 で {ten}, N=1000 で {thousand}（比 {ratio:.1}）"
    );
}

/// REV-015 案A: push は delta 課金であり、N 回 push した後の live heap は
/// 最終要素数ぶん（O(N)）に収まる。中間 backing を全体再課金・保持すると O(N^2) に
/// なるが、delta 課金＋in-place 更新ならそうならない（live は最終サイズのみ）。
#[test]
fn push_delta_charging_live_heap_is_linear_not_quadratic() {
    use tsumugi::budget::{BudgetConfig, BudgetLedger, ExecutionPhase};
    use tsumugi::value::Value;

    fn final_live_after_pushes(n: usize) -> u64 {
        let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
        config.max_live_heap_bytes = u64::MAX;
        let mut budget = BudgetLedger::with_config(config);
        let mut list = Value::new_list(vec![], &mut budget, ExecutionPhase::Run).unwrap();
        for i in 0..n as i64 {
            list.list_push_tracked(Value::Int(i), &mut budget, ExecutionPhase::Run)
                .unwrap();
        }
        let live = budget.live_heap_bytes();
        drop(list);
        // drop 後は 0 に戻る（delta で積んだぶんが 1 個の backing の release で戻る）。
        assert_eq!(
            budget.live_heap_bytes(),
            0,
            "push 後の drop で live が 0 に戻らない"
        );
        live
    }

    // 一意所有の in-place push は最終サイズ（list_body(n)）だけが live。
    let small = final_live_after_pushes(100);
    let large = final_live_after_pushes(1000);
    // list_body(n) = 24 + 32n。1000/100 の比は約 10（固定 header ぶんだけ超える）。
    let ratio = large as f64 / small as f64;
    assert!(
        (9.0..=11.0).contains(&ratio),
        "push 後 live heap が要素数に線形でない: n=100 で {small}, n=1000 で {large}（比 {ratio:.2}）"
    );
}

/// REV-015 PR-b / §15.2: String body の per-drop release で live heap が回復する。
/// 同じ論理サイズの String を N 回作っては drop すると、live heap は N に依存せず
/// 高々 1 個ぶんに収まる（O(1)）。全体を保持し続ければ O(N) になるのと対比する。
///
/// 実アロケータではなく論理台帳を測るため決定的で、`MEASURE_LOCK` は不要。
#[test]
fn string_live_heap_is_bounded_across_allocate_and_free() {
    use tsumugi::budget::{BudgetConfig, BudgetLedger, ExecutionPhase};
    use tsumugi::value::Value;

    fn ledger() -> BudgetLedger {
        let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
        config.max_live_heap_bytes = u64::MAX;
        BudgetLedger::with_config(config)
    }

    // 作っては即 drop（変数への再代入相当）を N 回。live heap は常に高々 1 個ぶん。
    fn hold_one_after(n: usize) -> u64 {
        let mut budget = ledger();
        let mut held: Option<Value> = None;
        for _ in 0..n {
            let s = Value::new_str("x".repeat(40), &mut budget, ExecutionPhase::Run).unwrap();
            held = Some(s); // 直前の held を上書き → drop → release。
        }
        drop(held);
        budget.live_heap_bytes()
    }

    // 全部保持すると live は N に比例する（対照）。
    fn holding_all(n: usize) -> u64 {
        let mut budget = ledger();
        let mut all = Vec::new();
        for _ in 0..n {
            all.push(Value::new_str("x".repeat(40), &mut budget, ExecutionPhase::Run).unwrap());
        }
        let live = budget.live_heap_bytes();
        drop(all);
        live
    }

    // 作っては捨てると、最後に held を drop したので live は 0。N=10 と N=1000 で不変。
    assert_eq!(hold_one_after(10), 0);
    assert_eq!(hold_one_after(1000), 0);

    // 全保持は N に比例（release と対照的に線形増加する）。
    let ten = holding_all(10);
    let thousand = holding_all(1000);
    assert!(ten > 0);
    let ratio = thousand as f64 / ten as f64;
    assert!(
        (95.0..=105.0).contains(&ratio),
        "全保持 String live heap が N に線形でない: N=10 で {ten}, N=1000 で {thousand}（比 {ratio:.1}）"
    );
}

/// REV-015 PR-b / §5.2: 同じ String backing を `Rc::clone` で共有しても追加課金せず、
/// 最後の参照 drop で 1 回だけ release する。共有と分離で live heap を対比する。
#[test]
fn shared_string_backing_is_charged_once() {
    use std::rc::Rc;
    use tsumugi::budget::{BudgetConfig, BudgetLedger, ExecutionPhase};
    use tsumugi::value::Value;

    let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
    config.max_live_heap_bytes = u64::MAX;
    let mut budget = BudgetLedger::with_config(config);

    // 1 本目を課金。
    let base = Value::new_str("shared-body".to_string(), &mut budget, ExecutionPhase::Run).unwrap();
    let one = budget.live_heap_bytes();
    assert!(one > 0);

    // Rc::clone で共有（無課金、§5.2）。live heap は変わらない。
    let Value::Str(ref backing) = base else {
        panic!("Str が返るはず");
    };
    let alias = Value::Str(Rc::clone(backing));
    assert_eq!(
        budget.live_heap_bytes(),
        one,
        "共有で追加課金してはならない"
    );

    // 1 本目を drop してもまだ alias が生きているので release されない。
    drop(base);
    assert_eq!(
        budget.live_heap_bytes(),
        one,
        "共有中の drop で release してはならない"
    );

    // 最後の参照が落ちて初めて release。
    drop(alias);
    assert_eq!(
        budget.live_heap_bytes(),
        0,
        "最後の参照 drop で release されていない"
    );
}

/// REV-015 PR-b: engine 実行後、builtin が生成した String（track_result 経由で live
/// heap に載る）が engine の破棄で解放され、残存量が生成量に依存しないことを固定する。
/// tree/VM 両対応。参照循環で String が残ると N に比例して残存する。
#[test]
fn strings_produced_by_builtins_are_released_in_both_engines() {
    // 実行後に残る量は生成した String 数に依存しないはず。固定コスト（lazy 初期化）を
    // 吸収するため上限は 16KiB とする。
    const LIMIT_BYTES: usize = 16 * 1024;
    const SMALL: usize = 200;
    const LARGE: usize = 2_000;

    // upper() は builtin なので dispatch 境界の track_result で live heap に載る。
    // 生成した String をどこにも溜めないので、実行終了後は残らないはず。
    fn source(n: usize) -> String {
        format!("let s = \"payload-string\"\nfor i in range(0, {n})\n    let u = upper(s)\nend\n")
    }

    // 他の測定が失敗してもロックを使い続けられるようにpoisonは無視する
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree-walk" };
        let small = retained_bytes(&source(SMALL), use_vm);
        let large = retained_bytes(&source(LARGE), use_vm);

        assert!(
            large < LIMIT_BYTES,
            "{mode}: builtin が生成した String が解放されていません。\
             n={SMALL}で{small}バイト, n={LARGE}で{large}バイト残存（上限 {LIMIT_BYTES}）"
        );
        assert!(
            large <= small + LIMIT_BYTES,
            "{mode}: 残存量が生成 String 数に比例しています。\
             n={SMALL}で{small}バイト, n={LARGE}で{large}バイト残存"
        );
    }
}

/// REV-015 PR-c: 変数 cell と関数 instance header の per-drop release で live heap が
/// 回復する。同じ論理サイズの cell / 関数 header を N 回作っては drop すると、live heap
/// は N に依存せず高々 1 個ぶんに収まる（O(1)）。全体保持と対比する。
///
/// 実アロケータではなく論理台帳を測るため決定的で、`MEASURE_LOCK` は不要。
#[test]
fn cell_and_fn_header_live_heap_is_bounded_across_allocate_and_free() {
    use tsumugi::budget::{BudgetConfig, BudgetLedger, ExecutionPhase, HeapLedgerWeak};
    use tsumugi::value::Value;

    fn ledger() -> BudgetLedger {
        let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
        config.max_live_heap_bytes = u64::MAX;
        BudgetLedger::with_config(config)
    }

    // 作っては即 drop（scope 離脱相当）を N 回。live heap は常に高々 1 個ぶん。
    fn hold_one_cell(n: usize, handle: &HeapLedgerWeak) {
        let mut held = None;
        for _ in 0..n {
            held = Some(Value::new_cell(Value::Int(1), handle, ExecutionPhase::Run).unwrap());
        }
        drop(held);
    }

    let budget = ledger();
    let handle = budget.heap_handle();
    hold_one_cell(10, &handle);
    assert_eq!(budget.live_heap_bytes(), 0, "cell: N=10 で解放漏れ");
    hold_one_cell(1000, &handle);
    assert_eq!(budget.live_heap_bytes(), 0, "cell: N=1000 で解放漏れ");

    // 関数 instance header も同様に per-drop で戻る。
    let mut held = None;
    for _ in 0..1000 {
        held = Some(Value::new_tree_fn_header(2, &handle, ExecutionPhase::Run).unwrap());
    }
    drop(held);
    assert_eq!(budget.live_heap_bytes(), 0, "fn header: 解放漏れ");
}

/// REV-015 PR-c: engine 実行後、大量に定義したクロージャ（cell + 関数 instance header
/// を含む）が engine の破棄で解放され、残存量が定義数に依存しないことを tree/VM 両方で
/// 固定する。cell / header が per-drop されないと定義数に比例して残存する。
#[test]
fn closures_defined_in_a_loop_are_released_in_both_engines() {
    // 実行後に残る量は定義したクロージャ数に依存しないはず。固定コスト吸収で上限 16KiB。
    const LIMIT_BYTES: usize = 16 * 1024;
    const SMALL: usize = 200;
    const LARGE: usize = 2_000;

    // ループ内でクロージャを定義するが、どこにも溜めない（各反復で drop される）。
    // クロージャは外側の n だけを参照し、cell と関数 header を確保する。
    fn source(n: usize) -> String {
        format!("let base = 7\nfor i in range(0, {n})\n    let f = fn(x) x + base end\nend\n")
    }

    // 他の測定が失敗してもロックを使い続けられるようにpoisonは無視する
    let _guard = MEASURE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    for use_vm in [false, true] {
        let mode = if use_vm { "VM" } else { "tree-walk" };
        let small = retained_bytes(&source(SMALL), use_vm);
        let large = retained_bytes(&source(LARGE), use_vm);

        assert!(
            large < LIMIT_BYTES,
            "{mode}: ループ内で定義したクロージャが解放されていません。\
             n={SMALL}で{small}バイト, n={LARGE}で{large}バイト残存（上限 {LIMIT_BYTES}）"
        );
        assert!(
            large <= small + LIMIT_BYTES,
            "{mode}: 残存量がクロージャ定義数に比例しています。\
             n={SMALL}で{small}バイト, n={LARGE}で{large}バイト残存"
        );
    }
}
