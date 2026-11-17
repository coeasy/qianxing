"""Select and validate the shared Rust report renderer without extra dependencies."""
from __future__ import annotations

import os
import shutil
from pathlib import Path
from typing import Any


class ReportRendererError(ValueError):
    def __init__(self, diagnostic: dict[str, Any]):
        self.diagnostic = diagnostic
        super().__init__(
            f"report renderer {diagnostic['status']}: {diagnostic['path'] or 'qx-cli'}; "
            f"{diagnostic['action']}"
        )


def _bundled_renderer() -> Path:
    directory = Path(__file__).resolve().parent / "_bin"
    expected = directory / ("qx-cli.exe" if os.name == "nt" else "qx-cli")
    # A damaged/wrong-platform wheel is not permission to use another CLI silently.
    alternate = directory / ("qx-cli" if os.name == "nt" else "qx-cli.exe")
    return alternate if not os.path.lexists(expected) and os.path.lexists(alternate) else expected


def _select_renderer(executable: str | os.PathLike[str] | None, cwd: Path | None) -> tuple[str | None, str]:
    if executable is not None:
        name = os.fspath(executable)
        if not name:
            return None, "external"
        candidate = Path(name)
        if candidate.is_absolute() or candidate.parent != Path(".") or "/" in name or "\\" in name:
            return str(((cwd or Path.cwd()) / candidate).resolve()), "external"
        found = shutil.which(name)
        return (str(Path(found).resolve()) if found else name), "external"
    bundled = _bundled_renderer()
    if os.path.lexists(bundled):
        return str(bundled.absolute()), "bundled"
    found = shutil.which("qx-cli")
    return (str(Path(found).resolve()), "external") if found else (None, "missing")


def probe_report_renderer(
    executable: str | os.PathLike[str] | None = None, *, cwd: Path | None = None,
    package_version: str, native_schema_sha256: str | None,
) -> dict[str, Any]:
    from .sdk_diagnostics import _probe_cli_identity

    path, origin = _select_renderer(executable, cwd)
    result = _probe_cli_identity(path, package_version, native_schema_sha256)
    version_compatibility = result["status"]
    if path is None:
        status = "missing"
    elif result.get("probe_error"):
        status = result["probe_error"]
    else:
        checks = (result["schema_catalog_compatibility"], result["platform_compatibility"])
        accepted_versions = {"matched"} if origin == "bundled" else {"matched", "compatible"}
        if version_compatibility == "mismatch" or "mismatch" in checks:
            status = "mismatch"
        elif version_compatibility in accepted_versions and checks == ("matched", "matched"):
            status = "ready"
        else:
            status = "unknown"
    repair = (
        f'python -m pip install --upgrade --force-reinstall "qianxing=={package_version}"'
    )
    return {
        **result,
        "origin": origin,
        "selection": "explicit" if executable is not None else origin,
        "status": status,
        "version_compatibility": version_compatibility,
        "action": None if status == "ready" else (
            "Reinstall the matching qianxing wheel or pass --cli PATH to a renderer with "
            "matching version, schema catalog and platform. No fallback was attempted."
        ),
        "repair_command": None if status == "ready" else repair,
    }


def require_report_renderer(
    executable: str | os.PathLike[str] | None = None, *, cwd: Path | None = None,
) -> dict[str, Any]:
    from qianxing_bridge import native
    from .sdk_diagnostics import _sdk_package_version

    result = probe_report_renderer(
        executable, cwd=cwd, package_version=_sdk_package_version(),
        native_schema_sha256=native.schema_catalog_sha256(),
    )
    if result["status"] != "ready":
        raise ReportRendererError(result)
    return result
