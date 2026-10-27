//! 撮合模型（FillModel 家族）。
//!
//! 每个模型必须显式声明它假设了什么、在什么数据档位下有效。
//! **没有 L2/L3 数据时不得使用 VolumeSensitive 并声称已还原订单簿**——
//! 更诚实的做法是输出"可交易容量区间"。

use qx_core::{DecimalRounding, ExactDecimal, Side};
use qx_guanxing::Bar;

use crate::execution_attempt::{ExecutionAttemptReason, FillEvaluation};
use crate::rng::DeterministicRng;

/// 数据档位：决定可见深度。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DataTier {
    /// 有价格与数量，可沿真实深度行走。
    L2L3,
    /// 只有买卖一档，成交具有不确定性。
    L1,
    /// 只有 OHLCV，最多重构有限路径。
    Bar,
}

impl DataTier {
    pub fn supports(self, model: Self) -> bool {
        matches!(
            (self, model),
            (
                DataTier::L2L3,
                DataTier::L2L3 | DataTier::L1 | DataTier::Bar
            ) | (DataTier::L1, DataTier::L1 | DataTier::Bar)
                | (DataTier::Bar, DataTier::Bar)
        )
    }
}

pub struct FillContext<'a> {
    pub side: Side,
    /// 剩余未成交数量（定点原始值）。
    pub qty: i128,
    /// 限价；None 为市价。
    pub limit: Option<i128>,
    /// 当前 bar（bar 级撮合下用 open 作为成交参考）。
    pub bar: &'a Bar,
    pub halted: bool,
}

/// Scale-aware counterpart to [`FillContext`]. The OHLC price and volume source remains the
/// declared Bar scale, while order quantity and limit values retain their exact decimal scale.
pub struct ExactFillContext<'a> {
    pub side: Side,
    pub qty: ExactDecimal,
    pub limit: Option<ExactDecimal>,
    pub bar: &'a Bar,
    pub halted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExactFillEvaluation {
    pub fill: Option<(ExactDecimal, ExactDecimal)>,
    pub no_fill_reason: ExecutionAttemptReason,
}

impl ExactFillEvaluation {
    pub fn filled(price: ExactDecimal, qty: ExactDecimal) -> Self {
        Self {
            fill: Some((price, qty)),
            no_fill_reason: ExecutionAttemptReason::ModelDeclined,
        }
    }

    pub fn declined(reason: ExecutionAttemptReason) -> Self {
        Self {
            fill: None,
            no_fill_reason: reason,
        }
    }
}

/// Machine-readable model identity and assumptions for summaries and reports.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct FillModelDescriptor {
    pub name: String,
    pub version: String,
    pub data_tier: String,
    pub parameters: String,
    pub assumption: String,
}

/// 限价是否满足：买单价不得高于限价，卖单价不得低于限价。
pub(crate) fn limit_ok(side: Side, limit: Option<i128>, px: i128) -> bool {
    match limit {
        None => true,
        Some(l) => match side {
            Side::Buy => px <= l,
            Side::Sell => px >= l,
        },
    }
}

fn raw_decimal(value: i128) -> ExactDecimal {
    ExactDecimal::from_parts(value, 9).expect("legacy Bar values use supported scale 9")
}

fn limit_ok_exact(side: Side, limit: Option<ExactDecimal>, px: ExactDecimal) -> bool {
    match limit {
        None => true,
        Some(limit) => match (side, px.checked_cmp(limit)) {
            (Side::Buy, Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal))
            | (Side::Sell, Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)) => true,
            _ => false,
        },
    }
}

/// 撮合模型：返回 (成交价, 成交量)，None 表示本轮不成交。
pub trait FillModel {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str {
        "v1"
    }
    fn tier(&self) -> DataTier;

    /// 声明本模型的不可知假设，用于审计输出。
    fn assumption(&self) -> &'static str;

    fn parameters(&self) -> String {
        String::new()
    }

    /// Optional cumulative participation cap shared by every order matched on one Bar.
    /// `None` preserves the model's ordinary capacity assumptions.
    fn bar_volume_participation_bps(&self) -> Option<u16> {
        None
    }

    fn audit_descriptor(&self) -> FillModelDescriptor {
        FillModelDescriptor {
            name: self.name().to_string(),
            version: self.version().to_string(),
            data_tier: format!("{:?}", self.tier()),
            parameters: self.parameters(),
            assumption: self.assumption().to_string(),
        }
    }

    fn descriptor(&self) -> String {
        let descriptor = self.audit_descriptor();
        format!(
            "{}@{}[tier={};params={};assumption={}]",
            descriptor.name,
            descriptor.version,
            descriptor.data_tier,
            descriptor.parameters,
            descriptor.assumption
        )
    }

    fn fill(&self, ctx: &FillContext, rng: &mut DeterministicRng) -> Option<(i128, i128)>;

    /// True when this model implements scale-aware quantities directly. Models that only
    /// implement the legacy callback can still receive exact inputs that project losslessly
    /// onto the historical 1e-9 scale.
    fn supports_exact_amounts(&self) -> bool {
        false
    }

    fn can_evaluate_exact(&self, ctx: &ExactFillContext<'_>) -> bool {
        self.supports_exact_amounts()
            || (ctx.qty.to_raw(9).is_ok() && ctx.limit.is_none_or(|limit| limit.to_raw(9).is_ok()))
    }

    /// Evaluate an exact order. The compatibility implementation delegates to the old
    /// callback only when every order amount is exactly representable at scale 1e-9.
    fn evaluate_exact(
        &self,
        ctx: &ExactFillContext<'_>,
        rng: &mut DeterministicRng,
    ) -> ExactFillEvaluation {
        let (Ok(qty), Some(limit)) = (
            ctx.qty.to_raw(9),
            match ctx.limit {
                Some(limit) => limit.to_raw(9).ok().map(Some),
                None => Some(None),
            },
        ) else {
            return ExactFillEvaluation::declined(ExecutionAttemptReason::ExactAmountsUnsupported);
        };
        let legacy = FillContext {
            side: ctx.side,
            qty,
            limit,
            bar: ctx.bar,
            halted: ctx.halted,
        };
        self.evaluate(&legacy, rng)
            .fill
            .and_then(|(price, qty)| Some((raw_decimal(price), raw_decimal(qty))))
            .map_or_else(
                || ExactFillEvaluation::declined(ExecutionAttemptReason::ModelDeclined),
                |(price, qty)| ExactFillEvaluation::filled(price, qty),
            )
    }

    /// Evaluate once and retain a machine-readable explanation for the attempt. Third-party
    /// models remain source-compatible and default to a generic decline reason until they
    /// override this method.
    fn evaluate(&self, ctx: &FillContext, rng: &mut DeterministicRng) -> FillEvaluation {
        self.fill(ctx, rng).map_or_else(
            || FillEvaluation::declined(ExecutionAttemptReason::ModelDeclined),
            |(price, qty)| FillEvaluation::filled(price, qty),
        )
    }
}

/// Decorates an OHLCV fill model with a cumulative per-Bar market-volume participation cap.
/// Unlike `VolumeSensitiveFillModel`, this does not claim to reconstruct order-book depth.
pub struct BarVolumeParticipationLimit {
    inner: Box<dyn FillModel>,
    participation_bps: u16,
}

impl BarVolumeParticipationLimit {
    pub fn new(inner: Box<dyn FillModel>, participation_bps: u16) -> Result<Self, String> {
        if participation_bps == 0 || participation_bps > 10_000 {
            return Err("Bar 成交量参与率必须在 1..=10000 bps 内".into());
        }
        Ok(Self {
            inner,
            participation_bps,
        })
    }
}

impl FillModel for BarVolumeParticipationLimit {
    fn name(&self) -> &'static str {
        "BarVolumeParticipationLimit"
    }

    fn version(&self) -> &'static str {
        "v1"
    }

    fn tier(&self) -> DataTier {
        self.inner.tier()
    }

    fn assumption(&self) -> &'static str {
        "底层FillModel定价假设保持不变；所有订单每根Bar累计成交量不超过Bar市场成交量声明比例；不重建盘口或队列"
    }

    fn parameters(&self) -> String {
        format!(
            "inner={}@{}({});participation_bps={}",
            self.inner.name(),
            self.inner.version(),
            self.inner.parameters(),
            self.participation_bps
        )
    }

    fn bar_volume_participation_bps(&self) -> Option<u16> {
        Some(self.participation_bps)
    }

    fn fill(&self, ctx: &FillContext, rng: &mut DeterministicRng) -> Option<(i128, i128)> {
        self.inner.fill(ctx, rng)
    }

    fn supports_exact_amounts(&self) -> bool {
        self.inner.supports_exact_amounts()
    }

    fn can_evaluate_exact(&self, ctx: &ExactFillContext<'_>) -> bool {
        self.inner.can_evaluate_exact(ctx)
    }

    fn evaluate_exact(
        &self,
        ctx: &ExactFillContext<'_>,
        rng: &mut DeterministicRng,
    ) -> ExactFillEvaluation {
        self.inner.evaluate_exact(ctx, rng)
    }

    fn evaluate(&self, ctx: &FillContext, rng: &mut DeterministicRng) -> FillEvaluation {
        self.inner.evaluate(ctx, rng)
    }
}

/// 下一根 bar 开盘价成交——**结构上杜绝同 bar 作弊**。
pub struct NextBarOpenFillModel;

impl FillModel for NextBarOpenFillModel {
    fn name(&self) -> &'static str {
        "NextBarOpen"
    }
    fn tier(&self) -> DataTier {
        DataTier::Bar
    }
    fn assumption(&self) -> &'static str {
        "假设下一根bar开盘可全部成交；不建模排队与容量"
    }
    fn fill(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> Option<(i128, i128)> {
        self.evaluate(ctx, _rng).fill
    }

    fn supports_exact_amounts(&self) -> bool {
        true
    }

    fn evaluate_exact(
        &self,
        ctx: &ExactFillContext<'_>,
        _rng: &mut DeterministicRng,
    ) -> ExactFillEvaluation {
        if ctx.halted {
            return ExactFillEvaluation::declined(ExecutionAttemptReason::MarketHalted);
        }
        let price = ctx.bar.open_exact();
        if limit_ok_exact(ctx.side, ctx.limit, price) {
            ExactFillEvaluation::filled(price, ctx.qty)
        } else {
            ExactFillEvaluation::declined(ExecutionAttemptReason::LimitNotMarketable)
        }
    }
    fn evaluate(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> FillEvaluation {
        if ctx.halted {
            return FillEvaluation::declined(ExecutionAttemptReason::MarketHalted);
        }
        let px = ctx.bar.open;
        if limit_ok(ctx.side, ctx.limit, px) {
            FillEvaluation::filled(px, ctx.qty)
        } else {
            FillEvaluation::declined(ExecutionAttemptReason::LimitNotMarketable)
        }
    }
}

/// 最优价无限流动性：容量几乎无限、抢流动性场景的上界。
pub struct BestPriceFillModel;

impl FillModel for BestPriceFillModel {
    fn name(&self) -> &'static str {
        "BestPrice"
    }
    fn tier(&self) -> DataTier {
        DataTier::Bar
    }
    fn assumption(&self) -> &'static str {
        "假设最优买卖价提供无限数量——乐观上界，不可用于容量评估"
    }
    fn fill(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> Option<(i128, i128)> {
        self.evaluate(ctx, _rng).fill
    }

    fn supports_exact_amounts(&self) -> bool {
        true
    }

    fn evaluate_exact(
        &self,
        ctx: &ExactFillContext<'_>,
        _rng: &mut DeterministicRng,
    ) -> ExactFillEvaluation {
        let price = ctx.bar.close_exact();
        if limit_ok_exact(ctx.side, ctx.limit, price) {
            ExactFillEvaluation::filled(price, ctx.qty)
        } else {
            ExactFillEvaluation::declined(ExecutionAttemptReason::LimitNotMarketable)
        }
    }
    fn evaluate(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> FillEvaluation {
        let px = ctx.bar.close;
        if limit_ok(ctx.side, ctx.limit, px) {
            FillEvaluation::filled(px, ctx.qty)
        } else {
            FillEvaluation::declined(ExecutionAttemptReason::LimitNotMarketable)
        }
    }
}

/// 固定一档滑点：保守上界或延迟近似。
pub struct OneTickSlippageFillModel {
    pub tick: i128,
}

impl FillModel for OneTickSlippageFillModel {
    fn name(&self) -> &'static str {
        "OneTickSlippage"
    }
    fn tier(&self) -> DataTier {
        DataTier::Bar
    }
    fn assumption(&self) -> &'static str {
        "所有订单固定滑点一档——保守上界"
    }
    fn parameters(&self) -> String {
        format!("tick={}", self.tick)
    }
    fn fill(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> Option<(i128, i128)> {
        self.evaluate(ctx, _rng).fill
    }

    fn supports_exact_amounts(&self) -> bool {
        true
    }

    fn evaluate_exact(
        &self,
        ctx: &ExactFillContext<'_>,
        _rng: &mut DeterministicRng,
    ) -> ExactFillEvaluation {
        let tick = raw_decimal(self.tick);
        let close = ctx.bar.close_exact();
        let price = match ctx.side {
            Side::Buy => close.checked_add(tick),
            Side::Sell => close.checked_sub(tick),
        };
        let Some(price) = price.filter(|price| price.is_positive()) else {
            return ExactFillEvaluation::declined(ExecutionAttemptReason::InvalidModelOutput);
        };
        if limit_ok_exact(ctx.side, ctx.limit, price) {
            ExactFillEvaluation::filled(price, ctx.qty)
        } else {
            ExactFillEvaluation::declined(ExecutionAttemptReason::LimitNotMarketable)
        }
    }
    fn evaluate(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> FillEvaluation {
        let px = match ctx.side {
            Side::Buy => ctx.bar.close + self.tick,
            Side::Sell => ctx.bar.close - self.tick,
        };
        if limit_ok(ctx.side, ctx.limit, px) {
            FillEvaluation::filled(px, ctx.qty)
        } else {
            FillEvaluation::declined(ExecutionAttemptReason::LimitNotMarketable)
        }
    }
}

/// L1 档位下的概率成交：触及限价不一定成交。
pub struct ProbabilisticFillModel {
    /// 触及限价后的成交概率，定点 [0, 1e9]。
    pub prob_fill_on_limit: i128,
    /// 一档滑点大小。
    pub tick: i128,
}

impl FillModel for ProbabilisticFillModel {
    fn name(&self) -> &'static str {
        "Probabilistic"
    }
    fn tier(&self) -> DataTier {
        DataTier::L1
    }
    fn assumption(&self) -> &'static str {
        "最优价与差一档之间按概率选择——建模L1下的成交不确定性"
    }
    fn parameters(&self) -> String {
        format!(
            "prob_fill_on_limit={};tick={}",
            self.prob_fill_on_limit, self.tick
        )
    }
    fn fill(&self, ctx: &FillContext, rng: &mut DeterministicRng) -> Option<(i128, i128)> {
        self.evaluate(ctx, rng).fill
    }

    fn supports_exact_amounts(&self) -> bool {
        true
    }

    fn evaluate_exact(
        &self,
        ctx: &ExactFillContext<'_>,
        rng: &mut DeterministicRng,
    ) -> ExactFillEvaluation {
        if rng.next_prob() >= self.prob_fill_on_limit {
            return ExactFillEvaluation::declined(ExecutionAttemptReason::ProbabilityMiss);
        }
        let base = ctx.bar.close_exact();
        let tick = raw_decimal(self.tick);
        let price = match ctx.side {
            Side::Buy => base.checked_add(tick),
            Side::Sell => base.checked_sub(tick),
        };
        let Some(price) = price.filter(|price| price.is_positive()) else {
            return ExactFillEvaluation::declined(ExecutionAttemptReason::InvalidModelOutput);
        };
        if limit_ok_exact(ctx.side, ctx.limit, price) {
            ExactFillEvaluation::filled(price, ctx.qty)
        } else {
            ExactFillEvaluation::declined(ExecutionAttemptReason::LimitNotMarketable)
        }
    }
    fn evaluate(&self, ctx: &FillContext, rng: &mut DeterministicRng) -> FillEvaluation {
        if rng.next_prob() >= self.prob_fill_on_limit {
            return FillEvaluation::declined(ExecutionAttemptReason::ProbabilityMiss);
        }
        let base = ctx.bar.close;
        let px = match ctx.side {
            Side::Buy => base + self.tick,
            Side::Sell => base - self.tick,
        };
        if limit_ok(ctx.side, ctx.limit, px) {
            FillEvaluation::filled(px, ctx.qty)
        } else {
            FillEvaluation::declined(ExecutionAttemptReason::LimitNotMarketable)
        }
    }
}

/// 成交量敏感：最优价只能吃掉近期成交量的一部分。
pub struct VolumeSensitiveFillModel {
    /// 可吃掉的成交量比例（基点，万分之一）。
    pub frac_bp: i128,
}

impl FillModel for VolumeSensitiveFillModel {
    fn name(&self) -> &'static str {
        "VolumeSensitive"
    }
    fn tier(&self) -> DataTier {
        DataTier::L2L3
    }
    fn assumption(&self) -> &'static str {
        "最优价容量=近期成交量×比例；需要L2/L3支撑，否则会高估可得流动性"
    }
    fn parameters(&self) -> String {
        format!("frac_bp={}", self.frac_bp)
    }
    fn fill(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> Option<(i128, i128)> {
        self.evaluate(ctx, _rng).fill
    }

    fn supports_exact_amounts(&self) -> bool {
        true
    }

    fn evaluate_exact(
        &self,
        ctx: &ExactFillContext<'_>,
        _rng: &mut DeterministicRng,
    ) -> ExactFillEvaluation {
        if self.frac_bp < 0 {
            return ExactFillEvaluation::declined(ExecutionAttemptReason::InvalidModelOutput);
        }
        let fraction = match ExactDecimal::from_parts(self.frac_bp, 0) {
            Ok(value) => value,
            Err(_) => {
                return ExactFillEvaluation::declined(ExecutionAttemptReason::InvalidModelOutput);
            }
        };
        let denominator = ExactDecimal::from_parts(10_000, 0).expect("constant is valid");
        let capacity = ctx
            .bar
            .volume_exact()
            .checked_mul_at_scale(
                fraction,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::TowardZero,
            )
            .and_then(|value| {
                value.checked_div_at_scale(
                    denominator,
                    ExactDecimal::MAX_SCALE,
                    DecimalRounding::TowardZero,
                )
            });
        let Some(capacity) = capacity.filter(|capacity| capacity.is_positive()) else {
            return ExactFillEvaluation::declined(ExecutionAttemptReason::NoCapacity);
        };
        let qty = if ctx
            .qty
            .checked_cmp(capacity)
            .is_some_and(|ordering| ordering.is_gt())
        {
            capacity
        } else {
            ctx.qty
        };
        let price = ctx.bar.close_exact();
        if limit_ok_exact(ctx.side, ctx.limit, price) {
            ExactFillEvaluation::filled(price, qty)
        } else {
            ExactFillEvaluation::declined(ExecutionAttemptReason::LimitNotMarketable)
        }
    }
    fn evaluate(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> FillEvaluation {
        let capacity = (ctx.bar.volume * self.frac_bp) / 10_000;
        if capacity <= 0 {
            return FillEvaluation::declined(ExecutionAttemptReason::NoCapacity);
        }
        let q = ctx.qty.min(capacity);
        let px = ctx.bar.close;
        if limit_ok(ctx.side, ctx.limit, px) {
            FillEvaluation::filled(px, q)
        } else {
            FillEvaluation::declined(ExecutionAttemptReason::LimitNotMarketable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar() -> Bar {
        Bar::new(10, 100, 110, 90, 105, 1_000)
    }

    #[test]
    fn next_bar_open_fills_at_open() {
        let b = bar();
        let ctx = FillContext {
            side: Side::Buy,
            qty: 10,
            limit: None,
            bar: &b,
            halted: false,
        };
        let m = NextBarOpenFillModel;
        let (px, q) = m.fill(&ctx, &mut DeterministicRng::new(1)).unwrap();
        assert_eq!(px, 100); // open，不是 close
        assert_eq!(q, 10);
    }

    #[test]
    fn limit_price_is_respected() {
        let b = bar();
        let ctx = FillContext {
            side: Side::Buy,
            qty: 10,
            limit: Some(99),
            bar: &b,
            halted: false,
        };
        assert!(NextBarOpenFillModel
            .fill(&ctx, &mut DeterministicRng::new(1))
            .is_none());
    }

    #[test]
    fn halted_never_fills() {
        let b = bar();
        let ctx = FillContext {
            side: Side::Buy,
            qty: 10,
            limit: None,
            bar: &b,
            halted: true,
        };
        assert!(NextBarOpenFillModel
            .fill(&ctx, &mut DeterministicRng::new(1))
            .is_none());
    }

    #[test]
    fn volume_caps_fill_size() {
        let b = bar();
        let ctx = FillContext {
            side: Side::Buy,
            qty: 10_000,
            limit: None,
            bar: &b,
            halted: false,
        };
        let m = VolumeSensitiveFillModel { frac_bp: 1000 }; // 10%
        let (_, q) = m.fill(&ctx, &mut DeterministicRng::new(1)).unwrap();
        assert_eq!(q, 100); // 1000 * 10%
    }

    #[test]
    fn probabilistic_respects_seed() {
        let b = bar();
        let ctx = FillContext {
            side: Side::Buy,
            qty: 1,
            limit: None,
            bar: &b,
            halted: false,
        };
        let m = ProbabilisticFillModel {
            prob_fill_on_limit: 500_000_000,
            tick: 0,
        };
        let mut r1 = DeterministicRng::new(99);
        let mut r2 = DeterministicRng::new(99);
        let a: Vec<bool> = (0..50).map(|_| m.fill(&ctx, &mut r1).is_some()).collect();
        let c: Vec<bool> = (0..50).map(|_| m.fill(&ctx, &mut r2).is_some()).collect();
        assert_eq!(a, c);
    }
}
