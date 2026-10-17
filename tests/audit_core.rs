//! Phase 6 実行時監査（audit）A-1 スライスの end-to-end 統合テスト。
//!
//! 設計正本は `docs/determinism-and-audit.md` §7/§8/§10/§11。tree engine のみ。
//! crate root から re-export される公開型だけを使い、監査の opt-in・emission 順序・
//! outcome 写像・fail-closed を固定する。

use std::sync::Arc;

use tsumugi::{
    AuditEvent, AuditSink, AuditedOutcome, BudgetConfig, CompileOptions, EmbeddingContext,
    EmbeddingEngine, EmbeddingOutcome, EmbeddingRequest, ExecutionId, FakeClock, InMemoryAuditSink,
    LinkRequest, MonotonicClock, ScriptedAuditSink, ScriptedResponse, Source, SourceId,
    TerminalOutcome,
};

/// 決定的テスト用の standard budget + 共有 clock から request を作る（監査 execution_id 付き）。
fn request_with_id(id: u128) -> EmbeddingRequest {
    let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
    let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
    EmbeddingRequest::new(budget, clock)
        .expect("standard budget は自身を生成した clock と同 domain")
        .with_execution_id(ExecutionId::new(id.try_into().expect("nonzero")))
}

/// execution_id を載せない request（no-sink parity の確認用）。
fn request_without_id() -> EmbeddingRequest {
    let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
    let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
    EmbeddingRequest::new(budget, clock).expect("standard budget は同 domain")
}

/// import なし link 用の既定 [`LinkRequest`]（C6-c の 3 引数 `new`）。
///
/// 本ファイルの script は import を持たないため、empty capabilities（resolver 未 grant）+ standard
/// budget で足りる（import 0 件は resolver を呼ばず空 graph を返す）。
fn link_request() -> LinkRequest {
    let clock = FakeClock::new();
    let budget = BudgetConfig::standard(&clock).expect("standard budget");
    LinkRequest::new(
        ExecutionId::new(1u128.try_into().expect("nonzero")),
        tsumugi::CapabilitySet::empty(),
        budget,
    )
}

fn source(id: &str, text: &'static str) -> Source<'static> {
    Source::new(SourceId::new(id).expect("valid source id"), text)
}

/// retain=true で compile → link し、runnable な linked script を返す。
fn link_source(
    engine: &EmbeddingEngine,
    id: &str,
    text: &'static str,
) -> tsumugi::EmbeddingCompiledScript {
    engine
        .compile(
            source(id, text),
            &CompileOptions {
                retain_source: true,
            },
        )
        .expect("compile")
}

// --- (c) opt-in no-sink parity ---

#[test]
fn no_sink_means_no_emission_and_unchanged_outcome() {
    // sink を設定しない engine。既存挙動と bit-identical（監査なし）。
    let engine = EmbeddingEngine::builder().build().expect("build");
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    // 別の in-memory sink を作っても、engine に設定していないので触られない。
    let observer = InMemoryAuditSink::new();

    let outcome = engine.run(&linked, &mut ctx, request_with_id(1));
    assert!(matches!(outcome, EmbeddingOutcome::Completed { .. }));
    // sink は engine に未設定なので何も記録されない。
    assert!(observer.is_empty());
}

#[test]
fn no_sink_runtime_error_outcome_is_unchanged() {
    let engine = EmbeddingEngine::builder().build().expect("build");
    let script = link_source(&engine, "m", "let x = undefined_name\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let outcome = engine.run(&linked, &mut ctx, request_without_id());
    assert!(matches!(outcome, EmbeddingOutcome::RuntimeError { .. }));
}

// --- (d) happy-path emission ---

#[test]
fn with_sink_emits_started_then_terminal_in_order() {
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let execution_id = ExecutionId::new(42u128.try_into().unwrap());
    let request = {
        let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
        let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
        EmbeddingRequest::new(budget, clock)
            .expect("same domain")
            .with_execution_id(execution_id)
    };

    let audited = engine.run_audited(&linked, &mut ctx, request);
    match audited.outcome() {
        Some(EmbeddingOutcome::Completed { .. }) => {}
        other => panic!("期待 Completed, 実際 {other:?}"),
    }

    let envelopes = sink.snapshot();
    // fuel 等を charge する script なので、Started と Terminal の間に BudgetCharged が挟まる。
    // 件数は固定しない: 不変条件（先頭 Started / 末尾 Terminal / 中間はすべて BudgetCharged /
    // sequence が 0 始まり gap なし）で検証する（§7.3 / §8）。
    assert!(
        envelopes.len() >= 3,
        "Started + BudgetCharged(>=1) + Terminal のはず（実際 {}）",
        envelopes.len()
    );
    assert_journal_invariants(&envelopes, execution_id);

    // Started は sequence 0 に 1 件。
    assert_eq!(envelopes[0].sequence, 0);
    assert_eq!(envelopes[0].schema_version, 1);
    assert_eq!(envelopes[0].execution_id, execution_id);
    assert_eq!(envelopes[0].source_hash, *script.source_hash().as_bytes());
    assert_eq!(envelopes[0].language_revision, "0.21");
    assert!(matches!(
        envelopes[0].event,
        AuditEvent::ExecutionStarted { .. }
    ));

    // 中間はすべて BudgetCharged（§8: Started と Terminal の間に挟まる）。
    for env in &envelopes[1..envelopes.len() - 1] {
        assert!(
            matches!(env.event, AuditEvent::BudgetCharged { .. }),
            "中間 event は BudgetCharged のはず: {:?}",
            env.event
        );
    }

    // Terminal は最後の 1 件・最大 sequence。
    let last = envelopes.last().expect("non-empty");
    assert_eq!(last.sequence as usize, envelopes.len() - 1);
    assert_eq!(last.execution_id, execution_id);
    match &last.event {
        AuditEvent::Terminal {
            outcome,
            context_committed,
            host_effects_may_remain,
            ..
        } => {
            assert_eq!(*outcome, TerminalOutcome::Completed);
            assert!(*context_committed, "Completed は commit する");
            assert!(!*host_effects_may_remain);
        }
        other => panic!("期待 Terminal, 実際 {other:?}"),
    }
}

/// journal 不変条件を検証する共有ヘルパ（§8）。
///
/// - 先頭は `ExecutionStarted`（sequence 0）、末尾は唯一の `Terminal`。
/// - sequence は 0 始まり・gap/重複なし・昇順。
/// - 全 envelope が同じ execution_id。
/// - Terminal は 1 件だけ。
fn assert_journal_invariants(envelopes: &[tsumugi::AuditEnvelope], execution_id: ExecutionId) {
    assert!(!envelopes.is_empty(), "少なくとも Started/Terminal がある");
    // 先頭 Started（seq 0）。
    assert_eq!(envelopes[0].sequence, 0);
    assert!(matches!(
        envelopes[0].event,
        AuditEvent::ExecutionStarted { .. }
    ));
    // sequence 連続性 + execution_id 一致。
    for (i, env) in envelopes.iter().enumerate() {
        assert_eq!(env.sequence as usize, i, "sequence は 0 始まり gap なし");
        assert_eq!(env.execution_id, execution_id);
    }
    // Terminal はちょうど 1 件で末尾。
    let terminal_count = envelopes
        .iter()
        .filter(|e| matches!(e.event, AuditEvent::Terminal { .. }))
        .count();
    assert_eq!(terminal_count, 1, "Terminal は 1 件だけ");
    assert!(
        matches!(
            envelopes.last().expect("non-empty").event,
            AuditEvent::Terminal { .. }
        ),
        "Terminal は末尾"
    );
}

#[test]
fn with_sink_runtime_error_maps_to_runtime_error_terminal() {
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = undefined_name\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(7));
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::RuntimeError { .. })
    ));

    let envelopes = sink.snapshot();
    // link/run で source/fuel を charge するため、Started と Terminal の間に BudgetCharged が入る。
    assert_journal_invariants(&envelopes, ExecutionId::new(7u128.try_into().unwrap()));
    let last = envelopes.last().expect("non-empty");
    match &last.event {
        AuditEvent::Terminal {
            outcome,
            error,
            context_committed,
            ..
        } => {
            assert_eq!(*outcome, TerminalOutcome::RuntimeError);
            assert!(!*context_committed, "RuntimeError は rollback する");
            assert!(error.is_some(), "error terminal は payload を持つ");
        }
        other => panic!("期待 Terminal, 実際 {other:?}"),
    }
}

#[test]
fn with_sink_pre_cancel_has_started_and_cancelled_terminal() {
    // pre-cancel（命令0）でも Started + Terminal(Cancelled) を持つ（§8）。
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = 1\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let token = tsumugi::CancellationToken::new();
    token.cancel();
    let request = {
        let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
        let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
        EmbeddingRequest::new(budget, clock)
            .expect("same domain")
            .with_execution_id(ExecutionId::new(99u128.try_into().unwrap()))
            .cancellation(token)
    };

    let audited = engine.run_audited(&linked, &mut ctx, request);
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::Cancelled { .. })
    ));
    let envelopes = sink.snapshot();
    assert_eq!(envelopes.len(), 2);
    assert!(matches!(
        envelopes[0].event,
        AuditEvent::ExecutionStarted { .. }
    ));
    match &envelopes[1].event {
        AuditEvent::Terminal { outcome, .. } => {
            assert_eq!(*outcome, TerminalOutcome::Cancelled);
        }
        other => panic!("期待 Terminal(Cancelled), 実際 {other:?}"),
    }
}

// --- §7.3 BudgetCharged end-to-end ---

#[test]
fn budget_charged_emitted_between_started_and_terminal() {
    // fuel を複数回 charge する script。BudgetCharged が Started(seq 0) と Terminal の間に出て、
    // 同 resource+reason は 1 件へ集約され charge_count > 1 になる（§7.3）。
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    // while ループは反復ごとに fuel を charge するため、同 (Fuel, Operation) が複数回集約される。
    let script = link_source(&engine, "m", "let i = 0\nwhile i < 5\n    i = i + 1\nend\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let execution_id = ExecutionId::new(50u128.try_into().unwrap());
    let request = {
        let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
        let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
        EmbeddingRequest::new(budget, clock)
            .expect("same domain")
            .with_execution_id(execution_id)
    };

    let audited = engine.run_audited(&linked, &mut ctx, request);
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::Completed { .. })
    ));

    let envelopes = sink.snapshot();
    assert_journal_invariants(&envelopes, execution_id);

    // 中間はすべて BudgetCharged（§8）。最後の BudgetCharged は Terminal の直前にある
    // （§8 規則 7: Terminal 直前 flush）。
    let charged: Vec<&AuditEvent> = envelopes[1..envelopes.len() - 1]
        .iter()
        .map(|e| &e.event)
        .collect();
    assert!(
        !charged.is_empty(),
        "少なくとも 1 件の BudgetCharged が出るはず"
    );
    for event in &charged {
        assert!(
            matches!(event, AuditEvent::BudgetCharged { .. }),
            "中間 event は BudgetCharged: {event:?}"
        );
    }
    // 最後の BudgetCharged は Terminal の直前 sequence に来る（取りこぼしなし）。
    let last_charged_idx = envelopes.len() - 2;
    assert!(matches!(
        envelopes[last_charged_idx].event,
        AuditEvent::BudgetCharged { .. }
    ));

    // 集約: Fuel+Operation が複数回 charge されると 1 件に畳まれ charge_count > 1。
    let fuel_charged: Vec<&AuditEvent> = charged
        .iter()
        .filter(|e| {
            matches!(
                e,
                AuditEvent::BudgetCharged {
                    resource: tsumugi::BudgetResource::Fuel,
                    reason: tsumugi::BudgetChargeReason::Operation,
                    ..
                }
            )
        })
        .copied()
        .collect();
    assert_eq!(
        fuel_charged.len(),
        1,
        "同 (Fuel, Operation) は 1 件へ集約されるはず（flush は 1 回）"
    );
    if let AuditEvent::BudgetCharged { charge_count, .. } = fuel_charged[0] {
        assert!(
            *charge_count > 1,
            "複数文の fuel charge は charge_count > 1 に集約されるはず（実際 {charge_count}）"
        );
    }

    // reconciliation: 全 BudgetCharged の committed_delta を resource ごとに合算した値が、
    // Terminal の usage（対応する BudgetCounters フィールド）と一致する（§7.3）。
    let terminal_usage = match &envelopes.last().expect("non-empty").event {
        AuditEvent::Terminal { usage, .. } => *usage,
        other => panic!("期待 Terminal, 実際 {other:?}"),
    };
    let fuel_committed: u64 = charged
        .iter()
        .filter_map(|e| match e {
            AuditEvent::BudgetCharged {
                resource: tsumugi::BudgetResource::Fuel,
                committed_delta,
                ..
            } => Some(*committed_delta),
            _ => None,
        })
        .sum();
    assert_eq!(
        fuel_committed, terminal_usage.committed.fuel,
        "Fuel の committed_delta 総和は Terminal usage の committed.fuel と一致するはず（§7.3）"
    );
    // usage_after は flush 時点（＝終了時点）の累積値なので、Fuel の最終 usage_after も committed.fuel。
    if let Some(AuditEvent::BudgetCharged { usage_after, .. }) = fuel_charged.first().copied() {
        assert_eq!(
            *usage_after, terminal_usage.committed.fuel,
            "Fuel の usage_after は終了時点の committed.fuel に一致するはず"
        );
    }

    // per-item peak の reconciliation: HeapBytes の peak_after が Terminal usage の
    // peak_heap_bytes（live heap の peak）と一致する（§7.3）。
    let heap_peaks: Vec<u64> = charged
        .iter()
        .filter_map(|e| match e {
            AuditEvent::BudgetCharged {
                resource: tsumugi::BudgetResource::HeapBytes,
                peak_after: Some(peak),
                ..
            } => Some(*peak),
            _ => None,
        })
        .collect();
    if let Some(max_peak) = heap_peaks.iter().copied().max() {
        assert_eq!(
            max_peak, terminal_usage.peak_heap_bytes,
            "HeapBytes の peak_after は Terminal usage の peak_heap_bytes と一致するはず（§7.3）"
        );
    }
}

#[test]
fn budget_charged_no_sink_emits_nothing_and_unchanged_outcome() {
    // no-sink: 同じ charge する script を sink なしで走らせても emission ゼロ・outcome 不変。
    let engine = EmbeddingEngine::builder().build().expect("build");
    let script = link_source(&engine, "m", "let i = 0\nwhile i < 5\n    i = i + 1\nend\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let observer = InMemoryAuditSink::new();
    let outcome = engine.run(&linked, &mut ctx, request_with_id(51));
    assert!(matches!(outcome, EmbeddingOutcome::Completed { .. }));
    // sink は未設定なので何も記録されない（opt-in、byte-for-byte 不変）。
    assert!(observer.is_empty());
}

// --- (e) fail-closed ---

#[test]
fn failing_sink_is_fail_closed_and_not_success() {
    // Started の submit で Failed を返す sink。script work は走らず、結果は success でない。
    let sink = Arc::new(InMemoryAuditSink::failing());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    // completion するはずの script。sink が Failed なら実行結果は success にならない。
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(5));

    // fail-closed: AuditFailed を返し、success な outcome ではない。
    assert!(!audited.is_audited_ok(), "fail-closed なら監査成立ではない");
    match audited {
        AuditedOutcome::AuditFailed { withheld, .. } => {
            // script は 1 命令も走っていないので「保留 terminal」は無く、監査失敗自体を載せる。
            assert_eq!(withheld, TerminalOutcome::AuditFailure);
        }
        other => panic!("fail-closed のはずが: {other:?}"),
    }
    // failing sink は何も記録しない（submit が常に Failed）。
    assert!(sink.is_empty());
}

#[test]
fn terminal_submit_failure_is_fail_closed_and_not_success() {
    // Started は Ack、Terminal で Failed を返す sink。script は走り切るが、Terminal の配送に
    // 失敗するので監査は成立せず、結果は success ではない（§10.1、二度目失敗の fail-closed）。
    let sink = Arc::new(ScriptedAuditSink::new([
        ScriptedResponse::AckCorrect, // Started
        ScriptedResponse::Fail,       // Terminal
    ]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(11));

    assert!(
        !audited.is_audited_ok(),
        "Terminal 配送失敗なら監査成立ではない"
    );
    match audited {
        AuditedOutcome::AuditFailed { withheld, .. } => {
            // Terminal 配送失敗時は、本来報告されたはずの terminal（Completed）を withheld に残す。
            assert_eq!(withheld, TerminalOutcome::Completed);
        }
        other => panic!("Terminal fail-closed のはずが: {other:?}"),
    }
    // Started は記録済み（Ack 時に記録）、Terminal は Fail なので記録されない。
    let envelopes = sink.snapshot();
    assert_eq!(envelopes.len(), 1, "記録は Started のみ");
    assert!(matches!(
        envelopes[0].event,
        AuditEvent::ExecutionStarted { .. }
    ));
}

#[test]
fn terminal_submit_failure_leaves_committed_state_documented_a1_behavior() {
    // §14 A-1 逸脱の明文化を pin する: tree engine は Completed の language-state を
    // 評価器内（Terminal 配送より前）で commit するため、Terminal 配送が失敗して AuditFailed を
    // 返しても、完了済み script の context 変更は rollback されず残る。host は AuditFailed を
    // 受け取った context を再利用してはならない、という逸脱を回帰で固定する。
    let sink = Arc::new(ScriptedAuditSink::new([
        ScriptedResponse::AckCorrect, // 1 回目実行の Started
        ScriptedResponse::Fail,       // 1 回目実行の Terminal（配送失敗）
                                      // 以降（2 回目実行の Started/Terminal）は tail = AckCorrect
    ]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");

    // 1 回目: `x` を束縛して完了する script。Terminal 配送に失敗する。
    let define = link_source(&engine, "m", "let x = 1\n");
    let define_linked = engine.link(&define, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(&define_linked, &mut ctx, request_with_id(30));

    // fail-closed: success ではない。journal 上の Terminal は Completed/committed のまま
    // （AuditFailure へ書き換えない。§8 規則10）＝ withheld は Completed。
    assert!(!audited.is_audited_ok());
    match &audited {
        AuditedOutcome::AuditFailed { withheld, .. } => {
            assert_eq!(*withheld, TerminalOutcome::Completed);
        }
        other => panic!("Terminal fail-closed のはずが: {other:?}"),
    }

    // 2 回目: 同じ context で `x` を参照する script を走らせる。state が rollback されていれば
    // `x` は未定義で RuntimeError になるはず。だが A-1 では commit 済みのまま残るので、`x` は
    // 参照でき、完了する（＝ state は rollback されていない、という逸脱を明文化どおり pin）。
    let use_binding = link_source(&engine, "m2", "let y = x + 2\n");
    let use_linked = engine.link(&use_binding, link_request()).expect("link");
    let audited2 = engine.run_audited(&use_linked, &mut ctx, request_with_id(31));
    match audited2.outcome() {
        Some(EmbeddingOutcome::Completed { .. }) => {}
        other => panic!(
            "A-1 では 1 回目完了 script の binding `x` は rollback されず残るはず（state 継続）: {other:?}"
        ),
    }
}

#[test]
fn started_wrong_id_ack_is_fail_closed_before_script_work() {
    // Started の ack が別 execution の id を指す（§10 protocol 違反）→ script work せず fail-closed。
    let sink = Arc::new(ScriptedAuditSink::new([ScriptedResponse::AckWrongId]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    // side-effect probe: completion すれば ctx に x が束縛されるが、ここでは走ってはならない。
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(21));
    assert!(!audited.is_audited_ok(), "不正 ack は監査成立ではない");
    assert!(matches!(audited, AuditedOutcome::AuditFailed { .. }));
    // Started は submit されたが ack が不正だったので gate は開かない。
    // Terminal(AuditFailure) の submit は AckCorrect（tail）で記録されるため、
    // 「Started を試みた記録 + emergency Terminal」で 2 件になり得る。重要なのは Terminal が
    // AuditFailure であること（script work しないので Completed Terminal は出ない）。
    let envelopes = sink.snapshot();
    assert!(
        envelopes.iter().all(|e| !matches!(
            &e.event,
            AuditEvent::Terminal {
                outcome: TerminalOutcome::Completed,
                ..
            }
        )),
        "script work は起きないので Completed Terminal は出ない"
    );
}

#[test]
fn started_wrong_sequence_ack_is_fail_closed_before_script_work() {
    // Started の ack が未送信 sequence を指す（§10 protocol 違反）→ fail-closed。
    let sink = Arc::new(ScriptedAuditSink::new([ScriptedResponse::AckWrongSequence]));
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = 1\nlet y = x + 2\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(22));
    assert!(
        !audited.is_audited_ok(),
        "不正 sequence ack は監査成立ではない"
    );
    assert!(matches!(audited, AuditedOutcome::AuditFailed { .. }));
    let envelopes = sink.snapshot();
    assert!(
        envelopes.iter().all(|e| !matches!(
            &e.event,
            AuditEvent::Terminal {
                outcome: TerminalOutcome::Completed,
                ..
            }
        )),
        "script work は起きないので Completed Terminal は出ない"
    );
}

#[test]
fn with_sink_but_missing_execution_id_fails_before_script_work() {
    // sink はあるが execution_id を載せていない → Started 前に fail-closed（§7.1）。
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = 1\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_without_id());
    // Started を発行する前の precondition 違反。InternalFailure（success ではない）。
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::InternalFailure { .. })
    ));
    // Started すら発行していないので sink は空。
    assert!(sink.is_empty());
}
