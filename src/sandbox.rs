//! ファイルI/Oサンドボックス
//!
//! 環境変数 `TSUMUGI_SANDBOX` が設定されている場合、
//! ファイル操作の対象パスが許可リスト内に収まっているか検証する。
//! 未設定の場合はサンドボックス無効（全パス許可）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::TsumugiError;

/// サンドボックスの許可パスリスト（プロセス起動時に一度だけ解決）
static SANDBOX_PATHS: OnceLock<Option<Vec<PathBuf>>> = OnceLock::new();

/// 許可パスリストを取得する（初回呼び出し時に環境変数から解決）
fn allowed_paths() -> &'static Option<Vec<PathBuf>> {
    SANDBOX_PATHS.get_or_init(|| {
        let val = std::env::var("TSUMUGI_SANDBOX").ok()?;
        if val.is_empty() {
            return None;
        }
        let paths: Vec<PathBuf> = val
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                // 絶対パスに正規化する（存在しないパスは absolutize で処理）
                let p = Path::new(s);
                if p.is_absolute() {
                    // canonicalize できればシンボリックリンクも解決する
                    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
                } else {
                    // 相対パスは CWD 基準で絶対化
                    std::env::current_dir()
                        .unwrap_or_else(|_| PathBuf::from("/"))
                        .join(p)
                        .canonicalize()
                        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(p))
                }
            })
            .collect();
        Some(paths)
    })
}

/// 指定パスがサンドボックスの許可範囲内かチェックする。
/// サンドボックスが無効（環境変数未設定）の場合は正規化パスを返す。
/// 範囲外の場合はランタイムエラーを返す。
/// 戻り値の PathBuf を実際のファイル操作に使うことで TOCTOU を防止する。
pub fn check_path(path_str: &str, line: usize) -> Result<PathBuf, TsumugiError> {
    // 対象パスを絶対パスに正規化
    let target = normalize_path(path_str);

    let Some(allowed) = allowed_paths() else {
        // サンドボックス無効: 正規化パスをそのまま返す
        return Ok(target);
    };

    // 許可パスのいずれかのプレフィックスに合致するか
    for allowed_path in allowed {
        if target.starts_with(allowed_path) {
            return Ok(target);
        }
    }

    Err(TsumugiError::runtime_with_kind(
        line,
        crate::error::ErrorKind::Sandbox,
        format!("サンドボックス違反: パス \"{}\" は許可範囲外です", path_str),
    ))
}

/// パスを絶対パスに正規化する。
/// 存在しないパスでも動作するように canonicalize ではなく手動で処理する。
/// 中間ディレクトリのシンボリックリンク迂回を防ぐため、
/// 存在する最も近い祖先ディレクトリまで遡って canonicalize する。
fn normalize_path(path_str: &str) -> PathBuf {
    let p = Path::new(path_str);
    let absolute = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(p)
    };

    // 最終パス全体が canonicalize できればそれを使う（シンボリックリンク解決 + .. 解決）
    if let Ok(resolved) = absolute.canonicalize() {
        return resolved;
    }

    // 存在する最も近い祖先まで遡って canonicalize し、残りを join する
    // これにより中間のシンボリックリンクが解決される
    let mut ancestor = absolute.as_path();
    let mut tail_parts: Vec<&std::ffi::OsStr> = Vec::new();

    while let Some(parent) = ancestor.parent() {
        if let Some(file_name) = ancestor.file_name() {
            tail_parts.push(file_name);
        }
        ancestor = parent;
        if let Ok(resolved_ancestor) = ancestor.canonicalize() {
            // 祖先を解決できた: 残りのパーツを join して返す
            let mut result = resolved_ancestor;
            for part in tail_parts.into_iter().rev() {
                result = result.join(part);
            }
            return result;
        }
    }

    // どの祖先も canonicalize できない場合は手動で .. を解決する
    resolve_dots(&absolute)
}

/// パス中の `.` と `..` を手動で解決する（パスが存在しなくても動作する）
fn resolve_dots(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_dots() {
        assert_eq!(resolve_dots(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(resolve_dots(Path::new("/a/b/./c")), PathBuf::from("/a/b/c"));
        assert_eq!(resolve_dots(Path::new("/a/b/../../c")), PathBuf::from("/c"));
    }
}
