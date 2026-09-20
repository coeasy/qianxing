//! 策略决策上下文：研究快照、账户观察与风险状态的只读边界。

use super::*;

/// 策略在一个决策时点看到的只读输入。研究产物、账户观察和风险状态
/// 统一进入这个边界，策略函数不能自行读取文件、Venue 或 Ledger 可变引用。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyContext {
    pub strategy_id: String,
    pub strategy_version: String,
    pub data_fingerprint: String,
    pub as_of: u64,
    pub research: qx_factor::StrategyResearchSnapshot,
    pub account_id: String,
    pub venue_id: String,
    pub positions: BTreeMap<String, i128>,
    pub cash: BTreeMap<String, i128>,
    pub available_margin_raw: Option<i128>,
    pub risk_state: String,
}

impl StrategyContext {
    pub fn validate(&self, now: u64, require_event_verified: bool) -> Result<(), String> {
        if self.strategy_id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.venue_id.trim().is_empty()
            || self.risk_state.trim().is_empty()
            || self.as_of != self.research.as_of
            || self.data_fingerprint.trim().is_empty()
            || self.available_margin_raw.is_some_and(|value| value < 0)
        {
            return Err("StrategyContext 账户、风险状态或可用保证金非法".into());
        }
        self.research
            .validate_for(
                &self.strategy_version,
                &self.data_fingerprint,
                now,
                require_event_verified,
            )
            .map_err(|error| format!("StrategyContext 研究输入非法: {error:?}"))
    }

    pub fn target_for(&self, instrument: &InstrumentId) -> Option<i128> {
        self.research.target_for(instrument)
    }

    pub fn to_contract_input(
        &self,
        request_id: impl Into<String>,
        instrument: &InstrumentId,
        bars: Option<StrategyContractBars>,
    ) -> Result<StrategyContractInput, String> {
        let research_targets = self
            .research
            .candidate
            .config
            .intended_exposure
            .iter()
            .map(|(instrument, target)| (instrument.to_string(), *target))
            .collect();
        let input = StrategyContractInput {
            schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: request_id.into(),
            strategy_id: self.strategy_id.clone(),
            strategy_version: self.strategy_version.clone(),
            data_fingerprint: self.data_fingerprint.clone(),
            as_of: self.as_of,
            instrument: instrument.to_string(),
            positions: self.positions.clone(),
            cash: self.cash.clone(),
            available_margin_raw: self.available_margin_raw,
            risk_state: self.risk_state.clone(),
            research_targets,
            bars,
        };
        input.validate()?;
        Ok(input)
    }
}
