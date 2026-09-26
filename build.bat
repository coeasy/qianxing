@echo off
chcp 65001 >nul
REM Qianxing - Windows build script. Double-click it, or run build.bat from the repo root.
REM Step [0/8] picks a Python interpreter that actually works; steps [1/8]..[8/8] are the release gates.
REM Bare `python` on Windows can be a Microsoft Store stub, a broken venv launcher, or any exe that
REM exits 0, so the probe demands the interpreter print a real version number before trusting it.
REM tzdata is a declared Windows dependency in python/pyproject.toml, so the probe checks it too.

echo ===== Qianxing =====

set "QX_PY=%QX_PYTHON%"
if not defined QX_PY if exist "python\.venv\Scripts\python.exe" set "QX_PY=python\.venv\Scripts\python.exe"
if not defined QX_PY set "QX_PY=python"
set "QX_PROBE=%TEMP%\qianxing_pyprobe.txt"
"%QX_PY%" -c "import sys;print(sys.version.split()[0])" > "%QX_PROBE%" 2>nul
if errorlevel 1 goto :err_py
findstr /r "^[0-9][.]" "%QX_PROBE%" >nul 2>nul
if errorlevel 1 goto :err_py
set /p QX_PYVER=<"%QX_PROBE%"
del "%QX_PROBE%" >nul 2>nul
"%QX_PY%" -c "import tzdata" >nul 2>nul
if errorlevel 1 goto :err_tz
echo [0/8] Python 解释器 = %QX_PY% (版本 %QX_PYVER%)

REM The steps below spawn Python from inside Rust, and the Rust side reads exactly one variable:
REM QX_PYTHON (crates/qx-cli/src/main.rs python_interpreter()). QX_PY alone is a cmd variable and
REM never reaches cargo, so [3/8] used to fall back to the PATH stub and build.bat could not pass
REM its own step 3 on a box without a global python (V12 §19 #133). Only the repo-venv candidate needs
REM the hand-off: when QX_PYTHON was already set it stays as it is, and a bare PATH name must keep
REM its own "stub" diagnosis instead of being rewritten into a path that does not exist.
if /i "%QX_PY%"=="python\.venv\Scripts\python.exe" for %%I in ("%QX_PY%") do set "QX_PYTHON=%%~fI"
if defined QX_PYTHON echo        QX_PYTHON 已交给 Rust 侧 = %QX_PYTHON%

echo.
echo [1/8] 格式检查 ...
cargo fmt --all -- --check
if errorlevel 1 goto :err

echo.
echo [2/8] 构建 Release ...
cargo build --release
if errorlevel 1 goto :err

echo.
echo [3/8] 运行 Rust 测试 ...
cargo test --workspace
if errorlevel 1 goto :err

echo.
echo [4/8] 运行 Clippy ...
cargo clippy --workspace --all-targets -- -D warnings
if errorlevel 1 goto :err

echo.
echo [5/8] 运行 Python/JSON 边界测试 ...
"%QX_PY%" -m unittest discover -s python/tests -v
if errorlevel 1 goto :err

echo.
echo [6/8] 运行核心语义校验 ...
"%QX_PY%" tools/validate_core.py
if errorlevel 1 goto :err

echo.
echo [7/8] 运行 CLI 全链路与生态冒烟 ...
cargo run -p qx-cli --release -- all
if errorlevel 1 goto :err
cargo run -p qx-cli --release -- ecosystem
if errorlevel 1 goto :err

echo.
echo [8/8] 校验运行时拓扑配置 ...
cargo run -p qx-cli --release -- runtime-check deploy/qianxing.runtime.example.json
if errorlevel 1 goto :err

echo.
echo ===== 全部完成 (all gates passed) =====
goto :eof

:err
echo.
echo [失败] 请检查上方错误信息 (build.bat step failed)
exit /b 1

:err_py
del "%QX_PROBE%" >nul 2>nul
echo [失败] 找不到可用的 Python 3 解释器 (no working interpreter)
echo        试过的候选: QX_PYTHON / python\.venv\Scripts\python.exe / PATH 上的 python
echo        当前选中的 "%QX_PY%" 没能打印出版本号, 所以它不是能用的 python (version probe failed)
echo        修法 1: cd python 后执行 uv venv .venv --python 3.12 --clear, 再 uv pip install tzdata
echo        修法 2: set QX_PYTHON=完整路径\python.exe, 再重跑 build.bat
exit /b 1

:err_tz
echo [失败] 解释器 "%QX_PY%" 能跑, 但缺 tzdata (Windows 必需, 见 python/pyproject.toml)
echo        缺它时 [5/8] 的 A 股用例会以 ZoneInfoNotFoundError 失败 (tzdata missing)
echo        修法: 执行 "%QX_PY%" -m pip install tzdata
exit /b 1
