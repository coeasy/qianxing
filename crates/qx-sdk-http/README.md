# Rust SDK 远程 HTTP 适配器

`qx-sdk-http` 为 `qx-sdk` 的 `RemoteApiClient` 提供可选的实际网络传输；它不重新实现回测、风控或交易逻辑。
它目前属于源码工作区，尚未发布到 crates.io。部署正在开发的版本后可在源码仓库运行：

```bash
cargo run -p qx-sdk-http --example discover -- loopback 127.0.0.1 18082
```

命令查询运行中的 `qx-api` 的策略配方；需要先启动相应 API。局部 Rust 工程可暂时以
`qx-sdk-http = { path = "<仓库>/crates/qx-sdk-http" }` 引入，使用同一仓库版本的依赖。

```rust,no_run
use qx_sdk_http::{HttpRemoteTransport, RemoteApiClient};
use std::time::Duration;
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let transport = HttpRemoteTransport::loopback("127.0.0.1", 18082, Duration::from_secs(5))?;
let client = RemoteApiClient::new(transport);
let recipes = client.builtin_strategy_recipes()?;
println!("{recipes}");
# Ok(())
# }
```

远程 Bar 回测复用本地 SDK 的规格和结果类型：

```rust,no_run
use qx_sdk_http::{BacktestDatasetInput, BacktestSpec, BuiltinStrategyKind,
    BuiltinStrategySpec, DatasetSpec, HttpRemoteTransport, RemoteApiClient};
use std::time::Duration;
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let client = RemoteApiClient::new(HttpRemoteTransport::loopback("127.0.0.1", 18082, Duration::from_secs(5))?);
let data = DatasetSpec::new("bars", "bars.json");
if !client.validate_dataset(&data)?.usable { return Err("data is not usable".into()); }
let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "sma");
strategy.fast_window = 2;
strategy.slow_window = 3;
let spec = BacktestSpec::new(
    BacktestDatasetInput { run_id: "research-1".into(), instrument: "BTCUSDT.BINANCE".into(), bars_path: "bars.json".into() },
    "USDT", 100_000 * 1_000_000_000, 7, "server-controlled", strategy,
);
let outcome = client.run_backtest(&spec)?;
let verification = client.verify_run(&outcome)?;
if !verification.verified { return Err(format!("verification failed: {:?}", verification.mismatches).into()); }
println!("verified={} hash={}", verification.verified, verification.result_hash);
# Ok(())
# }
```

数据路径相对于**服务端** `QX_API_DATA_ROOT`，调用前需由部署者准备数据；产物由服务端写入
`QX_API_ARTIFACT_ROOT`，请求的 `output_dir` 不决定服务端写入位置。异步调用使用
`start_backtest`、`run_status`、`get_run_events` 和 `cancel_run`；调用方应设置自己的等待时限，
取消请求被受理不等于运行已取消，仍需查询终态。

明文只接受数字 IPv4 回环地址。远程 HTTPS 使用 `HttpRemoteTransport::https(host, port, timeout)`，
校验服务器证书和主机名；配置客户端证书的服务使用
`HttpRemoteTransport::mtls(host, port, timeout, ca_pem, client_cert_pem, client_key_pem)`。
地址和端口分别传入，不接受带路径的 URL 或 IPv6。传输不自动重试；提交结果未知时应由应用按照
服务端状态与对账流程处理。远程操作权限由服务端证书身份与授权策略决定。

本地回环模式已通过真实 `qx-api` 的读取和错误响应契约测试。mTLS 测试使用专门生成的测试证书，
实际启动 `qx-api`，验证读取、证书身份映射、未授权证书拒绝及错误信任根；外部部署和真实交易所
授权尚未由此证明。
