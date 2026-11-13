import contextlib
import hashlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing import cli
from qianxing.reports.experiment_html import (
    default_report_path,
    load_verified_candidate_details,
    load_verified_multi_market_candidate_details,
    _project_multi_market_fill_exact_values,
    render_experiment_html,
    write_experiment_html,
)


class ExperimentHtmlTest(unittest.TestCase):
    def test_exact_fill_decimal_projection_preserves_i128_scale(self):
        fill = _project_multi_market_fill_exact_values(
            [
                {
                    "exact_amounts": {
                        "quantity": {
                            "coefficient": "170141183460469231731687303715884105727",
                            "scale": 18,
                        },
                        "price": {"coefficient": "1", "scale": 18},
                    },
                    "exact_fees": [
                        {
                            "amount": {
                                "coefficient": "123456789012345678901234567890",
                                "scale": 18,
                            },
                            "currency": "BNB",
                        }
                    ],
                }
            ]
        )[0]
        self.assertEqual(
            fill["qty_exact"],
            "170141183460469231731.687303715884105727",
        )
        self.assertEqual(fill["price_exact"], "0.000000000000000001")
        self.assertEqual(
            json.loads(fill["exact_fees_json"])[0]["amount"],
            "123456789012.345678901234567890",
        )

    def verified_artifact_result(
        self, root: Path, *, absolute_paths: bool = False, exact_equity: bool = False
    ):
        run_dir = root / "runs" / "run-verified"
        run_dir.mkdir(parents=True)
        paths = {
            "summary": run_dir / "summary.json",
            "run_manifest": run_dir / "manifest.json",
            "equity": run_dir / "equity.csv",
            "fills": run_dir / "fills.csv",
            "run_record": run_dir / "record.json",
        }
        relative = {name: path.relative_to(root).as_posix() for name, path in paths.items()}
        references = {
            name: str(path) if absolute_paths else relative[name]
            for name, path in paths.items()
        }
        run_id = "run-verified"
        data_fingerprint = "barframe:0123456789abcdef"
        result_hash = "0123456789abcdef"
        summary = {
            "schema_version": 7,
            "run_id": run_id,
            "result_hash": result_hash,
            "data_fingerprint": data_fingerprint,
            "instrument": "BTCUSDT.BINANCE",
            "account_id": "paper-a",
            "strategy_id": "sma",
            "strategy_version": "1.0",
            "currency": "USDT",
            "account": {
                "initial_cash_raw": "100",
                **({"initial_cash_exact": "100.0000000001"} if exact_equity else {}),
            },
            "final_equity_raw": 105,
            "fees_raw": 5,
            "turnover_raw": 80,
            "metrics_exact": {
                "turnover": {"amount": "0.0000000801", "currency": "USDT"},
                "fees_by_currency": {"BNB": "0.0000000002", "USDT": "0.0000000048"},
                **(
                    {
                        "equity": {
                            "initial": "100.0000000001",
                            "final": "105.0000000002",
                            "return": "5.0000000001",
                            "return_bps": "500",
                            "max_drawdown": "0",
                            "max_drawdown_bps": "0",
                        }
                    }
                    if exact_equity
                    else {}
                ),
            },
            "fee_model": "test-fee-v1",
            "fill_model_details": {"name": "NextBarOpen"},
            "equity_points": 2,
            "fills": 1,
            "run_manifest": references["run_manifest"],
            "run_record": references["run_record"],
            "artifact_locations": {
                name: references[name]
                for name in ("summary", "run_manifest", "equity", "fills")
            },
        }
        equity = (
            "ts,equity_raw,equity_exact,position_raw\n1,100,100.0000000001,0\n2,105,105.0000000002,1\n"
            if exact_equity
            else "ts,equity_raw,position_raw\n1,100,0\n2,105,1\n"
        )
        fills = (
            "order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id,qty_exact,price_exact,exact_fees_json\n"
            '1,2,1,80,5,paper-a,sma,,,,,0.000000000123,0.000000987654321123,"[{""amount"":{""coefficient"":""2"",""scale"":12},""currency"":""BNB""}]"\n'
        )
        manifest = {
            "run_id": run_id,
            "data_fingerprint": data_fingerprint,
            "result_hash": result_hash,
        }
        paths["equity"].write_text(equity, encoding="utf-8")
        paths["fills"].write_text(fills, encoding="utf-8")
        paths["run_manifest"].write_text(json.dumps(manifest), encoding="utf-8")
        paths["summary"].write_text(json.dumps(summary), encoding="utf-8")
        artifact_kinds = {
            "summary": "backtest-summary-json",
            "run_manifest": "run-manifest-json",
            "equity": "backtest-equity-csv",
            "fills": "backtest-fills-csv",
        }
        refs = []
        for name in ("summary", "run_manifest", "equity", "fills"):
            raw = paths[name].read_bytes()
            refs.append(
                {
                    "name": name,
                    "artifact_id": hashlib.sha256(raw).hexdigest(),
                    "kind": artifact_kinds[name],
                    "schema_version": (
                        7
                        if name == "summary"
                        else (
                            2
                            if name == "fills" or (name == "equity" and exact_equity)
                            else 1
                        )
                    ),
                    "byte_len": len(raw),
                }
            )
        paths["run_record"].write_text(
            json.dumps(
                {
                    "schema_version": 2,
                    "run_id": run_id,
                    "status": "completed",
                    "input_digest": data_fingerprint,
                    "code_identity": "test:python-fixture",
                    "config_fingerprint": "0123456789abcdef",
                    "started_at_ms": 1,
                    "finished_at_ms": 2,
                    "capability_level": "L1",
                    "replay_verdict": "verified",
                    "artifact_refs": refs,
                }
            ),
            encoding="utf-8",
        )
        result = {
            "experiment_id": "verified-search",
            "candidates": [
                {
                    "ordinal": 1,
                    "parameters": {"fast_window": 3},
                    "outcome": {
                        "run_id": run_id,
                        "result_hash": result_hash,
                        "data_fingerprint": data_fingerprint,
                        "artifacts": {
                            key: references[key]
                            for key in ("summary", "run_manifest", "equity", "fills")
                        },
                    },
                }
            ],
            "comparison": {"runs": [{"rank": 1, "run": {"run_id": run_id}}]},
        }
        return result, paths

    def multi_market_artifact_result(self, root: Path):
        run_dir = root / "runs" / "multi-verified"
        run_dir.mkdir(parents=True)
        run_path = run_dir / "multi-market.json"
        manifest_path = run_dir / "run.json"
        equity_path = run_dir / "equity.json"
        exact_equity_path = run_dir / "equity-exact.json"
        fills_path = run_dir / "fills.json"
        contributions_path = run_dir / "contributions.json"
        frame_paths = {
            "BTCUSDT.BINANCE": root / "bars-btc.json",
            "ETHUSDT.OKX": root / "bars-eth.json",
        }
        for path in frame_paths.values():
            path.write_text("{}", encoding="utf-8")
        hashes = {
            "BTCUSDT.BINANCE": "0123456789abcdef",
            "ETHUSDT.OKX": "fedcba9876543210",
        }
        market_data = [
            {
                "instrument": instrument,
                "path": str(path),
                "content_hash": hashes[instrument],
                "row_count": 3,
            }
            for instrument, path in frame_paths.items()
        ]
        fingerprint = "multi-market-fingerprint-verified"
        run_id, spec_fingerprint, result_hash = "multi-verified", "spec-1234", "result-1234"
        equity = [[1, 100], [2, 110], [3, 120]]
        fill = {
            "ts": 2,
            "order_id": 7,
            "qty": 1,
            "price": 10,
            "fee": 1,
            "fee_currency": "USDT",
            "account_id": "joint-a",
            "strategy_id": "btc-sma",
            "venue_id": "BINANCE",
            "exact_amounts": {
                "quantity": {"coefficient": "1", "scale": 12},
                "price": {"coefficient": "1234", "scale": 12},
            },
            "exact_fees": [
                {"amount": {"coefficient": "2", "scale": 15}, "currency": "BNB"}
            ],
        }
        fills = [fill]
        contributions = {
            "BTCUSDT.BINANCE": [
                {"equity_index": 0, "ts": 1, "pnl_contribution_raw": "0", "pnl_contribution_exact": "0"},
                {"equity_index": 1, "ts": 2, "pnl_contribution_raw": "2", "pnl_contribution_exact": "0.000000002"},
                {"equity_index": 2, "ts": 3, "pnl_contribution_raw": "5", "pnl_contribution_exact": "0.0000000050000001"},
            ],
            "ETHUSDT.OKX": [
                {"equity_index": 0, "ts": 1, "pnl_contribution_raw": "0", "pnl_contribution_exact": "0"},
                {"equity_index": 1, "ts": 2, "pnl_contribution_raw": "8", "pnl_contribution_exact": "0.000000008"},
                {"equity_index": 2, "ts": 3, "pnl_contribution_raw": "15", "pnl_contribution_exact": "0.000000015"},
            ],
        }
        exact_equity = [
            {"equity_index": 0, "ts": 1, "equity_exact": "0.0000001"},
            {"equity_index": 1, "ts": 2, "equity_exact": "0.00000011"},
            {"equity_index": 2, "ts": 3, "equity_exact": "0.0000001200000001"},
        ]
        equity_path.write_text(json.dumps(equity, separators=(",", ":")), encoding="utf-8")
        exact_equity_bytes = json.dumps(exact_equity, separators=(",", ":")).encode("utf-8")
        exact_equity_path.write_bytes(exact_equity_bytes)
        fills_path.write_text(json.dumps(fills, separators=(",", ":")), encoding="utf-8")
        contribution_bytes = json.dumps(contributions, separators=(",", ":")).encode("utf-8")
        contributions_path.write_bytes(contribution_bytes)
        components = {
            f"barframe:{instrument}": hashes[instrument]
            for instrument in frame_paths
        }
        components["derived:instrument-equity-contributions"] = hashlib.sha256(contribution_bytes).hexdigest()
        components["derived:equity-exact"] = hashlib.sha256(exact_equity_bytes).hexdigest()
        manifest = {
            "run_id": run_id,
            "data_fingerprint": fingerprint,
            "result_hash": result_hash,
            "strategy_version": f"multi-market-strategy-set:{spec_fingerprint}",
            "input_components": components,
        }
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        market_specs = [
            {
                "instrument": instrument,
                "bars_path": str(frame_paths[instrument]),
                "priority": index,
                "strategies": [
                    {"strategy": {"strategy_id": "btc-sma" if index == 0 else "eth-sma"}}
                ],
                "market_spec": {"tick_size_raw": 1},
                "fee_schedule": {"maker_fee_bps": 2},
            }
            for index, instrument in enumerate(frame_paths)
        ]
        outcome = {
            "run_id": run_id,
            "account_id": "joint-a",
            "spec_fingerprint": spec_fingerprint,
            "data_fingerprint": fingerprint,
            "shared_result_hash": result_hash,
            "markets_total": 2,
            "successful_fills": 1,
            "equity_points": 3,
            "final_equity_raw": 120,
            "return_bps": 2000,
            "max_drawdown_bps": 0,
            "artifact_path": str(run_path),
        }
        artifact = {
            "schema_version": 2,
            "replay_verified": True,
            "outcome": outcome,
            "spec": {
                "account_id": "joint-a",
                "settlement_currency": "USDT",
                "initial_cash_raw": 100,
                "markets": market_specs,
            },
            "market_data": market_data,
            "dataset_quality": {},
            "per_market_fills": {"BTCUSDT.BINANCE": 1, "ETHUSDT.OKX": 0},
            "equity": equity,
            "event_log": [{"kind": {"Filled": {"fill": fill}}}],
            "run_manifest_path": str(manifest_path),
            "equity_path": str(equity_path),
            "equity_exact_path": str(exact_equity_path),
            "fills_path": str(fills_path),
            "instrument_equity_contributions_path": str(contributions_path),
        }
        run_path.write_text(json.dumps(artifact, separators=(",", ":")), encoding="utf-8")
        result = {
            "experiment_id": "multi-search",
            "candidates": [
                {
                    "ordinal": 1,
                    "parameters": {"fast": 3},
                    "outcome": {
                        **outcome,
                        "artifact_path": str(run_path),
                    },
                }
            ],
            "ranking": [
                {"rank": 1, "ordinal": 1, "run_id": run_id, "shared_result_hash": result_hash}
            ],
        }

        def validate_market(spec):
            instrument = next(
                key for key, path in frame_paths.items() if Path(spec["bars_path"]).resolve() == path.resolve()
            )
            return SimpleNamespace(
                usable=True,
                instrument=instrument,
                rows=3,
                content_fingerprint=f"barframe:{hashes[instrument]}",
            )

        return result, validate_market, contributions_path

    def result(self):
        return {
            "schema_version": 1,
            "experiment_id": "search-1",
            "strategy_id": "sma",
            "total_candidates": 1,
            "completed_candidates": 1,
            "succeeded_candidates": 1,
            "failed_candidates": 0,
            "candidates": [
                {
                    "ordinal": 1,
                    "parameters": {"fast_window": 5},
                    "outcome": {
                        "run_id": "run-1",
                        "fills": 12,
                        "artifact_path": "runs/run-1.json",
                    },
                }
            ],
            "comparison": {
                "runs": [
                    {
                        "rank": 1,
                        "run": {
                            "run_id": "run-1",
                            "return_bps": 125,
                            "max_drawdown_bps": 25,
                        },
                    }
                ]
            },
            "out_of_sample": {
                "out_of_sample_return_bps": 85,
                "folds": [
                    {
                        "fold": 1,
                        "train_range": [0, 20],
                        "test_range": [20, 30],
                        "selected_parameters": {"fast_window": 5},
                        "in_sample_return_bps": 125,
                        "out_of_sample_return_bps": 85,
                    }
                ],
            },
            "search": {
                "algorithm": "tpe_kde",
                "seed": 17,
                "budget": 1,
                "fold_searches": [
                    {
                        "fold": 1,
                        "trials": [
                            {
                                "ordinal": 1,
                                "proposal": "tpe_kde",
                                "parameters": {"fast_window": 5},
                                "training_score_bps": 125,
                            }
                        ],
                    }
                ],
            },
            "artifact_path": "runs/search-1.json",
        }

    def test_render_includes_search_trace_oos_sort_filter_and_csv_export(self):
        page = render_experiment_html(self.result())
        for text in (
            "候选排名",
            "样本外折叠",
            "自适应搜索轨迹",
            "报告范围",
            "运行详情、净值与成交费用",
            "candidateFilter",
            "exportCandidates",
            "tpe_kde",
            "85",
            "无网络资源",
        ):
            self.assertIn(text, page)
        self.assertNotIn("<script src=", page)
        self.assertIn("max_drawdown_bps", page)

    def test_verified_candidate_details_are_hash_checked_and_rendered(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, _ = self.verified_artifact_result(Path(temporary), absolute_paths=True)
            verifier_calls = []

            def rust_verifier(outcome):
                verifier_calls.append(outcome["run_id"])
                self.assertEqual(
                    set(outcome["artifacts"]),
                    {"summary", "run_manifest", "equity", "fills"},
                )
                self.assertTrue(all(Path(value).is_absolute() for value in outcome["artifacts"].values()))
                return SimpleNamespace(
                    verified=True,
                    checks=("summary", "equity", "fills", "replay"),
                    mismatches=(),
                )

            details = load_verified_candidate_details(
                result, base_dir=temporary, rust_verifier=rust_verifier
            )
            self.assertEqual(len(details["runs"]), 1)
            self.assertEqual(verifier_calls, ["run-verified"])
            self.assertEqual(details["runs"][0]["fees_raw"], 5)
            self.assertEqual(
                details["runs"][0]["metrics_exact"]["turnover"]["amount"],
                "0.0000000801",
            )
            self.assertIn("Rust VerifyRun", details["runs"][0]["verified_artifacts"])
            self.assertTrue(details["runs"][0]["engine_verification"]["verified"])
            self.assertEqual(details["runs"][0]["equity"][-1]["equity_raw"], "105")
            result["research_details"] = details
            page = render_experiment_html(result)
            self.assertIn("运行详情、净值与成交费用", page)
            self.assertIn("equityChart", page)
            self.assertIn("downloadFills", page)
            self.assertIn('"fee_raw":"5"', page)
            self.assertIn("精确权益、成交额与费用", page)
            self.assertIn('"BNB":"0.0000000002"', page)
            self.assertIn("qty_exact", page)
            self.assertIn("price_exact", page)
            self.assertIn("exact_fees_json", page)
            self.assertIn("精确权益、成交额与费用", page)
            self.assertIn("0.000000000123", page)
            self.assertIn("0.000000987654321123", page)

    def test_exact_equity_csv_v2_is_verified_and_embedded_in_research_details(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, _ = self.verified_artifact_result(
                Path(temporary), absolute_paths=True, exact_equity=True
            )
            details = load_verified_candidate_details(result, base_dir=temporary)
            run = details["runs"][0]
            self.assertEqual(run["initial_cash_exact"], "100.0000000001")
            self.assertEqual(
                run["equity"][-1]["equity_exact"], "105.0000000002"
            )

    def test_run_record_fills_schema_v2_requires_exact_fill_columns(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, paths = self.verified_artifact_result(Path(temporary), absolute_paths=True)
            legacy_fills = b"order_id,ts,qty_raw,price_raw,fee_raw\n1,2,1,80,5\n"
            paths["fills"].write_bytes(legacy_fills)
            record = json.loads(paths["run_record"].read_text(encoding="utf-8"))
            fills_ref = next(row for row in record["artifact_refs"] if row["name"] == "fills")
            fills_ref["artifact_id"] = hashlib.sha256(legacy_fills).hexdigest()
            fills_ref["byte_len"] = len(legacy_fills)
            paths["run_record"].write_text(json.dumps(record), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "fills schema v2 is missing exact fill columns"):
                load_verified_candidate_details(result, base_dir=temporary)

    def test_candidate_details_reject_a_failed_rust_verification(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, _ = self.verified_artifact_result(Path(temporary), absolute_paths=True)

            def rust_verifier(_outcome):
                return SimpleNamespace(
                    verified=False,
                    checks=(),
                    mismatches=("replay ledger mismatch",),
                )

            with self.assertRaisesRegex(ValueError, "replay ledger mismatch"):
                load_verified_candidate_details(
                    result, base_dir=temporary, rust_verifier=rust_verifier
                )

    def test_candidate_details_consume_shared_versioned_rust_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, paths = self.verified_artifact_result(Path(temporary), absolute_paths=True)
            calls = []

            def report_reader(outcome):
                calls.append(outcome)
                summary = json.loads(paths["summary"].read_text(encoding="utf-8"))
                return {
                    "schema_version": 1,
                    "run_id": "run-verified",
                    "instrument": "BTCUSDT.BINANCE",
                    "verification": {
                        "verified": True,
                        "checks": ["RunManifest", "RunRecord"],
                        "mismatches": [],
                    },
                    "summary": summary,
                    "equity": {
                        "total": 2,
                        "truncated": False,
                        "rows": [
                            {"ts": "1", "equity_raw": "100", "position_raw": "0"},
                            {"ts": "2", "equity_raw": "105", "position_raw": "1"},
                        ],
                    },
                    "fills": {
                        "total": 1,
                        "truncated": False,
                        "rows": [{
                            "ts": "2",
                            "order_id": "1",
                            "qty_raw": "1",
                            "price_raw": "80",
                            "fee_raw": "5",
                            "account_id": "paper-a",
                            "signal_id": "",
                            "intent_id": "",
                            "venue_id": "",
                            "venue_order_id": "",
                            "qty_exact": "0.000000000123",
                            "price_exact": "0.000000987654321123",
                            "exact_fees_json": '[{"amount":{"coefficient":"2","scale":12},"currency":"BNB"}]',
                            "strategy_id": "sma",
                        }],
                    },
                }

            details = load_verified_candidate_details(
                result,
                base_dir=temporary,
                rust_report_reader=report_reader,
            )

            self.assertEqual(len(calls), 1)
            self.assertEqual(details["runs"][0]["engine_verification"]["report_schema_version"], 1)
            self.assertIn("Rust report/VerifyRun v1", details["runs"][0]["verified_artifacts"])

    def test_candidate_details_reject_a_rust_report_for_a_different_summary(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, _ = self.verified_artifact_result(Path(temporary), absolute_paths=True)

            def report_reader(_outcome):
                return {
                    "schema_version": 1,
                    "run_id": "run-verified",
                    "verification": {"verified": True, "checks": [], "mismatches": []},
                    "summary": {"run_id": "different-run"},
                    "equity": {"total": 0, "truncated": False, "rows": []},
                    "fills": {"total": 0, "truncated": False, "rows": []},
                }

            with self.assertRaisesRegex(ValueError, "differs from the bounded artifact read"):
                load_verified_candidate_details(
                    result, base_dir=temporary, rust_report_reader=report_reader
                )

    def test_candidate_detail_loading_rejects_tampered_run_artifact(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, paths = self.verified_artifact_result(Path(temporary))
            paths["fills"].write_text(
                paths["fills"].read_text(encoding="utf-8").replace(
                    "1,2,1,80,5,paper-a", "1,2,1,90,5,paper-a"
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "SHA-256"):
                load_verified_candidate_details(result, base_dir=temporary)

    def test_multi_market_details_verify_rust_inputs_and_contribution_conservation(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, validator, contribution_path = self.multi_market_artifact_result(Path(temporary))
            details = load_verified_multi_market_candidate_details(
                result, market_validator=validator, base_dir=temporary
            )
            self.assertEqual(details["kind"], "multi_market_bar")
            self.assertEqual(details["runs"][0]["shared_equity_points"], 3)
            self.assertEqual(details["runs"][0]["contribution_points_checked"], 3)
            self.assertEqual(
                details["runs"][0]["market_equity_contributions"]["BTCUSDT.BINANCE"][-1]["pnl_contribution_raw"],
                "5",
            )
            self.assertEqual(
                details["runs"][0]["market_equity_contributions"]["BTCUSDT.BINANCE"][-1]["pnl_contribution_exact"],
                "0.0000000050000001",
            )
            self.assertEqual(details["runs"][0]["equity"][-1]["equity_exact"], "0.0000001200000001")
            fill = details["runs"][0]["fills"][0]
            self.assertEqual(fill["qty_exact"], "0.000000000001")
            self.assertEqual(fill["price_exact"], "0.000000001234")
            self.assertEqual(
                json.loads(fill["exact_fees_json"])[0]["amount"],
                "0.000000000000002",
            )
            result["research_details"] = details
            page = render_experiment_html(result)
            self.assertIn("按标的权益贡献", page)
            self.assertIn("marketContributionChart", page)
            self.assertIn("pnl_contribution_exact", page)
            self.assertIn("0.0000000050000001", page)
            self.assertIn("共享账户", page)
            self.assertIn("qty_exact", page)
            self.assertIn("price_exact", page)
            self.assertIn("exact_fees_json", page)
            self.assertIn("0.000000000001", page)
            self.assertIn("0.000000001234", page)
            self.assertNotIn(str(contribution_path), page)
            with self.assertRaisesRegex(ValueError, "overwrite"):
                write_experiment_html(result, contribution_path)

    def test_multi_market_details_reject_a_modified_contribution_sidecar(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, validator, contribution_path = self.multi_market_artifact_result(Path(temporary))
            contribution = json.loads(contribution_path.read_text(encoding="utf-8"))
            contribution["BTCUSDT.BINANCE"][-1]["pnl_contribution_raw"] = "6"
            contribution_path.write_text(json.dumps(contribution), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "fingerprint"):
                load_verified_multi_market_candidate_details(
                    result, market_validator=validator, base_dir=temporary
                )

    def test_multi_market_legacy_raw_only_report_remains_readable(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, validator, contribution_path = self.multi_market_artifact_result(Path(temporary))
            run_path = Path(result["candidates"][0]["outcome"]["artifact_path"])
            artifact = json.loads(run_path.read_text(encoding="utf-8"))
            artifact["schema_version"] = 1
            artifact.pop("equity_exact_path")
            contributions = json.loads(contribution_path.read_text(encoding="utf-8"))
            for rows in contributions.values():
                for row in rows:
                    row.pop("pnl_contribution_exact")
            contribution_bytes = json.dumps(contributions, separators=(",", ":")).encode("utf-8")
            contribution_path.write_bytes(contribution_bytes)
            manifest_path = Path(artifact["run_manifest_path"])
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["input_components"].pop("derived:equity-exact")
            manifest["input_components"]["derived:instrument-equity-contributions"] = hashlib.sha256(
                contribution_bytes
            ).hexdigest()
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            run_path.write_text(json.dumps(artifact, separators=(",", ":")), encoding="utf-8")

            details = load_verified_multi_market_candidate_details(
                result, market_validator=validator, base_dir=temporary
            )
            run = details["runs"][0]
            self.assertNotIn("equity_exact", run["equity"][-1])
            self.assertNotIn(
                "pnl_contribution_exact",
                run["market_equity_contributions"]["BTCUSDT.BINANCE"][-1],
            )

    def test_multi_market_exact_sidecar_tampering_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, validator, _ = self.multi_market_artifact_result(Path(temporary))
            run_path = Path(result["candidates"][0]["outcome"]["artifact_path"])
            artifact = json.loads(run_path.read_text(encoding="utf-8"))
            exact_path = Path(artifact["equity_exact_path"])
            exact = json.loads(exact_path.read_text(encoding="utf-8"))
            exact[-1]["equity_exact"] = "0.0000001200000002"
            exact_path.write_text(json.dumps(exact, separators=(",", ":")), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "fingerprint"):
                load_verified_multi_market_candidate_details(
                    result, market_validator=validator, base_dir=temporary
                )

    def test_render_escapes_untrusted_values_and_warns_when_no_oos(self):
        result = self.result()
        result["experiment_id"] = "</script><script>alert(1)</script>"
        result.pop("out_of_sample")
        page = render_experiment_html(result)
        self.assertIn("&lt;/script&gt;&lt;script&gt;alert(1)&lt;/script&gt;", page)
        self.assertNotIn("<script>alert(1)</script>", page)
        self.assertIn("仅有样本内结果", page)
        self.assertIn("未包含未来数据、预热或前视偏差诊断", page)

    def test_candidate_diagnostics_render_per_candidate_status_and_parameters(self):
        result = self.result()
        result["candidate_diagnostics"] = {
            "coverage_status": "partial",
            "candidate_count": 2,
            "diagnosed_count": 1,
            "diagnostic_failure_count": 0,
            "unavailable_candidate_count": 0,
            "not_run_candidate_count": 1,
            "items": [
                {
                    "scope": "primary",
                    "fold": None,
                    "ordinal": 1,
                    "strategy_kind": "sma_cross",
                    "parameters": {"fast_window": 2},
                    "status": "diagnosed",
                    "diagnostics": {
                        "lookahead": {"status": "no_change_detected", "flagged_checks": 0},
                        "startup": {"status": "stable_across_requested_windows"},
                    },
                },
                {
                    "scope": "primary",
                    "ordinal": 2,
                    "parameters": {"fast_window": 3},
                    "status": "not_run_candidate_limit",
                    "reason": "diagnostic budget",
                },
            ],
        }
        page = render_experiment_html(result)
        self.assertIn("逐候选未来数据与预热诊断", page)
        self.assertIn("no_change_detected", page)
        self.assertIn("stable_across_requested_windows", page)
        self.assertIn('&quot;fast_window&quot;: 2', page)
        self.assertIn("not_run_candidate_limit", page)
        self.assertIn("coverage_status", page)

    def test_default_path_and_atomic_report_write(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result = self.result()
            result["artifact_path"] = str(root / "search.json")
            output = root / "nested" / "review.html"
            actual = write_experiment_html(result, output)
            self.assertEqual(actual, output)
            self.assertTrue(actual.is_file())
            self.assertIn("search-1", actual.read_text(encoding="utf-8"))
            self.assertEqual(default_report_path(result), root / "search.report.html")

    def test_rejects_output_that_would_overwrite_input_or_run_artifact(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result = self.result()
            protected = root / "run.json"
            protected.write_text("keep", encoding="utf-8")
            result["candidates"][0]["outcome"]["artifact_path"] = str(protected)
            with self.assertRaisesRegex(ValueError, "overwrite"):
                write_experiment_html(result, protected)
            self.assertEqual(protected.read_text(encoding="utf-8"), "keep")

    def test_cli_writes_full_report_even_when_stdout_uses_compact_summary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            strategy = root / "strategy.py"
            strategy.write_text(
                "def create_strategy(spec):\n    return spec\n", encoding="utf-8"
            )
            spec = root / "spec.json"
            spec.write_text(
                json.dumps(
                    {
                        "experiment_id": "cli-search",
                        "base": {"strategy": {"fast_window": 3}},
                        "parameter_space": [{"name": "fast_window", "values": [3]}],
                        "split_plan": {"train_bars": 20, "test_bars": 10},
                    }
                ),
                encoding="utf-8",
            )
            output = root / "report.html"
            result = self.result()
            result["experiment_id"] = "cli-search"
            result["artifact_path"] = str(root / "result.json")
            stdout = io.StringIO()
            stderr = io.StringIO()
            with patch.object(cli, "run_experiment_with_strategy", return_value=result):
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    code = cli.main(
                        ["search-strategy", str(strategy), str(spec), "--summary", "--html", str(output)]
                    )
            self.assertEqual(code, 0)
            summary = json.loads(stdout.getvalue())
            self.assertNotIn("candidates", summary)
            page = output.read_text(encoding="utf-8")
            self.assertIn('"fast_window":5', page)
            self.assertIn("Interactive HTML report:", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
