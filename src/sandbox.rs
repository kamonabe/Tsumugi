//! import 解決サンドボックス（C6 専用に縮退）
//!
//! 環境変数 `TSUMUGI_SANDBOX` が設定されている場合、import 解決（`module.rs::ModuleLoader`）の
//! 対象パスが許可リスト内に収まっているか検証する。未設定の場合はサンドボックス無効（全パス許可）。
//!
//! C10 で runtime fs builtin（`read_file` 等）の ambient 経路は capability 経路へ全面移行した
//! ため、本モジュールの `check_path` / `check_entry_path` を呼ぶのは **import 解決
//! （`module.rs`、C6）だけ** になった。import resolver が capability（`ModuleResolver`、C6/E7）
//! 化されたら本モジュールごと撤去する。

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
/// 戻り値を実際のファイル操作に使い、検査対象と操作対象を一致させる。
/// ただし、検査後にsymlinkを差し替えるcheck/use raceまでは防止しない。
///
/// path は `&Path` で受け取る（REV-002）。`&str` 経由の lossy 変換を挟まないため、
/// 非UTF-8なcanonical import path（Unix）でも認可対象と操作対象が分離しない。
pub fn check_path(path: &Path, line: usize) -> Result<PathBuf, TsumugiError> {
    authorize_path(normalize_path(path), path, line)
}

/// directory entry自体を変更する操作向けのサンドボックス検査。
/// 中間componentは解決するが、final componentはsymlink targetへ展開しない。
pub fn check_entry_path(path: &Path, line: usize) -> Result<PathBuf, TsumugiError> {
    authorize_path(normalize_entry_path(path), path, line)
}

/// 正規化済みpathを許可リストと照合する。
/// `original` は認可対象そのもの（`&Path`）で、違反時のメッセージ表示にだけ使う。
fn authorize_path(target: PathBuf, original: &Path, line: usize) -> Result<PathBuf, TsumugiError> {
    let Some(allowed) = allowed_paths() else {
        return Ok(target);
    };

    if allowed
        .iter()
        .any(|allowed_path| target.starts_with(allowed_path))
    {
        return Ok(target);
    }

    Err(TsumugiError::runtime_with_kind(
        line,
        crate::error::ErrorKind::Sandbox,
        format!(
            "サンドボックス違反: パス \"{}\" は許可範囲外です",
            original.display()
        ),
    ))
}

/// パスを絶対化する。
fn absolutize_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    }
}

/// targetを読み書きする操作向けに、final componentを含むpath全体を正規化する。
fn normalize_path(path: &Path) -> PathBuf {
    normalize_absolute_path(&absolutize_path(path))
}

/// unlink/rename対象のdirectory entry向けに、中間componentだけを正規化する。
fn normalize_entry_path(path: &Path) -> PathBuf {
    let absolute = absolutize_path(path);
    let Some(file_name) = absolute.file_name() else {
        return normalize_absolute_path(&absolute);
    };
    let Some(parent) = absolute.parent() else {
        return normalize_absolute_path(&absolute);
    };

    normalize_absolute_path(parent).join(file_name)
}

/// 絶対パスを正規化する。
/// 存在しないパスでも動作し、中間symlinkを解決するために
/// 存在する最も近い祖先まで遡ってcanonicalizeする。
fn normalize_absolute_path(absolute: &Path) -> PathBuf {
    // 最終パス全体が canonicalize できればそれを使う（シンボリックリンク解決 + .. 解決）
    if let Ok(resolved) = absolute.canonicalize() {
        return resolved;
    }

    // 存在する最も近い祖先まで遡って canonicalize し、残りを join する
    // これにより中間のシンボリックリンクが解決される
    let mut ancestor = absolute;
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
    resolve_dots(absolute)
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

    /// REV-002: 非UTF-8な path でも、認可対象が実際の path のバイト列を保持することを固定する。
    /// 旧実装は `check_path(&str)` へ渡す前に `to_str().unwrap_or("")` で非UTF-8 path を
    /// 空文字（≒CWD）へ潰していたため、認可対象と実際のI/O対象が分離していた。
    /// `&Path` を起点に正規化することで、非UTF-8 component が失われないことを検証する。
    #[cfg(unix)]
    #[test]
    fn non_utf8_path_is_authorized_as_itself() {
        use std::os::unix::ffi::OsStrExt;

        // 0x80 は単体では valid UTF-8 ではない。存在しない絶対 path を組み立てる。
        let raw = std::ffi::OsStr::from_bytes(b"/nonexistent-rev002/\x80/x.tsg");
        let path = Path::new(raw);

        // 前提: この path は UTF-8 化できない（旧経路なら "" へ潰れていた）。
        assert!(
            path.to_str().is_none(),
            "テスト用 path はUTF-8であってはならない"
        );

        // 正規化しても非UTF-8 component が保持され、空 path には潰れない。
        let normalized = normalize_path(path);
        assert!(
            !normalized.as_os_str().is_empty(),
            "正規化後の path が空になった（認可対象が実際の path と分離している）"
        );
        assert!(
            normalized.as_os_str().as_bytes().contains(&0x80),
            "正規化後の path が非UTF-8バイトを失った: {:?}",
            normalized
        );

        // check_path は認可した path（=正規化 path）をそのまま返し、I/O対象と一致させる。
        // sandbox 未設定時は allow-all なので、返り値は normalize_path と一致する。
        if allowed_paths().is_none() {
            let authorized = check_path(path, 1).expect("allow-all では認可される");
            assert_eq!(
                authorized, normalized,
                "check_path の返り値が正規化 path と一致しない"
            );
        }
    }
}
