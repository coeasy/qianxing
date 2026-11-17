"""Read-only sandbox credential probe for a CCXT venue configuration.

This command deliberately has no order, cancel, leverage, or position-mode mutation. It prints
only capability/count metadata, never account balances, positions, API keys, or secrets.
"""

from __future__ import annotations

import argparse
import json
import os
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any, Mapping

from qianxing_bridge.native import app_account_query_capability_preflight_v1

from . import CcxtConnectorError, CcxtErrorClass, CcxtExchangeClient
from .worker import _config_from_file


def _nonzero_asset_count(balance: Mapping[str, Any]) -> tuple[int, int]:
    totals = balance.get("total")
    if not isinstance(totals, Mapping):
        return 0, 0
    count = 0
    nonzero = 0
    for amount in totals.values():
        try:
            parsed = Decimal(str(amount))
        except (InvalidOperation, TypeError, ValueError):
            continue
        count += 1
        if parsed != 0:
            nonzero += 1
    return count, nonzero


def _missing_credential_env_names(path: Path, exchange_id: str) -> list[str]:
    value = json.loads(path.read_text(encoding="utf-8"))
    credentials = value.get("credential_env") or {}
    required = ["api_key", "secret"]
    if exchange_id.lower() in {"okx", "bitget"}:
        required.append("password")
    return [
        str(credentials.get(name, "<未配置>"))
        for name in required
        if not credentials.get(name)
        or not os.environ.get(str(credentials[name]), "").strip()
    ]


def _preflight_account_queries(path: Path) -> tuple[dict[str, Any], str]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("CCXT configuration must be an object")
    exchange_id = str(value.get("exchange_id", "")).strip().upper()
    default_type = str(value.get("default_type", "")).strip().lower()
    if default_type in {"spot", "cash"}:
        product = "spot"
        operations = ["balance.read"]
    elif default_type in {"future", "swap", "perpetual"}:
        product = "perpetual"
        operations = ["balance.read", "positions.read", "account_settings.read"]
    else:
        raise ValueError("private probe requires explicit default_type=spot, future, or swap")

    spec = {
        "schema_version": 1,
        "profile": "sandbox_acceptance",
        "capability_id": "qx.venue.ccxt_process",
        "venue": exchange_id,
        "product": product,
        "account_model": str(value.get("account_model", "")).strip().lower(),
        "required_operations": operations,
        "optional_operations": [],
        "sandbox_endpoint": value.get("sandbox") is True,
    }
    decision = json.loads(app_account_query_capability_preflight_v1(json.dumps(spec)))
    if decision.get("allowed") is not True:
        reason = decision.get("reason") or decision.get("reason_code") or "scope denied"
        raise ValueError(f"account query capability preflight denied: {reason}")
    return value, product


def main() -> int:
    parser = argparse.ArgumentParser(
        description="使用 sandbox 凭据执行只读账户连通性检查，不会发送订单"
    )
    parser.add_argument("--config", required=True, type=Path)
    args = parser.parse_args()

    try:
        config_value, product = _preflight_account_queries(args.config)
        if config_value.get("sandbox") is not True:
            print(
                json.dumps(
                    {"ok": False, "error": "sandbox probe requires sandbox=true"},
                    ensure_ascii=False,
                )
            )
            return 2
        config = _config_from_file(args.config)
        missing = _missing_credential_env_names(args.config, config.exchange_id)
        if missing:
            print(
                json.dumps(
                    {
                        "ok": False,
                        "exchange_id": config.exchange_id,
                        "error": "missing credential environment variables",
                        "missing_env_names": missing,
                    },
                    ensure_ascii=False,
                )
            )
            return 2

        client = CcxtExchangeClient(config)
        balance = client.fetch_balance()
        asset_count, nonzero_asset_count = _nonzero_asset_count(balance)
        positions_checked = False
        nonzero_position_count = None
        contract_settings_checked = 0
        contract_settings_summary = []
        instruments = config_value.get("instruments", [])
        if product == "perpetual" and instruments:
            positions = client.fetch_positions(instruments)
            positions_checked = True
            nonzero_position_count = sum(
                1 for position in positions if position.contracts_raw != 0
            )
            for instrument in instruments:
                settings = client.fetch_contract_settings(str(instrument))
                if not settings.get("contract"):
                    raise CcxtConnectorError(
                        CcxtErrorClass.INVALID,
                        "derivative probe instrument did not resolve to a contract market",
                    )
                contract_settings_checked += 1
                contract_settings_summary.append(
                    {
                        "instrument": str(instrument),
                        "position_mode": settings["position_mode"],
                        "margin_mode": settings["margin_mode"],
                        "leverage_long": settings["leverage_long"],
                        "leverage_short": settings["leverage_short"],
                    }
                )

        print(
            json.dumps(
                {
                    "ok": True,
                    "exchange_id": config.exchange_id,
                    "sandbox": True,
                    "read_only": True,
                    "balance_asset_count": asset_count,
                    "nonzero_asset_count": nonzero_asset_count,
                    "positions_checked": positions_checked,
                    "nonzero_position_count": nonzero_position_count,
                    "contract_settings_checked": contract_settings_checked,
                    "contract_settings": contract_settings_summary,
                    "orders_sent": 0,
                },
                ensure_ascii=False,
            )
        )
        return 0
    except CcxtConnectorError as error:
        # CCXT exception messages may contain request details. Report the stable class only.
        print(
            json.dumps(
                {
                    "ok": False,
                    "error_class": error.error_class.value,
                    "error": "read-only sandbox probe failed; credential values were not printed",
                    "orders_sent": 0,
                },
                ensure_ascii=False,
            )
        )
        return 1
    except Exception as error:
        print(
            json.dumps(
                {
                    "ok": False,
                    "error_class": type(error).__name__,
                    "error": "read-only sandbox probe failed; credential values were not printed",
                    "orders_sent": 0,
                },
                ensure_ascii=False,
            )
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
