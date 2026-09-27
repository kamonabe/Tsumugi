//! 埋め込み利用向けの最小実行 API。
//!
//! `Engine` はソースをパースして `CompiledScript` を作成し、
//! `ExecutionContext` は実行間で保持する状態を管理する。
//!
//! # 実行モデル（REV-015 Slice 3 PR-a）
//!
//! 実行制御仕様（`docs/execution-control.md`）第9節の state machine surface を公開する。
//! `Engine::create_execution` / `Engine::start` が [`ExecutionHandle`] を返し、host は
//! [`ExecutionHandle::poll`] で実行を進め、[`PollResult`] を観測する。
//!
//! **PR-a の実装範囲**: 公開型と状態機械の骨格を導入する。この PR の `poll` は初回に
//! 既存のツリーウォーク評価器を terminal まで一気に回して [`PollResult::Terminal`] を
//! 返す（continuation 分割・slice fuel での yield は後続 PR-b〜d）。同期
//! [`Engine::execute`] は handle を terminal まで poll する互換 wrapper である。
//!
//! **後続 PR / Phase で扱う範囲**（本 PR では未実装、意図的に公開型へ含めない）:
//! scheduler・admission queue・backpressure（Slice 5）、cancellation / pause / resume の
//! 実効化（Slice 4。型は骨格のみ）、capability set・host injection（Phase 2）、`exit()` の
//! 構造化 [`ExecutionOutcome::Exited`]（REV-023）。これらを表す `CapabilitySet` /
//! `ExecutionId` / `LinkOptions` / admission slot などの型は、実装が伴うまで公開しない。

use std::marker::PhantomData;
use std::path::Path;

use crate::ast::Program;
use crate::budget::BudgetUsage;
use crate::error::TsumugiError;
use crate::eval::{Evaluator, RunPhase};
use crate::lexer::Lexer;
use crate::parser::Parser;

/// Tsumugi スクリプトをコンパイルして実行するエントリポイント。
#[derive(Debug, Default)]
pub struct Engine;

impl Engine {
    /// 新しい実行エンジンを作成する。
    pub fn new() -> Self {
        Self
    }

    /// ソースをパースし、再利用可能なスクリプトを作成する。
    ///
    /// import の解決は実行コンテキストに依存するため、ここでは行わない。
    pub fn compile(&self, source: &str) -> Result<CompiledScript, Vec<TsumugiError>> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse()?;

        Ok(CompiledScript {
            program,
            // root source の生 UTF-8 byte 長（REV-015 Slice 2、source accounting）。
            source_bytes: source.len() as u64,
        })
    }

    /// `CompiledScript` から実行 handle を作る（第9節、Created フェーズから開始）。
    ///
    /// import 解決（Link）を含めて同じ handle で進める入口である。PR-a では
    /// [`ExecutionHandle::poll`] が最初の呼び出しで Link と実行を terminal まで行う。
    pub fn create_execution<'e, 's, 'c>(
        &'e self,
        script: &'s CompiledScript,
        context: &'c mut ExecutionContext,
        request: ExecutionRequest,
    ) -> ExecutionHandle<'e, 's, 'c> {
        ExecutionHandle {
            script,
            context,
            request,
            state: ExecutionState::Created,
            outcome: None,
            transactional: false,
            started: false,
            _engine: PhantomData,
            _not_send: PhantomData,
        }
    }

    /// `CompiledScript` から Linked フェーズ開始相当の実行 handle を作る（第9節）。
    ///
    /// 仕様の `Engine::start(&LinkedScript, ...)` に対応する互換入口。PR-a では
    /// `LinkedScript` 型を導入せず `CompiledScript` を受け取り、poll 時に Link を行う。
    /// 公開状態は [`ExecutionState::Linked`] から始まる。
    pub fn start<'e, 's, 'c>(
        &'e self,
        script: &'s CompiledScript,
        context: &'c mut ExecutionContext,
        request: ExecutionRequest,
    ) -> ExecutionHandle<'e, 's, 'c> {
        ExecutionHandle {
            script,
            context,
            request,
            state: ExecutionState::Linked,
            outcome: None,
            transactional: false,
            started: false,
            _engine: PhantomData,
            _not_send: PhantomData,
        }
    }

    /// スクリプトを実行コンテキスト内で同期的に実行する。
    ///
    /// 第9節の handle を terminal まで poll する互換 wrapper である（REV-015 Slice 3 PR-a）。
    /// 戻り値契約は従来どおり: 正常完了なら [`ExecutionOutcome::Completed`]、失敗なら
    /// その [`TsumugiError`] を `Err` で返す。構造化 terminal（`RuntimeError` /
    /// `LinkError` の `usage` 付き）は [`ExecutionHandle::poll`] で観測できる。
    ///
    /// `ExecutionContext` はスレッド局所の評価状態を保持するため、十分なスタックを
    /// 持つ同一スレッド内で生成・利用する。CLI はツリーウォークの再帰評価のために
    /// 8 MiB の実行スレッドを使う。
    pub fn execute(
        &self,
        script: &CompiledScript,
        context: &mut ExecutionContext,
    ) -> Result<ExecutionOutcome, TsumugiError> {
        let request = ExecutionRequest::new();
        let mut handle = self.create_execution(script, context, request);
        Self::drive_to_outcome(&mut handle)
    }

    /// REPL の1入力をトランザクションとして実行する（AUD-024）。
    ///
    /// 未捕捉ランタイムエラーで終了した入力は、その入力が変更した全 language-state を
    /// 入力開始時点へ巻き戻す。正常完了と catch されて完了した入力は commit する。
    /// stdout やファイル書き込みなどの外部効果は巻き戻さない。
    ///
    /// PR-a では handle を terminal まで poll する互換 wrapper であり、内部で
    /// transaction 経路（`run_repl_submission`）を使う。
    pub fn execute_repl_submission(
        &self,
        script: &CompiledScript,
        context: &mut ExecutionContext,
    ) -> Result<ExecutionOutcome, TsumugiError> {
        let request = ExecutionRequest::new();
        let mut handle = self.create_execution(script, context, request);
        handle.transactional = true;
        Self::drive_to_outcome(&mut handle)
    }

    /// handle を terminal まで poll し、従来互換の `Result` へ写す。
    fn drive_to_outcome(
        handle: &mut ExecutionHandle<'_, '_, '_>,
    ) -> Result<ExecutionOutcome, TsumugiError> {
        loop {
            match handle.poll(PollSlice::default()) {
                // poll は非 terminal state からは Terminal か Yielded を返す。PR-a の
                // poll は 1 回で terminal へ到達するが、将来 slice fuel で Yielded が
                // 返っても terminal まで駆動できるよう loop で回す。
                Ok(PollResult::Terminal { outcome, .. }) => {
                    return match outcome {
                        ExecutionOutcome::Completed => Ok(ExecutionOutcome::Completed),
                        ExecutionOutcome::RuntimeError { error }
                        | ExecutionOutcome::LinkError { error } => Err(error),
                        // cancel / deadline は既定の同期経路（`execute`）では起きない
                        // （token 未 cancel・clock 未注入）が、防御的に従来互換の Err へ写す。
                        // 構造化 terminal は `poll` で `ExecutionOutcome::Cancelled` /
                        // `DeadlineExceeded` として観測する。
                        ExecutionOutcome::Cancelled => Err(TsumugiError::cancelled(0)),
                        ExecutionOutcome::DeadlineExceeded => {
                            Err(TsumugiError::deadline_exceeded(0))
                        }
                    };
                }
                Ok(PollResult::Yielded { .. }) | Ok(PollResult::Paused { .. }) => continue,
                // terminal 後の poll などの handle 誤用は internal error として surface する。
                Err(handle_error) => {
                    return Err(TsumugiError::internal(
                        0,
                        format!("実行 handle の誤用: {handle_error:?}"),
                    ));
                }
            }
        }
    }
}

/// パース済みで、実行可能な Tsumugi スクリプト。
pub struct CompiledScript {
    program: Program,
    /// root source の生 UTF-8 byte 長（REV-015 Slice 2 の source accounting）。
    source_bytes: u64,
}

/// 実行間で維持する Tsumugi の状態。
///
/// 同じコンテキストを再利用すると、変数、関数、import の解決状態を保持する。
///
/// # スレッドとスタック
///
/// 内部の評価状態は `Send` ではないため、コンテキストを別スレッドへ移動できない。
/// `Engine::execute` は caller のスレッドで再帰的に評価するため、埋め込み先は必要な
/// スタック容量を確保したスレッド内でコンテキストを生成・利用する。
pub struct ExecutionContext {
    evaluator: Evaluator,
}

impl ExecutionContext {
    /// 新しい実行コンテキストを作成する。
    pub fn new() -> Self {
        Self {
            evaluator: Evaluator::new(),
        }
    }

    /// スクリプトファイルの完全なパスを設定する。
    ///
    /// 相対 import の解決と、スクリプト自身の再 import 防止に使われる。
    pub fn set_script_path(&mut self, path: impl AsRef<Path>) {
        self.evaluator.set_base_dir(path.as_ref());
    }

    /// `args()` が返すスクリプト引数の snapshot を設定する（AUD-018）。
    ///
    /// process argv ではなく実行 context に属し、CLI や埋め込み host が実行単位で注入する。
    pub fn set_script_args(&mut self, args: Vec<String>) {
        self.evaluator.set_script_args(args);
    }

    /// 直前の実行が `exit(code)` で終了していれば、その終了コードを取り出す（C7、REV-023）。
    ///
    /// alpha facade は `execute` の `Err` に終了コードを載せられないため、`exit()` terminal の
    /// あとに本メソッドで code を読む。CLI がこの code で process を終了する。
    pub fn take_pending_exit(&mut self) -> Option<u8> {
        self.evaluator.take_pending_exit()
    }

    /// REPL の次の入力を実行する前にステップ予算をリセットする。
    pub fn reset_step_budget(&mut self) {
        self.evaluator.reset_step_budget();
    }

    /// 現在の予算使用量 snapshot を返す（REV-015 Slice 3 PR-a、第9節 `BudgetUsage`）。
    pub fn budget_usage(&self) -> BudgetUsage {
        self.evaluator.budget_usage()
    }
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}

/// 1 回の実行に対する不変の設定（第3節 `ExecutionRequest`）。
///
/// PR-a では budget は `ExecutionContext` 内の評価器が既に保持する `BudgetConfig`
/// （legacy 環境変数由来）を使うため、本型は最小の骨格に留める。仕様の
/// `execution_id` / `capabilities` / `arguments` / `budget` / `cancellation` は、
/// scheduler・capability（Slice 4/5・Phase 2）と一体で導入する。
#[derive(Debug, Clone, Default)]
pub struct ExecutionRequest {
    // PR-a では field を持たない。将来 budget/capability/cancellation を追加する際、
    // 既存呼び出しを壊さないよう `ExecutionRequest::new()` を入口として維持する。
    _private: (),
}

impl ExecutionRequest {
    /// 既定の実行リクエストを作る。
    pub fn new() -> Self {
        Self { _private: () }
    }
}

/// 1 回の `poll` で実行を許可する量（第9節 `PollSlice`）。
///
/// PR-a では slice fuel を消費しない（poll は 1 回で terminal へ到達する）。`max_fuel`
/// の骨格だけを公開し、slice fuel の実効化は PR-d で行う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollSlice {
    /// この slice で消費を許す論理 fuel 量（第4.2節。既定 10,000）。PR-a では未使用。
    pub max_fuel: u64,
}

impl Default for PollSlice {
    fn default() -> Self {
        Self { max_fuel: 10_000 }
    }
}

/// 実行 handle の公開状態（第9節 `ExecutionState`）。
///
/// PR-a で観測され得るのは `Created` / `Linked` / `Ready` / `Running` / `Terminal`。
/// `Yielded` / `Paused` は骨格として型に含めるが、PR-a の `poll` は返さない（slice fuel
/// の yield は PR-d、pause は Slice 4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionState {
    Created,
    Linked,
    Ready,
    Running,
    Yielded(YieldReason),
    Paused(PausedState),
    Terminal,
}

/// yield の理由（第9節 `YieldReason`）。
///
/// 仕様の 6 種のうち、`SliceFuelExhausted`（REV-015 Slice 3 で実効化）と `ExplicitYield`
/// だけを持つ。`AdmissionQueued` / `HostCallPending` / `AuditBackpressure` /
/// `SchedulerPreempted` は scheduler・host pending（Slice 5）で導入する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YieldReason {
    /// slice fuel を使い切った（第4.2節）。REV-015 Slice 3 PR-d で実効化。
    SliceFuelExhausted,
    /// script / host による明示 yield。
    ///
    /// pause/resume の状態機械は Slice 4 (D) で実効化済みだが、この `ExplicitYield` を
    /// 生成する trigger は未配線。script 側に `yield` 構文が無く、surface の追加は言語機能
    /// 拡張になるため据え置く（enum variant だけ用意する）。host からの協調停止は現状
    /// [`ExecutionHandle::pause`] で表現する。
    ExplicitYield,
}

/// pause の理由（第9節 `PauseReason`）。PR-a では返さない骨格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseReason {
    /// host の明示要求による pause（Slice 4 で実効化）。
    HostRequested,
}

/// pause 中の状態（第9節 `PausedState`）。PR-a では返さない骨格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PausedState {
    pub reason: PauseReason,
    pub resume_to: ResumeState,
}

/// pause から戻る先の状態（第9節 `ResumeState`）。PR-a では返さない骨格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeState {
    Created,
    Linked,
    Ready,
    Yielded(YieldReason),
}

/// `poll` の結果（第9節 `PollResult`）。
#[derive(Debug, Clone, PartialEq)]
pub enum PollResult {
    /// 非 terminal の協調停止。PR-a では返さない（PR-d 以降）。
    Yielded {
        reason: YieldReason,
        usage: BudgetUsage,
    },
    /// host 要求による pause。PR-a では返さない（Slice 4）。
    Paused {
        state: PausedState,
        usage: BudgetUsage,
    },
    /// terminal 到達。outcome と最終 usage を公開する。
    Terminal {
        outcome: ExecutionOutcome,
        usage: BudgetUsage,
    },
}

/// handle 操作の誤用エラー（第9節 `HandleError`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleError {
    /// 作成スレッド以外からの操作（PR-a では `!Send` により型で防ぐ骨格）。
    WrongThread,
    /// その状態では不正な操作。
    InvalidState {
        operation: &'static str,
        state: ExecutionState,
    },
    /// terminal 後の操作。
    Terminal,
}

/// スクリプト実行の terminal payload（第9節 `ExecutionOutcome`）。
///
/// 到達し得るのは、正常完了 `Completed`、実行中失敗 `RuntimeError`、Link 中失敗
/// `LinkError`、協調停止 `Cancelled`、deadline 超過 `DeadlineExceeded` の 5 種。`Cancelled` /
/// `DeadlineExceeded` は catch 不能 terminal 信号（REV-015 Slice 4）で、tree evaluator が
/// [`crate::error::ErrorKind::Cancelled`] / [`crate::error::ErrorKind::DeadlineExceeded`] を
/// surface したときに [`ExecutionHandle::poll`] がこの専用 variant へ写す（`RuntimeError` に
/// 埋もれさせない）。仕様の他の terminal（`Exited` は REV-023、`Denied` / `HostError` /
/// `BudgetExceeded` などは Slice 5・Phase 2）は対応機構が入る後続 PR で追加する。予算超過
/// （limit 系）は現状 `RuntimeError` に含まれる。
///
/// 最終 `BudgetUsage` は [`PollResult::Terminal`] の `usage` field で観測する。terminal
/// 理由（outcome）と usage を二重に持たせないため、本 enum の error 変種は `error` だけを
/// 保持する（§1.1「state と outcome で terminal 理由を二重定義しない」に倣う）。
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionOutcome {
    /// スクリプトが最後まで実行された。
    Completed,
    /// スクリプト実行中に未捕捉エラーで終了した（予算超過を含む）。
    RuntimeError { error: TsumugiError },
    /// 最初の文を実行する前の Link フェーズ（import 解決・source/heap 課金）で失敗した。
    LinkError { error: TsumugiError },
    /// host が [`ExecutionHandle::cancellation_token`] で要求した協調的 cancel で停止した
    /// （REV-015 Slice 4、§8）。language-state は実行開始時点へ rollback 済み（§10 規則3）。
    Cancelled,
    /// 実行 deadline を超過して停止した（REV-015 Slice 4、§7 / §8）。
    /// language-state は実行開始時点へ rollback 済み（§10 規則3）。
    DeadlineExceeded,
}

/// 実行を進める handle（第9節 `ExecutionHandle`）。
///
/// 公開 handle は `!Send + !Sync` で、作成したスレッド上からだけ操作する（第9.1節）。
/// PR-a の `poll` は初回に評価器を terminal まで回し、以降の非 poll 操作
/// （`pause` / `resume`）は骨格として `HandleError` を返す。
pub struct ExecutionHandle<'engine, 'script, 'context> {
    script: &'script CompiledScript,
    context: &'context mut ExecutionContext,
    #[allow(dead_code)]
    request: ExecutionRequest,
    state: ExecutionState,
    outcome: Option<ExecutionOutcome>,
    /// REPL 用に transaction 経路（`run_repl_submission`）で実行するか。
    transactional: bool,
    /// 実行セッションを開始済みか（REV-015 Slice 3 PR-d-2）。初回 poll で `begin_execution`
    /// を呼んで true にし、以降の poll は `run_slice` で resume する。
    started: bool,
    _engine: PhantomData<&'engine Engine>,
    /// `!Send + !Sync` を保証する（第9.1節）。別スレッドへ move させない。
    _not_send: PhantomData<*const ()>,
}

impl ExecutionHandle<'_, '_, '_> {
    /// 現在の公開状態を返す。
    pub fn state(&self) -> ExecutionState {
        self.state.clone()
    }

    /// 現在の予算使用量 snapshot を返す。
    pub fn usage(&self) -> BudgetUsage {
        self.context.budget_usage()
    }

    /// terminal に到達済みなら outcome を返す。
    pub fn outcome(&self) -> Option<&ExecutionOutcome> {
        self.outcome.as_ref()
    }

    /// この実行の協調的 cancel token の clone を返す（第9.1節、REV-015 Slice 4）。
    ///
    /// 返る [`CancellationToken`](crate::budget::CancellationToken) は `Arc<AtomicBool>` の
    /// clone で `Send + Sync`。host はこれを別スレッドで保持し、`cancel()` を呼ぶことで実行中
    /// の handle を協調的に停止できる（§8）。cancel は各 fuel charge 前と文/反復境界の
    /// checkpoint で観測され、次の [`Self::poll`] が [`ExecutionState::Terminal`] へ遷移して
    /// [`ExecutionOutcome::Cancelled`] を返す。既に terminal へ到達した後の cancel は outcome を
    /// 変えない（§8「commit 後の cancel は結果を変えない」）。別スレッドへ公開するのはこの
    /// token と waker だけで、continuation や context への参照は渡さない（§9.1）。
    pub fn cancellation_token(&self) -> crate::budget::CancellationToken {
        self.context.evaluator.cancellation_token()
    }

    /// 実行を 1 slice 進める（第9節 `poll`）。
    ///
    /// `Created` / `Linked` / `Ready` / `Yielded` から呼ぶと `Running` を経て、初回は link を
    /// 済ませてから最大 `slice.max_fuel` の fuel ぶん実行を進める（REV-015 Slice 3 PR-d-2、
    /// §4.2）。slice fuel を使い切ると [`PollResult::Yielded`]（`SliceFuelExhausted`）を返し、
    /// 評価器の永続 continuation に状態が残る。次の poll で続きから resume する。terminal に
    /// 達すると [`PollResult::Terminal`] を返す。terminal 後の poll は [`HandleError::Terminal`]。
    pub fn poll(&mut self, slice: PollSlice) -> Result<PollResult, HandleError> {
        match self.state {
            ExecutionState::Terminal => Err(HandleError::Terminal),
            ExecutionState::Paused(_) => Err(HandleError::InvalidState {
                operation: "poll",
                state: self.state.clone(),
            }),
            // Created / Linked / Ready / Yielded はいずれも Running を経て 1 slice 進む。
            _ => {
                self.state = ExecutionState::Running;
                let poll = self.drive_one_slice(slice.max_fuel);
                let usage = self.context.budget_usage();
                match poll {
                    // slice fuel を使い切って協調停止した（§4.2）。continuation は
                    // 評価器の永続 frame stack に残り、次 poll で resume する。
                    SlicePoll::Yielded => {
                        self.state = ExecutionState::Yielded(YieldReason::SliceFuelExhausted);
                        Ok(PollResult::Yielded {
                            reason: YieldReason::SliceFuelExhausted,
                            usage,
                        })
                    }
                    SlicePoll::Terminal(outcome) => {
                        // terminal は 1 回だけセットする（§8 / §15.3）。handle は !Send + !Sync で
                        // 単一スレッド運用のため、cancel（別スレッドが token を立てるだけ）と正常
                        // 完了の決着は必ずこの poll 内で単一スレッド的に行われる。ここで state を
                        // Terminal にすると、以後の poll / pause / resume は先頭の
                        // `ExecutionState::Terminal` 分岐で `HandleError::Terminal` になり、後から
                        // 到達した cancel は outcome を変えない（cancel の linearization point は
                        // token の compare-and-set＝`CancellationToken::cancel` の false→true だが、
                        // それが観測されるのは次 checkpoint であり、既に terminal 済みなら無視される）。
                        self.state = ExecutionState::Terminal;
                        self.outcome = Some(outcome.clone());
                        Ok(PollResult::Terminal { outcome, usage })
                    }
                }
            }
        }
    }

    /// host 要求による pause（第9節、REV-015 Slice 4）。
    ///
    /// `Created` / `Linked` / `Ready` / `Yielded` でだけ成功し、直前の状態を
    /// [`PausedState::resume_to`] に保存して `Paused` へ遷移する（§9.2）。`Running` は
    /// poll 中の mutable borrow 内なので到達せず、`Paused` は二重 pause、`Terminal` は
    /// `HandleError::Terminal` になる。pause 中は評価器の永続 continuation（session /
    /// frame stack / transaction journal）を yield と同じくそのまま保持し、resume まで
    /// run-turn へ戻らない（§9.3 / §11）。deadline は pause 中も進む（clock を止めない）。
    pub fn pause(&mut self) -> Result<(), HandleError> {
        if self.state == ExecutionState::Terminal {
            return Err(HandleError::Terminal);
        }
        match Self::resume_target(&self.state) {
            Some(resume_to) => {
                self.state = ExecutionState::Paused(PausedState {
                    reason: PauseReason::HostRequested,
                    resume_to,
                });
                Ok(())
            }
            None => Err(HandleError::InvalidState {
                operation: "pause",
                state: self.state.clone(),
            }),
        }
    }

    /// pause からの resume（第9節、REV-015 Slice 4）。
    ///
    /// `Paused` でだけ成功し、pause 前に保存した [`PausedState::resume_to`] の状態
    /// （`Created` / `Linked` / `Ready` / `Yielded`）へ戻す（§9.2）。その状態から
    /// 次の `poll` が continuation を resume する。`Terminal` は `HandleError::Terminal`、
    /// それ以外の非 Paused は `InvalidState`。
    pub fn resume(&mut self) -> Result<(), HandleError> {
        match &self.state {
            ExecutionState::Terminal => Err(HandleError::Terminal),
            ExecutionState::Paused(paused) => {
                self.state = match &paused.resume_to {
                    ResumeState::Created => ExecutionState::Created,
                    ResumeState::Linked => ExecutionState::Linked,
                    ResumeState::Ready => ExecutionState::Ready,
                    ResumeState::Yielded(reason) => ExecutionState::Yielded(reason.clone()),
                };
                Ok(())
            }
            _ => Err(HandleError::InvalidState {
                operation: "resume",
                state: self.state.clone(),
            }),
        }
    }

    /// pause 可能な状態なら、pause 中に保存する [`ResumeState`] を返す（§9.2）。
    ///
    /// `Created` / `Linked` / `Ready` / `Yielded` だけが pause でき、その他（`Running` /
    /// `Paused` / `Terminal`）は `None`（pause 不可）。
    fn resume_target(state: &ExecutionState) -> Option<ResumeState> {
        match state {
            ExecutionState::Created => Some(ResumeState::Created),
            ExecutionState::Linked => Some(ResumeState::Linked),
            ExecutionState::Ready => Some(ResumeState::Ready),
            ExecutionState::Yielded(reason) => Some(ResumeState::Yielded(reason.clone())),
            ExecutionState::Running | ExecutionState::Paused(_) | ExecutionState::Terminal => None,
        }
    }

    /// 実行を 1 slice 進める（REV-015 Slice 3 PR-d-2 の poll コア）。
    ///
    /// 初回 poll では `begin_execution` で link と root frame push を済ませ、以降の poll は
    /// 評価器の永続 continuation を `run_slice` で resume する。slice fuel を使い切ると
    /// [`SlicePoll::Yielded`] を返し、terminal に達すると [`SlicePoll::Terminal`] を返す。
    fn drive_one_slice(&mut self, slice_fuel: u64) -> SlicePoll {
        let source_bytes = self.script.source_bytes;
        let transactional = self.transactional;

        if !self.started {
            // 初回 poll: link + Link フェーズ課金 + root frame push。まだ 1 文も実行しない。
            let program = &self.script.program;
            match self
                .context
                .evaluator
                .begin_execution(program, source_bytes, transactional)
            {
                Ok(()) => {
                    self.started = true;
                }
                Err((phase, error)) => {
                    // cancel / deadline は catch 不能 terminal 信号。Link フェーズの課金
                    // checkpoint（charge_link）で観測されても専用 terminal へ写す（§8）。
                    let outcome = match error.kind() {
                        Some(crate::error::ErrorKind::Cancelled) => ExecutionOutcome::Cancelled,
                        Some(crate::error::ErrorKind::DeadlineExceeded) => {
                            ExecutionOutcome::DeadlineExceeded
                        }
                        // それ以外の Link 失敗は terminal。transaction 経路は Link/Run を
                        // 区別せず RuntimeError。
                        _ if transactional => ExecutionOutcome::RuntimeError { error },
                        _ => match phase {
                            RunPhase::Link => ExecutionOutcome::LinkError { error },
                            RunPhase::Run => ExecutionOutcome::RuntimeError { error },
                        },
                    };
                    return SlicePoll::Terminal(outcome);
                }
            }
        }

        // 1 slice ぶん実行する。None = yield、Some(result) = terminal。
        match self.context.evaluator.run_slice(slice_fuel) {
            None => SlicePoll::Yielded,
            Some(Ok(())) => SlicePoll::Terminal(ExecutionOutcome::Completed),
            // 実行フェーズの失敗（transaction は commit/rollback を run_slice が済ませている）。
            // cancel / deadline は catch 不能 terminal 信号なので専用 outcome へ写し、
            // RuntimeError に埋もれさせない（REV-015 Slice 4、§8）。予算超過（limit 系）を
            // 含む他の未捕捉エラーは RuntimeError。
            Some(Err(error)) => {
                let outcome = match error.kind() {
                    Some(crate::error::ErrorKind::Cancelled) => ExecutionOutcome::Cancelled,
                    Some(crate::error::ErrorKind::DeadlineExceeded) => {
                        ExecutionOutcome::DeadlineExceeded
                    }
                    _ => ExecutionOutcome::RuntimeError { error },
                };
                SlicePoll::Terminal(outcome)
            }
        }
    }
}

/// [`ExecutionHandle::drive_one_slice`] の結果（REV-015 Slice 3 PR-d-2）。
enum SlicePoll {
    /// slice fuel を使い切って協調停止した。continuation は評価器に残る。
    Yielded,
    /// terminal に達した。
    Terminal(ExecutionOutcome),
}
