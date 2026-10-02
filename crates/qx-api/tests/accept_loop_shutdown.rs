//! V12 §16 第二遍（死循环清点）：两条 accept 循环都必须有停机出口，而停机不能牺牲正常接受。
//!
//! `serve` 与 `serve_tls_mtls_with_stores` 过去写成 `for stream in listener.incoming()`，
//! 那个迭代器只在 accept 出错时结束。Ctrl+C 之后监督器只能把 worker 标成"已请求停机"，
//! worker 线程仍卡在 `accept` 里 —— `join()` 永不返回，投影线程与 TLS 重载线程也永远
//! 停不掉。这里把两件事钉成一对：能停，且停之前照常服务一条完整 HTTP 连接。
//!
//! V13 R2 #218 补上另一半：accept 循环有了出口，不等于连接线程有出口。WebSocket 会话
//! 循环在握手之后只认"客户端自己关"，所以停机时已经连上的前端会把线程留在 `wait_after`
//! 的轮询里 —— 本文件末条用例按"停机请求 → 会话收到 `server_shutdown` → 连接读到 EOF"核对。

use qx_api::{ApiService, ApiState, MtlsIdentityPolicy, MtlsIdentityStore, TlsConfigStore};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::ServerConfig;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

/// 没有证书可解析的服务端配置：本用例从不握手，只需要一个能构造出的 `ServerConfig`。
#[derive(Debug)]
struct NoCertificate;

impl ResolvesServerCert for NoCertificate {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        None
    }
}

fn stop_channel() -> (Arc<AtomicBool>, impl Fn() -> bool) {
    let flag = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&flag);
    (flag, move || seen.load(Ordering::Acquire))
}

fn serves_health(address: std::net::SocketAddr) -> String {
    let mut client = TcpStream::connect(address).expect("连接监听端口");
    client
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("写入请求");
    // accepted socket 必须回到阻塞态：它继承监听端的非阻塞位时，这一行会以
    // WouldBlock 半路失败或读到截断响应（Linux 上 accept 确实继承）。
    let mut response = String::new();
    client.read_to_string(&mut response).expect("读取响应");
    response
}

#[test]
fn plaintext_accept_loop_serves_then_returns_on_the_stop() {
    let service = ApiService::new(ApiState::default());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = stop_channel();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(service.serve(listener, || 1, stopped)).unwrap());

    let response = serves_health(address);
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("{\"status\":\"ok\"}"), "{response}");

    stop.store(true, Ordering::Release);
    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("停机请求后 serve 必须返回，而不是留在 accept 里");
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn plaintext_accept_loop_returns_without_any_connection() {
    let service = ApiService::new(ApiState::default());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (stop, stopped) = stop_channel();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(service.serve(listener, || 1, stopped)).unwrap());

    // 还没停机时循环必须仍在值守：先确认它没有"立刻就返回"这种假绿。
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "没有停机请求时 serve 不得返回"
    );
    stop.store(true, Ordering::Release);
    assert!(rx
        .recv_timeout(Duration::from_secs(5))
        .expect("零连接也必须能退出")
        .is_ok());
}

#[test]
fn mtls_accept_loop_returns_on_the_stop() {
    let service = ApiService::new(ApiState::default());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let configs = TlsConfigStore::new(Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(NoCertificate)),
    ));
    let identities = MtlsIdentityStore::new(MtlsIdentityPolicy::default());
    let (stop, stopped) = stop_channel();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(service.serve_tls_mtls_with_stores(listener, &configs, &identities, || 1, stopped))
            .unwrap()
    });

    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    stop.store(true, Ordering::Release);
    assert!(rx
        .recv_timeout(Duration::from_secs(5))
        .expect("mTLS 循环同样要有停机出口")
        .is_ok());
}

/// 按 WebSocket 握手连上一条会话，返回客户端侧的连接。
///
/// 服务端到客户端的帧不掩码，所以帧载荷在字节流里是原样的 ASCII，本用例直接按子串取。
/// 握手响应与首帧 `connected` 是分两次写的，这里读到两条都出现为止（TCP 段边界不由我们定）。
fn open_websocket_session(address: std::net::SocketAddr) -> TcpStream {
    let mut client = TcpStream::connect(address).expect("连接 WebSocket 端点");
    // 整条用例都要"读一会儿没有就算数"，所以给连接一个轮询式的读超时。
    client
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("设置读超时");
    client
        .write_all(
            b"GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
               Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .expect("写入握手请求");
    let mut received = Vec::new();
    let mut chunk = [0_u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match client.read(&mut chunk) {
            Ok(0) => {
                panic!(
                    "握手还没完成连接就被关闭：{}",
                    String::from_utf8_lossy(&received)
                )
            }
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
            return client;
        }
        assert!(Instant::now() < deadline, "握手响应不完整: {text}");
    }
}

/// 在给定时间内读满一个结论：读到 `needle` 时返回读到的字节数，超时返回 None。
fn read_until(client: &mut TcpStream, needle: &str, within: Duration) -> Option<usize> {
    let deadline = Instant::now() + within;
    let mut received = Vec::new();
    let mut chunk = [0_u8; 1024];
    while Instant::now() < deadline {
        match client.read(&mut chunk) {
            Ok(0) => return None,
            Ok(size) => {
                received.extend_from_slice(&chunk[..size]);
                if String::from_utf8_lossy(&received).contains(needle) {
                    return Some(received.len());
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return None,
        }
    }
    None
}

#[test]
fn plaintext_accept_loop_stop_ends_the_inflight_websocket_session() {
    // 反向验证依据：`serve` 收摊时不置 `session_shutdown`，或 `serve_websocket` 的循环
    // 不读这个令牌时，下面的读会一直拿不到 `server_shutdown`（红）。
    let service = ApiService::new(ApiState::default());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = stop_channel();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(service.serve(listener, || 1, stopped)).unwrap());

    let mut client = open_websocket_session(address);
    // 还没停机时会话必须保持打开：否则这条判据会被"一握手就断"的实现骗绿。
    assert_eq!(
        read_until(&mut client, "server_shutdown", Duration::from_millis(300)),
        None,
        "停机请求之前会话不得自己结束"
    );

    stop.store(true, Ordering::Release);
    assert!(rx
        .recv_timeout(Duration::from_secs(5))
        .expect("停机后 serve 要返回")
        .is_ok());
    assert!(
        read_until(&mut client, "server_shutdown", Duration::from_secs(5)).is_some(),
        "飞行中的 WebSocket 会话没有拿到停机出口，只能等客户端自己关"
    );
    // 会话线程返回后连接关闭：客户端这一侧要读到 EOF，而不是悬着的半开连接。
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut tail = [0_u8; 64];
    loop {
        match client.read(&mut tail) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(error) => panic!("会话结束后连接要读到 EOF，而不是 {error}"),
        }
    }
}
