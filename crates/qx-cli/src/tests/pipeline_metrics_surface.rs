//! #178 的行为判据：`LiveEventPipeline` 的六个计数在生产里必须有出口。
//!
//! 第八遍立案时这六个数每笔 `ingest`/`refresh` 都在加，可是没有任何生产代码调用 `metrics()`
//! —— 数据算出来再丢掉（改前实测 `logs/s481_*.txt`：`worker-metrics/paper-execution.prom`
//! 根本不存在）。第二十遍把量纲定成**按 worker 进程累计**并接上 `.prom` 出口，这里的判据
//! 逐条对应那条量纲：真跑一轮 paper 链看落盘、真装配一次 API 看聚合、失鲜/多对象/节律各一。

use super::*;

/// 把一份 `.prom` 正文按抓取端口径拆开：每行必须是 `<名>{<标签>} <整数>`，且不带转义行分隔。
///
/// 不写成 `contains`：第七遍实测到 `contains` 断言能把"整份正文是一行"的坏出口判成绿（#177）。
fn parse_exposition(body: &str) -> Vec<(String, String, u64)> {
    assert!(
        !body.contains(r"\n"),
        "正文里出现字面反斜杠 + n：行分隔被写成转义文本，抓取端会把整份正文读成一行:\n{body}"
    );
    body.lines()
        .map(|line| {
            let (name_and_labels, value) = line
                .rsplit_once(' ')
                .unwrap_or_else(|| panic!("样本行必须是 `<名/标签> <值>`，实际 {line:?}"));
            let (name, labels) = name_and_labels
                .split_once('{')
                .unwrap_or_else(|| panic!("worker 侧样本必须带标签，实际 {line:?}"));
            assert!(labels.ends_with('}'), "标签段没有收口，实际 {line:?}");
            (
                name.to_string(),
                labels[..labels.len() - 1].to_string(),
                value.parse::<u64>().unwrap_or_else(|error| {
                    panic!("计数样本值要是不带小数的整数，实际 {line:?}: {error}")
                }),
            )
        })
        .collect()
}

const PIPELINE_COUNTERS: [&str; 6] = [
    "qx_pipeline_ingest_attempts_total",
    "qx_pipeline_ingested_events_total",
    "qx_pipeline_deduplicated_events_total",
    "qx_pipeline_transient_retries_total",
    "qx_pipeline_refreshes_total",
    "qx_pipeline_failures_total",
];

/// 真链路落盘：行情 + 一笔带费用的成交 + 一次 `--once` paper 执行 worker 之后，
/// worker 的 `.prom` 必须带着这六个计数和它们的主人（worker / account 标签）。
#[test]
fn paper_worker_publishes_pipeline_counters_to_its_metrics_file() {
    let root = temp_cli_case_dir("pipeline-metrics-surface");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);
    let path = worker_metrics_dir(&config).join("paper-execution.prom");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("paper 执行 worker 要落盘 {}: {error}", path.display()));
    let samples = parse_exposition(&body);
    let names = samples
        .iter()
        .map(|(name, _, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            ["qx_worker_up", "qx_worker_heartbeat_timestamp_seconds"].as_slice(),
            PIPELINE_COUNTERS.as_slice(),
        ]
        .concat(),
        "正文必须是这八行（心跳 + 六个计数），实际:\n{body}"
    );
    let expected_labels = format!(
        "worker=\"paper-execution\",account=\"{}\"",
        paper_account_log()
    );
    for (_, labels, _) in &samples {
        assert_eq!(
            labels, &expected_labels,
            "每行都要指认它是哪个 worker 的哪个账户，实际正文:\n{body}"
        );
    }
    let value = |name: &str| {
        samples
            .iter()
            .find(|(sample, _, _)| sample == name)
            .map(|(_, _, value)| *value)
            .unwrap()
    };
    // `--once` 的 worker 已经退出：这一格如实回 0，不是"这个 worker 现在健康"。
    assert_eq!(value("qx_worker_up"), 0, "`--once` 之后的正文:\n{body}");
    assert!(value("qx_worker_heartbeat_timestamp_seconds") > 0);
    // 计数要按这条夹具链的真实笔数钉死（实测正文 `logs/s484_*.txt`）：`seed_paper_fill_with_fee`
    // 之后 worker 自开的 pipeline 一共做过 4 次 `ingest`、每次都入账。取相等而不是取">= 2"，
    // 是因为 `absorb` 的两种错法都可能仍留着非零的数 —— 漏并一个对象是少计，把一个对象并两次
    // 是双计，两边都不会让正文里任何一行报错。
    assert_eq!(
        value("qx_pipeline_ingest_attempts_total"),
        4,
        "实际正文:\n{body}"
    );
    assert_eq!(
        value("qx_pipeline_ingested_events_total"),
        4,
        "实际正文:\n{body}"
    );
    assert_eq!(
        value("qx_pipeline_deduplicated_events_total"),
        0,
        "实际正文:\n{body}"
    );
    assert_eq!(value("qx_pipeline_refreshes_total"), 4, "实际正文:\n{body}");
    assert_eq!(
        value("qx_pipeline_transient_retries_total"),
        0,
        "实际正文:\n{body}"
    );
    assert_eq!(value("qx_pipeline_failures_total"), 0, "实际正文:\n{body}");
    let _ = std::fs::remove_dir_all(root);
}

/// 端到端：装配好的 API 服务把 worker 的 `.prom` 原样接在自身四条样本之后。
///
/// 这一条是当初立案的收口判据 —— 第八遍的发布面实测里 `/metrics` 三条样本没有 `qx_pipeline_*`。
#[test]
fn metrics_endpoint_aggregates_pipeline_counters_from_the_worker_file() {
    let root = temp_cli_case_dir("pipeline-metrics-aggregated");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);
    let file_body =
        std::fs::read_to_string(worker_metrics_dir(&config).join("paper-execution.prom")).unwrap();
    let on_disk = parse_exposition(&file_body);

    let config_path = data_dir.join("runtime.json");
    let service = build_configured_api_service(&config, &config_path).unwrap();
    let metrics = service.handle("GET", "/metrics", "", 3);
    assert_eq!(metrics.status, 200, "/metrics 必须可读: {}", metrics.body);
    let aggregated = metrics
        .body
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with("# "))
        .collect::<Vec<_>>();
    let pipeline_lines = on_disk
        .iter()
        .map(|(name, labels, value)| format!("{name}{{{labels}}} {value}"))
        .collect::<Vec<_>>();
    let found = aggregated
        .iter()
        .filter(|line| pipeline_lines.iter().any(|expected| expected == *line))
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(
        found,
        pipeline_lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        "抓取端从 `/metrics` 读到的 pipeline 样本要与 worker 落盘的那份逐字一致（聚合不改写值），\
         线上正文:\n{}",
        metrics.body
    );
    // 自身四条在前、worker 那份追加在后，是这个出口的既有形状。
    assert!(
        aggregated
            .iter()
            .position(|line| line.starts_with("qx_api_requests_total"))
            < aggregated
                .iter()
                .position(|line| line.starts_with("qx_pipeline_ingested_events_total")),
        "worker 的样本必须追加在 API 自身样本之后，实际正文:\n{}",
        metrics.body
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 失鲜的 paper 指标：`qx_worker_up` 被聚合侧改写成 0 并补一条 stale，六个计数仍要读得到。
///
/// 计数是历史事实，worker 死了不代表它没干过活；告警正是靠"up=0 而计数还在"区分崩溃与空闲。
#[test]
fn stale_paper_worker_metrics_keep_pipeline_counters_and_report_down() {
    let root = temp_cli_case_dir("pipeline-metrics-stale");
    let directory = root.join("worker-metrics");
    let labels = "worker=\"paper-execution\",account=\"main|paper\"";
    let body = format!(
        "qx_worker_up{{{labels}}} 1\nqx_worker_heartbeat_timestamp_seconds{{{labels}}} 1\n\
         qx_pipeline_ingested_events_total{{{labels}}} 7\nqx_pipeline_refreshes_total{{{labels}}} 3\n"
    );
    write_worker_metrics(&directory.join("paper-execution.prom"), &body).unwrap();
    let aggregated = read_worker_metrics(&directory, 40_000, 30_000);
    let samples = parse_exposition(&aggregated);
    assert!(
        samples
            .iter()
            .any(|(name, value_labels, value)| name == "qx_worker_up"
                && value_labels == labels
                && *value == 0),
        "心跳超时后 up 要按同一组标签改成 0，实际正文:\n{aggregated}"
    );
    assert!(
        aggregated.contains("qx_worker_metrics_stale{worker=\"paper-execution\"} 1"),
        "实际正文:\n{aggregated}"
    );
    assert!(
        samples
            .iter()
            .any(|(name, _, value)| name == "qx_pipeline_ingested_events_total" && *value == 7),
        "失鲜不该把计数一起抹掉，实际正文:\n{aggregated}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 量纲：一个 worker 进程把这一轮用过的每个 pipeline 各并一次，累计量单调、跨对象相加。
///
/// paper 循环里行情按命令、恢复按 tick 各开一个对象，所以"并一次"必须是**按对象**而不是按 tick；
/// 并漏一个对象是少计，并两次是双计，两种都不会让正文里任何一行报错，只能在这里钉。
#[test]
fn reporter_accumulates_each_pipeline_once_across_objects() {
    let root = temp_cli_case_dir("pipeline-metrics-accumulator");
    let config = paper_runtime_config(&root);
    let mut reporter = PipelineMetricsReporter::new(&config, "paper-execution", "main|paper");
    let quote = |ts: u64| {
        RuntimeEventEnvelope::market_quote(
            InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            QuoteTick::new(
                ts,
                Price::from_i64(99),
                Quantity::from_i64(1_000),
                Price::from_i64(100),
                Quantity::from_i64(1_000),
                ts,
            ),
            ts,
            ts,
            format!("accumulator:{ts}"),
        )
    };
    // 第一个对象：两次入账 + 一次同 external id 的重复。
    let mut first = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    first.ingest(quote(10)).unwrap();
    first.ingest(quote(20)).unwrap();
    let duplicate = first.ingest(quote(20)).unwrap();
    assert!(duplicate.deduplicated);
    reporter.absorb(&first);
    drop(first);
    // 第二个对象：两次入账各成一体 —— `absorb` 读的是对象**当前**的累计量，所以每个对象只许并一次：
    // 并两次会把已经并过的那段再加一遍。
    let mut second = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    second.ingest(quote(30)).unwrap();
    second.ingest(quote(40)).unwrap();
    reporter.absorb(&second);
    drop(second);

    let path = worker_metrics_dir(&config).join("paper-execution.prom");
    reporter.publish(true, 1_700_000_000_000);
    let samples = parse_exposition(&std::fs::read_to_string(&path).unwrap());
    let value = |name: &str| {
        samples
            .iter()
            .find(|(sample, _, _)| sample == name)
            .map(|(_, _, value)| *value)
            .unwrap()
    };
    // 两个对象各并一次：入账 2+2、尝试 3+2、重复 1+0，而每次 `ingest` 也各计一次 refresh。
    assert_eq!(
        value("qx_pipeline_ingested_events_total"),
        4,
        "实际样本: {samples:?}"
    );
    assert_eq!(
        value("qx_pipeline_ingest_attempts_total"),
        5,
        "实际样本: {samples:?}"
    );
    assert_eq!(
        value("qx_pipeline_deduplicated_events_total"),
        1,
        "实际样本: {samples:?}"
    );
    assert_eq!(
        value("qx_pipeline_refreshes_total"),
        5,
        "实际样本: {samples:?}"
    );
    assert_eq!(
        value("qx_pipeline_transient_retries_total"),
        0,
        "实际样本: {samples:?}"
    );
    assert_eq!(
        value("qx_pipeline_failures_total"),
        0,
        "实际样本: {samples:?}"
    );
    // 单调：同一份累计量再发布一次不会倒退。
    reporter.publish(true, 1_700_000_600_000);
    let again = parse_exposition(&std::fs::read_to_string(&path).unwrap());
    for (name, _, before) in &samples {
        let (_, _, after) = again.iter().find(|(sample, _, _)| sample == name).unwrap();
        assert!(after >= before, "{name} 从 {before} 倒退到 {after}");
    }
    let _ = std::fs::remove_dir_all(root);
}

/// 标签里混进引号时，正文要按 Prometheus 口径转义，且仍可逐行解析。
///
/// worker id 会进 `.prom` 的文件名，所以这里只让账户日志名带特殊字符 —— 它是标签，不是路径。
#[test]
fn reporter_escapes_labels_of_the_metrics_it_publishes() {
    let root = temp_cli_case_dir("pipeline-metrics-label-escape");
    let config = paper_runtime_config(&root);
    let reporter = PipelineMetricsReporter::new(&config, "paper-exec", "main\"paper");
    let path = worker_metrics_dir(&config).join("paper-exec.prom");
    reporter.publish(true, 1_700_000_000_000);
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读取 {} 失败: {error}", path.display()));
    assert!(
        body.contains("worker=\"paper-exec\",account=\"main\\\"paper\""),
        "标签里的引号要转义成 `\\\"`，否则抓取端在第一个内层引号处就把标签段截断，实际正文:\n{body}"
    );
    let samples = parse_exposition(&body);
    assert_eq!(samples.len(), 8, "实际正文:\n{body}");
    assert!(
        samples
            .iter()
            .all(|(_, labels, _)| labels.contains("main\\\"paper")),
        "八行的标签必须是同一组，实际正文:\n{body}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 发布节律：worker 循环里每次心跳都发一次指标，退出时把 `qx_worker_up` 收成 0。
///
/// 只在循环末尾"顺手"印一次的话，`--once` 之外的长跑进程在抓取端看就是"上次那个数"；
/// 心跳与发布同节律才是 NATS 那条 worker 已有的口径。这条取源码形状，因为节律不是
/// 单轮能跑出来的差异。
#[test]
fn pipeline_metrics_publish_shares_the_heartbeat_cadence() {
    let source = workspace_source("crates/qx-cli/src/venue_runtime/paper_worker.rs");
    let lines = source.lines().collect::<Vec<_>>();
    let heartbeats = lines
        .iter()
        .filter(|line| line.contains("context.heartbeat("))
        .count();
    assert_eq!(
        heartbeats, 2,
        "paper 的两条 worker 循环各有一次心跳，形状变了要先回看这条判据"
    );
    for (index, line) in lines.iter().enumerate() {
        if !line.contains("context.heartbeat(") {
            continue;
        }
        let previous = lines[index - 1].trim();
        assert!(
            previous.starts_with("metrics.publish(true,"),
            "心跳前必须先发布一次指标，否则抓取端读到的是上一 tick 的累计量:\n  {previous}"
        );
    }
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.trim().starts_with("metrics.publish(false,"))
            .count(),
        heartbeats,
        "每条循环退出时都要把 `qx_worker_up` 收成 0"
    );
    assert_eq!(
        source
            .lines()
            .filter(|line| line.contains("PipelineMetricsReporter::new("))
            .count(),
        heartbeats,
        "每条 worker 循环各有一份按进程累计的量"
    );
}

/// 接口文档要给出这六个样本的真形态：带 `worker`/`account` 标签，而不是标量。
///
/// 沿用的是 `documented_prometheus_metrics_are_the_names_the_runtime_emits` 的口径：文档点名
/// 的名字必须真有生产者印出，且带标签的不许写成 `名字=值`。
#[test]
fn interface_doc_publishes_the_labelled_pipeline_metric_shape() {
    let readme = workspace_source("deploy/README.md");
    for name in PIPELINE_COUNTERS {
        assert!(
            readme.contains(&format!("{name}{{worker=")),
            "接口文档点名带标签的 {name}，就必须给出它的真实形态 `{name}{{worker=\"<worker_id>\",…}} 计数`"
        );
        assert!(
            !readme.contains(&format!("{name}=")),
            "{name} 在生产里带 worker 标签，文档却写成 `{name}=…` 的标量形态：照文档写抓取会拿到空样本"
        );
    }
}
