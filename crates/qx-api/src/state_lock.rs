//! 状态锁的收口：锁中毒不再是 panic，而是可返回的错误（V13 R26 / §6.D3）。
//!
//! `ApiState` 的锁此前在每个取用处各写一次 `.lock().expect("api state mutex poisoned")`。
//! 危害不在"这一次请求失败"，而在**之后每一个**请求：任一持有者 panic 之后互斥量永久中毒，
//! 此后每个请求线程都在同一次 `.lock()` 上 panic——一次中毒把整个读面变成永久断连，而运维侧
//! 看不到任何 5xx，只看到"进程还在、连接全断"。
//!
//! 这里把取锁收成**一个可失败入口**，由调用方按自己那条面的口径决定回什么码：
//!
//! * HTTP 读面 / 写面：回 `503 api_state_lock_poisoned`（`error_json` 与状态码由调用点给）。
//! * 返回 `Result<_, String>` 的内部读：直接 `?` 上抛，由既有的 503 收口接住。
//! * `/ready`：`projection_readiness` 把中毒当成"未就绪原因"，于是探针回 503。
//!
//! 口径：**不在这一层兜底**。中毒是"这份进程内的读模型已不可信"的确定结论，兜一个空集合
//! 或旧副本等于把"读不出来"念成"没有数据"——与全仓 `checked_*` / fail-closed 的取向相反。

use crate::{error_json, ApiResponse, ApiState};
use std::sync::{Mutex, MutexGuard};

/// 锁中毒时对外公布的原因串（`error_json` 会把它印进响应体，与 `/ready` 的原因同一格）。
pub(crate) const API_STATE_LOCK_POISONED: &str = "api_state_lock_poisoned";

/// 取状态锁；中毒时返回 `Err(API_STATE_LOCK_POISONED)`，不 panic。
pub(crate) fn lock_state(state: &Mutex<ApiState>) -> Result<MutexGuard<'_, ApiState>, String> {
    state
        .lock()
        .map_err(|_| API_STATE_LOCK_POISONED.to_string())
}

/// 内部读面的错误收口：锁中毒是"读不出来"回 503，其余（键形状非法、缺必需参数）是调用方给错了回 400。
///
/// 两者此前并进同一个 `400`：一个永久中毒的进程会对每个请求回"你的 account_id 有问题"，
/// 而真实原因一个字都读不到。
pub(crate) fn read_error_response(error: &str) -> ApiResponse {
    let status = if error == API_STATE_LOCK_POISONED {
        503
    } else {
        400
    };
    ApiResponse::json(status, error_json(error))
}
