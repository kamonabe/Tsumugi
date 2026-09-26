//! Phase 2 capability — host function registry（スライス C8）。
//!
//! [Capability Model 仕様](../docs/capability-model.md) 第11節が定義する
//! [`HostFunctionRegistry`] とその descriptor / redaction metadata を実装する。埋め込み
//! host は業務ロジックを host function として登録し、[`crate::capability::CapabilitySet`] の
//! `HostFunction` authority で実行を grant する（登録と grant は別、第11.1節）。
//!
//! # スライス境界（C8、Phase 2 最小形）
//!
//! 本スライスは次を実装する。
//!
//! - registry の型・descriptor validation・名前/衝突検査（CAP-AT-20 の host function 面）。
//! - redaction metadata（`Omit`/`TypeOnly`/`LengthOnly`）と、runtime value 本文を出さない
//!   serializer（[`AuditValue`]、CAP-AT-22）。
//! - **tree engine** の call 配線（registered+granted→callback、registered-not-granted→
//!   catch 可能 `capability` error、unknown→通常の name error、`HostCallError::Host`→catch
//!   可能 `host` error、CAP-AT-19/21）。
//!
//! 次は後続スライス／Phase へ委ねる。
//!
//! - **fuel / host-call cost 課金**（[`HostCost`]）は Phase 3。C8 は metadata の validation と
//!   catalog 格納までで、課金しない（第11.2節 規則2）。
//! - **audit event 発行**（Phase 6）。C8 は descriptor→event の写像を固定するが発行しない
//!   （第13節）。
//! - **VM engine の call 配線**。VM は builtin 名をコンパイル時に解決し `CallBuiltin` へ lower
//!   するため、link 時登録の host function 名を知らず、通常の global lookup（`undefined_name`）
//!   に落ちる。VM 配線は experimental backend の後続統合（VM embedding = E9/Phase 5）に載せる。
//! - `CapabilityCallContext` を取る最終形の [`HostFunction::call`] シグネチャ（第11節）。C8 は
//!   C3/C4/C5 と同じく context を取らない最小形で進める。

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::capability::HostFunctionId;
use crate::embedding::ConfigError;
use crate::error::TsumugiError;
use crate::value::Value;

/// host function の引数個数契約（仕様第11.1節 `Arity`）。
///
/// builtin registry の `Arity`（`Exact(usize)`/`OneOf`/`Variadic`）とは別型で、host function
/// 用に仕様どおり `Exact(u16)` / `Range { min, max }` を持つ。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arity {
    /// ちょうど n 個。
    Exact(u16),
    /// min..=max 個（`min <= max`）。
    Range {
        /// 最小個数。
        min: u16,
        /// 最大個数。
        max: u16,
    },
}

impl Arity {
    /// 実際の引数個数が arity 契約を満たすか。
    pub fn accepts(self, actual: usize) -> bool {
        match self {
            Arity::Exact(n) => actual == n as usize,
            Arity::Range { min, max } => actual >= min as usize && actual <= max as usize,
        }
    }

    /// `argument_audit` の長さ検証に使う「最大引数個数」。
    fn max_arguments(self) -> usize {
        match self {
            Arity::Exact(n) => n as usize,
            Arity::Range { max, .. } => max as usize,
        }
    }
}

/// host call の cost metadata（仕様第11.1節 `HostCost`）。
///
/// Phase 2 は validation と格納だけで、fuel/host-call を課金しない（第11.2節 規則2、Phase 3）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostCost {
    /// 呼び出し1回あたりの基本 fuel。
    pub base_fuel: u64,
    /// 引数1個あたりの追加 fuel。
    pub per_argument_fuel: u64,
    /// value unit 1個あたりの追加 fuel。
    pub per_value_unit_fuel: u64,
    /// 結果の最大 byte 数（Phase 3 で script へ渡す前に検査）。
    pub max_result_bytes: Option<std::num::NonZeroU64>,
}

impl Default for HostCost {
    /// cost なしの既定（Phase 2 では課金に使われない）。
    fn default() -> Self {
        Self {
            base_fuel: 0,
            per_argument_fuel: 0,
            per_value_unit_fuel: 0,
            max_result_bytes: None,
        }
    }
}

/// audit 記録時の値 redaction policy（仕様第11.1・11.3節 `AuditValuePolicy`）。
///
/// runtime value の本文を記録する policy は Phase 2 では設けない。既定は [`Self::Omit`]。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditValuePolicy {
    /// 何も記録しない。
    Omit,
    /// 型だけを記録する（長さ・内容は出さない）。
    TypeOnly,
    /// 型と長さだけを記録する（内容・内容 hash は出さない）。
    LengthOnly,
}

/// redaction 済みの audit 値表現（仕様第11.3節）。
///
/// runtime value の本文（数値・文字列内容・要素）を一切含まない。Phase 6 の audit sink が
/// この metadata を使う。`LengthOnly` の length は value unit 数（第11.2節 規則3）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditValue {
    /// 記録なし（`Omit`）。
    Omitted,
    /// 型名だけ（`TypeOnly`）。
    Type {
        /// 値の型名（`int`/`str`/`list` 等）。
        type_name: &'static str,
    },
    /// 型名と長さ（`LengthOnly`）。length は value unit 数。
    TypeAndLength {
        /// 値の型名。
        type_name: &'static str,
        /// value unit 数（第11.2節 規則3）。
        length: u64,
    },
}

/// policy に従って値を redaction する（本文を出さない、CAP-AT-22）。
pub fn redact(policy: AuditValuePolicy, value: &Value) -> AuditValue {
    match policy {
        AuditValuePolicy::Omit => AuditValue::Omitted,
        AuditValuePolicy::TypeOnly => AuditValue::Type {
            type_name: value_type_name(value),
        },
        AuditValuePolicy::LengthOnly => AuditValue::TypeAndLength {
            type_name: value_type_name(value),
            length: value_units(value),
        },
    }
}

/// 値の型名（本文を出さない安全な分類）。
fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "str",
        Value::Bool(_) => "bool",
        Value::Null => "null",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        Value::Fn { .. } | Value::VmFn { .. } => "function",
        Value::Error { .. } => "error",
    }
}

/// value unit 数（仕様第11.2節 規則3）。各 Value node につき 1、String は加えて UTF-8 byte 数、
/// List は要素、Dict は key/value を再帰加算する。checked u64 加算で overflow は `u64::MAX` 飽和
/// （Phase 2 は課金しないため飽和で十分。Phase 3 の課金経路は BudgetExceeded を使う）。
fn value_units(value: &Value) -> u64 {
    match value {
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Null => 1,
        // Function / Error は node 1 だけ。
        Value::Fn { .. } | Value::VmFn { .. } | Value::Error { .. } => 1,
        Value::Str(s) => 1u64.saturating_add(s.as_str().len() as u64),
        Value::List(items) => {
            let mut total: u64 = 1;
            for item in items.iter() {
                total = total.saturating_add(value_units(item));
            }
            total
        }
        Value::Dict(map) => {
            let mut total: u64 = 1;
            for (key, val) in map.iter() {
                // key は String（1 + byte 数）、value は再帰。
                total = total.saturating_add(1u64.saturating_add(key.len() as u64));
                total = total.saturating_add(value_units(val));
            }
            total
        }
    }
}

/// host function の descriptor（仕様第11.1節 `HostFunctionDescriptor`）。
#[derive(Clone, Debug)]
pub struct HostFunctionDescriptor {
    /// registry 内で一意な ID。
    pub id: HostFunctionId,
    /// script から呼ぶ公開名（identifier grammar・1..=64 bytes）。
    pub name: String,
    /// 引数個数契約。
    pub arity: Arity,
    /// cost metadata（Phase 3 で課金）。
    pub cost: HostCost,
    /// 引数 index ごとの redaction policy（空なら全 index `Omit`）。
    pub argument_audit: Vec<AuditValuePolicy>,
    /// 結果の redaction policy。
    pub result_audit: AuditValuePolicy,
    /// blocking する可能性があるか（Phase 4 で cooperative 実行に使う）。
    pub may_block: bool,
}

impl HostFunctionDescriptor {
    /// descriptor field を検証する（仕様第11.1節 `validate`）。
    ///
    /// - `Arity::Range` は `min <= max`。
    /// - `argument_audit` が空でなければ、長さが arity の最大引数個数（`Exact(n)`→n、
    ///   `Range { max, .. }`→max）と一致しなければならない。
    /// - `max`（最大引数個数）は 1024 以下。
    /// - `name` は identifier grammar・1..=64 bytes。
    ///
    /// ID の一意性は registry build で検査する（単体 descriptor では判定できない）。
    pub fn validate(&self) -> Result<(), ConfigError> {
        // arity。
        if let Arity::Range { min, max } = self.arity
            && min > max
        {
            return Err(ConfigError::InvalidDescriptor {
                field: "arity",
                code: "range_min_gt_max",
            });
        }
        let max_args = self.arity.max_arguments();
        if max_args > 1024 {
            return Err(ConfigError::InvalidDescriptor {
                field: "arity",
                code: "max_arguments_too_large",
            });
        }
        // argument_audit の長さ。
        if !self.argument_audit.is_empty() && self.argument_audit.len() != max_args {
            return Err(ConfigError::InvalidDescriptor {
                field: "argument_audit",
                code: "length_mismatch",
            });
        }
        // name grammar（identifier・1..=64 bytes）。
        validate_callable_name(&self.name)?;
        Ok(())
    }

    /// 実 call の引数 index に対応する redaction policy を返す（第11.1節）。
    ///
    /// `argument_audit` が空なら全 index `Omit`。index が audit vector 長を超える場合も `Omit`
    /// （actual より後の policy は無視、逆に vector より後ろの index も既定 `Omit`）。
    pub fn argument_policy(&self, index: usize) -> AuditValuePolicy {
        self.argument_audit
            .get(index)
            .copied()
            .unwrap_or(AuditValuePolicy::Omit)
    }
}

/// callable 公開名を検証する（identifier grammar・1..=64 UTF-8 bytes、第11.1節）。
///
/// identifier grammar は lexer と同じ（先頭 ASCII alphabetic か `_`、以降 ASCII alphanumeric
/// か `_`）。`host_` prefix は必須にしない。core keyword・builtin 等との衝突は registry build
/// で検査する（ここでは grammar と長さだけ）。
fn validate_callable_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty() {
        return Err(ConfigError::EmptyIdentifier {
            field: "host_function_name",
        });
    }
    if name.len() > 64 {
        return Err(ConfigError::IdentifierTooLong {
            field: "host_function_name",
            max_bytes: 64,
        });
    }
    let bytes = name.as_bytes();
    let head_ok = matches!(bytes[0], b'a'..=b'z' | b'A'..=b'Z' | b'_');
    let tail_ok = bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_');
    if !head_ok || !tail_ok {
        return Err(ConfigError::InvalidDescriptor {
            field: "host_function_name",
            code: "invalid_identifier",
        });
    }
    Ok(())
}

/// `name` が core keyword か（lexer と drift しないよう lexer で 1 token に落として判定する）。
///
/// keyword は単一 token が `Token::Ident` 以外になる。identifier grammar を満たす name だけを
/// 渡す前提（`validate_callable_name` 後）。
fn is_core_keyword(name: &str) -> bool {
    let tokens = crate::lexer::Lexer::new(name).tokenize();
    // tokenize は末尾に Eof を付ける。keyword なら [Keyword, Eof]、ident なら [Ident, Eof]。
    match tokens.first() {
        Some(spanned) => !matches!(spanned.token, crate::token::Token::Ident(_)),
        None => false,
    }
}

/// script の call site が解決する host function 呼び出し可能実体（仕様第11.1節 `HostFunction`）。
///
/// C8 最小形は `CapabilityCallContext` を取らない（第6・11節最終形は後続 Phase）。callback は
/// 評価済みの `&[Value]` を受け取り、`Value` か [`HostCallError`] を返す。
pub trait HostFunction: Send + Sync + 'static {
    /// この function の descriptor。
    fn descriptor(&self) -> &HostFunctionDescriptor;

    /// host 実装本体。arity は呼び出し側が検証済み。
    fn call(&self, arguments: &[Value]) -> Result<Value, HostCallError>;
}

/// host function 呼び出しの失敗（仕様第11.1節 `HostCallError`）。
///
/// C8 最小形は host 起因の失敗（`Host`）だけを持つ。budget/deadline/cancel を運ぶ `Control`
/// variant は、その制御 context を導入する Phase 3/4 で追加する（`#[non_exhaustive]`）。
#[derive(Debug)]
#[non_exhaustive]
pub enum HostCallError {
    /// host 実装内部の失敗。script call 中は catch 可能な canonical `host` error へ写す
    /// （`null`/`false` へ潰さない、第11.2節 規則5）。`category` は host の生 message を
    /// 出さない安全な分類語。
    Host {
        /// 安全な分類語（例: `"lookup"`。secret・path・backtrace を含めない）。
        category: String,
    },
}

/// 登録済み host function の immutable な registry（仕様第11.1節 `HostFunctionRegistry`）。
///
/// name→ID と ID→function を保持する。build 後は不変で、run 中の name lookup・registry 差替え
/// をしない（第11.1節。link 時に name→ID を固定する）。
#[derive(Clone, Default)]
pub struct HostFunctionRegistry {
    by_id: BTreeMap<HostFunctionId, Arc<dyn HostFunction>>,
    by_name: BTreeMap<String, HostFunctionId>,
}

impl HostFunctionRegistry {
    /// builder を作る。
    pub fn builder() -> HostFunctionRegistryBuilder {
        HostFunctionRegistryBuilder::new()
    }

    /// 空 registry（host function なし。既定値）。
    pub fn empty() -> Self {
        Self::default()
    }

    /// ID から function を引く。
    pub fn get(&self, id: HostFunctionId) -> Option<&Arc<dyn HostFunction>> {
        self.by_id.get(&id)
    }

    /// 公開名から function を引く（call site の name 解決、第11.1節）。
    pub(crate) fn lookup_by_name(&self, name: &str) -> Option<&Arc<dyn HostFunction>> {
        let id = self.by_name.get(name)?;
        self.by_id.get(id)
    }

    /// 公開名が登録されているか（call site が引数評価前に host 経路を選ぶ判定に使う）。
    pub(crate) fn contains_name(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// 登録済み function が1つも無いか。
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

impl std::fmt::Debug for HostFunctionRegistry {
    /// function 本体を出さず、登録名と ID だけを表示する。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostFunctionRegistry")
            .field("names", &self.by_name.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// [`HostFunctionRegistry`] を組み立てる builder。
pub struct HostFunctionRegistryBuilder {
    by_id: BTreeMap<HostFunctionId, Arc<dyn HostFunction>>,
    by_name: BTreeMap<String, HostFunctionId>,
}

impl HostFunctionRegistryBuilder {
    fn new() -> Self {
        Self {
            by_id: BTreeMap::new(),
            by_name: BTreeMap::new(),
        }
    }

    /// host function を1つ登録する。
    ///
    /// descriptor の validation に加え、次を build error（第11.1節）とする。
    ///
    /// - core keyword・`print`・core/context builtin・他 host function との名前衝突。
    /// - registry 内での ID 重複・名前重複。
    pub fn register(mut self, function: Arc<dyn HostFunction>) -> Result<Self, ConfigError> {
        let descriptor = function.descriptor();
        descriptor.validate()?;
        let name = descriptor.name.clone();
        let id = descriptor.id;

        // core keyword（`print`/`import` 等の予約語を含む）との衝突。
        if is_core_keyword(&name) {
            return Err(ConfigError::DuplicateCallableName { name });
        }
        // core/context builtin（`print` は予約語 keyword として上で捕捉されるが、registry の
        // 公開名一覧とも突き合わせる）との衝突。
        if crate::builtin_registry::is_public_builtin(&name) {
            return Err(ConfigError::DuplicateCallableName { name });
        }
        // 他 host function との名前重複。
        if self.by_name.contains_key(&name) {
            return Err(ConfigError::DuplicateCallableName { name });
        }
        // ID 重複。
        if self.by_id.contains_key(&id) {
            return Err(ConfigError::DuplicateCapability {
                kind: crate::capability::CapabilityKind::HostFunction,
            });
        }

        self.by_name.insert(name, id);
        self.by_id.insert(id, function);
        Ok(self)
    }

    /// registry を freeze する。
    pub fn build(self) -> Result<HostFunctionRegistry, ConfigError> {
        Ok(HostFunctionRegistry {
            by_id: self.by_id,
            by_name: self.by_name,
        })
    }
}

/// call site の名前を host function として解決・実行する共通ロジック（第11.1・11.2節）。
///
/// 呼び出し側（engine）は user binding・builtin fallback の**後**にこれを呼ぶ。返り値は
/// `Ok(Some(value))` が host function 実行成功、`Ok(None)` が「この name は host function では
/// ない」（呼び出し側は通常の undefined-name error にする）、`Err` が catch 可能な error
/// （capability 不足・arity・host 失敗）。
///
/// - registered だが未 grant → callback 0 の catch 可能な `capability` error（第11.1節）。
/// - registered かつ granted → arity 検査（catch 可能な argument error）→ callback 実行。
///   `HostCallError::Host` は catch 可能な `host` error（第11.2節 規則5）。callback panic は
///   呼び出し側の host boundary（E6 `catch_host_unwind`）が InternalFailure + poison にする。
pub fn resolve_host_call(
    registry: &HostFunctionRegistry,
    capabilities: &crate::capability::CapabilitySet,
    name: &str,
    args: &[Value],
    line: usize,
) -> Result<Option<Value>, TsumugiError> {
    let Some(function) = registry.lookup_by_name(name) else {
        return Ok(None);
    };
    let descriptor = function.descriptor();
    // authority 検査を callback より先に行う（第3.7節 error precedence。callback call 0）。
    if !capabilities.contains_host_function(descriptor.id) {
        return Err(TsumugiError::capability_denied(line, name));
    }
    // arity 検査（catch 可能な argument error、第11.2節 規則1）。
    if !descriptor.arity.accepts(args.len()) {
        return Err(TsumugiError::host_function_arity(line, name, args.len()));
    }
    match function.call(args) {
        Ok(value) => Ok(Some(value)),
        // host 起因の失敗は null/false へ潰さず catch 可能な `host` error にする（規則5）。
        Err(HostCallError::Host { category }) => {
            Err(TsumugiError::host_adapter_failed(line, name, &category))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU128;

    fn hid(n: u128) -> HostFunctionId {
        HostFunctionId::new(NonZeroU128::new(n).unwrap())
    }

    /// テスト用 host function。descriptor と、call の挙動（成功/host error）を持つ。
    struct TestFn {
        descriptor: HostFunctionDescriptor,
        behavior: Behavior,
    }
    enum Behavior {
        Echo,
        HostError,
    }
    impl HostFunction for TestFn {
        fn descriptor(&self) -> &HostFunctionDescriptor {
            &self.descriptor
        }
        fn call(&self, arguments: &[Value]) -> Result<Value, HostCallError> {
            match self.behavior {
                Behavior::Echo => Ok(arguments.first().cloned().unwrap_or(Value::Null)),
                Behavior::HostError => Err(HostCallError::Host {
                    category: "lookup".to_string(),
                }),
            }
        }
    }

    fn descriptor(id: u128, name: &str, arity: Arity) -> HostFunctionDescriptor {
        HostFunctionDescriptor {
            id: hid(id),
            name: name.to_string(),
            arity,
            cost: HostCost::default(),
            argument_audit: Vec::new(),
            result_audit: AuditValuePolicy::Omit,
            may_block: false,
        }
    }

    fn echo_fn(id: u128, name: &str, arity: Arity) -> Arc<dyn HostFunction> {
        Arc::new(TestFn {
            descriptor: descriptor(id, name, arity),
            behavior: Behavior::Echo,
        })
    }

    // --- descriptor validation（CAP-AT-20 の descriptor 面）---

    #[test]
    fn validate_accepts_well_formed() {
        descriptor(1, "lookup", Arity::Exact(1)).validate().unwrap();
        descriptor(1, "lookup", Arity::Range { min: 0, max: 3 })
            .validate()
            .unwrap();
    }

    #[test]
    fn validate_rejects_range_min_gt_max() {
        let d = descriptor(1, "lookup", Arity::Range { min: 3, max: 1 });
        assert_eq!(
            d.validate(),
            Err(ConfigError::InvalidDescriptor {
                field: "arity",
                code: "range_min_gt_max",
            })
        );
    }

    #[test]
    fn validate_rejects_bad_name() {
        assert!(matches!(
            descriptor(1, "1bad", Arity::Exact(0)).validate(),
            Err(ConfigError::InvalidDescriptor { .. })
        ));
        assert!(matches!(
            descriptor(1, "", Arity::Exact(0)).validate(),
            Err(ConfigError::EmptyIdentifier { .. })
        ));
        let long = "a".repeat(65);
        assert!(matches!(
            descriptor(1, &long, Arity::Exact(0)).validate(),
            Err(ConfigError::IdentifierTooLong { .. })
        ));
    }

    #[test]
    fn validate_rejects_mismatched_argument_audit_len() {
        let mut d = descriptor(1, "lookup", Arity::Exact(2));
        d.argument_audit = vec![AuditValuePolicy::Omit]; // 長さ 1 ≠ arity 2
        assert_eq!(
            d.validate(),
            Err(ConfigError::InvalidDescriptor {
                field: "argument_audit",
                code: "length_mismatch",
            })
        );
    }

    // --- registry build collision（CAP-AT-20）---

    /// builder は Debug でないため、register 失敗は `err()` で取り出して判定する。
    fn register_err(function: Arc<dyn HostFunction>) -> ConfigError {
        HostFunctionRegistry::builder()
            .register(function)
            .err()
            .expect("registration must fail")
    }

    #[test]
    fn register_rejects_keyword_collision() {
        assert!(matches!(
            register_err(echo_fn(1, "let", Arity::Exact(0))),
            ConfigError::DuplicateCallableName { .. }
        ));
    }

    #[test]
    fn register_rejects_builtin_collision() {
        // `len` は core builtin、`print` は予約語 keyword。どちらも衝突。
        assert!(matches!(
            register_err(echo_fn(1, "len", Arity::Exact(1))),
            ConfigError::DuplicateCallableName { .. }
        ));
        assert!(matches!(
            register_err(echo_fn(1, "print", Arity::Exact(1))),
            ConfigError::DuplicateCallableName { .. }
        ));
    }

    #[test]
    fn register_rejects_duplicate_name_and_id() {
        let dup_name = HostFunctionRegistry::builder()
            .register(echo_fn(1, "lookup", Arity::Exact(1)))
            .unwrap()
            .register(echo_fn(2, "lookup", Arity::Exact(1)))
            .err()
            .expect("duplicate name must fail");
        assert!(matches!(
            dup_name,
            ConfigError::DuplicateCallableName { .. }
        ));

        let dup_id = HostFunctionRegistry::builder()
            .register(echo_fn(1, "lookup", Arity::Exact(1)))
            .unwrap()
            .register(echo_fn(1, "other", Arity::Exact(1)))
            .err()
            .expect("duplicate id must fail");
        assert!(matches!(
            dup_id,
            ConfigError::DuplicateCapability {
                kind: crate::capability::CapabilityKind::HostFunction
            }
        ));
    }

    #[test]
    fn register_accepts_business_name() {
        // `http_request` のような承認済み業務名は登録できる（第11.1節）。
        HostFunctionRegistry::builder()
            .register(echo_fn(1, "http_request", Arity::Range { min: 1, max: 2 }))
            .unwrap()
            .build()
            .unwrap();
    }

    // --- redaction serializer（CAP-AT-22）---

    #[test]
    fn redact_produces_no_value_body() {
        let secret = Value::str_constant("SUPER_SECRET".to_string());
        assert_eq!(redact(AuditValuePolicy::Omit, &secret), AuditValue::Omitted);
        assert_eq!(
            redact(AuditValuePolicy::TypeOnly, &secret),
            AuditValue::Type { type_name: "str" }
        );
        // LengthOnly は型と value unit 数（str = 1 + byte 数）だけ。本文は出ない。
        assert_eq!(
            redact(AuditValuePolicy::LengthOnly, &secret),
            AuditValue::TypeAndLength {
                type_name: "str",
                length: 1 + "SUPER_SECRET".len() as u64,
            }
        );
        // どの表現にも本文が現れない。
        for policy in [
            AuditValuePolicy::Omit,
            AuditValuePolicy::TypeOnly,
            AuditValuePolicy::LengthOnly,
        ] {
            let dbg = format!("{:?}", redact(policy, &secret));
            assert!(!dbg.contains("SUPER_SECRET"), "本文が漏洩: {dbg}");
        }
    }

    #[test]
    fn value_units_counts_recursively() {
        // list[int, str("ab")] = 1(list) + 1(int) + (1+2)(str) = 5。
        let list = Value::List(crate::value::Tracked::constant(vec![
            Value::Int(1),
            Value::str_constant("ab".to_string()),
        ]));
        assert_eq!(value_units(&list), 5);
    }

    // --- call resolution（CAP-AT-19/21）---

    fn granted_set(id: u128) -> crate::capability::CapabilitySet {
        crate::capability::CapabilitySet::builder()
            .grant_host_function(hid(id))
            .unwrap()
            .build()
    }

    #[test]
    fn unknown_name_returns_none() {
        let registry = HostFunctionRegistry::empty();
        let set = crate::capability::CapabilitySet::empty();
        let out = resolve_host_call(&registry, &set, "nope", &[], 1).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn registered_but_ungranted_is_capability_error() {
        let registry = HostFunctionRegistry::builder()
            .register(echo_fn(1, "lookup", Arity::Exact(1)))
            .unwrap()
            .build()
            .unwrap();
        let set = crate::capability::CapabilitySet::empty();
        let err = resolve_host_call(&registry, &set, "lookup", &[Value::Int(1)], 1)
            .expect_err("ungranted must be capability error");
        assert_eq!(err.error_type(), "capability");
    }

    #[test]
    fn registered_and_granted_invokes_callback() {
        let registry = HostFunctionRegistry::builder()
            .register(echo_fn(7, "lookup", Arity::Exact(1)))
            .unwrap()
            .build()
            .unwrap();
        let set = granted_set(7);
        let out = resolve_host_call(&registry, &set, "lookup", &[Value::Int(42)], 1)
            .unwrap()
            .expect("granted call returns value");
        assert_eq!(out, Value::Int(42));
    }

    #[test]
    fn granted_arity_mismatch_is_argument_error() {
        let registry = HostFunctionRegistry::builder()
            .register(echo_fn(7, "lookup", Arity::Exact(1)))
            .unwrap()
            .build()
            .unwrap();
        let set = granted_set(7);
        let err = resolve_host_call(&registry, &set, "lookup", &[], 1).expect_err("arity mismatch");
        assert_eq!(err.error_type(), "argument");
    }

    #[test]
    fn host_failure_is_catchable_host_error() {
        let registry = HostFunctionRegistry::builder()
            .register(Arc::new(TestFn {
                descriptor: descriptor(7, "lookup", Arity::Exact(0)),
                behavior: Behavior::HostError,
            }))
            .unwrap()
            .build()
            .unwrap();
        let set = granted_set(7);
        let err = resolve_host_call(&registry, &set, "lookup", &[], 1).expect_err("host failure");
        assert_eq!(err.error_type(), "host");
    }
}
