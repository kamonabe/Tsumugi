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

pub use engine::{CompiledScript, Engine, ExecutionContext, ExecutionOutcome};
