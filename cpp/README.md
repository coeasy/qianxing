# C++ 策略接入

项目提供三种 C++ 接入方式：

1. `include/qianxing_strategy.h` 是稳定的 C ABI，适用于宿主进程内嵌或动态库插件。策略只产生 `OrderIntent`，不接触凭证、账户账本和交易所连接。
2. `examples/jsonl_strategy.cpp` 是独立进程示例，使用与 Python 相同的版本化 JSONL 协议。编译后在运行时配置 `external_executable`、`external_args` 和可选的 `external_env`，即可复用 Strategy Worker 的超时、崩溃隔离、RiskGate、OMS、审计和重试边界。

持久 Strategy Worker 的外部进程需要配置 `strategy_artifact_sha256` 固定可执行文件身份，并在每次请求中读取 `strategy_checkpoint`、在响应中回传更新后的 checkpoint；无状态策略也回传空 `strategy_state`。C++ JSONL 示例包含这一往返。单次本地进程调用仍可处理没有 checkpoint 的旧版输入。

`include/qianxing_app.hpp` 提供共享应用 API 的轻量 C++17 客户端，可调用数据校验、Bar/多腿 Bar/Tick/OrderBook、多账户批量回测与 OKX/Binance/Bitget 联合账户回测、单账户活动查询、产物复核、运行对比、参数实验，以及 Paper/Live 控制命令。联合账户回测会保留各账户独立账簿并汇总结果，不跨账户净额；Tick/OrderBook 与多腿 JSON 可携带标准化产品规格和适用的订单策略。客户端把版本化请求交给相同的 Rust 路由，不在 C++ 重做撮合、风险或账簿。HTTP transport 由集成方实现，并负责 TLS、鉴权、超时和连接复用；Live 控制要求服务启用 Operator 授权。

`AppClient::get_account_activity(account_id, venue_id, after, before, limit)` 返回单账户活动报告 JSON。账户与 venue 必须同时给出；`after`/`before` 是互斥排他游标；事件页覆盖该账户保留的投影日志，不表示交易所完整历史。C++ client 会对 query 参数做百分号编码，并在发送前拒绝两种游标同时使用或越界的 `limit`。

长任务可调用 `start_backtest`、`start_depth_backtest`、`start_depth_experiment`、`start_multi_account_backtest`、`start_multi_account_joint_backtest`、`start_multi_account_depth_backtest`、`start_multi_market_backtest`、`start_multi_leg_backtest`、`start_experiment`、`start_multi_leg_experiment` 或 `start_multi_market_experiment`，再以 `run_status(run_id)` 轮询结果，并用 `cancel_run(run_id)` 请求协作取消。它们复用 Rust Bar/Tick/OrderBook 内核；终态结果由服务持久化并跨重启保留，重启时仍未完成的任务标记为失败；注册表最多保留 1024 项，同时最多运行 64 个有持久化观察器的任务，满额时服务会在启动前拒绝新任务。run_id 只接受字母、数字、`-`、`_`、`.`。`run_multi_market_experiment` 与 `start_multi_market_experiment` 提交由 Rust 展开的有界多市场策略参数网格。

`run_depth_backtest` / `start_depth_backtest` 接收同一版本化 JSON；L2 请求可包含 `market_making`（`spread_bps`、`quote_qty_raw`、`max_inventory_raw`、`tick_size_raw`、`inventory_skew_bps`），由 Rust 撮合器执行双边报价与撤单。C++ client 只传输该规格，不在本地实现交易规则。

独立进程要求：JSONL 模式每行读一个 `StrategyContractInput`，每行输出 `{"ok":true,"output":...}`；传入 `--protocol framed_json` 时使用 QXSF 版本化二进制分帧（24 字节头、长度上限、序号、CRC32）；传入 `--protocol shared_memory_json` 时通过 `--input-ring`/`--output-ring` 使用 Rust 创建的 QXRB 双向 SPSC mmap ring；这条传输没有 stdin 可关，必须同时传 `--parent-pid <驱动进程 pid>`，worker 每秒探一次该进程是否还在，父进程消失就以退出码 0 收摊（不传则不检查，与 Python worker 同口径）。标准输出只能写协议，日志写标准错误。`nlohmann/json` 仅用于示例，不是框架运行时依赖。

`include/qianxing_ring.hpp` 提供 C++17 的跨平台文件映射和 QXRB 读写实现；Ring 仍要求严格单生产者/单消费者，策略进程只处理 QXSF 帧，不得直接访问凭证、Venue 或 Ledger。

本地构建示例：`cmake -S cpp -B build/cpp -DQX_BUILD_JSONL_EXAMPLE=ON && cmake --build build/cpp --config Release`。CI 会在 Ubuntu 安装 `nlohmann-json3-dev` 后编译 C ABI 动态库和独立 Worker；Windows/macOS 的编译器矩阵仍需接入发布环境后验收。

C ABI v2 的 `qx_order_intent` 与旧 `qx_strategy_vtable` 布局保持稳定。订单状态 callback 可继续导出 `qx_strategy_on_order_status_v1` 接收版本化 JSON；新插件可导出 `qx_strategy_on_order_status_v2` 接收 typed `qx_strategy_order_status_update_v2`。成交策略回调可选导出 `qx_strategy_on_fill_update_v2` 接收 `StrategyFillUpdateV2` JSON，其中包含 schema v2、成交 `ts` 和 EventLog `event_seq`，用于识别重复成交投递。账户与持仓回调可选导出 `qx_strategy_on_account_update_v1`，接收 `StrategyAccountUpdateV1` 的完整余额或持仓快照及 EventLog `event_seq`；快照只读，不直接修改 Rust ledger。Rust 动态宿主分别探测这些符号；缺少 fill V2 时将更新投影到旧 vtable fill callback，缺少订单状态或账户回调时保留旧插件行为。更新中的字符串和 exact amount 指针由宿主持有，只在 callback 返回前有效；数量、价格以带 scale 的 128-bit coefficient 表达。需要显式下发衍生品订单政策的插件可额外导出 `qx_strategy_get_order_policy_v1`；字段格式和默认行为见 [`c-strategy-order-policy-v1.json`](../schemas/c-strategy-order-policy-v1.json)。不导出可选符号的旧插件继续使用原有默认行为。

策略可选导出 `qx_strategy_history_requirements_v1` 声明每标的 Bar 预热数，并以 `qx_strategy_on_warmup_complete_v1` 接收版本化就绪事件；未声明时默认 0 根预热。两个符号由 Rust 动态宿主独立探测，不改变既有 vtable；缺少就绪 callback 不影响策略运行。Rust 策略引擎按声明应用 warmup gate，并在首个可决策 Bar hook 前发送就绪通知。共享 fixture 固定 `on_init → on_start → 预热 Bar → warmup_complete → 首个可决策 Bar → on_stop` 的调用顺序；C++ 动态插件集成用例验证结构版本、标的和观测数量。旧插件缺少这些符号时维持零预热行为。

策略可选导出 `qx_strategy_on_error_v1` 接收 `StrategyErrorV1` JSON（`schemas/strategy-error-v1.json`）。该符号独立探测，不改变 vtable；错误观察器返回失败时，Rust 宿主仍保留原策略 callback 错误。

精确行情通过可选导出符号接入，不改变旧 `qx_strategy_vtable` 和 `qx_market_event` 布局：`qx_strategy_on_market_event_v2` 承载 exact OHLCV，`qx_strategy_on_market_event_v3` 在 v2 envelope 上再附加与原始 bids/asks 同序、同长度的 exact price/quantity sidecar。exact decimal 使用 `coefficient / 10^scale`，不会经过浮点；旧 v1/v2 插件遇到带 exact OrderBook 的事件会由 Rust 宿主明确拒绝，不会静默按 1e-9 raw 投影继续运行。C++ 示例动态库实现 v2/v3；宿主优先走 v3，再走 v2，raw-only 事件仍兼容旧 callback。标准 Timer 事件使用 `QX_MARKET_TIMER`、`timer_name` 与 `ts`，C++ 示例动态库也处理共享 Timer 和精确 OrderBook fixture，并在 Rust 动态宿主测试中验证回调结果。

作为 C++ SDK 消费时，可安装头文件与 CMake 导出目标：

```bash
cmake -S cpp -B build/cpp -DCMAKE_BUILD_TYPE=Release
cmake --build build/cpp --config Release
cmake --install build/cpp --prefix ./stage/qianxing
```

下游项目可用 `find_package(QianxingStrategySDK CONFIG REQUIRED)` 和
`target_link_libraries(app PRIVATE Qianxing::StrategySDK)`。发布压缩包包含相同的头文件与
CMake package 元数据，包含策略插件接口与应用 API 客户端；它不包含牵星交易运行时或实盘权限。

`qianxing_ring_smoke` 不依赖 nlohmann/json，专门验证 QXRB 文件头、容量背压、顺序和跨映射读写。

Rust 宿主绑定位于 `crates/qx-strategy/src/c_api.rs`，会校验 ABI 版本、复制插件返回的 intent，并在交给 Risk/OMS 前执行 schema、身份、数量和价格校验。`DynamicCAbiStrategy::load_verified(path, config_json, &DynamicCAbiLoadPolicy)` 会在动态链接前校验普通文件、大小上限和受信任 SHA-256 白名单；也可配置对完整动态库字节的 detached Ed25519 签名校验。加载后仍保证先销毁策略句柄、再卸载动态库。摘要/签名校验不等同于沙箱，信任根、轮换和不受信任代码隔离仍必须由部署系统负责。

该头文件与 `c_api.rs` 里的 `#[repr(C)]` 定义仍是手写镜像，没有编译期耦合，修改一侧必须同步另一侧。`tools/check_architecture.py` 的 `c_abi_header_check()` 逐字段比对两侧类型、字段顺序与类型，并核对 schema 版本、状态码及可选 callback 符号；`cpp/tests/strategy_update_contract.cpp` 由 CMake/CTest 编译运行。Rust 单测覆盖 order-status typed V2、fill JSON V2/旧 fill 回退、精确行情 v2 以及旧 v1 拒绝降级；C++ 示例动态库由 Rust 宿主实际加载，端到端检查精确系数/scale 和 EventLog `event_seq`。本机完成 C ABI 合同程序 CTest 和动态插件 Rust↔C++ 验收。

受信任 C ABI 动态库可以直接接入 strategy-backtest：配置 strategy.c_abi_library、对应的 strategy.c_abi_sha256，可选 c_abi_max_library_bytes、c_abi_ed25519_public_key 和 c_abi_ed25519_signature。它与 Python/独立进程策略共用 Bar 因果回测、OrderIntent、Risk 和 OMS；未通过摘要/签名校验的库不会被加载。不受信任 C++ 代码仍应使用独立 Worker。

用于持久 Strategy Worker 的有状态 C++ 插件还应成对导出 `qx_strategy_checkpoint_snapshot_v1` 和 `qx_strategy_checkpoint_restore_v1`。快照/恢复只交换最多 64 KiB 的 JSON object；宿主会将状态与插件 SHA-256、策略身份及 EventLog 游标一起保存。仅实现一个导出会在加载时被拒绝；旧插件仍可用于无状态策略，但不能安全恢复持久内存状态。`diagnostic_state` 是只读观测接口，不替代 checkpoint。
