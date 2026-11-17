#!/usr/bin/env python3
"""Build a conservative PIT ledger from timestamped BarFrame and action snapshots.

The supplied observation time means "first captured by this research process". It is
not treated as the exchange's publication timestamp. This intentionally favors visible
PIT warnings over claiming historical availability that the snapshots cannot prove.
Optional A-share corporate-action documents are captured with the same conservative
availability rule and linked to the BarFrame instrument for adjustment-chain checks.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import tempfile
from datetime import date, datetime, time, timedelta, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any, Sequence

MAX_SNAPSHOT_BYTES = 128 * 1024 * 1024
MAX_PIT_FACTS = 20_000
MAX_PIT_VERSIONS_PER_FACT = 100
PRICE_ADJUSTMENT_BASES = {"raw", "forward_adjusted", "backward_adjusted", "unknown"}
I128_MIN = -(1 << 127)
I128_MAX = (1 << 127) - 1
RAW_FIELDS = ("open_raw", "high_raw", "low_raw", "close_raw", "volume_raw")
STANDARD_FIELDS = ("open", "high", "low", "close", "volume")
INTEGER_TEXT = re.compile(r"^-?[0-9]+$")


class LedgerImportError(ValueError):
    """Provider input cannot be represented safely in the PIT ledger contract."""


def _timestamp(value: Any, label: str) -> int:
    if type(value) is not int or value <= 0:
        raise LedgerImportError(f"{label} must be a positive integer timestamp in milliseconds")
    return value


def _canonical_value(value: Any, field_name: str, snapshot: Path) -> str:
    if isinstance(value, bool) or value is None:
        raise LedgerImportError(f"{snapshot}: {field_name} contains a non-numeric value")
    raw_field = field_name.endswith("_raw")
    if isinstance(value, int):
        if raw_field and not I128_MIN <= value <= I128_MAX:
            raise LedgerImportError(f"{snapshot}: {field_name} is outside the signed i128 range")
        return str(value)
    if raw_field and isinstance(value, str):
        if not INTEGER_TEXT.fullmatch(value):
            raise LedgerImportError(f"{snapshot}: {field_name} raw values must be integers")
        negative = value.startswith("-")
        digits = value[1:] if negative else value
        canonical_integer = digits.lstrip("0") or "0"
        canonical = f"-{canonical_integer}" if negative and canonical_integer != "0" else canonical_integer
        if not I128_MIN <= int(canonical) <= I128_MAX:
            raise LedgerImportError(f"{snapshot}: {field_name} is outside the signed i128 range")
        return canonical
    try:
        decimal = value if isinstance(value, Decimal) else Decimal(str(value))
    except (InvalidOperation, ValueError):
        raise LedgerImportError(f"{snapshot}: {field_name} contains a non-numeric value") from None
    if not decimal.is_finite():
        raise LedgerImportError(f"{snapshot}: {field_name} contains a non-finite value")
    if raw_field and decimal != decimal.to_integral_value():
        raise LedgerImportError(f"{snapshot}: {field_name} raw values must be integers")
    canonical = format(decimal, "f")
    if "." in canonical:
        canonical = canonical.rstrip("0").rstrip(".")
    if raw_field and not I128_MIN <= int(canonical) <= I128_MAX:
        raise LedgerImportError(f"{snapshot}: {field_name} is outside the signed i128 range")
    return "0" if canonical in {"", "-0"} else canonical


def _read_snapshot(path: Path) -> tuple[str, str, list[int], dict[str, list[str]], str]:
    try:
        resolved = path.resolve(strict=True)
        size = resolved.stat().st_size
    except OSError as error:
        raise LedgerImportError(f"cannot read provider snapshot {path}: {error}") from error
    if size > MAX_SNAPSHOT_BYTES:
        raise LedgerImportError(f"provider snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes: {path}")
    raw = resolved.read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    try:
        payload = json.loads(raw.decode("utf-8"), parse_float=Decimal)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise LedgerImportError(f"provider snapshot is not valid UTF-8 JSON: {path}: {error}") from error
    if not isinstance(payload, dict):
        raise LedgerImportError(f"provider snapshot must be a JSON object: {path}")
    if type(payload.get("schema_version")) is not int or payload["schema_version"] != 1:
        raise LedgerImportError(f"provider snapshot must use BarFrame schema_version 1: {path}")
    instrument = payload.get("instrument")
    source = payload.get("source")
    timestamps = payload.get("ts")
    if not isinstance(instrument, str) or not instrument.strip():
        raise LedgerImportError(f"provider snapshot is missing instrument identity: {path}")
    if not isinstance(source, str) or not source.strip():
        raise LedgerImportError(f"provider snapshot is missing source identity: {path}")
    if not isinstance(timestamps, list) or not timestamps:
        raise LedgerImportError(f"provider snapshot must contain a non-empty ts array: {path}")
    times = [_timestamp(value, f"{path}: ts[{index}]") for index, value in enumerate(timestamps)]
    if any(right <= left for left, right in zip(times, times[1:])):
        raise LedgerImportError(f"provider snapshot timestamps must be strictly increasing: {path}")

    if all(name in payload for name in RAW_FIELDS):
        field_names = RAW_FIELDS
    elif all(name in payload for name in STANDARD_FIELDS):
        field_names = STANDARD_FIELDS
    else:
        raise LedgerImportError(
            f"provider snapshot must contain all raw OHLCV fields {RAW_FIELDS} "
            f"or standard OHLCV fields {STANDARD_FIELDS}: {path}"
        )
    fields: dict[str, list[str]] = {}
    for field_name in field_names:
        values = payload[field_name]
        if not isinstance(values, list) or len(values) != len(times):
            raise LedgerImportError(f"{path}: {field_name} length must match ts")
        fields[field_name] = [
            _canonical_value(value, field_name, path) for value in values
        ]
    return instrument, source, times, fields, digest


def _action_effective_at(ex_date: str, snapshot: Path) -> int:
    try:
        parsed = date.fromisoformat(ex_date)
    except (TypeError, ValueError):
        raise LedgerImportError(f"{snapshot}: corporate action ex_date must be YYYY-MM-DD") from None
    if parsed.isoformat() != ex_date:
        raise LedgerImportError(f"{snapshot}: corporate action ex_date must be YYYY-MM-DD")
    # A-share corporate actions become effective on their Shanghai calendar date.
    local_midnight = datetime.combine(
        parsed, time.min, tzinfo=timezone(timedelta(hours=8))
    )
    delta = local_midnight.astimezone(timezone.utc) - datetime(1970, 1, 1, tzinfo=timezone.utc)
    return delta.days * 86_400_000 + delta.seconds * 1_000 + delta.microseconds // 1_000


def _read_corporate_action_snapshot(
    path: Path,
) -> tuple[str, str, list[tuple[tuple[str, ...], int, str, str]], str]:
    try:
        resolved = path.resolve(strict=True)
        size = resolved.stat().st_size
    except OSError as error:
        raise LedgerImportError(f"cannot read corporate-action snapshot {path}: {error}") from error
    if size > MAX_SNAPSHOT_BYTES:
        raise LedgerImportError(f"corporate-action snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes: {path}")
    raw = resolved.read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    try:
        payload = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise LedgerImportError(
            f"corporate-action snapshot is not valid UTF-8 JSON: {path}: {error}"
        ) from error

    if isinstance(payload, dict):
        allowed = {"schema_version", "source", "instrument", "as_of", "actions"}
        unknown = sorted(str(key) for key in payload if key not in allowed)
        if unknown:
            raise LedgerImportError(
                f"{path}: corporate-action document has unknown key(s): {', '.join(unknown)}"
            )
        if type(payload.get("schema_version")) is not int or payload["schema_version"] != 1:
            raise LedgerImportError(f"{path}: corporate-action envelope must use schema_version 1")
        source = payload.get("source")
        instrument = payload.get("instrument")
        actions = payload.get("actions")
        if not isinstance(source, str) or not source.strip():
            raise LedgerImportError(f"{path}: corporate-action source must be non-empty")
        if not isinstance(instrument, str) or not instrument.strip():
            raise LedgerImportError(f"{path}: corporate-action instrument must be non-empty")
    elif isinstance(payload, list):
        # Legacy v0 action arrays remain readable when each row carries a single identity.
        actions = payload
        sources = {
            item.get("source") for item in actions
            if isinstance(item, dict) and isinstance(item.get("source"), str) and item["source"].strip()
        }
        instruments = {
            item.get("instrument") for item in actions
            if isinstance(item, dict) and isinstance(item.get("instrument"), str) and item["instrument"].strip()
        }
        if len(sources) != 1 or len(instruments) != 1:
            raise LedgerImportError(
                f"{path}: legacy corporate-action array must have one non-empty source and instrument"
            )
        source = next(iter(sources))
        instrument = next(iter(instruments))
    else:
        raise LedgerImportError(f"{path}: corporate-action snapshot must be a v1 object or legacy array")

    if not isinstance(actions, list):
        raise LedgerImportError(f"{path}: corporate-action actions must be an array")
    normalized: list[tuple[tuple[str, ...], int, str, str]] = []
    seen_keys: set[tuple[str, ...]] = set()
    for index, action in enumerate(actions):
        if not isinstance(action, dict):
            raise LedgerImportError(f"{path}: actions[{index}] must be an object")
        action_instrument = action.get("instrument", instrument)
        action_source = action.get("source") or source
        action_type = action.get("action_type")
        ex_date = action.get("ex_date")
        published_at = action.get("published_at")
        if action_instrument != instrument:
            raise LedgerImportError(f"{path}: actions[{index}] instrument differs from document identity")
        if action_source != source:
            raise LedgerImportError(f"{path}: actions[{index}] source differs from document identity")
        if not isinstance(action_type, str) or not re.fullmatch(r"[a-z0-9_]+", action_type):
            raise LedgerImportError(f"{path}: actions[{index}].action_type must be a lowercase identifier")
        if not isinstance(published_at, str) or not published_at.strip():
            raise LedgerImportError(f"{path}: actions[{index}].published_at must be non-empty")
        try:
            published = datetime.fromisoformat(published_at.replace("Z", "+00:00"))
        except ValueError:
            raise LedgerImportError(f"{path}: actions[{index}].published_at must be ISO-8601") from None
        if published.tzinfo is None:
            raise LedgerImportError(f"{path}: actions[{index}].published_at must include a timezone")
        effective_at = _action_effective_at(ex_date, path)
        action_id = action.get("action_id")
        if action_id is not None:
            if not isinstance(action_id, str) or not action_id.strip():
                raise LedgerImportError(f"{path}: actions[{index}].action_id must be non-empty when present")
            # A stable source ID must remain one fact across captures. The PIT v1
            # fact contract cannot version effective_at/field metadata, so the
            # caller must fail closed if a provider later changes either value.
            identity = (instrument, source, "action_id", action_id)
        else:
            record_date = action.get("record_date")
            if record_date is not None and not isinstance(record_date, str):
                raise LedgerImportError(f"{path}: actions[{index}].record_date must be a string or null")
            identity = (instrument, source, ex_date, action_type, "record_date", record_date or "")
        if identity in seen_keys:
            raise LedgerImportError(
                f"{path}: actions[{index}] duplicates an action identity; provide a stable action_id"
            )
        seen_keys.add(identity)
        canonical_action = dict(action)
        canonical_action["instrument"] = instrument
        canonical_action["source"] = source
        value_fingerprint = hashlib.sha256(
            json.dumps(
                canonical_action, sort_keys=True, separators=(",", ":"), ensure_ascii=False
            ).encode("utf-8")
        ).hexdigest()
        normalized.append((identity, effective_at, f"corporate_action.{action_type}", value_fingerprint))
    return instrument, source, normalized, digest


def build_provider_revision_ledger(
    snapshots: Sequence[Path], observed_at_ms: Sequence[int],
    price_adjustment_basis: str = "unknown",
    action_snapshots: Sequence[Path] = (),
    action_observed_at_ms: Sequence[int] = (),
) -> dict[str, Any]:
    if not isinstance(price_adjustment_basis, str) or price_adjustment_basis not in PRICE_ADJUSTMENT_BASES:
        raise LedgerImportError(
            "price adjustment basis must be raw, forward_adjusted, backward_adjusted, or unknown"
        )
    if not snapshots:
        raise LedgerImportError("at least one --snapshot is required")
    if len(snapshots) != len(observed_at_ms):
        raise LedgerImportError("each --snapshot must have exactly one matching --observed-at-ms")
    if len(action_snapshots) != len(action_observed_at_ms):
        raise LedgerImportError(
            "each --actions-snapshot must have exactly one matching --actions-observed-at-ms"
        )
    if len(snapshots) > MAX_PIT_VERSIONS_PER_FACT:
        raise LedgerImportError(
            f"at most {MAX_PIT_VERSIONS_PER_FACT} snapshots can contribute to one fact"
        )
    if len(action_snapshots) > MAX_PIT_VERSIONS_PER_FACT:
        raise LedgerImportError(
            f"at most {MAX_PIT_VERSIONS_PER_FACT} corporate-action snapshots can contribute to one fact"
        )
    observations = [
        _timestamp(value, f"observed_at_ms[{index}]")
        for index, value in enumerate(observed_at_ms)
    ]
    if any(right <= left for left, right in zip(observations, observations[1:])):
        raise LedgerImportError("snapshot observation times must be strictly increasing")
    action_observations = [
        _timestamp(value, f"action_observed_at_ms[{index}]")
        for index, value in enumerate(action_observed_at_ms)
    ]
    if any(right <= left for left, right in zip(action_observations, action_observations[1:])):
        raise LedgerImportError("corporate-action observation times must be strictly increasing")

    identity: tuple[str, str] | None = None
    expected_field_names: tuple[str, ...] | None = None
    fact_versions: dict[tuple[int, str], list[dict[str, Any]]] = {}
    field_fingerprints: dict[tuple[int, str], str] = {}
    for snapshot_path, observed_at in zip(snapshots, observations):
        instrument, source, timestamps, fields, snapshot_digest = _read_snapshot(snapshot_path)
        if timestamps[-1] > observed_at:
            raise LedgerImportError(
                f"snapshot capture time precedes its newest bar timestamp: {snapshot_path}"
            )
        current_identity = (instrument, source)
        if identity is None:
            identity = current_identity
        elif current_identity != identity:
            raise LedgerImportError(
                "all provider snapshots must have identical instrument and source identities"
            )
        current_field_names = tuple(fields)
        if expected_field_names is None:
            expected_field_names = current_field_names
        elif current_field_names != expected_field_names:
            raise LedgerImportError("all provider snapshots must contain the same OHLCV field set")
        revision = f"capture:{observed_at}:sha256:{snapshot_digest}"
        for bar_index, effective_at in enumerate(timestamps):
            for field_name, values in fields.items():
                fact_key = (effective_at, field_name)
                value_fingerprint = hashlib.sha256(
                    values[bar_index].encode("ascii")
                ).hexdigest()
                if field_fingerprints.get(fact_key) == value_fingerprint:
                    continue
                versions = fact_versions.setdefault(fact_key, [])
                if len(versions) >= MAX_PIT_VERSIONS_PER_FACT:
                    raise LedgerImportError(
                        f"fact at {effective_at} {field_name} exceeds the "
                        f"{MAX_PIT_VERSIONS_PER_FACT} version limit"
                    )
                versions.append(
                    {
                        "available_at": observed_at,
                        "revision": revision,
                        "value_fingerprint": value_fingerprint,
                    }
                )
                field_fingerprints[fact_key] = value_fingerprint
                if len(fact_versions) > MAX_PIT_FACTS:
                    raise LedgerImportError(
                        f"snapshot union exceeds the PIT ledger limit of {MAX_PIT_FACTS} facts"
                    )

    action_identity: tuple[str, str] | None = None
    action_versions_by_key: dict[tuple[str, ...], list[dict[str, Any]]] = {}
    action_metadata: dict[tuple[str, ...], tuple[int, str]] = {}
    action_fingerprints: dict[tuple[str, ...], str] = {}
    action_sources: set[str] = set()
    for snapshot_path, observed_at in zip(action_snapshots, action_observations):
        instrument, source, action_rows, snapshot_digest = _read_corporate_action_snapshot(snapshot_path)
        if identity is None or instrument != identity[0]:
            raise LedgerImportError(
                "corporate-action snapshots must match the instrument in the BarFrame snapshots"
            )
        current_identity = (instrument, source)
        if action_identity is None:
            action_identity = current_identity
        elif current_identity != action_identity:
            raise LedgerImportError(
                "all corporate-action snapshots must have identical instrument and source identities"
            )
        action_sources.add(source)
        revision = f"capture:{observed_at}:sha256:{snapshot_digest}"
        for key, effective_at, field_name, value_fingerprint in action_rows:
            metadata = (effective_at, field_name)
            if key in action_metadata and action_metadata[key] != metadata:
                raise LedgerImportError(
                    f"corporate action {key!r} changed ex_date or action_type; "
                    "PIT ledger v1 cannot version fact metadata safely"
                )
            if action_fingerprints.get(key) == value_fingerprint:
                continue
            versions = action_versions_by_key.setdefault(key, [])
            if len(versions) >= MAX_PIT_VERSIONS_PER_FACT:
                raise LedgerImportError(
                    f"corporate action {key!r} exceeds the {MAX_PIT_VERSIONS_PER_FACT} version limit"
                )
            versions.append({
                "available_at": observed_at,
                "revision": revision,
                "value_fingerprint": value_fingerprint,
            })
            action_fingerprints[key] = value_fingerprint
            action_metadata[key] = metadata
            if len(fact_versions) + len(action_versions_by_key) > MAX_PIT_FACTS:
                raise LedgerImportError(
                    f"snapshot union exceeds the PIT ledger limit of {MAX_PIT_FACTS} facts"
                )

    assert identity is not None
    instrument, provider_source = identity
    facts = []
    for (effective_at, field_name), versions in sorted(fact_versions.items()):
        fact_id = hashlib.sha256(
            f"{provider_source}\0{instrument}\0{effective_at}\0{field_name}".encode("utf-8")
        ).hexdigest()
        facts.append(
            {
                "fact_id": f"ohlcv:{fact_id}",
                "entity_id": instrument,
                "field": f"bar.{field_name}",
                "effective_at": effective_at,
                "versions": versions,
            }
        )
    for key, versions in sorted(action_versions_by_key.items()):
        effective_at, field_name = action_metadata[key]
        fact_id = hashlib.sha256(
            json.dumps(key, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        ).hexdigest()
        facts.append({
            "fact_id": f"corporate-action:{fact_id}",
            "entity_id": key[0],
            "field": field_name,
            "effective_at": effective_at,
            "versions": versions,
        })
    if not facts:
        raise LedgerImportError("provider snapshots did not yield any OHLCV facts")
    return {
        "schema_version": 1,
        "kind": "qianxing-pit-ledger",
        "price_adjustment_basis": price_adjustment_basis,
        "source": (
            f"bars={provider_source}; actions={','.join(sorted(action_sources)) or 'none'}; "
            "availability=first-captured-by-qianxing; capture time is not the exchange "
            "publication timestamp; action published_at is retained as source data, not "
            "treated as independently verified availability"
        ),
        "facts": facts,
    }


def _write_output(path: Path, ledger: dict[str, Any], force: bool) -> None:
    target = path.resolve()
    if target.exists() and not force:
        raise LedgerImportError(f"output already exists; pass --force to replace it: {target}")
    target.parent.mkdir(parents=True, exist_ok=True)
    serialized = json.dumps(ledger, ensure_ascii=False, sort_keys=True, indent=2) + "\n"
    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w", encoding="utf-8", newline="\n", dir=target.parent,
            prefix=f".{target.name}.", suffix=".tmp", delete=False,
        ) as temporary:
            temporary.write(serialized)
            temporary.flush()
            os.fsync(temporary.fileno())
            temporary_path = Path(temporary.name)
        if force:
            os.replace(temporary_path, target)
        else:
            try:
                # Publishing via a same-directory hard link is atomic and refuses to
                # replace a path that another process created after the initial check.
                os.link(temporary_path, target)
            except FileExistsError:
                raise LedgerImportError(
                    f"output already exists; pass --force to replace it: {target}"
                ) from None
            except OSError as error:
                raise LedgerImportError(
                    f"cannot atomically publish output without overwrite: {target}: {error}"
                ) from error
    finally:
        if temporary_path is not None and temporary_path.exists():
            temporary_path.unlink()


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--snapshot", type=Path, action="append", required=True,
        help="provider BarFrame JSON; repeat in capture order",
    )
    parser.add_argument(
        "--observed-at-ms", type=int, action="append", required=True,
        help="UTC Unix millisecond capture time, paired by order with --snapshot",
    )
    parser.add_argument(
        "--actions-snapshot", type=Path, action="append", default=[],
        help="A-share corporate-action document JSON; repeat in capture order",
    )
    parser.add_argument(
        "--actions-observed-at-ms", type=int, action="append", default=[],
        help="UTC Unix millisecond capture time paired by order with --actions-snapshot",
    )
    parser.add_argument("--output", type=Path, required=True, help="new PIT ledger JSON path")
    parser.add_argument(
        "--price-adjustment-basis", choices=sorted(PRICE_ADJUSTMENT_BASES), default="unknown",
        help="OHLC basis in provider snapshots; declare only when verified from provider metadata",
    )
    parser.add_argument("--force", action="store_true", help="replace an existing output file")
    args = parser.parse_args(argv)
    try:
        ledger = build_provider_revision_ledger(
            args.snapshot,
            args.observed_at_ms,
            args.price_adjustment_basis,
            args.actions_snapshot,
            args.actions_observed_at_ms,
        )
        output_resolved = args.output.resolve()
        input_paths = {
            path.resolve(strict=True) for path in [*args.snapshot, *args.actions_snapshot]
        }
        if output_resolved in input_paths:
            raise LedgerImportError("output path must not replace a provider snapshot")
        _write_output(args.output, ledger, args.force)
    except (LedgerImportError, OSError) as error:
        print(f"provider PIT ledger import failed: {error}", file=sys.stderr)
        return 2
    print(
        f"provider PIT ledger written: {args.output} "
        f"({len(ledger['facts'])} facts; source={ledger['source']})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
