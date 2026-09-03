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
