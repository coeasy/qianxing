"""Shared bounded JSON HTTP transport for Python SDK clients."""

from __future__ import annotations

import json
import math
from ssl import SSLContext
from typing import Any, Mapping
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import Request, urlopen

from .app import QianxingError, _APP_ERRORS, _encode_payload


class _HttpJsonClient:
    """Shared HTTP transport for the Rust API facades; contains no domain logic."""

    def __init__(
        self,
        base_url: str,
        *,
        headers: Mapping[str, str] | None = None,
        ssl_context: SSLContext | None = None,
        timeout: float = 30.0,
    ):
        parsed = urlsplit(base_url)
        if parsed.scheme not in ("http", "https") or not parsed.netloc or parsed.query or parsed.fragment:
            raise ValueError("base_url must be an http(s) origin/path without query or fragment")
        if not math.isfinite(timeout) or timeout <= 0:
            raise ValueError("timeout must be finite and positive")
        request_headers = dict(headers or {})
        for name, value in request_headers.items():
            if (
                not isinstance(name, str)
                or not name.strip()
                or not isinstance(value, str)
                or "\r" in name
                or "\n" in name
                or "\r" in value
                or "\n" in value
            ):
                raise ValueError("headers must contain valid string names and values")
            if name.casefold() == "content-type":
                raise ValueError("Content-Type is managed by the Qianxing API client")
        self.base_url = base_url.rstrip("/")
        self.headers = request_headers
        self.ssl_context = ssl_context
        self.timeout = timeout

    def _request_json(
        self,
        path: str,
        *,
        method: str,
        payload: Any = None,
    ) -> dict[str, Any]:
        value = self._request_json_value(path, method=method, payload=payload)
        if not isinstance(value, dict):
            raise QianxingError("Rust API returned a non-object JSON document")
        return value

    def _request_json_value(
        self,
        path: str,
        *,
        method: str,
        payload: Any = None,
    ) -> Any:
        data = None if payload is None else _encode_payload(payload).encode("utf-8")
        request = Request(
            f"{self.base_url}{path}",
            data=data,
            headers=(
                {**self.headers, "Content-Type": "application/json"}
                if data is not None
                else self.headers
            ),
            method=method,
        )
        try:
            with urlopen(request, timeout=self.timeout, context=self.ssl_context) as response:
                body = response.read().decode("utf-8")
                value = json.loads(body) if body else {}
                return value
        except HTTPError as exc:
            raw = exc.read().decode("utf-8", errors="replace")
            try:
                document = json.loads(raw)
            except ValueError:
                document = {"message": raw, "status": exc.code}
            if isinstance(document, dict):
                category = document.get("category")
                if category in _APP_ERRORS:
                    raise _APP_ERRORS[category](document.get("message", raw), payload=document) from exc
                message = document.get("message", document.get("error", raw))
                raise QianxingError(str(message), payload=document) from exc
            raise QianxingError(raw, payload={"status": exc.code}) from exc
        except (URLError, TimeoutError, OSError) as exc:
            raise QianxingError(f"Rust API request failed: {exc}") from exc

