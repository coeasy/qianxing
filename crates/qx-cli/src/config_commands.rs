//! 配置与运维查询命令面：`init` / `config` / `status` / `doctor` / `report` / `live-check`。
//!
//! 本模块只做配置读取、引用校验与人读/机读输出，不改变运行时语义；
//! 真实执行链路仍复用 crate 根上的同一批 Runtime/Storage 辅助函数。
//! 帮助文本本身不在这里，见 `cli_help.rs`（它与派发分支由架构门禁做集合相等校验）。

use super::*;
pub(crate) fn repository_deploy_path(file_name: &str) -> PathBuf {
    let source_tree_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join(file_name);
    if source_tree_path.exists() {
        source_tree_path
    } else {
        PathBuf::from("deploy").join(file_name)
    }
}

pub(crate) fn copy_init_asset(
    root: &Path,
    file_name: &str,
    force: bool,
) -> Result<PathBuf, String> {
    let source = repository_deploy_path(file_name);
    if !source.is_file() {
        return Err(format!("找不到初始化样例文件: {}", source.display()));
    }
    let target = root.join(file_name);
    if target.exists() {
        let same_file = source
            .canonicalize()
            .ok()
            .zip(target.canonicalize().ok())
            .is_some_and(|(source, target)| source == target);
        if same_file || !force {
            return Ok(target);
        }
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建初始化样例目录失败 {}: {error}", parent.display()))?;
    }
    std::fs::copy(&source, &target).map_err(|error| {
        format!(
            "复制初始化样例失败 {} -> {}: {error}",
            source.display(),
            target.display()
        )
    })?;
    Ok(target)
}

pub(crate) fn init_profile_template(
    profile: Option<&str>,
    strategy_name: Option<&str>,
) -> Result<(&'static str, Vec<&'static str>, &'static str), String> {
    let selected = match profile {
        None => {
            if strategy_name.is_some() {
                "builtin"
            } else {
                "base"
            }
        }
        Some("base") => "base",
        Some("builtin") => "builtin",
        Some("paper") => "paper",
        Some("ccxt") => "ccxt",
        Some("ashare") => "ashare",
        Some("multi-venue") => "multi-venue",
        Some("backtest") => "backtest",
        Some(other) => {
            return Err(format!(
                "未知 init profile={other}；可用值：base、paper、ccxt、ashare、multi-venue、backtest"
            ));
        }
    };
    if strategy_name.is_some() && !matches!(selected, "base" | "builtin") {
        return Err(format!(
            "init --strategy 只能与 base/builtin profile 组合；当前 profile={selected}"
        ));
    }
    let common = vec![
        "qianxing.scheduler.jobs.example.json",
        "qianxing.scheduler.paper-order-smoke.json",
        "qianxing.bar-frame.example.json",
        "qianxing.dataset-bundle.bar-frame.example.json",
        "qianxing.dataset-component.arrow.example.json",
        "qianxing.binance.spot.spec.json",
        "qianxing.strategy-target.paper.json",
    ];
    match selected {
        "base" | "builtin" => Ok((
            if selected == "builtin" {
                "qianxing.runtime.builtin-strategy.example.json"
            } else {
                "qianxing.runtime.example.json"
            },
            common,
            selected,
        )),
        "paper" => Ok((
            "qianxing.runtime.paper-strategy.example.json",
            common,
            selected,
        )),
        "ccxt" => Ok((
            "qianxing.runtime.ccxt.example.json",
            vec![
                "qianxing.ccxt.exchange.example.json",
                "qianxing.ccxt.okx.perpetual.spec.json",
                "qianxing.scheduler.jobs.example.json",
            ],
            selected,
        )),
        "ashare" => Ok((
            "qianxing.runtime.ashare.example.json",
            vec![
                "qianxing.ashare.actions.example.json",
                "qianxing.ashare.bar-frame.example.json",
                "qianxing.ashare.calendar.example.json",
                "qianxing.ashare.rules.json",
                "qianxing.ashare.spot.spec.json",
                "qianxing.dataset-bundle.ashare.example.json",
            ],
            selected,
        )),
        "multi-venue" => Ok((
            "qianxing.runtime.multi-venue-arbitrage.example.json",
            vec![
                "qianxing.ccxt.binance.spot.example.json",
                "qianxing.ccxt.okx.swap.example.json",
                "qianxing.binance.spot.spec.json",
                "qianxing.ccxt.okx.perpetual.spec.json",
                "qianxing.scheduler.jobs.example.json",
            ],
            selected,
        )),
        "backtest" => Ok((
            "qianxing.runtime.strategy-backtest.example.json",
            vec!["qianxing.bar-frame.example.json"],
            selected,
        )),
        _ => Err(format!(
            "未知 init profile={selected}；可用值：base、paper、ccxt、ashare、multi-venue、backtest"
        )),
    }
}

pub(crate) fn normalize_init_template_paths(
    value: &mut serde_json::Value,
    assets: &mut BTreeSet<String>,
) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(relative) = text.strip_prefix("deploy/") {
                let relative = relative.to_owned();
                if repository_deploy_path(&relative).is_file() {
                    *text = relative.clone();
                    assets.insert(relative);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                normalize_init_template_paths(value, assets);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                normalize_init_template_paths(value, assets);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

pub(crate) fn init_readme(output: &Path, strategy: Option<&str>, profile: &str) -> String {
    let strategy_line = strategy
        .map(|name| format!("已绑定内置策略：`{name}`。"))
        .unwrap_or_else(|| {
            "当前配置为基础运行时，可通过 `qianxing init --strategy macd --force` 绑定内置策略。"
                .into()
        });
    format!(
        "# Qianxing 本地项目\n\n运行时配置：`{}`。profile=`{}`。{}\n\n## 推荐流程\n\n```text\nqianxing doctor {}\nqianxing config explain {}\nqianxing config validate {}\nqianxing backtest {} qianxing.bar-frame.example.json qianxing.binance.spot.spec.json\nqianxing paper-check {}\n```\n\n初始化生成的样例文件只用于本地回测和 Paper 验收，不包含交易密钥，也不会自动发送真实订单。CCXT/多交易所 profile 只生成公共配置和凭据引用，必须自行配置环境变量后再做 sandbox 验收。\n",
        output.display(),
        profile,
        strategy_line,
        output.display(),
        output.display(),
        output.display(),
        output.display(),
        output.display()
    )
}

pub(crate) fn run_init_with_profile(
    output: &Path,
    force: bool,
    strategy_name: Option<&str>,
    profile: Option<&str>,
) -> Result<(), String> {
    let (template_name, profile_assets, profile_name) =
        init_profile_template(profile, strategy_name)?;
    let template = repository_deploy_path(template_name);
    if !template.exists() {
        return Err(format!("找不到运行时模板: {}", template.display()));
    }
    if output.exists() && !force {
        return Err(format!(
            "目标配置已存在: {}；如确认覆盖，请显式添加 --force",
            output.display()
        ));
    }
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建配置目录失败 {}: {error}", parent.display()))?;
    }
    let mut document: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&template)
            .map_err(|error| format!("读取运行时模板失败 {}: {error}", template.display()))?,
    )
    .map_err(|error| format!("运行时模板 JSON 无效 {}: {error}", template.display()))?;
    let mut init_assets = profile_assets.into_iter().map(str::to_owned).collect();
    normalize_init_template_paths(&mut document, &mut init_assets);
    if let Some(strategy_name) = strategy_name {
        let kind = BuiltinStrategyKind::parse(strategy_name)?;
        let strategy = document
            .get_mut("strategy")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| "内置策略运行时模板缺少 strategy 对象".to_string())?;
        strategy.insert(
            "id".into(),
            serde_json::Value::String(format!("strategy-{}", kind.name())),
        );
        strategy.insert(
            "version".into(),
            serde_json::Value::String(format!("builtin-{}-v1", kind.name())),
        );
        strategy.insert(
            "builtin_strategy".into(),
            serde_json::Value::String(kind.name().into()),
        );
        strategy.insert(
            "bars_snapshot_path".into(),
            serde_json::Value::String("qianxing.bar-frame.example.json".into()),
        );
        strategy.insert(
            "dataset_bundle_path".into(),
            serde_json::Value::String("qianxing.dataset-bundle.bar-frame.example.json".into()),
        );
        if let Some(storage) = document
            .get_mut("storage")
            .and_then(serde_json::Value::as_object_mut)
        {
            storage.insert(
                "data_dir".into(),
                serde_json::Value::String("data/qianxing".into()),
            );
        }
    }
    let payload = serde_json::to_string_pretty(&document)
        .map_err(|error| format!("编码初始化运行时配置失败: {error}"))?;
    RuntimeConfig::from_json(&payload)?;
    std::fs::write(output, payload)
        .map_err(|error| format!("写入运行时配置失败 {}: {error}", output.display()))?;
    let project_root = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    for asset in &init_assets {
        copy_init_asset(project_root, asset, force)?;
    }
    std::fs::create_dir_all(project_root.join("data"))
        .map_err(|error| format!("创建项目数据目录失败: {error}"))?;
    let readme_path = project_root.join("README.qianxing.md");
    if force || !readme_path.exists() {
        std::fs::write(
            &readme_path,
            init_readme(output, strategy_name, profile_name),
        )
        .map_err(|error| format!("写入初始化说明失败 {}: {error}", readme_path.display()))?;
    }
    let config = read_runtime_config(output)?;
    println!(
        "[初始化] 已创建 {} profile={} environment={} fingerprint={} assets={} strategy={}",
        output.display(),
        profile_name,
        config.environment,
        config.fingerprint()?,
        init_assets.len(),
        strategy_name.unwrap_or("none")
    );
    println!(
        "下一步：qianxing doctor {}；qianxing backtest {} qianxing.bar-frame.example.json qianxing.binance.spot.spec.json",
        output.display(),
        output.display()
    );
    Ok(())
}

pub(crate) fn run_strategy_init(
    strategy_name: &str,
    output: &Path,
    bars_path: Option<&Path>,
    force: bool,
) -> Result<(), String> {
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    if output.exists() && !force {
        return Err(format!(
            "目标策略配置已存在: {}；如确认覆盖，请显式添加 --force",
            output.display()
        ));
    }
    let template = repository_deploy_path("qianxing.runtime.builtin-strategy.example.json");
    let payload = std::fs::read_to_string(&template)
        .map_err(|error| format!("读取内置策略模板失败 {}: {error}", template.display()))?;
    let mut document: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("内置策略模板 JSON 无效: {error}"))?;
    let strategy = document
        .get_mut("strategy")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "内置策略模板缺少 strategy 对象".to_string())?;
    strategy.insert(
        "id".into(),
        serde_json::Value::String(format!("strategy-{}", kind.name())),
    );
    strategy.insert(
        "version".into(),
        serde_json::Value::String(format!("builtin-{}-v1", kind.name())),
    );
    strategy.insert(
        "builtin_strategy".into(),
        serde_json::Value::String(kind.name().into()),
    );
    let bars = bars_path
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "deploy/qianxing.bar-frame.example.json".into());
    strategy.insert(
        "bars_snapshot_path".into(),
        serde_json::Value::String(bars.clone()),
    );
    if bars_path.is_none() {
        strategy.insert(
            "dataset_bundle_path".into(),
            serde_json::Value::String(
                "deploy/qianxing.dataset-bundle.bar-frame.example.json".into(),
            ),
        );
    } else {
        strategy.remove("dataset_bundle_path");
    }
    let result = serde_json::to_string_pretty(&document)
        .map_err(|error| format!("编码内置策略配置失败: {error}"))?;
    RuntimeConfig::from_json(&result)?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建策略配置目录失败 {}: {error}", parent.display()))?;
    }
    std::fs::write(output, result)
        .map_err(|error| format!("写入策略配置失败 {}: {error}", output.display()))?;
    println!(
        "[Strategy · Init] strategy={} output={} bars={}",
        kind.name(),
        output.display(),
        bars
    );
    println!(
        "下一步：qianxing strategy backtest {} {}",
        output.display(),
        bars
    );
    Ok(())
}

pub(crate) fn default_runtime_path() -> PathBuf {
    repository_deploy_path("qianxing.runtime.example.json")
}

pub(crate) fn run_config_explain(path: &Path, as_json: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    if as_json {
        // RuntimeConfig 只包含凭据引用（环境变量名/投影文件路径），不包含
        // secret 内容；输出可直接交给脚本或配置审计工具继续处理。
        println!("{}", config.to_json()?);
        return Ok(());
    }
    let strategy_count = if config.strategies.is_empty() {
        1
    } else {
        config.strategies.len()
    };
    println!("[配置 · 有效] path={}", path.display());
    println!(
        "  schema={} environment={} profile={:?}",
        config.schema_version, config.environment, config.profile
    );
    println!(
        "  storage={:?} data_dir={} api={} transport={:?}",
        config.storage.backend,
        // 与 doctor 同口径：相对 data_dir 有两个落点（可写运行态按进程当前目录，
        // 回测产物按 runtime.json 同级），报告真正在被使用的那个。
        effective_storage_root(path, &config.storage.data_dir).display(),
        config.api.bind,
        config.api.transport
    );
    println!(
        "  workers={} enabled={} strategies={} fingerprint={} locked={}",
        config.workers.len(),
        config
            .workers
            .iter()
            .filter(|worker| worker.enabled)
            .count(),
        strategy_count,
        config.fingerprint()?,
        config.config_fingerprint.is_some()
    );
    for worker in config.workers.iter().filter(|worker| worker.enabled) {
        println!(
            "  worker id={} role={:?} account={} venue={}",
            worker.id,
            worker.role,
            worker.account_id.as_deref().unwrap_or("-"),
            worker.venue_id.as_deref().unwrap_or("-")
        );
    }
    println!("[配置 · 安全] 未读取密钥内容，仅检查引用名称和文件路径");
    Ok(())
}

pub(crate) fn run_config_fingerprint(path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    println!(
        "[配置 · Fingerprint] path={} fingerprint={}",
        path.display(),
        config.fingerprint()?
    );
    Ok(())
}

pub(crate) fn run_config_lock(input: &Path, output: &Path, force: bool) -> Result<(), String> {
    let config = read_runtime_config(input)?;
    let fingerprint = config.fingerprint()?;
    if output.exists() && !force {
        return Err(format!(
            "目标发布配置已存在: {}；如确认覆盖，请显式添加 --force",
            output.display()
        ));
    }
    let mut locked = config;
    locked.config_fingerprint = Some(fingerprint.clone());
    let payload = locked.to_json()?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建发布配置目录失败 {}: {error}", parent.display()))?;
    }
    std::fs::write(output, payload)
        .map_err(|error| format!("写入发布配置失败 {}: {error}", output.display()))?;
    let verified = read_runtime_config(output)?;
    verified.verify_fingerprint()?;
    println!(
        "[配置 · Lock] input={} output={} fingerprint={} locked=true",
        input.display(),
        output.display(),
        fingerprint
    );
    Ok(())
}

/// `run` 的可用入口清单：help 文案、缺参提示与"不支持"提示共用同一份。
/// 三处各写一遍正是 V10 §4.3 第 4 项"文案自称支持 backtest 而 match 没有该分支"的来源；
/// `tools/check_architecture.py` 会把本清单与 help 入口行、下面的 match 分支做集合相等校验。
pub(crate) const RUN_ENTRY_POINTS: [&str; 7] = [
    "backtest",
    "paper",
    "paper-check",
    "doctor",
    "live-check",
    "runtime-check",
    "report",
];

fn run_usage(phrase: &str) -> String {
    format!("run {phrase}；可用入口：{}", RUN_ENTRY_POINTS.join("、"))
}

pub(crate) fn run_unified_command(arguments: &[String]) -> Result<(), String> {
    let action = arguments
        .first()
        .map(String::as_str)
        .ok_or_else(|| run_usage("需要入口参数"))?;
    match action {
        "backtest" => {
            let positional: Vec<String> = arguments[1..]
                .iter()
                .filter(|value| !value.starts_with('-'))
                .cloned()
                .collect();
            // 收下却不处理等于对用户撒谎：旗标与多余位置参数都报用法，而不是静默丢弃。
            if arguments.len() - 1 > positional.len() || positional.len() > 3 {
                return Err(run_usage(
                    "backtest 不接受旗标，位置参数至多三个：<runtime.json> [bar-frame.json] [market-spec.json]",
                ));
            }
            run_unified_backtest(
                positional.first().map(PathBuf::from).as_deref(),
                positional.get(1).map(PathBuf::from).as_deref(),
                positional.get(2).map(PathBuf::from).as_deref(),
            )
        }
        "paper" | "paper-check" => {
            let path = arguments.get(1).map(PathBuf::from).unwrap_or_else(|| {
                repository_deploy_path("qianxing.runtime.paper-strategy.example.json")
            });
            run_paper_pipeline_once(&path)
        }
        "doctor" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(default_runtime_path);
            run_doctor(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        "live-check" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    repository_deploy_path("qianxing.runtime.production.example.json")
                });
            run_live_check(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        "runtime-check" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(default_runtime_path);
            run_runtime_check(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        "report" => {
            let path = arguments
                .iter()
                .skip(1)
                .find(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(default_runtime_path);
            run_report(&path, arguments.iter().any(|argument| argument == "--json"))
        }
        _ => Err(run_usage(&format!("不支持 {action}"))),
    }
}

pub(crate) fn list_backtest_summary_paths(runs_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut summaries = Vec::new();
    if !runs_dir.is_dir() {
        return Ok(summaries);
    }
    for entry in std::fs::read_dir(runs_dir)
        .map_err(|error| format!("读取回测结果目录失败 {}: {error}", runs_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("读取回测结果目录项失败: {error}"))?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".summary.json"))
        {
            summaries.push(path);
        }
    }
    summaries.sort_by(|left, right| {
        let left_modified = std::fs::metadata(left)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        let right_modified = std::fs::metadata(right)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        left_modified
            .cmp(&right_modified)
            .then_with(|| left.cmp(right))
    });
    Ok(summaries)
}

pub(crate) fn resolve_backtest_summary_path(path: &Path) -> Result<PathBuf, String> {
    let is_summary = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".summary.json"));
    if is_summary {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(format!("指定回测摘要不存在: {}", path.display()));
    }
    let config = read_runtime_config(path)?;
    let data_dir = resolve_runtime_relative_path(path, &config.storage.data_dir);
    let summaries = list_backtest_summary_paths(&data_dir.join("runs"))?;
    summaries
        .last()
        .cloned()
        .ok_or_else(|| format!("未找到回测摘要: {}", data_dir.join("runs").display()))
}

pub(crate) fn run_report(path: &Path, as_json: bool) -> Result<(), String> {
    let summary_path = resolve_backtest_summary_path(path)?;
    let payload = std::fs::read_to_string(&summary_path)
        .map_err(|error| format!("读取回测摘要失败 {}: {error}", summary_path.display()))?;
    let summary: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("回测摘要 JSON 无效 {}: {error}", summary_path.display()))?;
    if as_json {
        let report = serde_json::json!({
            "schema_version": 1,
            "summary_path": summary_path.display().to_string(),
            "summary": summary
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码回测报告 JSON 失败: {error}"))?
        );
        return Ok(());
    }

    let text = |pointer: &str, fallback: &str| -> String {
        summary
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(fallback)
            .to_string()
    };
    let integer = |pointer: &str| {
        summary
            .pointer(pointer)
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
    };
    println!("[Report] summary={}", summary_path.display());
    println!(
        "  strategy={} instrument={} bars={} fills={}",
        text("/strategy_id", "-"),
        text("/instrument", "-"),
        integer("/bars"),
        integer("/fills")
    );
    println!(
        "  return_bps={} max_drawdown_bps={} fees_raw={} turnover_raw={} final_equity_raw={}",
        integer("/metrics/return_bps"),
        integer("/metrics/max_drawdown_bps"),
        integer("/metrics/fees_raw"),
        integer("/metrics/turnover_raw"),
        integer("/metrics/final_equity_raw")
    );
    println!(
        "  input_data_hash={} result_hash={} replay_hash={}",
        text("/input_data_hash", "-"),
        text("/result_hash", "-"),
        text("/replay_hash", "-")
    );
    println!(
        "  risk_rule_set_version={}",
        text("/risk_rules/rule_set_version", "-")
    );
    Ok(())
}

pub(crate) fn run_status(path: &Path, as_json: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let data_dir = resolve_runtime_relative_path(path, &config.storage.data_dir);
    let runs_dir = data_dir.join("runs");
    let summaries = list_backtest_summary_paths(&runs_dir)?;
    let latest_summary = summaries.last().and_then(|summary_path| {
        std::fs::read_to_string(summary_path)
            .ok()
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
    });
    let enabled_workers = config
        .workers
        .iter()
        .filter(|worker| worker.enabled)
        .map(|worker| {
            serde_json::json!({
                "id": worker.id,
                "role": format!("{:?}", worker.role),
                "account_id": worker.account_id,
                "venue_id": worker.venue_id,
            })
        })
        .collect::<Vec<_>>();
    if as_json {
        let status = serde_json::json!({
            "schema_version": 1,
            "runtime_path": path,
            "environment": config.environment,
            "profile": config.profile,
            "storage_backend": config.storage.backend,
            "storage_consistency": config.storage.consistency,
            "data_dir": data_dir,
            "config_fingerprint": config.fingerprint()?,
            "enabled_workers": enabled_workers,
            "backtest_summary_count": summaries.len(),
            "latest_backtest": latest_summary,
            "network_accessed": false,
            "orders_sent": false,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&status)
                .map_err(|error| format!("编码 status JSON 失败: {error}"))?
        );
        return Ok(());
    }
    println!("[Status] runtime={}", path.display());
    println!(
        "  environment={} profile={:?} storage={:?} consistency={:?}",
        config.environment, config.profile, config.storage.backend, config.storage.consistency
    );
    println!(
        "  data_dir={} backtest_summaries={} network_accessed=false orders_sent=false",
        data_dir.display(),
        summaries.len()
    );
    for worker in config.workers.iter().filter(|worker| worker.enabled) {
        println!(
            "  worker id={} role={:?} account={} venue={} configured",
            worker.id,
            worker.role,
            worker.account_id.as_deref().unwrap_or("-"),
            worker.venue_id.as_deref().unwrap_or("-")
        );
    }
    if let Some(summary) = latest_summary {
        println!(
            "[Latest Backtest] strategy={} instrument={} fills={} return_bps={} max_drawdown_bps={} result_hash={}",
            summary
                .get("strategy_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-"),
            summary
                .get("instrument")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-"),
            summary
                .get("fills")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            summary
                .pointer("/metrics/return_bps")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
            summary
                .pointer("/metrics/max_drawdown_bps")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            summary
                .get("result_hash")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-")
        );
    } else {
        println!("[Latest Backtest] 暂无已保存回测摘要");
    }
    Ok(())
}

pub(crate) fn collect_doctor_report(path: &Path) -> Result<serde_json::Value, String> {
    let config = read_runtime_config(path)?;
    let fingerprint = config.fingerprint()?;
    let mut checks = Vec::new();
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    checks.push(serde_json::json!({
        "name": "config",
        "status": "pass",
        "message": "配置解析与领域校验通过"
    }));
    checks.push(serde_json::json!({
        "name": "fingerprint",
        "status": "pass",
        "message": format!("配置指纹={fingerprint}")
    }));

    let (reference_failures, reference_warnings) = validate_runtime_references(path, &config);
    for warning in reference_warnings {
        checks.push(serde_json::json!({
            "name": "runtime_reference",
            "status": "warn",
            "message": warning.clone()
        }));
        warnings.push(warning);
    }
    for failure in reference_failures {
        checks.push(serde_json::json!({
            "name": "runtime_reference",
            "status": "fail",
            "message": failure.clone()
        }));
        failures.push(failure);
    }

    for worker in config.workers.iter().filter(|worker| {
        worker.enabled
            && matches!(
                worker.role,
                WorkerRole::UserStream
                    | WorkerRole::Execution
                    | WorkerRole::SpreadRecovery
                    | WorkerRole::Reconciler
            )
            && worker.endpoint.is_some()
            && worker
                .endpoint
                .as_deref()
                .is_some_and(|endpoint| !endpoint.contains("://"))
    }) {
        match worker_credentials_ready(path, worker) {
            Ok(true) => checks.push(serde_json::json!({
                "name": format!("worker[{}].credentials", worker.id),
                "status": "pass",
                "message": "CCXT 配置中的凭据环境变量可用"
            })),
            Ok(false) => {
                let message = format!(
                    "worker {} 的 CCXT 配置未提供可用 credential_env；当前仅能运行公共能力",
                    worker.id
                );
                checks.push(serde_json::json!({
                    "name": format!("worker[{}].credentials", worker.id),
                    "status": "warn",
                    "message": message
                }));
                warnings.push(message);
            }
            Err(error) => {
                checks.push(serde_json::json!({
                    "name": format!("worker[{}].credentials", worker.id),
                    "status": "fail",
                    "message": error
                }));
                failures.push(error);
            }
        }
    }

    check_storage_data_dir(
        path,
        &config.storage.data_dir,
        &mut checks,
        &mut warnings,
        &mut failures,
    );

    match RuntimeSupervisor::new(config.clone()) {
        Ok(supervisor) => {
            let health = supervisor
                .health()
                .lock()
                .map_err(|_| "运行时健康锁已中毒".to_string())?
                .snapshot(0, config.shutdown_timeout_ms);
            checks.push(serde_json::json!({
                "name": "runtime_topology",
                "status": "pass",
                "message": format!("overall={:?}", health.overall)
            }));
        }
        Err(error) => {
            let message = format!("运行拓扑构建失败: {error}");
            checks.push(serde_json::json!({
                "name": "runtime_topology",
                "status": "fail",
                "message": message
            }));
            failures.push(message);
        }
    }

    Ok(serde_json::json!({
        "schema_version": 1,
        "runtime_path": path.display().to_string(),
        "environment": config.environment,
        "profile": config.profile,
        "config_fingerprint": fingerprint,
        "ok": failures.is_empty(),
        "checks": checks,
        "warnings": warnings,
        "failures": failures,
        "network_accessed": false,
        "orders_sent": false
    }))
}

pub(crate) fn run_doctor(path: &Path, as_json: bool) -> Result<(), String> {
    let report = collect_doctor_report(path)?;
    let failures = report
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let warnings = report
        .get("warnings")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let ok = report
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码 doctor JSON 失败: {error}"))?
        );
    } else {
        println!("[Doctor] 检查配置、路径、运行拓扑和策略输入");
        if let Some(checks) = report.get("checks").and_then(serde_json::Value::as_array) {
            for check in checks {
                let status = check
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_ascii_uppercase();
                let name = check
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("check");
                let message = check
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                println!("[{status}] {name}: {message}");
            }
            if ok {
                println!(
                    "[Doctor] 通过：{} 个警告；未连接交易所、未发送订单",
                    warnings
                );
            } else if let Some(items) = report.get("failures").and_then(serde_json::Value::as_array)
            {
                for failure in items.iter().filter_map(serde_json::Value::as_str) {
                    eprintln!("[FAIL] {failure}");
                }
            }
        }
    }

    if ok {
        Ok(())
    } else {
        Err(format!("Doctor 发现 {failures} 个必须修复的问题"))
    }
}
