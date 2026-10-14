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

    /// Match a concrete data request against configured Rust Providers without fetching data.
    pub fn provider_preflight(
        &self,
        request: &crate::DataRequest,
    ) -> Result<Value, RemoteApiError> {
        let request_value = provider_request_value(request)?;
        self.post("/app/capabilities/providers/preflight", &request_value)
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

    /// Run a synchronized multi-market Bar backtest through the shared Rust application engine.
    pub fn run_multi_market_backtest(
        &self,
        spec: &qx_app::MultiMarketBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/multi-market-backtest", &request)
    }

    /// Start a cancellable synchronized multi-market Bar backtest on the shared Rust server.
    pub fn start_multi_market_backtest(
        &self,
        spec: &qx_app::MultiMarketBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/multi-market-backtest/start", &request)
    }

    /// Run a shared-account multi-market L2 backtest on the Rust application server.
    pub fn run_multi_market_depth_backtest(
        &self,
        spec: &qx_app::MultiMarketDepthBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/multi-market-depth-backtest", &request)
    }

    /// Start a cancellable shared-account multi-market L2 backtest.
    pub fn start_multi_market_depth_backtest(
        &self,
        spec: &qx_app::MultiMarketDepthBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/multi-market-depth-backtest/start", &request)
    }

    /// Run a bounded strategy/parameter grid on the shared multi-market Rust engine.
    pub fn run_multi_market_experiment(
        &self,
        spec: &qx_app::RunMultiMarketExperimentSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/run-multi-market-experiment", &request)
    }

    /// Start a cancellable multi-market candidate grid on the shared Rust server.
    pub fn start_multi_market_experiment(
        &self,
        spec: &qx_app::RunMultiMarketExperimentSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/run-multi-market-experiment/start", &request)
    }

    pub fn run_multi_market_depth_experiment(
        &self,
        spec: &qx_app::RunMultiMarketDepthExperimentSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/run-multi-market-depth-experiment", &request)
    }

    pub fn start_multi_market_depth_experiment(
        &self,
        spec: &qx_app::RunMultiMarketDepthExperimentSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/run-multi-market-depth-experiment/start", &request)
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

fn provider_request_value(request: &crate::DataRequest) -> Result<Value, RemoteApiError> {
    let json = request
        .to_json()
        .map_err(|error| RemoteApiError::InvalidJson(error.message))?;
    serde_json::from_str(&json).map_err(|error| RemoteApiError::InvalidJson(error.to_string()))
}

#[path = "remote/strategy_catalog.rs"]
mod strategy_catalog;

#[cfg(test)]
#[path = "remote/tests.rs"]
mod tests;
