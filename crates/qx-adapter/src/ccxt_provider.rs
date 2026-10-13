//! CCXT historical Bars adapted to the shared Rust Provider port.
//!
//! Exchange protocol, pagination and decimal conversion stay in the isolated CCXT worker. This
//! adapter validates its canonical BarFrame response and emits the same provenance-bearing
//! `ProviderResult` consumed by Rust/Python `ProviderRegistry` workflows.

use crate::{CcxtProcessClient, CcxtRpc};
use qx_guanxing::Bar;
use qx_provider::{
    DataKind, DataProvider, DataQuery, ProviderCapability, ProviderError, ProviderErrorClass,
    ProviderPayload, ProviderResult,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const BAR_CONTENT_TYPE: &str = "application/vnd.qianxing.bar+json;v=1";
const DEFAULT_OHLCV_PAGE_LIMIT: u64 = 1_000;
const MAX_PROVIDER_BAR_ROWS: usize = 100_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CcxtBarFrame {
    #[serde(default)]
    schema_version: u32,
    instrument: String,
    source: String,
    ts: Vec<u64>,
    open_raw: Vec<i128>,
    high_raw: Vec<i128>,
    low_raw: Vec<i128>,
    close_raw: Vec<i128>,
    volume_raw: Vec<i128>,
}

/// CCXT historical OHLCV source implementing the shared Rust `DataProvider` port.
///
/// Create one instance per configured exchange; `ProviderRegistry` then selects and fails over
/// among exchanges according to their declared capability and stable priority.
pub struct CcxtBarDataProvider {
    capability: ProviderCapability,
    rpc: Mutex<Box<dyn CcxtRpc>>,
    clock_ms: fn() -> Result<u64, ProviderError>,
}

impl CcxtBarDataProvider {
    pub fn new(capability: ProviderCapability, rpc: Box<dyn CcxtRpc>) -> Self {
        Self {
            capability,
            rpc: Mutex::new(rpc),
            clock_ms: unix_time_ms,
        }
    }

    /// Start the isolated CCXT worker and attach its historical Bars port to this capability.
    pub fn spawn(
        capability: ProviderCapability,
        python: &str,
        config_path: &str,
        working_dir: Option<&str>,
    ) -> Result<Self, String> {
        let rpc = CcxtProcessClient::spawn(python, config_path, working_dir)?;
        Ok(Self::new(capability, Box::new(rpc)))
    }

    fn fetch_frame(&self, query: &DataQuery) -> Result<CcxtBarFrame, ProviderError> {
        if query.kind != DataKind::Bar || query.instrument_set.len() != 1 {
            return Err(permanent("CCXT Bar Provider 只接受单标的历史 Bar 查询"));
        }
        let instrument = query.instrument_set.iter().next().expect("one instrument");
        let request = json!({
            "op": "fetch_ohlcv",
            "instrument": instrument,
            "timeframe": query.frequency,
            "start_ms": query.start,
            "end_ms": query.end,
            "limit": DEFAULT_OHLCV_PAGE_LIMIT,
            "params": {},
        });
        let response = self
            .rpc
            .lock()
            .map_err(|_| permanent("CCXT Bar Provider RPC 锁损坏"))?
            .call(request)
            .map_err(classify_ccxt_error)?;
        let frame = response
            .get("frame")
            .cloned()
            .ok_or_else(|| permanent("CCXT OHLCV 响应缺少 frame"))?;
        let frame: CcxtBarFrame = serde_json::from_value(frame)
            .map_err(|error| permanent(format!("CCXT BarFrame 格式无效: {error}")))?;
        if frame.schema_version > 1
            || frame.instrument != *instrument
            || frame.source.trim().is_empty()
        {
            return Err(permanent(
                "CCXT BarFrame 版本、标的或数据源身份与请求不匹配",
            ));
        }
        let rows = frame.ts.len();
        if rows == 0 || rows > MAX_PROVIDER_BAR_ROWS {
            return Err(permanent(format!(
                "CCXT BarFrame 行数必须在 1..={MAX_PROVIDER_BAR_ROWS}"
            )));
        }
        if [
            frame.open_raw.len(),
            frame.high_raw.len(),
            frame.low_raw.len(),
            frame.close_raw.len(),
            frame.volume_raw.len(),
        ]
        .into_iter()
        .any(|length| length != rows)
            || frame.ts.windows(2).any(|window| window[0] >= window[1])
            || frame
                .ts
                .iter()
                .any(|timestamp| *timestamp < query.start || *timestamp > query.end)
        {
            return Err(permanent(
                "CCXT BarFrame 列长度、时间顺序或查询时间范围无效",
            ));
        }
        Ok(frame)
    }
}

impl DataProvider for CcxtBarDataProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.capability
    }

    fn fetch(&self, query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        let frame = self.fetch_frame(query)?;
        let received_at = (self.clock_ms)()?;
        let payloads = frame
            .ts
            .iter()
            .copied()
            .zip(frame.open_raw)
            .zip(frame.high_raw)
            .zip(frame.low_raw)
            .zip(frame.close_raw)
            .zip(frame.volume_raw)
            .map(|(((((timestamp, open), high), low), close), volume)| {
                let bar = Bar::new(timestamp, open, high, low, close, volume);
                (
                    timestamp,
                    received_at,
                    ProviderPayload::new_json(
                        frame.instrument.clone(),
                        BAR_CONTENT_TYPE,
                        serde_json::to_string(&bar).expect("Bar serialization is infallible"),
                    ),
                )
            })
            .collect();
        ProviderResult::from_payloads(
            self.capability.provider_id.clone(),
            self.capability.version.clone(),
            format!("ccxt-{:016x}-{received_at}", query.digest()),
            received_at,
            payloads,
        )
        .map_err(|error| permanent(format!("CCXT ProviderResult 校验失败: {}", error.message)))
    }
}

fn unix_time_ms() -> Result<u64, ProviderError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| permanent(format!("系统时钟早于 Unix epoch: {error}")))?;
    u64::try_from(duration.as_millis())
        .map_err(|error| permanent(format!("CCXT receive timestamp 溢出: {error}")))
}

fn classify_ccxt_error(message: String) -> ProviderError {
    let class = message
        .strip_prefix("CCXT Worker [")
        .and_then(|message| message.split_once(']'))
        .map(|(class, _)| class)
        .unwrap_or("unknown");
    let category = match class {
        "authentication" => ProviderErrorClass::ManualIntervention,
        "invalid" | "unsupported" => ProviderErrorClass::Permanent,
        "retryable" | "rate_limited" | "exchange" | "unknown" => ProviderErrorClass::SwitchProvider,
        _ => ProviderErrorClass::SwitchProvider,
    };
    ProviderError::new(category, message)
}

fn permanent(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorClass::Permanent, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_provider::{DataRequest, DataRequirements, ProviderRegistry};
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    struct FixtureRpc {
        response: Result<Value, String>,
        request: Arc<Mutex<Option<Value>>>,
    }

    impl CcxtRpc for FixtureRpc {
        fn call(&mut self, request: Value) -> Result<Value, String> {
            *self.request.lock().unwrap() = Some(request);
            self.response.clone()
        }
    }

    fn capability(id: &str) -> ProviderCapability {
        ProviderCapability {
            provider_id: id.into(),
            version: "ccxt-1".into(),
            data_kinds: [DataKind::Bar].into_iter().collect(),
            asset_classes: ["crypto".into()].into_iter().collect(),
            frequencies: ["1m".into()].into_iter().collect(),
            auth_scope: "public".into(),
            rate_limit_per_second: 100,
            freshness_seconds: 60,
            historical_start: 1,
            historical_end: 10_000,
            realtime: false,
            priority: 1,
            quality_score: 100,
            cost_score: 1,
        }
    }

    fn request() -> DataRequest {
        DataRequest::new(
            DataQuery {
                kind: DataKind::Bar,
                asset_class: "crypto".into(),
                instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
                field_set: BTreeSet::from(["close".into()]),
                frequency: "1m".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start: 1_000,
                end: 3_000,
                as_of: None,
            },
            DataRequirements::default(),
        )
    }

    fn frame_json(instrument: &str, timestamps: &[u64]) -> Value {
        json!({
            "schema_version": 1,
            "instrument": instrument,
            "source": "ccxt:binance:BTC/USDT:1m",
            "ts": timestamps,
            "open_raw": [10, 20],
            "high_raw": [12, 22],
            "low_raw": [9, 19],
            "close_raw": [11, 21],
            "volume_raw": [100, 200],
        })
    }

    fn make_provider(
        response: Result<Value, String>,
    ) -> (CcxtBarDataProvider, Arc<Mutex<Option<Value>>>) {
        let request = Arc::new(Mutex::new(None));
        let adapter = FixtureRpc {
            response,
            request: Arc::clone(&request),
        };
        (
            CcxtBarDataProvider {
                capability: capability("ccxt:binance"),
                rpc: Mutex::new(Box::new(adapter)),
                clock_ms: || Ok(4_000),
            },
            request,
        )
    }

    #[test]
    fn ccxt_historical_bars_enter_provider_registry_with_lineage_and_shared_frame() {
        let (provider, captured_request) = make_provider(Ok(json!({
            "frame": frame_json("BTCUSDT.BINANCE", &[1_000, 2_000])
        })));
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(provider)).unwrap();

        let fetched = registry.fetch_bar_frame(&request()).unwrap();
        assert_eq!(fetched.result.provider_id, "ccxt:binance");
        assert_eq!(fetched.result.records.len(), 2);
        assert_eq!(fetched.result.records[0].receive_time, 4_000);
        assert_eq!(fetched.frame.instrument.to_string(), "BTCUSDT.BINANCE");
        assert_eq!(fetched.frame.ts, [1_000, 2_000]);
        assert_eq!(fetched.frame.close_raw, [11, 21]);
        assert_eq!(
            *captured_request.lock().unwrap(),
            Some(json!({
                "op": "fetch_ohlcv",
                "instrument": "BTCUSDT.BINANCE",
                "timeframe": "1m",
                "start_ms": 1_000,
                "end_ms": 3_000,
                "limit": DEFAULT_OHLCV_PAGE_LIMIT,
                "params": {},
            }))
        );
    }

    #[test]
    fn ccxt_bar_provider_rejects_malformed_frames_and_future_as_of_claims() {
        let (provider, _) = make_provider(Ok(json!({
            "frame": frame_json("ETHUSDT.BINANCE", &[1_000, 2_000])
        })));
        assert_eq!(
            provider.fetch(&request().query).unwrap_err().class,
            ProviderErrorClass::Permanent
        );

        let (provider, _) = make_provider(Ok(json!({
            "frame": frame_json("BTCUSDT.BINANCE", &[1_000, 2_000])
        })));
        let mut point_in_time = request();
        point_in_time.query.as_of = Some(3_000);
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(provider)).unwrap();
        assert_eq!(
            registry.fetch_request(&point_in_time).unwrap_err(),
            qx_provider::RegistryError::Provider(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult 包含查询范围外或错误来源的数据",
            ))
        );
    }

    #[test]
    fn ccxt_authentication_errors_stop_failover_but_network_errors_allow_it() {
        assert_eq!(
            classify_ccxt_error("CCXT Worker [authentication] rejected".into()).class,
            ProviderErrorClass::ManualIntervention
        );
        assert_eq!(
            classify_ccxt_error("CCXT Worker [retryable] timeout".into()).class,
            ProviderErrorClass::SwitchProvider
        );
    }
}
