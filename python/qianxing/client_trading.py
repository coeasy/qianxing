"""Authenticated Paper and Live HTTP clients for the Rust control plane."""

from __future__ import annotations

import asyncio
from dataclasses import asdict, dataclass
from ssl import SSLContext
from typing import Any, Literal, Mapping
from urllib.parse import urlsplit

from .app import APP_ROUTE_TEMPLATES
from .client_transport import _HttpJsonClient

@dataclass(frozen=True)
class TradeCommand:
    """Existing Rust control-plane command contract; no Python execution logic is added."""

    command_id: int
    request_id: str
    operator_id: str
    reason: str
    kind: Literal[
        "SubmitOrder", "AmendOrder", "PauseStrategy", "ResumeStrategy", "ChangeRiskLimit",
        "CancelOrder", "ReconcileAccount", "RetryJob", "SwitchVenue",
    ]
    target: str
    payload: dict[str, str]
    permission: Literal["ReadOnly", "Research", "Trading", "Admin"] = "Trading"
    dry_run: bool = False

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class TradeWorkflowClient(_HttpJsonClient):
    """Thin HTTP client for the Rust API's authenticated Paper/Live control workflows."""

    def __init__(
        self,
        base_url: str,
        workflow: Literal["paper", "live"],
        *,
        headers: Mapping[str, str] | None = None,
        ssl_context: SSLContext | None = None,
        timeout: float = 30.0,
    ):
        if workflow not in ("paper", "live"):
            raise ValueError("workflow must be 'paper' or 'live'")
        if workflow == "live" and urlsplit(base_url).scheme != "https":
            raise ValueError("LiveClient requires HTTPS for every command request")
        super().__init__(
            base_url, headers=headers, ssl_context=ssl_context, timeout=timeout
        )
        self.workflow = workflow

    def submit(self, command: TradeCommand | dict[str, Any]) -> dict[str, Any]:
        """Submit through qx-api; Rust authenticates, validates, persists, and enqueues it."""
        command_document = command.to_dict() if isinstance(command, TradeCommand) else command
        # 工作流 → 路由键写成两个字面量分支。原写法 `APP_ROUTE_TEMPLATES[f"{self.workflow}_commands"]`
        # 把「工作流名」与「路由键名」用一条没写在任何地方的命名约定绑在一起：两个键在源码里
        # 没有字面量读者（门禁核不出它们到底有没有被用上），键改名时也只会拿到运行时 KeyError。
        route = (
            APP_ROUTE_TEMPLATES["paper_commands"]
            if self.workflow == "paper"
            else APP_ROUTE_TEMPLATES["live_commands"]
        )
        return self._request_json(
            route,
            method="POST",
            payload={"schema_version": 1, "workflow": self.workflow, "command": command_document},
        )

    async def async_submit(self, command: TradeCommand | dict[str, Any]) -> dict[str, Any]:
        """Submit through the Rust workflow without blocking the event loop.

        Cancelling this coroutine does not undo a request already sent to the server; callers
        must retain the command ``request_id`` and query the audit/run state after uncertainty.
        """
        return await asyncio.to_thread(self.submit, command)


class PaperClient(TradeWorkflowClient):
    def __init__(
        self,
        base_url: str,
        *,
        headers: Mapping[str, str] | None = None,
        ssl_context: SSLContext | None = None,
        timeout: float = 30.0,
    ):
        super().__init__(
            base_url, "paper", headers=headers, ssl_context=ssl_context, timeout=timeout
        )


class LiveClient(TradeWorkflowClient):
    def __init__(
        self,
        base_url: str,
        *,
        headers: Mapping[str, str] | None = None,
        ssl_context: SSLContext | None = None,
        timeout: float = 30.0,
    ):
        super().__init__(
            base_url, "live", headers=headers, ssl_context=ssl_context, timeout=timeout
        )

