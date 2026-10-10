# 牵星运行时部署说明

本文讲**运维面**：运行时配置、worker 拓扑、CCXT/A 股接入、SubmitOrder、存储后端、HTTP/WS 边界与停机
故障。它假定你已经按根目录 [`README.md`](../README.md) 的「安装」装好 `qx-cli`（路径 A：
`cargo install --path crates/qx-cli --locked`；路径 B：`build.bat` / `bash build.sh` 跑全套门禁）。
本文里多数命令写成 `cargo run --release -p qx-cli -- …`，那是**在源码仓库里**的写法；已按路径 A 装过的
人把前缀换成 `qx-cli ` 即可，参数与行为一致（同一份 clap 命令表）。上手细节（初始化项目、回测入口族、
产物字段、读模型 `null` 口径）在
[`docs/工业化易用性收口指南-V1.md`](../docs/工业化易用性收口指南-V1.md)。

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
cargo run --release -p qx-cli -- version
cargo run --release -p qx-cli -- live-check deploy/qianxing.runtime.production.example.json
cargo run --release -p qx-cli -- runtime-check deploy/qianxing.runtime.example.json
cargo run --release -p qx-cli -- runtime-check deploy/qianxing.runtime.production.example.json
# CI/部署平台可直接消费 JSON；失败时退出码非零
cargo run --release -p qx-cli -- runtime-check deploy/qianxing.runtime.example.json --json
cargo run --release -p qx-cli -- live-check deploy/qianxing.runtime.production.example.json --json
cargo run --release -p qx-cli -- binance-public-probe testnet BTCUSDT.BINANCE
cargo run --release -p qx-cli -- binance-private-probe deploy/qianxing.runtime.production.example.json binance-execution-main
```

`version` 只印一行构建身份，形状是 `qianxing <semver> (build <sha7>[-dirty], target <triple>, profile <release|debug>)`；`--version` 与 `-V` 是同一条入口的两个别名，三条写法逐字相同、都退 0，且都不带横幅，脚本可以直接取值（release 产物的实测形状：`qianxing 0.1.0 (build ad2908b-dirty, target x86_64-pc-windows-msvc, profile release)`，单行 85 B，见 `logs/s686_pass28_release_surface_probe.txt`）。
这一行的唯一来源是 `crates/qx-cli/src/build_identity.rs`：`doctor` 的第一格 `build_identity` 印同一行，`status --json` 与 `report --json` 的 `runtime_version` 也读同一个常量，人工读面与机器读面不会漂成两个版本。

用法错误（未知命令、未知参数、参数形状不对）不再打印整篇入口摘要：回的是 clap 的错误正文（含最接近的入口名）与该入口自己的 `Usage:`，再加「下一步」与「自证构建」两行，退出码仍是 2。实测（`logs/s686_pass28_release_surface_probe.txt`）：`bogus-entry` 回 8 行 / 289 B，`doctor --config x.json` 回 10 行 / 359 B 并印 `Usage: qx-cli.exe doctor [OPTIONS] [PATH]`（argv[0] 原样，这个入口收位置参数）；改前两条甩出的都是整篇摘要：同一份改前二进制上实测 162 行 / 12,319 B（`--version`/`-V`/未知名）与 164 行 / 12,361–12,389 B（`version`/`doctor --config …`），逐字在 `logs/s691_pass28_before_fix_error_wall.txt`。整篇摘要现在只在 `help` 与 `--help`/`-h` 两个出口打印，它自己是 157 行 / 12,347 B（`logs/s686_pass28_release_surface_probe.txt`）。

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
cargo run --release -p qx-cli -- quickstart my-qx
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
cargo run --release -p qx-cli -- paper-check deploy/qianxing.runtime.paper-strategy.example.json
```

`quickstart` 是上面那一串的一条命令版首跑路径（V13 R2 #255）：依次执行 `init --strategy macd` → `doctor` →
`backtest` → `report` → `status` 五步，每步成功印一行「[完成] <步骤名>：<该步的完整命令>」，任一步失败即停下、
只回显失败那一步的命令原文与重跑整条的写法并以 2 退出；它直调这五个入口所用的同一批函数，不另起实现，所以同一份输入的
`result_hash` 与逐条敲逐字相同（`crates/qx-cli/tests/quickstart_first_run.rs` 断言这条相等，并把「产物只落给定目录、
仓库 `deploy/data/` 一份都不碰」也钉成判据）。收尾给三条照抄就能退 0 的下一步（`report --json` / `strategy list` /
`init --profile ashare`）；`paper-check` 不在其中——它只对 paper profile 那份带启用 Scheduler worker 的运行时成立
（#261），文档把它写成带前提的一句话而不是待敲命令。

`paper-check` 是端到端的一次性验收（调度 → 策略 → 注入一条合成行情 → 执行 worker 撮合），末行按「本轮新增」而不是事实流的累计量给结论（V13 第三十一遍 ② #275）：真跑出成交的那一遍印 `orders=N (+N 本轮新增) … ✓`；同一目录当日重敲第二遍时调度把当天那轮判为 `skipped`、策略与执行各 `processed=0`，端到端一手没跑，末行于是如实写「本轮零新增：当日调度已跳过、复用上一轮既有事实，未端到端重跑」且不带 ✓。两遍都退 0——空转是幂等复用而不是失败。判据在 `tools/check_architecture.py` 的 `paper_check_delta_honesty_check`。

`doctor` 的第一格是 `build_identity`（与 `qx-cli version` 逐字相同的一行），之后才是一次检查运行时配置、策略输入、数据 Bundle、公司行为/日历文件、存储目录和运行拓扑；
它不会连接交易所或发送订单。`config explain --json` 输出经过校验的有效配置，只有凭据引用名称/路径，
不会读取或打印 key、secret 内容。

账户事实另有两条口径检查：同一 `(account, venue)` 身份上的启用写入方（user-stream / execution /
spread-recovery / reconciler）若声明了不同的 `settlement_currency`，判为配置错误
（`account_log_settlement: fail`）——成交扣减和风控读取会各记各的账簿，且币种随事实永久落盘；
`storage.data_dir` 里没有任何身份引用的 `*-events` 账本只告警点名（`event_logs.orphan: warn`），
账户日志名按 `(account_id, venue_id)` 派生，切换是硬切的，旧账本不自动改名也不自动删除，归档与否由运维决定。
该扫描读的是 Files 后端的目录，`backend` 为 `sqlite`/`postgres` 时这条检查同样给 `warn`，
但含义是"未覆盖"而不是"已确认干净"——那两类存储里的遗留账本要按存储侧自行核对。

`settlement_currency` 的**缺省值**只写在一处：`crates/qx-core/src/identity.rs` 的
`DEFAULT_SETTLEMENT_CURRENCY`（当前取值 `USDT`）。回测装配、worker 声明、账户日志三条回落链都读它，
所以"这一格没写"与"写了 USDT"给出同一个答案；要换整个盘子的缺省币种，改那一行即可，不必找第四处
（生产代码里再出现第二处写死的 `"USDT"`，架构门禁的 `settlement_currency_check` 会红）。
它是缺省值而不是合法币种白名单，配置里仍按上面那条规则要求同一账户日志的写入方声明一致。
记账币种会随事实永久落进产物：只换 `settlement_currency` 而其余输入不动，回测摘要的 `result_hash`
必须随之改变，这一格由 `crates/qx-cli/src/tests/settlement_currency_single_source.rs` 走真实回测入口
比对两份产物（V13 R1-A4）——产物里的收益数字因此能说清自己是哪种币的收益。

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

## 模板契约面：本目录每份模板都有人真读

本目录顶层的 **53 份** JSON 模板不是"给人看的示例"，而是契约的另一半：每份都在
`crates/qx-cli/src/tests/deploy_template_coverage.rs` 的登记表里点名了**一类生产读法**，
用例真的调用那个读点，并把读出来的身份印进日志。跑这一族用例：

```powershell
cargo test -p qx-cli --bin qx-cli deploy_template_coverage -- --nocapture
python tools/check_architecture.py
```

登记表是三列 `(文件名, Reader, Expected)`。`Reader` 有 15 类，各自对应链路上真实存在的那个读点，
不在用例里另写一份校验：`Runtime`（`read_runtime_config` + `config validate` 的引用体检）、
`MarketSpec`（`market_spec_from_value`，产品规格形状与 CCXT 归一化形状同源）、`Ccxt`
（`validate_ccxt_worker_binding`）、`BarFrame`（回测链那一对读点）、`DepthFrame`、`DatasetBundle`、
`ArrowComponent`、`AshareRules`、`AshareActions`、`AshareCalendar`、`CostRules`（`ExecutionCostRules::load`）、
`SchedulerJobs`、`StrategyTarget`、`SubmitOrder`（`order_from_submit_command`）、`FastBacktest`。

`Expected` 只有两档，没有"跳过"这一档：`Ok`，或 `Refuses(关键字清单)`。
`qianxing.runtime.production.example.json` 落在后者 —— 它按设计带着 `/var/lib/qianxing` 与
`/run/secrets` 的占位路径，`config validate` 会报 `research_snapshot_path`、`dataset_bundle_path`
两项不存在；用例把这 3 项关键字点名核对并把拒绝理由原样打印，**不是**把这份模板排除在覆盖之外。
真实部署机上这些路径必须存在，`live-check` 才会放行。

**新增一份模板时必须做两件事**：在登记表里加一行（文件名 + 读取器 + 预期），并保证它能被那一类
读点读通。只加文件不加登记，`deploy/` 清单与登记表逐名相等的那颗判据会当场变红；登记了却把
`Reader` 配错类别，用例会红。门禁另有 9 颗判据盯这套覆盖本身（登记表三列对齐、变体清点、
每个变体都真被用到、每个变体都进坏内容探针、读取都落在生产读点、三条用例与模块挂载、
预期结果两档、配对来源仍在册），删掉一段判据不会静默变绿。

两条会咬人的口径，写在这里是因为本轮实测就是按这两条抓到已发布示例的问题：

- **`schema_version >= 1` 的 BarFrame 文档只能带严格字段集**。`crates/qx-data/src/provider.rs` 的
  `parse_bar_frame` 先按旧格式宽解，`schema_version >= 1` 时改用 `deny_unknown_fields` 的严格视图
  重解，并要求 `source` 非空。手写行情帧时多出一格（例如把取数请求的字段名抄进行情文档）在只跑
  宽口径回测直读链时看不出来，一过 Provider 就炸。合法区间还有一条下限：Provider 拒 `start == 0`。
- **SubmitOrder 载荷里 `policy` 的三格是 snake_case 枚举名**：`position_side` 取
  `net`/`long`/`short`，`margin_mode` 取 `cross`/`isolated`，`position_mode` 取 `one_way`/`hedge`
  （`crates/qx-core/src/trading.rs` 上三个枚举都带 `rename_all = "snake_case"`）。写成
  `Net`/`Cross`/`OneWay` 的示例在反序列化那一格就失败，不会退回到"看起来更象交易所"的写法。
### 这一目录的模板是怎么被读到的

上面那张登记表说的是"每份模板都有人真读"，读取之前还有一格是"那份文件在哪儿"。V13 第三十一遍 ① 之前
这一格有两套机制并存：`doctor`/`status`/`config *` 能回到仓库的 `deploy/`，而 `runtime-check`/`live-check`/
`paper-check` 只把 `"deploy/…"` 拼在当前目录上 —— 同一棵树、同一个无关启动目录，前一组退 0、后一组退 2 并回
一行 `系统找不到指定的路径`（改前实测 `logs/s734_pass31_default_path_probe.txt`）。现在只有一条链
（`crates/qx-cli/src/deploy_lookup.rs`）：`QX_DEPLOY_DIR` → exe 同级 `deploy/` → 再往外一层 → 构建期源码树 →
当前目录 → 二进制内置清单（本目录顶层那 53 份 JSON 由 `crates/qx-cli/build.rs` 在构建期快照进 exe，需要时把
**整份清单**落进当前用户的临时目录，目录按清单内容签名分桶）。落整份而不是只落被点名的那一份：
`fast-backtest` 的 manifest 里作业按**同级文件名**引用 runtime/bars/spec，只落一份会让这条链在下一格读取上断掉。

两个口径要分清，本轮的覆盖就是按这条线铺的：

- **接了查找面的入口**：只读入口的默认值（命令表里以 `deploy/<文件名>` 形状作默认值的路径参数全部挂
  `parse_deploy_path`）、无默认值的必填/可选读取位置参数（`backtest` 的 `[runtime] [frame] [spec]`、`paper-submit-order` 的
  runtime 与 command、`reconcile` 的 runtime，第三十一遍 ② #274 补齐），加上 `fast-backtest` 的 manifest——它在读取点自己走这条链。只要那是"示例配置形状"的
  路径且当前目录没有这一份，就按上面的顺序搬走，并在 stderr 说一句 `[查找 · Lookup]`；不做静默替换，屏幕上
  那行 `config_fingerprint=` 描述的是哪一份文件始终可查。
- **没挂这个解析器的入口**（`scheduler-worker`、`dataset-*`、`strategy backtest`，以及要实盘凭据与运行中 worker 的 `binance-submit-order`——与 paper 侧那条日常入口相反，属精确路径）按当前目录
  原样解析；读不到时错误正文点名"这一份示例在别处存在: <路径>"，并说明这条不对称。

"读不到"那一句只由唯一读取口 `read_example_json` 拼一次：运行时配置、快速回测 manifest、BarFrame 与深度帧、
market spec、A 股规则三件、策略 `dataset_bundle_path` 指的那份 DatasetBundleManifest，以及数据集链的六格读取
都经它。`runtime-check` 里那两处按 runtime.json 同级目录展开的读取**不走**这条链：它的口径是把一批失败攒成
一张检查清单（`{label} 文件不存在: … (configured=…)`），与"这一份示例在别处"回答的不是同一个问题。

把 exe 拷到另一台机器、或 `cargo install` 之后删掉 clone 也跑得通：这一格不是文档愿望，2026-10-02 在第二棵
源码树上把 `deploy/` 整目录删掉（127 份）之后 `init`（项目内落 9 份）/`doctor`/`status`/裸 `backtest` 逐条退 0，
默认输入的落点就是临时目录里按签名分桶的那一份，`result_hash=1189853a7c12447d` 与同一份输入在仓库树里跑出的
逐字符相同（`logs/s744_pass31_standalone_exe_probe.txt`）；两条排队入口 `fast-backtest` 在同一棵无 `deploy/`
的树上也各退 0，A 股那条 `jobs=1`、BTC 那条 `jobs=2`（`logs/s752_pass31_standalone_fast_backtest.txt`）。
内置层的机制与"报错路径不写盘"由 `crates/qx-cli/src/tests/deploy_lookup.rs` 的 10 条单元用例与
`crates/qx-cli/tests/default_example_paths.rs` 的 7 条集成用例钉住。变异反向验证分三份看才完整：
`logs/s743_pass31_builtin_layer_mutation.txt`（内置层）、`logs/s753_pass31_funnel_mutation.txt` 与
`logs/s755_pass31_funnel_mutation.txt`（同一个"内置层只落被点名的那一份"的破坏在第一遍**当场假绿**——桶按内容
签名共用一个目录，本机已被上一轮落满整份，那条用例当时只在确认环境残留；改成先把要判的那几份删掉再落之后，
第二遍六颗破坏各自在预先声明的那一层咬住），以及 `logs/s756_pass31_lookup_parser_mutation.txt`（把命令表里那
七处解析器整片摘掉）；第三十一遍 ② #274 又把六个无默认值的读取位置参数（`backtest` 的三个、`paper-submit-order` 的两个、`reconcile` 的一个）挂上解析器，`logs/s779_pass32_lookup_mount_mutation.txt` 逐颗摘掉这六处、每颗都让门禁红在被点名的那一个字段上。门禁另有常驻判据钉住这条链的入口只有一处定义、示例读取的报错只由一处拼装、命令表里以示例形状作默认值的路径与这些必填读取位置参数全部挂着解析器。

## Paper API


```powershell
cargo run --release -p qx-cli -- serve deploy/qianxing.runtime.example.json
```

`serve` 会先校验配置，再启动 API worker；worker 异常、panic 和正常退出都会写入运行时健康状态。生产环境必须使用 `transport: "mtls"`，并通过部署系统把证书路径映射到只读文件或秘密管理器。mTLS 服务端会使用 `TlsPemReloader` 每秒轮询证书链、私钥和客户端 CA，并同步轮询 Operator 证书映射；新连接使用成功解析的新配置，已有连接继续使用握手时配置，解析失败保留旧配置。替换 Operator PEM 只更新已在运行时配置中声明的 operator 身份，不会仅靠替换 CA 自动授予新权限；私钥不会写入事件日志或配置摘要。

`serve` 启动时会恢复控制面状态；`storage.backend: "files"` 使用 `control-plane.json` 与 `control-queue/`，`storage.backend: "sqlite"` 使用 `sqlite_path` 中的事务表，`storage.backend: "postgres"` 使用 `postgres_dsn_env` 指向的 DSN 并自动执行幂等迁移。PostgreSQL 后端的控制面、控制命令队列和 JobQueue 使用事务、advisory lock、租约与 fencing token；带密码的 DSN 不得写入 JSON。控制命令先原子持久化再入队，队列写入失败不会丢失 Accepted 命令；Execution worker 启动后会扫描 pending 命令补队列。

### Shared research API

`POST /app/validate-dataset`、`POST /app/backtest` 和 `POST /app/verify` 使用与 CLI/Python 相同的版本化 `qx-app` contract。它们目前覆盖同步 Bar 研究流程。输入文件必须位于 `QX_API_DATA_ROOT`（默认 `.qianxing/api-data`）内；输出只写入 `QX_API_ARTIFACT_ROOT`（默认系统临时目录下的 `qianxing-api-runs/<run_id>`）。请求中的路径不能越出输入根目录，也不能覆盖服务端产物根目录。服务启动前把数据放入输入目录，并用 mTLS Operator 身份保护远程 API。

API 的认证边界只由 `transport` 与 `api.operators` 决定，不由 `environment` 的措辞决定（V13 R2 第二十三遍 #244）：

- `transport: "mtls"`：必须同时给出服务端证书三件套（`api.tls`）与至少一条 `api.operators` 证书映射，`serve` 据此装上操作员权限策略，operator 身份来自握手证书；这种部署可以绑可路由地址（仓库里唯一那份生产模板就绑 `0.0.0.0:8443`）。
- `transport: "plaintext"`：拿不到对端身份，`api.operators` 必须为空，于是**不装**权限策略 —— `POST /control/commands` 的档位直接取请求体里的 `permission` 字段，等于调用方自报。明文面因此只能绑回环地址：`config validate`、`doctor`、`serve` 共用同一个 `RuntimeConfig::validate()`，`api.bind` 的 IP 不是 loopback 即以「明文 API 只能绑定回环地址」拒绝（`production` 环境本来就禁止明文 API）。
- 「内网可信」不是这条闸门的例外：要跨主机调用就换成 `transport: "mtls"` 并登记 Operator 证书。仓库里 18 份明文 runtime 模板全部绑 `127.0.0.1`，与这条闸门天然兼容 —— 这个份数与"每份明文模板的 bind 都是回环"两件事都由用例钉住，不靠人工点数。

这三条不是散文承诺：`crates/qx-cli/src/tests/api_transport_auth_boundary_doc.rs` 按 `config validate` 的同一读法装载 `deploy/qianxing.runtime.example.json`，只把 `api.bind` 换成可路由地址后要求 `validate()` 报出上面那句原文，并在同一进程里对比"装了策略"与"没装策略"两种 `ApiService` 对同一条自报 `permission` 的下单请求各自的出口（没装策略那条拿到的是 202，装了策略而无证书身份的那条拿到 403）。源码侧的判定式住在 `crates/qx-runtime/src/runtime_config/topology_validation.rs`。

### environment 只有四种写法

运行时配置里的 `environment` 不是自由字符串，只认 `paper`、`sandbox`、`testnet`、`production` 四种写法：大小写不敏感，
但**不许带首尾空白**。名单的唯一来源是 `crates/qx-runtime/src/runtime_config/schema.rs` 的 `ENVIRONMENT_VOCAB`（4 颗写法），
判定式住在 `crates/qx-runtime/src/runtime_config/topology_validation.rs`，`config validate`、`doctor`、`runtime-check`、
`serve` 共用同一个 `RuntimeConfig::validate()`，名单外的值当场被拒，报错里回吐的就是同一份名单。

为什么要收紧到名单：这个字段被 14 处按措辞分派生产加固——配置面 9 处（`strategy_validation.rs` 5 处、
`topology_validation.rs` 4 处），CLI 侧 5 处（`live_check.rs` 1 处、`readiness.rs` 2 处、`strategy_binding.rs` 1 处、
`strategy_contract.rs` 1 处）；而实时策略作业走模拟还是真实提交只按其中一种写法判
（`crates/qx-cli/src/scheduler.rs` 的 `dry_run = environment 等于 "paper"`，大小写不敏感）。收紧之前 `"paper "`（尾部一个空格）
会被接受并顺着这条默认臂静默落进**真实提交**，`"production "` 会被接受并把上面那 14 处加固全部关掉。今天这两种写法都
读不回配置，要拼错只能拿到一条列出四种合法写法的报错。

`sandbox` 与 `testnet` 的隔离性不由这个字段承载：前者是 CCXT 端点 JSON 里的 `"sandbox": true`，后者是
`venue_id: "binance-testnet"`。`environment` 只决定"这轮实时作业按模拟还是按提交处理"与"生产加固开不开"这两件事，
措辞本身不是隔离边界，也不是认证边界（认证边界只看 `transport` 与 `api.operators`）。

名单与提交臂的对应关系有判据两处：`crates/qx-runtime/src/runtime_config/topology_tests.rs` 的
`environment_outside_the_closed_vocab_is_rejected_not_silently_branched` 钉装载面（名单外必须拒、报错必须回吐同一份名单），
`crates/qx-cli/src/tests/environment_submit_arm_table.rs` 的
`every_admitted_environment_spelling_declares_its_submit_arm` 要求那张"四种写法各自走模拟还是真实提交"的表与
`ENVIRONMENT_VOCAB` **集合相等**，再逐写法驱动真实的策略作业装配核对 `dry_run`，并钉住混排写法（`"Paper"`）不得换臂。
名单每加一个写法而不同轮为它声明提交臂，判据先红。仓库里 19 份带 `environment` 的 runtime 模板实测分布为 paper 15 份、
sandbox 2 份、testnet 1 份、production 1 份，逐份都被用例按 `config validate` 的同一读法装载。

### HTTP 读面与控制面路由

下表是 `qx-api` 当前实现的全部入口（18 条 HTTP 路由——17 条 `GET` 读面 + 1 条 `POST` 写面——再加 1 条 WebSocket 升级），逐条来自
`crates/qx-api/src/lib.rs` 的 `handle_inner`。除 `/health`、`/ready`、`/schema/account-snapshot-v1`、`/schema/contract-matrix`
之外，只要运行时配置里装了操作员权限策略（`transport: "mtls"` 必然装），未通过证书识别的
请求一律 `403 {"error":"authenticated_operator_required"}`。「返回」那一格里写成 `{…}` 的键集不是示意：`crates/qx-cli/src/tests/api_response_field_doc.rs` 会在同一进程里驱动 `ApiService`，把每条入口真的序列化出来的键集与这一格逐条比相等（V13 R2 第六遍）。
路由名这一层的相等由 `crates/qx-cli/src/tests/api_endpoint_table_routes.rs` **逐张表**核对，
口径与盲区见下「端点表按张核对」。

| 入口 | 返回 | 说明 |
| --- | --- | --- |
| `GET /health` | `{"status":"ok"}` | 只表示进程存活，不表示可放行交易流量 |
| `GET /ready` | `{"ready":<bool>,"detail":<string>}` | 就绪检查 + 投影健康；未就绪时状态码 `503` |
| `GET /metrics` | Prometheus 文本 | 见下「指标出口」：API 自身四条指标逐行印出，并追加 `worker-metrics/<worker_id>.prom` 聚合 |
| `GET /schema/account-snapshot-v1` | JSON Schema | 公布的就是仓库里的 `schemas/account-snapshot-v1.json`（编译期 `include_str!` 取用，不存在第二份），供读者自证 |
| `GET /schema/contract-matrix` | JSON 数组 | 稳定契约的**命名转换矩阵**（`qx_core::contract::CONTRACT_MATRIX`）：每行 `{concept, canonical_types, canonical_source, duplicates, adapter, note}`，登记同名概念谁是规范单点、哪些是同名兄弟、由哪个显式 adapter 桥接；与真实代码逐条对账由门禁 `contract_matrix_check` 看守 |
| `GET /account/snapshot` | 快照 JSON / `404 snapshot_not_found` / `404 account_projection_not_found` | 单账户投影快照；八个汇总钱字段里未算的那几格是 `null` 而不是 0，名单见下 |
| `GET /account/snapshot/envelope` | 投影信封 / `404` | `data` 满足上面那份 schema（V12 R4-h） |
| `GET /account/snapshot/diff?base_hash=<u64>` | `{schema_version, base_state_hash, target_state_hash, target_header, cash, positions, orders, fills, transfers, replacement}` | `base_hash` 缺失或非无符号整数 → `400`；基准不存在 → `409 snapshot_base_not_found`；`cash`/`positions`/`orders`/`fills`/`transfers` 五条是 `Change` 数组（`{"Upsert":{"key":…,"value":…}}` 或 `{"Remove":{"key":…}}`），八个汇总钱标量不走差分数组，只由 `replacement` 整格搬运，见下 |
| `GET /account/orders`、`/account/positions` | 数组 | 全局投影没有快照时返回空数组，不返回 `404`；带键且这份部署里没有该投影 → `404 account_projection_not_found` |
| `GET /account/balances` | `{cash_raw, equity_raw, available_raw, margin_raw}` | `cash_raw` 是按币种聚合的 map（没有快照时是 `{}`），另三格是标量；未算出的钱保持 `null`，不印成 `0`（V11 Q70） |
| `GET /account/ledger[?account_id=]` | 数组 | 读模型来自 `storage.data_dir`，非实时推送；`?account_id=` 在结果集上按账户过滤，过滤不出条目时是空数组而不是 `404`（V13 R31） |
| `GET /reconcile/reports[?account_id=&venue_id=]` | 数组 | 同上，`ReconcileReportSnapshot` 还带 `venue_id`，所以这把键也认；两把都认时是 AND（V13 R31） |
| `GET /scheduler/runs`、`/control/audit` | 数组 | 读模型同上，非实时推送；这两条读的是整份现读模型（默认账户那一份），**没有收窄键**，带任何查询串一律 `400`（V13 R2 #205） |
| `GET /events?after=<seq>` | `Event` 数组，每格 `{seq, ts, prio, kind, receive_time, engine_time, source_seq, correlation_id, metadata}` | 快照式读取；游标越界 → `409 event_cursor_requires_snapshot` |
| `GET /events/live?after=<seq>` | `ProjectionEnvelope` 数组，每格 `{schema_version, kind, tenant_id, run_id, account_id, portfolio_id, venue_id, as_of, event_seq, cursor, state_hash, source, lineage, data}` | 一次性 read-after，不是长连接；每条事件外面套一层投影信封、事件本体在 `data` 里，与 `/events` 的裸 `Event` **不同形**；游标语义与 `/events` 同口径（V12 R4-g） |
| `POST /control/commands` | 受理结果 | 载荷非法 → `400`；未识别操作员 → `403`；先持久化再入队，入队失败不回滚受理、计入 `qx_api_command_enqueue_failures_total`（见下「指标出口」） |
| `POST /app/validate-dataset` | 应用层用例 `qx_app::validate_dataset` 的裁决文档（请求体是 `DatasetSpec` JSON） | 数据集**不足**是成功返回（裁决里 `usable` 为假且 `gaps` 非空，仍 200），不是非 200；只有"在盘但读不出来"才失败（T2-2） |
| `POST /app/backtest` | 应用层用例 `qx_app::run_backtest` 的结果文档（请求体是 `BacktestSpec` JSON） | 四份产物落在 spec 的 `output_dir` 下；与 `qx-cli app backtest`、Python 的 `app.run_backtest` 是**同一份结果文档**（同一 use case、同一 `result_hash`，退出门 G1） |
| `POST /app/verify` | 应用层用例 `qx_app::verify_run` 的复核文档（请求体取上一行的响应体） | 产物缺失/互不一致是成功返回（`verified` 为假 + `mismatches` 非空，仍 200）；只有"这份 outcome 自己没带 run_id"才失败（T2-2） |
| 任意路径 + `Upgrade: websocket` | `101` 帧流 | 见下 |

七条读投影的入口（`/account/snapshot`、`/account/snapshot/envelope`、`/account/orders`、
`/account/positions`、`/account/balances`、`/events`、`/events/live`）都接受 `?account_id=&venue_id=`：
两者必须同时出现，否则 `400
{"error":"account_id 和 venue_id 必须同时提供"}`；都不出现时读全局投影。`?after=` 必须是十进制
无符号整数，含义是**事件序号**，不是这条日志的下标。方括号里的查询串是**名单**，不是「可以随便带」：`/account/snapshot`、`/account/snapshot/envelope`、`/account/orders`、`/account/positions`、`/account/balances` 只认 `account_id`/`venue_id` 这一对，`/events` 与 `/events/live` 再多认一把 `after`，`/account/snapshot/diff` 认 `base_hash` 加那一对；名单外的键当场 `400`，正文写 `{路由} 不接受查询参数 {键名}` 点名被拒的那把（V13 R6）。改前这一格反过来读才看得清：#205 只盖住四条整体现读端点，带键这几条照收任何查询串，于是 `?acount_id=` 拼错时它落到「没有收窄键」那一支，默认账户那份被念成调用方点名的账户——正是 #191 那句「拼错的账户 id 读成干净的空账户」剩下的下半格。
带键时先查这份部署里有没有该 `(account_id, venue_id)` 的投影：没有就是 `404
{"error":"account_projection_not_found"}`，七条入口同一口径，不再出现"订单表空数组 + 权益
`null` + 事件空数组"这种把"没这个账户"伪装成"这个账户什么都没发生"的读法（V13 R2 第十二遍 #191）。
投影存在但还没发布快照时**仍是**文档承诺的 200 空数组 / `null`（快照入口则是 `404
snapshot_not_found`）：一个账户刚挂上投影、还没算出第一份快照，与这个账户根本不在这份部署里，
是两件事，合成一个码就读不回来了。`/account/snapshot/diff` 不在这七条里——它的定位符是
`base_hash`，基准不存在已经由 `409 snapshot_base_not_found` 说话；它同样认 `account_id`/`venue_id` 这一对（只给一半是 400，那一格写在第二张表里）。这条路由**不产出 `404`**：`publish_snapshot` 把基准历史与当前快照同批写入，基准查得到就一定有当前快照，所以实现里那条 `404 snapshot_not_found` 是到不了的分支，已随 #205 删掉——两张表也就不用再去解释一个永不返回的码。反过来，那四条整体现读端点里只有两条**没有收窄键**：`/scheduler/runs` 与 `/control/audit` 带任何查询串一律 `400`（第一张表原先在 `/account/ledger` 那一格写着 `[?…]`，而那条臂从头到尾没读过 `query`，递来 `?account_id=shadow` 只会把默认账户的流水念成 shadow 的流水，V13 R2 第十三遍 #205）。`/account/ledger` 与 `/reconcile/reports` 则反过来**真能收窄**——#205 的口径对它们当时只是暂时正确（两条臂确实都没读 `query`），而数据模型一直在：`LedgerEntry` 带 `account_id`，`ReconcileReportSnapshot` 还多带 `venue_id`，只有 `JobRun` 与 `AuditRecord` 两格什么账户列都没有。R31 把这两条臂接上 `read_scope::ScopeFilter::from_query`，键形状合法但过滤不出条目时回 `200` 空数组，而不是投影那族的 `404 account_projection_not_found`：它读的还是整份现读模型，收窄只发生在结果集上，跟「这份部署里没有这个账户的投影」不是一回事，两条通道各说各话才不会把「空流水」读成「这个账户不存在」；账簿那一格只认 `account_id` 不是取巧，`LedgerEntry` 没有 `venue_id` 字段，递 `?venue_id=` 照样 `400` 并点名被拒的那把键。

账户快照的八个汇总钱字段（`raw` 是整数量纲）在本构建分两类：有算点的是 `equity_raw`、`available_raw`、
`fees_raw`、`realized_pnl_raw`、`unrealized_pnl_raw`；**账户级无生产者字段**：`margin_raw`、`frozen_raw`、
`funding_raw`。后三格在 `/account/snapshot` 与其 envelope 里恒为 `null`（`/account/balances` 只公布其中
`margin_raw` 一格，同样是 `null`）：`null` 的含义是"这一层没有算它"，不是 0，也不能读成"这个账户没有
保证金占用 / 没交过资金费"。这份名单不靠手抄维持——门禁 `account_money_field_registry_check`
把它与协议里的 `Option<i128>` 声明、`schemas/account-snapshot-v1.json` 的逐字段 description、
`maturity/capabilities.yaml` 的逐字段 limitation 与读侧 null 用例的点名集合逐条对齐，任一侧改口即红（V13 R1-A5；
V13 R26 把 `realized_pnl_raw`/`unrealized_pnl_raw` 接上生产者，名单从五格收到三格）。
`/account/snapshot/diff` 的八个汇总钱标量不在那五条差分数组里：`diff` 只按键集合化现金账簿与四张表，账户级标量整格走 `replacement`——两侧 `scalar_hash` 相同它就是 `null`。把这一格读丢的客户端会拿着基线的权益、费用与对账结论去核对目标状态哈希，`apply` 末尾那道 `target_state_hash` 比对正是为这种情况准备的；`qx-cli ecosystem` 的协议段就是按"改一格权益"跑这条回路。注意 `replacement` 是 `SnapshotDiff` 上的**私有字段**：线格式里有它，crate 外的 Rust 代码却点名不了它，所以跨语言读者只认这份文档（V13 R2 第六遍）。

WebSocket 不占路由表：任何路径带 `Upgrade: websocket` 即在 HTTP 分派前转交
`crates/qx-api/src/ws.rs` 的 `admit_websocket` / `serve_websocket`。升级判定只看请求头那一段
（首个空行之前），不看正文——按整份请求文本 `contains` 的旧写法下，一条正文里正好出现
`upgrade: websocket` 的 `POST /control/commands` 会被当成握手，命令体连同它的审计一起丢掉。
这条通道与 HTTP 读面共用同一枚 `qx_api_requests_total` 与同一只限流桶：超额同样是 429
`api_rate_limit_exceeded`，桶自己读不到状态同样是 503 `api_rate_limit_backend_unavailable`。
握手需要 `Sec-WebSocket-Key`；准入判定全部排在写下 `101` 之前，所以这一层还说得出口 HTTP 状态码——
装了 `api.cors_allowed_origins` 而请求带着名单外的 `Origin` → 403 `cors_origin_not_allowed`（浏览器
不把 CORS 用在 WS 握手上：它照发带任意 `Origin` 的 `Upgrade` 请求、只在响应侧拦，所以这份名单必须由
这一支自己问过，否则 HTTP 侧的名单挡不住任何跨源页面读事件流；完全不报 `Origin` 的原生客户端照常
握手——它本来就不在 CORS 的威胁模型内，把它拒掉只会先杀掉自家 CLI 与探测脚本）；
启用访问策略而未通过证书识别 → 403 `authenticated_operator_required`（同时计入
`qx_api_authentication_rejected_total`）；`?account_id=`/`?venue_id=` 只给一半、或 `?after=`
不是十进制无符号整数 → 400；这三个名字之外的查询键（拼错的 `acount_id` 就在其中）→ 400 且正文点名那把键，与 HTTP 那几条读面同一口径（V13 R6）；缺 `Sec-WebSocket-Key` → 400 `missing_websocket_key`（此前这一判定排在
`serve_websocket` 里、已在写下 `101` 的边上，客户端只能拿到一根被掐断的套接字而没有状态码；V13 R4 把它
提到 `admit_websocket`，让握手之前的每一支都还能说 HTTP）；带键但这份部署里没有该投影 → 404 `account_projection_not_found`；
`after` 越出这份日志的窗口 → 409 `event_cursor_requires_snapshot`。判定通过后依次下发
`connected`、可选的 `snapshot`、按 `after` 从**作用域**总线取出的首批 `events`（批次为空就不发
这一帧），再按 100ms 轮询逐条推 `event`；每条事件外面套的投影信封与 `/events/live` 出自同一份
实现，`account_id`/`venue_id` 与 `after` 在两条读链上是同一个口径，客户端从 HTTP 换成 WS 不会
静默读到另一个账户的事件流，也不会每次连上都被重发一整份日志。
退出条件有六类：游标过旧/超前发 `{"type":"resync_required"}` 后关闭、读到客户端 close 帧后关闭、
对端 EOF 或 `ConnectionReset` 后关闭、服务端监听循环按停机请求收摊时先发 `{"type":"server_shutdown"}`
再关闭（`serve` / `serve_tls_mtls_with_stores` 一退出就置位同一枚令牌，已在飞行中的会话在下一轮
100ms 轮询里读到它就结束，所以 `Ctrl+C` 不必等前端自己关连接，V13 R2 #218）、以及两个方向同时
静默满 `WS_MAX_IDLE_ROUNDS`（18000 轮 × 约 100ms ≈ 30 分钟）时发 `{"type":"idle_timeout"}` 后
关闭。前端把"连接被关闭"读成故障还是读成计划内停机，看的就是最后这两条：收到 `server_shutdown`
或 `idle_timeout` 都是计划内，重连即可。空闲上界不是给会话加寿命，而是让下面那条连接预算真的收得
回来——对端半开（不发 FIN、也不再写一个字节）时读永远超时、写永远成功，停机令牌之外的出口一个都
不会触发，那条线程和它占的一格预算就永久留在账上。本机明文绑定下这条通道可接受，公网暴露前仍必须
先接上层代理。

`serve` 暴露的端点就是下表这些，未列出的路径一律 404。表里第一列的 `METHOD 路径` 必须与
`crates/qx-api/src/lib.rs` 的路由集合逐一相等（门禁与逐张表的用例各守一侧，见下「端点表按张核对」），而第一列方括号里的查询串是名单不是提示：那几条带键入口只认列出的键名，名单外的键一律 `400`（V13 R6）；表里那五条**全局出口**（`/health`、`/ready`、`/metrics`、`/schema/account-snapshot-v1`、`/schema/contract-matrix`）不在任何名册里，走的是 `crates/qx-api/src/admission.rs` 的 `refused_query_param` 那句 `accepted_query_params(route)?`（名册里没有这条路径就整支不判，`read_scope` 是名册的单点）——它们**不判查询串**，给它们带 `?account_id=` 既不会换来 400，也不会换来任何按账户收窄的数据（这五条本来就不读收窄键，判它们没有意义）。所以「名单外一律 400」这条口径的覆盖面是 12 条读面入口 + WS 那一支，剩下 5 条是这里明写的边界，不是漏网：

| 端点 | 语义 | 非 200 口径 |
| --- | --- | --- |
| `GET /health` | 进程存活 | 429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /ready` | 依赖就绪：控制面存储、已声明研究快照、生产凭据/冻结规格、worker 指标 down/stale、投影缺口 | 503 未就绪（第二格那些条件）；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /metrics` | Prometheus 文本，按 LF 逐行（见下「指标出口是逐行的」），追加 worker 指标 | 429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /schema/account-snapshot-v1` | 账户快照 v1 JSON Schema，就是 `schemas/account-snapshot-v1.json` 那一份（编译期内嵌，不是第二份手抄） | 429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /schema/contract-matrix` | 稳定契约的命名转换矩阵，公布的就是 `qx_core::contract::CONTRACT_MATRIX` 那一份（编译期取用，不是第二份手抄）；与真实代码逐条对账由门禁 `contract_matrix_check` 看守 | 429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /account/snapshot[?account_id=&venue_id=]` | 账户快照 JSON；不带键时读默认账户=配置里第一个真有日志的账户 worker | 400 参数非法（键形状不合法，或点了这条入口不认的查询键——正文点名那把键，下同）；404 `snapshot_not_found`；404 `account_projection_not_found`（带键但这份部署没有该投影）；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /account/snapshot/envelope[?…]` | 投影信封（快照 hash 与 lineage） | 400 键形状非法或名单外的查询键；404 `snapshot_not_found`；404 `account_projection_not_found`；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /account/snapshot/diff?base_hash=[&…]` | 与历史基线快照的差异 | 400 参数非法（base_hash 缺失或非无符号整数，收窄键只给一半，或点了这三把之外的查询键）；409 `snapshot_base_not_found`（基准缺失与那条投影不存在是同一条码）；无 404 分支，判据与口径的来由见上段正文（V13 R2 第十三遍）；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /account/orders[?…]` `GET /account/positions[?…]` | 快照里的订单表/持仓表摊成数组 | 400 键形状非法或名单外的查询键；无快照时 200 空数组；带键但无该投影 404 `account_projection_not_found`；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /account/balances[?…]` | 四个钱字段原样，未计算的是 `null` 而不是 0 | 400 键形状非法或名单外的查询键；带键但无该投影 404 `account_projection_not_found`；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /account/ledger[?account_id=]` | 每次请求现读账户日志，启动之后落盘的读得到；`?account_id=` 在结果集上按账户过滤 | 400 带了这条入口不认的查询键（`LedgerEntry` 没有 venue_id 字段，`?venue_id=` 就落在这支，正文点名被拒的那把键）或空串键；过滤不出条目时是 200 空数组，不是 404（V13 R31）；503 读不到即报错，不念开机那份；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /reconcile/reports[?account_id=&venue_id=]` | 每次请求现读对账报告，启动之后落盘的读得到；两把收窄键都认时是 AND | 400 带了这条入口不认的查询键或空串键；过滤不出条目时是 200 空数组，不是 404（V13 R31）；503 读不到即报错，不念开机那份；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /scheduler/runs` | 每次请求现读调度记录，启动之后落盘的读得到；整份现读模型 | 400 带任何查询串——这条入口没有收窄键（`JobRun` 没有账户列），正文点名被拒的那把键（V13 R2 第十三遍 / R6）；503 读不到即报错，不念开机那份；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /events[?after=&account_id=&venue_id=]` | 投影事件全量，或 `after` 游标之后的增量 | 400 游标形状非法或名单外的查询键；409 `event_cursor_requires_snapshot`；带键但无该投影 404 `account_projection_not_found`；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /events/live[?after=&…]` | 事件总线现读增量，游标口径与上一行同一条实现 | 400 游标形状非法或名单外的查询键；409 `event_cursor_requires_snapshot`（游标过旧/超前，含空日志）；404 `account_projection_not_found`；500；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `GET /control/audit` | 控制面审计流水 | 400 带任何查询串——这条入口同样没有收窄键（`AuditRecord` 没有账户列），与上一行同属整体现读面，正文点名被拒的那把键（V13 R2 第十三遍 / R6）；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `POST /control/commands` | 提交控制命令；启用访问策略时 operator 身份必须来自认证边界 | 400 请求体不合法（缺审计字段，或该命令类型在当前构建里没有派发者）；403 未认证 `authenticated_operator_required`／已认证但策略给不出权限 `forbidden`；409 命令被控制面拒绝（`ControlError` 的 Debug 形态，四个变体名见下段）；503 队列不可用 `control_state_unavailable`；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `POST /app/validate-dataset` | 应用层用例 ValidateDataset：这份数据能否支撑一次 Bar 回测（请求体是 DatasetSpec JSON） | 非 200 状态码由响应体里的类别字段派生（crates/qx-api/src/app_surface.rs 的类别映射是唯一一份）：400 输入不合法；404 数据取不到；422 档位不足；403 权限不足；409 冲突；503 超时或存储故障；500 内部不一致。错误体是与 CLI/Python 逐字节相同的应用层错误文档，不是读面那套错误码形态；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `POST /app/backtest` | 应用层用例 RunBacktest：跑一次 Bar 回测并落四份产物（请求体是 BacktestSpec JSON） | 与上一行同一份类别映射；同一运行身份已存在且身份不同是 409（不覆盖历史）；产物写在请求体点名的产物目录下，是**服务进程**的文件系统——所以这条入口与 /control/commands 同一把锁（配了访问策略的部署里都要 operator 身份）；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |
| `POST /app/verify` | 应用层用例 VerifyRun：四份产物是否互相印证（请求体取上一行的响应体） | 与上一行同一份类别映射；产物缺失或互相矛盾是 200 加一个为假的裁决（不是非 200——「复核不通过」与「复核跑不起来」是两件事）；429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable` |

`POST /control/commands` 那一格的 409 是控制面按 `ControlError` 的 Debug 形态回报，四个会变 409 的
变体名是 `DuplicateRequest`、`DuplicateCommand`、`UnknownCommand`、`AlreadyFinal`；
限流在鉴权之前判定，所以 429 与 503 是**全表每一行**都有的出口，而不是某几条入口的特产：超额
429 `api_rate_limit_exceeded` 说的是"你的额度用完了"，限流后端自身故障 503
`api_rate_limit_backend_unavailable` 说的是"这道闸门自己读不到状态"。两格逐行都写，是因为它们只差
一个状态码而含义正好相反——与 #172 在风控端口上把"拒绝"与"端口坏了"分成两条通道同一口径。立案时
`GET /health`、`GET /metrics`、`GET /schema/account-snapshot-v1` 三格写的是 `—`，而 `/health` 那一格
的语义还写着"永远返回 200"：探活脚本照那句话写，收到的却是一个在文档里查不到出口的码。这一格由
`every_semantics_row_declares_the_shared_rate_limit_exits` 逐行核对（漏一行红一行，把某一格改回 `—`
同样红）。状态行的原因短语与响应体也必须说同一句话：`403` 与 `503` 过去共用 `_ => "Internal Server
Error"` 那一句，客户端按状态行分支会把"没权限"与"闸门坏了"听成同一句 500 的话；源码侧由
`status_line_reason_names_cover_every_code_the_read_face_emits` 核对覆盖与两两不同，线上侧由
`crates/qx-api/tests/status_line_and_limiter_exit.rs` 用真 socket 逐个出口核对。启用访问策略时，除
`/health`、`/ready`、`/schema/account-snapshot-v1`、`/schema/contract-matrix` 外都要求已认证 operator，否则 403
`authenticated_operator_required`。

控制命令的受理面按 `kind` 分档（V13 R2 #188），本构建里两档的名单是：

- 有派发者、能被受理的：`SubmitOrder`、`PauseStrategy`、`ResumeStrategy`。
- 没有派发者、提交即以 400 拒绝的：`ChangeRiskLimit`、`CancelOrder`、`ReconcileAccount`、`RetryJob`、`SwitchVenue`。

这条闸门只在提交入口判，不进存档校验：队列与审计里已经落盘的历史命令重启后仍然读得回来，
构建不受理与存档坏了是两条通道。上面两行名单由用例
`control_plane_acceptance_kinds_are_listed_in_the_ops_doc` 与 `CommandKind::executed()`
双向核对：给某一颗改档位而不改这里，用例红在那颗名字上；把同一颗写进两行也会红。

### 端点表按张核对

「路由集合逐一相等」这件事由两处分别把守，而**后者是必需的**：门禁 `api_surface_doc_check`
把全文所有 `` | `METHOD /path` `` 形态的行收成一个**并集**再比集合，所以「某一张表少了一行」
它看不见。第八遍实测到这份盲区还是**不对称**的（`logs/s155_*.txt`）：

- 删上面那张（写「返回」形状那张）的 `/events/live` 整行 → 门禁 `exit=0`，并原样印
  `[PASS] deploy/README.md 的端点表与 qx-api 路由集合完全一致`，因为本节这张表里那条
  `` `GET /events/live[?after=&…]` `` 还在并集里。
- 删本节这张表的 `/control/audit` 整行 → 门禁确实红，报错点名 `只在代码 [('GET', '/control/audit')]`，
  因为上面那张表把四条路径并列在同一格、只有第一条带 `GET` 前缀，并集里就没有这条路径了。

也就是说门禁只守得住「并集恰好还留着 METHOD 形态那一条」之外的情况。用例
`each_endpoint_table_lists_exactly_the_dispatch_routes` 不做并集：两张表各自与 `handle_inner`
的分派集合比相等，任一张表整行删一条路由都红在那张表的名字上。第八遍那两发变异各红一次、逐字节还原；
第九遍起这条按张核对住在三个用例文件里（`api_endpoint_table_routes.rs` 3 条、`api_response_field_doc.rs` 3 条、`api_doc_cross_references.rs` 1 条），共 7 条用例，本轮定向实跑 `running 7 tests`、`test result: ok. 7 passed`（`logs/s190_*.txt`）；核对发布物载荷口径的另两组在 `artifact_identity_doc.rs`（2 条，含 #181 的 wheel staging 先后顺序）。
同一张表第三格（「非 200 口径」）里点名的**错误码名与三位状态码**也由
`crates/qx-cli/src/tests/api_endpoint_table_routes.rs` 的
`non_200_column_names_match_the_read_face_implementation` 双向核对：读面写进 error 字段的码名
必须在文档里有名字，文档那一格承诺的码名与状态码也必须实现真产得出。第八遍立案时这一格是
纯手抄（#182）：读面 8 个错误码名里 `forbidden`（已认证但策略给不出权限的 403）与
`control_state_unavailable`（队列不可用的 503）在全仓文档里没有任何名字，而
`POST /control/commands` 那一格原先只写「403；503 队列不可用」。同一条路上有两个不同码名的
403，客户端按 error 分支写代码就会把「没登录」与「没权限」合成一件事 —— 现在那一格两个码名
都点到了，409 也按控制面 `ControlError` 的 Debug 形态如实写明（四个变体名不在「小写码名」这一类核对口径里，判据按类型面放行）。
这两张表与**本小节自己**住在哪一章，第九遍起也有一条常驻判据：`endpoint_tables_and_their_check_section_live_under_paper_api` 要求两张表的表头、
「### 端点表按张核对」这条标题、以及上面那句「未列出的路径一律 404」四者都落在 `## Paper API` 与
下一个一级标题之间，且各自在全文只出现一次（在别的章节再抄一份表会让读者面对两套口径）。立案时（#183）
这一整段其实挂在 `## Outbox 与 NATS JetStream` 下：上面两条判据都按整篇文档搜表头取数，所以
「搬错章节」在旧口径下永远不会红，而运维在 API 那一章读不到「非 200 口径」那一格。
方向词还会随搬家翻面：#183 把第二张表搬上来之后，那张表里 `/metrics` 那一格原先写的是朝上的
指代，而被点名的「指标出口是逐行的」此时已经在那张表下面——#183 的判据只管三样东西住在同一章
内、相对次序不变，看不见这句话。第九遍起这条指代有常驻判据
`directional_cross_references_point_the_right_side`：文档里每一处按「」点名的小节，被点名的标题
必须真的落在方向词声明的那一侧，且不能上下各有一份；点名点到不存在的小节同样红。

表格之外还有散文句子里的路由字面量。门禁 `api_surface_doc_check` 与本节上面两条判据都只吃表格行（每行以竖线开头），所以正文里写歪的一条入口不会被任何一侧抓到。第二十二遍实测（`logs/s523_pass22_probe_doc_routes.txt`；改后同口径复测见 `logs/s531_pass22_doc_routes_after.txt`）：本文 44 条 `METHOD 路径` 字面量里有 14 条写在两张表之外，其中「事件日志的读面与写面」那一节把按账户刷新快照的入口写成了一条带路径参数的复数形态，而这条路径从来没有被分派过，真形状是 `GET /account/snapshot?account_id=&venue_id=`。读者照那句话写客户端只会拿到 404 兜底，而这份文档是这套框架对外唯一的读面说明。现在由 `crates/qx-cli/src/tests/api_doc_prose_routes.rs` 的 `prose_route_literals_outside_the_tables_are_served_too` 把两张表之外的每一处反引号路由字面量都拿去与 `handle_inner` 的分派集合核对，并要求全文不再出现那种复数形态；它按本轮实测的 15 条表外字面量钉取数地板，防止扫描口径失灵后整条判据空转。

### 请求时间戳的三个时钟域

一条 HTTP 请求在这份构建里只有一个时间来源：`serve` 与 `serve_tls_mtls_with_stores` 每接受**一条
连接**现取一次 `runtime_timestamp_ms`，把它交给 `handle`（V13 R2 第十六遍 #221）。立案现场是这个戳
取在监听入口、之后再不更新，于是每条连接、每条控制命令、每个游标共用进程启动那一刻的同一个数。
共用同一个来源不等于共用同一个时钟域，三条通道各自是：

- **请求与审计：epoch 毫秒。** `handle` 的 `ts` 就是这个域，它原样进 `ControlPlane::submit_as(.., ts)`
  写出的 `AuditRecord.ts`，运维在 `/control/audit` 里读到的也是它。
- **限流桶：epoch 秒。** `handle_inner` 在把 `ts` 交给桶之前换算一次（`rate_limit_bucket_seconds`），
  所以 `refill_per_second` 说的"每秒"就是桶那一格里的一秒。换算缺失时"每秒 100 次"实际是"每毫秒
  100 次"，`DEFAULT_RATE_LIMIT_REFILL_PER_SECOND` 声明的政策与真实政策分了家，任何持续流量都会读成
  "额度用不完"；反过来，桶若读到一个定格的戳，第一次见底之后就再没有回血的能力。
- **命令租约：epoch 秒。** 入队走 `qx-cli` 的 `lease_clock(ts)`。它与限流桶是两处各除各的 1000，
  不是同一个换算点，所以跨层看数字时别把这两个"秒"读成同一个字段的两种写法。

判据分布：前两条在 `crates/qx-api/tests/request_timestamp_clock.rs`——
`the_request_timestamp_is_read_per_connection_not_per_server` 用真 socket 走两条连接，比对审计里的
`ts` 是否随连接前进；`the_limiter_clock_domain_is_bucket_seconds_and_the_bucket_refills_across_one`
用共享文件桶核"同一格内见底、跨过一格回血"。装配侧由 `qx-cli` 的
`both_serve_branches_pass_a_live_clock_to_the_api_worker` 按源码点名两条分支传的是那只钟本身、不是
它取出来的结果；租约域本身由 `crates/qx-cli/src/tests/lease_clock_domain.rs` 钉。

已知边界（如实写出）：`now` 的粒度是**每条连接一次**，不是每条请求一次。本构建的每个响应都带
`Connection: close`，一条连接只服务一个请求，所以这里读不到差异；把 HTTP 层改成 keep-alive 复用
时，必须连这条口径与上面那两条用例一起重看。

### 指标出口是逐行的

`/metrics` 的正文按 LF 分行，`# HELP`、`# TYPE` 与每条样本各占一行。这不是排版偏好，是这条入口
唯一的可读形态，也是它此前唯一坏掉而无人出声的地方：第七遍的发布面实测（`logs/s143_*.txt`）里
`qx-cli.exe serve` 的 `/metrics` 返回 200、462 字节，行分隔却是"渲染出字面反斜杠 + n"的转义文本，
整份正文是一行，一条样本都解析不出来。Prometheus 抓取端在这种正文上解析失败**不报错**，后果不是
看到错误，而是所有以这些指标为条件的告警永不触发——监控看着在跑，实际是瞎的。

- API 自身固定四条样本：`qx_api_requests_total`、`qx_api_rate_limit_rejected_total`、
  `qx_api_authentication_rejected_total`、`qx_api_command_enqueue_failures_total`，各带一行 `# HELP`
  与一行 `# TYPE`。第四条是第十二遍 #189 补上的：`POST /control/commands` 在命令已经落盘后把队列写
  失败原先是 `let _ =` 丢掉的，进程内不留痕迹；现在它计入这条计数，并在 stderr 打一行
  `控制命令入队失败，等 worker 补入`。202 回执不变——控制面按 `pending()` 补入队列是 worker 每个
  tick 都做的工作，一次入队失败最坏是延迟一个 tick，不是丢命令。
- `worker-metrics/<worker_id>.prom` 的聚合**追加**在这四条之后，不是替换；带 worker 标签的
  `qx_worker_up{worker="<worker_id>"} 0|1` 是线上形态，不是标量。
- 这一族指标现在有出口了，而且只在 paper 侧：`crates/qx-cli/src/pipeline_metrics_report.rs` 的
  `PipelineMetricsReporter` 是 `LiveEventPipeline::metrics()` 在生产里唯一的读者。它按 **worker 进程**
  累计（不是按 pipeline 对象），paper worker 每个 tick 把这一轮用完的行情/多腿恢复 pipeline 各
  `absorb` 一次，再随 `qx_worker_up` 一起把六个 `qx_pipeline_*_total` 写进
  `worker-metrics/<worker_id>.prom`，`/metrics` 聚合时原样透传正文、只改写 `qx_worker_up` 那一行。
  因此抓取端看到的是带 `worker`/`account` 两条标签、单调递增的累计量，**唯一的归零边界是进程重启**——
  这正是当初把"按请求各开一个 pipeline"的读路径挡在出口外的原因，对象级累计直接印成 `_total`
  会给抓取端一条每次请求归零的「累计计数」，比不印更糟。这份 `.prom` 的真形态（用例
  `paper_worker_publishes_pipeline_counters_to_its_metrics_file` 逐行取的就是它）：

  ```text
  qx_worker_up{worker="<worker_id>",account="<account_log>"} 0|1
  qx_worker_heartbeat_timestamp_seconds{worker="<worker_id>",account="<account_log>"} <秒>
  qx_pipeline_ingest_attempts_total{worker="<worker_id>",account="<account_log>"} <计数>
  qx_pipeline_ingested_events_total{worker="<worker_id>",account="<account_log>"} <计数>
  qx_pipeline_deduplicated_events_total{worker="<worker_id>",account="<account_log>"} <计数>
  qx_pipeline_transient_retries_total{worker="<worker_id>",account="<account_log>"} <计数>
  qx_pipeline_refreshes_total{worker="<worker_id>",account="<account_log>"} <计数>
  qx_pipeline_failures_total{worker="<worker_id>",account="<account_log>"} <计数>
  ```

- 两个边界要说清：`--once` 跑完后那份 `.prom` 的 `qx_worker_up` 是 0，计数仍在正文里，抓取端会把它
  连同 `qx_worker_metrics_stale{worker="…"}` 一起读到（心跳超时后的形态）；仍**未接线**的两半是
  `crates/qx-cli/src/api_service.rs` 的请求内只读 pipeline 与 live venue worker（后者走
  `PipelineStorage`，不经 `open_account_pipeline`），登记在 `maturity/capabilities.yaml` 的
  `pipeline_metrics_publish_per_worker_process_only`。这条登记、`crates/qx-runtime/src/lib.rs` 的
  公开面说明与上面那份带标签的样本形态，由判据 `pipeline_metrics_publication_and_docs_move_together`
  与生产读者清单双向钉住（V13 §9.15 #178 立案 → §9.27 接线）。
- 哪些入口的写入**不在**这族样本里，现在是机器清点而不是手工名单：`crates/qx-cli/src/tests/pipeline_metrics_open_sites.rs` 扫生产源码里每一处 `open_account_pipeline(` 与 `open_runtime_pipeline(` 站点（本轮实测 14 处），按函数分两类点名：paper 的两条 worker 循环（1 + 3 处站点，并账次数必须与站点数**逐函数相等**——少一次是漏计、多一次是双计）与 8 项豁免（1 处统一入口向下一层的内部委托、5 处按请求或装配期打开的只读站点、`run_paper_pipeline_once` 与 `run_paper_submit_order` 这两个一次性验收入口）。豁免项必须在 `pipeline_metrics_report.rs` 的模块文档里逐名点名，新增站点不登记就红。因此 `paper-submit-order` 的口径要说清：它写入的事实照旧进 EventLog，但**不在这族计数里**——它以 `paper-execution` 的名义领取租约，把计数并进 worker 只会让那份按进程累计的量更假（V13 §9.28）。
- 逐行这条口径由判据 `prometheus_exposition_is_line_separated_at_both_ends` 两头各扫一次：真驱动一次
  `/metrics` 按行解析，再把生产源码里"看起来是样本模板"的行扫一遍。变异验证见 `logs/s151_*.txt`
  （当时两处出口各改坏一次，红/绿成对）。第二十遍接线时 `crates/qx-runtime/src/pipeline.rs` 的
  `PipelineMetricsSnapshot::to_prometheus` 那份**无调用者的第二实现**已随接线删除：六个计数的渲染
  归 worker 侧的 `.prom` 出口一处，`PipelineMetricsSnapshot` 本身仍是 `metrics()` 的返回类型。

### 事件游标的冷启动口径

`?after=` 的含义是**事件序号**，不是这条日志的下标；缺省（不带 `after`）表示"从头给"。
服务刚起来、日志还没产出任何事件时，实测答复是固定的三格（`logs/s143_*.txt`）：

- `GET /events` 与 `GET /events/live` 不带游标 → `200` 加空数组：日志为空不是错误。
- 同两条入口带**任何**数值游标（`?after=0` 也算）→ `409 {"error":"event_cursor_requires_snapshot"}`：
  空日志里没有任何序号比 0 小，游标无从对齐，所以这里说的是"这个游标我无法解释"，不是"没有新事件"。
  把它读成空批次的客户端会停在旧状态而不自知，这正是 V12 R4-g 统一两条链口径时要堵的那一格。
- 与之配套的"服务端是空的"信号是 `GET /account/snapshot` → `404 {"error":"snapshot_not_found"}`；
  差分的基准缺失则是 `GET /account/snapshot/diff?base_hash=0` → `409 {"error":"snapshot_base_not_found"}`。
- 游标被裁剪到日志之前、或超前于下一个序号，同样回 `409 event_cursor_requires_snapshot`；
  非十进制无符号整数才是 `400`。

### 浏览器准入（CORS / 预检 / 并发连接预算）

这三样都住在 `crates/qx-api/src/admission.rs`，判定点在 `dispatch_request` 与它调用的
`admit_websocket`（WS 那一支问的是同一个 `CorsPolicy`，只是自己调用、不等预检那条臂）——也就是
限流之后、`handle_inner` 的路由分派之前，所以它们对**每一条**入口生效，不是某几条路由的特产。

`api.cors_allowed_origins` 是一份**精确**源名单，每项形如 `https://host[:port]`：不接受 `*`、
不接受通配、不接受带路径或查询串的写法，端口写了就必须是非零十进制（名单本身的校验在
`CorsPolicy::parse`，运行时配置只透传，不另立一套口径）。`config validate` 走的是同一个
`CorsPolicy::parse`（经 `qx_api::validate_admission_config`，因为 `qx-runtime` 的配置校验
不许反向依赖 `qx-api`），所以坏源在部署前就报成一条 `api cors_allowed_origins 里的 …`，
不必等 `serve` 起不来才看见。留空即"这份部署不开浏览器准入"：
响应不带任何 `Access-Control-*` 头，`OPTIONS` 照常走分派落到 404。装了名单之后，每条出口
（含升级前的 400/403/404/409/429、预检本身、以及正常分派的响应）都带同一份
`Vary: Origin` + `Access-Control-Allow-Origin: <请求里的那个源>`——漏一条就是"这条路径跨源读不到"，
而浏览器给的报错只有"被 CORS 拦了"，看不出是哪一支没带头。合法的预检（`OPTIONS` 且带
`Access-Control-Request-Method`）回 **204** 空正文，`Access-Control-Allow-Methods` 固定
`GET, POST, OPTIONS`、`Access-Control-Allow-Headers` 固定 `Content-Type`、`Access-Control-Max-Age`
固定 600；源不在名单里的预检回 403 `cors_origin_not_allowed`。允许头这一格**不回显**
`Access-Control-Request-Headers`：回显等于让调用方往响应头里写任意字串，而 operator 身份只来自
mTLS 证书、从不来自请求头，所以这个固定值不会挡住任何已支持的调用。

出厂模板这一格写的就是 `[]`（`deploy/qianxing.runtime.production.example.json:20`），所以
**照模板起起来的部署对任何浏览器源都是关着的**：`config validate` 不会报——`[]` 正是那份合法
的中性写法，不是「配置缺失」；响应不带任何 `Access-Control-*`；浏览器侧只看得到一句「被 CORS
拦了」，看不出名单是空的。Web / 桌面客户端要在浏览器上下文里直连这套 API，operator 必须先把
真实源写进 `api.cors_allowed_origins`（形如 `["https://ops.internal.example"]`）再重启；
没有可继承的默认值，也没有别的开关能绕过——名单是精确匹配。

`api.max_concurrent_connections` 是飞行中连接的条数上限；留空则用 `qx-api` 自己的默认值
（`DEFAULT_MAX_CONCURRENT_CONNECTIONS`，256），这个数只有那一个定义点，配置里不写第二份。
一条连接一个线程，所以上限同时是线程数上限。超出时**当场拒**而不是排队——排队只是把"拒绝"变成
"更慢的接受"，排着的连接照样各占一个已 accept 的套接字与一份读缓冲，压力不会因为排队而消失。
被拒的连接在读到任何路由之前就拿到 503 `connection_budget_exhausted`（正文形如
`{"error":"connection_budget_exhausted: limit=256"}`），同时 stderr 印一行「并发连接已达上限 …
（飞行中 …），拒绝新连接」——这两处就是这个数的读者，`/metrics` 不为它单开第五条指标。额度由
`ConnectionGuard` 的 `Drop` 归还，所以会话线程无论怎么退出（正常关闭、写失败、停机令牌、空闲上界）
都不会把额度留在账上；配 0 会在 `config validate` 就拒（与源名单同一个入口
`qx_api::validate_admission_config`，它调的是 `ConnectionBudget::new` 本身），因为那等于不接
任何连接。两格的中性写法（`[]` 与 `null`）由 `deploy/qianxing.runtime.production.example.json`
携带，所以它们的 JSON 形状每轮都被 `deploy_template_coverage` 那条读法解析一次。

查询串的百分号编码在这条边界上统一解码一次（`admission::percent_decode`）：`%XX` 与 `+`（当空格）
都认，解出来的字节必须是一串合法 UTF-8。编码非法时那一格回 400，正文写的是
「query string percent-encoding is malformed」这句话而不是一个码名。此前 `?account_id=main%zz`
会把半个转义原样当成账户号去查投影，读到的是 404「没有这个账户」，而真正坏掉的是请求本身——
一个 400 与一个 404 说的是两件事，合成后者就读不回来了。

### Web 控制台（`web/console/`，只读 + 控制面）

仓库自带一个控制台，落在 `web/console/{index.html,app.js,styles.css}`。它的**读面板**只调用
上表的 `GET` 读面端点（`/health`、`/ready`、`/account/snapshot`、`/account/balances`、
`/account/orders`、`/account/positions`、`/account/ledger`、`/reconcile/reports`、
`/scheduler/runs`、`/control/audit`、`/events`、`/events/live`）并接 WebSocket 增量。

它的**写面只有一条**（阶段四 M3' 控制面）：`POST /control/commands`。它**不直接下单**——
下单要走这条命令，由控制面**受理 → 判定执行者 → 走到终态退场**，页面把三个阶段都印出来。
受理回 `202` 只表示"收下并落了审计"，**不是**"已经执行"；②③两阶段从 `/control/audit`
回读（`CommandStatus` 走到 `Executed` / `Failed` 即终态退场）。本构建里只有
`SubmitOrder` / `PauseStrategy` / `ResumeStrategy` 有派发者，其余类型只会停在 `Accepted`。

**身份边界**：命令体里的 `operator_id` 是**审计字段，不是认证**。启用访问策略的部署里，
服务端会用 mTLS 认证边界上的身份覆盖它，且**认证边界拿不到身份时直接回
`403 authenticated_operator_required`**——页面不能自声明身份。控制台把这条如实印在界面上。

它是一份静态页面，不占服务端路由：后端仍用 `qx-cli serve <runtime.json>` 起，控制台另用
任意静态服务器（例如仓库根目录下 `python -m http.server 5173 --directory web/console`）
或直接打开文件。连接入口已做本机回环限制：只允许 `127.0.0.1` / `localhost`，
命令提交前会再次检查；这不是认证授权，只是避免把**没有会话/CSRF 的静态页**误当成可远端使用的控制面。
需要真正的同源形态（服务端会话 + CSRF + 身份注入）就用下一节的 `qx-cli console`。

因为控制台与 API 不同源，浏览器会先做 CORS：**必须把控制台的源逐字符写进
`api.cors_allowed_origins`**（形如 `["http://127.0.0.1:5173"]`，见上节"浏览器准入"），
否则页面上的每个请求都会被浏览器拦下，而后端日志里什么都看不到——那不是服务端故障。
控制台顶部的说明条把这条要求直接印在页面上。

`web_console_check`（门禁）把这份接线表钉住：`app.js` 点名的每个 API 路径都必须真的出现在
`qx-api` 路由表里；页面必须说明 `cors_allowed_origins` 与 `qx-cli serve`；唯一写请求必须是
`POST /control/commands`，不得直连下单端点。也就是说，"页面能打开"不算贯通，读面与唯一控制面
都必须接到真实后端契约上。

**版本化发布包（M4'/M5' 的可验收子项）**：推送与 Cargo / Python / baseline 三处版本一致的 `v*`
tag 时，发布流水线会额外生成 `qianxing-web-console-v<tag>.tar.gz`，内含 `index.html`、`app.js`、
`styles.css` 与 `release-identity.json`（版本、完整 commit、Schema Registry 版本、逐文件 SHA256、
`package_scope=static-assets-only`、`distribution_boundary=local-only`，以及**两组**能力字段：
`product_same_origin_bff` / `product_csrf` / `product_server_side_session` / `product_desktop_host`
说的是产品侧现状（前三者为 `true`，由下面那节 `qx-cli console` 提供；桌面 Host 仍为 `false`），
`sandbox_accepted` / `production_accepted` 两档外部验收仍为 `false`）。
归档本身进入 Release 的统一 `SHA256SUMS`，并附 build provenance；本地可用
`python tools/package_web_console.py --version 0.1.0 --commit <40位提交哈希> --output <输出路径>`
重建，并以压缩包内身份文件核对资产摘要。

**安全边界（逐条说清）**：**这个发布包**只是静态 UI，它自己没有浏览器会话、CSRF token 或权限代理层，
不要单独暴露到公网或当作生产交易控制台。**产品侧**的同源 BFF / CSRF / 服务端会话已经落地，落在
`qx-cli console`（见下一节）——它把这三件事放在服务端，浏览器只跟一个源说话；但它**只绑回环地址**、
是**单进程形态**，没有独立反向代理与 TLS/mTLS，因此不能替代跨主机的生产控制面。跨主机时仍须 mTLS
operator 身份与显式 CORS allowlist。**M4' 未关闭的部分**是桌面 Host 与多机反代形态；**M5'** 的
Paper/sandbox/production 外部验收也仍未关闭。

### 同源 BFF 控制台（`qx-cli console`）

上面那份静态页面单独部署时与 API 不同源。要把它变成"浏览器只跟一个源说话"，用 `qx-cli console`
起一条**同源 BFF**：同一个监听口既发静态三件，又把 API 代理在同一源上。会话、CSRF 与身份注入都留在
服务端，页面拿不到也不需要拿任何凭据。配置加一段 `api.console`（完整样例见
`deploy/qianxing.runtime.console.example.json`）：

```json
"api": {
  "bind": "127.0.0.1:18090",
  "console": {
    "bind": "127.0.0.1:18091",
    "static_dir": "web/console",
    "operator": "console-operator",
    "bootstrap_token_env": "QX_CONSOLE_BOOTSTRAP_TOKEN",
    "session_ttl_seconds": 3600
  }
}
```

启动（引导令牌**优先**从环境变量读，不进命令行、不进配置文件、不进日志）：

```bash
# 1) 准备引导令牌（≥16 字符）。想省事就照抄下面这条命令打印的那一行 export：
qx-cli console --generate-token
export QX_CONSOLE_BOOTSTRAP_TOKEN="<把上一步打印的令牌粘到这里>"
# 2) 启动（默认吃 deploy/qianxing.runtime.console.example.json）
qx-cli console deploy/qianxing.runtime.console.example.json
```

Windows PowerShell 等价写法：

```powershell
$env:QX_CONSOLE_BOOTSTRAP_TOKEN = "<至少16字符的引导令牌>"
.\target\debug\qx-cli.exe console deploy\qianxing.runtime.console.example.json
```

**忘了 export 也能起来（V13 R24）**：环境变量缺失或为空时，`qx-cli console` 会**临时生成**一枚一次性
令牌（96 个十六进制字符），在终端明确打印"已临时生成一次性引导令牌（进程退出即失效，重启换新）"，再把
入口 URL 印出来。这枚令牌随进程生灭、不落盘，只服务本机回环控制台的首次引导；它**不替代**运维显式配置
的长期秘密——要长期用就把令牌放进环境变量（或由 Secret Manager 投影到该变量）。

**两条不启动服务的易用性入口（V13 R24）**：

```bash
qx-cli console --generate-token        # 只打印一枚令牌 + 可直接粘贴的 export 行，随后退出
qx-cli console --init my.runtime.json  # 写出一份就绪的运行时模板（拒绝覆盖已有文件），并打印下一步
```

`--init` 写出的模板与 `deploy/qianxing.runtime.console.example.json` **同形**（绑回环、令牌只给环境
变量名、正文无令牌字面量字段），因此可以立刻 `qx-cli console my.runtime.json` 起一条同源 BFF。

进程印出一次入口 URL（`http://127.0.0.1:18091/?token=…`）。运维点开一次，服务端校验令牌后签发两枚
cookie：会话 cookie（`HttpOnly` + `SameSite=Strict`，页面脚本读不到）与一枚**非 HttpOnly** 的 CSRF
cookie（页面读出来放进 `X-QX-CSRF` 头——双提交模式）。此后地址栏里不再有凭据。

边界（逐条都有门禁牙齿 `console_front_check` / `console_usability_check` 守着）：

- **只绑回环**：`api.console.bind` 必须是 `127.0.0.1` / `::1`，拓扑校验在启动前就拒绝可路由地址。
  这一层没有 TLS、没有 mTLS、没有运维审批，绑到外部接口等于把控制面交给任何能连上的人。
- **非 GET 必须带 CSRF 头**且与会话记住的那一枚逐字符相等，还要过同源校验（带 `Origin` 的请求必须与
  `Host` 同源）——两道独立的锁，任一道都不许放宽成"有头就过"。
- **身份由服务端注入**：`handle_inner` 收到的是会话里那份 operator；命令体里的 `operator_id` 不被采信。
- **令牌来源**：优先从 `bootstrap_token_env` 点名的环境变量读；配置里只有变量名、没有令牌字面量字段。
  缺失时临时生成一枚（随进程生灭、不落盘），但长期使用仍应显式配置——命令行会进 shell 历史、配置文件会进版本库。
- **刻意不做**：独立反向代理、客户端证书持有、多机部署形态（那属于部署件，不是代码）。

#### 作为独立服务运行（systemd / Windows 服务）

控制台是**一个长驻进程**，可以直接交给系统服务管理器托管（进程生命周期、开机自启、崩溃重启、日志归集
交给 systemd / SCM，而不是 `nohup`）。**独立服务**指的是"把这条进程交给服务管理器"，**不是**"开放到网络"：
下面两段的 `bind` 仍是回环，服务管理器只负责把进程拉起来、把它读的环境变量喂给它。

Linux（systemd，`/etc/systemd/system/qianxing-console.service`）：

```ini
[Unit]
Description=Qianxing same-origin BFF console (loopback)
After=network-online.target

[Service]
Type=simple
WorkingDirectory=/opt/qianxing
# 令牌走 EnvironmentFile（或 LoadCredential=），不写进 unit 文件——unit 会进版本库、也会被 systemctl cat 出来
EnvironmentFile=/etc/qianxing/console.env      # 内容：QX_CONSOLE_BOOTSTRAP_TOKEN=<≥16 字符>
ExecStart=/opt/qianxing/qx-cli console /opt/qianxing/deploy/qianxing.runtime.console.example.json
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Windows（用 NSSM 把 `qx-cli console` 注册成服务；NSSM 负责"服务"与"前台进程"的桥接）：

```powershell
nssm install QianxingConsole "D:\qianxing\qx-cli.exe" "console D:\qianxing\deploy\qianxing.runtime.console.example.json"
nssm set QianxingConsole AppDirectory "D:\qianxing"
nssm set QianxingConsole AppEnvironmentExtra "QX_CONSOLE_BOOTSTRAP_TOKEN=<至少16字符的令牌>"
nssm start QianxingConsole
```

**要"给第三方使用"还差什么（诚实说清）**：把回环控制台交给服务管理器**不等于**可以对第三方开放。
第三方访问要的是**跨主机的生产控制面**，那需要这一层目前**没有**的东西：

- **TLS / mTLS 与独立反向代理**：`qx-cli console` 是单进程、无 TLS 的形态，且 `console_bind_is_loopback`
  在启动前就拒绝可路由地址。跨主机必须由独立反代终止 TLS、持有 operator 客户端证书、把身份注入进来。
- **operator 身份与显式 CORS allowlist**：跨主机时身份来自 mTLS（`403 authenticated_operator_required`
  那条边界），不是回环网络位置；浏览器跨源还要逐字符登记 `api.cors_allowed_origins`。
- **多机部署形态与运维审批**：这是 **M4' 未关闭的部分**（桌面 Host + 多机反代），属部署件而非本仓代码。

因此本仓能给出的"独立服务"是**本机 / 内网可信边界内的常驻进程**；对外暴露仍需运维侧补齐上面三样。

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

Scheduler worker 从 `scheduler.jobs_path` 装载 JobSpec，恢复 `scheduler.state_path`，按 UTC Cron 触发并写入带租约/fencing 的 JobQueue；Strategy worker 管理策略生命周期，执行 `Signal→Portfolio→RiskGate→OrderIntent`，并把通过风控的订单转成带审计的 SubmitOrder 命令交给 Execution worker。它不会绕过 OMS/Risk 直接调用 Venue。未知角色仍会被脚本拒绝；`-AllowUnmanagedRoles` 只适合外部扩展进程接管未知角色。`scheduler.jobs_path` 里的作业只接受 `trigger` 为 Cron 且 `window` 为 `Any` 的形状：交易日历、事件与手工触发在运行时没有派发者，会话窗口也没有交易日历数据源，因此这类作业会在装载时被拒绝并点名 `job_id`，而不是登记后永远不出队（V12 §18-B #117）。

一次作业运行在生产里只有一次执行机会，接口口径如下：Strategy worker 的**成功**收口只写终态（`Succeeded`，`error_code` 归零），**结果码不进 `JobRun`**——它没有"成功结果"这一格，硬塞会让一条成功运行在 `/scheduler/runs` 读出假 `error_code`（V12 §18-A #129 修的就是这个）。作业体自己报错时，worker 当场把那条运行写成 `Failed` + 固定错误码 `STRATEGY_JOB_FAILED`，于是"策略报错了"与"策略跑太久"是两条通道：超过 `timeout_seconds` 的运行仍由下一轮 tick 升级为 `NeedsIntervention` 并释放并发键（V13 R2 第十二遍 #190）。同一个 `run_id` 的条目若因为掉电还留在队列里、运行却已经收口（`Succeeded`/`Failed`/`NeedsIntervention`），worker 领取租约后只确认并跳过，不再执行第二次，`attempt` 与既有 `error_code` 原样保留。两条通道都不重试：调度器**不会自动重跑**，失败那一刻无法判定订单是否已经出网，自动重试等于二次提交。因此 `JobSpec.retry_policy`（`max_attempts`、退避、`retryable_codes`）与到期重试入口 `Scheduler::retry_run_at` 目前只在 `qx-scheduler` 库内和用例里生效，生产装配零调用者；`JobStatus::Failed` 这一格现在有了生产者，而 `next_retry_ts` 在默认 `max_attempts=1` 下恒为 `null`（用例把这条钉住：写了就是声明没人执行的重试），要接上自动重跑仍需先给出"这条作业失败后可安全重放"的判定依据（V12 §18-A #110 剩余 / #129，V13 R2 #190）。

Strategy 可以通过 `strategy.research_snapshot_path` 加载包含 CandidateBinding、FeatureArtifact、FactorReport、PIT 时间和数据血缘的研究快照；生产中已绑定交易对象的策略必须同时设置 `research_snapshot_required=true`、`research_data_fingerprint` 和 `dataset_bundle_path`，运行时还要求快照指纹匹配并绑定已验证的数据清单。回测/纸面配置仍兼容 `target_snapshot_path` 和 `target_qty`，但不应将裸目标仓位作为实盘发布物。

研究快照的字段契约（v1，逐层列出；快照 JSON 由仓库外的因子工程环节按此导出，**本框架不提供生成该文件的命令**，`qx-factor` 的物化与校验入口只在库和用例层被调用）：

```text
schema_version=1 的 research_snapshot JSON
顶层: schema_version, candidate, artifacts, reports, as_of
candidate: config, factor_keys, cost_bps, train_start, train_end, validation_start, validation_end, event_verified, event_manifest_digest
candidate.config: strategy_version, universe_version, feature_version, parameters, data_fingerprint, intended_exposure, constraints, execution_model, risk_model
artifacts[]: feature_key, input_fingerprint, as_of, coverage_bps, values
reports[]: feature_key, input_fingerprint, observation_hash, analysis_start, analysis_end, sample_count, coverage_bps, ic_bps, rank_ic_bps, turnover_bps, transform, missing_policy, decay_bps, capacity_raw, exposures
```

每一层的未知字段都会被拒绝（拼错的键不会被按默认值读回），`intended_exposure` 才是运行时下单数量的来源，`artifacts[].values` 目前只参与 PIT 与数据指纹校验、不改变交易决策（V12 §18-B #119）。

Paper Execution worker 可以配置 `paper_initial_cash_raw`，启动时通过幂等 `AccountCashflow(Transfer)` 写入结算币初始资金；资金进入同一 EventLog/Ledger，重启不会重复入金。该字段只能用于 `venue_id=paper`，金额使用核心定点 raw 单位。

`venue_id` 怎么被读，只有一个定义点（`crates/qx-core/src/venue.rs`，V13 R1-A3），三条口径都是配置方需要知道的：

- **算 Binance 家族**：去空格、转小写后**含** `binance` 即算，所以 `binance`、`binance-testnet`、`BINANCE`
  都会走私有 Binance worker；把它写成 `binance` 之外的名字不会被判进该家族。
- **算 Paper 虚拟执行域**：去空格、转小写后**整名等于** `paper` 才算（`" Paper "` 这种带空白与大小写的
  写法仍算）。`paper-proxy`、`paper-testnet` 这类前缀名**不是** Paper：它们拿不到
  `paper_initial_cash_raw`（配置校验直接拒绝），一条没有 CCXT `endpoint` 的 Execution worker 若配成这种
  名字，编排会在生成启动计划时就报 `Err`，而不是把它当本地纸面撮合拉起
  （`crates/qx-orchestrator/src/tests.rs` 的 `worker_plan_routes_only_the_exact_paper_venue_to_the_local_worker`
  钉的就是这一对）。
- **没有配 `venue_id`** 仍是"缺席"，不会被折成某个已知家族。缺席的含义按调用点分两种：需要凭据/规格的
  路径直接报错，Paper 提交匹配路径按"不匹配"处理。

标的（`instrument.venue`）上的 `BINANCE` 判定走的是另一把尺子（`VenueId::is_binance`，整名而非子串），
因为那一格是产品 venue 名而不是账户域；两把尺子的差异写在各自定义处，并由门禁 `venue_identity_check`
钉住"不得另起第二份"。

## 公共 CCXT 多交易所连接层

交易所连接优先使用 Python 公共 `ccxt`，配置 `exchange_id` 即可复用 Binance、OKX、Bybit 等交易所的统一 REST API。连接层入口为 `python/qianxing_ccxt`，负责 market/symbol 映射、OHLCV 分页、ticker、账户、订单和错误分类；`python -m qianxing_ccxt.worker --config <json>` 提供 JSONL 进程边界；核心 Rust 订单状态、Ledger 和回测撮合不直接依赖 CCXT。`qianxing.ccxt.binance.public.example.json` 提供无凭据公共探测样例，`credential_env: null` 也会被正确解释为匿名公共连接。

跑 CCXT 链路需显式装 `pip install "qianxing-bridge[ccxt]"`（#276 起基础 wheel 不再强制附带 `ccxt`）；本期运行时只依赖 REST 轮询、下单和对账，不依赖 CCXT Pro。`ccxt-pro` extra 与 `watch_*` 封装仅作为后续实时流扩展保留，当前不能把它们作为生产前置条件，也不能把 REST 轮询伪装成 WebSocket 用户流。当前 CCXT REST 连接层、MarketData ticker/OHLCV Worker、Execution SubmitOrder Worker、订单/余额/持仓/资金费率/资金流水 Reconcile、MarketSpec 快照、研究快照 StrategyContext、API QueryPort 和跨进程租约恢复验收已接入；现货和永续 ticker 在 bid/ask 缺失时会使用订单簿首档完成统一标准化。交易所账单字段差异和真实多交易所 sandbox 闭环仍需外部凭证与交易所环境验收，详见 [CCXT 多交易所方案](../docs/CCXT多交易所接入与策略运行方案-V1.md)。

可直接复制 `qianxing.runtime.ccxt.example.json` 作为多交易所 sandbox 拓扑样例；执行和行情 Worker 的 `endpoint` 指向 CCXT 配置文件，supervisor 会优先启动公共 CCXT 路径，旧 Binance Worker 仅作为无 CCXT endpoint 时的兼容回退。

该样例同时展示 `strategies[]` 多策略配置。每个策略实例的 `id` 必须等于对应 Strategy worker id；调度任务的 `owner` 必须填写该策略实例，多个策略共用 JobQueue 时不会互相领取任务。

双腿套利可使用 `backtest multi-builtin <strategy> <primary-bar.json> <reference-bar.json> [primary-spec.json] [reference-spec.json] [quantity]`。两条 BarFrame 必须时间戳对齐；信号由同一个套利策略生成，再分别通过统一撮合、手续费、风控和 Ledger 回测，适用于跨交易所价差与现货/期货基差策略。示例输入为 `qianxing.bar-frame.pairs-primary.example.json` 与 `qianxing.bar-frame.pairs-reference.example.json`：套利信号要等两腿累计收益差越过 `builtin_threshold_bps`（默认 100 bps）才发单，`qianxing.bar-frame.example.json` 与 `qianxing.bar-frame.okx.example.json` 只差约 9 bps，用它们跑双腿示例会得到两腿都 `fills=0`。

多标的、多币种批量回测使用 `fast-backtest manifest.json`。manifest 的 `jobs[]` 每项配置一个独立 `runtime`、`bars` 和可选 `market_spec`，CLI 会并行运行多个隔离账户/标的任务，适合同时比较 BTC、ETH、SOL，现货、永续、期货以及不同策略参数。示例见 `qianxing.fast-backtest.example.json`；每个 runtime 可以继续使用 `strategies[]` 配置多策略实例。

策略实例还可配置 `python_module`（Python 模块名或 `.py` 文件路径）。Rust 会通过 `python -m qianxing_strategy.worker` 传递版本化 JSONL 输入/输出，校验请求身份、PIT 时间、数据指纹和信号有效期；Python 策略不能直接访问交易所、EventLog 或 Ledger，也不能绕过 Rust RiskGate。未配置该字段时使用现有 Rust 策略兼容路径。

策略开发同时支持 Rust `qx-strategy`、Python `on_event`/持久 worker 和 C++ `cpp/include/qianxing_strategy.h` C ABI；三者统一返回 `StrategyDecision.intents[]`，旧 `target_qty` 仍兼容。

Rust/C++ 也可以编译成独立策略进程，通过 `strategy.external_executable`、`external_args` 和 `external_env` 接入。独立进程每行读取一个 `StrategyContractInput`，每行输出一个 `{"ok":true,"output":...}` JSON；标准输出只允许协议内容，日志写标准错误。该入口与 Python 共用超时、崩溃隔离、RiskGate、OMS、执行队列和审计链路，示例见 `cpp/examples/jsonl_strategy.cpp`。**每一行都有字节上限**：`crates/qx-cli/src/strategy_host.rs:235` 与 `crates/qx-cli/src/strategy_host.rs:263` 两处泵都走 `read_capped_worker_line(&mut reader, DEFAULT_MAX_FRAME_BYTES)`，上限是 16 MiB（`crates/qx-strategy/src/frame.rs:13`）；CCXT worker 的 stdout 泵同族，落在 `crates/qx-adapter/src/ccxt.rs:77` 的 `read_capped_line(&mut reader, crate::MAX_WORKER_LINE_BYTES)`，同一格数量级（`crates/qx-adapter/src/lib.rs:663`，与 HTTP 响应总量界同值）。越界不是截断也不是丢行：这一行判为超限、整条泵中止并把「单行超过 N 字节上限」回给调用侧，比让子进程少写一个换行符就把本进程的缓冲一路吃到内存耗尽便宜得多。三处形状由门禁按调用点钉住（`read_capped_line` 一处、`read_capped_worker_line` 两处、裸 `.read_line(` 零处），越界与 EOF 两条臂各有用例（`crates/qx-cli/src/tests/worker_pipe_failure_diagnostics.rs`、`crates/qx-adapter/src/tests.rs`）。

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
回测完成后会在 `storage.data_dir`/runs 下原子保存 RunManifest JSON，记录配置指纹、Bundle 聚合指纹、各数据组件指纹、模型、时钟和结果哈希。
该字段只有**一条**落点口径：相对值按当前进程的工作目录解析，所以 `report`/`status` 读侧、可写运行态（账本/队列/outbox）与事件回测证据闸门
读的都是同一棵树，从哪个目录启动就读写哪一棵产物树；`qx-cli init` 与 `strategy init` 生成的项目会把绝对落点钉进配置，那一行命令因此换目录也指同一棵树（V13 R2 #255）。
把同一份相对配置换目录启动、两棵产物树都留下了内容时，`config doctor` 会报 `[WARN] storage.data_dir.split` 并点名两棵树的落点。

**A 股涨跌停的昨收锚怎么取**（`qianxing.ashare.rules.json` 的口径）：锚 = 上一交易日的最后一根 Bar 收价；
除权除息日不按原始昨收，而是用**同一份规则快照装载进来的公司行为**折算
`(昨收 + 配股价×配股比例 − 每股现金红利) ÷ (1 + 送转比例 + 配股比例)`，结果对齐到 `price_tick`。
因此：想让除权日算对，必须给 `ashare_actions_path`（或 Bundle 的 corporate actions 组件）——
只给规则快照时，当日没有可折算的事实，锚就是不复权的原始昨收。
`rules.json` 的 `previous_close_raw`（`{"<被锚定 Bar 的毫秒 ts>": <定点昨收>}`）是手工覆盖出口，
优先级高于折算，用于复权口径由数据侧决定的场合；仓库自带的样例这一格是空的。

当配置包含 `strategies[]` 时，`backtest` 与 `strategy backtest` 会按策略实例逐个执行隔离回测，每个实例使用自己的 account/strategy 配置并输出独立结果哈希；示例见 `deploy/qianxing.runtime.strategy-multi-backtest.example.json`。组合级资金池、跨策略净额和归因需要在组合回测层显式配置，不会隐式共享单策略账户状态。

当前提供 17 个固定点运算的内置策略，可先查看目录再直接回测：

```powershell
cargo run --release -p qx-cli -- builtin-strategies
cargo run --release -p qx-cli -- backtest builtin macd deploy/qianxing.bar-frame.example.json deploy/qianxing.binance.spot.spec.json
cargo run --release -p qx-cli -- backtest deploy/qianxing.runtime.builtin-strategy.example.json deploy/qianxing.bar-frame.example.json
```

内置策略包括 SMA/EMA 交叉、MACD、RSI、布林带、Donchian 突破、动量、均值回归、网格、ATR/Keltner 趋势、VWAP 回归、波动率突破，以及配对、跨交易所、基差、现货/期货四类双腿套利。套利策略额外配置 `builtin_reference_instrument` 和 `builtin_reference_bars_snapshot_path`；跨交易所时主腿/对冲腿可以分别由不同 CCXT REST MarketData worker 维护，现货腿可设置 `builtin_reference_margin_mode: "cash"` 与 `builtin_reference_leverage: 1`，避免把期货杠杆参数发送给现货交易所。

示例夹具的三条硬约束由 `crates/qx-cli/tests/fast_backtest_manifest.rs::shipped_examples_fill_positions_and_pay_nonzero_fees` 钉住，改帧前先跑它：

- **帧长要够上窗口**。`qianxing.bar-frame.example.json` / `.okx.example.json` 各 70 根（1000…70000，步长 1000），因为 MACD 的门槛是 35 根可见 Bar（26 根慢 EMA + 9 个 MACD 值 + 上一根交叉判定），默认的 `fast 5 / slow 20` 只需要 21 根。
- **数量有两个口径，相差 1e9 倍**。运行时配置的 `builtin_quantity` 是定点裸值（`Quantity::from_raw`，1 个单位 = 1e9），命令行位置参数 `[QUANTITY]` 是整数单位（`Quantity::from_i64`）。裸值写 `1` 等于 1e-9 个单位，成交额小到让手续费按整数截断为零——摘要会显示"成交了但没付费"。示例配置一律用 `1000000000`（1 个单位），A 股示例用 `100000000000`（100 股，一手的整数量）。
- **A 股时间戳要落在录制日历内**。`qianxing.ashare.bar-frame.example.json` 的 6 根 Bar 用的是 2024-06-03/06-04 两个交易日的实际分钟戳，与 `qianxing.ashare.calendar.example.json` 和 `qianxing.ashare.rules.json` 的 `session_windows` 对齐；换成任意的 `1000/2000/…` 会让每根 Bar 都判定为不可交易，回测安静地零成交。

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
# 装了 wheel 的用户补带数据源的 extra（离线按上文用 --offline）；从源码根目录跑则等价的 `pip install -e "python[a-share-akshare]"`
pip install "qianxing-bridge[a-share-akshare]"
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

**公司行为 v1 线格式**（Python 写侧与 Rust 读侧共用，`schema_version: 1`）。这份契约不靠文档对齐，
由三处互相咬着：`tools/check_architecture.py` 的 `ashare_cross_language_contract_check()` 静态比两侧
名册，`python/tests/test_ashare_cross_language_contract.py` 在进程内重算夹具，
`crates/qx-xingban/src/ashare/tests.rs` 把同一份夹具读回并喂进折算锚。

- 信封顶层 5 个键：`schema_version`、`source`、`instrument`、`as_of`、`actions`。
- 单条动作 35 个键、交易日历 5 个键，两侧逐项同名同序；动作名 16 个：`cash_dividend`、`bonus_share`、
  `capital_transfer`、`capital_change`、`rights_issue`、`rights_issue_expiry`、`new_share_issue`、
  `repurchase`、`suspension`、`unknown`，以及可转债的 `convertible_bond_issue`、
  `convertible_bond_interest`、`convertible_bond_conversion`、`convertible_bond_call`、
  `convertible_bond_put`、`convertible_bond_redemption`。
- **单位口径只有一条规则**：以 `_raw` 结尾的输入列是**已定点整数**（SCALE = 1e9），原样收下；
  其余中文/英文别名按元或股读入再乘 SCALE。两种写法混用同一列时 `_raw` 优先。
  因此 `派息: 0.5` 与 `cash_dividend_raw: 500000000` 是同一件事，但把后者再乘一次 SCALE 是错的。
- **日期口径**：`YYYY-MM-DD`、`YYYY/M/D`、`YYYYMMDD`、以及带时间部分的 ISO 串都吃
  （空格与 `T` 两种分隔都截断到日期）。公告日期解析不出来时会回落成除权日，那等于把 PIT 可见时间
  改晚，所以自定义 provider 若要写 `published_at`，请写完整 ISO 时间戳而不是空串。
- 除权日的锚按上文那条折算公式算，Python 侧 `test_anchor_reference_is_recomputed_from_the_payload_events`
  与 Rust 侧 `python_written_actions_fold_into_the_shared_ex_rights_reference` 各自独立复算同一格期望值。

复算与自证（离线，不碰任何外部接口）：

```powershell
python -m unittest discover -s python/tests -p test_ashare_cross_language_contract.py
cargo test -p qx-xingban --lib python_written
python tools/check_architecture.py
```

夹具在 `python/tests/fixtures/ashare_actions_cross_check.{rows,payload,expectations}.json`：
`rows` 是数据源原始行，`payload` 是 Python 现在写出的字节，`expectations` 由 payload 派生。
三份都在版本控制里，改任何一份都会被上面三条命令中的一条打红 —— 不要手抄期望值。

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

先用完全本地的 Paper 闭环验收控制面、队列、订单事实和账簿归约。`paper-submit-order` 归约的是账户事件
日志 `paper-<account>-<venue>-events.json` 里**已有**的行情事实，它自己不造行情，所以先跑 `paper-e2e`
注入行情：

```powershell
cargo run --release -p qx-cli -- paper-e2e `
  deploy/qianxing.runtime.paper-strategy.example.json

cargo run --release -p qx-cli -- paper-submit-order `
  deploy/qianxing.runtime.paper-strategy.example.json `
  deploy/qianxing.paper-submit-order.example.json
```

后者输出 `PAPER_EXECUTED`，并验证 Accepted、Fill、LedgerApplied、控制命令终态和队列确认；它不连接任何网络。
在空数据目录上跳过 `paper-e2e` 直接执行，会把初始资金按账户身份落盘后以
`FAIL_CLOSED: Paper SubmitOrder 缺少 <instrument> 的最新行情事实` 退出、不下单；这句拒绝会当场写成该
命令的 `Failed` 终态并把队列确认掉（`control-queue/` 里不留命令与租约文件），末尾点名出路：换一个全新的
`command_id` 与 `request_id` 重新提交。同一 `request_id` 再投仍按幂等挡为 `DuplicateRequest`，但那条命令
已经终态，不会停在 `Accepted` 等人去清队列；一次性入口与常驻 `paper-worker` 对同一条命令给出同一个裁决，
缺行情只终这一条命令的态，worker 继续跑下一轮（V13 第三十一遍 ② #273，实测 `logs/s770_pass32_paper_submit_terminal.txt`）。

订单提交必须先经过 `ControlCommand(kind=SubmitOrder)` 审计；命令的 `payload.order_json` 只允许包含订单，不允许携带凭证。示例默认 `dry_run: true`，只验证权限、账户、Venue 和订单形状，不建立网络连接：

```powershell
cargo run --release -p qx-cli -- binance-submit-order `
  deploy/qianxing.runtime.production.example.json `
  binance-user-main `
  deploy/qianxing.submit-order.example.json

cargo run --release -p qx-cli -- ccxt-submit-order `
  deploy/qianxing.runtime.ccxt.example.json `
  ccxt-execution-main `
  deploy/qianxing.ccxt.exchange.example.json `
  deploy/qianxing.submit-order.ccxt-derivatives.example.json
```

确认沙盒/模拟账户、余额和回滚流程后，才可以把命令中的 `dry_run` 改为 `false`。执行器会先追加 `OrderSubmitted`，再调用 Binance REST submit；成功回报继续写入 Accepted/Fill/LedgerApplied，HTTP 5xx 或连接中断只保留待对账状态，不自动重试。CCXT 那条走 `ccxt-submit-order`：显式点名执行 worker、经 CCXT 沙盒发单，参数顺序是 `runtime.json <worker-id> <ccxt-config.json> <command.json>`，其中 `ccxt-config.json` 的 `exchange_id` 必须与 worker 的 `venue_id` 一致，凭据只按环境变量名引用、不进命令行也不进配置。

两条一次性入口与各自的常驻 worker 共用同一份账户/交易所拓扑判据：点名的 worker 与订单的 `account_id`/`instrument` venue 对不上时按「拓扑不一致」落成终态 `Failed`，而不是把订单写进另一台 worker 的 EventLog；命令刻意缺行情、缺风控规格同样走这条终态路径，不留在 `Accepted` 等人去清队列（V13 R28）。

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
## 事件日志的读面与写面

打开一本 EventLog 有两种语义，由每个调用点在 `OutboxRecovery` 里显式声明（V13 R2 第十七遍 #169）。**写面**（`ReprojectOnOpen`）给确实会追加事实的链路：订单提交、Paper 执行 worker、Paper 行情桥；打开时若日志非空，就把投影游标之后的事实补投影进 Outbox，补上上次崩溃留下的投递缺口。**读面**（`ReadOnly`）给读模型：`serve` 启动时按账户投影事件日志、装载 Ledger 与 JobRun 读模型这两步，按账户刷新 `GET /account/snapshot?account_id=&venue_id=` 的那条链，以及策略绑定、策略契约与 API 轮询桥；打开过程不写任何东西。

区分之前只有一条入口，日志非空就整段补投影，于是每个读请求都在写 Outbox。文件后端按 N=4000 实测一次读打开写出 4000 个 Outbox 文件、首次 5.4913s、二次 3.0854s（`logs/s377_pass17_probe_read_open.txt`）；分面后同档复测读面 `open_read_only` 是 0.0594s、Outbox 文件 0 → 0，同一档写面仍是 7.9600s 与 4000 个文件（`logs/s381_read_open_after.txt`）。两次探针的 profile 不同（`s377` 是 `--release`、`s381` 是 debug 的 `test` profile），所以 5.4913s 与 7.9600s 这两个写面数不能互比；能互比的是同一档内的倍率与写副作用计数：改前“读打开”与“写打开”是同一条路径（一次打开写 4000 个文件），改后读面写 0 个。两边耗时都随事件数线性增长（每翻倍档倍率≈2），差别在常数项：一次 GET 不再顺手重写一遍全量 Outbox。

写面补投影只序列化 `seq >= projection_cursor` 的事实，过滤排在 JSON 序列化之前（`crates/qx-storage/src/lib.rs` 的 `project_event_log_to_outbox`）；API 轮询桥把"这条事实投影过没有"的前缀核对改成按序号二分定位，把一份没变过的日志再投影一次在 N=16000 时从 0.6038s 降到 0.0090s，倍率由 3.87/4.18/4.32（平方）回到 2.06/2.05/2.49（线性）（`logs/s379_api_projection_before.txt`、`logs/s380_api_projection_after.txt`）。

**没有一起收口的部分**，写在这里以免被读成"长跑代价已解决"：写面链路的稳态单价仍与日志总长度成正比——`logs/s381_read_open_after.txt` 里 N 从 1000 到 8000 时 `refresh()` 空转从 0.01220s/tick 涨到 0.09595s/tick、一次 `register_order` 追加从 0.04838s/tick 涨到 0.36528s/tick（倍率 1.96/2.13/1.88 与 1.92/1.99/1.98），Outbox 文件数随事件数累计到 8005；`LiveEventPipeline::ingest_once` 每次追加仍先 `self.clone()` 整份状态。事件日志本身没有任何保留、压缩或归档策略，只有增长没有上限。分段后端可用（写面 `open_configured(root, name, currency, Some(max_events_per_segment))`、读面 `open_read_only(.., Some(..))`，段由 manifest 摘要校验），但 `storage.event_log_segment_events` 决定的是恢复与归档粒度，不决定日志总长度，今天也没有任何轮转触发者。

## 换 EventLog 后端会被当场拒绝

`storage.event_log_segment_events` 是可选字段，但它决定这个账户的事实写在哪套文件里：不配时写 `{name}.json`，配了之后写 `{name}.manifest.json` 加 `segments/` 目录，两套形状在同一个 `storage.data_dir` 下互不相交。所以改动这一个字段等于换一本账——改之前它不会提示，直接启动会让运行时读到 0 条事实（磁盘上那份仍在）、账户快照现金归零，随后追加的第一条事实拿到 `seq 0`，同一目录里从此并存两本历史。V13 R2 第十八遍 #226 之前实测到的正是这条：5 条事实的单文件日志换成分段后端打开，读到 0 条、现金 0，根目录变成 `["binance-main.json", "binance-main.manifest.json", "outbox", "segments"]`（`logs/s406_pass18_backend_switch_probe_before.txt`）。从文件后端换成 `storage.backend: "sqlite"` / `"postgres"` 是同族的：数据库读不到文件，旧历史会变成没人再读的文件，而数据库里的空表被当成首次启动。

现在这三条打开入口（`open` / `open_configured` / `open_read_only`）共用一道 fail-closed 闸门，`storage.root` 下留着"当前配置没选中的那套文件后端"的非空历史时当场拒绝启动，报错点名那本历史并把两条出路写全（`logs/s407_pass18_backend_switch_probe_after.txt` 原文）：

```text
EventLog「binance-main」选中的是分段后端，但同一目录下还留着另一本历史：单文件后端
C:\...\data\binance-main.json（5 条事实）。换后端不会自动迁移历史，直接启动会让账户读到空账本、
账簿归零，并从 seq 0 开始写第二本账。请改回原来读取的那套后端，或把上面这些文件
（分段后端另有 segments/ 目录）归档到别的数据目录后再启动。
```

两条出路的含义：**改回原来读取的那套后端**（把 `event_log_segment_events` 恢复成原值），或者**先把旧历史归档到别的数据目录**再启动——归档要连同 `segments/` 目录一起搬，且搬完这份日志的序号从 0 开始，只适合确实要另起一本账的场合。读模型（`serve` 的账户快照、Ledger 读模型、策略上下文）同样会被拒：换错后端时印一份"现金 0"的快照，比当场报错危险得多。首次启动不会被误伤——空占位文件不算历史，同一目录里别的账户、别的日志名也不拦。

两处不对称要说清。① 反向（数据库后端→文件后端）拦不住：数据库里的空表判断不了"这里曾经有过历史"，换回来时需要人工核对旧文件是否还在被读。② 不要为了省稳态代价去开分段：`logs/s404_pass18_steady_probe_segmented_vs_flat.txt` 量的是同一份日志、同一个稳态 tick，N=8000 时一次真实追加单文件 0.34309s/tick、分段 500 是 0.41688s/tick、分段 5000 是 0.41737s/tick（贵两成以上），磁盘合计三档只差 0.01% 量级——分段买到的是按段归档的抓手与 manifest 摘要校验，不是更省的写入。稳态那笔价钱的真正来源是"每 tick 把整本日志读回来"（N=1000→8000 时空转 `refresh` 0.01149→0.09007s/tick，`logs/s403_pass18_steady_probe_flat.txt`），而 paper 循环里行情桥与恢复桥各开一个 pipeline，所以还要加上"另一个进程刚追加过、本进程重放到最新"那一份整本重放（N=8000 时比"空转读+只写"之和多出 0.02705s/tick，`logs/s405_pass18_steady_probe_cross_worker.txt`）。这两笔都要靠保留/压缩/归档策略才能拿掉，本轮尚未做。

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

两条一次性 relay 命令都打印 `parked=`（停在门后、还要人处理的条数），退 0 只代表这一页搬完；
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
Relay 指标包括 `qx_worker_up`、心跳、扫描、发布、重试、租约冲突、发布失败与停在门后的 `qx_outbox_relay_parked`；Consumer
指标包括接收、成功、重复、重试、死信、格式错误和 ACK 失败。`qx_worker_up{worker="<worker_id>"} 1` 只表示最近
一次写入仍认为进程正常，生产告警还必须结合
`qx_worker_heartbeat_timestamp_seconds{worker="<worker_id>"}` 的新鲜度判断进程是否已经失联。

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

"心跳多久算陈旧"在全仓库只有一个窗口：`messaging.worker_stale_after_ms`（缺省值见
`crates/qx-runtime/src/runtime_config/schema.rs`）。`/ready`、`/metrics` 的 stale 判定与
`runtime-check` 的健康快照读的是同一个数、同一份 worker 指标目录；配置里另一处
`shutdown_timeout_ms` 是"优雅停机最多等多久"，与心跳新鲜度无关，不得混用（V12 §16 #122
修掉的就是 `runtime-check` 曾把两者当成一回事、并把当前时刻传成 `0` 使过期判定永不成立）。
同一枚预算的**计时起点**是"观察到停机请求"的那一刻，不是"开始等 worker"的那一刻（V13 R2 #217）：
监督循环可以在请求到来之前已经守了几小时，按进门计时会让长跑的 worker 一分宽限都拿不到就当众判
`StopTimedOut`；`StoppedWithinBudget` / `StopTimedOut` 报出的 `waited_ms` 同样只算请求之后的等待，
所以日志里那个数读作"给它多少时间它没走完"，不读作"它总共跑了多久"。
`runtime-check` 的 `health` 块是**拓扑快照**：它只按配置登记 worker（全部 `starting`），
不拉起进程，因此这里既不会出现 `ready` 也不会出现 `degraded`；运行期健康以 `/ready` 与
`/metrics` 为准。

### 三条 NATS 等待预算：连接、请求/确认、有界拉取

改前这几条等待的界**全在依赖默认值上，本仓一个字没写**：async-nats 0.50 的握手 5s、连接层请求 10s、
`Context` 的发布确认 5s。运维在配置文件里找不到这三格，日志也读不出某个等待是放了几个默认值。
本轮把它们写成 `crates/qx-storage/src/wait_budget.rs` 的 `NatsWaitBudget`
（`connect_timeout_ms` / `request_timeout_ms` / `pull_expires_ms`，默认 5000/5000/1000），配置面补
`messaging.connect_timeout_ms`、`messaging.request_timeout_ms`、`messaging.pull_expires_ms` 三键
（`deploy/qianxing.runtime.messaging.example.json` 与 `deploy/qianxing.runtime.consumer.example.json`
已写出这三行），打开配置时按 100..=60000、100..=300000、100..=30000 校验，越界的报错是
`messaging.<字段> 必须在 <下界>..<上界> 内`。`request_timeout_ms` 取 5000 不是照抄依赖：它一次替换
原来的 10s 请求默认与 5s 确认默认，把两格并成一格，**收紧不放宽**。三条下界一律 100ms 而不是 1，
理由与 `messaging.consumer_handler_timeout_ms` 同一条教训（V13 §9.21 #214）——拉取 expiry 或握手预算
被打成毫秒级时，worker 循环就变成对 broker 的热循环。

**请求侧要接两个旋钮才真的都有界**。`Context::set_timeout` 只管发布 ack；`$JS.API.*`
（`get_stream` / `get_consumer`）走的是连接层 `ConnectOptions::request_timeout`。只设前者时，
哑 server 上 `publish` 5.017s 返回 Err、`get_stream` 仍要 10.029s 才返回（本轮改前实测）。
所以发布者与消费者的 `connect` 都把同一枚预算接到这两处，pull 的 `.expires()` 接第三枚。

**改前更危险的一格是崩溃，不是超时**：`jetstream::new(client)` 曾在 `block_on_runtime` 返回之后才调用，
而它内部要 `tokio::spawn` 一条 ack 监视任务；qx-cli 的五个 relay/consumer 入口与 `#[test]` 线程都不在
Tokio 上下文里，于是握手刚成功就在下一步 panic `there is no reactor running, must be called from the
context of a Tokio 1.x runtime`——`--features nats` 的链路在"连接建立"当场断掉，既不返回 Ok 也不返回 Err。
现在 `Context` 在已进入的 runtime 内建好再交出，普通线程同样能拿到连接句柄。

判据是 `crates/qx-storage/tests/nats_wait_budget.rs` 七条，不需要真 broker（被拒端口 + 只完成握手、
对请求有去无回的哑 server），覆盖三条预算各自有界、默认值与被替换的依赖默认逐项相等、越界在发起任何 IO
之前被拒、以及两个 `connect` 在无 reactor 的线程上都能返回。**这七条要 `--features nats` 才编得进去**：
默认 profile 下 `cargo test --workspace --all-targets` 里该 target 报 `running 0 tests`，
即 §4 那条「在册 ≠ 实跑」在本轮新增面上同样成立。

### 流的三态：空闲、断链与放弃

行情流与用户流每条常驻读循环都区分三件事，混成一件事的两种后果都出现过（V13 §9.11）：把空闲当失败，
一个当天没有成交的账户会在几个窗口后被具名放弃；把空闲当交付，坏链路会每窗复位预算、永不放弃。

- **空闲（Idle）**：一个读窗内确实没有事件。它**不吃重连预算**。行情 worker 会因此降到 `Degraded`，
  详情行带 `market stream idle consecutive_windows=N`，收尾时同时播报 `quotes=N idle_windows=M`；
  CCXT Pro 用户流同口径（`ccxt pro user stream idle consecutive_windows=N`，收尾
  `… stopped idle_windows=M`）。Binance 用户流不把空闲写成 `Degraded`（一个没有新成交的账户流本来就该是安静的），
  它把 `idle_windows` 记进运行报告，由 `crates/qx-adapter/tests/binance_stream_retry.rs` 逐窗核对。
  薄成交对的 symbol 会长期停在 `Degraded`，这不是断链，也不需要人工介入。
  空闲只允许出现在**帧边界**上——读超时落在半条消息中间仍算故障，否则两帧会被拼成一帧。
- **断链（Closed / 失败）**：退避只有一个口径（`crates/qx-core/src/retry.rs`）：500ms 起、8s 封顶、
  **连续** 10 次失败后具名放弃；拿到一次成功应答即清零。放弃的报错会点名是哪条通道
  （`CCXT Pro 用户流` / `CCXT 行情子进程` / Binance 用户流），运维据此判断要重启的是哪一侧。
- **对冲恢复（不是一件事）**：多腿恢复链只有退避、**没有放弃**——节律 `100ms→8s` 有界，循环无界，
  因为停在 `HedgeRequired` 的分组是一条腿已成交、另一条还没对冲的裸腿，把它永久晾着比反复扫描更危险。
  三条恢复循环（Binance / CCXT / Paper）在没有待对冲分组时连扫描都不发起。

所以判据是：`Degraded` + `idle` 计数在涨 = 市场安静；出现带"超过上限"的具名放弃 = 链路真的断了。

Prometheus 告警规则模板位于 `deploy/prometheus/qianxing-alerts.yml`，覆盖 worker 失联、心跳
过期、Outbox 发布失败、Consumer 死信和 ACK 失败；生产环境应根据实际抓取间隔、租约窗口和
值班策略调整 `for` 与 heartbeat 阈值。

## 自检与内部命令

这四条命令跑的是**合成输入**（`DEMO.SIM`、`gen_bars`），进程内断言即冒烟测试，不访问网络、
不下单；输出行都带 `DEMO 合成输入` 标注，不要当成真实行情上的结果：

| 命令 | 作用 | 口径 |
| --- | --- | --- |
| `qx-cli ecosystem` | 跨 crate 装配冒烟，逐段打印：因子目录→QIFI 协议差分→ProviderRegistry 重试链→调度 JobSpec→控制命令→API 路由 | 断言失败即非零退出 |
| `qx-cli paper` | 只跑 Paper venue 主链路：受理→报价成交→快照 Filled→断线转 `ReconcileRequired`→恢复 | 同上 |
| `qx-cli verify` | 只校验确定性内核：合成 Bar 过质量门、同输入同哈希、改参数变哈希 | 不触发插件装配与 Paper |
| `qx-cli all` | 完整自校验：`verify` 的全部内容 + 插件装配顺序 + 上一条的 Paper 主链路 | `verify` 与 `paper` 的并集 |

`qx-cli recovery-child <占位> <占位> …` 是 `supervise` 用来重启跨进程恢复的**内部入口**，
位置参数按 argv 下标透传，不是给人手敲的命令；人工恢复请用 `outbox-relay`、
`consumer-dlq-replay` 与 `reconcile`。`ccxt-worker` 与 `binance-worker` 同属 worker 进程入口，
由运行时配置里 `workers[].role` 决定登记表分派，见上文「公共 CCXT 多交易所连接层」。

API 服务线程的停机出口：`serve` / `serve_tls_mtls_with_stores` 现在都接受一个停机闭包，
accept 循环按 2ms 轮询它并在置位时返回，`run_runtime_api` 传的是监督器的
`context.should_stop()`。此前两条循环写作 `listener.incoming()`，Ctrl+C 之后线程仍卡在
accept 里、`join()` 永不返回，投影线程与 TLS 重载线程永远停不掉（V12 §16 第二遍）。

## 停机与故障

服务进程收到 Ctrl+C / SIGINT / SIGTERM 后不再依赖默认强杀：`install_shutdown_signals` 把它转成监督器的
停机请求，`join_worker_handle` 按 `shutdown_timeout_ms` 预算等 worker 自己收摊，超预算才报
「收到停机请求后 Xms 仍未退出」。`serve` 的两条 API 出口（明文与 mTLS）本轮也汇入同一条阶梯，屏幕上
「按 Ctrl+C 停止」那句从此真有对应行为（V13 R2 #164）。预算与那句里的 `Xms` 都从**收到停机请求**那一刻
起算，不是从"开始等这个 worker"起算（V13 R2 #217）：守了一整天再按一次 Ctrl+C，拿到的仍是整份宽限。
阶梯走到出口时，API 还会给已在飞行的 WebSocket 会话发一帧 `{"type":"server_shutdown"}` 再关连接，
不必等前端自己断开（V13 R2 #218）。交易 worker 必须先停止新信号，再等待账户命令队列、
用户流关闭和对账完成。`Degraded` 单独出现可能只是行情安静（见上文「流的三态」）；但若 worker 进入
`Failed`，或 `Degraded` 伴随带"超过上限"的具名放弃，不得自动补单，必须走快照恢复与人工确认。
