param(
    [string] $InstallDir = (Join-Path $env:LOCALAPPDATA "Programs\dscan11")
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
$cargoScript = Join-Path $repoRoot "scripts\cargo-gnu.ps1"
$releaseDir = Join-Path $repoRoot "target\x86_64-pc-windows-gnu\release"
$releaseExe = Join-Path $releaseDir "dscan11.exe"
$installExe = Join-Path $InstallDir "dscan11.exe"

& powershell -ExecutionPolicy Bypass -File $cargoScript build --release
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

if (-not (Test-Path $releaseExe)) {
    throw "release binary was not found at $releaseExe"
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item -LiteralPath $releaseExe -Destination $installExe -Force

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$installNorm = $InstallDir.TrimEnd("\")
$releaseNorm = $releaseDir.TrimEnd("\")
$parts = @()

foreach ($part in ($userPath -split ";")) {
    $trimmed = $part.Trim()
    if ($trimmed -eq "") {
        continue
    }
    $norm = $trimmed.TrimEnd("\")
    if ($norm -ieq $releaseNorm) {
        continue
    }
    if ($norm -ieq $installNorm) {
        continue
    }
    $parts += $trimmed
}

$parts = @($InstallDir) + $parts
[Environment]::SetEnvironmentVariable("Path", ($parts -join ";"), "User")

Write-Host "Installed $installExe"
Write-Host "Updated User PATH so $InstallDir is the dscan11 location."
Write-Host "Open a new PowerShell window, then run: where.exe dscan11"
Write-Host "Try it with: dscan11 --version"
Write-Host "Then use commands like: dscan11 status"
