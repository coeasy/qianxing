"""Console entry point for the Python SDK's currently supported R workflows."""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict, is_dataclass
from pathlib import Path
from typing import Sequence

from .app import (
    DataUnavailableError,
    InvalidInputError,
    QianxingError,
    compare_runs,
    doctor,
    run_backtest,
    run_experiment,
    validate_dataset,
    verify_run,
)


def _document(value: object) -> object:
    return asdict(value) if is_dataclass(value) else value


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


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="qianxing",
        description="牵星 Python SDK command line (currently Bar research workflows).",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("doctor", help="show installed SDK and native use-case availability")
    for name, help_text in (
        ("validate-dataset", "validate a versioned DatasetSpec JSON file"),
        ("backtest", "run a versioned Bar BacktestSpec JSON file"),
        ("verify", "verify artifacts described by a BacktestOutcome JSON file"),
        ("compare-runs", "rank completed runs from identical market data"),
        ("run-experiment", "run and compare a bounded Rust parameter grid"),
    ):
        command = commands.add_parser(name, help=help_text)
        command.add_argument("input", help="path to the versioned JSON document")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "doctor":
            result = doctor()
        else:
            payload = _read_json(args.input)
            if args.command == "validate-dataset":
                result = validate_dataset(payload)
            elif args.command == "backtest":
                result = run_backtest(payload)
            elif args.command == "compare-runs":
                result = compare_runs(payload)
            elif args.command == "run-experiment":
                result = run_experiment(payload)
            else:
                result = verify_run(payload)
        print(json.dumps(_document(result), ensure_ascii=False, indent=2, sort_keys=True))
        return 0
    except QianxingError as exc:
        error = exc.payload or {"category": exc.code, "message": str(exc)}
        print(json.dumps(error, ensure_ascii=False, sort_keys=True), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
