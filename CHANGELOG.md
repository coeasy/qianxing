# Changelog


## Unreleased — V13 文档轮：V11/V12 归档进 `docs/archive/`、README 改成产品说明、安装面每条数字重跑后再写（2026-09-26）

本轮**不动一行代码、不加一条判据**，只做三件事：把已被取代的两代审计移进 `docs/archive/` 并修好每一条
入站引用；把 README 从"逐轮编年 + 数字堆叠"改成能读的产品说明；把安装与使用文档里每条"实测过"的句子
当场重跑一遍，跑不起来的措辞改掉。过程抓到**三处文档与事实不符**（`### Fixed`），并新登记一条量具盲区
（文档引用 CLI 输出字面量这件事无人核对，#157）。

### Changed（归档动作本身：移动 + 横幅 + 路径同步，正文结论一字未改）

| 动作 | 实测取证 |
| --- | --- |
| `docs/自研量化框架重构方案-V11.md` → `docs/archive/` 同名；`docs/自研量化框架审计与重构方案-V12.md` 同步 | 移动后 3,734 行 / 384,944 字节、1,904 行 / 195,766 字节，两份都是 LF（`CRLF=0`）；标题下各加一节「归档状态」，写明被谁取代、数字是当轮快照、归档只改了哪几处 |
| 新建 `docs/archive/README.md` 索引 | 在册两份的行数/字节/覆盖轮次/归档日期/被谁取代/今天仍有用的部分，加三条读法纪律与"为什么留而不删"（V1–V10 那批是直接删除的，这里改用移动，理由写在索引最后一节） |
| 入站引用逐条同步 | `CHANGELOG.md`：V11 的路径 token 56 处（28 行）、V12 的 14 处（7 行）全部改指 `docs/archive/`，替换后旧路径残留 **0** 处；`docs/archive/…V12.md` 内部对 V11 的 3 处路径提及同步；`README.md` 的「文档地图」与「实现状态」两处链接改指 V13 + archive。两份归档文档里**没有**任何 markdown 链接指向别的文档（`](` 计数 0），所以不需要相对路径改写 |
| 门禁复跑确认归档没打断任何按路径取数的判据 | 能力矩阵证据路径判据（`docs/` 前缀在核对字符类里）仍绿 —— `maturity/capabilities.yaml` 唯一一条 `docs/` 证据行是 `工业化易用性收口指南-V1.md:193`，那份文档不移动 |

### Fixed（三处文档与事实相反，全部实测抓到）

1. **`docs/工业化易用性收口指南-V1.md` §1 称 `qx-cli help` 的末行指向本文** —— 实跑末行是
   「使用 `qx-cli help` 查看入口摘要；…完整说明见 README.md 与 deploy/README.md」，指向的是那两份，
   本文只在 README 的「文档地图」里（`logs/s51_docs_round_guide_claims.txt`）。同一轮顺带复核实测
   `qx-cli --version` 确实以退出码 2 fail closed。文档改为按事实写。
2. **README「安装」开头把日志位置写成 `%TEMP%/qx_v12p2/logs/`，而正文各处引用的都是 `logs/…`** ——
   `/logs/` 在 `.gitignore` 第 23 行，两者不是同一个地方。统一成 `logs/` 并当场写明：`logs/` 是本机目录、
   **不随仓库分发**，判据本身在 `tools/check_architecture.py` 与 `crates/*/tests` 里可重跑。
3. **README 称"本轮实测把全部 9 步端到端跑通了一次"** —— 那是 V12 §22 那一轮的事实（460 条 `PASS` /
   92 段 843 passed），本轮没有重跑 `build.bat`。改成如实标注，并把本轮真正重跑的两步单列：
   `[1/9]` 的架构门禁现在 509 条全绿、`[4/9]` 的整树测试现在 92 段 / 858 passed / 0 failed。

### Added（文档结构，不是代码）

- README 新增「产品说明」一节：一句话定位 + 三条主链路表（入口 / 产出 / 凭什么算可复核）+
  「它不是什么」四条边界（真账号、热插拔、统一撮合机、跨节点 HA 与柜台协议），把原来散在文末的
  三条"容易读过头的边界"提到最前面；「为什么用它」补两条今天才立得住的：钱字段的 `null`/`0` 分别、
  52 份示例模板逐份真读。
- README「文档地图」从"按文档列"改成"按你要做的事列"，并加一行"想知道某一轮**当时**修了什么" → archive。
- `deploy/README.md` 标题下加安装前置：本文只讲运维面、命令里的 `cargo run --release -p qx-cli --` 是
  源码树写法、装过的人换成 `qx-cli `，并指向收口指南。

### Validation（本轮日志 `logs/s46_*.txt`—`logs/s64_*.txt`；口径记录同步写进 V13 §9.9）

| 项 | 命令 | 实测 | 日志 |
| --- | --- | --- | --- |
| 整树测试（带解释器） | `QX_PYTHON=<venv 解释器> cargo test --workspace` | 92 个 `test result:` 段、**858 passed / 0 failed**、`TEST_EXIT=0` | `s46_docs_round_cargo_test_workspace.txt` |
| 安装路径 A | `cargo install --path crates/qx-cli --locked` | `Finished in 2m 14s`、`INSTALL_EXIT=0` | `s47_…_cargo_install.txt` |
| 仓库外六条入口 | 临时目录里 `help`/`init --strategy macd`/`doctor`/`backtest`/`report`/`status` | 六条退出码全 0；`init` 落 14 份自包含文件；回测写四份同前缀产物、`result_hash=26fdd6b52d020700`（与 V12 §22 那一轮同一条命令逐字符相同） | `s48_…_installed_cli_smoke.txt`、`s49_…_install_measure.txt` |
| 命令面计数 | 已安装的 `~/.cargo/bin/qx-cli help`（在仓库外目录跑） | **52 行用法、41 个去重入口名**、help 输出 155 行、`HELP_EXIT=0`；入口名逐名列印进日志，README 那句"41 个入口"因此不再只由 `s49` 里一条正则失败的计数撑着 | `s54_docs_round_help_measure.txt` |
| 架构门禁（归档与 README 改写之后） | `python tools/check_architecture.py` | **509 项全绿**、`GATE_EXIT=0`，其中"能力矩阵证据路径全部存在"与三条 README 字面量判据都在绿侧。归档之后跑一次（`s50`），四份文档全部改完之后再跑（`s55`），V13 记完 §9.9 之后重跑到收尾（`s57`—`s62`），**六次全是 509/0**；README 定稿后又对最终字节的 README 跑了一次，仍是 509/0（`s63_docs_round_gate_after_readme_final.txt`） | `s50`、`s55`、`s57`—`s63` |
| 文档口径复核 | `qx-cli --version` 与 help 末行 | 退出码 2；末行指向 README 与 deploy/README（见 `### Fixed` 第 1 条） | `s51_docs_round_guide_claims.txt` |
| 安装包按当轮代码重建 | `powershell -NoProfile -ExecutionPolicy Bypass -File tools/build_python_wheel.ps1 -Python python\.venv\Scripts\python.exe` | `WHEEL_EXIT=0`，wheel 217025 字节 / 17 条目 / sha256 前缀 `2603b5c2be46926a`（上一份是 216439 字节、05:42 那轮产物，比当轮原生扩展旧 6 小时） | `s52_docs_round_wheel_build.txt` |
| 干净 venv 复核 wheel | `uv venv` + `uv pip install --offline --no-deps` + `tzdata` | 内嵌 `_qianxing_native.pyd` 与 `target/release/_qianxing_native.dll` md5 逐字节相同（`a9764db6131fb9cf…`）；`qianxing_ashare`/`_bridge`/`_ccxt`/`_strategy` 四包全部导入；`native.available() → True`；衍生品三字段归一成 `cross/hedge/3` 且读回一致；`margin_mode="weird"`/`position_mode="both"`/`leverage=0`/`position_side="Weird"` 各抛 `ValueError`；`VERIFY_EXIT=0` | `s53_docs_round_wheel_verify.txt` |
| 行尾与字节 | 逐文件按字节比对（`raw.count(b"\r\n")` vs `raw.count(b"\n")`） | `README.md`（521 行）/ `deploy/README.md`（815）/ `CHANGELOG.md`（4,393）/ 收口指南（412）改后**孤立 LF 为 0**（CRLF 数 = LF 数）；V13（783 行）、`docs/archive/README.md`（33）、两份归档文档（3,734 / 1,904）保持纯 LF（`CRLF=0`） | `s64_docs_round_line_endings_final.txt`（快照之后只有本条与 V13 §9.9 的引用行各改一次，README / deploy/README / 指南 / 归档字节未再动） |

**本轮没做**：`build.bat` 九步端到端（#82 未修 —— 它会把未跟踪产物写进仓库 `deploy/data/**`，本轮
为避免污染工作树而没有整体重跑）；任何代码、判据、夹具改动；`maturity/capabilities.yaml` 的
`sandbox_tested` 照旧全为 `false`。

### 量具盲区（登记，不在本轮修）

- **#157 文档引用 CLI 输出字面量无人核对**：`### Fixed` 第 1 条那处腐坏是靠人肉跑 `qx-cli help` 抓到的。
  门禁钉住的 README 字面量只有三条（PowerShell 入口带 Bypass、「不用系统时间」那句指向数据侧闸门、
  `deploy/README.md` 端点表 ≡ qx-api 路由集合），其余"某命令会印出这句话"的表述都是自由文本。
  候选判据与口径已登记为 V13 §4 L4 第 8 条，待立项。


## Unreleased — V13 R1-A6：52 份 deploy 模板逐份过一遍生产读法，三份示例被证明"照抄即坏"（2026-09-26）

§1.4 的那笔账是本轮的起点：`deploy/` 顶层 **52 份** JSON 模板里，**13 份没有任何代码或 CI 引用**，
其中 2 份（`qianxing.runtime.sqlite.example.json`、`qianxing.scheduler.jobs.smoke.json`）连文档都不提。
V13 §5 原本给的动作是"删掉那 2 份"，**实测不成立**：两份都能被它们那一类的生产读法读通
（`logs/s32_template_coverage_green_a6.txt` 第 8 行 `workers=3 storage=Sqlite`、第 52 行 `jobs=1`），
删它们等于把"没人读"这笔账用删除来销账。本轮改成给全部 52 份建立**按执行计的覆盖**：
每份模板在登记表里点名一类读取器，用例真的调用那个读点，并把它读出来的身份（worker 数、BarFrame
指纹、Bundle 摘要、作业条数……）印进日志。取证与口径见 `docs/自研量化框架审计与重构方案-V13.md` §9.8。

第一轮探针（`logs/s29_template_coverage_probe_a6.txt`）就报出 **9 处读取失败 + 1 处坏内容探针失败**，
逐条追下去是三类不同的问题，其中两类落在**已发布的示例文件本身**：

| 失败 | 根因 | 本轮处置 |
| --- | --- | --- |
| `qianxing.binance.spot.spec.json`、`qianxing.ccxt.okx.perpetual.spec.json`：`是 CCXT 形状却没有标的配对来源` | 判形状的那一步只看"有没有 `base_currency`"，产品规格形状自己带着 `instrument` 那一格，无需配对帧 | 登记表允许 `MarketSpec(None)`，标的由 `TradingInstrumentSpec` 的反序列化从文件自己那一格读出 |
| `qianxing.bar-frame.example.json`、`.okx`、`.ashare`：`invalid JSON BarFrame provider range` | 覆盖用例照抄 Provider 的入参形状，把 `start` 传成 `0`，而 `JsonBarFrameProvider::bars()` 第一步就拒 `start == 0` | 换成生产链真正用的那一对照点：`read_bar_frame_for_backtest` + `barframe_dataset_identity`（首末根时间戳即区间） |
| `qianxing.bar-frame.pairs-primary/reference`：`strict mode: unknown field \`frequency\`` | 见下面的 `### Removed` | 两份夹具各删两格 |
| `qianxing.submit-order.ccxt-derivatives`：`unknown variant \`Net\`, expected one of \`net\`, \`long\`, \`short\` at line 1 column 235` | 见下面的 `### Fixed` | 三格改成 snake_case |
| `qianxing.runtime.production`：`config validate 失败: strategy.research_snapshot_path 文件不存在 /var/lib/qianxing/…` | 不是缺陷：这份模板按设计带占位路径，`live-check` 文档明写"模板中的占位路径会按预期失败" | 不删预期，改成 `Expected::Refuses` 点名 3 项关键字，用例把拒绝理由原样印进日志（`logs/s32` 第 7 行） |
| 探针：`qianxing.costs.example.json 的读取器接受了坏内容` | `ExecutionCostRules` 是 `#[serde(default)]`，`{"coverage":"broken"}` 被静默补全成合法费率 | 坏内容探针改为**按读取器类别**给载荷，费率那一类给越界的 `taker_bp: 100001` |

### Removed（两份手写夹具里的两格，仓内无读者）

`deploy/qianxing.bar-frame.pairs-primary.example.json` 与 `...pairs-reference.example.json` 各删掉
`"frequency": "1h"` 与 `"quality": "recorded"`。这两格**只存在于这两份手写示例**（本轮 `grep` 全仓
`.rs`/`.py`：`frequency` 在 Rust 侧只有 `crates/qx-provider/src/lib.rs:28` 的 `DataQuery.frequency`
—— 那是**取数请求**里的一格，由调用方构造，BarFrame 文档里没有它；`quality` 作为字段名在 Rust 与
Python 两侧**零出现**（同文件 `:32` 那格叫 `quality_policy`，也是请求侧的）。这两格看起来就是把
`DataQuery` 的字段名抄进了行情文档。写侧同样不产它们：
`python/qianxing_bridge/__init__.py:37` 的 `BAR_FRAME_STRICT_FIELDS` 是这份文档的字段全集。
保留的后果是这两份示例**在 `schema_version >= 1` 的严格读点下必坏** ——
`crates/qx-data/src/provider.rs:231` 的 `parse_bar_frame` 先按旧格式宽解，`schema_version >= 1`
时改用 `#[serde(deny_unknown_fields)]` 的 `StrictBarFrameJson` 重解（`provider.rs:121`、`:254`）。

### Fixed（一份已发布示例照抄即失败）

`deploy/qianxing.submit-order.ccxt-derivatives.example.json` 的 `policy` 三格：`position_side`
由 `"Net"` → `"net"`、`margin_mode` 由 `"Cross"` → `"cross"`、`position_mode` 由 `"OneWay"` →
`"one_way"`。`crates/qx-core/src/trading.rs:29-46` 的三个枚举都带 `#[serde(rename_all = "snake_case")]`，
所以 PascalCase 字面量没有任何读者；这份示例被 `docs/外部链路验收执行方案-V1.md:69` 点名"只是载荷样例，
没有执行它的入口"，正因为无人执行，它带着读不懂的枚举躺在仓库里。现在它由覆盖用例的 SubmitOrder 读点执行。

### Added（1 个用例文件 + 9 颗判据）

- `crates/qx-cli/src/tests/deploy_template_coverage.rs`（779 行，登记进行数棘轮）三条用例：
  `every_deploy_template_is_registered_with_a_reader`（登记表与磁盘清单逐名相等，且没有重复登记）、
  `every_deploy_template_loads_through_its_registered_reader`（52 份逐份真读，15 类读取器各自调生产读点）、
  `every_reader_class_rejects_a_broken_template`（每一类各喂一份坏内容必须被拒，且探针覆盖数与变体数相等）。
- `tools/check_architecture.py` 的 `deploy_template_coverage_check()` 9 颗：登记表三列逐条对齐 /
  登记表与磁盘逐名相等 / `Reader` 变体清点 / 变体⊆已被使用 / 变体⊆坏内容探针 / 每类读取都落在生产读点 /
  三条用例与模块挂载在位 / 预期结果两档且 `Refuses` 必须点名 / 登记表点名的配对来源仍在册。
- 地板按本轮磁盘实测取值：`GATE_CHECK_FLOOR` 500 → **509**（+9）、`WORKSPACE_TEST_FLOOR` 863 → **866**（+3）、
  `CLI_TEST_FLOOR` 262 → **265**（src/tests 213 + `crates/qx-cli/tests` 递归 52）；
  `EXECUTION_TEST_FLOOR` 实测仍是 25（15 + 10），未动。

### Validation（日志在 `logs/s29_*.txt`—`logs/s43_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 首轮探针（建表即跑） | `9 处读取失败` + `坏内容探针失败 1 处`，逐条见上表 | `s29_template_coverage_probe_a6.txt` |
| 覆盖用例收口后 | `test result: ok. 3 passed; 0 failed`，52 行 `=>` 身份打印（51 `Ok` + 1 按点名关键字被拒） | `s31_…_rerun_a6.txt`、`s32_template_coverage_green_a6.txt` |
| 行为变异 7 发（真树 + `%TEMP%` 逐字节镜像还原） | MA（漏登记一份）红 2 条、MB（读取器配错类别）红 2 条、MC（CostRules 走过场）红 1 条、MD（坏探针退回通用形状）红 1 条、ME（`Refuses` 关键字放宽）红 1 条、MF（示例退回 PascalCase）红 1 条、MG（夹具退回 `frequency`）红 1 条；末行「还原核对: 全部逐字节相同」 | `s33_a6_mutation_summary.txt`、`s33_mutation_0{1..7}_*_a6.txt` |
| 门禁判据变异 6 发（只改文本，跑门禁） | GA（登记表少一份）红 2 条、GB（探针少一个变体）红 1 条、GC（生产读点换成测试内自造校验）红 1 条、GD（用例改名）红 1 条、GE（配对指向不存在的文件，只改 2 处中的 1 处）红 1 条、GF（在生产侧另起一处规格标签落点）红 1 条；末行同样「全部逐字节相同」 | `s37_a6_gate_mutation_summary.txt`、`s37_gate_mutation_G{A..F}_a6.txt` |
| 门禁整跑 | 抬地板前 `509 项` 全绿；六发变异还原后再跑仍 `509`；地板抬到 509 之后第三次整跑 `[PASS]` 计数 **509** / `[FAIL]` 0 / 退出码 0 | `s35_a6_gate_second_run.txt`、`s36_a6_gate_after_mutation.txt`、`s40_a6_gate_floors_raised.txt` |
| 整树测试（`cargo test --workspace`，`QX_PYTHON` 指向 venv） | 92 个 `test result:` 段全 `ok`、**858 passed / 0 failed / 0 ignored** | `s38_cargo_test_workspace_a6.txt` |
| 在册 / 实跑双口径 | 磁盘 `#[test]` **866** 条 vs 实跑 **858** 条，差 8 条与 A4 轮同族（feature 门后），已由 #144 单独点名 | `s39_a6_floor_measure.txt` 对 `s38` |
| fmt / clippy | `cargo fmt --all -- --check` 退出 0；`cargo clippy --workspace --all-targets -- -D warnings` 退出 0（新用例一次通过，无 `redundant_closure` 复发） | `s43_a6_cargo_fmt_check.txt`、`s41_a6_clippy.txt` |
| 被跟踪产物零改写 | `git status --porcelain deploy` 4 条：3 份本轮修的模板 + `deploy/README.md`；`deploy/data` 下 **0 条** → blessed 产物未被重 bless | 本轮命令输出 |
| 能力矩阵计数 | 改动前 21 块 / 283 证据行（251 以仓库内路径开头）/ 75 limitation → 改动后 21 / **287**（**255**）/ **76**；`sandbox_tested` 与 `production_approved` 为真的仍然 **0** 条 | `s42_a6_caps_before.txt`、`s42_a6_caps_after.txt` |

### Documented

- `deploy/README.md` 新增"模板契约面：每份模板都有人真读"一节：52 份的读法登记表在哪、15 类读取器是哪些、
  为什么 production 模板按预期被拒；并把两条会咬人的口径写进接口文档 —— **`schema_version >= 1` 的
  BarFrame 文档只能带严格字段集**（宽/严两个读点同源，多一格就在 Provider 侧炸），
  以及 SubmitOrder 载荷里 `policy` 的三格是 snake_case 枚举名。
- `maturity/capabilities.yaml`：`cli_scenario_init` +3 条证据行与 1 条 limitation
  （`production_runtime_template_refuses_validation_by_design_…`），`ccxt_rest` +1 条（那份衍生品示例的
  三格枚举字面量）。
- README「当前状态」段的数字整体换成上面这轮实测值（500 → 509、863 → 866、855 → 858、283/251 → 287/255）。

### 本轮抓到的一条量具盲区（进 V13 §9.8 与 §4 L4）

**同一份 BarFrame 有两个生产读点，字段集不一样**：回测直读链先走宽口径的 `read_bar_frame_for_backtest`
（`BarFrame::from_json`，未知字段忽略），紧接着 `barframe_dataset_identity` 走 Provider 的严格口径
（`deny_unknown_fields`）—— 于是 `frequency` 这一格能在仓库里活很久：**只跑前者的用例全绿，跑后者的链路必红**，
而 §18-B 那批 BarFrame 用例恰好全是前者。同类形状还有 `JsonBarFrameProvider::bars()` 拒 `start == 0`
（`crates/qx-data/src/provider.rs`），这是"合法区间"的最低门禁，任何按"整份文件"取数的新读点都得先满足它。
本轮没有把两个读点合成一个（那会牵动 Provider 的入参形状，属 §6 之外的动作），而是把口径钉成判据：
登记表里 BarFrame 那一类必须点名**生产的那一对**读点，第 6 颗（"每类读取都落在生产读点上"）盯的就是这个。


## Unreleased — V13 R1-A4：记账币种的缺省值收成一个常量，并证明它会动发布产物（2026-09-26）

"没人声明时按 USDT 记账"这句话，本轮实测在 Rust 生产代码里写着**四份**
（`crates/qx-cli/src/backtests/mod.rs:31`、`venue_runtime/worker_runtime.rs:23`、同文件 `:76`、
`ecosystem_smoke.rs:346`；取证见 `logs/s28_currency_sites_a4.txt`，口径与门禁一致：只认引号里整串
等于 usdt 的出现，`BTCUSDT` 这类标的名不算）。四份抄本的危害与 A3 那族一样：改一处就把另外三处留在
原地，产物里的账簿币种与代码里的判词分叉而无人报错。同一轮还要回答另一半问题 —— **记账币种真的进了
发布产物吗**？它决定现金腿落在哪本账，如果两条只差币种的回答同一个 `result_hash`，产物就无法证明
"这份收益是哪种币的收益"。取证与口径见 `docs/自研量化框架审计与重构方案-V13.md` §9.7。

### Changed（4 处字面量 → 1 处常量；对外的取值一格未变）

- 新增 `crates/qx-core/src/identity.rs` 的 `pub const DEFAULT_SETTLEMENT_CURRENCY: &str = "USDT"`，
  注释写明它只是**缺省值**、不是合法币种白名单。四条回落链逐个接回：回测装配
  （`.unwrap_or_else(|| DEFAULT_SETTLEMENT_CURRENCY.into())`）、worker 自身声明
  （`.unwrap_or(DEFAULT_SETTLEMENT_CURRENCY)` 后仍 `to_ascii_uppercase()`）、账户日志的写入方全体缺席
  （`.unwrap_or_else(|| DEFAULT_SETTLEMENT_CURRENCY.to_string())`）、生态烟测的现金簿键。
- 改造后 Rust 生产代码里的整串 `"USDT"` 只剩定义点那一格加它自己的文档注释
  （`logs/s28_currency_sites_after_a4.txt`：`.rs` 4 → 2，两处都在 `identity.rs`）。全仓整串计数
  45 → 49，多出的 6 处全部是本轮门禁脚本里判据自己的字面量（`SETTLEMENT_CURRENCY_LITERAL` 与其注释）。
- **无破坏性变更**：`settlement_currency` 缺席时四处的答案仍是 `USDT`，声明了任何币种时读声明值，
  因此不触发 `### Removed`；配置字段本身（`Option<String>`）与 `skip_serializing_if` 形状未动。

### Added（3 条用例 + 11 颗判据）

- `crates/qx-cli/src/tests/settlement_currency_single_source.rs` 三条：三条回落链同值（含把 paper
  模板里所有 worker 的声明抹掉后走真实账户日志判定）、声明缺省币种与不声明等价、
  **只换 `settlement_currency` 必须换掉发布产物的 `result_hash`**（走真实回测入口跑两遍再比对）。
- `tools/check_architecture.py` 的 `settlement_currency_check()` 11 颗：定义点唯一且值仍是 `USDT` /
  全仓只有一颗 `*SETTLEMENT_CURRENCY*` 公共常量 / 生产代码除定义点外没有第二处写死的 `"USDT"` /
  定义处带着"缺省值≠白名单"的口径说明 / 消费者登记表与实盘逐一对应 / 四条回落链各自仍读常量 /
  三条用例在册 / 指纹用例钉的是 `result_hash` 而不是随墙钟动的 digest。
- 地板按本轮实测取值：`GATE_CHECK_FLOOR` 489 → **500**（+11）、`WORKSPACE_TEST_FLOOR` 860 → **863**（+3）、
  `CLI_TEST_FLOOR` 244 → **262**（本轮 +3，其余 15 条是历轮未回填的欠账，按当轮实测一次补齐）。

### Validation（日志在 `logs/s28_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 门禁整跑 | `exit=0`，500 条 `PASS` / 0 条 `FAIL`（本轮 +11 颗） | `s28_gate_final_a4.txt` |
| 文档回合之后复跑门禁（本轮改了 README / deploy/README / capabilities / V13 / CHANGELOG） | 两份日志各自 `gate_exit=0`、500 条 `PASS` / 0 条 `FAIL`，末行打印「架构不变量自检全部通过 ✓（500 项）」；新增的 4 条证据行逐条被"证据路径存在性"判据认下（第二份是 §9.7 定稿之后再跑一次） | `s28_gate_docs_a4.txt`、`s28_gate_docs_final_a4.txt`、`s28_capability_counts_a4.txt` |
| 静态判据变异 5 次运行（真树 + `%TEMP%` 逐字节镜像还原） | MG1（回测兜底退回字面量）红 3 条、MG3（缺省值改 USDC）红 1 条、MG4（未登记的第五处消费者）红 1 条；MG2 首发只红 1 条，收紧签名匹配后复跑红 2 条；每次还原后回到 500 全绿 | `s28_gate_mut_a4.txt` |
| 行为变异（把 `crates/qx-xingban/src/backtest.rs` 的初始入金币种写死为 `"USDT"`） | 币种指纹用例 `changing_only_the_settlement_currency_moves_the_published_fingerprint` 变红（`2 passed; 1 failed`），另两条仍绿；按镜像逐字节还原后 3 条全绿（`3 passed; 0 failed`） | `s28_cargo_mut_a4.txt`、`s28_cargo_green_a4.txt` |
| 整树测试（`cargo test --workspace --all-targets --no-fail-fast`，`QX_PYTHON` 指向 venv） | 72 个测试二进制段、**855 passed / 0 failed**、退出码 0（较 A3 轮的 852 恰 +3 = 本轮 3 条新用例） | `s28_cargo_test_workspace_a4.txt` |
| Doc-tests（`cargo test --workspace --doc`） | 21 段全 ok、0 passed（仓库无 doctest）、0 failed；与上一行合起来 93 段 | `s28_cargo_test_workspace_doc_a4.txt` |
| `cargo test -p qx-storage --features sqlite`（feature 门后的 8 条不在整树里） | 10 段全 ok / 56 passed / 0 failed / 退出码 0 | `s28_cargo_test_storage_sqlite_a4.txt` |
| `cargo fmt --all -- --check` | 退出 0 | `s28_cargo_fmt_check_a4.txt` |
| `cargo clippy --workspace --all-targets -- -D warnings` | 首跑退出码 101，唯一一条是本轮新用例里的 `redundant_closure`（`settlement_currency_single_source.rs:49` 的 `.any(\|worker\| owns_account_event_log(worker))`）；改成 `.any(owns_account_event_log)` 后复跑 2 行输出、0 warning、`clippy_exit=0`（首跑那份日志被复跑覆盖） | `s28_cargo_clippy_a4.txt` |
| 被跟踪产物零改写（跑完整套测试之后 `git status --porcelain deploy`） | 1 条：` M deploy/README.md`（本轮接口文档自己改的），`deploy/data` 下 0 条 → blessed 产物未被重 bless | 本轮命令输出 |
| 能力矩阵计数（沿用 A3 那份脚本，口径是"缩进两格的条目"） | 改动前 21 条 / 279 证据行（其中以仓库内路径开头 247）/ 75 limitation；改动后 21 条 / **283**（**251** 以路径开头）/ 75。其中带 `implementation` 键的能力块是 19 个（另两条 `single_node`/`distributed` 是部署形态），16 个同时满足 `implementation` 与 `code_tested`，`sandbox_tested`/`production_approved` 无一为真 | `s28_capability_counts_before_a4.txt`、`s28_capability_counts_a4.txt` |

### Documented（口径进接口文档）

- `deploy/README.md` 在"账户事实两条口径检查"那段之后新增 `settlement_currency` 缺省值口径：缺省值只有
  一处定义、"没写这一格"与"写了 USDT"同解、换缺省币种改哪一行，以及"只换币种必须换掉 `result_hash`"
  这条产物级承诺及其用例现场。
- `maturity/capabilities.yaml`：`local_backtest` +2 条证据（装配回落读常量、指纹用例走真实入口）、
  `paper_execution` +2 条（worker 与账户日志两条链同缺省、paper 模板的三条用例）。
- `README.md` 的"当前状态"段按本轮日志重抄；`docs/自研量化框架审计与重构方案-V13.md` 新增 §9.7，
  并把 §4 L2-6 与 §5 A4 两行的状态改成已落地。

### 本轮抓到的一条量具盲区（登记为待办 #152 的补充）

- MG2 那发变异（给币种指纹用例改名）第一次**只红一条**判据：`_fn_body(text, sig)` 是按前缀匹配的，
  改名后的函数体仍能被旧名定位到，于是"在册"判据与"断言现场"判据重合成了同一条。把 needle 收成带
  左括号的完整签名后复跑，同一发变异红 2 条。这与 §9.6 记的"登记表类判据会被注释骗绿"是同一类失效：
  **定位用的字符串要收到能被改名的那一格，不能停在名字前缀**。


## Unreleased — V13 R1-A3：venue 识别收成一个函数，19 处 worker 级判定式全部作废（2026-09-26）

运行拓扑里有六处要问"这个 worker 属于哪个 Venue 家族"，而 `crates/qx-core` 早就有
`WorkerConfig.venue_id: Option<String>`。本轮实测：**同一个问题在生产代码里有 19 处写法**
—— 9 处问"是不是 Binance"（`qx-orchestrator` **五份逐字相同**的
`.map(|venue| venue.to_ascii_lowercase().contains("binance")) == Some(true)`、`qx-cli` 三份
`is_some_and` 变体、`qx-runtime` 一份私有 `fn is_binance(&Option<String>)`），10 处问"是不是
Paper"（`is_some_and` 与两种 `map(...).unwrap_or(...)` 抄法，None 的走向一份是"报错"一份是"放行"），
另有标的级整名比较 `instrument.venue.as_str().eq_ignore_ascii_case("BINANCE")` **六处**。危害方式不是
"当下算错"，而是下一次改口径只改其中一份：把子串收成整名，`binance-testnet` 会静默脱离 Binance 家族，
编排照样返回 `Ok` 而那条 worker 没人拉起；把 Paper 的整名放宽成前缀，`paper-proxy` 会被当成本地虚拟
撮合域。本轮把三条口径收进一个定义点，并给"只有一处定义"装上点名式判据（取证与口径见下表与
`docs/自研量化框架审计与重构方案-V13.md` §9.6）。

### Changed（19 + 6 处判定式换成一次读点；语义逐格保持）

- 新增 `crates/qx-core/src/venue.rs`：`pub enum VenueFamily { Paper, Binance, Other }` 与
  `parse(&str)` / `parse_option(Option<&str>)`。三条口径写在函数体里并各有一条用例：Binance 是
  **trim + 小写后的子串**、Paper 是 **trim + 小写后的整名**、缺席**不等于** `Other`（仍是 `None`，
  内核底线第 9 条）。
- 19 处 worker 级写法 → `VenueFamily::parse_option(worker.venue_id.as_deref()) == / !=
  Some(VenueFamily::Paper | Binance)`。`!=` 的两处（`paper_worker.rs` 的原
  `map(|venue| !…).unwrap_or(true)`、`topology_validation.rs` 的规格豁免）保留 None⇒报错/None⇒不豁免
  的方向；`paper_submit.rs` 那份 `unwrap_or(false)` 保留 None⇒不匹配。11 个消费者文件逐个登记进
  门禁的 `VENUE_FAMILY_CONSUMERS`。
- 6 处标的级整名比较 → `VenueId::is_binance()`（`crates/qx-core/src/identity.rs:30`）。那里必须是
  **整名**：`BINANCE` 是产品 venue 名、不是一种账户域，定义处注释写明"不能改成子串"。
- `scheduler.rs` 的 `environment.eq_ignore_ascii_case("paper")` 判的是部署环境不是 Venue，未并进本轮，
  四条判定式指纹都带 `venue` 接收者，所以它不会被误伤。
- 为满足 500 行棘轮（本轮把新增用例留在 `lib.rs` 时门禁报过
  `crates/qx-orchestrator/src/lib.rs(537 行) 未登记`，见 `logs/s27_gate_a3_draft.txt`），把
  `crates/qx-orchestrator` 的内联用例模块拆到 `src/tests.rs`：`lib.rs` 313 行 + `tests.rs` 225 行，
  `maturity/line_budgets.yaml` 无需新增登记项。

### Removed（私有面，附"仓内无读者"取证）

- `crates/qx-runtime` 的私有 `fn is_binance(venue_id: &Option<String>) -> bool`（3 个调用点：
  两条 `&& is_binance(&worker.venue_id)`、一条 `Reconciler && is_binance(...)`）。取证：它不是
  `pub` 项、只在该文件内被引用，改后 `grep -rn "fn is_binance" crates/` 只剩两处命中 ——
  `VenueId::is_binance`（新单源）与 `binance_venue.rs:3` 的 `is_binance_testnet`（判 testnet 端点，
  与家族无关，保留）。因此不外溢到任何公共面，随仓库发布的产物与命令面口径不变。

### Added（5 条用例 + 8 颗判据）

- `venue.rs` 三条家族用例（子串与大小写 / 整名且前缀不算 / 缺席不成 `Other`），加上编排侧两条
  **消费者**用例：`binance-testnet` 必须走私有 Binance worker；`" Paper "`（带空格）仍走
  `paper-worker`，而把同一格换成 `paper-proxy` 后 `plan_workers` 直接 `Err`。
- `tools/check_architecture.py` 的 `venue_identity_check()` 8 颗：定义点形状唯一（`pub fn parse(` /
  `pub fn parse_option(` / `pub enum VenueFamily` / `pub fn is_binance(` 各一处）、四条已作废判定式
  不得在生产代码复活、消费者登记表与实盘逐一对应、三条家族用例在册且 `#[test]` 数不减、编排两处
  断言现场各自钉住（testnet 分派 / `paper-proxy` 反例）、账户命名用例钉住同一家族口径、
  两份口径的差异必须写在各自定义处注释里。
- 地板按本轮实测取值：`GATE_CHECK_FLOOR` 481 → **489**（+8）、`WORKSPACE_TEST_FLOOR` 855 → **860**（+5）。

### Not done as planned（一处对方案的偏离，如实记录）

- V13 §5 A3 原本设想装一条通用的"同形 lambda 出现 ≥3 次即报"判据。本轮实测**不成立**：按与
  `merge_duplicate_block_check` 相同的取数口径扫 172 个生产代码文件（`crates/*/src/**/*.rs`，剔除
  tests 目录与 `*_tests.rs`、截断到首个 `#[cfg(test)]`），W=10 的逐字重复窗口有 **480 组 / 涉及
  1,095 个位置**，去掉捕获名后同形闭包体 ≥3 次的有 **101 组 / 492 个**（后者是粗归一化的上界，
  方法连取数口径一起落在 `logs/s27_shape_scan_a3.txt`）。绝大多数是合法并列（编排五个角色分支各自的
  启动参数、三家 Venue 的提交链、几家存储后端）。装上它要么第一天上百条允许清单，要么逼人把合法并列
  改绕。故改成"每条收拢口径一张指纹表 + 一份消费者登记表"的点名式判据，通用形状判据留在 R3-3 未装。
- 最直观的一条证据就在本轮产物里：收拢**之后**，`crates/qx-orchestrator/src/lib.rs:56,78,…` 那五个角色
  分支在 W=10 窗口下仍然逐字相同（`s27_shape_scan_a3.txt` 的第一组就是它，5 份；那里的行号按门禁口径
  是"去空行后的序号"，故与 56/78 不等）。相同的是"都调用同一个函数"，而这正是想要的形状 ——
  所以判据钉的是**那个函数只有一处定义**，不是"这段调用出现了几次"。
- 顺带说明 V12 §20 那条重复块判据（`MERGE_DUP_WINDOW`）为什么抓不到上面那 5 份**逐字相同**的复制：
  它的零容忍窗口只对 `tools/*.py` 生效，而同一把尺子在 Rust 侧刚量出 480 组合法并列 —— 这也是本轮把
  "只有一处定义"做成登记表而不是做成重复扫描的原因。

### Validation（日志在 `logs/s27_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 门禁整跑 | `gate_exit=0`，489 条 `PASS` / 0 条 `FAIL`（本轮 +8 颗） | `s27_gate_final_a3.txt` |
| 文档回合之后复跑门禁（本轮改了 README / deploy/README / capabilities / V13 / CHANGELOG） | 仍 489 条 `PASS` / 0 条 `FAIL` / `gate_exit=0`；能力矩阵计数同场重测 | `s27_gate_docs_a3.txt`、`s27_capability_counts_a3.txt` |
| 静态判据变异 12 次运行（`.gate_mut/` 副本树，真树未动） | BASELINE 0 红；M1–M11 **每发恰好点名 1 条红**，`合计 12 次运行，未咬住 0 条：[]` | `s27_gate_mut_a3.txt` |
| 行为变异 5 项检查（真树 + `%TEMP%` 逐字节镜像还原） | B1（子串→整名）同红两条编排/账户用例、B2（整名→前缀）红 `exact_paper_venue` 用例；复跑两次全绿；`合计 5 项检查，未咬住 0 项：[]` | `s27_behaviour_mut_a3.txt` |
| 整树测试（`cargo test --workspace --all-targets --no-fail-fast`，`QX_PYTHON` 指向 venv） | 72 个测试二进制段、**852 passed / 0 failed**、`workspace_exit=0`（较上轮 847 恰 +5 = 本轮 5 条新用例） | `s27_cargo_test_workspace_a3.txt` |
| Doc-tests（`cargo test --workspace --doc`，`--all-targets` 不含它，所以单跑补全口径） | 21 段全 ok、0 passed（仓库无 doctest）、0 failed、`doc_exit=0`；与上一行合起来 93 段 | `s27_cargo_test_workspace_doc_a3.txt` |
| `cargo test -p qx-storage --features sqlite`（feature 门后的 8 条不在整树里） | 10 段全 ok / 56 passed / 0 failed / 退出码 0 | `s27_cargo_test_storage_sqlite_a3.txt` |
| Python 全量 / 内核自校验 | `Ran 55 tests OK (skipped=1)`（与 A2 轮同数，本轮未动 Python）/「全部自校验通过 ✓」 | `s27_python_unittest_a3.txt`、`s27_validate_core_a3.txt` |
| fmt / clippy | `cargo fmt --all -- --check` 退出 0；`cargo clippy --workspace --all-targets -- -D warnings` 退出 0 | `s27_cargo_fmt_check_a3.txt`、`s27_cargo_clippy_a3.txt` |
| `venue_id` 取值分布实测（本轮新量具，三条口径各自的真实支撑面） | 全仓 103 处赋值：`paper` 36 / `okx` 23 / `binance-testnet` 19 / `binance` 15 / `" Paper "` 2 / `BINANCE` 2 / `Paper` 1 / `paper-proxy` 1 / `other`\|`unmanaged-venue`\|`unknown-venue`\|`OKX` 各 1。子串口径要保住 `binance`+`binance-testnet`+`BINANCE` = **36 处**，trim 口径要保住 `" Paper "` 那 **2 处**，整名口径要排除 `paper-proxy` 那 **1 处** | `s27_venue_values_a3.txt` |
| 通用"同形写法"判据的可行性实测（决定 A3 用点名式还是形状式） | 172 个生产代码文件：W=10 逐字重复窗口 **480 组 / 1,095 个位置**、同形闭包体 ≥3 次 **101 组 / 492 个**；收拢后的编排五分支仍在第一组里 | `s27_shape_scan_a3.txt` |

### Documented（口径进接口文档）

- `deploy/README.md` 的 `paper_initial_cash_raw` 段之后新增"`venue_id` 怎么被读"三条口径（子串 / 整名 /
  缺席），点名 `paper-proxy` 那条反例的用例现场；此前这三条只散在各处注释里。
- `maturity/capabilities.yaml` 的 `paper_execution` 块 +2 证据行（家族单源与其消费者用例现场），
  实测计数随之为 19 块 / 279 条证据行 / 其中 247 条以仓库内路径开头 / 75 条 limitation
  （`logs/s27_capability_counts_a3.txt`）。
- `README.md`：构建段明确标出 `logs/s22_build_bat_full.txt` 那串 460/843 是历史快照、与今天的门禁条数
  不是一回事（§4 L3-1 抓的正是"两个历史值并存"）；"当前状态"段按本轮日志重抄（481 → 489 条、
  地板 855 → 860、847 → 852 passed、证据行 277 → 279 / 245 → 247），并把 A1 那节的引用从 §9.3 改对到 §9.1。
- **本轮自己制造、也当场抓到一处文档漂移**：A3 在 `paper_worker.rs` 里删掉的几行使
  `reject_split_account_principal` 的调用点从 103 移到 **93** 行，而 `maturity/capabilities.yaml` 的
  证据行写的是 `paper_worker.rs:103`。门禁的"证据路径全部存在"那颗把 `:NN` 剥掉再查文件
  （`tools/check_architecture.py` 里 `re.sub(r":\d+$", "", token)`），所以锚点失效不会让它红。
  本轮手改这一处，并把它作为量具盲区登记进 V13 §9.6 L4-3。

### Corrected（本轮量具自己的三处设计缺陷，当场改红）

- 第一版判据把"已收拢的判定式不得复活"跑成 8 个文件命中 —— 因为 Paper 那一侧 10 处当时**还没收拢**。
  改成先收完再钉指纹，并把指纹全部加上 `venue` 接收者锚。
- "消费者登记表逐一对应"那颗一开始被 `identity.rs` 的文档链接骗绿：判据扫的是原文，注释里的
  `VenueFamily` 也算消费者。改成扫 `without_line_comments(non_test_source(..))`，文档注释单独取数
  喂给那颗"差异写在注释里"的判据。
- M4 第一次不咬（把登记项换掉后行里仍留着 `VenueFamily::Paper`）、M6 的 needle 因 `cargo fmt` 重排
  缩进而 0 命中、M11 原设计（改名 `parse_lenient`）根本不会撞到 `pub fn parse(` 的计数 —— 三条分别
  改成整表达式替换、按 fmt 后文本取 needle、复制出第二个 `pub fn parse_option(` 签名。


## Unreleased — V13 R1-A2：A 股线格式两侧对照钉住，写侧顺带修掉三处口径（2026-09-26）

`python/qianxing_ashare` 与 Rust 读侧（`crates/qx-xingban/src/ashare*`）共用一套公司行为线格式，此前
**没有任何判据比过两侧**：字段名册、动作名册、单位口径各抄一份，抄歪了全树用例照样绿。这一轮把对照
钉上，钉的过程本身就是收获 —— 三条都是实测抓出来的，不是读代码读出来的（取证见下表与
`docs/自研量化框架审计与重构方案-V13.md` §9.4）。

### Fixed（Python 写侧，三处真实口径错）

- **写侧认不回自己写出的动作名**：`_canonical_action_type` 是中文别名的子串匹配表，`suspension` 不含
  `suspend`、`new_share_issue` 不含 `增发`/`新股`，所以已经规范化的行读第二遍会掉成 `unknown` ——
  停牌事实丢、增发按现金红利参与折算。补上"线格式名精确认回"的短路（16 个名字逐个用例覆盖）。
- **`*_raw` 输入列被再乘一次 SCALE**：`cash_dividend_raw: 500000000`（每股 0.5 元的已定点值）读进来
  变成 5e17，即 5 亿元每股。按 `git show HEAD` 的那份基线数出来：**四列**走的是这条双重放大
  （`cash_dividend`、`interest_per_bond`、`settlement_qty`、`settlement_price`），另有**九列**的
  `_raw` 名根本不在输入别名里，规范化过的行读回时那格丢成 0（`rights_issue_price`、`issue_price`、
  `conversion_price`、`subscription_qty`、`rights_expiry_qty`、`repurchase_qty`、`repurchase_price`、
  `conversion_qty`、`conversion_target_qty`），只有 `issuer_total_shares` / `issuer_free_float_shares`
  两列当时是对的。新增 `_amount_pick`：**单位规则收敛到一处**，`<field>_raw` 视为已定点整数，其余别名
  按元/股乘 SCALE，15 个金额/数量字段全走它；比例另有 `_ratio_pair_pick` 认
  `<field>_num` / `<field>_den`。
- **带时区的 ISO 公告日期回落成除权日**：`_date_text` 只切空格，`2024-05-28T00:00:00+08:00` 解析失败
  返回 `None`，调用方把它当"没有公告日期"，于是 `published_at` 取除权日 —— PIT 可见时间被悄悄改晚，
  恰好是复现"公告日之后才可见"这类研究结论时最不能错的一格。改成空格与 `T` 两种分隔都截断。

### Changed（行为口径，会改结果）

- 上述第二条会改变**任何用 `_raw` 列名喂进 Python 规范化器的行**的结果：先前它按元/股解释（放大 1e9 倍）
  或丢成 0，现在按已定点整数收下，与 Rust 读侧、`deploy/README.md` 的线格式段同一口径。仓库内没有这种
  喂法：`normalize_corporate_action_rows` 的调用点在 Python 模块内部三处 provider 读行路径与用例里，
  而 `deploy/qianxing.ashare.actions.example.json` 与 `complex-actions.example.json` 那两份带 `_raw` 键的
  样例是**信封**、由 Rust 读侧装载，不经这条 Python 路径。因此不外溢到任何随仓库发布的产物；
  自定义 provider 若曾依赖那个错口径，需要改回真值。
- 第三条让 `published_at` 在带时间的公告日期下从"除权日"变成"公告日 00:00 上海"。仓库内样例走的是
  日期粒度，结果不变。

### Added（一条夹具链 + 两侧用例 + 14 颗静态判据）

- `python/tests/fixtures/ashare_actions_cross_check.{rows,payload,expectations}.json`：数据源原始行 →
  Python 写出的 v1 信封 → 由信封派生的期望值。两侧共读，锚定那格
  `(10 + 5×0.2 − 0.5) ÷ (1 + 0.3 + 0.2) = 7.00` 由 Python 与 Rust **各自独立复算**（延续 V11 R17
  日历指纹夹具那条"谁都不抄它"的设计）。
- Python 六条用例（payload 逐字节重算、期望值派生、锚独立复算、16 个动作名逐个认回、`_raw` 与别名
  两种单位口径不混、ISO `T` 日期不回落）、Rust 两条用例（信封读回逐字段比 + 喂进 A1 那道折算锚）。
- `tools/check_architecture.py` 的 `ashare_cross_language_contract_check()`：14 颗判据覆盖版本号、
  三份字段名册（逐项、含顺序、且等于 Rust 声明的数组长度）、16 个动作名、写侧认回自己的名字、
  定点单位单源、两种 ISO 分隔、夹具齐备、两侧词根逐字相同、以及**锚定期望值不得抄成数字字面量**。
- `deploy/README.md` 新增"公司行为 v1 线格式"段：5 个信封键、35 个动作键、16 个动作名、单位与日期
  两条口径、以及三条离线自证命令。

### Validation（日志在 `logs/s26_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 门禁整跑 | `exit=0`，481 条 `PASS` / 0 条 `FAIL`（`GATE_CHECK_FLOOR` 467 → **481**，本轮 +14） | `s26_gate_a2.txt` |
| 静态判据变异 15 例（`.gate_mut/` 副本树，真树未动） | baseline 14 项 0 红；M1–M15 **每发恰好点名 1 红**，`VOID-JUDGES: none` | `s26_mutate_a2_gate.txt` |
| 行为用例变异 6 例（真树 + `%TEMP%` 逐字节镜像还原） | B1–B6 全 HIT；B4（payload 改一字节）与 B5（锚期望值改一格）**两侧同红** | `s26_mutate_a2_behaviour.txt` |
| Python 全量 | `Ran 55 tests OK (skipped=1)`，本轮 +6 条（49 → 55） | `s26_pytest_full.txt` |
| 整树测试（`cargo test --workspace --all-targets --no-fail-fast`，`QX_PYTHON` 指向 venv） | 72 个测试二进制段、**847 passed / 0 failed**、退出码 0（较上轮 845 恰 +2 = 两条新 Rust 用例）；跑完 `git status -- deploy/data` 0 项 | `s26_cargo_test_workspace.txt` |
| Doc-tests（`cargo test --workspace --doc`，`--all-targets` 不含它，所以单跑一遍补全口径） | 21 段全 ok、0 failed、退出码 0；与上一行合起来就是 93 段 / 847 passed | `s26_cargo_test_workspace_doc.txt` |
| `cargo test -p qx-storage --features sqlite`（feature 门后的 8 条不在上树里） | 10 段全 ok / 56 passed / 0 failed / 退出码 0，与上一轮同形 | `s26_cargo_test_storage_sqlite.txt` |
| fmt / clippy / 行数棘轮 | `cargo fmt --all --check` 退出 0；`cargo clippy -p qx-xingban --all-targets -- -D warnings` 干净（新用例里一处 `u64 → u64` 冗余转换被它当场逮到）；`line_budgets.yaml` 重快照新增 `ashare/tests.rs: 764` | `s26_fmt_clippy_a2.txt` |

### Corrected（本轮量具自己的三处空转，全部当场补红）

- **第一轮 14 颗里有 3 颗打不红**：`VOID-JUDGES: [M8, M10, M13]`。M8 只查 `text.split("T", 1)[0]`
  在不在，把 `elif "T"` 改成 `elif "Z"` 那半行还原样在；M10 用子串查词根，`STEM =
  "..._cross_check_forked"` 照样算引用了同一份；M13 在整个 `tests.rs` 里查
  `expected_reference_raw`，把断言换成字面量 `7_000_000_000` 后它在别处还出现一次。三条分别改成
  "条件与截断都要在"、"两侧按整名引用夹具"、"断言现场不得是裸数字"，重跑 14/14 命中；再为下面那条
  补上第 15 发（M15），最终 `VOID-JUDGES: none`。
  → 又一条 V13 §4 L4 证据：**判据的取证范围要收到被断言那一格，不能是"文件里出现过"**。
- **锚那一格原本只有 Rust 有立场**：B5（改共读的 `expected_reference_raw`）第一次跑，Python 侧全绿 ——
  `test_expectations_are_derived_from_the_blessed_payload` 只派生计数与逐事件行，从不复算锚。补一条
  Python 侧独立复算公式的用例后，B5 才做到"两侧同红"。
- 磁盘 `#[test]` 地板 853 → **855**（两条新 Rust 用例）；在册/实跑差仍是 8 条 feature 门后用例，
  与 V12 同形，挂在 `#144` 未动。


## Unreleased — V13 R1-A1：除权除息日的锚改为从已装载的公司行为折算（FN3 收口）（2026-09-26）

V11 Q60 把涨跌停的锚从"上一根 Bar"改成"上一交易日收价"，同时在 `maturity/capabilities.yaml` 留了一条
limitations：`ashare_previous_close_raw_has_no_in_repo_producer_so_ex_rights_anchors_stay_unadjusted`。
当时的事实是：覆盖表是除权日唯一的正确锚出口，而仓库里没人往那张表里写东西。这一遍不去补写表的人，
而是把**已经装载好的红利/送转/配股**接进锚的计算 —— 数据侧那条路（`ashare_binding.rs` 与 `runtime_check.rs`
各自调用 `apply_corporate_actions_json`）本来就在，只是锚从不读它。

### Changed（行为口径，非破坏性但会改结果）

- **除权除息日的昨收现在会折算**（`ashare/trading.rs` 新增私有 `ex_rights_reference`）：
  `(昨收 + 配股款 − 每股现金红利) ÷ (1 + 送转比例 + 配股比例)`，结果按 `price_tick` 对齐。
  动作识别复用内核账本那一份 `is_cash_dividend_action`（由私有改 `pub(crate)`），**没有第二份动作清单**。
- **`previous_close_raw` 降级为覆盖出口**：它仍是锚的第一个读点（手工指定优先于折算），但不再是唯一出口。
  字段与 `deploy/qianxing.ashare.rules.json` 的样例键都保留 —— 本轮明确不改产物形状（Q1b 的重 bless 债另账）。
- 折算只在跨日推导**之后**发生：无公司行为的普通交易日，锚与 Q60 落地后逐字节相同。

### Added（`ashare_limit_anchor_check` 内四条，门禁 463 → 467）

- **折算读的是账本**：`ex_rights_reference` 必须命中 `.corporate_actions`、`is_cash_dividend_action`、
  `cash_dividend_raw`、`rights_issue_price_raw` 与 `self.price_tick`。
- **顺序**：`bars[..index]` 的跨日扫描必须在 `self.ex_rights_reference(...)` 之前，覆盖表仍是第一出口。
- **行为用例在册**：两条用例名与两个期望锚值（`Some(yuan(950))` / `Some(yuan(688))`）逐字核对。
- **折算的输入必须有非测试写点**：扫 `crates/*/src/**/*.rs`，只数实例方法调用点，注释行、文件尾
  `#[cfg(test)]` 模块、`src/**/tests.rs` 这类整文件即测试的路径、以及定义处向 `_with_report` 的
  `self.` 内部委托一律不算；qx-cli 侧期望 ≥2 处。

### Validation（日志在 `logs/s25_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| `ashare::tests::` 单库 | 18 passed / 0 failed（本轮新增 2 条） | `s25_mutate_a1_behaviour.txt` 首行 |
| 行为变异 3 例（真树 + `base_a1/` 镜像还原） | B1/B2/B3 每发各让 1~2 条用例变红，还原后回到 `('18','0')` | `s25_mutate_a1_behaviour.txt` |
| 文本判据变异 6 例（`.gate_mut/` 副本树，真树未动） | baseline 8 项 0 红；M1–M6 **每发恰好 1 红** | `s25_mutate_a1_gate.txt` |
| 门禁整跑 | `GATE_EXIT=0`，467 条 `PASS`、0 条 `FAIL` | `s25_gate_a1.txt` |
| `cargo test --workspace --no-fail-fast`（`QX_PYTHON` 指向 `python/.venv`） | 71 个测试二进制段 845 passed / 0 failed，另 21 段 Doc-tests 0 failed，退出码 0；相对上一轮 843 恰好 +2 | `s25_cargo_test_workspace.txt` |
| fmt / 行数棘轮 | `cargo fmt --all -- --check` 退出 0；`line_budgets.yaml` 重快照：新增 `ashare/tests.rs: 561`，`qx-guanxing/lib.rs 590 → 577` 一并收紧 | 同上配套 |

跑完整套测试后 `git status -- deploy/data` 为 0 项 —— 本轮没有把任何被跟踪产物改写。

### Corrected（本轮自己踩到的三个假绿/假绿边缘，全部立案）

- **M5/M6 第一发全绿**：新加的"非测试写点"判据被 `crates/qx-xingban/src/ashare/tests.rs` 里 13 处测试调用
  喂饱了 —— `is_test_scoped` 只认文件内尾部的 `#[cfg(test)]`，认不出"整份文件就是一个测试模块"这种挂载方式；
  另外定义处 `self.apply_corporate_actions_json_with_report(...)` 那一跳也被数成写点。两类都补进排除条件后
  重跑，M5/M6 才各自变红。这是 V13 §4 L4「判据的取数方式本身要能被证伪」的又一次实测。
- **字面量判据被 rustfmt 拆行打瞎**：`self.corporate_actions` 被格式化成 `self\n    .corporate_actions`，
   needle `self.corporate_actions` 当场红。判据改用跨行仍成立的 `.corporate_actions`，属于 V13 L4 的取证。
- **镜像必须在定义基线之后采集**：行为变异脚本的还原源指向了 A1 编辑**之前**的 `%TEMP%/mirror_s25/`，
  B1 一发就把整份实现还原掉，B2/B3 因此报"needle absent"，而结尾那行 `RESTORED-GREEN` 其实是
  `16 passed; 2 failed`。重放实现 + 改用 `base_a1/` 之后复跑才是上表那三行。附带一处 Windows 事故：
  `shutil.copy2` 还原时撞上 `WinError 1224`（文件被用户映射区域占用），脚本改成"写字节 + 读回比对 + 重试"，
  并且**只在内容确实等于基线时**才打 RESTORED 标记。


## Unreleased — V12 §23：变体级生产者判据落地，六颗零生产者变体删除，#137 收口（2026-09-26）

§19.6 列出四颗候选、§21.6 给出处置结论，两份都没动手。这一遍不写四个特例，而是把"每个 `pub enum`
变体都要有人在生产代码里把它造出来"接成常驻判据（门禁第 25 项）。同一把尺子在删除之前的快照上报出
**12 颗零限定名生产者**（52 个枚举 / 242 个变体），其中 3 颗是前两轮点名没点到的。

### Added（`enum_variant_producer_check`，门禁 460 → 463）

- **覆盖面地板**：解析出的枚举 ≥ 50、变体 ≥ 200（本轮实测 52/236）。挡"正则退化成一个都不匹配 → 第二条因为没有变体可判而全绿"。
- **变体级生产者**：每个 `pub enum` 变体必须有生产代码里的 `Enum::Variant` 限定名命中，或在允许清单里说明它的
  生产者在线格式上。`impl Enum` 花括号配平块内的 `Self::Variant` 一并计入（否则 `AshareBoard::Etf`、
  `CommandKind::CancelOrder` 会被误判成孤儿）；读者剔掉 `#[cfg(test)]` 与 `crates/*/src/tests/**`。
- **允许清单逐条可复核**：6 条 `CorporateActionType` 各自要同时满足"变体仍在册、所在枚举确实 derive 了
  `Deserialize`、且确实零生产者"；`Deserialize` 的证据只认 `pub enum` 上方连续的 `#` 属性行。

### Removed（破坏性，六颗变体）

- `qx-risk::RiskDecision::Rebalance`、`qx-zhenlu::ConnectorState::{Connecting, Disconnected, Degraded}`、
  `qx-zhenlu::StrategyState::Stopping`、`qx-provider::ProviderErrorClass::Retryable`。
  逐颗判定见 V12 §23.2；连接器侧"断开/降级"分别由 `ReconcileRequired` 义务与监管侧 `ServiceStatus::Degraded` 承担，
  组合再平衡的表示从来是 `qx-zhenlu::portfolio::RebalancePlan`，删除不使任何在做的事失去表示。
- **对外可见面**：`RiskDecision` 与 `ProviderErrorClass` 带 `Deserialize`，故 JSON 里的 `"Rebalance"` / `"Retryable"`
  从"能解出来但落进无人认的档位"变成**直接报错**。仓内 `schemas/`、`deploy/*.json`、`python/` 全搜无生产者；
  这两颗档位从未出现在 README 或接口文档里。

### Corrected

- **V12 §21.6 那条推测作废**：原文写"缺的映射点在 `crates/qx-adapter/src/ccxt.rs:178-188`"。复核后该处
  `CcxtRpc::call` 返回 `Result<Value, String>`（`ccxt.rs:26`），worker 的 `error.class` 只被拼进消息（`ccxt.rs:188`），
  与 `ProviderErrorClass` 之间**没有类型通路**——不存在漏接的映射点。`Retryable` 的处置结论仍是删，但理由换成"干净孤儿"。

### Validation（日志在 `logs/s23_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 门禁整跑（真实文件） | `GATE_EXIT=0`，463 条 `PASS`、0 条 `FAIL` | `s23_gate_final_green.txt` |
| 删除前/后同一把尺子对照 | before 52 枚举 / 242 变体 / 12 颗零生产者；after 52 / 236 / 0 颗 | `s23_before_after_scan.txt` |
| 裸名 vs 限定名词形实验 | 12 颗里 **7 颗**按裸名数会被数成"有生产者"（`Degraded` 裸名 12 次全属 `ServiceStatus`/`OverallHealth`；`Stopping` 3 次全属 `ServiceStatus`；`Disconnected` 2 次全属 `mpsc::RecvTimeoutError`） | 同上 |
| 变异 7 例（%TEMP% 镜像，真树未动） | `ALL_CASES_MATCH=yes`：M0 0 红；M_A/M_B/M_C 各 1 红命中第 2 条；M_D 2 红（覆盖面 + 清单）；M_E/M_F 各 1 红命中第 3 条 | `s23_mut_variant_harness.py`、`s23_mutation_summary.txt` |
| `cargo test --workspace --all-targets` | `TEST_WS_EXIT=0`，72 段 `test result: ok`、843 passed、0 failed，warning/error 行 0 | `s23_cargo_test.txt` |
| Clippy（本轮动过的 6 个 crate，`-D warnings`）/ fmt / check | 逐个退出 0；`cargo fmt --all --check` 退出 0；`cargo check --workspace --all-targets` 无警告 | 同上配套 |

843 与 §19/§20/§21/§22 逐项一致：本轮动的是类型面与门禁，被测行为没有漂移。
一处自伤值得留档：第一次跑整树测试时我把 `QX_PYTHON` 落到 WindowsApps 占位桩，两条 Python 契约用例当轮就红，
而报错文本自己点名了解释器来源（§19 那条诚实化第二次省下排查时间）。

## Unreleased — V12 §22：本地构建路径第一次自己跑完架构门禁（八步改九步，#139 收口）（2026-09-26）

§21 收工时量出的那颗：`build.bat` 的八步里没有一步执行 `tools/check_architecture.py`，只有 CI 跑它
（`.github/workflows/ci.yml:34`）。也就是说，照 README 安装档 B 在自己机器上看到"全部完成 (all gates
passed)"，并不证明这四百多条不变量成立 —— 而 §21 刚刚还给这把尺加了"判据不许被抄成两份"的自我约束。
这一遍把接线本身做成判据，并当场跑出九步。

### Added（`build_script_parity_check` 内三条，门禁 457 → 460）

- **调用存在**：`build.bat` 与 `build.sh` 各恰好有一行**命令**调用 `tools/check_architecture.py`；注释行与
  `echo` 提示语不计（§19 #136 的口径）。
- **失败会中止**：bat 调用行之后紧跟 `if errorlevel 1 goto :err`，sh 调用行之前必须已有 `set -euo pipefail`。
- **排在 cargo 之前**：调用行早于该脚本第一条非注释 `cargo` 命令 —— 挡"接在最后一步"那种慢失败接法。
- **`build.bat` / `build.sh` 新增第 `[1/9]` 步**：`"%QX_PY%" tools/check_architecture.py`，原 `[1/8]..[8/8]`
  顺移为 `[2/9]..[9/9]`，`[0/9]` 解释器探测与 `QX_PYTHON` 交接不变。排第一是因为它只读源码、本机实测
  38.9 秒，红得早；`build.bat` 全程按字节改以保持 CRLF，新增注释写成纯 ASCII（门禁要求每行以 ASCII 结尾）。

### Changed

- `build_script_parity_check` 的写死 pins 跟着走：步骤数 8 → 9，`first_gate` 正则由 `\[1/8\]` 改为
  `\[1/\d+\]`；`cargo` 命令多重集仍是 7 条（新步是 Python 调用）。
- `GATE_CHECK_FLOOR` 457 → 460（本轮实测总条数）。
- README 安装档 B 与"构建与发布""装不上时的四个坑"改成九道门禁/新步骤号，并说明第 `[1/9]` 是什么、
  为什么排第一；`docs/外部链路验收执行方案-V1.md` 的步骤号同步。历史章节里的 `[n/8]` 是当时实测事实，不追改。

### Validation（日志在 `logs/s22_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 门禁整跑（真实文件） | `GATE_EXIT=0`，460 条 `PASS`、0 条 `FAIL` | `s22_gate_final_green.txt` |
| 变异 `M0_control` | 0 条 FAIL（基线） | `s22_M0_control.log` |
| 变异 `M_A` 删掉整步 / `M_B` 只在注释里提脚本名 | 各 6 条 FAIL，含三条新判据 | `s22_M_A_no_call.txt`、`s22_M_B_comment_only.txt` |
| 变异 `M_C` bat 去掉 `goto :err` / `M_D` sh 去掉 errexit | 各 1 条 FAIL，只红"失败会中止" | `s22_M_C_*.txt`、`s22_M_D_*.txt` |
| 变异 `M_E` 把门禁挪到最后 | 1 条 FAIL，只红"排在 cargo 之前"（调用行 bat 81 / sh 74 vs 首个 cargo 37 / 49） | `s22_M_E_gate_moved_last.txt` |
| `build.bat` 全 9 步 | `BUILD_BAT_EXIT=0`；**`[1/9]` 由本地构建路径自己印出 460 PASS / 0 FAIL**；`[4/9]` 92 段 `test result: ok` / 843 passed / 0 failed；`[6/9]` `Ran 49 tests` | `s22_build_bat_full.txt` |

92/843/49 与 §19、§20、§21 逐项一致：本轮只动构建脚本与门禁，被测行为没有漂移。

### Known（未收口）

`build.bat` 九步全过不等于 CI 全集（`--ignored` 的 Postgres/NATS 契约、feature 矩阵、三平台 wheel 矩阵仍只在
CI）；"每遍都要重跑九步"依旧靠人工实跑，门禁执行不了 `cargo`；`#137` 的四颗只完成判定、代码未动；
`sandbox_tested` 仍全为 `false`；`qx-cli --version` 仍未实现。详见 V12 §22.7。


## Unreleased — V12 §21：把"合流接出来的第二份判据"变成常驻门禁，并量出本地构建路径根本不跑门禁（2026-09-26）

§20 留下一张欠条：那颗"双方各加同一段、git 不产生冲突标记"的缺陷只靠一个住在 `%TEMP%` 的一次性扫描器
证明过"全树无第二例"。这一遍把它变成仓库里常驻咬得住的判据，顺手补上 §20.5 第 5 条欠的 `build.bat`
全 8 步重跑 —— 并在核对那份日志时量出新的一颗（#139）。

### Added（门禁第 23 项：`merge_duplicate_block_check`，挂在 `main()` 最前）

- **窗口条**：`tools/*.py` 加上**正在运行的那一份自己**不得出现逐字重复的连续 10 行，命中时点名
  `文件:首处==次处`。门槛不是拍出来的：同一把尺在 318 份受版本控制的 `.rs`/`.py` 上
  W=8 命中 1,180 处 / 70 个文件、W=10 仍有 781 处 / 43 个（`crates/qx-storage/src/{nats,postgres,sqlite}.rs`
  那类合法并列实现），全树零容忍只会立一条假判据；而 `tools/*.py` 在 W=10 是 **5 份脚本 / 0 命中**。
- **描述条**：用 `ast` 静态收全部字面 `check(...)` 描述，同一条出现两次即红，并要求收到的条数 ≥ 300
  （跌破意味着取数方式本身失效）。本轮实测字面 353 条、另有 35 条 f-string 拼接不在集合内。
- `.gitignore` 收下 `/logs/`（每轮原始日志，数字才进文档）与 `/.gate_mut/`（变异取证用的门禁副本挂载点）。

### Changed

- 门禁条数地板 `455 → 457`（就是新加的两条），`GATE_EXIT=0`、打印 457 项。
- 描述条上线时当场抓出一处**既有**的共用描述（`日历夹具与摘要成对存在…` 在 `[4653, 4673]` 各印一次）：
  那条的分支支与聚合支断的是两件事，拆成两条各自描述，`DUP_LABELS` 归零。

### Validation（本轮实测，日志在 `logs/s21_*.txt`）

| 项 | 实测 | 日志 |
| --- | --- | --- |
| 红绿对（5 次跑真实文件的临时副本，被检文件 `GATE_FILE_UNTOUCHED=True`） | 基线 457 项 / 1 项已知伪红；M-A 整段重复 → 只有窗口条红；M-B 单行改描述 → 只有描述条红 | `s21_mutation_pairs.txt`、`s21_mut_*.txt` |
| §20 缺陷重建（取上游 `dbfc429` 的 19 行 R10 段落插回原处） | 摘掉新判据：退 1、**0 条 FAIL**、打印 315 条后 `NameError`；带上新判据：崩前先把重复窗口与重复描述各点名（317 条） | `s21_mut_control_defect_{with,without}_rule.txt` |
| 门禁整跑 | `GATE_EXIT=0`、457 项、0 条 FAIL | `s21_gate_final_green.txt` |
| `build.bat` 全 8 步（收口 §20.5 第 5 条） | `BUILD_BAT_EXIT=0`，`[3/8]` 92 段 `test result: ok` / 843 passed / 0 failed，与 §19、§20 逐项一致；解释器 3.12.13 且 `QX_PYTHON` 已外传 | `s21_build_bat_full.txt` |
| 本地构建是否跑架构门禁 | `grep -n check_architecture build.bat build.sh` = **0 / 0**，整份构建日志里没有"架构不变量自检"这一行；CI 跑（`.github/workflows/ci.yml:34`）→ 立案 #139 | 同上 |

### Known（本轮新登记）

- **#139**：照 README 安装档 B 从源码构建的用户，一次全绿的 `build.bat` **不执行**这 457 条不变量。
- **#137 只完成判定**：`ProviderErrorClass::Retryable` 全仓只出现在定义那一行而读者把它算作非终态
  （真生产者缺口，映射点在 `crates/qx-adapter/src/ccxt.rs:178-188`）；`RiskDecision::Rebalance` 与
  `ConnectorState::Connecting` 是干净孤儿应删；`CorporateActionType::{Split, Merge}` 带
  `serde(rename_all = "snake_case")`，属扫描正当盲区、进允许清单。代码未动。
- `sandbox_tested` 依旧全为 `false`；`qx-cli --version` 仍未实现。


## Unreleased — V12 §20：安装面三条路径各有退出码，并把上游 4 个提交真正合回来（2026-09-26）

§19 那一遍的产物全部留在未提交的工作树里，且 `origin/main` 领先 4 个提交。这一遍做两件事：把
"别人拿到仓库怎么装"写成实测过而不是期望中的路径，以及用一个**可复算的**合流判据替代"我看过了"。

### Added（安装面，README 新增「安装」章）

- **A 档 `cargo install --path crates/qx-cli --locked`**：2m10s 装出单个 `qx-cli.exe`，**在仓库外**
  跑 `help` / `init` / `doctor` / `backtest` / `report` / `status` 六条命令退出码全 0，回测落
  `result_hash=26fdd6b52d020700`；这一档不需要 Python（日志 `s22_cargo_install.txt`、`s22_installed_*.txt`）。
- **B 档 `build.bat` / `bash build.sh`** 与 **C 档 wheel** 各写清前置：wheel 216439 字节，内嵌
  `_qianxing_native.pyd` 的 md5 与当轮 `target/release/_qianxing_native.dll` 同为
  `3e3c793385cf155e92c2cdc4f3694eba`，干净 venv 里 `available()` 为真（`s21_*.txt`）。
- **「装不上时的四个坑」表**：WindowsApps 的裸 `python` 占位桩、PowerShell 未签名脚本要
  `-ExecutionPolicy Bypass`、缺 `tzdata` 时 A 股用例的失败形状、`sed -i` 会抹掉 `.bat` 的 CRLF。
- **如实记录一处缺口**：`qx-cli --version` 没有实现，会按未知参数 fail closed 退 2；确认安装用
  `qx-cli help` 首行。（挂在 §20.5 未收口。）

### Changed（合流 origin/main 的 4 个提交：`b4921ea` / `5cd11f8` / `2bf8ad6` / `dbfc429`）

- **合并提交 `dbaa9d9`** 把远端 `main` 接进本地历史；15 个冲突文件全部取我方，理由是两条判据：
  合流后 `git diff --cached HEAD` **为空**（206 个文件与提交后的树逐字节相同），且逐行审计 61 个
  上游路径后只有 65 行不在我们树里、逐行确认全是同一事实的另一种写法（`production_text()` 取代
  `.split("#[cfg(test)]")[0]`、`report.consecutive_failures` 取代局部 `consecutive`、文案与
  `line_budgets` 数字的先后版本、12 行已拆进 `snapshot_single_source/` 目录的旧单文件正文）。
- **推送走显式 `git@github.com:coeasy/qianxing.git`**（HTTPS 在本机被重置），不改 `git config`；
  推送后 `git ls-remote` 与 `HEAD` 同为 `dbaa9d9c2a4bd94e7fdfa65486df79b1793120e1`。

### Known（本轮新增的一类盲区，挂 §20.3 / #138）

- **自动合并会把"双方各加同一段"首尾相接，且不产生任何冲突标记**：`snapshot_row_wire_check()` 里
  上游那份 R10 对账判据落在我方那份之后，第一次运行以 `NameError: reader is not defined` 崩在中途 ——
  而门禁此时 `GATE_EXIT=1`、**0 条 FAIL**，看起来像环境问题而不是内容问题；两段都跑还会让同一条检查
  印两行 PASS，靠"判据条数"量的量具会被抬高。本轮用一个一次性扫描器（对合流改动的文件找出现两次以上
  的 8 行窗口）证明全树无第二例，常驻判据待 §20.5。
- **本轮没有重跑 `build.bat` 全 8 步**（§18.7 第 2 条）：推送的树与 §19 那一遍逐字节相同，八步在 §19
  已实跑通过（`s19_build_bat_fix.txt`）。

### Verified（本回合实测，逐条可 grep）

| 门槛 | 结果 | 日志 |
|---|---|---|
| 架构门禁（文档回写前） | `GATE_EXIT=0`，455 项全绿 | `s23_gate_after_install_docs.txt` |
| 架构门禁（合流后修复前） | `GATE_EXIT=1`、0 条 FAIL、`NameError: reader` | `s24_gate_merge1.txt` |
| 架构门禁（合流完成） | `GATE_EXIT=0`，455 项全绿 | `s24_gate_merge2.txt` |
| 整树测试（合流后的树） | `TEST_EXIT=0`，92 段 / 843 passed / 0 failed / 0 ignored | `s24_test_workspace_after_merge.txt` |
| 安装 A 档 | 仓库外六条命令退出码全 0 | `s22_installed_help/init/doctor/backtest/report/status.txt` |
| 推送 | `dbfc429..dbaa9d9 → main`，`PUSH_EXIT=0` | `s24_push.txt` |


## Unreleased — V12 §19：第一次把 `build.bat` 全 8 步端到端跑通，当场抓到一条从未生效过的解释器交接（2026-09-26）

§18.7 第 2 条欠的账：「每遍都要重跑 `build.bat` 全 8 步」这条口径没有判据能执行 `cargo`。这一遍不写判据，
直接实跑 —— 第一次跑就抓到 **#133**：`[0/8]` 探测出的解释器只留在脚本自己的变量里，Rust 侧读的是
`QX_PYTHON`，所以 §17 加进来的那一步**从来没生效过**，`[3/8]` 照旧回落 PATH 占位桩。顺带清掉 §18-B 删面的
残留孤儿（#134）与零读者判据根本不看的那半边类型面（#135）。更有价值的是变异反向验证当场抓出**两颗永远不会红**
的新判据（#136 / #138）：本轮新增的 6 条里有 2 条第一眼是绿的、其实是假的。全过程见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §19，日志在 `%TEMP%/qx_v12p2/logs/`。

### Fixed（§19 #133：构建脚本的解释器交接）

- **`build.bat` / `build.sh` 把探测选中的解释器导出成 `QX_PYTHON`**：`[3/8]` 的两条 Python 桥契约用例与
  `[7/8]` 的策略 worker 由 Rust 去起 Python，而 Rust 只读 `QX_PYTHON` 一个变量
  （`crates/qx-cli/src/main.rs` 的 `python_interpreter_origin()`）。修前 `BUILD_BAT_EXIT=1` 且
  `[3/8]` 是 `202 passed; 2 failed`；修后 `[0/8]` 多印一行交接、八步全过、`[3/8]` 92 段全 ok / 843 passed。
  只改"仓库 venv"那一档：用户已设 `QX_PYTHON` 时原样保留，PATH 上的裸 `python` 不改写成不存在的路径，
  留给它自己的占位桩诊断。
- **编辑面硬约束**：`sed -i` 改 `.bat` 会把 CRLF 抹成 LF（实测 98 个孤立 LF，门禁立刻红），归一化只能按字节做。

### Removed（§19 #134 / #135：类型面孤儿）

- **`QualityIssue::CrossedBook` 连同 `verdict()` 的致命臂删除**：§18-B 把交叉报价判定收到
  `crates/qx-adapter/src/binance.rs` 的报价入口时只删了构造点，留下一个"永远构造不出来却声称致命"的变体。
- **`crates/qx-data/src/calendar.rs` 整文件删除 + 摘挂载**：16 行 `TradingCalendar` trait 全仓零实现
  （V11 §5 早已写明），还与 `crates/qx-scheduler` 真在驱动调度的同名 struct 撞名。
- **`qx-guanxing` 的 `NumericExt` / `to_display` 删除**：定义之外零出现的定点展示辅助，躲在"零读者判据只扫
  `pub fn` / `pub const`"的覆盖面之外。

### Gates（本轮 +6 条，地板 449 → 455）

- `build_script_parity_check` 两条：每个构建脚本都必须有把解释器导出成 `QX_PYTHON` 的**命令**，且早于 `[1/8]`。
- `quality_issue_producer_check` 两条：`QualityIssue` 每个变体都有构造它的生产者（判定臂不算）；
  `CrossedBook` 这个名字不得在任何 `crates/*/src` 生产文本复活，且 adapter 必须留着 `.is_crossed()` 那位真读者。
- `dead_type_surface_check` 两条：`NumericExt` / `to_display` 这类"定义之外零出现"的类型面删除后不得复活；
  同名日历类型只能有一个定义，且就在真的驱动调度的 crate 里（按**裸名字**数读者的判据会被同名类型误判，这条是自纠）。
- **两颗假绿判据的修正**：`interpreter_handoff()` 原先把 `build.sh` 报错提示里那句
  `echo " 或: export QX_PYTHON=…"` 当成脚本自己的导出行为（M2 因此整颗不咬，#136）；
  `CROSSED_QUOTE_NAME in _production_lines(text)` 对**行列表**用 `in`，判的是"存在一整行正好等于该名字"，
  于是 `revived` 恒空、判据恒绿（M3 因此只红一条，#138）。收口口径：**新增判据必须当场配一颗让它红的变异**。
- **变异协议补 `try/finally`**：第一次 M2 未咬时脚本 `AssertionError` 退出，把变异**留在了 `build.sh` 里**，
  靠 `%TEMP%` 镜像才还原。

### Docs

- `README.md`：`[0/8]` 那段补"挑中之后必须外传成 `QX_PYTHON`"的口径与被否证的旧表述；当前状态数字
  449 → 455；`build.bat` 全 8 步一次跑通写进实测台账。
- **README 新增「安装」章，把"快速装好并用起来"变成一条可复制路径**：三档安装（A `cargo install
  --path crates/qx-cli --locked` 只装 CLI、不需要 Python；B `build.bat`/`build.sh` 八道门禁；
  C 两条 wheel 入口 + 干净 venv 复核），每档末尾带本轮实测值。实测口径：`cargo install` 2m10s 产出
  `qx-cli.exe`，在**仓库外**的临时目录里 `help`/`init`/`doctor`/`backtest`/`report`/`status` 六条全部
  退出码 0，`init --strategy macd` 生成的项目自包含，回测写出四份产物、`result_hash=26fdd6b52d020700`
  （`s22_cargo_install.txt`、`s22_installed_*.txt`）。另加「装不上时的四个坑」表（Store 占位桩 /
  执行策略 / 缺 tzdata / 用 `sed` 改 `.bat` 抹掉 CRLF），以及一条如实记录：安装后的 binary **没有**
  `--version` 旗标，`--version` 按未知参数 fail closed 退出 2，确认安装用 `qx-cli help`。
- `docs/工业化易用性收口指南-V1.md` §1 写明"源码前缀 `cargo run -p qx-cli --` ↔ 安装后 `qx-cli`"是同一条
  clap 命令表，两条路径不必分别维护；`docs/外部链路验收执行方案-V1.md` §6 补上解释器外传的口径。

### Verified（本回合实测，逐条可 grep）

| 门槛 | 结果 | 日志 |
|---|---|---|
| `build.bat` 全 8 步 | 修前 `BUILD_BAT_EXIT=1`（`[3/8]` `202 passed; 2 failed`）→ 修后 `BUILD_BAT_EXIT=0`，843 passed / 0 failed | `s20r_build_bat_full.txt`、`s19_build_bat_fix.txt` |
| 架构门禁 | `GATE_EXIT=0`，**455 项全绿 / 0 条 FAIL**；爬升 449 → 453 → 455 | `s19_gate_judgefix2.txt`（两颗假绿判据修完复跑） |
| 本轮变异 | 6 颗：M1/M4/M5/M6 一次咬；M2 第一遍不咬 → 抓出 #136，修后红在唯一目标判据；M3 第一遍只红 1/2 → 抓出 #138，修后一次红两条。颗颗 `RESTORE EXACT` + 还原后 455 全绿 | `s19_mut_round19b.txt`、`s19_mut_m3_recheck.txt` |
| 安装包重建 | `SH_EXIT=0` + `PS_EXIT=0`，两条腿都 `Created wheel … size=216439` | `s21_wheel_rebuild.txt` |
| 安装包与当轮代码一致 | wheel md5 `fe94d8e5…`；内嵌 `_qianxing_native.pyd` 353280 字节 md5 `3e3c793385cf155e92c2cdc4f3694eba` **等于**本轮 `target/release/_qianxing_native.dll`（`MATCH`），12 份 `*.py` `mismatch: none` | `s21_wheel_verify.txt` |
| 干净 venv 复核 | `PROBE_EXIT=0`：`native.available() → True`、四个包全部导入成功、衍生品三字段 `cross/hedge/3` 往返 `True`、`margin_mode="weird"` 仍 `ValueError` | `s21_install_probe.txt` |

## Unreleased — V12 §18：第五遍逐条判定 §16 留下的 13 颗断链，4 颗接进生产、8 颗拆掉假装接上的桥（2026-09-26）

§16.6 第 3 条欠的账：13 颗"零消费者 / 无生产装配"的断链发现。这一遍不新增发现，逐条判**该接还是该写明没接**：
钱与账的 4 颗接进生产（§18-A），数据与血缘的 8 颗**删掉那条假装接上的桥**并写进对外矩阵（§18-B），
第 13 颗（#129）判为"接了更危险"，只改语义。轮末验证链又抓到两颗：一颗 clippy lint（本轮新用例自己留的），
一颗 README 承诺的 PowerShell 入口在本机默认策略下**一行都不执行**就退出（§18-C #132）。
全过程与 70 颗变异见 [docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §18，
日志在 `%TEMP%/qx_v12p2/logs/`。

### Fixed（§18-A：接进生产的四条）

- **柜台未报的币种不再折成 0（#109，交易 TX6）**：`crates/qx-runtime/src/pipeline.rs` 的
  `venue_raw` 改成 `Option<i128>`，只有 `Some(ledger_raw)` 才可能判一致；Binance 对账播报里缺席的币种印「未报」。
  新集成用例 `crates/qx-runtime/tests/settlement_balance_absence.rs` 钉住三条读法（报了 0 不算差异、
  没报落 `None` 且与"只报别的币种"同形、报过数仍是那个数）。
- **对账裁决的动作归类真的有读者（#108）**：`status_action` / `filled_action` 在
  `crates/qx-genglu/src/reconcile/order.rs` 各定义一次，裁决只调用不另起比较，`crates/qx-adapter/src/reconcile.rs`
  的 `action()` 委托同一对判据，报告多印一栏 `action`，待对账事实的 `reason` 从维度码改成动作口径。
- **超时作业会升级、并发键会释放（#110）**：`dispatch_scheduled_jobs` 每轮遍历运行做
  `is_timed_out` → `mark_timed_out`（升级 `NeedsIntervention`、释放并发键），`NotReady` 只跳过本轮，
  播报加 `timed_out=` 一栏；顺带把租约时钟统一到秒域（`lease_clock` 是唯一换算入口，八处命令队列入口 +
  派发写入的 `JobRun` 全按秒 —— 30 秒租约此前会被写成 30 毫秒）。
- **一次运行只交一个终态（#129）**：`finish_run_with_code` 的 `error_code` 只在失败侧保留，成功一律 `None`；
  Strategy worker 的收口改传 `None`（结果码只走 stdout），于是 `/scheduler/runs` 不会再显示一条带着
  `3 orders: SUBMITTED` 的"成功"作业。**自动重跑刻意不接** —— 交易作业失败时无法判定订单是否已出网，
  重跑等于二次提交；`JobStatus::Failed` / `next_retry_ts` / `retry_run_at` 这条链改为立成 limitation
  `scheduler_run_retry_has_no_production_path`，并由判据去数生产调用点。

### Fixed（§18-C：安装面的一颗）

- **README 的 PowerShell wheel 入口带执行策略**：`powershell -File tools/build_python_wheel.ps1` 与
  `./tools/build_python_wheel.ps1` 在 Windows 客户端默认策略（本机 `Get-ExecutionPolicy -List` 五个作用域
  全 `Undefined`，即 Restricted）下以 `UnauthorizedAccess` 退出、一行脚本都不执行。入口写成
  `powershell -NoProfile -ExecutionPolicy Bypass -File …`，`.ps1` 文件头同步写明；§17.8 记的 `PS_EXIT=0`
  因此被否证为"当时那个进程的策略恰好放行"。

### Removed（§18-B：拆掉假装接上的桥）

- **静态帧到 `ProviderRegistry` 的注册桥整条删除（#112/#114）**：`with_received_at`、`with_registry_capability`、
  `registry_received_at`、`registry_capability`、`default_registry_capability`、`RegistryDataProvider` 六个名字
  全仓清零；帧读侧不再有 `as_of` / `receive_time` / `received_at` / `unwrap_or(query.end)`（桥把
  `query.end` 当"数据到达时间"哈希进血缘，是一条不可能失败的 PIT 断言）；`crates/qx-data/Cargo.toml`
  卸下只为这条桥存在的 `qx-provider` + `qx-guanxing` 依赖边。
- **`TestClock` / `advance_to` 删除（#116）**：README 的"回测不用系统时间"改指三处真闸门
  （入库前排序 + 同标的 `ts` 严格递增 + 帧读侧乱序报错），各配行为用例。

### Registered（§18-B：接不了的事实写进对外矩阵）

- **#115**：`event_verified` 不再是快照里手抄的一格 bool —— `crates/qx-cli/src/readiness.rs` 按本地
  `runs/*.json` 的 `RunManifest` 重算摘要并核对血缘与区间覆盖，摘要口径取自 qx-factor 的 `mark_event_verified`；
  三个研究快照读点都必经绑定入口。
- **#117**：调度装载入口按运行时真实能力拒形状（`unsupported_dispatch_shape`，写状态之前即拒并点名作业），
  一次 tick 先 `manifest.validate()` 再给 `JobRun` 盖血缘。
- **#118 / #119**：公司行为三条只读查询、因子物化写侧五个入口、`artifacts[].values` 全部进允许清单 +
  limitation（`corporate_action_read_side_has_no_production_reader`、
  `factor_snapshot_json_has_no_in_repo_producer`、`feature_artifact_values_have_no_production_reader`），
  接口文档逐层对齐五层快照字段并声明"由仓库外导出、拼错的键会被拒"。

### Added（门禁，397 → **449**）

- 九颗新判据函数共 51 条：`reconcile_action_and_absence_check` 8、`lease_clock_domain_check` 11、
  `bar_frame_pit_honesty_check` 4、`event_backtest_evidence_check` 5、`backtest_clock_honesty_check` 5、
  `scheduler_dispatch_honesty_check` 5、`corporate_action_read_side_check` 2、
  `factor_research_honesty_check` 7、`scheduler_retry_honesty_check` 4。
- `wheel_builder_check` 第 4 条（#132）：README 里每条 ASCII 的 PowerShell 入口都必须带
  `-ExecutionPolicy Bypass`，一条入口都没有也红。
- **#87 收口**：能力矩阵证据路径判据从"只认行首唯一路径"改成**整行每个 repo 前缀 token 都核对**
  （带 `:行号` 的剥后缀、glob 不强判）—— §16 把四份用例集拆目录后，五条指向已消失单文件的证据行
  在旧口径下继续全绿。
- `GATE_CHECK_FLOOR = 449`、`WORKSPACE_TEST_FLOOR = 851`（都是本轮实测值）。
- 两条正则口径修正：数生产调用点时接收者前缀 `.`/`::` 已是分隔符，再加 `(?<!\w)` 会让判据恒绿
  （#119 的 t3 首版就是这样不咬）；`SCHEDULER_FINISH_CALL` 之类紧化断言不能带尾参（rustfmt 保留尾逗号）。

### Docs

- `deploy/README.md`：一次作业运行的收口口径（不自动重跑、结果码不进 `JobRun`、超时如何升级）、
  调度派发只认 Cron + `window=Any`、研究快照五层字段与"仓库外导出"的契约。
- `README.md`：wheel 入口与三条前置（pip 探测、ASCII 行尾、执行策略）；`maturity/capabilities.yaml`
  新增 3 条 limitation + 8 条证据行。

### Verified（本回合实测，逐条可 grep）

| 门槛 | 结果 | 日志 |
|---|---|---|
| 架构门禁 | `GATE_EXIT=0`，**449 项全绿 / 0 条 FAIL**；爬升 405→425→430→435→444→448→449 | `s20r_gate_after_docs.txt`（README 台账与 §18.6 回写后复跑） |
| 整树测试 | `TEST_EXIT=0`，**92 段全 ok / 843 passed / 0 failed / 0 ignored**（§16.7 归零后的 821 → 843；逐名归账 = 新增 24 颗、删除 2 颗，删掉的两颗分别跟着 #116 的 `TestClock` 与 #112/#114 的三条假桥走） | `s20q_test_workspace.txt`（clippy 修复后重跑；修复前的 `s20a` 同数，中间还有一次因 `QX_PYTHON` 被我写成目录路径而作废的 `s20p`） |
| 全仓 `#[test]` 属性 | **851**（829 → 851，与整树同 +22；差的 8 条被 `nats`/`sqlite` feature 门控） | 门禁的 `workspace_test_floor_check`（同一份 `s20r_gate_after_docs.txt`） |
| 存储（sqlite feature） | `EXIT=0`，**56 passed / 0 failed** | `s20d_storage_sqlite.txt` |
| Clippy | **先 101 后 0**：`crates/qx-cli/src/tests/scheduler_dispatch_support.rs:92` 的 `&[job.clone()]`（`cloned_ref_to_slice_refs`）改 `std::slice::from_ref` | `s20b_clippy.txt`、`s20c_clippy2.txt` |
| 格式 | `cargo fmt --all -- --check` 退出码 0 | `s20f_fmt.txt` |
| Release 构建 | `cargo build --release --offline` `REL_EXIT=0` | `s20g_chain.txt` |
| Python 边界 / 核心语义 | `Ran 49 tests … OK (skipped=1)`；`全部自校验通过 ✓` | `s20e_pytests.txt`、`s20e_validate.txt` |
| 安装包（两条入口） | `SH_EXIT=0`（`sha256=4ef59cf7…`）、`PS_EXIT=0`（`sha256=c352502f…`，带 Bypass） | `s20g_chain.txt`、`s20i_wheel_ps1_bypass.log` |
| 安装包 = 当轮代码 | wheel 216440 字节；内嵌 `_qianxing_native.pyd` md5 `372906ad…` **等于**本轮 `target/release/_qianxing_native.dll`；12 份 `*.py` 与 `python/` 逐文件相等 `mismatch: none` | `s20j_wheel_verify.txt` |
| 装进干净 venv | `native.available() → True`；三字段 `cross/hedge/3` 往返 `True`；`margin_mode="weird"` 仍被拒 | `s20k_wheel_install_probe.txt` |
| 变异反向验证 | **70 颗**（结构 64 + 行为 6）逐颗红在预期判据、按字节还原后全绿 | §18.4 的表 + `s20h/s20l/s20m/s20n` 四份补跑日志 |

### 未收口（如实记录）

1. `sandbox_tested` 全部保持 `false`：本轮没有任何外部服务与凭据参与。
2. "每遍都要重跑 `build.bat` 全 8 步"仍无量具 —— 本轮那颗 clippy 命中正是第 4 步补跑才红的。
3. 门禁总数地板与用例地板只咬下跌侧，449 / 851 仍是人工回合的产物。
4. #106 / #107 待确认后关单（R4-d 的 `zero_reference_public_surface_check` 已实现同一口径）；
   #53（Q1b 可复现性绑定）未排期；`build.sh` 可执行位与 wheel 脚本的 `--offline` 口径未动。
5. 本地 `main` 与 `origin/main` 分叉未解：本轮成果全在工作树，未 stage、未 commit、未 push。


## Unreleased — V12 §17：第四遍清点构建与安装面，判据可以整段消失而门禁全绿（2026-09-25）

前三遍数的是产品代码，这一遍数的是"用户怎么把这个仓库跑起来"。结果是 `build.bat` 三处互相独立的缺陷
同时在场：双击必然中途死在 `\'不是内部或外部命令\'`、解释器探测只认退出码所以把 WindowsApps 的 `python`
存根当成可用解释器、步骤口径与 `build.sh` 已经分叉。收口时又撞到第四类：README 承诺的两条 wheel 入口
**两条都不通**（`.ps1` 的中文行尾被 PowerShell 5.1 按 ANSI 码页吞掉换行，紧跟其后的 pip 探测整行被并进注释、从不执行；`build.sh`
用的 `python/.venv` 是 uv 建的、里面没有 pip），而 `dist/` 里那份 wheel 内嵌的原生扩展是六天前的。
全过程与十一颗变异记录见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §17，日志在 `%TEMP%/qx_v12p2/logs/`。
本遍被改动的产品代码只有一颗，而且是收口重跑 8 步时才红的：`[4/8]` clippy
（`crates/qx-genglu/src/reconcile/tests.rs:43` 两处 `cloned_ref_to_slice_refs`，V12 §17.6）；
其余改动全在构建脚本、`.gitattributes`、门禁与文档。收获是一条量具盲区：
**门禁自己的判据可以被整段删掉而它照样打印"全部通过 ✓"**。

### Fixed（构建与安装面）

- **`build.bat` 重写**：`chcp 65001` 只修显示、不修解析，带 UTF-8 中文的 LF 版批处理 cmd.exe 读不了，
  于是强制 CRLF 并保证每行以 ASCII 字节结尾；第 `[0/8]` 步的探测改为要求候选**把版本号印出来**
  （退出码不算证据：Store 存根与 `cmd.exe` 都能以 0 退出而不打印任何 Python 版本），随后再探
  `import tzdata`（`python/pyproject.toml` 里声明的 Windows 依赖）。候选顺序 `QX_PYTHON` →
  `python\.venv\Scripts\python.exe` → PATH `python`，三项都不合格时退出码 1 并印出候选清单与两条修法。
- **`build.sh` 同口径重写**：同一组 8 步、同一串 7 条 `cargo` 命令、同一个探测契约（`QX_PYTHON` →
  `python/.venv/bin/python` → `python`）。
- **新增 `.gitattributes`**：`*.bat`/`*.cmd` 钉 `eol=crlf`、`*.sh` 钉 `eol=lf`。在此之前 CRLF 只是本机的
  巧合（`core.autocrlf=true`），换一台机器或换一种 checkout 方式就会把可运行的 `build.bat` 变成跑不了的。
- **仓库 venv 离线重建**（`uv venv .venv --python 3.12 --clear` + `uv pip install tzdata`，3.12.13 /
  tzdata 2026.4）：它曾在并发 uv 进程干扰下消失，导致 `[5/8]` 的 A 股用例以 `ZoneInfoNotFoundError` 失败。
  重建后 Python 边界测试从 `Ran 34 … FAILED(errors=1)` 变成 **`Ran 49 tests … OK (skipped=1)`** ——
  `test_ashare.py` 的 16 条原本整文件不跑（导入期 `ZoneInfoNotFoundError`），只留 1 条 loader 占位用例，
  所以差值是 15（`s17_py_no_tzdata.log`，本回合用缺 tzdata 的解释器复现）。
- **`[4/8]` clippy 补跑抓到的一颗 lint**：`crates/qx-genglu/src/reconcile/tests.rs:43` 用
  `&[local.clone()]` 造单元素切片，`-D warnings` 下 `CLIPPY_EXIT=101`（`cargo test`、架构门禁、
  release 构建全都不会红）。改法即 clippy 建议的 `std::slice::from_ref(&local)`，行为不变。
  这颗在 §16 那一遍改这个文件时就该红 —— 那一遍没跑第 4 步，也没写下"没跑"（V12 §17.6）。
- **两条 wheel 构建入口修通**：`tools/build_python_wheel.ps1` 与 `.sh` 都在跑 `cargo` 之前先探测
  `-m pip --version`，缺 pip 即以退出码 1 停下并印两条修法（装 pip / 离线 `uv build --no-build-isolation`）；
  `.ps1` 的注释与抛出消息改为 ASCII-only（中文行尾会被 PowerShell 5.1 按 ANSI 码页解码、吞掉换行，
  让下一行代码整行消失），并把被 dedent 出 `try` 块的四段缩进归位。两条入口修好后**各自真跑通一次**
  （`PS_EXIT=0` / `SH_EXIT=0`，`s17_wheel_ps1_2.log`、`s17_wheel_sh.log`）。
- **安装包按当轮代码重建**：旧 wheel 先 `cp -p` 镜像到 `%TEMP%`，重跑 `cargo build -p qx-python --release`
  → 新的 `_qianxing_native.dll`（md5 `c1991c58…`）→ 打包 → `dist/qianxing_bridge-0.1.0-cp312-cp312-win_amd64.whl`
  （216440 字节）。否证与复核都按 md5 而不是"文件存在"：旧 wheel 内嵌的是 `0cb3981c…`（六天前的扩展），
  新 wheel 内嵌的 `.pyd` 与本轮 `.dll` 同 md5，wheel 里 12 份 Python 源逐文件与仓库相等
  （`s17_wheel_verify.txt`）。`/dist/` 在 `.gitignore` 里，所以安装包是**本地产物**、不随提交分发。

### Added（门禁，387 → **397**）

- `windows_batch_parse_check`（4 条 + 1 条属性判据）：逐个 `.bat`/`.cmd` 核对无孤立 LF、每行以 ASCII 结尾，
  并**按实际存在的扩展名**各自要求一条 `eol=crlf` 属性规则（早先写法允许 `*.cmd` 替 `*.bat` 交差，被变异 M4 证伪后收紧）。
- `build_script_parity_check`（2 条）：两份构建脚本的 `echo [n/N]` 步骤逐项同名同序（8 步），
  `cargo` 命令多重集相等（7 条）。
- `wheel_builder_check`（3 条）：两个 wheel 脚本都要在 `-m pip wheel` **之前**探测 `-m pip --version`
  （按字符位置比较，缺一或顺序颠倒即红），且 `.ps1` 每一行以 ASCII 字节结尾 —— 这条正是本轮那两处
  安装面失效各自的判据。
- `GATE_CHECK_FLOOR = 397` 门禁自身总数地板：本轮两次手滑删掉 `TEST_MODULES` 的元组项时，
  门禁静默少跑 4 条判据却仍然报"全部通过 ✓" —— 现在少一条就红。

### Docs

- `README.md`：构建段从"Windows 下可直接双击 `build.bat`"扩成解释器契约 + 两份脚本同口径 + `.gitattributes`
  的因果；"当前状态"按本轮重测回写（387 → **397 项**、Python 边界 47 → **49 条**、
  `qx-storage --features sqlite` 9 段 51 → **10 段 56 passed**、venv 已重建但探测不因此取消）；文档指向段补 §17。
- V12 §17：三处缺陷的四象限否证表、修到什么程度、venv 重建前后对比、10 条判据的逐颗变异反向验证、
  本轮测量表、§17.7 未收口、§17.8 安装包重建（两条入口的失败原因表 + 修后跑通与 md5 复核）。

### Verified（本轮日志，非旧文档摘录）

| 门槛 | 本轮结果 |
|---|---|
| 架构门禁 | 退出码 0，**397 项全绿**（`s17_gate_397.txt` 抬地板后首跑；本轮文档回写后再跑一次仍 0 / 397、无 `FAIL` 行） |
| 整树测试 | `TEST_EXIT=0`，90 段全 ok / **821 passed** / 0 failed（`s17_full_test_225330.log`；lint 修好后复跑 `s17_full_test_230131.log` 同数） |
| fmt / release / clippy | `FMT_EXIT=0`、`REL_EXIT=0`（`s17_final_225640.log`）；`CLIPPY_EXIT` 从 101 → **0**（`s17_clippy.log`、`s17_clippy2.log`） |
| `qx-storage --features sqlite` | 退出码 0，10 段 / **56 passed** / 0 failed（`s17_storage_sqlite.log`） |
| Python 边界 / 核心语义 | `Ran 49 tests … OK (skipped=1)`、`全部自校验通过 ✓`，均退出码 0 |
| wheel 两条入口 | `PS_EXIT=0`（sha256 `8c6906c9…`）、`SH_EXIT=0`（sha256 `f58d2a62…`），同尺寸 216440；安装到干净 venv 后 `native.available()` 为 `True`，三字段往返相等 |
| 反向验证 | 十一颗变异（M1–M7 + M8/M8b/M9/M10）各自**只红一条**，`cp -p` 还原后逐字节 `identical=True` |
| 解释器回落 | 5 个 `QX_PYTHON` 场景 × `build.bat`/`build.sh` 两侧，全部按预期成功或 fail closed |

**仍不宣称**：`sandbox_tested` 依旧全为 `false`（本轮没有任何外部服务或凭据参与）；门禁总数地板只咬下跌侧；
wheel 脚本里的 `cargo build -p qx-python` 不带 `--offline`，且 wheel 文件名不带世代（V12 §17.7 第 5、6 条）。


## Unreleased — V12 §16：三遍连通性清点在合流后的代码上重跑，量具的第三类瞎法被点名（2026-09-25）

按"至少三遍、每遍全修再进下一遍"的口径，三遍分别在合流之后的工作树上重跑：第一遍找孤儿逻辑，
第二遍找无退出条件的循环，第三遍找前后端/三语言契约断链。全过程与逐颗反向验证记录见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §16。
本轮产品侧只修 4 个点，另加 6 条门禁判据 —— 收获是识别出**契约判据只覆盖三语言里的两种**这类失效：
它不会误报，也不会红，只会让第三种语言静默漂移。

### Fixed（三遍各一颗到两颗）

- **健康快照的 stale 判定不再恒不成立**（§16.1 #122）：`runtime-check` 调
  `HealthRegistry::snapshot(now_ms, stale_after_ms)` 时把 `now_ms` 传成 `0`、把预算传成
  `shutdown_timeout_ms`，饱和减法使"心跳过期"在任何配置下都不成立。现在传
  `runtime_timestamp_ms()` 与 `messaging.worker_stale_after_ms` —— "心跳多久算陈旧"全仓库只剩这一个窗口。
  同轮把 `runtime_check.rs` 里两处重复的报告数组读取抽成 `string_array()`（缺字段/非数组按空处理），
  以付该文件 500 行的预算账，落 498 行。
- **API 两条 accept 循环接上停机出口**（§16.2）：明文 `serve` 与 mTLS `serve_tls_mtls_with_stores`
  原先都阻塞在 `incoming()` 上，监督器的 `ShutdownToken` 递不进去 —— 没有连接就一直等。两条循环现在
  共用 `await_connection`（先 `stopped()` 判定，再 `set_nonblocking(false)` 取连接），并由 3 条用例
  （`crates/qx-api/tests/accept_loop_shutdown.rs`）+ 6 条判据按名字钉住。`SchedulerWorker` 与策略宿主
  循环也改为读同一个停机令牌。
- **Python SDK 的多腿衍生品意图补回三个字段**（§16.3 #123）：Rust `StrategyContractIntent` 与
  `schemas/strategy_api_v1.schema.json` 都认 `margin_mode` / `position_mode` / `leverage`，
  `python/qianxing_bridge/strategy.py` 的 `StrategyIntent` 没有这三格 —— Python 策略作者写不出这三字段，
  而带这三键的输入会被 Rust 的 `deny_unknown_fields` 整体拒绝。声明、校验、序列化、反序列化四段一并补齐，
  校验档位与 `contract.rs` 同口径（`cash|cross|isolated`、`one_way|hedge`、`leverage > 0`）。
- **outbox 的 `schema_version` 回到常量**（#120）、**`run_manifest.input_components` 有人写也有人读**（#121）。

### Added（门禁）

- `health_snapshot_knob_check`：读 `collect_runtime_check_report` 的**函数体**，要求真时钟与
  `worker_stale_after_ms` 在场、`snapshot(0` 与 `shutdown_timeout_ms` 不在场（只读代码，不接受注释）。
- `strategy_intent_three_language_check`：三侧字段集**两两逐项相等**（Python SDK↔Rust、JSON schema↔Rust）
  + 三条衍生品字段在位 + 两条跨语言往返用例点名钉住。这是本仓库第一条"schema 文件真的被门禁读过"的判据。
- `CLI_TEST_FLOOR` 243 → **244**（磁盘重测：`src/tests` 192 + `crates/qx-cli/tests` 递归 52）。
- `workspace_test_floor_check` / `WORKSPACE_TEST_FLOOR = 829`：递归数 `crates/*/src` 与 `crates/*/tests`
  下的 `#[test]`，全仓总数跌破即红，失败信息按 crate 印出分布。这条是本轮账目回合的直接产物 ——
  两个 crate 的地板护得住那两个 crate，护不住另外二十个（见下"Reconciled"）。

### Reconciled（整树 passed 从 §15 的 843 掉到 821，逐名归零后才写进文档）

用例只增不减是这批工作的自述，所以一次下降必须被解释。把两份日志的 `test <name> ... ok` 做集合差：
本轮少 39 个名字、多 17 个名字。39 个里 15 个住在本批整份删除的文件里（`qx-core/src/{engine,queue,fenye}.rs`、
`qx-data/src/cache.rs`、`qx-genglu/src/reconcile/account.rs`、`qx-zhenlu/src/portfolio/optimizer.rs`，
以及被目录模块接管的 3 份用例单文件，`git status` 里全是 `D`）；其余 24 个逐条判过，结论都是
**被测代码本身也不在了**或**同批改名且新名在跑**：`RestVenue`/`RequestSigner` 脚手架那 7 条（模块头就写明随装配读者一起删）、
`crossed_quote`/`vector_scan`/`signed_transport`/`hmac_signer`/`QuoteBook` 在整棵 `crates/` grep 为零、
`drawdown_computed`/`total_return_signed`/`attribution_keeps_strategy_chain` 是 V10 §4.8 的重复概念归位、
5 条旧对账用例由同文件的 canonical 裁决用例承担、重试分类改由 `qx-core/src/retry.rs` 的 3 条钉住。
**没有一条是"代码还活着而用例消失"**；全仓 `#[test]` 属性总数 `HEAD = 72c347e` 的 752 → 工作树 829。

### Docs

- `deploy/README.md`：新增"心跳新鲜度只有一个窗口"段（并明写 `runtime-check` 的 `health` 块是拓扑快照、
  全部 `starting`，运行期健康以 `/ready`、`/metrics` 为准）；新增 `## 自检与内部命令` 段覆盖
  `ecosystem` / `paper` / `verify` / `all` 四条此前无文档的命令，**每条文案都用
  `./target/debug/qx-cli.exe <cmd>` 实跑否证或证实过**（第一版把 `ecosystem` 写成覆盖 PaperVenue，
  实测 `grep -c PaperVenue` 得 0，已改为实测口径；`all` 改为"`verify` 与 `paper` 的并集"）。
- `maturity/line_budgets.yaml` 按 `--snapshot` 重算：37 项，46395 → **44120（−2275）**，20 项下降共 −2208。
  两处增长逐条判过并留结论：`crates/qx-cli/src/strategy_contract.rs` 822 → 840（R4-k 与 #123 的三侧口径
  都落在这里）、`crates/qx-storage/src/lib.rs` 2405 → 2406（#120 的常量绑定 1 行）。形状变化 4 项：跌出
  `qx-factor/src/materializer.rs`（507）与 `qx-plugin/src/lib.rs`（583），新进 `qx-cli/src/worker_entry.rs`（505）
  与 `qx-cli/src/tests/api_snapshot_money_fields.rs`（518）。

### Verified（本回合实测，逐条可 grep）

| 门槛 | 命令 | 本轮结果 |
|---|---|---|
| 架构门禁 | `tools/check_architecture.py` | `ARCH_EXIT=0`、**387 项全绿**；6 条新判据 `[PASS]` 文案逐字点名在 §16.5 |
| 全仓用例地板 | 同上 | `[PASS] 全仓行为用例不少于 829 条`；变异：抽掉 `qx-guanxing/src/lib.rs` 一条 `#[test]` → **只红这一条**（`当前 828 条 … qx-guanxing=7`），`cp -p` 还原 `RESTORED-IDENTICAL` 后回到 387 项全绿（`%TEMP%/qx_v12p2/logs/arch_mut1.log`、`arch_restored.log`） |
| 整树测试（带解释器） | `QX_PYTHON=<解释器> cargo test --offline --workspace` | `exit=0`、**90 段 / 821 passed / 0 failed / 0 ignored** —— 两次抓到同一组数：`final_r3_205909.log`（venv 解释器）与文档回写后的 `final_s16_docs.log`（基础解释器），均在 `%TEMP%/qx_v12p2/logs/` |
| Python 契约 | `cd python && PYTHONPATH=. .venv/Scripts/python.exe -m unittest discover -s tests -p "test_strategy_contract.py"` | `Ran 10 tests … OK`（含本轮新增 2 条跨语言往返用例） |
| fmt / check | `cargo fmt --all -- --check` / `cargo check --offline -p qx-cli --all-targets` | 退出码 0 / 无 warning |
| 命令面 | `./target/debug/qx-cli.exe ecosystem` / `paper` / `verify` / `all` | 四条均退出码 0 |
| 能力矩阵 | `tools/check_architecture.py` 的 `未拿到外部沙盒记录前 sandbox_tested 全为 false` | `[PASS]` —— 本轮没有任何外部服务或凭据参与 |

上表的 Python 两行是同一回合抓到两次的：第一次用 `python/.venv/Scripts/python.exe`（386 项全绿 / `Ran 10 tests OK`），
第二次在文档回写之后。两次之间本机 `python/.venv/Scripts/python.exe` 被一个并发的 uv 进程删掉了（该目录 mtime 21:39，
同机可见的并发命令行属于另一份仓库的 `-m pytest tests/architecture`），所以复跑改用 uv 基础解释器
`%APPDATA%\uv\python\cpython-3.12.13-windows-x86_64-none\python.exe`，结果一致（387 项 / `Ran 10 tests … OK`）。
这条不是链路缺陷，但它证伪了 README 与 `build.bat` 里"`python/.venv` 恒可依赖"的假设 —— 本机 venv 的启动器
仍缺 `python.exe`，需要 `uv venv` 重建（见 V12 §16.5 表下的说明）。

### 未收口（如实记录，不写进"已达成"）

1. `RuntimeSupervisor::new()` 只 `register`（状态 `Starting`）、从不 `spawn`，所以修好的 stale 判定在 CLI
   自检路径上仍无生产触发者；真读者在 `qx-api` 的 `/ready`。本轮把口径写进文档，没有把 `health` 块伪装成运行期健康。
2. "地板等于磁盘值"这件事仍无判据，244 与 829 两个数仍是人工回合的产物（§15.6 第 5 条原样成立）。
   新增的全仓地板补的是**下跌侧**：删用例现在会红；写歪的数它不知道，它只知道不得低于 829。
3. 三遍清点不动公共 API，13 颗零消费者/无生产装配的断链发现（#106/#107/#108/#109/#110/#112/#114–#119）
   留给单独一轮。
4. `maturity/capabilities.yaml` 的 `sandbox_tested` 保持 `false`。


## Unreleased — V12 §15：上游 4 个提交合流到本地 V12 R4 工作树，冲突面全在口径而不是代码（2026-09-25）

`origin/main` 领先 4 个提交（`b4921ea` V11 R/S 两轮、`5cd11f8` T1–T4 常驻牙齿、`2bf8ad6` 与
`dbfc429` 两次把 `main` 合进 `p0-on-v10`），且本地 `HEAD = 72c347e` 是它的祖先 —— 提交层面是纯
fast-forward，未提交层面 61 个文件与本批 V12 R4 改动重叠，逐文件走三方合并（`git merge-file -p`，
输出留在 `%TEMP%/qx_v12r5_pull/merge2/`）。判定口径：**同一缺陷的双轨修复，编码单源化取上游、
结构取本地**。合流记录（7 颗变异、门禁量具自身被打坏的三处）见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §15。

### Fixed（口径以哪一份为准，这一轮逐点判过）

- **账户快照四张键表的编码整份交给 serde，取上游那份更彻底的实现**（V11 R14/R15 → 与本地 TX5 同点）：
  `json_table_entries` / `json_position_entries` / `instrument_key` 三个渲染点各一处，`to_json` 的写入段
  不再自己抄序列化；`side_code` / `order_status_code` 只服务 `state_hash`，JSON 通道与读侧折算层都碰不到它们。
  上游在同批带来的两条协议用例（`stable_json_tables_round_trip_through_the_reader`、
  `the_cross_language_sample_is_what_the_writer_emits`）搬进本地的目录模块新文件
  `crates/qx-protocol/tests/snapshot_single_source/stable_json_tables.rs`，与 `optional_money.rs`
  共用同一个 `position_row`（可见性改成 `pub(super)`，仓库那份夹具由它产出）。
- **上游其余单源化一并取入**：`schemas/account-snapshot-v1.json` 经 `include_str!` 成为契约正文的唯一来源、
  `INIT_PROFILES` 一张表、`default_account_event_log`（无键读模型说的是哪个账户只有一处答案）、
  `ApiQueryModels::with_query_models_provider`（运维读模型每请求现读）、`publish_snapshot_for`、
  对账两格 `Option` + `apply_reconcile_reports`（没有报告 = `null`，对过且无差异 = `Some(0)`）。
- **`doctor` 的拓扑那一项改名 `runtime_supervisor_build`**（上游）：`doctor` 从不启动 worker，
  旧的 `runtime_topology: pass` 会被读成"运行拓扑健康"。现在通过语是
  `监督器可构建，启用 worker=<N>（未启动，不代表运行健康）`，运行健康仍归 `runtime-check`。
- **门禁量具被合流打坏三处，本轮修好并逐颗证明它咬得住**（`tools/check_architecture.py`）：
  `snapshot_row_wire_check` 里那份合并残留引用了本函数根本不存在的变量、钉的是已被删掉的手写编码器，
  整段重写为"写入段自己不碰 serde、持仓换键只在编码器里发生一次"；它与上游新增的
  `snapshot_json_table_check` 的重复判据删掉（同一条纪律两处钉，漂移时只会红错的那一处）；
  把合法业务档位 `OrderStatus::Unknown` 误判成 serde 兜底变体的子判据删掉，改名或兜底仍由
  `#[serde(...)]`/`#[repr(...)]` 那两条属性判据守住。`snapshot_contract_version_check` 的 needle 按
  当前代码逐条重校准，并新增 `without_line_comments()` —— 注释里复述一遍判据字符串，不该让门禁变绿。
  `case_source()` 改为递归读目录模块，用例搬家不再改常量。CLI 用例地板按磁盘重测抬起：合流后
  `^#[test]$` 实测 **243** 条（`src/tests` 191 + `crates/qx-cli/tests` 递归 52），而地板停在合流前的
  历史值 210 —— 落后 33 条的地板等于没有防守（`qx-execution` 同轮重测 15 + 10 = 25，恰好仍是磁盘值）。
- **README 两处说假话当场改掉**（本轮实测否证）：`--fill-tier` 只有 `l1`/`l2` 两档，原文写的"l2/l3 走
  订单簿内核"里那一档旗标根本不存在；`reconcile` 无参数运行打印的是用法并以退出码 2 结束，原文把它写成
  了一条"本地对账契约 smoke"命令。`multi-builtin` 的产物口径同时补上：只有 `--root` 点名时写 1 份归因产物。
- **行数棘轮按合并后的真实形状重算**（`maturity/line_budgets.yaml --snapshot`）：39 项，
  `crates/qx-adapter/src/binance.rs` 2291 → 2221（上游重连预算与本地拆分的净效果），
  新登记两项越过 500 行的文件（`qx-cli/src/worker_entry.rs` 505、`qx-cli/src/tests/api_snapshot_money_fields.rs` 518），
  六处因合并下降的文件同步下调。

### Verified（本轮实测，逐条可在 `%TEMP%/qx_v12r5_merge/` 的日志里 grep 到）

| 门槛 | 命令 | 本轮结果 |
|---|---|---|
| 架构门禁 | `python/.venv/Scripts/python.exe tools/check_architecture.py` | `ARCH_EXIT=0`、**370 项全绿**（`arch5.log`；合并当场红 9 项 + 残留 2 项，见 §15.3） |
| 整树测试（带解释器） | `QX_PYTHON=python/.venv/Scripts/python.exe cargo test --offline --workspace` | `TEST_EXIT=0`、**88 段 / 843 passed / 0 failed**（`test5.log`） |
| 整树测试（不带 `QX_PYTHON`） | 同上，去掉 `QX_PYTHON` | `TEST_EXIT=101`、**2 条 Python 桥用例红**（`test2.log`，本机 PATH `python` 是 WindowsApps 占位桩 —— 环境门槛，不是链路缺陷） |
| SQLite 后端 | `cargo test --offline -p qx-storage --features sqlite` | `SQLITE_EXIT=0`、9 段 / 51 passed / 0 failed（`sqlite4.log`） |
| fmt / clippy | `cargo fmt --all -- --check` / `cargo clippy --offline --workspace --all-targets -- -D warnings` | 均退出码 0（`clippy2.log`，`CLIPPY_EXIT=0`） |
| Python 侧 | `tools/validate_core.py` / `python -m unittest discover -s python/tests -q` | 退出码 0 / `Ran 47 tests … OK (skipped=1)`（`validate4.log`、`pyunit4.log`） |
| 命令面 | `qx-cli help` / `qx-cli strategy list` | 52 行用法 / 去重 41 个入口名（documented = clap 40 ∪ `help` = dispatched）；17 个内置策略 |
| 反向验证 | 7 颗变异（写入段自抄 serde、`side_from_code` 复活、版本闸门 `!=`→`>`、header 声明洗成合法值、`Side` 加 `#[serde(rename_all)]`、夹具被篡改、编码器不剥大括号） | **7/7 红在预期的那一条判据或用例上**，`cp -p` 还原后 `diff` 逐字节相同（`mut_m*.log`） |



## Unreleased — V12 R4：三遍清点（孤儿逻辑 / 无退出条件 / 契约贯通），当场抓出两条"账户一下单就读不回来"的断链（2026-09-24）

发布条件三遍清点的落地批。**第一遍**查孤儿逻辑：公共面上"只有别处测试在调"的死入口、看起来在读其实
读的是清单外的值；**第二遍**查无退出条件与阻塞点：停机令牌只有消费者没有生产者、重连上限按累计次数计、
一条用户流根本没有预算；**第三遍**查前后端与契约贯通，并当场抓出三条客户端或存储**立刻会失败**的断链
（包络不满足本进程自己公布的 schema、契约正文有两份互相矛盾的手抄、带订单的快照写得出读不回）。
收口记录（44 颗变异逐条对账、本轮量具自身失效的三处）见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §14。

### Fixed（死入口删掉、等待有上限、契约与实现对齐）

- **三条零生产消费者的公共入口全部删除**（V12 §4.6 / R4-d）：`submit_order_via_gateway`、
  `submit_order_via_gateway_with_risk` 与第二条多腿编排入口（`MultiVenueSpreadExecutionService` +
  `VenueRouterMap`）—— 现在 `git grep` 这四个符号在 `*.rs` 里 0 命中。同时把这条纪律变成判据：
  受管入口族（`pub fn submit_order*`、`pub fn (load|save)_control_state`）的每个 `pub fn` 必须在
  **定义文件之外、且不在测试路径里**有引用（`dead_public_entry_check`）。D3 搬家之前这三条入口
  谁都看不见，搬家之后 `git grep` 才第一次暴露"只有测试在调"。
- **停机阶梯接上了生产者**（R4-a）：`ShutdownToken` 此前只有读它的人，`Ctrl+C`/`SIGTERM` 从不落到
  `RuntimeSupervisor::request_shutdown`，所以那条按预算汇合 worker 的阶梯永远等不到令牌。
  新增 `crates/qx-runtime/src/supervision/shutdown.rs`（80 行）：信号处理置进程级标志、阶梯把请求转发给
  supervisor、到 `shutdown_timeout_ms` 报 `StopTimedOut` 而不是无限等 —— 不假装能强杀线程。
- **两条用户流的重连预算改成按连续失败计**（R4-b/R4-c）：Binance 用户流原上限按**累计**次数计，
  一条长期健康的流只要累计满 10 次就永久放弃、失败一次也不清零；CCXT Pro `watch_orders` 会话
  **完全没有预算**（固定 `base_delay` 无限重连）。现在前者 `delivered > 0` 即清零、退避统一走
  `qx-core::retry`，后者抽出 `ccxt_stream_retry.rs::CcxtStreamReconnectBudget`
  （500ms 起 / 8s 封顶 / 10 次连续），放弃时给具名原因。
- **`run backtest <cfg> <outer>` 的外层位置参数不再被静默吞掉**（R4-e）：以前传了东西没人读，
  现在 `cli.rs` / `cli_args.rs` 当场拒（`backtest_subcommand_rejects_outer_positionals_it_never_reads`）。
- **`--json` 必须真的产出机器可读正文**（R4-f）：`config explain` / `config doctor` 等命令上那个旗标
  原先只是把人类文案换个地方印；现在三条命令各自有用例钉住"旗标兑现"，
  `config validate` 保留它那行人读 PASS 线（`cli_json_surface.rs` 3 条）。
- **两条事件读链的 `after` 语义统一成"严格大于"**（R4-g）：内存游标含 `after`、SQLite 不含，
  同一个请求两处给出不同条数；`qx-api` + `qx-storage` 现在同侧（`event_cursor_after_semantics.rs` 2 条，
  其中一条同时跑两条链、另一条钉住过期与超前游标的拒绝）。
- **账户快照包络真的满足本进程公布的 schema**（R4-h）：`/account/snapshot/envelope` 原先把 serde 派生
  形状塞进 `data`，而 `/schema/account-snapshot-v1` 公布的是 `to_json()` 线格式（无 `protocol`、
  无顶层 `schema_version`、键从数字变字符串）—— 客户端照第 7 号路由的 schema 校验，第一道门就过不去。
  现在包络复用唯一编码器，`data` 与扁平路由逐字段等值（`snapshot_envelope_contract.rs` 2 条）。
- **服务端不再公布手抄的第二份 schema**（A2）：`ACCOUNT_SNAPSHOT_JSON_SCHEMA` 改成
  `include_str!("../../../schemas/account-snapshot-v1.json")`，仓库那份成为唯一正文 —— 内嵌那份
  少了 `positions/orders/fills/transfers` 的 `additionalProperties` 与顶层 `additionalProperties:true`，
  而 `deploy/README.md` 一直宣称两边是同一份。
- **`RunManifest` 的指针有了生产读者**（R4-i）：`run_manifest.json` 兄弟路径、`data_fingerprint`、
  产物身份此前只写不读，等于声明了一条永远不会失败的复核。现在数据集指纹回落接**被复核过**的那一份、
  引擎自哈希不得冒充输入身份，`report` 真的按声明比对兄弟 manifest，缺失/被篡改都拒。
- **回测摘要落"这一轮按哪几个参数跑"**（R4-j）：`backtests/artifacts.rs` 写 `signal{knobs,declared_unused}`
  与下单数量格，两条 Bar 链各按自己的 `--config` 读，读者第一次能从产物里区分"没配"与"配了但不生效"。
- **`schemas/strategy_api_v1.schema.json` 与 Rust/Python 两份实现对齐**（R4-k）：`required` 名单按
  Rust 非 `#[serde(default)]` 字段逐项重排、`raw` 定点值只走 JSON 整数（原先放行十进制字符串）、
  补上 `margin_mode`/`position_mode`/`leverage` 三格、`additionalProperties:false` 对齐
  `deny_unknown_fields`；"大写枚举名过得了运行时但过不了 schema"这条差异写进 `description` 而不是靠
  改口径掩盖，Python 桥 `_reject_unknown_keys` 同步。
- **带订单的账户快照写得出也读得回**（交易 TX5，本轮唯一一条"账户一旦真下过单就坏"的缺陷）：
  `AccountSnapshot::to_json()` 是手写稳定编码器，而 `orders`/`fills`/`transfers` 按 `u64` 建键 ——
  键原样拼进 `{}` **产出的不是合法 JSON**；订单行的 `instrument` 落显示串、`side`/`status` 落数字码，
  而 `OrderSnapshot` 的派生反序列化认的是内核身份对象与枚举名，折算层根本不存在。合起来的后果是
  `SqliteSnapshotStore`、`FileSnapshotStore`、`GET /account/snapshot` 三条读法在"下过单"的快照上当场失败，
  而仓库原有往返用例只放 `cash_raw` 与 `positions`。写侧三张表的键一律
  `json_string(&id.to_string())`（`crates/qx-protocol/src/lib.rs:409/427/444`）；读侧新增
  `wire.rs::fold_stable_order_row`（`:244`）与**唯一一份**方向码/状态码表（`:201`–`:237`），
  未知码报 `ProtocolError::Invalid`，不兜底猜方向。

### Added（旋钮单源、用例、门禁判据）

- **#102：四个信号旋钮按 kind 收成一张表**（本轮最实质的口径修正）。`fast_window`/`slow_window`/
  `period`/`threshold_bps` 此前无条件接受配置，但 `macd` 一个都不读（写死 `MACD_WINDOWS (12,26,9)`、
  `MACD_REQUIRED_BARS 35`）、`rsi` 只读 `period` —— "改了参数没反应"是设计内的，**播报和产物却把它说成
  生效了**。现在 `BuiltinStrategyKind::signal_knobs()`（Macd 为 `&[]`）是唯一一张表，
  内核历史需求、运行时体检、`builtin-strategies` 的生效列、stdout 的 `knobs=… declared_unused=…`、
  摘要的 `signal{}` 块全部问它，四条"看起来在读"的 phantom 通道删除。
  **一条对 §6 原方案的偏离写清楚**：清单外的旋钮**不 fail-closed**，照常跑并列进 `declared_unused` ——
  这些键同时是历史配置项，拒跑会让既有 runtime 全部起不来；代价是"配了但不生效"只在播报与产物里可见。
- **TX5 用例三层**：协议层 `rows_keyed_by_integer_still_produce_and_recover_valid_json`
  （写出→读回→篡改 `side=9`/`status=99` 必须被拒）、存储层新增
  `crates/qx-storage/tests/snapshot_rows_persist.rs`（110 行，文件与 `--features sqlite` 两个后端
  各跑一次带 order/fill/transfer 行的密封快照，SQLite 那一条带 `#[cfg(feature = "sqlite")]`）、
  门禁层 `snapshot_row_wire_check` 6 项（键引号恰好 3 次、折算调用点恰好 1 次、码表在 crate 根缺席且
  在 `wire.rs` 单源、读侧不得出现 `unwrap_or`/`.or(Some` 兜底、两侧用例名在位）。
- **R4 用例新增/扩充**：`crates/qx-runtime/src/supervision/tests.rs`（停机阶梯 4 条 + 汇合工具）、
  `crates/qx-adapter/tests/user_stream_retry.rs`（4 条）、
  `crates/qx-cli/src/tests/ccxt_stream_retry_budget.rs`（2 条）、
  `crates/qx-api/tests/event_cursor_after_semantics.rs`（2 条）、
  `crates/qx-api/tests/snapshot_envelope_contract.rs`（2 条）、
  `crates/qx-cli/src/tests/cli_json_surface.rs`（3 条）、
  `crates/qx-cli/src/tests/backtest_input_provenance.rs`（基线 9 条）、
  `crates/qx-cli/src/tests/backtest_signal_provenance.rs`（5 条）、
  `crates/qx-strategy/tests/builtin_signal_knobs.rs`（4 条：清单外旋钮改不动一步、清单内每个旋钮仍改得动、
  Macd 无可调旋钮仍出信号、清单覆盖全部 kind）、
  `crates/qx-runtime/tests/strategy_contract_schema.rs`（新增 4 条）与
  `python/tests/test_strategy_contract.py`（新增 1 条，output 与 intent 两类未知键各一次断言）。
- **门禁 315 → 336 项**（+21）：`dead_public_entry_check`（1）、`snapshot_row_wire_check`（6）、
  `builtin_knob_list_check`（9），以及 A2/R4-i/R4-j 在既有函数里补的判据 —— 其中
  `snapshot_contract_version_check` 那条"常量与内嵌文本同号"改成"常量/仓库契约/Python 桥三处同号"，
  因为内嵌那份已经没了，旧判据会自证成空转。
- **行数棘轮在本轮的处理方式**：`crates/qx-protocol/src/lib.rs` 因 TX5 涨到 902 行 > 预算 831，
  做法是**把折算搬进 `wire.rs`**（198 → 277 行，低于 500 行登记门槛）而不是抬预算；
  `crates/qx-storage/src/sqlite.rs` 的落库用例同理（2579 → 2593 > 预算）改写成独立集成用例文件。

### 验收（`/tmp/qx_v12r4_close/`，2026-09-24 收口本轮台账后重跑）

- 门禁：`tools/check_architecture.py` `ARCH_EXIT=0`、`架构不变量自检全部通过 ✓（336 项）`
  （`arch.log`），含"能力矩阵证据路径全部存在""未拿到外部沙盒记录前 `sandbox_tested` 全为 false"
  "单文件行数预算只降不升"三条。
- 整树 `cargo test --offline --workspace`（带 `QX_PYTHON`）：`SUITES=84 / PASSED_TOTAL=807 /
  test result: FAILED 0 行 / WS_EXIT=0`（`workspace.log`），其中 `--bin qx-cli` 那一段
  `169 passed; 0 failed`。
- 被 `#[cfg(feature = "sqlite")]` 挡住的那一条必须单独跑：`cargo test --offline -p qx-storage --features sqlite`
  → `SQLITE_EXIT=0`，`tests/snapshot_rows_persist.rs` 段 `2 passed; 0 failed`
  （`storage-sqlite.log`）—— 不带 `--features sqlite` 时 rusqlite 后端整段被编译掉，"全绿"里根本没有它。
- **fmt 与 clippy 两道门槛是写台账这一轮才补跑的，且各抓到东西**：`cargo fmt --all --check` 先报 4 处
  rustfmt 漂移（`wire.rs:245/261/270` 与 `snapshot_rows_persist.rs:58`，全在 TX5 新写的代码里），
  按 rustfmt 期望手工落定后 `^Diff in` 计数为 0；`cargo clippy --offline --workspace --all-targets
  --features qx-storage/sqlite -- -D warnings` 抓到三处本轮新代码的 lint
  （`qx-strategy/src/builtin.rs:196` collapsible_if、`qx-strategy/tests/builtin_signal_knobs.rs` 三处
  manual_is_multiple_of、`qx-runtime/tests/strategy_contract_schema.rs` 两处 err_expect），
  改掉后 `CLIPPY_EXIT=0` 且 `CHECKED=22` 个 crate 全部在这次里被重查。
  改过的包逐个复测：qx-protocol 10+10、qx-storage(sqlite) 49、qx-strategy 18+4、
  strategy_contract_schema 4，门禁复跑仍 `336 项 / EXIT=0`（文本判据最怕重排版，实测未失效）。
  **教训与 §13.4 第 5 类同形**：R4 的收口只跑了门禁与整树测试，"用例红绿成对"证明的是判据咬得住，
  不证明代码过了 lint 或 fmt —— 三道门槛得各走一遍。
- 变异反向验证 44 颗（43 颗必须打红 + 1 颗正对照必须保持绿），颗颗在对应日志里 grep 得到"红"与"还原复绿"
  （§14.6 的表按项给出颗数与日志名；该表第一次写的合计是 43，与自己的列和不等，本轮按日志重数后改正）。
  本轮**另有三处失败红在量具而不是代码上**并一并留档：R4-e 的第一次测量 `gate.log` 里 `MUTATED_=0`
  （红绿探测没跑起来）、R4-f 的 `r4f.log` 里 M4FC `RESTORE_M4FC=identical` 却 `FAILED_CASES=3`
  （还原了源码没重建被测 binary）、TX5 的 T3 第一次测时门禁不红（判据只扫 `fold_stable_order_row`
  函数体，兜底落在 `side_from_code` 里）—— 三处修完才复跑成对红。
- **README 的"当前状态"三处数字按本轮实测改正**（细节见 V12 §14.9 末条）：门禁 315 → **336 项**
  （`qx_v12r4_readme/arch.log`、`arch2.log` 两次都是 `ARCH_EXIT=0`）；整树 58 段 / 768 passed →
  **84 段 / 807 passed / 0 failed**（`qx_v12r4_close/workspace2.log`，`WS2_EXIT=0`），并补一句
  "`--features sqlite` 那一段不在整树里、必须另跑"；`help` 的用法行按门禁自己的两个口径重数成
  **52 行 / 去重 41 个入口名**。链路表四行同步补上本轮能力：回测行加 #102 旋钮单源与 R4-i 的
  manifest 生产读者，Paper 行加 TX5 的"带订单写得出也读得回"，实盘行加 R4-b/R4-c 两条重连预算，
  运行时行加 R4-a 停机生产者与 R4-h/A2/R4-g 的契约贯通；四条对应 limitation 一并进表。
- 本轮**没有**接任何外部服务或凭据，`sandbox_tested` 全部维持 `false`：停机阶梯、两条重连预算与
  契约贯通只在进程内与本机后端上证过。


## Unreleased — V12 R1+R2+R3+D3：读侧不许把缺席念成数、契约版本真的认版本、一份 runtime 一个本金口径（2026-09-24）

V12 §4 前四条 P0 的落地轮。前三轮（Q67/Q68/Q70/Q72）把"没算过的钱"从**写侧**赶了出去，
本轮管的是另外三件同类的事：**读侧**仍然能把缺席念成一个合法值、账户快照的"版本校验"其实不会失败、
同一份 runtime 里可以并存两个互不相等的账户本金而没人说一句话。
收口记录（含 20 颗变异逐条对账、门禁量具本轮修掉的六类静默失效）见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §13。

### Fixed（缺席就是缺席，版本必须会失败，本金只有一份）

- **报告与 `status` 的读侧换成 `Option` 语义**（V12 §4.1 / R1）：`report` 与 `[Latest Backtest]`
  原先用 `pointer(...).unwrap_or(0)` / `unwrap_or_default()` 渲染缺键，把 v1/v2 世代的产物念成
  `fills=0 return_bps=0 final_equity_raw=0`。现在排版集中在唯一一处
  `crates/qx-cli/src/report_readout.rs:91`（`report_readout_lines`）与 `:135`
  （`latest_backtest_readout_lines`），缺键一律印 `absent`，而**声明过的 0 仍印 0** ——
  两者必须在同一条断言里成对出现才算区分了"没数"与"数是零"。
  多腿 `cost_bps` 在 `turnover_raw == 0` 时报"算不出"（没有分母），`i64` 越界不再夹到 `i64::MAX`。
- **`report` 先报产物自己的世代**（`report_readout.rs:63` `summary_generation_note`）：
  `input` / `account` / `replay` 三块分别说清是"那一代还没这个块"（`not_declared_before_vN`）
  还是"本世代缺这块"（`MISSING_IN_THIS_GENERATION`），不再用同一句"没声明"糊住两种事实。
- **两个排队入口补做 A 股段闸门**（V12 §4.20 的漏项）：`api_service.rs`（`serve` 受理 Trading 命令）
  与 `workers.rs`（`strategy-worker` 入队）原先只在提交路径上问过 §4.20 的检查，排队路径放行 A 股段
  —— 订单形状在入队前就定死了，等到提交时再拒已经晚了。现在两处都调
  `reject_ashare_rules_on_submit_path`。
- **账户快照的"版本校验"现在真的会失败**（V12 §4.2 / R2）：`qx-protocol` 引入
  `ACCOUNT_SNAPSHOT_SCHEMA_VERSION: u32 = 1`（`lib.rs:53`，与内嵌契约、仓库契约文件、Python 桥四处同号），
  结构体入口（`validate()`，`lib.rs:207`）与字节入口（`from_json`，`lib.rs:496`）各有一道常量闸门。
  改之前只看"顶层与 header 自不自洽"，所以一份两边都写 7 的快照进得来。
  **§6 原文"缺 header 版本就拒"这一支被实测否证**：写侧本来就不在 header 里抄第二份版本号
  （契约的 header `required` 里没有它，`to_json` 与 `FileSnapshotStore` 也不写），拒缺席等于让自家存储读不回来 ——
  现口径是"缺席按顶层归一，声明了却读不出整数即拒"，边界钉在
  `header_without_version_is_the_stored_shape_and_still_round_trips`。
- **一份 runtime 只有一个账户本金口径**（V12 §4.4 / R3）：`strategy.initial_cash_raw`（回测）与
  `worker.paper_initial_cash_raw`（paper）此前互不知情，使用者可以回测按 A、paper 按 B 再拿两边数字互相印证。
  现在 `reject_split_account_principal()`（`backtests/account_base.rs:96`）在回测出口
  （`account_base_from_config`，`:117`）与**两个 Paper 入账入口**（`venue_runtime/paper_submit.rs:50`、
  `venue_runtime/paper_worker.rs:103`）三处都先拒，报错并列点出两侧名字与各自的数
  （`strategy.initial_cash_raw=… vs worker[<id>]=…`）；两处等值时来源才改口成第四格
  `strategy-initial-cash+paper-worker-cash`（`BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE`，`:21`），
  孤立声明不许冒充"两处一致"，Paper 侧等值时另印一行 `[Paper · Account] 账户本金两处声明一致`。

### Added（用例、门禁判据、棘轮范围）

- **读侧用例 11 条**：`crates/qx-cli/tests/report_readout_honesty.rs`（5 条真子进程：v1 产物缺键印
  `absent`、v4 声明过的 0 印 0、`status` 与 `report` 同一份排版、无产物要说没有产物、
  复核结论 `input_verified=` 单列一格）+ `src/tests/report_readout.rs`（6 条排版单元）。
- **本金单源用例 4 条**（`src/tests/account_principal_source.rs`）与命令行侧 2 条
  （`crates/qx-cli/tests/backtest_account_base.rs`）；排队入口的 A 股闸门用例并进
  `src/tests/ashare_submit_guard.rs`（现 7 条）。
- **契约版本用例 5 条**（`crates/qx-protocol/src/tests.rs`，从 `lib.rs` 末尾搬家）：自洽高版本被拒、
  假版本号（`"1"`/`true`/`null`/`0`/`-1`/`1.0`）被拒、缺席仍是存储常态、常量与契约同号、
  **未来版本且本构建读不出的文档也必须先被版本闸门叫住**（最后一条把字节入口那道闸门的独有职责钉住）。
- **门禁 293 → 315 项**（`tools/check_architecture.py` +435 行）：新增三个判据函数
  `report_readout_honesty_check()`、`snapshot_contract_version_check()`、
  `account_principal_single_source_check()`。每条新判据都配了一颗"摘掉它"的变异：
  M1–M8 与 M10–M14 静态红且行为红，M9/M15–M20 注入的是测试代码/路径/行数本身，故 static-only。
- **证据路径判据从"整行只有一个路径"收紧成"行首第一个 token 是路径就要存在"**：旧口径从不核对
  "路径 + 一句说明"这类最好写的证据行，于是 D3 搬家后 7 处死路径留在 `maturity/capabilities.yaml` 里
  而 315 项全绿。M20 专门注入这一类行，轮 7 实测 `ARCH_FAIL_LINES=1 / WANT_HITS=1 / OTHER_FAILS=0`。
  台账同步补齐：R1/R2/R3 共 14 条证据行、撤销一条已被 R3 闭合的 limitation
  （`backtest_initial_cash_and_paper_worker_cash_never_meet`）、新增 #82 那条未跟踪产物限制。
- **500 行棘轮扩到 `crates/*/tests/**/*.rs`（D3）并当场把 4 个超线文件搬家**：
  `multi_leg_attribution` 792 → 3 文件（最长 388）、`ledger` 1031 → 4 文件（最长 446）、
  `venue_report_contract` 943 → 3 文件（最长 467）、`snapshot_single_source` 669 → 3 文件（最长 333）；
  扩口径后登记表里 `crates/*/tests/` 条目为 0。`workers.rs` 里与 worker 循环无关的"闭合 Bar 指纹"
  那一簇搬进 `src/strategy_live_state.rs`（137 行）。用例计数判据改为按**目录拼接**取数
  （`case_source()`），搬家不再让任何一条"某个用例必须在位"的判据失效。
- **`CLI_TEST_FLOOR` 191 → 208 → 210**、`DISK_CLI_TESTS=210`、`DISK_EXEC_TESTS=25`。

### 验收（`/tmp/qx_v12_gate7.log`，2026-09-24 01:46:20–01:53:07 +08:00；567 行）

轮 6（01:16–01:23）是修好量具后的第一次有效测量，本轮把台账、地板与证据路径判据都改完之后
用**同一套脚本重跑一轮**作为收口口径 —— 下面每个数字都在这份日志里 grep 得到。

- 前置门槛：`cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`、`cargo fmt --all --check`
  `STAGE3_FMT_EXIT=0`、`clippy -D warnings` 四家 `WARNLINES=0`、20 颗变异锚点 `PREFLIGHT_BAD=0`。
- 变异反向验证：`grep -c 'MUT_STATE' = 20`，且 20 行状态全是 `= red`（`green`/`unexpected` 各 0），
  每颗 `ARCH_FAIL_LINES=1 / OTHER_FAILS=0`；`RESTORE_EXACT` 20 行、`RESTORE_DRIFT` 0 行、
  `RESIDUE_FILES=0 none`、`RESIDUE_MUTATIONS=none`、`PRISTINE_OK final`。
  行为口径 13 颗全红（M1/M2 各 `6 passed; 1 failed`，M3 `4;2`，M4 两口径 `4;1` 与 `5;1`，
  M5/M6 各 `8;1`，M7 `7;2`，M8 两段 `2;7` 与 `8;2`，M10 `6;1`，M11/M12 各 `3;1`+`6;1`，
  M13 `3;1`，M14 `2;2`）；其余 7 颗（M9、M15–M20）注入的是测试代码/路径/行数本身，static-only。
- 整树（带 `QX_PYTHON`）：`WS_SUITES=58 / WS_OK_SUITES=58 / WS_PASSED_TOTAL=768 / WS_FAILED_TOTAL=0`、
  `STAGE8_WS_EXIT=0`；整树（不带 `QX_PYTHON`）：`WS_NOPY_SUITES=78 / WS_NOPY_OK_SUITES=77 /
  WS_NOPY_FAILED_TOTAL=2`（`STAGE8_WS_NOPY_EXIT=101`），两条失败点名
  `rust_invokes_python_multi_intent_strategy_contract` 与 `…strategy_jsonl_worker_through_versioned_contract`
  （同一段 `153 passed; 2 failed`）—— §4.19 那条"Python 桥用例需要解释器"从断言变成实测。
- 门禁项数 `BASELINE_ARCH=315 → FINAL_ARCH_TOTAL=315`（`STAGE9_ARCH_EXIT=0`）；
  `DISK_CLI_TESTS=210`、`DISK_EXEC_TESTS=25`。
- 静态 `#[test]` 776 条（144 个文件；其中包内 `crates/<pkg>/tests/` 157 条 + `src/tests/` 173 条）
  − 执行 768 = **8**，与 V12 §12 R0 的归因表（5 条 `nats` + 3 条 `postgres`）逐条对上。
- 行数棘轮 `BUDGET_DIFF_LINES=0`（快照没改写任何预算）、`FINAL_BUDGET_SUM=37 files 46160 lines`、
  `TESTS_DIR_REGISTERED=0`；被跟踪的 `deploy` 产物被改写 `MODIFIED_DEPLOY=0` 项，
  但跑完仍有 **72 条未跟踪产物**（任务 #82 的根因，已作为 limitation 记进能力矩阵）。
- **门禁量具本轮修掉六类静默失效**（CRLF 污染 `mapfile`、heredoc 里的管道解析、`env -u` 被
  `~/.local/bin/env` 遮蔽、grep 不存在的键、**`-- <filter> --no-fail-fast` 被 libtest 拒认而基线 spec
  整轮空转**、**证据路径判据只核对"整行只有一个路径"的行**）。纪律随本轮钉下：
  **退出码与"0 失败"都不构成测量发生的证据，必须同时取到该跑必然产生的标记行**。
- 本轮**没有**接任何外部服务或凭据，`sandbox_tested` 全部维持 `false`。
## Unreleased — V11 合流轮（两次）：第一次契约对自家写侧说了谎，第二次冲突的全是文档口径（2026-09-23）

T 轮落进本地之后 `git fetch` 才看到 `origin/main` 上多了 Q70/Q71 两颗；把那颗合到全绿，远端又多出 Q72（72c347e），于是同一轮里合了两次。两侧各自跑完自己的门禁与用例都是绿的，
合流之后才发现**对外契约对它自家读模型每天印出的 `"equity_raw":null` 说了谎**：Q70 把协议侧 `equity_raw` 变成
`Option<i128>`（缺标记价的持仓不算权益，而不是拿剩余现金冒充），T3 刚把契约钉成"七格可空、权益不可空"。
本轮跟上事实，并把三处判据从"抄字段名单"换成"问写侧产物与协议类型"。逐项证据、5 颗变异与一句被当场证伪的话见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §39；第二次合流的四处口径判断在同文档 §39.5。

### Fixed

- **`schemas/account-snapshot-v1.json` 的 `equity_raw` 改为 `["integer", "null"]`**，判据同时换掉三处：
  `crates/qx-protocol/tests/snapshot_schema_contract.rs` 那条用例改由写侧自己交两份产物（八项全未算 /
  八项全算得出）逐格比契约，并要求那份"全部未算"能被 `from_json` 读回 `None`；门禁
  `account_snapshot_schema_check` 第 4 项直接正则读协议源码，契约里可空的那些必须正好等于类型为
  `Option<i128>` 的那些；`python/tests/test_bridge.py` 补"契约允许 null 的键，对面 Python 读侧也收得下"。
  名单与类型任何一侧单独漂都会红。
- **门禁的一条失败文案停在旧口径**：`/account/balances` 那一项的条件里 `equity_raw` 是第三条，文案只点名
  available/margin——判据对、文案错，一样会把人往错的方向上带（M5 顺手查出来的）。
- 合流余下的三处编译错与一处夹具分叉：T3 的两份夹具赋值补 `Some(...)`（`snapshot_schema_contract.rs:279`、
  `snapshot_single_source.rs:679`），`api_snapshot_money_fields.rs:249` 改调 `src/tests/mod.rs` 里那份共享
  paper 夹具 `paper_runtime_config`。

### 合流时塌掉的那一份夹具

- 上游那份文件顶部的 `paper_runtime`（`origin/main` 版 `:13`）与本仓 `crates/qx-cli/src/tests/mod.rs:192` 的
  `paper_runtime_config` 是同一份配置的两份手写字面量——逐行比过，除函数名与 `pub(crate)` 外完全相同；
  `seed_paper_fill_with_fee` 在 `:31` 与 `mod.rs:209` 也是同一对。合流取共享那一份，上游新增用例的调用点改名到
  `paper_runtime_config`（`api_snapshot_money_fields.rs:249`）。合并后"同一张 paper 模板 + 临时 data_dir"这一族
  只剩 `mod.rs` 一处公共夹具；`ashare_submit_guard.rs:22` 那个同名 helper 读的是同一张模板但额外写那三个
  A 股键、返回的是路径对，它是这一族的第三个成员，本轮不动它（动它就是又一次夹具收敛）。

### 第二次合流（Q72）：代码零重叠，四处口径要判

Q72 动的是 `crates/qx-cli/src/backtests/*` 与新增的 `account_base.rs`，与本仓 T 轮改动不重叠；五份冲突文件
全是文档与门禁。判断逐条落在 V11 §39.5，这里只记结论：

- **上游那份收口指南仍写 `--fill-tier` 有 `l2`/`l3` 两档订单簿内核**，合流后的代码只认 `l1`/`l2`
  （`cli_help.rs:67-68`，R11 的收口）。照抄上游就是把一条已修的缺陷重新写成现状：取本仓那份，
  只把"多腿链当场拒收 `strategy.initial_cash_raw`"折进同一格。
- **README 的 blessed 摘要那格两侧都漂**：实测被 git 跟踪的 16 份 `*.summary.json` 全停在
  `schema_version: 1` 且缺 `input`/`account` 两块，当前代码写 4（`backtests/artifacts.rs:301`）——
  本仓那句"（无 `input` 块）"已经少说了一块。
- **上游 §34.5 的"只报不修"清单回读过现状**：`multi_builtin.rs` 498 行 / 线 500 行属实，回测与 paper
  两格本金互不知情属实（`strategy_schema.rs:133` 的注释自己就写着"回测读不到它"），两条原样留在 README。
- **编号位移不能全交给正则**：机械抬位把"`§32`–`§35` 在远端已是收口记录"抬成了 `§36`，范围两端只有
  一端该动；回读手写改回。远端现占 §32（Q70）/§33（Q71）/§34（Q72），本仓五节落到 §35–§39。
- **两次合流各抬长了上游的文件，最先烂掉的是 `file:NNN` 这一层**：结构性引用（某个函数、
  判据、产物字段住在第几行）按**当前代码**逐条复位，取证性引用（"改前那一行长这样"、
  "这条变异打在哪"）留在原处——它们钉的是那一轮的树，复位等于把记录改成假话。本轮复位
  29 条（V11 22 / CHANGELOG 7），口径见 V11 §39.8。

### 门禁数字（两次合流后本轮实测，覆盖 §38.6）

| 项 | 第一次合流后 | 第二次合流后（本轮实测） |
|---|---|---|
| 架构不变量 | 318 项全绿 | **328 项全绿**（+13 / −3 逐标签比过：10 项 Q72 新增，3 项按现状改名——用例地板 202→210、内置链包装定义、摘要 `input` 块随 v3→v4 改口） |
| 整树 `cargo test --workspace` | 770 passed / 0 failed | **778 passed / 0 failed / 0 ignored**（+8 条全来自 Q72：三条读法单元 + 五条命令行）；`--all-features` 另测 **782 passed / 4 ignored** |
| `CLI_TEST_FLOOR` | 202 | **210**（两侧各报过一个历史值 202 / 191，合流后按磁盘 `^#[test]$` 重测：`src/tests` 165 + `crates/qx-cli/tests` 45） |
| Python `unittest` | 46 OK（2 skip） | **46 OK（2 skip）**（上游本轮没动 Python 侧） |
| `cargo fmt` / `clippy -D warnings` | 0 / 0 | **0 / 0**（base + `sqlite` / `postgres` / `nats` / `postgres,nats` 四个组合各跑一次） |
| 行数预算 | 全过（三处随合流改快照） | **全过且 `--snapshot` 后 0 改动**：Q72 的新文件全在 500 行线下，`multi_builtin.rs` 498 行仍未入册 |
| 命令面与示例配置 | 52 条入口行 / 41 个命令名 | **同值重测通过**（`target/debug/qx-cli.exe help` 52 条 / 41 个、`deploy/*.json` 顶层 52 份），README 那三处实测口径照旧 |
| 文档编号 | R=§34、S=§36、T=§37、合流 §38 | **远端占 §32（Q70）/§33（Q71）/§34（Q72），本仓五节整批落到 R=§35、三遍扫描=§36、S=§37、T=§38、合流轮=§39** |

## Unreleased — V11 Q72：回测压在多少钱上，可声明、来源可见（2026-09-23）

回测链路实测（V11 #54）排到的第九颗：回测 FN9。§28/§29/§32/§33 那条纪律管"没算过的钱不许印成 0"，
这一颗管**被假定的钱**：一个数字如果每条链都心里有数、嘴上不说，它和"没算过"是同一类缺陷 ——
它决定收益率的分母、风控门看到的可用现金，而使用者从头到尾没被问过一句，也没在哪个产物里读得到它。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §34。

### Fixed（本金从三处字面量变成一格可声明、四处可见）

- **默认本金只剩一处具名常数**：`Money::from_i64(100_000)` 在生产代码里原有 **6 次**
  （`backtests/mod.rs:49` 那个从没被人读过的装配默认、`depth.rs:80/81/144/157`、
  `single_strategy.rs:58`，另加 `ecosystem_smoke.rs` 的自检副本）。现在它是
  `crates/qx-cli/src/backtests/account_base.rs:12` 的 `DEFAULT_BACKTEST_INITIAL_CASH`，
  门禁把"全 qx-cli 生产代码扫不到 `from_i64(100_000)` 字面量"写成判据（跳过 `tests` 目录与
  文件尾 `#[cfg(test)]`），抄第二份当场红。
- **本金改成装配的必答题**：`BarBacktestAssembly::new` 多一个 `initial_cash: Money` 入参
  （`backtests/mod.rs:48`），字段去掉 `pub(crate)`（`:16`）—— 新链漏答过不了编译，答完也不许事后改写，
  `single_strategy.rs` 那行 `assembly.initial_cash = initial_cash;` 随之删除。
- **非正声明当场报错而不是回落默认**（`account_base.rs:31-48`）：0 元账户上的 `return_bps=0`
  是一句假话，报错文案格出键名、值与理由。
- **多腿链当场拒收那一格单账户数字**（`leg_funding.rs:73-86` 的 `reject_configured_initial_cash`，
  `multi_builtin.rs:44` 调用）：两条腿的本金各按本腿行情定资，一份数字定不了两条腿，收下再静默丢掉
  等于配置说假话 —— 与 §22 的 A 股段、§23 的延迟设置同一条纪律。
- **实测分叉**（同一份 `sma_cross` + 同一份 `pairs-primary` 夹具 + 同一个 `--quantity 2`，
  本轮 `/tmp/qx_q72_probe/probe.log`）：不声明 → `fills=0 return_bps=0`
  （两笔买入被"买入成本超过账户可用现金"挡下）、`result_hash=892f8478c281a3e5`；
  声明 `200000000000000` → `fills=1 return_bps=-23`、`result_hash=4faa4db9d340864d`。
  改前 `fills=0 return_bps=0` 念出来像"这策略不赚不赔"，真相是"这两单位没人给它算过钱"。

### Added（来源可见、摘要落盘、用例与门禁）

- **stdout 三行本金播报**：`[Strategy · Account]`（`single_strategy.rs:215-216`）、
  `[Builtin · Account]`（`:371-372`，这条链不落摘要，stdout 是本金对使用者唯一的出口）、
  `[Depth · Account]`（`depth.rs:228-229`），排版函数只有一处。来源分三种且可区分：
  `builtin-default` / `strategy-initial-cash` / `multi-leg-funding-rule` —— "没配"与"配了同一个数"必须分得开。
- **摘要升到 schema v4**（`artifacts.rs:301`）：`BacktestArtifactsInput` 多一个必填字段
  `account_base`（`:223`，新增落摘要的链必须答它），`:305-308` 落 `account` 块两格
  （期初本金与来源，定点整数按仓库惯例写成字符串）。
- **配置面那一格**（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:129-137`）：
  `initial_cash_raw: Option<i128>`（`Money` 是 i128 定点数，没有 i64 天花板），带
  `skip_serializing_if = "Option::is_none"` —— 省略时配置序列化字节逐字节不变，
  否则已 bless 的 `config_fingerprint` / `RunManifest` 会因一个不改变行为的字段集体失真。
- **用例 8 条，`CLI_TEST_FLOOR` 183 → 191**：单元 `crates/qx-cli/src/tests/backtest_account_base.rs`
  （3 条：默认必须有名字、声明值原样折成 `Money` 不被单位截断、`0/-1/-1e9` 全部 `unwrap_err`）；
  命令行 `crates/qx-cli/tests/backtest_account_base.rs`（5 条真实子进程，其中一条是**同一份配置只差
  那一格本金 → 结果必须不同**，断言 `fills` 0→1 且 `result_hash` 变化，另加来源两写法可区分）。
  既有两条跟着改口径：装配用例不再比对"装配自带的默认"，改成"必须原样带出调用方答的本金"。
- `tools/check_architecture.py` 门禁 **283 → 293 项**（3783 → 3964 行）：新增
  `backtest_account_base_check()`（10 条，字面量唯一 / 三个标签 / 装配私有 / 三条链各自读过 /
  三行播报 / 非正闸门 / 摘要 v4 / schema 字段形态 / 多腿拒绝必须带 `?` 传播 / 八条用例名齐备）。
- **一次被行数逼出来的搬家**：`single_strategy.rs` 加完两处读点与两行打印会越过 Phase 4s 的
  `cli_backtest_module_check()`，把与本金无关、但同样"三条链共用一句措辞"的信号参数侧
  （`apply_configured_builtin_signal`、`builtin_signal_note`）搬进
  `backtests/signal_binding.rs`（0 → 47 行），`single_strategy.rs` 496 → 477 行。
  `maturity/line_budgets.yaml` 本轮**一个字节都没动**（`BUDGET_DIFF_LINES=0`）。

### 验收（`/tmp/qx_q72_gate3.log`，整树 744 passed / 0 failed，77 个目标）

M1–M5 五条变异全部 `MUT_STATE=red` 且 `OTHER_FAILS=0`（每条只点亮被测那一项），五次还原均
`RESTORE_EXACT`，`PRISTINE_OK final`、`MODIFIED_DEPLOY=0`、`ARCH_ITEM_TOTAL=293`、`DISK_TEST_TOTAL=191`。
本轮另有两份作废日志，原因都写进 §34.3：`gate1` 的 M5 判据只数调用点、抽掉 `?` 仍然绿（一条真空判据，
预检变异当场抓到）；`gate2` 的 M2 改了判据文案没改脚本里的 want 串。**只有 gate3 是本轮验收依据。**
## Unreleased — V11 T 轮收口：上一轮"只报不修"里缺证据的四项，这轮全部改成有人咬着（2026-09-23，T1–T4）

§37.5 / §35.5 的清单里有四项，原因并不是"需要拍板"，而是**证据留不住**：修复已经在了（或两份读法已经漂了），
但全仓没有一条常驻用例会在它被改回缺陷态时变红。本轮只补这一件事，判据一句话——"任何人把它退回缺陷态，
CI 会不会响"。四项共 **33 颗变异逐颗验证**，每颗跑完按字节还原。逐项证据、变异报告与本轮踩到的两条见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §38。

### Fixed

- **T2（R17）日历组件指纹两侧一宽一严**：Python `AshareTradingCalendar.component_fingerprint` 把摘要登记进
  DatasetBundle，CLI 的 `dataset_component_file_fingerprint` 在回测启动前对同一份文件重算，两份 canonical 字节
  各写一遍，中间只有一句"必须和 CLI 保持一致"的 docstring。实读到的分叉是具体的：旧格式日历（顶层没有
  `sessions`）Python 与回测规则装载都读成"没有时段"，指纹侧却直接报错——**Python 登记好的 bundle 会被 CLI 判成
  组件非法而拒启**。现在 `None => &[]` 对齐宽度（`sessions` 是非数组仍然报错，fail-closed 的那一半不动），
  两侧改为读同一对夹具，摘要只存在 `.fingerprint` 那一份文件里，代码里出现抄本会被门禁点名。
- **T3（S10）账户快照的对外契约有两份手抄，且文件那份零消费者**：`ACCOUNT_SNAPSHOT_JSON_SCHEMA` 与
  `schemas/account-snapshot-v1.json` 逐键比过（已声明字段全等、差 6 个键），但**两份都漏掉了写侧恒印的八个钱
  字段**。改为常量按字节 `include_str!` 仓库那份文件——漂移在编译期就没有退路；文件补齐八个钱字段（那一轮
  是"七格可空、权益不可空"，权益那一格在同日合流轮被 Q70 推翻，见上方「合流轮」一节）。另一半是读侧：`from_json`
  此前只核对"顶层与表内版本号是否自相矛盾"，一份按别的
  版本自洽封存的文档会被当 v1 解出来，而对面 Python `load_account_snapshot` 对同一份产物直接抛错；现在补
  `ACCOUNT_SNAPSHOT_SCHEMA_VERSION` 闸门，Python 侧必填集合同步补 `equity_raw`（必填说的是键必须在，值可以是 null）。
- **T4（S12）`reconcile` 的默认 worker 名是字面量**：`cli_args.rs:269` 是全仓唯一一个可选 `worker_id`，省略时
  HEAD 回落 `"reconciler-main"`，而 CCXT 那条链的 reconciler 实际叫 `ccxt-reconciler-main`。两个方向都撒谎：
  名字合法改动的拓扑上报"找不到 worker: reconciler-main"（把"没解析"说成"不存在"），恰好有个同名 execution
  worker 时会真跑一次下单执行。现在按 **role + Venue 绑定**解析（与 S1 同一判据），零个或多个候选都报错并列出
  候选要求点名。

### Added

- **T1（R13）：已修的判定补上常驻反例**，生产代码 0 行改动（`git diff HEAD -- crates/qx-api/src/lib.rs` 为空）。
  一条用例三向钉住：本账户的无 venue 事实必须全投影、换账户必须拒（订单与账簿各一次）、带显式 venue 的成交
  仍必须守住交易所边界——退回收紧与放宽过头两个方向各咬一半。
- 新常驻用例：`crates/qx-api/tests/account_projection_identity.rs`（1）、
  `crates/qx-protocol/tests/snapshot_schema_contract.rs`（6）、
  `crates/qx-cli/src/tests/calendar_component_fingerprint.rs`（3）、
  `crates/qx-cli/src/tests/reconcile_worker_identity.rs`（5）、`python/tests/test_ashare.py`（2），
  `test_bridge.py` 那条改为必填与版本双向断言。
- 新夹具：`python/tests/fixtures/calendar-component-v1.json` 与 `…-legacy.json`，各配一份 `.fingerprint`
  （由 Python 写侧函数产出，Rust 读侧只重算不另抄）。
- 新门禁 12 项：`calendar_fingerprint_caliper_check()`（4：夹具成对且两侧都读、摘要无抄本、
  两侧字段白名单与契约版本号逐项相等且声明个数相符、5 条常驻反例都在）、
  `account_snapshot_schema_check()`（8：只有一份文本且生产 Rust 无字面量、契约自洽、键集合等于写侧产物、
  可空口径、协议名与版本同源、路由发出的就是它、对面 Python 同宽、六条判据各有常驻用例）。

### 门禁数字（本轮实测）

| 项 | T 轮前 | 现在 |
|---|---|---|
| 架构不变量 | 302 项 | **314 项**（302 → +8 T3 → +4 T2） |
| 整树 `cargo test --workspace` | 752 passed | **767 passed / 0 failed / 80 个测试壳**（+1 T1 +5 T4 +6 T3 +3 T2） |
| `CLI_TEST_FLOOR` | 191 | **199**（实测 `src/tests` 160 + `crates/qx-cli/tests` 39） |
| Python `unittest` | 44 | **46 OK（2 skip）** |
| `cargo fmt` / `clippy -D warnings` | 0 | 0（`--workspace --all-targets --all-features`），`tools/validate_core.py` 全过 |
| 行数预算 | — | 新入册 `worker_entry.rs: 509`，`cli.rs` 675→674；`qx-api/src/lib.rs` 与 `qx-protocol/src/lib.rs` 均零增行 |

### 本轮踩到（§38.4）

- **变异电池里的"红"必须是断言红**：一颗变异的红其实是 `link.exe exit code 1104`（另有一颗 cargo 在抢同一个
  `target/`），脚本按"编译失败也算被抓"把它记成了通过。单独重跑才拿到真红；判定式改为编译/链接失败一律记
  "这颗没做完"。
- **变异要改判据，不是改判据所在的语法结构**：把 `if let Some((a, b)) = identity {` 整段换成 `if false {` 会连
  块体引用的绑定一起删掉（E0425），这颗不合法。CRLF 仓库里用 `\n` 拼锚点 0 命中那条老教训本轮又撞一次。

### 只报不修（升级路径写在 §38.5）

上一轮清单全部维持（R8、C3、C4、C6、C9、S8、S9、S11、S13 行为侧、`qx-execution` 1619 行结构债）。本轮新立四条，
都不需要拍板但一拍就得改口径：T2 的夹具模式只钉住了 `calendar` 一条分支，`corporate_actions` 那半边
（Python 的 `source_hash` 与 CLI 的 `serde_json::to_vec` sha256）今天靠"看起来一样"活着、没有用例也没有示例绑上去
（`bars` 那条看着同类，但 `dataset_component_paths` 里 `kind == "bars"` 被拓扑校验当场拒，遇不上 JSON 复检，已排除）；
旧格式（无 `schema_version`）日历的宽度现在靠两侧各自同意，"谁该写 `sessions`"仍无声明；T3 之后契约仍无字段级 description、顶层保留
`additionalProperties: true`，未知顶层键的行为没用例；`$id` 指向的 `https://qianxing.dev/...` 解析不到东西，
真接 JSON Schema 校验器要引依赖，与零新依赖纪律冲突。`maturity/capabilities.yaml` 里 `sandbox_tested` 仍全为
`false`。

## Unreleased — V11 R/S 两轮收口：三遍连通性扫描，从数据面查到控制面与插件边界（2026-09-23）

判据是"主体流程全部联通、核心链路无断链、无孤儿逻辑、无死循环、前后端贯通，至少查三遍"。三遍各换一个
视角：第一遍顺主链路（配置 → 绑定 → 装配 → 提交/成交 → 记账 → 投影 → 读模型 → 产物），第二遍看运行期
与外部接口（worker 生命周期、重连、健康结论、跨语言边界），第三遍反方向核对（声明了没人读的东西、读了
没人声明的东西、循环能否终止、两侧契约是否互拒），末了把同一套反向核对推到仓库外那一端（C++ 插件的 ABI
镜像）。两轮共立案 30 项（R 轮 18 项、S 轮 12 项），23 项落代码并
留常驻证明，7 项判为"需要部署侧或契约决策"只报不修；§35.5 另挂着前几轮立案的 C3/C4/C6/C9 与两条证明缺口
（R13 无常驻反例、`qx-execution` 1619 行结构债），本轮复核后仍未动。方法、逐项证据与变异报告见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §35–§37。

### Fixed（R 轮 R1–R16、R18：数据面与读侧诚实）

- **一条事实只留一份编码**（R14/R15/R16/R18）：稳定 JSON 曾把裸 `u64` 当对象键，产出的 `{77:{…}}` 连合法
  JSON 都不是、三个读侧一起失败；`positions` 表仍是手写 `format!`，少印 `instrument` 且每加一个字段就静默
  丢一处；BarFrame 文档不自述契约版本，Rust 自己写出的帧在自家读侧被降级成 legacy 宽松分支；跨语言用例吃
  的是手抄字典（四张键表全填 `{}`）而不是写侧产物。四处统一成"读侧认的那一份 = 唯一被产出的那一份"，再用
  共用夹具与两侧断言焊死。
- **缺席不再印成零**（R7/R10/R12）：账户快照 `reconcile` 两格补"有没有来源"这一位；无键读模型端点过去各自
  挑各的"默认账户"（账簿说 A、快照说 B），现在 `default_account_event_log` 是唯一答案。
- **健康结论必须与"真的测过"绑定**（R4/R5/R6）：CCXT 行情 worker 整轮全部标的取数失败仍报 Ready、`/ready`
  把"一个 worker 指标都没有"读成依赖健康、doctor 的运行拓扑项只打印不判定（构建失败也记 pass）——同一个布尔
  在"没有观测"与"观测通过"之间被混用的三处，各自换成会红的判据来源。
- **运行期两处自欺**（R2/R3）：Binance 对账器重启后把自己发布的余额事实当成交易所回报，自比对自 → 永远
  "无差异"（身份只构造一次）；用户流重连预算按累计次数计，进程跑够久就永久断开且退避单调上涨（改按连续失败
  计数，`report.reconnects` 退回纯统计口径）。
- **配置与身份判定不再静默走偏**（R1/R11/R13）：五条回测链只有一部分问 `strategies[]` 的绑定，A 股段能从策略数组
  绕开"当场拒"闸门；深度链把配置里的 `strategy.fill_model` 丢掉（用户以为换了撮合，其实没有，现已加"零产物"
  负行）；API 投影拿标的后缀当账户 venue，把 paper 账户自己的订单事实判成外来事实。
- **文档面与命令面一致**（R9）：help 与 README 列过不存在的入口、漏过新增入口，用户照抄就跑不通。

### Fixed（S 轮 S1–S3、S5–S7、S13：控制面自身，以及验证手段本身可信）

- **S1 `serve` 启动即死**：监督器的 `HealthRegistry` 按 `worker.id` 建键，而两处 `spawn_worker` 写死字面量
  `"api"`；worker id 是自由命名、拓扑校验只数"启用的 api role worker 恰好一个"，所以任何改名后的**合法**配置
  都会让进程在启动瞬间死于"未知 worker: api"。改为 `configured_api_worker_id()` 按 role 解析（与拓扑校验同
  一判据），解析不到就报错而不是回落。
- **S2 两条生产循环永不自然结束**：`workers.rs` 的 scheduler 与 strategy 循环是唯二不读 `should_stop()` 的
  循环，`once` 之外的唯一出口是抛错。补停机轮询；令牌本身仍无人翻，立案为 R8（见下）。
- **S3 只读运维端点念开机那一刻**：`/scheduler/runs`、`/account/ledger`、`/reconcile/reports` 读的是启动时
  装进 `ApiState` 的三份读模型，worker 之后落的对账报告、追加的成交在 HTTP 侧永远看不见。新增
  `ApiQueryModels` + `with_query_models_provider`，每请求现读并回填 `ApiState`，使 trait 与 HTTP 同一口径。
  账户族四端点仍读 boot 投影（扩它要重投影泵），登记未动。
- **S5 可用值抄了三份**：`init --profile` 的接受集合在 match 里，help 的 `<…>` 表与"未知 profile"文案各抄
  一份且都漏了 `builtin`——用户被告知的取值集合里恰好少了那个真正能用的取值。改为 `INIT_PROFILES` 一份、
  三处由它生成。
- **S6 过期 binary 让子进程用例假绿**（本轮唯一一次"验证手段说谎"）：`tests/mod.rs` 的新鲜度守卫只装在
  `option_env!("CARGO_BIN_EXE_qx-cli")` 取得到值的分支上，而本 crate 的 `--bin` 单元测试走的是回落分支，
  `cargo test --bin` 又不重链 `qx-cli.exe`；于是 M4 变异第一次跑成全绿。回落分支补同一道守卫。
- **S7 对外端点没有文档面**：17 条路由里 15 条在全仓 markdown 零命中。`deploy/README.md` 补端点表（语义、
  查询串、非 200 口径、限流先于鉴权），并让"表内 `METHOD 路径` 集合 == 路由集合"与"未列路径一律 404"成为
  门禁项。
- **S13 C ABI 两份手抄镜像零比对**：插件契约由 `cpp/include/qianxing_strategy.h` 与
  `crates/qx-strategy/src/c_api.rs` 的 `#[repr(C)]` 各抄一份，头文件侧连一个 `static_assert` 都没有，Rust 侧
  的布局自证只钉自家 `QxRaw128` 与自家版本常量；CI 从没用宿主加载过 C++ 插件，所以改乱任一侧的字段顺序或
  类型宽度在 CI 里都是绿的。新增 `c_abi_header_check()` 做逐字段比对（10 类型 / 64 个结构字段 / 6 个判别值 /
  版本 `1`↔`1`，当前全等），M8–M12 五条变异证明"少一个字段、换一次顺序、改一下宽度、动一下版本、多一个
  类型"各当场红掉对应的门禁项。

### Added（用例与门禁）

- 新用例文件：`crates/qx-cli/src/tests/runtime_api_worker_identity.rs`（S1 两条）、
  `api_query_models_live.rs`（S3 两条）、`api_default_account_reads.rs`（R12）、
  `crates/qx-adapter/tests/binance_stream_retry.rs`（R3）、`crates/qx-datastruct/tests/frame_contract.rs`
  （R16 三条）、`crates/qx-datastruct/src/tests.rs`（用例搬家，lib 从 767 行降到 702 行）、
  `crates/qx-cli/src/backtests/config_declarations.rs`（R1/R11 共用的策略块解析入口）、
  `python/tests/fixtures/account-snapshot-v1.sample.json`（R18：写侧 `to_json` 原样输出，两侧各钉一次）。
- 新门禁：`control_plane_honesty_check()`（8 项，S1/S2/S3/S5/S6）、`api_surface_doc_check()`（2 项，S7）、
  `c_abi_header_check()`（4 项，S13：两侧类型集合对称差、字段名与顺序逐位、字段类型按映射表、枚举判别值与
  ABI 版本常量相等；未登记的 C 类型直接报错而不是跳过）。
  两轮的三遍扫描本身不新增依赖、不新增配置键：§36.3 的反向核对查到一处示例覆盖缺口（`cost_rules_path` 有文档
  无示例），已记为"缺口"而非"断链"，本轮未动。

### 门禁数字（本轮实测，覆盖 §31.4 / §35.4 口径）

| 项 | R 轮前（`99c7051`） | 现在 |
|---|---|---|
| 架构不变量 | 279 项 | 302 项（S 轮：288 → 295 → 296 → 298 → 302） |
| 整树 `cargo test --workspace` | 733 passed | **752 passed / 0 failed / 0 ignored**（78 个测试壳） |
| `CLI_TEST_FLOOR` | 178 | 191（实测 `src/tests` 152 + `crates/qx-cli/tests` 39） |
| Python `unittest` | 43 | 44（2 skip，均为本机缺依赖） |
| `cargo fmt` / `clippy -D warnings` | 0 | 0，`qx-cli` 特性矩阵四档（sqlite / postgres / nats / postgres,nats）同绿 |
| 行数预算 | — | 抬升：`qx-api` 3167→3224、`strategy_contract.rs` 822→840、`workers.rs` 718→728；下降：`binance.rs` 2291→2217、`qx-datastruct` 767→702、`qx-protocol` 885→844 |

### 本轮踩到（§36.2、§37.4）

- **委派报告里的每个 file:line 都要自己 grep 才算证据**：`/live` 端点、`is_backtest_artifact_path`、
  `CLI_TEST_FLOOR` 过期三条候选全经自跑驳回，未据此改代码。
- **绿色的子进程用例不等于绿色**（S6），**变异必须双向看**（M1a 只红用例、M1b 只红门禁），
  **门禁的锚点不能撞上自己要防的字符串**，**CRLF 仓库里用 `\n` 拼字节锚点会 0 命中**。
- **静态比对门禁要先证明解析器没在偷读**（S13）：比对器定稿前三次"报错"都是它自己没读对——tag 写在闭括号后的
  typedef 看不见、vtable 只取到回调名丢了 `abi_version`、`*const c_char` 一侧去空格另一侧不去（实测误报 18 处
  "类型不等"）。红得没有名字比不红更难查，所以未登记的 C 类型改为直接报错，并用五条变异证明它看得见差异。
- **日志为空不等于通过**：一次变异把输出写到 `$TEMP` 拼错的路径，读到的是另一个项目的过期日志，
  差点把编译失败的 `exit 101` 记成测试结果。

### 只报不修（升级路径写在 §35.5 / §37.5）

R8（停机令牌无人触发：出口有了、谁翻令牌是部署侧决策）、R17（日历组件指纹两侧各写一遍 canonical 字节，
无实测分叉但互不拒绝；**T 轮已收口**）、R13 无常驻反例（**T 轮已补**）、C3（账单水位是跨币种/跨腿标量）、
C4（Binance 行情流被对端关闭后不重连）、C6（实盘与回测对同一份帧给出两种数据集身份）、
C9（`valid_from/valid_to` 从不按交易时钟校验）、S8（产物摘要里的路径编码）、S9（`ProjectionLineage` 两格恒空）、
S10（账户快照 schema 两份、服务侧更松；**T 轮已收口**）、S11（API 限流参数硬编码）、
S12（对账默认 worker id 与 CCXT 链分叉；**T 轮已收口**）。另有一条不是缺陷而是证明形态缺口：
CI 从没把 C++ 插件交给 Rust 宿主加载过（`qianxing_strategy_example` 确实被 build 出来却无人 `load_verified`），
所以 S13 的 ABI 镜像一致性目前只有静态门禁一层保护。`maturity/capabilities.yaml` 里
`sandbox_tested` 仍全为 `false`——本轮全部是本机行为用例与静态门禁，不含真实下单回报。


## Unreleased — V11 Q71：多腿组合收益按两条腿的钱算，不再把两腿 bps 平均（2026-09-23）

回测链路实测（V11 #54）排到的第八颗：回测 FN8。§28/§29/§32 那条纪律管"没算过的钱不许印成 0"，
这一颗管**加权**：把两个分母不同的比率等权平均，念出来的既不是组合收益率、也不是任何一条腿的收益率，
而它在终端上看起来完全像一个合理的组合收益率。收口记录见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §33。

### Fixed（一个印在终端上的加权错误）

- **组合收益改为两条腿按钱合计**：`crates/qx-cli/src/backtests/multi_builtin.rs:274` 此前印
  `(i64::from(primary_report.return_bps) + i64::from(reference_report.return_bps)) / 2`。两条腿的本金由
  `multi_leg_leg_cash` 各按**本腿行情帧的最高价**定资，天然不等 —— 仓库自带的那对现货夹具按 `quantity=100`
  跑，主腿本金正好是对冲腿的 **21.0 倍**，两腿分别 -197bp 与 -7bp，平均念 **-102bp**，按钱算实际是
  **-189bp**：这个组合的真实亏损被念轻了 87bp，接近一半。现在口径只有一处
  `Σ(期末权益 − 期初本金) × 10000 ÷ Σ期初本金`（`crates/qx-cli/src/backtests/leg_funding.rs:161-185`，
  与定资同处一文件，"权重"因此不再有第二个答案）。
- **算不出时报错而不是折成 0**：合计本金 ≤ 0、`× 10000` 越界、结果超出 `i64` 三种情形一律 `Err`。
  印 0 会把"这个组合根本没法度量"伪装成"这单套利不赚不赔"，与 `multi_leg_leg_cash` 撤掉静默截断同一条纪律。
- **一次被行数逼出来的搬家**：`multi_builtin.rs` 加完新代码会越过 Phase 4s 的
  `cli_backtest_module_check()`（`tools/check_architecture.py:723`，`>= OVERSIZED(500)` 即红，登记进预算表
  也救不了）。把入口那 11 行组级合计折叠搬进归因内核 `crates/qx-cli/src/multi_leg.rs:452-470`
  （新增 `multi_leg_group_totals`，保证金仍取各组峰值而非求和），入口只留 1 行调用；
  `maturity/line_budgets.yaml` 本轮**一个字节都没动**。

### Added（用例、产物两端与门禁）

- `crates/qx-cli/src/tests/execution_and_multi_leg.rs:304-331`
  （`multi_leg_combined_return_weights_each_leg_by_its_own_capital`）：本金 3:1、两腿 +100bp/+1000bp 时必须
  给 **325bp** 而不是等权的 550bp；本金相等时两口径重合（排除"只是换了个说法"）；亏 1000bp 的主腿压过
  持平的对冲腿给 -750bp 而不是 -500bp；全零本金必须 `Err` 且文案含"本金合计必须为正"。
- `crates/qx-cli/tests/multi_leg_attribution.rs:726-792`
  （`combined_return_pools_both_legs_by_capital_instead_of_averaging_bps`）：跑真实 CLI，用产物自己声明的
  `accounts/*_initial_cash` 与 `accounts/*_final_equity_raw` 复算钱口径，要求 stdout 与
  `totals.combined_return_bps` 都等于它，并**另加一条 `!=` 两腿平均** —— 少了这条，用例在旧实现下也是绿的。
- **产物补齐复算所需的两端**：`multi_builtin.rs:445-446` 把两条腿的期末权益写进 `accounts`（期初本金本来就在
  同一块，缺一端别人复算不出来），`:459` 落 `totals.combined_return_bps`。
- `tools/check_architecture.py` 门禁 280 → **283 项**（四缩进 `check(` 201 → 204），
  `multi_leg_honesty_check()` 十二条长到十五条：调用点只许出现那一份实现且 `i64::from(primary_report.return_bps)`
  不得复活、实现体内必须留错误文案且不得出现 `Ok(0)` / `unwrap_or`；两条用例名 + 产物两腿期末权益键齐备；
  组级合计折叠只在 `multi_leg_group_totals` 一处。
- 用例地板 `CLI_TEST_FLOOR` 178 → **183**。这个数字本轮实测落后两次：Q69/Q70 新增的三条用例没回写地板
  （磁盘 181 而门禁仍写 178），连同 Q71 两条一起按磁盘总数（`src/tests` 143 + `crates/qx-cli/tests` 40）抬平。

### 本轮日志实测（`/tmp/qx_q71_gate1.log` 20:03:48 → 20:05:28，明细见 §33.4）

- `STAGE1_CHECK_EXIT=0`、`STAGE3_FMT_EXIT=0`、`STAGE4_CLIPPY[qx-cli]_EXIT=0`；`SNAPSHOT_EXIT=0` 且
  `BUDGET_DIFF_LINES=0`；基线三组用例 11 / 1 / 7 条通过，`0 failed`；
- 四条变异全部 `MUT_STATE=red`、`ARCH_FAIL_LINES=1`、`OTHER_FAILS=0`、`PASS_LINES=282`（每条只点亮被测那一项），
  四次还原全 `RESTORE_EXACT`。M1（调用点退回平均）红在 `multi_leg_attribution.rs:774`，M2（`Ok(0)`）红在
  `execution_and_multi_leg.rs:326`，M3（产物缺两腿期末权益）红在 `:756` 的缺键 panic，M4（入口重新自己折
  合计）只有结构项红 —— 算的是同一份五元组，行为用例分不出来，本轮如实记为"静态防守"而未补假用例；
- `QX_PYTHON` 指向 venv 的整树 `STAGE7_WS_EXIT=0`、`WS_OK_LINES=76`、合计 `736 passed / 0 failed`；
  `STAGE8_ARCH_EXIT=0`（283 项全绿）、`MODIFIED_DEPLOY=0`、`PRISTINE_OK final`。`gate1` 即验收轮，无作废轮次。

### 本轮踩到（记在 §33.3）

- **我自己写的第一版判据是假防守，被预检变异当场抓到**：原判据含 `"funded_raw <= 0" in combined_body`，而
  M2 的坏形状恰恰是保留该条件、只把 `Err` 换成 `Ok(0)` —— 条件还在，判据照样绿。改成钉错误文案 + 两种折零
  形状缺席。**每写一条"某串文本必须在/不在"的判据，先问一句"我要防的那个坏形状写出来还带着它吗"。**
- **rustfmt 按 76 字符把元组实参拆成 4 行**，两处共 8 行的增量正好把入口文件顶过 500 行线。修法不是压注释，
  而是先 `let primary_equity = primary_report.final_equity();` 再传，且这两个绑定在产物侧复用 —— 调用点与
  `accounts` 读的是同一个值。
- **零成交那档新旧口径恰好重合**：`crates/qx-cli/tests/builtin_signal_from_config.rs:433` 要求
  `combined_return_bps == "0"`（远档 `fills=0`，两腿权益都等于各自本金）。它在旧实现下也成立，因此**不构成
  新口径的证据**；加权与平均的分叉必须由本金不等的真实夹具来证明。

## Unreleased — V11 Q70：账户权益不再拿剩余现金冒充"算不出的权益"（2026-09-23）

交易链路实测（V11 #54）排到的第四颗：交易 TX4，是 Q67/Q68 那条"缺席不等于零"纪律的最后一颗。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §32。

### Fixed（最后一个不可区分的钱标量）

- **读模型不再替账户宣称"压在持仓上的那一腿不值钱"**：`crates/qx-cli/src/api_service.rs:268-272` 此前写
  `equity_for(...).unwrap_or_else(|| cash_for(...))`。内核 `Ledger::equity_for`
  （`crates/qx-core/src/ledger/query.rs:156-204`）的 `None` 含义明确 —— 有任意一条持仓拿不到标记价、或估值
  溢出。折成剩余现金之后，一个只有成交、没有行情事实的账户会把现金长期报成权益，且哈希/稳定 JSON/线格式/
  diff 全链路自洽，读侧无从发现。现在这个 `None` 原样发布为 `null`。
- **协议层第一次能表达"权益没算"**：`crates/qx-protocol/src/lib.rs:109` 的 `equity_raw` 由 `i128` 变
  `Option<i128>`（构造函数 `None`、`ScalarState` 同步），八个汇总钱字段从此共用 Q67 那一条带存在性标记的
  哈希与序列化通道，本轮没有新增任何一份手抄字段清单。
- **风控侧同一表达式是保守用法，明确不动**：`crates/qx-cli/src/venue_runtime/worker_runtime.rs:189-191`
  的 Paper 现货分支 `unwrap_or(cash_only)` 把权益往小里说、闸门只会更紧，其注释已写明；本轮红线只划在读模型侧。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/api_snapshot_money_fields.rs` 新增
  `equity_without_a_mark_price_is_absent_rather_than_the_remaining_cash`：成对钉"缺标记价 → `None` + 两份
  JSON 印 `null` + 可用资金照旧"与"补一条报价之后同一个账户必须重新算得出权益"。
- `crates/qx-protocol/tests/snapshot_single_source.rs` 的 `OPTIONAL_MONEY_FIELDS` 从七个变**八个**，
  Q67 那条"缺席≠零"遍历（哈希、seal、两份线格式、两个 diff 方向）由此自动覆盖权益，无需另写用例。
- `crates/qx-api/src/lib.rs` 的端点用例加第二段：默认快照在 `/account/balances` 上必须印
  `"equity_raw":null`（该端点生产体无需改动即不错报，但没有这一段就测不到权益的缺席态）。
- `tools/check_architecture.py` 门禁 279 → **280 项**（四缩进 `check(` 调用点 200 → 201）：新增"权益只在
  现金与每一条持仓的标记价都读得出时发布"一项（按语句区域判定，对 rustfmt 折行不敏感）；"八个字段全是
  `Option<i128>`"并入 equity；端点项加 `equity_raw` 的 null 发布判据；取法项从"数一行字面量"改成
  "盯整个 `fn scalar_money_raw` 函数体、禁 `Some(` 与 `unwrap_or`"。
- 行数预算：`crates/qx-protocol/src/lib.rs` 885 → 888、`crates/qx-api/src/lib.rs` 3167 → 3180（本轮唯一两处）。

### 本轮日志实测（`/tmp/qx_q70_gate1.log` 19:05:08 → 19:07:55，明细见 §32.4）

- `STAGE1_CHECK_EXIT=0`、`STAGE3_FMT_EXIT=0`、`qx-protocol|qx-api|qx-cli` 三格 clippy `-D warnings` 均 0；
  基线四组用例 10 / 7 / 11 / 1 条通过，`0 failed`；
- 六条变异（M1 读模型兜底、M2 取法折零、M3 端点折零、M4 共用 null 编码器印 0、M5 字段退回 `i128`、
  M6 读模型不算权益）全部 `MUT_STATE=red` 且 `OTHER_FAILS=0`，六次还原全 `RESTORE_EXACT`；其中五条
  （M1/M2/M3/M4/M6）都有行为用例同红，M5 属类型层、由静态项与编译器承担证明（`STAGE6_M5_CHECK_EXIT=101`）；
- `QX_PYTHON` 指向 venv 的整树 `STAGE7_WS_EXIT=0`、76 条 `test result: ok`、合计 `734 passed / 0 failed`。

### 本轮踩到（记在 §32.3）

- **第一版取法门禁项挡不住本轮自己的缺陷变异**：`Some(self.equity_raw.unwrap_or(0)),` 既不命中
  `"Some(self.equity_raw),"` 也不改变 `"self.equity_raw,"` 的计数，写出来就是一张不会红的静态项。判据由此
  改成盯函数体。**字面量计数型判据要先拿变异过一遍，再决定是否算"钉住了"。**
- **给字段新增"缺席"状态时，要逐个发布面检查是否真有一条用例让它缺席**：端点用例原本只把 equity 设成
  `Some(500)`，那一条"把 `None` 折成 0"的变异动不了它 —— M3 的 panic 点 `crates/qx-api/src/lib.rs:2545`
  正是本轮新增的那一段，没有它这个发布面就没有行为证明（日志里 M3 另外三组用例全绿）。
- 本轮 `gate1` 即验收日志：fmt/clippy 在变异段之前先跑绿，是 §31.3 那条教训的落地（无作废轮次）。

## Unreleased — V11 Q69：CCXT 对账的两半发现同时进事实流、报告与健康（2026-09-23）

交易链路实测（V11 #54）排到的第三颗：交易 TX3，§28.5 第 3、4 条（在 §29.5 以第 4、5 条重挂）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §31。

### Fixed（一轮对账有三份互相矛盾的"本轮结论"）

- **两半发现改由一个汇总点出口**：`crates/qx-cli/src/venue_runtime/ccxt_reconcile_worker.rs` 新增纯函数
  `ccxt_reconcile_round(remote_open_issues, local_order_issues)`，返回一份 `order_issues` 与一份
  `require_reconcile`。此前远端孤单挂单只进持久报告与健康判定，本地"查不到远端结果 / `sync_order` 报错"两处
  只进事实流 —— 于是这一轮已经把某张单子推到 `OrderStatus::Unknown`，报告里却写着"本轮没有 order 问题"，
  worker 心跳仍是 `Ready`。现在事实流（单一写入点）、`additional_order_issues`、`ServiceStatus` 与运行明细
  读的都是同一份 `round.*`。
- **"能否落事实流"改为按句柄判定，而不是按属于哪一半**：只有 `client_order_id` 能解析成 JSON 数字的发现才落
  `ReconcileRequired`（OMS 需要一个真实句柄），对不上的远端孤单留在报告里等人工确认归属。用例把
  "同一轮报告 5 条 / 事实流 3 条"钉成正确答案。
- **对账报告的三个覆盖度计数不再把"没取"印成 0**：`crates/qx-api/src/lib.rs` 的
  `position_snapshots_count` / `funding_rate_snapshots_count` / `cashflow_count` 变 `Option<usize>`
  （`#[serde(default)]`，落盘的老报告仍读得回来），CCXT 侧由 `positions_observed` /
  `funding_rates_observed` / `cashflows_observed` 三个观测位门控取值，Binance Spot 这条链对自己从不查询的三项
  直接报 `None`。Q67/Q68 的"缺席不等于零"纪律由此落到对账产物上。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/ccxt_reconcile_round.rs` 两条用例：`ccxt_reconcile_round_routes_both_discovery_halves…`
  断言报告 5 条 / 事实流 3 条与 `(client_order_id, event_tag)` 三元组，并禁止把字符串客户号当本地句柄；
  `ccxt_reconcile_service_status_degrades_on_a_local_only_finding…` 钉住"只有一张本地单子查不到远端结果"
  那一半也必须降级。为此把健康结论从 worker 循环里抽成纯函数 `ccxt_reconcile_service_status`。
- `crates/qx-cli/src/tests/e2e_and_python_contract.rs` 的
  `reconcile_report_persists_structured_balance_discrepancy` 扩到"落盘 JSON 里 `null` 与 `0` 可区分"，
  另加两条旧报告反序列化断言（数字读成 `Some(0)`、缺键读成 `None`）。
- `tools/check_architecture.py` 新增 `reconcile_round_honesty_check()`（10 项）：汇总只一次、三面读同一份、
  事实写入点唯一、`as_u64` 句柄判据、字段形态、Binance 报 `None`、三个观测位、用例在位。
  门禁 269 → 279 项；`crates/qx-api/src/lib.rs` 行数预算 3160 → 3167（唯一变动项），
  `ccxt_reconcile_worker.rs` 341 → 469 行。

### 本轮日志实测（`/tmp/qx_q69_gate4.log` 08:59:35 → 09:01:15，明细见 §31.4）

- `STAGE1_CHECK_EXIT=0`、`STAGE3_FMT_EXIT=0`、`STAGE4_CLIPPY[qx-api|qx-cli]_EXIT=0`、门禁 `279 项` 全绿；
- 六条变异全 `MUT_STATE=red` 且 `OTHER_FAILS=0`，六次还原全 `RESTORE_EXACT`；其中只有 M4（句柄判据）与
  M6（健康结论忽略发现清单）让行为用例真失败，M1/M2/M3/M5 只由静态门禁抓到；
- `QX_PYTHON` 指向 venv 的整树 `733 passed / 0 failed`（76 条 `test result:` 行，退出码 0）；不设该变量的
  gate4 内基线是 `2 failed` —— 本机无解释器的两条已知 Python worker 契约项，与 §29.4 同一口径。

### 本轮踩到（记在 §31.3）

- 三轮日志作废后才拿到验收日志：`/tmp/qx_q69_gate1.log` 里 `cargo clippy -D warnings` 把 `.then(|| …)` 报成
  `unnecessary_lazy_evaluations` 硬错（`CLIPPY[qx-cli]_EXIT=101`）、`rustfmt --check` 两处不过；
  `/tmp/qx_q69_gate2.log` 里 rustfmt 把 `let service_status = if …` 折成一行，M2 的 needle 0 次命中；
  `/tmp/qx_q69_gate3.log` 走完 M1–M5，但暴露出 M1/M2/M3/M5 四条**只有静态门禁红、行为用例全绿**。
  于是把健康结论抽成 `ccxt_reconcile_service_status` 并加 M6（判定忽略发现清单），M6 是唯一一条让行为用例真
  失败的缺陷变异。**教训：变异轮之前先把 fmt/clippy 跑绿、按格式化后的文本取锚点；"静态门禁能抓"不等于
  "行为用例能抓"，两件事要分别在日志里看见。**
- 两条门禁项最初共用一条标签，导致"健康只看远端一半"的变异红得没名字 → 拆成两条独立项。

## Unreleased — V11 文档收口轮：仓库里只留"今天仍然正确"的文档（2026-09-23）

用户请求的四件事（提交代码 / 删除历史无效文档 / 更新接口使用文档 / 更新项目说明文档）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §30。

### Changed（文档面）

- **删除 23 份历史文档**（`git rm`，原文全部在 git 历史里）：V1–V10 各代方案与审计（`牵星完整架构方案-V1`、
  `牵星最终架构方案-V2-Barter对齐版`、`牵星架构拆分与扩展决策-V1`、`牵星终极改造计划-V1` 与其实施状态附录、
  `自研量化框架重构方案-V6/V7/V8/V9/V10`、`Qianxing-Multi-Asset-Quant-Infrastructure-V2`、`QIANXING_CODE_AUDIT_V2`、
  四份 V5 计划/审计/状态/RC + `v0.0.1` release note、根目录 V4/V5.1 两份、可视化终态稿、产品化路线图、差距清单）。
  判据不是"旧"，而是**文档里的模块表或命令名在当前代码里已经不存在**：23 份里有 14 份、合计 115 行点名
  `qx-domain` / `qx-kernel` / `qx-application` / `qx-oms` / `qx-portfolio` / `qx-fenye` / `qx-viz` /
  `qx-server` / `qx-schema`（行数按 `git show HEAD:<路径>` 逐份数命中行；这九个名字在当前 workspace 实测
  23 个 crate 里一个都没有，其中也含"该 crate 已并入 x"这类当时正确的记述，故 115 是上限），
  README 也已经不链接其中大部分。
- **保留的专题文档只改错、不删除**：CCXT worker 契约、A 股接入、外部链路验收阶梯、衍生品统一模型、
  多语言策略契约仍是这些知识的唯一副本（`deploy/README.md` 只做摘要）。
- **`Qianxing-Visualization-Architecture-V1.md` 的去留**：正文是未落地的 Web/桌面端设计（dashboard、market
  terminal、前端接线），其 V1.1 修订自己承认 `qx-viz`/`qx-server`/`qx-schema`/`/ws` 不存在。唯一值得留的是那条
  边界（只读投影不做第二个事实源）——搬进 `README.md` 的"设计底线"第 8 条后删除整篇。

### Fixed（文档里的死命令名与不存在的文件）

- `builtin-backtest` → `backtest builtin`、`ccxt-builtin-backtest` → `backtest ccxt-builtin`、
  `strategy-backtest` → `strategy backtest`、`ccxt-backtest` → `backtest builtin`（前三条入口在 V9 Phase 2
  已整体删除，今天调用得到"未知命令"和退出码 2；本轮用当前 binary 实测确认）。
  位置：`docs/工业化易用性收口指南-V1.md`、`docs/CCXT多交易所接入与策略运行方案-V1.md`、
  `docs/A股数据源接入与快速选股回测方案-V1.md`、`docs/工业级多语言策略与高性能交易方案-V1.md`、`deploy/README.md`。
- 收口指南里 4 处 `deploy/qianxing.runtime.production.json` 指向**不存在的文件**，改为
  `deploy/qianxing.runtime.production.example.json`；同一篇的"策略包括 …10 个"改为按 `builtin-strategies`
  实测的 17 个（13 单标的 + 4 个只被 `backtest multi-builtin` 接受的套利 kind）并停止复述名单。
- CHANGELOG 与 V11 中指向已删文档的 4 处链接改为"已删除，原文在 git 历史"的注记；历史条目本身不改写。

### Added（接口使用文档）

`docs/工业化易用性收口指南-V1.md` 补三节，全部按当轮实测写：

- **§3.2 回测入口族与配置落点**：七条回测入口 × "哪些策略段在这条链上有落点 / 配了即整轮拒绝"的对照表
  （A 股段在 `multi-builtin`/`book` 无落点、深度档拒绝非零延迟、`--fee-bps` 优先级、衍生品只向声明为衍生品的腿计提）。
- **§3.3 产物、输入身份与重放结论**：`*.run.json` 16 个键与 `*.summary.json` 关键段（实抓自本轮一次
  `backtest builtin macd` 运行），并说明 `replay.log_digest == result_hash` 是**重新驱动事件流的结果**，
  而不是旧那个恒等式。
- **§3.4 读模型里的 `null` 与 `0`**：把 Q67/Q68 的钱/价格两套编码写成面向消费方的口径，含"Ledger 回退行恒定
  报 `null`"与"CCXT 缺 `side` 整条拒绝"两条生产者事实。
- `README.md` 新增**文档地图**（每份保留文档一句"什么时候读"）与**当前状态**的链路成熟度表，替换掉原来那段
  无法核对的能力长句和 V5 时代阶段表；演示输出块换成实抓文本。

### Verified（本轮实测，数字不来自旧文档）

| 事实 | 实测值 |
|---|---|
| workspace crate 数 = README 模块表行数 | 23（表内另列 `python/qianxing_ccxt`、`cpp/`）；脚本核对表内无缺项 |
| `qx-cli --help` 入口条数 | 50 |
| `builtin-strategies` | 17（13 单标的可走 builtin / ccxt-builtin / book；4 套利 kind 只被 multi-builtin 接受） |
| `deploy/*.json` 示例配置 | 52 |
| `tools/check_architecture.py` | `架构不变量自检全部通过 ✓（269 项）` |
| `qx-cli all`（进程内自校验） | 退出码 0，`全部自校验通过 ✓` + PaperVenue 契约行 |
| `backtest builtin macd deploy/qianxing.bar-frame.example.json` | `bars=70 fills=1`，产物四件套字段逐条读出 |
| 能力矩阵 | 18 个能力块；`implementation`+`code_tested` 双真 15；`sandbox_tested` 0；`production_approved` 0；证据 155 条 / limitation 52 条 |
| README 命令示例 ≡ help 入口 | 脚本比对：README 里所有 `-- <cmd>` 提及都能在 help 的 50 条中找到（0 例外） |
| 全仓 markdown 相对链接 | 14 份 `.md`，悬空链接 0 |

### Known issues（本轮没做）

- README 的快速开始命令表**没有门禁**：`check_architecture.py` 只钉「clap 命令表 ≡ 派发 ≡ help」，
  README/文档里的命令是 prose，漂移只能靠人工核对（本轮就是这么抓的）。
- 收口指南与 `deploy/README.md` 仍有重叠段（`storage.consistency` 三档、CCXT 凭据键、发布前检查），
  合并方向未定：要么把指南并成一篇教程、要么把 `deploy/README` 缩成配置键参考。
- 被删文档中的"三轮端到端链路审计"结论只留在 git 历史，未回填到 V11；如果以后要复用，从 commit 里取。

## Unreleased — V11 Q68：持仓行的未算钱不再印成 0，缺方向不再靠猜（2026-09-22）

交易链路实测（V11 #54）排到的第三颗：TX2 的下半截（持仓行）+ 实盘回报入口（TX2b），不在 §6 排期内。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §29。

### Fixed（交易链路：持仓行的三个钱字段与方向）

- **内核持仓观察的 `unrealized_pnl` / `initial_margin` / `maintenance_margin` 改 `Option<Money>`**
  （`crates/qx-core/src/event.rs:187-191`，各带 `#[serde(default)]` 让老日志缺键读成未报）：改前三者是裸
  `Money`，摘要里 `h.write_i128(position.unrealized_pnl.raw())`（HEAD `:451-453`）——"这个交易所不报维护保证金"
  与"报了零"在类型、在 `Event::digest`、在线格式上是同一份状态。现在摘要逐字段先写存在性标记再写值（`:463-466`）。
- **线格式行的两列钱改 `Option<i128>`，`Default` 不再兜 0**（`crates/qx-protocol/src/wire.rs:34-35`、`:46-47`）：
  任何 `..PositionSnapshot::default()` 起步的构造不再自动"报一个零"。**价格列刻意保持不变** —— 定点价格里 `0`
  不是合法值，钱没有这个性质；这条区分写进字段文档并由门禁钉住。
- **读模型的 Ledger 回退行不再替交易所报数**（`crates/qx-cli/src/api_service.rs:350-354`）：改前是写死的
  `unrealized_pnl_raw: 0, margin_raw: 0`（HEAD `:332-333`），而 Ledger 里没有这两个量（要按冻结的 market spec
  逐标的算，读侧没有规格来源）——一个上涨 5% 的现货仓位会长期报着"没有浮亏"。
- **CCXT 持仓方向不再靠猜**（`crates/qx-cli/src/ccxt_facts.rs:110-122`）：改前
  `.unwrap_or("long")`（HEAD `:109`），而上游连接器（`python/qianxing_ccxt/__init__.py:1240`）在交易所不给方向时
  印的是字面量 `"unknown"` —— HEAD 的实际行为是数量按**正数**入账、`position_side` 记 `"unknown"`，同一份观察里
  数量与方向自相矛盾。现在只接受 `long`/`short`，其余连 symbol 带"读到什么"一起拒绝；零数量行仍在闸门**之前**跳过。
- **CCXT 三项钱读成"没报"而不是零**（`:146-154`）：改前缺键与 `null` 一律 `Ok(Money::ZERO)`（HEAD `:131-140`），
  即生产方说"没报"、消费方改口说"零"。守卫侧（`crates/qx-runtime/src/pipeline.rs:1649-1657`）改成"报了才判"，
  不报保证金的交易所不会被判非法回报。
- **折算与哈希收口到共用写法**：行哈希与账户标量共用 `write_optional_money`（`crates/qx-protocol/src/lib.rs:236`、
  `:287-288`），`null` 的写法全仓唯一（`:248` `money_json`），线格式没有的那一列保证金折算回 `None` 而不是 0。

### Added（用例与门禁）

- 8 条新行为用例，全部走真实入口：`crates/qx-cli/src/tests/ccxt_position_facts_honesty.rs`（146 行 3 条——方向
  fail-closed、平仓位先跳过、省略/`null`/`"0"`/`0` 四态区分）、`api_snapshot_money_fields.rs` 加 2 条行侧
  （回退行缺席、venue 报的行保留两态）、`event.rs` 的摘要两态用例、`snapshot_single_source.rs` 加 3 条
  （折算两态、行两态逐个字段跑哈希/seal/两种线格式往返/增量双向、`Default` 不报钱）；`pipeline.rs` 落盘回读用例
  加第二行，让"报了零"与"没报"同时出现在一份快照里并要求回读后不收敛。
- `tools/check_architecture.py:2125` `position_money_honesty_check()` 13 项（静态门禁 256 → **269**）：内核
  Option 形状、摘要标记唯一、行字段形状**且价格列保持 0 哨兵**、折算与槽位各只有一处写法、全仓逐行禁止给这五个
  字段兜 `0`/`Money::ZERO`、方向闸门禁抄列表、跳过点必须先于闸门、守卫的 `is_some_and` 序列、八处用例按
  `fn NAME(` 取证。`CLI_TEST_FLOOR` 173 → **178**；行数棘轮重登记 `event.rs` 674→749、
  `qx-protocol/src/lib.rs` 856→885、`pipeline.rs` 2349→2391。

### Changed（有意的破坏性后果）

- `Event::digest` 的形状变了（多一次存在性标记）：**含 venue 持仓观察的既有事件日志**会在 manifest 核对处被拒
  （`crates/qx-storage/src/lib.rs:307-309`、`crates/qx-storage/src/sqlite.rs:2104`）。这是有意的 fail-closed ——
  旧日志里"没报"与"报零"本来就是同一份内容，无法事后区分。本轮实测被跟踪的产物侧不含这类日志
  （`deploy/` 中 `AccountPositionSnapshot`/`account_positions`/`unrealized_pnl`/`"digest"` 各 0 命中，
  `MODIFIED_DEPLOY=0`），但真实跑过 paper/CCXT 的用户目录会受影响，升级路径尚未设计。

### Verified（两轮门禁，数字取自 `/tmp/qx_q68_gate2.log`）

- 20:54:05 → 21:00:10：`cargo check --workspace --all-targets` `=0`；`rustfmt --check` `=0`；`cargo clippy`
  四个 crate 全 `=0`；7 条行为变异 + 1 条声明型 + 4 条静态变异全部 `MUT_STATE=red` 且各只点亮被测项
  （`OTHER_FAILS=0`、`MUT_STATE=unexpected` 0 次），12 次还原全部 `RESTORE_EXACT`；设 `QX_PYTHON` 的整树
  `731 passed / 0 failed`，不设 `729 passed / 2 failed`（本机无解释器的两条已知 Python worker 契约项）；
  `Ran 43 tests` Python 侧 `OK`；`MUTATION_RESIDUE=` 空。
- 首轮 `/tmp/qx_q68_gate1.log` 作废重跑，原因有三条（都记在 §29.3）：`MQ68a` 摘 `#[serde(default)]` 行为不变
  （serde 对 `Option<T>` 缺键本就补 `None`）→ 改登记为声明型；`MQ68d`（`Default` 兜零）当时没有任何用例失败 →
  补钉构造入口的用例；`MQ68h` 第一版 needle 与原守卫语义等价、不是可达缺陷 → 换成 `map_or(true, …)`。

### Known issues（本轮实测钉出的，交给下一轮）

- 持仓行的浮亏仍然**没有生产者**（只是不再撒谎）：要先把冻结的 market spec 送到读侧。账户级五个字段同样仍无生产者。
- `crates/qx-cli/src/ccxt_facts.rs:195-198` 资金费的 `timestamp_ms` 缺失仍回落 `0`（本轮只收了持仓侧）。
- ~~CCXT 对账的两半仍不相交（in-loop 发现只进事实流、`open_order_issues` 只进报告，worker 在已有单子"结果未知"时
  仍标 `Ready`）；`binance_reconcile.rs:154-156` 三个覆盖度计数恒 0~~ —— 两条均已由同日的 Q69 关闭（见顶部与 §31）。

## Unreleased — V11 Q67：账户快照不再把"没算过的钱"印成 0（2026-09-22）

交易链路实测（V11 #54）排到的第二颗：账户级读模型 TX2，不在 §6 排期内。收口记录见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §28。

### Fixed（交易链路：七个汇总钱字段里六个没有生产者）

- **`available_raw` 不再是 `equity_raw` 的副本**：改前 `crates/qx-cli/src/api_service.rs:273` 就是
  `snapshot.available_raw = snapshot.equity_raw;` —— 把已经压在持仓上的那段钱说成可自由花掉。现在取本条快照记账
  的那一本结算账簿现金 `cash_for(account_id, settlement_currency())`（`:276`）。
- **`fees_raw` 与同一条快照的逐笔成交同源**：此前账户级恒 0，而同一份 JSON 的 `fills[].fee_raw` 逐笔是真数。
  现在由这些 `fee_raw` 以 `checked_add` 加出（`:296-302`），**合计溢出即拒绝发布这份快照**而不是印一个回绕过的
  数（用例把账户现金精确压到 `i128::MIN`，证明溢出只可能出现在读模型这一侧）。
- **算不出来的钱在协议上必须能缺席**：`AccountSnapshot` 的七个汇总钱字段由裸 `i128` 改 `Option<i128>`（权益仍
  是恒算得出的 `i128`）。改前 `frozen_raw` 在 `git grep` 下只命中 `crates/qx-protocol/src/lib.rs` 一个文件
  ——生产者数量为零，每条对外快照都在宣布"这个账户没有冻结资金"。`margin`/`realized_pnl`/`unrealized_pnl`/
  `funding` 同样只有读法。现在 `new()` 一律 `None`，稳定 JSON 与线格式印 `null`，`/account/balances`
  （`crates/qx-api/src/lib.rs:1459-1460`）随之从 `map(…)` 改 `and_then(…)`。
- **`None` 与 `Some(0)` 是两份状态**：`state_hash` / `scalar_hash` 共用的那一次标量写入先写存在性标记
  `write_u64(u64::from(value.is_some()))` 再写数值（`crates/qx-protocol/src/lib.rs:235`），八个钱的取法收敛到
  `scalar_money_raw()`（`:220`）唯一一处，稳定 JSON 槽位只由 `scalar_json_values()`（`:242`）填。未算状态再也
  改不出一个"看起来算过"的 0。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/api_snapshot_money_fields.rs`（253 行，4 条，走真实 paper 入口与真实读模型）：结算
  账簿口径、费用与逐笔成交同源、未算缺席、费用合计溢出即拒。
- `crates/qx-protocol/tests/snapshot_single_source.rs` 新增 `uncomputed_money_is_not_the_same_state_as_computed_zero`：
  七个字段逐个跑"哈希不同 → seal/validate 自洽 → 稳定 JSON `null` vs `0` → 两种线格式往返保住区分 →
  `diff`+`apply` 两个方向都是一次真实改动"。`qx-api` 新增端点用例锁 `margin_raw: null`。
- `tools/check_architecture.py:1918` `snapshot_money_honesty_check()` 11 项（静态门禁 245 → **256**）：Option
  形状、取法唯一、两处哈希共用、存在性标记、槽位唯一写法、"权益副本"禁抄（含改类型后**仍可写出**的四种形状）、
  available 取结算现金、费用 checked 加总、五个未算字段在任何产码里都不得出现赋值写法、端点两处 `and_then`、
  六条用例按 `fn NAME(` 取证。`CLI_TEST_FLOOR` 169 → **173**；`maturity/line_budgets.yaml` 重登记
  `qx-protocol/src/lib.rs` 856 → 874、`qx-api/src/lib.rs` 3137 → 3160。
- 门禁自查补强两处（同一轮）：新增的一条 `available` 兜底分支因 `LiveEventPipeline::open` 已拒空结算币种而是
  死分支，删除；"权益副本"禁抄项原来盯的字面量在字段改 `Option<i128>` 后**根本编译不过**，换成可写出的形态，
  `MQ67b` 实测一次点亮 2 条具名项。

### Verified（两轮门禁，数字取自当轮日志）

- `/tmp/qx_q67_gate2.log`（19:46:49 → 19:50:32）：`cargo check --workspace --all-targets` `=0`；`rustfmt --check`
  `=0`；`cargo clippy --all-targets -- -D warnings` 三个 crate 全 `=0`；6 条行为变异 + 3 条静态变异全部
  `MUT_STATE=red` 且各只点亮被测项（`MUT_STATE=unexpected` 0 次），9 次还原全部 `RESTORE_EXACT`；
  设 `QX_PYTHON` 的整树 `722 passed / 0 failed`，不设 `720 passed / 2 failed`（本机无解释器的两条已知 Python
  worker 契约项）；`Ran 43 tests` Python 侧 `OK`；`MUTATION_RESIDUE=` 空。
- 首轮 `/tmp/qx_q67_gate1.log` 唯一非绿项是 `STAGE4_CLIPPY[qx-cli]_EXIT=101`（新增用例里的 3 条 lint），第二轮
  修复后归零。

### Known issues（本轮实测钉出的，交给下一轮）

- **同一份 JSON 的持仓行仍犯同一个错**：`api_service.rs:340-351` 走 Ledger 回退时写死
  `unrealized_pnl_raw: 0, margin_raw: 0`，协议上这两项也是裸 `i128`（TX2 下半截）。
- **CCXT 事实折算把缺字段读成零/多头**：`crates/qx-cli/src/ccxt_facts.rs:106-110` 缺 `side` 默认 `"long"`、
  `:131-140` 缺失或 `null` 的三项保证金/盈亏一律 `Money::ZERO`、`:181-184` 缺时间戳回落 0。
- ~~**CCXT 对账的两半不相交**：`ccxt_reconcile_worker.rs:249`/`:283` 的发现只进事实流（把订单推到
  `OrderStatus::Unknown`），不进报告也不参与健康判定 → 已有单子"结果未知"时 worker 仍标 `Ready`；
  `open_order_issues` 反之只进报告。`binance_reconcile.rs:154-156` 三个覆盖度计数恒 0~~ —— 已由同日的 Q69 关闭（见顶部与 §31）。

## Unreleased — V11 Q66：回测产物声明"跑的是哪一份输入"，这句话由别人重算得出（2026-09-22）

Q1b（可复现性绑定）的第一批，不在 V11 §6 排期内。收口记录见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §27。

### Fixed（回测链路：输入身份是引擎自哈希，摘要里根本没有）

- **`RunManifest.data_fingerprint` 在无数据集绑定时回落成引擎自哈希**：取的是 `report.input_data_hash`
  （`qx-guanxing::bars_digest`：条数 + ts/OHLCV，连 instrument 都不含），"输入身份"这一行答的是"引擎当时看见
  一串数字"。改前实测 `deploy/data` 里那份 OKX 示例 run.json 的 `data_fingerprint` 是 `d1eae507fc0a5d80`，与同
  文件的 `input_event_hash` 一字不差。现在回落取 `"{kind}:{被复核过的指纹}"`，那条示例的
  `data_fingerprint` 变成 `barframe:673fd995f8625b97`（产物文件名随之从 `…OKX-4bdd4398c79c6138` 换到
  `…OKX-5c38b56746b08948`）；旧形状 `format!("{:016x}", report.input_data_hash)` 全仓 0 处。
- **摘要不携带输入身份，而被注册表复核过的那份身份被打印后丢弃**：策略链跑完会拿到
  `DatasetManifest { dataset_id, version, fingerprint }`（`JsonDatasetRegistry::verify` 逐字节校验过），此前只
  印到 stdout。现在它随 `input` 块落进摘要（schema_version 2 → **3**），换一份同形状的输入文件再跑
  `qx report` 会直接拒绝，而不是像此前那样毫无反应。
- **读点唯一，声明只能是读出来的**：`crates/qx-cli/src/backtests/artifacts.rs:28`
  `read_bar_frame_for_backtest` / `:66` `read_depth_frame_for_backtest` 是全仓唯一两处解帧，`:37`
  `barframe_dataset_identity` 把帧喂回 `qx-data` 的 `JsonBarFrameProvider` 取回被复核过的身份并逐列比对；
  `:117` `recompute_declared_backtest_input` 用**同一个读点**重算，`dataset_id` / `dataset_version` /
  `fingerprint` 任一不符即拒（先整体校验声明块，`kind` 不认与缺键是两种不同的拒因）。
  `config_commands.rs:258` 以 `?` 传播复核失败，文本侧 `input_verified=<路径>`、旧 schema 只能印成
  "没有 input 块，输入身份未经核对"（未声明 ≠ 通过）。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/backtest_input_provenance.rs`（286 行，6 条，全部走真实入口）：声明等于重算、跑完
  篡改输入即拒、输入文件消失即拒、同一注册身份不容两种内容、深度链同形、无 `input` 块不算通过。
  `backtest_replay_gate.rs` 把摘要形状钉到 v3。
- `tools/check_architecture.py:1628` `input_provenance_check()` 9 项 + 6 条 `BACKTEST_ENTRY_OWNERS` 归属项
  （静态门禁 230 → 245）：`input` 块只写一次且 schema 为 3、三条链不自己解帧、身份步骤同时具备 Provider 读取
  与列式一致性、复核函数体形状、报告出口传播失败并区分未声明、回落不用引擎自哈希、两档版本号互不相同且字面量
  只在 `artifacts.rs`、六条用例按 `fn NAME(` 取证。
- `CLI_TEST_FLOOR` 163 → 169；`maturity/line_budgets.yaml` 重新登记 `config_commands.rs` 596 → 622。

### Known issues（本轮实测撞到的，交给下一轮）

- 产物文件名只由 `run_id` + `manifest.digest()` 决定，而 `digest()` 覆盖输入与模型、**不含摘要自身的
  schema 版本**。因此"只改摘要形状"的变更会让同一条示例命令在原地撞旧文件：本轮 STAGE8 三条集成用例
  （`builtin_signal_from_config.rs:292`、`fast_backtest_manifest.rs:65` / `:96`）就是如此，报"同一回测回测摘要
  路径已存在不同内容"。把 12 份改动前的本地 v2 产物移到 `%TEMP%\qx_q66_stale_v2_artifacts\` 复跑后
  `716 passed; 0 failed`。闸门本身没做错，缺的是一次真正的决策：摘要世代进文件名，或给示例集一条重 bless 命令。
- 被跟踪的 16 份示例摘要仍停在 `schema_version: 1`、`input` 块 0 份，当前代码在那些路径上不再落盘
  （本轮 `MODIFIED_DEPLOY=0` 是这个意思）—— 与 Q62 记下的
  `tracked_sample_artifacts_are_validated_by_nothing` 同一条，且随每轮变宽。

## Unreleased — V11 Q63：规格来源改成"实际读成的形状"，不再按"有没有传路径"印（2026-09-22）

同样不在 V11 §6 排期内，是"交易链路和回测链路还有哪些潜在问题，全部修复"这条实测指令的回测链路第六批
（FN6）。收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §26。

### Fixed（回测链路：产物上那行规格来源声明了错误的解析口径）

- **`instrument_spec_version` 问的是"有没有传 `--market-spec`"**：传了路径一律印 `ccxt-market-spec-v1`，于是
  仓库自己生成的产品规格 `deploy/qianxing.binance.spot.spec.json`（实测其键里带 `base_currency`，A 股那份带的是
  `base`）在产物里被写成 CCXT 形状。规格内容不进 `model_fingerprint`，`contract_size` 与三项定点精度也不进，
  这个字段是唯一交代"按哪种形状读的"的地方 —— 两种形状、两个解析分支顶着一个标签，等于声明两套记账口径是
  一回事。可达路径就是首屏命令：`init_project.rs:186` 给非 A 股 profile 印的那条回测命令带的正是这份产品形状
  文件。
- **判据与标签问同一个问题**：`crates/qx-cli/src/market_spec.rs:9` `market_value_is_product_spec`（唯一判据
  `base_currency` 存在）与 `:18` `market_spec_source_label` 同一处，`:26`–`:28` 三档常量各一名；`:48`
  `market_spec_from_value` 的解析分支复用同一个谓词，标签与实际解析不可能各答一份形状。
- **来源只在 loader 问一次**：`backtests/mod.rs:84` `market_spec_with_margin` 返回
  `MarketSpecLoad { spec, margin, source }`（结构体住在 `market_spec.rs:34`），`:108` 是全仓唯一一次
  `market_spec_source_label` 取用，"没给规格"那一档由 loader 独占。策略链与深度链以字段简写原样写进
  `RunManifestIdentity`（`single_strategy.rs:162`、`depth.rs:182`），不落摘要的内置链把它印成
  `[Builtin · Execution] … spec_source={}`（`single_strategy.rs:431`）。多腿链 `source: _`：一条命令两份帧、
  两条腿各一规格路径，产物上没有"这一份规格"可标，本轮不给它编一个。
- **示例集为什么看不出这条缺陷（如实记录）**：`deploy/data` 下带这个字段的 21 份 run.json 逐份读过 —— 被跟踪的
  18 份里 3 份 `ccxt-market-spec-v1` 全部来自真 CCXT 形状的 A 股规格（换口径前后同一标签）、15 份
  `default-instrument-spec-v1`（没传规格）。**没有一份 blessed 产物走过产品形状**，所以
  `MODIFIED_DEPLOY=0` 不是"改得没影响"，而是示例从没跑到这一档。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/market_spec_single_source.rs`（305 行）：`published_spec_source` 从落盘摘要顺着
  `run_manifest` 读回那一行；策略链与深度链各一条用例走**真实入口**跑三档可达输入，逐档断言标签并断言
  "三种输入给出三种来源"。
- `tools/check_architecture.py` 新增 `market_spec_source_check` 9 项（静态门禁 221 → 230）：谓词/标签各唯一且
  标签复用谓词、`base_currency` 判据只出现一次、三档常量取值互不相同、来源标签只在 loader 单点被问、
  兜底档由 loader 独占、两条链 **`RunManifestIdentity` 块内**只可能是 loader 返回的那个变量（新增
  `_identity_block` 按花括号配对取块）、builtin 那行占位符与实参一一对应、两条行为用例按 `fn NAME(` 取证。
- `CLI_TEST_FLOOR` 161 → 163。

门禁：`/tmp/qx_q63_gate2.log` 单轮通过 —— `cargo check` / `rustfmt --check` / `clippy -D warnings` 全 0、
行数棘轮 diff 为 0；3 条行为变异各红 1–2 条用例（MQ63a/MQ63b 同时各点亮 1 条具名静态项），7 条静态变异各
点亮 1 条（`MUT_STATE=unexpected` 0 次），10 次还原全 `RESTORE_EXACT`、`CONCURRENT_TREE_CHANGE` 0 次、
`MUTATION_RESIDUE` 空；全量测试设 `QX_PYTHON` `710 passed / 0 failed`（exit 0），不设
`708 passed / 2 failed`（本机无解释器的两条已知 Python worker 契约项）；Python 侧 `Ran 43 tests … OK
(skipped=1)`。第一轮日志作废：那条"链必须原样写 loader 来源"的判据被解构行 `source: instrument_spec_version,`
满足了子串，两个缺陷本体变异静态侧全绿 —— 作用域收到 identity 块内后同一对变异才咬住（§26.3）。MQ63c（产品
形状那一支接错常量）**静态全绿、只有行为用例能问出来**，§26.5 记了这条边界。另记一项环境事实：本机 venv 缺
`pyproject.toml` 已声明的 `tzdata`，导致 13 条 A 股 Python 用例长期被一个导入错误挡在门外（原基线读到
`Ran 30 tests … errors=1`）。

本轮未使用任何外部服务或凭据，`sandbox_tested` 全部保持 `false`。

## Unreleased — V11 Q62：重放改成真会失败的重新驱动，产物不再声明恒等（2026-09-22）

同样不在 V11 §6 排期内，是"交易链路和回测链路还有哪些潜在问题，全部修复"这条实测指令的回测链路第五批
（FN5）。收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §25。

### Fixed（回测链路：产物里那条"重放校验"在任何输入下都不可能失败）

- **`replay_hash` 是把同一段事件切片再哈希一遍**：`ReplayVerifier::rebuild_from` 与 `result_hash` 来自
  同一批事件，于是两者恒等。`deploy/data` 下被跟踪的摘要里有 16 份仍带着这个键，逐份读进来比对
  **16/16 相等** —— 一份都不例外。写在产物上像"结果被独立重算过一次"，实际什么都没问。
- **重放只有一个内核，做的是重新驱动**：`crates/qx-core/src/sourcing.rs:325` 的 `replay` 逐条
  `append_checked`（重复 seq、乱序、缺因果字段当场报错）+ 把 `LedgerApplied` 事实重新入账 + 整本
  `validate()`；`:349 replay_facts` / `:359 rebuild_ledger` 都是它的投影，`:368 verify(events, &Ledger)`
  再加一条真会失败的判据（重放账簿条数 ≠ 运行账簿条数即报 `replayed=/run=`）。`rebuild_from` 与
  `replay_hash` 全仓删除，门禁按指纹钉住不得复活。
- **闸门排在结果出口**：`qx-xingban` 两条链的 `run()` 各以
  `ReplayVerifier::verify(report.event_log.events(), &report.ledger)?;` 收尾（`backtest.rs:1049`、
  `orderbook_backtest.rs:742`），重放不过就不返回报告；`qx-cli` 落盘侧再问一次且问在写任何工件之前
  （`backtests/artifacts.rs:107`，首个写文件在 :164），另加 `replay.log_digest != result_hash` 即拒。
- **摘要形状换成可核对的三个事实**：`schema_version` 升到 2，`replay_hash` 键删除，代之以
  `"replay": { log_digest, events, ledger_entries, run_ledger_entries }`；`qx-cli report` 打印
  `replay_log_digest=` 与 `replay_events=` / `replay_ledger_entries=n/m`，读者能自己数而不必接受一个哈希。
- **顺带修掉深度链一条从没被问过的缺陷**：订单簿链的 `EventLog` 此前没做过规范序校验，接上引擎闸门后
  当场报错 —— 迟到成交回报（口径 `match-previous-submit-current`，因果上来自更早的提交）与账簿事实排在
  `Priority::APPLY`，会让同一时间戳上的策略 `COMMAND` 因果倒退。改占 `FEEDBACK` 槽
  （`orderbook_backtest.rs:434`、`:450`），初始入金排 `TIMER`（`:401`）。改的是引擎发出的事实序。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/backtest_replay_gate.rs`（162 行 / 3 条进程内用例）：摘要写的就是它查过的三个
  事实（并断言 `replay_hash` 键已消失）；账簿凭空多一条无事实支撑的条目 → 拒落盘且不产生任何文件；
  事实流同戳倒退 → 同样拒落盘。
- `backtest.rs` 的 `assert_replay_matches` helper（定义 + 两处调用）把原先两处"哈希相等"的自证换成逐条
  比对重放账簿，并在因果用例里加一条"凭空多记必须报错"的反向证据；`ashare_pit_asof.rs` 改走同一内核。
- `tools/check_architecture.py` 新增 `replay_kernel_check` 11 项（静态门禁 210 → 221）：内核四函数各自
  唯一、内核体三步齐、报告侧条数判据在位、`rebuild_from` / `"replay_hash":` / `fn replay_hash(` 指纹为 0、
  两条链报告出口各问一次且以 `?;` 向上抛、落盘点问在首个写文件之前、摘要 schema 与三个 replay 键、
  深度链三个因果槽位计数、两侧用例名按 `fn NAME(` 取证。
- `CLI_TEST_FLOOR` 158 → 161；`sourcing.rs`（567 行）新登记进行数棘轮，`config_commands.rs` 590→596、
  `strategy_host.rs` 812→801 一并快照。

门禁：`/tmp/qx_q62_gate3.log` 单轮通过 —— `cargo check` / `rustfmt --check` / `clippy -D warnings` 全 0；
11 次门禁变异各点亮 1 条具名条目（`MUT_STATE=unexpected` 0 次），4 条行为变异各红 1–3 条用例、
`RESTORE_EXACT` 11 次、`CONCURRENT_TREE_CHANGE` 0 次、`MUTATION_RESIDUE` 空；全量测试设 `QX_PYTHON`
`708 passed / 0 failed`（exit 0），不设 `706 passed / 2 failed`（本机无解释器的两条已知 Python worker 契约项）。
MQ62e（内核整本 `validate`）、MQ62h（入金槽位）、GA62e（`.ok();` 吞失败）三项**行为不可证伪、只由文本钉住**，
§25.3 记了原因与补法。

本轮未使用任何外部服务或凭据，`sandbox_tested` 全部保持 `false`。

## Unreleased — V11 Q65：A 股段在五个提交入口当场拒，拒在任何副作用之前（2026-09-22）

同样不在 V11 §6 排期内，是"交易链路和回测链路还有哪些潜在问题，全部修复"这条实测指令的交易链路第一批
（TX1）。收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §24。

### Fixed（交易链路：`strategy.ashare_rules_path` 在提交侧有声明、没有读者）

- **五个会提交新订单的入口此前都收下 A 股段再按无制度下单**：`paper-worker`（含 `paper-e2e` /
  `paper-check`）、`paper-submit-order`、`binance-worker`、`binance-submit-order`、`ccxt-worker` 走
  `read_runtime_config` 读得到整段配置，却没有任何一处读它 —— 订单照样 T+0、照样可以 150 股买入，
  产物与 stdout 上不显示这段被认了还是被丢了。Q61 只把回测侧那一半收干净了。
- **明确选择"整段拒"而不是"只接费率"**：费率确实有现成挂钩点（`execution_cost_binding_from_config`），
  但只接费率会让产物印着"A 股佣金"而制度项全空，比整段不认更容易骗人。缺的那一项写进拒绝文案
  （`ASHARE_SUBMIT_GAP`：T+1 需要"今日买入"的结算状态、整手与涨跌停要按板块和昨收逐单判定），
  并指向当前唯一能认这段的入口 `strategy backtest`。
- **闸门只有一处定义**：`crates/qx-cli/src/runtime_wiring.rs` 的 `reject_ashare_rules_on_submit_path`
  把 `--config` 转交回测侧唯一的 `reject_ashare_rules_config`，判据是"这条路径会不会把一笔新订单送进
  Venue" —— `Execution` 与 `SpreadRecovery` 算（补腿本身就是新订单，V10 Q57），Strategy / MarketData /
  UserStream / Reconciler / Scheduler / Api 不算，否则"研究用配置"连启动都做不到。
- **顺序就是危害边界**：`run_paper_submit_order` 的闸门是函数第一条语句，排在读配置与读命令之前；
  `binance-submit-order` 排在读命令之前；`ccxt-worker` 排在解析 CCXT 配置文件（再往后要起 Python 进程）之前。
- **命令面帮助改口**：五条提交入口各自写明"配了 `strategy.ashare_rules_path` 即在任何副作用之前拒绝"，
  并写明行情/策略角色不受影响。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/ashare_submit_guard.rs`（198 行，5 条进程内用例，走真实入口函数）：三个一次性
  提交入口 + `paper-worker` 的拒绝，每条都配"同一夹具只摘掉那三个 A 股键"的基线，证明红的原因是键而不是
  夹具；断言里点名"报错不能是读命令那一句"与"被拒入口不得留下 `data` 目录"，把副作用顺序也钉住。
- `only_roles_that_submit_orders_meet_the_ashare_gate`：2 个提交角色必须被拒、6 个非提交角色必须放行 ——
  闸门范围漂移的两个方向各有一条用例红（漏 `SpreadRecovery` 与扩到所有角色都落到这一条上）。
- `tools/check_architecture.py` 的 `ashare_submit_guard_check` 新增 9 项（静态门禁 201 → 210 项）：闸门与
  角色判定唯一定义、复用回测侧文案且点名 T+1、五个入口的调用点逐一计数、`venue_runtime` 不得自己装配
  A 股规则或费率、五条行为用例按 `fn NAME(` 取证、帮助文本口径在位。Q61 的两项锚点同轮升级为 `fn NAME(`。
- `CLI_TEST_FLOOR` 由 59 抬到实测的 158 条（地板停在历史值等于没有防守）。

门禁：`/tmp/qx_q65_gate.log` 单轮通过 —— `cargo check` / `rustfmt --check` / `clippy -D warnings` 全 0，
七个行为变异各红 1 条用例、十个门禁变异各点亮 1 条具名条目（`MUT_STATE=unexpected` 0 次），
`RESTORE_EXACT` 17 次且 `MUTATION_RESIDUE` 为空，全量 `cargo test --workspace --all-targets`
设 `QX_PYTHON` 时 `702 passed / 0 failed`。本轮未使用任何外部服务或凭据，`sandbox_tested` 保持 `false`。

## Unreleased — V11 Q64：四个内置信号参数五链同源，复检排在印口径之前（2026-09-22）

同样不在 V11 §6 排期内，是"回测链路还有哪些潜在问题，全部修复"这条实测指令的第五批（FN7）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §23。

### Fixed（回测 + 交易链路：`--config` 的 `strategy.builtin_*` 有声明、只有一个读者）

- **四条回测链此前各自走 `BuiltinStrategyConfig::new` 的 5/20/14/100 写死默认**：`backtest builtin` /
  `ccxt-builtin`、`backtest book`、`backtest multi-builtin` 都收下 `--config`，也真读它做风控与成本，
  唯独 `builtin_fast_window` / `builtin_slow_window` / `builtin_period` / `builtin_threshold_bps` 没有
  落点 —— 同一份配置在 `strategy backtest` 与内置链上跑的是两套信号，stdout 形状还一样。
- **赋值收成一个点**（`crates/qx-cli/src/strategy_binding.rs` 的 `builtin_signal_source` +
  `apply_builtin_signal_overrides`）：全仓没有第二处 `config.fast_window = …`。包装函数
  `apply_configured_builtin_signal`（`single_strategy.rs`）被四条链共问一次。
- **覆盖之后必须复检，且复检要早于生效口径那一行**：运行时体检只比较"两个都给齐"的窗口键，
  单边覆盖（只写 `builtin_fast_window: 25`、慢窗留默认 20）只有并入默认值之后才成为非法组合。
  复检现在由 `apply_configured_builtin_signal` 与 `builtin_strategy_config_from_runtime` 各做一次，
  报错文案点名配置路径与覆盖后的四项值；非法配置一律 `exit=2` 且 stdout 不出现 `[X · Signal]` ——
  先宣告再报错等于往 stdout 写了一套没跑过的参数。
- **四条链各印一行生效口径**：`[Strategy · Signal]` / `[Builtin · Signal]` / `[Depth · Signal]` /
  `[Multi · Signal]`，文案由唯一的 `builtin_signal_note` 生成（`source=config|builtin-default` + 四项值），
  读者不用猜本轮到底用的是配置还是默认。
- **paper/live worker 同读**：`invoke_builtin_strategy` 走的正是 `builtin_strategy_config_from_runtime`，
  本轮为交易链路补上进程内用例，"配置在 worker 侧也生效"不再只由回测链证明。
- **命令面帮助改口**：四个信号键在 `backtest builtin` / `book` / `multi-builtin` 的说明里写明在本链生效、
  并会印出生效口径。

### Added（用例与门禁）

- `crates/qx-cli/tests/builtin_signal_from_config.rs`（442 行，7 条真实子进程用例）：窗口对换出成交、
  周期与阈值各自三档给出三个不同 `result_hash`、非法窗口组合 fail-closed、两条 Bar 链同源、深度档与
  多腿链同样认账，以及新增的单边覆盖复检用例（同时跑三个入口，断言报错文案与"口径行未印出"）。
- `crates/qx-cli/src/tests/strategy_worker_entries.rs` 的 `builtin_worker_reads_the_declared_signal_parameters`：
  在克隆配置上自设 2/3/9/50 与"四项全缺回落 5/20/14/100"两档 —— 示例配置写的恰好就是默认值，
  拿它做"配置值"对比的用例在实现完全没读配置时也会通过。
- `tools/check_architecture.py` 的 `builtin_signal_check` 新增 15 项（静态门禁 186 → 201 项）：赋值点全仓
  唯一、四条链各问过它、复检形状钉在 `config.validate()`、判词单一定义 + 四处调用、帮助点名四个键、
  七个行为用例按 `fn NAME(` 取证、交易链路侧一条进程内用例在位。

## Unreleased — V11 Q61：A 股段在四条回测链上要么生效、要么当场拒（2026-09-22）

同样不在 V11 §6 排期内，是"回测链路还有哪些潜在问题，全部修复"这条实测指令的第四批（FN4）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §22。

### Fixed（回测链路：`--config` 里的 A 股段有声明、无读者）

- **`backtest builtin` / `backtest ccxt-builtin` 收下 `--config` 却不读
  `strategy.ashare_rules_path|ashare_actions_path|ashare_calendar_path`**：同一份配置在
  `strategy backtest` 上按 T+1、整手、涨跌停与 A 股佣金跑，在内置链上退成"无交易制度 +
  maker/taker 兜底费率"，两边 stdout 形状完全相同。
- **读法收进单一模块**（新文件 `crates/qx-cli/src/backtests/ashare_binding.rs`，118 行）：
  配对（只给 actions/calendar 即错）、fail-closed（配了快照必须 `enabled: true`，JSON 非法或
  `validate()` 不过整轮失败）、规则与 `AShareFeeModel` 成对进入装配。两条 Bar 链各自绑定
  （策略链走已解析的三元组、内置链走 `--config` 路径），成本来源改写为
  `source=ashare-rules:{路径}`，并新增 `[Builtin · A 股规则] t_plus_one=… lot_size=… price_tick=…
  commission_bp=… stamp_duty_bp=… transfer_fee_bp=… path=…` 一行。
- **承不了这个段的两条链当场拒绝**（`reject_ashare_rules_config`）：`backtest book` 的 L1/L2
  盘口引擎没有 T+1/整手/涨跌停挂钩点；`backtest multi-builtin` 一份策略段套不住两条腿各自的交易制度。
- **命令面帮助改口**：`backtest builtin` / `ccxt-builtin` 的 `[--config <runtime.json>]` 写明读取的是
  策略口径、配了就必须生效；`book` / `multi-builtin` 写明 A 股段在这里没有落点、配置了即整轮拒绝。
- **门禁脚本自身的死代码**：`check_architecture.py` 的"四个文件状态存储各只有一份定义"（`# (e)`）
  自 V10 P1c 起写在 `module_declarations()` 的 `return` 之后，语法合法而永不执行。搬回
  `storage_retry_check` 后静态门禁由 182 项变 186 项，四项当轮即 PASS —— 半个月里这条约束无人看守。
- **两处挂载判定由子串包含改为整行正则**（`mount_pair_present`，Phase 4p/4s 同口径）：把
  `pub(crate) use x::*;` 注释掉过去照样绿。
- **A 股快照解析点唯一性的作用域扩到 `crates/*/src/**`**（排除用例盘）：原来只扫 `backtests/` 目录，
  抄第二份读法到目录外不会红。

### Added（用例与门禁）

- `crates/qx-cli/tests/ashare_builtin_backtest.rs`（新文件，300 行，5 条真实子进程用例）：
  整手闸门改变结果、A 股佣金单独改变结果（成交笔数与拒单数全同）、`enabled:false` 与
  actions/calendar 配对缺失两处 fail-closed、`book` 与 `multi-builtin` 两处拒绝、
  `strategy backtest` 报同一条配对规则（证明校验确实住在共用加载器里）。
- `tools/check_architecture.py` 的 `ashare_backtest_binding_check` 6 项：解析点全仓唯一、
  `enabled` 判定单点且文案钉住、两条 Bar 链各绑规则与费率（`binding.fee` / `assembly.fee = ` 各 2 处）、
  `depth.rs` 与 `multi_builtin.rs` 各一处当场拒绝、Q61 行为用例逐条在位；新模块与新入口进登记表。

### Validation（本轮日志实测，`/tmp/qx_q61_gate2.log`）

- `cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`；`cargo fmt --all --check`
  `STAGE2_FMT_EXIT=0`；`cargo clippy -p qx-cli --all-targets` `CLIPPY_WARNING_LINES=0`。
- `cargo test -p qx-cli --test ashare_builtin_backtest` `5 passed; 0 failed`；
  `QX_PYTHON=… cargo test -p qx-cli --bins` `110 passed; 0 failed`。
- 整仓测试：设 `QX_PYTHON` 时 `WORKSPACE_WITH_QX_PYTHON_EXIT=0`、
  `SUM_with_qxpython_passed=689 failed=0`、`OK_SUITES=75`；不设时 `WORKSPACE_EXIT=101`、
  `SUM_without_qxpython_passed=168 failed=2`（本机 WindowsApps 占位桩那两条必然失败项）。
- 静态门禁：基线 `ARCH_FAIL_LINES=0`，收口 `FINAL_ARCH_EXIT=0`、
  `架构不变量自检全部通过 ✓（186 项）`。
- 变异成对（行为级 6 项，各红后还原即 `5 passed; 0 failed`）：MQ61a 内置链不读 A 股段 → 4 条失败；
  MQ61b 只接规则不接费率 → 2 条；MQ61c 深度档不拒 → 2 条；MQ61d 多腿链不拒 → 2 条；
  MQ61e `enabled:false` 放行 → 2 条；MQ61f 配对校验失效 → 3 条（覆盖两条 Bar 链）。
- 变异成对（静态门禁级 9 项，每项 `MUT_STATE=red` 且只点亮一项）：G1 解析点复制进同目录、
  G1b 复制进 `dataset_commands.rs`（目录外，本轮新增的作用域才咬得住）；G2 内置链不绑定、
  G7 费率件不装配，同点"两条 Bar 回测链各自绑定"；G3/G4 两条链的拒绝调用消失；G5/G5b 任一 Q61
  用例改名点证据项；G6 注释掉 `pub(crate) use ashare_binding::*;` 点挂载配对。
  G8 探针确认存储定义检查已回到活路径（`STORE_DEFINITION_ITEMS=4`、`G8_STORE_CHECK_LIVE=True`、
  `G8_DEAD_CODE_IN_HELPER=False`）。
- 收口：`PRISTINE_OK final`、每次还原 `RESTORE_EXACT`、`MUTATION_RESIDUE=` 空、`BUILD_final_EXIT=0`。
- 行数：`ashare_binding.rs=118`、`single_strategy.rs=435`、`depth.rs=272`、`multi_builtin.rs=478`、
  `cli_help.rs=176`、`tests/ashare_builtin_backtest.rs=300`、`check_architecture.py=2563`；
  `MODIFIED_TRACKED_FILES=32` 是 Q54–Q61 未提交成果的合计。

### Known（本轮记录、未修的相邻缺陷）

- **Q64**：`--config` 的 `strategy.builtin_*` 信号参数同样在内置链没人读（写死 5/20/14/100），
  同一份配置在两条 Bar 链上给出不同信号（实测 `fills=1 / 3efef64ed899097a` 对
  `fills=0 / 216c9e646a571444`）。与 Q61 同族，本轮刻意不同批修，以免"哪一处让结果变了"重新变模糊。
- **Q65**：A 股制度在 paper/live 交易链路无任何落点（反序列化点全仓 1 处且在回测侧、
  `validate_order` 只被回测引擎调用、`venue_runtime` / `runtime_wiring` / `qx-execution` 里 `ashare`
  零命中）。配置面依旧"配了不认"，下一轮要么接线要么按同一口径当场拒。

本轮未使用任何外部服务或凭据，`sandbox_tested` 全部保持 `false`。


## Unreleased — V11 Q60：A 股涨跌停锚到上一交易日收价，封死的板不再成交（2026-09-22）

同样不在 V11 §6 排期内，是"回测链路还有哪些潜在问题，全部修复"这条实测指令的第三批（FN3）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §21。

### Fixed（回测链路：涨跌停的锚锚错了对象）

- **`AshareRuleConfig::previous_close` 拿"上一根 Bar"当昨收**（HEAD 版 `ashare/trading.rs:28-35`：
  覆盖表 miss 就 `index.checked_sub(1)`）。日线数据上上一根恰好是昨天，缺陷因此不可见；
  分钟线上进来的是同一个交易日里 5 分钟前的收价 —— ±10% 的板被窄化成"最近 5 分钟 ±10%"，
  于是**涨停封死的那根分钟线照样买入成交**、跌停封死的照样卖出。
- **改后的锚（`ashare/trading.rs:40-51`）**：`previous_close_raw` 仍按当前 Bar 的 ts 优先
  （除权除息日的昨收只能由数据侧给，仓库内不产生该映射 —— `deploy/qianxing.ashare.rules.json:17`
  里它就是空的），否则向前扫到第一根跨日的 Bar，即上一交易日的**最后一根**（`.rev()` +
  `Self::day_key(bar.ts) != day`）；整个历史里没有上一交易日时返回 `None`，`blocks_fill` 因此不判板 ——
  宁可不判，也不能拿当天的价格当昨收，那会把板算成 ±0%。
- **引擎侧不改**：唯一调用点仍是 `backtest.rs:504`（`rules.previous_close(bars, i)`），
  本轮把它钉成"只有一个定义点、一个调用点"，防止有人在高保真链旁边再抄一份"上一根 Bar"。

### Added（门禁与用例）

- `crates/qx-xingban/tests/ashare_limit_anchor.rs`（新文件，2 条）：
  `a_sealed_intraday_limit_up_blocks_the_fill_when_anchored_to_the_session_close` ——
  昨天 14:55 收在 10.00、今天封死在涨停价 11.00 的那根分钟线上，买入单必须不成交且现金额不动；
  `anchoring_to_the_previous_bar_would_fill_on_that_same_board` —— 同一份行情、同一根封板线，
  只把锚换成"上一根 Bar 的收价 10.50"（经 `previous_close_raw` 注入，即缺陷口径），
  这单就在 11.00 成交。两条合起来才是"结果确实变化"的证据：光有前一条，改坏锚也可能被别的口径兜住。
- `crates/qx-xingban/src/ashare/tests.rs` 的 `limit_band_anchors_to_the_previous_session_close_not_the_previous_bar`：
  逐 Bar 验锚本身（首两根 `None`、跨日两根都锚 10.00），并证明按旧口径推出的 11.55 那条板
  不再拦得住同一笔买入。
- `tools/check_architecture.py` 的 `ashare_limit_anchor_check` 新增 4 项（169 → 173）：锚只有一个
  定义点与一个引擎调用点、覆盖表优先 + 跨日取该日最后一根、锚的口径与"覆盖表才是除权除息出口"
  写进代码文档、上述行为用例与单元测试在位。

### Validation（本轮日志实测，`/tmp/qx_q60_gate.log`）

- `cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`；`cargo fmt --all --check` `STAGE2_FMT_EXIT=0`；
  `cargo clippy --workspace --all-targets -- -D warnings` `STAGE3_CLIPPY_EXIT=0`、`CLIPPY_WARNING_LINES=0`。
- `cargo test -p qx-xingban --all-targets`：`CARGO_TEST_xingban_EXIT=0`、`SUM_xingban_passed=87 failed=0`
  （其中新用例 `2 passed` + 单元 `1 passed`；邻近的 A 股 PIT 链 `ashare_pit_asof` 仍 `3 passed`，
  说明锚的改动没有改口 PIT 语义）。
- 整仓测试：未设 `QX_PYTHON` 时 `CARGO_TEST_workspace_EXIT=101`、`SUM_workspace_passed=168 failed=2`
  （断言 `WORKSPACE_FAILED_WITHOUT_QX_PYTHON=2`，即本机 WindowsApps 占位桩那两条必然失败项）；
  设解释器后 `CARGO_TEST_WORKSPACE_WITH_PYTHON_EXIT=0`、`SUM_workspace_with_python_passed=684 failed=0`、
  `OK_SUITES=54`；含 doc-test `CARGO_TEST_WITH_DOC_EXIT=0`、`SUM_with_doc_passed=684 failed=0`。
- 静态门禁：`STAGE5_ARCH_EXIT=0`、`架构不变量自检全部通过 ✓（173 项）`、`STAGE5_ARCH_FAIL_LINES=0`、
  `STAGE5_ARCH_PASS_LINES=173`。
- 变异成对（cargo 级）：MQ60a（锚退回上一根 Bar）红在集成 `ashare_limit_anchor.rs:143` 与单元
  `ashare/tests.rs:361`，`1 passed; 1 failed` / `0 passed; 1 failed`；MQ60b（覆盖表失效）
  红在**另一条**集成用例 `:163` 与单元 `:377`；MQ60c（去掉 `.rev()`，锚落到整段历史的第一根）
  红在 `:143` 与单元 `:362`。三处还原后各自回到 `2 passed; 0 failed` 与 `1 passed; 0 failed`。
- 变异成对（静态门禁级）：G1 删"不复权"口径文案 → `[FAIL] 锚的口径与…写进代码文档`；
  G2 删 `.rev()`、G3 把覆盖表按错的 key 取 → 双双点亮 `昨收锚按上一交易日推导`；
  G4 在引擎里再抄一份锚 → `涨跌停的昨收锚只有一个定义点与一个引擎调用点 — 定义 1 处、引擎调用 2 处`；
  G5/G6 分别重命名集成/单元用例 → 点亮 `同一根封板线在两种锚下结果不同…`。
  六项 `ARCH_MUTATED_G*_EXIT=1` 且各自只亮一项（不牵连行数棘轮），`ARCH_RESTORED_G*_EXIT=0` ×6。
- 收口：`PRISTINE_OK` 20 处逐个 `cmp` 通过、`MUTATION_RESIDUE=none`、`FINAL_ARCH_EXIT=0`、
  `BUILD_final_EXIT=0`。
- 行数：`trading.rs=168`、`ashare.rs=1202`（钉在预算上，本轮把字段文档压成一行才没有抬棘轮）、
  `src/ashare/tests.rs=378`、`tests/ashare_limit_anchor.rs=165`；登记预算 `HEAD 37 项 / 46411` →
  `本轮 36 项 / 45573`（未新增登记项）。`MODIFIED_TRACKED_FILES=31` 是 Q54–Q60 未提交成果的合计。

本轮未使用任何外部服务或凭据，`sandbox_tested` 全部保持 `false`。

## Unreleased — V11 Q58：多腿回测的规格闸门换成真会红的那一份，保证金/资金费只向衍生品腿计提（2026-09-22）

同样不在 V11 §6 排期内，是"回测链路还有哪些潜在问题，全部修复"这条实测指令的第一批（FN1+FN2 合并一轮）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §20。

### Fixed（回测链路：`multi-builtin` 的记账前提没人守）

- **原有守卫是死代码**：`multi_builtin.rs`（HEAD 版 :133-139）判的是"主腿 spec 是衍生品 **且**
  `primary_spec_path` 没给"，而 spec 只可能从那个 path 解析出来 —— 条件永不成立。于是
  `--funding-bps 25` 一条 spec 都不给也照样跑通：名义额按现货乘数 1 记账、保证金与资金费
  静默记 0，而 stdout 与产物里硬编码的 `margin_model=realized-initial-margin-leverage-1`
  还在宣称本轮按真实保证金记了账。
- **现货腿被按全额名义额记保证金**：`multi_leg_leg_margin` 只要 `leg.spec` 是 `Some` 就计费，
  不看产品形态。Bar 引擎对现货用 `NoMargin`（衍生品规格才有 `leverage_tiers`），归因侧却给
  现货腿再记一笔等于全额现金的保证金 —— 同一笔现金算两次，`margin_peak_raw` 与引擎口径直接矛盾。
- **资金费落到现货腿 = 造一笔不存在的成本**：`multi_leg_leg_buckets` 同样只看"有没有 spec"。
- **新的规格闸门 `multi_leg_spec_guard`（`backtests/leg_funding.rs:82-130`）四条判据，先于任何腿级
  撮合**（`multi_builtin.rs:69-78`，腿级 `run_leg` 在 :204）：① `--config` 声明衍生品却没给主腿
  spec 直接拒；② `--funding-bps` 非零时两条腿都必须带 spec（缺 spec 既算不出金额也判不出形态）；
  ③ 两条腿都不是衍生品时拒绝计提资金费；④ 其余组合放行。产品形态的第二个事实来源由
  `configured_instrument_product`（同文件 :66-80）读 `strategy.product`，与 `backtest_risk_binding`
  / `configured_fill_model` 同构：给了 `--config` 就必须认它。
- **计费只向衍生品腿**：`multi_leg.rs:191-195`（资金费）与 `:241-244`（保证金）都改成
  `leg.spec.filter(|spec| spec.product.is_derivative())`，其余腿记 0；衍生品缺 spec 那种
  组合已在闸门处被拒，所以这里不会再静默把衍生品腿的钱记成 0。
- **产物与 stdout 如实披露**：`margin_model` 按规格真实推导（`multi_builtin.rs:153-161`，
  有衍生品腿规格 → `realized-initial-margin-leverage-1`，否则 `none-no-derivative-leg-spec`），
  stdout 的 Attribution 行带上它（:366），产物新增 `market_specs`（逐腿 spec 路径来源，:415-417）
  与 `margin_model`（:419），`assumptions` 里的资金费/保证金口径同步改写。`cli_help.rs` 的
  `multi-builtin` 说明补上这条命令面契约。

### Added（门禁与用例）

- `crates/qx-cli/tests/multi_leg_attribution.rs` 四个 Q58 行为用例：
  `funding_without_leg_spec_is_refused_before_matching`（缺规格先拒、且不出现任何腿级输出行）、
  `funding_on_spot_only_legs_is_refused`（全现货要求资金费被拒，文案点名两条腿的产品形态）、
  `only_derivative_legs_bear_margin_and_funding`（混合规格：现货腿保证金与资金费列合计为 0、
  衍生品腿资金费合计等于产物总额、`margin_model` 与 `market_specs` 来源如实）、
  `declared_derivative_product_without_primary_spec_is_refused`（配置声明 perpetual 而无主腿规格）。
- `tools/check_architecture.py` 的 `multi_leg_honesty_check` 新增 5 项（164 → 169）：闸门覆盖两类
  非法组合、闸门调用点先于 `run_leg(`、腿级计费的衍生品过滤与 0 记账、产物披露规格来源且
  `margin_model` 由规格推导、四种规格组合各有行为用例。
- 既有回测用例中被本轮口径改写的部分：六条 `--funding-bps 25` 却不给任何 spec 的调用改成 0
  （它们验的是成本/风险/成交溯源，不是资金费），`backtest_entries.rs` 的多腿入口用例补
  `market_specs` 两项为 `null`、`margin_model=none-no-derivative-leg-spec`、
  `margin_peak_raw=0`、`funding_raw=0` 的断言，`vetoed_leg_never_pairs...` 补齐两条 swap 规格
  —— 它声明 `product=perpetual` 且 `allow_short`，现货口径在配置校验期就被拒，原本就是不可能
  无规格跑通的组合。

### Validation（本轮日志实测，`/tmp/qx_q58_gate.log`）

- `cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`；`cargo fmt --all --check` `STAGE2_FMT_EXIT=0`；
  `cargo clippy --workspace --all-targets -- -D warnings` `STAGE3_CLIPPY_EXIT=0`、`CLIPPY_WARNING_LINES=0`。
- 整仓测试：未设 `QX_PYTHON` 时 `CARGO_TEST_workspace_EXIT=101`、`SUM_workspace_passed=168 failed=2`
  （断言 `WORKSPACE_FAILED_WITHOUT_QX_PYTHON=2`）；设解释器后 `CARGO_TEST_WORKSPACE_WITH_PYTHON_EXIT=0`、
  `SUM_workspace_with_python_passed=681 failed=0`、`OK_SUITES=53`；含 doc-test `CARGO_TEST_WITH_DOC_EXIT=0`、
  `SUM_with_doc_passed=681 failed=0`。
- 静态门禁：`STAGE5_ARCH_EXIT=0`、`架构不变量自检全部通过 ✓（169 项）`、`STAGE5_ARCH_FAIL_LINES=0`、
  `STAGE5_ARCH_PASS_LINES=169`。
- 变异成对（cargo 级，整跑 `--test multi_leg_attribution` 全 10 条）：MQ58a（现货腿也记保证金）
  红在 `multi_leg_attribution.rs:653`、`9 passed; 1 failed`；MQ58b（现货腿也计提资金费）红在 `:654`；
  MQ58c（`funding_bps >= 0` 让两条资金费判据重新变死代码）一次点亮两条用例、`8 passed; 2 failed`
  （`:577` 与 `:616`）；MQ58d（声明侧判据永不成立）红在 `:703`；MQ58e（`margin_model` 退回常量）
  红在库侧入口用例 `src/tests/backtest_entries.rs:141`（`8 passed; 1 failed`）。还原侧
  MQ58a/b/c/e 与 STAGE 8 均为 `10 passed; 0 failed`（MQ58e 另带 `9 passed`）。
  **MQ58d 的还原行是 Windows 链接锁报错**（`failed to remove target\debug\qx-cli.exe`）而不是绿 ——
  同一批 pristine 字节（`PRISTINE_OK at restored-after-MQ58d`）随后在 MQ58e 还原行与 STAGE 8
  各跑到一次 `10 passed`，故按那两次记账。
- 变异成对（静态门禁级）：G1 改文案 → `[FAIL] 多腿规格闸门必须对缺规格与全现货两种组合都报错`；
  G2 删闸门调用 → `=1`（"规格闸门落到了撮合之后"）；G3 删两处衍生品过滤 → `=1`；
  G4 `|| true` 让 `margin_model` 不再由规格决定 → `=1`；G5 重命名 Q58 用例 → `=1`；
  五项 `ARCH_RESTORED_G*_EXIT=0`。
- 收口：`PRISTINE_OK` 22 处逐个 `cmp` 通过、`MUTATION_RESIDUE=none`、`FINAL_ARCH_EXIT=0`、
  `BUILD_final_EXIT=0`。
- 行数：`multi_builtin.rs=470`、`single_strategy.rs=428`、`leg_funding.rs=130`、`multi_leg.rs=450`、
  `backtest_entries.rs=423`、`multi_leg_attribution.rs=717`；登记预算 `HEAD 37 项 / 46411` →
  `本轮 36 项 / 45573`（继续下行，未新增登记项）。`MODIFIED_TRACKED_FILES=28` 是 Q54–Q58 未提交成果的合计。

为守住 `< 500` 结构线做了三处搬家（没有抬棘轮、没有改门槛）：`run_ccxt_builtin_backtest` 搬进它自己
委派的那条链 `single_strategy.rs`（`BACKTEST_ENTRY_OWNERS` 随之改指），`configured_instrument_product`
搬进同域的 `leg_funding.rs`，`backtest_entries.rs` 里三条 worker 入口用例拆成新主题文件
`src/tests/strategy_worker_entries.rs` 并补 `mod` 挂载。本轮未使用任何外部服务或凭据，
`sandbox_tested` 全部保持 `false`。

## Unreleased — V11 Q57：对冲补偿提交并入同一道闸门，补偿成交不再按乘数 1 记账（2026-09-22）

同样不在 V11 §6 排期内，是"交易链路还有哪些潜在问题，全部修复"这条实测指令的第三批。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §19。

### Fixed（交易链路：第三条入账入口同时绕过形状契约与精度闸门）

- **`HedgeRecoveryWorker::execute_with_validator` 是 §18 之后剩下的第三条提交入口**：多腿对冲补偿
  腿自己 `router.submit_order(...)` 拿回包，却既不做形状校验（`validate_submit_events`）也不做
  tick/step 校验，直接把回包事实 append 落库。上一轮把"越界回报只能转待对账"补成两条入口同纪律，
  这一轮发现真正在跑资金的那第三条仍然独走 —— 补偿腿由故障恢复自动触发，无人复核。
  现在两条提交入口共用同一个守卫 `gate_submit_facts`（形状 + 精度），越界只留一条
  `ReconcileRequired`（相关串带 `submit-fill-out-of-spec`），并冒泡进本轮诊断。
- **补偿成交退回乘数 1 记账**：该入口 append 的是裸 `ExecutionEvent::Fill`，经
  `RuntimeExternalEvent::Fill` → `apply_fill` → `FillTerms::LegacyMultiplier(1)`，衍生品腿因此按
  `contract_size = 1` 入账 —— 与实盘/回测的冻结规格口径不同源，名义额与保证金都会算错。
  现在 `HedgeOrderValidator` 新增 `instrument_spec()`（默认 `Ok(None)`，保持既有 blanket impl），
  `append_fact` 在带规格时把 `Fill` 升成 `FillWithSpec`。
- **规格解析失败时拒绝补偿**：`instrument_spec()` 返回 `Err` 时该腿不提交、不落事实，诊断点名
  "未解析到产品规格，拒绝自动对冲"；返回 `Ok(None)`（未配置 `instrument_spec_path`，即现货乘数 1
  旧口径）时保持原行为。这是刻意的不对称：缺配置可以继续，配置读坏了不能继续。
- **Paper 补偿重放侧同步接线**：`crates/qx-cli/src/spread.rs` 的 `RecoveryGuard` 把 worker 冻结的
  market spec 交给 `ingest_venue_events_with_spec`，补偿成交落进账本时用的就是那一份规格。

### Added（门禁与用例）

- `crates/qx-execution/src/tests/recovery_and_replay.rs`
  `hedge_compensation_submit_shares_the_submit_precision_gate_and_spec_bookkeeping`：4 行场景表
  （越界回包只留待对账 / 在 tick 上带规格落库且 `contract_size` 保持 / 未配规格回落裸 `Fill` /
  规格解析失败不提交），断言事实的**形状标签序列**与 router 调用次数。
- `crates/qx-cli/src/tests/paper_hedge_recovery.rs`（新模块，从 `execution_and_multi_leg.rs` 拆出）：
  原有部分成交重放用例搬迁 + 新用例
  `paper_hedge_recovery_ingests_venue_fills_through_the_worker_frozen_spec`（在 tick / 越界两档
  验证 Paper 补偿链的落库口径）。
- `tools/check_architecture.py` 新增 5 项（159 → 164）：两道提交入口共用同一闸门（`lib.rs` 内
  `gate_submit_facts(` 调用点恰为 2）、worker 体过闸门且规格解析失败拒绝补偿、`append_fact` 带
  规格升级、两类 reason 段各只有一处定义、每条生产回报路径都把冻结规格交给归约入口
  （`LIVE_REPORT_FILES` 计入 `spread.rs`）；`EXECUTION_TEST_FLOOR` 13 → 25（口径含集成测试目录，
  并把"删一条就红"的注释改成实测总数地板）。

### Validation（本轮日志实测，`/tmp/qx_q57_gate.log`）

- `cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`；`cargo fmt --all --check` `=0`；
  `cargo clippy --workspace --all-targets -- -D warnings` `STAGE3_CLIPPY_EXIT=0`、`CLIPPY_WARNING_LINES=0`。
- 整仓测试：未设 `QX_PYTHON` 时 `SUM_workspace_passed=168 failed=2`（WindowsApps 占位桩必然项，
  断言 `WORKSPACE_FAILED_WITHOUT_QX_PYTHON=2`），设解释器后 `CARGO_TEST_WORKSPACE_WITH_PYTHON_EXIT=0`、
  `SUM_workspace_with_python_passed=677 failed=0`、`OK_SUITES=53`；含 doc-test `CARGO_TEST_WITH_DOC_EXIT=0`、
  `SUM_with_doc_passed=677 failed=0`。
- 静态门禁：`架构不变量自检全部通过 ✓（164 项）`、`STAGE5_ARCH_FAIL_LINES=0`、`FINAL_ARCH_EXIT=0`。
- 变异成对（cargo 级）：MQ57a（worker 里把规格传成 `None`）与 MQ57b（`append_fact` 丢弃规格）
  各自让 qx-execution 用例 `0 passed; 1 failed`，MQ57c（`spread.rs` 忽略规格）让 qx-cli 新用例
  `1 passed; 1 failed`；三者还原后分别 `1 passed` / `1 passed` / `2 passed`。
- 变异成对（静态门禁级）：G1 删 worker 闸门 → `=1`（同时点亮"调用点 1 处（期望 2）"）；
  G2 删 `append_fact` 升级 → `=1`；G3 破坏用例证据 → `=1`；G4 复制精度 reason 段 → `=1`（"2 处（期望 1）"）；
  G5 让 `RecoveryGuard::instrument_spec` 恒返回 `Ok(None)` → `=1`（点名 `crates/qx-cli/src/spread.rs`）；
  五项 `ARCH_RESTORED_G*_EXIT=0`。
- `PRISTINE_OK` 18 处逐个 `cmp` 通过，`MUTATION_RESIDUE=none`，`BUILD_final_EXIT=0`。
- 行数：`crates/qx-execution/src/lib.rs` 1,517 → 1,619（连续第二轮上调，已在 §19.4 记为下一轮先拆）；
  `crates/qx-cli/src/spread.rs` 486 行；登记集 37 → 36 项、总和 46,411 → 45,573。

## Unreleased — V11 Q56：提交同步返回的成交纳入同一条精度闸门（2026-09-22）

同样不在 V11 §6 排期内，来自"交易链路还有哪些潜在问题，全部修复"这条实测指令的第二批。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §18。

### Fixed（交易链路：越界成交只剩半条纪律）

- **实盘 `submit` 同步返回的成交回报绕过精度闸门**：`TradingInstrumentSpec::validate_fill` 此前只在
  用户流归约入口 `ingest_venue_events_with_pipeline` 调用。同一条落在 tick/step 之外的成交，
  走"交易所把回报推下来"会转 `ReconcileRequired`、不记账；走"Venue 在 submit 里直接返回"
  却经 `PortExecutionService::submit` 直接 append 入账——两家适配器（Paper 的即时成交、
  Binance/CCXT 的一次性回包）都属后者，"越界回报只能转待对账"因此只剩半条纪律。
  现在提交路径在形状校验（`validate_submit_events`）之后过同一个谓词：规格优先取回报自带的
  `FillWithSpec.spec`（落库保留的就是它），缺省回落到服务冻结的那份；越界即
  `mark_reconcile(order, "submit-fill-out-of-spec")` 并冒泡错误，Accepted 与成交一条都不落，
  与既有"未知结果"契约同构。

### Added（门禁与用例）

- `crates/qx-execution/src/tests/venue_submit_contract.rs`
  `submit_returned_fills_share_the_precision_gate_with_user_stream_reports`（新，5 形状一张表）：
  裸 `Fill` 在 tick / 越界、自带规格的 `FillWithSpec` 在 tick / 越界、以及"冻结规格宽 + 回报规格严"
  的优先级用例；断言用回报事实的**形状标签序列**（`["accepted","fill-with-spec"]` vs `["reconcile"]`），
  而不是数量相等。
- `tools/check_architecture.py` 新增 3 项（156 → 159）：提交同步回报也过同一闸门并转待对账、
  `lib.rs` 内 `validate_fill(` 调用点**恰为两处**（第三处即红）、上述行为用例在位。
  原"只有一个谓词、一个调用点"改名为"一个谓词（调用点只允许归约入口与 submit）"。
- `EXECUTION_TEST_FLOOR` 13 → 14：新用例进棘轮下限，删掉即红。
- `maturity/capabilities.yaml` `paper_execution` 挂上这条新证据。

### Validation（本轮日志实测，`/tmp/qx_q56_gate.log`）

- `cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`；`cargo fmt --all --check` `=0`；
  `cargo clippy --workspace --all-targets -- -D warnings` `STAGE3_CLIPPY_EXIT=0`。
- 整仓测试：未设 `QX_PYTHON` 时 `SUM_workspace_passed=167 failed=2`（那 2 条是 WindowsApps
  `python` 占位桩必然失败项），带解释器重跑 qx-cli 单 binary `test result: ok. 109 passed; 0 failed`，
  含 doc-test 的整仓跑 `SUM_with_doc_passed=675 failed=0`（`CARGO_TEST_WITH_DOC_EXIT=0`）。
- 静态门禁：`架构不变量自检全部通过 ✓（159 项）`，`FINAL_ARCH_EXIT=0`。
- 变异成对（cargo 级）：MQ56a 把闸门变成永不相中 → 用例 `0 passed; 1 failed`；
  MQ56b 把规格优先级反转成"冻结优先" → 同样转红；两者 `RESTORE_OK` 后 `1 passed; 0 failed`。
- 变异成对（静态门禁级）：G1 删掉整个提交侧闸门 → `ARCH_MUTATED_G1_EXIT=1`（同时点亮新两项），
  G2 在 `mark_reconcile` 前插第三处 `validate_fill(` 调用 → `=1`（点名"3 处（期望 2）"），
  G3 破坏用例里的 `vec!["reconcile"]` 证据 → `=1`；三项 `ARCH_RESTORED_G*_EXIT=0`。
- `PRISTINE_OK` 在 8 个位置逐个 `cmp` 通过，`MUTATION_RESIDUE=none`。
- 行数：`crates/qx-execution/src/lib.rs` 1,498 → 1,517（+19，本轮首次也是唯一一处上调，
  已 `--snapshot` 记账）；登记集仍 36 项，总和 `WORK_BUDGET_SUM=45471`（HEAD 为 37 项 / 46,411）。

## Unreleased — V11 Q54/Q55：回测链与 onboarding 的规格同源，首屏命令不再注定失败（2026-09-22）

本轮不在 V11 §6 排期内：它来自"交易链路与回测链路还有哪些潜在问题，全部修复"这条实测指令，
顺带关掉 §16.4 第 4 条。收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §17。

### Fixed（回测链与首屏命令）

- **D1 回测链只认一种 spec 形状**：`init` 生成的 `qianxing.binance.spot.spec.json` 是冻结产品规格，
  而回测入口此前只吃 CCXT 归一化形状，首屏第一条回测命令必报 `CCXT market 缺少 base`。
  现在回测链与 worker 链共用 `market_spec_from_value`（按 `base_currency` 判形状，不做 try-parse 回落）。
- **D2 缺精度即兜底 = fail-open**：`price_tick_raw` / `qty_step_raw` / `min_qty_raw` 缺失时以前退给 `1`，
  衍生品缺 `max_leverage` / `maintenance_margin_bps` 时退给 1x / 500bp。编造值既通过
  `TradingInstrumentSpec::validate()`，又在产物里与真实声明长得一模一样，滑点与取整两道闸门同时静默失效。
  现在一律拒绝并点名缺哪一项、该改传哪种形状。
- **D3 / D4 `init` 首屏命令**：7 个 profile 曾印同一行回测命令，其中 `base`/`paper` 没绑策略、
  `ccxt`/`multi-venue` 连 BarFrame 都没复制；印出的路径是裸文件名，隐含"先 cd 到项目目录"这条从未写在屏幕上的前提。
  现在只印"文件真的复制进项目 **且** 入口真的读得动这份绑定"的命令，且路径带上项目目录。
- **D5 `strategy init` 生成不可自包含的项目**（本轮实测新发现）：模板里的 `deploy/…` 相对路径被原样
  写进输出配置，而运行时相对路径按**配置文件所在目录**解析，于是刚生成的配置读不到自己的行情夹具。
  现在与 `init` 共用同一套归一：键改裸文件名、资产复制进项目、命令给绝对路径。

### Changed（概念与结构单点）

- `crates/qx-cli/src/market_spec.rs`（新，198 行）：产品规格 JSON 的唯一读法 + CCXT 快照到规格的转换。
- `crates/qx-cli/src/init_project.rs`（新，486 行）：从 `config_commands.rs`（965 → 590 行）逐字拆出的
  onboarding 命令面；`worker_entry.rs` 拆出 `market_spec.rs` 后 460 行，退出行数登记集。
- "哪些内置 kind 是双腿"只剩 `BuiltinStrategyKind::needs_reference_leg()` 一个谓词，
  内核与运行时配置面的三处四元 `matches!` 名单删除；CLI 侧准入名单与之由新门禁逐项对齐。

### Added（门禁 8 项 + 用例 9 条）

- 门禁（148 → 156）：规格解析/构造单点、三项定点精度必须声明、两形状按 `base_currency` 判定、
  回测链与 worker 链各自经 loader、同源用例在位、双腿 kind 内核谓词≡CLI 名单、
  `init` 两个单标的入口按谓词拒绝、拆出模块成对挂载。
- 用例：`crates/qx-cli/src/tests/init_onboarding.rs`（新，4 条，把 `init` 印出的每条命令原样执行）、
  `crates/qx-cli/src/tests/market_spec_single_source.rs`（新，5 条，含"回测链吃 CCXT 形状且与冻结形状同 `result_hash`"）。
- 门禁脚本新增并发保护：开跑时给变异目标拍 pristine 快照，每次动手前 `cmp`，被第三方改过即
  `CONCURRENT_TREE_CHANGE` + `exit 6`（本轮实测：单实例锁会被手工 `rmdir` 弄失效，而 `TaskStop`
  只杀掉外层包装、旧脚本继续跑完整个变异段，两份日志同时作废）。

### Validation（本轮日志实测，`/tmp/qx_q55_gate.log`，2026-09-22）

- 前置与静态面：`cargo check --workspace --all-targets` = 0；`cargo fmt --all --check` = 0；
  `cargo clippy --workspace --all-targets` = 0 且 `CLIPPY_WARNING_LINES=0`；门禁 156 项全绿。
- 行数棘轮：`HEAD_BUDGET_ENTRIES=37 / SUM=46411` → `BUDGET_ENTRIES=36 / SUM=45452`（−1 项、−959 行），
  与 HEAD 登记表的 diff 只含下行与删项。
- qx-cli 用例两个口径：不设 `QX_PYTHON` 退 101 且 `106 passed; 2 failed`（既知的两条解释器依赖用例）；
  设 `QX_PYTHON` 退 0 且 `108 passed; 0 failed`；`CLI_TEST_FUNCS=112`。
- 反向验证 7 个变异逐个红、还原逐个绿：M1 `ARCH=1` + `TESTS=101`（新补的那条用例死在
  `missing field \`instrument\``）、M2 `ARCH=1` + `TESTS=101`、M3 `ARCH=1`、M4/M5/M6 `TESTS=101`、
  M7 `ARCH=1`；还原侧 `ARCH_*_RESTORED=0` / `TESTS_*_RESTORED=0`、`RESTORE[…]=identical` ×7、
  `MUTATION_RESIDUE=no`、`PRISTINE_OK` ×8 且无 `CONCURRENT_TREE_CHANGE`。
- 全量：`QX_PYTHON=… cargo test --workspace --all-targets --no-fail-fast` = 0，
  `OK_SUITES=53`、`RUST_PASSED=674`、`RUST_FAILED=0`（上一轮基线 673，本轮净 +1 条用例，没有靠删用例换绿）。
- 产物零污染：`MODIFIED_DEPLOY=0`（`DEPLOY_LINES=48` 全是 Q1a 遗留的未跟踪 run 目录）。
- 本轮不使用任何外部服务或凭据，`maturity/capabilities.yaml` 的 `sandbox_tested` 保持 `false`。

### Changed（文档与能力表）

- `cli_scenario_init` 的证据路径改指 `init_project.rs` 与 `init_onboarding.rs`（此前指向的代码已拆走），
  并新增 limitation `ccxt_and_multi_venue_profiles_print_no_backtest_step_because_init_copies_no_market_frames_for_them`；
  `local_backtest` 增记单一读法文件与"两形状同 `result_hash`"用例。改完单跑门禁三次均 `EXIT=0`。

## Unreleased — V11 Q1a 第二批：Bar 链撮合口径从配置面可达且看得见（2026-09-22）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§6 的 Q1a 行；本轮结掉"Bar 链撮合模型接到命令面"，`--virtual-trading` 未做（前置条件见
同文档 §16.4 第 1 条），收口记录见同文档 §16。

### Added（`strategy.fill_model` 从"没人读的配置名"变成生效口径）

- `crates/qx-cli/src/backtests/fill_model.rs`（新，167 行）：`BarFillModel` 三成员
  （`next_bar_open` / `best_price` / `one_tick_slippage`）是**这张表就是命令面**，
  `bar_fill_model(configured, instrument_spec)` 是四条 Bar 回测链唯一的取口径入口，返回
  `BarFillModelBinding { fill, name, source }`。`source` 区分"配置没提这一项"与"配置里声明了
  同一个模型"，与 `ExecutionCostBinding::source` 同一条理由：分不清来源等于让默认值冒充选择。
- `strategy.fill_model`（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:162`）：
  `#[serde(default, skip_serializing_if = "Option::is_none")]`，与 `cost_rules_path` 同口径。
  三条 Bar 装配链（策略 / 内置 / 多腿的每一条腿）都读它；深度链不读，因为它不经过 `FillModel`。
- 产物留痕三处：策略链摘要 `fill_model: {name, source}`（`backtests/artifacts.rs:137`）、
  多腿归因产物同键（`multi_builtin.rs`）、stdout 的 `[Builtin · Execution]` 与
  `[Multi-leg · Execution]` 两行。内核侧 `model_descriptors[0]` 从此不再是恒为
  `NextBarOpen@v1` 的假话。
- **多腿腿级口径守卫**（`multi_builtin.rs:214-223`）：逐腿记录 `(name, source)`，两腿不一致或
  一条都没记录都报错；一档滑点的**大小**允许随各腿自己的 `price_tick` 变化（标的规格，非口径分叉）。
- **不可达者按档位 fail-closed**：`probabilistic` → "需要 L1 一档盘口"、`volume_sensitive` →
  "需要 L2/L3 深度盘口"，并说明 Bar 输入只有 OHLCV、深度链不经过 `FillModel`；
  `one_tick_slippage` 缺 market spec 或 spec 的 `price_tick` 非正数一律拒绝，不退成
  `ccxt_market_to_spec` 的兜底 `1`。拼错的名字才报"未知"并列全清单。
- 架构不变量 137 → **142 项**（`backtest_assembly_check()` 新增 5 条）：撮合模型只在
  `fill_model.rs` 构造且装配字段取自 `self.fill`；三条链的口径全部经 `bar_fill_model` 解析
  （计数 ≥3 且 `configured_fill_model` 在位）；schema 声明该字段；`config validate` 调用同一判据；
  模型驱动用例在位。

### Changed（校验与装配同源）

- `fill_model_problem()` 是唯一判据，`bar_fill_model()` 用同一张表，`config validate` 经
  `fill_model_failure()` 只多套一层字段名前缀——"validate 放行过的名字，装配要么跑得动要么只缺
  market spec"由用例 `unreachable_or_under_specified_fill_models_fail_closed` 钉住。
- `BarBacktestAssembly::new()` 多一个 `BarFillModelBinding` 必答题，装配处不再给撮合模型默认值；
  `ecosystem_smoke.rs:357` 显式传 `bar_fill_model(None, None)`（内核默认不需要 market spec）。
- 能力矩阵 `local_backtest` 的限制项按事实拆开：原"Bar 链撮合模型与虚拟成交配置都不可达"一条
  改为"虚拟成交缺标记价输入" + "深档模型缺盘口输入"两条，证据加 `backtests/fill_model.rs`
  与 `tests/backtest_fill_model.rs`。

### Added（用例：5 条，删 0 条）

- `crates/qx-cli/src/tests/backtest_fill_model.rs`（新，368 行，挂在 `tests/mod.rs:243`）：
  `each_reachable_fill_model_changes_the_result_and_is_declared`（三种口径逐个与同 spec 基线比
  `result_hash` 与 `turnover_raw`，并断言名称/来源/描述子三处留痕）、
  `unconfigured_fill_model_is_absent_from_the_runtime_bytes`（省略即序列化字节不变，护住 66 份
  blessed 产物）、`unreachable_or_under_specified_fill_models_fail_closed`（四条拒绝路径的理由
  与"校验=装配"同判据）、`command_line_entries_read_the_declared_fill_model`（内置链缺 spec 先红
  后绿、多腿归因产物两种来源标注）、`depth_summary_carries_no_fill_model_key`（深度链不得冒充
  Bar `FillModel` 描述子）。

### Validation（本轮日志实测，`/tmp/qx_q1a2b_gate.log` + `/tmp/qx_q1a2_ev.log`）

- 基线：`cargo check --workspace` 0（`CHECK_ERROR_LINES=0`）、`cargo fmt --all --check` 0、
  `cargo clippy --workspace --all-targets` 0 且 `CLIPPY_WARNING_LINES=0`、
  `check_architecture.py` 0（142 项全过）。
- 全量 `cargo test --workspace --all-targets --no-fail-fast` 两口径：不设 `QX_PYTHON` 退 101、
  53 目标、`NOENV_PASSED=663`（本机必然失败的两条 Python strategy worker 用例，PATH `python`
  是占位桩）；设 `QX_PYTHON` 后 `FULLTEST_QXPYTHON_EXIT=0`、53 目标、**665 通过 / 0 失败**。
  对上一轮记录的 660 是净增 5 条，恰为本轮新增用例数，删 0 条。
- 反向验证 13 对，锚点预检 `ANCHOR_PRECHECK_BAD=0`：静态段 S1–S7 逐个
  `MUTATED_*_ARCH_EXIT=1` → `RESTORE[*]=identical` → `RESTORED_*_ARCH_EXIT=0`；行为段 B1–B6 逐个
  `MUTATED_*_EXIT=101`（每例 `4 passed; 1 failed`，非编译失败）→ `RESTORE[*]=identical` →
  `RESTORED_*_EXIT=0`（`5 passed`）。收口 `RESIDUE[*]=clean` ×13、`FINAL_ARCH_EXIT=0`、
  被测面复跑 17 passed。
- 命令行可区分性（同一份 Bar 帧 + 同一份 `sma_cross` 配置，只改 `strategy.fill_model`）：
  未声明与声明 `next_bar_open` 的 `result_hash` 逐位相同（`4b128bdee0ce75dd`）而摘要
  `source` 分别为 `builtin-default` / `runtime-config`；`best_price` → `629219d09096e384`、
  `one_tick_slippage`（带 spec，`tick=1000000`）→ `96de2663bbc67d3c`，两者相对基线的
  `turnover_raw`（86500000000 → 87500000000 / 87501000000）、`fees_raw`（43250000 →
  43750000 / 43750500）、`final_equity_raw`、`result_hash` 四项全变。
  同一份配置只改这一项时 `config fingerprint` 五值两两不同
  （`d2a4c4e335209221` / `813ede121ff6d041` / `7f606946e554a65c` / `463d9c0e60fc0c58` /
  `ba153af4bd0baf3c`）——这是"改做配置字段而非旗标"的全部理由。
- 四条拒绝路径（缺 spec、`next-bar-open`、`probabilistic`、`volume_sensitive`）全部退 2、
  stderr 原文说清理由，且 `SUMMARIES_AFTER_FAILURES=0`（失败轮不留产物）；
  `config validate` 对 `probabilistic` 退 2 印 `[FAIL] strategy.fill_model …`，对 `best_price`
  与"未声明"退 0 且 0 次提及该字段。
- 行数棘轮：`SNAPSHOT_EXIT=0`，`BUDGET_DIFF_EXIT=1` 的唯一差异是
  `crates/qx-xingban/src/orderbook_backtest.rs 1247 → 1245`（收紧，本轮未触及该文件）。
  **本轮无增长项**：新 `fill_model.rs` 167 行与用例 368 行都在 500 门槛外，无需登记；
  触及的 `runtime_check.rs` 与 `tests/backtest_entries.rs` 均停在 499（P4P 兄弟模块门槛是严格
  `< 500`，rustfmt 会把嵌套调用折成 4 行，故改用 `let` + `extend` 两行形状）。
- 产物卫生：跟踪的 66 个 `deploy/data/**/runs/*` 产物在本轮全量用例后
  `MODIFIED_DEPLOY=0`（逐个未被改写）；本轮测试另产生 12 个未跟踪文件（3 个 run 哈希 × 4 文件），
  连同上一轮遗留共 24 个未跟踪——这条脏源与 16 份过期 blessed 摘要一起留给 Q1b 裁决。
- 诚实性边界：本轮未使用网络、凭据或外部服务，`maturity/capabilities.yaml` 的 `sandbox_tested`
  仍 18 条全为 `false`。

### 本轮踩到并记进 §16.2 的坑

- `cargo check -p qx-cli --all-targets` **不编译依赖 crate 的测试**：给 `StrategyRuntimeConfig`
  加必填字段后，只有 `cargo test --workspace --all-targets` 才报出
  `qx-runtime/src/runtime_config/strategy_tests.rs:259` 的 `E0063 missing field fill_model`。
- 回测入口的 market spec 是 **CCXT 形状**（`base`/`price_tick_raw`），与 worker 侧
  `instrument_spec_path` 吃的 `TradingInstrumentSpec` 不是一种文件：把
  `deploy/qianxing.binance.spot.spec.json` 传给回测入口退 2 报 `CCXT market 缺少 base`。
  仓库里没有现货的 CCXT 形状规格，故命令行证据用合约规格配现货帧（只证"档位取自 spec"），
  用例自造 `price_tick_raw=3_000_000` 夹具以避开兜底 `1` 与仓库里的 `1_000_000`。
- `BarFillModelBinding` 装 `Box<dyn FillModel>` 因而没有 `Debug`，用例取 `unwrap_err()` 要过一层
  `map(|binding| binding.name)`——顺带钉住"错误里带模型名"。

## Unreleased — V11 Q1a 第一批：示例真成交、深度链参数可达且看得见（2026-09-22）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§6 的 Q1a 行；本轮结掉"深度链撮合参数 + 现金门 + 信号层两缺陷 + 三份示例夹具"，
Bar 链的 `--fill-model` / `--virtual-trading` 未做（前置条件见同文档 §15.4），
收口记录见同文档 §15。

### Fixed（信号层：内置策略此前"永远不交叉"与"永远不信号"）

- `crates/qx-strategy/src/builtin.rs:777`：`ema()` 的旧值权重从 `window` 改成 `window-1`。
  α = 2/(window+1) 时旧值权重必须是 1−α，写成 `window` 让两权重之和成为 `(window+2)/(window+1)`，
  直流增益不为 1、常数价格列收敛到 2× 该常数，快慢线相对位置随窗口大小漂移。`ema_cross` /
  `macd` / `keltner_trend` 共用该函数，交叉判定此前无从谈起。
- 同文件 `:225`/`:233`/`:349`：滚动历史上限低于信号门槛。`max_history()` 原来只按
  `slow_window`/`period` 取值（默认 5/20/14 得 32 根），而 MACD 第一个信号需要 35 根，
  于是示例帧再长也永不信号。新增 `required_bars()` 作为唯一门槛清单，历史上限取
  `max(required_bars, 窗口上限)`，信号门槛读同一函数。
- `crates/qx-xingban/src/orderbook_backtest.rs:573-586`：深度链补上**无 market spec 时的现货买入
  现金门**，与 Bar 链及同文件 `:535-556` 的有规格分支同口径（名义额 + 名义额 × `fee_bps`/10000
  超过可用现金即写 `Rejected` 事件并跳过），不再允许账本记成负现金。

### Added（命令面：高保真撮合参数接线）

- `backtest book` 新增 `--latency-snapshots` / `--market-impact-bps`（`cli_args.rs:477/479`，
  越界由 `parse_bps()` `:34` 在 clap 层挡住）→ `DepthExecutionModel`（`backtests/depth.rs:13`）
  → 内核 `OrderBookExecutionModel`，L1 与 L2 两条装配各自 `with_execution_model()`。三处同时留痕：
  `[Depth · Execution]` 行、产物 `model_descriptors` 的四参数描述子
  （`orderbook_backtest.rs:361`）、`depth_run_config_hash()`（`depth.rs:258-259`）——参数不进
  config hash 会让两个不同结果抢同一个内容寻址路径。缺省全 0 时与改动前逐位一致。
- **内核第三项 `queue_position_bps` 故意不做成旗标**：它只作用于限价单档位
  （`orderbook.rs:332`），而内置策略 intent 恒 `limit: None`（`builtin.rs:721`），17 条策略全发
  市价单。用例反向钉住它不存在（`--queue-position-bps` 退非 0 并点名旗标）。
- 拒单事实从 Q0e 的多腿私有实现提升为三条链共用：`backtests/artifacts.rs:43/63/76`
  （`rejection_facts` / `rejection_facts_line` / `rejection_count`），摘要产物新增
  `rejected_orders` 与 `rejection_reasons`，stdout 新增 `[Builtin · Integrity]` 与
  `[Strategy · Integrity]` 两行。`fills=0` 从此能区分"没发信号"与"全被挡"。

### Changed（示例夹具与口径文案）

- `deploy/qianxing.bar-frame.example.json` 5 → 70 根（旧夹具短于任何策略的预热窗口）；
  `deploy/qianxing.ashare.bar-frame.example.json` 时间戳换成真实交易时段 epoch 毫秒、价格改成
  先跌后涨；`python/examples/backtest_momentum.py` 的目标仓位 `1` → `ONE_UNIT = 1e9`（raw 口径）；
  四份 runtime 模板共 5 处 `builtin_quantity: 1 → 1000000000`，并在 `strategy_schema.rs` 写明
  runtime 用 raw、CLI 位置参数用整数单位，两者相差 1e9 倍。
- Q0c 的深度链延迟冲突文案改成可执行指令（`depth.rs:99`）：点名"把 `latency_base_ns` /
  `latency_insert_ns` 置 0，或改用 `--latency-snapshots`"。

### Added（用例：6 条，删 0 条）

`ema_keeps_unit_dc_gain_and_lags_on_the_trend_side`、`macd_signals_within_the_rolling_history_cap`
（qx-strategy）、`specless_spot_buy_beyond_available_cash_is_rejected_not_overdrawn`（qx-xingban）、
`depth_execution_model_flags_change_results_and_are_recorded`、
`shipped_examples_fill_positions_and_pay_nonzero_fees`、
`blocked_signals_are_distinguishable_from_silent_strategies`（qx-cli 集成用例）。

### Validation（本轮日志实测，`/tmp/qx_gateQ1a2_v3.log` + `/tmp/qx_q1a_evidence_000305.log`）

- `cargo fmt --all --check` 与 `cargo clippy --workspace --all-targets` 退出 0、warning 0 行；
  `cargo test --workspace --all-targets --no-fail-fast` `TEST_TARGETS=53`、
  `TEST_PASSED=660 TEST_FAILED=0`（对照上一轮基线 654，净增 6 条即上述新用例）；
  本轮改过的两份模板 `config validate` 退出 0；`tools/check_architecture.py` `ARCH_EXIT=0`、
  137 项不变量全过；`MUTATION_RESIDUE=none`。
- 反向验证成对红/绿：M1（摘掉 `[Depth · Execution]` 行）`MUT_M1_EXIT=101` →
  `RESTORE[M1]=identical` → `MUT_M1R_EXIT=0`；M2（四参数描述子退回只写 `fee_bps`）双红
  （引擎 `MUT_M2_EXIT=101` + CLI `MUT_M2T_EXIT=101`）→ `RESTORE[M2]=identical` →
  `MUT_M2R_EXIT=0`。M2 首轮只红在 CLI 侧、引擎 76 passed 全绿，于是补了引擎断言再跑一遍——
  只有消费者侧断言的门禁不算门禁。
- 撮合参数六 case（`/tmp/qx_q1a_ev_table.txt`）：L2/L1 在缺省 / 冲击 50bp / 延迟 2 快照三个口径下
  `result_hash` 分别为 `32d8ea011a3ec946` / `ccc1ea04991c300b` / `71d8e01d91418b7e`，
  六行 `model_fingerprint` 两两不同（`31f6292c7d0acf38` / `230e5a68855baf50` / `509ea904c41e2caa`
  / `1f6ef283165e7476` / `920aa1e71f2cc44a` / `868e9e5f10cf32c4`）；冲击 50bp 让
  `fees_raw` 32411000000 → 32573055000、`final_equity_raw` 100265589000000 → 99941316945000、
  `return_bps` 26 → -5；`--queue-position-bps 5000` 实测 `QUEUE_FLAG_EXIT=2`。
- 13 条内置策略在同一份 Bar 帧上（全部退出 0）：6 条 `fills=1`、4 条 `fills=2`、
  3 条 `fills=0`；其中 `bollinger` 带 3 次 `NoShort` 拒单（原因看得见），`atr_trend` 与
  `volatility_breakout` 的 `rejected_orders=0` 且可证明是夹具性质——70 根里
  `|Δclose| > ATR14` 的根数为 0/56（最大单根变动 1.0e9，最小 ATR14 1.5e9），
  波动率突破需要振幅比 > 2 而实测最大 1.333。
- 棘轮 re-baseline（本轮唯一增长项）：`crates/qx-cli/src/cli.rs` 668 → 675、
  `crates/qx-strategy/src/builtin.rs` 1009 → 1097、
  `crates/qx-xingban/src/orderbook_backtest.rs` 1112 → 1247。
- 产物卫生：跟踪的 `deploy/data/**/runs/*` 66 个（`*.summary.json` 16 份），其中含
  `rejected_orders` 的 0 份、含 `latency_snapshots=` 描述子的 0 份 —— 全部 blessed 产物早于本轮
  两次 schema 变更，移交 Q1b 重 bless；本轮测试另产生 12 个未跟踪 run 文件。
- 诚实性边界：本轮未使用网络、凭据或外部服务，`maturity/capabilities.yaml` 的 `sandbox_tested`
  仍 18 条全 `false`（`SANDBOX_TESTED_TRUE=0 / SANDBOX_TESTED_TOTAL=18`）。

### 本轮踩到并记进 §15.2 的坑

- **陈旧二进制冒充证据**：门禁末尾 `cp -p` 还原把源码 mtime 带回变异前，独立使用的
  `target/debug/qx-cli.exe`（23:51:40.74）于是是变异期产物，跑出的表里描述子只剩 `fee_bps=5`、
  六 case 出现两个相同 `model_fingerprint`，看着像真缺陷。重链后自洽；证据日志从此第一段
  先打印 exe 与相关源码 mtime。
- **MSYS `/tmp` ≠ 原生解释器 `/tmp`**（本轮两次）与 **`cargo test -- <filter>` 的 0 命中假绿**
  （过滤器匹配函数名，写错得到 `0 passed` 且退出 0）；变异段改为把 `test result:` 原文打进日志。

## Unreleased — V11 Q0e：多腿归因只承认实际成交（2026-09-21）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§4.18 与 §6 的 Q0e 行；一腿被挡时的显式动作按编号默认值取**标记 `PendingReconcile`**
（不做自动收口），收口记录见同文档 §14。

### Changed（`multi-builtin` 的归因与产物不再乐观记账）

- `crates/qx-cli/src/multi_leg.rs`：套利组**只由实际成交配对**（`:381-387`）。改之前只要某条腿
  在信号计划里出现过量，即使它一笔都没成交（现金不足、名义额上限、reduce-only 被挡），组里照样
  挂上它的计划数量，净敞口 / 保证金峰值 / 归因费用可以描述一个现实中拿不到的组合；改后
  `filled_qty_raw` 为零即不成组，该腿成本与成交量原样进 `residual_*`，一腿成交而对手腿落空时
  登记 `MultiLegPendingReconcile`（含对手腿成交量），策略写死为
  `mark-pending-reconcile-no-auto-close`。每腿新增"计划 vs 成交"事实
  （`planned / filled / unfilled_planned_qty_raw / vetoed_signal_ts / rejected_orders /
  rejection_reasons`），拒单原因从该腿事件日志的 `EventKind::Rejected` 归并而来，
  **不新增事件、`result_hash` 不变**。
- 单腿定资拆到 `crates/qx-cli/src/backtests/leg_funding.rs`（新模块，60 行）：改用**本腿自己的**
  全帧最高价（旧口径取两腿全局最大值，一条 1e8 倍的参考价腿会把主腿账户撑爆）、手续费余量按
  **当前生效的成本绑定** `taker_bp` 折算（旧口径是 ×2 的估算），并且全程 checked——算不出来
  直接报错，不再 `.min(i64::MAX as i128)` 静默截断。截断正是"买不起的计划被伪装成跑通且零成交"
  的机制（§4.18 的同一类失真）。
- 产物升 `schema_version: 2`，新增 `accounts`（两腿初始现金与定资规则原文）、`legs`、
  `pending_reconcile`；stdout 增 `[Multi-leg · Integrity]` 与 `[Multi-leg · Reconcile]` 两行，
  归因行增 `residual_filled_qty_raw` / `residual_fees_raw`。两道闭合守卫
  （`multi_builtin.rs:285`、`:301`）让"裸腿事实 vs 残余成交"和"归因成交量 vs 撮合 fills"
  不闭合时直接失败。

### Added（用例与门禁）

- `crates/qx-cli/tests/multi_leg_attribution.rs` 用例 3 → 6 条：四条多腿 kind 各一条端到端
  （过去只有 `pairs_arbitrage` 被覆盖）、风控挡腿场景（用运行时配置
  `risk_rules.max_notional_raw = 10_000_000_000_000` 落在 ETH 腿 6e12 与 BTC 腿 1.2e14 之间，
  实测主腿 `fills=0 / rejected_orders=34 / unfilled_planned_qty_raw=6000000000`、
  `groups=0`、`pending=3`、`residual_filled_qty_raw=6000000000`）、定资越界必须报错
  （退出码非零 + stderr 点名上限与 `quantity` + 不得产出归因摘要）。
- `tools/check_architecture.py` 新增 `multi_leg_honesty_check()`（挂在 `kernel_claim_check()` 后），
  架构不变量 **130 → 137 项**；七条判据逐条注入实测红（§14.3）。回测主题模块登记新增
  `leg_funding`，`backtests/mod.rs` 的顶层条目保持在 8 个上限内。
- 反向验证成对记录在 §14.3：行为侧 R1（抽掉"只看成交才成组"）与 R2（抽掉裸腿登记）都让
  `vetoed_leg_never_pairs_against_a_filled_counterpart` 红、R3（定资退回静默截断）让
  `multi_leg_funding_bound_fails_loudly_instead_of_capping_cash` 红，门禁侧 G1–G7 七次注入全红，
  十次还原全部 `RESTORE[*]=identical` 且还原后复跑绿。

### 已知偏离（写进产物与能力矩阵，不当作已完成）

- §6 的"每腿费用按各自 venue spec"**未落地**：`TradingInstrumentSpec` 没有 maker/taker 字段，
  唯一生效费率来源 `ExecutionCostRules` 是全局的；仓库里带分档费率的
  `crates/qx-core/src/fenye.rs`（464 行）在自身文件之外零消费者，是 V10 P2a 留下的死码。
  两腿因此共用一份成本绑定，该事实写进产物 assumptions 与
  `capabilities.yaml` 的 `multi_leg_execution.limitations`。
- 策略侧仓位仍是"意图"口径（`StrategyContext::positions` 由策略自持、成交不回填），
  本轮只把归因与产物改成实际成交，并把 `vetoed_signal_ts` 留作后续接线的入口，见 §14.4。

## Unreleased — V11 Q0d：撮合内核表述如实化（2026-09-21）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§4.3 第 3 条与 §6 的 Q0d 行；本轮按其编号默认值执行——**只改表述 + 建 Q1d 排期，不改行为**，
收口记录见同文档 §13。

### Added（`kernel_claim_check()`：把"共用内核"这类说法钉成可判的六条）

- `tools/check_architecture.py` 新增 `kernel_claim_check()`，挂在 `paper_fee_same_source_check()`
  之后，架构不变量 **124 → 130 项**。六条判据各自"抽掉就变红"：
  paper 侧（`crates/qx-zhenlu/src/**`，整文件、剥注释）不得出现 `OrderBook` / `BookLevel` 符号；
  文档指向的成交入口必须真实存在（`impl PaperVenue` 与 `pub fn on_quote(`，实测在
  `crates/qx-zhenlu/src/lib.rs:1330`）；Tick 链必须确实复用 L2 引擎
  （`crates/qx-xingban/src/tick_backtest.rs:12-16` 的 `use crate::{… OrderBookBacktestEngine …}`）；
  `orderbook.rs` 模块文档必须点名两处真实消费者、并写明 paper 走首档 touch 与 Q1d 指向；
  最后是全局连坐——`crates/*/src/**`（跳过用例文件）加 `README.md`、`deploy/README.md` 里，
  任何把 Paper 与"共用 / 共享 / 同一 / 都走 / 复用 + 内核 / 撮合 / 订单簿"写进**同一句**、
  又不引用真实共享符号（`FeeModel` / `apply_fill_to_books` / `apply_ledger_fill` / `Ledger` / `Oms`）
  的表述一律红；否定语（不成立 / 不得 / 没有 / 不走 / 谎称 …）豁免，
  否则诚实记录错误说法的文字本身过不了门禁。
- 新判据的反向验证成对记录在 §13.3：M1（文档退回旧版）、M2（README 写入"Paper 与回测共用撮合内核"）、
  M3（`oms.rs` 引用 `OrderBookSnapshot`）、M4（Tick 不再复用 L2 引擎）、M5（`on_quote` 改名）五条红，
  M3n（同位置只加一行 `OrderBook` 注释）**阴性对照保持绿**。

### Changed（三处表述按事实改写，README 经核算不改）

- `crates/qx-xingban/src/orderbook.rs` 模块文档：原第 3 行"可被历史 Tick 回放、Paper 模拟和性能基准
  共同使用"不成立。新文档给出真实消费者两处、**Paper 不走这里**（首档一次性 touch、无逐档队列、
  无排队中的部分成交）、paper 与回测目前真正共享的只有执行平面下游三件
  （`FeeModel`、`qx_core::apply_fill_to_books`（调用点：`qx-runtime/src/pipeline.rs:1448,1462`、
  `qx-xingban/src/backtest.rs:848`、`qx-xingban/src/orderbook_backtest.rs:405`）、`Ledger`），
  并把"改接本内核"显式指向 V11 §9 的 Q1d。
- `crates/qx-cli/src/backtests/kernels.rs`：该文件是产物清单里 `matching_kernel` 三个名字的家，
  模块文档补明"还有第四套撮合不在清单里，因为它不产出回测产物"，避免读者以为内核只有三个。
- 给当时的 `docs/牵星完整架构方案-V1.md` §2.3 加现状对照（该文档已在文档收口时删除，原文在 git 历史）：核对后确认该节没说谎（它把撮合适配器放在"可替换"
  那一侧），缺的是现状标注——事件 / 归约 / `Oms` / `Ledger` / `FeeModel` 四模式同源已成立且有门禁，
  **撮合尚未同源**。`README.md:20` 的"回测与实盘共享同一规则内核"是**规则**口径而非撮合口径
  （风控与费用同源已由 Q0a/Q0b/Q0c 接成配置驱动），故保留原文。

### Fixed（门禁自身的缺陷）

- 判据 1 最初照惯例套了 `non_test_source()`，而 `crates/qx-zhenlu/src/lib.rs:16` 就挂着一行
  `#[cfg(test)] use`，按"首个 `#[cfg(test)]` 之前"截断后这条判据实际只扫了 15 行，
  `PaperVenue` 本体根本没进扫描——M3 第一次跑没红才暴露它。改为整文件扫描 + 剥注释。
  这条缺陷不影响 Q0a–Q0c 的任何结论（那些判据不依赖被截断的区段）。

### Validation（本轮日志实测，`/tmp/qx_q0d_round_main.log` + `/tmp/qx_q0d_gate_203911.*.log`）

- 前置门槛（`GIT_HEAD=b311f90`）：`CARGO_CHECK_EXIT=0`、`FMT_CHECK_PREFLIGHT_EXIT=0`、
  `CLIPPY_DENY_EXIT=0 CLIPPY_WARNING_LINES=0`、`ARCH_BASELINE_EXIT=0 ARCH_PASS_LINES=130`、
  `BUDGET_DRIFT_LINES=0`、`BUDGET_RESTORED=identical`。
- 用例基线与收口一致（无行为改动，本轮不新增 Rust 用例）：
  `SUITE_BASELINE_EXIT=0 TARGETS=53 PASSED=651 FAILED=0` →
  `SUITE_FINAL_EXIT=0 TARGETS=53 PASSED=651 FAILED=0`；`qx-cli` 单测 95 条全绿。
- 六次注入均 `INJECT_OK`（anchor 唯一）、六次还原均 `RESTORE[*]=identical` + `ANCHOR_BACK[*]=yes`，
  每次还原后复跑 `ARCH_PASS_LINES=130`（六次）。`MUT_RESIDUE[M2]` / `[M3n]` 为 yes 是 §7.4
  已登记的判据假象（M2 的 replacement 首行等于 anchor 首行；M3n 的 replacement 以换行起头使首行为空串），
  不是残留。
- 收口复跑：`FINAL_CHECK_EXIT=0`、`FMT_CHECK_EXIT=0`、`ARCH_FINAL_EXIT=0 ARCH_PASS_LINES=130`、
  `SUITE_FINAL_*` 同上。`maturity/capabilities.yaml` 的 `sandbox_tested` 仍 18 条全为 `false`；
  本轮未使用任何网络、凭据或外部服务。
- 行数棘轮例外（显式声明）：`orderbook.rs` 664 → 671 行，增量全在模块文档，按快照头部规则重 bless，
  `--snapshot` 的 diff 只有这一行。

### Known issues（移交，见 §13.4）

- Q1d 落地时判据是**成对**的：行为改接簿内核那一次必须同时改掉判据 1、判据 4 与 `kernels.rs` 的说法，
  否则门禁会把旧口径钉成化石；该轮的"paper vs book 成对数字"目前没有可复用的夹具，需要新造
  "同一 L1 输入两跑"的最小夹具。
- 本轮没跑任何 CLI，仅两次全量用例就把 `deploy/data/**/runs/` 的未跟踪文件推到 **40 条**，
  §12.4 第 1 条的产物冲突已升到必须裁决，归 Q1b。

## Unreleased — V11 Q0c：执行成本配置面接线（2026-09-21）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§3B 末条（死配置面）与 §6；本轮采用其 §0 的编号默认值 **E3 = 接入**（而不是删净），
收口记录见同文档 §12。上游 `77eb160`（成交归约 + 账本记账币种）在本轮断点合并进主线，
合并提交 `ef461b4`，取舍口径见 §12.1 末段。

### Added（`strategy.cost_rules_path` 从死配置变成生效配置）

- **成本规则文件有了第一个生产读者**：`ExecutionCostRules::load`（`crates/qx-xingban/src/cost_rules.rs:20`）
  此前全仓零消费者，`deploy/qianxing.costs.example.json` 是一行"宣称能配、实际没人读"的死配置。
  现在唯一读取点是 `runtime_wiring::execution_cost_binding_from_config`（`crates/qx-cli/src/runtime_wiring.rs:184`），
  它返回的 `ExecutionCostBinding`（`runtime_wiring.rs:156`）同时提供 `fee_model()` 与 `latency_model()`，
  消费方为 Bar 两条链（`backtests/single_strategy.rs:94` 与 `:278`）、多腿链
  （`backtests/multi_builtin.rs:155`，两条腿共用同一份绑定）、深度档（`backtests/depth.rs:69`）
  与三处 Paper 构造点（`venue_runtime/paper_worker.rs:48/191/244`、`paper_submit.rs:121`）——
  Q0a 解决的"两条链取同一个模型"从此不必再靠两边都写同一个常数。
- **产物记录成本来源**：回测摘要新增 `execution_costs.source`（`backtests/artifacts.rs:81`），
  取值 `cost-rules-file:<绝对路径>` / `runtime-config-default`（给了配置但没配这一项）/
  `builtin-default`（根本没给配置）三态；A 股规则链自带佣金模型时打印
  `ashare-rules:<path>+cost-rules-file:<path>`，把"费率来自规则快照、延迟来自成本文件"两件事分开记账
  （`backtests/single_strategy.rs:105-118`）。`backtest builtin` 另印一行 `[Builtin · Cost] source=… maker_bp=… taker_bp=… latency_base_ns=… latency_insert_ns=…`。
- **深度档的三层优先级**：显式 `--fee-bps` > 成本文件 `taker_bp` > 内核默认
  （`backtests/depth.rs` 的 `fee_bps.unwrap_or(costs.rules.taker_bp)`）。深度内核只有单一吃单费率，
  成本规则里的延迟与 maker 费率在这条链上无处落地，因此**延迟非零时直接报错**并点名来源文件，
  而不是静默跑出一份"配置写着 2ms、实际零延迟"的产物（Q0b 删 `--config` 判掉的正是这个形状）。
- **校验与装配同一个读者**：`config validate` / `runtime-check` 走 `cost_rules_problem()`
  （`runtime_check.rs:306-312`），与装配调用同一份 `ExecutionCostRules::load`，
  缺文件、坏 JSON、bp 越界都带解析后的路径失败，不可能再出现"校验说没问题、装配跑不动"。
- 新用例文件 `crates/qx-cli/src/tests/backtest_cost_provenance.rs`（334 行、5 条）逐条钉住上面四件事，
  共享夹具 `read_first_artifact` / `read_first_backtest_summary` 上移到 `tests/mod.rs`
  （`backtest_risk_provenance.rs` 里的两份私有副本删除）。

### Changed（使用者可见，但不破坏既有产物）

- `strategy.cost_rules_path` 声明为 `#[serde(default, skip_serializing_if = "Option::is_none")]`
  （`qx-runtime/src/runtime_config/strategy_schema.rs:138`）：**没配置时连键都不序列化**，
  因此仓库里 66 份已 bless 的 `deploy/data/**/runs/*.run.json` 与全部 `config_fingerprint` 逐字节不变，
  本轮无需重 bless（对比 Q0a 需要成对改 5 条 paper 断言）。
- 深度档 `--fee-bps` 的缺省值不再由 `cli.rs` 分派层给出：Q0b 装的
  `unwrap_or(qx_core::DEFAULT_TAKER_BP)` 下沉进深度入口，分派只把 `Option<i64>` 原样传下去。

### Fixed（本轮顺带消灭的两处分裂）

- Bar 装配此前写死 `latency: Box::new(ZeroLatency)` —— 成本规则里的延迟字段配了也不生效；
  现在延迟与费用成对取自同一份绑定，`ZeroLatency` 字面量在 `backtests/mod.rs` 已不允许出现。
- 默认费率常数的定义点收敲为唯一一处：`qx-core/src/fee.rs` 定义 `DEFAULT_MAKER_BP` / `DEFAULT_TAKER_BP`，
  `qx-xingban/src/cost_rules.rs` 只做再导出（门禁按 `pub const` 形状区分"定义"与"再导出"）。

### Added（架构不变量 117 → 124 项）

`paper_fee_same_source_check()` 从 8 项扩到 15 项，新增 7 条：成本规则文件的读者全仓唯一、
成本规则模板随仓库发布、Bar 装配不得写死零延迟、深度档缺省费率取自成本绑定、
运行时配置声明 `strategy.cost_rules_path`、`config validate` 覆盖该项、存在成本驱动行为用例；
另把两条旧判据改写为"缺省费率不再由分派层写死"与"费用与延迟成对来自同一份绑定"。
新增共享谓词 `is_cli_case_file()`（`tools/check_architecture.py`）：`qx-cli/src/tests/` 下的主题文件
整体就是用例现场，按路径认领（原 `definitions` / `loaders` / Paper 构造点三处各写一份过滤）。

### Validation（v11-q0c 轮实测，日志 `/tmp/qx_q0c_gate_194740.log`，2026-09-21 19:47:40–19:49:15）

- 前置门槛（HEAD 为合并提交 `ef461b4`）：`CARGO_CHECK_EXIT=0`、`FMT_CHECK_PREFLIGHT_EXIT=0`、
  `CLIPPY_DENY_EXIT=0` 且 `CLIPPY_WARNING_LINES=0`、`ARCH_BASELINE_EXIT=0 ARCH_PASS_LINES=124`。
- 行数棘轮：`SNAPSHOT_EXIT=0`、`BUDGET_DRIFT_LINES=0`、`BUDGET_RESTORED=identical`
  —— 本轮改动（含上游合入）之后无需新登记；合并时按既有先例（`ec57a62`）重基线一次，
  结果是 `crates/qx-execution/src/lib.rs` 1496 → **1498（上游 `fill_tag` 语义修复 +2）**，
  另有 `config_commands.rs` 971 → 965、`worker_entry.rs` 586 → 578 两项下降。
- 基线与收口（`cargo test --workspace --all-targets --no-fail-fast`，覆盖 `crates/*/tests/`）：
  `SUITE_BASELINE_EXIT=0 TARGETS=53 PASSED=651 FAILED=0` → `SUITE_FINAL_EXIT=0 TARGETS=53 PASSED=651 FAILED=0`，
  逐项一致，没靠删用例换绿；收口另加 `FINAL_CHECK_EXIT=0`、`FMT_CHECK_EXIT=0`、
  `ARCH_FINAL_EXIT=0 ARCH_PASS_LINES=124`。与 Q0b 轮基线（51 目标 / 604 例）的差里，
  本轮自身贡献是 `backtest_cost_provenance.rs` 的 5 条，其余来自上游 7 个提交带的新用例文件。
- 反向验证八组全部成对（静态面 M2/M3/M4/M5/M6，行为面 M1/M3/M4/M7/M8）：
  M1 让加载器读完文件后仍返回 `ExecutionCostRules::default()` → `M1_TEST_EXIT=101`
  （成本驱动用例红）→ `restore_M1_TEST_EXIT=0`；
  M2 在 `runtime_check.rs` 里造第二个 `ExecutionCostRules::load` 读者 → `ARCH_MUT_M2_EXIT=1`
  （`ARCH_PASS_LINES=123`，"读者全仓唯一"点名两个文件）→ 还原 `ARCH_PASS_LINES=124`；
  M3 装配退回写死 `ZeroLatency` → `ARCH_MUT_M3_EXIT=1`（`122`，成对性与零延迟两条同时红）
  且 `M3_CHECK_EXIT=0`、`M3_TEST_EXIT=101` → 两项还原后均 0；
  M4 深度档缺省费率退回 `qx_core::DEFAULT_TAKER_BP` → `ARCH_MUT_M4_EXIT=1`（`123`）
  + `M4_TEST_EXIT=101` → 均 0；M5 让 `config validate` 不再报告成本文件问题 → `ARCH_MUT_M5_EXIT=1` → 0；
  M6 把成本驱动用例改名 → `ARCH_MUT_M6_EXIT=1`（"存在 Q0c 证据"红）→ 0；
  M7 把多腿归因产物里的 `execution_costs` 键挪走 → `M7_TEST_EXIT=101` → 0；
  M8 关掉深度档的延迟拒绝 → `M8_TEST_EXIT=101` → 0。
  八组 `RESTORE[*]=identical`、`ANCHOR_BACK[*]=yes`。
- 一处如实记录的判据假象：`MUT_RESIDUE[M2]=yes`。残留检查只 grep 替换文本的首行，
  而 M2 的替换首行恰好就是锚点原行，于是报 yes；同轮 `RESTORE[M2]=identical`、
  `ARCH_RESTORE_M2_EXIT=0 ARCH_PASS_LINES=124`，且收口前的全仓残留扫描（`RESIDUE_SCAN_DONE` 之前）
  没有输出任何文件，可判无残留。该判据本身的这一盲点属 §7 门禁设计待办，不改本轮结论。
- 模板整跑（本轮实测，非日志内条目）：18 份 `deploy/qianxing.runtime*.json` 逐项
  `qx-cli config validate` → `TEMPLATE_SWEEP_OK=17 FAIL=1`，唯一红项是 production 模板的
  `[FAIL] strategy.research_snapshot_path / dataset_bundle_path 文件不存在: /var/lib/qianxing/research/*`
  两条（部署机绝对路径，与成本面无关，自 V10 起就是记录性条目）；
  CI 侧 `deploy/qianxing.runtime*.json` 的 for-loop（`.github/workflows/ci.yml:204-213`）
  经过 `runtime_check.rs:306` 的新校验，因此成本文件错误会在这 18 份模板上自动暴露。

### Known issues（本轮记录、未修）

- **`code_commit` 进了 RunManifest 文件名**（上游 `crates/qx-cli/build.rs` 把 `QX_GIT_COMMIT` 烧进产物）：
  每次提交后跑全量用例，`deploy/data/*/runs/` 就多出一批新的未跟踪产物 —— 本轮收口时实测
  未跟踪 24 个文件（`git ls-files --others` 计 runs 条目），仓库内已 bless 的 runs 文件 66 个。
  这是"产物可追溯"与"测试不留垃圾"的正面冲突，需在 Q1b 一并决定（候选：文件名不含 commit、
  或 runs 目录整体 `.gitignore` 只 bless 摘要白名单）。
- **工作树全量 CRLF**：`core.autocrlf=true` 下 `git ls-files --eol` 记到 224 个 `.rs` 里有 **201 个**
  是 `i/lf w/crlf`（索引内 LF、工作树 CRLF），任何跨行匹配源码文本的断言都会脆断。
  本轮踩到一次并修在测试侧（`crates/qx-cli/src/tests/backtest_entries.rs:373` 先 `.replace('\r', "")`
  再匹配帮助文本），未动 git 配置（属用户级设置，本轮不改）。


## Unreleased — V11 Q0b：命令面旗标诚实性与回测准入分区（2026-09-21）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§4 P0 第 2 项与 §6；本轮采用其 §0 的编号默认值 **E2 = 删除被吞的 `--config`**（而不是接线），
延续 E1 的"改动可见即记账"口径。收口记录见同文档 §11。

### Changed（破坏性 CLI 行为）

- **`qx-cli backtest --config <path>` 这一形态不再存在**（Q0b，§4 P0 第 2 项）：`Command::Backtest`
  父命令此前声明 `--config`、分派处以 `config: _` 收下并丢掉 —— 统一回测链路根本不读这份配置，
  使用者以为换了风控与费用口径而实际什么都没生效，这比没有旗标更坏。按 E2 选择删旗标而非接线：
  现在该命令行按 clap 未知参数**用法错误退出 2**、stderr 点名 `--config`；真正吃配置的子入口
  （`backtest builtin` / `multi-builtin` / `ccxt-builtin` / `book`）保留各自的 `--config` 且确实读取
  （V10 P0b 已收口），帮助里也继续暴露。脚本若曾给统一回测传 `--config`，需改用位置参数
  `runtime`/`frame`/`spec` 或改走子入口。
- 深度档回测的缺省手续费不再写死游离字面量：`cli.rs` 改为
  `fee_bps.unwrap_or(qx_core::DEFAULT_TAKER_BP)`，缺省值取自 Q0a 收进内核的同一常数（当前 5bp），
  Bar 链与深度链的默认口径失去各自漂移的可能；`book` 的帮助文本同步写明该来源。

### Fixed（能力自述诚实性）

- **"17 个内置策略都可直接用于回测/Paper/策略接入"这类笼统宣称**：准入分区收敛为唯一事实来源
  `cli_help::MULTI_LEG_KINDS`（4 个套利 kind 需两条对齐 BarFrame，只被 `backtest multi-builtin` 接受）
  与 `backtest_entry_of()`；此前是两处各写 4 个 kind 的字面量判断（`backtests/depth.rs` 的 `matches!`
  与 `backtests/multi_builtin.rs` 的反向判断）加一句帮助文案。现在两个入口的门、
  `builtin-strategies` 与 `strategy list` 的第三列输出、帮助里的 13/4 计数全部消费同一个列表
  （`depth.rs:30` / `multi_builtin.rs:21` 改为 `MULTI_LEG_KINDS.contains(&kind)`）。
  `print_builtin_strategies` 从 `cli.rs` 迁入 `cli_help.rs`，输出由"名称 + 说明"变成
  "名称 + 说明 + 真正接受它的回测入口"。
- `paper-check` 的帮助此前只说"验收主体链路"，未交代行情来源；现与 `paper-e2e` 同口径写明注入一条
  固定合成 L1 报价（99/100）、不接真实 feed。

### Added（架构不变量 114 → 117 项，用例 602 → 604 条）

- `cli_flag_honesty_check()` 两项：`cli.rs` 分派正文不得出现任何 `ident: _` 丢弃绑定；
  `cli_args.rs` 声明的 12 个长旗标字段逐个必须在 `cli.rs` 被点名（新声明却没人读的旗标同样红）。
- `paper_fee_same_source_check()` 追加一项：深度档 `fee_bps` 缺省值必须引用
  `qx_core::DEFAULT_TAKER_BP`。
- `src/tests/cli_surface.rs::backtest_rejects_the_config_flag_it_used_to_swallow`：子进程验证
  `backtest --config` 退出码 2 且点名旗标，同时验证 `backtest builtin --help` 仍暴露 `--config`。
- `src/tests/backtest_entries.rs::builtin_strategy_entries_match_the_printed_partition`：逐 17 个
  `BuiltinStrategyKind` 打通三条事实做交叉断言 —— 内核侧 `BuiltinStrategyConfig::new` 的合法性、
  深度档与多腿两个真实入口的错误文案、以及 `(单标的, 套利) = (13, 4)` 的计数；再断言帮助文本里的
  计数与 `builtin-strategies` 实际印出的准入列逐项等于 `backtest_entry_of`。

### Validation（v11-q0b 轮实测，日志 `/tmp/qx_q0b_gate_091927.log`，2026-09-21 09:19:27–09:20:48）

- 前置门槛：`CARGO_CHECK_EXIT=0`、`FMT_EXIT=0`、`CLIPPY_DENY_EXIT=0` 且 `CLIPPY_WARNING_LINES=0`。
- 基线（本轮基线是"Q0b 改动已落地、变异前"的状态，`cargo test --workspace --all-targets --no-fail-fast`
  覆盖 `crates/*/tests/`）：`BASELINE_TARGETS=51`、`BASELINE_PASSED=604`、`BASELINE_FAILED=0`、
  `BASELINE_EXIT=0`；`ARCH_BEFORE_MUTATIONS_EXIT=0`（117 项全绿）。
- 行数棘轮：`SNAPSHOT_EXIT=0`、`BUDGET_DIFF_EXIT=1`，本轮唯一差异是
  `crates/qx-cli/src/cli.rs` 登记预算 674 → **668（下降）**，无增长项、无需例外申报。
  实际行数 cli.rs 668 / cli_args.rs 475 / cli_help.rs 167 / tests/cli_surface.rs 368 /
  tests/backtest_entries.rs 417 / tests/mod.rs 206，测试文件均在 500 行阈值之下。
  快照后 `ARCH_AFTER_SNAPSHOT_EXIT=0`。
- 反向验证（六组，静态判据 N1/N2/N3/N5 走架构自检，行为判据 N4/N5/N6 走子进程用例）：
  `ARCH_MUT_N1_EXIT=1`（写回 `config: _,` → 两项红：丢弃形状 `['config']` + cli.rs 669 > 预算 668）
  → `ARCH_REST_N1_EXIT=0`；
  `ARCH_MUT_N2_EXIT=1`（新声明没人读的 `--experimental-never-read` → "13 个长旗标全部被读到"红）
  → `ARCH_REST_N2_EXIT=0`；
  `ARCH_MUT_N3_EXIT=1`（缺省费率退回 `unwrap_or(5)` → 内核常数引用判据红）
  → `ARCH_REST_N3_EXIT=0`；
  `TEST_N4_RED_EXIT=101`（分区里 `PairsArbitrage` 换成 `VolatilityBreakout` → 逐 kind 用例在
  `backtest_entries.rs:348` FAILED，其余 62 例被过滤）→ `TEST_N4_GREEN_EXIT=0`；
  `ARCH_MUT_N5_EXIT=1` + `TEST_N5_RED_EXIT=101`（两个文件同时复活"父命令声明、分派丢弃"的形状 →
  架构红且 `backtest_rejects_the_config_flag_it_used_to_swallow` 在 `cli_surface.rs:351` FAILED）
  → `ARCH_REST_N5_EXIT=0` + `TEST_N5_GREEN_EXIT=0`；
  `TEST_N6_RED_EXIT=101`（帮助退回旧的笼统宣称 → 帮助口径断言在 `backtest_entries.rs:395` FAILED）
  → `TEST_N6_GREEN_EXIT=0`。
  七个 `RESTORE[*]=identical` 且 `MUTATION_STILL_PRESENT[*]=no`；N5a 恢复后 `config: Option<PathBuf>,`
  字段总数回到 4（四个子入口各自合法声明，计入式守卫 `POST_N5a_CONFIG_FIELDS=4`）；
  收口 `MUT_RESIDUE=no`。
- 收口复跑：`ARCH_FINAL_EXIT=0`（117 项）、`BUILD_FINAL_EXIT=0`、`FINAL_EXIT=0`、
  `FINAL_TARGETS=51`、`FINAL_PASSED=604`、`FINAL_FAILED=0`、`FMT_FINAL_EXIT=0` —— 与基线逐项一致，
  没靠删用例换绿；`qx-cli` 单 binary 用例数 61 → 63（本轮新增 2 条）。
- 门禁日志之后仅追加一处文档注释修正（`cli_help.rs` 的 `MULTI_LEG_KINDS` 注释把行为用例文件指向
  `cli_surface.rs`，实际落在 `backtest_entries.rs`），不涉及任何可执行语义；复跑记在
  `/tmp/qx_q0b_postcomment_092425.log`：`FMT_EXIT=0`、架构自检 117 项通过、
  增量重编 `qx-cli` 后 `TARGETS=51`、`PASSED=604`、`FAILED=0`、`TEST_EXIT=0`。


## Unreleased — V11 Q0a：Paper 成交费用与回测同源（2026-09-21）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§4.1 与 §6；本轮采用其 §0 的编号默认值 E1（允许受控重 bless paper 常数并成对记录）、
E2–E5 留待后续阶段。

### Fixed（正确性）

- **Paper 成交恒定零手续费，与它自己的文档注释相反**（Q0a，§4.1）：`PaperVenue::new` 自带
  `ZeroFeeModel` 默认、唯一的换费率入口 `with_fee_model` 只有单元测试调用，于是生产 Paper 的
  `Fill.fee` 恒为 0 —— 同一份策略在 Paper 上系统性优于回测与实盘，属于"执行平面成本口径分叉"
  而不是显示问题。现在费用模型是 `PaperVenue::new` 的**必填构造参数**（默认值这个形状被删除，
  编译期就不许回落），并沿 `execute_paper_submit_effect` 的形参显式上溯到调用方；qx-cli 侧
  新增唯一构造点 `runtime_wiring::execution_fee_model()`，Bar 回测装配（`backtests/mod.rs`）与
  全部 Paper 构造点（`paper_submit.rs` / `paper_worker.rs` / `spread.rs` / `ecosystem_smoke.rs`）
  共用它，因此"同源"是构造出来的而非两份相同字面量的巧合。默认费率常数的定义点收进内核
  （`qx_core::fee::DEFAULT_MAKER_BP/DEFAULT_TAKER_BP` + `MakerTakerFeeModel::default_maker_taker()`），
  `qx_xingban::cost_rules` 只做再导出。不计费的用例一律显式传 `Box::new(ZeroFeeModel)`。
- 新增行为面证据 `crates/qx-execution/tests/paper_accounting.rs::paper_spot_fill_charges_the_shared_fee_model_into_the_ledger`：
  现货买入成交后按 `bp_amount(notional(...), DEFAULT_TAKER_BP)` 推导费用，并要求 Ledger 里
  `Fee` 分录的求和等于该值（`Ledger::apply_fill*` 只在 `fill.fee` 非零时写 Fee 分录，所以
  "费用真的进了账"是可观测的 +1 条分录）。`qx-zhenlu` 侧另把 Paper 费用模型的 descriptor 冻结为
  `MakerTaker@v1[params=maker_bp=2;taker_bp=5]` —— descriptor 变了就是执行平面成本口径变了，须与
  Bar 回测装配同步审阅。

### Added（架构不变量 107 → 114 项）

- `paper_fee_same_source_check()` 七项：默认 maker/taker 常数只在 `qx-core::fee` 定义（再导出不算
  第二处）、`cost_rules` 保留再导出、`execution_fee_model` 在 qx-cli 只有一个定义点、每个生产
  Paper 构造点的实参显式回答费用模型、qx-cli 生产代码不得出现 `ZeroFeeModel`、`backtests/mod.rs`
  的 `fee` 字段调用同一函数、上述 Ledger 用例存在。

### Validation（v11-q0a 轮实测，日志 `/tmp/qx_q0a_gate_084708.log`，2026-09-21）

- 前置门槛：`CARGO_CHECK_EXIT=0`、`FMT_EXIT=0`、`CLIPPY_DENY_EXIT=0` 且 `CLIPPY_WARNING_LINES=0`。
- 基线（`cargo test --workspace --all-targets --no-fail-fast`，覆盖 `crates/*/tests/`）：
  `BASELINE_TARGETS=51`、`BASELINE_PASSED=602`、`BASELINE_FAILED=0`、`BASELINE_EXIT=0`。
  快照前的架构自检 `ARCH_PRESNAPSHOT_EXIT=1`，唯一红项就是下面申报的行数棘轮增长。
- 行数棘轮：`SNAPSHOT_EXIT=0`、`BUDGET_DIFF_EXIT=1`，逐行差异只有两项 ——
  **唯一增长项** `crates/qx-execution/src/lib.rs` 1494 → 1496（+2 行：`execute_paper_submit_effect`
  新增的 `fee_model` 形参与其一行文档；这是第二次为正确性修复上调预算，不构成"后续无需再降行数"的
  结论，本轮曾试过用格式化腾挪压成零增量，`cargo fmt` 后仍回到 +2 故按既有出口记账）。
  另一项 `crates/qx-zhenlu/src/lib.rs` 2270 → 2269（−1，删掉 `zero_fee_paper()` 辅助与重复断言）。
  快照后 `ARCH_AFTER_SNAPSHOT_EXIT=0`，114 项全绿。
- 反向验证（M1/M5 走子进程行为，M2/M3/M4/M6/M7 是静态文本判据，配对记录）：
  `TEST_M1_RED_EXIT=101`（把注入退回 `ZeroFeeModel` → `paper_spot_fill_charges_…` FAILED，
  其余 3 例仍过）→ 还原 `TEST_M1_GREEN_EXIT=0`；
  `TEST_M5_RED_EXIT=101`（`Ledger` 两处 `if !fill.fee.is_zero()` 改成恒假 → 同一用例 FAILED）
  → 还原 `TEST_M5_GREEN_EXIT=0`；
  `ARCH_MUT_M2_EXIT=1` / `ARCH_MUT_M3_EXIT=1`（两项：构造点未给出 + 生产代码出现零费）/
  `ARCH_MUT_M4_EXIT=1`（两项：常数第二定义点 + 再导出缺失）/ `ARCH_MUT_M6_EXIT=1` /
  `ARCH_MUT_M7_EXIT=1`，各自 `ARCH_REST_*_EXIT=0`。七个 `RESTORE[*]=identical` 且
  `MUTATION_STILL_PRESENT[*]=no`，收口 `MUT_RESIDUE=no`。
- 收口复跑：`ARCH_FINAL_EXIT=0`（114 项）、`FINAL_EXIT=0`、`FINAL_TARGETS=51`、
  `FINAL_PASSED=602`、`FINAL_FAILED=0` —— 与基线逐项目标数一致，没靠删用例换绿。
- **受控重 bless（E1）：旧 → 新成对**。Paper 成交开始计费后，五条"Ledger 分录条数"断言各多 1 条
  `Fee` 分录：
  `paper_strategy_reads_filled_position_before_emitting_next_order` 2 → 3（`e2e_and_python_contract.rs:149`）、
  `paper_worker_cleans_stale_queue_after_terminal_commit` 3 → 4（同文件 :221）、
  `paper_e2e_entrypoint_runs_scheduler_strategy_execution_and_ledger` 3 → 4（同文件 :278）、
  `paper_multi_leg_spread_submits_each_leg_through_single_track_and_reduces_group`
  (1,1,2) → (1,1,3)（`execution_and_multi_leg.rs:274`，每腿成交各多 1 条）、
  `paper_submit_order_runs_queue_pipeline_ledger_and_ack` 3 → 4（`paper_and_strategy_worker.rs:82`）。
  名义额、持仓与成交条数均未变，变的只有费用分录。

## Unreleased — V10 重构收口（2026-09-20）

方案、逐阶段验收口径与实测数字见已删除的 `docs/自研量化框架重构方案-V10.md`（文档收口时移除，原文在 git 历史）
§6 与收口记录；本轮四决策（D1 实盘一律 fail-closed / D2 回测风控同源 / D3 命令面按审计收口 /
D4 外部验收只交付可执行方案）记在同文档 §0、§8.1。

### Fixed（正确性）

- **实盘存在静默降级的无风控提交路径**（P0a，§4.1）：`worker_risk_context` 在
  `instrument_spec_path` 缺失时返回 `Ok(None)`，调用方随即走不带风控的提交分支，而 paper 侧
  对同一缺口是显式拒绝——即缺规格的部署照样下单且无人察觉。现在判定收敛为唯一的
  `require_worker_risk_spec`，Binance / CCXT / Paper 三条链的每个提交点都先过它，缺配置一律
  `FAIL_CLOSED: … 缺少风控配置（instrument_spec_path），拒绝提交订单` 且不留任何成交事实；
  `crates/qx-cli/src/tests/live_submit_fail_closed.rs` 钉住"被判 Failed 且 EventLog 无成交事实"。
- **两条回测入口根本不读风控配置**（P0b，§4.2）：`multi-builtin` 与深度档此前把规则集写死成
  `None`，同一份 `strategy.risk_rules` 在四条链上得到不同门禁（回测"通过"而实盘被拒，或反之更危险）。
  现在命令行型入口经唯一的 `backtest_risk_binding` 取配置，产物里显式写明规则来源
  （`runtime-config` / `conservative-default`），深度档另在清单里声明自己用的是
  `TickBacktestEngine` / `OrderBookBacktestEngine`，不再谎称与 Bar 链同一内核。
  `src/tests/backtest_risk_provenance.rs` 断言四条链得到同一规则集版本。
- **假数据被当作能力入口**（P0c，§4.3/§4.4）：`reconcile` 无参数时手写两组持仓打印差异的行为
  删除，改为必须给本地与远端来源，否则用法错误退出码 2；`run` 的错误文案此前宣称支持
  `backtest` 而 match 无该分支，现帮助表与派发集合由门禁做集合相等校验；
  `all` / `verify` 自带的第二套撮合循环删除，改调 `qx-xingban` 真实内核（只有输入序列是合成的，
  产物里 `input_fingerprint` 明写 `synthetic:*`）。
- **现货多腿归因少一条腿成交**（§10.12，原推送阻断项）：上游合并带进的"现货买入必须由账户
  可用现金支付"检查让 `multi-builtin` 的示例数据首次暴露问题——每腿装配仍沿用默认的
  100_000 USDT，2-BTC 腿第三笔买入的名义 ≈122_400 直接被内核拒成废单，成交额常数从
  `385_200_000_000_000` 掉到 `262_800_000_000_000`。定因排除了两个嫌疑（费用原语换实现前后逐字符
  相同、成交关联号形状回退后同一断言以同样数字失败），并按门禁变异流程证明共享风控实例不是原因
  （`backtest_risk_binding` 每次 `gate()` 都新构造规则集）。修法是补齐装配而非重 bless：每腿起始
  现金按"全帧最高价 × quantity × 2"取足余量且不低于默认值，期望常数一字未改即回到 green；
  M11 把该行退回默认值后测试立刻以同样的数字转红，证明修复是承重的。

### Refactored（架构与边界收敛）

- **概念单点化**（P1a，§4.5/§4.7/§4.8）：`qx-zhenlu::RiskGate` 的默认构造绕过点归零，回测与
  实盘的风控门全部经 `strategy_risk_gate` 构造；`PositionSnapshot` 收在 `qx-protocol` 线格式一处、
  `TargetPosition` 收在一处；对账归一为 `qx-genglu` 的 `order_reconcile_verdict`
  （`Consistent` / `PendingReconcile` / `AutoConverge` / `NeedsHuman`）+ 唯一动作映射，
  Binance / CCXT / EventLog 三处不再各自推导"是否一致"。
- **spread 屏障下沉网关**（P1b，§4.10）：屏障判定与执行只在 `qx_execution::spread_group_barrier`，
  CLI 侧那份薄壳连同"绕过网关的预检"一并删除；五个提交入口一律新增组存储形参，让编译器强制
  每个调用点回答它；命令带 `spread_group_id` 而提交路径未注入组存储不再是"跳过屏障"，
  而是 `FAIL_CLOSED` 拒绝提交。门禁改查"判定原语不得搬回 CLI"。
- **存储写路径与重试退避**（P1c，§4.9）：四个 JSON 文件状态存储的序列化 / schema 版本拒绝 /
  损坏判定 / 读改写事务收敛为 `qx-storage/src/state_envelope.rs` 一份实现（原子替换与追加锁
  逐字节沿用，磁盘兼容是硬约束），并拆出 `src/file/` 目录模块；退避与尝试计数收敛为
  `qx-core::retry`（`Backoff::Fixed|Exponential` + `RetryPolicy`，纯函数、不读系统时钟），
  连接器重连、调度器重试、存储计数三处只承载形状参数并委托它。
- **薄壳 crate 与中文代号**（P2a，§4.11）：四个薄壳 crate 按语义归属并入并留下出处注释——
  `qx-oms` → `qx-zhenlu/src/oms.rs`、`qx-portfolio` → `qx-zhenlu/src/portfolio/`、
  `qx-application` → `qx-execution/src/application.rs`、`qx-fenye` → `qx-core/src/fenye.rs`，
  workspace 成员随之减少；中文代号（牵星 / 观星 / 星板 / 针路 / 更路 / 卯眼榫头，以及并入内核的
  分野）在 README 的模块表逐条给出一句话职责，并补齐此前漏登记的 `qx-api` / `qx-data` /
  `qx-orchestrator` / `qx-risk` 四行、删掉已不存在的 `qx-application` 行（表与 `crates/`
  目录现已逐项对齐）。同一段里"`cli.rs` 是命令名→处理器唯一分派点"的旧描述也随 P2b 改为
  clap 派生口径。能力矩阵的失效证据路径与"行为用例只增不减"普查口径同步收口（§10.10）。
- **CLI 参数框架**（P2b，§4.12）：约 40 个命令的手写字符串派发迁移到 clap 派生，命令表只有一份，
  `qx-cli` 仍是单 binary，未知参数退出码保持 2。
- **超大文件真拆分**（P2c，§4.13）：`qx-xingban/src/ashare.rs` 与 `qx-runtime/src/lib.rs`
  按职责边界拆目录模块，等价性用符号 token 多重集比对证明；登记集规模与逐文件行数只降不升。

### Verification（内部门禁：M1–M11 突变复跑，收口轮）

- 日志 `qx_m1m11_round_231438.log`（本轮抓到）：前置编译 `PRECHECK_EXIT=0`，`FMT_CHECK_EXIT=0`、
  `CLIPPY_EXIT=0`（0 warning 行）、`ARCH_EXIT=0`（107 项不变量全过），基线 `61 passed; 0 failed`。
  十二个红绿对全部咬住：M1 实盘 fail-closed、M2A/M2B 回测读同一份风控配置、M4 run 帮助谎报入口、
  M5 用法退出码、M11 现货多腿起始现金——变异后测试退出 101 并打印点名断言，还原后退出 0；
  M3 与 M6–M10 走静态架构门禁，变异时 `ARCH_EXIT=1` 并打印对应 `[FAIL]` 不变量原文，还原回到 107 项全过。
  `MUTATION_RESIDUE=none`，终态 `FINAL_EXIT=0`、`61 passed; 0 failed`。
- 整工作区同轮复跑（`qx_workspace_test_231340.log`）：51 个测试目标、`601 passed / 0 failed`。
- 验证脚本自身有两处失真在本轮被修掉并记入方案文档 §10.13：还原用 `cp -p` 会把备份的旧 mtime 带回
  去，cargo 按 mtime 判新鲜于是"还原后的绿"跑在变异版 binary 上（现还原后 `touch` 并自检
  `FRESHNESS_STALE`）；P2b 之后 M5 的锚点字符串已随手写派发一起删除，锚点未命中会伪装成红绿同向
  （现 `swap` 未命中即 `SWAP_ABORTED` 终止本轮）。

### Verification（外部验收，D4：本轮不执行）

- `tools/binance_testnet_acceptance.py` 现在逐段记录退出码 / 耗时 / 输出末行，并把带时间戳的
  结果包落到 `maturity/evidence/testnet/<UTC>-{orders|dryrun}/`（不再跑完即删临时目录）；
  缺凭据时仍只跑离线两段并以退出码 3 结束。结果包里的 `sandbox_tested_flip` 是
  `maturity/capabilities.yaml` 翻转的唯一依据。
- 新增 [docs/外部链路验收执行方案-V1.md](docs/外部链路验收执行方案-V1.md)：三段链路的前置条件、
  崩溃后远端未知态的处置程序、以及**当前缺口如实记录**（CCXT 侧没有 `ccxt-submit-order`，
  因此第二交易所的第三段暂不可跑）。
- CI 的 wheel 腿补 macOS 平台；`sandbox_tested` 在未拿到真实外部结果包前全量保持 `false`，
  §5 的"能力齐备、内部闭环、外部未证"结论本轮不变。

### Merged（上游分叉收口，2026-09-20 追加）

- 拉取并合并上游 `6743ae0`（"fix: close audit gaps in execution, backtest and CLI layout"）。
  该提交与本地 V9/V10 在 `296e56e` 分叉，**独立重做了一遍 CLI 巨型文件拆分**（把当时的
  `qx-cli/src/main.rs` 拆成 19 个扁平顶层模块并改用宏命令表），与本仓库的目录模块布局
  （`backtests/`、`venue_runtime/`、`tests/`）和按路径取数的架构门禁互斥。
  合并口径逐 hunk 判定：**结构与 API 形状取本仓库**（受祝福风控构造、`spread_group_barrier`
  单点、`runtime_config/` 目录模块、config 持有 `risk` 字段、存储信封与 `qx-core::retry` 单点），
  **行为与修复移植上游**（`qx-core::fee` 统一 `FeeModel` 并把合约乘数与反向计费基准折进费用价、
  `cost_rules.rs` 费用规则单点、单向净持仓强平、强平按 taker 费率、报表费用与成交额改由成交账本推导）。
- 同一"第二笔成交被静默丢弃"缺陷两侧各修了一次：本仓库按订单定序关联号（V9 §8.2 反向验证 F），
  上游改按成交内容判重。合并后以本仓库口径为准并由既有用例钉住，不保留第二套幂等键推导。
- 被合并丢弃的上游模块随时可用 `git show 6743ae0:<path>` 取回；`crates/qx-{oms,application,portfolio,fenye}`
  在本仓库已由 P2a 并入语义归属 crate，合并未复活它们。

## Unreleased — V9 重构收口（2026-09-19）

方案与逐阶段判定见已删除的 `docs/自研量化框架重构方案-V9.md` §8（文档收口时移除，原文在 git 历史）。

### Fixed（正确性）

- 现货回测费用按 raw 名义额计收：`BarMatchingEngine::fee_price_multiplier` 与 `contract_size`
  同为 SCALE 定点倍数，历史上被当成整数乘数传入，现货手续费被整体压小 10⁹ 倍，
  而既有费用断言全部使用 0 bp 因此无人捕获；`crates/qx-xingban/src/backtest.rs`
  的 `fees_scale_with_raw_notional_for_spot_and_derivative` 现钉住现货与永续两条口径。
- 执行层关联号未按订单定序导致**同一 EventLog 的第二笔订单事实被静默丢弃**（P0）：EventLog 幂等键是
  `{correlation_id}:source:{source_seq}`，而 `execute_paper_submit_effect` 每收到一条 SubmitOrder 命令就把
  `source_seq` 从 0 重新计数，Venue 回报关联号只到 `{worker}:venue:{venue_id}`、成交回报关联号只到
  `{worker}:fill:{source_seq}`。于是多腿 spread 的第二条腿（以及策略第二轮迭代的订单）其 `Accepted`、
  `Filled` 与派生的两条 `LedgerApplied` 全被判为重放丢弃，组状态永远停在 `PartiallyFilled`，账本少记一笔；
  此前所有 Paper 用例与 smoke 都只跑一笔订单，因此从未暴露。现按订单定序
  （`{worker}:venue:{venue_id}:{client_order_id}`、`{worker}:fill:{order_id}:{source_seq}`、
  `paper-execution:market:{instrument}:{source_seq}`），同一订单内仍由 `source_seq` 区分，重复提交的幂等语义不变。
  由 `crates/qx-cli/src/tests_main.rs::paper_multi_leg_spread_submits_each_leg_through_single_track_and_reduces_group`
  逐单断言"每笔腿各留 Accepted + Fill + 双 Ledger 条目"钉住（门禁记录见 V9 §8.2 反向验证 F）。
- 交易所**回报侧**两类会污染账本的事实错误（Phase 4j）：
  (1) CCXT 与 Binance 适配器在订单已终态（`Cancelled`/`Rejected`/`Filled`）后仍会把远端累计量的推进
  当成新增量，凭空补出一条 `Fill`（撤单落地后远端仍有成交的常见场景），Binance 的 `mark_cancelled`
  亦会把终态订单回退成 `PartiallyFilled`；
  (2) 违反 tick/step 精度的成交回报被直接记入账本，而无精度校验。
  现统一拒收：终态不回退（各适配器把远端量映射成事件处先查本地状态——`crates/qx-adapter/src/binance.rs`
  的成交回报与 `mark_cancelled`、`crates/qx-adapter/src/ccxt.rs` 的 `sync_order`，本轮日志计得
  `TERMINAL_GUARD_HITS=6` 处）、归约入口 `crates/qx-runtime/src/pipeline.rs` 对终态后的变更
  只产出 `ReconcileRequired` 事实（拒绝必须以"结果未知"分类返回，否则 worker 会把它当硬错误而不入对账队列），
  绝不伪造成交/撤单。精度闸门落在三条生产回报路径的唯一漏斗
  `ingest_venue_events_with_pipeline`（`crates/qx-execution/src/lib.rs`）而非 `Ledger`，因此回测口径不受影响；
  `crates/qx-cli/src/venue_runtime/binance_stream_worker.rs` 的 Binance 用户流此前拿不到产品规格，
  现经 `worker_report_spec()` + `ingest_venue_events_with_spec` 接入冻结规格
  （模板 `deploy/qianxing.runtime.production.example.json` 的 `binance-user-main` 补 `instrument_spec_path`）。
- `tools/verify_cpp_worker.py` 曾把 `--protocol` 硬编码为 `shared_memory_json`，
  列式协议 `shared_memory_columnar` 从未被真正执行；改为透传协议后两种协议均通过。
- Python wheel 打包：`python/pyproject.toml` 显式收敛 `packages.find` 范围（此前会把
  `python/build/lib/...` 递归打进 wheel），构建脚本按平台改写导入名
  （`_qianxing_native.pyd` / `.so`），并声明 `tzdata; sys_platform == 'win32'`
  使 `zoneinfo` 用例在 Windows 不再整模块报错。
- `cpp/CMakeLists.txt` 在找不到系统 `nlohmann_json` 时回退到 FetchContent，
  Windows/macOS 无需预装依赖即可配置。

### Added（链路与验收）

- Binance Spot **testnet** 拓扑模板 `deploy/qianxing.runtime.binance-testnet.example.json`
  与 fail-closed 验收驱动 `tools/binance_testnet_acceptance.py`
  （无凭据退出 3，`--allow-skip` 供 CI 跑离线半边；重复 `request_id` 必须被幂等拒绝）。
- 后端契约测试：`crates/qx-storage/tests/outbox_backend_semantics.rs` 增加 PostgreSQL
  租约/围栏令牌/重试契约；新增 `crates/qx-storage/tests/nats_jetstream.rs`
  （JetStream 一发一收 + 按 `event_id` 幂等，流与消费者由测试自建自删）。
- CI 从 4 个作业扩为 7 个：新增 `python-wheel`（Linux/Windows × Py 3.10/3.12/3.13）、
  `service-backends`（postgres:16 + `nats -js` 服务容器跑 `--ignored` 契约）、
  `venue-acceptance`；`cpp-sdk` 扩为 ubuntu/windows/macos 三平台并覆盖两种共享内存协议。
- 三家共用的回报契约测试 `crates/qx-execution/tests/venue_report_contract.rs`（937 行）：
  Paper（真实 `PaperVenue` 撮合）、CCXT（脚本化 `CcxtRpc` 报文）、Binance
  （`NoRestTransport` + 真实 `executionReport` 用户流报文）跑同一份
  `assert_venue_report_contract` 断言序列（基线成交与重放幂等 → 乱序旧累计回报不回退 →
  撤单后迟到成交 → 成交后迟到撤单 → off-tick 回报 → off-step 回报），
  全部经同一个 `LiveEventPipeline` 归约，只断言可观察事实（Fill/Cancelled/Reconcile 计数、
  Ledger 条目数、订单状态）。`tools/check_architecture.py` 随之从 22 项扩到 26 项，新增
  "精度闸门只在唯一归约入口生效并转待对账""精度判定只有一个谓词与一个调用点（不在 Ledger/回测侧重复）"
  "每条生产回报路径都带冻结产品规格""三家共用契约测试在位"四条不变量；
  三项注入反向验证（G 摘掉精度闸门、H/I 分别摘掉 Binance 与 CCXT 的终态守卫）都令契约用例转红
  `MUTATED_*_TEST_EXIT=101` 且还原后 `RESTORE[*]=identical` 复绿。
  收口日志：`TEST_EXIT=0`、工作区 `RUST_PASSED=526`（`OK_LINES=68` 个测试套件全部 ok）、
  `--bin qx-cli` 默认 56 / 全特性 59、`ARCH_EXIT=0`（26 项）、Python 43 例 OK、
  同一回测命令两次 `result_hash=b26e1d4d4d430cb1` 一致。
- `python/tests/test_native_extension.py` 新增 Rust 扩展与纯 Python 指纹一致性用例。

### Removed（破坏性清理，决策 2）

- 回测入口从 8 种收敛到 `backtest builtin` / `backtest multi-builtin` 两条。
- `qx-domain`、`qx-kernel` crate 与 `qx-zhenlu` 死导出删除；`RiskGate` 退化为 `RuleSet` 门面。
- 第二个多腿编排入口删除：`MultiVenueSpreadExecutionService`（含 `SpreadExecutionOutcome` 与
  `apply_spread_application_event`）+ `VenueRouterMap` 共 347 行。它从未被生产代码构造，且缺少生产链路
  必需的两道门（无账户级风控预检、无"行情必须来自 EventLog 事实"的 fail-closed 门禁），若启用还会与
  worker 的逐腿提交双写事实。其唯一独有的安全语义（组内有腿结果未知或待补偿时禁止继续提交其余腿）
  收进 `crates/qx-zhenlu/src/lib.rs::SpreadOrderGroup::blocks_new_leg_submission()` 单点谓词，由
  `crates/qx-cli/src/spread.rs::spread_group_barrier()` 在五个腿提交点（Paper 一次性与 worker 循环、
  CCXT、Binance 一次性与 worker 循环）执行前拦截，被拒命令以 `FAIL_CLOSED:` 前缀进控制面终态审计。
  `VenueRouterPort`/`VenuePortAdapter`/`BorrowedVenuePort` 保留（`HedgeRecoveryWorker` 补偿路径在用）。
  架构门禁由 20 项增至 22 项：第二编排入口标识符出现即为违规、屏障谓词全仓唯一定义、
  任何调用腿提交入口的文件必须同时调用屏障。

### Changed（Phase 4：qx-cli 内部模块化，仍是单 binary）

- `crates/qx-cli/src/main.rs` 从 16,359 行降到 3,601 行，按职责拆出 13 个兄弟模块：
  `cli.rs`（命令名 → 处理器的唯一分派点）、`selfcheck.rs`、`config_commands.rs`、
  `strategy_host.rs`、`strategy_contract.rs`、`backtests.rs`、`multi_leg.rs`、`ccxt_facts.rs`、
  `spread.rs`、`venue_runtime.rs`、`worker_entry.rs`、`event_pipeline.rs`、`tests_main.rs`。
  搬迁以整块原样移动 + `pub(crate)` 收口进行，未改任何算法。
- 未知命令从"静默落到 `all` 自校验演示"改为打印 `未知命令: <x>`、帮助与退出码 2。
  既有命令与参数宽松度（`--json` 可出现在任意位置、`--strategy=` 混写等）保持不变，
  因此未引入 clap `Subcommand`；理由与保留项见 V9 §5 Phase 4 与 §8.3。
- CCXT 与 Binance 两条 worker 入口的角色校验合并为 `worker_entry.rs` 里的单张表：
  `VENUE_ROLES` 白名单 + `VenueEntry::{CCXT, BINANCE}` 登记表 + `venue_worker()` 校验入口，
  "存在性→启用→角色→Venue 绑定"四步与错误文案只有一份实现。
- `QX_PYTHON` 解释器解析从 9 处 `std::env::var` 收敛为 `python_interpreter()` 一处。
- `backtest builtin` 与多腿腿级回测补上 `strategy_risk_gate`（前者保守禁空，后者允许对冲空头腿），
  审计时点记录的"空风控门回测入口"至此全部走同一规则内核；进程内合成演示链路保持原样。
- 三条 Bar 回测链的引擎装配收进 `crates/qx-cli/src/backtests.rs::BarBacktestAssembly`：
  乘数 1、`NextBarOpenFillModel`、`ZeroLatency`、`DataTier::Bar`、初始资金 100_000 与
  "缺省即带风控门"只留一份实现，各入口只覆盖会分叉的费用、保证金、风控与种子；
  market spec 读取合并为 `market_spec_with_margin()`（策略/内置/多腿/深度四处，错误文案统一带上路径），
  内置策略执行段合并为 `run_builtin_strategy_on_bars()`。同一轮内两次运行
  `backtest builtin sma_cross` 的 `result_hash` 均为 `b26e1d4d4d430cb1`，装配收敛未破坏确定性。
- 两条此前无测试的内置回测入口补 3 例（共用装配默认口径、输入校验 fail-closed、
  多腿归因产物按腿级真实成交计提且费用等于 taker 5 bp），见 `crates/qx-cli/src/tests_main.rs`。
- `selfcheck::run(&str)` 里残留的第二处命令名分派（`mode == "backtest" || mode == "verify"`，
  其中 `backtest` 分支在 Phase 4 收敛后已永不可达）改由 `cli.rs` 以
  `selfcheck::Scope::{KernelOnly, Full}` 传意；`verify` 仍止于确定性内核、`all` 仍续跑插件装配与
  Paper 冒烟，两者的退出码与阶段结论文本均未变（由 `crates/qx-cli/tests/cli_dispatch.rs` 断言）。
- 新增 `tools/check_architecture.py`（架构不变量自检，落地时 12 项、本轮扩为 14 项）并接入 `ci.yml` 的 `rust-core` 作业：
  已删 crate 不得复活、`QX_PYTHON` 单点读取、命令名分派只允许出现在 `cli.rs`、
  生产代码不得有未登记或无规则的空风控门、`BacktestConfig` 装配字面量唯一、
  market spec 与 `strategy_risk_gate` 保持单一入口、能力矩阵四档状态与证据路径可核验且
  `sandbox_tested` 不得越界为 `true`、单文件行数按 `maturity/line_budgets.yaml` 快照只降不升
  （`--snapshot` 重新生成）。README 的本地验证段同步加入该命令。
- 新增 `crates/qx-cli/tests/cli_dispatch.rs` 3 例集成测试，从二进制外部钉住 `verify` / `all` /
  未知命令三条分派语义；README 快速开始里重复的一行裸 `backtest` 示例已删除。

### Fixed（正确性：Paper 主链路仍在第二套实现上）

- `crates/qx-execution/src/lib.rs::execute_paper_submit_effect`（qx-cli Paper 路径的唯一生产入口）此前仍
  构造端口化之前的遗留 `ExecutionService`，因此 V9 §8.1 的"执行统一走 `PortExecutionService`"对 Paper
  并不成立：遗留实现缺少 gateway 的"空 Venue 回报 = 未知结果 → 待对账"fail-closed 语义。现改为
  `ExecutionGateway`（`PortExecutionService` 别名）+ `BorrowedVenuePort`，风控预检、幂等与事件写入判定
  只由 gateway 一处决定；两条文档注释与 `ExecutionService` 的定位同步收口为"仅由两个多腿编排器复用"。
  改道后订单级风控由 gateway 的 `CanonicalRiskPort` 统一评估：缺 `TradingInstrumentSpec` 的 Paper 提交会以
  `订单级风控缺少 TradingInstrumentSpec` 被拒（本轮实测口径，新用例据此带上 spec；两套实现在这一点上的
  历史差异未做对照，故不声称行为变化方向）。
- 新增 `crates/qx-execution/src/tests.rs::paper_submit_reuses_gateway_idempotency_without_new_facts`：
  从生产入口重投同一 `client_id`，断言返回 `ALREADY_APPLIED_FROM_EVENT_LOG` 且订单数、账簿条目数均不增长。
- `tools/check_architecture.py` 增加两项执行侧不变量（共 14 项）：遗留 `ExecutionService` 不得被
  `qx-execution` 之外的任何路径引用；`execute_paper_submit_effect` 必须出现 `ExecutionGateway::new`
  且不得回退 `ExecutionService::new`。两项均已反向验证（注入占位实现 + 他 crate 注释提及 → 同时 FAIL，
  还原后 `cmp` 字节一致、14 项复绿）。
- `crates/qx-execution` 的 1,244 行内嵌 `#[cfg(test)] mod tests` 拆到 `src/tests.rs`（沿用 qx-cli 的
  `mod tests;` 形态，私有项可见性不变），`src/lib.rs` 从 3,225 行降到 2,072 行；
  `maturity/line_budgets.yaml` 相应登记 43 个超 500 行文件（新增 `src/tests.rs`，`lib.rs` 预算下调）。

### Removed（Phase 3 第 2 项收口：第二套执行实现整体删除）

- 删除端口化之前的 `ExecutionService`（含 238 行 `impl` 与从未被任何调用方使用的 `new_with_spec`）
  和唯一复用它的多腿编排器 `SpreadExecutionService`（文档+结构体+`impl` 136 行），连同只服务两者的
  `apply_spread_venue_events`（14 行），`crates/qx-execution/src/lib.rs` 从 2,072 行降到 1,662 行。
  至此本 crate 内不存在任何绕过 `ExecutionGateway` 的订单副作用路径，多腿编排入口只剩
  `MultiVenueSpreadExecutionService` 一个。删除前实测确认：两者在全仓 Rust 代码里的生产构造点为 0
  （仅 `src/tests.rs` 触达），Paper/Live/CCXT 三条单腿路径与 CLI 多腿路径均已走 gateway。
- 被删编排器的 Paper 多腿生命周期语义没有丢：`spread_execution_submits_all_legs_and_keeps_group_lifecycle`
  改写为 `crates/qx-execution/src/tests.rs::paper_multi_leg_spread_routes_each_leg_and_keeps_group_lifecycle`，
  改由 `MultiVenueSpreadExecutionService` + `VenueRouterMap`（两条腿各挂一个 `PaperVenue` 适配器）驱动，
  断言集合原样保留（无错误、组停在 `Submitting`、补偿目标为空、管线内两笔订单、快照持久化后状态一致）。
- `tools/check_architecture.py` 的执行侧不变量从"限制遗留实现扩散"升级为"禁止其存在"（共 16 项）：
  `ExecutionService`/`SpreadExecutionService` 两个标识符在全仓 Rust 代码中出现次数必须为 0；
  `execute_paper_submit_effect` 函数体必须含 `ExecutionGateway::new`；`qx-execution` 里
  `*SpreadExecutionService` 形态的 `pub struct` 必须恰好只有 `MultiVenueSpreadExecutionService` 一个；
  `pub type ExecutionGateway` 别名定义必须唯一。已知盲区记入 V9 §8.3 第 6 项：该判定是文本级，
  改名后的第三套实现不会被它拦住。
- `maturity/line_budgets.yaml` 用 `--snapshot` 同步：diff 只有两行且均为下降
  （`lib.rs: 2072 → 1662`、`tests.rs: 1244 → 1242`），其余 41 条一字未动。
- `maturity/capabilities.yaml` 的 `multi_leg_execution.limitations` 按实测重写：删除已失效的
  `spread_execution_services_are_constructed_only_in_unit_tests`，改为
  `multi_venue_orchestrator_is_constructed_only_in_unit_tests` 与
  `venue_router_map_has_no_production_registration_site` 两条准确表述。

### Validation（phase4f 轮实测，日志 `/tmp/qx_phase4f_gate.log` + `/tmp/qx_phase4f_mutation.log`，2026-09-19：Paper 并入 ExecutionGateway 单轨 + `qx-execution` 测试模块拆分后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=66; RUST_PASSED=523 RUST_FAILED_SUITES=0
cargo test -p qx-cli --bin qx-cli: 54 passed；--features sqlite,postgres,nats: 57 passed
PY_EXIT=0  Ran 43 tests in 0.064s  OK
VALIDATE_EXIT=0  全部自校验通过 ✓
ARCH_EXIT=0      架构不变量自检全部通过 ✓（14 项）
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 未知命令=2 binance-worker 非法角色=2
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1（与上一轮同值）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
         live-check 只读校验：network_accessed=False orders_sent=False
执行单轨门禁反向验证：注入占位 gateway + 他 crate 注释提及 ExecutionService → MUTATED_ARCH_EXIT=1
                     （两项 FAIL：Paper 共用 ExecutionGateway / 遗留实现不被外部引用）
                     还原后 cmp 判定两份文件字节一致、RESTORED_ARCH_EXIT=0（14 项）
未提交改动：UNCOMMITTED_PATHS=121（本轮全部工作仍未提交，未获提交授权）
```

`postgres` / `nats` 的契约测试仍以 `#[ignore]` 等待首次服务容器 CI 记录，
`maturity/capabilities.yaml` 中所有 `sandbox_tested` 保持 `false`。

### Validation（phase4g 轮实测，日志 `/tmp/qx_phase4g_gate.log`，2026-09-19：第二套执行实现删除后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=66; RUST_PASSED=523 RUST_FAILED_SUITES=0        # 与上一轮逐位相同（本轮是替换测试而非新增）
cargo test -p qx-cli --bin qx-cli: 54 passed；--features sqlite,postgres,nats: 57 passed
PY_EXIT=0  Ran 43 tests in 0.063s  OK
VALIDATE_EXIT=0  全部自校验通过 ✓
ARCH_EXIT=0  ARCH_ITEMS=16   架构不变量自检全部通过 ✓（16 项）
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 未知命令=2 binance-worker 非法角色=2
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1（与上一轮同值）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
         live-check 只读校验：network_accessed=False orders_sent=False
门禁反向验证：在 compensation_client_id() 注入一行 `ExecutionService::new(0)`
             → MUTATED_ARCH_EXIT=1（第二套实现已移除 / 行数棘轮 两项 FAIL）
             → RESTORE_CMP=identical、RESTORED_ARCH_EXIT=0（16 项）
未提交改动：UNCOMMITTED_PATHS=121（本轮全部工作仍未提交，未获提交授权）
```

Phase 3 第 2 项的判定随之收窄：单腿与多腿的**副作用实现**已合一（一套 gateway + 一个多腿编排器），
仍未闭合的是**编排入口**合一——CLI 多腿生产路径以逐腿命令 + 组快照归约推进生命周期，
`MultiVenueSpreadExecutionService` 与 `VenueRouterMap` 尚无生产构造点（V9 §8.3 第 7 项）。

### Added（Phase 5 门禁收口：A 股 PIT 结果可区分）

- 新增 `crates/qx-xingban/tests/ashare_pit_asof.rs`（3 例）：同一份录制分红快照在
  发布前/发布后两个研究截止日（`as_of`）下分别加载，走完整引擎后现金差额恰为
  `100 股 × 每股 1 元`、`result_hash` 互不相同；另有"同一截止日两次运行 `result_hash`
  与 `replay_hash` 一致"的可复现钉子。它兑现的是 V9 §5 Phase 5 门禁里此前唯一没有
  证据的条款（L1/L2 与 SQLite 两条早有覆盖，A 股 PIT 这条只写了文字）。
- `tools/check_architecture.py` 由 16 项扩到 20 项，新增 4 条 A 股 PIT 不变量：
  第二道 PIT 闸门（`is_visible_at`/`corporate_actions_visible_at`）出现即为违规、
  可见性谓词与加载闸门调用点各唯一、回测引擎不得自行判定 PIT 可见性、
  上述结果可区分测试必须在位。
- `maturity/capabilities.yaml` 的 `ashare_corporate_action_ledger.evidence` 登记新测试文件，
  `limitations` 增加 `pit_asof_filter_runs_only_at_corporate_action_json_load`；
  冒烟门禁新增 `fast-backtest ashare` 一条命令（phase5b 轮实测退出 0）。

### Removed（Phase 5 收口：第二道 PIT 闸门是零调用者死代码）

- 删除 `AshareCorporateActionEvent::is_visible_at` 与
  `AshareRuleConfig::corporate_actions_visible_at`：全仓零调用，且 `AshareRuleConfig`
  不携带 `as_of`，把它们接进运行时会等于新增语义而非收口。PIT 可见性从此只有
  公司行为 JSON 加载闸门一处判定，`published_at_ms` 字段文档同步改写为"随快照携带供审计"。
  `crates/qx-xingban/src/ashare.rs` 2,006 → 1,978 行，`maturity/line_budgets.yaml`
  重跑 `--snapshot` 后 diff 仅此一条下降，其余 42 条一字未动。

### Changed（Phase 4h：`venue_runtime.rs` 按 Venue 边界拆成目录模块，仍是单 binary）

- `crates/qx-cli/src/venue_runtime.rs`（2,823 行、39 个顶层条目）拆为
  `crates/qx-cli/src/venue_runtime/`：CCXT 侧 `ccxt_execution / ccxt_live_bars /
  ccxt_market_worker / ccxt_reconcile_worker`，Binance 侧 `binance_venue /
  binance_stream_worker / binance_reconcile / binance_submit`，跨 Venue 的
  `worker_runtime`（worker 路径解析、风控上下文、共享提交 effect），Paper 侧
  `paper_submit / paper_worker`，外加 27 行 `mod.rs` 做 `pub(crate) use …::*;` 再导出。
  最大子文件 423 行，12 个文件全部低于 500 行门槛，`maturity/line_budgets.yaml` 里的
  超 500 行文件随之从 43 个减到 42 个。
- 纯搬迁：39 个条目逐个与原文件比对，只有 `LiveStrategyBarSpec` 因跨子模块读字段把
  7 个字段升为 `pub(crate)`，其余逐字相同；crate 根的
  `pub(crate) use venue_runtime::*;` 一字未改，其他模块与 `tests_main.rs` 的引用路径不变。
- `tools/check_architecture.py` 的"命令分派单点"检查从单层 `glob("*.rs")` 改为
  `rglob("*.rs")`——这是拆目录的前置条件（旧写法会让子目录里的第二分派点逃出检查，
  此前作为已知盲区记录在 V9 §8.3 第 6 项），违规输出现在带模块内相对路径。
- `maturity/capabilities.yaml` 三条指向旧单文件的证据路径改指 `venue_runtime/` 内的新文件。

### Validation（本轮实测，日志 `/tmp/qx_phase4h_gate.log`，2026-09-19：venue_runtime 拆目录模块后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=67; RUST_PASSED=526 RUST_FAILED_SUITES=0     # 与上轮（拆分前）逐位相同
cargo test -p qx-cli --bin qx-cli: 54 passed；--features sqlite,postgres,nats: 57 passed
venue_runtime/：12 个文件 2,857 行，最大 423 行（ccxt_execution.rs），全部 < 500
PY_EXIT=0  Ran 43 tests in 0.063s  OK
VALIDATE_EXIT=0  全部自校验通过 ✓
ARCH_EXIT=0  ARCH_ITEMS=20   架构不变量自检全部通过 ✓（20 项，与上轮同数）
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0
                 未知命令=2 binance-worker 非法角色=2
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1（与拆分前同值）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
         live-check 只读校验：network_accessed=False orders_sent=False
反向验证 C（递归分派检查真的生效）：在 venue_runtime/paper_worker.rs 注入 `if command == "paper"`
         → MUTATED_ARCH_C_EXIT=1，唯一 FAIL 是
           `命令名分派只存在于 cli.rs — 额外分派点 ['venue_runtime/paper_worker.rs:…']`
         → RESTORE_C_CMP=identical、RESTORED_ARCH_EXIT=0（20 项）
反向验证 D（旧单文件路径不得当证据）：把能力矩阵一条路径改回已删除的 venue_runtime.rs
         → MUTATED_ARCH_D_EXIT=1，`能力矩阵证据路径全部存在 — 失效路径 ['crates/qx-cli/src/venue_runtime.rs']`
         → 改回后 RESTORED_ARCH_D_EXIT=0
未提交改动：UNCOMMITTED_PATHS=122（Phase 0–6 全部工作仍未提交，未获提交授权）
```

同一轮还修正了门禁脚本自身的一处错误：`fast-backtest` 冒烟最初被写成
`fast-backtest ashare deploy/qianxing.runtime.ashare.example.json`（把 venue 名当 manifest
路径传）而退出 2；改为 `fast-backtest deploy/qianxing.fast-backtest.ashare.example.json`
后上面的 0 才是真实结果——上面的日志是修正后的复跑。

### Validation（phase5b 轮实测，日志 `/tmp/qx_phase5b_gate.log`，2026-09-19：A 股 PIT 收口后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=67; RUST_PASSED=526 RUST_FAILED_SUITES=0     # 上一轮 66 / 523，+1 目标 +3 例全在新测试文件
cargo test -p qx-xingban --test ashare_pit_asof: 3 passed
cargo test -p qx-cli --bin qx-cli: 54 passed；--features sqlite,postgres,nats: 57 passed（与上一轮相同）
PY_EXIT=0  Ran 43 tests in 0.176s  OK                 # 该轮无 Python 改动，用例数持平
VALIDATE_EXIT=0  全部自校验通过 ✓
ARCH_EXIT=0  ARCH_ITEMS=20   架构不变量自检全部通过 ✓（20 项）
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0
                 未知命令=2 binance-worker 非法角色=2
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1（与上一轮同值）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
         live-check 只读校验：network_accessed=False orders_sent=False
门禁反向验证 A（死代码不得复活）：把 `is_visible_at` 原样注回 ashare.rs
         → MUTATED_ARCH_A_EXIT=1（第二道 PIT 闸门 / 行数棘轮 1985>1978 两项 FAIL）
         → RESTORE_A_CMP=identical
门禁反向验证 B（加载闸门必须真的在过滤）：`if !contract.visible_at(..)` 改成恒不隐藏
         → MUTATED_PIT_TEST_EXIT=101，3 例中 2 例转红，"同一截止日可复现"一例仍 ok
           （它只钉确定性、不依赖过滤，符合预期）
         → RESTORE_B_CMP=identical、RESTORED_PIT_TEST_EXIT=0（3 passed）、RESTORED_ARCH_EXIT=0（20 项）
未提交改动：UNCOMMITTED_PATHS=122（Phase 0–6 全部工作仍未提交，未获提交授权）
```

该轮同时记下门禁自身的边界（V9 §8.3 第 6 项，其中"命令分派只扫单层目录"的盲区已在 Phase 4h 闭合）：新增 4 条与既有各条同为文本/正则级形状检查，
谓词改名可绕过、测试辅助函数改名会误报缺失；命令分派单点那条只扫 `crates/qx-cli/src/*.rs`
单层 glob，后续把 `venue_runtime.rs` 拆进子目录前必须先改成递归。

### Changed（Phase 4k：内核 `Ledger` 按资产类别拆成目录模块，只拆文件不改语义）

- `crates/qx-core/src/ledger.rs`（2,876 行、42 个 `pub fn`，把现货成交、合约成交与乘数、现金
  （入金/资金费/利息/交收/调整/强平）、公司行为、权证与认购、可转债转股、查询投影混在一个文件里）
  拆为 `crates/qx-core/src/ledger/`：`fill.rs` 261 / `cash.rs` 127 / `corporate_action.rs` 173 /
  `rights.rs` 255 / `subscription.rs` 367 / `query.rs` 341，外加 `mod.rs` 373 行持有全部类型定义、
  `pub struct Ledger` 字段与唯一的归约入口 `apply_entry`。每个子文件恰含一个 `impl Ledger` 块，
  子模块直接读写 `Ledger` 的模块私有字段，因此没有为了拆文件而放宽任何 API：
  `crates/qx-core/src/lib.rs` 的 `pub use self::ledger::{…}` 出口一字未改。
- 18 例内核账簿用例从 `ledger.rs` 尾部的 `#[cfg(test)] mod tests` 搬到
  `crates/qx-core/tests/ledger.rs`（1,031 行），改成只用公开 API 的集成用例；`cargo test -p qx-core`
  从"lib 52 例"变成"lib 34 例 + 集成 18 例"，`CORE_TEST_FNS=52` 不变。
- `tools/check_architecture.py` 由 26 项扩到 30 项：新增"内核 `Ledger` 单文件已拆分且不得复活"
  "账簿状态结构体只有一处定义""账簿归约实现按资产类别分文件，且不在目录外另起 `impl Ledger`"
  "账簿各子模块都在单文件行数门槛之内"。`maturity/line_budgets.yaml` 登记数 42 → 41
  （完整 diff 只有一行：删掉 `crates/qx-core/src/ledger.rs: 2876`）；
  `maturity/capabilities.yaml` 的 `ashare_corporate_action_ledger` 证据路径改指新模块与新的集成用例。
- 中立性的额外证据（不只靠"测试没变红"）：把拆分前后所有行做归一化（去空行、`//!` 文档行、
  `use`/`mod` 行、裸 `}` 与 `impl Ledger {` 骨架行，并把 `pub(super) fn` 折回 `fn`）后取多重集比对，
  两侧各 2,578 行且双向差集为空（`CARVE_EQUIV_EXIT=0`）。
- 该轮留下的已知形状例外：行数棘轮只扫 `crates/*/src/**/*.rs`，因此 1,031 行的
  `crates/qx-core/tests/ledger.rs` 不在登记集内（V9 §8.3 第 5 项已记）。

### Validation（phase4k 轮实测，日志 `/tmp/qx_phase4k_gate.log`，2026-09-19：内核 Ledger 拆目录模块后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0
OK_LINES=69; RUST_PASSED=526 RUST_FAILED_SUITES=0     # 用例总数与上轮逐位相同；68 → 69 来自新测试二进制
cargo test -p qx-core: 34 passed（lib）+ 18 passed（tests/ledger.rs）+ 0 passed（doc），CORE_TEST_FNS=52
cargo test -p qx-execution --test venue_report_contract: 1 passed
cargo test -p qx-cli --bin qx-cli: 56 passed；--features sqlite,postgres,nats: 59 passed
src/ledger/：cash 127 / corporate_action 173 / fill 261 / mod 373 / query 341 / rights 255 /
             subscription 367 行；tests/ledger.rs 1,031 行；合计 2,928
IMPL_LEDGER_BLOCKS=7  IMPL_LEDGER_OUTSIDE_DIR=0  OLD_SINGLE_FILE_EXISTS=no
CARVE_EQUIV_EXIT=0（归一化 2,578 → 2,578，双向差集为空）  REGISTERED_OVERSIZED=41
PY_EXIT=0  Ran 43 tests in 0.063s  OK
VALIDATE_EXIT=0  全部自校验通过 ✓
ARCH_EXIT=0  ARCH_ITEMS=30   架构不变量自检全部通过 ✓（26 → 30 项）
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 runtime-check-binance=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0
                 未知命令=2 binance-worker 非法角色=2
                 runtime-check production=2（模板引用部署机绝对路径，本机必然退 2，记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1
         与 Phase 4j 基线同值（DETERMINISM=same、HASH_VS_BASELINE=same）
门禁反向验证 J（不得在目录外另起 impl Ledger）：在 crates/qx-core/src/engine.rs 追加一个
         `impl Ledger { pub fn stray_impl_probe(…) -> u64 { 0 } }`
         → MUTATED_J_ARCH_EXIT=1（目录外 {'crates/qx-core/src/engine.rs': 1}）
         → RESTORE[J]=identical、RESTORED_J_ARCH_EXIT=0
门禁反向验证 K（被拆掉的 2,876 行单文件不得复活）：把原文件原样注回
         → MUTATED_K_ARCH_EXIT=1，四条同时转红：单文件复活 / `pub struct Ledger` 两处定义 /
           目录外 {'crates/qx-core/src/ledger.rs': 2} / 行数棘轮"2876 行未登记"
         → 确认 OLD_SINGLE_FILE_REMOVED=yes、RESTORED_K_ARCH_EXIT=0
用例反向验证 L（搬进 tests/ 的 18 例仍咬得住语义）：把 `apply_position_state_delta` 判定平仓方向的
         `if current > 0 {` 改成 `if current < -1 {`（多/空已实现盈亏符号分支互换）
         → MUTATED_L_TEST_EXIT=101、L_FAILED_CASES=3，含 tests/ledger.rs:285
           （accounts_and_realized_pnl_are_isolated）与 :452
           （multiplier_is_preserved_in_realized_pnl_replay）
         → RESTORE[L]=identical、RESTORED_L_TEST_EXIT=0（lib 34 + 集成 18 两侧复绿）
未提交改动：UNCOMMITTED_PATHS=131（Phase 0–6 全部工作仍未提交，未获提交授权）
```

### Removed（Phase 4l：持仓概念的第二份实现是逐字复制，删）

- `crates/qx-zhenlu/src/lib.rs` 的 `pub struct PositionSnapshot`（五个 `i128` 字段）与它的
  `impl`（`new` / `new_with_multiplier`（含 `multiplier.max(1)` 钳位）/ `with_hedge_legs` /
  `active_qty_for` / `canonical_position`）、`crates/qx-genglu/src/lib.rs` 的同名结构体，
  都是 `qx_risk::OrderRiskPosition` 的逐字复制：字段一一对应，`active_qty_for` 除 `qx_core::` 路径前缀
  与 `&self`/`self` 接收者写法外完全相同。三个构造函数搬到 `OrderRiskPosition` 上
  （孤儿规则不允许在 zhenlu 给外部类型写 impl），两份重复结构删除，
  `RiskContext` / `legacy_gate_context` / `RiskGate::check*` 的签名随之改用 `&OrderRiskPosition`。
- `qx-genglu::reconcile_positions` + `position_map` + 唯一由它构造的 `Discrepancy::PositionMismatch`
  变体删除（`qx-genglu/src/lib.rs` 627 → 559 行，`qx-zhenlu/src/lib.rs` 2,318 → 2,210 行）。
  删除依据是零调用者 + 职责已有归属：
  持仓快照在生产线由 CCXT worker 的 `fetch_positions` 采集
  （`crates/qx-cli/src/venue_runtime/ccxt_reconcile_worker.rs`）、由 Binance 对账 worker 以
  `position_snapshots_count` 上报（`binance_reconcile.rs`），而 genglu 那份既不接风控也不接 fail-closed 门禁。
- `OrderIntent::validate` / `OrderIntent::validate_against` 及只为它们服务的私有自由函数
  `validate_reduce_only(order, position)` 删除。该函数与 `qx_risk::OrderRiskContext::validate_reduce_only`
  逐字相同（连 `"reduce_only 订单必须只减少目标持仓腿且不得反向穿仓"` 文案都一致），
  而 reduce-only 判定在 `RiskGate::check → RuleSet::evaluate_rules_only → rule_violations`
  这条活路径上已由规范实现承担（`crates/qx-risk/src/rules.rs:203`）。
  原 zhenlu 用例 `rebalance_intent_rejects_quantity_overflow` 里对死校验器的那半段断言随之删除，
  保留 `target_qty: i128::MIN` 时 `rebalance_intent(...)` 返回 `None` 的断言。
- 核实后**保留**的两组同名概念：`Bar` 两份（`qx_guanxing::Bar` 是引擎六列定点值对象，
  从数据集进入引擎只有一次投影 `impl From<&BarFrame> for Vec<Bar>`；`qx_data::schema::Bar` 是数据集侧
  逐条记录，全仓只有 `qx-data` 内部使用、没有任何一处转成引擎 Bar）；
  Intent 四份（`StrategyOrderIntent` Rust SDK 原生 → `StrategyContractIntent` 跨语言 JSON 线格式，
  投影只有 `StrategyContractOutput::from_native_decision` 一处 → `QxOrderIntent` 是 `#[repr(C)]` ABI 镜像，
  消费方为 `cpp/include/qianxing_strategy.h` 与 `cpp/examples/momentum_strategy.cpp` →
  `OrderIntent` 是风控前的下单意图，唯一调用点 `crates/qx-cli/src/strategy_contract.rs` 的
  `rebalance_intent(...).into_order()`）。它们是分层投影而非重复实现，只做登记不做合并。

### Changed（Phase 4l：概念权威定义进登记表，架构不变量 30 → 41 项）

- `tools/check_architecture.py` 新增 `concept_registry_check()`，共 11 项：
  8 个概念名（`PositionState` / `OrderRiskPosition` / `PositionSnapshot` / `Bar` / `OrderIntent` /
  `StrategyOrderIntent` / `StrategyContractIntent` / `QxOrderIntent`）的"定义位置与登记表一致"——
  登记表外的新定义与登记表内被搬走或改名的定义**两侧都报红**；再加
  "数据集列式 BarFrame 到引擎 Bar 只有一次投影定义"、
  "已删除的第二套持仓归约与死校验器不得复活"（`reconcile_positions` / `validate_against` / `position_map`
  三个标识符全仓命中数必须为 0，本轮实测 `DEAD_IDENTS=0`）、"持仓可用量判定只有一个实现"
  （实测 `ACTIVE_QTY_FOR=1`，唯一实现是 `crates/qx-risk/src/lib.rs:114`）。
- 改名波及的引用点全部接线：`qx-cli`（`main.rs` / `strategy_contract.rs` / `tests_main.rs` /
  `venue_runtime/worker_runtime.rs`）、`qx-execution`（`lib.rs` 与 `src/tests.rs`）、
  `qx-xingban`（`backtest.rs` / `orderbook_backtest.rs`）、`qx-risk/tests/risk_parity.rs`、
  `qx-zhenlu/tests/risk_projection.rs`。`qx_protocol::PositionSnapshot`（交易所/账户回报线格式）
  与被误改到的同名引用已复原，全仓该名字只剩 `qx-protocol` 一处定义。
- `maturity/line_budgets.yaml` 登记条数不变（41），本轮触及的 5 个登记项之和 8,331 → 8,158 行：
  `crates/qx-zhenlu/src/lib.rs` 2,318 → 2,210、`crates/qx-genglu/src/lib.rs` 627 → 559，
  另有 `crates/qx-execution/src/lib.rs`、`crates/qx-xingban/src/backtest.rs`、
  `crates/qx-xingban/src/orderbook_backtest.rs` 各 +1 行——类型改名后 `use` 从 `qx_zhenlu` 组里
  拆成独立一行，是本轮唯一的增长，已逐条审阅。

### Validation（本轮实测，日志 `/tmp/qx_phase4l_gate.log`，2026-09-19：持仓概念收敛 + 概念登记表落地后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=69; RUST_PASSED=526 RUST_FAILED_SUITES=0     # 与 Phase 4k 逐位相同
qx-core: tests/ledger.rs 18 passed / lib 34 passed；venue_report_contract 1 passed
cargo test -p qx-cli --bin qx-cli: 56 passed；--features sqlite,postgres,nats: 59 passed
概念清点: PositionState=1 OrderRiskPosition=1 PositionSnapshot=1 Bar=2 OrderIntent=1
          StrategyOrderIntent=1 StrategyContractIntent=1 QxOrderIntent=1
ACTIVE_QTY_FOR=1  DEAD_IDENTS=0  文件规模 zhenlu 2,210 / genglu 559 / risk 393
ARCH_EXIT=0  架构不变量自检全部通过 ✓（41 项，Phase 4k 的 30 项 + 本轮 11 项）
PY_EXIT=0  Ran 43 tests in 0.064s  OK     VALIDATE_EXIT=0  全部自校验通过 ✓
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 runtime-check-binance=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0
                 未知命令=2 binance-worker 非法角色=2
                 runtime-check production=2（模板引用部署机绝对路径，本机必然退 2，记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 Phase 4k 基线逐位相同
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（四次变异实验后工作树逐字节校验、无 .bak4l 残留）
UNCOMMITTED_PATHS=131
```

反向验证四次（同一段日志）：M 在 genglu 另写 `pub struct PositionSnapshot` → 登记项报红为
`实际 ['crates/qx-genglu/src/lib.rs', 'crates/qx-protocol/src/lib.rs']`（`MUTATED_M_ARCH_EXIT=1`）；
N 在回测内核另起 `fn active_qty_for` → 唯一实现项报红（`MUTATED_N_ARCH_EXIT=1`）；
O 注回 `pub fn reconcile_positions()` → "不得复活"项报
`{'reconcile_positions': ['crates/qx-genglu/src/lib.rs']}`（`MUTATED_O_ARCH_EXIT=1`）；
P 在第三处写 `pub struct Bar` → Bar 登记项报红（`MUTATED_P_ARCH_EXIT=1`）。
四次都还原到 `RESTORE[x]=identical` 且 `RESTORED_x_ARCH_EXIT=0`。

环境事实（本轮实测发现，已写进门禁脚本首行 `QX_PYTHON=…`）：`cargo test -p qx-cli --bin qx-cli` 的两条
Python strategy worker 用例依赖 `python_interpreter()` 的解释器解析，缺省回落 `python`；本机 `python` 是
WindowsApps 占位桩，不设 `QX_PYTHON` 时实测 `54 passed; 2 failed`，两条都 panic 于
"Strategy worker 已关闭输出"（`crates/qx-cli/src/tests_main.rs:2812` 与 `:2856`）。
显式指向可用解释器后 `56 passed; 0 failed`。属本机环境约束而非代码回归（CI 侧 `python` 真实可用），
但它记下一条尚未收的 fail-closed 缺口：那条 `unwrap_or_else(|_| "python".into())` 回落找不到解释器时
不给任何提示。

### Changed（Phase 4m：运行时配置面 fail-closed，架构不变量 41 → 50 项）

- 新增 `crates/qx-runtime/src/worker_policy.rs`（322 行）：`FieldScope`（`Required`/`Allowed`/`Forbidden`）、
  `WorkerRoleFieldScopes`（九个字段组）、`WorkerRole::field_scopes()`（逐角色显式声明，无通配臂，
  新增变体在编译期必须补表）、`WorkerRole::is_venue_role()` / `uses_private_venue()`、
  `ALL_WORKER_ROLES`、`RoleFieldStatus` 与 `WorkerConfig::role_field_status()`。角色能配哪些字段
  第一次成为类型系统里的一张表，而不是散落在 `validate()` 与 CLI 分支里的判断。
  策略表逐字取自 18 份运行时模板与每个字段的实际消费点：`Api` 角色的字段全仓无人读取，
  `MarketData` 可以合法携带 CCXT 私有端点凭据，`UserStream` 会复用同一份冻结规格做精度预检。
- 12 个运行时配置结构体逐个加 `#[serde(deny_unknown_fields)]`：把 `max_order_notional_raw` 拼成
  `max_order_notional_raws` 不再是"这条风控没配"，而是启动即失败。为兼容 deploy 模板里的中文说明键，
  `RuntimeConfig::from_json` 在反序列化前递归剥离 `_` 前缀键，且剥离发生在指纹计算之前，
  加注释不会改变 `config_fingerprint`。
- `RuntimeConfig::validate()` 的 worker 循环改为先调 `worker.role_field_status()` 再判 `enabled`：
  角色必填三件套、"该角色永不读取却配了它"、凭据来源二选一且内容有效、`paper_initial_cash_raw`
  只能配在 Paper 场地，四类判定统一在策略表一处；被删的重复实现包括角色 `match`、
  MarketData/UserStream 的 endpoint 必填分支、Paper 场地初始资金分支，以及只在
  `is_binance` 分支里生效的凭据配对（凭据判定现在与 Venue 无关）。
- `qx-cli` 侧的角色白名单并入同一张表：`worker_entry.rs` 的 `const VENUE_ROLES` 删除，
  未知角色提示文案改由 `ALL_WORKER_ROLES.iter().filter(|role| role.is_venue_role())` 生成，
  `main.rs` 里手写的五角色 `worker_credentials_ready` 列表改判 `role.is_venue_role()`。
- `tools/check_architecture.py` 新增 `runtime_config_fail_closed_check()` 九项：结构体全部拒绝未知键、
  名单与源码一致（名单不再按结构体名字后缀猜测，而是扫"行首 `pub struct` + 派生 `Deserialize`"这一事实，
  并与 `LOOSE_DESERIALIZE_STRUCTS` 这 8 个刻意保留宽松反序列化的契约/快照结构体做差集比对——
  新写一个可反序列化的结构体却不进这两张表之一即报红）、注释键剥离只有一处且被 `from_json`
  使用、策略表只有一处声明、不得用通配臂、覆盖全部 `WorkerRole` 变体、凭据判定与 Venue 无关
  （`worker_policy.rs` 出现 `is_binance` 即红）、字段可见性判定先于 `enabled` 短路、
  角色白名单不得在策略表之外另抄一份。属性级判定统一走新的 `struct_attribute_blocks()`，
  因此属性块里再插别的属性或注释都不会误判为"缺少 `deny_unknown_fields`"。
- `crates/qx-runtime` 的角色策略用例（8 例）写在 `crates/qx-runtime/tests/worker_policy.rs`，
  只用公开 API；`deploy/qianxing.runtime.production.example.json` 的 api worker 删掉一条无人读取的
  `symbols`，而不是放宽策略表。

### Validation（phase4m 轮实测，日志 `/tmp/qx_phase4m_gate.log`，2026-09-19：运行时配置面 fail-closed 落地后复跑）

```text
QX_PYTHON=C:\Users\Administrator\AppData\Local\Temp\qxvenv\Scripts\python.exe
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=70; RUST_PASSED=536 RUST_FAILED_SUITES=0        # Phase 4l 的 526 + 本轮新增 10 例
qx-core: tests/ledger.rs 18 passed / lib 34 passed；venue_report_contract 1 passed
qx-runtime（--no-fail-fast）: RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
  含 tests/worker_policy.rs 8 例 + lib 两条配置面用例
cargo test -p qx-cli --bin qx-cli: 56 passed；--features sqlite,postgres,nats: 59 passed
18 份运行时模板逐项 config validate: TEMPLATES_VALIDATED=17/18
未知键端到端: TYPO_EXIT=2（错误文本含 max_order_notional_raws）
注释键: COMMENT_KEYS_INJECTED=2 → COMMENT_ACCEPT_EXIT=0
        FINGERPRINT_STABLE=same（4d407ff36051fc81b1702bc0ef3cdd88df7d4bdc3af1ad801d4d81b6efe938c8）
非法角色字段: ROLE_FORBIDDEN_EXIT=2 / 禁用 worker: DISABLED_BYPASS_EXIT=2
  两者文案均为「credential_env 不能配置在 Api 角色；该角色的运行路径不会读取它」
ARCH_EXIT=0  架构不变量自检全部通过 ✓（50 项，Phase 4l 的 41 项 + 本轮 9 项）
PY_EXIT=0  Ran 43 tests in 0.067s  OK     VALIDATE_EXIT=0  全部自校验通过 ✓
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 runtime-check-binance=0 paper-e2e=0
                 config-validate=0 backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0
                 未知命令=2 binance-worker 非法角色=2
                 runtime-check production=2（模板引用部署机绝对路径，本机必然退 2，记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 Phase 4j/4k/4l 基线逐位相同
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（八次变异实验后工作树逐字节校验、无 .bak4m 残留）
UNCOMMITTED_PATHS=133
```

本轮唯一的行数增长项是 `crates/qx-runtime/src/lib.rs` 3,566 → 3,660（`deny_unknown_fields` × 12 +
`validate()` 改判 + 两条配置面用例），删除侧同量级：`crates/qx-cli/src/main.rs` 3,601 → 3,594、
`crates/qx-cli/src/worker_entry.rs` 594 → 583（两处手抄角色白名单并入策略表）；
新文件 `worker_policy.rs` 322 行与 `tests/worker_policy.rs` 222 行均低于 `OVERSIZED = 500`，无需登记。
`BUDGET_DIFF_EXIT=1` 只包含上述三项，本轮触及的 3 个登记项之和 7,761 → 7,837、41 条求和 59,785 → 59,861，
即棘轮落地以来**第一次净增 +76 行**，已逐条审阅后重新快照（并把上一轮遗留的偏高条目 3,662 收紧到实测的 3,660），
理由与边界写进 V9 §8.3 第 5 项。

反向验证八次（同一段日志，每次 `cp` 备份 → 注错 → 复跑 → `cp` 还原 → `cmp` 必须 `identical` → 复跑必须 `RESTORED_x_ARCH_EXIT=0`）：
Q 摘掉 `WorkerConfig` 的 `deny_unknown_fields` → "结构体全部拒绝未知键"报
`缺少 deny_unknown_fields: ['WorkerConfig']`，且 `runtime_config_rejects_unknown_keys_and_strips_comment_keys` 转红；
R 把策略表改回 `_ =>` 通配臂 → "逐角色声明"与"覆盖全部 `WorkerRole` 变体"同时报红，并列出
枚举 10 个变体 / 策略表 6 个的差集；
S 把凭据来源判定收回 `is_binance` 分支 → "凭据判定与 Venue 无关"报红，
`binance_private_workers_require_one_valid_credential_source` 与配置面用例双双转红；
T 把 `enabled` 短路挪到字段策略之前 → "字段可见性判定先于 `enabled` 短路"报红，
禁用 worker 的绕过用例复跑 `44 passed; 1 failed`；
U 在 `worker_entry.rs` 另抄一份角色白名单 → "角色白名单不得在策略表之外另抄一份"报
`重复出现于 ['crates/qx-cli/src/worker_entry.rs']`；
V 让 `from_json` 不再剥离注释键 → "注释键剥离只有一处实现且被 `from_json` 使用"报红。
W 在 `lib.rs` 插入一个未登记的 `pub struct RuntimeLimits`（派生 `Deserialize`、不带 `deny_unknown_fields`）
→ `MUTATED_W_ARCH_EXIT=1`，"运行时配置结构体名单与登记表一致"把三张集合全印出来
（名单 12 / 宽松 8 / 源码可反序列化 21，多出的正是 `RuntimeLimits`）——证明名单不再靠名字后缀猜测；
X 把 `TlsPaths` 的 `#[serde(deny_unknown_fields)]` 从"紧邻声明行"挪到 `#[derive(…)]` 之上（等行数改写）
→ `MUTATED_X_ARCH_EXIT=0`，两条属性级判定仍 `[PASS]`，说明判定读的是整个属性块而不是两行相邻的形状
（旧的紧邻正则会在这里误报缺失，反而逼后来人把属性顺序写死）。

环境事实（本轮新增三条，均已固化进 `qx_phase4m_gate.sh`）：
(1) `config validate` 按**配置文件所在目录**解析模板内的相对引用，把模板 `cp` 到别处再校验必然退 2，
所以注释键这条断言的证据改用 `config fingerprint`（指纹仅在 `RuntimeConfig::from_json` 成功后计算）；
(2) `python -c` 源码里的 `/tmp/...` 路径不会被 MSYS 转换而 argv 会，脚本改用 `SW=$(cygpath -w /tmp/qx4m_scratch)`，
避免"退出码看着正常、实际读的是另一个文件"；
(3) 期望"仍绿"的变异实验必须等行数改写：第一版 X 在两行属性之间插了空行与注释，`lib.rs` 由 3,660 变 3,662，
于是 `MUTATED_X_ARCH_EXIT=1` 红在**行数棘轮**那条而不是被检的属性判定上——结论正确、证据错项，
改成 `#[serde(deny_unknown_fields)]` 与 `#[derive(…)]` 两行互换后才是干净的"属性块形状无关"反证。
此外 `18 份模板` 中必然失败的
`qianxing.runtime.production.example.json` 只败在 `[FAIL] strategy.research_snapshot_path` /
`dataset_bundle_path` 两条 `/var/lib/qianxing/research/*` 部署机绝对路径，属结构校验之外的记录性条目。

### Changed（Phase 4n：跨语言 worker 启动/无响应失败必须自证原因，架构不变量 50 → 54 项）

- `crates/qx-cli/src/main.rs`：`python_interpreter()` 的 `unwrap_or_else(|_| "python".into())`
  静默回落改为 `python_interpreter_origin()`，返回「解释器路径 + 来源」二元组（来源只有两种文案：
  `来自 QX_PYTHON` 与 `QX_PYTHON 未设置，回落 PATH python`）。`QX_PYTHON` 的读取点仍只有
  `python_interpreter()` 一处，所以原有一条门禁继续有效。
- `crates/qx-cli/src/strategy_host.rs`：`WorkerProcess` 新增 `diagnostic_program` 字段（Python 路径把来源
  一起编进去），并新增单一诊断出口 `death_note()` —— 一次给出「程序名（含来源）+ 子进程状态
  （`try_wait()` 的退出码 / 信号终止 / 仍在运行 / 退出码不可读）+ stderr 尾部」，缺 stderr 时直接提示
  "若该程序是 WindowsApps 的 python 占位桩，请把 QX_PYTHON 指向可用解释器"。它接满四个原本各说各话的
  失败出口：共享 ring 超时、管道响应超时、响应通道断开、读线程送上来的"worker 已关闭输出"协议错误
  （最后一条此前被裸解包冒泡，把已抓到的 stderr 与退出码整个丢掉）；`spawn()` 失败则新增独立文案
  `启动 … worker 失败: <程序> 无法执行: <os 错误>`。诊断在 `child.kill()` **之前**取，否则退出码读不到。
- `crates/qx-adapter/src/ccxt.rs`：同一处"提交结果未知"的 EOF 文案追加死掉的是哪个程序，但**不改**
  `CCXT Worker 已退出，提交结果未知` 这段安全语义（订单是否已提交是资金安全问题，不能被诊断文本稀释）；
  `spawn()` 失败同样点名解释器。
- `strategy_contract.rs:56` 与 `workers.rs:306` 两处 `external_executable`（"外部 Strategy"）调用点显式传
  `None`：诊断只写程序名，不会给一个 CPython 之外的进程编上"QX_PYTHON 来源"。
- 新增黑盒用例 `crates/qx-cli/tests/worker_launch_diagnostics.rs`（128 行，3 例）：(1) 不存在的解释器
  → 退出码 2 且失败信息含该程序名与"无法执行"；(2) "存在但对协议完全沉默"的程序 → 必须同时给出
  `程序=`、来源、子进程状态与 `stderr=`；(3) 不注入任何解释器变量 → 回落路径必须在失败信息里说明
  "QX_PYTHON 未设置，回落 PATH python"。用例把运行时模板的 `data_dir` 重写到自己 `%TEMP%` 的副本里，
  不再往 `deploy/data/*/runs/` 写脏产物。
- `tools/check_architecture.py` 新增 `worker_diagnostics_check()` 四项：解释器来源判定只在
  `python_interpreter_origin()` 一处；worker 诊断只有一处实现且**接满每个出口**（`death_note` 定义 1 次、
  调用 ≥ 4 次，且必须用 `try_wait()` 取退出码）；`recv_timeout` 之后到解出响应之前那一段必须附诊断、
  不得用两个问号裸传；CCXT 的 EOF 文案必须同时保留"提交结果未知"与程序名。

### Validation（phase4n 轮实测，日志 `/tmp/qx_phase4n_gate.log`，2026-09-19：worker 失败自证原因落地后复跑）

```text
QX_PYTHON=C:\Users\Administrator\AppData\Local\Temp\qxvenv\Scripts\python.exe
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0 (CLIPPY_WARNING_LINES=0)  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=71; RUST_PASSED=539 RUST_FAILED_SUITES=0        # Phase 4m 的 536 + 本轮新增 3 例
WORKER_DIAG_PASSED=3；单独复跑（摘掉 QX_PYTHON）: 3 passed，DIAG_NO_ENV_EXIT=0
qx-core: tests/ledger.rs 18 passed / lib 34 passed；venue_report_contract 1 passed
qx-runtime（--no-fail-fast）: RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
cargo test -p qx-cli --bin qx-cli: 56 passed；--features sqlite,postgres,nats: 59 passed
QX_PYTHON=python（占位桩）对照: STUB_COMPARE_EXIT=101，54 passed; 2 failed
解释器三口径端到端（同一条 strategy backtest 链）：
  A 回落 PATH python: A_EXIT=2
    策略回测失败: 跨语言策略回测失败: BusinessViolation("Strategy worker 已关闭输出（程序=python
    （QX_PYTHON 未设置，回落 PATH python），进程未退出，worker 无 stderr 输出；若该程序是 WindowsApps
    的 python 占位桩，请把 QX_PYTHON 指向可用解释器）")
  B 不存在的解释器: B_EXIT=2
    策略回测失败: 启动 Python Strategy worker 失败: qx-4n-no-such-interpreter（来自 QX_PYTHON） 无法执行: program not found
  C 可用解释器: C_OK_EXIT=0  result_hash=06ec367ae53ab542
18 份含 workers 段的模板逐项 config validate: TEMPLATES_VALIDATED=17/18
  唯一失败仍是 production 模板的两条 /var/lib/qianxing/research/* 部署机绝对路径（记录性条目）
快照前 ARCH_EXIT=1 / ARCH_ITEMS=53，唯一红项是行数棘轮；--snapshot 后
  ARCH_EXIT_AFTER_SNAPSHOT=0 / ARCH_ITEMS_AFTER_SNAPSHOT=54
PY_EXIT=0  Ran 43 tests in 0.088s  OK     VALIDATE_EXIT=0  全部自校验通过 ✓
CLI 冒烟：verify=0 all=0 runtime-check=0 runtime-check-binance=0 config-validate=0
          backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0 paper-e2e=0
          strategy backtest=0（本轮新增口径） 未知命令=2 binance-worker 非法角色=2
          runtime-check production=2（绝对路径，本机必然）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 Phase 4j/4k/4l/4m 基线逐位相同
离线验收：NO_CREDS_EXIT=3 / --allow-skip ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（三次变异实验后工作树逐字节校验，无 .bak4n 残留）
UNCOMMITTED_PATHS=140
```

反向验证三次（`cp` 备份 → 锚点唯一替换 → 复跑 → `cp` 还原 → `cmp` `identical` → `RESTORED_x_ARCH_EXIT=0`）：
Y 把"读线程协议错误"的诊断包装退回裸 `return Err(error)` → `MUTATED_Y_ARCH_EXIT=1`（"接满每个出口"印出
`定义 1 次、调用 3 次`），且黑盒用例 `MUTATED_Y_TEST_EXIT=101`、`1 passed; 2 failed`，失败文本正是
"失败信息必须说明是哪个解释器" —— 门禁项与行为测试钉住的是同一件事；
W 把"响应通道断开"出口的诊断退回旧的只带 stderr 尾部 → 调用点变 3，`MUTATED_W_ARCH_EXIT=1`；
Z 把 CCXT 的 EOF 文案改成不含程序名 → `MUTATED_Z_ARCH_EXIT=1`，报
`CCXT 无响应既保留未知结果语义又点名解释器 — EOF 文案缺少程序名或被改写`。

本轮行数增长 5 个登记项：`strategy_host.rs` 772 → 812（`death_note()` 与其四个接入点）、
`main.rs` 3,594 → 3,604（来源二元组）、`ccxt.rs` 1,064 → 1,068、`strategy_contract.rs` 821 → 822、
`workers.rs` 717 → 718（各加一个实参）。本轮触及的 5 个登记项之和 6,968 → 7,024、41 条求和
59,861 → 59,917，即**连续第二次净增（+56 行）**，没有新增登记项（新测试文件 128 行在棘轮集之外）。
边界与下一步的回收计划记在 V9 §8.3 第 5、6 项：这一类"失败路径自证"的文本无法压缩到零，但
`main.rs` 与 `strategy_host.rs` 的下一个拆分点已经明确。

环境事实（本轮新增三条，均已固化进 `qx_phase4n_gate.sh`）：
(1) **本机 Git Bash 的 `env -u QX_PYTHON <cmd>` 会静默不执行命令**（实测退出码 0、零字节输出、38ms），
第一版门禁因此把"回落口径"记成了 `A_EXIT=0 / WORKER_DIAG_LINES=0` 的假绿；脚本改用子壳
`noenv() { ( unset QX_PYTHON; "$@" ); }` 后同一命令真实地报出 `A_EXIT=2` 与完整诊断文本；
(2) 需要"存在但对协议沉默、且跨平台一致"的假解释器时，用 `std::env::current_exe()`（测试二进制自身）：
它收到未知参数即退 101 且 stdout 为空、stderr 固定一行。`cmd.exe` 会往 stdout 吐 129 字节横幅、
`/bin/true` 只在类 Unix 存在、qx-cli 自身会把帮助写到 stdout，都不合格；
(3) `cargo test` 在用例失败时退 **101**（不是 1），"占位桩对照"这类"必然红"的步骤要把期望写成非 0；
`python -m unittest discover` 的正确调用是 `-s python/tests -q`，写成 `-s python -p "test_*.py"` 会得到
`NO TESTS RAN` + `PY_EXIT=5`，看着像"测试通过计数为 0"。

### Changed（Phase 4o：删除侧收敛 —— qx-cli 行为用例拆成 `src/tests/` 目录模块，架构不变量 54 → 58 项）

- `crates/qx-cli/src/tests_main.rs`（2,860 行，登记列表里第二大项）按主题拆成
  `crates/qx-cli/src/tests/` 目录模块：`cli_surface`（6 例）/ `paper_bridge_and_bundles`（7）/
  `worker_observability`（18）/ `execution_and_multi_leg`（4）/ `paper_and_strategy_worker`（7）/
  `backtest_entries`（9）/ `e2e_and_python_contract`（8），另有 103 行的 `mod.rs` 收 4 个共享夹具
  （`smoke_paper_risk_context`、`builtin_backtest_example_paths`、`temp_cli_case_dir`、
  `isolated_backtest_runtime`）。最大子文件 459 行，8 个文件合计 2,878 行（多出的 18 行是各主题文件的
  `use super::*;` 头与 `mod.rs` 的模块声明）。
- **只搬行、不改语义**：59 条 `#[test]` 按原顺序整段移动，用例体一字未改；`main.rs` 尾部的
  `#[cfg(test)]` + `#[path = "tests_main.rs"]` + `mod tests;` 三行变两行（3,604 → 3,603）。
  拆分动机不是观感：Phase 4m/4n 连续两轮净增之后，V9 §8.3 第 5 项要求下一轮必须是删除侧收敛，而登记列表里
  唯一能整体消失的大项就是这份测试单文件——棘轮只数 `crates/*/src/**/*.rs`，目录模块让每个文件落回 500 行门槛内。
- 新增 4 项架构不变量（54 → 58）钉住这次拆分自身：被拆掉的单文件不得复活；用例必须以目录模块挂载
  （`main.rs` 不得再出现 `#[path = "tests_main.rs"]`）；**行为用例条数下限棘轮 ≥ 59**（拆分让"删几条用例来压
  行数"成为可行路径，于是行数只降不升的同时用例数只能升）；4 个共享夹具只在 `tests/mod.rs` 定义一份。
- 两处既有门禁的豁免随形状调整而未削弱：命令分派单点检查原先按文件名 `tests_main.rs` 豁免，现按 `src/tests/`
  目录豁免；多腿屏障检查原先靠"文件名 stem 含 `test`"豁免用例文件，现同样按目录豁免，其可靠性由新增的第二项
  不变量兜住——该目录只能经 `#[cfg(test)] mod tests;` 挂载，生产提交入口不可能躲进去。

### Validation（phase4o 轮实测，日志 `/tmp/qx_phase4o_gate.log`，2026-09-19，本轮不做功能改动，只验收"搬家不改语义"与新增的四项形状门禁）

```text
FMT_EXIT=0
CLIPPY_DEFAULT_EXIT=0             # cargo clippy --workspace --all-targets
CLIPPY_WARNING_LINES=0
CLIPPY_FEAT_EXIT=0                # cargo clippy -p qx-cli --all-targets --features sqlite,postgres,nats
TEST_EXIT=0
OK_LINES=71  RUST_PASSED=539  RUST_FAILED_SUITES=0
                                  # 与 Phase 4n 逐位相同：本轮只搬行，用例不增不减
CORE_EXIT=0（tests/ledger.rs 18 passed）/ CORE_LIB_EXIT=0（lib 34 passed）
CONTRACT_EXIT=0（venue_report_contract 1 passed）
RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
56 passed / 59 passed             # cargo test -p qx-cli --bin qx-cli 默认特性 / sqlite,postgres,nats
TEST_TOTAL=59                     # 6+7+18+4+7+9+8，等于拆分前 tests_main.rs 的 59 条
最大用例文件 459 行（paper_and_strategy_worker.rs）、mod.rs 103 行、8 个文件合计 2,878 行
ARCH_EXIT_BEFORE_SNAPSHOT=1  ARCH_ITEMS_BEFORE=58
  [FAIL] 单文件行数预算只降不升 — crates/qx-cli/src/tests_main.rs 已不存在，请重新生成快照
已写入 maturity/line_budgets.yaml（40 个超 500 行文件）
ARCH_EXIT_AFTER_SNAPSHOT=0  ARCH_ITEMS_AFTER_SNAPSHOT=58
entries 41 → 40 / sum 59,917 → 57,056（−2,861）
dropped=['crates/qx-cli/src/tests_main.rs']  added=[]  shrunk={main.rs: 3604 → 3603}
MUTATED_AA/BB/CC/DD_ARCH_EXIT=1 → RESTORE=identical → RESTORED_*_ARCH_EXIT=0
PY_EXIT=0（Ran 43 tests）/ VALIDATE_EXIT=0 / TEMPLATES_VALIDATED=17/18
cli smoke：verify / all / runtime-check / runtime-check-binance / config-validate /
           backtest-builtin / backtest-multi-builtin / fast-backtest-ashare / paper-e2e /
           strategy-backtest 全 0；runtime-check-production=2、unknown-command=2、
           binance-worker-bad-role=2（三条均为记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 4j/4k/4l/4m/4n 基线逐位相同
STUB_EXIT=2 + 自证文案仍在（QX_PYTHON 未设置时回落 WindowsApps 占位桩，Phase 4n 的诊断未因搬家退化）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（四次变异实验后工作树逐字节校验、无 tests_main.rs 残留）
UNCOMMITTED_PATHS=139
```

反向验证四次，各自只红在本轮新增的那一项：AA 在 `crates/qx-cli/src/` 建一个 1 行的 `tests_main.rs` →
"用例单文件已拆分且不得复活"报红；BB 把 `reconcile_report_persists_structured_balance_discrepancy` 的
`#[test]` 注释掉 → "qx-cli 行为用例不少于 59 条 — 当前 58 条"报红并把逐文件条数印出来，证明下限棘轮确实拦得住
"删用例换行数"这条被本轮拆分新打开的路径；CC 在 `backtest_entries.rs` 另抄一份 `fn temp_cli_case_dir` →
"共享测试夹具只在 tests/mod.rs 定义一份"报红（定义位置 `['backtest_entries.rs', 'mod.rs']`）；
DD 把 `#[cfg(test)]` 原地改成 `#[path = "tests_main.rs"]`（等行数改写，避免红因落到行数棘轮上）→
"用例以目录模块挂载"报红。四次实验后 `cp` 还原并 `cmp` 逐字节相同、复跑 `check_architecture.py` 全绿。

行数账：本轮是棘轮落地以来第一次**净降** —— 登记条数 41 → 40，41 条求和 59,917 → 57,056（−2,861 行），
唯一一条被删的登记项就是 `tests_main.rs`（2,860 行），另有 `main.rs` −1 行；新增的 8 个文件全部在
`OVERSIZED = 500` 门槛之下，按规则不需登记，因此没有新增登记项。上一轮记下的"连续两次净增需先做一次
删除侧收敛"的约束到此兑现，第三次增长的判断重新回到"是否有其它同量级回收点"，现存最大项依次是
`main.rs` 3,603、`ashare.rs` 1,978、`backtests.rs` 1,379、`crates/qx-execution/src/tests.rs` 1,088。

### Changed（Phase 4p：qx-cli crate 根职责簇拆分 —— `main.rs` 3,603 → 1,884 行，架构不变量 58 → 65 项）

- 将 Phase 4o 之后登记列表里的最大项、也是唯一一项仍可"纯搬家"回收的条目 —— `crates/qx-cli/src/main.rs`
  （3,603 行 / 此前把整仓 CLI 命令分派、冒烟自检、运行时装配都写在一起）—— 按职责簇拆出 5 个兄弟模块：
  `ecosystem_smoke.rs`（481 行，`run_ecosystem_smoke` / `run_paper_smoke` / `run_backtest` 三条冒烟链与
  `DemoProvider`、`Outcome`、`gen_bars`、`mk_order` 等自检夹具）、`runtime_wiring.rs`（344 行，
  `read_runtime_config`、`PipelineStorage`、`open_runtime_pipeline`、`strategy_risk_gate`、
  `config_margin_mode`、`CONSERVATIVE_MAX_QTY_RAW` 与 `worker_metrics_*` 七个观测口辅助）、
  `configured_backends.rs`（305 行，`ControlStateBackend` / `ConfiguredJobQueue` 两个后端口与其
  `configured_*` 构造器、`postgres_dsn`）、`api_service.rs`（386 行，`build_configured_api_service` 与
  账户快照 / 查询模型四个加载器）、`readiness.rs`（267 行，`configured_api_readiness`、
  `production_trading_assets_ready`、`worker_credentials_ready`、`validate_research_snapshot_binding` 等
  就绪判定）。5 个文件全部落在 `OVERSIZED = 500` 门槛内，按棘轮规则不需登记，因此登记条数不变而最大项腰斩。
- **只搬行、不改语义**：由脚本按锚点整段切移，`use super::*;` + `pub(crate)` 提升是全部形态变化。等价性证据
  是符号 token 多重集 25,157 → 25,157（lost=0 / gained=0）；行级多重集的 9 减 33 增逐条核对为 rustfmt 把
  9 个超长签名改成多行（每处换行即多出一行），不是代码增删。用例数与结果口径逐项与 Phase 4o 相同
  （workspace 539、`--bin qx-cli` 默认 56 / 全特性 59、`src/tests/` 内 59 条），`result_hash` 逐位未变。
- 新增 7 项架构不变量（58 → 65）钉住这次拆分自身，而不只是记录它：5 个拆出模块逐个在 500 行门槛内；
  每个模块必须以 `mod x;` + `pub(crate) use x::*;` 成对挂载在 crate 根；**根内顶层条目数上限 41**
  （`CLI_ROOT_ITEM_CEILING`，拦"实现又长回 main.rs"这条由本轮新打开的出口 —— 行数棘轮只约束登记项，
  把函数挪回根目录不再违反任何既有门禁）；`run_ecosystem_smoke` / `run_paper_smoke` / `run_backtest` /
  `DemoProvider` 四个冒烟链路入口的定义点必须唯一且落在 `ecosystem_smoke.rs`。
- 一处既有门禁的豁免键随代码搬家：`BARE_RISK_GATE_ALLOWLIST`（"生产代码不存在无规则的风控门"）原先记
  `main.rs` 里的 1 处裸 `RiskGate::new`（`run_backtest` 的冒烟装配），现随函数移到
  `ecosystem_smoke.rs` 且计数仍为 1。这是按文件名索引的白名单的固有耦合，边界记入 §8.3 第 6 项。

### Validation（phase4p 轮实测，日志 `/tmp/qx_phase4p_gate.log`，2026-09-19，本轮不做功能改动，只验收"搬家不改语义"与新增的七项形状门禁）

```text
FMT_EXIT=0
CLIPPY_DEFAULT_EXIT=0             # cargo clippy --workspace --all-targets
CLIPPY_WARNING_LINES=0
CLIPPY_FEAT_EXIT=0                # cargo clippy -p qx-cli --all-targets --features sqlite,postgres,nats
TEST_EXIT=0
OK_LINES=71  RUST_PASSED=539  RUST_FAILED_SUITES=0
                                  # 与 Phase 4o/4n 逐位相同：本轮只搬行，用例不增不减
CORE_EXIT=0（tests/ledger.rs 18 passed）/ CORE_LIB_EXIT=0（lib 34 passed）
CONTRACT_EXIT=0（venue_report_contract 1 passed）
RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
56 passed / 59 passed             # cargo test -p qx-cli --bin qx-cli 默认特性 / sqlite,postgres,nats
SHAPE: main.rs 1,884 + ecosystem_smoke 481 + runtime_wiring 344 + configured_backends 305
       + api_service 386 + readiness 267 = 3,667 行（合计比拆前 3,603 多 64 行，即 5 份模块头
       + `use super::*;` 与根里 10 行 `mod`/`pub(crate) use` 挂载的固定代价）
ROOT_ITEMS=41  TEST_TOTAL=59
SIG_LINES before=3458 after=3482
LINE_MULTISET lost=9 gained=33    # 逐条核对为 9 个签名的 rustfmt 重排，非代码增删
TOKENS before=25157 after=25157
TOKEN_MULTISET lost=0 gained=0    # 真正的"只搬家"判据
ARCH_EXIT_BEFORE_SNAPSHOT=0       # 与 Phase 4o 不同：本轮登记项只降不升，快照前不该有红，全绿是预期
已写入 maturity/line_budgets.yaml（40 个超 500 行文件）
ARCH_EXIT_AFTER_SNAPSHOT=0  ARCH_ITEMS_AFTER_SNAPSHOT=65
entries 40 → 40 / sum 57,056 → 55,337（−1,719）
dropped=[]  added=[]  changed={'crates/qx-cli/src/main.rs': (3603, 1884)}
MUTATED_EE/FF/GG/HH_ARCH_EXIT=1 → RESTORE=identical → RESTORED_*_ARCH_EXIT=0
PY_EXIT=0（Ran 43 tests）/ VALIDATE_EXIT=0 / TEMPLATES_VALIDATED=17/18
cli smoke：verify / all / runtime-check / runtime-check-binance / config-validate /
           backtest-builtin / backtest-multi-builtin / fast-backtest-ashare / paper-e2e /
           strategy-backtest 全 0；runtime-check-production=2、unknown-command=2、
           binance-worker-bad-role=2（三条均为记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 4j/4k/4l/4m/4n/4o 基线逐位相同
STUB_EXIT=2 + 自证文案仍在（QX_PYTHON 未设置时回落 WindowsApps 占位桩，Phase 4n 的诊断未因搬家退化）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（四次变异实验后工作树逐字节校验）
UNCOMMITTED_PATHS=144
```

反向验证四次，各自只红在本轮新增的那一项：EE 在 `main.rs` 里等行数注入一个新的 `pub(crate) fn`（根条目
41 → 42）→ "crate 根顶层条目不多于 41 个（实现不得长回 main.rs）"报红，证明这条上限确实拦得住拆分带来的
新出口；FF 把 `api_service.rs` 撑到恰好 500 行（越出 `< OVERSIZED` 判定、但仍不需登记，故红因不可能落到
行数棘轮上）→ "Phase 4p 拆出的兄弟模块逐个在单文件行数门槛内"报红；GG 在 `main.rs` 里等行数改写既有签名、
顺手再定义一份 `run_ecosystem_smoke` → "smoke 链路入口 … 定义点唯一且在 ecosystem_smoke.rs — 定义于
['ecosystem_smoke.rs', 'main.rs']"报红；HH 删掉 `readiness` 的 `pub(crate) use readiness::*;` 一行 →
"拆出的模块在 crate 根以 mod + pub(crate) use x::* 成对挂载 — 缺配对 ['readiness']"报红。四次实验后
`cp` 还原并 `cmp` 逐字节相同、复跑 `check_architecture.py` 全绿。

行数账：登记条数 40 → 40（不增项），40 条求和 57,056 → 55,337（−1,719 行），唯一变化项是
`crates/qx-cli/src/main.rs` 3,603 → 1,884。这是棘轮落地以来第一次连续两轮净降（4o −2,861、4p −1,719），
`main.rs` 也从登记列表第 1 大项掉到第 11 位（首位是 `crates/qx-runtime/src/lib.rs` 3,660，第 10 位
`ashare.rs` 1,978 紧接在 `main.rs` 之前）。代价是真实代码总量没减：六个文件合计 3,667 行，比拆前的
3,603 行多 64 行，纯粹是模块头、`use super::*;` 与根里成对挂载行的固定开销，因此本轮收敛的是"单文件
长度"这一个可维护性口径，不是删掉了任何功能。剩余登记项 `main.rs` 1,884 / `ashare.rs` 1,978 /
`backtests.rs` 1,379 / `crates/qx-execution/src/tests.rs` 1,088 此后都要按真实职责重写才能再拆，
同量级的"纯搬家"回收点已尽。

### Fixed（Phase 4t：qx-storage 追加锁在 Windows 上的偶发拒绝访问 —— 有界重试覆盖 PermissionDenied）

- 根因：`acquire_storage_lock`（crates/qx-storage/src/lib.rs）用 `create_new(true)` 抢锁，只对 `AlreadyExists` 重试；`StorageLock::drop` 删除锁文件后存在短暂的 delete-pending 窗口，此窗口内的 `create_new` 在 Windows 上返回 `ERROR_ACCESS_DENIED`（os error 5），不落入重试分支就直接抛 `StorageError::Io`。16 线程 × 200 次并发取令牌的压力探针改动前为 ok 3142 / os_error_5 8 / other_io 0 / conflict 50，把 `PermissionDenied` 并入同一有界重试后为 ok 3163 / os_error_5 0 / other_io 0 / conflict 37（探针日志由 cargo test -- --nocapture 当场捕获）。
- 排除过的错误假设：一度以为是 `write_atomic_path` 里 `std::fs::rename` 撞上目标文件被占用的共享冲突。为此新写的回归用例直接把它证伪 —— 同进程持有目标文件句柄时 rename 照样成功（`unwrap_err()` 拿到 `Ok(true)`）。那套 `src/atomic.rs` 重试与配套用例已整体撤销，未留下半套改动。
- 代价如实记录：修复让 `crates/qx-storage/src/lib.rs` 从登记值 3,536 增长到 3,541（rustfmt 把多行 `matches!` 与中文注释按 100 列重新展开）。行数棘轮本轮被有意放宽 5 行并由 `--snapshot` 记为 3,541 —— 这是首次为经过验证的正确性修复上调预算，与「纯搬家类回收点已尽」是两件事。
- 本轮实测（日志 /tmp/qx_phase4t_gate.log）：专项 `cargo fmt --all` 通过、`cargo clippy -p qx-storage --all-targets` 零告警、`cargo test -p qx-storage --no-fail-fast` 6 个套件全绿 0 失败；随后补跑全量门禁，`FMT=0`、`CLIPPY=0 / CLIPPY_WARNING_LINES=0`、`TEST_EXIT=0`、`OK_SUITES=71 / RUST_PASSED=539 / RUST_FAILED_SUITES=0`（与 Phase 4s 复跑基线一致，未因本轮改动新增或丢失用例）、架构不变量 88 项全绿。151 个路径仍全部未提交。

### Changed（Phase 4s：qx-cli 回测编排目录模块拆分 —— `src/backtests.rs` 1,379 行整体退出登记集，架构不变量 76 → 88 项）

- 把登记列表里最后一项"仍能靠纯搬家收掉"的目标搬走：`crates/qx-cli/src/backtests.rs`（1,379 行）按回测
  入口拆成 `src/backtests/` 七个文件 —— `mod.rs` 172 行只留共享装配（`BarBacktestAssembly`、
  `market_spec_with_margin`、`run_builtin_strategy_on_bars`，以及多腿共用的 `ScheduledTargetStrategy` 与
  `read_bar_frame_for_multi_backtest`）、`multi_builtin.rs` 347、`single_strategy.rs` 296、
  `strategy_backtest.rs` 234、`depth.rs` 180、`artifacts.rs` 109、`fast_backtest.rs` 77。原单文件删除，
  `main.rs` 的 `mod backtests;` 不改一字即指向目录模块。
- 拆完暴露两处真实耦合，都按最小面修掉而非绕开：`BacktestArtifacts` 的 21 个字段与 `DatasetRunBinding`
  的 2 个字段从私有提到 `pub(crate)`（兄弟模块要用结构体字面量构造，否则 E0451）；第 6 项"Bar 回测引擎
  装配只有一份"原先读死单文件路径，现改为**按目录聚合**读七个子文件 —— 不改这一条，在拆出去的子文件里
  再写一份 `BacktestConfig {` 字面量就是门禁盲区（本轮反向验证 SS 正是用它证明新口径咬得住）。
- 架构不变量新增第 14 项，把 Phase 4o / 4r 只用在行为用例上的那套形状约束搬到**生产代码**：单文件不得
  复活、六个主题模块逐个低于门槛、在 `mod.rs` 以 `mod x;` + `pub(crate) use x::*;` 成对挂载、共享装配的
  顶层条目数不增（当前 7 个，上限 8）、八条回测链入口的定义点唯一。`maturity/capabilities.yaml` 里三条
  指向 `backtests.rs` 的证据路径同步改成真实子文件（这项由第 7 项不变量自动报红，不靠人记住）。

### Validation（phase4s 轮实测，日志 `/tmp/qx_phase4s_gate.log`，2026-09-19：回测编排拆目录模块后复跑）

```
FMT_EXIT=0
CLIPPY_DEFAULT_EXIT=0 / CLIPPY_WARNING_LINES=0 / CLIPPY_FEAT_EXIT=0
TEST_EXIT=101  OK_LINES=70  RUST_PASSED=517  RUST_FAILED_SUITES=1        # 首轮：见下方"同轮复跑"
XINGBAN_EXIT=0 test result: ok. 58 / 3 / 1 / 0
BIN_DEFAULT: test result: ok. 56 passed  /  BIN_FEATURES: test result: ok. 59 passed
SHAPE: mod.rs 172 artifacts.rs 109 depth.rs 180 fast_backtest.rs 77
       multi_builtin.rs 347 single_strategy.rs 296 strategy_backtest.rs 234  total 1415
TOP_ITEMS mod.rs=7（其余 1–3）  DIR_TOTAL=1415
tokens 10693 -> 10693   lost=0 gained=0   EQUIV
ARCH_EXIT_BEFORE_SNAPSHOT=1  PRE_PASS_LINES: 87  唯一红因：backtests.rs 已不存在，请重新生成快照
已写入 maturity/line_budgets.yaml（37 个超 500 行文件）
ARCH_EXIT_AFTER_SNAPSHOT=0  ARCH_ITEMS_AFTER_SNAPSHOT=88  架构不变量自检全部通过 ✓（88 项）
MUTATED_NN/OO/PP/QQ/RR/SS_ARCH_EXIT=1（各命中对应 [FAIL]）RESTORE[*]=identical RESTORED_*_ARCH_EXIT=0
7d6 < crates/qx-cli/src/backtests.rs: 1379   BUDGET_DIFF_EXIT=1
entries 38 -> 37   sum 52365 -> 50986 (-1379)   dropped: ['crates/qx-cli/src/backtests.rs']
added: []   changed: {}
PY_EXIT=0 Ran 43 tests / VALIDATE_EXIT=0 全部自校验通过 ✓ / TEMPLATES_VALIDATED=17/18
cli smoke：全部 exit=0，仅 runtime-check-production=2、unknown-command=2、binance-worker-bad-role=2
DETERMINISM=same  HASH_VS_BASELINE=same（result_hash=b26e1d4d4d430cb1）
STUB_EXIT=2（QX_PYTHON 未设置时回落桩仍自证占位桩）  NO_CREDS_EXIT=3 / ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes  FMT_EXIT_AFTER_MUTATION=0  CHECK_EXIT_AFTER_MUTATION=0  UNCOMMITTED_PATHS=150
```

**首轮红因不是本轮代码**。首轮 `TEST_EXIT=101` 的那一条是 `qx-storage --lib` 的
`file_token_bucket_is_persistent_and_serializes_concurrent_consumers`，panic 在
`crates/qx-storage/src/lib.rs:3521` 的 `unwrap()` 收到 `Io("拒绝访问。 (os error 5)")` —— Windows
下跨进程令牌桶在全量并发跑时的文件共享冲突。同轮复跑把它隔离出来连跑 8 次全部 `17 passed; 0 failed`，
再整跑一遍 `cargo test --workspace`（导出 `QX_PYTHON`）得 `TEST_EXIT3=0 / OK_LINES3=71 /
RUST_PASSED3=539 / RUST_FAILED_SUITES3=0`，与 Phase 4r 末的 539 条逐位相同。复跑还包含一次**故意**
不导出 `QX_PYTHON` 的对照：`RUST_PASSED=483`、红在 `crates/qx-cli/src/tests/e2e_and_python_contract.rs`
两条跨语言契约用例 —— 这正是 Phase 4n 要的 fail-closed 回落，不是回归。三段结果都已写进同一份日志末尾，
但**这条 Windows 抖动没有修**，属本轮遗留（下条）。

反向验证六次：NN 复活空单文件 → "不得复活"报红；OO 去掉 `pub(crate) use depth::*;` 的出口 →
"缺配对 ['depth']"；PP 在 `fast_backtest.rs` 里另写一份 `fn run_depth_backtest(` → 入口唯一性报红并
打印出 `['depth.rs', 'fast_backtest.rs']` 两处；QQ 把 `fast_backtest.rs` 撑到 532 行 → 主题模块门槛报红
（同一变异还额外触发棘轮的"未登记超大文件"，两条门禁重叠）；RR 往 `mod.rs` 写回两条实现 →
条目数 9 > 8 报红；SS 在子模块 `depth.rs` 里第二处装配 `BacktestConfig {` → 第 6 项聚合口径报红。

行数账：登记条数 38 → 37、37 条求和 52,365 → 50,986（−1,379），变化只有 `backtests.rs` 整项退出。
连续五轮净降（4o −2,861、4p −1,719、4q −1,884、4r −1,088、4s −1,379）。当前最大项依次是
`qx-runtime/src/lib.rs` 3,660、`qx-storage/src/lib.rs` 3,536、`qx-api/src/lib.rs` 3,137、
`qx-xingban/src/backtest.rs` 3,024、`qx-storage/src/sqlite.rs` 2,565、`qx-adapter/src/binance.rs` 2,307。
代价与前几轮同性质，且这轮回填得比"收敛"更多：七个文件合计 1,415 行，比原单文件多 **36 行**模块头与
挂载脚手架 —— 收敛的仍只是"单文件长度"这一个可维护性口径，真实代码总量没降。
`ashare.rs` 1,978 是下一个同量级目标，但它与 3,660 行的 `qx-runtime/src/lib.rs` 一样需要按真实职责重写，
不属"纯搬家"这一类；本轮遗留另有一条：上面那个 `os error 5` 抖动需要在令牌桶的文件读写上加有界重试
（属正确性收口，不该混进搬家轮）。

### Changed（Phase 4r：qx-execution 用例目录模块拆分 —— `src/tests.rs` 1,088 行整体退出登记集，架构不变量 72 → 76 项）

- 将登记列表里最后一项"可以纯搬家回收"的条目 —— `crates/qx-execution/src/tests.rs`（1,088 行，
  Phase 4i 删第二入口用例后由 1,242 降来的那一版）—— 按职责拆成 `src/tests/` 目录模块：
  `mod.rs`（223 行，模块头 + `use super::*;` + crate 内 `use` 段 + 六个共享夹具
  `PortState` / `PortVenue` / `NeverCalledVenue` / `RejectingRisk` / `PortRouter` / `port_order`
  与四条 `mod` 声明）、`gateway_port.rs`（103 行 / 5 例，`PortExecutionService` 的注册与标准事实
  追加、空响应与非法事实的 fail-closed、注册前拒绝、`CanonicalRiskPort` 无需 zhenlu 上下文转换）、
  `venue_submit_contract.rs`（200 行 / 2 例，Paper/CCXT/Binance 三家共用的提交-取消端口契约与
  未知提交事实契约）、`recovery_and_replay.rs`（303 行 / 3 例，`HedgeRecoveryWorker` 幂等且对未知
  状态 fail-closed、按 correlation 重放、风控预检先于 EventLog 与 Venue 副作用）、
  `paper_accounting.rs`（268 行 / 3 例，衍生品成交按规格 PnL 记账而非现货现金、缺风控上下文/行情
  时 fail-closed、复用网关幂等不产生新事实）。五文件合计 1,097 行（比拆前多 9 行模块头与挂载声明），
  逐个都在 `OVERSIZED = 500` 门槛内，因此 `src/tests.rs` 整项退出登记列表。`lib.rs` 的
  `#[cfg(test)]\nmod tests;` 挂载未改。与 Phase 4p / 4q 不同，本轮**没有任何一处需要提升可见性**：
  夹具全部留在父模块 `tests/mod.rs`，子模块经 `use super::*;` 看到的是祖先模块的私有项，
  这是 4o 已经验证过的形态。
- **只搬行、不改语义**：符号 token 多重集 6,985 → 6,985（lost=0 / gained=0）；13 条用例逐条以新路径
  运行并通过（`tests::gateway_port::…` 等，`--lib` 目标 `13 passed`）。用例条数逐文件为
  5 / 0 / 2 / 3 / 3，与拆前同一分组。
- 把 Phase 4o 的四项"用例目录模块形状"门禁**参数化**成一张 `TEST_MODULES` 表，对 qx-cli 与
  qx-execution 各查一遍（不变量 72 → 76）：被拆掉的单文件不得复活、不得用 `#[path]` 指回单文件、
  用例条数只增不减（新下限 13）、共享夹具只在 `tests/mod.rs` 定义一份。夹具判定同时把 Phase 4o 的
  `f"fn {name}("` 放宽为按行首的 `fn|struct|enum|trait|type {name}\b`（可带可见性前缀）——
  六条 qx-execution 夹具里有五条是结构体，沿用旧判定会让这五项在无人察觉的情况下恒为绿。
- 一处随代码搬家的**证据链修复**：`maturity/capabilities.yaml` 的 `paper_execution` 与
  `multi_leg_execution` 两条能力项把证据指向 `crates/qx-execution/src/tests.rs`，文件删除后即成为
  失效路径（拆分之前的 `check_architecture.py` 实测红为
  `能力矩阵证据路径全部存在 — 失效路径 ['crates/qx-execution/src/tests.rs']`，记录在
  `/tmp/qx4r_arch_pre1.txt`），改指到承接该断言的三个新文件。该项由"证据路径必须真实存在"这条
  既有门禁自动发现，本轮再用一次性失效路径（`..._typo.rs`）反向验证它仍然拦得住。

### Validation（phase4r 轮实测，日志 `/tmp/qx_phase4r_gate.log`，2026-09-19，本轮不做功能改动，只验收"搬家不改语义"与新增的四项参数化门禁）

```text
FMT_EXIT=0
CLIPPY_DEFAULT_EXIT=0  CLIPPY_WARNING_LINES=0
CLIPPY_FEAT_EXIT=0                       # -p qx-cli --all-targets --features sqlite,postgres,nats
TEST_EXIT=0
OK_LINES=71  RUST_PASSED=539  RUST_FAILED_SUITES=0
                                         # 与 Phase 4n/4o/4p/4q 逐位相同：本轮只搬行，用例不增不减
CORE_EXIT=0（tests/ledger.rs 18 passed）/ CORE_LIB_EXIT=0（lib 34 passed）
EXECUTION_EXIT=0  EXECUTION_TARGET=13 + 3 + 1 + 0（lib / 集成 / 文档 / doctest）
EXEC_LIB=13 passed 且 13 条用例逐条以 tests::<主题>::<用例名> 打印
RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
56 passed / 59 passed                    # cargo test -p qx-cli --bin qx-cli 默认特性 / sqlite,postgres,nats
SHAPE: mod.rs 223 + gateway_port.rs 103 + venue_submit_contract.rs 200
       + recovery_and_replay.rs 303 + paper_accounting.rs 268 = 1,097 行（拆前 1,088）
TESTS_IN 逐文件 5 / 0 / 3 / 3 / 2      TOP_ITEMS mod.rs=12 其余 3–8  EXEC_LIB_ITEMS=38
TOKENS before=6985 after=6985  TOKEN_MULTISET lost=0 gained=0 → EQUIV
ARCH_EXIT_BEFORE_SNAPSHOT=1  PRE_PASS_LINES=75
  [FAIL] 单文件行数预算只降不升 — crates/qx-execution/src/tests.rs 已不存在，请重新生成快照
                                        # 快照前唯一的红因就是预算表仍写着被删文件，符合预期
已写入 maturity/line_budgets.yaml（38 个超 500 行文件）
ARCH_EXIT_AFTER_SNAPSHOT=0  ARCH_ITEMS_AFTER_SNAPSHOT=76  架构不变量自检全部通过 ✓（76 项）
MUTATED_II/JJ/KK/LL/MM_ARCH_EXIT=1 → RESTORE[*]=identical → RESTORED_*_ARCH_EXIT=0
19d18 < crates/qx-execution/src/tests.rs: 1088   BUDGET_DIFF_EXIT=1
entries 39 → 38  sum 53,453 → 52,365（−1,088）  dropped=['…/tests.rs']  added=[]  changed={}
PY_EXIT=0（Ran 43 tests）/ VALIDATE_EXIT=0 / TEMPLATES_VALIDATED=17/18
cli smoke：verify / all / runtime-check / runtime-check-binance / config-validate /
           backtest-builtin / backtest-multi-builtin / fast-backtest-ashare / paper-e2e /
           strategy-backtest 全 0；runtime-check-production=2、unknown-command=2、
           binance-worker-bad-role=2（三条均为记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 4j–4q 基线逐位相同
STUB_EXIT=2 + 自证文案仍在（QX_PYTHON 未设置时回落 WindowsApps 占位桩）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（五次变异实验后逐字节校验，且 tests.rs 未复活）
UNCOMMITTED_PATHS=150
```

反向验证五次，各自只红在本轮新增（或本轮刚重连）的那一项：II 让 `src/tests.rs` 以空文件复活
（0 行不足以触发行数门禁，红因不可能落到棘轮上）→ "qx-execution 用例单文件 tests.rs 已拆分且不得
复活"报红；JJ 把 `lib.rs` 的挂载原地改成 `#[path = "tests.rs"] mod tests;`（等行数）→ "用例以目录
模块挂载，不得用 #[path] 指回单文件"报红；KK 把一条 `#[test]` 原地改成 `#[cfg(any())]`（等行数、
仍可编译，条数 13 → 12）→ "行为用例不少于 13 条 — 当前 12 条"报红，并打印出逐文件计数差；LL 在
`gateway_port.rs` 末尾再定义一份 `struct PortState` → "共享测试夹具只在 tests/mod.rs 定义一份 —
定义位置 {'PortState': ['gateway_port.rs', 'mod.rs'], …其余五项 ['mod.rs']"报红，这一次同时证明
放宽后的结构体判定不是摆设；MM 把 `capabilities.yaml` 里本轮新接的那条证据路径改写成不存在的文件名 →
"能力矩阵证据路径全部存在"报红。五次实验后 `cp` 还原并 `cmp` 逐字节相同、复跑 `check_architecture.py`
全绿，且额外断言了 `src/tests.rs` 未被留下。

行数账：登记条数 39 → 38（本轮唯一退出项 `crates/qx-execution/src/tests.rs` 1,088），38 条求和
53,453 → 52,365（−1,088），`added=[] changed={}`。这是连续第四轮净降（4o −2,861、4p −1,719、
4q −1,884、4r −1,088），退出登记集后列表首位仍是 `crates/qx-runtime/src/lib.rs` 3,660，前十位与
Phase 4q 记录完全一致（`ashare.rs` 1,978 仍在第 10）。**口径要如实说明**：这已是第二次让用例文件
整体离开计数集（第一次是 Phase 4o 的 `tests_main.rs`），棘轮从来只数 `crates/*/src/**/*.rs`，
本轮新写的五个文件仍在 `src/` 下、只是各自低于 500 行才不需登记，因此约束由"登记行数"换成了
"目录内文件逐个 < 500 行 + 用例总数 ≥ 13 只增不减"这两条形状门禁 —— 与 4o 同一套做法，代价是真实
代码总量没减（1,097 vs 1,088，多 9 行脚手架），收敛的仍是"单文件长度"这一个可维护性口径。

### Changed（Phase 4q：qx-cli crate 根第二批六簇拆分 —— `main.rs` 1,884 → 288 行，架构不变量 65 → 72 项）

- Phase 4p 之后 `main.rs` 还剩 1,884 行、41 个顶层条目，仍是登记列表第 11 项。本轮把剩余的六条职责链
  整簇搬出，crate 根只留命令分派壳（288 行 / 7 个顶层条目：`main`、`run_unified_backtest`、
  `run_recovery_child`、`parse_backtest_quantity`、`python_interpreter`、`python_interpreter_origin`、
  `static NEXT_STRATEGY_RING_ID`）：`runtime_check.rs`（490 行 5 项，`collect_runtime_check_report` /
  `run_runtime_check` / `validate_runtime_references` / `validate_ashare_component_json` /
  `validate_dataset_bundle_component_references`）、`live_check.rs`（336 行 3 项，
  `push_live_check` / `collect_live_check_report` / `run_live_check`）、`market_bridges.rs`（264 行 10 项，
  行情桥一侧的 `account_event_log_name` / `configured_account_event_logs` /
  `spawn_api_projection_bridge` / `ccxt_event_log_name` / `ccxt_market_event_log_name` /
  `PaperMarketBridge` / `PaperMarketQuote` / `paper_market_worker_matches_instrument` /
  `open_paper_market_bridges` / `bridge_market_quote_to_paper`）、`strategy_binding.rs`（210 行 4 项，
  `validate_ccxt_worker_binding` / `strategy_current_qty` / `strategy_current_qty_for` /
  `build_strategy_contract_input`）、`path_resolution.rs`（192 行 6 项，`resolve_ccxt_config_path` /
  `is_explicit_absolute_path` / `resolve_runtime_relative_path` / `resolve_runtime_asset_path` /
  `resolve_strategy_runtime_paths` / `verify_strategy_artifact`）、`scheduler.rs`（143 行 6 项，
  `utc_schedule_tick` / `civil_from_days` / `scheduler_manifest` / `scheduler_jobs_path` /
  `load_scheduler_state` / `dispatch_scheduled_jobs`）。六个文件全部在 `OVERSIZED = 500` 门槛内，
  而 crate 根自身也降到门槛以下 —— 于是它整项退出登记列表。
- **仍然只搬行、不改语义**：形态变化只有 `//!` 模块头、`use super::*;`、`pub(crate)` 提升与根里的成对挂载，
  共 34 个条目升为 `pub(crate)`；`PaperMarketBridge` / `PaperMarketQuote` 的 12 个字段一并提升
  （否则 `E0616` / `E0451`，即 Phase 4p 记过的同一类可见性错误）。等价性判据是符号 token 多重集
  13,653 → 13,653（lost=0 / gained=0），且两侧经同一个 `strip()` 处理 —— 首轮版本只剥了拼接侧，
  把 `main.rs` 自带的 6 行 `//!` 与 16 处既有 `pub(crate)` 只计在左边，报出 `LOST 209` 的假差异。
  用例口径逐项未变（workspace 539、`--bin qx-cli` 默认 56 / 全特性 59、`src/tests/` 内 59 条），
  `result_hash=b26e1d4d4d430cb1` 与 4j～4p 基线逐位相同。
- 新增 7 项不变量（65 → 72），全部针对本轮新打开的出口：职责簇模块清单从 5 个扩到 11 个（逐个 500 行门槛，
  同一条检查覆盖）；`CLI_ROOT_ITEM_CEILING` 从 41 收到 **7**（根已是分派壳，任何实现回流都会立刻越限）；
  链路入口唯一性检查从 4 个冒烟标识符扩到 10 个（新增
  `collect_runtime_check_report` / `collect_live_check_report` / `dispatch_scheduled_jobs` /
  `open_paper_market_bridges` / `resolve_strategy_runtime_paths` / `build_strategy_contract_input`
  六个定义点，共 6 项检查）；再新增"crate 根 `main.rs` 已退出行数登记集（低于门槛且不在预算表内）"，
  把"拆到不再登记"这件事本身钉成门禁 —— 否则往根里写回 500 行以下代码只违反条目上限、不违反棘轮。
- `crates/qx-cli/src/` 现在是 25 个 `.rs` + `venue_runtime/` 目录 + `tests/` 目录。本轮没有改动任何
  一条 CLI 语义、任何一份模板、任何一个测试断言。

### Validation（phase4q 轮实测，日志 `/tmp/qx_phase4q_gate.log`，2026-09-19，本轮不做功能改动，只验收第二批搬家与新增的七项形状门禁）

```text
FMT_EXIT=0
CLIPPY_DEFAULT_EXIT=0             # cargo clippy --workspace --all-targets
CLIPPY_WARNING_LINES=0
CLIPPY_FEAT_EXIT=0                # cargo clippy -p qx-cli --all-targets --features sqlite,postgres,nats
TEST_EXIT=0
OK_LINES=71  RUST_PASSED=539  RUST_FAILED_SUITES=0
                                  # 与 Phase 4p/4o 逐位相同：本轮只搬行，用例不增不减
CORE_EXIT=0（tests/ledger.rs 18 passed）/ CORE_LIB_EXIT=0（lib 34 passed）
CONTRACT_EXIT=0（venue_report_contract 1 passed）
RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
56 passed / 59 passed             # cargo test -p qx-cli --bin qx-cli 默认特性 / sqlite,postgres,nats
SHAPE: main.rs 288 + ecosystem_smoke 481 + runtime_wiring 344 + readiness 267 + configured_backends 305
       + api_service 386 + runtime_check 490 + live_check 336 + scheduler 143 + market_bridges 264
       + path_resolution 192 + strategy_binding 210 = 3,706 行
       （本轮六文件 1,635 行 + 根 288 行 = 1,923，比拆前根 1,884 多 39 行模块头/挂载固定代价）
ROOT_ITEMS=7  ROOT_LINES=288  TEST_TOTAL=59
TOKENS before=13653 after=13653  LOST 0 GAINED 0  EQUIV
ARCH_EXIT_BEFORE_SNAPSHOT=1       # 唯一红项："crate 根 main.rs 已退出行数登记集 … 登记=True"
                                  # 预算表此刻仍写着 main.rs: 1884，快照前必然红，属预期
已写入 maturity/line_budgets.yaml（39 个超 500 行文件）
ARCH_EXIT_AFTER_SNAPSHOT=0  ARCH_ITEMS_AFTER_SNAPSHOT=72
diff: 11d10  < crates/qx-cli/src/main.rs: 1884   BUDGET_DIFF_EXIT=1
entries 40 → 39 / sum 55,337 → 53,453（−1,884）
dropped=['crates/qx-cli/src/main.rs']  added=[]  changed={}
MUTATED_II_ARCH_EXIT=1   → "crate 根顶层条目不多于 7 个" — 当前 8 个
MUTATED_JJ_ARCH_EXIT=1   → "Phase 4p/4q 拆出的兄弟模块逐个在单文件行数门槛内" — 越界 ['scheduler.rs']（JJ_LINES=500）
MUTATED_KK_ARCH_EXIT=1   → "链路入口 collect_live_check_report 的定义点唯一且在 live_check.rs" — 定义于 ['live_check.rs', 'main.rs']
MUTATED_LL_ARCH_EXIT=1   → "拆出的模块在 crate 根以 mod + pub(crate) use x::* 成对挂载" — 缺配对 ['scheduler']
MUTATED_MM_ARCH_EXIT=1   → "crate 根 main.rs 已退出行数登记集" — 288 行，门槛 500，登记=True
RESTORE[II/JJ/KK/LL/MM]=identical  RESTORED_*=0（五次实验后逐字节还原并复跑全绿）
PY_EXIT=0（Ran 43 tests）/ VALIDATE_EXIT=0 / TEMPLATES_VALIDATED=17/18
cli smoke：verify / all / runtime-check / runtime-check-binance / config-validate /
           backtest-builtin / backtest-multi-builtin / fast-backtest-ashare / paper-e2e /
           strategy-backtest 全 0；runtime-check-production=2、unknown-command=2、
           binance-worker-bad-role=2（三条均为记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 4j～4p 基线逐位相同
STUB_EXIT=2 + 自证文案仍在（QX_PYTHON 未设置时回落 WindowsApps 占位桩，Phase 4n 的诊断未因搬家退化）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（五次变异实验后工作树逐字节校验）
UNCOMMITTED_PATHS=150
```

反向验证五次，各自只红在本轮新增（或收紧）的那一项：II 在根里等行数注入一个新的 `pub(crate) fn`（7 → 8）
→ 条目上限报红，证明 Phase 4p 那条 41 的上限收到 7 之后确实咬得住；JJ 把 `scheduler.rs` 撑到恰好 500 行
→ 模块门槛报红（`>= OVERSIZED` 判定，且 500 行仍不需登记，红因不可能落到棘轮上）；KK 在根里另抄一份
`collect_live_check_report` → 链路入口唯一性报红，同时条目上限也报红（第二处定义本身也是一个新根条目，
两条门禁重叠而非冲突）；LL 删掉 `pub(crate) use scheduler::*;` 一行 → 配对挂载报红；MM 把
`crates/qx-cli/src/main.rs: 1884` 写回预算表 → "已退出登记集"报红。

本轮另有一处**门禁脚本自身的缺陷**值得记下：首次整跑把 `line_budgets.yaml` 的变异备份做在 `--snapshot`
**之前**，MM 的 `cp` 还原因此把上一轮（Phase 4p）的旧表写回工作树 —— 表现为 `RESTORED_MM_ARCH_EXIT=1`、
棘轮差异 `entries 40 → 40 / sum +0`、而末尾 `MUTATION_RESIDUE=yes` 仍是假绿（它比对的就是那份陈旧备份）。
修法是在快照段末尾重抓一次备份，然后**整跑重跑换新日志**（上方数字全部出自重跑那一份），不拼接。

行数账：登记条数 40 → 39、39 条求和 55,337 → 53,453（−1,884），唯一变化就是 `main.rs` 整项退出登记列表。
连续三轮净降（4o −2,861、4p −1,719、4q −1,884）。当前最大项依次是 `crates/qx-runtime/src/lib.rs` 3,660、
`qx-storage/src/lib.rs` 3,536、`qx-api/src/lib.rs` 3,137、`qx-xingban/src/backtest.rs` 3,024、
`qx-storage/src/sqlite.rs` 2,565、`qx-adapter/src/binance.rs` 2,307；`qx-cli` 的 crate 根已完全不在列表内。
代价与 Phase 4p 同性质：十二个文件合计 3,706 行，比 Phase 4p 末的 3,667 行多 39 行模块头与挂载开销，
收敛的是"单文件长度"口径而非功能体积。剩余同量级回收点 `ashare.rs` 1,978 / `backtests.rs` 1,379 /
`crates/qx-execution/src/tests.rs` 1,088 需要按真实职责重写才能拆 —— "纯搬家"这一类到此确实见底。

## v0.0.1

Initial Qianxing V5 architecture release candidate.

### Added

- Event-driven kernel architecture
- Domain model layer
- Market data abstraction
- Trading lifecycle
- Matching engine foundation
- Risk rule engine
- State replay foundation
- Plugin extension framework
- SDK boundary
- CLI and CI validation framework

### Validation

- Architecture checks
- E2E pipeline specification
- Deterministic replay verification
- Benchmark validation framework
