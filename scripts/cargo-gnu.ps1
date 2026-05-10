param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $CargoArgs
)

$ErrorActionPreference = "Stop"

$toolchainRoot = Join-Path $env:USERPROFILE ".rustup\toolchains\stable-x86_64-pc-windows-gnu"
$selfContainedBin = Join-Path $toolchainRoot "lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained"
$cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"

if (-not (Test-Path $cargo)) {
    throw "cargo.exe was not found at $cargo"
}

if (-not (Test-Path (Join-Path $selfContainedBin "dlltool.exe"))) {
    throw "dlltool.exe was not found at $selfContainedBin"
}

$env:PATH = "$selfContainedBin;$env:PATH"
& $cargo @CargoArgs
exit $LASTEXITCODE
