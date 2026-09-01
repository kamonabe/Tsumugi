# Tsumugi — 検証・リリース・運用設計

最終更新: 2026-08-31
設計ステータス: **実装仕様確定・未実装**

## 1. 目的と適用範囲

本文書は、[ロードマップ](roadmap.md) Phase 7「運用保証と検証」、AUD-022「REPL・differential・limit境界・defensive VMテスト」、AUD-045「配布・実行手順とtoolchainの下限」を完了させるため、検証、配布、supply chain、OCI image、参照用Kubernetes Job、rollback、deprecation、runbookの実装契約を定める。

本文書の作成時点では、現行CIのfmt・Clippy・3 OS test・coverage artifact、golden test、timeout、scaling test、defensive VM testだけが実装済みである。本文書が追加するMSRV gate、release workflow、fuzz/stress、artifact署名、OCI image、Kubernetes manifest、運用SLOは**すべて未実装**であり、現在の保証として扱わない。

本文書では次を決定済みとし、各実装sliceで再選択しない。

- MSRVはRust 1.97とし、`Cargo.toml`へ`rust-version = "1.97"`を宣言する。
- PR CIはrolling stableとMSRV 1.97の両方を検証する。
- Cargo package version、言語仕様revision、Git tag、binary artifact、OCI tagの対応関係を固定する。
- release artifactはLinux / macOS / Windowsのx86_64 / aarch64を対象とする。
- Phase 7のrelease gateにfuzz、stress、資源制約、failure injection、capability、budget、audit、backend conformanceを含める。
- OCI imageはCLI配布形式でありserviceではない。Helm chartは作らない。
- Kubernetes資材はOCI image公開後だけ作成し、参照用のone-shot Jobとdeny-all NetworkPolicyに限定する。
- Kustomize導入はmanifest作成時の別承認事項とし、本文書だけでは作成しない。

### 1.1 本文書で作成しないもの

本文書の追加と同時に、コード、test、workflow、Dockerfile、OCI image、Kubernetes manifest、Kustomize資材、Helm chartは作成しない。特に、存在しないimageを参照する`deploy/kubernetes/smoke-job.yaml`や`networkpolicy-deny-all.yaml`を先行作成してはならない。

### 1.2 規範語

- **必須**: releaseまたはPhase完了に必要で、省略不可。
- **禁止**: 実装または運用で採用してはならない。
- **任意**: 既定動作を変えず、追加してもrelease互換性を壊さないもの。

## 2. 正本、状態、責務境界

### 2.1 正本の優先順位

| 順位 | 正本 | 本文書との関係 |
|---:|---|---|
| 1 | [Tsumugi Manifesto](manifesto.md) | ホスト安定性、明示的capability、有限budget、決定性、audit、OS隔離の価値基準 |
| 2 | [言語仕様](language-spec.md) | scriptから観測可能な規範意味論と言語revision |
| 3 | [Capability仕様](capability-model.md)、[実行予算・協調実行仕様](execution-control.md)、[決定性・監査仕様](determinism-and-audit.md)、[組み込みAPI仕様](embedding-api.md)、[脅威モデル](threat-model.md) | Phase 1〜6の公開型、不変条件、受入基準 |
| 4 | 本文書 | Phase 7、release、配布、運用のgateと手順 |
| 5 | [ロードマップ](roadmap.md) | PhaseとAUD項目の実装状態 |
| 6 | `README.md`、`.github/workflows/*.yml`、`Cargo.toml` | 利用者向け手順と実装された設定 |

本文書は言語意味論、capability、budget、audit schemaを再定義しない。矛盾時は上位の正本を修正してから本文書を追随させる。

### 2.2 現行と目標の分離

| 領域 | 現行 | 本文書の完了状態 |
|---|---|---|
| toolchain | rolling stable、`rust-version`なし | stable + Rust 1.97、`rust-version = "1.97"` |
| Clippy | default target | `--all-targets --all-features -D warnings` |
| test | stable、3 OS | stable 3 OS + MSRV + release artifact smoke |
| coverage | LCOV artifactのみ | line coverage 80%以上をgate化 |
| fuzz/stress | 未実装 | PR smoke + weekly + release candidate gate |
| release | workflow・artifactなし | 6 platform artifact、SHA-256、SBOM、provenance、署名 |
| OCI | Dockerfile・imageなし | amd64/arm64 image、nonroot、署名・SBOM |
| Kubernetes | 資材なし | image公開後に参照用Jobとdeny-all NetworkPolicy |
| capability/budget/audit | 現行のstep・collection・深度上限、差分検証、構造化errorは部分実装済み。Phase 2〜6の完全な受入契約は未実装 | 各正本の受入基準をPhase 7 gateで再検証 |
| backend | tree規範、VM experimental | 差分0のgateを満たさない限りVM experimentalを維持 |

## 3. バージョンと互換性

### 3.1 二つのversion軸

Tsumugiは次のversionを独立管理する。

1. **Cargo package version**: `Cargo.toml`の`package.version`。実装、CLI、embedding API、配布物全体へSemVerを適用する。
2. **言語仕様revision**: `docs/language-spec.md`のrevision。scriptから観測可能な意味論の規範版でありSemVerではない。

関係を次で固定する。

- 1つのpackage releaseは、必ず1つの言語仕様revisionをrelease metadataと`ExecutionStarted` metadataへ記録する。
- packageだけの修正では言語revisionを変更しなくてよく、複数package releaseが同じ言語revisionを実装してよい。
- scriptから観測可能な意味論を変更する場合は、先に言語仕様とrevisionを更新し、そのrevisionを実装するpackage versionを上げる。
- 言語revisionの増加だけではCargo package releaseを意味しない。
- release noteには`Package version`と`Language revision`を別項目で必ず記載する。
- package `0.x`の間もSemVerを使う。breakingな公開API・CLI・言語変更はminor、後方互換な修正はpatch、release candidateは`-rc.N`とする。
- package `1.0.0`以降は通常のSemVer互換性規則を適用する。

### 3.2 MSRV

MSRVは**Rust 1.97**とする。

```toml
[package]
rust-version = "1.97"
```

運用規則:

- CIのMSRV jobは`1.97.0`を明示installし、`cargo test --all-features --locked`を実行する。
- stable jobはrolling stableを使い、将来のcompilerでの退行を検出する。
- MSRVを意図せず上げるdependency updateは禁止する。`Cargo.lock`更新PRはMSRV jobを必須とする。
- MSRV引き上げはpackage `0.x`ではminor、`1.x`以降ではmajorでのみ行い、release noteのBreaking Changesへ理由、移行先、旧MSRVで利用可能な最終versionを記載する。
- security修正が旧MSRVで実装不能な場合だけ例外を許し、security advisoryとrelease noteで明示する。

### 3.3 tagとrelease名

- Git tag: `v<semver>`、例`v0.2.0`、`v0.2.0-rc.1`。
- GitHub Release title: `Tsumugi v<semver>`。
- OCI tag: `ghcr.io/kamonabe/tsumugi:<semver>`。先頭の`v`を付けない。
- `latest`、branch名、日付だけのtagを配布・manifest・runbookで使用しない。
- tag、GitHub Release asset、OCI manifestはimmutableとし、同じversionへ異なるbytesを再公開しない。
- tagのversion、`Cargo.toml`のversion、artifact名、OCI label `org.opencontainers.image.version`は完全一致させる。

## 4. CI設計

### 4.1 workflow構成

PRと`main` pushでは次を必須checkとする。release tag workflowは同じcheckを再実行し、PR結果だけを流用しない。

| Job | OS / toolchain | 必須コマンド・判定 |
|---|---|---|
| `fmt` | Ubuntu / stable | `cargo fmt --check` |
| `clippy` | Ubuntu / stable | `cargo clippy --all-targets --all-features -- -D warnings` |
| `test` | Ubuntu, macOS, Windows / stable | `cargo test --all-features --locked`、`fail-fast: false` |
| `msrv` | Ubuntu / 1.97.0 | `cargo test --all-features --locked` |
| `coverage` | Ubuntu / stable | `cargo llvm-cov --all-features --workspace --fail-under-lines 80 --lcov --output-path lcov.info` |
| `docs` | Ubuntu / stable | `cargo doc --all-features --no-deps`、local link、version・件数・既知差分drift検査 |
| `manifest` | Ubuntu / stable | Cargo metadata/package、workflow、将来のOCI/Kubernetes manifest検証 |
| `fuzz-smoke` | Ubuntu / nightly | lexer/parser/engine/vmを各60秒、RSS 512 MiB上限で実行 |

`coverage`はline coverage **80%以上**をrelease gateとする。過去のスナップショット値をbaselineとして固定せず、同じcommandの当該commit結果を判定する。生成物、third-party code、fuzz corpusはcoverage対象外にできるが、手書きのproduction moduleを除外してはならない。coverage低下を回避するための到達不能コード追加や`cfg`除外は禁止する。

### 4.2 docs linkとdrift

`docs` jobは最低限次を検証する。

- `README.md`、`LANG_GUIDE.md`、`docs/**/*.md`の相対linkとanchorが存在する。
- `Cargo.toml`のpackage versionとrelease tagが一致する。
- `README.md`に記載するpackage versionと言語revisionが各正本と一致する。
- builtin catalog、tree、VM、compiler、生成文書のbuiltin名・arity・metadataが一致する。
- 既知backend差のAUD IDが`language-spec.md`と`roadmap.md`で一致する。
- `README.md`のCI job・ローカルcommand・artifact名がworkflowと一致する。
- release noteのpackage version、language revision、MSRVがrelease metadataと一致する。

local Markdown linkはnetworkなしで検証し、外部URLは週次jobで検証する。外部site障害はPRを即時blockせず、2週連続失敗または恒久的404を修正対象とする。相対link・anchor失敗は常にPRをblockする。

### 4.3 manifest validation

`manifest` jobは段階に応じて次を実行する。

```bash
cargo metadata --locked --no-deps --format-version 1
cargo package --locked
cargo package --locked --list
```

- `.github/workflows/*.yml`は`actionlint`で検証する。
- `Cargo.toml`と`Cargo.lock`は`cargo metadata`および`cargo package`成功を必須とする。
- Dockerfile作成後はBuildKitの構文check、両platform build、container smokeを追加する。
- Kubernetes manifest作成後は本書10.1の`kubeconform`とserver-side dry-runを追加する。
- release assetの許可list外ファイル、secret、`target/`、fuzz crash artifactを`cargo package --list`へ含めない。

### 4.4 GitHub Actions SHA pin

- 全`uses:`は40桁commit SHAへ固定し、同じ行のcommentにupstream tagを記載する。
- `@v4`、`@main`、branch、mutable tagだけの参照は禁止する。
- Action更新は自動PRで行い、変更元repository、release note、SHAとtagの対応をreviewする。
- third-party Actionが不要な処理はshell commandまたは公式Actionを優先する。
- fork PRへwrite permission、OIDC token、package write、release writeを付与しない。
- provenance、署名、GHCR pushに必要な`id-token: write`と`packages: write`はprotected tagのrelease jobだけにjob単位で付与する。

### 4.5 CI artifact retention

| Artifact | Retention |
|---|---:|
| `lcov.info` | 30日 |
| PR fuzz smoke failure | 30日 |
| weekly fuzz/stress failure | 90日 |
| release candidate binary | 30日 |
| published GitHub Release asset | release存続中 |
| SBOM / provenance / signature | 対象releaseと同期間 |

## 5. Phase 7検証戦略

### 5.1 テスト階層

| 階層 | 実行契機 | 目的 |
|---|---|---|
| unit / integration / golden | 全PR | 規範意味論、error、REPL、現行回帰 |
| defensive / scaling | 全PR | host panic防止、計算量・解放漏れ |
| fuzz smoke | 全PR | 明白なpanic、hang、memory blow-upの早期検出 |
| capability / budget / audit matrix | 該当Phase変更の全PR | Phase 2〜6契約の完全性 |
| resource-constrained | main、release candidate | 低資源下の完了・停止・失敗の観測 |
| weekly fuzz / stress / failure injection | 週次schedule | 長時間・競合・稀な入力の検出 |
| release conformance | release tag前 | 全backend、artifact、OCI、運用gate |

wall-clockに敏感なstressは通常のunit testへ混在させない。決定可能な性質はgolden、logical counter、allocation byte、fake clock、barrierで検証する。

### 5.2 fuzz target

将来の`fuzz/`は次の4 targetを持つ。target名を`lexer`、`parser`、`engine`、`vm`に固定する。

| Target | 入力 | Oracle |
|---|---|---|
| `lexer` | 任意byte列をUTF-8 lossless/invalid caseに分類したsource | panic、abort、stack overflow、timeoutなし。token列または構造化lex/parse error |
| `parser` | lexer出力とgrammar dictionaryで変異したsource | panicなし。成功ASTは深度・span不変条件を満たし、失敗は構造化error |
| `engine` | fixture由来・生成source、deterministic `ExecutionContext`、有限budget | terminalまたは構造化compile/link error。ambient OS accessなし。同じseedとDeterminismInputで同じ結果 |
| `vm` | sourceからcompileしたChunkと公開APIへ渡す破損Chunk | host panicなし。正当Chunkはtreeとのconformance oracle、破損Chunkは構造化internal error |

#### Corpusとseed

- initial corpusは`tests/fixtures/`、parser/REPL/limit regression、過去の最小failureから生成する。
- random seedは実行ごとに記録し、CI failure summaryとartifact名へ含める。
- grammar dictionaryは予約語、operator、delimiter、f-string、import、try/catch、function、collection、path literalを含む`fuzz/dictionaries/tsumugi.dict`に固定する。
- confidential source、環境変数、filesystem内容をcorpusやartifactへ入れない。
- tree/VM差分oracleは既知非適合を無条件allow-listにしない。未解消差分はAUD ID、最小入力、期限を持つ明示的expectationとし、VM production gateは差分1件でも失敗させる。

#### 時間とmemory

| 実行 | 各target時間 | RSS上限 | 1入力timeout |
|---|---:|---:|---:|
| PR smoke | 60秒 | 512 MiB | 10秒 |
| weekly | 15分 | 1 GiB | 10秒 |
| release candidate | 30分 | 1 GiB | 10秒 |

- Linux isolated runner上で実行し、runner全体のjob timeoutはtarget合計時間の2倍に固定する。
- sanitizerがOOMする前にRSS上限で停止させ、OOM自体を成功扱いしない。
- timeout、OOM、panic、abort、backend差、非決定結果はすべてfailureである。

#### Minimizeとartifact

1. failure inputを自動保存する。
2. `cargo fuzz tmin <target> <artifact>`相当で最小化する。
3. 最小入力を同じcommit・seed・targetで3回再現する。
4. `fuzz/artifacts/<target>/<UTC日付>-<commit SHA>/`をCI artifactとして保存する。
5. 修正時は最小入力をcorpusへ加え、可能ならunit/golden/defensive testにも昇格する。
6. secret混入scan後だけartifactを共有する。外部へ自動公開しない。

### 5.3 stress、資源制約、failure injection

#### 資源制約profile

resource-constrained jobはLinux container/cgroupで次を固定する。

- CPU: 1 vCPU
- memory: 512 MiB、swapなし
- PID: 64
- `/tmp`: 64 MiB
- network: deny
- wall-clock timeout: test suite 20分、個別process 60秒
- test thread: `cargo test -j 1 -- --test-threads=1`

確認事項:

- 通常fixture、import、REPL継続、tree、VM defensive testが上限内で完了する。
- budget超過はOOM killより前に構造化terminalとなる。
- timeout harnessが子processと子孫processを残さない。
- memory peak、実行時間、終了理由をcommit SHA、OS、arch、rustcとともに記録する。

#### Stress profile

weekly stressは固定seedを含む30分のsoakを行い、最低限次のworkloadを反復する。

- AST/import/call chainは各上限のN-1/N/N+1を各100回。
- List/Dict/String、copy-on-write、allocate/free、closure、rollback journalは要素数1,000と2,000を各100回。logical allocation比を2.2以下とする。
- REPLのcompile error、runtime error、catch、cancel、pause/resumeを10,000 chunk。warm-up 1,000 chunk後のlogical live bytes増加を1 MiB以下とする。
- active/queueを各設定上限まで満たすroundを1,000回。観測queue長は設定上限以下、Started数とTerminal数を完全一致させる。
- filesystem symlink policy、rename、dangling link、check/use raceを各policy 10,000回。root外変更を0件とする。
- record/replay、audit backpressure、orphan queueを各1,000 round。sequence gap・重複を0件とする。

panic、deadlock、root外I/O、terminal重複、audit欠落、cgroup OOMを1件でも検出したら失敗とする。30分終了時のprocess RSSは384 MiB以下、最後のfixture解放後のlogical live heapはbaseline + 1 MiB以下とする。wall-clock throughputはgateにせず、上記のlogical counter、ratio、上限だけを判定に使う。

#### Failure injection

fake hostとfaulting adapterで次を注入する。

- stdin EOF/read error、stdout short write/broken pipe。
- filesystem permission denied、not found、ENOSPC相当、partial write、rename failure、secure resolution unsupported。
- module resolver denial/error/cycle/depth超過。
- host function error、panic隔離、pending、late response、oversized response。
- monotonic deadline一致、clock後退、cancelとのbarrier race。
- audit sink temporary/permanent error、protocol error、永続Pending、event/byte上限。
- record store prepare/finish error、crash recovery、replay mismatch。
- malformed VM Chunk、invalid slot/constant/upvalue/jump/line table。

外部効果開始前の失敗はfail closed、開始後の効果は`effect_status`とterminalへ記録し、language stateは[実行予算・協調実行仕様](execution-control.md)どおりrollbackする。

## 6. Phase 7受入matrix

### 6.1 Capability matrix

`CapabilitySet::empty()`を既定にし、各rowは他authorityなしで単独検証する。deny時はadapter、callback、OS callが0でなければ失敗とする。

| Authority | grantなし | grantあり | 隣接authority分離 | 主な受入ID |
|---|---|---|---|---|
| `Environment` | `env()`はterminal `Denied` | snapshot内keyだけ取得、missingは`null` | process env変更、Clock、argsへ波及しない | CAP-AT-01/02/05/26/27 |
| `Clock` | clock trait call 0で`Denied` | injected fixed clockだけ参照 | deadline用monotonic clockと混同しない | CAP-AT-01/02/06/27 |
| `Stdin` | input adapter call 0で`Denied` | EOF、HostError、入力値を区別 | Stdoutを暗黙grantしない | CAP-AT-01/02/07/09 |
| `Stdout` | output adapter call 0で`Denied` | 許可された出力だけwrite | Stdin、filesystemを暗黙grantしない | CAP-AT-01/02/08 |
| `Filesystem.Read` | read前`Denied` | `read_file`/`read_lines`だけ成功 | Import、Metadataを許可しない | CAP-AT-01/02/10〜15 |
| `Filesystem.Write` + `Create` | upsert前`Denied` | `write_file`/`append_file`。新規作成には両方必要 | Read/Deleteを許可しない | CAP-AT-01/02/10〜15 |
| `Filesystem.Delete` | remove/rename前`Denied` | granted root・policy内だけ削除 | rename先Create、replace先Deleteを別検査 | CAP-AT-01/02/10〜15 |
| `Filesystem.Metadata` | metadata前`Denied` | exists/type/sizeだけ成功 | Read/Listを許可しない | CAP-AT-01/02/10〜15 |
| `Filesystem.List` | list前`Denied` | Metadata+Listの両方で`list_dir` | Read/Importを許可しない | CAP-AT-01/02/10〜15 |
| `Filesystem.Import` | resolver/file call 0で`Denied` | link時だけmodule取得 | runtime Readへ流用しない | CAP-AT-01/02/13/16/17 |
| `ProcessExit` | processを終了せず`Denied` | 0..255を`Exited` outcome化 | OS process自体を終了しない | CAP-AT-01/02/18/27 |
| `ModuleResolver` | import時resolver call 0で`Denied` | link時だけresolve | runtime filesystemを暗黙grantしない | CAP-AT-01/02/16 |
| `HostFunction` | callback 0で`Denied` | registeredかつgranted IDだけcall | 未登録/未grant/arityを分離 | CAP-AT-01/02/19〜22 |

追加の必須条件:

- `CapabilitySet` cloneはID・権限が一致し、revoke APIを持たない。
- start後にset、snapshot、adapter、redaction policyを変更できない。
- filesystemは7つの`FsOperation`全組合せ、3つのsymlink policy、複数mount、存在/不存在oracleを検証する。
- safe/legacy profileの差はgolden化し、safeではambient OS accessを0にする。
- CAP-AT-01〜30のうち実装Phaseに該当する全項目を通過しなければPhase 7適合としない。

### 6.2 Budget N-1 / N / N+1

各limitは独立fixtureで、消費量を厳密に`N-1`、`N`、`N+1`へ構成する。`N-1`と`N`は成功、`N+1`は対象操作・allocation・I/O・callbackの**前**に対応する`BudgetExceeded`となる。`N=0`は0消費成功と1消費失敗を別testにする。

| `BudgetConfig` field | 単位・対象 | N+1で必要な結果 |
|---|---|---|
| `heap_accounting_revision` | revision | `1`だけ受理、他はconfiguration error |
| `total_fuel` | logical fuel | `BudgetExceeded(Fuel)`、script catch不可 |
| `max_live_heap_bytes` | live logical bytes | allocation前`HeapBytes`、reservation refund |
| `max_string_allocations` | 累積count | `StringAllocations` |
| `max_string_bytes` | 累積bytes | `StringBytes` |
| `max_single_string_bytes` | 1 String bytes | `SingleStringBytes`、peakはN |
| `max_source_count` | root + module count | `SourceCount`、parse前拒否 |
| `max_source_bytes` | 累積source bytes | `SourceBytes`、chunk蓄積前拒否 |
| `max_single_source_bytes` | 1 source bytes | `SingleSourceBytes` |
| `max_import_count` | import count | `ImportCount`、resolver副作用前拒否 |
| `max_import_bytes` | import bytes | `ImportBytes` |
| `max_collection_elements` | 1 List/Dict elements | `CollectionElements`、mutation前拒否 |
| `max_input_calls` | call count | `InputCalls`、adapter callなし |
| `max_input_bytes` | response bytes | `InputBytes`、蓄積前拒否 |
| `max_output_calls` | call count | `OutputCalls`、adapter callなし |
| `max_output_bytes` | request bytes | `OutputBytes`、writeなし |
| `max_host_calls` | call count | `HostCalls`、callbackなし |
| `max_host_request_bytes` | request bytes | `HostRequestBytes`、callbackなし |
| `max_host_response_bytes` | response bytes | `HostResponseBytes`、script公開なし |
| `max_host_call_bytes` | request + response bytes | `HostCallBytes`、固定優先順位で判定 |
| `deadline` | monotonic ns | deadline直前は実行、等しい時点で`DeadlineExceeded` |

全fieldで次も必須とする。

- `u64::MAX`近傍、`usize`変換、加算・乗算overflowでwrapしない。
- 複合reservationはatomicで、部分commitしない。
- cumulative、live、per-itemのaccountingを区別する。
- `BudgetCharged`合計、`Terminal.usage`、`BudgetPeaks`が一致する。
- yield、pause、resumeを跨いでもtotal usageを補充しない。
- tree/VMでcharge point、N境界、terminal reasonを一致させる。

### 6.3 Audit completeness

| 観点 | 必須条件 |
|---|---|
| lifecycle | `ExecutionStarted`がsequence 0に1件、`Terminal`が最後に1件。gap・重複・terminal後eventなし |
| capability | 全allow/denyを`CapabilityDecision`で外部効果前に記録しackする |
| host call | `HostCallStarted` → `CapabilityDecision` → `HostCallFinished`を同じoperation/call IDで閉じる。deny/cancel/detach/errorもpairを閉じる |
| budget | `BudgetCharged`合計とterminal usage、per-item peakが一致する |
| cooperation | `Yielded`と`Resumed`が交互に対応し、pause、host pending、audit backpressureを識別できる |
| sink failure | temporary/permanent/protocol/Pending/上限で成功扱いにせずfail closed。Terminal reserveを常に残す |
| redaction | path/source/body/args/result/env/credentialのcanaryがencoded eventとdiagnosticへ平文で出ない |
| replay | capability/budgetをbypassせず、request順・ID・entry差を`ReplayMismatch`にする |
| drop/cancel | orphan queue引継ぎでもevent欠落・重複なし、callback再入でdeadlockなし |

監査event名は`ExecutionStarted`、`CapabilityDecision`、`HostCallStarted`、`HostCallFinished`、`BudgetCharged`、`Yielded`、`Resumed`、`Terminal`を正式名とし、旧暫定名を追加しない。

### 6.4 Backend conformance

production backendはtreeだけとする。VMは次の全行が0差分になるまでexperimentalであり、release assetへ含めてもproduction選択肢として宣伝しない。

| 比較対象 | 合格条件 |
|---|---|
| 規範fixture | return value、error、source位置、trace、context commitが完全一致 |
| host effect | FakeHost effect logがbyte・順序とも一致 |
| resource | fuel charge、budget usage/peak、yield位置、terminal reasonが一致 |
| audit | `normalize_for_conformance`後のsemantic event、sequence、payloadが一致 |
| identity | function生成履歴、closure、同一scope再宣言を含む値同一性が一致 |
| state | REPL継続、未捕捉error、rollback、import marker、pause/resumeが一致 |
| record/replay | tree recordとVM replayを含む全組合せが一致 |
| fuzz | AUD-022 generation matrixと4 fuzz targetで差分0 |
| known deviations | AUD-016、AUD-017、AUD-019、AUD-024、AUD-048を含む既知非適合0件 |

stdoutだけの一致、hello smoke、benchmark優位性はconformanceの証拠にしない。

## 7. Cargo installとbinary release

### 7.1 install経路

本文書の初期release範囲ではcrates.io publishを行わない。supportedなCargo installはsource checkoutまたはimmutable Git tagから行う。

```bash
# source checkoutから開発版をinstall
cargo install --locked --path .

# 同じpackageを明示的に再install
cargo install --locked --path . --force

# 公開済みrelease tagからinstall
cargo install --locked --git https://github.com/kamonabe/tsumugi.git --tag v<semver> tsumugi
```

- install先はCargo既定の`$CARGO_HOME/bin`、未設定時は`~/.cargo/bin`とする。
- PATHへ`$HOME/.cargo/bin`を追加する。
- bare `cargo install tsumugi`はcrates.io publishを別途承認するまでsupported手順に記載しない。
- uninstallは`cargo uninstall tsumugi`とする。
- Git tagからのsource installはGitHub repository access controlとtag immutabilityを信頼する**低保証経路**であり、Sigstore署名済みbinaryと同等のsupply-chain保証を持たない。検証可能なproduction installにはGitHub Release binaryまたは署名済みOCI digestを使う。
- Slice 4までに`tsumugi --version`を実装し、stdoutを`tsumugi <package-version> (language <revision>, commit <12桁SHA>)`の1行へ固定する。install smokeはこの値とfile実行のstdout完全一致を両方検証する。

### 7.2 platform artifact名

releaseは次の6 artifactを必須とする。各archiveは`tsumugi-v<semver>-<target>/`というtop-level directoryをちょうど1つ持ち、その直下へbinary、`README.md`、`LICENSE`を置く。root直下へのfile配置、追加の親directory、absolute path、symlinkを禁止する。

| OS | Arch | Rust target | Artifact |
|---|---|---|---|
| Linux | x86_64 | `x86_64-unknown-linux-gnu` | `tsumugi-v<semver>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux | aarch64 | `aarch64-unknown-linux-gnu` | `tsumugi-v<semver>-aarch64-unknown-linux-gnu.tar.gz` |
| macOS | x86_64 | `x86_64-apple-darwin` | `tsumugi-v<semver>-x86_64-apple-darwin.tar.gz` |
| macOS | aarch64 | `aarch64-apple-darwin` | `tsumugi-v<semver>-aarch64-apple-darwin.tar.gz` |
| Windows | x86_64 | `x86_64-pc-windows-msvc` | `tsumugi-v<semver>-x86_64-pc-windows-msvc.zip` |
| Windows | aarch64 | `aarch64-pc-windows-msvc` | `tsumugi-v<semver>-aarch64-pc-windows-msvc.zip` |

binary名はUnixで`tsumugi`、Windowsで`tsumugi.exe`とする。support floorはLinux glibc 2.36、macOS 13.0、Windows x86_64はWindows 10 22H2 / Server 2022、Windows aarch64はWindows 11とする。Linux artifactはDebian 12相当のpinned builder、macOSは`MACOSX_DEPLOYMENT_TARGET=13.0`、WindowsはMSVC targetでbuildする。

6 artifactはすべて、artifact SHA-256、release commit、target、runner image、package/language versionを記録した自動smokeを必須とする。native runnerを第一選択とし、Linux aarch64だけはQEMUによる自動実行を許可する。macOS/Windowsはnative runnerを必須とし、manual attestationをrelease成功へ数えない。必要なrunnerが利用できない場合は当該releaseをpublishしない。

### 7.3 SHA-256

- 6 archiveをfilename byte順に並べた`SHA256SUMS`を生成する。
- 形式はlowercase 64桁hex、空白2文字、filename、LF終端とする。
- `SHA256SUMS`自身は署名対象とし、同じfileへ自己hashを入れない。
- 利用者向け検証commandはLinux/macOSで`sha256sum -c SHA256SUMS`、Windowsで`Get-FileHash -Algorithm SHA256`による照合を示す。

### 7.4 GitHub Release内容

GitHub Releaseには最低限次を添付する。

- 6 platform archive
- `SHA256SUMS`
- 各archiveと`SHA256SUMS`のkeyless signature `.sig`およびcertificate `.pem`
- `tsumugi-v<semver>-sbom.spdx.json`
- `tsumugi-v<semver>-provenance.intoto.jsonl`
- SBOMの`.sig` / `.pem`
- release note

release noteの固定section:

1. Package version
2. Language revision
3. MSRV
4. Highlights
5. Breaking Changes / Deprecations
6. Security
7. Known backend deviations
8. Install
9. SHA-256 / signature / provenance検証
10. Rollback

### 7.5 promotion stateとrelease手順

1つのreleaseは次の状態を順に進む。後段を通過する前にGitHub Releaseを公開しない。

```text
PreflightValidated
  -> TagPushed
  -> BinaryDraftVerified
  -> OciPublishedUnrecommended
  -> ClusterValidated
  -> ReleasePublished
```

1. release PRでversion、language revision参照、release noteを確定し、stable/MSRV CI、coverage、docs、manifest、fuzz/stress、Phase 7 matrixを通す。
2. tag ruleset、required checks、release environment approval、job単位の最小permissionを有効化する。
3. protected environmentの手動承認後、`v<semver>` annotated tagを検証済みrelease commitへpushする。tag push後はversionを消費済みとし、workflow失敗時もtagを移動・再利用しない。
4. protected tag workflowがclean checkoutから6 artifactをbuildし、archive smoke、SHA-256、SBOM、provenance、署名を生成する。
5. draft GitHub Releaseへassetをuploadし、allow-list、hash、signature、subject commit、version、language revisionを独立verify jobで再検証する。draftは非公開のまま維持する。
6. 同じtag/commitからOCI imageをbuildし、第9章のgateを通してGHCRへ公開する。この時点のimageは`OciPublishedUnrecommended`であり、GitHub Releaseはまだdraftである。
7. image公開後にだけ第10章のmanifest PRを作成し、cluster smokeを完了する。初回manifest追加と後続version更新のどちらも同じgateに従う。
8. cluster smoke成功後、GitHub Releaseをpublishし、OCI imageを推奨対象へ昇格する。
9. release記録へtag、commit、workflow run、artifact hash、OCI index digest、SBOM/provenance digest、Kubernetes smoke証跡を保存する。

pre-tag検証失敗時はtagを作らない。tag push後の失敗時はそのSemVerをburnし、draft ReleaseとOCIの状態を記録して配布推奨にせず、修正を新しいSemVerで行う。同じtagを別commitまたは別bytesへ付け替えない。

## 8. Provenance、SBOM、signing

### 8.1 Binary provenance

- protected GitHub Actions OIDCを使い、SLSA provenance v1互換のin-toto JSONLを生成する。
- subjectは6 archive、`SHA256SUMS`、SBOMを含む。
- materialsにrepository URL、release commit SHA、`Cargo.lock` digest、workflow pathとcommit SHAを含める。
- self-hostedまたはmanual build artifactを正式releaseへ混在させない。manual attestationは実行smokeの補助だけに使い、artifact build provenanceを置換しない。

### 8.2 SBOM

- binary releaseはSPDX 2.3 JSONを正本とする。
- package、Rust dependencies、version、license、source commit、artifact digestを含める。
- OCI imageはimage indexと各platform manifestへSPDX JSON SBOMをOCI referrerとしてattachする。
- SBOM生成toolのversionとdatabase snapshotをprovenanceへ記録する。

### 8.3 Signing

- release assetはSigstore keyless `cosign sign-blob`相当で署名し、signatureとcertificateをGitHub Releaseへ添付する。
- OCI imageはtagでなくindex digestへ`cosign sign`相当で署名する。
- provenanceとSBOMはattestationとしてdigestへbindする。
- verify時はOIDC issuerを`https://token.actions.githubusercontent.com`、certificate identityを当repositoryのprotected release workflowへ制限する。
- private signing keyをrepository secretへ保存しない。
- signature、certificate、attestation検証に失敗したasset/imageは配布しない。

## 9. OCI image設計

### 9.1 位置づけ

OCI imageはTsumugi CLIを再現可能に実行する配布形式であり、daemon、HTTP service、controllerではない。image内のcapability/budgetはdefense-in-depthであり、container runtimeのCPU、memory、PID、mount、network隔離を置換しない。

### 9.2 Dockerfile要件

将来のDockerfileは次をすべて満たす。

- BuildKit syntaxを使うmulti-stage build。
- builderはRust 1.97のDebian slim系multi-arch image、runtimeはdistroless `cc-debian12:nonroot`系multi-arch imageとする。
- `FROM`はtagに加えて検証済みmulti-arch index digestへpinする。digestは実装時にregistryから機械取得し、review対象としてcommitする。
- buildは`cargo build --release --locked`を使用し、`Cargo.lock`不一致で失敗する。
- `.dockerignore`をDockerfileと同時に作成し、build contextから`.git`、`target`、fuzz artifact、local secret、editor cacheを除外する。CIは各禁止fileを一時配置したnegative context testでimage/layerへ含まれないことを確認する。
- runtimeへcopyする実行物は`/usr/local/bin/tsumugi`と必要なlicense metadataだけとする。compiler、Cargo、shell、package manager、source treeを含めない。
- `USER 65532:65532`、`WORKDIR /work`とする。
- entrypointはJSON形式`["/usr/local/bin/tsumugi"]`に固定する。
- image自体へscriptを焼き込まない。
- OCI label `source`、`revision`、`version`、`licenses=MIT`、`title=Tsumugi`を設定する。
- imageのroot filesystemは実行時read-onlyで動作し、書込みが必要な場合は明示mountした`/tmp`だけを使う。
- HEALTHCHECKは設定しない。serviceでないためhealth endpointを持たない。

### 9.3 build・tag・platform

```text
ghcr.io/kamonabe/tsumugi:<semver>
platforms = linux/amd64,linux/arm64
```

- `latest`をpushしない。
- `buildx`で1つのmulti-platform OCI indexを生成する。
- 各platformでnonroot、entrypoint、file smoke、read-only rootfsを検証する。
- index digestとplatform manifest digestをrelease記録へ保存する。
- tagを別digestへ付け替えない。

### 9.4 OCI supply chain gate

publish前に次を必須とする。

1. Linux amd64/arm64の両manifestが存在する。
2. container内のUID/GIDが65532である。
3. read-only rootfs、networkなし、64 MiB memory、100m CPUでsmoke scriptが完了する。
4. entrypoint以外のshell/package managerが存在しない。
5. SPDX SBOMとprovenanceがindex digestへattachされる。
6. index digestのkeyless signatureを独立jobでverifyする。
7. pinned versionのTrivyでOS/package scanを行う。DB snapshotは取得後24時間以内、DB取得・署名検証・scanのいずれかが失敗した場合はfail closedとする。CriticalまたはHighは既定でpublishをblockする。例外はrepository maintainer 1名の承認、CVE、影響分析、緩和策、owner、最長30日の期限を持つmachine-readable allow-listだけを許可し、期限切れをCIで拒否する。

## 10. Kubernetes参照Job

### 10.1 作成gate

次をすべて満たすまで`deploy/kubernetes/`とmanifestを作成してはならない。

- 対象SemVerのOCI imageがGHCRへ公開済み。
- amd64/arm64 index、SBOM、provenance、signatureをverify済み。
- image tagが実在するimmutable digestへ解決される。
- container smokeが両architectureで成功済み。
- manifest PRで`<semver>`を実在する完全なSemVerへ、`<index-digest>`を署名検証済みOCI indexの64桁SHA-256へ置換し、placeholder、`latest`、存在しないtag/digestが0件。

manifest作成時のimage fieldは`ghcr.io/kamonabe/tsumugi:<semver>@sha256:<index-digest>`とし、**完全なSemVer tagとindex digestの両方**を固定する。例えば公開済みversionが`0.2.0`なら、tag `ghcr.io/kamonabe/tsumugi:0.2.0`が解決した検証済みindex digestを同じfieldへ付記する。以下の規範template中の2つのplaceholderは設計上の置換位置であり、そのままcommitしてはならない。

### 10.2 HelmとMitori CronJob規約を流用しない理由

Tsumugiはserviceでも定期実行applicationでもなく、配布物はCLI/libraryである。参照資材の目的はimageのone-shot smokeだけであるため、Helmのvalues、release lifecycle、Service、Deployment、probe、Ingressを持たせない。

MitoriのCronJob規約は、k3s上で定期収集・通知するapplication向けである。Tsumugi smokeにはschedule、`concurrencyPolicy`、Job履歴数、DB/Slack Secret、`common-lib`、service固有ConfigMap generatorが存在しないため、そのまま流用しない。一方、namespace明示、固定image、`ghcr-secret`、resources、nonroot、最小権限というcluster共通の安全要件は採用する。

### 10.3 `deploy/kubernetes/smoke-job.yaml`の完成形

このfileはConfigMapとJobの2 documentだけを含む。scriptはimageへ焼き込まずConfigMapから`/scripts/smoke.tsg`へread-only mountする。

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: tsumugi-smoke-script
  namespace: app
  labels:
    app.kubernetes.io/name: tsumugi
    app.kubernetes.io/component: smoke-test
    app.kubernetes.io/managed-by: kubectl
immutable: true
data:
  smoke.tsg: |
    print("tsumugi smoke ok")
---
apiVersion: batch/v1
kind: Job
metadata:
  name: tsumugi-smoke
  namespace: app
  labels:
    app.kubernetes.io/name: tsumugi
    app.kubernetes.io/component: smoke-test
    app.kubernetes.io/managed-by: kubectl
spec:
  completions: 1
  parallelism: 1
  backoffLimit: 0
  activeDeadlineSeconds: 60
  ttlSecondsAfterFinished: 300
  template:
    metadata:
      labels:
        app.kubernetes.io/name: tsumugi
        app.kubernetes.io/component: smoke-test
    spec:
      automountServiceAccountToken: false
      enableServiceLinks: false
      restartPolicy: Never
      terminationGracePeriodSeconds: 5
      imagePullSecrets:
        - name: ghcr-secret
      securityContext:
        runAsNonRoot: true
        runAsUser: 65532
        runAsGroup: 65532
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: tsumugi
          image: ghcr.io/kamonabe/tsumugi:<semver>@sha256:<index-digest>
          imagePullPolicy: IfNotPresent
          command:
            - /usr/local/bin/tsumugi
          args:
            - /scripts/smoke.tsg
          workingDir: /tmp
          stdin: false
          tty: false
          env:
            - name: HOME
              value: /tmp
            - name: TMPDIR
              value: /tmp
          securityContext:
            runAsNonRoot: true
            runAsUser: 65532
            runAsGroup: 65532
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop:
                - ALL
          resources:
            requests:
              cpu: 10m
              memory: 16Mi
            limits:
              cpu: 100m
              memory: 64Mi
          volumeMounts:
            - name: smoke-script
              mountPath: /scripts/smoke.tsg
              subPath: smoke.tsg
              readOnly: true
            - name: tmp
              mountPath: /tmp
          terminationMessagePolicy: FallbackToLogsOnError
      volumes:
        - name: smoke-script
          configMap:
            name: tsumugi-smoke-script
            defaultMode: 0444
            items:
              - key: smoke.tsg
                path: smoke.tsg
        - name: tmp
          emptyDir:
            sizeLimit: 16Mi
```

Jobは`tsumugi smoke ok`を1行だけstdoutへ出し、exit 0で完了しなければ失敗とする。`backoffLimit: 0`なので自動retryせず、原因を確認して新しいJobとして再実行する。

### 10.4 `deploy/kubernetes/networkpolicy-deny-all.yaml`の完成形

```yaml
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: tsumugi-smoke-deny-all
  namespace: app
  labels:
    app.kubernetes.io/name: tsumugi
    app.kubernetes.io/component: smoke-test
    app.kubernetes.io/managed-by: kubectl
spec:
  podSelector:
    matchLabels:
      app.kubernetes.io/name: tsumugi
      app.kubernetes.io/component: smoke-test
  policyTypes:
    - Ingress
    - Egress
  ingress: []
  egress: []
```

smoke scriptはnetworkを必要としない。DNSを含むegressを許可しない。将来networkを使うhost function smokeはこのJobを変更せず、別設計・別manifest・別承認で追加する。

NetworkPolicyの存在だけをdenyの証拠にしない。deploy対象clusterはNetworkPolicy ingress/egress enforcement対応CNIを必須とし、直近30日以内にupstream準拠のnetwork-policy conformance testへ成功していなければdeployを中止する。deploy前検証は`app` namespace内の全NetworkPolicy selectorを評価し、smoke Podを選択してegressをallowする別policyが1件でもあれば失敗する。

apply後はrepositoryへ保存しない一時negative-probe Podを同じPod labelsで起動し、Kubernetes API ClusterIPへのTCP接続と外部DNS解決がtimeout/denyになることを確認する。probe imageはworkflowでdigest pin・署名検証したnetwork diagnostic imageとし、probe成功（通信成立）はgate失敗、通信拒否確認後は必ず削除する。Tsumugi Job成功、negative probeの通信失敗、競合allow policy 0件の3条件をまとめて「deny-all下で成功」と判定する。

### 10.5 Kustomizeの扱い

現repositoryには`kustomization.yaml`がない。上記2 fileだけではoverlayやgeneratorが不要なため、初回はplain manifestとする。Kustomize導入、`kustomization.yaml`作成、ConfigMap generator化はmanifest作成時の**別承認**を必須とし、本文書の承認に含めない。

Kustomizeを将来承認した場合も、generated ConfigMap名とJob参照の整合、固定image tag、namespace `app`、security context、NetworkPolicyを`kustomize build`出力に対して再検証する。

## 11. Kubernetes検証、deploy、rollback

### 11.1 kubeconform

manifest作成後、CIではKubernetes 1.30.0を最低schema baselineとして次を実行する。

```bash
kubeconform \
  -strict \
  -summary \
  -kubernetes-version 1.30.0 \
  deploy/kubernetes/networkpolicy-deny-all.yaml \
  deploy/kubernetes/smoke-job.yaml
```

対象clusterへdeployする前に、clusterのserver versionとserver-side schemaでも検証する。

```bash
kubectl apply --dry-run=server \
  -f deploy/kubernetes/networkpolicy-deny-all.yaml \
  -f deploy/kubernetes/smoke-job.yaml
```

`-ignore-missing-schemas`は使用しない。warning、unknown field、namespace欠落、placeholder image、`latest`、security field欠落をfailureとする。

### 11.2 deploy順

1. `app` namespaceと`ghcr-secret`の存在だけを確認する。本文書のmanifestから作成しない。
2. manifestのimage tagをGHCRでresolveし、release記録のindex digestと一致することを確認する。
3. OCI signature、SBOM、provenanceをverifyする。
4. `kubeconform`とserver-side dry-runを実行する。
5. 同名Job/immutable ConfigMapが残る場合は、log保存後にJob、ConfigMapの順で削除する。
6. NetworkPolicyを先にapplyする。
7. ConfigMap + Jobをapplyする。
8. Job完了を待つ。
9. `scripts/verify-kubernetes-smoke.sh`を実行し、stdout byte列、Pod/Container status、実行digest、security context、NetworkPolicy競合、negative egress probeを機械判定する。scriptはSlice 6でmanifestと同時に実装し、差異時にnon-zeroを返す。
10. 結果をrelease記録へ保存し、TTL後にJobが削除されたことを確認する。

```bash
kubectl apply -f deploy/kubernetes/networkpolicy-deny-all.yaml
kubectl apply -f deploy/kubernetes/smoke-job.yaml
kubectl wait --namespace app --for=condition=complete job/tsumugi-smoke --timeout=90s
scripts/verify-kubernetes-smoke.sh \
  --namespace app \
  --job tsumugi-smoke \
  --expected-output 'tsumugi smoke ok' \
  --expected-index-digest 'sha256:<index-digest>'
```

検証scriptは最低限、次をassertする。

- Job `Complete=True`、`Failed` conditionなし、Pod phase `Succeeded`、container exit code 0、restart count 0。
- stdoutはUTF-8 `tsumugi smoke ok\n`とのbyte完全一致で、stderrは空。
- Podのresolved image IDが署名検証済みindex配下の対象platform manifest digestと一致。
- effective Pod specがUID/GID 65532、nonroot、read-only rootfs、privilege escalation false、capabilities drop ALL、RuntimeDefault seccomp、service account tokenなし。
- active deadline内に完了し、requests/limits、ConfigMap read-only mount、`/tmp` emptyDirが規範templateと一致。
- 対象Podを選ぶ別のegress allow NetworkPolicyが0件。
- digest pinした一時probeからKubernetes API ClusterIPへのTCP接続と外部DNS解決が失敗し、probe自体は判定結果を保存して削除される。

JSON表示や人手でのlog確認だけを合格判定に使用しない。

### 11.3 rollback

Kubernetes資材は参照用Jobでありtrafficを持たない。rollbackは次とする。

1. 失敗Jobのdescribe、events、logs、Pod image IDを保存する。
2. `kubectl delete job tsumugi-smoke -n app --ignore-not-found`を実行する。
3. script変更がある場合はimmutable ConfigMapも削除する。
4. manifestのimageを直前の検証済みSemVerへ戻す。tagを付け替えない。
5. kubeconform、signature/digest verify、server-side dry-runを再実行する。
6. NetworkPolicyを残したままConfigMap + Jobを再applyし、同じsmokeを確認する。

NetworkPolicy自体に問題がある場合だけ、影響を記録して削除する。deny-allを一時的に緩和してsmokeを通してはならない。

## 12. Runbook

### 12.1 Release前

- release commit、tag予定version、Cargo version、language revision、MSRVを照合する。
- required CI、weekly fuzz/stressの直近成功がrelease commitを含むことを確認する。
- known backend deviationsをrelease noteへ列挙する。
- 6 target runner、OIDC、GHCR permission、GitHub environment approvalを確認する。
- dependency、license、vulnerability、secret scanを確認する。

### 12.2 Release失敗

| 症状 | 対応 |
|---|---|
| pre-tag build/test失敗 | tagを作成せず修正PR |
| tag push後のbuild/test失敗 | tagを移動せず当該SemVerをburnし、draftを非公開のまま記録して新SemVerで修正 |
| hash/signature不一致 | 全assetを破棄しworkflowを調査。部分release禁止 |
| SBOM/provenance欠落 | release publish禁止 |
| 1 platformだけ失敗 | 6 platformが揃うまでpublish禁止 |
| GitHub upload一部失敗 | draftのまま保持し、同じverified artifactだけ再upload |
| OCI 1 arch失敗 | image publishとKubernetes manifest作成を禁止 |
| Kubernetes smoke失敗 | image ID/log/eventを保存し、releaseを推奨対象にしない |

### 12.3 公開後の不具合

1. severityと影響platform/versionを判定する。
2. compromised release pageの先頭へ警告を追記し、install推奨を停止する。
3. tagを別bytesへ付け替えず、必要なら危険なasset/imageを配布先からremove/revokeする。
4. 直前の安全なSemVerへのrollback commandを公開する。
5. 修正版を新しいpatch/minor versionでreleaseする。
6. root cause、検出できなかったgate、追加testをroadmap/AUDへ記録する。

### 12.4 証跡

各releaseについて次を保存する。

- Git tag、commit SHA、package version、language revision、MSRV
- workflow run URLと使用したAction SHA
- 6 archive hash、SBOM/provenance/signature hash
- OCI index/platform digestとsignature verification結果
- test/fuzz/stress/capability/budget/audit/backend gate結果
- Kubernetes smokeのnamespace、Job UID、Pod image ID、開始/終了時刻、stdout、終了code
- 例外承認がある場合の理由、owner、期限

## 13. SLOと運用指標

Tsumugiはserviceではないため、uptime、request latency、throughputのservice SLOは設定しない。次をrelease operation SLOとする。

| SLO | 目標 |
|---|---:|
| 公開releaseの必須asset・hash・SBOM・provenance・署名完全性 | 100% |
| 公開releaseの6 platform smoke成功 | 100% |
| 公開OCIのamd64/arm64、nonroot、read-only smoke成功 | 100% |
| Kubernetes参照Jobの完了 | 60秒以内、成功率100%をrelease gateとする |
| Job stdout | `tsumugi smoke ok` 1行との完全一致100% |
| confirmed critical releaseのinstall推奨停止と警告掲示 | 判定後60分以内 |
| rollback手順の公開 | 判定後4時間以内 |
| weekly fuzz/stress failureのtriage | 次の2営業日以内 |

release gateの100%は統計的availabilityではなく、1件でも欠ければ公開しないという完全性条件である。

監視する指標:

- CI job時間、flaky retry数、coverage、fuzz executions、unique crash数。
- stress時peak RSS、logical heap、queue長、terminal reason分布。
- release asset download error、signature verification failure報告。
- OCI digest、vulnerability scan、Kubernetes Job duration/reason。

## 14. Rollbackとdeprecation policy

### 14.1 Binary / source rollback

- rollback先は直前の検証済みSemVerとcommit SHAで指定する。
- Git tagとartifactを上書きしない。
- Cargo installは旧tagを明示して再installする。

```bash
cargo install --locked --force \
  --git https://github.com/kamonabe/tsumugi.git \
  --tag v<previous-semver> \
  tsumugi
```

- GitHub binaryは旧releaseのSHA-256とsignatureをverifyして置換する。
- 実行中continuation、REPL state、record storeをversion間でresumeしない。script sourceとhost側dataから新processで開始する。

### 14.2 OCI rollback

- manifestのimageを旧SemVer tagと、そのtagに対応する検証済みindex digestの組へ戻し、signature/SBOMを再verifyする。
- `latest`や同じtagのretagでrollbackしない。
- security理由でrevokeしたdigestへ戻さない。

### 14.3 Deprecation

- package `0.x`でも、通常の公開API・CLI・言語機能削除は最低1 minor releaseのdeprecation期間を設ける。
- deprecation開始releaseでrelease note、compiler/runtime warning、移行例、削除予定versionを提供する。
- security、data corruption、host panicを防ぐ緊急変更はdeprecation期間を省略できるが、release noteとsecurity advisoryへ理由を記載する。
- 言語意味論変更は言語revisionを上げ、旧revisionを実装する最終package versionを記録する。
- audit schema、record format、OCI layoutの互換性変更はmigration/rollback手順なしに行わない。
- VM experimentalの挙動は互換保証対象外だが、既知差分と修正はAUD IDで追跡する。

## 15. ローカル検証command

### 15.1 現在実行可能な基本検証

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features --locked
cargo test -j 1 -- --test-threads=1
cargo build --release --locked
cargo run -- examples/hello.tsg
cargo run -- --vm examples/hello.tsg
```

### 15.2 MSRVとcoverage

```bash
rustup toolchain install 1.97.0 --profile minimal
cargo +1.97.0 test --all-features --locked
cargo llvm-cov --all-features --workspace \
  --fail-under-lines 80 \
  --lcov --output-path lcov.info
```

### 15.3 Package / install smoke

```bash
cargo metadata --locked --no-deps --format-version 1
cargo package --locked
cargo package --locked --list
cargo install --locked --path . --force

smoke_file="$(mktemp)"
printf 'print("tsumugi smoke ok")\n' > "$smoke_file"
tsumugi "$smoke_file"
rm -f "$smoke_file"
```

WindowsではPowerShellのtemporary fileと`tsumugi.exe`で同じstdoutを検証する。

### 15.4 将来資材作成後

```bash
# fuzz smoke
cargo fuzz run lexer -- -max_total_time=60 -timeout=10 -rss_limit_mb=512
cargo fuzz run parser -- -max_total_time=60 -timeout=10 -rss_limit_mb=512
cargo fuzz run engine -- -max_total_time=60 -timeout=10 -rss_limit_mb=512
cargo fuzz run vm -- -max_total_time=60 -timeout=10 -rss_limit_mb=512

# Kubernetes schema
kubeconform -strict -summary -kubernetes-version 1.30.0 \
  deploy/kubernetes/networkpolicy-deny-all.yaml \
  deploy/kubernetes/smoke-job.yaml
```

これらは対応するtoolとfileが実装された後のcommandであり、本文書作成時点では実行対象が存在しない。

## 16. 実装slice

依存順を次に固定する。各sliceは独立PRとし、前sliceの受入を通してから次へ進む。

### Slice 1 — MSRVとversion関係

変更対象:

- `Cargo.toml`へ`rust-version = "1.97"`
- READMEへCargo install、PATH、package/language revision関係
- MSRV CI job

受入:

- Rust 1.97.0とstableで`cargo test --all-features --locked`成功。
- 1.96ではsupportを主張しない。
- AUD-045のtoolchain/install部分を完了扱いにできる。

### Slice 2 — CI hardening

変更対象:

- Clippy all-targets/all-features
- coverage 80% gate
- docs link/drift、Cargo/workflow manifest validation
- 全ActionのSHA pin

受入:

- 3 OS stable、Ubuntu MSRVがrequired check。
- relative link、version drift、unpinned Actionを故意に入れるnegative testが失敗する。
- current main相当の正常caseが成功する。

### Slice 3 — AUD-022 fuzz / matrix / failure injection

変更対象:

- 4 fuzz target、corpus、dictionary、minimize手順
- PR smoke、weekly、release candidate schedule
- capability、budget、audit、backend matrix harness
- resource-constrained、stress、failure injection

受入:

- 5章と6章の全表を自動化。
- crash artifactを最小化・再実行・retentionできる。
- timeout、memory、secret scanが機能する。
- AUD-022の残るmatrix/fuzzを完了扱いにできる。

### Slice 4 — Binary release candidate

開始条件: branch protection required checks、protected tag ruleset、release environment approval、publish jobの最小permissionが有効であること。

変更対象:

- `tsumugi --version`の固定出力
- pre-tag validationとprotected tag release workflow
- 6 artifact、SHA256SUMS、draft GitHub Release
- SBOM、provenance、keyless signing
- install/rollback runbook

受入:

- draft releaseで全artifact名、archive内容、hash、signature、provenance subjectを検証。
- tag/Cargo/CLI/artifact version不一致をblock。
- partial releaseをpublishできず、tag push後の失敗SemVerを再利用できない。
- source installの低保証経路と署名済みbinaryの検証経路を区別する。
- AUD-045のrelease/install部分を完了扱いにできる。

### Slice 5 — OCI image

開始条件: Slice 4のtagと検証済みdraft GitHub Release assetが存在し、GitHub Release自体は未公開であること。

変更対象:

- multi-stage Dockerfileと`.dockerignore`
- amd64/arm64 build/push
- OCI SBOM、provenance、signature、scan

受入:

- 9章の全gate成功。
- UID/GID 65532、read-only、networkなし、resource制約下でsmoke成功。
- build context禁止fileがimage/layerに含まれないnegative test成功。
- `latest`とmutable tagが存在せず、公開imageは`OciPublishedUnrecommended`として記録される。

### Slice 6 — Kubernetes参照Job

開始条件: 対象OCI imageが公開・verify済みで、GitHub Releaseがdraftであること。

変更対象:

- `deploy/kubernetes/smoke-job.yaml`
- `deploy/kubernetes/networkpolicy-deny-all.yaml`
- `scripts/verify-kubernetes-smoke.sh`
- kubeconform/server dry-run/CNI conformance/競合policy/negative probe手順

受入:

- 10.3と10.4の全fieldに一致し、imageは実在する固定SemVer tagと署名済みindex digestの両方を持つ。
- kubeconform strictとserver-side dry-run成功。
- NetworkPolicy対応CNI、競合allow policy 0件、negative probe通信拒否を機械判定する。
- deny-all下で60秒以内、stdout完全一致、restart 0、exit 0。
- Helm、Service、Deployment、CronJobを追加しない。
- Kustomizeは別承認がない限り追加しない。

### Slice 7 — 公開・運用gate

変更対象:

- cluster smoke成功後のGitHub Release publish/promotion
- runbook、SLO証跡、rollback drill

受入:

- binary draft、OCI、Kubernetesの部分成功でGitHub Releaseをpublishできない。
- 旧SemVerへのbinary/OCI/Kubernetes rollback drillを完了する。
- release記録の全fieldを追跡できる。
- `ReleasePublished`後にartifact/tag/image bytesを変更できない。

## 17. 最終受入基準

本文書の「実装済み」への変更は次をすべて満たした場合だけ許可する。

| ID | 基準 |
|---|---|
| VRO-AT-01 | `rust-version = "1.97"`、stable/MSRV CI、3 OS testがrequired |
| VRO-AT-02 | fmt、all-targets Clippy、coverage 80%、docs drift、manifest validation成功 |
| VRO-AT-03 | 全Actionが40桁SHA pin、release permissionがprotected jobだけ |
| VRO-AT-04 | 4 fuzz targetのPR/weekly/release profile、minimize、artifact retentionが動作 |
| VRO-AT-05 | resource-constrained、stress、failure injectionでpanic/deadlock/OOM前無制限allocationが0 |
| VRO-AT-06 | Capability matrixとCAP-AT-01〜30の該当項目が成功 |
| VRO-AT-07 | 全`BudgetConfig` fieldのN-1/N/N+1、overflow、reservation、deadlineが成功 |
| VRO-AT-08 | audit Started/Terminal、sequence、host pair、budget、yield/resume、redaction、sink failureが完全 |
| VRO-AT-09 | treeが規範backend。VMは差分0までexperimentalを強制 |
| VRO-AT-10 | 6 binary artifact、SHA256SUMS、GitHub Release、SBOM、provenance、signatureが完全 |
| VRO-AT-11 | Cargo install、upgrade、uninstall、rollback手順がrelease tagで再現可能 |
| VRO-AT-12 | OCI amd64/arm64、fixed SemVer tag/index digest、UID/GID 65532、read-only、entrypoint、SBOM、署名が完全 |
| VRO-AT-13 | image公開前にKubernetes manifestが存在せず、公開後manifestは規範templateと一致 |
| VRO-AT-14 | kubeconform strict、server dry-run、CNI/競合policy/negative probe、deploy順、rollback drillが成功 |
| VRO-AT-15 | service/Helm/CronJobを追加せず、参照用JobのSLOと証跡を満たす |

## 18. ロードマップ・AUD項目との対応

- **Phase 7:** timeout・golden・scalingという現行土台へ、本文書のfuzz、stress、資源制約、failure injection、capability、budget、audit、backend conformance、release/OCI/Kubernetes gateを追加する。VRO-AT-01〜15完了までPhase 7は部分実装のままとする。
- **AUD-022:** 既存のtimeout、完全一致golden、fixture整合、一時directory分離を維持し、Slice 3で網羅matrix、fuzz、failure artifact、backend semantic comparisonを完成させる。
- **AUD-045:** Slice 1、2、4でMSRV、Cargo install、release artifact、GitHub Release、Action SHA pin、rollbackを完成させる。
- **配布・運用資材不足:** Slice 4〜7でbinary release、OCI、参照用Kubernetes Job、runbook、SLOを順に追加する。存在しないimageを参照する先行manifestは禁止する。

本文書で未決定事項は残さない。toolやbase imageのpatch version・digestは、実装時点で本文書の固定policyに従って機械取得しSHAへpinする実装値であり、architectureや運用方針を再判断する事項ではない。
