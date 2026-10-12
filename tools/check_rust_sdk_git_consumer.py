#!/usr/bin/env python3
"""Build and run qx-sdk from an isolated consumer pinned to the current Git commit."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CONSUMER = ROOT / "target" / "sdk-validation" / "rust-git-consumer"
TARGET = ROOT / "target"
SAMPLE_BARS = ROOT / "deploy" / "qianxing.bar-frame.example.json"
GIT_URL = "https://github.com/coeasy/qianxing.git"

MAIN_RS = r'''use qx_sdk::{
    BacktestSpec, BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability, Client,
    DatasetSpec, RemoteApiClient, RemoteResponse, RemoteTransport,
};
use std::path::PathBuf;

struct FakeRemote;
impl RemoteTransport for FakeRemote {
    fn post(&self, path: &str, body: &str) -> Result<RemoteResponse, String> {
        assert!(path.starts_with("/app/paper/algo-executions/"), "unexpected route: {path}");
        let _: serde_json::Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
        Ok(RemoteResponse::new(200, r#"{"accepted":true}"#))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bars = std::env::var("QX_SAMPLE_BARS")?;
    let output = PathBuf::from(std::env::var("QX_OUTPUT_DIR")?);
    std::fs::create_dir_all(&output)?;
    let client = Client::new(CallerCapability::Research, "external-git-consumer");
    let dataset = client.validate_dataset(&DatasetSpec::new("external-bars", bars.clone()))?;
    assert!(dataset.usable, "sample dataset must be usable: {:?}", dataset.gaps);

    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "external-sma");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    let spec = BacktestSpec::new(
        "external-git-run",
        "BTCUSDT.BINANCE",
        bars,
        "USDT",
        100_000_i128 * 1_000_000_000,
        20261012,
        output.to_string_lossy().into_owned(),
        strategy,
    );
    let result = client.run_backtest(&spec)?;
    assert!(result.fills > 0, "external consumer backtest must trade");
    let checked = client.verify_run(&result)?;
    assert!(checked.verified, "external consumer verification failed: {:?}", checked.mismatches);
    let remote = RemoteApiClient::new(FakeRemote);
    let started = remote.start_paper_algo_execution(&serde_json::json!({"parent_id":"external-paper"}))?;
    assert_eq!(started["accepted"], true);
    remote.get_paper_algo_execution("external-paper")?;
    remote.cancel_paper_algo_execution("external-paper")?;
    remote.list_paper_algo_executions(None, 25)?;
    println!("rust_sdk_git_consumer=passed fills={} remote_paper_lifecycle=4 result_hash={}", result.fills, result.result_hash);
    Ok(())
}
'''


def main() -> int:
    revision = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout.strip()
    CONSUMER.mkdir(parents=True, exist_ok=True)
    (CONSUMER / "src").mkdir(exist_ok=True)
    manifest = f'''[package]
name = "qx-sdk-git-consumer-check"
version = "0.1.0"
edition = "2021"

[workspace]

[dependencies]
qx-sdk = {{ git = "{GIT_URL}", rev = "{revision}", package = "qx-sdk" }}
serde_json = "1"
'''
    (CONSUMER / "Cargo.toml").write_text(manifest, encoding="utf-8")
    (CONSUMER / "src" / "main.rs").write_text(MAIN_RS, encoding="utf-8")

    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(TARGET)
    # Avoid a known MSVC debug-info invocation failure in this host image; this
    # is a consumer build configuration, not a source-code workaround.
    env["CARGO_PROFILE_DEV_DEBUG"] = "0"
    env["QX_SAMPLE_BARS"] = str(SAMPLE_BARS)
    env["QX_OUTPUT_DIR"] = str(
        ROOT / "target" / "sdk-validation" / f"rust-git-consumer-run-{os.getpid()}"
    )
    command = [
        "cargo", "run", "--manifest-path", str(CONSUMER / "Cargo.toml"),
    ]
    if os.environ.get("QX_CARGO_OFFLINE") == "1":
        command.append("--offline")
    completed = subprocess.run(command, cwd=CONSUMER, env=env, text=True)
    if completed.returncode != 0:
        return completed.returncode
    print(f"source_revision={revision}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
