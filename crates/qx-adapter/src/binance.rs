//! Binance Spot 具体连接器。
//!
//! 该模块只负责 Binance 的协议边界：参数编码/签名、订单 JSON、快照和
//! `executionReport` 用户事件映射。订单事实仍通过 `VenueEvent` 返回，绝不
//! 直接修改 OMS、Ledger 或 Kernel。

use super::venue_cache::evict_stale_terminal_orders;
use super::{
    HttpRequest, HttpResponse, HttpTransport, TlsWebSocketUserStream, WebSocketMessage,
    WebSocketRead,
};
use qx_core::{
    retry, Fill, InstrumentId, Money, Order, OrderStatus, Price, Quantity, QxError, QxResult, Side,
};
use qx_guanxing::QuoteTick;
use qx_zhenlu::{
    AdapterHealth, ConnectorCapabilities, ConnectorState, RateLimiter, Venue, VenueAdapter,
    VenueEvent, VenueOrderSnapshot,
};
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

    /// 从部署系统投影的两个凭据文件读取 API key 和 secret。
    ///
    /// 文件内容只允许是一行凭据（首尾空白会被去除），读取结果不会进入
    /// Debug、事件日志或运行时健康详情。Secret Manager/CSI/容器 secrets
    /// 可以通过原子替换文件来提供轮换；调用方应在建立新连接或新请求前重新
    /// 调用本方法，旧连接继续使用已经建立的认证上下文。
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

/// 进程毫秒钟：本文件里所有"现在几点"都走这一颗，别处不再各写一遍换算。
fn system_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

impl BinanceSpotAuth {
    pub fn new(api_key: impl Into<String>, secret: &[u8]) -> Result<Self, String> {
        Self::with_clock(api_key, secret, system_timestamp_ms)
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

    /// 生成 Binance Spot WebSocket API 的签名用户流订阅请求。
    ///
    /// 该方法只生成协议 payload，不建立网络连接，便于在沙盒前先做 golden
    /// 校验，也避免密钥进入日志或事件事实。
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

/// 一次流读取的三态结果：拿到消息、这一轮连接上没有数据、对端已关闭。
///
/// `Idle` 单独成态是 V11 N10 的关键：读超时代表连接完好、只是没人说话。把它算作失败，
/// 一条长时间无人成交的薄行情就会在十几个超时周期内耗尽重连预算，把订阅进程——以及
/// 监督器按进程粒度收走的同机其它 worker——一起带走。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BinanceStreamRead<T> {
    Message(T),
    Idle,
    Closed,
}

/// 可测试的长连接会话抽象；真实实现为用户流 `BinanceSpotUserStream` 与行情流
/// `BinanceSpotMarketStream`，测试可注入内存会话。
pub trait BinanceStreamSession {
    type Item;

    /// `now` 由重连循环的时钟给出：行情流用它给报价打接收时间戳，用户流不消费它。
    fn recv(&mut self, now: u64) -> Result<BinanceStreamRead<Self::Item>, String>;

    fn close(&mut self) -> Result<(), String>;
}

impl BinanceStreamSession for BinanceSpotUserStream {
    type Item = String;

    fn recv(&mut self, _now: u64) -> Result<BinanceStreamRead<Self::Item>, String> {
        Self::recv_event(self)
    }

    fn close(&mut self) -> Result<(), String> {
        Self::close(self)
    }
}

impl BinanceStreamSession for BinanceSpotMarketStream {
    type Item = QuoteTick;

    fn recv(&mut self, now: u64) -> Result<BinanceStreamRead<Self::Item>, String> {
        self.recv_quote(now)
    }

    fn close(&mut self) -> Result<(), String> {
        Self::close(self)
    }
}

/// 把传输层的三态交给对应的解码函数：静默与关闭都不进解码，也不许被换成故障。
///
/// 两条流共用这一颗映射（V11 N10）。生产侧的 socket 没法在测试里注入，所以"传输层
/// 说静默、会话层就必须说静默"这一格只能由这里的一处实现加一条用例守住——写歪成
/// `Idle => Closed` 就是让薄行情重新按重连上限杀掉 worker。
fn read_or_forward<T>(
    read: WebSocketRead,
    decode: impl FnOnce(WebSocketMessage) -> Result<T, String>,
) -> Result<BinanceStreamRead<T>, String> {
    Ok(match read {
        WebSocketRead::Message(message) => BinanceStreamRead::Message(decode(message)?),
        WebSocketRead::Idle => BinanceStreamRead::Idle,
        WebSocketRead::Closed => BinanceStreamRead::Closed,
    })
}

fn decode_book_ticker(
    message: WebSocketMessage,
    receive_ts: u64,
    instrument_symbol: &str,
) -> Result<QuoteTick, String> {
    if message.opcode != 0x1 {
        return Err("Binance bookTicker 不是 JSON 文本".into());
    }
    let payload = String::from_utf8(message.payload)
        .map_err(|error| format!("Binance bookTicker 不是 UTF-8: {error}"))?;
    let (symbol, quote) = parse_book_ticker(&payload, receive_ts)
        .map_err(|error| format!("Binance bookTicker 解析失败: {error}"))?;
    if symbol != instrument_symbol.to_ascii_uppercase() {
        return Err(format!(
            "Binance bookTicker symbol 不匹配: expected={instrument_symbol}, actual={symbol}"
        ));
    }
    Ok(quote)
}

fn decode_user_event(message: WebSocketMessage) -> Result<String, String> {
    if message.opcode != 0x1 {
        return Err("Binance 用户流事件不是 JSON 文本".into());
    }
    String::from_utf8(message.payload)
        .map_err(|error| format!("Binance 用户流事件不是 UTF-8: {error}"))
}

/// 驱动长连接的确定性重连循环，用户流与行情流共用同一套预算与静默口径。
/// 连接器、休眠、时钟、停止条件和回调均可替换，便于沙盒/故障注入验收。
pub fn run_binance_stream<S, C, Stop, Sleep, Clock, OnItem>(
    mut connect: C,
    policy: BinanceStreamRetryPolicy,
    mut should_stop: Stop,
    mut sleep: Sleep,
    mut now: Clock,
    mut on_item: OnItem,
) -> Result<BinanceStreamRunReport, String>
where
    S: BinanceStreamSession,
    C: FnMut() -> Result<S, String>,
    Stop: FnMut() -> bool,
    Sleep: FnMut(std::time::Duration),
    Clock: FnMut() -> u64,
    OnItem: FnMut(S::Item) -> Result<(), String>,
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
                        "Binance 流连续 {} 次重连失败且超过重试上限: {error}",
                        report.consecutive_failures
                    ));
                }
                sleep(policy.delay_for(report.consecutive_failures));
                continue;
            }
        };
        let mut callback_error = None;
        let mut delivered = 0_u64;
        // 只有"回调自己拒了"才算本地确定性故障；链路断开（`recv` 报错）不是，
        // 否则一条交付过事件后被网络掐掉的会话永远无法给预算复位。
        let mut callback_failed = false;
        while !should_stop() {
            match session.recv(now()) {
                Ok(BinanceStreamRead::Message(item)) => match on_item(item) {
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
                // 静默既不是失败也不是恢复：连接仍在，只是这一轮没有帧。不计预算、
                // 不复位，回到循环顶部重新等——顺带让停机令牌有机会被读到。
                Ok(BinanceStreamRead::Idle) => {}
                Ok(BinanceStreamRead::Closed) => break,
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
            return Err(callback_error.unwrap_or_else(|| "Binance 流关闭".into()));
        }
        sleep(policy.delay_for(report.consecutive_failures));
    }
    Ok(report)
}

/// 用户流的重连循环入口：事件自带交易所时间戳，循环层面不需要外部时钟，
/// 因此这里给共用引擎接上进程毫秒钟。行情流请直接使用 [`run_binance_stream`]。
pub fn run_binance_user_stream<S, C, Stop, Sleep, Event>(
    connect: C,
    policy: BinanceStreamRetryPolicy,
    should_stop: Stop,
    sleep: Sleep,
    mut on_event: Event,
) -> Result<BinanceStreamRunReport, String>
where
    S: BinanceStreamSession<Item = String>,
    C: FnMut() -> Result<S, String>,
    Stop: FnMut() -> bool,
    Sleep: FnMut(std::time::Duration),
    Event: FnMut(&str) -> Result<(), String>,
{
    run_binance_stream(
        connect,
        policy,
        should_stop,
        sleep,
        system_timestamp_ms,
        move |event| on_event(&event),
    )
}

/// 使用凭据加载器维护用户流；每次建立新 WebSocket 会话前重新读取凭据。
///
/// 这允许部署系统原子替换 Secret Manager 投影的 key/secret 文件：已有会话
/// 不被中途打断，下一次连接/重连使用新凭据。加载失败会进入既有重连退避，
/// 不会把半成品认证上下文交给连接器。
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

    /// 读出一条 bookTicker 报价；`Idle` 表示这一轮订阅静默，连接仍然可用。
    pub fn recv_quote(&mut self, receive_ts: u64) -> Result<BinanceStreamRead<QuoteTick>, String> {
        let read = self.stream.recv_message()?;
        let symbol = self.instrument.symbol.clone();
        read_or_forward(read, move |message| {
            decode_book_ticker(message, receive_ts, &symbol)
        })
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
        // 订阅回执必须在超时内到：这里读不到东西不是静默，而是握手没完成。
        let acknowledgement = match stream.recv_message()? {
            WebSocketRead::Message(message) => message,
            WebSocketRead::Idle => return Err("Binance 用户流订阅回执读取超时".into()),
            WebSocketRead::Closed => return Err("Binance 用户流订阅连接已关闭".into()),
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

    pub fn recv_event(&mut self) -> Result<BinanceStreamRead<String>, String> {
        let read = self.stream.recv_message()?;
        read_or_forward(read, decode_user_event)
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

/// Binance Spot REST + `executionReport` 用户事件适配器。
pub struct BinanceSpotVenue {
    id: String,
    auth: BinanceSpotAuth,
    transport: Arc<dyn HttpTransport>,
    host: String,
    port: u16,
    connected: bool,
    state: ConnectorState,
    orders: BTreeMap<u64, Order>,
    venue_order_ids: BTreeMap<u64, String>,
    seen_fill_keys: BTreeSet<(u64, u64, i128, i128, i128)>,
    rate_limiter: RateLimiter,
    market_data: BinanceSpotMarketData,
    balances: BTreeMap<String, BinanceBalance>,
    reconcile_symbols: BTreeSet<String>,
    last_event_ts: u64,
    reconnects: u64,
}

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
            last_event_ts: 0,
            reconnects: 0,
        }
    }

    /// 设置重连对账需要覆盖的 Spot symbol 集合。
    ///
    /// Binance `openOrders` 只返回当前未完成订单，不能恢复进程中断期间已经
    /// 成交、撤销或过期的本地订单。对账 worker 应提供自己的交易标的，连接器
    /// 会改用 `allOrders` 拉取完整订单历史；未配置时退化为本地 EventLog 中的
    /// symbol，完全没有本地订单时才使用无 symbol 的 `openOrders`。
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
        let response =
            self.request_with_weight("GET", "/api/v3/account", BTreeMap::new(), ts, 20)?;
        let wire: BinanceAccountWire = parse_success(response, "account")?;
        let balances = wire
            .balances
            .into_iter()
            .map(|balance| {
                Ok(BinanceBalance {
                    asset: balance.asset,
                    free: parse_money(&balance.free, "free balance")?,
                    locked: parse_money(&balance.locked, "locked balance")?,
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
                trade_id: value.get("t").and_then(Value::as_u64).unwrap_or(0),
                transact_time: value.get("T").and_then(Value::as_u64).unwrap_or(event_ts),
            };
            if let Some(event) = self.ingest_fill_wire(client_order_id, &venue_order_id, fill)? {
                events.push(event);
            }
        }
        match value.get("X").and_then(Value::as_str).unwrap_or_default() {
            "NEW" if events.is_empty() => {
                if self.orders.get(&client_order_id).is_some_and(|order| {
                    matches!(
                        order.status,
                        OrderStatus::Submitted | OrderStatus::PendingSubmit
                    )
                }) {
                    self.orders
                        .get_mut(&client_order_id)
                        .expect("order checked")
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

    pub fn fetch_remote_snapshot(&mut self) -> QxResult<Vec<VenueOrderSnapshot>> {
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
                let _ = wire.symbol;
                Ok(VenueOrderSnapshot {
                    client_order_id,
                    status: map_status(&wire.status)?,
                    filled: parse_quantity(&wire.executed_qty, "executedQty")?,
                })
            })
            .collect()
    }

    pub fn reconcile_remote(
        &mut self,
        remote: &[VenueOrderSnapshot],
    ) -> QxResult<Vec<super::AdapterReconcileIssue>> {
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

    /// 级联丢掉已退场订单的派生索引：远端订单号绑定与成交去重键。
    ///
    /// 去重键必须跟着订单一起走——只退订单不退键，缓存里照样留着账户历史上每一笔
    /// 成交的身份，封顶就成了摆设。
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
        let qty = parse_quantity(&wire.qty, "fill qty")?;
        let price = parse_price(&wire.price, "fill price")?;
        let fee = parse_money(&wire.commission, "commission")?;
        let key = (
            client_order_id,
            wire.trade_id,
            qty.raw(),
            price.raw(),
            fee.raw(),
        );
        if self.seen_fill_keys.contains(&key) {
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
        if qty.raw() <= 0 || qty.raw() > order.remaining().raw() {
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
        order.filled = Quantity::from_raw(order.filled.raw() + qty.raw());
        let next_status = if order.filled == order.qty {
            OrderStatus::Filled
        } else {
            OrderStatus::PartiallyFilled
        };
        order
            .status
            .transition(next_status)
            .map_err(QxError::Invariant)?;
        let mut fill = Fill {
            order_id: client_order_id,
            qty,
            price,
            fee,
            ts: wire.transact_time,
            account_id: order.account_id.clone(),
            fee_currency: (!wire.commission_asset.is_empty()).then_some(wire.commission_asset),
            ..Fill::default()
        };
        order.trace_fill(&mut fill, Some(&self.id), Some(venue_order_id));
        self.last_event_ts = self.last_event_ts.max(fill.ts);
        self.seen_fill_keys.insert(key);
        Ok(Some(VenueEvent::Fill(fill)))
    }

    /// 应用远端撤单回报，返回本地状态是否真的推进到 `Cancelled`。
    ///
    /// 重复的撤单回报返回 `false`（幂等，不产生第二条事实）；订单在本地已因成交或拒绝
    /// 进入终态时返回错误——那是本地与远端的真实分歧，只能交给对账，绝不能把终态改写
    /// 成 Cancelled。
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
                self.state = ConnectorState::ReconcileRequired;
                Err(QxError::Ambiguous(format!(
                    "Binance HTTP {}，订单结果未知: {}",
                    response.status,
                    error_message(&response.body)
                )))
            }
            Ok(response) => Err(QxError::Permanent(format!(
                "Binance HTTP {}: {}",
                response.status,
                error_message(&response.body)
            ))),
            Err(error) => {
                self.state = ConnectorState::ReconcileRequired;
                Err(QxError::Ambiguous(error))
            }
        }
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
            let snapshot = BinanceBalance {
                asset: asset.into(),
                free: parse_money(&free, "event free balance")?,
                locked: parse_money(&locked, "event locked balance")?,
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

    fn submit(&mut self, order: Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        order.validate().map_err(QxError::BusinessViolation)?;
        if !matches!(
            order.status,
            OrderStatus::PendingSubmit | OrderStatus::Submitted
        ) {
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
            if order.limit.is_some() {
                "LIMIT".into()
            } else {
                "MARKET".into()
            },
        );
        parameters.insert("quantity".into(), order.qty.to_string());
        parameters.insert("newClientOrderId".into(), client_order_id(order.client_id));
        parameters.insert("newOrderRespType".into(), "FULL".into());
        if let Some(price) = order.limit {
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
        Ok(events)
    }

    fn cancel(&mut self, local_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        let order = self
            .orders
            .get(&local_id)
            .ok_or_else(|| QxError::Permanent("Binance 本地订单不存在".into()))?
            .clone();
        if order.status.is_terminal() {
            return Ok(Vec::new());
        }
        if matches!(order.status, OrderStatus::Unknown) {
            return Err(QxError::ReconcileRequired(
                "未知订单状态不能直接撤 Binance 订单".into(),
            ));
        }
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
            })
            .collect()
    }

    fn connected(&self) -> bool {
        self.connected
    }
}

impl VenueAdapter for BinanceSpotVenue {
    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            market_data: true,
            user_stream: true,
            submit: true,
            cancel: true,
            replace: false,
        }
    }

    fn health(&self) -> AdapterHealth {
        AdapterHealth {
            state: self.state,
            last_event_ts: self.last_event_ts,
            reconnects: self.reconnects,
        }
    }
}

fn client_order_id(id: u64) -> String {
    format!("qx-{id}")
}

fn validate_binance_instrument(instrument: &InstrumentId) -> QxResult<()> {
    if !instrument.venue.as_str().eq_ignore_ascii_case("BINANCE") {
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
    let bid_qty = parse_quantity(&wire.bid_qty, "bidQty")?;
    let ask = parse_price(&wire.ask, "askPrice")?;
    let ask_qty = parse_quantity(&wire.ask_qty, "askQty")?;
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
    );
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
mod tests {
    use super::*;
    use qx_core::{InstrumentId, QxError};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 传输层的 `Idle` 必须原样变成会话层的 `Idle`：写歪成 `Closed` 就等于把
    /// "这条连接还能用"重新宣布成"这条会话结束了"，薄行情会再次按预算自杀。
    #[test]
    fn read_or_forward_keeps_silence_and_closure_out_of_the_decoder() {
        assert_eq!(
            read_or_forward::<String>(WebSocketRead::Idle, |_| unreachable!("静默不该进解码"))
                .unwrap(),
            BinanceStreamRead::Idle
        );
        assert_eq!(
            read_or_forward::<String>(WebSocketRead::Closed, |_| unreachable!("关闭不该进解码"))
                .unwrap(),
            BinanceStreamRead::Closed
        );
        let frame = |opcode: u8, payload: &str| WebSocketMessage {
            opcode,
            payload: payload.as_bytes().to_vec(),
        };
        assert_eq!(
            read_or_forward(
                WebSocketRead::Message(frame(0x1, "{\"e\":\"executionReport\"}")),
                decode_user_event
            )
            .unwrap(),
            BinanceStreamRead::Message("{\"e\":\"executionReport\"}".to_string())
        );
        assert_eq!(
            read_or_forward(WebSocketRead::Message(frame(0x2, "{}")), decode_user_event)
                .expect_err("解码失败必须是 Err，不能被降级成 Idle 或 Closed"),
            "Binance 用户流事件不是 JSON 文本"
        );
    }

    /// `recv_quote` 现在只剩转发，symbol 守卫与 `receive_ts` 兜底都搬进了解码函数；
    /// 这条用例是那半截逻辑唯一的常驻证据。
    #[test]
    fn book_ticker_decode_guards_symbol_and_falls_back_to_receive_ts() {
        let frame = |payload: &str| WebSocketMessage {
            opcode: 0x1,
            payload: payload.as_bytes().to_vec(),
        };
        let quote = decode_book_ticker(
            frame(r#"{"s":"btcusdt","b":"100.5","B":"2","a":"101.5","A":"3"}"#),
            7_777,
            "BTCUSDT",
        )
        .expect("合法 bookTicker 必须解出报价");
        assert_eq!(quote.ts, 7_777, "没有事件时间戳时用接收时刻兜底");
        assert_eq!(
            decode_book_ticker(
                frame(r#"{"s":"ETHUSDT","b":"100.5","B":"2","a":"101.5","A":"3"}"#),
                7_777,
                "BTCUSDT"
            )
            .expect_err("订阅 BTCUSDT 却收到 ETHUSDT 不能当成报价"),
            "Binance bookTicker symbol 不匹配: expected=BTCUSDT, actual=ETHUSDT"
        );
    }

    #[test]
    fn credentials_redact_secret_and_construct_auth() {
        let credentials = BinanceSpotCredentials::new("key", "super-secret").unwrap();
        let debug = format!("{credentials:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret"));
        assert_eq!(credentials.api_key(), "key");
        assert!(credentials.into_auth().is_ok());
    }

    #[test]
    fn credentials_load_from_projected_files_and_reject_control_bytes() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-binance-credentials-{}-{}",
            std::process::id(),
            UNIX_EPOCH.elapsed().unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let key_path = root.join("api-key");
        let secret_path = root.join("secret");
        std::fs::write(&key_path, b"api-key\r\n").unwrap();
        std::fs::write(&secret_path, b"api-secret\n").unwrap();
        let credentials = BinanceSpotCredentials::from_files(&key_path, &secret_path).unwrap();
        assert_eq!(credentials.api_key(), "api-key");

        std::fs::write(&secret_path, b"bad\nsecret").unwrap();
        assert!(BinanceSpotCredentials::from_files(&key_path, &secret_path).is_err());
        let _ = std::fs::remove_file(key_path);
        let _ = std::fs::remove_file(secret_path);
        let _ = std::fs::remove_dir(root);
    }

    #[test]
    fn user_stream_run_config_rejects_missing_endpoint_context() {
        let policy = BinanceStreamRetryPolicy::new(
            2,
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(10),
        )
        .unwrap();
        assert!(BinanceUserStreamRunConfig::new(
            "",
            "id",
            std::time::Duration::from_secs(1),
            policy
        )
        .is_err());
        assert!(BinanceUserStreamRunConfig::new(
            "ws-api.binance.com",
            "",
            std::time::Duration::from_secs(1),
            policy
        )
        .is_err());
        assert!(BinanceUserStreamRunConfig::new(
            "ws-api.binance.com",
            "id",
            std::time::Duration::ZERO,
            policy
        )
        .is_err());
    }

    #[test]
    fn binance_hmac_matches_official_ascii_payload() {
        let auth = BinanceSpotAuth::with_clock(
            "vmPUZE6mv9SD5VNHk4HlWFsOr6aKE2zvsw0MuIgwCIPy6utIco14y7Ju91duEh8A",
            b"NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j",
            || 1_499_827_319_559,
        )
        .unwrap();
        let parameters: Vec<(String, String)> = [
            ("symbol", "LTCBTC"),
            ("side", "BUY"),
            ("type", "LIMIT"),
            ("timeInForce", "GTC"),
            ("quantity", "1"),
            ("price", "0.1"),
            ("recvWindow", "5000"),
            ("timestamp", "1499827319559"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
        // 官方样例的 query 顺序不是 BTreeMap 字典序，因此直接钉住“签名的就是发出去的那段
        // 编码串”这一条私有能力；公开的 ordered 入口没有任何生产读者（V12 §16）。
        assert_eq!(
            auth.sign_encoded_payload(&form_encode_pairs(&parameters)),
            "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71"
        );
    }

    #[test]
    fn binance_user_stream_subscription_payload_is_signed_and_explicit() {
        let auth = BinanceSpotAuth::with_clock("api-key", b"secret", || 1_700_000_000_123).unwrap();
        let payload: Value =
            serde_json::from_str(&auth.user_stream_subscribe_payload("request-1")).unwrap();
        assert_eq!(payload["id"], "request-1");
        assert_eq!(payload["method"], "userDataStream.subscribe.signature");
        assert_eq!(payload["params"]["apiKey"], "api-key");
        assert_eq!(payload["params"]["timestamp"], 1_700_000_000_123_u64);
        assert_eq!(payload["params"]["recvWindow"], 5_000_u64);
        let mut parameters = BTreeMap::new();
        parameters.insert("apiKey".into(), "api-key".into());
        parameters.insert("recvWindow".into(), "5000".into());
        parameters.insert("timestamp".into(), "1700000000123".into());
        assert_eq!(
            payload["params"]["signature"],
            auth.sign_parameters(&parameters)
        );
    }

    #[test]
    fn binance_book_ticker_maps_to_l1_quote_and_rejects_crossed_market() {
        let (symbol, quote) = parse_book_ticker(
            r#"{"u":42,"s":"BTCUSDT","b":"100.10","B":"2.5","a":"100.20","A":"3.0"}"#,
            99,
        )
        .unwrap();
        assert_eq!(symbol, "BTCUSDT");
        assert_eq!(quote.ts, 99);
        assert_eq!(quote.source_seq, 42);
        assert_eq!(quote.bid, Price::from_dec("100.10").unwrap());
        assert_eq!(quote.ask_qty, Quantity::from_dec("3.0").unwrap());
        assert!(parse_book_ticker(
            r#"{"u":43,"s":"BTCUSDT","b":"100.30","B":"1","a":"100.20","A":"1"}"#,
            100,
        )
        .is_err());
    }

    #[test]
    fn binance_market_data_fetches_public_rest_book_ticker() {
        let transport = Arc::new(MockTransport {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(vec![HttpResponse {
                status: 200,
                body: r#"{"u":9,"s":"BTCUSDT","b":"100","B":"1","a":"101","A":"2"}"#.into(),
            }]),
        });
        let mut market =
            BinanceSpotMarketData::with_endpoint(transport.clone(), "mock.binance", 443);
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let quote = market.fetch_book_ticker(&instrument, 123).unwrap();
        assert_eq!(quote.mid(), Some(Price::from_dec("100.5").unwrap()));
        let request = transport.requests.lock().unwrap().pop().unwrap();
        assert_eq!(request.path, "/api/v3/ticker/bookTicker?symbol=BTCUSDT");
        assert!(request.headers.is_empty());
    }

    #[test]
    fn binance_book_ticker_accepts_official_rest_field_names() {
        let (symbol, quote) = parse_book_ticker(
            r#"{"symbol":"BTCUSDT","bidPrice":"100.10","bidQty":"2.5","askPrice":"100.20","askQty":"3.0"}"#,
            99,
        )
        .unwrap();
        assert_eq!(symbol, "BTCUSDT");
        assert_eq!(quote.ts, 99);
        assert_eq!(quote.bid, Price::from_dec("100.10").unwrap());
        assert_eq!(quote.ask_qty, Quantity::from_dec("3.0").unwrap());
    }

    #[test]
    fn binance_account_snapshot_and_position_events_update_balances() {
        let transport = Arc::new(MockTransport {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(vec![HttpResponse {
                status: 200,
                body: r#"{"balances":[{"asset":"USDT","free":"12.5","locked":"1"}]}"#.into(),
            }]),
        });
        let auth = BinanceSpotAuth::with_clock("key", b"secret", || 100).unwrap();
        let mut venue = BinanceSpotVenue::with_endpoint(
            "binance",
            auth,
            transport.clone(),
            "mock.binance",
            443,
        );
        let balances = venue.fetch_account_balances(100).unwrap();
        assert_eq!(balances[0].asset, "USDT");
        assert_eq!(balances[0].free, Money::from_dec("12.5").unwrap());
        let request = transport.requests.lock().unwrap().pop().unwrap();
        assert!(request.path.starts_with("/api/v3/account?"));
        venue
            .ingest_user_event(
                r#"{"e":"outboundAccountPosition","E":120,"B":[{"a":"USDT","f":"13","l":"0.5"}]}"#,
            )
            .unwrap();
        assert_eq!(venue.balances()[0].free, Money::from_dec("13").unwrap());
        assert_eq!(venue.health().last_event_ts, 120);
    }

    #[test]
    fn restored_orders_are_available_before_user_stream_events() {
        let (mut venue, _transport) = venue(
            r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"NEW","transactTime":1700000000000,"fills":[]}"#,
        );
        venue.restore_orders([order()]).unwrap();
        let events = venue
            .ingest_user_event(
                r#"{"e":"executionReport","E":1700000000100,"s":"BTCUSDT","c":"qx-7","S":"BUY","x":"NEW","X":"NEW","i":42,"T":1700000000100}"#,
            )
            .unwrap();
        assert!(matches!(
            events.as_slice(),
            [VenueEvent::Accepted {
                client_order_id: 7,
                ..
            }]
        ));
    }

    #[test]
    fn binance_reconcile_uses_all_orders_and_restores_remote_order_id() {
        let (mut venue, transport) = venue(
            r#"[{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"NEW","executedQty":"0"}]"#,
        );
        let mut local = order();
        local.status = OrderStatus::Working;
        venue.restore_orders([local]).unwrap();
        venue.set_reconcile_symbols(["BTCUSDT"]).unwrap();
        venue.reconnect();

        let issues = venue.fetch_and_reconcile().unwrap();

        assert!(issues.is_empty());
        assert_eq!(venue.health().state, ConnectorState::Live);
        assert_eq!(venue.venue_order_ids.get(&7), Some(&"42".to_string()));
        let request = transport.requests.lock().unwrap().pop().unwrap();
        assert!(request.path.starts_with("/api/v3/allOrders?"));
        assert!(request.path.contains("limit=1000"));
        assert!(request.path.contains("symbol=BTCUSDT"));
    }

    struct MockTransport {
        requests: Mutex<Vec<HttpRequest>>,
        responses: Mutex<Vec<HttpResponse>>,
    }

    impl HttpTransport for MockTransport {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| "mock response exhausted".into())
        }
    }

    fn order() -> Order {
        Order {
            client_id: 7,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_dec("100").unwrap()),
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "spot-a".into(),
            trace: None,
            policy: None,
        }
    }

    fn venue(response: &str) -> (BinanceSpotVenue, Arc<MockTransport>) {
        let transport = Arc::new(MockTransport {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(vec![HttpResponse {
                status: 200,
                body: response.into(),
            }]),
        });
        let auth = BinanceSpotAuth::with_clock("key", b"secret", || 1_700_000_000_000).unwrap();
        (
            BinanceSpotVenue::with_endpoint(
                "binance-spot",
                auth,
                transport.clone(),
                "mock.binance",
                443,
            ),
            transport,
        )
    }

    #[test]
    fn binance_submit_is_form_signed_and_maps_full_fill() {
        let (mut venue, transport) = venue(
            r#"{"symbol":"BTCUSDT","orderId":42,"orderListId":-1,"clientOrderId":"qx-7","transactTime":1700000000100,"price":"100","origQty":"1","executedQty":"1","status":"FILLED","timeInForce":"GTC","type":"LIMIT","side":"BUY","fills":[{"price":"100","qty":"1","commission":"0.001","commissionAsset":"BTC","tradeId":9,"transactTime":1700000000101}]}"#,
        );
        let events = venue.submit(order(), 1_700_000_000_000).unwrap();
        assert!(matches!(events[0], VenueEvent::Accepted { .. }));
        assert!(
            matches!(events[1], VenueEvent::Fill(ref fill) if fill.qty == Quantity::from_i64(1))
        );
        assert_eq!(venue.snapshot()[0].status, OrderStatus::Filled);
        let request = &transport.requests.lock().unwrap()[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/v3/order");
        assert!(request.body.contains("newClientOrderId=qx-7"));
        assert!(request.body.contains("signature="));
        assert_eq!(request.headers.get("X-MBX-APIKEY"), Some(&"key".into()));
    }

    #[test]
    fn binance_submit_events_reach_live_pipeline_and_ledger() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-adapter-pipeline-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut pipeline = qx_runtime::LiveEventPipeline::open(&root, "binance", "USDT").unwrap();
        let local_order = order();
        pipeline.register_order(local_order.clone(), 1).unwrap();
        let (mut venue, _transport) = venue(
            r#"{"symbol":"BTCUSDT","orderId":42,"orderListId":-1,"clientOrderId":"qx-7","transactTime":1700000000100,"price":"100","origQty":"1","executedQty":"1","status":"FILLED","timeInForce":"GTC","type":"LIMIT","side":"BUY","fills":[{"price":"100","qty":"1","commission":"0.001","commissionAsset":"BTC","tradeId":9,"transactTime":1700000000101}]}"#,
        );
        let events = venue.submit(local_order, 100).unwrap();
        let mut source_seq = 0_u64;
        for event in events {
            source_seq += 1;
            let (event, ts) = match event {
                VenueEvent::Accepted {
                    client_order_id,
                    ts,
                    ..
                } => (
                    qx_runtime::RuntimeExternalEvent::Accepted {
                        client_order_id,
                        venue_order_id: None,
                    },
                    ts,
                ),
                VenueEvent::Fill(fill) => {
                    let ts = fill.ts;
                    (qx_runtime::RuntimeExternalEvent::Fill { fill }, ts)
                }
                VenueEvent::Cancelled {
                    client_order_id,
                    ts,
                } => (
                    qx_runtime::RuntimeExternalEvent::Cancelled { client_order_id },
                    ts,
                ),
            };
            pipeline
                .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                    event,
                    ts,
                    ts,
                    source_seq,
                    format!("binance:{source_seq}"),
                ))
                .unwrap();
        }
        assert_eq!(pipeline.ledger().entries().len(), 3);
        assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
        assert_eq!(pipeline.snapshot().log_len, 6);
        let restored = qx_runtime::LiveEventPipeline::open(&root, "binance", "USDT").unwrap();
        assert_eq!(restored.snapshot(), pipeline.snapshot());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn user_event_is_idempotent_and_cancel_is_mapped() {
        let (mut venue, transport) = venue(
            r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"NEW","transactTime":1700000000000,"fills":[]}"#,
        );
        venue.submit(order(), 1_700_000_000_000).unwrap();
        let first = venue
            .ingest_user_event(
                r#"{"e":"executionReport","E":1700000000100,"s":"BTCUSDT","c":"qx-7","S":"BUY","x":"TRADE","X":"PARTIALLY_FILLED","i":42,"l":"0.5","L":"100","n":"0.001","N":"BTC","T":1700000000100,"t":11}"#,
            )
            .unwrap();
        assert!(matches!(first.as_slice(), [VenueEvent::Fill(_)]));
        assert!(venue
            .ingest_user_event(
                r#"{"e":"executionReport","E":1700000000100,"s":"BTCUSDT","c":"qx-7","S":"BUY","x":"TRADE","X":"PARTIALLY_FILLED","i":42,"l":"0.5","L":"100","n":"0.001","N":"BTC","T":1700000000100,"t":11}"#,
            )
            .unwrap()
            .is_empty());
        *transport.responses.lock().unwrap() = vec![HttpResponse {
            status: 200,
            body: r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"CANCELED"}"#
                .into(),
        }];
        let canceled = venue.cancel(7, 1_700_000_000_100).unwrap();
        assert!(matches!(
            canceled.as_slice(),
            [VenueEvent::Cancelled { .. }]
        ));
    }

    /// V11 N9：柜台缓存必须有封顶。这三个索引在进程启动时由 `restore_orders` 灌成
    /// 账户的全部历史订单，运行期只会继续变长；而退场必须连派生索引一起退——只退
    /// Order 不退远端号与去重键，"封顶"就只是换了个地方漏。
    #[test]
    fn order_cache_cap_evicts_terminal_orders_and_cascades_derived_indexes() {
        const CAP: u64 = crate::venue_cache::MAX_CACHED_ORDERS as u64;
        let cached = |client_id: u64| Order {
            client_id,
            // 1 号还在场上，其余全部已终态：可退的只有历史终态那批。
            status: if client_id == 1 {
                OrderStatus::Working
            } else {
                OrderStatus::Filled
            },
            filled: if client_id == 1 {
                Quantity::ZERO
            } else {
                Quantity::from_i64(1)
            },
            ..order()
        };
        let (mut venue, _) = venue("{}");
        venue.restore_orders((1..=CAP).map(cached)).unwrap();
        assert_eq!(venue.orders.len() as u64, CAP);
        // 退场前先给"将被退掉的"和"必须留下的"各挂一份派生索引。
        for client_id in [2, CAP] {
            venue
                .bind_remote_order(client_id, format!("{client_id}"))
                .unwrap();
            venue.seen_fill_keys.insert((client_id, 11, 1, 100, 0));
        }
        venue.restore_orders([cached(CAP + 1)]).unwrap();

        assert_eq!(
            venue.orders.len() as u64,
            CAP + 1 - 1_025,
            "越限后必须一次退到迟滞线，而不是留着全部历史"
        );
        assert!(venue.orders.contains_key(&1), "未终态订单永不退场");
        assert!(!venue.orders.contains_key(&2), "退的该是发号最早的终态单");
        assert!(venue.orders.contains_key(&(CAP + 1)));
        assert!(!venue.venue_order_ids.contains_key(&2));
        assert!(!venue.seen_fill_keys.contains(&(2, 11, 1, 100, 0)));
        assert_eq!(
            venue.venue_order_ids.get(&CAP),
            Some(&CAP.to_string()),
            "留下的订单不能被动过"
        );
        assert!(venue.seen_fill_keys.contains(&(CAP, 11, 1, 100, 0)));

        let trade = |client_id: u64| {
            format!(
                r#"{{"e":"executionReport","E":1700000000100,"s":"BTCUSDT","c":"qx-{client_id}","S":"BUY","x":"TRADE","X":"PARTIALLY_FILLED","i":42,"l":"0.5","L":"100","n":"0.001","N":"BTC","T":1700000000100,"t":11}}"#
            )
        };
        // 留下的活跃订单照常接单，且重复回报仍按幂等吞掉。
        assert!(matches!(
            venue.ingest_user_event(&trade(1)).unwrap().as_slice(),
            [VenueEvent::Fill(_)]
        ));
        assert!(venue.ingest_user_event(&trade(1)).unwrap().is_empty());
        // 被退场的订单一旦有回报进来，必须显式升级对账，不能被静默收下。
        let error = match venue.ingest_user_event(&trade(2)) {
            Err(error) => error,
            Ok(events) => panic!("已退场订单的回报不该被收下: {events:?}"),
        };
        assert!(
            matches!(error, QxError::ReconcileRequired(ref reason)
                if reason.contains("未知本地订单")),
            "退场后的回报要按分歧升级: {error:?}"
        );
    }

    #[test]
    fn binance_five_xx_is_ambiguous_and_requires_reconcile() {
        let transport = Arc::new(MockTransport {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(vec![HttpResponse {
                status: 500,
                body: r#"{"code":-1000,"msg":"unknown"}"#.into(),
            }]),
        });
        let auth = BinanceSpotAuth::with_clock("key", b"secret", || 1_700_000_000_000).unwrap();
        let mut venue =
            BinanceSpotVenue::with_endpoint("binance-spot", auth, transport, "mock.binance", 443);
        assert!(matches!(
            venue.submit(order(), 1_700_000_000_000),
            Err(QxError::Ambiguous(_))
        ));
        assert_eq!(venue.health().state, ConnectorState::ReconcileRequired);
    }
}
