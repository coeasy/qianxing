use crate::{app_surface, ApiResponse, ApiService};

impl ApiService {
    pub(crate) fn route_app_surface(
        &self,
        method: &str,
        route: &str,
        query: &str,
        body: &str,
    ) -> Option<ApiResponse> {
        app_surface::route_app_run(
            method,
            route,
            query,
            body,
            &self.app_paths,
            &self.app_runs,
            self.provider_registry.as_deref(),
        )
    }
}
