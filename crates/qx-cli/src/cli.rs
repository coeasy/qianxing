//! CLI 入口分派：clap 派生的 [`Command`] 变体 → 具体处理器。
//!
//! 参数语法与命令表都在 `cli_args.rs`（V10 P2b：clap `Subcommand` 迁移，旧手写
//! 字符串解析已整体删除、不留双轨）；本模块只保留一次对 `Command` 的显式 `match`、
//! 横幅行的机读输出判定与错误出口（退出码、`--json` 语义）。命令名与旗标语义与
//! 迁移前逐条一致，`tools/check_architecture.py` 校验「clap 命令表 ≡ 这里的分支集合
//! ≡ help 印出的入口」。所有业务实现仍直接复用 crate 根的同一批函数。

use super::cli_args::{BacktestCommand, Cli, Command, ConfigCommand, RunCommand, StrategyCommand};
use super::*;
use clap::Parser;

fn print_banner() {
    println!("牵星 Qianxing — 分级校准，量天定位\n");
}

/// 旧派发在用法错误（未知命令/未知参数）时同样先打印横幅，再打印帮助并退出 2。
fn fail_usage(error: clap::Error) -> ! {
    if matches!(
        error.kind(),
        clap::error::ErrorKind::DisplayHelp
            | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        let _ = error.print();
        std::process::exit(0);
    }
    print_banner();
    // 前缀同时包含「未知命令」与「未知参数」：验收器（tests/cli_dispatch.rs、
    // tests/multi_leg_attribution.rs）分别点名这两种失败。
    eprintln!("未知命令或未知参数: {error}");
    print_cli_help();
    std::process::exit(2);
}

fn run_arguments(entry: &str, mut arguments: Vec<String>) -> Vec<String> {
    arguments.insert(0, entry.to_string());
    arguments
}

fn print_builtin_strategies() {
    for kind in BuiltinStrategyKind::ALL {
        println!("{}\t{}", kind.name(), kind.description());
    }
}

#[cfg(feature = "nats")]
fn dispatch_outbox_relay(root: PathBuf, url: String, subject_prefix: String, limit: Option<usize>) {
    if let Err(error) = run_file_outbox_relay(&root, &url, &subject_prefix, limit.unwrap_or(100)) {
        eprintln!("Outbox relay 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
fn dispatch_outbox_relay(
    _root: PathBuf,
    _url: String,
    _subject_prefix: String,
    _limit: Option<usize>,
) {
    eprintln!("outbox-relay 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

#[cfg(all(feature = "nats", feature = "postgres"))]
fn dispatch_outbox_relay_postgres(
    runtime: PathBuf,
    url: String,
    subject_prefix: String,
    limit: Option<usize>,
) {
    if let Err(error) =
        run_postgres_outbox_relay(&runtime, &url, &subject_prefix, limit.unwrap_or(100))
    {
        eprintln!("PostgreSQL Outbox relay 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(all(feature = "nats", feature = "postgres")))]
fn dispatch_outbox_relay_postgres(
    _runtime: PathBuf,
    _url: String,
    _subject_prefix: String,
    _limit: Option<usize>,
) {
    eprintln!("outbox-relay-postgres 需要使用 --features 'nats postgres' 构建 qx-cli");
    std::process::exit(2);
}

#[cfg(feature = "nats")]
fn dispatch_outbox_relay_worker(path: PathBuf, worker_id: String, once: bool) {
    if let Err(error) = run_outbox_relay_worker(&path, &worker_id, once) {
        eprintln!("Outbox relay worker 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
fn dispatch_outbox_relay_worker(_path: PathBuf, _worker_id: String, _once: bool) {
    eprintln!("outbox-relay-worker 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

#[cfg(feature = "nats")]
fn dispatch_event_consumer_worker(path: PathBuf, worker_id: String, once: bool) {
    if let Err(error) = run_event_consumer_worker(&path, &worker_id, once) {
        eprintln!("Event consumer worker 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
fn dispatch_event_consumer_worker(_path: PathBuf, _worker_id: String, _once: bool) {
    eprintln!("event-consumer-worker 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

#[cfg(feature = "nats")]
fn dispatch_consumer_dlq_replay(runtime: PathBuf, group_id: String, event_id: String) {
    if let Err(error) = run_dead_letter_replay(&runtime, &group_id, &event_id) {
        eprintln!("DLQ 重放失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
fn dispatch_consumer_dlq_replay(_runtime: PathBuf, _group_id: String, _event_id: String) {
    eprintln!("consumer-dlq-replay 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

pub(crate) fn run() {
    let argv: Vec<String> = std::env::args().collect();
    // `help` 与 `--help`/`-h` 是同一条预检分支（迁移前后一致：打印入口摘要横幅帮助）。
    if matches!(
        argv.get(1).map(String::as_str),
        Some("help") | Some("--help") | Some("-h")
    ) {
        print_banner();
        print_cli_help();
        return;
    }
    let cli = match Cli::try_parse_from(argv.iter().map(String::as_str)) {
        Ok(cli) => cli,
        Err(error) => fail_usage(error),
    };
    let Some(command) = cli.command else {
        // 空 argv 与显式 `all` 同义（迁移前的默认 mode）。
        print_banner();
        selfcheck::run(selfcheck::Scope::Full);
        return;
    };
    if !command.machine_output() {
        print_banner();
    }
    match command {
        Command::Init {
            output,
            force,
            strategy,
            profile,
        } => {
            if strategy.as_deref() == Some("") {
                eprintln!("init --strategy= 缺少策略名称");
                std::process::exit(2);
            }
            if profile.as_deref() == Some("") {
                eprintln!("init --profile= 缺少场景名称");
                std::process::exit(2);
            }
            let output = output.unwrap_or_else(|| PathBuf::from("qianxing.runtime.json"));
            if let Err(error) =
                run_init_with_profile(&output, force, strategy.as_deref(), profile.as_deref())
            {
                eprintln!("初始化失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Doctor { path, json } => {
            let path = path.unwrap_or_else(default_runtime_path);
            if let Err(error) = run_doctor(&path, json) {
                eprintln!("Doctor 失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Config { action } => {
            let validate_path = match &action {
                Some(ConfigCommand::Validate { path, .. }) => path.clone(),
                _ => None,
            };
            let result = match action {
                Some(ConfigCommand::Explain { path, json }) => {
                    run_config_explain(&path.unwrap_or_else(default_runtime_path), json)
                }
                Some(ConfigCommand::Validate { .. }) | None => match read_runtime_config(
                    &validate_path.clone().unwrap_or_else(default_runtime_path),
                ) {
                    Ok(config) => {
                        let path = validate_path.unwrap_or_else(default_runtime_path);
                        let (failures, warnings) = validate_runtime_references(&path, &config);
                        for warning in warnings {
                            println!("[WARN] {warning}");
                        }
                        if failures.is_empty() {
                            println!("[PASS] config validate 通过: {}", path.display());
                            Ok(())
                        } else {
                            for failure in &failures {
                                eprintln!("[FAIL] {failure}");
                            }
                            Err(format!("配置引用校验失败，共 {} 项", failures.len()))
                        }
                    }
                    Err(error) => Err(error),
                },
                Some(ConfigCommand::Fingerprint { path, .. }) => {
                    run_config_fingerprint(&path.unwrap_or_else(default_runtime_path))
                }
                Some(ConfigCommand::Lock {
                    path,
                    output,
                    force,
                    ..
                }) => {
                    let path = path.unwrap_or_else(default_runtime_path);
                    let output = output.unwrap_or_else(|| {
                        let stem = path
                            .file_stem()
                            .and_then(|value| value.to_str())
                            .unwrap_or("qianxing.runtime");
                        path.with_file_name(format!("{stem}.locked.json"))
                    });
                    run_config_lock(&path, &output, force)
                }
            };
            if let Err(error) = result {
                eprintln!("配置命令失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Run { action } => {
            let arguments = match action {
                Some(RunCommand::Backtest { arguments }) => run_arguments("backtest", arguments),
                Some(RunCommand::Paper { arguments }) => run_arguments("paper", arguments),
                Some(RunCommand::PaperCheck { arguments }) => {
                    run_arguments("paper-check", arguments)
                }
                Some(RunCommand::Doctor { arguments }) => run_arguments("doctor", arguments),
                Some(RunCommand::LiveCheck { arguments }) => run_arguments("live-check", arguments),
                Some(RunCommand::RuntimeCheck { arguments }) => {
                    run_arguments("runtime-check", arguments)
                }
                Some(RunCommand::Report { arguments }) => run_arguments("report", arguments),
                None => Vec::new(),
            };
            if let Err(error) = run_unified_command(&arguments) {
                eprintln!("统一运行入口失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Status { path, json } => {
            let path = path.unwrap_or_else(default_runtime_path);
            if let Err(error) = run_status(&path, json) {
                eprintln!("状态查看失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Report { path, json } => {
            let path = path.unwrap_or_else(default_runtime_path);
            if let Err(error) = run_report(&path, json) {
                eprintln!("报告查看失败: {error}");
                std::process::exit(2);
            }
        }
        Command::LiveCheck { path, json } => {
            let path = path.unwrap_or_else(|| {
                PathBuf::from("deploy/qianxing.runtime.production.example.json")
            });
            if let Err(error) = run_live_check(&path, json) {
                eprintln!("实盘前置检查失败: {error}");
                std::process::exit(2);
            }
        }
        Command::RuntimeCheck { path, json } => {
            let path =
                path.unwrap_or_else(|| PathBuf::from("deploy/qianxing.runtime.example.json"));
            if let Err(error) = run_runtime_check(&path, json) {
                eprintln!("运行时配置校验失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Strategy { action } => match action {
            None | Some(StrategyCommand::List) => print_builtin_strategies(),
            Some(StrategyCommand::Init {
                name,
                output,
                bars,
                force,
            }) => {
                let output = output
                    .unwrap_or_else(|| PathBuf::from(format!("qianxing.strategy.{name}.json")));
                if let Err(error) = run_strategy_init(&name, &output, bars.as_deref(), force) {
                    eprintln!("策略初始化失败: {error}");
                    std::process::exit(2);
                }
            }
            Some(StrategyCommand::Backtest {
                runtime,
                bars,
                spec,
            }) => {
                if let Err(error) = run_strategy_backtest(&runtime, &bars, spec.as_deref()) {
                    eprintln!("策略回测失败: {error}");
                    std::process::exit(2);
                }
            }
        },
        Command::Backtest {
            runtime,
            frame,
            spec,
            config: _,
            action,
        } => match action {
            None => {
                if let Err(error) =
                    run_unified_backtest(runtime.as_deref(), frame.as_deref(), spec.as_deref())
                {
                    eprintln!("统一策略回测失败: {error}");
                    std::process::exit(2);
                }
            }
            Some(BacktestCommand::Builtin {
                strategy,
                frame,
                spec,
                quantity,
                config,
            }) => {
                // 命令行型回测入口的 `--config` 与 strategy 回测吃同一份 `strategy.risk_rules`。
                if let Err(error) = run_builtin_backtest(
                    &strategy,
                    &frame,
                    spec.as_deref(),
                    quantity.unwrap_or(1),
                    config.as_deref(),
                ) {
                    eprintln!("内置策略回测失败: {error}");
                    std::process::exit(2);
                }
            }
            Some(BacktestCommand::MultiBuiltin {
                strategy,
                primary_bar,
                reference_bar,
                primary_spec,
                reference_spec,
                positional_quantity,
                quantity,
                funding_bps,
                root,
                config,
            }) => {
                let quantity = quantity.or(positional_quantity).unwrap_or(1);
                if let Err(error) = run_multi_builtin_backtest(
                    &strategy,
                    &primary_bar,
                    &reference_bar,
                    primary_spec.as_deref(),
                    reference_spec.as_deref(),
                    quantity,
                    funding_bps.unwrap_or(0),
                    root.as_deref(),
                    config.as_deref(),
                ) {
                    eprintln!("多腿内置策略回测失败: {error}");
                    std::process::exit(2);
                }
            }
            Some(BacktestCommand::CcxtBuiltin {
                ccxt_config,
                strategy,
                instrument,
                start_ms,
                end_ms,
                timeframe,
                spec,
                quantity,
                config,
            }) => {
                if let Err(error) = run_ccxt_builtin_backtest(
                    &ccxt_config,
                    &strategy,
                    &instrument,
                    &timeframe,
                    start_ms,
                    end_ms,
                    spec.as_deref(),
                    quantity.unwrap_or(1),
                    config.as_deref(),
                ) {
                    eprintln!("CCXT 内置策略回测失败: {error}");
                    std::process::exit(2);
                }
            }
            Some(BacktestCommand::Book {
                fill_tier,
                root,
                strategy,
                frame,
                spec,
                quantity,
                fee_bps,
                config,
            }) => {
                if let Err(error) = run_depth_backtest(
                    &fill_tier,
                    &strategy,
                    &frame,
                    spec.as_deref(),
                    quantity.unwrap_or(1),
                    fee_bps.unwrap_or(5).into(),
                    &root,
                    config.as_deref(),
                ) {
                    eprintln!("深度档位回测失败: {error}");
                    std::process::exit(2);
                }
            }
        },
        Command::BuiltinStrategies => print_builtin_strategies(),
        Command::FastBacktest { manifest } => {
            if let Err(error) = run_fast_backtest_manifest(&manifest) {
                eprintln!("快速批量回测失败: {error}");
                std::process::exit(2);
            }
        }
        Command::DatasetIngest {
            frame,
            dataset_id,
            version,
            data_dir,
        } => {
            if let Err(error) = run_dataset_ingest(&frame, &dataset_id, &version, &data_dir) {
                eprintln!("数据集摄取失败: {error}");
                std::process::exit(2);
            }
        }
        Command::DatasetBundle {
            bundle,
            data_dir,
            bars,
        } => {
            if let Err(error) = run_dataset_bundle(&bundle, &data_dir, bars.as_deref()) {
                eprintln!("数据集 Bundle 保存失败: {error}");
                std::process::exit(2);
            }
        }
        Command::CcxtMarketSpec {
            config,
            instrument,
            output,
        } => {
            if let Err(error) = run_ccxt_market_spec(&config, &instrument, &output) {
                eprintln!("CCXT market spec 下载失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Serve { path } => {
            if let Err(error) = run_runtime_api(&path) {
                eprintln!("运行时 API 启动失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Supervise {
            path,
            allow_unmanaged_roles,
        } => {
            if let Err(error) = run_process_supervisor(&path, allow_unmanaged_roles) {
                eprintln!("进程监督器停止: {error}");
                std::process::exit(2);
            }
        }
        Command::SchedulerWorker {
            path,
            worker_id,
            once,
        } => {
            if let Err(error) = run_scheduler_worker(&path, &worker_id, once) {
                eprintln!("scheduler-worker 启动/运行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::StrategyWorker {
            path,
            worker_id,
            once,
        } => {
            if let Err(error) = run_strategy_worker(&path, &worker_id, once) {
                eprintln!("strategy-worker 启动/运行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::PaperWorker {
            path,
            worker_id,
            once,
        } => {
            if let Err(error) = run_paper_execution_worker(&path, &worker_id, once) {
                eprintln!("Paper Execution worker 启动/运行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::BinanceWorker {
            path,
            worker_id,
            once,
        } => {
            if let Err(error) = run_binance_worker(&path, &worker_id, once) {
                eprintln!("Binance worker 启动/运行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::CcxtWorker {
            path,
            worker_id,
            ccxt_config,
            once,
        } => {
            if let Err(error) = run_ccxt_worker(&path, &worker_id, &ccxt_config, once) {
                eprintln!("CCXT worker 启动/运行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::CcxtFetchOhlcv {
            ccxt_config,
            instrument,
            start_ms,
            end_ms,
            output,
            timeframe,
        } => {
            if let Err(error) = run_ccxt_fetch_ohlcv(
                &ccxt_config,
                &instrument,
                &timeframe,
                start_ms,
                end_ms,
                &output,
            ) {
                eprintln!("CCXT OHLCV 下载失败: {error}");
                std::process::exit(2);
            }
        }
        Command::OutboxRelay {
            root,
            url,
            subject_prefix,
            limit,
        } => dispatch_outbox_relay(root, url, subject_prefix, limit),
        Command::OutboxRelayPostgres {
            runtime,
            url,
            subject_prefix,
            limit,
        } => dispatch_outbox_relay_postgres(runtime, url, subject_prefix, limit),
        Command::OutboxRelayWorker {
            path,
            worker_id,
            once,
        } => {
            dispatch_outbox_relay_worker(path, worker_id, once);
        }
        Command::EventConsumerWorker {
            path,
            worker_id,
            once,
        } => {
            dispatch_event_consumer_worker(path, worker_id, once);
        }
        Command::ConsumerDlqReplay {
            runtime,
            group_id,
            event_id,
        } => dispatch_consumer_dlq_replay(runtime, group_id, event_id),
        Command::RecoveryChild { arguments } => {
            // 位置参数按迁移前的 argv 下标约定透传：index 0/1 是占位（可执行文件与模式名）。
            let mut argv = vec![String::new(), "recovery-child".to_string()];
            argv.extend(arguments);
            if let Err(error) = run_recovery_child(&argv) {
                eprintln!("跨进程恢复子进程失败: {error}");
                std::process::exit(2);
            }
        }
        Command::BinancePublicProbe {
            network,
            instrument,
        } => {
            let testnet = match network.as_str() {
                "testnet" => true,
                "mainnet" => false,
                _ => {
                    eprintln!("binance-public-probe network 必须是 testnet 或 mainnet");
                    std::process::exit(2);
                }
            };
            if let Err(error) = run_binance_public_probe(testnet, &instrument) {
                eprintln!("Binance public probe 失败: {error}");
                std::process::exit(2);
            }
        }
        Command::BinancePrivateProbe { path, worker_id } => {
            if let Err(error) = run_binance_private_probe(&path, &worker_id) {
                eprintln!("Binance private probe 失败: {error}");
                std::process::exit(2);
            }
        }
        Command::BinanceSubmitOrder {
            path,
            worker_id,
            command_path,
        } => {
            if let Err(error) = run_binance_submit_order(&path, &worker_id, &command_path) {
                eprintln!("Binance SubmitOrder 执行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::PaperSubmitOrder { path, command_path } => {
            if let Err(error) = run_paper_submit_order(&path, &command_path) {
                eprintln!("Paper SubmitOrder 执行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::PaperE2e { path } => {
            if let Err(error) = run_paper_pipeline_once(&path) {
                eprintln!("Paper 主链路验收失败: {error}");
                std::process::exit(2);
            }
        }
        Command::PaperCheck { path } => {
            if let Err(error) = run_paper_pipeline_once(&path) {
                eprintln!("Paper 主链路验收失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Reconcile { path, worker_id } => {
            if let Some(path) = path {
                let worker_id = worker_id.unwrap_or_else(|| "reconciler-main".into());
                if let Err(error) = run_binance_worker(&path, &worker_id, true) {
                    eprintln!("Binance 对账失败: {error}");
                    std::process::exit(2);
                }
            } else {
                // V10 §4.3：无参时曾用两份手写向量打印"差异"，看起来像真对账能力。
                // 对账必须同时点名本地与远端来源，缺任一即是用法错误。
                eprintln!(
                    "reconcile 需要本地与远端两个来源：reconcile <runtime.json> [worker-id]\n  \
                     本地来源 = 运行时配置指向的 EventLog 账本\n  \
                     远端来源 = 该配置中 Binance worker 的交易所账户（凭据由配置引用，不在命令行传）"
                );
                std::process::exit(2);
            }
        }
        Command::Ecosystem => run_ecosystem_smoke(),
        Command::Paper => run_paper_smoke(),
        Command::All => {
            selfcheck::run(selfcheck::Scope::Full);
        }
        Command::Verify => {
            selfcheck::run(selfcheck::Scope::KernelOnly);
        }
    }
}
