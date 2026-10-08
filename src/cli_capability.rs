//! CLI の capability profile 解析と frozen `CapabilitySet` 構築（C9 / C10）。
//!
//! `tsumugi` CLI は safe（既定）/ legacy の 2 profile を持つ。本モジュールは:
//!
//! - CLI option grammar（`--profile` / `--allow-*` / `--fs-*` / `--allow-import-root`）の
//!   副作用前解析（[`parse_cli`]、`Result<CliOutcome, CliUsageError>`）。
//! - safe / legacy profile ごとの frozen [`tsumugi::CapabilitySet`] 構築
//!   （[`build_safe_capabilities`] / [`build_legacy_capabilities`]）。
//!
//! C10 により core evaluator / builtin は ambient OS access を一切持たない。CLI が本モジュールで
//! 組んだ frozen set を全実行経路へ注入することで、OS access は CLI adapter 境界へ一本化される。

use std::sync::Arc;

use tsumugi::{
    CapabilitySet, DataClassification, EnvironmentSnapshot, EnvironmentValue, FilesystemCapability,
    FilesystemRoot, FsOperation, MountName, OsDirectoryHandle, ProcessExit, SymlinkPolicy,
    SystemClock, SystemInput, SystemOutput, derive_policy_id,
};

/// 実行 backend（ツリーウォーク / VM）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Tree,
    Vm,
}

/// script source の取得元（AUD-018）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// 引数なし → REPL
    Repl,
    /// `-` → 標準入力から source 全体を読む
    Stdin,
    /// 通常の positional → ファイルパス
    File(String),
}

/// CLI capability profile（§14）。既定は [`Profile::Safe`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Safe,
    Legacy,
}

/// safe profile で受理する capability option（順序保持）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliCapabilityOptions {
    pub allow_env: Vec<String>,
    pub allow_clock: bool,
    pub allow_script_stdin: bool,
    pub allow_exit: bool,
    pub deny_stdout: bool,
    pub fs_roots: Vec<(String, String)>,
    pub fs_ops: Vec<(String, Vec<FsOperation>)>,
    pub import_roots: Vec<(String, String)>,
}

/// CLI 起動の確定結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliInvocation {
    pub backend: Backend,
    pub source: Source,
    pub script_args: Vec<String>,
    pub profile: Profile,
    pub capability_options: CliCapabilityOptions,
}

/// `parse_cli` の結果（§2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliOutcome {
    Run(CliInvocation),
    /// `--help`: usage を stdout、exit 0。
    Help,
    /// `--version`: バージョンを stdout、exit 0。
    Version,
}

/// usage error（exit code は常に 1、§14.1）。[`std::fmt::Display`] で日本語診断本文を返す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliUsageError {
    message: String,
}

impl CliUsageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CliUsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// `--help` / usage error 末尾に出す usage 文字列（§7.1、golden）。
pub const USAGE: &str = "\
使い方: tsumugi [オプション] [スクリプト [引数...]]

オプション:
  --vm                        バイトコード VM で実行する
  --profile safe|legacy       capability profile を選ぶ（既定: safe）
  --allow-env KEY             環境変数 KEY を公開する（複数指定可）
  --allow-clock               now() を許可する
  --allow-script-stdin        input() を許可する
  --allow-exit                exit() を許可する
  --deny-stdout               print の標準出力を拒否する
  --fs-root NAME=PATH         ファイルシステム root を mount する（--fs-op と対にする）
  --fs-op NAME=OP[,OP...]     mount NAME に操作を許可する（read|write|create|delete|metadata|list）
  --allow-import-root NAME=PATH  import root を宣言する（値検証のみ）
  --help                      この使い方を表示する
  --version                   バージョンを表示する
  --                          以降をオプションとして解釈しない";

// ---------------------------------------------------------------------------
// §2 解析
// ---------------------------------------------------------------------------

/// `--fs-op` が受理する 6 トークンを [`FsOperation`] の固定対応へ写す（§3 step 7、方針 A）。
///
/// `RecursiveDelete` / `Import` は公式 CLI では付与しない（受理トークンが無い）。
fn fs_op_token(token: &str) -> Option<FsOperation> {
    match token {
        "read" => Some(FsOperation::Read),
        "write" => Some(FsOperation::Write),
        "create" => Some(FsOperation::Create),
        "delete" => Some(FsOperation::Delete),
        "metadata" => Some(FsOperation::Metadata),
        "list" => Some(FsOperation::List),
        _ => None,
    }
}

/// `NAME=VALUE` を分割する。`=` が無ければ `None`。
fn split_name_value(spec: &str) -> Option<(&str, &str)> {
    spec.split_once('=')
}

/// 値を取る option が消費する次 token を取り出す。欠落（`None` or `--`）は usage error。
fn take_value<'a, I>(iter: &mut I, opt: &str) -> Result<&'a str, CliUsageError>
where
    I: Iterator<Item = &'a String>,
{
    match iter.next() {
        Some(v) if v != "--" => Ok(v.as_str()),
        _ => Err(CliUsageError::new(format!(
            "エラー: {} には値が必要です",
            opt
        ))),
    }
}

/// argv（program 名を除く）を CLI grammar に従って解析する（§2）。副作用前。
///
/// フェーズ 1（字句的・局所的、左→右 1 パス最初の 1 件）→ フェーズ 2（profile 確定後の大域
/// 整合）の 2 段で usage error を決定的に報告する（§2.2）。
pub fn parse_cli(argv: &[String]) -> Result<CliOutcome, CliUsageError> {
    let mut backend = Backend::Tree;
    let mut profile: Option<Profile> = None;
    let mut profile_seen = false;
    let mut opts = CliCapabilityOptions::default();
    // 不整合（フェーズ 2）報告用に、最初に出現した capability option を記録する。
    let mut first_capability_opt: Option<String> = None;
    let mark_cap = |name: &str, slot: &mut Option<String>| {
        if slot.is_none() {
            *slot = Some(name.to_string());
        }
    };

    let mut iter = argv.iter();
    let source = loop {
        let Some(token) = iter.next() else {
            break Source::Repl;
        };
        match token.as_str() {
            "--vm" => backend = Backend::Vm,
            "--help" => return Ok(CliOutcome::Help),
            "--version" => return Ok(CliOutcome::Version),
            "--" => match iter.next() {
                None => break Source::Repl,
                Some(s) if s == "-" => break Source::Stdin,
                Some(s) => break Source::File(s.clone()),
            },
            "-" => break Source::Stdin,
            "--profile" => {
                let value = take_value(&mut iter, "--profile")?;
                if profile_seen {
                    return Err(CliUsageError::new(
                        "エラー: --profile は 1 回だけ指定できます",
                    ));
                }
                profile_seen = true;
                profile = Some(match value {
                    "safe" => Profile::Safe,
                    "legacy" => Profile::Legacy,
                    other => {
                        return Err(CliUsageError::new(format!(
                            "エラー: --profile の値は safe か legacy です: {}",
                            other
                        )));
                    }
                });
            }
            "--allow-clock" => {
                mark_cap("--allow-clock", &mut first_capability_opt);
                if opts.allow_clock {
                    return Err(dup_bool("--allow-clock"));
                }
                opts.allow_clock = true;
            }
            "--allow-script-stdin" => {
                mark_cap("--allow-script-stdin", &mut first_capability_opt);
                if opts.allow_script_stdin {
                    return Err(dup_bool("--allow-script-stdin"));
                }
                opts.allow_script_stdin = true;
            }
            "--allow-exit" => {
                mark_cap("--allow-exit", &mut first_capability_opt);
                if opts.allow_exit {
                    return Err(dup_bool("--allow-exit"));
                }
                opts.allow_exit = true;
            }
            "--deny-stdout" => {
                mark_cap("--deny-stdout", &mut first_capability_opt);
                if opts.deny_stdout {
                    return Err(dup_bool("--deny-stdout"));
                }
                opts.deny_stdout = true;
            }
            "--allow-env" => {
                mark_cap("--allow-env", &mut first_capability_opt);
                let key = take_value(&mut iter, "--allow-env")?;
                // 固定順（値形式 > protected > 重複）。--allow-env に値形式制約は無いので
                // protected → 重複 の順で判定する（§2.2 フェーズ 1、finding 6）。
                if tsumugi_is_protected(key) {
                    return Err(CliUsageError::new(format!(
                        "エラー: --allow-env に保護されたキーは指定できません: {}",
                        key
                    )));
                }
                let norm = tsumugi_normalize_env_key(key);
                if opts
                    .allow_env
                    .iter()
                    .any(|k| tsumugi_normalize_env_key(k) == norm)
                {
                    return Err(CliUsageError::new(format!(
                        "エラー: --allow-env のキーが重複しています: {}",
                        key
                    )));
                }
                opts.allow_env.push(key.to_string());
            }
            "--fs-root" => {
                mark_cap("--fs-root", &mut first_capability_opt);
                let spec = take_value(&mut iter, "--fs-root")?;
                let Some((name, path)) = split_name_value(spec) else {
                    return Err(CliUsageError::new(format!(
                        "エラー: --fs-root は NAME=PATH の形式で指定します: {}",
                        spec
                    )));
                };
                if MountName::new(name).is_err() {
                    return Err(CliUsageError::new(format!(
                        "エラー: --fs-root の名前が不正です: {}",
                        name
                    )));
                }
                if opts.fs_roots.iter().any(|(n, _)| n == name) {
                    return Err(CliUsageError::new(format!(
                        "エラー: --fs-root の名前が重複しています: {}",
                        name
                    )));
                }
                opts.fs_roots.push((name.to_string(), path.to_string()));
            }
            "--fs-op" => {
                mark_cap("--fs-op", &mut first_capability_opt);
                let spec = take_value(&mut iter, "--fs-op")?;
                let Some((name, ops_str)) = split_name_value(spec) else {
                    return Err(CliUsageError::new(format!(
                        "エラー: --fs-op は NAME=OP[,OP...] の形式で指定します: {}",
                        spec
                    )));
                };
                if MountName::new(name).is_err() {
                    return Err(CliUsageError::new(format!(
                        "エラー: --fs-op の名前が不正です: {}",
                        name
                    )));
                }
                if opts.fs_ops.iter().any(|(n, _)| n == name) {
                    return Err(CliUsageError::new(format!(
                        "エラー: --fs-op の名前が重複しています: {}",
                        name
                    )));
                }
                let mut ops = Vec::new();
                for op_token in ops_str.split(',') {
                    match fs_op_token(op_token) {
                        Some(op) => ops.push(op),
                        None => {
                            return Err(CliUsageError::new(format!(
                                "エラー: --fs-op の操作が不正です: {}",
                                op_token
                            )));
                        }
                    }
                }
                opts.fs_ops.push((name.to_string(), ops));
            }
            "--allow-import-root" => {
                mark_cap("--allow-import-root", &mut first_capability_opt);
                let spec = take_value(&mut iter, "--allow-import-root")?;
                let Some((name, path)) = split_name_value(spec) else {
                    return Err(CliUsageError::new(format!(
                        "エラー: --allow-import-root は NAME=PATH の形式で指定します: {}",
                        spec
                    )));
                };
                if MountName::new(name).is_err() {
                    return Err(CliUsageError::new(format!(
                        "エラー: --allow-import-root の名前が不正です: {}",
                        name
                    )));
                }
                if opts.import_roots.iter().any(|(n, _)| n == name) {
                    return Err(CliUsageError::new(format!(
                        "エラー: --allow-import-root の名前が重複しています: {}",
                        name
                    )));
                }
                opts.import_roots.push((name.to_string(), path.to_string()));
            }
            unknown if unknown.starts_with("--") => {
                return Err(CliUsageError::new(format!(
                    "エラー: 不明なオプションです: {}",
                    unknown
                )));
            }
            _ => break Source::File(token.clone()),
        }
    };

    // SCRIPT 確定後の残り token はすべて verbatim に script args とする。
    let script_args: Vec<String> = iter.cloned().collect();

    // --- フェーズ 2: profile 確定後の大域整合 ---
    let profile = profile.unwrap_or(Profile::Safe);

    // ① profile × capability option 整合（legacy + capability option）。
    if profile == Profile::Legacy
        && let Some(opt) = &first_capability_opt
    {
        return Err(CliUsageError::new(format!(
            "エラー: capability オプションは safe profile でのみ指定できます: {}",
            opt
        )));
    }

    // ② --fs-root / --fs-op の 1:1（NAME 集合一致、固定順）。
    for (name, _) in &opts.fs_roots {
        if !opts.fs_ops.iter().any(|(n, _)| n == name) {
            return Err(CliUsageError::new(format!(
                "エラー: --fs-root と --fs-op は同じ名前で対にします: {}",
                name
            )));
        }
    }
    for (name, _) in &opts.fs_ops {
        if !opts.fs_roots.iter().any(|(n, _)| n == name) {
            return Err(CliUsageError::new(format!(
                "エラー: --fs-root と --fs-op は同じ名前で対にします: {}",
                name
            )));
        }
    }

    Ok(CliOutcome::Run(CliInvocation {
        backend,
        source,
        script_args,
        profile,
        capability_options: opts,
    }))
}

fn dup_bool(opt: &str) -> CliUsageError {
    CliUsageError::new(format!("エラー: {} は 1 回だけ指定できます", opt))
}

// §5 OS-aware env key 判定（argv のみ依存、process env を読まない）。

/// `TSUMUGI_` prefix を protected として判定する（Windows は case-insensitive）。
fn tsumugi_is_protected(key: &str) -> bool {
    const PREFIX: &str = "TSUMUGI_";
    #[cfg(windows)]
    {
        key.to_uppercase().starts_with(PREFIX)
    }
    #[cfg(not(windows))]
    {
        key.starts_with(PREFIX)
    }
}

/// env key を OS 規則で正規化する（Windows は uppercase、他 OS はそのまま）。
fn tsumugi_normalize_env_key(key: &str) -> String {
    #[cfg(windows)]
    {
        key.to_uppercase()
    }
    #[cfg(not(windows))]
    {
        key.to_string()
    }
}

// ---------------------------------------------------------------------------
// §3 safe profile builder
// ---------------------------------------------------------------------------

/// 構成記述を安定 bytes へ直列化して policy_id を導出するための buffer を作る。
fn policy_seed(tag: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(tag);
    for part in parts {
        buf.extend_from_slice(&(part.len() as u64).to_be_bytes());
        buf.extend_from_slice(part);
    }
    buf
}

/// safe profile の frozen [`CapabilitySet`] を構築する（§3）。
///
/// empty から始め、option に応じて stdout（既定）/ clock / stdin / exit / env snapshot /
/// filesystem root を grant する。`--allow-import-root` は値検証のみで set には寄与しない。
pub fn build_safe_capabilities(
    opts: &CliCapabilityOptions,
) -> Result<CapabilitySet, CliUsageError> {
    let mut builder = CapabilitySet::builder();

    // stdout 既定（--deny-stdout が無ければ）。
    if !opts.deny_stdout {
        let pid = derive_policy_id(&policy_seed(b"stdout", &[]));
        builder = builder
            .stdout(Arc::new(SystemOutput::new(pid)))
            .expect("single grant never duplicates");
    }

    if opts.allow_clock {
        let pid = derive_policy_id(&policy_seed(b"clock", &[]));
        builder = builder
            .clock(Arc::new(SystemClock::new(pid)))
            .expect("single grant never duplicates");
    }

    if opts.allow_script_stdin {
        let pid = derive_policy_id(&policy_seed(b"stdin", &[]));
        builder = builder
            .stdin(Arc::new(SystemInput::new(pid)))
            .expect("single grant never duplicates");
    }

    if opts.allow_exit {
        let pid = derive_policy_id(&policy_seed(b"exit", &[]));
        builder = builder
            .process_exit(ProcessExit::new(pid))
            .expect("single grant never duplicates");
    }

    // --allow-env: execution 作成時に OS key 同一性で process env を snapshot。
    if !opts.allow_env.is_empty() {
        let snapshot = snapshot_requested_env(&opts.allow_env)?;
        builder = builder
            .environment(snapshot)
            .expect("single grant never duplicates");
    }

    // --fs-root + --fs-op（1:1 は parse_cli 済み）→ 各 root を DenyAll で open。
    if !opts.fs_roots.is_empty() {
        let fs = build_filesystem(&opts.fs_roots, &opts.fs_ops, SymlinkPolicy::DenyAll)?;
        builder = builder
            .filesystem(fs)
            .expect("single grant never duplicates");
    }

    // --allow-import-root は値検証のみ（parse_cli 済み）。set へは寄与しない。

    Ok(builder.build())
}

/// `--allow-env KEY...` で要求された key を OS key 同一性で process env から snapshot する。
fn snapshot_requested_env(keys: &[String]) -> Result<EnvironmentSnapshot, CliUsageError> {
    let mut entries: Vec<(String, EnvironmentValue)> = Vec::new();
    for key in keys {
        let norm = tsumugi_normalize_env_key(key);
        // OS key 同一性で lookup する。Windows は case-insensitive のため normalize した key で
        // process env を走査する。他 OS は exact match（std::env::var と同じ）。
        if let Some(value) = lookup_env_os(&norm) {
            // snapshot key 検証（UTF-8・1..=256 bytes・NUL なし）を満たさないものは落とす。
            if norm.is_empty() || norm.len() > 256 || norm.as_bytes().contains(&0) {
                continue;
            }
            let value = EnvironmentValue::new(value, DataClassification::Public)
                .map_err(|_| CliUsageError::new("エラー: 環境変数の値が不正です"))?;
            entries.push((norm, value));
        }
    }
    EnvironmentSnapshot::from_entries(entries)
        .map_err(|_| CliUsageError::new("エラー: 環境変数 snapshot の構築に失敗しました"))
}

/// 正規化済み key で process env を引く（Windows は case-insensitive 走査）。
fn lookup_env_os(norm_key: &str) -> Option<String> {
    #[cfg(windows)]
    {
        std::env::vars().find_map(|(k, v)| (k.to_uppercase() == norm_key).then_some(v))
    }
    #[cfg(not(windows))]
    {
        std::env::var(norm_key).ok()
    }
}

/// safe/legacy 共通の filesystem capability 構築。各 root を指定 symlink policy で open する。
fn build_filesystem(
    fs_roots: &[(String, String)],
    fs_ops: &[(String, Vec<FsOperation>)],
    symlink_policy: SymlinkPolicy,
) -> Result<FilesystemCapability, CliUsageError> {
    let mut roots = Vec::new();
    for (name, path) in fs_roots {
        let ops = fs_ops
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, ops)| ops.clone())
            .unwrap_or_default();
        let operations: std::collections::BTreeSet<FsOperation> = ops.into_iter().collect();
        let mount = MountName::new(name)
            .map_err(|_| CliUsageError::new(format!("エラー: mount 名が不正です: {}", name)))?;
        let abs = absolutize_lexical(path);
        let pid = derive_policy_id(&policy_seed(
            b"fs",
            &[name.as_bytes(), abs.to_string_lossy().as_bytes()],
        ));
        let handle = OsDirectoryHandle::new(abs, pid, symlink_policy);
        let root = FilesystemRoot::new(mount, pid, operations, symlink_policy, Arc::new(handle))
            .map_err(|_| {
                CliUsageError::new(format!(
                    "エラー: ファイルシステム root の構成が不正です: {}",
                    name
                ))
            })?;
        roots.push(root);
    }
    FilesystemCapability::new(roots)
        .map_err(|_| CliUsageError::new("エラー: ファイルシステム構成が不正です"))
}

/// path を CWD 基準で絶対化する（lexical。canonicalize しない、§4.2 finding 3）。
fn absolutize_lexical(path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        lexical_normalize(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
        lexical_normalize(&cwd.join(p))
    }
}

/// `.` / `..` を lexical に解決する（symlink 非追従）。
fn lexical_normalize(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// §4 legacy profile builder
// ---------------------------------------------------------------------------

/// legacy profile 構築の結果（frozen set と stderr へ出す警告）。
pub struct LegacyBuild {
    pub capabilities: CapabilitySet,
    pub warnings: Vec<String>,
}

/// 空 sandbox 警告（§7.3）。
pub const WARN_EMPTY_SANDBOX: &str = "警告: TSUMUGI_SANDBOX が未設定のため、legacy profile はファイルシステム全体へのアクセスを許可します";
/// 空 env-allow 警告（§7.3）。
pub const WARN_EMPTY_ENV_ALLOW: &str = "警告: TSUMUGI_ENV_ALLOW が未設定のため、legacy profile は TSUMUGI_ 以外の全環境変数を公開します";

/// legacy profile の frozen [`CapabilitySet`] を構築する（§4）。
///
/// CLI adapter が `TSUMUGI_SANDBOX` / `TSUMUGI_ENV_ALLOW` をその場で 1 回だけ読む
/// （process-global OnceLock 不使用）。env/clock/stdin/stdout/exit を grant し、sandbox 設定に
/// 応じて filesystem を構築する。
pub fn build_legacy_capabilities() -> Result<LegacyBuild, CliUsageError> {
    let mut warnings = Vec::new();
    let mut builder = CapabilitySet::builder();

    // clock / stdin / stdout / exit を grant（safe と同じ adapter）。
    builder = builder
        .stdout(Arc::new(SystemOutput::new(derive_policy_id(&policy_seed(
            b"legacy-stdout",
            &[],
        )))))
        .expect("single grant never duplicates")
        .clock(Arc::new(SystemClock::new(derive_policy_id(&policy_seed(
            b"legacy-clock",
            &[],
        )))))
        .expect("single grant never duplicates")
        .stdin(Arc::new(SystemInput::new(derive_policy_id(&policy_seed(
            b"legacy-stdin",
            &[],
        )))))
        .expect("single grant never duplicates")
        .process_exit(ProcessExit::new(derive_policy_id(&policy_seed(
            b"legacy-exit",
            &[],
        ))))
        .expect("single grant never duplicates");

    // env snapshot: TSUMUGI_ 除外 + TSUMUGI_ENV_ALLOW 適用。未設定/空は全 snapshot + 警告。
    let env_allow = std::env::var("TSUMUGI_ENV_ALLOW")
        .ok()
        .filter(|v| !v.is_empty());
    if env_allow.is_none() {
        warnings.push(WARN_EMPTY_ENV_ALLOW.to_string());
    }
    let snapshot = legacy_env_snapshot(env_allow.as_deref());
    builder = builder
        .environment(snapshot)
        .expect("single grant never duplicates");

    // filesystem: TSUMUGI_SANDBOX 設定 → mount 構築、未設定/空 → unrestricted + 警告。
    let sandbox = std::env::var("TSUMUGI_SANDBOX")
        .ok()
        .filter(|v| !v.is_empty());
    match sandbox {
        Some(spec) => {
            let fs = build_legacy_sandbox_fs(&spec)?;
            builder = builder
                .filesystem(fs)
                .expect("single grant never duplicates");
        }
        None => {
            warnings.push(WARN_EMPTY_SANDBOX.to_string());
            let fs = build_legacy_unrestricted_fs()?;
            builder = builder
                .filesystem(fs)
                .expect("single grant never duplicates");
        }
    }

    Ok(LegacyBuild {
        capabilities: builder.build(),
        warnings,
    })
}

/// legacy の env snapshot を作る。`TSUMUGI_` 除外 + allow-list 適用（未設定なら全 key）。
fn legacy_env_snapshot(env_allow: Option<&str>) -> EnvironmentSnapshot {
    let patterns: Option<Vec<String>> = env_allow.map(|spec| {
        spec.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });
    let is_allowed = |key: &str| -> bool {
        let Some(patterns) = &patterns else {
            return true;
        };
        for pattern in patterns {
            if let Some(prefix) = pattern.strip_suffix('*') {
                if key.starts_with(prefix) {
                    return true;
                }
            } else if pattern == key {
                return true;
            }
        }
        false
    };

    let entries = std::env::vars().filter_map(|(key, value)| {
        if tsumugi_is_protected(&key) || !is_allowed(&key) {
            return None;
        }
        let key = tsumugi_normalize_env_key(&key);
        if key.is_empty() || key.len() > 256 || key.as_bytes().contains(&0) {
            return None;
        }
        let value = EnvironmentValue::new(value, DataClassification::Public).ok()?;
        Some((key, value))
    });
    EnvironmentSnapshot::from_entries(entries).unwrap_or_else(|_| EnvironmentSnapshot::empty())
}

/// `TSUMUGI_SANDBOX` 設定時の legacy filesystem を構築する（§4.2）。
///
/// comma 区切りの host root を CWD 基準で絶対化（lexical）し、`legacy{N}` mount（root 1 個なら
/// `default` alias も）を `FollowWithinRoot` で open する。
fn build_legacy_sandbox_fs(spec: &str) -> Result<FilesystemCapability, CliUsageError> {
    let hosts: Vec<String> = spec
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if hosts.is_empty() {
        // comma のみ等で実質空 → unrestricted へは倒さず error 回避のため unrestricted を使う。
        return build_legacy_unrestricted_fs();
    }

    let all_fs_ops = [
        FsOperation::Read,
        FsOperation::Write,
        FsOperation::Create,
        FsOperation::Delete,
        FsOperation::Metadata,
        FsOperation::List,
        FsOperation::RecursiveDelete,
    ];

    let mut roots = Vec::new();
    let mut translation_roots: Vec<(MountName, std::path::PathBuf)> = Vec::new();
    let single = hosts.len() == 1;
    for (index, host) in hosts.iter().enumerate() {
        let abs = absolutize_lexical(host);
        let operations: std::collections::BTreeSet<FsOperation> =
            all_fs_ops.iter().copied().collect();
        let mount_names: Vec<String> = if single {
            vec![format!("legacy{}", index), "default".to_string()]
        } else {
            vec![format!("legacy{}", index)]
        };
        for mount_name in mount_names {
            let pid = derive_policy_id(&policy_seed(
                b"legacy-fs",
                &[mount_name.as_bytes(), abs.to_string_lossy().as_bytes()],
            ));
            let mount = MountName::new(&mount_name).map_err(|_| {
                CliUsageError::new(format!("エラー: legacy mount 名が不正です: {}", mount_name))
            })?;
            let handle = OsDirectoryHandle::new(abs.clone(), pid, SymlinkPolicy::FollowWithinRoot);
            let root = FilesystemRoot::new(
                mount.clone(),
                pid,
                operations.clone(),
                SymlinkPolicy::FollowWithinRoot,
                Arc::new(handle),
            )
            .map_err(|_| {
                CliUsageError::new("エラー: legacy ファイルシステム root の構成が不正です")
            })?;
            roots.push(root);
            // host path 翻訳表には primary mount（legacy{N}）だけを載せる。`default` alias は
            // 同一 root の別名なので最長一致の重複判定を避けるため翻訳表には加えない。
            if mount.as_str().starts_with("legacy") {
                translation_roots.push((mount, abs.clone()));
            }
        }
    }
    FilesystemCapability::new_legacy(roots, translation_roots).map_err(|e| {
        if matches!(
            e,
            tsumugi::ConfigError::InvalidFilesystemPolicy {
                code: "ambiguous_legacy_root"
            }
        ) {
            // §7.4: 同深さ複数一致（同一絶対 path の root）は曖昧エラー → exit 1。
            CliUsageError::new(format!(
                "エラー: TSUMUGI_SANDBOX のパスが複数の root に同じ深さで一致します: {}",
                spec
            ))
        } else {
            CliUsageError::new("エラー: legacy ファイルシステム構成が不正です")
        }
    })
}

/// `TSUMUGI_SANDBOX` 未設定時の legacy filesystem（unrestricted namespace）を構築する（§4.3）。
///
/// root `/`（Unix）/ CWD の volume root（Windows）を `FollowWithinRoot` で開く。unqualified path
/// 解決のため `default` mount に割り当てる。
fn build_legacy_unrestricted_fs() -> Result<FilesystemCapability, CliUsageError> {
    let root_path = fs_root_path();
    let all_fs_ops: std::collections::BTreeSet<FsOperation> = [
        FsOperation::Read,
        FsOperation::Write,
        FsOperation::Create,
        FsOperation::Delete,
        FsOperation::Metadata,
        FsOperation::List,
        FsOperation::RecursiveDelete,
    ]
    .into_iter()
    .collect();

    let pid = derive_policy_id(&policy_seed(
        b"legacy-unrestricted",
        &[root_path.to_string_lossy().as_bytes()],
    ));
    let mount = MountName::new("default")
        .map_err(|_| CliUsageError::new("エラー: legacy mount 名が不正です"))?;
    let handle = OsDirectoryHandle::new(root_path.clone(), pid, SymlinkPolicy::FollowWithinRoot);
    let root = FilesystemRoot::new(
        mount.clone(),
        pid,
        all_fs_ops,
        SymlinkPolicy::FollowWithinRoot,
        Arc::new(handle),
    )
    .map_err(|_| CliUsageError::new("エラー: legacy ファイルシステム root の構成が不正です"))?;
    // unrestricted は root `/`（Unix）/ volume root（Windows）に `default` を割り当てる。
    // host path 翻訳表にもこの root を載せ、絶対パスがそのまま default 配下へ route されるようにする。
    FilesystemCapability::new_legacy(vec![root], vec![(mount, root_path)])
        .map_err(|_| CliUsageError::new("エラー: legacy ファイルシステム構成が不正です"))
}

/// unrestricted legacy filesystem の root path（Unix は `/`、Windows は CWD の volume root）。
fn fs_root_path() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        use std::path::Component;
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("C:\\"));
        let mut root = std::path::PathBuf::new();
        for comp in cwd.components() {
            match comp {
                Component::Prefix(_) | Component::RootDir => root.push(comp.as_os_str()),
                _ => break,
            }
        }
        if root.as_os_str().is_empty() {
            std::path::PathBuf::from("C:\\")
        } else {
            root
        }
    }
    #[cfg(not(windows))]
    {
        std::path::PathBuf::from("/")
    }
}

// ---------------------------------------------------------------------------
// §9 parse_cli 単体テスト（副作用前・exit 1、2 フェーズ順序、固定優先順位）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod parse_tests {
    use super::*;

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn parse(items: &[&str]) -> Result<CliOutcome, CliUsageError> {
        parse_cli(&strs(items))
    }

    fn run(items: &[&str]) -> CliInvocation {
        match parse(items).expect("Run を期待") {
            CliOutcome::Run(inv) => inv,
            other => panic!("Run を期待したが {:?}", other),
        }
    }

    fn err_msg(items: &[&str]) -> String {
        match parse(items) {
            Err(e) => e.to_string(),
            Ok(o) => panic!("usage error を期待したが {:?}", o),
        }
    }

    // --- 既存 grammar（backend / source / args）の回帰 ---

    #[test]
    fn no_args_starts_tree_repl_safe() {
        let inv = run(&[]);
        assert_eq!(inv.backend, Backend::Tree);
        assert_eq!(inv.source, Source::Repl);
        assert_eq!(inv.profile, Profile::Safe);
        assert!(inv.script_args.is_empty());
    }

    #[test]
    fn vm_flag_before_script_selects_vm() {
        let inv = run(&["--vm", "app.tsg", "a", "--vm"]);
        assert_eq!(inv.backend, Backend::Vm);
        assert_eq!(inv.source, Source::File("app.tsg".to_string()));
        assert_eq!(inv.script_args, strs(&["a", "--vm"]));
    }

    #[test]
    fn double_dash_ends_option_parsing() {
        let inv = run(&["--", "app.tsg", "--help"]);
        assert_eq!(inv.source, Source::File("app.tsg".to_string()));
        assert_eq!(inv.script_args, strs(&["--help"]));
    }

    #[test]
    fn dash_reads_stdin_source() {
        let inv = run(&["-", "a", "b"]);
        assert_eq!(inv.source, Source::Stdin);
        assert_eq!(inv.script_args, strs(&["a", "b"]));
    }

    #[test]
    fn tokens_after_script_are_verbatim() {
        let inv = run(&["app.tsg", "--vm", "--profile", "legacy"]);
        assert_eq!(inv.backend, Backend::Tree);
        assert_eq!(inv.source, Source::File("app.tsg".to_string()));
        assert_eq!(inv.script_args, strs(&["--vm", "--profile", "legacy"]));
        // script 確定後の --profile は再解釈されないので profile は既定 safe のまま。
        assert_eq!(inv.profile, Profile::Safe);
    }

    #[test]
    fn vm_flag_is_idempotent() {
        let inv = run(&["--vm", "--vm", "app.tsg"]);
        assert_eq!(inv.backend, Backend::Vm);
    }

    #[test]
    fn help_and_version_are_terminal() {
        assert_eq!(parse(&["--help"]).unwrap(), CliOutcome::Help);
        assert_eq!(parse(&["--version"]).unwrap(), CliOutcome::Version);
        // SCRIPT 後の --help は arg（terminal にしない）。
        let inv = run(&["app.tsg", "--help"]);
        assert_eq!(inv.script_args, strs(&["--help"]));
    }

    // --- profile ---

    #[test]
    fn profile_values_and_errors() {
        assert_eq!(run(&["--profile", "safe"]).profile, Profile::Safe);
        assert_eq!(run(&["--profile", "legacy"]).profile, Profile::Legacy);
        assert!(err_msg(&["--profile", "weird"]).contains("--profile の値は safe か legacy"));
        assert!(err_msg(&["--profile", "safe", "--profile", "safe"]).contains("1 回だけ"));
        assert!(err_msg(&["--profile"]).contains("値が必要"));
    }

    // --- capability options（safe） ---

    #[test]
    fn boolean_options_parse_and_reject_duplicates() {
        let inv = run(&["--allow-clock", "--allow-script-stdin", "--allow-exit"]);
        assert!(inv.capability_options.allow_clock);
        assert!(inv.capability_options.allow_script_stdin);
        assert!(inv.capability_options.allow_exit);
        assert!(err_msg(&["--allow-clock", "--allow-clock"]).contains("1 回だけ"));
        assert!(err_msg(&["--deny-stdout", "--deny-stdout"]).contains("1 回だけ"));
    }

    #[test]
    fn unknown_option_is_usage_error() {
        assert!(err_msg(&["--nope"]).contains("不明なオプションです"));
    }

    #[test]
    fn fs_root_and_fs_op_pairing() {
        let inv = run(&["--fs-root", "data=/tmp/x", "--fs-op", "data=read,write"]);
        assert_eq!(
            inv.capability_options.fs_roots,
            vec![("data".to_string(), "/tmp/x".to_string())]
        );
        assert_eq!(
            inv.capability_options.fs_ops,
            vec![(
                "data".to_string(),
                vec![FsOperation::Read, FsOperation::Write]
            )]
        );
    }

    #[test]
    fn fs_root_without_fs_op_is_error() {
        assert!(err_msg(&["--fs-root", "data=/tmp/x"]).contains("同じ名前で対にします"));
    }

    #[test]
    fn fs_op_without_fs_root_is_error() {
        assert!(err_msg(&["--fs-op", "data=read"]).contains("同じ名前で対にします"));
    }

    #[test]
    fn fs_op_rejects_import_and_unknown_tokens() {
        assert!(
            err_msg(&["--fs-root", "d=/tmp", "--fs-op", "d=import"]).contains("操作が不正です")
        );
        assert!(
            err_msg(&["--fs-root", "d=/tmp", "--fs-op", "d=recurse"]).contains("操作が不正です")
        );
    }

    #[test]
    fn fs_op_accepts_all_six_tokens() {
        let inv = run(&[
            "--fs-root",
            "d=/tmp",
            "--fs-op",
            "d=read,write,create,delete,metadata,list",
        ]);
        let ops = &inv.capability_options.fs_ops[0].1;
        assert_eq!(
            ops,
            &vec![
                FsOperation::Read,
                FsOperation::Write,
                FsOperation::Create,
                FsOperation::Delete,
                FsOperation::Metadata,
                FsOperation::List,
            ]
        );
    }

    #[test]
    fn fs_root_name_validation_and_duplicate() {
        assert!(err_msg(&["--fs-root", "1bad=/tmp"]).contains("名前が不正"));
        assert!(
            err_msg(&[
                "--fs-root",
                "d=/a",
                "--fs-root",
                "d=/b",
                "--fs-op",
                "d=read"
            ])
            .contains("名前が重複")
        );
    }

    #[test]
    fn allow_env_duplicate_and_protected() {
        let inv = run(&["--allow-env", "HOME", "--allow-env", "PATH"]);
        assert_eq!(inv.capability_options.allow_env, strs(&["HOME", "PATH"]));
        assert!(err_msg(&["--allow-env", "HOME", "--allow-env", "HOME"]).contains("重複"));
        assert!(err_msg(&["--allow-env", "TSUMUGI_X"]).contains("保護されたキー"));
    }

    #[test]
    fn allow_env_protected_before_duplicate() {
        // 固定順（値形式 > protected > 重複）: TSUMUGI_X 2 回は protected を先に報告（finding 6）。
        assert!(
            err_msg(&["--allow-env", "TSUMUGI_X", "--allow-env", "TSUMUGI_X"])
                .contains("保護されたキー")
        );
    }

    #[test]
    fn allow_import_root_is_value_checked_only() {
        let inv = run(&["--allow-import-root", "foo=/some/path"]);
        assert_eq!(
            inv.capability_options.import_roots,
            vec![("foo".to_string(), "/some/path".to_string())]
        );
        assert!(err_msg(&["--allow-import-root", "1bad=/x"]).contains("名前が不正"));
    }

    // --- profile × capability 整合（フェーズ 2） ---

    #[test]
    fn legacy_with_capability_option_is_error() {
        assert!(
            err_msg(&["--profile", "legacy", "--allow-clock"])
                .contains("capability オプションは safe profile でのみ")
        );
    }

    // --- 2 フェーズ順序の決定性（finding 5） ---

    #[test]
    fn phase1_profile_duplicate_before_phase2_mismatch() {
        // --profile legacy --allow-env FOO --profile safe:
        // フェーズ 1 の --profile 重複が先に報告され、legacy+capability 不整合（フェーズ 2）へ進まない。
        assert!(
            err_msg(&[
                "--profile",
                "legacy",
                "--allow-env",
                "FOO",
                "--profile",
                "safe"
            ])
            .contains("--profile は 1 回だけ")
        );
    }

    #[test]
    fn phase2_mismatch_before_fs_pairing() {
        // legacy + capability 不整合（フェーズ 2 ①）が --fs-root/--fs-op 1:1（②）より先。
        let msg = err_msg(&["--profile", "legacy", "--fs-root", "d=/tmp"]);
        assert!(msg.contains("capability オプションは safe profile でのみ"));
    }
}
