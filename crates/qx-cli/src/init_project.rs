//! 项目初始化命令面：`init` / `init --profile` / `strategy init`。
//!
//! 只负责挑模板、复制样例资产、生成 README 与首屏下一步，不改变运行时语义；
//! 真正读取运行时配置和回测的入口仍复用 crate 根上的同一批辅助函数。

use crate::cli_args;
use crate::deploy_lookup::{
    current_deploy_roots, deploy_miss_note, locate_deploy_file, DEPLOY_DIR_NAME,
};
use crate::init_guidance;
use crate::path_resolution::anchor_init_data_dir;
use crate::project_manifest::write_init_project_manifest;
use crate::runtime_wiring::read_runtime_config;
use qx_runtime::{RuntimeConfig, DEFAULT_EVENT_LOG_SEGMENT_EVENTS};
use qx_strategy::BuiltinStrategyKind;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
/// 默认示例配置的取数入口（链路入口符号，定义点由架构门禁点名本文件）。
///
/// 查找链本身在 `deploy_lookup.rs`；全缺时回落到「当前目录的 `deploy/`」这一写法，
/// 让错误文案仍然指向使用者刚敲下的那条相对路径。
pub(crate) fn repository_deploy_path(file_name: &str) -> PathBuf {
    locate_deploy_file(&current_deploy_roots(), file_name)
        .unwrap_or_else(|| PathBuf::from(DEPLOY_DIR_NAME).join(file_name))
}

pub(crate) fn copy_init_asset(
    root: &Path,
    file_name: &str,
    force: bool,
) -> Result<PathBuf, String> {
    let source = repository_deploy_path(file_name);
    if !source.is_file() {
        return Err(format!(
            "找不到初始化样例文件: {}\n{}",
            source.display(),
            deploy_miss_note(file_name, &current_deploy_roots())
        ));
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

/// `init --profile` 的可用值。help 的 `<...>` 表、两处错误文案和实际接受的集合都由这一份来
/// （V11 S5）：`builtin` 此前只在 match 里存在，帮助和"未知 profile"提示都不提它，
/// 用户被告知的取值集合里恰好少了那个真正能用的取值。
pub(crate) const INIT_PROFILES: [&str; 10] = [
    "base",
    "builtin",
    "paper",
    "ccxt",
    "ashare",
    "multi-venue",
    "backtest",
    "research-local",
    "crypto-paper",
    "crypto-sandbox-acceptance",
];

pub(crate) fn init_profile_template(
    profile: Option<&str>,
    strategy_name: Option<&str>,
) -> Result<(&'static str, Vec<&'static str>, &'static str), String> {
    let selected: &'static str = match profile {
        None => {
            if strategy_name.is_some() {
                "builtin"
            } else {
                "base"
            }
        }
        Some(value) => INIT_PROFILES
            .into_iter()
            .find(|candidate| *candidate == value)
            .ok_or_else(|| {
                format!(
                    "未知 init profile={value}；可用值：{}",
                    INIT_PROFILES.join("、")
                )
            })?,
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
        "grafana/qianxing-overview.json",
    ];
    let (template_name, mut assets, profile_name) = match selected {
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
                "qianxing.bar-frame.okx.example.json",
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
                "qianxing.ccxt.binance.usdm.example.json",
                "qianxing.ccxt.binance.spot.example.json",
                "qianxing.ccxt.okx.swap.example.json",
                "qianxing.binance.spot.spec.json",
                "qianxing.ccxt.okx.perpetual.spec.json",
                "qianxing.crypto.multi-venue-perpetual-spec.example.json",
                "qianxing.scheduler.jobs.example.json",
                "data/qianxing-multi-venue-arbitrage/btc-usdt-okx.bar-frame.json",
                "data/qianxing-multi-venue-arbitrage/btc-usdt-binance.bar-frame.json",
            ],
            selected,
        )),
        "backtest" => Ok((
            "qianxing.runtime.strategy-backtest.example.json",
            vec!["qianxing.bar-frame.example.json"],
            selected,
        )),
        "research-local" => Ok((
            "qianxing.runtime.strategy-backtest.example.json",
            vec!["qianxing.bar-frame.example.json"],
            selected,
        )),
        "crypto-paper" => Ok((
            "qianxing.runtime.paper-strategy.example.json",
            common,
            selected,
        )),
        "crypto-sandbox-acceptance" => Ok((
            "qianxing.runtime.ccxt.example.json",
            vec![
                "qianxing.ccxt.exchange.example.json",
                "qianxing.ccxt.okx.perpetual.spec.json",
                "qianxing.bar-frame.okx.example.json",
                "qianxing.scheduler.jobs.example.json",
            ],
            selected,
        )),
        _ => Err(format!(
            "未知 init profile={selected}；可用值：{}",
            INIT_PROFILES.join("、")
        )),
    }?;
    if !assets.contains(&"grafana/qianxing-overview.json") {
        assets.push("grafana/qianxing-overview.json");
    }
    Ok((template_name, assets, profile_name))
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

pub(crate) fn run_init_with_profile(
    output: &Path,
    force: bool,
    strategy_name: Option<&str>,
    profile: Option<&str>,
    emit_next_step: bool,
) -> Result<(), String> {
    run_init_with_profile_selection(output, force, strategy_name, profile, emit_next_step, None)
}

pub(crate) fn run_init_with_ccxt_selection(
    output: &Path,
    force: bool,
    exchange: &str,
    instrument: &str,
) -> Result<(), String> {
    run_init_with_profile_selection(
        output,
        force,
        None,
        Some("ccxt"),
        false,
        Some((exchange, instrument)),
    )
}

fn run_init_with_profile_selection(
    output: &Path,
    force: bool,
    strategy_name: Option<&str>,
    profile: Option<&str>,
    emit_next_step: bool,
    ccxt_selection: Option<(&str, &str)>,
) -> Result<(), String> {
    let (template_name, mut profile_assets, profile_name) =
        init_profile_template(profile, strategy_name)?;
    if ccxt_selection.is_some() && profile_name != "ccxt" {
        return Err("交易所选择只能用于 ccxt 初始化配置".into());
    }
    if ccxt_selection.is_some_and(|(exchange, _)| exchange != "okx") {
        profile_assets.retain(|asset| *asset != "qianxing.ccxt.okx.perpetual.spec.json");
    }
    let template = repository_deploy_path(template_name);
    if !template.exists() {
        return Err(format!(
            "找不到运行时模板: {}\n{}",
            template.display(),
            deploy_miss_note(template_name, &current_deploy_roots())
        ));
    }
    if output.exists() && !force {
        return Err(format!(
            "目标配置已存在: {}；如确认覆盖，请显式添加 --force",
            output.display()
        ));
    }
    let manifest_root = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let project_manifest_path = manifest_root.join("qianxing.project.json");
    if project_manifest_path.exists() && !force {
        return Err(format!(
            "项目清单已存在: {}；如确认覆盖，请显式添加 --force",
            project_manifest_path.display()
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
    if let Some(storage) = document
        .get_mut("storage")
        .and_then(serde_json::Value::as_object_mut)
    {
        if storage.get("backend").and_then(serde_json::Value::as_str) == Some("files") {
            storage.insert(
                "event_log_segment_events".into(),
                serde_json::Value::from(DEFAULT_EVENT_LOG_SEGMENT_EVENTS),
            );
        }
    }
    if matches!(
        profile_name,
        "ccxt" | "multi-venue" | "crypto-sandbox-acceptance"
    ) {
        // A fresh scaffold cannot assert a supplier-signed role catalog or venue credentials.
        // Leave private execution unarmed until the operator installs those prerequisites.
        if let Some(workers) = document
            .get_mut("workers")
            .and_then(serde_json::Value::as_array_mut)
        {
            for worker in workers {
                if worker.get("role").and_then(serde_json::Value::as_str) == Some("execution") {
                    if let Some(fields) = worker.as_object_mut() {
                        fields.insert("enabled".into(), serde_json::Value::Bool(false));
                    }
                }
            }
        }
        if let Some(strategies) = document
            .get_mut("strategies")
            .and_then(serde_json::Value::as_array_mut)
        {
            for strategy in strategies {
                if let Some(fields) = strategy.as_object_mut() {
                    fields.insert("live_enabled".into(), serde_json::Value::Bool(false));
                    if !matches!(profile_name, "multi-venue") {
                        fields.insert(
                            "bars_snapshot_path".into(),
                            serde_json::Value::String("qianxing.bar-frame.okx.example.json".into()),
                        );
                    }
                }
            }
        }
        if let Some(strategy) = document
            .get_mut("strategy")
            .and_then(serde_json::Value::as_object_mut)
        {
            strategy.insert("live_enabled".into(), serde_json::Value::Bool(false));
            if !matches!(profile_name, "multi-venue") {
                strategy.insert(
                    "bars_snapshot_path".into(),
                    serde_json::Value::String("qianxing.bar-frame.okx.example.json".into()),
                );
            }
        }
    }
    if let Some((exchange, instrument)) = ccxt_selection {
        align_ccxt_profile_selection(&mut document, exchange, instrument)?;
        if exchange != "okx" {
            init_assets.retain(|asset| asset != "qianxing.ccxt.okx.perpetual.spec.json");
        }
    }
    let project_root = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if let Some(strategy_name) = strategy_name {
        let kind = single_leg_builtin_strategy(strategy_name)?;
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
        // `init --strategy` ships a BarFrame fixture, but the copied legacy DatasetBundle
        // template has no byte-bound DatasetSnapshotV1. Bind this generated local project
        // to the actual BarFrame; users can attach a verified bundle through dataset-bundle.
        strategy.remove("dataset_bundle_path");
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
    anchor_init_data_dir(&mut document, project_root);
    let payload = serde_json::to_string_pretty(&document)
        .map_err(|error| format!("编码初始化运行时配置失败: {error}"))?;
    RuntimeConfig::from_json(&payload)?;
    std::fs::write(output, payload)
        .map_err(|error| format!("写入运行时配置失败 {}: {error}", output.display()))?;
    for asset in &init_assets {
        copy_init_asset(project_root, asset, force)?;
    }
    if let Some((exchange, instrument)) = ccxt_selection {
        write_ccxt_profile_selection_assets(project_root, exchange, instrument, force)?;
    }
    std::fs::create_dir_all(project_root.join("data"))
        .map_err(|error| format!("创建项目数据目录失败: {error}"))?;
    let readme_path = project_root.join("README.qianxing.md");
    let config = read_runtime_config(output)?;
    let project_manifest =
        write_init_project_manifest(output, project_root, profile_name, &config, force)?;
    let backtest_step = init_guidance::init_backtest_step(project_root, output, &config);
    let flow_tail = init_guidance::init_flow_tail(output, profile_name, &config);
    if force || !readme_path.exists() {
        std::fs::write(
            &readme_path,
            init_guidance::init_readme(
                output,
                strategy_name,
                profile_name,
                backtest_step.as_deref(),
                flow_tail.as_deref(),
            ),
        )
        .map_err(|error| format!("写入初始化说明失败 {}: {error}", readme_path.display()))?;
    }
    println!(
        "[初始化] 已创建 {} profile={} environment={} fingerprint={} assets={} strategy={}",
        output.display(),
        profile_name,
        config.environment,
        config.fingerprint()?,
        init_assets.len(),
        strategy_name.unwrap_or("none")
    );
    if let Some((path, fingerprint)) = project_manifest {
        println!(
            "[ProjectManifest] path={} fingerprint={fingerprint}",
            path.display()
        );
    }
    let program = cli_args::PROGRAM_NAME;
    if emit_next_step {
        println!(
            "下一步：{program} doctor {}{}",
            output.display(),
            backtest_step
                .map(|step| format!("；{step}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

fn align_ccxt_profile_selection(
    document: &mut serde_json::Value,
    exchange: &str,
    instrument: &str,
) -> Result<(), String> {
    let workers = document
        .get_mut("workers")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "CCXT 运行时模板缺少 workers 列表".to_string())?;
    for worker in workers {
        let Some(fields) = worker.as_object_mut() else {
            continue;
        };
        if fields.get("venue_id").is_some_and(|value| !value.is_null()) {
            fields.insert(
                "venue_id".into(),
                serde_json::Value::String(exchange.into()),
            );
        }
        if let Some(symbols) = fields
            .get_mut("symbols")
            .and_then(serde_json::Value::as_array_mut)
        {
            for symbol in symbols {
                if symbol.as_str().is_some_and(|value| value.ends_with(".OKX")) {
                    *symbol = serde_json::Value::String(instrument.into());
                }
            }
        }
        if fields
            .get("role")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|role| matches!(role, "execution" | "spread_recovery" | "reconciler"))
        {
            let venue = exchange.to_ascii_uppercase();
            fields.insert(
                "credential_env".into(),
                serde_json::json!({
                    "api_key": format!("QX_CCXT_{venue}_API_KEY"),
                    "secret": format!("QX_CCXT_{venue}_SECRET"),
                }),
            );
        }
        if exchange != "okx"
            && fields
                .get("role")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|role| matches!(role, "execution" | "spread_recovery"))
        {
            // There is no verified static contract specification for every venue. These workers
            // are already disabled by the CCXT scaffold; never carry an OKX spec into another venue.
            fields.remove("instrument_spec_path");
        }
    }
    if let Some(providers) = document
        .pointer_mut("/api/data_providers")
        .and_then(serde_json::Value::as_array_mut)
    {
        for provider in providers {
            if let Some(id) = provider.get_mut("provider_id") {
                *id = serde_json::Value::String(format!("ccxt-{exchange}-1m"));
            }
        }
    }
    if let Some(strategies) = document
        .get_mut("strategies")
        .and_then(serde_json::Value::as_array_mut)
    {
        for strategy in strategies {
            align_ccxt_strategy_selection(strategy, exchange, instrument);
        }
    }
    if let Some(strategy) = document.get_mut("strategy") {
        align_ccxt_strategy_selection(strategy, exchange, instrument);
    }
    Ok(())
}

fn align_ccxt_strategy_selection(
    strategy: &mut serde_json::Value,
    exchange: &str,
    instrument: &str,
) {
    let Some(fields) = strategy.as_object_mut() else {
        return;
    };
    if fields.contains_key("venue_id") {
        fields.insert(
            "venue_id".into(),
            serde_json::Value::String(exchange.into()),
        );
    }
    if fields.contains_key("instrument") {
        fields.insert(
            "instrument".into(),
            serde_json::Value::String(instrument.into()),
        );
    }
    if fields.contains_key("bars_snapshot_path") {
        fields.insert(
            "bars_snapshot_path".into(),
            serde_json::Value::String(format!("qianxing.bar-frame.{exchange}.example.json")),
        );
    }
    if fields.contains_key("live_enabled") {
        fields.insert("live_enabled".into(), serde_json::Value::Bool(false));
    }
}

fn write_ccxt_profile_selection_assets(
    project_root: &Path,
    exchange: &str,
    instrument: &str,
    force: bool,
) -> Result<(), String> {
    let exchange_config_path = project_root.join("qianxing.ccxt.exchange.example.json");
    let mut exchange_config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&exchange_config_path).map_err(|error| {
            format!(
                "读取 CCXT 样例配置失败 {}: {error}",
                exchange_config_path.display()
            )
        })?)
        .map_err(|error| format!("CCXT 样例配置 JSON 无效: {error}"))?;
    exchange_config["exchange_id"] = serde_json::Value::String(exchange.into());
    exchange_config["default_type"] = serde_json::Value::String("swap".into());
    exchange_config["instruments"] = serde_json::json!([instrument]);
    if let Some(credentials) = exchange_config
        .get_mut("credential_env")
        .and_then(serde_json::Value::as_object_mut)
    {
        let venue = exchange.to_ascii_uppercase();
        for (field, suffix) in [
            ("api_key", "API_KEY"),
            ("secret", "SECRET"),
            ("password", "PASSWORD"),
        ] {
            if credentials.contains_key(field) {
                credentials.insert(
                    field.into(),
                    serde_json::Value::String(format!("QX_CCXT_{venue}_{suffix}")),
                );
            }
        }
    }
    let serialized = serde_json::to_vec_pretty(&exchange_config)
        .map_err(|error| format!("编码 CCXT 样例配置失败: {error}"))?;
    std::fs::write(&exchange_config_path, serialized).map_err(|error| {
        format!(
            "写入 CCXT 样例配置失败 {}: {error}",
            exchange_config_path.display()
        )
    })?;

    let source_frame = project_root.join("qianxing.bar-frame.okx.example.json");
    let selected_frame = project_root.join(format!("qianxing.bar-frame.{exchange}.example.json"));
    if exchange != "okx" && source_frame.exists() {
        if selected_frame.exists() && !force {
            return Err(format!(
                "目标行情样例已存在: {}；如确认覆盖，请显式添加 --force",
                selected_frame.display()
            ));
        }
        std::fs::rename(&source_frame, &selected_frame)
            .or_else(|error| {
                if force && selected_frame.exists() {
                    std::fs::remove_file(&selected_frame)?;
                    std::fs::rename(&source_frame, &selected_frame)
                } else {
                    Err(error)
                }
            })
            .map_err(|error| format!("调整行情样例文件名失败: {error}"))?;
    }
    let frame_path = if exchange == "okx" {
        source_frame
    } else {
        selected_frame
    };
    if frame_path.exists() {
        let mut frame: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&frame_path)
                .map_err(|error| format!("读取行情样例失败 {}: {error}", frame_path.display()))?,
        )
        .map_err(|error| format!("行情样例 JSON 无效: {error}"))?;
        frame["instrument"] = serde_json::Value::String(instrument.into());
        frame["source"] = serde_json::Value::String(format!("example-{exchange}-bars-v1"));
        let serialized = serde_json::to_vec_pretty(&frame)
            .map_err(|error| format!("编码行情样例失败: {error}"))?;
        std::fs::write(&frame_path, serialized)
            .map_err(|error| format!("写入行情样例失败 {}: {error}", frame_path.display()))?;
    }
    Ok(())
}

/// `init --strategy` 与 `strategy init` 只接受单标的内置策略。
///
/// 四个双腿套利 kind 要第二条 BarFrame 和 `reference_instrument`，而这两个入口只生成一份
/// 行情夹具；放过去只会让使用者随后在回测里撞上"内置策略参数非法"这种看不出真正原因的
/// 报错，所以在这里点名真正的缺口。
fn single_leg_builtin_strategy(name: &str) -> Result<BuiltinStrategyKind, String> {
    let kind = BuiltinStrategyKind::parse(name)?;
    let program = cli_args::PROGRAM_NAME;
    if kind.needs_reference_leg() {
        return Err(format!(
            "内置策略 {} 是双腿套利，需要第二条 BarFrame 与 reference_instrument：\
             初始化只生成单标的项目，请改用 `{program} backtest multi-builtin {}` 配两份行情\
             夹具（deploy 里的 pairs-primary/pairs-reference 示例），或选一个单标的策略。",
            kind.name(),
            kind.name()
        ));
    }
    Ok(kind)
}

pub(crate) fn run_strategy_init(
    strategy_name: &str,
    output: &Path,
    bars_path: Option<&Path>,
    force: bool,
) -> Result<(), String> {
    let kind = single_leg_builtin_strategy(strategy_name)?;
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
    // The bundled example manifest predates byte-bound DatasetSnapshotV1 and is not
    // executable evidence. A local strategy init binds bars directly; an explicit
    // DatasetBundle can be attached after publishing its snapshot.
    strategy.remove("dataset_bundle_path");
    // 与 `init` 共用同一套归一：`deploy/…` 一律改成配置文件同目录的裸文件名并复制过去。
    // 少了这一步，`strategy init` 打印的首条命令只在仓库根可用——运行时相对路径按配置
    // 文件所在目录解析，`deploy/qianxing.bar-frame.example.json` 会拼成
    // `<配置目录>/deploy/…`，于是刚生成的配置自己读不到自己的行情夹具。
    let mut copied_assets = BTreeSet::new();
    normalize_init_template_paths(&mut document, &mut copied_assets);
    let project_root = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let project_manifest_path = project_root.join("qianxing.project.json");
    if project_manifest_path.exists() && !force {
        return Err(format!(
            "项目清单已存在: {}；如确认覆盖，请显式添加 --force",
            project_manifest_path.display()
        ));
    }
    anchor_init_data_dir(&mut document, project_root);
    let bars = document
        .pointer("/strategy/bars_snapshot_path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(bars.as_str())
        .to_string();
    let result = serde_json::to_string_pretty(&document)
        .map_err(|error| format!("编码内置策略配置失败: {error}"))?;
    let config = RuntimeConfig::from_json(&result)?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建策略配置目录失败 {}: {error}", parent.display()))?;
    }
    std::fs::write(output, result)
        .map_err(|error| format!("写入策略配置失败 {}: {error}", output.display()))?;
    for asset in &copied_assets {
        copy_init_asset(project_root, asset, force)?;
    }
    if let Some((path, fingerprint)) =
        write_init_project_manifest(output, project_root, "builtin", &config, force)?
    {
        println!(
            "[ProjectManifest] path={} fingerprint={fingerprint}",
            path.display()
        );
    }
    println!(
        "[Strategy · Init] strategy={} output={} bars={} assets={}",
        kind.name(),
        output.display(),
        bars,
        copied_assets.len()
    );
    // 打印绝对路径：命令行里的裸文件名等于要求使用者先 cd 到配置所在目录，而首屏只给命令不给目录。
    let program = cli_args::PROGRAM_NAME;
    println!(
        "下一步：{program} strategy backtest {} {}",
        output.display(),
        project_root.join(&bars).display()
    );
    Ok(())
}

#[cfg(test)]
mod ccxt_selection_tests {
    use super::run_init_with_ccxt_selection;
    use serde_json::Value;
    use std::path::PathBuf;

    #[test]
    fn ccxt_quickstart_selection_aligns_runtime_and_demo_assets() {
        let unique = format!(
            "qianxing-ccxt-selection-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.task-tmp")
            .join(unique);
        std::fs::create_dir_all(&root).unwrap();
        let runtime = root.join("qianxing.runtime.json");

        run_init_with_ccxt_selection(&runtime, false, "bitget", "BTC/USDT:USDT.BITGET").unwrap();

        let config: Value = serde_json::from_slice(&std::fs::read(&runtime).unwrap()).unwrap();
        for worker in config["workers"].as_array().unwrap() {
            if !worker["venue_id"].is_null() {
                assert_eq!(worker["venue_id"], "bitget");
            }
            if let Some(symbols) = worker["symbols"].as_array() {
                assert!(symbols
                    .iter()
                    .all(|symbol| symbol == "BTC/USDT:USDT.BITGET"));
            }
        }
        assert_eq!(config["strategy"]["venue_id"], "bitget");
        assert_eq!(config["strategy"]["instrument"], "BTC/USDT:USDT.BITGET");
        assert_eq!(config["strategy"]["live_enabled"], false);
        let execution = config["workers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|worker| worker["role"] == "execution")
            .unwrap();
        assert_eq!(execution["enabled"], false);
        assert!(execution.get("instrument_spec_path").is_none());
        let reconciler = config["workers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|worker| worker["role"] == "reconciler")
            .unwrap();
        assert_eq!(
            reconciler["credential_env"]["api_key"],
            "QX_CCXT_BITGET_API_KEY"
        );
        assert_eq!(
            reconciler["credential_env"]["secret"],
            "QX_CCXT_BITGET_SECRET"
        );

        let exchange_config: Value = serde_json::from_slice(
            &std::fs::read(root.join("qianxing.ccxt.exchange.example.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(exchange_config["exchange_id"], "bitget");
        assert_eq!(exchange_config["sandbox"], true);
        assert_eq!(exchange_config["default_type"], "swap");
        assert_eq!(exchange_config["instruments"][0], "BTC/USDT:USDT.BITGET");
        assert!(root
            .join("qianxing.bar-frame.bitget.example.json")
            .is_file());
        assert!(!root.join("qianxing.bar-frame.okx.example.json").exists());
        assert!(!root.join("qianxing.ccxt.okx.perpetual.spec.json").exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn binance_quickstart_scaffold_names_credentials_without_loading_secrets() {
        let unique = format!(
            "qianxing-binance-ccxt-selection-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.task-tmp")
            .join(unique);
        std::fs::create_dir_all(&root).unwrap();
        let runtime = root.join("qianxing.runtime.json");

        run_init_with_ccxt_selection(&runtime, false, "binance", "BTC/USDT:USDT.BINANCE").unwrap();

        let config: Value = serde_json::from_slice(&std::fs::read(&runtime).unwrap()).unwrap();
        let workers = config["workers"].as_array().unwrap();
        assert!(workers
            .iter()
            .filter(|worker| !worker["venue_id"].is_null())
            .all(|worker| worker["venue_id"] == "binance"));
        let execution = workers
            .iter()
            .find(|worker| worker["role"] == "execution")
            .unwrap();
        assert_eq!(execution["enabled"], false);
        let reconciler = workers
            .iter()
            .find(|worker| worker["role"] == "reconciler")
            .unwrap();
        assert_eq!(
            reconciler["credential_env"]["api_key"],
            "QX_CCXT_BINANCE_API_KEY"
        );
        assert_eq!(
            reconciler["credential_env"]["secret"],
            "QX_CCXT_BINANCE_SECRET"
        );
        assert!(config["workers"].to_string().find("secret-value").is_none());

        let _ = std::fs::remove_dir_all(root);
    }
}
