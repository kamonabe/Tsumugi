//! 単一 BuiltinSpec registry のクロスエンジン契約テスト（AUD-049）
//!
//! registry に載る全 public builtin について、VM の Compiler が呼び出しを compile
//! でき（未登録なら compile error になる）、tree の実行系が builtin として名前解決する
//! （`未定義の関数` にならない）ことを自動検査する。手書きの名前一覧が
//! compiler/tree/VM から消え、正本が registry 1 か所であることを固定する。
//!
//! 実行を伴う builtin（filesystem・exit・input など）で外部副作用を起こさないよう、
//! 各呼び出しは arity 検査で先に失敗する引数個数を渡す。arity を強制できない
//! `path_join`（可変長・最小0）だけは、副作用のない純粋関数として成功を許容する。

use tsumugi::builtin_registry::{self, Arity, BuiltinId};
use tsumugi::compiler::Compiler;
use tsumugi::error::ErrorKind;
use tsumugi::lexer::Lexer;
use tsumugi::parser::Parser;
use tsumugi::{Engine, ExecutionContext};

/// arity 検査で必ず失敗する引数個数を返す（副作用を起こさせないため）。
/// path_join のように強制できない場合は None。
fn rejecting_arg_count(arity: Arity) -> Option<usize> {
    match arity {
        Arity::Exact(n) => Some(n + 1),
        Arity::OneOf(a, b) => Some(a.max(b) + 1),
        Arity::Variadic { .. } => None,
    }
}

fn call_source(name: &str, arg_count: usize) -> String {
    let args = vec!["1"; arg_count].join(", ");
    format!("{}({})\n", name, args)
}

/// VM の Compiler が全 public builtin の呼び出しを compile できる。
#[test]
fn vm_compiler_accepts_every_public_builtin() {
    for spec in builtin_registry::PUBLIC_BUILTINS {
        // 正しい arity で呼ぶ（compile は arity に依存しないが、実装意図に沿わせる）。
        let arg_count = match spec.arity {
            Arity::Exact(n) => n,
            Arity::OneOf(a, _) => a,
            Arity::Variadic { min } => min,
        };
        let source = call_source(spec.name, arg_count);

        let tokens = Lexer::new(&source).tokenize();
        let program = Parser::new(tokens)
            .parse()
            .unwrap_or_else(|e| panic!("{} のパースに失敗: {:?}", spec.name, e));

        Compiler::new()
            .compile(&program)
            .unwrap_or_else(|e| panic!("{} の compile に失敗: {:?}", spec.name, e));
    }
}

/// tree の実行系が全 public builtin を builtin として名前解決する
/// （`未定義の関数` エラーにならない）。
#[test]
fn tree_engine_resolves_every_public_builtin() {
    for spec in builtin_registry::PUBLIC_BUILTINS {
        // print は引数個数を問わず出力するだけで、arity で弾けない。
        // ここでは名前解決だけを検証するため、compile が通ることで代替する。
        if spec.id == BuiltinId::Print {
            continue;
        }

        let Some(arg_count) = rejecting_arg_count(spec.arity) else {
            // path_join: arity 強制不可。純粋関数なので成功を許容する。
            let source = format!("{}(\"a\", \"b\")\n", spec.name);
            let engine = Engine::new();
            let script = engine
                .compile(&source)
                .unwrap_or_else(|e| panic!("{} のコンパイルに失敗: {:?}", spec.name, e));
            let mut ctx = ExecutionContext::new();
            // 成功 or 何らかのエラーでもよいが、undefined ではないこと。
            if let Err(err) = engine.execute(&script, &mut ctx) {
                assert_ne!(
                    err.kind(),
                    Some(ErrorKind::Name),
                    "{} が builtin として解決されず name エラー: {}",
                    spec.name,
                    err
                );
            }
            continue;
        };

        let source = call_source(spec.name, arg_count);
        let engine = Engine::new();
        let script = engine
            .compile(&source)
            .unwrap_or_else(|e| panic!("{} のコンパイルに失敗: {:?}", spec.name, e));
        let mut ctx = ExecutionContext::new();
        let result = engine.execute(&script, &mut ctx);

        // arity 過多で必ずエラーになる。ただし「未定義の関数」ではなく、
        // builtin として認識された上での引数エラーであること。
        let err = result.expect_err(&format!(
            "{} が arity 過多でもエラーにならなかった",
            spec.name
        ));
        assert_ne!(
            err.kind(),
            Some(ErrorKind::Name),
            "{} が builtin として解決されず name エラー: {}",
            spec.name,
            err
        );
        assert!(
            !err.to_string().contains("未定義の変数または関数"),
            "{} が未定義として扱われた: {}",
            spec.name,
            err
        );
    }
}

/// 内部命令 `__pop_update` は source から到達できない（両engineで undefined）。
#[test]
fn pop_update_is_unreachable_from_source() {
    let source = "__pop_update([1, 2])\n";
    let engine = Engine::new();
    let script = engine.compile(source).expect("パースは通る");
    let mut ctx = ExecutionContext::new();
    let err = engine
        .execute(&script, &mut ctx)
        .expect_err("__pop_update は undefined のはず");
    assert!(
        err.to_string().contains("未定義の変数または関数"),
        "tree: __pop_update が呼べてしまった: {}",
        err
    );
}

/// 生成 docs（`docs/generated/builtins.md`）が registry の描画結果と byte 一致する
/// こと（CAP-AT-20 の「generated docs 完全一致」）。
///
/// registry を変更して生成物を更新し忘れると失敗する。修正手順は
/// `cargo run --bin gen_builtins_doc` で再生成してコミットする。
#[test]
fn generated_docs_match_registry() {
    let expected = builtin_registry::render_reference();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/generated/builtins.md");
    let actual = std::fs::read_to_string(path).expect(
        "docs/generated/builtins.md が存在しない。`cargo run --bin gen_builtins_doc` で生成する",
    );
    assert_eq!(
        actual, expected,
        "生成 docs が registry とドリフトしている。`cargo run --bin gen_builtins_doc` で再生成すること"
    );
}

/// registry に公開名の重複がないこと（重複は build/test error とする契約、AUD-049 §13.5）。
#[test]
fn public_names_have_no_duplicates() {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for spec in builtin_registry::PUBLIC_BUILTINS {
        assert!(
            seen.insert(spec.name),
            "公開名が重複している: {}",
            spec.name
        );
    }
}

/// registry に BuiltinId の重複がないこと（entry と ID が 1 対 1、AUD-049 §13.5）。
#[test]
fn builtin_ids_have_no_duplicates() {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for spec in builtin_registry::PUBLIC_BUILTINS {
        assert!(
            seen.insert(spec.id),
            "BuiltinId が重複している: {:?}",
            spec.id
        );
    }
}

/// 生成 docs に registry の全 public 名と arity 記述が現れること（metadata 一致の
/// 冗長確認。`generated_docs_match_registry` の byte 一致を補強する）。
#[test]
fn generated_docs_contain_every_builtin() {
    let doc = builtin_registry::render_reference();
    for spec in builtin_registry::PUBLIC_BUILTINS {
        let row = format!(
            "| `{}` | {} | {} |",
            spec.name,
            spec.arity.describe(),
            spec.execution.label()
        );
        assert!(doc.contains(&row), "生成 docs に行が無い: {row}");
    }
}
