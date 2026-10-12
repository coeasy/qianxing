import unittest


class ProtectiveExitContractTests(unittest.TestCase):
    def test_typed_and_legacy_specs_preserve_wire_shape(self):
        from qianxing import BacktestSpec, BuiltinStrategySpec, ProtectiveExitSpec
        from qianxing.core import ProtectiveExitSpec as CoreProtectiveExitSpec

        self.assertIs(ProtectiveExitSpec, CoreProtectiveExitSpec)
        strategy = BuiltinStrategySpec(kind="sma_cross", strategy_id="stop-test")
        common = dict(
            run_id="stop-test",
            instrument="BTCUSDT.BINANCE",
            bars_path="bars.json",
            settlement_currency="USDT",
            initial_cash_raw=1_000_000,
            output_dir="out",
            strategy=strategy,
        )
        legacy = BacktestSpec(**common)
        self.assertNotIn("protective_exit", legacy.to_dict())
        self.assertNotIn("account_id", legacy.to_dict())
        positional = BacktestSpec("positional", "BTCUSDT.BINANCE", "bars.json", "USDT", 1_000_000, "out", strategy, 7, 1)
        self.assertEqual(positional.seed, 7)
        self.assertEqual(positional.schema_version, 1)

        account_owned = BacktestSpec(**common, account_id="acct.paper-01")
        self.assertEqual(account_owned.to_dict()["account_id"], "acct.paper-01")

        typed = BacktestSpec(
            **common,
            protective_exit=ProtectiveExitSpec(stop_loss_bps=250, trailing_stop_bps=100),
        )
        self.assertEqual(
            typed.to_dict()["protective_exit"],
            {"stop_loss_bps": 250, "trailing_stop_bps": 100},
        )


if __name__ == "__main__":
    unittest.main()
