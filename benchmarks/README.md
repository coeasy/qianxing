# Qianxing Benchmarks

性能基线与复现入口。**本目录不是免责声明**：`run_baseline.py` 是真会跑的驱动器，读数落
`benchmarks/results/baseline.json`。

当前可复核负载的统一资源表见[容量与资源画像](../docs/容量与资源画像-2026-10-23.md)；表中数字按证据路径区分合成研究负载、异步 API 压测与未测产品 profile，不构成通用最低配置承诺。

## Targets（延续 V5 口径）

- kernel event throughput
- matching latency
- replay speed
- runtime scheduling
- language bridge overhead

## Requirements（延续 V5 口径）

Benchmarks must preserve deterministic inputs and report:

- operations/sec
- p50/p95/p99 latency
- memory usage
- allocation behavior

## 端到端基线：`run_baseline.py`

无新依赖（纯标准库），驱动**已构建的 qx-cli 公开命令面**：

```bash
# 先构建
cargo build --release -p qx-cli
# 再跑（缺省 N=1000,8000,64000，重复 7 次；p50/p95 按墙钟取）
python benchmarks/run_baseline.py --binary target/release/qx-cli.exe --out benchmarks/results/current.json
# 机器可读
python benchmarks/run_baseline.py --json --out benchmarks/results/current.json
# 与同硬件基线对比；目标台账登记组回归超过 10% 时非零退出
python benchmarks/compare_baseline.py benchmarks/results/baseline.json benchmarks/results/current.json --targets maturity/targets.yaml
```

它测什么、不测什么（诚实口径，别把没测的读成测了）：

| 覆盖 | 不覆盖 |
|---|---|
| 端到端墙钟：读帧 → 校验 → 逐事件回测 → 印摘要 | 内核内部 append / refresh / replay / read-model / recovery 的**逐段** p50/p95 |
| 回测吞吐随 N 的变化（N=1k/8k/64k） | 内存峰值与分配行为（需单独采集） |
| 「同样输入必得同样结果」：`result_hash` 跨重复稳定 | L2 撮合时延分位数、语言桥开销、内存峰值与分配行为 |

JSON 同时包含 `linear_scaling_curve`：相邻 N 的 p50 差分、每 Bar 边际墙钟与斜率变化。它只描述
包含进程启动和 JSON 解析的端到端回测成本；斜率下降表示固定启动成本被摊薄，只有边际成本上升
超过 20% 阈值才标记为候选正向拐点。该结果不是内核稳态单 tick 成本曲线，也不是统计显著性结论；
不得用它替代 `maturity/capabilities.yaml` 里按 EventLog 长度测得的稳态单 tick 数据。

## Crypto Paper worker 端到端

```bash
python benchmarks/run_crypto_paper_e2e.py \
  --binary target/release/qx-cli \
  --repeats 5 \
  --out benchmarks/results/crypto-paper-e2e.json
```

该采样重复调用现有 `tools/paper_acceptance.py`，每轮在隔离目录里走 scheduler、strategy、Paper execution、
账簿/EventLog/审计、同目录幂等重跑和独立目录事实一致性。每个样本都必须先通过 acceptance，随后报告
整轮 wall-clock p50/p95、主链订单/成交/账本计数、进程树峰值 RSS、CPU、I/O 和进程数；资源项按平台能力
如实输出 `null`，不以缺少采样的 0 代替。它不拆解单笔订单延迟，也不覆盖真实 funding provider 或 8 小时结算调度。
CI 把 workload `crypto-paper-worker-acceptance-v1` 作为已登记的 `end_to_end` 项比较，首次匹配仅记录，
后续同硬件回退超过 targets.yaml 的 10% 阈值会阻断。它与 qx-zhenlu v2 microbenchmark 共同形成 Crypto Paper
性能证据，但不单独声称完整 profile 已测量。

## Rust 微基准：Criterion + JSON 证据

`criterion_performance` 使用 Criterion 覆盖 `append_batch`、EventLog `digest`、全量重放、
Scheduler 活跃运行索引、L2 撮合、Paper Venue 订单生命周期、SQLite 管线的全量/增量重放对拍，
以及 A 股昨收逐点反扫/序列预计算；对拍必须得到
相同 digest，A 股两种计算必须逐 Bar 返回相同锚点。运行完整测量并同时保留兼容 JSON 记录：

```bash
cargo bench -p qx-core --bench criterion_performance -- --noplot
cargo bench -p qx-scheduler --bench criterion_performance -- --noplot
cargo bench -p qx-xingban --bench criterion_performance -- --noplot
cargo bench -p qx-zhenlu --bench criterion_performance -- --noplot
cargo bench -p qx-cli --bench criterion_performance -- --noplot
python benchmarks/run_rust_bench.py --out benchmarks/results/rust-micro.json
```

EventLog 与 Ledger 基准归 `qx-core`，调度索引归 `qx-scheduler`，撮合/A 股归 `qx-xingban`，
Paper Venue 订单生命周期归 `qx-zhenlu`，应用级回测归 `qx-cli`。各 crate 的 Criterion group 使用
独立前缀，避免 workspace 共用的 `target/criterion` 下同名结果互相覆盖。`run_rust_bench.py`
依序运行所有 crate，核对必需样本后再运行稳定 JSON 微基准，并把 Criterion 的 mean、median、
标准差和 95% 置信区间摘要写进同一份证据。只有同一硬件指纹和相同 workload 才参与趋势比较；
跨硬件结果单独留档，不合并成一条趋势。

`qx_zhenlu_crypto_paper_lifecycle_v2` 测量 1/16/256 笔并行挂单的定点规格校验、加权限频、
Paper Venue 提交、maker/taker 费用、首次部分成交、账簿成交记账及一次 funding 计算/结算。
它不包含策略 worker、真实 provider 费率读取、结算周期调度或持久 EventLog，不能单独作为完整
`crypto-paper` profile 测量。`v2` workload id 与之前不含精度/限频/账簿/funding 的 PaperVenue 微基准隔离，
避免将不同语义范围当作同一 Criterion 趋势基线。

`backtest_risk_metrics` 在相同权益序列上配对比较 qx-core 当前计算与旧的双临时向量算法，覆盖
1k/8k/64k 个周期；测量前逐字段断言 Sharpe、Sortino、Calmar、胜率和收益因子完全相等。当前实现
保留一份收益向量供多个指标共享，避免旧实现额外分配 downside 向量并重复遍历。它只测量指标计算核，
不代表完整回测提速，也不采集 allocator 级分配计数；对应测量保存在
`maturity/evidence/rust-micro-backtest-risk-metrics-2026-10-17.json`。两遍无分配试验因端到端计时
回退约 17%–18% 而未保留；当前单向量/双向量配对与该裁定的原始指标见
`maturity/evidence/rust-micro-risk-metrics-algorithm-selection-2026-10-18.json` 和 V13 §9.422。

`benchmark_relative_columns` 对拍 qx-core 的 tuple 输入和列式输入。两种路径计时前要求完整指标
结构完全相等；tuple 路径在计时区构造 `(timestamp, close)` Vec，列式路径直接消费 BarFrame 的
`ts` / `close_raw` 两列。它只测量基准相对指标核，不包含 CSV/BarFrame 解码或 JSON 报告序列化，
也不代表整轮 A 股回测提速。该轮 1k/8k/64k P50 与可避免的 tuple payload 见
`maturity/evidence/rust-micro-benchmark-relative-columns-2026-10-18.json`。

Criterion 还并列测量 `EventLog::validate` 的当前缓存有效性实现和原先「重复构建 seq BTreeSet」
基线，分别覆盖 1k/8k/64k 条有效事件。两者使用同一日志和完整校验条件；该微基准只量化
校验函数，不替代回测端到端基线，也不改变结果身份。

SQLite EventLog manifest 的增量写入使用带 `v2:` 标记的 rolling event digest，避免每次追加都为
旧版“长度前缀 + 全部事件”摘要重新扫描历史；读取仍兼容旧的十进制摘要，并在全量读取时校验事件内容。
`event_log_manifest_digest` 在同一份 1k/8k/64k 日志上配对比较旧全量重算与新 rolling digest；它验证
摘要计算的代价，不等同 SQLite I/O 或整个 ingest 链的加速倍率。

EventLog 前缀摘要同时测量追加后复用缓存的摘要+序号核对，与旧实现分别扫描摘要和最大序号的两趟路径，覆盖 1k/8k/64k
事件。追加会自动缓存追加前的不可变前缀摘要，存储写入可直接核对；缓存有界且不改变公开 digest 格式，超过八个未提交追加导致边界淘汰时仍会扫描前缀。

分段 EventLog 文件写入另有独立基准，不依赖 Criterion：同一 `qx-storage` 实例复用已验证 manifest 的热追加，与新建 store 实例、在计时区内读取并校验历史后追加，分别测量 1k/8k/64k 历史各 7 次。运行命令和落盘证据：

```bash
python benchmarks/run_segmented_event_log_bench.py
```

它报告同实例热写、新实例完整校验后追加及 manifest 字节数；操作系统文件缓存不受控，因此“新实例”不等于冷盘。它是合成 `Settle` 历史的存储微基准，不替代端到端回测或九档产品 profile。

稳定 Rust 基准另含 `steady_state_pipeline_ingest`，证据 JSON 的 `steady_state_tick_curve` 字段
输出每档的单 tick p50/p95、相邻边际成本和候选拐点：SQLite EventLog 预置 N 条确定性历史，随后
执行 5 个独立窗口，每窗口在同一 live pipeline 上逐笔 ingest 21 条新行情事实；每档共计 105 个
计时样本，分别按 N=1k/8k/64k 报告单笔 p50/p95 与
每秒 tick 数。历史只用于形成不同日志长度，计时覆盖真实 `LiveEventPipeline::ingest` 的刷新、
事件校验/归约和 SQLite 追加。它是合成 `Settle` 历史 + QuoteTick 的工作负载，不代表某个交易
profile；此曲线可观察 EventLog 变长时实际单次写侧 tick 是否继续线性增长。

## 应用层分配计数

`measure_allocations` 是单独编译的 `qx-app::run_backtest` 负载示例，使用包装 `std::alloc::System`
的计数 allocator 记录应用用例期间的分配/释放调用、累计申请字节和峰值活跃申请字节。Python 驱动器
另记录进程墙钟、RSS、CPU、I/O、输入/产物大小、二进制哈希与源码指纹；分配计数不混作 RSS，且不包含
spec 解析、进程启动或结果 JSON 序列化。它只测一个合成 crypto-spot SMA 回测，不提升任何产品 profile
的测量状态。

```bash
cargo build --release -p qx-cli --example measure_allocations
python benchmarks/run_allocation_profile.py \
  --binary target/release/examples/measure_allocations.exe \
  --sizes 1000,8000,64000 --repeats 3 \
  --out maturity/evidence/bar-allocator-capacity-2026-10-23.json
```

证据含每次原始采样和工作负载输入/规格哈希，位置为
`maturity/evidence/bar-allocator-capacity-2026-10-23.json`。Windows 文件名按本机构建产物调整；
Linux/macOS 使用无 `.exe` 的路径。

## 市场身份回测代理

`run_profile_backtest.py` 读取 `maturity/fixtures/scenarios/` 中七类产品规格，按每份规格的标的、
报价单位、最小下单量生成 N=1k/8k/64k Bar，并实际调用 CLI `backtest builtin`。它可比较产品规格
绑定下的回测成本，但不执行各 profile 的专属规则，因此输出显式写 `profile_coverage_claim: false`，
`maturity/targets.yaml` 九档仍保持 `unmeasured / unavailable`。期权 Bar 回测因缺少链/行权输入会拒绝；
Testnet 和多 Venue profile 需要各自的端到端外部/组合路径，不从此代理推断性能。

```bash
python benchmarks/run_profile_backtest.py
```

报告落在 `maturity/evidence/profile-market-backtest.json`；任何 profile 从 `unmeasured` 改为
`measured` 前，必须补齐对应完整场景路径与 evidence，而不能引用本代理的市场身份结果。

2026-10-18 UTC 的当前 Release 复测覆盖 6 个可由 Bar 回测表达的市场规格、每类 1k/8k/64k Bars，各 5 次；
期权 fixture 在缺少合约链/行权输入时明确拒绝。每个「fixture × Bar 规模」使用单独的 runtime 配置和 storage 目录，
证据同时固定 runtime 配置与 market spec 的 SHA-256，避免同一标的不同数据帧之间发生真实的 dataset manifest 冲突。
64k 档 P50：A 股 506.524ms、国内期货 513.000ms、国际股票 500.829ms、FX/CFD 493.235ms、加密现货
509.863ms、加密永续 503.483ms。六类全部跨重复得到稳定结果哈希。证据 `maturity/evidence/profile-market-backtest.json`
（归档副本 `profile-market-backtest-current-2026-10-19.json`）。这些是产品规格绑定的合成 Bar CLI 代理，不执行各 profile 的
专属 Paper/日历/公司行为/资金费/venue 工作流；九档完整 profile 仍为 `unmeasured / unavailable`。

### A 股规则开启的吞吐曲线

`run_ashare_profile_backtest.py` 在同一三个 Bar 规模上调用 `backtest builtin ema_cross --config`，
实际装配 A 股规则与费率、交易时段日历和 PIT 可见现金分红；报告记录输入 SHA-256、绑定观察项、
拒单数、Release 二进制 SHA-256、重复运行 hash、bars/s 和每个进程的 `peak_rss_bytes_max`。通用产品规格代理同样记录进程峰值 RSS。Windows 使用进程 PeakWorkingSetSize，Linux 使用 VmHWM，macOS 以 10ms 间隔采样 RSS；报告标明只统计 qx-cli 进程，不含后代进程或共享系统缓存。该读数可以比较同一环境内的进程常驻峰值，不统计分配次数，也不是整机峰值内存。它比只绑定 market spec 的通用代理更接近 A 股回测路径，
但合成日历只跳过周末，不含交易所节假日/临时休市，且没有逐项验证 T+1、整手、涨跌停、停牌和
除权除息分支，所以 `ashare-research` 仍须保持 `unmeasured`，直到完整 profile 的场景验收证据齐备。
2026-10-22 同机 Release 采样（Ryzen 7 8700G / 8 核 / 61 GiB RAM）新增峰值 RSS：通用规格代理在
1k/8k/64k Bar 的六个可测市场夹具中分别为 8.1–8.2 / 11.0–11.2 / 40.6–41.0 MiB；A 股规则路径为
8.4 / 11.2 / 40.8 MiB。证据见 `maturity/evidence/profile-market-backtest-rss-2026-10-22.json` 与
`maturity/evidence/ashare-rules-backtest-rss-2026-10-22.json`。测量只覆盖 qx-cli 进程的峰值工作集，
不是工作集之外的总进程树或整机峰值；全部输入为合成 Bar，因此九个产品 profile 仍为 `unmeasured`。
日期型公司行为会映射到同一上海交易日首根 Bar；`--actions > 1` 时驱动器还会执行一条公司行为对照，
要求 stress 结果 hash 与单事件控制不同，防止只扫描了未生效事件却被误读为有效负载。
不同 Bar 规模使用独立的合成证券代码和临时存储目录，避免数据集注册表把同一 instrument 的不同帧判为清单身份冲突。
2026-10-18 本机 Release 复测使用 512 条有效现金分红、每档 5 次，1k/8k/64k P50 为
268.593/306.120/603.794ms，P95 为 280.307/319.789/606.705ms；三档规则/费用来源、动作影响与重复 hash 均核验通过。
证据为 `maturity/evidence/ashare-rules-backtest-2026-10-18.json`。这是合成压力基线，不改变 A 股产品 profile 的测量状态。

2026-10-18 UTC 按当前工作树重新构建 Release 后，以同一 64k Bar / 512 条 PIT 分红工作负载重复 7 次，P50/P95 为
600.940/620.199ms、106,499.82 bars/s，二进制 SHA-256 为
`16b0a1d7e77fc2842413a8ac410b90371eb2c0ec096bfa16c16ae061121bd584`，证据为
`maturity/evidence/ashare-rules-backtest-current-2026-10-19.json`。该轮结果哈希跨重复稳定，且与单条分红控制组不同；
与前次证据的运行配置摘要不同，不作严格 A/B 或提速归因。仍不覆盖逐项规则分支和 Paper 生命周期，因此不翻转 profile 状态。

2026-10-21 基于当前工作树重建 Release 后，以三档分钟 Bar、每档 4 条 PIT 可见现金分红、每档 5 次重复复测；1k/8k/64k P50/P95 为
275.235/298.256ms、310.972/321.875ms、550.624/560.258ms，64k P50 吞吐 116,235 bars/s。拒单数为 32/258/2,064，
每档结果哈希跨重复稳定，4 条动作均相对单动作对照改变结果。当前二进制 SHA-256 为
`656a932bf7c45cfd8464dccbb414b6ff518f316b86ffc59651f6b71c7ca00047`，证据为
`maturity/evidence/ashare-rules-backtest-2026-10-21-current-release.json`。同日 A 股定向规则合同 33/33、
分钟涨停锚/T+1 在途卖单/PIT as-of 生命周期用例 8/8 通过，并登记在 `maturity/quant_scenarios.yaml`。
此证据仍使用合成分钟行情；日历现改为上交所/深交所公布的 2024-2025 节假日安排，不覆盖公告外临时休市，
也没有把全部制度分支压进吞吐负载，
因此 `ashare-research` 仍保持 `unmeasured`。二进制来自脏工作树，不能作为发布候选产物。

2026-10-22 当前工作树重新构建 Release CLI（SHA-256 `de6b1604e0ee290fbc5db7e6a04d3fcba2e6add3dd6a7347a3eec32c951796d4`，
测量源码指纹 `120262fd2ff81c4be74bdac6fe7fcaec62d62e2c2e9488d079f5cad7852a8d32`）后，
对通用六类市场规格代理与 A 股规则开启路径各完成 1k/8k/64k、每档 3 次重复测量，并采集 qx-cli 后代进程树 RSS。
通用代理的 64k P50 为 504.286–524.607ms，主进程峰值 RSS 为 42.45–42.78MB，进程树采样峰值为 38.01–42.78MB；
A 股规则路径 64k P50/P95 为 546.551/578.304ms，主进程峰值 RSS 为 42.70MB、进程树采样峰值为 41.46MB。
证据为 `maturity/evidence/profile-market-process-tree-current-2026-10-22.json` 与
`maturity/evidence/ashare-process-tree-current-2026-10-22.json`。进程树 RSS 是 10ms 采样值，短时尖峰可能漏采；
脚本运行时未发现后代进程（最大进程数为 1）。这些仍是合成 Bar 负载与有限规则路径，不能外推为 Paper、Testnet、
多 Venue 或完整产品 profile 容量，九档目标继续保持 `unmeasured / unavailable`。

```bash
python benchmarks/run_ashare_profile_backtest.py
```

本地证据写入 `maturity/evidence/ashare-rules-backtest.json`；CI 将同机同规模 p50/p95 纳入 ±20%
非阻塞趋势比较，但该 CI 读数不改变 `maturity/targets.yaml` 的 profile 状态。

### 独立进程并发容量代理

```powershell
python benchmarks/run_concurrent_capacity.py --binary target/release/qx-cli.exe
```

采样器使用同一份合成 256k crypto-spot Bar 输入，在隔离 storage 根目录中并发运行 1/2/4 个 qx-cli 回测，
由父级协调进程的后代树采集聚合 RSS 和进程数，同时校验每个任务在 warmup 与三次重复中的结果哈希。
当前证据 `maturity/evidence/concurrent-capacity-2026-10-22.json`：1/2/4 并发 P50 为 1344.860/1590.062/2210.233ms，
进程树峰值 RSS 为 174.03/244.79/336.11MB，P50 总吞吐为 190,354/322,000/463,300 Bars/s；4 并发观测到 5 个进程。
该测量使用 Windows 10ms 采样，短时 RSS 峰值可能漏采；它测的是独立进程并发，不测内部任务队列、完整 Paper 生命周期、
并行参数搜索或九档产品场景，不能据此设定 profile 容量承诺或改写 `maturity/targets.yaml`。

### 活服务异步任务容量

`run_live_api_capacity.py` 在隔离目录启动真实 `qx-cli serve`，仅开放本机回环 HTTP，生成合成 crypto-spot Bar，
同时提交回测并读取 `/health`、`/metrics` 和运行状态。它记录启动与健康检查时延、观察器活跃数、拒绝计数、
任务终态和 50ms 采样进程树 RSS。进程由脚本结束；证据里的非零退出码是这一停止动作，不是运行失败。

```powershell
python benchmarks/run_live_api_capacity.py --binary target/release/qx-cli.exe --bars 1000000 --concurrency 64 --timeout-seconds 300 --require-saturation --out maturity/evidence/live-api-capacity-release-64x1m-2026-10-22.json
```

`--require-saturation` 只有在实际观测到 64 个活跃观察器、第 65 个请求因观察器限额收到 503、
64 个原任务全部成功、观察器归零且释放后的新任务成功时才返回成功。预热或小规模联调可省略该参数，
但输出会明确标示 `watcher_saturation_and_recovery_proven=false`。采样器不连接真实交易所，也不把单次
合成现货负载当作九档产品 profile 的容量结论；尚需重复测量、长时间稳定性、分配统计和完整场景矩阵。
本机单次 Release 证据为 `maturity/evidence/live-api-capacity-release-64x1m-2026-10-22.json`：
64 路各 100 万 Bar 均成功，满额第 65 次启动返回 503，释放后新任务成功；启动 P50/P95 为
950/1587ms，运行中健康检查 P50/P95 为 252/828ms，50ms 采样进程树 RSS 峰值约 21.8GB。
这一组数不能当成内存上限或其他硬件的配置建议。较短的 512k Release 样本只观察到最高 50 个
同时活跃观察器，虽然 64 个任务均成功，也没有满足饱和边界验收。
运行完整复现前应预留高于本机实测 21.8GB 峰值的可用内存；小机器先用 `--bars 8000 --concurrency 4`
确认链路，不应直接运行 64 路负载。

### 多账户批次与策略槽容量代理

该采样直接运行一次 `qx-cli app multi-account-backtest`，比较账户数量及每账户策略槽数量；
预热后重复同一批次，并校验账户结果哈希、成交数和成功资金合计稳定。标准矩阵为 2/8/16/32 账户、每账户 1/4 个策略槽、每账户 8k Bar、三次重复；扩展 64k 容量矩阵见下方：

```powershell
python benchmarks/run_multi_account_capacity.py --binary target/release/qx-cli.exe --bars 8000 --repeats 3
```

正式规模代理证据 `maturity/evidence/multi-account-capacity-2026-10-22.json` 覆盖 2/8/16/32 账户、
每账户 1/4 策略槽、每账户 8,000 个合成 crypto-spot Bar、每配置三次重复；8 种工作负载均有成交且结果哈希稳定。
四策略槽的单策略数量按等额预留资金缩放，避免名义订单超过槽预算形成空跑。32 账户 × 4 槽配置观测到
60,224 笔成交、P50/P95 约 33.9 秒、进程树峰值 RSS 约 40.4 MiB；本机数据仅用于同机规划参考。
另保留 1,024 Bar smoke 证据作为测量链快速复核。该批次入口按账户顺序执行。完整 64k 矩阵仍只验证合成 crypto-spot 批次；它不覆盖内部队列并发、真实供应商、Paper、分配数或完整市场 profile，也不会更新产品 profile 状态。
测量器在 psutil 可用的平台还输出采样进程树 CPU 时间中位数，以及 OS 进程 I/O 计数器的读/写字节最大值；不可取得时写入 `null`。
CPU/I/O 来自 10ms 进程树采样，短任务或快速退出的子进程可能漏采，不能视为硬件计数器或 allocator 级分配画像。

针对 64k Bar 使用显式组合运行完整扩展矩阵，避免脚本默认矩阵意外扩容：

```powershell
python benchmarks/run_multi_account_capacity.py --binary target/release/qx-cli.exe --workloads 2:1,2:4,4:1,4:4,8:1,8:4,16:1,16:4,32:1,32:4 --bars 64000 --repeats 3 --out maturity/evidence/multi-account-capacity-64k-full-run-2026-10-22.json
```

初始证据覆盖 2 账户 × 1/4 槽、4 账户 × 1 槽；扩展后完整证据汇总见 `maturity/evidence/multi-account-capacity-64k-matrix-2026-10-22.json`。10 组矩阵（2/4/8/16/32 账户 × 1/4 槽）均完成三次重复、每组有成交且结果哈希稳定；使用相同 Release 二进制与硬件。2 账户 × 4 槽的汇总 P50/P95 为 36.961/38.738 秒，进程树 RSS 峰值 123.6 MiB。

补充高槽数负载：4 账户 × 4 策略槽、每账户 64k Bar、三次重复，见
`maturity/evidence/multi-account-capacity-64k-4x4-2026-10-22.json`。该负载处理 1,024,000 个
Bar-slot，观测 24,020 笔成交，P50/P95 为 68.543/68.973 秒，进程树 RSS 峰值 124.0 MiB、
CPU 时间中位数 62.531 秒；三次结果哈希稳定。该组合仍是顺序账户执行与合成现货数据，不能外推为
并发调度或真实市场容量。

另测 8 账户 × 1 策略槽的 64k 配置，处理 512,000 个 Bar，观测 8 笔成交，P50/P95 为
1.634/1.890 秒，进程树 RSS 峰值 41.6 MiB，CPU 时间 P50 为 0.766 秒，读写 I/O 计数均可取；
三次结果哈希稳定，证据为 `maturity/evidence/multi-account-capacity-64k-8x1-2026-10-22.json`。

增加 16 账户 × 1 槽（P50/P95 2.833/3.396 秒，RSS 41.6 MiB，CPU 1.875 秒，16 笔成交）及
8 账户 × 4 槽高负载（32 策略槽，P50/P95 125.557/125.702 秒，RSS 125.3 MiB，CPU 118.219 秒，
48,040 笔成交）。两项均三次重复且哈希稳定，证据分别为
`maturity/evidence/multi-account-capacity-64k-16x1-2026-10-22.json` 和
`maturity/evidence/multi-account-capacity-64k-8x4-2026-10-22.json`。账户按序执行；策略数增多带来的耗时与内存增长
只对这组同机合成输入成立，不构成通用线性外推。

32 账户 × 1 策略槽处理 2,048,000 个 Bar，P50/P95 为 5.457/6.771 秒，进程树 RSS 峰值 41.8 MiB、
CPU P50 2.859 秒，观测 32 笔成交且哈希稳定，见
`maturity/evidence/multi-account-capacity-64k-32x1-2026-10-22.json`。更大策略槽组合仍需独立测量，
不能按账户数或槽数简单倍乘推断。

16 账户 × 4 策略槽（64 个策略槽）处理 4,096,000 个 Bar-slot，观测 96,080 笔成交，P50/P95 为
267.605/282.284 秒，进程树 RSS 峰值约 128.2 MiB、CPU 时间 P50 为 241.938 秒；三次结果哈希稳定。
证据为 `maturity/evidence/multi-account-capacity-64k-16x4-2026-10-22.json`。这组数值仍是顺序账户批次
的同机合成输入，不能代表并行执行性能。完整扩展矩阵的最大配置为 32 账户 × 4 槽（128 个策略槽），处理 8,192,000 个 Bar-slot，观测 192,160 笔成交，P50/P95 为 621.547/697.248 秒，进程树 RSS 峰值约 133.2 MiB；三次哈希稳定。矩阵的十组配置均成功，证据汇总保存每组指标、二进制 SHA、硬件、来源证据 SHA 与 source fingerprint。所有工作负载为顺序账户执行和合成 crypto-spot；10ms 采样 RSS 是估值，不是硬上限。尚缺真实服务饱和响应/恢复时延、内部并行参数、长时间稳定性、分配数、供应商/Paper 及完整市场 profile。

### 多市场共享账户深度容量代理

`run_multi_market_depth_capacity.py` 直接测量 `app multi-market-depth-backtest`，按市场数与每市场快照数构造共享账户现货 L2 工作负载；每轮产物要求包含产品引擎的 EventLog/ledger 回放标志，且 RunManifest 结果哈希必须等于 outcome 哈希，再比较隔离重复的共享结果与数据指纹：

```powershell
python benchmarks/run_multi_market_depth_capacity.py --binary target/release/qx-cli.exe --markets 2,4,8 --snapshots 1000,8000 --depth-levels 10 --repeats 21 --out maturity/evidence/multi-market-depth-capacity-2026-10-23.json
```

驱动器还记录每市场输入 SHA、共享事件/账本规模、Sidecar SHA、进程树 RSS/CPU/I/O 和产物字节数。测试输入为同一时钟的合成 L2 盘口和单槽内置 SMA；它只测共享账户多市场深度回测，不代表供应商行情、跨市场衍生品风险、Paper/Testnet、并发服务或完整产品 profile，且不会改写 `maturity/targets.yaml`。默认矩阵较轻；64k 快照与更高市场数应在确认可用磁盘/内存后显式扩展。

2026-10-23 Ryzen 7 8700G Release 结果：2/4/8 市场 × 每市场 1k/8k 快照的六组负载均完成 21 次独立进程重复，shared result hash 与数据指纹稳定；最高 8 市场 × 8k（64k 总快照）P50/P95 为 872.288/902.233 ms，进程树峰值 RSS 92.1 MiB，处理 1,649 个事件并产生 25 条账本记录。P95 使用 21 个样本的最近邻分位数；所有输入为合成、时间戳对齐的现货 L2，不应外推到异步供应商流或完整市场 profile。原始结果绑定 Release SHA-256 `85ec860baa211722b71f577817a96e8d31f98de3fe85d3025f27f5ac98c210cc`、源码指纹 `3b20f287b0d721b900620b013aacbae08ae12a8533626f024f7f83cc5816f329` 与驱动器 SHA-256 `11c51cf7214877ddff89e5c3aac921df19bd5ac1e9ec3bb18b45e58870befa52`，不翻转完整 profile 状态。

### A 股公司行为索引同输入 A/B

`compare_ashare_action_schedule.py` 用同一份生成的 Bar、日历、公司行为、runtime、规则和 market-spec
文件交错调用旧/新 `qx-cli`，比对结果哈希并记录二进制与输入 SHA-256。`--actions` 可增加稀疏事件数；
结果衡量完整 CLI 回测耗时，不冒充隔离内核微基准，也不会更新 profile 状态：

```bash
python benchmarks/compare_ashare_action_schedule.py \
  --before target/release/qx-cli-before-action-index.exe \
  --after target/release/qx-cli.exe \
  --bars 64000 --actions 512 --repeats 9
```

2026-10-16 Ryzen 7 8700G 同输入确认样本：旧/新 P50 为 784.691/606.668 ms，P95 为 870.037/678.303 ms，
`result_hash` 同为 `71edd838dc772436`，P50 下降约 22.7%。记录在
`maturity/evidence/ashare-action-schedule-confirm-2026-10-16.json`。这是合成工作负载下的方向性端到端 A/B；
不能推导到真实全量市场数据，`ashare-research` 仍需完整场景验收后才可从 `unmeasured` 改状态。

### 多市场共享账户可见 Bar 查询 A/B

`compare_multi_market_visible_lookup.py` 用同一组三市场合成 Bar 运行 `app run-multi-market-experiment`，
市场时间段彼此错开，使首市场策略决策时另外两条行情序列含有尚不可见的未来 Bar。它交错执行优化前/后的 Release
二进制，逐轮比较组合 `result_hash`，并记录帧 SHA、二进制 SHA 和完整 CLI 耗时。默认 8k Bars、5 次重复：

```bash
python benchmarks/compare_multi_market_visible_lookup.py \
  --before target/release/qx-cli-before-visible-index.exe \
  --after target/release/qx-cli.exe \
  --bars 8000 --repeats 5
```

2026-10-16 Ryzen 7 8700G 样本中两侧 hash 均为 `cb99e0efb4f617e2`，P50 为 4546.782/4566.779 ms（优化后 +0.44%），
说明该 8k 合成场景的整条产品链没有可归因提速；策略与账本重放主导了总耗时。记录在
`maturity/evidence/multi-market-visible-lookup-ab-2026-10-16.json`。这不否定查询从 O(B) 降到 O(log B)，
但该场景尚未测出端到端收益，也不代表生产多市场 profile。

`compare_baseline.py` 只在 runner 形状一致且 workload size 重叠时比较 p50 与 median 延迟；
吞吐指标按「数值下降为回归」处理。p95 保留在报告中作观察，不作为阻断指标：此前只有
5–7 次独立进程采样，nearest-rank p95 实际接近最大值，容易被一次 runner 抢占放大。
CI 将端到端、A 股规则及 Crypto Paper E2E 提高到 21 次采样，以降低 p50/median 的采样波动。只有
`maturity/targets.yaml` 登记的性能组参与阻断；回归超过登记阈值会输出 error annotation 并非零退出，
性能提升不会导致失败。硬件的系统、架构、处理器和逻辑核数必须一致；Linux 容器可用 RAM 读数允许 1% 内波动。
比较器还要求报告中的测量源码指纹不同；源码一致时，性能差异属于测量环境噪声，不作为产品回归。
旧工件缺少指纹、首次采样、没有匹配 workload 或 runner 形状不同都会明确跳过比较并记录基线状态。
CI 优先下载主分支最近成功运行的基准，
没有可用工件时回退到当前候选分支；找到工件的运行若下载失败会阻断当前 job。

`run_profile_backtest.py` 与 `run_ashare_profile_backtest.py` 现在同时记录主进程峰值 RSS，以及可选
`psutil` 10ms 采样得到的进程树聚合 RSS/峰值进程数；未安装 `psutil` 时进程树读数明确为 `null`。
进程树采样可能漏掉短时尖峰，仍不等同于 Job Object/cgroup 硬上限或长期驻留容量保证。生产 CLI/API
没有常驻分配计数或逐阶段 p99 钩子；独立 `measure_allocations` 示例单独测量一个 `qx_app::run_backtest`
应用用例的 allocator 请求，不能替代生产链路、报告生成或服务并发分配画像。可用进程 I/O 计数器记录
工作负载读写字节，但这不是硬件 I/O 计数或阶段延迟拆分。
`maturity/targets.yaml` 仍只将真实具备 profile 对应证据的场景标为 measured。

> `tools/check_architecture.py` 的 `performance_baseline_check` 常驻核对本驱动器存在、
> 且本 README 指向它 —— 删掉任一侧即红，防止基线退化成一份「曾经写过」的文档。

### 自适应搜索容量

`run_adaptive_search_capacity.py` 通过正式 `app run-experiment` 路径测量 seeded `tpe_kde`，
对 8k/64k Bar 和 8/32/64 候选预算记录耗时、进程树 RSS、CPU/I/O、候选成功数、搜索轨迹哈希及候选结果哈希。
默认测有限网格；`--space continuous` 则测 fast_window 的有界步进整数范围。每组独立目录重复执行，要求提议轨迹与回测结果哈希一致：

```powershell
python benchmarks/run_adaptive_search_capacity.py --binary target/release/qx-cli.exe --sizes 1000,8000,64000 --budgets 8,32 --repeats 2 --out maturity/evidence/adaptive-search-capacity-2026-10-23.json
python benchmarks/run_adaptive_search_capacity.py --binary target/release/qx-cli.exe --space continuous --sizes 8000,64000 --budgets 8,32,64 --repeats 2 --out maturity/evidence/continuous-adaptive-search-capacity-2026-10-23.json
python benchmarks/run_adaptive_search_capacity.py --binary target/release/qx-cli.exe --space continuous --sizes 8000,64000 --budgets 8,32 --repeats 2 --robustness-subwindows 4 --out maturity/evidence/robust-continuous-adaptive-search-capacity-2026-10-23.json
```

2026-10-23 步进整数范围的 SMA 滚动和优化前后配对测量仍保留为历史 A/B：64k Bar 的 8/32/64 候选 P50 从 9.912/33.828/69.987 秒降至 6.265/24.277/49.374 秒，候选提议轨迹及结果哈希一致；原始和复测证据分别为 `maturity/evidence/continuous-adaptive-search-capacity-2026-10-23.json` 与 `maturity/evidence/continuous-adaptive-search-capacity-rolling-sma-2026-10-23.json`，绑定当时二进制 SHA `c6d128ce3f7ad3bc0e26a69fd38ddf41a890405df847282c082f89b5a43dd823` 和源码指纹 `d1bf8a52dbf1ed96f5f38c12b2994fdb238e8d7501a591ebe778462bc4cc3625`。这是单一合成 SMA 搜索负载的历史 A/B，不外推到其它策略或完整 profile。

2026-10-23 当前 Release 有限网格复测覆盖 1k/8k/64k × 8/32 候选、每格两次，共六格、240 次候选回测。Release SHA-256 `450f317e0102e7e745bec9a11c70a359fb7feb6447d3314db3575f4718e9c000`、源码指纹 `cdd918fc32e9826780720c6d0326ded55bf0b2e0bd6322986c97bf07abc96b8e` 下，64k P50 为 6.300/24.203 秒，进程树 RSS 峰值为 181.0/200.6 MiB；六格零失败，重复提议轨迹和候选结果哈希稳定。证据 `maturity/evidence/adaptive-search-capacity-2026-10-23.json` 绑定当前基准驱动 SHA。一次含 64 候选预算的完整矩阵在 64k 最大格触发了重复一致性断言；同 ID 的直接和 `run_measured` 两次复验均为 64/64 成功且轨迹/结果哈希一致。当前能力容量证据因此不把 64 候选格列入认证矩阵。

2026-10-23 新增稳健选参负载：以当前工作树重建 Release CLI，使用 4 个训练子窗的最差收益作为 TPE-KDE 目标，覆盖 8k/64k Bars × 8/32 候选、每格两次重复；4 组候选全部成功，重复运行的提议轨迹与候选结果哈希一致。8k/64k 下 8 候选 P50 为 1.337/6.330 秒，进程树峰值 RSS 为 30.9/197.9 MiB；32 候选 P50 为 3.508/23.932 秒，RSS 为 31.4/197.2 MiB。证据 `maturity/evidence/robust-continuous-adaptive-search-capacity-2026-10-23.json` 绑定 Release SHA-256 `c0a64bef425e4f6ecb96283dff3f203d8d50a37d97992162f42227929fa6c0ac` 与源码指纹 `50fe4952e06518ed470718f3f55b2b6c33a3847fc7a41683dcba5b0d8d944d6a`。该测量仅覆盖单标的合成 crypto-spot SMA、步进整数范围和稳健选参；不代表早停剪枝、并行搜索或任一完整产品 profile 容量。

费用邻域门单独使用 `run_cost_neighborhood_capacity.py` 测量：每个搜索候选外还会重放所选候选的费用压力运行及配置半径内的全部单维邻居。脚本要求每个邻居都被计算且基线/压力结果哈希跨重复稳定；示例：

```powershell
python benchmarks/run_cost_neighborhood_capacity.py --binary target/release/qx-cli.exe --sizes 8000,64000 --budget 8 --repeats 2 --out maturity/evidence/adaptive-search-cost-neighborhood-capacity-2026-10-23.json
```

该证据只测合成单标的费用压力邻域重放，不计为连续浮点参数或完整产品 profile 的容量结果。

连续整数范围的原始测量暴露出 SMA 策略每根 Bar 都复制并重扫窗口的热点。`SmaCross` 改为延迟初始化滚动定点和，并保持预热时点、左到右溢出检查及信号语义；优化前后使用相同输入和 seed 重复测量，提议轨迹与每个候选的 `result_hash` 完全相同。最新 Release 下 64k Bar 的 8/32/64 候选 P50 从原始 9.912/33.828/69.987 秒降至 6.265/24.277/49.374 秒（约 1.58×/1.39×/1.42×）；进程树峰值 RSS 基线约 193.9–200.0 MiB，当前为 193.6/197.5/201.1 MiB。优化前证据保存在 `maturity/evidence/continuous-adaptive-search-capacity-2026-10-23.json`，当前 Release SHA-256 `c6d128ce3f7ad3bc0e26a69fd38ddf41a890405df847282c082f89b5a43dd823`、源码指纹 `d1bf8a52dbf1ed96f5f38c12b2994fdb238e8d7501a591ebe778462bc4cc3625` 的复测在 `maturity/evidence/continuous-adaptive-search-capacity-rolling-sma-2026-10-23.json`。该单一 SMA 工作负载的提升不能外推到其它策略或完整产品 profile。

### 研究报告生成容量

`run_report_capacity.py` 在同一份已回放复核的合成回测摘要上，分别测量 JSON、HTML 和 XLSX 报告进程，
并覆盖 1k/8k/64k Bar。运行示例：

```powershell
python benchmarks/run_report_capacity.py --binary target/release/qx-cli.exe --sizes 1000,8000,64000 --repeats 3 --out maturity/evidence/report-generation-capacity-2026-10-23.json
```

证据记录报告耗时、主进程与进程树 RSS、进程数、可用的 CPU/I/O 计数、报告字节数与哈希；同一规模的三种格式共用同一回测结果。该工作负载是合成 crypto-spot 与单一内置策略，只描述报告生成，不改变完整市场 profile 状态，也未覆盖多市场/深度报告、PDF、参数搜索内部并发或 Paper/live 生命周期。

当前 Release 的 1k/8k/64k JSON P50 为 310.280/334.960/392.620 ms，HTML 为 335.020/333.660/432.340 ms，XLSX 为 314.470/339.250/492.600 ms；证据绑定 Release SHA-256 `f555cbae14d56e4d0a45f31aff5339bd49d44993bb2e4f636632c5bdef074619` 与源码指纹 `4d5c8285d90e83952cb3a518857abc997805184f0723675adb1921b118fe216a`。更新覆盖了已复核诊断附件 XLSX 路径，同时不提升完整 profile 状态。

诊断附件导出也可单独测量：传入 `--with-diagnostics` 后，脚本会为每档回测摘要生成绑定策略 ID、标的、已复核数据指纹和终点时间的有界合成诊断报告，额外测 `html-diagnostics` 与 `xlsx-diagnostics` 两种单进程导出。诊断 JSON 的字节数计入报告输入，产物照常记录哈希、主进程/进程树 RSS、CPU 和 I/O。它测量的是已生成诊断附件的报告呈现成本，不包含诊断策略重放时间，也不是完整产品 profile：

```bash
python benchmarks/run_report_capacity.py --binary target/release/qx-cli.exe --sizes 1000,8000,64000 --repeats 3 --with-diagnostics --out maturity/evidence/report-diagnostics-generation-capacity-2026-10-23.json
```

当前 Release 的 64k Bar / 200 检查样本，HTML 诊断报告 P50/P95 为 435.797/441.781 ms，XLSX 为 510.086/528.316 ms；最大进程树 RSS 分别为 30.6/46.0 MiB，三次输出哈希均唯一稳定。原始数据绑定 Release SHA-256 `f555cbae14d56e4d0a45f31aff5339bd49d44993bb2e4f636632c5bdef074619`，证据为 `maturity/evidence/report-diagnostics-generation-capacity-2026-10-23.json`。它测报告渲染而非策略重放，不改变完整 profile 状态。

多市场 Bar 策略诊断生成与 HTML 报告另由 `run_multi_market_diagnostics_capacity.py` 测量。每个 workload 先实际创建共享账户多市场回测并保存经复核摘要，再重复执行 `strategy diagnose-multi-bar --html`；计时包含输入身份校验、回放复核、策略偏差诊断和 JSON/HTML 输出，回测准备时间单独记录。

```powershell
python benchmarks/run_multi_market_diagnostics_capacity.py --binary target/release/qx-cli.exe --sizes 1000,8000,64000 --markets 3,4 --repeats 3 --out maturity/evidence/multi-market-diagnostics-capacity-2026-10-23.json
```

当前 Release（SHA-256 `90011a5148a714d1e136830d8128f29ba2f92f284b633e7fb8037767ed1fd6ea`，源码指纹 `e0fc25d339b4c9ea5c0b8ccc0dd293d5dcf1f82a1aeed127fe84e33a4e11f1dc`，驱动脚本 SHA-256 `1df85d0ca40a422a9d96e7f73d8019314195b4ad1afa7629b533ddb591f5f344`）下，3/4/8 市场 × 1k/8k/64k Bar 的诊断 P50/P95 分别为：1k 档 524/847、535/547、725/729 ms；8k 档 1,518/1,569、1,876/1,881、3,216/3,219 ms；64k 档 8,208/8,242、10,637/10,663、20,852/21,122 ms（每档按 3/4/8 市场顺序）。进程树峰值 RSS 分别为 21.2/32.0/53.7、91.2/117.6/226.0、612.4/814.5/1,622.0 MiB。每格三次诊断 JSON 与 HTML 哈希稳定；输入为合成现货 SMA，诊断现金按市场数等额预留且持仓为空，PIT 仍未知。8 市场 × 64k 的总策略-Bar 事件数为 512k，峰值 RSS 约 1.58 GiB。该证据测量诊断与报告链，不是九档产品 profile，也不代表真实供应商容量。

多轮交互对照报告另由 `run_interactive_comparison_capacity.py` 测量：先对同一冻结 BarFrame 跑不同 SMA 窗口的 2/4/8 份回测摘要，再单独采样 `report --against --html`。输出包含权益曲线数据、浏览器筛选和排序；报告端测量和浏览器初始渲染/鼠标交互分开记录。

```powershell
python benchmarks/run_interactive_comparison_capacity.py --binary target/release/qx-cli.exe --sizes 64000 --run-counts 2,4,8 --repeats 3 --measure-browser --hover-events 100 --out maturity/evidence/interactive-comparison-capacity-2026-10-23.json
```

当前 Release SHA-256 `f647bb853b355eb04d37ff58c3b05066fc1b2247fc1d94f89d3b3bb54d99f152` 下，64k Bar、2/4/8 轮交互报告生成 P50/P95 为 492.300/507.907、679.644/703.970、1046.947/1049.175 ms；进程树 RSS 最大为 42.4/45.8/52.5 MiB，输出哈希稳定。Chromium 到曲线可用 P50/P95 为 81.201/90.477、85.940/86.961、99.364/101.908 ms；每个样本执行过滤和鼠标 tooltip 检查，并测 100 次指针事件，处理 P95 为 0.1/0.2/0.2 ms。页面没有异常或外部请求。单轮最多 2,000 点，因此 8 轮曲线最多 16,000 点。证据仍未覆盖跨拓扑事件时间线加载、多市场/深度报告或完整 profile。

`--measure-browser` 需要本机已安装 Playwright Python 绑定及其 Chromium 浏览器；不传此开关时只测报告生成，不会把浏览器指标填成已测。

训练窗回撤预算早停有独立的配对容量模式：

```powershell
python benchmarks/run_adaptive_search_capacity.py --binary target/release/qx-cli.exe --space continuous --sizes 8000,64000 --budgets 8,32 --repeats 2 --early-pruning-drawdown-bps 1 --early-pruning-min-training-bars 64 --out maturity/evidence/adaptive-search-pruning-capacity-2026-10-23.json
```

驱动器在相同候选与 seed 下交错运行完整训练和早停版本；合成输入先涨后跌，早停版本必须至少剪枝一项才记为有效样本。报告中 `pruned_bars_saved`、`pruning_trace_sha256`、候选结果/提议轨迹哈希和进程树资源只描述此剪枝负载，不说明 OOS 选参质量或完整 profile 容量。

2026-10-23 当前 Release（SHA-256 `450f317e0102e7e745bec9a11c70a359fb7feb6447d3314db3575f4718e9c000`）的 64k × 32 候选样本中，剪枝 P50 为 3,593.355 ms，完整基线为 29,598.086 ms，配对比值 8.2369×；进程树 RSS 峰值约 69.0 MiB，32 个候选共节省 1,332,440 个训练 Bar。其余三格及 Release/source fingerprint 以原始证据为准。所有候选在 1 bp 合成回撤压力下都被剪枝，因此该数据不能用来判断搜索收益或样本外质量。
