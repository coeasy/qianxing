"""Shared bounded JSON HTTP transport for Python SDK clients."""

from __future__ import annotations

import json
import math
from http.client import HTTPException
from ssl import SSLContext
from typing import Any, Mapping
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, HTTPSHandler, Request, build_opener

from .app import OutcomeUnknownError, QianxingError, _APP_ERRORS, _encode_payload

MAX_HTTP_RESPONSE_BYTES = 16 * 1024 * 1024


class _RejectRedirects(HTTPRedirectHandler):
    def redirect_request(self, request, file_pointer, code, message, headers, new_url):
        return None


def _urlopen(request, *, timeout, context):
    """Open one request without forwarding credentials or side effects to a redirect."""
    return build_opener(_RejectRedirects(), HTTPSHandler(context=context)).open(request, timeout=timeout)


def _read_bounded(response) -> bytes:
    headers = getattr(response, "headers", None)
    lengths = headers.get_all("Content-Length", []) if headers is not None else []
    transfer_encodings = headers.get_all("Transfer-Encoding", []) if headers is not None else []
    if len(lengths) > 1 or (lengths and transfer_encodings):
        raise QianxingError("Rust API response has conflicting framing headers",
                            payload={"category": "REMOTE_RESPONSE_INVALID"})
    expected = None
    if lengths:
        raw = lengths[0].strip()
        if not raw.isascii() or not raw.isdigit():
            raise QianxingError("Rust API response has invalid Content-Length",
                                payload={"category": "REMOTE_RESPONSE_INVALID"})
        expected = int(raw)
        if expected > MAX_HTTP_RESPONSE_BYTES:
            raise QianxingError("Rust API response exceeds the 16 MiB limit",
                                payload={"category": "REMOTE_RESPONSE_INVALID"})
    body = response.read(MAX_HTTP_RESPONSE_BYTES + 1)
    if len(body) > MAX_HTTP_RESPONSE_BYTES:
        raise QianxingError("Rust API response exceeds the 16 MiB limit",
                            payload={"category": "REMOTE_RESPONSE_INVALID"})
    if expected is not None and len(body) != expected:
        raise QianxingError("Rust API response body is incomplete",
                            payload={"category": "REMOTE_RESPONSE_INVALID"})
    return body


def _reject_non_finite_json(value: str) -> None:
    raise ValueError(f"non-finite JSON number: {value}")


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

    def _request_mutation(
        self, path: str, *, payload: Any, recovery_id: str | None = None,
    ) -> dict[str, Any]:
        """Send once; report ambiguous failures without attempting automatic retries."""
        try:
            return self._request_json(path, method="POST", payload=payload)
        except QianxingError as exc:
            status = exc.payload.get("http_status")
            if isinstance(status, int) and 400 <= status < 500 and status not in (408, 409):
                raise
            raise OutcomeUnknownError(path, recovery_id, exc) from exc

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
            with _urlopen(request, timeout=self.timeout, context=self.ssl_context) as response:
                body = _read_bounded(response)
                try:
                    text = body.decode("utf-8")
                    value = json.loads(text, parse_constant=_reject_non_finite_json) if text else {}
                except (UnicodeError, ValueError) as exc:
                    raise QianxingError("Rust API returned invalid UTF-8 or JSON",
                                        payload={"category": "REMOTE_RESPONSE_INVALID"}) from exc
                return value
        except HTTPError as exc:
            try:
                raw = _read_bounded(exc).decode("utf-8", errors="replace")
            except (OSError, HTTPException) as error:
                raise QianxingError(f"Rust API error response could not be read: {error}",
                                    payload={"category": "REMOTE_RESPONSE_INVALID"}) from error
            try:
                document = json.loads(raw, parse_constant=_reject_non_finite_json)
            except ValueError:
                document = {"message": raw, "status": exc.code}
            if isinstance(document, dict):
                document["http_status"] = exc.code
                category = document.get("category")
                if isinstance(category, str) and category in _APP_ERRORS:
                    raise _APP_ERRORS[category](document.get("message", raw), payload=document) from exc
                message = document.get("message", document.get("error", raw))
                raise QianxingError(str(message), payload=document) from exc
            raise QianxingError(raw, payload={"http_status": exc.code}) from exc
        except (URLError, TimeoutError, OSError, HTTPException) as exc:
            raise QianxingError(f"Rust API request failed: {exc}",
                                payload={"category": "TRANSPORT_ERROR"}) from exc

