//! 配置类命令：项目初始化、配置解释/指纹/发布锁与样例资产复制。

use super::*;

fn copy_init_asset(root: &Path, file_name: &str, force: bool) -> Result<PathBuf, String> {
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

fn init_profile_template(
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

fn normalize_init_template_paths(value: &mut serde_json::Value, assets: &mut BTreeSet<String>) {
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

fn init_readme(output: &Path, strategy: Option<&str>, profile: &str) -> String {
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
        resolve_runtime_relative_path(path, &config.storage.data_dir).display(),
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

pub(crate) fn init_command(argv: &[String]) {
    let arguments = argv.iter().skip(2).cloned().collect::<Vec<_>>();
    let force = arguments.iter().any(|argument| argument == "--force");
    let mut positional = Vec::new();
    let mut strategy_name = None;
    let mut profile = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--strategy" {
            index += 1;
            strategy_name = arguments.get(index).cloned();
            if strategy_name.is_none() {
                eprintln!("init --strategy 缺少策略名称");
                std::process::exit(2);
            }
        } else if let Some(value) = argument.strip_prefix("--strategy=") {
            if value.is_empty() {
                eprintln!("init --strategy= 缺少策略名称");
                std::process::exit(2);
            }
            strategy_name = Some(value.to_string());
        } else if argument == "--profile" {
            index += 1;
            profile = arguments.get(index).cloned();
            if profile.is_none() {
                eprintln!("init --profile 缺少场景名称");
                std::process::exit(2);
            }
        } else if let Some(value) = argument.strip_prefix("--profile=") {
            if value.is_empty() {
                eprintln!("init --profile= 缺少场景名称");
                std::process::exit(2);
            }
            profile = Some(value.to_string());
        } else if !argument.starts_with('-') {
            positional.push(argument.clone());
        }
        index += 1;
    }
    let output = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("qianxing.runtime.json"));
    if let Err(error) =
        run_init_with_profile(&output, force, strategy_name.as_deref(), profile.as_deref())
    {
        eprintln!("初始化失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn config_command(argv: &[String]) {
    let action = argv.get(2).cloned().unwrap_or_else(|| "validate".into());
    let path = argv
        .get(3)
        .cloned()
        .map(PathBuf::from)
        .unwrap_or_else(default_runtime_path);
    let result = match action.as_str() {
        "explain" => run_config_explain(&path, argv.iter().any(|argument| argument == "--json")),
        "validate" => match read_runtime_config(&path) {
            Ok(config) => {
                let (failures, warnings) = validate_runtime_references(&path, &config);
                for warning in warnings {
                    println!("[WARN] {warning}");
                }
                if failures.is_empty() {
                    println!("[PASS] config validate 通过: {}", path.display());
                    Ok(())
                } else {
                    for failure in &failures {
                        eprintln!("[FAIL] {failure}");
                    }
                    Err(format!("配置引用校验失败，共 {} 项", failures.len()))
                }
            }
            Err(error) => Err(error),
        },
        "fingerprint" => run_config_fingerprint(&path),
        "lock" => {
            let output = argv
                .get(4)
                .cloned()
                .filter(|value| !value.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    let stem = path
                        .file_stem()
                        .and_then(|value| value.to_str())
                        .unwrap_or("qianxing.runtime");
                    path.with_file_name(format!("{stem}.locked.json"))
                });
            run_config_lock(
                &path,
                &output,
                argv.iter().any(|argument| argument == "--force"),
            )
        }
        _ => Err("config 仅支持 explain、validate、fingerprint 或 lock".into()),
    };
    if let Err(error) = result {
        eprintln!("配置命令失败: {error}");
        std::process::exit(2);
    }
}
