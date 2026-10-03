//! `deploy/` 顶层模板的读取覆盖（V13 R1-A6）。
//!
//! 审计起点是一份实测点名：52 份模板里 12 份在代码与 CI 里零引用（`logs/s28_template_refs_a6.txt`），
//! 于是"模板写坏了会怎样"没有人回答过。零引用不等于该删 —— 例如
//! `qianxing.scheduler.jobs.smoke.json` 被两份 runtime 模板挂着的 jobs_path 就是它。
//! 真正缺的是**读取**：CI 只用 `config validate` 读 1 份 runtime 模板，其余 51 份从未被任何
//! 进程按自己的格式解析过。
//!
//! 本文件把口径换成"每份模板都必须被它在生产里对应的那个读法解析一次"：登记表点名一份文件
//! 配一个读取器（`Reader`），读取器一律调用生产函数本身，而不是在这里另写一份字段校验。
//! 三条用例各管一件事：登记表与磁盘清单相等（新增模板不登记就红）、每份模板按登记的读取器
//! 读出预期结果、每个读取器对坏内容真的会拒（否则"读过了"只是把 JSON 换成 `Value` 走过场）。

use super::*;

/// 预期结果：`Ok` 要求读取器成功；`Refuses(关键字组)` 要求读取器失败，且报错里
/// **每一条关键字都出现** —— 也就是"只剩这几条已点名的缺口，多一条少一条都算红"。
///
/// 后者存在的理由不是反例夹具，而是 `qianxing.runtime.production.example.json`：它引用的是
/// 部署机上的 `/var/lib/qianxing/research/…`，仓库里没有、也不该有。把这一点钉成期望而不
/// 是"读到文件不存在就算过"，是为了让**新增**的引用缺口照样把用例打红。
#[derive(Clone, Copy, Debug)]
enum Expected {
    Ok,
    Refuses(&'static [&'static str]),
}
/// 一份模板在生产里的读法。变体上的额外参数都是**配对来源**：规格要标的、CCXT 配置要挂的
/// worker、A 股规则要连带的公司行为与日历 —— 全部取自仓库里已存在的另一份模板，
/// 不在这里抄第二份字面量。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reader {
    /// `read_runtime_config` + `config validate` 用的同一份引用体检。
    Runtime,
    /// `market_spec_from_value`。`Some(frame)` 把标的钉成那份 BarFrame 模板自己声明的标的
    /// （跨文件配对，规格与行情对不上就红）；`None` 只用于产品规格形状 —— 它的标的写在文件
    /// 自己的 `instrument` 那一格里，由 `TradingInstrumentSpec` 的反序列化读出。
    MarketSpec(Option<&'static str>),
    /// `validate_ccxt_worker_binding`；配 for (runtime 模板, 该 runtime 里实际挂载的 endpoint)。
    Ccxt(&'static str, &'static str),
    /// 单标的回测链的那一对读点：`read_bar_frame_for_backtest` + `barframe_dataset_identity`。
    BarFrame,
    /// `qx_xingban::DepthFrame::from_json`。
    DepthFrame,
    /// `DatasetBundleManifest` 反序列化 + `validate` + `fingerprint`。
    DatasetBundle,
    /// `ArrowDatasetManifest::from_json`。
    ArrowComponent,
    /// `ashare_backtest_binding`：规则 + 公司行为 + 交易日历一次装配（回测链的唯一读法）。
    AshareRules,
    /// `AshareRuleConfig::apply_corporate_actions_json_with_report`。
    AshareActions,
    /// `AshareRuleConfig::apply_calendar_json_with_report`。
    AshareCalendar,
    /// `ExecutionCostRules::load`。
    CostRules,
    /// `Vec<JobSpec>` + 派发形状闸门 + `Scheduler::register`。
    SchedulerJobs,
    /// `StrategyTargetSnapshot` 反序列化 + `validate`。
    StrategyTarget,
    /// `ControlCommand` 反序列化 + `order_from_submit_command`。
    SubmitOrder,
    /// `parse_fast_backtest_manifest` + 被引用文件存在性。
    FastBacktest,
}

/// 登记表：`deploy/` 顶层每一份 JSON 模板 → 生产读法 + 预期结果。
///
/// 新增模板必须在这里登记，否则 `every_deploy_template_is_registered_with_a_reader` 变红；
/// `tools/check_architecture.py` 的模板覆盖判据用同一份清单和磁盘核对，双向都不许漏。
const COVERAGE: &[(&str, Reader, Expected)] = &[
    // —— 运行时配置（18）：`config validate` 的完整引用体检 ——
    (
        "qianxing.runtime.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.production.example.json",
        Reader::Runtime,
        // 它引用的是部署机上的研究快照与数据集包（`/var/lib/qianxing/research/…`），仓库里
        // 没有、也不该有那两格产物。除此之外它必须过完整的引用体检。
        Expected::Refuses(&[
            "config validate 失败 2 项",
            "research_snapshot_path",
            "dataset_bundle_path",
        ]),
    ),
    (
        "qianxing.runtime.sqlite.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.postgres.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.consumer.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.messaging.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.ccxt.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.paper-strategy.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.binance-testnet.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.ashare.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.builtin-strategy.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.builtin-strategy.okx.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.multi-venue-arbitrage.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.strategy-backtest.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.strategy-columnar.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.strategy-framed.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.strategy-multi-backtest.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    (
        "qianxing.runtime.strategy-shared.example.json",
        Reader::Runtime,
        Expected::Ok,
    ),
    // —— 产品规格（5）：回测与 worker 共用的唯一读法 ——
    (
        "qianxing.binance.spot.spec.json",
        Reader::MarketSpec(None),
        Expected::Ok,
    ),
    (
        "qianxing.ccxt.okx.perpetual.spec.json",
        Reader::MarketSpec(None),
        Expected::Ok,
    ),
    (
        "qianxing.ashare.spot.spec.json",
        Reader::MarketSpec(Some("qianxing.ashare.bar-frame.example.json")),
        Expected::Ok,
    ),
    (
        "qianxing.market.binance.btc-swap.spec.json",
        Reader::MarketSpec(Some("qianxing.bar-frame.pairs-primary.example.json")),
        Expected::Ok,
    ),
    (
        "qianxing.market.binance.eth-swap.spec.json",
        Reader::MarketSpec(Some("qianxing.bar-frame.pairs-reference.example.json")),
        Expected::Ok,
    ),
    // —— CCXT endpoint 配置（4）：worker 绑定体检 ——
    (
        "qianxing.ccxt.exchange.example.json",
        Reader::Ccxt(
            "qianxing.runtime.ccxt.example.json",
            "qianxing.ccxt.exchange.example.json",
        ),
        Expected::Ok,
    ),
    (
        "qianxing.ccxt.binance.spot.example.json",
        Reader::Ccxt(
            "qianxing.runtime.multi-venue-arbitrage.example.json",
            "qianxing.ccxt.binance.spot.example.json",
        ),
        Expected::Ok,
    ),
    (
        "qianxing.ccxt.okx.swap.example.json",
        Reader::Ccxt(
            "qianxing.runtime.multi-venue-arbitrage.example.json",
            "qianxing.ccxt.okx.swap.example.json",
        ),
        Expected::Ok,
    ),
    // 公共数据样例没有任何 runtime 挂它，只能借同一交易所的 worker 做绑定体检：
    // 这里钉的是 exchange_id 与凭据引用形状，钉不出"哪个 worker 该用它"。
    (
        "qianxing.ccxt.binance.public.example.json",
        Reader::Ccxt(
            "qianxing.runtime.multi-venue-arbitrage.example.json",
            "qianxing.ccxt.binance.spot.example.json",
        ),
        Expected::Ok,
    ),
    // —— 行情夹具（7）——
    (
        "qianxing.bar-frame.example.json",
        Reader::BarFrame,
        Expected::Ok,
    ),
    (
        "qianxing.bar-frame.okx.example.json",
        Reader::BarFrame,
        Expected::Ok,
    ),
    (
        "qianxing.bar-frame.pairs-primary.example.json",
        Reader::BarFrame,
        Expected::Ok,
    ),
    (
        "qianxing.bar-frame.pairs-reference.example.json",
        Reader::BarFrame,
        Expected::Ok,
    ),
    (
        "qianxing.ashare.bar-frame.example.json",
        Reader::BarFrame,
        Expected::Ok,
    ),
    (
        "qianxing.depth-frame.example.json",
        Reader::DepthFrame,
        Expected::Ok,
    ),
    (
        "qianxing.depth-frame.l1.example.json",
        Reader::DepthFrame,
        Expected::Ok,
    ),
    // —— 数据集清单（4）——
    (
        "qianxing.dataset-bundle.example.json",
        Reader::DatasetBundle,
        Expected::Ok,
    ),
    (
        "qianxing.dataset-bundle.bar-frame.example.json",
        Reader::DatasetBundle,
        Expected::Ok,
    ),
    (
        "qianxing.dataset-bundle.ashare.example.json",
        Reader::DatasetBundle,
        Expected::Ok,
    ),
    (
        "qianxing.dataset-component.arrow.example.json",
        Reader::ArrowComponent,
        Expected::Ok,
    ),
    // —— A 股制度段（4）——
    (
        "qianxing.ashare.rules.json",
        Reader::AshareRules,
        Expected::Ok,
    ),
    (
        "qianxing.ashare.calendar.example.json",
        Reader::AshareCalendar,
        Expected::Ok,
    ),
    (
        "qianxing.ashare.actions.example.json",
        Reader::AshareActions,
        Expected::Ok,
    ),
    (
        "qianxing.ashare.complex-actions.example.json",
        Reader::AshareActions,
        Expected::Ok,
    ),
    // —— 其余单份模板（7）——
    (
        "qianxing.costs.example.json",
        Reader::CostRules,
        Expected::Ok,
    ),
    (
        "qianxing.scheduler.jobs.example.json",
        Reader::SchedulerJobs,
        Expected::Ok,
    ),
    (
        "qianxing.scheduler.jobs.smoke.json",
        Reader::SchedulerJobs,
        Expected::Ok,
    ),
    (
        "qianxing.scheduler.paper-order-smoke.json",
        Reader::SchedulerJobs,
        Expected::Ok,
    ),
    (
        "qianxing.strategy-target.paper.json",
        Reader::StrategyTarget,
        Expected::Ok,
    ),
    (
        "qianxing.submit-order.example.json",
        Reader::SubmitOrder,
        Expected::Ok,
    ),
    (
        "qianxing.submit-order.ccxt-derivatives.example.json",
        Reader::SubmitOrder,
        Expected::Ok,
    ),
    (
        "qianxing.paper-submit-order.example.json",
        Reader::SubmitOrder,
        Expected::Ok,
    ),
    // —— 快速回测 manifest（2）——
    (
        "qianxing.fast-backtest.example.json",
        Reader::FastBacktest,
        Expected::Ok,
    ),
    (
        "qianxing.fast-backtest.ashare.example.json",
        Reader::FastBacktest,
        Expected::Ok,
    ),
];

fn deploy_template(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join(name)
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("读取 {} 失败: {error}", path.display()))
}

fn parse_json(path: &Path) -> Result<serde_json::Value, String> {
    let payload = read_text(path)?;
    serde_json::from_str(&payload)
        .map_err(|error| format!("{} 不是合法 JSON: {error}", path.display()))
}

/// BarFrame 模板自己声明的标的字符串；规格的标的就从这里取。
fn frame_instrument(name: &str) -> Result<String, String> {
    let value = parse_json(&deploy_template(name))?;
    value
        .get("instrument")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{name} 没有顶层 instrument，不能当标的配对来源"))
}

fn runtime_config(name: &str) -> Result<RuntimeConfig, String> {
    read_runtime_config(&deploy_template(name))
}

/// 在 `runtime` 模板里找真正挂着 `endpoint_of` 的那个工作进程 —— 配对失效即报错，
/// 这样登记表里写死的 runtime 一旦改走别的文件，本用例就会红，而不是静默换个 worker 继续绿。
fn worker_mounting_endpoint(runtime: &str, endpoint_of: &str) -> Result<WorkerConfig, String> {
    let config = runtime_config(runtime)?;
    config
        .workers
        .into_iter()
        .find(|worker| {
            worker.endpoint.as_deref().is_some_and(|endpoint| {
                Path::new(endpoint)
                    .file_name()
                    .is_some_and(|name| name == endpoint_of)
            })
        })
        .ok_or_else(|| format!("runtime 模板 {runtime} 里已没有挂载 {endpoint_of} 的 worker"))
}

/// 按登记的读法解析一份模板；`Err` 的字符串就是生产读法给出的报错。
fn load_through_registered_reader(
    name: &str,
    reader: Reader,
    path: &Path,
) -> Result<String, String> {
    match reader {
        Reader::Runtime => {
            let config = read_runtime_config(path)?;
            let (failures, _warnings) = validate_runtime_references(path, &config);
            if failures.is_empty() {
                Ok(format!(
                    "workers={} storage={:?}",
                    config.workers.len(),
                    config.storage.backend
                ))
            } else {
                Err(format!(
                    "config validate 失败 {} 项: {}",
                    failures.len(),
                    failures.join("；")
                ))
            }
        }
        Reader::MarketSpec(instrument_source) => {
            let market = parse_json(path)?;
            let label = market_spec_source_label(&market);
            let instrument = match instrument_source {
                Some(frame) => {
                    let text = frame_instrument(frame)?;
                    InstrumentId::parse(&text).ok_or_else(|| format!("标的 {text} 非法"))?
                }
                None => {
                    // 产品规格形状把标的写在文件里（对象，不是 `SYMBOL.VENUE` 字符串）。
                    let declared = market
                        .get("instrument")
                        .cloned()
                        .ok_or_else(|| format!("{name} 是产品规格形状却没有 instrument"))?;
                    serde_json::from_value::<InstrumentId>(declared)
                        .map_err(|error| format!("{name} 的 instrument 读不回标的: {error}"))?
                }
            };
            let spec = market_spec_from_value(&instrument, &market)?;
            Ok(format!(
                "instrument={} product={:?} settle={} source={label}",
                spec.instrument, spec.product, spec.settlement_currency
            ))
        }
        Reader::Ccxt(runtime, endpoint_of) => {
            let worker = worker_mounting_endpoint(runtime, endpoint_of)?;
            validate_ccxt_worker_binding(&worker, path)?;
            let exchange_id = parse_json(path)?
                .get("exchange_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            Ok(format!(
                "worker={} venue={:?} exchange_id={exchange_id}",
                worker.id, worker.venue_id
            ))
        }
        Reader::BarFrame => {
            // 单标的回测链真正执行的是**两个**读点：先按列式帧解一次，再按 qx-data 的严格
            // Provider 解一次并比对（`strategy_backtest.rs` 里紧挨着的那两行）。只调前者等于
            // 放过"同一份文件在两条口径里读成两个答案"，这里因此要求两读都过。
            let frame = read_bar_frame_for_backtest(path)?;
            let (bars, manifest) = barframe_dataset_identity(path, &frame)?;
            if bars.is_empty() {
                return Err(format!("{name} 解析出 0 根 Bar"));
            }
            Ok(format!(
                "instrument={} bars={} fingerprint={}",
                frame.instrument,
                bars.len(),
                manifest.fingerprint
            ))
        }
        Reader::DepthFrame => {
            let frame = DepthFrame::from_json(&read_text(path)?)?;
            if frame.snapshots.is_empty() {
                return Err(format!("{name} 解析出 0 条快照"));
            }
            Ok(format!(
                "instrument={} snapshots={}",
                frame.instrument,
                frame.snapshots.len()
            ))
        }
        Reader::DatasetBundle => {
            let bundle: qx_data::DatasetBundleManifest = serde_json::from_str(&read_text(path)?)
                .map_err(|error| format!("DatasetBundleManifest JSON 无效: {error}"))?;
            bundle.validate()?;
            let fingerprint = bundle.fingerprint()?;
            Ok(format!(
                "bundle={} version={} components={} fingerprint={fingerprint}",
                bundle.bundle_id,
                bundle.version,
                bundle.components.len()
            ))
        }
        Reader::ArrowComponent => {
            let manifest = qx_data::ArrowDatasetManifest::from_json(&read_text(path)?)?;
            Ok(format!(
                "dataset={} version={} rows={} kind={}",
                manifest.dataset.dataset_id,
                manifest.dataset.version,
                manifest.row_count,
                manifest.kind
            ))
        }
        Reader::AshareRules => {
            let instrument = frame_instrument("qianxing.ashare.bar-frame.example.json")?;
            let binding = ashare_backtest_binding(
                Some(path.to_string_lossy().as_ref()),
                Some(
                    deploy_template("qianxing.ashare.actions.example.json")
                        .to_string_lossy()
                        .as_ref(),
                ),
                Some(
                    deploy_template("qianxing.ashare.calendar.example.json")
                        .to_string_lossy()
                        .as_ref(),
                ),
                &instrument,
            )?;
            let binding = binding.ok_or_else(|| "规则路径已给出却没有装配出 A 股段".to_string())?;
            Ok(format!(
                "instrument={instrument} commission_bp={} stamp_duty_bp={}",
                binding.rules.commission_bp, binding.rules.stamp_duty_bp
            ))
        }
        Reader::AshareActions => {
            let document = parse_json(path)?;
            let rows = document
                .as_array()
                .ok_or_else(|| format!("{name} 必须是动作数组"))?;
            let instrument = rows
                .first()
                .and_then(|row| row.get("instrument"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("{name} 的行里没有 instrument"))?
                .to_string();
            let mut rules = AshareRuleConfig::default();
            let report =
                rules.apply_corporate_actions_json_with_report(&instrument, &read_text(path)?)?;
            Ok(format!(
                "instrument={instrument} rows={} schema_version={}",
                report.row_count, report.schema_version
            ))
        }
        Reader::AshareCalendar => {
            let mut rules = AshareRuleConfig::default();
            let report = rules.apply_calendar_json_with_report(&read_text(path)?)?;
            Ok(format!(
                "calendar_id={} trading_days={} schema_version={}",
                report.calendar_id, report.trading_days, report.schema_version
            ))
        }
        Reader::CostRules => {
            let rules = ExecutionCostRules::load(path).map_err(|error| format!("{error:?}"))?;
            Ok(rules.descriptor())
        }
        Reader::SchedulerJobs => {
            let jobs: Vec<JobSpec> = serde_json::from_str(&read_text(path)?)
                .map_err(|error| format!("Scheduler JobSpec JSON 无效: {error}"))?;
            let refused = jobs
                .iter()
                .filter_map(unsupported_dispatch_shape)
                .collect::<Vec<_>>();
            if !refused.is_empty() {
                return Err(refused.join("；"));
            }
            let mut scheduler = Scheduler::default();
            for job in jobs {
                scheduler
                    .register(job)
                    .map_err(|error| format!("注册 Scheduler JobSpec 失败: {error:?}"))?;
            }
            Ok(format!("jobs={}", scheduler.len()))
        }
        Reader::StrategyTarget => {
            let snapshot: StrategyTargetSnapshot = serde_json::from_str(&read_text(path)?)
                .map_err(|error| format!("StrategyTargetSnapshot JSON 无效: {error}"))?;
            snapshot.validate()?;
            Ok(format!(
                "strategy_version={} targets={}",
                snapshot.strategy_version,
                snapshot.targets.len()
            ))
        }
        Reader::SubmitOrder => {
            let command: ControlCommand = serde_json::from_str(&read_text(path)?)
                .map_err(|error| format!("ControlCommand JSON 无效: {error}"))?;
            let order = order_from_submit_command(&command)
                .map_err(|error| format!("订单载荷非法: {error:?}"))?;
            Ok(format!(
                "command_id={} instrument={} side={:?} dry_run={}",
                command.command_id, order.instrument, order.side, command.dry_run
            ))
        }
        Reader::FastBacktest => {
            let jobs = parse_fast_backtest_manifest(path)?;
            if jobs.is_empty() {
                return Err(format!("{name} 没有作业"));
            }
            let mut missing = Vec::new();
            for job in &jobs {
                for declared in [&job.runtime, &job.bars]
                    .into_iter()
                    .chain(job.market_spec.iter())
                {
                    if !declared.is_file() {
                        missing.push(declared.display().to_string());
                    }
                }
            }
            if !missing.is_empty() {
                return Err(format!("{name} 引用了不存在的文件: {}", missing.join("、")));
            }
            Ok(format!("jobs={}", jobs.len()))
        }
    }
}

/// 坏内容探针：任何读取器都不能把一份坏 JSON 读成成功。
///
/// 默认给"连形状都不是"的一档；`CostRules` 单独给"形状对、取值越界"的一档 —— 它每个字段都带
/// serde 默认值，空对象会被读成内置默认费率（本轮实测），只换形状打不中它。
fn broken_payload(reader: Reader) -> String {
    match reader {
        Reader::CostRules => "{\"name\":\"broken\",\"maker_bp\":2,\"taker_bp\":100001}".to_string(),
        _ => "{\"coverage\":\"broken\"}".to_string(),
    }
}

fn load_broken_copy(name: &str, reader: Reader, root: &Path) -> Result<String, String> {
    let path = root.join(format!("broken-{name}"));
    std::fs::write(&path, broken_payload(reader)).expect("写入坏探针失败");
    load_through_registered_reader(name, reader, &path)
}

/// 登记表必须与磁盘上的顶层模板清单**完全相等**：既不许漏登记，也不许登记已删除的文件。
#[test]
fn every_deploy_template_is_registered_with_a_reader() {
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let mut on_disk = std::fs::read_dir(&deploy)
        .unwrap_or_else(|error| panic!("读不到 {}: {error}", deploy.display()))
        .map(|entry| entry.expect("目录项可读").path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            path.file_name()
                .expect("顶层文件有文件名")
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    on_disk.sort();
    let mut registered = COVERAGE
        .iter()
        .map(|(name, _, _)| (*name).to_string())
        .collect::<Vec<_>>();
    registered.sort();
    registered.dedup();
    assert_eq!(
        registered.len(),
        COVERAGE.len(),
        "登记表里有重名模板，去重前后数量不等"
    );
    assert_eq!(
        registered, on_disk,
        "deploy 顶层模板与登记表不一致：左边是登记表、右边是磁盘"
    );
}

/// 每一模板都必须按登记的读法被解析一次；预期结果写在登记表里，不靠"跑通"自证。
#[test]
fn every_deploy_template_loads_through_its_registered_reader() {
    let mut problems = Vec::new();
    for (name, reader, expected) in COVERAGE {
        let path = deploy_template(name);
        let outcome = load_through_registered_reader(name, *reader, &path);
        match (expected, outcome) {
            (Expected::Ok, Err(error)) => problems.push(format!("{name} 读取失败: {error}")),
            (Expected::Ok, Ok(identity)) => println!("{name} => {identity}"),
            (Expected::Refuses(needles), Ok(identity)) => problems.push(format!(
                "{name} 本该按 {needles:?} 被拒，却读成了: {identity}"
            )),
            (Expected::Refuses(needles), Err(error)) => {
                let missed = needles
                    .iter()
                    .filter(|needle| !error.contains(**needle))
                    .collect::<Vec<_>>();
                if missed.is_empty() {
                    println!("{name} => 按已点名的 {needles:?} 被拒: {error}");
                } else {
                    problems.push(format!(
                        "{name} 被拒了，但报错里点不出已知的缺口 {missed:?}: {error}"
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "deploy 模板读取覆盖失败 {} 处:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// 读取器的全部变体（探针用；带参数的变体取一个真实配对作为代表）。
fn reader_classes() -> Vec<Reader> {
    vec![
        Reader::Runtime,
        Reader::MarketSpec(None),
        Reader::Ccxt(
            "qianxing.runtime.ccxt.example.json",
            "qianxing.ccxt.exchange.example.json",
        ),
        Reader::MarketSpec(Some("qianxing.ashare.bar-frame.example.json")),
        Reader::BarFrame,
        Reader::DepthFrame,
        Reader::DatasetBundle,
        Reader::ArrowComponent,
        Reader::AshareRules,
        Reader::AshareActions,
        Reader::AshareCalendar,
        Reader::CostRules,
        Reader::SchedulerJobs,
        Reader::StrategyTarget,
        Reader::SubmitOrder,
        Reader::FastBacktest,
    ]
}

/// 每个读取器都要对坏内容表态：同一个变体至少要有一条"给了垃圾就报错"的实测。
///
/// 反向验证（V13 R1-A6）：把任一读取器换成 `serde_json::Value` 走过场，本用例即红。
#[test]
fn every_reader_class_rejects_a_broken_template() {
    let root = temp_cli_case_dir("a6-template-coverage-broken");
    let mut checked = 0usize;
    let mut problems = Vec::new();
    for class in reader_classes() {
        let Some((name, _, _)) = COVERAGE.iter().find(|(_, reader, _)| *reader == class) else {
            problems.push(format!("{class:?} 在登记表里没有任何模板，读取器无人验证"));
            continue;
        };
        match load_broken_copy(name, class, &root) {
            Ok(identity) => problems.push(format!("{name} 的读取器接受了坏内容: {identity}")),
            Err(_) => checked += 1,
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        problems.is_empty(),
        "坏内容探针失败 {} 处:\n{}",
        problems.len(),
        problems.join("\n")
    );
    assert_eq!(
        checked,
        reader_classes().len(),
        "探针没能覆盖全部读取器变体"
    );
}
