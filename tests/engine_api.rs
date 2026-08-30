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
