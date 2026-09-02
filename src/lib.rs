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
pub mod builtin_core;
pub mod builtin_registry;
pub mod chunk;
pub mod compiler;
pub mod engine;
pub mod env;
pub mod error;
pub mod eval;
pub mod lexer;
pub(crate) mod limits;
pub mod module;
pub mod opcode;
pub mod parser;
pub mod sandbox;
pub mod token;
pub mod value;
pub mod vm;

pub use engine::{CompiledScript, Engine, ExecutionContext, ExecutionOutcome};
