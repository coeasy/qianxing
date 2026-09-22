//! 运行时接线：读取 runtime 配置、装配 EventLog 管道、构造策略风控门与 worker 指标文件。
//!
//! 由 `main.rs` 的 crate 根职责簇拆出（Phase 4p），条目经根部的
//! `pub(crate) use runtime_wiring::*;` 再导出，行为与拆分前逐字相同。

use super::*;

pub(crate) fn read_runtime_config(path: &Path) -> Result<RuntimeConfig, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取运行时配置失败 {}: {error}", path.display()))?;
    RuntimeConfig::from_json(&payload)
}

#[derive(Clone, Debug)]
pub(crate) struct PipelineStorage {
    pub(crate) root: PathBuf,
    segment_events: Option<usize>,
    postgres_dsn: Option<String>,
    sqlite_db: Option<PathBuf>,
    #[cfg(feature = "postgres")]
    postgres_pool_size: usize,
}

impl PipelineStorage {
    pub(crate) fn from_config(config: &RuntimeConfig) -> Result<Self, String> {
        Ok(Self {
            root: Path::new(&config.storage.data_dir).to_path_buf(),
            segment_events: config.storage.event_log_segment_events,
            postgres_dsn: configured_postgres_dsn(config)?,
            sqlite_db: configured_sqlite_event_log(config)?,
            #[cfg(feature = "postgres")]
            postgres_pool_size: config.storage.postgres_pool_size,
        })
    }

    pub(crate) fn open(
        &self,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> Result<LiveEventPipeline, String> {
        if let Some(dsn) = self.postgres_dsn.as_deref() {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = dsn;
                return Err(
                    "当前 qx-cli 未启用 postgres feature，无法打开 PostgreSQL EventLog".into(),
                );
            }
            #[cfg(feature = "postgres")]
            {
                return LiveEventPipeline::open_postgres_with_pool_size(
                    dsn,
                    self.postgres_pool_size,
                    log_name,
                    currency,
                )
                .map_err(|error| format!("打开 PostgreSQL EventLog 失败: {error:?}"));
            }
        }
        if let Some(db) = self.sqlite_db.as_deref() {
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = db;
                return Err("当前 qx-cli 未启用 sqlite feature，无法打开 SQLite EventLog".into());
            }
            #[cfg(feature = "sqlite")]
            {
                return LiveEventPipeline::open_sqlite(db, log_name, currency)
                    .map_err(|error| format!("打开 SQLite EventLog 失败: {error:?}"));
            }
        }
        LiveEventPipeline::open_configured(
            self.root.clone(),
            log_name,
            currency,
            self.segment_events,
        )
        .map_err(|error| format!("打开运行时 EventLog 失败: {error:?}"))
    }
}

pub(crate) fn configured_sqlite_event_log(
    config: &RuntimeConfig,
) -> Result<Option<PathBuf>, String> {
    if config.storage.backend != StorageBackend::Sqlite {
        return Ok(None);
    }
    let Some(path) = config.storage.sqlite_path.as_deref() else {
        return Err("SQLite backend 缺少 sqlite_path".to_string());
    };
    Ok(Some(PathBuf::from(path)))
}

/// 未配置任何规则时三条链路共用的保守阈值：单笔最大 1000 张。
pub(crate) const CONSERVATIVE_MAX_QTY_RAW: i128 = 1_000 * SCALE;
/// 未配置 `strategy.risk_rules` 时生效的规则集版本标识。
pub(crate) const CONSERVATIVE_DEFAULT_RULE_SET_VERSION: &str = "conservative-default-v1";

/// 回测 / Paper / Live 唯一的风禁构造入口：同一份运行时配置必须得到同一套规则。
///
/// - 配置里一条规则都没有时退回保守默认（历史 worker 行为）。
/// - `allow_short=false`（现货、现金保证金）时追加 `NoShortRule`。
///
/// 版本号始终描述实际生效的规则链：追加保守默认或隐式禁空时分别带上
/// `+conservative-max-qty`、`+implicit-no-short` 后缀，使回测摘要与执行审计能区分
/// “配置里就禁空”和“由产品推导禁空”。
pub(crate) fn strategy_risk_gate(
    rules: Option<&qx_runtime::RiskRulesConfig>,
    allow_short: bool,
) -> RiskGate {
    let short_banned_by_config = rules.is_some_and(|rules| rules.no_short);
    let implicit_no_short = !allow_short && !short_banned_by_config;
    let conservative_max_qty = rules.is_none_or(|rules| {
        rules.max_qty_raw.is_none() && rules.max_notional_raw.is_none() && !rules.no_short
    });
    let mut version = match rules {
        None => CONSERVATIVE_DEFAULT_RULE_SET_VERSION.to_string(),
        Some(rules) if conservative_max_qty => {
            format!("{}+conservative-max-qty", rules.version)
        }
        Some(rules) => rules.version.clone(),
    };
    if implicit_no_short {
        version.push_str("+implicit-no-short");
    }
    let mut rule_set = RuleSet::with_version(version);
    if conservative_max_qty {
        rule_set.add(Box::new(MaxQtyRule {
            max_qty: CONSERVATIVE_MAX_QTY_RAW,
        }));
    }
    if let Some(rules) = rules {
        if let Some(max_qty) = rules.max_qty_raw {
            rule_set.add(Box::new(MaxQtyRule { max_qty }));
        }
        if let Some(max_notional) = rules.max_notional_raw {
            rule_set.add(Box::new(MaxNotionalRule { max_notional }));
        }
        if rules.no_short {
            rule_set.add(Box::new(NoShortRule));
        }
    }
    if implicit_no_short {
        rule_set.add(Box::new(NoShortRule));
    }
    RiskGate::from_rule_set(rule_set)
}

/// 执行成本口径的单点：Bar 回测装配、Paper 提交与补偿单恢复共用同一份 [`ExecutionCostRules`]。
///
/// 与 [`strategy_risk_gate`] 同构——那是风控的唯一构造入口，这是成本的唯一构造入口。
/// Q0a 让两条链取到**同一个**模型，Q0c 才让它**来自配置**：`strategy.cost_rules_path`
/// 指向的文件只在这里被读取。缺文件、坏 JSON、bp 越界一律失败并带上路径；静默退回
/// 内置 2/5bp 就是 §4 P0 第 2 项里 `--config` 被吞掉的那类形状（"宣称能配、实际不生效"）。
#[derive(Debug)]
pub(crate) struct ExecutionCostBinding {
    pub(crate) rules: ExecutionCostRules,
    /// 实际读取的成本规则文件；`None` 表示本次用的是内核默认费率。
    pub(crate) loaded_from: Option<PathBuf>,
    /// 给了运行时配置（无论其中有没有 `cost_rules_path`），与"根本没给配置"要能区分。
    pub(crate) from_runtime_config: bool,
}

impl ExecutionCostBinding {
    pub(crate) fn fee_model(&self) -> Box<dyn FeeModel + Send> {
        self.rules.fee_model()
    }

    pub(crate) fn latency_model(&self) -> Box<dyn LatencyModel + Send> {
        self.rules.latency_model()
    }

    /// 产物与打印行里的成本来源。费率数字本身已经进了 `model_descriptors`，
    /// 这里只回答"这组数字从哪来"，避免同一个事实出现第二份记录。
    pub(crate) fn source(&self) -> String {
        match &self.loaded_from {
            Some(path) => format!("cost-rules-file:{}", path.display()),
            None if self.from_runtime_config => "runtime-config-default".to_string(),
            None => "builtin-default".to_string(),
        }
    }
}

pub(crate) fn execution_cost_binding_from_config(
    config: &RuntimeConfig,
    runtime_config_path: Option<&Path>,
) -> Result<ExecutionCostBinding, String> {
    let Some(configured) = config.strategy.cost_rules_path.as_deref() else {
        return Ok(ExecutionCostBinding {
            from_runtime_config: runtime_config_path.is_some(),
            ..default_execution_cost_binding()
        });
    };
    // 这里就是该字段的唯一解析点：策略链的绝对化预处理不含 cost_rules_path，
    // 相对值一律按运行时配置文件所在目录落地。
    let rules_path = match runtime_config_path {
        Some(base) => resolve_runtime_relative_path(base, configured),
        None => PathBuf::from(configured),
    };
    let rules = ExecutionCostRules::load(&rules_path)
        .map_err(|error| format!("读取执行成本规则失败 {}: {error}", rules_path.display()))?;
    Ok(ExecutionCostBinding {
        rules,
        loaded_from: Some(rules_path),
        from_runtime_config: true,
    })
}

/// `config validate` 用的成本规则体检：把"文件缺失"和"内容非法"翻译成一行结论。
///
/// 校验与装配读的是同一个文件、走的是同一份 `ExecutionCostRules::load`，所以
/// "校验说没问题、装配却跑不动"这类分裂不可能出现——成本规则文件的读者只有这一处。
pub(crate) fn cost_rules_problem(runtime_config_path: &Path, configured: &str) -> Option<String> {
    let rules_path = resolve_runtime_relative_path(runtime_config_path, configured);
    if !rules_path.is_file() {
        return Some(format!(
            "文件不存在: {} (configured={configured})",
            rules_path.display()
        ));
    }
    ExecutionCostRules::load(&rules_path)
        .err()
        .map(|error| format!("内容非法: {error}"))
}

/// 完全没有运行时配置时的内核默认口径（CLI 自检与单测用）。
pub(crate) fn default_execution_cost_binding() -> ExecutionCostBinding {
    ExecutionCostBinding {
        rules: ExecutionCostRules::default(),
        loaded_from: None,
        from_runtime_config: false,
    }
}

/// 手上只有运行时配置路径、没有内存里配置对象的入口（内置策略、深度档）用的薄壳。
pub(crate) fn execution_cost_binding(
    runtime_config_path: Option<&Path>,
) -> Result<ExecutionCostBinding, String> {
    match runtime_config_path {
        Some(path) => {
            let config = read_runtime_config(path)?;
            execution_cost_binding_from_config(&config, Some(path))
        }
        None => Ok(default_execution_cost_binding()),
    }
}

/// 与 Strategy worker 完全一致的做空许可判定：现金保证金一律禁止，
/// 其余场景由 `strategy.allow_short` 决定，缺省按产品是否为衍生品。
pub(crate) fn strategy_allows_short(config: &RuntimeConfig, margin_mode: MarginMode) -> bool {
    if margin_mode == MarginMode::Cash {
        return false;
    }
    let product = config.strategy.product.unwrap_or(TradingProduct::Spot);
    config
        .strategy
        .allow_short
        .unwrap_or(product.is_derivative())
}

/// 回测入口使用的保证金模式，与 Strategy worker 的推导保持一致。
pub(crate) fn config_margin_mode(config: &RuntimeConfig) -> MarginMode {
    let product = config.strategy.product.unwrap_or(TradingProduct::Spot);
    config
        .strategy
        .margin_mode
        .unwrap_or(if product == TradingProduct::Spot {
            MarginMode::Cash
        } else {
            MarginMode::Cross
        })
}

pub(crate) fn open_runtime_pipeline(
    config: &RuntimeConfig,
    root: &Path,
    log_name: impl Into<String>,
    currency: impl Into<String>,
) -> Result<LiveEventPipeline, String> {
    let storage = PipelineStorage::from_config(config)?;
    let mut storage = storage;
    storage.root = root.to_path_buf();
    storage.open(log_name, currency)
}

/// 读模型打开账户级 EventLog 的统一入口：记账币种跟着日志身份走，打开之后
/// 再按 [`LiveEventPipeline::settlement_currency`] 回读，调用处不再各写一本账。
pub(crate) fn open_account_pipeline(
    config: &RuntimeConfig,
    root: &Path,
    log_name: &str,
) -> Result<LiveEventPipeline, String> {
    open_runtime_pipeline(
        config,
        root,
        log_name.to_string(),
        settlement_currency_for_log(config, log_name)?,
    )
}

pub(crate) fn event_log_exists(
    config: &RuntimeConfig,
    root: &Path,
    log_name: &str,
) -> Result<bool, String> {
    if config.storage.backend == StorageBackend::Postgres {
        #[cfg(not(feature = "postgres"))]
        return Err("当前 qx-cli 未启用 postgres feature；无法查询 PostgreSQL EventLog".into());
        #[cfg(feature = "postgres")]
        {
            let dsn = postgres_dsn(config)?;
            let store = PostgresEventLogStore::connect_with_pool_size(
                &dsn,
                config.storage.postgres_pool_size,
            )
            .map_err(|error| format!("连接 PostgreSQL EventLog 失败: {error:?}"))?;
            return store
                .read_if_exists(log_name)
                .map(|value| value.is_some())
                .map_err(|error| format!("查询 PostgreSQL EventLog 失败: {error:?}"));
        }
    }
    if config.storage.backend == StorageBackend::Sqlite {
        #[cfg(not(feature = "sqlite"))]
        return Err("当前 qx-cli 未启用 sqlite feature；无法查询 SQLite EventLog".into());
        #[cfg(feature = "sqlite")]
        {
            let path = config
                .storage
                .sqlite_path
                .as_deref()
                .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
            let store = qx_storage::SqliteEventLogStore::new(path)
                .map_err(|error| format!("连接 SQLite EventLog 失败: {error:?}"))?;
            return store
                .read_if_exists(log_name)
                .map(|value| value.is_some())
                .map_err(|error| format!("查询 SQLite EventLog 失败: {error:?}"));
        }
    }
    Ok(root.join(format!("{log_name}.json")).exists()
        || (config.storage.event_log_segment_events.is_some()
            && root.join(format!("{log_name}.manifest.json")).exists()))
}

pub(crate) fn runtime_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn worker_metrics_dir(config: &RuntimeConfig) -> PathBuf {
    Path::new(&config.storage.data_dir).join("worker-metrics")
}

#[cfg(feature = "nats")]
pub(crate) fn worker_metrics_path(config: &RuntimeConfig, worker_id: &str) -> PathBuf {
    worker_metrics_dir(config).join(format!("{worker_id}.prom"))
}

#[cfg(feature = "nats")]
pub(crate) fn write_worker_metrics(path: &Path, body: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("worker metrics 路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建 worker metrics 目录失败: {error}"))?;
    let temporary = path.with_extension(format!("prom.tmp.{}", std::process::id()));
    std::fs::write(&temporary, body)
        .map_err(|error| format!("写入 worker metrics 临时文件失败: {error}"))?;
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("提交 worker metrics 文件失败: {error}")
    })
}

pub(crate) fn read_worker_metrics(directory: &Path, now_ms: u64, stale_after_ms: u64) -> String {
    let mut paths = match std::fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("prom"))
            .collect::<Vec<_>>(),
        Err(_) => return String::new(),
    };
    paths.sort();
    let mut output = String::new();
    for path in paths {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let content = if worker_metrics_stale(&content, now_ms, stale_after_ms) {
                force_worker_metrics_down(&content)
            } else {
                content
            };
            output.push_str(&content);
            if !output.ends_with('\n') {
                output.push('\n');
            }
        }
    }
    output
}

pub(crate) fn worker_metrics_stale(content: &str, now_ms: u64, stale_after_ms: u64) -> bool {
    let heartbeat_ms = content.lines().find_map(|line| {
        if !line.starts_with("qx_worker_heartbeat_timestamp_seconds") {
            return None;
        }
        line.split_whitespace()
            .last()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| (value * 1_000.0) as u64)
    });
    heartbeat_ms.is_none_or(|heartbeat| now_ms.saturating_sub(heartbeat) > stale_after_ms)
}

pub(crate) fn force_worker_metrics_down(content: &str) -> String {
    let mut output = String::new();
    let mut worker_label = None;
    for line in content.lines() {
        if line.starts_with("qx_worker_up{") {
            worker_label = line
                .split("worker=\"")
                .nth(1)
                .and_then(|value| value.split('\"').next())
                .map(str::to_string);
            let mut fields = line.split_whitespace().collect::<Vec<_>>();
            if let Some(value) = fields.last_mut() {
                *value = "0";
            }
            output.push_str(&fields.join(" "));
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    if let Some(worker) = worker_label {
        output.push_str(&format!(
            "qx_worker_metrics_stale{{worker=\"{worker}\"}} 1\n"
        ));
    }
    output
}

pub(crate) fn worker_metrics_unhealthy(content: &str, now_ms: u64, stale_after_ms: u64) -> bool {
    if worker_metrics_stale(content, now_ms, stale_after_ms) {
        return true;
    }
    content.lines().any(|line| {
        line.starts_with("qx_worker_up{")
            && line
                .split_whitespace()
                .last()
                .is_none_or(|value| value != "1")
    })
}

/// A 股交易制度缺的那个状态，写进拒绝文案而不是只说"不支持"（V11 Q65）。
///
/// 费率倒是接得上（`execution_cost_binding_from_config` 就是挂钩点），但只接费率会让产物显示
/// "A 股佣金"而订单照样 T+0、照样可以 150 股买入 —— 比整段不认更容易骗人。
const ASHARE_SUBMIT_GAP: &str =
    "T+1 需要\"今日买入\"的结算状态、整手与涨跌停要按板块和昨收逐单判定，\
     而提交前的账户闸门只做规格、杠杆、名义额与可用保证金预检";

/// 会向 Venue 提交**新订单**的角色。`SpreadRecovery` 算在内：补腿本身就是一笔新订单（V10 Q57）。
fn worker_submits_orders(worker: &WorkerConfig) -> bool {
    matches!(
        worker.role,
        WorkerRole::Execution | WorkerRole::SpreadRecovery
    )
}

/// Paper/Live 的提交入口在动手前先拒 A 股段（V11 Q65，与 Q61 的 `book` / `multi-builtin` 同一口径）。
///
/// 判据只有一个：**这条路径会不会把一笔新订单送进 Venue**。行情、用户流、对账、策略与 API worker
/// 不在此列 —— 它们本来就不承诺交易制度，把它们一起拒会让"研究用配置"连启动都做不到。
/// `worker` 缺位表示一次性提交入口（`paper submit-order` / `binance submit-order`），按提交对待。
pub(crate) fn reject_ashare_rules_on_submit_path(
    config_path: &Path,
    worker: Option<&WorkerConfig>,
    entry: &str,
) -> Result<(), String> {
    if worker.is_some_and(worker_submits_orders) || worker.is_none() {
        return reject_ashare_rules_config(Some(config_path), entry, ASHARE_SUBMIT_GAP);
    }
    Ok(())
}
