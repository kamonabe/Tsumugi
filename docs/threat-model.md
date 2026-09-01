# Tsumugi — 脅威モデル

最終更新: 2026-08-31
設計ステータス: **実装仕様確定・未実装**

## 1. 目的と適用範囲

本文書は、Tsumugiをサーバーアプリケーションまたは業務システムへ組み込むときの保証境界、攻撃主体、資産、責任分界、残余リスクを固定するPhase 0の規範文書である。[Tsumugi Manifesto](manifesto.md)の原則を脅威へ展開し、[組み込みAPI仕様](embedding-api.md)と[capability model](capability-model.md)が満たすべきsecurity要件を定める。

対象はTsumugi library、公式CLI adapter、Tsumugi sourceとimport、hostが登録するcapabilityおよびhost functionである。HTTP、database、mail等のhost adapterと外部serviceは境界上の主体として扱うが、core実装には含めない。

本文書の「拒否」は、指定がない限りOS操作、host callback、外部service呼び出しより前に成立することを意味する。「実行」はroot sourceのcompile、import graphのlink、scriptのstartからterminal outcomeまでを指す。

## 2. 文書間の規範関係

- [次期意味論・実装決定](semantic-decisions.md): 次期言語挙動、CLI、canonical error、script catch可否の正本
- [実行予算・協調実行仕様](execution-control.md): Phase 3/4の公開API、状態機械、有限budget、AUD-024 transactionの正本
- [決定性・実行時監査仕様](determinism-and-audit.md): Phase 5/6のaudit schema、record/replay、redaction、fail-closedの正本
- 本文書: Phase 0の保証、非保証、責任分界、脅威と検証条件の正本
- [組み込みAPI仕様](embedding-api.md): Phase 1/2のEngine、compile/link、source identity、terminal channelの先行契約
- [Capability Model仕様](capability-model.md): Phase 2の外部効果認可、path-handle、adapter、deny-by-defaultの先行契約
- [言語仕様](language-spec.md): 次期revision統合前の現行観測意味論の正本
- [ロードマップ](roadmap.md): 実装状態と順序の正本

新規6文書で矛盾した場合は上記の領域別正本を優先する。embedding/capabilityの先行bootstrapが後続最終契約と異なる場合は後続へ統合し、同じbuildへ旧型・旧状態・旧event・旧transactionを併存させない。security boundaryとhost/Tsumugi/OS責任は本文書を優先するが、catch可否やAPI名を本文書だけで上書きしない。ロードマップの完了表示は各正本の受入基準を緩和しない。

## 3. 用語

| 用語 | 定義 |
|---|---|
| host | Tsumugi libraryを呼び出し、source、入力、capability、予算、audit sinkを所有するアプリケーション |
| script | root sourceおよびlinkされたTsumugi moduleのコード |
| ambient access | 実行requestに明示されていないprocess環境、filesystem、clock、stdio、argv、network等へのアクセス |
| capability | hostが1実行へ明示的に付与する、偽造不能かつ実行中不変の外部操作権限 |
| host function | hostがregistryへ登録し、さらに実行単位でgrantした場合だけ呼べる同期callback |
| terminal outcome | `Completed`、`Exited`、`Denied`、`LinkError`、`RuntimeError`、`HostError`、`BudgetExceeded`、`DeadlineExceeded`、`Cancelled`、`AuditFailure`、`InternalFailure`、`RecordFailure`、`ReplayMismatch`のいずれか |
| security boundary | それ単独で信頼度の異なるコードを隔離できる境界 |
| safe point | evaluator/compilerがcancellation、fuel、deadlineを確認し、hostへ制御を返せる地点 |
| N | 設定された上限値。N-1、N、N+1は境界試験の消費量を表す |

## 4. Actor

| Actor | 信頼 | 能力と前提 |
|---|---|---|
| 信頼済みhost | trusted computing base | Engineを構成し、source、capability、adapter、予算、audit方針を渡す。誤設定・実装bugはあり得る |
| 誤作動script | 非信頼 | 善意だが、無限loop、巨大allocation、誤path、secret出力、過剰なhost callを起こし得る |
| 敵対的script | 非信頼 | parser/evaluator差、resource limit、symlink race、error/audit、host functionを利用して権限取得・情報窃取・可用性低下を狙う |
| 外部service | 部分信頼または非信頼 | host adapter経由で遅延、巨大応答、不正応答、secret要求、supply-chain由来の挙動を持ち得る |
| OS・container runtime | TCB | process、file descriptor、memory、CPU、thread、signal、filesystem namespaceを隔離する |
| Tsumugi実装・依存crate | TCBだが欠陥を想定 | parser、tree backend、VM、adapter interface、依存ライブラリ。panic、resource accounting漏れ、backend差を起こし得る |

hostが悪意を持つ場合、同一process内のscriptをhostから保護することは目標にしない。host functionまたはadapterが契約に違反した場合、Tsumugiは検出できる範囲だけを`HostError`または`InternalFailure`へ変換する。

## 5. Asset

1. **host可用性**: event loop、worker、他tenant、process継続、応答時間
2. **host完全性**: host memory、execution context、registry、他実行の状態、filesystem、外部service状態
3. **機密性**: environment、stdin、filesystem、host function引数・結果、認証情報、source、error、audit record
4. **実行の完全性**: source hash、language revision、import graph、backend、入力、capability、予算、評価順、終了理由
5. **監査完全性**: start、grant、allow/deny、host call、resource消費、terminal outcomeの対応関係
6. **供給網完全性**: Tsumugi crate、依存crate、binary、import source、host adapter、外部service client

## 6. Trust boundary

```text
非信頼script/source
        |
        | B1: lexer/parser/linker（不正入力・import graph）
        v
Tsumugi core ── B2: capability dispatcher ──> host adapter ── B4 ──> OS / 外部service
        |                    |
        | B3: host function registry/callback
        v                    v
 ExecutionContext       信頼済みhost code
        |
        | B5: audit sink（redaction済みeventのみ）
        v
   audit backend

Tsumugi process全体 ── B6: process/container/cgroup/OS boundary ──> 他process・host OS
```

- **B1**は入力検証境界であり、sourceを信頼済みに昇格させない。
- **B2**は認可境界である。capabilityがなければOS APIを呼ばない。
- **B3**はnative code境界である。host callbackはscriptより強い権限を持ち得る。
- **B4**はTsumugi外のI/O境界である。timeout、TLS、credential、response上限はadapter/host責任である。
- **B5**は情報流出境界である。audit sinkを信頼しても、不要な値をeventへ載せない。
- **B6**だけが敵対的scriptに対する強制的なCPU、memory、process、filesystemのsecurity boundaryになり得る。

Tsumugiのsandbox、path check、step limit、capability modelはdefense-in-depthであり、**Tsumugi単体はsecurity boundaryではない**。敵対的scriptは別process、非root、最小mount、cgroupまたは同等のCPU/memory制限、OS deadline、必要ならnetwork namespaceと組み合わせる。

## 7. 保証する性質

Phase 0〜2の受入完了時、次を保証する。

1. 新しい`Engine`は`CapabilitySet::empty()`でdeny-by-defaultである。safe CLI profileはprofile builderがstdout adapterだけを明示grantする既定構成であり、ambient stdio accessではない。filesystem、environment、clock、script stdin、process exit、module resolver、host functionはoptionなしでgrantしない。
2. capability denialは対象のOS操作、callback、resolver呼び出しより前に成立する。script操作中はsanitizedなcanonical `capability` / `sandbox` errorとしてcatchできるが権限は得られず、未捕捉なら`RuntimeError`になる。link/import等のhandler開始前はterminal `Denied`でcatchできない。
3. library実行経路はscript入力によって`std::process::exit`を呼ばない。catch可能なpanicをhost境界外へunwindさせない。
4. root source hash、language revision、link済みimport graph、backend、capability ID、terminal outcomeをhostが構造化して取得できる。
5. capabilityとregistryは実行開始時にfreezeされ、その実行中のgrant/revokeはできない。別実行・別contextへ暗黙に継承しない。
6. tree-walkを規範backendとし、VMはconformance完了までexperimentalとして明示される。
7. errorはsecret本文を記録しない。Phase 2でaudit用redaction metadataを固定し、Phase 6でeventを発行する場合も同じpolicyを適用する。

## 8. 保証しない性質

1. Tsumugiだけで敵対的codeを完全隔離すること
2. Rust processのOOM abort、stack overflow abort、`panic=abort`、OS kill、hardware faultからの同一process回復
3. host callbackが`abort`、`_exit`、undefined behavior、無制限blocking、権限外I/Oを行わないこと
4. OS・filesystemがsecure handle契約を提供しない環境で、path文字列検査だけによりTOCTOUを排除すること。この場合adapterは操作を拒否しなければならない
5. deadline到達時に、実行中のblocking OS callまたは外部service callをTsumugi coreがpreemptすること
6. global heap quota、全allocation accounting、engine全体の同時実行制御。これらはPhase 3/4で扱う
7. host、audit backend、dependency、import source、外部serviceそのものの正当性
8. rollback不能な外部副作用を含むexecution transaction

## 9. 責任分界

| 責任 | host | Tsumugi | OS / container |
|---|---|---|---|
| source・import元の選定 | 所有 | hash・link情報を記録 | file/network隔離 |
| capability grant | 最小権限で構成 | deny-by-default・事前判定 | 最終強制 |
| filesystem symlink/TOCTOU | secure adapterを選択 | portable handle契約を要求し、未対応adapterを拒否 | descriptor・namespace・mount隔離 |
| CPU | Phase 3のexecution budgetとOS hard limitを設定 | Phase 3でsafe pointのfuel/deadline/cancelを確認 | cgroup/rlimit/別processでhard limit |
| memory/OOM | process memory limit設定 | 既存collection上限、Phase 3のheap/input上限 | cgroup/rlimit/OOM isolation |
| stack | 適切なthread/別process | AST/call/import深度検査、panic変換 | stack limit・process隔離 |
| blocking I/O | timeout対応adapter | Phase 3でdeadline/cancel contextを有効化 | socket timeout・signal・process kill |
| secret | 必要最小限だけ注入 | snapshot・redaction・deny | process/mount/credential隔離 |
| audit | sink保護・retention | Phase 2でschema/redactionを固定し、Phase 6でevent/sequenceを発行 | log storage ACL |
| host function | 安全なcallback実装 | arity/grant/cost/redaction/panic捕捉 | native codeの最終隔離 |
| supply chain | lockfile、署名、review、更新 | revision/hash公開 | package/image verification |
| terminal outcome処理 | outcomeを必ず処理 | 全正常・制御失敗を構造化 | process deathは別channelで通知 |

## 10. 脅威一覧

`受入`列のIDは第11節の試験を指す。Phase 3以降を含む脅威は、Phase 0〜2では残余リスクを明記しinterfaceだけを固定する。

| ID | Attack / failure | Mitigation | Residual risk | Phase | 受入 |
|---|---|---|---|---|---|
| TM-001 | scriptがprocess env、cwd、argv、stdio、clockへambient accessする | `CapabilitySet::empty()`を既定とし、全外部builtinをdispatcher経由へ移す | legacy CLI profileは明示的に広い権限を持つ | 2 | TM-AT-01, CAP-AT-01 |
| TM-002 | `..`、absolute path、separator差、case aliasでfilesystem rootを脱出する | normalized relative pathのみ受理し、root-bound handle adapter内で認可とopenを一体化 | OS/filesystem固有alias。secure adapter未対応時は拒否 | 2 | TM-AT-02, CAP-AT-10 |
| TM-003 | check/use間のsymlink差替え、dangling final symlinkで許可外へcreate/writeする | path文字列のcanonicalize→I/Oを廃止。directory/file handleに束縛し、symlink policyをadapterが原子的に強制 | malicious filesystem、契約違反adapter | 2/OS | TM-AT-03, CAP-AT-11 |
| TM-004 | canonicalize、metadata、import error差で許可外pathの存在をoracle化する | capability/grant/rootの拒否をresolver/OS操作前に同一のsanitized denialとして生成し、script操作ではcanonical error、link/control-planeでは`Denied`へ写像する。許可外pathをcanonicalizeしない | 許可済みroot内の存在情報は操作仕様上観測可能 | 2 | TM-AT-04 |
| TM-005 | loop、callback、host call連打でCPUを占有する | fuel hookを全評価・callback経路へ置き、host call costを事前課金 | Phase 2時点のstepは包括的でない。hard CPU limitはOS責任 | 3/OS | TM-AT-05 |
| TM-006 | 巨大文字列、総collection、input/host resultでOOMまたはallocator abortを起こす | 入出力・host resultのbyte上限、将来の総heap budget、上限超過前の拒否 | Rust allocation failureは同一process回復不能の場合がある | 3/OS | TM-AT-06 |
| TM-007 | 深いAST/call/importまたはnative callbackでstackを枯渇させる | 既存AST/call/import深度検査、tree実行thread要件、host callbackの再入禁止 | OS stack overflowはabortし得る。別processが最終防御 | 0/1/OS | TM-AT-07 |
| TM-008 | `input()`、filesystem、resolver、host function、外部serviceが無期限blockする | Phase 3でcallbackへdeadline/cancellationを渡し、adapterに有限timeoutを要求する | 同期v1は実行中syscallをpreemptできない | 3/OS | TM-AT-08 |
| TM-009 | env、file、stdin、host resultのsecretをstdout、error、traceへ漏らす | 必要値だけsnapshot/grant、output別capability、errorにsecret値を含めない | scriptへ正当に渡したsecretの意図的出力は情報フロー制御対象外 | 2 | TM-AT-09 |
| TM-010 | audit eventへsource、path、引数、result、secret、PIIが漏れる | Phase 2でredaction metadataと既定`Omit`を固定し、Phase 6でsink前redactionする | hostがsafe messageへsecretを入れる誤設定 | 2/6 | TM-AT-10 |
| TM-011 | eventをdrop/reorderし、denyまたはhost callを監査から隠す | execution IDと単調sequence、start/terminal必須、sink failure policyを設定 | crash/abort直前のbuffered event消失 | 6 | TM-AT-11 |
| TM-012 | 改竄import、依存crate、host adapter、外部service updateで挙動が変わる | source/import hashとrevision固定、lockfile/署名/reviewはhost責任 | trusted dependencyの未知脆弱性 | 0/1/Host | TM-AT-12 |
| TM-013 | host functionが未grantでも呼ばれる、arity/costを迂回する、core builtinをshadowする | Phase 2でregistry/grant、単一catalog、name/arityを固定し、Phase 3でcostをcallback前課金 | callback内部の隠れたresource消費 | 2/3 | TM-AT-13, CAP-AT-20 |
| TM-014 | host functionがpanic、再入、`exit`、abort、長時間blockする | callbackを`catch_unwind`し`InternalFailure`、同一contextへの再入禁止、契約違反を文書化 | `panic=abort`、FFI UB、process exit/abortは捕捉不能 | 2/OS | TM-AT-14 |
| TM-015 | scriptの`exit()`、runtime bug、panicがhost processを終了する | `exit()`は`Exited` outcome。libraryからprocess exit禁止。catch可能panicを`InternalFailure`へ変換 | OOM/stack/panic=abortは非保証 | 1/2 | TM-AT-15 |
| TM-016 | scriptが`try/catch`でcontrol-plane failureを捕捉して無制限再開する、またはcatch可能なcapability/host errorから権限を得る | script操作中のcanonical capability/host errorはcatch可能だが対象adapter/OS/callbackを呼ばない。budget、deadline、cancel、valid `exit`、panic、audit/record/replay failureとlink/control-planeの`Denied` / `HostError`はcatch不可terminalにする | host functionがpolicy errorを通常値として返す誤実装 | 1/2/3/6 | TM-AT-16 |
| TM-017 | backend差を使い認可・評価順・error・副作用順を変える | treeを規範、VMはexperimental、全security testをbackend pairで実行し差があればVM拒否 | tree自身のbug | 1/5/7 | TM-AT-17 |
| TM-018 | stdout、file、host function resultを大量生成してmemory/disk/logを枯渇させる | capability単位byte予算、N超過は書込み前拒否、auditは値を省略 | filesystem自体のquota、partial OS write | 3/OS | TM-AT-18 |
| TM-019 | import graphを循環・大量・巨大化し、実行前にresourceを消費する | resolver capability、opaque module ID、既存depth制限、source/import byte・module数予算 | Phase 2時点では総source budget未実装 | 2/3 | TM-AT-19 |
| TM-020 | context再利用で前実行のcapability、args、env、import、secretが次実行へ漏れる | contextは言語stateだけ保持。request/capability/inputはhandle所有でterminal時破棄。engine/revision一致検査 | scriptが前実行で明示的にglobalへ保存した通常Value | 1/2 | TM-AT-20 |
| TM-021 | 実行中にgrant/revokeされ、check後にauthorityが変わる | start時freeze。grant/revoke APIなし。取消はtokenで実行全体をcancelし、新しいsetで再実行 | adapter内部の外部ACL変更 | 2 | TM-AT-21 |
| TM-022 | builtin名一覧のdriftによりVMだけruntime fallbackし、認可を迂回する | core builtinとhost callableを単一catalogからparser/compiler/tree/VMへ導出 | generator自体のbug | 2/5 | TM-AT-22 |
| TM-023 | error messageのpath・存在・host内部情報からsecretを得る | public error codeとsafe messageを分離、causeはhost側診断のみ、Deniedは対象情報を含めない | timing side channelは完全には除去しない | 1/2 | TM-AT-23 |
| TM-024 | root sourceは制限外、importだけ制限内等、load経路差でpolicyを迂回する | root sourceはhostが明示供給、全importはlink時にresolver capabilityだけを使う。runtime import禁止 | hostがrootを不安全に読む責任 | 1/2 | TM-AT-24 |

## 11. Phase別受入試験

### Phase 0

- 本文書のactor、asset、trust boundary、保証/非保証、責任分界、全脅威IDがrelease documentationから参照できる。
- 各TM-IDにAttack、Mitigation、Residual risk、Phase、受入IDが1件以上あり、孤立IDがない。
- **TM-AT-07:** 既存のAST深度256/257、import深度128/129、call depth試験がhost abortではなく構造化errorになることを確認する。境界差は[AUD-017](roadmap.md)で追跡する。
- **TM-AT-12（host部分）:** dependency lock、review、OS隔離がTsumugi保証外のhost責任として明記される。

### Phase 1

- **TM-AT-12（engine部分）:** 同じroot bytes、revision、import bytesから同じsource/import graph hashを得て、1 byte変更でhashが変わる。
- **TM-AT-15:** library経路のruntime panicは`InternalFailure`となりhost processが継続する。Phase 2で`exit(0/255)`を`Exited`、範囲外を`RuntimeError`として追加検証する。
- **TM-AT-17:** security関連golden testをtree/VMで比較し、差が1件でもあるVMをstable backendとして公開しない。
- **TM-AT-20（context部分）:** 同じcontextで2実行し、前回requestのargsが次回へ現れず、scriptがglobalへ明示保存した通常Valueだけが残る。
- **TM-AT-23（error部分）:** public `HostError`と`ExecutionError`のDebug/Displayにfake secretが含まれない。
- **TM-AT-24:** compile後・start前に全importがresolver経由でlinkされ、execution中のresolver呼出しは0回。

### Phase 2

- **TM-AT-01:** empty capabilityでenv/clock/stdin/stdout/filesystem/exit/host functionを各1回試し、対象OS/callback呼出し0回のcanonical capability errorになり、未捕捉時は`RuntimeError`となる。importはhandler開始前なのでresolver呼出し0回のterminal `Denied`になる。
- **TM-AT-02:** absolute、`..`、mixed separator、mount prefix衝突を拒否する。
- **TM-AT-03:** deterministic fake adapterでsymlink途中/final/danglingとcreate/write/appendを検証し、root外変更0。secure resolution不能adapterは拒否する。反復race stressはPhase 7。
- **TM-AT-04:** 許可外の存在path/不存在pathでresolver/OS呼出し0回、同じdenial code/public message。
- **TM-AT-09:** secret snapshotを読める実行でも、grantされていないstdout/file/host functionへ流せず、public errorにsecret本文がない。
- **TM-AT-10（metadata部分）:** host functionの`Omit`、`TypeOnly`、`LengthOnly`で値本文を出さず、既定`Omit`。
- **TM-AT-13（grant/arity部分）:** registered-but-not-granted、unknown、arity不一致でcallback呼出し0。cost不足はPhase 3。
- **TM-AT-14:** unwind panicするhost functionでhost processが継続し`InternalFailure`。`panic=abort`は隔離process異常終了という非保証を確認する。
- **TM-AT-16（deny/host部分）:** `try/catch`内のscript操作中capability denialとsanitized host failureはcatch bodyへ入り、対象adapter/OS/callback呼出し0のまま継続できる。link/control-planeの`Denied` / `HostError`、valid `Exited`、panicはcatch bodyへ入らずterminalになる。
- **TM-AT-20（capability部分）:** 前回capability/env/inputが次実行へ現れない。
- **TM-AT-21:** start後に元builderを変更してもset不変で、revoke APIが存在しない。
- **TM-AT-22:** generated callable catalogとtree/VM/compilerの公開名・arity・metadata集合が完全一致。
- **TM-AT-23（denial部分）:** Deniedの存在path/不存在path表現にsecretまたはhost pathがない。

### Phase 3、6、7およびOS隔離

- **TM-AT-05（Phase 3）:** 全loop/function/callback/collection builtin/host callでfuel N-1/Nは完了、N+1は`BudgetExceeded`。超過後callback/副作用0。
- **TM-AT-06（Phase 3/OS）:** heap/source/input/host resultのN-1/N/N+1と、OS memory上限下の隔離process試験。
- **TM-AT-08（Phase 3/OS）:** deadline対応fake callbackは`DeadlineExceeded`。意図的blocking native callbackは同一processでpreempt不能、隔離process timeoutで停止。
- **TM-AT-10（Phase 6）:** sinkへ出た全redaction eventにfake secret本文0。
- **TM-AT-11（Phase 6）:** execution audit sequenceに欠番/重複なし、start/terminal各1、sink failure policyどおり。
- **TM-AT-13（Phase 3）:** host call cost N-1/N成功、N+1はcallback 0のBudgetExceeded。
- **TM-AT-16（Phase 3）:** budget/deadline/runtime cancelがscript catch bodyへ入らずterminal。
- **TM-AT-18（Phase 3/OS）:** stdout/file/host resultのN-1/N byte成功、N+1は追加I/O前拒否。
- **TM-AT-19（Phase 3）:** import source byte、module数、graph総量のN-1/N/N+1。
- **TM-AT-03 stress（Phase 7）:** symlink交換raceを隔離環境で反復しroot外変更0。

## 12. 既存監査項目との関係

- **Phase 0:** 本文書により、[ロードマップ](roadmap.md)のPhase 0にある対象actor、保証境界、host/Tsumugi/OS責任を固定する。実装とOS隔離guideの完了は別途必要である。
- **Phase 1:** terminal outcome、panic/exit、backend、source identityは[組み込みAPI仕様](embedding-api.md)で固定する。
- **Phase 2:** ambient access廃止とpath handle認可は[capability model](capability-model.md)で固定する。
- **AUD-020:** security boundaryではないこと、fail-open legacy、TOCTOU、dangling symlink、存在oracleをTM-002〜004および責任分界へ取り込んだ。現行`canonicalize`検査の文書化だけでは完了とせず、CAP-AT-10〜12合格を実装完了条件とする。
- **AUD-018:** `args()`はprocess argvを読まず、ExecutionRequestのsnapshotだけを読む。CLI引数転送と移行は組み込みAPI仕様のCLI mappingで固定する。
- **AUD-049:** host function追加で名前表を増やさず、単一callable catalogを要求する。TM-022とCAP-AT-20を完了条件とする。

## 13. 変更管理

脅威の削除、保証の緩和、script catch可否、security boundary、責任分界の変更はsecurity-relevant breaking changeである。変更時はthreat IDを再利用せず、新IDを追加し、影響するAPI/capability/受入試験と言語revisionを同一変更で更新する。
