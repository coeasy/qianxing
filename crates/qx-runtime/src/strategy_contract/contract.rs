//! Runtime-only operations over the Strategy SDK's stable wire contract.

pub use qx_strategy::contract::{
    encode_strategy_columnar_input, StrategyAccountUpdateV1, StrategyCheckpointV1,
    StrategyContractBars, StrategyContractInput, StrategyContractIntent,
    StrategyContractMarketOrderBookV1, StrategyContractMarketTickV1,
    StrategyContractOrderBookLevelV1, StrategyContractOutput, STRATEGY_CHECKPOINT_SCHEMA_VERSION,
    STRATEGY_COLUMNAR_EXACT_OHLCV_FLAG, STRATEGY_COLUMNAR_EXACT_VERSION,
    STRATEGY_COLUMNAR_HEADER_LEN, STRATEGY_COLUMNAR_MAGIC, STRATEGY_COLUMNAR_VERSION,
    STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION, STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION,
    STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION, STRATEGY_CONTRACT_INPUT_V5_SCHEMA_VERSION,
    STRATEGY_CONTRACT_INPUT_V6_SCHEMA_VERSION, STRATEGY_CONTRACT_INPUT_V7_SCHEMA_VERSION,
    STRATEGY_CONTRACT_INPUT_V8_SCHEMA_VERSION, STRATEGY_CONTRACT_INPUT_V9_SCHEMA_VERSION,
    STRATEGY_CONTRACT_SCHEMA_VERSION,
};

/// Normalize a native Rust strategy decision into the same response contract used by process
/// workers. The runtime owns request identity, single-instrument fallback, and input validation
/// because those rules are part of the strategy invocation boundary rather than a CLI concern.
pub fn native_decision_to_contract_output(
    decision: &qx_strategy::StrategyDecision,
    input: &StrategyContractInput,
) -> Result<StrategyContractOutput, String> {
    let mut output = StrategyContractOutput::from_native_decision(decision)?;
    output.request_id = input.request_id.clone();
    output.strategy_id = input.strategy_id.clone();
    if output.instrument.is_empty() {
        output.instrument = input.instrument.clone();
    }
    if output.intents.is_empty() {
        output.target_qty = input
            .positions
            .get(&input.instrument)
            .copied()
            .unwrap_or_default();
    }
    output.validate_for(input)?;
    Ok(output)
}

/// Runtime portfolio operation kept outside the SDK wire types to avoid an OMS dependency there.
pub trait StrategyContractRebalanceExt {
    fn build_rebalance_plan(
        &self,
        input: &StrategyContractInput,
        max_turnover_bps: u32,
        min_trade_size: i128,
    ) -> Result<qx_zhenlu::portfolio::RebalancePlan, String>;

    #[cfg(test)]
    fn to_json_for(&self, input: &StrategyContractInput) -> Result<String, String>;
}

impl StrategyContractRebalanceExt for StrategyContractOutput {
    fn build_rebalance_plan(
        &self,
        input: &StrategyContractInput,
        max_turnover_bps: u32,
        min_trade_size: i128,
    ) -> Result<qx_zhenlu::portfolio::RebalancePlan, String> {
        self.validate_for(input)?;
        if !self.intents.is_empty() {
            return Err("包含 intents[] 的策略输出不能再次转换为 target_qty 调仓计划".into());
        }
        if self.target_qty_exact.is_some() {
            return Err(
                "target_qty_exact 需要按精确仓位构造 Live 订单，不能投影为 raw 调仓计划".into(),
            );
        }
        let current = qx_zhenlu::portfolio::PortfolioState {
            portfolio_id: input.strategy_id.clone(),
            timestamp: input.as_of,
            cash: input
                .cash
                .values()
                .copied()
                .fold(0_i128, i128::saturating_add),
            positions: input.positions.clone(),
        };
        qx_zhenlu::portfolio::rebalance(
            &current,
            &[qx_core::TargetPosition::single(
                qx_core::InstrumentId::parse(&self.instrument)
                    .ok_or_else(|| format!("instrument 非法: {}", self.instrument))?,
                self.target_qty,
            )],
            &qx_zhenlu::portfolio::PortfolioConstraint {
                max_turnover_bps,
                min_trade_size,
            },
        )
    }

    #[cfg(test)]
    fn to_json_for(&self, input: &StrategyContractInput) -> Result<String, String> {
        self.validate_for(input)?;
        serde_json::to_string(self).map_err(|error| format!("策略输出契约序列化失败: {error}"))
    }
}
