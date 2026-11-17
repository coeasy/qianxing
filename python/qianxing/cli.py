"""Console entry point for the Python SDK's shared Rust application workflows."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from dataclasses import asdict, is_dataclass
from pathlib import Path
from typing import Any, Callable, Mapping, Sequence

from .app import (
    DataUnavailableError,
    InvalidInputError,
    QianxingError,
    compare_runs,
    doctor,
    run_backtest,
    run_backtest_with_strategy,
    start_backtest_with_strategy,
    start_depth_backtest_with_strategy,
    start_multi_market_backtest_with_strategy_slots,
    run_depth_backtest,
    run_depth_backtest_with_strategy,
    run_depth_experiment,
    run_depth_experiment_with_strategy,
    run_experiment,
    run_experiment_with_strategy,
    _prepare_quick_research_spec as prepare_quick_research_spec,
    run_multi_leg_experiment,
    run_multi_market_backtest_with_strategy_slots,
    run_multi_market_experiment,
    run_multi_market_experiment_with_strategy,
    read_run_report_v1,
    validate_dataset,
    validate_strategy_execution_preflight_v1,
    verify_run,
    verify_depth_run,
)
from .experiment_summary import summarize_experiment as _summarize_experiment
from .experiment_strategy import build_multi_market_experiment_factory as _build_multi_market_experiment_factory
from .experiment_diagnostics import (
    _candidate_diagnostic_spec as _build_candidate_diagnostic_spec,
    _experiment_candidate_rows as _build_experiment_candidate_rows,
    diagnose_experiment_candidates as _run_experiment_candidate_diagnostics,
)
from .strategy_diagnostics import (
    _multi_market_diagnostic_request,
    _referenced_input_paths,
    _strategy_diagnostic_request,
)
from .reports.experiment_html import (
    load_verified_candidate_details,
    load_verified_multi_market_candidate_details,
    write_experiment_html,
)
from .strategy_manifest import (
    apply_parameter_defaults,
    build_module_manifest,
    declared_config_validator,
    resolve_strategy_parameters,
)
from .strategy_project import (
    init_strategy_project,
    load_strategy_file,
    load_strategy_project,
    local_project_preflight,
    server_project_plan,
    project_deployments,
    show_strategy_project,
    static_project_doctor,
    dynamic_project_doctor,
    validate_strategy_config,
)
from .strategy_execution import (
    _bind_manifest_config_factory as _bind_execution_manifest_config_factory,
    _bind_warmup_factory,
    _strategy_identity as _resolve_strategy_identity,
    run_project_strategy,
    run_strategy_file,
)
from .project_runs import (run_project, wait_project_run, record_project_result,
                           project_runs, project_run_status, cancel_project_run, project_run_events,
                           record_project_error, record_project_plan,
                           prepare_project_input, snapshot_project_input, snapshot_project_strategy,
                           verify_project_artifacts, generate_project_report)


def _document(value: object) -> object:
    return asdict(value) if is_dataclass(value) else value








def _write_protected_json(path: Path, document: object, protected_paths: Sequence[str | Path]) -> Path:
    target = path.expanduser().resolve(strict=False)
    target_key = os.path.normcase(str(target))
    for source in protected_paths:
        source_key = os.path.normcase(str(Path(source).expanduser().resolve(strict=False)))
        if source_key == target_key:
            message = f"diagnostic output would overwrite an input file: {source}"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    target.parent.mkdir(parents=True, exist_ok=True)
    encoded = json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="\n",
            dir=target.parent,
            prefix=f".{target.name}.",
            suffix=".tmp",
            delete=False,
        ) as handle:
            temporary = Path(handle.name)
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, target)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()
    return target


def _read_json(path: str) -> object:
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except OSError as exc:
        message = f"cannot read JSON input {path!r}: {exc}"
        raise DataUnavailableError(
            message, payload={"category": "DATA_UNAVAILABLE", "message": message}
        ) from exc
    except json.JSONDecodeError as exc:
        message = f"invalid JSON input {path!r}: {exc}"
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        ) from exc


def _load_strategy_file(path: str):
    """Compatibility wrapper for project-service strategy loading."""
    return load_strategy_file(path)

def _strategy_identity(
    args: argparse.Namespace,
    module: object,
    strategy_file: str,
    payload: dict,
    strategy_manifest: Mapping[str, Any] | None = None,
) -> tuple[str, str]:
    return _resolve_strategy_identity(
        module,
        strategy_file,
        payload,
        strategy_manifest or {},
        strategy_id=args.strategy_id,
        strategy_version=args.strategy_version,
    )


def _bind_manifest_warmup_factory(
    factory: Callable[..., object], warmup_bars: int
) -> Callable[..., object]:
    return _bind_warmup_factory(factory, warmup_bars)


def _validate_strategy_config(
    module: object,
    manifest: Mapping[str, Any],
    execution_spec: Mapping[str, Any],
    target_strategy_id: str | None,
) -> None:
    """Compatibility wrapper for project-service strategy validation."""
    return validate_strategy_config(module, manifest, execution_spec, target_strategy_id)


def _bind_manifest_config_factory(
    factory: Callable[..., object],
    module: object,
    manifest: Mapping[str, Any],
    target_strategy_id: str | None,
) -> Callable[..., object]:
    return _bind_execution_manifest_config_factory(
        factory, module, manifest, target_strategy_id
    )


def _experiment_summary(result: object) -> dict[str, object]:
    """Compatibility facade for the experiment summary projection."""
    return _summarize_experiment(result)


def _multi_market_experiment_factory(
    experiment_spec: dict,
    factory: Callable[[dict], object],
    strategy_id: str,
) -> Callable[[dict], object]:
    """Compatibility facade for multi-market candidate strategy binding."""
    return _build_multi_market_experiment_factory(experiment_spec, factory, strategy_id)


def _experiment_candidate_rows(document: Mapping[str, Any]) -> list[dict[str, Any]]:
    return _build_experiment_candidate_rows(document)


def _candidate_diagnostic_spec(
    base_spec: Mapping[str, Any],
    candidate: Mapping[str, Any],
    experiment_spec: Mapping[str, Any],
    strategy_id: str,
) -> tuple[dict[str, Any], dict[str, Any] | None]:
    return _build_candidate_diagnostic_spec(base_spec, candidate, experiment_spec, strategy_id)


def _diagnose_experiment_candidates(
    experiment_spec: Mapping[str, Any],
    experiment_document: Mapping[str, Any],
    strategy_file: str | Path,
    *,
    strategy_id: str,
    strategy_version: str,
    strategy_bundle_sha256: str,
    startup_windows: Sequence[int],
    max_checks: int,
    timeout_seconds: int,
    candidate_limit: int | None = None,
) -> dict[str, Any]:
    return _run_experiment_candidate_diagnostics(
        experiment_spec,
        experiment_document,
        strategy_file,
        strategy_id=strategy_id,
        strategy_version=strategy_version,
        strategy_bundle_sha256=strategy_bundle_sha256,
        startup_windows=startup_windows,
        max_checks=max_checks,
        timeout_seconds=timeout_seconds,
        candidate_limit=candidate_limit,
        candidate_rows_builder=_experiment_candidate_rows,
        candidate_spec_builder=_candidate_diagnostic_spec,
        multi_market_request_builder=_multi_market_diagnostic_request,
        strategy_request_builder=_strategy_diagnostic_request,
    )


def _configure_console_encoding() -> None:
    """Keep JSON output valid on Windows terminals whose default code page is legacy."""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if callable(reconfigure):
            try:
                reconfigure(encoding="utf-8", errors="backslashreplace")
            except (OSError, ValueError):
                pass


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="qianxing",
        description="牵星 Python SDK command line (shared Rust research workflows).",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("doctor", help="show installed SDK and native use-case availability")
    data = commands.add_parser(
        "data",
        help="list data sources, fetch bars/actions, install dataset bundles",
        add_help=False,
    )
    data.add_argument("data_argv", nargs=argparse.REMAINDER)
    run_file = commands.add_parser(
        "run",
        help="compose a BacktestSpec from a strategy file and run it on the Rust engine",
    )
    run_file.add_argument("-f", "--strategy-file", required=True,
                          help="Python file exporting STRATEGY_CLASS or create_strategy(spec)")
    run_file.add_argument("--spec", type=Path,
                          help="explicit BacktestSpec JSON; mutually exclusive with --bars/--data")
    run_file.add_argument("--bars", type=Path, help="frozen BarFrame JSON (explicit data source)")
    run_file.add_argument("--data", type=Path, help="dataset bundle directory (bars + snapshot)")
    run_file.add_argument("--source", help="fetch bars from this source before running (network)")
    run_file.add_argument("--instrument", help="instrument symbol; defaults to the bars file's")
    run_file.add_argument("-s", "--start", help="inclusive start date YYYYMMDD")
    run_file.add_argument("-e", "--end", help="inclusive end date YYYYMMDD")
    run_file.add_argument("-a", "--cash", help="initial cash in major units (default: 100000)")
    run_file.add_argument("--currency", help="settlement currency; required outside A shares/sample")
    run_file.add_argument("--frequency", help="fetch frequency (default: daily)")
    run_file.add_argument("--adjustment", choices=("none", "qfq", "hfq"))
    run_file.add_argument("--seed", type=int)
    run_file.add_argument("-o", "--output", type=Path,
                          help="run directory for bars/spec/artifacts (default: runs/<strategy>)")
    project = commands.add_parser(
        "project",
        help="initialize, inspect, and run a versioned local strategy project",
    )
    project_commands = project.add_subparsers(dest="project_command", required=True)
    project_init = project_commands.add_parser("init", help="create a strategy project scaffold")
    project_init.add_argument("directory", help="project directory to create or update")
    project_init.add_argument("--strategy-file", help="link an existing strategy file")
    project_init.add_argument("--input-spec", help="link a versioned BacktestSpec JSON file")
    project_init.add_argument("--runtime-config", help="optional server runtime config for plan")
    project_init.add_argument("--worker-id", help="strategy worker id in the server runtime config")
    project_init.add_argument("--force", action="store_true", help="replace the project manifest")
    project_init.add_argument("--demo", action="store_true",
                              help="write an offline sample dataset and a runnable input spec")
    project_init.add_argument("--source", help="fetch real bars from this source (implies demo wiring)")
    project_init.add_argument("--instrument", help="instrument symbol for the dataset")
    project_init.add_argument("--start", help="inclusive start date YYYYMMDD")
    project_init.add_argument("--end", help="inclusive end date YYYYMMDD")
    project_init.add_argument("--cash", default="100000", help="initial cash in major units")
    project_init.add_argument("--currency", help="settlement currency")
    project_init.add_argument("--frequency", default="daily")
    project_init.add_argument("--adjustment", default="none", choices=("none", "qfq", "hfq"))
    project_init.add_argument("--rows", type=int, default=600, help="sample rows (--demo only)")
    project_doctor = project_commands.add_parser(
        "doctor", help="statically inspect a project; run user code only with --dynamic",
    )
    project_doctor.add_argument("project_path", help="project directory or qianxing.strategy.json")
    project_doctor.add_argument("--dynamic", action="store_true",
                                help="run strategy preflight in a timed trusted-local subprocess")
    project_doctor.add_argument("--timeout", type=int, default=30,
                                help="dynamic preflight timeout in seconds (1-300)")
    project_show = project_commands.add_parser(
        "show", help="show project metadata and resolved paths without executing code"
    )
    project_show.add_argument("project_path", help="project directory or qianxing.strategy.json")
    project_plan = project_commands.add_parser(
        "plan", help="show a validated local or static server execution plan",
    )
    project_plan.add_argument("project_path", help="project directory or qianxing.strategy.json")
    project_plan.add_argument("--profile", choices=("local", "server"), default="local")
    project_plan.add_argument("--runtime-config", help="server runtime JSON path")
    project_plan.add_argument("--worker-id", help="strategy worker id in the runtime config")
    project_plan.add_argument("--explain", action="store_true", help="show effective values, their sources and overridden values")
    project_run = project_commands.add_parser("run", help="run the project's strategy locally")
    project_run.add_argument("project_path", help="project directory or qianxing.strategy.json")
    project_report = project_commands.add_parser("report", help="generate verified HTML/XLSX/JSON reports for a completed project run")
    project_report.add_argument("project_path")
    project_report.add_argument("run_id")
    project_report.add_argument(
        "--cli", default=None,
        help="report renderer executable; defaults to the qx-cli bundled in this wheel, then PATH",
    )
    project_report.add_argument("-o", "--output", help="HTML output path (relative to project root)")
    project_report.add_argument(
        "--format", default="html", metavar="LIST",
        help="comma-separated report formats: html, xlsx, json (default: html)",
    )
    deployments = project_commands.add_parser("deployments", help="query observed server strategy processes")
    deployments.add_argument("project_path")
    deployments.add_argument("--api-url", required=True, help="Qianxing API origin")
    deployments.add_argument("--worker-id", help="override the worker selected by the project manifest")
    deployments.add_argument("--ca-cert", help="CA certificate bundle for HTTPS")
    deployments.add_argument("--client-cert", help="mTLS client certificate")
    deployments.add_argument("--client-key", help="mTLS client private key path")
    for action, help_text in (("runs", "list project run receipts"),
                              ("status", "inspect a project run"),
                              ("cancel", "request cooperative Rust cancellation"),
                              ("logs", "read bounded Rust lifecycle events"),
                              ("artifacts", "verify recorded artifact bytes")):
        action_parser = project_commands.add_parser(action, help=help_text)
        action_parser.add_argument("project_path")
        if action != "runs":
            action_parser.add_argument("run_id")
        if action == "runs":
            action_parser.add_argument("--limit", type=int, default=100)
        if action == "logs":
            action_parser.add_argument("--tail", type=int, default=100)
    diagnose = commands.add_parser(
        "diagnose-strategy",
        help="probe a Python strategy class or factory for future-data and warmup sensitivity",
    )
    diagnose.add_argument(
        "strategy_file",
        help="Python file exporting STRATEGY_CLASS or create_strategy(spec)",
    )
    diagnose.add_argument("input", help="BacktestSpec, MultiMarketBacktestSpec, or experiment JSON file")
    diagnose.add_argument("--strategy-id", help="override the strategy ID")
    diagnose.add_argument("--strategy-version", help="override the strategy version")
    diagnose.add_argument(
        "--startup-windows",
        default="20,50,100",
        help="comma-separated history lengths used to compare warmup sensitivity",
    )
    diagnose.add_argument(
        "--max-lookahead-checks",
        type=int,
        default=32,
        help="maximum sampled decision points for future-Bar perturbations (1..1000)",
    )
    diagnose.add_argument("--timeout-seconds", type=int, default=10)
    diagnose.add_argument("--pit-ledger", type=Path, help="optional versioned point-in-time fact ledger")
    diagnose.add_argument("--out", type=Path, help="optional diagnostic JSON output path")
    inspect_strategy = commands.add_parser(
        "inspect-strategy",
        help="inspect a Python strategy's versioned metadata and code identity",
    )
    inspect_strategy.add_argument(
        "strategy_file",
        help="Python file exporting STRATEGY_CLASS or create_strategy(spec)",
    )
    inspect_strategy.add_argument("--out", type=Path, help="write the manifest JSON to this path")
    for name, help_text in (
        ("validate-dataset", "validate a versioned DatasetSpec JSON file"),
        ("backtest", "run a versioned Bar BacktestSpec JSON file"),
        ("verify", "verify artifacts described by a BacktestOutcome JSON file"),
        ("compare-runs", "rank completed runs from identical market data"),
        ("run-experiment", "run and compare a bounded Rust parameter grid"),
        ("run-depth-experiment", "run a bounded Rust Tick/OrderBook parameter grid"),
        ("run-multi-leg-experiment", "run a bounded Rust multi-leg parameter grid"),
        ("run-multi-market-experiment", "run a bounded shared-account multi-market grid"),
        ("depth-backtest", "run an L1 Tick or L2 order-book backtest"),
        ("verify-depth", "verify artifacts from an L1 Tick or L2 order-book run"),
    ):
        command = commands.add_parser(name, help=help_text)
        command.add_argument("input", help="path to the versioned JSON document")
    for name, help_text in (
        ("run-strategy", "run a local Python strategy file on the shared Rust Bar, Tick, or OrderBook engine"),
        ("search-strategy", "search parameters for a Python strategy file on the Rust engine"),
    ):
        command = commands.add_parser(name, help=help_text)
        command.add_argument(
            "strategy_file",
            help="Python file exporting STRATEGY_CLASS or create_strategy(spec)",
        )
        command.add_argument(
            "input",
            help="BacktestSpec, DepthBacktestSpec, RunExperimentSpec, or RunDepthExperimentSpec JSON file",
        )
        command.add_argument("--strategy-id", help="override the strategy ID")
        command.add_argument("--strategy-version", help="override the strategy version")
        if name == "search-strategy":
            command.add_argument(
                "--summary",
                action="store_true",
                help="print a compact ranking and out-of-sample effectiveness review",
            )
            command.add_argument(
                "--html",
                nargs="?",
                const="",
                metavar="PATH",
                help="write an offline interactive HTML report (default: beside the result artifact)",
            )
            command.add_argument(
                "--diagnostics",
                type=Path,
                help="attach a matching diagnose-strategy JSON sidecar to the HTML report",
            )
            command.add_argument(
                "--diagnose-candidates",
                action="store_true",
                help="replay future-data and warmup diagnostics for every materialized candidate",
            )
            command.add_argument(
                "--diagnostic-startup-windows",
                nargs="+",
                type=int,
                default=[20, 50, 100],
                metavar="BARS",
                help="startup history lengths used for every candidate diagnostic",
            )
            command.add_argument(
                "--diagnostic-max-checks",
                type=int,
                default=8,
                help="maximum sampled decision points per candidate (1..1000)",
            )
            command.add_argument(
                "--diagnostic-timeout-seconds",
                type=int,
                default=10,
                help="strategy callback timeout for each candidate diagnostic",
            )
            command.add_argument(
                "--diagnostic-candidate-limit",
                type=int,
                help="optional cap; remaining candidates stay listed as not_run_candidate_limit",
            )
    research = commands.add_parser(
        "research",
        help="run strategy search, per-candidate bias diagnostics, and an interactive report",
    )
    research.add_argument(
        "strategy_file",
        help="Python file exporting STRATEGY_CLASS or create_strategy(spec)",
    )
    research.add_argument(
        "input",
        help="RunExperimentSpec with an explicit parameter space and train/OOS split_plan",
    )
    research.add_argument("--strategy-id", help="override the strategy ID")
    research.add_argument("--strategy-version", help="override the strategy version")
    research.add_argument(
        "--html",
        nargs="?",
        const="",
        default="",
        metavar="PATH",
        help="write the interactive HTML report (default: beside the experiment artifact)",
    )
    research.add_argument(
        "--diagnostic-startup-windows",
        nargs="+",
        type=int,
        default=[20, 50, 100],
        metavar="BARS",
        help="startup history lengths used for every materialized candidate",
    )
    research.add_argument(
        "--diagnostic-max-checks",
        type=int,
        default=8,
        help="maximum sampled decision points per candidate (1..1000)",
    )
    research.add_argument(
        "--diagnostic-timeout-seconds",
        type=int,
        default=10,
        help="strategy callback timeout for each candidate diagnostic",
    )
    research.add_argument(
        "--diagnostic-candidate-limit",
        type=int,
        default=16,
        help="maximum candidates to diagnose (default: 16); remaining candidates stay explicitly unverified",
    )
    remote_research = commands.add_parser(
        "remote-research",
        help="run a Python strategy research batch in an explicitly selected remote sandbox",
    )
    remote_research.add_argument(
        "strategy_file", help="Python file exporting create_strategy(spec)"
    )
    remote_research.add_argument(
        "input",
        help="RunExperimentSpec with an explicit parameter space and required train/OOS split_plan",
    )
    remote_research.add_argument(
        "--provider",
        choices=("e2b", "microsandbox", "opensandbox"),
        default="e2b",
        help="remote sandbox provider; E2B remains the default, with no automatic provider fallback",
    )
    remote_research.add_argument(
        "--template",
        default=os.environ.get("QX_E2B_TEMPLATE"),
        help="E2B template with the matching qianxing wheel and pinned strategy dependencies (or QX_E2B_TEMPLATE)",
    )
    remote_research.add_argument(
        "--domain",
        default=os.environ.get("OPEN_SANDBOX_DOMAIN"),
        help="OpenSandbox API domain (or OPEN_SANDBOX_DOMAIN)",
    )
    remote_research.add_argument(
        "--image",
        help="digest-pinned OCI image with the matching qianxing wheel and pinned strategy dependencies; provider-specific image environment variables are also supported",
    )
    remote_research.add_argument(
        "--backend",
        choices=("local", "cloud"),
        default=os.environ.get("QX_MICROSANDBOX_BACKEND", "local"),
        help="Microsandbox backend (default: local; cloud requires host credentials)",
    )
    remote_research.add_argument(
        "--cpus",
        type=int,
        help="sandbox CPU limit (1..64; defaults from the selected provider's host settings)",
    )
    remote_research.add_argument(
        "--memory-mib",
        type=int,
        help="sandbox memory limit in MiB (512..65536; defaults from the selected provider's host settings)",
    )
    remote_research.add_argument(
        "--timeout-seconds",
        type=int,
        default=900,
        help="sandbox lease and command timeout (1..86400 seconds)",
    )
    remote_research.add_argument(
        "--output-dir",
        type=Path,
        help="new local directory for downloaded result artifacts (default: <input>-<provider>-research)",
    )
    remote_research.add_argument("--strategy-id", help="override the strategy ID")
    remote_research.add_argument(
        "--strategy-version", help="override the strategy version"
    )
    remote_research.add_argument(
        "--diagnostic-startup-windows",
        nargs="+",
        type=int,
        default=[20, 50, 100],
        metavar="BARS",
        help="history lengths used for candidate warmup diagnostics",
    )
    remote_research.add_argument(
        "--diagnostic-max-checks",
        type=int,
        default=8,
        help="maximum sampled future-data checks per candidate (1..1000)",
    )
    remote_research.add_argument(
        "--diagnostic-timeout-seconds",
        type=int,
        default=10,
        help="strategy callback timeout used by each diagnostic probe",
    )
    remote_research.add_argument(
        "--diagnostic-candidate-limit",
        type=int,
        default=16,
        help="maximum candidates to diagnose (default: 16); remaining candidates stay explicitly unverified",
    )
    return parser


def _prepare_guided_research_input(payload: Any) -> dict[str, Any]:
    """Delegate guided research schema and baseline policy to the Rust app layer."""
    if not isinstance(payload, dict):
        raise InvalidInputError("qianxing research input must be a JSON object")
    return prepare_quick_research_spec(payload)


def _run_strategy_command(args: argparse.Namespace) -> Any:
    from uuid import uuid4

    from . import run_spec
    from .app import BuiltinStrategySpec

    strategy_file = Path(args.strategy_file).expanduser().resolve(strict=True)
    data_flags = [value for value in (args.bars, args.data, args.source) if value is not None]
    composition_flags = (args.instrument, args.start, args.end, args.cash, args.currency,
                         args.frequency, args.adjustment, args.seed, args.output)
    if args.spec is not None:
        if data_flags or any(value is not None for value in composition_flags):
            raise ValueError("--spec cannot be combined with data, date, cash or output flags")
        return run_strategy_file(_read_json(args.spec), strategy_file)
    if len(data_flags) != 1:
        raise ValueError("run requires exactly one explicit data source: --bars, --data or --source")
    if args.source is None and (args.frequency is not None or args.adjustment is not None):
        raise ValueError("--frequency/--adjustment require --source; frozen bars cannot be reinterpreted")
    cash_raw = run_spec.parse_cash(args.cash or "100000")
    seed = args.seed if args.seed is not None else 0
    if not 0 <= seed < 2 ** 64:
        raise ValueError("--seed must be an unsigned 64-bit integer")
    run_id = uuid4().hex
    output = (args.output or Path.cwd() / "runs" / strategy_file.stem).expanduser().resolve() / run_id
    snapshot = None
    if args.data is not None:
        bundle = run_spec.load_data_bundle(args.data)
        bars_path = bundle["bars"]
        snapshot = bundle["snapshot"]
    elif args.bars is not None:
        bars_path = args.bars.expanduser().resolve(strict=True)
    else:
        if not (args.instrument and args.start and args.end):
            raise ValueError("--source requires --instrument/--start/--end")
        from .strategy_project import _write_fetched_dataset

        bundle = _write_fetched_dataset(
            output / "data", source=args.source, instrument=args.instrument,
            start=args.start, end=args.end, frequency=args.frequency or "daily",
            adjustment=args.adjustment or "none",
        )
        bars_path = output / "data" / bundle["bars_path"]
        snapshot = output / "data" / bundle["snapshot_path"]
    original = run_spec.load_bars(bars_path)
    if args.instrument is not None:
        expected = args.instrument
        if args.source is not None:
            from qianxing_ashare import normalize_instrument

            expected = normalize_instrument(expected)
        if expected != original.instrument:
            raise ValueError(f"--instrument {expected!r} differs from bars {original.instrument!r}")
    frame = run_spec.slice_frame(original, args.start, args.end)
    currency = args.currency or run_spec.settlement_currency(frame.instrument)
    loaded = load_strategy_file(strategy_file)
    module, _factory, digest = loaded
    metadata = build_module_manifest(module, strategy_file, digest)
    output.mkdir(parents=True, exist_ok=True)
    if frame is not original:
        bars_path = output / "bars.json"
        bars_path.write_text(frame.to_json(), encoding="utf-8", newline="\n")
        if snapshot is not None:
            print("Date slicing creates new bars without the original snapshot binding.", file=sys.stderr)
        snapshot = None
    spec = run_spec.compose_run_spec(
        instrument=frame.instrument, bars_path=bars_path, dataset_snapshot_path=snapshot,
        output_dir=output, initial_cash_raw=cash_raw,
        strategy_config=BuiltinStrategySpec("sma_cross", metadata["strategy_id"]).to_dict(),
        run_id=run_id, settlement_currency=currency, seed=seed,
    )
    path = run_spec.write_run_spec(output, spec)
    print(f"Execution spec: {path}", file=sys.stderr)
    return run_strategy_file(spec, strategy_file, _loaded_strategy=loaded)


def _dispatch_command(argv: Sequence[str] | None = None) -> int:
    _configure_console_encoding()
    # `data` 的旗标面由 qianxing_ashare 拥有（含 -h）；先在顶层截走，避免外层
    # argparse 把 `--json`/`-h` 当成未知参数（方案 D11 的薄分派）。
    tokens = list(sys.argv[1:] if argv is None else argv)
    if tokens and tokens[0] == "data":
        from qianxing_ashare.cli import main as data_main

        return data_main(tokens[1:], prog="qianxing data")
    args = _parser().parse_args(tokens)
    guided_research = args.command == "research"
    research_workflow = guided_research or args.command == "remote-research"
    if guided_research:
        # The guided flow is a preset over the same validated search path. Keep
        # one implementation for search, candidate identity, diagnostics, and reports.
        args.command = "search-strategy"
        args.summary = True
        args.diagnostics = None
        args.diagnose_candidates = True
    try:
        if args.command == "data":
            # 取数命令由 qianxing_ashare 拥有；这里只做薄分派（方案 D11）。
            from qianxing_ashare.cli import main as data_main

            return data_main(args.data_argv, prog="qianxing data")
        if args.command == "project":
            try:
                if args.project_command == "init":
                    result = init_strategy_project(
                        args.directory,
                        strategy_file=args.strategy_file,
                        input_spec=args.input_spec,
                        runtime_config=args.runtime_config,
                        worker_id=args.worker_id,
                        force=args.force,
                        demo=args.demo,
                        source=args.source,
                        instrument=args.instrument,
                        start=args.start,
                        end=args.end,
                        cash=args.cash,
                        currency=args.currency,
                        frequency=args.frequency,
                        adjustment=args.adjustment,
                        rows=args.rows,
                    )
                elif args.project_command == "doctor":
                    result = (
                        dynamic_project_doctor(args.project_path, timeout_seconds=args.timeout)
                        if args.dynamic
                        else static_project_doctor(args.project_path)
                    )
                elif args.project_command == "show":
                    result = show_strategy_project(args.project_path)
                elif args.project_command == "plan":
                    if args.profile == "local":
                        if args.runtime_config is not None or args.worker_id is not None:
                            raise ValueError("--runtime-config/--worker-id require --profile server")
                        result = local_project_preflight(args.project_path, explain=args.explain)
                    else:
                        result = server_project_plan(
                            args.project_path,
                            runtime_config=args.runtime_config,
                            worker_id=args.worker_id,
                            explain=args.explain,
                        )
                elif args.project_command == "report":
                    root, _manifest = load_strategy_project(args.project_path)
                    result = generate_project_report(root, args.run_id, executable=args.cli,
                                                     output=args.output, formats=args.format)
                elif args.project_command == "deployments":
                    context = None
                    if args.client_key and not args.client_cert:
                        raise ValueError("--client-key requires --client-cert")
                    if args.ca_cert or args.client_cert:
                        import ssl
                        context = ssl.create_default_context(cafile=args.ca_cert)
                        if args.client_cert:
                            context.load_cert_chain(args.client_cert, args.client_key)
                    result = project_deployments(args.project_path, args.api_url,
                                                 worker_id=args.worker_id, ssl_context=context)
                elif args.project_command in {"runs", "status", "cancel", "logs", "artifacts"}:
                    root, _manifest = load_strategy_project(args.project_path)
                    if args.project_command == "runs":
                        result = project_runs(root, args.limit)
                    elif args.project_command == "status":
                        result = project_run_status(root, args.run_id)
                    elif args.project_command == "cancel":
                        result = cancel_project_run(root, args.run_id)
                    elif args.project_command == "artifacts":
                        result = verify_project_artifacts(root, args.run_id)
                    else:
                        result = project_run_events(root, args.run_id, args.tail)
                elif args.project_command == "run":
                    execution_result: dict[str, Any] = {}

                    def execute(root, manifest):
                        strategy_path = Path(manifest["strategy_file"]).expanduser()
                        if not strategy_path.is_absolute():
                            strategy_path = root / strategy_path
                        strategy_snapshot = snapshot_project_strategy(
                            root, strategy_path.resolve(strict=True)
                        )
                        input_path = Path(manifest["input_spec"]).expanduser()
                        if not input_path.is_absolute():
                            input_path = root / input_path
                        snapshot = snapshot_project_input(root, input_path.resolve(strict=True))
                        run_plan = local_project_preflight(
                            root, input_spec_override=snapshot,
                            strategy_file_override=strategy_snapshot,
                        )
                        record_project_plan(run_plan)
                        execution_result["value"] = run_project_strategy(
                            root, manifest, snapshot, run_plan
                        )
                        return 0

                    run_status = run_project(args.project_path, execute)
                    if run_status != 0:
                        return run_status
                    result = execution_result.get("value")
                    if result is None:
                        raise RuntimeError("project run completed without a strategy result")
                else:
                    raise InvalidInputError("unknown project action")
            except QianxingError:
                raise
            except (OSError, TypeError, ValueError) as exc:
                raise InvalidInputError(
                    f"strategy project {args.project_command} failed: {exc}",
                    payload={"category": "INVALID_INPUT", "message": str(exc)},
                ) from exc
        elif args.command == "doctor":
            result = doctor()
        elif args.command == "inspect-strategy":
            module, _factory, bundle_sha256 = _load_strategy_file(args.strategy_file)
            try:
                strategy_manifest = build_module_manifest(
                    module, Path(args.strategy_file).expanduser().resolve(), bundle_sha256
                )
            except (OSError, ValueError) as exc:
                raise InvalidInputError(
                    f"invalid Python strategy manifest: {exc}",
                    payload={"category": "INVALID_INPUT", "message": str(exc)},
                ) from exc
            if args.out is not None:
                protected = [args.strategy_file]
                protected.extend(
                    Path(args.strategy_file).expanduser().resolve().parent / dependency
                    for dependency in strategy_manifest["dependencies"]
                )
                output_path = _write_protected_json(args.out, strategy_manifest, protected)
                print(f"Strategy manifest: {output_path}", file=sys.stderr)
            result = strategy_manifest
        elif args.command == "run":
            try:
                result = _run_strategy_command(args)
            except (OSError, ValueError) as exc:
                raise InvalidInputError(str(exc), payload={"category": "INVALID_INPUT", "message": str(exc)}) from exc
        else:
            payload = _read_json(args.input)
            if research_workflow:
                payload = _prepare_guided_research_input(payload)
            if args.command == "remote-research":
                if not isinstance(payload, dict):
                    raise InvalidInputError("remote research input must be a JSON object")
                try:
                    if args.provider == "e2b":
                        if not args.template:
                            raise InvalidInputError(
                                "E2B remote-research needs --template or QX_E2B_TEMPLATE"
                            )
                        from .sandbox_execution import E2BStrategySandboxProvider

                        provider = E2BStrategySandboxProvider(
                            template_id=args.template,
                            timeout_seconds=args.timeout_seconds,
                        )
                    elif args.provider == "opensandbox":
                        domain = args.domain or os.environ.get("OPEN_SANDBOX_DOMAIN")
                        if not domain:
                            raise InvalidInputError(
                                "OpenSandbox remote-research needs --domain or OPEN_SANDBOX_DOMAIN"
                            )
                        image = args.image or os.environ.get("QX_OPENSANDBOX_IMAGE")
                        if not image:
                            raise InvalidInputError(
                                "OpenSandbox remote-research needs --image or QX_OPENSANDBOX_IMAGE"
                            )
                        from .sandbox_execution import (
                            OpenSandboxStrategySandboxProvider,
                        )

                        provider = OpenSandboxStrategySandboxProvider(
                            domain=domain,
                            image=image,
                            api_key=os.environ.get("OPEN_SANDBOX_API_KEY"),
                            timeout_seconds=args.timeout_seconds,
                            cpu_count=(
                                args.cpus
                                if args.cpus is not None
                                else int(os.environ.get("QX_OPENSANDBOX_CPUS", "1"))
                            ),
                            memory_mib=(
                                args.memory_mib
                                if args.memory_mib is not None
                                else int(
                                    os.environ.get(
                                        "QX_OPENSANDBOX_MEMORY_MIB", "2048"
                                    )
                                )
                            ),
                        )
                    else:
                        image = args.image or os.environ.get("QX_MICROSANDBOX_IMAGE")
                        if not image:
                            raise InvalidInputError(
                                "Microsandbox remote-research needs --image or QX_MICROSANDBOX_IMAGE"
                            )
                        from .sandbox_execution import (
                            MicrosandboxStrategySandboxProvider,
                        )

                        provider = MicrosandboxStrategySandboxProvider(
                            image=image,
                            backend=args.backend,
                            api_key=os.environ.get("MICROSANDBOX_API_KEY"),
                            profile=os.environ.get("QX_MICROSANDBOX_PROFILE"),
                            url=os.environ.get("QX_MICROSANDBOX_URL"),
                            timeout_seconds=args.timeout_seconds,
                            cpu_count=(
                                args.cpus
                                if args.cpus is not None
                                else int(os.environ.get("QX_MICROSANDBOX_CPUS", "1"))
                            ),
                            memory_mib=(
                                args.memory_mib
                                if args.memory_mib is not None
                                else int(
                                    os.environ.get(
                                        "QX_MICROSANDBOX_MEMORY_MIB", "1024"
                                    )
                                )
                            ),
                        )
                    result = provider.run_research(
                        strategy_file=args.strategy_file,
                        input_path=args.input,
                        input_spec=payload,
                        output_dir=args.output_dir,
                        strategy_id=args.strategy_id,
                        strategy_version=args.strategy_version,
                        diagnostic_startup_windows=args.diagnostic_startup_windows,
                        diagnostic_max_checks=args.diagnostic_max_checks,
                        diagnostic_timeout_seconds=args.diagnostic_timeout_seconds,
                        diagnostic_candidate_limit=args.diagnostic_candidate_limit,
                    )
                except InvalidInputError:
                    raise
                except Exception as exc:
                    raise InvalidInputError(
                        f"remote {args.provider} research failed: {type(exc).__name__}: {exc}",
                        payload={
                            "category": "REMOTE_SANDBOX_FAILED",
                            "message": str(exc),
                        },
                    ) from exc
            elif args.command == "validate-dataset":
                result = validate_dataset(payload)
            elif args.command == "diagnose-strategy":
                if not isinstance(payload, dict):
                    message = "diagnose-strategy input must be a BacktestSpec object"
                    raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
                diagnostic_spec = payload.get("base") if isinstance(payload.get("base"), dict) else payload
                module, _factory, source_sha256 = _load_strategy_file(args.strategy_file)
                try:
                    strategy_manifest = build_module_manifest(
                        module, Path(args.strategy_file).expanduser().resolve(), source_sha256
                    )
                    multi_market = isinstance(diagnostic_spec.get("markets"), list)
                    if multi_market:
                        slots = [
                            strategy.get("strategy_id")
                            for market in diagnostic_spec["markets"]
                            if isinstance(market, dict)
                            for slot in market.get("strategies", [])
                            if isinstance(slot, dict)
                            for strategy in [slot.get("strategy")]
                            if isinstance(strategy, dict)
                        ]
                        configured_id = payload.get("target_strategy_id")
                        if args.strategy_id and configured_id and args.strategy_id != configured_id:
                            raise InvalidInputError(
                                "--strategy-id must match target_strategy_id in the experiment spec"
                            )
                        strategy_id = (
                            args.strategy_id
                            or configured_id
                            or getattr(module, "STRATEGY_ID", None)
                            or strategy_manifest.get("strategy_id")
                            or (slots[0] if len(slots) == 1 else None)
                        )
                        if not isinstance(strategy_id, str) or not strategy_id.strip():
                            raise InvalidInputError(
                                "multi-market diagnostics need --strategy-id or target_strategy_id"
                            )
                        validate_strategy_execution_preflight_v1(
                            strategy_manifest,
                            diagnostic_spec,
                            purpose="diagnose",
                            target_strategy_id=strategy_id,
                        )
                        strategy_version = (
                            args.strategy_version
                            or getattr(module, "STRATEGY_VERSION", None)
                            or strategy_manifest.get("strategy_version")
                            or "local"
                        )
                        (
                            request,
                            strategy_config,
                            factory_context,
                            diagnostic_market_bars,
                            market_inputs,
                        ) = _multi_market_diagnostic_request(
                            diagnostic_spec, strategy_id, str(strategy_version)
                        )
                    else:
                        strategy_id, strategy_version = _strategy_identity(
                            args, module, args.strategy_file, payload, strategy_manifest
                        )
                        validate_strategy_execution_preflight_v1(
                            strategy_manifest,
                            diagnostic_spec,
                            purpose="diagnose",
                            target_strategy_id=None,
                        )
                        request = _strategy_diagnostic_request(
                            diagnostic_spec, strategy_id, strategy_version
                        )
                        strategy_config = diagnostic_spec.get("strategy")
                        factory_context = None
                        diagnostic_market_bars = None
                        market_inputs = None
                    windows = [
                        int(value.strip())
                        for value in args.startup_windows.split(",")
                        if value.strip()
                    ]
                except ValueError as exc:
                    raise InvalidInputError(
                        str(exc), payload={"category": "INVALID_INPUT", "message": str(exc)}
                    ) from exc
                pit_ledger = None
                if args.pit_ledger is not None:
                    try:
                        pit_ledger_value = json.loads(args.pit_ledger.read_text(encoding="utf-8"))
                    except OSError as exc:
                        message = f"cannot read PIT ledger {str(args.pit_ledger)!r}: {exc}"
                        raise DataUnavailableError(message, payload={"category": "DATA_UNAVAILABLE", "message": message}) from exc
                    except json.JSONDecodeError as exc:
                        message = f"PIT ledger JSON is invalid: {exc}"
                        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message}) from exc
                    if not isinstance(pit_ledger_value, dict):
                        message = "PIT ledger must be a JSON object"
                        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
                    pit_ledger = pit_ledger_value
                try:
                    from qianxing_strategy.diagnostics import diagnose_callback_strategy

                    result = diagnose_callback_strategy(
                        request,
                        args.strategy_file,
                        diagnostic_spec,
                        strategy_bundle_sha256=source_sha256,
                        startup_windows=windows,
                        max_checks=args.max_lookahead_checks,
                        timeout_seconds=args.timeout_seconds,
                        pit_ledger=pit_ledger,
                        diagnostic_market_bars=diagnostic_market_bars,
                        factory_context=factory_context,
                    )
                    result["backtest_strategy_config"] = strategy_config
                    if multi_market:
                        result["multi_market"] = {
                            "target_strategy_id": strategy_id,
                            "market_count": len(diagnostic_spec["markets"]),
                            "peer_market_bars_included": len(diagnostic_market_bars or {}),
                            "peer_market_instruments": sorted(diagnostic_market_bars or {}),
                            "market_inputs": market_inputs,
                            "point_in_time_ledger_scope": (
                                "target_market_only" if pit_ledger is not None else "not_supplied"
                            ),
                        }
                except (OSError, ValueError, TimeoutError, subprocess.SubprocessError) as exc:
                    message = f"Python strategy diagnostic failed: {exc}"
                    raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message}) from exc
                if args.out is not None:
                    protected = [args.input, args.strategy_file, *_referenced_input_paths(diagnostic_spec)]
                    if args.pit_ledger is not None:
                        protected.append(args.pit_ledger)
                    output_path = _write_protected_json(args.out, result, protected)
                    print(f"Strategy diagnostics: {output_path}", file=sys.stderr)
            elif args.command == "backtest":
                result = run_backtest(payload)
            elif args.command == "compare-runs":
                result = compare_runs(payload)
            elif args.command == "run-experiment":
                result = run_experiment(payload)
            elif args.command == "run-depth-experiment":
                result = run_depth_experiment(payload)
            elif args.command == "run-multi-leg-experiment":
                result = run_multi_leg_experiment(payload)
            elif args.command == "run-multi-market-experiment":
                result = run_multi_market_experiment(payload)
            elif args.command == "depth-backtest":
                result = run_depth_backtest(payload)
            elif args.command == "verify-depth":
                result = verify_depth_run(payload)
            elif args.command == "run-strategy":
                result = run_strategy_file(
                    payload,
                    args.strategy_file,
                    strategy_id=args.strategy_id,
                    strategy_version=args.strategy_version,
                )
            elif args.command == "search-strategy":
                if not isinstance(payload, dict):
                    message = "strategy input JSON must be an object"
                    raise InvalidInputError(
                        message,
                        payload={"category": "INVALID_INPUT", "message": message},
                    )
                module, factory, source_sha256 = _load_strategy_file(args.strategy_file)
                try:
                    strategy_manifest = build_module_manifest(
                        module, Path(args.strategy_file).expanduser().resolve(), source_sha256
                    )
                    selected_strategy_id = args.strategy_id
                    if selected_strategy_id is None and args.command == "search-strategy":
                        selected_strategy_id = payload.get("target_strategy_id")
                    if selected_strategy_id is None and args.command == "search-strategy":
                        selected_strategy_id = strategy_manifest.get("strategy_id")
                    if (
                        selected_strategy_id is None
                        and not isinstance(payload.get("markets"), list)
                    ):
                        selected_strategy_id = strategy_manifest.get("strategy_id")
                    payload = apply_parameter_defaults(
                        payload,
                        strategy_manifest,
                        target_strategy_id=selected_strategy_id,
                    )
                    factory = _bind_manifest_config_factory(
                        factory,
                        module,
                        strategy_manifest,
                        selected_strategy_id,
                    )
                    validate_strategy_execution_preflight_v1(
                        strategy_manifest,
                        payload,
                        purpose="search" if args.command == "search-strategy" else "run",
                        target_strategy_id=selected_strategy_id,
                        split_plan=(
                            payload.get("split_plan")
                            if args.command == "search-strategy"
                            and isinstance(payload.get("split_plan"), dict)
                            else None
                        ),
                    )
                    if strategy_manifest.get("warmup_bars") is not None:
                        factory = _bind_manifest_warmup_factory(
                            factory, strategy_manifest["warmup_bars"]
                        )
                except (OSError, ValueError) as exc:
                    raise InvalidInputError(
                        f"strategy manifest preflight failed: {exc}",
                        payload={"category": "INVALID_INPUT", "message": str(exc)},
                    ) from exc
                strategy_id, strategy_version = _strategy_identity(
                    args, module, args.strategy_file, payload, strategy_manifest
                )
                depth_base = payload.get("base")
                if (
                    isinstance(depth_base, dict)
                    and isinstance(depth_base.get("depth_path"), str)
                    and depth_base.get("tier") in {"l1", "l2"}
                ):
                    expected_data_level = (
                        "tick" if depth_base["tier"] == "l1" else "orderbook"
                    )
                    declared_data_level = strategy_manifest.get("data_level")
                    if declared_data_level not in (None, expected_data_level):
                        message = (
                            f"strategy manifest data_level={declared_data_level!r} does not match "
                            f"depth tier={depth_base['tier']!r} (expected {expected_data_level!r})"
                        )
                        raise InvalidInputError(
                            message,
                            payload={"category": "INVALID_INPUT", "message": message},
                        )
                    if payload.get("strategy_kinds"):
                        message = (
                            "strategy_kinds cannot be combined with a Python strategy file; "
                            "use parameter_space for the depth strategy"
                        )
                        raise InvalidInputError(
                            message,
                            payload={"category": "INVALID_INPUT", "message": message},
                        )
                    if not payload.get("parameter_space"):
                        message = "RunDepthExperimentSpec needs parameter_space"
                        raise InvalidInputError(
                            message,
                            payload={"category": "INVALID_INPUT", "message": message},
                        )
                    if args.html is not None or args.diagnostics is not None:
                        message = (
                            "interactive HTML and diagnostic sidecars currently support Bar experiments only; "
                            "depth strategy search returns the complete ranked JSON report"
                        )
                        raise InvalidInputError(
                            message,
                            payload={"category": "INVALID_INPUT", "message": message},
                        )
                    if args.diagnose_candidates:
                        message = (
                            "candidate future-data diagnostics currently support Bar experiments only"
                        )
                        raise InvalidInputError(
                            message,
                            payload={"category": "INVALID_INPUT", "message": message},
                        )
                    print(
                        "[建议] RunDepthExperimentSpec 没有训练/样本外切分；当前排名只表示该深度输入上的样本内候选排序。",
                        file=sys.stderr,
                    )
                    result = run_depth_experiment_with_strategy(
                        payload,
                        factory,
                        strategy_id=strategy_id,
                        strategy_version=strategy_version,
                        source_sha256=source_sha256,
                    )
                elif isinstance(payload.get("base"), dict) and isinstance(
                    payload["base"].get("markets"), list
                ):
                    if payload.get("strategy_kinds"):
                        message = (
                            "strategy_kinds cannot be combined with a Python strategy file; "
                            "use parameter_space for the target strategy slot"
                        )
                        raise InvalidInputError(
                            message,
                            payload={"category": "INVALID_INPUT", "message": message},
                        )
                    callback_space = payload.get("callback_parameter_space")
                    parameter_space = payload.get("parameter_space")
                    search = payload.get("search") or {}
                    continuous_space = (
                        search.get("continuous_parameter_space", [])
                        if isinstance(search, dict)
                        else []
                    )
                    if callback_space and parameter_space:
                        message = (
                            "use either callback_parameter_space for arbitrary Python strategy "
                            "parameters or parameter_space for Rust engine parameters, not both"
                        )
                        raise InvalidInputError(
                            message,
                            payload={"category": "INVALID_INPUT", "message": message},
                        )
                    if not callback_space and not parameter_space and not continuous_space:
                        message = (
                            "multi-market search-strategy needs parameter_space or "
                            "callback_parameter_space or search.continuous_parameter_space"
                        )
                        raise InvalidInputError(
                            message,
                            payload={"category": "INVALID_INPUT", "message": message},
                        )
                    if not payload.get("split_plan"):
                        print(
                            "[建议] 多市场文件策略实验未配置 split_plan；结果只有样本内排序，不能据此判断样本外有效性。",
                            file=sys.stderr,
                        )
                    result = run_multi_market_experiment_with_strategy(
                        payload,
                        _multi_market_experiment_factory(
                            payload, factory, strategy_id
                        ),
                        strategy_id=strategy_id,
                        strategy_version=strategy_version,
                        source_sha256=source_sha256,
                    )
                else:
                    search = payload.get("search") or {}
                    if not isinstance(search, dict):
                        message = "RunExperimentSpec.search must be an object"
                        raise InvalidInputError(
                            message,
                            payload={"category": "INVALID_INPUT", "message": message},
                        )
                    if not payload.get("parameter_space") and not search.get(
                        "continuous_parameter_space"
                    ):
                        message = (
                            "search-strategy needs parameter_space or "
                            "search.continuous_parameter_space"
                        )
                        raise InvalidInputError(
                            message,
                            payload={"category": "INVALID_INPUT", "message": message},
                        )
                    if not payload.get("split_plan"):
                        print(
                            "[建议] RunExperimentSpec 未配置 split_plan；结果只有样本内排序，不能据此判断样本外有效性。",
                            file=sys.stderr,
                        )
                    result = run_experiment_with_strategy(
                        payload,
                        factory,
                        strategy_id=strategy_id,
                        strategy_version=strategy_version,
                        source_sha256=source_sha256,
                    )
            else:
                result = verify_run(payload)
        full_document = _document(result)
        record_project_result(full_document)
        if args.command == "search-strategy" and args.diagnose_candidates:
            if not isinstance(payload, dict) or not isinstance(payload.get("base"), dict):
                message = "--diagnose-candidates requires a RunExperimentSpec or RunMultiMarketExperimentSpec"
                raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
            try:
                candidate_diagnostics = _diagnose_experiment_candidates(
                    payload,
                    full_document if isinstance(full_document, dict) else {},
                    args.strategy_file,
                    strategy_id=strategy_id,
                    strategy_version=str(strategy_version),
                    strategy_bundle_sha256=source_sha256,
                    startup_windows=args.diagnostic_startup_windows,
                    max_checks=args.diagnostic_max_checks,
                    timeout_seconds=args.diagnostic_timeout_seconds,
                    candidate_limit=args.diagnostic_candidate_limit,
                )
            except (TypeError, ValueError) as exc:
                raise InvalidInputError(
                    f"candidate diagnostics configuration failed: {exc}",
                    payload={"category": "INVALID_INPUT", "message": str(exc)},
                ) from exc
            if not isinstance(full_document, dict):
                message = "search-strategy result cannot carry candidate diagnostics"
                raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
            full_document = {**full_document, "candidate_diagnostics": candidate_diagnostics}
        document = (
            _experiment_summary(full_document)
            if args.command == "search-strategy" and args.summary
            else full_document
        )
        if (
            args.command == "search-strategy"
            and args.summary
            and isinstance(full_document, dict)
            and isinstance(full_document.get("candidate_diagnostics"), dict)
            and isinstance(document, dict)
        ):
            diagnostics = full_document["candidate_diagnostics"]
            document["candidate_diagnostics"] = {
                key: diagnostics.get(key)
                for key in (
                    "coverage_status",
                    "candidate_count",
                    "diagnosed_count",
                    "diagnostic_failure_count",
                    "unavailable_candidate_count",
                    "not_run_candidate_count",
                    "items",
                )
            }
            document["candidate_diagnostics"]["items"] = [
                {
                    key: item.get(key)
                    for key in (
                        "scope",
                        "fold",
                        "ordinal",
                        "strategy_kind",
                        "parameters",
                        "candidate_fingerprint",
                        "status",
                        "reason",
                    )
                }
                for item in diagnostics.get("items", [])
                if isinstance(item, dict)
            ]
        if args.command == "search-strategy" and (args.html is not None or args.diagnostics is not None):
            if args.diagnostics is not None and args.html is None:
                message = "--diagnostics requires --html"
                raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
            if not isinstance(full_document, dict):
                message = "search-strategy result is not an object and cannot be rendered as an HTML report"
                raise InvalidInputError(
                    message, payload={"category": "INVALID_INPUT", "message": message}
                )
            report_document = full_document
            if args.diagnostics is not None:
                if not isinstance(payload, dict) or not isinstance(payload.get("base"), dict):
                    message = "--diagnostics requires a RunExperimentSpec or RunMultiMarketExperimentSpec"
                    raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
                try:
                    diagnostics = _read_json(str(args.diagnostics))
                except QianxingError as exc:
                    raise InvalidInputError(
                        f"cannot read diagnostics attachment: {exc}",
                        payload={"category": "INVALID_INPUT", "message": str(exc)},
                    ) from exc
                if not isinstance(diagnostics, dict):
                    message = "diagnostics attachment must be a JSON object"
                    raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
                base_spec_sha256 = hashlib.sha256(
                    json.dumps(
                        payload["base"],
                        ensure_ascii=False,
                        sort_keys=True,
                        separators=(",", ":"),
                        allow_nan=False,
                    ).encode("utf-8")
                ).hexdigest()
                expected_identity = {
                    "kind": "qianxing-strategy-bias-diagnostics",
                    "strategy": "python_create_strategy_callback",
                    "strategy_id": strategy_id,
                    "strategy_version": strategy_version,
                    "strategy_bundle_sha256": source_sha256,
                    "backtest_spec_sha256": base_spec_sha256,
                }
                multi_market_base = isinstance(payload["base"].get("markets"), list)
                if multi_market_base:
                    (
                        expected_request,
                        expected_strategy_config,
                        _expected_factory_context,
                        expected_peer_frames,
                        expected_market_inputs,
                    ) = _multi_market_diagnostic_request(
                        payload["base"], strategy_id, str(strategy_version)
                    )
                    expected_identity["data_fingerprint"] = expected_request.data_fingerprint
                    expected_identity["instrument"] = expected_request.instrument
                    expected_identity["backtest_strategy_config"] = expected_strategy_config
                    attached_multi_market = diagnostics.get("multi_market")
                    if not isinstance(attached_multi_market, dict):
                        mismatches = ["multi_market"]
                    else:
                        mismatches = [
                            key
                            for key, expected in (
                                ("target_strategy_id", strategy_id),
                                ("market_count", len(payload["base"]["markets"])),
                                ("market_inputs", expected_market_inputs),
                                (
                                    "peer_market_instruments",
                                    sorted(expected_peer_frames),
                                ),
                                (
                                    "peer_market_bars_included",
                                    len(expected_peer_frames),
                                ),
                                (
                                    "point_in_time_ledger_scope",
                                    "target_market_only"
                                    if diagnostics.get("data_visibility") is not None
                                    else "not_supplied",
                                ),
                            )
                            if attached_multi_market.get(key) != expected
                        ]
                else:
                    expected_identity["data_fingerprint"] = full_document.get(
                        "input_data_fingerprint"
                    )
                    expected_identity["instrument"] = payload["base"].get("instrument")
                    mismatches = []
                mismatches = [
                    *mismatches,
                    *(
                        key
                        for key, expected in expected_identity.items()
                        if diagnostics.get(key) != expected
                    ),
                ]
                if mismatches:
                    message = "diagnostics attachment identity does not match this experiment: " + ", ".join(mismatches)
                    raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
                report_document = {**full_document, "diagnostics": diagnostics}
            try:
                base_spec = payload.get("base") if isinstance(payload, dict) else None
                if isinstance(base_spec, dict) and isinstance(base_spec.get("markets"), list):
                    if all(
                        isinstance(market, dict) and isinstance(market.get("bars_path"), str)
                        for market in base_spec["markets"]
                    ):
                        verified_details = load_verified_multi_market_candidate_details(
                            full_document,
                            market_validator=validate_dataset,
                            base_dir=Path.cwd(),
                        )
                    else:
                        verified_details = {
                            "schema_version": 1,
                            "runs": [],
                            "unsupported_scope": "当前 Python 搜索报告仅接入共享账户多市场 Bar 产物；Tick/L2 和多账户归因尚未从其独立产物导入。",
                        }
                else:
                    verified_details = load_verified_candidate_details(
                        full_document,
                        base_dir=Path.cwd(),
                        rust_report_reader=read_run_report_v1,
                    )
                report_document = {**report_document, "research_details": verified_details}
            except (OSError, ValueError) as exc:
                message = f"cannot verify candidate run details for HTML report: {exc}"
                raise InvalidInputError(
                    message, payload={"category": "INVALID_INPUT", "message": message}
                ) from exc
            try:
                report_path = write_experiment_html(
                    report_document,
                    args.html or None,
                    protected_paths=(
                        args.input,
                        args.strategy_file,
                        *_referenced_input_paths(payload),
                    ),
                )
            except ValueError as exc:
                raise InvalidInputError(
                    str(exc), payload={"category": "INVALID_INPUT", "message": str(exc)}
                ) from exc
            except OSError as exc:
                message = f"cannot write experiment HTML report: {exc}"
                raise DataUnavailableError(
                    message, payload={"category": "DATA_UNAVAILABLE", "message": message}
                ) from exc
            print(f"Interactive HTML report: {report_path}", file=sys.stderr)
        print(json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True))
        return 0
    except QianxingError as exc:
        error = exc.payload or {"category": exc.code, "message": str(exc)}
        record_project_error(error)
        print(json.dumps(error, ensure_ascii=False, sort_keys=True), file=sys.stderr)
        return 2


def main(argv: Sequence[str] | None = None) -> int:
    """CLI entry point; project services call their own APIs without re-entering this wrapper."""
    from qianxing_ashare.sources import AshareProviderError

    try:
        return _dispatch_command(argv)
    except AshareProviderError as error:
        record_project_error(json.loads(error.to_json()))
        print(error.to_json(), file=sys.stderr)
        return error.exit_code


if __name__ == "__main__":
    raise SystemExit(main())
