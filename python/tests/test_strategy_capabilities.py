import importlib.util
from pathlib import Path
import unittest
import sys
import types
import subprocess
sys.path.insert(0, str(Path(__file__).parents[1]))
from qianxing_strategy.worker import _handler_from_module, _validate_host_requirements

spec = importlib.util.spec_from_file_location("_manifest_under_test", Path(__file__).parents[1] / "qianxing" / "strategy_manifest.py")
manifest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(manifest)


class StrategyCapabilityManifestTests(unittest.TestCase):
    def raw(self):
        return {"schema_version": 1, "parameters": {}, "instrument_requirements": {"requires_explicit_spec": False}, "data_level": "bar", "warmup_bars": 0}

    def normalize(self, raw):
        return manifest.normalize_manifest(raw, strategy_id="demo", strategy_version="1", dependencies=[])

    def test_legacy_metadata_shape_is_preserved(self):
        self.assertNotIn("required_capabilities", self.normalize(self.raw()))

    def test_requirements_survive_normalization_for_rust_validation(self):
        raw = self.raw()
        raw["required_capabilities"] = ["fill_updates", "bar"]
        self.assertEqual(self.normalize(raw)["required_capabilities"], ["bar", "fill_updates"])

    def test_malformed_and_duplicate_requirements_are_rejected(self):
        for value in [None, "bar", ["bar", "bar"], [1], [" "]]:
            with self.subTest(value=value):
                raw = self.raw()
                raw["required_capabilities"] = value
                with self.assertRaises(ValueError):
                    self.normalize(raw)


class ProcessHostCapabilityTests(unittest.TestCase):
    def test_worker_class_setup_does_not_import_application_sdk_or_ssl(self):
        program = '''import importlib.abc, sys, types
class DenyApplication(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path=None, target=None):
        if fullname == "qianxing" or fullname.startswith("qianxing.") or fullname == "ssl":
            raise ImportError("application dependency reached strategy worker: " + fullname)
sys.meta_path.insert(0, DenyApplication())
from qianxing_strategy import BaseStrategy
from qianxing_strategy.worker import _handler_from_module
class Strategy(BaseStrategy):
    pass
handler = _handler_from_module(types.SimpleNamespace(STRATEGY_CLASS=Strategy), {})
assert isinstance(handler.strategy, Strategy)
'''
        result = subprocess.run([sys.executable, "-c", program], cwd=Path(__file__).parents[1],
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)

    def catalog(self, level="bar"):
        return {"schema_version": 1, "host_id": f"runtime_python_class_{level}",
                "profile": "server", "supported": [level, "fill_updates", "durable_checkpoint"]}

    def test_runtime_catalog_accepts_requirements_before_constructing_class(self):
        from qianxing_strategy import BaseStrategy
        class Strategy(BaseStrategy):
            STRATEGY_MANIFEST = {"schema_version": 1, "parameters": {},
                "instrument_requirements": {"requires_explicit_spec": False},
                "data_level": "bar", "warmup_bars": 0,
                "required_capabilities": ["bar", "durable_checkpoint"]}
        handler = _handler_from_module(types.SimpleNamespace(STRATEGY_CLASS=Strategy), {},
                                       host_capabilities=self.catalog())
        self.assertIsInstance(handler.strategy, Strategy)

    def test_missing_and_unknown_requirements_fail_before_factory(self):
        for required in (["timer"], ["future_magic"], ["bar", "bar"]):
            module = types.SimpleNamespace(STRATEGY_MANIFEST={"required_capabilities": required},
                                           create_strategy=lambda spec: self.fail("must not construct"))
            with self.subTest(required=required), self.assertRaises(ValueError):
                _handler_from_module(module, {}, host_capabilities=self.catalog())

    def test_catalog_must_match_schema_profile_and_selected_data_level(self):
        for changes in ({"schema_version": True}, {"schema_version": 2},
                        {"profile": "local"}, {"host_id": "runtime_python_class_tick"},
                        {"supported": ["bar", "bar"]}, {"supported": [1]}, {"extra": 1}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                _validate_host_requirements(types.SimpleNamespace(), {}, {**self.catalog(), **changes})

    def test_catalog_cannot_enable_module_function_mode(self):
        with self.assertRaises(ValueError):
            _validate_host_requirements(types.SimpleNamespace(), None, self.catalog())

    def test_explicit_requirements_fail_before_strategy_factory(self):
        def factory(spec):
            self.fail("factory must not run without capability negotiation")
        module = types.SimpleNamespace(
            STRATEGY_MANIFEST={"required_capabilities": ["bar"]},
            create_strategy=factory,
        )
        with self.assertRaisesRegex(ValueError, "capability negotiation is unavailable"):
            _handler_from_module(module, {})

    def test_module_function_mode_cannot_ignore_required_capabilities(self):
        module = types.SimpleNamespace(STRATEGY_MANIFEST={"required_capabilities": ["account_updates"]}, on_event=lambda request: None)
        with self.assertRaisesRegex(ValueError, "cannot establish required_capabilities"):
            _handler_from_module(module)

    def test_class_manifest_requirements_are_also_checked(self):
        class Strategy:
            STRATEGY_MANIFEST = {"required_capabilities": ["durable_checkpoint"]}
        with self.assertRaisesRegex(ValueError, "cannot establish required_capabilities"):
            _handler_from_module(types.SimpleNamespace(STRATEGY_CLASS=Strategy), {})


if __name__ == "__main__":
    unittest.main()
