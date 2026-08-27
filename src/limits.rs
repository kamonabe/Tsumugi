//! 処理系全体で共有する安全上限。

/// AST の最大ネスト深度。
pub(crate) const MAX_AST_DEPTH: usize = 256;

/// root script を除く active import chain の最大深度。
pub(crate) const MAX_IMPORT_DEPTH: usize = 128;
