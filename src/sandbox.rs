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
/// サンドボックスが無効（環境変数未設定）の場合は常に Ok。
/// 範囲外の場合はランタイムエラーを返す。
pub fn check_path(path_str: &str, line: usize) -> Result<(), TsumugiError> {
    let Some(allowed) = allowed_paths() else {
        // サンドボックス無効
        return Ok(());
    };

    // 対象パスを絶対パスに正規化
    let target = normalize_path(path_str);

    // 許可パスのいずれかのプレフィックスに合致するか
    for allowed_path in allowed {
        if target.starts_with(allowed_path) {
            return Ok(());
        }
    }

    Err(TsumugiError::Runtime {
        line,
        message: format!("サンドボックス違反: パス \"{}\" は許可範囲外です", path_str),
        trace: Vec::new(),
    })
}

/// パスを絶対パスに正規化する。
/// 存在しないパスでも動作するように canonicalize ではなく手動で処理する。
fn normalize_path(path_str: &str) -> PathBuf {
    let p = Path::new(path_str);
    let absolute = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(p)
    };

    // canonicalize が成功すればそれを使う（シンボリックリンク解決 + .. 解決）
    // 失敗する場合（パスが存在しない）は手動で .. を解決する
    absolute
        .canonicalize()
        .unwrap_or_else(|_| resolve_dots(&absolute))
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
