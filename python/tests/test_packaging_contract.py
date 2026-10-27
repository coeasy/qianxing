"""wheel 依赖分层契约：基础安装离线可装成，ccxt/tzdata 只在调用点惰性加载（V13 #276）。

改前 `python/pyproject.toml` 把 ccxt 与 tzdata 写成顶层 mandatory dependencies，
没有索引时 `pip install <wheel>` 直接失败（"ccxt was not found ... cannot be used"），
可这四个包 import 时都不需要它们——项目自己的安装文档因此一直挂着 `--offline --no-deps`。
这里钉两件事：基础安装没有强制第三方依赖；缺 ccxt 时适配器抛的是点名真实存在
extra 的可执行错误（而不是 import 期崩，也不是指一个不存在的 extra）。
"""

import sys
import json
try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10
    import tomli as tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_ccxt import (
    SUPPORTED_CCXT_VERSION,
    CcxtConnectorError,
    CcxtErrorClass,
    CcxtExchangeClient,
)

PYPROJECT = Path(__file__).resolve().parents[1] / "pyproject.toml"
EXAMPLES = Path(__file__).resolve().parents[1] / "examples"
FORBIDDEN_MANDATORY = ("ccxt", "tzdata")
REQUIRED_EXTRAS = (
    "ccxt",
    "ccxt-pro",
    "tz",
    "a-share",
    "a-share-akshare",
    "a-share-baostock",
    "a-share-easy-tdx",
)


class PackagingContractTest(unittest.TestCase):
    def setUp(self):
        meta = tomllib.loads(PYPROJECT.read_text(encoding="utf-8"))
        self.deps = meta["project"].get("dependencies", [])
        self.extras = meta["project"].get("optional-dependencies", {})

    def test_base_install_has_no_mandatory_third_party_runtime_deps(self):
        offenders = [d for d in self.deps if any(name in d for name in FORBIDDEN_MANDATORY)]
        self.assertEqual(
            offenders,
            [],
            "ccxt/tzdata 又变回强制依赖，离线 pip install 会重新变成脚枪（#276）",
        )

    def test_capability_extras_are_declared(self):
        missing = [name for name in REQUIRED_EXTRAS if name not in self.extras]
        self.assertEqual(missing, [], f"缺这些可选 extras: {missing}")

    def test_exchange_extras_pin_the_runtime_checked_ccxt_version(self):
        expected = f"ccxt=={SUPPORTED_CCXT_VERSION}"
        for name in ("ccxt", "ccxt-pro"):
            self.assertIn(expected, self.extras[name])
            self.assertFalse(
                any(dep.startswith("ccxt>") or dep.startswith("ccxt<") for dep in self.extras[name]),
                f"{name} extra 又放宽成未核验版本范围",
            )

    def test_missing_ccxt_degrades_to_actionable_extra_pointer(self):
        def fake_import(name, *args, **kwargs):
            raise ImportError(name)

        with patch("qianxing_ccxt.importlib.import_module", side_effect=fake_import):
            with self.assertRaises(CcxtConnectorError) as ctx:
                CcxtExchangeClient._load_ccxt()

        message = str(ctx.exception)
        self.assertEqual(ctx.exception.error_class, CcxtErrorClass.UNSUPPORTED)
        self.assertIn("qianxing[ccxt]", message)
        # 报错点名的 extra 必须真的在 pyproject 里定义（不许指一个不存在的 extra）
        self.assertIn("ccxt", self.extras)

    def test_unreviewed_ccxt_runtime_version_fails_closed(self):
        from types import SimpleNamespace

        with patch(
            "qianxing_ccxt.importlib.import_module",
            return_value=SimpleNamespace(__version__="4.5.88"),
        ):
            with self.assertRaises(CcxtConnectorError) as ctx:
                CcxtExchangeClient._load_ccxt()

        self.assertEqual(ctx.exception.error_class, CcxtErrorClass.UNSUPPORTED)
        self.assertIn(SUPPORTED_CCXT_VERSION, str(ctx.exception))

    def test_private_probe_examples_cover_spot_and_derivatives_for_all_three_venues(self):
        expected = {
            "ccxt-binance-spot.example.json": ("binance", "spot"),
            "ccxt-binance-usdm.example.json": ("binance", "future"),
            "ccxt-okx-spot.example.json": ("okx", "spot"),
            "ccxt-okx-swap.example.json": ("okx", "swap"),
            "ccxt-bitget-spot.example.json": ("bitget", "spot"),
            "ccxt-bitget-perpetual.example.json": ("bitget", "swap"),
            "ccxt-bitget-uta-perpetual.example.json": ("bitget", "swap"),
        }
        for filename, (exchange_id, default_type) in expected.items():
            with self.subTest(example=filename):
                config = json.loads((EXAMPLES / filename).read_text(encoding="utf-8"))
                self.assertEqual(config["exchange_id"], exchange_id)
                self.assertTrue(config["sandbox"])
                self.assertEqual(config["default_type"], default_type)
                self.assertTrue(config["account_id"].strip())
                self.assertTrue(config["instruments"])
                self.assertTrue(
                    all(
                        instrument.endswith(f".{exchange_id.upper()}")
                        for instrument in config["instruments"]
                    )
                )
                self.assertEqual(
                    set(config["credential_env"]),
                    {"api_key", "secret"}
                    | ({"password"} if exchange_id in {"okx", "bitget"} else set()),
                )
                self.assertTrue(
                    all(name.isupper() and name.startswith("QX_CCXT_")
                        for name in config["credential_env"].values())
                )
                self.assertNotIn("apiKey", config)
                self.assertNotIn("secret", config)
                if filename == "ccxt-bitget-uta-perpetual.example.json":
                    self.assertIs(config["options"].get("uta"), True)


if __name__ == "__main__":
    unittest.main()
