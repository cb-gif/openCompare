$ErrorActionPreference = 'Stop'
$cargoArguments = if ($args.Count -gt 0) { @($args) } else { @('run', '-p', 'compare-gui') }
$projectRoot = Split-Path -Parent $PSScriptRoot
$localCargo = Join-Path $projectRoot '.tools\cargo\bin\cargo.exe'
if (Test-Path -LiteralPath $localCargo) {
    $env:CARGO_HOME = Join-Path $projectRoot '.tools\cargo'
    $env:RUSTUP_HOME = Join-Path $projectRoot '.tools\rustup'
    $env:PATH = (Split-Path -Parent $localCargo) + ';' + $env:PATH
    $cargoExe = $localCargo
} else {
    $cargoExe = 'cargo'
}
Push-Location $projectRoot
try {
    & $cargoExe @cargoArguments
    exit $LASTEXITCODE
} finally {
    Pop-Location
}

