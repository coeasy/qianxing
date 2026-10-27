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
            "/app/capabilities/query",
            "capabilities::product_capability_query(body)",
        ),
        (
            "POST",
            "/app/capabilities/providers/preflight",
            "capabilities::provider_preflight(body, provider_registry)",
        ),
        (
            "POST",
            "/app/research-artifacts",
            "research_artifacts::route(body, paths)",
        ),
        (
            "POST",
            "/app/read-provider-report",
            "research_artifacts::provider_bar_report_route(body, paths)",
        ),
        (
            "POST",
            "/app/read-provider-depth-report",
            "research_artifacts::provider_depth_report_route(body, paths)",
        ),
        (
            "POST",
            "/app/read-multi-account-joint-report",
            "research_artifacts::multi_account_joint_report_route(",
        ),
        (
            "POST",
            "/app/read-multi-leg-report",
            "research_artifacts::multi_leg_report_route(body, paths)",
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
