# Tsumugi — エージェント向け入口

Claude Code はこのファイルを、Kiro は `.kiro/steering/agent-entry.md` 経由で同じ内容を読む。
中身はこのファイルだけに書く（`AGENTS.md` は手元用のローカルルールで、リポジトリには含めない）。

## 作業を始める前に必ず読む

1. [`docs/documentation-guide.md`](docs/documentation-guide.md) — どの情報をどのドキュメントに書くか、実装スライスの運用
2. [`docs/slices.toml`](docs/slices.toml) — 実装スライスの一覧と状態の正本

「次に何をやるか」は roadmap ではなく `slices.toml` で決まる。`status = "ready"` のうち `order` が最小のスライスが次の作業である。
`ready` が無ければ実装には着手せず、そのことを報告する。

## 守ること

ワークスペース共通のルール（kanade の steering）が読めない環境でも、最低限これは守る。

- **spec が実装の正本。** spec に書かれていない判断が実装範囲・公開 API・データ構造・言語挙動に影響するなら、推測で埋めずに止まり、`needs-design` issue を立てる。
- **main へ直接 push しない。** ブランチを切って PR を出す。マージするのは人間（kamonabe）だけで、エージェントはマージしない。
- **`slices.toml` の状態は PR の中でだけ変える。** 遷移ごとに含めるものは documentation-guide の「実装スライスの運用」に従う。
- **PR の前に CI と同じゲートを通す。** `cargo fmt --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test`（Draft の間は省略してよい）。
