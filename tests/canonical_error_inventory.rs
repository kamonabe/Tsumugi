//! AUD-019: canonical error 契約の inventory テスト。
//!
//! [`docs/semantic-decisions.md`] 第3.4節の operation 表のうち、現行言語コアから
//! 到達できる各行について、tree-walk 評価器と VM が同一の `(kind, message, line)` を
//! 返すことを固定する。あわせて、対象経路が「任意文字列からの kind 推測」に頼る
//! `ErrorKind::Runtime` を生成しないことを保証する（3.11 受入基準）。
//!
//! Phase 2 以降でしか到達しない行（capability / host / budget / timeout / class）は
//! 現行実装に経路が無いため対象外とする。

use tsumugi::error::{ErrorKind, TsumugiError};

/// ソースを tree-walk 評価器で実行し、最初の runtime error を返す。
fn run_tree(source: &str) -> TsumugiError {
    let tokens = tsumugi::lexer::Lexer::new(source).tokenize();
    let program = tsumugi::parser::Parser::new(tokens)
        .parse()
        .unwrap_or_else(|errors| panic!("パースに失敗: {errors:?}"));
    let mut evaluator = tsumugi::eval::Evaluator::new();
    evaluator
        .run(&program)
        .expect_err("tree: エラーになるはずのソースが成功した")
}

/// ソースを VM で実行し、最初の runtime error を返す。
fn run_vm(source: &str) -> TsumugiError {
    let tokens = tsumugi::lexer::Lexer::new(source).tokenize();
    let program = tsumugi::parser::Parser::new(tokens)
        .parse()
        .unwrap_or_else(|errors| panic!("パースに失敗: {errors:?}"));
    // import を実行前に解決する（AUD-030）。import の失敗も canonical error として
    // 返るため、tree の `run()` と同じ経路をたどる。
    let mut loader = tsumugi::module::ModuleLoader::new();
    let (linked, _) = match loader.link(&program) {
        Ok(linked) => linked,
        Err(error) => return error,
    };
    let target = linked.as_ref().unwrap_or(&program);
    // VM は break/continue のループ外使用や arity などを compile 時に検出する。
    // tree は同じ診断を実行時に返すが、canonical な kind/message/line は一致する。
    let chunk = match tsumugi::compiler::Compiler::new().compile(target) {
        Ok(chunk) => chunk,
        Err(error) => return error,
    };
    tsumugi::vm::Vm::new(chunk)
        .run()
        .expect_err("VM: エラーになるはずのソースが成功した")
}

fn parts(error: &TsumugiError) -> (Option<ErrorKind>, String, usize) {
    (error.kind(), error.message().to_string(), error.line())
}

/// operation 表の各行を、両 engine で同一の kind/message/line にする。
///
/// 各要素は `(ラベル, ソース, 期待kind, 期待message, 期待line)`。
/// 期待値はテンプレートの完全一致で固定する。
struct Case {
    label: &'static str,
    source: &'static str,
    kind: ErrorKind,
    message: &'static str,
    line: usize,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "変数read未定義",
            source: "print(missing)\n",
            kind: ErrorKind::Name,
            message: "未定義の変数または関数: missing",
            line: 1,
        },
        Case {
            label: "callee名未定義",
            source: "missing(1)\n",
            kind: ErrorKind::Name,
            message: "未定義の変数または関数: missing",
            line: 1,
        },
        Case {
            label: "未定義変数への代入",
            source: "x = 1\n",
            kind: ErrorKind::Name,
            message: "未定義の変数に代入: x",
            line: 1,
        },
        Case {
            label: "ゼロ除算",
            source: "let x = 1 / 0\n",
            kind: ErrorKind::ZeroDivision,
            message: "ゼロ除算",
            line: 1,
        },
        Case {
            label: "剰余のゼロ除算",
            source: "let x = 1 % 0\n",
            kind: ErrorKind::ZeroDivision,
            message: "ゼロ除算",
            line: 1,
        },
        Case {
            label: "算術型不正",
            source: "let x = \"a\" - 1\n",
            kind: ErrorKind::Type,
            message: "演算子 - は Str と Int に適用できません",
            line: 1,
        },
        Case {
            label: "文字列と数値の加算",
            source: "let x = \"a\" + 1\n",
            kind: ErrorKind::Type,
            message: "演算子 + は Str と Int に適用できません",
            line: 1,
        },
        Case {
            label: "単項マイナスの型不正",
            source: "let x = -\"a\"\n",
            kind: ErrorKind::Type,
            message: "演算子 - は Str に適用できません",
            line: 1,
        },
        Case {
            label: "大小比較の型不正",
            source: "let x = \"a\" < 1\n",
            kind: ErrorKind::Type,
            message: "比較演算子 < は Str と Int に適用できません",
            line: 1,
        },
        Case {
            label: "List indexが非Int",
            source: "let xs = [1]\nlet y = xs[\"a\"]\n",
            kind: ErrorKind::Type,
            message: "List のインデックスは Int である必要があります: Str",
            line: 2,
        },
        Case {
            label: "Str indexが非Int",
            source: "let s = \"a\"\nlet y = s[\"a\"]\n",
            kind: ErrorKind::Type,
            message: "Str のインデックスは Int である必要があります: Str",
            line: 2,
        },
        Case {
            label: "Dict keyが非Str",
            source: "let d = {}\nlet y = d[1]\n",
            kind: ErrorKind::Type,
            message: "Dict のキーは Str である必要があります: Int",
            line: 2,
        },
        Case {
            label: "List index範囲外",
            source: "let xs = [1, 2]\nlet y = xs[5]\n",
            kind: ErrorKind::Index,
            message: "List のインデックスが範囲外です: 5 (長さ: 2)",
            line: 2,
        },
        Case {
            label: "Str index範囲外",
            source: "let s = \"ab\"\nlet y = s[5]\n",
            kind: ErrorKind::Index,
            message: "Str のインデックスが範囲外です: 5 (長さ: 2)",
            line: 2,
        },
        Case {
            label: "index read非対応型",
            source: "let n = 42\nlet y = n[0]\n",
            kind: ErrorKind::Type,
            message: "インデックスアクセスできない型です: Int",
            line: 2,
        },
        Case {
            label: "index assignment非対応型",
            source: "let n = 42\nn[0] = 1\n",
            kind: ErrorKind::Type,
            message: "インデックス代入できない型です: Int",
            line: 2,
        },
        Case {
            label: "反復非対応型",
            source: "for x in 42\n    print(x)\nend\n",
            kind: ErrorKind::Iteration,
            message: "反復できない型です: Int",
            line: 1,
        },
        Case {
            label: "非callableの呼び出し",
            source: "let n = 42\nn()\n",
            kind: ErrorKind::Type,
            message: "呼び出せない型です: Int",
            line: 2,
        },
        Case {
            label: "user関数arity",
            source: "fn f(a, b)\n    return a\nend\nf(1)\n",
            kind: ErrorKind::Argument,
            message: "f の引数個数が一致しません: 期待 2, 実際 1",
            line: 4,
        },
        Case {
            label: "builtin arity",
            source: "len(1, 2)\n",
            kind: ErrorKind::Argument,
            message: "len の引数個数が一致しません: 期待 1, 実際 2",
            line: 1,
        },
        Case {
            label: "builtin argument型",
            source: "len(42)\n",
            kind: ErrorKind::BuiltinType,
            message: "len の第 1 引数は List/Str/Dict である必要があります: Int",
            line: 1,
        },
        Case {
            label: "callback非callable",
            source: "map([1], 42)\n",
            kind: ErrorKind::Type,
            message: "map のコールバックは呼び出し可能である必要があります: Int",
            line: 1,
        },
        Case {
            label: "callback arity",
            source: "fn two(a, b)\n    return a\nend\nmap([1], two)\n",
            kind: ErrorKind::Argument,
            message: "map のコールバック引数個数が一致しません: 期待 1, 実際 2",
            line: 4,
        },
        Case {
            label: "push対象が変数でない",
            source: "push([1], 2)\n",
            kind: ErrorKind::BuiltinType,
            message: "push の第 1 引数には List 変数を指定してください",
            line: 1,
        },
        Case {
            label: "空Listへのpop",
            source: "let xs = []\npop(xs)\n",
            kind: ErrorKind::BuiltinType,
            message: "pop は空の List には使用できません",
            line: 2,
        },
        Case {
            label: "loop外のbreak",
            source: "break\n",
            kind: ErrorKind::ControlFlow,
            message: "break はループの中でのみ使用できます",
            line: 1,
        },
        Case {
            label: "loop外のcontinue",
            source: "continue\n",
            kind: ErrorKind::ControlFlow,
            message: "continue はループの中でのみ使用できます",
            line: 1,
        },
        Case {
            label: "整数演算overflow(加算)",
            source: "let x = 9223372036854775807 + 1\n",
            kind: ErrorKind::IntOverflow,
            message: "整数オーバーフロー: 加算",
            line: 1,
        },
        Case {
            label: "整数演算overflow(乗算)",
            source: "let x = 9223372036854775807 * 2\n",
            kind: ErrorKind::IntOverflow,
            message: "整数オーバーフロー: 乗算",
            line: 1,
        },
        Case {
            label: "大小比較(>=)の型不正",
            source: "let x = null >= 1\n",
            kind: ErrorKind::Type,
            message: "比較演算子 >= は Null と Int に適用できません",
            line: 1,
        },
        Case {
            label: "min の第2引数型不正",
            source: "min(1, \"a\")\n",
            kind: ErrorKind::BuiltinType,
            message: "min の第 2 引数は Int/Float である必要があります: Str",
            line: 1,
        },
        Case {
            label: "builtin argument型(第2引数)",
            source: "has_key({}, 1)\n",
            kind: ErrorKind::BuiltinType,
            message: "has_key の第 2 引数は Str である必要があります: Int",
            line: 1,
        },
        Case {
            label: "to_int の変換失敗",
            source: "to_int(\"abc\")\n",
            kind: ErrorKind::Conversion,
            message: "to_int で Int に変換できません: 数値として解釈できません",
            line: 1,
        },
        Case {
            label: "コレクション以外のindex代入(list index非Int)",
            source: "let xs = [1]\nxs[\"a\"] = 1\n",
            kind: ErrorKind::Type,
            message: "List のインデックスは Int である必要があります: Str",
            line: 2,
        },
    ]
}

#[test]
fn tree_and_vm_produce_identical_canonical_errors() {
    for case in cases() {
        let tree = run_tree(case.source);
        let vm = run_vm(case.source);

        let (tree_kind, tree_msg, tree_line) = parts(&tree);
        let (vm_kind, vm_msg, vm_line) = parts(&vm);

        // tree と VM が完全一致する（kind / message / line）
        assert_eq!(
            (tree_kind, tree_msg.as_str(), tree_line),
            (vm_kind, vm_msg.as_str(), vm_line),
            "{}: tree と VM の error が一致しない",
            case.label
        );

        // canonical テンプレートに完全一致する
        assert_eq!(tree_kind, Some(case.kind), "{}: kind が不一致", case.label);
        assert_eq!(tree_msg, case.message, "{}: message が不一致", case.label);
        assert_eq!(tree_line, case.line, "{}: line が不一致", case.label);

        // 任意文字列からの kind 推測（Runtime catch-all）を対象経路で生成しない
        assert_ne!(
            tree_kind,
            Some(ErrorKind::Runtime),
            "{}: 対象経路が Runtime catch-all を生成した",
            case.label
        );
    }
}

/// 表に載る全 operation について、少なくとも 1 つの paired ケースが存在することを確認する。
/// ケース数が減った場合に気付けるようにするための下限ガード。
#[test]
fn inventory_covers_reachable_operations() {
    assert!(
        cases().len() >= 34,
        "inventory ケースが想定より少ない: {}",
        cases().len()
    );
}

// =============================================================
// trace 軸: スタックトレースを含む Display 全文が tree/VM で一致する（3.6）
// =============================================================

#[test]
fn multi_level_trace_is_identical_across_engines() {
    // divide → calc の2段呼び出し。trace は内側から外側へ、各frameの行は
    // 呼び出し元のcall-site行になる（3.6）。
    let source = "fn divide(a, b)\n\
                  return a / b\n\
                  end\n\
                  fn calc(x)\n\
                  return divide(x, 0)\n\
                  end\n\
                  calc(10)\n";
    let tree = run_tree(source);
    let vm = run_vm(source);

    let expected = "2行目: ゼロ除算\n  in divide() (5行目)\n  in calc() (7行目)";
    assert_eq!(tree.to_string(), expected, "tree の trace 全文が不一致");
    assert_eq!(vm.to_string(), expected, "VM の trace 全文が不一致");
}

#[test]
fn callback_trace_preserves_origin_error() {
    // callback body 内で発生したエラーは、callback専用messageへ包み直さず
    // 元のkind/message/lineを保持する（3.4 / 3.6）。map自体はframeに出さない。
    let source = "fn boom(x)\n\
                  return x / 0\n\
                  end\n\
                  map([1], boom)\n";
    let tree = run_tree(source);
    let vm = run_vm(source);

    assert_eq!(tree.kind(), Some(ErrorKind::ZeroDivision));
    let tree_str = tree.to_string();
    assert_eq!(tree_str, vm.to_string(), "callback trace が不一致");
    // callback の user function frame は通常関数と同じく追加される
    assert!(
        tree_str.contains("in boom()"),
        "callback frame が trace に無い: {tree_str}"
    );
}

// =============================================================
// catch 軸: caught error の type/message/line が tree/VM で一致する（3.6）
// =============================================================

#[test]
fn caught_error_fields_are_identical_across_engines() {
    // catch 変数 `e` の `e["type"]` / `e["message"]` / `e["line"]` は、未捕捉時の
    // error の kind/message/line と同じ値を読む（`Value::Error` に写し取られる）。
    // ここでは caller frame で発生させた error の origin フィールドが tree/VM で
    // 一致することを、未捕捉 error の値経由で確認する。
    // （catch 済み値の stdout 突合は `error_structured` フィクスチャが担う。）
    let source = "fn f()\n\
                  return \"a\" + 1\n\
                  end\n\
                  f()\n";
    let tree = run_tree(source);
    let vm = run_vm(source);

    // Value::Error へ写し取られる3フィールドが tree/VM で一致する
    assert_eq!(
        tree.error_type(),
        vm.error_type(),
        "type フィールドが不一致"
    );
    assert_eq!(tree.message(), vm.message(), "message フィールドが不一致");
    assert_eq!(tree.line(), vm.line(), "line フィールドが不一致");

    // origin line は関数本体の行（2行目）で、呼び出し元(4行目)ではない
    assert_eq!(tree.error_type(), "type");
    assert_eq!(tree.message(), "演算子 + は Str と Int に適用できません");
    assert_eq!(tree.line(), 2);
}

// =============================================================
// line 軸: エラー発生位置の行番号が tree/VM で一致する（3.5）
//
// Tsumugi の文法は行志向で、式は1行に収まる。そのため「式の開始行」と
// 「文の行」は一致する。ここでは後続文・ネストしたブロック本体・ループ本体で
// 正しい行が両 engine で一致することを固定する。
// =============================================================

#[test]
fn error_line_matches_across_engines_in_various_positions() {
    struct LineCase {
        label: &'static str,
        source: &'static str,
        line: usize,
    }

    let cases = [
        LineCase {
            label: "後続文",
            source: "let a = 1\nlet b = 2\nlet c = \"x\" + 1\n",
            line: 3,
        },
        LineCase {
            label: "if本体",
            source: "if true\n    let x = \"x\" + 1\nend\n",
            line: 2,
        },
        LineCase {
            label: "whileループ本体",
            source: "while true\n    let x = \"x\" + 1\nend\n",
            line: 2,
        },
        LineCase {
            label: "forループ本体",
            source: "for i in [1]\n    let x = \"x\" + 1\nend\n",
            line: 2,
        },
        LineCase {
            label: "関数本体(origin line)",
            source: "fn f()\n    return \"x\" + 1\nend\nf()\n",
            line: 2,
        },
    ];

    for case in cases {
        let tree = run_tree(case.source);
        let vm = run_vm(case.source);

        assert_eq!(
            (tree.kind(), tree.message(), tree.line()),
            (vm.kind(), vm.message(), vm.line()),
            "{}: error が tree/VM で不一致",
            case.label
        );
        assert_eq!(tree.line(), case.line, "{}: line が不一致", case.label);
        assert_eq!(
            tree.kind(),
            Some(ErrorKind::Type),
            "{}: kind 不一致",
            case.label
        );
    }
}

// =============================================================
// import 軸: import 解決失敗の canonical error が tree/VM で一致する（3.4）
// =============================================================

#[test]
fn import_errors_are_identical_across_engines() {
    struct ImportCase {
        label: &'static str,
        source: &'static str,
        kind: ErrorKind,
        message: &'static str,
        line: usize,
    }

    let cases = [
        ImportCase {
            label: "存在しないモジュール",
            source: "import \"tests/fixtures/does_not_exist.tsg\"\n",
            kind: ErrorKind::Import,
            message: "import に失敗しました: モジュールを読み込めません: tests/fixtures/does_not_exist.tsg",
            line: 1,
        },
        ImportCase {
            label: "構文が不正なモジュール",
            source: "import \"tests/fixtures/import_bad_syntax.tsg\"\n",
            kind: ErrorKind::Import,
            message: "import に失敗しました: モジュールの構文が不正です: tests/fixtures/import_bad_syntax.tsg",
            line: 1,
        },
    ];

    for case in cases {
        let tree = run_tree(case.source);
        let vm = run_vm(case.source);

        assert_eq!(
            (tree.kind(), tree.message(), tree.line()),
            (vm.kind(), vm.message(), vm.line()),
            "{}: import error が tree/VM で不一致",
            case.label
        );
        assert_eq!(tree.kind(), Some(case.kind), "{}: kind 不一致", case.label);
        assert_eq!(
            tree.message(),
            case.message,
            "{}: message 不一致",
            case.label
        );
        assert_eq!(tree.line(), case.line, "{}: line 不一致", case.label);
    }
}
