# 项目与文档指南

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
[`docs/archive/`](archive/README.md)，2026-10-09 又把七份钉在旧提交基线上的规划与发布审计稿
（2026-10-03 的四份——架构审计、量化回测 Web 平台、桌面/Web 客户端三轮终版、tauron 一体化桌面客户端——加 2026-10-04 的终版合订本，
2026-10-08 的综合路线图与 R22 发布审计）连同本 README 的**历史实测读数**一起移入同一目录。归档不等于作废：
那些文件里"当时成立"的记录原样保留，只是不再当作现状来读，索引与各自的基线写在
[`docs/archive/README.md`](archive/README.md) 里。

| 你要做的事 | 读这份 |
|---|---|
| 装上、跑通第一轮回测与体检 | 本 README 的「安装」与「快速开始」 |
| 看结果：离线报告 / 静态页面 / 同源 BFF 控制台 | 本 README「看结果」+ [deploy/README.md](../deploy/README.md) 的控制台两节 |
| 上手细节：按场景初始化项目、七条回测入口各自的落点与拒收口径、四份产物字段、读模型里 `null` 与 `0` 的分别、实盘发布前检查 | [工业化易用性收口指南](工业化易用性收口指南-V1.md) |
| 部署与运维：运行时配置、worker 拓扑与监督停机、CCXT/Binance/A 股接入、SubmitOrder、存储后端、HTTP 读面与 WS 事件流、模板契约面 | [deploy/README.md](../deploy/README.md) |
| 机器可读的能力证据分级：每条能力的实现 / 代码测试 / 沙盒 / 生产批准四档 + 证据路径 + 缺口 | [maturity/capabilities.yaml](../maturity/capabilities.yaml) |
| 现行审计结论、不合理清单与下一轮怎么排 | [自研量化框架审计与重构方案 V13](自研量化框架审计与重构方案-V13.md) |
| 架构事实、工业级差距的工作包与逐轮登记（WP-* 与 §17 执行记录） | [架构设计与工业级优化改进方案](qianxing-架构设计与工业级优化改进方案-2026-10-06.md) |
| 项目结构、模块划分与同类开源项目的对照 | [项目结构与 GitHub 竞品对比](qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md) |
| 逐轮变更、验收数字与"本轮没修什么" | [CHANGELOG.md](../CHANGELOG.md) |
| CCXT worker 契约、凭据隔离、失败即闭的归约规则 | [CCXT 多交易所接入与策略运行方案](archive/ccxt多交易所接入与策略运行方案-v1.md)（已归档） |
| A 股提供方、代码归一化、公司行为与八条交易制度 | [A 股数据源接入与快速选股回测方案](archive/a股数据源接入与快速选股回测方案-v1.md)（已归档） |
| Rust/C++/Python 策略契约与共享内存传输的边界 | [工业级多语言策略与高性能交易方案](archive/工业级多语言策略与高性能交易方案-v1.md)（已归档） |
| 现货/杠杆/永续/交割的撮合与记账统一口径 | [虚拟交易与衍生品统一模型](archive/虚拟交易与衍生品统一模型-v1.md)（已归档） |
| 有凭据时怎么按五段阶梯验收，`sandbox_tested` 何时才允许翻转 | [外部链路验收执行方案](外部链路验收执行方案-V1.md) |
| 跑性能基线、看回测轨的耗时口径 | [benchmarks/README.md](../benchmarks/README.md) |
| 想知道某一轮**当时**修了什么、怎么证的 | [docs/archive/](archive/README.md)（V11 / V12 / 2026-10 那批规划与发布审计稿，数字为当轮快照） |

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

