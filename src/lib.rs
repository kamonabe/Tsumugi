//! Tsumugi — ライブラリクレート
//!
//! 埋め込み利用では [`Engine`]、[`CompiledScript`]、[`ExecutionContext`] を使う。
//! これらの crate root re-export が現時点の埋め込み入口である。個別モジュールは既存の
//! ベンチマーク・テストツールとの互換性のため公開しており、埋め込み API としての
//! 安定性は保証しない。crate 全体は引き続き alpha 段階である。

#![allow(clippy::new_without_default)]
#![allow(clippy::result_unit_err)]
#![allow(clippy::len_without_is_empty)]

pub mod ast;
pub mod audit;
pub mod budget;
pub mod builtin_core;
pub mod builtin_registry;
pub mod capability;
pub mod embedding;
pub mod engine;
pub mod env;
pub mod error;
pub mod eval;
pub mod host_function;
pub mod host_pending;
pub mod lexer;
pub(crate) mod limits;
pub mod module;
pub mod parser;
pub mod sandbox;
pub mod scheduler;
pub mod token;
pub mod value;

// raw bytecode モジュール群（REV-018）。安定 surface は `Engine` 系のみとし、
// これらは `unstable-bytecode` feature でのみ公開する。feature 無効時は crate 内部限定。
#[cfg(feature = "unstable-bytecode")]
pub mod chunk;
#[cfg(not(feature = "unstable-bytecode"))]
pub(crate) mod chunk;

#[cfg(feature = "unstable-bytecode")]
pub mod compiler;
#[cfg(not(feature = "unstable-bytecode"))]
pub(crate) mod compiler;

#[cfg(feature = "unstable-bytecode")]
pub mod opcode;
#[cfg(not(feature = "unstable-bytecode"))]
pub(crate) mod opcode;

#[cfg(feature = "unstable-bytecode")]
pub mod verifier;
#[cfg(not(feature = "unstable-bytecode"))]
pub(crate) mod verifier;

#[cfg(feature = "unstable-bytecode")]
pub mod vm;
#[cfg(not(feature = "unstable-bytecode"))]
pub(crate) mod vm;

pub use engine::{
    AdmissionPhase, CompiledScript, Engine, ExecutionContext, ExecutionHandle, ExecutionOutcome,
    ExecutionRequest, ExecutionState, HandleError, PauseReason, PausedState, PollResult, PollSlice,
    ResumeState, YieldReason,
};

// 協調スケジューラの公開型（REV-015 Slice 5、実行制御仕様 §9 / §12）。`Engine::with_limits`
// の引数 `EngineLimits` と、`create_execution` / `start` の `Result` の失敗型 `StartError` を
// crate root へ re-export する。
pub use scheduler::{EngineLimits, StartError};

// host-call pending プロトコルの公開型（REV-015 Slice 5、実行制御仕様 §9 / 設計 §4.4）。
// cooperative adapter が `Pending` を返す host-call の ticket / waker surface。
pub use host_pending::{ExecutionWaker, HostCallCompleter, HostCallPoll, HostCallTicket, Wake};
// cooperative adapter + bounded executor の公開型（REV-015 Slice 5、設計 §4.4 / FR-8/FR-9）。
// 登録しなければ cooperative path は一切起きない（opt-in、§7）。
pub use host_pending::{
    AdapterExecutor, AdapterExecutorLimits, CancelObserver, CooperativeAdapter, SubmitError,
    new_ticket,
};

// 協調的 cancel token（REV-015 Slice 4、実行制御仕様 §8）。alpha facade の
// `ExecutionHandle::cancellation_token` の戻り値型、および `EmbeddingRequest::cancellation(..)` で
// request に載せる協調 cancel token（REV-015 最終形移行 Slice 2）として公開する。別スレッドへ
// 渡せる `Send + Sync` な `Arc<AtomicBool>` ハンドル。
pub use budget::CancellationToken;

// 実行予算・deadline の公開型（REV-015 最終形移行 Slice 1、実行制御仕様 §3 / §7）。埋め込み
// host は `EmbeddingRequest::new(budget, clock)` で有限 budget と deadline clock を必須所有させ、
// deadline（`budget.deadline`）を実効化する。`FakeClock` は決定的な test / host utility 向けの
// clock 実装。
pub use budget::{
    BudgetConfig, BudgetCounters, BudgetPeaks, BudgetUsage, FakeClock, MonotonicClock,
    MonotonicInstant, SystemMonotonicClock,
};
// `budget::ConfigError` は `embedding::ConfigError` と名前衝突するため、budget 側は
// `BudgetConfigError` として別名公開する。
pub use budget::ConfigError as BudgetConfigError;

// cooperative adapter が catch 不能 terminal（cancel/deadline/budget/internal-control）を運ぶ
// 制御信号型（REV-015 Slice 5、設計 §4.4）。`AdapterError::Control(ControlStop)` に載せる。
pub use budget::{BudgetExceeded, BudgetResource, BudgetUnit, ControlStop, ExecutionPhase};
pub use capability::AdapterError;

// Phase 1 embedding（スライス E1、`docs/embedding-api.md` 第3・8節）の公開型。
//
// 名前が衝突しない型は crate root へそのまま re-export する。alpha facade（`engine`）と
// 衝突する `Engine` / `ExecutionOutcome` / `TraceFrame` は、統合（E10）まで別名で公開する:
// - `embedding::Engine`         → [`EmbeddingEngine`]
// - `embedding::ExecutionOutcome` → [`EmbeddingOutcome`]
// - `embedding::TraceFrame`     → [`EmbeddingTraceFrame`]
pub use embedding::{
    AuditedOutcome, Backend, ConfigError, EngineBuilder, EngineConfig, EngineId, ExecutionError,
    ExecutionId, HostError, HostErrorCode, LanguageRevision, SourceHash, SourceId,
};
pub use embedding::{
    Engine as EmbeddingEngine, ExecutionOutcome as EmbeddingOutcome,
    TraceFrame as EmbeddingTraceFrame,
};
// スライス E2（`docs/embedding-api.md` 第4・5節）: compile / import なし link / hash。
// `CompiledScript` は alpha facade（`engine`）と衝突するため、統合（E10）まで
// `embedding::CompiledScript` → [`EmbeddingCompiledScript`] として別名公開する。
pub use embedding::CompiledScript as EmbeddingCompiledScript;
pub use embedding::{
    CompileDiagnostic, CompileDiagnosticCode, CompileErrors, CompileOptions, ImportGraph,
    ImportNode, LinkError, LinkRequest, LinkedScript, ModuleId, Source,
};
// スライス E3（`docs/embedding-api.md` 第2・8節）: tree backend adapter（実行入口）。
// `ExecutionContext` / `ExecutionRequest` は alpha facade と衝突するため、統合（E10）まで
// `EmbeddingContext` / `EmbeddingRequest` として別名公開する。
pub use embedding::ExecutionContext as EmbeddingContext;
pub use embedding::ExecutionRequest as EmbeddingRequest;
// スライス E4（`docs/embedding-api.md` 第6・9・10節）: Context cleanup と transaction 縦切り。
// `ContextError` は alpha facade と衝突しないため、そのまま re-export する。
pub use embedding::ContextError;

// Phase 2 capability（スライス C1、`docs/capability-model.md` 第3・8・13節）の公開型。
// alpha facade と名前衝突しないため、そのまま crate root へ re-export する。
pub use capability::{
    CapabilityCallContext, CapabilityKind, CapabilitySet, CapabilitySetBuilder, CapabilitySetId,
    Clock, DataClassification, DirectoryHandle, EnvironmentSnapshot, EnvironmentValue,
    FilesystemCapability, FilesystemRoot, FsOperation, HostFunctionId, Input, ModuleChunk,
    ModuleResolver, ModuleSource, MountName, OsDirectoryHandle, Output, ProcessExit,
    ResolveRequest, ResolvedModule, SymlinkPolicy, SystemClock, SystemInput, SystemOutput,
    derive_policy_id,
};

// Phase 2 capability — host function registry（スライス C8/E7、`docs/capability-model.md`
// 第11節）の公開型。埋め込み host はこれらで registry を組み立て、`EngineBuilder::host_functions`
// へ渡す（登録と grant は別、第11.1節）。
//
// `host_function::Arity`（`Exact(u16)` / `Range`）は `builtin_registry::Arity` と同名の別型で、
// crate root での bare な `Arity` は将来曖昧になり得る。そこで `Embedding*` 別名の前例に倣い
// `HostArity` として公開して衝突を避ける。他の host 型は衝突がないためそのままの名前で公開する。
pub use host_function::Arity as HostArity;
pub use host_function::{
    AuditValuePolicy, CooperativeHostFunction, HostCallError, HostCost, HostFunction,
    HostFunctionDescriptor, HostFunctionRegistry, HostFunctionRegistryBuilder,
};

// cooperative host function / adapter テストが script 値を構築・判定するために `Value` を
// crate root へ公開する（REV-015 Slice 5）。host 側は `Response → Value` 変換の責務を担う（OQ-9）。
pub use value::Value;

// Phase 6 実行時監査（audit）の公開型（A-1 スライス、`docs/determinism-and-audit.md`
// §7/§8/§10/§11）。schema v1 の envelope/event、sink 契約、監査予算を crate root へ公開する。
// alpha facade と名前衝突しないため、そのままの名前で re-export する。opt-in: sink を
// `EngineBuilder::audit_sink` で設定しない限り emission は起きない（§1.1 の sink 必須は後続）。
pub use audit::{
    AuditAck, AuditBudget, AuditConfigError, AuditEnvelope, AuditError, AuditErrorPayload,
    AuditEvent, AuditFailure, AuditFrame, AuditJournal, AuditSink, AuditSinkError, AuditSubmit,
    AuditTicket, AuditWaker, BudgetChargeReason, CapabilityDecisionKind, EffectStatus,
    ExecutionMode, HostCallOutcome, HostTimestamp, InMemoryAuditSink, ScriptedAuditSink,
    ScriptedResponse, SyncAuditSink, TerminalOutcome,
};
