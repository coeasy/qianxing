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
$PythonCommand = if (Test-Path -LiteralPath $Python -PathType Leaf) { (Resolve-Path -LiteralPath $Python).Path } else { $Python }
$OutputPath = if ([System.IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory
} else {
    Join-Path $RepositoryRoot $OutputDirectory
}

# Maturin and its CycloneDX SBOM use this timestamp. Derive it from the source commit so
# repeated builds of the same checkout produce byte-identical wheel metadata and SBOM.
if ([string]::IsNullOrWhiteSpace($env:SOURCE_DATE_EPOCH)) {
    $SourceDateEpoch = & git -C $RepositoryRoot show -s --format=%ct HEAD
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($SourceDateEpoch)) {
        throw "could not derive SOURCE_DATE_EPOCH from the repository commit"
    }
    $env:SOURCE_DATE_EPOCH = $SourceDateEpoch.Trim()
}

# The default `-Python python` is often the Microsoft Store stub. Probe pip and maturin before
# compiling either the CLI renderer or the native extension.
& $PythonCommand -m pip --version *> $null
if ($LASTEXITCODE -ne 0) {
    throw "interpreter ${PythonCommand} has no pip. Install Python with pip enabled, then retry the wheel build."
}

& $PythonCommand -m maturin --version *> $null
if ($LASTEXITCODE -ne 0) {
    throw "interpreter ${PythonCommand} has no maturin 1.15.0. Install build requirements with: uv pip install --python ${PythonCommand} maturin==1.15.0"
}

# The wheel carries the platform report renderer so a clean venv can run `project report`
# without a second install (docs plan D3 option A).
& cargo build --release --locked -p qx-cli
if ($LASTEXITCODE -ne 0) {
    throw "cargo build -p qx-cli failed with exit code $LASTEXITCODE, so the wheel would ship without a report renderer"
}
$RendererDirectory = Join-Path $PythonProject "qianxing/_bin"
New-Item -ItemType Directory -Path $RendererDirectory -Force | Out-Null
Copy-Item -Path (Join-Path $RepositoryRoot "target/release/qx-cli.exe") -Destination (Join-Path $RendererDirectory "qx-cli.exe") -Force

New-Item -ItemType Directory -Path $OutputPath -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $RepositoryRoot "target/maturin") -Force | Out-Null
Push-Location $PythonProject
try {
    & $PythonCommand -m maturin build --release --locked --out $OutputPath
} finally {
    Pop-Location
}
if ($LASTEXITCODE -ne 0) {
    throw "maturin Python wheel build failed with exit code $LASTEXITCODE"
}
