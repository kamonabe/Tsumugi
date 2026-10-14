//! StableError — backend 非依存の安定した構造化 error（決定性 Slice 1）。
//!
//! 正本: [`docs/determinism-and-audit.md`] §4.6 / §14 Slice 1、
//! [`docs/semantic-decisions.md`] §3.4 / §3.5 / §3.6。
//!
//! `StableError` は §4.6 が規定する「backend 間で一致する code / script_type / message_id /
//! source 位置 / trace frame 順を持つ error」を Tsumugi に実装する primitive である。本実装の
//! 本体は「新しい値 domain を作ること」ではなく、既存の正本（[`crate::error::ErrorKind`] /
//! `error.rs` の canonical constructor / [`crate::error::TraceFrame`]）から `StableError` へ
//! **決定的に射影する関数を 1 本用意し、二重管理を発生させないこと**にある。
//!
//! # 設計の要点
//!
//! - [`ErrorCode`] は独立 enum を作らず [`crate::error::ErrorKind`] を正本として型エイリアス
//!   再利用する（[`crate::embedding::ExecutionError`] `code` の前例に倣う）。`code` が
//!   機械可読の正本、`script_type` はその投影（`ErrorKind::as_str()`）。
//! - [`ErrorMessageId`] は per-template で採番する（安定 ASCII キー）。1 kind が複数テンプレートを
//!   持つ kind（クラス B）は canonical constructor が [`message_id`] 定数を内部タグとして載せ、
//!   kind だけで一意に決まる kind（クラス A）は射影側 [`message_id_for`] が kind から補完する
//!   二層 resolver を使う。これにより [`crate::error::TsumugiError::runtime_with_kind`] の
//!   signature を変えずに済む（error.rs 外の直接呼び出し元に影響しない）。
//! - uncatchable terminal（budget / deadline / cancel / internal-control / process-exit）は
//!   [`crate::error::ErrorKind::is_uncatchable`] を唯一の判定正本として射影しない（`None`）。
//! - [`StableFrame`] は [`crate::error::TraceFrame`] の順序・内容をそのまま射影する
//!   （`line: usize -> u32` は飽和変換）。root script frame は含まれない（tree の `call_stack`
//!   が user 関数 call 時のみ frame を push し root `<main>` を合成しないため、射影元 trace に
//!   そもそも入らない。eval.rs の push 箇所で確認済み）。
//!
//! 本 Slice の射程は「tree backend 上で StableError を決定的に生成できる primitive の確立」に
//! 限る。VM parity / FakeHost gate / audit normalize / record-replay は後続スライス（§11）。

use crate::embedding::ModuleId;
use crate::error::{ErrorKind, TraceFrame, TsumugiError};

/// StableError の機械可読 error code（§4.6）。
///
/// 独立 enum を作らず [`crate::error::ErrorKind`] を正本として再利用する型エイリアス。安定型
/// 文字列は `ErrorKind::as_str()` が既に持ち、`script_type` もこれを使うため、別 enum を作ると
/// 二重管理になる。`ErrorKind` は `Copy + Eq + Hash` で決定的比較にそのまま使える。
pub type ErrorCode = ErrorKind;

/// per-template の安定 message ID（§4.6 / §2）。
///
/// canonical message テンプレート 1 個に 1 ID を対応させる。値は locale 非依存の ASCII
/// snake-case キーで、message テンプレートの文言変更から ID を分離する（将来の message 変更
/// からの isolation）。audit は raw message ではなく code / message ID を使う（§4.6）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ErrorMessageId(&'static str);

impl ErrorMessageId {
    /// 安定 ID 文字列を返す。
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// script trace の 1 フレーム（§3.6 / §4.6）。
///
/// [`crate::error::TraceFrame`] から射影する（`name -> function`、`line: usize -> u32` は飽和
/// 変換）。frame の意味論（どの関数を内側から外側へ並べるか、line は caller call-site 行、
/// bound method 名 `{Class}.{method}` 等）は §3.6 の trace 規則が正本で、`error::TraceFrame` が
/// 既にそれに従って生成されている。StableFrame はその順序・内容をそのまま保持し、並べ替え・
/// 再計算をしない。
///
/// `error::TraceFrame.name` の doc は「トップレベルなら `<main>`」と記すが、tree の `call_stack`
/// （eval.rs）は user 関数 call 時のみ frame を push し root `<main>` を合成しないため、射影元
/// trace に `<main>` は入らない（§3.6 どおり root script frame は trace に含まれない）。
///
/// audit 層の `AuditFrame`（redaction 適用済みの監査専用型）とは別型であり、StableFrame →
/// AuditFrame 変換は Slice 5（audit normalize）の管轄で本 Slice では行わない。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableFrame {
    /// 関数名（§3.6 の命名規則どおり既存 trace が決めたものをコピー）。
    pub function: String,
    /// その frame を呼び出した caller 側 call-site の行番号（§3.6）。
    pub line: u32,
}

/// backend 非依存の安定した構造化 error（§4.6）。
///
/// `code` / `script_type` / `message_id` / source 位置 / `trace` frame 順を backend 間で一致
/// させる。`script_message` も try/catch から観測可能なため同じ日本語テンプレートと値
/// serialization を共有する。内部 Rust 型名や debug 表現は埋め込まない（既存 constructor が
/// 既に型名だけを使うため、message をそのままコピーすればこの不変条件を保つ）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableError {
    /// 機械可読の canonical code（determinism 比較・audit キー）。
    pub code: ErrorCode,
    /// script の `try/catch` で `e["type"]` として観測される文字列（`code.as_str()` の投影）。
    pub script_type: String,
    /// per-template の安定 message ID。
    pub message_id: ErrorMessageId,
    /// script から観測可能な canonical message（既存テンプレートをそのままコピー）。
    pub script_message: String,
    /// error が発生した module の識別子（呼び出し側が供給）。
    pub module_id: ModuleId,
    /// error 発生行（`u32` 飽和変換）。
    pub line: u32,
    /// 列情報（現状 `TsumugiError` は列を持たないため常に `None`。§4.6 は `Option<u32>`）。
    pub column: Option<u32>,
    /// script trace（§3.6 準拠の既存 trace をそのまま射影。root script frame は含まれない）。
    pub trace: Vec<StableFrame>,
}

/// per-template の安定 message ID の tag と文字列の単一テーブル（§2 の 1 テーブル集約）。
///
/// 二重管理を避けるため、canonical constructor（error.rs / ast.rs / module.rs /
/// builtin_core.rs / builtin.rs / sandbox.rs のクラス B constructor）と射影側 resolver
/// [`message_id_for`] が、この単一のテーブルを参照する。各 tag は §3.4 の 1 テンプレート
/// （= 1 constructor が生む 1 日本語テンプレート）に 1:1 で対応する。
///
/// tag は `NonZeroU16`（constructor が `TsumugiError::Runtime.message_id` に載せる内部表現）で、
/// 安定 ASCII キー文字列への対応は [`id_str`] が持つ。`NonZeroU16` にして `Runtime` 変種の
/// 既存 padding に収め、深い tree 再帰で stack を伝播する error のサイズを増やさない
/// （`&'static str` 直持ちは 16 byte 増やし 128 深度 native 再帰を stack overflow させた）。
pub mod message_id {
    use std::num::NonZeroU16;

    /// tag 値を宣言し、同時に tag→文字列の対応と全 tag 一覧を 1 箇所で生成するマクロ。
    macro_rules! message_ids {
        ($($name:ident = $value:expr => $id:expr;)*) => {
            $(
                pub(crate) const $name: NonZeroU16 = match NonZeroU16::new($value) {
                    Some(v) => v,
                    None => panic!("message_id tag must be non-zero"),
                };
            )*

            /// tag から安定 ASCII キー文字列を引く（唯一の対応表）。
            pub(crate) fn id_str(tag: NonZeroU16) -> &'static str {
                match tag.get() {
                    $($value => $id,)*
                    // 未登録 tag は到達不能（constructor は必ず上記定数を使う）。防御的に返す。
                    _ => "runtime.unknown",
                }
            }

            /// 全 tag と文字列の一覧（テスト用: 重複検出・網羅確認）。
            #[cfg(test)]
            pub(crate) const ALL: &[(NonZeroU16, &str)] = &[
                $(($name, $id),)*
            ];
        };
    }

    message_ids! {
        // Name（クラス B: 2 テンプレート）
        NAME_UNDEFINED = 1 => "name.undefined";
        NAME_ASSIGN_UNDEFINED = 2 => "name.assign_undefined";

        // Type（クラス B: 多テンプレート）
        TYPE_ARITHMETIC = 3 => "type.arithmetic";
        TYPE_UNARY = 4 => "type.unary";
        TYPE_COMPARISON = 5 => "type.comparison";
        TYPE_LIST_INDEX = 6 => "type.list_index";
        TYPE_STR_INDEX = 7 => "type.str_index";
        TYPE_DICT_KEY = 8 => "type.dict_key";
        TYPE_INDEX_READ_UNSUPPORTED = 9 => "type.index_read_unsupported";
        TYPE_INDEX_ASSIGN_UNSUPPORTED = 10 => "type.index_assign_unsupported";
        TYPE_NOT_CALLABLE = 11 => "type.not_callable";
        TYPE_CALLBACK_NOT_CALLABLE = 12 => "type.callback_not_callable";

        // Index（クラス B: 2 テンプレート）
        INDEX_LIST_OUT_OF_RANGE = 13 => "index.list_out_of_range";
        INDEX_STR_OUT_OF_RANGE = 14 => "index.str_out_of_range";

        // Argument（クラス B: 多テンプレート）
        ARGUMENT_USER_ARITY = 15 => "argument.user_arity";
        ARGUMENT_BUILTIN_ARITY = 16 => "argument.builtin_arity";
        ARGUMENT_BUILTIN_MIN_ARITY = 17 => "argument.builtin_min_arity";
        ARGUMENT_HOST_FUNCTION_ARITY = 18 => "argument.host_function_arity";
        ARGUMENT_CALLBACK_ARITY = 19 => "argument.callback_arity";
        ARGUMENT_EXIT_CODE_RANGE = 20 => "argument.exit_code_range";
        ARGUMENT_EXIT_ARITY = 21 => "argument.exit_arity";
        ARGUMENT_INVALID_PATH = 22 => "argument.invalid_path";

        // BuiltinType（クラス B: 多テンプレート）
        BUILTIN_TYPE_ARG_TYPE = 23 => "builtin_type.arg_type";
        BUILTIN_TYPE_MUTATION_TARGET = 24 => "builtin_type.mutation_target";
        BUILTIN_TYPE_POP_EMPTY = 25 => "builtin_type.pop_empty";
        BUILTIN_TYPE_STATE = 26 => "builtin_type.state";

        // StackOverflow（クラス B: 2 テンプレート）
        OVERFLOW_CALL_DEPTH = 27 => "overflow.call_depth";
        OVERFLOW_AST_DEPTH = 28 => "overflow.ast_depth";

        // ControlFlow（クラス B: 2 テンプレート）
        CONTROL_FLOW_BREAK = 29 => "control_flow.break_outside_loop";
        CONTROL_FLOW_CONTINUE = 30 => "control_flow.continue_outside_loop";

        // Import（クラス B: 3 テンプレート）
        IMPORT_DEPTH = 31 => "import.depth";
        IMPORT_UNREADABLE = 32 => "import.unreadable";
        IMPORT_SYNTAX = 33 => "import.syntax";

        // Conversion（クラス B: 3 テンプレート）
        CONVERSION_TO_INT = 34 => "conversion.to_int";
        CONVERSION_TO_FLOAT = 35 => "conversion.to_float";
        CONVERSION_FLOAT_TO_INT = 36 => "conversion.float_to_int";

        // IntOverflow（クラス B: 2 テンプレート）
        INT_OVERFLOW_ARITHMETIC = 37 => "int_overflow.arithmetic";
        INT_OVERFLOW_FILE_SIZE = 38 => "int_overflow.file_size";

        // Sandbox（クラス B: 2 テンプレート）
        SANDBOX_FILESYSTEM_DENIED = 39 => "sandbox.filesystem_denied";
        SANDBOX_PATH_DENIED = 40 => "sandbox.path_denied";
    }
}

/// クラス A kind（kind 単独で 1:1）と fallback の安定 ID 文字列。
///
/// クラス A は tag を付与せず射影側で kind から ID を補完する（§8.2）。これらは tag 経由では
/// なく kind から直接引くため、`message_id` tag テーブルとは分けて文字列で持つ。
mod class_a_id {
    pub(crate) const ZERO_DIVISION: &str = "zero_division";
    pub(crate) const ITERATION_NOT_ITERABLE: &str = "iteration.not_iterable";
    pub(crate) const CAPABILITY_DENIED: &str = "capability.denied";
    pub(crate) const HOST_ADAPTER_FAILED: &str = "host.adapter_failed";
    pub(crate) const IO_STDOUT_WRITE_FAILED: &str = "io.stdout_write_failed";
    pub(crate) const INTERNAL: &str = "internal";

    // kind だけで ID が決まらないが射影経路（主に VM、本 Slice では tree 対象外）で tag 無しの
    // error が来た場合に `message_id_for` が total であることを保証する fallback 群。tree
    // backend では canonical constructor が必ずタグまたは 1:1 kind を経由するため、ここへ落ちる
    // 経路は本 Slice のテスト対象外（§8.2 解決順 3）。
    pub(crate) const TYPE_UNKNOWN: &str = "type.unknown";
    pub(crate) const INDEX_UNKNOWN: &str = "index.unknown";
    pub(crate) const NAME_UNKNOWN: &str = "name.unknown";
    pub(crate) const ARGUMENT_UNKNOWN: &str = "argument.unknown";
    pub(crate) const BUILTIN_TYPE_UNKNOWN: &str = "builtin_type.unknown";
    pub(crate) const OVERFLOW_UNKNOWN: &str = "overflow.unknown";
    pub(crate) const CONTROL_FLOW_UNKNOWN: &str = "control_flow.unknown";
    pub(crate) const IMPORT_UNKNOWN: &str = "import.unknown";
    pub(crate) const CONVERSION_UNKNOWN: &str = "conversion.unknown";
    pub(crate) const INT_OVERFLOW_UNKNOWN: &str = "int_overflow.unknown";
    pub(crate) const SANDBOX_UNKNOWN: &str = "sandbox.unknown";
    pub(crate) const RUNTIME_UNKNOWN: &str = "runtime.unknown";
    pub(crate) const PARSE: &str = "parse";
}

/// `TsumugiError` から per-template の [`ErrorMessageId`] を引く total 関数（§8.2 二層 resolver）。
///
/// 解決順:
/// 1. error の内部 message_id タグが `Some(id)` ならそれを使う（クラス B constructor が付与済み）。
/// 2. `None` なら kind から引く（クラス A、または kind 単独で決まる kind）。
/// 3. kind から一意に引けない kind で tag 無しの経路（主に VM、本 Slice では tree 対象外）は
///    kind ごとの fallback ID へ落とす。`None` を決して panic させず total を保証する。
///
/// 本関数は catch 可能 error（射影対象）に対して呼ばれる前提。uncatchable kind は
/// [`StableError::project`] が先に `None` を返すため、ここには来ない。
fn message_id_for(err: &TsumugiError) -> ErrorMessageId {
    // 層 1: constructor が付与した per-template タグ（クラス B）。
    if let TsumugiError::Runtime {
        message_id: Some(tag),
        ..
    } = err
    {
        return ErrorMessageId(message_id::id_str(*tag));
    }

    // 層 2 / 3: kind から補完（クラス A）。クラス B kind の tag 無し経路は fallback。
    use class_a_id as a;
    let id = match err {
        TsumugiError::Parse { .. } => a::PARSE,
        TsumugiError::Runtime { kind, .. } => match kind {
            // クラス A（kind 単独で 1:1）
            ErrorKind::ZeroDivision => a::ZERO_DIVISION,
            ErrorKind::Iteration => a::ITERATION_NOT_ITERABLE,
            ErrorKind::Capability => a::CAPABILITY_DENIED,
            ErrorKind::Host => a::HOST_ADAPTER_FAILED,
            ErrorKind::Io => a::IO_STDOUT_WRITE_FAILED,
            ErrorKind::Internal => a::INTERNAL,
            // クラス B kind の tag 無し fallback（tree では到達しない）
            ErrorKind::Type => a::TYPE_UNKNOWN,
            ErrorKind::Index => a::INDEX_UNKNOWN,
            ErrorKind::Name => a::NAME_UNKNOWN,
            ErrorKind::Argument => a::ARGUMENT_UNKNOWN,
            ErrorKind::BuiltinType => a::BUILTIN_TYPE_UNKNOWN,
            ErrorKind::StackOverflow => a::OVERFLOW_UNKNOWN,
            ErrorKind::ControlFlow => a::CONTROL_FLOW_UNKNOWN,
            ErrorKind::Import => a::IMPORT_UNKNOWN,
            ErrorKind::Conversion => a::CONVERSION_UNKNOWN,
            ErrorKind::IntOverflow => a::INT_OVERFLOW_UNKNOWN,
            ErrorKind::Sandbox => a::SANDBOX_UNKNOWN,
            ErrorKind::Runtime => a::RUNTIME_UNKNOWN,
            // uncatchable kind は project が先に None を返すため到達しない。防御的に
            // runtime fallback を返す（panic しない）。
            ErrorKind::StepLimit
            | ErrorKind::DeadlineExceeded
            | ErrorKind::Cancelled
            | ErrorKind::ProcessExit
            | ErrorKind::CollectionLimit
            | ErrorKind::StringLimit
            | ErrorKind::SourceLimit
            | ErrorKind::HeapLimit
            | ErrorKind::IoLimit
            | ErrorKind::InternalControl => a::RUNTIME_UNKNOWN,
        },
    };
    ErrorMessageId(id)
}

impl StableError {
    /// catch 可能な runtime error を [`StableError`] へ射影する（§4.6 / §5）。
    ///
    /// uncatchable terminal（budget / deadline / cancel / process-exit / internal-control）は
    /// [`crate::error::ErrorKind::is_uncatchable`] を唯一の判定正本として `None` を返す（§6）。
    /// `TsumugiError::Parse` も script-visible な runtime error ではないため `None`。
    ///
    /// 既存の正本（canonical message / stable 型文字列 / trace）を再利用し、二重管理を発生
    /// させない。canonical message を組み立て直さず、既存 `message()` をコピーするだけ。
    pub fn project(err: &TsumugiError, module_id: ModuleId) -> Option<StableError> {
        // Parse は script-visible な catch 対象ではない（射影対象外）。
        let kind = err.kind()?;

        // uncatchable terminal は StableError 化しない（§6、唯一の判定正本）。
        if ErrorKind::is_uncatchable(Some(kind)) {
            return None;
        }

        let message_id = message_id_for(err);
        let trace = match err {
            TsumugiError::Runtime { trace, .. } => {
                trace.iter().map(StableFrame::from_trace).collect()
            }
            TsumugiError::Parse { .. } => Vec::new(),
        };

        Some(StableError {
            code: kind,
            // code から導出（独立に食い違う余地を作らない、§3）。
            script_type: kind.as_str().to_string(),
            message_id,
            // 既存の canonical message をそのままコピー（新規テンプレートを作らない、§5.2）。
            script_message: err.message().to_string(),
            module_id,
            // usize -> u32 飽和変換（範囲外は u32::MAX、panic しない、§4）。
            line: saturating_u32(err.line()),
            // TsumugiError は列情報を持たない（§5.2）。
            column: None,
            trace,
        })
    }
}

impl StableFrame {
    /// 内部 [`crate::error::TraceFrame`] から射影する（§4）。
    fn from_trace(frame: &TraceFrame) -> StableFrame {
        StableFrame {
            function: frame.name.clone(),
            line: saturating_u32(frame.line),
        }
    }
}

/// `usize -> u32` の飽和変換（範囲外は `u32::MAX`、panic しない。§4 / semantic-decisions §2）。
fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::BinOpKind;
    use crate::value::Value;

    fn test_module_id() -> ModuleId {
        ModuleId::new("test").unwrap()
    }

    /// catch 可能 error の全 field が正本から射影される（§9 ユニット 1）。
    #[test]
    fn projects_all_fields_from_canonical_error() {
        let err = TsumugiError::zero_division(7);
        let stable = StableError::project(&err, test_module_id()).expect("catchable -> Some");

        assert_eq!(stable.code, ErrorKind::ZeroDivision);
        assert_eq!(stable.script_type, "zero_division");
        assert_eq!(stable.message_id.as_str(), "zero_division");
        assert_eq!(stable.script_message, "ゼロ除算");
        assert_eq!(stable.module_id.as_str(), "test");
        assert_eq!(stable.line, 7);
        assert_eq!(stable.column, None);
        assert!(stable.trace.is_empty());
    }

    /// 代表的な catch 可能 error の message_id が per-template で引ける（§9 ユニット 1 / 4）。
    #[test]
    fn representative_errors_resolve_per_template_ids() {
        let left = Value::Int(1);
        let right = Value::Bool(false);
        let cases: &[(TsumugiError, ErrorKind, &str)] = &[
            (
                TsumugiError::zero_division(1),
                ErrorKind::ZeroDivision,
                "zero_division",
            ),
            (
                TsumugiError::arithmetic_type(1, BinOpKind::Add, &left, &right),
                ErrorKind::Type,
                "type.arithmetic",
            ),
            (
                TsumugiError::list_index_out_of_range(1, 9, 2),
                ErrorKind::Index,
                "index.list_out_of_range",
            ),
            (
                TsumugiError::str_index_out_of_range(1, 9, 2),
                ErrorKind::Index,
                "index.str_out_of_range",
            ),
            (
                TsumugiError::undefined_name(1, "x"),
                ErrorKind::Name,
                "name.undefined",
            ),
            (
                TsumugiError::assign_undefined(1, "x"),
                ErrorKind::Name,
                "name.assign_undefined",
            ),
            (
                TsumugiError::user_arity(1, "f", 1, 2),
                ErrorKind::Argument,
                "argument.user_arity",
            ),
            (
                TsumugiError::call_depth_limit(1, 128),
                ErrorKind::StackOverflow,
                "overflow.call_depth",
            ),
        ];
        for (err, kind, expected_id) in cases {
            let stable = StableError::project(err, test_module_id()).expect("catchable");
            assert_eq!(stable.code, *kind, "code for {expected_id}");
            assert_eq!(stable.message_id.as_str(), *expected_id);
            assert_eq!(stable.script_type, kind.as_str());
        }
    }

    /// code と script_type は常に整合する（§9 ユニット 2）。
    #[test]
    fn script_type_matches_code_for_catchable_kinds() {
        for kind in all_kinds() {
            if ErrorKind::is_uncatchable(Some(kind)) {
                continue;
            }
            let err = TsumugiError::runtime_with_kind(1, kind, "msg");
            let stable =
                StableError::project(&err, test_module_id()).expect("catchable kind -> Some");
            assert_eq!(stable.script_type, stable.code.as_str());
            assert_eq!(stable.code, kind);
        }
    }

    /// uncatchable kind はすべて `None` に射影され、`Internal`（catch 可能）は `Some`
    /// （§9 ユニット 3、carryover MEDIUM: is_uncatchable を唯一の正本とする）。
    #[test]
    fn uncatchable_kinds_project_to_none() {
        // carryover findings が明示する uncatchable 列挙。IoLimit（io_limit、I/O 予算上限）が
        // ここに含まれ、catch 可能な Io（io、stdout 書込失敗）とは別物であることを固定する。
        let uncatchable = [
            ErrorKind::StepLimit,
            ErrorKind::CollectionLimit,
            ErrorKind::StringLimit,
            ErrorKind::SourceLimit,
            ErrorKind::HeapLimit,
            ErrorKind::IoLimit,
            ErrorKind::InternalControl,
            ErrorKind::ProcessExit,
            ErrorKind::DeadlineExceeded,
            ErrorKind::Cancelled,
        ];
        for kind in uncatchable {
            assert!(
                ErrorKind::is_uncatchable(Some(kind)),
                "{kind:?} must be uncatchable per is_uncatchable"
            );
            let err = TsumugiError::runtime_with_kind(1, kind, "terminal");
            assert_eq!(
                StableError::project(&err, test_module_id()),
                None,
                "{kind:?} must not project to StableError"
            );
        }

        // Io（catch 可能 stdout 書込失敗）は射影される。IoLimit との取り違え防止。
        assert!(!ErrorKind::is_uncatchable(Some(ErrorKind::Io)));
        let io =
            TsumugiError::runtime_with_kind(1, ErrorKind::Io, "標準出力への書き込みに失敗しました");
        let io_stable = StableError::project(&io, test_module_id()).expect("Io is catchable");
        assert_eq!(io_stable.code, ErrorKind::Io);
        assert_eq!(io_stable.script_type, "io");
        assert_eq!(io_stable.message_id.as_str(), "io.stdout_write_failed");

        // Internal（catch 可能）は Some、InternalControl（catch 不能）は None。
        let internal = TsumugiError::internal(1, "x");
        assert!(StableError::project(&internal, test_module_id()).is_some());
        let internal_control = TsumugiError::internal_control(1, "x");
        assert_eq!(
            StableError::project(&internal_control, test_module_id()),
            None
        );
    }

    /// IoLimit（I/O 予算上限）は as_str=="io_limit"、Io（stdout 書込失敗）は as_str=="io" で
    /// 別 variant（carryover MEDIUM: inventory として as_str と kind の対応を固定）。
    #[test]
    fn io_and_io_limit_are_distinct_variants() {
        assert_eq!(ErrorKind::Io.as_str(), "io");
        assert_eq!(ErrorKind::IoLimit.as_str(), "io_limit");
        assert!(!ErrorKind::is_uncatchable(Some(ErrorKind::Io)));
        assert!(ErrorKind::is_uncatchable(Some(ErrorKind::IoLimit)));
    }

    /// 全 catch 可能 kind がクラス A/B のいずれかで ID を引け、`message_id_for` は total
    /// （§9 ユニット 5）。tag 無し（kind のみ）経路でも panic せず ID を返す。
    #[test]
    fn message_id_for_is_total_over_catchable_kinds() {
        for kind in all_kinds() {
            if ErrorKind::is_uncatchable(Some(kind)) {
                continue;
            }
            // tag 無し（runtime_with_kind）でも total。
            let untagged = TsumugiError::runtime_with_kind(1, kind, "m");
            let _ = message_id_for(&untagged);
            // 射影も Some になる（catch 可能 kind はすべて射影対象）。
            assert!(
                StableError::project(&untagged, test_module_id()).is_some(),
                "{kind:?} catchable must project"
            );
        }
    }

    /// StackOverflow は 2 テンプレートを持ちクラス B（両 ID が別値）であることを固定
    /// （§9 ユニット 5、carryover MEDIUM の再発防止: 1:N kind がクラス A へ残らない）。
    #[test]
    fn stack_overflow_has_two_distinct_template_ids() {
        let call_depth = TsumugiError::call_depth_limit(1, 128);
        // AST 深度 constructor は ast.rs 内 validate_program_depth が生成するため、
        // テストでは同じ ErrorKind::StackOverflow + OVERFLOW_AST_DEPTH タグで直接構築する。
        let ast_depth = TsumugiError::runtime_with_message_id(
            1,
            ErrorKind::StackOverflow,
            message_id::OVERFLOW_AST_DEPTH,
            "ASTのネストが深すぎます (上限: 256)",
        );

        let call_id = StableError::project(&call_depth, test_module_id())
            .unwrap()
            .message_id;
        let ast_id = StableError::project(&ast_depth, test_module_id())
            .unwrap()
            .message_id;

        assert_eq!(call_id.as_str(), "overflow.call_depth");
        assert_eq!(ast_id.as_str(), "overflow.ast_depth");
        assert_ne!(
            call_id, ast_id,
            "2 テンプレートが同一 ID へ潰れてはならない"
        );
    }

    /// 全 message_id（tag の文字列 + クラス A/fallback 文字列）に重複がない
    /// （§9 ユニット 4: ID の一意性）。tag と文字列の両方の一意性を固定する。
    #[test]
    fn all_message_ids_are_unique() {
        // クラス B の tag テーブル: tag 値も文字列も一意。
        let mut tags: Vec<u16> = message_id::ALL.iter().map(|(t, _)| t.get()).collect();
        let tag_count = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), tag_count, "message_id tag に重複がある");

        // クラス B 文字列 + クラス A/fallback 文字列をすべて集めて一意性を確認。
        let mut ids: Vec<&str> = message_id::ALL.iter().map(|(_, s)| *s).collect();
        ids.extend_from_slice(&[
            class_a_id::ZERO_DIVISION,
            class_a_id::ITERATION_NOT_ITERABLE,
            class_a_id::CAPABILITY_DENIED,
            class_a_id::HOST_ADAPTER_FAILED,
            class_a_id::IO_STDOUT_WRITE_FAILED,
            class_a_id::INTERNAL,
            class_a_id::TYPE_UNKNOWN,
            class_a_id::INDEX_UNKNOWN,
            class_a_id::NAME_UNKNOWN,
            class_a_id::ARGUMENT_UNKNOWN,
            class_a_id::BUILTIN_TYPE_UNKNOWN,
            class_a_id::OVERFLOW_UNKNOWN,
            class_a_id::CONTROL_FLOW_UNKNOWN,
            class_a_id::IMPORT_UNKNOWN,
            class_a_id::CONVERSION_UNKNOWN,
            class_a_id::INT_OVERFLOW_UNKNOWN,
            class_a_id::SANDBOX_UNKNOWN,
            class_a_id::RUNTIME_UNKNOWN,
            class_a_id::PARSE,
        ]);
        let id_count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), id_count, "message_id 文字列に重複がある");
    }

    /// tag→文字列テーブルが tag の往復で安定（id_str が全 tag に文字列を返す）。
    #[test]
    fn id_str_round_trips_all_tags() {
        for (tag, expected) in message_id::ALL {
            assert_eq!(message_id::id_str(*tag), *expected);
        }
    }

    /// script_message に Rust 型名や debug 表現が混ざらない（§9 ユニット 6: 回帰防止）。
    #[test]
    fn script_message_has_no_rust_type_names() {
        let left = Value::Int(1);
        let right = Value::Bool(false);
        let err = TsumugiError::arithmetic_type(1, BinOpKind::Add, &left, &right);
        let stable = StableError::project(&err, test_module_id()).unwrap();
        assert!(!stable.script_message.contains("Value::"));
        assert!(!stable.script_message.contains("{:?}"));
        // canonical 型名（Int / Bool）は使われる（Rust 型名ではなく §3.3 の型名一覧）。
        assert!(stable.script_message.contains("Int"));
        assert!(stable.script_message.contains("Bool"));
    }

    /// trace 順序・function 名・line が error::TraceFrame と一致する（§9 ユニット 7）。
    #[test]
    fn trace_order_and_fields_are_preserved() {
        let err = TsumugiError::undefined_name(10, "x").with_trace(vec![
            TraceFrame {
                name: "inner".into(),
                line: 20,
            },
            TraceFrame {
                name: "outer".into(),
                line: 30,
            },
        ]);
        let stable = StableError::project(&err, test_module_id()).unwrap();
        assert_eq!(stable.trace.len(), 2);
        assert_eq!(stable.trace[0].function, "inner");
        assert_eq!(stable.trace[0].line, 20);
        assert_eq!(stable.trace[1].function, "outer");
        assert_eq!(stable.trace[1].line, 30);
    }

    /// line の u32 飽和変換: 通常行は等値、範囲外は u32::MAX（§9 ユニット 8）。
    #[test]
    fn line_saturates_without_panic() {
        assert_eq!(saturating_u32(42), 42);
        assert_eq!(saturating_u32(usize::MAX), u32::MAX);

        let err = TsumugiError::zero_division(usize::MAX);
        let stable = StableError::project(&err, test_module_id()).unwrap();
        assert_eq!(stable.line, u32::MAX);
    }

    /// 決定性: 同一 error・同一 module_id から 2 回射影した結果が完全一致する。
    #[test]
    fn projection_is_deterministic() {
        let make = || TsumugiError::user_arity(5, "f", 1, 2);
        let a = StableError::project(&make(), test_module_id()).unwrap();
        let b = StableError::project(&make(), test_module_id()).unwrap();
        assert_eq!(a, b);
    }

    /// `ErrorKind` の全 variant を網羅列挙（match 網羅性で漏れを構造的に防ぐ）。
    ///
    /// variant を追加したらここが compile error になり、分類漏れに気付ける。
    fn all_kinds() -> Vec<ErrorKind> {
        let all = vec![
            ErrorKind::ZeroDivision,
            ErrorKind::Type,
            ErrorKind::Index,
            ErrorKind::Name,
            ErrorKind::StepLimit,
            ErrorKind::DeadlineExceeded,
            ErrorKind::Cancelled,
            ErrorKind::StackOverflow,
            ErrorKind::Sandbox,
            ErrorKind::Capability,
            ErrorKind::ProcessExit,
            ErrorKind::Import,
            ErrorKind::Argument,
            ErrorKind::IntOverflow,
            ErrorKind::ControlFlow,
            ErrorKind::CollectionLimit,
            ErrorKind::StringLimit,
            ErrorKind::SourceLimit,
            ErrorKind::HeapLimit,
            ErrorKind::IoLimit,
            ErrorKind::Conversion,
            ErrorKind::BuiltinType,
            ErrorKind::Iteration,
            ErrorKind::Io,
            ErrorKind::Host,
            ErrorKind::Internal,
            ErrorKind::InternalControl,
            ErrorKind::Runtime,
        ];
        // 網羅性の構造的強制: 下の match に variant 追加漏れがあれば compile error。
        for kind in &all {
            let _: () = match kind {
                ErrorKind::ZeroDivision
                | ErrorKind::Type
                | ErrorKind::Index
                | ErrorKind::Name
                | ErrorKind::StepLimit
                | ErrorKind::DeadlineExceeded
                | ErrorKind::Cancelled
                | ErrorKind::StackOverflow
                | ErrorKind::Sandbox
                | ErrorKind::Capability
                | ErrorKind::ProcessExit
                | ErrorKind::Import
                | ErrorKind::Argument
                | ErrorKind::IntOverflow
                | ErrorKind::ControlFlow
                | ErrorKind::CollectionLimit
                | ErrorKind::StringLimit
                | ErrorKind::SourceLimit
                | ErrorKind::HeapLimit
                | ErrorKind::IoLimit
                | ErrorKind::Conversion
                | ErrorKind::BuiltinType
                | ErrorKind::Iteration
                | ErrorKind::Io
                | ErrorKind::Host
                | ErrorKind::Internal
                | ErrorKind::InternalControl
                | ErrorKind::Runtime => (),
            };
        }
        all
    }
}
