//! clap 参数框架（V10 P2b）：命令表由本文件 `Command` 枚举派生，`cli.rs` 只保留对 `Command` 的一次显式 `match`；门禁以「clap 命令表 ≡ `cli.rs` 派发 ≡ `cli_help.rs` 印出」三方集合相等，使"文案宣称支持某入口而派发没有分支"不可表达。`help`/`--help`/`-h` 进 clap 前同一条预检分支（按规范名 `help` 计一条）；`<worker>` 等必填位置参数前必须显式给 runtime 路径（带 `default_value` 的会被 Clap 判为可选）。

use clap::{Parser, Subcommand};
use std::path::PathBuf;

use super::console_args::ConsoleArgs;
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

fn parse_quantity(value: &str) -> Result<i64, String> {
    value
        .parse::<i64>()
        .map_err(|error| format!("quantity 非法: {value}（{error}）"))
}

/// 基点类旗标共用 `0..=10000` 取值域，越界在 clap 层挡住。
fn parse_bps(value: &str) -> Result<i64, String> {
    match value.parse::<i64>() {
        Ok(parsed) if (0..=10_000).contains(&parsed) => Ok(parsed),
        _ => Err("必须在 0..=10000 内".to_string()),
    }
}

/// relay 的位置参 `limit` 与 `relay_batch_size` 同域；拒绝零值，避免空转却报成功（V13 R4）。
fn parse_relay_limit(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(parsed) if (1..=10_000).contains(&parsed) => Ok(parsed),
        _ => Err("relay limit 必须在 1..=10000 内".to_string()),
    }
}

/// 顶层命令表：每个变体一个 `#[command(name = "…")]`，与 `cli_help.rs` 的入口一一对应。
#[derive(Subcommand)]
// clap 要求每个子命令把旗标平铺在自己的变体里，最大的 `Backtest` 变体 392 字节；装箱能让 clippy 闭嘴，但会让 `cli.rs` 每条派发分支多一次解构，而这张表整个进程只构造一次。
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
        #[arg(long)]
        force: bool,
    },
    #[command(name = "doctor")]
    Doctor {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
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
    Report {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        html: bool,
        /// T1-1：复核通过后再聚一份 `<stem>.evidence.json`（opt-in，默认面产物集合不变）。
        #[arg(long)]
        evidence: bool,
        #[arg(short = 'o', long = "output", value_name = "PATH")]
        out: Option<PathBuf>,
    },
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
    // T2-2/G1：应用层用例入口；参数结构在 `app_args.rs`（cli_args.rs 顶格在行数棘轮上，而它是本仓第一个自带子命令的外部参数结构）。
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
    },
    #[command(name = "reconcile")]
    Reconcile {
        #[arg(value_parser = parse_deploy_path)]
        path: Option<PathBuf>,
        worker_id: Option<String>,
    },
    #[command(name = "ecosystem")]
    Ecosystem,
    #[command(name = "paper")]
    Paper,
    #[command(name = "all")]
    All,
    #[command(name = "verify")]
    Verify,
    #[command(name = "version")]
    Version,
}

impl Command {
    /// 机读输出判定：与迁移前一致，`--json` 在这组命令上抑制横幅行。
    pub(crate) fn machine_output(&self) -> bool {
        match self {
            Self::Doctor { json, .. }
            | Self::Status { json, .. }
            | Self::Report { json, .. }
            | Self::LiveCheck { json, .. }
            | Self::RuntimeCheck { json, .. } => *json,
            Self::Config { action } => action.as_ref().is_some_and(ConfigCommand::machine_output),
            Self::Run { action } => action.as_ref().is_some_and(RunCommand::machine_output),
            // version 只打一行构建身份，与 --json 同口径抑制横幅，便于脚本直接取值（U1）；
            // `app` 的 stdout 本来就只有一份 JSON，没有可切换的人读正文，所以同样一律抑制。
            Self::Version | Self::App(_) => true,
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
        // V12 R4-f：`config validate|fingerprint|lock` 曾收下 `--json` 却不产机读输出（只有
        // `config explain --json` 真的产 JSON）。旗标按“说谎即删”摘掉，现传 --json 由 clap 报未知参数。
        match self {
            Self::Explain { json, .. } => *json,
            Self::Validate { .. } | Self::Fingerprint { .. } | Self::Lock { .. } => false,
        }
    }
}

/// `run` 的子命令表与 `RUN_ENTRY_POINTS`（错误文案的来源）由门禁做集合相等校验；参数向量按迁移前的宽容语义原样交给 `run_unified_command`（它是处理器，不是分派器）。
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
        // 迁移前口径：仅这四个子入口吃 --json，backtest/paper/paper-check 不吃。
        match self {
            Self::Doctor { arguments }
            | Self::LiveCheck { arguments }
            | Self::RuntimeCheck { arguments }
            | Self::Report { arguments } => arguments.iter().any(|argument| argument == "--json"),
            Self::Backtest { .. } | Self::Paper { .. } | Self::PaperCheck { .. } => false,
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
        // 只放"内置策略这条路真能生效"的撮合参数：内核的 `queue_position_bps` 只作用于限价单所在
        // 档位，而 17 个内置策略发的全是市价单，配上也不改结果——声明换不动结果的旗标就是 Q0b 的"假风控"。
        #[arg(long = "market-impact-bps", value_parser = parse_bps)]
        market_impact_bps: Option<i64>,
        #[arg(long = "latency-snapshots")]
        latency_snapshots: Option<u64>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
}
