//! Phase 6 R-1（redaction 本体、R-1a 粒度）の統合テスト。
//!
//! 設計正本は `docs/determinism-and-audit.md` §9（redaction policy）/ §9.1（field class / 型）/
//! §9.2（canonical encoding）、および §14「R-1 実装スライス」。filesystem は tree 唯一の host-call
//! 境界なので、`read_file` / `write_file` を監査 sink 付きで実行し、host-call payload が §9.1 の
//! `AuditField` ツリーへ redaction されること（Path は Allow で HmacSha256・Deny で Omitted、
//! body は Omitted、raw path/label は 1 byte も漏れない）を固定する。R-AT-01〜04・06・09 を担保する。
//!
//! R-AT-05（非破壊）は既存 `tests/audit_*.rs` の sequence/yield 位置/encoded 推定長の不変で担保し、
//! R-AT-07（Secret の compile-fail）/ R-AT-08（byte 計上の据え置き）は `src/audit.rs` の doctest /
//! unit test で担保する。crate root の公開型だけを使う。

use std::collections::BTreeSet;
use std::num::NonZeroU128;
use std::sync::Arc;

use tsumugi::{AuditDirection, AuditFieldKey, AuditScalar, AuditValue, RedactionMode};
use tsumugi::{
    AuditEvent, AuditField, AuditFieldClass, AuditSink, BudgetConfig, CapabilityDecisionKind,
    CapabilitySet, CompileOptions, EmbeddingContext, EmbeddingEngine, EmbeddingOutcome,
    EmbeddingRequest, ExecutionId, FakeClock, FilesystemCapability, FilesystemRoot, FsOperation,
    HostCallOutcome, InMemoryAuditSink, LinkRequest, MonotonicClock, MountName, OsDirectoryHandle,
    RedactionPolicy, SecretBytes, Source, SourceId, SymlinkPolicy,
};

// -----------------------------------------------------------------------------
// ヘルパ（audit_hostcall.rs と同形。test 間で private helper は共有できないため複製）
// -----------------------------------------------------------------------------

fn temp_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "tsg-audit-redaction-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

/// mount `data` に指定 operation 群を grant した filesystem capability。
fn fs_with_ops(base: &std::path::Path, ops: &[FsOperation]) -> FilesystemCapability {
    let pid = NonZeroU128::new(1).expect("non-zero");
    let mut op_set = BTreeSet::new();
    for op in ops {
        op_set.insert(*op);
    }
    let mount = MountName::new("data").expect("mount");
    let handle = OsDirectoryHandle::new(base.to_path_buf(), pid, SymlinkPolicy::DenyAll);
    let root = FilesystemRoot::new(mount, pid, op_set, SymlinkPolicy::DenyAll, Arc::new(handle))
        .expect("root");
    FilesystemCapability::new(vec![root]).expect("fs")
}

fn fs_capabilities(fs: FilesystemCapability) -> CapabilitySet {
    CapabilitySet::builder()
        .filesystem(fs)
        .expect("grant filesystem")
        .build()
}

/// Read + Write + Create を grant した通常の rw capability。
fn rw_fs(base: &std::path::Path) -> FilesystemCapability {
    fs_with_ops(
        base,
        &[FsOperation::Read, FsOperation::Write, FsOperation::Create],
    )
}

fn source(id: &str, text: &'static str) -> Source<'static> {
    Source::new(SourceId::new(id).expect("valid source id"), text)
}

fn link_source(
    engine: &EmbeddingEngine,
    id: &str,
    text: &'static str,
) -> tsumugi::EmbeddingCompiledScript {
    engine
        .compile(
            source(id, text),
            &CompileOptions {
                retain_source: true,
            },
        )
        .expect("compile")
}

fn link_request() -> LinkRequest {
    let clock = FakeClock::new();
    let budget = BudgetConfig::standard(&clock).expect("standard budget");
    LinkRequest::new(
        ExecutionId::new(1u128.try_into().expect("nonzero")),
        CapabilitySet::empty(),
        budget,
    )
}

fn run_request(id: u128, capabilities: CapabilitySet) -> EmbeddingRequest {
    let clock: Arc<dyn MonotonicClock> = Arc::new(FakeClock::new());
    let budget = BudgetConfig::standard(clock.as_ref()).expect("standard budget");
    EmbeddingRequest::new(budget, clock)
        .expect("standard budget は同 domain")
        .with_execution_id(ExecutionId::new(id.try_into().expect("nonzero")))
        .with_capabilities(capabilities)
}

/// 既定 policy の監査付き engine を作る。
fn default_engine(sink: &Arc<InMemoryAuditSink>) -> EmbeddingEngine {
    EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .build()
        .expect("build")
}

fn host_call_events(envelopes: &[tsumugi::AuditEnvelope]) -> Vec<&AuditEvent> {
    envelopes
        .iter()
        .map(|e| &e.event)
        .filter(|e| {
            matches!(
                e,
                AuditEvent::HostCallStarted { .. }
                    | AuditEvent::CapabilityDecision { .. }
                    | AuditEvent::HostCallFinished { .. }
            )
        })
        .collect()
}

/// triple[1]（CapabilityDecision）の resource.path field を取り出す。
fn resource_path<'a>(triple: &[&'a AuditEvent]) -> &'a AuditField {
    match triple[1] {
        AuditEvent::CapabilityDecision { resource, .. } => resource
            .fields
            .get("path")
            .expect("resource に path field がある"),
        other => panic!("triple[1] は CapabilityDecision のはず: {other:?}"),
    }
}

/// ExecutionStarted の redaction_policy_id を取り出す。
fn redaction_policy_id(envelopes: &[tsumugi::AuditEnvelope]) -> String {
    match &envelopes[0].event {
        AuditEvent::ExecutionStarted {
            redaction_policy_id,
            ..
        } => redaction_policy_id.clone(),
        other => panic!("envelope[0] は ExecutionStarted のはず: {other:?}"),
    }
}

/// payload / error を Debug 出力した文字列へ needle が 1 度も現れないことを確認する。
///
/// Debug は redaction 後の構造を文字列化するため、raw path / canonical label がどこにも
/// 入っていないことを機械的に走査できる（§9.1 の「1 byte も漏れない」）。
fn assert_no_substring(envelopes: &[tsumugi::AuditEnvelope], needle: &str) {
    for env in envelopes {
        let dumped = format!("{:?}", env);
        assert!(
            !dumped.contains(needle),
            "payload に {needle:?} が漏れている: {dumped}"
        );
    }
}

// -----------------------------------------------------------------------------
// R-AT-01: Allow 時の Path（Success と HostError 両方）は HmacSha256（raw でない）
// -----------------------------------------------------------------------------

#[test]
fn r_at_01_allow_path_is_hmac_same_label_same_digest_different_differs() {
    let base = temp_dir("at01");
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = default_engine(&sink);

    // 同じ path を 2 回 write（same label → same digest）、別 path を 1 回 write（different）。
    let script = link_source(
        &engine,
        "m",
        "write_file(\"@data/a.txt\", \"hello\")\n\
         write_file(\"@data/a.txt\", \"world\")\n\
         write_file(\"@data/b.txt\", \"other\")\n",
    );
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(
        &linked,
        &mut ctx,
        run_request(101, fs_capabilities(rw_fs(&base))),
    );
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::Completed { .. })
    ));

    let envelopes = sink.snapshot();
    let hc = host_call_events(&envelopes);
    assert_eq!(hc.len(), 9, "3 write × 3 event");

    // 3 つの write の path field を取り出す。
    let digest = |triple: &[&AuditEvent]| -> [u8; 32] {
        match resource_path(triple) {
            AuditField::HmacSha256 {
                class: AuditFieldClass::Path,
                digest,
                original_bytes,
            } => {
                // original_bytes は canonical label "@data/a.txt" 等の UTF-8 byte 長（11）。
                assert_eq!(*original_bytes, "@data/a.txt".len() as u64);
                *digest
            }
            other => panic!("Allow の Path は HmacSha256 のはず: {other:?}"),
        }
    };
    let d_a1 = digest(&hc[0..3]);
    let d_a2 = digest(&hc[3..6]);
    let d_b = digest(&hc[6..9]);

    assert_eq!(d_a1, d_a2, "同じ canonical label は同じ digest");
    assert_ne!(d_a1, d_b, "別 label（別 path）は別 digest");

    // raw path 文字列も canonical label 文字列も payload に現れない。
    assert_no_substring(&envelopes, "a.txt");
    assert_no_substring(&envelopes, "b.txt");
    assert_no_substring(&envelopes, "@data/a.txt");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r_at_01_allow_host_error_path_is_still_hmac() {
    let base = temp_dir("at01he");
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = default_engine(&sink);

    // 存在しないファイルを read: route/認可（Read）は通り label 確定、その後 open が失敗して
    // HostError（Allow/HostError、fs_result は Err）。denial ではないので try で握る。
    let script = link_source(
        &engine,
        "m",
        "let ok = false\ntry\n    read_file(\"@data/missing.txt\")\ncatch e\n    ok = true\nend\n",
    );
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(
        &linked,
        &mut ctx,
        run_request(102, fs_capabilities(rw_fs(&base))),
    );
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::Completed { .. })
    ));

    let envelopes = sink.snapshot();
    let hc = host_call_events(&envelopes);
    assert_eq!(hc.len(), 3, "1 host call × 3 event");

    // outcome は HostError・decision は Allow。
    match hc[2] {
        AuditEvent::HostCallFinished { outcome, .. } => {
            assert_eq!(*outcome, HostCallOutcome::HostError);
        }
        other => panic!("{other:?}"),
    }
    match hc[1] {
        AuditEvent::CapabilityDecision { decision, .. } => {
            assert_eq!(*decision, CapabilityDecisionKind::Allow);
        }
        other => panic!("{other:?}"),
    }

    // host error でも Path は HmacSha256（label は失敗前に確定）。
    assert!(matches!(
        resource_path(&hc[0..3]),
        AuditField::HmacSha256 {
            class: AuditFieldClass::Path,
            ..
        }
    ));
    assert_no_substring(&envelopes, "missing.txt");

    let _ = std::fs::remove_dir_all(&base);
}

// -----------------------------------------------------------------------------
// R-AT-02: Deny 時の Path は Omitted{class:Path}、raw/label は非露出（4 経路）
// -----------------------------------------------------------------------------

/// Deny 1 件（host call 1 回）の script を走らせ、Path field が Omitted{class:Path} で、raw/label
/// が payload に出ないことを確認する共通ヘルパ。
fn assert_deny_path_omitted(
    id: u128,
    capabilities: CapabilitySet,
    script_text: &'static str,
    leak_needles: &[&str],
    expected_original_bytes: u64,
) {
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = default_engine(&sink);
    let script = link_source(&engine, "m", script_text);
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(&linked, &mut ctx, run_request(id, capabilities));
    assert!(
        matches!(audited.outcome(), Some(EmbeddingOutcome::Completed { .. })),
        "denial は catch 可能なのでスクリプトは Completed: {:?}",
        audited.outcome()
    );

    let envelopes = sink.snapshot();
    let hc = host_call_events(&envelopes);
    assert_eq!(hc.len(), 3, "Deny でも 3 event が揃う（§8 規則4）: {hc:?}");

    match hc[1] {
        AuditEvent::CapabilityDecision { decision, .. } => {
            assert_eq!(*decision, CapabilityDecisionKind::Deny, "decision は Deny");
        }
        other => panic!("{other:?}"),
    }
    match resource_path(&hc[0..3]) {
        AuditField::Omitted {
            class: AuditFieldClass::Path,
            original_bytes,
        } => {
            assert_eq!(
                *original_bytes, expected_original_bytes,
                "Deny の Path original_bytes は requested path 長（引数型エラーは 0）"
            );
        }
        other => panic!("Deny の Path は Omitted{{class:Path}} のはず: {other:?}"),
    }
    for needle in leak_needles {
        assert_no_substring(&envelopes, needle);
    }
}

#[test]
fn r_at_02a_deny_not_granted() {
    // (a) filesystem 未 grant。label 確定前の Deny。
    assert_deny_path_omitted(
        201,
        CapabilitySet::empty(),
        "let ok = false\ntry\n    read_file(\"@data/secret.txt\")\ncatch e\n    ok = true\nend\n",
        &["secret.txt", "@data/secret.txt"],
        "@data/secret.txt".len() as u64,
    );
}

#[test]
fn r_at_02b_deny_argument_path_error() {
    // (b) path lexical/argument エラー（絶対 path）。path 文字列はあるので original_bytes は長さ。
    // require_str は成功し、FilesystemTarget::parse が PathError::Absolute を返す経路。
    let base = temp_dir("at02b");
    assert_deny_path_omitted(
        202,
        fs_capabilities(rw_fs(&base)),
        "let ok = false\ntry\n    read_file(\"/etc/passwd\")\ncatch e\n    ok = true\nend\n",
        &["/etc/passwd", "passwd"],
        "/etc/passwd".len() as u64,
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r_at_02b_deny_argument_type_error_is_zero_bytes() {
    // (b') 引数型エラー（require_str 失敗、path 文字列すら無い）。original_bytes は 0。
    let base = temp_dir("at02bt");
    assert_deny_path_omitted(
        203,
        fs_capabilities(rw_fs(&base)),
        "let ok = false\ntry\n    read_file(123)\ncatch e\n    ok = true\nend\n",
        &[],
        0,
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r_at_02c_deny_mount_unregistered() {
    // (c) mount 未登録（@other は未登録）。provider 問い合わせ前の sandbox denial。
    let base = temp_dir("at02c");
    assert_deny_path_omitted(
        204,
        fs_capabilities(rw_fs(&base)),
        "let ok = false\ntry\n    read_file(\"@other/x.txt\")\ncatch e\n    ok = true\nend\n",
        &["@other/x.txt", "x.txt"],
        "@other/x.txt".len() as u64,
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r_at_02c_deny_operation_not_granted() {
    // (c') operation 未 grant（Read だけ grant、write_file は Write 不足）。sandbox denial。
    let base = temp_dir("at02cop");
    assert_deny_path_omitted(
        205,
        fs_capabilities(fs_with_ops(&base, &[FsOperation::Read])),
        "let ok = false\ntry\n    write_file(\"@data/w.txt\", \"x\")\ncatch e\n    ok = true\nend\n",
        &["w.txt", "@data/w.txt"],
        "@data/w.txt".len() as u64,
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r_at_02d_deny_upsert_create_missing_label_exists_but_omitted() {
    // (d) Upsert の Create 不足: Write は grant・Create 無し。fs_route（Write 代表 op）成功で
    // label が確定した「後」に filesystem_denied になる。decision は Deny なので label が
    // 手元にあっても Path は Omitted（§9.1 / R-AT-02d）。original_bytes は requested path 長
    // （label 長を漏らさない）。
    let base = temp_dir("at02d");
    assert_deny_path_omitted(
        206,
        fs_capabilities(fs_with_ops(&base, &[FsOperation::Read, FsOperation::Write])),
        "let ok = false\ntry\n    write_file(\"@data/c.txt\", \"x\")\ncatch e\n    ok = true\nend\n",
        &["c.txt", "@data/c.txt"],
        "@data/c.txt".len() as u64,
    );
    let _ = std::fs::remove_dir_all(&base);
}

// -----------------------------------------------------------------------------
// R-AT-03: body は Omitted{class:Body}、original_bytes は pre-redaction byte 長・prefix も残さない
// -----------------------------------------------------------------------------

#[test]
fn r_at_03_body_is_omitted_with_original_bytes() {
    let base = temp_dir("at03");
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = default_engine(&sink);

    // write する本文は canary（payload に 1 byte も残ってはいけない）。
    let script = link_source(
        &engine,
        "m",
        "write_file(\"@data/body.txt\", \"CANARYBODYSECRET\")\n",
    );
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(
        &linked,
        &mut ctx,
        run_request(301, fs_capabilities(rw_fs(&base))),
    );
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::Completed { .. })
    ));

    let envelopes = sink.snapshot();
    let hc = host_call_events(&envelopes);

    // request.body は Omitted{class:Body}、original_bytes は request_bytes（§9.1）。
    match hc[0] {
        AuditEvent::HostCallStarted {
            request,
            request_bytes,
            ..
        } => {
            let body = request.fields.get("body").expect("request に body field");
            match body {
                AuditField::Omitted {
                    class: AuditFieldClass::Body,
                    original_bytes,
                } => {
                    assert_eq!(*original_bytes, *request_bytes, "body は原本 byte 長を保持");
                }
                other => panic!("request.body は Omitted{{class:Body}} のはず: {other:?}"),
            }
        }
        other => panic!("{other:?}"),
    }

    // 本文も、その prefix も payload に残らない。
    assert_no_substring(&envelopes, "CANARY");
    assert_no_substring(&envelopes, "CAN");

    let _ = std::fs::remove_dir_all(&base);
}

// -----------------------------------------------------------------------------
// R-AT-04: ExecutionStarted.redaction_policy_id が注入 policy_id をそのまま反映する
// -----------------------------------------------------------------------------

#[test]
fn r_at_04_default_policy_id_is_default_r1() {
    let base = temp_dir("at04d");
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = default_engine(&sink);
    let script = link_source(&engine, "m", "write_file(\"@data/p.txt\", \"x\")\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let _ = engine.run_audited(
        &linked,
        &mut ctx,
        run_request(401, fs_capabilities(rw_fs(&base))),
    );

    let envelopes = sink.snapshot();
    assert_eq!(
        redaction_policy_id(&envelopes),
        "default-r1",
        "既定 policy の policy_id は default-r1（R-AT-04）"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r_at_04_injected_policy_id_is_reflected_verbatim() {
    let base = temp_dir("at04i");
    let sink = Arc::new(InMemoryAuditSink::new());
    // 別 policy_id を注入する（test 専用 shim）。
    let policy = RedactionPolicy {
        policy_id: "custom-policy-xyz".to_string(),
        ..RedactionPolicy::default()
    };
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .audit_redaction_policy_for_test(policy)
        .build()
        .expect("build");

    let script = link_source(&engine, "m", "write_file(\"@data/p.txt\", \"x\")\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let _ = engine.run_audited(
        &linked,
        &mut ctx,
        run_request(402, fs_capabilities(rw_fs(&base))),
    );

    let envelopes = sink.snapshot();
    assert_eq!(
        redaction_policy_id(&envelopes),
        "custom-policy-xyz",
        "注入 policy_id がそのまま反映される（R-AT-04）"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r_at_04_injected_hmac_key_changes_digest() {
    // 別 hmac_key を注入すると同じ label でも digest が変わる（§15.4）。
    let base = temp_dir("at04k");

    let digest_for_key = |key: Vec<u8>, id: u128| -> [u8; 32] {
        let sink = Arc::new(InMemoryAuditSink::new());
        let policy = RedactionPolicy {
            hmac_key: SecretBytes::new(key),
            ..RedactionPolicy::default()
        };
        let engine = EmbeddingEngine::builder()
            .audit_sink(sink.clone() as Arc<dyn AuditSink>)
            .audit_redaction_policy_for_test(policy)
            .build()
            .expect("build");
        let script = link_source(&engine, "m", "write_file(\"@data/k.txt\", \"x\")\n");
        let linked = engine.link(&script, link_request()).expect("link");
        let mut ctx = EmbeddingContext::new(&engine);
        let _ = engine.run_audited(
            &linked,
            &mut ctx,
            run_request(id, fs_capabilities(rw_fs(&base))),
        );
        let envelopes = sink.snapshot();
        let hc = host_call_events(&envelopes);
        match resource_path(&hc[0..3]) {
            AuditField::HmacSha256 { digest, .. } => *digest,
            other => panic!("{other:?}"),
        }
    };

    let d1 = digest_for_key(b"key-one".to_vec(), 403);
    let d2 = digest_for_key(b"key-two".to_vec(), 404);
    assert_ne!(d1, d2, "別 hmac_key は別 Path digest（§15.4）");
    let _ = std::fs::remove_dir_all(&base);
}

// -----------------------------------------------------------------------------
// R-AT-06: 同 script・同 key・同 policy なら 100 回で redaction/envelope 列が一致する
// -----------------------------------------------------------------------------

#[test]
fn r_at_06_determinism_100_runs_identical() {
    let base = temp_dir("at06");

    let run_once = |id: u128| -> Vec<tsumugi::AuditEnvelope> {
        let sink = Arc::new(InMemoryAuditSink::new());
        let engine = default_engine(&sink);
        let script = link_source(
            &engine,
            "m",
            "write_file(\"@data/det.txt\", \"payload\")\nlet c = read_file(\"@data/det.txt\")\n",
        );
        let linked = engine.link(&script, link_request()).expect("link");
        let mut ctx = EmbeddingContext::new(&engine);
        let _ = engine.run_audited(
            &linked,
            &mut ctx,
            run_request(id, fs_capabilities(rw_fs(&base))),
        );
        sink.snapshot()
    };

    // 比較対象は redaction 結果と envelope の semantic 列（sequence + event）。engine 内部の
    // clock identity（`deadline.clock_id`）だけは FakeClock を run ごとに作るため動くが、これは
    // audit semantic stream の一部ではない（§15.1 の正規化対象）。redaction（Path digest・
    // payload ツリー）と event 列が 100 回一致することを固定する。
    let semantic = |envelopes: &[tsumugi::AuditEnvelope]| -> Vec<(u64, String)> {
        envelopes
            .iter()
            .map(|e| (e.sequence, format!("{:?}", e.event)))
            .collect()
    };

    let baseline_env = run_once(600);
    let baseline = semantic(&baseline_env);
    let baseline_hc: Vec<String> = host_call_events(&baseline_env)
        .iter()
        .map(|e| format!("{e:?}"))
        .collect();
    assert!(!baseline_hc.is_empty());
    // ExecutionStarted の redaction_policy_id と deadline 以外は event に clock 依存が無い。
    // baseline の ExecutionStarted を除く全 event が clock-free であることを前提に比較する。

    for _ in 0..100 {
        let again_env = run_once(600);
        // host-call triple（redaction 本体）は完全一致。
        let again_hc: Vec<String> = host_call_events(&again_env)
            .iter()
            .map(|e| format!("{e:?}"))
            .collect();
        assert_eq!(
            baseline_hc, again_hc,
            "redaction（Path digest・payload）は 100 回一致（R-AT-06）"
        );
        // ExecutionStarted（clock_id を含む deadline）を除く semantic 列も一致。
        assert_eq!(
            &baseline[1..],
            &semantic(&again_env)[1..],
            "ExecutionStarted 以外の envelope semantic 列は 100 回一致（R-AT-06）"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}

// -----------------------------------------------------------------------------
// R-AT-09: Terminal/error payload は安定 code/message_id/位置だけで script 可視 message を含まない
// -----------------------------------------------------------------------------

#[test]
fn r_at_09_terminal_error_has_no_script_visible_message() {
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = default_engine(&sink);

    // 未捕捉 runtime error を起こす（0 除算）。error message の自由文は script 可視だが、
    // Terminal payload には安定 code / message_id / 位置だけが入り、自由文は入らない（§9.1）。
    let script = link_source(&engine, "m", "let x = 1 / 0\n");
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(&linked, &mut ctx, run_request(901, CapabilitySet::empty()));
    assert!(
        matches!(
            audited.outcome(),
            Some(EmbeddingOutcome::RuntimeError { .. })
        ),
        "0 除算は RuntimeError terminal: {:?}",
        audited.outcome()
    );

    let envelopes = sink.snapshot();
    // Terminal は末尾。error payload があることを確認する。
    let terminal = &envelopes.last().expect("non-empty").event;
    match terminal {
        AuditEvent::Terminal { error, .. } => {
            assert!(
                error.is_some(),
                "runtime error の Terminal は error payload を持つ"
            );
        }
        other => panic!("末尾は Terminal のはず: {other:?}"),
    }

    // script 可視の error message 自由文（日本語メッセージ）が payload に現れない。
    // tsumugi の 0 除算 message は「0 で除算」等の自由文。payload にはその自由文を載せない。
    assert_no_substring(&envelopes, "除算");
    assert_no_substring(&envelopes, "ゼロ");
}

// -----------------------------------------------------------------------------
// R-1b: body allow-list Reveal（R1b-AT-01〜05・07）。allow-list 入り policy は test shim で注入する。
// -----------------------------------------------------------------------------

fn body_key(op: &str, direction: AuditDirection, field: &str) -> AuditFieldKey {
    AuditFieldKey {
        operation: op.to_string(),
        direction,
        field: field.to_string(),
    }
}

fn reveal_policy(keys: Vec<AuditFieldKey>, max: u64) -> RedactionPolicy {
    RedactionPolicy {
        body_mode: RedactionMode::Reveal,
        reveal_body_fields: keys.into_iter().collect(),
        max_revealed_body_bytes: max,
        ..RedactionPolicy::default()
    }
}

/// `policy` を注入して script を実行し、host-call 3 event 列と全 envelope を返す。
fn run_with_policy(
    policy: RedactionPolicy,
    id: u128,
    text: &'static str,
    base: &std::path::Path,
) -> Vec<tsumugi::AuditEnvelope> {
    let sink = Arc::new(InMemoryAuditSink::new());
    let engine = EmbeddingEngine::builder()
        .audit_sink(sink.clone() as Arc<dyn AuditSink>)
        .audit_redaction_policy_for_test(policy)
        .build()
        .expect("build");
    let script = link_source(&engine, "m", text);
    let linked = engine.link(&script, link_request()).expect("link");
    let mut ctx = EmbeddingContext::new(&engine);
    let audited = engine.run_audited(
        &linked,
        &mut ctx,
        run_request(id, fs_capabilities(rw_fs(base))),
    );
    assert!(matches!(
        audited.outcome(),
        Some(EmbeddingOutcome::Completed { .. })
    ));
    sink.snapshot()
}

fn utf8(s: &str) -> AuditValue {
    AuditValue::Scalar(AuditScalar::Utf8(s.to_string()))
}

/// write_file の (request.body, arguments[1]) と read_file の response.body を取り出す。
fn write_bodies(envelopes: &[tsumugi::AuditEnvelope]) -> (AuditField, AuditField) {
    let hc = host_call_events(envelopes);
    match hc[0] {
        AuditEvent::HostCallStarted { request, .. } => {
            let body = request.fields.get("body").expect("body").clone();
            let args = match request.fields.get("arguments").expect("arguments") {
                AuditField::Revealed {
                    value: AuditValue::Sequence(items),
                    ..
                } => items.clone(),
                other => panic!("{other:?}"),
            };
            (body, args[1].clone())
        }
        other => panic!("{other:?}"),
    }
}

fn response_body(envelopes: &[tsumugi::AuditEnvelope], triple_index: usize) -> AuditField {
    let hc = host_call_events(envelopes);
    match hc[triple_index * 3 + 2] {
        AuditEvent::HostCallFinished { response, .. } => {
            response.fields.get("body").expect("body").clone()
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn r1b_at_01_allow_listed_body_within_limit_is_revealed() {
    let base = temp_dir("r1b01");
    let policy = reveal_policy(
        vec![
            body_key("write_file", AuditDirection::Request, "body"),
            body_key("write_file", AuditDirection::Request, "1"),
            body_key("read_file", AuditDirection::Response, "body"),
        ],
        16,
    );
    let envelopes = run_with_policy(
        policy,
        1101,
        "write_file(\"@data/b.txt\", \"HELLO-BODY\")\nlet c = read_file(\"@data/b.txt\")\n",
        &base,
    );
    let revealed = AuditField::Revealed {
        class: AuditFieldClass::Body,
        value: utf8("HELLO-BODY"),
        original_bytes: 10,
    };
    let (req_body, arg1) = write_bodies(&envelopes);
    assert_eq!(req_body, revealed);
    assert_eq!(arg1, revealed);
    assert_eq!(response_body(&envelopes, 1), revealed);
    // write の response は body を持たない（Omitted のまま）。
    assert!(matches!(
        response_body(&envelopes, 0),
        AuditField::Omitted {
            class: AuditFieldClass::Body,
            original_bytes: 0
        }
    ));
    // path は allow-list と無関係に HMAC のまま、raw path は漏れない。
    assert!(matches!(
        resource_path(&host_call_events(&envelopes)[0..3]),
        AuditField::HmacSha256 { .. }
    ));
    assert_no_substring(&envelopes, "b.txt");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r1b_at_02_over_limit_body_is_omitted_without_prefix() {
    let base = temp_dir("r1b02");
    let policy = reveal_policy(
        vec![
            body_key("write_file", AuditDirection::Request, "body"),
            body_key("write_file", AuditDirection::Request, "1"),
        ],
        9, // "CANARYBODY" は 10 byte
    );
    let envelopes = run_with_policy(
        policy,
        1102,
        "write_file(\"@data/o.txt\", \"CANARYBODY\")\n",
        &base,
    );
    let omitted = AuditField::Omitted {
        class: AuditFieldClass::Body,
        original_bytes: 10,
    };
    let (req_body, arg1) = write_bodies(&envelopes);
    assert_eq!(req_body, omitted);
    assert_eq!(arg1, omitted);
    assert_no_substring(&envelopes, "CAN");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r1b_at_03_key_mismatch_is_omitted() {
    let base = temp_dir("r1b03");
    // direction 違い・field 違い・operation 違いのいずれも一致しない。
    let policy = reveal_policy(
        vec![
            body_key("write_file", AuditDirection::Response, "body"),
            body_key("write_file", AuditDirection::Request, "0"),
            body_key("append_file", AuditDirection::Request, "body"),
            body_key("append_file", AuditDirection::Request, "1"),
        ],
        100,
    );
    let envelopes = run_with_policy(
        policy,
        1103,
        "write_file(\"@data/m.txt\", \"MISMATCH\")\n",
        &base,
    );
    let (req_body, arg1) = write_bodies(&envelopes);
    for f in [req_body, arg1] {
        assert!(matches!(
            f,
            AuditField::Omitted {
                class: AuditFieldClass::Body,
                original_bytes: 8
            }
        ));
    }
    assert_no_substring(&envelopes, "MISMATCH");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r1b_at_04_path_argument_zero_is_never_body_revealed() {
    let base = temp_dir("r1b04");
    let policy = reveal_policy(
        vec![
            body_key("write_file", AuditDirection::Request, "0"),
            body_key("write_file", AuditDirection::Request, "1"),
        ],
        100,
    );
    let envelopes = run_with_policy(policy, 1104, "write_file(\"@data/p0.txt\", \"X\")\n", &base);
    let hc = host_call_events(&envelopes);
    match hc[0] {
        AuditEvent::HostCallStarted { request, .. } => match request.fields.get("arguments") {
            Some(AuditField::Revealed {
                value: AuditValue::Sequence(items),
                ..
            }) => {
                assert!(matches!(
                    items[0],
                    AuditField::Omitted {
                        class: AuditFieldClass::Path,
                        ..
                    }
                ));
                assert!(matches!(items[1], AuditField::Revealed { .. }));
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    assert_no_substring(&envelopes, "p0.txt");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r1b_at_05_non_reveal_body_mode_never_reveals() {
    let base = temp_dir("r1b05");
    for (i, mode) in [RedactionMode::Omit, RedactionMode::HmacSha256]
        .into_iter()
        .enumerate()
    {
        let policy = RedactionPolicy {
            body_mode: mode,
            ..reveal_policy(
                vec![
                    body_key("write_file", AuditDirection::Request, "body"),
                    body_key("write_file", AuditDirection::Request, "1"),
                ],
                100,
            )
        };
        let envelopes = run_with_policy(
            policy,
            1105 + i as u128,
            "write_file(\"@data/n.txt\", \"NOREVEAL\")\n",
            &base,
        );
        assert_no_substring(&envelopes, "NOREVEAL");
        let (req_body, _) = write_bodies(&envelopes);
        assert!(matches!(req_body, AuditField::Omitted { .. }));
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r1b_read_lines_response_reveals_each_line_in_order() {
    let base = temp_dir("r1bl");
    let policy = reveal_policy(
        vec![body_key("read_lines", AuditDirection::Response, "body")],
        100,
    );
    let envelopes = run_with_policy(
        policy,
        1107,
        "write_file(\"@data/l.txt\", \"a\\nbc\")\nlet c = read_lines(\"@data/l.txt\")\n",
        &base,
    );
    match response_body(&envelopes, 1) {
        AuditField::Revealed {
            class: AuditFieldClass::Body,
            value: AuditValue::Sequence(items),
            original_bytes: 3,
        } => {
            let texts: Vec<_> = items
                .iter()
                .map(|f| match f {
                    AuditField::Revealed {
                        value: AuditValue::Scalar(AuditScalar::Utf8(t)),
                        ..
                    } => t.clone(),
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(texts, vec!["a".to_string(), "bc".to_string()]);
        }
        other => panic!("read_lines response は行ごとの Sequence のはず: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn r1b_at_07_determinism_100_runs_identical_with_reveal() {
    let base = temp_dir("r1b07");
    let run_once = || -> Vec<(u64, String)> {
        let policy = reveal_policy(
            vec![
                body_key("write_file", AuditDirection::Request, "body"),
                body_key("read_file", AuditDirection::Response, "body"),
            ],
            64,
        );
        let envelopes = run_with_policy(
            policy,
            1108,
            "write_file(\"@data/d.txt\", \"det\")\nlet c = read_file(\"@data/d.txt\")\n",
            &base,
        );
        envelopes
            .iter()
            .skip(1) // ExecutionStarted は FakeClock の clock_id が run ごとに動く
            .map(|e| (e.sequence, format!("{:?}", e.event)))
            .collect()
    };
    let baseline = run_once();
    assert!(baseline.iter().any(|(_, e)| e.contains("Revealed")));
    for _ in 0..100 {
        assert_eq!(
            baseline,
            run_once(),
            "Reveal 結果は 100 回一致（R1b-AT-07）"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}
