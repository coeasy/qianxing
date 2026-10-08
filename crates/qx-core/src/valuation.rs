//! 估值单点（P1-6 / WP-19）：把「三把权益/保证金尺子」收敛成一份上下文 + 一份结果。
//!
//! 收敛前，同一个问题——「这个账户现在值多少钱、还能用多少保证金」——在仓里有三处各自
//! 成立的算法：
//!
//! 1. [`crate::ledger::Ledger`] 的 `equity*` 家族（[`crate::ledger::query`]）：现货按乘数、衍生按合约规格、
//!    跨币种再叠一层 FX。这是**算术**的正确落点，但选了哪一把由调用方自己判断。
//! 2. [`MarginState::equity`] / [`MarginState::available`]：抵押 + 未实现 − 资金费 − 利息，
//!    再减初始保证金。它描述的是保证金账户口径，与 1 的币种/规格无关。
//! 3. 回测里的 `equity_for` 派发（`qx-xingban::backtest` / `orderbook_backtest`）与 paper
//!    账户的可用保证金派发（`qx-cli::venue_runtime::worker_runtime`）：各自手工按
//!    「有杠杆规格吗 / 有汇率吗 / 是衍生品吗」在 1 的方法之间选——**同一类派发被抄了三份**。
//!
//! 收敛后只有两个入口：
//!
//! * `Ledger::valuate`：吃一份 [`ValuationContext`]，内部按规格/汇率派发到 1 的正确方法，
//!   再叠上保证金占用，吐一份 [`ValuationResult`]。调用方不再需要知道该调哪个 `equity_*`。
//!   它的实现落在 `crate::ledger::query`（本仓不允许在 `ledger/` 目录外另起 `impl Ledger`，
//!   见门禁「账簿归约实现按资产类别分文件」）。
//! * [`MarginState::valuate`]：把保证金账户口径折成**同一个** [`ValuationResult`]；
//!   [`MarginState::equity`] / [`MarginState::available`] 只是它的两个字段视图，算式不再各写一遍。
//!
//! 两个入口共用 [`ValuationResult`]：字段名、`available = equity − 保证金占用` 这条关系、
//! 以及「币种随结果一起走」都只定义一次。

use crate::error::{QxError, QxResult};
use crate::identity::InstrumentId;
use crate::numeric::{Money, Price};
use crate::trading::{MarginState, TradingInstrumentSpec};
use std::collections::BTreeMap;

/// 估值上下文：一次估值需要的全部输入，收成一份参数对象（P1-6 单点）。
///
/// 字段刻意都是借用，估值本身不产生所有权变更；`marks` / `fx_rates` 用 `&BTreeMap` 与
/// [`crate::ledger::Ledger`] 的既有口径一致（顺序敏感迭代只走 `BTreeMap`）。
#[derive(Clone, Copy, Debug)]
pub struct ValuationContext<'a> {
    /// 要估的账户。
    pub account_id: &'a str,
    /// 标记价格表：持有仓位里每个 instrument 都要能查到，否则按口径返回错误。
    pub marks: &'a BTreeMap<InstrumentId, Price>,
    /// 报什么币种（`Ledger` 现金/权益的记账币种）。
    pub reporting_currency: &'a str,
    /// 合约规格；`None` 表示现货，按 `multiplier` 折算。
    pub spec: Option<&'a TradingInstrumentSpec>,
    /// 现货乘数（`spec` 为 `None` 或非杠杆时使用）。
    pub multiplier: i128,
    /// 跨币种抵押品汇率（「1 单位资产币种 = 多少报价币种」）；空表表示不做 FX 换算。
    pub fx_rates: &'a BTreeMap<String, Price>,
    /// 初始保证金**占用**（由保证金规则算出）；现货/不评估保证金时记 0。
    ///
    /// 刻意不叫 `initial_margin`：那个名字在本仓是**持仓行观察到的**钱字段（可缺席、不许
    /// 兜零，见门禁 `position_money_honesty_check`）。这里的 0 是"这笔估值没有保证金要求"
    /// 的确定结论，不是替缺席补的占位零，两个语义不能共用一个字段名。
    pub margin_required: Money,
    /// 维持保证金占用；同上。
    pub maintenance_required: Money,
}

impl<'a> ValuationContext<'a> {
    /// 现货/无保证金上下文的便捷构造：`spec = None`、`multiplier = 1`、无 FX、保证金记 0。
    ///
    /// 需要别的口径时用 `with_*` 链式覆盖，不要在调用处逐字段拼结构体——`ValuationContext`
    /// 是唯一入口，字段拼错（比如忘给 `fx_rates`）不该由每个调用方各自负责。
    pub fn spot(
        account_id: &'a str,
        marks: &'a BTreeMap<InstrumentId, Price>,
        reporting_currency: &'a str,
    ) -> Self {
        Self {
            account_id,
            marks,
            reporting_currency,
            spec: None,
            multiplier: 1,
            fx_rates: empty_fx_rates(),
            margin_required: Money::ZERO,
            maintenance_required: Money::ZERO,
        }
    }

    /// 覆盖合约规格（链式）。
    pub fn with_spec(mut self, spec: Option<&'a TradingInstrumentSpec>) -> Self {
        self.spec = spec;
        self
    }

    /// 覆盖现货乘数（链式）。
    pub fn with_multiplier(mut self, multiplier: i128) -> Self {
        self.multiplier = multiplier;
        self
    }

    /// 覆盖跨币种汇率表（链式）。
    pub fn with_fx(mut self, fx_rates: &'a BTreeMap<String, Price>) -> Self {
        self.fx_rates = fx_rates;
        self
    }
}

/// 空汇率表：`ValuationContext::spot` 需要一个 `'static` 的借用，这里给一份进程级共享的空表。
fn empty_fx_rates() -> &'static BTreeMap<String, Price> {
    static EMPTY: std::sync::OnceLock<BTreeMap<String, Price>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(BTreeMap::new)
}

/// 估值结果：两个入口（`Ledger::valuate` / [`MarginState::valuate`]）共用的唯一出口形状。
///
/// `available = equity − margin_required` 这条关系只在 [`ValuationResult::new`] 里算一次；
/// 币种随结果一起走，避免调用方拿到一个数却不知道它是什么币。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ValuationResult {
    /// 账户权益（含未实现盈亏）。
    pub equity: Money,
    /// 初始保证金占用。
    pub initial_margin: Money,
    /// 维持保证金占用。
    pub maintenance_margin: Money,
    /// 可用保证金 = 权益 − 初始保证金占用。
    pub available: Money,
    /// 结果币种。
    pub currency: String,
}

impl ValuationResult {
    /// 由权益与保证金占用构造结果；`available` 的减法在这里单点做（溢出即报错）。
    pub fn new(
        equity: Money,
        margin_required: Money,
        maintenance_required: Money,
        currency: impl Into<String>,
    ) -> QxResult<Self> {
        let available = equity
            .raw()
            .checked_sub(margin_required.raw())
            .ok_or_else(|| QxError::Invariant("可用保证金计算溢出".into()))?;
        Ok(Self {
            equity,
            initial_margin: margin_required,
            maintenance_margin: maintenance_required,
            available: Money::from_raw(available),
            currency: currency.into(),
        })
    }
}

impl MarginState {
    /// 保证金账户口径的估值（P1-6）：折成与 `Ledger::valuate` **同一个** [`ValuationResult`]。
    ///
    /// `equity = 抵押 + 未实现 − 资金费 − 利息`；`available = equity − 初始保证金`（后者在
    /// [`ValuationResult::new`] 里单点算）。`MarginState` 不带币种，调用方给什么就记什么。
    pub fn valuate(self, currency: impl Into<String>) -> Option<ValuationResult> {
        let equity = self
            .collateral
            .raw()
            .checked_add(self.unrealized_pnl.raw())?
            .checked_sub(self.funding.raw())?
            .checked_sub(self.interest.raw())?;
        ValuationResult::new(
            Money::from_raw(equity),
            self.initial_margin,
            self.maintenance_margin,
            currency,
        )
        .ok()
    }
}
