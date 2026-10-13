import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_bridge.app import _PythonBacktestCallback


class StrategyOrderUpdateBridgeTest(unittest.TestCase):
    def test_optional_order_update_handler_returns_null_when_absent(self):
        callback = _PythonBacktestCallback(lambda _context, _event: {})
        self.assertEqual(callback._qx_on_order_update("{}", '{"event":"fill"}'), "null")

    def test_order_update_handler_receives_decoded_context_and_fill_fact(self):
        class Strategy:
            def on_order_update(self, context, update):
                self.context = context
                self.update = update
                return {"schema_version": 1, "strategy_id": "alpha"}

        strategy = Strategy()
        callback = _PythonBacktestCallback(strategy)
        response = callback._qx_on_order_update(
            '{"strategy_id":"alpha"}',
            '{"schema_version":1,"event":"fill","fill":{"qty":{"raw":3}}}',
        )
        self.assertEqual(strategy.context, {"strategy_id": "alpha"})
        self.assertEqual(strategy.update["event"], "fill")
        self.assertEqual(json.loads(response)["strategy_id"], "alpha")


if __name__ == "__main__":
    unittest.main()
