"""Server planning must remain read-only and use the runtime-selected binding."""
from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "qianxing.strategy_project",
    Path(__file__).parents[1] / "qianxing" / "strategy_project.py",
)
project = importlib.util.module_from_spec(spec)
spec.loader.exec_module(project)
explain_spec = importlib.util.spec_from_file_location(
    "qianxing.project_explain",
    Path(__file__).parents[1] / "qianxing" / "project_explain.py",
)
explanations = importlib.util.module_from_spec(explain_spec)
explain_spec.loader.exec_module(explanations)


class ServerProjectPlanTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.marker = self.root / "executed.txt"
        self.source = self.root / "strategy.py"
        self.source.write_text(
            "from pathlib import Path\nPath(" + repr(str(self.marker)) + ").write_text('executed')\nraise RuntimeError('must not import')\n",
            encoding="utf-8",
        )
        self.runtime = {
            "schema_version": 1, "environment": "paper",
            "strategies": [{"id": "selected-worker"}],
            "workers": [{"id": "selected-worker", "role": "strategy", "enabled": True}],
        }
        (self.root / "runtime.json").write_text(json.dumps(self.runtime), encoding="utf-8")
        self.manifest = {"project_id": "demo", "runtime_config": "runtime.json", "worker_id": "selected-worker", "strategy_file": "strategy.py", "input_spec": "input.json"}
        self.binding = {
            "worker": {"id": "selected-worker", "role": "strategy", "enabled": True, "symbols": []},
            "strategy": {"id": "selected-worker", "version": "strategy-runtime-v1", "python_module": "selected", "strategy_artifact_sha256": None},
        }
        self.calls = []

    def plan(self, **kwargs):
        app = types.ModuleType("qianxing.app")
        def select(runtime, worker_id):
            self.calls.append((runtime, worker_id))
            return self.binding
        app.plan_strategy_runtime_v1 = select
        with patch.object(project, "load_strategy_project", return_value=(self.root, self.manifest)), patch.dict(sys.modules, {"qianxing.app": app}):
            return project.server_project_plan(self.root, **kwargs)

    def explain_server(self, *, runtime_config=None, worker_id=None):
        return explanations.explain_server_configuration(
            root=self.root, manifest=self.manifest, runtime=self.runtime, binding=self.binding,
            runtime_path=self.root / (runtime_config or self.manifest["runtime_config"]),
            runtime_config=runtime_config, worker_id=worker_id,
        )

    def explain_local(self, raw, module, *, manifest=None, **kwargs):
        manifest = self.manifest if manifest is None else manifest
        selected = manifest.get("strategy_id") or module.get("strategy_id")
        bound = project.apply_parameter_defaults(raw, module, target_strategy_id=selected)
        resolved, _failures = project._resolve_input_paths(self.root, bound)
        return explanations.explain_local_configuration(
            root=self.root, manifest=manifest, raw_spec=raw, module_manifest=module,
            selected_id=selected, resolved_spec=resolved, strategy_file=self.source,
            input_path=self.root / "input.json", **kwargs,
        )

    def test_plan_does_not_execute_source_and_uses_runtime_binding(self):
        result = self.plan()
        self.assertFalse(self.marker.exists())
        self.assertEqual(self.calls, [(self.runtime, "selected-worker")])
        self.assertEqual(result["strategy_host"], "python_module")
        self.assertEqual(result["readiness"]["state"], "not_evaluated")
        self.assertFalse(result["readiness"]["trading_enabled"])
        self.assertEqual(result["strategy_artifact_binding"]["status"], "unbound")
        self.runtime["api"] = {"password": "server-password"}
        self.runtime["strategies"][0]["external_env"] = {"TOKEN": "runtime-secret"}
        self.binding["strategy"]["external_env"] = {"TOKEN": "binding-secret"}
        self.binding["worker"]["credential_env"] = {"api_key": "worker-secret"}
        before = copy.deepcopy((self.manifest, self.runtime, self.binding))
        files = set(self.root.iterdir())
        with patch.object(project, "load_strategy_file", side_effect=AssertionError("strategy import")), \
             patch.object(explanations, "apply_parameter_defaults", side_effect=AssertionError("server binding")), \
             patch.object(Path, "open", side_effect=AssertionError("explanation file I/O")):
            rows = self.explain_server()
        self.assertEqual(before, (self.manifest, self.runtime, self.binding))
        self.assertEqual(files, set(self.root.iterdir()))
        self.assertFalse(self.marker.exists())
        self.assertTrue(rows["strategy.id"]["source"].endswith("#$.strategies[0].id"))
        self.assertEqual(rows["strategy.version"]["source"], "rust_default:StrategyRuntimeConfig.version")
        self.assertEqual(rows["worker.symbols"]["source"], "rust_default:WorkerConfig.symbols")
        self.assertTrue(rows["worker.enabled"]["source"].endswith("#$.workers[0].enabled"))
        self.assertEqual(set(rows), {
            "runtime_config", "worker_id", "environment", "strategy.id", "strategy.version",
            "strategy.artifact_sha256", "strategy.account_id", "strategy.venue_id", "strategy.instrument",
            "worker.id", "worker.role", "worker.enabled", "worker.account_id", "worker.venue_id", "worker.symbols",
        })
        for hidden in ("server-password", "runtime-secret", "binding-secret", "worker-secret", "python_module", "external_env"):
            self.assertNotIn(hidden, json.dumps(rows))

    def test_source_digest_and_bundle_digest_have_distinct_evidence(self):
        self.binding["strategy"]["strategy_artifact_sha256"] = hashlib.sha256(self.source.read_bytes()).hexdigest()
        self.assertEqual(self.plan()["strategy_artifact_binding"]["status"], "source_matched")
        self.binding["strategy"]["strategy_artifact_sha256"] = "a" * 64
        self.assertEqual(self.plan()["strategy_artifact_binding"]["status"], "bundle_verification_required")
        self.assertFalse(self.marker.exists())

    def test_external_artifact_is_not_compared_to_python_scaffold(self):
        self.binding["strategy"].pop("python_module")
        self.binding["strategy"]["external_executable"] = "strategy.exe"
        self.assertEqual(self.plan()["strategy_artifact_binding"]["status"], "not_applicable")

    def test_explicit_version_mismatch_fails(self):
        self.manifest["strategy_version"] = "v2"
        with self.assertRaisesRegex(ValueError, "does not match"):
            self.plan()

    def test_legacy_target_position_profile_remains_plannable(self):
        self.binding["strategy"].pop("python_module")
        self.assertEqual(self.plan()["strategy_host"], "target_position")

    def test_local_input_paths_resolve_without_mutating_cwd_or_source_spec(self):
        data = self.root / "bars.csv"
        data.write_text("timestamp,open,high,low,close,volume\n", encoding="utf-8")
        source = {"bars_path": "bars.csv", "nested": [{"file_path": "missing.csv"}]}
        before = Path.cwd()
        resolved, failures = project._resolve_input_paths(self.root, source)
        self.assertEqual(Path.cwd(), before)
        self.assertEqual(source["bars_path"], "bars.csv")
        self.assertEqual(resolved["bars_path"], str(data.resolve()))
        self.assertEqual(resolved["nested"][0]["file_path"], str((self.root / "missing.csv").resolve()))
        self.assertEqual(failures, [str((self.root / "missing.csv").resolve())])
        with patch.object(Path, "open", side_effect=AssertionError("explanation file I/O")):
            rows = explanations.explain_local_configuration(
                root=self.root, manifest=self.manifest, raw_spec=source, module_manifest={},
                selected_id=None, resolved_spec=resolved, strategy_file=self.source,
                input_path=self.root / "input.json",
            )
        self.assertEqual(rows["spec.bars_path"]["value"], str(data.resolve()))
        self.assertEqual(rows["spec.bars_path"]["original_value"], "bars.csv")
        self.assertTrue(rows["spec.bars_path"]["source"].endswith("input.json#$.bars_path"))
        self.assertEqual(rows["spec.nested[0].file_path"]["original_value"], "missing.csv")
        self.assertEqual(rows["input_spec"]["value"], str(self.root / "input.json"))
        self.assertEqual(rows["input_spec"]["original_value"], "input.json")
        self.assertEqual(rows["backend"]["value"], "local")
        self.assertEqual(rows["execution_boundary"]["value"], "trusted_local_python")
        self.assertNotIn("environment", rows)
        self.assertEqual(Path.cwd(), before)

    def test_local_parameter_provenance_tracks_real_multimarket_merge(self):
        module = {
            "strategy_id": "module-id", "strategy_version": "module-v1",
            "parameters": {"fast": {"default": 2}, "slow": {"default": 20}},
        }
        self.manifest.update(strategy_id="selected", strategy_version="project-v2")
        raw = {
            "strategy": {"strategy_id": "selected", "fast": 8, "undeclared": 1},
            "parameters": {"fast": 9, "undeclared": 99},
            "strategy_slot": {"strategy": {"strategy_id": "selected", "fast": 9}},
            "markets": [
                {"strategies": [
                    {"strategy": {"strategy_id": "selected", "fast": 6}},
                    {"strategy": {"strategy_id": "other", "fast": 7}},
                ]},
                {"strategies": [{"strategy": {"strategy_id": "selected"}}]},
            ],
            "base": {
                "parameters": {"fast": 5}, "strategy": {"fast": 4},
                "base": {"strategy": {}},
            },
        }
        originals = copy.deepcopy((raw, module, self.manifest))
        resolved = project.apply_parameter_defaults(raw, module, target_strategy_id="selected")
        resolved_before = copy.deepcopy(resolved)
        rows = explanations.explain_local_configuration(
            root=self.root, manifest=self.manifest, raw_spec=raw, module_manifest=module,
            selected_id="selected", resolved_spec=resolved, strategy_file=self.source,
            input_path=self.root / "input.json",
        )
        self.assertEqual(originals, (raw, module, self.manifest))
        self.assertEqual(resolved_before, resolved)
        self.assertEqual(rows["strategy_id"]["value"], "selected")
        self.assertTrue(rows["strategy_id"]["source"].endswith("qianxing.strategy.json#$.strategy_id"))
        self.assertEqual(rows["strategy_id"]["overridden"][0]["value"], "module-id")
        self.assertEqual(rows["strategy_version"]["value"], "project-v2")
        for path in ("strategy", "strategy_slot.strategy", "markets[0].strategies[0].strategy", "markets[1].strategies[0].strategy"):
            with self.subTest(path=path):
                self.assertEqual(rows[f"spec.{path}.fast"]["value"], 9)
                self.assertTrue(rows[f"spec.{path}.fast"]["source"].endswith("input.json#$.parameters.fast"))
                self.assertEqual(rows[f"spec.{path}.slow"]["value"], 20)
                self.assertTrue(rows[f"spec.{path}.slow"]["source"].endswith("strategy.py#$.parameters.slow.default"))
        self.assertEqual([entry["value"] for entry in rows["spec.strategy_slot.strategy.fast"]["overridden"]], [2, 9])
        self.assertEqual([entry["value"] for entry in rows["spec.parameters.fast"]["overridden"]], [2])
        self.assertEqual(rows["spec.strategy.undeclared"]["value"], 1)
        self.assertEqual(rows["spec.markets[0].strategies[1].strategy.fast"]["value"], 7)
        self.assertEqual(rows["spec.markets[0].strategies[1].strategy.fast"]["overridden"], [])
        self.assertNotIn("spec.markets[0].strategies[1].strategy.slow", rows)
        self.assertEqual(rows["spec.base.strategy.fast"]["value"], 5)
        self.assertTrue(rows["spec.base.strategy.fast"]["source"].endswith("#$.base.parameters.fast"))
        self.assertEqual(rows["spec.base.base.strategy.fast"]["value"], 2)
        without_top = copy.deepcopy(raw)
        del without_top["parameters"]["fast"]
        fallback = self.explain_local(without_top, module)
        self.assertEqual(fallback["spec.strategy.fast"]["value"], 8)
        self.assertTrue(fallback["spec.strategy.fast"]["source"].endswith("#$.strategy.fast"))
        self.assertEqual(fallback["spec.markets[0].strategies[0].strategy.fast"]["value"], 6)
        del without_top["strategy"]["fast"]
        default = self.explain_local(without_top, module)
        self.assertEqual(default["spec.strategy.fast"]["value"], 2)
        self.assertTrue(default["spec.strategy.fast"]["source"].startswith("module_manifest:"))
        fallback_manifest = {key: value for key, value in self.manifest.items() if key not in {"strategy_id", "strategy_version"}}
        self.assertEqual(self.explain_local({}, module, manifest=fallback_manifest)["strategy_id"]["value"], "module-id")

    def test_optional_trace_preserves_candidate_merge_and_input_objects(self):
        module = {"parameters": {"fast": {"default": 2}, "slow": {"default": 20}}}
        raw = {
            "strategy": {"fast": 8}, "parameters": {"fast": 9},
            "markets": [{"strategies": [{"strategy": {"fast": 7}}]}],
            "base": {"strategy": {"fast": 3}},
        }
        candidate = {"fast": 9, "slow": 30, "undeclared": 100}
        before = copy.deepcopy((raw, module, candidate))
        trace = {}
        normal = project.apply_parameter_defaults(raw, module, candidate_parameters=candidate)
        traced = project.apply_parameter_defaults(raw, module, candidate_parameters=candidate, parameter_trace=trace)
        self.assertEqual(normal, traced)
        self.assertEqual(before, (raw, module, candidate))
        self.assertEqual([item["value"] for item in trace[("strategy", "fast")]], [2, 8, 9, 9])
        self.assertEqual([item["value"] for item in trace[("parameters", "fast")]], [2, 9, 9])
        market_path = ("markets", 0, "strategies", 0, "strategy")
        self.assertEqual([item["value"] for item in trace[(*market_path, "fast")]], [2, 7, 9, 9])
        # Defaults/candidate writes into parameters must not become invented input sources.
        self.assertEqual([item["source"] for item in trace[(*market_path, "slow")]], ["module_manifest", "candidate_parameters"])
        self.assertEqual(trace[("base", "strategy", "fast")][-1]["source"], "candidate_parameters")
        self.assertNotIn("undeclared", traced["strategy"])
        traced["strategy"]["fast"] = 0
        trace[("strategy", "fast")][0]["value"] = 0
        self.assertEqual(before, (raw, module, candidate))

    def test_local_explanation_redacts_values_history_and_nested_fields(self):
        module = {"parameters": {"api_key": {"default": "module-key"}}}
        raw = {
            "strategy": {"api_key": "slot-key"}, "parameters": {"api_key": "top-key"},
            "credentials": {"arbitrary": "credential-value"},
            "nested": [{
                "password": "password-value", "Private-Key": "private-value",
                "accessToken": "token-value", "secret": {"arbitrary": "secret-value"},
                "visible": {"value": 42, "client_api_key": "nested-key"},
            }],
        }
        before = copy.deepcopy((raw, module))
        rows = self.explain_local(raw, module)
        encoded = json.dumps(rows)
        for secret in ("module-key", "slot-key", "top-key", "credential-value", "password-value", "private-value", "token-value", "secret-value", "nested-key"):
            self.assertNotIn(secret, encoded)
        self.assertEqual(rows["spec.strategy.api_key"]["value"], "[REDACTED]")
        self.assertEqual([item["value"] for item in rows["spec.strategy.api_key"]["overridden"]], ["[REDACTED]", "[REDACTED]"])
        self.assertEqual(rows["spec.credentials"]["value"], "[REDACTED]")
        self.assertEqual(rows["spec.nested[0].visible.value"]["value"], 42)
        self.assertEqual(before, (raw, module))
        rows["spec.nested[0].visible.value"]["value"] = 99
        self.assertEqual(before, (raw, module))

    def test_explicit_selectors_override_manifest_and_keep_original_paths(self):
        self.manifest["worker_id"] = "manifest-worker"
        (self.root / "alternative.json").write_text(json.dumps(self.runtime), encoding="utf-8")
        self.plan(runtime_config="alternative.json", worker_id="selected-worker")
        self.assertEqual(self.calls, [(self.runtime, "selected-worker")])
        rows = self.explain_server(runtime_config="alternative.json", worker_id="selected-worker")
        self.assertEqual(rows["runtime_config"]["source"], "flag:--runtime-config")
        self.assertEqual(rows["runtime_config"]["value"], str(self.root / "alternative.json"))
        self.assertEqual(rows["runtime_config"]["original_value"], "alternative.json")
        self.assertEqual(rows["runtime_config"]["overridden"][0]["original_value"], "runtime.json")
        self.assertEqual(rows["runtime_config"]["overridden"][0]["value"], str((self.root / "runtime.json").resolve()))
        self.assertEqual(rows["worker_id"]["source"], "flag:--worker-id")
        self.assertEqual(rows["worker_id"]["overridden"][-1]["value"], "manifest-worker")
        self.runtime["workers"].append({"id": "manifest-worker", "role": "strategy", "enabled": True})
        self.runtime["strategies"].append({"id": "manifest-worker", "version": "manifest-v1"})
        self.binding["worker"]["id"] = "manifest-worker"
        self.binding["strategy"].update(id="manifest-worker", version="manifest-v1")
        fallback = self.explain_server()
        self.assertEqual(fallback["runtime_config"]["value"], str(self.root / "runtime.json"))
        self.assertTrue(fallback["runtime_config"]["source"].endswith("#$.runtime_config"))
        self.assertEqual(fallback["worker_id"]["value"], "manifest-worker")
        self.assertTrue(fallback["worker_id"]["source"].endswith("#$.worker_id"))
        self.assertTrue(fallback["strategy.version"]["source"].endswith("#$.strategies[1].version"))
        del self.manifest["worker_id"]
        self.assertTrue(self.explain_server()["worker_id"]["source"].endswith("#$.workers[1].id"))
        chosen_input = (Path.cwd() / "chosen-input.json").resolve()
        chosen_strategy = (Path.cwd() / "chosen-strategy.py").resolve()
        local = explanations.explain_local_configuration(
            root=self.root, manifest=self.manifest, raw_spec={}, module_manifest={},
            selected_id=None, resolved_spec={}, strategy_file=chosen_strategy, input_path=chosen_input,
            input_spec_override="chosen-input.json", strategy_file_override="chosen-strategy.py",
        )
        for field, flag in (("input_spec", "input-spec"), ("strategy_file", "strategy-file")):
            self.assertEqual(local[field]["source"], "flag:--" + flag)
            self.assertEqual(local[field]["overridden"][0]["original_value"], self.manifest[field])
        self.assertEqual(local["input_spec"]["value"], str(chosen_input))
        self.assertEqual(local["strategy_file"]["value"], str(chosen_strategy))
        self.assertEqual(local["input_spec"]["original_value"], "chosen-input.json")
        self.assertFalse(self.marker.exists())

    def test_server_sources_distinguish_legacy_runtime_defaults_and_unknowns(self):
        example = Path(__file__).parents[2] / "deploy" / "qianxing.runtime.strategy-backtest.example.json"
        self.runtime = json.loads(example.read_text(encoding="utf-8"))
        self.manifest["worker_id"] = "strategy-backtest"
        self.binding = {
            "worker": copy.deepcopy(self.runtime["workers"][1]),
            "strategy": {**self.runtime["strategy"], "strategy_artifact_sha256": None},
        }
        rows = self.explain_server()
        self.assertTrue(rows["strategy.version"]["source"].endswith("#$.strategy.version"))
        self.assertTrue(rows["worker.account_id"]["source"].endswith("#$.workers[1].account_id"))
        self.assertEqual(rows["strategy.artifact_sha256"]["source"], "rust_default:StrategyRuntimeConfig.strategy_artifact_sha256")
        del self.runtime["strategy"]["version"]
        self.binding["strategy"]["version"] = "strategy-runtime-v1"
        fallback = self.explain_server()
        self.assertNotEqual(rows["strategy.version"]["value"], fallback["strategy.version"]["value"])
        self.assertEqual(fallback["strategy.version"]["source"], "rust_default:StrategyRuntimeConfig.version")
        self.runtime["strategy"]["version"] = "strategy-runtime-v1"
        self.assertTrue(self.explain_server()["strategy.version"]["source"].endswith("#$.strategy.version"))
        del self.runtime["workers"][1]["enabled"]
        self.assertTrue(self.explain_server()["worker.enabled"]["source"].startswith("unknown:"))
        self.runtime["strategies"] = [{"id": "not-selected", "version": "unrelated"}]
        self.assertTrue(self.explain_server()["strategy.version"]["source"].startswith("unknown:"))
        self.binding["strategy"]["instrument"] = {"arbitrary": "do-not-dump"}
        self.runtime["environment"] = {"arbitrary": "do-not-dump"}
        self.assertNotIn("do-not-dump", json.dumps(self.explain_server()))


if __name__ == "__main__":
    unittest.main()
