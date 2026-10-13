//! API data-provider capability validation.

use super::*;
use std::collections::BTreeSet;

pub(super) fn validate_api_data_providers(
    providers: &[ApiDataProviderRuntimeConfig],
) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for provider in providers {
        if provider.provider_id.trim().is_empty()
            || provider.version.trim().is_empty()
            || provider.auth_scope.trim().is_empty()
        {
            return Err("API data Provider 必须声明 provider_id/version/auth_scope".into());
        }
        if !ids.insert(provider.provider_id.as_str()) {
            return Err(format!(
                "API data Provider provider_id 重复: {}",
                provider.provider_id
            ));
        }
        if provider.data_kinds.is_empty()
            || provider.data_kinds.iter().any(|kind| kind != "Bar")
            || provider.asset_classes.is_empty()
            || provider.frequencies.is_empty()
            || provider
                .asset_classes
                .iter()
                .chain(provider.frequencies.iter())
                .any(|value| value.trim().is_empty())
        {
            return Err(format!(
                "API data Provider {} 目前必须声明 Bar、asset_classes 和 frequencies",
                provider.provider_id
            ));
        }
        if provider.rate_limit_per_second == 0
            || provider.freshness_seconds == 0
            || provider.historical_start > provider.historical_end
            || provider.quality_score > 100
            || provider.cost_score > 100
            || provider.realtime
        {
            return Err(format!(
                "API data Provider {} 的速率/新鲜度/历史区间/评分无效，或声明了当前不支持的 realtime",
                provider.provider_id
            ));
        }
        match &provider.adapter {
            ApiDataProviderAdapterConfig::Ccxt { config_path } if config_path.trim().is_empty() => {
                return Err(format!(
                    "API data Provider {} 的 CCXT config_path 不能为空",
                    provider.provider_id
                ));
            }
            ApiDataProviderAdapterConfig::Ccxt { .. } => {}
        }
    }
    Ok(())
}
