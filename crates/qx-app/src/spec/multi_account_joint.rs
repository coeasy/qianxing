//! Joint reporting across independently funded exchange accounts.

use super::is_identity_token;
use super::multi_market::{MultiMarketBacktestMarket, MultiMarketBacktestOutcome};
use crate::error::{AppError, AppErrorCategory};
use qx_core::InstrumentId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MULTI_ACCOUNT_JOINT_BACKTEST_SCHEMA_VERSION: u32 = 1;
pub const MAX_JOINT_ACCOUNTS: usize = 32;
pub const MAX_JOINT_MARKETS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JointBacktestAccount {
    /// CCXT exchange id such as `okx`, `binance`, or `bitget`.
    pub exchange_id: String,
    pub account_id: String,
    pub initial_cash_raw: i128,
    pub markets: Vec<MultiMarketBacktestMarket>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiAccountJointBacktestSpec {
    pub schema_version: u32,
    pub run_id: String,
    pub settlement_currency: String,
    pub seed: u64,
    pub output_dir: String,
    pub accounts: Vec<JointBacktestAccount>,
}

impl MultiAccountJointBacktestSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != MULTI_ACCOUNT_JOINT_BACKTEST_SCHEMA_VERSION {
            return Err(invalid(format!(
                "MultiAccountJointBacktestSpec schema_version={} 本版本只认 {}",
                self.schema_version, MULTI_ACCOUNT_JOINT_BACKTEST_SCHEMA_VERSION
            )));
        }
        if !is_identity_token(&self.run_id)
            || self.settlement_currency.trim().is_empty()
            || self.output_dir.trim().is_empty()
        {
            return Err(invalid(
                "联合账户回测 run_id/结算币/产物目录不能为空且 run_id 必须规范",
            ));
        }
        if self.settlement_currency.to_ascii_uppercase() != self.settlement_currency {
            return Err(invalid("settlement_currency 必须使用大写规范代码"));
        }
        if !(2..=MAX_JOINT_ACCOUNTS).contains(&self.accounts.len()) {
            return Err(invalid(format!(
                "联合账户回测 accounts 数量必须在 2..={MAX_JOINT_ACCOUNTS}"
            )));
        }
        let mut account_ids = BTreeSet::new();
        let mut total_cash = 0_i128;
        let mut total_markets = 0_usize;
        for account in &self.accounts {
            let exchange_id = account.exchange_id.trim().to_ascii_lowercase();
            if !matches!(exchange_id.as_str(), "okx" | "binance" | "bitget")
                || account.exchange_id != exchange_id
            {
                return Err(invalid(format!(
                    "exchange_id 目前支持规范小写 okx/binance/bitget，实际 {:?}",
                    account.exchange_id
                )));
            }
            if !is_identity_token(&account.account_id)
                || !account_ids.insert((exchange_id.clone(), account.account_id.as_str()))
            {
                return Err(invalid(format!(
                    "联合账户 account_id 必须规范且同一交易所内唯一: {}",
                    account.account_id
                )));
            }
            if account.initial_cash_raw <= 0 {
                return Err(invalid(format!(
                    "账户 {} 初始资金必须为正数",
                    account.account_id
                )));
            }
            total_cash = total_cash
                .checked_add(account.initial_cash_raw)
                .ok_or_else(|| invalid("联合账户初始资金合计溢出"))?;
            if account.markets.is_empty() {
                return Err(invalid(format!(
                    "账户 {} 至少需要一个回测标的",
                    account.account_id
                )));
            }
            total_markets = total_markets
                .checked_add(account.markets.len())
                .ok_or_else(|| invalid("联合账户市场数量溢出"))?;
            if total_markets > MAX_JOINT_MARKETS {
                return Err(invalid(format!(
                    "联合账户市场总数不得超过 {MAX_JOINT_MARKETS}"
                )));
            }
            let mut member_spec = crate::MultiMarketBacktestSpec {
                schema_version: crate::MULTI_MARKET_BACKTEST_SCHEMA_VERSION,
                run_id: format!("joint-check-{}", account.account_id),
                account_id: account.account_id.clone(),
                settlement_currency: self.settlement_currency.clone(),
                initial_cash_raw: account.initial_cash_raw,
                initial_cash_exact: None,
                seed: self.seed,
                output_dir: self.output_dir.clone(),
                markets: account.markets.clone(),
            };
            // Child ids only participate in nested validation; the runner replaces them with
            // deterministic bounded ids before writing artifacts.
            if !is_identity_token(&member_spec.run_id) {
                member_spec.run_id = "joint-account-check".into();
            }
            member_spec.validate()?;
            for market in &account.markets {
                let instrument = InstrumentId::parse(&market.instrument)
                    .filter(|parsed| parsed.to_string() == market.instrument)
                    .ok_or_else(|| {
                        invalid(format!("InstrumentId 不规范: {}", market.instrument))
                    })?;
                if !instrument
                    .venue
                    .to_string()
                    .eq_ignore_ascii_case(&exchange_id)
                {
                    return Err(invalid(format!(
                        "市场 {} venue 与账户交易所 {} 不一致",
                        instrument, account.exchange_id
                    )));
                }
            }
        }
        if total_cash <= 0 {
            return Err(invalid("联合账户初始资金合计必须为正数"));
        }
        Ok(())
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload).map_err(|error| {
            invalid(format!("MultiAccountJointBacktestSpec JSON 无效: {error}"))
        })?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JointAccountBacktestResult {
    pub exchange_id: String,
    pub account_id: String,
    pub initial_cash_raw: i128,
    pub outcome: MultiMarketBacktestOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountJointBacktestOutcome {
    pub run_id: String,
    pub settlement_currency: String,
    pub spec_fingerprint: String,
    pub accounts_total: usize,
    pub markets_total: usize,
    pub initial_cash_raw: i128,
    pub final_equity_raw: i128,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub successful_fills: u64,
    pub equity_path: String,
    pub fills_path: String,
    pub artifact_path: String,
    /// Parent RunManifest reference; absent only in historical v1 outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_manifest_path: Option<String>,
    /// Parent RunManifest result digest; absent only for historical joint reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_hash: Option<String>,
    /// RunRecord V2 pointer; absent only for historical joint reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_record: Option<String>,
    /// Authorized storage locations for RunRecord V2 content IDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_locations: Option<BTreeMap<String, String>>,
    pub accounts: Vec<JointAccountBacktestResult>,
}

impl MultiAccountJointBacktestOutcome {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
