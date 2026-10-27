"""Optional remote strategy research execution.

Local ``run-strategy``/``research`` never imports this module's provider SDK. A
remote run is an explicit batch job: source and declared local inputs are
staged into an ephemeral sandbox, while exchange credentials are never copied.
"""

from __future__ import annotations

import asyncio
import ast
import hashlib
import importlib.metadata
import io
import json
import os
import re
import shlex
import shutil
import stat
import tempfile
import threading
import uuid
import zipfile
from datetime import timedelta
from pathlib import Path, PurePosixPath
from typing import Any, Protocol

from qianxing_strategy.sandbox_audit import (
    SandboxExecutionAudit,
)
from qianxing_strategy.sandbox_audit import (
    failure_code as sandbox_failure_code,
)

MAX_INPUT_BYTES = 128 * 1024 * 1024
MAX_OUTPUT_ARCHIVE_BYTES = 128 * 1024 * 1024
MAX_OUTPUT_EXPANDED_BYTES = 256 * 1024 * 1024
MAX_OUTPUT_FILES = 20_000
DEFAULT_TIMEOUT_SECONDS = 900

_DATA_PATH_KEYS = {
    "path",
    "file",
    "file_path",
    "input_path",
    "source_path",
    "data_path",
    "bars_path",
    "ticks_path",
    "trades_path",
    "book_path",
    "csv_path",
}
_SECRET_KEYS = {
    "api_key",
    "apikey",
    "api_secret",
    "secret",
    "password",
    "passphrase",
    "private_key",
    "access_token",
    "refresh_token",
    "authorization",
    "credential",
    "credentials",
    "token",
    "jwt",
    "client_secret",
    "secret_key",
    "secretkey",
    "apisecret",
}
_SENSITIVE_INPUT_NAMES = {
    ".netrc",
    "credentials",
    "credentials.json",
    "credentials.toml",
    "secrets",
    "secrets.json",
    "secrets.toml",
    "id_rsa",
    "id_ed25519",
}
_SENSITIVE_INPUT_SUFFIXES = {".pem", ".key", ".p12", ".pfx", ".keystore"}
_CREDENTIAL_ASSIGNMENT_PATTERN = re.compile(
    rb"(?im)\b(?:api[_-]?(?:key|secret)|secret[_-]?key|passphrase|password|"
    rb"access[_-]?token|refresh[_-]?token|authorization)\b\s*[:=]\s*[\"']?\S{6,}"
)
_INLINE_CREDENTIAL_PATTERN = re.compile(
    rb"(?im)\b(?:api[_-]?(?:key|secret)|secret[_-]?key|passphrase|password|"
    rb"access[_-]?token|refresh[_-]?token|authorization)\b\s*[:=]\s*"
    rb"(?P<quote>[\"'])(?P<value>[^\"'\r\n]{8,})(?P=quote)"
)
_REMOTE_ROOT = "/workspace"
_REMOTE_OUTPUT = f"{_REMOTE_ROOT}/output"
_OCI_SHA256_IMAGE_REFERENCE = re.compile(r"^.+@sha256:[0-9a-fA-F]{64}$")
_MICROSANDBOX_RESEARCH_BACKEND_LOCK = threading.Lock()


class StrategySandboxProvider(Protocol):
    """Host-owned provider seam for batch strategy research jobs."""

    def run_research(self, **kwargs: Any) -> dict[str, Any]: ...


class SandboxExecutionError(RuntimeError):
    """A sandbox job failed before it produced a usable research result."""


def _require_digest_pinned_image(image: str, setting: str) -> str:
    """Require immutable OCI content identity for self-hosted sandbox images."""
    if not isinstance(image, str) or not _OCI_SHA256_IMAGE_REFERENCE.fullmatch(
        image.strip()
    ):
        raise ValueError(
            f"{setting} must use an OCI SHA-256 image reference: "
            "repository@sha256:<64 hexadecimal characters>"
        )
    return image.strip()


def _sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _read_json_document(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise ValueError(
            f"cannot read research input {str(path)!r}: {error}"
        ) from error
    except json.JSONDecodeError as error:
        raise ValueError(f"research input is invalid JSON: {error}") from error
    if not isinstance(value, dict):
        raise TypeError("remote research input must be a JSON object")
    return value


def _reject_credentials(value: Any, path: str = "$") -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            raw_key = str(key).strip()
            normalized = (
                re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", raw_key)
                .lower()
                .replace("-", "_")
            )
            compact = re.sub(r"[^a-z0-9]", "", normalized)
            if (
                normalized in _SECRET_KEYS
                or compact
                in {
                    "apikey",
                    "apisecret",
                    "secretkey",
                    "clientsecret",
                    "accesstoken",
                    "refreshtoken",
                    "token",
                    "jwt",
                    "passphrase",
                    "privatekey",
                    "authorization",
                    "credential",
                    "credentials",
                    "password",
                }
                or normalized.endswith(("_secret", "_token", "_apikey", "_password"))
            ):
                raise ValueError(
                    f"remote research input contains a credential-like field at {path}.{raw_key}; "
                    "remove credentials before uploading a strategy or research bundle"
                )
            _reject_credentials(child, f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            _reject_credentials(child, f"{path}[{index}]")


def _reject_sensitive_input_file(path: Path, content: bytes) -> None:
    """Reject obvious credential files before data inputs leave the host."""
    name = path.name.casefold()
    if (
        name in _SENSITIVE_INPUT_NAMES
        or name.startswith(".env")
        or path.suffix.casefold() in _SENSITIVE_INPUT_SUFFIXES
        or b"-----BEGIN " in content
        and b"PRIVATE KEY-----" in content
        or _CREDENTIAL_ASSIGNMENT_PATTERN.search(content) is not None
    ):
        raise ValueError(
            f"remote input file {path.name!r} appears to contain credentials or private-key material; "
            "remove it from strategy data inputs"
        )


def _reject_inline_strategy_credentials(path: Path, content: bytes) -> None:
    if (
        b"-----BEGIN " in content and b"PRIVATE KEY-----" in content
    ) or _INLINE_CREDENTIAL_PATTERN.search(content) is not None:
        raise ValueError(
            f"strategy source {path.name!r} contains an inline credential or private key; "
            "remove secrets before uploading the strategy"
        )


def _module_candidate(root: Path, parts: tuple[str, ...]) -> Path | None:
    if not parts or any(part in {"", ".", ".."} for part in parts):
        return None
    base = root.joinpath(*parts)
    candidates = (base.with_suffix(".py"), base / "__init__.py")
    for candidate in candidates:
        try:
            resolved = candidate.resolve(strict=True)
            resolved.relative_to(root)
        except (OSError, ValueError):
            continue
        if resolved.is_file() and resolved.suffix.lower() == ".py":
            return resolved
    return None


def _strategy_python_files(entry: Path) -> dict[str, bytes]:
    """Collect the entry file and static local Python imports without importing code."""
    root = entry.parent.resolve(strict=True)
    pending = [entry.resolve(strict=True)]
    seen: set[Path] = set()
    files: dict[str, bytes] = {}

    def add_dependency(raw: str) -> None:
        portable = PurePosixPath(raw.replace("\\", "/"))
        if (
            portable.is_absolute()
            or ".." in portable.parts
            or portable.suffix.lower() != ".py"
        ):
            raise ValueError(f"invalid strategy dependency path: {raw!r}")
        candidate = (root / Path(*portable.parts)).resolve(strict=True)
        try:
            candidate.relative_to(root)
        except ValueError as error:
            raise ValueError(
                f"strategy dependency escapes its source directory: {raw!r}"
            ) from error
        if not candidate.is_file():
            raise ValueError(f"strategy dependency is not a regular file: {raw!r}")
        pending.append(candidate)

    while pending:
        path = pending.pop().resolve(strict=True)
        if path in seen:
            continue
        try:
            relative = path.relative_to(root)
        except ValueError as error:
            raise ValueError(
                f"strategy source import escapes its directory: {path}"
            ) from error
        if not path.is_file() or path.suffix.lower() != ".py":
            raise ValueError(f"strategy source dependency is not a Python file: {path}")
        source = path.read_text(encoding="utf-8")
        _reject_inline_strategy_credentials(path, source.encode("utf-8"))
        try:
            tree = ast.parse(source, filename=str(path))
        except SyntaxError as error:
            raise ValueError(
                f"cannot inspect strategy imports in {path}: {error}"
            ) from error
        files[relative.as_posix()] = source.encode("utf-8")
        seen.add(path)

        package_parts = relative.parent.parts
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign) and any(
                isinstance(target, ast.Name) and target.id == "STRATEGY_DEPENDENCIES"
                for target in node.targets
            ):
                try:
                    declared = ast.literal_eval(node.value)
                except (ValueError, TypeError):
                    raise ValueError(
                        "STRATEGY_DEPENDENCIES must be a literal tuple/list of paths"
                    )
                if not isinstance(declared, (tuple, list)) or any(
                    not isinstance(item, str) or not item.strip() for item in declared
                ):
                    raise ValueError(
                        "STRATEGY_DEPENDENCIES must contain non-empty paths"
                    )
                for item in declared:
                    add_dependency(item)
            elif (
                isinstance(node, ast.AnnAssign)
                and isinstance(node.target, ast.Name)
                and node.target.id == "STRATEGY_DEPENDENCIES"
            ):
                try:
                    declared = (
                        ast.literal_eval(node.value) if node.value is not None else None
                    )
                except (ValueError, TypeError):
                    raise ValueError(
                        "STRATEGY_DEPENDENCIES must be a literal tuple/list of paths"
                    )
                if not isinstance(declared, (tuple, list)) or any(
                    not isinstance(item, str) or not item.strip() for item in declared
                ):
                    raise ValueError(
                        "STRATEGY_DEPENDENCIES must contain non-empty paths"
                    )
                for item in declared:
                    add_dependency(item)
            elif isinstance(node, ast.Import):
                for alias in node.names:
                    candidate = _module_candidate(root, tuple(alias.name.split(".")))
                    if candidate is not None:
                        pending.append(candidate)
            elif isinstance(node, ast.ImportFrom):
                if node.level:
                    base_parts = package_parts[
                        : max(0, len(package_parts) - node.level + 1)
                    ]
                    module_parts = tuple(node.module.split(".")) if node.module else ()
                    imported_parts = base_parts + module_parts
                else:
                    imported_parts = (
                        tuple(node.module.split(".")) if node.module else ()
                    )
                candidate = _module_candidate(root, imported_parts)
                if candidate is not None:
                    pending.append(candidate)
                for alias in node.names:
                    if alias.name == "*":
                        continue
                    nested = _module_candidate(
                        root, imported_parts + tuple(alias.name.split("."))
                    )
                    if nested is not None:
                        pending.append(nested)
    return files


def _resolve_data_path(raw: str) -> Path:
    candidate = Path(raw).expanduser()
    if not candidate.is_absolute():
        candidate = Path.cwd() / candidate
    try:
        resolved = candidate.resolve(strict=True)
    except OSError as error:
        raise ValueError(
            f"remote research input file does not exist: {raw!r}"
        ) from error
    if not resolved.is_file():
        raise ValueError(f"remote research input must be a regular file: {raw!r}")
    return resolved


def _bundle_remote_inputs(
    strategy_file: Path,
    spec: dict[str, Any],
    *,
    package_version: str,
    job_id: str,
) -> tuple[bytes, dict[str, Any]]:
    _reject_credentials(spec)
    files = {
        f"strategy/{name}": content
        for name, content in _strategy_python_files(strategy_file).items()
    }
    path_map: dict[str, str] = {}
    data_files: list[dict[str, Any]] = []
    total_bytes = sum(len(content) for content in files.values())

    def collect(value: Any) -> None:
        nonlocal total_bytes
        if isinstance(value, dict):
            for key, child in value.items():
                if key in _DATA_PATH_KEYS and isinstance(child, str) and child.strip():
                    source = _resolve_data_path(child)
                    source_key = os.path.normcase(str(source))
                    if source_key not in path_map:
                        content = source.read_bytes()
                        _reject_sensitive_input_file(source, content)
                        total_bytes += len(content)
                        if total_bytes > MAX_INPUT_BYTES:
                            raise ValueError(
                                f"remote research bundle exceeds {MAX_INPUT_BYTES} bytes; "
                                "use a remote dataset location or a smaller input set"
                            )
                        digest = _sha256(content)
                        remote = f"/workspace/input/data/{digest[:16]}-{source.name}"
                        archive_path = f"input/data/{digest[:16]}-{source.name}"
                        files[archive_path] = content
                        path_map[source_key] = remote
                        data_files.append(
                            {
                                "path": archive_path,
                                "sha256": digest,
                                "bytes": len(content),
                            }
                        )
                else:
                    collect(child)
        elif isinstance(value, list):
            for child in value:
                collect(child)

    collect(spec)
    if total_bytes > MAX_INPUT_BYTES:
        raise ValueError(f"remote research bundle exceeds {MAX_INPUT_BYTES} bytes")

    def rewrite(value: Any, key: str = "", pointer: str = "root") -> Any:
        if isinstance(value, dict):
            return {
                item_key: rewrite(item, item_key, f"{pointer}.{item_key}")
                for item_key, item in value.items()
            }
        if isinstance(value, list):
            return [
                rewrite(item, key, f"{pointer}[{index}]")
                for index, item in enumerate(value)
            ]
        if key in _DATA_PATH_KEYS and isinstance(value, str) and value.strip():
            source = _resolve_data_path(value)
            try:
                return path_map[os.path.normcase(str(source))]
            except KeyError as error:
                raise ValueError(
                    f"research data input was not packaged: {value!r}"
                ) from error
        if key == "output_dir" and isinstance(value, str):
            suffix = hashlib.sha256(f"{pointer}\0{value}".encode()).hexdigest()[:12]
            return f"{_REMOTE_OUTPUT}/artifacts/{suffix}"
        return value

    remote_spec = rewrite(spec)
    spec_bytes = (
        json.dumps(
            remote_spec, ensure_ascii=False, sort_keys=True, indent=2, allow_nan=False
        )
        + "\n"
    ).encode("utf-8")
    files["input/spec.json"] = spec_bytes
    strategy_root = strategy_file.parent.resolve(strict=True)
    strategy_relative = (
        strategy_file.resolve(strict=True).relative_to(strategy_root).as_posix()
    )
    input_spec_sha256 = _sha256(
        json.dumps(
            spec,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    )
    strategy_digest = hashlib.sha256()
    strategy_digest.update(b"qianxing-strategy-transfer-v1\0")
    strategy_files: list[dict[str, Any]] = []
    for archive_path, content in sorted(files.items()):
        if not archive_path.startswith("strategy/"):
            continue
        relative = archive_path.removeprefix("strategy/").encode("utf-8")
        strategy_files.append(
            {
                "path": archive_path,
                "sha256": _sha256(content),
                "bytes": len(content),
            }
        )
        strategy_digest.update(len(relative).to_bytes(4, "big"))
        strategy_digest.update(relative)
        strategy_digest.update(len(content).to_bytes(8, "big"))
        strategy_digest.update(content)
    manifest = {
        "schema_version": 1,
        "kind": "qianxing-remote-research-input",
        "job_id": job_id,
        "package_version": package_version,
        "strategy_entrypoint": f"/workspace/strategy/{strategy_relative}",
        "strategy_transfer_sha256": strategy_digest.hexdigest(),
        "strategy_files": strategy_files,
        "input_spec_sha256": input_spec_sha256,
        "remote_spec_sha256": _sha256(spec_bytes),
        "data_files": sorted(data_files, key=lambda item: item["path"]),
    }
    files["input/job-manifest.json"] = (
        json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n"
    ).encode("utf-8")
    total_bytes += len(spec_bytes) + len(files["input/job-manifest.json"])
    if total_bytes > MAX_INPUT_BYTES:
        raise ValueError(f"remote research bundle exceeds {MAX_INPUT_BYTES} bytes")
    archive_buffer = io.BytesIO()
    with zipfile.ZipFile(
        archive_buffer, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6
    ) as archive:
        for archive_path, content in sorted(files.items()):
            archive.writestr(archive_path, content)
    packed = archive_buffer.getvalue()
    if len(packed) > MAX_INPUT_BYTES:
        raise ValueError(
            f"compressed remote research bundle exceeds {MAX_INPUT_BYTES} bytes"
        )
    return packed, manifest


def _output_pack_script() -> str:
    return "\n".join(
        [
            "import os, stat, zipfile",
            "root='/workspace/output'; archive='/workspace/qianxing-output.zip'",
            f"max_files={MAX_OUTPUT_FILES}; max_bytes={MAX_OUTPUT_EXPANDED_BYTES}; max_archive={MAX_OUTPUT_ARCHIVE_BYTES}",
            "entries=[]; total=0; failure=None",
            "for base, dirs, names in os.walk(root, followlinks=False):",
            "    dirs[:] = [name for name in dirs if not os.path.islink(os.path.join(base, name))]",
            "    for name in names:",
            "        path=os.path.join(base, name)",
            "        if os.path.islink(path) or not stat.S_ISREG(os.stat(path, follow_symlinks=False).st_mode): continue",
            "        size=os.path.getsize(path); total+=size; entries.append(path)",
            "        if len(entries)>max_files or total>max_bytes: failure='remote output exceeded file-count or expanded-size limit'; break",
            "    if failure: break",
            "if failure:",
            "    for name in os.listdir(root):",
            "        path=os.path.join(root, name)",
            "        if os.path.isfile(path) and not os.path.islink(path): os.remove(path)",
            "        elif os.path.isdir(path) and not os.path.islink(path): __import__('shutil').rmtree(path)",
            "    open(os.path.join(root, 'exit-code'), 'w').write('79\\n')",
            "    open(os.path.join(root, 'research.stderr.log'), 'w').write(failure+'\\n')",
            "    entries=[os.path.join(root, name) for name in ('exit-code', 'research.stderr.log')]",
            "with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as z:",
            "    for path in entries: z.write(path, os.path.relpath(path, root).replace(os.sep, '/'))",
            "if os.path.getsize(archive)>max_archive:",
            "    os.remove(archive)",
            "    for name in ('exit-code', 'research.stderr.log'):",
            "        path=os.path.join(root, name)",
            "        if os.path.isfile(path): os.remove(path)",
            "    open(os.path.join(root, 'exit-code'), 'w').write('79\\n')",
            "    open(os.path.join(root, 'research.stderr.log'), 'w').write('remote output archive exceeded compressed-size limit\\n')",
            "    with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as z:",
            "        z.write(os.path.join(root, 'exit-code'), 'exit-code'); z.write(os.path.join(root, 'research.stderr.log'), 'research.stderr.log')",
        ]
    )


def _input_preflight_script(package_version: str) -> str:
    return f"""
import hashlib
import importlib.metadata
import json
from pathlib import Path, PurePosixPath

root = Path('/workspace').resolve()
expected_version = {package_version!r}

def fail(message):
    raise SystemExit('remote input verification failed: ' + message)

def read_bundle_path(raw):
    relative = PurePosixPath(raw)
    if relative.is_absolute() or not relative.parts or '..' in relative.parts:
        fail('unsafe manifest path')
    if any(':' in part or '\\x00' in part for part in relative.parts):
        fail('unsafe manifest path')
    path = (root / Path(*relative.parts)).resolve(strict=True)
    try:
        path.relative_to(root)
    except ValueError:
        fail('manifest path escapes workspace')
    if not path.is_file():
        fail('manifest path is not a file')
    return path

manifest = json.loads((root / 'input/job-manifest.json').read_text(encoding='utf-8'))
if manifest.get('kind') != 'qianxing-remote-research-input' or manifest.get('schema_version') != 1:
    fail('unsupported job manifest')
if manifest.get('package_version') != expected_version:
    fail('manifest package version mismatch')

strategy_rows = manifest.get('strategy_files')
if not isinstance(strategy_rows, list) or not strategy_rows:
    fail('strategy file manifest is missing')
expected_paths = []
digest = hashlib.sha256()
digest.update(b'qianxing-strategy-transfer-v1\\0')
for row in sorted(strategy_rows, key=lambda item: item['path']):
    relative = row.get('path')
    if not isinstance(relative, str) or not relative.startswith('strategy/'):
        fail('invalid strategy file path')
    path = read_bundle_path(relative)
    content = path.read_bytes()
    expected_paths.append(relative)
    if row.get('bytes') != len(content) or row.get('sha256') != hashlib.sha256(content).hexdigest():
        fail('strategy file fingerprint mismatch')
    name = relative.removeprefix('strategy/').encode('utf-8')
    digest.update(len(name).to_bytes(4, 'big'))
    digest.update(name)
    digest.update(len(content).to_bytes(8, 'big'))
    digest.update(content)
actual_paths = sorted(path.relative_to(root).as_posix() for path in (root / 'strategy').rglob('*') if path.is_file() and path.suffix.lower() == '.py')
if sorted(expected_paths) != actual_paths:
    fail('strategy file set mismatch')
if manifest.get('strategy_transfer_sha256') != digest.hexdigest():
    fail('strategy bundle fingerprint mismatch')

data_files = manifest.get('data_files')
if not isinstance(data_files, list):
    fail('data file manifest is invalid')
for row in data_files:
    path = read_bundle_path(row.get('path', ''))
    content = path.read_bytes()
    if row.get('bytes') != len(content) or row.get('sha256') != hashlib.sha256(content).hexdigest():
        fail('data file fingerprint mismatch')

spec_bytes = (root / 'input/spec.json').read_bytes()
if manifest.get('remote_spec_sha256') != hashlib.sha256(spec_bytes).hexdigest():
    fail('research spec fingerprint mismatch')
entry = Path(manifest.get('strategy_entrypoint', '')).resolve(strict=True)
try:
    entry.relative_to((root / 'strategy').resolve(strict=True))
except ValueError:
    fail('strategy entrypoint escapes strategy bundle')
if entry.relative_to(root).as_posix() not in expected_paths:
    fail('strategy entrypoint is not in strategy manifest')
version = importlib.metadata.version('qianxing')
if version != expected_version:
    fail('template Qianxing version mismatch: expected ' + expected_version + ', got ' + version)
print(version)
""".strip()


def _remote_command(
    *,
    strategy_entrypoint: str,
    strategy_id: str | None,
    strategy_version: str | None,
    package_version: str,
    diagnostic_startup_windows: list[int],
    diagnostic_max_checks: int,
    diagnostic_timeout_seconds: int,
    diagnostic_candidate_limit: int | None,
) -> str:
    command_parts = [
        "qianxing",
        "research",
        strategy_entrypoint,
        f"{_REMOTE_ROOT}/input/spec.json",
        "--html",
        f"{_REMOTE_OUTPUT}/research.html",
        "--diagnostic-startup-windows",
        *(str(value) for value in diagnostic_startup_windows),
        "--diagnostic-max-checks",
        str(diagnostic_max_checks),
        "--diagnostic-timeout-seconds",
        str(diagnostic_timeout_seconds),
    ]
    if diagnostic_candidate_limit is not None:
        command_parts.extend(
            ("--diagnostic-candidate-limit", str(diagnostic_candidate_limit))
        )
    if strategy_id:
        command_parts.extend(("--strategy-id", strategy_id))
    if strategy_version:
        command_parts.extend(("--strategy-version", strategy_version))
    quoted = " ".join(shlex.quote(part) for part in command_parts)
    preflight_script = _input_preflight_script(package_version)
    pack_script = _output_pack_script()
    return "\n".join(
        [
            "set +e",
            f"mkdir -p {shlex.quote(_REMOTE_ROOT + '/input')} {shlex.quote(_REMOTE_OUTPUT)}",
            f"python -m zipfile -e {shlex.quote(_REMOTE_ROOT + '/qianxing-input.zip')} {shlex.quote(_REMOTE_ROOT)}",
            f"python -c {shlex.quote(preflight_script)} > {shlex.quote(_REMOTE_OUTPUT + '/package-version.txt')} 2> {shlex.quote(_REMOTE_OUTPUT + '/package-version-error.txt')}",
            f"if [ \"$?\" -ne 0 ]; then printf '%s\\n' 77 > {shlex.quote(_REMOTE_OUTPUT + '/exit-code')}; printf '%s\\n' 'remote input preflight failed' > {shlex.quote(_REMOTE_OUTPUT + '/research.stderr.log')}; cat {shlex.quote(_REMOTE_OUTPUT + '/package-version-error.txt')} >> {shlex.quote(_REMOTE_OUTPUT + '/research.stderr.log')}; else {quoted} > {shlex.quote(_REMOTE_OUTPUT + '/research.json')} 2> {shlex.quote(_REMOTE_OUTPUT + '/research.stderr.log')}; printf '%s\\n' \"$?\" > {shlex.quote(_REMOTE_OUTPUT + '/exit-code')}; fi",
            f"python -c {shlex.quote(pack_script)}",
        ]
    )


def _extract_output_archive(
    archive_bytes: bytes,
    target: Path,
    *,
    max_archive_bytes: int = MAX_OUTPUT_ARCHIVE_BYTES,
    max_expanded_bytes: int = MAX_OUTPUT_EXPANDED_BYTES,
) -> None:
    if len(archive_bytes) > max_archive_bytes:
        raise SandboxExecutionError(
            f"remote output archive exceeds {max_archive_bytes} bytes"
        )
    target = target.expanduser().resolve(strict=False)
    if target.exists():
        raise SandboxExecutionError(f"remote output directory already exists: {target}")
    target.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".qianxing-e2b-", dir=target.parent))
    total_size = 0
    count = 0
    try:
        with zipfile.ZipFile(io.BytesIO(archive_bytes)) as archive:
            for member in archive.infolist():
                count += 1
                if count > MAX_OUTPUT_FILES:
                    raise SandboxExecutionError("remote output contains too many files")
                normalized = member.filename.replace("\\", "/")
                path = PurePosixPath(normalized)
                if (
                    path.is_absolute()
                    or ".." in path.parts
                    or not path.parts
                    or any(":" in part or "\x00" in part for part in path.parts)
                ):
                    raise SandboxExecutionError(
                        "remote output archive contains an unsafe path"
                    )
                mode = member.external_attr >> 16
                if stat.S_ISLNK(mode):
                    raise SandboxExecutionError(
                        "remote output archive contains a symbolic link"
                    )
                total_size += member.file_size
                if total_size > max_expanded_bytes:
                    raise SandboxExecutionError(
                        f"remote output expands beyond {max_expanded_bytes} bytes"
                    )
                destination = (staging / Path(*path.parts)).resolve(strict=False)
                try:
                    destination.relative_to(staging.resolve(strict=True))
                except ValueError as error:
                    raise SandboxExecutionError(
                        "remote output archive escapes its directory"
                    ) from error
                if member.is_dir():
                    destination.mkdir(parents=True, exist_ok=True)
                    continue
                destination.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(member) as source, destination.open("wb") as output:
                    shutil.copyfileobj(source, output, length=1024 * 1024)
        os.replace(staging, target)
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def _rewrite_remote_output_paths(value: Any, output_dir: Path) -> Any:
    if isinstance(value, dict):
        return {
            key: _rewrite_remote_output_paths(child, output_dir)
            for key, child in value.items()
        }
    if isinstance(value, list):
        return [_rewrite_remote_output_paths(child, output_dir) for child in value]
    if isinstance(value, str) and (
        value == _REMOTE_OUTPUT or value.startswith(_REMOTE_OUTPUT + "/")
    ):
        suffix = value[len(_REMOTE_OUTPUT) :].lstrip("/")
        if not suffix:
            return str(output_dir)
        relative = PurePosixPath(suffix)
        if (
            relative.is_absolute()
            or ".." in relative.parts
            or "\\" in suffix
            or any("\x00" in part or ":" in part for part in relative.parts)
        ):
            raise SandboxExecutionError(
                "remote research result contains an unsafe artifact path"
            )
        resolved = (output_dir / Path(*relative.parts)).resolve(strict=False)
        try:
            resolved.relative_to(output_dir.resolve(strict=False))
        except ValueError as error:
            raise SandboxExecutionError(
                "remote research artifact path escapes its output directory"
            ) from error
        return str(resolved)
    return value


def _finalize_remote_research(
    *,
    provider_id: str,
    job_id: str,
    sandbox_id: str | None,
    provider_details: dict[str, Any],
    package_version: str,
    timeout_seconds: int,
    manifest: dict[str, Any],
    output_archive: bytes,
    output_path: Path,
    audit_path: Path,
) -> dict[str, Any]:
    _extract_output_archive(output_archive, output_path)
    exit_file = output_path / "exit-code"
    try:
        exit_code = int(exit_file.read_text(encoding="utf-8").strip())
    except (OSError, ValueError) as error:
        raise SandboxExecutionError(
            f"remote job did not return a valid exit code; inspect {output_path}"
        ) from error
    stderr_path = output_path / "research.stderr.log"
    stderr_excerpt = ""
    if stderr_path.is_file():
        stderr_excerpt = stderr_path.read_text(encoding="utf-8", errors="replace")[
            -8_000:
        ]
    result_path = output_path / "research.json"
    research_result: dict[str, Any] | None = None
    if result_path.is_file():
        try:
            decoded = json.loads(result_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            decoded = None
        if isinstance(decoded, dict):
            research_result = _rewrite_remote_output_paths(decoded, output_path)
            result_path.write_text(
                json.dumps(
                    research_result, ensure_ascii=False, indent=2, sort_keys=True
                )
                + "\n",
                encoding="utf-8",
            )
    if exit_code != 0:
        raise SandboxExecutionError(
            f"remote Qianxing research exited with code {exit_code}; artifacts were kept at "
            f"{output_path}. {stderr_excerpt.strip()}"
        )
    if research_result is None:
        raise SandboxExecutionError(
            f"remote Qianxing research returned no JSON result; artifacts were kept at {output_path}"
        )
    return {
        "schema_version": 1,
        "kind": "qianxing-remote-research-execution",
        "provider": provider_id,
        "job_id": job_id,
        "sandbox_id": sandbox_id,
        **provider_details,
        "qianxing_version": package_version,
        "timeout_seconds": timeout_seconds,
        "strategy_transfer_sha256": manifest["strategy_transfer_sha256"],
        "input_spec_sha256": manifest["input_spec_sha256"],
        "remote_spec_sha256": manifest["remote_spec_sha256"],
        "data_files": manifest["data_files"],
        "output_dir": str(output_path),
        "audit_log": str(audit_path),
        "report_html": str(output_path / "research.html"),
        "research_result": research_result,
    }


def _research_audit_directory(value: str | Path | None) -> Path:
    configured = (
        value if value is not None else os.environ.get("QX_REMOTE_RESEARCH_AUDIT_DIR")
    )
    if configured is not None and str(configured).strip():
        return Path(configured).expanduser().resolve(strict=False)
    return (Path.home() / ".qianxing" / "strategy-sandbox-audit").resolve(strict=False)


def _create_research_audit(
    *,
    provider: str,
    job_id: str,
    package_version: str,
    manifest: dict[str, Any],
    runtime_ref: str,
    backend: str,
    timeout_seconds: int,
    cpu_count: int,
    memory_bytes: int,
    audit_dir: Path,
) -> tuple[SandboxExecutionAudit, str, Path, str]:
    entrypoint = manifest.get("strategy_entrypoint")
    if not isinstance(entrypoint, str) or not entrypoint.startswith(
        f"{_REMOTE_ROOT}/strategy/"
    ):
        raise SandboxExecutionError("remote strategy manifest has no valid entrypoint")
    entry_archive_path = entrypoint.removeprefix(f"{_REMOTE_ROOT}/")
    strategy_files = manifest.get("strategy_files")
    if not isinstance(strategy_files, list):
        raise SandboxExecutionError("remote strategy manifest has no file digests")
    entry_sha256 = next(
        (
            row.get("sha256")
            for row in strategy_files
            if isinstance(row, dict) and row.get("path") == entry_archive_path
        ),
        None,
    )
    bundle_sha256 = manifest.get("strategy_transfer_sha256")
    spec_sha256 = manifest.get("input_spec_sha256")
    data_files = manifest.get("data_files")
    if not isinstance(data_files, list):
        raise SandboxExecutionError("remote strategy manifest has invalid data files")
    data_fingerprint = _sha256(
        json.dumps(
            data_files, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        ).encode("utf-8")
    )
    path = audit_dir / f"remote-research-{job_id}.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if path.exists():
        raise FileExistsError(f"remote research audit path already exists: {path}")
    audit = SandboxExecutionAudit(
        provider=provider,
        job_id=job_id,
        host_worker_id=f"remote-research:{os.getpid()}",
        package_version=package_version,
        entry_sha256=entry_sha256,
        bundle_sha256=bundle_sha256,
        spec_sha256=spec_sha256,
        resource_limits={
            "cpu_count": cpu_count,
            "memory_bytes": memory_bytes,
            "request_timeout_ms": timeout_seconds * 1000,
            "session_ttl_seconds": timeout_seconds,
            "max_request_bytes": MAX_INPUT_BYTES,
            "max_response_bytes": MAX_OUTPUT_ARCHIVE_BYTES,
        },
        network_policy="deny_all",
        runtime_ref=runtime_ref,
        backend=backend,
        path=path,
    )
    session_id = str(uuid.uuid4())
    audit.emit(session_id=session_id, state="create_requested")
    return audit, session_id, path, data_fingerprint


class E2BStrategySandboxProvider:
    """Run one Qianxing research job in an isolated E2B sandbox."""

    provider_id = "e2b"

    def __init__(
        self,
        *,
        template_id: str,
        timeout_seconds: int = DEFAULT_TIMEOUT_SECONDS,
        expected_cpu_count: int | None = None,
        expected_memory_mb: int | None = None,
        audit_dir: str | Path | None = None,
    ) -> None:
        if not isinstance(template_id, str) or not template_id.strip():
            raise ValueError("an E2B template ID is required")
        if (
            isinstance(timeout_seconds, bool)
            or not isinstance(timeout_seconds, int)
            or not 1 <= timeout_seconds <= 86_400
        ):
            raise ValueError("timeout_seconds must be an integer from 1 through 86400")
        if expected_cpu_count is None:
            raw_cpu = os.environ.get("QX_E2B_EXPECTED_CPU_COUNT", "").strip()
            try:
                expected_cpu_count = int(raw_cpu)
            except ValueError as error:
                raise ValueError(
                    "E2B remote research requires QX_E2B_EXPECTED_CPU_COUNT"
                ) from error
        if expected_memory_mb is None:
            raw_memory = os.environ.get("QX_E2B_EXPECTED_MEMORY_MB", "").strip()
            try:
                expected_memory_mb = int(raw_memory)
            except ValueError as error:
                raise ValueError(
                    "E2B remote research requires QX_E2B_EXPECTED_MEMORY_MB"
                ) from error
        if (
            isinstance(expected_cpu_count, bool)
            or not isinstance(expected_cpu_count, int)
            or not 1 <= expected_cpu_count <= 64
        ):
            raise ValueError("expected_cpu_count must be an integer from 1 through 64")
        if (
            isinstance(expected_memory_mb, bool)
            or not isinstance(expected_memory_mb, int)
            or not 512 <= expected_memory_mb <= 65_536
        ):
            raise ValueError(
                "expected_memory_mb must be an integer from 512 through 65536"
            )
        self.template_id = template_id.strip()
        self.timeout_seconds = timeout_seconds
        self.expected_cpu_count = expected_cpu_count
        self.expected_memory_mb = expected_memory_mb
        self.audit_dir = _research_audit_directory(audit_dir)

    def run_research(
        self,
        *,
        strategy_file: str | Path,
        input_path: str | Path,
        input_spec: dict[str, Any] | None = None,
        output_dir: str | Path | None = None,
        strategy_id: str | None = None,
        strategy_version: str | None = None,
        diagnostic_startup_windows: list[int] | None = None,
        diagnostic_max_checks: int = 8,
        diagnostic_timeout_seconds: int = 10,
        diagnostic_candidate_limit: int | None = None,
    ) -> dict[str, Any]:
        strategy_path = Path(strategy_file).expanduser().resolve(strict=True)
        if not strategy_path.is_file() or strategy_path.suffix.lower() != ".py":
            raise ValueError(
                f"strategy_file must be an existing .py file: {strategy_path}"
            )
        spec_path = Path(input_path).expanduser().resolve(strict=True)
        if not spec_path.is_file():
            raise ValueError(f"research input must be a regular JSON file: {spec_path}")
        spec = _read_json_document(spec_path) if input_spec is None else input_spec
        if not isinstance(spec, dict):
            raise TypeError("remote research input must be a JSON object")
        startup_windows = (
            [20, 50, 100]
            if diagnostic_startup_windows is None
            else diagnostic_startup_windows
        )
        if not startup_windows or any(
            isinstance(value, bool) or not isinstance(value, int) or value < 1
            for value in startup_windows
        ):
            raise ValueError(
                "diagnostic_startup_windows must contain positive integers"
            )
        if (
            isinstance(diagnostic_max_checks, bool)
            or not isinstance(diagnostic_max_checks, int)
            or not 1 <= diagnostic_max_checks <= 1000
        ):
            raise ValueError("diagnostic_max_checks must be from 1 through 1000")
        if (
            isinstance(diagnostic_timeout_seconds, bool)
            or not isinstance(diagnostic_timeout_seconds, int)
            or not 1 <= diagnostic_timeout_seconds <= 3600
        ):
            raise ValueError("diagnostic_timeout_seconds must be from 1 through 3600")
        if diagnostic_candidate_limit is not None and (
            isinstance(diagnostic_candidate_limit, bool)
            or not isinstance(diagnostic_candidate_limit, int)
            or diagnostic_candidate_limit < 0
        ):
            raise ValueError(
                "diagnostic_candidate_limit must be a non-negative integer"
            )
        try:
            package_version = importlib.metadata.version("qianxing")
        except importlib.metadata.PackageNotFoundError as error:
            raise SandboxExecutionError(
                "Qianxing package metadata is unavailable; install the Python wheel before remote execution"
            ) from error
        job_id = str(uuid.uuid4())
        archive_bytes, manifest = _bundle_remote_inputs(
            strategy_path,
            spec,
            package_version=package_version,
            job_id=job_id,
        )
        output_path = (
            Path(output_dir).expanduser().resolve(strict=False)
            if output_dir is not None
            else (Path.cwd() / f"{spec_path.stem}-e2b-research").resolve(strict=False)
        )
        if output_path.exists():
            raise ValueError(f"remote output directory already exists: {output_path}")

        try:
            from e2b import Sandbox
        except ImportError as error:
            raise SandboxExecutionError(
                "E2B support is optional; install with `pip install 'qianxing[sandbox-e2b]'`"
            ) from error

        audit, audit_session_id, audit_path, data_fingerprint = _create_research_audit(
            provider=self.provider_id,
            job_id=job_id,
            package_version=package_version,
            manifest=manifest,
            runtime_ref=self.template_id,
            backend="cloud",
            timeout_seconds=self.timeout_seconds,
            cpu_count=self.expected_cpu_count,
            memory_bytes=self.expected_memory_mb * 1_000_000,
            audit_dir=self.audit_dir,
        )

        sandbox = None
        sandbox_id = None
        output_archive: bytes | None = None
        operation_error: SandboxExecutionError | None = None
        try:
            sandbox = Sandbox.create(
                self.template_id,
                timeout=self.timeout_seconds,
                allow_internet_access=False,
                metadata={
                    "qianxing_job_id": job_id,
                    "qianxing_spec_sha256": manifest["input_spec_sha256"],
                    "qianxing_strategy_sha256": manifest["strategy_transfer_sha256"],
                    "qianxing_expected_cpu_count": str(self.expected_cpu_count),
                    "qianxing_expected_memory_mb": str(self.expected_memory_mb),
                },
            )
            sandbox_id = getattr(sandbox, "sandbox_id", None)
            audit.emit(session_id=audit_session_id, state="created")
            get_info = getattr(sandbox, "get_info", None)
            if not callable(get_info):
                raise SandboxExecutionError(
                    "E2B SDK does not expose sandbox CPU and memory inspection"
                )
            info = get_info()
            actual_cpu_count = getattr(info, "cpu_count", None)
            actual_memory_mb = getattr(info, "memory_mb", None)
            if (
                type(actual_cpu_count) is not int
                or type(actual_memory_mb) is not int
                or actual_cpu_count <= 0
                or actual_memory_mb <= 0
            ):
                raise SandboxExecutionError(
                    "E2B sandbox did not report verifiable CPU and memory limits"
                )
            if (
                actual_cpu_count != self.expected_cpu_count
                or actual_memory_mb != self.expected_memory_mb
            ):
                raise SandboxExecutionError(
                    "E2B sandbox resource limits do not match the host policy: "
                    f"expected cpu={self.expected_cpu_count}, memory_mb="
                    f"{self.expected_memory_mb}; got cpu={actual_cpu_count}, "
                    f"memory_mb={actual_memory_mb}"
                )
            sandbox.files.write(f"{_REMOTE_ROOT}/qianxing-input.zip", archive_bytes)
            command = _remote_command(
                strategy_entrypoint=manifest["strategy_entrypoint"],
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                package_version=package_version,
                diagnostic_startup_windows=startup_windows,
                diagnostic_max_checks=diagnostic_max_checks,
                diagnostic_timeout_seconds=diagnostic_timeout_seconds,
                diagnostic_candidate_limit=diagnostic_candidate_limit,
            )
            audit.emit(session_id=audit_session_id, state="ready")
            try:
                sandbox.commands.run(command, timeout=self.timeout_seconds)
            except Exception as error:
                # A timeout or transport loss must not fall back to local execution.
                raise SandboxExecutionError(
                    f"E2B research command failed: {error}"
                ) from error
            output_archive = sandbox.files.read(
                f"{_REMOTE_ROOT}/qianxing-output.zip", format="bytes"
            )
            if not isinstance(output_archive, bytes):
                raise SandboxExecutionError("E2B returned a non-binary output archive")
            audit.emit(
                session_id=audit_session_id,
                state="request_completed",
                request_sequence=1,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                data_fingerprint=data_fingerprint,
                response_sha256=_sha256(output_archive),
            )
        except SandboxExecutionError as error:
            operation_error = SandboxExecutionError(
                f"{error}; host audit log: {audit_path}"
            )
            audit.emit_best_effort(
                session_id=audit_session_id,
                state="create_failed" if sandbox is None else "request_failed",
                failure_code=sandbox_failure_code(error),
                request_sequence=1 if sandbox is not None else None,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                data_fingerprint=data_fingerprint,
            )
            raise operation_error from error
        except Exception as error:
            operation_error = SandboxExecutionError(
                f"E2B sandbox operation failed: {error}; host audit log: {audit_path}"
            )
            audit.emit_best_effort(
                session_id=audit_session_id,
                state="create_failed" if sandbox is None else "request_failed",
                failure_code=sandbox_failure_code(error),
                request_sequence=1 if sandbox is not None else None,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                data_fingerprint=data_fingerprint,
            )
            raise operation_error from error
        finally:
            if sandbox is not None:
                audit.emit_best_effort(
                    session_id=audit_session_id, state="destroy_requested"
                )
                try:
                    sandbox.kill()
                except Exception as error:
                    audit.emit_best_effort(
                        session_id=audit_session_id,
                        state="destroy_failed",
                        failure_code=sandbox_failure_code(error),
                    )
                    cleanup_error = (
                        f"E2B sandbox {sandbox_id or '<unknown>'} cleanup failed: "
                        f"{error}; host audit log: {audit_path}"
                    )
                    if operation_error is not None:
                        raise SandboxExecutionError(
                            f"{operation_error}; additionally, {cleanup_error}"
                        ) from operation_error
                    raise SandboxExecutionError(cleanup_error) from error
                else:
                    audit.emit_best_effort(
                        session_id=audit_session_id, state="destroyed"
                    )

        assert output_archive is not None
        try:
            return _finalize_remote_research(
                provider_id=self.provider_id,
                job_id=job_id,
                sandbox_id=sandbox_id,
                provider_details={"template_id": self.template_id},
                package_version=package_version,
                timeout_seconds=self.timeout_seconds,
                manifest=manifest,
                output_archive=output_archive,
                output_path=output_path,
                audit_path=audit_path,
            )
        except Exception as error:
            raise SandboxExecutionError(
                f"E2B result finalization failed: {error}; host audit log: {audit_path}"
            ) from error


class OpenSandboxStrategySandboxProvider:
    """Run one research job in a host-managed OpenSandbox service.

    Provider selection is explicit. This class never silently falls back to E2B
    or local execution, and the sandbox is created with deny-all egress.
    """

    provider_id = "opensandbox"

    def __init__(
        self,
        *,
        domain: str,
        image: str,
        api_key: str | None = None,
        timeout_seconds: int = DEFAULT_TIMEOUT_SECONDS,
        use_server_proxy: bool = True,
        cpu_count: int = 1,
        memory_mib: int = 2048,
        audit_dir: str | Path | None = None,
    ) -> None:
        if not isinstance(domain, str) or not domain.strip():
            raise ValueError("an OpenSandbox domain is required")
        if not isinstance(image, str) or not image.strip():
            raise ValueError("an OpenSandbox image is required")
        if api_key is not None and not isinstance(api_key, str):
            raise TypeError("OpenSandbox API key must be a string or None")
        if (
            isinstance(timeout_seconds, bool)
            or not isinstance(timeout_seconds, int)
            or not 1 <= timeout_seconds <= 86_400
        ):
            raise ValueError("timeout_seconds must be an integer from 1 through 86400")
        if not isinstance(use_server_proxy, bool):
            raise TypeError("use_server_proxy must be a boolean")
        if (
            isinstance(cpu_count, bool)
            or not isinstance(cpu_count, int)
            or not 1 <= cpu_count <= 64
        ):
            raise ValueError("cpu_count must be an integer from 1 through 64")
        if (
            isinstance(memory_mib, bool)
            or not isinstance(memory_mib, int)
            or not 512 <= memory_mib <= 65_536
        ):
            raise ValueError("memory_mib must be an integer from 512 through 65536")
        self.domain = domain.strip()
        self.image = _require_digest_pinned_image(image, "QX_OPENSANDBOX_IMAGE")
        self.api_key = api_key.strip() if api_key and api_key.strip() else None
        self.timeout_seconds = timeout_seconds
        self.use_server_proxy = use_server_proxy
        self.cpu_count = cpu_count
        self.memory_mib = memory_mib
        self.audit_dir = _research_audit_directory(audit_dir)

    def run_research(
        self,
        *,
        strategy_file: str | Path,
        input_path: str | Path,
        input_spec: dict[str, Any] | None = None,
        output_dir: str | Path | None = None,
        strategy_id: str | None = None,
        strategy_version: str | None = None,
        diagnostic_startup_windows: list[int] | None = None,
        diagnostic_max_checks: int = 8,
        diagnostic_timeout_seconds: int = 10,
        diagnostic_candidate_limit: int | None = None,
    ) -> dict[str, Any]:
        strategy_path = Path(strategy_file).expanduser().resolve(strict=True)
        if not strategy_path.is_file() or strategy_path.suffix.lower() != ".py":
            raise ValueError(
                f"strategy_file must be an existing .py file: {strategy_path}"
            )
        spec_path = Path(input_path).expanduser().resolve(strict=True)
        if not spec_path.is_file():
            raise ValueError(f"research input must be a regular JSON file: {spec_path}")
        spec = _read_json_document(spec_path) if input_spec is None else input_spec
        if not isinstance(spec, dict):
            raise TypeError("remote research input must be a JSON object")
        startup_windows = (
            [20, 50, 100]
            if diagnostic_startup_windows is None
            else diagnostic_startup_windows
        )
        if not startup_windows or any(
            isinstance(value, bool) or not isinstance(value, int) or value < 1
            for value in startup_windows
        ):
            raise ValueError(
                "diagnostic_startup_windows must contain positive integers"
            )
        if (
            isinstance(diagnostic_max_checks, bool)
            or not isinstance(diagnostic_max_checks, int)
            or not 1 <= diagnostic_max_checks <= 1000
        ):
            raise ValueError("diagnostic_max_checks must be from 1 through 1000")
        if (
            isinstance(diagnostic_timeout_seconds, bool)
            or not isinstance(diagnostic_timeout_seconds, int)
            or not 1 <= diagnostic_timeout_seconds <= 3600
        ):
            raise ValueError("diagnostic_timeout_seconds must be from 1 through 3600")
        if diagnostic_candidate_limit is not None and (
            isinstance(diagnostic_candidate_limit, bool)
            or not isinstance(diagnostic_candidate_limit, int)
            or diagnostic_candidate_limit < 0
        ):
            raise ValueError(
                "diagnostic_candidate_limit must be a non-negative integer"
            )
        try:
            package_version = importlib.metadata.version("qianxing")
        except importlib.metadata.PackageNotFoundError as error:
            raise SandboxExecutionError(
                "Qianxing package metadata is unavailable; install the Python wheel before remote execution"
            ) from error

        job_id = str(uuid.uuid4())
        archive_bytes, manifest = _bundle_remote_inputs(
            strategy_path,
            spec,
            package_version=package_version,
            job_id=job_id,
        )
        output_path = (
            Path(output_dir).expanduser().resolve(strict=False)
            if output_dir is not None
            else (Path.cwd() / f"{spec_path.stem}-opensandbox-research").resolve(
                strict=False
            )
        )
        if output_path.exists():
            raise ValueError(f"remote output directory already exists: {output_path}")

        try:
            from opensandbox import SandboxSync
            from opensandbox.config import ConnectionConfigSync
            from opensandbox.models.execd import RunCommandOpts
            from opensandbox.models.filesystem import WriteEntry
            from opensandbox.models.sandboxes import NetworkPolicy
        except ImportError as error:
            raise SandboxExecutionError(
                "OpenSandbox support is optional; install with `pip install 'qianxing[sandbox-opensandbox]'`"
            ) from error

        config = ConnectionConfigSync(
            domain=self.domain,
            api_key=self.api_key,
            request_timeout=timedelta(seconds=self.timeout_seconds + 30),
            use_server_proxy=self.use_server_proxy,
        )
        audit, audit_session_id, audit_path, data_fingerprint = _create_research_audit(
            provider=self.provider_id,
            job_id=job_id,
            package_version=package_version,
            manifest=manifest,
            runtime_ref=self.image,
            backend="self_hosted",
            timeout_seconds=self.timeout_seconds,
            cpu_count=self.cpu_count,
            memory_bytes=self.memory_mib * 1024 * 1024,
            audit_dir=self.audit_dir,
        )
        sandbox = None
        sandbox_id: str | None = None
        output_archive: bytes | None = None
        operation_error: SandboxExecutionError | None = None
        try:
            sandbox = SandboxSync.create(
                self.image,
                connection_config=config,
                timeout=timedelta(seconds=self.timeout_seconds),
                resource={
                    "cpu": str(self.cpu_count),
                    "memory": f"{self.memory_mib}Mi",
                },
                network_policy=NetworkPolicy(defaultAction="deny", egress=[]),
                metadata={
                    "qianxing_job_id": job_id,
                    "qianxing_spec_sha256": manifest["input_spec_sha256"],
                    "qianxing_strategy_sha256": manifest["strategy_transfer_sha256"],
                    "qianxing_resource_cpu": str(self.cpu_count),
                    "qianxing_resource_memory_mib": str(self.memory_mib),
                },
            )
            sandbox_id = getattr(sandbox, "id", None)
            audit.emit(session_id=audit_session_id, state="created")
            sandbox.commands.run(
                "mkdir -p /workspace",
                opts=RunCommandOpts(timeout=timedelta(seconds=30)),
            )
            sandbox.files.write_files(
                [
                    WriteEntry(
                        path=f"{_REMOTE_ROOT}/qianxing-input.zip",
                        data=archive_bytes,
                        mode=0o600,
                    )
                ]
            )
            command = _remote_command(
                strategy_entrypoint=manifest["strategy_entrypoint"],
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                package_version=package_version,
                diagnostic_startup_windows=startup_windows,
                diagnostic_max_checks=diagnostic_max_checks,
                diagnostic_timeout_seconds=diagnostic_timeout_seconds,
                diagnostic_candidate_limit=diagnostic_candidate_limit,
            )
            audit.emit(session_id=audit_session_id, state="ready")
            sandbox.commands.run(
                command,
                opts=RunCommandOpts(timeout=timedelta(seconds=self.timeout_seconds)),
            )
            output_archive = sandbox.files.read_bytes(
                f"{_REMOTE_ROOT}/qianxing-output.zip"
            )
            if not isinstance(output_archive, bytes):
                raise SandboxExecutionError(
                    "OpenSandbox returned a non-binary output archive"
                )
            audit.emit(
                session_id=audit_session_id,
                state="request_completed",
                request_sequence=1,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                data_fingerprint=data_fingerprint,
                response_sha256=_sha256(output_archive),
            )
        except SandboxExecutionError as error:
            operation_error = SandboxExecutionError(
                f"{error}; host audit log: {audit_path}"
            )
            audit.emit_best_effort(
                session_id=audit_session_id,
                state="create_failed" if sandbox is None else "request_failed",
                failure_code=sandbox_failure_code(error),
                request_sequence=1 if sandbox is not None else None,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                data_fingerprint=data_fingerprint,
            )
            raise operation_error from error
        except Exception as error:
            operation_error = SandboxExecutionError(
                f"OpenSandbox research job failed: {error}; host audit log: {audit_path}"
            )
            audit.emit_best_effort(
                session_id=audit_session_id,
                state="create_failed" if sandbox is None else "request_failed",
                failure_code=sandbox_failure_code(error),
                request_sequence=1 if sandbox is not None else None,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                data_fingerprint=data_fingerprint,
            )
            raise operation_error from error
        finally:
            if sandbox is not None:
                audit.emit_best_effort(
                    session_id=audit_session_id, state="destroy_requested"
                )
                try:
                    sandbox.destroy()
                except Exception as error:
                    audit.emit_best_effort(
                        session_id=audit_session_id,
                        state="destroy_failed",
                        failure_code=sandbox_failure_code(error),
                    )
                    cleanup_error = (
                        f"OpenSandbox sandbox {sandbox_id or '<unknown>'} cleanup failed: "
                        f"{error}; host audit log: {audit_path}"
                    )
                    if operation_error is not None:
                        raise SandboxExecutionError(
                            f"{operation_error}; additionally, {cleanup_error}"
                        ) from operation_error
                    raise SandboxExecutionError(cleanup_error) from error
                else:
                    audit.emit_best_effort(
                        session_id=audit_session_id, state="destroyed"
                    )

        assert output_archive is not None
        try:
            return _finalize_remote_research(
                provider_id=self.provider_id,
                job_id=job_id,
                sandbox_id=sandbox_id,
                provider_details={
                    "domain": self.domain,
                    "image": self.image,
                    "network_egress": "deny_all",
                    "resource_limits": {
                        "cpu": str(self.cpu_count),
                        "memory": f"{self.memory_mib}Mi",
                    },
                },
                package_version=package_version,
                timeout_seconds=self.timeout_seconds,
                manifest=manifest,
                output_archive=output_archive,
                output_path=output_path,
                audit_path=audit_path,
            )
        except Exception as error:
            raise SandboxExecutionError(
                f"OpenSandbox result finalization failed: {error}; host audit log: {audit_path}"
            ) from error


class MicrosandboxStrategySandboxProvider:
    """Run one remote research job in an isolated Microsandbox microVM."""

    provider_id = "microsandbox"

    def __init__(
        self,
        *,
        image: str,
        backend: str = "local",
        api_key: str | None = None,
        profile: str | None = None,
        url: str | None = None,
        timeout_seconds: int = DEFAULT_TIMEOUT_SECONDS,
        cpu_count: int = 1,
        memory_mib: int = 1024,
        audit_dir: str | Path | None = None,
    ) -> None:
        if backend not in {"local", "cloud"}:
            raise ValueError("Microsandbox backend must be local or cloud")
        if backend == "cloud" and not any(
            isinstance(value, str) and value.strip()
            for value in (api_key, profile)
        ):
            raise ValueError(
                "Microsandbox cloud mode requires MICROSANDBOX_API_KEY or "
                "QX_MICROSANDBOX_PROFILE"
            )
        if api_key is not None and not isinstance(api_key, str):
            raise TypeError("Microsandbox API key must be a string or None")
        if profile is not None and not isinstance(profile, str):
            raise TypeError("Microsandbox profile must be a string or None")
        if url is not None:
            from urllib.parse import urlsplit

            if not isinstance(url, str):
                raise TypeError("Microsandbox URL must be a string or None")
            parsed_url = urlsplit(url)
            if (
                parsed_url.scheme not in {"http", "https"}
                or not parsed_url.hostname
                or parsed_url.username is not None
                or parsed_url.password is not None
                or parsed_url.query
                or parsed_url.fragment
            ):
                raise ValueError(
                    "Microsandbox URL must be an HTTP(S) base URL without "
                    "embedded credentials, query, or fragment"
                )
        if (
            isinstance(timeout_seconds, bool)
            or not isinstance(timeout_seconds, int)
            or not 1 <= timeout_seconds <= 86_400
        ):
            raise ValueError("timeout_seconds must be an integer from 1 through 86400")
        if (
            isinstance(cpu_count, bool)
            or not isinstance(cpu_count, int)
            or not 1 <= cpu_count <= 64
        ):
            raise ValueError("cpu_count must be an integer from 1 through 64")
        if (
            isinstance(memory_mib, bool)
            or not isinstance(memory_mib, int)
            or not 512 <= memory_mib <= 65_536
        ):
            raise ValueError("memory_mib must be an integer from 512 through 65536")
        self.image = _require_digest_pinned_image(image, "QX_MICROSANDBOX_IMAGE")
        self.backend = backend
        self.api_key = api_key.strip() if api_key and api_key.strip() else None
        self.profile = profile.strip() if profile and profile.strip() else None
        self.url = url.strip() if url and url.strip() else None
        self.timeout_seconds = timeout_seconds
        self.cpu_count = cpu_count
        self.memory_mib = memory_mib
        self.audit_dir = _research_audit_directory(audit_dir)

    def _redact_error(self, error: BaseException) -> str:
        message = str(error)
        for secret in (self.api_key,):
            if secret:
                message = message.replace(secret, "<redacted>")
        return message

    def run_research(
        self,
        *,
        strategy_file: str | Path,
        input_path: str | Path,
        input_spec: dict[str, Any] | None = None,
        output_dir: str | Path | None = None,
        strategy_id: str | None = None,
        strategy_version: str | None = None,
        diagnostic_startup_windows: list[int] | None = None,
        diagnostic_max_checks: int = 8,
        diagnostic_timeout_seconds: int = 10,
        diagnostic_candidate_limit: int | None = None,
    ) -> dict[str, Any]:
        try:
            asyncio.get_running_loop()
        except RuntimeError:
            pass
        else:
            raise SandboxExecutionError(
                "Microsandbox remote-research cannot run inside an active event loop; "
                "call it from a synchronous CLI or worker process"
            )
        strategy_path = Path(strategy_file).expanduser().resolve(strict=True)
        if not strategy_path.is_file() or strategy_path.suffix.lower() != ".py":
            raise ValueError(
                f"strategy_file must be an existing .py file: {strategy_path}"
            )
        spec_path = Path(input_path).expanduser().resolve(strict=True)
        if not spec_path.is_file():
            raise ValueError(f"research input must be a regular JSON file: {spec_path}")
        spec = _read_json_document(spec_path) if input_spec is None else input_spec
        if not isinstance(spec, dict):
            raise TypeError("remote research input must be a JSON object")
        startup_windows = (
            [20, 50, 100]
            if diagnostic_startup_windows is None
            else diagnostic_startup_windows
        )
        if not startup_windows or any(
            isinstance(value, bool) or not isinstance(value, int) or value < 1
            for value in startup_windows
        ):
            raise ValueError(
                "diagnostic_startup_windows must contain positive integers"
            )
        if (
            isinstance(diagnostic_max_checks, bool)
            or not isinstance(diagnostic_max_checks, int)
            or not 1 <= diagnostic_max_checks <= 1000
        ):
            raise ValueError("diagnostic_max_checks must be from 1 through 1000")
        if (
            isinstance(diagnostic_timeout_seconds, bool)
            or not isinstance(diagnostic_timeout_seconds, int)
            or not 1 <= diagnostic_timeout_seconds <= 3600
        ):
            raise ValueError("diagnostic_timeout_seconds must be from 1 through 3600")
        if diagnostic_candidate_limit is not None and (
            isinstance(diagnostic_candidate_limit, bool)
            or not isinstance(diagnostic_candidate_limit, int)
            or diagnostic_candidate_limit < 0
        ):
            raise ValueError(
                "diagnostic_candidate_limit must be a non-negative integer"
            )
        try:
            package_version = importlib.metadata.version("qianxing")
        except importlib.metadata.PackageNotFoundError as error:
            raise SandboxExecutionError(
                "Qianxing package metadata is unavailable; install the Python wheel before remote execution"
            ) from error
        job_id = str(uuid.uuid4())
        archive_bytes, manifest = _bundle_remote_inputs(
            strategy_path,
            spec,
            package_version=package_version,
            job_id=job_id,
        )
        output_path = (
            Path(output_dir).expanduser().resolve(strict=False)
            if output_dir is not None
            else (Path.cwd() / f"{spec_path.stem}-microsandbox-research").resolve(
                strict=False
            )
        )
        if output_path.exists():
            raise ValueError(f"remote output directory already exists: {output_path}")
        try:
            from microsandbox import BackendKind, Network, Sandbox, backend_scope
        except ImportError as error:
            raise SandboxExecutionError(
                "Microsandbox support is optional; install with `pip install 'qianxing[sandbox-microsandbox]'`"
            ) from error

        backend_kind = (
            BackendKind.LOCAL if self.backend == "local" else BackendKind.CLOUD
        )
        backend_options: dict[str, str] = {}
        if self.profile:
            backend_options["profile"] = self.profile
        elif self.api_key:
            backend_options["api_key"] = self.api_key
        if self.url:
            backend_options["url"] = self.url
        audit, audit_session_id, audit_path, data_fingerprint = _create_research_audit(
            provider=self.provider_id,
            job_id=job_id,
            package_version=package_version,
            manifest=manifest,
            runtime_ref=self.image,
            backend=self.backend,
            timeout_seconds=self.timeout_seconds,
            cpu_count=self.cpu_count,
            memory_bytes=self.memory_mib * 1024 * 1024,
            audit_dir=self.audit_dir,
        )
        sandbox = None
        sandbox_id: str | None = None
        output_archive: bytes | None = None
        operation_error: SandboxExecutionError | None = None

        async def execute() -> None:
            nonlocal sandbox, sandbox_id, output_archive, operation_error
            try:
                with backend_scope(backend_kind, **backend_options):
                    sandbox = await Sandbox.create(
                        "qx-research-" + job_id.replace("-", "")[:24],
                        image=self.image,
                        cpus=self.cpu_count,
                        memory=self.memory_mib,
                        max_duration=float(self.timeout_seconds),
                        network=Network.none(),
                        labels={
                            "app": "qianxing-remote-research",
                            "job": job_id[:48],
                            "qianxing_spec_sha256": manifest["input_spec_sha256"],
                            "qianxing_bundle_sha256": manifest[
                                "strategy_transfer_sha256"
                            ],
                        },
                    )
                    sandbox_id = getattr(sandbox, "name", None) or getattr(
                        sandbox, "id", None
                    )
                    audit.emit(session_id=audit_session_id, state="created")
                    prepared = await sandbox.exec(
                        "mkdir", ["-p", _REMOTE_ROOT], timeout=30.0
                    )
                    if not getattr(prepared, "success", False):
                        raise SandboxExecutionError(
                            "Microsandbox could not create the research workspace"
                        )
                    await sandbox.fs.write(
                        f"{_REMOTE_ROOT}/qianxing-input.zip", archive_bytes
                    )
                    audit.emit(session_id=audit_session_id, state="ready")
                    result = await sandbox.exec(
                        "env",
                        [
                            "-i",
                            "PATH=/usr/local/bin:/usr/bin:/bin",
                            "sh",
                            "-c",
                            _remote_command(
                                strategy_entrypoint=manifest["strategy_entrypoint"],
                                strategy_id=strategy_id,
                                strategy_version=strategy_version,
                                package_version=package_version,
                                diagnostic_startup_windows=startup_windows,
                                diagnostic_max_checks=diagnostic_max_checks,
                                diagnostic_timeout_seconds=diagnostic_timeout_seconds,
                                diagnostic_candidate_limit=diagnostic_candidate_limit,
                            ),
                        ],
                        timeout=float(self.timeout_seconds),
                    )
                    if not getattr(result, "success", False):
                        detail = str(getattr(result, "stderr_text", ""))[-8_000:]
                        raise SandboxExecutionError(
                            "Microsandbox research command failed"
                            + (f": {detail}" if detail else "")
                        )
                    output_archive = await sandbox.fs.read(
                        f"{_REMOTE_ROOT}/output/qianxing-output.zip"
                    )
                    if not isinstance(output_archive, bytes):
                        raise SandboxExecutionError(
                            "Microsandbox returned a non-binary output archive"
                        )
                    audit.emit(
                        session_id=audit_session_id,
                        state="request_completed",
                        request_sequence=1,
                        strategy_id=strategy_id,
                        strategy_version=strategy_version,
                        data_fingerprint=data_fingerprint,
                        response_sha256=_sha256(output_archive),
                    )
            except SandboxExecutionError as error:
                operation_error = SandboxExecutionError(
                    f"{self._redact_error(error)}; host audit log: {audit_path}"
                )
                audit.emit_best_effort(
                    session_id=audit_session_id,
                    state="create_failed" if sandbox is None else "request_failed",
                    failure_code=sandbox_failure_code(error),
                    request_sequence=1 if sandbox is not None else None,
                    strategy_id=strategy_id,
                    strategy_version=strategy_version,
                    data_fingerprint=data_fingerprint,
                )
                raise operation_error from error
            except Exception as error:
                operation_error = SandboxExecutionError(
                    "Microsandbox research job failed: "
                    f"{self._redact_error(error)}; host audit log: {audit_path}"
                )
                audit.emit_best_effort(
                    session_id=audit_session_id,
                    state="create_failed" if sandbox is None else "request_failed",
                    failure_code=sandbox_failure_code(error),
                    request_sequence=1 if sandbox is not None else None,
                    strategy_id=strategy_id,
                    strategy_version=strategy_version,
                    data_fingerprint=data_fingerprint,
                )
                raise operation_error from error
            finally:
                if sandbox is not None:
                    audit.emit_best_effort(
                        session_id=audit_session_id, state="destroy_requested"
                    )
                    try:
                        await sandbox.destroy(force=True, timeout=10.0)
                    except Exception as error:
                        audit.emit_best_effort(
                            session_id=audit_session_id,
                            state="destroy_failed",
                            failure_code=sandbox_failure_code(error),
                        )
                        cleanup_error = (
                            f"Microsandbox {sandbox_id or '<unknown>'} cleanup failed: "
                            f"{self._redact_error(error)}; host audit log: {audit_path}"
                        )
                        if operation_error is not None:
                            raise SandboxExecutionError(
                                f"{operation_error}; additionally, {cleanup_error}"
                            ) from operation_error
                        raise SandboxExecutionError(cleanup_error) from error
                    else:
                        audit.emit_best_effort(
                            session_id=audit_session_id, state="destroyed"
                        )

        # Microsandbox backend_scope changes the SDK's process-wide default.
        # Serialize this provider's calls so concurrent jobs cannot create a
        # sandbox through another job's local/cloud backend selection.
        with _MICROSANDBOX_RESEARCH_BACKEND_LOCK:
            asyncio.run(execute())
        assert output_archive is not None
        try:
            return _finalize_remote_research(
                provider_id=self.provider_id,
                job_id=job_id,
                sandbox_id=sandbox_id,
                provider_details={
                    "image": self.image,
                    "backend": self.backend,
                    "resource_limits": {
                        "cpu": self.cpu_count,
                        "memory_mib": self.memory_mib,
                    },
                    "network_egress": "deny_all",
                },
                package_version=package_version,
                timeout_seconds=self.timeout_seconds,
                manifest=manifest,
                output_archive=output_archive,
                output_path=output_path,
                audit_path=audit_path,
            )
        except Exception as error:
            raise SandboxExecutionError(
                f"Microsandbox result finalization failed: {error}; host audit log: {audit_path}"
            ) from error
