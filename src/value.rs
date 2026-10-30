use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;

use crate::budget::{AllocationId, ControlStop, ExecutionPhase, HeapLedgerWeak, heap_size};
use crate::chunk::Chunk;
use crate::limits::MAX_VALUE_DEPTH;

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
    /// 子要素の深度の最大（List の要素・Dict の値。空・スカラーだけなら 0）。
    /// List / Dict backing では `1 + child_max` がこの値の深度（semantic-decisions §21.3）。
    /// 値を再帰でたどらず、生成・変更のたびに子の保存済み深度から更新する。
    child_max: u32,
    /// 子要素のうち深度が `child_max` に等しいものの数。index 代入・`pop` で最大の
    /// 子を取り除いても、この数が 0 になるときだけ全要素を読み直す（深度を正確に保つ）。
    child_max_count: u32,
}

/// backing の中身が持つ子要素の深度の要約を求める（[`Tracked`] の生成時に使う）。
///
/// 返す組は（子の深度の最大, その深度を持つ子の数）。List / Dict の backing だけが
/// 実装する。crate 内部の trait で、公開の生成関数（`Tracked::new` など）の境界には
/// 使わない（公開 API を増やさない、PR #301 判断 Q1=B）。
pub(crate) trait ChildDepth {
    /// 子要素の保存済み深度を 1 段だけ読んで要約する（再帰しない）。
    fn child_depth_summary(&self) -> (u32, u32);
}

fn summarize_depths<'a>(values: impl Iterator<Item = &'a Value>) -> (u32, u32) {
    let mut max = 0u32;
    let mut count = 0u32;
    for v in values {
        let d = v.depth();
        if d > max {
            max = d;
            count = 1;
        } else if d == max {
            count = count.saturating_add(1);
        }
    }
    // 空・全要素が深度 0 のときは count = 要素数（max = 0 の子の数）。空は (0, 0)。
    (max, count)
}

impl ChildDepth for Vec<Value> {
    fn child_depth_summary(&self) -> (u32, u32) {
        summarize_depths(self.iter())
    }
}

impl ChildDepth for BTreeMap<String, Value> {
    fn child_depth_summary(&self) -> (u32, u32) {
        summarize_depths(self.values())
    }
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
            child_max: 0,
            child_max_count: 0,
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
            child_max: 0,
            child_max_count: 0,
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
                child_max: 0,
                child_max_count: 0,
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
            child_max: 0,
            child_max_count: 0,
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
    /// 生成直後の backing に、子要素の深度の要約を書き込む（List / Dict 専用）。
    ///
    /// 公開の生成関数（`new` / `constant`）は要約を 0 で初期化するので、crate 内で
    /// List / Dict の backing を作る経路（`Value::new_list` / `new_dict`・COW の detach・
    /// builtin の untracked な結果）はここを通す。生成直後の `Rc` は一意所有なので必ず書ける。
    pub(crate) fn summarized(mut rc: Rc<Self>) -> Rc<Self>
    where
        T: ChildDepth,
    {
        if let Some(tracked) = Rc::get_mut(&mut rc) {
            let (m, c) = tracked.data.child_depth_summary();
            tracked.child_max = m;
            tracked.child_max_count = c;
        }
        rc
    }

    /// 深度の要約つきの untracked backing（[`Tracked::constant`] + [`Tracked::summarized`]）。
    pub(crate) fn constant_with_depth(data: T) -> Rc<Self>
    where
        T: ChildDepth,
    {
        Self::summarized(Self::constant(data))
    }

    /// この backing（List / Dict）の深度（semantic-decisions §21.3。空は 1）。
    pub(crate) fn depth(&self) -> u32 {
        self.child_max.saturating_add(1)
    }

    /// 子要素 1 つを取り除いたことを深度の要約へ反映する（深度 `d` の子）。
    fn note_child_removed(&mut self, d: u32) {
        if d == self.child_max && self.child_max_count > 0 {
            self.child_max_count -= 1;
        }
    }

    /// 子要素 1 つを入れたことを深度の要約へ反映する（深度 `d` の子）。
    fn note_child_added(&mut self, d: u32) {
        if d > self.child_max {
            self.child_max = d;
            self.child_max_count = 1;
        } else if d == self.child_max {
            self.child_max_count = self.child_max_count.saturating_add(1);
        }
    }

    /// 最大深度の子がいなくなっていたら、全要素の保存済み深度を読み直す（再帰しない）。
    fn settle_child_depth(&mut self)
    where
        T: ChildDepth,
    {
        if self.child_max_count == 0 {
            let (m, c) = self.data.child_depth_summary();
            self.child_max = m;
            self.child_max_count = c;
        }
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

/// 全ての `Value` を覆う決定的な全順序（REV-017、semantic-decisions §17.8）
///
/// sort（builtin）と `list_dir`（名前の整列）が revision 0.21 で参照する単一実装。
/// tree・VM・builtin が同じこの実装を通ることで backend 間の順序差を作らない
/// （REV-003 の `NumericOrder` と同じ集約方針）。常に `Less`/`Equal`/`Greater` を返す
/// 全順序で、`NumericOrder` の `UnorderedNaN` のような未決着を外へ漏らさない。
///
/// # 型帯（type band）順（§17.8.3）
///
/// ```text
/// Null < Bool < 数値(Int/Float 統合) < Str < List < Dict < Fn(Fn/VmFn) < Error
/// ```
///
/// 帯割り当ては `crate::error::type_name`（canonical 型名）と整合させる。特に
/// `Fn` と `VmFn` は同一帯「Fn」とし（`type_name` が両者を `"Fn"` へ畳むのと一致）、
/// backend 差（tree の `Fn` / VM の `VmFn`）を順序へ漏らさない。
pub struct TotalOrder;

impl TotalOrder {
    /// 2 つの値を全順序で比較する。常に `Less`/`Equal`/`Greater` を返す。
    pub fn compare(a: &Value, b: &Value) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        let (ra, rb) = (Self::band_rank(a), Self::band_rank(b));
        if ra != rb {
            // 帯が異なれば帯 rank で決着する。
            return ra.cmp(&rb);
        }

        // 同一帯内の順序（§17.8.3）。
        match (a, b) {
            // Null: 単一値。常に Equal。
            (Value::Null, Value::Null) => Ordering::Equal,
            // Bool: false < true。
            (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
            // Str: Unicode コードポイント順（UTF-8 バイト順＝コードポイント順）。
            (Value::Str(x), Value::Str(y)) => x.as_str().cmp(y.as_str()),
            // List: 要素ごとに同じ TotalOrder で再帰する辞書式順序。
            // prefix は短い方が小（Iterator::cmp 相当）。
            (Value::List(x), Value::List(y)) => Self::compare_seq(x.iter(), y.iter()),
            // Dict: BTreeMap の (key, value) 列を辞書式比較する。
            // key を Str 規則で、等しければ value を再帰比較。prefix は entry 数が少ない
            // 方が小（空 Dict が最小）。
            (Value::Dict(x), Value::Dict(y)) => Self::compare_dict(x, y),
            // Fn / VmFn: 内容順序を持たないため帯内は全て Equal（FunctionId を key にしない）。
            // 安定ソートで元の相対順序が保たれる。tree の Fn と VM の VmFn も相互 Equal。
            (Value::Fn { .. }, Value::Fn { .. })
            | (Value::Fn { .. }, Value::VmFn { .. })
            | (Value::VmFn { .. }, Value::Fn { .. })
            | (Value::VmFn { .. }, Value::VmFn { .. }) => Ordering::Equal,
            // Error: 内容順序を持たせず帯内は全て Equal
            // （error_type/message/line を key にしない）。
            (Value::Error { .. }, Value::Error { .. }) => Ordering::Equal,
            // 数値帯（Int/Float 統合）: NumericOrder を経由し、NaN を数値帯の末尾へ固定。
            _ => Self::compare_numeric(a, b),
        }
    }

    /// budget 課金付きの全順序比較（REV-017 §17.8.5）。
    ///
    /// [`Self::compare`] と同じ順序を返すが、List / Dict の再帰的要素走査ごとに `1 / element`
    /// の fuel を課金する。これにより共有 DAG（1 つの List 値を多数の位置から参照して論理
    /// サイズが指数化した値）の比較も、走査課金が有限に積み上がり budget 超過で停止する
    /// （manifesto 原則: ホストの安定性）。sort の 0.21 経路がソート比較ごとにこれを呼ぶ。
    /// charge table の per-element 課金（値は不変）を比較走査へ適用するもので、新しい課金概念は
    /// 導入しない。
    pub fn compare_budgeted(
        a: &Value,
        b: &Value,
        budget: &mut crate::budget::BudgetLedger,
        phase: ExecutionPhase,
    ) -> Result<std::cmp::Ordering, ControlStop> {
        use std::cmp::Ordering;

        let (ra, rb) = (Self::band_rank(a), Self::band_rank(b));
        if ra != rb {
            return Ok(ra.cmp(&rb));
        }

        match (a, b) {
            (Value::List(x), Value::List(y)) => {
                let mut ix = x.iter();
                let mut iy = y.iter();
                loop {
                    match (ix.next(), iy.next()) {
                        (Some(ex), Some(ey)) => {
                            // 要素 1 つの走査ごとに課金する（§17.8.5 の 1 / element を比較走査へ）。
                            budget.charge_fuel(1, phase)?;
                            match Self::compare_budgeted(ex, ey, budget, phase)? {
                                Ordering::Equal => continue,
                                non_eq => return Ok(non_eq),
                            }
                        }
                        (Some(_), None) => return Ok(Ordering::Greater),
                        (None, Some(_)) => return Ok(Ordering::Less),
                        (None, None) => return Ok(Ordering::Equal),
                    }
                }
            }
            (Value::Dict(x), Value::Dict(y)) => {
                let mut ix = x.iter();
                let mut iy = y.iter();
                loop {
                    match (ix.next(), iy.next()) {
                        (Some((kx, vx)), Some((ky, vy))) => {
                            budget.charge_fuel(1, phase)?;
                            match kx.as_str().cmp(ky.as_str()) {
                                Ordering::Equal => {}
                                non_eq => return Ok(non_eq),
                            }
                            match Self::compare_budgeted(vx, vy, budget, phase)? {
                                Ordering::Equal => continue,
                                non_eq => return Ok(non_eq),
                            }
                        }
                        (Some(_), None) => return Ok(Ordering::Greater),
                        (None, Some(_)) => return Ok(Ordering::Less),
                        (None, None) => return Ok(Ordering::Equal),
                    }
                }
            }
            // 非 collection 帯（スカラ・Str・Fn・Error）は再帰しないので課金なしで決着する。
            _ => Ok(Self::compare(a, b)),
        }
    }

    /// 型帯 rank（§17.8.3）。`error.rs::type_name` の畳み（Fn/VmFn→"Fn"）と一致させる。
    fn band_rank(v: &Value) -> u8 {
        match v {
            Value::Null => 0,
            Value::Bool(_) => 1,
            // Int と Float を 1 本の数値帯へ統合する。
            Value::Int(_) | Value::Float(_) => 2,
            Value::Str(_) => 3,
            Value::List(_) => 4,
            Value::Dict(_) => 5,
            // Fn/VmFn は同一帯（type_name が両者を "Fn" へ畳むのと一致）。
            Value::Fn { .. } | Value::VmFn { .. } => 6,
            Value::Error { .. } => 7,
        }
    }

    /// 数値帯の順序（§17.8.3）。`NumericOrder::compare` の `Less`/`Equal`/`Greater` を
    /// 素通しし、`UnorderedNaN`（NaN が絡む比較）のときだけ「NaN は非 NaN より大、NaN
    /// 同士は Equal」で `Ordering` へ写す。これで `-Inf < 有限値 < +Inf < NaN` を実現する。
    fn compare_numeric(a: &Value, b: &Value) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match NumericOrder::compare(a, b) {
            Some(NumericOrdering::Less) => Ordering::Less,
            Some(NumericOrdering::Equal) => Ordering::Equal,
            Some(NumericOrdering::Greater) => Ordering::Greater,
            Some(NumericOrdering::UnorderedNaN) => {
                let a_nan = Self::is_nan_value(a);
                let b_nan = Self::is_nan_value(b);
                match (a_nan, b_nan) {
                    (true, true) => Ordering::Equal, // NaN == NaN（canonical quiet NaN 1 種）
                    (true, false) => Ordering::Greater, // NaN は末尾
                    (false, true) => Ordering::Less,
                    // 数値帯で両者が非 NaN なら NumericOrder が決着するはずで、ここへは来ない。
                    (false, false) => Ordering::Equal,
                }
            }
            // band_rank で数値帯に入ったものだけが来るため None は発生しない。
            None => Ordering::Equal,
        }
    }

    fn is_nan_value(v: &Value) -> bool {
        matches!(v, Value::Float(f) if f.is_nan())
    }

    /// 要素列を同じ TotalOrder で再帰する辞書式比較（prefix は短い方が小）。
    fn compare_seq<'a>(
        a: impl Iterator<Item = &'a Value>,
        b: impl Iterator<Item = &'a Value>,
    ) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let mut a = a;
        let mut b = b;
        loop {
            match (a.next(), b.next()) {
                (Some(x), Some(y)) => match Self::compare(x, y) {
                    Ordering::Equal => continue,
                    non_eq => return non_eq,
                },
                (Some(_), None) => return Ordering::Greater, // b が prefix（短い）
                (None, Some(_)) => return Ordering::Less,    // a が prefix（短い）
                (None, None) => return Ordering::Equal,
            }
        }
    }

    /// Dict の辞書式比較。(key, value) 列として key は Str 規則、value は再帰、
    /// prefix は entry 数が少ない方が小（空 Dict が最小）。
    fn compare_dict(
        a: &BTreeMap<String, Value>,
        b: &BTreeMap<String, Value>,
    ) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let mut ia = a.iter();
        let mut ib = b.iter();
        loop {
            match (ia.next(), ib.next()) {
                (Some((ka, va)), Some((kb, vb))) => {
                    match ka.as_str().cmp(kb.as_str()) {
                        Ordering::Equal => {}
                        non_eq => return non_eq,
                    }
                    match Self::compare(va, vb) {
                        Ordering::Equal => continue,
                        non_eq => return non_eq,
                    }
                }
                (Some(_), None) => return Ordering::Greater,
                (None, Some(_)) => return Ordering::Less,
                (None, None) => return Ordering::Equal,
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
        /// この closure が retain する code object subtree（自分の `FnDef.body` と、そこから
        /// transitive に辿れる内側 `FnDef` / `Lambda` の body）の §5.1 論理サイズ（`ast_node`
        /// 単位）を課金する共有トークン集合（A-1、REV-015 Slice 6）。各トークンは Link 時に
        /// code object 単位で 1 回だけ課金され、生成時に side-table 経由で `Rc::clone` 共有
        /// される（追加課金なし）。この closure instance が生きているあいだ強参照を保ち、
        /// その code object を retain する最後の `Rc` が drop した時点で release する（§5.2）。
        /// header（instance 固有）とは別トークンとして並置する。`clone`（関数値の共有）は
        /// `Rc` ハンドル共有で無課金。
        retain_tokens: Vec<Rc<HeapToken>>,
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
        /// この closure が retain する code object subtree（自分の prototype `chunk` と、
        /// そこから transitive に辿れる子孫 prototype chunk）の §5.1 論理サイズ
        /// （`bytecode_chunk` 単位）を課金する共有トークン集合（A-1、REV-015 Slice 6）。
        /// 各トークンは adopt 時に chunk 単位で 1 回だけ課金され、`MakeClosure` 時に
        /// side-table 経由で `Rc::clone` 共有される（追加課金なし）。この closure instance が
        /// 生きているあいだ強参照を保ち、その chunk を retain する最後の `Rc` が drop した
        /// 時点で release する（§5.2）。header（instance 固有）とは別トークンとして並置する。
        retain_tokens: Vec<Rc<HeapToken>>,
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
            Value::Float(n) => write!(f, "Float({})", crate::determinism::format_float(*n)),
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
    /// この値の深度（semantic-decisions §21.3）。スカラー・関数値・エラー値は 0、
    /// List / Dict は backing が持つ保存済みの深度（空は 1）。値を再帰でたどらない。
    pub(crate) fn depth(&self) -> u32 {
        match self {
            Value::List(l) => l.depth(),
            Value::Dict(d) => d.depth(),
            _ => 0,
        }
    }

    /// この値を List / Dict の要素にすると深度の上限（`MAX_VALUE_DEPTH`）を超えるなら、
    /// catch できる `overflow` を返す（semantic-decisions §21.3）。
    ///
    /// 検査は tree / VM の呼び出し側で、値の生成・変更の primitive（`new_list` /
    /// `new_dict` / `list_push_tracked` / `index_set_tracked`）を呼ぶ前に行う。primitive の
    /// 失敗は catch 不能 terminal の `ControlStop` だけなので、深度超過はそこへ混ぜない
    /// （execution-control §7、PR #301 判断 Q1=B）。呼ぶ前に検査するので、拒否したとき
    /// COW の detach・課金は起きず、対象は変わらない。
    pub(crate) fn check_element_depth(
        &self,
        line: usize,
    ) -> Result<(), crate::error::TsumugiError> {
        if self.depth() as usize >= MAX_VALUE_DEPTH {
            return Err(crate::error::TsumugiError::value_depth_limit(line));
        }
        Ok(())
    }

    /// 複数の要素について [`Value::check_element_depth`] を行う。
    pub(crate) fn check_elements_depth<'a>(
        mut items: impl Iterator<Item = &'a Value>,
        line: usize,
    ) -> Result<(), crate::error::TsumugiError> {
        items.try_for_each(|v| v.check_element_depth(line))
    }

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
        Ok(Value::List(Tracked::summarized(Tracked::new(
            items, bytes, budget, phase,
        )?)))
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
        Ok(Value::Dict(Tracked::summarized(Tracked::new(
            map, bytes, budget, phase,
        )?)))
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
        let fresh = Tracked::summarized(Tracked::new(cloned, bytes, budget, phase)?);
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
        let fresh = Tracked::summarized(Tracked::new(cloned, bytes, budget, phase)?);
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
            // 深度の上限の検査は呼び出し側（`Value::check_element_depth`）。ここは要約の更新だけ。
            let item_depth = item.depth();
            Value::detach_list(list, budget, phase)?;
            let old_bytes = list.bytes;
            {
                let tracked = Rc::get_mut(list).expect("detach 後は一意所有");
                tracked.data_mut().push(item);
                tracked.note_child_added(item_depth);
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
                let tracked = Rc::get_mut(list).expect("detach 後は一意所有");
                if let Some(removed) = tracked.data_mut().pop() {
                    tracked.note_child_removed(removed.depth());
                    tracked.settle_child_depth();
                }
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
                // 深度の上限の検査は呼び出し側（`Value::check_element_depth`）。
                let new_depth = value.depth();
                Value::detach_list(list, budget, phase)?;
                let old_bytes = list.bytes;
                {
                    let tracked = Rc::get_mut(list).expect("detach 後は一意所有");
                    if idx < tracked.data.len() {
                        let old = std::mem::replace(&mut tracked.data_mut()[idx], value);
                        tracked.note_child_removed(old.depth());
                        tracked.note_child_added(new_depth);
                        tracked.settle_child_depth();
                    }
                }
                Value::retrack_list(list, old_bytes, budget, phase)?;
            }
            (Value::Dict(dict), IndexTarget::DictKey(key)) => {
                let new_depth = value.depth();
                Value::detach_dict(dict, budget, phase)?;
                let old_bytes = dict.bytes;
                {
                    let tracked = Rc::get_mut(dict).expect("detach 後は一意所有");
                    let old = tracked.data_mut().insert(key, value);
                    if let Some(old) = old {
                        tracked.note_child_removed(old.depth());
                    }
                    tracked.note_child_added(new_depth);
                    tracked.settle_child_depth();
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
            Value::Float(n) => write!(f, "{}", crate::determinism::format_float(*n)),
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

    // ---- REV-017: TotalOrder（全順序）, semantic-decisions §17.8 ----

    use std::cmp::Ordering;

    fn s(x: &str) -> Value {
        Value::str_from(x)
    }
    fn list(xs: Vec<Value>) -> Value {
        Value::List(Tracked::constant(xs))
    }
    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        Value::Dict(Tracked::constant(m))
    }
    fn tree_fn() -> Value {
        Value::Fn {
            id: FunctionId(1),
            def: Rc::new(FnDef {
                name: "<lambda>".to_string(),
                params: vec![],
                body: crate::ast::Block::from(Vec::new()),
            }),
            captured: Rc::new(HashMap::new()),
            header: Value::fn_header_untracked(),
            retain_tokens: Vec::new(),
        }
    }
    fn vm_fn() -> Value {
        Value::VmFn {
            id: FunctionId(2),
            name: "<lambda>".to_string(),
            arity: 0,
            params: vec![],
            chunk: Rc::new(crate::chunk::Chunk::new()),
            upvalues: Vec::new(),
            header: Value::fn_header_untracked(),
            retain_tokens: Vec::new(),
        }
    }
    fn err_val() -> Value {
        Value::Error {
            error_type: "type".to_string(),
            message: "boom".to_string(),
            line: 1,
        }
    }

    /// TotalOrder で安定ソートした結果を返すヘルパ（builtin_sort_budgeted の 0.21 経路と同じ比較）。
    fn total_sort(mut xs: Vec<Value>) -> Vec<Value> {
        xs.sort_by(TotalOrder::compare);
        xs
    }

    #[test]
    fn total_order_numeric_sort() {
        assert_eq!(
            total_sort(vec![int(3), int(10), int(2)]),
            vec![int(2), int(3), int(10)]
        );
        // Int/Float 混在は 1 本の数値帯で並ぶ。
        assert_eq!(
            total_sort(vec![int(2), flt(1.5), int(10)]),
            vec![flt(1.5), int(2), int(10)]
        );
    }

    #[test]
    fn total_order_numeric_near_2pow53() {
        // 2^53 = 9007199254740992。丸めなしの厳密順序。
        let a = int(9007199254740993); // 2^53 + 1
        let f = flt(9007199254740992.0); // 2^53
        assert_eq!(TotalOrder::compare(&f, &a), Ordering::Less);
        assert_eq!(TotalOrder::compare(&a, &f), Ordering::Greater);
    }

    #[test]
    fn total_order_signed_zero_equal() {
        // -0.0 と 0.0 は数値として等しい。
        assert_eq!(TotalOrder::compare(&flt(-0.0), &flt(0.0)), Ordering::Equal);
        assert_eq!(TotalOrder::compare(&flt(0.0), &flt(-0.0)), Ordering::Equal);
    }

    #[test]
    fn total_order_infinities_and_nan_at_end() {
        let ninf = flt(f64::NEG_INFINITY);
        let pinf = flt(f64::INFINITY);
        let nan = flt(f64::NAN);
        // -Inf < 有限 < +Inf < NaN。NaN を含むため Value の PartialEq（NaN != NaN）に依存せず、
        // 各位置を型・値で確認する。
        let sorted = total_sort(vec![pinf, int(0), ninf, nan.clone()]);
        assert!(matches!(sorted[0], Value::Float(f) if f == f64::NEG_INFINITY));
        assert_eq!(sorted[1], int(0));
        assert!(matches!(sorted[2], Value::Float(f) if f == f64::INFINITY));
        assert!(matches!(sorted[3], Value::Float(f) if f.is_nan())); // NaN は末尾
        // NaN は末尾・NaN 同士は Equal。
        assert_eq!(TotalOrder::compare(&nan, &nan), Ordering::Equal);
        assert_eq!(TotalOrder::compare(&nan, &int(0)), Ordering::Greater);
        assert_eq!(TotalOrder::compare(&int(0), &nan), Ordering::Less);
    }

    #[test]
    fn total_order_str_code_point_order() {
        assert_eq!(
            total_sort(vec![s("banana"), s("apple"), s("Cherry")]),
            // 大文字は小文字よりコードポイントが小さい。
            vec![s("Cherry"), s("apple"), s("banana")]
        );
    }

    #[test]
    fn total_order_type_band_order() {
        // Null < Bool < 数値 < Str < List < Dict < Fn < Error（代表値 1 つずつ）。
        let input = vec![
            err_val(),
            tree_fn(),
            dict(vec![("k", int(1))]),
            list(vec![int(1)]),
            s("x"),
            int(5),
            Value::Bool(true),
            Value::Null,
        ];
        let sorted = total_sort(input);
        let ranks: Vec<u8> = sorted.iter().map(TotalOrder::band_rank).collect();
        assert_eq!(ranks, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn total_order_list_recursion_and_prefix() {
        // [] < [1] < [1,2] < [2]
        assert_eq!(
            total_sort(vec![
                list(vec![int(2)]),
                list(vec![int(1), int(2)]),
                list(vec![int(1)]),
                list(vec![]),
            ]),
            vec![
                list(vec![]),
                list(vec![int(1)]),
                list(vec![int(1), int(2)]),
                list(vec![int(2)]),
            ]
        );
        // ネスト List も同じ規則で再帰する。
        assert_eq!(
            total_sort(vec![
                list(vec![int(1), list(vec![int(2)])]),
                list(vec![int(1), list(vec![int(1)])]),
                list(vec![int(1)]),
            ]),
            vec![
                list(vec![int(1)]),
                list(vec![int(1), list(vec![int(1)])]),
                list(vec![int(1), list(vec![int(2)])]),
            ]
        );
    }

    #[test]
    fn total_order_dict_empty_keysorted_value_recursion_prefix() {
        // 空 Dict が最小、key ソート済み (key,value) 列の辞書式。
        assert_eq!(
            total_sort(vec![
                dict(vec![("b", int(1))]),
                dict(vec![("a", int(2))]),
                dict(vec![]),
            ]),
            vec![
                dict(vec![]),
                dict(vec![("a", int(2))]),
                dict(vec![("b", int(1))]),
            ]
        );
        // key が等しければ value を再帰比較する。
        assert_eq!(
            total_sort(vec![dict(vec![("a", int(2))]), dict(vec![("a", int(1))])]),
            vec![dict(vec![("a", int(1))]), dict(vec![("a", int(2))])]
        );
        // prefix: entry 数が少ない方が小。
        assert_eq!(
            TotalOrder::compare(
                &dict(vec![("a", int(1))]),
                &dict(vec![("a", int(1)), ("b", int(1))])
            ),
            Ordering::Less
        );
    }

    #[test]
    fn total_order_fn_and_error_equal_within_band() {
        // Fn 帯内は全て Equal。tree の Fn と VM の VmFn は同一帯・同順位。
        assert_eq!(TotalOrder::compare(&tree_fn(), &vm_fn()), Ordering::Equal);
        assert_eq!(TotalOrder::compare(&vm_fn(), &tree_fn()), Ordering::Equal);
        assert_eq!(
            TotalOrder::band_rank(&tree_fn()),
            TotalOrder::band_rank(&vm_fn())
        );
        // Error 帯内も全て Equal（内容を key にしない）。
        let e1 = err_val();
        let e2 = Value::Error {
            error_type: "name".to_string(),
            message: "other".to_string(),
            line: 99,
        };
        assert_eq!(TotalOrder::compare(&e1, &e2), Ordering::Equal);
    }

    #[test]
    fn total_order_stable_for_equal_rank() {
        // 同順位（Fn 帯）要素は安定ソートで元の相対順序を保つ。
        let f1 = Value::Fn {
            id: FunctionId(10),
            def: Rc::new(FnDef {
                name: "a".to_string(),
                params: vec![],
                body: crate::ast::Block::from(Vec::new()),
            }),
            captured: Rc::new(HashMap::new()),
            header: Value::fn_header_untracked(),
            retain_tokens: Vec::new(),
        };
        let f2 = Value::Fn {
            id: FunctionId(11),
            def: Rc::new(FnDef {
                name: "b".to_string(),
                params: vec![],
                body: crate::ast::Block::from(Vec::new()),
            }),
            captured: Rc::new(HashMap::new()),
            header: Value::fn_header_untracked(),
            retain_tokens: Vec::new(),
        };
        let sorted = total_sort(vec![f1.clone(), f2.clone()]);
        // 元順序 f1, f2 が保たれる（id で判定）。
        assert_eq!(sorted[0], f1);
        assert_eq!(sorted[1], f2);
    }

    #[test]
    fn total_order_three_properties() {
        // 代表値集合で完全性・反対称性・推移性を網羅的に検査する。
        let samples = vec![
            Value::Null,
            Value::Bool(false),
            Value::Bool(true),
            flt(f64::NEG_INFINITY),
            int(-10),
            flt(1.5),
            int(2),
            int(10),
            flt(f64::INFINITY),
            flt(f64::NAN),
            s("a"),
            s("b"),
            list(vec![]),
            list(vec![int(1)]),
            list(vec![int(1), int(2)]),
            dict(vec![]),
            dict(vec![("a", int(1))]),
            tree_fn(),
            vm_fn(),
            err_val(),
        ];

        // 完全性: 任意 2 値が比較可能（compare は常に Ordering を返すので型で保証されるが、
        // 反対称性と併せて確認する）。
        for a in &samples {
            for b in &samples {
                let ab = TotalOrder::compare(a, b);
                let ba = TotalOrder::compare(b, a);
                // 反対称性: compare(a,b) == reverse(compare(b,a))。
                assert_eq!(ab, ba.reverse(), "antisymmetry failed");
            }
        }

        // 推移性: a<=b && b<=c ⇒ a<=c。
        let le = |a: &Value, b: &Value| TotalOrder::compare(a, b) != Ordering::Greater;
        for a in &samples {
            for b in &samples {
                for c in &samples {
                    if le(a, b) && le(b, c) {
                        assert!(le(a, c), "transitivity failed");
                    }
                }
            }
        }
    }

    #[test]
    fn total_order_terminates_on_large_and_deep_lists() {
        // 純比較なので大きい/深いリストでも停止する（panic/無限ループしない）。
        let big_a: Vec<Value> = (0..10_000).map(int).collect();
        let big_b: Vec<Value> = (0..10_000).map(int).collect();
        assert_eq!(
            TotalOrder::compare(&list(big_a), &list(big_b)),
            Ordering::Equal
        );
        // 深いネスト（1000 段）。
        let mut deep_a = list(vec![int(0)]);
        let mut deep_b = list(vec![int(0)]);
        for _ in 0..1000 {
            deep_a = list(vec![deep_a]);
            deep_b = list(vec![deep_b]);
        }
        assert_eq!(TotalOrder::compare(&deep_a, &deep_b), Ordering::Equal);
    }
}
