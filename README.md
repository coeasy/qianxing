# 牵星 Qianxing

**分级校准，量天定位。**

确定性量化交易与回测内核 · 多交易所 · 多数据源 · 多账户 · 多策略 · 多语言策略

---

## 产品说明

**牵星是一台"先把数据分级、再把模型校准"的确定性回测与纸面交易内核。**
它要解决的是一件很具体的事：在一台没有交易所账号、没有网络凭据的机器上，把一份行情、一条策略、
一套假设跑出**能被别人复算的结论**——同样输入必然得到同样结果，而产物自己说得出这个结论压在哪些输入上。

名字取自明代"过洋牵星术"：以分级量具（牵星板）测星高以定纬度。方法论照搬过来就是
**先给数据分档，再按档位选模型**。牵星不追求更复杂的滑点公式，而是让每个撮合模型先回答
**"我不知道什么"**：L2/L3 沿真实深度行走，L1 用概率模型，Bar 只重构有限路径 ——
**数据档位不足时直接拒绝运行**，绝不从 K 线里读出不存在的盘口，也不静默换成更宽松的假设。

### 三条主链路，每条都能在本机一条命令跑完

| 主链路 | 入口 | 产出 | 凭什么算"可复核" |
|---|---|---|---|
| Bar 档策略回测（Rust / Python / C++ 三种语言写的策略同一条内核） | `qx-cli backtest …` | `*.summary.json` / `*.equity.csv` / `*.fills.csv` / `*.run.json` 四份同前缀产物 | 事件流逐条重放后得到的 `log_digest` 与 `result_hash` 相等；`summary.input.fingerprint` 按**实际读到的**帧重算，不是抄配置 |
| 深度档回测（L1 Tick 与 L2 订单簿两套内核） | `qx-cli backtest book --fill-tier l1\|l2 …` | 同上一族，落在点名的 `--root` | 档位不符即拒：`l1` 的帧里多一档盘口就失败，`l2` 按帧带的全部深度吃单，没有第三个 `l3` 旗标 |
| Paper 闭环（调度→策略→风控→队列→成交→Ledger） | `qx-cli paper-e2e <runtime>` | 订单、Ledger、审计事实与队列终态 | 全链路在本机进程内，拒单与拒绝原因一起写进产物；缺凭据的实盘路径一律 fail closed |

### 它不是什么（先说清边界，免得按错的期望用它）

- **不是已经对接真实账户的系统。** 能力矩阵的四档证据里，`sandbox_tested` 与 `production_approved`
  当前**没有一条为真**（下面「版本与现状」给了本轮现读的计数）。Binance 直连与公共 CCXT 的提交/回报/对账
  是代码级契约，缺凭据即以退出码 3 停下，本机不宣称真账号往返。
- **不是热插拔插件平台。** `qx-plugin` 只做启动期清单注册与静态装配计划，运行时动态加载没有实现，
  内核组件由编译期决定。
- **不是一台统一撮合机。** 回测与 Paper 同源的是**规则**（风控、费用、延迟、保证金四模型与事件归约
  有门禁钉着），不是撮合：回测按数据档位分三套内核，Paper 的成交由 `PaperVenue::on_quote` 的首档
  touch 产生，与回测簿内核**不是**同一台撮合机 —— 读到"同一内核"时不要把"同一撮合"一起读进去。
- **不含跨节点高可用、WASM、券商柜台协议与逐家签名实现**，这些按供应商另立项，不因 feature 开关
  存在就宣称完成。

## 为什么用它

- **回测与实盘共享同一规则内核** —— 只替换数据、事件驱动、提交与反馈，策略代码零改动
- **多交易所是身份问题，不是连接问题** —— `CanonicalProduct` / `InstrumentId`（qx-core）与 `DataSourceId`（qx-guanxing）分层分离，绝不相互覆盖
- **多数据源固定主源** —— 备源只做回填与离群校验；四级存储 + 机器可读质量门
- **多账户是结算边界，不是资金字段** —— 账户 ID 贯穿全链，双树风控
- **多策略是治理，不是多线程** —— 隔离 + 净额求解 + 显式优先级 + 归因
- **"没算过"必须和"是零"长得不一样** —— 钱字段用 `null` 表达未算、`0` 只表达算过且为零；读侧印 `absent` 而不是替交易所报一个 0
- **示例配置不是免责声明** —— `deploy/` 顶层 53 份模板逐份被生产读法真读一遍（`crates/qx-cli/src/tests/deploy_template_coverage.rs`），照抄即坏的那三份已在 V13 R1-A6 修掉

## 交付面一览

可数的部分先摆在明处，每一格都点名"谁在核对这个数"。这些读数是 2026-10-09 在同一棵工作树上现取的：

| 交付面 | 本轮数量 | 核对者 |
|---|---|---|
| CLI 入口名（`qx-cli help` 印出的顶层与二级） | 46，其中 18 条二级入口 | 门禁 `cli_help_surface_check`：help ≡ clap 命令表 ≡ `cli.rs` 派发分支；18 条二级入口逐条实跑 `--help`（`crates/qx-cli/tests/command_surface.rs`） |
| 内置策略 | 17（13 条单腿 + 4 条双腿/多腿） | `qx-cli builtin-strategies` 现印，表体来自 `BuiltinStrategyKind` |
| HTTP 路由 | 18（17 条 `GET` 读面 + 1 条 `POST` 写面） | 门禁 `api_surface_doc_check` 与 `crates/qx-cli/src/tests/api_endpoint_table_routes.rs`：`deploy/README.md` 的端点表 ≡ `handle_inner` 的路由集合 |
| WebSocket 事件流 | 1（任意路径 + `Upgrade: websocket`） | `admit_websocket` 的准入判定，口径见 `deploy/README.md` 的 WS 段 |
| `deploy/` 顶层示例配置模板 | 53 | `crates/qx-cli/src/tests/deploy_template_coverage.rs`：登记表与磁盘清单逐名相等，新增不登记即红 |
| Cargo workspace 成员 | 25（24 个 `qx-*` crate + `contract-tests`） | 根 `Cargo.toml`；`cargo test --workspace` 按成员出段落 |
| 静态架构不变量 | 见 `maturity/gate_snapshot.json`（`checks` / `gate_check_floor` / `labels_sha256`） | `tools/check_architecture.py`：`--snapshot` 写入该机读快照，收尾处拿本轮实测与它逐值比对（条数只降不升由地板常量守） |
| Rust 用例 | 全仓地板 `WORKSPACE_TEST_FLOOR = 1233`；本轮整树 121 段 / 1218 passed | 同一门禁 + `cargo test --workspace --no-fail-fast`，读数见「版本与现状」 |

数字之外那半句更重要：`maturity/capabilities.yaml` 的四档证据里 `sandbox_tested` 与
`production_approved` **无一为真**。所以上面这些"跑得通"说的是本机可重放的回测、Paper 闭环与
代码级契约，不是"已对接真实账户"。

## 模块

| 模块 | 职责 |
|---|---|
| `qx-core` **牵星** | 确定性内核：时间戳口径（`clock.rs` 只有 `pub type Ts = u64;`，**内核里没有时钟对象** —— 时间轴由推进方决定）、事件溯源、重放校验，以及 `VenueId` / `InstrumentId` / `MarketId` / `CanonicalProduct` 身份契约（`identity.rs`）；因果序由 `EventLog::validate` 与 `qx-runtime` 的 `append_at_engine` 单点裁决（`TIMER < FEEDBACK < MARKET < COMMAND < MATCH < APPLY < POST`）；合约规格与市场状态分离落在 `trading.rs`（**无独立 `fenye` 模块** —— 该模块已删且不复活，此处按代码事实改口，见 docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md WP-15） |
| `qx-guanxing` **观星** | 数据平面：`DataSourceId`、质量门、标准化、`as_of()` point-in-time 可见性 |
| `qx-data` | 多资产数据基础设施：统一市场数据契约、目录、摄取与增量管道（提供方不进内核） |
| `qx-xingban` **星板** | Bar/L1 Tick/L2 订单簿撮合与仿真：成本、延迟、保证金、因果回测 |
| `qx-zhenlu` **针路** | 执行与路由：风控门禁、OMS、路由决策 |
| `qx-risk` | 风控规则集：`RiskRule`（禁空 / 最大数量 / 最大名义）与保守默认规则集版本 |
| `qx-genglu` **更路** | 订单维度对账裁决（**绩效指标见 `qx-xingban`、成交归因见 `qx-cli`、现金对账见 `qx-runtime`**，本 crate 刻意不重复实现） |
| `qx-plugin` **卯眼/榫头** | 能力清单注册表：manifest schema/哈希校验与 Ed25519 签名、扩展点贡献声明、独占冲突检测、依赖求解、Profile/Bundle/Patch 静态装配计划（不含运行时动态加载） |
| `qx-factor` | 因子与特征：版本、PIT 工件、分析报告、候选策略绑定 |
| `qx-protocol` | 账户协议：Canonical Snapshot、Diff、QIFI 兼容边界 |
| `qx-provider` | 数据提供方：能力矩阵、稳定选择、主备故障切换 |
| `qx-scheduler` | 调度契约：JobSpec、依赖、交易日历、幂等与重试 |
| `qx-control` | 控制面：权限、审计命令、终态退场（**事件订阅游标在 `qx-api`**，本 crate 不含） |
| `qx-api` | 框架无关的本地查询、控制命令、事件流与 WebSocket 边界；写操作只经 `ControlPlane` |
| `qx-datastruct` | 列式 BarFrame、PIT 视图、JSON/Arrow C Data Interface |
| `qx-adapter` | REST/TLS/WebSocket 传输与 Venue/Provider 适配边界；含 Binance Spot REST/L1 行情基线 |
| `qx-runtime` | 运行时拓扑配置、worker 监督、停机信号与健康状态 |
| `qx-orchestrator` | 运行编排：把已校验的 `RuntimeConfig` 变成 worker 启动计划并管理子进程生命周期；不执行策略、不下单 |
| `qx-storage` | 文件/分段 EventLog、控制面、队列、快照、审计以及 SQLite/PostgreSQL 事务后端 |
| `qx-strategy` | Rust Strategy API、上下文/事件/多订单意图和原生策略 SDK |
| `qx-execution` | Venue 回报统一归约、SubmitOrder 执行副作用边界，以及执行事实/订单/风控/行情/对账/Venue 的稳定端口（V10 P2a 由独立端口 crate 并入） |
| `python/qianxing_ccxt` | 公共 CCXT 多交易所 REST 数据/交易连接层；CCXT Pro 仅保留后续扩展接口 |
| `qx-python` | PyO3 原生扩展、Arrow C Data Interface capsule 协议 |
| `cpp/` | C++ Strategy API v1 稳定 C ABI、CMake 示例 |
| `qx-cli` | 单一 CLI binary：命令分派、worker 装配、回测与运维命令，外加进程内确定性自校验 |
| `web/console/` | Web 控制台（M1' 快照 / M2' 实时事件 / M3' 控制面）：读面板只调 `qx-api` 的 `GET` 读面与 WebSocket 增量，**写面只有一条** `POST /control/commands`（受理 → 执行者判定 → 终态退场），**不直接下单**；它点名的每个端点由门禁 `web_console_check` 与后端路由表逐一核对 |

`qx-cli` 是单个 binary（决策：不为拆进程而拆 crate），内部按职责分文件：命令语法与命令表只有一份，在 `cli_args.rs` 由 clap 派生（V10 P2b，旧手写字符串解析已整体删除、不留双轨），`cli.rs` 保留对 `Command` 的一次显式 `match`，未识别的命令或未知参数只回错误正文、clap 给出的最接近入口建议与该入口自己的 `Usage:`，再补两行「下一步」，仍以退出码 2 fail closed（改前每条用法错误甩出的是整篇入口摘要：实测 162 行 / 12,319 B（`logs/s691_pass28_before_fix_error_wall.txt`，改前 debug 树上五条命令 162–164 行）；V13 R2 #257/#258 收成未知入口 8 行 / 289 B、参数形状不对 10 行 / 359 B，退出码仍 2），`tools/check_architecture.py` 校验「clap 命令表 ≡ `cli.rs` 分支集合 ≡ help 印出的入口」；`worker_entry.rs` 用类型系统里的 `WorkerRole::is_venue_role()` 加一张 `VenueEntry` 登记表同时服务 `ccxt-worker` 与 `binance-worker`，新增角色只需在登记表上补一条 Venue 绑定判定；跨语言子进程的 Python 解释器统一由 `QX_PYTHON` 解析（缺省 `python`）。

## 安装

本仓库不发布到 PyPI / crates.io，三条路径都从源码装。每条路径写的是**行为**与它的核对者，
不是某一轮的计时读数：能当场重跑的行为直接写行为，跑不了的整跑读数一律写明"本轮没有重跑"，
并把当轮数字留在归档里。所谓"日志"是执行那一轮时在本机 `logs/` 目录留下的命令输出，`/logs/` 在
`.gitignore` 里，**不随仓库分发** —— 每一条判据本身都在
`tools/check_architecture.py` 与 `crates/*/tests` 里，clone 下来可以当场重跑。

### A. 只装 CLI —— 最短路径，不需要 Python

前置只有一样：Rust 工具链（本轮实测 `cargo 1.98.1` / `rustc 1.98.1`）。在仓库根目录：

```bash
cargo install --path crates/qx-cli --locked      # 完全离线的机器再加 --offline
```

得到 `qx-cli`（Windows 上是 `qx-cli.exe`），落在 `~/.cargo/bin`；把该目录加进 PATH 之后，**在任意目录**
都能开一个自包含项目并跑通回测，不依赖仓库里的任何相对路径（`init` 会把 `storage.data_dir`
钉成项目目录下的绝对落点，所以下面几条命令换哪个目录启动都读写同一棵树）：

```bash
qx-cli quickstart my-qx        # 一条命令跑完下面那五条：建 macd 项目 → 静态检查 → 回测 → 读回摘要 → 安全状态
```

`quickstart` 只写 `my-qx/` 这一个目录：回测产物落项目内的 `data/`，既不碰仓库的 `deploy/data/`，也不碰启动时的
当前目录。任一步失败就停下，只回显**那一步**的命令原文和重跑整条的写法，以退出码 2 结束；成功时五步逐条印
「[完成]」，收尾给「你刚做完了什么」+ 三条能直接敲的下一步。它直调下面那五条入口所用的同一批函数，所以同一份输入
下 `result_hash` 与逐条敲完全相同 —— 这条相等关系由 `crates/qx-cli/tests/quickstart_first_run.rs` 钉成用例，
而不是文档上的一句承诺。

同样的链条逐条敲（`quickstart` 内部执行的就是这五条）：

```bash
mkdir my-qx && cd my-qx
qx-cli init qianxing.runtime.json --strategy macd   # 生成配置 + 样例 BarFrame + README.qianxing.md
qx-cli doctor qianxing.runtime.json                 # 静态检查：不联网、不启动 worker、不下单
qx-cli backtest qianxing.runtime.json               # 真跑一轮 MACD 回测，产物落 data_dir 指向的 runs/
qx-cli report qianxing.runtime.json                 # 读回最新一份摘要
qx-cli status qianxing.runtime.json                 # 本地配置/worker/回测结果的安全状态；不连交易所
qx-cli help                                         # 全部入口清单
qx-cli version                                      # 一行构建身份：版本 / git 提交 / 目标三元组 / 构建档
```

需要可视化结果时，在回测后加 `--html`：`qx-cli report qianxing.runtime.json --html` 会写一份可离线打开的 HTML 和三张独立 SVG；用 `-o reports/latest.html` 可改 HTML 与 SVG 的输出前缀。

本轮没有重跑 `cargo install` 的计时（上一轮的用时读数见归档）；能核的是行为而不是耗时：
`help` / `init --strategy macd` / `doctor` / `backtest` / `report` / `status` 这六条在**仓库外**
目录里的等价链路，由 `crates/qx-cli/tests/quickstart_first_run.rs` 钉成用例——同一条输入下
`quickstart` 与逐条敲那五条的 `result_hash` 必须逐字符相同，本轮随整树测试跑绿。纯回测与 Paper
链路只有这一个 binary —— 只有跨语言策略 worker 与 CCXT worker 才需要 Python（见 C）。

**产物身份**：带可读本地数据集的 `init` 还会生成经 `project-manifest-v1` 校验的 `qianxing.project.json`；每次通过重放的回测除 RunManifest、summary、equity、fills 外，还会生成 `*.record.json`（RunRecord），对这四类文件逐个写入摘要。`report` 会按摘要指针读取 RunRecord、核对同轮 RunManifest 并重算每个产物摘要，缺失或被篡改即拒绝展示报告。落盘份数由用例钉住而不是文档自说：`init --strategy macd` 落 **10** 份、首轮回测再写 **6** 份，所以一个 quickstart 项目共 **16** 份（`crates/qx-cli/src/tests/init_onboarding.rs` 逐个数）。

装好的是哪个构建不必靠文件哈希自证：`qx-cli version` 那一行与 `doctor` 的第一格 `build_identity`、`status --json` 与 `report --json` 里的 `runtime_version` 同出一处（`crates/qx-cli/src/build_identity.rs`），值由构建期注入的包版本、git 提交、目标三元组与构建档拼成，源码里没有硬编码的版本号；`--version` 与 `-V` 是同一条入口的两个别名，三条写法逐字相同且都退 0。

装好的 exe 不用把仓库带着走。示例配置只经**一条**查找链解析（`crates/qx-cli/src/deploy_lookup.rs`，V13 第三十一遍 ①/#266，另有常驻门禁判据钉住这条链的入口只有一处定义、示例读取的报错只由一处拼装）：`QX_DEPLOY_DIR` → 可执行文件同级 `deploy/` → 再往外一层 → 构建期源码树的 `deploy/` → 当前目录的 `deploy/` → **二进制里的内置模板清单**。最后一层由 `crates/qx-cli/build.rs` 在构建期把本目录顶层那 53 份 JSON 原样快照进 exe，需要时把**整份清单**落进当前用户的临时目录（不是只落被点名的那一份——`fast-backtest` 的 manifest 里作业按同级文件名引用 runtime/bars/spec，只落一份会让这条链在下一格读取上断掉），并按清单内容签名分桶；只读入口那 7 处以 deploy 目录示例文件名为默认值的路径参数全部挂着 `parse_deploy_path`，这条挂载面本身也是常驻判据（少挂一处就报，判据没有对象时同样报，不会静默给绿），所以同名目录里的陈旧副本不可能被新二进制读到；清单为空时构建直接失败，模板缺失这件事不该跟发布物一起出门。

这一格两侧都有常驻判据：链条形状由 `crates/qx-cli/src/tests/deploy_lookup.rs` 钉，屏幕与退出码由 `crates/qx-cli/tests/default_example_paths.rs` 从**无关启动目录**逐条实跑钉（只读入口四条 + 裸 `backtest` + A 股 `fast-backtest` + 位置参数搬迁 + worker 精确路径）。把整棵工作树复制到仓库外、删掉那份 `deploy/` 再逐条跑通属于**当轮实测**，连同它的三枚 `result_hash` 一起留在 [`README-历史实测快照-2026-10-09.md`](docs/archive/README-历史实测快照-2026-10-09.md)。两条口径要分开：**接了查找面的入口**（只读入口的默认值，加上 `fast-backtest` 的 manifest——它在读取点自己走这条链）里，只要那是"示例配置形状"的路径且当前目录没有这一份，就按上面的顺序搬走，并在 stderr 说一句 `[查找 · Lookup]`，不做静默替换；**没接这条链的入口**（`scheduler-worker` 一类精确路径）按当前目录原样解析，读不到时错误正文点名"这一份示例在别处存在: <路径>"。屏幕上那行 `config_fingerprint=` 描述的是哪一份文件，两种情形下都始终可查。

### B. 装开发 / 发布环境 —— 跑全部九道门禁

```bash
build.bat          # Windows，cmd.exe 里跑
bash build.sh      # Linux / macOS
```

九步依次是**架构不变量自检**、格式检查、release 构建、Rust 测试、clippy、Python/JSON 边界测试、核心语义
自校验、CLI 全链路与生态冒烟、运行时拓扑校验；两份脚本被门禁按步骤名与命令多重集逐项断言同序同条。前置是
Python 3.10+（本轮实测 3.12.13）且能 `import tzdata`。第 `[0/9]` 步自己按 `QX_PYTHON` → 仓库 venv → PATH
挑一个**真能打印版本号**的解释器，并把它导出成 `QX_PYTHON` 交给后面由 Rust 起的子进程（V12 §19 #133），所以不需要
用户预先设环境变量。仓库 venv 离线重建：

```bash
cd python && uv venv .venv --python 3.12 && uv pip install tzdata
```

第 `[1/9]` 跑的是 `tools/check_architecture.py`，也就是本仓库那五百多条架构不变量。V12 §22 之前它**只在
CI 跑**：本地把八步走完看到"全部完成"，并不证明这些不变量成立（#139）。它只读源码、不碰编译产物，所以排在
几分钟的 release 构建之前，红了当场就知道。

实测口径：本轮（V13 R25，2026-10-09）单独重跑的是 `[1/9]` 那条架构门禁与 `[4/9]`/`[6/9]`
两条测试腿（读数见下面「版本与现状」），**没有**整跑 `build.bat` 九步；九步整跑的当轮读数留在
[`docs/archive/README-历史实测快照-2026-10-09.md`](docs/archive/README-历史实测快照-2026-10-09.md)
与 CHANGELOG 里。这两件事不该混着报：`build.bat` 与 `build.sh` 的同序同条由
`build_script_parity_check` 按步骤名与命令多重集逐项核对，脚本没改就不必重装一遍环境来证明它成立；
而门禁与测试腿是每轮真跑的，所以每轮重读。

### C. 装 Python 侧 —— 跨语言策略与 CCXT worker

wheel 带的是 `qx-python` 编译出的原生扩展（Windows `.pyd` / Linux `.so`）加四个纯 Python 包，两条入口
分平台。**任何 `cargo` 之前**先探测解释器有没有 pip，缺 pip 即退出码 1 并印两条修法。

```bash
# Windows：默认执行策略是 Restricted，直接跑 .ps1 会以 SecurityError 退出，所以入口必须显式带 Bypass
powershell -NoProfile -ExecutionPolicy Bypass -File tools/build_python_wheel.ps1 -Python python\.venv\Scripts\python.exe
# Linux / macOS（PYTHON 指向你那个能跑的解释器，缺省会落到 PATH 上的 python3）
PYTHON=python/.venv/bin/python bash tools/build_python_wheel.sh
```

装进一个干净 venv 并复核原生扩展确实可用：

```bash
uv venv .venv-qx --python 3.12
# #276：基础安装零强制第三方依赖，离线也能装成，不再需要 --no-deps
uv pip install --offline dist/qianxing-0.1.0-cp312-cp312-win_amd64.whl
python -c "import qianxing as qx; print(qx.doctor())"
# 按能力选装 extras（不装也 import 得动，用到才在调用点给可执行提示）
uv pip install --offline "qianxing[ccxt]"     # CCXT 行情/交易 worker
uv pip install --offline "qianxing[a-share]"  # A 股数据源（Windows 连 tzdata 一起带）
```

Release 工作流可在配置 `PYPI_PUBLISH_ENABLED=true` 和 PyPI Trusted Publisher 后自动发布兼容 Python 3.10–3.13、Windows/macOS/Linux 的 wheel；未配置该仓库变量时只构建并附加发布件，不向 PyPI 上传。

#276 之后 wheel 的强制依赖清单是空的：`ccxt` 与 Windows 的 `tzdata` 都改成可选 extras（`[ccxt]` / `[ccxt-pro]` / `[tz]` / `[a-share*]`），`pip install <wheel>` 在任何索引状态下都能装成，`--no-deps` 不再是必需项。`ccxt` 只在真的跑 CCXT worker 时才 import，`Asia/Shanghai` 只在真的取 A 股时区时才解析（#253 把顶层求值挪到用时），缺谁都在调用点抛带 extras 名字的可执行提示。

正式 SDK 的入口为 `import qianxing`。目前 `qx-app` 已向 SDK 开放同步 Bar 数据集校验、内置策略回测和产物复核；金额与数量使用 `*_raw` 定点整数，报告和指标由 Rust 内核生成。Tick/OrderBook、Paper、Live 与长任务控制尚未接入这层 SDK，不会因安装 wheel 而被宣称可用。

安装 wheel 后也可用 `qianxing doctor`、`qianxing validate-dataset <spec.json>`、`qianxing backtest <spec.json>` 和 `qianxing verify <outcome.json>` 调用同一组研究用例。

```python
from qianxing import BacktestSpec, BuiltinStrategySpec, run_backtest, verify_run

spec = BacktestSpec(
    run_id="sample-001",
    instrument="BTCUSDT.BINANCE",
    bars_path="data/btc-usdt.bars.json",
    settlement_currency="USDT",
    initial_cash_raw=100_000_000_000_000,
    output_dir="runs",
    strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="sma-5-20"),
)
outcome = run_backtest(spec)
assert verify_run(outcome).verified
```

**产物身份按载荷报，不按整档摘要报（口径由用例常驻核对）**：装完包要核对的是尺寸 / 条目数 / 条目 CRC / 内嵌 `_qianxing_native.pyd` 的 md5 等于当轮 `target/release/_qianxing_native.dll` / 发布产物里那颗几百万字节的 `target/release/qx-cli.exe` 中的播报字面量计数。为什么这里不报整档 sha256：同一份载荷重打包出来的整档 sha 就会变（zip 时间戳参与打包），把它写进交付文档等于给读者一把量不出东西的尺子——所以产物身份只按载荷报，这条口径由 `crates/qx-cli/src/tests/artifact_identity_doc.rs` 核对：它不许交付文档里出现 64 位十六进制的整档摘要，并要求上面这套骨架与判据文件互相点名（#159 收口）。

**exe 字面量计数是单向证据（#179）**：数到 **N>0** 次能证明这条播报进了装机产物；数到 **0** 次证明不了"这个构建没有这个能力"——链接器会把重复常量池化、会把没用到的分支整个丢掉，一次改名或一次字符串拼接就能让计数归零而行为照旧。所以计数只用作"当轮改动真的落进了发布物"的正向核对，反向结论（某能力在发布物里缺席）一律回源码、用例与 `--features` 组合去判，不从二进制计数推。

**安装包必须排在本轮最后一次构建之后**（#194）：打包脚本在 stage 之前重链原生扩展，所以 wheel 必须通过 `tools/build_python_wheel.*` 构建，不能把旧的扩展文件直接打包。本轮已修改 Python facade 与发行元数据，归档里的历史载荷尺寸、扩展摘要和包导入记录不能作为当前发布件证据；发布前要用当前源码重建 wheel，并在干净环境从安装后的 `qianxing` 命名空间检查能力。

### 装不上时的四个坑（都是本机踩过的）

| 症状 | 真正的原因 | 修法 |
|---|---|---|
| `[0/9]` 报「找不到可用的 Python 3 解释器」，或 `[4/9]` 恰好 2 条 Python 桥用例失败 | Windows 上 PATH 里的 `python` 是 Microsoft Store 占位桩：退出码 0、什么都不打印 | 按 `[0/9]` 印出的两条修法之一：建仓库 venv，或 `set QX_PYTHON=<完整绝对路径>`（必须是文件路径，写成目录会让整树测试以 `TEST_EXIT=101` 崩） |
| 跑 `.ps1` 入口直接 `SecurityError / UnauthorizedAccess`，一行脚本都没执行 | Windows 客户端默认执行策略是 Restricted，未签名脚本一律拒 | 用上面写着的形式：`-ExecutionPolicy Bypass -File`；README 里每条 PowerShell 入口都由门禁核对带着 Bypass |
| `[6/9]` 或 A 股相关用例 `AshareProviderError`（报错原文点名 `pip install tzdata`） | Windows 没有系统 IANA 时区库，`tzdata` 是 `python/pyproject.toml` 里声明的平台依赖；`import qianxing_ashare` 本身已不再需要它（V13 #253 把时区取值挪到用时） | `python -m pip install tzdata`（或 `uv pip install tzdata`） |
| 改了 `build.bat` 之后 `[1/9]` 之前就炸，报 `\'不是内部或外部命令\'` | 用 `sed -i`/MSYS 工具编辑会把 CRLF 抹成 LF，cmd.exe 读不了带中文的 LF 批处理 | 归一化回 CRLF，且按字节做（`raw.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")`）；门禁会数孤立 LF |

## 快速开始

下面这段是可复制的常用链路示例，写法是在**源码仓库里**直接用 cargo 跑；已经按上面「安装 A」装好的人会
得到同名 binary，把 `cargo run -p qx-cli -- ` 换成 `qx-cli ` 即可，参数与行为完全一致（同一份 clap 命令表）。
参数与入口的权威来源是 `cargo run -p qx-cli -- help`——入口清单每加一条就变一次，所以这里不抄份数；
`deploy/` 下 53 份示例配置有登记表逐名核对（`crates/qx-cli/src/tests/deploy_template_coverage.rs`），完整使用口径见
[工业化易用性收口指南](docs/工业化易用性收口指南-V1.md)。

```bash
# 构建
cargo build --release

# Rust CLI 安装方式：从源码仓库固定 tag 编译安装；也可直接下载 GitHub Release 的平台二进制
cargo install --git https://github.com/coeasy/qianxing --tag v0.1.0 qx-cli

# 运行端到端演示（含确定性自校验）
cargo run -p qx-cli --release

# 纸面交易验收
cargo run -p qx-cli --release -- paper
# Binance 单轮对账：本地来源是运行时配置指向的账本，远端来源是该 worker 的交易所账户
cargo run -p qx-cli --release -- reconcile deploy/qianxing.runtime.production.example.json reconciler-main
# V5.1 生态层验收
cargo run -p qx-cli --release -- ecosystem
# 一条命令验收 Scheduler → Strategy → Paper Execution → Ledger
cargo run -p qx-cli --release -- paper-e2e deploy/qianxing.runtime.paper-strategy.example.json

# 构建 Python wheel（包含 qx-python 原生扩展；Windows PowerShell）
# 默认执行策略是 Restricted，直接跑 .ps1 会以 SecurityError 退出，所以入口必须显式带 Bypass
powershell -NoProfile -ExecutionPolicy Bypass -File tools/build_python_wheel.ps1 -Python python\.venv\Scripts\python.exe
# Linux/macOS：
bash tools/build_python_wheel.sh

# 全量测试
cargo test --workspace

# 架构不变量自检（V9 收口：单一分派点、无空风控门、回测装配唯一、能力矩阵证据、行数棘轮）
python3 tools/check_architecture.py

# 工业化统一入口：初始化、回测、Paper 主链路和实盘前检查
cargo run -p qx-cli -- help
cargo run -p qx-cli -- init qianxing.runtime.json
# 一步生成绑定内置 MACD 和样例数据的可回测项目
cargo run -p qx-cli -- init qianxing.runtime.json --strategy macd
# 一次检查配置、路径和策略输入；不连接交易所、不发送订单
cargo run -p qx-cli -- doctor qianxing.runtime.json
cargo run -p qx-cli -- doctor qianxing.runtime.json --json
# 查看有效配置摘要；脚本需要 JSON 时加 --json
cargo run -p qx-cli -- config explain qianxing.runtime.json
cargo run -p qx-cli -- config explain qianxing.runtime.json --json
cargo run -p qx-cli -- config fingerprint qianxing.runtime.json
cargo run -p qx-cli -- backtest
cargo run -p qx-cli -- run paper
cargo run -p qx-cli -- status qianxing.runtime.json
cargo run -p qx-cli -- report qianxing.runtime.json
cargo run -p qx-cli -- report qianxing.runtime.json --json
cargo run -p qx-cli -- builtin-strategies
cargo run -p qx-cli -- strategy list
cargo run -p qx-cli -- strategy init macd qianxing.strategy.macd.json
cargo run -p qx-cli -- strategy backtest qianxing.strategy.macd.json deploy/qianxing.bar-frame.example.json
cargo run -p qx-cli -- backtest builtin sma_cross deploy/qianxing.bar-frame.example.json
cargo run -p qx-cli -- backtest multi-builtin spot_futures_arbitrage deploy/qianxing.bar-frame.pairs-primary.example.json deploy/qianxing.bar-frame.pairs-reference.example.json --quantity 2
cargo run -p qx-cli -- fast-backtest deploy/qianxing.fast-backtest.example.json
cargo run -p qx-cli -- backtest deploy/qianxing.runtime.builtin-strategy.example.json deploy/qianxing.bar-frame.example.json
cargo run -p qx-cli -- dataset-bundle deploy/qianxing.dataset-bundle.example.json data/datasets
cargo run -p qx-cli -- dataset-bundle deploy/qianxing.dataset-bundle.bar-frame.example.json data/datasets deploy/qianxing.bar-frame.example.json
cargo run -p qx-cli -- runtime-check deploy/qianxing.runtime.ccxt.example.json
cargo run -p qx-cli -- runtime-check deploy/qianxing.runtime.multi-venue-arbitrage.example.json
cargo run -p qx-cli -- runtime-check deploy/qianxing.runtime.example.json --json
cargo run -p qx-cli -- live-check deploy/qianxing.runtime.production.example.json --json
cargo run -p qx-cli -- paper-check deploy/qianxing.runtime.paper-strategy.example.json
cargo run -p qx-cli -- live-check deploy/qianxing.runtime.production.example.json

# 内置 Rust、Python/C++ JSONL 策略直接复用 Bar 回测引擎；CCXT 实时模式会持续维护闭合 BarFrame 并按 digest 触发策略
cargo run -p qx-cli -- backtest deploy/qianxing.runtime.strategy-backtest.example.json deploy/qianxing.bar-frame.example.json

# backtest 会在 storage.data_dir（相对值按当前工作目录解析）下的 runs/ 生成 summary.json、equity.csv、fills.csv，
# 并与同一回测的 RunManifest 使用相同前缀，便于归档和二次分析

# 校验运行时拓扑配置，并启动 paper API（默认示例配置）
cargo run -p qx-cli --release -- runtime-check deploy/qianxing.runtime.example.json
cargo run -p qx-cli --release -- serve deploy/qianxing.runtime.example.json
# serve 暴露的 18 条 HTTP 路由（17 条 `GET` 读面 + 1 条 `POST` 写面）与 WebSocket 事件流逐条列在
# deploy/README.md 的「HTTP 读面与控制面路由」一节（含鉴权、游标与限流边界）
# 以独立进程启动已配置的 Binance 行情/用户流/执行/对账 worker
cargo run -p qx-cli --release -- binance-worker deploy/qianxing.runtime.example.json <worker-id>
# 跨平台监督器：启动拓扑中全部受管 worker，任一异常退出则停止其余 worker
cargo run -p qx-cli --release -- supervise deploy/qianxing.runtime.example.json
# 执行/对账 worker 单轮验收：
cargo run -p qx-cli --release -- binance-worker deploy/qianxing.runtime.example.json <worker-id> --once
# 通过审计后的 SubmitOrder 命令执行（示例默认为 dry_run）
cargo run -p qx-cli --release -- binance-submit-order deploy/qianxing.runtime.production.example.json binance-user-main deploy/qianxing.submit-order.example.json
# CCXT 一次性提交（OKX 等第二交易所经 CCXT 沙盒）：显式点名 worker + CCXT 配置 + 命令
cargo run -p qx-cli --release -- ccxt-submit-order deploy/qianxing.runtime.ccxt.example.json ccxt-execution-main deploy/qianxing.ccxt.exchange.example.json deploy/qianxing.submit-order.ccxt-derivatives.example.json
# 本地 Paper 控制面→队列→成交→Ledger 闭环（不连接网络）
cargo run -p qx-cli --release -- paper-submit-order deploy/qianxing.runtime.paper-strategy.example.json deploy/qianxing.paper-submit-order.example.json
```

### 看结果：离线报告、静态页面与同源 BFF 控制台

三条由轻到重的读法，共同边界是"读面只读投影、写面只有控制面那一条"：

1. **离线 HTML**：`qx-cli report <runtime> --html` 写一份可离线打开的 HTML 与三张独立 SVG，无外链。
2. **静态页面**：`web/console/` 是只读面板加一条写面（`POST /control/commands`）。它自己没有浏览器会话、
   CSRF token 或权限代理层，单独部署时与 API 不同源，因此不能当作生产交易控制台。
3. **同源 BFF**：`qx-cli console` 把上面那三件静态资产与 API 代理挂在同一个监听口上，会话 cookie、
   CSRF 双提交与 operator 身份注入都留在服务端，页面拿不到也不需要拿任何凭据：

```bash
qx-cli console --generate-token       # 只打印一枚引导令牌与可粘贴的 export 行，不启动服务
export QX_CONSOLE_BOOTSTRAP_TOKEN="<上一步打印的那枚令牌>"
qx-cli console deploy/qianxing.runtime.console.example.json   # 默认吃这份模板，也可换成自己的配置
qx-cli console --init my.runtime.json  # 写出一份同形的就绪模板（已存在的文件拒绝覆盖）
```

进程只在启动那一刻印一次入口 URL（`http://127.0.0.1:18091/?token=…`）；点开一次，服务端校验令牌后签发两枚
cookie——会话那枚 `HttpOnly` + `SameSite=Strict`（页面脚本读不到），CSRF 那枚故意非 HttpOnly，页面把它读出来
放进 `X-QX-CSRF` 头（双提交）。此后地址栏里不再有凭据。

三个不能读错的边界：**只绑回环**（`api.console.bind` 必须是 `127.0.0.1` / `::1`，拓扑校验在启动前就拒可路由
地址；这一层没有 TLS、没有 mTLS、没有运维审批）、**非 GET 必须带 CSRF 头且过同源校验**（两道独立的锁）、
**身份由服务端注入**（命令体里的 `operator_id` 是审计字段，不被采信为身份）。引导令牌**只从
`bootstrap_token_env` 点名的环境变量读**，配置里只有变量名、没有令牌字面量字段——命令行会进 shell 历史、
配置文件会进版本库；变量缺失时 `console` 会临时生成一枚一次性令牌并明确播报（随进程生灭、不落盘），但长期
使用仍应显式配置。逐条边界、PowerShell 写法与 systemd / NSSM 托管形态都在
[deploy/README.md](deploy/README.md) 的「同源 BFF 控制台」一节。

### 外部链路验收（缺凭据即 fail closed）

```bash
# Binance Spot 测试网络验收：runtime-check/live-check 必须先通过，
# 缺少 QX_BINANCE_TESTNET_API_KEY / QX_BINANCE_TESTNET_API_SECRET 时以退出码 3 跳过交易所步骤
python tools/binance_testnet_acceptance.py --binary target/release/qx-cli
# CI 使用 --allow-skip，让离线半边始终执行
python tools/binance_testnet_acceptance.py --binary target/release/qx-cli --allow-skip

# C++ 外部策略进程：分别用 JSON 与列式两种协议跑一遍共享内存环契约
python tools/verify_cpp_worker.py build/cpp/Release/qianxing_strategy_jsonl.exe shared_memory_json
python tools/verify_cpp_worker.py build/cpp/Release/qianxing_strategy_jsonl.exe shared_memory_columnar
```

`deploy/qianxing.runtime.binance-testnet.example.json` 是测试网络拓扑模板：行情走
`wss://stream.testnet.binance.vision/ws`，执行与对账 worker 默认指向 `paper` 之外的
testnet 账户，凭据只从环境变量读取，配置文件中不落任何密钥。

Windows 下用 `build.bat`（cmd.exe 里跑），POSIX 下用 `bash build.sh`。两份脚本跑**同一组 9 步门禁**，门禁按命令多重集与步骤名逐项断言两者同序同条（`build_script_parity_check`）。第 `[0/9]` 步先选出一个真的可用的 Python 解释器，候选顺序是 `QX_PYTHON` → 仓库 venv（Windows `python\.venv\Scripts\python.exe`，POSIX `python/.venv/bin/python`）→ PATH 上的 `python`；判定不信退出码（WindowsApps 的 `python` 存根退出码为 0 却什么都不打印），要求候选把版本号印出来，再要求它能 `import tzdata`。三者都不合格时以退出码 1 停下并印出候选清单与两条修法。挑中之后**必须把它外传成 `QX_PYTHON`**（`build.bat` 用 `for %%I in ("%QX_PY%") do set "QX_PYTHON=%%~fI"`，`build.sh` 用 `export QX_PYTHON`）：`[4/9]` 的两条 Python 桥契约用例与 `[8/9]` 的策略 worker 是 Rust 去起 Python，而 Rust 只读 `QX_PYTHON` 这一个变量（`crates/qx-cli/src/main.rs` 的 `python_interpreter_origin()`），漏掉这一步时 `[0/9]` 打印"已选中"却仍在 Rust 测试那一步以两条桥用例失败收场（V12 §19 #133，当时是 `[3/8]`）。`build_script_parity_check` 逐脚本核对"导出那一行存在且早于 `[1/9]`"，并且只认命令、不认报错提示里那句教用户怎么设 `QX_PYTHON` 的 `echo`（§19 #136）；同一条判据还核对**架构门禁本身在这两份脚本里各被调用一次、失败会中止、且排在任何 `cargo` 之前**（§22 #139）。`.gitattributes` 把 `*.bat`/`*.cmd` 钉成 CRLF、`*.sh` 钉成 LF：带 UTF-8 中文的 LF 版批处理 cmd.exe 读不了，会在文件中途以 `\'不是内部或外部命令\'` 死掉（`windows_batch_parse_check` 逐文件核对行尾与"每行以 ASCII 字节结尾"）。

上面那两条 wheel 构建命令同样有前置条件：`tools/build_python_wheel.ps1` / `.sh` 最后一步用的是 `pip wheel`，所以它们在任何 `cargo` 之前先跑 `-m pip --version` 探测，解释器没有 pip 就以退出码 1 停下并印两条修法（装 pip，或离线走 `uv build --wheel --offline --no-build-isolation`）—— 由 `wheel_builder_check` 按"探测必须早于 `pip wheel`"逐脚本核对。`.ps1` 整体是 ASCII-only：Windows PowerShell 5.1 按 ANSI 码页读无 BOM 文件，行尾的中文会让下一行代码整行消失。第三条前置是**执行策略**：Windows 客户端默认策略禁止运行未签名脚本，未带执行策略的调用会在一行代码都不执行的情况下以 `UnauthorizedAccess` 退出，所以上面的入口写成 `-ExecutionPolicy Bypass -File`（同一条判据核对"README 里每一行 PowerShell 入口都带着 Bypass"）。第四条前置是**不许绕开这两个脚本**（#181 实测）：`pip wheel ./python` 打的是包目录里已经就位的那一份原生扩展，而"删掉 `python/qianxing_bridge/` 里旧的那份、再把当轮 `target/release/` 的构建产物 copy 成导入名"只有脚本中段做得到；自己拼 `cargo build --release -p qx-python` + `pip wheel` 两步，打包器一声不响，产出的却是上一轮扩展的 wheel。这条先后顺序（构建 → 删旧 → stage → 打包）由 `crates/qx-cli/src/tests/artifact_identity_doc.rs` 的 `wheel_packaging_entry_stages_the_fresh_native_extension` 逐脚本按位置核对。安装包产物落在 `dist/`，而 `/dist/` 在 `.gitignore` 里，故它是本地产物、不随仓库分发。

实现状态与未完成外部边界的**现行**版本只有一份：
[自研量化框架审计与重构方案 V13](docs/自研量化框架审计与重构方案-V13.md) —— §1–§2 是功能地图与架构事实，
§3 回答"核心功能是否全部实现、主体链路是否贯通"，§4 是不合理清单（分级），§5 是分轮方案，
§9 是逐条执行记录（每条都抄当轮日志）。它取代 V12 的方案序列；V12 与更早的 V11 逐轮收口记录
已在 2026-09-26 移入 [docs/archive/](docs/archive/README.md)，其中的数字是那一轮工作树的快照。

能力证据按 `implementation` / `code_tested` / `sandbox_tested` / `production_approved` 四档分级（见下方文档地图
里的 `maturity/capabilities.yaml`）。默认 `single_node` 使用 SQLite/Files；PostgreSQL、NATS、真实交易所沙盒和券商柜台不会因为代码或 feature 存在而被标记为生产批准。

## 持续集成作业

`.github/workflows/ci.yml` 把每条链路都钉在独立的作业上，任何一条断裂都会红：

| 作业 | 覆盖链路 |
| --- | --- |
| `rust-core` | 全 workspace fmt/clippy/test、独立确定性自校验、架构不变量与能力矩阵证据门禁（`tools/check_architecture.py`）、Python 适配层契约 |
| `python-wheel` | Linux/Windows × Python 3.10/3.12/3.13 wheel 构建、装入干净环境后原生扩展可用且指纹与纯 Python 一致 |
| `feature-matrix` | `qx-cli` 在 sqlite / postgres / nats / postgres+nats 四种特性组合下可编译可测试 |
| `service-backends` | `postgres:16` 与 `nats -js` 服务容器下跑 Outbox 租约/围栏/重试与 JetStream 一发一收幂等契约（`--ignored`） |
| `runtime-contracts` | 全部非生产 runtime 示例的 `runtime-check`、`live-check` 失败即闭、Paper 进程边界 E2E |
| `cpp-sdk` | Ubuntu/Windows/macOS 三平台 C++ SDK 构建、ring smoke、JSONL 契约、JSON 与列式两种共享内存协议 |
| `venue-acceptance` | Binance 测试网络验收；无凭据只做离线 fail-closed 半边（`--allow-skip`） |

## 文档地图

按"你现在要做什么"选文档。这一页只列**当前仍然正确**的文档：更早的方案与审计（V1–V10 各代计划、
Barter 对齐稿、可视化终态稿、产品化路线图、差距清单、release note）已删除，需要回溯时在 git 历史里
按文件名取；V11 与 V12 两代审计/逐轮收口记录于 2026-09-26 移入
[`docs/archive/`](docs/archive/README.md)，2026-10-09 又把七份钉在旧提交基线上的规划与发布审计稿
（2026-10-03 的四份——架构审计、量化回测 Web 平台、桌面/Web 客户端三轮终版、tauron 一体化桌面客户端——加 2026-10-04 的终版合订本，
2026-10-08 的综合路线图与 R22 发布审计）连同本 README 的**历史实测读数**一起移入同一目录。归档不等于作废：
那些文件里"当时成立"的记录原样保留，只是不再当作现状来读，索引与各自的基线写在
[`docs/archive/README.md`](docs/archive/README.md) 里。

| 你要做的事 | 读这份 |
|---|---|
| 装上、跑通第一轮回测与体检 | 本 README 的「安装」与「快速开始」 |
| 看结果：离线报告 / 静态页面 / 同源 BFF 控制台 | 本 README「看结果」+ [deploy/README.md](deploy/README.md) 的控制台两节 |
| 上手细节：按场景初始化项目、七条回测入口各自的落点与拒收口径、四份产物字段、读模型里 `null` 与 `0` 的分别、实盘发布前检查 | [工业化易用性收口指南](docs/工业化易用性收口指南-V1.md) |
| 部署与运维：运行时配置、worker 拓扑与监督停机、CCXT/Binance/A 股接入、SubmitOrder、存储后端、HTTP 读面与 WS 事件流、模板契约面 | [deploy/README.md](deploy/README.md) |
| 机器可读的能力证据分级：每条能力的实现 / 代码测试 / 沙盒 / 生产批准四档 + 证据路径 + 缺口 | [maturity/capabilities.yaml](maturity/capabilities.yaml) |
| 现行审计结论、不合理清单与下一轮怎么排 | [自研量化框架审计与重构方案 V13](docs/自研量化框架审计与重构方案-V13.md) |
| 架构事实、工业级差距的工作包与逐轮登记（WP-* 与 §17 执行记录） | [架构设计与工业级优化改进方案](docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md) |
| 项目结构、模块划分与同类开源项目的对照 | [项目结构与 GitHub 竞品对比](docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md) |
| 逐轮变更、验收数字与"本轮没修什么" | [CHANGELOG.md](CHANGELOG.md) |
| CCXT worker 契约、凭据隔离、失败即闭的归约规则 | [CCXT 多交易所接入与策略运行方案](docs/archive/ccxt多交易所接入与策略运行方案-v1.md)（已归档） |
| A 股提供方、代码归一化、公司行为与八条交易制度 | [A 股数据源接入与快速选股回测方案](docs/archive/a股数据源接入与快速选股回测方案-v1.md)（已归档） |
| Rust/C++/Python 策略契约与共享内存传输的边界 | [工业级多语言策略与高性能交易方案](docs/archive/工业级多语言策略与高性能交易方案-v1.md)（已归档） |
| 现货/杠杆/永续/交割的撮合与记账统一口径 | [虚拟交易与衍生品统一模型](docs/archive/虚拟交易与衍生品统一模型-v1.md)（已归档） |
| 有凭据时怎么按五段阶梯验收，`sandbox_tested` 何时才允许翻转 | [外部链路验收执行方案](docs/外部链路验收执行方案-V1.md) |
| 跑性能基线、看回测轨的耗时口径 | [benchmarks/README.md](benchmarks/README.md) |
| 想知道某一轮**当时**修了什么、怎么证的 | [docs/archive/](docs/archive/README.md)（V11 / V12 / 2026-10 那批规划与发布审计稿，数字为当轮快照） |

任何一条命令怎么用、参数是什么，以 `cargo run -p qx-cli -- help` 为准：那份摘要由 clap 的命令定义派生，
架构门禁校验「help ≡ `cli.rs` 派发分支」，因此它不会与实现漂移；本 README 不复述完整命令表。

演示会输出（2026-10-09 本机 `target/debug/qx-cli.exe all` 实抓，退出码 0）：

```
[观星 · 质量门 · DEMO 合成输入] bars=400 判定=Ok

[星板 · 回测 A · DEMO 合成输入] 内核=qx-xingban::BacktestEngine(bar) 成交=1 手续费=0.5171 总收益=0.09% 最大回撤=0.07% 终值=100096.72
[更路 · RunManifest · DEMO 合成输入] digest=915bfad3b6872a36

[更路 · 重放校验]
  ① 同输入两次运行哈希一致 : true
  ② 改参数后哈希发生变化   : true

[卯眼 · 插件装配]
  插件数=2 加载顺序=["sys.simulation", "sys.transaction-cost"]
  独占冲突=[]

全部自校验通过 ✓
[针路 · PaperVenue] 订单接受/报价成交/断线转对账/恢复通过 ✓
```

## 设计底线（改动前请先读）

1. **热路径不用浮点** —— 金额/价格/数量一律 128-bit 定点（`SCALE = 1e9`）。
   IEEE-754 的 NaN 位模式不确定，会直接破坏 bit-level 可重放。
2. **不用系统时间** —— 回测只消费输入数据自带的时间戳；时间轴由 `qx-data` 先排序、再用
   「同一标的 ts 必须严格递增」的闸门决定，帧读侧对乱序同样直接报错。内核不提供虚拟时钟对象，
   真实墙钟只出现在 paper/live 的 worker 循环里。
3. **不用无序容器做顺序敏感迭代** —— 顺序敏感处一律 `BTreeMap` / `Vec` + 排序。
4. **不用 `DefaultHasher` 做摘要** —— 其输出不保证跨版本稳定，改用内置 FNV-1a。
5. **不用外部 RNG** —— `rand` 实现细节可能随版本变化，自实现 xorshift64\* 锁定种子语义。
6. **同时间戳按因果优先级排序** —— 不是任意顺序。`MARKET < COMMAND < MATCH < APPLY < POST`。
7. **bar t 决策，bar t+1 开盘成交** —— 从结构上杜绝 cheat-on-close。
8. **只读投影不做第二个事实源** —— API、状态查看与任何前端只消费 EventLog 派生的快照与游标，
   写操作一律经 `ControlPlane` 落审计后再进内核；投影不得回写交易状态。
9. **"没算过"必须显式缺席** —— 钱字段用 `null`（`None`）表达"这一层没算/交易所没报"，`0` 只表达"算过且为零"；
   只有价格为保留哨兵语义继续用 `0`（0 不是合法定点价格）。缺数据的字段兜一个合法的 0 等于替交易所报数。

## 版本与现状

> 这一节只写**本轮（V13 R25，2026-10-09）在本机现读的读数**。逐轮的历史读数（各代 wheel 尺寸、
> `build.bat` 九步整跑、门禁条数怎么从 460 一路涨上来、`cargo install` 用时）已经整块移到
> [`docs/archive/README-历史实测快照-2026-10-09.md`](docs/archive/README-历史实测快照-2026-10-09.md)，
> 改动内容与"本轮没修什么"看 [CHANGELOG.md](CHANGELOG.md) 与
> [自研量化框架审计与重构方案 V13](docs/自研量化框架审计与重构方案-V13.md)。

本轮整跑（同一台机器、同一棵终树，2026-10-09）：

- `python tools/check_architecture.py`：0 项 `[FAIL]`，`exit=0`；本轮条数与地板不手抄，读
  `maturity/gate_snapshot.json`（`--snapshot` 生成；收尾有一条判据核「本轮实测 == 快照」）。
- `cargo fmt --all --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0、
  诊断 0 行；`-p qx-runtime`、`-p qx-storage`、`-p qx-api`、`-p qx-cli` 各按
  `--no-default-features --all-targets` 再跑一遍，同样 rc 0、诊断 0 行。**这条 feature-off 腿已在 CI**
  （`.github/workflows/ci.yml` 的 `feature-off` job，与 `feature-matrix` 互补——矩阵逐组合点亮「特性开着」
  的分支，它补上「特性全关」的另一半，因为 `qx-cli` 的 `default = ["sqlite"]` 经转发把
  `qx-runtime/sqlite` 一并打开，`#[cfg(not(feature = "sqlite"))]` 那半边在 workspace 构建里从不被 lint；
  V13 R28 收口了此前登记在 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` §17.11 的这条缺口）。
- `cargo test --workspace --offline`：**122 个 `test result:` 段 / 1227 passed / 0 failed / 1 ignored**；
  磁盘在册 `#[test]` 的全仓地板是 `WORKSPACE_TEST_FLOOR = 1233`。
- `cargo test -p qx-cli --no-default-features --features sqlite`：**19 段 / 453 passed / 0 failed**；
  `cargo test -p qx-cli --bin qx-cli`（文档契约那一批）373 passed / 0 failed。
- `python -X utf8 -m unittest discover -s python/tests`：**Ran 68 tests / OK**。
- 反向变异：V13 R27 新增判据放 **35 枪**、V13 R28 放 **17 枪**，**各自打红自己点名的那颗判据**
  （COLLATERAL 0 / EQUIVALENT 0 / BAD 0），被改文件跑完逐份按 sha256 回原字节；
  口径与空枪披露见 V13 §17.11（R27）与 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` §18（R28）。

能力矩阵把每条能力钉在四档证据上（机器可读的那份是 `maturity/capabilities.yaml`，2026-10-09 现读）：
顶层能力条目 **25** 个，**25** 个都带 `implementation` 键，其中 `true` 的 **21** 个
（`postgres` / `nats` / `broker_gateway` 三格写的是可选与"没有厂商协议就没有实现"，
`merge_c07ad22_rollback_register` 按裁定写 `false`）；`code_tested` 为真的 **21** 个；
**`sandbox_tested` 与 `production_approved` 无一为真**；limitations 合计 **162** 条，
以仓库内路径开头的证据行 **387** 条（逐行经门禁核对存在性）。因此可宣称的交付边界是：
**本机可重放的确定性回测、Paper 闭环、以及 CCXT/Binance 的代码级契约** —— 不是"已对接真实账户"。

| 链路 | 已落地且有本地测试证据 | 明确未收口（不得当作已完成） |
|---|---|---|
| 回测 | Bar 单标的链（撮合/成本/延迟/保证金四模型可配）、深度 `backtest book`（`--fill-tier` 只认 `l1`/`l2`：前者走 Tick 且要求帧内每档单档盘口，多一档即拒；后者走订单簿，按 `L2L3` 吃掉帧里带的全部深度，没有第三个 `l3` 旗标）、双腿 `multi-builtin`（组合收益按两条腿的钱合算，并落两腿期初本金与期末权益）、`fast-backtest` 并行、Bar 与深度链各写四份同前缀产物与可重算的输入指纹、事件重放结论；账户本金可经 `strategy.initial_cash_raw` 声明，三条单腿链各印一行生效本金与来源，摘要 v4 落 `account` 块，非法声明与非正本金当场报错（V11 Q71/Q72）；一份 runtime 里回测侧与 Paper 侧的本金必须同号，否则三条链与两个 Paper 入账入口都在任何副作用之前拒掉（V12 R3）；`report`/`status` 的读侧改成 `Option` 语义：缺键印 `absent`、声明过的 0 仍印 0，首行报产物世代与三块的有无，复核结论独占 `input_verified=` 一格（V12 R1）；四个信号旋钮按 kind 收成唯一一张表（`BuiltinStrategyKind::signal_knobs()`），内核需求、运行时体检、stdout 播报与摘要的 `signal{knobs,declared_unused}` 全问它，读者第一次能从产物里区分"没配"与"配了但不生效"（V12 R4 / #102）；`run_manifest.json` 的兄弟路径、`data_fingerprint` 与产物身份有了真正的生产读者，缺失或被篡改都拒（V12 R4-i） | 被 git 跟踪的 16 份 blessed 摘要停在 `schema_version: 1`（当前代码落 v4，缺 `input` 与 `account` 两块），且没有任何用例校验这些跟踪产物；重 bless 与"摘要世代写进文件名"的取舍仍未拍板（V11 §27.5 / §34.5 第 3 条，任务 #53）；多腿链仍无 `[X · Account]` 播报，因为它同时是四条链里最长的一条（V11 Q72 §34.5 第 1、4 条）；`multi-builtin` 只在 `--root` 点名时写 1 份归因产物，没有 Bar/深度链那四份同前缀产物、也没有 RunManifest；清单外的旋钮**不 fail-closed**、照常跑只列进 `declared_unused`，"配了但不生效"只有看播报与产物才看得见（V12 §14.2 的偏离记录）；集成用例仍会把产物写进启动目录口径下的 `data/**/runs/`（本轮终树整跑为 0 条，去掉 init 绝对化那一跑立刻写出 26 条未跟踪产物），#82 未修；被 git 跟踪的 `deploy/data/**` 自 #255 起不再被写脏 |
| 交易 · Paper | 控制面→队列→成交→Ledger、三条入账入口共用精度闸门、拒单与拒绝原因写进产物、崩溃窗口恢复；账户快照的八个钱标量全部区分"算过"与"没算"，权益在任一持仓缺标记价时报 `null` 而不是剩余现金（V11 Q67/Q68/Q70）；七个会提交/排队订单的入口（含 `serve` 与 `strategy-worker`）一律在第一个语句拒 A 股段（V11 Q65 + V12 R1）；账户快照的契约版本真的认版本 —— 只认 `schema_version=1`，且版本判定排在字段解析之前（V12 R2）；带订单的快照写得出也读得回 —— 四张键表（`positions`/`orders`/`fills`/`transfers`）整份交给 `from_json` 认的那一份 serde 编码（`json_table_entries` / `json_position_entries`），键一律是带引号的字符串，`Side`/`OrderStatus` 印变体名而不是数字码，未知变体名当场拒（V12 R4 / TX5，与 V11 R14/R15/R18 同一收口点） | 本地 Ledger 拼出的持仓行没有浮盈/保证金生产者，恒定报 `null`；账户级五个钱字段同样无来源；权益报 `null` 时读侧看不出缺的是哪条标记价（V11 §32.5 第 5 条）；`fills`/`transfers` 行只编码数值与已同形的字符串，因此没有读侧折算层，也就没有"未知码"可拒（V12 §14.5 记为口径而非缺陷） |
| 交易 · 实盘 | Binance Spot 直连与公共 CCXT 的提交/回报/对账代码路径，缺凭据即退出码 3 fail closed；一轮 CCXT 对账的两半发现（远端孤单 / 本地无远端结果）经同一份清单同时落到事实流、持久报告与健康判定（V11 Q69）；两条用户流的重连都有连续失败预算 —— Binance 按累计次数计改为 `delivered > 0` 清零并统一走 `qx-core::retry`，CCXT Pro `watch_orders` 从"固定间隔无限重连"补上 500ms 起 / 8s 封顶 / 10 次连续（V12 R4-b/R4-c） | 零真实账户往返：`sandbox_tested=false`；预算与退避只在进程内证过，网络真断时的重连语义无外部记录；Binance 对账链从不查询持仓/资金费/账单，报告只能报 `null`（取数器仍缺，V11 §31.5 第 5 条）；CCXT 资金费快照缺 `timestamp_ms` 时仍兜 0（§31.5 第 1 条）；远端孤单挂单只有报告与健康两面，没有可落事实流的本地句柄（§31.5 第 4 条，口径而非缺陷）；衍生品无直连（只经 CCXT） |
| 数据 | 四级质量门、PIT `as_of()` 可见性、DatasetBundle 与组件指纹、A 股公司行为台账与八条交易制度；台账的两侧线格式（信封 5 键、动作 35 键、16 个规范动作名、`*_raw` 的定点单位、ISO 日期的空格/`T` 两种分隔）钉在 `python/tests/fixtures/ashare_actions_cross_check.*` 这一份共读夹具上：Python 侧在进程内复算 payload、期望值与除权参考价，Rust 侧读同一格而不是各自抄一份，除权除息日的锚由已装载的公司行为折算（V13 R1-A1/A2，口径见 [`deploy/README.md`](deploy/README.md) 的"公司行为 v1 线格式"一节） | 外部数据源正确性只能按供应商逐个验收，本机不可证明 —— 上面那份夹具是本机自造的对照样本，不是 akshare/交易所落下来的记录 |
| 运行时 | worker 监督与停机阶梯、共享文件系统租约/fencing token、outbox relay（sqlite/postgres/nats 四种 feature 组合可编译）；停机阶梯接上了生产者 —— `Ctrl+C`/`SIGTERM` 真的落到 `RuntimeSupervisor::request_shutdown`，等到 `shutdown_timeout_ms` 报 `StopTimedOut` 而不是无限等（V12 R4-a）；只读投影的契约贯通同轮收口：`/account/snapshot/envelope` 的 `data` 复用唯一编码器、与本进程公布的 schema 逐字段等值，schema 正文改成 `include_str!` 单一来源，两条事件读链的 `after` 统一为严格大于（V12 R4-h/A2/R4-g） | PostgreSQL/NATS 无生产批准；券商柜台无供应商协议；停机信号投递本身没有用例 —— 用例钉住的是阶梯与预算，不是真把 `SIGTERM` 发给进程（Windows 下也没有可投递的 `SIGTERM`，V12 §14.8） |

跨语言策略传输默认兼容 JSONL，也支持 `transport: "framed_json"` 的 QXSF 二进制分帧（版本、序号、长度上限、CRC32）；`transport: "shared_memory_json"` 会将同一 QXSF 帧放入双向固定槽位 SPSC mmap ring；`transport: "shared_memory_columnar"` 会将 Bar 历史编码为 QXCB 固定宽度列后放入同一 ring，适合减少行情数值 JSON 解析。三种传输的载荷由同一份 `schemas/strategy_api_v1.schema.json` 描述：`required` 名单按 Rust 非 `#[serde(default)]` 字段逐项对齐，定点 `raw` 值只走 JSON 整数，未知键两侧同拒；一条已知差异写进 schema 的 `description` 而不是靠改口径掩盖 —— 大写枚举名过得了 Rust 运行时、过不了 schema。示例：

```powershell
cargo run -p qx-cli -- backtest deploy/qianxing.runtime.strategy-framed.example.json deploy/qianxing.bar-frame.example.json
```

共享内存策略 Worker 回测：

```powershell
cargo run -p qx-cli -- backtest deploy/qianxing.runtime.strategy-shared.example.json deploy/qianxing.bar-frame.example.json
```

列式共享内存策略 Worker 回测：

```powershell
cargo run -p qx-cli -- backtest deploy/qianxing.runtime.strategy-columnar.example.json deploy/qianxing.bar-frame.example.json
```

共享内存传输层微基准（不代表完整策略端到端延迟）：

```powershell
cargo run --release -p qx-strategy --example ring_bench
```

本地 SubmitOrder 已补齐 Paper `Queue→Fill→Ledger` 可执行闭环；生产配置下的真实 Binance 交易、秘密/证书托管、跨节点 PostgreSQL/MQ 和部署级高可用仍按审计表单独验收。
运行时还提供可恢复的 `scheduler-worker` 与 `strategy-worker`：Scheduler 恢复 JobSpec/Run 状态并投递 JobQueue，Strategy 管理生命周期、消费任务、执行 `Signal→Portfolio→RiskGate→OrderIntent` 并提交审计化 SubmitOrder；策略按账户/Venue 读取对应事件日志计算当前持仓，Paper 执行器在控制面已落终态但队列尚未确认的崩溃窗口只清理旧队列、不重复产生副作用；策略进程不会绕过 Risk/OMS 直接调用 Venue。
Paper 主链路还提供 `paper-e2e` 统一验收入口，按 Scheduler→Strategy→Paper Execution 顺序运行一轮并检查订单、Ledger、审计和队列终态。

三条容易被读过头的边界，写在这里而不是散落在阶段表里：

- **插件只有启动期装配**：`qx-plugin` 做 manifest schema/哈希校验与 Ed25519 签名验证、扩展点贡献声明、
  独占冲突检测、依赖求解并产出静态装配计划；运行时动态加载与热替换**没有实现**，内核组件由编译期决定。
- **回测与实盘同源的是规则，不是撮合**：风控、费用、延迟、保证金四模型与事件归约同源且有门禁；
  撮合按数据档位分内核（Bar / Tick / 订单簿三套回测内核，Paper 成交由 `PaperVenue::on_quote` 首档 touch 产生），
  与回测簿内核**不是**同一台撮合机 —— 读到"同一内核"时不要把"同一撮合"一起读进去。
- **WASM、跨节点 HA、连接池/读写分离、逐家签名协议、manylinux/musllinux 与发布签名仍未接入**，
  需要按实际供应商与部署环境立项，不由 feature 开关存在而宣称完成。

## 许可

Apache-2.0

> 本框架仅参考业界项目的公开架构与模块设计思路（LEAN、RQAlpha、NautilusTrader、vn.py、
> Hummingbot、CCXT、vectorbt、QUANTAXIS），不复制任何第三方源代码。
