# 牵星运行时部署说明

`qianxing.runtime.example.json` 是 paper/testnet 的拓扑模板，不包含 API key、secret、私钥或任何账户余额。

`qianxing.runtime.production.example.json` 是后续分布式生产拓扑模板，显式使用 `profile: "distributed"`、PostgreSQL 和 mTLS；其中 `/run/secrets` 和 `/var/lib` 只是部署约定，必须由实际的秘密管理器和持久卷提供。本阶段默认不启用该 profile。

本阶段推荐 `profile: "single_node"`（省略时默认）：运行时使用 SQLite 或 Files，研究/回测使用 Files，不依赖 PostgreSQL/NATS。`single_node` 会在配置校验阶段拒绝 PostgreSQL、NATS、OutboxRelay 和 EventConsumer，避免编译了可选 feature 后误把分布式组件带入单机部署。需要使用 PostgreSQL/NATS 时，必须显式声明 `profile: "distributed"`。

`storage.consistency` 是必须与拓扑匹配的显式声明：单机 Files/SQLite 使用
`local_durable`；PostgreSQL 事务事实使用 `transactional`；启用 NATS Outbox Relay
使用 `distributed_outbox`。配置指纹、`runtime-check` 和 `status --json` 都会包含该字段。
它描述的是事实持久化与 Outbox 发布边界，不代表跨系统分布式事务；真实集群仍需故障演练。

生产配置支持 `config_fingerprint` 发布锁。指纹计算会排除该字段自身；策略、worker、存储、凭据引用或 API 参数被修改后，API/worker 启动会拒绝加载。`runtime-check` 会输出当前指纹和 `locked=true/false`。

## 启动前检查

```powershell
cargo run --release -p qx-cli -- help
cargo run --release -p qx-cli -- live-check deploy/qianxing.runtime.production.example.json
cargo run --release -p qx-cli -- runtime-check deploy/qianxing.runtime.example.json
cargo run --release -p qx-cli -- runtime-check deploy/qianxing.runtime.production.example.json
# CI/部署平台可直接消费 JSON；失败时退出码非零
cargo run --release -p qx-cli -- runtime-check deploy/qianxing.runtime.example.json --json
cargo run --release -p qx-cli -- live-check deploy/qianxing.runtime.production.json --json
cargo run --release -p qx-cli -- binance-public-probe testnet BTCUSDT.BINANCE
cargo run --release -p qx-cli -- binance-private-probe deploy/qianxing.runtime.production.example.json binance-execution-main
```

`live-check` 是不连接交易所、不发送订单的生产发布前静态门禁。它会额外检查 production 环境、配置指纹锁、TLS 文件、凭据来源、Execution 品种规格与名义额上限、研究快照文件；模板中的占位路径或 `config_fingerprint: null` 会按预期失败，必须替换为部署机上的真实发布配置后再通过。

`runtime-check --json` 输出版本化运行时诊断（健康快照、配置指纹、引用 warnings/failures 和安全边界字段），
适合 CI、启动脚本和部署平台采集；它不会连接交易所或发送订单。`run runtime-check --json` 是等价的统一入口。

`live-check --json` 输出实盘发布前的逐项环境、TLS、凭据、产品规格、风险限额和研究快照检查，
同样不会连接交易所或发送订单；`run live-check --json` 是等价的统一入口。

公共 CCXT 的私有 worker 可以只在 `endpoint` 指向的 CCXT JSON 中配置
`credential_env.api_key`、`credential_env.secret`（以及交易所需要的 `password`），
RuntimeConfig 无需重复配置。`doctor` 会检查 endpoint 的 `exchange_id` 是否与 worker 的
`venue_id` 一致，并提示 CCXT 凭据环境变量；生产 `live-check` 会把该来源纳入凭据门禁。

本地开发推荐使用统一入口：

```powershell
cargo run --release -p qx-cli -- init qianxing.runtime.json
cargo run --release -p qx-cli -- init qianxing.runtime.json --strategy macd
cargo run --release -p qx-cli -- init qianxing.runtime.ccxt.json --profile ccxt
cargo run --release -p qx-cli -- init qianxing.runtime.ashare.json --profile ashare
cargo run --release -p qx-cli -- doctor qianxing.runtime.json
cargo run --release -p qx-cli -- config explain qianxing.runtime.json
cargo run --release -p qx-cli -- config validate qianxing.runtime.json
cargo run --release -p qx-cli -- config fingerprint qianxing.runtime.json
cargo run --release -p qx-cli -- config lock qianxing.runtime.json qianxing.runtime.locked.json
cargo run --release -p qx-cli -- backtest
cargo run --release -p qx-cli -- run paper
cargo run --release -p qx-cli -- backtest
cargo run --release -p qx-cli -- paper-check deploy/qianxing.runtime.paper-strategy.example.json
```

`doctor` 会一次检查运行时配置、策略输入、数据 Bundle、公司行为/日历文件、存储目录和运行拓扑；
它不会连接交易所或发送订单。`config explain --json` 输出经过校验的有效配置，只有凭据引用名称/路径，
不会读取或打印 key、secret 内容。

`init` 会将运行时配置所需的调度样例、BarFrame、DatasetBundle、品种规格和 Paper 目标复制到同一目录，
避免“配置本身合法但引用样例文件不存在”。`--strategy macd` 可直接生成绑定内置策略的本地回测项目。

DatasetBundle 的非行情组件默认使用 JSON；Arrow 组件需要在 Bundle 中声明 `"format": "arrow"`，
并把策略路径指向带 schema、行数和 fingerprint 的 Arrow Dataset manifest。参考
`deploy/qianxing.dataset-component.arrow.example.json`。

配置检查会拒绝：

- production 环境使用明文 API；
- mTLS 没有服务端证书、私钥、客户端 CA 或 Operator 证书映射；
- worker id 重复，或用户流/对账 worker 缺少账户与 Venue；
- 行情/用户流 worker 缺少 endpoint；
- SQLite 后端缺少数据库路径；
- PostgreSQL 后端缺少 `postgres_dsn_env`；
- 没有且仅有一个 API worker。

`binance-public-probe` 只访问 Binance 公共 `bookTicker`，不读取凭据、不下单，用于验证 Testnet 网络连通性和 REST 字段解析；私有交易验收仍必须使用单独的签名、用户流和对账矩阵。

`binance-private-probe` 只调用签名账户余额接口，不下单，凭据从 runtime worker 的 `credential_env` 或 `credential_files` 读取，输出不包含余额数值；没有凭据时应明确失败，不能退化为匿名请求。

## Paper API

```powershell
cargo run --release -p qx-cli -- serve deploy/qianxing.runtime.example.json
```

`serve` 会先校验配置，再启动 API worker；worker 异常、panic 和正常退出都会写入运行时健康状态。生产环境必须使用 `transport: "mtls"`，并通过部署系统把证书路径映射到只读文件或秘密管理器。mTLS 服务端会使用 `TlsPemReloader` 每秒轮询证书链、私钥和客户端 CA，并同步轮询 Operator 证书映射；新连接使用成功解析的新配置，已有连接继续使用握手时配置，解析失败保留旧配置。替换 Operator PEM 只更新已在运行时配置中声明的 operator 身份，不会仅靠替换 CA 自动授予新权限；私钥不会写入事件日志或配置摘要。

`serve` 启动时会恢复控制面状态；`storage.backend: "files"` 使用 `control-plane.json` 与 `control-queue/`，`storage.backend: "sqlite"` 使用 `sqlite_path` 中的事务表，`storage.backend: "postgres"` 使用 `postgres_dsn_env` 指向的 DSN 并自动执行幂等迁移。PostgreSQL 后端的控制面、控制命令队列和 JobQueue 使用事务、advisory lock、租约与 fencing token；带密码的 DSN 不得写入 JSON。控制命令先原子持久化再入队，队列写入失败不会丢失 Accepted 命令；Execution worker 启动后会扫描 pending 命令补队列。

## Binance worker

用户流、执行和对账 worker 支持两种互斥凭据来源：`credential_env` 环境变量，或由 Secret Manager/CSI/容器 secrets 原子投影的 `credential_files` 文件。凭据值不会进入配置 JSON、运行时健康详情或日志；用户流新建连接、执行新订单和对账新轮次会重新读取文件，已有连接继续使用当前认证上下文。先校验拓扑，再单独启动 worker：

```powershell
$env:QX_BINANCE_API_KEY = "<api-key>"
$env:QX_BINANCE_API_SECRET = "<api-secret>"
cargo run --release -p qx-cli -- binance-worker deploy/qianxing.runtime.production.example.json binance-user-main
```

生产用户流/对账 worker 使用 production 模板中的 `QX_BINANCE_API_KEY` 和 `QX_BINANCE_API_SECRET`。缺少环境变量会在建立网络连接前失败；真实账户必须先完成小额或模拟账户验收。

文件投影配置示例（将 worker 中的 `credential_env` 替换为以下字段，二者不能同时存在）：

```json
"credential_files": {
  "api_key": "/run/secrets/qianxing/binance-api-key",
  "secret": "/run/secrets/qianxing/binance-api-secret"
}
```

## 本机进程托管

`start-qianxing.ps1` 会先执行 `runtime-check`，再为 API、Scheduler、Strategy 和已配置的 Binance market/user/execution/reconcile worker 启动隐藏子进程，按 worker 分离 stdout/stderr 日志，并在任一子进程异常退出时停止其余子进程：

```powershell
cargo build --release -p qx-cli --features sqlite
./deploy/start-qianxing.ps1 -Config ./deploy/qianxing.runtime.production.example.json
```

同一套 fail-fast 监督器也由 `qx-cli supervise` 提供，Linux/macOS 可直接使用：

```bash
cargo build --release -p qx-cli --features sqlite
./deploy/start-qianxing.sh ./deploy/qianxing.runtime.production.example.json
```

监督器会先执行运行时拓扑校验，为每个 worker 分离 `process-logs/<worker>.out.log`/
`err.log`，并在任一受管 worker 退出时停止其余 worker；不认识的 Venue 必须由外部
进程托管，只有明确传入 `--allow-unmanaged-roles` 才会跳过该角色。它不自动重启交易
worker，避免把未知网络结果误当成可安全重放；恢复依赖控制命令幂等、租约 fencing 和
EventLog 对账。

Scheduler worker 从 `scheduler.jobs_path` 装载 JobSpec，恢复 `scheduler.state_path`，按 UTC Cron 触发并写入带租约/fencing 的 JobQueue；Strategy worker 管理策略生命周期，执行 `Signal→Portfolio→RiskGate→OrderIntent`，并把通过风控的订单转成带审计的 SubmitOrder 命令交给 Execution worker。它不会绕过 OMS/Risk 直接调用 Venue。未知角色仍会被脚本拒绝；`-AllowUnmanagedRoles` 只适合外部扩展进程接管未知角色。

Strategy 可以通过 `strategy.research_snapshot_path` 加载包含 CandidateBinding、FeatureArtifact、FactorReport、PIT 时间和数据血缘的研究快照；生产中已绑定交易对象的策略必须同时设置 `research_snapshot_required=true`、`research_data_fingerprint` 和 `dataset_bundle_path`，运行时还要求快照指纹匹配并绑定已验证的数据清单。回测/纸面配置仍兼容 `target_snapshot_path` 和 `target_qty`，但不应将裸目标仓位作为实盘发布物。

Paper Execution worker 可以配置 `paper_initial_cash_raw`，启动时通过幂等 `AccountCashflow(Transfer)` 写入结算币初始资金；资金进入同一 EventLog/Ledger，重启不会重复入金。该字段只能用于 `venue_id=paper`，金额使用核心定点 raw 单位。

## 公共 CCXT 多交易所连接层

交易所连接优先使用 Python 公共 `ccxt`，配置 `exchange_id` 即可复用 Binance、OKX、Bybit 等交易所的统一 REST API。连接层入口为 `python/qianxing_ccxt`，负责 market/symbol 映射、OHLCV 分页、ticker、账户、订单和错误分类；`python -m qianxing_ccxt.worker --config <json>` 提供 JSONL 进程边界；核心 Rust 订单状态、Ledger 和回测撮合不直接依赖 CCXT。`qianxing.ccxt.binance.public.example.json` 提供无凭据公共探测样例，`credential_env: null` 也会被正确解释为匿名公共连接。

安装 Python wheel 时会安装公共 `ccxt`；本期运行时只依赖 REST 轮询、下单和对账，不依赖 CCXT Pro。`ccxt-pro` extra 与 `watch_*` 封装仅作为后续实时流扩展保留，当前不能把它们作为生产前置条件，也不能把 REST 轮询伪装成 WebSocket 用户流。当前 CCXT REST 连接层、MarketData ticker/OHLCV Worker、Execution SubmitOrder Worker、订单/余额/持仓/资金费率/资金流水 Reconcile、MarketSpec 快照、研究快照 StrategyContext、API QueryPort 和跨进程租约恢复验收已接入；现货和永续 ticker 在 bid/ask 缺失时会使用订单簿首档完成统一标准化。交易所账单字段差异和真实多交易所 sandbox 闭环仍需外部凭证与交易所环境验收，详见 [CCXT 多交易所方案](../docs/CCXT多交易所接入与策略运行方案-V1.md)。

可直接复制 `qianxing.runtime.ccxt.example.json` 作为多交易所 sandbox 拓扑样例；执行和行情 Worker 的 `endpoint` 指向 CCXT 配置文件，supervisor 会优先启动公共 CCXT 路径，旧 Binance Worker 仅作为无 CCXT endpoint 时的兼容回退。

该样例同时展示 `strategies[]` 多策略配置。每个策略实例的 `id` 必须等于对应 Strategy worker id；调度任务的 `owner` 必须填写该策略实例，多个策略共用 JobQueue 时不会互相领取任务。

双腿套利可使用 `backtest multi-builtin <strategy> <primary-bar.json> <reference-bar.json> [primary-spec.json] [reference-spec.json] [quantity]`。两条 BarFrame 必须时间戳对齐；信号由同一个套利策略生成，再分别通过统一撮合、手续费、风控和 Ledger 回测，适用于跨交易所价差与现货/期货基差策略。示例输入为 `qianxing.bar-frame.example.json` 与 `qianxing.bar-frame.okx.example.json`。

多标的、多币种批量回测使用 `fast-backtest manifest.json`。manifest 的 `jobs[]` 每项配置一个独立 `runtime`、`bars` 和可选 `market_spec`，CLI 会并行运行多个隔离账户/标的任务，适合同时比较 BTC、ETH、SOL，现货、永续、期货以及不同策略参数。示例见 `qianxing.fast-backtest.example.json`；每个 runtime 可以继续使用 `strategies[]` 配置多策略实例。

策略实例还可配置 `python_module`（Python 模块名或 `.py` 文件路径）。Rust 会通过 `python -m qianxing_strategy.worker` 传递版本化 JSONL 输入/输出，校验请求身份、PIT 时间、数据指纹和信号有效期；Python 策略不能直接访问交易所、EventLog 或 Ledger，也不能绕过 Rust RiskGate。未配置该字段时使用现有 Rust 策略兼容路径。

策略开发同时支持 Rust `qx-strategy`、Python `on_event`/持久 worker 和 C++ `cpp/include/qianxing_strategy.h` C ABI；三者统一返回 `StrategyDecision.intents[]`，旧 `target_qty` 仍兼容。

Rust/C++ 也可以编译成独立策略进程，通过 `strategy.external_executable`、`external_args` 和 `external_env` 接入。独立进程每行读取一个 `StrategyContractInput`，每行输出一个 `{"ok":true,"output":...}` JSON；标准输出只允许协议内容，日志写标准错误。该入口与 Python 共用超时、崩溃隔离、RiskGate、OMS、执行队列和审计链路，示例见 `cpp/examples/jsonl_strategy.cpp`。

策略子进程不会继承父进程的交易凭证环境；`external_env` 仅允许非敏感业务参数，包含 `SECRET`、`TOKEN`、`PASSWORD`、`API_KEY`、`PRIVATE_KEY` 或 `CREDENTIAL` 的变量名会被拒绝。

同一套 Python/C++/Rust JSONL 策略也可以直接进入 Bar 回测：
`cargo run -p qx-cli -- backtest deploy/qianxing.runtime.strategy-backtest.example.json deploy/qianxing.bar-frame.example.json [market-spec.json]`。回测传入的 `bars` 只包含当前撮合 Bar 之前的数据，成交仍由 Rust `BacktestEngine` 统一处理。策略配置可将 `transport` 设为 `framed_json`，使用带版本、序号、长度上限和 CRC32 的二进制分帧；也可设为 `shared_memory_json` 或 `shared_memory_columnar` 使用双向 SPSC mmap ring；对应示例为 `deploy/qianxing.runtime.strategy-framed.example.json`、`deploy/qianxing.runtime.strategy-shared.example.json` 和 `deploy/qianxing.runtime.strategy-columnar.example.json`。

回测策略还可通过 `strategy.dataset_bundle_path` 绑定冻结的数据 Bundle。配置后启动门禁会校验 Bundle 的 `bars` fingerprint/行数，以及已支持的 `corporate_actions`、`calendar` 文件内容 fingerprint/行数，避免策略实际使用的研究组件与清单不一致；可运行的绑定示例见 `deploy/qianxing.runtime.builtin-strategy.example.json`。

Bundle 的其它组件使用 `strategy.dataset_component_paths` 显式绑定，例如：

```json
{
  "dataset_component_paths": {
    "suspensions": "qianxing.ashare.suspensions.json",
    "limit_rules": "qianxing.ashare.limit-rules.json",
    "factors": "research/factors.snapshot.json"
  }
}
```

组件文件必须是数组，或包含 `rows`/`data` 数组；系统会递归规范化 JSON 字段顺序后计算 fingerprint，并校验行数。未显式绑定的组件会在回测启动前拒绝，避免把“Bundle 中声明存在”误当成“策略实际加载成功”。公司行为和交易日历仍兼容 `ashare_actions_path`、`ashare_calendar_path`。
回测完成后会在运行时 data_dir/runs 下原子保存 RunManifest JSON，记录配置指纹、Bundle 聚合指纹、各数据组件指纹、模型、时钟和结果哈希。

当配置包含 `strategies[]` 时，`strategy-backtest` 会按策略实例逐个执行隔离回测，每个实例使用自己的 account/strategy 配置并输出独立结果哈希；示例见 `deploy/qianxing.runtime.strategy-multi-backtest.example.json`。组合级资金池、跨策略净额和归因需要在组合回测层显式配置，不会隐式共享单策略账户状态。

当前提供 17 个固定点运算的内置策略，可先查看目录再直接回测：

```powershell
cargo run --release -p qx-cli -- builtin-strategies
cargo run --release -p qx-cli -- backtest builtin macd deploy/qianxing.bar-frame.example.json deploy/qianxing.binance.spot.spec.json
cargo run --release -p qx-cli -- backtest deploy/qianxing.runtime.builtin-strategy.example.json deploy/qianxing.bar-frame.example.json
```

内置策略包括 SMA/EMA 交叉、MACD、RSI、布林带、Donchian 突破、动量、均值回归、网格、ATR/Keltner 趋势、VWAP 回归、波动率突破，以及配对、跨交易所、基差、现货/期货四类双腿套利。套利策略额外配置 `builtin_reference_instrument` 和 `builtin_reference_bars_snapshot_path`；跨交易所时主腿/对冲腿可以分别由不同 CCXT REST MarketData worker 维护，现货腿可设置 `builtin_reference_margin_mode: "cash"` 与 `builtin_reference_leverage: 1`，避免把期货杠杆参数发送给现货交易所。

### CCXT 实时策略

`qianxing.runtime.ccxt.example.json` 已包含可运行的 sandbox 实时配置。MarketData worker 会按 `live_timeframe` 持续拉取 OHLCV，默认只落盘已闭合 K 线到 `bars_snapshot_path`；Strategy worker 发现 BarFrame 摘要变化后只投递一次幂等 JobRun，随后沿原有策略、风控、订单和 CCXT Execution 链路执行。

先配置公共 CCXT 凭据（不同交易所只需替换变量名）：

```powershell
$env:QX_CCXT_OKX_API_KEY = "<api-key>"
$env:QX_CCXT_OKX_SECRET = "<secret>"
$env:QX_CCXT_OKX_PASSWORD = "<passphrase>"
```

然后启动监督器：

```powershell
cargo run --release -p qx-cli -- runtime-check deploy/qianxing.runtime.ccxt.example.json
cargo run --release -p qx-cli -- supervise deploy/qianxing.runtime.ccxt.example.json
```

实时模式仍然只让 Python 公共 CCXT 负责交易所协议；密钥通过 `credential_env` 注入，不写入 JSON、日志、事件或策略进程。sandbox 验证通过后，将 `sandbox` 切换为 `false` 前必须完成交易所权限、限频、最小数量、杠杆和订单恢复验收。

多交易所现货/期货套利可直接参考 `qianxing.runtime.multi-venue-arbitrage.example.json`：Binance Spot 和 OKX Swap 各自运行独立 REST MarketData、Execution、Reconcile worker，分别维护两条 BarFrame 快照；`spot_futures_arbitrage` 等待两腿闭合时间一致后生成双腿订单。现货腿使用 Cash/1x policy，期货腿使用 Cross/3x policy，订单按 InstrumentId 的 venue 自动路由到对应 CCXT Execution worker。

CCXT 数据进入回测的推荐流程：

```powershell
cargo run --release -p qx-cli -- ccxt-fetch-ohlcv `
  deploy/qianxing.ccxt.exchange.example.json `
  BTC/USDT.OKX 1704067200000 1706745600000 bars.json 1h
cargo run --release -p qx-cli -- ccxt-market-spec `
  deploy/qianxing.ccxt.exchange.example.json BTC/USDT.OKX market.json
cargo run --release -p qx-cli -- backtest builtin sma-cross bars.json market.json
```

## A 股数据源、快速选股与回测

A 股研究数据通过 `python/qianxing_ashare` 统一转换为 BarFrame。AkShare、Baostock 和
easy_tdx 都是可选依赖，默认不会改变现有 CCXT 运行时。安装一种数据源后即可获取并冻结
历史数据：

```powershell
pip install -e "python[a-share-akshare]"
python -m qianxing_ashare fetch `
  --provider akshare --code 000001 --start 20240101 --end 20241231 `
  --frequency daily --adjustment qfq `
  --output data/ashare/000001.SZSE.json
python -m qianxing_ashare screen `
  --bars-dir data/ashare --min-return-bps 500 --limit 50 `
  --output data/ashare/screen.json
python -m qianxing_ashare screen `
  --bars-dir data/ashare --min-return-bps 500 --limit 50 `
  --output data/ashare/screen.json `
  --backtest-manifest data/ashare/fast-backtest.json `
  --runtime deploy/qianxing.runtime.ashare.example.json `
  --market-spec deploy/qianxing.ashare.spot.spec.json
```

需要将 BarFrame 固化为单机数据集时，可先通过 Rust 数据层做幂等摄取和
DatasetManifest 注册：

```powershell
cargo run --release -p qx-cli -- dataset-ingest `
  data/ashare/000001.SZSE.json ashare.daily snapshot-20260915 data/datasets
```

同一个 `dataset-id + version` 如果产生不同 fingerprint 会被拒绝，避免回测输入被静默替换。

需要把行情、公司行为、交易日历等组件绑定为同一研究快照时，准备
`DatasetBundleManifest` 后执行：

```powershell
cargo run --release -p qx-cli -- dataset-bundle `
  deploy/qianxing.dataset-bundle.example.json data/datasets [bar-frame.json]
```

Bundle 只保存组件身份和 fingerprint，组件数据仍由各自数据集存储管理；真实
fingerprint 必须替换示例文件中的占位值。策略回测配置 `strategy.dataset_bundle_path`
后，会在启动前校验 Bundle 的 bars fingerprint 和 row_count，校验失败直接拒绝回测。
命令末尾提供 `bar-frame.json` 时，Bundle 落盘前也会执行同样的 bars fingerprint 校验。
可直接运行的 BarFrame 绑定示例是 `qianxing.dataset-bundle.bar-frame.example.json`；
生产配置不得直接使用示例清单。

随后可将候选标的的 BarFrame 组成 `fast-backtest` manifest 并行回测：

```powershell
cargo run --release -p qx-cli -- fast-backtest `
  deploy/qianxing.fast-backtest.ashare.example.json
```

公司行为可以单独冻结，避免把在线数据直接带入回测：

```powershell
python -m qianxing_ashare actions `
  --provider akshare --code 000001 --start 20200101 --end 20241231 `
  --output data/ashare/000001.SZSE.actions.json
```

Bars、公司行为和交易日历 manifest 可以由 Python 直接绑定成 Rust Bundle 清单：

```powershell
python -m qianxing_ashare bundle `
  --bundle-id ashare.000001 --version snapshot-20260915 `
  --source akshare+calendar `
  --bars-manifest data/ashare/000001.SZSE.manifest.json `
  --bars-frame data/ashare/000001.SZSE.json `
  --actions-manifest data/ashare/000001.SZSE.actions.manifest.json `
  --calendar data/ashare/cn-calendar.json `
  --output data/ashare/ashare.000001.bundle.json
```

提供 `--bars-frame` 时 Python 会使用与 Rust `qx-data` 一致的 bars fingerprint；生产清单必须提供该参数，避免仅用来源摘要代替实际数据指纹。

统一层会保留原始字段和来源摘要，并支持现金分红、送股、转增、配股、增发、回购和可转债等事件类型。
当前 Rust Ledger 自动处理现金分红、送股和转增；配股/增发认购、回购要约、可转债转股
在 JSON 中同时提供明确数量、价格和目标标的时进入多腿账本并可重放；配股还支持独立的
权利登记、部分认购和剩余权利失效事实。缺少明确生命周期事实时不会自动推导。按登记日自动生成权利、
发行人级增发过程、可转债发行/回售/赎回/到账周期以及缺少参与事实的复杂事件仍会被安全门禁拒绝；
其中 `capital_change` 已支持显式的 `issuer_total_shares_raw` 绝对总股本和可选的
`issuer_free_float_shares_raw` 绝对流通股本，回测报告保留生效日股本快照但不生成账户流水，
缺少绝对总股本时会拒绝配置，
避免产生看似成功但实际错误的净值。可参考 `deploy/qianxing.ashare.complex-actions.example.json`。

示例输入、A 股现货精度、规则快照和批量 manifest 分别见 `qianxing.ashare.bar-frame.example.json`、
`qianxing.ashare.spot.spec.json`、`qianxing.ashare.rules.json` 和 `qianxing.fast-backtest.ashare.example.json`。
当前入口已经完成数据获取、标准化、筛选和 A 股规则化回测接入；`ashare_rules_path`
会启用 T+1、整手、涨跌停封板、停牌和费用模型；配置 `ashare_actions_path` 后，
Python 标准化公司行为 JSON 会在回测启动时转换并合并到规则快照。公司行为 Ledger 变更和真实券商柜台
公司行为快照中的现金分红和拆股会进入 Ledger 并支持重放；完整历史数据覆盖和真实券商柜台
仍需按具体历史数据与券商协议验收。完整边界与后续交付顺序见
[`A股数据源接入与快速选股回测方案-V1`](../docs/A股数据源接入与快速选股回测方案-V1.md)。

三条命令分别冻结历史行情、冻结交易所产品规格并运行本地回测；回测过程不再访问交易所。若 MarketSpec 没有提供精度或维持保证金档位，必须在部署侧补齐后再用于真实合约风险评估。

快速试跑内置策略也可以使用一条命令：

```powershell
cargo run --release -p qx-cli -- backtest ccxt-builtin `
  deploy/qianxing.ccxt.exchange.example.json macd `
  BTC/USDT.OKX 1704067200000 1706745600000 1h `
  deploy/qianxing.ccxt.okx.perpetual.spec.json 1
```

该快捷入口不会保存中间行情快照；生产研究和审计仍应使用上面的三步冻结流程。

Paper/虚拟交易也复用同一份产品规格：现货按现金买卖记账，保证金/永续/期货按持仓、杠杆、保证金和 PnL 记账。为启用执行前规格门禁，在 Execution worker 配置 `instrument_spec_path`；示例规格见 `qianxing.binance.spot.spec.json` 和 `qianxing.ccxt.okx.perpetual.spec.json`。Paper 的订单标的可以使用任意已解析的交易所 InstrumentId，不会被虚拟 Venue 硬编码到 Binance。

多腿策略建议额外配置一个独立的 `spread_recovery` worker。它只扫描持久化的
`HedgeRequired` 组并执行幂等 reduce-only 补偿，不消费普通 SubmitOrder 队列，适合单独重启和扩容：

```json
{
  "id": "paper-spread-recovery",
  "role": "spread_recovery",
  "enabled": true,
  "account_id": "main",
  "venue_id": "paper",
  "endpoint": null,
  "symbols": [],
  "settlement_currency": "USDT",
  "instrument_spec_path": "qianxing.binance.spot.spec.json"
}
```

Binance 的恢复 worker 使用同一账户凭据；公共 CCXT 恢复 worker 将 `endpoint` 配置为对应的
CCXT JSON 配置文件。旧配置不增加该角色时，Execution worker 仍保留兼容恢复扫描；同账户、同
Venue 启用独立恢复 worker 后，Execution 会自动关闭兼容扫描，避免两个进程同时补偿同一订单组。
文件存储还会为每个订单组创建带过期时间和 fencing token 的 recovery claim；同一时刻只有一个
owner 能执行补偿，进程崩溃后 lease 到期即可由下一轮恢复接管。旧 owner 即使在过期后恢复，
也不能凭旧 token 保存或释放新 owner 的状态。该 claim 是单机文件后端能力，分布式部署仍需使用
带租约/fencing 的共享存储并完成外部故障演练。

可以用 `--once` 做本地启动验收：

```powershell
cargo run --release -p qx-cli -- scheduler-worker `
  deploy/qianxing.runtime.example.json scheduler --once
cargo run --release -p qx-cli -- strategy-worker `
  deploy/qianxing.runtime.example.json strategy-paper --once
```

完整本地策略到成交验收使用 Paper 拓扑：

```powershell
cargo run --release -p qx-cli -- scheduler-worker `
  deploy/qianxing.runtime.paper-strategy.example.json scheduler-paper --once
cargo run --release -p qx-cli -- strategy-worker `
  deploy/qianxing.runtime.paper-strategy.example.json strategy-paper --once
cargo run --release -p qx-cli -- paper-worker `
  deploy/qianxing.runtime.paper-strategy.example.json paper-execution --once
```

该链路会产生 `OrderSubmitted → Accepted → Fill → LedgerApplied`，并将控制命令推进到 `Executed`。策略重跑会从 `paper-<account>-<venue>-events.json` 恢复持仓，达到目标仓位后不重复发单；如果执行进程恰好在控制面落终态后崩溃，重启时会确认并清理旧队列，不重复撮合。

也可以用统一验收入口按相同顺序执行三类 worker：

```powershell
cargo run --release -p qx-cli -- paper-e2e `
  deploy/qianxing.runtime.paper-strategy.example.json
```

该入口会验证订单、Ledger、控制面终态和命令队列均已完成。

执行 worker 使用同一入口启动：

```powershell
cargo run --release -p qx-cli -- binance-worker deploy/qianxing.runtime.production.example.json binance-execution-main
```

执行/对账 worker 支持单轮验收（执行一次队列轮询或一次 REST 对账后退出）：

```powershell
cargo run --release -p qx-cli -- binance-worker `
  deploy/qianxing.runtime.production.example.json reconciler-main --once
```

也可以使用等价的对账入口；它默认执行一次 Binance Reconciler：

```powershell
cargo run --release -p qx-cli -- reconcile `
  deploy/qianxing.runtime.production.example.json reconciler-main
```

对账 worker 的 `symbols` 应列出账户允许交易的 Binance symbol，例如
`BTCUSDT.BINANCE`。配置后恢复阶段按 symbol 分页读取 `allOrders`，覆盖进程中断期间
已经成交、撤销或过期的订单；历史终态但不在本地 EventLog 的订单不会被误报为活动差异，
未知活动订单仍会进入人工对账。

每轮对账还会在 `storage.data_dir/reconcile/<worker-id>.json` 原子保存结构化报告，
包含订单差异、余额快照数量和结算币种账簿/柜台差异；报告只记录观察结果，不会自动
修改 Ledger。

它消费 `control-queue/` 中的 `SubmitOrder`，领取带租约和 fencing token 的命令，执行后通过事务回写 `Executed/Failed`。不同账户的执行 worker 不会领取彼此订单；未能确认的网络结果进入 `Unknown/ReconcileRequired`，禁止自动补单。

## SubmitOrder 执行入口

先用完全本地的 Paper 闭环验收控制面、队列、订单事实和账簿归约：

```powershell
cargo run --release -p qx-cli -- paper-submit-order `
  deploy/qianxing.runtime.example.json `
  deploy/qianxing.paper-submit-order.example.json
```

该命令输出 `PAPER_EXECUTED`，并验证 Accepted、Fill、LedgerApplied、控制命令终态和队列确认；它不连接任何网络。

订单提交必须先经过 `ControlCommand(kind=SubmitOrder)` 审计；命令的 `payload.order_json` 只允许包含订单，不允许携带凭证。示例默认 `dry_run: true`，只验证权限、账户、Venue 和订单形状，不建立网络连接：

```powershell
cargo run --release -p qx-cli -- binance-submit-order `
  deploy/qianxing.runtime.production.example.json `
  binance-user-main `
  deploy/qianxing.submit-order.example.json
```

确认沙盒/模拟账户、余额和回滚流程后，才可以把命令中的 `dry_run` 改为 `false`。执行器会先追加 `OrderSubmitted`，再调用 Binance REST submit；成功回报继续写入 Accepted/Fill/LedgerApplied，HTTP 5xx 或连接中断只保留待对账状态，不自动重试。

Execution、SpreadRecovery 与 HedgeRecovery worker 必须配置冻结的 `instrument_spec_path`
（可直接使用 `ccxt-market-spec` 生成的市场快照），并可配置 `max_order_notional_raw`、
`max_position_notional_raw`。提交订单前，Paper/CCXT/Binance 会在同一 EventLog 上重建
账户权益、持仓和标记价，统一执行 lot/tick、产品、杠杆、保证金和名义额预检；缺少规格时
运行时会直接拒绝执行该 SubmitOrder，而不是退回“没有风控”的裸提交——否则同一条策略订单
会在配了规格时被拒、没配时静默成交，风控成了可选项。对账与行情 worker 不提交订单，
不需要该字段。

示例配置已经为 CCXT OKX 永续和 Binance Spot 提供冻结规格文件：
`qianxing.ccxt.okx.perpetual.spec.json`、`qianxing.binance.spot.spec.json`。
真实部署应使用当前交易所 `load_markets`/`fetch_leverage_tiers` 重新生成并审核，不能长期复用示例参数。

行情 worker 在 `storage.data_dir` 下维护自己的 `<worker-id>-events.json`；同一账户/交易所的 UserStream、Execution、Reconciler 共享 `binance-<account>-<venue>-events.json`。行情 worker 写入标准 `MarketQuote` 事实；用户流 worker 将 Accepted/Fill/Cancelled 映射为 Kernel 事件，并在成交后追加 `LedgerApplied`；对账 worker 写入账户余额快照和 `ReconcileRequired` 事实。文件使用 EventLog 的序号、时间/优先级校验和原子替换，进程并发追加冲突时会重新载入并重放，重启时会恢复账簿、行情标记、去重集合和本地订单跟踪状态。

订单提交方必须先通过 `LiveEventPipeline::register_order` 写入 `OrderSubmitted`，再调用 Venue 的 submit；用户流/执行/对账 worker 启动时会从同一日志恢复订单，避免只凭远端回报猜测本地订单形状。事件日志是事实恢复边界，不是跨节点数据库；文件后端适合单机账户级部署，跨节点仍需接入共享数据库或消息队列并保留幂等键和租约 fencing。

## SQLite 限流

需要跨进程共享 API 限流时，用 `storage.backend: "sqlite"` 和 `sqlite_path`，并启用 CLI feature：

```powershell
cargo run --release -p qx-cli --features sqlite -- serve path/to/runtime.json
```

PostgreSQL 生产/多节点起步配置见 `qianxing.runtime.postgres.example.json`。启动前只通过秘密管理器注入 DSN：

```powershell
$env:QX_POSTGRES_DSN = "postgresql://qx_user:<password>@db.example:5432/qianxing?sslmode=require"
cargo run --release -p qx-cli --features postgres -- runtime-check deploy/qianxing.runtime.postgres.example.json
cargo run --release -p qx-cli --features postgres -- serve deploy/qianxing.runtime.postgres.example.json
```

当前 PostgreSQL 后端已覆盖控制面、控制命令、JobQueue、审计链、快照和 EventLog 存储契约，并支持通过 `storage.postgres_pool_size` 配置单进程连接池；实际数据库高可用、读写分离、TLS 证书策略和灾备恢复仍需在部署环境执行验收矩阵。

## Outbox 与 NATS JetStream

Outbox 会随 EventLog 事实追加生成稳定 `event_id` 的事件 envelope。文件后端可用可选
NATS feature 做单批 relay；`qx-storage` 同时提供 Pull Consumer 适配器，JetStream stream、复制数、保留策略和 consumer 应由部署系统
预先创建，交易进程不自动修改这些策略：

```powershell
cargo build --release -p qx-cli --features nats
cargo run --release -p qx-cli --features nats -- outbox-relay `
  ./data nats://127.0.0.1:4222 qianxing 100
```

PostgreSQL Outbox 使用运行时配置中的 `postgres_dsn_env`，需要同时启用两个 feature：

```powershell
cargo run --release -p qx-cli --features "nats postgres" -- outbox-relay-postgres `
  deploy/qianxing.runtime.postgres.example.json `
  nats://127.0.0.1:4222 qianxing 100
```

生产环境可将 Relay 纳入 `qx-cli supervise`。示例配置见
`deploy/qianxing.runtime.messaging.example.json`，构建并启动持续 worker：

```powershell
cargo build --release -p qx-cli --features "nats postgres sqlite"
cargo run --release -p qx-cli --features "nats postgres sqlite" -- `
  outbox-relay-worker deploy/qianxing.runtime.messaging.example.json outbox-relay
```

单次批处理验收可追加 `--once`；worker 使用 `relay_interval_ms`、`relay_batch_size`、
`lease_seconds` 和 `storage.backend` 装配对应的文件/SQLite/PostgreSQL Outbox。

每个持续 worker 会将当前状态原子写入
`storage.data_dir/worker-metrics/<worker_id>.prom`，API 的 `/metrics` 会聚合这些文件。
Relay 指标包括 `qx_worker_up`、心跳、扫描、发布、重试、租约冲突和发布失败；Consumer
指标包括接收、成功、重复、重试、死信、格式错误和 ACK 失败。`qx_worker_up=1` 只表示最近
一次写入仍认为进程正常，生产告警还必须结合
`qx_worker_heartbeat_timestamp_seconds` 的新鲜度判断进程是否已经失联。

死信重放不会删除原始死信或覆盖原事件，而是发布带有确定性 `:replay:<attempts>` 后缀
的新 `event_id`；重复执行同一重放命令仍由消费者幂等保护：

```powershell
cargo run --release -p qx-cli --features "nats postgres sqlite" -- `
  consumer-dlq-replay deploy/qianxing.runtime.messaging.example.json ledger-reducer event-123
```

持续消费示例见 `deploy/qianxing.runtime.consumer.example.json`。外部 handler 接收一行
Outbox envelope，退出码 0 表示业务事务成功，非 0 或超时表示可重试失败：

```powershell
cargo run --release -p qx-cli --features "nats postgres sqlite" -- `
  event-consumer-worker deploy/qianxing.runtime.consumer.example.json ledger-reducer
```

示例 handler 位于 `tools/event_consumer_example.py`。外部 handler 与 qx-cli 的 checkpoint
不共享数据库事务；需要严格原子性的领域 reducer 应在进程内使用
`ConsumerEngine::consume_with_projection` /
`NatsJetStreamConsumer::consume_batch_with_projection`，由文件、SQLite 或 PostgreSQL
同时提交 projection、processed marker 和 checkpoint。JetStream durable consumer 的
`max_deliver` 不应小于 `messaging.consumer_max_attempts`，否则消息会在框架写入内部
DLQ 前被 broker 提前终止；生产环境还应配置 broker 原生 DLQ/告警。

该命令执行一批 `claim → JetStream publish ack → ack`；网络或发布失败只会 retry，事件
不会被静默删除。消费者处理成功或进入内部死信后 ACK，临时业务失败 NAK 由 JetStream
重投；状态端可使用 `FileConsumerStateStore`、`SqliteConsumerStateStore` 或
`PostgresConsumerStateStore`。生产环境应由 supervisor/容器编排持续拉起 relay/consumer，
并配置监控、退避、max-deliver、外部 DLQ、重放和真实集群故障演练；当前命令不是完整
MQ 集群治理器。

SQLite 现在承载控制面、SubmitOrder 队列、审计、快照和限流的单机事务语义；跨节点队列、PostgreSQL 高可用、NATS stream/consumer 治理和消息链路灾备仍需部署层验收，不能把 SQLite 配置误当成集群一致性保证。

API 探针语义固定为：`GET /health` 只表示进程存活；`GET /ready` 检查控制面存储、已声明的研究快照、生产凭据/冻结规格，以及
已产生的 Relay/Consumer worker 指标中的 down/stale 状态，依赖不可用时返回 HTTP 503。生产编排仍应把
MQ、用户流、对账和交易安全状态继续接入同一 readiness provider，不能只依据 `/health` 放行交易流量。

Prometheus 告警规则模板位于 `deploy/prometheus/qianxing-alerts.yml`，覆盖 worker 失联、心跳
过期、Outbox 发布失败、Consumer 死信和 ACK 失败；生产环境应根据实际抓取间隔、租约窗口和
值班策略调整 `for` 与 heartbeat 阈值。

## 停机与故障

服务进程收到 Ctrl+C 后由进程管理器负责终止；交易 worker 必须先停止新信号，再等待账户命令队列、用户流关闭和对账完成。若用户流或对账 worker 进入 `Failed`/`Degraded`，不得自动补单，必须走快照恢复与人工确认。
