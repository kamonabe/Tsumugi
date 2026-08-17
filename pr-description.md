## Summary

`Rc<Chunk>` 化とビルトイン共通モジュール(`builtin_core`)導入の2点をまとめて実施。

## Changes

### Rc\<Chunk\> 化

- `Value::VmFn` の `chunk` フィールドを `Chunk` → `Rc<Chunk>` に変更
- `CallFrame` も `Rc<Chunk>` を保持
- 関数呼び出し(`Call`)・クロージャ生成(`MakeClosure`)・`map`/`filter`/`each` でのクローンがポインタコピーのみに

### ビルトイン共通化

- `src/builtin_core.rs` を新設し、VM/ツリーウォーク共通のビルトインロジック(~45関数)を集約
- `vm.rs` の `exec_builtin` を `builtin_core::dispatch` への委譲 + VM固有処理に簡略化
- `builtin.rs` をコンテキスト依存のビルトイン(`push`/`pop`/`map`/`filter`/`each`/`print`/`input`/`exit`/`args`)のみに縮小
- 重複していた `format_unix_timestamp`/`is_leap_year` を一本化

### ドキュメント

- `docs/design.md` に設計判断(Rc採用理由、builtin_core の方針)を追記

## Stats

- 6 files changed, +815 / -1902 (約1100行の純減)

## What to verify

- `cargo test` で VM/ツリーウォーク両方のテストがグリーンか確認
- 特に `map`/`filter`/`each` + クロージャの組み合わせ（`Rc<Chunk>` の共有が正しく動くか）
