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

## 已落地：`run_baseline.py`（M0 · 关闭 G6「无性能基线」）

无新依赖（纯标准库），驱动**已构建的 qx-cli 公开命令面**：

```bash
# 先构建
cargo build --release -p qx-cli
# 再跑（缺省 N=1000,8000,32000，重复 7 次；p50/p95 按墙钟取）
python benchmarks/run_baseline.py --binary target/release/qx-cli.exe
# 机器可读
python benchmarks/run_baseline.py --json
```

它测什么、不测什么（诚实口径，别把没测的读成测了）：

| 覆盖 | 不覆盖 |
|---|---|
| 端到端墙钟：读帧 → 校验 → 逐事件回测 → 印摘要 | 内核内部 append / refresh / replay / read-model / recovery 的**逐段** p50/p95 |
| 回测吞吐随 N 的变化（N=1k/8k/32k） | 内存峰值与分配行为（需 allocator 钩子） |
| 「同样输入必得同样结果」：`result_hash` 跨重复稳定 | 撮合延迟（需 L1/L2 深度帧）与语言桥开销 |

未覆盖的三项需要 **in-process 钩子（Rust bench target）**，按「零新依赖」原则本轮未落；
缺口登记在 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` §4.3 G6 与 §6.6。

> `tools/check_architecture.py` 的 `performance_baseline_check` 常驻核对本驱动器存在、
> 且本 README 指向它 —— 删掉任一侧即红，防止基线退化成一份「曾经写过」的文档。
