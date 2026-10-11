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


class TopLevelStabilityTest(unittest.TestCase):
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
