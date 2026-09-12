@echo off
REM 牵星 Qianxing — Windows 构建脚本
REM 用法：双击运行，或在项目根目录执行 build.bat

echo ===== 牵星 Qianxing 构建 =====
echo.

echo [1/7] 格式检查...
cargo fmt --all -- --check
if errorlevel 1 goto :err

echo.
echo [2/7] 构建 Release...
cargo build --release
if errorlevel 1 goto :err

echo.
echo [3/7] 运行 Rust 测试...
cargo test --workspace
if errorlevel 1 goto :err

echo.
echo [4/7] 运行 Clippy...
cargo clippy --workspace --all-targets -- -D warnings
if errorlevel 1 goto :err

echo.
echo [5/7] 运行 Python/JSON 边界测试...
python -m unittest discover -s python/tests -v
if errorlevel 1 goto :err

echo.
echo [6/7] 运行核心语义校验...
python tools/validate_core.py
if errorlevel 1 goto :err

echo.
echo [7/7] 运行 CLI 全链路与生态冒烟...
cargo run -p qx-cli --release -- all
if errorlevel 1 goto :err
cargo run -p qx-cli --release -- ecosystem
if errorlevel 1 goto :err

echo.
echo [8/8] 校验运行时拓扑配置...
cargo run -p qx-cli --release -- runtime-check deploy/qianxing.runtime.example.json
if errorlevel 1 goto :err

echo.
echo ===== 全部完成 =====
goto :eof

:err
echo.
echo [失败] 请检查上方错误信息
exit /b 1
