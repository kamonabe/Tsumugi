//! REPL 持続 closure が retain する code の課金整合テスト（A-1、REV-015 Slice 6 PR-6b）。
//!
//! 設計正本 `docs/execution-control.md` §5.3「closure が retain する code の課金（A-1）」を
//! 固定する。従来は closure を定義した入力の次入力で artifact（AST / bytecode chunk）課金を
//! 丸ごと release していたため、生き残った closure が握る code（tree=`FnDef.body`、
//! VM=prototype `Rc<Chunk>`）が両 engine で under-charge されていた。A-1 はこれを closure
//! 寿命 token へ移譲し、closure が reachable な限り code を課金し続ける。
//!
//! ここで固定する不変量（§5.3 のとおり、**byte-exact な engine 間一致は要求しない**。課金の
//! 有無と release 寿命の一致を固定する）:
//! 1. 入力1 で closure を定義 → 入力2 で closure を触らない無関係入力 → 入力2 完了後も、
//!    closure が retain する code は **両 engine で課金され続ける**（従来の release で落ちない）。
//! 2. closure を drop する入力の後は、両 engine とも retained code を release する。
//! 3. ネスト closure の跨入力生成: 外側を入力1 で定義 → 入力2 で呼んで内側を生成 →
//!    内側が retain する code も両 engine で課金され続ける。
//!
//! 既存の `tests/scaling.rs` の `live_heap_after` は単一 source を 1 回実行するだけで、複数の
//! REPL 入力を 1 つの台帳で回せない（調査 §5）。本ファイルは「同一 Vm / Evaluator で複数入力を
//! 回し、入力間で `budget_usage()` を snapshot する」in-process driver を新設する。

use tsumugi::compiler::Compiler;
use tsumugi::eval::Evaluator;
use tsumugi::lexer::Lexer;
use tsumugi::parser::Parser;
use tsumugi::vm::Vm;

/// tree engine で REPL 入力列を 1 台帳で回し、各入力完了後の live heap を返す。
///
/// `src/main.rs` の tree REPL ループを模す（import は使わないので loader は省略し、
/// `run_repl_submission` が内部で link する）。
fn tree_live_heap_per_input(inputs: &[&str]) -> Vec<u64> {
    let mut evaluator = Evaluator::new();
    let mut snapshots = Vec::with_capacity(inputs.len());
    for input in inputs {
        let tokens = Lexer::new(input).tokenize();
        let program = Parser::new(tokens).parse().expect("パースに失敗");
        evaluator
            .run_repl_submission(&program, input.len() as u64)
            .expect("tree REPL 入力の実行に失敗");
        snapshots.push(evaluator.budget_usage().live_heap_bytes);
    }
    snapshots
}

/// VM engine で REPL 入力列を 1 台帳で回し、各入力完了後の live heap を返す。
///
/// `src/main.rs` の VM REPL ループを模す: 永続 `Compiler` + `Vm::new_repl()` で、入力ごとに
/// `charge_link`（source 会計）→ `compile_repl_line` → `run_repl_chunk` を回す。import は
/// 使わないので `charge_link` へは空の module slice を渡す。
fn vm_live_heap_per_input(inputs: &[&str]) -> Vec<u64> {
    let mut compiler = Compiler::new();
    let mut vm = Vm::new_repl();
    let mut snapshots = Vec::with_capacity(inputs.len());
    for input in inputs {
        let tokens = Lexer::new(input).tokenize();
        let program = Parser::new(tokens).parse().expect("パースに失敗");
        vm.charge_link(input.len() as u64, &[])
            .expect("VM charge_link に失敗");
        let chunk = compiler
            .compile_repl_line(&program)
            .expect("VM compile に失敗");
        vm.run_repl_chunk(chunk).expect("VM REPL 入力の実行に失敗");
        snapshots.push(vm.budget_usage().live_heap_bytes);
    }
    snapshots
}

/// 両 engine で入力列を回し、`(tree_snapshots, vm_snapshots)` を返す。
fn both_engines_per_input(inputs: &[&str]) -> (Vec<u64>, Vec<u64>) {
    (
        tree_live_heap_per_input(inputs),
        vm_live_heap_per_input(inputs),
    )
}

/// 不変量1: closure を定義して保持したまま無関係な入力を回しても、retained code は両 engine で
/// 課金され続ける。
///
/// 入力1 は両列で同一（closure を global f に束縛）。入力2 だけを変え、保持列は f を触らない
/// 無関係入力、対照列は f を Int へ再束縛して closure を drop する。入力2 完了後 live heap を
/// 比べ、保持列が厳密に大きいことを両 engine で確認する。これにより「closure が生きている限り
/// retained code が release されずに残り、drop すると落ちる」ことを byte-exact な engine 間一致に
/// 頼らず固定する。
#[test]
fn retained_closure_code_stays_charged_in_both_engines() {
    // 入力1: closure を global f に束縛（両列で同一）。
    // 入力2: 保持列は f を触らない無関係入力 / 対照列は f を drop（Int へ再束縛）。
    let retained = ["let f = fn(x) x + 1 end\n", "let y = 1\n"];
    let dropped = ["let f = fn(x) x + 1 end\n", "f = 0\n"];

    let (tree_retained, vm_retained) = both_engines_per_input(&retained);
    let (tree_dropped, vm_dropped) = both_engines_per_input(&dropped);

    // 入力2 完了後（index 1）で比較する。保持列は closure が生きているため retained code の
    // ぶんだけ live heap が多い。対照列では closure が死んでいるのでそのぶんがない。
    assert!(
        tree_retained[1] > tree_dropped[1],
        "tree: 保持 closure の retained code が入力2 後も課金され続けるはず \
         (retained={}, dropped={})",
        tree_retained[1],
        tree_dropped[1]
    );
    assert!(
        vm_retained[1] > vm_dropped[1],
        "VM: 保持 closure の retained code が入力2 後も課金され続けるはず \
         (retained={}, dropped={})",
        vm_retained[1],
        vm_dropped[1]
    );
}

/// 不変量2: closure を drop する入力の後は、両 engine とも retained code を release する。
///
/// 入力2 で f を closure 以外の値へ再束縛すると、closure instance は last-ref drop され、
/// retained code token も release される。入力2 完了後 live heap は、入力1 完了後（closure
/// 保持中）より小さくなる（retained code ぶんが落ちる）ことを両 engine で確認する。
#[test]
fn dropping_closure_releases_retained_code_in_both_engines() {
    let inputs = [
        "let f = fn(x) x + 1 end\n", // 入力1: closure を保持
        "f = 0\n",                   // 入力2: closure を捨てる（Int へ再束縛）
    ];
    let (tree, vm) = both_engines_per_input(&inputs);

    assert!(
        tree[1] < tree[0],
        "tree: closure を drop した入力後は retained code が release されるはず \
         (input1={}, input2={})",
        tree[0],
        tree[1]
    );
    assert!(
        vm[1] < vm[0],
        "VM: closure を drop した入力後は retained code が release されるはず \
         (input1={}, input2={})",
        vm[0],
        vm[1]
    );
}

/// 不変量3: ネスト closure の跨入力生成。外側を入力1 で定義 → 入力2 で呼んで内側を生成し保持 →
/// 内側が retain する code も両 engine で課金され続ける。
///
/// 内側 closure は外側 call が走る入力2 で初めて生成される（side-table が `Weak` の長命 index で
/// あり、生成時に subtree 全体の token を一括移譲するからこそ `Weak::upgrade` が成立する）。
/// 保持列（内側を global g に束縛）と対照列（呼ぶが内側を捨てる）の入力3 完了後 live heap を
/// 比べ、保持列が厳密に大きいことを両 engine で確認する。
#[test]
fn nested_closure_cross_input_creation_stays_charged() {
    // 入力1: 外側 outer を定義（内側 inner はまだ生成されない）。inner は lambda 式で返す。
    // 入力2: outer() を呼んで inner を生成し g に束縛（跨入力生成）。
    // 入力3: inner を触らない無関係入力。
    let retained = [
        "fn outer()\n  return fn(x) return x + 1 end\nend\n",
        "let g = outer()\n",
        "let z = 2\n",
    ];
    // 対照: 入力3 で g を drop（Int へ再束縛）し、inner を落とす。入力1/2 は保持列と同一。
    let dropped = [
        "fn outer()\n  return fn(x) return x + 1 end\nend\n",
        "let g = outer()\n",
        "g = 0\n",
    ];

    let (tree_retained, vm_retained) = both_engines_per_input(&retained);
    let (tree_dropped, vm_dropped) = both_engines_per_input(&dropped);

    // 入力3 完了後（index 2）で比較する。保持列は inner が生きているため inner の retained
    // code ぶんだけ live heap が多い。対照列は入力3 で inner を落とすのでそのぶんがない。
    assert!(
        tree_retained[2] > tree_dropped[2],
        "tree: 跨入力生成した内側 closure の retained code が課金され続けるはず \
         (retained={}, dropped={})",
        tree_retained[2],
        tree_dropped[2]
    );
    assert!(
        vm_retained[2] > vm_dropped[2],
        "VM: 跨入力生成した内側 closure の retained code が課金され続けるはず \
         (retained={}, dropped={})",
        vm_retained[2],
        vm_dropped[2]
    );
}

/// 課金の有無・release 寿命が両 engine で一致することの直接確認（§5.3）。
///
/// byte-exact な数値一致（A-2）は要求しないが、「保持列は対照列より多い」という**符号の向き**は
/// 両 engine で揃うべき。各不変量を 1 つの実行で突き合わせ、tree と VM で同じ向きの差分が出る
/// ことを確認する（片方だけ落ちる非対称を禁じる）。
#[test]
fn charge_presence_and_lifetime_agree_across_engines() {
    let retained = ["let f = fn(x) x + 1 end\n", "let y = 1\n"];
    let dropped = ["let f = fn(x) x + 1 end\n", "f = 0\n"];

    let tree_retained = tree_live_heap_per_input(&retained)[1];
    let tree_dropped = tree_live_heap_per_input(&dropped)[1];
    let vm_retained = vm_live_heap_per_input(&retained)[1];
    let vm_dropped = vm_live_heap_per_input(&dropped)[1];

    let tree_delta = tree_retained as i128 - tree_dropped as i128;
    let vm_delta = vm_retained as i128 - vm_dropped as i128;

    // 課金の有無が一致 = どちらの engine でも「保持列 - drop 列 > 0」。数値は engine 固有で
    // 異なってよい（tree=ast_node / VM=bytecode_chunk）が、符号の向きは一致する。
    assert!(
        tree_delta > 0 && vm_delta > 0,
        "retained code 課金の有無が両 engine で一致するはず \
         (tree_delta={tree_delta}, vm_delta={vm_delta})"
    );
}
