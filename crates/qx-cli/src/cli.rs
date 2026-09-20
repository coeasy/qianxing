//! CLI 对外表面：命令注册表、帮助文本与 `run <子命令>` 统一入口。
//!
//! 命令表是这个二进制唯一的派发真相：`main()` 只查表，帮助文本只从表生成。
//! 加一条命令因此只剩两步——在所属模块写 `xxx_command(argv: &[String])`，
//! 再在 `qx_commands!` 里加一行；不需要碰 `main()`，也不会漏掉帮助。

use super::*;

/// 一条命令对外暴露的全部事实：叫什么、怎么用、归哪类、由谁执行。
pub(crate) struct CommandSpec {
    pub(crate) name: &'static str,
    /// 同一命令的其他拼法（如 `--help`、`-h`），参与派发但不重复出现在帮助里。
    pub(crate) aliases: &'static [&'static str],
    /// 帮助文本分组标题；表中相邻同类命令会被折叠成一节。
    pub(crate) category: &'static str,
    pub(crate) usage: &'static str,
    pub(crate) summary: &'static str,
    /// 同一命令值得单列的用法变体，只影响帮助文本。
    pub(crate) variants: &'static [(&'static str, &'static str)],
    /// 支持 `--json` 的命令在管道里被脚本消费，不能再打印横幅与装饰文本。
    pub(crate) machine_readable: bool,
    pub(crate) run: fn(&[String]),
}

/// 声明式命令表：`group` 给分组，`command` 给条目，展开成一张 `COMMANDS`。
macro_rules! qx_commands {
    (
        $(group $category:literal {
            $(command $name:literal {
                usage: $usage:literal,
                summary: $summary:literal,
                aliases: [$($alias:literal),*],
                variants: [$(($variant_usage:literal, $variant_summary:literal)),*],
                json: $json:literal,
                run: $run:path,
            })*
        })*
    ) => {
        pub(crate) const COMMANDS: &[CommandSpec] = &[$(
            $(CommandSpec {
                name: $name,
                category: $category,
                usage: $usage,
                summary: $summary,
                aliases: &[$($alias),*],
                variants: &[$(($variant_usage, $variant_summary)),*],
                machine_readable: $json,
                run: $run,
            },)*
        )*];
    };
}

qx_commands! {
    group "常用入口" {
        command "init" {
            usage: "init [runtime.json] [--force]",
            summary: "创建本地运行时配置及可复用的样例数据/调度文件。",
            aliases: [],
            variants: [
                ("init [runtime.json] --profile <base|paper|ccxt|ashare|multi-venue|backtest> [--force]", "按场景创建自包含项目；会自动改写 deploy/ 样例路径并复制依赖文件。"),
                ("init [runtime.json] --strategy <name> [--force]", "创建绑定内置策略和样例 BarFrame 的可直接回测项目。")
            ],
            json: false,
            run: init_command,
        }
        command "doctor" {
            usage: "doctor [runtime.json] [--json]",
            summary: "一次检查配置、路径、策略输入和运行拓扑；不连接交易所、不发送订单。",
            aliases: [],
            variants: [],
            json: true,
            run: doctor_command,
        }
        command "config" {
            usage: "config validate [runtime.json]",
            summary: "校验配置和所有已配置的本地文件引用。",
            aliases: [],
            variants: [
                ("config explain [runtime.json] [--json]", "输出有效配置摘要或机器可读配置；只显示凭据引用，不显示密钥内容。"),
                ("config fingerprint [runtime.json]", "输出不含 config_fingerprint 字段自身的稳定配置指纹。"),
                ("config lock <runtime.json> [output.json] [--force]", "生成带发布指纹锁的配置副本，不读取或输出密钥内容。")
            ],
            json: true,
            run: config_command,
        }
        command "run" {
            usage: "run <backtest|paper|doctor|live-check|runtime-check|report> [参数...]",
            summary: "统一执行常用安全入口；paper 只运行本地 Paper 验收，不发送真实订单。",
            aliases: [],
            variants: [],
            json: false,
            run: run_command,
        }
        command "status" {
            usage: "status [runtime.json] [--json]",
            summary: "查看本地运行配置、Worker、回测结果和安全状态；不连接交易所。",
            aliases: [],
            variants: [],
            json: true,
            run: status_command,
        }
        command "report" {
            usage: "report [runtime.json|summary.json] [--json]",
            summary: "查看最新或指定回测报告；--json 输出可供脚本消费的完整摘要。",
            aliases: [],
            variants: [],
            json: true,
            run: report_command,
        }
        command "strategy" {
            usage: "strategy list",
            summary: "列出内置策略。",
            aliases: [],
            variants: [
                ("strategy init <strategy> [runtime.json] [bar-frame.json] [--force]", "从模板生成可直接回测的内置策略配置。"),
                ("strategy backtest <runtime.json> <bar-frame.json> [market-spec.json]", "使用统一回测引擎运行策略并保存结果产物。")
            ],
            json: false,
            run: strategy_command,
        }
        command "backtest" {
            usage: "backtest [runtime.json] [bar-frame.json] [market-spec.json]",
            summary: "使用统一 Rust 撮合引擎运行跨语言策略回测。",
            aliases: [],
            variants: [],
            json: false,
            run: backtest_command,
        }
        command "builtin-strategies" {
            usage: "builtin-strategies",
            summary: "列出可直接用于回测/Paper/策略接入的 17 个内置策略。",
            aliases: [],
            variants: [],
            json: false,
            run: builtin_strategies_command,
        }
        command "builtin-backtest" {
            usage: "builtin-backtest <strategy> <bar-frame.json> [market-spec.json] [quantity] [--costs costs.json]",
            summary: "使用内置策略和统一 Rust 撮合引擎回测。",
            aliases: [],
            variants: [],
            json: false,
            run: builtin_backtest_command,
        }
        command "multi-builtin-backtest" {
            usage: "multi-builtin-backtest <strategy> <primary-bar.json> <reference-bar.json> [primary-spec.json] [reference-spec.json] [quantity] [--costs costs.json]",
            summary: "对齐两条 BarFrame，使用同一信号驱动双腿独立账户回测。",
            aliases: [],
            variants: [],
            json: false,
            run: multi_builtin_backtest_command,
        }
        command "fast-backtest" {
            usage: "fast-backtest <manifest.json>",
            summary: "并行执行多个独立回测任务，适合多标的、多币种和多参数批量验证。",
            aliases: [],
            variants: [],
            json: false,
            run: fast_backtest_command,
        }
        command "dataset-ingest" {
            usage: "dataset-ingest <bar-frame.json> <dataset-id> <version> <data-dir>",
            summary: "将标准化 BarFrame 增量合并到单机数据集缓存并注册 DatasetManifest。",
            aliases: [],
            variants: [],
            json: false,
            run: dataset_ingest_command,
        }
        command "dataset-bundle" {
            usage: "dataset-bundle <bundle.json> <data-dir> [bar-frame.json]",
            summary: "校验并持久化 DatasetBundleManifest；提供 BarFrame 时同时校验 bars fingerprint。",
            aliases: [],
            variants: [],
            json: false,
            run: dataset_bundle_command,
        }
        command "ccxt-builtin-backtest" {
            usage: "ccxt-builtin-backtest <ccxt-config> <strategy> <instrument> <start_ms> <end_ms> [timeframe] [market-spec.json] [quantity] [--costs costs.json]",
            summary: "一次完成 CCXT OHLCV 获取、内置策略回测和结果输出。",
            aliases: [],
            variants: [],
            json: false,
            run: ccxt_builtin_backtest_command,
        }
        command "paper-check" {
            usage: "paper-check [runtime.json]",
            summary: "按 Scheduler → Strategy → Paper Execution → Ledger 验收主体链路。",
            aliases: [],
            variants: [],
            json: false,
            run: paper_pipeline_command,
        }
        command "live-check" {
            usage: "live-check [production.runtime.json] [--json]",
            summary: "执行实盘启动前静态门禁，不连接交易所、不发送订单。",
            aliases: [],
            variants: [],
            json: true,
            run: live_check_command,
        }
        command "runtime-check" {
            usage: "runtime-check [runtime.json] [--json]",
            summary: "校验运行时拓扑并输出健康与配置指纹。",
            aliases: [],
            variants: [],
            json: true,
            run: runtime_check_command,
        }
    }

    group "运行与监督入口" {
        command "serve" {
            usage: "serve [runtime.json]",
            summary: "启动本地运行时 API，提供只读状态查询与受控命令提交入口。",
            aliases: [],
            variants: [],
            json: false,
            run: serve_command,
        }
        command "supervise" {
            usage: "supervise [runtime.json] [--allow-unmanaged-roles]",
            summary: "按配置拉起并监督 worker 进程，异常退出后重启并移交租约。",
            aliases: [],
            variants: [],
            json: false,
            run: supervise_command,
        }
        command "scheduler-worker" {
            usage: "scheduler-worker <runtime.json> <worker-id> [--once]",
            summary: "调度器 worker：把到期的调度窗口投递成作业队列任务。",
            aliases: [],
            variants: [],
            json: false,
            run: scheduler_worker_command,
        }
        command "strategy-worker" {
            usage: "strategy-worker <runtime.json> <worker-id> [--once]",
            summary: "策略 worker：领取作业、调用策略并落地意图与信号产物。",
            aliases: [],
            variants: [],
            json: false,
            run: strategy_worker_command,
        }
        command "paper-worker" {
            usage: "paper-worker <runtime.json> <worker-id> [--once]",
            summary: "Paper 执行 worker；配置为 spread_recovery 角色时承载价差单恢复。",
            aliases: [],
            variants: [],
            json: false,
            run: paper_worker_command,
        }
        command "binance-worker" {
            usage: "binance-worker [runtime.json] <worker-id> [--once]",
            summary: "Binance 执行 worker：行情桥接、下单与回执落账；需要实盘凭据。",
            aliases: [],
            variants: [],
            json: false,
            run: binance_worker_command,
        }
        command "ccxt-worker" {
            usage: "ccxt-worker [runtime.json] <worker-id> <ccxt-config.json> [--once]",
            summary: "CCXT 执行 worker：适配 CCXT 交易所的行情与执行回路。",
            aliases: [],
            variants: [],
            json: false,
            run: ccxt_worker_command,
        }
        command "recovery-child" {
            usage: "recovery-child <queue-root> <claim|takeover|ack|stale-ack> <owner> <now_ms> [lease_seconds|fencing_token] <command_id>",
            summary: "控制队列跨进程恢复验证的子进程入口，由恢复契约测试拉起。",
            aliases: [],
            variants: [],
            json: false,
            run: recovery_child_command,
        }
    }

    group "行情与回测入口" {
        command "strategy-backtest" {
            usage: "strategy-backtest <runtime.json> <bar-frame.json> [market-spec.json]",
            summary: "按运行时配置里的策略执行统一回测并保存产物。",
            aliases: [],
            variants: [],
            json: false,
            run: strategy_backtest_command,
        }
        command "ccxt-backtest" {
            usage: "ccxt-backtest <bar-frame.json> [fast] [slow] [market-spec.json] [--costs costs.json]",
            summary: "对已取回的 CCXT BarFrame 跑均线策略回测。",
            aliases: [],
            variants: [],
            json: false,
            run: ccxt_backtest_command,
        }
        command "ccxt-fetch-ohlcv" {
            usage: "ccxt-fetch-ohlcv <ccxt-config.json> <instrument> <start_ms> <end_ms> <output.json> [timeframe]",
            summary: "拉取 CCXT OHLCV 并写出标准化 BarFrame，供回测与数据集入库复用。",
            aliases: [],
            variants: [],
            json: false,
            run: ccxt_fetch_ohlcv_command,
        }
        command "ccxt-market-spec" {
            usage: "ccxt-market-spec <ccxt-config.json> <instrument> <output.json>",
            summary: "把 CCXT market 元数据导出为交易标的规格，含保证金规则。",
            aliases: [],
            variants: [],
            json: false,
            run: ccxt_market_spec_command,
        }
        command "binance-public-probe" {
            usage: "binance-public-probe [testnet|mainnet] [instrument]",
            summary: "只读探测 Binance 公共行情与交易规则，不读取凭据。",
            aliases: [],
            variants: [],
            json: false,
            run: binance_public_probe_command,
        }
        command "binance-private-probe" {
            usage: "binance-private-probe [production.runtime.json] [worker-id]",
            summary: "只读探测账户、持仓与余额；需要凭据引用可用，不下单。",
            aliases: [],
            variants: [],
            json: false,
            run: binance_private_probe_command,
        }
    }

    group "下单、Paper 与对账入口" {
        command "paper-e2e" {
            usage: "paper-e2e [paper.runtime.json]",
            summary: "跑一次 Paper 主链路端到端验收，不发送真实订单。",
            aliases: [],
            variants: [],
            json: false,
            run: paper_pipeline_command,
        }
        command "paper-submit-order" {
            usage: "paper-submit-order [runtime.json] <command.json>",
            summary: "把一条 SubmitOrder 命令按 Paper 撮合与计费规则执行并记账。",
            aliases: [],
            variants: [],
            json: false,
            run: paper_submit_order_command,
        }
        command "binance-submit-order" {
            usage: "binance-submit-order [production.runtime.json] <worker-id> <command.json>",
            summary: "经风险门与执行规格向 Binance 提交一条真实订单命令。",
            aliases: [],
            variants: [],
            json: false,
            run: binance_submit_order_command,
        }
        command "reconcile" {
            usage: "reconcile [runtime.json] [worker-id]",
            summary: "有配置时按 worker 拉取 venue 事实做对账，无配置时演示订单差异比对。",
            aliases: [],
            variants: [],
            json: false,
            run: reconcile_command,
        }
    }

    group "消息与死信入口" {
        command "outbox-relay" {
            usage: "outbox-relay <data-root> <nats-url> <subject-prefix> [limit]",
            summary: "把文件 Outbox 中未投递的事件中继到 NATS，需 --features nats。",
            aliases: [],
            variants: [],
            json: false,
            run: outbox_relay_command,
        }
        command "outbox-relay-postgres" {
            usage: "outbox-relay-postgres <runtime.json> <nats-url> <subject-prefix> [limit]",
            summary: "把 PostgreSQL Outbox 表中的事件中继到 NATS，需 --features nats postgres。",
            aliases: [],
            variants: [],
            json: false,
            run: outbox_relay_postgres_command,
        }
        command "outbox-relay-worker" {
            usage: "outbox-relay-worker [runtime.json] <worker-id> [--once]",
            summary: "常驻中继 worker，按配置的投递节拍持续搬运 Outbox 事件。",
            aliases: [],
            variants: [],
            json: false,
            run: outbox_relay_worker_command,
        }
        command "event-consumer-worker" {
            usage: "event-consumer-worker [runtime.json] <worker-id> [--once]",
            summary: "消费 NATS 事件流并按消费组落状态，重复投递幂等跳过。",
            aliases: [],
            variants: [],
            json: false,
            run: event_consumer_worker_command,
        }
        command "consumer-dlq-replay" {
            usage: "consumer-dlq-replay <runtime.json> <group-id> <event-id>",
            summary: "把死信队列里的单条事件重新投递回消费链路。",
            aliases: [],
            variants: [],
            json: false,
            run: consumer_dlq_replay_command,
        }
    }

    group "自校验与演示入口" {
        command "all" {
            usage: "all（或直接不带参数运行）",
            summary: "完整确定性演示链路：质量门 → 双次回测 → 重放校验 → 插件装配 → Paper 冒烟。",
            aliases: [],
            variants: [],
            json: false,
            run: demo_command,
        }
        command "verify" {
            usage: "verify",
            summary: "跑到重放哈希校验为止并断言通过，用作 CI 确定性门禁。",
            aliases: [],
            variants: [],
            json: false,
            run: verify_command,
        }
        command "ecosystem" {
            usage: "ecosystem",
            summary: "跨内核、撮合、风控与账本的生态联动冒烟演示。",
            aliases: [],
            variants: [],
            json: false,
            run: ecosystem_command,
        }
        command "paper" {
            usage: "paper",
            summary: "Paper 撮合与真实计费的冒烟演示。",
            aliases: [],
            variants: [],
            json: false,
            run: paper_command,
        }
        command "help" {
            usage: "help",
            summary: "按分组列出全部命令与用法。",
            aliases: ["--help", "-h"],
            variants: [],
            json: false,
            run: help_command,
        }
    }
}

/// 按命令名或别名查表；`main()` 的派发和横幅判断共用这一份真相。
pub(crate) fn find_command(invoked: &str) -> Option<&'static CommandSpec> {
    COMMANDS
        .iter()
        .find(|command| command.name == invoked || command.aliases.contains(&invoked))
}

/// 输出交给脚本消费的命令跳过横幅，保证 `--json` 的 stdout 是纯 JSON。
/// `run doctor --json` 这类统一入口同样算机器输出。
pub(crate) fn machine_output(argv: &[String]) -> bool {
    if !argv.iter().any(|argument| argument == "--json") {
        return false;
    }
    match argv.get(1).map(String::as_str) {
        Some("run") => matches!(
            argv.get(2).map(String::as_str),
            Some("doctor") | Some("live-check") | Some("runtime-check") | Some("report")
        ),
        Some(invoked) => find_command(invoked).is_some_and(|command| command.machine_readable),
        None => false,
    }
}

pub(crate) fn print_cli_help() {
    println!("牵星 Qianxing CLI\n");
    let mut category = "";
    let mut first_group = true;
    for command in COMMANDS {
        if command.category != category {
            category = command.category;
            if !first_group {
                println!();
            }
            first_group = false;
            println!("{category}：");
        }
        println!("  {}", command.usage);
        println!("      {}", command.summary);
        for (usage, summary) in command.variants {
            println!("  {usage}");
            println!("      {summary}");
        }
    }
    println!(
        "\n使用 `qianxing help` 查看入口摘要；既有入口参数保持兼容，完整说明见 README.md 与 deploy/README.md。\n未识别的命令会以 2 号退出码失败，不再静默落到演示链路。"
    );
}

pub(crate) fn run_unified_command(arguments: &[String]) -> Result<(), String> {
    let action = arguments.first().map(String::as_str).ok_or_else(|| {
        "run 需要 backtest、paper、doctor、live-check 或 runtime-check".to_string()
    })?;
    match action {
        "backtest" => {
            let runtime = arguments.get(1).map(PathBuf::from);
            let frame = arguments.get(2).map(PathBuf::from);
            let spec = arguments.get(3).map(PathBuf::from);
            run_unified_backtest(runtime.as_deref(), frame.as_deref(), spec.as_deref())
        }
        "paper" | "paper-check" => {
            let path = arguments.get(1).map(PathBuf::from).unwrap_or_else(|| {
                repository_deploy_path("qianxing.runtime.paper-strategy.example.json")
            });
            run_paper_pipeline_once(&path)
        }
        "doctor" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(default_runtime_path);
            run_doctor(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        "live-check" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    repository_deploy_path("qianxing.runtime.production.example.json")
                });
            run_live_check(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        "runtime-check" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(default_runtime_path);
            run_runtime_check(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        "report" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(default_runtime_path);
            run_report(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        _ => Err(format!(
            "run 不支持 {action}；可用入口：backtest、paper、doctor、live-check、runtime-check、report"
        )),
    }
}

pub(crate) fn run_unified_backtest(
    runtime: Option<&Path>,
    frame: Option<&Path>,
    spec: Option<&Path>,
) -> Result<(), String> {
    let default_runtime = repository_deploy_path("qianxing.runtime.strategy-backtest.example.json");
    let default_frame = repository_deploy_path("qianxing.bar-frame.example.json");
    run_strategy_backtest(
        runtime.unwrap_or(&default_runtime),
        frame.unwrap_or(&default_frame),
        spec,
    )
}

pub(crate) fn help_command(_argv: &[String]) {
    print_cli_help();
}

pub(crate) fn run_command(argv: &[String]) {
    let arguments = argv.iter().skip(2).cloned().collect::<Vec<_>>();
    if let Err(error) = run_unified_command(&arguments) {
        eprintln!("统一运行入口失败: {error}");
        std::process::exit(2);
    }
}
