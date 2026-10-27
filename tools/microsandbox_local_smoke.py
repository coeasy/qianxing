"""Run a no-network command inside a local Microsandbox microVM."""

from __future__ import annotations

import argparse
import asyncio
import sys
import uuid

_MARKER = "QIANXING_MICROSANDBOX_LOCAL_OK"


async def _run(image: str) -> None:
    try:
        from microsandbox import BackendKind, Network, Sandbox, backend_scope
    except ImportError as error:
        raise RuntimeError(
            "Install the optional SDK with `pip install 'qianxing[sandbox-microsandbox]'`."
        ) from error

    name = "qianxing-local-smoke-" + uuid.uuid4().hex[:16]
    with backend_scope(BackendKind.LOCAL):
        sandbox = await Sandbox.create(
            name,
            image=image,
            cpus=1,
            memory=512,
            max_duration=120,
            network=Network.none(),
            ephemeral=True,
        )
        try:
            result = await sandbox.exec(
                "python",
                ["-c", f"print('{_MARKER}')"],
                timeout=30,
            )
            if not result.success or _MARKER not in result.stdout_text:
                raise RuntimeError(
                    "The local microVM did not return the expected marker; "
                    f"success={result.success}, stdout={result.stdout_text!r}, "
                    f"stderr={result.stderr_text!r}"
                )
            print(_MARKER)
        finally:
            await sandbox.destroy(force=False, timeout=10.0)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Verify that Microsandbox can execute Python in a local, network-disabled microVM."
    )
    parser.add_argument(
        "--image",
        required=True,
        help="Python OCI image to run, for example python:3.12-slim",
    )
    args = parser.parse_args()
    image = args.image.strip()
    if not image:
        parser.error("--image must not be empty")
    try:
        asyncio.run(_run(image))
    except Exception as error:  # noqa: BLE001 - this is a user-facing acceptance command.
        print(f"Microsandbox local acceptance failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
