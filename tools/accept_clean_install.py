"""Build and exercise a clean local Python wheel and qx-cli installation.

This is an installation acceptance runner, not a unit-test suite. All build, venv,
and sample-project output is kept in a caller-provided directory outside the repo.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import shlex
import subprocess
import sys
import time
import xml.etree.ElementTree as ElementTree
from pathlib import Path
from typing import TextIO

import tomllib

DEFAULT_COMMAND_TIMEOUT_SECONDS = 300
BUILD_COMMAND_TIMEOUT_SECONDS = 2_400
QUICKSTART_COMPLETION_STEPS = (
    "建项目",
    "静态检查",
    "跑一轮回测",
    "复核摘要并生成 EvidenceBundle",
    "离线核验证据包",
    "看安全状态",
)
QUICKSTART_REPORT_ASSETS = ("html", "equity_svg", "fills_svg", "monthly_svg", "evidence")


def run(
    command: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout_seconds: int = DEFAULT_COMMAND_TIMEOUT_SECONDS,
) -> subprocess.CompletedProcess[str]:
    if timeout_seconds <= 0:
        raise ValueError("command timeout must be positive")
    print("$ " + shlex.join(command), flush=True)
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            check=False,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as error:
        for output, stream in ((error.stdout, sys.stdout), (error.stderr, sys.stderr)):
            if output:
                text = output.decode("utf-8", errors="replace") if isinstance(output, bytes) else output
                print(text, end="" if text.endswith("\n") else "\n", file=stream, flush=True)
        raise TimeoutError(
            f"command timed out after {timeout_seconds}s: {shlex.join(command)}"
        ) from error
    if result.stdout:
        print(result.stdout, end="" if result.stdout.endswith("\n") else "\n", flush=True)
    if result.stderr:
        print(result.stderr, end="" if result.stderr.endswith("\n") else "\n", file=sys.stderr, flush=True)
    if result.returncode:
        raise subprocess.CalledProcessError(result.returncode, command, result.stdout, result.stderr)
    return result


def ensure_cli_install(repo: Path, cli_root: Path, env: dict[str, str]) -> Path:
    cli_bin = cli_root / "bin"
    cli_name = "qx-cli.exe" if os.name == "nt" else "qx-cli"
    cli = cli_bin / cli_name
    if not cli.is_file():
        run(
            [
                "cargo", "install", "--path", str(repo / "crates/qx-cli"), "--locked", "--offline",
                "--root", str(cli_root), "--force",
            ],
            cwd=repo,
            env=env,
            timeout_seconds=BUILD_COMMAND_TIMEOUT_SECONDS,
        )
    if not cli.is_file():
        raise RuntimeError(f"cargo install did not produce {cli}")
    return cli


def configure_utf8_output(stream: TextIO) -> None:
    if hasattr(stream, "reconfigure"):
        stream.reconfigure(encoding="utf-8", errors="backslashreplace")


def _verified_artifact(path: Path, project: Path) -> dict[str, str | int]:
    resolved = path.expanduser().resolve()
    project_root = project.resolve()
    if not resolved.is_relative_to(project_root):
        raise RuntimeError(f"quickstart artifact escaped its project directory: {resolved}")
    if not resolved.is_file() or resolved.stat().st_size == 0:
        raise RuntimeError(f"quickstart artifact is missing or empty: {resolved}")
    size = resolved.stat().st_size
    digest = hashlib.sha256()
    with resolved.open("rb") as artifact:
        for chunk in iter(lambda: artifact.read(1024 * 1024), b""):
            digest.update(chunk)
    return {"path": str(resolved), "bytes": size, "sha256": digest.hexdigest()}


def verify_quickstart_acceptance(
    output: str, *, project: Path, elapsed_ms: float
) -> dict[str, object]:
    """Fail closed unless one visible CLI command completes the verified report path."""
    completed = [
        line.removeprefix("[完成] ").split("：", 1)[0]
        for line in output.splitlines()
        if line.startswith("[完成] ")
    ]
    if tuple(completed) != QUICKSTART_COMPLETION_STEPS:
        raise RuntimeError(
            "quickstart did not complete the six documented steps in order: "
            f"expected {QUICKSTART_COMPLETION_STEPS}, got {tuple(completed)}"
        )
    if "EvidenceBundle 已离线核验：" not in output:
        raise RuntimeError("quickstart did not report the verified EvidenceBundle path")

    verification = None
    for line in output.splitlines():
        try:
            candidate = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(candidate, dict) and candidate.get("operation") == (
            "run-evidence-input-and-artifact-verified"
        ):
            verification = candidate
            break
    if (
        verification is None
        or verification.get("ok") is not True
        or verification.get("input_identity_recomputed") is not True
    ):
        raise RuntimeError("quickstart did not perform successful offline EvidenceBundle verification")

    reported_paths: dict[str, Path] = {}
    for line in output.splitlines():
        for asset in QUICKSTART_REPORT_ASSETS:
            prefix = f"[Report] {asset}="
            if line.startswith(prefix):
                reported_paths[asset] = Path(line.removeprefix(prefix).strip())
    missing = set(QUICKSTART_REPORT_ASSETS) - reported_paths.keys()
    if missing:
        raise RuntimeError(f"quickstart omitted report assets: {sorted(missing)}")

    result_hashes = set(re.findall(r"\bresult_hash=([0-9a-f]{16,64})\b", output))
    if len(result_hashes) != 1:
        raise RuntimeError(
            "quickstart must expose exactly one stable result_hash; "
            f"found {sorted(result_hashes)}"
        )
    result_hash = next(iter(result_hashes))
    if verification.get("result_hash") != result_hash:
        raise RuntimeError("quickstart EvidenceBundle result identity does not match the backtest")
    evidence_line = next(
        line.removeprefix("EvidenceBundle 已离线核验：").strip()
        for line in output.splitlines()
        if line.startswith("EvidenceBundle 已离线核验：")
    )
    if Path(evidence_line).resolve() != reported_paths["evidence"].resolve():
        raise RuntimeError("quickstart's verified EvidenceBundle path does not match the report")
    assets = {name: _verified_artifact(path, project) for name, path in reported_paths.items()}
    html_path = Path(str(assets["html"]["path"]))
    html_source = html_path.read_text(encoding="utf-8")
    if "<!doctype html>" not in html_source.lower() or "<body" not in html_source.lower():
        raise RuntimeError(f"quickstart HTML report is not a complete document: {html_path}")
    for name in ("equity_svg", "fills_svg", "monthly_svg"):
        svg_path = Path(str(assets[name]["path"]))
        try:
            svg_root = ElementTree.parse(svg_path).getroot()
        except ElementTree.ParseError as error:
            raise RuntimeError(f"quickstart SVG report is invalid: {svg_path}: {error}") from error
        if svg_root.tag.rsplit("}", 1)[-1] != "svg":
            raise RuntimeError(f"quickstart report asset is not an SVG: {svg_path}")
    return {
        "profile": "builtin",
        "command_count_to_verified_report": 1,
        "completed_steps": completed,
        "result_hash": result_hash,
        "result_recomputed": verification.get("result_recomputed") is True,
        "evidence_verified_artifact_count": verification.get("verified_artifact_count"),
        "quickstart_elapsed_ms": round(elapsed_ms, 3),
        "report_artifacts": assets,
    }


def main() -> int:
    # Windows hosted runners may expose a legacy console code page (for example
    # cp1252), while qx-cli help and diagnostics intentionally contain Chinese.
    # Keep acceptance logs lossless and prevent an otherwise successful install
    # from failing while printing subprocess output.
    for stream in (sys.stdout, sys.stderr):
        configure_utf8_output(stream)

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", required=True, help="Python interpreter with pip and maturin")
    parser.add_argument("--work-dir", required=True, help="New directory outside the source tree")
    parser.add_argument("--wheel", help="Use an already-built wheel instead of building one")
    parser.add_argument("--cli-root", help="Reuse a qx-cli cargo-install root shared by related acceptance runs")
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[1]
    build_python = Path(args.python).resolve()
    work = Path(args.work_dir).resolve()
    if not build_python.is_file():
        raise SystemExit(f"Python interpreter not found: {build_python}")
    isolated_build_dir = repo / "target"
    if work == repo or (repo in work.parents and isolated_build_dir not in work.parents):
        raise SystemExit("work-dir must be outside the source tree (the ignored target directory is allowed)")
    if work.exists() and any(work.iterdir()):
        raise SystemExit(f"work-dir must be new or empty: {work}")
    work.mkdir(parents=True, exist_ok=True)

    project_dir = repo / "python"
    wheelhouse = work / "wheelhouse"
    wheelhouse.mkdir()
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(repo / "target")
    env["CARGO_PROFILE_RELEASE_DEBUG"] = "0"
    cargo_temp = repo / "target/r20-native-temp"
    cargo_temp.mkdir(parents=True, exist_ok=True)
    env["TEMP"] = str(cargo_temp)
    env["TMP"] = str(cargo_temp)
    env["CARGO_BUILD_JOBS"] = "1"
    if args.wheel:
        wheel = Path(args.wheel).resolve()
        if not wheel.is_file() or wheel.suffix != ".whl":
            raise SystemExit(f"wheel not found: {wheel}")
        wheels = [wheel]
    else:
        maturin_name = "maturin.exe" if os.name == "nt" else "maturin"
        maturin_candidates = (
            build_python.parent / maturin_name,
            build_python.parent / "Scripts" / maturin_name,
        )
        maturin = next((candidate for candidate in maturin_candidates if candidate.is_file()), None)
        if maturin is None:
            searched = ", ".join(str(candidate) for candidate in maturin_candidates)
            raise SystemExit(f"maturin executable not found beside Python or in Scripts/: {searched}")
        run(
            [
                str(maturin), "build", "--release", "--out", str(wheelhouse),
                "--interpreter", str(build_python),
            ],
            cwd=project_dir,
            env=env,
            timeout_seconds=BUILD_COMMAND_TIMEOUT_SECONDS,
        )
        wheels = sorted(wheelhouse.glob("qianxing-*.whl"))
    if len(wheels) != 1:
        raise RuntimeError(f"expected one qianxing wheel, found {[wheel.name for wheel in wheels]}")

    install_env_dir = work / "installed-venv"
    run([str(build_python), "-m", "venv", str(install_env_dir)], cwd=repo, env=env)
    scripts = install_env_dir / ("Scripts" if os.name == "nt" else "bin")
    python = scripts / ("python.exe" if os.name == "nt" else "python")
    wheel_install_started = time.perf_counter()
    run(
        [str(python), "-m", "pip", "install", "--no-index", str(wheels[0])],
        cwd=work,
        env=env,
    )
    wheel_install_elapsed_ms = round((time.perf_counter() - wheel_install_started) * 1000, 3)

    cli_root = Path(args.cli_root).resolve() if args.cli_root else work / "installed-cli"
    cli_setup_started = time.perf_counter()
    cli = ensure_cli_install(repo, cli_root, env)
    cli_setup_elapsed_ms = round((time.perf_counter() - cli_setup_started) * 1000, 3)
    cli_bin = cli.parent
    cli_env = env.copy()
    cli_env["PATH"] = str(cli_bin) + os.pathsep + str(scripts) + os.pathsep + cli_env.get("PATH", "")

    identity = run([str(cli), "version"], cwd=work, env=cli_env).stdout.strip()
    run([str(cli), "help"], cwd=work, env=cli_env)
    sample_parent = work / "quickstart"
    sample_parent.mkdir()
    quickstart_started = time.perf_counter()
    quickstart = run(
        [str(cli), "quickstart", "sample-project", "--html"], cwd=sample_parent, env=cli_env
    )
    quickstart_elapsed_ms = (time.perf_counter() - quickstart_started) * 1000
    project = sample_parent / "sample-project"
    if not (project / "qianxing.runtime.json").is_file():
        raise RuntimeError("quickstart did not create its runtime configuration")
    first_run = verify_quickstart_acceptance(
        quickstart.stdout, project=project, elapsed_ms=quickstart_elapsed_ms
    )

    smoke = r'''import json, pathlib, sys
import qianxing, qianxing.core, qianxing.strategy, qianxing.reports
from qianxing import BacktestSpec, BuiltinStrategySpec, doctor, list_builtin_strategies
report = doctor()
assert report["native_extension_compatibility"] == "matched", report
assert report["native_app_compatibility"] == "complete", report
assert report["application_use_cases_available"] is True, report
assert len(report["native_schema_catalog_sha256"] or "") == 64, report
assert report["native_platform_compatibility"] == "matched", report
assert report["cli"]["status"] == "matched", report["cli"]
assert report["cli"]["schema_catalog_compatibility"] == "matched", report["cli"]
assert report["cli"]["platform_compatibility"] == "matched", report["cli"]
assert report["cli"]["target"], report["cli"]
strategies = list_builtin_strategies()
assert strategies, "native built-in strategy capability catalog is empty"
spec = BacktestSpec("install-smoke", "BTCUSDT.BINANCE", "bars.json", "USDT", 1000000000, "runs", BuiltinStrategySpec(kind="sma_cross", strategy_id="install-smoke"))
assert spec.to_dict()["schema_version"] == 1
assert pathlib.Path(qianxing.__file__).resolve().is_relative_to(pathlib.Path(sys.prefix).resolve())
print(json.dumps({"version": report["version"], "schema_catalog_sha256": report["native_schema_catalog_sha256"], "native_app": report["native_app_compatibility"], "cli": report["cli"]["status"], "builtin_strategy_count": len(strategies), "backtest_schema_version": spec.to_dict()["schema_version"], "native_extension_target": report["native_extension_target"], "native_platform_compatibility": report["native_platform_compatibility"], "cli_target": report["cli"]["target"], "cli_platform_compatibility": report["cli"]["platform_compatibility"]}, sort_keys=True))'''
    smoke_result = run([str(python), "-c", smoke], cwd=work, env=cli_env)
    smoke_report = json.loads(smoke_result.stdout.strip().splitlines()[-1])

    examples = sorted((project_dir / "examples").glob("*.py"))
    for example in examples:
        compile(example.read_text(encoding="utf-8"), str(example), "exec")
    print(f"Compiled {len(examples)} documented Python example modules.", flush=True)

    momentum_path = project_dir / "examples/backtest_momentum.py"
    spec_from_file = importlib.util.spec_from_file_location("qx_install_example", momentum_path)
    if spec_from_file is None or spec_from_file.loader is None:
        raise RuntimeError(f"could not load example {momentum_path}")
    example_module = importlib.util.module_from_spec(spec_from_file)
    spec_from_file.loader.exec_module(example_module)
    from types import SimpleNamespace
    event = SimpleNamespace(
        bars=SimpleNamespace(close_raw=[100, 101]), schema_version=1, request_id="install",
        strategy_id="install-example", as_of=2, instrument="BTCUSDT.BINANCE",
    )
    output = example_module.on_event(event)
    if output.get("schema_version") != 1 or output.get("request_id") != "install":
        raise RuntimeError("installed SDK environment could not execute the packaged strategy example contract")

    metadata = tomllib.loads((project_dir / "pyproject.toml").read_text(encoding="utf-8"))
    result = {
        "platform": sys.platform,
        "python": str(build_python),
        "wheel": wheels[0].name,
        "package_version": metadata["project"]["version"],
        "cli_identity": identity,
        "quickstart_exit": quickstart.returncode,
        "quickstart_project": str(project),
        "first_run": first_run,
        "quickstart_user_command_count": first_run["command_count_to_verified_report"],
        "wheel_install_elapsed_ms": wheel_install_elapsed_ms,
        "cli_setup_elapsed_ms": cli_setup_elapsed_ms,
        "measured_install_to_verified_report_ms": round(
            wheel_install_elapsed_ms + cli_setup_elapsed_ms + quickstart_elapsed_ms, 3
        ),
        "measured_install_to_report_scope": (
            "local wheel pip install + CLI setup/availability + one qx-cli quickstart --html; "
            "excludes venv creation and package build"
        ),
        "python_example_modules_compiled": len(examples),
        "python_strategy_example_invoked": True,
        "native_extension_target": smoke_report["native_extension_target"],
        "native_platform_compatibility": smoke_report["native_platform_compatibility"],
        "cli_target": smoke_report["cli_target"],
        "cli_platform_compatibility": smoke_report["cli_platform_compatibility"],
        "work_dir": str(work),
    }
    (work / "acceptance-result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
