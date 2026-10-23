"""Python SDK 分层（方案 §6.2「接口分层」）。

守的是四件事，全部**构造性**——不读散文、不比对文档：

1. 八个子层都能 import，每层的 `__all__` 里的名字都真能从那一层取到；
2. 子层是**顶层命名空间的分层视图**：每层的名字都取自顶层那份稳定名单，子层不另造私有名字；
3. 分层**语义**对得上：本地层没有远端面、远端层没有本地自由函数、Paper 与 Live 各自
   指向自己那个客户端；
4. 顶层命名空间与 `qianxing.app` 的公开面**逐一相等**——不多导出、也不少导出。

第 2 条与第 4 条一起把「分层」钉死：想给某一层加一个名字，就必须先想清楚它是不是
顶层稳定面的一部分；想给 `app.py` 加一个公开类而不导出，第 4 条当场红。

「optional dependencies 不改变核心语义」由第 5 条用例用**子进程**核：在只装了核心包的
解释器里 `import qianxing.core` 必须成功，且 `ccxt` / `qianxing_ccxt` / `qianxing_ashare`
不得被顺带拉进来。
"""

from __future__ import annotations

import re
import subprocess
import sys
import unittest
from dataclasses import fields
from typing import get_args, get_type_hints
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import qianxing  # noqa: E402
from qianxing import app as app_module  # noqa: E402

PYTHON_DIR = Path(__file__).resolve().parents[1]
PACKAGE_DIR = PYTHON_DIR / "qianxing"

#: 方案 §6.2 点名的八个子层。
LAYERS = (
    "core",
    "client",
    "data",
    "strategy",
    "paper",
    "live",
    "experiments",
    "reports",
)

#: 子层里允许出现、但**刻意不进**顶层稳定名单的名字（按子层点名）。
#: 理由：它们来自 `qianxing_strategy` 这个独立发行包，是策略作者那一侧的契约类型，
#: 不是 `qx-app` 用例面的一部分；放进顶层会把稳定面撑大一圈。
SUB_NAMESPACE_ONLY = {
    "strategy": ("SCHEMA_VERSION", "StrategyBars", "StrategyInput", "StrategyIntent", "StrategyOutput"),
    "core": ("AsyncCoreClient", "AsyncRunHandle"),
    "data": (
        "BAR_JSON_CONTENT_TYPE",
        "FUNDING_RATE_JSON_CONTENT_TYPE",
        "ORDER_BOOK_SNAPSHOT_JSON_CONTENT_TYPE",
        "ProviderAdapter",
        "ProviderAdapterError",
        "ProviderCapability",
        "ProviderDatasetBacktestOutcome",
        "ProviderDepthBacktestOutcome",
        "ProviderManualInterventionError",
        "ProviderPayloadRow",
        "ProviderPermanentError",
        "ProviderQuery",
        "ProviderRegistry",
        "ProviderResponse",
    ),
}

#: 远端面：只有 `qianxing.client` 与服务端执行层能出现。
REMOTE_NAMES = ("AppClient", "RemoteRunHandle", "PaperClient", "LiveClient", "TradeWorkflowClient")

#: 本地自由函数：只有 `qianxing.core` 能出现。
LOCAL_FREE_FUNCTIONS = (
    "run_backtest",
    "start_backtest",
    "validate_dataset",
    "run_depth_backtest",
    "run_multi_leg_backtest",
    "run_multi_market_depth_backtest",
)

#: 可选适配器：核心层不得在 import 期把它们拉进来。
OPTIONAL_MODULES = ("ccxt", "qianxing_ccxt", "qianxing_ashare")


def _layer(name: str):
    return __import__(f"qianxing.{name}", fromlist=["*"])


def _layer_path(name: str) -> Path:
    """子层的落点：包（`<name>/__init__.py`）或单文件模块（`<name>.py`）。"""
    package = PACKAGE_DIR / name / "__init__.py"
    if package.is_file():
        return package
    return PACKAGE_DIR / f"{name}.py"


class LayerShapeTest(unittest.TestCase):
    def test_every_layer_has_a_landing_place_and_a_non_empty_all(self):
        for name in LAYERS:
            with self.subTest(layer=name):
                self.assertTrue(_layer_path(name).is_file(), f"qianxing.{name} 没有落点")
                exported = _layer(name).__all__
                self.assertTrue(exported, f"qianxing.{name}.__all__ 是空的")

    def test_every_exported_name_resolves_from_its_own_layer(self):
        for name in LAYERS:
            module = _layer(name)
            with self.subTest(layer=name):
                unresolved = [item for item in module.__all__ if not hasattr(module, item)]
                self.assertEqual(unresolved, [], f"qianxing.{name} 导出了取不到的名字")

    def test_layers_are_views_over_the_top_level_namespace(self):
        top = set(qianxing.__all__)
        for name in LAYERS:
            allowed = top | set(SUB_NAMESPACE_ONLY.get(name, ()))
            with self.subTest(layer=name):
                outside = sorted(set(_layer(name).__all__) - allowed)
                self.assertEqual(
                    outside,
                    [],
                    f"qianxing.{name} 出现了顶层稳定名单之外的名字：{outside}",
                )

    def test_sub_namespace_only_roster_does_not_rot(self):
        """名册里的名字必须真在那个子层里，否则它只是一张过期的通行证。"""
        for name, roster in SUB_NAMESPACE_ONLY.items():
            exported = set(_layer(name).__all__)
            with self.subTest(layer=name):
                self.assertEqual(sorted(set(roster) - exported), [])


class LayerSemanticsTest(unittest.TestCase):
    def test_local_layer_carries_no_remote_surface(self):
        exported = set(_layer("core").__all__)
        self.assertEqual(sorted(exported & set(REMOTE_NAMES)), [])

    def test_remote_layer_carries_no_local_free_functions(self):
        exported = set(_layer("client").__all__)
        self.assertEqual(sorted(exported & set(LOCAL_FREE_FUNCTIONS)), [])
        self.assertIn("AppClient", exported)
        self.assertIn("RemoteRunHandle", exported)

    def test_paper_and_live_point_at_their_own_client(self):
        paper = set(_layer("paper").__all__)
        live = set(_layer("live").__all__)
        self.assertIn("PaperClient", paper)
        self.assertNotIn("LiveClient", paper)
        self.assertIn("LiveClient", live)
        self.assertNotIn("PaperClient", live)
        # 两条轨共用同一条控制面，所以都摆出那个共用基类。
        self.assertIn("TradeWorkflowClient", paper)
        self.assertIn("TradeWorkflowClient", live)

    def test_data_layer_keeps_the_optional_adapters_out(self):
        exported = set(_layer("data").__all__)
        self.assertEqual(sorted(exported & set(OPTIONAL_MODULES)), [])

    def test_portfolio_spec_is_available_in_local_and_remote_shared_contract(self):
        from qianxing import (
            BacktestSpec,
            BuiltinStrategySpec,
            PortfolioBacktestConfig,
            PortfolioStrategySpec,
        )
        from qianxing.core import PortfolioBacktestConfig as CorePortfolioConfig

        self.assertIs(CorePortfolioConfig, PortfolioBacktestConfig)
        basket = PortfolioBacktestConfig(
            primary_priority=0,
            primary_reserved_capital_raw=40_000_000_000,
            additional_strategies=(
                PortfolioStrategySpec(
                    priority=1,
                    reserved_capital_raw=50_000_000_000,
                    strategy=BuiltinStrategySpec(kind="momentum", strategy_id="mom"),
                ),
            ),
        )
        spec = BacktestSpec(
            run_id="portfolio-contract",
            instrument="BTCUSDT.BINANCE",
            bars_path="bars.json",
            settlement_currency="USDT",
            initial_cash_raw=100_000_000_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="sma"),
            portfolio=basket,
        )
        payload = spec.to_dict()
        self.assertEqual(payload["portfolio"]["additional_strategies"][0]["strategy"]["strategy_id"], "mom")
        self.assertNotIn("portfolio", BacktestSpec(
            run_id="legacy-single",
            instrument="BTCUSDT.BINANCE",
            bars_path="bars.json",
            settlement_currency="USDT",
            initial_cash_raw=100_000_000_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="sma"),
        ).to_dict())


class TopLevelStabilityTest(unittest.TestCase):
    def test_all_python_app_dtos_match_same_named_rust_specs(self):
        python_fields = {
            name: {field.name for field in fields(value)}
            for name, value in vars(app_module).items()
            if isinstance(value, type)
            and value.__module__ == app_module.__name__
            and hasattr(value, "__dataclass_fields__")
        }
        rust_fields = {}
        spec_root = PYTHON_DIR.parent / "crates" / "qx-app" / "src" / "spec"
        for path in spec_root.rglob("*.rs"):
            source = path.read_text(encoding="utf-8")
            for match in re.finditer(
                r"pub struct ([A-Za-z_]\w*)\s*\{([\s\S]*?)\n\}", source
            ):
                fields_found = set()
                skip_next = False
                for line in match.group(2).splitlines():
                    if "#[serde(skip)]" in line:
                        skip_next = True
                        continue
                    field = re.match(r"\s*pub ([a-z_]\w*):", line)
                    if field:
                        if not skip_next:
                            fields_found.add(field.group(1))
                        skip_next = False
                    elif line.strip() and not line.strip().startswith("#["):
                        skip_next = False
                rust_fields[match.group(1)] = fields_found

        shared = sorted(python_fields.keys() & rust_fields.keys())
        self.assertTrue(shared, "Python SDK and Rust app must expose same-named DTOs")
        for name in shared:
            with self.subTest(dto=name):
                self.assertEqual(python_fields[name], rust_fields[name])

    def test_python_backtest_spec_fields_match_the_rust_contract(self):
        from qianxing import BacktestSpec

        source = (
            PYTHON_DIR.parent
            / "crates"
            / "qx-app"
            / "src"
            / "spec"
            / "backtest.rs"
        ).read_text(encoding="utf-8")
        match = re.search(r"pub struct BacktestSpec \{([\s\S]*?)\n\}", source)
        self.assertIsNotNone(match, "Rust BacktestSpec definition must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]+):", match.group(1), re.MULTILINE))
        python_fields = {field.name for field in fields(BacktestSpec)}
        self.assertEqual(python_fields, rust_fields)

    def test_dataset_spec_fields_match_the_rust_contract_and_registered_schema(self):
        import json

        from qianxing import DatasetSpec, DatasetVerdict

        rust_source = (
            PYTHON_DIR.parent / "crates" / "qx-app" / "src" / "spec.rs"
        ).read_text(encoding="utf-8")
        match = re.search(r"pub struct DatasetSpec \{([\s\S]*?)\n\}", rust_source)
        self.assertIsNotNone(match, "Rust DatasetSpec definition must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]+):", match.group(1), re.MULTILINE))
        python_fields = {field.name for field in fields(DatasetSpec)}
        schema_path = PYTHON_DIR.parent / "schemas" / "dataset-spec-v1.json"
        schema = json.loads(schema_path.read_text(encoding="utf-8"))

        self.assertEqual(python_fields, rust_fields)
        self.assertEqual(set(schema["properties"]), rust_fields)
        self.assertEqual(set(schema["required"]), rust_fields)
        self.assertEqual(schema["properties"]["schema_version"]["const"], 1)
        self.assertEqual(schema["properties"]["dataset_id"]["maxLength"], 128)
        self.assertEqual(schema["properties"]["dataset_id"]["pattern"], "^[A-Za-z0-9._-]+$")
        self.assertEqual(schema["properties"]["bars_path"]["pattern"], ".*\\S.*")
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(
            DatasetSpec(dataset_id="dataset-1", bars_path="bars.json").to_dict(),
            {"dataset_id": "dataset-1", "bars_path": "bars.json", "schema_version": 1},
        )
        result_source = (PYTHON_DIR.parent / "crates" / "qx-app" / "src" / "spec" / "results.rs").read_text(encoding="utf-8")
        result_match = re.search(r"pub struct DatasetVerdict \{([\s\S]*?)\n\}", result_source)
        self.assertIsNotNone(result_match, "Rust DatasetVerdict definition must remain discoverable")
        result_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", result_match.group(1), re.MULTILINE))
        result_schema = json.loads((PYTHON_DIR.parent / "schemas" / "dataset-verdict-v1.json").read_text(encoding="utf-8"))
        self.assertEqual({field.name for field in fields(DatasetVerdict)}, result_fields)
        self.assertEqual(set(result_schema["properties"]), result_fields)
        registry = json.loads((PYTHON_DIR.parent / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        entry = next(item for item in registry["entries"] if item["schema_id"] == "dataset-verdict-v1")
        self.assertEqual(entry["producer"], "qx-app")
        self.assertIn("qx-sdk", entry["consumers"])
        from jsonschema import Draft202012Validator, ValidationError

        Draft202012Validator.check_schema(result_schema)
        verdict = {"dataset_id": "ds-1", "instrument": "BTCUSDT.BINANCE", "rows": 0, "content_fingerprint": "barframe:0123456789abcdef", "usable": True, "gaps": []}
        validator = Draft202012Validator(result_schema)
        validator.validate(verdict)
        validator.validate({**verdict, "usable": False, "gaps": ["not enough rows"]})
        with self.assertRaises(ValidationError):
            validator.validate({**verdict, "usable": False, "gaps": []})
        with self.assertRaises(ValidationError):
            validator.validate({**verdict, "unexpected": True})

    def test_builtin_strategy_spec_fields_and_kind_values_match_schema(self):
        import json

        from qianxing import BuiltinStrategySpec

        rust_source = (
            PYTHON_DIR.parent / "crates" / "qx-app" / "src" / "spec.rs"
        ).read_text(encoding="utf-8")
        match = re.search(
            r"pub struct BuiltinStrategySpec \{([\s\S]*?)\n\}", rust_source
        )
        self.assertIsNotNone(
            match, "Rust BuiltinStrategySpec definition must remain discoverable"
        )
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]+):", match.group(1), re.MULTILINE))
        python_fields = {field.name for field in fields(BuiltinStrategySpec)}
        schema_path = PYTHON_DIR.parent / "schemas" / "builtin-strategy-spec-v1.json"
        schema = json.loads(schema_path.read_text(encoding="utf-8"))
        kind_values = set(get_args(get_type_hints(BuiltinStrategySpec)["kind"]))

        self.assertEqual(python_fields, rust_fields)
        self.assertEqual(set(schema["properties"]), rust_fields)
        self.assertEqual(set(schema["required"]), {"kind", "strategy_id", "quantity_raw", "fast_window", "slow_window"})
        self.assertEqual(set(schema["properties"]["kind"]["enum"]), kind_values)
        self.assertFalse(schema["additionalProperties"])

    def test_backtest_spec_schema_fields_match_rust_python_and_nested_specs(self):
        import json

        from qianxing import BacktestSpec

        root = PYTHON_DIR.parent
        schema = json.loads((root / "schemas" / "backtest-spec-v1.json").read_text(encoding="utf-8"))
        spec_path = root / "crates" / "qx-app" / "src" / "spec" / "backtest.rs"
        spec_source = spec_path.read_text(encoding="utf-8")
        backtest_match = re.search(r"pub struct BacktestSpec \{([\s\S]*?)\n\}", spec_source)
        self.assertIsNotNone(backtest_match, "Rust BacktestSpec definition must remain discoverable")
        backtest_fields = set(re.findall(r"^\s*pub ([a-z_]+):", backtest_match.group(1), re.MULTILINE))
        python_fields = {field.name for field in fields(BacktestSpec)}
        self.assertEqual(backtest_fields, python_fields)
        self.assertEqual(set(schema["properties"]), backtest_fields)
        self.assertEqual(
            set(schema["required"]),
            {"schema_version", "run_id", "instrument", "bars_path", "settlement_currency", "initial_cash_raw", "seed", "output_dir", "strategy"},
        )
        self.assertEqual(schema["properties"]["schema_version"]["const"], 1)
        self.assertEqual(schema["properties"]["strategy"]["$ref"], "#/$defs/BuiltinStrategySpec")
        self.assertFalse(schema["additionalProperties"])
        strategy_schema = json.loads(
            (root / "schemas" / "builtin-strategy-spec-v1.json").read_text(encoding="utf-8")
        )
        strategy_definition = {
            key: value
            for key, value in strategy_schema.items()
            if key not in {"$schema", "$id", "title", "description"}
        }
        self.assertEqual(schema["$defs"]["BuiltinStrategySpec"], strategy_definition)

        struct_sources = {
            "BuiltinStrategySpec": "crates/qx-app/src/spec.rs",
            "InstrumentId": "crates/qx-core/src/identity.rs",
            "TradingInstrumentSpec": "crates/qx-core/src/trading.rs",
            "BacktestMarginTier": "crates/qx-app/src/spec/margin.rs",
            "MakerTakerFeeSchedule": "crates/qx-core/src/fee.rs",
            "ExecutionCostRules": "crates/qx-xingban/src/cost_rules.rs",
            "AshareCorporateActionEvent": "crates/qx-xingban/src/ashare.rs",
            "AshareRuleConfig": "crates/qx-xingban/src/ashare.rs",
            "BacktestRiskRules": "crates/qx-app/src/spec/backtest.rs",
            "PortfolioStrategySpec": "crates/qx-app/src/spec/portfolio.rs",
            "PortfolioBacktestConfig": "crates/qx-app/src/spec/portfolio.rs",
            "ProtectiveExitSpec": "crates/qx-app/src/spec/protective_exit.rs",
        }
        for name, relative_path in struct_sources.items():
            source = (root / relative_path).read_text(encoding="utf-8")
            match = re.search(re.escape(f"pub struct {name} {{") + r"([\s\S]*?)\n\}", source)
            self.assertIsNotNone(match, f"Rust {name} definition must remain discoverable")
            rust_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", match.group(1), re.MULTILINE))
            with self.subTest(nested_spec=name):
                self.assertEqual(set(schema["$defs"][name]["properties"]), rust_fields)
        self.assertNotIn("additionalProperties", schema["$defs"]["TradingInstrumentSpec"])
        self.assertNotIn("additionalProperties", schema["$defs"]["AshareRuleConfig"])

    def test_depth_backtest_schema_matches_rust_python_and_registered_nested_contracts(self):
        import json

        from qianxing import BuiltinStrategySpec, DepthBacktestSpec

        root = PYTHON_DIR.parent
        schema = json.loads((root / "schemas" / "depth-backtest-spec-v1.json").read_text(encoding="utf-8"))
        rust_source = (root / "crates" / "qx-app" / "src" / "spec" / "depth.rs").read_text(encoding="utf-8")
        rust_match = re.search(r"pub struct DepthBacktestSpec \{([\s\S]*?)\n\}", rust_source)
        self.assertIsNotNone(rust_match, "Rust DepthBacktestSpec definition must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]+):", rust_match.group(1), re.MULTILINE))
        python_fields = {field.name for field in fields(DepthBacktestSpec)}

        self.assertEqual(rust_fields, python_fields)
        self.assertEqual(set(schema["properties"]), rust_fields)
        self.assertEqual(
            set(schema["required"]),
            {"schema_version", "run_id", "depth_path", "tier", "settlement_currency", "initial_cash_raw", "latency_snapshots", "queue_position_bps", "market_impact_bps", "output_dir", "strategy"},
        )
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(schema["properties"]["queue_position_model"]["$ref"], "#/$defs/QueuePositionModel")
        self.assertEqual(
            {variant["properties"]["kind"]["const"] for variant in schema["$defs"]["QueuePositionModel"]["oneOf"]},
            {"disabled", "fifo_volume_estimate"},
        )
        self.assertEqual(schema["$defs"]["MarketMakingConfig"]["additionalProperties"], False)
        for name, source_path in (
            ("MarketMakingConfig", root / "crates" / "qx-xingban" / "src" / "market_making.rs"),
            ("OrderPolicy", root / "crates" / "qx-core" / "src" / "trading.rs"),
        ):
            source = source_path.read_text(encoding="utf-8")
            nested_match = re.search(re.escape(f"pub struct {name} {{") + r"([\s\S]*?)\n\}", source)
            self.assertIsNotNone(nested_match, f"Rust {name} definition must remain discoverable")
            nested_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", nested_match.group(1), re.MULTILINE))
            with self.subTest(nested_spec=name):
                self.assertEqual(set(schema["$defs"][name]["properties"]), nested_fields)

        strategy_schema = json.loads((root / "schemas" / "builtin-strategy-spec-v1.json").read_text(encoding="utf-8"))
        self.assertEqual(
            schema["$defs"]["BuiltinStrategySpec"],
            {key: value for key, value in strategy_schema.items() if key not in {"$schema", "$id", "title", "description"}},
        )
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        self.assertIn(
            {"schema_id": "depth-backtest-spec-v1", "version": 1, "path": "schemas/depth-backtest-spec-v1.json", "producer": "qx-app", "consumers": ["qx-cli", "qx-api", "qx-python"], "compatibility": "backward", "golden_fixtures": ["crates/qx-app/tests/spec_contract.rs"]},
            registry["entries"],
        )

        legacy = DepthBacktestSpec(
            run_id="legacy-depth",
            depth_path="depth.json",
            tier="l2",
            settlement_currency="USDT",
            initial_cash_raw=10_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="legacy-depth"),
        ).to_dict()
        self.assertNotIn("fee_bps", legacy)
        self.assertNotIn("queue_position_model", legacy)
        explicit_fee = DepthBacktestSpec(
            run_id="explicit-fee",
            depth_path="depth.json",
            tier="l2",
            settlement_currency="USDT",
            initial_cash_raw=10_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="explicit-fee"),
            fee_bps=0,
        ).to_dict()
        self.assertEqual(explicit_fee["fee_bps"], 0)

    def test_multi_leg_backtest_schema_matches_rust_python_and_runtime_constraints(self):
        import json

        from qianxing import BuiltinStrategySpec, MultiLegBacktestOutcome, MultiLegBacktestSpec

        root = PYTHON_DIR.parent
        schema = json.loads((root / "schemas" / "multi-leg-backtest-spec-v1.json").read_text(encoding="utf-8"))
        rust_source = (root / "crates" / "qx-app" / "src" / "spec" / "multi_leg.rs").read_text(encoding="utf-8")
        rust_match = re.search(r"pub struct MultiLegBacktestSpec \{([\s\S]*?)\n\}", rust_source)
        self.assertIsNotNone(rust_match, "Rust MultiLegBacktestSpec definition must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]+):", rust_match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(MultiLegBacktestSpec)}, rust_fields)
        self.assertEqual(set(schema["properties"]), rust_fields)
        self.assertEqual(
            set(schema["required"]),
            {"schema_version", "run_id", "primary_bars_path", "reference_bars_path", "settlement_currency", "primary_initial_cash_raw", "reference_initial_cash_raw", "seed", "output_dir", "strategy"},
        )
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(
            set(schema["$defs"]["MultiLegStrategySpec"]["properties"]["kind"]["enum"]),
            {"pairs_arbitrage", "basis_arbitrage", "cross_venue_arbitrage", "spot_futures_arbitrage"},
        )
        self.assertEqual(schema["$defs"]["FundingRateEvent"]["properties"]["settlement_ts"]["multipleOf"], 28_800_000)
        self.assertEqual(len(schema["allOf"]), 3, "paired manifests and mutually exclusive funding inputs stay documented")

        funding_source = (root / "crates" / "qx-core" / "src" / "ledger" / "mod.rs").read_text(encoding="utf-8")
        funding_match = re.search(r"pub struct FundingRateEvent \{([\s\S]*?)\n\}", funding_source)
        self.assertIsNotNone(funding_match, "Rust FundingRateEvent definition must remain discoverable")
        funding_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", funding_match.group(1), re.MULTILINE))
        self.assertEqual(set(schema["$defs"]["FundingRateEvent"]["properties"]), funding_fields)

        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        self.assertTrue(any(entry["schema_id"] == "multi-leg-backtest-spec-v1" for entry in registry["entries"]))
        result_schema = json.loads((root / "schemas" / "multi-leg-backtest-result-v1.json").read_text(encoding="utf-8"))
        result_source = (root / "crates" / "qx-app" / "src" / "spec" / "multi_leg.rs").read_text(encoding="utf-8")
        result_match = re.search(r"pub struct MultiLegBacktestOutcome \{([\s\S]*?)\n\}", result_source)
        self.assertIsNotNone(result_match, "Rust MultiLegBacktestOutcome definition must remain discoverable")
        result_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", result_match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(MultiLegBacktestOutcome)}, result_fields)
        self.assertEqual(set(result_schema["properties"]), result_fields)
        self.assertNotIn("account_id", result_schema["required"], "Rust skips absent account_id during serialization")
        result_entry = next(item for item in registry["entries"] if item["schema_id"] == "multi-leg-backtest-result-v1")
        self.assertEqual(result_entry["producer"], "qx-app")
        self.assertEqual(result_entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        from jsonschema import Draft202012Validator, ValidationError

        Draft202012Validator.check_schema(result_schema)
        result_payload = {
            "run_id": "multi-leg-v1",
            "primary_instrument": "BTCUSDT.BINANCE",
            "reference_instrument": "ETHUSDT.BINANCE",
            "primary_result_hash": "0123456789abcdef",
            "reference_result_hash": "fedcba9876543210",
            "combined_return_bps": -12,
            "primary_fills": 1,
            "reference_fills": 2,
            "attribution_artifact": "runs/multi-leg.json",
        }
        Draft202012Validator(result_schema).validate(result_payload)
        Draft202012Validator(result_schema).validate({**result_payload, "account_id": None})
        with self.assertRaises(ValidationError):
            Draft202012Validator(result_schema).validate({**result_payload, "unexpected": True})
        payload = MultiLegBacktestSpec(
            run_id="multi-leg-v1",
            primary_bars_path="primary.json",
            reference_bars_path="reference.json",
            settlement_currency="USDT",
            primary_initial_cash_raw=100_000,
            reference_initial_cash_raw=100_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="pairs_arbitrage", strategy_id="multi-leg-v1"),
        ).to_dict()
        self.assertIsNone(payload["primary_market_spec"])
        self.assertIsNone(payload["reference_market_spec"])
        self.assertNotIn("funding_events", payload)
        self.assertEqual(payload["fill_model"], "next_bar_open")

    def test_multi_account_backtest_schemas_match_rust_python_and_bound_batches(self):
        import json
        from jsonschema import Draft202012Validator

        from qianxing import (
            BacktestOutcome,
            DepthBacktestOutcome,
            MultiAccountBacktestResult,
            MultiAccountBacktestSpec,
            MultiAccountDepthBacktestResult,
            MultiAccountDepthBacktestSpec,
            MultiAccountDepthRunResult,
            MultiAccountRunResult,
            JointAccountBacktestResult,
            JointBacktestAccount,
            MultiAccountJointBacktestOutcome,
            MultiAccountJointBacktestSpec,
            VerificationResult,
        )

        root = PYTHON_DIR.parent
        def rust_fields(source, name):
            matched = re.search(re.escape(f"pub struct {name} {{") + r"([\s\S]*?)\n\}", source)
            self.assertIsNotNone(matched, f"Rust {name} definition must remain discoverable")
            return set(re.findall(r"^\s*pub ([a-z_]\w*):", matched.group(1), re.MULTILINE))

        contracts = (
            ("MultiAccountBacktestSpec", "multi-account-backtest-spec-v1", "crates/qx-app/src/spec/multi_account.rs", "crates/qx-app/tests/multi_account_backtest.rs"),
            ("MultiAccountDepthBacktestSpec", "multi-account-depth-backtest-spec-v1", "crates/qx-app/src/spec/multi_account/depth.rs", "crates/qx-app/tests/multi_account_depth_backtest.rs"),
        )
        python_types = {
            "MultiAccountBacktestSpec": MultiAccountBacktestSpec,
            "MultiAccountDepthBacktestSpec": MultiAccountDepthBacktestSpec,
        }
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        for name, schema_id, rust_path, fixture_path in contracts:
            schema = json.loads((root / "schemas" / f"{schema_id}.json").read_text(encoding="utf-8"))
            source = (root / rust_path).read_text(encoding="utf-8")
            rust_field_set = rust_fields(source, name)
            self.assertEqual({field.name for field in fields(python_types[name])}, rust_field_set)
            self.assertEqual(set(schema["properties"]), rust_field_set)
            self.assertEqual(set(schema["required"]), {"schema_version", "batch_id", "runs"})
            self.assertFalse(schema["additionalProperties"])
            self.assertEqual(schema["properties"]["runs"]["minItems"], 2)
            self.assertEqual(schema["properties"]["runs"]["maxItems"], 32)
            entry = next(item for item in registry["entries"] if item["schema_id"] == schema_id)
            self.assertEqual(entry["golden_fixtures"], [fixture_path])
            self.assertEqual(entry["producer"], "qx-app")
            self.assertEqual(entry["consumers"], ["qx-cli", "qx-api", "qx-python"])

        joint_source = (root / "crates" / "qx-app" / "src" / "spec" / "multi_account_joint.rs").read_text(encoding="utf-8")
        joint_contracts = (
            ("JointBacktestAccount", JointBacktestAccount, "#/$defs/JointBacktestAccount"),
            ("MultiAccountJointBacktestSpec", MultiAccountJointBacktestSpec, None),
        )
        joint_schema = json.loads((root / "schemas" / "multi-account-joint-backtest-spec-v1.json").read_text(encoding="utf-8"))
        Draft202012Validator.check_schema(joint_schema)
        for name, python_type, nested_path in joint_contracts:
            with self.subTest(joint_spec=name):
                properties = joint_schema["properties"] if nested_path is None else joint_schema["$defs"]["JointBacktestAccount"]["properties"]
                rust_field_set = rust_fields(joint_source, name)
                self.assertEqual({field.name for field in fields(python_type)}, rust_field_set)
                self.assertEqual(set(properties), rust_field_set)
        self.assertEqual((joint_schema["properties"]["accounts"]["minItems"], joint_schema["properties"]["accounts"]["maxItems"]), (2, 32))
        self.assertEqual(joint_schema["$defs"]["JointBacktestAccount"]["properties"]["markets"]["maxItems"], 32)
        joint_entry = next(item for item in registry["entries"] if item["schema_id"] == "multi-account-joint-backtest-spec-v1")
        self.assertEqual(joint_entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk", "cpp"])
        self.assertIn("python/tests/test_sdk_layering.py", joint_entry["golden_fixtures"])

        result_contracts = (
            (
                "MultiAccountBacktestResult",
                MultiAccountBacktestResult,
                "multi-account-backtest-result-v1",
                "crates/qx-app/src/spec/multi_account.rs",
                "MultiAccountRunResult",
                MultiAccountRunResult,
                "MultiAccountBacktestSummary",
            ),
            (
                "MultiAccountDepthBacktestResult",
                MultiAccountDepthBacktestResult,
                "multi-account-depth-backtest-result-v1",
                "crates/qx-app/src/spec/multi_account/depth.rs",
                "MultiAccountDepthRunResult",
                MultiAccountDepthRunResult,
                "MultiAccountDepthBacktestSummary",
            ),
        )
        for result_name, python_result, schema_id, rust_path, run_name, python_run, summary_name in result_contracts:
            with self.subTest(result=schema_id):
                schema = json.loads((root / "schemas" / f"{schema_id}.json").read_text(encoding="utf-8"))
                entry = next(item for item in registry["entries"] if item["schema_id"] == schema_id)
                source = (root / rust_path).read_text(encoding="utf-8")
                self.assertEqual({field.name for field in fields(python_result)}, rust_fields(source, result_name))
                self.assertEqual(set(schema["properties"]), rust_fields(source, result_name))
                self.assertEqual({field.name for field in fields(python_run)}, rust_fields(source, run_name))
                self.assertEqual(set(schema["$defs"][run_name]["properties"]), rust_fields(source, run_name))
                self.assertEqual(set(schema["$defs"][summary_name]["properties"]), rust_fields(source, summary_name))
                self.assertEqual(schema["properties"]["runs"]["minItems"], 2)
                self.assertEqual(schema["properties"]["runs"]["maxItems"], 32)
                self.assertEqual(entry["producer"], "qx-app")
                self.assertIn("qx-sdk", entry["consumers"])
                self.assertIn("python/tests/test_sdk_layering.py", entry["golden_fixtures"])

        joint_result_schema = json.loads((root / "schemas" / "multi-account-joint-backtest-result-v1.json").read_text(encoding="utf-8"))
        Draft202012Validator.check_schema(joint_result_schema)
        self.assertEqual({field.name for field in fields(MultiAccountJointBacktestOutcome)}, rust_fields(joint_source, "MultiAccountJointBacktestOutcome"))
        self.assertEqual(set(joint_result_schema["properties"]), rust_fields(joint_source, "MultiAccountJointBacktestOutcome"))
        self.assertEqual({field.name for field in fields(JointAccountBacktestResult)}, rust_fields(joint_source, "JointAccountBacktestResult"))
        self.assertEqual(set(joint_result_schema["$defs"]["JointAccountBacktestResult"]["properties"]), rust_fields(joint_source, "JointAccountBacktestResult"))
        joint_result_entry = next(item for item in registry["entries"] if item["schema_id"] == "multi-account-joint-backtest-result-v1")
        self.assertEqual(joint_result_entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk", "cpp"])
        self.assertIn("python/tests/test_sdk_layering.py", joint_result_entry["golden_fixtures"])

        backtest_source = (root / "crates" / "qx-app" / "src" / "spec" / "results.rs").read_text(encoding="utf-8")
        backtest_fields = rust_fields(backtest_source, "BacktestOutcome")
        backtest_schema = json.loads((root / "schemas" / "backtest-outcome-v1.json").read_text(encoding="utf-8"))
        multi_account_schema = json.loads((root / "schemas" / "multi-account-backtest-result-v1.json").read_text(encoding="utf-8"))
        self.assertEqual(set(backtest_schema["properties"]), backtest_fields)
        self.assertEqual(set(multi_account_schema["$defs"]["BacktestOutcome"]["properties"]), backtest_fields)
        self.assertEqual({field.name for field in fields(BacktestOutcome)}, backtest_fields)
        self.assertNotIn("account_id", backtest_schema["required"], "Rust skips absent account_id during serialization")
        self.assertNotIn("account_id", multi_account_schema["$defs"]["BacktestOutcome"]["required"])
        backtest_entry = next(item for item in registry["entries"] if item["schema_id"] == "backtest-outcome-v1")
        self.assertEqual(backtest_entry["producer"], "qx-app")
        self.assertEqual(backtest_entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        self.assertIn("crates/qx-app/tests/spec_contract.rs", backtest_entry["golden_fixtures"])
        from jsonschema import Draft202012Validator, ValidationError

        Draft202012Validator.check_schema(backtest_schema)
        outcome_payload = {
            "run_id": "run-1",
            "instrument": "BTCUSDT.BINANCE",
            "result_hash": "0123456789abcdef",
            "data_fingerprint": "barframe:0123456789abcdef",
            "fills": 1,
            "equity_points": 2,
            "return_bps": -10,
            "max_drawdown_bps": 25,
            "artifacts": {"run_manifest": "r.run.json", "summary": "r.summary.json", "equity": "r.equity.csv", "fills": "r.fills.csv"},
        }
        Draft202012Validator(backtest_schema).validate(outcome_payload)
        Draft202012Validator(backtest_schema).validate({**outcome_payload, "account_id": None})
        with self.assertRaises(ValidationError):
            Draft202012Validator(backtest_schema).validate({**outcome_payload, "unexpected": True})
        verification_source = backtest_source
        verification_fields = rust_fields(verification_source, "VerificationResult")
        verification_schema = json.loads((root / "schemas" / "verification-result-v1.json").read_text(encoding="utf-8"))
        self.assertEqual(set(verification_schema["properties"]), verification_fields)
        self.assertEqual({field.name for field in fields(VerificationResult)}, verification_fields)
        verification_entry = next(item for item in registry["entries"] if item["schema_id"] == "verification-result-v1")
        self.assertEqual(verification_entry["producer"], "qx-app")
        self.assertIn("qx-sdk", verification_entry["consumers"])
        Draft202012Validator.check_schema(verification_schema)
        verification_payload = {
            "run_id": "run-1",
            "result_hash": "0123456789abcdef",
            "data_fingerprint": "barframe:0123456789abcdef",
            "verified": False,
            "checks": ["manifest hashes agree"],
            "mismatches": ["fills count differs"],
        }
        Draft202012Validator(verification_schema).validate(verification_payload)
        Draft202012Validator(verification_schema).validate({**verification_payload, "verified": True, "mismatches": []})
        with self.assertRaises(ValidationError):
            Draft202012Validator(verification_schema).validate({**verification_payload, "unexpected": True})
        depth_source = (root / "crates" / "qx-app" / "src" / "spec" / "depth.rs").read_text(encoding="utf-8")
        depth_fields = rust_fields(depth_source, "DepthBacktestOutcome") - {"display_summary"}
        depth_schema = json.loads((root / "schemas" / "multi-account-depth-backtest-result-v1.json").read_text(encoding="utf-8"))
        self.assertEqual(set(depth_schema["$defs"]["DepthBacktestOutcome"]["properties"]), depth_fields)
        self.assertEqual({field.name for field in fields(DepthBacktestOutcome)}, depth_fields)

        from jsonschema import Draft202012Validator, ValidationError

        summary = {
            "accounts_total": 2,
            "accounts_succeeded": 1,
            "accounts_failed": 1,
            "successful_initial_cash_raw": 1000,
            "successful_fills": 1,
            "weighted_return_bps": None,
            "complete": False,
        }
        artifacts = {
            "run_manifest": "runs/r.run.json",
            "summary": "runs/r.summary.json",
            "equity": "runs/r.equity.csv",
            "fills": "runs/r.fills.csv",
        }
        bar_outcome = {
            "run_id": "r1",
            "instrument": "BTCUSDT.BINANCE",
            "result_hash": "0123456789abcdef",
            "data_fingerprint": "barframe:0123456789abcdef",
            "fills": 1,
            "equity_points": 2,
            "return_bps": -5,
            "max_drawdown_bps": 4,
            "artifacts": artifacts,
            "account_id": "acct-1",
        }
        depth_outcome = {
            **bar_outcome,
            "tier": "l1",
            "snapshots": 2,
        }
        payloads = (
            (
                "multi-account-backtest-result-v1",
                {"batch_id": "batch-1", "settlement_currency": "USDT", "spec_fingerprint": "0123456789abcdef", "runs": [{"account_id": "acct-1", "outcome": bar_outcome, "error_json": None}, {"account_id": "acct-2", "outcome": None, "error_json": "{}"}], "summary": summary},
            ),
            (
                "multi-account-depth-backtest-result-v1",
                {"batch_id": "batch-1", "settlement_currency": "USDT", "spec_fingerprint": "0123456789abcdef", "runs": [{"account_id": "acct-1", "outcome": depth_outcome, "error_json": None}, {"account_id": "acct-2", "outcome": None, "error_json": "{}"}], "summary": summary},
            ),
        )
        for schema_id, payload in payloads:
            with self.subTest(output_schema=schema_id):
                schema = json.loads((root / "schemas" / f"{schema_id}.json").read_text(encoding="utf-8"))
                Draft202012Validator.check_schema(schema)
                validator = Draft202012Validator(schema)
                self.assertTrue(validator.is_valid(payload))
                self.assertFalse(validator.is_valid({**payload, "orphan_field": True}))

    def test_multi_market_backtest_schemas_match_rust_python_contracts(self):
        import json

        from qianxing import (
            MultiMarketBacktestMarket,
            MultiMarketBacktestOutcome,
            MultiMarketBacktestSpec,
            MultiMarketDepthBacktestMarket,
            MultiMarketDepthBacktestOutcome,
            MultiMarketDepthBacktestSpec,
        )

        root = PYTHON_DIR.parent
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        contracts = (
            ("MultiMarketBacktestSpec", MultiMarketBacktestSpec, "multi-market-backtest-spec-v1", "crates/qx-app/src/spec/multi_market.rs", "MultiMarketBacktestMarket", MultiMarketBacktestMarket, "crates/qx-app/tests/multi_market_backtest.rs", 1),
            ("MultiMarketDepthBacktestSpec", MultiMarketDepthBacktestSpec, "multi-market-depth-backtest-spec-v1", "crates/qx-app/src/spec/multi_market_depth.rs", "MultiMarketDepthBacktestMarket", MultiMarketDepthBacktestMarket, "crates/qx-app/tests/multi_market_depth_backtest.rs", 2),
        )
        forbidden = {"pairs_arbitrage", "basis_arbitrage", "cross_venue_arbitrage", "spot_futures_arbitrage"}
        for name, python_type, schema_id, rust_path, nested_name, nested_python_type, fixture, min_markets in contracts:
            schema = json.loads((root / "schemas" / f"{schema_id}.json").read_text(encoding="utf-8"))
            source = (root / rust_path).read_text(encoding="utf-8")
            rust_match = re.search(re.escape(f"pub struct {name} {{") + r"([\s\S]*?)\n\}", source)
            self.assertIsNotNone(rust_match, f"Rust {name} definition must remain discoverable")
            rust_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", rust_match.group(1), re.MULTILINE))
            self.assertEqual({field.name for field in fields(python_type)}, rust_fields)
            self.assertEqual(set(schema["properties"]), rust_fields)
            self.assertEqual(set(schema["required"]), rust_fields)
            self.assertFalse(schema["additionalProperties"])
            market_schema = schema["$defs"][nested_name]
            self.assertEqual(
                {field.name for field in fields(nested_python_type)},
                set(market_schema["properties"]),
            )
            self.assertEqual(market_schema["properties"]["strategies"]["minItems"] if name == "MultiMarketBacktestSpec" else market_schema["required"].count("strategy"), 1)
            markets = schema["properties"]["markets"]
            self.assertEqual(markets["minItems"], min_markets)
            self.assertEqual(markets["maxItems"], 32)
            strategy_kinds = set(schema["$defs"]["BuiltinStrategySpec"]["properties"]["kind"]["enum"])
            self.assertFalse(strategy_kinds & forbidden)
            entry = next(item for item in registry["entries"] if item["schema_id"] == schema_id)
            self.assertEqual(entry["golden_fixtures"], [fixture])
            self.assertEqual(entry["producer"], "qx-app")
        outcome_schema = json.loads((root / "schemas" / "multi-market-backtest-result-v1.json").read_text(encoding="utf-8"))
        outcome_source = (root / "crates" / "qx-app" / "src" / "spec" / "multi_market.rs").read_text(encoding="utf-8")
        outcome_match = re.search(r"pub struct MultiMarketBacktestOutcome \{([\s\S]*?)\n\}", outcome_source)
        self.assertIsNotNone(outcome_match, "Rust MultiMarketBacktestOutcome definition must remain discoverable")
        outcome_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", outcome_match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(MultiMarketBacktestOutcome)}, outcome_fields)
        self.assertEqual(set(outcome_schema["properties"]), outcome_fields)
        outcome_entry = next(item for item in registry["entries"] if item["schema_id"] == "multi-market-backtest-result-v1")
        self.assertEqual(outcome_entry["producer"], "qx-app")
        self.assertEqual(outcome_entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        from jsonschema import Draft202012Validator, ValidationError

        Draft202012Validator.check_schema(outcome_schema)
        outcome_payload = {
            "run_id": "multi-market-v1", "account_id": "account-1",
            "spec_fingerprint": "0123456789abcdef", "data_fingerprint": "fedcba9876543210",
            "shared_result_hash": "0011223344556677", "markets_total": 3,
            "strategies_total": 3, "successful_fills": 2, "equity_points": 64,
            "final_equity_raw": 1000, "return_bps": 12, "max_drawdown_bps": 0,
            "artifact_path": "runs/multi-market.json",
        }
        Draft202012Validator(outcome_schema).validate(outcome_payload)
        with self.assertRaises(ValidationError):
            Draft202012Validator(outcome_schema).validate({**outcome_payload, "unknown": True})

        depth_outcome_schema = json.loads((root / "schemas" / "multi-market-depth-backtest-result-v1.json").read_text(encoding="utf-8"))
        depth_outcome_source = (root / "crates" / "qx-app" / "src" / "spec" / "multi_market_depth.rs").read_text(encoding="utf-8")
        depth_match = re.search(r"pub struct MultiMarketDepthBacktestOutcome \{([\s\S]*?)\n\}", depth_outcome_source)
        self.assertIsNotNone(depth_match, "Rust MultiMarketDepthBacktestOutcome definition must remain discoverable")
        depth_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", depth_match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(MultiMarketDepthBacktestOutcome)}, depth_fields)
        self.assertEqual(set(depth_outcome_schema["properties"]), depth_fields)
        self.assertNotIn("execution_models", depth_outcome_schema["required"], "Rust omits an empty execution model map")
        depth_entry = next(item for item in registry["entries"] if item["schema_id"] == "multi-market-depth-backtest-result-v1")
        self.assertEqual(depth_entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        execution_source = (root / "crates" / "qx-xingban" / "src" / "orderbook" / "audit.rs").read_text(encoding="utf-8")
        for rust_name, schema_name in (("OrderBookExecutionDescriptor", "OrderBookExecutionDescriptor"), ("QueuePositionDescriptor", "QueuePositionDescriptor")):
            descriptor_match = re.search(re.escape(f"pub struct {rust_name} {{") + r"([\s\S]*?)\n\}", execution_source)
            self.assertIsNotNone(descriptor_match, f"Rust {rust_name} definition must remain discoverable")
            descriptor_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", descriptor_match.group(1), re.MULTILINE))
            self.assertEqual(set(depth_outcome_schema["$defs"][schema_name]["properties"]), descriptor_fields)
        Draft202012Validator.check_schema(depth_outcome_schema)

    def test_experiment_input_schemas_match_rust_python_and_bound_candidates(self):
        import json
        from jsonschema import Draft202012Validator, ValidationError

        from qianxing import (
            ExperimentFoldSearchReport,
            ExperimentCandidatePruning,
            ExperimentEarlyPruningPlan,
            ExecutionCostSensitivityResult,
            ExecutionCostSensitivityScenario,
            ExecutionCostSensitivityNeighborResult,
            ExecutionCostSensitivityNeighborhoodResult,
            ExperimentNeighborhoodReport,
            ExperimentParameterRange,
            ExperimentParameterSpace,
            ExperimentSearchPlan,
            ExperimentSearchReport,
            RunDepthExperimentSpec,
            RunExperimentSpec,
            RunMultiLegExperimentSpec,
            RunMultiMarketDepthExperimentSpec,
            RunMultiMarketExperimentSpec,
        )

        root = PYTHON_DIR.parent
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        contracts = (
            ("RunExperimentSpec", RunExperimentSpec, "run-experiment-spec-v1", "crates/qx-app/src/cases/run_experiment.rs", "crates/qx-app/src/cases/run_experiment.rs", "crates/qx-app/src/cases/run_experiment.rs"),
            ("RunDepthExperimentSpec", RunDepthExperimentSpec, "run-depth-experiment-spec-v1", "crates/qx-app/src/cases/run_depth_experiment.rs", "crates/qx-app/src/cases/run_experiment.rs", "crates/qx-app/tests/depth_experiment.rs"),
            ("RunMultiLegExperimentSpec", RunMultiLegExperimentSpec, "run-multi-leg-experiment-spec-v1", "crates/qx-app/src/cases/run_multi_leg_experiment.rs", "crates/qx-app/src/cases/run_experiment.rs", "crates/qx-app/tests/multi_leg_experiment.rs"),
            ("RunMultiMarketExperimentSpec", RunMultiMarketExperimentSpec, "run-multi-market-experiment-spec-v1", "crates/qx-app/src/cases/run_multi_market_experiment/types.rs", "crates/qx-app/src/cases/run_experiment.rs", "crates/qx-app/tests/multi_market_experiment.rs"),
            ("RunMultiMarketDepthExperimentSpec", RunMultiMarketDepthExperimentSpec, "run-multi-market-depth-experiment-spec-v1", "crates/qx-app/src/cases/run_multi_market_depth_experiment/types.rs", "crates/qx-app/src/cases/run_experiment.rs", "crates/qx-app/tests/multi_market_depth_backtest.rs"),
        )
        builtin = json.loads((root / "schemas" / "builtin-strategy-spec-v1.json").read_text(encoding="utf-8"))
        all_kinds = set(builtin["properties"]["kind"]["enum"])
        multileg_kinds = {"pairs_arbitrage", "basis_arbitrage", "cross_venue_arbitrage", "spot_futures_arbitrage"}
        registry_by_id = {entry["schema_id"]: entry for entry in registry["entries"]}
        for name, python_type, schema_id, rust_path, parameter_path, fixture in contracts:
            schema = json.loads((root / "schemas" / f"{schema_id}.json").read_text(encoding="utf-8"))
            source = (root / rust_path).read_text(encoding="utf-8")
            rust_match = re.search(re.escape(f"pub struct {name} {{") + r"([\s\S]*?)\n\}", source)
            self.assertIsNotNone(rust_match, f"Rust {name} definition must remain discoverable")
            rust_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", rust_match.group(1), re.MULTILINE))
            self.assertEqual({field.name for field in fields(python_type)}, rust_fields)
            self.assertEqual(set(schema["properties"]), rust_fields)
            self.assertFalse(schema["additionalProperties"])
            self.assertEqual(schema["properties"]["parameter_space"]["maxItems"], 6)
            self.assertEqual(schema["properties"]["parameter_space"]["items"]["$ref"], "#/$defs/ExperimentParameterSpace")
            allowed_kinds = multileg_kinds if name == "RunMultiLegExperimentSpec" else all_kinds - multileg_kinds
            self.assertEqual(set(schema["properties"]["strategy_kinds"]["items"]["enum"]), allowed_kinds)
            entry = registry_by_id[schema_id]
            self.assertEqual(entry["golden_fixtures"], [fixture])
            self.assertEqual(entry["producer"], "qx-app")

        parameter_source = (root / "crates/qx-app/src/cases/run_experiment.rs").read_text(encoding="utf-8")
        parameter_match = re.search(r"pub struct ExperimentParameterSpace \{([\s\S]*?)\n\}", parameter_source)
        self.assertIsNotNone(parameter_match, "Rust ExperimentParameterSpace definition must remain discoverable")
        parameter_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", parameter_match.group(1), re.MULTILINE))
        for _, _, schema_id, *_ in contracts:
            schema = json.loads((root / "schemas" / f"{schema_id}.json").read_text(encoding="utf-8"))
            self.assertEqual(set(schema["$defs"]["ExperimentParameterSpace"]["properties"]), parameter_fields)
        bar_schema = json.loads((root / "schemas" / "run-experiment-spec-v1.json").read_text(encoding="utf-8"))
        bar_parameters = {
            "fast_window",
            "slow_window",
            "period",
            "threshold_bps",
            "quantity_raw",
            "target_notional_raw",
            "target_available_margin_bps",
            "target_volatility_bps",
            "target_kelly_fraction_bps",
        }
        maker_parameters = {
            "market_making_spread_bps",
            "market_making_quote_qty_raw",
            "market_making_max_inventory_raw",
            "market_making_tick_size_raw",
            "market_making_inventory_skew_bps",
        }
        for _, _, schema_id, *_ in contracts:
            schema = json.loads((root / "schemas" / f"{schema_id}.json").read_text(encoding="utf-8"))
            actual = set(schema["$defs"]["ExperimentParameterSpace"]["properties"]["name"]["enum"])
            expected = bar_parameters | maker_parameters if "depth" in schema_id else bar_parameters
            self.assertEqual(actual, expected, f"{schema_id} parameter names must match Rust's applicable dimensions")
        self.assertEqual(
            set(bar_schema["$defs"]["ExperimentParameterRange"]["properties"]["name"]["enum"]),
            bar_parameters,
        )
        for name in ("target_notional_raw", "target_available_margin_bps", "target_volatility_bps"):
            Draft202012Validator(bar_schema["$defs"]["ExperimentParameterSpace"]).validate(
                ExperimentParameterSpace(name, (1_000, 2_000)).to_dict()
            )
        self.assertEqual(set(bar_schema["$defs"]["ExperimentSplitPlan"]["properties"]), {"train_bars", "test_bars", "walk_forward", "step_bars", "purge_bars", "embargo_bars"})
        self.assertEqual(set(bar_schema["properties"]["baselines"]["items"]["enum"]), {"buy-and-hold", "zero-signal", "none"})
        cost_scenario = ExecutionCostSensitivityScenario(
            name="higher-fees",
            fill_model="best_price",
            execution_cost_rules={
                "name": "stress", "maker_bp": 10, "taker_bp": 20,
                "latency_base_ns": 0, "latency_insert_ns": 0,
            },
            max_training_return_degradation_bps=25,
            max_neighborhood_training_return_degradation_bps=40,
        )
        cost_scenario_payload = cost_scenario.to_dict()
        self.assertEqual(cost_scenario_payload["max_training_return_degradation_bps"], 25)
        self.assertEqual(cost_scenario_payload["max_neighborhood_training_return_degradation_bps"], 40)
        Draft202012Validator(bar_schema["$defs"]["ExecutionCostSensitivityScenario"]).validate(cost_scenario_payload)
        with self.assertRaises(ValidationError):
            Draft202012Validator(bar_schema["$defs"]["ExecutionCostSensitivityScenario"]).validate(
                {**cost_scenario_payload, "max_training_return_degradation_bps": 10001}
            )
        with self.assertRaises(ValidationError):
            Draft202012Validator(bar_schema["$defs"]["ExecutionCostSensitivityScenario"]).validate(
                {**cost_scenario_payload, "max_neighborhood_training_return_degradation_bps": 10001}
            )
        scenario_source = (root / "crates" / "qx-app" / "src" / "cases" / "run_experiment.rs").read_text(encoding="utf-8")
        scenario_match = re.search(r"pub struct ExecutionCostSensitivityScenario \{([\s\S]*?)\n\}", scenario_source)
        self.assertIsNotNone(scenario_match, "Rust cost scenario definition must remain discoverable")
        rust_scenario_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", scenario_match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(ExecutionCostSensitivityScenario)}, rust_scenario_fields)
        self.assertEqual(set(bar_schema["$defs"]["ExecutionCostSensitivityScenario"]["properties"]), rust_scenario_fields)
        search_plan = ExperimentSearchPlan(algorithm="tpe", seed=2026, budget=8)
        self.assertEqual(search_plan.to_dict(), {"algorithm": "tpe", "seed": 2026, "budget": 8})
        self.assertEqual(
            ExperimentSearchPlan(algorithm="tpe_kde", seed=2026, budget=8).to_dict(),
            {"algorithm": "tpe_kde", "seed": 2026, "budget": 8},
        )
        Draft202012Validator(bar_schema["$defs"]["ExperimentSearchPlan"]).validate(search_plan.to_dict())
        constrained_search = ExperimentSearchPlan(algorithm="tpe", seed=2026, budget=8, max_training_drawdown_bps=1800)
        self.assertEqual(constrained_search.to_dict()["max_training_drawdown_bps"], 1800)
        Draft202012Validator(bar_schema["$defs"]["ExperimentSearchPlan"]).validate(constrained_search.to_dict())
        early_search = ExperimentSearchPlan(
            algorithm="tpe", seed=2026, budget=8, max_training_drawdown_bps=1800,
            early_pruning=ExperimentEarlyPruningPlan(min_training_bars=25),
            robustness_subwindows=3,
            minimum_training_robustness_score_bps=25,
            neighborhood_radius=2,
        )
        self.assertEqual(
            early_search.to_dict()["early_pruning"], {"min_training_bars": 25}
        )
        self.assertEqual(early_search.to_dict()["robustness_subwindows"], 3)
        self.assertEqual(early_search.to_dict()["minimum_training_robustness_score_bps"], 25)
        self.assertEqual(early_search.to_dict()["neighborhood_radius"], 2)
        Draft202012Validator(bar_schema["$defs"]["ExperimentSearchPlan"]).validate(early_search.to_dict())
        with self.assertRaises(ValidationError):
            Draft202012Validator(bar_schema["$defs"]["ExperimentSearchPlan"]).validate(
                {
                    "algorithm": "tpe",
                    "seed": 2026,
                    "budget": 8,
                    "minimum_training_robustness_score_bps": 25,
                }
            )
        with self.assertRaises(ValidationError):
            Draft202012Validator(bar_schema["$defs"]["ExperimentSearchPlan"]).validate(
                {**early_search.to_dict(), "neighborhood_radius": 0}
            )
        fold_search = ExperimentSearchPlan(algorithm="tpe", seed=2026, budget=8, fold_local_search=True)
        self.assertIs(fold_search.to_dict()["fold_local_search"], True)
        Draft202012Validator(bar_schema["$defs"]["ExperimentSearchPlan"]).validate(fold_search.to_dict())
        continuous_search = ExperimentSearchPlan(
            algorithm="tpe_kde",
            seed=2026,
            budget=8,
            continuous_parameter_space=(
                ExperimentParameterRange("period", 2, 60, 2),
                ExperimentParameterRange("target_notional_raw", 1_000, 2_000, 1_000),
                ExperimentParameterRange("target_available_margin_bps", 1_000, 2_000, 1_000),
                ExperimentParameterRange("target_volatility_bps", 1_000, 2_000, 1_000),
            ),
        )
        self.assertEqual(
            continuous_search.to_dict()["continuous_parameter_space"],
            [
                {"name": "period", "min": 2, "max": 60, "step": 2},
                {"name": "target_notional_raw", "min": 1_000, "max": 2_000, "step": 1_000},
                {"name": "target_available_margin_bps", "min": 1_000, "max": 2_000, "step": 1_000},
                {"name": "target_volatility_bps", "min": 1_000, "max": 2_000, "step": 1_000},
            ],
        )
        search_contract = {
            "$schema": bar_schema["$schema"],
            "$defs": bar_schema["$defs"],
            "$ref": "#/$defs/ExperimentSearchPlan",
        }
        Draft202012Validator(search_contract).validate(continuous_search.to_dict())
        report = {
            "algorithm": "tpe",
            "seed": 2026,
            "budget": 5,
            "early_pruning": {"min_training_bars": 25},
            "robustness_subwindows": 2,
            "minimum_training_robustness_score_bps": 25,
            "cost_robustness_passed": False,
            "neighborhood_cost_robustness_passed": False,
            "trials": [{
                "ordinal": 1,
                "proposal": "tpe_random_startup",
                "parameters": {"period": 8},
                "training_score_bps": 120,
                "training_robustness_score_bps": 40,
            }],
            "neighborhood": {
                "center_ordinal": 1,
                "center_parameters": {"period": 8},
                "radius": 2,
                "possible_neighbors": 2,
                "evaluated_neighbors": 2,
                "successful_neighbors": 2,
                "failed_neighbors": 0,
                "mean_training_score_bps": 80,
                "min_training_score_bps": 40,
                "max_training_score_bps": 120,
                "assessment": "stable_positive",
            },
        }
        result_schema = json.loads((root / "schemas" / "run-experiment-result-v1.json").read_text(encoding="utf-8"))
        from referencing import Registry, Resource

        local_schemas = [
            json.loads(path.read_text(encoding="utf-8"))
            for path in (root / "schemas").glob("*.json")
        ]
        local_registry = Registry().with_resources(
            (schema["$id"], Resource.from_contents(schema))
            for schema in local_schemas if "$id" in schema
        )
        search_schema = {
            "$schema": result_schema["$schema"],
            "$id": result_schema["$id"],
            "$defs": result_schema["$defs"],
            **result_schema["$defs"]["SearchReport"],
        }
        search_validator = Draft202012Validator(search_schema, registry=local_registry)
        search_validator.validate(report)
        parsed = ExperimentSearchReport.from_dict(report)
        self.assertEqual(parsed.trials[0].parameters, {"period": 8})
        self.assertEqual(parsed.trials[0].training_score_bps, 120)
        self.assertEqual(parsed.trials[0].training_robustness_score_bps, 40)
        self.assertEqual(parsed.neighborhood.assessment, "stable_positive")
        self.assertEqual(parsed.neighborhood.radius, 2)
        self.assertEqual(parsed.early_pruning.min_training_bars, 25)
        self.assertEqual(parsed.robustness_subwindows, 2)
        self.assertEqual(parsed.minimum_training_robustness_score_bps, 25)
        self.assertIs(parsed.cost_robustness_passed, False)
        self.assertIs(parsed.neighborhood_cost_robustness_passed, False)
        search_source = (root / "crates" / "qx-app" / "src" / "cases" / "experiment_search.rs").read_text(encoding="utf-8")
        search_match = re.search(r"pub struct ExperimentSearchReport \{([\s\S]*?)\n\}", search_source)
        self.assertIsNotNone(search_match, "Rust ExperimentSearchReport must remain discoverable")
        rust_search_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", search_match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(ExperimentSearchReport)}, rust_search_fields)
        self.assertEqual(set(result_schema["$defs"]["SearchReport"]["properties"]), rust_search_fields)
        pruned_candidate = {
            "ordinal": 1,
            "parameters": {"period": 8},
            "outcome": None,
            "error": None,
            "pruning": {
                "processed_bars": 25,
                "observed_max_drawdown_bps": 1801,
                "max_training_drawdown_bps": 1800,
            },
        }
        candidate_schema = {
            "$schema": result_schema["$schema"],
            "$id": result_schema["$id"],
            "$defs": result_schema["$defs"],
            "$ref": "#/$defs/Candidate",
        }
        Draft202012Validator(candidate_schema, registry=local_registry).validate(pruned_candidate)
        self.assertEqual(
            ExperimentCandidatePruning.from_dict(pruned_candidate["pruning"]).processed_bars,
            25,
        )
        report["fold_searches"] = [{
            "fold": 1, "seed": 2026, "train_range": [0, 20], "test_range": [20, 30],
            "trials": report["trials"],
            "candidates": [{"ordinal": 1, "parameters": {"period": 8}, "outcome": None, "error": None}],
            "selected_ordinal": 1, "selected_run_manifest": "fold-0001/trial-0001.run.json",
            "neighborhood": report["neighborhood"],
            "cost_sensitivity": [{
                "selected_ordinal": 1, "name": "higher-fees", "fill_model": "best_price",
                "return_delta_bps": -25, "drawdown_delta_bps": 10,
                "max_training_return_degradation_bps": 12,
                "cost_robustness_passed": False,
                "max_neighborhood_training_return_degradation_bps": 40,
                "neighborhood_cost_robustness": {
                    "radius": 1, "possible_neighbors": 1, "evaluated_neighbors": 1,
                    "successful_neighbors": 0, "passed_neighbors": 0,
                    "complete": True, "passed": False,
                    "candidates": [{
                        "ordinal": 2, "parameters": {"period": 10},
                        "fold_deltas": [], "cost_robustness_passed": False,
                        "outcome": None, "error": "baseline candidate failed",
                    }],
                },
                "fold_deltas": [{"fold": 1, "train_range": [0, 20], "test_range": [20, 30],
                    "training_return_delta_bps": -12, "training_drawdown_delta_bps": 3,
                    "out_of_sample_return_delta_bps": -25, "out_of_sample_drawdown_delta_bps": 10}],
                "outcome": None, "error": None,
                "execution_cost_rules": {"name": "stress", "maker_bp": 10, "taker_bp": 20,
                    "latency_base_ns": 0, "latency_insert_ns": 0},
            }],
        }]
        search_validator.validate(report)
        parsed_fold = ExperimentSearchReport.from_dict(report).fold_searches[0]
        self.assertEqual(parsed_fold.trials[0].training_score_bps, 120)
        self.assertEqual(parsed_fold.selected_run_manifest, "fold-0001/trial-0001.run.json")
        self.assertEqual(parsed_fold.neighborhood.assessment, "stable_positive")
        self.assertEqual(parsed_fold.cost_sensitivity[0].return_delta_bps, -25)
        self.assertEqual(parsed_fold.cost_sensitivity[0].max_training_return_degradation_bps, 12)
        self.assertIs(parsed_fold.cost_sensitivity[0].cost_robustness_passed, False)
        neighborhood = parsed_fold.cost_sensitivity[0].neighborhood_cost_robustness
        self.assertIsNotNone(neighborhood)
        self.assertEqual(neighborhood.possible_neighbors, 1)
        self.assertFalse(neighborhood.passed)
        self.assertEqual(neighborhood.candidates[0].ordinal, 2)
        self.assertEqual(neighborhood.candidates[0].parameters, {"period": 10})
        result_source = (root / "crates" / "qx-app" / "src" / "cases" / "run_experiment.rs").read_text(encoding="utf-8")
        result_match = re.search(r"pub struct ExecutionCostSensitivityResult \{([\s\S]*?)\n\}", result_source)
        self.assertIsNotNone(result_match, "Rust cost result definition must remain discoverable")
        rust_result_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", result_match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(ExecutionCostSensitivityResult)}, rust_result_fields)
        self.assertEqual(set(result_schema["$defs"]["CostSensitivityResult"]["properties"]), rust_result_fields)
        for python_type, rust_name, schema_name in (
            (ExecutionCostSensitivityNeighborResult, "ExecutionCostSensitivityNeighborResult", "CostSensitivityNeighborResult"),
            (ExecutionCostSensitivityNeighborhoodResult, "ExecutionCostSensitivityNeighborhoodResult", "CostSensitivityNeighborhood"),
        ):
            source = (root / "crates" / "qx-app" / "src" / "cases" / "run_experiment.rs").read_text(encoding="utf-8")
            match = re.search(re.escape(f"pub struct {rust_name} {{") + r"([\s\S]*?)\n\}", source)
            self.assertIsNotNone(match, f"Rust {rust_name} must remain discoverable")
            rust_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", match.group(1), re.MULTILINE))
            self.assertEqual({field.name for field in fields(python_type)}, rust_fields)
            self.assertEqual(set(result_schema["$defs"][schema_name]["properties"]), rust_fields)
        delta = parsed_fold.cost_sensitivity[0].fold_deltas[0]
        self.assertEqual(delta.train_range, (0, 20))
        self.assertEqual(delta.training_return_delta_bps, -12)
        self.assertEqual(delta.out_of_sample_drawdown_delta_bps, 10)
        fold_source = (root / "crates" / "qx-app" / "src" / "cases" / "experiment_search.rs").read_text(encoding="utf-8")
        fold_match = re.search(r"pub struct ExperimentFoldSearchReport \{([\s\S]*?)\n\}", fold_source)
        self.assertIsNotNone(fold_match, "Rust ExperimentFoldSearchReport must remain discoverable")
        rust_fold_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", fold_match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(ExperimentFoldSearchReport)}, rust_fold_fields)
        self.assertEqual(set(result_schema["$defs"]["FoldSearchReport"]["properties"]), rust_fold_fields)
        self.assertEqual(
            set(result_schema["$defs"]["SearchNeighborhood"]["properties"]["assessment"]["enum"]),
            {
                "incomplete_neighborhood",
                "no_neighbors",
                "no_successful_neighbors",
                "stable_positive",
                "fragile_positive",
                "drawdown_budget_exceeded",
                "drawdown_budget_unverified",
                "no_positive_training_edge",
            },
        )
        neighborhood_source = (root / "crates" / "qx-app" / "src" / "cases" / "experiment_search.rs").read_text(encoding="utf-8")
        neighborhood_match = re.search(r"pub struct ExperimentNeighborhoodReport \{([\s\S]*?)\n\}", neighborhood_source)
        self.assertIsNotNone(neighborhood_match, "Rust ExperimentNeighborhoodReport must remain discoverable")
        rust_neighborhood_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", neighborhood_match.group(1), re.MULTILINE))
        schema_neighborhood = result_schema["$defs"]["SearchNeighborhood"]
        self.assertEqual({field.name for field in fields(ExperimentNeighborhoodReport)}, rust_neighborhood_fields)
        self.assertEqual(set(schema_neighborhood["properties"]), rust_neighborhood_fields)
        legacy_report = {key: value for key, value in report.items() if key not in {"neighborhood", "fold_searches", "early_pruning"}}
        legacy_report["neighborhood"] = {
            key: value for key, value in report["neighborhood"].items() if key != "assessment"
        }
        search_validator.validate(legacy_report)
        self.assertIsNone(ExperimentSearchReport.from_dict(legacy_report).neighborhood.assessment)
        self.assertEqual(ExperimentSearchReport.from_dict(legacy_report).fold_searches, ())

    def test_multi_leg_manifest_fields_match_rust_and_preserve_legacy_payload(self):
        from qianxing import BuiltinStrategySpec, MultiLegBacktestSpec

        source = (
            PYTHON_DIR.parent
            / "crates"
            / "qx-app"
            / "src"
            / "spec"
            / "multi_leg.rs"
        ).read_text(encoding="utf-8")
        match = re.search(r"pub struct MultiLegBacktestSpec \{([\s\S]*?)\n\}", source)
        self.assertIsNotNone(match, "Rust MultiLegBacktestSpec must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]+):", match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(MultiLegBacktestSpec)}, rust_fields)

        spec = MultiLegBacktestSpec(
            run_id="multi-leg-manifests",
            primary_bars_path="primary.json",
            reference_bars_path="reference.json",
            settlement_currency="USDT",
            primary_initial_cash_raw=100_000,
            reference_initial_cash_raw=100_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="pairs_arbitrage", strategy_id="pair"),
            primary_dataset_manifest_path="primary-manifest.json",
            reference_dataset_manifest_path="reference-manifest.json",
        )
        payload = spec.to_dict()
        self.assertEqual(payload["primary_dataset_manifest_path"], "primary-manifest.json")
        self.assertEqual(payload["reference_dataset_manifest_path"], "reference-manifest.json")

        legacy = MultiLegBacktestSpec(
            run_id="legacy-multi-leg",
            primary_bars_path="primary.json",
            reference_bars_path="reference.json",
            settlement_currency="USDT",
            primary_initial_cash_raw=100_000,
            reference_initial_cash_raw=100_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="pairs_arbitrage", strategy_id="pair"),
        ).to_dict()
        self.assertNotIn("primary_dataset_manifest_path", legacy)
        self.assertNotIn("reference_dataset_manifest_path", legacy)

    def test_multi_market_experiment_result_schema_matches_rust_python_contracts(self):
        import json
        import re
        from dataclasses import fields
        from jsonschema import Draft202012Validator
        from qianxing import RunMultiMarketExperimentResult

        root = PYTHON_DIR.parent
        schema = json.loads((root / "schemas" / "run-multi-market-experiment-result-v1.json").read_text(encoding="utf-8"))
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        source = (root / "crates" / "qx-app" / "src" / "cases" / "run_multi_market_experiment" / "types.rs").read_text(encoding="utf-8")
        match = re.search(r"pub struct RunMultiMarketExperimentResult \{([\s\S]*?)\n\}", source)
        self.assertIsNotNone(match, "Rust RunMultiMarketExperimentResult definition must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(RunMultiMarketExperimentResult)}, rust_fields)
        self.assertEqual(set(schema["properties"]), rust_fields)
        self.assertEqual(set(schema["required"]), rust_fields)
        self.assertFalse(schema["additionalProperties"])
        entry = next(item for item in registry["entries"] if item["schema_id"] == "run-multi-market-experiment-result-v1")
        self.assertEqual(entry["producer"], "qx-app")
        self.assertEqual(entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        Draft202012Validator.check_schema(schema)

    def test_multi_market_depth_experiment_result_schema_matches_rust_python_contracts(self):
        import json
        import re
        from dataclasses import fields
        from jsonschema import Draft202012Validator
        from qianxing import RunMultiMarketDepthExperimentResult

        root = PYTHON_DIR.parent
        schema = json.loads((root / "schemas" / "run-multi-market-depth-experiment-result-v1.json").read_text(encoding="utf-8"))
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        source = (root / "crates" / "qx-app" / "src" / "cases" / "run_multi_market_depth_experiment" / "types.rs").read_text(encoding="utf-8")
        match = re.search(r"pub struct RunMultiMarketDepthExperimentResult \{([\s\S]*?)\n\}", source)
        self.assertIsNotNone(match, "Rust RunMultiMarketDepthExperimentResult definition must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(RunMultiMarketDepthExperimentResult)}, rust_fields)
        self.assertEqual(set(schema["properties"]), rust_fields)
        self.assertEqual(set(schema["required"]), rust_fields)
        self.assertFalse(schema["additionalProperties"])
        entry = next(item for item in registry["entries"] if item["schema_id"] == "run-multi-market-depth-experiment-result-v1")
        self.assertEqual(entry["producer"], "qx-app")
        self.assertEqual(entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        Draft202012Validator.check_schema(schema)

    def test_run_experiment_result_schema_matches_rust_python_contracts(self):
        import json
        import re
        from dataclasses import fields
        from jsonschema import Draft202012Validator
        from qianxing import RunExperimentResult

        root = PYTHON_DIR.parent
        schema = json.loads((root / "schemas" / "run-experiment-result-v1.json").read_text(encoding="utf-8"))
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        source = (root / "crates" / "qx-app" / "src" / "cases" / "run_experiment.rs").read_text(encoding="utf-8")
        match = re.search(r"pub struct RunExperimentResult \{([\s\S]*?)\n\}", source)
        self.assertIsNotNone(match, "Rust RunExperimentResult definition must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(RunExperimentResult)}, rust_fields)
        self.assertEqual(set(schema["properties"]), rust_fields)
        self.assertEqual(set(schema["required"]), {"schema_version", "experiment_id", "spec_fingerprint", "total_candidates", "completed_candidates", "succeeded_candidates", "failed_candidates", "candidates", "comparison", "artifact_path"})
        self.assertFalse(schema["additionalProperties"])
        fold_properties = schema["$defs"]["FoldReport"]["properties"]
        self.assertEqual(fold_properties["purge_range"]["$ref"], "#/$defs/TimeRange")
        self.assertEqual(fold_properties["embargo_range"]["$ref"], "#/$defs/TimeRange")
        self.assertNotIn("purge_range", schema["$defs"]["FoldReport"]["required"])
        self.assertNotIn("embargo_range", schema["$defs"]["FoldReport"]["required"])
        entry = next(item for item in registry["entries"] if item["schema_id"] == "run-experiment-result-v1")
        self.assertEqual(entry["producer"], "qx-app")
        self.assertEqual(entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        Draft202012Validator.check_schema(schema)

    def test_run_depth_experiment_result_schema_matches_rust_python_contracts(self):
        import json
        import re
        from dataclasses import fields
        from jsonschema import Draft202012Validator
        from qianxing import RunDepthExperimentResult

        root = PYTHON_DIR.parent
        schema = json.loads((root / "schemas" / "run-depth-experiment-result-v1.json").read_text(encoding="utf-8"))
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        source = (root / "crates" / "qx-app" / "src" / "cases" / "run_depth_experiment.rs").read_text(encoding="utf-8")
        match = re.search(r"pub struct RunDepthExperimentResult \{([\s\S]*?)\n\}", source)
        self.assertIsNotNone(match, "Rust RunDepthExperimentResult definition must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(RunDepthExperimentResult)}, rust_fields)
        self.assertEqual(set(schema["properties"]), rust_fields)
        self.assertEqual(set(schema["required"]), rust_fields)
        self.assertFalse(schema["additionalProperties"])
        entry = next(item for item in registry["entries"] if item["schema_id"] == "run-depth-experiment-result-v1")
        self.assertEqual(entry["producer"], "qx-app")
        self.assertEqual(entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        Draft202012Validator.check_schema(schema)

    def test_run_multi_leg_experiment_result_schema_matches_rust_python_contracts(self):
        import json
        import re
        from dataclasses import fields
        from jsonschema import Draft202012Validator
        from qianxing import RunMultiLegExperimentResult

        root = PYTHON_DIR.parent
        schema = json.loads((root / "schemas" / "run-multi-leg-experiment-result-v1.json").read_text(encoding="utf-8"))
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        source = (root / "crates" / "qx-app" / "src" / "cases" / "run_multi_leg_experiment.rs").read_text(encoding="utf-8")
        match = re.search(r"pub struct RunMultiLegExperimentResult \{([\s\S]*?)\n\}", source)
        self.assertIsNotNone(match, "Rust RunMultiLegExperimentResult definition must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", match.group(1), re.MULTILINE))
        self.assertEqual({field.name for field in fields(RunMultiLegExperimentResult)}, rust_fields)
        self.assertEqual(set(schema["properties"]), rust_fields)
        self.assertEqual(set(schema["required"]), rust_fields)
        self.assertFalse(schema["additionalProperties"])
        entry = next(item for item in registry["entries"] if item["schema_id"] == "run-multi-leg-experiment-result-v1")
        self.assertEqual(entry["producer"], "qx-app")
        self.assertEqual(entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        Draft202012Validator.check_schema(schema)

    def test_multi_market_market_fields_match_rust_and_serialize_manifest(self):
        from qianxing import MultiMarketBacktestMarket

        source = (
            PYTHON_DIR.parent
            / "crates"
            / "qx-app"
            / "src"
            / "spec"
            / "multi_market.rs"
        ).read_text(encoding="utf-8")
        match = re.search(
            r"pub struct MultiMarketBacktestMarket \{([\s\S]*?)\n\}", source
        )
        self.assertIsNotNone(match, "Rust MultiMarketBacktestMarket must remain discoverable")
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]+):", match.group(1), re.MULTILINE))
        python_fields = {field.name for field in fields(MultiMarketBacktestMarket)}
        self.assertEqual(python_fields, rust_fields)

        market = MultiMarketBacktestMarket(
            instrument="000001.SZSE",
            bars_path="bars.json",
            priority=0,
            strategies=(),
            dataset_manifest_path="manifest.json",
        )
        self.assertEqual(market.to_dict()["dataset_manifest_path"], "manifest.json")
        legacy_market = MultiMarketBacktestMarket(
            instrument="BTCUSDT.BINANCE",
            bars_path="bars.json",
            priority=0,
            strategies=(),
        )
        self.assertNotIn("dataset_manifest_path", legacy_market.to_dict())

    def test_depth_backtest_dtos_match_rust_fields_and_serialize_queue_model(self):
        from qianxing import (
            BuiltinStrategySpec,
            DepthBacktestSpec,
            InstrumentId,
            MultiMarketDepthBacktestMarket,
            QueuePositionFifoVolumeEstimate,
            TradingInstrumentSpec,
        )

        app_spec = PYTHON_DIR.parent / "crates" / "qx-app" / "src" / "spec"

        def rust_fields(path, type_name: str) -> set[str]:
            source = path.read_text(encoding="utf-8")
            match = re.search(rf"pub struct {type_name} \{{([\s\S]*?)\n\}}", source)
            self.assertIsNotNone(match, f"Rust {type_name} definition must remain discoverable")
            return set(re.findall(r"^\s*pub ([a-z_]+):", match.group(1), re.MULTILINE))

        self.assertEqual(
            {field.name for field in fields(DepthBacktestSpec)},
            rust_fields(app_spec / "depth.rs", "DepthBacktestSpec"),
        )
        market_type_fields = {field.name for field in fields(MultiMarketDepthBacktestMarket)}
        self.assertEqual(
            market_type_fields,
            rust_fields(app_spec / "multi_market_depth.rs", "MultiMarketDepthBacktestMarket"),
        )

        queue = QueuePositionFifoVolumeEstimate(volume_advance_bps=7500)
        market_spec = TradingInstrumentSpec(
            instrument=InstrumentId("BTCUSDT", "BINANCE"),
            product="spot",
            base_currency="BTC",
            quote_currency="USDT",
            settlement_currency="USDT",
            contract_size=1_000_000_000,
            linear=True,
            inverse=False,
            price_tick=1_000,
            qty_step=1,
            min_qty=1,
            max_leverage=1,
            maintenance_margin_bps=0,
            valid_from=0,
        )
        market = MultiMarketDepthBacktestMarket(
            instrument="BTCUSDT.BINANCE",
            depth_path="depth.json",
            priority=0,
            market_spec=market_spec,
            dataset_manifest_path="depth-quality.json",
            queue_position_model=queue,
        )
        market_payload = market.to_dict()
        self.assertEqual(market_payload["dataset_manifest_path"], "depth-quality.json")
        self.assertEqual(
            market_payload["queue_position_model"],
            {"kind": "fifo_volume_estimate", "volume_advance_bps": 7500},
        )
        legacy_market = MultiMarketDepthBacktestMarket(
            instrument="BTCUSDT.BINANCE",
            depth_path="depth.json",
            priority=0,
            market_spec=market_spec,
        ).to_dict()
        self.assertNotIn("dataset_manifest_path", legacy_market)
        self.assertNotIn("queue_position_model", legacy_market)

        spec = DepthBacktestSpec(
            run_id="typed-depth",
            depth_path="depth.json",
            tier="l2",
            settlement_currency="USDT",
            initial_cash_raw=10_000_000_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="typed-depth"),
            manifest_run_id="depth-backtest:l2:sma_cross:BTCUSDT.BINANCE",
            queue_position_model=queue,
            quality_manifest_path="depth-quality.json",
        )
        payload = spec.to_dict()
        self.assertEqual(payload["queue_position_model"], market_payload["queue_position_model"])
        self.assertEqual(payload["quality_manifest_path"], "depth-quality.json")
        legacy_payload = DepthBacktestSpec(
            run_id="legacy-depth",
            depth_path="depth.json",
            tier="l2",
            settlement_currency="USDT",
            initial_cash_raw=10_000_000_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="legacy-depth"),
        ).to_dict()
        self.assertNotIn("queue_position_model", legacy_payload)
        self.assertNotIn("quality_manifest_path", legacy_payload)

    def test_backtest_spec_serializes_all_runtime_optional_bindings(self):
        from qianxing import BacktestSpec, BuiltinStrategySpec

        spec = BacktestSpec(
            run_id="runtime-bindings",
            instrument="000001.SZSE",
            bars_path="bars.json",
            settlement_currency="CNY",
            initial_cash_raw=100_000_000_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="ashare"),
            margin_model={"no_margin": {}},
            execution_cost_rules={"maker_bp": 1, "taker_bp": 2},
            ashare_rules={"version": "ashare-rules-v1"},
            fill_model="best_price",
            max_bar_volume_participation_bps=2500,
            risk_rules={"version": "runtime-risk-v1", "no_short": True},
            allow_short=False,
        )
        payload = spec.to_dict()
        for field_name in (
            "margin_model",
            "execution_cost_rules",
            "ashare_rules",
            "fill_model",
            "max_bar_volume_participation_bps",
            "risk_rules",
            "allow_short",
        ):
            self.assertEqual(payload[field_name], getattr(spec, field_name))
        legacy_payload = BacktestSpec(
            run_id="legacy",
            instrument="BTCUSDT.BINANCE",
            bars_path="bars.json",
            settlement_currency="USDT",
            initial_cash_raw=100_000_000_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="legacy"),
        ).to_dict()
        self.assertFalse(set(legacy_payload) & {
            "margin_model",
            "execution_cost_rules",
            "ashare_rules",
            "fill_model",
            "max_bar_volume_participation_bps",
            "risk_rules",
            "allow_short",
        })

    def test_runtime_binding_nested_objects_have_typed_wire_serializers(self):
        from qianxing import (
            BacktestExecutionCostRules,
            BacktestNoMargin,
            BacktestRiskRules,
            BacktestSpec,
            BuiltinStrategySpec,
        )

        spec = BacktestSpec(
            run_id="typed-runtime-bindings",
            instrument="000001.SZSE",
            bars_path="bars.json",
            settlement_currency="CNY",
            initial_cash_raw=100_000_000_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="typed"),
            margin_model=BacktestNoMargin(),
            execution_cost_rules=BacktestExecutionCostRules(
                name="ashare-cost-v1",
                maker_bp=1,
                taker_bp=2,
                latency_base_ns=3,
                latency_insert_ns=4,
            ),
            risk_rules=BacktestRiskRules(max_qty_raw=500, no_short=True),
        )

        payload = spec.to_dict()
        self.assertEqual(payload["margin_model"], {"kind": "no_margin"})
        self.assertEqual(
            payload["execution_cost_rules"],
            {
                "name": "ashare-cost-v1",
                "maker_bp": 1,
                "taker_bp": 2,
                "latency_base_ns": 3,
                "latency_insert_ns": 4,
            },
        )
        self.assertEqual(
            payload["risk_rules"],
            {"version": "runtime-risk-v1", "max_qty_raw": 500, "no_short": True},
        )

    def test_ashare_backtest_dtos_match_rust_rule_and_event_fields(self):
        from qianxing import AshareBacktestRules, AshareCorporateActionEvent

        source = (PYTHON_DIR.parent / "crates" / "qx-xingban" / "src" / "ashare.rs").read_text(
            encoding="utf-8"
        )

        def rust_fields(type_name: str) -> set[str]:
            match = re.search(
                rf"pub struct {type_name} \{{([\s\S]*?)\n\}}",
                source,
            )
            self.assertIsNotNone(match, f"Rust {type_name} definition must remain discoverable")
            return set(re.findall(r"^\s*pub ([a-z_]+):", match.group(1), re.MULTILINE))

        self.assertEqual(set(AshareBacktestRules.__annotations__), rust_fields("AshareRuleConfig"))
        self.assertEqual(
            set(get_type_hints(AshareCorporateActionEvent)),
            rust_fields("AshareCorporateActionEvent"),
        )

        def rust_snake_case_variants(type_name: str) -> set[str]:
            match = re.search(rf"pub enum {type_name} \{{([\s\S]*?)\n\}}", source)
            self.assertIsNotNone(match, f"Rust {type_name} enum must remain discoverable")
            variants = re.findall(r"^\s*(?:#\[default\]\s*)?([A-Z][A-Za-z0-9]*)\s*,?\s*$", match.group(1), re.MULTILINE)
            return {
                re.sub(r"(?<!^)(?=[A-Z])", "_", variant).lower()
                for variant in variants
            }

        rules = get_type_hints(AshareBacktestRules)
        event = get_type_hints(AshareCorporateActionEvent)
        self.assertEqual(set(get_args(rules["board"])), rust_snake_case_variants("AshareBoard"))
        self.assertEqual(
            set(get_args(event["action_type"])),
            rust_snake_case_variants("AshareCorporateActionType"),
        )

        backtest_source = (PYTHON_DIR.parent / "crates" / "qx-app" / "src" / "spec" / "backtest.rs").read_text(
            encoding="utf-8"
        )
        fill_model = re.search(r"pub enum BacktestFillModel \{([\s\S]*?)\n\}", backtest_source)
        self.assertIsNotNone(fill_model, "Rust BacktestFillModel enum must remain discoverable")
        rust_fill_models = {
            re.sub(r"(?<!^)(?=[A-Z])", "_", variant).lower()
            for variant in re.findall(r"^\s*([A-Z][A-Za-z0-9]*)\s*,?\s*$", fill_model.group(1), re.MULTILINE)
        }
        from qianxing import BacktestSpec

        self.assertEqual(set(get_args(get_type_hints(BacktestSpec)["fill_model"])[0].__args__), rust_fill_models)

    def test_top_level_all_equals_the_app_public_surface(self):
        """顶层稳定名单 == `qianxing.app` 的公开类/函数集合，**双向**。"""
        source = (PACKAGE_DIR / "app.py").read_text(encoding="utf-8")
        public = set(re.findall(r"^(?:class|def) ([A-Za-z]\w*)", source, re.MULTILINE))
        exported = set(qianxing.__all__)
        self.assertEqual(sorted(public - exported), [], "app.py 有公开面没被导出")
        self.assertEqual(sorted(exported - public), [], "顶层导出了 app.py 里没有的名字")

    def test_top_level_names_are_all_reachable(self):
        unresolved = [item for item in qianxing.__all__ if not hasattr(qianxing, item)]
        self.assertEqual(unresolved, [])

    def test_walk_forward_and_baseline_contracts_round_trip_through_typed_sdk(self):
        from qianxing import (
            BacktestSpec,
            BuiltinStrategySpec,
            ExperimentBaselineReport,
            ExperimentOutOfSampleReport,
            ExperimentParameterSpace,
            ExperimentSplitPlan,
            InstrumentId,
            MakerTakerFeeSchedule,
            RunExperimentResult,
            RunExperimentSpec,
            TradingInstrumentSpec,
        )

        base = BacktestSpec(
            run_id="typed-experiment-base",
            instrument="BTCUSDT.BINANCE",
            bars_path="bars.json",
            settlement_currency="USDT",
            initial_cash_raw=100_000_000_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(
                kind="sma_cross", strategy_id="typed-sma", fast_window=2, slow_window=4,
                target_notional_raw=25_000_000_000,
            ),
        )
        self.assertEqual(base.to_dict()["strategy"]["target_notional_raw"], 25_000_000_000)
        market_spec = TradingInstrumentSpec(
            instrument=InstrumentId("BTCUSDT", "BINANCE"),
            product="spot",
            base_currency="BTC",
            quote_currency="USDT",
            settlement_currency="USDT",
            contract_size=1_000_000_000,
            linear=True,
            inverse=False,
            price_tick=1_000,
            qty_step=1,
            min_qty=1,
            max_leverage=1,
            maintenance_margin_bps=0,
            valid_from=0,
        )
        market_aware = BacktestSpec(**{**base.__dict__, "market_spec": market_spec})
        self.assertEqual(
            market_aware.to_dict()["market_spec"]["instrument"],
            {"symbol": "BTCUSDT", "venue": "BINANCE"},
        )
        self.assertNotIn("market_spec", base.to_dict())
        self.assertEqual(
            MakerTakerFeeSchedule(maker_fee_bps=11, taker_fee_bps=37).to_dict(),
            {"maker_fee_bps": 11, "taker_fee_bps": 37},
        )
        fee_aware = BacktestSpec(
            **{
                **base.__dict__,
                "fee_schedule": MakerTakerFeeSchedule(maker_fee_bps=11, taker_fee_bps=37),
            }
        )
        self.assertEqual(
            fee_aware.to_dict()["fee_schedule"],
            {"maker_fee_bps": 11, "taker_fee_bps": 37},
        )
        self.assertNotIn("fee_schedule", base.to_dict())
        margin_sized = BuiltinStrategySpec(
            kind="sma_cross", strategy_id="margin-sma", target_available_margin_bps=2_500
        )
        self.assertEqual(margin_sized.to_dict()["target_available_margin_bps"], 2_500)
        volatility_sized = BuiltinStrategySpec(
            kind="sma_cross", strategy_id="volatility-sma", target_volatility_bps=25
        )
        self.assertEqual(volatility_sized.to_dict()["target_volatility_bps"], 25)
        kelly_sized = BuiltinStrategySpec(
            kind="sma_cross",
            strategy_id="kelly-sma",
            target_kelly_fraction_bps=5_000,
            kelly_lookback_bars=60,
        )
        self.assertEqual(kelly_sized.to_dict()["target_kelly_fraction_bps"], 5_000)
        self.assertEqual(kelly_sized.to_dict()["kelly_lookback_bars"], 60)
        legacy_strategy = BuiltinStrategySpec(kind="sma_cross", strategy_id="legacy-sma")
        self.assertNotIn("target_notional_raw", legacy_strategy.to_dict())
        self.assertNotIn("target_available_margin_bps", legacy_strategy.to_dict())
        self.assertNotIn("target_volatility_bps", legacy_strategy.to_dict())
        self.assertNotIn("target_kelly_fraction_bps", legacy_strategy.to_dict())
        self.assertNotIn("kelly_lookback_bars", legacy_strategy.to_dict())
        legacy = RunExperimentSpec(
            experiment_id="legacy-grid",
            base=base,
            parameter_space=(ExperimentParameterSpace("fast_window", (2, 3)),),
        )
        self.assertNotIn("split_plan", legacy.to_dict())
        self.assertNotIn("baselines", legacy.to_dict())
        self.assertNotIn("strategy_kinds", legacy.to_dict())

        spec = RunExperimentSpec(
            experiment_id="walk-forward-grid",
            base=base,
            parameter_space=(ExperimentParameterSpace("fast_window", (2, 3)),),
            split_plan=ExperimentSplitPlan(20, 10, walk_forward=True, step_bars=10),
            baselines=("buy-and-hold", "zero-signal"),
        )
        self.assertEqual(spec.to_dict()["split_plan"]["step_bars"], 10)
        self.assertEqual(spec.to_dict()["baselines"], ["buy-and-hold", "zero-signal"])

        strategy_selection = RunExperimentSpec(
            experiment_id="classic-selection",
            base=base,
            parameter_space=(),
            strategy_kinds=("ema_cross", "sma_cross"),
        )
        self.assertEqual(
            strategy_selection.to_dict()["strategy_kinds"], ["ema_cross", "sma_cross"]
        )

        result = RunExperimentResult.from_dict(
            {
                "schema_version": 1,
                "experiment_id": "walk-forward-grid",
                "total_candidates": 1,
                "completed_candidates": 1,
                "succeeded_candidates": 1,
                "failed_candidates": 0,
                "spec_fingerprint": "abc",
                "input_data_fingerprint": "barframe:0123456789abcdef",
                "candidates": [
                    {
                        "ordinal": 1,
                        "strategy_kind": "ema_cross",
                        "parameters": {},
                        "outcome": None,
                        "error": "candidate unavailable",
                    }
                ],
                "comparison": None,
                "artifact_path": "experiment.json",
                "out_of_sample": {
                    "folds": [
                        {
                            "fold": 1,
                            "train_range": [0, 20],
                            "test_range": [20, 30],
                            "selected_ordinal": 0,
                            "selected_strategy_kind": "ema_cross",
                            "selected_parameters": {"fast_window": 2},
                            "in_sample_return_bps": 15,
                            "out_of_sample_return_bps": 7,
                        }
                    ],
                    "in_sample_return_bps": 15,
                    "out_of_sample_return_bps": 7,
                    "degradation_bps": 8,
                },
                "baselines": [
                    {"name": "buy-and-hold", "return_bps": 100, "max_drawdown_bps": 20}
                ],
            }
        )
        self.assertIsInstance(result.out_of_sample, ExperimentOutOfSampleReport)
        self.assertEqual(result.input_data_fingerprint, "barframe:0123456789abcdef")
        self.assertEqual(result.candidates[0].strategy_kind, "ema_cross")
        self.assertEqual(result.out_of_sample.folds[0].test_range, (20, 30))
        self.assertEqual(result.out_of_sample.folds[0].selected_strategy_kind, "ema_cross")
        self.assertIsInstance(result.baselines[0], ExperimentBaselineReport)
        self.assertEqual(result.baselines[0].return_bps, 100)


    def test_compare_runs_schemas_match_rust_python_and_ranked_result(self):
        import json

        from qianxing import (
            CompareRunsResult,
            CompareRunsSpec,
            ComparedRun,
            ComparedRunResult,
        )

        root = PYTHON_DIR.parent
        source = (root / "crates" / "qx-app" / "src" / "cases" / "compare_runs.rs").read_text(encoding="utf-8")
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        registry_by_id = {entry["schema_id"]: entry for entry in registry["entries"]}

        def rust_fields(name):
            matched = re.search(re.escape(f"pub struct {name} {{") + r"([\s\S]*?)\n\}", source)
            self.assertIsNotNone(matched, f"Rust {name} definition must remain discoverable")
            return set(re.findall(r"^\s*pub ([a-z_]\w*):", matched.group(1), re.MULTILINE))

        schemas = {
            "CompareRunsSpec": json.loads((root / "schemas" / "compare-runs-spec-v1.json").read_text(encoding="utf-8")),
            "CompareRunsResult": json.loads((root / "schemas" / "compare-runs-result-v1.json").read_text(encoding="utf-8")),
        }
        python_types = {"CompareRunsSpec": CompareRunsSpec, "CompareRunsResult": CompareRunsResult}
        for name, schema in schemas.items():
            with self.subTest(contract=name):
                expected = rust_fields(name)
                self.assertEqual({field.name for field in fields(python_types[name])}, expected)
                self.assertEqual(set(schema["properties"]), expected)
                self.assertEqual(set(schema["required"]), expected)
                self.assertFalse(schema["additionalProperties"])

        self.assertEqual({field.name for field in fields(ComparedRun)}, rust_fields("ComparedRun"))
        self.assertEqual(
            set(schemas["CompareRunsSpec"]["$defs"]["ComparedRun"]["properties"]),
            rust_fields("ComparedRun"),
        )
        self.assertEqual(
            {field.name for field in fields(ComparedRunResult)},
            rust_fields("ComparedRunResult"),
        )
        self.assertEqual(
            set(schemas["CompareRunsResult"]["$defs"]["ComparedRunResult"]["properties"]),
            rust_fields("ComparedRunResult"),
        )
        run_array = schemas["CompareRunsSpec"]["properties"]["runs"]
        self.assertEqual((run_array["minItems"], run_array["maxItems"]), (2, 10_000))
        compared_run_properties = schemas["CompareRunsSpec"]["$defs"]["ComparedRun"]["properties"]
        self.assertEqual(compared_run_properties["max_drawdown_bps"]["maximum"], 10_000)
        for field in ("run_id", "instrument", "data_fingerprint", "result_hash"):
            self.assertEqual(compared_run_properties[field]["pattern"], r".*\S.*")
        for field in ("instrument", "data_fingerprint"):
            self.assertEqual(schemas["CompareRunsResult"]["properties"][field]["pattern"], r".*\S.*")

        expected_fixtures = [
            "crates/qx-app/src/cases/compare_runs.rs",
            "crates/qx-api/src/lib.rs",
            "python/tests/test_sdk_layering.py",
        ]
        for schema_id in ("compare-runs-spec-v1", "compare-runs-result-v1"):
            entry = registry_by_id[schema_id]
            self.assertEqual(entry["golden_fixtures"], expected_fixtures)
            self.assertEqual(entry["producer"], "qx-app")
            self.assertEqual(entry["consumers"], ["qx-cli", "qx-api", "qx-python"])

        runs = (
            ComparedRun("run-a", "600000.SSE", "bars:abc", "hash-a", 100, 200),
            ComparedRun("run-b", "600000.SSE", "bars:abc", "hash-b", 50, 100),
        )
        payload = CompareRunsSpec(runs).to_dict()
        self.assertEqual(set(payload), set(schemas["CompareRunsSpec"]["properties"]))
        ranked = CompareRunsResult.from_dict({
            "schema_version": 1,
            "instrument": "600000.SSE",
            "data_fingerprint": "bars:abc",
            "runs": [
                {"rank": 1, "run": runs[0].to_dict()},
                {"rank": 2, "run": runs[1].to_dict()},
            ],
        })
        self.assertEqual([item.rank for item in ranked.runs], [1, 2])

    def test_provider_backtest_schemas_match_rust_contract_and_routes(self):
        import json

        root = PYTHON_DIR.parent
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        registry_by_id = {entry["schema_id"]: entry for entry in registry["entries"]}
        provider_source = (root / "crates" / "qx-provider" / "src" / "lib.rs").read_text(encoding="utf-8")

        def rust_fields(source, name):
            matched = re.search(re.escape(f"pub struct {name} {{") + r"([\s\S]*?)\n\}", source)
            self.assertIsNotNone(matched, f"Rust {name} definition must remain discoverable")
            return set(re.findall(r"^\s*pub ([a-z_]\w*):", matched.group(1), re.MULTILINE))

        request_fields = rust_fields(provider_source, "DataQuery") | rust_fields(provider_source, "DataRequirements")
        contracts = (
            (
                "provider-backtest-spec-v1",
                "ProviderBacktestSpec",
                root / "crates" / "qx-app" / "src" / "cases" / "run_provider_backtest_with_manifest.rs",
                "/app/provider-backtest",
            ),
            (
                "provider-depth-backtest-spec-v1",
                "ProviderDepthBacktestSpec",
                root / "crates" / "qx-app" / "src" / "cases" / "run_provider_depth_backtest.rs",
                "/app/provider-depth-backtest",
            ),
        )
        app_routes = (PACKAGE_DIR / "app.py").read_text(encoding="utf-8")
        api_routes = (root / "crates" / "qx-api" / "src" / "lib.rs").read_text(encoding="utf-8")

        for schema_id, rust_name, rust_path, route in contracts:
            with self.subTest(schema=schema_id):
                schema_path = root / registry_by_id[schema_id]["path"]
                schema = json.loads(schema_path.read_text(encoding="utf-8"))
                source = rust_path.read_text(encoding="utf-8")
                fields_found = rust_fields(source, rust_name)
                self.assertEqual(set(schema["properties"]), fields_found)
                self.assertEqual(set(schema["required"]), fields_found - {"manifest_path"})
                self.assertFalse(schema["additionalProperties"])
                self.assertEqual(set(schema["$defs"]["DataRequest"]["properties"]), request_fields)
                self.assertTrue(schema["$defs"]["DataRequest"]["additionalProperties"])
                self.assertIn(route, app_routes)
                self.assertIn(route, api_routes)
                entry = registry_by_id[schema_id]
                self.assertEqual(entry["producer"], "qx-app")
                expected_consumers = ["qx-cli", "qx-api", "qx-python", "qx-sdk"]
                self.assertEqual(entry["consumers"], expected_consumers)
                self.assertIn("crates/qx-api/src/provider_backtest_tests.rs", entry["golden_fixtures"])

        bar_schema = json.loads((root / "schemas" / "provider-backtest-spec-v1.json").read_text(encoding="utf-8"))
        backtest_schema = json.loads((root / "schemas" / "backtest-spec-v1.json").read_text(encoding="utf-8"))
        self.assertEqual(bar_schema["$defs"]["BacktestSpec"]["properties"], backtest_schema["properties"])
        self.assertEqual(bar_schema["$defs"]["BacktestSpec"]["required"], backtest_schema["required"])
        self.assertEqual(
            {name: schema for name, schema in bar_schema["$defs"].items() if name not in {"DataRequest", "BacktestSpec"}},
            backtest_schema["$defs"],
        )
        bar_request_constraints = bar_schema["allOf"][0]["properties"]["data_request"]["properties"]
        self.assertEqual(bar_request_constraints["kind"]["const"], "Bar")
        self.assertEqual((bar_request_constraints["instrument_set"]["minItems"], bar_request_constraints["instrument_set"]["maxItems"]), (1, 1))

        depth_schema = json.loads((root / "schemas" / "provider-depth-backtest-spec-v1.json").read_text(encoding="utf-8"))
        depth_backtest_schema = json.loads((root / "schemas" / "depth-backtest-spec-v1.json").read_text(encoding="utf-8"))
        self.assertEqual(depth_schema["$defs"]["DepthBacktestSpec"]["properties"], depth_backtest_schema["properties"])
        self.assertEqual(depth_schema["$defs"]["DepthBacktestSpec"]["required"], depth_backtest_schema["required"])
        self.assertEqual(
            {name: schema for name, schema in depth_schema["$defs"].items() if name not in {"DataRequest", "DepthBacktestSpec"}},
            depth_backtest_schema["$defs"],
        )
        request_properties = depth_schema["$defs"]["DataRequest"]["properties"]
        depth_request_constraints = depth_schema["allOf"][0]["properties"]["data_request"]["properties"]
        self.assertEqual(depth_request_constraints["kind"]["const"], "Quote")
        self.assertEqual(depth_request_constraints["frequency"]["const"], "tick")
        self.assertEqual((depth_request_constraints["instrument_set"]["minItems"], depth_request_constraints["instrument_set"]["maxItems"]), (1, 1))
        self.assertEqual(request_properties["kind"]["enum"], ["Bar", "Quote", "Trade", "Financial", "Instrument"])
        self.assertEqual(
            {branch["then"]["properties"]["data_request"]["properties"]["required_data_tier"]["const"] for branch in depth_schema["allOf"][1:]},
            {"l1", "l2"},
        )

        result_schema = json.loads((root / "schemas" / "provider-backtest-result-v1.json").read_text(encoding="utf-8"))
        result_entry = registry_by_id["provider-backtest-result-v1"]
        result_source = (root / "crates" / "qx-app" / "src" / "cases" / "run_provider_backtest_with_manifest.rs").read_text(encoding="utf-8")
        result_fields = rust_fields(result_source, "ProviderBacktestOutcome")
        from qianxing.data.providers import ProviderDatasetBacktestOutcome

        self.assertEqual(set(result_schema["properties"]), result_fields)
        self.assertEqual({field.name for field in fields(ProviderDatasetBacktestOutcome)}, result_fields)
        self.assertEqual(result_entry["producer"], "qx-app")
        self.assertEqual(result_entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        manifest = json.loads((root / "schemas" / "dataset-manifest-v2.json").read_text(encoding="utf-8"))
        verdict = json.loads((root / "schemas" / "dataset-verdict-v1.json").read_text(encoding="utf-8"))
        outcome = json.loads((root / "schemas" / "backtest-outcome-v1.json").read_text(encoding="utf-8"))
        self.assertEqual(result_schema["$defs"]["DatasetManifestV2"], {key: value for key, value in manifest.items() if key not in {"$schema", "$id", "title", "description"}})
        self.assertEqual(result_schema["$defs"]["DatasetVerdict"], {key: value for key, value in verdict.items() if key not in {"$schema", "$id", "title", "description"}})
        self.assertEqual(result_schema["$defs"]["BacktestArtifacts"], outcome["$defs"]["BacktestArtifacts"])
        self.assertEqual(result_schema["$defs"]["BacktestOutcome"]["properties"], {**outcome["properties"], "artifacts": {"$ref": "#/$defs/BacktestArtifacts"}})
        from jsonschema import Draft202012Validator

        Draft202012Validator.check_schema(result_schema)

        depth_result_schema = json.loads((root / "schemas" / "provider-depth-backtest-result-v1.json").read_text(encoding="utf-8"))
        depth_result_entry = registry_by_id["provider-depth-backtest-result-v1"]
        depth_result_source = (root / "crates" / "qx-app" / "src" / "cases" / "run_provider_depth_backtest.rs").read_text(encoding="utf-8")
        self.assertEqual(set(depth_result_schema["properties"]), rust_fields(depth_result_source, "ProviderDepthBacktestOutcome"))
        from qianxing.data.providers import ProviderDepthBacktestOutcome

        self.assertEqual({field.name for field in fields(ProviderDepthBacktestOutcome)}, set(depth_result_schema["properties"]))
        self.assertEqual(depth_result_entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        depth_source = (root / "crates" / "qx-app" / "src" / "spec" / "depth.rs").read_text(encoding="utf-8")
        rust_depth_fields = rust_fields(depth_source, "DepthBacktestOutcome") - {"display_summary"}
        self.assertEqual(set(depth_result_schema["$defs"]["DepthBacktestOutcome"]["properties"]) - {"artifacts"}, rust_depth_fields - {"artifacts"})
        depth_account_schema = json.loads((root / "schemas" / "multi-account-depth-backtest-result-v1.json").read_text(encoding="utf-8"))
        self.assertNotIn("account_id", depth_result_schema["$defs"]["DepthBacktestOutcome"]["required"])
        self.assertNotIn("manifest_digest", depth_result_schema["$defs"]["DepthBacktestOutcome"]["required"])
        self.assertNotIn("account_id", depth_account_schema["$defs"]["DepthBacktestOutcome"]["required"])
        self.assertNotIn("manifest_digest", depth_account_schema["$defs"]["DepthBacktestOutcome"]["required"])
        standalone_depth_schema = json.loads((root / "schemas" / "depth-backtest-outcome-v1.json").read_text(encoding="utf-8"))
        self.assertEqual(standalone_depth_schema["properties"], depth_result_schema["$defs"]["DepthBacktestOutcome"]["properties"])
        self.assertEqual(standalone_depth_schema["required"], depth_result_schema["$defs"]["DepthBacktestOutcome"]["required"])
        self.assertEqual(standalone_depth_schema["properties"]["manifest_digest"]["pattern"], "^[0-9a-f]{16}$")
        Draft202012Validator.check_schema(depth_result_schema)
        Draft202012Validator.check_schema(standalone_depth_schema)

    def test_remaining_app_request_schemas_match_rust_and_sdk_routes(self):
        import json

        from qianxing import RiskParityResult, RiskParitySpec

        root = PYTHON_DIR.parent
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        registry_by_id = {entry["schema_id"]: entry for entry in registry["entries"]}
        api = (root / "crates" / "qx-api" / "src" / "lib.rs").read_text(encoding="utf-8")
        app_routes = (PACKAGE_DIR / "app.py").read_text(encoding="utf-8")

        def rust_fields(source, name):
            matched = re.search(re.escape(f"pub struct {name} {{") + r"([\s\S]*?)\n\}", source)
            self.assertIsNotNone(matched, f"Rust {name} definition must remain discoverable")
            return set(re.findall(r"^\s*pub ([a-z_]\w*):", matched.group(1), re.MULTILINE))

        cases = (
            ("trade-control-spec-v1", "TradeControlSpec", "crates/qx-app/src/cases/validate_trade_control.rs", ("/app/paper/commands", "/app/live/commands"), ["qx-api", "qx-python"]),
            ("risk-parity-spec-v1", "RiskParitySpec", "crates/qx-app/src/spec/risk_parity.rs", ("/app/plan-risk-parity",), ["qx-cli", "qx-api", "qx-python", "qx-sdk"]),
            ("algo-execution-spec-v1", "AlgoExecutionSpec", "crates/qx-app/src/spec/algo_execution.rs", ("/app/plan-algo-execution",), ["qx-cli", "qx-api", "qx-python", "qx-sdk"]),
            ("capital-allocation-spec-v1", "CapitalAllocationSpec", "crates/qx-app/src/spec/capital_allocation.rs", ("/app/plan-capital-allocation",), ["qx-api", "qx-python", "qx-sdk"]),
        )
        for schema_id, rust_name, rust_path, routes, consumers in cases:
            with self.subTest(schema=schema_id):
                schema = json.loads((root / registry_by_id[schema_id]["path"]).read_text(encoding="utf-8"))
                source = (root / rust_path).read_text(encoding="utf-8")
                self.assertEqual(set(schema["properties"]), rust_fields(source, rust_name))
                self.assertFalse(schema["additionalProperties"])
                entry = registry_by_id[schema_id]
                self.assertEqual(entry["producer"], "qx-app")
                self.assertEqual(entry["consumers"], consumers)
                self.assertIn("python/tests/test_sdk_layering.py", entry["golden_fixtures"])
                for route in routes:
                    self.assertIn(route, app_routes)
                    self.assertIn(route, api)

        risk_schema = json.loads((root / "schemas" / "risk-parity-spec-v1.json").read_text(encoding="utf-8"))
        risk_source = (root / "crates" / "qx-app" / "src" / "spec" / "risk_parity.rs").read_text(encoding="utf-8")
        self.assertEqual({field.name for field in fields(RiskParitySpec)}, rust_fields(risk_source, "RiskParitySpec"))
        self.assertEqual(set(risk_schema["$defs"]["RiskParityAssetSpec"]["properties"]), rust_fields(risk_source, "RiskParityAssetSpec"))
        self.assertEqual((risk_schema["properties"]["assets"]["minItems"], risk_schema["properties"]["assets"]["maxItems"]), (1, 64))
        self.assertEqual((risk_schema["$defs"]["RiskParityAssetSpec"]["properties"]["returns_bps"]["minItems"], risk_schema["$defs"]["RiskParityAssetSpec"]["properties"]["returns_bps"]["maxItems"]), (2, 10000))

        risk_result_schema = json.loads((root / "schemas" / "risk-parity-result-v1.json").read_text(encoding="utf-8"))
        risk_result_entry = registry_by_id["risk-parity-result-v1"]
        zhenlu_risk = (root / "crates" / "qx-zhenlu" / "src" / "portfolio" / "risk_parity.rs").read_text(encoding="utf-8")
        self.assertEqual(set(risk_result_schema["properties"]), rust_fields(risk_source, "RiskParityResult"))
        self.assertEqual({field.name for field in fields(RiskParityResult)}, rust_fields(risk_source, "RiskParityResult"))
        self.assertEqual(set(risk_result_schema["$defs"]["RiskParityPlan"]["properties"]), rust_fields(zhenlu_risk, "RiskParityPlan"))
        self.assertEqual(set(risk_result_schema["$defs"]["RiskParityAllocation"]["properties"]), rust_fields(zhenlu_risk, "RiskParityAllocation"))
        self.assertEqual(set(risk_result_schema["$defs"]["RiskParityOrderDelta"]["properties"]), rust_fields(zhenlu_risk, "RiskParityOrderDelta"))
        self.assertEqual(risk_result_schema["properties"]["schema_version"]["const"], 1)
        self.assertFalse(risk_result_schema["additionalProperties"])
        self.assertEqual(risk_result_entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])
        self.assertIn("crates/qx-cli/tests/app_three_entrypoints/risk_parity.rs", risk_result_entry["golden_fixtures"])

        algo_schema = json.loads((root / "schemas" / "algo-execution-spec-v1.json").read_text(encoding="utf-8"))
        algo_source = (root / "crates" / "qx-app" / "src" / "spec" / "algo_execution.rs").read_text(encoding="utf-8")
        zhenlu_algo = (root / "crates" / "qx-zhenlu" / "src" / "algo.rs").read_text(encoding="utf-8")
        self.assertEqual(set(algo_schema["$defs"]["VwapVolumeBucket"]["properties"]), rust_fields(zhenlu_algo, "VwapVolumeBucket"))
        self.assertEqual(algo_schema["properties"]["method"]["enum"], ["twap", "vwap"])
        self.assertEqual(algo_schema["properties"]["volume_profile"]["maxItems"], 10000)
        self.assertEqual(len(algo_schema["allOf"]), 2)

        algo_plan_schema = json.loads((root / "schemas" / "algo-execution-plan-v1.json").read_text(encoding="utf-8"))
        algo_plan_entry = registry_by_id["algo-execution-plan-v1"]
        self.assertEqual(set(algo_plan_schema["properties"]), rust_fields(algo_source, "AlgoExecutionPlan"))
        self.assertEqual(set(algo_plan_schema["$defs"]["AlgoChildSlice"]["properties"]), rust_fields(zhenlu_algo, "AlgoChildSlice"))
        self.assertEqual(algo_plan_schema["properties"]["schema_version"]["const"], 1)
        self.assertEqual(algo_plan_schema["properties"]["children"]["maxItems"], 10000)
        self.assertFalse(algo_plan_schema["additionalProperties"])
        self.assertEqual(algo_plan_entry["consumers"], ["qx-cli", "qx-api", "qx-python", "qx-sdk"])

        capital_schema = json.loads((root / "schemas" / "capital-allocation-spec-v1.json").read_text(encoding="utf-8"))
        capital_source = (root / "crates" / "qx-app" / "src" / "spec" / "capital_allocation.rs").read_text(encoding="utf-8")
        allocation_source = (root / "crates" / "qx-zhenlu" / "src" / "portfolio" / "capital_allocation.rs").read_text(encoding="utf-8")
        self.assertEqual(set(capital_schema["$defs"]["CapitalAllocationAccount"]["properties"]), rust_fields(allocation_source, "CapitalAllocationAccount"))
        self.assertEqual(set(capital_schema["$defs"]["CapitalAllocationRequest"]["properties"]), rust_fields(allocation_source, "CapitalAllocationRequest"))
        self.assertEqual(capital_schema["properties"]["accounts"]["maxItems"], 1024)
        self.assertEqual(capital_schema["properties"]["requests"]["maxItems"], 10000)

        capital_result_schema = json.loads((root / "schemas" / "capital-allocation-result-v1.json").read_text(encoding="utf-8"))
        capital_result_entry = registry_by_id["capital-allocation-result-v1"]
        self.assertEqual(set(capital_result_schema["properties"]), rust_fields(capital_source, "CapitalAllocationResult"))
        self.assertEqual(set(capital_result_schema["$defs"]["CapitalAllocation"]["properties"]), rust_fields(allocation_source, "CapitalAllocation"))
        self.assertEqual(capital_result_schema["properties"]["schema_version"]["const"], 1)
        self.assertEqual(capital_result_schema["properties"]["allocations"]["maxItems"], 10000)
        self.assertFalse(capital_result_schema["additionalProperties"])
        self.assertEqual(capital_result_entry["consumers"], ["qx-api", "qx-python", "qx-sdk"])

        trade_schema = json.loads((root / "schemas" / "trade-control-spec-v1.json").read_text(encoding="utf-8"))
        trade_source = (root / "crates" / "qx-app" / "src" / "cases" / "validate_trade_control.rs").read_text(encoding="utf-8")
        control_source = (root / "crates" / "qx-control" / "src" / "lib.rs").read_text(encoding="utf-8")
        self.assertEqual(set(trade_schema["$defs"]["ControlCommand"]["properties"]), rust_fields(control_source, "ControlCommand"))
        self.assertTrue(trade_schema["$defs"]["ControlCommand"]["additionalProperties"])
        self.assertEqual(trade_schema["properties"]["workflow"]["enum"], ["paper", "live"])
        self.assertEqual(trade_schema["properties"]["schema_version"]["const"], 1)

    def test_depth_dataset_validation_schema_matches_rust_spec_and_cli_tiers(self):
        import json

        root = PYTHON_DIR.parent
        schema = json.loads((root / "schemas" / "depth-dataset-manifest-validation-spec-v1.json").read_text(encoding="utf-8"))
        registry = json.loads((root / "maturity" / "schema-registry.json").read_text(encoding="utf-8"))
        entry = next(item for item in registry["entries"] if item["schema_id"] == "depth-dataset-manifest-validation-spec-v1")
        source = (root / "crates" / "qx-app" / "src" / "spec" / "dataset_manifest.rs").read_text(encoding="utf-8")
        cli = (root / "crates" / "qx-cli" / "src" / "data_validate.rs").read_text(encoding="utf-8")

        matched = re.search(re.escape("pub struct DepthDatasetManifestValidationSpec {") + r"([\s\S]*?)\n\}", source)
        self.assertIsNotNone(matched)
        rust_fields = set(re.findall(r"^\s*pub ([a-z_]\w*):", matched.group(1), re.MULTILINE))
        self.assertEqual(set(schema["properties"]), rust_fields)
        self.assertEqual(schema["properties"]["tier"]["enum"], ["l1", "l2"])
        self.assertEqual(schema["properties"]["interval_ms"]["minimum"], 1)
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(entry["producer"], "qx-app")
        self.assertEqual(entry["consumers"], ["qx-cli"])
        self.assertIn("DepthDatasetManifestValidationSpec", cli)
        self.assertIn("L1", cli)
        self.assertIn("L2", cli)


class OptionalDependencyTest(unittest.TestCase):
    def test_core_layers_import_without_the_optional_adapters(self):
        """在子进程里核：核心层 import 不得顺带拉起可选适配器。"""
        probe = (
            "import sys;"
            "import qianxing.core, qianxing.client, qianxing.data,"
            " qianxing.experiments, qianxing.reports;"
            f"pulled=[name for name in {OPTIONAL_MODULES!r} if name in sys.modules];"
            "assert not pulled, pulled;"
            "print('ok')"
        )
        completed = subprocess.run(
            [sys.executable, "-c", probe],
            cwd=str(PYTHON_DIR),
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            completed.returncode,
            0,
            f"核心层 import 拉起了可选适配器：{completed.stdout}{completed.stderr}",
        )
        self.assertIn("ok", completed.stdout)


if __name__ == "__main__":
    unittest.main()
