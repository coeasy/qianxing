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
- **示例配置不是免责声明** —— `deploy/` 顶层 53 份模板逐份被生产读法真读一遍（`crates/qx-cli/tests/unit/deploy_template_coverage.rs`），照抄即坏的那三份已在 V13 R1-A6 修掉

## 交付面一览

可数的部分先摆在明处，每一格都点名"谁在核对这个数"。这些读数是 2026-10-09 在同一棵工作树上现取的：

| 交付面 | 本轮数量 | 核对者 |
|---|---|---|
| CLI 入口名（`qx-cli help` 印出的顶层与二级） | 46，其中 18 条二级入口 | 门禁 `cli_help_surface_check`：help ≡ clap 命令表 ≡ `cli.rs` 派发分支；18 条二级入口逐条实跑 `--help`（`crates/qx-cli/tests/command_surface.rs`） |
| 内置策略 | 17（13 条单腿 + 4 条双腿/多腿） | `qx-cli builtin-strategies` 现印，表体来自 `BuiltinStrategyKind` |
| HTTP 路由 | 18（17 条 `GET` 读面 + 1 条 `POST` 写面） | 门禁 `api_surface_doc_check` 与 `crates/qx-cli/tests/unit/api_endpoint_table_routes.rs`：`deploy/README.md` 的端点表 ≡ `handle_inner` 的路由集合 |
| WebSocket 事件流 | 1（任意路径 + `Upgrade: websocket`） | `admit_websocket` 的准入判定，口径见 `deploy/README.md` 的 WS 段 |
| `deploy/` 顶层示例配置模板 | 53 | `crates/qx-cli/tests/unit/deploy_template_coverage.rs`：登记表与磁盘清单逐名相等，新增不登记即红 |
| Cargo workspace 成员 | 29（28 个 `qx-*` crate + `contract-tests`） | 根 `Cargo.toml`；`cargo test --workspace` 按成员出段落 |
| 静态架构不变量 | `tools/check_architecture.py` 中的具名契约检查 | 每条检查验证具体行为、边界或实现归属；不以检查数/测试数作为验收质量 |
| Rust 用例 | 以实际测试执行结果为准 | `cargo test --workspace --no-fail-fast` |

数字之外那半句更重要：`maturity/capabilities.yaml` 的四档证据里 `sandbox_tested` 与
`production_approved` **无一为真**。所以上面这些"跑得通"说的是本机可重放的回测、Paper 闭环与
代码级契约，不是"已对接真实账户"。

## 模块

模块职责与 crate 边界见[模块地图](docs/module-map.md)。

Rust、Python 与 C++ 策略入口以及动量、SMA 和均值回归示例见[多语言策略示例](docs/multi-language-strategies.md)。
Python `BaseStrategy` 本地接入、回测/研究命令和服务器沙箱边界也见该指南；本地可信策略默认直跑。
Python 策略的未来数据扰动与预热长度敏感性检查见[策略偏差诊断说明](docs/策略未来数据与预热偏差诊断.md)，命令为 `qianxing-strategy-diagnose`。
参数实验支持固定种子的随机搜索、分类 TPE 与有限网格核密度 TPE 候选提议，入口和训练/OOS 边界见[自适应参数搜索说明](docs/自适应参数搜索.md)。

## 安装

详细安装、发布环境与 Python wheel 步骤见[安装指南](docs/installation.md)。最短安装路径与 wheel 构建命令仍列在本页「快速开始」。

## 快速开始

下面这段是可复制的常用链路示例，写法是在**源码仓库里**直接用 cargo 跑；已经按[安装指南](docs/installation.md)装好的人会
得到同名 binary，把 `cargo run -p qx-cli -- ` 换成 `qx-cli ` 即可，参数与行为完全一致（同一份 clap 命令表）。
参数与入口的权威来源是 `cargo run -p qx-cli -- help`——入口清单每加一条就变一次，所以这里不抄份数；
`deploy/` 下 53 份示例配置有登记表逐名核对（`crates/qx-cli/tests/unit/deploy_template_coverage.rs`），完整使用口径见
[工业化易用性收口指南](docs/工业化易用性收口指南-V1.md)。

```bash
# 构建
cargo build --release

# Rust CLI 安装方式：从源码仓库固定 tag 编译安装；也可直接下载 GitHub Release 的平台二进制
cargo install --git https://github.com/coeasy/qianxing --tag v0.3.0 qx-cli

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

# 架构不变量自检（验证单一分派点、风险门装配、回测归属与能力证据等具名契约）
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
   需要对照指数时，可加 `--benchmark-bars <index-bars.json>`；基准与权益曲线必须逐时间戳精确对齐，当前使用未复权收盘价并输出每 Bar 跟踪误差/信息比率，不年化。`--benchmark-csv <path>` 导出逐 Bar 组合、基准与主动收益序列；`--xlsx <path>` 导出摘要、权益/持仓、成交、月度收益及可选基准序列工作簿。`--against <summary>` 可重复指定同输入运行做横向比较，也可与 `--benchmark-bars`、`--benchmark-calendar`、`--benchmark-csv` 一起使用，逐轮分析并按主动收益排名；HTML 比较页可按运行/策略/最低收益筛选，并按收益或回撤排序，还可叠加已校验权益 CSV 的归一化时间曲线、切换曲线并查看时间提示。每条曲线最多抽样展示 2,000 点，完整数据保留在 CSV。横向对比不接受 `--evidence`，需分别为每轮生成运行证据包。单轮 `--evidence` 会把基准输入与分析结果登记、导出并在验签时重算。
   多账户批次结果 JSON 可直接交给 `qx-cli report <multi-account-result.json> --html`；报告逐账户核对运行产物，并提供账户、标的、状态与收益筛选。展开成交可查看时间、订单、数量/价格/费用 raw 值、strategy_id 和 fills CSV 来源。Fill 契约不含 side，因此方向明确标记为缺席。不同账户的现金、持仓、风险与收益仍保持隔离，不合并为资金池。
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

Windows 下用 `build.bat`（cmd.exe 里跑），POSIX 下用 `bash build.sh`。两份脚本执行相同的发布检查步骤，并由 `build_script_parity_check` 核对对应命令与步骤顺序。脚本先探测可用的 Python 解释器，候选顺序是 `QX_PYTHON` → 仓库 venv → PATH 上的 `python`，并确认 `tzdata` 可导入；之后执行架构检查、格式检查、Release 构建、Rust 测试、Clippy、Python/JSON 测试、CLI 与生态冒烟、运行时拓扑检查。Rust 启动的 Python 子进程通过 `QX_PYTHON` 选择解释器。`.gitattributes` 将 `*.bat`/`*.cmd` 固定为 CRLF、`*.sh` 固定为 LF，避免 Windows `cmd.exe` 无法解析批处理文件。

上面那两条 wheel 构建命令同样有前置条件：`tools/build_python_wheel.ps1` / `.sh` 先检查目标解释器能否运行 `pip` 和 `maturin`，随后通过 Python 项目的 PEP 517 `maturin` 后端从当前工作树和 `crates/qx-python/Cargo.toml` 一次构建源码与原生扩展；PyO3 `abi3-py310` 生成通用 `cp310-abi3` wheel，不再把某次旧 `.pyd` 复制进新包。maturin 从当前源码和 qx-python Cargo manifest 构建原生扩展；构建器契约由 `crates/qx-cli/tests/unit/artifact_identity_doc.rs` 检查。CI 与发布工作流会安装构建前端；本地解释器缺 `maturin` 时可运行 `uv pip install --python <python> maturin`。`.ps1` 整体是 ASCII-only：Windows PowerShell 5.1 按 ANSI 码页读无 BOM 文件，行尾的中文会让下一行代码整行消失。执行策略也是前置条件：Windows 客户端默认策略禁止运行未签名脚本，未带执行策略的调用会在一行代码都不执行的情况下以 `UnauthorizedAccess` 退出，所以上面的入口写成 `-ExecutionPolicy Bypass -File`。安装包产物落在 `dist/`，而 `/dist/` 在 `.gitignore` 里，故它是本地产物、不随仓库分发。

实现状态与未完成外部边界的**现行**版本只有一份：
[自研量化框架审计与重构方案 V13](docs/自研量化框架审计与重构方案-V13.md) —— §1–§2 是功能地图与架构事实，
§3 回答"核心功能是否全部实现、主体链路是否贯通"，§4 是不合理清单（分级），§5 是分轮方案，
§9 是逐条执行记录（每条都抄当轮日志）。它取代 V12 的方案序列；V12 与更早的 V11 逐轮收口记录
已在 2026-09-26 移入 [docs/archive/](docs/archive/README.md)，其中的数字是那一轮工作树的快照。

能力证据按 `implementation` / `code_tested` / `sandbox_tested` / `production_approved` 四档分级（见[项目指南](docs/project-guide.md)里的 `maturity/capabilities.yaml`）。默认 `single_node` 使用 SQLite/Files；PostgreSQL、NATS、真实交易所沙盒和券商柜台不会因为代码或 feature 存在而被标记为生产批准。

产品支持范围与内部成熟度证据通过稳定 capability ID 关联；生成目录同时供 API、Python/C++ 客户端、Web 控制台和本地 CLI 使用。离线查看能力清单运行 `qx-cli doctor --capabilities`，查看单项详情运行 `qx-cli doctor --capabilities --capability-id qx.venue.binance_spot`，按具体交易范围筛选运行 `qx-cli doctor --capabilities --venue BINANCE --product perpetual --account-mode hedge --json`；静态支持登记不代表当前环境连通或真实沙盒验收通过。详见[生成的产品能力目录](docs/capability-catalog.md)及其[矩阵](maturity/capability-matrix.yaml)、[成熟度登记](maturity/capabilities.yaml)和[稳定 ID 关联表](maturity/capability-crosswalk.yaml)。

## 持续集成与文档

CI 作业说明与完整文档索引见[项目指南](docs/project-guide.md)。
研究、Paper、交易所 sandbox 三种初始化与验收场景见[发布场景与验收入口](docs/发布场景与验收入口-V1.md)。

### 发布产物身份

为什么这里不报整档 sha256：产物身份只按载荷核对，不把压缩档时间戳或 PE 重链接差异当作版本差异。Python wheel 用条目数、条目 CRC 与 `_qianxing_native.pyd` 的 md5 复核；Windows CLI 记录 `target/release/qx-cli.exe` 的字节数和版本文案。字面量计数是单向证据：数到新增文案能证明改动进入产物，数到 0 不能证明能力缺席。交付口径由 `crates/qx-cli/tests/unit/artifact_identity_doc.rs` 守护，详见[安装指南](docs/installation.md)。

## 设计底线（改动前请先读）

1. **热路径不用浮点** —— 金额/价格/数量一律 128-bit 定点（`SCALE = 1e9`）。
   IEEE-754 的 NaN 位模式不确定，会直接破坏 bit-level 可重放。
2. **不用系统时间** —— 回测只消费输入数据自带的时间戳；时间轴由 `qx-data` 先排序、再用
   「同一标的 ts 必须严格递增」的闸门决定，帧读侧对乱序同样直接报错。内核不提供虚拟时钟对象，
   这意味着**内核里没有时钟对象**；真实墙钟只出现在 paper/live 的 worker 循环里。回测由
   `qx-xingban` 消费 Bar 时间轴，实盘按 EventLog 到达顺序推进，并由
   `qx-runtime::append_at_engine` 执行单调因果校验。
3. **不用无序容器做顺序敏感迭代** —— 顺序敏感处一律 `BTreeMap` / `Vec` + 排序。
4. **不用 `DefaultHasher` 做摘要** —— 其输出不保证跨版本稳定，改用内置 FNV-1a。
5. **不用外部 RNG** —— `rand` 实现细节可能随版本变化，自实现 xorshift64\* 锁定种子语义。
6. **同时间戳按因果优先级排序** —— 不是任意顺序。`TIMER < FEEDBACK < MARKET < COMMAND < MATCH < APPLY < POST`。
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

- `python tools/check_architecture.py`：0 项 `[FAIL]`，`exit=0`；架构检查直接验证具名契约，不依赖历史数量快照。
- `cargo fmt --all --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0、
  诊断 0 行；`-p qx-runtime`、`-p qx-storage`、`-p qx-api`、`-p qx-cli` 各按
  `--no-default-features --all-targets` 再跑一遍，同样 rc 0、诊断 0 行。**这条 feature-off 腿已在 CI**
  （`.github/workflows/ci.yml` 的 `feature-off` job，与 `feature-matrix` 互补——矩阵逐组合点亮「特性开着」
  的分支，它补上「特性全关」的另一半，因为 `qx-cli` 的 `default = ["sqlite"]` 经转发把
  `qx-runtime/sqlite` 一并打开，`#[cfg(not(feature = "sqlite"))]` 那半边在 workspace 构建里从不被 lint；
  V13 R28 收口了此前登记在 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` §17.11 的这条缺口）。
- `cargo test --workspace --offline`：**122 个 `test result:` 段 / 1227 passed / 0 failed / 1 ignored**；
  历史测试数量仅作旧记录参考，不作为当前验收门槛。
- `cargo test -p qx-cli --no-default-features --features sqlite`：**19 段 / 453 passed / 0 failed**；
  `cargo test -p qx-cli --bin qx-cli`（文档契约那一批）373 passed / 0 failed。
- `python -X utf8 -m unittest discover -s python/tests`：**Ran 68 tests / OK**。
- 反向变异：V13 R27 新增判据放 **35 枪**、V13 R28 放 **17 枪**，**各自打红自己点名的那颗判据**
  （COLLATERAL 0 / EQUIVALENT 0 / BAD 0），被改文件跑完逐份按 sha256 回原字节；
  口径与空枪披露见 V13 §17.11（R27）与 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` §18（R28）。

内部成熟度登记把每条代码能力钉在四档证据上（机器可读的那份是 `maturity/capabilities.yaml`，2026-10-24 现读；产品支持矩阵另见[能力目录](docs/capability-catalog.md)）：
成熟度条目 **33** 个，其中 `implementation=true` 的 **29** 个
（`postgres` / `nats` / `broker_gateway` 三格写的是可选与"没有厂商协议就没有实现"，
`merge_c07ad22_rollback_register` 按裁定写 `false`）；`code_tested` 为真的 **29** 个；
**`sandbox_tested` 与 `production_approved` 无一为真**；limitations 合计 **168** 条，
证据路径 **697** 条（逐行经门禁核对存在性）。因此可宣称的交付边界是：
**本机可重放的确定性回测、Paper 闭环、以及 CCXT/Binance 的代码级契约** —— 不是"已对接真实账户"。

| 链路 | 已落地且有本地测试证据 | 明确未收口（不得当作已完成） |
|---|---|---|
| 回测 | Bar 单标的链（撮合/成本/延迟/保证金四模型可配）、深度 `backtest book`（`--fill-tier` 只认 `l1`/`l2`：前者走 Tick 且要求帧内每档单档盘口，多一档即拒；后者走订单簿，按 `L2L3` 吃掉帧里带的全部深度，没有第三个 `l3` 旗标）、双腿 `multi-builtin`（组合收益按两条腿的钱合算，并落两腿期初本金与期末权益）、`fast-backtest` 并行、Bar 与深度链各写四份同前缀产物与可重算的输入指纹、事件重放结论；账户本金可经 `strategy.initial_cash_raw` 声明，三条单腿链各印一行生效本金与来源，schema v7 摘要记录 `account` 块，非法声明与非正本金当场报错（V11 Q71/Q72）；一份 runtime 里回测侧与 Paper 侧的本金必须同号，否则三条链与两个 Paper 入账入口都在任何副作用之前拒掉（V12 R3）；`report`/`status` 的读侧改成 `Option` 语义：缺键印 `absent`、声明过的 0 仍印 0，首行报产物世代与三块的有无，复核结论独占 `input_verified=` 一格（V12 R1）；四个信号旋钮按 kind 收成唯一一张表（`BuiltinStrategyKind::signal_knobs()`），内核需求、运行时体检、stdout 播报与摘要的 `signal{knobs,declared_unused}` 全问它，读者第一次能从产物里区分"没配"与"配了但不生效"（V12 R4 / #102）；`run_manifest.json` 的兄弟路径、`data_fingerprint` 与产物身份有了真正的生产读者，缺失或被篡改都拒（V12 R4-i） | 被 git 跟踪的 16 份 v1 历史摘要缺输入身份与重放证据，`report` 明确拒绝并提示用当前回测命令重生 v7 摘要；`qx-cli` 回归覆盖旧摘要拒绝与副作用前拦截，迁移不会伪造缺失字段；多腿链仍无 `[X · Account]` 播报，因为它同时是四条链里最长的一条（V11 Q72 §34.5 第 1、4 条）；`multi-builtin` 只在 `--root` 点名时写 1 份归因产物，没有 Bar/深度链那四份同前缀产物、也没有 RunManifest；清单外的旋钮**不 fail-closed**、照常跑只列进 `declared_unused`，"配了但不生效"只有看播报与产物才看得见（V12 §14.2 的偏离记录）；集成用例仍会把产物写进启动目录口径下的 `data/**/runs/`（本轮终树整跑为 0 条，去掉 init 绝对化那一跑立刻写出 26 条未跟踪产物），#82 未修；被 git 跟踪的 `deploy/data/**` 自 #255 起不再被写脏 |
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
运行时还提供可恢复的 `scheduler-worker` 与 `strategy-worker`：Scheduler 恢复 JobSpec/Run 状态并投递 JobQueue，最多保留 4,096 条终态运行明细，旧明细由作业级日期水位和摘要锚点承接；`/scheduler/runs` 返回活动记录与保留窗口。Strategy 管理生命周期、消费任务、执行 `Signal→Portfolio→RiskGate→OrderIntent` 并提交审计化 SubmitOrder；策略按账户/Venue 读取对应事件日志计算当前持仓，Paper 执行器在控制面已落终态但队列尚未确认的崩溃窗口只清理旧队列、不重复产生副作用；策略进程不会绕过 Risk/OMS 直接调用 Venue。
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
