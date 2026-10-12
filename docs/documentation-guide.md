# Tsumugi — ドキュメント運用ガイド

このガイドは「どの情報をどのドキュメントに書くか」を定める。仕様駆動開発であり、設計正本（spec doc）が実装の正本である。各ドキュメントの役割を固定することで、情報の重複・drift・置き場所の誤りを防ぐ。

AI エージェント・人間のどちらも、新しい情報を書く前にまず「その情報はどの種別か」を判断し、下表の owning document へ書くこと。

## 大原則

- **仕様駆動開発であり、spec doc が実装の正本。** 実装と spec が食い違う場合は spec を正とし、spec を更新してから実装を直す。
- **ロードマップは spec を要約しない。進捗だけを追う。** 機能の詳細・採用/不採用の理由は spec に書く。ロードマップに detail を書き始めたら、それは置き場所の誤りである。
- **状態（✅/🟡/⬜）の唯一の正本は [ロードマップ](roadmap.md) の「ステータス一覧」表。** 他の節・表・ドキュメントは状態を再掲せず、その表を参照する。同じ事項の状態を複数箇所に書くと食い違いの原因になる。

## 情報種別 → owning document

| 情報種別 | owning document | ロードマップに書くか |
|---|---|---|
| プロジェクトの価値基準・非目標・設計原則 | [マニフェスト](manifesto.md) | ✕（link のみ） |
| 言語構文・組み込み関数・言語挙動の規範 | [言語仕様](language-spec.md) | ✕ |
| 全体アーキテクチャ・モジュール構成・設計トレードオフ | [設計](design.md) | ✕ |
| 埋め込み API（Engine 系・terminal outcome・exit code） | [組み込みAPI仕様](embedding-api.md) | ✕ |
| 実行予算・協調実行・transaction・scheduler | [実行予算・協調実行仕様](execution-control.md) | ✕ |
| capability モデル・adapter・filesystem・host function | [Capability Model仕様](capability-model.md) | ✕ |
| 決定性・監査・StableError・record/replay | [決定性・実行時監査仕様](determinism-and-audit.md) | ✕ |
| 脅威モデル・security boundary・TOCTOU | [脅威モデル](threat-model.md) | ✕ |
| 次期言語挙動・採用/却下理由・class/HTTP/i18n 等の設計判断 | [次期意味論・実装決定](semantic-decisions.md) | ✕ |
| 検証・リリース・MSRV・運用 gate | [検証・リリース・運用設計](verification-release-operations.md) | ✕ |
| 詳細レビュー報告書（原本） | [`semantic-review/Tsumugi-detailed-review-20260907.md`](../semantic-review/Tsumugi-detailed-review-20260907.md) | ✕（link のみ） |
| 歴史的測定値・ベンチマーク・検証スナップショット | [`docs/history/`](history/) | ✕ |
| **どの update-image（マイルストーン）が存在し、どこまで終わり、何が残るか（進捗のみ）** | **[ロードマップ](roadmap.md)** | ◯（唯一の役割） |

## ロードマップと spec の責務分離

ロードマップは「今どのフェーズにいて、次に何をやるか」と「各項目の状態」だけを管理する。具体的には次を持つ。

- 現在地（フェーズ・現在の slice・次の単一タスク）を数行で。
- マイルストーン（Phase 0〜7）の目的・設計正本 link・状態・完了 gate。
- 状態の唯一の正本である「ステータス一覧」表（ID・一行概要・状態・owning spec）。
- 着手順（タスク名・状態参照・spec link）。

ロードマップは次を持たない。これらは spec doc の役割である。

- 機能の実装詳細（型名・PR 単位の変更内容・コミット単位の説明）。
- 設計判断の根拠（採用理由・却下した案・トレードオフ）。
- 受入基準の本文（gate の名前は参照してよいが、条件の全文は spec に置く）。

新しく仕様を書くときは、まず上表で owning document を特定し、そのドキュメントの既存の節構成・prose スタイルに合わせて追記する。ロードマップ側は「状態」と「spec への link」だけを更新する。
