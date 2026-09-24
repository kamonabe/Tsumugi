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
pub mod budget;
pub mod builtin_core;
pub mod builtin_registry;
pub mod capability;
pub mod embedding;
pub mod engine;
pub mod env;
pub mod error;
pub mod eval;
pub mod lexer;
pub(crate) mod limits;
pub mod module;
pub mod parser;
pub mod sandbox;
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
    CompiledScript, Engine, ExecutionContext, ExecutionHandle, ExecutionOutcome, ExecutionRequest,
    ExecutionState, HandleError, PauseReason, PausedState, PollResult, PollSlice, ResumeState,
    YieldReason,
};

// Phase 1 embedding（スライス E1、`docs/embedding-api.md` 第3・8節）の公開型。
//
// 名前が衝突しない型は crate root へそのまま re-export する。alpha facade（`engine`）と
// 衝突する `Engine` / `ExecutionOutcome` / `TraceFrame` は、統合（E10）まで別名で公開する:
// - `embedding::Engine`         → [`EmbeddingEngine`]
// - `embedding::ExecutionOutcome` → [`EmbeddingOutcome`]
// - `embedding::TraceFrame`     → [`EmbeddingTraceFrame`]
pub use embedding::{
    Backend, ConfigError, EngineBuilder, EngineConfig, EngineId, ExecutionError, ExecutionId,
    HostError, HostErrorCode, LanguageRevision, SourceHash, SourceId,
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
    CapabilityKind, CapabilitySet, CapabilitySetBuilder, CapabilitySetId, Clock,
    DataClassification, DirectoryHandle, EnvironmentSnapshot, EnvironmentValue,
    FilesystemCapability, FilesystemRoot, FsOperation, HostFunctionId, Input, ModuleResolver,
    MountName, Output, ProcessExit, SymlinkPolicy,
};
