//! 状态行的原因短语与响应体说的是同一件事吗（V13 R2 第十六遍 #221 的线上那一半）。
//!
//! `write_http_response` 的短语表过去只点名六个码，`403` 与 `503` 一起落进 `_ => "Internal Server
//! Error"`：客户端按状态行分支会把"你没权限""这道闸门自己坏了"听成同一句 500 的话，而同一份响应
//! 的 body 里写着 `authenticated_operator_required` / `api_rate_limit_backend_unavailable` —— 两半
//! 各说一件事。`crates/qx-cli` 那条 `status_line_reason_names_cover_every_code_the_read_face_emits`
//! 按源码核对覆盖与两两不同，这里按**真 socket** 核对三个最常踩的出口：200、429、403，外加一条
//! 由限流后端自身故障产生的 503。

use qx_api::{ApiPolicy, ApiService, ApiState};
use qx_storage::FileTokenBucket;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn temp_root(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "qianxing-api-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// 起一条明文监听，返回 `(地址, 停机旗标, serve 的返回通道)`。
fn spawn_serve(
    service: ApiService,
) -> (
    SocketAddr,
    Arc<AtomicBool>,
    mpsc::Receiver<std::io::Result<()>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(service.serve(listener, || 1, move || stopped.load(Ordering::Acquire)))
            .unwrap()
    });
    (address, stop, rx)
}

fn get(address: SocketAddr, path: &str) -> String {
    let mut client = TcpStream::connect(address).expect("连接监听端口");
    client
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
        .expect("写入请求");
    let mut response = String::new();
    client.read_to_string(&mut response).expect("读取响应");
    response
}

/// 状态行的第一行，例如 `HTTP/1.1 429 Too Many Requests`。
fn status_line(response: &str) -> &str {
    response
        .lines()
        .next()
        .unwrap_or_else(|| panic!("空响应：{response:?}"))
}

/// 200 与 429 走的是同一条臂（`/health`），区别只在桶里还有没有令牌。
#[test]
fn the_rate_limit_exit_says_the_same_thing_in_the_status_line_and_the_body() {
    let root = temp_root("wire-429");
    let service = ApiService::new(ApiState::default()).with_shared_file_rate_limit(
        FileTokenBucket::new(&root, "wire", 1, 0).expect("一额度的桶要能建"),
    );
    let (address, stop, rx) = spawn_serve(service);

    let granted = get(address, "/health");
    assert_eq!(
        status_line(&granted),
        "HTTP/1.1 200 OK",
        "桶里还有令牌时的成功出口"
    );
    let rejected = get(address, "/health");
    assert_eq!(
        status_line(&rejected),
        "HTTP/1.1 429 Too Many Requests",
        "超额那一格的短语曾是空字符串以外的错话吗：整条响应 {rejected:?}"
    );
    assert!(
        rejected.contains("api_rate_limit_exceeded"),
        "状态行说了 429，body 就要给出可分支的码名：{rejected}"
    );

    stop.store(true, Ordering::Release);
    assert!(rx
        .recv_timeout(Duration::from_secs(5))
        .expect("停机后 serve 要返回")
        .is_ok());
    let _ = std::fs::remove_dir_all(root);
}

/// 限流后端自己读不到状态：这是"闸门坏了"，不是"你被限流了"，两格不能共用一句话。
#[test]
fn a_broken_rate_limit_backend_says_service_unavailable_not_internal_error() {
    // 把桶的根目录挂在一个普通文件下面：`create_dir_all` 必然失败，且失败原因是确定的。
    let root = temp_root("wire-503");
    std::fs::create_dir_all(&root).expect("临时根要能建");
    let blocker = root.join("not-a-directory");
    std::fs::write(&blocker, b"x").expect("占位文件要能写");
    let service = ApiService::new(ApiState::default()).with_shared_file_rate_limit(
        FileTokenBucket::new(blocker.join("nested"), "wire", 10, 10).expect("桶本身构造得出来"),
    );
    let (address, stop, rx) = spawn_serve(service);

    let response = get(address, "/health");
    assert_eq!(
        status_line(&response),
        "HTTP/1.1 503 Service Unavailable",
        "后端故障那一格过去落进 `_ => \"Internal Server Error\"`，与 403 共用一句 500 的话：{response:?}"
    );
    assert!(
        response.contains("api_rate_limit_backend_unavailable"),
        "503 要能让读者分辨是桶坏了而不是超额：{response}"
    );

    stop.store(true, Ordering::Release);
    assert!(rx
        .recv_timeout(Duration::from_secs(5))
        .expect("停机后 serve 要返回")
        .is_ok());
    let _ = std::fs::remove_dir_all(root);
}

/// 访问策略挡下来的 403：状态行不能再替它说"服务坏了"。
#[test]
fn an_unauthenticated_control_read_says_forbidden_not_internal_error() {
    let service = ApiService::with_policy(ApiState::default(), ApiPolicy::new());
    let (address, stop, rx) = spawn_serve(service);

    let exempt = get(address, "/health");
    assert_eq!(status_line(&exempt), "HTTP/1.1 200 OK", "{exempt}");
    let refused = get(address, "/account/snapshot");
    assert_eq!(
        status_line(&refused),
        "HTTP/1.1 403 Forbidden",
        "明文连接没有 operator 身份，这一格必须是 403 那句：{refused:?}"
    );
    assert!(
        refused.contains("authenticated_operator_required"),
        "{refused}"
    );

    stop.store(true, Ordering::Release);
    assert!(rx
        .recv_timeout(Duration::from_secs(5))
        .expect("停机后 serve 要返回")
        .is_ok());
}
