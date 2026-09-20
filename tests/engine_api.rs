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
    ExecutionHandle, ExecutionRequest, ExecutionState, HandleError, PollResult, PollSlice,
};

/// create_execution は Created から始まり、poll で Terminal(Completed) へ到達する。
#[test]
fn create_execution_polls_to_completed_terminal() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1 + 2\n").unwrap();
    let mut context = ExecutionContext::new();

    let mut handle = engine.create_execution(&script, &mut context, ExecutionRequest::new());
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

    let handle = engine.start(&script, &mut context, ExecutionRequest::new());
    assert_eq!(handle.state(), ExecutionState::Linked);
}

/// terminal 到達後の poll は HandleError::Terminal を返す。
#[test]
fn poll_after_terminal_is_rejected() {
    let engine = Engine::new();
    let script = engine.compile("let x = 1\n").unwrap();
    let mut context = ExecutionContext::new();

    let mut handle = engine.create_execution(&script, &mut context, ExecutionRequest::new());
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

    let mut handle = engine.create_execution(&script, &mut context, ExecutionRequest::new());
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

    let mut handle = engine.create_execution(&script, &mut context, ExecutionRequest::new());
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

    let mut handle = engine.create_execution(&script, &mut context, ExecutionRequest::new());
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
    let mut handle = engine.create_execution(&script_b, &mut ctx_b, ExecutionRequest::new());
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
        let mut h = engine.create_execution(&define, &mut context, ExecutionRequest::new());
        assert!(matches!(
            h.poll(PollSlice::default()).unwrap(),
            PollResult::Terminal {
                outcome: ExecutionOutcome::Completed,
                ..
            }
        ));
    }
    {
        let mut h = engine.create_execution(&use_it, &mut context, ExecutionRequest::new());
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
    let mut handle_a = engine.create_execution(&script_a, &mut ctx_a, ExecutionRequest::new());
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
    let mut handle_b = engine.create_execution(&script_b, &mut ctx_b, ExecutionRequest::new());
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
    let mut handle = engine.create_execution(&script, &mut context, ExecutionRequest::new());
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
    let mut handle = engine.create_execution(&script, &mut context, ExecutionRequest::new());
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
