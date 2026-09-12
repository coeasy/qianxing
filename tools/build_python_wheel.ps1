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
        Where-Object { $_.Name -match '^_qianxing_native\.(pyd|so|dll|dylib)$' } |
        Select-Object -First 1
if ($null -eq $native) {
    throw "native extension artifact was not found in $TargetDirectory"
}

Get-ChildItem -LiteralPath $PackageDirectory -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match '^_qianxing_native\.(pyd|so|dll|dylib)$' } |
    ForEach-Object { [System.IO.File]::Delete($_.FullName) }
$destination = Join-Path $PackageDirectory $native.Name
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
