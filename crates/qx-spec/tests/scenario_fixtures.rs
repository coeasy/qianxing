//! 七场景最小 fixture：`maturity/fixtures/scenarios/*.json` 必须能被真实的领域类型读入并通过校验。
//!
//! 规划口径（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §18 M0）：
//! 「为 A 股、国内期货、国内期权、国际股票、FX、加密现货/永续各提供最小 fixture」。
//! 这些 fixture 的价值不在于数据本身，而在于它们是**跨场景的身份样板**：任何一次领域契约
//! 变更（产品类型、币种、精度、档位、实验切分、运行记录）都必须同时改得动这七份样例，
//! 否则「支持 A 股/期货/期权/国际股票/FX/加密」就只是 README 上的一句话。
//!
//! 本用例把每份 fixture 的四块分别喂给真实类型：
//!   instrument_spec → qx_core::TradingInstrumentSpec
//!   dataset         → qx_data::DatasetManifestV2
//!   experiment      → qx_spec::ExperimentSpec
//!   run             → qx_spec::RunRecord
//! 四块都过，才算这个场景「表达得出、校验得过」。

use std::collections::BTreeSet;
use std::path::PathBuf;

use qx_spec::FoundationDocument;
use serde_json::Value;

/// 规划 §18 M0 点名的七个场景；少一个就是有一类市场没有样板。
const REQUIRED_SCENARIOS: [&str; 7] = [
    "ashare-equity",
    "cn-futures",
    "cn-options",
    "global-equity",
    "fx-cfd",
    "crypto-spot",
    "crypto-perpetual",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("maturity")
        .join("fixtures")
        .join("scenarios")
}

fn load_fixtures() -> Vec<(String, Value)> {
    let dir = fixtures_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("读取场景 fixture 目录失败 {dir:?}: {error}"))
        .map(|entry| entry.expect("目录项必须可读").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "场景 fixture 目录是空的：{dir:?}");
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("读取 {path:?} 失败: {error}"));
            let value: Value = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{path:?} 不是合法 JSON: {error}"));
            let name = value["scenario"]
                .as_str()
                .unwrap_or_else(|| panic!("{path:?} 缺 scenario 字段"))
                .to_string();
            (name, value)
        })
        .collect()
}

#[test]
fn every_scenario_fixture_carries_the_four_identity_blocks() {
    for (name, value) in load_fixtures() {
        for block in ["instrument_spec", "dataset", "experiment", "run"] {
            assert!(
                value.get(block).is_some_and(|node| node.is_object()),
                "场景 {name} 缺 {block} 块：四块是「身份样板」的最小完整集"
            );
        }
    }
}

#[test]
fn the_required_market_scenarios_are_all_present() {
    let present: BTreeSet<String> = load_fixtures().into_iter().map(|(name, _)| name).collect();
    let required: BTreeSet<String> = REQUIRED_SCENARIOS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let missing: Vec<&String> = required.difference(&present).collect();
    assert!(
        missing.is_empty(),
        "规划 §18 M0 点名的场景缺样板：{missing:?}（现有 {present:?}）"
    );
}

#[test]
fn every_instrument_spec_is_a_valid_trading_instrument_spec() {
    for (name, value) in load_fixtures() {
        let spec: qx_core::TradingInstrumentSpec =
            serde_json::from_value(value["instrument_spec"].clone()).unwrap_or_else(|error| {
                panic!("场景 {name} 的 instrument_spec 读不进 TradingInstrumentSpec: {error}")
            });
        spec.validate()
            .unwrap_or_else(|error| panic!("场景 {name} 的 instrument_spec 校验不过: {error}"));
    }
}

#[test]
fn the_fixtures_cover_every_declared_trading_product() {
    // 规划 §15.1 的产品类型表：equity/etf/bond 归 spot，另有 margin/future/option/perpetual。
    // 这里钉住「每一格产品类型至少有一个场景样板」，避免新增产品变体后没人给样例。
    let mut covered: BTreeSet<String> = BTreeSet::new();
    for (name, value) in load_fixtures() {
        let spec: qx_core::TradingInstrumentSpec =
            serde_json::from_value(value["instrument_spec"].clone())
                .unwrap_or_else(|error| panic!("场景 {name} 的 instrument_spec 读不进: {error}"));
        covered.insert(format!("{:?}", spec.product).to_ascii_lowercase());
    }
    for product in ["spot", "margin", "future", "option", "perpetual"] {
        assert!(
            covered.contains(product),
            "产品类型 {product} 没有任何场景样板（现有 {covered:?}）"
        );
    }
}

#[test]
fn every_dataset_block_is_a_valid_dataset_manifest_v2() {
    for (name, value) in load_fixtures() {
        let dataset: qx_data::DatasetManifestV2 = serde_json::from_value(value["dataset"].clone())
            .unwrap_or_else(|error| {
                panic!("场景 {name} 的 dataset 读不进 DatasetManifestV2: {error}")
            });
        dataset
            .validate()
            .unwrap_or_else(|error| panic!("场景 {name} 的 dataset 校验不过: {error}"));
    }
}

#[test]
fn every_experiment_block_is_a_valid_experiment_spec() {
    for (name, value) in load_fixtures() {
        let experiment: qx_spec::ExperimentSpec =
            serde_json::from_value(value["experiment"].clone()).unwrap_or_else(|error| {
                panic!("场景 {name} 的 experiment 读不进 ExperimentSpec: {error}")
            });
        experiment
            .validate()
            .unwrap_or_else(|error| panic!("场景 {name} 的 experiment 校验不过: {error}"));
    }
}

#[test]
fn every_run_block_is_a_valid_run_record() {
    for (name, value) in load_fixtures() {
        let run: qx_spec::RunRecord = serde_json::from_value(value["run"].clone())
            .unwrap_or_else(|error| panic!("场景 {name} 的 run 读不进 RunRecord: {error}"));
        run.validate()
            .unwrap_or_else(|error| panic!("场景 {name} 的 run 校验不过: {error}"));
    }
}
