//! Bar 链撮合模型的单点装配（V11 Q1a 第二批）。
//!
//! 内核的 `FillModel` 家族有五个成员，Bar 输入只撑得起三个；本模块就是那条分界线：
//! 谁能装进 Bar 装配、谁必须被拒绝、拒绝时说什么，全在这里，四条 Bar 回测链共用。
//! 口径与 [`super::BarBacktestAssembly`] 的费用/延迟绑定同构——那是"配置了就必须生效"，
//! 这是"生效的必须是输入撑得起的"。

use super::*;
use qx_xingban::{BestPriceFillModel, NextBarOpenFillModel, OneTickSlippageFillModel};

/// 本入口可达的撮合模型清单。**这张表就是命令面**：多一行就多一种成交口径。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BarFillModel {
    /// 下一根 Bar 开盘成交（结构上杜绝同 bar 作弊）。
    NextBarOpen,
    /// 当根 Bar 收盘成交、无限流动性：抢流动性的乐观上界。
    BestPrice,
    /// 当根 Bar 收盘价加一档滑点：保守上界。
    OneTickSlippage,
}

impl BarFillModel {
    const REACHABLE: [Self; 3] = [Self::NextBarOpen, Self::BestPrice, Self::OneTickSlippage];

    /// 配置里写的名字，与 [`Self::parse`] 一一对应。
    fn name(self) -> &'static str {
        match self {
            Self::NextBarOpen => "next_bar_open",
            Self::BestPrice => "best_price",
            Self::OneTickSlippage => "one_tick_slippage",
        }
    }

    /// 报错里那份"能用什么"的清单：它与 [`Self::REACHABLE`] 同源，所以不会写着写着
    /// 就多出一个早就被删掉的模型名。
    fn reachable() -> String {
        Self::REACHABLE
            .iter()
            .map(|kind| kind.name())
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// 配置名 → 模型。名字不合法时报出可达清单，而不是只说"未知"。
    fn parse(configured: &str) -> Result<Self, String> {
        let trimmed = configured.trim();
        if let Some(kind) = Self::REACHABLE
            .into_iter()
            .find(|kind| kind.name() == trimmed)
        {
            return Ok(kind);
        }
        // 内核另有两个更高保真的模型。把它们和拼错的名字混成"未知模型"会让人以为内核
        // 没这能力，而真实情况是 Bar 输入撑不起它们——这句区别必须说清楚。
        let missing_tier = match trimmed {
            "probabilistic" => Some("L1 一档盘口"),
            "volume_sensitive" => Some("L2/L3 深度盘口"),
            _ => None,
        };
        Err(match missing_tier {
            Some(tier) => format!(
                "撮合模型 {trimmed} 需要{tier}，而 Bar 回测的输入只有 OHLCV：装上它等于声称还原了\
                 并不存在的盘口。深度链（backtest book）走 Tick/OrderBook 内核、不经过 FillModel，\
                 所以它暂时没有任何回测入口（V11 §15.4 第 1 条）。strategy.fill_model 只接受 {}。",
                Self::reachable()
            ),
            None => format!(
                "运行时配置 strategy.fill_model 未知: {configured}（只接受 {}）",
                Self::reachable()
            ),
        })
    }
}

/// 已经装配好的 Bar 撮合模型，连同"它是从哪来的"。
pub(crate) struct BarFillModelBinding {
    pub(crate) fill: Box<dyn FillModel>,
    /// 打印行里的模型名；内核自己的 `descriptor()` 带着参数与假设，那是另一份事实，
    /// 两者互相核对（换模型没换描述子 = 装配漏了，描述子没名字 = 产物在撒谎）。
    pub(crate) name: &'static str,
    /// 来源标注，取值见 [`fill_model_source`]。
    pub(crate) source: &'static str,
}

/// Bar 链撮合口径的唯一解析点：`strategy.fill_model` 只在这里被翻译成模型。
///
/// `instrument_spec` 是 `one_tick_slippage` 的唯一一档来源：一档必须是该产品真实的报价
/// 粒度，而 `price_tick` 只有 market spec 声明得了。缺 spec 时拒绝而不是兜一个默认值——
/// 猜出来的滑点会一路进到成交额与终值，产物里却没有任何地方写着它来自猜测。
pub(crate) fn bar_fill_model(
    configured: Option<&str>,
    instrument_spec: Option<&TradingInstrumentSpec>,
) -> Result<BarFillModelBinding, String> {
    let kind = match configured {
        Some(configured) => BarFillModel::parse(configured)?,
        None => BarFillModel::NextBarOpen,
    };
    let source = fill_model_source(configured);
    let fill: Box<dyn FillModel> = match kind {
        BarFillModel::NextBarOpen => Box::new(NextBarOpenFillModel),
        BarFillModel::BestPrice => Box::new(BestPriceFillModel),
        BarFillModel::OneTickSlippage => Box::new(OneTickSlippageFillModel {
            tick: declared_price_tick(kind.name(), instrument_spec)?,
        }),
    };
    Ok(BarFillModelBinding {
        fill,
        name: kind.name(),
        source,
    })
}

/// 撮合口径的来源标注：`builtin-default` 是"配置没提这一项"，`runtime-config` 是"配置里
/// 声明了"。两者可能指向同一个模型，但产物必须分得清——与 [`ExecutionCostBinding::source`]
/// 同一条理由：分不清来源，就等于把默认值冒充成使用者选过的口径。
pub(crate) fn fill_model_source(configured: Option<&str>) -> &'static str {
    if configured.is_some() {
        "runtime-config"
    } else {
        "builtin-default"
    }
}

fn declared_price_tick(
    name: &str,
    instrument_spec: Option<&TradingInstrumentSpec>,
) -> Result<i128, String> {
    match instrument_spec {
        Some(spec) if spec.price_tick > 0 => Ok(spec.price_tick),
        Some(spec) => Err(format!(
            "撮合模型 {name} 的一档滑点取自 market spec，但它的 price_tick={} 不是正数",
            spec.price_tick
        )),
        None => Err(format!(
            "撮合模型 {name} 的一档滑点只认 market spec 的 price_tick，本次回测没有加载 market \
             spec，拒绝为标的猜一档：请给本入口传 market spec 文件，或改用 {} / {}",
            BarFillModel::NextBarOpen.name(),
            BarFillModel::BestPrice.name()
        )),
    }
}

/// 命令行回测入口（`backtest builtin` / `multi-builtin`）读 `strategy.fill_model` 的落点。
///
/// 与 [`super::backtest_risk_binding`] 同构：这些入口不以运行时配置为主输入，但一旦给了
/// `--config`，配置里声明的口径就必须生效，否则"四条 Bar 链得到四种撮合"会重演 V10 §4.2。
pub(crate) fn configured_fill_model(config_path: Option<&Path>) -> Result<Option<String>, String> {
    Ok(match config_path {
        Some(path) => read_runtime_config(path)?.strategy.fill_model,
        None => None,
    })
}

/// `config validate` 用的名字体检：与装配共用同一张表，所以"校验通过、装配拒绝"分裂不了。
///
/// 档位前置（`one_tick_slippage` 要有 price_tick）不在这里判——market spec 是命令行的位置
/// 参数，`config validate` 看不到它，硬判会把"传了 spec 就能跑"的正常配置误报成错误。
pub(crate) fn fill_model_problem(configured: &str) -> Option<String> {
    BarFillModel::parse(configured).err()
}

/// 把上面那份体检折算成 `config validate` 的一条失败项（`{label}.fill_model 原因`）。
pub(crate) fn fill_model_failure(configured: Option<&str>, label: &str) -> Option<String> {
    configured
        .and_then(fill_model_problem)
        .map(|problem| format!("{label}.fill_model {problem}"))
}
