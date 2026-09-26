<!-- 生成物: このファイルは編集しないこと。 -->
<!-- src/builtin_registry.rs の PUBLIC_BUILTINS から生成する。 -->
<!-- 再生成: `cargo run --bin gen_builtins_doc`（または対応する生成コマンド）。 -->
<!-- 整合性は tests/builtin_registry_contract.rs が検証する（CAP-AT-20 / AUD-049）。 -->

# 組み込み関数リファレンス（生成物）

`src/builtin_registry.rs` の単一 `BuiltinSpec` registry（AUD-049 の正本）から生成した、
language から呼べる組み込み関数の一覧である。tree / VM / compiler と本ドキュメントは
同じ registry を source of truth とし、名前・arity・実行分類が一致する（CAP-AT-20）。

- **arity**: 受理する引数個数。`0 or 1` は 0 個または 1 個、`>=0` は可変長（最小 0 個）。
- **実行分類**: `pure-core` は評価済み引数から純粋に計算する共有実装、
  `context` は stdio・argv・変数束縛・closure 呼び出しなど実行コンテキストを要する。

| 名前 | arity | 実行分類 |
|---|---|---|
| `len` | 1 | pure-core |
| `push` | 2 | context |
| `pop` | 1 | context |
| `keys` | 1 | pure-core |
| `values` | 1 | pure-core |
| `has_key` | 2 | pure-core |
| `type` | 1 | pure-core |
| `slice` | 3 | pure-core |
| `contains` | 2 | pure-core |
| `sort` | 1 | pure-core |
| `reverse` | 1 | pure-core |
| `range` | 2 | pure-core |
| `split` | 2 | pure-core |
| `join` | 2 | pure-core |
| `trim` | 1 | pure-core |
| `upper` | 1 | pure-core |
| `lower` | 1 | pure-core |
| `starts_with` | 2 | pure-core |
| `ends_with` | 2 | pure-core |
| `replace` | 3 | pure-core |
| `to_int` | 1 | pure-core |
| `to_str` | 1 | pure-core |
| `to_float` | 1 | pure-core |
| `abs` | 1 | pure-core |
| `min` | 2 | pure-core |
| `max` | 2 | pure-core |
| `floor` | 1 | pure-core |
| `ceil` | 1 | pure-core |
| `round` | 1 | pure-core |
| `now` | 0 | context |
| `format_time` | 2 | pure-core |
| `print` | 0 | context |
| `input` | 0 | context |
| `args` | 0 | context |
| `exit` | 0 or 1 | context |
| `map` | 2 | context |
| `filter` | 2 | context |
| `each` | 2 | context |
| `read_file` | 1 | pure-core |
| `read_lines` | 1 | pure-core |
| `write_file` | 2 | pure-core |
| `append_file` | 2 | pure-core |
| `env` | 1 | context |
| `path_exists` | 1 | pure-core |
| `path_join` | >=0 | pure-core |
| `mkdir` | 1 | pure-core |
| `remove` | 1 | pure-core |
| `remove_dir` | 1 | pure-core |
| `remove_tree` | 1 | pure-core |
| `rename` | 2 | pure-core |
| `list_dir` | 1 | pure-core |
| `file_size` | 1 | pure-core |
| `is_file` | 1 | pure-core |
| `is_dir` | 1 | pure-core |

合計 54 個（pure-core 43 個・context 11 個）。
