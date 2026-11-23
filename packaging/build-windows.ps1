# Builds Carrick's Windows installer into <target>\dist\: the release programs, the
# third-party licence file and the Inno Setup installer.
#   pwsh -File packaging\build-windows.ps1 [-SkipBuild]
# Needs Rust, Python 3 and Inno Setup 6 (winget install JRSoftware.InnoSetup).
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$version = (Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
$bin = Join-Path $target 'release'
# Built output stays with the build, out of the source tree.
$dist = Join-Path $target 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null

if (-not $SkipBuild) {
    Push-Location $repo
    try { cargo build --release -p carrick -p carrick-gui; if ($LASTEXITCODE) { throw 'cargo build failed' } }
    finally { Pop-Location }
}
foreach ($exe in 'carrick.exe', 'carrick-gui.exe') {
    if (-not (Test-Path (Join-Path $bin $exe))) { throw "$exe is not in $bin" }
}

python (Join-Path $PSScriptRoot 'third_party.py') (Join-Path $bin 'THIRD-PARTY-LICENSES.txt')
if ($LASTEXITCODE) { throw 'third_party.py failed' }

$iscc = @(
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe')
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 is not installed (winget install JRSoftware.InnoSetup)' }

& $iscc /Q "/DVersion=$version" "/DBinDir=$bin" "/DDistDir=$dist" (Join-Path $PSScriptRoot 'windows\carrick.iss')
if ($LASTEXITCODE) { throw 'Inno Setup failed' }
Get-Item (Join-Path $dist "carrick-$version-setup.exe")
