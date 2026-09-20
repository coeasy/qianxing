//! clap 参数框架（V10 P2b）：命令表由本文件的 `Command` 枚举派生。
//!
//! `cli.rs` 只保留对 [`Command`] 的一次显式 `match`；`tools/check_architecture.py`
//! 以「clap 派生的命令表 ≡ `cli.rs` 显式派发分支 ≡ `cli_help.rs` 印出的入口」三方
//! 集合相等做门禁（V10 §4.4 的"文案宣称支持某入口而派发没有该分支"因此不可表达）。
//! `help` / `--help` / `-h` 是进入 clap 之前的同一条预检分支，门禁按规范名 `help` 计一条。
//! 命令名与旗标语义与迁移前逐条一致（§8.1 裁定）；旧手写字符串解析不保留双轨。

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "qx-cli", disable_help_subcommand = true)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

fn parse_quantity(value: &str) -> Result<i64, String> {
    value
        .parse::<i64>()
        .map_err(|error| format!("quantity 非法: {value}（{error}）"))
}

fn parse_funding_bps(value: &str) -> Result<i64, String> {
    match value.parse::<i64>() {
        Ok(parsed) if (0..=10_000).contains(&parsed) => Ok(parsed),
        _ => Err("必须在 0..=10000 内".to_string()),
    }
}

/// 顶层命令表：每个变体一个 `#[command(name = "…")]`，与 `cli_help.rs` 的入口一一对应。
#[derive(Subcommand)]
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
    },
    #[command(name = "live-check")]
    LiveCheck {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "runtime-check")]
    RuntimeCheck {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "strategy")]
    Strategy {
        #[command(subcommand)]
        action: Option<StrategyCommand>,
    },
    #[command(name = "backtest")]
    Backtest {
        runtime: Option<PathBuf>,
        frame: Option<PathBuf>,
        spec: Option<PathBuf>,
        /// 统一回测兜底与迁移前一致：`--config` 会被接受但不参与该链路。
        #[arg(long)]
        config: Option<PathBuf>,
        #[command(subcommand)]
        action: Option<BacktestCommand>,
    },
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
    #[command(name = "ccxt-market-spec")]
    CcxtMarketSpec {
        config: PathBuf,
        instrument: String,
        output: PathBuf,
    },
    #[command(name = "serve")]
    Serve {
        #[arg(default_value = "deploy/qianxing.runtime.example.json")]
        path: PathBuf,
    },
    #[command(name = "supervise")]
    Supervise {
        #[arg(default_value = "deploy/qianxing.runtime.example.json")]
        path: PathBuf,
        #[arg(long)]
        allow_unmanaged_roles: bool,
    },
    #[command(name = "scheduler-worker")]
    SchedulerWorker {
        #[arg(default_value = "deploy/qianxing.runtime.example.json")]
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "strategy-worker")]
    StrategyWorker {
        #[arg(default_value = "deploy/qianxing.runtime.example.json")]
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "paper-worker")]
    PaperWorker {
        #[arg(default_value = "deploy/qianxing.runtime.example.json")]
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "binance-worker")]
    BinanceWorker {
        #[arg(default_value = "deploy/qianxing.runtime.example.json")]
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "ccxt-worker")]
    CcxtWorker {
        #[arg(default_value = "deploy/qianxing.runtime.example.json")]
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
        limit: Option<usize>,
    },
    #[command(name = "outbox-relay-postgres")]
    OutboxRelayPostgres {
        runtime: PathBuf,
        url: String,
        subject_prefix: String,
        limit: Option<usize>,
    },
    #[command(name = "outbox-relay-worker")]
    OutboxRelayWorker {
        #[arg(default_value = "deploy/qianxing.runtime.example.json")]
        path: PathBuf,
        worker_id: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "event-consumer-worker")]
    EventConsumerWorker {
        #[arg(default_value = "deploy/qianxing.runtime.example.json")]
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
        #[arg(default_value = "deploy/qianxing.runtime.production.example.json")]
        path: PathBuf,
        #[arg(default_value = "binance-execution-main")]
        worker_id: String,
    },
    #[command(name = "binance-submit-order")]
    BinanceSubmitOrder {
        #[arg(default_value = "deploy/qianxing.runtime.production.example.json")]
        path: PathBuf,
        worker_id: String,
        command_path: PathBuf,
    },
    #[command(name = "paper-submit-order")]
    PaperSubmitOrder {
        #[arg(default_value = "deploy/qianxing.runtime.example.json")]
        path: PathBuf,
        command_path: PathBuf,
    },
    #[command(name = "paper-e2e")]
    PaperE2e {
        #[arg(default_value = "deploy/qianxing.runtime.paper-strategy.example.json")]
        path: PathBuf,
    },
    #[command(name = "paper-check")]
    PaperCheck {
        #[arg(default_value = "deploy/qianxing.runtime.paper-strategy.example.json")]
        path: PathBuf,
    },
    #[command(name = "reconcile")]
    Reconcile {
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
}

impl Command {
    /// 机读输出判定：与迁移前一致，`--json` 在这组命令上抑制横幅行。
    pub(crate) fn machine_output(&self) -> bool {
        match self {
            Self::Doctor { json }
            | Self::Status { json }
            | Self::Report { json }
            | Self::LiveCheck { json }
            | Self::RuntimeCheck { json } => *json,
            Self::Config { action } => {
                action.as_ref().is_some_and(ConfigCommand::machine_output)
            }
            Self::Run { action } => action.as_ref().is_some_and(RunCommand::machine_output),
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
    Validate {
        path: Option<PathBuf>,
        /// 与迁移前一致：validate 不产出 JSON，但该旗标仍抑制横幅行。
        #[arg(long)]
        json: bool,
    },
    #[command(name = "fingerprint")]
    Fingerprint {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "lock")]
    Lock {
        path: Option<PathBuf>,
        output: Option<PathBuf>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
}

impl ConfigCommand {
    fn machine_output(&self) -> bool {
        match self {
            Self::Explain { json }
            | Self::Validate { json }
            | Self::Fingerprint { json }
            | Self::Lock { json, .. } => *json,
        }
    }
}

/// `run` 的子命令表与 `RUN_ENTRY_POINTS`（错误文案的来源）由门禁做集合相等校验；
/// 参数向量按迁移前的宽容语义原样交给 `run_unified_command`（它是处理器，不是分派器）。
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
        #[arg(long = "funding-bps", value_parser = parse_funding_bps)]
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
        #[arg(long)]
        config: Option<PathBuf>,
    },
}
