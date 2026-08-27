//! バイトコード命令セット

/// 破壊的更新の対象となる binding。
///
/// `push` / `pop` の書き戻しと、インデックス代入の in-place 更新で共有する。
/// local / upvalue はコンパイル時に解決し、それ以外は実行時 global として扱う。
#[derive(Debug, Clone, PartialEq)]
pub enum MutationTarget {
    Local(usize),
    Upvalue(usize),
    Global(String),
}

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

    /// 実行時にglobal名を解決し、値をスタックトップにコピーする。
    /// static local/upvalueで解決できなかった識別子だけに使用する。
    GetGlobal(String),

    /// 関数callee用のglobal名解決。未定義時は「未定義の関数」として報告する。
    GetGlobalForCall(String),

    /// スタックトップの値で実行時globalを更新する（値はスタックに残る）。
    SetGlobal(String),

    /// top-level宣言の名前と既存ローカルslotをglobal registryへ登録する。
    /// 値は複製せず、stack/locals_cells上の同じbindingを参照する。
    RegisterGlobal(String, usize),

    /// 実行時globalに名前が登録済みなら指定位置へジャンプする。
    /// builtin名とuser bindingのruntime fallback選択に使用する。
    JumpIfGlobalDefined(String, usize),

    /// 実行時globalが定義済みであることだけを検査する（値は積まない）。
    /// 破壊的更新の対象bindingを、他の被演算子の評価前に検証するために使う。
    RequireGlobal(String),

    // --- ジャンプ ---
    /// 無条件ジャンプ（ip を指定位置に設定）
    Jump(usize),

    /// スタックトップが偽ならジャンプ（値をpop）
    JumpIfFalse(usize),

    /// スタックトップが偽ならジャンプ（値をpopしない — and の短絡評価用）
    JumpIfFalseKeep(usize),

    /// スタックトップが真ならジャンプ（値をpopしない — or の短絡評価用）
    JumpIfTrueKeep(usize),

    /// 後方ジャンプ（ループ先頭に戻る: ip を指定位置に設定）
    Loop(usize),

    // --- 関数呼び出し ---
    /// 呼び出しのstep予算と深度をcallee評価前に検査する
    PrepareCall,

    /// calleeが関数でarityが一致することを引数評価前に検査する
    /// operand: 引数の数。スタックトップのcalleeは保持する
    ValidateCall(usize),

    /// 検査済みの関数を呼び出す。operand: 引数の数
    /// スタック: [fn, arg0, arg1, ...] → 関数を実行
    Call(usize),

    /// 関数から値を返す（スタックトップが戻り値）
    ReturnValue,

    /// upvalue（クロージャがキャプチャした値）をスタックに積む
    GetUpvalue(usize),

    /// upvalue（クロージャがキャプチャした変数）を更新する
    SetUpvalue(usize),

    /// クロージャを作る: スタックの [VmFn, upval0, upval1, ...] → クロージャ値
    /// operand: upvalue の数
    MakeClosure(usize),

    // --- 組み込み関数呼び出し ---
    /// context依存builtinのarityと破壊対象構文を引数評価前に検査する
    /// operand: (関数名, 引数の数, 第1引数が識別子か)
    ValidateBuiltinCall(String, usize, bool),

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

    /// インデックス代入: スタックの [index, value] を pop し、
    /// 対象 binding が保持するコレクションを in-place で更新する（値は積まない）。
    /// binding 全体を書き戻さないため、index/value の評価中に同じ binding へ
    /// 加えられた変更を上書きしない。
    SetIndex(MutationTarget),

    /// for ループ用: コレクションをイテレート可能なリストに変換
    /// List → そのまま、Dict → keys のリスト、Str → 1文字ずつのリスト
    ToIterList,

    /// 組み込み関数呼び出し
    /// operand: (定数テーブル上の関数名インデックス, 引数の数)
    CallBuiltin(usize, usize),

    /// プログラム終了
    Return,

    /// 例外ハンドラを設定する: 失敗時に指定アドレスへジャンプ
    /// operand: catch ブロックの先頭アドレス
    SetupTry(usize),

    /// 例外ハンドラを解除する（try ブロック正常完了時）
    TeardownTry,

    /// f-string 連結: スタックから N 個の値を pop して文字列として連結
    /// operand: パーツ数
    FStrConcat(usize),
}
