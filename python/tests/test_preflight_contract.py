"""Strict Python decoding for the Rust-owned backtest preflight response contract."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing.app import BacktestPreflightResultV1

try:
    import jsonschema
except ImportError:  # pragma: no cover - optional developer validation dependency
    jsonschema = None


def _valid_result() -> dict:
    fixture = Path(__file__).parent / "fixtures" / "backtest-preflight-result-v1.json"
    return json.loads(fixture.read_text(encoding="utf-8"))


class BacktestPreflightContractTests(unittest.TestCase):
    @unittest.skipIf(jsonschema is None, "install jsonschema to validate the published schema")
    def test_registered_json_schema_accepts_the_shared_fixture_and_rejects_type_drift(self):
        schema_path = Path(__file__).resolve().parents[2] / "schemas" / "backtest-preflight-result-v1.json"
        schema = json.loads(schema_path.read_text(encoding="utf-8"))
        jsonschema.Draft202012Validator.check_schema(schema)
        validator = jsonschema.Draft202012Validator(schema)
        self.assertEqual(list(validator.iter_errors(_valid_result())), [])

        blocked = _valid_result()
        blocked["ready"] = False
        blocked["data"]["usable"] = False
        blocked["data"]["gaps"] = ["insufficient rows"]
        blocked["checks"][0]["status"] = "blocked"
        self.assertEqual(list(validator.iter_errors(blocked)), [])

        for mutate in (
            lambda value: value.update(ready="true"),
            lambda value: value.update(ready=False),
            lambda value: value["data"].update(rows=True),
            lambda value: value["data"].update(usable=False),
            lambda value: value["data"].update(unregistered=True),
            lambda value: value["data"].pop("first_ts"),
        ):
            invalid = _valid_result()
            mutate(invalid)
            with self.subTest(invalid=invalid):
                self.assertTrue(list(validator.iter_errors(invalid)))

    def test_decodes_rust_response_without_coercion_and_accepts_optional_serde_fields(self):
        value = _valid_result()
        del value["data"]["first_ts"]

        result = BacktestPreflightResultV1.from_dict(value)

        self.assertEqual(result.schema_version, 1)
        self.assertTrue(result.ready)
        self.assertIsNone(result.data.first_ts)
        self.assertEqual(result.checks[0].detail, None)
        self.assertEqual(result.data.gaps, ())

        blocked = _valid_result()
        blocked["ready"] = False
        blocked["data"]["usable"] = False
        blocked["data"]["gaps"] = ["insufficient rows"]
        blocked["checks"][0]["status"] = "blocked"
        self.assertFalse(BacktestPreflightResultV1.from_dict(blocked).ready)

    def test_rejects_unknown_fields_at_each_nested_contract_level(self):
        cases = []
        for path, key in (
            ((), "extra"),
            (("data",), "extra"),
            (("checks", 0), "extra"),
        ):
            value = _valid_result()
            target = value
            for part in path:
                target = target[part]
            target[key] = True
            cases.append((path, value))

        for path, value in cases:
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "unknown="):
                BacktestPreflightResultV1.from_dict(value)

    def test_rejects_python_coercions_that_rust_serde_rejects(self):
        mutations = (
            lambda value: value.update(schema_version="1"),
            lambda value: value.update(ready="false"),
            lambda value: value["data"].update(usable="false"),
            lambda value: value["data"].update(rows=True),
            lambda value: value["data"].update(rows="70"),
            lambda value: value["data"].update(first_ts=-1),
            lambda value: value["data"].update(last_ts=1 << 64),
            lambda value: value["data"].update(gaps=[1]),
            lambda value: value["checks"][0].update(id=1),
            lambda value: value["checks"][0].update(status=[]),
            lambda value: value["checks"][0].update(detail=1),
            lambda value: value.update(limitations=[1]),
        )
        for mutate in mutations:
            value = _valid_result()
            mutate(value)
            with self.subTest(value=value), self.assertRaises(ValueError):
                BacktestPreflightResultV1.from_dict(value)

    def test_rejects_ready_flag_that_disagrees_with_blocked_check(self):
        value = _valid_result()
        value["checks"][0]["status"] = "blocked"
        with self.assertRaisesRegex(ValueError, "ready flag disagrees"):
            BacktestPreflightResultV1.from_dict(value)

    def test_rejects_dataset_usability_that_disagrees_with_gaps(self):
        for usable, gaps in ((False, []), (True, ["missing interval"])):
            value = _valid_result()
            value["data"]["usable"] = usable
            value["data"]["gaps"] = gaps
            with self.subTest(usable=usable, gaps=gaps), self.assertRaisesRegex(
                ValueError, "usable flag disagrees"
            ):
                BacktestPreflightResultV1.from_dict(value)

    def test_rejects_reversed_dataset_timestamps(self):
        value = _valid_result()
        value["data"]["first_ts"] = 4_000
        value["data"]["last_ts"] = 3_000
        with self.assertRaisesRegex(ValueError, "first_ts must not be after last_ts"):
            BacktestPreflightResultV1.from_dict(value)

    def test_rejects_wrong_container_shapes_instead_of_iterating_them(self):
        cases = []
        for field, replacement in (
            ("checks", {"id": "dataset_sample"}),
            ("limitations", "not-an-array"),
        ):
            value = _valid_result()
            value[field] = replacement
            cases.append(value)
        for value in cases:
            with self.subTest(value=value), self.assertRaises(ValueError):
                BacktestPreflightResultV1.from_dict(value)


if __name__ == "__main__":
    unittest.main()
