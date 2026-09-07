use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::value::{SharedValue, Value};

/// 関数呼び出しから戻るための復元情報
///
/// スコープスタックを退避・複製せず、位置だけを覚えて巻き戻す（AUD-046）。
#[derive(Debug, Clone, Copy)]
pub struct CallFrame {
    /// 呼び出し前のフレーム開始位置
    previous_base: usize,
    /// 呼び出し前のスコープ数（戻る際にここまで truncate する）
    scope_len: usize,
}

/// REPL入力（submission）が加えた言語状態の変更を、入力開始時点へ
/// 巻き戻すためのundo journal（AUD-024）。
///
/// 記録するのは「最初の書き換え時点」の元値だけで、同じ場所を複数回変更しても
/// 元値は一度しか積まない。COW（AUD-047）により List/Dict の値クローンは
/// ハンドル共有O(1)なので、記録量は変更した場所の数に比例する。
#[derive(Debug, Default, Clone)]
struct SubmissionJournal {
    /// undo 操作を後ろから順に適用する。
    undo_log: Vec<UndoEntry>,
    /// 元値を記録済みのcell（Rcポインタ同一性で判定）。
    seen_cells: HashSet<usize>,
    /// 元エントリを記録済みのscope binding（scope index と名前）。
    seen_scope_entries: HashSet<(usize, String)>,
}

/// journal に積む1件のundo操作。
#[derive(Debug, Clone)]
enum UndoEntry {
    /// scope の binding を元へ戻す。`original` が `None` なら削除する。
    ScopeEntry {
        scope: usize,
        name: String,
        original: Option<SharedValue>,
    },
    /// cell の中身を元の値へ戻す。
    CellValue { cell: SharedValue, original: Value },
}

/// 変数のスコープを管理する環境
#[derive(Debug, Clone)]
pub struct Env {
    /// スコープのスタック（末尾が現在のスコープ）
    /// 各変数は Rc<RefCell<Value>> で保持し、クロージャと共有可能
    scopes: Vec<HashMap<String, SharedValue>>,
    /// 現在のcall frameで見えるスコープの開始位置
    ///
    /// レキシカルスコープを保つため、探索対象はこの位置以降のスコープと
    /// グローバルスコープ（index 0）だけにする。間にある呼び出し元の
    /// ローカルスコープは見えない。
    frame_base: usize,
    /// REPL submission 中だけ有効な undo journal（AUD-024）。
    /// `None` のときは記録しない（ファイル実行など非トランザクション実行）。
    journal: Option<SubmissionJournal>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()], // グローバルスコープ
            frame_base: 0,
            journal: None,
        }
    }

    /// REPL submission のトランザクションを開始する（AUD-024）。
    ///
    /// 以降の binding 追加・cell 書き換えは undo journal に記録され、
    /// `rollback_submission` で入力開始時点へ戻せる。
    pub fn begin_submission(&mut self) {
        self.journal = Some(SubmissionJournal::default());
    }

    /// submission を確定し、journal を破棄する（AUD-024）。
    pub fn commit_submission(&mut self) {
        self.journal = None;
    }

    /// submission を巻き戻し、記録した全 binding・cell を入力開始時点へ戻す（AUD-024）。
    ///
    /// undo_log を逆順に適用する。同じ場所への複数回の変更は最初の元値だけを
    /// 記録しているため、逆順適用で開始時点の値に戻る。
    pub fn rollback_submission(&mut self) {
        let Some(journal) = self.journal.take() else {
            return;
        };
        for entry in journal.undo_log.into_iter().rev() {
            match entry {
                UndoEntry::ScopeEntry {
                    scope,
                    name,
                    original,
                } => {
                    if let Some(map) = self.scopes.get_mut(scope) {
                        match original {
                            Some(cell) => {
                                map.insert(name, cell);
                            }
                            None => {
                                map.remove(&name);
                            }
                        }
                    }
                }
                UndoEntry::CellValue { cell, original } => {
                    *cell.borrow_mut() = original;
                }
            }
        }
    }

    /// scope の binding を書き換える前に、その位置の元の cell を journal へ記録する。
    fn journal_scope_entry(&mut self, scope: usize, name: &str) {
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        let key = (scope, name.to_string());
        if !journal.seen_scope_entries.insert(key) {
            return;
        }
        let original = self
            .scopes
            .get(scope)
            .and_then(|map| map.get(name).cloned());
        journal.undo_log.push(UndoEntry::ScopeEntry {
            scope,
            name: name.to_string(),
            original,
        });
    }

    /// cell の中身を書き換える前に、その cell の元の値を journal へ記録する。
    ///
    /// index 代入・push・pop など `Env` の外で `borrow_mut` するコードは、
    /// 変更前に必ずこれを呼ぶ。COW により Value クローンはハンドル共有O(1)。
    pub fn journal_cell(&mut self, cell: &SharedValue) {
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        let id = Rc::as_ptr(cell) as usize;
        if !journal.seen_cells.insert(id) {
            return;
        }
        let original = cell.borrow().clone();
        journal.undo_log.push(UndoEntry::CellValue {
            cell: Rc::clone(cell),
            original,
        });
    }

    /// 現在のcall frameから見えるスコープを内側→外側の順に返す
    ///
    /// フレーム内のスコープを内側から辿り、最後にグローバルスコープを見る。
    /// `frame_base == 0`（トップレベル）ではグローバルもフレーム内に含まれる。
    fn visible_scopes(&self) -> impl Iterator<Item = &HashMap<String, SharedValue>> {
        let frame = self.scopes[self.frame_base..].iter().rev();
        let global = (self.frame_base > 0).then(|| &self.scopes[0]);
        frame.chain(global)
    }

    /// 新しいスコープを作る（関数呼び出し時）
    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// スコープを抜ける
    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// 現在のスコープに変数を定義（新しい SharedValue セルを作成）
    pub fn set(&mut self, name: &str, value: Value) {
        if let Some(scope) = self.scopes.len().checked_sub(1) {
            // binding の追加・置換前に元エントリを journal へ記録する（AUD-024）。
            self.journal_scope_entry(scope, name);
            self.scopes[scope].insert(name.to_string(), Rc::new(RefCell::new(value)));
        }
    }

    /// 現在のスコープに既存の SharedValue セルを直接挿入（クロージャの参照共有用）
    pub fn set_shared(&mut self, name: &str, cell: SharedValue) {
        if let Some(scope) = self.scopes.len().checked_sub(1) {
            self.journal_scope_entry(scope, name);
            self.scopes[scope].insert(name.to_string(), cell);
        }
    }

    /// 既存の変数を更新（内側→外側へ探索）。見つからなければ Err を返す
    /// 同じ SharedValue セルの中身を書き換えるため、参照を共有しているクロージャにも反映される
    pub fn update(&mut self, name: &str, value: Value) -> Result<(), ()> {
        match self.get_cell(name) {
            Some(cell) => {
                // cell の元値を journal へ記録してから書き換える（AUD-024）。
                self.journal_cell(&cell);
                *cell.borrow_mut() = value;
                Ok(())
            }
            None => Err(()),
        }
    }

    /// 変数の値をクローンして返す（現在のスコープ → 外側へ）
    pub fn get(&self, name: &str) -> Option<Value> {
        for scope in self.visible_scopes() {
            if let Some(cell) = scope.get(name) {
                return Some(cell.borrow().clone());
            }
        }
        None
    }

    /// 変数の SharedValue セルを取得（参照キャプチャ用）
    pub fn get_cell(&self, name: &str) -> Option<SharedValue> {
        for scope in self.visible_scopes() {
            if let Some(cell) = scope.get(name) {
                return Some(Rc::clone(cell));
            }
        }
        None
    }

    /// 指定した名前のうち、現在見えている変数セルだけを取得する（クロージャ定義時のキャプチャ用）
    ///
    /// `get_cell` と同じく内側のスコープを優先するため、shadowingはそのまま保たれる。
    /// 本体で言及されない名前を捕捉しないことで、クロージャを保持するコンテナまで
    /// 抱え込んで参照循環を作るのを避ける（AUD-042）。
    pub fn capture_referenced(&self, names: &HashSet<String>) -> HashMap<String, SharedValue> {
        let mut captured = HashMap::with_capacity(names.len());
        for name in names {
            if let Some(cell) = self.get_cell(name) {
                captured.insert(name.clone(), cell);
            }
        }
        captured
    }

    /// 関数呼び出し用: 関数スコープを積み、探索範囲をそのスコープ以降へ移す。
    ///
    /// 呼び出し元のローカルスコープはスタック上に残るが、`frame_base` より
    /// 手前にあるため見えない。グローバルスコープは複製せず共有する。
    pub fn push_call_frame(&mut self) -> CallFrame {
        let frame = CallFrame {
            previous_base: self.frame_base,
            scope_len: self.scopes.len(),
        };
        // 関数用のローカルスコープを作成し、そこをフレームの底にする
        self.scopes.push(HashMap::new());
        self.frame_base = self.scopes.len() - 1;
        frame
    }

    /// 関数から戻る時: 関数スコープを捨て、呼び出し元の探索範囲へ戻す。
    pub fn pop_call_frame(&mut self, frame: CallFrame) {
        self.scopes.truncate(frame.scope_len);
        self.frame_base = frame.previous_base;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get() {
        let mut env = Env::new();
        env.set("x", Value::Int(10));
        assert_eq!(env.get("x"), Some(Value::Int(10)));
    }

    #[test]
    fn undefined_variable() {
        let env = Env::new();
        assert_eq!(env.get("nope"), None);
    }

    #[test]
    fn scope_shadowing() {
        let mut env = Env::new();
        env.set("x", Value::Int(1));

        env.push_scope();
        env.set("x", Value::Int(2));
        assert_eq!(env.get("x"), Some(Value::Int(2)));

        env.pop_scope();
        assert_eq!(env.get("x"), Some(Value::Int(1)));
    }

    #[test]
    fn inner_scope_sees_outer() {
        let mut env = Env::new();
        env.set("outer", Value::Str("visible".to_string()));

        env.push_scope();
        assert_eq!(env.get("outer"), Some(Value::Str("visible".to_string())));
        env.pop_scope();
    }

    #[test]
    fn update_existing_variable() {
        let mut env = Env::new();
        env.set("x", Value::Int(1));
        assert!(env.update("x", Value::Int(2)).is_ok());
        assert_eq!(env.get("x"), Some(Value::Int(2)));
    }

    #[test]
    fn update_undefined_variable_fails() {
        let mut env = Env::new();
        assert!(env.update("nope", Value::Int(1)).is_err());
    }

    #[test]
    fn update_outer_scope_variable() {
        let mut env = Env::new();
        env.set("x", Value::Int(1));
        env.push_scope();
        // 内側スコープから外側の変数を更新できる
        assert!(env.update("x", Value::Int(99)).is_ok());
        env.pop_scope();
        assert_eq!(env.get("x"), Some(Value::Int(99)));
    }

    #[test]
    fn call_frame_hides_caller_locals_but_keeps_globals() {
        let mut env = Env::new();
        env.set("global_var", Value::Int(1));
        env.push_scope();
        env.set("caller_local", Value::Int(2));

        let frame = env.push_call_frame();
        // グローバルは見えるが、呼び出し元のローカルは見えない
        assert_eq!(env.get("global_var"), Some(Value::Int(1)));
        assert_eq!(env.get("caller_local"), None);

        // フレーム内のローカルはフレーム内だけで有効
        env.set("callee_local", Value::Int(3));
        assert_eq!(env.get("callee_local"), Some(Value::Int(3)));

        env.pop_call_frame(frame);
        assert_eq!(env.get("callee_local"), None);
        assert_eq!(env.get("caller_local"), Some(Value::Int(2)));
        assert_eq!(env.get("global_var"), Some(Value::Int(1)));
    }

    #[test]
    fn call_frame_updates_globals_through_shared_cells() {
        let mut env = Env::new();
        env.set("counter", Value::Int(0));

        let frame = env.push_call_frame();
        assert!(env.update("counter", Value::Int(5)).is_ok());
        // フレーム内で作ったローカルは呼び出し元へ漏れない
        env.set("counter", Value::Int(99));
        env.pop_call_frame(frame);

        assert_eq!(env.get("counter"), Some(Value::Int(5)));
    }

    #[test]
    fn nested_call_frames_isolate_each_level() {
        let mut env = Env::new();
        env.set("g", Value::Int(0));

        let outer = env.push_call_frame();
        env.set("outer_local", Value::Int(1));

        let inner = env.push_call_frame();
        assert_eq!(
            env.get("outer_local"),
            None,
            "内側から外側のローカルが見えている"
        );
        assert_eq!(env.get("g"), Some(Value::Int(0)));
        env.pop_call_frame(inner);

        assert_eq!(env.get("outer_local"), Some(Value::Int(1)));
        env.pop_call_frame(outer);
        assert_eq!(env.get("outer_local"), None);
    }

    #[test]
    fn block_scopes_inside_a_call_frame_stay_visible() {
        let mut env = Env::new();
        let frame = env.push_call_frame();
        env.set("param", Value::Int(1));

        env.push_scope();
        env.set("block_local", Value::Int(2));
        assert_eq!(env.get("param"), Some(Value::Int(1)));
        assert_eq!(env.get("block_local"), Some(Value::Int(2)));
        env.pop_scope();

        assert_eq!(env.get("block_local"), None);
        env.pop_call_frame(frame);
    }

    #[test]
    fn capture_referenced_takes_only_named_cells() {
        // 言及されない名前は捕捉しない（AUD-042の参照循環対策）
        let mut env = Env::new();
        env.set("wanted", Value::Int(1));
        env.set("container", Value::Int(2));

        let names = HashSet::from(["wanted".to_string(), "missing".to_string()]);
        let captured = env.capture_referenced(&names);

        assert_eq!(captured.len(), 1);
        assert_eq!(*captured["wanted"].borrow(), Value::Int(1));
        assert!(!captured.contains_key("container"));
        assert!(!captured.contains_key("missing"));
    }

    #[test]
    fn capture_referenced_prefers_inner_scope_and_shares_cells() {
        let mut env = Env::new();
        env.set("x", Value::Int(1));
        env.push_scope();
        env.set("x", Value::Int(2));

        let captured = env.capture_referenced(&HashSet::from(["x".to_string()]));
        assert_eq!(*captured["x"].borrow(), Value::Int(2));

        // 捕捉したセルは共有されるため、後の更新が見える
        env.update("x", Value::Int(3)).unwrap();
        assert_eq!(*captured["x"].borrow(), Value::Int(3));
    }

    #[test]
    fn shared_capture() {
        // クロージャが変数セルを共有し、外側からの更新が反映される
        let mut env = Env::new();
        env.set("counter", Value::Int(0));
        let cell = env.get_cell("counter").unwrap();

        // 外側から更新
        env.update("counter", Value::Int(42)).unwrap();

        // 共有セル経由でも最新の値が見える
        assert_eq!(*cell.borrow(), Value::Int(42));
    }

    // --- AUD-024: submission transaction ---

    #[test]
    fn rollback_reverts_new_binding_and_assignment() {
        let mut env = Env::new();
        env.set("x", Value::Int(1));

        env.begin_submission();
        env.update("x", Value::Int(2)).unwrap(); // 既存cellの書換
        env.set("y", Value::Int(9)); // 新規binding
        env.rollback_submission();

        assert_eq!(env.get("x"), Some(Value::Int(1)), "代入が巻き戻っていない");
        assert_eq!(env.get("y"), None, "新規bindingが公開されたまま");
    }

    #[test]
    fn commit_keeps_changes() {
        let mut env = Env::new();
        env.set("x", Value::Int(1));

        env.begin_submission();
        env.update("x", Value::Int(2)).unwrap();
        env.set("y", Value::Int(9));
        env.commit_submission();

        assert_eq!(env.get("x"), Some(Value::Int(2)));
        assert_eq!(env.get("y"), Some(Value::Int(9)));
    }

    #[test]
    fn rollback_reverts_redeclaration_to_original_cell() {
        // 再宣言は新cellを作る（AUD-016）。rollbackで元cellへ戻す。
        let mut env = Env::new();
        env.set("x", Value::Int(1));
        let original = env.get_cell("x").unwrap();

        env.begin_submission();
        env.set("x", Value::Int(2)); // 再宣言 = 新cell
        assert!(!Rc::ptr_eq(&original, &env.get_cell("x").unwrap()));
        env.rollback_submission();

        let restored = env.get_cell("x").unwrap();
        assert!(
            Rc::ptr_eq(&original, &restored),
            "rollbackで元のcellへ戻っていない"
        );
        assert_eq!(*restored.borrow(), Value::Int(1));
    }

    #[test]
    fn rollback_reverts_shared_cell_seen_by_closure() {
        // closureが共有するcellの破壊的更新も巻き戻す。
        let mut env = Env::new();
        env.set("count", Value::Int(0));
        let captured = env.get_cell("count").unwrap();

        env.begin_submission();
        env.update("count", Value::Int(100)).unwrap();
        assert_eq!(*captured.borrow(), Value::Int(100));
        env.rollback_submission();

        assert_eq!(
            *captured.borrow(),
            Value::Int(0),
            "共有cellが巻き戻っていない"
        );
    }

    #[test]
    fn journal_records_original_only_once_per_cell() {
        // 同じcellを複数回変更しても、最初の元値へ戻る。
        let mut env = Env::new();
        env.set("x", Value::Int(1));

        env.begin_submission();
        env.update("x", Value::Int(2)).unwrap();
        env.update("x", Value::Int(3)).unwrap();
        env.rollback_submission();

        assert_eq!(env.get("x"), Some(Value::Int(1)));
    }

    #[test]
    fn no_journal_outside_submission() {
        // トランザクション外の変更は記録されず、rollbackは何もしない。
        let mut env = Env::new();
        env.set("x", Value::Int(1));
        env.update("x", Value::Int(2)).unwrap();
        env.rollback_submission(); // journalなし: no-op
        assert_eq!(env.get("x"), Some(Value::Int(2)));
    }
}
