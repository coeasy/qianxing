//! Binance Spot credential handling and HMAC request signing.

use super::{form_encode, HttpRequest};
use ring::hmac;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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

    pub(super) fn sign_encoded_payload(&self, payload: &str) -> String {
        let tag = hmac::sign(&self.secret, payload.as_bytes());
        tag.as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub(super) fn signed_request(
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
