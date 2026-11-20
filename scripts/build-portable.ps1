# Builds the portable multipath.exe (static C runtime, nothing to install) into exe\,
# which git ignores:
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-portable.ps1
#
# Own target folder so it does not disturb a build in progress. (From DecHAMDRM.)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$env:RUSTFLAGS = '-C target-feature=+crt-static'
$env:CARGO_TARGET_DIR = Join-Path $root 'target\portable'
cargo build --release --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'build failed' }
New-Item -ItemType Directory -Force (Join-Path $root 'exe') | Out-Null
$name = 'multipath.exe'
$built = Join-Path $env:CARGO_TARGET_DIR "x86_64-pc-windows-msvc\release\$name"
# A static build must not ask for the C runtime DLLs.
$text = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($built))
if ($text -match '(?i)vcruntime140|api-ms-win-crt') { throw "$name imports a C runtime DLL: not a static build" }
Copy-Item $built (Join-Path $root "exe\$name") -Force
$item = Get-Item (Join-Path $root "exe\$name")
"{0}  {1:N1} MB  {2}" -f $item.FullName, ($item.Length / 1MB), (Get-FileHash $item.FullName -Algorithm SHA256).Hash
