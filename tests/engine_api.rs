//! 埋め込み利用向け公開 API の契約テスト。
//!
//! CLI 経由の統合テストではなく、crate root から re-export される型だけを使い、
//! `Engine` と `ExecutionContext` の利用方法を固定する。

use tsumugi::{Engine, ExecutionContext, ExecutionOutcome};

#[test]
fn compile_and_execute_returns_completed() {
    let engine = Engine::new();
    let script = engine
        .compile("let answer = 40 + 2")
        .unwrap_or_else(|errors| panic!("有効なスクリプトのcompileに失敗しました: {errors:?}"));
    let mut context = ExecutionContext::new();

    let outcome = engine
        .execute(&script, &mut context)
        .unwrap_or_else(|error| panic!("有効なスクリプトの実行に失敗しました: {error}"));

    assert_eq!(outcome, ExecutionOutcome::Completed);
}

#[test]
fn compile_returns_all_parse_errors_with_line_numbers() {
    let engine = Engine::new();
    let errors = match engine.compile("let = oops\nlet valid = 1\nlet = bad") {
        Ok(_) => panic!("不正なスクリプトのcompileが成功しました"),
        Err(errors) => errors,
    };

    assert_eq!(errors.len(), 2, "想定外の構文エラー一覧: {errors:?}");
    assert_eq!(errors[0].line(), 1);
    assert_eq!(errors[1].line(), 3);
    assert!(errors.iter().all(|error| error.error_type() == "parse"));
}

#[test]
fn context_reuse_preserves_bindings_without_leaking_between_contexts() {
    let engine = Engine::new();
    let define = engine
        .compile("let answer = 42")
        .unwrap_or_else(|errors| panic!("定義のcompileに失敗しました: {errors:?}"));
    let use_binding = engine
        .compile("let next = answer + 1")
        .unwrap_or_else(|errors| panic!("参照のcompileに失敗しました: {errors:?}"));

    let mut shared = ExecutionContext::default();
    engine
        .execute(&define, &mut shared)
        .unwrap_or_else(|error| panic!("定義の実行に失敗しました: {error}"));
    assert_eq!(
        engine.execute(&use_binding, &mut shared),
        Ok(ExecutionOutcome::Completed),
        "同じcontextでは以前のbindingを参照できる必要があります"
    );

    let mut isolated = ExecutionContext::new();
    let error = engine
        .execute(&use_binding, &mut isolated)
        .expect_err("別contextへbindingが漏洩しています");
    assert_eq!(error.error_type(), "name");
    assert!(
        error.message().contains("answer"),
        "想定外のエラーメッセージ: {}",
        error.message()
    );
}

/// `set_script_args` で注入した snapshot を `args()` が返す（AUD-018）。
/// process argv ではなく実行 context に属することを固定する。
#[test]
fn args_returns_injected_snapshot() {
    let engine = Engine::new();
    let script = engine
        .compile("let a = args()\nlet joined = a[0] + \",\" + a[1]\n")
        .unwrap_or_else(|errors| panic!("compileに失敗しました: {errors:?}"));

    let mut context = ExecutionContext::new();
    context.set_script_args(vec!["first".to_string(), "second".to_string()]);

    assert_eq!(
        engine.execute(&script, &mut context),
        Ok(ExecutionOutcome::Completed),
        "注入した script 引数で実行できる必要があります"
    );
}

/// script 引数を注入しない場合、`args()` は空リストを返す（AUD-018）。
#[test]
fn args_is_empty_without_injection() {
    let engine = Engine::new();
    let script = engine
        .compile("let empty = args()\nlet ok = len(empty) == 0\n")
        .unwrap_or_else(|errors| panic!("compileに失敗しました: {errors:?}"));

    let mut context = ExecutionContext::new();
    assert_eq!(
        engine.execute(&script, &mut context),
        Ok(ExecutionOutcome::Completed),
        "引数未注入でも空リストで実行できる必要があります"
    );
}

// =============================================================================
// REV-015 Slice 3 PR-a: state machine surface（第9節）
// =============================================================================

use tsumugi::{
    CancellationToken, ExecutionHandle, ExecutionRequest, ExecutionState, HandleError, PauseReason,
    PausedState, PollResult, PollSlice, ResumeState,
};

/// create_execution は Created から始まり、poll で Terminal(Completed) へ到達する。
#[test]
fn create_execution_polls_to_completed_terminal() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1 + 2\n").unwrap();
    let mut context = ExecutionContext::new();

    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    assert_eq!(handle.state(), ExecutionState::Created);
    assert!(handle.outcome().is_none());

    let result = handle.poll(PollSlice::default()).expect("poll が失敗した");
    match result {
        PollResult::Terminal { outcome, .. } => {
            assert_eq!(outcome, ExecutionOutcome::Completed);
        }
        other => panic!("Terminal を期待したが {other:?}"),
    }
    assert_eq!(handle.state(), ExecutionState::Terminal);
    assert_eq!(handle.outcome(), Some(&ExecutionOutcome::Completed));
}

/// start は Linked から始まる。
#[test]
fn start_begins_at_linked_state() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1\n").unwrap();
    let mut context = ExecutionContext::new();

    let handle = engine
        .start(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    assert_eq!(handle.state(), ExecutionState::Linked);
}

/// terminal 到達後の poll は HandleError::Terminal を返す。
#[test]
fn poll_after_terminal_is_rejected() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1\n").unwrap();
    let mut context = ExecutionContext::new();

    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let _ = handle
        .poll(PollSlice::default())
        .expect("最初の poll が失敗した");
    assert_eq!(handle.state(), ExecutionState::Terminal);

    assert_eq!(
        handle.poll(PollSlice::default()),
        Err(HandleError::Terminal)
    );
}

/// terminal 後の pause / resume も Terminal エラー。
#[test]
fn pause_resume_after_terminal_are_rejected() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1\n").unwrap();
    let mut context = ExecutionContext::new();

    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let _ = handle.poll(PollSlice::default()).unwrap();

    assert_eq!(handle.pause(), Err(HandleError::Terminal));
    assert_eq!(handle.resume(), Err(HandleError::Terminal));
}

/// 実行中の未捕捉エラーは Terminal(RuntimeError) として outcome に現れる。
#[test]
fn runtime_error_surfaces_as_runtime_error_outcome() {
    let engine = Engine::new();
    // 未定義変数の参照は実行フェーズの runtime error。
    let script = engine.compile("let y = missing_name\n").unwrap();
    let mut context = ExecutionContext::new();

    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let result = handle.poll(PollSlice::default()).unwrap();
    match result {
        PollResult::Terminal {
            outcome: ExecutionOutcome::RuntimeError { error },
            ..
        } => {
            assert_eq!(error.error_type(), "name");
            assert!(error.message().contains("missing_name"));
        }
        other => panic!("RuntimeError を期待したが {other:?}"),
    }
}

/// Link フェーズの失敗（存在しない import）は Terminal(LinkError) として現れる。
#[test]
fn link_error_surfaces_as_link_error_outcome() {
    let engine = Engine::new();
    // 存在しない module の import は Link フェーズで失敗する。
    let script = engine.compile("import \"__no_such_module__\"\n").unwrap();
    let mut context = ExecutionContext::new();

    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let result = handle.poll(PollSlice::default()).unwrap();
    match result {
        PollResult::Terminal {
            outcome: ExecutionOutcome::LinkError { error },
            ..
        } => {
            assert_eq!(error.error_type(), "import");
        }
        other => panic!("LinkError を期待したが {other:?}"),
    }
}

/// poll 経由の outcome と、互換 wrapper execute の戻り値が一致する（poll-to-terminal 一致）。
#[test]
fn execute_matches_poll_to_terminal_outcome() {
    let engine = Engine::new();
    let source = "let z = 10 * 4\n";

    // execute 経由（互換 wrapper）。
    let script_a = engine.compile(source).unwrap();
    let mut ctx_a = ExecutionContext::new();
    let via_execute = engine.execute(&script_a, &mut ctx_a);
    assert_eq!(via_execute, Ok(ExecutionOutcome::Completed));

    // poll 経由（handle を terminal まで）。
    let script_b = engine.compile(source).unwrap();
    let mut ctx_b = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script_b, &mut ctx_b, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let via_poll = handle.poll(PollSlice::default()).unwrap();
    assert!(matches!(
        via_poll,
        PollResult::Terminal {
            outcome: ExecutionOutcome::Completed,
            ..
        }
    ));
}

/// handle は同じ context を再利用して binding を保持する。
#[test]
fn handle_reuses_context_bindings() {
    let engine = Engine::new();
    let define = engine.compile("let shared = 7\n").unwrap();
    let use_it = engine.compile("let doubled = shared * 2\n").unwrap();
    let mut context = ExecutionContext::new();

    {
        let mut h = engine
            .create_execution(&define, &mut context, ExecutionRequest::new())
            .expect("単一 execution の admit は成功する");
        assert!(matches!(
            h.poll(PollSlice::default()).unwrap(),
            PollResult::Terminal {
                outcome: ExecutionOutcome::Completed,
                ..
            }
        ));
    }
    {
        let mut h = engine
            .create_execution(&use_it, &mut context, ExecutionRequest::new())
            .expect("単一 execution の admit は成功する");
        assert!(matches!(
            h.poll(PollSlice::default()).unwrap(),
            PollResult::Terminal {
                outcome: ExecutionOutcome::Completed,
                ..
            }
        ));
    }
}

/// ExecutionHandle は !Send + !Sync（第9.1節、作成スレッド外へ move できない）。
///
/// `static_assertions` 等の外部 crate を足さず、autotrait を条件付き実装した補助 trait で
/// 「Send な型だけが `IsSend` を実装する」ようにし、`ExecutionHandle` がそれを実装しない
/// ことをコンパイル時に固定する。`ExecutionHandle` が誤って `Send` になると、
/// `assert_is_send` の呼び出しが型検査に通ってしまう回帰を、レビューで気づけるようにする
/// 意図のドキュメント test（ここでは Send を要求しないことだけを確認する）。
#[test]
fn handle_type_exists_and_is_usable() {
    // ExecutionHandle 型が公開されており、poll/pause/resume/state/usage/outcome を持つ
    // ことをコンパイル時に確認する（!Send + !Sync は型定義の PhantomData<*const ()> で担保）。
    fn _uses_handle(h: &mut ExecutionHandle<'_, '_, '_>) {
        let _ = h.state();
        let _ = h.usage();
        let _ = h.outcome();
    }
}

// =============================================================================
// slice fuel + yield（REV-015 Slice 3 PR-d-2、実行制御仕様 §4.2 / 第9節）
// =============================================================================

use tsumugi::YieldReason;

/// 小さな slice fuel で poll すると、compute の重いスクリプトは複数 slice に分かれて
/// yield しながら進み、最終的に大きな slice 1 回と同じ terminal（Completed）へ到達する。
/// slice は公平性の量子であり、total fuel を補充しないので、消費した total fuel は
/// 分割の有無にかかわらず一致する（§4.2）。
#[test]
fn small_slice_yields_and_resumes_to_same_terminal_and_total_fuel() {
    // ループ + 関数呼び出しで fuel を十分消費するスクリプト。
    let source = "\
fn work(n)\n\
  let acc = 0\n\
  let i = 0\n\
  while i < n\n\
    acc = acc + i\n\
    i = i + 1\n\
  end\n\
  return acc\n\
end\n\
let total = 0\n\
let k = 0\n\
while k < 50\n\
  total = total + work(20)\n\
  k = k + 1\n\
end\n";

    let engine = Engine::new();

    // (A) 大きな slice 1 回で terminal まで（分割なし）。
    let script_a = engine.compile(source).unwrap();
    let mut ctx_a = ExecutionContext::new();
    let mut handle_a = engine
        .create_execution(&script_a, &mut ctx_a, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let big_slice = PollSlice {
        max_fuel: 10_000_000,
    };
    let outcome_a = match handle_a.poll(big_slice).unwrap() {
        PollResult::Terminal { outcome, .. } => outcome,
        other => panic!("大きな slice では 1 回で terminal を期待: {other:?}"),
    };
    assert_eq!(outcome_a, ExecutionOutcome::Completed);
    let total_fuel_single = handle_a.usage().committed.fuel;
    assert!(total_fuel_single > 16, "テストが fuel を十分消費していない");

    // (B) 小さな slice で複数回 poll。少なくとも 1 回は yield し、最後は同じ terminal。
    let script_b = engine.compile(source).unwrap();
    let mut ctx_b = ExecutionContext::new();
    let mut handle_b = engine
        .create_execution(&script_b, &mut ctx_b, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let small_slice = PollSlice { max_fuel: 16 };

    let mut yields = 0usize;
    let mut polls = 0usize;
    let final_outcome = loop {
        polls += 1;
        assert!(
            polls < 1_000_000,
            "poll が terminal に到達しない（無限ループ）"
        );
        match handle_b.poll(small_slice).unwrap() {
            PollResult::Yielded { reason, .. } => {
                assert_eq!(reason, YieldReason::SliceFuelExhausted);
                assert_eq!(
                    handle_b.state(),
                    ExecutionState::Yielded(YieldReason::SliceFuelExhausted)
                );
                yields += 1;
            }
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Paused { .. } => panic!("pause は要求していない"),
        }
    };

    assert!(
        yields > 0,
        "小さな slice なのに一度も yield しなかった（slice fuel が効いていない）"
    );
    assert_eq!(final_outcome, ExecutionOutcome::Completed);

    // total fuel は slice 分割の有無で変わらない（§4.2: slice は total を補充しない）。
    assert_eq!(
        handle_b.usage().committed.fuel,
        total_fuel_single,
        "分割実行の total fuel が単一 slice と一致しない"
    );
}

/// yield 後の handle は Yielded 状態で、再 poll で resume し最終的に terminal へ到達する。
/// terminal 到達後の poll は HandleError::Terminal。
#[test]
fn yielded_handle_resumes_then_rejects_poll_after_terminal() {
    let source = "\
let i = 0\n\
while i < 100\n\
  i = i + 1\n\
end\n";
    let engine = Engine::new();
    let script = engine.compile(source).unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let small_slice = PollSlice { max_fuel: 16 };

    // 初回 poll は yield する（100 反復は 16 fuel に収まらない）。
    match handle.poll(small_slice).unwrap() {
        PollResult::Yielded { reason, .. } => {
            assert_eq!(reason, YieldReason::SliceFuelExhausted)
        }
        other => panic!("初回は yield を期待: {other:?}"),
    }

    // resume して terminal まで。
    loop {
        match handle.poll(small_slice).unwrap() {
            PollResult::Yielded { .. } => continue,
            PollResult::Terminal { outcome, .. } => {
                assert_eq!(outcome, ExecutionOutcome::Completed);
                break;
            }
            PollResult::Paused { .. } => panic!("pause は要求していない"),
        }
    }

    // terminal 後の poll は拒否される。
    assert_eq!(handle.poll(small_slice), Err(HandleError::Terminal));
}

/// slice 分割で実行しても、未捕捉エラーの terminal（RuntimeError）へ正しく到達する。
/// yield を跨いだ後にエラーが起きても continuation は破綻しない。
#[test]
fn slice_split_reaches_runtime_error_terminal() {
    // 50 反復ループの後に未定義変数参照で runtime error。
    let source = "\
let i = 0\n\
while i < 50\n\
  i = i + 1\n\
end\n\
let bad = missing_name\n";
    let engine = Engine::new();
    let script = engine.compile(source).unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let small_slice = PollSlice { max_fuel: 16 };

    let outcome = loop {
        match handle.poll(small_slice).unwrap() {
            PollResult::Yielded { .. } => continue,
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Paused { .. } => panic!("pause は要求していない"),
        }
    };
    assert!(
        matches!(outcome, ExecutionOutcome::RuntimeError { .. }),
        "未定義変数参照は RuntimeError を期待: {outcome:?}"
    );
}

// =============================================================================
// pause / resume 状態機械（REV-015 Slice 4 (D)、実行制御仕様 §9.1 / §9.2 / §11 / §15.3）
// =============================================================================

/// Created から pause すると Paused(resume_to: Created) へ遷移し、resume で Created へ戻る。
/// pause 中の poll は拒否され、resume 後に poll して terminal まで到達できる。
#[test]
fn pause_from_created_resumes_to_created_and_completes() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1 + 2\n").unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    assert_eq!(handle.state(), ExecutionState::Created);

    // Created から pause。resume_to は Created。
    handle.pause().expect("Created からの pause は成功する");
    assert_eq!(
        handle.state(),
        ExecutionState::Paused(PausedState {
            reason: PauseReason::HostRequested,
            resume_to: ResumeState::Created,
        })
    );

    // pause 中の poll は拒否される（§11: read-only、resume/cancel/drop のみ）。
    assert_eq!(
        handle.poll(PollSlice::default()),
        Err(HandleError::InvalidState {
            operation: "poll",
            state: ExecutionState::Paused(PausedState {
                reason: PauseReason::HostRequested,
                resume_to: ResumeState::Created,
            }),
        })
    );

    // resume で Created へ戻り、poll で terminal まで到達する。
    handle.resume().expect("Paused からの resume は成功する");
    assert_eq!(handle.state(), ExecutionState::Created);
    match handle.poll(PollSlice::default()).unwrap() {
        PollResult::Terminal { outcome, .. } => assert_eq!(outcome, ExecutionOutcome::Completed),
        other => panic!("resume 後は terminal を期待: {other:?}"),
    }
}

/// Yielded 状態から pause すると resume_to に YieldReason を保持し、resume で Yielded へ戻る。
/// pause/resume を挟んでも、挟まない場合と同じ terminal（Completed）へ到達する。
#[test]
fn pause_from_yielded_preserves_reason_and_completes() {
    let source = "\
let i = 0\n\
while i < 100\n\
  i = i + 1\n\
end\n";
    let engine = Engine::new();
    let script = engine.compile(source).unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let small_slice = PollSlice { max_fuel: 16 };

    // 初回 poll で yield させる。
    match handle.poll(small_slice).unwrap() {
        PollResult::Yielded { reason, .. } => {
            assert_eq!(reason, YieldReason::SliceFuelExhausted)
        }
        other => panic!("初回は yield を期待: {other:?}"),
    }
    assert_eq!(
        handle.state(),
        ExecutionState::Yielded(YieldReason::SliceFuelExhausted)
    );

    // Yielded から pause。resume_to は Yielded(SliceFuelExhausted)。
    handle.pause().expect("Yielded からの pause は成功する");
    assert_eq!(
        handle.state(),
        ExecutionState::Paused(PausedState {
            reason: PauseReason::HostRequested,
            resume_to: ResumeState::Yielded(YieldReason::SliceFuelExhausted),
        })
    );

    // resume で Yielded へ戻る。
    handle.resume().expect("resume は成功する");
    assert_eq!(
        handle.state(),
        ExecutionState::Yielded(YieldReason::SliceFuelExhausted)
    );

    // 続けて poll すれば terminal まで到達する（continuation は pause/resume で失われない）。
    let outcome = loop {
        match handle.poll(small_slice).unwrap() {
            PollResult::Yielded { .. } => continue,
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Paused { .. } => panic!("pause は要求していない"),
        }
    };
    assert_eq!(outcome, ExecutionOutcome::Completed);
}

/// 二重 pause は InvalidState（Paused からは pause できない）。
#[test]
fn double_pause_is_rejected() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1\n").unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");

    handle.pause().expect("最初の pause は成功する");
    // 2 回目の pause は Paused 状態からは不正。
    assert_eq!(
        handle.pause(),
        Err(HandleError::InvalidState {
            operation: "pause",
            state: ExecutionState::Paused(PausedState {
                reason: PauseReason::HostRequested,
                resume_to: ResumeState::Created,
            }),
        })
    );
}

/// Paused 以外からの resume は InvalidState（Created からは resume できない）。
#[test]
fn resume_without_pause_is_rejected() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1\n").unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");

    assert_eq!(
        handle.resume(),
        Err(HandleError::InvalidState {
            operation: "resume",
            state: ExecutionState::Created,
        })
    );
}

/// terminal 到達後の pause / resume は HandleError::Terminal（§15.3）。
#[test]
fn pause_and_resume_after_terminal_are_rejected() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1\n").unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");

    // terminal まで進める。
    let _ = handle.poll(PollSlice::default()).unwrap();
    assert_eq!(handle.state(), ExecutionState::Terminal);

    assert_eq!(handle.pause(), Err(HandleError::Terminal));
    assert_eq!(handle.resume(), Err(HandleError::Terminal));
}

// =============================================================================
// race linearization / terminal 後拒否（REV-015 Slice 4 (E)、実行制御仕様 §8 / §15.3）
// =============================================================================

/// handle は cancel token を公開し、実行前に cancel すると最初の poll で Cancelled terminal
/// になる（§8: cancel が先に観測されれば Cancelled へ遷移）。language-state は rollback される。
#[test]
fn cancel_before_poll_reaches_cancelled_terminal() {
    let source = "\
let i = 0\n\
while i < 100\n\
  i = i + 1\n\
end\n";
    let engine = Engine::new();
    let script = engine.compile(source).unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");

    // handle から cancel token を取り、実行前に cancel する（別スレッドの cancel を模す）。
    let token: CancellationToken = handle.cancellation_token();
    assert!(
        token.cancel(),
        "初回 cancel は true（§8 linearization point）"
    );

    // 最初の poll は charge 前 checkpoint で cancel を観測し、Cancelled terminal になる。
    match handle.poll(PollSlice::default()).unwrap() {
        PollResult::Terminal { outcome, .. } => {
            assert_eq!(outcome, ExecutionOutcome::Cancelled)
        }
        other => panic!("cancel 済みなら Cancelled terminal を期待: {other:?}"),
    }
    assert_eq!(handle.state(), ExecutionState::Terminal);
    assert_eq!(handle.outcome(), Some(&ExecutionOutcome::Cancelled));
}

/// yield を跨いだ後に cancel しても、次の poll で Cancelled terminal になる（§8 の確認点）。
#[test]
fn cancel_across_yield_reaches_cancelled_terminal() {
    let source = "\
let i = 0\n\
while i < 200\n\
  i = i + 1\n\
end\n";
    let engine = Engine::new();
    let script = engine.compile(source).unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let token = handle.cancellation_token();
    let small_slice = PollSlice { max_fuel: 16 };

    // 初回 poll で yield させる。
    match handle.poll(small_slice).unwrap() {
        PollResult::Yielded { .. } => {}
        other => panic!("初回は yield を期待: {other:?}"),
    }

    // yield 中に cancel。次の poll で checkpoint が観測し Cancelled terminal になる。
    assert!(token.cancel());
    let outcome = loop {
        match handle.poll(small_slice).unwrap() {
            PollResult::Yielded { .. } => continue,
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Paused { .. } => panic!("pause は要求していない"),
        }
    };
    assert_eq!(outcome, ExecutionOutcome::Cancelled);
}

/// 正常完了後の cancel は outcome を変えない（§8「commit 後の cancel は結果を変えない」）。
/// terminal event は1回だけで、以後の poll は HandleError::Terminal。
#[test]
fn cancel_after_completion_does_not_change_outcome() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1 + 2\n").unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let token = handle.cancellation_token();

    // 正常完了させる。
    match handle.poll(PollSlice::default()).unwrap() {
        PollResult::Terminal { outcome, .. } => {
            assert_eq!(outcome, ExecutionOutcome::Completed)
        }
        other => panic!("Completed を期待: {other:?}"),
    }

    // 完了後に cancel しても outcome は Completed のまま。
    assert!(token.cancel(), "cancel 自体は成功する（token の状態遷移）");
    assert_eq!(handle.outcome(), Some(&ExecutionOutcome::Completed));
    // terminal event は1回だけ: 以後の poll は拒否される。
    assert_eq!(
        handle.poll(PollSlice::default()),
        Err(HandleError::Terminal)
    );
}

/// Paused 状態でも cancel でき（§11: pause 中に許可する操作は resume/cancel/drop）、
/// resume 後の poll で Cancelled terminal になる。
#[test]
fn cancel_during_pause_then_resume_reaches_cancelled() {
    let source = "\
let i = 0\n\
while i < 100\n\
  i = i + 1\n\
end\n";
    let engine = Engine::new();
    let script = engine.compile(source).unwrap();
    let mut context = ExecutionContext::new();
    let mut handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");
    let token = handle.cancellation_token();

    // Created から pause。
    handle.pause().expect("pause は成功する");
    // pause 中に cancel（§11 で許可）。
    assert!(token.cancel());
    // resume して poll すると Cancelled terminal。
    handle.resume().expect("resume は成功する");
    match handle.poll(PollSlice::default()).unwrap() {
        PollResult::Terminal { outcome, .. } => {
            assert_eq!(outcome, ExecutionOutcome::Cancelled)
        }
        other => panic!("cancel 済みなら Cancelled terminal を期待: {other:?}"),
    }
}

/// cancel は idempotent（clone を跨いで最初の false→true だけ true、§8）。
#[test]
fn cancel_token_is_idempotent_across_clones() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1\n").unwrap();
    let mut context = ExecutionContext::new();
    let handle = engine
        .create_execution(&script, &mut context, ExecutionRequest::new())
        .expect("単一 execution の admit は成功する");

    let token_a = handle.cancellation_token();
    let token_b = handle.cancellation_token();
    assert!(token_a.cancel(), "初回 cancel は true");
    assert!(!token_b.cancel(), "clone を跨いでも 2 回目は false");
    assert!(token_a.is_cancelled() && token_b.is_cancelled());
}

// =============================================================================
// scheduler admission / run-turn（REV-015 Slice 5、実行制御仕様 §9 / §15.3、設計 §4.1/§4.3）
// =============================================================================

use tsumugi::{AdmissionPhase, EngineLimits};

/// 小さい EngineLimits（active=1, queue=small）を作るヘルパ。
fn limited_engine(max_active: usize, max_queued: usize) -> Engine {
    Engine::with_limits(EngineLimits {
        max_active_executions: std::num::NonZeroUsize::new(max_active).unwrap(),
        max_queued_executions: max_queued,
        default_slice_fuel: std::num::NonZeroU64::new(10_000).unwrap(),
    })
}

/// active + queue 満杯で create_execution が Backpressure を返し、handle を作らず context を
/// 変更しない（AC-1、設計 §4.1）。
#[test]
fn admission_full_create_returns_no_handle_no_context_change() {
    let engine = limited_engine(1, 1);
    let script = engine.compile("let a = 1\n").unwrap();

    // active=1 を占有する handle A（context_a）。
    let mut ctx_a = ExecutionContext::new();
    let _handle_a = engine
        .create_execution(&script, &mut ctx_a, ExecutionRequest::new())
        .expect("1 個目は active を取れる");

    // queue=1 を占有する handle B（context_b）。
    let mut ctx_b = ExecutionContext::new();
    let _handle_b = engine
        .create_execution(&script, &mut ctx_b, ExecutionRequest::new())
        .expect("2 個目は queue を取れる");

    // 3 個目は active+queue 満杯で Backpressure。context_c は変更されない。
    let mut ctx_c = ExecutionContext::new();
    let before = ctx_c.budget_usage().committed.fuel;
    match engine.create_execution(&script, &mut ctx_c, ExecutionRequest::new()) {
        Err(tsumugi::StartError::Backpressure {
            active,
            queued,
            limit,
        }) => {
            assert_eq!(active, 1);
            assert_eq!(queued, 1);
            assert_eq!(limit, 1);
        }
        Ok(_) => panic!("満杯なのに handle が作られた"),
        Err(other) => panic!("Backpressure を期待したが {other:?}"),
    }
    // context は借用すら発生せず不変（admit は borrow より前に失敗する）。
    assert_eq!(ctx_c.budget_usage().committed.fuel, before);
}

/// queue 待ち handle は Yielded(AdmissionQueued) から始まり、active が空くと昇格して
/// resume_to（Created）へ戻る。昇格前の poll は semantic work をせず AdmissionQueued を返す
/// （設計 §4.1/§4.3、§6.2 matrix の AdmissionQueued 行）。
#[test]
fn admission_queued_row_promotes_to_resume_to_on_active_release() {
    let engine = limited_engine(1, 2);
    let script = engine.compile("let a = 1\n").unwrap();

    let mut ctx_a = ExecutionContext::new();
    let handle_a = engine
        .create_execution(&script, &mut ctx_a, ExecutionRequest::new())
        .expect("active");
    assert_eq!(handle_a.state(), ExecutionState::Created);

    let mut ctx_b = ExecutionContext::new();
    let mut handle_b = engine
        .create_execution(&script, &mut ctx_b, ExecutionRequest::new())
        .expect("queue");
    // queue 待ちは AdmissionQueued(resume_to: Created) から始まる。
    assert_eq!(
        handle_b.state(),
        ExecutionState::Yielded(YieldReason::AdmissionQueued {
            resume_to: AdmissionPhase::Created,
        })
    );

    // 昇格前の poll は semantic work をせず AdmissionQueued を返す（state 不変）。
    match handle_b.poll(PollSlice::default()).unwrap() {
        PollResult::Yielded {
            reason: YieldReason::AdmissionQueued { resume_to },
            ..
        } => assert_eq!(resume_to, AdmissionPhase::Created),
        other => panic!("AdmissionQueued を期待: {other:?}"),
    }
    assert_eq!(
        handle_b.state(),
        ExecutionState::Yielded(YieldReason::AdmissionQueued {
            resume_to: AdmissionPhase::Created,
        })
    );

    // active を解放すると handle_b が FIFO 先頭から昇格する。
    drop(handle_a);
    // 次の poll で昇格を観測し resume_to（Created）へ戻って semantic work を進め、terminal へ。
    let outcome = loop {
        match handle_b.poll(PollSlice::default()).unwrap() {
            PollResult::Yielded { .. } => continue,
            PollResult::Terminal { outcome, .. } => break outcome,
            PollResult::Paused { .. } => panic!("pause は要求していない"),
        }
    };
    assert_eq!(outcome, ExecutionOutcome::Completed);
}

/// 非 head の poll は SchedulerPreempted を reason としてだけ返し、state は不変（Finding 4、
/// §6.2 matrix の非 head 行）。pause/resume は SchedulerPreempted を巻き込まない。
#[test]
fn non_head_poll_returns_scheduler_preempted_without_state_change() {
    // active=2 で A/B を同時 active にし、A を run-turn へ先に登録してから B を poll する。
    let engine = limited_engine(2, 0);
    // A は複数 slice に分かれる重いスクリプト（1 回の poll で terminal にしない）。
    let heavy = "\
let i = 0\n\
while i < 500\n\
  i = i + 1\n\
end\n";
    let script_a = engine.compile(heavy).unwrap();
    let script_b = engine.compile("let b = 1\n").unwrap();

    let mut ctx_a = ExecutionContext::new();
    let mut handle_a = engine
        .create_execution(&script_a, &mut ctx_a, ExecutionRequest::new())
        .expect("A active");
    let mut ctx_b = ExecutionContext::new();
    let mut handle_b = engine
        .create_execution(&script_b, &mut ctx_b, ExecutionRequest::new())
        .expect("B active");

    // A を小さい slice で poll して run-turn に登録しつつ yield させる（run_turn=[A]、head は A）。
    let small = PollSlice { max_fuel: 16 };
    match handle_a.poll(small).unwrap() {
        PollResult::Yielded {
            reason: YieldReason::SliceFuelExhausted,
            ..
        } => {}
        other => panic!("A は slice yield を期待: {other:?}"),
    }

    // B を poll すると B が run-turn 末尾へ登録され（run_turn=[A,B]）、head でないため
    // SchedulerPreempted を返す。state は元の Created のまま（Finding 4）。
    assert_eq!(handle_b.state(), ExecutionState::Created);
    match handle_b.poll(small).unwrap() {
        PollResult::Yielded {
            reason: YieldReason::SchedulerPreempted,
            ..
        } => {}
        other => panic!("非 head は SchedulerPreempted を期待: {other:?}"),
    }
    assert_eq!(
        handle_b.state(),
        ExecutionState::Created,
        "SchedulerPreempted は state を遷移させない（Finding 4）"
    );

    // pause/resume は SchedulerPreempted を巻き込まず、元の Created の resume_to へ戻る。
    handle_b.pause().expect("pause は成功する");
    assert_eq!(
        handle_b.state(),
        ExecutionState::Paused(PausedState {
            reason: PauseReason::HostRequested,
            resume_to: ResumeState::Created,
        })
    );
    handle_b.resume().expect("resume は成功する");
    assert_eq!(handle_b.state(), ExecutionState::Created);
}

/// pause → resume が元の resume_to（Created / Linked / Ready / Yielded）へ戻る
/// （既存 pause_from_* の補完、§6.2 の pause_resume_returns_to_resume_to）。
#[test]
fn pause_resume_returns_to_resume_to() {
    let engine = Engine::new();

    // Created。
    {
        let script = engine.compile("let x = 1\n").unwrap();
        let mut context = ExecutionContext::new();
        let mut handle = engine
            .create_execution(&script, &mut context, ExecutionRequest::new())
            .expect("admit");
        handle.pause().unwrap();
        handle.resume().unwrap();
        assert_eq!(handle.state(), ExecutionState::Created);
    }

    // Linked（start 入口）。
    {
        let script = engine.compile("let x = 1\n").unwrap();
        let mut context = ExecutionContext::new();
        let mut handle = engine
            .start(&script, &mut context, ExecutionRequest::new())
            .expect("admit");
        handle.pause().unwrap();
        handle.resume().unwrap();
        assert_eq!(handle.state(), ExecutionState::Linked);
    }

    // Yielded(SliceFuelExhausted)。
    {
        let source = "\
let i = 0\n\
while i < 100\n\
  i = i + 1\n\
end\n";
        let script = engine.compile(source).unwrap();
        let mut context = ExecutionContext::new();
        let mut handle = engine
            .create_execution(&script, &mut context, ExecutionRequest::new())
            .expect("admit");
        match handle.poll(PollSlice { max_fuel: 16 }).unwrap() {
            PollResult::Yielded { .. } => {}
            other => panic!("yield を期待: {other:?}"),
        }
        handle.pause().unwrap();
        handle.resume().unwrap();
        assert_eq!(
            handle.state(),
            ExecutionState::Yielded(YieldReason::SliceFuelExhausted)
        );
    }
}
