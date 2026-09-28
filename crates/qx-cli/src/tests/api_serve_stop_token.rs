//! 长驻 accept 循环要读停机令牌（V11 J2）。
//!
//! S2 让 scheduler 与 strategy 两个循环轮询了令牌，API 这条却是仓库里唯一一处"接住请求就
//! 再也不回来"的循环：`run_runtime_api` 主线程 `worker.join()`，而 `serve` 永远等在 accept 上，
//! 于是令牌置起之后 `join` 也回不来——投影桥与 TLS 重载线程的收尾全排在 join 之后，
//! 优雅退出这条路等于不存在。这里只有一格可测：令牌翻起来，`serve` 得自己返回。

use super::*;

#[test]
fn serve_returns_once_the_supervisor_requests_shutdown() {
    let root = temp_cli_case_dir("api-serve-stop-token");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    let config_path = data_dir.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    let api_worker_id = configured_api_worker_id(&config).expect("paper 拓扑里有启用的 api worker");
    let service = build_configured_api_service(&config, &config_path).unwrap();
    let supervisor = RuntimeSupervisor::new(config).expect("监督器接受这份拓扑");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&finished);
    let worker = supervisor
        .spawn_worker(&api_worker_id, move |context| {
            service
                .serve(listener, runtime_timestamp_ms(), move || {
                    context.should_stop()
                })
                .map_err(|error| format!("API 服务停止: {error}"))?;
            flag.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        })
        .expect("按 role 解析出的 api worker 能注册");

    // 先走一条真请求：改成非阻塞轮询之后，监听循环还得照样接客。
    let mut client = std::net::TcpStream::connect(addr).expect("监听地址接得住一条真连接");
    // 读侧必须自己带上界：下面那颗 5 秒上界排在 `read_line` **之后**，管不到这一次等待。
    // 少了这一行，"接客但不吐 HTTP 首行"的回归不是红，而是把整颗用例挂到 CI 作业超时。
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .expect("测试客户端应能设置读超时");
    std::io::Write::write_all(
        &mut client,
        b"GET /ready HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )
    .unwrap();
    let mut head = String::new();
    match std::io::BufRead::read_line(&mut std::io::BufReader::new(&mut client), &mut head) {
        Ok(_) => {}
        Err(error) => panic!("5 秒内没有读回 HTTP 首行，等待本身必须以超时红收掉: {error}"),
    }
    assert!(
        head.starts_with("HTTP/1.1 "),
        "accept 循环不再接客，第一行读回来的是：{head:?}"
    );
    assert!(
        !finished.load(std::sync::atomic::Ordering::Acquire),
        "没人停机时 serve 不该自己退出"
    );

    supervisor.request_shutdown();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !finished.load(std::sync::atomic::Ordering::Acquire)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        finished.load(std::sync::atomic::Ordering::Acquire),
        "停机令牌已置起，serve 却还卡在 accept 上：调用方的 join 回不来"
    );
    worker
        .join()
        .expect("API worker 线程正常收尾")
        .expect("serve 以 Ok 结束");
    let _ = std::fs::remove_dir_all(root);
}
