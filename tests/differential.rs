//! tree/VM differential charge-parity matrix（REV-015 Slice 6 PR-6c）。
//!
//! 設計正本 `docs/determinism-and-audit.md` §15.5 / `docs/execution-control.md` §15.5 の
//! differential gate のうち、**PR-6c の射程 = charge/budget parity matrix** を自動化する。
//! 1 つの fixture（source 群 + 上限設定）を宣言すると、tree と VM の両 engine で実行し、
//! budget usage・terminal reason・stdout/stderr を突合する。
//!
//! ## 比較契約: 軸別比較ポリシー（最重要・設計 §5.3 と整合）
//!
//! **`BudgetUsage` の全 field byte-exact 一致は tree/VM 間の不変量として成立しない。** これは
//! prod のバグや usage API の不足ではなく、設計上の既知差である。設計正本
//! `docs/execution-control.md` §5.3 が明記するとおり、A-1 で backend 間で揃えるのは
//! **「課金の有無と release 寿命」** であって `live_heap` / `peak_heap` の byte 単位の数値一致
//! ではない（byte-exact を狙う案 A-2 は却下済み）。tree は AST を・VM は bytecode chunk を
//! 課金するため code artifact のサイズは engine 固有に異なってよい。fuel count も storage
//! モデル差による既知差で engine 間で非可換（fixture により tree>VM にも VM>tree にもなる）。
//!
//! よって本 harness は **軸ごとに比較ポリシーを変える**:
//!
//! - **(A) engine 間不変な軸 → exact 突合**:
//!   - terminal reason（`TsumugiError` 全 field: kind/message/line/trace）。`PartialEq` derive 済みで
//!     `Option<TsumugiError>` を `assert_eq!` 一発。既存 `canonical_error_inventory.rs` と同じ契約。
//!   - I/O counter（`output_calls`/`output_bytes`/`host_calls`/`host_request_bytes`/
//!     `host_response_bytes`/`host_call_bytes`/`input_calls`/`input_bytes`）。
//!   - string/source/import の count 系（`string_allocations`/`string_bytes`/`source_count`/
//!     `source_bytes`/`import_count`/`import_bytes`）。
//!   - per-item peak（`single_string_bytes`/`single_source_bytes`/`collection_elements`）。
//! - **(B) engine 間で非可換な軸 → 向き・符号・release 寿命の一致（または比較対象外）**:
//!   - `live_heap_bytes` / `peak_heap_bytes`（code artifact の engine 固有サイズ差）→ 単発では
//!     「課金の有無（>0 か）の向き」、REPL 継続では「入力間増減の符号」を両 engine で突合する。
//!     byte 単位の絶対値一致は要求しない。既存 `scaling.rs`（線形性のみ突合）・`closure_retain.rs`
//!     （符号の向きのみ突合）と同じ粒度。
//!   - `committed.fuel`（step 課金）→ **向き・符号ですら engine 間で一致しない**。tree は成功 path で
//!     committed.fuel=0 を snapshot する（fuel は入力ごとに reset され committed へ積まない）が
//!     VM は実歩数を積む。さらに fuel 枯渇の terminal `line` も step 粒度差で tree/VM で異なる。
//!     よって fuel parity は **fuel 境界 fixture（StepLimit terminal の kind/message observable 一致）**
//!     と **step env subprocess 行**で担保し、usage の committed.fuel は比較対象から外す。
//!
//! この軸別ポリシーは設計 §5.3 の A-1（byte-exact 一致は backend 間で成立しないため要求しない）
//! と整合する。`docs/execution-control.md` Slice 6 / `docs/determinism-and-audit.md` §15.5 の
//! 線引きにも同じ方針を明文化してある（後から「なぜ全 field exact にしていないのか」を追える）。
//!
//! stdout/stderr の文字列 byte 一致は subprocess 層で突合する（in-process では VM の capability
//! setter が public でないため、in-process は I/O counter の exact 一致で代替し、byte 一致は
//! subprocess で見る。調査 §stdout 取得を参照）。
//!
//! ## PR-6c の線引き（harness の射程外・defer 先）
//!
//! この harness が **やらない**こと（`docs/execution-control.md` Slice 6 / §15.5・
//! `docs/determinism-and-audit.md` §15.5 の線引きと対応）:
//! - execution-trace determinism（yield 位置 / host-effect 順序ログ / audit payload /
//!   charge 系列 trace の完全一致）→ Phase 5/6 の differential gate。
//! - pause/resume・cancel・host pending → VM は scheduler 非経由（Ready フォールバック・
//!   `vm.rs` 不変）で実行経路が無く differential が原理的に成立しない。**VM 対象外**。
//!   tree 側は `tests/scheduler_api.rs` で担保済み。
//! - fuzz/stress/生成 matrix → AUD-022 / REV-024 / Phase 7。
//!
//! **PR-6c-min 完了 = Slice 6 charge-parity-matrix 自動化の完了であって、VM の experimental
//! 卒業ではない。** 卒業 gate は determinism/audit conformance（後続フェーズの管轄）。
//!
//! ## 上限注入経路の非対称性（2 層構成の根拠）
//!
//! configured per-resource 上限の注入経路は engine 間で非対称:
//! - tree は in-process で `Evaluator::with_budget(BudgetConfig)` で任意の per-resource 上限を
//!   注入できる。
//! - **VM は in-process で per-resource 上限を注入する public API が無い**（`Vm::new`/`new_repl`
//!   が `BudgetConfig::from_legacy_env()` を内部固定呼び出し、公開 setter は `set_max_steps`
//!   のみ）。
//!
//! よって:
//! - 既定上限 + fuel だけで足りる行は **in-process 層**で usage 数値まで突合する（`Limits::Default`
//!   / `Limits::Fuel`）。両 engine とも `from_legacy_env` 既定（fuel は `for_legacy`/`set_max_steps`
//!   で対称に注入）で回せる。
//! - configured per-resource 上限（collection/string/source/I-O/heap など）を使う行は
//!   **subprocess 層**で `TSUMUGI_MAX_*` env を注入する（env は `from_legacy_env` 経由で tree/VM
//!   両方へ対称に効く唯一の経路）。`Limits::Env`。

use std::process::Command;

use tsumugi::budget::{BudgetConfig, BudgetUsage};
use tsumugi::compiler::Compiler;
use tsumugi::error::TsumugiError;
use tsumugi::eval::Evaluator;
use tsumugi::lexer::Lexer;
use tsumugi::parser::Parser;
use tsumugi::vm::Vm;

// =============================================================================
// 上限の宣言
// =============================================================================

/// fixture に適用する実行予算上限と、それに対応する実行層。
#[derive(Debug, Clone, Copy)]
enum Limits {
    /// 既定上限（`from_legacy_env`）。両 engine を in-process で回して usage を数値突合する。
    Default,
    /// fuel 上限のみ注入。in-process で回す。tree は `with_budget(for_legacy(fuel, 1_000_000))`、
    /// VM は `new`/`new_repl` + `set_max_steps(fuel)`。collection 上限は既定（1_000_000）で両者一致。
    Fuel(u64),
}

/// subprocess 層で `TSUMUGI_MAX_*` env により per-resource 上限を注入する fixture の宣言。
#[derive(Debug, Clone)]
struct EnvLimits {
    envs: Vec<(&'static str, &'static str)>,
}

// =============================================================================
// in-process ランナー: 単発実行
// =============================================================================

/// 単発実行の宣言。
struct SingleFixture {
    /// 失敗時のメッセージに使うラベル。
    label: &'static str,
    /// 実行する 1 入力の source。
    source: &'static str,
    /// 適用する上限（in-process で回せる `Default` / `Fuel` のみ）。
    limits: Limits,
    /// 期待する実行結果。`Ok` は成功、`Err` は両 engine が同一 `TsumugiError` で terminal。
    expect: ExpectOutcome,
}

/// 単発 fixture の期待結果の分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectOutcome {
    /// 両 engine が成功する（usage は engine 間で全一致）。
    Success,
    /// 両 engine が失敗する（同一 `TsumugiError` で terminal）。
    Failure,
}

/// in-process で 1 入力を tree 実行し、`(usage, result)` を返す。
///
/// tree 単発入口 `Evaluator::run(&program, source_len)`（`src/eval.rs:753`）を模す。
fn run_tree_single(source: &str, limits: Limits) -> (BudgetUsage, Result<(), TsumugiError>) {
    let tokens = Lexer::new(source).tokenize();
    let program = Parser::new(tokens).parse().expect("パースに失敗");
    let mut evaluator = match limits {
        Limits::Default => Evaluator::new(),
        // VM の `from_legacy_env` 既定 collection 上限（1_000_000）に合わせる。
        Limits::Fuel(fuel) => Evaluator::with_budget(BudgetConfig::for_legacy(fuel, 1_000_000)),
    };
    let result = evaluator.run(&program, source.len() as u64);
    (evaluator.budget_usage(), result)
}

/// in-process で 1 入力を VM 実行し、`(usage, result)` を返す。
///
/// VM 単発入口 `Vm::new(Compiler::new().compile(&program)?).run()` を模す。
fn run_vm_single(source: &str, limits: Limits) -> (BudgetUsage, Result<(), TsumugiError>) {
    let tokens = Lexer::new(source).tokenize();
    let program = Parser::new(tokens).parse().expect("パースに失敗");
    // compile は tree の link と同じく実行前段。compile error も terminal reason として突合する。
    let chunk = match Compiler::new().compile(&program) {
        Ok(chunk) => chunk,
        Err(error) => {
            // compile error 時は VM を生成していないので usage は既定の空 snapshot を返す。
            // この経路に乗る fixture は budget 境界 matrix には現状無い（compile error 突合は
            // canonical_error_inventory.rs の管轄）。安全側に空 usage を返す。
            return (BudgetUsage::default(), Err(error));
        }
    };
    let mut vm = Vm::new(chunk);
    if let Limits::Fuel(fuel) = limits {
        vm.set_max_steps(fuel);
    }
    // tree の `run()` は root source を `charge_link` で課金する。VM 単発の `Vm::new().run()` は
    // この経路を通らず source counter が 0 のままになるため、tree と対称に root source を課金して
    // 比較を apples-to-apples にする（harness 側だけの対称化で prod 変更は無い）。
    if vm.charge_link(source.len() as u64, &[]).is_err() {
        return (
            vm.budget_usage(),
            Err(TsumugiError::runtime_with_kind(
                0,
                tsumugi::error::ErrorKind::SourceLimit,
                "harness: charge_link 失敗",
            )),
        );
    }
    let result = vm.run();
    (vm.budget_usage(), result)
}

/// 軸(A): engine 間不変な usage 軸を exact 突合する。
///
/// I/O counter・string/source/import の count 系・per-item peak を両 engine で完全一致させる。
/// fuel（committed/reserved とも）・live/peak heap は軸(B) で別途扱うため、ここでは比較しない。
fn assert_invariant_axes(label: &str, tree: &BudgetUsage, vm: &BudgetUsage) {
    // committed/reserved の fuel 以外の counter を突合する。fuel は軸(B)。
    type CounterAccessor = fn(&tsumugi::budget::BudgetCounters) -> u64;
    let axes: &[(&str, CounterAccessor)] = &[
        ("string_allocations", |c| c.string_allocations),
        ("string_bytes", |c| c.string_bytes),
        ("source_count", |c| c.source_count as u64),
        ("source_bytes", |c| c.source_bytes),
        ("import_count", |c| c.import_count as u64),
        ("import_bytes", |c| c.import_bytes),
        ("input_calls", |c| c.input_calls),
        ("input_bytes", |c| c.input_bytes),
        ("output_calls", |c| c.output_calls),
        ("output_bytes", |c| c.output_bytes),
        ("host_calls", |c| c.host_calls),
        ("host_request_bytes", |c| c.host_request_bytes),
        ("host_response_bytes", |c| c.host_response_bytes),
        ("host_call_bytes", |c| c.host_call_bytes),
    ];
    for (name, get) in axes {
        assert_eq!(
            get(&tree.committed),
            get(&vm.committed),
            "[{label}] committed.{name} が tree/VM で不一致（engine 間不変軸）"
        );
        assert_eq!(
            get(&tree.reserved),
            get(&vm.reserved),
            "[{label}] reserved.{name} が tree/VM で不一致（engine 間不変軸）"
        );
    }
    // per-item peak（single_string_bytes/single_source_bytes/collection_elements）。
    assert_eq!(
        tree.peaks, vm.peaks,
        "[{label}] per-item peak が tree/VM で不一致（engine 間不変軸）\n tree={:#?}\n vm={:#?}",
        tree.peaks, vm.peaks
    );
}

/// 単発 fixture を両 engine で回し、terminal reason と usage を軸別ポリシーで突合する。
fn assert_single(fixture: &SingleFixture) {
    let (tree_usage, tree_result) = run_tree_single(fixture.source, fixture.limits);
    let (vm_usage, vm_result) = run_vm_single(fixture.source, fixture.limits);

    // (A) terminal reason（TsumugiError 全 field）の構造一致。Option<TsumugiError> を一括突合する。
    let tree_err = tree_result.err();
    let vm_err = vm_result.err();
    assert_eq!(
        tree_err, vm_err,
        "[{}] terminal reason（kind/message/line/trace）が tree/VM で不一致",
        fixture.label
    );

    // 期待した成功/失敗の分類と実際が一致するか。
    match fixture.expect {
        ExpectOutcome::Success => assert!(
            tree_err.is_none(),
            "[{}] 成功を期待したが terminal error: {:?}",
            fixture.label,
            tree_err
        ),
        ExpectOutcome::Failure => assert!(
            tree_err.is_some(),
            "[{}] 失敗を期待したが両 engine とも成功",
            fixture.label
        ),
    }

    // (A) engine 間不変な usage 軸を exact 突合する。
    assert_invariant_axes(fixture.label, &tree_usage, &vm_usage);

    // (B) absolute heap は engine 間で非可換。単発では「課金の有無（> 0 か）」の向きが両 engine で
    // 揃うことだけ突合する（絶対値は要求しない。設計 §5.3 A-1）。
    assert_eq!(
        tree_usage.live_heap_bytes > 0,
        vm_usage.live_heap_bytes > 0,
        "[{}] live_heap 課金の有無の向きが tree/VM で不一致 (tree={}, vm={})",
        fixture.label,
        tree_usage.live_heap_bytes,
        vm_usage.live_heap_bytes
    );

    // 注: `committed.fuel` は tree が成功 path で 0 を snapshot する（fuel は入力ごとに reset され
    // committed へ積まない）のに対し VM は実歩数を積むため、**向き・符号ですら engine 間で一致
    // しない**。fuel parity は fuel 境界 fixture（StepLimit terminal の一致）と step env subprocess
    // 行で担保し、ここでは committed.fuel を比較対象から外す（設計 §5.3 A-1 の既知差）。
}

// =============================================================================
// in-process ランナー: REPL 継続（複数入力を 1 台帳で回す）
// =============================================================================

/// engine 間の usage 比較ポリシー（軸別ポリシーの REPL 版）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParityMode {
    /// 各入力完了後に、engine 間不変な軸（I/O counter・string/source/import counts・per-item peak）を
    /// exact 突合し、加えて fuel/live_heap の **入力間増減の向き（符号）** を両 engine で突合する。
    /// 跨入力の usage 累積・rollback/release を見る fixture（source_bytes 累積・index recover・
    /// rollback release）に使う。byte-exact な heap 絶対値一致は設計 §5.3 A-1 により要求しない。
    InvariantAxesAndTrend,
}

/// REPL 継続 fixture の宣言。
struct ReplFixture {
    label: &'static str,
    /// 1 台帳で順に流す入力列。
    inputs: &'static [&'static str],
    /// 入力ごとの usage 比較ポリシー。
    parity: ParityMode,
}

/// tree engine で REPL 入力列を 1 台帳で回し、各入力完了後の `BudgetUsage` を返す。
///
/// `src/main.rs` の tree REPL ループを模す（`run_repl_submission` が内部で link する）。
/// `closure_retain.rs` の `tree_live_heap_per_input` を全 usage snapshot へ一般化したもの。
fn tree_usage_per_input(inputs: &[&str]) -> Vec<BudgetUsage> {
    let mut evaluator = Evaluator::new();
    let mut snapshots = Vec::with_capacity(inputs.len());
    for input in inputs {
        let tokens = Lexer::new(input).tokenize();
        let program = Parser::new(tokens).parse().expect("パースに失敗");
        // エラー入力も REPL は継続する（rollback して次入力へ）。結果は捨て、usage を snapshot
        // することで入力間 rollback/release の効果を観測する。
        let _ = evaluator.run_repl_submission(&program, input.len() as u64);
        snapshots.push(evaluator.budget_usage());
    }
    snapshots
}

/// VM engine で REPL 入力列を 1 台帳で回し、各入力完了後の `BudgetUsage` を返す。
///
/// `src/main.rs` の VM REPL ループを模す: 永続 `Compiler` + `Vm::new_repl()` で、入力ごとに
/// `charge_link`（source 会計）→ `compile_repl_line` → `run_repl_chunk` を回す。
/// `closure_retain.rs` の `vm_live_heap_per_input` を全 usage snapshot へ一般化したもの。
fn vm_usage_per_input(inputs: &[&str]) -> Vec<BudgetUsage> {
    let mut compiler = Compiler::new();
    let mut vm = Vm::new_repl();
    let mut snapshots = Vec::with_capacity(inputs.len());
    for input in inputs {
        let tokens = Lexer::new(input).tokenize();
        let program = Parser::new(tokens).parse().expect("パースに失敗");
        // source 会計は実行可否と独立に行う（charge_link が超過なら以降は skip）。
        if vm.charge_link(input.len() as u64, &[]).is_err() {
            snapshots.push(vm.budget_usage());
            continue;
        }
        match compiler.compile_repl_line(&program) {
            Ok(chunk) => {
                let _ = vm.run_repl_chunk(chunk);
            }
            Err(_) => {
                // compile error も REPL は継続する（次入力へ）。
            }
        }
        snapshots.push(vm.budget_usage());
    }
    snapshots
}

/// 両 engine で入力列を回し、`(tree_snapshots, vm_snapshots)` を返す。
/// `closure_retain.rs` の `both_engines_per_input` を全 usage snapshot へ一般化した harness コア。
fn both_engines_per_input(inputs: &[&str]) -> (Vec<BudgetUsage>, Vec<BudgetUsage>) {
    (tree_usage_per_input(inputs), vm_usage_per_input(inputs))
}

/// REPL 継続 fixture を両 engine で回し、parity ポリシーに従って突合する。
fn assert_repl(fixture: &ReplFixture) {
    let (tree, vm) = both_engines_per_input(fixture.inputs);
    assert_eq!(
        tree.len(),
        vm.len(),
        "[{}] snapshot 本数が tree/VM で不一致",
        fixture.label
    );
    match fixture.parity {
        ParityMode::InvariantAxesAndTrend => {
            // 各入力完了後に (A) engine 間不変な軸を exact 突合する。
            for (i, (t, v)) in tree.iter().zip(vm.iter()).enumerate() {
                assert_invariant_axes(&format!("{} 入力{i}", fixture.label), t, v);
            }
            // (B) live_heap は入力間増減の向き（符号）を両 engine で突合する。
            // committed.fuel は tree が成功 path で 0 を snapshot するため向きも非可換で、ここでは
            // 比較しない（fuel parity は fuel 境界 fixture と step env 行で担保。設計 §5.3 A-1）。
            assert_heap_trend_agrees(fixture.label, &tree, &vm);
        }
    }
}

// =============================================================================
// in-process ランナー: retained-closure 課金（paired retained-vs-dropped）
// =============================================================================
//
// retained-closure 系は「入力間増減の符号が両 engine で一致」だけでは **同方向の回帰**
// （両 engine が同時に retained code を誤って release する / drop しても誰も release しない）を
// 取りこぼす。削除した `closure_retain.rs` は 2 系列（retained vs dropped）を回し各 engine で
// `retained > dropped` という **engine 内の絶対不変量** を assert していた。これを harness にも
// 戻す: paired fixture で (1) 各 engine 内の `retained[cmp] > dropped[cmp]`（不変量 1/3）、
// (2) 各 engine 内の drop 後の release = `post_drop < pre_drop`（不変量 2）、(3) engine 間の
// 符号一致（不変量 4）を同時に固定する。live_heap 絶対値の engine 間 byte-exact 一致は
// 設計 §5.3 A-1 により要求しない（engine 内の大小関係のみ使う）。

/// retained-closure 課金の paired fixture 宣言。
///
/// `retained` と `dropped` は入力列。両列の先頭入力は closure を定義する共通 prefix で、
/// 末尾（または指定遷移）で retained 列は closure を保持し続け、dropped 列は closure を drop する。
struct PairedClosureFixture {
    label: &'static str,
    /// closure を保持し続ける入力列。
    retained: &'static [&'static str],
    /// 同じ prefix で最後に closure を drop する対照入力列。
    dropped: &'static [&'static str],
    /// `retained[cmp] > dropped[cmp]` を比較する入力 index（closure 保持 vs drop が分岐した後）。
    cmp: usize,
    /// drop による release を見る遷移: `pre` 入力後 > `post` 入力後（同一 dropped 列内の release）。
    /// `None` のとき release 遷移の assert を省く（dropped 列が単純再束縛でないなど）。
    release: Option<(usize, usize)>,
}

/// paired retained-vs-dropped fixture を両 engine で回し、engine 内の絶対不変量 +
/// engine 間の符号一致を突合する。
fn assert_paired_closure(fixture: &PairedClosureFixture) {
    let (tree_retained, vm_retained) = both_engines_per_input(fixture.retained);
    let (tree_dropped, vm_dropped) = both_engines_per_input(fixture.dropped);

    let cmp = fixture.cmp;
    let tree_r = tree_retained[cmp].live_heap_bytes;
    let tree_d = tree_dropped[cmp].live_heap_bytes;
    let vm_r = vm_retained[cmp].live_heap_bytes;
    let vm_d = vm_dropped[cmp].live_heap_bytes;

    // 不変量 1/3（engine 内の絶対不変量）: closure を保持する列は、drop する列より入力 cmp 完了後の
    // live_heap が厳密に大きい。これを **各 engine 内で独立に** assert する。両 engine が同方向に
    // 誤って release する回帰（符号一致だけでは通ってしまう）をここで落とす。
    assert!(
        tree_r > tree_d,
        "[{}] tree: 保持 closure の retained code が入力{cmp} 後も課金され続けるはず \
         (retained={tree_r}, dropped={tree_d})",
        fixture.label
    );
    assert!(
        vm_r > vm_d,
        "[{}] VM: 保持 closure の retained code が入力{cmp} 後も課金され続けるはず \
         (retained={vm_r}, dropped={vm_d})",
        fixture.label
    );

    // 不変量 2（engine 内の release 寿命）: drop 列で closure を捨てる入力の後は、retained code が
    // release され live_heap が減る（`post < pre`）。各 engine 内で独立に assert する。drop しても
    // 誰も release しない回帰をここで落とす。
    if let Some((pre, post)) = fixture.release {
        let tree_pre = tree_dropped[pre].live_heap_bytes;
        let tree_post = tree_dropped[post].live_heap_bytes;
        let vm_pre = vm_dropped[pre].live_heap_bytes;
        let vm_post = vm_dropped[post].live_heap_bytes;
        assert!(
            tree_post < tree_pre,
            "[{}] tree: closure を drop した入力後は retained code が release されるはず \
             (pre={tree_pre}, post={tree_post})",
            fixture.label
        );
        assert!(
            vm_post < vm_pre,
            "[{}] VM: closure を drop した入力後は retained code が release されるはず \
             (pre={vm_pre}, post={vm_post})",
            fixture.label
        );
    }

    // 不変量 4（engine 間の符号一致）: `retained[cmp] - dropped[cmp]` の符号が両 engine で揃う。
    // 絶対値は engine 固有（tree=ast_node / VM=bytecode_chunk）で異なってよい（§5.3 A-1）が、
    // 「保持列が drop 列より多い」という向きは両 engine で一致する。片方だけ落ちる非対称を禁じる。
    let tree_delta = tree_r as i128 - tree_d as i128;
    let vm_delta = vm_r as i128 - vm_d as i128;
    assert_eq!(
        tree_delta.signum(),
        vm_delta.signum(),
        "[{}] retained-dropped の符号が tree/VM で不一致 \
         (tree_delta={tree_delta}, vm_delta={vm_delta})",
        fixture.label
    );
}

/// 入力間の live_heap 増減の向き（符号）が両 engine で揃うことを突合する（軸(B)・設計 §5.3 A-1）。
fn assert_heap_trend_agrees(label: &str, tree: &[BudgetUsage], vm: &[BudgetUsage]) {
    for i in 1..tree.len() {
        let tree_delta = tree[i].live_heap_bytes as i128 - tree[i - 1].live_heap_bytes as i128;
        let vm_delta = vm[i].live_heap_bytes as i128 - vm[i - 1].live_heap_bytes as i128;
        assert_eq!(
            tree_delta.signum(),
            vm_delta.signum(),
            "[{label}] 入力{i} での live_heap 増減の向きが tree/VM で不一致 \
             (tree_delta={tree_delta}, vm_delta={vm_delta})"
        );
    }
}

// =============================================================================
// subprocess ランナー: configured per-resource 上限 + observable byte 一致
// =============================================================================

/// 統合テストが使うバイナリ path（`tests/integration.rs:23` と同形）。
fn tsumugi_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsumugi")
}

/// REPL subprocess を起動し stdin へ source を流して `Output` を得る
/// （`tests/integration.rs:1347` の `run_repl_process` を最小複製。cross-crate 共有は
/// 統合テストでは困難なため、本 harness で必要な範囲だけ複製する）。
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

    let mut child = command.spawn().expect("REPL プロセスの起動に失敗");
    let mut stdin = child.stdin.take().expect("REPL stdin の取得に失敗");
    stdin
        .write_all(source.as_bytes())
        .expect("REPL stdin への書き込みに失敗");
    drop(stdin); // EOF を送り REPL を終了させる
    child.wait_with_output().expect("REPL プロセスの待機に失敗")
}

/// subprocess の stdout/stderr を CRLF 正規化して取り出す（`integration.rs` の `output_text` と同形）。
fn output_text(output: &std::process::Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n"),
    )
}

/// REPL のプロンプト（tree/VM で異なる）を除いた可視出力行を取り出す
/// （`integration.rs:1375` の `repl_visible_lines` と同形）。stdout 文字列は tree=`tsumugi> `、
/// VM=`tsumugi:vm> ` のプロンプト差があるため、byte 一致はこの正規化後に突合する。
fn repl_visible_lines(stdout: &str, use_vm: bool) -> Vec<String> {
    let prompt = if use_vm { "tsumugi:vm> " } else { "tsumugi> " };
    stdout
        .lines()
        .filter_map(|line| {
            line.rsplit_once(prompt)
                .map(|(_, value)| value.to_string())
                .filter(|value| !value.is_empty())
        })
        .collect()
}

/// subprocess 層の budget 境界 fixture の宣言。
struct SubprocessFixture {
    label: &'static str,
    /// REPL stdin へ流す source。
    source: &'static str,
    /// per-resource 上限を注入する env。
    limits: EnvLimits,
    /// 両 engine で成功するか（exact-limit 行）/ 両 engine で同一の超過メッセージが出るか（+1 行）。
    expect: SubprocessExpect,
}

/// subprocess fixture の期待結果。
enum SubprocessExpect {
    /// 両 engine が成功し、可視出力行が byte 一致する。
    Success,
    /// 両 engine が（exit は成功しつつ）stderr に指定部分文字列を含む超過を出す。REPL は 1 入力の
    /// budget 超過でプロセス自体は正常終了する（`integration.rs` の既存 budget 群と同じ観測）。
    Exceeded { stderr_contains: &'static str },
}

/// subprocess fixture を両 engine で回し、observable（stdout 可視行 / stderr 超過）を突合する。
fn assert_subprocess(fixture: &SubprocessFixture) {
    let mut tree_lines: Option<Vec<String>> = None;
    for use_vm in [false, true] {
        let output = run_repl_process(fixture.source, use_vm, &fixture.limits.envs);
        let (stdout, stderr) = output_text(&output);
        let mode = if use_vm { "VM" } else { "tree" };

        assert!(
            output.status.success(),
            "[{}] {mode} REPL が異常終了: {stderr}",
            fixture.label
        );

        match fixture.expect {
            SubprocessExpect::Success => {
                let lines = repl_visible_lines(&stdout, use_vm);
                match &tree_lines {
                    None => tree_lines = Some(lines),
                    Some(expected) => assert_eq!(
                        expected, &lines,
                        "[{}] 可視出力行が tree/VM で byte 不一致",
                        fixture.label
                    ),
                }
            }
            SubprocessExpect::Exceeded { stderr_contains } => {
                assert!(
                    stderr.contains(stderr_contains),
                    "[{}] {mode} で期待した budget 超過メッセージ `{stderr_contains}` が無い: {stderr}",
                    fixture.label
                );
            }
        }
    }
}

// =============================================================================
// 疎通 fixture（harness の土台確認）
// =============================================================================

/// 既定上限で成功する単発 fixture。harness の in-process 単発経路が tree/VM 一致で緑であることを
/// 確認する疎通テスト（matrix の基礎疎通）。
#[test]
fn smoke_default_single_fixture_agrees() {
    assert_single(&SingleFixture {
        label: "疎通: 算術と print（既定上限で成功）",
        source: "let a = 1 + 2\nlet b = a * 3\nprint(b)\n",
        limits: Limits::Default,
        expect: ExpectOutcome::Success,
    });
}

/// 疎通（失敗側）: 非 fuel の runtime error（未定義名）は両 engine が **terminal reason 全 field
/// （kind/message/line/trace）exact 一致**で terminal になる。fuel 枯渇と違い line も engine 間
/// 不変であることを固定する（canonical_error_inventory.rs と同じ契約を harness で押さえる）。
#[test]
fn smoke_default_single_failure_agrees() {
    assert_single(&SingleFixture {
        label: "疎通: 未定義名で両 engine 同一 terminal",
        source: "let a = 1\nprint(missing)\n",
        limits: Limits::Default,
        expect: ExpectOutcome::Failure,
    });
}

// =============================================================================
// matrix: budget 境界（exact-limit 成功 / +1 で BudgetExceeded）
// =============================================================================
//
// 注入経路は Limits 制約に従う:
// - fuel は in-process `Fuel(n)` で両 engine を数値突合（集約元: integration.rs:1622
//   step_limit_env_still_gates_both_engines_via_budget_ledger の in-process 格上げ）。
// - collection/string/source/I-O/heap は VM に in-process の per-resource 注入 API が無いため
//   subprocess 層 `TSUMUGI_MAX_*` で宣言する。各行の doc に集約元 integration.rs 行を明記する。

/// fuel 境界（exact-limit 成功）: in-process で fuel 上限を注入し、十分な上限なら両 engine が
/// 成功して engine 間不変軸が一致する。集約元: integration.rs:1622。
///
/// +1 失敗（StepLimit terminal）は **subprocess の step env 行**（`budget_boundary_step_env_subprocess`）
/// で担保する。fuel 枯渇の terminal `line` は tree/VM で異なる（tree=2 / VM=3 等、step 粒度差に
/// よる既知差）ため、in-process で terminal 全 field exact を要求せず、subprocess で kind/message
/// の observable 一致を見る。これは軸(B)（fuel は engine 間非可換）と整合する線引き。
#[test]
fn budget_boundary_fuel_exact_limit_in_process() {
    // 既定上限で成功する基準（fuel を十分大きく取り、両 engine が成功 + 不変軸一致）。
    assert_single(&SingleFixture {
        label: "fuel: 十分な上限で成功",
        source: "let i = 0\nwhile i < 10\n    i = i + 1\nend\nprint(i)\n",
        limits: Limits::Fuel(100_000),
        expect: ExpectOutcome::Success,
    });
}

/// collection 要素数境界（集約元: integration.rs:1596
/// collection_limit_is_consistent_in_both_engines）。
#[test]
fn budget_boundary_collection_subprocess() {
    // exact-limit: 上限 3 で 3 要素 literal は両 engine で成功し、同じ出力。
    assert_subprocess(&SubprocessFixture {
        label: "collection: 上限ちょうど(3)で成功",
        source: "print([1, 2, 3])\n",
        limits: EnvLimits {
            envs: vec![("TSUMUGI_MAX_COLLECTION_SIZE", "3")],
        },
        expect: SubprocessExpect::Success,
    });
    // +1: 上限 2 で 3 要素 literal は両 engine で CollectionLimit 超過。
    assert_subprocess(&SubprocessFixture {
        label: "collection: 上限+1(2<3)で両 engine 超過",
        source: "print([1, 2, 3])\n",
        limits: EnvLimits {
            envs: vec![("TSUMUGI_MAX_COLLECTION_SIZE", "2")],
        },
        expect: SubprocessExpect::Exceeded {
            stderr_contains: "コレクション要素数が上限を超えました",
        },
    });
}

/// step 上限 env 境界（集約元: integration.rs:1622）。subprocess 層でも env が両 engine へ
/// 対称に効くことを固定する（in-process `Fuel` 行と相補的: env 注入経路の parity）。
#[test]
fn budget_boundary_step_env_subprocess() {
    assert_subprocess(&SubprocessFixture {
        label: "step: TSUMUGI_MAX_STEPS=50 で両 engine 超過",
        source: "let i = 0\nwhile i < 1000000\n    i = i + 1\nend\nprint(i)\n",
        limits: EnvLimits {
            envs: vec![("TSUMUGI_MAX_STEPS", "50")],
        },
        expect: SubprocessExpect::Exceeded {
            stderr_contains: "ステップ上限に達しました",
        },
    });
}

/// heap 累積境界（集約元: integration.rs:2918
/// heap_accumulation_trips_limit_in_both_engines）。live_heap 上限を跨入力で積み上げ、
/// 両 engine で HeapLimit 超過に至る。
#[test]
fn budget_boundary_heap_subprocess() {
    assert_subprocess(&SubprocessFixture {
        label: "heap: live_heap 上限を跨入力で超過",
        source: "let a = \"xxxxxxxxxxxxxxxx\"\n\
                 let b = a + a\n\
                 let c = b + b\n\
                 let d = c + c\n\
                 let e = d + d\n",
        limits: EnvLimits {
            // 小さな live heap 上限。文字列連結で積み上げると超過する。
            envs: vec![("TSUMUGI_MAX_LIVE_HEAP_BYTES", "256")],
        },
        expect: SubprocessExpect::Exceeded {
            stderr_contains: "ヒープ",
        },
    });
}

/// output_calls 境界（集約元: integration.rs:2031 output_calls_limit_*）。
#[test]
fn budget_boundary_output_calls_subprocess() {
    assert_subprocess(&SubprocessFixture {
        label: "output_calls: 上限超過で両 engine 停止",
        source: "print(1)\nprint(2)\nprint(3)\n",
        limits: EnvLimits {
            envs: vec![("TSUMUGI_MAX_OUTPUT_CALLS", "2")],
        },
        expect: SubprocessExpect::Exceeded {
            stderr_contains: "上限",
        },
    });
}

// =============================================================================
// matrix: REPL 継続（複数入力を 1 台帳で回し入力間 usage を突合）
// =============================================================================

/// 跨入力の source_bytes 累積（集約元: integration.rs:2011
/// source_bytes_cumulative_limit_gates_across_inputs_in_both_engines の in-process 数値格上げ）。
/// 複数入力を 1 台帳で回し、各入力後の usage が両 engine で全一致することを突合する。
#[test]
fn repl_continuation_cumulative_usage_agrees() {
    assert_repl(&ReplFixture {
        label: "REPL: 跨入力の usage 累積一致",
        inputs: &[
            "let x = 1\n",
            "let y = x + 2\n",
            "print(x + y)\n",
            "let z = [1, 2, 3]\n",
        ],
        parity: ParityMode::InvariantAxesAndTrend,
    });
}

/// index 代入の recover + 次入力での書き込み継続（集約元: integration.rs:2094
/// index_assign_recovers_and_writes_across_inputs_in_both_engines）。
/// 失敗入力後に language-state が巻き戻り、次入力の書き込みが継続することを usage で突合する。
#[test]
fn repl_continuation_index_assign_recover_agrees() {
    assert_repl(&ReplFixture {
        label: "REPL: index 代入 recover 後の継続",
        inputs: &[
            "let xs = [10, 20, 30]\n",
            "xs[9] = 1\n",  // 範囲外: エラーで rollback
            "xs[0] = 99\n", // 次入力で正常書き込み
            "print(xs[0])\n",
        ],
        parity: ParityMode::InvariantAxesAndTrend,
    });
}

/// エラー入力後の rollback/release で live_heap が回復する（集約元: integration.rs:2957
/// heap_released_after_repl_rollback_in_both_engines / :2879 reassignment release）。
/// エラー入力の前後で live_heap が巻き戻る向きが両 engine で一致することを突合する。
#[test]
fn repl_continuation_rollback_release_agrees() {
    assert_repl(&ReplFixture {
        label: "REPL: エラー入力後の rollback release",
        inputs: &[
            "let base = [1, 2, 3]\n",
            "let big = [1, 2, 3, 4, 5]\nundefined_name\n", // 大きな確保のあとエラー → rollback
            "print(len(base))\n",
        ],
        parity: ParityMode::InvariantAxesAndTrend,
    });
}

/// retained closure code の課金持続/解放（集約元: closure_retain.rs の 4 不変量）。
///
/// closure が retain する code（tree=`FnDef.body`、VM=prototype `Rc<Chunk>`）は、engine 固有の
/// 表現差で live_heap/peak の絶対値が異なってよい（§5.3 A-1）。よって byte-exact な engine 間
/// 数値一致は求めず、**engine 内の大小関係（絶対不変量）+ engine 間の符号一致**で突合する。
///
/// 単純な「入力間増減の符号が両 engine で一致」だけだと、両 engine が同方向に誤って release する
/// 回帰（例: 無関係入力で retained code を落とす / drop しても release しない）が符号一致のまま
/// 通ってしまう。そこで paired fixture（retained 列 vs dropped 列）で各 engine 内の絶対不変量を
/// assert する。削除した `closure_retain.rs` の以下 4 不変量を移植する。
///
/// - 不変量1: closure を保持したまま無関係入力を回しても retained code は課金され続ける（保持列 > drop 列、各 engine 内で独立に）。
/// - 不変量2: closure を drop する入力の後は retained code が release される（post < pre、各 engine 内）。
/// - 不変量3: （ネスト版・下の test）跨入力生成した内側 closure も保持列 > drop 列。
/// - 不変量4: 保持列 - drop 列の符号が両 engine で一致する（片方だけ落ちる非対称を禁じる）。
#[test]
fn repl_continuation_retained_closure_charge_agrees() {
    // retained 列: 入力1 で closure 定義 → 入力2 で無関係入力（closure 保持）。
    // dropped 列: 入力1 で同じ closure 定義 → 入力2 で closure を drop（Int 再束縛）。
    // 入力2 完了後（index 1）で retained[1] > dropped[1] を各 engine で assert（不変量 1）。
    // dropped 列単体では input1(保持) > input2(drop 後) で release を assert（不変量 2）。
    assert_paired_closure(&PairedClosureFixture {
        label: "REPL: retained closure の課金持続と解放",
        retained: &["let f = fn(x) x + 1 end\n", "let y = 1\n"],
        dropped: &["let f = fn(x) x + 1 end\n", "f = 0\n"],
        cmp: 1,
        release: Some((0, 1)),
    });
}

/// ネスト closure の跨入力生成（集約元: closure_retain.rs:148
/// nested_closure_cross_input_creation_stays_charged）。
/// 外側を入力1 で定義 → 入力2 で呼んで内側を生成し保持 → 内側 retained code の課金持続。
#[test]
fn repl_continuation_nested_closure_charge_agrees() {
    // retained 列: 入力3 で無関係入力（内側 closure を保持）。
    // dropped 列: 入力3 で g を drop（内側 closure を落とす）。
    // 入力3 完了後（index 2）で retained[2] > dropped[2] を各 engine で assert（不変量 3）。
    // dropped 列単体では input2(内側保持) > input3(drop 後) で release を assert（不変量 2）。
    assert_paired_closure(&PairedClosureFixture {
        label: "REPL: ネスト closure 跨入力生成の課金",
        retained: &[
            "fn outer()\n  return fn(x) return x + 1 end\nend\n",
            "let g = outer()\n", // 内側生成 + 保持
            "let z = 2\n",       // 無関係入力: 内側 closure 保持
        ],
        dropped: &[
            "fn outer()\n  return fn(x) return x + 1 end\nend\n",
            "let g = outer()\n", // 内側生成 + 保持
            "g = 0\n",           // 内側 closure を drop
        ],
        cmp: 2,
        release: Some((1, 2)),
    });
}
