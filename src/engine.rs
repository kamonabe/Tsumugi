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
use std::sync::Arc;

use crate::ast::Program;
use crate::budget::BudgetUsage;
use crate::error::TsumugiError;
use crate::eval::{Evaluator, RunPhase, SliceOutcome};
use crate::host_pending::{ExecutionWaker, TicketErased};
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::scheduler::{AdmissionSlot, ExecutionSlotId, SchedulerShared, SlotState};

pub use crate::scheduler::{AdmissionPhase, EngineLimits, StartError};

/// Tsumugi スクリプトをコンパイルして実行するエントリポイント。
///
/// # 協調スケジューラ（REV-015 Slice 5）
///
/// `Engine` は Engine 全体で共有する協調制御層 [`SchedulerShared`] を `Arc` で保持する
/// （有限 active/queue slot の admission と run-turn FIFO、設計 §4.1）。`create_execution` /
/// `start` は handle 生成前に 1 個の slot を予約し、満杯なら [`StartError::Backpressure`] を
/// 返す（handle を作らず context を変更しない）。
#[derive(Clone)]
pub struct Engine {
    shared: Arc<SchedulerShared>,
}

impl Engine {
    /// 既定の [`EngineLimits`] で新しい実行エンジンを作成する。
    pub fn new() -> Self {
        Self {
            shared: Arc::new(SchedulerShared::new()),
        }
    }

    /// 指定した [`EngineLimits`] で実行エンジンを作成する（REV-015 Slice 5、設計 §4.1）。
    ///
    /// active/queue 上限と既定 slice fuel をテストや埋め込み host が指定する opt-in 入口。
    pub fn with_limits(limits: EngineLimits) -> Self {
        Self {
            shared: Arc::new(SchedulerShared::with_limits(limits)),
        }
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
    /// import 解決（Link）を含めて同じ handle で進める入口である。[`ExecutionHandle::poll`] が
    /// 最初の呼び出しで Link と実行を進める。
    ///
    /// # 協調スケジューラ（REV-015 Slice 5、設計 §4.1）
    ///
    /// handle 生成前に [`SchedulerShared::admit`](crate::scheduler::SchedulerShared::admit) で
    /// slot を 1 個予約する（phase は [`AdmissionPhase::Created`]）。active 上限に空きがあれば
    /// active slot を得て `Created` から、満杯なら queue slot を得て
    /// `Yielded(AdmissionQueued { resume_to: Created })` から始まる。active/queue 共に満杯なら
    /// [`StartError::Backpressure`] を返し、**handle を作らず context の `&mut` 借用も発生しない**。
    ///
    /// 仕様 §9.1 の `link_options: LinkOptions` 引数は capability/import 制御（Phase 2）に属し
    /// 現行コードに無いため本 Slice では据え置き、引数は追加しない。`&CompiledScript` を保持し
    /// 初回 poll の `begin_execution` で Link する（`Result` 化のみ行う。設計 §4.1）。
    pub fn create_execution<'e, 's, 'c>(
        &'e self,
        script: &'s CompiledScript,
        context: &'c mut ExecutionContext,
        request: ExecutionRequest,
    ) -> Result<ExecutionHandle<'e, 's, 'c>, StartError> {
        // admit を context 借用より前に呼ぶ（backpressure => handle 無し・context 不変、AC-1）。
        let slot = self.shared.admit(AdmissionPhase::Created)?;
        Ok(Self::new_handle(
            Arc::clone(&self.shared),
            slot,
            AdmissionPhase::Created,
            script,
            context,
            request,
        ))
    }

    /// `CompiledScript` から Linked フェーズ開始相当の実行 handle を作る（第9節）。
    ///
    /// 仕様の `Engine::start(&LinkedScript, ...)` に対応する互換入口。本 Slice では
    /// `LinkedScript` 型を導入せず `CompiledScript` を受け取り、poll 時に Link を行う
    /// （`Result` 化のみ、設計 §4.1）。active なら公開状態は [`ExecutionState::Linked`] から、
    /// queue なら `Yielded(AdmissionQueued { resume_to: Linked })` から始まる。
    pub fn start<'e, 's, 'c>(
        &'e self,
        script: &'s CompiledScript,
        context: &'c mut ExecutionContext,
        request: ExecutionRequest,
    ) -> Result<ExecutionHandle<'e, 's, 'c>, StartError> {
        let slot = self.shared.admit(AdmissionPhase::Linked)?;
        Ok(Self::new_handle(
            Arc::clone(&self.shared),
            slot,
            AdmissionPhase::Linked,
            script,
            context,
            request,
        ))
    }

    /// admit 済み slot から handle を組み立てる（設計 §4.1）。queue slot なら初期状態は
    /// `Yielded(AdmissionQueued { resume_to })`、active slot なら `resume_to` 相当の
    /// `Created` / `Linked`。
    fn new_handle<'e, 's, 'c>(
        shared: Arc<SchedulerShared>,
        slot: AdmissionSlot,
        resume_to: AdmissionPhase,
        script: &'s CompiledScript,
        context: &'c mut ExecutionContext,
        request: ExecutionRequest,
    ) -> ExecutionHandle<'e, 's, 'c> {
        let slot_id = slot.slot_id();
        let slot_state = slot.slot_state_arc();
        let state = if slot.is_active() {
            match resume_to {
                AdmissionPhase::Created => ExecutionState::Created,
                AdmissionPhase::Linked => ExecutionState::Linked,
            }
        } else {
            ExecutionState::Yielded(YieldReason::AdmissionQueued { resume_to })
        };
        ExecutionHandle {
            script,
            context,
            request,
            state,
            outcome: None,
            transactional: false,
            started: false,
            shared,
            slot_id,
            slot_state,
            waker: None,
            pending_ticket: None,
            run_turn_registered: false,
            _engine: PhantomData,
            _not_send: PhantomData,
            slot,
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
        // 単一 execution・既定 limits では admit が必ず成功する（active=0<max）。防御的に
        // Backpressure を従来互換の internal error へ写し、Ok(Completed)/Err 契約を保つ（設計 §4.1）。
        let mut handle = self
            .create_execution(script, context, request)
            .map_err(Self::start_error_to_tsumugi_error)?;
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
        let mut handle = self
            .create_execution(script, context, request)
            .map_err(Self::start_error_to_tsumugi_error)?;
        handle.transactional = true;
        Self::drive_to_outcome(&mut handle)
    }

    /// 同期 wrapper 用に [`StartError`] を従来互換の [`TsumugiError`] へ写す（設計 §4.1）。
    ///
    /// 同期経路（単一 execution・既定 limits）は構造的に backpressure に到達しないが、防御的に
    /// internal error へ写して `Ok(Completed)` / `Err` の戻り値契約を保つ。
    fn start_error_to_tsumugi_error(error: StartError) -> TsumugiError {
        TsumugiError::internal(0, format!("実行の受理に失敗しました: {error:?}"))
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

impl Default for Engine {
    fn default() -> Self {
        Self::new()
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

    /// deadline clock を注入した実行コンテキストを作成する（REV-015 Slice 5、設計 §4.6）。
    ///
    /// alpha facade で admission queue 待ち・host 待ち中の deadline を機能させる唯一の clock
    /// 注入経路。`checkpoint_deadline` は注入 clock を基準に判定し、clock 未注入（既存
    /// [`Self::new`]）なら no-op のまま（NFR-1、観測挙動不変）。clock の domain は台帳の
    /// `BudgetConfig.deadline` と一致する必要があり、別 domain の clock は
    /// [`crate::budget::ConfigError::ForeignClock`] を [`StartError::Config`] として返す。
    pub fn new_with_clock(
        clock: std::sync::Arc<dyn crate::budget::MonotonicClock>,
    ) -> Result<Self, StartError> {
        let mut evaluator = Evaluator::new();
        evaluator
            .validate_budget_against(clock.as_ref())
            .map_err(StartError::Config)?;
        evaluator.set_deadline_clock(clock);
        Ok(Self { evaluator })
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
/// `SliceFuelExhausted`（REV-015 Slice 3 で実効化）と `ExplicitYield` に加え、Slice 5 で
/// `AdmissionQueued`（admission queue 待ち）・`HostCallPending`（cooperative host-call 待ち）・
/// `SchedulerPreempted`（run-turn で非 head）を配線する。
///
/// `AuditBackpressure`（仕様 §9）は本 enum に **加えない**: audit event emission は Phase 6 で
/// あり、backpressure を audit sink の詰まりへ変換する機構は audit sink が入って初めて意味を
/// 持つ。variant を今足すと「返り得ない公開 variant」になり、「state と outcome で terminal
/// 理由を二重定義しない」精神に反するため、依存機構が入る Phase 6 で追加する（設計 §4.6）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YieldReason {
    /// admission queue で active slot の空きを待っている（Slice 5、設計 §4.1/§4.6）。
    ///
    /// この yield の間は link を含む semantic work を一切行わない。active slot が空いて FIFO
    /// 先頭へ昇格すると `resume_to` の [`AdmissionPhase`]（`Created` / `Linked`）へ戻る。
    AdmissionQueued { resume_to: AdmissionPhase },
    /// slice fuel を使い切った（第4.2節）。REV-015 Slice 3 PR-d で実効化。
    SliceFuelExhausted,
    /// cooperative host-call が `Pending` を返し結果を待っている（Slice 5、設計 §4.4/§4.7）。
    ///
    /// この yield の間は run-turn FIFO から外れ、ticket へ 1 個の
    /// [`ExecutionWaker`](crate::host_pending::ExecutionWaker) を登録して待つ。adapter executor が
    /// 結果を ticket へ格納して wake すると、次 poll が `try_take_value` で取り出し resume する
    /// （continuation は作成 thread の poll でだけ進む、INV-7）。`call_id` は host-call の識別子。
    HostCallPending { call_id: u64 },
    /// script / host による明示 yield。
    ///
    /// pause/resume の状態機械は Slice 4 (D) で実効化済みだが、この `ExplicitYield` を
    /// 生成する trigger は未配線。script 側に `yield` 構文が無く、surface の追加は言語機能
    /// 拡張になるため据え置く（enum variant だけ用意する）。host からの協調停止は現状
    /// [`ExecutionHandle::pause`] で表現する。
    ExplicitYield,
    /// run-turn FIFO で先頭でないため semantic work をせず譲った（Slice 5、設計 §4.3）。
    ///
    /// これは [`PollResult`] の **reason としてのみ** 返り、`self.state` は遷移させない
    /// （Finding 4）。非 head poll は元の `Ready` / `Yielded(..)` を保ったまま、戻り値だけ
    /// `SchedulerPreempted` になる。run-turn 内の自分の位置も変えない（FR-3）。
    SchedulerPreempted,
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
    /// Engine 全体で共有する協調制御層（Slice 5、設計 §4.1）。run-turn 登録・head 判定・
    /// AdmissionQueued 昇格の観測に使う。`Arc<SchedulerShared>` は `Send + Sync` だが、handle
    /// 全体は `_not_send` により `!Send + !Sync` のまま。
    shared: Arc<SchedulerShared>,
    /// この execution の slot id（run-turn / admission の識別子）。
    slot_id: ExecutionSlotId,
    /// 昇格通知を poll 時に lock なしで読む read-only ミラー（設計 §4.1/§4.3）。
    slot_state: Arc<SlotState>,
    /// host/ticket 完了・cancel・deadline が state を ready にしたときに鳴らす waker
    /// （REV-015 Slice 5、設計 §4.5）。[`Self::set_waker`] で設定し、host-call pending 進入時に
    /// ticket へ、取得時に `CancellationToken` へ登録する。別 thread から invoke してよい唯一の
    /// handle 由来 handle（`Send + Sync`）。
    waker: Option<ExecutionWaker>,
    /// host-call pending 中のみ `Some` となる型消去 ticket（設計 §4.4/§4.8）。
    ///
    /// `run_slice` が `YieldedHostCall` を返した直後に `evaluator.take_pending_ticket()` から
    /// 移送する（§4.3 step 6）。以後の poll で `try_take_value()` を駆動し、cancel/deadline/drop
    /// 時は `cancel()` で遅着結果を破棄する（§4.6/§4.8）。
    pending_ticket: Option<TicketErased>,
    /// run-turn FIFO へ push 済みか（設計 §4.3 step 5）。
    run_turn_registered: bool,
    _engine: PhantomData<&'engine Engine>,
    /// `!Send + !Sync` を保証する（第9.1節）。別スレッドへ move させない。
    _not_send: PhantomData<*const ()>,
    /// handle が所有する admission slot の RAII トークン（設計 §4.1/§4.8）。
    ///
    /// **末尾 field** に置く: Rust は field を宣言順に drop するため、将来の `impl Drop` 本体で
    /// cancel linearize / ticket detach / rollback（§4.8 step 1-4）を行った後に、この field の
    /// drop で slot が解放される（§4.8 step 5）ようにする。本 FEAT では明示 `Drop` 本体は持たず、
    /// この field の [`AdmissionSlot`] 自身の `Drop` が slot を 1 個だけ解放する（§4.1 INV-2）。
    /// 読み取りはしないが RAII 解放のため保持し続ける。[`Drop`] 本体（§4.8 step 1-4）の後に、
    /// この field drop（step 5）で slot が解放される。
    #[allow(dead_code)]
    slot: AdmissionSlot,
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
        // (1) terminal guard / (2) paused guard（設計 §4.3）。
        match &self.state {
            ExecutionState::Terminal => return Err(HandleError::Terminal),
            ExecutionState::Paused(_) => {
                return Err(HandleError::InvalidState {
                    operation: "poll",
                    state: self.state.clone(),
                });
            }
            _ => {}
        }

        // (3) AdmissionQueued: cancel/deadline checkpoint → 昇格確認（設計 §4.3 step 3）。
        if let ExecutionState::Yielded(YieldReason::AdmissionQueued { resume_to }) = self.state {
            // queue 待ち中の cancel/deadline を発火する（§4.6）。AdmissionQueued は
            // begin_execution 前・session 無しのため rollback 不要（§4.6）。
            if let Some(outcome) = self.checkpoint_terminal() {
                return Ok(self.finish_terminal(outcome));
            }
            // 昇格済みか（別 thread の release_active が SlotState を Active へ store したか）。
            if !self.slot_state.is_active() {
                // 未昇格: semantic work をせず AdmissionQueued を返す（FR-2）。
                let usage = self.context.budget_usage();
                return Ok(PollResult::Yielded {
                    reason: YieldReason::AdmissionQueued { resume_to },
                    usage,
                });
            }
            // 昇格済み: resume_to（Created/Linked）へ遷移して step 5 以降へ落とす。昇格直後は
            // まだ run-turn 未登録なので、step 5 の登録ロジックが push_back して head を待つ。
            self.state = match resume_to {
                AdmissionPhase::Created => ExecutionState::Created,
                AdmissionPhase::Linked => ExecutionState::Linked,
            };
        }

        // (4) HostCallPending: cancel/deadline checkpoint を try_take より先に発火（設計 §4.3
        // step 4 / §4.6 linearization）。
        if let ExecutionState::Yielded(YieldReason::HostCallPending { call_id }) = self.state {
            // checkpoint（cancel→deadline）を try_take より先に評価する（host response 対 cancel
            // の linearization、§4.6 Finding 8）。cancel/deadline なら ticket を cancel して遅着
            // response を破棄し、session を rollback（HostCallPending は begin_execution 済みで
            // session 有り、§4.6）してから terminal にする。
            if let Some(outcome) = self.checkpoint_terminal() {
                if let Some(ticket) = &self.pending_ticket {
                    ticket.cancel();
                }
                self.pending_ticket = None;
                self.context.evaluator.abort_session();
                self.shared.run_turn_remove(self.slot_id);
                self.run_turn_registered = false;
                return Ok(self.finish_terminal(outcome));
            }
            // cancel/deadline でなければ ticket の完了を確認する。
            let taken = self
                .pending_ticket
                .as_ref()
                .and_then(|ticket| ticket.try_take_value());
            match taken {
                // 未完了: semantic work せず HostCallPending を返す（work しない、§4.3 step 4）。
                None => {
                    let usage = self.context.budget_usage();
                    return Ok(PollResult::Yielded {
                        reason: YieldReason::HostCallPending { call_id },
                        usage,
                    });
                }
                // 完了: 結果を Evaluator の pending host-call 位置へ注入し resume 可能にする。
                Some(result) => {
                    if let Err(error) = self.context.evaluator.resume_host_call(call_id, result) {
                        // call_id 不整合などの内部不整合は catch 不能 terminal（§4.7）。
                        self.pending_ticket = None;
                        self.context.evaluator.abort_session();
                        self.shared.run_turn_remove(self.slot_id);
                        self.run_turn_registered = false;
                        return Ok(self.finish_terminal(ExecutionOutcome::RuntimeError { error }));
                    }
                    self.pending_ticket = None;
                    // 下の run-turn 判定（step 5）へ落とす。resume 後は run-turn へ再登録する。
                }
            }
        }

        // (5) run-turn 登録と head 判定（設計 §4.3 step 5）。
        if !self.run_turn_registered {
            self.shared.run_turn_push_back(self.slot_id);
            self.run_turn_registered = true;
        }
        if !self.shared.is_head(self.slot_id) {
            // 非 head: semantic work せず SchedulerPreempted を reason としてだけ返す。
            // self.state は遷移させない（Finding 4）。run-turn 内の位置も変えない（FR-3）。
            let usage = self.context.budget_usage();
            return Ok(PollResult::Yielded {
                reason: YieldReason::SchedulerPreempted,
                usage,
            });
        }

        // (6) head 路: 1 slice 進める（設計 §4.3 step 6）。
        self.state = ExecutionState::Running;
        let poll = self.drive_one_slice(slice.max_fuel);
        let usage = self.context.budget_usage();
        match poll {
            // slice fuel を使い切って協調停止した（§4.2）。continuation は評価器の永続 frame
            // stack に残り、run-turn 先頭を末尾へ回して（§12.1 規則 2/3）次 poll で resume する。
            SlicePoll::YieldedSliceFuel => {
                self.shared.run_turn_rotate(self.slot_id);
                self.state = ExecutionState::Yielded(YieldReason::SliceFuelExhausted);
                Ok(PollResult::Yielded {
                    reason: YieldReason::SliceFuelExhausted,
                    usage,
                })
            }
            // cooperative host-call が Pending を返した（設計 §4.3 step 6 host-pending 分岐）。
            // ticket を評価器から移送して handle が保持し、handle の waker を ticket へ登録する。
            // run-turn から外して run_turn_registered=false に戻す（再 Ready は waker 後の次 poll
            // で step 5 が末尾へ再登録する）。
            SlicePoll::YieldedHostCall { call_id } => {
                // 評価器が保持する TicketErased を handle 側へ移す（Finding 3、§4.7）。
                if let Some(ticket) = self.context.evaluator.take_pending_ticket() {
                    // waker が設定済みなら ticket へ登録する（host 完了時に handle を wake、§4.5）。
                    if let Some(waker) = &self.waker {
                        ticket.register_waker(waker);
                    }
                    self.pending_ticket = Some(ticket);
                }
                self.shared.run_turn_remove(self.slot_id);
                self.run_turn_registered = false;
                self.state = ExecutionState::Yielded(YieldReason::HostCallPending { call_id });
                Ok(PollResult::Yielded {
                    reason: YieldReason::HostCallPending { call_id },
                    usage,
                })
            }
            // 明示 yield（本 Slice では未 trigger、§4.7）。通常 yield と同じ扱いで run-turn rotate。
            // SchedulerPreempted にはしない（preempt は非 head poll の reason、Finding 4）。
            SlicePoll::YieldedExplicit => {
                self.shared.run_turn_rotate(self.slot_id);
                self.state = ExecutionState::Yielded(YieldReason::ExplicitYield);
                Ok(PollResult::Yielded {
                    reason: YieldReason::ExplicitYield,
                    usage,
                })
            }
            SlicePoll::Terminal(outcome) => {
                // terminal は 1 回だけセットする（§8 / §15.3）。handle は !Send + !Sync で
                // 単一スレッド運用のため、cancel（別スレッドが token を立てるだけ）と正常
                // 完了の決着は必ずこの poll 内で単一スレッド的に行われる。ここで state を
                // Terminal にすると、以後の poll / pause / resume は先頭の
                // `ExecutionState::Terminal` 分岐で `HandleError::Terminal` になり、後から
                // 到達した cancel は outcome を変えない。
                self.shared.run_turn_remove(self.slot_id);
                Ok(self.finish_terminal(outcome))
            }
        }
    }

    /// handle の wake handle を設定する（REV-015 Slice 5、設計 §4.5）。
    ///
    /// `Some(w)` で以前の waker を置換し、`None` で解除する。host-call ticket 完了・cancel・
    /// deadline・admission 昇格が state を ready にしたときに、この waker を鳴らして host に
    /// 次 poll を促す。設定した waker は [`crate::budget::CancellationToken`] にも登録し、別 thread
    /// の `cancel()` が待機中の handle を wake できるようにする（§8/§4.5）。別 thread から許可する
    /// handle 由来の操作は cancel と waker invocation だけ（§9.1）。terminal state では
    /// [`HandleError::Terminal`]、それ以外（非 pending を含む）は no-op 成功。
    pub fn set_waker(&mut self, waker: Option<ExecutionWaker>) -> Result<(), HandleError> {
        if self.state == ExecutionState::Terminal {
            return Err(HandleError::Terminal);
        }
        // cancel waker 連携: 設定した waker を cancel token へ登録する（解除時は登録しない）。
        if let Some(waker) = &waker {
            self.cancellation_token().register_waker(waker);
            // host-call pending 中に差し替えられた場合は、現在の ticket へも登録し直す（§4.5）。
            if let Some(ticket) = &self.pending_ticket {
                ticket.register_waker(waker);
            }
        }
        self.waker = waker;
        Ok(())
    }

    /// AdmissionQueued 中の cancel/deadline checkpoint（設計 §4.3 step 3 / §4.6）。
    ///
    /// cancel が立っていれば [`ExecutionOutcome::Cancelled`]、deadline 到達なら
    /// [`ExecutionOutcome::DeadlineExceeded`] を返す。どちらでもなければ `None`。clock 未注入の
    /// context では `checkpoint_deadline` は no-op（設計 §4.6、`ExecutionContext::new_with_clock`
    /// で clock を注入した場合のみ deadline 側が機能する）。
    fn checkpoint_terminal(&self) -> Option<ExecutionOutcome> {
        if self.context.evaluator.checkpoint_cancel().is_err() {
            return Some(ExecutionOutcome::Cancelled);
        }
        if self.context.evaluator.checkpoint_deadline().is_err() {
            return Some(ExecutionOutcome::DeadlineExceeded);
        }
        None
    }

    /// terminal state をセットして [`PollResult::Terminal`] を組み立てる（§8 / §15.3）。
    fn finish_terminal(&mut self, outcome: ExecutionOutcome) -> PollResult {
        let usage = self.context.budget_usage();
        self.state = ExecutionState::Terminal;
        self.outcome = Some(outcome.clone());
        PollResult::Terminal { outcome, usage }
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

        // 1 slice ぶん実行する。SliceOutcome を SlicePoll へ写す（REV-015 Slice 5、設計 §4.7）。
        match self.context.evaluator.run_slice(slice_fuel) {
            SliceOutcome::YieldedSliceFuel => SlicePoll::YieldedSliceFuel,
            SliceOutcome::YieldedHostCall { call_id } => SlicePoll::YieldedHostCall { call_id },
            SliceOutcome::YieldedExplicit => SlicePoll::YieldedExplicit,
            SliceOutcome::Terminal(Ok(())) => SlicePoll::Terminal(ExecutionOutcome::Completed),
            // 実行フェーズの失敗（transaction は commit/rollback を run_slice が済ませている）。
            // cancel / deadline は catch 不能 terminal 信号なので専用 outcome へ写し、
            // RuntimeError に埋もれさせない（REV-015 Slice 4、§8）。予算超過（limit 系）を
            // 含む他の未捕捉エラーは RuntimeError。
            SliceOutcome::Terminal(Err(error)) => {
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

/// [`ExecutionHandle::drive_one_slice`] の結果（REV-015 Slice 3 PR-d-2 / Slice 5 §4.3/§4.7）。
enum SlicePoll {
    /// slice fuel を使い切って協調停止した。continuation は評価器に残る。
    YieldedSliceFuel,
    /// cooperative host-call が `Pending` を返して停止した（Slice 5、設計 §4.4/§4.7）。
    YieldedHostCall { call_id: u64 },
    /// 明示 yield（本 Slice では未 trigger、設計 §4.7）。
    YieldedExplicit,
    /// terminal に達した。
    Terminal(ExecutionOutcome),
}

impl Drop for ExecutionHandle<'_, '_, '_> {
    /// 非 terminal handle の drop 時に cancel を linearize し、host-call を detach し、
    /// language-state を rollback する（REV-015 Slice 5、設計 §4.8）。panic / blocking I/O を
    /// しない。audit 実発行（orphan-audit queue / `AuditUnavailable`）は Phase 6 で追補する。
    fn drop(&mut self) {
        // terminal に到達済みなら何もしない（slot は field drop で解放される）。
        if self.state == ExecutionState::Terminal {
            return;
        }
        // 1. cancel linearize: cancel()->bool の false->true CAS が linearization point（§4.8 step 1）。
        //    既に cancel 済み / terminal 済みなら no-op。
        self.cancellation_token().cancel();
        // 2. pending host-call を Detached close: ticket cancel で遅着 result を破棄する（step 2）。
        if let Some(ticket) = &self.pending_ticket {
            ticket.cancel();
        }
        self.pending_ticket = None;
        // 3. language-state rollback + continuation 破棄（step 3、Finding 6）。rollback 要否は
        //    abort_session 内部の self.session 有無で自己判定する（started は見ない）。
        self.context.evaluator.abort_session();
        // 4. logical Terminal(Cancelled) append は context 側の cancel 済み状態で満たす。context は
        //    poison しない（正常な cancel 終了、§4.8 step 4 / §10 規則4）。
        // 5. slot 解放は末尾 field `slot: AdmissionSlot` の drop で 1 個だけ行われる（step 5、§4.1）。
    }
}
