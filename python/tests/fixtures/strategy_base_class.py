"""Cross-process BaseStrategy fixture for the Rust Strategy worker contract tests."""

from qianxing_strategy import BaseStrategy


class ExactIntentStrategy(BaseStrategy):
    def __init__(self, spec):
        self.spec = spec

    def history_requirements(self):
        return {"warmup_bars": 1}

    def on_bar(self, context, bar):
        intent = self.order_intent(
            intent_id=17,
            instrument=bar["instrument"],
            side="buy",
            qty_exact=self.spec["parameters"]["qty_exact"],
            limit_price_exact=self.spec["parameters"]["limit_price_exact"],
            policy={
                "position_side": "net",
                "margin_mode": "cross",
                "position_mode": "one_way",
                "leverage": 3,
            },
        )
        return self.decision(context, "Bar", bar, intents=[intent])


def create_strategy(spec):
    return ExactIntentStrategy(spec)
