//! Binance Spot 具体连接器。
//!
//! 该模块只负责 Binance 协议边界（参数编码/签名、订单 JSON、快照及用户事件映射）；订单事实通过 `VenueEvent` 返回，不直接修改 OMS、Ledger 或 Kernel。

use super::order_lifecycle::OrderLifecycle;
use super::venue_cache::evict_stale_terminal_orders;
use super::{HttpRequest, HttpResponse, HttpTransport, TlsWebSocketUserStream, WebSocketPoll};
use qx_core::{
    retry, DecimalRounding, ExactDecimal, ExactFillAmounts, ExactFillFee, Fill, FillFee,
    InstrumentId, Money, Order, OrderStatus, Price, Quantity, QxError, QxResult, Side,
    TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_guanxing::QuoteTick;
use qx_zhenlu::{ConnectorState, RateLimiter, Venue, VenueEvent, VenueOrderSnapshot};
use ring::hmac;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_HOST: &str = "api.binance.com";
const DEFAULT_PORT: u16 = 443;
const DEFAULT_WS_PATH: &str = "/ws-api/v3";
const TESTNET_HOST: &str = "testnet.binance.vision";

/// Binance Spot HMAC API 凭据与参数签名边界。
#[derive(Clone)]
pub struct BinanceSpotAuth {
    api_key: String,
    secret: Arc<hmac::Key>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    recv_window: u64,
}

/// Binance 凭据的部署边界表示；`secret` 不实现 Debug，也不进入配置 JSON、事件或日志。
pub struct BinanceSpotCredentials {
    api_key: String,
    secret: Vec<u8>,
}

impl BinanceSpotCredentials {
    pub fn new(api_key: impl Into<String>, secret: impl AsRef<[u8]>) -> Result<Self, String> {
        let api_key = api_key.into();
        let secret = secret.as_ref().to_vec();
        if api_key.trim().is_empty() || secret.is_empty() {
            return Err("Binance API key 和 secret 不能为空".into());
        }
        Ok(Self { api_key, secret })
    }

    /// 从环境变量读取凭据；变量名只接受调用方显式传入的键名。
    pub fn from_env(api_key_var: &str, secret_var: &str) -> Result<Self, String> {
        if api_key_var.trim().is_empty() || secret_var.trim().is_empty() {
            return Err("Binance 凭据环境变量名不能为空".into());
        }
        let api_key = std::env::var(api_key_var)
            .map_err(|_| format!("缺少 Binance API key 环境变量: {api_key_var}"))?;
        let secret = std::env::var(secret_var)
            .map_err(|_| format!("缺少 Binance secret 环境变量: {secret_var}"))?;
        Self::new(api_key, secret)
    }

    /// 从部署系统投影的凭据文件读取 API key/secret（只允许一行，首尾空白去除）；
    /// 读取结果不进 Debug/事件日志/健康详情，部署系统可原子替换文件做轮换，
    /// 调用方应在建立新连接或新请求前重新调用，旧连接沿用已建立的认证上下文。
    pub fn from_files(
        api_key_path: impl AsRef<std::path::Path>,
        secret_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, String> {
        let api_key = read_credential_file(api_key_path.as_ref(), "Binance API key")?;
        let secret = read_credential_file(secret_path.as_ref(), "Binance secret")?;
        Self::new(api_key, secret)
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn into_auth(self) -> Result<BinanceSpotAuth, String> {
        let mut credentials = self;
        let api_key = std::mem::take(&mut credentials.api_key);
        let mut secret = std::mem::take(&mut credentials.secret);
        let auth = BinanceSpotAuth::new(api_key, &secret);
        secret.fill(0);
        auth
    }
}

fn read_credential_file(path: &std::path::Path, label: &str) -> Result<String, String> {
    if path.as_os_str().is_empty() {
        return Err(format!("{label} 文件路径不能为空"));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读取 {label} 文件失败 {}: {error}", path.display()))?;
    let value = std::str::from_utf8(&bytes)
        .map_err(|error| format!("{label} 文件不是 UTF-8: {error}"))?
        .trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(format!("{label} 文件内容非法"));
    }
    Ok(value.to_owned())
}

impl std::fmt::Debug for BinanceSpotCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BinanceSpotCredentials")
            .field("api_key", &self.api_key)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl Drop for BinanceSpotCredentials {
    fn drop(&mut self) {
        self.secret.fill(0);
    }
}

impl BinanceSpotAuth {
    pub fn new(api_key: impl Into<String>, secret: &[u8]) -> Result<Self, String> {
        Self::with_clock(api_key, secret, || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0)
        })
    }

    pub fn with_clock<F>(
        api_key: impl Into<String>,
        secret: &[u8],
        clock: F,
    ) -> Result<Self, String>
    where
        F: Fn() -> u64 + Send + Sync + 'static,
    {
        let api_key = api_key.into();
        if api_key.trim().is_empty() || secret.is_empty() {
            return Err("Binance API key 和 secret 不能为空".into());
        }
        Ok(Self {
            api_key,
            secret: Arc::new(hmac::Key::new(hmac::HMAC_SHA256, secret)),
            clock: Arc::new(clock),
            recv_window: 5_000,
        })
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// 返回 Binance HMAC 签名使用的 RFC3986 编码参数串对应的十六进制摘要。
    pub fn sign_parameters(&self, parameters: &BTreeMap<String, String>) -> String {
        self.sign_encoded_payload(&form_encode(parameters))
    }

    fn sign_encoded_payload(&self, payload: &str) -> String {
        let tag = hmac::sign(&self.secret, payload.as_bytes());
        tag.as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn signed_request(
        &self,
        method: &str,
        host: &str,
        port: u16,
        path: &str,
        mut parameters: BTreeMap<String, String>,
    ) -> HttpRequest {
        parameters.insert("recvWindow".into(), self.recv_window.to_string());
        parameters.insert("timestamp".into(), (self.clock)().to_string());
        let signature = self.sign_parameters(&parameters);
        parameters.insert("signature".into(), signature);
        let payload = form_encode(&parameters);
        let upper = method.to_ascii_uppercase();
        let (path, body) = if matches!(upper.as_str(), "GET" | "DELETE") {
            (format!("{path}?{payload}"), String::new())
        } else {
            (path.to_string(), payload)
        };
        let mut headers = BTreeMap::new();
        headers.insert("X-MBX-APIKEY".into(), self.api_key.clone());
        headers.insert(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        );
        HttpRequest {
            method: upper,
            host: host.into(),
            port,
            path,
            body,
            headers,
        }
    }

    /// 生成 Binance Spot WebSocket API 的签名用户流订阅请求；只产出协议 payload，
    /// 不建立网络连接，便于先做 golden 校验，也避免密钥进入日志或事件事实。
    pub fn user_stream_subscribe_payload(&self, request_id: impl Into<String>) -> String {
        let mut parameters = BTreeMap::new();
        parameters.insert("apiKey".into(), self.api_key.clone());
        parameters.insert("recvWindow".into(), self.recv_window.to_string());
        parameters.insert("timestamp".into(), (self.clock)().to_string());
        let signature = self.sign_parameters(&parameters);
        serde_json::json!({
            "id": request_id.into(),
            "method": "userDataStream.subscribe.signature",
            "params": {
                "apiKey": self.api_key,
                "recvWindow": self.recv_window,
                "timestamp": parameters["timestamp"].parse::<u64>().unwrap_or_default(),
                "signature": signature,
            }
        })
        .to_string()
    }
}

/// Binance Spot 用户流会话。
///
/// 会话只负责 WSS 连接、签名订阅和原始 JSON 事件接收；业务事件仍必须通过
/// [`BinanceSpotVenue::ingest_user_event`] 转换为统一 `VenueEvent`。
pub struct BinanceSpotUserStream {
    stream: TlsWebSocketUserStream,
    subscription_id: u64,
}

/// 连接器重连退避策略。时间由调用方的 `sleep` 注入；V10 §4.9 起退避公式与终态判定统一委托 [`qx_core::retry`]，此处只承载形状参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinanceStreamRetryPolicy {
    pub max_reconnects: u32,
    pub base_delay: std::time::Duration,
    pub max_delay: std::time::Duration,
}

impl BinanceStreamRetryPolicy {
    pub fn new(
        max_reconnects: u32,
        base_delay: std::time::Duration,
        max_delay: std::time::Duration,
    ) -> Result<Self, String> {
        if base_delay.is_zero() || max_delay.is_zero() || base_delay > max_delay {
            return Err("Binance 流重连退避参数非法".into());
        }
        Ok(Self {
            max_reconnects,
            base_delay,
            max_delay,
        })
    }

    /// 判定与延迟都委托 `qx-core::retry` 的唯一实现；`reconnect_number` 从 1 起算。
    pub fn allows_reconnect(&self, reconnect_number: u32) -> bool {
        retry::RetryPolicy::attempts_only(self.max_reconnects)
            .should_retry(reconnect_number.saturating_sub(1))
    }

    pub fn delay_for(&self, reconnect_number: u32) -> std::time::Duration {
        retry::Backoff::exponential(self.base_delay, self.max_delay)
            .delay_before_attempt(reconnect_number)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BinanceStreamRunReport {
    pub events: u64,
    /// 生涯累计重连次数，只用于观测。
    pub reconnects: u32,
    /// 连续失败的重连次数：交付过事件的会话会把它清零，终态判定只看这个。
    pub consecutive_failures: u32,
    /// 空闲读取窗口的次数：链路仍开着但这一窗没有事件，是"静默≠故障"的落地证据。
    pub idle_windows: u64,
}

#[derive(Clone, Debug)]
pub struct BinanceUserStreamRunConfig {
    pub ws_host: String,
    pub ws_port: u16,
    pub request_id_prefix: String,
    pub timeout: std::time::Duration,
    pub retry_policy: BinanceStreamRetryPolicy,
}

impl BinanceUserStreamRunConfig {
    pub fn new(
        ws_host: impl Into<String>,
        request_id_prefix: impl Into<String>,
        timeout: std::time::Duration,
        retry_policy: BinanceStreamRetryPolicy,
    ) -> Result<Self, String> {
        Self::with_port(
            ws_host,
            DEFAULT_PORT,
            request_id_prefix,
            timeout,
            retry_policy,
        )
    }

    pub fn with_port(
        ws_host: impl Into<String>,
        ws_port: u16,
        request_id_prefix: impl Into<String>,
        timeout: std::time::Duration,
        retry_policy: BinanceStreamRetryPolicy,
    ) -> Result<Self, String> {
        let ws_host = ws_host.into();
        let request_id_prefix = request_id_prefix.into();
        if ws_host.trim().is_empty() || request_id_prefix.trim().is_empty() {
            return Err("Binance 用户流主机和 request_id 前缀不能为空".into());
        }
        if timeout.is_zero() {
            return Err("Binance 用户流连接超时必须为正数".into());
        }
        if ws_port == 0 {
            return Err("Binance 用户流端口必须为正数".into());
        }
        Ok(Self {
            ws_host,
            ws_port,
            request_id_prefix,
            timeout,
            retry_policy,
        })
    }
}

/// 用户流一次读取窗口的结局。`Idle` 单独成态是这条链的关键：**没有事件**不能消耗重连预算，
/// 否则一个连着但长时间不出单的纸面账户会按预算被判死（10s 读窗 × 10 次）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BinanceStreamPoll {
    Event(String),
    Idle,
    Closed,
}

/// 行情流一次读取窗口的结局；静默交给调用方判 staleness，不作为致命错误上抛。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BinanceQuotePoll {
    Quote(QuoteTick),
    Idle,
    Closed,
}

/// 可测试的用户流会话抽象；真实实现为 `BinanceSpotUserStream`，测试可注入内存会话。
pub trait BinanceUserStreamSession {
    fn recv_event(&mut self) -> Result<BinanceStreamPoll, String>;
    fn close(&mut self) -> Result<(), String>;
}

impl BinanceUserStreamSession for BinanceSpotUserStream {
    fn recv_event(&mut self) -> Result<BinanceStreamPoll, String> {
        Self::recv_event(self)
    }

    fn close(&mut self) -> Result<(), String> {
        Self::close(self)
    }
}

/// 驱动长连接的确定性重连循环。连接器、休眠和停止条件均可替换，便于沙盒/故障注入验收。
pub fn run_binance_user_stream<S, C, Stop, Sleep, Event>(
    mut connect: C,
    policy: BinanceStreamRetryPolicy,
    mut should_stop: Stop,
    mut sleep: Sleep,
    mut on_event: Event,
) -> Result<BinanceStreamRunReport, String>
where
    S: BinanceUserStreamSession,
    C: FnMut() -> Result<S, String>,
    Stop: FnMut() -> bool,
    Sleep: FnMut(std::time::Duration),
    Event: FnMut(&str) -> Result<(), String>,
{
    let mut report = BinanceStreamRunReport::default();
    while !should_stop() {
        let mut session = match connect() {
            Ok(session) => session,
            Err(error) => {
                report.reconnects = retry::RetryPolicy::next_attempt_count(report.reconnects);
                report.consecutive_failures =
                    retry::RetryPolicy::next_attempt_count(report.consecutive_failures);
                if !policy.allows_reconnect(report.consecutive_failures) {
                    return Err(format!(
                        "Binance 用户流连续 {} 次重连失败且超过重试上限: {error}",
                        report.consecutive_failures
                    ));
                }
                sleep(policy.delay_for(report.consecutive_failures));
                continue;
            }
        };
        let mut callback_error = None;
        let mut delivered = 0_u64;
        // 只有"回调自己拒了"才算本地确定性故障；链路断开（`recv_event` 报错）不是，
        // 否则一条交付过事件后被网络掐掉的会话永远无法给预算复位。
        let mut callback_failed = false;
        while !should_stop() {
            match session.recv_event() {
                Ok(BinanceStreamPoll::Event(event)) => match on_event(&event) {
                    Ok(()) => {
                        report.events = report.events.saturating_add(1);
                        delivered += 1;
                    }
                    Err(error) => {
                        callback_error = Some(error);
                        callback_failed = true;
                        break;
                    }
                },
                // 链路还开着、只是这一窗没有事件：既不充当交付（否则空闲会替坏链路复位预算），
                // 也不消耗重连预算（否则空闲账户几个窗口内就被判死）。
                Ok(BinanceStreamPoll::Idle) => {
                    report.idle_windows = report.idle_windows.saturating_add(1);
                    continue;
                }
                Ok(BinanceStreamPoll::Closed) => break,
                Err(error) => {
                    callback_error = Some(error);
                    break;
                }
            }
        }
        let _ = session.close();
        if delivered > 0 && !callback_failed {
            // 交付过事件的会话证明链路可用：退避预算重新计，长期健康的流不会被判死。
            // 但回调失败通常是确定性错误，即使这条会话此前交付过事件也不算"已恢复"，
            // 复位只会让同一个错误被无限重试。
            report.consecutive_failures = 0;
        }
        if should_stop() {
            break;
        }
        report.reconnects = retry::RetryPolicy::next_attempt_count(report.reconnects);
        report.consecutive_failures =
            retry::RetryPolicy::next_attempt_count(report.consecutive_failures);
        if !policy.allows_reconnect(report.consecutive_failures) {
            return Err(callback_error.unwrap_or_else(|| "Binance 用户流关闭".into()));
        }
        sleep(policy.delay_for(report.consecutive_failures));
    }
    Ok(report)
}

/// 使用凭据加载器维护用户流：每次新建 WebSocket 会话前重新读凭据，允许部署系统原子
/// 替换投影的 key/secret 文件（已有会话不被打断）；加载失败进入既有重连退避。
pub fn run_binance_user_stream_with_config_loader<Stop, Sleep, Event, Load>(
    mut load_auth: Load,
    config: BinanceUserStreamRunConfig,
    should_stop: Stop,
    sleep: Sleep,
    on_event: Event,
) -> Result<BinanceStreamRunReport, String>
where
    Stop: FnMut() -> bool,
    Sleep: FnMut(std::time::Duration),
    Event: FnMut(&str) -> Result<(), String>,
    Load: FnMut() -> Result<BinanceSpotAuth, String>,
{
    let mut request_sequence = 0_u64;
    let ws_host = config.ws_host;
    let ws_port = config.ws_port;
    let request_id_prefix = config.request_id_prefix;
    let timeout = config.timeout;
    let retry_policy = config.retry_policy;
    run_binance_user_stream(
        || {
            request_sequence = request_sequence.saturating_add(1);
            let auth = load_auth()?;
            BinanceSpotUserStream::connect_with_endpoint_port(
                &auth,
                &ws_host,
                ws_port,
                format!("{request_id_prefix}-{request_sequence}"),
                timeout,
            )
        },
        retry_policy,
        should_stop,
        sleep,
        on_event,
    )
}

/// Binance Spot 公共 L1 行情快照与 `bookTicker` 流。
pub struct BinanceSpotMarketData {
    transport: Arc<dyn HttpTransport>,
    host: String,
    port: u16,
    rate_limiter: RateLimiter,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BinanceBalance {
    pub asset: String,
    pub free: Money,
    pub locked: Money,
    pub free_exact: ExactDecimal,
    pub locked_exact: ExactDecimal,
}

impl BinanceSpotMarketData {
    pub fn new(transport: Arc<dyn HttpTransport>) -> Self {
        Self::with_endpoint(transport, DEFAULT_HOST, DEFAULT_PORT)
    }

    pub fn testnet(transport: Arc<dyn HttpTransport>) -> Self {
        Self::with_endpoint(transport, TESTNET_HOST, DEFAULT_PORT)
    }

    pub fn with_endpoint(
        transport: Arc<dyn HttpTransport>,
        host: impl Into<String>,
        port: u16,
    ) -> Self {
        Self {
            transport,
            host: host.into(),
            port,
            rate_limiter: RateLimiter::new(6_000, 6_000, 0),
        }
    }

    pub fn fetch_book_ticker(
        &mut self,
        instrument: &InstrumentId,
        receive_ts: u64,
    ) -> QxResult<QuoteTick> {
        if !self
            .rate_limiter
            .try_acquire(2, receive_ts.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted(
                "Binance 公共行情请求限频".into(),
            ));
        }
        validate_binance_instrument(instrument)?;
        let request = HttpRequest {
            method: "GET".into(),
            host: self.host.clone(),
            port: self.port,
            path: format!(
                "/api/v3/ticker/bookTicker?symbol={}",
                percent_encode(&instrument.symbol.to_ascii_uppercase())
            ),
            body: String::new(),
            headers: BTreeMap::new(),
        };
        let response = self.transport.send(request).map_err(QxError::Ambiguous)?;
        if response.status == 429 || response.status == 418 {
            return Err(QxError::ResourceExhausted(format!(
                "Binance HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        if !(200..300).contains(&response.status) {
            return Err(QxError::Permanent(format!(
                "Binance HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        let (raw_symbol, quote) = parse_book_ticker(&response.body, receive_ts)?;
        if raw_symbol != instrument.symbol.to_ascii_uppercase() {
            return Err(QxError::ReconcileRequired(format!(
                "Binance 行情 symbol 不匹配: expected={}, actual={raw_symbol}",
                instrument.symbol
            )));
        }
        Ok(quote)
    }
}

/// Binance 公共 `bookTicker` 单品种原始 WSS 流。
pub struct BinanceSpotMarketStream {
    stream: TlsWebSocketUserStream,
    instrument: InstrumentId,
}

impl BinanceSpotMarketStream {
    pub fn connect_with_endpoint(
        instrument: InstrumentId,
        host: impl Into<String>,
        port: u16,
        timeout: std::time::Duration,
    ) -> Result<Self, String> {
        validate_binance_instrument(&instrument).map_err(|error| match error {
            QxError::Permanent(message) => message,
            other => format!("Binance instrument 非法: {other:?}"),
        })?;
        let path = format!("/ws/{}@bookTicker", instrument.symbol.to_ascii_lowercase());
        let host = host.into();
        let stream = TlsWebSocketUserStream::connect(host, port, path, timeout)?;
        Ok(Self { stream, instrument })
    }

    pub fn instrument(&self) -> &InstrumentId {
        &self.instrument
    }

    pub fn recv_quote(&mut self, receive_ts: u64) -> Result<BinanceQuotePoll, String> {
        let message = match self.stream.recv_message()? {
            WebSocketPoll::Message(message) => message,
            // 一个窗口里没有行情 ≠ 链路坏了：静默要交给调用方做 staleness 判定，
            // 而不能顺着 `?` 把 worker 判成致命退出。
            WebSocketPoll::Idle => return Ok(BinanceQuotePoll::Idle),
            WebSocketPoll::Closed => return Ok(BinanceQuotePoll::Closed),
        };
        if message.opcode != 0x1 {
            return Err("Binance bookTicker 不是 JSON 文本".into());
        }
        let payload = String::from_utf8(message.payload)
            .map_err(|error| format!("Binance bookTicker 不是 UTF-8: {error}"))?;
        let (symbol, quote) = parse_book_ticker(&payload, receive_ts)
            .map_err(|error| format!("Binance bookTicker 解析失败: {error}"))?;
        if symbol != self.instrument.symbol.to_ascii_uppercase() {
            return Err(format!(
                "Binance bookTicker symbol 不匹配: expected={}, actual={symbol}",
                self.instrument.symbol
            ));
        }
        Ok(BinanceQuotePoll::Quote(quote))
    }

    pub fn close(&mut self) -> Result<(), String> {
        self.stream.close()
    }
}

impl BinanceSpotUserStream {
    pub fn connect_with_endpoint_port(
        auth: &BinanceSpotAuth,
        host: &str,
        port: u16,
        request_id: impl Into<String>,
        timeout: std::time::Duration,
    ) -> Result<Self, String> {
        let mut headers = BTreeMap::new();
        headers.insert("X-MBX-APIKEY".into(), auth.api_key().into());
        let mut stream = TlsWebSocketUserStream::connect_with_headers(
            host,
            port,
            DEFAULT_WS_PATH,
            timeout,
            headers,
        )?;
        stream.send_text(&auth.user_stream_subscribe_payload(request_id))?;
        let acknowledgement = match stream.recv_message()? {
            WebSocketPoll::Message(message) => message,
            WebSocketPoll::Idle => return Err("Binance 用户流订阅回执在读取窗口内未到".into()),
            WebSocketPoll::Closed => return Err("Binance 用户流订阅连接已关闭".into()),
        };
        if acknowledgement.opcode != 0x1 {
            return Err("Binance 用户流订阅回执不是 JSON 文本".into());
        }
        let acknowledgement: Value = serde_json::from_slice(&acknowledgement.payload)
            .map_err(|error| format!("Binance 用户流订阅回执 JSON 无效: {error}"))?;
        if acknowledgement.get("status").and_then(Value::as_u64) != Some(200) {
            return Err(format!("Binance 用户流订阅失败: {acknowledgement}"));
        }
        let subscription_id = acknowledgement
            .get("result")
            .and_then(|result| result.get("subscriptionId"))
            .and_then(Value::as_u64)
            .ok_or_else(|| "Binance 用户流订阅回执缺少 subscriptionId".to_string())?;
        Ok(Self {
            stream,
            subscription_id,
        })
    }

    pub fn subscription_id(&self) -> u64 {
        self.subscription_id
    }

    pub fn recv_event(&mut self) -> Result<BinanceStreamPoll, String> {
        match self.stream.recv_message()? {
            WebSocketPoll::Message(message) => {
                if message.opcode != 0x1 {
                    return Err("Binance 用户流事件不是 JSON 文本".into());
                }
                let payload = String::from_utf8(message.payload)
                    .map_err(|error| format!("Binance 用户流事件不是 UTF-8: {error}"))?;
                Ok(BinanceStreamPoll::Event(payload))
            }
            WebSocketPoll::Idle => Ok(BinanceStreamPoll::Idle),
            WebSocketPoll::Closed => Ok(BinanceStreamPoll::Closed),
        }
    }

    pub fn close(&mut self) -> Result<(), String> {
        self.stream.close()
    }
}

fn form_encode(parameters: &BTreeMap<String, String>) -> String {
    form_encode_pairs(
        &parameters
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>(),
    )
}

fn form_encode_pairs(parameters: &[(String, String)]) -> String {
    parameters
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(*byte as char);
        } else {
            output.push('%');
            output.push_str(&format!("{byte:02X}"));
        }
    }
    output
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceFillWire {
    price: String,
    qty: String,
    commission: String,
    #[serde(rename = "commissionAsset", default)]
    commission_asset: String,
    #[serde(rename = "tradeId", default)]
    trade_id: u64,
    #[serde(rename = "transactTime", default)]
    transact_time: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceOrderWire {
    #[serde(rename = "orderId", default)]
    order_id: u64,
    #[serde(rename = "clientOrderId", default)]
    client_order_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    fills: Vec<BinanceFillWire>,
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceSnapshotWire {
    symbol: String,
    #[serde(rename = "orderId")]
    order_id: u64,
    #[serde(rename = "clientOrderId")]
    client_order_id: String,
    status: String,
    #[serde(rename = "executedQty")]
    executed_qty: String,
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceOpenOrderWire {
    symbol: String,
    #[serde(rename = "orderId")]
    order_id: u64,
    side: String,
    #[serde(rename = "origQty")]
    original_quantity: String,
    #[serde(rename = "executedQty")]
    executed_quantity: String,
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceAccountWire {
    #[serde(default)]
    balances: Vec<BinanceBalanceWire>,
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceBalanceWire {
    asset: String,
    free: String,
    locked: String,
}

#[derive(Clone, Debug)]
struct BinanceSpotMarketRules {
    fetched_at_ms: u64,
    status: String,
    base_asset: String,
    quote_asset: String,
    price_tick: i128,
    qty_step: i128,
    min_qty: i128,
    min_notional: i128,
    notional_filters: Vec<BinanceSpotNotionalFilter>,
    percent_price: Option<BinanceSpotPercentPriceFilter>,
    percent_price_by_side: Option<BinanceSpotPercentPriceBySideFilter>,
    exact_price_tick: ExactDecimal,
    exact_min_price: ExactDecimal,
    exact_max_price: ExactDecimal,
    exact_qty_step: ExactDecimal,
    exact_min_qty: ExactDecimal,
    exact_max_qty: ExactDecimal,
    exact_market_qty_step: Option<ExactDecimal>,
    exact_market_min_qty: Option<ExactDecimal>,
    exact_market_max_qty: Option<ExactDecimal>,
    exact_min_notional: ExactDecimal,
    exact_max_position: Option<ExactDecimal>,
}

#[derive(Clone, Debug)]
struct BinanceSpotNotionalFilter {
    apply_min_to_market: bool,
    apply_max_to_market: bool,
    avg_price_mins: u32,
    exact_min_notional: Option<ExactDecimal>,
    exact_max_notional: Option<ExactDecimal>,
}

#[derive(Clone, Debug)]
struct BinanceSpotPercentPriceFilter {
    multiplier_down: ExactDecimal,
    multiplier_up: ExactDecimal,
    avg_price_mins: u32,
}

#[derive(Clone, Debug)]
struct BinanceSpotPercentPriceBySideFilter {
    bid_multiplier_down: ExactDecimal,
    bid_multiplier_up: ExactDecimal,
    ask_multiplier_down: ExactDecimal,
    ask_multiplier_up: ExactDecimal,
    avg_price_mins: u32,
}

const BINANCE_MARKET_RULES_MAX_AGE_MS: u64 = 60_000;
const BINANCE_REFERENCE_PRICE_MAX_AGE_MS: u64 = 1_000;

/// Binance Spot REST + `executionReport` 用户事件适配器。
pub struct BinanceSpotVenue {
    id: String,
    auth: BinanceSpotAuth,
    transport: Arc<dyn HttpTransport>,
    host: String,
    port: u16,
    connected: bool,
    state: ConnectorState,
    /// 本地订单状态、远端订单号映射与成交去重键共用同一封顶策略：终态订单在超过
    /// `MAX_CACHED_ORDERS + ORDER_EVICT_HYSTERESIS` 后按 client_id 升序退场，派生索引同步级联。
    /// 退场即升级对账：被退订单再收到回报会落到「成交对应订单不存在」→ ReconcileRequired。
    orders: BTreeMap<u64, Order>,
    venue_order_ids: BTreeMap<u64, String>,
    seen_fill_keys: BTreeSet<(u64, u64, i128, u8, i128, u8, i128, u8)>,
    rate_limiter: RateLimiter,
    market_rules: BTreeMap<String, BinanceSpotMarketRules>,
    /// Cached symbol-to-base-asset map used for Binance's account-wide MAX_POSITION filter.
    exchange_base_assets: Option<(u64, BTreeMap<String, String>)>,
    reference_prices: BTreeMap<(String, u32), (u64, ExactDecimal)>,
    market_data: BinanceSpotMarketData,
    balances: BTreeMap<String, BinanceBalance>,
    reconcile_symbols: BTreeSet<String>,
    last_event_ts: u64,
    reconnects: u64,
}

impl OrderLifecycle for BinanceSpotVenue {}

impl BinanceSpotVenue {
    pub fn new(
        id: impl Into<String>,
        auth: BinanceSpotAuth,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self::with_endpoint(id, auth, transport, DEFAULT_HOST, DEFAULT_PORT)
    }

    pub fn testnet(
        id: impl Into<String>,
        auth: BinanceSpotAuth,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self::with_endpoint(id, auth, transport, TESTNET_HOST, DEFAULT_PORT)
    }

    pub fn with_endpoint(
        id: impl Into<String>,
        auth: BinanceSpotAuth,
        transport: Arc<dyn HttpTransport>,
        host: impl Into<String>,
        port: u16,
    ) -> Self {
        let host = host.into();
        Self {
            id: id.into(),
            auth,
            market_data: BinanceSpotMarketData::with_endpoint(
                Arc::clone(&transport),
                host.clone(),
                port,
            ),
            balances: BTreeMap::new(),
            reconcile_symbols: BTreeSet::new(),
            transport,
            host,
            port,
            connected: true,
            state: ConnectorState::Live,
            orders: BTreeMap::new(),
            venue_order_ids: BTreeMap::new(),
            seen_fill_keys: BTreeSet::new(),
            rate_limiter: RateLimiter::new(6_000, 6_000, 0),
            market_rules: BTreeMap::new(),
            exchange_base_assets: None,
            reference_prices: BTreeMap::new(),
            last_event_ts: 0,
            reconnects: 0,
        }
    }

    /// 设置重连对账要覆盖的 Spot symbol 集合。Binance `openOrders` 只返回当前未完成订单，
    /// 恢复不了中断期间已成交/撤销/过期的本地订单；对账 worker 提供自己的标的，连接器改用
    /// `allOrders` 拉全量历史，未配置时退化为 EventLog 的 symbol，无本地订单才用 `openOrders`。
    pub fn set_reconcile_symbols<I, S>(&mut self, symbols: I) -> QxResult<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut normalized = BTreeSet::new();
        for symbol in symbols {
            let symbol = symbol.as_ref().trim().to_ascii_uppercase();
            if symbol.is_empty() || !symbol.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                return Err(QxError::BusinessViolation(format!(
                    "Binance 对账 symbol 非法: {symbol}"
                )));
            }
            normalized.insert(symbol);
        }
        self.reconcile_symbols = normalized;
        Ok(())
    }

    /// 从可恢复的 Kernel/EventLog 重新装载本地订单跟踪状态。
    ///
    /// Binance 用户流本身只携带远端回报，不会携带策略、账户和本地订单
    /// 全量形状；重启后必须先恢复这些订单，再接受 `executionReport`，否则
    /// 未知 `clientOrderId` 会被错误地当作可忽略事件。
    pub fn restore_orders<I>(&mut self, orders: I) -> QxResult<()>
    where
        I: IntoIterator<Item = Order>,
    {
        for order in orders {
            order.validate().map_err(QxError::BusinessViolation)?;
            if let Some(existing) = self.orders.get(&order.client_id) {
                if existing != &order {
                    return Err(QxError::ReconcileRequired(format!(
                        "恢复订单 {} 与连接器本地状态冲突",
                        order.client_id
                    )));
                }
                continue;
            }
            self.orders.insert(order.client_id, order);
        }
        let evicted = evict_stale_terminal_orders(&mut self.orders);
        self.forget_orders(&evicted);
        Ok(())
    }

    /// 用共享 EventLog 的最新订单状态同步已存在的连接器缓存。
    ///
    /// `restore_orders` 用于启动时的严格一致性检查；多进程 UserStream/Reconciler
    /// 在运行中看到其他 worker 追加的 Accepted/Fill 后，必须显式调用本方法，
    /// 不能把本地缓存的旧状态误判成恢复冲突。
    pub fn refresh_orders<I>(&mut self, orders: I) -> QxResult<()>
    where
        I: IntoIterator<Item = Order>,
    {
        for order in orders {
            order.validate().map_err(QxError::BusinessViolation)?;
            self.orders.insert(order.client_id, order);
        }
        let evicted = evict_stale_terminal_orders(&mut self.orders);
        self.forget_orders(&evicted);
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.connected = false;
        self.state = ConnectorState::ReconcileRequired;
    }

    pub fn reconnect(&mut self) {
        self.connected = true;
        self.state = ConnectorState::Snapshotting;
        self.reconnects += 1;
    }

    pub fn fetch_book_ticker(
        &mut self,
        instrument: &InstrumentId,
        receive_ts: u64,
    ) -> QxResult<QuoteTick> {
        self.market_data.fetch_book_ticker(instrument, receive_ts)
    }

    pub fn balances(&self) -> Vec<BinanceBalance> {
        self.balances.values().cloned().collect()
    }

    pub fn fetch_account_balances(&mut self, ts: u64) -> QxResult<Vec<BinanceBalance>> {
        // A failed account read is retryable observation failure, not an unknown order result.
        let response = self.request_readonly_with_weight("/api/v3/account", ts, 20)?;
        let wire: BinanceAccountWire = parse_success(response, "account")?;
        let balances = wire
            .balances
            .into_iter()
            .map(|balance| {
                let free_exact = parse_exact_decimal(&balance.free, "free balance")?;
                let locked_exact = parse_exact_decimal(&balance.locked, "locked balance")?;
                Ok(BinanceBalance {
                    asset: balance.asset,
                    free: Money::from_raw(exact_decimal_projection(free_exact)?),
                    locked: Money::from_raw(exact_decimal_projection(locked_exact)?),
                    free_exact,
                    locked_exact,
                })
            })
            .collect::<QxResult<Vec<_>>>()?;
        self.balances = balances
            .iter()
            .cloned()
            .map(|balance| (balance.asset.clone(), balance))
            .collect();
        Ok(balances)
    }

    fn fetch_open_orders_for_max_position(
        &mut self,
        now_ms: u64,
    ) -> QxResult<Vec<BinanceOpenOrderWire>> {
        let response = self.request_readonly_with_weight("/api/v3/openOrders", now_ms, 80)?;
        parse_success(response, "MAX_POSITION openOrders")
    }

    /// 解析并应用 Binance Spot `executionReport`/listenKeyExpired 用户事件。
    /// 非订单类账户事件返回空列表，由上层账户快照流处理。
    pub fn ingest_user_event(&mut self, payload: &str) -> QxResult<Vec<VenueEvent>> {
        let value: Value = serde_json::from_str(payload)
            .map_err(|error| QxError::Permanent(format!("Binance 用户事件 JSON 非法: {error}")))?;
        let event_type = value.get("e").and_then(Value::as_str).unwrap_or_default();
        if event_type == "listenKeyExpired" {
            self.disconnect();
            return Err(QxError::ReconcileRequired(
                "Binance listenKey 已过期，必须重新建立用户流并对账".into(),
            ));
        }
        if event_type != "executionReport" {
            if event_type == "outboundAccountPosition" {
                self.ingest_account_position(&value)?;
            }
            return Ok(Vec::new());
        }
        let client_order_id = value
            .get("c")
            .and_then(Value::as_str)
            .and_then(parse_client_order_id)
            .ok_or_else(|| {
                QxError::ReconcileRequired("Binance 回报缺少可映射 clientOrderId".into())
            })?;
        let venue_order_id = value
            .get("i")
            .and_then(Value::as_u64)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 回报缺少 orderId".into()))?
            .to_string();
        let event_ts = value
            .get("E")
            .and_then(Value::as_u64)
            .or_else(|| value.get("T").and_then(Value::as_u64))
            .unwrap_or(self.last_event_ts);
        self.last_event_ts = self.last_event_ts.max(event_ts);
        self.bind_remote_order(client_order_id, venue_order_id.clone())?;
        let mut events = Vec::new();
        if value.get("x").and_then(Value::as_str) == Some("TRADE") {
            let fill = BinanceFillWire {
                price: json_string(&value, "L")?,
                qty: json_string(&value, "l")?,
                commission: json_string(&value, "n")?,
                commission_asset: value
                    .get("N")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                trade_id: value.get("t").and_then(Value::as_u64).ok_or_else(|| {
                    QxError::ReconcileRequired("Binance 回报缺少 trade_id(t)".into())
                })?,
                transact_time: value.get("T").and_then(Value::as_u64).unwrap_or(event_ts),
            };
            if let Some(event) = self.ingest_fill_wire(client_order_id, &venue_order_id, fill)? {
                events.push(event);
            }
        }
        match value.get("X").and_then(Value::as_str).unwrap_or_default() {
            "NEW" if events.is_empty() => {
                let order = self
                    .orders
                    .get_mut(&client_order_id)
                    .ok_or(QxError::Invariant("accepted order disappeared".into()))?;
                if matches!(
                    order.status,
                    OrderStatus::Submitted | OrderStatus::PendingSubmit
                ) {
                    order
                        .status
                        .transition(OrderStatus::Accepted)
                        .map_err(QxError::Invariant)?;
                    events.push(VenueEvent::Accepted {
                        client_order_id,
                        venue_order_id,
                        ts: event_ts,
                    });
                }
            }
            "CANCELED" => {
                if self.mark_cancelled(client_order_id)? {
                    events.push(VenueEvent::Cancelled {
                        client_order_id,
                        ts: event_ts,
                    });
                }
            }
            "FILLED" => self.mark_remote_filled(client_order_id)?,
            "EXPIRED" | "REJECTED" => {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(format!(
                    "Binance 订单 {} 进入 {}，需要上层处理终态事实",
                    client_order_id,
                    value.get("X").and_then(Value::as_str).unwrap_or_default()
                )));
            }
            _ => {}
        }
        Ok(events)
    }

    fn mark_remote_filled(&mut self, client_order_id: u64) -> QxResult<()> {
        let order = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("Binance FILLED 订单本地不存在".into()))?;
        let complete = if let Some(amounts) = order.exact_amounts {
            if amounts.quote_budget.is_some() {
                amounts.filled.is_positive() && amounts.quote_filled.is_positive()
            } else {
                amounts.filled == amounts.quantity
            }
        } else {
            order.filled == order.qty
        };
        if !complete {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance 已报告 FILLED，但本地成交事实未覆盖订单数量".into(),
            ));
        }
        if order.status != OrderStatus::Filled {
            order
                .status
                .transition(OrderStatus::Filled)
                .map_err(QxError::Invariant)?;
        }
        Ok(())
    }

    pub fn fetch_remote_snapshot(&mut self) -> QxResult<Vec<VenueOrderSnapshot>> {
        qx_zhenlu::venue_capabilities::BINANCE_SPOT_VENUE_CAPABILITIES.require_query()?;
        let mut symbols = self.reconcile_symbols.clone();
        if symbols.is_empty() {
            symbols = self
                .orders
                .values()
                .map(|order| order.instrument.symbol.to_ascii_uppercase())
                .collect();
        }
        let mut wires = Vec::new();
        if symbols.is_empty() {
            let response = self.request_with_weight(
                "GET",
                "/api/v3/openOrders",
                BTreeMap::new(),
                self.last_event_ts,
                80,
            )?;
            wires = parse_success(response, "openOrders")?;
        } else {
            for symbol in symbols {
                let mut next_order_id: Option<u64> = None;
                let mut previous_last_order_id: Option<u64> = None;
                loop {
                    let mut parameters = BTreeMap::new();
                    parameters.insert("limit".into(), "1000".into());
                    parameters.insert("symbol".into(), symbol.clone());
                    if let Some(order_id) = next_order_id {
                        parameters.insert("orderId".into(), order_id.to_string());
                    }
                    let response = self.request_with_weight(
                        "GET",
                        "/api/v3/allOrders",
                        parameters,
                        self.last_event_ts,
                        20,
                    )?;
                    let mut page: Vec<BinanceSnapshotWire> = parse_success(response, "allOrders")?;
                    let page_len = page.len();
                    let last_order_id = page.last().map(|order| order.order_id);
                    wires.append(&mut page);
                    if page_len < 1_000 {
                        break;
                    }
                    let last_order_id = last_order_id.ok_or_else(|| {
                        QxError::ReconcileRequired("Binance allOrders 分页返回空 orderId".into())
                    })?;
                    if last_order_id == 0 || previous_last_order_id == Some(last_order_id) {
                        return Err(QxError::ReconcileRequired(
                            "Binance allOrders 分页游标未前进".into(),
                        ));
                    }
                    previous_last_order_id = Some(last_order_id);
                    next_order_id = Some(last_order_id.checked_add(1).ok_or_else(|| {
                        QxError::ReconcileRequired("Binance orderId 分页游标溢出".into())
                    })?);
                }
            }
        }
        wires
            .into_iter()
            .map(|wire| {
                let client_order_id =
                    parse_client_order_id(&wire.client_order_id).ok_or_else(|| {
                        QxError::ReconcileRequired(format!(
                            "Binance 快照包含无法映射的 clientOrderId: {}",
                            wire.client_order_id
                        ))
                    })?;
                if wire.order_id == 0 {
                    return Err(QxError::ReconcileRequired(
                        "Binance 快照缺少有效 orderId".into(),
                    ));
                }
                let remote_order_id = wire.order_id.to_string();
                if self
                    .venue_order_ids
                    .insert(client_order_id, remote_order_id.clone())
                    .is_some_and(|existing| existing != remote_order_id)
                {
                    return Err(QxError::ReconcileRequired(format!(
                        "Binance 快照中的 clientOrderId {} 对应多个 orderId",
                        client_order_id
                    )));
                }
                let _ = wire.symbol; // 归属只认 clientOrderId（`qx-<u64>` 全局唯一）；symbol 是交易所回显，显式丢弃以保持该字段被读
                let (filled, filled_exact) =
                    parse_exact_quantity_projection(&wire.executed_qty, "executedQty")?;
                Ok(VenueOrderSnapshot {
                    client_order_id,
                    status: map_status(&wire.status)?,
                    filled,
                    filled_exact: Some(filled_exact),
                })
            })
            .collect()
    }

    pub fn reconcile_remote(
        &mut self,
        remote: &[VenueOrderSnapshot],
    ) -> QxResult<Vec<super::AdapterReconcileIssue>> {
        qx_zhenlu::venue_capabilities::BINANCE_SPOT_VENUE_CAPABILITIES.require_reconciliation()?;
        if !self.connected || !matches!(self.state, ConnectorState::Snapshotting) {
            return Err(QxError::VenueState("Binance Spot 不在重连对账阶段".into()));
        }
        let remote = remote.iter().try_fold(BTreeMap::new(), |mut map, order| {
            if map.insert(order.client_order_id, order).is_some() {
                return Err(QxError::ReconcileRequired(
                    "Binance 快照包含重复 client_order_id".into(),
                ));
            }
            Ok(map)
        })?;
        // venue 专属过滤：本地终态订单不参与对账（事件日志已收口，无需再向柜台核对）。
        let local_facts: Vec<_> = self
            .orders
            .iter()
            .filter(|(_, order)| !order.status.is_terminal())
            .map(|(id, order)| super::reconcile::local_order_fact(id, order))
            .collect();
        let mut issues = super::reconcile::reconcile_issues(
            &local_facts,
            &super::reconcile::remote_order_facts(&remote),
        );
        // venue 专属过滤：`allOrders` 会返回账户历史终态单，或本地已以终态收口的订单，
        // 它们都不属于本 worker 待核对的活跃订单，不应把连接器永久钉在 ReconcileRequired。
        issues.retain(|issue| match issue {
            super::AdapterReconcileIssue::MissingLocally { client_order_id } => {
                !self.orders.contains_key(client_order_id)
                    && !remote
                        .get(client_order_id)
                        .is_some_and(|order| order.status.is_terminal())
            }
            _ => true,
        });
        self.state = if issues.is_empty() {
            ConnectorState::Live
        } else {
            ConnectorState::ReconcileRequired
        };
        Ok(issues)
    }

    pub fn fetch_and_reconcile(&mut self) -> QxResult<Vec<super::AdapterReconcileIssue>> {
        let remote = self.fetch_remote_snapshot()?;
        self.reconcile_remote(&remote)
    }

    /// 封顶退场后级联清派生索引：远端订单号与成交去重键必须跟着退场订单一起消失。
    fn forget_orders(&mut self, evicted: &BTreeSet<u64>) {
        if evicted.is_empty() {
            return;
        }
        self.venue_order_ids
            .retain(|client_id, _| !evicted.contains(client_id));
        self.seen_fill_keys.retain(|key| !evicted.contains(&key.0));
    }

    fn bind_remote_order(&mut self, client_order_id: u64, venue_order_id: String) -> QxResult<()> {
        if !self.orders.contains_key(&client_order_id) {
            return Err(QxError::ReconcileRequired(format!(
                "Binance 用户回报对应未知本地订单 {}",
                client_order_id
            )));
        }
        self.venue_order_ids.insert(client_order_id, venue_order_id);
        Ok(())
    }

    fn ingest_fill_wire(
        &mut self,
        client_order_id: u64,
        venue_order_id: &str,
        wire: BinanceFillWire,
    ) -> QxResult<Option<VenueEvent>> {
        let quantity_exact = parse_exact_decimal(&wire.qty, "fill qty")?;
        let price_exact = parse_exact_decimal(&wire.price, "fill price")?;
        let fee_exact = parse_exact_decimal(&wire.commission, "commission")?;
        let fee_currency = (!wire.commission_asset.trim().is_empty())
            .then(|| wire.commission_asset.trim().to_ascii_uppercase());
        if fee_exact.is_negative() {
            return Err(QxError::ReconcileRequired(
                "Binance 成交手续费不能为负数".into(),
            ));
        }
        if !fee_exact.is_zero() && fee_currency.is_none() {
            return Err(QxError::ReconcileRequired(
                "Binance 非零成交手续费缺少 commissionAsset".into(),
            ));
        }
        let is_exact_order = self
            .orders
            .get(&client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 成交对应订单不存在".into()))?
            .exact_amounts
            .is_some();
        let (qty, price, fee) = if is_exact_order {
            (
                Quantity::from_raw(exact_decimal_projection(quantity_exact)?),
                Price::from_raw(exact_decimal_projection(price_exact)?),
                Money::from_raw(exact_decimal_projection(fee_exact)?),
            )
        } else {
            (
                parse_quantity(&wire.qty, "fill qty")?,
                parse_price(&wire.price, "fill price")?,
                parse_money(&wire.commission, "commission")?,
            )
        };
        let key = (
            client_order_id,
            wire.trade_id,
            quantity_exact.coefficient(),
            quantity_exact.scale(),
            price_exact.coefficient(),
            price_exact.scale(),
            fee_exact.coefficient(),
            fee_exact.scale(),
        );
        if Self::fill_seen(&self.seen_fill_keys, &key) {
            return Ok(None);
        }
        let order = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 成交对应订单不存在".into()))?;
        if order.status.is_terminal() {
            // 本地订单已终态（撤单/成完/拒绝），远端却又推来成交：这是真实的状态分歧，
            // 只能按未知结果交给对账，绝不能伪造一条成交事实把终态订单改写。
            return Err(QxError::ReconcileRequired(format!(
                "Binance 订单 {} 本地已终态 {:?} 但远端仍有成交回报",
                client_order_id, order.status
            )));
        }
        if !quantity_exact.is_positive() || !price_exact.is_positive() {
            return Err(QxError::ReconcileRequired(
                "Binance 成交数量和价格必须为正数".into(),
            ));
        }
        let quote_cost = quantity_exact
            .checked_mul_at_scale(
                price_exact,
                ExactDecimal::MAX_SCALE,
                if order.side == Side::Buy {
                    DecimalRounding::AwayFromZero
                } else {
                    DecimalRounding::TowardZero
                },
            )
            .ok_or_else(|| QxError::ReconcileRequired("Binance 精确成交计价金额溢出".into()))?;
        if let Some(mut amounts) = order.exact_amounts {
            if let Some(budget) = amounts.quote_budget {
                let cumulative_quote =
                    amounts
                        .quote_filled
                        .checked_add(quote_cost)
                        .ok_or_else(|| {
                            QxError::ReconcileRequired("Binance quoteOrderQty 累计溢出".into())
                        })?;
                if cumulative_quote
                    .checked_cmp(budget)
                    .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err(QxError::ReconcileRequired(
                        "Binance quoteOrderQty 成交花费超过委托预算".into(),
                    ));
                }
                amounts.quote_filled = cumulative_quote;
            } else {
                let cumulative = amounts.filled.checked_add(quantity_exact).ok_or_else(|| {
                    QxError::ReconcileRequired("Binance 精确累计成交量溢出".into())
                })?;
                if cumulative
                    .checked_cmp(amounts.quantity)
                    .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err(QxError::ReconcileRequired(
                        "Binance 成交数量超过本地订单剩余量".into(),
                    ));
                }
                amounts.filled = cumulative;
            }
            order.exact_amounts = Some(amounts);
        } else if qty.raw() <= 0 || qty.raw() > order.remaining().raw() {
            // 回报越界时不占用去重键：否则一次虚假回报会让该笔成交在对账后永远无法
            // 被重新接受（去重键只在回报真正落到本地状态后才登记）。
            return Err(QxError::ReconcileRequired(
                "Binance 成交数量超过本地订单剩余量".into(),
            ));
        }
        if matches!(order.status, OrderStatus::Submitted | OrderStatus::Accepted) {
            order
                .status
                .transition(OrderStatus::Working)
                .map_err(QxError::Invariant)?;
        }
        let next_status = if let Some(amounts) = order.exact_amounts {
            if amounts.quote_budget.is_some() {
                OrderStatus::PartiallyFilled
            } else if amounts.filled == amounts.quantity {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            }
        } else {
            order.filled = Quantity::from_raw(order.filled.raw() + qty.raw());
            if order.filled == order.qty {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            }
        };
        if !(order.status == OrderStatus::CancelPending
            && next_status == OrderStatus::PartiallyFilled)
        {
            order
                .status
                .transition(next_status)
                .map_err(QxError::Invariant)?;
        }
        let mut fill = Fill {
            order_id: client_order_id,
            qty: if is_exact_order { Quantity::ZERO } else { qty },
            price: if is_exact_order { Price::ZERO } else { price },
            fee: if is_exact_order { Money::ZERO } else { fee },
            ts: wire.transact_time,
            account_id: order.account_id.clone(),
            fee_currency: fee_currency.clone(),
            fees: (!is_exact_order && fee.raw() != 0)
                .then(|| FillFee {
                    amount: fee,
                    currency: fee_currency.clone(),
                    source_trade_ids: Vec::new(),
                })
                .into_iter()
                .collect(),
            ..Fill::default()
        };
        if is_exact_order {
            fill.exact_amounts = Some(ExactFillAmounts {
                quantity: quantity_exact,
                price: price_exact,
                quote_cost: quote_cost.is_positive().then_some(quote_cost),
                quote_budget_complete: false,
            });
            if !fee_exact.is_zero() {
                fill.exact_fees.push(ExactFillFee {
                    amount: fee_exact,
                    currency: fee_currency.clone(),
                    source_trade_ids: vec![wire.trade_id.to_string()],
                });
            }
        }
        order.trace_fill(&mut fill, Some(&self.id), Some(venue_order_id));
        self.last_event_ts = self.last_event_ts.max(fill.ts);
        Self::remember_fill(&mut self.seen_fill_keys, key);
        Ok(Some(VenueEvent::Fill(fill)))
    }

    /// 应用远端撤单回报，返回本地是否真的推进到 `Cancelled`：重复回报返回 `false`（幂等）；
    /// 本地已因成交/拒绝进终态时返回错误（真实分歧，只能交对账，绝不能把终态改写）。
    fn mark_cancelled(&mut self, client_order_id: u64) -> QxResult<bool> {
        let order = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 取消回报对应订单不存在".into()))?;
        if order.status == OrderStatus::Cancelled {
            return Ok(false);
        }
        if order.status.is_terminal() {
            return Err(QxError::ReconcileRequired(format!(
                "Binance 订单 {} 本地已终态 {:?} 但远端回报撤单",
                client_order_id, order.status
            )));
        }
        if !matches!(order.status, OrderStatus::CancelPending) {
            order
                .status
                .transition(OrderStatus::CancelPending)
                .map_err(QxError::Invariant)?;
        }
        order
            .status
            .transition(OrderStatus::Cancelled)
            .map_err(QxError::Invariant)?;
        Ok(true)
    }

    fn request(
        &mut self,
        method: &str,
        path: &str,
        parameters: BTreeMap<String, String>,
        now_ms: u64,
    ) -> QxResult<HttpResponse> {
        self.request_with_weight(method, path, parameters, now_ms, 1)
    }

    fn request_with_weight(
        &mut self,
        method: &str,
        path: &str,
        parameters: BTreeMap<String, String>,
        now_ms: u64,
        weight: u64,
    ) -> QxResult<HttpResponse> {
        if !self.connected {
            return Err(QxError::Ambiguous("Binance Spot 连接中断".into()));
        }
        if !self
            .rate_limiter
            .try_acquire(weight, now_ms.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted("Binance REST 请求限频".into()));
        }
        let request = self
            .auth
            .signed_request(method, &self.host, self.port, path, parameters);
        match self.transport.send(request) {
            Ok(response) if (200..300).contains(&response.status) => Ok(response),
            Ok(response) if response.status == 429 || response.status == 418 => {
                Err(QxError::ResourceExhausted(format!(
                    "Binance HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )))
            }
            Ok(response) if response.status >= 500 => {
                let error = QxError::Ambiguous(format!(
                    "Binance HTTP {}，订单结果未知: {}",
                    response.status,
                    error_message(&response.body)
                ));
                if Self::reconciliation_trigger(&error) {
                    self.state = ConnectorState::ReconcileRequired;
                }
                Err(error)
            }
            Ok(response) => Err(QxError::Permanent(format!(
                "Binance HTTP {}: {}",
                response.status,
                error_message(&response.body)
            ))),
            Err(error) => {
                let error = QxError::Ambiguous(error);
                if Self::reconciliation_trigger(&error) {
                    self.state = ConnectorState::ReconcileRequired;
                }
                Err(error)
            }
        }
    }

    /// Signed read-only REST call whose failures must not put the order connector into an
    /// ambiguous-submit state. It is used for balance/open-order observations before a write.
    fn request_readonly_with_weight(
        &mut self,
        path: &str,
        now_ms: u64,
        weight: u64,
    ) -> QxResult<HttpResponse> {
        if !self.connected {
            return Err(QxError::VenueState(
                "Binance Spot 连接中断，无法读取账户状态".into(),
            ));
        }
        if !self
            .rate_limiter
            .try_acquire(weight, now_ms.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted("Binance REST 请求限频".into()));
        }
        let request = self
            .auth
            .signed_request("GET", &self.host, self.port, path, BTreeMap::new());
        match self.transport.send(request) {
            Ok(response) if (200..300).contains(&response.status) => Ok(response),
            Ok(response) if response.status == 429 || response.status == 418 => {
                Err(QxError::ResourceExhausted(format!(
                    "Binance HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )))
            }
            Ok(response) if response.status >= 500 => Err(QxError::Transient(format!(
                "Binance 只读账户请求 HTTP {}: {}",
                response.status,
                error_message(&response.body)
            ))),
            Ok(response) => Err(QxError::Permanent(format!(
                "Binance HTTP {}: {}",
                response.status,
                error_message(&response.body)
            ))),
            Err(error) => Err(QxError::Transient(format!(
                "Binance 只读账户请求失败: {error}"
            ))),
        }
    }

    fn current_spot_base_assets(&mut self, now_ms: u64) -> QxResult<BTreeMap<String, String>> {
        if let Some((fetched_at_ms, cached)) = &self.exchange_base_assets {
            if now_ms.saturating_sub(*fetched_at_ms) <= BINANCE_MARKET_RULES_MAX_AGE_MS {
                return Ok(cached.clone());
            }
        }
        if !self
            .rate_limiter
            .try_acquire(20, now_ms.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted(
                "Binance exchangeInfo 公共请求限频".into(),
            ));
        }
        let response = self
            .transport
            .send(HttpRequest {
                method: "GET".into(),
                host: self.host.clone(),
                port: self.port,
                path: "/api/v3/exchangeInfo".into(),
                body: String::new(),
                headers: BTreeMap::new(),
            })
            .map_err(|error| {
                QxError::Transient(format!("Binance exchangeInfo 公共请求失败: {error}"))
            })?;
        if response.status == 429 || response.status == 418 {
            return Err(QxError::ResourceExhausted(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        if response.status >= 500 {
            return Err(QxError::Transient(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        if !(200..300).contains(&response.status) {
            return Err(QxError::Permanent(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        let base_assets = parse_exchange_base_assets(&response.body)?;
        self.exchange_base_assets = Some((now_ms, base_assets.clone()));
        Ok(base_assets)
    }

    fn current_spot_market_rules(&mut self, symbol: &str) -> QxResult<BinanceSpotMarketRules> {
        let symbol = symbol.to_ascii_uppercase();
        if symbol.is_empty() || !symbol.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return Err(QxError::BusinessViolation(format!(
                "Binance Spot symbol 非法: {symbol}"
            )));
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if let Some(rules) = self.market_rules.get(&symbol) {
            if now_ms.saturating_sub(rules.fetched_at_ms) <= BINANCE_MARKET_RULES_MAX_AGE_MS {
                return Ok(rules.clone());
            }
        }
        if !self
            .rate_limiter
            .try_acquire(20, now_ms.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted(
                "Binance exchangeInfo 公共请求限频".into(),
            ));
        }

        // Symbol is restricted to ASCII alphanumerics above, so it can be inserted directly
        // into this public query without credentials, signatures, or user-controlled separators.
        let request = HttpRequest {
            method: "GET".into(),
            host: self.host.clone(),
            port: self.port,
            path: format!("/api/v3/exchangeInfo?symbol={symbol}"),
            body: String::new(),
            headers: BTreeMap::new(),
        };
        let response = self.transport.send(request).map_err(|error| {
            QxError::Transient(format!("Binance exchangeInfo 公共请求失败: {error}"))
        })?;
        if response.status == 429 || response.status == 418 {
            return Err(QxError::ResourceExhausted(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        if response.status >= 500 {
            return Err(QxError::Transient(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        if !(200..300).contains(&response.status) {
            return Err(QxError::Permanent(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        let rules = parse_spot_market_rules(&response.body, &symbol, now_ms)?;
        self.market_rules.insert(symbol, rules.clone());
        Ok(rules)
    }

    /// Fetch the exact reference window required by an exchange filter. `/avgPrice` only
    /// certifies the window it returns, so a mismatch fails closed instead of reusing a
    /// superficially recent but semantically different price.
    fn current_spot_reference_exact_price(
        &mut self,
        symbol: &str,
        avg_price_mins: u32,
    ) -> QxResult<ExactDecimal> {
        let symbol = symbol.to_ascii_uppercase();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let key = (symbol.clone(), avg_price_mins);
        if let Some((fetched_at_ms, price)) = self.reference_prices.get(&key) {
            if now_ms.saturating_sub(*fetched_at_ms) <= BINANCE_REFERENCE_PRICE_MAX_AGE_MS {
                return Ok(*price);
            }
        }
        let reference_path = format!("/api/v3/referencePrice?symbol={symbol}");
        if !self
            .rate_limiter
            .try_acquire(2, now_ms.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted(
                "Binance Spot referencePrice 请求限频".into(),
            ));
        }
        let response = self
            .transport
            .send(HttpRequest {
                method: "GET".into(),
                host: self.host.clone(),
                port: self.port,
                path: reference_path,
                body: String::new(),
                headers: BTreeMap::new(),
            })
            .map_err(|error| {
                QxError::Transient(format!("Binance Spot referencePrice 请求失败: {error}"))
            })?;
        let mut reference_price = None;
        if (200..300).contains(&response.status) {
            let value: Value = serde_json::from_str(&response.body).map_err(|error| {
                QxError::Transient(format!("Binance Spot referencePrice JSON 非法: {error}"))
            })?;
            let actual_symbol = value
                .get("symbol")
                .and_then(Value::as_str)
                .ok_or_else(|| QxError::Transient("Binance referencePrice 缺少 symbol".into()))?;
            if actual_symbol != symbol {
                return Err(QxError::ReconcileRequired(format!(
                    "Binance referencePrice symbol 不匹配: expected={symbol}, actual={actual_symbol}"
                )));
            }
            if let Some(value) = value.get("referencePrice").filter(|value| !value.is_null()) {
                let price = value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_number().map(ToString::to_string))
                    .ok_or_else(|| {
                        QxError::Transient("Binance referencePrice 字段不是十进制值".into())
                    })?;
                let price = parse_exact_decimal(&price, "referencePrice")?;
                if !price.is_positive() {
                    return Err(QxError::Transient(
                        "Binance referencePrice 必须为正数或 null".into(),
                    ));
                }
                reference_price = Some(price);
            }
        } else if response.status == 429 || response.status == 418 {
            return Err(QxError::ResourceExhausted(format!(
                "Binance Spot referencePrice HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        } else if response.status >= 500 {
            return Err(QxError::Transient(format!(
                "Binance Spot referencePrice HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        } else {
            let error_value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
            if error_value.get("code").and_then(Value::as_i64) != Some(-2043) {
                return Err(QxError::Permanent(format!(
                    "Binance Spot referencePrice HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )));
            }
        }
        let price = if let Some(reference_price) = reference_price {
            reference_price
        } else {
            if !self
                .rate_limiter
                .try_acquire(1, now_ms.saturating_mul(1_000_000))
            {
                return Err(QxError::ResourceExhausted(
                    "Binance Spot 均价回退请求限频".into(),
                ));
            }
            let endpoint = if avg_price_mins == 0 {
                "/api/v3/ticker/price"
            } else {
                "/api/v3/avgPrice"
            };
            let response = self
                .transport
                .send(HttpRequest {
                    method: "GET".into(),
                    host: self.host.clone(),
                    port: self.port,
                    path: format!("{endpoint}?symbol={symbol}"),
                    body: String::new(),
                    headers: BTreeMap::new(),
                })
                .map_err(|error| {
                    QxError::Transient(format!("Binance Spot 均价回退请求失败: {error}"))
                })?;
            if response.status == 429 || response.status == 418 {
                return Err(QxError::ResourceExhausted(format!(
                    "Binance Spot 均价回退 HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )));
            }
            if response.status >= 500 {
                return Err(QxError::Transient(format!(
                    "Binance Spot 均价回退 HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )));
            }
            if !(200..300).contains(&response.status) {
                return Err(QxError::Permanent(format!(
                    "Binance Spot 均价回退 HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )));
            }
            let value: Value = serde_json::from_str(&response.body).map_err(|error| {
                QxError::Transient(format!("Binance Spot 均价回退 JSON 非法: {error}"))
            })?;
            if avg_price_mins == 0 {
                let actual_symbol = value
                    .get("symbol")
                    .and_then(Value::as_str)
                    .ok_or_else(|| QxError::Transient("Binance ticker/price 缺少 symbol".into()))?;
                if actual_symbol != symbol {
                    return Err(QxError::ReconcileRequired(format!(
                        "Binance ticker/price symbol 不匹配: expected={symbol}, actual={actual_symbol}"
                    )));
                }
            } else {
                let actual_mins = value
                    .get("mins")
                    .and_then(Value::as_u64)
                    .and_then(|mins| u32::try_from(mins).ok())
                    .ok_or_else(|| QxError::Transient("Binance avgPrice 缺少有效 mins".into()))?;
                if actual_mins != avg_price_mins {
                    return Err(QxError::Transient(format!(
                        "Binance avgPrice 窗口为 {actual_mins} 分钟，但交易过滤器要求 {avg_price_mins} 分钟；拒绝使用不匹配的参考价"
                    )));
                }
            }
            let price = json_string(&value, "price")?;
            parse_exact_decimal(&price, "filter average price")?
        };
        if !price.is_positive() {
            return Err(QxError::Transient(
                "Binance Spot 过滤器参考价必须为正数".into(),
            ));
        }
        self.reference_prices.insert(key, (now_ms, price));
        Ok(price)
    }

    fn validate_percent_price_filters(
        &mut self,
        rules: &BinanceSpotMarketRules,
        symbol: &str,
        side: Side,
        price: ExactDecimal,
    ) -> QxResult<()> {
        if let Some(filter) = &rules.percent_price {
            let reference =
                self.current_spot_reference_exact_price(symbol, filter.avg_price_mins)?;
            validate_binance_percent_bounds(
                price,
                reference,
                filter.multiplier_down,
                filter.multiplier_up,
                "PERCENT_PRICE",
            )?;
        }
        if let Some(filter) = &rules.percent_price_by_side {
            let reference =
                self.current_spot_reference_exact_price(symbol, filter.avg_price_mins)?;
            let (down, up) = match side {
                Side::Buy => (filter.bid_multiplier_down, filter.bid_multiplier_up),
                Side::Sell => (filter.ask_multiplier_down, filter.ask_multiplier_up),
            };
            validate_binance_percent_bounds(price, reference, down, up, "PERCENT_PRICE_BY_SIDE")?;
        }
        Ok(())
    }

    fn validate_max_position_filter(
        &mut self,
        rules: &BinanceSpotMarketRules,
        order: &Order,
    ) -> QxResult<()> {
        let Some(maximum) = rules.exact_max_position else {
            return Ok(());
        };
        if order.side != Side::Buy {
            return Ok(());
        }
        if order.quote_budget_decimal().is_some() {
            return Err(QxError::BusinessViolation(
                "Binance MAX_POSITION 存在时无法仅凭 quoteOrderQty 证明买入基础币数量不会超限"
                    .into(),
            ));
        }

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        // Read open orders first and the account balance second. If one of the observed buys
        // fills between the two snapshots, the later balance includes the fill while the earlier
        // open-order snapshot still reserves its remaining quantity, which is conservative.
        let open_orders = self.fetch_open_orders_for_max_position(now_ms)?;
        let base_assets = self.current_spot_base_assets(now_ms)?;
        let balances = self.fetch_account_balances(now_ms)?;
        let base_asset = rules.base_asset.trim().to_ascii_uppercase();
        let balance = balances
            .iter()
            .find(|balance| balance.asset.eq_ignore_ascii_case(&base_asset));
        let mut current_position = balance
            .map(|balance| balance.free_exact.checked_add(balance.locked_exact))
            .unwrap_or(Some(ExactDecimal::ZERO))
            .ok_or_else(|| {
                QxError::ReconcileRequired("Binance MAX_POSITION 余额合计溢出".into())
            })?;
        if current_position.is_negative() {
            return Err(QxError::ReconcileRequired(
                "Binance MAX_POSITION 账户余额为负".into(),
            ));
        }

        let mut seen_orders = BTreeSet::new();
        for open in open_orders {
            let symbol = open.symbol.trim().to_ascii_uppercase();
            if symbol.is_empty() || !symbol.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                return Err(QxError::ReconcileRequired(
                    "Binance openOrders 包含非法 symbol，无法校验 MAX_POSITION".into(),
                ));
            }
            if open.order_id == 0 || !seen_orders.insert((symbol.clone(), open.order_id)) {
                return Err(QxError::ReconcileRequired(
                    "Binance openOrders 包含无效或重复 orderId".into(),
                ));
            }
            if !open.side.eq_ignore_ascii_case("BUY") {
                if !open.side.eq_ignore_ascii_case("SELL") {
                    return Err(QxError::ReconcileRequired(format!(
                        "Binance open order {} side 非法",
                        open.order_id
                    )));
                }
                continue;
            }
            let order_base = base_assets.get(&symbol).ok_or_else(|| {
                QxError::ReconcileRequired(format!(
                    "Binance exchangeInfo 缺少 open order {symbol} 的 baseAsset，无法校验 MAX_POSITION"
                ))
            })?;
            if !order_base.eq_ignore_ascii_case(&base_asset) {
                continue;
            }
            let original = parse_exact_decimal(&open.original_quantity, "open order origQty")?;
            let executed = parse_exact_decimal(&open.executed_quantity, "open order executedQty")?;
            if !original.is_positive()
                || executed.is_negative()
                || executed
                    .checked_cmp(original)
                    .is_none_or(|ordering| ordering.is_gt())
            {
                return Err(QxError::ReconcileRequired(format!(
                    "Binance open order {} 数量不一致，无法校验 MAX_POSITION",
                    open.order_id
                )));
            }
            let remaining = original.checked_sub(executed).ok_or_else(|| {
                QxError::ReconcileRequired("Binance MAX_POSITION 在途买单数量溢出".into())
            })?;
            current_position = current_position.checked_add(remaining).ok_or_else(|| {
                QxError::ReconcileRequired("Binance MAX_POSITION 在途买单合计溢出".into())
            })?;
        }

        let projected = current_position
            .checked_add(order.quantity_decimal())
            .ok_or_else(|| QxError::BusinessViolation("Binance MAX_POSITION 投影溢出".into()))?;
        if projected
            .checked_cmp(maximum)
            .is_none_or(|ordering| ordering.is_gt())
        {
            return Err(QxError::BusinessViolation(format!(
                "Binance MAX_POSITION 超限: 当前余额与全部交易对未成交买单加本单为 {projected}，上限为 {maximum} {base_asset}"
            )));
        }
        Ok(())
    }

    fn ingest_account_position(&mut self, value: &Value) -> QxResult<()> {
        let event_ts = value
            .get("E")
            .and_then(Value::as_u64)
            .or_else(|| value.get("u").and_then(Value::as_u64))
            .unwrap_or(self.last_event_ts);
        let balances = value
            .get("B")
            .and_then(Value::as_array)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 账户事件缺少 B balances".into()))?;
        for balance in balances {
            let asset = balance
                .get("a")
                .and_then(Value::as_str)
                .ok_or_else(|| QxError::ReconcileRequired("Binance 账户事件缺少 asset".into()))?;
            let free = json_string(balance, "f")?;
            let locked = json_string(balance, "l")?;
            let free_exact = parse_exact_decimal(&free, "event free balance")?;
            let locked_exact = parse_exact_decimal(&locked, "event locked balance")?;
            let snapshot = BinanceBalance {
                asset: asset.into(),
                free: Money::from_raw(exact_decimal_projection(free_exact)?),
                locked: Money::from_raw(exact_decimal_projection(locked_exact)?),
                free_exact,
                locked_exact,
            };
            self.balances.insert(asset.into(), snapshot);
        }
        self.last_event_ts = self.last_event_ts.max(event_ts);
        Ok(())
    }
}

impl Venue for BinanceSpotVenue {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &'static qx_zhenlu::venue_capabilities::VenueCapabilities {
        &qx_zhenlu::venue_capabilities::BINANCE_SPOT_VENUE_CAPABILITIES
    }

    fn validate_instrument_spec(&mut self, spec: &TradingInstrumentSpec) -> QxResult<()> {
        spec.validate()?;
        self.execution_readiness()?;
        if spec.product != TradingProduct::Spot
            || spec.contract_size != SCALE
            || spec.linear
            || spec.inverse
            || spec.max_leverage != 1
            || spec.maintenance_margin_bps != 0
            || !spec
                .settlement_currency
                .eq_ignore_ascii_case(&spec.quote_currency)
        {
            return Err(QxError::BusinessViolation(
                "Binance Spot 冻结规格包含不支持的产品、合约或杠杆字段".into(),
            ));
        }
        validate_binance_instrument(&spec.instrument)?;
        if !spec.instrument.venue.is_binance() {
            return Err(QxError::BusinessViolation(format!(
                "Binance Spot 规格 venue 不匹配: {}",
                spec.instrument.venue
            )));
        }
        let symbol = spec.instrument.symbol.to_ascii_uppercase();
        let live = self.current_spot_market_rules(&symbol)?;
        if live.status != "TRADING" {
            return Err(QxError::VenueState(format!(
                "Binance Spot {symbol} 当前状态为 {}",
                live.status
            )));
        }
        let rule_mismatch = if let Some(exact) = &spec.exact_rules {
            exact.contract_size != ExactDecimal::ONE
                || exact.price_tick != live.exact_price_tick
                || exact.quantity_step != live.exact_qty_step
                || exact.minimum_quantity != live.exact_min_qty
                || exact.minimum_notional != live.exact_min_notional
                || spec.price_tick != exact_decimal_projection(exact.price_tick)?
                || spec.qty_step != exact_decimal_projection(exact.quantity_step)?
                || spec.min_qty != exact_decimal_projection(exact.minimum_quantity)?
                || spec.min_notional_raw != exact_decimal_projection(exact.minimum_notional)?
        } else {
            live.price_tick != spec.price_tick
                || live.qty_step != spec.qty_step
                || live.min_qty != spec.min_qty
                || live.min_notional != spec.min_notional_raw
        };
        let mismatch = !live.base_asset.eq_ignore_ascii_case(&spec.base_currency)
            || !live.quote_asset.eq_ignore_ascii_case(&spec.quote_currency)
            || rule_mismatch;
        if mismatch {
            return Err(QxError::BusinessViolation(format!(
                "Binance Spot {symbol} 当前交易规则与冻结规格不一致；拒绝提交订单"
            )));
        }
        Ok(())
    }

    fn validate_order_constraints(
        &mut self,
        spec: &TradingInstrumentSpec,
        order: &Order,
    ) -> QxResult<()> {
        self.validate_instrument_spec(spec)?;
        if order.instrument != spec.instrument {
            return Err(QxError::BusinessViolation(
                "Binance Spot 订单 instrument 与冻结规格不一致".into(),
            ));
        }
        let symbol = spec.instrument.symbol.to_ascii_uppercase();
        let rules = self.current_spot_market_rules(&symbol)?;
        if rules.status != "TRADING" {
            return Err(QxError::VenueState(format!(
                "Binance Spot {symbol} 当前状态为 {}",
                rules.status
            )));
        }
        let quote_budget = order.quote_budget_decimal();
        let quantity = if quote_budget.is_some() {
            if order.side != Side::Buy || order.limit_price_decimal().is_some() {
                return Err(QxError::BusinessViolation(
                    "Binance Spot quoteOrderQty 仅支持市价买单".into(),
                ));
            }
            None
        } else {
            Some(order.quantity_decimal())
        };
        let limit_price = order.limit_price_decimal();
        if let Some(budget) = quote_budget {
            spec.validate_exact_quote_budget(budget)?;
            if budget
                .checked_cmp(rules.exact_min_notional)
                .is_some_and(|ordering| ordering.is_lt())
            {
                return Err(QxError::BusinessViolation(
                    "Binance Spot 订单名义金额低于交易所最小限制".into(),
                ));
            }
        } else if let Some(quantity) = quantity {
            spec.validate_exact_order_precision(quantity, limit_price)?;
            validate_binance_exact_qty_filter(
                quantity,
                rules.exact_qty_step,
                rules.exact_min_qty,
                rules.exact_max_qty,
                "LOT_SIZE",
            )?;
            if limit_price.is_none() {
                if let Some(step) = rules.exact_market_qty_step {
                    validate_binance_exact_qty_filter(
                        quantity,
                        step,
                        rules.exact_market_min_qty.unwrap_or(ExactDecimal::ZERO),
                        rules.exact_market_max_qty.unwrap_or(ExactDecimal::ZERO),
                        "MARKET_LOT_SIZE",
                    )?;
                }
            }
            if let Some(price) = limit_price {
                validate_binance_exact_price_filter(
                    price,
                    rules.exact_price_tick,
                    rules.exact_min_price,
                    rules.exact_max_price,
                )?;
                self.validate_percent_price_filters(&rules, &symbol, order.side, price)?;
            }
        }
        for filter in &rules.notional_filters {
            let notional = if let Some(budget) = quote_budget {
                Some((budget, budget))
            } else if let Some(quantity) = quantity {
                if let Some(price) = limit_price {
                    Some((
                        quantity
                            .checked_mul_at_scale(
                                price,
                                ExactDecimal::MAX_SCALE,
                                DecimalRounding::TowardZero,
                            )
                            .ok_or_else(|| {
                                QxError::BusinessViolation("Binance Spot 名义金额计算溢出".into())
                            })?,
                        quantity
                            .checked_mul_at_scale(
                                price,
                                ExactDecimal::MAX_SCALE,
                                DecimalRounding::AwayFromZero,
                            )
                            .ok_or_else(|| {
                                QxError::BusinessViolation("Binance Spot 名义金额计算溢出".into())
                            })?,
                    ))
                } else if filter.apply_min_to_market || filter.apply_max_to_market {
                    let reference_price =
                        self.current_spot_reference_exact_price(&symbol, filter.avg_price_mins)?;
                    Some((
                        quantity
                            .checked_mul_at_scale(
                                reference_price,
                                ExactDecimal::MAX_SCALE,
                                DecimalRounding::TowardZero,
                            )
                            .ok_or_else(|| {
                                QxError::BusinessViolation("Binance Spot 名义金额计算溢出".into())
                            })?,
                        quantity
                            .checked_mul_at_scale(
                                reference_price,
                                ExactDecimal::MAX_SCALE,
                                DecimalRounding::AwayFromZero,
                            )
                            .ok_or_else(|| {
                                QxError::BusinessViolation("Binance Spot 名义金额计算溢出".into())
                            })?,
                    ))
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((min_check, max_check)) = notional {
                if filter.apply_min_to_market || limit_price.is_some() {
                    validate_binance_exact_notional_filter(
                        min_check,
                        filter.exact_min_notional,
                        None,
                        true,
                        false,
                    )?;
                }
                if filter.apply_max_to_market || limit_price.is_some() {
                    validate_binance_exact_notional_filter(
                        max_check,
                        None,
                        filter.exact_max_notional,
                        false,
                        true,
                    )?;
                }
            }
        }
        self.validate_max_position_filter(&rules, order)?;
        Ok(())
    }

    fn execution_readiness(&self) -> QxResult<()> {
        if !self.connected {
            return Err(QxError::VenueState("Binance Spot 连接中断".into()));
        }
        if !matches!(self.state, ConnectorState::Live) {
            return Err(QxError::VenueState(format!(
                "Binance Spot 连接器状态为 {:?}，尚未完成恢复对账",
                self.state
            )));
        }
        Ok(())
    }

    fn submit(&mut self, order: Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        self.capabilities()
            .validate_submit(&order, self.id())
            .map_err(QxError::BusinessViolation)?;
        Self::validate_order_spec(&order)?;
        if !Self::can_submit(&order) {
            return Err(QxError::BusinessViolation(
                "Binance Spot 只接受待提交或已提交订单".into(),
            ));
        }
        if !self.connected {
            return Err(QxError::Ambiguous("Binance Spot 连接中断".into()));
        }
        if !matches!(self.state, ConnectorState::Live) {
            return Err(QxError::VenueState("Binance Spot 尚未完成对账".into()));
        }
        if let Some(venue_order_id) = self.venue_order_ids.get(&order.client_id) {
            return Ok(vec![VenueEvent::Accepted {
                client_order_id: order.client_id,
                venue_order_id: venue_order_id.clone(),
                ts,
            }]);
        }
        if self.orders.contains_key(&order.client_id) {
            return Err(QxError::ReconcileRequired(
                "本地订单已存在但缺少 Binance 远端订单号".into(),
            ));
        }
        validate_binance_instrument(&order.instrument)?;
        let symbol = order.instrument.symbol.to_ascii_uppercase();
        let mut parameters = BTreeMap::new();
        parameters.insert("symbol".into(), symbol);
        parameters.insert(
            "side".into(),
            match order.side {
                Side::Buy => "BUY".into(),
                Side::Sell => "SELL".into(),
            },
        );
        parameters.insert(
            "type".into(),
            if order.limit_price_decimal().is_some() {
                "LIMIT".into()
            } else {
                "MARKET".into()
            },
        );
        if let Some(budget) = order.quote_budget_decimal() {
            parameters.insert("quoteOrderQty".into(), budget.to_string());
        } else {
            parameters.insert("quantity".into(), order.quantity_decimal().to_string());
        }
        parameters.insert("newClientOrderId".into(), client_order_id(order.client_id));
        parameters.insert("newOrderRespType".into(), "FULL".into());
        if let Some(price) = order.limit_price_decimal() {
            parameters.insert("timeInForce".into(), "GTC".into());
            parameters.insert("price".into(), price.to_string());
        }
        let response = self.request("POST", "/api/v3/order", parameters, ts)?;
        let wire: BinanceOrderWire = parse_success(response, "order")?;
        if wire.order_id == 0 || wire.client_order_id != client_order_id(order.client_id) {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance 下单响应缺少或篡改 client/order id".into(),
            ));
        }
        let local_id = order.client_id;
        let mut accepted_order = order;
        accepted_order.status = OrderStatus::Accepted;
        self.orders.insert(local_id, accepted_order);
        let id = wire.order_id.to_string();
        self.venue_order_ids.insert(local_id, id.clone());
        let evicted = evict_stale_terminal_orders(&mut self.orders);
        self.forget_orders(&evicted);
        let mut events = vec![VenueEvent::Accepted {
            client_order_id: local_id,
            venue_order_id: id.clone(),
            ts,
        }];
        for fill in wire.fills {
            if let Some(event) = self.ingest_fill_wire(local_id, &id, fill)? {
                events.push(event);
            }
        }
        if wire.status == "FILLED" && events.len() == 1 {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance 已报告 FILLED 但响应缺少成交事实".into(),
            ));
        }
        if wire.status == "FILLED" {
            self.mark_remote_filled(local_id)?;
        }
        Ok(events)
    }

    fn cancel(&mut self, local_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        self.capabilities()
            .validate_cancel()
            .map_err(QxError::BusinessViolation)?;
        let order = self
            .orders
            .get(&local_id)
            .ok_or_else(|| QxError::Permanent("Binance 本地订单不存在".into()))?
            .clone();
        if order.status.is_terminal() {
            return Ok(Vec::new());
        }
        Self::validate_cancel_state(&order, "Binance")?;
        let mut parameters = BTreeMap::new();
        validate_binance_instrument(&order.instrument)?;
        parameters.insert(
            "symbol".into(),
            order.instrument.symbol.to_ascii_uppercase(),
        );
        parameters.insert("origClientOrderId".into(), client_order_id(local_id));
        let response = self.request("DELETE", "/api/v3/order", parameters, ts)?;
        let wire: BinanceOrderWire = parse_success(response, "cancel order")?;
        if wire.client_order_id != client_order_id(local_id) || wire.status != "CANCELED" {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance 撤单响应与本地订单不一致".into(),
            ));
        }
        self.mark_cancelled(local_id)?;
        Ok(vec![VenueEvent::Cancelled {
            client_order_id: local_id,
            ts,
        }])
    }

    fn snapshot(&self) -> Vec<VenueOrderSnapshot> {
        self.orders
            .values()
            .map(|order| VenueOrderSnapshot {
                client_order_id: order.client_id,
                status: order.status,
                filled: order.filled,
                filled_exact: order.exact_amounts.map(|amounts| amounts.filled),
            })
            .collect()
    }

    fn connected(&self) -> bool {
        self.connected
    }
}

fn client_order_id(id: u64) -> String {
    format!("qx-{id}")
}

fn validate_binance_instrument(instrument: &InstrumentId) -> QxResult<()> {
    if !instrument.venue.is_binance() {
        return Err(QxError::Permanent(format!(
            "Binance Venue 不接受 instrument venue: {}",
            instrument.venue
        )));
    }
    if instrument.symbol.trim().is_empty() || instrument.symbol.contains('.') {
        return Err(QxError::Permanent("Binance Spot symbol 非法".into()));
    }
    Ok(())
}

fn parse_client_order_id(value: &str) -> Option<u64> {
    value.strip_prefix("qx-")?.parse().ok()
}

fn json_string(value: &Value, key: &str) -> QxResult<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| QxError::ReconcileRequired(format!("Binance 用户事件缺少 {key}")))
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceBookTickerWire {
    #[serde(rename = "s", alias = "symbol")]
    symbol: String,
    #[serde(rename = "b", alias = "bidPrice")]
    bid: String,
    #[serde(rename = "B", alias = "bidQty")]
    bid_qty: String,
    #[serde(rename = "a", alias = "askPrice")]
    ask: String,
    #[serde(rename = "A", alias = "askQty")]
    ask_qty: String,
    #[serde(rename = "u", default)]
    update_id: u64,
    #[serde(rename = "E", default)]
    event_ts: u64,
}

fn parse_book_ticker(payload: &str, receive_ts: u64) -> QxResult<(String, QuoteTick)> {
    let wire: BinanceBookTickerWire = serde_json::from_str(payload)
        .map_err(|error| QxError::Permanent(format!("Binance bookTicker JSON 非法: {error}")))?;
    let bid = parse_price(&wire.bid, "bidPrice")?;
    let (bid_qty, bid_qty_exact) = parse_exact_quantity_projection(&wire.bid_qty, "bidQty")?;
    let ask = parse_price(&wire.ask, "askPrice")?;
    let (ask_qty, ask_qty_exact) = parse_exact_quantity_projection(&wire.ask_qty, "askQty")?;
    if !bid_qty_exact.is_positive() || !ask_qty_exact.is_positive() {
        return Err(QxError::ReconcileRequired(
            "Binance bookTicker bidQty/askQty 必须为正".into(),
        ));
    }
    let quote = QuoteTick::new(
        if wire.event_ts == 0 {
            receive_ts
        } else {
            wire.event_ts
        },
        bid,
        bid_qty,
        ask,
        ask_qty,
        wire.update_id,
    )
    .with_exact_sizes(bid_qty_exact, ask_qty_exact);
    if quote.is_crossed() {
        return Err(QxError::ReconcileRequired(
            "Binance bookTicker 出现 crossed quote".into(),
        ));
    }
    Ok((wire.symbol.to_ascii_uppercase(), quote))
}

fn parse_success<T: for<'de> Deserialize<'de>>(
    response: HttpResponse,
    operation: &str,
) -> QxResult<T> {
    serde_json::from_str(&response.body)
        .map_err(|error| QxError::Permanent(format!("Binance {operation} JSON 非法: {error}")))
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("msg").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

fn parse_quantity(value: &str, field: &str) -> QxResult<Quantity> {
    Quantity::from_dec(value)
        .ok_or_else(|| QxError::Permanent(format!("Binance {field} 定点格式非法")))
}

fn parse_exact_decimal(value: &str, field: &str) -> QxResult<ExactDecimal> {
    ExactDecimal::parse(value)
        .map_err(|error| QxError::Permanent(format!("Binance {field} 精确格式非法: {error}")))
}

fn exact_decimal_projection(value: ExactDecimal) -> QxResult<i128> {
    value
        .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| QxError::Permanent("Binance 十进制值无法投影到兼容 raw 范围".into()))
}

fn parse_exact_quantity_projection(value: &str, field: &str) -> QxResult<(Quantity, ExactDecimal)> {
    let exact = ExactDecimal::parse(value).map_err(|error| {
        QxError::ReconcileRequired(format!("Binance {field} 精确格式非法: {error}"))
    })?;
    if exact.is_negative() {
        return Err(QxError::ReconcileRequired(format!(
            "Binance {field} 成交数量不能为负"
        )));
    }
    let one = ExactDecimal::from_parts(1, 0).map_err(|error| QxError::Invariant(error.into()))?;
    let raw = exact
        .checked_mul_at_scale(one, 9, DecimalRounding::TowardZero)
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| {
            QxError::ReconcileRequired(format!("Binance {field} 无法投影到兼容 raw 范围"))
        })?;
    Ok((Quantity::from_raw(raw), exact))
}

fn parse_spot_market_rules(
    body: &str,
    expected_symbol: &str,
    fetched_at_ms: u64,
) -> QxResult<BinanceSpotMarketRules> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| QxError::Permanent(format!("Binance exchangeInfo JSON 非法: {error}")))?;
    let symbols = value
        .get("symbols")
        .and_then(Value::as_array)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 symbols 列表".into()))?;
    let market = symbols
        .iter()
        .find(|item| item.get("symbol").and_then(Value::as_str) == Some(expected_symbol))
        .ok_or_else(|| {
            QxError::BusinessViolation(format!(
                "Binance exchangeInfo 未返回交易对 {expected_symbol}"
            ))
        })?;
    let status = market
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 market status".into()))?
        .to_owned();
    let base_asset = market
        .get("baseAsset")
        .and_then(Value::as_str)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 baseAsset".into()))?
        .to_owned();
    let quote_asset = market
        .get("quoteAsset")
        .and_then(Value::as_str)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 quoteAsset".into()))?
        .to_owned();
    let filters = market
        .get("filters")
        .and_then(Value::as_array)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 filters 列表".into()))?;

    let mut price_tick = None;
    let mut qty_step = None;
    let mut min_qty = None;
    let mut min_notional = 0_i128;
    let mut notional_filters = Vec::new();
    let mut percent_price = None;
    let mut percent_price_by_side = None;
    let mut exact_price_tick = None;
    let mut exact_min_price = None;
    let mut exact_max_price = None;
    let mut exact_qty_step = None;
    let mut exact_min_qty = None;
    let mut exact_max_qty = None;
    let mut exact_market_qty_step = None;
    let mut exact_market_min_qty = None;
    let mut exact_market_max_qty = None;
    let mut exact_min_notional = ExactDecimal::ZERO;
    let mut exact_max_position = None;
    for filter in filters {
        match filter.get("filterType").and_then(Value::as_str) {
            Some("PRICE_FILTER") => {
                let tick = filter
                    .get("tickSize")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance PRICE_FILTER 缺少 tickSize".into())
                    })?;
                let minimum = filter
                    .get("minPrice")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance PRICE_FILTER 缺少 minPrice".into())
                    })?;
                let maximum = filter
                    .get("maxPrice")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance PRICE_FILTER 缺少 maxPrice".into())
                    })?;
                exact_price_tick = Some(parse_exact_decimal(tick, "PRICE_FILTER.tickSize")?);
                exact_min_price = Some(parse_exact_decimal(minimum, "PRICE_FILTER.minPrice")?);
                exact_max_price = Some(parse_exact_decimal(maximum, "PRICE_FILTER.maxPrice")?);
                price_tick = Some(exact_decimal_projection(exact_price_tick.unwrap())?);
            }
            Some("LOT_SIZE") => {
                let step = filter
                    .get("stepSize")
                    .and_then(Value::as_str)
                    .ok_or_else(|| QxError::Permanent("Binance LOT_SIZE 缺少 stepSize".into()))?;
                let minimum = filter
                    .get("minQty")
                    .and_then(Value::as_str)
                    .ok_or_else(|| QxError::Permanent("Binance LOT_SIZE 缺少 minQty".into()))?;
                let maximum = filter
                    .get("maxQty")
                    .and_then(Value::as_str)
                    .ok_or_else(|| QxError::Permanent("Binance LOT_SIZE 缺少 maxQty".into()))?;
                exact_qty_step = Some(parse_exact_decimal(step, "LOT_SIZE.stepSize")?);
                exact_min_qty = Some(parse_exact_decimal(minimum, "LOT_SIZE.minQty")?);
                exact_max_qty = Some(parse_exact_decimal(maximum, "LOT_SIZE.maxQty")?);
                qty_step = Some(exact_decimal_projection(exact_qty_step.unwrap())?);
                min_qty = Some(exact_decimal_projection(exact_min_qty.unwrap())?);
            }
            Some("MARKET_LOT_SIZE") => {
                let step = filter
                    .get("stepSize")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance MARKET_LOT_SIZE 缺少 stepSize".into())
                    })?;
                let minimum = filter
                    .get("minQty")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance MARKET_LOT_SIZE 缺少 minQty".into())
                    })?;
                let maximum = filter
                    .get("maxQty")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance MARKET_LOT_SIZE 缺少 maxQty".into())
                    })?;
                exact_market_qty_step =
                    Some(parse_exact_decimal(step, "MARKET_LOT_SIZE.stepSize")?);
                exact_market_min_qty =
                    Some(parse_exact_decimal(minimum, "MARKET_LOT_SIZE.minQty")?);
                exact_market_max_qty =
                    Some(parse_exact_decimal(maximum, "MARKET_LOT_SIZE.maxQty")?);
            }
            Some("MIN_NOTIONAL" | "NOTIONAL") => {
                let value = filter
                    .get("minNotional")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance NOTIONAL filter 缺少 minNotional".into())
                    })?;
                let exact_minimum = parse_exact_decimal(value, "NOTIONAL.minNotional")?;
                let raw = exact_decimal_projection(exact_minimum)?;
                if exact_minimum
                    .checked_cmp(exact_min_notional)
                    .is_some_and(|ordering| ordering.is_gt())
                {
                    exact_min_notional = exact_minimum;
                }
                min_notional = min_notional.max(raw);
                let avg_price_mins = json_u32(filter, "avgPriceMins", "NOTIONAL.avgPriceMins")?;
                let (exact_max_notional, apply_min_to_market, apply_max_to_market) =
                    if filter.get("filterType").and_then(Value::as_str) == Some("MIN_NOTIONAL") {
                        let apply = filter
                            .get("applyToMarket")
                            .and_then(Value::as_bool)
                            .ok_or_else(|| {
                                QxError::Permanent("Binance MIN_NOTIONAL 缺少 applyToMarket".into())
                            })?;
                        (None, apply, false)
                    } else {
                        let max_value = filter
                            .get("maxNotional")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                QxError::Permanent("Binance NOTIONAL 缺少 maxNotional".into())
                            })?;
                        let max_exact = parse_exact_decimal(max_value, "NOTIONAL.maxNotional")?;
                        let apply_min = filter
                            .get("applyMinToMarket")
                            .and_then(Value::as_bool)
                            .ok_or_else(|| {
                                QxError::Permanent("Binance NOTIONAL 缺少 applyMinToMarket".into())
                            })?;
                        let apply_max = filter
                            .get("applyMaxToMarket")
                            .and_then(Value::as_bool)
                            .ok_or_else(|| {
                                QxError::Permanent("Binance NOTIONAL 缺少 applyMaxToMarket".into())
                            })?;
                        (Some(max_exact), apply_min, apply_max)
                    };
                notional_filters.push(BinanceSpotNotionalFilter {
                    apply_min_to_market,
                    apply_max_to_market,
                    avg_price_mins,
                    exact_min_notional: exact_minimum.is_positive().then_some(exact_minimum),
                    exact_max_notional,
                });
            }
            Some("MAX_POSITION") => {
                if exact_max_position.is_some() {
                    return Err(QxError::Permanent(
                        "Binance exchangeInfo 重复包含 MAX_POSITION".into(),
                    ));
                }
                let max_position = filter
                    .get("maxPosition")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance MAX_POSITION 缺少 maxPosition".into())
                    })?;
                let max_position = parse_exact_decimal(max_position, "MAX_POSITION.maxPosition")?;
                if !max_position.is_positive() {
                    return Err(QxError::Permanent(
                        "Binance MAX_POSITION.maxPosition 必须为正数".into(),
                    ));
                }
                exact_max_position = Some(max_position);
            }
            Some("PERCENT_PRICE") => {
                if percent_price.is_some() {
                    return Err(QxError::Permanent(
                        "Binance exchangeInfo 重复包含 PERCENT_PRICE".into(),
                    ));
                }
                percent_price = Some(BinanceSpotPercentPriceFilter {
                    multiplier_down: parse_filter_decimal(
                        filter,
                        "multiplierDown",
                        "PERCENT_PRICE.multiplierDown",
                    )?,
                    multiplier_up: parse_filter_decimal(
                        filter,
                        "multiplierUp",
                        "PERCENT_PRICE.multiplierUp",
                    )?,
                    avg_price_mins: json_u32(filter, "avgPriceMins", "PERCENT_PRICE.avgPriceMins")?,
                });
            }
            Some("PERCENT_PRICE_BY_SIDE") => {
                if percent_price_by_side.is_some() {
                    return Err(QxError::Permanent(
                        "Binance exchangeInfo 重复包含 PERCENT_PRICE_BY_SIDE".into(),
                    ));
                }
                percent_price_by_side = Some(BinanceSpotPercentPriceBySideFilter {
                    bid_multiplier_down: parse_filter_decimal(
                        filter,
                        "bidMultiplierDown",
                        "PERCENT_PRICE_BY_SIDE.bidMultiplierDown",
                    )?,
                    bid_multiplier_up: parse_filter_decimal(
                        filter,
                        "bidMultiplierUp",
                        "PERCENT_PRICE_BY_SIDE.bidMultiplierUp",
                    )?,
                    ask_multiplier_down: parse_filter_decimal(
                        filter,
                        "askMultiplierDown",
                        "PERCENT_PRICE_BY_SIDE.askMultiplierDown",
                    )?,
                    ask_multiplier_up: parse_filter_decimal(
                        filter,
                        "askMultiplierUp",
                        "PERCENT_PRICE_BY_SIDE.askMultiplierUp",
                    )?,
                    avg_price_mins: json_u32(
                        filter,
                        "avgPriceMins",
                        "PERCENT_PRICE_BY_SIDE.avgPriceMins",
                    )?,
                });
            }
            _ => {}
        }
    }
    let price_tick = price_tick
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少有效 PRICE_FILTER".into()))?;
    let qty_step = qty_step.ok_or_else(|| {
        QxError::Permanent("Binance exchangeInfo 缺少有效 LOT_SIZE.stepSize".into())
    })?;
    let min_qty = min_qty.ok_or_else(|| {
        QxError::Permanent("Binance exchangeInfo 缺少有效 LOT_SIZE.minQty".into())
    })?;
    let exact_price_tick = exact_price_tick
        .filter(|value| value.is_positive())
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少有效 PRICE_FILTER".into()))?;
    let exact_min_price = exact_min_price.ok_or_else(|| {
        QxError::Permanent("Binance exchangeInfo 缺少 PRICE_FILTER.minPrice".into())
    })?;
    let exact_max_price = exact_max_price.ok_or_else(|| {
        QxError::Permanent("Binance exchangeInfo 缺少 PRICE_FILTER.maxPrice".into())
    })?;
    let exact_qty_step = exact_qty_step
        .filter(|value| value.is_positive())
        .ok_or_else(|| {
            QxError::Permanent("Binance exchangeInfo 缺少有效 LOT_SIZE.stepSize".into())
        })?;
    let exact_min_qty = exact_min_qty
        .filter(|value| value.is_positive())
        .ok_or_else(|| {
            QxError::Permanent("Binance exchangeInfo 缺少有效 LOT_SIZE.minQty".into())
        })?;
    let exact_max_qty = exact_max_qty
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 LOT_SIZE.maxQty".into()))?;
    Ok(BinanceSpotMarketRules {
        fetched_at_ms,
        status,
        base_asset,
        quote_asset,
        price_tick,
        qty_step,
        min_qty,
        min_notional,
        notional_filters,
        percent_price,
        percent_price_by_side,
        exact_price_tick,
        exact_min_price,
        exact_max_price,
        exact_qty_step,
        exact_min_qty,
        exact_max_qty,
        exact_market_qty_step,
        exact_market_min_qty,
        exact_market_max_qty,
        exact_min_notional,
        exact_max_position,
    })
}

fn parse_exchange_base_assets(body: &str) -> QxResult<BTreeMap<String, String>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| QxError::Permanent(format!("Binance exchangeInfo JSON 非法: {error}")))?;
    let symbols = value
        .get("symbols")
        .and_then(Value::as_array)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 symbols 列表".into()))?;
    let mut base_assets = BTreeMap::new();
    for market in symbols {
        let symbol = market
            .get("symbol")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_alphanumeric()))
            .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 包含非法 symbol".into()))?
            .to_ascii_uppercase();
        let base_asset = market
            .get("baseAsset")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_alphanumeric()))
            .ok_or_else(|| {
                QxError::Permanent(format!("Binance exchangeInfo {symbol} 缺少有效 baseAsset"))
            })?
            .to_ascii_uppercase();
        if base_assets.insert(symbol.clone(), base_asset).is_some() {
            return Err(QxError::Permanent(format!(
                "Binance exchangeInfo 重复包含交易对 {symbol}"
            )));
        }
    }
    if base_assets.is_empty() {
        return Err(QxError::Permanent(
            "Binance exchangeInfo 未返回任何交易对".into(),
        ));
    }
    Ok(base_assets)
}

fn json_u32(value: &Value, key: &str, field: &str) -> QxResult<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| QxError::Permanent(format!("Binance {field} 缺少有效非负整数")))
}

fn parse_filter_decimal(value: &Value, key: &str, field: &str) -> QxResult<ExactDecimal> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| QxError::Permanent(format!("Binance {field} 缺少十进制字符串")))?;
    let decimal = ExactDecimal::parse(text)
        .map_err(|error| QxError::Permanent(format!("Binance {field} 非法: {error}")))?;
    if !decimal.is_positive() {
        return Err(QxError::Permanent(format!("Binance {field} 必须为正数")));
    }
    Ok(decimal)
}

fn validate_binance_exact_qty_filter(
    qty: ExactDecimal,
    step: ExactDecimal,
    minimum: ExactDecimal,
    maximum: ExactDecimal,
    filter: &str,
) -> QxResult<()> {
    if !qty.is_positive() || !step.is_positive() {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 数量或 {filter}.stepSize 非法"
        )));
    }
    if minimum.is_positive()
        && qty
            .checked_cmp(minimum)
            .is_none_or(|ordering| ordering.is_lt())
    {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 数量低于 {filter}.minQty"
        )));
    }
    if qty.is_multiple_of(step) != Some(true) {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 数量不符合 {filter}.stepSize"
        )));
    }
    if maximum.is_positive()
        && qty
            .checked_cmp(maximum)
            .is_none_or(|ordering| ordering.is_gt())
    {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 数量超过 {filter}.maxQty"
        )));
    }
    Ok(())
}

fn validate_binance_exact_price_filter(
    price: ExactDecimal,
    tick: ExactDecimal,
    minimum: ExactDecimal,
    maximum: ExactDecimal,
) -> QxResult<()> {
    if !price.is_positive() || !tick.is_positive() {
        return Err(QxError::BusinessViolation(
            "Binance Spot 限价或 PRICE_FILTER.tickSize 非法".into(),
        ));
    }
    if minimum.is_positive()
        && price
            .checked_cmp(minimum)
            .is_none_or(|ordering| ordering.is_lt())
    {
        return Err(QxError::BusinessViolation(
            "Binance Spot 限价低于 PRICE_FILTER.minPrice".into(),
        ));
    }
    if maximum.is_positive()
        && price
            .checked_cmp(maximum)
            .is_none_or(|ordering| ordering.is_gt())
    {
        return Err(QxError::BusinessViolation(
            "Binance Spot 限价超过 PRICE_FILTER.maxPrice".into(),
        ));
    }
    if price.is_multiple_of(tick) != Some(true) {
        return Err(QxError::BusinessViolation(
            "Binance Spot 限价不符合 PRICE_FILTER.tickSize".into(),
        ));
    }
    Ok(())
}

fn validate_binance_exact_notional_filter(
    notional: ExactDecimal,
    min_notional: Option<ExactDecimal>,
    max_notional: Option<ExactDecimal>,
    apply_minimum: bool,
    apply_maximum: bool,
) -> QxResult<()> {
    if !notional.is_positive() {
        return Err(QxError::BusinessViolation(
            "Binance Spot 名义金额必须为正数".into(),
        ));
    }
    if apply_minimum {
        if let Some(minimum) = min_notional.filter(|value| value.is_positive()) {
            if notional
                .checked_cmp(minimum)
                .is_none_or(|ordering| ordering.is_lt())
            {
                return Err(QxError::BusinessViolation(
                    "Binance Spot 订单名义金额低于 MIN_NOTIONAL/NOTIONAL.minNotional".into(),
                ));
            }
        }
    }
    if apply_maximum {
        if let Some(maximum) = max_notional.filter(|value| value.is_positive()) {
            if notional
                .checked_cmp(maximum)
                .is_none_or(|ordering| ordering.is_gt())
            {
                return Err(QxError::BusinessViolation(
                    "Binance Spot 订单名义金额超过 NOTIONAL.maxNotional".into(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_binance_percent_bounds(
    price: ExactDecimal,
    reference: ExactDecimal,
    multiplier_down: ExactDecimal,
    multiplier_up: ExactDecimal,
    filter: &str,
) -> QxResult<()> {
    if multiplier_down
        .checked_cmp(multiplier_up)
        .is_none_or(|ordering| ordering.is_gt())
    {
        return Err(QxError::Permanent(format!(
            "Binance {filter} multiplier 区间非法"
        )));
    }
    let minimum = reference
        .checked_mul(multiplier_down)
        .ok_or_else(|| QxError::BusinessViolation(format!("Binance {filter} 下界计算溢出")))?;
    let maximum = reference
        .checked_mul(multiplier_up)
        .ok_or_else(|| QxError::BusinessViolation(format!("Binance {filter} 上界计算溢出")))?;
    if price
        .checked_cmp(minimum)
        .is_none_or(|ordering| ordering.is_lt())
    {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 限价低于 {filter} 动态价格下界"
        )));
    }
    if price
        .checked_cmp(maximum)
        .is_none_or(|ordering| ordering.is_gt())
    {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 限价超过 {filter} 动态价格上界"
        )));
    }
    Ok(())
}

fn parse_price(value: &str, field: &str) -> QxResult<Price> {
    Price::from_dec(value)
        .ok_or_else(|| QxError::Permanent(format!("Binance {field} 定点格式非法")))
}

fn parse_money(value: &str, field: &str) -> QxResult<Money> {
    Money::from_dec(value)
        .ok_or_else(|| QxError::Permanent(format!("Binance {field} 定点格式非法")))
}

fn map_status(value: &str) -> QxResult<OrderStatus> {
    match value {
        "NEW" => Ok(OrderStatus::Working),
        "PARTIALLY_FILLED" => Ok(OrderStatus::PartiallyFilled),
        "FILLED" => Ok(OrderStatus::Filled),
        "CANCELED" => Ok(OrderStatus::Cancelled),
        "EXPIRED" => Ok(OrderStatus::Expired),
        "REJECTED" => Ok(OrderStatus::Rejected),
        other => Err(QxError::ReconcileRequired(format!(
            "Binance 未知订单状态: {other}"
        ))),
    }
}

#[cfg(test)]
#[path = "binance/tests.rs"]
mod tests;
