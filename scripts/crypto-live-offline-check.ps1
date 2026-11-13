param(
    [string]$ProbeConfig
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$pythonRoot = Join-Path $repoRoot 'python'
$resolvedProbe = $null
if (-not [string]::IsNullOrWhiteSpace($ProbeConfig)) {
    $probePath = if ([IO.Path]::IsPathRooted($ProbeConfig)) {
        $ProbeConfig
    }
    else {
        Join-Path $repoRoot $ProbeConfig
    }
    $resolvedProbe = (Resolve-Path $probePath).Path
}

function Invoke-Check {
    param(
        [string]$Name,
        [scriptblock]$Action
    )

    Write-Host "`n== $Name =="
    & $Action
    if ($LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE"
    }
}

$previousDevDebug = $env:CARGO_PROFILE_DEV_DEBUG
$previousTestDebug = $env:CARGO_PROFILE_TEST_DEBUG
$previousTemp = $env:TEMP
$previousTmp = $env:TMP
try {
    # Keeps Windows/MSVC test builds within the local compiler's debug-record limits.
    $env:CARGO_PROFILE_DEV_DEBUG = '0'
    $env:CARGO_PROFILE_TEST_DEBUG = '0'
    $testTemp = Join-Path $repoRoot 'target\qx-test-temp'
    New-Item -ItemType Directory -Force -Path $testTemp | Out-Null
    $env:TEMP = $testTemp
    $env:TMP = $testTemp

    Push-Location $repoRoot
    try {
        Invoke-Check 'Rust formatting' { cargo fmt --all -- --check }
        Invoke-Check 'Crypto live Rust contracts' {
            cargo test -p qx-core -p qx-risk -p qx-adapter -p qx-execution -p qx-service -p qx-protocol -p qx-zhenlu -p qx-xingban -p qx-runtime -p qx-storage -p qx-api --lib
        }
        Invoke-Check 'Exact order and ledger integration contracts' {
            cargo test -p qx-core --test ledger -j 1
            if ($LASTEXITCODE -ne 0) { throw "Exact ledger integration contracts failed with exit code $LASTEXITCODE" }
            cargo test -p qx-app --test strategy_exact_intent -j 1
        }
        Invoke-Check 'Architecture fast checks' { python tools/check_architecture.py --fast }
    }
    finally {
        Pop-Location
    }

    Push-Location $pythonRoot
    try {
        Invoke-Check 'CCXT offline request/config contracts' {
            python -m unittest discover -s tests -p 'test_ccxt*.py' -q
        }
        Invoke-Check 'Python Strategy API contracts' {
            python -m unittest discover -s tests -p 'test_strategy_contract.py' -q
        }
        Invoke-Check 'Python packaging contracts' {
            python -m unittest discover -s tests -p 'test_packaging_contract.py' -q
        }

        if (-not [string]::IsNullOrWhiteSpace($ProbeConfig)) {
            Invoke-Check 'Read-only sandbox private probe' {
                python -m qianxing_ccxt.private_probe --config $resolvedProbe
            }
        }
    }
    finally {
        Pop-Location
    }

    Write-Host "`nAll selected local checks passed. No order was submitted and no account mode was changed."
}
finally {
    if ($null -eq $previousDevDebug) {
        Remove-Item Env:CARGO_PROFILE_DEV_DEBUG -ErrorAction SilentlyContinue
    }
    else {
        $env:CARGO_PROFILE_DEV_DEBUG = $previousDevDebug
    }
    if ($null -eq $previousTestDebug) {
        Remove-Item Env:CARGO_PROFILE_TEST_DEBUG -ErrorAction SilentlyContinue
    }
    else {
        $env:CARGO_PROFILE_TEST_DEBUG = $previousTestDebug
    }
    if ($null -eq $previousTemp) {
        Remove-Item Env:TEMP -ErrorAction SilentlyContinue
    }
    else {
        $env:TEMP = $previousTemp
    }
    if ($null -eq $previousTmp) {
        Remove-Item Env:TMP -ErrorAction SilentlyContinue
    }
    else {
        $env:TMP = $previousTmp
    }
}
