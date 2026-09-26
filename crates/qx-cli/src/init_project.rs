//! 项目初始化命令面：`init` / `init --profile` / `strategy init`。
//!
//! 只负责挑模板、复制样例资产、生成 README 与首屏下一步，不改变运行时语义；
//! 真正读取运行时配置和回测的入口仍复用 crate 根上的同一批辅助函数。

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

/// `init --profile` 的可用值。help 的 `<...>` 表、两处错误文案和实际接受的集合都由这一份来
/// （V11 S5）：`builtin` 此前只在 match 里存在，帮助和"未知 profile"提示都不提它，
/// 用户被告知的取值集合里恰好少了那个真正能用的取值。
pub(crate) const INIT_PROFILES: [&str; 7] = [
    "base",
    "builtin",
    "paper",
    "ccxt",
    "ashare",
    "multi-venue",
    "backtest",
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
            "未知 init profile={selected}；可用值：{}",
            INIT_PROFILES.join("、")
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

/// init 生成的项目里那条回测命令：只承认"文件真的被复制进项目、入口真的读得动这份绑定"的组合。
///
/// 以前 7 个 profile 打印同一行 `backtest <runtime> qianxing.bar-frame.example.json
/// qianxing.binance.spot.spec.json`，而 `base`/`paper` 没绑策略（必报"跨语言回测必须配置
/// strategy.python_module…"），`ccxt`/`multi-venue` 连 BarFrame 都没复制（必报"找不到文件"）。
/// 首屏命令自己先失败等于没有文档，所以宁可不印也不能印一条注定报错的命令。
/// 印出来的行情与规格路径一律带上项目目录：裸文件名等于把"先 cd 到项目目录"这条从未写在
/// 屏幕上的前提，变成一次必然的"找不到文件"。
fn init_backtest_step(
    project_root: &Path,
    output: &Path,
    config: &RuntimeConfig,
) -> Option<String> {
    let copied = |name: &str| project_root.join(name).is_file().then(|| name.to_owned());
    let declared = config
        .strategy
        .bars_snapshot_path
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .and_then(copied);
    let frame = declared
        .or_else(|| copied("qianxing.bar-frame.example.json"))
        .or_else(|| copied("qianxing.ashare.bar-frame.example.json"))?;
    let spec = if frame.starts_with("qianxing.ashare.") {
        copied("qianxing.ashare.spot.spec.json")
    } else {
        copied("qianxing.binance.spot.spec.json")
    };
    let spec_arg = spec
        .map(|name| format!(" {}", project_root.join(name).display()))
        .unwrap_or_default();
    let frame = project_root.join(&frame).display().to_string();
    let strategy = &config.strategy;
    // 绑定了策略就让它自己的配置跑；否则退回不读运行时的内置策略入口。
    Some(
        if strategy.builtin_strategy.is_some()
            || strategy.python_module.is_some()
            || strategy.external_executable.is_some()
            || strategy.c_abi_library.is_some()
        {
            format!("qianxing backtest {} {frame}{spec_arg}", output.display())
        } else {
            format!("qianxing backtest builtin sma_cross {frame}{spec_arg}")
        },
    )
}

pub(crate) fn init_readme(
    output: &Path,
    strategy: Option<&str>,
    profile: &str,
    backtest_step: Option<&str>,
) -> String {
    let strategy_line = strategy
        .map(|name| format!("已绑定内置策略：`{name}`。"))
        .unwrap_or_else(|| {
            "当前配置为基础运行时，可通过 `qianxing init --strategy macd --force` 绑定内置策略。"
                .into()
        });
    let backtest_line = backtest_step.unwrap_or(
        "本 profile 未附带 BarFrame，暂无本地回测命令；自备行情文件后走 `qianxing strategy init`。",
    );
    format!(
        "# Qianxing 本地项目\n\n运行时配置：`{}`。profile=`{}`。{}\n\n## 推荐流程\n\n```text\nqianxing doctor {}\nqianxing config explain {}\nqianxing config validate {}\n{}\nqianxing paper-check {}\n```\n\n初始化生成的样例文件只用于本地回测和 Paper 验收，不包含交易密钥，也不会自动发送真实订单。CCXT/多交易所 profile 只生成公共配置和凭据引用，必须自行配置环境变量后再做 sandbox 验收。\n",
        output.display(),
        profile,
        strategy_line,
        output.display(),
        output.display(),
        output.display(),
        backtest_line,
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
    let config = read_runtime_config(output)?;
    let backtest_step = init_backtest_step(project_root, output, &config);
    if force || !readme_path.exists() {
        std::fs::write(
            &readme_path,
            init_readme(
                output,
                strategy_name,
                profile_name,
                backtest_step.as_deref(),
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
    println!(
        "下一步：qianxing doctor {}{}",
        output.display(),
        backtest_step
            .map(|step| format!("；{step}"))
            .unwrap_or_default()
    );
    Ok(())
}

/// `init --strategy` 与 `strategy init` 只接受单标的内置策略。
///
/// 四个双腿套利 kind 要第二条 BarFrame 和 `reference_instrument`，而这两个入口只生成一份
/// 行情夹具；放过去只会让使用者随后在回测里撞上"内置策略参数非法"这种看不出真正原因的
/// 报错，所以在这里点名真正的缺口。
fn single_leg_builtin_strategy(name: &str) -> Result<BuiltinStrategyKind, String> {
    let kind = BuiltinStrategyKind::parse(name)?;
    if kind.needs_reference_leg() {
        return Err(format!(
            "内置策略 {} 是双腿套利，需要第二条 BarFrame 与 reference_instrument：\
             初始化只生成单标的项目，请改用 `qianxing backtest multi-builtin {}` 配两份行情\
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
    // 与 `init` 共用同一套归一：`deploy/…` 一律改成配置文件同目录的裸文件名并复制过去。
    // 少了这一步，`strategy init` 打印的首条命令只在仓库根可用——运行时相对路径按配置
    // 文件所在目录解析，`deploy/qianxing.bar-frame.example.json` 会拼成
    // `<配置目录>/deploy/…`，于是刚生成的配置自己读不到自己的行情夹具。
    let mut copied_assets = BTreeSet::new();
    normalize_init_template_paths(&mut document, &mut copied_assets);
    let bars = document
        .pointer("/strategy/bars_snapshot_path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(bars.as_str())
        .to_string();
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
    let project_root = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    for asset in &copied_assets {
        copy_init_asset(project_root, asset, force)?;
    }
    println!(
        "[Strategy · Init] strategy={} output={} bars={} assets={}",
        kind.name(),
        output.display(),
        bars,
        copied_assets.len()
    );
    // 打印绝对路径：命令行里的裸文件名等于要求使用者先 cd 到配置所在目录，而首屏只给命令不给目录。
    println!(
        "下一步：qianxing strategy backtest {} {}",
        output.display(),
        project_root.join(&bars).display()
    );
    Ok(())
}
