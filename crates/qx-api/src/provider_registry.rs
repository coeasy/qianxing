use super::ApiService;
use qx_provider::ProviderRegistry;
use std::sync::Arc;

impl ApiService {
    /// Attach the deployment-owned Provider registry used by Provider-backed Bar/depth routes.
    /// Registry construction, credentials and adapter lifetimes remain the host's responsibility.
    pub fn with_provider_registry(mut self, registry: Arc<ProviderRegistry>) -> Self {
        self.provider_registry = Some(registry);
        self
    }
}
