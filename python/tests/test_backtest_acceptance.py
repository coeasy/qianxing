"""Regression coverage for portable artifact comparison on the Windows release path."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parents[2] / "tools"
sys.path.insert(0, str(TOOLS))

import backtest_acceptance as acceptance  # noqa: E402


def _summary(root: Path, config_fingerprint: str, config_hash: str, result_hash: str) -> str:
    stem = (
        "strategy-backtest_strategy-macd_BTCUSDT.BINANCE_"
        f"{config_fingerprint}_{'a' * 24}-{'b' * 16}.v7.summary.json"
    )
    return json.dumps(
        {
            "config_hash": config_hash,
            "result_hash": result_hash,
            "run_manifest": str(root / "data" / "runs" / stem.replace(".v7.summary.json", ".run.json")),
            "run_record": str(root / "data" / "runs" / stem.replace(".v7.summary.json", ".v7.record.json")),
        },
        separators=(",", ":"),
    )


class BacktestAcceptanceNormalizationTests(unittest.TestCase):
    def test_path_normalization_handles_v7_names_without_erasing_result_identity(self) -> None:
        temp_root = TOOLS.parent / "target" / "test-tmp"
        temp_root.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="backtest-acceptance-normalize-", dir=temp_root
        ) as directory:
            root = Path(directory)
            roots = (root / "project-a", root / "project-b")
            config_fingerprints = ("1" * 64, "2" * 64)
            config_hashes = ("3" * 64, "4" * 64)
            files = []
            for index, (project, fingerprint, config_hash) in enumerate(
                zip(roots, config_fingerprints, config_hashes)
            ):
                path = root / f"{index}.json"
                path.write_text(
                    _summary(project, fingerprint, config_hash, "same-result"),
                    encoding="utf-8",
                )
                files.append(path)

            self.assertEqual(
                acceptance.sha256_normalized(files[0], roots[0]),
                acceptance.sha256_normalized(files[1], roots[1]),
            )

            changed_result = root / "changed-result.json"
            changed_result.write_text(
                _summary(roots[1], config_fingerprints[1], config_hashes[1], "different-result"),
                encoding="utf-8",
            )
            self.assertNotEqual(
                acceptance.sha256_normalized(files[0], roots[0]),
                acceptance.sha256_normalized(changed_result, roots[1]),
            )
