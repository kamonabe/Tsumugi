//! バイトコード命令セット

/// VM が実行する命令
#[derive(Debug, Clone, PartialEq)]
pub enum OpCode {
    /// 定数テーブルからスタックに値をロード
    /// operand: 定数テーブルのインデックス
    LoadConst(usize),

    // --- 算術演算（スタックから2つpop → 結果をpush） ---
    Add,
    Sub,
    Mul,
    Div,
    Mod,

    // --- 比較演算 ---
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,

    // --- 論理演算 ---
    Not,

    // --- 単項演算 ---
    Negate,

    // --- 組み込み関数呼び出し ---
    /// print: 引数の数を operand で指定
    Print(usize),

    /// スタックトップを捨てる（式文の結果を破棄）
    Pop,

    /// プログラム終了
    Return,
}
