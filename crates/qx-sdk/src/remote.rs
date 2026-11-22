//! Small HTTP-facing Rust SDK adapter for routes implemented by `qx-api`.
//!
//! The adapter owns request construction and route selection; an embedding application supplies
//! the transport so authentication, TLS, retries, and timeouts stay under the caller's policy.
//! Trading, validation, and lifecycle state remain in the shared Rust server runtime.

use serde_json::Value;
use std::fmt;

const JOINT_BACKTEST: &str = "/app/multi-account-joint-backtest";
const JOINT_BACKTEST_START: &str = "/app/multi-account-joint-backtest/start";
const MARKET_EXPERIMENT: &str = "/app/run-multi-market-experiment";
const MARKET_EXPERIMENT_START: &str = "/app/run-multi-market-experiment/start";
const DEPTH_EXPERIMENT: &str = "/app/run-multi-market-depth-experiment";
const DEPTH_EXPERIMENT_START: &str = "/app/run-multi-market-depth-experiment/start";

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
    Http {
        status: u16,
        body: String,
    },
    InvalidJson(String),
    /// A state-changing request may have reached the server. Inspect its state before retrying.
    OutcomeUnknown {
        operation: String,
        recovery_id: Option<String>,
        cause: Box<RemoteApiError>,
    },
}

impl fmt::Display for RemoteApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "remote transport failed: {message}"),
            Self::Http { status, body } => write!(f, "remote API returned HTTP {status}: {body}"),
            Self::InvalidJson(message) => write!(f, "remote API returned invalid JSON: {message}"),
            Self::OutcomeUnknown {
                operation,
                recovery_id,
                cause,
            } => {
                write!(f, "remote operation {operation} has an unknown outcome")?;
                if let Some(id) = recovery_id {
                    write!(f, " (recovery ID: {id})")?;
                }
                write!(f, "; inspect server state before retrying: {cause}")
            }
        }
    }
}

impl std::error::Error for RemoteApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OutcomeUnknown { cause, .. } => Some(cause.as_ref()),
            _ => None,
        }
    }
}

/// HTTP route facade for remote applications. It forwards to the same `qx-api` routes used by
/// Python and C++; it does not implement trading behavior in the SDK.
pub struct RemoteApiClient<T> {
    transport: T,
}

impl<T: RemoteTransport> RemoteApiClient<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Validate a Bar dataset located beneath the server's configured data root.
    pub fn validate_dataset(
        &self,
        spec: &crate::DatasetSpec,
    ) -> Result<crate::DatasetVerdict, RemoteApiError> {
        self.post_typed("/app/validate-dataset", spec)
    }

    /// Check local Bar data and market-rule coverage before starting a remote backtest.
    pub fn preflight_backtest(
        &self,
        spec: &crate::BacktestSpec,
    ) -> Result<crate::BacktestPreflightResultV1, RemoteApiError> {
        self.post_typed("/app/backtest/preflight", spec)
    }

    /// Run a Bar backtest. The service chooses the artifact directory; spec paths are server-side.
    pub fn run_backtest(
        &self,
        spec: &crate::BacktestSpec,
    ) -> Result<crate::BacktestOutcome, RemoteApiError> {
        self.run_backtest_request(spec)
    }

    /// Start an asynchronous Bar backtest; query its returned run ID with `run_status`.
    pub fn start_backtest(&self, spec: &crate::BacktestSpec) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post_run_mutation("/app/backtest/start", &request, &spec.run_id)
    }

    /// Verify server-side artifacts returned by a completed Bar backtest.
    pub fn verify_run(
        &self,
        outcome: &crate::BacktestOutcome,
    ) -> Result<crate::VerificationResult, RemoteApiError> {
        self.verify_backtest_request(outcome)
    }

    /// Read an asynchronous run's state, terminal outcome or error without polling automatically.
    pub fn run_status(&self, run_id: &str) -> Result<Value, RemoteApiError> {
        validate_run_id(run_id)?;
        self.get_run_status(run_id)
    }

    /// Request cooperative cancellation of a server run. A successful response is not a terminal state.
    pub fn cancel_run(&self, run_id: &str) -> Result<Value, RemoteApiError> {
        validate_run_id(run_id)?;
        let path = format!("/app/runs/{run_id}/cancel");
        self.post_run_mutation(&path, &serde_json::json!({}), run_id)
    }

    /// Match a concrete data request against configured Rust Providers without fetching data.
    pub fn provider_preflight(
        &self,
        request: &crate::DataRequest,
    ) -> Result<Value, RemoteApiError> {
        let request_value = provider_request_value(request)?;
        self.post("/app/capabilities/providers/preflight", &request_value)
    }

    /// Discover versioned built-in strategy recipes with defaults and research cautions.
    pub fn builtin_strategy_recipes(&self) -> Result<Value, RemoteApiError> {
        self.get_discovery("/app/strategy-recipes")
    }

    /// Run Provider selection, immutable dataset freezing and Bar backtest on the shared Rust API.
    pub fn run_provider_backtest_with_manifest(
        &self,
        spec: &qx_app::ProviderBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post_mutation("/app/provider-backtest", &request, None)
    }

    /// Load replay-verified bounded artifacts for a completed Bar backtest.
    pub fn research_artifacts(
        &self,
        outcome: &qx_app::BacktestOutcome,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(outcome)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post("/app/research-artifacts", &request)
    }

    /// Run Provider Quote selection, immutable depth freezing and the shared Rust depth engine.
    pub fn run_provider_depth_backtest(
        &self,
        spec: &qx_app::ProviderDepthBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post_mutation("/app/provider-depth-backtest", &request, None)
    }

    /// Run a synchronized multi-market Bar backtest through the shared Rust application engine.
    pub fn run_multi_market_backtest(
        &self,
        spec: &qx_app::MultiMarketBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        self.post_mutation_spec("/app/multi-market-backtest", spec, Some(&spec.run_id))
    }

    /// Start a cancellable synchronized multi-market Bar backtest on the shared Rust server.
    pub fn start_multi_market_backtest(
        &self,
        spec: &qx_app::MultiMarketBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        self.post_run_mutation_spec("/app/multi-market-backtest/start", spec, &spec.run_id)
    }

    /// Run a joint multi-exchange portfolio while preserving each account's independent ledger.
    pub fn run_multi_account_joint_backtest(
        &self,
        spec: &qx_app::MultiAccountJointBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        self.post_mutation_spec(JOINT_BACKTEST, spec, Some(&spec.run_id))
    }

    /// Start a cancellable joint multi-exchange portfolio run on the Rust application server.
    pub fn start_multi_account_joint_backtest(
        &self,
        spec: &qx_app::MultiAccountJointBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        self.post_run_mutation_spec(JOINT_BACKTEST_START, spec, &spec.run_id)
    }

    /// Run a shared-account multi-market L2 backtest on the Rust application server.
    pub fn run_multi_market_depth_backtest(
        &self,
        spec: &qx_app::MultiMarketDepthBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        self.post_mutation_spec("/app/multi-market-depth-backtest", spec, Some(&spec.run_id))
    }

    /// Start a cancellable shared-account multi-market L2 backtest.
    pub fn start_multi_market_depth_backtest(
        &self,
        spec: &qx_app::MultiMarketDepthBacktestSpec,
    ) -> Result<Value, RemoteApiError> {
        self.post_run_mutation_spec("/app/multi-market-depth-backtest/start", spec, &spec.run_id)
    }

    /// Run a bounded strategy/parameter grid on the shared multi-market Rust engine.
    pub fn run_multi_market_experiment(
        &self,
        spec: &qx_app::RunMultiMarketExperimentSpec,
    ) -> Result<Value, RemoteApiError> {
        self.post_mutation_spec(MARKET_EXPERIMENT, spec, Some(&spec.experiment_id))
    }

    /// Start a cancellable multi-market candidate grid on the shared Rust server.
    pub fn start_multi_market_experiment(
        &self,
        spec: &qx_app::RunMultiMarketExperimentSpec,
    ) -> Result<Value, RemoteApiError> {
        self.post_run_mutation_spec(MARKET_EXPERIMENT_START, spec, &spec.experiment_id)
    }

    pub fn run_multi_market_depth_experiment(
        &self,
        spec: &qx_app::RunMultiMarketDepthExperimentSpec,
    ) -> Result<Value, RemoteApiError> {
        self.post_mutation_spec(DEPTH_EXPERIMENT, spec, Some(&spec.experiment_id))
    }

    pub fn start_multi_market_depth_experiment(
        &self,
        spec: &qx_app::RunMultiMarketDepthExperimentSpec,
    ) -> Result<Value, RemoteApiError> {
        self.post_run_mutation_spec(DEPTH_EXPERIMENT_START, spec, &spec.experiment_id)
    }

    pub fn start_paper_algo_execution(&self, request: &Value) -> Result<Value, RemoteApiError> {
        self.start_paper_request(request)
    }

    pub fn get_paper_algo_execution(&self, parent_id: &str) -> Result<Value, RemoteApiError> {
        self.get_paper_request(parent_id)
    }

    pub fn cancel_paper_algo_execution(&self, parent_id: &str) -> Result<Value, RemoteApiError> {
        self.cancel_paper_request(parent_id)
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

    /// Read up to 10,000 audit records newest-first. Use `get_command_audit_page` to continue
    /// through larger histories without collecting the full result in memory.
    pub fn get_command_audit(&self) -> Result<Vec<crate::ControlAuditRecordV1>, RemoteApiError> {
        control_audit::get_command_audit(self)
    }

    /// Read only the audit record for a request whose submission outcome is uncertain.
    pub fn get_command_audit_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Vec<crate::ControlAuditRecordV1>, RemoteApiError> {
        self.get_command_audit_page(None, 2, Some(request_id))
            .map(|page| page.entries.into_iter().map(|entry| entry.record).collect())
    }

    /// Read one newest-first page. The returned cursor is stable across API process restarts.
    pub fn get_command_audit_page(
        &self,
        cursor: Option<&str>,
        limit: usize,
        request_id: Option<&str>,
    ) -> Result<crate::ControlAuditPageV1, RemoteApiError> {
        control_audit::get_command_audit_page(self, cursor, limit, request_id)
    }

    /// Read one run's persisted Rust worker lifecycle events after an exclusive cursor.
    pub fn get_run_events(&self, run_id: &str, after_seq: u64) -> Result<Value, RemoteApiError> {
        self.get_run_events_after(run_id, after_seq)
    }

    fn get_run_events_after(&self, run_id: &str, after_seq: u64) -> Result<Value, RemoteApiError> {
        validate_run_id(run_id)?;
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

    fn post_typed<S: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        request: &S,
    ) -> Result<R, RemoteApiError> {
        let request = serde_json::to_value(request)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        let value = self.post(path, &request)?;
        serde_json::from_value(value)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))
    }

    fn post_mutation_typed<S: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        request: &S,
        recovery_id: Option<&str>,
    ) -> Result<R, RemoteApiError> {
        let request = serde_json::to_value(request)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        let value = self.post_mutation(path, &request, recovery_id)?;
        serde_json::from_value(value).map_err(|error| {
            unknown_outcome(
                path,
                recovery_id,
                RemoteApiError::InvalidJson(error.to_string()),
            )
        })
    }

    fn run_backtest_request(
        &self,
        spec: &crate::BacktestSpec,
    ) -> Result<crate::BacktestOutcome, RemoteApiError> {
        let path = "/app/backtest";
        let outcome: crate::BacktestOutcome =
            self.post_mutation_typed(path, spec, Some(&spec.run_id))?;
        if outcome.run_id != spec.run_id || outcome.instrument != spec.instrument {
            return Err(unknown_outcome(
                path,
                Some(&spec.run_id),
                RemoteApiError::InvalidJson(
                    "backtest response has a mismatched run or instrument".into(),
                ),
            ));
        }
        Ok(outcome)
    }

    fn verify_backtest_request(
        &self,
        outcome: &crate::BacktestOutcome,
    ) -> Result<crate::VerificationResult, RemoteApiError> {
        let result: crate::VerificationResult = self.post_typed("/app/verify", outcome)?;
        if result.run_id != outcome.run_id
            || result.verified != result.mismatches.is_empty()
            || (result.verified
                && (result.result_hash != outcome.result_hash
                    || result.data_fingerprint != outcome.data_fingerprint))
        {
            return Err(RemoteApiError::InvalidJson(
                "verification response is inconsistent with the requested run".into(),
            ));
        }
        Ok(result)
    }

    fn post_run_mutation(
        &self,
        path: &str,
        request: &Value,
        run_id: &str,
    ) -> Result<Value, RemoteApiError> {
        let value = self.post_mutation(path, request, Some(run_id))?;
        if !valid_run_state(&value, run_id) {
            return Err(unknown_outcome(
                path,
                Some(run_id),
                RemoteApiError::InvalidJson(
                    "run response has a mismatched identity or invalid status".into(),
                ),
            ));
        }
        Ok(value)
    }

    fn post_mutation_spec<S: serde::Serialize>(
        &self,
        path: &str,
        spec: &S,
        recovery_id: Option<&str>,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post_mutation(path, &request, recovery_id)
    }

    fn post_run_mutation_spec<S: serde::Serialize>(
        &self,
        path: &str,
        spec: &S,
        run_id: &str,
    ) -> Result<Value, RemoteApiError> {
        let request = serde_json::to_value(spec)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        self.post_run_mutation(path, &request, run_id)
    }

    fn post_paper_mutation(
        &self,
        path: &str,
        request: &Value,
        parent_id: &str,
    ) -> Result<Value, RemoteApiError> {
        let value = self.post_mutation(path, request, Some(parent_id))?;
        if !valid_paper_state(&value, parent_id) {
            return Err(unknown_outcome(
                path,
                Some(parent_id),
                RemoteApiError::InvalidJson(
                    "paper algorithm response has an invalid identity, schema, or status".into(),
                ),
            ));
        }
        Ok(value)
    }

    fn start_paper_request(&self, request: &Value) -> Result<Value, RemoteApiError> {
        self.post_paper_mutation(
            "/app/paper/algo-executions/start",
            request,
            paper_parent_id(request)?,
        )
    }

    fn cancel_paper_request(&self, parent_id: &str) -> Result<Value, RemoteApiError> {
        self.post_paper_mutation(
            "/app/paper/algo-executions/cancel",
            &serde_json::json!({"parent_id": parent_id}),
            parent_id,
        )
    }

    fn get_run_status(&self, run_id: &str) -> Result<Value, RemoteApiError> {
        let value = self.get_discovery(&format!("/app/runs/{run_id}"))?;
        if !valid_run_state(&value, run_id) {
            return Err(RemoteApiError::InvalidJson(
                "run response has a mismatched identity or invalid status".into(),
            ));
        }
        Ok(value)
    }

    fn get_paper_request(&self, parent_id: &str) -> Result<Value, RemoteApiError> {
        let value = self.post(
            "/app/paper/algo-executions/get",
            &serde_json::json!({"parent_id": parent_id}),
        )?;
        if !value
            .get("state")
            .is_some_and(|state| valid_paper_state(state, parent_id))
        {
            return Err(RemoteApiError::InvalidJson(
                "paper algorithm snapshot has an invalid identity, schema, or status".into(),
            ));
        }
        Ok(value)
    }

    fn post_mutation(
        &self,
        path: &str,
        request: &Value,
        recovery_id: Option<&str>,
    ) -> Result<Value, RemoteApiError> {
        // Serialization happens before transport; only failures after sending are ambiguous.
        let body = serde_json::to_string(request)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        let response = self.transport.post(path, &body).map_err(|error| {
            unknown_outcome(path, recovery_id, RemoteApiError::Transport(error))
        })?;
        let value = Self::decode_response(response).map_err(|error| match error {
            RemoteApiError::Http { status, .. }
                if !(400..500).contains(&status) || matches!(status, 408 | 409) =>
            {
                unknown_outcome(path, recovery_id, error)
            }
            RemoteApiError::InvalidJson(_) => unknown_outcome(path, recovery_id, error),
            other => other,
        })?;
        if !value.is_object() {
            return Err(unknown_outcome(
                path,
                recovery_id,
                RemoteApiError::InvalidJson(
                    "expected a JSON object for a state-changing response".into(),
                ),
            ));
        }
        Ok(value)
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

    fn get_discovery(&self, path: &str) -> Result<Value, RemoteApiError> {
        let response = self
            .transport
            .get(path)
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

fn paper_parent_id(request: &Value) -> Result<&str, RemoteApiError> {
    request
        .pointer("/spec/parent_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            RemoteApiError::InvalidJson("paper algorithm request needs spec.parent_id".into())
        })
}

fn valid_run_state(value: &Value, run_id: &str) -> bool {
    value.get("run_id").and_then(Value::as_str) == Some(run_id)
        && matches!(
            value.get("status").and_then(Value::as_str),
            Some("starting" | "running" | "cancelling" | "succeeded" | "cancelled" | "failed")
        )
}

fn valid_paper_state(value: &Value, parent_id: &str) -> bool {
    value.get("schema_version").and_then(Value::as_u64)
        == Some(u64::from(qx_execution::ALGO_EXECUTION_STATE_SCHEMA_VERSION))
        && value.get("parent_id").and_then(Value::as_str) == Some(parent_id)
        && matches!(
            value.get("status").and_then(Value::as_str),
            Some(
                "ready"
                    | "running"
                    | "cancel_requested"
                    | "completed"
                    | "cancelled"
                    | "failed"
                    | "reconcile_required"
            )
        )
}

fn unknown_outcome(path: &str, recovery_id: Option<&str>, cause: RemoteApiError) -> RemoteApiError {
    RemoteApiError::OutcomeUnknown {
        operation: path.into(),
        recovery_id: recovery_id.map(str::to_owned),
        cause: Box::new(cause),
    }
}

fn validate_run_id(run_id: &str) -> Result<(), RemoteApiError> {
    if run_id.is_empty()
        || run_id == "."
        || run_id == ".."
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(RemoteApiError::InvalidJson(
            "run_id must use ASCII letters, digits, '.', '_' or '-' and not be '.' or '..'".into(),
        ));
    }
    Ok(())
}

fn provider_request_value(request: &crate::DataRequest) -> Result<Value, RemoteApiError> {
    let json = request
        .to_json()
        .map_err(|error| RemoteApiError::InvalidJson(error.message))?;
    serde_json::from_str(&json).map_err(|error| RemoteApiError::InvalidJson(error.to_string()))
}

#[path = "remote/strategy_catalog.rs"]
mod strategy_catalog;

#[path = "remote/control_audit.rs"]
mod control_audit;

#[cfg(test)]
#[path = "remote/tests.rs"]
mod tests;
