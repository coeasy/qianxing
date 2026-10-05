//! 连接生命周期：接受之后开线程，接不下时体面地拒掉。
//!
//! 从 crate 根拆出，理由与 `transport.rs`、`ws.rs` 是同一句：`lib.rs` 的行数预算已经
//! 登记，而这两个方法都不含领域判定——一格是“这条连接交给哪个线程”，一格是“预算用尽
//! 时怎么关”。调用点只有两条 accept 循环，所以两个方法都是 `pub(crate)`。

use super::*;
use crate::admission::ConnectionGuard;
use crate::transport::refuse_connection;

impl ApiService {
    /// 每个长连接独立处理，避免 WebSocket 或慢客户端占住监听循环。
    /// 连接线程只拥有 API 的共享读模型和不可变服务配置；领域事实仍由
    /// Runtime owner 写入，连接处理失败只影响当前客户端。
    ///
    /// 并发预算的那一格在**调用方**（两条 accept 循环）就判掉：拒绝时手里还是裸
    /// `TcpStream`，才能半关+排空地把那条 503 送到对端；等到 TLS 包好再判，
    /// 就已经没有可用的关闭口径了。守卫随线程一起活——会话无论怎么退出（客户端关闭、
    /// 写失败、停机令牌、空闲上界），`Drop` 都会把这一格还回预算。
    pub(crate) fn spawn_connection<S>(
        &self,
        stream: S,
        ts: u64,
        operator_id: Option<String>,
        guard: ConnectionGuard,
    ) where
        S: Read + Write + Send + 'static,
    {
        let service = self.clone();
        std::thread::spawn(move || {
            let _guard = guard;
            if let Err(error) = service.serve_stream_as(stream, ts, operator_id.as_deref()) {
                eprintln!("[qx-api] connection closed with error: {error}");
            }
        });
    }

    /// 预算用尽时的一条连接：一条连接一个线程，没有上限时"多来几个客户端"就等价于
    /// "多开几个线程"，而半开连接（对端不发 FIN）那一格永远不会自己退。超额当场拒，
    /// 而不是排队——排队只是把拒绝换成更慢的接受，已 accept 的套接字与读缓冲照样各占一份。
    pub(crate) fn refuse_over_budget(&self, stream: &TcpStream) {
        let (live, limit) = self.connection_budget.occupancy();
        eprintln!("[qx-api] 并发连接已达上限 {limit}（飞行中 {live}），拒绝新连接");
        refuse_connection(
            stream,
            &ApiResponse::json(
                503,
                error_json(&format!("connection_budget_exhausted: limit={limit}")),
            ),
        );
    }
}
