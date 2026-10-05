//! 浏览器准入这一层的五件事：跨源名单、预检、并发连接预算、"升级判定只看请求头"、
//! 以及 WS 握手自己那一问跨源。
//!
//! 立案现场是四条同时成立的缺口，而它们都在同一个位置——`dispatch_request` 里限流之后、
//! `handle_inner` 分派之前：
//!
//! 1. 没有任何跨源判定。前端只要不是从 API 自己那个源打开的，浏览器就会把响应扣下，
//!    而服务端日志里一切正常：这类故障在实现侧完全无声。
//! 2. 没有 `OPTIONS` 预检出口。带 `Content-Type: application/json` 的 `POST /control/commands`
//!    是"非简单请求"，浏览器一定先发预检；预检落到 404 就等于这条写入口在浏览器里不可用。
//! 3. 一条连接一个线程，而线程数没有上限。半开连接永不 EOF，飞行中的连接就单调增长。
//! 4. 升级判定按**整份请求文本** `contains("upgrade: websocket")`：一条正文里正好出现这串
//!    字的 `POST /control/commands` 会被当成握手，命令体连同它的审计一起丢掉。
//!
//! 第 5 轮在这条链上又找到第五格，位置不同：`is_websocket_upgrade` 那一支一次都不问跨源
//! 名单。浏览器不把 CORS 用在 WS 握手上（它照发带任意 `Origin` 的 `Upgrade` 请求，只在响应
//! 侧拦），所以那份名单只在 HTTP 侧生效时，任何跨源页面都能直连读事件流（CSWSH）。判据折在
//! 下面那条 WS 用例里，与"查询串排在 101 之前"共用同一支通道。
//!
//! 每条用例都按"改回去就红"的形状写：预检那条钉住 204 与四格定值头（含"不回显
//! `Access-Control-Request-Headers`"），预算那条钉住第二条连接拿到的 503 与它正文里的
//! `limit=`，升级那条钉住"正文里的那串字不算数"。

use qx_api::{ApiService, ApiState};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

/// 一个在跑的 API 监听循环。`Drop` 时置停机令牌并等它返回，用例不必各自收摊。
struct Running {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    done: mpsc::Receiver<std::io::Result<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.done.recv_timeout(Duration::from_secs(10));
    }
}

/// 在**调用方已经绑好的**监听端口上起服务：预检那两条用例要让 allowlist 里写的源与
/// 实际端口逐字符相等，所以端口必须先量出来再造 service。
fn spawn_on(service: ApiService, listener: TcpListener) -> Running {
    let address = listener.local_addr().expect("取监听地址");
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&stop);
    let (tx, done) = mpsc::channel();
    thread::spawn(move || {
        tx.send(service.serve(listener, || 1, move || seen.load(Ordering::Acquire)))
            .unwrap()
    });
    Running {
        address,
        stop,
        done,
    }
}

fn spawn(service: ApiService) -> Running {
    spawn_on(
        service,
        TcpListener::bind("127.0.0.1:0").expect("绑定监听端口"),
    )
}

/// 写一条请求、读到对端关闭为止。这条 API 每条响应都带 `Connection: close`，
/// 所以"读到 EOF"就是"响应完整"。
fn roundtrip(address: SocketAddr, request: &str) -> String {
    let mut client = TcpStream::connect(address).expect("连接监听端口");
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("设置读超时");
    client.write_all(request.as_bytes()).expect("写入请求");
    let mut response = String::new();
    client.read_to_string(&mut response).expect("读取响应");
    response
}

fn status_line(response: &str) -> &str {
    response
        .lines()
        .next()
        .unwrap_or_else(|| panic!("响应是空的：{response:?}"))
}

/// 上面那条示例 key 的 RFC 6455 握手应答：`base64(sha1(key + "258EAFA5-…5B11"))`，
/// 逐字符抄自规范 §1.3。服务端把这一格算错不是"这条通道拒绝你"，而是任何浏览器都连不上，
/// 而日志里一切正常——所以它必须被钉住，而不是靠"看起来连上了"。
const SAMPLE_WEBSOCKET_ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

/// 握手并读满 101 与首帧 `connected`，返回客户端侧连接（用例要拿它占住一格连接预算）。
/// `extra` 是要多带的请求头（含自己的 `\r\n`）；传 `""` 就是"不报源"的原生客户端形状。
fn open_websocket(address: SocketAddr, target: &str, extra: &str) -> TcpStream {
    let mut client = TcpStream::connect(address).expect("连接 WebSocket 端点");
    client
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("设置读超时");
    client
        .write_all(
            format!(
                "GET {target} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n{extra}\r\n\r\n"
            )
            .as_bytes(),
        )
        .expect("写入握手请求");
    let mut received = Vec::new();
    let mut chunk = [0_u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client.read(&mut chunk) {
            Ok(0) => panic!(
                "握手没完成连接就关了：{}",
                String::from_utf8_lossy(&received)
            ),
            Ok(size) => received.extend_from_slice(&chunk[..size]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("读取握手响应失败：{error}"),
        }
        let text = String::from_utf8_lossy(&received).to_string();
        if text.contains("101 Switching Protocols") && text.contains("{\"type\":\"connected\"") {
            assert!(
                text.contains(SAMPLE_WEBSOCKET_ACCEPT),
                "101 带的必须是这把 key 的规范应答：{text}"
            );
            return client;
        }
        assert!(Instant::now() < deadline, "握手响应不完整: {text}");
    }
}

#[test]
fn preflight_from_a_listed_origin_is_204_and_never_echoes_the_requested_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定监听端口");
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let service = ApiService::new(ApiState::default())
        .with_cors_allowed_origins([origin.clone()])
        .expect("精确源必须能装进 allowlist");
    let running = spawn_on(service, listener);

    let response = roundtrip(
        running.address,
        &format!(
            "OPTIONS /control/commands HTTP/1.1\r\nHost: localhost\r\nOrigin: {origin}\r\n\
             Access-Control-Request-Method: POST\r\n\
             Access-Control-Request-Headers: X-Spoofed-Operator\r\n\r\n"
        ),
    );
    assert_eq!(
        status_line(&response),
        "HTTP/1.1 204 No Content",
        "预检必须回 204：落到 404 就等于这条写入口在浏览器里不可用（非简单请求一定先发预检）。{response}"
    );
    for needle in [
        "Vary: Origin",
        &format!("Access-Control-Allow-Origin: {origin}"),
        "Access-Control-Allow-Methods: GET, POST, OPTIONS",
        "Access-Control-Allow-Headers: Content-Type",
        "Access-Control-Max-Age: 600",
    ] {
        assert!(
            response.contains(needle),
            "预检响应缺 {needle:?}：{response}"
        );
    }
    // 回显 `Access-Control-Request-Headers` 等于让调用方往响应头里写任意字串。
    assert!(
        !response.contains("X-Spoofed-Operator"),
        "预检把客户端请求的头名回显进了响应：{response}"
    );
    // RFC 9110 §15.3.5：204 不得带 Content-Length。带上之后严格实现会去等那 0 个正文
    // 字节，预检就以协议错误收场而不是以"允许"收场。
    assert!(
        !response.to_ascii_lowercase().contains("content-length"),
        "204 预检响应带了 Content-Length：{response}"
    );
}

#[test]
fn preflight_from_an_unlisted_origin_says_403_with_its_own_code_name() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定监听端口");
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let service = ApiService::new(ApiState::default())
        .with_cors_allowed_origins([origin.clone()])
        .expect("精确源必须能装进 allowlist");
    let running = spawn_on(service, listener);

    let response = roundtrip(
        running.address,
        "OPTIONS /control/commands HTTP/1.1\r\nHost: localhost\r\nOrigin: http://evil.example\r\n\
         Access-Control-Request-Method: POST\r\n\r\n",
    );
    assert_eq!(
        status_line(&response),
        "HTTP/1.1 403 Forbidden",
        "名单外的源必须拿到 403，而不是静默落进 404 兜底：{response}"
    );
    assert!(
        response.contains("cors_origin_not_allowed"),
        "403 的正文要点名 `cors_origin_not_allowed`，否则它与\"没登录\"的那个 403 在客户端读起来是同一件事：{response}"
    );
    assert!(
        !response.contains("Access-Control-Allow-Origin"),
        "名单外的源不该拿到 Allow-Origin：{response}"
    );
    // `Vary: Origin` 是无条件的：响应随 Origin 而变，中间缓存不按源分开存就会串台。
    assert!(
        response.contains("Vary: Origin"),
        "配了跨源策略的响应必须无条件带 Vary: Origin：{response}"
    );
}

#[test]
fn without_an_allowlist_the_preflight_falls_through_to_the_404_and_no_cors_headers_appear() {
    let running = spawn(ApiService::new(ApiState::default()));
    let response = roundtrip(
        running.address,
        "OPTIONS /health HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:5173\r\n\
         Access-Control-Request-Method: GET\r\n\r\n",
    );
    assert_eq!(
        status_line(&response),
        "HTTP/1.1 404 Not Found",
        "没开浏览器准入时 OPTIONS 要照常走分派落到 404，不能凭空多出一条路由：{response}"
    );
    assert!(
        !response.contains("Access-Control-Allow-Origin"),
        "没开浏览器准入却发了 Allow-Origin：{response}"
    );
    let health = roundtrip(
        running.address,
        "GET /health HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:5173\r\n\r\n",
    );
    assert_eq!(status_line(&health), "HTTP/1.1 200 OK", "{health}");
    assert!(
        !health.contains("Vary: Origin"),
        "没开浏览器准入时不该无端多出 Vary: Origin：{health}"
    );
}

#[test]
fn every_exit_of_a_cors_deployment_carries_the_same_allow_origin() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定监听端口");
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let service = ApiService::new(ApiState::default())
        .with_cors_allowed_origins([origin.clone()])
        .expect("精确源必须能装进 allowlist");
    let running = spawn_on(service, listener);

    // 三条出口分别落在 200、404、400 上：漏任何一条就是"这条路径跨源读不到"，
    // 而浏览器只会说"被 CORS 拦了"，看不出是哪一支没带头。
    for (label, request, expected) in [
        (
            "正常读面",
            format!("GET /health HTTP/1.1\r\nHost: localhost\r\nOrigin: {origin}\r\n\r\n"),
            "HTTP/1.1 200 OK",
        ),
        (
            "未知路径",
            format!("GET /nope HTTP/1.1\r\nHost: localhost\r\nOrigin: {origin}\r\n\r\n"),
            "HTTP/1.1 404 Not Found",
        ),
        (
            "参数非法",
            format!(
                "GET /account/orders?account_id=only-half HTTP/1.1\r\nHost: localhost\r\nOrigin: {origin}\r\n\r\n"
            ),
            "HTTP/1.1 400 Bad Request",
        ),
    ] {
        let response = roundtrip(running.address, &request);
        assert_eq!(status_line(&response), expected, "{label}：{response}");
        assert!(
            response.contains(&format!("Access-Control-Allow-Origin: {origin}")),
            "{label}那条出口没带 Allow-Origin：{response}"
        );
    }
}

#[test]
fn the_second_inflight_connection_is_refused_with_503_and_its_limit() {
    let service = ApiService::new(ApiState::default())
        .with_max_concurrent_connections(1)
        .expect("正数上限必须能装");
    assert_eq!(service.max_concurrent_connections(), 1);
    let running = spawn(service);

    // 一条 WebSocket 会话占住唯一那格预算：它不会自己结束，所以第二条连接必须在
    // 读到任何路由之前就被拒。
    let session = open_websocket(running.address, "/events/live", "");
    let response = roundtrip(
        running.address,
        "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert_eq!(
        status_line(&response),
        "HTTP/1.1 503 Service Unavailable",
        "预算用尽时第二条连接要当场 503，而不是排队（排队只是把拒绝变成更慢的接受）：{response}"
    );
    assert!(
        response.contains("connection_budget_exhausted: limit=1"),
        "503 的正文要说清是哪一格上限：{response}"
    );

    // 会话结束就把额度还回来：否则预算是只减不增的漏斗，重启才复位。
    drop(session);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let response = roundtrip(
            running.address,
            "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        if status_line(&response) == "HTTP/1.1 200 OK" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "会话关闭后预算没有归还，第二条连接一直被 503：{response}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_request_body_cannot_hijack_the_websocket_handshake() {
    let running = spawn(ApiService::new(ApiState::default()));
    let body = "{\"kind\":\"upgrade: websocket\"}";
    let response = roundtrip(
        running.address,
        &format!(
            "POST /control/commands HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{}",
            body.len(),
            body
        ),
    );
    assert_ne!(
        status_line(&response),
        "HTTP/1.1 101 Switching Protocols",
        "正文里出现 upgrade: websocket 就把这条 POST 当成握手了：命令体连同它的审计一起丢掉。{response}"
    );
    assert!(
        status_line(&response).starts_with("HTTP/1.1 400"),
        "载荷非法的 POST 要说 400：{response}"
    );
}

#[test]
fn the_websocket_branch_reads_the_query_string_before_it_writes_101() {
    let running = spawn(ApiService::new(ApiState::default()));
    // 立案时这一支一次都没读过查询串：任何 `Upgrade: websocket` 都直接 101，
    // `account_id`/`venue_id` 与 `after` 全被丢掉。五格各按 HTTP 状态码核对——
    // 一旦写下 101，状态码就没有第二次机会了，所以判定必须排在握手之前。
    // 第五格是 V13 R6 补的：名单外的键（拼错的 `acount_id`）不能当成"没带键"放行进全局那一份。
    for (target, expected, needle) in [
        ("/ws?after=abc", "HTTP/1.1 400 Bad Request", ""),
        ("/ws?account_id=ghost", "HTTP/1.1 400 Bad Request", ""),
        (
            "/ws?account_id=ghost&venue_id=ghost",
            "HTTP/1.1 404 Not Found",
            "account_projection_not_found",
        ),
        (
            "/ws?after=0",
            "HTTP/1.1 409 Conflict",
            "event_cursor_requires_snapshot",
        ),
        (
            "/ws?acount_id=ghost",
            "HTTP/1.1 400 Bad Request",
            "不接受查询参数 acount_id",
        ),
    ] {
        let response = roundtrip(
            running.address,
            &format!(
                "GET {target} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
            ),
        );
        assert_eq!(status_line(&response), expected, "{target}：{response}");
        if !needle.is_empty() {
            assert!(
                response.contains(needle),
                "{target} 的正文要点名 {needle:?}：{response}"
            );
        }
    }
    // 握手前的第六种码：连 `Sec-WebSocket-Key` 都没带。这一判定原先落在 `serve_websocket` 里，
    // 而那里已经在写 101 的边上——客户端拿到的是一根被掐断的套接字、一个状态码都没有，
    // 而 deploy/README 承诺握手需要这把 key。现在它跟其余几种一样，在 101 之前用 HTTP 说清。
    let keyless = roundtrip(
        running.address,
        "GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
    );
    assert_eq!(
        status_line(&keyless),
        "HTTP/1.1 400 Bad Request",
        "缺握手 key 要说 HTTP 400，不能只掐连接：{keyless}"
    );
    assert!(
        keyless.contains("missing_websocket_key"),
        "400 的正文要点名是缺了哪把 key：{keyless}"
    );
    // 没有查询串时照常握手：上面这六支拒绝不是把这条通道改成了"一律拒"。
    let session = open_websocket(running.address, "/ws", "");
    session.shutdown(std::net::Shutdown::Both).ok();

    // 同一支上还排着第五种判定：跨源。浏览器不把 CORS 用在 WS 握手上（照发 `Upgrade`、
    // 只在响应侧拦），所以配了名单的部署必须自己在写下 101 之前问一次，否则 HTTP 侧那份
    // 名单挡不住任何跨源页面读事件流。这里钉三面：名单外的源拿到 403 且码名与预检那一支
    // 同一个、名单内的源照常握手、完全不报源的原生客户端也照常握手（把它拒掉只会先杀掉
    // 自家 CLI 与探测脚本，而拦不住任何浏览器）。
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定监听端口");
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let gated = spawn_on(
        ApiService::new(ApiState::default())
            .with_cors_allowed_origins([origin.clone()])
            .expect("精确源必须能装进 allowlist"),
        listener,
    );
    let denied = roundtrip(
        gated.address,
        "GET /ws HTTP/1.1\r\nHost: localhost\r\nOrigin: http://evil.example\r\n\
         Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
    );
    assert_eq!(
        status_line(&denied),
        "HTTP/1.1 403 Forbidden",
        "名单外的源做 WS 升级必须当场 403，而不是换来一根 101 之后的长连接：{denied}"
    );
    assert!(
        denied.contains("cors_origin_not_allowed"),
        "这一支的 403 要与预检共用同一个码名，客户端才分得清「源不对」与「没登录」：{denied}"
    );
    assert!(
        denied.contains("Vary: Origin"),
        "拒绝也必须带 Vary: Origin：{denied}"
    );
    assert!(
        !denied.contains("Access-Control-Allow-Origin"),
        "名单外的源不该拿到 Allow-Origin：{denied}"
    );
    // 名单内的源与不报源的原生客户端都照常握手：这一支不是"带 Upgrade 就拒"。
    // `open_websocket` 只有在读满 101 与首帧 `connected` 之后才返回，拿到连接就是握手成了。
    for extra in [format!("Origin: {origin}\r\n"), String::new()] {
        let session = open_websocket(gated.address, "/ws", &extra);
        session.shutdown(std::net::Shutdown::Both).ok();
    }
}

#[test]
fn a_malformed_percent_escape_is_a_400_about_the_request_not_a_404_about_the_account() {
    let running = spawn(ApiService::new(ApiState::default()));
    let response = roundtrip(
        running.address,
        "GET /account/orders?account_id=main%zz&venue_id=paper HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert_eq!(
        status_line(&response),
        "HTTP/1.1 400 Bad Request",
        "半个转义原先被原样当成账户号去查投影，读到的是 404「没有这个账户」，而真正坏掉的是请求本身：{response}"
    );
    assert!(
        response.contains("query string percent-encoding is malformed"),
        "400 要说清坏在查询串的编码上：{response}"
    );
    // 同一格解码在合法输入上照常工作：`%20` 与 `+` 都解成空格，于是这一对键指向的是
    // 一个（这份部署里没有的）带空格的账户号，结论是 404 而不是 400。
    let decoded = roundtrip(
        running.address,
        "GET /account/orders?account_id=main%20paper&venue_id=pa+per HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert_eq!(status_line(&decoded), "HTTP/1.1 404 Not Found", "{decoded}");
    assert!(
        decoded.contains("account_projection_not_found"),
        "{decoded}"
    );
}
