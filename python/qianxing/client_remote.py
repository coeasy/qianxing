"""Typed HTTP client and remote Rust run lifecycle handles."""

from __future__ import annotations

import asyncio
import math
import time
from typing import Any, Generic, Mapping
from urllib.parse import urlencode

from .app import (
    APP_ROUTE_TEMPLATES,
    AppError,
    BacktestOutcome,
    BacktestSpec,
    BuiltinStrategyInfo,
    BuiltinStrategyRecipe,
    CompareRunsResult,
    CompareRunsSpec,
    DatasetSpec,
    DatasetVerdict,
    DepthBacktestOutcome,
    DepthBacktestSpec,
    InternalInvariantError,
    MultiAccountBacktestResult,
    MultiAccountBacktestSpec,
    MultiAccountDepthBacktestResult,
    MultiAccountDepthBacktestSpec,
    MultiAccountJointBacktestOutcome,
    MultiAccountJointBacktestSpec,
    MultiLegBacktestOutcome,
    MultiLegBacktestSpec,
    MultiMarketBacktestOutcome,
    MultiMarketBacktestSpec,
    MultiMarketDepthBacktestOutcome,
    MultiMarketDepthBacktestSpec,
    QianxingError,
    RiskParityResult,
    RiskParitySpec,
    RunDepthExperimentResult,
    RunDepthExperimentSpec,
    RunExperimentResult,
    RunExperimentSpec,
    RunMultiLegExperimentResult,
    RunMultiLegExperimentSpec,
    RunMultiMarketDepthExperimentResult,
    RunMultiMarketDepthExperimentSpec,
    RunMultiMarketExperimentResult,
    RunMultiMarketExperimentSpec,
    VerificationResult,
    _ACCOUNT_READ_PATHS,
    _APP_ERRORS,
    _ResultT,
    _parse_builtin_strategy_catalog,
    _parse_builtin_strategy_recipes,
)


from .client_transport import _HttpJsonClient


class RemoteRunHandle(Generic[_ResultT]):
    """Bounded HTTP lifecycle handle backed by the Rust API run registry."""

    def __init__(
        self,
        client: "AppClient",
        run_id: str,
        result_type: type[_ResultT] | None = None,
    ):
        if not run_id or any(not (char.isascii() and (char.isalnum() or char in "-_.")) for char in run_id):
            raise ValueError("run_id must contain only ASCII letters, digits, '.', '_' or '-'")
        self._client = client
        self.run_id = run_id
        self._result_type = result_type

    def document(self) -> dict[str, Any]:
        return self._client._request_json(
            APP_ROUTE_TEMPLATES["run_status"].format(run_id=self.run_id), method="GET"
        )

    async def async_document(self) -> dict[str, Any]:
        """Read the same Rust run document without blocking the caller's event loop."""
        return await asyncio.to_thread(self.document)

    def events(self, after_seq: int = 0) -> list[dict[str, Any]]:
        if isinstance(after_seq, bool) or not isinstance(after_seq, int) or after_seq < 0:
            raise ValueError("after_seq must be a non-negative integer")
        route = APP_ROUTE_TEMPLATES["run_events"].format(run_id=self.run_id)
        return self._client._request_json(f"{route}?after_seq={after_seq}", method="GET")

    async def async_events(self, after_seq: int = 0) -> list[dict[str, Any]]:
        return await asyncio.to_thread(self.events, after_seq)

    @property
    def status(self) -> str:
        return str(self.document().get("status", "unknown"))

    async def async_status(self) -> str:
        return str((await self.async_document()).get("status", "unknown"))

    def cancel(self) -> dict[str, Any]:
        return self._client._request_json(
            APP_ROUTE_TEMPLATES["run_cancel"].format(run_id=self.run_id),
            method="POST",
            payload={},
        )

    async def async_cancel(self) -> dict[str, Any]:
        return await asyncio.to_thread(self.cancel)

    def wait(self, timeout: float = 30.0, poll_interval: float = 0.25) -> str:
        if (
            not math.isfinite(timeout)
            or not math.isfinite(poll_interval)
            or timeout <= 0
            or poll_interval <= 0
        ):
            raise ValueError("timeout and poll_interval must be finite and positive")
        deadline = time.monotonic() + timeout
        while True:
            status = self.status
            if status in {"succeeded", "cancelled", "failed"}:
                return status
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return status
            time.sleep(min(poll_interval, remaining))

    async def async_wait(self, timeout: float = 30.0, poll_interval: float = 0.25) -> str:
        """Await terminal Rust status with the same finite bounds as ``wait``."""
        if (
            not math.isfinite(timeout)
            or not math.isfinite(poll_interval)
            or timeout <= 0
            or poll_interval <= 0
        ):
            raise ValueError("timeout and poll_interval must be finite and positive")
        deadline = time.monotonic() + timeout
        while True:
            status = await self.async_status()
            if status in {"succeeded", "cancelled", "failed"}:
                return status
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return status
            await asyncio.sleep(min(poll_interval, remaining))

    def result(self) -> _ResultT | dict[str, Any] | None:
        document = self.document()
        return self._result_from_document(document)

    async def async_result(self) -> _ResultT | dict[str, Any] | None:
        return self._result_from_document(await self.async_document())

    def _result_from_document(self, document: dict[str, Any]) -> _ResultT | dict[str, Any] | None:
        status = document.get("status")
        if status == "failed":
            error = document["error"]
            if not isinstance(error, dict):
                raise InternalInvariantError(
                    "Rust API reported a failed run without an error document",
                    payload={"run_id": self.run_id, "status": status},
                )
            error_type = _APP_ERRORS.get(error.get("category"), AppError)
            raise error_type(error.get("message", "Rust run failed"), payload=error)
        if status != "succeeded":
            return None
        result = document.get("result")
        if not isinstance(result, dict):
            raise InternalInvariantError(
                "Rust API reported a successful run without a result document",
                payload={"run_id": self.run_id, "status": status},
            )
        if self._result_type is not None:
            return self._result_type.from_dict(result)
        return result


class AppClient(_HttpJsonClient):
    """HTTP facade over the same Rust API use cases exposed by the native SDK."""

    def _post(self, route: str, spec: Any) -> dict[str, Any]:
        return self._request_json(route, method="POST", payload=spec)

    def get_strategy_deployments(self, worker_id: str | None = None) -> list[dict[str, Any]]:
        """Read observed process state; it does not grant live trading readiness."""
        route = "/strategies/deployments"
        if worker_id is not None:
            if not isinstance(worker_id, str) or not worker_id.strip():
                raise ValueError("worker_id must be non-empty text")
            route += "?" + urlencode({"worker_id": worker_id})
        rows = self._request_json_value(route, method="GET")
        states = {"unobserved", "initializing", "running", "paused", "stopped",
                  "failed", "stale", "configuration_changed", "disabled", "ownership_conflict"}
        if not isinstance(rows, list) or any(
            not isinstance(row, dict) or type(row.get("schema_version")) is not int
            or row["schema_version"] != 1 or not isinstance(row.get("state"), str)
            or row["state"] not in states
            or not isinstance(row.get("worker_id"), str) or not row["worker_id"].strip()
            or "trading_enabled" not in row
            or (row["trading_enabled"] is not None and type(row["trading_enabled"]) is not bool)
            or not isinstance(row.get("blocking_reasons"), list)
            or any(not isinstance(reason, str) for reason in row["blocking_reasons"])
            or (worker_id is not None and row["worker_id"] != worker_id)
            for row in rows
        ):
            raise QianxingError("Rust API returned invalid strategy deployment status")
        return rows

    async def async_get_strategy_deployments(self, worker_id: str | None = None) -> list[dict[str, Any]]:
        return await asyncio.to_thread(self.get_strategy_deployments, worker_id)

    def application_capabilities(self) -> dict[str, Any]:
        """Discover Rust use cases; venue/provider readiness remains runtime-specific."""
        value = self._request_json_value(
            APP_ROUTE_TEMPLATES["application_capabilities"], method="GET"
        )
        if (
            not isinstance(value, dict)
            or value.get("schema_version") != 1
            or value.get("execution_backend") != "shared-rust"
            or value.get("runtime_preflight_required") is not True
            or not isinstance(value.get("capabilities"), list)
            or not isinstance(value.get("limitations"), list)
        ):
            raise QianxingError(
                "Rust API returned an invalid application capability catalog",
                payload={"category": "INTERNAL_INVARIANT"},
            )
        return value

    async def async_application_capabilities(self) -> dict[str, Any]:
        return await asyncio.to_thread(self.application_capabilities)

    def query_product_capability(
        self,
        capability_id: str | None = None,
        *,
        venue: str | None = None,
        product: str | None = None,
        account_mode: str | None = None,
        account_model: str | None = None,
        margin_mode: str | None = None,
    ) -> dict[str, Any]:
        """Query capability evidence by stable ID or declared execution scope."""
        query = {
            name: value
            for name, value in (
                ("capability_id", capability_id),
                ("venue", venue),
                ("product", product),
                ("account_mode", account_mode),
                ("account_model", account_model),
                ("margin_mode", margin_mode),
            )
            if value is not None
        }
        if not query:
            raise ValueError("provide capability_id or at least one scope filter")
        for name, value in query.items():
            if not isinstance(value, str) or not value.strip():
                raise ValueError(f"{name} must be a non-empty string")
        return self._post(
            APP_ROUTE_TEMPLATES["product_capability_query"],
            query,
        )

    async def async_query_product_capability(
        self,
        capability_id: str | None = None,
        *,
        venue: str | None = None,
        product: str | None = None,
        account_mode: str | None = None,
        account_model: str | None = None,
        margin_mode: str | None = None,
    ) -> dict[str, Any]:
        return await asyncio.to_thread(
            self.query_product_capability,
            capability_id,
            venue=venue,
            product=product,
            account_mode=account_mode,
            account_model=account_model,
            margin_mode=margin_mode,
        )

    def provider_preflight(self, data_request: Mapping[str, Any]) -> dict[str, Any]:
        """Check a data request against configured Rust Providers without I/O."""
        return self._post(APP_ROUTE_TEMPLATES["provider_preflight"], data_request)

    async def async_provider_preflight(self, data_request: Mapping[str, Any]) -> dict[str, Any]:
        return await asyncio.to_thread(self.provider_preflight, data_request)

    def list_builtin_strategies(self) -> tuple[BuiltinStrategyInfo, ...]:
        """Fetch the same Rust-owned strategy catalog used by local SDK calls."""
        value = self._request_json_value(
            APP_ROUTE_TEMPLATES["builtin_strategy_catalog"], method="GET"
        )
        try:
            return _parse_builtin_strategy_catalog(value)
        except (TypeError, ValueError) as exc:
            raise QianxingError(
                "Rust API returned an invalid built-in strategy catalog",
                payload={"category": "INTERNAL_INVARIANT"},
            ) from exc

    async def async_list_builtin_strategies(self) -> tuple[BuiltinStrategyInfo, ...]:
        return await asyncio.to_thread(self.list_builtin_strategies)

    def list_builtin_strategy_recipes(self) -> tuple[BuiltinStrategyRecipe, ...]:
        """Fetch versioned strategy defaults, data requirements, and risk/cost notes."""
        value = self._request_json_value(
            APP_ROUTE_TEMPLATES["builtin_strategy_recipes"], method="GET"
        )
        try:
            return _parse_builtin_strategy_recipes(value)
        except (TypeError, ValueError) as exc:
            raise QianxingError(
                "Rust API returned an invalid built-in strategy recipe catalog",
                payload={"category": "INTERNAL_INVARIANT"},
            ) from exc

    async def async_list_builtin_strategy_recipes(self) -> tuple[BuiltinStrategyRecipe, ...]:
        return await asyncio.to_thread(self.list_builtin_strategy_recipes)

    def get_live_events(self, after_seq: int | None = None) -> list[dict[str, Any]]:
        if after_seq is not None and (
            isinstance(after_seq, bool)
            or not isinstance(after_seq, int)
            or after_seq < 0
            or after_seq > (1 << 64) - 1
        ):
            raise ValueError("after_seq must be a u64 sequence or None")
        route = APP_ROUTE_TEMPLATES["live_events"]
        if after_seq is not None:
            route = f"{route}?after={after_seq}"
        events = self._request_json_value(route, method="GET")
        if not isinstance(events, list) or any(not isinstance(event, dict) for event in events):
            raise QianxingError("Rust API returned an invalid live event page")
        return events

    async def async_get_live_events(self, after_seq: int | None = None) -> list[dict[str, Any]]:
        return await asyncio.to_thread(self.get_live_events, after_seq)

    @staticmethod
    def _account_scope_query(
        account_id: str | None, venue_id: str | None
    ) -> str:
        if (account_id is None) != (venue_id is None):
            raise ValueError("account_id and venue_id must be supplied together")
        if account_id is None:
            return ""
        if (
            not isinstance(account_id, str)
            or not isinstance(venue_id, str)
            or not account_id.strip()
            or not venue_id.strip()
        ):
            raise ValueError("account_id and venue_id must be non-empty")
        return "?" + urlencode((
            ("account_id", account_id),
            ("venue_id", venue_id),
        ))

    def _get_account_list(
        self, key: str, *, account_id: str | None, venue_id: str | None
    ) -> list[dict[str, Any]]:
        path = _ACCOUNT_READ_PATHS[key] + self._account_scope_query(account_id, venue_id)
        value = self._request_json_value(path, method="GET")
        if not isinstance(value, list) or any(not isinstance(item, dict) for item in value):
            raise QianxingError(f"Rust API returned an invalid {key} read model")
        return value

    def get_account_snapshot(
        self, *, account_id: str | None = None, venue_id: str | None = None
    ) -> dict[str, Any]:
        """Read the current account projection, optionally scoped to one account/venue pair."""
        path = _ACCOUNT_READ_PATHS["snapshot"] + self._account_scope_query(account_id, venue_id)
        return self._request_json(path, method="GET")

    async def async_get_account_snapshot(
        self, *, account_id: str | None = None, venue_id: str | None = None
    ) -> dict[str, Any]:
        return await asyncio.to_thread(
            self.get_account_snapshot, account_id=account_id, venue_id=venue_id
        )

    def get_account_balances(
        self, *, account_id: str | None = None, venue_id: str | None = None
    ) -> dict[str, Any]:
        """Read the balance summary, preserving per-currency cash and nullable totals."""
        path = _ACCOUNT_READ_PATHS["balances"] + self._account_scope_query(
            account_id, venue_id
        )
        return self._request_json(path, method="GET")

    async def async_get_account_balances(
        self, *, account_id: str | None = None, venue_id: str | None = None
    ) -> dict[str, Any]:
        return await asyncio.to_thread(
            self.get_account_balances, account_id=account_id, venue_id=venue_id
        )

    def get_account_positions(
        self, *, account_id: str | None = None, venue_id: str | None = None
    ) -> list[dict[str, Any]]:
        """Read positions from one account or the configured default projection."""
        return self._get_account_list(
            "positions", account_id=account_id, venue_id=venue_id
        )

    async def async_get_account_positions(
        self, *, account_id: str | None = None, venue_id: str | None = None
    ) -> list[dict[str, Any]]:
        return await asyncio.to_thread(
            self.get_account_positions, account_id=account_id, venue_id=venue_id
        )

    def get_account_orders(
        self, *, account_id: str | None = None, venue_id: str | None = None
    ) -> list[dict[str, Any]]:
        """Read the account's current order projection."""
        return self._get_account_list("orders", account_id=account_id, venue_id=venue_id)

    async def async_get_account_orders(
        self, *, account_id: str | None = None, venue_id: str | None = None
    ) -> list[dict[str, Any]]:
        return await asyncio.to_thread(
            self.get_account_orders, account_id=account_id, venue_id=venue_id
        )

    def get_account_ledger(self, *, account_id: str | None = None) -> list[dict[str, Any]]:
        """Read ledger facts; the API scopes ledger rows by account_id only."""
        path = _ACCOUNT_READ_PATHS["ledger"]
        if account_id is not None:
            if not isinstance(account_id, str) or not account_id.strip():
                raise ValueError("account_id must be non-empty")
            path += "?" + urlencode((("account_id", account_id),))
        value = self._request_json_value(path, method="GET")
        if not isinstance(value, list) or any(not isinstance(item, dict) for item in value):
            raise QianxingError("Rust API returned an invalid ledger read model")
        return value

    async def async_get_account_ledger(
        self, *, account_id: str | None = None
    ) -> list[dict[str, Any]]:
        return await asyncio.to_thread(self.get_account_ledger, account_id=account_id)

    def get_reconcile_reports(
        self, *, account_id: str | None = None, venue_id: str | None = None
    ) -> list[dict[str, Any]]:
        """Read reconciliation reports, optionally filtered by account and venue."""
        return self._get_account_list(
            "reconcile_reports", account_id=account_id, venue_id=venue_id
        )

    async def async_get_reconcile_reports(
        self, *, account_id: str | None = None, venue_id: str | None = None
    ) -> list[dict[str, Any]]:
        return await asyncio.to_thread(
            self.get_reconcile_reports, account_id=account_id, venue_id=venue_id
        )

    def get_account_portfolio(
        self, portfolio_id: str, *, reporting_currency: str | None = None
    ) -> dict[str, Any]:
        """Read a portfolio rollup while preserving each account's native currency rows."""
        if not isinstance(portfolio_id, str) or not portfolio_id.strip():
            raise ValueError("portfolio_id must be non-empty")
        query = [("portfolio_id", portfolio_id)]
        if reporting_currency is not None:
            if (
                not isinstance(reporting_currency, str)
                or not reporting_currency.strip()
            ):
                raise ValueError("reporting_currency must be non-empty")
            query.append(("reporting_currency", reporting_currency))
        path = _ACCOUNT_READ_PATHS["portfolio"] + "?" + urlencode(query)
        return self._request_json(path, method="GET")

    async def async_get_account_portfolio(
        self, portfolio_id: str, *, reporting_currency: str | None = None
    ) -> dict[str, Any]:
        return await asyncio.to_thread(
            self.get_account_portfolio,
            portfolio_id,
            reporting_currency=reporting_currency,
        )

    def get_account_activity(
        self,
        *,
        account_id: str,
        venue_id: str,
        after: int | None = None,
        before: int | None = None,
        client_order_id: int | None = None,
        exchange_order_id: str | None = None,
        limit: int = 50,
    ) -> dict[str, Any]:
        """Read one account report, optionally filtering events by order identity."""
        if not isinstance(account_id, str) or not account_id.strip():
            raise ValueError("account_id must be non-empty")
        if not isinstance(venue_id, str) or not venue_id.strip():
            raise ValueError("venue_id must be non-empty")
        if after is not None and before is not None:
            raise ValueError("after and before are mutually exclusive")
        if exchange_order_id is not None and (
            not isinstance(exchange_order_id, str) or not exchange_order_id.strip()
        ):
            raise ValueError("exchange_order_id must be a non-empty string")
        for name, value in (
            ("after", after),
            ("before", before),
            ("client_order_id", client_order_id),
        ):
            if value is not None and (
                isinstance(value, bool) or not isinstance(value, int) or value < 0
            ):
                raise ValueError(f"{name} must be a non-negative integer")
        if isinstance(limit, bool) or not isinstance(limit, int) or not 1 <= limit <= 300:
            raise ValueError("limit must be between 1 and 300")

        query = [("account_id", account_id), ("venue_id", venue_id)]
        if after is not None:
            query.append(("after", str(after)))
        if before is not None:
            query.append(("before", str(before)))
        if client_order_id is not None:
            query.append(("client_order_id", str(client_order_id)))
        if exchange_order_id is not None:
            query.append(("exchange_order_id", exchange_order_id))
        query.append(("limit", str(limit)))
        path = _ACCOUNT_READ_PATHS["activity"] + "?" + urlencode(query)
        return self._request_json(path, method="GET")

    async def async_get_account_activity(
        self,
        *,
        account_id: str,
        venue_id: str,
        after: int | None = None,
        before: int | None = None,
        client_order_id: int | None = None,
        exchange_order_id: str | None = None,
        limit: int = 50,
    ) -> dict[str, Any]:
        return await asyncio.to_thread(
            self.get_account_activity,
            account_id=account_id,
            venue_id=venue_id,
            after=after,
            before=before,
            client_order_id=client_order_id,
            exchange_order_id=exchange_order_id,
            limit=limit,
        )

    def _start(self, route: str, spec: Any, result_type: type[_ResultT]) -> RemoteRunHandle[_ResultT]:
        document = self._post(route, spec)
        run_id = document.get("run_id")
        if not isinstance(run_id, str):
            raise QianxingError("Rust API start response omitted run_id", payload=document)
        return RemoteRunHandle(self, run_id, result_type)

    async def _async_start(self, method: Any, spec: Any) -> RemoteRunHandle[Any]:
        task = asyncio.create_task(asyncio.to_thread(method, spec))
        try:
            return await asyncio.shield(task)
        except asyncio.CancelledError:
            # The HTTP request itself cannot be interrupted once urllib has sent it. If the
            # server accepted the run, request cooperative cancellation before propagating the
            # caller's cancellation so a detached worker is not silently left behind.
            try:
                handle = await task
                await handle.async_cancel()
            except Exception:
                pass
            raise

    def validate_dataset(self, spec: DatasetSpec | dict[str, Any]) -> DatasetVerdict:
        return DatasetVerdict.from_dict(self._post(APP_ROUTE_TEMPLATES["validate_dataset"], spec))

    def run_backtest(self, spec: BacktestSpec | dict[str, Any]) -> BacktestOutcome:
        return BacktestOutcome.from_dict(self._post(APP_ROUTE_TEMPLATES["backtest"], spec))

    def run_provider_backtest_with_manifest(
        self, spec: dict[str, Any]
    ) -> dict[str, Any]:
        """Run the shared Rust Provider selection, freeze and backtest workflow remotely."""
        return self._post(APP_ROUTE_TEMPLATES["provider_backtest"], spec)

    def run_provider_depth_backtest(self, spec: Mapping[str, Any]) -> dict[str, Any]:
        """Fetch Provider Quote snapshots and run the shared Rust depth backtest."""
        return self._post(APP_ROUTE_TEMPLATES["provider_depth_backtest"], spec)

    def read_provider_run_report_v1(self, outcome: Mapping[str, Any]) -> dict[str, Any]:
        """Read the shared verified Provider Bar report DTO through the Rust API."""
        return self._post(APP_ROUTE_TEMPLATES["read_provider_report"], outcome)

    async def async_read_provider_run_report_v1(
        self, outcome: Mapping[str, Any]
    ) -> dict[str, Any]:
        return await asyncio.to_thread(self.read_provider_run_report_v1, outcome)

    def read_provider_depth_run_report_v1(
        self, outcome: Mapping[str, Any]
    ) -> dict[str, Any]:
        """Read the shared verified Provider Depth report DTO through the Rust API."""
        return self._post(APP_ROUTE_TEMPLATES["read_provider_depth_report"], outcome)

    async def async_read_provider_depth_run_report_v1(
        self, outcome: Mapping[str, Any]
    ) -> dict[str, Any]:
        return await asyncio.to_thread(self.read_provider_depth_run_report_v1, outcome)

    def read_multi_account_joint_report_v1(
        self, outcome: Mapping[str, Any]
    ) -> dict[str, Any]:
        """Read the replay-verified, versioned joint-account report DTO through Rust."""
        return self._post(APP_ROUTE_TEMPLATES["read_multi_account_joint_report"], outcome)

    async def async_read_multi_account_joint_report_v1(
        self, outcome: Mapping[str, Any]
    ) -> dict[str, Any]:
        return await asyncio.to_thread(self.read_multi_account_joint_report_v1, outcome)

    def read_multi_leg_research_report_v1(
        self, request: Mapping[str, Any]
    ) -> dict[str, Any]:
        """Read the replay-verified, versioned multi-leg report DTO through Rust."""
        return self._post(APP_ROUTE_TEMPLATES["read_multi_leg_report"], request)

    async def async_read_multi_leg_research_report_v1(
        self, request: Mapping[str, Any]
    ) -> dict[str, Any]:
        return await asyncio.to_thread(self.read_multi_leg_research_report_v1, request)

    def start_backtest(self, spec: BacktestSpec | dict[str, Any]) -> RemoteRunHandle[BacktestOutcome]:
        return self._start(APP_ROUTE_TEMPLATES["backtest_start"], spec, BacktestOutcome)

    def verify_run(self, outcome: BacktestOutcome | dict[str, Any]) -> VerificationResult:
        return VerificationResult.from_dict(self._post(APP_ROUTE_TEMPLATES["verify"], outcome))

    def research_artifacts(self, outcome: BacktestOutcome | Mapping[str, Any]) -> dict[str, Any]:
        """Load replay-verified bounded research details for a completed backtest."""
        return self._post(APP_ROUTE_TEMPLATES["research_artifacts"], outcome)

    async def async_research_artifacts(
        self, outcome: BacktestOutcome | Mapping[str, Any]
    ) -> dict[str, Any]:
        return await asyncio.to_thread(self.research_artifacts, outcome)

    def compare_runs(self, spec: CompareRunsSpec | dict[str, Any]) -> CompareRunsResult:
        return CompareRunsResult.from_dict(self._post(APP_ROUTE_TEMPLATES["compare_runs"], spec))

    def plan_risk_parity(self, spec: RiskParitySpec | dict[str, Any]) -> RiskParityResult:
        return RiskParityResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["plan_risk_parity"], spec)
        )

    def plan_algo_execution(self, spec: dict[str, Any]) -> dict[str, Any]:
        return self._post(APP_ROUTE_TEMPLATES["plan_algo_execution"], spec)

    def plan_capital_allocation(self, spec: dict[str, Any]) -> dict[str, Any]:
        return self._post(APP_ROUTE_TEMPLATES["plan_capital_allocation"], spec)

    def start_paper_algo_execution(self, request: dict[str, Any]) -> dict[str, Any]:
        """Persist a shared Rust TWAP/VWAP parent for the configured Paper worker."""
        return self._post(APP_ROUTE_TEMPLATES["paper_algo_start"], request)

    def get_paper_algo_execution(self, parent_id: str) -> dict[str, Any]:
        return self._post(APP_ROUTE_TEMPLATES["paper_algo_get"], {"parent_id": parent_id})

    def cancel_paper_algo_execution(self, parent_id: str) -> dict[str, Any]:
        return self._post(APP_ROUTE_TEMPLATES["paper_algo_cancel"], {"parent_id": parent_id})

    def list_paper_algo_executions(
        self, *, after_parent_id: str | None = None, limit: int = 100
    ) -> dict[str, Any]:
        return self._post(
            APP_ROUTE_TEMPLATES["paper_algo_list"],
            {"after_parent_id": after_parent_id, "limit": limit},
        )

    def run_experiment(self, spec: RunExperimentSpec | dict[str, Any]) -> RunExperimentResult:
        return RunExperimentResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["run_experiment"], spec)
        )

    def start_experiment(self, spec: RunExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunExperimentResult]:
        return self._start(APP_ROUTE_TEMPLATES["run_experiment_start"], spec, RunExperimentResult)

    def run_depth_backtest(self, spec: DepthBacktestSpec | dict[str, Any]) -> DepthBacktestOutcome:
        return DepthBacktestOutcome.from_dict(
            self._post(APP_ROUTE_TEMPLATES["depth_backtest"], spec)
        )

    def start_depth_backtest(self, spec: DepthBacktestSpec | dict[str, Any]) -> RemoteRunHandle[DepthBacktestOutcome]:
        return self._start(APP_ROUTE_TEMPLATES["depth_backtest_start"], spec, DepthBacktestOutcome)

    def verify_depth_run(self, outcome: DepthBacktestOutcome | dict[str, Any]) -> VerificationResult:
        return VerificationResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["verify_depth"], outcome)
        )

    def run_depth_experiment(self, spec: RunDepthExperimentSpec | dict[str, Any]) -> RunDepthExperimentResult:
        return RunDepthExperimentResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["depth_experiment"], spec)
        )

    def start_depth_experiment(self, spec: RunDepthExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunDepthExperimentResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["depth_experiment_start"], spec, RunDepthExperimentResult
        )

    def run_multi_leg_backtest(self, spec: MultiLegBacktestSpec | dict[str, Any]) -> MultiLegBacktestOutcome:
        return MultiLegBacktestOutcome.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_leg_backtest"], spec)
        )

    def run_multi_market_backtest(
        self, spec: MultiMarketBacktestSpec | dict[str, Any]
    ) -> MultiMarketBacktestOutcome:
        return MultiMarketBacktestOutcome.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_market_backtest"], spec)
        )

    def start_multi_market_backtest(
        self, spec: MultiMarketBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiMarketBacktestOutcome]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_market_backtest_start"], spec, MultiMarketBacktestOutcome
        )

    def run_multi_market_depth_backtest(
        self, spec: MultiMarketDepthBacktestSpec | dict[str, Any]
    ) -> MultiMarketDepthBacktestOutcome:
        return MultiMarketDepthBacktestOutcome.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_market_depth_backtest"], spec)
        )

    def start_multi_market_depth_backtest(
        self, spec: MultiMarketDepthBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiMarketDepthBacktestOutcome]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_market_depth_backtest_start"],
            spec,
            MultiMarketDepthBacktestOutcome,
        )

    def run_multi_market_experiment(self, spec: RunMultiMarketExperimentSpec | dict[str, Any]) -> RunMultiMarketExperimentResult:
        return RunMultiMarketExperimentResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_market_experiment"], spec)
        )

    def start_multi_market_experiment(self, spec: RunMultiMarketExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunMultiMarketExperimentResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_market_experiment_start"], spec, RunMultiMarketExperimentResult
        )

    def run_multi_market_depth_experiment(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any]
    ) -> RunMultiMarketDepthExperimentResult:
        return RunMultiMarketDepthExperimentResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_market_depth_experiment"], spec)
        )

    def start_multi_market_depth_experiment(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any]
    ) -> RemoteRunHandle[RunMultiMarketDepthExperimentResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_market_depth_experiment_start"],
            spec,
            RunMultiMarketDepthExperimentResult,
        )

    def run_multi_account_backtest(
        self, spec: MultiAccountBacktestSpec | dict[str, Any]
    ) -> MultiAccountBacktestResult:
        return MultiAccountBacktestResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_account_backtest"], spec)
        )

    def run_multi_account_joint_backtest(
        self, spec: MultiAccountJointBacktestSpec | dict[str, Any]
    ) -> MultiAccountJointBacktestOutcome:
        return MultiAccountJointBacktestOutcome.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_account_joint_backtest"], spec)
        )

    def start_multi_account_joint_backtest(
        self, spec: MultiAccountJointBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiAccountJointBacktestOutcome]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_account_joint_backtest_start"],
            spec,
            MultiAccountJointBacktestOutcome,
        )

    def start_multi_account_backtest(
        self, spec: MultiAccountBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiAccountBacktestResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_account_backtest_start"],
            spec,
            MultiAccountBacktestResult,
        )

    def run_multi_account_depth_backtest(
        self, spec: MultiAccountDepthBacktestSpec | dict[str, Any]
    ) -> MultiAccountDepthBacktestResult:
        return MultiAccountDepthBacktestResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_account_depth_backtest"], spec)
        )

    def start_multi_account_depth_backtest(
        self, spec: MultiAccountDepthBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiAccountDepthBacktestResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_account_depth_backtest_start"],
            spec,
            MultiAccountDepthBacktestResult,
        )

    def start_multi_leg_backtest(self, spec: MultiLegBacktestSpec | dict[str, Any]) -> RemoteRunHandle[MultiLegBacktestOutcome]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_leg_backtest_start"], spec, MultiLegBacktestOutcome
        )

    def run_multi_leg_experiment(self, spec: RunMultiLegExperimentSpec | dict[str, Any]) -> RunMultiLegExperimentResult:
        return RunMultiLegExperimentResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_leg_experiment"], spec)
        )

    def start_multi_leg_experiment(self, spec: RunMultiLegExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunMultiLegExperimentResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_leg_experiment_start"],
            spec,
            RunMultiLegExperimentResult,
        )

    # Async facade methods deliberately delegate to the synchronous typed methods above.
    # This keeps schemas, error translation, and Rust routes single-sourced.
    async def async_validate_dataset(self, spec: DatasetSpec | dict[str, Any]) -> DatasetVerdict:
        return await asyncio.to_thread(self.validate_dataset, spec)

    async def async_run_backtest(self, spec: BacktestSpec | dict[str, Any]) -> BacktestOutcome:
        return await asyncio.to_thread(self.run_backtest, spec)

    async def async_start_backtest(self, spec: BacktestSpec | dict[str, Any]) -> RemoteRunHandle[BacktestOutcome]:
        return await self._async_start(self.start_backtest, spec)

    async def async_verify_run(self, outcome: BacktestOutcome | dict[str, Any]) -> VerificationResult:
        return await asyncio.to_thread(self.verify_run, outcome)

    async def async_compare_runs(self, spec: CompareRunsSpec | dict[str, Any]) -> CompareRunsResult:
        return await asyncio.to_thread(self.compare_runs, spec)

    async def async_plan_risk_parity(self, spec: RiskParitySpec | dict[str, Any]) -> RiskParityResult:
        return await asyncio.to_thread(self.plan_risk_parity, spec)

    async def async_run_experiment(self, spec: RunExperimentSpec | dict[str, Any]) -> RunExperimentResult:
        return await asyncio.to_thread(self.run_experiment, spec)

    async def async_start_experiment(self, spec: RunExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunExperimentResult]:
        return await self._async_start(self.start_experiment, spec)

    async def async_run_depth_backtest(self, spec: DepthBacktestSpec | dict[str, Any]) -> DepthBacktestOutcome:
        return await asyncio.to_thread(self.run_depth_backtest, spec)

    async def async_start_depth_backtest(self, spec: DepthBacktestSpec | dict[str, Any]) -> RemoteRunHandle[DepthBacktestOutcome]:
        return await self._async_start(self.start_depth_backtest, spec)

    async def async_verify_depth_run(self, outcome: DepthBacktestOutcome | dict[str, Any]) -> VerificationResult:
        return await asyncio.to_thread(self.verify_depth_run, outcome)

    async def async_run_depth_experiment(self, spec: RunDepthExperimentSpec | dict[str, Any]) -> RunDepthExperimentResult:
        return await asyncio.to_thread(self.run_depth_experiment, spec)

    async def async_start_depth_experiment(self, spec: RunDepthExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunDepthExperimentResult]:
        return await self._async_start(self.start_depth_experiment, spec)

    async def async_run_multi_leg_backtest(self, spec: MultiLegBacktestSpec | dict[str, Any]) -> MultiLegBacktestOutcome:
        return await asyncio.to_thread(self.run_multi_leg_backtest, spec)

    async def async_run_multi_market_backtest(
        self, spec: MultiMarketBacktestSpec | dict[str, Any]
    ) -> MultiMarketBacktestOutcome:
        return await asyncio.to_thread(self.run_multi_market_backtest, spec)

    async def async_start_multi_market_backtest(
        self, spec: MultiMarketBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiMarketBacktestOutcome]:
        return await self._async_start(self.start_multi_market_backtest, spec)

    async def async_run_multi_market_depth_backtest(
        self, spec: MultiMarketDepthBacktestSpec | dict[str, Any]
    ) -> MultiMarketDepthBacktestOutcome:
        return await asyncio.to_thread(self.run_multi_market_depth_backtest, spec)

    async def async_start_multi_market_depth_backtest(
        self, spec: MultiMarketDepthBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiMarketDepthBacktestOutcome]:
        return await self._async_start(self.start_multi_market_depth_backtest, spec)

    async def async_run_multi_market_experiment(self, spec: RunMultiMarketExperimentSpec | dict[str, Any]) -> RunMultiMarketExperimentResult:
        return await asyncio.to_thread(self.run_multi_market_experiment, spec)

    async def async_start_multi_market_experiment(self, spec: RunMultiMarketExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunMultiMarketExperimentResult]:
        return await self._async_start(self.start_multi_market_experiment, spec)

    async def async_run_multi_market_depth_experiment(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any]
    ) -> RunMultiMarketDepthExperimentResult:
        return await asyncio.to_thread(self.run_multi_market_depth_experiment, spec)

    async def async_start_multi_market_depth_experiment(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any]
    ) -> RemoteRunHandle[RunMultiMarketDepthExperimentResult]:
        return await self._async_start(self.start_multi_market_depth_experiment, spec)

    async def async_run_multi_account_backtest(
        self, spec: MultiAccountBacktestSpec | dict[str, Any]
    ) -> MultiAccountBacktestResult:
        return await asyncio.to_thread(self.run_multi_account_backtest, spec)

    async def async_run_multi_account_joint_backtest(
        self, spec: MultiAccountJointBacktestSpec | dict[str, Any]
    ) -> MultiAccountJointBacktestOutcome:
        return await asyncio.to_thread(self.run_multi_account_joint_backtest, spec)

    async def async_start_multi_account_joint_backtest(
        self, spec: MultiAccountJointBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiAccountJointBacktestOutcome]:
        return await self._async_start(self.start_multi_account_joint_backtest, spec)

    async def async_start_multi_account_backtest(
        self, spec: MultiAccountBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiAccountBacktestResult]:
        return await self._async_start(self.start_multi_account_backtest, spec)

    async def async_run_multi_account_depth_backtest(
        self, spec: MultiAccountDepthBacktestSpec | dict[str, Any]
    ) -> MultiAccountDepthBacktestResult:
        return await asyncio.to_thread(self.run_multi_account_depth_backtest, spec)

    async def async_start_multi_account_depth_backtest(
        self, spec: MultiAccountDepthBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiAccountDepthBacktestResult]:
        return await self._async_start(self.start_multi_account_depth_backtest, spec)

    async def async_start_multi_leg_backtest(self, spec: MultiLegBacktestSpec | dict[str, Any]) -> RemoteRunHandle[MultiLegBacktestOutcome]:
        return await self._async_start(self.start_multi_leg_backtest, spec)

    async def async_run_multi_leg_experiment(self, spec: RunMultiLegExperimentSpec | dict[str, Any]) -> RunMultiLegExperimentResult:
        return await asyncio.to_thread(self.run_multi_leg_experiment, spec)

    async def async_start_multi_leg_experiment(self, spec: RunMultiLegExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunMultiLegExperimentResult]:
        return await self._async_start(self.start_multi_leg_experiment, spec)
