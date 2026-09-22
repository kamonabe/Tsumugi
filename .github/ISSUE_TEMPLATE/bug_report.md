---
name: バグ報告
about: 想定と異なる挙動を報告する
title: ''
labels: bug
assignees: ''
---

<!--
セキュリティ上の脆弱性は、この Issue ではなく Private Vulnerability Reporting で報告してください。
詳細は SECURITY.md を参照。
-->

## 概要

<!-- 何が起きたかを簡潔に -->

## 実行系

<!-- 該当するものにチェック -->

- [ ] ツリーウォーク（デフォルト）
- [ ] バイトコード VM（`--vm`）
- [ ] 埋め込み API（`Engine` / `ExecutionContext`）
- [ ] わからない

## 再現手順

<!-- 可能なら再現用の最小スクリプト（.tsg）またはコマンドを貼る -->

```
# 例: cargo run -- repro.tsg
```

```
# repro.tsg の内容
```

## 期待される挙動

<!-- どうなるべきか -->

## 実際の挙動

<!-- 実際に何が起きたか。エラーメッセージやスタックトレースがあれば貼る -->

## 環境

- OS:
- Rust バージョン（`rustc --version`）:
- Tsumugi のバージョン / コミット:

## 補足

<!-- 関連する docs/ のドキュメントや脅威モデルの TM-ID があれば -->
