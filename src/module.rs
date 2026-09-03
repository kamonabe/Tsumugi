//! import の解決（モジュールリンク）
//!
//! `import` は実行前にすべて解決する（AUD-030）。読み込み・パース・サンドボックス検査・
//! 深度検査をプログラム開始前に終わらせ、`import` 文を対象モジュールの文へ置き換えた
//! 「リンク済みプログラム」を作る。モジュールのトップレベル文は `import` 文があった位置に
//! 展開されるため、実行順序は従来と同じである。
//!
//! ツリーウォーク版とVM版が同じ実装を共有するので、評価時点がengine間でずれない。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::ast::{Program, Stmt};
use crate::error::{ErrorKind, TsumugiError};
use crate::limits::MAX_IMPORT_DEPTH;

fn import_error(line: usize, message: impl Into<String>) -> TsumugiError {
    TsumugiError::runtime_with_kind(line, ErrorKind::Import, message)
}

/// import の解決状態を保持するローダー
///
/// 解決済みモジュールの集合はセッション内で保持する。REPLでは入力をまたいで同じ
/// モジュールを二重に展開しないために使い、リンクが失敗した入力の分は巻き戻す。
#[derive(Debug, Clone)]
pub struct ModuleLoader {
    /// 相対パスの基準ディレクトリ
    base_dir: PathBuf,
    /// 解決済みモジュールの正規パス（循環importの検出と二重展開の防止）
    loaded: HashSet<PathBuf>,
}

impl ModuleLoader {
    pub fn new() -> Self {
        Self {
            base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            loaded: HashSet::new(),
        }
    }

    /// 実行するスクリプトを基準に、相対パスの解決元と自己importの防止を設定する
    pub fn set_base_dir(&mut self, script: &Path) {
        if let Some(parent) = script.parent() {
            self.base_dir = parent.to_path_buf();
        }
        // 実行されたファイル自体も解決済みにする（自分自身の import を防ぐ）
        if let Ok(canonical) = std::fs::canonicalize(script) {
            self.loaded.insert(canonical);
        }
    }

    /// 解決済みの記録を取り消す。
    ///
    /// リンクは成功したが実行が完了しなかったモジュールを、未解決へ戻すために使う。
    /// これで同じパスを再度 import できる（AUD-006）。
    pub fn forget(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.loaded.remove(path);
        }
    }

    /// top-level import を解決し、展開済みプログラムと新たに解決したパスを返す。
    ///
    /// import が無ければプログラムは `None` を返し、呼び出し側は元のプログラムを
    /// そのまま使える（リンクのためだけにAST全体を複製しない）。
    pub fn link(
        &mut self,
        program: &Program,
    ) -> Result<(Option<Program>, Vec<PathBuf>), TsumugiError> {
        if !program
            .iter()
            .any(|stmt| matches!(stmt, Stmt::Import { .. }))
        {
            return Ok((None, Vec::new()));
        }

        let mut linked = Vec::with_capacity(program.len());
        let mut newly_loaded = Vec::new();
        let base_dir = self.base_dir.clone();
        if let Err(error) = self.link_into(program, &base_dir, 0, &mut linked, &mut newly_loaded) {
            // 失敗した import は解決済みにしない（同じパスを再試行できる）
            self.forget(&newly_loaded);
            return Err(error);
        }
        Ok((Some(linked), newly_loaded))
    }

    /// `program` の文を出力へ写しつつ、import を対象モジュールの文へ置き換える。
    ///
    /// 再帰の深さは `MAX_IMPORT_DEPTH` で抑えられているため、host stackを消費し切らない。
    fn link_into(
        &mut self,
        program: &Program,
        base_dir: &Path,
        depth: usize,
        out: &mut Vec<Stmt>,
        newly_loaded: &mut Vec<PathBuf>,
    ) -> Result<(), TsumugiError> {
        for stmt in program {
            let Stmt::Import { path, line } = stmt else {
                out.push(stmt.clone());
                continue;
            };

            // 解決済み（循環を含む）は成功扱いでスキップし、深度も消費しない
            let Some((canonical, source)) = self.resolve(base_dir, path, *line, depth)? else {
                continue;
            };
            self.loaded.insert(canonical.clone());
            newly_loaded.push(canonical.clone());

            let module = parse_module(&source, path, *line)?;
            let module_dir = canonical
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| base_dir.to_path_buf());
            self.link_into(&module, &module_dir, depth + 1, out, newly_loaded)?;
        }
        Ok(())
    }

    /// import 先を特定して読み込む。解決済みなら `None` を返す。
    fn resolve(
        &self,
        base_dir: &Path,
        path: &str,
        line: usize,
        depth: usize,
    ) -> Result<Option<(PathBuf, String)>, TsumugiError> {
        let resolved = base_dir.join(path);
        let canonical = std::fs::canonicalize(&resolved).map_err(|_| {
            import_error(
                line,
                format!(
                    "import に失敗しました: モジュールを読み込めません: {}",
                    path
                ),
            )
        })?;

        // サンドボックスチェック: import 先が許可範囲内か検証
        crate::sandbox::check_path(canonical.to_str().unwrap_or(""), line)?;

        if self.loaded.contains(&canonical) {
            return Ok(None);
        }
        if depth >= MAX_IMPORT_DEPTH {
            return Err(import_error(
                line,
                format!(
                    "import 失敗: ネストが深すぎます (上限: {})",
                    MAX_IMPORT_DEPTH
                ),
            ));
        }

        let source = std::fs::read_to_string(&canonical).map_err(|_| {
            import_error(
                line,
                format!(
                    "import に失敗しました: モジュールを読み込めません: {}",
                    path
                ),
            )
        })?;
        Ok(Some((canonical, source)))
    }
}

impl Default for ModuleLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// モジュールのソースをパースする。構文エラーは import エラーとして報告する。
fn parse_module(source: &str, path: &str, line: usize) -> Result<Program, TsumugiError> {
    let tokens = crate::lexer::Lexer::new(source).tokenize();
    crate::parser::Parser::new(tokens)
        .parse()
        .map_err(|_errors| {
            // parse詳細はcauseとして保持する設計だが、cause機構は未実装のため
            // 現状は canonical wrapper message のみを返す（AUD-019）。
            import_error(
                line,
                format!(
                    "import に失敗しました: モジュールの構文が不正です: {}",
                    path
                ),
            )
        })
}
