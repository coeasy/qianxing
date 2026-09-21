# Changelog

## Unreleased — V11 Q1a 第一批：示例真成交、深度链参数可达且看得见（2026-09-22）

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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
- `docs/牵星完整架构方案-V1.md` §2.3 加现状对照：核对后确认该节没说谎（它把撮合适配器放在"可替换"
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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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
  本轮踩到一次并修在测试侧（`crates/qx-cli/src/tests/backtest_entries.rs:435` 先 `.replace('\r', "")`
  再匹配帮助文本），未动 git 配置（属用户级设置，本轮不改）。


## Unreleased — V11 Q0b：命令面旗标诚实性与回测准入分区（2026-09-21）

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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

方案、逐阶段验收口径与实测数字见 [docs/自研量化框架重构方案-V10.md](docs/自研量化框架重构方案-V10.md)
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

方案与逐阶段判定见 [docs/自研量化框架重构方案-V9.md](docs/自研量化框架重构方案-V9.md) §8。

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
