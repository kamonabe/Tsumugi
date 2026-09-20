use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;

use crate::budget::{AllocationId, ControlStop, ExecutionPhase, HeapLedgerWeak, heap_size};
use crate::chunk::Chunk;

/// 共有可能な変数セル（参照キャプチャ用）。
///
/// backing は [`TrackedCell`] で、生成時に §5.1 の captured cell（32 byte 固定）を
/// live heap へ課金し、最後の参照 drop で release する（REV-015 PR-c、§5.2）。
/// `Rc::clone`（closure capture・代入）は無課金の共有。cell の中身の書き換えは
/// `Deref` 経由の `RefCell` 内部可変で行い、サイズは 32 固定なので再課金しない。
pub type SharedValue = Rc<TrackedCell>;
/// tracked な変数 cell backing（`RefCell<Value>`）。
pub type TrackedCell = Tracked<RefCell<Value>>;
/// 関数 instance header の heap 課金トークン（REV-015 PR-c）。
///
/// data を持たない（`()`）tracked allocation で、生成時に §5.1 の function instance
/// header（tree: 64 + 16×captured / VM: 48 + 16×upvalue）を live heap へ課金し、最後の
/// 参照 drop で release する。captured / upvalue cell 実体は cell 側で別途課金するため、
/// header だけをこのトークンで持つ（二重計上を避ける）。
pub type FnHeader = Tracked<()>;

/// heap object 汎用の課金トークン（REV-015 PR-d）。
///
/// [`FnHeader`] と同じ data を持たない（`()`）tracked allocation で、`Value` ツリーに
/// 現れず所有構造側が保持する heap object（AST program root / node、bytecode chunk、
/// imported module record、rollback journal entry）の per-drop 追跡に使う。生成時に
/// §5.1 の論理サイズを live heap へ課金し、最後の参照 drop で release する。
pub type HeapToken = Tracked<()>;

/// heap 課金付きの collection backing（REV-015 案A、per-drop release）。
///
/// `T`（`Vec<Value>` または `BTreeMap<String, Value>`）と、その論理サイズ・
/// [`AllocationId`]・heap 台帳への弱参照を束ねる。`Drop` で台帳へ `release` を通知し、
/// この backing を指す最後の `Rc` が落ちた瞬間に live heap を戻す（§5.2）。
///
/// # 課金と共有
///
/// 生成（[`Tracked::new`]）だけが台帳へ `charge` する fallible な入口である。`Rc` の
/// clone（＝共有）は無課金で、`Drop` は最後の 1 本が落ちたときだけ発火するため、
/// 「共有は課金しない・最後の参照で release」という §5.2 の規則を `Rc` の refcount が
/// そのまま担保する。台帳が engine より先に drop された場合、`Weak::upgrade` が `None`
/// を返し release は no-op になる（その時点で live 集計は不要）。
///
/// # `Deref`
///
/// 読み取りは `Deref<Target = T>` 経由で透過的に行えるため、既存の `items.len()` /
/// `map.iter()` 等はそのまま動く。書き込みは呼び出し側が台帳を持つ fallible 経路で
/// 新しい `Tracked` を作る（COW detach）。`Tracked` は不変とみなし、内部可変はしない。
pub struct Tracked<T> {
    data: T,
    id: AllocationId,
    bytes: u64,
    ledger: HeapLedgerWeak,
}

impl<T> Tracked<T> {
    /// backing を台帳へ課金してから包む（§5.2）。`logical_bytes` は §5.1 の論理サイズ。
    ///
    /// `charge` が上限超過・overflow なら `ControlStop` を返し、`Tracked` は作られない
    /// （＝ live heap は増えない）。`AllocationId` は成功時に 1 個発番する。
    pub fn new(
        data: T,
        logical_bytes: u64,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<Rc<Self>, ControlStop> {
        // charge を先に行い、成功した場合だけ id を発番して包む。
        budget.charge_heap(logical_bytes, phase)?;
        let id = budget.allocate_heap_id()?;
        Ok(Rc::new(Self {
            data,
            id,
            bytes: logical_bytes,
            ledger: budget.heap_handle(),
        }))
    }

    /// compile 時定数用の untracked backing を作る（課金しない）。
    ///
    /// bytecode の定数プールに置く空の List/Dict リテラル用。ledger を持たない
    /// （`Weak::new()`）ため `Drop` は no-op、`AllocationId(0)` は「未割り当て」の
    /// sentinel である。実行時に最初の mutation（`ListPush`/`DictInsert`）で
    /// [`Value::detach_list`]/[`Value::detach_dict`] が共有中の定数を検出し、runtime
    /// 台帳で課金した実 backing へ複製・差し替える（§5.2 COW）。定数自体は live heap を
    /// 増減させない。
    pub fn constant(data: T) -> Rc<Self> {
        Rc::new(Self {
            data,
            id: AllocationId(0),
            bytes: 0,
            ledger: HeapLedgerWeak::new(),
        })
    }

    /// heap 台帳への弱参照だけを使って課金してから包む（`&mut BudgetLedger` を持てない
    /// 文脈用、REV-015 PR-c）。
    ///
    /// [`Tracked::new`] は `&mut BudgetLedger` を要求するが、変数 cell は `Env`（tree）や
    /// frame（VM）が所有し、`Value` ツリーに現れないため dispatch 境界の `track_result`
    /// では拾えない。cell の生成点は `&mut BudgetLedger` を持たないことがあるので、
    /// 台帳への `Weak` を直接受け取り、`upgrade` して `charge` / `allocate_id` する。
    ///
    /// - `ledger` が `upgrade` できない（＝台帳がない / drop 済み）場合は課金せず untracked
    ///   （`AllocationId(0)`・`bytes = 0`・`Drop` no-op）で包む。`Env` 単体テストのように
    ///   台帳を持たない文脈では live heap を増減させない。
    /// - `charge` が上限超過・overflow なら `ControlStop` を返し、`Tracked` は作られない。
    pub fn new_via_handle(
        data: T,
        logical_bytes: u64,
        ledger: &HeapLedgerWeak,
        phase: ExecutionPhase,
    ) -> Result<Rc<Self>, ControlStop> {
        let Some(strong) = ledger.upgrade() else {
            // 台帳なし: untracked で包む（課金・release とも no-op）。
            return Ok(Rc::new(Self {
                data,
                id: AllocationId(0),
                bytes: 0,
                ledger: HeapLedgerWeak::new(),
            }));
        };
        let id = {
            let mut l = strong.borrow_mut();
            l.charge(logical_bytes, phase)?;
            l.allocate_id()?
        };
        Ok(Rc::new(Self {
            data,
            id,
            bytes: logical_bytes,
            ledger: ledger.clone(),
        }))
    }

    /// この backing の [`AllocationId`]（同一性・デバッグ用）。
    pub fn alloc_id(&self) -> AllocationId {
        self.id
    }

    /// この backing の課金済み論理サイズ。
    pub fn logical_bytes(&self) -> u64 {
        self.bytes
    }
}

impl<T> Tracked<T> {
    /// 中身への可変参照（一意所有時の in-place mutation 用、value.rs 内部専用）。
    ///
    /// `Rc::get_mut` で一意所有を確認した後にだけ呼ぶ。論理サイズの再計算・課金は
    /// 呼び出し側（`retrack_*`）の責務で、ここでは data への `&mut` を返すだけ。
    fn data_mut(&mut self) -> &mut T {
        &mut self.data
    }
}

impl<T: Clone> Tracked<T> {
    /// 中身の `T` を clone で取り出す（`track_result` の untracked backing 変換用）。
    ///
    /// untracked backing（`bytes == 0` / `AllocationId(0)`）に対してのみ呼ぶ。その場合
    /// `Drop` の release は元々 no-op なので、data を clone しても二重計上・二重解放は
    /// 起きない。tracked backing に使うと clone 後の元 `Tracked` の Drop が release して
    /// しまうため使わない。
    pub fn clone_data(&self) -> T {
        self.data.clone()
    }
}

impl<T> Deref for Tracked<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.data
    }
}

impl<T> Drop for Tracked<T> {
    fn drop(&mut self) {
        // この backing を指す最後の Rc が落ちた瞬間に呼ばれる（§5.2 release）。
        // 台帳がまだ生きていれば論理サイズを戻す。engine drop 後なら no-op。
        if let Some(ledger) = self.ledger.upgrade() {
            ledger.borrow_mut().release(self.bytes);
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Tracked<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 論理サイズや id は出さず、中身だけを表示する（既存の Debug 互換）。
        self.data.fmt(f)
    }
}

/// tracked な `List` backing（`Vec<Value>`）。
pub type TrackedList = Tracked<Vec<Value>>;
/// tracked な `Dict` backing（`BTreeMap<String, Value>`）。
pub type TrackedDict = Tracked<BTreeMap<String, Value>>;
/// tracked な `Str` backing（`String`）。生成時に §5.1 の String body を課金し、
/// 最後の参照 drop で release する（REV-015 PR-b、§5.2）。
pub type TrackedStr = Tracked<String>;

/// [`Value::index_set_tracked`] へ渡す、正規化済みの index 代入ターゲット。
///
/// 呼び出し側が index の型・範囲（List は負数正規化・範囲内、Dict は key 文字列）を
/// 検査してから構築する。
pub enum IndexTarget {
    /// List の 0 始まり index（負数は呼び出し側で正規化済み）。
    ListIndex(usize),
    /// Dict の key。
    DictKey(String),
}

/// 関数値の同一性を表す識別子（AUD-048）
///
/// 関数式・関数定義を実行して値を生成するたびに、ExecutionContext 内で
/// 単調増加する値を 1 つ割り当てる。clone・変数代入・引数渡し・collection 格納では
/// 同じ ID を保持する。tree/VM の関数等価性は FunctionId だけで判定する。
/// ID は実行内の比較専用で、script や audit event へ生値を公開しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionId(pub u64);

/// 数値の厳密比較の結果（REV-003）
///
/// `Less` / `Equal` / `Greater` は全順序上の位置を表す。`UnorderedNaN` は
/// NaN が絡む比較で、大小でも等価でもないことを表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericOrdering {
    Less,
    Equal,
    Greater,
    UnorderedNaN,
}

impl NumericOrdering {
    /// `<` の結果。`UnorderedNaN`（NaN 比較）は false。
    pub fn is_lt(self) -> bool {
        matches!(self, NumericOrdering::Less)
    }
    /// `<=` の結果。`UnorderedNaN` は false。
    pub fn is_le(self) -> bool {
        matches!(self, NumericOrdering::Less | NumericOrdering::Equal)
    }
    /// `>` の結果。`UnorderedNaN` は false。
    pub fn is_gt(self) -> bool {
        matches!(self, NumericOrdering::Greater)
    }
    /// `>=` の結果。`UnorderedNaN` は false。
    pub fn is_ge(self) -> bool {
        matches!(self, NumericOrdering::Greater | NumericOrdering::Equal)
    }
}

/// Int / Float の厳密比較を担う単一実装（REV-003）
///
/// tree・VM・`PartialEq`・関係演算子・`min`・`max` がすべてこの実装を経由し、
/// backend 間で結果が食い違わないようにする。
///
/// Int×Float は Int を `f64` へ丸めず、Float を整数部と小数部へ分離して
/// 数学的に厳密な順序を返す。これにより `2^53` 近傍での等価の非推移性
/// （`a == f` かつ `f == b` だが `a != b`）が解消する。
pub struct NumericOrder;

impl NumericOrder {
    /// 2 つの値を数値として比較する。
    ///
    /// 両者が数値（Int / Float）でない場合は `None` を返す。呼び出し側が
    /// 型エラーにするか非等価として扱うかを決める。
    pub fn compare(a: &Value, b: &Value) -> Option<NumericOrdering> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Some(Self::from_ord(x.cmp(y))),
            (Value::Float(x), Value::Float(y)) => Some(Self::compare_float_float(*x, *y)),
            (Value::Int(x), Value::Float(y)) => Some(Self::compare_int_float(*x, *y)),
            (Value::Float(x), Value::Int(y)) => Some(Self::flip(Self::compare_int_float(*y, *x))),
            _ => None,
        }
    }

    /// 数値として等価なら true。NaN が絡む場合と非数値は false。
    pub fn numeric_eq(a: &Value, b: &Value) -> bool {
        matches!(Self::compare(a, b), Some(NumericOrdering::Equal))
    }

    /// 数値の大小比較。両者が数値なら `Some(NumericOrdering)`、非数値なら `None`。
    /// NaN が絡む比較は `Some(UnorderedNaN)` を返し、呼び出し側で `<`/`>` などを
    /// すべて false にする。
    pub fn compare_relational(a: &Value, b: &Value) -> Option<NumericOrdering> {
        Self::compare(a, b)
    }

    fn from_ord(o: std::cmp::Ordering) -> NumericOrdering {
        match o {
            std::cmp::Ordering::Less => NumericOrdering::Less,
            std::cmp::Ordering::Equal => NumericOrdering::Equal,
            std::cmp::Ordering::Greater => NumericOrdering::Greater,
        }
    }

    fn flip(o: NumericOrdering) -> NumericOrdering {
        match o {
            NumericOrdering::Less => NumericOrdering::Greater,
            NumericOrdering::Greater => NumericOrdering::Less,
            other => other,
        }
    }

    fn compare_float_float(x: f64, y: f64) -> NumericOrdering {
        match x.partial_cmp(&y) {
            Some(o) => Self::from_ord(o),
            None => NumericOrdering::UnorderedNaN,
        }
    }

    /// `i64` と `f64` の厳密比較（丸め・浮動小数演算を経由しない）。
    ///
    /// アルゴリズム（semantic-decisions §17.1.3）:
    /// - NaN は `UnorderedNaN`
    /// - `±Infinity` は全ての有限 Int より大小が確定
    /// - 有限 Float は `trunc` で整数部を取り、`i128` へ widen して Int と比較する。
    ///   整数部が等しいときは小数部（`fract`）の符号で決着させる。
    ///
    /// `f.trunc()` は絶対値が `2^53` 以上の f64 では小数部を持たないため、
    /// `trunc` 値は常に f64 で正確に表現でき、`i128` へロスなく変換できる。
    fn compare_int_float(i: i64, f: f64) -> NumericOrdering {
        if f.is_nan() {
            return NumericOrdering::UnorderedNaN;
        }
        if f == f64::INFINITY {
            return NumericOrdering::Less; // i < +inf
        }
        if f == f64::NEG_INFINITY {
            return NumericOrdering::Greater; // i > -inf
        }

        // 有限 Float。整数部と小数部へ分離する（丸めではなく分離なので厳密）。
        let trunc = f.trunc(); // 小数部を切り捨てた整数値（f64 で正確）
        let fract = f - trunc; // 小数部（-1 < fract < 1）

        // i64 の全値は i128 に収まる。Float の整数部は `1e300` のように
        // i128 範囲を超え得るため、範囲外は Float の magnitude が Int を上回る
        // として符号で決着する（`trunc as i128` の飽和には依存しない）。
        const I128_MAX_AS_F64: f64 = i128::MAX as f64;
        const I128_MIN_AS_F64: f64 = i128::MIN as f64;
        if trunc >= I128_MAX_AS_F64 {
            return NumericOrdering::Less; // i < +巨大 Float
        }
        if trunc <= I128_MIN_AS_F64 {
            return NumericOrdering::Greater; // i > -巨大 Float
        }

        let trunc_i128 = trunc as i128;
        let i_i128 = i as i128;

        match i_i128.cmp(&trunc_i128) {
            std::cmp::Ordering::Less => NumericOrdering::Less,
            std::cmp::Ordering::Greater => NumericOrdering::Greater,
            std::cmp::Ordering::Equal => {
                // 整数部が一致。小数部の符号で決着する。
                if fract > 0.0 {
                    // f = trunc + fract > i
                    NumericOrdering::Less
                } else if fract < 0.0 {
                    // f = trunc + fract < i
                    NumericOrdering::Greater
                } else {
                    NumericOrdering::Equal
                }
            }
        }
    }
}

/// ツリーウォーク用関数の不変部分
///
/// 呼び出しごとに本体ASTを複製しないよう `Rc` で共有する。
/// VM側の `VmFn` が `Rc<Chunk>` で同じ問題を避けているのと同じ方針。
#[derive(Debug)]
pub struct FnDef {
    /// 関数名（無名関数は `<lambda>`）
    pub name: String,
    pub params: Vec<String>,
    /// 本体。`Block`（= `Rc<[Stmt]>`）で共有し、永続 continuation の call frame が
    /// 本体 AST の寿命を保てるようにする（REV-015 Slice 3 PR-d）。
    pub body: crate::ast::Block,
}

/// Tsumugi の実行時の値
#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    /// 文字列。backing は [`TrackedStr`] で、生成時に heap 課金し最後の参照 drop で
    /// release する（REV-015 PR-b、§5.2）。読み取りは `Deref<Target = String>` 経由で
    /// 透過する。cumulative `StringAllocations`/`StringBytes` 会計（§5.3）は別で、
    /// 解放しても減らさない。連結・substring は現行どおり新しい backing を作る
    /// （§5.3 の共有最適化は本 PR の範囲外）。
    Str(Rc<TrackedStr>),
    Bool(bool),
    Null,
    /// リスト。copy-on-write（AUD-047）。clone はハンドル共有で O(1)、
    /// mutation は書き込み時だけ backing を複製する。backing は [`TrackedList`] で、
    /// 生成時に heap 課金し最後の参照 drop で release する（REV-015 案A、§5.2）。
    List(Rc<TrackedList>),
    /// 辞書。copy-on-write（AUD-047）。List と同じく backing は [`TrackedDict`]。
    Dict(Rc<TrackedDict>),
    /// 関数値（ツリーウォーク用: ユーザー定義関数を値として扱う）
    /// `Rc` により関数呼び出し・self-binding・クロージャ生成時のディープコピーを回避
    Fn {
        /// 関数値の同一性（AUD-048）。生成のたびに新規発番、clone では保持
        id: FunctionId,
        /// 定義時に確定する不変部分（名前・引数・本体）
        def: Rc<FnDef>,
        /// 定義時にキャプチャした変数セル。セル自体は参照共有される
        captured: Rc<HashMap<String, SharedValue>>,
        /// tree function instance header（§5.1 tree_function = 64 + 16×captured）の
        /// heap 課金トークン（REV-015 PR-c）。生成時に課金し、最後の参照 drop で release
        /// する。captured cell 実体は cell 側（`SharedValue`）で別途課金するため、ここでは
        /// header ぶんだけを持つ。`clone`（＝関数値の共有）は `Rc` ハンドル共有で無課金。
        header: Rc<FnHeader>,
    },
    /// VM用関数値（コンパイル済みバイトコード）
    /// Rc<Chunk> により関数呼び出し・クロージャ生成時のディープコピーを回避
    VmFn {
        /// 関数値の同一性（AUD-048）。生成のたびに新規発番、clone では保持
        id: FunctionId,
        name: String,
        arity: usize,
        params: Vec<String>,
        chunk: Rc<Chunk>,
        /// クロージャがキャプチャした値（参照キャプチャ方式）
        upvalues: Vec<SharedValue>,
        /// VM function instance header（§5.1 vm_function = 48 + 16×upvalue）の heap 課金
        /// トークン（REV-015 PR-c）。生成時に課金し、最後の参照 drop で release する。
        /// upvalue cell 実体は cell 側で別途課金するため header ぶんだけを持つ。
        header: Rc<FnHeader>,
    },
    /// 構造化エラー値（try/catch で捕捉したエラー）
    /// Display では message を返すため、既存の文字列結合と互換性がある。
    /// インデックスアクセスで "type" / "message" / "line" を取得可能。
    Error {
        error_type: String,
        message: String,
        line: usize,
    },
}

impl PartialEq for Value {
    /// 規範仕様の等価比較（AUD-014）
    ///
    /// 全ての型の組み合わせで結果を返し、型エラーにしない。型が違う値は等しくない。
    /// 数値だけは例外で、IntとFloatを数値として比較する（`1 == 1.0` は true）。
    /// List / Dict / Error は構造で比較し、関数値は同一の関数値とだけ等しい。
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            // 数値はIntとFloatを跨いで厳密比較する（REV-003）。
            // Int を f64 へ丸めず NumericOrder を経由するため、2^53 超でも
            // 等価が対称・推移的になる。NaN が絡む比較は非等価。
            (Value::Int(_), Value::Float(_)) | (Value::Float(_), Value::Int(_)) => {
                NumericOrder::numeric_eq(self, other)
            }
            // 共有 backing（同じ Rc）なら省略。分離済みでも中身の String で比較する。
            (Value::Str(a), Value::Str(b)) => Rc::ptr_eq(a, b) || ***a == ***b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            // 共有 backing（同じ Rc）なら要素比較を省く。分離済みでも要素で比較する。
            // `Tracked` は `Deref` で中身へ透過するため、`***` で `Vec`/`BTreeMap` を比較する。
            (Value::List(a), Value::List(b)) => Rc::ptr_eq(a, b) || ***a == ***b,
            (Value::Dict(a), Value::Dict(b)) => Rc::ptr_eq(a, b) || ***a == ***b,
            // 関数値は FunctionId だけで同一性を判定する（AUD-048）。
            // ID は関数式・関数定義を評価して値を生成するたびに新規発番され、
            // clone・代入・引数渡し・collection 格納では保持される。
            // capture の有無や backend（tree/VM）に関係なく、同じ ID の値だけが等しい。
            (Value::Fn { id: id_a, .. }, Value::Fn { id: id_b, .. }) => id_a == id_b,
            (Value::VmFn { id: id_a, .. }, Value::VmFn { id: id_b, .. }) => id_a == id_b,
            (
                Value::Error {
                    error_type: t1,
                    message: m1,
                    line: l1,
                },
                Value::Error {
                    error_type: t2,
                    message: m2,
                    line: l2,
                },
            ) => t1 == t2 && m1 == m2 && l1 == l2,
            _ => false,
        }
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "Int({})", n),
            Value::Float(n) => write!(f, "Float({})", n),
            Value::Str(s) => write!(f, "Str({:?})", s.as_str()),
            Value::Bool(b) => write!(f, "Bool({})", b),
            Value::Null => write!(f, "Null"),
            Value::List(items) => write!(f, "List({:?})", items),
            Value::Dict(map) => write!(f, "Dict({:?})", map),
            Value::Fn { def, .. } => {
                write!(f, "Fn({}, params={:?})", def.name, def.params)
            }
            Value::VmFn { name, arity, .. } => {
                write!(f, "VmFn({}, arity={})", name, arity)
            }
            Value::Error {
                error_type,
                message,
                line,
            } => {
                write!(f, "Error({}: {} at line {})", error_type, message, line)
            }
        }
    }
}

impl Value {
    /// `Vec<Value>` から heap 課金済みの `Value::List` を作る（REV-015 案A、§5.2）。
    ///
    /// §5.1 の List body（24 + 32 × 要素数）を課金してから包む。上限超過は
    /// `ControlStop` を返し、live heap は増えない。要素自体の Value slot / 子 payload は
    /// 各要素の allocation（別 `Tracked` や String 課金）で数えるため、ここでは backing
    /// の body だけを課金する（baseline 走査の per-node 課金とは役割が異なる）。
    pub fn new_list(
        items: Vec<Value>,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<Value, ControlStop> {
        let bytes = heap_size::list_body(items.len() as u64);
        Ok(Value::List(Tracked::new(items, bytes, budget, phase)?))
    }

    /// `String` から heap 課金済みの `Value::Str` を作る（REV-015 PR-b、§5.2）。
    ///
    /// §5.1 の String body（24 + UTF-8 byte 長）を live heap（`HeapBytes`）へ課金してから
    /// 包む。上限超過は `ControlStop` を返し、live heap は増えない。cumulative
    /// `StringAllocations`/`StringBytes`（§5.3）はこのヘルパでは触らない。builtin
    /// 経由で生成する String はこの live heap 課金を dispatch 境界の `track_result` が
    /// 担うため、builtin_core は [`Value::str_constant`] で untracked に作る。
    pub fn new_str(
        s: String,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<Value, ControlStop> {
        let bytes = heap_size::string_body(s.len() as u64);
        Ok(Value::Str(Tracked::new(s, bytes, budget, phase)?))
    }

    /// heap 課金しない untracked な `Value::Str` を作る。
    ///
    /// compile 時定数・builtin_core の生成・パターンマッチ束縛の再構築など、runtime 台帳を
    /// 持たない文脈で使う。`AllocationId(0)`・`Drop` no-op で live heap を増減させない。
    /// runtime で live heap に載せたい場合は dispatch 境界の `track_result` か
    /// [`Value::new_str`] を通す（§5.2 COW と同じ昇格規則）。
    pub fn str_constant(s: String) -> Value {
        Value::Str(Tracked::constant(s))
    }

    /// `&str` から untracked な `Value::Str` を作る（`str_constant` の借用版）。
    pub fn str_from(s: &str) -> Value {
        Value::str_constant(s.to_string())
    }

    /// heap 課金済みの変数 cell（[`SharedValue`]）を作る（REV-015 PR-c、§5.2）。
    ///
    /// §5.1 の captured cell（32 byte 固定）を heap 台帳への弱参照経由で課金し、最後の
    /// 参照 drop で release する。cell は `Env`（tree）/ frame（VM）が所有し `Value` ツリー
    /// に現れないため、`&mut BudgetLedger` ではなく台帳への `Weak` を直接受け取る。台帳を
    /// 持たない文脈（`Env` 単体テストなど）では untracked（無課金）で包む。
    pub fn new_cell(
        value: Value,
        ledger: &crate::budget::HeapLedgerWeak,
        phase: ExecutionPhase,
    ) -> Result<SharedValue, ControlStop> {
        Tracked::new_via_handle(RefCell::new(value), heap_size::CAPTURED_CELL, ledger, phase)
    }

    /// heap 課金しない untracked な変数 cell を作る（台帳を持たない文脈用）。
    pub fn cell_untracked(value: Value) -> SharedValue {
        Tracked::constant(RefCell::new(value))
    }

    /// tree function instance header（§5.1 tree_function = 64 + 16×captured）の heap 課金
    /// トークンを作る（REV-015 PR-c）。生成時に live heap へ課金し drop で release する。
    /// 台帳を持たない文脈では untracked（無課金）で包む。
    pub fn new_tree_fn_header(
        captured_count: u64,
        ledger: &crate::budget::HeapLedgerWeak,
        phase: ExecutionPhase,
    ) -> Result<Rc<FnHeader>, ControlStop> {
        Tracked::new_via_handle((), heap_size::tree_function(captured_count), ledger, phase)
    }

    /// VM function instance header（§5.1 vm_function = 48 + 16×upvalue）の heap 課金
    /// トークンを作る（REV-015 PR-c）。生成時に live heap へ課金し drop で release する。
    pub fn new_vm_fn_header(
        upvalue_count: u64,
        ledger: &crate::budget::HeapLedgerWeak,
        phase: ExecutionPhase,
    ) -> Result<Rc<FnHeader>, ControlStop> {
        Tracked::new_via_handle((), heap_size::vm_function(upvalue_count), ledger, phase)
    }

    /// heap 課金しない untracked な関数 header トークンを作る（台帳を持たない文脈用）。
    pub fn fn_header_untracked() -> Rc<FnHeader> {
        Tracked::constant(())
    }

    /// 論理サイズ `bytes` の heap 課金トークン（`Tracked<()>`）を作る（REV-015 PR-d）。
    ///
    /// AST / bytecode chunk / imported module record / rollback journal entry のように
    /// `Value` ツリーに現れず所有構造側（`ModuleLoader` / `Vm` / `Env` など）が保持する
    /// heap object を per-drop 追跡するための汎用トークン。関数 header（PR-c）と同じ
    /// `Tracked<()>` + 台帳への `Weak` 経由の課金パターンで、生成時に §5.1 の論理サイズ
    /// `bytes` を live heap へ課金し、この token を握る最後の `Rc` が drop した時点で
    /// release する。台帳を持たない文脈では untracked（無課金）で包む。
    pub fn new_heap_token(
        bytes: u64,
        ledger: &crate::budget::HeapLedgerWeak,
        phase: ExecutionPhase,
    ) -> Result<Rc<HeapToken>, ControlStop> {
        Tracked::new_via_handle((), bytes, ledger, phase)
    }

    /// heap 課金しない untracked な heap token を作る（台帳を持たない文脈用）。
    pub fn heap_token_untracked() -> Rc<HeapToken> {
        Tracked::constant(())
    }

    /// `BTreeMap<String, Value>` から heap 課金済みの `Value::Dict` を作る（§5.2）。
    ///
    /// §5.1 の Dict body（24 + 64 × entry 数 + 各 key の byte 長）を課金する。
    pub fn new_dict(
        map: BTreeMap<String, Value>,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<Value, ControlStop> {
        let key_bytes_total: u64 = map
            .keys()
            .map(|k| k.len() as u64)
            .fold(0u64, u64::saturating_add);
        let bytes = heap_size::dict_body(map.len() as u64, key_bytes_total);
        Ok(Value::Dict(Tracked::new(map, bytes, budget, phase)?))
    }

    /// `Value::List` の backing を書き込み用に detach し、`&mut Vec<Value>` を得る
    /// （COW、REV-015 案A）。
    ///
    /// - 一意所有（`Rc::strong_count == 1`）なら複製せず in place で返す。呼び出し側は
    ///   size を変える操作の前後で [`Value::retrack_list`] を使い delta を課金/release する。
    /// - 共有中なら backing を複製し、新しい `AllocationId` で `charge` して差し替える
    ///   （旧 backing は `self` が握る Rc が落ちた時点で release）。複製直後は要素数が
    ///   同じなので追加課金は複製ぶんの body サイズであり、`Tracked::new` が担う。
    ///
    /// 返す `&mut Vec` への push 等で要素数が増える場合の delta 課金は、呼び出し側が
    /// 操作後に [`Value::recharge_list_delta`] で行う。ここでは detach だけを担当する。
    fn detach_list<'a>(
        list: &'a mut Rc<TrackedList>,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<&'a mut Vec<Value>, ControlStop> {
        // untracked な compile 時定数（id==0）は、たとえ一意所有でも実 backing へ
        // 複製して runtime 台帳に載せる（定数を直接書き換えて live 集計から漏らさない）。
        if list.id.0 != 0 && Rc::strong_count(list) == 1 && Rc::weak_count(list) == 0 {
            // 一意所有かつ tracked 済み。複製せず in place で書き換える。
            // Rc::get_mut は strong==1 && weak==0 のとき Some。
            let tracked = Rc::get_mut(list).expect("strong==1 && weak==0 のとき get_mut は Some");
            return Ok(&mut tracked.data);
        }
        // 共有中または未 tracked。複製して新しい tracked backing を作る（§5.2 COW）。
        let cloned: Vec<Value> = (**list).clone();
        let bytes = heap_size::list_body(cloned.len() as u64);
        let fresh = Tracked::new(cloned, bytes, budget, phase)?;
        *list = fresh;
        let tracked = Rc::get_mut(list).expect("新規 Rc は一意所有");
        Ok(&mut tracked.data)
    }

    /// `Value::Dict` の backing を書き込み用に detach する（COW）。詳細は
    /// [`Value::detach_list`] と同じ。
    fn detach_dict<'a>(
        dict: &'a mut Rc<TrackedDict>,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<&'a mut BTreeMap<String, Value>, ControlStop> {
        if dict.id.0 != 0 && Rc::strong_count(dict) == 1 && Rc::weak_count(dict) == 0 {
            let tracked = Rc::get_mut(dict).expect("strong==1 && weak==0 のとき get_mut は Some");
            return Ok(&mut tracked.data);
        }
        let cloned: BTreeMap<String, Value> = (**dict).clone();
        let key_bytes_total: u64 = cloned
            .keys()
            .map(|k| k.len() as u64)
            .fold(0u64, u64::saturating_add);
        let bytes = heap_size::dict_body(cloned.len() as u64, key_bytes_total);
        let fresh = Tracked::new(cloned, bytes, budget, phase)?;
        *dict = fresh;
        let tracked = Rc::get_mut(dict).expect("新規 Rc は一意所有");
        Ok(&mut tracked.data)
    }

    /// 一意所有 backing の要素数が変わったとき、live heap を旧サイズから新サイズへ
    /// 調整する（in place 書き換え用の delta 課金）。
    ///
    /// 共有 backing を detach した直後は要素数が変わっていないのでこの調整は不要
    /// （`Tracked::new` が既に新サイズを課金済み）。in place 経路（push/pop/index-set）で
    /// 呼ぶ。増加分は `charge`、減少分は `release`。id や `Tracked.bytes` は据え置くと
    /// 二重計上になるため、`bytes` を新サイズへ更新する。
    fn retrack_list(
        list: &mut Rc<TrackedList>,
        old_bytes: u64,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        let new_bytes = heap_size::list_body(list.data.len() as u64);
        if new_bytes > old_bytes {
            budget.charge_heap(new_bytes - old_bytes, phase)?;
        } else if new_bytes < old_bytes {
            budget.release_heap(old_bytes - new_bytes);
        }
        if let Some(tracked) = Rc::get_mut(list) {
            tracked.bytes = new_bytes;
        }
        Ok(())
    }

    /// 一意所有 Dict backing の delta 課金（[`Value::retrack_list`] の Dict 版）。
    fn retrack_dict(
        dict: &mut Rc<TrackedDict>,
        old_bytes: u64,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        let key_bytes_total: u64 = dict
            .data
            .keys()
            .map(|k| k.len() as u64)
            .fold(0u64, u64::saturating_add);
        let new_bytes = heap_size::dict_body(dict.data.len() as u64, key_bytes_total);
        if new_bytes > old_bytes {
            budget.charge_heap(new_bytes - old_bytes, phase)?;
        } else if new_bytes < old_bytes {
            budget.release_heap(old_bytes - new_bytes);
        }
        if let Some(tracked) = Rc::get_mut(dict) {
            tracked.bytes = new_bytes;
        }
        Ok(())
    }

    /// `self`（`Value::List`）末尾へ 1 要素を push する（delta 課金、REV-015 案A）。
    ///
    /// COW detach（共有なら複製・課金、単独なら in place）→ push →要素数増加ぶんの
    /// delta（+32 byte）を課金する。共有からの複製時は `detach_list` が新サイズを課金済み
    /// なので、`retrack_list` は複製後サイズ基準の delta（=+32）だけを追加課金する。
    /// `self` が List でなければ呼び出し側の型エラー（ここでは何もしない）。
    pub fn list_push_tracked(
        &mut self,
        item: Value,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        if let Value::List(list) = self {
            Value::detach_list(list, budget, phase)?;
            let old_bytes = list.bytes;
            {
                let data = Rc::get_mut(list).expect("detach 後は一意所有").data_mut();
                data.push(item);
            }
            Value::retrack_list(list, old_bytes, budget, phase)?;
        }
        Ok(())
    }

    /// `self`（`Value::List`）末尾要素を除去する（delta 課金）。空なら何もしない。
    pub fn list_pop_tracked(
        &mut self,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        if let Value::List(list) = self {
            if list.data.is_empty() {
                return Ok(());
            }
            Value::detach_list(list, budget, phase)?;
            let old_bytes = list.bytes;
            {
                let data = Rc::get_mut(list).expect("detach 後は一意所有").data_mut();
                data.pop();
            }
            Value::retrack_list(list, old_bytes, budget, phase)?;
        }
        Ok(())
    }

    /// `self[index] = value`（List/Dict）を tracked backing 上で行う（delta 課金）。
    ///
    /// 呼び出し側は index 型・範囲・collection 上限を検査済みで、正規化した
    /// `list_index`（List）または `dict_key`（Dict）を渡す。List の index 代入は要素数
    /// 不変で delta 0、Dict の新規 key 追加は key 長ぶんを課金する。
    pub fn index_set_tracked(
        &mut self,
        target: IndexTarget,
        value: Value,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<(), ControlStop> {
        match (self, target) {
            (Value::List(list), IndexTarget::ListIndex(idx)) => {
                Value::detach_list(list, budget, phase)?;
                let old_bytes = list.bytes;
                {
                    let data = Rc::get_mut(list).expect("detach 後は一意所有").data_mut();
                    if idx < data.len() {
                        data[idx] = value;
                    }
                }
                Value::retrack_list(list, old_bytes, budget, phase)?;
            }
            (Value::Dict(dict), IndexTarget::DictKey(key)) => {
                Value::detach_dict(dict, budget, phase)?;
                let old_bytes = dict.bytes;
                {
                    let data = Rc::get_mut(dict).expect("detach 後は一意所有").data_mut();
                    data.insert(key, value);
                }
                Value::retrack_dict(dict, old_bytes, budget, phase)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// 真偽判定（if / while の条件で使う）
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Null => false,
            Value::Int(0) => false,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(v) => !v.is_empty(),
            Value::Dict(m) => !m.is_empty(),
            Value::Fn { .. } => true,
            Value::VmFn { .. } => true,
            Value::Error { .. } => true,
            _ => true,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{}", n),
            Value::Float(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{}", s.as_str()),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Null => write!(f, "null"),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(format_value_repr).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Dict(map) => {
                let parts: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("\"{}\": {}", k, format_value_repr(v)))
                    .collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
            Value::Fn { def, .. } => {
                write!(f, "<fn {}({})>", def.name, def.params.join(", "))
            }
            Value::VmFn { name, params, .. } => {
                write!(f, "<fn {}({})>", name, params.join(", "))
            }
            Value::Error { message, .. } => {
                write!(f, "{}", message)
            }
        }
    }
}

/// Display 用に値を repr 形式（文字列はクォート付き）で表示する
fn format_value_repr(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("\"{}\"", s.as_str()),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_values() {
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Int(1).is_truthy());
        assert!(Value::Int(-1).is_truthy());
        assert!(Value::Float(0.1).is_truthy());
        assert!(Value::str_from("hello").is_truthy());
        assert!(Value::List(Tracked::constant(vec![Value::Int(1)])).is_truthy());
        assert!(
            Value::Dict(Tracked::constant(BTreeMap::from([(
                "a".into(),
                Value::Int(1)
            )])))
            .is_truthy()
        );
    }

    #[test]
    fn falsy_values() {
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Null.is_truthy());
        assert!(!Value::Int(0).is_truthy());
        assert!(!Value::Float(0.0).is_truthy());
        assert!(!Value::str_from("").is_truthy());
        assert!(!Value::List(Tracked::constant(vec![])).is_truthy());
        assert!(!Value::Dict(Tracked::constant(BTreeMap::new())).is_truthy());
    }

    #[test]
    fn display() {
        assert_eq!(Value::Int(42).to_string(), "42");
        assert_eq!(Value::Float(2.5).to_string(), "2.5");
        assert_eq!(Value::str_from("hi").to_string(), "hi");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Null.to_string(), "null");
        assert_eq!(
            Value::List(Tracked::constant(vec![Value::Int(1), Value::str_from("a")])).to_string(),
            "[1, \"a\"]"
        );
        assert_eq!(
            Value::Dict(Tracked::constant(BTreeMap::from([(
                "x".into(),
                Value::Int(10)
            )])))
            .to_string(),
            "{\"x\": 10}"
        );
    }

    // ---- REV-003: NumericOrder 厳密比較 ----

    fn int(i: i64) -> Value {
        Value::Int(i)
    }
    fn flt(f: f64) -> Value {
        Value::Float(f)
    }

    #[test]
    fn numeric_order_int_int() {
        assert_eq!(
            NumericOrder::compare(&int(1), &int(2)),
            Some(NumericOrdering::Less)
        );
        assert_eq!(
            NumericOrder::compare(&int(2), &int(2)),
            Some(NumericOrdering::Equal)
        );
        assert_eq!(
            NumericOrder::compare(&int(3), &int(2)),
            Some(NumericOrdering::Greater)
        );
    }

    #[test]
    fn numeric_order_float_float_and_nan() {
        assert_eq!(
            NumericOrder::compare(&flt(1.0), &flt(2.0)),
            Some(NumericOrdering::Less)
        );
        assert_eq!(
            NumericOrder::compare(&flt(2.0), &flt(2.0)),
            Some(NumericOrdering::Equal)
        );
        // -0.0 == 0.0
        assert_eq!(
            NumericOrder::compare(&flt(-0.0), &flt(0.0)),
            Some(NumericOrdering::Equal)
        );
        assert_eq!(
            NumericOrder::compare(&flt(f64::NAN), &flt(1.0)),
            Some(NumericOrdering::UnorderedNaN)
        );
    }

    #[test]
    fn numeric_order_non_numeric_is_none() {
        assert_eq!(NumericOrder::compare(&int(1), &Value::str_from("a")), None);
        assert_eq!(NumericOrder::compare(&Value::Bool(true), &int(1)), None);
    }

    #[test]
    fn numeric_order_int_float_fractional() {
        // 1 < 1.5 < 2
        assert_eq!(
            NumericOrder::compare(&int(1), &flt(1.5)),
            Some(NumericOrdering::Less)
        );
        assert_eq!(
            NumericOrder::compare(&int(2), &flt(1.5)),
            Some(NumericOrdering::Greater)
        );
        // 負の小数部
        assert_eq!(
            NumericOrder::compare(&int(-1), &flt(-1.5)),
            Some(NumericOrdering::Greater)
        );
        assert_eq!(
            NumericOrder::compare(&int(-2), &flt(-1.5)),
            Some(NumericOrdering::Less)
        );
        // 整数値どうし（小数部 0）
        assert_eq!(
            NumericOrder::compare(&int(3), &flt(3.0)),
            Some(NumericOrdering::Equal)
        );
    }

    #[test]
    fn numeric_order_int_float_infinity() {
        assert_eq!(
            NumericOrder::compare(&int(i64::MAX), &flt(f64::INFINITY)),
            Some(NumericOrdering::Less)
        );
        assert_eq!(
            NumericOrder::compare(&int(i64::MIN), &flt(f64::NEG_INFINITY)),
            Some(NumericOrdering::Greater)
        );
        assert_eq!(
            NumericOrder::compare(&flt(f64::INFINITY), &int(0)),
            Some(NumericOrdering::Greater)
        );
    }

    #[test]
    fn numeric_order_int_float_nan() {
        assert_eq!(
            NumericOrder::compare(&int(1), &flt(f64::NAN)),
            Some(NumericOrdering::UnorderedNaN)
        );
        assert_eq!(
            NumericOrder::compare(&flt(f64::NAN), &int(1)),
            Some(NumericOrdering::UnorderedNaN)
        );
        // NaN は等価でも大小でもない
        assert!(!NumericOrder::numeric_eq(&int(1), &flt(f64::NAN)));
    }

    /// 2^53 近傍で丸めずに厳密比較できる（REV-003 の核心）。
    #[test]
    fn numeric_order_exact_beyond_2_pow_53() {
        let two53 = 1i64 << 53; // 9_007_199_254_740_992
        // f64 で 2^53 は正確に表現できる。2^53 と 2^53+1 は f64 では同じ値になるが、
        // Int 側は区別できるため、厳密比較では 2^53+1 > (2^53 as f64) になる。
        let f_two53 = two53 as f64;
        assert_eq!(
            NumericOrder::compare(&int(two53), &flt(f_two53)),
            Some(NumericOrdering::Equal)
        );
        assert_eq!(
            NumericOrder::compare(&int(two53 + 1), &flt(f_two53)),
            Some(NumericOrdering::Greater)
        );
        assert_eq!(
            NumericOrder::compare(&int(two53 - 1), &flt(f_two53)),
            Some(NumericOrdering::Less)
        );
    }

    /// 従来 `i64 as f64` では桁落ちで等価になっていた非推移性が解消する。
    /// a = 2^53, f = 2^53 as f64, b = 2^53 + 1。旧実装では a==f かつ f==b だが a!=b。
    #[test]
    fn equality_is_transitive_near_2_pow_53() {
        let a = int(1i64 << 53);
        let b = int((1i64 << 53) + 1);
        let f = flt((1i64 << 53) as f64);
        // 厳密比較: a == f は true、f == b は false（旧実装では両方 true だった）
        assert!(a == f);
        assert!(!(f == b));
        // 推移性: a == f かつ f == b なら a == b でなければならない。
        // f == b が false なので前件が成り立たず、非推移性は起きない。
        assert!(!(a == b));
    }

    #[test]
    fn equality_is_symmetric() {
        let cases = [
            (int(5), flt(5.0)),
            (int(1i64 << 53), flt((1i64 << 53) as f64)),
            (int(0), flt(-0.0)),
        ];
        for (a, b) in cases {
            assert_eq!(a == b, b == a, "対称性: {:?} vs {:?}", a, b);
        }
    }

    /// 巨大 Float（i128 範囲外）は Int より大きい／小さいと確定する。
    #[test]
    fn numeric_order_huge_float() {
        assert_eq!(
            NumericOrder::compare(&int(i64::MAX), &flt(1e300)),
            Some(NumericOrdering::Less)
        );
        assert_eq!(
            NumericOrder::compare(&int(i64::MIN), &flt(-1e300)),
            Some(NumericOrdering::Greater)
        );
    }

    #[test]
    fn ordering_helpers() {
        assert!(NumericOrdering::Less.is_lt());
        assert!(NumericOrdering::Less.is_le());
        assert!(!NumericOrdering::Less.is_gt());
        assert!(NumericOrdering::Equal.is_le());
        assert!(NumericOrdering::Equal.is_ge());
        assert!(!NumericOrdering::Equal.is_lt());
        assert!(NumericOrdering::Greater.is_gt());
        assert!(NumericOrdering::Greater.is_ge());
        // NaN は全比較で false
        assert!(!NumericOrdering::UnorderedNaN.is_lt());
        assert!(!NumericOrdering::UnorderedNaN.is_le());
        assert!(!NumericOrdering::UnorderedNaN.is_gt());
        assert!(!NumericOrdering::UnorderedNaN.is_ge());
    }

    // ---- REV-015 案A: tracked collection の per-drop release / delta 課金 ----

    use crate::budget::{BudgetConfig, BudgetLedger, ExecutionPhase};

    fn heap_ledger(limit: u64) -> BudgetLedger {
        let mut config = BudgetConfig::for_legacy(1_000_000, 1_000_000);
        config.max_live_heap_bytes = limit;
        BudgetLedger::with_config(config)
    }

    #[test]
    fn tracked_list_charges_on_new_and_releases_on_drop() {
        let mut budget = heap_ledger(1_000_000);
        assert_eq!(budget.live_heap_bytes(), 0);
        // 3 要素 List body = 24 + 32*3 = 120。
        let list = Value::new_list(
            vec![Value::Int(1), Value::Int(2), Value::Int(3)],
            &mut budget,
            ExecutionPhase::Run,
        )
        .unwrap();
        assert_eq!(budget.live_heap_bytes(), heap_size::list_body(3));
        // 最後の参照 drop で live heap が戻る（§5.2）。
        drop(list);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn tracked_dict_charges_body_with_key_bytes_and_releases_on_drop() {
        let mut budget = heap_ledger(1_000_000);
        let mut map = BTreeMap::new();
        map.insert("ab".to_string(), Value::Int(1));
        map.insert("cde".to_string(), Value::Int(2));
        let dict = Value::new_dict(map, &mut budget, ExecutionPhase::Run).unwrap();
        // 2 entry, key bytes 2+3=5 → 24 + 64*2 + 5 = 157。
        assert_eq!(budget.live_heap_bytes(), heap_size::dict_body(2, 5));
        drop(dict);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn rc_clone_shares_without_extra_charge_and_releases_once() {
        let mut budget = heap_ledger(1_000_000);
        let list = Value::new_list(vec![Value::Int(1)], &mut budget, ExecutionPhase::Run).unwrap();
        let one = budget.live_heap_bytes();
        assert_eq!(one, heap_size::list_body(1));
        // clone は Rc ハンドル共有。追加課金しない（§5.2）。
        let alias = list.clone();
        assert_eq!(budget.live_heap_bytes(), one);
        // 1 本目を drop してもまだ alias が生きているので release されない。
        drop(list);
        assert_eq!(budget.live_heap_bytes(), one);
        // 最後の参照が落ちて初めて release。
        drop(alias);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn untracked_constant_does_not_charge_or_release() {
        let budget = heap_ledger(1_000_000);
        // Tracked::constant は課金しない（compile 時定数用）。台帳の live は 0 のまま。
        let c = Value::List(Tracked::constant(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(budget.live_heap_bytes(), 0);
        drop(c);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn push_charges_delta_only() {
        let mut budget = heap_ledger(1_000_000);
        let mut list = Value::new_list(vec![], &mut budget, ExecutionPhase::Run).unwrap();
        let empty = budget.live_heap_bytes();
        assert_eq!(empty, heap_size::list_body(0)); // 24
        // push 1 要素 → +32（1 要素ぶんの body delta）。全体再課金ではない。
        list.list_push_tracked(Value::Int(1), &mut budget, ExecutionPhase::Run)
            .unwrap();
        assert_eq!(budget.live_heap_bytes(), heap_size::list_body(1));
        list.list_push_tracked(Value::Int(2), &mut budget, ExecutionPhase::Run)
            .unwrap();
        assert_eq!(budget.live_heap_bytes(), heap_size::list_body(2));
        // pop で delta release。
        list.list_pop_tracked(&mut budget, ExecutionPhase::Run)
            .unwrap();
        assert_eq!(budget.live_heap_bytes(), heap_size::list_body(1));
        drop(list);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn cow_detach_on_shared_push_charges_new_backing_and_releases_old_on_drop() {
        let mut budget = heap_ledger(1_000_000);
        let base = Value::new_list(
            vec![Value::Int(1), Value::Int(2)],
            &mut budget,
            ExecutionPhase::Run,
        )
        .unwrap();
        let base_bytes = budget.live_heap_bytes();
        assert_eq!(base_bytes, heap_size::list_body(2));
        // 共有してから push すると COW で新 backing を作る（§5.2: 新 AllocationId）。
        let mut shared = base.clone();
        // clone は無課金。
        assert_eq!(budget.live_heap_bytes(), base_bytes);
        shared
            .list_push_tracked(Value::Int(3), &mut budget, ExecutionPhase::Run)
            .unwrap();
        // 新 backing（3 要素）を課金。旧 backing（2 要素）は base がまだ握るので live。
        assert_eq!(
            budget.live_heap_bytes(),
            heap_size::list_body(2) + heap_size::list_body(3)
        );
        // base を drop すると旧 backing が release。
        drop(base);
        assert_eq!(budget.live_heap_bytes(), heap_size::list_body(3));
        drop(shared);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn index_set_same_size_is_zero_delta() {
        let mut budget = heap_ledger(1_000_000);
        let mut list = Value::new_list(
            vec![Value::Int(1), Value::Int(2)],
            &mut budget,
            ExecutionPhase::Run,
        )
        .unwrap();
        let before = budget.live_heap_bytes();
        // 要素の入れ替えは要素数不変 → live heap delta 0。
        list.index_set_tracked(
            IndexTarget::ListIndex(0),
            Value::Int(99),
            &mut budget,
            ExecutionPhase::Run,
        )
        .unwrap();
        assert_eq!(budget.live_heap_bytes(), before);
        drop(list);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn nested_list_releases_inner_on_outer_drop() {
        let mut budget = heap_ledger(1_000_000);
        let inner = Value::new_list(vec![Value::Int(1)], &mut budget, ExecutionPhase::Run).unwrap();
        let inner_bytes = budget.live_heap_bytes();
        // 外側 List に内側 List を格納。内側は共有（clone）されるので追加課金なし。
        let outer = Value::new_list(vec![inner.clone()], &mut budget, ExecutionPhase::Run).unwrap();
        assert_eq!(
            budget.live_heap_bytes(),
            inner_bytes + heap_size::list_body(1)
        );
        // inner の変数参照を落としても、outer が内側を握るので release されない。
        drop(inner);
        assert_eq!(
            budget.live_heap_bytes(),
            inner_bytes + heap_size::list_body(1)
        );
        // outer を drop すると外側 backing と、最後の参照になった内側 backing が両方 release。
        drop(outer);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn tracked_str_charges_on_new_and_releases_on_drop() {
        let mut budget = heap_ledger(1_000_000);
        assert_eq!(budget.live_heap_bytes(), 0);
        // String body = 24 + byte 長。"hello" は 5 byte。
        let s = Value::new_str("hello".to_string(), &mut budget, ExecutionPhase::Run).unwrap();
        assert_eq!(budget.live_heap_bytes(), heap_size::string_body(5));
        // 最後の参照 drop で live heap が戻る（§5.2）。
        drop(s);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn str_rc_clone_shares_without_extra_charge_and_releases_once() {
        let mut budget = heap_ledger(1_000_000);
        let s = Value::new_str("shared".to_string(), &mut budget, ExecutionPhase::Run).unwrap();
        let one = budget.live_heap_bytes();
        assert_eq!(one, heap_size::string_body(6));
        // clone は Rc ハンドル共有。追加課金しない（§5.2）。
        let alias = s.clone();
        assert_eq!(budget.live_heap_bytes(), one);
        // 1 本目を drop してもまだ alias が生きているので release されない。
        drop(s);
        assert_eq!(budget.live_heap_bytes(), one);
        // 最後の参照が落ちて初めて release。
        drop(alias);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn str_constant_does_not_charge_or_release() {
        let budget = heap_ledger(1_000_000);
        // str_constant は課金しない（untracked、AllocationId(0)）。台帳の live は 0 のまま。
        let c = Value::str_from("literal");
        assert_eq!(budget.live_heap_bytes(), 0);
        drop(c);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn string_in_list_releases_on_outer_drop() {
        let mut budget = heap_ledger(1_000_000);
        // tracked String を tracked List に格納。String は共有（clone）されるので追加課金なし。
        let s = Value::new_str("body".to_string(), &mut budget, ExecutionPhase::Run).unwrap();
        let str_bytes = budget.live_heap_bytes();
        assert_eq!(str_bytes, heap_size::string_body(4));
        let outer = Value::new_list(vec![s.clone()], &mut budget, ExecutionPhase::Run).unwrap();
        assert_eq!(
            budget.live_heap_bytes(),
            str_bytes + heap_size::list_body(1)
        );
        // s の変数参照を落としても、outer が String backing を握るので release されない。
        drop(s);
        assert_eq!(
            budget.live_heap_bytes(),
            str_bytes + heap_size::list_body(1)
        );
        // outer を drop すると List backing と、最後の参照になった String backing が両方 release。
        drop(outer);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn new_cell_charges_captured_cell_and_releases_on_drop() {
        let budget = heap_ledger(1_000_000);
        assert_eq!(budget.live_heap_bytes(), 0);
        // captured cell = 32 byte 固定。
        let cell =
            Value::new_cell(Value::Int(1), &budget.heap_handle(), ExecutionPhase::Run).unwrap();
        assert_eq!(budget.live_heap_bytes(), heap_size::CAPTURED_CELL);
        // clone（closure capture・共有）は Rc ハンドル共有で無課金。
        let alias = Rc::clone(&cell);
        assert_eq!(budget.live_heap_bytes(), heap_size::CAPTURED_CELL);
        drop(cell);
        assert_eq!(budget.live_heap_bytes(), heap_size::CAPTURED_CELL);
        // 最後の参照 drop で release。
        drop(alias);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn cell_untracked_does_not_charge() {
        let budget = heap_ledger(1_000_000);
        let c = Value::cell_untracked(Value::Int(1));
        assert_eq!(budget.live_heap_bytes(), 0);
        drop(c);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn tree_fn_header_charges_and_releases_on_drop() {
        let budget = heap_ledger(1_000_000);
        // captured 2 個 → tree_function(2) = 64 + 16*2 = 96。cell 実体は別課金なので含めない。
        let header =
            Value::new_tree_fn_header(2, &budget.heap_handle(), ExecutionPhase::Run).unwrap();
        assert_eq!(budget.live_heap_bytes(), heap_size::tree_function(2));
        // 関数値の clone（共有）は header の Rc ハンドル共有で無課金。
        let alias = Rc::clone(&header);
        assert_eq!(budget.live_heap_bytes(), heap_size::tree_function(2));
        drop(header);
        assert_eq!(budget.live_heap_bytes(), heap_size::tree_function(2));
        drop(alias);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn vm_fn_header_charges_and_releases_on_drop() {
        let budget = heap_ledger(1_000_000);
        // upvalue 3 個 → vm_function(3) = 48 + 16*3 = 96。
        let header =
            Value::new_vm_fn_header(3, &budget.heap_handle(), ExecutionPhase::Run).unwrap();
        assert_eq!(budget.live_heap_bytes(), heap_size::vm_function(3));
        drop(header);
        assert_eq!(budget.live_heap_bytes(), 0);
    }

    #[test]
    fn cell_charge_trips_at_limit() {
        // captured cell 1 個ちょうどの上限。2 個目は超過する。
        let budget = heap_ledger(heap_size::CAPTURED_CELL);
        let ok = Value::new_cell(Value::Int(1), &budget.heap_handle(), ExecutionPhase::Run);
        assert!(ok.is_ok());
        let over = Value::new_cell(Value::Int(2), &budget.heap_handle(), ExecutionPhase::Run);
        assert!(matches!(
            over,
            Err(crate::budget::ControlStop::BudgetExceeded(_))
        ));
    }

    #[test]
    fn string_heap_limit_trips_at_allocation() {
        // 上限を "hi"（body 24+2=26）ちょうどに設定。次の allocation は超過する。
        let mut budget = heap_ledger(heap_size::string_body(2));
        let ok = Value::new_str("hi".to_string(), &mut budget, ExecutionPhase::Run);
        assert!(ok.is_ok());
        // ok が live なので次の allocation は必ず超過。
        let over = Value::new_str("x".to_string(), &mut budget, ExecutionPhase::Run);
        assert!(matches!(
            over,
            Err(crate::budget::ControlStop::BudgetExceeded(_))
        ));
    }

    #[test]
    fn heap_limit_trips_at_allocation() {
        // 上限を 2 要素 List body ちょうどに設定。3 要素は超過する。
        let mut budget = heap_ledger(heap_size::list_body(2));
        let ok = Value::new_list(
            vec![Value::Int(1), Value::Int(2)],
            &mut budget,
            ExecutionPhase::Run,
        );
        assert!(ok.is_ok());
        // ok が live なので次の allocation は必ず超過。
        let over = Value::new_list(vec![Value::Int(1)], &mut budget, ExecutionPhase::Run);
        assert!(matches!(
            over,
            Err(crate::budget::ControlStop::BudgetExceeded(_))
        ));
    }
}
