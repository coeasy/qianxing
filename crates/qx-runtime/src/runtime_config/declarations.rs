//! Cross-entry queries over declared runtime strategy blocks.

use super::{RuntimeConfig, StrategyRuntimeConfig};

/// Return the first strategy block that explicitly declares a runtime option.
///
/// The legacy `strategy` block is checked before `strategies[]`, preserving stable diagnostic
/// paths while ensuring list entries cannot bypass configuration admission checks.
pub fn first_strategy_block_declaring_in_config(
    config: &RuntimeConfig,
    declares: fn(&StrategyRuntimeConfig) -> bool,
) -> Option<String> {
    let mut blocks = vec![("strategy".to_string(), &config.strategy)];
    blocks.extend(config.strategies.iter().map(|strategy| {
        (
            format!(
                "strategy[{}]",
                strategy.id.as_deref().unwrap_or("<missing-id>")
            ),
            strategy,
        )
    }));
    blocks
        .into_iter()
        .find(|(_, strategy)| declares(strategy))
        .map(|(label, _)| label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_lookup_covers_legacy_and_list_strategy_blocks_in_order() {
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../../deploy/qianxing.runtime.example.json"
        ))
        .expect("runtime example config should parse");
        config.strategy.ashare_rules_path = Some("rules.json".into());
        assert_eq!(
            first_strategy_block_declaring_in_config(&config, |strategy| {
                strategy.ashare_rules_path.is_some()
            })
            .as_deref(),
            Some("strategy")
        );

        config.strategy.ashare_rules_path = None;
        let mut listed = config.strategy.clone();
        listed.id = Some("secondary".into());
        listed.cost_rules_path = Some("costs.json".into());
        config.strategies.push(listed);
        assert_eq!(
            first_strategy_block_declaring_in_config(&config, |strategy| {
                strategy.cost_rules_path.is_some()
            })
            .as_deref(),
            Some("strategy[secondary]")
        );
    }
}
