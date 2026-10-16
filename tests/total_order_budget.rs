//! REV-017 §17.8.9: sort の全順序化による budget / fuel 消費差を同一 backend で固定する。
//!
//! tree/VM 間の fuel は byte-exact 一致しない（`differential.rs` 不変量）ため、消費差は
//! **同一 backend（embedding の tree engine）で revision 0.20 と 0.21 を比較**して固定する。
//! 0.21 は描画 byte 課金（`render_display`）が sort 経路から外れるぶん fuel 消費が 0.20 より
//! 減る（§17.8.5 / §17.8.7 の観測差。charge table の値は不変）。あわせて per-element 走査課金が
//! 残っていること（要素数に対して fuel が単調増加）を間接的に固定する。

use std::sync::Arc;

use tsumugi::{
    Backend, BudgetConfig, CompileOptions, EmbeddingContext, EmbeddingEngine, EmbeddingOutcome,
    EmbeddingRequest, EngineConfig, ExecutionId, FakeClock, LanguageRevision, LinkRequest,
    MonotonicClock, Source, SourceId,
};

/// 指定 revision の tree engine で `source` を実行し、committed fuel を返す。
fn run_committed_fuel(revision: LanguageRevision, id: &str, source: &'static str) -> u64 {
    let config = EngineConfig {
        backend: Backend::TreeWalk,
        language_revision: revision,
        ..EngineConfig::default()
    };
    let engine = EmbeddingEngine::builder()
        .config(config)
        .build()
        .expect("build");

    let script = engine
        .compile(
            Source::new(SourceId::new(id).expect("valid id"), source),
            &CompileOptions {
                retain_source: true,
            },
        )
        .expect("compile");

    let link_clock = FakeClock::new();
    let link_budget = BudgetConfig::standard(&link_clock).expect("standard budget");
    let link_request = LinkRequest::new(
        ExecutionId::new(1u128.try_into().expect("nonzero")),
        tsumugi::CapabilitySet::empty(),
        link_budget,
    );
    let linked = engine.link(&script, link_request).expect("link");

    let mut ctx = EmbeddingContext::new(&engine);
    let run_clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
    let run_budget = BudgetConfig::standard(run_clock.as_ref()).expect("standard budget");
    let request = EmbeddingRequest::new(run_budget, run_clock).expect("同 domain");

    match engine.run(&linked, &mut ctx, request) {
        EmbeddingOutcome::Completed { usage } => usage.committed.fuel,
        other => panic!("Completed のはず: {other:?}"),
    }
}

/// sort の budget / fuel 消費は revision で決定的に変わる（§17.8.7 / §17.8.9 の観測差）。
///
/// 0.21 は値どうしを全順序 TotalOrder で直接比較し、文字列描画（`render_display`）の byte-payload
/// 走査を sort 経路から外す一方、charge table の per-element 走査課金（1 / element、値は不変）を
/// 比較走査へ適用する。0.20 は要素を render_display で描画するキー生成で、複合ノードごとに
/// per-node fuel を課金する。両経路の課金構造が異なるため、同一入力でも committed fuel が
/// revision で変わる（revision 境界で説明される観測差。manifesto 原則5・原則7）。
///
/// 消費差の「向き」は入力の形（スカラ列か複合要素か）で変わり、普遍的に 0.20 > 0.21 とは限ら
/// ないため、ここでは「revision で消費が決定的に異なること」を固定する（§17.8.9 の消費差ピン）。
/// 各 revision 内では同一入力の committed fuel は決定的である。
#[test]
fn sort_fuel_differs_between_0_20_and_0_21() {
    let source =
        "let xs = [[3, [1, 2]], [1, [2, 3]], [2, [3, 1]], [1, [1, 1]]]\nlet sorted = sort(xs)\n";
    let fuel_020 = run_committed_fuel(LanguageRevision::V0_20, "fuel_020", source);
    let fuel_021 = run_committed_fuel(LanguageRevision::V0_21, "fuel_021", source);
    assert_ne!(
        fuel_020, fuel_021,
        "sort の全順序化は revision 境界で budget / fuel 消費差を生むはず（0.20={fuel_020}, 0.21={fuel_021}）"
    );
    // 各 revision 内では決定的（同一入力の再実行で同じ値）。
    let fuel_021_again = run_committed_fuel(LanguageRevision::V0_21, "fuel_021b", source);
    assert_eq!(fuel_021, fuel_021_again, "0.21 の消費は決定的");
}

/// 0.21 でも per-element 走査課金（1 / element）が残っていること。要素数の多い入力の方が
/// sort 経路の fuel を多く積むことで単調増加を間接的に固定する。
#[test]
fn sort_fuel_monotonic_in_element_count_at_0_21() {
    let small = "let xs = [3, 1, 2]\nlet sorted = sort(xs)\n";
    let large =
        "let xs = [3, 1, 2, 9, 8, 7, 6, 5, 4, 10, 11, 12, 13, 14, 15]\nlet sorted = sort(xs)\n";
    let fuel_small = run_committed_fuel(LanguageRevision::V0_21, "mono_small", small);
    let fuel_large = run_committed_fuel(LanguageRevision::V0_21, "mono_large", large);
    assert!(
        fuel_large > fuel_small,
        "要素数の多い入力 ({fuel_large}) は少ない入力 ({fuel_small}) より fuel を多く消費するはず"
    );
}

/// revision による順序差（0.21 は数値順、0.20 は文字列順）が Completed で観測できることを、
/// 消費差テストと地続きに確認する（順序そのものの網羅は golden fixture total_order_sort）。
#[test]
fn both_revisions_complete_sort() {
    let source = "let sorted = sort([10, 2, 1])\n";
    // どちらの revision でも実行が Completed に到達する（型エラーを増やさない、§17.8.6）。
    let _ = run_committed_fuel(LanguageRevision::V0_20, "complete_020", source);
    let _ = run_committed_fuel(LanguageRevision::V0_21, "complete_021", source);
}
