//! `ApiState` 的锁中毒不再是 panic，而是可返回的 503（V13 R26 / §6.D3）。
//!
//! 反例形状：任一持有者 panic 之后互斥量**永久**中毒，此后每个请求线程都在同一次 `.lock()`
//! 上 panic——进程还在、连接全断，运维侧看不到任何 5xx。这里先把锁真弄中毒，再逐条打读面：
//! 每一条都必须回 503 `api_state_lock_poisoned`，既不许 panic，也不许拿空数据冒充"没有数据"。
//!
//! 反向验证依据写在断言里：`is_poisoned()` 必须先为真（夹具真的把锁弄中毒了），否则整条用例
//! 会在"锁根本没坏"的进程上恒绿。

use qx_api::{ApiService, ApiState};

/// 把服务内部的状态锁真中毒（持锁 panic 一次），返回仍可用的服务句柄。
fn poisoned_service() -> ApiService {
    let service = ApiService::new(ApiState::default());
    let state = service.state();
    // 这一次 panic 是夹具的一部分：静音默认 hook，免得测试输出里混进一段"预期内"的 panic。
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = state.lock().expect("fresh lock");
        panic!("poison the api state lock");
    }));
    std::panic::set_hook(hook);
    assert!(caught.is_err(), "夹具必须真的 panic 一次");
    assert!(state.is_poisoned(), "夹具必须真的把状态锁弄中毒");
    service
}

/// 中毒之后每一条读面都要回 503 + 原因，而不是 panic、也不是 200 空数据。
#[test]
fn poisoned_state_lock_turns_http_reads_into_503_not_a_panic() {
    let service = poisoned_service();
    for (method, path) in [
        ("GET", "/ready"),
        ("GET", "/account/snapshot"),
        ("GET", "/account/orders"),
        ("GET", "/account/positions"),
        ("GET", "/account/balances"),
        ("GET", "/account/snapshot/envelope"),
        ("GET", "/account/snapshot/diff?base_hash=1"),
        ("GET", "/events"),
        ("GET", "/events/live"),
        ("GET", "/scheduler/runs"),
        ("GET", "/account/ledger"),
        ("GET", "/reconcile/reports"),
        ("GET", "/control/audit"),
    ] {
        let response = service.handle(method, path, "", 1);
        assert_eq!(
            response.status, 503,
            "{method} {path} 锁中毒时必须回 503，实际 {}: {}",
            response.status, response.body
        );
        assert!(
            response.body.contains("api_state_lock_poisoned"),
            "{method} {path} 要印出中毒原因，而不是一份看起来正常的空数据: {}",
            response.body
        );
    }
}

/// 对照组：锁没坏时这些读面照常工作——否则上一条用例可以被"把所有读面都改成 503"骗过。
#[test]
fn healthy_state_lock_still_serves_the_same_reads() {
    let service = ApiService::new(ApiState::default());
    for path in ["/account/snapshot", "/events", "/scheduler/runs"] {
        let response = service.handle("GET", path, "", 1);
        assert_ne!(
            response.status, 503,
            "{path} 在锁健康时不得回 503: {}",
            response.body
        );
    }
    assert_eq!(service.handle("GET", "/ready", "", 1).status, 200);
}
