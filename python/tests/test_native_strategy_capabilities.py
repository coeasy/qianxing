"""Load the freshly built extension when QX_TEST_NATIVE_LIBRARY is provided."""
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import unittest


class NativeStrategyCapabilityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = os.environ.get("QX_TEST_NATIVE_LIBRARY")
        if not path:
            raise unittest.SkipTest("set QX_TEST_NATIVE_LIBRARY to test a freshly built extension")
        path = str(Path(path).resolve(strict=True))
        loader = importlib.machinery.ExtensionFileLoader("_qianxing_native", path)
        spec = importlib.util.spec_from_file_location("_qianxing_native", path, loader=loader)
        cls.native = importlib.util.module_from_spec(spec)
        loader.exec_module(cls.native)

    def request(self, required, execution=None):
        return json.dumps({
            "schema_version": 1,
            "purpose": "run",
            "strategy_manifest": {
                "schema_version": 1, "strategy_id": "bridge", "strategy_version": "1",
                "entrypoint": "create_strategy(spec)", "dependencies": [],
                "completeness": "partial", "data_level": "bar", "warmup_bars": 0,
                "required_capabilities": required,
            },
            "execution_spec": execution or {"bars_path": "must-not-be-read.json"},
        })

    def test_native_returns_the_rust_capability_catalog(self):
        result = json.loads(self.native.app_validate_strategy_execution_preflight_v1(self.request(["bar", "bar_history"])))
        self.assertEqual(result["host_capabilities"]["host_id"], "local_python_bar")
        self.assertIn("bar_history", result["host_capabilities"]["supported"])
        self.assertEqual(result["checked_markets"], [])

    def test_native_rejects_unsupported_requirements_before_reading_files(self):
        with self.assertRaises(self.native.QxAppError) as raised:
            self.native.app_validate_strategy_execution_preflight_v1(self.request(["durable_checkpoint"]))
        error = json.loads(str(raised.exception))
        self.assertEqual(error["category"], "INVALID_INPUT")
        self.assertEqual(error["retry"], "NEVER")
        self.assertFalse(error["safe_to_retry"])
        self.assertIn("durable_checkpoint", error["message"])

    def test_native_selects_actual_tick_host_for_partial_manifest(self):
        with self.assertRaises(self.native.QxAppError) as raised:
            self.native.app_validate_strategy_execution_preflight_v1(self.request(["bar_history"], {"depth_path": "must-not-be-read.json", "tier": "l1"}))
        error = json.loads(str(raised.exception))
        self.assertIn("local_python_tick", error["message"])
        self.assertEqual(error["correlation_id"], "bridge")


if __name__ == "__main__":
    unittest.main()
