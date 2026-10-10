//! 用例边界的 panic 闸（T2-0）。
//!
//! 「panic 边界」这条要求（路线图 T2-0）问的不是"我们写不写 panic"，而是**别人的 panic
//! 会不会把调用方一起带走**。应用层下游挂着引擎、策略与适配器：它们里的一个 `unwrap`、
//! 一次切片越界、一次 `expect`，在 CLI 里是进程崩掉（用户看到一堆 backtrace），
//! 在 HTTP 里是连接被重置（用户什么也看不到），在 Python 里是解释器收到 abort（用户拿不到
//! 任何可 catch 的异常）。同一场故障在三个入口有三张脸，而 §15.5 要的是**同一张**。
//!
//! 所以用例体一律从 [`guard_panics`] 里跑：panic 在边界处被翻成
//! [`AppErrorCategory::InternalInvariant`]——"这是 bug，去报，附上 correlation id"。
//!
//! ## 两条刻意的取舍
//!
//! 1. **不吞掉 panic 的诊断**。默认 panic hook 仍然会往 stderr 打那条消息（含位置），
//!    这里只是**额外**把它翻成返回值。把 hook 换成静默是另一种错——排障的人会失去唯一线索。
//! 2. **不假装它能兜住一切**。`catch_unwind` 兜不住 `panic = "abort"` 构建、兜不住
//!    double panic、也兜不住跨 FFI 边界 unwind 未定义行为。它兜的是本仓实际形态：
//!    unwind 构建下的 Rust panic。这条限制写在这里，而不是让读者以为有了它就万事大吉。

use crate::error::{AppError, AppErrorCategory};

/// 在 panic 边界内执行一段用例体。
///
/// `correlation_id` 会挂到翻出来的 [`AppError`] 上——panic 也必须是可定位的。
pub(crate) fn guard_panics<T>(
    correlation_id: &str,
    body: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(payload) => Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("用例边界捕获到 panic: {}", panic_message(&*payload)),
        )
        .with_correlation_id(correlation_id)
        .with_source_code("panic")),
    }
}

/// panic 载荷的文本形态。`panic!("...")` 是 `&str`，`panic!("{}", x)` 是 `String`，
/// 其余（例如 `panic_any(42)`）没有稳定文本——那类给一句固定的说明，**不**编造内容。
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "非字符串 panic 载荷".to_string()
}
