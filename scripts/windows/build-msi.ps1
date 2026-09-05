# Builds the Windows installer for the castr sender.
#
# Needs the WiX toolset, once per machine, as a per-user tool - no admin:
#   dotnet tool install --global wix
#   wix extension add --global WixToolset.UI.wixext
#   wix extension add --global WixToolset.Firewall.wixext
#
# Then:
#   powershell -File scripts\windows\build-msi.ps1
#
# The MSI lands in dist\. It is unsigned, so Windows will call the publisher
# unknown; signing needs a certificate, not a code change.
param(
    [string]$Version = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repo = Resolve-Path "$PSScriptRoot\..\.."
$dist = Join-Path $repo 'dist'
$wix = Join-Path $env:USERPROFILE '.dotnet\tools\wix.exe'
if (-not (Test-Path $wix)) {
    $wix = (Get-Command wix -ErrorAction SilentlyContinue).Source
}
if (-not $wix) { throw "wix not found. Run: dotnet tool install --global wix" }

# The version the workspace declares, so the MSI and the exe never disagree.
if (-not $Version) {
    $line = Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
            Select-Object -First 1
    if (-not $line) { throw "could not read the workspace version from Cargo.toml" }
    $Version = $line.Matches[0].Groups[1].Value
}
"version : $Version"

if (-not $SkipBuild) {
    Push-Location $repo
    try {
        cargo build --release -p castr-sender
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally { Pop-Location }
}

$exe = Join-Path $repo 'target\release\castr-sender.exe'
if (-not (Test-Path $exe)) { throw "missing $exe" }
if (-not (Test-Path $dist)) { New-Item -ItemType Directory -Path $dist | Out-Null }

$out = Join-Path $dist "castr-$Version-x64.msi"
$args = @(
    'build',
    (Join-Path $repo 'packaging\windows\castr.wxs'),
    '-arch', 'x64',
    '-d', "Version=$Version",
    '-d', "SourceDir=$(Join-Path $repo 'target\release')",
    '-d', "IconPath=$(Join-Path $repo 'assets\castr.ico')",
    '-d', "LicensePath=$(Join-Path $repo 'packaging\windows\license.rtf')",
    '-ext', 'WixToolset.UI.wixext',
    '-ext', 'WixToolset.Firewall.wixext',
    '-o', $out
)
& $wix @args
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }

"built   : $out"
"size    : {0:N1} MB" -f ((Get-Item $out).Length / 1MB)
