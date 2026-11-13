use serde::{Deserialize, Serialize};

/// Explicit margin model for callers that already normalized a market snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BacktestMarginConfig {
    NoMargin,
    Leverage {
        leverage: u32,
        maintenance_bp: i64,
    },
    Tiers {
        tiers: Vec<BacktestMarginTier>,
    },
    /// Exchange tiers whose band limits are expressed as contract counts rather than quote notional.
    ContractTiers {
        tiers: Vec<BacktestContractMarginTier>,
    },
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

/// One exchange margin band whose upper limit is denominated in contracts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestContractMarginTier {
    pub max_contracts: i128,
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
            Self::Tiers { tiers } => validate_tiers(
                tiers.iter().map(|tier| {
                    (
                        tier.max_notional,
                        tier.initial_bp,
                        tier.maintenance_bp,
                        tier.max_leverage,
                    )
                }),
                "正名义额",
            ),
            Self::ContractTiers { tiers } => validate_tiers(
                tiers.iter().map(|tier| {
                    (
                        tier.max_contracts,
                        tier.initial_bp,
                        tier.maintenance_bp,
                        tier.max_leverage,
                    )
                }),
                "正合约数",
            ),
        }
    }
}

fn validate_tiers(
    tiers: impl Iterator<Item = (i128, i64, i64, Option<u32>)>,
    limit_name: &str,
) -> Result<(), String> {
    let mut previous = 0;
    let mut count = 0;
    for (limit, initial_bp, maintenance_bp, max_leverage) in tiers {
        if limit <= previous
            || !(0..=10_000).contains(&initial_bp)
            || !(0..=10_000).contains(&maintenance_bp)
            || max_leverage.is_some_and(|leverage| leverage == 0)
        {
            return Err(format!(
                "BacktestSpec.margin_model.tiers 必须按{limit_name}升序且 bp/leverage 合法"
            ));
        }
        previous = limit;
        count += 1;
    }
    if count == 0 {
        return Err("BacktestSpec.margin_model.tiers 不能为空".into());
    }
    Ok(())
}
