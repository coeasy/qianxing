"""Build and exercise a clean local Python wheel and qx-cli installation.

This is an installation acceptance runner, not a unit-test suite. All build, venv,
and sample-project output is kept in a caller-provided directory outside the repo.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shlex
import subprocess
import sys
from pathlib import Path

import tomllib


def run(command: list[str], *, cwd: Path, env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    print("$ " + shlex.join(command), flush=True)
    result = subprocess.run(
        command,
        cwd=cwd,
        env=env,
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=True,
        check=False,
    )
    if result.stdout:
        print(result.stdout, end="" if result.stdout.endswith("\n") else "\n", flush=True)
    if result.stderr:
        print(result.stderr, end="" if result.stderr.endswith("\n") else "\n", file=sys.stderr, flush=True)
    if result.returncode:
        raise subprocess.CalledProcessError(result.returncode, command, result.stdout, result.stderr)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", required=True, help="Python interpreter with pip and maturin")
    parser.add_argument("--work-dir", required=True, help="New directory outside the source tree")
    parser.add_argument("--wheel", help="Use an already-built wheel instead of building one")
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
        )
        wheels = sorted(wheelhouse.glob("qianxing-*.whl"))
    if len(wheels) != 1:
        raise RuntimeError(f"expected one qianxing wheel, found {[wheel.name for wheel in wheels]}")

    install_env_dir = work / "installed-venv"
    run([str(build_python), "-m", "venv", str(install_env_dir)], cwd=repo, env=env)
    scripts = install_env_dir / ("Scripts" if os.name == "nt" else "bin")
    python = scripts / ("python.exe" if os.name == "nt" else "python")
    run(
        [str(python), "-m", "pip", "install", "--no-index", str(wheels[0])],
        cwd=work,
        env=env,
    )

    cli_root = work / "installed-cli"
    run(
        [
            "cargo", "install", "--path", str(repo / "crates/qx-cli"), "--locked", "--offline",
            "--root", str(cli_root), "--force",
        ],
        cwd=repo,
        env=env,
    )
    cli_bin = cli_root / "bin"
    cli_env = env.copy()
    cli_env["PATH"] = str(cli_bin) + os.pathsep + str(scripts) + os.pathsep + cli_env.get("PATH", "")
    cli_name = "qx-cli.exe" if os.name == "nt" else "qx-cli"
    cli = cli_bin / cli_name
    if not cli.is_file():
        raise RuntimeError(f"cargo install did not produce {cli}")

    identity = run([str(cli), "version"], cwd=work, env=cli_env).stdout.strip()
    run([str(cli), "help"], cwd=work, env=cli_env)
    sample_parent = work / "quickstart"
    sample_parent.mkdir()
    quickstart = run([str(cli), "quickstart", "sample-project"], cwd=sample_parent, env=cli_env)
    project = sample_parent / "sample-project"
    if not (project / "qianxing.runtime.json").is_file():
        raise RuntimeError("quickstart did not create its runtime configuration")

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
