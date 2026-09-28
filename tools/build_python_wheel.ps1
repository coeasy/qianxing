param(
    [string]$Python = "python",
    [ValidateSet("debug", "release")]
    [string]$Profile = "release",
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
$TargetDirectory = Join-Path $RepositoryRoot (Join-Path "target" $Profile)
$PackageDirectory = Join-Path $PythonProject "qianxing_bridge"

# The default `-Python python` is often the Microsoft Store stub, and the repo venv is built by uv
# without pip, while the last step needs `pip wheel`. Probe before cargo so a broken interpreter
# fails in milliseconds instead of after a full release build.
& $Python -m pip --version *> $null
if ($LASTEXITCODE -ne 0) {
    throw "interpreter ${Python} has no pip, so pip wheel cannot run. Fix 1: uv pip install pip --python ${Python} (or ${Python} -m ensurepip --upgrade). Fix 2 (offline): uv build --wheel --offline --no-build-isolation --out-dir ${OutputDirectory} ${PythonProject}"
}

$cargoArgs = @("build", "-p", "qx-python")
if ($Profile -eq "release") {
    $cargoArgs += "--release"
}
Push-Location $RepositoryRoot
try {
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build qx-python failed with exit code $LASTEXITCODE"
    }

    $native = Get-ChildItem $TargetDirectory -File |
    Where-Object { $_.Name -match '^(lib)?_qianxing_native\.(pyd|so|dll|dylib)$' } |
    Select-Object -First 1
    if ($null -eq $native) {
        throw "native extension artifact was not found in $TargetDirectory"
    }

    # Cargo names the cdylib `_qianxing_native.dll`; Python only imports `_qianxing_native.pyd`
    # (Windows) or `_qianxing_native.so` (Unix), so stage the import name next to the build output
    # and inside the package directory before packaging.
    $importName = if ($env:OS -eq "Windows_NT") { "_qianxing_native.pyd" } else { "_qianxing_native.so" }
    Copy-Item -LiteralPath $native.FullName -Destination (Join-Path $TargetDirectory $importName) -Force

    Get-ChildItem -LiteralPath $PackageDirectory -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^_qianxing_native\.(pyd|so|dll|dylib)$' } |
        ForEach-Object { [System.IO.File]::Delete($_.FullName) }
    $destination = Join-Path $PackageDirectory $importName
    Copy-Item -LiteralPath $native.FullName -Destination $destination -Force

    New-Item -ItemType Directory -Path (Join-Path $RepositoryRoot $OutputDirectory) -Force | Out-Null
    & $Python -m pip wheel --no-deps $PythonProject --wheel-dir (Join-Path $RepositoryRoot $OutputDirectory)
    if ($LASTEXITCODE -ne 0) {
        throw "python wheel build failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}
