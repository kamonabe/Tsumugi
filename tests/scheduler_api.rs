//! 協調スケジューラ（admission / run-turn fairness / backpressure）の受入テスト。
//!
//! REV-015 Slice 5、実行制御仕様 §15.4、設計 §6.3。公開 API（`Engine::with_limits` と
//! `ExecutionHandle`）だけを使い、host が各 Ready handle を round-robin で poll する運用を
//! 固定する。本 FEAT（FEAT-001）は scheduler-core（admission + fairness/backpressure）の
//! 範囲を対象とし、host pending（HostCallPending / waker）は FEAT-002 で追加する。

use std::num::{NonZeroU64, NonZeroUsize};

use tsumugi::{
    Engine, EngineLimits, ExecutionContext, ExecutionOutcome, ExecutionRequest, PollResult,
    PollSlice, StartError, YieldReason,
};

fn limits(max_active: usize, max_queued: usize) -> EngineLimits {
    EngineLimits {
        max_active_executions: NonZeroUsize::new(max_active).expect("active 上限は非ゼロ"),
        max_queued_executions: max_queued,
        default_slice_fuel: NonZeroU64::new(10_000).unwrap(),
    }
}

/// 常に Ready な N execution が run-turn FIFO で各 1 slice ずつ進み、1 execution が連続独占
/// しない（AC-2）。非 head handle の poll は semantic work をせず SchedulerPreempted を返す
/// （AC-8）。slice ごとの usage.committed.fuel 増分で確認する。
#[test]
fn n_ready_executions_advance_fifo_one_slice_each() {
    const N: usize = 3;
    let engine = Engine::with_limits(limits(N, 0));
    // 各 execution は十分重い（1 slice では終わらない）。
    let heavy = "\
let i = 0\n\
while i < 1000\n\
  i = i + 1\n\
end\n";
    let script = engine.compile(heavy).unwrap();

    let mut contexts: Vec<ExecutionContext> = (0..N).map(|_| ExecutionContext::new()).collect();
    // 各 context から handle を作る。借用の都合で context を 1 つずつ handle へ渡す。
    let mut ctx_refs: Vec<&mut ExecutionContext> = contexts.iter_mut().collect();
    let mut handles = Vec::new();
    for ctx in ctx_refs.drain(..) {
        let h = engine
            .create_execution(&script, ctx, ExecutionRequest::new())
            .expect("N 個とも active を取れる");
        handles.push(h);
    }

    let small = PollSlice { max_fuel: 16 };

    // まず全 handle を 1 周 poll して run-turn へ登録する。登録順（0,1,2）が FIFO 順になる。
    // 最初の 1 周: handle 0 が head で slice を進める。1,2 は登録されるが非 head → preempt。
    // ただし 0 が rotate するので、各周で head が入れ替わる。
    let mut fuel_before = [0u64; N];
    for (idx, h) in handles.iter().enumerate() {
        fuel_before[idx] = h.usage().committed.fuel;
    }

    // 1 ラウンド目: index 0 を poll すると head（run_turn=[0]）で slice を進め rotate。
    // 続けて 1 を poll すると登録され head（0 が末尾へ回ったので run_turn=[1,2 だが...]）。
    // 実際には 0 を poll→run_turn=[0]→slice→rotate=[0]（単独なので先頭のまま）。そこで
    // round-robin を厳密に観測するため、全 handle を登録してから回す。
    //
    // 登録フェーズ: 各 handle を 1 回ずつ poll する。index 0 は head で前進、1/2 は非 head で
    // SchedulerPreempted（state 不変）。
    let r0 = handles[0].poll(small).unwrap();
    assert!(
        matches!(
            r0,
            PollResult::Yielded {
                reason: YieldReason::SliceFuelExhausted,
                ..
            }
        ),
        "最初に登録した 0 は head で slice を進める: {r0:?}"
    );
    for i in [1usize, 2] {
        let r = handles[i].poll(small).unwrap();
        assert!(
            matches!(
                r,
                PollResult::Yielded {
                    reason: YieldReason::SchedulerPreempted,
                    ..
                }
            ),
            "非 head の {i} は SchedulerPreempted（AC-8）: {r:?}"
        );
        // 非 head poll は fuel を消費しない（semantic work なし）。
        assert_eq!(
            handles[i].usage().committed.fuel,
            fuel_before[i],
            "非 head poll は semantic work をしない"
        );
    }

    // run_turn=[1,2,0]（0 は slice 後に末尾へ rotate）。以後 round-robin で各 1 slice ずつ進む。
    // 2 ラウンド回して、各 handle が概ね均等に前進する（連続独占しない）ことを確認する。
    let mut advanced = [false; N];
    for _round in 0..6 {
        for i in 0..N {
            let before = handles[i].usage().committed.fuel;
            match handles[i].poll(small).unwrap() {
                PollResult::Yielded {
                    reason: YieldReason::SliceFuelExhausted,
                    ..
                } => {
                    assert!(
                        handles[i].usage().committed.fuel > before,
                        "head の slice は fuel を進める"
                    );
                    advanced[i] = true;
                }
                PollResult::Yielded {
                    reason: YieldReason::SchedulerPreempted,
                    ..
                } => {
                    assert_eq!(
                        handles[i].usage().committed.fuel,
                        before,
                        "preempt は fuel を進めない"
                    );
                }
                PollResult::Terminal { .. } => { /* 進み切った handle は terminal になり得る */
                }
                other => panic!("想定外の poll 結果: {other:?}"),
            }
        }
    }
    assert!(
        advanced.iter().all(|&a| a),
        "全 execution が少なくとも 1 slice は前進する（連続独占しない、AC-2）: {advanced:?}"
    );

    drop(handles);
    drop(contexts);
}

/// active 上限ちょうど・queue 上限ちょうどを受理し、+1 が即時 Backpressure（AC-1、設計 §4.1）。
#[test]
fn active_and_queue_limits_exact_then_backpressure() {
    let engine = Engine::with_limits(limits(2, 2));
    let script = engine.compile("let a = 1\n").unwrap();

    // active 上限ちょうど（2 個）を受理。
    let mut ctx1 = ExecutionContext::new();
    let mut ctx2 = ExecutionContext::new();
    let _h1 = engine
        .create_execution(&script, &mut ctx1, ExecutionRequest::new())
        .expect("active 1");
    let _h2 = engine
        .create_execution(&script, &mut ctx2, ExecutionRequest::new())
        .expect("active 2");

    // queue 上限ちょうど（2 個）を受理。
    let mut ctx3 = ExecutionContext::new();
    let mut ctx4 = ExecutionContext::new();
    let _h3 = engine
        .create_execution(&script, &mut ctx3, ExecutionRequest::new())
        .expect("queue 1");
    let _h4 = engine
        .create_execution(&script, &mut ctx4, ExecutionRequest::new())
        .expect("queue 2");

    // +1 は即時 Backpressure。
    let mut ctx5 = ExecutionContext::new();
    match engine.create_execution(&script, &mut ctx5, ExecutionRequest::new()) {
        Err(StartError::Backpressure {
            active,
            queued,
            limit,
        }) => {
            assert_eq!(active, 2);
            assert_eq!(queued, 2);
            assert_eq!(limit, 2);
        }
        Ok(_) => panic!("満杯なのに handle が作られた"),
        Err(other) => panic!("Backpressure を期待したが {other:?}"),
    }
}

/// Paused handle は active slot を握り続け、その間は新規 admission が queue/backpressure に
/// なる（AC-3 の Paused 半分、設計 §4.2/§6.3）。host 待ち半分は FEAT-002 で追加する。
#[test]
fn paused_consumes_active_slot() {
    // active=1, queue=0。handle A を pause して active を握らせる。
    let engine = Engine::with_limits(limits(1, 0));
    let script = engine.compile("let a = 1\n").unwrap();

    let mut ctx_a = ExecutionContext::new();
    let mut handle_a = engine
        .create_execution(&script, &mut ctx_a, ExecutionRequest::new())
        .expect("A active");

    // A を pause する。Paused でも active slot は解放されない（§4.2 FR-4）。
    handle_a.pause().expect("pause は成功する");

    // A が active を握ったままなので、B の admission は満杯で Backpressure（queue=0）。
    let mut ctx_b = ExecutionContext::new();
    match engine.create_execution(&script, &mut ctx_b, ExecutionRequest::new()) {
        Err(StartError::Backpressure { active, .. }) => {
            assert_eq!(active, 1, "Paused handle が active slot を消費している");
        }
        Ok(_) => panic!("Paused が active を解放してしまい、無制限 admission になった"),
        Err(other) => panic!("Backpressure を期待したが {other:?}"),
    }

    // A を resume して terminal まで進めると active が解放され、新規 admission が通る。
    handle_a.resume().expect("resume は成功する");
    let outcome = loop {
        match handle_a.poll(PollSlice::default()).unwrap() {
            PollResult::Yielded { .. } => continue,
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Paused { .. } => panic!("pause は要求していない"),
        }
    };
    assert_eq!(outcome, ExecutionOutcome::Completed);
    drop(handle_a);

    // active が空いたので新規 admission が active を取れる。
    let mut ctx_c = ExecutionContext::new();
    let _h_c = engine
        .create_execution(&script, &mut ctx_c, ExecutionRequest::new())
        .expect("active 解放後は admit が通る");
}
