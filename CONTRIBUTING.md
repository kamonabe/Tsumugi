# コントリビューションガイド

Tsumugi への貢献に興味を持ってくれてありがとう。このドキュメントは、変更を提案するときに知っておくべき原則・手順・検証をまとめる。

Tsumugi は教育・実験用途の **alpha 版**であり、言語仕様・組み込みAPI・CLI の後方互換性は保証していない。この段階では、設計の一貫性と検証の再現性を特に重視する。

## はじめに: 設計ドキュメントが正本

Tsumugi では **`docs/` の設計ドキュメントが正本**であり、実装はそれに従う。

- 実装を変える前に、`docs/` の該当仕様を確認する
- 仕様の変更を伴う変更では、コードと同じ PR でドキュメントも更新する
- 迷ったら次を参照する
  - [Tsumugi Manifesto](docs/manifesto.md) — 価値基準・設計原則・非目標
  - [設計ドキュメント](docs/design.md) — 現行実装のアーキテクチャ
  - [言語仕様](docs/language-spec.md) — 現行実装の観測仕様（規範）
  - [ロードマップ](docs/roadmap.md) — 実装状態・進捗・実装差の一覧

ホストの安定性を実行速度より優先する、というマニフェストの原則を全体の指針とする。

## 開発環境

- 言語: Rust（edition 2024）。MSRV は `Cargo.toml` の `rust-version` を参照する（意図せず上げる変更は禁止）
- ベンチ: Criterion 0.5（`benches/interpreter.rs`）

```bash
# ビルド
cargo build

# ファイル実行（ツリーウォーク）
cargo run -- examples/hello.tsg

# バイトコードVMモードで実行
cargo run -- --vm examples/hello.tsg

# REPL
cargo run
```

## 変更を出すまでの流れ

Git 運用は次のとおり。`main` は保護されており、直接 push はできない。

```bash
# 1. main を最新化
git checkout main && git pull

# 2. ブランチを切る（prefix は下記のコミット種別に準じる）
git checkout -b <prefix>/<短い説明>

# 3. 変更してコミット
git add <files>
git commit -m "<type>: <summary>"

# 4. push して PR を作成
git push -u origin <ブランチ名>
```

### コミットメッセージ規約

| prefix | 用途 |
|---|---|
| `feat:` | 新機能 |
| `fix:` | バグ修正 |
| `refactor:` | リファクタリング |
| `docs:` | ドキュメント変更 |
| `style:` | フォーマット修正（機能変更なし） |
| `ci:` | CI/CD 設定変更 |
| `deps:` | 依存関係更新 |

## 検証（変更後は必ず実行）

CI と同じゲートをローカルでも通してから PR を出す。

```bash
cargo fmt --check              # フォーマット（ローカル整形は cargo fmt）
cargo clippy -- -D warnings    # lint。warning もエラー扱い
cargo test                     # テスト
```

補足:

- CI の clippy はデフォルトターゲットだけを見る。テストやベンチも含めて検査するなら、ローカルで `cargo clippy --all-targets -- -D warnings` を実行する
- 資源の限られた環境では並列度を抑える: `cargo test -j 1 -- --test-threads=1`
- カバレッジは CI で `cargo llvm-cov` を実行する（ローカル必須ではない）
- テストは ubuntu / macos / windows のマトリクスで CI 実行される。パス区切り・改行など OS 依存の挙動に注意する

## 2つの実行系（tree-walk / VM）

Tsumugi は Lexer・Parser・AST を共有し、2つの実行系を持つ。

- デフォルト: ツリーウォーク評価器（**規範 backend**）
- `--vm`: バイトコードコンパイラ + スタック VM（実験的 backend、デフォルトとの既知の差異あり）

言語の挙動を変える変更では、**tree-walk と VM の両方で確認する**。両者の差異を新たに増やす変更は慎重に判断し、[ロードマップ](docs/roadmap.md)の実装差一覧との整合を確認する。

## テスト方針

- 新機能・バグ修正には統合テスト（`tests/`）またはユニットテストを添える
- fixture ベースのテストは `tests/fixtures/` に `.tsg` と期待出力を置き、`fixture_tests!` へ宣言すると tree-walk 版 / VM 版の両方が生成される
- テスト構成の詳細は [README](README.md) の「テスト」節と `docs/design.md` を参照する

## セキュリティ

脆弱性の報告は Issue ではなく Private Vulnerability Reporting を使う。詳細と保証境界は [SECURITY.md](SECURITY.md) を参照する。

## PR チェックリスト

PR を出す前に次を確認する（PR テンプレートにも同じ項目がある）。

- [ ] `cargo fmt --check` / `cargo clippy -- -D warnings` / `cargo test` が通る
- [ ] 言語挙動を変える場合、tree-walk と VM の両方で確認した
- [ ] 仕様変更を伴う場合、`docs/` の該当ドキュメントを更新した
- [ ] 新機能・バグ修正にテストを添えた
