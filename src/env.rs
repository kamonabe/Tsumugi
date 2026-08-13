use std::collections::HashMap;

use crate::ast::Stmt;
use crate::value::Value;

/// 関数定義を保持する構造体
#[derive(Debug, Clone)]
pub struct Function {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
}

/// 変数と関数のスコープを管理する環境
#[derive(Debug, Clone)]
pub struct Env {
    /// スコープのスタック（末尾が現在のスコープ）
    scopes: Vec<HashMap<String, Value>>,
    /// 関数定義（グローバル）
    pub functions: HashMap<String, Function>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()], // グローバルスコープ
            functions: HashMap::new(),
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

    /// 現在のスコープに変数を定義
    pub fn set(&mut self, name: &str, value: Value) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), value);
        }
    }

    /// 変数を検索（現在のスコープ → 外側へ）
    pub fn get(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get() {
        let mut env = Env::new();
        env.set("x", Value::Int(10));
        assert_eq!(env.get("x"), Some(&Value::Int(10)));
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
        assert_eq!(env.get("x"), Some(&Value::Int(2)));

        env.pop_scope();
        assert_eq!(env.get("x"), Some(&Value::Int(1)));
    }

    #[test]
    fn inner_scope_sees_outer() {
        let mut env = Env::new();
        env.set("outer", Value::Str("visible".to_string()));

        env.push_scope();
        assert_eq!(env.get("outer"), Some(&Value::Str("visible".to_string())));
        env.pop_scope();
    }
}
