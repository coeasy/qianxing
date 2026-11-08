"""``python -m qianxing_ashare``：``qianxing data`` 的兼容别名（方案 D11）。

两个入口共用 :mod:`qianxing_ashare.cli` 的分派与取数实现，不复制逻辑。
"""

from __future__ import annotations

from .cli import main

if __name__ == "__main__":
    raise SystemExit(main(prog="python -m qianxing_ashare"))
