# 多语言策略示例

牵星的策略层负责生成目标仓位或订单意图；成交、费用、风险检查和账本仍由同一 Rust 内核执行。
三个示例都使用整数定点数，避免语言间浮点舍入造成回测差异。

| 语言 / 接口 | 示例 | 策略实例 | 使用方式 |
|---|---|---|---|
| Rust 原生 SDK | `crates/qx-strategy/src/lib.rs` 的 `SmaCrossStrategy` | SMA 金叉 | `cargo run -p qx-cli -- backtest builtin sma_cross deploy/qianxing.bar-frame.example.json` |
| Python Worker | `python/examples/strategy_instances.py` | 动量、SMA、均值回归 | 配置 `strategy.external_executable` 指向 Python，并将该文件导出为 Worker 的 `on_event` 入口 |
| C++17 C ABI | `cpp/examples/momentum_strategy.cpp` | Bar 事件策略插件骨架 | `cmake -S cpp -B build/cpp -DCMAKE_BUILD_TYPE=Release && cmake --build build/cpp --config Release` |

Python 示例通过 `StrategyContractInput` 接收不可变行情帧，返回带策略身份、时间戳和定点目标仓位的决策。
可以在 Worker 启动模块中将 `on_event = momentum`、`sma_cross` 或 `mean_reversion` 绑定为入口；
参数需在部署时固定，作为运行配置的一部分记录。示例没有调用交易所，也不绕过 Rust RiskGate / OMS。

C++ 示例展示 C ABI 生命周期和 Bar 回调契约。C ABI 当前示例只返回空意图，适合验证插件加载边界；
需要实际下单时应在 `decision->intents` 中填充经 SDK 分配器创建的订单意图，并实现配对释放回调。

统一回测产物可用 `qx-cli compare-runs` 与 `qx-cli report` 对照策略结果；相同冻结 BarFrame、费用配置、
仓位单位和运行参数是跨语言比较的前提。策略样例用于演示接口与链路，不代表投资建议或收益承诺。
