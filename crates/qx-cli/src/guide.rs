use crate::guide_args::{GuideArgs, GuideAudience};

pub(crate) fn run(args: GuideArgs) {
    match args.audience {
        None => print_index(),
        Some(GuideAudience::Researcher) => print_researcher(),
        Some(GuideAudience::StrategyAuthor) => print_strategy_author(),
        Some(GuideAudience::Operator) => print_operator(),
        Some(GuideAudience::Integrator) => print_integrator(),
    }
}

fn print_index() {
    println!(
        "上手指南 / Getting started\n\
         研究者：qx-cli guide researcher\n\
         策略作者：qx-cli guide strategy-author\n\
         运维：qx-cli guide operator\n\
         集成：qx-cli guide integrator\n\n\
         首次复算报告：qx-cli quickstart ./qianxing-demo\n\
         该入口依次建项目、体检、回测并生成可读报告；只运行本地样例，不连接交易所。"
    );
}

fn print_researcher() {
    println!(
        "研究者 / Researcher\n\
         1. 运行 `qx-cli quickstart ./qianxing-demo`，生成项目、运行体检、执行样例回测并输出报告。\n\
         2. 阅读命令输出中的报告路径，核对摘要、权益曲线与成交明细。\n\
         3. 用 `qx-cli report <runtime.json> --json` 读取机器可用的摘要。\n\
         4. 用 `qx-cli verify <BacktestOutcome.json>` 复核回测产物（路径取自 app backtest 输出）。\n\
         5. 保存运行目录与输入文件；相同输入和版本的 result_hash 用于复算对照。\n\n\
         术语 / Terms：Bar（周期行情柱），回测 / backtest，结果指纹 / result_hash。"
    );
}

fn print_strategy_author() {
    println!(
        "策略作者 / Strategy author\n\
         1. 运行 `qx-cli builtin-strategies`，查看策略支持的入口和生效参数。\n\
         2. 用 `qx-cli strategy init sma_cross ./runtime.json` 创建可回测配置与样例数据。\n\
         3. 按初始化输出中的 BarFrame 路径运行 `qx-cli strategy backtest ./runtime.json <bar-frame.json>`。\n\
         4. 用 `qx-cli report ./runtime.json --json` 查看摘要与 result_hash。\n\
         5. 修改策略配置后重跑，并比较成交、收益、回撤和 result_hash。\n\n\
         术语 / Terms：信号 / signal，撮合 / matching，成交模型 / fill model。"
    );
}

fn print_operator() {
    println!(
        "运维 / Operator\n\
         1. 运行 `qx-cli config validate <runtime.json>` 检查配置和本地文件引用。\n\
         2. 运行 `qx-cli doctor <runtime.json> --json` 检查运行条件；这不会启动 worker。\n\
         3. 运行 `qx-cli runtime-check <runtime.json> --json` 查看运行时健康与配置指纹。\n\
         4. 实盘启动前运行 `qx-cli live-check <production.runtime.json>`；这是静态检查，不发送订单。\n\
         5. 确认部署参数后再运行 `qx-cli serve <runtime.json>`，并按部署文档配置 TLS 与权限。\n\n\
         术语 / Terms：运行时 / runtime，健康检查 / health check，最小权限 / least privilege。"
    );
}

fn print_integrator() {
    println!(
        "集成者 / Integrator\n\
         1. 运行 `qx-cli config explain <runtime.json> --json` 获取脱敏后的有效配置。\n\
         2. 运行 `qx-cli plan schema-registry maturity/schema-registry.json --json` 查看登记的契约版本。\n\
         3. 用 `qx-cli app validate-dataset <DatasetSpec.json>` 检查输入数据集。\n\
         4. 用 `qx-cli app backtest <BacktestSpec.json>` 运行应用层回测并读取 stdout 的产物路径。\n\
         5. 将该路径交给 `qx-cli app verify <BacktestOutcome.json>`；HTTP/SDK 消费者可查部署 API 文档。\n\n\
         术语 / Terms：应用用例 / use case，数据集清单 / DatasetManifest，契约矩阵 / contract matrix。"
    );
}
