//! Phase 6 実行時監査（audit）A-1 スライスの end-to-end 統合テスト。
//!
//! 設計正本は `docs/determinism-and-audit.md` §7/§8/§10/§11。tree engine のみ。
//! crate root から re-export される公開型だけを使い、監査の opt-in・emission 順序・
//! outcome 写像・fail-closed を固定する。

use std::sync::Arc;

use tsumugi::{
    AuditEvent, AuditSink, AuditedOutcome, BudgetConfig, CompileOptions, EmbeddingContext,
    EmbeddingEngine, EmbeddingOutcome, EmbeddingRequest, ExecutionId, FakeClock, InMemoryAuditSink,
    LinkRequest, MonotonicClock, Source, SourceId, TerminalOutcome,
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
    let linked = engine.link(&script, LinkRequest::new()).expect("link");
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
    let linked = engine.link(&script, LinkRequest::new()).expect("link");
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
    let linked = engine.link(&script, LinkRequest::new()).expect("link");
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
    assert_eq!(envelopes.len(), 2, "Started と Terminal の 2 件のはず");

    // Started は sequence 0 に 1 件。
    assert_eq!(envelopes[0].sequence, 0);
    assert_eq!(envelopes[0].schema_version, 1);
    assert_eq!(envelopes[0].execution_id, execution_id);
    assert_eq!(envelopes[0].source_hash, *script.source_hash().as_bytes());
    assert_eq!(envelopes[0].language_revision, "0.20");
    assert!(matches!(
        envelopes[0].event,
        AuditEvent::ExecutionStarted { .. }
    ));

    // Terminal は最後（sequence 1）に 1 件、gap/重複なし。
    assert_eq!(envelopes[1].sequence, 1);
    assert_eq!(envelopes[1].execution_id, execution_id);
    match &envelopes[1].event {
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

#[test]
fn with_sink_runtime_error_maps_to_runtime_error_terminal() {
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", "let x = undefined_name\n");
    let linked = engine.link(&script, LinkRequest::new()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(7));
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::RuntimeError { .. })
    ));

    let envelopes = sink.snapshot();
    assert_eq!(envelopes.len(), 2);
    match &envelopes[1].event {
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
    let linked = engine.link(&script, LinkRequest::new()).expect("link");
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
    let linked = engine.link(&script, LinkRequest::new()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);

    let audited = engine.run_audited(&linked, &mut ctx, request_with_id(5));

    // fail-closed: AuditFailed を返し、success な outcome ではない。
    assert!(!audited.is_audited_ok(), "fail-closed なら監査成立ではない");
    match audited {
        AuditedOutcome::AuditFailed { withheld, .. } => {
            // 監査が成立していれば Completed だったはず（参考情報）。
            assert_eq!(withheld, TerminalOutcome::Completed);
        }
        other => panic!("fail-closed のはずが: {other:?}"),
    }
    // failing sink は何も記録しない（submit が常に Failed）。
    assert!(sink.is_empty());
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
    let linked = engine.link(&script, LinkRequest::new()).expect("link");
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
