//! StableError 統合テスト（決定性 Slice 1）。
//!
//! tree backend で catch 可能 error を実際に起こし、[`tsumugi::StableError`] への射影が
//! §4.6 の全 field を矛盾なく持つことを end-to-end で確認する。Slice 1 の受入は「tree
//! backend で StableError を決定的に生成できる」こと（§9 統合テスト）。

use tsumugi::embedding::ModuleId;
use tsumugi::error::{ErrorKind, TsumugiError};
use tsumugi::stable_error::StableError;

/// ソースを tree-walk 評価器で実行し、最初の runtime error を返す。
fn run_tree(source: &str) -> TsumugiError {
    let tokens = tsumugi::lexer::Lexer::new(source).tokenize();
    let program = tsumugi::parser::Parser::new(tokens)
        .parse()
        .unwrap_or_else(|errors| panic!("パースに失敗: {errors:?}"));
    let mut evaluator = tsumugi::eval::Evaluator::new();
    evaluator
        .run(&program, source.len() as u64)
        .expect_err("tree: エラーになるはずのソースが成功した")
}

fn test_module_id() -> ModuleId {
    ModuleId::new("test").unwrap()
}

/// tree backend で catch 可能 error を起こし、StableError が全 field を矛盾なく持つ。
#[test]
fn tree_zero_division_projects_to_stable_error() {
    let err = run_tree("1 / 0");
    let stable =
        StableError::project(&err, test_module_id()).expect("catch 可能 error は射影される");

    assert_eq!(stable.code, ErrorKind::ZeroDivision);
    assert_eq!(stable.script_type, "zero_division");
    assert_eq!(stable.message_id.as_str(), "zero_division");
    assert_eq!(stable.script_message, "ゼロ除算");
    assert_eq!(stable.module_id.as_str(), "test");
    assert_eq!(stable.line, 1);
    assert_eq!(stable.column, None);
    assert!(stable.trace.is_empty());
}

/// tree backend で user function arity error を起こし、code / message_id / trace を確認。
#[test]
fn tree_user_arity_with_trace() {
    let source = "fn f(a)\n  a\nend\nfn g()\n  f(1,2)\nend\ng()";
    let err = run_tree(source);
    let stable =
        StableError::project(&err, test_module_id()).expect("catch 可能 error は射影される");

    assert_eq!(stable.code, ErrorKind::Argument);
    assert_eq!(stable.message_id.as_str(), "argument.user_arity");
    assert_eq!(stable.line, 5);
    // trace: g() → f()（error は f の call site, caller は g の body）。
    assert!(!stable.trace.is_empty());
    assert_eq!(stable.trace[0].function, "g");
}

/// tree backend で undefined name error を起こし、Name + message_id を確認。
#[test]
fn tree_undefined_name() {
    let err = run_tree("x");
    let stable = StableError::project(&err, test_module_id()).expect("catch 可能");

    assert_eq!(stable.code, ErrorKind::Name);
    assert_eq!(stable.message_id.as_str(), "name.undefined");
    assert!(stable.script_message.contains("x"));
}

/// 決定性: 同一ソース・同一 module_id から同じ error を 2 回起こし、射影結果が完全一致する。
#[test]
fn tree_deterministic_projection() {
    let source = "1 / 0";
    let err1 = run_tree(source);
    let err2 = run_tree(source);
    let s1 = StableError::project(&err1, test_module_id()).unwrap();
    let s2 = StableError::project(&err2, test_module_id()).unwrap();
    assert_eq!(s1, s2, "同一入力からの射影は完全一致するべき");
}

/// tree backend で Type error (arithmetic) を起こし、class B タグが機能する。
#[test]
fn tree_type_arithmetic_class_b() {
    let err = run_tree("1 + null");
    let stable = StableError::project(&err, test_module_id()).unwrap();

    assert_eq!(stable.code, ErrorKind::Type);
    assert_eq!(stable.script_type, "type");
    assert_eq!(stable.message_id.as_str(), "type.arithmetic");
}

/// tree backend で Index error (list) を起こし、class B タグが機能する。
#[test]
fn tree_index_list_out_of_range() {
    let err = run_tree("let a = [1, 2]\na[10]");
    let stable = StableError::project(&err, test_module_id()).unwrap();

    assert_eq!(stable.code, ErrorKind::Index);
    assert_eq!(stable.message_id.as_str(), "index.list_out_of_range");
}

// 注: StackOverflow（call_depth）の end-to-end 誘発は 128 深度の native 再帰を要し、既定の
// テストスレッド stack（CLI の 8MB thread ではない）を溢れさせる。call_depth / ast_depth の
// 射影（class B タグ）は stable_error.rs のユニットテストで constructor から直接固定している。
