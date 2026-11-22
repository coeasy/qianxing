use super::{
    BinanceSpotAuth, TlsWebSocketUserStream, WebSocketPoll, DEFAULT_PORT, DEFAULT_WS_PATH,
};
use qx_core::retry;
use serde_json::Value;
use std::collections::BTreeMap;

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
    connect: C,
    policy: BinanceStreamRetryPolicy,
    should_stop: Stop,
    sleep: Sleep,
    on_event: Event,
) -> Result<BinanceStreamRunReport, String>
where
    S: BinanceUserStreamSession,
    C: FnMut() -> Result<S, String>,
    Stop: FnMut() -> bool,
    Sleep: FnMut(std::time::Duration),
    Event: FnMut(&str) -> Result<(), String>,
{
    run_binance_user_stream_observing(connect, policy, should_stop, sleep, on_event, |_| {})
}

/// Drive the user stream and report each reconnect boundary as it is reached.
/// The callback runs after the monotonically increasing reconnect count is committed, including
/// the final failed boundary when the retry budget is exhausted.
pub fn run_binance_user_stream_observing<S, C, Stop, Sleep, Event, Reconnect>(
    mut connect: C,
    policy: BinanceStreamRetryPolicy,
    mut should_stop: Stop,
    mut sleep: Sleep,
    mut on_event: Event,
    mut on_reconnect: Reconnect,
) -> Result<BinanceStreamRunReport, String>
where
    S: BinanceUserStreamSession,
    C: FnMut() -> Result<S, String>,
    Stop: FnMut() -> bool,
    Sleep: FnMut(std::time::Duration),
    Event: FnMut(&str) -> Result<(), String>,
    Reconnect: FnMut(u32),
{
    let mut report = BinanceStreamRunReport::default();
    while !should_stop() {
        let mut session = match connect() {
            Ok(session) => session,
            Err(error) => {
                report.reconnects = retry::RetryPolicy::next_attempt_count(report.reconnects);
                on_reconnect(report.reconnects);
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
        on_reconnect(report.reconnects);
        report.consecutive_failures =
            retry::RetryPolicy::next_attempt_count(report.consecutive_failures);
        if !policy.allows_reconnect(report.consecutive_failures) {
            return Err(callback_error.unwrap_or_else(|| "Binance 用户流关闭".into()));
        }
        sleep(policy.delay_for(report.consecutive_failures));
    }
    Ok(report)
}

/// Credential-loading user stream with a live reconnect observer for worker diagnostics.
pub fn run_binance_user_stream_with_config_loader_observing<Stop, Sleep, Event, Load, Reconnect>(
    mut load_auth: Load,
    config: BinanceUserStreamRunConfig,
    should_stop: Stop,
    sleep: Sleep,
    on_event: Event,
    on_reconnect: Reconnect,
) -> Result<BinanceStreamRunReport, String>
where
    Stop: FnMut() -> bool,
    Sleep: FnMut(std::time::Duration),
    Event: FnMut(&str) -> Result<(), String>,
    Load: FnMut() -> Result<BinanceSpotAuth, String>,
    Reconnect: FnMut(u32),
{
    let mut request_sequence = 0_u64;
    let ws_host = config.ws_host;
    let ws_port = config.ws_port;
    let request_id_prefix = config.request_id_prefix;
    let timeout = config.timeout;
    let retry_policy = config.retry_policy;
    run_binance_user_stream_observing(
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
        on_reconnect,
    )
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
