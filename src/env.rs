use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::value::{SharedValue, Value};

/// 変数のスコープを管理する環境
#[derive(Debug, Clone)]
pub struct Env {
    /// スコープのスタック（末尾が現在のスコープ）
    /// 各変数は Rc<RefCell<Value>> で保持し、クロージャと共有可能
    scopes: Vec<HashMap<String, SharedValue>>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()], // グローバルスコープ
        }
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
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), Rc::new(RefCell::new(value)));
        }
    }

    /// 現在のスコープに既存の SharedValue セルを直接挿入（クロージャの参照共有用）
    pub fn set_shared(&mut self, name: &str, cell: SharedValue) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), cell);
        }
    }

    /// 既存の変数を更新（内側→外側へ探索）。見つからなければ Err を返す
    /// 同じ SharedValue セルの中身を書き換えるため、参照を共有しているクロージャにも反映される
    pub fn update(&mut self, name: &str, value: Value) -> Result<(), ()> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(cell) = scope.get(name) {
                *cell.borrow_mut() = value;
                return Ok(());
            }
        }
        Err(())
    }

    /// 変数の値をクローンして返す（現在のスコープ → 外側へ）
    pub fn get(&self, name: &str) -> Option<Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(cell) = scope.get(name) {
                return Some(cell.borrow().clone());
            }
        }
        None
    }

    /// 変数の SharedValue セルを取得（参照キャプチャ用）
    pub fn get_cell(&self, name: &str) -> Option<SharedValue> {
        for scope in self.scopes.iter().rev() {
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

    /// 関数呼び出し用: 現在のスコープスタックを退避し、
    /// グローバルスコープ + 関数スコープだけの独立環境に切り替える。
    /// 戻り値は復元用のスコープスタック。
    pub fn push_call_frame(&mut self) -> Vec<HashMap<String, SharedValue>> {
        // グローバルスコープ（最下層）を保持し、残りを退避
        let global = self.scopes[0].clone();
        let saved = std::mem::replace(&mut self.scopes, vec![global]);
        // 関数用のローカルスコープを作成
        self.scopes.push(HashMap::new());
        saved
    }

    /// 関数から戻る時: 退避していたスコープスタックを復元する。
    pub fn pop_call_frame(&mut self, saved: Vec<HashMap<String, SharedValue>>) {
        self.scopes = saved;
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
}
