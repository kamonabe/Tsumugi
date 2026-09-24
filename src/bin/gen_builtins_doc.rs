//! 組み込み関数リファレンス（生成物）の再生成ツール（CAP-AT-20 / AUD-049）。
//!
//! 単一 `BuiltinSpec` registry（`src/builtin_registry.rs` の `PUBLIC_BUILTINS`）から
//! `docs/generated/builtins.md` を生成する。registry へ builtin を追加・変更したら
//! 本ツールを実行して生成物を更新する。生成物と registry の byte 一致は
//! `tests/builtin_registry_contract.rs` が検証するため、更新漏れは CI で検出される。
//!
//! 使い方:
//!
//! ```text
//! cargo run --bin gen_builtins_doc
//! ```

use std::fs;
use std::path::Path;

fn main() -> std::io::Result<()> {
    let rendered = tsumugi::builtin_registry::render_reference();
    let out_path = Path::new("docs/generated/builtins.md");
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(out_path, rendered)?;
    println!("生成しました: {}", out_path.display());
    Ok(())
}
