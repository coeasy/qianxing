//! clap 参数框架（V10 P2b）：命令表由本文件 `Command` 枚举派生，`cli.rs` 只保留对 `Command` 的一次显式 `match`；生成清单与显式派发逐项对账，帮助用例逐项运行真实子进程。`help`/`--help`/`-h` 进 clap 前同一条预检分支（按规范名 `help` 计一条）；`<worker>` 等必填位置参数前必须显式给 runtime 路径（带 `default_value` 的会被 Clap 判为可选）。

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

use super::cli_value_parsers::{parse_bps, parse_quantity, parse_relay_limit};
use super::console_args::ConsoleArgs;
use super::data_validate_args::DepthDatasetValidateArgs;
use super::evidence_signature_args::EvidenceArgs;
use super::report_args::ReportArgs;
use super::storage_adopt_args::PostgresSchemaAdoptArgs;
use super::{
    app_args::AppArgs, data_validate_args::DatasetValidateArgs, parse_deploy_path,
    plan_args::PlanArgs,
};

/// 使用者要敲的程序名：`#[command(name = …)]` 与所有引导文案共用这一份。不在别处再抄：`init` 的「下一步」曾印成 `qianxing …`，而装好的机器上没有那个名字，首跑第二句就是 command not found（V13 R2 #260）。
pub(crate) const PROGRAM_NAME: &str = "qx-cli";

#[derive(Parser)]
#[command(name = PROGRAM_NAME, bin_name = PROGRAM_NAME, disable_help_subcommand = true)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Command {
    #[command(name = "init")]
    Init {
        output: Option<PathBuf>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        strategy: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    #[command(name = "quickstart")]
    Quickstart {
        project: Option<PathBuf>,
        /// 额外生成自包含 HTML 报告与三张 SVG 图表。
        #[arg(long)]
        html: bool,
        #[arg(long)]
        force: bool,
    },
    #[command(name = "guide")]
    Guide(super::guide_args::GuideArgs),
    #[command(name = "doctor")]
    Doctor {
        #[arg(conflicts_with = "capabilities")]
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        /// Print the offline product capability catalog instead of checking a runtime configuration.
        #[arg(long)]
        capabilities: bool,
        /// Show full support limits and linked maturity evidence for one stable capability ID.
        #[arg(long, requires = "capabilities")]
        capability_id: Option<String>,
        /// Filter declared execution scopes by venue (for example BINANCE, OKX, BITGET).
        #[arg(long, requires = "capabilities")]
        venue: Option<String>,
        /// Filter declared execution scopes by product (for example spot or perpetual).
        #[arg(long, requires = "capabilities")]
        product: Option<String>,
        /// Filter by normalized account mode: cash, one_way, or hedge.
        #[arg(long, requires = "capabilities")]
        account_mode: Option<String>,
        /// Filter by venue account model (for example classic, unified, or uta).
        #[arg(long, requires = "capabilities")]
        account_model: Option<String>,
        /// Filter by margin mode (cash, cross, or isolated).
        #[arg(long, requires = "capabilities")]
        margin_mode: Option<String>,
        /// Filter private account-read scopes (for example balance.read or account_settings.read).
        #[arg(long, requires = "capabilities")]
        operation: Option<String>,
    },
    #[command(name = "config")]
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommand>,
    },
    #[command(name = "run")]
    Run {
        #[command(subcommand)]
        action: Option<RunCommand>,
    },
    #[command(name = "status")]
    Status {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "report")]
    Report(ReportArgs),
    #[command(name = "live-check")]
    LiveCheck {
        #[arg(default_value = "deploy/qianxing.runtime.production.example.json", value_parser = parse_deploy_path)]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "runtime-check")]
    RuntimeCheck {
        #[arg(default_value = "deploy/qianxing.runtime.example.json", value_parser = parse_deploy_path)]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "strategy")]
    Strategy {
        #[command(subcommand)]
        action: Option<StrategyCommand>,
    },
    // V12 R4-e：外层 `[runtime] [frame] [spec]` 与子命令自带的输入是两条链，混写时子命令那条链不读外层值；clap 4.6 的 `args_conflicts_with_subcommands` 会把子命令名当第三个位置参数吃掉，所以互斥改在派发处拒绝（`reject_shadowed_backtest_inputs`）。
    #[command(name = "backtest")]
    Backtest {
        #[arg(value_parser = parse_deploy_path)]
        runtime: Option<PathBuf>,
        #[arg(value_parser = parse_deploy_path)]
        frame: Option<PathBuf>,
        #[arg(value_parser = parse_deploy_path)]
        spec: Option<PathBuf>,
        #[command(subcommand)]
        action: Option<BacktestCommand>,
    },
    #[command(name = "app")]
    App(AppArgs),
    #[command(name = "builtin-strategies")]
    BuiltinStrategies,
    #[command(name = "fast-backtest")]
    FastBacktest { manifest: PathBuf },
    #[command(name = "dataset-ingest")]
    DatasetIngest {
        frame: PathBuf,
        dataset_id: String,
        version: String,
        data_dir: PathBuf,
    },
    #[command(name = "dataset-bundle")]
    DatasetBundle {
        bundle: PathBuf,
        data_dir: PathBuf,
        bars: Option<PathBuf>,
    },
    #[command(name = "data-validate")]
    DataValidate(DatasetValidateArgs),
    #[command(name = "data-validate-depth")]
    DataValidateDepth(DepthDatasetValidateArgs),
    #[command(name = "plan")]
    Plan(PlanArgs),
    #[command(name = "ccxt-market-spec")]
    CcxtMarketSpec {
        config: PathBuf,
        instrument: String,
        output: PathBuf,
    },
    #[command(name = "serve")]
    Serve {
        #[arg(default_value = "deploy/qianxing.runtime.example.json", value_parser = parse_deploy_path)]
        path: PathBuf,
    },
    #[command(name = "console")]
    Console(ConsoleArgs),
    #[command(name = "supervise")]
    Supervise {
        #[arg(default_value = "deploy/qianxing.runtime.example.json", value_parser = parse_deploy_path)]
        path: PathBuf,
        #[arg(long)]
        allow_unmanaged_roles: bool,
    },
    #[command(name = "scheduler-worker")]
    SchedulerWorker {
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "strategy-worker")]
    StrategyWorker {
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "paper-worker")]
    PaperWorker {
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "binance-worker")]
    BinanceWorker {
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "ccxt-worker")]
    CcxtWorker {
        path: PathBuf,
        worker_id: String,
        ccxt_config: PathBuf,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "ccxt-fetch-ohlcv")]
    CcxtFetchOhlcv {
        ccxt_config: PathBuf,
        instrument: String,
        start_ms: u64,
        end_ms: u64,
        output: PathBuf,
        #[arg(default_value = "1m")]
        timeframe: String,
    },
    #[command(name = "outbox-relay")]
    OutboxRelay {
        root: PathBuf,
        url: String,
        subject_prefix: String,
        #[arg(value_parser = parse_relay_limit)]
        limit: Option<usize>,
    },
    #[command(name = "outbox-relay-postgres")]
    OutboxRelayPostgres {
        runtime: PathBuf,
        url: String,
        subject_prefix: String,
        #[arg(value_parser = parse_relay_limit)]
        limit: Option<usize>,
    },
    #[command(name = "outbox-relay-worker")]
    OutboxRelayWorker {
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "event-consumer-worker")]
    EventConsumerWorker {
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "consumer-dlq-replay")]
    ConsumerDlqReplay {
        runtime: PathBuf,
        group_id: String,
        event_id: String,
    },
    #[command(name = "recovery-child")]
    RecoveryChild {
        /// 位置参数全部按迁移前的原样透传给 `run_recovery_child`（其报错文案已验收）。
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    #[command(name = "binance-public-probe")]
    BinancePublicProbe {
        #[arg(default_value = "testnet")]
        network: String,
        #[arg(default_value = "BTCUSDT.BINANCE")]
        instrument: String,
    },
    #[command(name = "binance-private-probe")]
    BinancePrivateProbe {
        #[arg(default_value = "deploy/qianxing.runtime.production.example.json", value_parser = parse_deploy_path)]
        path: PathBuf,
        #[arg(default_value = "binance-execution-main")]
        worker_id: String,
    },
    #[command(name = "binance-submit-order")]
    BinanceSubmitOrder {
        path: PathBuf,
        worker_id: String,
        command_path: PathBuf,
    },
    #[command(name = "paper-submit-order")]
    PaperSubmitOrder {
        #[arg(value_parser = parse_deploy_path)]
        path: PathBuf,
        #[arg(value_parser = parse_deploy_path)]
        command_path: PathBuf,
    },
    #[command(name = "ccxt-submit-order")]
    CcxtSubmitOrder(crate::ccxt_submit_args::CcxtSubmitArgs),
    #[command(name = "paper-e2e")]
    PaperE2e {
        #[arg(default_value = "deploy/qianxing.runtime.paper-strategy.example.json", value_parser = parse_deploy_path)]
        path: PathBuf,
    },
    #[command(name = "paper-check")]
    PaperCheck {
        #[arg(default_value = "deploy/qianxing.runtime.paper-strategy.example.json", value_parser = parse_deploy_path)]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "reconcile")]
    Reconcile {
        #[arg(value_parser = parse_deploy_path)]
        path: Option<PathBuf>,
        worker_id: Option<String>,
    },
    #[command(name = "ecosystem")]
    Ecosystem(SmokeReportArgs),
    #[command(name = "paper")]
    Paper,
    #[command(name = "all")]
    All(SmokeReportArgs),
    #[command(name = "verify")]
    Verify(SmokeReportArgs),
    #[command(name = "version")]
    Version,
    /// Adopt an unversioned PostgreSQL schema only when it matches a known migration exactly.
    #[command(name = "storage-adopt-postgres")]
    StorageAdoptPostgres(PostgresSchemaAdoptArgs),
    #[command(name = "evidence")]
    Evidence(EvidenceArgs),
}
#[derive(Args)]
pub(crate) struct SmokeReportArgs {
    #[arg(long)]
    pub(crate) json: bool,
}

impl Command {
    /// 机读输出判定：与迁移前一致，`--json` 在这组命令上抑制横幅行。
    pub(crate) fn machine_output(&self) -> bool {
        match self {
            Self::Doctor { json, .. }
            | Self::Status { json, .. }
            | Self::LiveCheck { json, .. }
            | Self::RuntimeCheck { json, .. }
            | Self::PaperCheck { json, .. } => *json,
            Self::DataValidate(args) => args.json,
            Self::Report(args) => args.json,
            Self::Plan(args) => args.json,
            Self::Ecosystem(args) | Self::All(args) | Self::Verify(args) => args.json,
            Self::Config { action } => action.as_ref().is_some_and(ConfigCommand::machine_output),
            Self::Run { action } => action.as_ref().is_some_and(RunCommand::machine_output),
            // version 与 app 只有机读正文，和 --json 一样抑制横幅（U1）。
            Self::Version | Self::App(_) | Self::StorageAdoptPostgres(_) | Self::Evidence(_) => {
                true
            }
            _ => false,
        }
    }
}
#[derive(Subcommand)]
pub(crate) enum ConfigCommand {
    #[command(name = "explain")]
    Explain {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "validate")]
    Validate { path: Option<PathBuf> },
    #[command(name = "fingerprint")]
    Fingerprint { path: Option<PathBuf> },
    #[command(name = "lock")]
    Lock {
        path: Option<PathBuf>,
        output: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
}
impl ConfigCommand {
    fn machine_output(&self) -> bool {
        // V12 R4-f：只有 explain 产 JSON，其余子命令传 `--json` 由 clap 拒绝。
        match self {
            Self::Explain { json, .. } => *json,
            Self::Validate { .. } | Self::Fingerprint { .. } | Self::Lock { .. } => false,
        }
    }
}
/// `run` 子命令由生成清单供帮助测试与错误文案消费；参数向量按迁移前的宽容语义原样交给 `run_unified_command`（它是处理器，不是分派器）。
#[derive(Subcommand)]
pub(crate) enum RunCommand {
    #[command(name = "backtest")]
    Backtest {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    #[command(name = "paper")]
    Paper {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    #[command(name = "paper-check")]
    PaperCheck {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    #[command(name = "doctor")]
    Doctor {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    #[command(name = "live-check")]
    LiveCheck {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    #[command(name = "runtime-check")]
    RuntimeCheck {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    #[command(name = "report")]
    Report {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
}
impl RunCommand {
    fn machine_output(&self) -> bool {
        // 迁移前口径：诊断/报告子入口吃 --json；backtest/paper 不吃。
        match self {
            Self::Doctor { arguments }
            | Self::LiveCheck { arguments }
            | Self::RuntimeCheck { arguments }
            | Self::Report { arguments }
            | Self::PaperCheck { arguments } => {
                arguments.iter().any(|argument| argument == "--json")
            }
            Self::Backtest { .. } | Self::Paper { .. } => false,
        }
    }
}
#[derive(Subcommand)]
pub(crate) enum StrategyCommand {
    #[command(name = "list")]
    List,
    #[command(name = "init")]
    Init {
        name: String,
        output: Option<PathBuf>,
        bars: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    #[command(name = "backtest")]
    Backtest {
        runtime: PathBuf,
        bars: PathBuf,
        spec: Option<PathBuf>,
        #[arg(long = "dataset-manifest", value_name = "DATASET_MANIFEST_V2")]
        dataset_manifest: Option<PathBuf>,
    },
    /// 从已复核的多市场深度运行产物生成内置策略预热诊断。
    #[command(name = "diagnose-depth")]
    DiagnoseDepth {
        artifact: PathBuf,
        #[arg(short = 'o', long = "output", value_name = "JSON")]
        output: PathBuf,
        /// 诊断 JSON 写出后立即生成对应的交互式 HTML 研究报告。
        #[arg(long)]
        html: bool,
    },
    /// 从已复核的多市场 Bar 运行产物生成所有 Rust 内置策略偏差诊断。
    #[command(name = "diagnose-multi-bar")]
    DiagnoseMultiBar {
        artifact: PathBuf,
        #[arg(short = 'o', long = "output", value_name = "JSON")]
        output: PathBuf,
        /// 诊断 JSON 写出后立即生成对应的交互式 HTML 研究报告。
        #[arg(long)]
        html: bool,
    },
    /// 从独立账户 Bar 批次生成逐账户 Rust 内置策略偏差诊断。
    #[command(name = "diagnose-multi-account")]
    DiagnoseMultiAccount {
        artifact: PathBuf,
        #[arg(long, value_name = "MULTI_ACCOUNT_SPEC_JSON")]
        spec: PathBuf,
        #[arg(short = 'o', long = "output", value_name = "JSON")]
        output: PathBuf,
        /// 诊断 JSON 写出后立即生成对应的交互式 HTML 研究报告。
        #[arg(long)]
        html: bool,
    },
    /// 从已验证的单标的 Bar 回测摘要和原始规格重建 Rust 内置策略偏差诊断。
    #[command(name = "diagnose-bar")]
    DiagnoseBar {
        summary: PathBuf,
        #[arg(long, value_name = "BACKTEST_SPEC_JSON")]
        spec: PathBuf,
        #[arg(short = 'o', long = "output", value_name = "JSON")]
        output: PathBuf,
        /// 诊断 JSON 写出后立即生成对应的交互式 HTML 研究报告。
        #[arg(long)]
        html: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum BacktestCommand {
    #[command(name = "builtin")]
    Builtin {
        strategy: String,
        frame: PathBuf,
        spec: Option<PathBuf>,
        #[arg(value_parser = parse_quantity)]
        quantity: Option<i64>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        dataset_manifest: Option<PathBuf>,
    },
    #[command(name = "multi-builtin")]
    MultiBuiltin {
        strategy: String,
        primary_bar: PathBuf,
        reference_bar: PathBuf,
        primary_spec: Option<PathBuf>,
        reference_spec: Option<PathBuf>,
        #[arg(value_parser = parse_quantity)]
        positional_quantity: Option<i64>,
        #[arg(long = "quantity", value_parser = parse_quantity)]
        quantity: Option<i64>,
        #[arg(long = "funding-bps", value_parser = parse_bps)]
        funding_bps: Option<i64>,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        primary_dataset_manifest: Option<PathBuf>,
        #[arg(long)]
        reference_dataset_manifest: Option<PathBuf>,
    },
    #[command(name = "ccxt-builtin")]
    CcxtBuiltin {
        ccxt_config: PathBuf,
        strategy: String,
        instrument: String,
        start_ms: u64,
        end_ms: u64,
        #[arg(default_value = "1h")]
        timeframe: String,
        spec: Option<PathBuf>,
        #[arg(value_parser = parse_quantity)]
        quantity: Option<i64>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    #[command(name = "book")]
    Book {
        #[arg(long = "fill-tier")]
        fill_tier: String,
        #[arg(long)]
        root: PathBuf,
        strategy: String,
        frame: PathBuf,
        spec: Option<PathBuf>,
        #[arg(value_parser = parse_quantity)]
        quantity: Option<i64>,
        #[arg(long = "fee-bps")]
        fee_bps: Option<i64>,
        // 内置策略仅发市价单；queue_position_bps 只作用于限价，因此不提供无效旗标。
        #[arg(long = "market-impact-bps", value_parser = parse_bps)]
        market_impact_bps: Option<i64>,
        #[arg(long = "latency-snapshots")]
        latency_snapshots: Option<u64>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long = "dataset-manifest", value_name = "DATASET_MANIFEST_V2")]
        dataset_manifest: Option<PathBuf>,
    },
}
