//! Prometheus exposition 必须是逐行文本（V13 R2 第七遍 #177）。
//!
//! 缺陷形态是发布面实测抓到的那一个：`ApiMetricsSnapshot::to_prometheus` 把行分隔写成
//! "渲染出字面反斜杠 + n"的转义文本，`/metrics` 照常返回 200，正文却是一整行；抓取端解析
//! 失败后**不报错**，所有以这些指标为条件的告警永不触发。`contains` 在那种正文上照样读得出
//! 名字，所以这条判据只能按行取：名字读得出、样本读不出，就是没有可抓取的指标。
//!
//! 住在 `tests/` 而不是 `src/lib.rs` 的内联用例里，是为了不把常驻反例的压力加到被行数棘轮
//! 看管的读面上（同 `account_projection_identity.rs` 的口径，V11 T1/T3）。

use qx_api::{ApiService, ApiState};

/// 把 exposition 正文按 Prometheus 的读法逐行解析，返回样本名清单。
///
/// 刻意不用 `contains`：缺陷形态是"整份正文只有一行、行与行之间是字面的 `\n` 两个字符"
/// （V13 R2 第七遍实测到的 `\\n` 写法），`contains` 在那种正文上照样读得出名字，
/// 而抓取端一条样本都解析不出来。逐行解析是唯一能区分"有这个名字"与"这个名字是一条样本"的读法。
fn parse_prometheus_exposition(body: &str) -> Vec<String> {
    assert!(
        !body.contains("\\n"),
        "正文里出现字面反斜杠 + n：行分隔被写成转义文本，抓取端会把整份正文读成一行:\n{body}"
    );
    let mut samples = Vec::new();
    for line in body.trim_end_matches('\n').split('\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            let head = rest.split_whitespace().next().unwrap_or_default();
            assert!(
                matches!(head, "HELP" | "TYPE"),
                "注释行只能是 `# HELP` / `# TYPE`，实际 {line:?}"
            );
            continue;
        }
        let (metric, value) = line
            .rsplit_once(' ')
            .unwrap_or_else(|| panic!("样本行必须是 `<名> <值>`，实际 {line:?}"));
        assert!(
            !metric.contains(' '),
            "指标名（含标签）与值之间只许一个空格，实际 {line:?}"
        );
        value
            .parse::<f64>()
            .unwrap_or_else(|error| panic!("样本值必须是数字，实际 {line:?} / {error}"));
        samples.push(metric.to_string());
    }
    assert!(
        !samples.is_empty(),
        " exposition 正文里一条样本都没有，等于没有可抓取的指标"
    );
    samples
}

/// `/metrics` 必须是逐行的 exposition 文本，而不是一行里塞着字面 `\n`。
///
/// 这条判据守的是运维面的整读链：`deploy/prometheus/qianxing-alerts.yml` 里的告警按
/// `qx_api_*` / `qx_worker_*` 查询，抓取端解析失败时告警是"永不触发"而不是"报错"，
/// 所以这个缺陷在服务端与告警侧都不会自己出声（V13 R2 第七遍，实测见 logs/s143_*）。
#[test]
fn metrics_body_is_line_separated_prometheus_exposition() {
    let service = ApiService::new(ApiState::default())
        .with_worker_metrics_provider(|| "qx_worker_up{worker=\"relay\"} 1\n".into());
    let metrics = service.handle("GET", "/metrics", "", 1);
    assert_eq!(metrics.status, 200);
    assert_eq!(
        metrics.content_type,
        "text/plain; version=0.0.4; charset=utf-8"
    );
    assert_eq!(
        parse_prometheus_exposition(&metrics.body),
        vec![
            "qx_api_requests_total",
            "qx_api_rate_limit_rejected_total",
            "qx_api_authentication_rejected_total",
            "qx_api_command_enqueue_failures_total",
            "qx_worker_up{worker=\"relay\"}",
        ]
    );
}

#[test]
fn metrics_route_appends_supervised_worker_metrics() {
    let service = ApiService::new(ApiState::default())
        .with_worker_metrics_provider(|| "qx_worker_up{worker=\"relay\"} 1\n".into());
    let metrics = service.handle("GET", "/metrics", "", 1);
    assert_eq!(metrics.status, 200);
    let names = parse_prometheus_exposition(&metrics.body);
    assert!(
        names.contains(&"qx_worker_up{worker=\"relay\"}".to_string()),
        "被监督 worker 的样本必须出现在 /metrics 上: {names:?}"
    );
    assert!(
        metrics.body.find("qx_worker_up") > metrics.body.find("qx_api_requests_total"),
        "worker 段是追加在 API 自身样本之后，不是替换掉它们:\n{}",
        metrics.body
    );
}

/// 取一条样本的值；名字出现两次即判红，因为那意味着同一份 exposition 里有两个口径。
fn sample_value(body: &str, name: &str) -> f64 {
    let mut hits = body.trim_end_matches('\n').split('\n').filter_map(|line| {
        let (metric, value) = line.rsplit_once(' ')?;
        (metric == name).then(|| value.parse::<f64>().expect("sample value"))
    });
    let value = hits
        .next()
        .unwrap_or_else(|| panic!("exposition 里没有样本 {name}:\n{body}"));
    assert!(hits.next().is_none(), "样本 {name} 出现了多次:\n{body}");
    value
}

/// #189：入队失败必须有能被看见的通道。
///
/// 立案形态是 `let _ = enqueuer(..)` —— 命令已经落进控制面、回执仍是 202，所以"受理成功但
/// 这一次没进队列"在进程内**零痕迹**（worker 每轮按 `pending()` 补入，功能上自愈；运维上却
/// 只剩下一句"等一会儿就好了"的假设）。这条判据钉住收口后的两端：回执仍是 202（不推翻
/// 已成立的受理），而计数必须动。
#[test]
fn failed_command_enqueue_is_counted_without_retracting_the_acceptance() {
    fn submitter(
        command: qx_control::ControlCommand,
        granted: qx_control::Permission,
        ts: u64,
    ) -> Result<(qx_control::ControlPlane, qx_control::AuditRecord), qx_api::ControlSubmitError>
    {
        let mut plane = qx_control::ControlPlane::default();
        let audit = plane
            .submit_as(command, granted, ts)
            .map_err(qx_api::ControlSubmitError::Rejected)?;
        Ok((plane, audit))
    }
    fn command(request_id: &str) -> String {
        serde_json::to_string(&qx_control::ControlCommand {
            command_id: 7,
            request_id: request_id.into(),
            operator_id: "ops".into(),
            reason: "enqueue failure probe".into(),
            kind: qx_control::CommandKind::PauseStrategy,
            target: "strategy-1".into(),
            payload: Default::default(),
            permission: qx_control::Permission::Trading,
            dry_run: true,
        })
        .expect("command is serializable")
    }

    let failing = ApiService::new(ApiState::default())
        .with_control_submitter(submitter)
        .with_command_enqueuer(|_, _| Err("queue is gone".into()));
    assert_eq!(
        sample_value(
            &failing.handle("GET", "/metrics", "", 1).body,
            "qx_api_command_enqueue_failures_total"
        ),
        0.0,
        "没有提交过命令时这条计数从 0 起"
    );
    let accepted = failing.handle("POST", "/control/commands", &command("enqueue-fail-1"), 2);
    assert_eq!(
        accepted.status, 202,
        "入队失败不推翻已成立的受理: {}",
        accepted.body
    );
    assert_eq!(
        sample_value(
            &failing.handle("GET", "/metrics", "", 3).body,
            "qx_api_command_enqueue_failures_total"
        ),
        1.0,
        "入队失败必须留下一条能被抓取到的痕迹"
    );

    // 反向对照：入队成功时这条计数不动，否则它就不是"失败"计数而是"提交"计数。
    let healthy = ApiService::new(ApiState::default())
        .with_control_submitter(submitter)
        .with_command_enqueuer(|_, _| Ok(()));
    assert_eq!(
        healthy
            .handle("POST", "/control/commands", &command("enqueue-ok-1"), 2)
            .status,
        202
    );
    assert_eq!(
        sample_value(
            &healthy.handle("GET", "/metrics", "", 3).body,
            "qx_api_command_enqueue_failures_total"
        ),
        0.0
    );
}
