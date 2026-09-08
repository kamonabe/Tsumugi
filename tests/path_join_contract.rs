//! AUD-034: `path_join` の引数型契約テスト（semantic-decisions.md §9）。
//!
//! `path_join` は可変長引数で、全引数が Str でなければならない。非 Str を無言で
//! skip せず、最初の非 Str 引数で `builtin_type` エラーを返す。正常系は Rust の
//! `PathBuf` に左から順に push した結果と一致する。
//!
//! 正常系の期待値は OS 依存の separator/prefix 挙動を含むため、固定文字列ではなく
//! テスト側でも `PathBuf` を構築して比較する（§9.6）。tree/VM はどちらも AUD-049 の
//! 同じ registry/handler（`builtin_path_join`）へ委譲するため、join 意味論はこの
//! 共有 handler のテストで両 engine を覆う。error の tree/VM 一致は
//! `canonical_error_inventory` で別途固定する。

use tsumugi::builtin_core::builtin_path_join;
use tsumugi::error::ErrorKind;
use tsumugi::value::Value;

/// Str 引数から `path_join` を呼ぶ。
fn join(parts: &[&str]) -> String {
    let args: Vec<Value> = parts.iter().map(|s| Value::Str((*s).to_string())).collect();
    match builtin_path_join(&args, 1) {
        Ok(Value::Str(s)) => s,
        other => panic!("Str が返るはず: {other:?}"),
    }
}

/// テスト側でも同じ順序で `PathBuf::push` した OS 依存の期待値を作る。
fn expected(parts: &[&str]) -> String {
    let mut path = std::path::PathBuf::new();
    for part in parts {
        path.push(part);
    }
    path.to_string_lossy().to_string()
}

fn assert_join(parts: &[&str]) {
    assert_eq!(
        join(parts),
        expected(parts),
        "path_join({parts:?}) が PathBuf::push 相当と一致しない"
    );
}

#[test]
fn zero_args_returns_empty_string() {
    assert_eq!(join(&[]), "", "0引数は空文字列を返す");
    assert_eq!(join(&[]), expected(&[]));
}

#[test]
fn single_arg_matches_pathbuf() {
    assert_join(&["home"]);
    assert_join(&["file.txt"]);
}

#[test]
fn multiple_args_match_pathbuf_push_order() {
    assert_join(&["home", "user", "file.txt"]);
    assert_join(&["a", "b", "c", "d"]);
}

#[test]
fn empty_string_components() {
    assert_join(&["", "b"]);
    assert_join(&["a", "", "b"]);
    assert_join(&["a", ""]);
}

#[test]
fn absolute_component_follows_pathbuf_semantics() {
    // 途中の absolute component は PathBuf::push が既存 path を置き換える。
    // 固定文字列にせず PathBuf 側の挙動へ委ねる（§9.2 の「常に / で結合する」却下）。
    assert_join(&["a", "/b", "c"]);
    assert_join(&["/a", "b"]);
}

#[test]
fn dot_and_dotdot_components_are_not_normalized() {
    // path_join は正規化・存在確認をしない（§9.1）。
    assert_join(&["a", ".", "b"]);
    assert_join(&["a", "..", "b"]);
    assert_join(&[".", "a"]);
}

#[test]
fn unicode_components() {
    assert_join(&["ホーム", "ユーザー", "ファイル.txt"]);
    assert_join(&["café", "naïve"]);
}

#[test]
fn components_containing_separators() {
    // component 内の separator も PathBuf の解釈に委ねる。
    assert_join(&["a/b", "c"]);
    assert_join(&["a", "b/c/d"]);
}

#[test]
fn non_str_at_first_position_errors() {
    let args = vec![Value::Int(1), Value::Str("b".to_string())];
    let error = builtin_path_join(&args, 7).expect_err("非 Str 第1引数はエラーになる");
    assert_eq!(error.kind(), Some(ErrorKind::BuiltinType));
    assert_eq!(
        error.message(),
        "path_join の第 1 引数は Str である必要があります: Int"
    );
    assert_eq!(error.line(), 7, "line は call 式の行");
}

#[test]
fn non_str_at_middle_position_errors_and_reports_first() {
    // 最初の非 Str 引数について報告する（§9.4）。後続の非 Str があっても
    // position は最初のものになる。
    let args = vec![
        Value::Str("a".to_string()),
        Value::Int(123),
        Value::Bool(true),
    ];
    let error = builtin_path_join(&args, 1).expect_err("非 Str 中間引数はエラーになる");
    assert_eq!(error.kind(), Some(ErrorKind::BuiltinType));
    assert_eq!(
        error.message(),
        "path_join の第 2 引数は Str である必要があります: Int"
    );
}

#[test]
fn non_str_at_last_position_errors() {
    let args = vec![Value::Str("a".to_string()), Value::Float(1.5)];
    let error = builtin_path_join(&args, 1).expect_err("非 Str 末尾引数はエラーになる");
    assert_eq!(error.kind(), Some(ErrorKind::BuiltinType));
    assert_eq!(
        error.message(),
        "path_join の第 2 引数は Str である必要があります: Float"
    );
}

#[test]
fn various_non_str_types_report_their_type_name() {
    // 型名は type_name() の一覧に一致する（値そのものは埋め込まない）。
    let cases = [
        (Value::Int(1), "Int"),
        (Value::Float(1.0), "Float"),
        (Value::Bool(false), "Bool"),
        (Value::Null, "Null"),
        (Value::List(std::rc::Rc::new(vec![])), "List"),
    ];
    for (value, type_name) in cases {
        let args = vec![Value::Str("a".to_string()), value];
        let error = builtin_path_join(&args, 1).expect_err("非 Str はエラーになる");
        assert_eq!(error.kind(), Some(ErrorKind::BuiltinType));
        assert_eq!(
            error.message(),
            format!("path_join の第 2 引数は Str である必要があります: {type_name}")
        );
    }
}
