#!/usr/bin/env python3
"""Generate the capability verification register from maturity/capabilities.yaml.

This deliberately parses only the small, stable YAML subset used for the four
verification fields and limitations. Unknown shapes fail closed so the report
cannot silently drift away from its source of truth.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "maturity" / "capabilities.yaml"
OUTPUT = ROOT / "maturity" / "unverified.json"
STATUS_FIELDS = (
    "implementation",
    "code_tested",
    "sandbox_tested",
    "production_approved",
)
CAPABILITY_KEY = re.compile(r"^  ([a-z][a-z0-9_]*):$")
STATUS_LINE = re.compile(
    r"^    (implementation|code_tested|sandbox_tested|production_approved): (.+)$"
)


def scalar(raw: str, *, where: str) -> Any:
    value = raw.strip()
    if value == "true":
        return True
    if value == "false":
        return False
    if value.startswith("'"):
        if len(value) < 2 or not value.endswith("'"):
            raise ValueError(f"{where}: unterminated single-quoted YAML scalar")
        return value[1:-1].replace("''", "'")
    if value.startswith('"'):
        try:
            decoded = json.loads(value)
        except json.JSONDecodeError as error:
            raise ValueError(f"{where}: unsupported double-quoted scalar: {error}") from error
        if not isinstance(decoded, str):
            raise ValueError(f"{where}: expected a string scalar")
        return decoded
    if not value or " #" in value:
        raise ValueError(f"{where}: unsupported or empty plain scalar")
    return value


def parse_capabilities(text: str) -> tuple[str, dict[str, dict[str, Any]]]:
    lines = text.splitlines()
    try:
        section = lines.index("capabilities:")
    except ValueError as error:
        raise ValueError("capabilities.yaml is missing its root capabilities section") from error

    updated_at = None
    for line in lines[:section]:
        match = re.fullmatch(r'updated_at: "(\d{4}-\d{2}-\d{2})"', line)
        if match:
            updated_at = match.group(1)
            break
    if updated_at is None:
        raise ValueError("capabilities.yaml is missing a quoted root updated_at date")

    capabilities: dict[str, dict[str, Any]] = {}
    current_name: str | None = None
    current: dict[str, Any] | None = None
    collecting_limitations = False
    active_limitation: list[str] | None = None
    active_limitation_line = 0

    def finish_limitation() -> None:
        nonlocal active_limitation
        if active_limitation is None or current is None:
            return
        current["limitations"].append(
            scalar(
                " ".join(part for part in active_limitation if part),
                where=f"line {active_limitation_line} {current_name}.limitations",
            )
        )
        active_limitation = None

    def finish() -> None:
        finish_limitation()
        if current_name is None or current is None:
            return
        missing = [field for field in STATUS_FIELDS if field not in current]
        if missing:
            raise ValueError(f"capability {current_name} is missing status fields: {missing}")
        current.setdefault("limitations", [])
        capabilities[current_name] = current

    for number, line in enumerate(lines[section + 1 :], section + 2):
        if line and not line[0].isspace():
            break
        match = CAPABILITY_KEY.fullmatch(line)
        if match:
            finish()
            current_name = match.group(1)
            if current_name in capabilities:
                raise ValueError(f"line {number}: duplicate capability {current_name}")
            current = {}
            collecting_limitations = False
            continue
        if current is None:
            if line.strip():
                raise ValueError(f"line {number}: content before first capability entry")
            continue
        status_match = STATUS_LINE.fullmatch(line)
        if status_match:
            finish_limitation()
            field, raw = status_match.groups()
            if field in current:
                raise ValueError(f"line {number}: duplicate {current_name}.{field}")
            current[field] = scalar(raw, where=f"line {number} {current_name}.{field}")
            collecting_limitations = False
            continue
        if line == "    limitations: []":
            finish_limitation()
            if "limitations" in current:
                raise ValueError(f"line {number}: duplicate {current_name}.limitations")
            current["limitations"] = []
            collecting_limitations = False
            continue
        if line == "    limitations:":
            finish_limitation()
            if "limitations" in current:
                raise ValueError(f"line {number}: duplicate {current_name}.limitations")
            current["limitations"] = []
            collecting_limitations = True
            continue
        if collecting_limitations and line.startswith("      - "):
            finish_limitation()
            active_limitation = [line[8:]]
            active_limitation_line = number
            continue
        if collecting_limitations and line.startswith("        "):
            if active_limitation is not None:
                active_limitation.append(line.strip())
            continue
        if not line.strip() or line.startswith("    evidence:") or line.startswith("      - "):
            collecting_limitations = False if line.startswith("    evidence:") else collecting_limitations
            continue
        if line.startswith("    "):
            finish_limitation()
            collecting_limitations = False
            continue
        if line.strip():
            raise ValueError(f"line {number}: unsupported capabilities.yaml shape: {line!r}")

    finish()
    if not capabilities:
        raise ValueError("capabilities.yaml contains no capabilities")
    return updated_at, capabilities


def build_report(text: str) -> dict[str, Any]:
    updated_at, source = parse_capabilities(text)
    capabilities: dict[str, Any] = {}
    unverified: list[dict[str, Any]] = []
    for name, fields in source.items():
        statuses = {field: fields[field] for field in STATUS_FIELDS}
        limitations = fields["limitations"]
        if not isinstance(limitations, list) or any(not isinstance(item, str) for item in limitations):
            raise ValueError(f"capability {name}: limitations must be a list of strings")
        unverified_fields = [field for field, status in statuses.items() if status is not True]
        capabilities[name] = {
            **statuses,
            "unverified_fields": unverified_fields,
            "limitations": limitations,
        }
        unverified.extend(
            {
                "capability": name,
                "field": field,
                "registered_value": statuses[field],
            }
            for field in unverified_fields
        )
    return {
        "schema_version": 1,
        "source": "maturity/capabilities.yaml",
        "source_updated_at": updated_at,
        "interpretation": (
            "Statuses and limitations are copied from the register. A false or non-true value "
            "is listed as unverified; this report does not infer whether a stage applies."
        ),
        "capabilities": capabilities,
        "unverified": unverified,
    }


def render(report: dict[str, Any]) -> str:
    return json.dumps(report, ensure_ascii=False, indent=2) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail if maturity/unverified.json differs from capabilities.yaml",
    )
    args = parser.parse_args()
    try:
        expected = render(build_report(SOURCE.read_text(encoding="utf-8")))
        if args.check:
            actual = OUTPUT.read_text(encoding="utf-8")
            if actual != expected:
                print(f"{OUTPUT.relative_to(ROOT)} is stale; run python tools/generate_unverified.py", file=sys.stderr)
                return 1
            print(f"{OUTPUT.relative_to(ROOT)} matches {SOURCE.relative_to(ROOT)}")
            return 0
        OUTPUT.write_text(expected, encoding="utf-8", newline="\n")
        print(f"wrote {OUTPUT.relative_to(ROOT)}")
        return 0
    except (OSError, ValueError) as error:
        print(f"generate_unverified: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
