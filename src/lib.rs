//! Tsumugi — ライブラリクレート（ベンチマーク・外部ツールからの利用用）
//!
//! このクレートは内部モジュールをベンチマークやテストツールから利用可能にするために存在する。
//! API の安定性は保証しない（内部用途）。

#![allow(clippy::new_without_default)]
#![allow(clippy::result_unit_err)]
#![allow(clippy::len_without_is_empty)]

pub mod ast;
pub mod builtin_core;
pub mod chunk;
pub mod compiler;
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
