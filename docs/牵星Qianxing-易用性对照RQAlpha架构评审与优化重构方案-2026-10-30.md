# 牵星 Qianxing 易用性对照 RQAlpha 的架构评审与优化重构方案

> 日期：2026-10-30
> 对照对象：`ricequant/rqalpha` 公开仓库 master（2026-10 读取）。外部行号会随上游迭代漂移，检索时以“文件 + 函数名”为准；本文对外部实现的描述只用于机制对照，不复制其代码或数据。
> 本仓基线：分支 `codex/release-windows-longpaths`，最近提交 `4a70e850`；工作树含本轮未提交改动。
> 本文件是**评审 + 方案**，不是实施记录。§4 的 D1–D16 每条自带**状态**行，按事实分【已实现】/【部分已实现】/【设计意图，未排期】，未标注的仍是设计意图；§2、§3 是 2026-10-30 评审时点的现状快照，其中的 `path:NN` 引用按当时的文件长度核对过，后续拆分会让它们指向偏移后的位置——按**文件 + 函数名**检索，不当精确坐标读。本文件不改变任何既有台账（`maturity/strategy-run-refactor-work-packages.json` 的 R01–R23）的完成状态。

---

## 0. 结论先行

rqalpha 的“易安装、快速写策略、一条命令回测”不来自某个组件，而来自四组**默认值**：

1. **安装默认值**：`pip install rqalpha` 一条命令装齐运行所需（上游 `pyproject.toml` 声明 18 个运行时依赖），装完即有一个 `rqalpha` 命令；
2. **数据默认值**：`rqalpha download-bundle` 把月度数据包解到 `~/.rqalpha/bundle`，`run` 默认就从这里读，不需要用户先写数据清单；
3. **策略默认值**：一个 `.py` 文件、两个函数（`init(context)` / `handle_bar(context, bar_dict)`），行情与下单 API 由框架注入策略模块的 globals；
4. **运行默认值**：`rqalpha run -f s.py -s 2014-01-01 -e 2014-12-31 -a stock 100000` 里没有 JSON、没有 manifest、没有输出目录参数。

千行的强项恰好是 rqalpha 的弱项：版本化契约、可复核产物、多语言策略、风控/OMS/Ledger、取消与审计。千行的弱项也恰好是 rqalpha 的强项：**“第一次跑起来”被拆成了两份安装、两个 CLI、三份输入（策略文件 / BacktestSpec / runtime 配置），外加一份要自己准备、还得自带血缘的行情数据**。【已实现】

本方案的判断是：不应该把千行改成 rqalpha，而应该把 rqalpha 的**默认值形状**补到千行现有证据体系之上，并且所有快捷入口都落到同一套 `BacktestSpec`、`RunRecord`、`verify_run` 上，不新开一条证据较弱的路。

> 六条优先动作（评审时点全部【设计意图，未排期】；1–5 已于 2026-10-30 落地，6 只搬出部分函数族——状态逐条见 §4）：
> 1. `qianxing run -f strategy.py -s … -e … -a …`：在 Python 侧合成一份标准 `BacktestSpec`，让 wheel 用户不写 JSON 也能首跑；
> 2. `qianxing project init --demo`：脚手架同时落内置策略、样例行情和 `input_spec`，建完即可 `project run`；
> 3. `qianxing data sources|doctor|fetch|bundle|install`：把已有的 A 股三源适配器与 CCXT 适配器提升为一条“选源 → 取数 → 登记血缘 → 喂给 spec”的命令链，内置一份合成样例包，真实数据按需取；
> 4. 把报告渲染能力随 wheel 分发（或把渲染器下沉到可复用库 crate），消除 `project report` 对外部 `qx-cli` 的硬依赖；
> 5. `qianxing project plan --explain`：逐键打印本次运行的有效配置及来源（runtime / manifest / spec / 默认值 / 环境变量 / 数据源）；
> 6. 先做规模热点“搬而不改”的拆分（`app.py`、`project_runs.py`、`qx-cli` worker 树），每轮只搬一个函数族并同轮搬测试。

---

## 1. RQAlpha 的运行机制拆解

### 1.1 安装：一条 pip + 一条数据包

上游安装文档（`docs/source/intro/install.rst:24-43`）给出的最短路径是两步：

```bash
pip install rqalpha
rqalpha download-bundle
```

数据包命令的实现（`rqalpha/cmds/bundle.py:69-84`、`:107-158`）从固定 CDN 模板下载按月分桶的 `tar.bz2`：

```
http://bundle.assets.ricequant.com/bundles_v4/rqbundle_%04d%02d.tar.bz2
```

解包后落在用户目录 `~/.rqalpha/bundle`，读取侧依赖约定文件名：`instruments.pk`、`stocks.h5`、`indexes.h5`、`futures.h5`、`funds.h5`、`trading_dates.npy` 等。**数据身份由 bundle 版本承担；本次调研未见读取侧的逐文件摘要校验**——这一点是千行不能照搬的：千行的 `DatasetSnapshot` / `BarManifest` / `RunRecord` 已经把“这份回测用的是哪批字节”做成了可复核事实。

### 1.2 策略：单文件 + globals 注入

上游示例 `rqalpha/examples/buy_and_hold.py` 约 30 行：`init(context)` 记录标的与资金，`handle_bar(context, bar_dict)` 里调用 `order_target_percent`。策略作者不需要继承基类、不需要声明 manifest、不需要提供输入文档。

注入发生在 `rqalpha/main.py:118-120`、`:191-194`：策略模块的执行作用域由“用户模块 globals + `g` + `rqalpha.api.__all__`”组成。因此 `order_*`、`history_*`、`context` 这些名字在策略文件里“天然存在”，代价是：静态检查、沙箱边界、多语言契约都只能围绕“一个 Python 模块的 globals”来设计。

### 1.3 配置：六层优先级

`rqalpha/utils/config.py:139-224` 定义了从低到高的合并顺序：

```
内置默认值 < ~/.rqalpha/config.yml < ./config.yml < --config 指定文件 < 策略内 __config__ < 命令行参数
```

命令行用 `-mc`（mod config）覆盖任意层，键名是 `mod__<mod>__<option>`，值按普通字符串强制转换。这套机制的价值不在“层数”，而在于**每一层都能被单独解释**：用户能回答“这个参数最后为什么是这个值”。

### 1.4 运行时：mod、hook、executor

- `rqalpha/mod/__init__.py:31-106` 的 `ModHandler` 负责加载、启动、拆卸 mod；内置 `SYSTEM_MOD_LIST` 含 7 个 `sys_*` 模块，分别承担账户、撮合、分析、进度等横切能力；
- `rqalpha/core/events.py` + `rqalpha/core/hooks.py:98-103` 用两阶段 `Notify` 把“事件”和“钩子”分开：事件表示“发生了什么”，钩子表示“谁在这个时点插入”；
- `rqalpha/core/executor.py:37-99` 把一次事件拆成 PRE / MAIN / POST 三段，mod 只在明确阶段插入。

这是一套**框架级插件平面**：能力扩展通过“新增 mod”完成，而不是修改内核。它的代价是策略运行时的每个时点都有多个插入点，出错时的调用链比“直接调用函数”长。

### 1.5 结果：pickle + report + plot

`rqalpha_mod_sys_analyser/mod.py:600-781` 把组合、账户、成交、指标等结果写进 `result.pkl`；`--report DIR` 另出 xlsx/csv，`--plot` 出图。它的优点是“跑完立刻有一个可读结果”；缺点是结果对象是 pickle，跨版本/跨语言不可验证，也没有“产物字节是否被改过”的概念。

### 1.6 机制—价值判定

| rqalpha 机制 | 它解决的问题 | 千行对应物 | 判定 |
|---|---|---|---|
| `pip install` 单入口 | 装得上、装完就有命令 | wheel 零强制依赖；`qx-cli` 需另行 `cargo install` | 部分借鉴：保持零依赖，但要让 wheel 自带首跑与报告能力 |
| `download-bundle` | 装完就有数据可跑 | `dataset-ingest` / `dataset-bundle` / `DatasetSnapshot`；`init` 只带一份样例 BarFrame；`qianxing_ashare` 三源适配器与 CCXT 适配器各自为战 | 借鉴“一条命令拿到能跑的数据”；改为“内置合成样例包 + 按需多源取数”，不搬其 CDN、数据包与许可（D6、D11–D16） |
| 单文件策略 + globals 注入 | 30 行写出第一个策略 | `BaseStrategy` 子类 + 版本化 manifest + `BacktestSpec` | 有条件借鉴：补命令行 sugar，不引入 globals |
| 六层配置 + `-mc` | 覆盖默认且可解释 | runtime JSON + project manifest + spec + `QX_*` 环境变量 | 借鉴“逐键来源”，不做六层合并 |
| mod 系统 + 两阶段 hook | 扩展不改内核 | `qx-plugin`（Matcher / FeeModel 两格）+ 沙箱 provider 选择 | 不采纳：会形成第二套装配平面 |
| `result.pkl` | 单一结果对象 | summary / record / `verify_run` / HTML / XLSX / JSON | 不采纳 pickle；借鉴“单一读回入口” |
| `--plot` / `--report` | 一眼看到结果 | `report --html/--xlsx/--json`，含产物复核 | 已实现，差在 wheel 用户可达性 |

一条边界要写清楚：rqalpha 的“数据默认值”是**全市场离线包 + 月度 CDN 更新**；千行采纳的是它的**体验**（一条命令拿到能跑的数据），不采纳它的**分发方式**（体积、许可、更新责任都与本仓的零依赖 wheel 与 `DatasetSnapshot` 血缘口径冲突，见 7.7）。因此数据侧的目标不是“再做一个 bundle”，而是“样例包保证装完能跑、真实数据按需可换源、每一份数据都带可复核来源”。

---

## 2. 千行现状

### 2.0 分层与职责【已实现】

按 `docs/module-map.md` 的权威划分，压缩成与“首跑/策略编写”相关的七层：

| 层 | 代表模块 | 职责 | 与易用性的关系 |
|---|---|---|---|
| 领域内核 | `qx-core` / `qx-guanxing` / `qx-xingban` / `qx-zhenlu` / `qx-risk` / `qx-genglu` | 确定性内核、行情语义、撮合、执行路由、风控、对账 | 决定“回测是什么”，不直接决定第一次怎么跑 |
| 应用用例 | `qx-app` | 回测/实验流程、产物身份与持久化、报告读回与核验 | 证据体系的事实 owner；快捷入口必须落回这里 |
| 数据面 | `qx-data` / `qx-provider` / `qx-datastruct` / `qx-storage` | 摄取、数据集、列式结构、存储 | 数据规范完整；首跑缺一份默认可用的数据 |
| 运行时与服务 | `qx-runtime` / `qx-orchestrator` / `qx-service` / `qx-api` / `qx-control` | 拓扑、worker 监督、服务装配、HTTP/控制面 | 能力强，但与“装完就能跑”不是同一条路径 |
| 语言边界 | `qx-strategy` / `qx-execution` / `qx-python` / `cpp/` | Rust 策略 API、执行端口、PyO3、C ABI | 策略作者概念量的来源 |
| 入口 | `qx-cli` / `python/qianxing` / `web/console/` | CLI、SDK、控制台 | 首跑体验的分裂点 |
| 扩展 | `qx-plugin` | 扩展点声明、独占冲突、依赖求解 | 克制；不承担“快速开始”职责 |

### 2.1 三个入口

| 入口 | 安装物 | 命令面 | 首跑路径 | 能力边界 |
|---|---|---|---|---|
| `qx-cli` | `cargo install --path crates/qx-cli` | 76 个具名命令（含子命令，`cli_args.rs`） | `qx-cli quickstart my-qx` | 全部 Rust 研究/服务端/worker/报告能力 |
| `qianxing` | `pip install` wheel | 16 个顶层命令、约 25 个叶子动作 | `qianxing project init` 后仍需补 spec | 研究用例、project 管理、策略诊断、远端研究 |
| `import qianxing` | 同一个 wheel | — | `run_backtest(spec)` | 稳定 SDK 面 |

`qx-cli` 的 `quickstart`【已实现】把 `init --strategy macd` → `doctor` → `backtest` → `report` → `status` 五步收进一条命令，失败时只回显失败那一步的命令原文并以 2 退出（`crates/qx-cli/src/quickstart.rs:90-200`）。它直调五个入口的同一批函数，`result_hash` 与逐条敲相同，这条相等由 `crates/qx-cli/tests/quickstart_first_run.rs` 钉住。

Python 侧没有对应物：`project init` 的脚手架只写 `qianxing.strategy.json`、`strategies/main.py` 与 `.gitignore`（`python/qianxing/strategy_project.py:122-177`），不带样例数据、不带 `input_spec`（评审时点事实；2026-10-30 首跑收口轮已按 D1 提供 `project init --demo`，落 9 份含内置合成样例与 `input_spec` 的文件，口径见 §4 D1 的状态行与 `docs/first-run.md` B2）；没有 `--strategy macd` 之类的内置策略选项；`project run` 在读取 `manifest["input_spec"]` 时要求它非空（`python/qianxing/cli.py:661-685`），静态 doctor 对缺失项报 `INPUT_SPEC_MISSING`（`python/qianxing/strategy_project.py:268`），本地预检则直接抛出 `input_spec is required`（同文件 `:668`）。

### 2.2 安装面【已实现】

- wheel 由 maturin 构建，`dependencies = []`（`python/pyproject.toml:32`），`abi3-py310`，脚本入口只有 `qianxing` 与 `qianxing-strategy-diagnose`（`:34-36`）；
- 可选 extras 覆盖 CCXT、A 股三类数据源、四类远端沙箱（`:38-81`）；
- `qx-cli` 不发布到 crates.io，只能从源码 `cargo install`；安装文档把三条路径写成 A（只装 CLI）、B（开发/发布环境）、C（Python 侧）三段（`docs/installation.md:17-125`）；
- 安装后的 `qianxing doctor` 会报告原生扩展、用例集与 CLI 的匹配状态。它是**事后核对**，不是安装时的强约束。

### 2.3 策略面【已实现】

`BaseStrategy`（`python/qianxing_strategy/base.py:414`）的钩子面：

| 类别 | 钩子 | 用途 |
|---|---|---|
| 生命周期 | `on_init` / `on_start` / `on_stop` | 初始化、启动、收尾 |
| 预热 | `history_requirements` / `on_warmup_complete` / `prenext` / `nextstart` / `next` | 声明预热并区分“未就绪/首个就绪/就绪” |
| 行情 | `on_bar` / `on_tick` / `on_order_book` / `on_timer` | 四类市场事件 |
| 成交回报 | `on_order_update` / `on_fill_update` / `on_trade` / `on_order` | 已提交事实的回报 |
| 决策 | `decision` / `buy` / `sell` / `close_position` | 构造版本化 intent |

一个可运行策略还需要：`STRATEGY_CLASS` 或 `create_strategy(spec)` 导出、可选的 `STRATEGY_MANIFEST`、以及运行时的 `BacktestSpec`。仓库自带的 `python/qianxing_strategy/long_only_sma.py` 是“只出决策、宿主负责撮合/风控/账务”的范式（`:38-64`），但它的参数校验、预热声明、历史读取、仓位读取、资金换算都写在策略里，首次阅读的概念量明显高于 rqalpha 的两个函数。

### 2.4 数据面【已实现】

**规范与产物**

- 版本化数据：`DatasetSpec` 校验、`dataset-ingest`、`dataset-bundle`、`DatasetSnapshot`、`BarManifest`、`data-validate`；
- `qx-cli init --strategy macd` 把 `deploy/qianxing.bar-frame.example.json` 复制进项目并把 `storage.data_dir` / `bars_snapshot_path` 锚到项目内（`crates/qx-cli/src/init_project.rs:424-452`）；
- **没有**“下载一份历史数据包”的命令：`cli_args.rs` 的命令面只有 `dataset-ingest` / `dataset-bundle`，没有 rqalpha 式的 `download-bundle`；仓库内也检索不到 `download-bundle` 的实现，只有竞品文档对 rqalpha 的提及。

**真实取数（能力已存在，但不成产品面）**

`python/qianxing_ashare/__init__.py`（1,900 行）已经是一套合格的适配层：

| 组成 | 位置 | 现状 |
|---|---|---|
| 查询与清单 | `:419`、`:450` | `AshareQuery` / `AshareManifest`，清单含 `provider`、`provider_version`、`source_hash`、`received_at` |
| 行归一 | `:795` | `normalize_bar_rows` 负责排序、去重、raw 缩放与精确十进制 |
| provider 协议 | `:647` | `AshareDataProvider`（`fetch` / `fetch_corporate_actions`） |
| 三个适配器 | `:1137`、`:1209`、`:1306` | AkShare / BaoStock / easy-tdx，全部惰性 import |
| 选源 | `:1438-1455` | `create_provider("auto")` 按 akshare → baostock → easy_tdx 取第一个已安装；**显式失败不静默换源** |
| bundle 组装 | `:530-615` | `build_dataset_bundle_manifest` 复用 Rust `app_assemble_dataset_bundle` |

缺 extra 时三个适配器都会抛带安装命令的 `AshareProviderError`（`python/qianxing_ashare/__init__.py:1146-1150`），这一点包内已经做对了；差的是把它接到统一错误信封与 `data doctor` 上。它同时具备三件别的“数据能力”往往缺的东西：**血缘清单**（清单里能读出用了哪个源、哪个版本、哪批字节）、**离线可测**（provider 以注入方式接收模块，`python/tests/test_ashare.py:199-210` 用 `_FakeAkShare` 全程不联网）、**跨语言同源**（`source_hash` 复用 Rust `qx-data` 的规范 BarFrame 指纹）。

但它的产品面只有一条：`python -m qianxing_ashare fetch|actions|bundle|screen`（`python/qianxing_ashare/cli.py:670`）。`qianxing` 命令表（`python/qianxing/cli.py:267-340`）里没有 `data`——这是评审时点事实，2026-10-30 首跑收口轮已按 D5 落地 `data sources/doctor/fetch/install` 等子命令（口径见 §4 D5 的状态行）；SDK 侧只有再导出 `qianxing.ashare`（`python/qianxing/ashare.py:3-4`）；wheel 打包清单包含 `qianxing_ashare`（`python/pyproject.toml:88`），第三方库在 extras 里（`:51-68`）。

**缺口**

1. 缺 **mootdx**（通达信协议）这一在国内常用的免登录源；
2. 没有取数缓存，也没有 `--offline`，重复取数与 CI 都依赖网络；
3. 没有跨源能力矩阵（频率 / 复权 / 公司行为 / 交易日历 / 凭据 / 许可 / 是否内置），用户无法在取数前判断“这个源给不给得了我要的东西”；
4. 取数与项目清单之间没有自动登记：取完的文件仍要手工写进 `input_spec`，`_INPUT_PATH_KEYS`（`python/qianxing/strategy_project.py:91-94`）只是校验它存不存在；
5. “显式失败不静默换源”的语义只存在于 A 股包内部，没有上升为全入口规则，CCXT 走的是另一条路（`python/qianxing_ccxt/provider.py:25`）。

上面 1–4 条到 2026-10-30 首跑收口轮已闭合（mootdx → D13，缓存与 `--offline` → D15，跨源能力矩阵 → D11 的 `data sources` + D12 的能力位，取数自动登记 → D14），第 5 条的前半句由 D12 的统一优先级链与 D16 的固定失败码接走；**CCXT 仍是独立适配面**这一点没有变：注册表里那一行显式写着"独立适配面（qianxing.ccxt），不经 data fetch 路由"（`python/qianxing_ashare/sources.py` 的 `ccxt` 条目），本轮没有把它并进 `data fetch`。

### 2.5 运行与证据【已实现】

- `project run` 先做策略与输入快照，再跑本地预检、记录 run、执行、登记产物并复核（`python/qianxing/project_runs.py:934-1015`）；stdout/stderr 有上限捕获，并发运行跳过捕获；
- 回测成功必须同时通过产物完整性检查，否则整轮记 `failed`（`ARTIFACT_INTEGRITY_INCOMPLETE`）；
- 报告由 `project report` 生成：目标是 HTML/XLSX/JSON 三选多，生成前保护输入与证据文件不被别名覆盖，生成后再次复核引擎产物（`python/qianxing/project_runs.py:2262-2330`）；
- 错误统一走 `project_error_envelope`，带 category、stage、run_id。

### 2.6 配置面【已实现】

- 一份 runtime JSON 是运行时权威；项目清单是项目权威；`BacktestSpec` 是单轮研究权威；
- `config explain` / `config fingerprint` / `config lock` 解释与锁定 runtime；`project plan` 产出本地或服务端执行计划（`python/qianxing/cli.py:625-633`）；
- 示例配置查找按 `QX_DEPLOY_DIR` → 可执行文件同级 → 上一层 → 构建期源码树 → 当前目录 → 内置模板六层解析（`docs/installation.md:64`）；
- 环境变量面很宽：`QX_PYTHON`、`QX_OPERATOR`、`QX_PYTHON_STRATEGY_EXECUTION_MODE`、`QX_PYTHON_STRATEGY_SANDBOX_REQUIRED`，以及四个沙箱后端各自的模板、镜像、密钥变量（`crates/qx-cli/src/strategy_host/providers.rs:40-180`）；
- 少数入口有显式优先级，例如 `--fee-bps` > runtime `cost_rules_path` 的 taker_bp > 内核默认（`crates/qx-cli/src/cli_help.rs:220`），但这类“逐键优先级”没有统一出口。

### 2.7 扩展与装配面【已实现】

- `qx-plugin` 只保留内核真正分派的两格：`Matcher` / `FeeModel`；规则是“显式 `replaces` 解决独占冲突、依赖拓扑决定顺序、一次装配不热替换”（`crates/qx-plugin/src/lib.rs:1-23`）；
- 远端策略执行有四个后端：E2B、llm-sandbox、OpenSandbox、MicroSandbox；每个后端在 Rust 侧做 worker 选择与环境准备，在 Python 侧各有一个持久 worker 桥（`python/qianxing_strategy/{e2b,llm_sandbox,microsandbox,opensandbox}_worker.py`，合计约 2,800 行），外加共享的 `sandbox_execution.py`（1,929 行）；
- 这套面是“能力广度”的证明，但也是首次安装/首次排障时最难解释的部分。

### 2.8 开发者工作流【已实现】

| 要做的事 | 命令 | 出处 |
|---|---|---|
| 装 CLI | `cargo install --path crates/qx-cli --locked` | `docs/installation.md:22` |
| 跑发布检查 | `build.bat` / `bash build.sh` | `docs/installation.md:70-73` |
| 构建 wheel | `powershell -NoProfile -ExecutionPolicy Bypass -File tools/build_python_wheel.ps1 -Python python\.venv\Scripts\python.exe` | `docs/installation.md:102` |
| 架构门禁 | `python -X utf8 tools/check_architecture.py --fast` | `tools/check_architecture.py` |
| 台账门禁 | `python -X utf8 tools/check_strategy_refactor_ledger.py` | `maturity/strategy-run-refactor-work-packages.json` |
| Python 全量 | `python -X utf8 -m unittest discover -s python/tests` | CI 同款 |
| 取 A 股数据（当前唯一入口） | `python -m qianxing_ashare fetch --provider akshare --code 000001.SZSE --start 20240101 --end 20241231 --output bars.json --manifest bars.manifest.json` | `python/qianxing_ashare/cli.py:670` |

开发细节里最容易被忽略的两条：① wheel 必须在本轮最后一次构建之后重打，扩展文件不能复用旧产物（`docs/installation.md:147`）；② 活文档里的 `path:NN` 引用会被架构门禁做可达性检查（文件存在 + 行号不越界），新增文档时行号范围要按当前文件长度核过。

### 2.9 规模热点【已实现】

| 文件/目录 | 行数 | 说明 |
|---|---:|---|
| `python/qianxing/app.py` | 3,539 | 用例门面 + 校验 + 序列化 + 编排 |
| `python/qianxing/project_runs.py` | 2,426 | run 记录、依赖锁、报告、事件、取消 |
| `python/qianxing/sandbox_execution.py` | 1,929 | 沙箱执行与输入打包 |
| `python/qianxing/cli.py` | 1,582 | 命令解析 + 分派 + 输出 |
| `python/qianxing_ashare/__init__.py` | 2,361 | 四个 A 股源适配 + 行归一 + 血缘清单 + 公司行为/日历（注册表/缓存/命令面已拆出） |
| `crates/qx-cli/src` | 49,514 | 研究、报告、worker、服务端命令 |
| `crates/qx-cli/src/workers/strategy_worker/` | 4,000+ | 执行更新、重放、预置命令 |
| `python/qianxing_strategy` | 9,097 | 策略基类 + 四个沙箱 worker + 诊断 |
| `python`（全量） | 115,453 | 含测试与桥接包 |

规模本身不是缺陷；“一个文件同时承担门面、校验、IO、编排、序列化”才是。R19 已经把 façade 拆出 `strategy_execution` / `client_local`，但测试仍在补丁路径上追着搬（本仓已出现过 `qianxing.app.*` 补丁失效的漂移）。

---

## 3. 架构设计不合理点

以下按影响排序。每条给出“现象—证据—为什么不合理—判定”，判定分 `改架构` / `改细节` / `不改`。

### A1【P0】wheel 用户没有零配置首跑路径

**现象**：`pip install qianxing` 之后，`qianxing project init` 可以建出脚手架，但 `project run` 跑不起来：清单里 `input_spec` 为 `null`，静态检查直接报 `INPUT_SPEC_MISSING`，本地预检抛 `input_spec is required`。用户必须自己写一份 BacktestSpec JSON、准备行情文件，才可能跑第一轮。

**证据**：`python/qianxing/strategy_project.py:151-164`、`:268`、`:668`；`python/qianxing/cli.py:661-685`。

**为什么不合理**：`qx-cli quickstart` 已经证明“零配置首跑”可以做到，而且不必牺牲证据（五步直调同一批函数，`result_hash` 逐字相同）。但这份能力只存在于 Rust 入口；Python 是产品对外的主安装面（wheel），却把首跑成本留给了用户。两条入口的“第一条命令”体验不一致，是当前最大的易用性缺口。

**判定**：改架构（补一条与 `quickstart` 对称、但落到同一 `BacktestSpec` 的路径）。

### A2【P0】能力面与安装面不一致：报告能力绑定在外部二进制上

**现象**：`qianxing project run` 通过 PyO3 在进程内执行；`qianxing project report` 却 `subprocess.run(["qx-cli", "report", …])`，找不到可执行文件时报 “qx-cli was not found; install the matching CLI or pass --cli PATH”。装 wheel 的用户要得到 HTML 报告，必须另装 Rust 工具链并 `cargo install`。

**证据**：`python/qianxing/project_runs.py:2292-2296`；`python/qianxing/cli.py:307-315`。

**为什么不合理**：报告是“回测结果”的一部分，不是另一个产品的功能。把渲染器留在 `qx-cli` 里，等于让安装方式决定能力边界；而 `qx-python` 已经能读回 run report（`app_read_run_report_v1` 等），只差渲染与文件落盘这一段。这也是分层问题：`qx-cli` 同时承担“命令行壳”和“可复用渲染库”，复用只能靠起进程。

**判定**：改架构。短期把 `qx-cli` 产物随 wheel 分发；长期把报告渲染下沉到库 crate，CLI 只做参数解析与退出码。

### A3【P0】数据没有“最后一公里”

**现象**：千行有完整的数据规范（spec / bundle / snapshot / manifest / 校验），但没有“装完就有一份能跑的数据”。Rust 入口的首跑只能用 `qx-cli init` 自带的一份样例 BarFrame，wheel 入口连这份都没有；真实 A 股/加密数据要装 extra、写 provider 配置、跑 ingest。

**证据**：`crates/qx-cli/src/init_project.rs:424-443`；`python/pyproject.toml:38-81`；`crates/qx-cli/src/cli_args.rs:137-155`（只有 ingest/bundle，没有下载命令）；`python/qianxing_ashare/cli.py:670`（真实取数确实存在，但入口在另一个包的 `__main__` 里）。

**为什么不合理**：数据规范越完整，首次使用越需要一份“合规、带血缘、可复核”的样例包来展示规范怎么用。rqalpha 用 `download-bundle` 把这一步变成一条命令；千行没有对应物，用户第一次接触 `DatasetSnapshot` 就是在自己造数据的时候。

**判定**：改架构（数据分发与样例包策略），但**不照搬 rqalpha 的数据包**：其数据许可不覆盖千行，且本次调研未见其读取侧的逐文件摘要校验。落点见 D6（样例包 + `data install`）与 D11–D16（多源取数）。

### A4【P1】策略作者要理解的概念数量仍然偏多

**现象**：写一个策略至少要知道：`BaseStrategy` 钩子选择、`STRATEGY_CLASS` 导出、`STRATEGY_MANIFEST`、`BacktestSpec`、`decision` 的返回形状、intent 的数量单位（`qty_raw`）、`history` / `position_qty` / `available_margin_raw` 的语义、预热声明与 `prenext` 的区别。

**证据**：`python/qianxing_strategy/base.py:414-570`、`:726-830`；`python/qianxing_strategy/long_only_sma.py:38-64`。

**为什么不合理**：这些概念在“生产级策略”里都是必要的，但**不应该全部出现在第一次阅读里**。rqalpha 用两个函数换来了低门槛，代价是边界模糊；千行不必复制它的代价，但需要一份“最小可用策略只需要 3 个概念”的入口。

**判定**：改细节（文档与模板分层），不改契约。

### A5【P1】配置平面分散，缺“本次运行有效配置”的统一解释

**现象**：一次 `project run` 至少涉及 runtime JSON、project manifest、BacktestSpec、策略内参数、`QX_*` 环境变量、沙箱后端配置。`config explain` 只解释 runtime；`project plan` 只覆盖项目三件套；环境变量与策略参数没有来源视图。

**证据**：`python/qianxing/cli.py:298-304`、`:625-633`；`crates/qx-cli/src/strategy_host/providers.rs:40-180`。

**为什么不合理**：rqalpha 的六层配置之所以好用，不是层多，而是“每一层可解释”。千行的配置更少、更严格，却更难回答“这个值最后为什么是这个值”。排障时用户只能逐个文件翻。

**判定**：改细节（新增只读的 explain 视图，不改变合并语义）。

### A6【P1】规模热点与测试耦合

**现象**：`app.py` 3,539 行、`project_runs.py` 2,385 行、`qx-cli` 49.5k 行；测试大量通过补丁门面模块来隔离，门面一拆，补丁就失效。

**证据**：§2.9 表；R19 拆分后 `test_sdk_layering` / `test_async_sdk` / `test_strategy_process_adapter` 的补丁路径迁移（`docs/architecture-local-server-refactor-review-2026-10-28.md` §15.1）。

**为什么不合理**：文件大小本身不是问题，问题是**一个文件同时拥有多个变化原因**。补丁路径是这种耦合的显影剂：拆分时测试被迫跟着改，说明测试断言的其实是“调用发生在哪个模块”，而不是“行为是否正确”。

**判定**：改架构（按函数族拆，测试同轮搬；只搬不改行为）。

### A7【P2】扩展与沙箱装配面过宽

**现象**：四个远端沙箱后端、各自的环境变量、各自的可选依赖、各自的 worker 桥；`qx-plugin` 只有两格扩展点。

**证据**：§2.7；`python/pyproject.toml:72-81`。

**为什么不合理**：扩展机制本身是克制的（两格、显式替换），但**执行后端**的选择面远大于扩展点，导致“装什么、配什么、为什么跑不起来”的组合爆炸。rqalpha 的 mod 系统把这类横切能力收进框架，但代价是更长的调用链；千行不需要 mod，但需要一个“默认后端 + 显式升级”的产品口径。

**判定**：不改能力，改默认值与文档（首跑只承诺 local；远端后端列为进阶）。

### A8【P2】版本、错误与文档口径仍按实现组织

**现象**：`qx-cli version` 有构建身份，`qianxing doctor` 能核对 CLI 匹配，但两者没有“安装时即保证”的关系；错误信封已经统一，但 Python CLI 顶层异常到 category 的映射仍分散在各命令分支；文档把 `cargo run -p qx-cli --` 与安装后的 `qx-cli` 混写（指南开头已声明替换规则，但新用户仍容易照抄）。

**证据**：`docs/installation.md:18-50`；`docs/工业化易用性收口指南-V1.md:7-10`；`python/qianxing/project_runs.py` 的 `project_error_envelope` 调用点。

**为什么不合理**：易用性的一半是“出错时知道下一步”。当前错误正文质量不低，但**入口之间不一致**：同一种失败在 `qx-cli` 与 `qianxing` 下可能是不同的 category、不同的退出码、不同的修复建议。

**判定**：改细节（统一 category/退出码表 + 单一首跑文档）。

### A9【P1】数据源能力存在但不成“产品面”：选源、缓存与血缘没有统一出口

**现象**：A 股三源适配器（akshare / baostock / easy-tdx）、CCXT 适配器、Rust 侧 `dataset-bundle` 各自成立，但五件事都没有：

1. `qianxing` 命令表里没有 `data`，真实取数只经 `python -m qianxing_ashare`；
2. 缺 mootdx（通达信协议）这一在国内常用的免登录源；
3. 没有取数缓存与 `--offline`，重复取数与 CI 都依赖网络；
4. 没有跨源能力矩阵，用户无法在取数前判断“这个源给不给得了我要的频率 / 复权 / 公司行为 / 日历”；
5. 取数结果与项目清单之间没有自动登记，取完仍要手工拼 `input_spec`。

这五条是**评审时点的现象记录**，2026-10-30 首跑收口轮已逐条落地：第 1 条见 D11（`qianxing data sources|doctor|env|fetch|actions|bundle|screen|install` 与 `python -m qianxing_ashare` 共用同一实现，命令链写在 `docs/first-run.md` B5）；第 2 条见 D13（`MootdxProvider` 进注册表，只给不复权行情、能力位显式 `false`，离线假模块用例）；第 3 条见 D15（内容寻址缓存 + `--offline/--refresh/--no-cache/--cache-dir`，CI 与单测不联网）；第 4 条见 D11 的 `data sources` 与 D12 的能力位注册表（每行给源 / 市场 / 频率 / 复权 / 公司行为 / 日历 / 凭据 / extra）；第 5 条见 D14（`init --demo --source …` 与 `run -f --source` 把取数结果直接写成数据集包 + `input_spec` + 血缘）。失败面由 D16 收口（固定 category 与退出码）。原有的“显式失败不静默换源”性质保留，并从 A 股包上移到 D12 的统一优先级链。

**证据**：`python/qianxing_ashare/__init__.py:1438-1455`；`python/qianxing_ashare/cli.py:670`；`python/pyproject.toml:51-68`；`python/qianxing/cli.py:267-340`；`python/qianxing/strategy_project.py:91-94`。

**为什么不合理**：数据是首跑的最后一道门槛，而当前取数能力藏在“另一个包的 `__main__`”里。rqalpha 用一条 `download-bundle` 把“拿到能跑的数据”变成命令；千行不必搬它的数据包，但必须把“选源 → 取数 → 登记血缘 → 喂给 spec”做成同一条命令链。否则 D1/D2 的零配置首跑只能用内置样例，一碰真实数据又回到手工拼装。已有的“显式失败不静默换源”是这条链最该保留的性质，却只活在 A 股包内部。

**判定**：改架构（数据入口与源注册表），不改取数实现（适配器已存在且离线可测）。

---

## 4. 细节改进清单

优先级：P0 = 首个 wheel 用户能完成回测；P1 = 减少排障与理解成本；P2 = 结构整理与可选能力。

2026-10-30 收口后的状态（逐条证据写在各条目的**状态**行里）：D1–D6、D9–D16【已实现】；D7、D8【部分已实现】，未做的部分留在原条目里标【设计意图，未排期】；D3 只落选项 A。D5 的进阶页在 2026-10-31 补上（`docs/strategy-hooks.md`）。首跑命令链与本轮实测读数集中写在 `docs/first-run.md`。

### D1 `qianxing run -f strategy.py -s … -e … -a …`（P0）

**状态【已实现】**：`python/qianxing/cli.py` 的 `run` 命令 + `python/qianxing/run_spec.py` 合成同一份 `BacktestSpec`，日期只收 `YYYYMMDD` / `YYYY-MM-DD`，`-a` 按大单位换算一次。`result_hash` 与显式 spec 逐字符相同由 `python/tests/test_native_project_runs.py` 的离线首跑用例钉住；命令链与本轮实测读数见 `docs/first-run.md` B6。

**做什么**：新增一个顶层命令，把“策略文件 + 起止日期 + 初始资金 + 标的”合成为标准 `BacktestSpec`，复用现有 `run_strategy_file` 路径与默认输出目录。JSON 仍然可以显式传（`--spec`），两者互斥。

**落点**：`python/qianxing/cli.py` 命令表 + 一个薄合成函数；不新增 schema，不新增依赖。

**验收**：同一策略、同一行情下，`run -f …` 与手写等价 `BacktestSpec` 的 `result_hash` 逐字相同；用一个现有 owning test 扩一条断言，不新开测试文件（避免重定价用例总数面）。

**风险**：日期/资金单位必须与 spec 的 raw 单位一致，合成器要做唯一一处换算并给出可读错误；默认数据来源必须显式（项目内 fixture 或 `--bars PATH`），不允许静默找仓库 `deploy/`。

### D2 `qianxing project init --demo`（P0）

**状态【已实现】**：`init_strategy_project(demo=True)` 落 9 份文件（内置单钩子策略 + `input.json` + 合成数据集包四件套），`is_sample: true` 与许可文案写进清单，`project doctor` 零 issue、`project run` 退 0 的用例同在 `test_native_project_runs.py`。

**做什么**：在现有脚手架基础上增加一个可选模式：写内置策略（复用 `long_only_sma` 或内置 MACD 语义）、写一份样例 BarFrame、写一份 `input_spec`，使 `project doctor` 与 `project run` 建完即可通过。

**落点**：`python/qianxing/strategy_project.py` 的 `init_strategy_project`；样例数据与 spec 走项目内相对路径，`.gitignore` 不变。

**验收**：干净临时目录 → `project init --demo` → `project doctor` 零 issue → `project run` 退 0 且产物可 `project artifacts` 复核；文件份数写进用例（扩展现有 `python/tests/test_native_project_runs.py`）而不是文档。

**风险**：默认生成的数据必须标注“样例/合成”，不能冒充真实历史；manifest 里要能看出数据来源。

### D3 报告能力随 wheel 分发（P0/P1，需产品决策）

**状态：选项 A【已实现】，选项 B【设计意图，未排期】**。
A 已落地：`tools/build_python_wheel.ps1` / `.sh` 在 `pip wheel` 之前 `cargo build --release -p qx-cli` 并把宿主平台二进制装进 `python/qianxing/_bin/`（先后顺序由 `tools/check_architecture.py` 的 wheel 构建器判据核对，包内载荷由 `tools/check_python_wheel.py` 逐名核对，`_bin/` 作为构建副产物进 `.gitignore`）；渲染器按 `--cli` > 内置副本 > PATH 单链选中即用，`qianxing doctor` 的 `report_renderer` 报`bundled` / `external` / `missing`，不做静默回落（用例：`python/tests/test_project_runs.py` 的`test_report_renderer_prefers_the_bundled_copy_and_never_falls_back_silently` 与`test_project_report_records_missing_renderer_without_running_a_substitute`，`test_app_use_cases.py` 钉 doctor 面）。B（把渲染下沉到库 crate、去掉子进程与超时）仍未排期。

**现状**：`project report` 依赖外部 `qx-cli`。

**三个选项**：

| 选项 | 做法 | 优点 | 代价 |
|---|---|---|---|
| A | wheel 内带平台对应的 `qx-cli` 可执行文件，`project report` 优先用内置副本 | 改动最小；行为与 CLI 完全同源 | wheel 体积上升；构建矩阵与签名要覆盖二进制；需处理“用户显式 `--cli` 优先” |
| B | 把报告渲染从 `qx-cli` 下沉到库 crate，`qx-python` 暴露渲染函数 | 分层最干净；无子进程、无 300s 超时 | 需要拆 crate + 迁测试；一轮做不完 |
| C | 单独发布 `qianxing-cli` 的 pip 包 | 安装仍是 pip | 又回到“装两个包”，没有解决根因 |

**建议**：短期 A（先让 wheel 用户能出报告），中期 B（把 `report_html` / `report_xlsx` / `report_svg` 变成可复用库）。C 不采纳。

**验收**：干净 venv 只装 wheel → `project report` 退 0；HTML/XLSX/JSON 三格式由同一渲染器产生，且产物复核通过；`doctor` 增加 `report_renderer` 检查项，报告“内置/外部/缺失”。

### D4 `qianxing project plan --explain`（P1）

**状态【已实现】**：`project plan --explain` 由 `python/qianxing/project_explain.py` 逐键给`value` / `source` / `overridden`（路径键另带 `original_value` 与 `resolution`），数据侧独立一格`data_selection`。脱敏与来源标签由 `python/tests/test_strategy_project_plan.py` 的`test_local_explanation_redacts_values_history_and_nested_fields` 与`test_explicit_selectors_override_manifest_and_keep_original_paths` 钉住。

**做什么**：在现有 plan 输出上增加逐键来源：`runtime` / `manifest` / `spec` / `default` / `env`，并标出被覆盖的值。只读，不改变合并语义。

**落点**：`local_project_preflight` 的返回结构 + CLI 渲染；复用 `config explain` 的 JSON 形状。

**验收**：对同一个项目，`--explain` 的每个键都能在来源文件里找到；删掉一份来源后，对应键的来源标签变化且值随之变化（负向断言）。

### D5 策略作者入口分层（P1）

**状态【已实现】**：`project init --demo` 的模板保持单钩子（只有 `on_bar`，行数未增），首跑页 `docs/first-run.md` 只走 `on_bar` / `decision` / `buy` / `history` 这条最小链路；tick / order book / 定时器 / 成交回报 / 预热细节已拆到 `docs/strategy-hooks.md`，文档地图（`docs/project-guide.md`）与首跑页 B6 末尾各有一处入口。进阶页的命令一律从仓库根目录用 `python -m qianxing.cli` 执行，读数写在页内：Tick `snapshots=80 fills=1`、OrderBook `fills=1 tier=l2`、成交回调 `fills=2`、经典生命周期与 `on_bar` 版同读 `fills=1 return_bps=1`。同轮修掉两处入口事实偏差：本地 `run-strategy` 桥此前只派发 `on_event`，`prenext/nextstart/next` 只存在于 Runtime 类宿主，随包 `strategy_file_nextstyle.py` 因此跑出 0 成交；桥已按与 `class_adapter.py` 相同的谓词派发，并由 `python/tests/test_strategy_base.py` 的预热用例钉住。Tick 示例 spec 的 `depth_path` 由 `../deploy/...` 改为仓库根相对的 `deploy/...`，`python/README.md` 公布的 Tick 两条命令现在能在仓库根直接执行。模板未被改动，按首跑页从零建的项目 `project doctor --dynamic` 仍为 `ready`、`project run` 的 `result_hash=095d45d044ddc688` 与首跑页登记的读数一致。

**做什么**：首屏文档只教三个概念：`on_bar`、`decision`/`buy`/`sell`、`history`/仓位查询；把 tick/order book/定时器/成交回报/预热细节移到“进阶”页。`project init` 模板保持单钩子，不增加注释密度。

**落点**：`docs/` 单一首跑页 + `strategy_project.py` 模板注释。

**验收**：按首跑页从零写出的策略能通过 `project doctor --dynamic`；模板文件行数不增加。

### D6 样例数据包与 `data install`（P1，落地在 P3a）

**状态【已实现】**：`qianxing data install --sample --target <目录>` 离线生成并校验内置合成包，非 `--sample` 时逐文件复核摘要、缺一份即拒收；安装走 `DatasetBundleManifest` / `DatasetSnapshot` / `BarManifest`，不引入下载依赖。许可与来源在清单里可见（`is_sample`、`license`），`project show` 一并读出。

**做什么**：提供一份**许可清晰、带 manifest/snapshot** 的小型样例数据包（合成数据或明确授权的公开数据），以及一个 `qianxing data install <bundle>` 命令：校验 bundle、落项目内数据目录、登记 snapshot。默认不联网。它是 D11 命令面的一部分，不另立顶层命令。

**落点**：复用 `dataset-bundle` / `DatasetSnapshot` / `BarManifest`；安装器只做校验与落盘，不引入下载依赖。

**验收**：离线环境安装样例包 → `run -f` 首跑 → `verify_run` 通过；包内每个文件都有摘要，缺一份即拒绝安装。

**风险**：不能把“样例”写成“历史行情”；许可与来源必须进包内清单并在 `project show` 可见。

### D7 错误面统一（P1）

**状态【部分已实现】**：Python 侧已有两张固定表——`project_runs._ERROR_CATEGORY_BY_TYPE` / `_ERROR_REMEDIATION`（终态失败经 `project_error_envelope` 出 `code/category/retryable/stage/cause/remediation`）与 `qianxing_ashare.sources.DATA_ERROR_EXIT_CODES`（取数八档：invalid-request 2 / credentials 3 / missing-extra 3 / network 4 / partial 5 / unsupported 6 / cache-miss 7 / provider 8）。取消仍走 130、CLI 用法错误仍走 `QianxingError` → 2，这两格不动。**这张表现在有对家**（2026-10-30 收口轮）：`python/tests/test_ashare.py` 的 `test_data_error_ladder_is_the_only_source_of_data_exit_codes` 钉住表值本身、每档的 `exit_code_for` 与 `AshareProviderError.exit_code`、未知 category 回落 `provider`、从产出方源码读出的词汇表与表的两个集合差（`credentials` 是表里唯一没有产出方的保留行），以及 `docs/first-run.md` 公布的那一串数字。**尚未做**：`qx-cli` 与 `qianxing` 两侧同一故障共用一张 category 表（Rust 侧词汇表不同源），登记为【设计意图，未排期】。

**做什么**：给 Python CLI 顶层异常建立一张固定映射表（异常类型/字符串 → category → 退出码 → 建议命令），并让 `qx-cli` 的同类错误使用同一 category。`project_error_envelope` 已经是载体，补的是“入口一致性”。

**落点**：`python/qianxing/cli.py` 的顶层 `except` + `project_runs.project_error_envelope`；不改变已有错误正文。

**验收**：一张表驱动用例：每个 category 至少一条触发路径，断言 category 与退出码；同一故障经 `qx-cli` 与 `qianxing` 触发时 category 相同。**口径核对（2026-10-30）**：【部分已实现】七个有产出方的 category 各有触发用例断言 category（`python/tests/test_ashare.py`），退出码由上面那条表驱动用例统一钉住，其中 `invalid-request` / `unsupported` / `partial` 三档另有进程退出码实弹断言；跨二进制同 category 未做。

### D8 规模热点“搬而不改”（P2）

**状态【部分已实现】**：本轮按族搬出的有 `run_spec.py`（spec 合成）、`project_explain.py`（解释视图）、`sample_data.py`（合成样例）、`report_renderer.py`（渲染器选择）、`qianxing_ashare/{sources,cache,cli}.py`（注册表 / 缓存 / 命令面），`app.py` 因此从 `4a70e850` 的 5,015 行降到 3,539 行；`project_runs.py` 的 run 记录 / 依赖锁 / 报告 / 事件四族与 `qx-cli` worker 树仍未排期。

**做什么**：按函数族拆分，每轮只搬一个族：

1. `app.py`：校验/IO/编排三段先分文件，公开面不变（用现有 layering 用例锁住）；
2. `project_runs.py`：run 记录与存储、依赖锁、报告生成、事件读取四族；
3. `qx-cli` worker 树：`execution_updates` 与 `prepared_command_replay` 已基本成族，先锁公开接口再搬。

**纪律**：不顺手改行为；测试与实现同轮搬；每轮结束跑同一组门禁；不新开测试文件，优先扩展现有 owning test。

**验收**：每轮 `git diff` 只含移动与必要的 `use`/模块声明；行为用例零改动通过；架构检查片数/检查数不下降。

### D9 文档按“用户路径”重组（P2）

**状态【已实现】**：新增单页 `docs/first-run.md`，按 wheel / cargo 两支给可复制命令，每条命令标明属于哪个二进制；`README.md` 的「安装」、`docs/installation.md` 的「安装」与 C 段、`docs/project-guide.md` 的文档地图都改为指向它（首跑不再只让人翻 README）。

**做什么**：新增单一首跑页，按安装方式分两支（wheel / cargo），所有命令可直接复制；`工业化易用性收口指南` 保留为能力全景，不再承担首跑职责；明确“哪条命令属于哪个二进制”。

**验收**：文档中的每条首跑命令在一个干净环境里逐个执行通过（可用现有文档行为用例的做法，把命令当数据源解析执行）。

### D10 默认执行后端口径（P2）

**状态【已实现】**：`doctor` 增加 `strategy_runtimes` 一格——`local_default: trusted_python`、本机后端 `available: true`、`remote_selected: null`，四个远端后端即使 extra 已装也只登记`readiness: unverified`，并注明首跑只承诺本机。用例在 `python/tests/test_app_use_cases.py`。

**做什么**：首跑文档与 `project init` 只承诺 local 后端；远端沙箱列为进阶能力，并在 `doctor` 中给出“本机可用后端”一行。不改后端实现，不改默认值语义。

**验收**：`doctor` 在未装任何沙箱 extra 时仍退 0，并明确打印 local 可用、远端未配置。

### D11 `qianxing data` 统一取数入口（P0）

**状态【已实现】**：`qianxing data sources|doctor|env|fetch|actions|bundle|screen|install` 全部落在`python/qianxing_ashare/cli.py`，`qianxing` 顶层与 `python -m qianxing_ashare` 共用同一份实现与同一个 `main(tokens, prog=...)`，取数逻辑不复制；`data doctor` 在一个 extra 都没装时逐源打印安装命令并退 0（本轮实测读数见 `docs/first-run.md`）。

**做什么**：新增顶层 `qianxing data`，把现有 `fetch` / `actions` / `bundle` / `screen` 提升为产品命令，并补两个只读命令：

- `data sources [--json]`：打印源注册表与能力矩阵（源 / 市场 / 频率 / 复权 / 公司行为 / 交易日历 / 凭据 / 许可标签 / 是否内置 / 对应 extra）；
- `data doctor`：打印本机每个源的状态（可 import、版本、凭据是否就绪、缺哪个 extra），未装任何 extra 时退 0。

旧入口 `python -m qianxing_ashare …` 保留为兼容别名，二者走同一实现，不复制取数逻辑。

**落点**：`python/qianxing/cli.py` 命令表 + 一层薄分派；取数仍留在 `qianxing_ashare`。

**验收**：同一查询经 `qianxing data fetch` 与 `python -m qianxing_ashare fetch` 产出字节相同（`source_hash` 相同）；未装 extra 时 `data doctor` 退 0 并逐源打印“未安装 + 安装命令”，`data fetch` 给可执行错误而不是 ImportError 栈。

**风险**：不要把 `data` 做成第二个包管理器；命令只做“选源 + 校验 + 落盘 + 登记”。

### D12 源注册表与唯一优先级链（P0）

**状态【已实现】**：注册表在 `python/qianxing_ashare/sources.py`（名称 / 模块 / extra / 能力位 / 凭据 / 许可），优先级解析在 `resolve_data_config`：`--source` > 清单 `data.source` > `QIANXING_DATA_SOURCE` > 内置样例，`auto` 是唯一允许顺序挑源的写法，且实际命中源写进清单与 `plan --explain` 的 `data_selection`；其余环节失败即停，错误正文点名源、原因与 extra。用例：`python/tests/test_ashare.py` 的四级覆盖与`test_mootdx_unsupported_requests_never_load_provider` 等。

**做什么**：把三个 A 股源、CCXT、内置样例收进一张注册表（名称、模块名、extra 名、能力位、许可标签、是否内置），并把取数优先级固定成一条链：

```
--source > 项目清单 data.source > QIANXING_DATA_SOURCE > 内置样例
```

`--source auto` 是**唯一**允许“按顺序挑第一个可用”的写法，且必须把最终生效的源写进清单与 `plan --explain`。其余任何一步失败都停下报错，**不**回落到下一个源——这正是 `create_provider` 现有语义的上升。

**落点**：`qianxing_ashare.create_provider` 的候选表提升为公开注册表；`project plan --explain` 增加一行 `data.source`（来源标签 + 最终值）。

**验收**：四级覆盖各一条用例（旗标覆盖清单、清单覆盖环境变量、环境变量覆盖默认、全缺省用样例）；任一环失败时不换源，错误正文点名“哪个源、为什么不可用、装哪个 extra”；`auto` 命中后 `plan --explain` 能读出实际源。

**风险**：新增一个环境变量就是新增一个配置面，必须写进同一张解释表，不能成为第二套隐式配置。

### D13 mootdx 适配器（P1，需产品决策）

**状态【已实现】**：`MootdxProvider` 只提供不复权行情，公司行为位显式 `false`；extra 名为 `a-share-mootdx`，刻意不并入 `a-share` 元 extra（`python/pyproject.toml` 的注释写明理由），注册表与 `data sources` 自动带出一行。四条离线假模块用例：`test_mootdx_lazily_imports_quotes_and_normalizes_frame`、`test_mootdx_partial_history_is_not_reported_as_complete`、`test_mootdx_schema_empty_and_version_failures_are_categorized`、`test_mootdx_unsupported_requests_never_load_provider`（`python/tests/test_ashare.py`），无联网用例。

**做什么**：按现有 provider 形状新增 `MootdxProvider`（`fetch` / `fetch_corporate_actions` 的能力边界以该库实际提供为准，缺的能力位就标“不支持”，不做假实现），并新增 `a-share-mootdx` extra。**默认不进 `a-share` 元 extra**，理由见 7.6。

**落点**：`python/qianxing_ashare/__init__.py` 的 provider 表 + `python/pyproject.toml` 的 extras；注册表与 `data sources` 自动带出。

**验收**：全部用假模块注入的离线契约用例（沿用 `python/tests/test_ashare.py:199-210` 的 `_FakeAkShare` 模式）覆盖行归一、缺列、空结果、版本不符；不新增联网用例。

**风险**：通达信协议无官方 SLA、服务器地址会变；公司行为与复权口径弱于 akshare/baostock。因此它只能是“多一个源”，不能成为首跑默认源。

**新源接入契约（mootdx 与之后任何源同样适用）**：

1. 只暴露 `fetch` / `fetch_corporate_actions`；源不提供的能力显式标为不支持（能力位 false），不做降级填充；
2. 产出必须是 `BarFrame` + `AshareManifest`，清单字段齐全（`provider` / `provider_version` / `source_hash` / `received_at` / 区间 / 复权）；
3. 第三方库惰性 import，模块名同时进注册表与 extra，不进入 `dependencies`；
4. 必须带假模块注入的离线契约用例（行归一 / 缺列 / 空结果 / 版本不符），联网测试不入 CI；
5. 错误映射到固定 category（D16），凭据不进日志、不进 manifest；
6. 文档侧只加两处：能力矩阵一行 + 许可标签；不新写一篇“某源使用指南”。

### D14 首跑与真实数据的接线（P0/P1）

**状态【已实现】**：`project init --demo --source … --instrument … --start … --end …` 走取数并把结果写成项目内数据集包 + `input_spec` + 血缘（`_write_fetched_dataset`）；`run -f … --source` 同义，缺省只读本地已有数据、不联网。两条路径的 spec 都过 `verify_run`；离线用例断言取数函数被调用即失败（`fetch_bars` 打桩抛 `offline demo attempted a fetch`）。

**做什么**：把 D1/D2 的入口与 D11 的取数接起来：

- `project init --demo` 默认写合成样例，清单标 `is_sample: true`；
- 加 `--source/--instrument/--start/--end` 后改走 `data fetch` 取真实数据，自动写入 `input_spec` 与血缘清单；
- `run -f` 同样接受 `--source`，缺省仍只用本地已有数据，**不联网**。

**落点**：`python/qianxing/strategy_project.py` 的 `init_strategy_project` + `cli.py`；复用 `_INPUT_PATH_KEYS`（`python/qianxing/strategy_project.py:91-94`）与 `build_dataset_bundle_manifest`（`python/qianxing_ashare/__init__.py:530-615`）。

**验收**：离线用 sample 建项目 → `project doctor` 零 issue → `project run` 退 0；用真实源（或有缓存）建项目 → `project show` 能看到源/版本/区间/是否样例；两条路径生成的 spec 都能被 `verify_run` 复核。

**目标形态**（与 rqalpha 的“两条命令起步”对齐，但每一步都落回证据链）：

```bash
pip install "qianxing[a-share-akshare]"
qianxing project init demo --demo --source akshare \
    --instrument 000001.SZSE --start 20240101 --end 20241231
cd demo
qianxing project run .
qianxing project report . <run_id> --format html,xlsx,json
```

不装 extra、不写 `--source` 时，前两条命令退化为“合成样例 + 零配置”，第 3、4 条完全相同——这正是“装完就能跑”与“换真实数据只需一个旗标”的分界。

**风险**：样例与真实数据必须一眼可辨；`is_sample` 缺失即拒绝入库。

### D15 内容寻址缓存与 `--offline`（P1）

**状态【已实现】**：内容寻址缓存在 `python/qianxing_ashare/cache.py`（键含源 / 标的 / 频率 / 复权 / 区间 / 请求版本，根目录由 `QIANXING_DATA_CACHE` 或 `--cache-dir` 决定），`--offline` 未命中即报 `cache-miss` 并列出缺失键、`--refresh` 强制重取、`--no-cache` 读写同关，`--offline` 与后两者互斥（`invalid-request`）。用例：`test_fetch_cache_hit_corruption_refresh_and_offline`、`test_no_cache_disables_reads_and_writes_for_bars_and_actions`、`test_offline_no_cache_rejected_before_project_or_fetch_io`、`test_cache_atomic_writers_and_failed_replace_keep_coherent_generations` 等（`python/tests/test_ashare.py`）。缓存不进证据链：run 读的仍是 `source_hash` 与快照摘要。

**做什么**：取数结果按“源 + 标的 + 频率 + 复权 + 区间 + 请求版本”写入内容寻址缓存（默认用户缓存目录，可用 `QIANXING_DATA_CACHE` 改），另存请求键清单；`--offline` 只读缓存，未命中即报错并列出缺失键；`--refresh` 强制重取。缓存不参与证据链：run 仍以 manifest/snapshot 摘要为准。

**落点**：`qianxing_ashare` 一层薄缓存封装，不改 `normalize_bar_rows` 与清单格式。

**验收**：同一查询第二次取数不触网（假 provider 计调用次数）；`--offline` 命中退 0、未命中退非 0 且错误正文列键；改区间或改复权后缓存不误命中。

**风险**：缓存目录是新的写面，必须可清理、可禁用；缓存不是“数据版本”，清单里的 `source_hash` 才是。

### D16 取数失败面（P1）

**状态【已实现】**：四类失败各有固定 category 与退出码（表见 D7），网络类只对幂等请求做有上限指数退避（`--retries`，0..9，默认 2），多标的默认整批失败零产物、`--allow-partial` 才落盘并逐条登记失败项，能力不支持点名能力位与可替代源。断言用例集中在 `python/tests/test_ashare.py`（`retryable` 只对 `network` 为真、`unsupported` 档退出码取自表、`partial` 未加开关时零产物；八档退出码整表由 `test_data_error_ladder_is_the_only_source_of_data_exit_codes` 钉住，`missing-extra` / `unsupported` 两类的建议命令与能力位文本另各有断言）。**其中“凭据缺失只说放哪、不打印值”这一格【设计意图，未排期】**：A 股取数面没有 token 类源，`credentials`（退 3）在表里是保留行、没有任何产出方；注册表的 `credentials` 列只有 `none` 与 ccxt 行的 `optional`，而 ccxt 行标注 `fetchable=False`、不经 `data fetch` 路由。CCXT 适配面自带凭据探针（`python/qianxing_ccxt/private_probe.py` 只回环境变量名与 `<未配置>` 占位，不打印值），但它不在这张退出码表的通路上。

**做什么**：四类失败各有固定 category 与建议命令：

| 失败 | 语义 |
|---|---|
| 网络/超时/限频 | 只对幂等请求重试（指数退避 + jitter、次数有上限、可配），超限后报“建议改用另一个源或稍后重试” |
| 多标的**部分成功** | 默认整批失败、不落半份；`--allow-partial` 才落盘并逐条登记失败项 |
| 凭据缺失（token 类源） | 报“需要哪种凭据、放在哪”的命令，不打印凭据本身 |
| 源不支持该能力（如该源无复权） | 点名能力位与可替代源，不静默降级成不复权 |

**验收**：四类各一条用例，断言 category / 退出码 / 建议命令；部分成功用例断言“未加开关时零产物”。**口径核对（2026-10-30）**：【部分已实现】`missing-extra` / `unsupported` / `partial` / `network` 四类各有用例断言 category，退出码另有整表用例与 `invalid-request` / `unsupported` / `partial` 三条进程退出码实弹；建议命令文本目前只对 `missing-extra` 与 `unsupported` 两类做了断言（含能力位与可替代源）；凭据一类没有产出方，见上。跨二进制同 category 见 D7。

---

## 5. 分阶段重构方案

阶段划分按“用户可见价值 / 改动风险”排，不按模块排。每个阶段都必须能单独回滚。

### P0 判据与口径（0.5–1 周）

**状态【已实现】（2026-10-30 收口轮）**：三列各自有命令、退出码与产物断言——wheel-only 是 `docs/first-run.md` B 支（`init --demo` → `project run` → `project report` 三格式，全程不带 `--cli`），cargo-only 是 A 支（`qx-cli quickstart`，`crates/qx-cli/tests/quickstart_first_run.rs` 钉 `result_hash` 相等），wheel+qx-cli 是显式 `--cli PATH` 那一格（`python/tests/test_project_runs.py` 的渲染器选择用例 + `tools/check_python_wheel.py` 核对包内载荷）。不承诺边界写在 `docs/first-run.md` 末节：远端沙箱、服务端部署、实盘就绪。矩阵以文档 + 用例两处落地，没有另立机器可读文件。

- 写一张“首跑验收矩阵”：wheel-only / cargo-only / wheel+qx-cli 三种安装，各自承诺到哪一步（回测 / 报告 / 取消 / 服务端），以及**数据从哪来**（内置样例 / `qianxing[a-share-*]` / 自带文件）——三列必须分别可跑，不能只测“样例能跑”；
- 把矩阵落成行为用例：优先放进现有 `crates/qx-cli/tests/quickstart_first_run.rs`、`python/tests/test_native_project_runs.py` 与 `python/tests/test_strategy_project_plan.py`，不新开文件；
- 明确“不承诺”的边界（例如 wheel-only 不承诺服务端部署），写进同一张表。

**退出条件**：矩阵中每一格都有命令、退出码、产物断言；没有“设计上应该有但没人跑过”的格子。

### P1 wheel 首跑（1–2 周）

**状态【已实现】（2026-10-30 收口轮）**：D1 `run -f`、D2 `init --demo`、D4 `plan --explain`、D11 的 `data sources` / `data doctor`、D12 优先级链、D14 接线全部落码；零新增强制依赖（`dependencies` 仍为空），未新增 schema，快捷入口仍走 `BacktestSpec` → `RunRecord` → `verify_run`。

- D1 `run -f` + D2 `project init --demo` + D4 `plan --explain`；
- D11 的两个只读命令（`data sources` / `data doctor`）+ D12 优先级链 + D14 接线：首跑既能用内置样例，也能一条命令换成真实源；
- 只动 Python 侧与文档；不新增强制依赖（数据源仍走 extras）、不新增 schema；
- 复用 `BacktestSpec` / `RunRecord` / `verify_run`，不新开证据链。

**退出条件**：干净 venv 只装 wheel，三条命令内完成一轮可复核回测；`result_hash` 与等价手写 spec 相同；`project artifacts` 通过；只装 `qianxing[a-share-akshare]` 时同一组命令能改用真实数据并留下血缘清单。

### P2 报告能力打包（2–3 周，需在 D3 选项中拍板）

**状态【已实现】（2026-10-30 收口轮；本阶段范围就是选项 A）**：wheel 在 `pip wheel` 前装载宿主 `qx-cli` 进 `qianxing/_bin`，`doctor` 增加 `report_renderer` 一格，错误正文区分 `missing` / `mismatch` / `timeout` 并点名 `--cli PATH`，且承诺不做静默回落。选项 B（渲染下沉库 crate）仍未排期。

- 先做 A（内置二进制），把 B 作为独立重构项排期；
- `doctor` 增加渲染器检查；`project report` 的错误正文区分“未安装/版本不匹配/超时”。

**退出条件**：wheel-only 用户能出 HTML/XLSX/JSON；报告载荷与 `qx-cli` 同源；无 `qx-cli` 时的错误可操作。

### P3 数据包与多源取数（3–4 周）

**状态【已实现】（2026-10-30 收口轮）**：P3a 样例包与 `data install`、P3b mootdx + 注册表能力矩阵、P3c 内容寻址缓存与 `--offline` / `--refresh` / `--no-cache`、P3d 取数失败面四档全部落码；数据来源与真实历史的边界在清单（`is_sample` / `license`）、`project show` 与 `plan --explain` 三处可见。不接 rqalpha 的 CDN、不引入联网默认行为——单测里取数函数被打桩成抛错即失败。

- P3a（先做）：D6 样例数据包 + `data install` + 包内许可/血缘清单；离线安装 → 首跑 → 复核；
- P3b：D13 mootdx（若产品拍板）+ 注册表与能力矩阵补全；
- P3c：D15 缓存与 `--offline`；
- P3d：D16 取数失败面；
- 数据来源与真实历史的边界写进 `project show` 与报告页脚；
- 不接 rqalpha 的 CDN，不引入联网默认行为。

**退出条件**：离线安装样例包 → 首跑 → 复核通过；缺文件/摘要不符时安装失败且错误可读；`data sources` 的每一行都能对上一个真实能力或明确的“不支持”；缓存命中时断网可跑；四类取数失败的错误可操作。

### P4 结构与文档（3–4 周，可与 P2/P3 并行）

**状态【部分已实现】（2026-10-30 收口轮）**：D9 首跑页与三处文档路由、D10 后端口径已完成；D8 搬出了 `run_spec` / `project_explain` / `sample_data` / `report_renderer` 与 `qianxing_ashare` 的三个模块，`project_runs.py` 四族与 `qx-cli` worker 树未搬；D7 的跨二进制同表未做，登记为【设计意图，未排期】。

- D7 错误面统一；D8 三个热点按函数族搬；D9 首跑页；D10 后端口径；
- 每轮只搬一个族，门禁全绿再进入下一族。

**退出条件**：热点文件的“变化原因”降到单一维度；行为用例零改动通过；文档命令逐个可执行。

### P5 明确不排期

**状态：保持不排期**（globals 注入 / mod 系统 / pickle 结果对象三项均未采纳，本轮没有改变这一判定）。

- 策略 globals 注入（`from qianxing.api import *` 式）；
- 引入 mod 系统或第二套插件装配面；
- pickle 结果对象。

---

## 6. 验收与门禁

### 6.1 当前基线（2026-10-30 当轮实测）

| 门禁 | 命令 | 当轮结果 |
|---|---|---|
| 架构不变量 | `python -X utf8 tools/check_architecture.py --fast` | 4 片，完整覆盖 336 个具名检查且无重复（125.23 s） |
| 台账 | `python -X utf8 tools/check_strategy_refactor_ledger.py` | 退出码 0（R01–R23 valid，23 个验收场景、132 个产物绑定运行通过） |
| Python 全量 | `python -X utf8 -m unittest discover -s python/tests` | 589 tests，OK（skipped=5），468.929 s |
| Rust 单元锚点腿 | `cargo test -p qx-cli --release --no-fail-fast --bin qx-cli` | 红 39 条（rc 101，墙钟 248.01 s，通过 446 条）——逐簇 A 快照守卫 19 / B 角色证据 9 / C 沙箱权限 0 / 名单锁 11，均登记为在途重构待裁决，见 `docs/changelog/current.md` |
| 格式 | `cargo fmt --all -- --check` | rc 0（Diff in 提示 0 条） |
| 空白 | `git diff --check` | rc 0（提示行 170 条） |
| 字节码 | `python -X utf8 -m py_compile …` | rc 0（工作区改动 53 个 .py 全部编译通过） |

这些数字是 **2026-10-30 首跑收口轮在最终代码字节上的实测读数**（run 1），不是常量；该轮同时改了 Python 侧代码（`project_runs._replace` 的 Windows 覆写重试、`run_spec._date_ms` 的日期宽度、取数失败面的单源用例）与文档，表里每一行都由当轮门禁日志重测回填，脚本里没有一个手抄数字。会读到本文档的四条腿（架构不变量、台账、`qx-cli` 单元锚点、失败面单源用例）在文本回填后再复跑一遍（run 2），读数逐键与 run 1 比对，不一致即拒绝收尾。Rust 单元锚点腿这一行是已裁决的实测红：那批红属于工作区在途的 Rust 重构，产物面清单已登记在 `docs/changelog/current.md` 的待裁决条目里，本轮不改 `crates/`。实施每个阶段后必须用同一组命令重跑并更新，不在文档里手写“应该还是多少”这种预期值。

补写数据侧方案后，在同一份文档改动上重跑过一轮：`python -X utf8 tools/check_architecture.py --fast` 退出码 0，报“4 片，完整覆盖 336 个具名检查且无重复”，墙钟 145.58 s（本轮实测，与上表不是同一轮）。其余四行没有再跑——本轮没有改代码，不把旧读数冒充成新读数。

2026-11-01 复测（夹具字节与台账重造之后，命令不变）：架构不变量 rc 0，4 片 / 336 个具名检查，墙钟 114.01 s；台账 rc 0，R01–R23、23 个验收场景、132 个产物绑定；Python 全量 589 tests verdict OK（skipped=5，墙钟 447.117 s）；失败面单源用例 1 项 rc 0；`cargo fmt --all -- --check` 与 `git diff --check` 分别 rc 0、0（后者提示 178 行）；字节码 55 个 .py rc 0。Rust 单元锚点腿由红 39 条变红 20 条（465 通过 / 20 失败、rc 101、墙钟 207.29 s）：A 快照守卫那 19 条由两个 bundle 补 `snapshots.bars` 清零，剩余 20 条逐名回接到归因账本，归因探针的未归因读数 unattributed=none。本段数字同样是实测读数、不是预期值；本轮仍未改 `crates/`。

2026-11-02 复测（反引号路径引用进门禁之后，命令不变，另加两条新腿）：架构不变量 rc `@{B:arch_rc}@`，`@{B:arch_shards}@` 片 / `@{B:arch_checks}@` 个具名检查，墙钟 `@{B:arch_seconds}@ s；台账 rc `@{B:ledger_rc}@`，`@{B:ledger_range}@`、`@{B:ledger_scenes}@` 个验收场景、`@{B:ledger_binds}@` 个产物绑定；Python 全量 rc `@{B:py_rc}@`、verdict `@{B:py_verdict}@`（skipped=`@{B:py_skipped}@`，墙钟 `@{B:py_seconds}@ s）——用例数从 `589` 涨到 `@{B:py_tests}@`，涨的是本轮新加的已跟踪用例文件 `python/tests/test_markdown_link_checker.py`（`7` 条，判的是这只出厂工具对反引号路径引用的判定边界），`py_compile` 覆盖数随之 `55` → `@{B:pyc_files}@`；`cargo fmt --all -- --check` 与 `git diff --check` 分别 rc `@{B:fmt_rc}@`、`@{B:diffcheck_rc}@`（后者提示 `@{B:diffcheck_lines}@` 行）；Rust 单元锚点腿 `@{B:rust_unit_passed}@` 通过 / `@{B:rust_unit_failed}@` 失败、rc `@{B:rust_unit_rc}@`。两条新腿进门禁：`python -X utf8 tools/check_markdown_links.py` rc `@{B:mdlinks_rc}@`（`@{B:mdlinks_files}@` 个已跟踪 Markdown、断链 `@{B:mdlinks_broken}@`、路径引用 `examined @{B:mdlinks_citations_examined}@ / case-mismatched @{B:mdlinks_citations_bad}@ / ambiguous @{B:mdlinks_citations_ambiguous}@`）与本地 CI 视角指针腿 rc `@{B:pointers_rc}@`（审计自身按设计退 `@{B:pointers_audit_exit}@`，腿的 rc 由基线判定而非透传；`@{B:pointers_local_only}@` 条本地专有链接、门禁类引用 `@{B:pointers_citations_gated}@` 行、HEAD 名册 `@{B:pointers_head_face}@` 条）。本轮把 `13` 行大小写错配（`-V1.md` 对已跟踪 `-v1.md`，Windows 点得开、Linux CI 404）改成 0，并把 `examined` 拆成可闭合的五桶：精确命中 `@{B:pointers_citations_tracked_exact}@`、大小写 `@{B:pointers_citations_case_mismatched}@`、歧义 `@{B:pointers_citations_ambiguous}@`、本地专有 `@{B:pointers_citations_local_only}@`、盘上与索引都没有 `@{B:pointers_citations_dead}@`，五桶相加 `@{BUCKET_SUM_B}@` 等于 `examined`。最后一桶此前被两条腿同时静默丢掉，实测而不设门禁（`@{B:pointers_dead_alias_unique}@` 行是模块别名或 README 相对自己工程根的写法、`@{B:pointers_dead_no_target}@` 行整仓没有这个尾巴，多数是书写约定不是缺陷）。可达性下限与两条棘轮都落在驱动里，读的是这一趟的字节：地板 `@{FLOOR_B}@` 等于这一趟的 examined、门禁类引用基线 `@{BASE_CITE_B}@`、本地专有链接基线 `@{BASE_LINK_B}@`，后两条只许降。本段数字同样是实测读数、不是预期值；本轮同样没有改 `crates/`。

### 6.2 每个阶段的共同门禁

1. **零新增依赖**：`python/pyproject.toml` 的 `dependencies` 保持空；`Cargo.toml` 不新增 crate；extras 不变，除非产品明确决策；
2. **不降证据**：所有快捷入口必须落到 `BacktestSpec` → `RunRecord` → `verify_run`，不得绕过产物复核；
3. **同源断言**：快捷路径与显式路径在同一输入下 `result_hash` 相同（沿用 `quickstart_first_run` 的相等模式）；
4. **用例计数口径**：新增 `#[test]` / `TestCase` 会重定价“用例总数”“通过数”等派生面。优先扩展现有 owning test；确需新文件时，同轮更新所有引用该数字的文档与台账；
5. **文档口径**：每项能力必须标【已实现】或【设计意图，未排期】；命令示例必须在干净环境可执行；
6. **失败面**：每个新入口至少一条负向用例（缺数据、缺 spec、缺渲染器、版本不匹配），断言 category 与退出码；
7. **数据默认离线**：CI 与单测不得联网；每个数据源都必须有假模块注入的离线契约用例（模式见 `python/tests/test_ashare.py:199-210`），联网只作为人工验收，不作为门禁；
8. **不静默换源**：任何取数入口在显式指定源失败后必须报错退出，不得回落到下一个源或内置样例；“用了哪个源”必须能从清单与 `plan --explain` 读出。

9. **引用与链接同权**：文档里反引号内的仓库路径引用，按 CI 视角（跟踪集合，不是本地磁盘）必须可解析；大小写错配即缺陷（在 Windows 工作机上点得开、在 Linux 检出里 404），`./` 前缀是导航不是拼写、判定前先归一；指向构建产物（`target/`、`dist/`、`.venv` 等被 exclude 规则挡住的路径）的散文引用只报告不设基线，因为任何提交都变不成签出后可读，而同形状的*链接*不允许——链接是给读者打开的；任何一条腿的「绿」必须自报它检视了多少条（`examined`），否则一个停止扫描的检查器和一个干净的仓库是同一张脸。

10. **还原要复跑，不能只核 sha**：突变电池把源码还原回 pre-image sha 只证明盘上的字节，不证明跑的是这份字节——字节码缓存按「秒级 mtime + 字节数」判失效，一处等长改动若在同一秒内还原，突变体的字节码就继续生效，而它长得和「一切正常」一模一样：引用面塌成 `0` 与一个干净的仓库是同一张脸。所以三条：跑被测模块的腿一律禁写字节码；导入被测模块的探针必须把加载到的判定对象与盘上源码里的字面量逐字比对之后才允许打印计数；电池在最后一枪还原之后必须再跑一遍原样并要求绿。

### 6.3 阶段验收判据（行为，不是文档）

| 阶段 | 必须新增/扩展的行为断言 |
|---|---|
| P0 | 三种安装矩阵各有一条命令链的退出码与产物断言 |
| P1 | `run -f` 与手写 spec 的 `result_hash` 相等；`init --demo` 后 `doctor` 零 issue、`run` 退 0；`plan --explain` 的来源标签随来源变化；`data doctor` 无 extra 时退 0 且逐源可读；`--source` 四级覆盖各一条且失败不换源 |
| P2 | wheel-only 三格式报告载荷与 CLI 一致；无渲染器时错误 category 正确 |
| P3 | 样例包逐文件摘要校验；缺文件拒绝安装；首跑复核通过；`data sources` 每行可对上真实能力或不支持；缓存命中断网可跑、`--offline` 未命中报错；四类取数失败 category 正确 |
| P4 | 每轮移动后行为用例零改动通过；文档命令逐条执行通过 |

---

## 7. 风险与不采纳项

### 7.1 许可证与数据边界

rqalpha 仓库的许可证对商业使用有单独要求，数据包也受其自身条款约束。本方案只借鉴交互形状（命令分层、默认值、单文件策略的阅读顺序），**不复制代码、不复制数据、不复制示例文本**。千行的 Apache-2.0 边界与数据血缘口径不变。

### 7.2 不采纳 globals 注入

把 `order_*` / `history_*` 注入策略模块 globals 能显著降低首行成本，但会：

- 让静态检查无法确定策略实际调用了哪些能力；
- 让沙箱必须信任“模块里出现的任何名字”；
- 让多语言契约退化为“Python 特有”；
- 与版本化 manifest、导入审计、可复核产物冲突。

替代方案：保持显式 `BaseStrategy`，用 `run -f` 与模板降低样板，用 `context` 上的少量 helper 降低 API 记忆量。

### 7.3 不采纳 mod 系统

`qx-plugin` 的两格扩展点是“内核真的会分派”的最小面；再引入 mod 生命周期与两阶段 hook，会形成第二套装配平面，且与现有 worker/sandbox 装配冲突。rqalpha 的 mod 系统解决的是它自己的内核扩展问题，不是千行当前的主要矛盾。

### 7.4 不采纳 pickle 结果

千行的结果已经是“可复核产物 + 版本化报告”，pickle 会引入跨版本不可读、不可验、跨语言不可用的问题。若需要“单一结果对象”，应提供只读的 Python DTO（现有 `run_backtest` 返回形状）而不是序列化内核对象。

### 7.5 方案与既有台账冲突时的处理

本文件不修改 R01–R23 的完成状态；若某条建议与台账中的“已回退未排期”条目冲突，登记为待裁决，不静默改口径。实施时以“行为用例 + 门禁读数”为准，不以本文的优先级为准。

### 7.6 数据源条款、可用性与凭据

- 各源条款独立：akshare / baostock / easy-tdx / mootdx / CCXT 的上游条款与再分发限制各不相同，千行**不随包分发任何真实行情**，只分发合成样例包，并在清单里显式标注 `is_sample`；
- mootdx 走通达信协议：无官方 SLA、服务器地址会变，且公司行为/复权口径弱于 akshare/baostock。因此单列 `a-share-mootdx` extra，**默认不进 `a-share` 元 extra**——加进去等于替用户承接一个不稳定的默认源；是否加，属产品决策，不是实现细节；
- 公司行为、复权与交易日历的差异必须体现在能力矩阵里，不能让用户在“结果不对”时才发现换了源就等于换了口径；
- 凭据类源（token）只从环境变量或项目本地配置读取，不进 manifest 明文字段、不写日志；
- 每个源的 `provider_version` 与请求参数进清单，网络与代理不是“数据版本”。

### 7.7 不采纳“内置全市场数据包”

rqalpha 的 bundle 是“全市场 + 月度更新 + 固定 CDN”。千行不采纳：

- 体积会压垮 wheel（当前零强制依赖、`abi3` 单轮构建、单平台一份）；
- 许可与更新责任无法在本仓声明，数据转分发风险由使用者承担；
- 与 `DatasetSnapshot` 的“逐文件血缘 + 可复核”口径冲突：一个大包只会把“这份回测用了哪批字节”重新糊掉。

替代方案就是本文的数据侧结论：**合成样例包保证装完能跑（小、离线、带摘要）+ 按需多源取数（extra、显式选源、内容寻址缓存、血缘清单）**。

---

## 8. 证据索引

### 8.1 千行（本地）

- 命令面与首跑：`crates/qx-cli/src/cli_args.rs:30-165`、`crates/qx-cli/src/quickstart.rs:90-200`
- 项目初始化与样例行情：`crates/qx-cli/src/init_project.rs:390-495`
- Python CLI 命令表（评审时点无 `data` 子命令，2026-10-30 首跑收口轮已按 D5 加上）：`python/qianxing/cli.py:267-520`、`:606-699`
- 项目脚手架与 spec 要求：`python/qianxing/strategy_project.py:122-177`、`:247-280`、`:668`
- 项目运行与产物复核：`python/qianxing/project_runs.py:934-1015`
- 报告生成与外部 CLI 依赖：`python/qianxing/project_runs.py:2262-2330`
- 策略基类与钩子：`python/qianxing_strategy/base.py:414-570`、`:726-830`
- 内置示例策略：`python/qianxing_strategy/long_only_sma.py:38-64`
- 安装与依赖：`python/pyproject.toml:32-81`、`:88`、`docs/installation.md:17-125`
- A 股数据适配与血缘清单：`python/qianxing_ashare/__init__.py:419`、`:450`、`:530-615`、`:647`、`:795`、`:1137`、`:1209`、`:1306`、`:1438-1455`
- A 股取数命令：`python/qianxing_ashare/cli.py:670`
- SDK 数据再导出：`python/qianxing/ashare.py:3-4`
- 数据 extras：`python/pyproject.toml:51-68`
- 离线契约测试范式：`python/tests/test_ashare.py:199-210`
- CCXT 适配器：`python/qianxing_ccxt/provider.py:25`
- 项目输入路径键：`python/qianxing/strategy_project.py:91-94`
- 首跑与配置口径：`docs/工业化易用性收口指南-V1.md:5-108`
- 扩展点：`crates/qx-plugin/src/lib.rs:1-23`
- 沙箱后端选择：`crates/qx-cli/src/strategy_host/providers.rs:25-180`
- R01–R23 台账与已落地项：`docs/architecture-local-server-refactor-review-2026-10-28.md`

### 8.2 RQAlpha（外部，2026-10 读取）

- 安装：`docs/source/intro/install.rst:24-43`
- 数据包：`rqalpha/cmds/bundle.py:69-84`、`:107-158`
- 示例策略：`rqalpha/examples/buy_and_hold.py`
- 策略作用域注入：`rqalpha/main.py:118-120`、`:191-194`
- 配置优先级：`rqalpha/utils/config.py:139-224`
- mod 装配：`rqalpha/mod/__init__.py:31-106`
- 两阶段 hook：`rqalpha/core/events.py`、`rqalpha/core/hooks.py:98-103`
- 执行阶段：`rqalpha/core/executor.py:37-99`
- 分析与报告：`rqalpha_mod_sys_analyser/mod.py:600-781`

> 外部文件的行号只用于定位当时的实现；rqalpha 迭代后行号会漂移。引用时请以“文件 + 函数名”复核，不要把行号当成稳定契约。