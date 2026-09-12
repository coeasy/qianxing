#!/usr/bin/env bash
# 牵星 Qianxing — POSIX 构建脚本
set -euo pipefail

echo "===== 牵星 Qianxing 构建 ====="
echo
echo "[1/7] 格式检查..."
cargo fmt --all -- --check
echo
echo "[2/7] 构建 Release..."
cargo build --release
echo
echo "[3/7] 运行 Rust 测试..."
cargo test --workspace
echo
echo "[4/7] 运行 Clippy..."
cargo clippy --workspace --all-targets -- -D warnings
echo
echo "[5/7] 运行 Python/JSON 边界测试..."
python -m unittest discover -s python/tests -v
echo
echo "[6/7] 运行核心语义校验..."
python tools/validate_core.py
echo
echo "[7/7] 运行 CLI 全链路与生态冒烟..."
cargo run -p qx-cli --release -- all
cargo run -p qx-cli --release -- ecosystem
cargo run -p qx-cli --release -- runtime-check deploy/qianxing.runtime.example.json
echo
echo "===== 全部完成 ====="
