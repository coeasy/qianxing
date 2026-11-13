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


def _summary(
    root: Path,
    config_fingerprint: str,
    config_hash: str,
    result_hash: str,
    schema_version: int,
) -> str:
    stem = (
        "strategy-backtest_strategy-macd_BTCUSDT.BINANCE_"
        f"{config_fingerprint}_{'a' * 24}-{'b' * 16}.v{schema_version}.summary.json"
    )
    return json.dumps(
        {
            "config_hash": config_hash,
            "result_hash": result_hash,
            "run_manifest": str(root / "data" / "runs" / stem.replace(f".v{schema_version}.summary.json", ".run.json")),
            "run_record": str(root / "data" / "runs" / stem.replace(".summary.json", ".record.json")),
        },
        separators=(",", ":"),
    )


class BacktestAcceptanceNormalizationTests(unittest.TestCase):
    def test_evidence_bundle_path_fields_normalize_but_run_identity_does_not(self) -> None:
        temp_root = TOOLS.parent / "target" / "test-tmp"
        temp_root.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="backtest-acceptance-evidence-", dir=temp_root) as directory:
            root = Path(directory)
            roots = (root / "project-a", root / "project-b")
            paths = (root / "evidence-a.json", root / "evidence-b.json")
            config_hashes = ("1" * 64, "2" * 64)
            for path, project, config_hash in zip(paths, roots, config_hashes):
                project.mkdir()
                config = project / "runtime.json"
                config.write_text(json.dumps({"data_dir": str(project / "data")}), encoding="utf-8")
                path.write_text(
                    json.dumps(
                        {
                            "generation": {
                                "run": {
                                    "identity": {"config_digest": config_hash},
                                    "run": {
                                        "config_hash": config_hash,
                                        "run_id": f"strategy-backtest:fixture:BTCUSDT.BINANCE:{config_hash}:stable-result",
                                    },
                                    "artifacts": [
                                        {
                                            "name": "runtime_config",
                                            "path": str(config),
                                            "digest": acceptance.sha256_file(config),
                                        }
                                    ],
                                    "recompute": {
                                        "inputs": [str(config)],
                                        "steps": [f"config_hash={config_hash}"],
                                    },
                                }
                            }
                        },
                        separators=(",", ":"),
                    ),
                    encoding="utf-8",
                )

            self.assertEqual(
                acceptance.sha256_normalized_evidence(paths[0], roots[0]),
                acceptance.sha256_normalized_evidence(paths[1], roots[1]),
            )

            changed = root / "evidence-changed.json"
            changed.write_text(
                json.dumps(
                    {
                        "generation": {
                            "run": {
                                "identity": {"config_digest": config_hashes[1]},
                                "run": {
                                    "config_hash": config_hashes[1],
                                    "run_id": f"strategy-backtest:fixture:BTCUSDT.BINANCE:{config_hashes[1]}:changed-result",
                                },
                                "artifacts": [
                                    {
                                        "name": "runtime_config",
                                        "path": str(roots[1] / "runtime.json"),
                                        "digest": acceptance.sha256_file(roots[1] / "runtime.json"),
                                    }
                                ],
                                "recompute": {
                                    "inputs": [str(roots[1] / "runtime.json")],
                                    "steps": [f"config_hash={config_hashes[1]}"],
                                },
                            }
                        }
                    },
                    separators=(",", ":"),
                ),
                encoding="utf-8",
            )
            self.assertNotEqual(
                acceptance.sha256_normalized_evidence(paths[0], roots[0]),
                acceptance.sha256_normalized_evidence(changed, roots[1]),
            )

    def test_path_normalization_handles_versioned_names_without_erasing_result_identity(self) -> None:
        temp_root = TOOLS.parent / "target" / "test-tmp"
        temp_root.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="backtest-acceptance-normalize-", dir=temp_root
        ) as directory:
            root = Path(directory)
            roots = (root / "project-a", root / "project-b")
            config_fingerprints = ("1" * 64, "2" * 64)
            config_hashes = ("3" * 64, "4" * 64)
            for schema_version in (7, 8, 12):
                with self.subTest(schema_version=schema_version):
                    files = []
                    for index, (project, fingerprint, config_hash) in enumerate(
                        zip(roots, config_fingerprints, config_hashes)
                    ):
                        path = root / f"{schema_version}-{index}.json"
                        path.write_text(
                            _summary(project, fingerprint, config_hash, "same-result", schema_version),
                            encoding="utf-8",
                        )
                        files.append(path)

                    self.assertEqual(
                        acceptance.sha256_normalized(files[0], roots[0]),
                        acceptance.sha256_normalized(files[1], roots[1]),
                    )

                    changed_result = root / f"{schema_version}-changed-result.json"
                    changed_result.write_text(
                        _summary(roots[1], config_fingerprints[1], config_hashes[1], "different-result", schema_version),
                        encoding="utf-8",
                    )
                    self.assertNotEqual(
                        acceptance.sha256_normalized(files[0], roots[0]),
                        acceptance.sha256_normalized(changed_result, roots[1]),
                    )
