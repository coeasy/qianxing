//! 策略与调度侧配置类型：目标快照、调度器、风控规则与策略实例。

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyTransport {
    #[default]
    Jsonl,
    FramedJson,
    SharedMemoryJson,
    SharedMemoryColumnar,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyTargetSnapshot {
    pub schema_version: u32,
    pub strategy_version: String,
    pub data_fingerprint: String,
    pub as_of: u64,
    /// JSON 使用 InstrumentId 字符串作为 key，避免结构体 key 的非稳定编码。
    pub targets: BTreeMap<String, i128>,
}

impl StrategyTargetSnapshot {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(format!(
                "StrategyTargetSnapshot schema_version 必须为 {}",
                Self::SCHEMA_VERSION
            ));
        }
        if self.strategy_version.trim().is_empty() || self.data_fingerprint.trim().is_empty() {
            return Err("StrategyTargetSnapshot 缺少 strategy_version 或 data_fingerprint".into());
        }
        if self.targets.is_empty() {
            return Err("StrategyTargetSnapshot targets 不能为空".into());
        }
        for instrument in self.targets.keys() {
            InstrumentId::parse(instrument)
                .ok_or_else(|| format!("StrategyTargetSnapshot instrument 非法: {instrument}"))?;
        }
        Ok(())
    }

    /// 校验该产物是否属于当前运行中的策略版本，并且不是未来时间的结果。
    ///
    /// `as_of == 0` 表示离线构建器没有提供时间锚点，属于非法运行时输入；
    /// 这样可以避免把另一个策略版本或尚未到达的研究结果静默带入交易链路。
    pub fn validate_for(&self, strategy_version: &str, now: u64) -> Result<(), String> {
        self.validate()?;
        if strategy_version.trim().is_empty() || self.strategy_version != strategy_version {
            return Err(format!(
                "StrategyTargetSnapshot strategy_version 不匹配: snapshot={} runtime={}",
                self.strategy_version, strategy_version
            ));
        }
        if self.as_of == 0 || self.as_of > now {
            return Err(format!(
                "StrategyTargetSnapshot as_of 无效或晚于运行时: as_of={} now={}",
                self.as_of, now
            ));
        }
        Ok(())
    }

    pub fn target_for(&self, instrument: &InstrumentId) -> Option<i128> {
        self.targets.get(&instrument.to_string()).copied()
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerRuntimeConfig {
    #[serde(default = "default_scheduler_state_path")]
    pub state_path: String,
    #[serde(default = "default_scheduler_jobs_path")]
    pub jobs_path: String,
    #[serde(default = "default_scheduler_job_queue_path")]
    pub job_queue_path: String,
    #[serde(default = "default_scheduler_tick_interval_ms")]
    pub tick_interval_ms: u64,
}

impl Default for SchedulerRuntimeConfig {
    fn default() -> Self {
        Self {
            state_path: default_scheduler_state_path(),
            jobs_path: default_scheduler_jobs_path(),
            job_queue_path: default_scheduler_job_queue_path(),
            tick_interval_ms: default_scheduler_tick_interval_ms(),
        }
    }
}

/// 回测/Paper/Live 共用的可序列化账户级风控规则集配置。判定实现唯一存在于
/// `qx-risk::RuleSet`；本结构只负责把 JSON 配置映射为规则参数。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskRulesConfig {
    /// 规则集版本，随判定结果与运行摘要输出，用于结果归因。
    #[serde(default = "default_risk_rules_version")]
    pub version: String,
    /// 单笔最大数量（128-bit 定点 raw）。缺省=不启用该规则。
    #[serde(default)]
    pub max_qty_raw: Option<i128>,
    /// 投影名义额上限（128-bit 定点 raw）。缺省=不启用该规则。
    #[serde(default)]
    pub max_notional_raw: Option<i128>,
    /// 是否禁止建立空头。
    #[serde(default)]
    pub no_short: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyRuntimeConfig {
    /// 多策略运行时中用于绑定 Strategy worker 的稳定 ID；旧版单策略配置可省略。
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default = "default_strategy_version")]
    pub version: String,
    #[serde(default = "default_strategy_max_orders")]
    pub max_orders: u64,
    #[serde(default)]
    pub account_id: Option<String>,
    /// 回测链的账户本金（1e-9 定点整数）。收益率的分母、风控门看到的可用现金与可用保证金、
    /// 以及"买入成本（含预估手续费）超过账户可用现金"这类拒单，全部压在这一个数上：不声明时
    /// 三条单腿回测链按 `qx-cli` 的常数 100,000 记账，并在 stdout 与摘要里写
    /// `account_base_source=builtin-default`（V11 Q72）。Paper 侧的同名事实是
    /// `worker.paper_initial_cash_raw`，回测读不到它，所以这条字段是回测唯一的声明入口。
    /// `skip_serializing_if` 的口径与下面的 `cost_rules_path` 相同：省略时配置序列化字节必须
    /// 逐字节不变，否则已 bless 的 `config_fingerprint` 会因一个没有改变行为的字段集体失真。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_cash_raw: Option<i128>,
    /// 回测与 Strategy worker 共用的账户级风控规则集；未配置时使用默认规则集
    /// （仅 reduce-only 不变式），运行摘要记录其默认版本号。
    #[serde(default)]
    pub risk_rules: Option<RiskRulesConfig>,
    /// 执行成本规则文件（maker/taker 费率与撮合延迟），路径相对本配置文件解析。
    /// 省略时回测与 Paper 使用 `qx-core` 的默认费率，产物里的 `execution_costs.source`
    /// 会区分这两种情况。`skip_serializing_if` 是刻意的：省略该字段时运行时配置的
    /// 序列化字节必须与引入它之前完全一致，否则所有已 bless 的 `config_fingerprint`
    /// 与 RunManifest 哈希会在一夜之间全部失真，而行为并没有变。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_rules_path: Option<String>,
    /// Bar 链的撮合模型：`next_bar_open`（内核默认）/ `best_price` / `one_tick_slippage`。
    /// 三者都是"只有 OHLCV 也能诚实撮合"的模型，换它们会换成交价，因而换成交额、费用与
    /// 结果哈希；`one_tick_slippage` 的一档只取自 market spec 的 `price_tick`——没有 spec 就
    /// 拒绝，因为替某个产品猜一档滑点等于把猜测写进结果。
    ///
    /// 内核另有 `probabilistic`（要 L1 一档）与 `volume_sensitive`（要 L2/L3 深度）两个更高保真
    /// 的模型，本字段**不接受**它们：Bar 链的输入只有 OHLCV，装进去就是"声称还原了并不存在的
    /// 盘口"。要让它们可达得先让 Bar 链吃到深度帧（V11 §15.4 第 1 条）。
    ///
    /// `skip_serializing_if` 的口径与上面的 `cost_rules_path` 完全相同：省略时运行时配置的
    /// 序列化字节必须逐字节不变，否则所有已 bless 的 `config_fingerprint` 与 RunManifest
    /// 哈希会因为一个没有改变行为的字段而集体失真。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_model: Option<String>,
    #[serde(default)]
    pub venue_id: Option<String>,
    #[serde(default)]
    pub instrument: Option<String>,
    #[serde(default)]
    pub target_qty: i128,
    #[serde(default)]
    pub target_snapshot_path: Option<String>,
    /// 研究层输出的 CandidateBinding + FeatureArtifact + FactorReport 快照。
    /// 配置此字段后，Strategy 不允许退化为只读取裸目标仓位文件。
    #[serde(default)]
    pub research_snapshot_path: Option<String>,
    /// 要求策略必须由研究快照驱动；生产环境中的已绑定策略必须显式开启。
    #[serde(default)]
    pub research_snapshot_required: bool,
    /// 发布时锁定的研究数据指纹；运行时会拒绝快照与该指纹不一致。
    #[serde(default)]
    pub research_data_fingerprint: Option<String>,
    /// 回测/研究使用的 DatasetBundleManifest 文件。配置后，回测启动前
    /// 必须证明输入 BarFrame 的 fingerprint 与 bundle 的 bars 组件一致。
    #[serde(default)]
    pub dataset_bundle_path: Option<String>,
    /// DatasetBundle 中除 bars 外的组件文件绑定。键必须与 Bundle 的组件 kind
    /// 一致，值是相对于 runtime 配置文件的 JSON/Arrow 清单路径。公司行为和
    /// 交易日历仍兼容下方的专用字段；显式映射用于停牌、涨跌停、因子、股票池
    /// 以及未来新增组件，避免每增加一种数据就修改运行时核心。
    #[serde(default)]
    pub dataset_component_paths: BTreeMap<String, String>,
    /// 策略订单的统一产品语义；省略时兼容现货 Cash/1x/NoShort。
    #[serde(default)]
    pub product: Option<TradingProduct>,
    #[serde(default)]
    pub margin_mode: Option<MarginMode>,
    #[serde(default)]
    pub position_mode: Option<PositionMode>,
    #[serde(default)]
    pub leverage: Option<u32>,
    /// None 时：衍生品默认允许双向，现货/杠杆默认禁止空头，必须显式开启。
    #[serde(default)]
    pub allow_short: Option<bool>,
    /// 开启后由 CCXT MarketData worker 持续维护 BarFrame 快照，并在新闭合 Bar
    /// 到达时向 Strategy JobQueue 投递一次幂等运行任务。
    #[serde(default)]
    pub live_enabled: bool,
    /// 实时 OHLCV 周期，例如 1m、5m、1h。
    #[serde(default = "default_strategy_live_timeframe")]
    pub live_timeframe: String,
    /// 实时策略保留的历史 Bar 数量；必须覆盖策略预热窗口。
    #[serde(default = "default_strategy_live_history_limit")]
    pub live_history_limit: usize,
    /// 默认只将已闭合 K 线送入策略，避免同一根未闭合 K 线反复触发下单。
    #[serde(default = "default_strategy_live_closed_only")]
    pub live_closed_only: bool,
    /// 实时策略允许的最新 Bar 最大滞后时间；省略时按 3 个周期计算。
    /// 超过该窗口只保持运行，不再生成新的策略订单。
    #[serde(default)]
    pub live_max_staleness_ms: Option<u64>,
    /// 内置 Rust Bar 策略名称。配置后 Strategy Worker/Backtest 会使用同一套
    /// 固定点策略实现，并继续经过统一 OrderIntent、RiskGate 和 OMS。
    #[serde(default)]
    pub builtin_strategy: Option<String>,
    /// 内置策略的目标仓位，单位是**定点裸值**（`Quantity::from_raw`，1 个计价单位 = 1e9）。
    /// 命令行入口的位置参数 `[QUANTITY]` 走的是另一个口径（`Quantity::from_i64`，按整数
    /// 单位计），两者相差 1e9 倍。这里写 `1` 表示 1e-9 个单位：成交额小到让手续费按整数
    /// 截断成零，回测看起来"成交了但一分钱费用都没付"。
    #[serde(default)]
    pub builtin_quantity: Option<i64>,
    #[serde(default)]
    pub builtin_fast_window: Option<usize>,
    #[serde(default)]
    pub builtin_slow_window: Option<usize>,
    #[serde(default)]
    pub builtin_period: Option<usize>,
    #[serde(default)]
    pub builtin_threshold_bps: Option<i128>,
    /// 双腿套利的对冲腿 InstrumentId；仅 pairs_arbitrage/basis_arbitrage 使用。
    #[serde(default)]
    pub builtin_reference_instrument: Option<String>,
    /// 双腿套利对冲腿的 BarFrame 快照路径。
    #[serde(default)]
    pub builtin_reference_bars_snapshot_path: Option<String>,
    /// 双腿套利对冲腿的执行策略；用于现货/永续混合时给每条腿独立设置
    /// Cash/1x 或 Cross/Isolated/杠杆，不把期货参数误发给现货交易所。
    #[serde(default)]
    pub builtin_reference_margin_mode: Option<MarginMode>,
    #[serde(default)]
    pub builtin_reference_position_mode: Option<PositionMode>,
    #[serde(default)]
    pub builtin_reference_leverage: Option<u32>,
    /// Strategy worker 可读取的冻结 BarFrame 快照。外部策略会收到 bars 输入；
    /// 内置策略运行时必须配置该字段，才能基于历史 K 线产生信号。
    #[serde(default)]
    pub bars_snapshot_path: Option<String>,
    /// A 股规则快照；启用后回测和纸面交易使用 T+1、整手、涨跌停、停牌和费用规则。
    #[serde(default)]
    pub ashare_rules_path: Option<String>,
    /// 可选 Python/A 股标准化公司行为 JSON；加载后会合并进 ashare_rules_path。
    /// 配股登记/认购/失效、增发/回购/转股必须携带显式账户事实；
    /// 登记日/除权日自动推导及发行人级生命周期事件仍 fail-closed。
    #[serde(default)]
    pub ashare_actions_path: Option<String>,
    /// 可选 Python/A 股交易日历 JSON；会展开交易日和交易时段并合并进规则快照。
    #[serde(default)]
    pub ashare_calendar_path: Option<String>,
    /// 可选 Python JSONL 策略模块；Strategy Worker 只通过稳定契约调用它。
    #[serde(default)]
    pub python_module: Option<String>,
    /// JSONL 保持兼容；framed_json 使用 QXSF，shared_memory_columnar 使用 QXCB 固定列。
    #[serde(default)]
    pub transport: StrategyTransport,
    /// SharedMemoryJson/SharedMemoryColumnar 的 SPSC ring 容量和固定槽位大小。
    #[serde(default = "default_strategy_shared_memory_capacity")]
    pub shared_memory_capacity: u32,
    #[serde(default = "default_strategy_shared_memory_slot_bytes")]
    pub shared_memory_slot_bytes: u32,
    /// Python 策略单次事件处理超时；超时会终止该策略 worker，避免未知状态继续下单。
    #[serde(default = "default_strategy_python_timeout_ms")]
    pub python_timeout_ms: u64,
    /// Rust/C++/其他语言策略可编译为独立进程，通过同一 JSONL 策略契约接入。
    #[serde(default)]
    pub external_executable: Option<String>,
    /// 独立策略文件或 Python `.py` 文件的发布摘要；配置后会在 worker
    /// 启动前读取文件并校验 SHA-256。C ABI 动态库使用 c_abi_sha256。
    #[serde(default)]
    pub strategy_artifact_sha256: Option<String>,
    #[serde(default)]
    pub external_args: Vec<String>,
    #[serde(default)]
    pub external_env: BTreeMap<String, String>,
    /// 受信任 C/C++ 原生策略动态库；必须同时配置 SHA-256 白名单。
    #[serde(default)]
    pub c_abi_library: Option<String>,
    #[serde(default)]
    pub c_abi_sha256: Option<String>,
    #[serde(default = "default_strategy_c_abi_max_library_bytes")]
    pub c_abi_max_library_bytes: u64,
    #[serde(default)]
    pub c_abi_ed25519_public_key: Option<String>,
    #[serde(default)]
    pub c_abi_ed25519_signature: Option<String>,
}

impl Default for StrategyRuntimeConfig {
    fn default() -> Self {
        Self {
            id: None,
            version: default_strategy_version(),
            max_orders: default_strategy_max_orders(),
            risk_rules: None,
            cost_rules_path: None,
            fill_model: None,
            account_id: None,
            initial_cash_raw: None,
            venue_id: None,
            instrument: None,
            target_qty: 0,
            target_snapshot_path: None,
            research_snapshot_path: None,
            research_snapshot_required: false,
            research_data_fingerprint: None,
            dataset_bundle_path: None,
            dataset_component_paths: BTreeMap::new(),
            product: None,
            margin_mode: None,
            position_mode: None,
            leverage: None,
            allow_short: None,
            live_enabled: false,
            live_timeframe: default_strategy_live_timeframe(),
            live_history_limit: default_strategy_live_history_limit(),
            live_closed_only: default_strategy_live_closed_only(),
            live_max_staleness_ms: None,
            builtin_strategy: None,
            builtin_quantity: None,
            builtin_fast_window: None,
            builtin_slow_window: None,
            builtin_period: None,
            builtin_threshold_bps: None,
            builtin_reference_instrument: None,
            builtin_reference_bars_snapshot_path: None,
            builtin_reference_margin_mode: None,
            builtin_reference_position_mode: None,
            builtin_reference_leverage: None,
            bars_snapshot_path: None,
            ashare_rules_path: None,
            ashare_actions_path: None,
            ashare_calendar_path: None,
            python_module: None,
            transport: StrategyTransport::Jsonl,
            shared_memory_capacity: default_strategy_shared_memory_capacity(),
            shared_memory_slot_bytes: default_strategy_shared_memory_slot_bytes(),
            python_timeout_ms: default_strategy_python_timeout_ms(),
            external_executable: None,
            strategy_artifact_sha256: None,
            external_args: Vec::new(),
            external_env: BTreeMap::new(),
            c_abi_library: None,
            c_abi_sha256: None,
            c_abi_max_library_bytes: default_strategy_c_abi_max_library_bytes(),
            c_abi_ed25519_public_key: None,
            c_abi_ed25519_signature: None,
        }
    }
}
