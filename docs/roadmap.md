# Tsumugi — ロードマップ

最終更新: 2026-10-20（F-1 fuel parity を実装）

> このロードマップは「進捗だけ」を追う。機能の詳細・採用/不採用の理由は各設計正本に置く
> （ドキュメントの役割分担は [ドキュメント運用ガイド](documentation-guide.md) を参照）。
> 状態の唯一の正本は下記「ステータス一覧」表であり、他の節・表はこの表を参照する。

## 現在地

- **フェーズ:** Phase 2（deny-by-default capability）・Phase 3/4（包括的な実行予算・協調実行）を並行して進行中。
- **現在の slice:** 決定性 Slice 1（規範的 primitive）＝Phase 5 の着手部分は実装完了（StableError を含む deterministic primitive が一通り揃った。VM parity・FakeHost gate・audit normalize は後続 slice）。
- **次のタスク:** 着手順 3（Phase 6 audit の残り）から slice を選定する（受入基準・変更箇所を持つ slice 定義は未作成）。正本は [決定性・実行時監査仕様](determinism-and-audit.md) §14 Slice 5。§4.1 の課金表（F-2）は未定義。
- **直近完了:** F-1 fuel parity（[実行予算・協調実行仕様](execution-control.md) §14「F-1 実装スライス」）。VM の fuel を tree と同じ課金点に揃え、停止性を fuel から独立した instruction watchdog へ移した（[次期意味論・実装決定](semantic-decisions.md) §17.7 改訂）。同じ script の成功・失敗と上限到達時の line が tree/VM で一致する。
- **直近完了（その前）:** legacy budget env（`TSUMUGI_MAX_*`）の CLI adapter 一本化（[実行予算・協調実行仕様](execution-control.md) §13）。tree file/stdin だけが env を無視していた tree/VM 差を解消し、library は env を読まなくなった。
- **直近完了（さらに前）:** audit drain queue byte 会計（§10.2、B-2）を tree backend で完了（per-execution 上限・opt-in。engine 全体 16 MiB は E10 統合後）。あわせて yield と host call が同じ実行で起きるときの §8 規則8 違反（B-1 からの既存不具合）を修正。詳細は [決定性・実行時監査仕様](determinism-and-audit.md) §14「B-2 実装スライス」。
- **直近完了（さらに前）:** `AuditBackpressure`-yield 配線（§10.2、B-1、sink-Pending 由来）を tree backend で完了。A-1 監査付き driver を facade 内の単一 execution poll driver へ移行し、sink の `Pending` 応答で実際に `Yielded(AuditBackpressure)` へ遷移・ack で resume、永続 Pending→`AuditFailure::BackpressureDeadline`（script Deadline より優先）。公開 `YieldReason::AuditBackpressure` / `AuditFailure::BackpressureDeadline` を追加（破壊的・additive、roadmap 明記で受容）、`language_revision` bump なし、backpressure なし実行は envelope 列不変（詳細は下記「完了（直近）」）。
- **直近完了（さらに前）:** E8b（capability profile/options の safe/legacy 移行）を完了。EMB-AT-10/11 を profile（safe）経由の subprocess named test で tree/VM 両 engine に固定した。`tests/cli_safe_profile.rs` に `safe_capability_deny_is_catchable_via_profile`（未 grant の exit/env/now を try/catch で catch でき、catch 後に後続処理が継続＝EMB-AT-10a/11c/11d）、`allow_exit_propagates_exit_code_via_profile`（`--allow-exit` + exit(0/42/255) → 実 process exit code 伝播＝EMB-AT-11a）、`allow_exit_out_of_range_maps_to_runtime_error_via_profile`（`--allow-exit` + exit(256)/exit(0 - 1) → RuntimeError 写像 exit 1・process は panic せず終了＝EMB-AT-11b）を追加。既存の tree 生 `CapabilitySet` 経由 unit test（`src/embedding.rs` の `c7_*`/`c3_*`）は outcome 種別レベルの正本として残す（profile 経由 subprocess test と補完関係）。受入 ID ↔ test の対応は [組み込みAPI仕様](embedding-api.md) 第16節 Phase 2 に明記。profile/options 配線（C9/C10/E7/E11）と migration warning の descope（§14.4）は先行完了済み。production コード変更なし（test と docs のみ）。
- **さらに前（E9 の一片）:** VM file/stdin の import・resolver 統合（E9/Phase 5 の一片）を shipped。VM file/stdin（`src/main.rs::execute_vm_with_path`）の import 解決を、tree file/stdin と同じ Engine resolver gate（`compile`→`link`、`LinkRequest` の `module_resolver` capability、`--allow-import-root NAME=PATH` で grant）へ統合した。link 層の capability gate を VM 経路へ前置し、未 grant + import は tree と同じく terminal `Denied`（exit 1、actionable 診断）、import 0 件は resolver 不問で通過、grant 時は従来どおり解決・実行。実際の import 展開（run 層）は tree file/stdin と同じく評価器内蔵 `ModuleLoader` のまま保全する（gate 専用に `Backend::TreeWalk` engine を build する最小統合で、VM backend adapter は新設しない）。VM REPL（`run_repl_vm`）は tree REPL（E7 の `link_for_repl`/`run_for_repl` gate 迂回保全）と対称に現状維持（観測挙動不変、その旨をコードコメントに明記）。追加/差し替えテスト: `tests/cli_safe_profile.rs::relative_import_vm_requires_import_root`（旧 `relative_import_vm_still_works_without_import_root` を挙動反転で置き換え）・`import_free_vm_runs_without_import_root`、`tests/differential.rs::vm_import_gate_matches_tree_file_stdin`（tree/VM 差分 parity）。既存 VM import 系（`tests/integration.rs` の import fixture・`import_count_limit_gates_file_execution_vm`・`import_outside_sandbox_is_blocked_in_both_engines` ほか）を gate 通過の `--allow-import-root` grant へ更新。**E9 は未完了**（VM の charge parity・StableError・FunctionId・transaction 等 conformance と experimental flag 除去が残る）。
- **さらに前:** tree REPL の Engine API 統合（E7 残）を shipped。tree REPL（`src/main.rs::run_repl`）を alpha facade 直叩きから Engine embedding API（`compile`→`link_for_repl`→`run_for_repl`、session で 1 つ保持する `EmbeddingContext` を再利用）へ載せ替えた。state 継続・import 解決・capability の submission 間持ち越し・エラー後継続・`exit()` 写像を観測挙動不変で達成。import 解決は従来どおり評価器内蔵 loader に委ねる内部 shim `link_for_repl`（resolver gate を迂回＝従来の CWD 相対 import・固定エラー文字列を保全）を使い、budget は session 累積・fuel のみ reset する内部 shim `run_for_repl` を使う（いずれも `#[doc(hidden)]` の crate 内部 shim で公開契約は増やさない。file/stdin の one-shot run は不変）。VM REPL は E9 まで現状維持。追加した統合テスト: `repl_state_continues_across_submissions_in_both_engines` / `repl_imported_names_resolve_across_submissions_in_both_engines` / `repl_capabilities_persist_across_submissions_in_both_engines` / `repl_continues_session_after_uncaught_error_in_both_engines`（いずれも tree/VM 両方）。既存 REPL テスト（`tree_repl_retries_failed_import`・`runtime_globals_resolve_across_repl_submissions`・cumulative budget 3 件ほか）が引き続き緑で観測挙動不変を担保。
- **さらに前（その前）:** CAP-AT-17（§19）の source budget N 境界受入試験を shipped。tree 経路（standard budget を in-process 注入）で source/import の本数が N-1/N は成功し、N+1 は chunk 蓄積前（Link フェーズの `charge_source`/`charge_import`、`exec_program` 到達前）に拒否されて 1 文も実行しないことを固定した。`tests/integration.rs::cap_at_17_source_count_boundary_tree` / `cap_at_17_import_count_boundary_tree`（tree in-process・observable）、`tests/differential.rs::cap_at_17_source_count_charge_parity`（同 import graph の tree/VM source/import 会計一致）、`src/eval.rs` の unit test 2 件（§7.2 優先順位どおり primary resource が `SourceCount` / `ImportCount`）で担保。production 変更なし（会計は既に蓄積前拒否で正しいことを確認）。あわせて `import_count_limit_gates_file_execution_vm` の Phase 3 TODO コメントを done 参照へ更新。
- **さらに前（さらにその前）:** 着手順 1 の Phase 6 audit への event 伝播が一区切り。`BudgetCharged`・host-call lifecycle の 3 event（`CapabilityDecision` / `HostCallStarted` / `HostCallFinished`）・yield/resume lifecycle の 2 event（`Yielded` / `Resumed`）が tree backend で emission 済みで、§7.2 の 8 event 型のうち 7 event が tree backend で出る。budget 本体は tree/VM 完了。着手順 1 に残るのは I/O/host budget の残りと、`AuditBackpressure`-yield 配線・VM 対応（別フェーズ）。REV-001 の全順序化（REV-017）は revision 0.21 で実装済み。状態・詳細は「ステータス一覧」の REV-001/REV-015/REV-017 と [実行予算・協調実行仕様](execution-control.md) を参照（決定性 Slice 1 の deterministic shared helpers・DeterminismRulesRevision/ModuleId 正規化・FunctionId＝AUD-048・StableError は実装完了済み）。

## マイルストーン（Phase 0〜7）

目標アーキテクチャへの移行順序。各 Phase の状態は粗い 1 記号で、Phase 内の個別項目の状態は下記「ステータス一覧」を参照する（ここでは再掲しない）。現行の step 上限・collection 上限・深度制限・filesystem/env allow-list・構造化エラー・差分テストは土台として再利用するが、それだけで各 Phase が完了したとはみなさない。

| Phase | 目的 | 設計正本 | 状態 | 完了 gate |
|---|---|---|---|---|
| 0 | 保証範囲と脅威モデル | [脅威モデル](threat-model.md) | 🟡 | 第11節 Phase 0、TM-AT-07・TM-AT-12（host 部分） |
| 1 | 安定した組み込み API | [組み込みAPI仕様](embedding-api.md) | 🟡 | 第14〜15節 E1〜E6・E8a、EMB-AT-01〜09・13・15〜17 |
| 2 | deny-by-default capability | [Capability Model仕様](capability-model.md)・[組み込みAPI仕様](embedding-api.md) | 🟡 | Capability 第18〜19節 C1〜C10・Phase 2 CAP-AT、Embedding E7〔tree REPL 統合済み〕・E8b〔完了：profile/options 配線済み・migration warning descope 済み・EMB-AT-10/11 を profile 経由 subprocess で固定済み〕。embedding 側 E7・E8b は完了、Phase 2 全体の状態は capability model 側 Phase 2 基準（CAP-AT 等）にも依存 |
| 3 | 包括的な実行予算 | [実行予算・協調実行仕様](execution-control.md) | 🟡 | 第3〜8・10・15.1〜15.3・16節、EMB-AT-21 |
| 4 | 協調実行と負荷制御 | [実行予算・協調実行仕様](execution-control.md) | 🟡 | 第6.2・9・11〜12・15.3〜15.5・16節、EMB-AT-22 |
| 5 | 規範意味論と決定的境界 | [決定性・実行時監査仕様](determinism-and-audit.md)・[次期意味論・実装決定](semantic-decisions.md) | 🟡 | Determinism 第2〜6・14節 Slice 1〜4/6・15.1/15.2/15.5・16節 |
| 6 | 実行時監査 | [決定性・実行時監査仕様](determinism-and-audit.md) | 🟡 | 第7〜12・14節 Slice 5・15.3/15.4・16節、EMB-AT-23・CAP-AT-30 |
| 7 | 運用保証と検証 | [検証・リリース・運用設計](verification-release-operations.md) | 🟡 | 第6節 matrix、第17節 VRO-AT-01〜15、第18節 |

> 長時間処理について「確実に終わる」とは、任意の script の成功を保証することではない。ホストを不安定にせず、完了・停止・失敗を観測可能な結果として扱い、有限の処理を設定された負荷の中で着実に進められることを目標とする。
>
> **OS 対応の保証レベル:** 最終的に OS 非依存を目指すが、Windows の機能完全性の保証は release（Phase 7）の対象とし、開発フェーズ（alpha、Phase 1〜6）では必須としない。ただし開発フェーズでも Windows CI による退行検出とセキュリティ境界（capability / secure resolution / sandbox）の Windows 健全性は維持する。正本は [検証・リリース・運用設計](verification-release-operations.md) §4.1.1。

## ステータス一覧（状態の唯一の正本）

未完了（🟡 部分実装 / ⬜ 未実装）と、矛盾解消のため明示する完了済み（✅）項目を AUD・REV 横断で並べる。ここが全項目の状態の唯一の正本であり、他の節・表はこの表を参照する。✅ 完了項目の実装詳細は各 owning spec を参照する。深層監査（AUD-001〜050）の大半は実装済みで、ここに載らない AUD は完了（✅）とみなしてよい。

| ID | 一行概要 | 状態 | owning spec |
|---|---|---|---|
| REV-001 | 共有 DAG の比較・表示・join・sort を有限化（指数化対策は完結、全順序化は REV-017 で実装済み・revision 0.21。残りは包括 budget 側の Phase 6 伝播で、うち `BudgetCharged`・host-call lifecycle 3 event・yield/resume 2 event emission は実装済み） | 🟡 | [実行予算・協調実行仕様](execution-control.md) |
| REV-015 | source/string/heap/I-O/bulk work の包括 budget・deadline・cancel（budget 本体は tree/VM 完了。fuel の課金点は F-1 で tree/VM を揃えた。§4.1 の課金表は未実装（F-2）、CAP-AT-17 の source/import 本数 N 境界受入試験も tree 経路で完了。残りは Phase 6 への伝播。うち `BudgetCharged`・host-call lifecycle 3 event・yield/resume 2 event（`Yielded` / `Resumed`）emission は tree backend で実装済み） | 🟡 | [実行予算・協調実行仕様](execution-control.md) |
| REV-007 | `ExecutionContext` の session state と run meter を request ごとに分離 | ⬜ | [実行予算・協調実行仕様](execution-control.md) |
| REV-008 | 全 stable execution へ transaction を適用（REPL は AUD-024 で完了） | 🟡 | [組み込みAPI仕様](embedding-api.md)・[実行予算・協調実行仕様](execution-control.md) |
| REV-010 | 32-bit target の `i64 as usize` wrap を `try_from`/checked へ | ⬜ | [実行予算・協調実行仕様](execution-control.md) |
| REV-011 | language revision・engine 差・status を機械可読な単一正本から生成 | ⬜ | — |
| REV-012 | call 評価順の status / doc drift 解消（意味論正本は第5節＝AUD-017） | 🟡 | [次期意味論・実装決定](semantic-decisions.md) §17.5 |
| REV-014 | sandbox/env/limits/stdio/clock を `EngineConfig`/`ExecutionRequest` へ移す | ⬜ | [Capability Model仕様](capability-model.md)・[組み込みAPI仕様](embedding-api.md) |
| REV-016 | script からの Rc cycle による回収不能 heap（baseline 課金・clear・tenant 分離） | ⬜ | [設計](design.md) |
| REV-017 | HumanDisplay/CanonicalRepr/TotalOrder の分離による sort/list_dir 全順序化（設計確定・実装済み、revision 0.21、REV-001 と連動） | ✅ | [次期意味論・実装決定](semantic-decisions.md) §17.8 |
| REV-019 | `now()` の epoch 前/clock error と `u64 as i64` を clock capability + checked で扱う | ⬜ | [Capability Model仕様](capability-model.md)・[実行予算・協調実行仕様](execution-control.md) |
| REV-022 | safe profile で denial と OS 失敗を区別（EOF/I-O error を Null/false へ畳まない） | ⬜ | [Capability Model仕様](capability-model.md)・[組み込みAPI仕様](embedding-api.md) |
| REV-024 | MSRV 固定・action pin・release/fuzz/stress gate の段階導入 | 🟡 | [検証・リリース・運用設計](verification-release-operations.md) |
| REV-025 | parse diagnostic 件数の上限 | ⬜ | [次期意味論・実装決定](semantic-decisions.md) |
| AUD-018 | CLI script 引数を `ExecutionRequest.arguments` snapshot へ（profile/options は E8b。profile/options 配線は C9/C10/E7/E11 で統合済み、migration warning は descope 済み、EMB-AT-10/11 は profile 経由 subprocess で固定済み——E8b 完了） | ✅ | [次期意味論・実装決定](semantic-decisions.md) §6・[組み込みAPI仕様](embedding-api.md) 第12・14〜16節 |
| AUD-020 | sandbox の TOCTOU/oracle/dangling symlink を path-handle で受入試験化 | 🟡 | [脅威モデル](threat-model.md) TM-002〜004・[Capability Model仕様](capability-model.md) §8・CAP-AT-10〜14 |
| AUD-021 | language-spec / LANG_GUIDE / design の drift 解消 | 🟡 | [言語仕様](language-spec.md)・[設計](design.md) |
| AUD-022 | REPL/differential/limit/defensive に加え fuzz・stress・failure injection gate | 🟡 | [検証・リリース・運用設計](verification-release-operations.md) §4〜6・17節 |
| AUD-036 | lossy 数値変換（Float→Int・file_size・exit は完了、`Exited` の `usage` field が残り） | 🟡 | [次期意味論・実装決定](semantic-decisions.md) §10 |
| AUD-045 | MSRV 1.97・stable/MSRV CI・install/release・artifact・署名/SBOM/OCI・運用 gate | 🟡 | [検証・リリース・運用設計](verification-release-operations.md) §3〜10・17〜18節 |
| AUD-048 | 動的評価ごとの per-instance `FunctionId` による関数等価性 | ✅ | [次期意味論・実装決定](semantic-decisions.md) §12・[決定性・実行時監査仕様](determinism-and-audit.md) §4.7 |
| Phase 5 残 | deterministic shared helpers（NaN canonicalization `0x7ff8_0000_0000_0000`・Float format）・DeterminismRulesRevision/ModuleId 正規化・StableError（`src/stable_error.rs`、tree 射影）は完了。Slice 1 の deterministic primitive は一通り揃った。残りは Slice 2〜6（HostBoundary/FakeHost・VM parity・audit normalize・record/replay 等） | 🟡 | [決定性・実行時監査仕様](determinism-and-audit.md) 第14節 Slice 1〜6 |
| Phase 6 残 | audit drain queue の engine 全体 16 MiB 上限（§10.2、E10 統合後。per-execution 上限は B-2 で実装済み）・redaction 本体・strict CBOR・record/replay・VM 対応・sink 必須化・`AdmissionQueued`/`SchedulerPreempted`/`HostPaused` の yield emission（engine-poll 経路に sink が無い）（A-1 ＋ `BudgetCharged` ＋ host-call lifecycle 3 event `CapabilityDecision` / `HostCallStarted` / `HostCallFinished` ＋ yield/resume 2 event `Yielded` / `Resumed` emission ＋ `AuditBackpressure`-yield 配線〔B-1、sink-Pending 由来〕＋ audit drain queue byte 会計〔B-2、per-execution〕は tree backend で実装済み） | 🟡 | [決定性・実行時監査仕様](determinism-and-audit.md) §6〜12・14 Slice 5 |
| 品質改善 | 未着手の処理改善（`dispatch` の match 分割・`exec_stmt` 分割・sandbox `OnceLock` テスタビリティ・fuzzing 導入） | ⬜ | [設計](design.md) |

## 着手順（次に何をやるか）

先頭ほど優先。各ステップの状態は上記「ステータス一覧」の対応 ID を参照する（ここでは再掲しない）。

1. **包括 budget の残り（REV-001/REV-015、Phase 3/4）** — 残 I/O/host budget と Phase 6 audit への budget/event 伝播。budget 本体は tree/VM 完了で、Phase 6 伝播のうち `BudgetCharged`・host-call lifecycle の 3 event（`CapabilityDecision` / `HostCallStarted` / `HostCallFinished`）・yield/resume lifecycle の 2 event（`Yielded` / `Resumed`）・`AuditBackpressure`-yield 配線（§10.2、B-1、sink-Pending 由来）は tree backend で実装済み。これで host-call + yield/resume + backpressure の audit 伝播は完了（tree-only・opt-in・非破壊）。B-2 の audit drain queue byte 会計（§10.2、per-execution）も実装済みで、残るのは VM 対応（別フェーズ）。REV-001 の全順序化（REV-017）は revision 0.21 で実装済み（§17.8）。正本: [実行予算・協調実行仕様](execution-control.md)、全順序化の設計は [次期意味論・実装決定](semantic-decisions.md) §17.8。
2. **Phase 1/2 の残り** — E8b（capability profile/options の safe/legacy 移行）は完了。profile/options 配線は C9/C10/E7/E11 で統合済み、migration warning は正式に descope 済み（理由は [Capability Model仕様](capability-model.md) §14.4）、EMB-AT-10/11 は profile 経由 subprocess test で固定済み（上記「直近完了」）。これで embedding 側の Phase 1/2（E7・E8b）が揃った。VM/E9 の import・resolver 統合（Phase 5）・tree REPL の Engine API 統合（E7 残）・CAP-AT-17 の budget N 境界（Phase 3/C11）は完了（上記「直近完了」系）。VM/E9 の import・resolver 統合は VM file/stdin の link 層 capability gate のみ done で、E9 の adapter/conformance（charge parity 他）・VM stable 化は別途残る（Phase 5／着手順 3 側）。Phase 2 全体の完了可否は capability model 側の Phase 2 基準（CAP-AT 等）にも依存する（「ステータス一覧」を正とする）。正本: [組み込みAPI仕様](embedding-api.md)・[Capability Model仕様](capability-model.md)。
3. **Phase 6 audit の残り** — audit drain queue の engine 全体上限（§10.2、E10 統合後）・redaction 本体・strict CBOR・record/replay・VM 対応・sink 必須化ほか（`Yielded` / `Resumed` emission ＋ `AuditBackpressure`-yield 配線〔B-1〕は shipped）。正本: [決定性・実行時監査仕様](determinism-and-audit.md)。
4. **Phase 7 運用保証** — MSRV・release/install・artifact・OCI・運用資材。正本: [検証・リリース・運用設計](verification-release-operations.md)。

> 完了（直近）: audit drain queue byte 会計（§10.2、B-2）を tree backend で shipped。詳細・逸脱・受入基準 B2-AT-01〜10 は [決定性・実行時監査仕様](determinism-and-audit.md) §14「B-2 実装スライス」を参照。
>
> 完了（その前）: `AuditBackpressure`-yield 配線（§10.2、B-1、sink-Pending 由来）を tree backend で shipped（[決定性・実行時監査仕様](determinism-and-audit.md) §10/§10.2・§14 A-1 スライス）。A-1 の監査付き driver を `run_transactional` の同期 loop から **facade 内の単一 execution poll driver**（poll ベース、設計 B-0 §2.3 案 2b）へ移行し、audit sink が batch を `AuditSubmit::Pending` にしたとき execution を実際に `Yielded(AuditBackpressure)` へ遷移させ、ack（同一 `Arc` batch の再送で Ack される）で `Resumed{previous_reason: AuditBackpressure}` へ resume する。backpressure は sink 応答由来で評価器の意味論（`SliceOutcome`）を跨がず、`Yielded`/`Resumed` の poll_index 発番・pair 共有は評価器の単一 `poll_index` カウンタに乗る（SliceFuel yield と同一規律、driver 独自カウンタなし）。永続 `Pending` のまま execution deadline へ達した場合は `AuditFailure::BackpressureDeadline` の terminal にし、script の `Deadline` より優先する（「実行結果は成功だが監査が欠ける」状態を作らない）。**実装差（公開 Rust API の破壊的変更、alpha・後方互換無保証につき roadmap 明記を条件に受容）**: `engine.rs::YieldReason` に公開 variant `AuditBackpressure` を追加（facade poll driver が返す。scheduler engine 側で返すのは E10 統合後）、`audit.rs::AuditFailure` に公開 variant `BackpressureDeadline` を追加、テスト facade `ScriptedResponse` に `PendingThenAck`/`PendingForever` を追加。`language_revision` bump なし（既存 `Yielded` tag 5 / `Resumed` tag 6 に既存 `AuditYieldReason::AuditBackpressure` を載せるだけで CBOR tag 表・charge table・script 観測値は不変）。backpressure が一度も起きない実行の envelope 列は移行前と完全一致（既存 `tests/audit_yield_resume.rs` / `tests/audit_core.rs` を無改変で緑のまま保つ回帰条件で固定）。opt-in・非破壊（sink なし経路 `run_guarded` は byte-for-byte 不変）、tree-only（VM+sink は build 拒否）。新規 `tests/audit_backpressure.rs` で Pending→Ack の Yielded/Resumed（nesting 対応の LIFO pair 整合・poll_index 単調）・単一 slice backpressure・永続 Pending→BackpressureDeadline・backpressure なし envelope 一致を固定。B-2 の audit drain queue byte 会計（§10.2、per-execution 1 MiB / engine 全体 16 MiB）は別スライスへ。
>
> 完了（直近）: yield/resume lifecycle の 2 event（`Yielded` tag 5 / `Resumed` tag 6）の emission を tree backend で shipped（[決定性・実行時監査仕様](determinism-and-audit.md) §7.2/§8 規則7/8/9・§14 A-1 スライス）。tree evaluator の slice 境界（`run_slice`）で `Yielded { reason, poll_index, usage }` と `Resumed { previous_reason, poll_index, usage }` を評価器にバッファし、A-1 の audited embedding facade が `BudgetCharged`・host-call triple と同じ deferred post-run drain で `ExecutionStarted` と `Terminal` の間に emit する。固定 drain 順は host-call triple → `Yielded`/`Resumed`（temporal 順）→ 最終 `BudgetCharged` flush → `Terminal`。実 yield を監査下で観測可能にするため `EngineBuilder::audit_slice_fuel` を追加し、小さい slice fuel で複数 slice に分けると文/反復境界で実 `Yielded(SliceFuelExhausted)` が起きる（既定＝未設定＝`u64::MAX` は従来挙動そのもの、sink なし経路は常に `u64::MAX` で非破壊）。§8 規則7（flush-before-Yielded）・規則8（Resumed-before-work）・規則9（Terminal last）を満たし、`poll_index` は execution ごとに 0 始まり単調（overflow fail-closed）で pair の `Yielded`/`Resumed` が同値を共有、`Resumed.previous_reason` は対の `Yielded.reason` と一致。yield-cause → `AuditYieldReason` 写像（`SliceFuelExhausted` 主経路／`HostCallPending`／`ExplicitYield`、および pause の `HostPaused`）を純粋関数で実装。`HostPaused` を公開 `YieldReason` へは追加しない。opt-in・非破壊（sink なし経路は従来どおり drain せず、観測結果・output byte は不変）、tree-only（VM は scheduler/pause 経路が無く Slice 6 へ延期）、新 event 型・charge table / revision の変更なし。`AdmissionQueued`/`SchedulerPreempted`/`HostPaused` の yield は engine-poll 経路（sink 無し）でのみ生じるため写像は定義するが A-1 facade では emission しない（§14 に明記）。これで §7.2 の 8 event 型のうち 7 event が tree backend で emission され、host-call + yield/resume の audit 伝播（着手順 1）が完了した。残る audit 伝播は `AuditBackpressure`-yield と VM 対応。unit test（写像 6 件）＋ integration test（`tests/audit_yield_resume.rs`、multi-slice 交互性・poll_index 単調・previous_reason 一致・規則7・非破壊性・既定 slice fuel で yield なし）で固定。
>
> 完了（過去）: host-call lifecycle の 3 event（`CapabilityDecision` / `HostCallStarted` / `HostCallFinished`、CBOR tag 1/2/3）の emission を tree backend で shipped（[決定性・実行時監査仕様](determinism-and-audit.md) §7.2/§8/§11・§14 A-1 スライス）。tree 唯一の host-call 境界である filesystem builtin（`read_file` / `read_lines` / `write_file` / `append_file`）で、`HostCallStarted` → `CapabilityDecision` → `HostCallFinished` の triple を評価器にバッファし、A-1 の audited embedding facade が `BudgetCharged` と同じ deferred post-run drain で `ExecutionStarted` と `Terminal` の間（host-call triple 群 → `BudgetCharged` の固定順）に emit する。opt-in・非破壊（sink なし経路は従来どおり drain せず、観測結果・output byte・書き込みファイル byte は不変）。operation_id/call_id は 0 始まり単調で triple 内再利用、deny 経路も Started + Finished（`Denied` / `effect_status=None`）を出す。§11 reserve は drain 時の容量チェック（`can_reserve_host_call`、Started 1 event/≤60 KiB ＋ close reserve 2 events/128 KiB）として実装。新 event 型・VM 配線・charge table / revision の変更なし。§8 規則5/6（ack-before-effect / ack-before-expose）と faithful な pre-effect reservation は retry/backpressure スライスへ延期（§14 に明記）。残る audit 伝播は `Yielded` / `Resumed`。
>
> 完了（過去）: Phase 6 への budget/event 伝播の最初の一片として、`BudgetCharged` audit event の emission を tree backend で shipped（[決定性・実行時監査仕様](determinism-and-audit.md) §7.2/§7.3/§8）。既存の `AuditEvent::BudgetCharged`（CBOR tag 4）を A-1 の audited embedding facade に配線し、`BudgetChargeAccumulator`（§7.3、per-(resource, reason) 集約・256-charge cut・決定的 flush 順）で集計、`ExecutionStarted` と `Terminal` の間（§8 rule 7）に drain して emit する。opt-in・非破壊（sink なし経路は従来どおり drain せず、観測結果・output byte は不変）。新 event 型・VM 配線・charge table / revision の変更なし。
>
> 完了（その前）: REV-017（全順序化）の実装が revision 0.21 で shipped（[次期意味論・実装決定](semantic-decisions.md) §17.8）。sort / `list_dir` を文字列表現比較から全値型（10 variant）を覆う単一の全順序 `TotalOrder`（数値は REV-003 の `NumericOrder` を再利用した厳密数値順、NaN は数値帯末尾、List/Dict は再帰辞書式）へ移した。`src/value.rs` に `TotalOrder`（純比較版と per-element 課金版）を新設、`LanguageRevision::V0_21`（`CURRENT`）を追加して sort/list_dir が revision で経路を分岐（0.20 以下は従来順を温存）、tree/VM に対称な `language_revision` を注入。unit test 12 件・golden fixture `total_order_sort`（tree/VM parity）・`tests/total_order_budget.rs` で固定、charge table は不変。language-spec.md の sort/list_dir 行と §17.1.9・§17.8.10 の drift / 実装状況を実装済みへ更新した。
>
> 完了（その前）: REV-017（全順序化）の設計を確定（[次期意味論・実装決定](semantic-decisions.md) §17.8）。sort / `list_dir` を文字列表現比較から全値型を覆う単一の全順序 `TotalOrder`（数値は REV-003 の `NumericOrder` を再利用した厳密数値順）へ移す破壊的変更を、採用判断・却下案・データモデル・tree/VM 変更箇所・budget 課金・error・移行・test・受入基準の 8 観点で設計確定した。この設計は上記のとおり revision 0.21 で実装された。
>
> 完了（その前）: 決定性 Slice 1 の残り＝StableError（`src/stable_error.rs`、tree 射影）を実装。これで Slice 1 の deterministic primitive（deterministic shared helpers・DeterminismRulesRevision/ModuleId 正規化・FunctionId＝AUD-048・StableError）は一通り揃った。VM parity・FakeHost gate・audit normalize は Slice 2〜6。

> 設計 slice に沿う詳細な実装順は各 spec（特に [次期意味論・実装決定](semantic-decisions.md) 第17節「移行順」と [実行予算・協調実行仕様](execution-control.md) 第14節 Slice 定義）を正本とする。

## 実装済み（完了マイルストーンの粗い一覧）

完了した update-image の粗い進捗一覧。各項目の実装詳細は owning spec を参照する。

- [x] 言語中核: 基本型・変数・演算・制御フロー・関数・第一級関数/lambda・クロージャ・List/Dict・index・f-string・try/catch・構造化エラー・module/import
- [x] 組み込み関数（print, len, push, pop, keys, type, slice, contains, split, join, to_int, to_str, range ほか）
- [x] ファイル I/O（read_file, read_lines, write_file, append_file）とサンドボックス
- [x] REPL（複数行入力）・行番号付きエラー・スタックトレース
- [x] バイトコード VM（Phase 0〜8）: OpCode/Chunk/Compiler/VM、変数、制御フロー、関数、クロージャ、組み込み関数 54 個、互換性修正、IEEE 754 統一
- [x] bytecode 検証・API 封印（REV-006/004/005/018）: `VerifiedChunk`/verifier、per-instruction step 課金、`MakeClosure(proto_index)`+`CaptureDesc`、`patch_jump` fallible 化、`unstable-bytecode` feature 封印
- [x] ステップ予算（ループ反復 + 関数呼び出しのカウント、無限ループ/再帰防止、tree/VM 両対応）→ 詳細は [実行予算・協調実行仕様](execution-control.md)
- [x] 深層監査 AUD-001〜050 の大半（REPL transaction・collection 上限・深度制限・canonical error・COW collection・FunctionId ほか）→ 状態は「ステータス一覧」、詳細は各 spec
- [x] Phase 6 実行時監査 A-1（schema v1・8 event 型・logical journal・`AuditSink`・fail-closed・opt-in、tree-only）＋ `BudgetCharged` emission ＋ host-call lifecycle 3 event emission（`CapabilityDecision` / `HostCallStarted` / `HostCallFinished`、§7.2/§8/§11）＋ yield/resume lifecycle 2 event emission（`Yielded` / `Resumed`、§7.2/§8 規則7/8/9、tree backend、opt-in・非破壊）→ 残件は「ステータス一覧」Phase 6 残

> 歴史的なベンチマーク測定値・検証スナップショットは [`docs/history/benchmarks.md`](history/benchmarks.md) に退避した。

## 将来機能（設計済み・未着手）

- **class（継承なし）:** 設計済み・低優先度。基盤と総 heap budget 後の実装候補 → [次期意味論・実装決定](semantic-decisions.md) 第15節
- **HTTP:** 設計済み・着手禁止（具体ユースケース承認待ち）。Phase 1〜6 完了と着手 gate 承認後だけ別計画を作る → [次期意味論・実装決定](semantic-decisions.md) 第16節
- **表示レイヤ i18n:** 現時点で見送り → [次期意味論・実装決定](semantic-decisions.md) §3.12

## 設計方針: 言語中核とホスト機能の境界

言語中核には外部状態へ触れない純粋な計算（文字列・数値・List/Dict・型変換）を置き、filesystem・環境変数・時刻・標準入出力・process・network・業務操作などの外部効果は、ホストが実行単位で明示的に付与する capability または host function として提供する。この境界の目標・現行実装からの移行表・採用理由（最小権限・資源制御・監査可能性・予測可能性・依存分離・テスト容易性）・外部機能の追加順序は [マニフェスト](manifesto.md) 原則 2〜8 と各 spec（[組み込みAPI仕様](embedding-api.md)・[Capability Model仕様](capability-model.md)・[実行予算・協調実行仕様](execution-control.md)・[決定性・実行時監査仕様](determinism-and-audit.md)）を正本とする。
