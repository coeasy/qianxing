//! V12 §16 第二遍（死循环清点）：两条 accept 循环都必须有停机出口，而停机不能牺牲正常接受。
//!
//! `serve` 与 `serve_tls_mtls_with_stores` 过去写成 `for stream in listener.incoming()`，
//! 那个迭代器只在 accept 出错时结束。Ctrl+C 之后监督器只能把 worker 标成"已请求停机"，
//! worker 线程仍卡在 `accept` 里 —— `join()` 永不返回，投影线程与 TLS 重载线程也永远
//! 停不掉。这里把两件事钉成一对：能停，且停之前照常服务一条完整 HTTP 连接。

use qx_api::{ApiService, ApiState, MtlsIdentityPolicy, MtlsIdentityStore, TlsConfigStore};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::ServerConfig;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

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
    thread::spawn(move || tx.send(service.serve(listener, 1, stopped)).unwrap());

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
    thread::spawn(move || tx.send(service.serve(listener, 1, stopped)).unwrap());

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
        tx.send(service.serve_tls_mtls_with_stores(listener, &configs, &identities, 1, stopped))
            .unwrap()
    });

    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    stop.store(true, Ordering::Release);
    assert!(rx
        .recv_timeout(Duration::from_secs(5))
        .expect("mTLS 循环同样要有停机出口")
        .is_ok());
}
