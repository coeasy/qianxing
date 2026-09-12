# C++ 策略接入

项目提供两种 C++ 接入方式：

1. `include/qianxing_strategy.h` 是稳定的 C ABI，适用于宿主进程内嵌或动态库插件。策略只产生 `OrderIntent`，不接触凭证、账户账本和交易所连接。
2. `examples/jsonl_strategy.cpp` 是独立进程示例，使用与 Python 相同的版本化 JSONL 协议。编译后在运行时配置 `external_executable`、`external_args` 和可选的 `external_env`，即可复用 Strategy Worker 的超时、崩溃隔离、RiskGate、OMS、审计和重试边界。

独立进程要求：JSONL 模式每行读一个 `StrategyContractInput`，每行输出 `{"ok":true,"output":...}`；传入 `--protocol framed_json` 时使用 QXSF 版本化二进制分帧（24 字节头、长度上限、序号、CRC32）；传入 `--protocol shared_memory_json` 时通过 `--input-ring`/`--output-ring` 使用 Rust 创建的 QXRB 双向 SPSC mmap ring。标准输出只能写协议，日志写标准错误。`nlohmann/json` 仅用于示例，不是框架运行时依赖。

`include/qianxing_ring.hpp` 提供 C++17 的跨平台文件映射和 QXRB 读写实现；Ring 仍要求严格单生产者/单消费者，策略进程只处理 QXSF 帧，不得直接访问凭证、Venue 或 Ledger。

本地构建示例：`cmake -S cpp -B build/cpp -DQX_BUILD_JSONL_EXAMPLE=ON && cmake --build build/cpp --config Release`。CI 会在 Ubuntu 安装 `nlohmann-json3-dev` 后编译 C ABI 动态库和独立 Worker；Windows/macOS 的编译器矩阵仍需接入发布环境后验收。

`qianxing_ring_smoke` 不依赖 nlohmann/json，专门验证 QXRB 文件头、容量背压、顺序和跨映射读写。

Rust 宿主绑定位于 `crates/qx-strategy/src/c_api.rs`，会校验 ABI 版本、复制插件返回的 intent，并在交给 Risk/OMS 前执行 schema、身份、数量和价格校验。`DynamicCAbiStrategy::load_verified(path, config_json, &DynamicCAbiLoadPolicy)` 会在动态链接前校验普通文件、大小上限和受信任 SHA-256 白名单；也可配置对完整动态库字节的 detached Ed25519 签名校验。加载后仍保证先销毁策略句柄、再卸载动态库。摘要/签名校验不等同于沙箱，信任根、轮换和不受信任代码隔离仍必须由部署系统负责。

受信任 C ABI 动态库可以直接接入 strategy-backtest：配置 strategy.c_abi_library、对应的 strategy.c_abi_sha256，可选 c_abi_max_library_bytes、c_abi_ed25519_public_key 和 c_abi_ed25519_signature。它与 Python/独立进程策略共用 Bar 因果回测、OrderIntent、Risk 和 OMS；未通过摘要/签名校验的库不会被加载。不受信任 C++ 代码仍应使用独立 Worker。
