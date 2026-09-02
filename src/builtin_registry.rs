//! 単一 BuiltinSpec registry（AUD-049）
//!
//! language から呼べる組み込み関数の**正本**をここに一元化する。以前は
//! `builtin_core.rs` の `dispatch`、`builtin.rs` の `match name`、
//! `compiler.rs` の `is_builtin()` の3か所へ手書きの名前一覧が分散しており、
//! 追加時に1か所でも漏らすと「tree では呼べるが VM では呼べない」状態を
//! compile error ではなく実行時の `name` エラーとして持ち越していた。
//!
//! 本モジュール導入後は、tree の名前解決・委譲判定、Compiler の builtin 判定、
//! VM の dispatch 分類、arity・context metadata をすべてこの registry から導出する。
//! 新しい builtin を足す手順は「handler 実装 + [`BuiltinSpec`] 1 entry + tests」に
//! 一本化される。
//!
//! 内部命令 `__pop_update` は public registry へ置かず、Compiler の pop lowering
//! 専用の [`crate::opcode::OpCode::PopUpdate`] 命令として実装する。そのため
//! source からは到達できない。

/// public builtin の安定 ID。
///
/// VM の [`crate::opcode::OpCode::CallBuiltin`] は名前文字列ではなくこの ID を
/// 保持する。typo を runtime まで持ち越さず、compile 時に registry と突き合わせる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinId {
    // --- コレクション操作 ---
    Len,
    Push,
    Pop,
    Keys,
    Values,
    HasKey,
    Type,
    Slice,
    Contains,
    Sort,
    Reverse,
    Range,
    // --- 文字列操作 ---
    Split,
    Join,
    Trim,
    Upper,
    Lower,
    StartsWith,
    EndsWith,
    Replace,
    // --- 型変換・数値 ---
    ToInt,
    ToStr,
    ToFloat,
    Abs,
    Min,
    Max,
    Floor,
    Ceil,
    Round,
    // --- 日時 ---
    Now,
    FormatTime,
    // --- I/O・process（コンテキスト依存） ---
    Print,
    Input,
    Args,
    Exit,
    // --- 高階関数（コンテキスト依存） ---
    Map,
    Filter,
    Each,
    // --- ファイルI/O ---
    ReadFile,
    ReadLines,
    WriteFile,
    AppendFile,
    // --- 環境 ---
    Env,
    // --- パス・ファイルシステム ---
    PathExists,
    PathJoin,
    Mkdir,
    Remove,
    RemoveDir,
    Rename,
    ListDir,
    FileSize,
    IsFile,
    IsDir,
}

/// builtin の実行分類。
///
/// - [`Execution::PureCore`]: 評価済み `&[Value]` から純粋に計算でき、
///   `builtin_core::dispatch` で両 engine が同じ実装を共有する。
/// - [`Execution::Context`]: 実行コンテキスト（stdio・argv・変数 binding・
///   closure 呼び出し）を必要とし、各 engine が固有実装を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Execution {
    PureCore,
    Context,
}

/// builtin の引数個数契約（arity）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// ちょうど n 個
    Exact(usize),
    /// n 個または m 個（例: `exit` は 0 または 1）
    OneOf(usize, usize),
    /// 最小 min 個以上の可変長（例: `path_join`）
    Variadic { min: usize },
}

impl Arity {
    /// 実際の引数個数が arity 契約を満たすか。
    pub fn accepts(&self, actual: usize) -> bool {
        match self {
            Arity::Exact(n) => actual == *n,
            Arity::OneOf(a, b) => actual == *a || actual == *b,
            Arity::Variadic { min } => actual >= *min,
        }
    }
}

/// public builtin 1 個分の宣言。
#[derive(Debug, Clone, Copy)]
pub struct BuiltinSpec {
    pub id: BuiltinId,
    /// source から呼ぶ名前。`print` は lexer 上の予約 token だが registry entry も持つ。
    pub name: &'static str,
    pub arity: Arity,
    pub execution: Execution,
}

use Arity::{Exact, OneOf, Variadic};
use BuiltinId as B;
use Execution::{Context, PureCore};

/// public builtin の正本一覧。
///
/// この配列が language-visible builtin 名の唯一の source of truth である。
/// tree / VM / compiler はここから導出し、手書きの名前一覧を持たない。
pub const PUBLIC_BUILTINS: &[BuiltinSpec] = &[
    // --- コレクション操作 ---
    BuiltinSpec {
        id: B::Len,
        name: "len",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Push,
        name: "push",
        arity: Exact(2),
        execution: Context,
    },
    BuiltinSpec {
        id: B::Pop,
        name: "pop",
        arity: Exact(1),
        execution: Context,
    },
    BuiltinSpec {
        id: B::Keys,
        name: "keys",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Values,
        name: "values",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::HasKey,
        name: "has_key",
        arity: Exact(2),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Type,
        name: "type",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Slice,
        name: "slice",
        arity: Exact(3),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Contains,
        name: "contains",
        arity: Exact(2),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Sort,
        name: "sort",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Reverse,
        name: "reverse",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Range,
        name: "range",
        arity: Exact(2),
        execution: PureCore,
    },
    // --- 文字列操作 ---
    BuiltinSpec {
        id: B::Split,
        name: "split",
        arity: Exact(2),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Join,
        name: "join",
        arity: Exact(2),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Trim,
        name: "trim",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Upper,
        name: "upper",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Lower,
        name: "lower",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::StartsWith,
        name: "starts_with",
        arity: Exact(2),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::EndsWith,
        name: "ends_with",
        arity: Exact(2),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Replace,
        name: "replace",
        arity: Exact(3),
        execution: PureCore,
    },
    // --- 型変換・数値 ---
    BuiltinSpec {
        id: B::ToInt,
        name: "to_int",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::ToStr,
        name: "to_str",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::ToFloat,
        name: "to_float",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Abs,
        name: "abs",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Min,
        name: "min",
        arity: Exact(2),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Max,
        name: "max",
        arity: Exact(2),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Floor,
        name: "floor",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Ceil,
        name: "ceil",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Round,
        name: "round",
        arity: Exact(1),
        execution: PureCore,
    },
    // --- 日時 ---
    BuiltinSpec {
        id: B::Now,
        name: "now",
        arity: Exact(0),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::FormatTime,
        name: "format_time",
        arity: Exact(2),
        execution: PureCore,
    },
    // --- I/O・process（コンテキスト依存） ---
    BuiltinSpec {
        id: B::Print,
        name: "print",
        arity: Exact(0),
        execution: Context,
    },
    BuiltinSpec {
        id: B::Input,
        name: "input",
        arity: Exact(0),
        execution: Context,
    },
    BuiltinSpec {
        id: B::Args,
        name: "args",
        arity: Exact(0),
        execution: Context,
    },
    BuiltinSpec {
        id: B::Exit,
        name: "exit",
        arity: OneOf(0, 1),
        execution: Context,
    },
    // --- 高階関数（コンテキスト依存） ---
    BuiltinSpec {
        id: B::Map,
        name: "map",
        arity: Exact(2),
        execution: Context,
    },
    BuiltinSpec {
        id: B::Filter,
        name: "filter",
        arity: Exact(2),
        execution: Context,
    },
    BuiltinSpec {
        id: B::Each,
        name: "each",
        arity: Exact(2),
        execution: Context,
    },
    // --- ファイルI/O ---
    BuiltinSpec {
        id: B::ReadFile,
        name: "read_file",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::ReadLines,
        name: "read_lines",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::WriteFile,
        name: "write_file",
        arity: Exact(2),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::AppendFile,
        name: "append_file",
        arity: Exact(2),
        execution: PureCore,
    },
    // --- 環境 ---
    BuiltinSpec {
        id: B::Env,
        name: "env",
        arity: Exact(1),
        execution: PureCore,
    },
    // --- パス・ファイルシステム ---
    BuiltinSpec {
        id: B::PathExists,
        name: "path_exists",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::PathJoin,
        name: "path_join",
        arity: Variadic { min: 0 },
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Mkdir,
        name: "mkdir",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Remove,
        name: "remove",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::RemoveDir,
        name: "remove_dir",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::Rename,
        name: "rename",
        arity: Exact(2),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::ListDir,
        name: "list_dir",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::FileSize,
        name: "file_size",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::IsFile,
        name: "is_file",
        arity: Exact(1),
        execution: PureCore,
    },
    BuiltinSpec {
        id: B::IsDir,
        name: "is_dir",
        arity: Exact(1),
        execution: PureCore,
    },
];

/// 名前から public BuiltinSpec を引く唯一の入口。
pub fn lookup_public(name: &str) -> Option<&'static BuiltinSpec> {
    PUBLIC_BUILTINS.iter().find(|spec| spec.name == name)
}

/// public builtin 名かどうか（Compiler の builtin 判定に使う）。
pub fn is_public_builtin(name: &str) -> bool {
    lookup_public(name).is_some()
}

/// 実行コンテキストを必要とする builtin かどうか。
pub fn is_context_builtin(name: &str) -> bool {
    lookup_public(name).is_some_and(|spec| spec.execution == Execution::Context)
}

/// PureCore builtin（`builtin_core::dispatch` へ委譲できる）かどうか。
pub fn is_pure_core_builtin(name: &str) -> bool {
    lookup_public(name).is_some_and(|spec| spec.execution == Execution::PureCore)
}

/// 名前から BuiltinId を引く（Compiler の opcode 生成に使う）。
pub fn id_of(name: &str) -> Option<BuiltinId> {
    lookup_public(name).map(|spec| spec.id)
}

/// BuiltinId から名前を引く（VM の handler dispatch に使う）。
pub fn name_of(id: BuiltinId) -> &'static str {
    // id ごとに一意な entry が存在することは contract test で保証する。
    PUBLIC_BUILTINS
        .iter()
        .find(|spec| spec.id == id)
        .map(|spec| spec.name)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// public builtin 名の正本が1つだけであること（重複した名前を許さない）。
    #[test]
    fn public_builtin_names_are_unique() {
        let mut seen = HashSet::new();
        for spec in PUBLIC_BUILTINS {
            assert!(
                seen.insert(spec.name),
                "public builtin 名が重複しています: {}",
                spec.name
            );
        }
    }

    /// BuiltinId が重複しないこと（entry と ID が 1 対 1）。
    #[test]
    fn public_builtin_ids_are_unique() {
        let mut seen = HashSet::new();
        for spec in PUBLIC_BUILTINS {
            assert!(
                seen.insert(spec.id),
                "BuiltinId が重複しています: {:?}",
                spec.id
            );
        }
    }

    /// 全 entry で name -> id -> name が往復すること。
    #[test]
    fn name_and_id_round_trip() {
        for spec in PUBLIC_BUILTINS {
            assert_eq!(id_of(spec.name), Some(spec.id), "id_of({})", spec.name);
            assert_eq!(name_of(spec.id), spec.name, "name_of({:?})", spec.id);
            assert!(is_public_builtin(spec.name));
            assert!(lookup_public(spec.name).is_some());
        }
    }

    /// public 名に内部用 prefix `__` を持つものが混ざらないこと。
    #[test]
    fn no_public_name_uses_internal_prefix() {
        for spec in PUBLIC_BUILTINS {
            assert!(
                !spec.name.starts_with("__"),
                "public builtin に内部 prefix が付いています: {}",
                spec.name
            );
        }
    }

    /// 内部命令 `__pop_update` は public registry から到達不能であること（AUD-049）。
    #[test]
    fn pop_update_is_not_public() {
        assert!(!is_public_builtin("__pop_update"));
        assert!(lookup_public("__pop_update").is_none());
        assert!(id_of("__pop_update").is_none());
        assert!(!is_context_builtin("__pop_update"));
        assert!(!is_pure_core_builtin("__pop_update"));
    }

    /// context / pure-core の分類が execution フィールドと矛盾せず、全 entry を
    /// ちょうど一方へ分けること。
    #[test]
    fn execution_partition_is_consistent() {
        for spec in PUBLIC_BUILTINS {
            let ctx = is_context_builtin(spec.name);
            let pure = is_pure_core_builtin(spec.name);
            assert_ne!(ctx, pure, "{} が context/pure の両方または両否", spec.name);
            match spec.execution {
                Execution::Context => assert!(ctx, "{} は Context のはず", spec.name),
                Execution::PureCore => assert!(pure, "{} は PureCore のはず", spec.name),
            }
        }
    }

    /// context builtin の集合が期待どおりであること（実行コンテキストを要する 8 個）。
    #[test]
    fn context_builtins_match_expected_set() {
        let mut ctx: Vec<&str> = PUBLIC_BUILTINS
            .iter()
            .filter(|s| s.execution == Execution::Context)
            .map(|s| s.name)
            .collect();
        ctx.sort_unstable();
        assert_eq!(
            ctx,
            vec![
                "args", "each", "exit", "filter", "input", "map", "pop", "print", "push"
            ]
        );
    }

    /// Arity 契約が well-formed であること（OneOf は相異なる 2 値など）。
    #[test]
    fn arity_is_well_formed() {
        for spec in PUBLIC_BUILTINS {
            match spec.arity {
                Arity::Exact(_) => {}
                Arity::OneOf(a, b) => assert_ne!(a, b, "{} の OneOf が同値", spec.name),
                Arity::Variadic { .. } => {}
            }
        }
    }

    #[test]
    fn arity_accepts_behaves() {
        assert!(Arity::Exact(2).accepts(2));
        assert!(!Arity::Exact(2).accepts(1));
        assert!(Arity::OneOf(0, 1).accepts(0));
        assert!(Arity::OneOf(0, 1).accepts(1));
        assert!(!Arity::OneOf(0, 1).accepts(2));
        assert!(Arity::Variadic { min: 0 }.accepts(0));
        assert!(Arity::Variadic { min: 1 }.accepts(5));
        assert!(!Arity::Variadic { min: 2 }.accepts(1));
    }
}
