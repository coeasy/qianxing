//! CLI 帮助文本：打印的入口集合必须与 `cli.rs` 实际派发的命令名等价。
//!
//! `tools/check_architecture.py` 按"两空格缩进行的首个 token"取本文件打印的入口集合，
//! 与 `cli.rs` 里 `mode` 比较到的命令名做集合相等校验（V10 §7.2 新增不变量）：
//! 帮助里出现派发不了的入口、或存在不写进帮助的能力，都判红。
//! 约定：每条入口独占一行，行内不使用英文逗号，说明行缩进六空格。

use qx_strategy::BuiltinStrategyKind;

pub(crate) fn print_cli_help() {
    println!(
        r#"牵星 Qianxing CLI

配置与运维入口：
  init [runtime.json] [--force]
      创建本地运行时配置及可复用的样例数据/调度文件。
  init [runtime.json] --profile <base|builtin|paper|ccxt|ashare|multi-venue|backtest> [--force]
      按场景创建自包含项目；会自动改写 deploy/ 样例路径并复制依赖文件。
  init [runtime.json] --strategy <name> [--force]
      创建绑定内置策略和样例 BarFrame 的可直接回测项目。
  doctor [runtime.json] [--json]
      一次检查配置、路径、策略输入和"这份配置能否构建出运行时监督器"；不启动 worker，
      因此不判运行健康，也不连接交易所、不发送订单。
  config explain [runtime.json] [--json]
      输出有效配置摘要或机器可读配置；只显示凭据引用，不显示密钥内容。
  config validate [runtime.json]
      校验配置和所有已配置的本地文件引用。
  config fingerprint [runtime.json]
      输出不含 config_fingerprint 字段自身的稳定配置指纹。
  config lock <runtime.json> [output.json] [--force]
      生成带发布指纹锁的配置副本，不读取或输出密钥内容。
  run <backtest|paper|paper-check|doctor|live-check|runtime-check|report> [参数...]
      统一执行常用安全入口；paper 只运行本地 Paper 验收，不发送真实订单。
  status [runtime.json] [--json]
      查看本地运行配置、Worker、回测结果和安全状态；不连接交易所。
  report [runtime.json|summary.json] [--json]
      查看最新或指定回测报告；--json 输出可供脚本消费的完整摘要。
  live-check [production.runtime.json] [--json]
      执行实盘启动前静态门禁，不连接交易所、不发送订单。
  runtime-check [runtime.json] [--json]
      校验运行时拓扑并输出健康与配置指纹。

策略与回测入口（撮合内核统一为 qx-xingban）：
  strategy list
      列出内置策略。
  strategy init <strategy> [runtime.json] [bar-frame.json] [--force]
      从模板生成可直接回测的内置策略配置。
  strategy backtest <runtime.json> <bar-frame.json> [market-spec.json]
      使用统一回测引擎运行策略并保存结果产物。strategy.builtin_* 信号参数在这里生效，
      并印进 [Strategy · Signal] 行；同一份配置在 backtest builtin 上必须给出同一套信号。
  backtest [runtime.json] [bar-frame.json] [market-spec.json]
      使用统一 Rust 撮合引擎运行跨语言策略回测。
  backtest builtin <strategy> <bar-frame.json> [market-spec.json] [quantity] [--config <runtime.json>]
      使用内置策略和统一 Rust 撮合引擎回测。
      --config 读取的是策略口径（风控规则、撮合模型、成本规则、market spec 与 A 股段），
      以及 strategy.builtin_fast_window / builtin_slow_window / builtin_period /
      builtin_threshold_bps 这四个信号参数：逐项可省，写了就必须生效并在 stdout 印出 [· Signal] 行，
      非法取值整轮失败。下单数量仍由命令行 quantity 点名；配了 A 股快照就必须生效，快照非法即整轮失败。
  backtest multi-builtin <strategy> <primary-bar.json> <reference-bar.json> [primary-spec.json] [reference-spec.json] [quantity] [--funding-bps <n>] [--quantity <n>] [--root <产物目录>] [--config <runtime.json>]
      对齐两条 BarFrame，使用同一信号驱动双腿独立账户回测，并按 SpreadOrderGroup 汇总组级费用/保证金/资金费归因。
      保证金与资金费只向 market spec 声明为衍生品的腿计提；--funding-bps 非零时两条腿都必须带 spec，缺失一律先拒再跑。
      --config 的 A 股段（strategy.ashare_rules_path 等）在这里没有落点：一份策略段套不住两条腿各自的交易制度，配置了即整轮拒绝。
      strategy.builtin_* 信号参数在本链生效（两条腿共用同一份信号），生效口径印进 [Multi · Signal] 行。
      产物只有 --root 点名时落盘的 1 份 spread-attribution.json：本链不写单标的链那四份同前缀产物。
  backtest ccxt-builtin <ccxt-config> <strategy> <instrument> <start_ms> <end_ms> [timeframe] [market-spec.json] [quantity] [--config <runtime.json>]
      一次完成 CCXT OHLCV 获取、内置策略回测和结果输出。取完数据后交给 backtest builtin，A 股段与费用口径同样生效。
  backtest book --fill-tier <l1|l2> --root <产物目录> <strategy> <depth-frame.json> [market-spec.json] [quantity] [--fee-bps <n>] [--latency-snapshots <n>] [--market-impact-bps <n>] [--config <runtime.json>]
      深度档回测：--fill-tier 只认 l1 与 l2 两个值。l1 走 Tick 内核，帧里每一 snapshot 必须只有一档
      盘口（多一档即拒）；l2 走订单簿内核，按内核的 L2L3 档位吃掉帧里带的全部深度，因此没有第三个
      l3 旗标——更深档位不是另一个参数，而是 depth frame 里那些档；产物会写明实际使用的撮合内核。
      --fee-bps 优先级：显式旗标 > 运行时配置 cost_rules_path 的 taker_bp > 内核默认吃单费率。
      成本规则里的延迟设置在深度档没有落点，非零会直接报错而不是被忽略。
      --config 的 A 股段同理：盘口引擎没有 T+1、整手与涨跌停的挂钩点，配置了即整轮拒绝。
      strategy.fill_model 在这条链同样没有落点——深度档的撮合口径就是上面那三个参数，配了即整轮拒绝。
      strategy.builtin_* 信号参数在本链生效，生效口径印进 [Depth · Signal] 行。
      --latency-snapshots/--market-impact-bps 是深度撮合模型参数，缺省全 0 即逐档吃单；
      两者都会写进执行描述符与产物摘要，换参数就是换结果口径。内核的队列前置参数只作用于
      限价单，而内置策略一律发市价单，因此没有做成旗标。
  builtin-strategies
      列出 17 个内置策略及各自被哪个回测入口接受：13 个单标的策略可走 builtin / ccxt-builtin /
      book，4 个套利 kind 只被 multi-builtin 接受（book 会明确拒绝它们）。
  fast-backtest <manifest.json>
      并行执行多个独立回测任务，适合多标的、多币种和多参数批量验证。
  dataset-ingest <bar-frame.json> <dataset-id> <version> <data-dir>
      将标准化 BarFrame 增量合并到单机数据集缓存并注册 DatasetManifest。
  dataset-bundle <bundle.json> <data-dir> [bar-frame.json]
      校验并持久化 DatasetBundleManifest；提供 BarFrame 时同时校验 bars fingerprint。
  ccxt-market-spec <ccxt-config.json> <instrument> <output.json>
      拉取并冻结一份 CCXT 市场规格，供回测与实盘共用（需要 CCXT worker 依赖）。

运行时与服务入口：
  serve [runtime.json]
      启动 qx-api HTTP 服务，只读查询与受权限约束的控制命令。
  supervise [runtime.json] [--allow-unmanaged-roles]
      按配置拉起并监督 worker 子进程。
  scheduler-worker <runtime.json> <worker-id> [--once]
      运行调度 worker；--once 只推进一个 tick。
  strategy-worker <runtime.json> <worker-id> [--once]
      运行策略 worker；信号在 Rust 侧过风控后入队。
  paper-worker <runtime.json> <worker-id> [--once]
      运行 Paper 执行 worker（也承载配置中的 spread_recovery 角色）。
      strategy.ashare_rules_path 在这里会被当场拒绝：提交前的闸门没有 T+1/整手/涨跌停的落点（V11 Q65）。
  binance-worker <runtime.json> <worker-id> [--once]
      运行 Binance 行情/执行/对账 worker。
      执行与补腿角色同样当场拒绝 strategy.ashare_rules_path；行情与对账角色不受影响。
  ccxt-worker <runtime.json> <worker-id> <ccxt-config.json> [--once]
      运行 CCXT 执行 worker。
      执行与补腿角色同样当场拒绝 strategy.ashare_rules_path；行情角色不受影响。
  ccxt-fetch-ohlcv <ccxt-config.json> <instrument> <start_ms> <end_ms> <output.json> [timeframe]
      下载一段 CCXT OHLCV 并写成标准 BarFrame 文件。
  outbox-relay <data-root> <nats-url> <subject-prefix> [limit]
      把文件 outbox 中的事件转发到 NATS（需 --features nats 构建）。
  outbox-relay-postgres <runtime.json> <nats-url> <subject-prefix> [limit]
      从 PostgreSQL outbox 转发事件到 NATS（需 --features 'nats postgres' 构建）。
  outbox-relay-worker <runtime.json> <worker-id> [--once]
      以配置驱动的 outbox relay worker（需 --features nats 构建）。
  event-consumer-worker <runtime.json> <worker-id> [--once]
      事件消费 worker，带消费位点与死信队列（需 --features nats 构建）。
  consumer-dlq-replay <runtime.json> <group-id> <event-id>
      回放一条进入死信的事件（需 --features nats 构建）。
  recovery-child <queue-root> <action> <owner> <now> [token-or-lease] <command-id>
      供跨进程恢复验收器调用的最小子命令：只操作持久化控制命令队列。

实盘与订单入口（会访问交易所）：
  binance-public-probe <testnet|mainnet> [instrument]
      只读探测 Binance 公共接口，不需要凭据。
  binance-private-probe [runtime.json] [worker-id]
      只读探测 Binance 私有接口，需要配置引用的凭据。
  binance-submit-order <runtime.json> <worker-id> <command.json>
      向 Binance 提交一条命令队列中的 SubmitOrder，真实下单。
      配了 strategy.ashare_rules_path 即在任何副作用之前拒绝（V11 Q65）。
  paper-submit-order <runtime.json> <command.json>
      在 Paper venue 上提交一条 SubmitOrder，不访问交易所。
      配了 strategy.ashare_rules_path 即在任何副作用之前拒绝（V11 Q65）。
  paper-e2e [runtime.json]
      跑一次 Paper 主链路验收（注入合成行情，不接真实 feed）。
  paper-check [runtime.json]
      按 Scheduler → Strategy → Paper Execution → Ledger 验收主体链路；与 paper-e2e 一样注入
      一条固定合成 L1 报价（99/100），不接真实 feed。
  reconcile <runtime.json> [worker-id]
      用运行时配置指向的本地 EventLog 账本与配置内 Binance 账户做真实对账；
      缺少本地或远端来源时按用法错误退出，不再打印演示差异。

自校验与帮助：
  all
      完整自校验：质量门 + 真实 qx-xingban 内核算法回测 + 插件装配 + Paper venue 冒烟。
  verify
      只校验确定性内核：质量门与同输入同哈希的重放校验。
  ecosystem
      因子/协议/数据源/调度/控制面/API 路由的进程内生态冒烟，全部使用合成数据。
  paper
      Paper venue 的进程内冒烟，不写入任何账本产物。
  help
      打印本入口摘要；--help 与 -h 是同一条入口的别名。未知命令也会打印它并退出 2。

使用 `qx-cli help` 查看入口摘要；worker 与下单入口必须显式给出 runtime 配置，完整说明见 README.md 与 deploy/README.md。"#
    );
}

/// 内置策略 kind 的回测准入分区：这 4 个 kind 需要两条对齐的 BarFrame，只有
/// `backtest multi-builtin` 接受；三个单标的入口（`builtin` / `ccxt-builtin` / `book`）必须拒绝。
/// `qx-strategy` 里"缺 `reference_instrument` 即非法"是同一条事实的内核侧表述，两边一致性由
/// `src/tests/backtest_entries.rs` 的行为用例逐 kind 复现（V11 Q0b）。
pub(crate) const MULTI_LEG_KINDS: [BuiltinStrategyKind; 4] = [
    BuiltinStrategyKind::PairsArbitrage,
    BuiltinStrategyKind::BasisArbitrage,
    BuiltinStrategyKind::CrossVenueArbitrage,
    BuiltinStrategyKind::SpotFuturesArbitrage,
];

/// 某个 kind 实际能被哪个回测入口接受 —— `builtin-strategies` 的输出与帮助文本共用这一处，
/// 免得帮助再写成"17 个策略都能用于回测/Paper/策略接入"。
pub(crate) fn backtest_entry_of(kind: BuiltinStrategyKind) -> &'static str {
    if MULTI_LEG_KINDS.contains(&kind) {
        "multi-builtin"
    } else {
        "builtin / ccxt-builtin / book"
    }
}

/// `builtin-strategies` 与 `strategy list` 的三列输出：名称、说明、真正接受它的回测入口。
pub(crate) fn print_builtin_strategies() {
    for kind in BuiltinStrategyKind::ALL {
        println!(
            "{}\t{}\t[{}]",
            kind.name(),
            kind.description(),
            backtest_entry_of(kind)
        );
    }
}
