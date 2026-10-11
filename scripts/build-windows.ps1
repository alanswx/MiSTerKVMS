# Build the Windows app: Rust core + CLI, then the WinUI 3 shell.
# Usage: scripts\build-windows.ps1 [-Arch x64|arm64] [-Configuration Debug|Release] [-Run] [-Package]
#
#   -Package   publish a self-contained folder and zip it to build\MiSTerKVMS-<version>-win-<arch>.zip
#   -Version   version for the app and the zip (default: Cargo.toml's)
param(
    [ValidateSet("x64", "arm64")] [string]$Arch = "x64",
    [ValidateSet("Debug", "Release")] [string]$Configuration = "Debug",
    [string]$Version,
    [switch]$Run,
    [switch]$Package
)
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

if (-not $Version) {
    $Version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value
}
$Triple = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$Platform = if ($Arch -eq "arm64") { "ARM64" } else { "x64" }
$Project = "windows\MiSTerKVMS\MiSTerKVMS.csproj"

Write-Host "==> Rust core $Version ($Triple)"
cargo build --release --target $Triple -p kvms-core -p kvms-cli
if ($LASTEXITCODE) { exit $LASTEXITCODE }

# A running copy keeps the core DLL locked.
Get-Process MiSTerKVMS -ErrorAction SilentlyContinue | ForEach-Object {
    throw "MiSTerKVMS is running (pid $($_.Id)); close it first."
}

if ($Package) {
    Write-Host "==> Publish ($Platform)"
    $Out = "build\MiSTerKVMS-$Version-win-$Arch"
    if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
    dotnet publish $Project -c Release -p:Platform=$Platform -r "win-$Arch" --self-contained true `
        -p:Version=$Version -o $Out
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
    Copy-Item LICENSE "$Out\LICENSE.txt"
    $Zip = "$Out.zip"
    if (Test-Path $Zip) { Remove-Item $Zip }
    Compress-Archive -Path "$Out\*" -DestinationPath $Zip
    Write-Host "Built $Zip"
    $Exe = "$Out\MiSTerKVMS.exe"
} else {
    Write-Host "==> App ($Configuration, $Platform)"
    dotnet build $Project -c $Configuration -p:Platform=$Platform -p:Version=$Version
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
    $Exe = Get-ChildItem "windows\MiSTerKVMS\bin\$Platform\$Configuration" -Recurse -Filter MiSTerKVMS.exe |
        Select-Object -First 1 -ExpandProperty FullName
    Write-Host "Built $Exe"
}

if ($Run) { Start-Process $Exe }
