#!/usr/bin/env python3
"""Regenerate the checked-in machine-readable HTTP route inventory."""

from pathlib import Path

import check_architecture


def main() -> None:
    output = check_architecture.ROOT / check_architecture.HTTP_ROUTE_MANIFEST_FILE
    output.write_text(check_architecture.render_http_route_manifest(), encoding="utf-8")
    print(f"已生成 {output.relative_to(check_architecture.ROOT).as_posix()}")


if __name__ == "__main__":
    main()
