use super::*;
use std::collections::BTreeSet;

pub(crate) fn insert_strategy_catalog(pairs: &mut BTreeSet<(String, String)>) {
    let source = workspace_source("crates/qx-api/src/app_surface.rs");
    assert!(
        source.contains("if let (\"GET\", \"/app/strategies\")"),
        "app_surface 缺少策略目录静态路由分支"
    );
    pairs.insert(("GET".to_string(), "/app/strategies".to_string()));
}
