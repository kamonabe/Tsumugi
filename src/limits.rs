//! 処理系全体で共有する安全上限。

/// AST の最大ネスト深度。
pub(crate) const MAX_AST_DEPTH: usize = 256;

/// root script を除く active import chain の最大深度。
pub(crate) const MAX_IMPORT_DEPTH: usize = 128;

/// ユーザー定義呼び出しフレームの最大深度（スタックオーバーフロー防止）。
///
/// # 何を数えるか
/// root script frame を**除いた**、現在 active な user 定義の関数・lambda・method・
/// init のフレーム数を数える。通常の呼び出しと `map` / `filter` / `each` の callback は
/// 同じ規則で数える。host function と core builtin 自体は user frame として数えない。
///
/// # いつ拒否するか
/// 128 個目の user frame は実行可能で、129 個目を作る直前に拒否する。動的 callee は
/// 先に一度だけ評価・分類し、user callable の場合だけ引数評価より前に深度を検査する。
///
/// tree-walk 版は user frame だけを積む `call_stack` の長さで、VM 版は root frame を
/// 除いた active user frame 数（`frames.len() - 1`）で、同じ境界を判定する。
pub(crate) const MAX_USER_CALL_DEPTH: usize = 128;

/// List / Dict の最大ネスト深度（semantic-decisions §21.3）。
///
/// スカラー・関数値・エラー値の深度は 0、List の深度は「1 + 要素の深度の最大」、Dict は
/// 「1 + 値の深度の最大」（空は 1）。深度 256 の値は作れ、257 になる操作を値を作る前に
/// `overflow` で拒否する。`MAX_AST_DEPTH` と同じ値なので、source のリテラルとして書ける
/// 入れ子はすべて値として作れる。コンパイル時定数で、設定の入口は設けない。
pub(crate) const MAX_VALUE_DEPTH: usize = 256;
