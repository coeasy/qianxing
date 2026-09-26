//! 回测产物声明的输入身份可复核（V11 Q66 / Q1b 第一批）。
//!
//! 改之前：摘要里的 `input_data_hash` 是引擎对自己手里那段切片的自哈希（连 instrument 都不吃），
//! 那条真被数据集注册表复核过的 `DatasetManifest` 只印在 stdout 上。于是"这份产物跑的是哪一份
//! 数据"没有任何可核对的答案，跑完之后把输入文件换掉也检不出来。
//! 改之后：摘要带 `input` 块（路径 + 数据集身份 + 已复核指纹），`qx report` 按它写的路径走
//! **同一个读点**重算，对不上就拒绝出报告。
//! 摘要旁边那本 `*.run.json` 走的是同一条腿（V12 R4-i）：报告按摘要写下的 `run_manifest`
//! 指针重读清单，两格哈希对不上同样拒绝。此前那本清单只由写侧碰撞检查和用例读过，
//! 产物被拆开换掉一半——摘要留着、清单换成别一轮的——也照样能出报告。

use super::*;

/// 把仓库示例 BarFrame 复制到用例自己的目录，让产物声明的是副本路径而不是仓库文件。
fn bar_frame_copy(dir: &Path, label: &str) -> PathBuf {
    let (_, example, _) = builtin_backtest_example_paths();
    let path = dir.join(format!("{label}.bar-frame.json"));
    std::fs::copy(&example, &path).expect("复制示例 BarFrame");
    path
}

/// 改首根 Bar 的收盘价并原地写回：内容变了、声明的输入身份没变——这正是产物要能检出的一种。
fn tamper_bar_frame(path: &Path) {
    let mut payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let close = payload["close_raw"]
        .as_array_mut()
        .expect("close_raw 数组")
        .first_mut()
        .expect("至少一根 Bar");
    *close = serde_json::json!(close.as_i64().unwrap() + 1);
    std::fs::write(path, serde_json::to_string_pretty(&payload).unwrap()).unwrap();
}

fn depth_frame_copy(dir: &Path, label: &str) -> PathBuf {
    let (deploy, _, _) = builtin_backtest_example_paths();
    let path = dir.join(format!("{label}.depth-frame.json"));
    std::fs::copy(deploy.join("qianxing.depth-frame.l1.example.json"), &path)
        .expect("复制示例深度帧");
    path
}

fn tamper_depth_frame(path: &Path) {
    let mut payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let bid = &mut payload["snapshots"][0]["bids"][0]["price"];
    *bid = serde_json::json!(bid.as_i64().unwrap() + 1);
    std::fs::write(path, serde_json::to_string_pretty(&payload).unwrap()).unwrap();
}

/// `isolated_backtest_runtime` 固定给示例 Bundle；本模块要"没有 Bundle"那一档——RunManifest 的
/// `data_fingerprint` 正是从这一档回落到被复核过的数据集指纹（V11 Q66）。
fn runtime_without_bundle(runtime: &Path) {
    let mut config = read_runtime_config(runtime).unwrap();
    config.strategy.dataset_bundle_path = None;
    std::fs::write(
        runtime,
        serde_json::to_string_pretty(&config).expect("序列化运行时配置"),
    )
    .unwrap();
}

/// 跑一遍策略链并把摘要与 run.json 读回来。
fn run_bar_chain_and_read(
    runtime: &Path,
    frame: &Path,
    root: &Path,
) -> (serde_json::Value, serde_json::Value) {
    run_strategy_backtest(runtime, frame, None).expect("策略回测应跑通");
    (
        read_first_backtest_summary(root),
        read_first_artifact(root, ".run.json"),
    )
}

#[test]
fn bar_chain_declares_the_identity_the_registry_already_verified() {
    let (deploy, example, template) = builtin_backtest_example_paths();
    let root = temp_cli_case_dir("q66-bar-declared");
    let frame = bar_frame_copy(&root, "declared");
    let config = read_runtime_config(&template).unwrap();
    let (strategy_root, runtime) = isolated_backtest_runtime(&deploy, &config, "q66-bar-declared");
    runtime_without_bundle(&runtime);

    let (summary, run_manifest) = run_bar_chain_and_read(&runtime, &frame, &strategy_root);
    // 声明的身份必须出自那唯一的读点：与当场重算的一字不差。
    let verified = recompute_declared_backtest_input(&summary)
        .expect("干净的输入应当复核通过")
        .expect("策略链摘要必须带 input 块");
    let (bars, manifest) = {
        let parsed = read_bar_frame_for_backtest(&frame).unwrap();
        barframe_dataset_identity(&frame, &parsed).unwrap()
    };
    assert!(!bars.is_empty());
    assert_eq!(verified.kind, "barframe");
    assert_eq!(verified.path.as_str(), frame.to_string_lossy().as_ref());
    assert_eq!(verified.dataset_id, manifest.dataset_id);
    assert_eq!(verified.dataset_version, manifest.version);
    assert_eq!(verified.fingerprint, manifest.fingerprint);
    assert_eq!(
        summary["input"]["fingerprint"],
        serde_json::json!(manifest.fingerprint)
    );
    assert_eq!(
        summary["input"]["dataset_id"],
        serde_json::json!(format!("strategy-bars:{}", example_instrument(&example)))
    );
    // 回落那一格从此是被复核过的数据集指纹，而不是引擎对自己切片的自哈希。
    assert_eq!(
        run_manifest["data_fingerprint"],
        serde_json::json!(format!("barframe:{}", manifest.fingerprint))
    );
    assert_ne!(
        run_manifest["data_fingerprint"],
        serde_json::json!(summary["input_data_hash"]),
        "RunManifest 不得再拿引擎自哈希当输入身份"
    );
    // 报告这一面：绿灯路径也要真跑一次，否则"会拒绝"可能只是"从没检查"。
    run_report(&runtime, false).expect("未篡改时报告应通过");
    for path in [root, strategy_root] {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// 仓库示例 BarFrame 的标的，用来拼期望的 dataset id（不抄字面量）。
fn example_instrument(path: &Path) -> String {
    read_bar_frame_for_backtest(path)
        .unwrap()
        .instrument
        .to_string()
}

#[test]
fn report_refuses_when_the_declared_input_changed_after_the_run() {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let root = temp_cli_case_dir("q66-bar-tampered");
    let frame = bar_frame_copy(&root, "tampered");
    let config = read_runtime_config(&template).unwrap();
    let (strategy_root, runtime) = isolated_backtest_runtime(&deploy, &config, "q66-bar-tampered");
    runtime_without_bundle(&runtime);
    let (summary, _) = run_bar_chain_and_read(&runtime, &frame, &strategy_root);
    run_report(&runtime, false).expect("篡改前报告应通过");

    tamper_bar_frame(&frame);
    let error = recompute_declared_backtest_input(&summary).unwrap_err();
    assert!(
        error.contains("回测产物声明的输入与实况不符") && error.contains("fingerprint"),
        "篡改后的错误要指明是哪一格不符: {error}"
    );
    let report_error = run_report(&runtime, false).unwrap_err();
    assert!(
        report_error.contains("与实况不符"),
        "qx report 必须把这条拒绝原样抛给使用者: {report_error}"
    );
    // 摘要落盘的那份产物不被改写：报告拒绝的是"声明与实况不符"，不是把声明改成实况。
    let unchanged = read_first_backtest_summary(&strategy_root);
    assert_eq!(unchanged["input"], summary["input"]);
    for path in [root, strategy_root] {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[test]
fn report_refuses_when_the_declared_input_file_is_gone() {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let root = temp_cli_case_dir("q66-bar-missing");
    let frame = bar_frame_copy(&root, "removed");
    let config = read_runtime_config(&template).unwrap();
    let (strategy_root, runtime) = isolated_backtest_runtime(&deploy, &config, "q66-bar-missing");
    runtime_without_bundle(&runtime);
    let (summary, _) = run_bar_chain_and_read(&runtime, &frame, &strategy_root);
    std::fs::remove_file(&frame).unwrap();
    let error = recompute_declared_backtest_input(&summary).unwrap_err();
    assert!(
        error.contains("读取策略回测 BarFrame 失败"),
        "缺文件要走同一个读点的错误: {error}"
    );
    assert!(run_report(&runtime, false)
        .unwrap_err()
        .contains("读取策略回测 BarFrame 失败"));
    for path in [root, strategy_root] {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[test]
fn tampered_frame_cannot_take_the_registered_identity_of_the_clean_one() {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let root = temp_cli_case_dir("q66-bar-reingest");
    let frame = bar_frame_copy(&root, "reingest");
    let config = read_runtime_config(&template).unwrap();
    let (strategy_root, runtime) = isolated_backtest_runtime(&deploy, &config, "q66-bar-reingest");
    runtime_without_bundle(&runtime);
    run_bar_chain_and_read(&runtime, &frame, &strategy_root);
    tamper_bar_frame(&frame);
    // 同一份身份（数据集 id + 版本）不能对应两种内容：注册这一关先拦住。
    let error = run_strategy_backtest(&runtime, &frame, None).unwrap_err();
    assert!(
        error.contains("already registered with different manifest")
            || error.contains("fingerprint mismatch"),
        "篡改帧重跑应被数据集注册表拒绝: {error}"
    );
    for path in [root, strategy_root] {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[test]
fn depth_chain_declares_and_revalidates_its_own_frame() {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let root = temp_cli_case_dir("q66-depth");
    let frame = depth_frame_copy(&root, "depth");
    let depth_root = temp_cli_case_dir("q66-depth-root");
    let config = read_runtime_config(&template).unwrap();
    let (runtime_root, runtime) = isolated_backtest_runtime(&deploy, &config, "q66-depth");
    run_depth_backtest(
        "l1",
        "sma_cross",
        &frame,
        None,
        1,
        Some(5),
        DepthExecutionModel::default(),
        &depth_root,
        Some(&runtime),
    )
    .expect("深度回测应跑通");
    let summary = read_first_backtest_summary(&depth_root);
    let verified = recompute_declared_backtest_input(&summary)
        .expect("未篡改的深度输入应复核通过")
        .expect("深度链摘要也要声明输入");
    assert_eq!(verified.kind, "depth-frame");
    assert_eq!(verified.path.as_str(), frame.to_string_lossy().as_ref());
    assert_eq!(verified.dataset_version, DEPTH_FRAME_DATASET_VERSION);
    let parsed = read_depth_frame_for_backtest(&frame).unwrap();
    assert_eq!(
        verified.fingerprint,
        format!("{:016x}", parsed.input_hash())
    );
    assert_eq!(
        summary["input"]["dataset_id"],
        serde_json::json!(verified.dataset_id)
    );
    tamper_depth_frame(&frame);
    let error = recompute_declared_backtest_input(&summary).unwrap_err();
    assert!(
        error.contains("回测产物声明的输入与实况不符") && error.contains("fingerprint"),
        "深度链篡改要报同一形状的错: {error}"
    );
    for path in [root, depth_root, runtime_root] {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// 旧 schema 的产物没有 `input` 块：那是"没作过声明"，既不能算通过也不能算失败。
#[test]
fn summary_without_an_input_block_is_not_declared_rather_than_verified() {
    let mut legacy =
        serde_json::json!({ "schema_version": 2, "input_data_hash": "0000000000000001" });
    assert!(recompute_declared_backtest_input(&legacy)
        .unwrap()
        .is_none());
    // 声明了一个读不存在的形状同样不能被当成核对过：这里必须失败，而不是退回"没声明"。
    legacy["input"] = serde_json::json!({
        "kind": "csv",
        "path": "ignored.csv",
        "dataset_id": "x",
        "dataset_version": "v1",
        "fingerprint": "0",
    });
    let error = recompute_declared_backtest_input(&legacy).unwrap_err();
    assert!(
        error.contains("未知的回测输入种类"),
        "未知 kind 要失败: {error}"
    );
    // 少一格也不给过：声明得全，才轮得到去读那本文件。
    let mut partial = serde_json::json!({ "input": {
        "kind": "depth-frame",
        "path": "never-read.depth.json",
        "dataset_id": "x",
        "dataset_version": "v1",
    }});
    let error = recompute_declared_backtest_input(&partial).unwrap_err();
    assert!(error.contains("缺 fingerprint"), "缺字段要失败: {error}");
    // 声明完整但没写 kind 也一样：不给"缺的那格大概不重要"的余地。
    partial["input"].as_object_mut().unwrap().remove("kind");
    let error = recompute_declared_backtest_input(&partial).unwrap_err();
    assert!(error.contains("缺 kind"), "缺 kind 要失败: {error}");
}

/// 改写摘要指向的那本 RunManifest：模拟"清单被换成另一轮运行"或"清单本身坏了"。
fn rewrite_run_manifest(summary: &serde_json::Value, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = PathBuf::from(
        summary["run_manifest"]
            .as_str()
            .expect("策略链摘要必须写下 run_manifest 指针"),
    );
    let payload = std::fs::read_to_string(&path).expect("清单必须读得到");
    let mut manifest: serde_json::Value = serde_json::from_str(&payload).unwrap();
    edit(&mut manifest);
    std::fs::write(&path, serde_json::to_string(&manifest).unwrap()).expect("改写清单");
}

/// 跑一遍策略链并读回摘要。返回（摘要，runtime 路径，需要回收的用例目录）。
fn bar_chain_products(label: &str) -> (serde_json::Value, PathBuf, Vec<PathBuf>) {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let root = temp_cli_case_dir(label);
    let frame = bar_frame_copy(&root, label);
    let config = read_runtime_config(&template).unwrap();
    let (strategy_root, runtime) = isolated_backtest_runtime(&deploy, &config, label);
    runtime_without_bundle(&runtime);
    let (summary, _) = run_bar_chain_and_read(&runtime, &frame, &strategy_root);
    (summary, runtime, vec![root, strategy_root])
}

fn clean_up(dirs: Vec<PathBuf>) {
    for path in dirs {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// 拆开换掉一半产物：摘要不动、清单里的那两格哈希各换成别一轮的值。
#[test]
fn report_refuses_when_the_sibling_run_manifest_no_longer_matches() {
    let (summary, runtime, dirs) = bar_chain_products("r4i-manifest-drift");
    run_report(&runtime, false).expect("未拆开的产物报告应通过");
    let result_hash = summary["result_hash"].as_str().unwrap().to_string();
    // 两格各钉一次：只比对其中一格的实现在这里就会红。
    for (field, manifest_key, tampered) in [
        ("result_hash", "result_hash", "ffffffffffffffff"),
        ("input_data_hash", "input_event_hash", "eeeeeeeeeeeeeeee"),
    ] {
        rewrite_run_manifest(&summary, |manifest| {
            manifest["result_hash"] = serde_json::json!(result_hash);
            manifest[manifest_key] = serde_json::json!(tampered);
        });
        let error = recompute_declared_backtest_input(&summary).unwrap_err();
        assert!(
            error.contains("RunManifest 不一致") && error.contains(field),
            "对账要指名是 {field} 与清单不符: {error}"
        );
    }
    let report_error = run_report(&runtime, false).unwrap_err();
    assert!(
        report_error.contains("RunManifest 不一致"),
        "qx report 必须把清单对账失败原样抛给使用者: {report_error}"
    );
    clean_up(dirs);
}

/// 指针指不到文件，与"文件根本没声明"是两种结论：都得说清，都不能默默放过。
#[test]
fn report_refuses_when_the_declared_run_manifest_is_gone_or_unreadable() {
    let (summary, runtime, dirs) = bar_chain_products("r4i-manifest-gone");
    let pointer = summary["run_manifest"].as_str().unwrap().to_string();
    std::fs::remove_file(&pointer).unwrap();
    let error = recompute_declared_backtest_input(&summary).unwrap_err();
    assert!(
        error.contains("读取回测 RunManifest 失败") && error.contains(&pointer),
        "缺清单要报同一个路径: {error}"
    );
    assert!(
        run_report(&runtime, false)
            .unwrap_err()
            .contains("读取回测 RunManifest 失败"),
        "报告侧必须同样拒绝"
    );
    // 文件在、内容不是清单：那是"读不懂"，不能退成"没声明"。
    std::fs::write(&pointer, "{\"run_id\":\"only-half-a-manifest\"}").unwrap();
    let error = recompute_declared_backtest_input(&summary).unwrap_err();
    assert!(
        error.contains("RunManifest 无效"),
        "残缺清单要按无效处理: {error}"
    );
    clean_up(dirs);
}

/// 摘要声明了输入却不写清单指针：不给"少一格大概不重要"的余地。
#[test]
fn summary_that_omits_the_run_manifest_pointer_fails_instead_of_skipping_the_check() {
    let (mut summary, runtime, dirs) = bar_chain_products("r4i-manifest-missing");
    summary
        .as_object_mut()
        .expect("摘要必须是对象")
        .remove("run_manifest");
    let error = recompute_declared_backtest_input(&summary).unwrap_err();
    assert!(
        error.contains("没写 run_manifest 指针"),
        "指针缺失本身要失败，而不是跳过对账: {error}"
    );
    // 报告读的是落盘那份完整摘要：它仍然通过，说明失败来自指针缺失而不是产物坏了。
    run_report(&runtime, false).expect("落盘摘要必须照常通过");
    clean_up(dirs);
}
