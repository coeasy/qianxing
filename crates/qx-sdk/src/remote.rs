//! Small HTTP-facing Rust SDK adapter for routes implemented by `qx-api`.
//!
//! The adapter owns request construction and route selection; an embedding application supplies
//! the transport so authentication, TLS, retries, and timeouts stay under the caller's policy.
//! Trading, validation, and lifecycle state remain in the shared Rust server runtime.

use serde_json::Value;
use std::fmt;

/// HTTP implementation boundary. Implementations should return the response status and raw body
/// without interpreting application semantics.
pub trait RemoteTransport: Send + Sync {
    fn post(&self, path: &str, body: &str) -> Result<RemoteResponse, String>;

    /// Optional read transport. Implement this when the client needs GET based snapshots/events.
    fn get(&self, _path: &str) -> Result<RemoteResponse, String> {
        Err("GET is not supported by this transport".into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteResponse {
    pub status: u16,
    pub body: String,
}

impl RemoteResponse {
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteApiError {
    Transport(String),
    Http { status: u16, body: String },
    InvalidJson(String),
}

impl fmt::Display for RemoteApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "remote transport failed: {message}"),
            Self::Http { status, body } => write!(f, "remote API returned HTTP {status}: {body}"),
            Self::InvalidJson(message) => write!(f, "remote API returned invalid JSON: {message}"),
        }
    }
}

impl std::error::Error for RemoteApiError {}

/// HTTP route facade for remote applications. It forwards to the same `qx-api` routes used by
/// Python and C++; it does not implement trading behavior in the SDK.
pub struct RemoteApiClient<T> {
    transport: T,
}

impl<T: RemoteTransport> RemoteApiClient<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Run Provider selection, immutable dataset freezing and Bar backtest on the shared Rust API.
    pub fn run_provider_backtest_with_manifest(
        &self,
        spec: &qx_app::ProviderBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/provider-backtest", &request)
    }

    /// Run Provider Quote selection, immutable depth freezing and the shared Rust depth engine.
    pub fn run_provider_depth_backtest(
        &self,
        spec: &qx_app::ProviderDepthBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/provider-depth-backtest", &request)
    }

    pub fn start_paper_algo_execution(&self, request: &Value) -> Result<Value, RemoteApiError> {
        self.post("/app/paper/algo-executions/start", request)
    }

    pub fn get_paper_algo_execution(&self, parent_id: &str) -> Result<Value, RemoteApiError> {
        self.post(
            "/app/paper/algo-executions/get",
            &serde_json::json!({"parent_id": parent_id}),
        )
    }

    pub fn cancel_paper_algo_execution(&self, parent_id: &str) -> Result<Value, RemoteApiError> {
        self.post(
            "/app/paper/algo-executions/cancel",
            &serde_json::json!({"parent_id": parent_id}),
        )
    }

    pub fn list_paper_algo_executions(
        &self,
        after_parent_id: Option<&str>,
        limit: u32,
    ) -> Result<Value, RemoteApiError> {
        self.post(
            "/app/paper/algo-executions/list",
            &serde_json::json!({"after_parent_id": after_parent_id, "limit": limit}),
        )
    }

    /// Read the server's live event feed after a sequence cursor. A missing cursor asks the
    /// server for its initial window; stale/ahead cursor errors are returned as HTTP errors so
    /// the caller can fetch a fresh snapshot before resuming.
    pub fn get_live_events(&self, after_seq: Option<u64>) -> Result<Value, RemoteApiError> {
        self.get_live_events_after(after_seq)
    }

    /// Read one run's persisted Rust worker lifecycle events after an exclusive cursor.
    pub fn get_run_events(&self, run_id: &str, after_seq: u64) -> Result<Value, RemoteApiError> {
        self.get_run_events_after(run_id, after_seq)
    }

    fn get_run_events_after(&self, run_id: &str, after_seq: u64) -> Result<Value, RemoteApiError> {
        if run_id.is_empty()
            || !run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(RemoteApiError::InvalidJson(
                "run_id must use only ASCII letters, digits, '.', '_' or '-'".into(),
            ));
        }
        let path = format!("/app/runs/{run_id}/events?after_seq={after_seq}");
        let response = self
            .transport
            .get(&path)
            .map_err(RemoteApiError::Transport)?;
        Self::decode_response(response)
    }

    fn get_live_events_after(&self, after_seq: Option<u64>) -> Result<Value, RemoteApiError> {
        let path = match after_seq {
            Some(sequence) => format!("/events/live?after={sequence}"),
            None => "/events/live".into(),
        };
        let response = self
            .transport
            .get(&path)
            .map_err(RemoteApiError::Transport)?;
        Self::decode_response(response)
    }

    fn post(&self, path: &str, request: &Value) -> Result<Value, RemoteApiError> {
        let body = serde_json::to_string(request)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        let response = self
            .transport
            .post(path, &body)
            .map_err(RemoteApiError::Transport)?;
        Self::decode_response(response)
    }

    fn decode_response(response: RemoteResponse) -> Result<Value, RemoteApiError> {
        if !(200..300).contains(&response.status) {
            return Err(RemoteApiError::Http {
                status: response.status,
                body: response.body,
            });
        }
        serde_json::from_str(&response.body)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeTransport(Mutex<Vec<(String, String)>>);

    impl RemoteTransport for FakeTransport {
        fn post(&self, path: &str, body: &str) -> Result<RemoteResponse, String> {
            self.0
                .lock()
                .unwrap()
                .push((path.to_owned(), body.to_owned()));
            Ok(RemoteResponse::new(200, r#"{"ok":true}"#))
        }

        fn get(&self, path: &str) -> Result<RemoteResponse, String> {
            self.0
                .lock()
                .unwrap()
                .push((path.to_owned(), String::new()));
            Ok(RemoteResponse::new(200, "[]"))
        }
    }

    #[test]
    fn paper_algo_lifecycle_uses_server_routes_and_json_contract() {
        let transport = FakeTransport::default();
        let client = RemoteApiClient::new(transport);
        client
            .start_paper_algo_execution(&serde_json::json!({"parent_id":"p-1"}))
            .unwrap();
        client.get_paper_algo_execution("p/2").unwrap();
        client.cancel_paper_algo_execution("p-3").unwrap();
        client.list_paper_algo_executions(Some("p-4"), 17).unwrap();
        client.get_live_events(Some(23)).unwrap();
        let calls = client.transport.0.lock().unwrap();
        assert_eq!(
            calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
            [
                "/app/paper/algo-executions/start",
                "/app/paper/algo-executions/get",
                "/app/paper/algo-executions/cancel",
                "/app/paper/algo-executions/list",
                "/events/live?after=23",
            ]
        );
        assert_eq!(
            serde_json::from_str::<Value>(&calls[1].1).unwrap(),
            serde_json::json!({"parent_id":"p/2"})
        );
        assert_eq!(
            serde_json::from_str::<Value>(&calls[3].1).unwrap(),
            serde_json::json!({"after_parent_id":"p-4","limit":17})
        );
    }

    #[test]
    fn provider_backtest_uses_shared_server_route_and_preserves_spec_json() {
        let client = RemoteApiClient::new(FakeTransport::default());
        let query = crate::DataQuery {
            kind: crate::DataKind::Bar,
            asset_class: "crypto".into(),
            instrument_set: std::collections::BTreeSet::from(["BTCUSDT.BINANCE".into()]),
            field_set: std::collections::BTreeSet::from(["close".into()]),
            frequency: "1m".into(),
            adjustment: "none".into(),
            quality_policy: "strict".into(),
            start: 1,
            end: 10,
            as_of: Some(10),
        };
        let spec = crate::ProviderBacktestSpec {
            data_request: crate::DataRequest::new(query, crate::DataRequirements::default()),
            backtest: crate::BacktestSpec::new(
                "provider-run",
                "BTCUSDT.BINANCE",
                "bars.json",
                "USDT",
                100_000_000_000_000,
                20261013,
                "out",
                crate::BuiltinStrategySpec::new(
                    crate::BuiltinStrategyKind::SmaCross,
                    "provider-sma",
                ),
            ),
            dataset_id: "bars".into(),
            dataset_version: "v1".into(),
            interval_ms: 60_000,
            timezone: "UTC".into(),
            manifest_path: None,
        };
        let _ = client.run_provider_backtest_with_manifest(&spec).unwrap();
        let calls = client.transport.0.lock().unwrap();
        assert_eq!(calls[0].0, "/app/provider-backtest");
        let expected = serde_json::to_value(spec).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&calls[0].1).unwrap(),
            expected
        );
    }

    #[test]
    fn event_reads_preserve_optional_cursor_and_server_recovery_errors() {
        struct CursorTransport(Mutex<Vec<String>>);
        impl RemoteTransport for CursorTransport {
            fn post(&self, _: &str, _: &str) -> Result<RemoteResponse, String> {
                unreachable!()
            }
            fn get(&self, path: &str) -> Result<RemoteResponse, String> {
                self.0.lock().unwrap().push(path.into());
                let status = if path.contains("after=99") { 409 } else { 200 };
                Ok(RemoteResponse::new(
                    status,
                    if status == 409 {
                        r#"{"error":"event_cursor_requires_snapshot"}"#
                    } else {
                        "[]"
                    },
                ))
            }
        }
        let transport = CursorTransport(Mutex::new(Vec::new()));
        let client = RemoteApiClient::new(transport);
        assert_eq!(client.get_live_events(None).unwrap(), serde_json::json!([]));
        assert!(matches!(
            client.get_live_events(Some(99)),
            Err(RemoteApiError::Http { status: 409, .. })
        ));
        assert_eq!(
            *client.transport.0.lock().unwrap(),
            ["/events/live", "/events/live?after=99"]
        );
    }

    #[test]
    fn run_event_reads_are_scoped_and_use_an_exclusive_cursor() {
        struct RunEventsTransport(Mutex<Vec<String>>);
        impl RemoteTransport for RunEventsTransport {
            fn post(&self, _: &str, _: &str) -> Result<RemoteResponse, String> {
                unreachable!()
            }
            fn get(&self, path: &str) -> Result<RemoteResponse, String> {
                self.0.lock().unwrap().push(path.into());
                Ok(RemoteResponse::new(200, "[]"))
            }
        }
        let transport = RunEventsTransport(Mutex::new(Vec::new()));
        let client = RemoteApiClient::new(transport);
        assert_eq!(
            client.get_run_events("run-1", 7).unwrap(),
            serde_json::json!([])
        );
        assert_eq!(
            *client.transport.0.lock().unwrap(),
            ["/app/runs/run-1/events?after_seq=7"]
        );
        assert!(client.get_run_events("../secret", 0).is_err());
    }

    #[test]
    fn remote_client_preserves_http_and_transport_errors() {
        struct ErrorTransport;
        impl RemoteTransport for ErrorTransport {
            fn post(&self, _: &str, _: &str) -> Result<RemoteResponse, String> {
                Ok(RemoteResponse::new(
                    403,
                    r#"{"error":"trading_permission_required"}"#,
                ))
            }
        }
        let client = RemoteApiClient::new(ErrorTransport);
        assert!(matches!(
            client.cancel_paper_algo_execution("p-1"),
            Err(RemoteApiError::Http { status: 403, .. })
        ));

        struct OfflineTransport;
        impl RemoteTransport for OfflineTransport {
            fn post(&self, _: &str, _: &str) -> Result<RemoteResponse, String> {
                Err("connection refused".into())
            }
        }
        let offline = RemoteApiClient::new(OfflineTransport);
        assert_eq!(
            offline.get_paper_algo_execution("p-1"),
            Err(RemoteApiError::Transport("connection refused".into()))
        );
    }

    #[test]
    fn remote_client_rejects_invalid_success_json() {
        struct InvalidJsonTransport;
        impl RemoteTransport for InvalidJsonTransport {
            fn post(&self, _: &str, _: &str) -> Result<RemoteResponse, String> {
                Ok(RemoteResponse::new(200, "not-json"))
            }
        }
        let client = RemoteApiClient::new(InvalidJsonTransport);
        assert!(matches!(
            client.get_paper_algo_execution("p-1"),
            Err(RemoteApiError::InvalidJson(_))
        ));
    }
}
