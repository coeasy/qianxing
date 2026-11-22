"""Authenticated Paper and Live HTTP clients for the Rust control plane."""

from __future__ import annotations

import asyncio
import json
from dataclasses import asdict, dataclass
from ssl import SSLContext
from typing import Any, Literal, Mapping
from urllib.parse import urlencode, urlsplit

from .app import APP_ROUTE_TEMPLATES, OutcomeUnknownError, QianxingError
from .client_transport import _HttpJsonClient

_CONTROL_AUDIT_RECORD_FIELDS = frozenset(
    {
        "schema_version",
        "command_id",
        "request_id",
        "operator_id",
        "command_digest",
        "status",
        "result_code",
        "ts",
    }
)
_CONTROL_AUDIT_STATUSES = frozenset({"Accepted", "Executed", "Failed"})
_CONTROL_AUDIT_PAGE_FIELDS = frozenset(
    {
        "schema_version",
        "entries",
        "next_cursor",
        "has_more",
        "snapshot_through_sequence",
        "oldest_available_sequence",
        "retired_records",
    }
)
_CONTROL_AUDIT_MAX_PAGE_SIZE = 1_000
_CONTROL_AUDIT_MAX_PAGE_BYTES = 1_048_576
_CONTROL_AUDIT_AGGREGATE_LIMIT = 10_000


def _is_control_audit_record(value: Any, request_id: str | None = None) -> bool:
    if not isinstance(value, dict) or set(value) != _CONTROL_AUDIT_RECORD_FIELDS:
        return False
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        return False
    for field_name in ("command_id", "command_digest", "ts"):
        if type(value[field_name]) is not int or not 0 <= value[field_name] < 2**64:
            return False
    if any(
        not isinstance(value[field_name], str) or not value[field_name].strip()
        for field_name in ("request_id", "operator_id")
    ):
        return False
    if type(value["status"]) is not str or value["status"] not in _CONTROL_AUDIT_STATUSES:
        return False
    if not isinstance(value["result_code"], str):
        return False
    return request_id is None or value["request_id"] == request_id


def _cursor_parts(value: str) -> tuple[int, int] | None:
    parts = value.split(".")
    if len(parts) != 3 or parts[0] != "v1":
        return None
    if not parts[1].isascii() or not parts[1].isdecimal():
        return None
    if not parts[2].isascii() or not parts[2].isdecimal():
        return None
    before_sequence, through_sequence = int(parts[1]), int(parts[2])
    if not 0 < before_sequence <= through_sequence < 2**64:
        return None
    return before_sequence, through_sequence


def _is_control_audit_page(
    value: Any,
    *,
    limit: int,
    request_id: str | None = None,
) -> bool:
    if not isinstance(value, dict) or set(value) != _CONTROL_AUDIT_PAGE_FIELDS:
        return False
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        return False
    if type(value["has_more"]) is not bool:
        return False
    if type(value["snapshot_through_sequence"]) is not int or not 0 <= value["snapshot_through_sequence"] < 2**64:
        return False
    if type(value["retired_records"]) is not int or not 0 <= value["retired_records"] < 2**64:
        return False
    oldest = value["oldest_available_sequence"]
    if oldest is not None and (type(oldest) is not int or not 0 < oldest <= value["snapshot_through_sequence"]):
        return False
    entries = value["entries"]
    if not isinstance(entries, list) or len(entries) > limit:
        return False
    prior_sequence = 2**64
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"sequence", "record"}:
            return False
        sequence = entry["sequence"]
        if type(sequence) is not int or not 0 < sequence <= value["snapshot_through_sequence"]:
            return False
        if sequence >= prior_sequence:
            return False
        if not _is_control_audit_record(entry["record"], request_id):
            return False
        prior_sequence = sequence

    next_cursor = value["next_cursor"]
    if type(next_cursor) is not str and next_cursor is not None:
        return False
    if value["has_more"] != (next_cursor is not None) or (value["has_more"] and not entries):
        return False
    if next_cursor is not None:
        cursor = _cursor_parts(next_cursor)
        if cursor is None or cursor != (entries[-1]["sequence"], value["snapshot_through_sequence"]):
            return False
    if request_id is not None and value["has_more"]:
        return False
    try:
        encoded_size = len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))
    except (TypeError, ValueError, UnicodeEncodeError):
        return False
    return encoded_size <= _CONTROL_AUDIT_MAX_PAGE_BYTES


@dataclass(frozen=True)
class TradeCommand:
    """Existing Rust control-plane command contract; no Python execution logic is added."""

    command_id: int
    request_id: str
    operator_id: str
    reason: str
    kind: Literal[
        "SubmitOrder", "SubmitOrderGroup", "AmendOrder", "PauseStrategy", "ResumeStrategy", "ChangeRiskLimit",
        "CancelOrder", "CancelOrderGroup", "ReconcileAccount", "RetryJob", "SwitchVenue",
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
        if not isinstance(command_document, dict):
            raise ValueError("command must be a TradeCommand or object")
        request_id, command_id = command_document.get("request_id"), command_document.get("command_id")
        if (not isinstance(request_id, str) or not request_id.strip()
                or type(command_id) is not int or not 0 <= command_id < 2**64):
            raise ValueError("command needs a non-empty request_id and unsigned 64-bit command_id")
        # 工作流 → 路由键写成两个字面量分支。原写法 `APP_ROUTE_TEMPLATES[f"{self.workflow}_commands"]`
        # 把「工作流名」与「路由键名」用一条没写在任何地方的命名约定绑在一起：两个键在源码里
        # 没有字面量读者（门禁核不出它们到底有没有被用上），键改名时也只会拿到运行时 KeyError。
        route = (
            APP_ROUTE_TEMPLATES["paper_commands"]
            if self.workflow == "paper"
            else APP_ROUTE_TEMPLATES["live_commands"]
        )
        receipt = self._request_mutation(
            route,
            payload={"schema_version": 1, "workflow": self.workflow, "command": command_document},
            recovery_id=request_id,
        )
        if not _is_control_audit_record(receipt, request_id) or receipt["command_id"] != command_id:
            cause = QianxingError(
                "Rust API command receipt does not match control-audit-record-v1",
                payload={"category": "REMOTE_RESPONSE_INVALID"},
            )
            raise OutcomeUnknownError(route, request_id, cause) from cause
        return receipt

    async def async_submit(self, command: TradeCommand | dict[str, Any]) -> dict[str, Any]:
        """Submit through the Rust workflow without blocking the event loop.

        Cancelling this coroutine does not undo a request already sent to the server; callers
        must retain the command ``request_id`` and query the audit/run state after uncertainty.
        """
        return await asyncio.to_thread(self.submit, command)

    def get_command_audit(self, request_id: str | None = None) -> list[dict[str, Any]]:
        """Collect at most 10,000 rows; use ``get_command_audit_page`` for larger histories."""
        if request_id is not None and (not isinstance(request_id, str) or not request_id.strip()):
            raise ValueError("request_id must be non-empty text when provided")
        rows: list[dict[str, Any]] = []
        cursor: str | None = None
        seen_cursors: set[str] = set()
        while True:
            page = self.get_command_audit_page(
                cursor=cursor,
                limit=100,
                request_id=request_id,
            )
            rows.extend(entry["record"] for entry in page["entries"])
            if len(rows) > _CONTROL_AUDIT_AGGREGATE_LIMIT:
                raise QianxingError(
                    "control audit exceeds the convenience collection limit; use get_command_audit_page",
                    payload={"category": "CONTROL_AUDIT_PAGE_REQUIRED"},
                )
            if not page["has_more"]:
                return rows
            cursor = page["next_cursor"]
            if cursor in seen_cursors:
                raise QianxingError(
                    "Rust API repeated a control audit cursor",
                    payload={"category": "REMOTE_RESPONSE_INVALID"},
                )
            seen_cursors.add(cursor)

    def get_command_audit_page(
        self,
        *,
        cursor: str | None = None,
        limit: int = 100,
        request_id: str | None = None,
    ) -> dict[str, Any]:
        """Read one newest-first audit page with a durable cursor for the next older page."""
        if type(limit) is not int or not 1 <= limit <= _CONTROL_AUDIT_MAX_PAGE_SIZE:
            raise ValueError(f"limit must be an integer from 1 to {_CONTROL_AUDIT_MAX_PAGE_SIZE}")
        if request_id is not None and (not isinstance(request_id, str) or not request_id.strip()):
            raise ValueError("request_id must be non-empty text when provided")
        if cursor is not None and (not isinstance(cursor, str) or not cursor):
            raise ValueError("cursor must be non-empty text when provided")
        if cursor is not None and _cursor_parts(cursor) is None:
            raise ValueError("cursor is not a supported control audit cursor")
        if cursor is not None and request_id is not None:
            raise ValueError("cursor cannot be combined with request_id")
        if request_id is not None and limit < 2:
            raise ValueError("request_id lookup requires limit >= 2")
        route = APP_ROUTE_TEMPLATES["control_audit"]
        params = {"limit": limit}
        if request_id is not None:
            params["request_id"] = request_id
        if cursor is not None:
            params["cursor"] = cursor
        route += "?" + urlencode(params)
        page = self._request_json_value(route, method="GET")
        if not _is_control_audit_page(page, limit=limit, request_id=request_id):
            raise QianxingError(
                "Rust API returned a malformed control audit page",
                payload={"category": "REMOTE_RESPONSE_INVALID"},
            )
        return page

    async def async_get_command_audit(self, request_id: str | None = None) -> list[dict[str, Any]]:
        """Read and validate the command audit without blocking the event loop."""
        return await asyncio.to_thread(self.get_command_audit, request_id)

    async def async_get_command_audit_page(self, **kwargs: Any) -> dict[str, Any]:
        """Read one audit page without blocking the event loop."""
        return await asyncio.to_thread(self.get_command_audit_page, **kwargs)


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

