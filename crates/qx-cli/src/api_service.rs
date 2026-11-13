//! CLI host adapter for the configured API service.
//!
//! API assembly and read-only projections are owned by `qx-service`; this module retains
//! process-bound panic handling and supplies CLI-specific readiness/algorithm callbacks.

use crate::catch_panic;
use qx_api::ApiService;
use qx_runtime::RuntimeConfig;
use std::path::Path;

mod api_service_builder;

pub(crate) fn build_configured_api_service(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
) -> Result<ApiService, String> {
    catch_panic("api-service", || {
        api_service_builder::build_configured_api_service_inner(config, runtime_config_path)
    })
}
