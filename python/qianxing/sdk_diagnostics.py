"""Installed Python/native/CLI compatibility diagnostics for the SDK."""

from __future__ import annotations

import platform
import re
import shutil
import subprocess
import sys
import sysconfig
from importlib.metadata import PackageNotFoundError, version
from pathlib import Path
from typing import Any

from qianxing_bridge import native


def _normalize_architecture(value: str) -> str:
    value = value.lower()
    return {
        "amd64": "x86_64",
        "x64": "x86_64",
        "arm64": "aarch64",
    }.get(value, value)


def _compare_cli_platform(target: str | None) -> str:
    if not target:
        return "unknown"
    target = target.lower()
    python_arch = _normalize_architecture(platform.machine())
    cli_arch = _normalize_architecture(target.split("-", 1)[0])
    if python_arch != cli_arch:
        return "mismatch"
    if sys.platform == "win32":
        return "matched" if "windows" in target else "mismatch"
    if sys.platform == "darwin":
        return "matched" if "apple" in target and "darwin" in target else "mismatch"
    if sys.platform.startswith("linux"):
        if "linux" not in target:
            return "mismatch"
        python_libc = platform.libc_ver()[0].lower()
        cli_uses_musl = "musl" in target
        if (python_libc == "musl") != cli_uses_musl:
            return "mismatch"
        return "matched"
    return "unknown"


def _sdk_package_version() -> str:
    # In a source checkout, a globally installed distribution can have stale metadata while
    # its Python package is shadowed by this checkout. Prefer this checkout's project version.
    project_file = Path(__file__).resolve().parents[1] / "pyproject.toml"
    if project_file.is_file():
        project_text = project_file.read_text(encoding="utf-8")
        project_match = re.search(r"(?ms)^\[project\]\s*(.*?)(?=^\[|\Z)", project_text)
        version_match = re.search(
            r'^\s*version\s*=\s*"([^\"]+)"',
            project_match.group(1) if project_match else "",
            re.MULTILINE,
        )
        if version_match:
            return version_match.group(1)
    try:
        return version("qianxing")
    except PackageNotFoundError:
        return "0.4.0"


def _probe_qx_cli(
    package_version: str, native_schema_sha256: str | None
) -> dict[str, Any]:
    # This public diagnostic deliberately describes PATH, not the wheel renderer.
    return _probe_cli_identity(shutil.which("qx-cli"), package_version, native_schema_sha256)


def _probe_cli_identity(
    executable: str | None, package_version: str, native_schema_sha256: str | None
) -> dict[str, Any]:
    install_command = (
        "cargo install --git https://github.com/coeasy/qianxing "
        f"--tag v{package_version} qx-cli --locked --force"
    )
    if executable is None:
        return {
            "status": "unavailable",
            "version": None,
            "target": None,
            "platform_compatibility": "unavailable",
            "schema_catalog_sha256": None,
            "schema_catalog_compatibility": "unavailable",
            "path": None,
            "action": "Install the qx-cli release matching this SDK.",
            "repair_command": install_command,
        }
    try:
        result = subprocess.run(
            [executable, "version"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {
            "status": "unavailable",
            "probe_error": (
                "timeout" if isinstance(exc, subprocess.TimeoutExpired) else
                "missing" if isinstance(exc, FileNotFoundError) else "unavailable"
            ),
            "version": None,
            "target": None,
            "platform_compatibility": "unavailable",
            "schema_catalog_sha256": None,
            "schema_catalog_compatibility": "unavailable",
            "path": executable,
            "action": f"qx-cli could not report its build identity ({exc}).",
            "repair_command": install_command,
        }
    identity = (result.stdout or result.stderr).strip()
    version_match = re.search(r"(?:qianxing|qx-cli)\s+(\d+\.\d+\.\d+(?:[-+][\w.-]+)?)", identity)
    target_match = re.search(r"\btarget\s+([^, )]+)", identity)
    schema_match = re.search(r"\bschemas\s+([0-9a-fA-F]{64})\b", identity)
    cli_version = version_match.group(1) if version_match else None
    target = target_match.group(1) if target_match else None
    cli_schema_sha256 = schema_match.group(1).lower() if schema_match else None
    if result.returncode != 0 or cli_version is None:
        status = "unknown"
    elif cli_version == package_version:
        status = "matched"
    elif cli_version.rsplit(".", 1)[0] == package_version.rsplit(".", 1)[0]:
        status = "compatible"
    else:
        status = "mismatch"
    action = None if status in {"matched", "compatible"} else (
        "Install a qx-cli build with a readable version identity."
        if status == "unknown"
        else "Install the qx-cli release matching this SDK."
    )
    schema_compatibility = (
        "unknown"
        if cli_schema_sha256 is None or native_schema_sha256 is None
        else "matched"
        if cli_schema_sha256 == native_schema_sha256
        else "mismatch"
    )
    platform_compatibility = _compare_cli_platform(target)
    return {
        "status": status,
        "version": cli_version,
        "target": target,
        "platform_compatibility": platform_compatibility,
        "schema_catalog_sha256": cli_schema_sha256,
        "schema_catalog_compatibility": schema_compatibility,
        "path": executable,
        "identity": identity or None,
        "action": action,
        "repair_command": None if action is None else install_command,
    }


def _strategy_runtimes(app_available: bool) -> dict[str, Any]:
    remote = {}
    for runtime, distribution, extra in (
        ("e2b", "e2b", "sandbox-e2b"),
        ("llm_sandbox", "llm-sandbox", "sandbox-llm"),
        ("opensandbox", "opensandbox", "sandbox-opensandbox"),
        ("microsandbox", "microsandbox", "sandbox-microsandbox"),
    ):
        # Distribution metadata only: doctor must not import optional runtimes,
        # start containers, contact services, or interpret installation as readiness.
        try:
            installed_version = version(distribution)
        except PackageNotFoundError:
            installed_version = None
        remote[runtime] = {
            "distribution": distribution,
            "extra": extra,
            "installed": installed_version is not None,
            "version": installed_version,
            "readiness": "unverified",
            "configured": None,
            "running": None,
        }
    return {
        "local_default": "trusted_python",
        "local": {"trusted_python": {"available": app_available}},
        "remote_selected": None,
        "remote_readiness": "unverified",
        "remote": remote,
        "note": "Local trusted_python is the default. Installed remote packages are not configured or running; no remote runtime is selected and readiness is unverified.",
    }


def doctor() -> dict[str, Any]:
    """Report which native application entry points are available in this install."""
    from .report_renderer import probe_report_renderer

    extension_available = native.available()
    app_available = native.app_available()
    package_version = _sdk_package_version()
    native_version = native.extension_version()
    native_schema_sha256 = native.schema_catalog_sha256()
    native_target = native.extension_target_triple()
    missing_entrypoints = native.missing_app_entrypoints()
    native_app_compatibility = "unavailable" if not extension_available else (
        "complete" if not missing_entrypoints else "incomplete"
    )
    compatibility = (
        "unavailable" if not extension_available else
        "unknown" if native_version is None else
        "matched" if native_version == package_version else
        "mismatch"
    )
    sdk_repair_command = (
        f'python -m pip install --upgrade --force-reinstall "qianxing=={package_version}"'
    )
    compatibility_action = {
        "unavailable": "Install the qianxing wheel for this Python environment.",
        "unknown": "Reinstall or rebuild the qianxing wheel so the native extension exposes its build version.",
        "mismatch": "Install the Python SDK and native extension from the same qianxing wheel build.",
        "matched": None,
    }[compatibility]
    if compatibility_action:
        compatibility_action += f" Repair: {sdk_repair_command}"
    cli = _probe_qx_cli(package_version, native_schema_sha256)
    implemented_use_cases = native.app_implemented_use_cases() if app_available else []
    return {
        "package": "qianxing",
        "version": package_version,
        "native_extension_version": native_version,
        "native_extension_compatibility": compatibility,
        "native_extension_action": compatibility_action,
        "native_schema_catalog_sha256": native_schema_sha256,
        "schema_catalog_compatibility": cli["schema_catalog_compatibility"],
        "native_extension_target": native_target,
        "native_platform_compatibility": (
            _compare_cli_platform(native_target) if native_target else "unknown"
        ),
        "native_extension_repair_command": (
            sdk_repair_command if compatibility != "matched" else None
        ),
        "cli": cli,
        "report_renderer": probe_report_renderer(
            package_version=package_version, native_schema_sha256=native_schema_sha256,
        ),
        "strategy_runtimes": _strategy_runtimes(app_available),
        "platform": {
            "python_implementation": platform.python_implementation(),
            "python_version": platform.python_version(),
            "python_cache_tag": sys.implementation.cache_tag,
            "python_platform": sysconfig.get_platform(),
            "sys_platform": sys.platform,
        },
        "native_app_compatibility": native_app_compatibility,
        "missing_native_entrypoints": missing_entrypoints,
        "native_app_action": (
            "Reinstall or rebuild the qianxing wheel; its native extension is missing required application entrypoints."
            if missing_entrypoints else None
        ),
        "native_extension_available": extension_available,
        "application_use_cases_available": app_available,
        "implemented_use_cases": implemented_use_cases,
        "versioned_application_contracts": implemented_use_cases,
        "remote_workflows": ["control.paper.v1", "control.live.v1"],
        "limitations": [
            "Paper/Live submission requires a running qx-api service with matching workers; Live additionally requires authenticated operator policy.",
        ],
    }
