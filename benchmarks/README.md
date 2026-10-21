# Qianxing Benchmarks

性能基线与复现入口。**本目录不是免责声明**：`run_baseline.py` 是真会跑的驱动器，读数落
`benchmarks/results/baseline.json`。

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
| 回测吞吐随 N 的变化（N=1k/8k/64k） | 内存峰值与分配行为（需 allocator 钩子） |
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
整轮 wall-clock p50/p95；它不拆解单笔订单延迟，也不覆盖真实 funding provider 或 8 小时结算调度。
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
拒单数、Release 二进制 SHA-256、重复运行 hash 和 bars/s。它比只绑定 market spec 的通用代理更接近 A 股回测路径，
但合成日历只跳过周末，不含交易所节假日/临时休市，且没有逐项验证 T+1、整手、涨跌停、停牌和
除权除息分支，所以 `ashare-research` 仍须保持 `unmeasured`，直到完整 profile 的场景验收证据齐备。
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

```bash
python benchmarks/run_ashare_profile_backtest.py
```

本地证据写入 `maturity/evidence/ashare-rules-backtest.json`；CI 将同机同规模 p50/p95 纳入 ±20%
非阻塞趋势比较，但该 CI 读数不改变 `maturity/targets.yaml` 的 profile 状态。

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
首次采样、没有匹配 workload 或 runner 形状不同仍明确跳过比较。CI 优先下载主分支最近成功运行的基准，
没有可用工件时回退到当前候选分支；找到工件的运行若下载失败会阻断当前 job。

当前 Rust target 不采集内存峰值、分配次数或逐阶段 p99；这些指标没有测量钩子，不在报告里
伪造。`maturity/targets.yaml` 仍只将真实具备 profile 对应证据的场景标为 measured。

> `tools/check_architecture.py` 的 `performance_baseline_check` 常驻核对本驱动器存在、
> 且本 README 指向它 —— 删掉任一侧即红，防止基线退化成一份「曾经写过」的文档。
