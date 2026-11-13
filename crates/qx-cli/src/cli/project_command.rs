//! CLI adapters for project initialization, quickstart, and guided workflows.

use super::*;

pub(super) fn dispatch_init(
    output: Option<PathBuf>,
    force: bool,
    strategy: Option<String>,
    profile: Option<String>,
) {
    if strategy.as_deref() == Some("") {
        eprintln!("[qx-cli · CLI] init --strategy= 缺少策略名称");
        std::process::exit(2);
    }
    if profile.as_deref() == Some("") {
        eprintln!("[qx-cli · CLI] init --profile= 缺少场景名称");
        std::process::exit(2);
    }
    let output = output.unwrap_or_else(|| PathBuf::from("qianxing.runtime.json"));
    if let Err(error) = run_init_with_profile(
        &output,
        force,
        strategy.as_deref(),
        profile.as_deref(),
        true,
    ) {
        eprintln!("[qx-cli · CLI] 初始化失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_quickstart(
    project: Option<PathBuf>,
    html: bool,
    force: bool,
    profile: crate::cli_args::QuickstartProfile,
    strategy: Option<String>,
    days: Option<u32>,
    timeframe: Option<String>,
    instrument: Option<String>,
    exchange: Option<String>,
) {
    quickstart::run(
        project, html, force, profile, strategy, days, timeframe, instrument, exchange,
    );
}

pub(super) fn dispatch_guide(args: crate::guide_args::GuideArgs) {
    guide::run(args);
}
