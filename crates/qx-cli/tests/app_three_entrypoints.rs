//! 退出门 G1：**同一 use case 从 CLI / Python / HTTP 三个入口调同一个 `qx-app` service**，
//! 结果哈希相同、错误 code 与 correlation id 相同。
//!
//! 三个入口在这里被当成三件**不同的东西**对待，而不是三次函数调用：
//!
//! - CLI 腿真的 spawn 一个 `qx-cli` 进程，读它的 stdout；
//! - HTTP 腿真的走 `qx_api::ApiService::handle("POST", "/app/backtest", …)`（与 `qx-cli serve`
//!   后面那台服务是同一个分派入口），读响应体；
//! - Python 腿真的起一个解释器，import `qianxing_bridge.app`，读它打印的那一行。
//!
//! 三腿交出**同一份 spec 字节**（同一 `run_id`、同一 `output_dir`），所以三份产物会落到同一个
//! 目录：第二次与第三次运行命中幂等分支（同一 `config_hash` + 同一 `data_fingerprint`），
//! `result_hash` 必须逐位相等。这条断言比"三个进程各跑各的、比一比哈希"强——它同时证明了
//! 幂等分支在三入口下是同一个分支。
//!
//! ## Python 腿的两态
//!
//! PyO3 扩展是**构建产物**（`cargo build -p qx-python` 或 `tools/build_python_wheel.*`），
//! 干净检出里没有它。所以：扩展在盘 → 逐字节断言；扩展不在盘 → 这一条腿**跳过并打印原因**
//! （与 `python/tests/test_native_extension.py` 同一约定）。「Python 入口存在」这件事不由这条
//! 用例守，而由门禁 `qx_app_check` 结构性地钉住（源码面在盘）——两件事分开守，
//! 不让一个 `skip` 把另一个也一起放掉。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// 仓库根：本文件在 `crates/qx-cli/tests/` 下。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/qx-cli 一定有仓库根")
        .to_path_buf()
}

/// 本用例独享的临时目录。
fn scratch(label: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("qx-app-g1/{label}-{}-{serial}", std::process::id()));
    std::fs::create_dir_all(&root).expect("建临时目录");
    root
}

fn sample_bars() -> String {
    repo_root()
        .join("deploy")
        .join("qianxing.bar-frame.example.json")
        .to_string_lossy()
        .into_owned()
}

/// 三腿共用的那份 spec 字节。
fn backtest_spec_json(run_id: &str, bars_path: &str, output_dir: &Path) -> String {
    json!({
        "schema_version": 1,
        "run_id": run_id,
        "instrument": "BTCUSDT.BINANCE",
        "bars_path": bars_path,
        "settlement_currency": "USDT",
        "initial_cash_raw": 100_000_000_000_000_i128,
        "seed": 20261010,
        "output_dir": output_dir.to_string_lossy(),
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "g1-sma",
            "quantity_raw": 1_000_000_000_i128,
            "fast_window": 2,
            "slow_window": 3,
        },
    })
    .to_string()
}

/// CLI 腿：真 spawn 二进制，读 stdout。
fn via_cli(spec_json: &str, root: &Path) -> Value {
    let spec_path = root.join("spec.json");
    std::fs::write(&spec_path, spec_json).expect("写 spec");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "backtest"])
        .arg(&spec_path)
        .output()
        .expect("启动 qx-cli 失败");
    assert_eq!(
        output.status.code(),
        Some(0),
        "CLI 腿必须成功：stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI 腿的 stdout 必须是一份 JSON")
}

/// HTTP 腿：走 `qx_api` 的分派入口（与 `qx-cli serve` 后面那台服务同一个入口）。
fn via_http(route: &str, body: &str) -> (u16, Value) {
    static CONFIGURED_PATHS: OnceLock<()> = OnceLock::new();
    CONFIGURED_PATHS.get_or_init(|| {
        std::env::set_var("QX_API_DATA_ROOT", repo_root());
        std::env::set_var(
            "QX_API_ARTIFACT_ROOT",
            std::env::temp_dir().join(format!("qx-app-g1-api-{}", std::process::id())),
        );
    });
    let service = qx_api::ApiService::new(qx_api::ApiState::default());
    let response = service.handle("POST", route, body, 0);
    let payload = serde_json::from_str(&response.body)
        .unwrap_or_else(|error| panic!("HTTP 腿的响应体必须是 JSON（{error}）：{}", response.body));
    (response.status, payload)
}

/// Python 腿：起一个解释器 import `qianxing_bridge.app`。`None` 表示扩展没构建。
fn via_python(function: &str, argument: &str) -> Option<Result<Value, String>> {
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            repo_root()
                .join("python/.venv/Scripts/python.exe")
                .to_string_lossy()
                .into_owned()
        } else {
            repo_root()
                .join("python/.venv/bin/python")
                .to_string_lossy()
                .into_owned()
        }
    });
    if !Path::new(&python).is_file() {
        return None;
    }
    let root = repo_root();
    let script = format!(
        "import json, sys\n\
         sys.path.insert(0, {python_dir:?})\n\
         try:\n\
         \x20   from qianxing_bridge import app, native\n\
         except Exception as error:\n\
         \x20   print('QX_APP_PYTHON_UNAVAILABLE: ' + str(error), file=sys.stderr)\n\
         \x20   sys.exit(3)\n\
         if not native.app_available():\n\
         \x20   print('QX_APP_PYTHON_UNAVAILABLE: 扩展没构建，或是没有应用层入口的旧世代', file=sys.stderr)\n\
         \x20   sys.exit(3)\n\
         try:\n\
         \x20   result = app.{function}({argument:?})\n\
         except Exception as error:\n\
         \x20   payload = app.app_error_payload(error)\n\
         \x20   print(json.dumps({{'qx_app_error': payload if payload is not None else str(error)}}))\n\
         \x20   sys.exit(4)\n\
         print(json.dumps(result))\n",
        python_dir = root.join("python").to_string_lossy(),
    );
    let output = Command::new(&python)
        .arg("-c")
        .arg(script)
        .current_dir(root.join("python"))
        .output()
        .expect("启动解释器失败");
    if output.status.code() == Some(3) {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match output.status.code() {
        Some(0) => Some(Ok(
            serde_json::from_str(stdout.trim()).expect("Python 腿的 stdout 是 JSON")
        )),
        Some(4) => {
            let value: Value = serde_json::from_str(stdout.trim()).expect("Python 腿的错误是 JSON");
            Some(Err(value["qx_app_error"].to_string()))
        }
        other => panic!(
            "Python 腿异常退出 {other:?}：stderr={}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

#[test]
fn the_same_backtest_spec_gives_the_same_result_hash_from_cli_http_and_python() {
    let root = scratch("backtest");
    let output_dir = root.join("out");
    let spec = backtest_spec_json("g1-run", &sample_bars(), &output_dir);

    let cli = via_cli(&spec, &root);
    let (status, http) = via_http("/app/backtest", &spec);
    assert_eq!(status, 200, "HTTP 腿必须回 200：{http}");

    for (name, value) in [("CLI", &cli), ("HTTP", &http)] {
        assert_eq!(value["run_id"], "g1-run", "{name} 腿的 run_id");
        assert!(
            value["fills"].as_u64().unwrap_or(0) > 0,
            "{name} 腿必须真跑出成交"
        );
        assert_eq!(value["equity_points"], 70, "{name} 腿的权益点数");
    }
    assert_eq!(
        cli["result_hash"], http["result_hash"],
        "CLI 与 HTTP 必须给出同一个结果哈希（同一份 spec、同一个 use case）"
    );
    assert_eq!(cli["data_fingerprint"], http["data_fingerprint"]);
    assert_eq!(cli["fills"], http["fills"]);
    assert!(
        Path::new(http["artifacts"]["run_manifest"].as_str().unwrap()).starts_with(
            std::env::temp_dir().join(format!("qx-app-g1-api-{}", std::process::id())),
        ),
        "HTTP request must not control the server-side artifact path"
    );

    match via_python("run_backtest", &spec) {
        None => eprintln!(
            "[跳过] Python 腿：PyO3 扩展未构建。跑 `cargo build -p qx-python` 后这条腿会逐字节断言。"
        ),
        Some(Ok(python)) => {
            assert_eq!(
                python["result_hash"], cli["result_hash"],
                "Python 与 CLI 必须给出同一个结果哈希"
            );
            assert_eq!(python["data_fingerprint"], cli["data_fingerprint"]);
            assert_eq!(python["fills"], cli["fills"]);
        }
        Some(Err(payload)) => panic!("Python 腿本该成功，却报了错：{payload}"),
    }
}

#[test]
fn the_same_failure_gives_the_same_error_document_from_cli_http_and_python() {
    let root = scratch("error");
    // 让三腿都走同一条失败路径：spec 合法、数据文件不在。
    let spec = backtest_spec_json(
        "g1-missing",
        &repo_root().join("no-such-bars.json").to_string_lossy(),
        &root.join("out"),
    );

    // CLI：退出码 2，stderr 的 `[qx-cli · CLI] {文档}` 那一行取 `{` 之后的原文。
    let spec_path = root.join("spec.json");
    std::fs::write(&spec_path, &spec).expect("写 spec");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "backtest"])
        .arg(&spec_path)
        .output()
        .expect("启动 qx-cli 失败");
    assert_eq!(output.status.code(), Some(2), "CLI 腿失败必须退 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let document = stderr
        .split_once('{')
        .map(|(_, rest)| format!("{{{rest}"))
        .expect("CLI 的 stderr 必须带一份 AppError 文档");
    let cli: Value = serde_json::from_str(document.trim()).expect("CLI 的错误文档可解析");

    let (status, http) = via_http("/app/backtest", &spec);
    assert_eq!(status, 404, "数据不在 → 404（由 category 派生）：{http}");

    for (name, value) in [("CLI", &cli), ("HTTP", &http)] {
        assert_eq!(value["category"], "DATA_UNAVAILABLE", "{name} 腿的类别");
        assert_eq!(value["action"], "PROVIDE_DATA", "{name} 腿的行动提示");
        assert_eq!(value["retry"], "NEVER", "{name} 腿的重试档");
        assert_eq!(value["safe_to_retry"], false, "{name} 腿的安全重试");
        assert_eq!(value["source_code"], "io:NotFound", "{name} 腿的底层诊断码");
        assert_eq!(value["correlation_id"], "g1-missing", "{name} 腿的对齐 id");
    }
    assert_eq!(cli["category"], http["category"]);
    assert_eq!(cli["correlation_id"], http["correlation_id"]);

    match via_python("run_backtest", &spec) {
        None => eprintln!("[跳过] Python 腿：PyO3 扩展未构建。"),
        Some(Err(payload)) => {
            let python: Value = serde_json::from_str(&payload).expect("Python 的错误文档可解析");
            assert_eq!(python["category"], cli["category"]);
            assert_eq!(python["source_code"], cli["source_code"]);
            assert_eq!(python["correlation_id"], cli["correlation_id"]);
        }
        Some(Ok(value)) => panic!("Python 腿本该失败，却成功返回了：{value}"),
    }
}

#[test]
fn the_verify_use_case_also_agrees_across_entrypoints() {
    // G1 的那条 use case 链是「validate → backtest → artifact → verify」，所以 verify 也要三入口一致。
    let root = scratch("verify");
    let output_dir = root.join("out");
    let spec = backtest_spec_json("g1-verify", &sample_bars(), &output_dir);
    let outcome = via_cli(&spec, &root);
    let outcome_document = outcome.to_string();

    let (status, http) = via_http("/app/verify", &outcome_document);
    assert_eq!(status, 200, "HTTP verify 必须回 200：{http}");
    assert_eq!(http["verified"], true, "刚落的产物必须自洽：{http}");
    assert_eq!(http["result_hash"], outcome["result_hash"]);
    assert!(http["checks"].as_array().map(Vec::len).unwrap_or(0) > 0);

    // 数据集校验同样三入口一致。
    let dataset = json!({
        "schema_version": 1,
        "dataset_id": "g1-dataset",
        "bars_path": sample_bars(),
    })
    .to_string();
    let (status, http_verdict) = via_http("/app/validate-dataset", &dataset);
    assert_eq!(status, 200);
    assert_eq!(http_verdict["usable"], true);
    assert_eq!(http_verdict["rows"], 70);

    let traversal = json!({
        "schema_version": 1,
        "dataset_id": "g1-traversal",
        "bars_path": "../Cargo.toml",
    })
    .to_string();
    let (status, error) = via_http("/app/validate-dataset", &traversal);
    assert_eq!(
        status, 400,
        "HTTP data path traversal must be refused: {error}"
    );

    match via_python("verify_run", &outcome_document) {
        None => eprintln!("[跳过] Python 腿：PyO3 扩展未构建。"),
        Some(Ok(python)) => {
            assert_eq!(python["verified"], true);
            assert_eq!(python["result_hash"], outcome["result_hash"]);
        }
        Some(Err(payload)) => panic!("Python verify 本该成功：{payload}"),
    }
}

#[test]
fn compare_runs_agrees_across_cli_http_and_python_with_stable_ranking() {
    let root = scratch("compare-runs");
    let bars = sample_bars();
    let first = via_cli(
        &backtest_spec_json("g1-compare-a", &bars, &root.join("a")),
        &root,
    );
    let mut second_spec: Value =
        serde_json::from_str(&backtest_spec_json("g1-compare-b", &bars, &root.join("b"))).unwrap();
    second_spec["strategy"]["strategy_id"] = json!("g1-sma-b");
    second_spec["strategy"]["fast_window"] = json!(3);
    second_spec["strategy"]["slow_window"] = json!(4);
    let second = via_cli(&second_spec.to_string(), &root);
    let run_summary = |outcome: &Value| {
        json!({
            "run_id": outcome["run_id"],
            "instrument": outcome["instrument"],
            "data_fingerprint": outcome["data_fingerprint"],
            "result_hash": outcome["result_hash"],
            "return_bps": outcome["return_bps"],
            "max_drawdown_bps": outcome["max_drawdown_bps"],
        })
    };
    let spec = json!({"schema_version": 1, "runs": [run_summary(&first), run_summary(&second)]});
    let spec_path = root.join("compare.json");
    std::fs::write(&spec_path, spec.to_string()).unwrap();
    let cli_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "compare-runs"])
        .arg(&spec_path)
        .output()
        .expect("启动 qx-cli compare-runs 失败");
    assert_eq!(
        cli_output.status.code(),
        Some(0),
        "CLI compare 失败: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli_output.stdout).unwrap();
    let (status, http) = via_http("/app/compare-runs", &spec.to_string());
    assert_eq!(status, 200, "HTTP compare 失败: {http}");
    assert_eq!(cli, http, "CLI/HTTP 应返回逐字节同形的确定性对比结果");
    assert_eq!(http["runs"].as_array().unwrap().len(), 2);
    assert_eq!(http["runs"][0]["rank"], 1);
    match via_python("compare_runs", &spec.to_string()) {
        None => eprintln!("[跳过] Python 腿：PyO3 扩展未构建。"),
        Some(Ok(python)) => assert_eq!(python, cli, "Python compare 应与 CLI/HTTP 返回同一结果"),
        Some(Err(payload)) => panic!("Python compare 本该成功：{payload}"),
    }
}

#[test]
fn parameter_experiment_uses_the_same_rust_use_case_from_cli_http_and_python() {
    let root = scratch("g1-experiment");
    let bars = sample_bars();
    let base: Value =
        serde_json::from_str(&backtest_spec_json("unused-base", &bars, &root.join("out"))).unwrap();
    let spec = json!({
        "schema_version": 1,
        "experiment_id": "g1-parameter-grid",
        "base": base,
        "parameter_space": [{"name": "fast_window", "values": [1, 2]}]
    });
    let spec_path = root.join("experiment.json");
    std::fs::write(&spec_path, spec.to_string()).unwrap();
    let cli_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "run-experiment"])
        .arg(&spec_path)
        .output()
        .expect("启动 qx-cli run-experiment 失败");
    assert_eq!(
        cli_output.status.code(),
        Some(0),
        "CLI experiment 失败: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli_output.stdout).unwrap();
    let (status, http) = via_http("/app/run-experiment", &spec.to_string());
    assert_eq!(status, 200, "HTTP experiment 失败: {http}");
    let same_results = |value: &Value| {
        json!({
            "schema_version": value["schema_version"],
            "experiment_id": value["experiment_id"],
            "total_candidates": value["total_candidates"],
            "completed_candidates": value["completed_candidates"],
            "succeeded_candidates": value["succeeded_candidates"],
            "failed_candidates": value["failed_candidates"],
            "comparison": value["comparison"],
            "candidates": value["candidates"].as_array().unwrap().iter().map(|candidate| {
                json!({
                    "ordinal": candidate["ordinal"],
                    "parameters": candidate["parameters"],
                    "run_id": candidate["outcome"]["run_id"],
                    "result_hash": candidate["outcome"]["result_hash"],
                })
            }).collect::<Vec<_>>(),
        })
    };
    assert_eq!(same_results(&cli), same_results(&http));
    assert_eq!(cli["completed_candidates"], 2);
    assert_eq!(cli["succeeded_candidates"], 2);
    assert_eq!(cli["failed_candidates"], 0);
    match via_python("run_experiment", &spec.to_string()) {
        None => eprintln!("[跳过] Python 腿：PyO3 扩展未构建。"),
        Some(Ok(python)) => assert_eq!(same_results(&python), same_results(&cli)),
        Some(Err(payload)) => panic!("Python experiment 本该成功：{payload}"),
    }
}
