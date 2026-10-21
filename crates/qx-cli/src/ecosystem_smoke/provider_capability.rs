use qx_provider::{DataKind, ProviderCapability};

pub(crate) fn provider_capability(id: &str, priority: u32) -> ProviderCapability {
    ProviderCapability {
        provider_id: id.into(),
        version: "v1".into(),
        data_kinds: [DataKind::Bar].into_iter().collect(),
        data_tiers: [qx_provider::MarketDataTier::Bar].into_iter().collect(),
        asset_classes: ["crypto".into()].into_iter().collect(),
        frequencies: ["1d".into()].into_iter().collect(),
        auth_scope: "public".into(),
        rate_limit_per_second: 10,
        freshness_seconds: 60,
        historical_start: 0,
        historical_end: u64::MAX,
        realtime: false,
        priority,
        quality_score: 100,
        cost_score: 1,
    }
}
