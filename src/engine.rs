//! 埋め込み利用向けの最小実行 API。
//!
//! `Engine` はソースをパースして `CompiledScript` を作成し、
//! `ExecutionContext` は実行間で保持する状態を管理する。
//!
//! 現時点ではデフォルトのツリーウォーク評価器だけを対象とする。VM、ホスト I/O の
//! 注入、capability、cancellation、`exit()` を結果として返す契約は含まれない。

use std::path::Path;

use crate::ast::Program;
use crate::error::TsumugiError;
use crate::eval::Evaluator;
use crate::lexer::Lexer;
use crate::parser::Parser;

/// Tsumugi スクリプトをコンパイルして実行するエントリポイント。
#[derive(Debug, Default)]
pub struct Engine;

impl Engine {
    /// 新しい実行エンジンを作成する。
    pub fn new() -> Self {
        Self
    }

    /// ソースをパースし、再利用可能なスクリプトを作成する。
    ///
    /// import の解決は実行コンテキストに依存するため、ここでは行わない。
    pub fn compile(&self, source: &str) -> Result<CompiledScript, Vec<TsumugiError>> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse()?;

        Ok(CompiledScript { program })
    }

    /// スクリプトを実行コンテキスト内で同期的に実行する。
    ///
    /// `ExecutionContext` はスレッド局所の評価状態を保持するため、十分なスタックを
    /// 持つ同一スレッド内で生成・利用する。CLI はツリーウォークの再帰評価のために
    /// 8 MiB の実行スレッドを使う。
    pub fn execute(
        &self,
        script: &CompiledScript,
        context: &mut ExecutionContext,
    ) -> Result<ExecutionOutcome, TsumugiError> {
        context.evaluator.run(&script.program)?;
        Ok(ExecutionOutcome::Completed)
    }
}

/// パース済みで、実行可能な Tsumugi スクリプト。
pub struct CompiledScript {
    program: Program,
}

/// 実行間で維持する Tsumugi の状態。
///
/// 同じコンテキストを再利用すると、変数、関数、import の解決状態を保持する。
///
/// # スレッドとスタック
///
/// 内部の評価状態は `Send` ではないため、コンテキストを別スレッドへ移動できない。
/// `Engine::execute` は caller のスレッドで再帰的に評価するため、埋め込み先は必要な
/// スタック容量を確保したスレッド内でコンテキストを生成・利用する。
pub struct ExecutionContext {
    evaluator: Evaluator,
}

impl ExecutionContext {
    /// 新しい実行コンテキストを作成する。
    pub fn new() -> Self {
        Self {
            evaluator: Evaluator::new(),
        }
    }

    /// スクリプトファイルの完全なパスを設定する。
    ///
    /// 相対 import の解決と、スクリプト自身の再 import 防止に使われる。
    pub fn set_script_path(&mut self, path: impl AsRef<Path>) {
        self.evaluator.set_base_dir(path.as_ref());
    }

    /// REPL の次の入力を実行する前にステップ予算をリセットする。
    pub fn reset_step_budget(&mut self) {
        self.evaluator.reset_step_budget();
    }
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}

/// スクリプト実行の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOutcome {
    /// スクリプトが最後まで実行された。
    Completed,
}
