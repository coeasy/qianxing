"""Validate the strategy-run refactor work-package ledger and path evidence."""
from __future__ import annotations

import json
import hashlib
from datetime import datetime
from pathlib import Path
from pathlib import PurePosixPath
import sys
import uuid


ROOT = Path(__file__).resolve().parents[1]
LEDGER = ROOT / "maturity" / "strategy-run-refactor-work-packages.json"
ACCEPTANCE = ROOT / "maturity" / "strategy-run-acceptance-v1.json"
EXPECTED_IDS = {f"R{number:02}" for number in range(1, 24)}
VALID_STATUSES = {"partial", "not_started", "blocked", "complete"}


def validate() -> list[str]:
    errors: list[str] = []
    try:
        value = json.loads(LEDGER.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return [f"cannot read work-package ledger: {error}"]
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        return ["ledger must be an object with schema_version=1"]
    packages = value.get("work_packages")
    if not isinstance(packages, list):
        return ["ledger work_packages must be an array"]

    seen: set[str] = set()
    for package in packages:
        if not isinstance(package, dict):
            errors.append("work package entry must be an object")
            continue
        package_id = package.get("id")
        label = package_id if isinstance(package_id, str) else "<missing id>"
        if not isinstance(package_id, str) or package_id in seen:
            errors.append(f"{label}: missing or duplicate ID")
        else:
            seen.add(package_id)
        if package.get("status") not in VALID_STATUSES:
            errors.append(f"{label}: invalid status {package.get('status')!r}")
        for field in ("code_paths", "evidence_paths"):
            paths = package.get(field)
            if field == "evidence_paths" and paths == [] and package.get("status") == "not_started":
                continue
            if not isinstance(paths, list) or not paths:
                errors.append(f"{label}: {field} must contain at least one path")
                continue
            for raw_path in paths:
                if not isinstance(raw_path, str) or not raw_path:
                    errors.append(f"{label}: {field} contains an invalid path")
                    continue
                candidate = (ROOT / raw_path).resolve()
                try:
                    candidate.relative_to(ROOT)
                except ValueError:
                    errors.append(f"{label}: path escapes repository: {raw_path}")
                    continue
                if not candidate.exists():
                    errors.append(f"{label}: path does not exist: {raw_path}")
        for field in ("blockers", "next_action"):
            content = package.get(field)
            if isinstance(content, str):
                valid = bool(content.strip())
            elif isinstance(content, list):
                valid = bool(content) and all(isinstance(item, str) and item.strip() for item in content)
            else:
                valid = False
            if not valid:
                errors.append(f"{label}: {field} must explain current gaps and next work")
    if seen != EXPECTED_IDS:
        missing, extra = sorted(EXPECTED_IDS - seen), sorted(seen - EXPECTED_IDS)
        errors.append(f"work package IDs mismatch; missing={missing}, extra={extra}")

    try:
        acceptance = json.loads(ACCEPTANCE.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return errors + [f"cannot read acceptance records: {error}"]
    if not isinstance(acceptance, dict) or acceptance.get("schema_version") != 1 or acceptance.get("schema") != "schemas/strategy-run-acceptance-v1.json":
        errors.append("acceptance records must be an object with schema_version=1")
        return errors
    scenarios = acceptance.get("scenarios")
    runs = acceptance.get("runs")
    if not isinstance(scenarios, list) or not isinstance(runs, list):
        errors.append("acceptance scenarios and runs must be arrays")
        return errors
    scenario_ids: set[str] = set()
    package_scenarios: dict[str, int] = {}
    for scenario in scenarios:
        if not isinstance(scenario, dict):
            errors.append("acceptance scenario must be an object")
            continue
        scenario_id = scenario.get("acceptance_id")
        package_id = scenario.get("work_package")
        if not isinstance(scenario_id, str) or not scenario_id.strip() or scenario_id in scenario_ids:
            errors.append(f"invalid or duplicate acceptance ID: {scenario_id!r}")
            continue
        scenario_ids.add(scenario_id)
        if package_id not in EXPECTED_IDS:
            errors.append(f"{scenario_id}: unknown work package {package_id!r}")
        else:
            package_scenarios[package_id] = package_scenarios.get(package_id, 0) + 1
        if scenario.get("status") not in {"planned", "accepted", "blocked"}:
            errors.append(f"{scenario_id}: invalid scenario status")
        for field in ("acceptance_criteria", "required_artifacts"):
            value = scenario.get(field)
            if not isinstance(value, list) or not value or not all(isinstance(item, str) and item.strip() for item in value):
                errors.append(f"{scenario_id}: {field} must contain nonempty strings")
    for package_id in EXPECTED_IDS:
        if package_scenarios.get(package_id, 0) == 0:
            errors.append(f"{package_id}: no acceptance scenario is registered")

    run_ids: set[str] = set()
    passed_scenarios: set[str] = set()
    for run in runs:
        if not isinstance(run, dict):
            errors.append("acceptance run record must be an object")
            continue
        run_id = run.get("run_id")
        if not isinstance(run_id, str) or not run_id.strip() or run_id in run_ids:
            errors.append(f"invalid or duplicate acceptance run ID: {run_id!r}")
            continue
        run_ids.add(run_id)
        try:
            uuid.UUID(run_id)
        except ValueError:
            errors.append(f"{run_id}: run_id must be a UUID")
        scenario_id = run.get("acceptance_id")
        if scenario_id not in scenario_ids:
            errors.append(f"{run_id}: references unknown acceptance scenario {scenario_id!r}")
        if run.get("outcome") not in {"passed", "failed", "inconclusive"}:
            errors.append(f"{run_id}: invalid outcome")
        elif run.get("outcome") == "passed" and isinstance(scenario_id, str):
            passed_scenarios.add(scenario_id)
        if not isinstance(run.get("recorded_at_utc"), str) or not run["recorded_at_utc"].endswith("Z"):
            errors.append(f"{run_id}: recorded_at_utc must be an ISO-8601 UTC timestamp")
        else:
            try:
                datetime.fromisoformat(run["recorded_at_utc"].replace("Z", "+00:00"))
            except ValueError:
                errors.append(f"{run_id}: recorded_at_utc is not a valid ISO-8601 timestamp")
        environment = run.get("environment")
        if environment is not None and (
            not isinstance(environment, dict)
            or any(not isinstance(environment.get(key), str) or not environment[key].strip() for key in (
                "system", "release", "machine", "python_version", "python_executable"
            ))
        ):
            errors.append(f"{run_id}: environment metadata is incomplete")
        if not isinstance(run.get("source_revision"), str) or not run["source_revision"].strip():
            errors.append(f"{run_id}: source_revision is required")
        if not is_sha256(run.get("source_tree_sha256")):
            errors.append(f"{run_id}: source_tree_sha256 must be a lowercase SHA-256 digest")
        source_manifest = run.get("source_manifest")
        manifest_file_digests: dict[str, str] | None = None
        if source_manifest is not None:
            if not isinstance(source_manifest, str):
                errors.append(f"{run_id}: source_manifest must be a repository-relative path")
            else:
                manifest_path = (ROOT / source_manifest).resolve()
                try:
                    manifest_path.relative_to(ROOT)
                except ValueError:
                    errors.append(f"{run_id}: source manifest escapes repository: {source_manifest}")
                else:
                    try:
                        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
                    except (OSError, json.JSONDecodeError) as error:
                        errors.append(f"{run_id}: cannot read source manifest {source_manifest}: {error}")
                    else:
                        files = manifest.get("files") if isinstance(manifest, dict) else None
                        if not isinstance(files, list) or manifest.get("schema_version") != 1:
                            errors.append(f"{run_id}: invalid source manifest shape")
                        else:
                            digest = hashlib.sha256()
                            previous_path = None
                            manifest_file_digests = {}
                            for source_file in files:
                                if not isinstance(source_file, dict) or not isinstance(source_file.get("path"), str) or not is_sha256(source_file.get("sha256")):
                                    errors.append(f"{run_id}: malformed source manifest file entry")
                                    continue
                                source_path = source_file["path"]
                                manifest_file_digests[source_path] = source_file["sha256"]
                                if "\\" in source_path or PurePosixPath(source_path).is_absolute() or ".." in PurePosixPath(source_path).parts:
                                    errors.append(f"{run_id}: unsafe source path in manifest: {source_path}")
                                if previous_path is not None and source_path <= previous_path:
                                    errors.append(f"{run_id}: source manifest paths must be unique and sorted")
                                previous_path = source_path
                                encoded = source_path.encode("utf-8")
                                digest.update(len(encoded).to_bytes(4, "big"))
                                digest.update(encoded)
                                digest.update(bytes.fromhex(source_file["sha256"]))
                            calculated = digest.hexdigest()
                            if calculated != run.get("source_tree_sha256") or manifest.get("source_tree_sha256") != calculated:
                                errors.append(f"{run_id}: source manifest does not match source_tree_sha256")
                            artifact_items = run.get("artifacts") if isinstance(run.get("artifacts"), list) else []
                            if not any(isinstance(item, dict) and item.get("path") == source_manifest for item in artifact_items):
                                errors.append(f"{run_id}: source manifest must be included as a hashed output artifact")
        if not isinstance(run.get("command"), list) or not run["command"] or not all(isinstance(part, str) for part in run["command"]):
            errors.append(f"{run_id}: command must be a nonempty string array")
        for field in ("inputs", "artifacts"):
            entries = run.get(field)
            if not isinstance(entries, list):
                errors.append(f"{run_id}: {field} must be an array")
                continue
            if field == "artifacts" and not entries:
                errors.append(f"{run_id}: at least one output artifact is required")
            for entry in entries:
                if not isinstance(entry, dict) or not isinstance(entry.get("path"), str) or not is_sha256(entry.get("sha256")):
                    errors.append(f"{run_id}: {field} entries require path and lowercase SHA-256")
                    continue
                artifact_path = (ROOT / entry["path"]).resolve()
                try:
                    artifact_path.relative_to(ROOT)
                except ValueError:
                    errors.append(f"{run_id}: artifact path escapes repository: {entry['path']}")
                    continue
                if not artifact_path.is_file():
                    errors.append(f"{run_id}: artifact does not exist: {entry['path']}")
                    continue
                if field == "inputs" and manifest_file_digests is not None:
                    actual = manifest_file_digests.get(entry["path"])
                    if actual is None:
                        errors.append(f"{run_id}: input is absent from its source manifest: {entry['path']}")
                        continue
                elif field == "inputs":
                    # Legacy V1 records stored input hashes but not the source manifest needed
                    # to reconstruct an earlier working tree after the input file has changed.
                    # Preserve their recorded digest; new records always include the manifest.
                    actual = entry["sha256"]
                else:
                    actual = hashlib.sha256(artifact_path.read_bytes()).hexdigest()
                if actual != entry["sha256"]:
                    errors.append(f"{run_id}: artifact digest mismatch: {entry['path']}")
    for scenario in scenarios:
        if isinstance(scenario, dict) and scenario.get("status") == "accepted":
            if scenario.get("acceptance_id") not in passed_scenarios:
                errors.append(f"{scenario.get('acceptance_id')}: accepted scenario has no passed run")
    return errors


def is_sha256(value: object) -> bool:
    return isinstance(value, str) and len(value) == 64 and all(character in "0123456789abcdef" for character in value)


if __name__ == "__main__":
    problems = validate()
    if problems:
        print("Strategy refactor ledger validation failed:", file=sys.stderr)
        for problem in problems:
            print(f"- {problem}", file=sys.stderr)
        raise SystemExit(1)
    acceptance = json.loads(ACCEPTANCE.read_text(encoding="utf-8"))
    print(f"Strategy refactor ledger: R01-R23 valid; {len(acceptance['scenarios'])} acceptance scenarios and {len(acceptance['runs'])} artifact-bound runs validated.")
