use serde::{Deserialize, Serialize};

/// Explicit margin model for callers that already normalized a market snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BacktestMarginConfig {
    NoMargin,
    Leverage { leverage: u32, maintenance_bp: i64 },
    Tiers { tiers: Vec<BacktestMarginTier> },
}

/// One notional band in an exchange-provided tiered margin schedule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestMarginTier {
    pub max_notional: i128,
    pub initial_bp: i64,
    pub maintenance_bp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_leverage: Option<u32>,
}

impl BacktestMarginConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self {
            Self::NoMargin => Ok(()),
            Self::Leverage {
                leverage,
                maintenance_bp,
            } if *leverage > 0 && (0..=10_000).contains(maintenance_bp) => Ok(()),
            Self::Leverage { .. } => Err(
                "BacktestSpec.margin_model leverage 必须为正且 maintenance_bp 在 0..=10000".into(),
            ),
            Self::Tiers { tiers } => {
                let mut previous = 0;
                for tier in tiers {
                    if tier.max_notional <= previous
                        || !(0..=10_000).contains(&tier.initial_bp)
                        || !(0..=10_000).contains(&tier.maintenance_bp)
                        || tier.max_leverage.is_some_and(|leverage| leverage == 0)
                    {
                        return Err(
                            "BacktestSpec.margin_model.tiers 必须按正名义额升序且 bp/leverage 合法"
                                .into(),
                        );
                    }
                    previous = tier.max_notional;
                }
                if tiers.is_empty() {
                    return Err("BacktestSpec.margin_model.tiers 不能为空".into());
                }
                Ok(())
            }
        }
    }
}
