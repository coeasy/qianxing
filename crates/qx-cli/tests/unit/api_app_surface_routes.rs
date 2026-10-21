use super::*;
use std::collections::BTreeSet;

pub(crate) fn insert_static_app_routes(pairs: &mut BTreeSet<(String, String)>) {
    let source = workspace_source("crates/qx-api/src/app_surface.rs");
    for (method, route, handler) in [
        ("GET", "/app/strategies", "builtin_strategy_catalog()"),
        (
            "GET",
            "/app/capabilities",
            "capabilities::application_capabilities(provider_registry)",
        ),
        (
            "POST",
            "/app/capabilities/providers/preflight",
            "capabilities::provider_preflight(body, provider_registry)",
        ),
    ] {
        let branch = format!("if let (\"{method}\", \"{route}\")");
        assert!(
            source.contains(&branch) && source.contains(handler),
            "app_surface 缺少静态路由分支或 handler: {method} {route}"
        );
        pairs.insert((method.to_string(), route.to_string()));
    }
}
