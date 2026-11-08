"""Verified HS300 index research; requires real CSV and an independent calendar.

Never substitutes fixtures, interpolates missing sessions, or represents the
synthetic index position as a tradable stock/ETF. Financial results come from
qx-cli. Run with --help for the explicit data and experiment parameters.
"""
from __future__ import annotations

import argparse
import csv
from datetime import date, datetime, time, timedelta, timezone
from decimal import Decimal, InvalidOperation, localcontext
import hashlib
import html
import importlib.util
import json
import platform
from pathlib import Path
import shutil
import subprocess
import sys
import zipfile

import backtest_acceptance as acceptance

SCALE = 10**9
SHANGHAI = timezone(timedelta(hours=8))
REPO = Path(__file__).resolve().parents[1]


def save(path, value):
    Path(path).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def _file_identity(path: Path) -> dict[str, int | str]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
            size += len(chunk)
    return {"sha256": digest.hexdigest(), "bytes": size}


def _create_evidence_archive(
    work: Path, reports: Path, archive_path: Path, run_identity: dict,
) -> tuple[str, str]:
    """Pack frozen inputs and generated reports with a digest manifest and verify the ZIP."""
    members: dict[str, Path] = {}
    manifest_path = reports / "archive-manifest.json"
    for prefix, root in (("inputs", work), ("reports", reports)):
        for path in sorted(root.rglob("*")):
            if not path.is_file() or path.resolve() in {archive_path.resolve(), manifest_path.resolve()}:
                continue
            members[f"{prefix}/{path.relative_to(root).as_posix()}"] = path
    file_manifest = {
        name: {"sha256": identity["sha256"], "bytes": identity["bytes"]}
        for name, path in sorted(members.items())
        for identity in [_file_identity(path)]
    }
    manifest = {
        "schema_version": 1,
        "kind": "qianxing-hs300-run-archive",
        "run_identity": run_identity,
        "members": file_manifest,
    }
    save(manifest_path, manifest)
    archive_members = {**members, "reports/archive-manifest.json": manifest_path}
    archive_path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive_path, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for name, path in sorted(archive_members.items()):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            archive.writestr(info, path.read_bytes(), compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
    with zipfile.ZipFile(archive_path) as archive:
        decoded = json.loads(archive.read("reports/archive-manifest.json"))
        if decoded != manifest:
            raise RuntimeError("archive manifest changed during ZIP creation")
        if set(archive.namelist()) != set(archive_members):
            raise RuntimeError("evidence archive member list does not match its manifest")
        for name, expected in file_manifest.items():
            payload = archive.read(name)
            if len(payload) != expected["bytes"] or hashlib.sha256(payload).hexdigest() != expected["sha256"]:
                raise RuntimeError(f"evidence archive member digest mismatch: {name}")
    return _file_identity(archive_path)["sha256"], _file_identity(manifest_path)["sha256"]


def parse_date(value):
    result = date.fromisoformat(value)
    if result.isoformat() != value:
        raise ValueError(f"date must be YYYY-MM-DD: {value!r}")
    return result


def raw(value, field):
    try:
        number = Decimal(value)
        with localcontext() as context:
            context.prec = max(80, len(value) + 12)
            scaled = number * SCALE
            if not number.is_finite() or scaled != scaled.to_integral_value() or abs(scaled) >= 2**127:
                raise ValueError()
            result = int(scaled)
    except (InvalidOperation, ValueError, OverflowError) as error:
        raise ValueError(f"{field}: invalid lossless 1e-9 decimal {value!r}") from error
    if result < 0 or (field != "volume" and result == 0):
        raise ValueError(f"{field}: price must be positive and volume nonnegative")
    return result


def load_history(path, calendar_path, start, end, slow, source):
    if not source.strip() or start > end:
        raise ValueError("source is required and start must not exceed end")
    calendar = json.loads(calendar_path.read_text(encoding="utf-8"))
    if not calendar.get("source", "").strip() or calendar.get("schema_version") != 1:
        raise ValueError("independent calendar requires schema_version=1 and source")
    days = [parse_date(day) for day in calendar["trading_days"]]
    if not days or days != sorted(set(days)) or any(day.weekday() >= 5 for day in days):
        raise ValueError("calendar trading_days must be unique ascending weekdays")
    # A coverage statement is necessary even when the requested end is a holiday.
    covered = calendar.get("coverage", {})
    if parse_date(covered.get("start", "")) > start or parse_date(covered.get("end", "")) < end:
        raise ValueError("calendar coverage does not cover the requested range")
    expected = [day for day in days if start <= day <= end]
    prefix = [day for day in days if day < start][-slow:]
    if len(prefix) < slow or not expected:
        raise ValueError("calendar lacks warmup sessions or requested trading days")
    required = prefix + expected
    rows = {}
    with path.open(encoding="utf-8-sig", newline="") as stream:
        reader = csv.DictReader(stream)
        if not {"date", "open", "high", "low", "close", "volume"} <= set(reader.fieldnames or ()):
            raise ValueError("CSV requires date,open,high,low,close,volume")
        previous = None
        for line, record in enumerate(reader, 2):
            day = parse_date(record["date"])
            if previous is not None and day <= previous:
                raise ValueError(f"CSV line {line}: duplicate or nonascending date")
            previous = day
            if day < required[0] or day > end:
                continue
            values = {field: raw(record[field], field) for field in ("open", "high", "low", "close", "volume")}
            if values["low"] > min(values["open"], values["close"]) or values["high"] < max(values["open"], values["close"]):
                raise ValueError(f"CSV line {line}: inconsistent OHLC envelope")
            rows[day] = values
    missing, extra = sorted(set(required) - rows.keys()), sorted(rows.keys() - set(required))
    if missing or extra:
        raise ValueError(f"calendar/CSV mismatch; missing={list(map(str, missing[:12]))}, unexpected={list(map(str, extra[:12]))}")
    frame = {"instrument": "000300.SSE", "source": source,
             "ts": [int(datetime.combine(day, time(15), SHANGHAI).timestamp() * 1000) for day in required]}
    for field in ("open", "high", "low", "close", "volume"):
        frame[field + "_raw"] = [rows[day][field] for day in required]
    quality = {"source": source, "csv_sha256": acceptance.sha256_file(path),
               "calendar_sha256": acceptance.sha256_file(calendar_path), "calendar_source": calendar["source"],
               "requested_start": str(start), "requested_end": str(end),
               "actual_first_session": str(expected[0]), "actual_last_session": str(expected[-1]),
               "warmup_sessions": len(prefix), "evaluation_sessions": len(expected),
               "missing_sessions": 0, "duplicate_sessions": 0, "interpolated_rows": 0,
               "source_authenticity": "user_supplied_not_independently_price_verified"}
    return frame, quality


def prepare(work, frame, start, fast, slow, cash, exposure, fee):
    save(work / "bars.json", frame)
    costs = {"name": "synthetic-index-constant-assumption", "maker_bp": fee, "taker_bp": fee,
             "latency_base_ns": 0, "latency_insert_ns": 0}
    save(work / "costs.json", costs)
    spec = json.loads((REPO / "deploy/qianxing.ashare.spot.spec.json").read_text(encoding="utf-8"))
    spec.update(base="000300", price_tick_raw=100000, qty_step_raw=SCALE, min_qty_raw=SCALE)
    save(work / "spec.json", spec)
    config = json.loads((REPO / "deploy/qianxing.runtime.strategy-backtest.example.json").read_text(encoding="utf-8"))
    config["storage"]["data_dir"] = str(work / "data")
    strategy = config["strategy"]
    strategy.update(id="long-only-sma", version="1.0.0", instrument="000300.SSE",
                    python_module="qianxing_strategy.long_only_sma", python_timeout_ms=30000,
                    initial_cash_raw=cash, max_orders=len(frame["ts"]) * 2,
                    fill_model="next_bar_open", cost_rules_path=str(work / "costs.json"),
                    python_strategy_spec={"data_level": "bar", "parameters": {
                        "fast_window": fast, "slow_window": slow, "exposure_bps": exposure,
                        "start_ts": int(datetime.combine(start, time(), SHANGHAI).timestamp() * 1000),
                        "lot_raw": SCALE}})
    worker = config["workers"][1]
    worker.update(id="long-only-sma", symbols=["000300.SSE"])
    save(work / "runtime.json", config)
    return work / "runtime.json"


def execute(binary, args, work, reports, label):
    result = subprocess.run([str(binary), *map(str, args)], cwd=work, capture_output=True,
                            encoding="utf-8", errors="replace", timeout=1800)
    (reports / f"{label}.log").write_text(result.stdout + "\n" + result.stderr, encoding="utf-8")
    if result.returncode:
        raise RuntimeError(f"{label}: qx-cli exit {result.returncode}; inspect {reports / (label + '.log')}")
    return result.stdout


def _load_frozen_acceptance(work: Path):
    original = acceptance
    source = Path(original.__file__).resolve(strict=True)
    snapshot = work / "backtest_acceptance.py"
    shutil.copyfile(source, snapshot)
    digest = _file_identity(snapshot)["sha256"]
    spec = importlib.util.spec_from_file_location("qianxing_hs300_frozen_acceptance", snapshot)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load frozen backtest acceptance helper")
    frozen = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = frozen
    spec.loader.exec_module(frozen)
    # Preserve repository-owned data roots while executing the frozen helper bytes.
    for name in ("WORKSPACE", "RECORD", "ASHARE_EXAMPLE_RECORD"):
        if hasattr(original, name):
            setattr(frozen, name, getattr(original, name))
    return frozen, digest


def _python_runtime_identity(work: Path, runner_sha256: str, helper_sha256: str) -> dict:
    executable = Path(sys.executable).resolve(strict=True)
    identity = {
        "executable": str(executable),
        "executable_sha256": _file_identity(executable)["sha256"],
        "version": sys.version,
        "implementation": sys.implementation.name,
        "cache_tag": sys.implementation.cache_tag,
        "platform": platform.platform(),
        "runner_sha256": runner_sha256,
        "acceptance_helper_sha256": helper_sha256,
    }
    save(work / "python-runtime.json", identity)
    return identity


def _run_impl(args, *, directories_ready=False, runner_sha256=None):
    reports, work = args.report_dir.resolve(), args.work_dir.resolve()
    if reports == work or reports in work.parents or work in reports.parents:
        raise ValueError("work-dir and report-dir must be separate, nonnested directories")
    if not directories_ready and (reports.exists() or work.exists()):
        raise ValueError("fresh work-dir and report-dir required; previous evidence is preserved")
    reports.mkdir(parents=True, exist_ok=directories_ready)
    work.mkdir(parents=True, exist_ok=directories_ready)
    status = {"schema_version": 1, "status": "blocked", "requested_start": str(args.start),
              "requested_end": str(args.end), "experiment": "HS300 index long-only SMA research",
              "fast_window": args.fast, "slow_window": args.slow, "market": "synthetic_index_research",
              "performance_metrics": None}
    frozen_acceptance, helper_sha256 = _load_frozen_acceptance(work)
    globals()["acceptance"] = frozen_acceptance
    if runner_sha256 is None:
        runner_sha256 = _file_identity(Path(__file__).resolve())["sha256"]
    runtime_identity = _python_runtime_identity(work, runner_sha256, helper_sha256)
    runtime_sha256 = hashlib.sha256(json.dumps(runtime_identity, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()
    status.update(runner_sha256=runner_sha256, acceptance_helper_sha256=helper_sha256,
                  python_runtime_sha256=runtime_sha256, python_runtime=runtime_identity)
    try:
        if not 1 <= args.fast < args.slow <= 9999 or not 1 <= args.exposure_bps <= 10000 or not 0 <= args.fee_bps <= 10000:
            raise ValueError("invalid SMA windows, exposure or fee parameters")
        cash = raw(args.cash, "initial_cash")
        # Freeze caller inputs before validation; the backtest and archive both use these bytes.
        source_snapshot = work / "source.csv"
        calendar_snapshot = work / "calendar-source.json"
        shutil.copyfile(args.csv.resolve(strict=True), source_snapshot)
        shutil.copyfile(args.calendar.resolve(strict=True), calendar_snapshot)
        strategy_source = REPO / "python/qianxing_strategy/long_only_sma.py"
        strategy_snapshot = work / "executed-strategy.py"
        shutil.copyfile(strategy_source, strategy_snapshot)
        if _file_identity(work / "runner.py")["sha256"] != runner_sha256:
            raise RuntimeError("frozen runner digest changed before execution")
        frame, quality = load_history(source_snapshot, calendar_snapshot,
                                      args.start, args.end, args.slow, args.source)
        quality["strategy_sha256"] = acceptance.sha256_file(strategy_snapshot)
        quality["runner_sha256"] = runner_sha256
        quality["acceptance_helper_sha256"] = helper_sha256
        quality["python_runtime_sha256"] = runtime_sha256
        quality["execution_binary_sha256"] = acceptance.sha256_file(args.binary.resolve(strict=True))
        binary = args.binary.resolve(strict=True)
        runtime = prepare(work, frame, args.start, args.fast, args.slow, cash, args.exposure_bps, args.fee_bps)
        save(reports / "data-quality.json", quality)
        snapshots = []
        for attempt in (1, 2):
            output = execute(binary, ["backtest", runtime, work / "bars.json", work / "spec.json"],
                             work, reports, f"backtest-{attempt}")
            snapshots.append(acceptance.snapshot_from_output(output, work))
        problems = acceptance.compare_reruns({"label": "hs300", "first": snapshots[0], "second": snapshots[1]})
        if problems:
            raise RuntimeError("; ".join(problems))
        document = json.loads(execute(binary, ["report", runtime, "--json"], work, reports, "verification"))
        if document["input_check"]["verdict"] != "verified" or snapshots[0]["replay_verdict"] != "verified":
            raise RuntimeError("Rust input/replay verification did not pass")
        summary = Path(document["summary_path"])
        execute(binary, ["report", summary, "--html", "--xlsx", reports / "hs300-sma.xlsx",
                         "-o", reports / "engine-report.html", "--evidence"], work, reports, "report")
        save(reports / "engine-report.json", document)
        run_identity = {
            "run_id": document["summary"].get("run_id"),
            "result_hash": document["summary"]["result_hash"],
            "summary_sha256": _file_identity(summary)["sha256"],
            "strategy_sha256": quality["strategy_sha256"],
            "runner_sha256": quality["runner_sha256"],
            "acceptance_helper_sha256": quality["acceptance_helper_sha256"],
            "python_runtime_sha256": runtime_sha256,
            "execution_binary_sha256": quality["execution_binary_sha256"],
            "input_csv_sha256": quality["csv_sha256"],
            "calendar_sha256": quality["calendar_sha256"],
        }
        if not run_identity["run_id"]:
            raise RuntimeError("verified report summary is missing run_id")
        archive_sha256, archive_manifest_sha256 = _create_evidence_archive(
            work, reports, reports / "inputs-and-artifacts.zip", run_identity,
        )
        status.update(status="completed", data_quality=quality, performance_metrics=document["summary"]["metrics"],
                      metrics_exact=document["summary"].get("metrics_exact"), result_hash=document["summary"]["result_hash"],
                      repeatability=snapshots, run_identity=run_identity,
                      binary_sha256=acceptance.sha256_file(binary), archive_sha256=archive_sha256,
                      archive_manifest_sha256=archive_manifest_sha256)
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.TimeoutExpired, SystemExit) as error:
        status["reason"] = str(error)
    save(reports / "status.json", status)
    assumptions = (f"日线 SMA{args.fast}/{args.slow}；金叉买入、死叉清仓、禁止做空；可用资金的{args.exposure_bps / 100:.2f}%；"
                   f"次Bar开盘成交；双边恒定费用{args.fee_bps}bp为研究假设；无额外滑点。"
                   "指数头寸为合成研究单位，不是股票或ETF，不代表实际可交易收益。预热段计入引擎样本范围，"
                   "交易起点由start_ts限制；期末按市价估值，不强制清仓。完整交易规则、跟踪误差和历史分段税费不在此模型内。")
    if status["status"] == "completed":
        outcome = '<a href="engine-report.html">引擎完整报告</a> · <a href="hs300-sma.xlsx">Excel明细</a> · <a href="inputs-and-artifacts.zip">原始数据与证据</a>'
    else:
        outcome = "<strong>回测未执行完成，无可发布的绩效结果。</strong><pre>" + html.escape(status["reason"]) + "</pre>"
    page = ('<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
            '<title>沪深300双均线回测</title><style>body{font:16px/1.7 system-ui;margin:24px auto;padding:0 16px;max-width:1080px}pre{white-space:pre-wrap;overflow-wrap:anywhere}iframe{width:100%;height:85vh;border:1px solid #ccc}</style>'
            f'<h1>沪深300双均线回测</h1><p>请求范围：{args.start} 至 {args.end}；状态：{status["status"]}</p><p>{html.escape(assumptions)}</p><p>{outcome}</p>')
    if status["status"] == "completed":
        page += '<p>实际覆盖与数据质量见 <a href="data-quality.json">数据校验记录</a>；引擎指标包含预热段，不应当作严格裁剪至交易起点的年化绩效。</p><iframe title="Rust验证回测报告" src="engine-report.html"></iframe>'
    (reports / "report.html").write_text(page + '</html>', encoding="utf-8")
    return status


def run(args):
    reports, work = args.report_dir.resolve(), args.work_dir.resolve()
    if reports == work or reports in work.parents or work in reports.parents:
        raise ValueError("work-dir and report-dir must be separate, nonnested directories")
    if reports.exists() or work.exists():
        raise ValueError("fresh work-dir and report-dir required; previous evidence is preserved")
    reports.mkdir(parents=True)
    work.mkdir(parents=True)
    source = Path(__file__).resolve(strict=True)
    runner_snapshot = work / "runner.py"
    shutil.copyfile(source, runner_snapshot)
    runner_sha256 = _file_identity(runner_snapshot)["sha256"]
    spec = importlib.util.spec_from_file_location("qianxing_hs300_frozen_runner", runner_snapshot)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load frozen HS300 runner")
    frozen = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = frozen
    spec.loader.exec_module(frozen)
    frozen.REPO = REPO
    return frozen._run_impl(args, directories_ready=True, runner_sha256=runner_sha256)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csv", type=Path, required=True)
    parser.add_argument("--calendar", type=Path, required=True)
    parser.add_argument("--source", required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--report-dir", type=Path, required=True)
    parser.add_argument("--start", type=parse_date, default=date(2010, 1, 1))
    parser.add_argument("--end", type=parse_date, default=datetime.now(SHANGHAI).date())
    parser.add_argument("--fast", type=int, default=20)
    parser.add_argument("--slow", type=int, default=60)
    parser.add_argument("--cash", default="1000000")
    parser.add_argument("--exposure-bps", type=int, default=9500)
    parser.add_argument("--fee-bps", type=int, default=3)
    args = parser.parse_args()
    try:
        status = run(args)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    print(json.dumps({"status": status["status"], "report": str(args.report_dir / "report.html"), "reason": status.get("reason")}, ensure_ascii=False))
    return 0 if status["status"] == "completed" else 2


if __name__ == "__main__":
    sys.exit(main())
