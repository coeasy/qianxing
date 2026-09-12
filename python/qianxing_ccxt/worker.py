"""CCXT JSONL 进程边界。

每行一个请求、每行一个响应，便于 Rust Runtime 通过子进程或 supervisor
调用公共 CCXT，而不把 Python/CCXT 依赖引入核心 crate。秘密只从环境变量加载。
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
from pathlib import Path
from typing import Any, TextIO

from . import (
    CcxtConfig,
    CcxtConnectorError,
    CcxtErrorClass,
    CcxtExchangeClient,
    create_ccxt_pro_exchange,
    normalize_stream_event,
    watch_stream,
)


class CcxtJsonWorker:
    def __init__(self, client: CcxtExchangeClient, *, pro_exchange: Any | None = None):
        self.client = client
        self.pro_exchange = pro_exchange

    def handle(self, request: dict[str, Any]) -> dict[str, Any]:
        operation = str(request.get("op", "")).strip()
        if operation == "load_markets":
            markets = self.client.load_markets(reload=bool(request.get("reload", False)))
            return {"markets": list(markets.values())}
        if operation == "resolve_market":
            market = self.client.resolve_market(str(request["instrument"]))
            return {"market": market.__dict__}
        if operation == "fetch_ohlcv":
            frame = self.client.fetch_ohlcv(
                str(request["instrument"]),
                timeframe=str(request.get("timeframe", "1m")),
                start_ms=int(request["start_ms"]),
                end_ms=int(request["end_ms"]),
                limit=int(request.get("limit", 1_000)),
                params=dict(request.get("params", {})),
            )
            return {"frame": json.loads(frame.to_json())}
        if operation == "fetch_ticker":
            ticker = self.client.fetch_ticker(
                str(request["instrument"]), params=dict(request.get("params", {}))
            )
            return {"ticker": ticker.__dict__}
        if operation == "create_order":
            order = self.client.create_order(
                str(request["instrument"]),
                side=str(request["side"]),
                order_type=str(request["order_type"]),
                amount_raw=int(request["amount_raw"]),
                price_raw=None
                if request.get("price_raw") is None
                else int(request["price_raw"]),
                client_order_id=None
                if request.get("client_order_id") is None
                else str(request["client_order_id"]),
                params=dict(request.get("params", {})),
            )
            return {"order": order.__dict__}
        if operation == "fetch_order":
            order = self.client.fetch_order(
                str(request["order_id"]),
                str(request["instrument"]),
                params=dict(request.get("params", {})),
            )
            return {"order": order.__dict__}
        if operation == "fetch_my_trades":
            trades = self.client.fetch_my_trades(
                str(request["instrument"]),
                since_ms=None if request.get("since_ms") is None else int(request["since_ms"]),
                limit=None if request.get("limit") is None else int(request["limit"]),
                params=dict(request.get("params", {})),
            )
            return {"trades": [trade.__dict__ for trade in trades]}
        if operation == "cancel_order":
            order = self.client.cancel_order(
                str(request["order_id"]),
                str(request["instrument"]),
                params=dict(request.get("params", {})),
            )
            return {"order": order.__dict__}
        if operation == "fetch_balance":
            return {"balance": self.client.fetch_balance(params=dict(request.get("params", {})))}
        if operation == "set_margin_mode":
            return {
                "result": self.client.set_margin_mode(
                    str(request["instrument"]),
                    str(request["margin_mode"]),
                    params=dict(request.get("params", {})),
                )
            }
        if operation == "set_leverage":
            return {
                "result": self.client.set_leverage(
                    str(request["instrument"]),
                    int(request["leverage"]),
                    margin_mode=request.get("margin_mode"),
                    params=dict(request.get("params", {})),
                )
            }
        if operation == "set_position_mode":
            return {
                "result": self.client.set_position_mode(
                    bool(request["hedged"]),
                    instrument=request.get("instrument"),
                    params=dict(request.get("params", {})),
                )
            }
        if operation == "fetch_positions":
            positions = self.client.fetch_positions(
                request.get("instruments"), params=dict(request.get("params", {}))
            )
            return {"positions": [position.__dict__ for position in positions]}
        if operation == "fetch_funding_rate":
            funding = self.client.fetch_funding_rate(
                str(request["instrument"]), params=dict(request.get("params", {}))
            )
            return {"funding": funding.__dict__}
        if operation == "fetch_ledger":
            cashflows = self.client.fetch_ledger(
                code=request.get("code"),
                since_ms=None if request.get("since_ms") is None else int(request["since_ms"]),
                limit=None if request.get("limit") is None else int(request["limit"]),
                params=dict(request.get("params", {})),
            )
            return {"cashflows": [cashflow.__dict__ for cashflow in cashflows]}
        if operation == "fetch_funding_history":
            cashflows = self.client.fetch_funding_history(
                request.get("instrument"),
                since_ms=None if request.get("since_ms") is None else int(request["since_ms"]),
                limit=None if request.get("limit") is None else int(request["limit"]),
                params=dict(request.get("params", {})),
            )
            return {"cashflows": [cashflow.__dict__ for cashflow in cashflows]}
        if operation == "fetch_leverage_tiers":
            tiers = self.client.fetch_leverage_tiers(
                str(request["instrument"]), params=dict(request.get("params", {}))
            )
            return {"tiers": [tier.__dict__ for tier in tiers]}
        if operation == "fetch_open_orders":
            orders = self.client.fetch_open_orders(
                request.get("instrument"), params=dict(request.get("params", {}))
            )
            return {"orders": [order.__dict__ for order in orders]}
        raise CcxtConnectorError(CcxtErrorClass.UNSUPPORTED, f"不支持的 CCXT Worker 操作: {operation}")

    async def handle_async(self, request: dict[str, Any]) -> dict[str, Any]:
        operation = str(request.get("op", "")).strip()
        if not operation.startswith("watch_"):
            return self.handle(request)
        stream = operation.removeprefix("watch_")
        payload = await self._watch_with_reconnect(request, stream)
        return {
            "event": normalize_stream_event(
                stream,
                payload,
                exchange_id=self.client.config.exchange_id,
                received_ts=request.get("received_ts"),
            )
        }

    async def _watch_with_reconnect(self, request: dict[str, Any], stream: str) -> Any:
        attempts = 0
        while True:
            if self.pro_exchange is None:
                self.pro_exchange = create_ccxt_pro_exchange(self.client.config)
            try:
                return await watch_stream(
                    self.pro_exchange,
                    stream,
                    instrument=request.get("instrument"),
                    timeframe=str(request.get("timeframe", "1m")),
                    limit=None if request.get("limit") is None else int(request["limit"]),
                    params=dict(request.get("params", {})),
                )
            except CcxtConnectorError as error:
                retryable = error.error_class in {
                    CcxtErrorClass.RETRYABLE,
                    CcxtErrorClass.RATE_LIMITED,
                }
                if not retryable or attempts >= self.client.config.ws_max_retries:
                    raise
                await self._close_pro_async()
                delay_ms = self.client.config.ws_retry_backoff_ms * (2**attempts)
                if delay_ms:
                    await asyncio.sleep(delay_ms / 1_000)
                attempts += 1

    async def _close_pro_async(self) -> None:
        if self.pro_exchange is None:
            return
        close = getattr(self.pro_exchange, "close", None)
        self.pro_exchange = None
        if close is not None and callable(close):
            result = close()
            if hasattr(result, "__await__"):
                await result

    async def close_async(self) -> None:
        await self._close_pro_async()

    async def handle_line_async(self, line: str) -> str:
        try:
            request = json.loads(line)
            if not isinstance(request, dict):
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "请求必须是 JSON object")
            return json.dumps(
                {"ok": True, "result": await self.handle_async(request)},
                separators=(",", ":"),
                ensure_ascii=False,
            )
        except CcxtConnectorError as error:
            return json.dumps(
                {
                    "ok": False,
                    "error": {"class": error.error_class.value, "message": str(error)},
                },
                separators=(",", ":"),
                ensure_ascii=False,
            )
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
            return json.dumps(
                {"ok": False, "error": {"class": "invalid", "message": str(error)}},
                separators=(",", ":"),
                ensure_ascii=False,
            )

    def handle_line(self, line: str) -> str:
        return asyncio.run(self.handle_line_async(line))


def _config_from_file(path: Path) -> CcxtConfig:
    value = json.loads(path.read_text(encoding="utf-8"))
    credentials = value.get("credential_env") or {}
    if not isinstance(credentials, dict):
        raise ValueError("credential_env must be an object or null")

    def env(name: str) -> str:
        return os.environ.get(str(credentials.get(name, "")), "") if credentials.get(name) else ""

    return CcxtConfig(
        exchange_id=str(value["exchange_id"]),
        api_key=env("api_key"),
        secret=env("secret"),
        password=env("password"),
        uid=env("uid"),
        sandbox=bool(value.get("sandbox", False)),
        enable_rate_limit=bool(value.get("enable_rate_limit", True)),
        timeout_ms=int(value.get("timeout_ms", 30_000)),
        default_type=str(value.get("default_type", "spot")),
        options=dict(value.get("options", {})),
        ws_max_retries=int(value.get("ws_max_retries", 3)),
        ws_retry_backoff_ms=int(value.get("ws_retry_backoff_ms", 250)),
    )


def serve(worker: CcxtJsonWorker, input_stream: TextIO, output_stream: TextIO) -> None:
    async def run() -> None:
        for line in input_stream:
            if not line.strip():
                continue
            output_stream.write(await worker.handle_line_async(line))
            output_stream.write("\n")
            output_stream.flush()
        await worker.close_async()

    asyncio.run(run())


def main() -> int:
    parser = argparse.ArgumentParser(description="Qianxing public CCXT JSONL worker")
    parser.add_argument("--config", required=True, type=Path)
    args = parser.parse_args()
    worker = CcxtJsonWorker(CcxtExchangeClient(_config_from_file(args.config)))
    serve(worker, sys.stdin, sys.stdout)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
