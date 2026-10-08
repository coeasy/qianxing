//! # contract-tests — 跨 crate 契约用例宿主
//!
//! 本 crate **不含生产代码**，只提供一个落点，让「必须同时看到两个 crate 才能断言」的契约
//! 用例不再以 `[dev-dependencies]` 的形式挂在生产 crate 上。
//!
//! 背景（方案 §5.4 P1-12 / §8 WP-22）：此前两条 dev 边正好反向压在正常边上，构成包图上的环——
//!
//! - `qx-execution --dev--> qx-runtime`，而 `qx-runtime --normal--> qx-execution`；
//! - `qx-risk --dev--> qx-zhenlu`，而 `qx-zhenlu --normal--> qx-risk`。
//!
//! Cargo 允许 dev 环，但它们让「谁依赖谁」的读法出现歧义：`cargo tree` 的默认视图看不见这两条边，
//! 只有 `--edges dev` 才现形，于是 `qx-execution` / `qx-risk` 的依赖面看起来比实际窄、而实际
//! 反向边又真实存在。把这几份用例搬到本 crate 之后两条环消失——本 crate 是叶子，没有任何 crate
//! 依赖它，`[dev-dependencies]` 再宽也压不到别人身上。
//!
//! 这里放的都是**契约**用例：断言两个 crate 之间的边界，而不是某一个 crate 的内部行为。
//!
//! - `tests/paper_accounting.rs` —— 纸面账本归约与运行时流水线共用同一份事实；
//! - `tests/reconcile_port_contract.rs` —— 对账端口在 `EventLog` 上的读回契约；
//! - `tests/recovery_and_replay.rs` —— 崩溃恢复后重放得到同一状态；
//! - `tests/venue_report_contract/` —— 三家 venue（Paper / CCXT / Binance）共用同一份回报契约；
//! - `tests/risk_parity.rs` —— 风控三条路径（回测 / paper / live）判定一致。
//!
//! 门禁 `dev_dependency_cycle_check` 钉住「两条环不许回来」与「这几份用例仍在本 crate 里」，
//! 所以搬家不会变成丢用例。
