use super::*;

impl<T: RemoteTransport> RemoteApiClient<T> {
    /// Discover the shared Rust application's core use cases and their routes.
    /// The response explicitly requires runtime preflight for venue readiness.
    pub fn application_capabilities(&self) -> Result<Value, RemoteApiError> {
        self.get_discovery("/app/capabilities")
    }

    /// Discover the canonical built-in Rust strategy catalog served by qx-api.
    pub fn builtin_strategy_catalog(&self) -> Result<Value, RemoteApiError> {
        self.get_strategy_catalog()
    }

    fn get_strategy_catalog(&self) -> Result<Value, RemoteApiError> {
        self.get_discovery("/app/strategies")
    }

    fn get_discovery(&self, path: &str) -> Result<Value, RemoteApiError> {
        let response = self
            .transport
            .get(path)
            .map_err(RemoteApiError::Transport)?;
        Self::decode_response(response)
    }
}
