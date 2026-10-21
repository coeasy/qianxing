from __future__ import annotations

import sys
import unittest
from dataclasses import fields
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing.app import BuiltinStrategySpec


class BuiltinStrategySpecSerializationTests(unittest.TestCase):
    def assert_optional_fields_follow_dataclass_values(self, spec: BuiltinStrategySpec) -> None:
        payload = spec.to_dict()
        expected_fields = {
            field.name for field in fields(spec) if getattr(spec, field.name) is not None
        }
        self.assertEqual(set(payload), expected_fields)
        self.assertNotIn(None, payload.values())
        self.assertEqual(BuiltinStrategySpec(**payload), spec)

    def test_default_spec_omits_all_unset_sizing_fields(self) -> None:
        self.assert_optional_fields_follow_dataclass_values(
            BuiltinStrategySpec(kind="sma_cross", strategy_id="default-spec")
        )

    def test_configured_sizing_field_is_serialized_without_a_second_key_list(self) -> None:
        self.assert_optional_fields_follow_dataclass_values(
            BuiltinStrategySpec(
                kind="sma_cross",
                strategy_id="configured-spec",
                target_notional_raw=25_000_000_000,
            )
        )


if __name__ == "__main__":
    unittest.main()
