param(
    [string]$Python = "python",
    [string]$OutputDirectory = "dist"
)

# Comments in this file stay ASCII-only: Windows PowerShell 5.1 reads a BOM-less .ps1 as the
# ANSI codepage, and a UTF-8 Chinese byte can pair with the following line break, merging the next
# (code) line into the comment. cmd.exe has the same hazard for .bat, which is why build.bat keeps
# every line ending ASCII too (see tools/check_architecture.py windows_batch_parse_check).

# Invoke this file with an explicit policy: `powershell -NoProfile -ExecutionPolicy Bypass -File
# tools/build_python_wheel.ps1`. Windows clients default to a policy that refuses unsigned scripts,
# so a bare `-File` (or `./tools/build_python_wheel.ps1`) exits with UnauthorizedAccess without
# running a single line (V12 §18-C #132; README.md and wheel_builder_check keep that promise).

$ErrorActionPreference = "Stop"
$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot ".."))
$PythonProject = Join-Path $RepositoryRoot "python"

# The default `-Python python` is often the Microsoft Store stub, and the repo venv is built by uv
# without pip, while the wheel frontend needs `pip wheel`. Probe before the PEP 517 build.
& $Python -m pip --version *> $null
if ($LASTEXITCODE -ne 0) {
    throw "interpreter ${Python} has no pip, so pip wheel cannot run. Fix 1: uv pip install pip --python ${Python} (or ${Python} -m ensurepip --upgrade). Fix 2 (offline): uv build --wheel --offline --no-build-isolation --out-dir ${OutputDirectory} ${PythonProject}"
}
& $Python -m maturin --version *> $null
if ($LASTEXITCODE -ne 0) {
    throw "interpreter ${Python} has no maturin. Install build requirements with: uv pip install --python ${Python} maturin"
}

New-Item -ItemType Directory -Path (Join-Path $RepositoryRoot $OutputDirectory) -Force | Out-Null
& $Python -m pip wheel --no-deps --no-build-isolation $PythonProject --wheel-dir (Join-Path $RepositoryRoot $OutputDirectory)
if ($LASTEXITCODE -ne 0) {
    throw "maturin-backed Python wheel build failed with exit code $LASTEXITCODE"
}
