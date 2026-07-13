$ErrorActionPreference = "Stop"

$projectRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $projectRoot "src-tauri\Cargo.toml"
$cliTarget = Join-Path $projectRoot "src-tauri\target-cli"
$source = Join-Path $cliTarget "release\lazyswitch-cli.exe"
$bundleDir = Join-Path $projectRoot "src-tauri\cli-bundle"

Write-Host "Building the native CLI before bundling: $manifest"
& cargo build --manifest-path $manifest --target-dir $cliTarget --release --bin lazyswitch-cli
if ($LASTEXITCODE -ne 0) {
    throw "cargo build --bin lazyswitch-cli failed with exit code $LASTEXITCODE"
}
Copy-Item -LiteralPath $source -Destination $bundleDir -Force
Write-Host "Prepared CLI resource: $bundleDir"
