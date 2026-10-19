# 安装与构建指南

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

**产物身份**：带可读本地数据集的 `init` 还会生成经 `project-manifest-v1` 校验的 `qianxing.project.json`；每次通过重放的回测除 RunManifest、summary、equity、fills 外，还会生成 `*.record.json`（RunRecord），对这四类文件逐个写入摘要。`report` 会按摘要指针读取 RunRecord、核对同轮 RunManifest 并重算每个产物摘要，缺失或被篡改即拒绝展示报告。落盘份数由用例钉住而不是文档自说：`init --strategy macd` 落 **10** 份、首轮回测再写 **6** 份，所以一个 quickstart 项目共 **16** 份（`crates/qx-cli/tests/init_onboarding_process.rs` 逐个数）。

装好的是哪个构建不必靠文件哈希自证：`qx-cli version` 那一行与 `doctor` 的第一格 `build_identity`、`status --json` 与 `report --json` 里的 `runtime_version` 同出一处（`crates/qx-cli/src/build_identity.rs`），值由构建期注入的包版本、git 提交、目标三元组与构建档拼成，源码里没有硬编码的版本号；`--version` 与 `-V` 是同一条入口的两个别名，三条写法逐字相同且都退 0。

装好的 exe 不用把仓库带着走。示例配置只经**一条**查找链解析（`crates/qx-cli/src/deploy_lookup.rs`，V13 第三十一遍 ①/#266，另有常驻门禁判据钉住这条链的入口只有一处定义、示例读取的报错只由一处拼装）：`QX_DEPLOY_DIR` → 可执行文件同级 `deploy/` → 再往外一层 → 构建期源码树的 `deploy/` → 当前目录的 `deploy/` → **二进制里的内置模板清单**。最后一层由 `crates/qx-cli/build.rs` 在构建期把本目录顶层那 53 份 JSON 原样快照进 exe，需要时把**整份清单**落进当前用户的临时目录（不是只落被点名的那一份——`fast-backtest` 的 manifest 里作业按同级文件名引用 runtime/bars/spec，只落一份会让这条链在下一格读取上断掉），并按清单内容签名分桶；只读入口那 7 处以 deploy 目录示例文件名为默认值的路径参数全部挂着 `parse_deploy_path`，这条挂载面本身也是常驻判据（少挂一处就报，判据没有对象时同样报，不会静默给绿），所以同名目录里的陈旧副本不可能被新二进制读到；清单为空时构建直接失败，模板缺失这件事不该跟发布物一起出门。

这一格两侧都有常驻判据：链条形状由 `crates/qx-cli/tests/unit/deploy_lookup.rs` 钉，屏幕与退出码由 `crates/qx-cli/tests/default_example_paths.rs` 从**无关启动目录**逐条实跑钉（只读入口四条 + 裸 `backtest` + A 股 `fast-backtest` + 位置参数搬迁 + worker 精确路径）。把整棵工作树复制到仓库外、删掉那份 `deploy/` 再逐条跑通属于**当轮实测**，连同它的三枚 `result_hash` 一起留在 [`README-历史实测快照-2026-10-09.md`](archive/README-历史实测快照-2026-10-09.md)。两条口径要分开：**接了查找面的入口**（只读入口的默认值，加上 `fast-backtest` 的 manifest——它在读取点自己走这条链）里，只要那是"示例配置形状"的路径且当前目录没有这一份，就按上面的顺序搬走，并在 stderr 说一句 `[查找 · Lookup]`，不做静默替换；**没接这条链的入口**（`scheduler-worker` 一类精确路径）按当前目录原样解析，读不到时错误正文点名"这一份示例在别处存在: <路径>"。屏幕上那行 `config_fingerprint=` 描述的是哪一份文件，两种情形下都始终可查。

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
[`docs/archive/README-历史实测快照-2026-10-09.md`](archive/README-历史实测快照-2026-10-09.md)
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
uv pip install --offline dist/qianxing-0.1.0-cp310-abi3-win_amd64.whl
python -c "import qianxing as qx; print(qx.doctor())"
# 按能力选装 extras（不装也 import 得动，用到才在调用点给可执行提示）
uv pip install --offline "qianxing[ccxt]"     # CCXT 行情/交易 worker
uv pip install --offline "qianxing[a-share]"  # A 股数据源（Windows 连 tzdata 一起带）
```

PyO3 已启用 `abi3-py310`，wheel 以 `cp310-abi3` 标签构建。发布工作流每个平台/架构只需构建一份 wheel，可供 CPython 3.10 及更新版本安装；Windows、macOS、Linux 仍各自需要匹配的原生平台 wheel。CI 在 Python 3.10–3.13 上验证安装和回测。配置 `PYPI_PUBLISH_ENABLED=true` 与 PyPI Trusted Publisher 后会自动发布；未配置时只构建并附加到 GitHub Release，不上传 PyPI。

#276 之后 wheel 的强制依赖清单是空的：`ccxt` 与 Windows 的 `tzdata` 都改成可选 extras（`[ccxt]` / `[ccxt-pro]` / `[tz]` / `[a-share*]`），`pip install <wheel>` 在任何索引状态下都能装成，`--no-deps` 不再是必需项。`ccxt` 只在真的跑 CCXT worker 时才 import，`Asia/Shanghai` 只在真的取 A 股时区时才解析（#253 把顶层求值挪到用时），缺谁都在调用点抛带 extras 名字的可执行提示。

正式 Python SDK 的入口为 `import qianxing`。SDK 已开放 Bar、L1 Tick、L2 OrderBook（含双边做市回测）、多腿回测、参数实验、产物复核、可取消 RunHandle，以及经服务端授权的 Paper/Live 命令工作流；匹配、风控、OMS、账簿、事件与回放由共享 Rust 用例执行。实盘交易所认证、TWAP/VWAP 父单运行生命周期和各市场/venue 能力仍按 maturity 登记面验收，不因 SDK 路由存在而视为通过。

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

**产物身份按载荷报，不按整档摘要报（口径由用例常驻核对）**：装完包要核对的是尺寸 / 条目数 / 条目 CRC / 内嵌 `_qianxing_native.pyd` 的 md5 等于当轮 `target/release/_qianxing_native.dll` / 发布产物里那颗几百万字节的 `target/release/qx-cli.exe` 中的播报字面量计数。为什么这里不报整档 sha256：同一份载荷重打包出来的整档 sha 就会变（zip 时间戳参与打包），把它写进交付文档等于给读者一把量不出东西的尺子——所以产物身份只按载荷报，这条口径由 `crates/qx-cli/tests/unit/artifact_identity_doc.rs` 核对：它不许交付文档里出现 64 位十六进制的整档摘要，并要求上面这套骨架与判据文件互相点名（#159 收口）。

**exe 字面量计数是单向证据（#179）**：数到 **N>0** 次能证明这条播报进了装机产物；数到 **0** 次证明不了"这个构建没有这个能力"——链接器会把重复常量池化、会把没用到的分支整个丢掉，一次改名或一次字符串拼接就能让计数归零而行为照旧。所以计数只用作"当轮改动真的落进了发布物"的正向核对，反向结论（某能力在发布物里缺席）一律回源码、用例与 `--features` 组合去判，不从二进制计数推。

**安装包必须排在本轮最后一次构建之后**（#194）：打包脚本在 stage 之前重链原生扩展，所以 wheel 必须通过 `tools/build_python_wheel.*` 构建，不能把旧的扩展文件直接打包。本轮已修改 Python facade 与发行元数据，归档里的历史载荷尺寸、扩展摘要和包导入记录不能作为当前发布件证据；发布前要用当前源码重建 wheel，并在干净环境从安装后的 `qianxing` 命名空间检查能力。

### 装不上时的四个坑（都是本机踩过的）

| 症状 | 真正的原因 | 修法 |
|---|---|---|
| `[0/9]` 报「找不到可用的 Python 3 解释器」，或 `[4/9]` 恰好 2 条 Python 桥用例失败 | Windows 上 PATH 里的 `python` 是 Microsoft Store 占位桩：退出码 0、什么都不打印 | 按 `[0/9]` 印出的两条修法之一：建仓库 venv，或 `set QX_PYTHON=<完整绝对路径>`（必须是文件路径，写成目录会让整树测试以 `TEST_EXIT=101` 崩） |
| 跑 `.ps1` 入口直接 `SecurityError / UnauthorizedAccess`，一行脚本都没执行 | Windows 客户端默认执行策略是 Restricted，未签名脚本一律拒 | 用上面写着的形式：`-ExecutionPolicy Bypass -File`；README 里每条 PowerShell 入口都由门禁核对带着 Bypass |
| `[6/9]` 或 A 股相关用例 `AshareProviderError`（报错原文点名 `pip install tzdata`） | Windows 没有系统 IANA 时区库，`tzdata` 是 `python/pyproject.toml` 里声明的平台依赖；`import qianxing_ashare` 本身已不再需要它（V13 #253 把时区取值挪到用时） | `python -m pip install tzdata`（或 `uv pip install tzdata`） |
| 改了 `build.bat` 之后 `[1/9]` 之前就炸，报 `\'不是内部或外部命令\'` | 用 `sed -i`/MSYS 工具编辑会把 CRLF 抹成 LF，cmd.exe 读不了带中文的 LF 批处理 | 归一化回 CRLF，且按字节做（`raw.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")`）；门禁会数孤立 LF |

