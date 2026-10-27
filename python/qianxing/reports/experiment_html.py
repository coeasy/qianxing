"""Offline HTML views for stored strategy experiment results.

The report only displays values already recorded in the experiment artifact. It
does not replay strategies or recalculate performance metrics.
"""

from __future__ import annotations

import html
import csv
import hashlib
import io
import json
import os
import tempfile
from collections.abc import Mapping, Sequence
from decimal import Decimal, InvalidOperation, localcontext
from pathlib import Path
from typing import Any, Callable


MAX_DETAIL_RUNS = 12
MAX_EQUITY_POINTS_PER_RUN = 65_536
MAX_FILLS_PER_RUN = 20_000
MAX_DETAIL_ARTIFACT_BYTES = 8 * 1024 * 1024
MAX_DETAIL_PAYLOAD_BYTES = 8 * 1024 * 1024
MAX_MULTI_MARKET_INPUTS = 64
MAX_MULTI_MARKET_EQUITY_POINTS = 65_536
MAX_MULTI_MARKET_CONTRIBUTION_POINTS = 1_000
MAX_MULTI_MARKET_RUN_BYTES = 32 * 1024 * 1024


def _json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True, default=str)


def _inline_json(value: Any) -> str:
    # A JSON script element is not an executable script, but escape HTML parser
    # delimiters as well so untrusted strategy IDs cannot terminate the element.
    return (
        json.dumps(value, ensure_ascii=False, separators=(",", ":"), default=str)
        .replace("&", "\\u0026")
        .replace("<", "\\u003c")
        .replace(">", "\\u003e")
        .replace("\u2028", "\\u2028")
        .replace("\u2029", "\\u2029")
    )


def _mapping(value: Any) -> Mapping[str, Any]:
    return value if isinstance(value, Mapping) else {}


def _sequence(value: Any) -> Sequence[Any]:
    return value if isinstance(value, (list, tuple)) else ()


def _text(value: Any) -> str:
    if value is None:
        return "—"
    if isinstance(value, (dict, list, tuple)):
        return _json(value)
    return str(value)


def _escape(value: Any) -> str:
    return html.escape(_text(value), quote=True)


def _candidate_rows(document: Mapping[str, Any]) -> list[dict[str, Any]]:
    candidates = {
        row.get("ordinal"): row
        for row in _sequence(document.get("candidates"))
        if isinstance(row, Mapping)
    }
    candidates_by_run_id = {
        _mapping(row.get("outcome")).get("run_id"): row
        for row in _sequence(document.get("candidates"))
        if isinstance(row, Mapping) and _mapping(row.get("outcome")).get("run_id")
    }
    ranking = [row for row in _sequence(document.get("ranking")) if isinstance(row, Mapping)]
    comparison = _mapping(document.get("comparison"))
    runs = [row for row in _sequence(comparison.get("runs")) if isinstance(row, Mapping)]
    if not ranking and runs:
        ranking = [
            {
                "rank": row.get("rank"),
                "ordinal": row.get("ordinal")
                or candidates_by_run_id.get(_mapping(row.get("run")).get("run_id"), {}).get("ordinal"),
                "run": _mapping(row.get("run")),
            }
            for row in runs
        ]

    rows: list[dict[str, Any]] = []
    for rank_row in ranking:
        candidate = candidates.get(rank_row.get("ordinal")) or candidates_by_run_id.get(
            _mapping(rank_row.get("run")).get("run_id", rank_row.get("run_id")), {}
        )
        outcome = _mapping(candidate.get("outcome"))
        run = _mapping(rank_row.get("run"))
        metrics = {**outcome, **run, **rank_row}
        rows.append(
            {
                "rank": rank_row.get("rank"),
                "ordinal": rank_row.get("ordinal"),
                "run_id": metrics.get("run_id"),
                "parameters": candidate.get("parameters", {}),
                "return_bps": metrics.get("return_bps"),
                "max_drawdown_bps": metrics.get("max_drawdown_bps"),
                "fills": metrics.get("successful_fills", metrics.get("fills")),
                "status": "failed" if candidate.get("error") else "completed",
                "error": candidate.get("error"),
            }
        )
    ranked_ordinals = {row.get("ordinal") for row in rows}
    for candidate in candidates.values():
        if candidate.get("error") and candidate.get("ordinal") not in ranked_ordinals:
            rows.append(
                {
                    "rank": None,
                    "ordinal": candidate.get("ordinal"),
                    "run_id": _mapping(candidate.get("outcome")).get("run_id"),
                    "parameters": candidate.get("parameters", {}),
                    "return_bps": None,
                    "max_drawdown_bps": None,
                    "fills": None,
                    "status": "failed",
                    "error": candidate.get("error"),
                }
            )
    return rows


def _artifact_path(value: Any, *, base_dir: Path, run_dir: Path) -> Path:
    if not isinstance(value, str) or not value:
        raise ValueError("candidate run artifact path is missing")
    path = Path(value)
    if not path.is_absolute():
        path = base_dir / path
    resolved = path.resolve(strict=True)
    if not resolved.is_file() or resolved.parent != run_dir:
        raise ValueError("candidate artifacts must be regular files in one run directory")
    return resolved


def _read_artifact(path: Path) -> bytes:
    size = path.stat().st_size
    if size < 0 or size > MAX_DETAIL_ARTIFACT_BYTES:
        raise ValueError(f"candidate run artifact exceeds the {MAX_DETAIL_ARTIFACT_BYTES}-byte report limit")
    return path.read_bytes()


def _json_object(raw: bytes, label: str) -> dict[str, Any]:
    try:
        value = json.loads(raw.decode("utf-8-sig"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"candidate {label} artifact is not valid UTF-8 JSON") from exc
    if not isinstance(value, dict):
        raise ValueError(f"candidate {label} artifact must be a JSON object")
    return value


def _csv_rows(raw: bytes, label: str, required: set[str], limit: int) -> list[dict[str, str]]:
    try:
        reader = csv.DictReader(io.StringIO(raw.decode("utf-8-sig", errors="strict")))
        if reader.fieldnames is None or not required.issubset(reader.fieldnames):
            raise ValueError(f"candidate {label} CSV is missing required columns")
        rows = []
        for row in reader:
            if len(rows) >= limit:
                raise ValueError(f"candidate {label} CSV exceeds the {limit}-row report limit")
            if None in row:
                raise ValueError(f"candidate {label} CSV has an invalid row shape")
            rows.append({str(key): str(value or "") for key, value in row.items() if key is not None})
        return rows
    except UnicodeDecodeError as exc:
        raise ValueError(f"candidate {label} CSV is not valid UTF-8") from exc


def load_verified_candidate_details(
    result: Mapping[str, Any],
    *,
    base_dir: str | os.PathLike[str] | None = None,
    rust_verifier: Callable[[Mapping[str, Any]], Any] | None = None,
    rust_report_reader: Callable[[Mapping[str, Any]], Any] | None = None,
) -> dict[str, Any]:
    """Load bounded per-run detail after Rust consistency and RunRecord digest checks.

    The CLI supplies the shared Rust report reader. The legacy ``rust_verifier`` callback
    remains available for callers that only need a verification verdict.
    """
    root = Path(base_dir) if base_dir is not None else Path.cwd()
    root = root.resolve(strict=True)
    candidates = [row for row in _sequence(result.get("candidates")) if isinstance(row, Mapping)]
    ranks = {
        _mapping(row.get("run")).get("run_id"): row.get("rank")
        for row in _sequence(_mapping(result.get("comparison")).get("runs"))
        if isinstance(row, Mapping)
    }
    comparison_runs = {
        _mapping(row.get("run")).get("run_id"): _mapping(row.get("run"))
        for row in _sequence(_mapping(result.get("comparison")).get("runs"))
        if isinstance(row, Mapping)
    }
    eligible = [
        row for row in candidates
        if isinstance(_mapping(_mapping(row.get("outcome")).get("artifacts")), Mapping)
        and _mapping(_mapping(row.get("outcome")).get("artifacts"))
    ]
    eligible.sort(key=lambda row: (ranks.get(_mapping(row.get("outcome")).get("run_id"), 1_000_000), row.get("ordinal", 0)))
    selected = eligible[:MAX_DETAIL_RUNS]
    details: list[dict[str, Any]] = []
    total_bytes = 0

    for candidate in selected:
        outcome = _mapping(candidate.get("outcome"))
        engine_verification: dict[str, Any] | None = None
        shared_core_report: Mapping[str, Any] | None = None
        artifacts = _mapping(outcome.get("artifacts"))
        summary_hint = artifacts.get("summary")
        if not isinstance(summary_hint, str):
            raise ValueError("candidate artifact list does not include its run summary")
        summary_path = Path(summary_hint)
        if not summary_path.is_absolute():
            summary_path = root / summary_path
        summary_path = summary_path.resolve(strict=True)
        if not summary_path.is_file():
            raise ValueError("candidate run summary is not a regular file")
        run_dir = summary_path.parent

        paths = {
            name: _artifact_path(artifacts.get(name), base_dir=root, run_dir=run_dir)
            for name in ("summary", "run_manifest", "equity", "fills")
        }
        summary_bytes = _read_artifact(paths["summary"])
        summary = _json_object(summary_bytes, "summary")
        run_record_hint = summary.get("run_record")
        record_path = _artifact_path(run_record_hint, base_dir=root, run_dir=run_dir)
        payloads = {name: _read_artifact(path) for name, path in paths.items()}
        record_bytes = _read_artifact(record_path)
        if sum(map(len, payloads.values())) + len(record_bytes) > MAX_DETAIL_ARTIFACT_BYTES:
            raise ValueError("candidate run artifacts exceed the aggregate report limit")
        run_record = _json_object(record_bytes, "RunRecord")
        run_manifest = _json_object(payloads["run_manifest"], "RunManifest")
        declared_manifest_path = _artifact_path(
            summary.get("run_manifest"), base_dir=root, run_dir=run_dir
        )
        if declared_manifest_path != paths["run_manifest"]:
            raise ValueError("candidate summary RunManifest path does not match the experiment artifact")
        record_schema_version = run_record.get("schema_version")
        if record_schema_version not in (1, 2):
            raise ValueError(f"candidate RunRecord schema version {record_schema_version!r} is unsupported")
        ref_rows = [row for row in _sequence(run_record.get("artifact_refs")) if isinstance(row, Mapping)]
        record_refs = {row.get("name"): row for row in ref_rows if isinstance(row.get("name"), str)}
        if len(record_refs) != len(ref_rows):
            raise ValueError("candidate RunRecord has duplicate or invalid artifact references")
        for name, path in paths.items():
            ref = record_refs.get(name)
            if not isinstance(ref, Mapping):
                raise ValueError(f"candidate RunRecord has no {name} content identity")
            if record_schema_version == 1:
                location = ref.get("path")
                content_id = ref.get("digest")
            else:
                if set(ref) != {"name", "artifact_id", "kind", "schema_version", "byte_len"}:
                    raise ValueError(f"candidate RunRecord V2 {name} reference has unsupported fields")
                expected_kind = {
                    "summary": "backtest-summary-json",
                    "equity": "backtest-equity-csv",
                    "fills": "backtest-fills-csv",
                    "run_manifest": "run-manifest-json",
                }[name]
                if name == "summary":
                    expected_schema_version = summary.get("schema_version")
                elif name == "equity":
                    try:
                        equity_header = payloads["equity"].splitlines()[0].decode("utf-8-sig").split(",")
                    except (IndexError, UnicodeDecodeError) as exc:
                        raise ValueError("candidate equity CSV has no valid header") from exc
                    expected_schema_version = 2 if "equity_exact" in equity_header else 1
                elif name == "fills":
                    # RunRecord V2 writes fills schema v2 today, while the Rust
                    # reader intentionally accepts historical v1 and compatible
                    # v3 artifacts. Validate against that contract rather than
                    # assuming every V2 RunRecord points at fills schema v1.
                    expected_schema_version = ref.get("schema_version")
                    if expected_schema_version not in (1, 2, 3):
                        expected_schema_version = None
                else:
                    expected_schema_version = 1
                artifact_id = ref.get("artifact_id")
                if (
                    ref.get("kind") != expected_kind
                    or ref.get("schema_version") != expected_schema_version
                    or not isinstance(ref.get("byte_len"), int)
                    or isinstance(ref.get("byte_len"), bool)
                    or ref.get("byte_len") < 0
                    or not isinstance(artifact_id, str)
                    or len(artifact_id) != 64
                    or any(char not in "0123456789abcdef" for char in artifact_id)
                ):
                    raise ValueError(f"candidate RunRecord V2 {name} metadata is invalid")
                locations = summary.get("artifact_locations")
                if not isinstance(locations, Mapping):
                    raise ValueError("candidate RunRecord V2 summary has no artifact_locations")
                location = locations.get(name)
                content_id = ref.get("artifact_id")
                if ref.get("byte_len") != len(payloads[name]):
                    raise ValueError(f"candidate RunRecord V2 {name} byte length does not match its artifact")
                if name == "fills" and ref.get("schema_version") == 2:
                    try:
                        fills_header = payloads["fills"].splitlines()[0].decode("utf-8-sig").split(",")
                    except (IndexError, UnicodeDecodeError) as exc:
                        raise ValueError("candidate fills CSV has no valid header") from exc
                    if not {"qty_exact", "price_exact", "exact_fees_json"}.issubset(fills_header):
                        raise ValueError("candidate RunRecord V2 fills schema v2 is missing exact fill columns")
            referenced_path = _artifact_path(location, base_dir=root, run_dir=run_dir)
            if referenced_path != path:
                raise ValueError(f"candidate RunRecord {name} location does not match the experiment artifact")
            if rust_verifier is not None or rust_report_reader is not None:
                rust_path = Path(str(location))
                if not rust_path.is_absolute():
                    rust_path = Path.cwd() / rust_path
                if rust_path.resolve(strict=True) != path:
                    raise ValueError(
                        f"candidate RunRecord {name} location cannot be safely resolved by Rust VerifyRun"
                    )
        if rust_verifier is not None or rust_report_reader is not None:
            for label, hint, resolved in (
                ("RunRecord", run_record_hint, record_path),
                ("RunManifest", summary.get("run_manifest"), paths["run_manifest"]),
            ):
                rust_path = Path(str(hint))
                if not rust_path.is_absolute():
                    rust_path = Path.cwd() / rust_path
                if rust_path.resolve(strict=True) != resolved:
                    raise ValueError(
                        f"candidate summary {label} path cannot be safely resolved by Rust VerifyRun"
                    )
        if rust_report_reader is not None:
            # Give Rust only the already-contained artifact paths. This keeps its
            # file reads under the same report boundary even for untrusted results.
            safe_outcome = {
                **dict(outcome),
                "artifacts": {name: str(path) for name, path in paths.items()},
            }
            try:
                core_report = rust_report_reader(safe_outcome)
            except Exception as exc:
                raise ValueError(
                    f"candidate {outcome.get('run_id')} Rust report reader failed: {exc}"
                ) from exc
            if not isinstance(core_report, Mapping):
                raise ValueError("Rust report reader returned a non-object report")
            shared_core_report = core_report
            verification = _mapping(core_report.get("verification"))
            verified = verification.get("verified")
            mismatches = _sequence(verification.get("mismatches"))
            checks = _sequence(verification.get("checks"))
            rust_summary = core_report.get("summary")
            if (
                core_report.get("schema_version") != 1
                or core_report.get("run_id") != outcome.get("run_id")
                or not isinstance(rust_summary, Mapping)
                or dict(rust_summary) != summary
            ):
                raise ValueError("Rust report DTO identity or summary differs from the bounded artifact read")
            if verified is not True:
                reasons = "; ".join(str(item) for item in _sequence(mismatches))
                suffix = f": {reasons}" if reasons else ""
                raise ValueError(
                    f"candidate {outcome.get('run_id')} failed Rust report verification{suffix}"
                )
            engine_verification = {
                "verified": True,
                "report_schema_version": core_report["schema_version"],
                "checks": [str(item) for item in _sequence(checks)],
            }
        elif rust_verifier is not None:
            # Legacy callback form stays available to downstream users.
            safe_outcome = {
                **dict(outcome),
                "artifacts": {name: str(path) for name, path in paths.items()},
            }
            try:
                verification = rust_verifier(safe_outcome)
            except Exception as exc:
                raise ValueError(
                    f"candidate {outcome.get('run_id')} Rust VerifyRun failed: {exc}"
                ) from exc
            verified = getattr(verification, "verified", None)
            mismatches = getattr(verification, "mismatches", ())
            checks = getattr(verification, "checks", ())
            if isinstance(verification, Mapping):
                verified = verification.get("verified", verified)
                mismatches = verification.get("mismatches", mismatches)
                checks = verification.get("checks", checks)
            if verified is not True:
                reasons = "; ".join(str(item) for item in _sequence(mismatches))
                suffix = f": {reasons}" if reasons else ""
                raise ValueError(
                    f"candidate {outcome.get('run_id')} failed Rust VerifyRun{suffix}"
                )
            engine_verification = {
                "verified": True,
                "checks": [str(item) for item in _sequence(checks)],
            }
        expected = {
            "run_id": outcome.get("run_id"),
            "result_hash": outcome.get("result_hash"),
            "data_fingerprint": outcome.get("data_fingerprint"),
        }
        if outcome.get("instrument") is not None:
            expected["instrument"] = outcome.get("instrument")
        comparison_run = comparison_runs.get(outcome.get("run_id"), {})
        for key in ("instrument", "result_hash", "data_fingerprint"):
            if comparison_run.get(key) is not None:
                expected[key] = comparison_run[key]
        for key, value in expected.items():
            if value is None or summary.get(key) != value or (key != "instrument" and run_manifest.get(key) != value):
                raise ValueError(f"candidate {key} does not match its run artifacts")
        if (
            run_record.get("run_id") != outcome.get("run_id")
            or run_record.get("status") != "completed"
            or run_record.get("replay_verdict") != "verified"
            or summary.get("run_manifest") is None
            or declared_manifest_path != paths["run_manifest"]
        ):
            raise ValueError("candidate run record or manifest identity is incomplete")

        for name, path in paths.items():
            ref = record_refs.get(name)
            if (
                rust_verifier is None
                and rust_report_reader is None
                and hashlib.sha256(payloads[name]).hexdigest()
                != ref.get("digest" if record_schema_version == 1 else "artifact_id")
            ):
                raise ValueError(f"candidate RunRecord {name} SHA-256 verification failed")

        equity = _csv_rows(payloads["equity"], "equity", {"ts", "equity_raw", "position_raw"}, MAX_EQUITY_POINTS_PER_RUN)
        fills = _csv_rows(payloads["fills"], "fills", {"ts", "order_id", "qty_raw", "price_raw", "fee_raw"}, MAX_FILLS_PER_RUN)
        for row in equity:
            for key in ("ts", "equity_raw", "position_raw"):
                try:
                    int(row[key])
                except ValueError as exc:
                    raise ValueError(f"candidate equity CSV has a non-integer {key}") from exc
            if "equity_exact" in row:
                try:
                    exact_value = Decimal(row["equity_exact"])
                except (InvalidOperation, ValueError) as exc:
                    raise ValueError("candidate equity CSV has an invalid equity_exact") from exc
                if not exact_value.is_finite():
                    raise ValueError("candidate equity CSV has a non-finite equity_exact")
        for row in fills:
            for key in ("ts", "order_id", "qty_raw", "price_raw", "fee_raw"):
                try:
                    int(row[key])
                except ValueError as exc:
                    raise ValueError(f"candidate fills CSV has a non-integer {key}") from exc
        if summary.get("equity_points") != len(equity) or summary.get("fills") != len(fills):
            raise ValueError("candidate CSV row counts do not match the run summary")
        if shared_core_report is not None:
            core_equity = _mapping(shared_core_report.get("equity"))
            core_fills = _mapping(shared_core_report.get("fills"))
            if (
                core_equity.get("total") != summary.get("equity_points")
                or core_fills.get("total") != summary.get("fills")
            ):
                raise ValueError("Rust report DTO row counts do not match the run summary")
            rust_fill_rows = [
                _mapping(row) for row in _sequence(core_fills.get("rows"))
            ]
            if rust_fill_rows != fills[: len(rust_fill_rows)]:
                raise ValueError("Rust report DTO fills sample differs from the bounded CSV read")
            core_equity_rows = [
                _mapping(row) for row in _sequence(core_equity.get("rows"))
            ]
            stride = max(1, (len(equity) + 998) // 999)
            expected_indexes = [
                index for index in range(len(equity))
                if index % stride == 0 or index + 1 == len(equity)
            ]
            expected_equity_rows = [
                {
                    "ts": equity[index]["ts"],
                    "equity_raw": equity[index]["equity_raw"],
                    "position_raw": equity[index]["position_raw"],
                }
                for index in expected_indexes
            ]
            if core_equity_rows != expected_equity_rows:
                raise ValueError("Rust report DTO equity sample differs from the bounded CSV read")
        if equity and str(summary.get("final_equity_raw")) != equity[-1]["equity_raw"]:
            raise ValueError("candidate final equity does not match the saved equity curve")
        equity_metrics = _mapping(_mapping(summary.get("metrics_exact")).get("equity"))
        if equity and "equity_exact" in equity[-1] and equity_metrics.get("final") is not None:
            if Decimal(equity[-1]["equity_exact"]) != Decimal(str(equity_metrics["final"])):
                raise ValueError("candidate exact final equity does not match the saved exact curve")
        if summary.get("fees_raw") is not None and sum(int(row["fee_raw"]) for row in fills) != int(summary["fees_raw"]):
            raise ValueError("candidate fill fees do not match the run summary")

        size = sum(map(len, payloads.values())) + len(record_bytes)
        total_bytes += size
        if total_bytes > MAX_DETAIL_PAYLOAD_BYTES:
            raise ValueError("verified candidate details exceed the total embedded report limit")
        verified_artifacts = ["summary", "run manifest", "equity", "fills"]
        if engine_verification is not None:
            rust_label = (
                "Rust report/VerifyRun v1"
                if engine_verification.get("report_schema_version") == 1
                else "Rust VerifyRun"
            )
            verified_artifacts.extend([rust_label, "RunRecord SHA-256 (Rust)"])
        else:
            verified_artifacts.append("RunRecord SHA-256")
        detail = {
            "rank": ranks.get(outcome.get("run_id")),
            "ordinal": candidate.get("ordinal"),
            "run_id": outcome.get("run_id"),
            "parameters": candidate.get("parameters", {}),
            "instrument": summary.get("instrument"),
            "account_id": summary.get("account_id"),
            "strategy_id": summary.get("strategy_id"),
            "strategy_version": summary.get("strategy_version"),
            "currency": summary.get("currency"),
            "initial_cash_raw": _mapping(summary.get("account")).get("initial_cash_raw"),
            "initial_cash_exact": _mapping(summary.get("account")).get("initial_cash_exact"),
            "final_equity_raw": summary.get("final_equity_raw"),
            "fees_raw": summary.get("fees_raw"),
            "turnover_raw": summary.get("turnover_raw"),
            "metrics_exact": summary.get("metrics_exact"),
            "fee_model": summary.get("fee_model"),
            "fill_model": summary.get("fill_model_details", summary.get("fill_model")),
            "data_fingerprint": summary.get("data_fingerprint"),
            "equity": equity,
            "fills": fills,
            "verified_artifacts": verified_artifacts,
        }
        if engine_verification is not None:
            detail["engine_verification"] = engine_verification
        details.append(detail)

    payload = {
        "schema_version": 1,
        "runs": details,
        "omitted_verified_runs": max(0, len(eligible) - len(selected)),
        "candidates_without_artifact_details": max(0, len(candidates) - len(eligible)),
        "embedded_artifact_bytes": total_bytes,
    }
    if len(_inline_json(payload).encode("utf-8")) > MAX_DETAIL_PAYLOAD_BYTES:
        raise ValueError("verified candidate details exceed the total embedded report limit")
    return payload


def _resolve_run_reference(reference: Any, artifact_path: Path, *, sibling: bool) -> Path:
    if not isinstance(reference, str) or not reference:
        raise ValueError("multi-market run artifact path is missing")
    path = Path(reference)
    if not path.is_absolute() and not path.exists():
        path = artifact_path.parent / path
    resolved = path.resolve(strict=True)
    if not resolved.is_file() or (sibling and resolved.parent != artifact_path.parent):
        raise ValueError("multi-market run files must be regular sibling artifacts")
    return resolved


def _same_existing_path(left: Path, right: Path) -> bool:
    try:
        return os.path.samefile(left, right)
    except OSError:
        def normalized(path: Path) -> str:
            value = os.path.normcase(str(path.resolve(strict=True)))
            if value.startswith("\\\\?\\"):
                value = value[4:]
            return value.rstrip("\\/")

        return normalized(left) == normalized(right)


def _read_multi_artifact(path: Path) -> bytes:
    size = path.stat().st_size
    if size < 0 or size > MAX_MULTI_MARKET_RUN_BYTES:
        raise ValueError("multi-market run artifact exceeds the 32 MiB verification limit")
    return path.read_bytes()


def _sample_indices(length: int, limit: int) -> list[int]:
    if length <= limit:
        return list(range(length))
    stride = max(1, (length + limit - 2) // (limit - 1))
    indices = list(range(0, length, stride))
    if indices[-1] != length - 1:
        indices.append(length - 1)
    return indices


def _exact_decimal(value: Any, label: str) -> Decimal:
    if not isinstance(value, str):
        raise ValueError(f"{label} must be a decimal string")
    try:
        parsed = Decimal(value)
    except InvalidOperation as exc:
        raise ValueError(f"{label} is not a valid decimal") from exc
    if not parsed.is_finite():
        raise ValueError(f"{label} must be finite")
    return parsed


def _exact_decimal_wire_string(value: Any, label: str) -> str:
    """Project the Rust coefficient/scale wire value to a readable decimal string."""
    if not isinstance(value, Mapping):
        raise ValueError(f"{label} must use the ExactDecimal coefficient/scale shape")
    coefficient = value.get("coefficient")
    scale = value.get("scale")
    if not isinstance(coefficient, str) or isinstance(scale, bool) or not isinstance(scale, int):
        raise ValueError(f"{label} has an invalid ExactDecimal coefficient or scale")
    try:
        integer = int(coefficient)
    except ValueError as exc:
        raise ValueError(f"{label} has an invalid ExactDecimal coefficient") from exc
    if str(integer) != coefficient or not 0 <= scale <= 18:
        raise ValueError(f"{label} has a non-canonical coefficient or unsupported scale")
    with localcontext() as decimal_context:
        decimal_context.prec = max(80, len(coefficient) + scale + 2)
        return format(Decimal(integer).scaleb(-scale), "f")


def _project_multi_market_fill_exact_values(fills: Sequence[Any]) -> list[dict[str, Any]]:
    projected: list[dict[str, Any]] = []
    for index, value in enumerate(fills):
        fill = _mapping(value)
        row = dict(fill)
        amounts_value = fill.get("exact_amounts")
        if amounts_value is not None and not isinstance(amounts_value, Mapping):
            raise ValueError(f"multi-market fill {index} exact_amounts must be an object")
        amounts = _mapping(amounts_value)
        if amounts_value is not None and not {"quantity", "price"}.issubset(amounts):
            raise ValueError(f"multi-market fill {index} exact_amounts is incomplete")
        for source, target in (("quantity", "qty_exact"), ("price", "price_exact")):
            if source in amounts:
                row[target] = _exact_decimal_wire_string(
                    amounts[source], f"multi-market fill {index} {source}"
                )
        exact_fees = []
        exact_fees_value = fill.get("exact_fees", [])
        if not isinstance(exact_fees_value, list):
            raise ValueError(f"multi-market fill {index} exact_fees must be an array")
        for fee_index, fee_value in enumerate(exact_fees_value):
            if not isinstance(fee_value, Mapping):
                raise ValueError(f"multi-market fill {index} exact fee {fee_index} must be an object")
            fee = _mapping(fee_value)
            fee_row = dict(fee)
            if "amount" not in fee:
                raise ValueError(f"multi-market fill {index} exact fee {fee_index} has no amount")
            fee_row["amount"] = _exact_decimal_wire_string(
                fee["amount"], f"multi-market fill {index} exact fee {fee_index}"
            )
            exact_fees.append(fee_row)
        if exact_fees:
            row["exact_fees_json"] = json.dumps(
                exact_fees, ensure_ascii=False, separators=(",", ":")
            )
        projected.append(row)
    return projected


def load_verified_multi_market_candidate_details(
    result: Mapping[str, Any],
    *,
    market_validator: Callable[[dict[str, Any]], Any],
    base_dir: str | os.PathLike[str] | None = None,
) -> dict[str, Any]:
    """Load shared-account Bar run details after Rust input validation and artifact checks.

    The Rust backtest marks its EventLog replay before writing this artifact. This loader
    independently rechecks that marker, RunManifest identities and input components,
    BarFrame fingerprints, persisted equity/fill sidecars, and the per-instrument equity
    conservation invariant. It does not recompute the composed market fingerprint or rerun
    the event ledger; use ``qx-cli report`` for independent fingerprint and replay checks.
    """
    root = Path(base_dir) if base_dir is not None else Path.cwd()
    root = root.resolve(strict=True)
    candidates = [row for row in _sequence(result.get("candidates")) if isinstance(row, Mapping)]
    ranks = {
        row.get("run_id"): row.get("rank")
        for row in _sequence(result.get("ranking"))
        if isinstance(row, Mapping)
    }
    eligible = [
        row for row in candidates
        if isinstance(_mapping(row.get("outcome")).get("artifact_path"), str)
        and _mapping(row.get("outcome")).get("artifact_path")
    ]
    eligible.sort(
        key=lambda row: (
            ranks.get(_mapping(row.get("outcome")).get("run_id"), 1_000_000),
            row.get("ordinal", 0),
        )
    )
    selected = eligible[:MAX_DETAIL_RUNS]
    details: list[dict[str, Any]] = []
    protected: set[str] = set()
    embedded_bytes = 0

    for candidate in selected:
        outcome = _mapping(candidate.get("outcome"))
        run_path = Path(str(outcome["artifact_path"]))
        if not run_path.is_absolute():
            run_path = root / run_path
        run_path = run_path.resolve(strict=True)
        if not run_path.is_file():
            raise ValueError("multi-market run artifact is not a regular file")
        artifact_bytes = _read_multi_artifact(run_path)
        artifact = _json_object(artifact_bytes, "multi-market run")
        if artifact.get("schema_version") not in (1, 2) or artifact.get("replay_verified") is not True:
            raise ValueError("multi-market run artifact lacks the completed Rust replay verification marker")

        run_outcome = _mapping(artifact.get("outcome"))
        expected_run = {
            "run_id": outcome.get("run_id"),
            "data_fingerprint": outcome.get("data_fingerprint"),
            "shared_result_hash": outcome.get("shared_result_hash"),
            "spec_fingerprint": outcome.get("spec_fingerprint"),
        }
        for key, value in expected_run.items():
            if value is None or run_outcome.get(key) != value:
                raise ValueError(f"multi-market candidate {key} does not match its run artifact")
        if outcome.get("account_id") is not None and run_outcome.get("account_id") != outcome.get("account_id"):
            raise ValueError("multi-market candidate account identity does not match its run artifact")

        run_manifest_path = _resolve_run_reference(artifact.get("run_manifest_path"), run_path, sibling=True)
        equity_path = _resolve_run_reference(artifact.get("equity_path"), run_path, sibling=True)
        fills_path = _resolve_run_reference(artifact.get("fills_path"), run_path, sibling=True)
        contribution_path = _resolve_run_reference(
            artifact.get("instrument_equity_contributions_path"), run_path, sibling=True
        )
        exact_equity_reference = artifact.get("equity_exact_path")
        exact_equity_path = (
            _resolve_run_reference(exact_equity_reference, run_path, sibling=True)
            if exact_equity_reference is not None
            else None
        )
        manifest_bytes = _read_multi_artifact(run_manifest_path)
        equity_bytes = _read_multi_artifact(equity_path)
        fills_bytes = _read_multi_artifact(fills_path)
        contribution_bytes = _read_multi_artifact(contribution_path)
        exact_equity_bytes = _read_multi_artifact(exact_equity_path) if exact_equity_path else None
        all_artifact_bytes = (artifact_bytes, manifest_bytes, equity_bytes, fills_bytes, contribution_bytes)
        if exact_equity_bytes is not None:
            all_artifact_bytes += (exact_equity_bytes,)
        if sum(map(len, all_artifact_bytes)) > MAX_MULTI_MARKET_RUN_BYTES:
            raise ValueError("multi-market run and sidecars exceed the 32 MiB per-run verification limit")
        manifest = _json_object(manifest_bytes, "multi-market RunManifest")
        try:
            equity = json.loads(equity_bytes.decode("utf-8-sig"))
            fills = json.loads(fills_bytes.decode("utf-8-sig"))
            contributions = json.loads(contribution_bytes.decode("utf-8-sig"))
            exact_equity = (
                json.loads(exact_equity_bytes.decode("utf-8-sig"))
                if exact_equity_bytes is not None
                else None
            )
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ValueError("multi-market equity, fills, contribution or exact equity sidecar is invalid JSON") from exc

        if (
            manifest.get("run_id") != outcome.get("run_id")
            or manifest.get("data_fingerprint") != outcome.get("data_fingerprint")
            or manifest.get("result_hash") != outcome.get("shared_result_hash")
            or manifest.get("strategy_version") != f"multi-market-strategy-set:{outcome.get('spec_fingerprint')}"
        ):
            raise ValueError("multi-market RunManifest identity does not match the candidate outcome")
        if not isinstance(equity, list) or not equity or len(equity) > MAX_MULTI_MARKET_EQUITY_POINTS:
            raise ValueError("multi-market equity curve must contain 1..=65,536 points")
        if not isinstance(fills, list) or len(fills) > MAX_FILLS_PER_RUN:
            raise ValueError("multi-market fills must contain at most 20,000 rows")
        if not isinstance(contributions, dict):
            raise ValueError("multi-market contribution sidecar must be indexed by instrument")
        if equity != artifact.get("equity"):
            raise ValueError("multi-market equity sidecar differs from the embedded run curve")

        spec = _mapping(artifact.get("spec"))
        market_specs = [row for row in _sequence(spec.get("markets")) if isinstance(row, Mapping)]
        market_data = [row for row in _sequence(artifact.get("market_data")) if isinstance(row, Mapping)]
        if not market_specs or len(market_specs) != len(market_data) or len(market_data) > MAX_MULTI_MARKET_INPUTS:
            raise ValueError("multi-market report requires 1..=64 uniquely declared Bar inputs")
        spec_by_instrument = {row.get("instrument"): row for row in market_specs}
        data_by_instrument = {row.get("instrument"): row for row in market_data}
        if (
            len(spec_by_instrument) != len(market_specs)
            or len(data_by_instrument) != len(market_data)
            or set(spec_by_instrument) != set(data_by_instrument)
        ):
            raise ValueError("multi-market Bar input instruments do not match the saved spec")

        components: dict[str, str] = {}
        verified_markets: list[dict[str, Any]] = []
        for data_row in market_data:
            instrument = str(data_row["instrument"])
            market_spec = spec_by_instrument[instrument]
            frame_path = _resolve_run_reference(data_row.get("path"), run_path, sibling=False)
            configured_path = _resolve_run_reference(market_spec.get("bars_path"), run_path, sibling=False)
            if not _same_existing_path(frame_path, configured_path):
                raise ValueError(f"multi-market spec and recorded input path differ for {instrument}")
            dataset_spec: dict[str, Any] = {
                "schema_version": 1,
                "dataset_id": "multi-market-report-" + hashlib.sha256(instrument.encode("utf-8")).hexdigest()[:16],
                "bars_path": str(frame_path),
            }
            quality = _mapping(_mapping(artifact.get("dataset_quality")).get(instrument))
            if market_spec.get("dataset_manifest_path"):
                quality_path = _resolve_run_reference(quality.get("manifest_path"), run_path, sibling=False)
                configured_quality = _resolve_run_reference(market_spec.get("dataset_manifest_path"), run_path, sibling=False)
                if not _same_existing_path(quality_path, configured_quality):
                    raise ValueError(f"dataset quality path does not match the multi-market spec for {instrument}")
                quality_bytes = _read_multi_artifact(quality_path)
                quality_sha = hashlib.sha256(quality_bytes).hexdigest()
                if quality_sha != quality.get("manifest_sha256"):
                    raise ValueError(f"dataset quality manifest SHA-256 verification failed for {instrument}")
                dataset_spec["dataset_manifest_path"] = str(quality_path)
                quality_fingerprint = quality.get("manifest_fingerprint")
                if not isinstance(quality_fingerprint, str):
                    raise ValueError(f"dataset quality fingerprint is missing for {instrument}")
                components[f"dataset-quality:{instrument}"] = quality_fingerprint
            verdict = market_validator(dataset_spec)
            fingerprint = str(data_row.get("content_hash") or "")
            if (
                not getattr(verdict, "usable", False)
                or getattr(verdict, "instrument", None) != instrument
                or getattr(verdict, "rows", None) != data_row.get("row_count")
                or getattr(verdict, "content_fingerprint", None) != f"barframe:{fingerprint}"
            ):
                raise ValueError(f"Rust BarFrame validation does not match the saved run input for {instrument}")
            components[f"barframe:{instrument}"] = fingerprint
            verified_markets.append({
                "instrument": instrument,
                "content_hash": fingerprint,
                "row_count": data_row.get("row_count"),
                "source_path": str(frame_path),
                "priority": market_spec.get("priority"),
                "strategies": market_spec.get("strategies", []),
                "fills": _mapping(artifact.get("per_market_fills")).get(instrument, 0),
                "fee_schedule": market_spec.get("fee_schedule"),
                "market_spec": market_spec.get("market_spec"),
            })

        components["derived:instrument-equity-contributions"] = hashlib.sha256(contribution_bytes).hexdigest()
        if exact_equity_bytes is not None:
            components["derived:equity-exact"] = hashlib.sha256(exact_equity_bytes).hexdigest()
        if manifest.get("data_fingerprint") != outcome.get("data_fingerprint"):
            raise ValueError("multi-market RunManifest data identity does not match the candidate outcome")
        if manifest.get("input_components") != components:
            raise ValueError("multi-market RunManifest input component fingerprints do not match the verified artifacts")

        contribution_instruments = set(contributions)
        expected_instruments = set(spec_by_instrument)
        if contribution_instruments != expected_instruments:
            raise ValueError("multi-market contribution instruments do not match the verified Bar inputs")
        initial_cash = spec.get("initial_cash_raw")
        if type(initial_cash) is not int or initial_cash <= 0:
            raise ValueError("multi-market spec initial_cash_raw must be a positive integer")
        exact_equity_values: list[Decimal] | None = None
        exact_equity_strings: list[str] | None = None
        if exact_equity is not None:
            if not isinstance(exact_equity, list) or len(exact_equity) != len(equity):
                raise ValueError("multi-market exact equity sidecar must match every shared equity point")
            exact_equity_values = []
            exact_equity_strings = []
            for index, item in enumerate(exact_equity):
                row = _mapping(item)
                if set(row) != {"equity_index", "ts", "equity_exact"}:
                    raise ValueError("multi-market exact equity row has an invalid shape")
                if (
                    type(row.get("equity_index")) is not int
                    or row["equity_index"] != index
                    or type(row.get("ts")) is not int
                    or not isinstance(equity[index], list)
                    or len(equity[index]) != 2
                    or row["ts"] != equity[index][0]
                ):
                    raise ValueError("multi-market exact equity axis differs from raw shared equity")
                exact_value = row.get("equity_exact")
                exact_equity_values.append(_exact_decimal(exact_value, "exact equity"))
                exact_equity_strings.append(exact_value)
        contribution_curves: dict[str, list[dict[str, Any]]] = {}
        for instrument in sorted(expected_instruments):
            rows = contributions.get(instrument)
            if not isinstance(rows, list) or not rows or len(rows) > MAX_MULTI_MARKET_CONTRIBUTION_POINTS:
                raise ValueError(f"{instrument} contribution curve must contain 1..=1,000 rows")
            previous_index = -1
            curve = []
            for row in rows:
                item = _mapping(row)
                raw_shape = {"equity_index", "ts", "pnl_contribution_raw"}
                exact_shape = raw_shape | {"pnl_contribution_exact"}
                if set(item) not in (raw_shape, exact_shape):
                    raise ValueError(f"{instrument} contribution row has an invalid shape")
                if exact_equity_values is not None and set(item) != exact_shape:
                    raise ValueError(f"{instrument} exact contribution is missing from the exact report")
                if exact_equity_values is None and "pnl_contribution_exact" in item:
                    raise ValueError("exact contribution exists without an exact shared equity sidecar")
                index = item.get("equity_index")
                timestamp = item.get("ts")
                raw = item.get("pnl_contribution_raw")
                if type(index) is not int or type(timestamp) is not int or not isinstance(raw, str):
                    raise ValueError(f"{instrument} contribution row has an invalid numeric value")
                try:
                    contribution = int(raw)
                except ValueError as exc:
                    raise ValueError(f"{instrument} contribution raw value is invalid") from exc
                if index <= previous_index or index < 0 or index >= len(equity):
                    raise ValueError(f"{instrument} contribution indices are not strictly increasing")
                if not isinstance(equity[index], list) or len(equity[index]) != 2 or equity[index][0] != timestamp:
                    raise ValueError(f"{instrument} contribution timestamp does not match shared equity")
                point = {"equity_index": index, "ts": timestamp, "pnl_contribution_raw": str(contribution)}
                if "pnl_contribution_exact" in item:
                    _exact_decimal(item["pnl_contribution_exact"], "exact contribution")
                    point["pnl_contribution_exact"] = item["pnl_contribution_exact"]
                curve.append(point)
                previous_index = index
            if curve[0]["equity_index"] != 0 or curve[-1]["equity_index"] != len(equity) - 1:
                raise ValueError(f"{instrument} contribution samples do not cover shared equity endpoints")
            contribution_curves[instrument] = curve
        sample_count = len(next(iter(contribution_curves.values())))
        if sample_count == 0 or any(len(curve) != sample_count for curve in contribution_curves.values()):
            raise ValueError("multi-market contribution curves do not share a sample axis")
        for sample_index in range(sample_count):
            axis = next(iter(contribution_curves.values()))[sample_index]
            total = 0
            for curve in contribution_curves.values():
                point = curve[sample_index]
                if point["equity_index"] != axis["equity_index"] or point["ts"] != axis["ts"]:
                    raise ValueError("multi-market contribution sample axes differ")
                total += int(point["pnl_contribution_raw"])
            shared = equity[axis["equity_index"]]
            if int(shared[1]) != initial_cash + total:
                raise ValueError("per-instrument contributions do not conserve shared account equity")
            if exact_equity_values is not None:
                with localcontext() as decimal_context:
                    decimal_context.prec = 100
                    exact_total = sum(
                        (Decimal(curve[sample_index]["pnl_contribution_exact"]) for curve in contribution_curves.values()),
                        Decimal(0),
                    )
                    initial_cash_exact = Decimal(initial_cash) / Decimal(1_000_000_000)
                    if exact_equity_values[axis["equity_index"]] != initial_cash_exact + exact_total:
                        raise ValueError("exact per-instrument contributions do not conserve shared account equity")

        event_fills = []
        for event in _sequence(artifact.get("event_log")):
            fill = _mapping(_mapping(event).get("kind")).get("Filled")
            if isinstance(fill, Mapping) and isinstance(fill.get("fill"), Mapping):
                event_fills.append(fill["fill"])
        if (
            len(fills) != run_outcome.get("successful_fills")
            or fills != event_fills
            or sum(int(value) for value in _mapping(artifact.get("per_market_fills")).values()) != len(fills)
        ):
            raise ValueError("multi-market fills sidecar does not match saved outcome and event fills")
        display_fills = _project_multi_market_fill_exact_values(fills)
        final_equity = int(equity[-1][1]) if isinstance(equity[-1], list) and len(equity[-1]) == 2 else None
        if final_equity != run_outcome.get("final_equity_raw"):
            raise ValueError("multi-market final equity does not match its summary")

        indices = _sample_indices(len(equity), 2_000)
        sampled_equity = []
        for index in indices:
            row = {"ts": int(equity[index][0]), "equity_raw": str(int(equity[index][1]))}
            if exact_equity_strings is not None:
                row["equity_exact"] = exact_equity_strings[index]
            sampled_equity.append(row)
        sampled_contributions = {
            instrument: [
                contribution_curves[instrument][sample_index]
                for sample_index, point in enumerate(contribution_curves[instrument])
                if point["equity_index"] in set(indices)
            ]
            for instrument in sorted(expected_instruments)
        }
        run_details = {
            "rank": ranks.get(outcome.get("run_id")),
            "ordinal": candidate.get("ordinal"),
            "run_id": run_outcome.get("run_id"),
            "parameters": candidate.get("parameters", {}),
            "account_id": run_outcome.get("account_id"),
            "settlement_currency": spec.get("settlement_currency"),
            "initial_cash_raw": str(initial_cash),
            "final_equity_raw": str(final_equity),
            "return_bps": run_outcome.get("return_bps"),
            "max_drawdown_bps": run_outcome.get("max_drawdown_bps"),
            "successful_fills": run_outcome.get("successful_fills"),
            "markets": verified_markets,
            "market_equity_contributions": sampled_contributions,
            "equity": sampled_equity,
            "fills": display_fills,
            "fees": [
                {"order_id": fill.get("order_id"), "fee": fill.get("fee"), "fee_currency": fill.get("fee_currency"), "fees": fill.get("fees"), "exact_fees": fill.get("exact_fees")}
                for fill in display_fills
            ],
            "contribution_points_checked": sample_count,
            "shared_equity_points": len(equity),
        }
        details.append(run_details)
        embedded_bytes += len(_inline_json(run_details).encode("utf-8"))
        protected_paths = (run_path, run_manifest_path, equity_path, fills_path, contribution_path)
        if exact_equity_path is not None:
            protected_paths += (exact_equity_path,)
        protected.update(str(path) for path in protected_paths)
        if embedded_bytes > MAX_DETAIL_PAYLOAD_BYTES:
            raise ValueError("multi-market candidate details exceed the 8 MiB embedded report limit")

    return {
        "schema_version": 1,
        "kind": "multi_market_bar",
        "runs": details,
        "omitted_verified_runs": max(0, len(eligible) - len(selected)),
        "candidates_without_artifact_details": max(0, len(candidates) - len(eligible)),
        "embedded_artifact_bytes": embedded_bytes,
        "_protected_paths": sorted(protected),
        "replay_note": "replay_verified was produced by the Rust backtest; this report rechecks BarFrame, RunManifest input components, sidecars and contribution conservation, but does not recompute the composed market fingerprint or rerun ReplayVerifier. Use qx-cli report for Rust fingerprint and event-ledger replay verification.",
    }


def _section(title: str, content: str, *, hint: str = "") -> str:
    return (
        f'<section><h2>{_escape(title)}</h2>'
        + (f'<p class="hint">{_escape(hint)}</p>' if hint else "")
        + content
        + "</section>"
    )


def _run_details_section(details: Mapping[str, Any]) -> str:
    runs = [row for row in _sequence(details.get("runs")) if isinstance(row, Mapping)]
    is_multi_market = details.get("kind") == "multi_market_bar"
    if not runs:
        if details.get("unsupported_scope"):
            return _section("运行详情、净值与成交费用", f'<p class="hint">{_escape(details.get("unsupported_scope"))}</p>')
        return _section(
            "运行详情、净值与成交费用",
            '<p class="hint">实验结果没有可通过 RunRecord 身份与摘要校验的候选运行附件。本报告不会根据收益汇总伪造权益点、成交或费用明细。</p>',
        )
    omitted = int(details.get("omitted_verified_runs", 0) or 0)
    unavailable = int(details.get("candidates_without_artifact_details", 0) or 0)
    coverage = (
        f"已核验 {len(runs)} 个候选运行，嵌入 {details.get('embedded_artifact_bytes', 0)} 字节附件。"
        + (f"另有 {omitted} 个已核验候选超过报告曲线数量上限。" if omitted else "")
        + (f"{unavailable} 个候选没有运行附件。" if unavailable else "")
        + ("图表按已核验的共享账户权益与逐标的贡献 sidecar 绘制；不做跨账户或跨币种折算。 " if is_multi_market else "")
        + "净值图只呈现已落盘的权益数据，不计算新的收益或绩效指标；成交明细同时展示原始定点值和工件已有的精确十进制值。"
    )
    content_parts = [
        f'<p class="hint">{_escape(coverage)}</p>',
        '<div class="detail-grid">',
        '<label>成交明细运行<select id="detailRun"></select></label>',
        '<label>权益曲线对比（最多 5 条）<select id="equityRuns" multiple size="5"></select></label>',
    ]
    if is_multi_market:
        content_parts.append('<label>权益贡献标的<select id="marketSelect"></select></label>')
    content_parts.extend([
        '<button id="downloadFills" type="button">导出当前运行成交 CSV</button>',
        '</div><div id="runFacts" class="cards"></div>',
        '<canvas id="equityChart" width="1200" height="300" aria-label="已记录的定点权益曲线对比"></canvas>',
        '<p id="equityHover" class="hint">将鼠标移到净值曲线上，可查看精确时间戳和 equity_raw。</p>',
    ])
    if is_multi_market:
        content_parts.append(
            '<h3>按标的权益贡献</h3><canvas id="marketContributionChart" width="1200" height="300" aria-label="经过守恒校验的标的权益贡献"></canvas><p id="marketContributionHover" class="hint">将鼠标移到曲线上查看时间戳、raw 兼容值和精确金额。</p><div class="table-wrap"><table id="marketContributionTable"><thead><tr><th>时间戳</th><th>贡献 raw（1e-9）</th><th>精确贡献</th></tr></thead><tbody></tbody></table></div><div class="table-wrap"><table id="marketFillTable"><thead><tr><th>标的</th><th>策略</th><th>成交数</th><th>市场规则</th><th>费用模型</th></tr></thead><tbody></tbody></table></div>'
        )
    content_parts.append(
        '<div class="table-wrap"><table id="fillTable"><thead><tr><th>时间戳</th><th>订单 ID</th><th>数量 raw</th><th>数量 exact</th><th>价格 raw</th><th>价格 exact</th><th>费用 raw</th><th>精确费用</th><th>策略</th></tr></thead><tbody></tbody></table></div>'
    )
    content = "".join(content_parts)
    scope_hint = (
        "候选多市场产物通过 Rust BarFrame 校验、RunManifest 输入组件身份、成交/权益 sidecar、事件成交一致性和逐标的权益守恒校验。组合数据指纹与事件重放请用 qx-cli report 独立复核。"
        + str(details.get("replay_note", ""))
        if is_multi_market
        else "候选产物必须通过 RunRecord 摘要与运行身份核验后才会出现在本节。此校验证明本地文件之间保持一致，不等同于签名证据包的真实性验证。"
    )
    return _section("运行详情、净值与成交费用", content, hint=scope_hint)


def render_experiment_html(result: Mapping[str, Any]) -> str:
    """Render a self-contained interactive report from a versioned result mapping."""
    document = result
    rows = _candidate_rows(document)
    has_oos = bool(_mapping(document.get("out_of_sample")))
    out_of_sample = _mapping(document.get("out_of_sample"))
    search = _mapping(document.get("search"))
    comparisons = _mapping(document.get("comparison"))
    research_details = _mapping(document.get("research_details"))
    artifact_path = document.get("artifact_path")

    cards = [
        ("Experiment", document.get("experiment_id")),
        ("Candidates", document.get("total_candidates", len(_sequence(document.get("candidates"))))),
        ("Completed", document.get("completed_candidates", "—")),
        ("Succeeded", document.get("succeeded_candidates", "—")),
        ("Failed", document.get("failed_candidates", "—")),
        ("Strategy", document.get("target_strategy_id", document.get("strategy_id"))),
        ("Search", search.get("algorithm")),
        ("Artifact", artifact_path),
    ]
    overview = '<div class="cards">' + "".join(
        f'<div class="card"><span>{_escape(label)}</span><strong>{_escape(value)}</strong></div>'
        for label, value in cards
        if value is not None
    ) + "</div>"
    if not has_oos:
        overview += '<p class="warning">仅有样本内结果：当前实验没有样本外切分，排名不能作为策略有效性的独立证据。</p>'

    headers = (
        "rank",
        "run_id",
        "parameters",
        "return_bps",
        "max_drawdown_bps",
        "fills",
        "status",
        "error",
    )
    candidate_table = (
        '<div class="toolbar"><input id="candidateFilter" type="search" placeholder="筛选参数、Run ID 或状态">'
        '<button id="exportCandidates" type="button">导出候选 CSV</button></div>'
        '<canvas id="candidateChart" width="1200" height="280" aria-label="候选收益和最大回撤对比"></canvas>'
        '<div class="table-wrap"><table id="candidateTable"><thead><tr>'
        + "".join(
            f'<th data-key="{key}">{_escape(label)}</th>'
            for key, label in zip(
                headers,
                ("排名", "Run ID", "参数", "收益 bps", "最大回撤 bps", "成交数", "状态", "失败原因"),
            )
        )
        + '</tr></thead><tbody>'
        + "".join(
            f'<tr data-index="{index}">'
            + "".join(
                f'<td data-value="{_escape(row.get(key))}">{_escape(row.get(key))}</td>'
                for key in headers
            )
            + "</tr>"
            for index, row in enumerate(rows)
        )
        + "</tbody></table></div>"
    )
    sections = [_section("实验概览", overview)]
    sections.append(
        _section(
            "候选排名",
            candidate_table,
            hint="收益、回撤、成交数和参数均直接取自实验结果；点击表头排序，可筛选并导出当前候选表。",
        )
    )
    sections.append(_run_details_section(research_details))
    sections.append(
        _section(
            "报告范围",
            '<p class="hint">按候选运行摘要显示账户、单一标的、费用模型和撮合模型；报告没有从组合汇总推导跨市场或跨账户损益贡献。当前交互曲线覆盖排名靠前且通过完整附件校验的候选，超过上限的候选会单独计数。</p>',
        )
    )

    fold_rows = [row for row in _sequence(out_of_sample.get("folds")) if isinstance(row, Mapping)]
    if fold_rows:
        fold_table = (
            '<div class="table-wrap"><table><thead><tr><th>Fold</th><th>训练区间</th><th>测试区间</th><th>选择参数</th><th>训练收益 bps</th><th>样本外收益 bps</th></tr></thead><tbody>'
            + "".join(
                "<tr>"
                + "".join(
                    f"<td>{_escape(value)}</td>"
                    for value in (
                        fold.get("fold"),
                        fold.get("train_range"),
                        fold.get("test_range"),
                        fold.get("selected_parameters", {}),
                        fold.get("in_sample_return_bps"),
                        fold.get("out_of_sample_return_bps"),
                    )
                )
                + "</tr>"
                for fold in fold_rows
            )
            + "</tbody></table></div>"
        )
        oos_summary = {
            key: out_of_sample.get(key)
            for key in (
                "in_sample_return_bps",
                "out_of_sample_return_bps",
                "degradation_bps",
                "cross_fold_robustness",
            )
            if out_of_sample.get(key) is not None
        }
        sections.append(_section("样本外折叠", f'<pre>{_escape(oos_summary)}</pre>{fold_table}'))

    fold_searches = [row for row in _sequence(search.get("fold_searches")) if isinstance(row, Mapping)]
    if fold_searches:
        trial_rows = []
        for fold in fold_searches:
            for trial in _sequence(fold.get("trials")):
                if isinstance(trial, Mapping):
                    trial_rows.append({"fold": fold.get("fold"), **trial})
        trial_table = (
            '<div class="table-wrap"><table><thead><tr><th>Fold</th><th>Trial</th><th>提案类型</th><th>参数</th><th>训练分数 bps</th><th>训练最大回撤 bps</th></tr></thead><tbody>'
            + "".join(
                "<tr>"
                + "".join(
                    f"<td>{_escape(value)}</td>"
                    for value in (
                        trial.get("fold"),
                        trial.get("ordinal"),
                        trial.get("proposal"),
                        trial.get("parameters", {}),
                        trial.get("training_score_bps"),
                        trial.get("training_max_drawdown_bps"),
                    )
                )
                + "</tr>"
                for trial in trial_rows
            )
            + "</tbody></table></div>"
        )
        sections.append(
            _section(
                "自适应搜索轨迹",
                f'<pre>{_escape({key: search.get(key) for key in ("algorithm", "objective", "seed", "budget", "fold_local_search") if search.get(key) is not None})}</pre>{trial_table}',
                hint="搜索提案和训练得分只展示已保存的训练结果。",
            )
        )

    for title, key, value in (
        ("基准策略", "baselines", document.get("baselines")),
        ("成本敏感性", "cost_sensitivity", document.get("cost_sensitivity")),
        ("参数邻域", "neighborhood", search.get("neighborhood")),
        ("数据质量", "dataset_quality", document.get("dataset_quality")),
        ("策略偏差与预热诊断", "diagnostics", document.get("diagnostics")),
    ):
        if value is not None and value != []:
            if key == "diagnostics":
                diagnostics = _mapping(value)
                lookahead = _mapping(diagnostics.get("lookahead"))
                startup = _mapping(diagnostics.get("startup"))
                trajectory = _mapping(startup.get("trajectory"))
                visibility = _mapping(diagnostics.get("data_visibility"))
                diagnostic_cards = [
                    ("未来数据", lookahead.get("status", "unavailable")),
                    ("扰动检查点", lookahead.get("decision_points_checked", 0)),
                    ("变化决策", lookahead.get("flagged_checks", 0)),
                    ("预热状态", startup.get("status", "unavailable")),
                    ("预热轨迹点", trajectory.get("decision_points_checked", 0)),
                    ("PIT 可见性", visibility.get("status", "not_provided")),
                ]
                diagnostic_summary = '<div class="cards">' + "".join(
                    f'<div class="card"><span>{_escape(label)}</span><strong>{_escape(item)}</strong></div>'
                    for label, item in diagnostic_cards
                ) + "</div>"
                diagnostic_details = (
                    '<details><summary>查看完整诊断明细</summary>'
                    f'<pre>{_escape(value)}</pre></details>'
                )
                sections.append(
                    _section(
                        title,
                        '<p class="hint">报告生成时会核对诊断文件中的策略源码摘要、数据指纹和 BacktestSpec 摘要字段；诊断附件仍是本地 JSON，报告不证明它未被手工修改。若状态为 no_change_detected，只代表已覆盖扰动未改变输出，不证明所有未来数据路径不存在。</p>'
                        + diagnostic_summary
                        + diagnostic_details,
                    )
                )
                continue
            sections.append(
                _section(
                    title,
                    f"<pre>{_escape(value)}</pre>",
                )
            )
        elif key == "diagnostics":
            sections.append(
                _section(
                    title,
                    '<p class="hint">此实验产物未包含未来数据、预热或前视偏差诊断；本报告不会从汇总指标推断这些结论。</p>',
                )
            )

    candidate_diagnostics = document.get("candidate_diagnostics")
    if isinstance(candidate_diagnostics, Mapping):
        diagnostic_items = [
            item
            for item in _sequence(candidate_diagnostics.get("items"))
            if isinstance(item, Mapping)
        ]
        rows = []
        for item in diagnostic_items:
            report = _mapping(item.get("diagnostics"))
            lookahead = _mapping(report.get("lookahead"))
            startup = _mapping(report.get("startup"))
            rows.append(
                "<tr>"
                + "".join(
                    f"<td>{_escape(value)}</td>"
                    for value in (
                        item.get("scope"),
                        item.get("fold"),
                        item.get("ordinal"),
                        item.get("strategy_kind"),
                        item.get("parameters", {}),
                        item.get("status"),
                        lookahead.get("status", "unavailable"),
                        lookahead.get("flagged_checks", "—"),
                        startup.get("status", "unavailable"),
                    )
                )
                + "</tr>"
            )
        table = (
            '<div class="table-wrap"><table><thead><tr>'
            "<th>候选范围</th><th>折</th><th>序号</th><th>策略类型</th>"
            "<th>参数</th><th>诊断状态</th><th>未来数据</th><th>变化检查</th><th>预热</th>"
            "</tr></thead><tbody>"
            + "".join(rows)
            + "</tbody></table></div>"
        )
        summary = "<div class=\"cards\">" + "".join(
            f'<div class="card"><span>{_escape(label)}</span><strong>{_escape(candidate_diagnostics.get(key))}</strong></div>'
            for label, key in (
                ("覆盖状态", "coverage_status"),
                ("候选数", "candidate_count"),
                ("已诊断", "diagnosed_count"),
                ("诊断失败", "diagnostic_failure_count"),
                ("未运行", "not_run_candidate_count"),
            )
        ) + "</div>"
        sections.append(
            _section(
                "逐候选未来数据与预热诊断",
                summary
                + '<p class="hint">诊断使用完整声明输入，对每个已完成候选的实际参数独立重建策略；它不改变候选排名。未运行、裁剪和失败候选会保留明确状态。未发现扰动影响只表示本次采样未检测到变化，不证明不存在所有未来数据路径。</p>'
                + table
                + '<details><summary>查看候选诊断完整明细</summary>'
                + f'<pre>{_escape(candidate_diagnostics)}</pre></details>',
            )
        )
    else:
        sections.append(
            _section(
                "逐候选未来数据与预热诊断",
                '<p class="hint">此实验未运行候选级诊断；本报告不会从候选指标推断未来数据安全或预热稳定。</p>',
            )
        )

    provenance = {
        key: document.get(key)
        for key in ("schema_version", "spec_fingerprint", "strategy_bundle_sha256", "strategy_source_sha256", "artifact_path")
        if document.get(key) is not None
    }
    if provenance:
        sections.append(_section("来源与复现信息", f'<pre>{_escape(provenance)}</pre>'))
    if comparisons:
        sections.append(
            _section(
                "指标来源",
                '<p class="hint">候选排名字段来自原始 comparison / ranking / candidate outcome。此 HTML 仅展示已保存数据，不重新计算收益、回撤或显著性。</p>',
            )
        )

    payload = _inline_json(rows)
    detail_payload = _inline_json(
        {key: value for key, value in research_details.items() if not key.startswith("_")}
    )
    page_title = f"牵星策略研究报告 — {_text(document.get('experiment_id') or 'experiment')}"
    return f"""<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{_escape(page_title)}</title><style>
:root{{color-scheme:light;--ink:#17212b;--muted:#61717e;--line:#dbe3e8;--panel:#fff;--bg:#f3f6f8;--accent:#176b87;--warn:#fff4d6}}
*{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--ink);font:14px/1.5 system-ui,"Microsoft YaHei",sans-serif}}
main{{max-width:1440px;margin:0 auto;padding:28px}}h1{{font-size:25px;margin:0 0 22px}}h2{{font-size:18px;margin:0 0 12px}}section{{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:20px;margin:16px 0;box-shadow:0 1px 3px #14202a0a}}
.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px}}.card{{padding:12px;background:#f7fafb;border:1px solid var(--line);border-radius:8px;min-width:0}}.card span{{display:block;color:var(--muted);font-size:12px}}.card strong{{display:block;overflow-wrap:anywhere;margin-top:4px;font-size:14px}}
.warning{{background:var(--warn);border-left:4px solid #d69e00;padding:10px 12px;margin:16px 0 0}}.hint{{color:var(--muted);margin:0 0 12px}}pre{{white-space:pre-wrap;overflow-wrap:anywhere;background:#f7fafb;padding:12px;border-radius:7px;max-height:360px;overflow:auto}}canvas{{width:100%;height:auto;margin:10px 0 4px;border:1px solid var(--line);border-radius:7px;background:#fff}}
.toolbar{{display:flex;gap:8px;margin:12px 0}}input,button,select{{font:inherit;padding:8px 10px;border:1px solid #bdcbd3;border-radius:6px;background:#fff}}input{{min-width:260px;flex:1}}button{{cursor:pointer;color:#fff;background:var(--accent);border-color:var(--accent)}}select{{min-width:220px}}.detail-grid{{display:grid;grid-template-columns:2fr 2fr auto;align-items:end;gap:10px;margin:12px 0}}.detail-grid label{{display:grid;gap:5px;color:var(--muted)}}.detail-grid select[multiple]{{height:112px}}.table-wrap{{overflow:auto}}table{{width:100%;border-collapse:collapse;min-width:760px}}th,td{{text-align:left;padding:9px 10px;border-bottom:1px solid var(--line);vertical-align:top;white-space:pre-wrap;overflow-wrap:anywhere}}th{{background:#f5f8f9;position:sticky;top:0;cursor:pointer}}tr:hover td{{background:#f8fbfc}}footer{{color:var(--muted);font-size:12px;padding:8px 2px}}
@media(max-width:700px){{main{{padding:14px}}section{{padding:14px}}.toolbar{{flex-direction:column}}input{{min-width:0}}.detail-grid{{grid-template-columns:1fr}}}}
</style></head><body><main><h1>{_escape(page_title)}</h1>{''.join(sections)}<footer>牵星离线研究报告 · 数据来自本次实验产物 · 无网络资源</footer></main>
<script type="application/json" id="candidateData">{payload}</script><script type="application/json" id="runDetailData">{detail_payload}</script><script>
const table=document.getElementById('candidateTable');
if(table){{
 const body=table.tBodies[0], input=document.getElementById('candidateFilter');
 const data=JSON.parse(document.getElementById('candidateData').textContent),chart=document.getElementById('candidateChart'),ctx=chart.getContext('2d');
 const visible=()=>[...body.rows].filter(row=>!row.hidden).map(row=>data[Number(row.dataset.index)]).filter(Boolean);
 const draw=()=>{{const rows=visible(),dpr=window.devicePixelRatio||1,w=chart.clientWidth,h=220;chart.width=w*dpr;chart.height=h*dpr;ctx.setTransform(dpr,0,0,dpr,0,0);ctx.clearRect(0,0,w,h);ctx.font='12px system-ui';ctx.fillStyle='#61717e';ctx.fillText('候选收益与最大回撤（bps）',12,18);const panels=[{{key:'return_bps',title:'收益',x:10,w:(w-30)/2,color:'#18815c'}},{{key:'max_drawdown_bps',title:'最大回撤',x:(w+10)/2,w:(w-30)/2,color:'#c65353'}}];for(const p of panels){{const vals=rows.map(r=>Number(r[p.key])).filter(Number.isFinite),max=Math.max(1,...vals.map(Math.abs)),top=40,bottom=188,base=p.key==='return_bps'?top+(bottom-top)/2:top;ctx.fillStyle='#61717e';ctx.fillText(p.title,p.x+6,34);ctx.strokeStyle='#dbe3e8';ctx.beginPath();ctx.moveTo(p.x+4,base);ctx.lineTo(p.x+p.w-4,base);ctx.stroke();const n=Math.max(1,rows.length),slot=(p.w-16)/n,bw=Math.max(2,Math.min(24,slot*.65));rows.forEach((r,i)=>{{const v=Number(r[p.key]);if(!Number.isFinite(v))return;const bh=Math.max(1,Math.abs(v)/max*(bottom-top)*.44),x=p.x+8+i*slot+(slot-bw)/2,y=p.key==='return_bps'?(v>=0?base-bh:base):(top+bh);ctx.fillStyle=p.color;ctx.fillRect(x,y,bw,bh);if(slot>34){{ctx.save();ctx.translate(x+bw/2,bottom+4);ctx.rotate(-.55);ctx.fillStyle='#61717e';ctx.fillText(String(r.run_id||r.ordinal||i+1).slice(-12),0,0);ctx.restore();}}}});}}}};
 const redraw=()=>draw();input.addEventListener('input',redraw);
 table.tHead.addEventListener('click',e=>{{const th=e.target.closest('th');if(!th)return;const idx=th.cellIndex,dir=th.dataset.dir==='asc'?'desc':'asc';th.dataset.dir=dir;const rows=[...body.rows].sort((a,b)=>{{const x=a.cells[idx].dataset.value||'',y=b.cells[idx].dataset.value||'',nx=Number(x),ny=Number(y);const cmp=Number.isFinite(nx)&&Number.isFinite(ny)?nx-ny:x.localeCompare(y,undefined,{{numeric:true}});return dir==='asc'?cmp:-cmp;}});for(const row of rows)body.appendChild(row);draw();}});
 document.getElementById('exportCandidates').addEventListener('click',()=>{{const keys=['rank','run_id','parameters','return_bps','max_drawdown_bps','fills','status','error'];const quote=v=>{{let s=String(v??'');if(typeof v==='string'&&/^[=+@\\-\\t\\r]/.test(s))s="'"+s;return '"'+s.replaceAll('"','""')+'"';}};const chosen=visible();const csv=[keys.join(','),...chosen.map(item=>keys.map(key=>quote(typeof item[key]==='object'?JSON.stringify(item[key]):item[key])).join(','))].join('\\r\\n');const blob=new Blob(['\\ufeff'+csv],{{type:'text/csv;charset=utf-8'}});const a=document.createElement('a');a.href=URL.createObjectURL(blob);a.download='qianxing-experiment-candidates.csv';a.click();URL.revokeObjectURL(a.href);}});
 draw();
}}
const detailData=JSON.parse(document.getElementById('runDetailData').textContent),detailRun=document.getElementById('detailRun'),equityRuns=document.getElementById('equityRuns');
if(detailRun&&equityRuns){{
 const runs=detailData.runs||[],facts=document.getElementById('runFacts'),fillBody=document.querySelector('#fillTable tbody'),chart=document.getElementById('equityChart'),ctx=chart.getContext('2d'),hover=document.getElementById('equityHover'),isMulti=detailData.kind==='multi_market_bar';
 const marketSelect=document.getElementById('marketSelect'),marketContributionChart=document.getElementById('marketContributionChart'),marketContributionHover=document.getElementById('marketContributionHover'),marketContributionBody=document.querySelector('#marketContributionTable tbody'),marketFillBody=document.querySelector('#marketFillTable tbody');
 const factLabels=isMulti?[['run_id','Run ID'],['account_id','共享账户'],['settlement_currency','结算币种'],['initial_cash_raw','初始资金 raw'],['final_equity_raw','期末权益 raw'],['return_bps','保存收益 bps'],['max_drawdown_bps','保存最大回撤 bps'],['successful_fills','成交数'],['shared_equity_points','共享权益点'],['contribution_points_checked','贡献守恒检查点']]:[['run_id','Run ID'],['strategy_id','策略'],['strategy_version','策略版本'],['instrument','标的'],['account_id','账户'],['currency','结算币种'],['initial_cash_raw','初始资金 raw'],['initial_cash_exact','初始资金 exact'],['final_equity_raw','期末权益 raw'],['fees_raw','总费用 raw'],['turnover_raw','成交额 raw'],['metrics_exact','精确权益、成交额与费用'],['fee_model','费用模型'],['fill_model','撮合模型']];
 runs.forEach((run,i)=>{{const label=`#${{run.rank||run.ordinal}} · ${{run.run_id}} · ${{JSON.stringify(run.parameters||{{}})}}`;const a=document.createElement('option');a.value=run.run_id;a.textContent=label;detailRun.appendChild(a);const b=a.cloneNode(true);b.selected=i<Math.min(3,runs.length);equityRuns.appendChild(b);}});
 const selectedRun=()=>runs.find(run=>run.run_id===detailRun.value);
 const showFacts=run=>{{facts.replaceChildren();if(!run)return;factLabels.forEach(([key,label])=>{{if(run[key]===undefined||run[key]===null)return;const card=document.createElement('div'),name=document.createElement('span'),value=document.createElement('strong');card.className='card';name.textContent=label;value.textContent=typeof run[key]==='object'?JSON.stringify(run[key]):String(run[key]);card.append(name,value);facts.appendChild(card);}});}};
 const stringify=value=>typeof value==='object'&&value!==null?JSON.stringify(value):String(value??'');
 const showFills=run=>{{fillBody.replaceChildren();if(!run)return;const items=run.fills||[],keys=isMulti?['ts','order_id','qty','qty_exact','price','price_exact','fee','fees','fee_currency','exact_fees_json','strategy_id','venue_id','account_id']:['ts','order_id','qty_raw','qty_exact','price_raw','price_exact','fee_raw','exact_fees_json','strategy_id'];const head=document.querySelector('#fillTable thead tr');head.replaceChildren();keys.forEach(key=>{{const th=document.createElement('th');th.textContent=key;head.appendChild(th);}});items.slice(0,500).forEach(fill=>{{const tr=document.createElement('tr');keys.forEach(key=>{{const td=document.createElement('td');td.textContent=stringify(fill[key]);tr.appendChild(td);}});fillBody.appendChild(tr);}});if(items.length>500)hover.textContent=`当前展示前 500 笔，点击 CSV 导出可取得全部 ${{items.length}} 笔成交。`;}};
 const drawEquity=()=>{{const chosen=[...equityRuns.selectedOptions].map(o=>runs.find(run=>run.run_id===o.value)).filter(Boolean).slice(0,5),dpr=window.devicePixelRatio||1,w=chart.clientWidth||900,h=280;chart.width=w*dpr;chart.height=h*dpr;ctx.setTransform(dpr,0,0,dpr,0,0);ctx.clearRect(0,0,w,h);if(!chosen.length){{ctx.fillStyle='#61717e';ctx.fillText('请选择 1 至 5 条已核验权益曲线',18,28);return;}}const series=chosen.map(run=>({{run,points:(run.equity||[]).map(p=>({{ts:String(p.ts),raw:String(p.equity_raw),exact:String(p.equity_exact??''),value:BigInt(p.equity_raw)}}))}})).filter(s=>s.points.length);if(!series.length)return;let min=series[0].points[0].value,max=min;series.forEach(s=>s.points.forEach(p=>{{if(p.value<min)min=p.value;if(p.value>max)max=p.value;}}));let range=max-min;if(range===0n)range=1n;const left=150,right=w-18,top=28,bottom=h-30,plotH=bottom-top;ctx.font='11px system-ui';ctx.fillStyle='#61717e';ctx.fillText(`equity_raw 最低 ${{min}}`,8,h-10);ctx.fillText(`最高 ${{max}}`,8,18);ctx.strokeStyle='#dbe3e8';ctx.beginPath();ctx.moveTo(left,top);ctx.lineTo(left,bottom);ctx.lineTo(right,bottom);ctx.stroke();const colors=['#176b87','#c65353','#18815c','#8959a8','#c17c16'];series.forEach((s,si)=>{{ctx.beginPath();ctx.strokeStyle=colors[si%colors.length];ctx.lineWidth=2;const n=Math.max(1,s.points.length-1);s.points.forEach((p,i)=>{{const x=left+(right-left)*i/n,y=bottom-Number((p.value-min)*1000000n/range)/1000000*plotH;if(i===0)ctx.moveTo(x,y);else ctx.lineTo(x,y);}});ctx.stroke();ctx.fillStyle=colors[si%colors.length];ctx.fillText(`#${{s.run.rank||s.run.ordinal}} ${{s.run.run_id}}`,left+8,top+14+si*16);}});chart._series=series;chart._bounds={{left,right,top,bottom}};if(chosen.length===5&&equityRuns.selectedOptions.length>5)hover.textContent='曲线数量已限制为 5 条。';else hover.textContent='将鼠标移到净值曲线上，可查看时间戳、raw 兼容值和 exact 金额。';}};
 detailRun.addEventListener('change',()=>{{const run=selectedRun();showFacts(run);showFills(run);}});equityRuns.addEventListener('change',drawEquity);window.addEventListener('resize',drawEquity);
 chart.addEventListener('mousemove',e=>{{if(!chart._series||!chart._bounds)return;const rect=chart.getBoundingClientRect(),x=e.clientX-rect.left,{{left,right}}=chart._bounds;if(x<left||x>right)return;const found=[];chart._series.forEach(s=>{{const i=Math.max(0,Math.min(s.points.length-1,Math.round((x-left)/(right-left)*Math.max(1,s.points.length-1)))),p=s.points[i];found.push(`${{s.run.run_id}}: ts=${{p.ts}}, equity_raw=${{p.raw}}, equity_exact=${{p.exact||'未提供'}}`);}});chart.title=found.join(String.fromCharCode(10));hover.textContent=found.join(' · ');}});
 document.getElementById('downloadFills').addEventListener('click',()=>{{const run=selectedRun();if(!run)return;const keys=isMulti?['ts','order_id','qty','qty_exact','price','price_exact','fee','fees','fee_currency','exact_fees_json','strategy_id','venue_id','account_id']:['ts','order_id','qty_raw','qty_exact','price_raw','price_exact','fee_raw','exact_fees_json','strategy_id'],quote=value=>{{let s=stringify(value);if(/^[=+@-]/.test(s)||s.charCodeAt(0)===9||s.charCodeAt(0)===13)s="'"+s;return '"'+s.replaceAll('"','""')+'"';}},csv=[keys.join(','),...(run.fills||[]).map(fill=>keys.map(key=>quote(fill[key])).join(','))].join(String.fromCharCode(13,10)),blob=new Blob([String.fromCharCode(0xfeff)+csv],{{type:'text/csv;charset=utf-8'}}),a=document.createElement('a');a.href=URL.createObjectURL(blob);a.download=`${{run.run_id}}-fills.csv`;a.click();URL.revokeObjectURL(a.href);}});
 const showMarketDetails=run=>{{if(!isMulti||!run)return;const items=run.markets||[];marketSelect.replaceChildren();items.forEach(market=>{{const option=document.createElement('option');option.value=market.instrument;option.textContent=market.instrument;marketSelect.appendChild(option);}});marketFillBody.replaceChildren();items.forEach(market=>{{const tr=document.createElement('tr');[market.instrument,(market.strategies||[]).map(slot=>slot.strategy?.strategy_id||'').join(', '),market.fills,market.market_spec,market.fee_schedule].forEach(value=>{{const td=document.createElement('td');td.textContent=stringify(value);tr.appendChild(td);}});marketFillBody.appendChild(tr);}});drawContribution();}};
 const drawContribution=()=>{{if(!isMulti||!selectedRun()||!marketContributionChart)return;const run=selectedRun(),instrument=marketSelect.value,rows=(run.market_equity_contributions||{{}})[instrument]||[],dpr=window.devicePixelRatio||1,w=marketContributionChart.clientWidth||900,h=260;marketContributionChart.width=w*dpr;marketContributionChart.height=h*dpr;const c=marketContributionChart.getContext('2d');c.setTransform(dpr,0,0,dpr,0,0);c.clearRect(0,0,w,h);if(!rows.length){{marketContributionHover.textContent='该标的没有经守恒校验的贡献采样';return;}}const values=rows.map(row=>BigInt(row.pnl_contribution_raw));let min=values[0],max=values[0];values.forEach(value=>{{if(value<min)min=value;if(value>max)max=value;}});let range=max-min;if(range===0n)range=1n;const left=130,right=w-18,top=28,bottom=h-28;c.strokeStyle='#dbe3e8';c.beginPath();c.moveTo(left,top);c.lineTo(left,bottom);c.lineTo(right,bottom);c.stroke();c.fillStyle='#61717e';c.font='11px system-ui';c.fillText(`raw ${{min}}`,8,h-8);c.fillText(`raw ${{max}}`,8,18);c.strokeStyle='#18815c';c.lineWidth=2;c.beginPath();rows.forEach((row,i)=>{{const x=left+(right-left)*i/Math.max(1,rows.length-1),y=bottom-Number((values[i]-min)*1000000n/range)/1000000*(bottom-top);if(i===0)c.moveTo(x,y);else c.lineTo(x,y);}});c.stroke();marketContributionChart._rows=rows;marketContributionChart._bounds={{left,right}};marketContributionHover.textContent=`${{instrument}} · ${{rows.length}} 个已验证贡献采样；原始值与共享权益逐点守恒。`;}};
 if(isMulti){{marketSelect.addEventListener('change',drawContribution);detailRun.addEventListener('change',()=>showMarketDetails(selectedRun()));marketContributionChart.addEventListener('mousemove',e=>{{if(!marketContributionChart._rows||!marketContributionChart._bounds)return;const rect=marketContributionChart.getBoundingClientRect(),x=e.clientX-rect.left,{{left,right}}=marketContributionChart._bounds;if(x<left||x>right)return;const index=Math.max(0,Math.min(marketContributionChart._rows.length-1,Math.round((x-left)/(right-left)*Math.max(1,marketContributionChart._rows.length-1)))),row=marketContributionChart._rows[index];marketContributionChart.title=`ts=${{row.ts}}, pnl_contribution_raw=${{row.pnl_contribution_raw}}`;marketContributionHover.textContent=`${{marketSelect.value}} · ts=${{row.ts}} · pnl_contribution_raw=${{row.pnl_contribution_raw}}`;}});}}
 if(isMulti){{const renderContributionTable=()=>{{const run=selectedRun(),rows=(run?.market_equity_contributions||{{}})[marketSelect.value]||[];marketContributionBody.replaceChildren();rows.forEach(row=>{{const tr=document.createElement('tr');[row.ts,row.pnl_contribution_raw,row.pnl_contribution_exact??'旧报告未提供'].forEach(value=>{{const td=document.createElement('td');td.textContent=String(value);tr.appendChild(td);}});marketContributionBody.appendChild(tr);}});}};marketSelect.addEventListener('change',renderContributionTable);detailRun.addEventListener('change',()=>requestAnimationFrame(renderContributionTable));requestAnimationFrame(renderContributionTable);}}
 if(runs.length){{detailRun.value=runs[0].run_id;showFacts(runs[0]);showFills(runs[0]);drawEquity();if(isMulti)showMarketDetails(runs[0]);}}
}}
</script></body></html>"""


def default_report_path(result: Mapping[str, Any]) -> Path:
    artifact = result.get("artifact_path")
    if not artifact:
        raise ValueError("experiment result has no artifact_path; pass an explicit --html path")
    source = Path(str(artifact))
    return source.with_suffix(".report.html") if source.suffix else Path(f"{source}.report.html")


def write_experiment_html(
    result: Mapping[str, Any],
    output_path: str | os.PathLike[str] | None = None,
    *,
    protected_paths: Sequence[str | os.PathLike[str]] = (),
) -> Path:
    """Atomically write a report, rejecting any output that aliases its inputs/artifacts."""
    target = Path(output_path) if output_path is not None else default_report_path(result)
    resolved = target.resolve(strict=False)
    protected = [result.get("artifact_path"), *(str(path) for path in protected_paths)]
    protected.extend(
        _mapping(_mapping(row).get("outcome")).get("artifact_path")
        for row in _sequence(result.get("candidates"))
    )
    protected.extend(
        path
        for row in _sequence(result.get("candidates"))
        for path in _mapping(_mapping(_mapping(row).get("outcome")).get("artifacts")).values()
    )
    protected.extend(
        _sequence(_mapping(result.get("research_details")).get("_protected_paths"))
    )
    protected.extend(
        _mapping(_mapping(_mapping(row).get("run")).get("artifacts")).get("manifest_path")
        for row in _sequence(_mapping(result.get("comparison")).get("runs"))
    )
    target_key = os.path.normcase(str(resolved))
    for source in protected:
        if source:
            source_key = os.path.normcase(str(Path(str(source)).resolve(strict=False)))
            if source_key == target_key:
                raise ValueError(f"HTML report output would overwrite an input or experiment artifact: {source}")

    target.parent.mkdir(parents=True, exist_ok=True)
    content = render_experiment_html(result)
    temp_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", newline="\n", dir=target.parent, prefix=f".{target.name}.", suffix=".tmp", delete=False
        ) as handle:
            temp_path = Path(handle.name)
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp_path, target)
    finally:
        if temp_path is not None and temp_path.exists():
            temp_path.unlink()
    return target


__all__ = ["default_report_path", "render_experiment_html", "write_experiment_html"]
