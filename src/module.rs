//! import の解決（モジュールリンク）
//!
//! `import` は実行前にすべて解決する（AUD-030）。読み込み・パース・サンドボックス検査・
//! 深度検査をプログラム開始前に終わらせ、`import` 文を対象モジュールの文へ置き換えた
//! 「リンク済みプログラム」を作る。モジュールのトップレベル文は `import` 文があった位置に
//! 展開されるため、実行順序は従来と同じである。
//!
//! ツリーウォーク版とVM版が同じ実装を共有するので、評価時点がengine間でずれない。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::ast::{Program, Stmt};
use crate::error::{ErrorKind, TsumugiError};
use crate::limits::MAX_IMPORT_DEPTH;
use crate::value::HeapToken;

fn import_error(line: usize, message: impl Into<String>) -> TsumugiError {
    TsumugiError::runtime_with_kind(line, ErrorKind::Import, message)
}

/// リンク中に初めて解決した import モジュール 1 本の記録。
///
/// 実行が完了しなかった入力を巻き戻す（[`ModuleLoader::forget`]）ためのパスに加えて、
/// source/import 予算（REV-015 Slice 2）の課金に使う生 byte 長と、診断行番号を持つ。
#[derive(Debug, Clone)]
pub struct LoadedModule {
    /// 正規化した import 先パス（`forget` で未解決へ戻す対象）。
    pub path: PathBuf,
    /// import source の生 UTF-8 byte 長（`source_bytes` / `import_bytes` の課金量）。
    pub byte_len: u64,
    /// import 文の行番号（予算超過エラーの行表示に使う）。
    pub line: usize,
}

/// import の解決状態を保持するローダー
///
/// 解決済みモジュールの集合はセッション内で保持する。REPLでは入力をまたいで同じ
/// モジュールを二重に展開しないために使い、リンクが失敗した入力の分は巻き戻す。
#[derive(Clone)]
pub struct ModuleLoader {
    /// 相対パスの基準ディレクトリ
    base_dir: PathBuf,
    /// 解決済みモジュールの正規パス（循環importの検出と二重展開の防止）
    loaded: HashSet<PathBuf>,
    /// 解決済みモジュールの imported module record（§5.1）を live heap へ課金する
    /// トークン（REV-015 PR-d）。canonical path をキーに保持し、`loaded` set と寿命を
    /// 揃える。`forget` で該当トークンを drop して release し、loader 全体の drop でも
    /// 残存トークンが release される。
    record_tokens: HashMap<PathBuf, Rc<HeapToken>>,
}

impl std::fmt::Debug for ModuleLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleLoader")
            .field("base_dir", &self.base_dir)
            .field("loaded", &self.loaded)
            .finish_non_exhaustive()
    }
}

impl ModuleLoader {
    pub fn new() -> Self {
        Self {
            base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            loaded: HashSet::new(),
            record_tokens: HashMap::new(),
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
    pub fn forget(&mut self, modules: &[LoadedModule]) {
        for module in modules {
            self.loaded.remove(&module.path);
            // imported module record の live heap を release する（REV-015 PR-d）。
            // token を drop すると `Tracked::Drop` が台帳へ release を通知する。
            self.record_tokens.remove(&module.path);
        }
    }

    /// 初めて解決した import module の imported module record トークンを登録する
    /// （REV-015 PR-d）。
    ///
    /// engine の `charge_link` が §5.1 `imported_module_record` を課金して作った token を、
    /// canonical path をキーに保持する。`forget` で該当 module を未解決へ戻すとき同時に
    /// drop して live heap を release する。同一 path の再登録（通常は起きない）では新しい
    /// token で置き換え、旧 token は drop される。
    pub fn register_record_token(&mut self, path: PathBuf, token: Rc<HeapToken>) {
        self.record_tokens.insert(path, token);
    }

    /// top-level import を解決し、展開済みプログラムと新たに解決したパスを返す。
    ///
    /// import が無ければプログラムは `None` を返し、呼び出し側は元のプログラムを
    /// そのまま使える（リンクのためだけにAST全体を複製しない）。
    pub fn link(
        &mut self,
        program: &Program,
    ) -> Result<(Option<Program>, Vec<LoadedModule>), TsumugiError> {
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
        newly_loaded: &mut Vec<LoadedModule>,
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
            // source/import 予算（REV-015 Slice 2）の課金は生 byte 長で行う。
            newly_loaded.push(LoadedModule {
                path: canonical.clone(),
                byte_len: source.len() as u64,
                line: *line,
            });

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
