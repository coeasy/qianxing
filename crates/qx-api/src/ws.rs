//! WebSocket 会话层：握手之前的准入判定与握手之后的帧循环。
//!
//! 从 crate 根拆出（lib.rs 的行数预算不允许它继续待在那里），行为与拆分前逐字相同；
//! 两个方法都是 `pub(crate)`，调用点只有 `dispatch_request` 一处，所以“判定失败时还能说
//! HTTP”这条口径没有变。

use super::*;
use crate::admission::{header_value, parse_request_line, split_target};
use crate::transport::write_ws_text;

impl ApiService {
    /// WebSocket 会话的准入判定：跨源、身份、握手 key、查询串、作用域总线与首批事件。
    ///
    /// 返回 `Ok(session)` 才允许握手；`Err(response)` 是握手之前还说得出口的那几种拒绝
    /// （403 跨源与身份、400 缺 key 或查询串、404 投影不存在、409 游标越界、500 事件总线
    /// 读失败）。词表与 `deploy/README.md` 的握手那一节逐条对齐，漏一种就是文档承诺不了的码。
    ///
    /// 作用域与游标这两格此前是断的：`serve_websocket` 拿的是 `state.event_bus`（全局）
    /// 与 `state.events`（整份日志），查询串一次都没读过。同一条 `/events/live` 上，HTTP
    /// 认 `account_id`/`venue_id` 与 `after`、WS 不认，客户端换成 WS 就静默读到了另一个
    /// 账户的事件流，而且每次连上都要把整份日志重发一遍。这里与 `live_events` 走同一份
    /// 取数（作用域总线 + `read_after(after)`），两条读链才是同一条链。
    pub(crate) fn admit_websocket(
        &self,
        request: &str,
        authenticated_operator: Option<&str>,
    ) -> Result<WsSession, ApiResponse> {
        // 跨源判定排在最前：名单外的浏览器页面连"这份部署是谁的"都不该问到。
        if self
            .cors
            .as_ref()
            .is_some_and(|cors| cors.websocket_denied(request))
        {
            return Err(ApiResponse::json(
                403,
                error_json("cors_origin_not_allowed"),
            ));
        }
        if self.policy.is_some()
            && authenticated_operator
                .and_then(|operator| self.policy.as_ref()?.permission(operator))
                .is_none()
        {
            self.metrics
                .authentication_rejected_total
                .fetch_add(1, Ordering::Relaxed);
            return Err(ApiResponse::json(
                403,
                error_json("authenticated_operator_required"),
            ));
        }
        // 缺 `Sec-WebSocket-Key` 必须在握手之前用 HTTP 说清（deploy/README 承诺握手需要这把 key）。
        // 此前这一判定落在 `serve_websocket` 里，而那里已经在写 101 的边上——客户端拿到的是一根
        // 被掐断的套接字、没有状态码，违背本模块"握手之前的每一支都还能说 HTTP"的自订口径。
        // 这里一次读出、随会话带走：帧循环不再第二次读它，"准入读到一个值、握手用另一个值"
        // 与那条走不到的拒绝臂一起消失。命名带 handshake_ 是因为下面还有一枚投影 `key`
        // 会把它盖掉——两枚 key 一枚是握手凭据、一枚是作用域。
        let Some(handshake_key) = header_value(request, "Sec-WebSocket-Key") else {
            return Err(ApiResponse::json(400, error_json("missing_websocket_key")));
        };
        let (path, query) = parse_request_line(request)
            .map(|(_, target)| split_target(target))
            .unwrap_or_default();
        let after = match parse_after_cursor(query) {
            Ok(after) => after,
            Err(error) => return Err(ApiResponse::json(400, error_json(error))),
        };
        let key = match projection_key_from_query(query) {
            Ok(key) => key,
            Err(error) => return Err(ApiResponse::json(400, error_json(&error))),
        };
        // 名单外的键当场 400：拼错的 `account_id` 落进"没有收窄键"那一支，就会把全局事件流
        // 念成调用方点名的那个账户，与 HTTP 七条读面同一口径（V13 R6）。报错点名的是请求实际
        // 打到的 target——WS 通道不占路由表，硬写 `/events/live` 会让连到别的路径的客户端
        // 收到谎报的路由名。
        if let Some(name) = crate::admission::refused_event_query_param(query) {
            return Err(ApiResponse::json(
                400,
                error_json(&format!("{path} 不接受查询参数 {name}")),
            ));
        }
        let state = self.state.lock().expect("api state mutex poisoned");
        if let Some(key) = &key {
            // 与七条 HTTP 读面同一口径：键指向的投影不在这份部署里就是 404，
            // 不能用"空事件流"冒充"这个账户什么都没发生"（V13 R2 #191）。
            if !state.projections.contains_key(key) {
                return Err(ApiResponse::json(
                    404,
                    error_json("account_projection_not_found"),
                ));
            }
        }
        let event_bus = match &key {
            Some(key) => state
                .projections
                .get(key)
                .map(|projection| projection.event_bus.clone())
                .unwrap_or_default(),
            None => state.event_bus.clone(),
        };
        let snapshot = match &key {
            Some(key) => state
                .projections
                .get(key)
                .and_then(|projection| projection.snapshot.clone()),
            None => state.snapshot.clone(),
        };
        drop(state);
        let initial = match event_bus.read_after(after) {
            Ok(initial) => initial,
            Err(EventBusError::CursorTooOld { .. } | EventBusError::CursorAhead { .. }) => {
                return Err(ApiResponse::json(
                    409,
                    error_json("event_cursor_requires_snapshot"),
                ))
            }
            Err(error) => return Err(ApiResponse::json(500, error_json(&format!("{error:?}")))),
        };
        Ok(WsSession {
            accept: websocket_accept(&handshake_key),
            event_bus,
            snapshot,
            initial,
            cursor: after,
        })
    }

    pub(crate) fn serve_websocket<S: Read + Write>(
        &self,
        stream: &mut S,
        session: WsSession,
    ) -> std::io::Result<()> {
        let accept = session.accept;
        let handshake = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        stream.write_all(handshake.as_bytes())?;
        write_ws_text(stream, "{\"type\":\"connected\",\"stream\":\"qianxing\"}")?;
        if let Some(snapshot) = &session.snapshot {
            write_ws_text(
                stream,
                &format!("{{\"type\":\"snapshot\",\"data\":{}}}", snapshot.to_json()),
            )?;
        }
        // 首批不再无条件整份重发：它由 `admit_websocket` 按 `after` 从**作用域**总线取出，
        // 与 `/events/live` 的 HTTP 那一支同一份实现。空批次（客户端已经追平）就不发这一帧。
        let mut cursor = session.cursor;
        if !session.initial.is_empty() {
            let events = session
                .initial
                .iter()
                .cloned()
                .map(|event| event_projection_envelope(event, "api-event-bus"))
                .collect::<Vec<_>>();
            if let Some(last) = session.initial.last() {
                cursor = Some(last.seq);
            }
            let events = serde_json::to_string(&events)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            write_ws_text(
                stream,
                &format!("{{\"type\":\"events\",\"data\":{events}}}"),
            )?;
        }
        let event_bus = session.event_bus;
        let mut client_buffer = [0_u8; 2048];
        let mut idle_rounds = 0_u32;
        loop {
            // 会话循环的出口不能只有"客户端自己关"：监听循环按 `stopped()` 收摊后，
            // 已经握手的会话若不读这个令牌，线程就永远等在 `wait_after` 的 100ms 轮询里，
            // `join()` 回不来，停机只能靠强杀（V13 R2 #218）。
            if self.session_shutdown.load(Ordering::Acquire) {
                write_ws_text(stream, "{\"type\":\"server_shutdown\"}")?;
                return Ok(());
            }
            let mut advanced = false;
            match event_bus.wait_after(cursor, Duration::from_millis(100)) {
                Ok(events) => {
                    for event in events {
                        let event_seq = event.seq;
                        let envelope = event_projection_envelope(event, "api-event-bus");
                        write_ws_text(
                            stream,
                            &format!(
                                "{{\"type\":\"event\",\"data\":{}}}",
                                serde_json::to_string(&envelope).map_err(|error| {
                                    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                                })?
                            ),
                        )?;
                        cursor = Some(event_seq);
                        advanced = true;
                    }
                }
                Err(EventBusError::CursorTooOld { .. } | EventBusError::CursorAhead { .. }) => {
                    write_ws_text(stream, "{\"type\":\"resync_required\"}")?;
                    return Ok(());
                }
                Err(error) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("event bus error: {error:?}"),
                    ));
                }
            }
            match stream.read(&mut client_buffer) {
                Ok(0) => return Ok(()),
                Ok(size)
                    if client_buffer[..size]
                        .iter()
                        .any(|byte| (*byte & 0x0f) == 0x8) =>
                {
                    return Ok(())
                }
                Ok(_) => advanced = true,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => return Ok(()),
                Err(error) => return Err(error),
            }
            // 空闲上界。这条不是"给会话加个寿命"，而是让连接预算真的收得回来：
            // 对端半开（不发 FIN、也不再写一个字节）时，上面的读永远拿到 TimedOut，
            // 写永远成功（数据进本地缓冲），停机令牌之外的出口一个都不会触发——
            // 那条线程和它占的那一格预算就永久留在账上。两个方向都静默满
            // `WS_MAX_IDLE_ROUNDS` 轮（每轮约 100ms）即主动收摊，客户端读到
            // `idle_timeout` 就重连，与读到 `server_shutdown` 一样是计划内关闭。
            idle_rounds = if advanced { 0 } else { idle_rounds + 1 };
            if idle_rounds >= WS_MAX_IDLE_ROUNDS {
                write_ws_text(stream, "{\"type\":\"idle_timeout\"}")?;
                return Ok(());
            }
        }
    }
}

/// 一条 WebSocket 会话在握手前就定下来的取数口径：握手应答、作用域总线、当前快照、首批事件与游标。
///
/// 单独成一个结构体是为了让"准入判定"与"帧循环"分开：判定失败时还能说 HTTP，
/// 而判定成功的产物必须**原样**交给帧循环，否则两侧各读一次 state 就会读到两份不同的游标。
/// `accept` 同属"判定成功的产物"：它是准入那一刻读出的那把 key 的应答，帧循环不许再读一次请求。
pub(crate) struct WsSession {
    accept: String,
    event_bus: ApiEventBus,
    snapshot: Option<AccountSnapshot>,
    initial: Vec<Event>,
    cursor: Option<u64>,
}

/// 会话空闲轮次上界：18000 轮 × 约 100ms ≈ 30 分钟双向静默。
///
/// 取值理由是"任何真客户端都不会两个方向同时静默这么久"——它要么收事件、要么发 ping/
/// close 帧。取得比反向代理的空闲超时（常见 60s）更长，是为了让**代理**先断开、由客户端
/// 读到 EOF，而不是让本进程先动手；代理不在场时这一条才是唯一出口。
const WS_MAX_IDLE_ROUNDS: u32 = 18_000;
