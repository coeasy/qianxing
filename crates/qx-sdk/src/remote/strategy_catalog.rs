use super::*;

impl<T: RemoteTransport> RemoteApiClient<T> {
    /// Discover the canonical built-in Rust strategy catalog served by qx-api.
    pub fn builtin_strategy_catalog(&self) -> Result<Value, RemoteApiError> {
        self.get_strategy_catalog()
    }

    fn get_strategy_catalog(&self) -> Result<Value, RemoteApiError> {
        let response = self
            .transport
            .get("/app/strategies")
            .map_err(RemoteApiError::Transport)?;
        Self::decode_response(response)
    }
}
