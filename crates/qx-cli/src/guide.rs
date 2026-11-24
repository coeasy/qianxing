use crate::guide_args::{GuideArgs, GuideAudience};
use serde::Serialize;

const RESEARCHER_STEPS: [GuideStep; 5] = [
    GuideStep {
        number: 1,
        action: "运行",
        command: Some("qx-cli quickstart ./qianxing-demo"),
        result: "生成项目、运行体检、执行样例回测并输出报告",
    },
    GuideStep {
        number: 2,
        action: "阅读命令输出中的报告路径，核对摘要、权益曲线与成交明细",
        command: None,
        result: "",
    },
    GuideStep {
        number: 3,
        action: "用",
        command: Some("qx-cli report <runtime.json> --json"),
        result: "读取机器可用的摘要",
    },
    GuideStep {
        number: 4,
        action: "用",
        command: Some("qx-cli verify <BacktestOutcome.json>"),
        result: "复核回测产物（路径取自 app backtest 输出）",
    },
    GuideStep {
        number: 5,
        action: "保存运行目录与输入文件",
        command: None,
        result: "相同输入和版本的 result_hash 用于复算对照",
    },
];

const STRATEGY_AUTHOR_STEPS: [GuideStep; 5] = [
    GuideStep {
        number: 1,
        action: "运行",
        command: Some("qx-cli builtin-strategies"),
        result: "查看策略支持的入口和生效参数",
    },
    GuideStep {
        number: 2,
        action: "用",
        command: Some("qx-cli strategy init sma_cross ./runtime.json"),
        result: "创建可回测配置与样例数据",
    },
    GuideStep {
        number: 3,
        action: "按初始化输出中的 BarFrame 路径运行",
        command: Some("qx-cli strategy backtest ./runtime.json <bar-frame.json>"),
        result: "",
    },
    GuideStep {
        number: 4,
        action: "用",
        command: Some("qx-cli report ./runtime.json --json"),
        result: "查看摘要与 result_hash",
    },
    GuideStep {
        number: 5,
        action: "修改策略配置后重跑",
        command: None,
        result: "比较成交、收益、回撤和 result_hash",
    },
];

const OPERATOR_STEPS: [GuideStep; 5] = [
    GuideStep {
        number: 1,
        action: "运行",
        command: Some("qx-cli config validate <runtime.json>"),
        result: "检查配置和本地文件引用",
    },
    GuideStep {
        number: 2,
        action: "运行",
        command: Some("qx-cli doctor <runtime.json> --json"),
        result: "检查运行条件；这不会启动 worker",
    },
    GuideStep {
        number: 3,
        action: "运行",
        command: Some("qx-cli runtime-check <runtime.json> --json"),
        result: "查看运行时健康与配置指纹",
    },
    GuideStep {
        number: 4,
        action: "实盘启动前运行",
        command: Some("qx-cli live-check <production.runtime.json>"),
        result: "这是静态检查，不发送订单",
    },
    GuideStep {
        number: 5,
        action: "确认部署参数后再运行",
        command: Some("qx-cli serve <runtime.json>"),
        result: "并按部署文档配置 TLS 与权限",
    },
];

const INTEGRATOR_STEPS: [GuideStep; 5] = [
    GuideStep {
        number: 1,
        action: "运行",
        command: Some("qx-cli config explain <runtime.json> --json"),
        result: "获取脱敏后的有效配置",
    },
    GuideStep {
        number: 2,
        action: "运行",
        command: Some("qx-cli plan schema-registry maturity/schema-registry.json --json"),
        result: "查看登记的契约版本",
    },
    GuideStep {
        number: 3,
        action: "用",
        command: Some("qx-cli app validate-dataset <DatasetSpec.json>"),
        result: "检查输入数据集",
    },
    GuideStep {
        number: 4,
        action: "用",
        command: Some("qx-cli app backtest <BacktestSpec.json>"),
        result: "运行应用层回测并读取 stdout 的产物路径",
    },
    GuideStep {
        number: 5,
        action: "将该路径交给",
        command: Some("qx-cli app verify <BacktestOutcome.json>"),
        result: "HTTP/SDK 消费者可查部署 API 文档",
    },
];

const RESEARCHER_TERMS: [GuideTerm; 3] = [
    GuideTerm {
        term: "Bar",
        translation: "周期行情柱",
    },
    GuideTerm {
        term: "回测",
        translation: "backtest",
    },
    GuideTerm {
        term: "结果指纹",
        translation: "result_hash",
    },
];

const STRATEGY_AUTHOR_TERMS: [GuideTerm; 3] = [
    GuideTerm {
        term: "信号",
        translation: "signal",
    },
    GuideTerm {
        term: "撮合",
        translation: "matching",
    },
    GuideTerm {
        term: "成交模型",
        translation: "fill model",
    },
];

const OPERATOR_TERMS: [GuideTerm; 3] = [
    GuideTerm {
        term: "运行时",
        translation: "runtime",
    },
    GuideTerm {
        term: "健康检查",
        translation: "health check",
    },
    GuideTerm {
        term: "最小权限",
        translation: "least privilege",
    },
];

const INTEGRATOR_TERMS: [GuideTerm; 3] = [
    GuideTerm {
        term: "应用用例",
        translation: "use case",
    },
    GuideTerm {
        term: "数据集清单",
        translation: "DatasetManifest",
    },
    GuideTerm {
        term: "契约矩阵",
        translation: "contract matrix",
    },
];

#[derive(Clone, Copy, Serialize)]
struct GuideStep {
    number: u8,
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'static str>,
    result: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct GuideTerm {
    term: &'static str,
    translation: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct AudienceGuide {
    id: &'static str,
    title_zh: &'static str,
    title_en: &'static str,
    goal: &'static str,
    steps: &'static [GuideStep],
    terms: &'static [GuideTerm],
}

#[derive(Serialize)]
struct GuideDocument {
    schema_version: u32,
    audiences: Vec<AudienceGuide>,
}

pub(crate) fn run(args: GuideArgs) {
    if args.json {
        print_json(args.audience);
        return;
    }

    match args.audience {
        None => print_index(),
        Some(audience) => print_audience(audience_guide(audience)),
    }
}

fn print_json(selected: Option<GuideAudience>) {
    let audiences = match selected {
        Some(audience) => vec![audience_guide(audience)],
        None => all_audience_guides(),
    };
    let document = GuideDocument {
        schema_version: 1,
        audiences,
    };
    match serde_json::to_string_pretty(&document) {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("[guide · 序列化 · 无法生成 JSON 指南 · 检查静态指南定义 · {error}]");
            std::process::exit(2);
        }
    }
}

fn all_audience_guides() -> Vec<AudienceGuide> {
    [
        GuideAudience::Researcher,
        GuideAudience::StrategyAuthor,
        GuideAudience::Operator,
        GuideAudience::Integrator,
    ]
    .into_iter()
    .map(audience_guide)
    .collect()
}

fn audience_guide(audience: GuideAudience) -> AudienceGuide {
    let (id, title_zh, title_en, goal, steps, terms) = match audience {
        GuideAudience::Researcher => (
            "researcher",
            "研究者",
            "Researcher",
            "从本地示例回测得到可复核报告",
            &RESEARCHER_STEPS[..],
            &RESEARCHER_TERMS[..],
        ),
        GuideAudience::StrategyAuthor => (
            "strategy-author",
            "策略作者",
            "Strategy author",
            "创建、运行并比较策略配置",
            &STRATEGY_AUTHOR_STEPS[..],
            &STRATEGY_AUTHOR_TERMS[..],
        ),
        GuideAudience::Operator => (
            "operator",
            "运维",
            "Operator",
            "安全检查配置并运行服务",
            &OPERATOR_STEPS[..],
            &OPERATOR_TERMS[..],
        ),
        GuideAudience::Integrator => (
            "integrator",
            "集成者",
            "Integrator",
            "通过 CLI 契约接入数据集与回测用例",
            &INTEGRATOR_STEPS[..],
            &INTEGRATOR_TERMS[..],
        ),
    };

    AudienceGuide {
        id,
        title_zh,
        title_en,
        goal,
        steps,
        terms,
    }
}

fn print_index() {
    println!(
        "上手指南 / Getting started\n\
         研究者：qx-cli guide researcher\n\
         策略作者：qx-cli guide strategy-author\n\
         运维：qx-cli guide operator\n\
         集成：qx-cli guide integrator\n\n\
         机器可读指南：qx-cli guide --json [researcher|strategy-author|operator|integrator]\n\n\
         首次复算报告：qx-cli quickstart ./qianxing-demo\n\
         该入口依次建项目、体检、回测并生成可读报告；只运行本地样例，不连接交易所。"
    );
}

fn print_audience(guide: AudienceGuide) {
    println!(
        "{} / {}\n目标 / Goal：{}",
        guide.title_zh, guide.title_en, guide.goal
    );
    for step in guide.steps {
        print!("{}. {}", step.number, step.action);
        if let Some(command) = step.command {
            print!(" `{command}`");
        }
        if !step.result.is_empty() {
            print!("，{}", step.result);
        }
        println!("。");
    }
    print!("\n术语 / Terms：");
    println!(
        "{}",
        guide
            .terms
            .iter()
            .map(|term| format!("{} / {}", term.term, term.translation))
            .collect::<Vec<_>>()
            .join("，")
    );
}
