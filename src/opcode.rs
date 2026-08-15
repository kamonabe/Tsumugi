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

    // --- 変数操作 ---
    /// ローカル変数の値をスタックトップにコピー
    /// operand: スタック上のスロット位置
    GetLocal(usize),

    /// スタックトップの値でローカル変数を上書き（値はスタックに残る）
    /// operand: スタック上のスロット位置
    SetLocal(usize),

    // --- ジャンプ ---
    /// 無条件ジャンプ（ip を指定位置に設定）
    Jump(usize),

    /// スタックトップが偽ならジャンプ（値をpop）
    JumpIfFalse(usize),

    /// 後方ジャンプ（ループ先頭に戻る: ip を指定位置に設定）
    Loop(usize),

    // --- 関数呼び出し ---
    /// 関数を呼び出す。operand: 引数の数
    /// スタック: [fn, arg0, arg1, ...] → 関数を実行
    Call(usize),

    /// 関数から値を返す（スタックトップが戻り値）
    ReturnValue,

    /// upvalue（クロージャがキャプチャした値）をスタックに積む
    GetUpvalue(usize),

    /// クロージャを作る: スタックの [VmFn, upval0, upval1, ...] → クロージャ値
    /// operand: upvalue の数
    MakeClosure(usize),

    // --- 組み込み関数呼び出し ---
    /// print: 引数の数を operand で指定
    Print(usize),

    /// スタックトップを捨てる（式文の結果を破棄）
    Pop,

    /// 複数のスタックスロットを一度に捨てる（スコープ離脱時）
    PopN(usize),

    // --- コレクション操作 ---
    /// スタックトップのコレクションの長さを取得
    Len,

    /// collection[index]: スタックから index, collection の順に pop → 要素を push
    Index,

    /// リストビルド用: スタックの [list, value] → list に value を push → list を残す
    ListPush,

    /// 辞書ビルド用: スタックの [dict, key, value] → dict に key:value を挿入 → dict を残す
    DictInsert,

    /// インデックス代入: スタックの [collection, index, value] → collection[index] = value
    SetIndex,

    /// for ループ用: コレクションをイテレート可能なリストに変換
    /// List → そのまま、Dict → keys のリスト、Str → 1文字ずつのリスト
    ToIterList,

    /// 組み込み関数呼び出し
    /// operand: (関数名, 引数の数)
    CallBuiltin(String, usize),

    /// プログラム終了
    Return,
}
