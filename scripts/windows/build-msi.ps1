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
#
# -PerUser builds the other package: %LOCALAPPDATA%\Programs\castr, the user's
# own PATH and Start Menu, and no elevation prompt at any point. It carries no
# firewall rule, because a per-user one does not exist - `castr-sender firewall
# --allow` adds that later, once, from an administrator terminal.
param(
    [string]$Version = "",
    [switch]$SkipBuild,
    [switch]$PerUser
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

# Two packages, two names, one dist directory: the per-user one is a different
# package with its own UpgradeCode, not a variant of the other, and someone
# looking in dist\ should be able to tell them apart without opening them.
$suffix = if ($PerUser) { '-x64-peruser' } else { '-x64' }
$out = Join-Path $dist "castr-$Version$suffix.msi"
"scope   : $(if ($PerUser) { 'per-user (no elevation)' } else { 'per-machine' })"

$args = @(
    'build',
    (Join-Path $repo 'packaging\windows\castr.wxs'),
    '-arch', 'x64',
    '-d', "Version=$Version",
    '-d', "SourceDir=$(Join-Path $repo 'target\release')",
    '-d', "IconPath=$(Join-Path $repo 'assets\castr.ico')",
    '-d', "LicensePath=$(Join-Path $repo 'packaging\windows\license.rtf')",
    '-ext', 'WixToolset.UI.wixext',
    # Loaded for both builds. Only the per-machine package uses a fw: element,
    # but castr.wxs declares the namespace either way - a processing
    # instruction cannot live inside the <Wix> start tag, so the declaration
    # cannot be made conditional.
    '-ext', 'WixToolset.Firewall.wixext',
    '-o', $out
)
if ($PerUser) { $args += @('-d', 'PerUser=1') }
& $wix @args
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }

"built   : $out"
"size    : {0:N1} MB" -f ((Get-Item $out).Length / 1MB)
