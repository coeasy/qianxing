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

    fn post(&self, path: &str, request: &Value) -> Result<Value, RemoteApiError> {
        let body = serde_json::to_string(request)
            .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
        let response = self
            .transport
            .post(path, &body)
            .map_err(RemoteApiError::Transport)?;
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
        let calls = client.transport.0.lock().unwrap();
        assert_eq!(
            calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
            [
                "/app/paper/algo-executions/start",
                "/app/paper/algo-executions/get",
                "/app/paper/algo-executions/cancel",
                "/app/paper/algo-executions/list",
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
