#!/usr/bin/env python3
"""Measure verified interactive same-input comparison HTML generation.

The workload freezes one synthetic crypto-spot BarFrame, creates distinct built-in SMA runs on
that exact input, then measures only the local interactive comparison-report command. It is not a
full product profile or a browser-rendering benchmark.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import statistics
import tempfile
import time
from pathlib import Path
from typing import Any

from hardware import hardware_fingerprint
from process_metrics import run_measured
from run_profile_backtest import (
    SCALE,
    find_binary,
    generate_frame,
    load_fixture,
)
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent
SMA_WINDOWS = ((2, 5), (3, 6), (4, 7), (5, 8), (2, 7), (3, 8), (4, 9), (5, 10))


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def percentile(values: list[float], percent: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, round(percent * (len(ordered) - 1)))
    return round(ordered[index], 3)


def write_runtime_config(identity: dict, workdir: Path, size: int, ordinal: int) -> Path:
    template = ROOT / "deploy" / "qianxing.runtime.builtin-strategy.example.json"
    config = json.loads(template.read_text(encoding="utf-8"))
    config["storage"]["data_dir"] = str(workdir / f"storage-{size}-{ordinal}")
    config["strategy"]["instrument"] = identity["instrument"]
    fast, slow = SMA_WINDOWS[ordinal]
    config["strategy"]["builtin_fast_window"] = fast
    config["strategy"]["builtin_slow_window"] = slow
    for worker in config["workers"]:
        if worker.get("role") == "strategy":
            worker["symbols"] = [identity["instrument"]]
    path = workdir / f"runtime-{size}-{ordinal}.json"
    path.write_text(json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8")
    return path


def run_backtest(binary: Path, frame: Path, market: Path, quantity: int, config: Path) -> Path:
    measured = run_measured(
        [
            str(binary), "backtest", "builtin", "sma_cross", str(frame), str(market),
            str(quantity), "--config", str(config),
        ],
        cwd=ROOT,
    )
    if measured.return_code != 0:
        raise RuntimeError(f"comparison setup backtest failed: {measured.stderr[-2000:]}")
    match = re.search(r"\[Artifacts\] summary=(\S+)", measured.stdout)
    if not match:
        raise RuntimeError(f"backtest output omitted summary path: {measured.stdout[-2000:]}")
    summary = Path(match.group(1)).resolve()
    if not summary.is_file():
        raise RuntimeError(f"comparison summary missing: {summary}")
    return summary


def measure_report(binary: Path, summaries: list[Path], output: Path) -> dict:
    command = [str(binary), "report", str(summaries[0])]
    for summary in summaries[1:]:
        command.extend(["--against", str(summary)])
    command.extend(["--json", "--html", "--output", str(output)])
    measured = run_measured(command, cwd=ROOT)
    if measured.return_code != 0:
        raise RuntimeError(f"interactive comparison report failed: {measured.stderr[-2000:]}")
    report = json.loads(measured.stdout)
    if len(report.get("runs", [])) != len(summaries):
        raise RuntimeError("interactive comparison report did not preserve every run")
    html = output.read_bytes()
    return {
        "wall_ms": round(measured.elapsed_ms, 3),
        "peak_rss_bytes": measured.peak_rss_bytes,
        "process_tree_peak_rss_bytes": measured.peak_tree_rss_bytes,
        "peak_process_count": measured.peak_process_count,
        "process_tree_cpu_seconds": measured.tree_cpu_seconds,
        "process_tree_io_read_bytes": measured.tree_io_read_bytes,
        "process_tree_io_write_bytes": measured.tree_io_write_bytes,
        "output_bytes": len(html),
        "output_sha256": sha256_bytes(html),
        "reported_run_count": len(report["runs"]),
    }


def measure_browser(browser: Any, output: Path, expected_runs: int, hover_events: int = 100) -> dict:
    """Measure an actual Chromium page load and its local chart interactions."""
    page = browser.new_page(viewport={"width": 1440, "height": 1000}, device_scale_factor=1)
    page_errors: list[str] = []
    external_requests: list[str] = []
    page.on("pageerror", lambda error: page_errors.append(str(error)))
    page.on(
        "request",
        lambda request: external_requests.append(request.url)
        if not request.url.startswith("file:")
        else None,
    )
    started = time.perf_counter()
    try:
        page.goto(output.resolve().as_uri(), wait_until="load", timeout=30_000)
        page.wait_for_function(
            "document.querySelector('#qx-equity-chart')?._chart?.selected?.length > 0",
            timeout=10_000,
        )
        load_ms = (time.perf_counter() - started) * 1000
        chart_state = page.evaluate(
            """() => ({
                selectedRuns: document.querySelector('#qx-equity-chart')._chart.selected.length,
                loadEventMs: performance.getEntriesByType('navigation')[0]?.loadEventEnd ?? null,
                visibleRows: [...document.querySelectorAll('#qx-runs tbody tr')].filter(row => !row.hidden).length,
            })"""
        )
        if chart_state["selectedRuns"] != expected_runs:
            raise RuntimeError(
                f"browser rendered {chart_state['selectedRuns']} curves; expected {expected_runs}"
            )

        search = page.locator("#qx-search")
        search.fill("__qx_no_matching_run__")
        empty_visible = page.locator("#qx-empty").is_visible()
        search.fill("")
        rows_restored = page.locator("#qx-visible-count").inner_text()
        canvas = page.locator("#qx-equity-chart")
        bounds = canvas.bounding_box()
        if bounds is None:
            raise RuntimeError("browser did not expose the interactive chart canvas")
        page.mouse.move(bounds["x"] + bounds["width"] * 0.55, bounds["y"] + bounds["height"] * 0.5)
        tooltip_visible = page.locator("#qx-equity-tooltip").is_visible()
        tooltip_text = page.locator("#qx-equity-tooltip").inner_text() if tooltip_visible else ""
        hover = page.evaluate(
            """({eventCount}) => {
                const canvas = document.querySelector('#qx-equity-chart');
                const bounds = canvas.getBoundingClientRect();
                const samples = [];
                for (let index = 0; index < eventCount; index++) {
                    const x = bounds.left + 60 + (index % 80) / 80 * Math.max(1, bounds.width - 80);
                    const y = bounds.top + bounds.height / 2;
                    const start = performance.now();
                    canvas.dispatchEvent(new PointerEvent('pointermove', {
                        clientX: x, clientY: y, bubbles: true, pointerType: 'mouse'
                    }));
                    samples.push(performance.now() - start);
                }
                const sorted = [...samples].sort((a, b) => a - b);
                const percentile = p => sorted[Math.min(sorted.length - 1, Math.round(p * (sorted.length - 1)))];
                return {p50Ms: percentile(0.5), p95Ms: percentile(0.95), eventCount: samples.length};
            }""",
            {"eventCount": hover_events},
        )
        if not empty_visible or "显示" not in rows_restored or not tooltip_visible or not tooltip_text:
            raise RuntimeError("browser interaction checks failed (filter or chart tooltip)")
        if page_errors or external_requests:
            raise RuntimeError(
                f"offline report emitted browser errors or external requests: "
                f"errors={page_errors[:3]}, requests={external_requests[:3]}"
            )
        return {
            "browser": "chromium",
            "browser_version": browser.version,
            "initial_load_to_chart_ready_ms": round(load_ms, 3),
            "navigation_load_event_ms": round(chart_state["loadEventMs"], 3)
            if chart_state["loadEventMs"] is not None
            else None,
            "rendered_curve_count": chart_state["selectedRuns"],
            "visible_run_count_after_filter_reset": rows_restored,
            "hover_event_count": hover["eventCount"],
            "hover_handler_p50_ms": round(hover["p50Ms"], 4),
            "hover_handler_p95_ms": round(hover["p95Ms"], 4),
            "filter_interaction_passed": empty_visible,
            "pointer_tooltip_interaction_passed": tooltip_visible,
            "page_errors": page_errors,
            "external_requests": external_requests,
        }
    finally:
        page.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--sizes", default="64000")
    parser.add_argument("--run-counts", default="2,4,8")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument(
        "--measure-browser",
        action="store_true",
        help="打开本机 headless Chromium，测量实际图表首次呈现、筛选与悬停处理耗时",
    )
    parser.add_argument("--hover-events", type=int, default=100)
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT / "maturity" / "evidence" / "interactive-comparison-capacity-2026-10-23.json",
    )
    args = parser.parse_args()
    sizes = [int(value) for value in args.sizes.split(",") if value.strip()]
    run_counts = [int(value) for value in args.run_counts.split(",") if value.strip()]
    if not sizes or any(value < 3 for value in sizes) or len(sizes) != len(set(sizes)):
        parser.error("--sizes 必须为互不重复且至少 3 的正整数")
    if not run_counts or any(value < 2 or value > len(SMA_WINDOWS) for value in run_counts):
        parser.error(f"--run-counts 必须落在 2..={len(SMA_WINDOWS)}")
    if len(run_counts) != len(set(run_counts)):
        parser.error("--run-counts 不能重复")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2")
    if args.hover_events < 10 or args.hover_events > 10_000:
        parser.error("--hover-events 必须落在 10..=10000")

    binary = find_binary(args.binary)
    fixture = "crypto-spot"
    spec, identity = load_fixture(fixture)
    quantity = max(1, (int(spec["min_qty"]) + SCALE - 1) // SCALE)
    evidence = {
        "schema_version": 1,
        "evidence_kind": "verified_interactive_comparison_report_capacity",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": sha256_file(binary),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "benchmark_script_sha256": sha256_file(Path(__file__).resolve()),
        "scenario": "qx-cli report --against verified same-input summaries with interactive local HTML equity chart, filtering and ranking",
        "input_is_shared_and_fingerprint_verified": True,
        "browser_rendering_measured": args.measure_browser,
        "browser_interaction_metrics": "chromium page load, filter, and pointer handler latency"
        if args.measure_browser
        else None,
        "profile_coverage_claim": False,
        "profile_target_statuses_updated": False,
        "limitations": [
            "synthetic crypto spot Bar input and built-in SMA strategies",
            "browser rendering and pointer interaction are measured only when --measure-browser is set",
            "each run contributes at most 2000 sampled equity points to the interactive chart",
            "does not measure multi-market/depth reports, long-running stability, or full product profiles",
        ],
        "workloads": [],
    }
    playwright = None
    browser = None
    if args.measure_browser:
        try:
            from playwright.sync_api import sync_playwright

            playwright = sync_playwright().start()
            browser = playwright.chromium.launch(headless=True)
        except Exception as error:
            if playwright is not None:
                playwright.stop()
            raise RuntimeError(
                "无法启动 Chromium 浏览器测量；安装 Playwright Chromium 后重试，"
                "不要把未执行的浏览器测量记录为已测"
            ) from error
    with tempfile.TemporaryDirectory(prefix="qx-interactive-compare-", dir=ROOT / "target") as temp:
        workdir = Path(temp)
        market = workdir / "market.json"
        market.write_text(json.dumps(spec, separators=(",", ":")), encoding="utf-8")
        for size in sizes:
            frame = workdir / f"crypto-spot-{size}.bars.json"
            generate_frame(size, fixture, spec, identity, frame)
            summaries: list[Path] = []
            for ordinal in range(max(run_counts)):
                config = write_runtime_config(identity, workdir, size, ordinal)
                summaries.append(run_backtest(binary, frame, market, quantity, config))
            fingerprints = {
                json.loads(summary.read_text(encoding="utf-8"))["input"]["fingerprint"]
                for summary in summaries
            }
            if len(fingerprints) != 1:
                raise RuntimeError("comparison setup runs do not share the same verified input fingerprint")
            for count in run_counts:
                selected = summaries[:count]
                output = workdir / f"interactive-{size}-{count}.html"
                samples = [measure_report(binary, selected, output) for _ in range(args.repeats)]
                browser_samples = (
                    [measure_browser(browser, output, count, args.hover_events) for _ in range(args.repeats)]
                    if browser is not None
                    else []
                )
                hashes = {sample["output_sha256"] for sample in samples}
                if len(hashes) != 1:
                    raise RuntimeError(f"interactive report output changed across repeats: bars={size}, runs={count}")
                walls = [sample["wall_ms"] for sample in samples]
                trees = [sample["process_tree_peak_rss_bytes"] for sample in samples if sample["process_tree_peak_rss_bytes"] is not None]
                cpus = [sample["process_tree_cpu_seconds"] for sample in samples if sample["process_tree_cpu_seconds"] is not None]
                summary_source_bytes = sum(summary.stat().st_size for summary in selected)
                equity_source_bytes = sum(
                    summary.with_name(
                        summary.name.removesuffix(".summary.json") + ".equity.csv"
                    ).stat().st_size
                    for summary in selected
                )
                evidence["workloads"].append(
                    {
                        "bars_per_run": size,
                        "compared_runs": count,
                        "repeats": args.repeats,
                        "input_fingerprint": next(iter(fingerprints)),
                        "summary_source_bytes": summary_source_bytes,
                        "equity_source_bytes": equity_source_bytes,
                        "report_input_source_bytes": summary_source_bytes + equity_source_bytes,
                        "equity_points_upper_bound": count * 2000,
                        "wall_ms_p50": round(statistics.median(walls), 3),
                        "wall_ms_p95": percentile(walls, 0.95),
                        "process_tree_peak_rss_bytes_max": max(trees) if trees else None,
                        "process_tree_cpu_seconds_p50": round(statistics.median(cpus), 3) if cpus else None,
                        "output_bytes": samples[0]["output_bytes"],
                        "output_sha256": samples[0]["output_sha256"],
                        "reported_run_count": samples[0]["reported_run_count"],
                        "browser": {
                            "initial_load_to_chart_ready_ms_p50": round(
                                statistics.median(
                                    sample["initial_load_to_chart_ready_ms"] for sample in browser_samples
                                ),
                                3,
                            ),
                            "initial_load_to_chart_ready_ms_p95": percentile(
                                [sample["initial_load_to_chart_ready_ms"] for sample in browser_samples], 0.95
                            ),
                            "hover_handler_p50_ms_p50": round(
                                statistics.median(sample["hover_handler_p50_ms"] for sample in browser_samples), 4
                            ),
                            "hover_handler_p95_ms_p95": percentile(
                                [sample["hover_handler_p95_ms"] for sample in browser_samples], 0.95
                            ),
                            "filter_checks_passed": all(
                                sample["filter_interaction_passed"] for sample in browser_samples
                            ),
                            "tooltip_checks_passed": all(
                                sample["pointer_tooltip_interaction_passed"] for sample in browser_samples
                            ),
                            "external_request_count": sum(
                                len(sample["external_requests"]) for sample in browser_samples
                            ),
                            "page_error_count": sum(len(sample["page_errors"]) for sample in browser_samples),
                            "samples": browser_samples,
                        }
                        if browser_samples
                        else None,
                    }
                )
    if browser is not None:
        browser.close()
    if playwright is not None:
        playwright.stop()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(evidence, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
