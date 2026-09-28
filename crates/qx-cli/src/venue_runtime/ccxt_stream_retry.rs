//! CCXT 子进程回话的空闲判据。
//!
//! 重连预算不住在这里：行情链与用户流的预算都是 `ccxt_market_worker.rs` 的
//! `CCXT_DEAD_CYCLE_BUDGET` / `ccxt_respawn_delay` / `CCXT_RESPAWN_STRIKE_BUDGET`
//! 一族（V11 K1、O5），这里只留跨语言的那一枚 `idle` 键。

/// 子进程对 `wait_ms` 窗口的回话是否表示"接得上话、这一窗没有订单事件"。
///
/// 单独成点是因为这个键跨语言（Python 侧在 `python/qianxing_ccxt/worker.py` 写
/// `event["idle"] = True`）。读错键名的两种失败方式在日志里看不出区别：把空闲
/// 当断链会吃掉重连预算，把空闲当交付会替坏链路复位预算——两者都会让当天没有
/// 成交的账户在几个窗口后被具名放弃。
pub(crate) fn ccxt_watch_reply_is_idle(event: &serde_json::Value) -> bool {
    event.get("idle").and_then(serde_json::Value::as_bool) == Some(true)
}
