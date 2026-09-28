#!/usr/bin/env bash
# 牵星 Qianxing — POSIX 构建脚本
# 步骤编号与 build.bat 逐项对齐：[0/9] 选解释器，[1/9]..[9/9] 是九道发布门禁。
set -euo pipefail

# 解释器按 QX_PYTHON -> 仓库 venv -> PATH 上的 python 依次取第一个能真的跑起来的。
# 与 build.bat 同一口径：不信任 PATH，探测方式是让候选自己报出版本号。
QX_PY="${QX_PYTHON:-}"
if [ -z "$QX_PY" ] && [ -x "python/.venv/bin/python" ]; then
  QX_PY="python/.venv/bin/python"
fi
if [ -z "$QX_PY" ]; then
  QX_PY="python"
fi
if ! QX_PY_VER="$("$QX_PY" -c 'import sys;print(sys.version.split()[0])' 2>/dev/null)"; then
  echo "[失败] 找不到可用的 Python 3 解释器 (no working interpreter)" >&2
  echo "       试过的候选: QX_PYTHON / python/.venv/bin/python / PATH 上的 python" >&2
  echo "       当前选中的 '$QX_PY' 连版本号都没能打印出来" >&2
  echo "       修法: cd python && uv venv .venv --python 3.12 && uv pip install tzdata" >&2
  echo "       或: export QX_PYTHON=/完整路径/python3.12" >&2
  exit 1
fi
if ! "$QX_PY" -c "import tzdata" >/dev/null 2>&1; then
  echo "[失败] 解释器 '$QX_PY' 能跑, 但缺 tzdata (见 python/pyproject.toml)" >&2
  echo "       缺它时 [6/9] 的 A 股用例会以 ZoneInfoNotFoundError 失败" >&2
  echo "       修法: \"$QX_PY\" -m pip install tzdata" >&2
  exit 1
fi
echo "[0/9] Python 解释器 = $QX_PY (版本 $QX_PY_VER)"

# 与 build.bat 同一收口（V12 §19 #133）：[4/9] 与 [8/9] 由 Rust 侧自己起 Python，而 Rust 只读
# QX_PYTHON 这一个变量（crates/qx-cli/src/main.rs python_interpreter()）。上面选中的 QX_PY 若
# 不外传，cargo 就永远看不见它，两条 Python 桥契约用例会回落到 PATH 上的桩并失败。
# 只有"仓库 venv"这一档需要外传：用户已经设过 QX_PYTHON 时原样保留，PATH 上的裸 `python`
# 留给它自己的占位桩诊断，不改写成一个不存在的路径。
if [ "$QX_PY" = "python/.venv/bin/python" ]; then
  QX_PYTHON="$PWD/python/.venv/bin/python"
  export QX_PYTHON
fi
if [ -n "${QX_PYTHON:-}" ]; then
  echo "       QX_PYTHON 已交给 Rust 侧 = $QX_PYTHON"
fi

echo "===== 牵星 Qianxing 构建 ====="
echo
# 架构门禁排在最前面：它只读源码，约 40 秒就能判掉，不必等 release 构建和整树测试跑完才红。
echo "[1/9] 运行架构不变量自检..."
"$QX_PY" tools/check_architecture.py
echo
echo "[2/9] 格式检查..."
cargo fmt --all -- --check
echo
echo "[3/9] 构建 Release..."
cargo build --release
echo
echo "[4/9] 运行 Rust 测试..."
cargo test --workspace
echo
echo "[5/9] 运行 Clippy..."
cargo clippy --workspace --all-targets -- -D warnings
echo
echo "[6/9] 运行 Python/JSON 边界测试..."
"$QX_PY" -m unittest discover -s python/tests -v
echo
echo "[7/9] 运行核心语义校验..."
"$QX_PY" tools/validate_core.py
echo
echo "[8/9] 运行 CLI 全链路与生态冒烟..."
cargo run -p qx-cli --release -- all
cargo run -p qx-cli --release -- ecosystem
echo
echo "[9/9] 校验运行时拓扑配置..."
cargo run -p qx-cli --release -- runtime-check deploy/qianxing.runtime.example.json
echo
echo "===== 全部完成 ====="
