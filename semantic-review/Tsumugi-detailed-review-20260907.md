# Tsumugi 詳細レビュー報告書

- 対象: `Tsumugi-main(20260907-094929).zip`
- 対象スナップショット識別子: ZIPコメント記載 `092da35d0a01c6e3f403123df8416ec0819746d7`
- レビュー日: 2026-09-07
- ZIP SHA-256: `3dcd8dcbd3cec6ccc3f355bdc78807be11d65e9c2b7a47d8ced611d3a2ebc3ce`
- `Cargo.lock` SHA-256: `4afc76e4e4a07a3bcad76bce5797145f1da2d1bae8ab16e70216cd73c4fd82a3`

## 0. 結論

Tsumugiは、**「何を目指すか」と「将来どう設計するか」の成熟度が非常に高い一方、現行実装はその安全境界にまだ到達していない**。特に、マニフェストが最優先する「ホストを止めない」「実行資源を有限化する」「権限を明示付与する」という観点では、現時点の実装を非信頼スクリプトの同一プロセス実行へ用いるべきではない。

一方で、これは文書でも概ね正直に説明されており、単純な「設計漏れ」ではない。今回の重要な発見は次の二群である。

1. **既存文書が一般論として扱っているが、具体的な攻撃・故障経路として未登録の問題**
   - 共有DAG値の比較・文字列化による指数時間／指数出力
   - Unix非UTF-8 canonical pathを使ったimportサンドボックス判定不整合
   - Int–Float比較の精度崩れと等価関係の非推移性
   - 公開されている未検証bytecodeからの無課金後方ジャンプ、無期限実行
   - `list_dir`の部分失敗黙殺とlossyファイル名衝突

2. **検討・設計・実装の状態表示が実態とずれている問題**
   - 言語仕様revision 0.15に対し、READMEは0.14、現行設計文書と次期Embedding APIは0.11
   - 現行仕様と実装は「call予算・深度検査をcallee評価前」としている一方、共有上限定数コメントと次期意味論はcallee先行
   - AUD-024はREPL transactionとして実装済みだが、crosswalkの決定概要は全execution transactionであり、`✅ 完了`表示が範囲を誤認させる
   - 現行仕様はtree/VM差なしとする一方、README・設計文書・LANG_GUIDEはAUD-024差が残ると記載

### 最優先の判断

現行Tsumugiを「制御可能な組み込み言語」として次段階へ進めるなら、機能追加より先に以下を完了させるべきである。

1. 値グラフの比較・表示・serializeを、反復処理・visited管理・fuel／bytes／depth上限付きにする。
2. filesystem認可APIを`String`から`Path`／認可済みhandleへ変更し、認可した対象以外をI/Oしない。
3. `Chunk`を封印し、`Vm`は検証済みbytecodeだけを受け取る。後方制御移動とcall protocolを検証する。
4. `exit()`、stdio、argv、env、clockをprocess-globalからExecutionRequest注入へ移す。
5. `ExecutionContext`から実行ごとのmeterを分離し、全stable executionへtransactionを適用する。
6. revision・実装状態・AUD状態を機械可読な単一正本から生成する。

---

## 1. レビュー範囲と方法

### 1.1 対象

- Rust production source: 21ファイル
- Rust integration test source: 6ファイル
- `tests/`配下総ファイル: 170ファイル（Rust test 6、fixture等164）
- `#[test]`出現数: 241
- Markdown: 13ファイル
- 主な対象領域:
  - Lexer / Parser / AST
  - tree evaluator
  - bytecode compiler / VM
  - scope / closure / transaction
  - import / filesystem sandbox
  - builtin / process-global I/O
  - embedding facade
  - language specification / manifesto / roadmap / threat model / capability / budget / audit / release design

### 1.2 実施内容

- production sourceの全体読解と、危険API・再帰・unchecked cast・ambient state・I/O・panic経路の横断検索
- 現行仕様、次期仕様、設計履歴、ロードマップ、実装、テストの相互照合
- 公開APIから不正入力を与えられる境界の確認
- 資源量について、物理allocation数だけでなく論理展開量・共有グラフ・再帰深度を確認
- ローカル相対Markdownリンクの整合性確認（破損リンク0件）

### 1.3 制約

この環境には`rustc`／`cargo`がなく、外部ネットワークの名前解決も遮断されていたため、Rust toolchainを追加できなかった。したがって、次は未実施である。

- `cargo build`
- `cargo test`
- `cargo clippy`
- 実バイナリによる再現
- sanitizer / fuzz / memory計測

本報告で「確認」とするものは、明示しない限り**コード経路を静的に追跡して成立を確認したもの**である。OS依存項目にはその旨を記載した。既存文書内に過去のテスト成功記録はあるが、今回のスナップショットについて独立には再検証していない。

### 1.4 優先度

| 優先度 | 意味 |
|---|---|
| P0 | ホスト停止、権限境界逸脱、非信頼実行不可、または組み込み公開の阻害要因。次の安全性リリース前に必須 |
| P1 | 誤結果、状態破損、公開API不変条件違反、重大な文書・実装齟齬。近いマイルストーンで必須 |
| P2 | 特定条件の不整合、移植性・決定性・運用性リスク。計画的に修正 |
| P3 | 診断品質、将来保守性、低頻度境界。バックログ化 |

### 1.5 文書状態の記号

| 記号 | 意味 |
|---|---|
| ◎ | 問題と具体的な契約・受入基準まで十分に記載 |
| ○ | 一般的な問題または方針は記載されている |
| △ | 関連記載はあるが、今回の具体的経路・完了範囲が不足 |
| × | 実質的に未記載、または記載が実装と矛盾 |

---

## 2. 総合評価

| 評価軸 | 評価 | コメント |
|---|---:|---|
| ミッション・価値基準 | 非常に良い | `docs/manifesto.md:24-100`は、安定性、capability、有限budget、決定性、failure outcome、audit、小さいcoreを一貫して定義 |
| 次期設計の網羅性 | 非常に良い | capability、budget、state machine、audit、releaseまで、通常の個人言語処理系を大きく超える具体性 |
| 現行意味論の明文化 | 良い | `language-spec.md`は現行観測仕様として詳細。ただしrevision・差分状態のdriftあり |
| 現行のホスト安定性 | 不十分 | 普通のscriptから指数的比較・文字列化、無制限source/string/I/O、`exit()`が到達可能 |
| 現行の権限分離 | 不十分 | ambient env/argv/stdin/stdout/clock/filesystem。allow-listも実行単位でなくprocess-global |
| 現行の資源制御 | 不十分 | loop/call stepとcollection cardinalityのみ。DAG traversal、string、source、I/O、heapを包括しない |
| tree/VM整合性 | 改善中 | paired testと統一作業は強い。意味実装は二重で、文書の差分表示が追随していない |
| 公開API境界 | 不十分 | 高水準facadeは小さいが、内部moduleとraw `Chunk`/`Vm`も公開され、不変条件を外部が破れる |
| テスト設計 | 良い | golden、paired、defensive、scaling、3 OS CIがある。今回実行不能、fuzz/stress/release gateは未実装 |
| 文書→実装トレーサビリティ | 要改善 | AUD crosswalkは有益だが、完了scope、revision、現行／次期の状態が再度driftしている |

---

## 3. 指摘一覧

| ID | 優先度 | 現在／将来 | 到達範囲 | 検討 | 設計 | 実装 | 概要 |
|---|---:|---|---|:---:|:---:|:---:|---|
| REV-001 | P0 | 現在 | 通常script | ○ | ○ | × | 共有DAGの構造比較・表示・join・sortが指数時間／指数出力になる |
| REV-002 | P0 | 現在 | Unix + import | △ | ○ | × | 非UTF-8 canonical pathが空文字へ変換され、sandbox判定対象とI/O対象が分離する |
| REV-003 | P1 | 現在 | 通常script | × | × | × | Int–Float比較が2^53超で誤り、`==`が非推移的になる |
| REV-004 | P1 | 現在 | 公開low-level API | △ | △ | × | 公開`Chunk::patch_jump`が範囲外／非jump offsetでpanicする |
| REV-005 | P1 | 現在 | 公開raw bytecode | △ | △ | × | 不正`MakeClosure` descriptorをNull captureとして黙認する |
| REV-006 | P0 | 現在 | 公開raw bytecode | △ | ○ | × | `Jump(0)`等でstep課金を迂回し無期限実行できる。raw `Call`もcall課金を迂回 |
| REV-007 | P1 | 現在 | stable facade | △ | ◎ | × | `ExecutionContext`がsession stateとrun meterを混在し、`execute`間でstepを累積する |
| REV-008 | P1 | 現在／目標差 | stable facade | △ | ◎ | △ | transactionはREPL限定。通常`Engine::execute`はエラー前のstate mutationを保持 |
| REV-009 | P2 | 現在 | filesystem | × | △ | × | `list_dir`がentry errorを黙殺し、非UTF-8名をlossy変換して衝突させる |
| REV-010 | P2 | 現在 | 32-bit target | △ | ○ | × | `i64 as usize`でslice/rangeがwrapし、limit迂回・巨大collectを起こし得る |
| REV-011 | P1 | 現在 | 文書・release | ○ | ◎ | × | language revision、engine差、実装statusが複数文書でdrift |
| REV-012 | P1 | 現在／移行 | call semantics | ○ | × | △ | call順序が現行仕様・実装と次期仕様・code commentで矛盾し、完了表示も不整合 |
| REV-013 | P1 | 現在 | embedding | ◎ | ◎ | × | `args()`がhost process argvを読み、tree/VMで解析規則も異なる |
| REV-014 | P1 | 現在／multi-tenant | embedding | ◎ | ◎ | × | sandbox/env/limits/stdio/clockがprocess-globalまたはfirst-use global |
| REV-015 | P0 | 現在／既知 | 通常script | ◎ | ◎ | × | source、string、heap、builtin work、I/Oが包括的に有限化されていない |
| REV-016 | P2 | 現在／将来 | 長寿命context | ◎ | ○ | × | scriptからRc cycleを作れ、context再利用で回収不能heapが累積する |
| REV-017 | P2 | 現在／決定性 | 通常script | × | △ | × | human Displayが非escapeで、sort key・repr・outputが同じ表現へ結合されている |
| REV-018 | P1 | 現在 | embedding API | ○ | ◎ | × | internal module／raw bytecodeの公開が安全境界とstable surfaceを弱める |
| REV-019 | P3 | 現在 | clock | △ | ◎ | × | `now()`がepoch前／clock errorを0にし、`u64 as i64`も未検査 |
| REV-020 | P2 | 現在 | import diagnostics | ◎ | ○ | × | import先parse errorの原因を捨て、wrapper messageだけを返す |
| REV-021 | P1 | 将来移行 | filesystem capability | × | × | × | 現行`remove_dir`は再帰削除だが次期capabilityは`EmptyDirectory`へ割当て |
| REV-022 | P2 | 現在／移行 | I/O API | ◎ | ◎ | × | EOF・I/O error・permission等をNull/falseへ畳み、原因とdenialを区別できない |
| REV-023 | P0 | 現在／既知 | embedding | ◎ | ◎ | × | script `exit()`がホストプロセスを終了する |
| REV-024 | P2 | 将来 | CI／release | ◎ | ◎ | △ | 現行CIはrolling stable・mutable action・`--locked`なし・fuzz/stress/MSRVなし |
| REV-025 | P2 | 現在／将来 | compile | × | △ | × | source/token制限に加え、parse diagnostic件数にも上限がない |

---

## 4. 重要指摘の詳細

## REV-001 — 共有DAGの比較・表示による指数時間／指数出力

**優先度:** P0
**確度:** 高。通常scriptから到達するコード経路を静的に確認。
**影響:** CPU占有、巨大allocation、stdout膨張、stack overflow、ホスト応答停止。

### 現状

`Value`のList/Dict等価比較は再帰的なRustの構造比較へ委譲している。

- `src/value.rs:78-117`
- Listは`Rc::ptr_eq`で同じrootだけ短絡し、異なるroot間の共有subgraphは記録しない: `src/value.rs:94-96`
- Displayは子要素を再帰的に`String`化し、各階層で`Vec<String>`と`join`を作る: `src/value.rs:166-203`
- `contains`: `src/builtin_core.rs:353-378`
- `sort`のkey: `src/builtin_core.rs:381-386`
- `join`: `src/builtin_core.rs:457-466`
- `to_str`: `src/builtin_core.rs:536-539`
- tree `print`: `src/builtin.rs:175-181`
- VM `Print`: `src/vm.rs:1110-1118`

TsumugiのListは`Rc`共有を持つため、少数の物理nodeから非常に大きい論理木を作れる。次の例は各反復で2要素Listを1個作るだけだが、`a == b`はほぼ`2^60`個のpair pathを辿る。

```tsg
let a = [0]
let b = [0]
for i in range(0, 60)
    a = [a, a]
    b = [b, b]
end
print(a == b)
```

`print(a)`や`to_str(a)`は論理展開を文字列化するため、指数的な出力／中間文字列を作る。collection size上限は各Listの要素数2しか見ないため防止できない。現在のstepはloop反復と関数・callback呼び出し中心で、比較・表示の要素走査を課金しない（`src/eval.rs:37-40,78-90`）。

また、共有なしでも1要素Listを数千段入れ子にすると、Rust再帰によるDisplay／equalityがAST深度制限とは独立にnative stackを消費する。

### 文書状況

- **検討:** ○。`language-spec.md:921-942`は重いbuiltin、文字列、総heapが未制限と明記。
- **設計:** ○。`execution-control.md:242-258`はcollection走査・serializeの要素課金、bytes課金、chunk checkpointを規定。`293-304`はAllocationIdと反復worklistを規定。
- **不足:** 共有DAGによる論理展開増幅、pair memoization、renderer depth/node/output上限、sort keyとの分離が明示されていない。
- **実装:** ×。

### 改善仕様

1. すべてのValue graph traversalに、少なくとも以下を適用する。
   - `max_traversal_nodes`
   - `max_traversal_depth`
   - fuel per visited edge／element
   - output byte予約
   - cancellation／deadline checkpoint
2. equalityは「同じ共有構造か」ではなく現行どおり構造的値比較を保ちつつ、同一execution中に同じnode pairを再比較しない。
3. 表示は次の3用途へ分ける。
   - `HumanDisplay`: 人向け、必ずbounded
   - `CanonicalRepr`: escape済み・決定的・監査可能
   - `TotalOrderKey`: sort専用。表示変更で順序が変わらない
4. budget超過はcatch不能な`BudgetExceeded`とし、巨大中間`String`を作る前に停止する。

### 設計案

- heap objectへ次期設計の`AllocationId`を付与。
- equalityは再帰でなくworklistを使い、`HashSet<(AllocationId, AllocationId)>`をvisited pairとする。
- rendererは`String`を子ごとに生成せず、bounded writerへstreamingする。
- 1 chunkを最大256 elementまたは16 KiBとし、chunk前にcontrol check。
- sortはkey全文を事前生成しない。明示的なtype rank + value comparatorを使うか、bounded canonical keyをcacheする。

### 実装案

```text
compare_values(left, right, control):
  work = [(left, right, depth=0)]
  seen_pairs = set()
  while work not empty:
    control.charge(Fuel::CompareNode(1))
    check depth
    pop pair
    scalarならexact compare
    collectionならAllocationId pairをseenへ追加
    未訪問なら対応する子pairをworkへ積む
```

`Display for Value`を安全境界に使わず、`ValueRenderer::render(value, &mut BudgetedWriter, mode)`を唯一の言語表示APIにする。RustのDebugも巨大値を無制限展開しない。

### 受入テスト

- DAG depth 20/31/32/60のequal、末尾だけdifferent
- 1要素List 10,000段
- output budget N／N+1
- sort keyに巨大DAGを含むList
- tree／VM完全一致
- subprocess timeoutとRSS上限下で、panic／abort／OOM／hangなし
- budget超過時にstdout write callが契約どおり0または許容済みprefixのみ

---

## REV-002 — 非UTF-8 canonical import pathによるsandbox認可対象のすり替わり

**優先度:** P0
**確度:** 高。Unix依存。条件付きの認可迂回経路を静的に確認。
**影響:** `TSUMUGI_SANDBOX`外のimport source読取り。

### 現状

`ModuleLoader::resolve`は対象をcanonicalizeした後、`PathBuf`をUTF-8 `&str`へ変換してsandboxへ渡す。

- canonicalize: `src/module.rs:130-139`
- `canonical.to_str().unwrap_or("")`: `src/module.rs:141-142`
- 実I/Oは元の`canonical`: `src/module.rs:157-166`

Unixではpath componentは任意byte列を持てる。canonical pathに非UTF-8 componentがあれば`to_str()`は`None`となり、sandboxには空文字が渡る。`check_path("")`は空pathをCWDへ絶対化する。

- `check_path`: `src/sandbox.rs:51-53`
- 相対pathのCWD結合: `src/sandbox.rs:81-95`

したがって、CWDが許可root内にあり、許可root内のUTF-8名symlinkがsandbox外の非UTF-8 pathへ向く場合、認可はCWDに対して成功し、実際にはsandbox外のcanonical pathを読み得る。

### 文書状況

- **検討:** △。一般的なsymlink、TOCTOU、dangling link、path oracleは`language-spec.md:945-955`、`threat-model.md:139-141`で扱う。
- **設計:** ○。次期path-handle方式はこの種の問題を根本的に避ける。
- **不足:** OS path→UTF-8変換失敗時の認可対象置換は未記載。
- **実装:** ×。

### 改善仕様

- OS filesystem pathを認可する層では`str`へ変換しない。
- 認可APIは`&Path`を受け、認可済み対象をopaque型で返す。
- callerは返された認可済み対象以外をI/Oしてはならない。
- error表示用文字列は認可対象と分離し、lossy表示しても認可判断へ使わない。
- 次期handle方式では、root-bound directory handleから相対componentを辿り、認可とopenを同じadapter operationへ統合する。

### 実装案

```rust
pub(crate) struct AuthorizedPath(PathBuf);

pub(crate) fn authorize_existing_path(
    path: &Path,
    operation: FsOperation,
    line: usize,
) -> Result<AuthorizedPath, TsumugiError>;
```

`ModuleLoader`は`AuthorizedPath`を受け取り、その内部pathでだけ`read_to_string`する。より安全には`AuthorizedFile`を返し、pathを再利用しない。

### 受入テスト

- Unix `OsStringExt::from_vec`で非UTF-8 directory/fileを作成
- 許可root内symlink→許可外非UTF-8 target
- CWDが許可root内／外の両方
- importが必ずdenyされ、外部file内容・存在差がscriptへ漏れない
- testはprocess-global OnceLockの影響を避けるためchild processで実行

---

## REV-003 — Int–Float比較の精度崩れと非推移的等価性

**優先度:** P1
**確度:** 高。IEEE 754 binary64とコード上のcastから確定。
**影響:** 条件分岐、contains、min/max、検証ルール、業務数値判定の誤り。

### 現状

Int–Float比較はIntを`f64`へcastする。

- equality: `src/value.rs:86-90`
- tree relational: `src/eval.rs:654-661`付近
- VM relational: `src/vm.rs:1762-1812`
- `min`/`max`: `src/builtin_core.rs:586-616`

binary64は2^53を超えるすべての整数を区別できない。例えば次が成立する。

```tsg
let a = 9007199254740992
let b = 9007199254740993
let f = 9007199254740992.0
print(a == f)  # true
print(b == f)  # 現実装ではtrue
print(a == b)  # false
print(b > f)   # 現実装ではfalse
```

この結果、`a == f`かつ`f == b`だが`a != b`となり、等価関係が推移的でなくなる。`contains`も`Value::PartialEq`を使うため影響する。`max(b, f)`もIntをFloatへ変換し、正確な`b`を失う。

### 文書状況

- `language-spec.md:312-330`は「IntとFloatを数値として比較」とするが、精度境界・変換規則を定義していない。
- AUD-014の486ケースは型の組合せを網羅したが、2^53等の値境界は対象外だった（`roadmap.md:62`）。
- **検討／設計:** ×に近い。
- **実装:** ×。

### 改善仕様

- Int–Float比較は整数をFloatへ丸めず、Floatのbit表現を分解して数学的に正確に比較する。
- NaNの大小比較はすべてfalse、等価もfalseという現行IEEE方針を維持。
- ±Infinityはすべての有限Intより大小が確定。
- `min`/`max`は比較後に**選択された元のoperandをその型のまま返す**。同値時に第1引数を返す等、tie ruleを固定する。
- NaNを含む`min/max`の規則を明文化する。推奨はNaN入力があればcanonical NaNを返す。

### 設計・実装案

共通moduleに1つだけ実装する。

```text
NumericCmp::compare(&Value, &Value) -> Result<NumericOrdering, TypeError>
NumericOrdering = Less | Equal | Greater | UnorderedNaN
```

`i64`対`f64`は`f64::to_bits()`からsign、exponent、significandを取り出し、整数側を`u128` magnitudeへ変換してexact compareする。tree、VM、PartialEq、relational、contains、min/maxが全て同じhelperを使う。

### 受入テスト

- `±(2^53-1)`, `±2^53`, `±(2^53+1)`
- `i64::MIN`, `i64::MAX`
- 各整数近傍の`next_up`／`next_down`相当Float
- `-0.0`, `0.0`, NaN, ±Infinity
- equalityの対称性・推移性property
- comparatorの反対称性
- tree／VM／builtin共有

---

## REV-004 — 公開`Chunk::patch_jump`のpanic

**優先度:** P1
**到達範囲:** 通常scriptからは直接不可。公開low-level Rust API利用者から到達。
**確度:** 高。

`Chunk`のfieldとbuilder操作は公開されている（`src/chunk.rs:8-20`）。`patch_jump`は`self.code[offset]`を無検査indexし、非jump opcodeなら明示的`panic!`する（`src/chunk.rs:63-74`）。`lib.rs:12-29`でmodule自体も公開される。

`tests/defensive_vm.rs:1-7`は「公開APIへ任意Chunkを渡してもhost panicしない」を明示しているが、構築中の公開APIは同じ安全方針を満たしていない。

### 文書状況

- 不正ChunkのVM実行は十分検討済み。
- `patch_jump`自身は未記載。
- internal moduleの安定性を保証しない旨は`lib.rs:3-6`にあるが、panic許容契約とは別問題。

### 改善案

第一選択:

- `ChunkBuilder`を`pub(crate)`にする。
- 公開可能な`Chunk`はimmutableかつverifiedにする。
- raw builderは`test-support`または`unstable-bytecode` featureでのみ公開。

最小修正:

```rust
pub(crate) fn patch_jump(&mut self, offset: usize) -> Result<(), ChunkBuildError> {
    let op = self.code.get_mut(offset).ok_or(ChunkBuildError::BadOffset)?;
    // jump variantでなければErr
}
```

### 受入テスト

- offset == len、usize::MAX、通常opcode、すべてのjump variant
- `catch_unwind`でpanicなし
- builder error時にcode/linesが部分破損しない

---

## REV-005 — 不正`MakeClosure` descriptorのNull capture黙認

**優先度:** P1
**到達範囲:** 公開raw bytecode。
**確度:** 高。

`MakeClosure(N)`は直前N命令が`GetLocal`／`GetUpvalue`であることを想定する。しかし別opcodeの場合、errorではなく`(true, usize::MAX)`を格納し、その後Null cellへ置換する。

- descriptor解析: `src/vm.rs:971-1001`
- Null fallback: `src/vm.rs:1008-1017`

このため、例えば`LoadConst(VmFn) → LoadConst(Int) → MakeClosure(1)`という不正列が、壊れたcaptureを持つclosureとして成功し得る。既存defensive testはfunction先頭のunderflowを検査するが（`tests/defensive_vm.rs:110-139`）、descriptor opcode不正は対象外。

### 改善仕様・設計

- descriptor不正は即時`InternalFailure`。
- より良い設計はcapture metadataを隣接opcode列から逆算しないこと。

```rust
struct FunctionPrototype {
    chunk: Rc<VerifiedChunk>,
    captures: Box<[CaptureDesc]>,
}

enum CaptureDesc { Local(u32), Upvalue(u32) }
```

`MakeClosure`はprototype indexだけを持ち、verifierがlocal/upvalue範囲を検証する。

### 受入テスト

- 直前命令不足、別opcode、descriptor順序不正、local/upvalue範囲外
- 0 capture、multi-level captureの正常系
- 不正列が成功／Null補完されない

---

## REV-006 — 未検証bytecodeによるmeter迂回と無期限実行

**優先度:** P0
**到達範囲:** `Vm::new(Chunk)`／`run_repl_chunk`利用者。通常compiler出力からは原則不可。
**確度:** 高。

`Vm::new`は任意の公開`Chunk`を受け取る（`src/vm.rs:122-141`）。通常の後方ジャンプは`Loop` opcodeで、ここだけstepを数える（`src/vm.rs:953-955`）。一方、汎用`Jump`は方向を検査せず、単にIPを設定する（`src/vm.rs:920-922`）。したがって次のbytecodeはstep上限に一度も触れず無期限実行する。

```text
0: Jump(0)
```

同様に、compilerはcall前に`PrepareCall`をemitする（`src/compiler.rs:821-843`）が、raw `Call`は`PrepareCall`がなくても実行できる。depthは防御的に再検査するが、stepは「PrepareCallだけで数える」として省略される（`src/vm.rs:1052-1075`）。

これは「各opcodeのpanic防止」だけでは解決しない。**bytecodeがcompiler不変条件を満たすかの検証がないこと**が根因である。

### 文書状況

- `tests/defensive_vm.rs`と`docs/design.md:823-827`は任意Chunkのpanicを扱う。
- step／fuel設計は詳細。
- 後方`Jump`、call protocol、control-flow graph検証は明示されていない。

### 改善仕様

- public VMは`Chunk`でなく`VerifiedChunk`だけを受理する。
- すべてのback edgeは課金可能なopcode／basic blockへ入る。
- runtime defenseとして、verifier済みでもIPが現在値以下へ移る場合は必ずcontrol checkする方法も検討する。
- callはprotocol stateを持ち、`PrepareCall → callee → ValidateCall → args → Call`以外を拒否する。

### verifier要件

1. `code.len() == lines.len()`
2. constant／local／upvalue index範囲
3. jump target範囲
4. `Jump`系の方向制約。back edgeは`Loop`または`Charge`付きblockだけ
5. stack height dataflowとjoin point整合
6. exception handlerのnesting
7. closure capture descriptor
8. call protocol
9. function entry／return／fallthrough
10. internal-only opcodeのsource artifact混入禁止

### 実装案

```rust
pub struct VerifiedChunk {
    chunk: Chunk,
    verification_revision: BytecodeVerificationRevision,
    _sealed: private::Seal,
}

impl Vm {
    pub fn new(chunk: VerifiedChunk, request: ExecutionRequest) -> Self;
}
```

runtimeの既存bounds checkはdefense-in-depthとして残す。raw builder／unchecked constructorはtest-onlyにする。

### 受入テスト

- `Jump(0)`, conditional back edge, cycle with multiple blocks
- raw `Call` without prepare／validate
- inconsistent stack height at merge
- malformed try handler／closure descriptor
- verifierは有限時間・有限memoryで失敗
- verifierを通ったchunkは全back edgeでfuel減少

---

## REV-007 — session stateとper-run meterの混在

**優先度:** P1
**到達範囲:** crate rootの現行embedding facade。
**確度:** 高。

`ExecutionContext`はEvaluatorを保持し（`src/engine.rs:82-104`）、Evaluatorは変数／module stateと同時に`steps`／`max_steps`を保持する（`src/eval.rs:33-56`）。`Engine::execute`はmeterをresetせず、そのまま`run`する（`src/engine.rs:44-50`）。resetはcallerが手動で呼ぶAPIで、READMEもそのように案内する（`README.md:45`）。

したがって同じcontextで独立した2 executionを行うと、2回目は1回目の消費分を引き継ぐ。これは「session累積quota」として明示されたものではなく、API利用者がresetを忘れると挙動が変わるfootgunである。

### 文書状況

- 現行挙動はREADMEに記載されているため隠れた挙動ではない。
- 次期設計は`ExecutionRequest`が有限`BudgetConfig`を所有する（`execution-control.md:175-195`）ため、設計は十分。
- 実装未反映。

### 改善仕様・設計

- `ExecutionContext`: commit済みlanguage/session stateだけを所有。
- `ExecutionRequest`: capability、arguments、budget、cancellation、auditを所有。
- `ExecutionMeter`: request作成ごとに必ず新規。
- session全体quotaが必要なら、`EngineAdmissionBudget`等の別概念で明示する。
- `reset_step_budget`は廃止予定／deprecatedにする。

### 受入テスト

- 同一contextで同じscriptを2回実行し、各回が全budgetを持つ
- 1回目failure後に2回目へmeter／deadline／cancelを持ち越さない
- session aggregate quotaを設定した場合だけ累積する

---

## REV-008 — stable `Engine::execute`がtransactionでない

**優先度:** P1
**種別:** 現行仕様違反ではなく、目標・次期契約とのギャップ。
**確度:** 高。

`Evaluator::run`はerror時にmodule markerを戻すだけで、bindingやcell mutationをrollbackしない（`src/eval.rs:98-110`）。journalを開始するのは`run_repl_submission`だけ（`src/eval.rs:113-143`）。`Engine::execute`は通常`run`を呼ぶ（`src/engine.rs:44-50`）。

例として、同じcontextで`let x = 0`を成功させた後、`x = 1`の後にゼロ除算するscriptを`execute`すると、error後も`x == 1`が残る。

現行`language-spec.md:265`はREPLだけをrollback対象とするため、現行規範には合っている。しかし次期Embedding APIは`Completed`／`Exited`以外の全terminalをexecution開始時点までrollbackすると明記する（`embedding-api.md:444-463`、`execution-control.md:735-742`）。

`roadmap.md:242`は決定概要を全execution transactionとして記載しつつ、実装欄をREPL実装で`✅ 完了`としている。括弧内で範囲は説明されているが、完了表示と決定概要のscopeが一致しない。

### 改善仕様

- stable Engine経由の全executionはtransaction必須。
- `Completed`／`Exited`のみcommit。
- RuntimeError、Denied、HostError、BudgetExceeded、Deadline、Cancelled、AuditFailure、InternalFailureはrollback。
- rollback failureはcontextをpoisonし、再利用させない。
- external effectは戻せないことをoutcome／auditで明示。

### 設計・実装案

`ExecutionTransaction`をtree／VM共通の抽象へする。

```text
begin(context)
  link/start stateをcheckpoint
  first-write journal開始
run
  terminal policyでcommitまたはrollback
  module/compiler/VM/envを同じ境界で終了
```

REPL専用methodはこの共通transactionを呼ぶthin adapterへ縮小する。

### 受入テスト

- crate root `Engine` APIから、binding、cell、List/Dict、closure upvalue、import markerを各々変更後failure
- catch済みerror後Completedはcommit
- external fake effectは残り、`host_effects_may_remain=true`
- rollback失敗fault injectionでpoison

---

## REV-009 — `list_dir`の部分失敗黙殺とlossy name衝突

**優先度:** P2
**確度:** 高。OS上でentry iterator errorを起こす条件は環境依存。非UTF-8名はUnixで再現可能。

`builtin_list_dir`は`entries.flatten()`を使うため、directoryの列挙開始後に個別entry取得が失敗しても、そのentryを無言で欠落させて成功Listを返す（`src/builtin_core.rs:1000-1015`）。また`file_name().to_string_lossy()`により、異なるbyte名が同じ置換文字列へ写像され得る。

問題は次の二つである。

1. 完全なsnapshotに見えるが実際は部分結果であり、業務処理が「存在しない」と誤認する。
2. 異なるentryが同名に見え、後続のpath操作・audit・deterministic replayでidentityが崩れる。

### 文書状況

- 次期`DirectoryEntry.name`は検証済みStringとする（`capability-model.md:362-366,456`）。
- 個別entry error時のatomicityと、invalid UTF-8 nameの扱いは明示不足。

### 改善仕様

- 安全profileでは個別entry errorが1件でもあればlisting全体をstructured HostErrorにする。
- 部分結果が必要なら`DirectoryListing { entries, incomplete, errors }`のように明示し、通常Listへ偽装しない。
- path identityにlossy Stringを使わない。次期APIでStringしか許さないなら、非UTF-8 entryは明示的HostErrorにするか、opaque entry ID／Bytes型を設計する。
- 列挙はsnapshot、ordering、件数／bytes budgetをadapter契約に含める。

### 受入テスト

- Unixの異なるinvalid-byte名が同じlossy文字列になるcase
- adapterで2件目だけentry errorを注入
- partial successを成功Listとして返さない
- orderingとbudget N／N+1

---

## REV-010 — 32-bitでのunchecked `i64 → usize`変換

**優先度:** P2
**確度:** 高。32-bit target依存。

- `slice`: `src/builtin_core.rs:324-326`
- `range`: `src/builtin_core.rs:427-438`

非負の大きな`i64`を`usize`へ`as`変換すると、32-bit targetでは下位bitへtruncateされる。例えば`range(0, 4294967296)`のcandidate sizeが0へwrapすればcollection limitを通過し、その後の`collect()`は約43億要素を作ろうとする。slice indexも意図しない小さい値へwrapする。

### 文書状況

- 次期budget受入は`usize`変換・overflowを拒否するとしている（`execution-control.md:719-721`）。
- AUD-036は別のlossy castを扱うが、この2箇所は明示されていない。

### 改善案

- `usize::try_from`とchecked arithmeticを使用。
- sliceはまず`i64`／`u64`領域で0・lengthへclampしてから、安全に`usize`へ変換。
- rangeはcandidate countを`u128`等で計算し、collection limitと`usize::MAX`の両方を先に検査。
- 64-bit targetだけを正式対応とするなら、Cargo／README／release matrixで明示し、compile-time guardを置く。それでもlibrary helperのchecked conversionは残す。

### 受入テスト

- `usize::MAX`境界をtarget非依存helperへ切り出し、模擬32-bit上限でunit test
- 0、負数、`u32::MAX`、`u32::MAX+1`、`i64::MAX`
- allocation開始前にerror

---

## REV-011 — revision・engine差・実装statusの文書drift

**優先度:** P1
**確度:** 高。

### 確認した齟齬

1. 現行言語仕様は0.15: `docs/language-spec.md:3`
2. READMEは0.14と記載: `README.md:22`
3. 現行設計文書は0.11と記載: `docs/design.md:347`
4. 次期Embedding APIの型定義は`V0_11`／CURRENT 0.11: `docs/embedding-api.md:36-44`
5. 現行仕様は「両engine差: 現在なし」: `docs/language-spec.md:15`
6. READMEはAUD-024のREPL差が残ると記載: `README.md:20`
7. 設計文書も差が残ると記載: `docs/design.md:511,728,1025`
8. LANG_GUIDEも差が残ると記載: `LANG_GUIDE.md:200`
9. 実装はtree／VM双方にREPL transaction codeがあり、roadmap／semantic-decisionsは完了としている。
10. `semantic-decisions.md:5`は文書全体を「次期仕様確定・未実装」と表示するが、同文書内には実装完了した節が複数ある。

AUD-044は過去にもrevision driftを修正済みとしている（`roadmap.md:95`）ため、手作業同期では再発を防げていない。

### 影響

- hostが`LanguageRevision::CURRENT`を実装すると旧revisionを固定する。
- 利用者がどの意味論へ依存できるか判断できない。
- release note／audit metadata／replay compatibilityが誤る。
- 「完了済みか未実装か」の判定が文書によって変わる。

### 改善仕様・設計

機械可読な単一正本を導入する。

```toml
# project-metadata.toml
package_version = "0.1.0"
current_language_revision = "0.15"
next_language_revision = "0.16-draft"
current_backend_deviations = []

[audit.AUD-024]
design = "accepted"
implementation = "repl-complete"
stable_execution = "not-implemented"
```

- Rustの`LanguageRevision`、README snippet、design header、release metadataを生成。
- 文書全体に単一statusを置かず、節またはdecision ID単位で`planned / designed / partial / implemented / verified / released`を持つ。
- CIで旧revision literal、矛盾したAUD状態、known deviation集合を検査。
- `implemented`はcodeだけ、`verified`は受入test通過、`released`は公開artifactというように分離。

### 受入条件

- revision literalを1箇所変更すると生成物以外の手修正不要
- README／spec／Rust API／audit event／release manifestが一致
- `language-spec`のknown deviation集合とroadmapのopen deviation集合が完全一致
- stale literalを意図的に入れるnegative testでCI失敗

---

## REV-012 — user call順序の現行／次期矛盾と完了判定

**優先度:** P1
**種別:** 現行runtimeのバグではなく、migration・statusの不整合。
**確度:** 高。

### 現行の正しい組合せ

現行仕様は次の順序を規定する。

1. step／call depth
2. callee評価
3. callable／arity
4. argument評価
5. body

根拠:

- `docs/language-spec.md:465-475`
- tree: `src/eval.rs:715-729`
- compiler: `src/compiler.rs:821-843`
- VM: `src/vm.rs:1052-1075`
- 既存回帰テストもcallee評価前のlimitを固定している。

### 矛盾する記載

- `src/limits.rs:16-18`は「動的calleeを先に評価・分類し、user callableの場合だけ深度検査」と説明。
- `docs/semantic-decisions.md:165-170,290-292,327`もcallee先行を次期契約とする。
- `roadmap.md:248,253`はAUD-017／050を完了扱い。

次期仕様が意図的な破壊的変更であること自体は問題ではない。問題は、共有定数のdoc commentが現行実装を説明する場所で次期挙動を断定し、roadmapの完了表示からは「境界値だけ完了、error precedence変更は未実装」と読み取りにくいことである。

### 改善案

推奨する最終意味論は次期設計どおりでよい。

```text
calleeを一度だけ評価
→ callable分類
→ user callableだけstep/depth検査
→ arity
→ args左から右
→ frame/body
```

理由はbuiltin／host functionをuser depthで拒否せず、動的calleeの種類を確定してから適切なbudgetを選べるため。ただしcalleeの副作用がlimit error前に発生する破壊的変更になる。

実施時は:

- `LanguageRevision`を上げる。
- tree／compiler／VM／callbackを同時変更。
- `limits.rs` commentは「current」と「target」を混在させない。
- roadmapを`depth-counting=verified`、`callee-precedence=planned`の別sub-statusへ分ける。

### 受入テスト

- calleeにcounter更新を持つ式
- budget 0／depth 128でcallee副作用の有無
- non-callable／wrong arityでargument非評価
- builtin／host function／callbackの分類
- tree／VMのstdout、state、error kind/message/line完全一致

---

## REV-013 — `args()`がhost process argvを参照し、backend間でも解釈が異なる

**優先度:** P1
**確度:** 高。

- tree: `std::env::args_os().skip(2)`: `src/builtin.rs:204-214`
- VM: binaryをskipし、全`--vm`をfilter後にscript pathをskip: `src/vm.rs:1528-1539`
- 現行CLIはscript path以外の引数を拒否: `src/main.rs:65-89`

CLIからは仕様どおり通常空Listになるが、library embeddingではTsumugi scriptと無関係なhost process argvを読み得る。hostがcredential、endpoint、tenant ID等をargvに置く運用なら情報漏えいになる。treeとVMで`--vm`の位置・個数に対する解釈も異なる。

### 文書状況

AUD-018と次期`ExecutionRequest.arguments`で十分に設計済み。未実装であることも明記されている。今回の追加指摘は、単なるCLI機能不足でなく**embedding時のambient data leak**として優先度を上げる点である。

### 改善仕様・実装

- runtime coreから`std::env::args_os()`を除去。
- `ExecutionRequest.arguments: Arc<[String]>`だけをscriptへ公開。
- CLIは`--`以降だけをscript argsとしてsnapshotする。
- invalid UTF-8はCLI adapterで診断し、runtimeでlossy変換しない。
- safe profileのdefault argumentsは空。

### 受入テスト

- test hostを秘密argv付きで起動してもscript `args()`はrequest注入値だけを見る
- tree／VM同一
- request A/Bを同じEngineで並べても混ざらない

---

## REV-014 — process-global／first-use globalな設定と外部入力

**優先度:** P1
**確度:** 高。

### 現行箇所

- sandbox allow-list `OnceLock`: `src/sandbox.rs:12-43`
- collection limit `OnceLock`: `src/builtin_core.rs:32-45`
- env allow-list `OnceLock`: `src/builtin_core.rs:829-865`
- max stepsはEvaluator／VM構築時にambient envから読む: `src/eval.rs:24-55`, `src/vm.rs:39-47,122-152`
- argv、stdin、stdout、clock、process exitもambient
- ModuleLoader default base dirもprocess CWD

### 影響

- 同一processでtenant／Engineごとにpolicyを変えられない。
- 初回呼出し順でpolicyが固定され、test order依存になる。
- hostが環境変数を変更しても一部は反映、一部は反映されず、設定モデルが不統一。
- deterministic replayができない。
- libraryがhost processのglobal resourceを直接操作する。

### 文書状況

`design.md:739`はOnceLockのtestability問題を認識し、次期Capability／ExecutionRequest設計はほぼ十分。実装が追いついていない。

### 改善設計

```rust
pub struct EngineConfig {
    pub language_revision: LanguageRevision,
    pub backend: Backend,
    pub host_services: Arc<dyn HostServices>,
    pub admission: AdmissionConfig,
}

pub struct ExecutionRequest {
    pub capabilities: CapabilitySet,
    pub arguments: Arc<[String]>,
    pub budget: BudgetConfig,
    pub cancellation: CancellationToken,
    pub audit: AuditConfig,
}
```

CLIだけがenvironment／CWD／stdioを読み、明示configへ変換する。library coreは`std::env`、`std::process::exit`、process stdin/stdout、wall clockを直接参照しない。

### 受入テスト

- 同一processで異なるsandbox/env/budgetを持つ2 Engine
- 構築順を逆にしても結果不変
- tenant Aのcapability／argv／clockがBへ漏れない
- test並列実行可能

---

## REV-015 — 現行resource controlがsource・string・heap・I/O・bulk workを有限化しない

**優先度:** P0（production blocker）
**文書状況:** 検討◎、設計◎、実装×。既に十分考慮されているため要点のみ。

`language-spec.md:921-942`は、現行stepが構文解析、コンパイル、直線的コード、文字列、重いbuiltin、I/O待ち、総heapを包括しないと明記する。次期`execution-control.md`と`capability-model.md`はsource、string、heap、input/output、host call、fuel、deadline、cancelのかなり詳細な契約を持つ。

現行例:

- root sourceを全読込み: `src/main.rs:93-101`
- import sourceを全読込み: `src/module.rs:157-165`
- file read／read lines、replace、join、to_str、sort key等がallocation前の総bytes予約なし
- `print`は値全体をString化してから出力
- inputは行長上限なし

### 実装時の必須原則

- reserve-before-allocate／reserve-before-write
- `.take(limit + 1)`またはchunked readerで上限検知
- `try_reserve`とoverflow検査
- source count／single source bytes／total source bytes
- single string／total string allocation bytes
- live heapとpeak heap
- input/output call count・bytes
- bulk workを小chunkに分け、fuel／deadline／cancelを確認
- N／N+1で、N+1は対象allocation・I/O前に失敗

REV-001の共有DAG増幅を、次期budget実装の受入条件へ追加する必要がある。

---

## REV-016 — scriptから作れるRc cycleと長寿命contextのheap残留

**優先度:** P2
**文書状況:** 検討◎、設計○、実装×。

`docs/design.md:434-440,865-879`に明確に記載されている。AUD-042は「不要な全binding capture」による偶発cycleを減らしたが、scriptが実際に参照するcollectionへclosure自身を格納する`cell → List → closure → cell`はtree／VMとも残る。

これは隠れた問題ではない。追加すべきなのは完了表示の粒度である。

- `AUD-042: accidental capture-all cycle削減`は完了
- `General cyclic garbage collection`は未解決

### 改善案

近期:

- execution開始時のcontext baseline heap課金
- `clear_user_state()`
- tenantを跨いでcontext再利用しない契約
- cycleを含むsession stress test

中期:

- 長寿命sessionを正式要件にするならarena + tracing GCまたはcycle collector
- session不要ならexecution終了時にarenaを一括dropする構造を優先

---

## REV-017 — 表示、repr、sort orderingの結合

**優先度:** P2
**確度:** 高。

`Value::Display`はList内StringとDict keyを引用するが、quote、backslash、newline、control characterをescapeしない（`src/value.rs:174-203`）。そのため異なる値が同じ／紛らわしい表現を持ち得る。さらに`sort`はこのhuman-oriented `to_string()`をordering keyにする（`src/builtin_core.rs:381-386`、`language-spec.md:867`）。

結果:

- 表示改善がsort semanticsの破壊的変更になる。
- audit／diagnosticへ流用すると境界が曖昧になる。
- newlineを含む値が1 event／1 lineの前提を壊す。
- REV-001の巨大文字列化がsort比較でも発生する。

### 文書状況

sortが文字列表現順であることは記載済みだが、display/repr/audit/orderの分離設計は不足。

### 改善仕様

- `HumanDisplay`: user-facing、bounded
- `CanonicalRepr`: type tag、escape、revision付き、決定的
- `TotalOrder`: type rankと値の比較を明示
- audit fieldはtyped schemaで、Display文字列をidentityに使わない

alpha期間中にsort semanticsを変更し、revisionを上げることを推奨する。

---

## REV-018 — internal module／raw bytecode公開による安全境界の拡大

**優先度:** P1
**確度:** 高。

crate rootでは高水準`Engine`を埋め込み入口としつつ、`ast`、`chunk`、`compiler`、`eval`、`opcode`、`parser`、`sandbox`、`vm`等を全て`pub mod`で公開する（`src/lib.rs:3-29`）。コメント上は安定性非保証だが、公開されている以上、SemVer以前にhost safetyとinvariantの入口が広がる。

REV-004〜006はこの公開境界が直接の根因である。

### 改善設計

- stable root surfaceはEngine、compiled/linked artifact、request、outcome、capability descriptorへ限定。
- internalsは`pub(crate)`。
- test／benchはcrate-internal testまたは`test-support` featureを使用。
- advanced bytecode APIが必要なら`unstable-bytecode-v1` featureで明示し、`VerifiedChunk`だけを公開。
- public typeは「不正値を構築できない」か、constructorで完全検証する。

---

## REV-019 — `now()`がclock failureを0へ畳み、変換も未検査

**優先度:** P3
**確度:** 高。

`builtin_now`は`SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64`相当で、epoch前またはclock errorを0として返し、`u64 → i64`もcheckedでない（`src/builtin_core.rs:683-690`）。通常の現代時刻では問題化しにくいが、fake clock、異常clock、将来の注入APIで誤結果を成功値として扱う。

### 改善案

- 次期設計どおりClock capabilityを注入。
- wall clock取得失敗はstructured HostError。
- monotonic timeとwall timeを別型にする。
- conversionはchecked。
- deterministic testではfake clockだけを使う。

---

## REV-020 — import先parse errorのcause消失

**優先度:** P2
**確度:** 高。

`parse_module`はchild parserが返した複数errorをすべて捨て、import siteのwrapper messageだけを返す（`src/module.rs:176-191`）。コメントにもcause機構未実装とある。

### 文書状況

既に検討済み。canonical public errorとcause chainの方針もあるため、長い再検討は不要。

### 改善仕様

- public safe diagnostic:
  - import site line
  - normalized module ID（host pathでない）
  - child diagnostic code／line／message
- host-only detail:
  - resolver／OS cause、必要に応じてredacted path
- cause depth／diagnostic count／総byte数をboundedにする。
- nested importはcause chainを最大深度まで保持し、超過時はtruncated marker。

---

## REV-021 — `remove_dir`の現行再帰意味論と次期capabilityの不一致

**優先度:** P1
**種別:** 将来実装時に権限過大化を起こす設計矛盾。
**確度:** 高。

現行実装・仕様:

- `remove_dir`は`std::fs::remove_dir_all`: `src/builtin_core.rs:971-979`
- 「中身ごと再帰削除」: `docs/language-spec.md:853`

次期Capability Model:

- `RemoveKind`は`FileOrSymlink`または`EmptyDirectory`: `docs/capability-model.md:368-370`
- `remove_dir`を`EmptyDirectory`へmapping: `docs/capability-model.md:456`

このまま現行builtin semanticsを保ってadapterへ接続すると、`EmptyDirectory`権限で再帰削除を許可するか、builtinが非空directoryで突然失敗する。どちらも契約違反である。

### 推奨仕様

最も安全で明確な案:

- 次期revisionで`remove_dir(path)`を**空directoryだけ**へ変更。
- `remove_tree(path)`を新設し、別 capability `RecursiveDelete`を要求。
- recursive deleteはentry count／depth／bytes／fuel／deadline／cancel／auditを持つ。

互換性を優先するなら、`RemoveKind::RecursiveTree`を明示的に追加し、`remove_dir`へ割り当てる。`EmptyDirectory`へ暗黙昇格してはならない。

### 受入テスト

- 空／非空directory
- nested tree
- final symlinkと中間symlink
- partial failure／permission change／cancel
- capability隣接操作deny
- auditで削除対象数とpartial effectを記録

---

## REV-022 — I/O errorをNull／falseへ畳むAPI

**優先度:** P2
**文書状況:** 検討◎、設計◎、実装×。現行仕様として意図的なので要点のみ。

例:

- input EOFとread errorがどちらもNull: `src/builtin.rs:184-202`, `src/vm.rs:1492-1507`
- read/list/metadataは失敗時Null
- write/append/remove等は失敗時false
- `language-spec.md:840-856`に現行契約を明記

この契約はlegacy CLIでは簡便だが、安全なembeddingではnot found、permission、I/O failure、budget、denial、invalid encodingを区別できず、監査・retry判断を誤る。

### 改善案

- safe profile: structured HostError／Denied／BudgetExceeded
- EOFだけをNull等の明示的値にする
- legacy profile: 現行Null／false互換をadapterで提供
- capability denialをNull／falseへ変換しない

---

## REV-023 — `exit()`がホストprocessを終了する

**優先度:** P0
**文書状況:** 検討◎、設計◎、実装×。既に明確に認識されている。

- tree: `src/builtin.rs:216-236`
- VM: `src/vm.rs:1509-1526`
- READMEの警告: `README.md:47`
- 次期`Exited` outcomeとpanic／exit boundary: `embedding-api.md:455-470`

### 即時修正案

完全なPhase 1を待たず、crate root `Engine`経由では`exit()`を内部control signalとして捕捉し、`ExecutionOutcome::Exited { code }`を返す。CLI adapterだけがoutcomeをOS exit codeへ変換する。

少なくともstable facadeで`std::process::exit`へ到達する状態は、次の公開マイルストーン前に解消すべきである。

---

## REV-024 — CI／release gateの現状差

**優先度:** P2
**文書状況:** 検討◎、設計◎、実装△。既に詳細設計済み。

現行CI（`.github/workflows/ci.yml`）:

- rolling `stable`
- `actions/checkout@v4`等のmutable tag
- `cargo clippy -- -D warnings`で`--all-targets --all-features`なし
- `cargo test`で`--locked`なし
- coverageの最低率gateなし
- MSRV、docs drift、fuzz、stress、sanitizer、release artifactなし
- `Cargo.toml`に`rust-version`なし

`verification-release-operations.md`はこれらを正しく未実装として整理しており、設計追加より実装が必要。

### 近接改善

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo build --release --locked --all-features
cargo doc --locked --all-features --no-deps
```

- toolchain／MSRV pin
- actionをcommit SHA pin
- docs metadata drift checker
- weekly fuzz／stress、PR smoke
- timeout／RSS上限下のhost-stability tests

---

## REV-025 — parse diagnostic件数の無上限

**優先度:** P2
**確度:** 高。

Parserはerror recoveryで全errorを`self.errors`へ蓄積し、EOFまで継続する（`src/parser.rs:87-110`）。source size自体も現行では無制限であるため、攻撃的sourceから大量diagnostic、message allocation、表示出力を発生させ得る。次期`CompileErrors.diagnostics: Vec`にも上限契約が必要である。

### 改善仕様

- `max_source_bytes`
- `max_tokens`
- `max_ast_nodes`
- `max_compile_diagnostics`（例100）
- 上限到達時に最後の1件を`TooManyDiagnostics { omitted_at_least }`とする
- diagnostic総UTF-8 bytesも制限
- source excerptはbounded、secret redaction可能

### 受入テスト

- 99／100／101件
- 1行に大量token、各行1error、大量Unicode
- diagnostics表示のoutput budget
- recoveryが同一位置で進まないcaseでも無限loopしない

---

## 5. 既に十分検討されている項目

以下は重要だが、文書に問題・決定・受入基準が既に十分ある。今回あらためて長い設計議論をするより、実装issueとgateへ直結させるべきである。

| 項目 | 文書状況 | 現状 | 今回の補足 |
|---|---|---|---|
| deny-by-default capability | 十分 | 未実装 | REV-002、013、014、021を具体的な追加受入caseにする |
| 有限fuel／heap／source／I/O budget | 非常に十分 | 未実装 | REV-001の共有DAGとdiagnostic上限を追加 |
| cooperative execution／cancel／backpressure | 非常に十分 | 未実装 | bulk renderer、sort、comparison、directory traversalもyield point対象にする |
| audit／record-replay／fail-closed | 非常に十分 | 未実装 | Display文字列でなくtyped canonical representationを使用 |
| panic boundary | 十分 | 部分実装 | raw bytecodeのhangとbuilder panicも対象に追加 |
| filesystem TOCTOU／path handle | 十分 | 未実装 | 非UTF-8 Path変換を明示caseに追加 |
| `path_join`非Str黙殺 AUD-034 | 十分 | 未実装 | 既存計画どおりstrict typingでよい |
| checked cast AUD-036 | 十分 | 未実装 | 32-bit `usize`変換と`now()`を同じ共通helper群へ追加 |
| CLI args AUD-018 | 十分 | 未実装 | embeddingのhost argv漏えいとしてpriorityを上げる |
| incomplete EOF AUD-033 | 十分 | 未実装 | 現行計画どおり |
| fuzz／stress／release gate AUD-022/045 | 十分 | 部分実装 | shared DAG、raw Jump、invalid UTF-8 pathをseedへ追加 |
| intentional Rc cycle | 十分 | 未解決 | AUD-042完了とgeneral cycle未解決を別statusにする |
| VM backend parity | 十分 | 継続 | current deviationsの文書driftを機械化する |

---

## 6. Tsumugiの目標に対するギャップと強化点

マニフェストの原則ごとに、現状・文書・必要な強化を整理する。

### 6.1 ホストの安定性

**目標:** 最悪負荷を制御し、script failureでhostをpanic／abort／exitさせない（`manifesto.md:32-38,69-77`）。

**現状の主な阻害:** REV-001、006、015、023、016。

**文書:** 将来設計は強い。今回追加すべきは「小さい物理graphから大きい論理workを作る」caseと、「公開raw artifactそのものをresource boundaryとして検証する」case。

**強化方針:**

- 「全loopへmeter」ではなく、**全ての反復・再帰・graph traversal・back edge・bulk operation**へ共通control hookを入れる。
- public artifactは構築時検証、runtime defense、process isolationの三層。
- unsafeな同期convenience methodにもhard deadlineを暗黙生成するのではなく、finite requestを必須化。

### 6.2 明示的な権限

**目標:** ambient authorityをdeny-by-defaultにする（`manifesto.md:39-45`）。

**現状の主な阻害:** REV-002、013、014、021、022。

**文書:** capability modelは詳細で概ね十分。

**強化方針:**

- library coreからprocess-global accessをゼロにすることを機械検査する。
- `rg`に依存せず、`core` crateと`cli` crateを分離し、coreから`std::env`／`std::process`／direct filesystem importを構造的に禁止。
- filesystemはpath string認可でなくhandle認可。
- recursive destructive operationは別capability。

### 6.3 すべての実行に予算

**目標:** CPU、memory、time、I/O、host call、同時実行を制御（`manifesto.md:46-53`）。

**現状の主な阻害:** REV-001、007、010、015、025。

**文書:** 非常に良い。

**強化方針:**

- meterをcontextからrequestへ移す。
- logical graph workを課金する。
- conversion前にplatform widthを検査。
- compile/link/run/serialize/auditの全phaseにbudgetを持たせる。
- budget accounting revisionをartifactとauditへ記録。

### 6.4 負荷平準化とcooperative execution

**目標:** yield、pause、cancel、backpressure（`manifesto.md:55-60`）。

**現状:** 未実装だが設計は詳細。

**追加強化:** evaluator statement境界だけでなく、以下がcontinuationを持てる必要がある。

- deep equality
- sort
- string replace／encode
- canonical renderer
- directory listing
- import read／parse
- audit sink backpressure

「builtinは1回のdispatchで終わる」という構造のままでは、長いbuiltin中にhostへ制御を返せない。

### 6.5 予測可能性・backend一致

**目標:** backendで意味、error、副作用、資源消費を変えない（`manifesto.md:62-67`）。

**現状の主な阻害:** REV-003、011、012、017、020。

**強化方針:**

- Numeric semantics、ordering、reprを共有moduleへ集約。
- tree／VM共通HIRまたは少なくともshared semantic operationsを増やす。
- differential testはstdoutだけでなくstate delta、outcome、budget usage、audit sequenceまで比較。
- known deviation集合を機械可読化。

### 6.6 失敗を制御可能な結果にする

**目標:** failureを正式outcomeとして扱う。

**現状の主な阻害:** REV-004〜008、019、020、022、023。

**強化方針:**

- panic、exit、host error、budget、denial、cancelを別terminal channelへ。
- stable executionは常にtransaction。
- Null／false互換はlegacy adapterだけ。
- internal invariant failureはcontext poisonとfault ID。

### 6.7 観測・監査可能性

**目標:** script、revision、input、capability、budget、effect、terminationを構造化記録。

**現状:** 未実装。設計は非常に詳細。

**追加強化:**

- REV-011の単一metadata正本が前提。
- canonical typed valueとredactionをDisplayから分離。
- partial filesystem effect、rollback済みlanguage state、残存host effectを別fieldで記録。
- invalid encodingはlossy identityへ変換しない。

### 6.8 小さく理解可能な中核

**目標:** 外部効果をhost boundaryへ置き、coreを検証可能に保つ。

**現状の主な阻害:** process-global builtinと広いpublic module surface。

**強化方針:**

- `tsumugi-core`: pure semantics、artifact、execution state machine
- `tsumugi-host`: capability traits、budgeted adapters
- `tsumugi-cli`: argv/env/CWD/stdio/process exit mapping

必ずしもworkspace分割が必要ではないが、Rust module dependency方向で同等の境界を強制する。

---

## 7. 推奨アーキテクチャ

### 7.1 型と責務

```text
EngineConfig (immutable, shareable)
  - language revision
  - backend
  - verifier/accounting revisions
  - host service factories
  - engine-wide admission/backpressure

Engine
  - compile(source, CompileBudget) -> CompiledScript | CompileErrors
  - link(compiled, Resolver, LinkBudget) -> LinkedScript | LinkOutcome
  - create_execution(context, linked, request) -> ExecutionHandle

ExecutionContext (!Send, !Sync)
  - committed session language state only
  - no argv/capability/meter/deadline/stdin/stdout

ExecutionRequest
  - finite BudgetConfig
  - CapabilitySet
  - arguments
  - cancellation
  - audit/record/replay policy

ExecutionHandle
  - request-owned meter
  - transaction journal
  - continuation
  - terminal outcome exactly once
```

### 7.2 実行の一貫した流れ

```text
compile
  source bytes予約 → lex → parse → diagnostics bounded → artifact verify

link
  resolver capability → source/import budget → module graph → verified artifact

start
  Engine/Context/Revision整合 → baseline heap → transaction begin → admission

poll
  logical charge → semantic work → bounded host adapter → yield/terminal

terminal
  Completed/Exited: commit
  その他: rollback
  audit terminalをfail-closedで確定
```

### 7.3 shared semantic services

二重実装差を減らすため、最低限以下をtree／VMで共有する。

- `NumericSemantics`
- `ValueEquality`
- `ValueOrdering`
- `ValueRenderer`
- `PathSyntax`とcapability request生成
- `ErrorFactory`
- `FuelSchedule`
- `ExecutionTransaction`
- `BuiltinSpec`（既に進展あり）

VMはこれらの意味を変えず、dispatch方式だけを変える。

---

## 8. 実装順序

### Phase A — 安全性ホットフィックス

1. `exit()`をOutcomeへ変更（REV-023）
2. `Chunk`／`Vm`の公開縮小、`patch_jump` Result化（REV-004、018）
3. bytecode verifierと`VerifiedChunk`、back edge／call protocol検証（REV-005、006）
4. exact mixed numeric comparison（REV-003）
5. filesystem APIを`&Path`へ変更し、importの認可対象一致（REV-002）
6. bounded graph equality／renderer（REV-001）

この段階のrelease noteには、旧raw bytecode APIとsort／numeric semanticsのbreaking changeを明記する。

### Phase B — stable embedding縦切り

1. EngineConfig／ExecutionRequest／ExecutionMeter分離（REV-007、014）
2. request-injected args／stdio／clock（REV-013、019）
3. 全stable execution transaction（REV-008）
4. structured outcome／HostError／Denied（REV-022）
5. public surfaceの封印（REV-018）

### Phase C — capability filesystem

1. root-bound handleとopaque module ID
2. importとruntime Readの分離
3. invalid encoding policy
4. `list_dir` atomicity（REV-009）
5. recursive delete capabilityの決定（REV-021）
6. failure injection tests

### Phase D — finite budgetとcooperative execution

1. compile/link source/token/diagnostic budget（REV-025）
2. string／heap／I/O reservation
3. graph traversal fuel（REV-001）
4. builtin continuation化
5. cancel／deadline／yield
6. context baselineとcycle stress（REV-016）

### Phase E — determinism、audit、release

1. CanonicalRepr／TotalOrder（REV-017）
2. budget usageとstate deltaのbackend differential
3. audit／record-replay
4. metadata single source（REV-011）
5. CI/MSRV/fuzz/stress/release gate（REV-024）

---

## 9. 文書管理の改善案

### 9.1 「検討・設計・実装・検証・公開」を分離

現在は`✅ 完了`が、意味論決定、REPLだけの実装、全execution実装、paired test通過のどれを指すか行によって異なる。次のstate machineを推奨する。

```text
Identified
→ DecisionAccepted
→ SpecWritten
→ ImplementationPartial
→ ImplementationComplete
→ AcceptanceVerified
→ Released
```

各AUD／REVに`scope`を持たせる。

```yaml
id: AUD-024
scope:
  repl_tree: AcceptanceVerified
  repl_vm: AcceptanceVerified
  stable_execution_tree: Identified
  stable_execution_vm: Identified
  budget_cancel_terminals: SpecWritten
```

### 9.2 正本文書の役割を機械化

- `language-spec.md`: 現行released／verified semanticsのみ
- `semantic-decisions.md`: 次revision decisionとmigration
- `design.md`: current architecture。revision literalは生成
- `roadmap.md`: machine-readable statusのrendering
- 各future spec: API／invariant／acceptance。個別節statusを持つ

### 9.3 新規追跡項目として登録すべきもの

既存AUD番号を勝手に割り当てず、以下を新規backlog IDとして登録する。

1. Shared-DAG traversal amplification
2. Non-UTF8 canonical import authorization mismatch
3. Exact mixed Int/Float comparison
4. Bytecode verifier／verified artifact
5. Per-execution meter ownership
6. Stable execution transaction
7. Directory listing error and encoding semantics
8. Recursive delete capability semantics
9. Display／CanonicalRepr／TotalOrder separation
10. Documentation metadata generation
11. Compile diagnostic count budget

---

## 10. 必須受入ゲート

### 10.1 Host stability gate

- ordinary script、raw artifact、malformed sourceの全corpusでpanic／abort／unexpected process exitなし
- timeoutとRSS制限下でhang／OOMを成功扱いしない
- DAG depth 60、deep chain、backward Jump、huge diagnosticを含む
- `panic=unwind`だけでなく、stack overflow／OOMはprocess isolation testで検出

### 10.2 Semantic gate

- tree／VMでvalue、stdout、structured outcome、state delta、error kind/message/line/traceが一致
- Int/Float境界値
- call precedence
- sort/repr escaping
- invalid encoding

### 10.3 State gate

- Completed／Exitedだけcommit
- その他terminalはbinding、cell、collection、closure、module markerをrollback
- host effectは残存を明示
- context poison条件を固定
- meter、arguments、capabilities、cancelを次executionへ持ち越さない

### 10.4 Capability gate

- default emptyで外部operation 0件
- operationごとの最小grant
- denied pathはresolver／OS call 0
- invalid UTF-8、symlink、rename、recursive delete、list partial failure
- tenant間のpolicy分離

### 10.5 Budget gate

各resourceについて0、N-1、N、N+1、overflowを検査する。

- source count/bytes
- token/AST/diagnostic
- fuel
- graph traversal nodes/depth
- string allocation/single string
- heap live/peak
- collection cardinality
- input/output/host request/response
- audit buffer

### 10.6 Documentation/release gate

- package version、language revision、backend deviation、accounting revisionが全artifactで一致
- stale literal negative test
- `cargo ... --locked`
- MSRV + stable
- coverage threshold
- all-targets Clippy
- fuzz/stress直近成功
- action SHA pinとartifact provenance

---

## 11. 良い点・特記事項

### 11.1 特に良い点

1. **マニフェストが明確**
   単に「安全な言語」とせず、host stability、deny-by-default capability、finite budget、cooperative execution、determinism、structured failure、audit、小さいcoreという優先順位を明文化している。

2. **残余リスクを隠していない**
   現行sandboxをsecurity boundaryと呼ばず、source/string/heap/I/O未制限、TOCTOU、ambient authority、`exit()`等をかなり正直に記載している。

3. **監査履歴が豊富**
   AUD ID、問題、決定、実装、回帰test、測定値を残している。後から設計判断を検証できる。

4. **backend差への姿勢が良い**
   Lexer/Parser/AST共有だけで満足せず、paired golden、canonical error inventory、defensive VM testを用意している。

5. **過去の重大panicを具体的に修正している**
   malformed bytecode、broken pipe、invalid argv、REPL state等について、原因と防止策を記録している。

6. **次期設計が実装可能な粒度**
   単なる方向性ではなく、型、state machine、budget reservation、acceptance IDまで書かれている。

7. **依存が小さい**
   production dependencyがなく、core理解可能性という目標に合う。dev dependencyはcriterionのみ。

8. **文書リンク品質**
   今回確認したローカル相対Markdownリンクは破損0件だった。

### 11.2 注意すべき特記事項

- 文書量が増えた結果、**同じ事実のコピーが新たなリスク源**になっている。AUD-044で直したrevision driftが再発していることが象徴的。
- 将来設計が詳細なため、「設計済み」が心理的に「ほぼ完成」へ見えやすい。実装・acceptance・releaseを厳密に分ける必要がある。
- 2 backendの完全一致を維持するコストは今後さらに増える。class、capability、cooperative continuationまで別実装すると、testだけで差を抑えるのは難しくなる。共有HIRまたはsemantic serviceの拡大を早めに判断すべき。
- `Rc`＋COW＋transaction＋heap accounting＋continuationが同時に入ると、所有権と課金の複雑度が急増する。Phase 3前に「session stateをどこまで長寿命にするか」を固定した方がよい。
- `ExecutionContext: !Send + !Sync`は合理的だが、業務systemでの並列実行はEngine-level schedulerと複数contextで実現することを利用者向けに明示する必要がある。

---

## 12. 最終判定

### 現行版をどう位置づけるべきか

- **教育・実験用途:** 良好
- **信頼済みscriptをCLIで試す:** 制約を理解すれば可能
- **信頼済みscriptをlibrary embedding:** `exit()`、ambient I/O、手動step reset、非transaction executeを許容できる限定用途のみ
- **非信頼scriptを同一processで実行:** 不可
- **multi-tenant業務systemへ組み込む:** 現時点では不可
- **将来の制御可能なembedded languageの設計基盤:** 非常に有望

### 次のマイルストーンの定義

「機能が増えた版」ではなく、次を満たす**安全境界縦切り**を最初の重要マイルストーンにすることを推奨する。

1. `Engine`以外のunsafe internal surfaceを閉じる。
2. `exit`、argv、stdio、clock、filesystemをrequest/capability経由にする。
3. finite budgetを必須にする。
4. verified artifactだけを実行する。
5. 全terminalでtransactionとstructured outcomeを成立させる。
6. shared-DAG、invalid path、numeric boundary、malformed bytecodeの回帰testを通す。
7. revision／statusを単一正本から生成する。

ここまで到達すれば、Tsumugiは「安全性を目標に掲げる言語」から、**限定された保証を実装と受入試験で示せる組み込み実行基盤**へ一段進む。
