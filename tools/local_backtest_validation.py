#!/usr/bin/env python3
"""Offline A-share/crypto backtest acceptance with verified HTML reports.

Uses only repository example data. Creates fresh directories, never deletes or
overwrites an existing validation workspace, and delegates financial calculations
and replay verification to qx-cli. No exchange workers are launched.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from datetime import datetime, timezone, timedelta
import zipfile

import backtest_acceptance as acceptance


def execute(binary: Path, args: list[str], cwd: Path, logs: Path, name: str) -> tuple[int, str]:
    code, output = acceptance.run([str(binary), *args], cwd)
    (logs / f"{name}.log").write_text(output, encoding="utf-8")
    return code, output


def require_success(binary: Path, args: list[str], cwd: Path, logs: Path, name: str) -> str:
    code, output = execute(binary, args, cwd, logs, name)
    if code:
        raise RuntimeError(f"{name}: qx-cli exit {code}; inspect {logs / (name + '.log')}")
    return output


def validate(binary: Path, work: Path, reports: Path) -> dict:
    result = {"schema_version": 1, "status": "running", "data_scope": "repository_examples_only",
              "generated_at": datetime.now(timezone(timedelta(hours=8))).isoformat(),
              "binary": str(binary), "binary_sha256": acceptance.sha256_file(binary),
              "work_dir": str(work), "markets": {}, "problems": []}
    logs = reports / "logs"
    logs.mkdir()
    legs_by_market = {}
    for market in ("crypto", "ashare"):
        legs = []
        for index in range(2):
            label = f"{market}-{index + 1}"
            project = work / (market[0] + str(index + 1))
            project.mkdir()
            runtime = project / "qianxing.runtime.json"
            if market == "crypto":
                require_success(binary, ["quickstart", str(project)], work, logs, f"{label}-setup")
                bars = project / "qianxing.bar-frame.example.json"
                spec = project / "qianxing.binance.spot.spec.json"
            else:
                require_success(binary, ["init", str(runtime), "--profile", "ashare"], work, logs, f"{label}-setup")
                bars = project / "qianxing.ashare.bar-frame.example.json"
                spec = project / "qianxing.ashare.spot.spec.json"
            snapshots = []
            for attempt in range(2):
                output = require_success(binary, ["backtest", str(runtime), str(bars), str(spec)], work, logs, f"{label}-backtest-{attempt + 1}")
                snapshots.append(acceptance.snapshot_from_output(output, project))
            status = acceptance.read_status(binary, runtime, work)
            leg = {"label": label, "first": snapshots[0], "second": snapshots[1],
                   "network_accessed": status.get("network_accessed"), "orders_sent": status.get("orders_sent")}
            legs.append(leg)
            if index == 0:
                document = json.loads(require_success(binary, ["report", str(runtime), "--json"], work, logs, f"{market}-report-json"))
                summary_path = Path(document["summary_path"])
                html_path = reports / f"{market}-backtest.html"
                require_success(binary, ["report", str(summary_path), "--html", "-o", str(html_path)], work, logs, f"{market}-report-html")
                if not html_path.is_file() or html_path.stat().st_size == 0:
                    raise RuntimeError(f"{market}: HTML report was not written")
                (reports / f"{market}-backtest.json").write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
                negatives = []
                # Verify that report readers reject changed inputs and artifact bytes.
                for kind, path in (("input", bars), ("fills", Path(document["summary"]["artifact_locations"]["fills"]))):
                    original = path.read_bytes()
                    try:
                        if kind == "input":
                            frame = json.loads(original)
                            frame["close_raw"][0] += 1
                            path.write_text(json.dumps(frame), encoding="utf-8")
                        else:
                            path.write_bytes(original + b"\n")
                        code, _ = execute(binary, ["report", str(summary_path), "--json"], work, logs, f"{market}-expected-reject-{kind}")
                        negatives.append({"kind": kind, "exit_code": code, "expected_rejection": code != 0})
                        if code == 0:
                            result["problems"].append(f"{market}: tampered {kind} was not rejected")
                    finally:
                        path.write_bytes(original)
                require_success(binary, ["report", str(summary_path), "--json"], work, logs, f"{market}-restored-report")
                summary = document["summary"]
                if document.get("input_check", {}).get("verdict") != "verified":
                    result["problems"].append(f"{market}: Rust input verification did not pass")
                if summary.get("fills", 0) < 1:
                    result["problems"].append(f"{market}: fixture did not exercise a fill")
                result["markets"][market] = {
                    "instrument": summary["instrument"], "strategy_id": summary["strategy_id"],
                    "bars": summary["bars"], "fills": summary["fills"],
                    "metrics": summary["metrics"], "metrics_exact": summary.get("metrics_exact"),
                    "replay": summary["replay"], "input_verdict": document["input_check"]["verdict"],
                    "replay_verdict": snapshots[0]["replay_verdict"], "result_hash": summary["result_hash"],
                    "html_report": html_path.name, "negative_checks": negatives,
                    "source_summary": str(summary_path), "snapshots": snapshots,
                }
        result["problems"].extend(problem for leg in legs for problem in acceptance.compare_reruns(leg))
        result["problems"].extend(acceptance.compare_independent(legs))
        result["problems"].extend(acceptance.compare_no_credentials(legs))
        legs_by_market[market] = legs
    result["repeatability_evidence"] = legs_by_market
    result["status"] = "passed" if not result["problems"] else "failed"
    return result


def write_summary(reports: Path, result: dict) -> None:
    lines = ["# 本地 A 股与数字货币回测验证", "", f"执行时间：{result['generated_at']}", "",
             f"结果：**{result['status']}**。数据范围为仓库示例数据，不代表真实历史数据或实盘验收。", "",
             "| 市场 | 标的 | 策略 | Bar 数 | 成交数 | 输入校验 | 回放校验 | 报告 |",
             "|---|---|---|---:|---:|---|---|---|"]
    for market, value in result["markets"].items():
        lines.append(f"| {market} | {value['instrument']} | {value['strategy_id']} | {value['bars']} | {value['fills']} | {value['input_verdict']} | {value['replay_verdict']} | [HTML]({value['html_report']}) |")
    lines += ["", "验证包含：同目录重复执行产物一致性、独立目录结果一致性、Rust 输入与回放校验、修改输入/成交文件时拒绝生成报告，以及恢复文件后的再次验证。", "",
              "负向检查日志中的报错是预期拒绝，不是正常回测失败。正向步骤失败会以 failed 记录并返回非零退出码。", "",
              "报告中的收益来自短小示例数据，仅用于功能验证。手续费、资金曲线和成交事实均由 Rust 引擎产生，验证脚本不计算交易收益。", "",
              f"运行数据目录：`{result['work_dir']}`。详细证据见 [validation.json](validation.json)，命令输出在 `logs/`。", "",
              "全方案交付状态：尚未完成。完整服务端部署、实盘就绪判定、原子提交恢复和跨宿主验收不在本次回测通过结论内。"]
    if "archive" in result:
        lines += ["", "原始输入和引擎产物：[input-and-artifacts.zip](input-and-artifacts.zip)。ZIP 内的摘要保留原执行路径；换目录后请重新执行验证脚本生成匹配的新产物。"]
    for problem in result["problems"]:
        lines.append(f"\n- {problem}")
    (reports / "README.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True, help="fresh, short local data path")
    parser.add_argument("--report-dir", type=Path, required=True, help="fresh report destination")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    work, reports = args.work_dir.resolve(), args.report_dir.resolve()
    if work == reports or work in reports.parents or reports in work.parents:
        parser.error("work-dir and report-dir must be separate directories, not nested")
    if work.exists() or reports.exists():
        parser.error("work-dir and report-dir must not exist; previous evidence is preserved")
    work.mkdir(parents=True)
    reports.mkdir(parents=True)
    try:
        result = validate(binary, work, reports)
    except (Exception, SystemExit) as error:
        result = {"schema_version": 1, "status": "failed", "generated_at": datetime.now(timezone(timedelta(hours=8))).isoformat(),
                  "work_dir": str(work), "markets": {}, "problems": [str(error)]}
    if result["status"] == "passed":
        try:
            archive = reports / "input-and-artifacts.zip"
            with zipfile.ZipFile(archive, "x", zipfile.ZIP_DEFLATED) as bundle:
                for path in sorted(work.rglob("*")):
                    if path.is_file():
                        bundle.write(path, path.relative_to(work).as_posix())
            result["archive"] = {"path": archive.name, "sha256": acceptance.sha256_file(archive)}
        except Exception as error:
            result["status"] = "failed"
            result["problems"].append(f"evidence archive could not be completed: {error}")
    (reports / "validation.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    write_summary(reports, result)
    print(json.dumps({"status": result["status"], "report": str(reports / "README.md"), "problems": result["problems"]}, ensure_ascii=False))
    return 0 if result["status"] == "passed" else 2


if __name__ == "__main__":
    raise SystemExit(main())
