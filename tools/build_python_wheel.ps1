param(
    [string]$Python = "python",
    [ValidateSet("debug", "release")]
    [string]$Profile = "release",
    [string]$OutputDirectory = "dist"
)

$ErrorActionPreference = "Stop"
$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot ".."))
$PythonProject = Join-Path $RepositoryRoot "python"
$TargetDirectory = Join-Path $RepositoryRoot (Join-Path "target" $Profile)
$PackageDirectory = Join-Path $PythonProject "qianxing_bridge"

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

# Cargo 的 cdylib 产物名与 Python 导入名不同：Python 只接受
# `_qianxing_native.pyd`（Windows）或 `_qianxing_native.so`（Unix）。
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
