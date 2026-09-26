//! REV-021（§17.6）: `remove_dir` は空 directory のみ、`remove_tree` は再帰削除。
//!
//! ambient（legacy）経路の観測挙動を、共有 handler（`builtin_core`）の直接呼び出しで固定する。
//! sandbox 未設定時は allow-all（fail-open）なので一時ディレクトリを操作できる。capability
//! 経路（`RecursiveDelete` / `EmptyDirectory` の認可分離・`directory_not_empty` host error）は
//! `src/vm.rs` の `#[cfg(unix)]` unit test で覆う。

use std::sync::atomic::{AtomicU64, Ordering};

use tsumugi::builtin_core::{builtin_remove_dir, builtin_remove_tree};
use tsumugi::value::Value;

/// 一意な一時ディレクトリを作る（後始末は best-effort）。
fn temp_dir(tag: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "tsumugi-rev021-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn str_arg(path: &std::path::Path) -> Value {
    Value::str_constant(path.to_string_lossy().to_string())
}

#[test]
fn remove_dir_deletes_empty_directory() {
    let base = temp_dir("empty");
    let target = base.join("empty_child");
    std::fs::create_dir(&target).unwrap();

    let result = builtin_remove_dir(&[str_arg(&target)], 1).expect("remove_dir");
    assert_eq!(result, Value::Bool(true));
    assert!(!target.exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn remove_dir_does_not_delete_non_empty_directory() {
    // REV-021: remove_dir は非空 directory を再帰削除しない（false を返し、中身は残る）。
    let base = temp_dir("nonempty");
    let target = base.join("full");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("f.txt"), b"x").unwrap();

    let result = builtin_remove_dir(&[str_arg(&target)], 1).expect("remove_dir");
    assert_eq!(result, Value::Bool(false), "非空 dir は削除されない");
    assert!(target.join("f.txt").exists(), "中身は残る");
    assert!(target.exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn remove_tree_deletes_recursively() {
    let base = temp_dir("tree");
    let target = base.join("tree");
    std::fs::create_dir_all(target.join("sub")).unwrap();
    std::fs::write(target.join("a.txt"), b"a").unwrap();
    std::fs::write(target.join("sub").join("b.txt"), b"b").unwrap();

    let result = builtin_remove_tree(&[str_arg(&target)], 1).expect("remove_tree");
    assert_eq!(result, Value::Bool(true));
    assert!(!target.exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
#[cfg(unix)]
fn remove_tree_on_symlink_removes_link_not_target() {
    // final symlink はリンク自体を削除し、リンク先ツリーを辿らない（§17.6）。
    let base = temp_dir("symlink");
    let real = base.join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("keep.txt"), b"keep").unwrap();
    let link = base.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let result = builtin_remove_tree(&[str_arg(&link)], 1).expect("remove_tree symlink");
    assert_eq!(result, Value::Bool(true));
    assert!(!link.exists(), "リンクは消える");
    assert!(real.join("keep.txt").exists(), "リンク先ツリーは残る");

    let _ = std::fs::remove_dir_all(&base);
}
