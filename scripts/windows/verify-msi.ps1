# Installs the MSI, checks everything it claims to do, uninstalls, and checks
# it left nothing behind.
#
# Two packages, two round trips:
#
#   powershell -File scripts\windows\verify-msi.ps1
#       The per-machine package. Needs an elevated PowerShell, because it
#       installs per-machine.
#
#   powershell -File scripts\windows\verify-msi.ps1 -PerUser
#       The per-user package. Run it from an ORDINARY PowerShell: running it
#       unelevated is most of the point, since the claim being tested is that
#       this package prompts for nothing.
#
# Nothing here is interactive: it is the silent round trip, not the wizard.
# The wizard's appearance still needs a person to look at it.
param([string]$Msi = "", [switch]$PerUser)

$ErrorActionPreference = 'Stop'
$repo = Resolve-Path "$PSScriptRoot\..\.."
if (-not $Msi) {
    # `castr-*-x64.msi` deliberately does not match `castr-*-x64-peruser.msi`,
    # so each run picks up its own package rather than whichever was built last.
    $pattern = if ($PerUser) { 'castr-*-x64-peruser.msi' } else { 'castr-*-x64.msi' }
    $Msi = (Get-ChildItem (Join-Path $repo 'dist') -Filter $pattern |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
}
if (-not $Msi -or -not (Test-Path $Msi)) { throw "no MSI found; run build-msi.ps1 first" }

$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$elevated = (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if ($PerUser) {
    if ($elevated) {
        "NOTE  this shell is elevated, so the package installs into the administrator's"
        "      profile and the 'prompts for nothing' claim is not being exercised."
        "      Re-run from an ordinary PowerShell to test that properly."
    }
} elseif (-not $elevated) {
    throw "this needs an elevated PowerShell: the package installs per-machine"
}

$results = [ordered]@{}
function Check($name, [bool]$ok) {
    $results[$name] = $ok
    "{0}  {1}" -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $name
}

# Everywhere the two packages differ is here, and nowhere else: the round trip
# below is the same round trip for both.
if ($PerUser) {
    $exe = Join-Path $env:LOCALAPPDATA 'Programs\castr\castr-sender.exe'
    $startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\castr\castr.lnk'
    $desktop = Join-Path ([Environment]::GetFolderPath('DesktopDirectory')) 'castr.lnk'
    $pathScope = 'User'
    $arpRoot = 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall'
} else {
    $exe = Join-Path $env:ProgramFiles 'castr\castr-sender.exe'
    $startMenu = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\castr\castr.lnk'
    $desktop = Join-Path ([Environment]::GetFolderPath('CommonDesktopDirectory')) 'castr.lnk'
    $pathScope = 'Machine'
    $arpRoot = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall'
}

function SystemPath { [Environment]::GetEnvironmentVariable('PATH', $pathScope) }
# MSI resolves [INSTALLFOLDER] with a trailing backslash, so the PATH entry
# reads `C:\Program Files\castr\`. Harmless to Windows, but a comparison
# against a path without one silently fails - which it did, and was briefly
# mistaken for the PATH entry not working at all.
function PathHas([string]$dir) {
    $want = $dir.TrimEnd('\')
    (SystemPath) -split ';' | Where-Object { $_.TrimEnd('\') -eq $want } | Measure-Object |
        ForEach-Object { $_.Count -gt 0 }
}
# What the package declares about its icon, read from the package itself.
#
# Deliberately not a probe of the live registry. Two attempts at that asserted
# the wrong thing - DisplayIcon under the Uninstall key, then ProductIcon
# through the installer API - and both failed while the install log plainly
# showed `IconCreate(Icon=castr.ico)` and `ProductIcon=castr.ico`. Whether
# Windows then *draws* it in Apps and Features is a question about Windows,
# and like the wizard's appearance it wants an eye, not an assertion.
function DeclaresIcon([string]$path) {
    $inst = New-Object -ComObject WindowsInstaller.Installer
    $db = $inst.OpenDatabase($path, 0)
    $hasProperty = $false
    $v = $db.OpenView("SELECT Value FROM Property WHERE Property='ARPPRODUCTICON'")
    $v.Execute(); if ($v.Fetch()) { $hasProperty = $true }; $v.Close()
    $hasIcon = $false
    $v = $db.OpenView("SELECT Name FROM Icon")
    $v.Execute(); if ($v.Fetch()) { $hasIcon = $true }; $v.Close()
    $hasProperty -and $hasIcon
}
# Read from the package for the same reason as the icon: this is what decides
# whether Windows asks for elevation, and it is a property of the package, not
# something to be inferred from whether a prompt happened to appear. A
# per-machine package sets ALLUSERS=1; a per-user one leaves it unset.
function DeclaredAllUsers([string]$path) {
    $inst = New-Object -ComObject WindowsInstaller.Installer
    $db = $inst.OpenDatabase($path, 0)
    $value = ''
    $v = $db.OpenView("SELECT Value FROM Property WHERE Property='ALLUSERS'")
    $v.Execute()
    $rec = $v.Fetch()
    if ($rec) { $value = $rec.StringData(1) }
    $v.Close()
    $value
}
function ArpEntry {
    Get-ChildItem $arpRoot -EA SilentlyContinue |
        ForEach-Object { Get-ItemProperty $_.PSPath -EA SilentlyContinue } |
        Where-Object { $_.DisplayName -eq 'castr' } | Select-Object -First 1
}
function FirewallRule {
    Get-NetFirewallRule -DisplayName 'castr sender' -EA SilentlyContinue | Select-Object -First 1
}

"=== before ==="
Check 'not already installed' (-not (ArpEntry))
if ($PerUser) {
    # The one thing that makes this package elevation-free. Checked before
    # anything is installed, so a package built wrong fails here rather than
    # after touching the machine.
    Check 'the package does not declare ALLUSERS' ((DeclaredAllUsers $Msi) -ne '1')
} else {
    Check 'the package declares ALLUSERS=1' ((DeclaredAllUsers $Msi) -eq '1')
}

"`n=== installing ==="
$log = Join-Path $env:TEMP 'castr-install.log'
$p = Start-Process msiexec -ArgumentList "/i","`"$Msi`"","/qn","/l*v","`"$log`"" -Wait -PassThru
"msiexec exit: $($p.ExitCode)"
Check 'install succeeded' ($p.ExitCode -eq 0)

Check 'the exe is installed' (Test-Path $exe)
Check 'Start Menu shortcut exists' (Test-Path $startMenu)
Check 'Desktop shortcut exists' (Test-Path $desktop)
Check "PATH ($pathScope) contains the install directory" (PathHas (Split-Path $exe))
$arp = ArpEntry
Check 'Apps and Features lists it' ($null -ne $arp)
if ($arp) {
    Check 'it shows a version' ($arp.DisplayVersion -eq (
        Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value)
    Check 'it shows a publisher' ($arp.Publisher -eq 'castr')
    Check 'the package declares an icon for it' (DeclaresIcon $Msi)
    Check 'it offers an uninstall command' ([bool]$arp.UninstallString)
}
if ($PerUser) {
    # Not an omission: a firewall rule lives in the machine-wide policy store,
    # so this package cannot add one and must not appear to have tried.
    # `castr-sender firewall --allow` is the way to get it.
    Check 'no firewall rule was added' ($null -eq (FirewallRule))
    Check 'nothing was written to Program Files' (
        -not (Test-Path (Join-Path $env:ProgramFiles 'castr')))
    Check 'the machine PATH was left alone' (
        ([Environment]::GetEnvironmentVariable('PATH', 'Machine') -split ';' |
            Where-Object { $_ -like '*\castr*' } | Measure-Object).Count -eq 0)
} else {
    Check 'the firewall rule is present' ($null -ne (FirewallRule))
}
Check 'the exe runs from the installed path' (
    (& $exe --help 2>&1 | Out-String) -match 'miracast-cast')
Check 'the exe offers the firewall command' (
    (& $exe --help 2>&1 | Out-String) -match 'firewall')

"`n=== uninstalling ==="
$p = Start-Process msiexec -ArgumentList "/x","`"$Msi`"","/qn" -Wait -PassThru
"msiexec exit: $($p.ExitCode)"
Check 'uninstall succeeded' ($p.ExitCode -eq 0)

# The two things installers most often strand.
Check 'the exe is gone' (-not (Test-Path $exe))
Check 'the Start Menu shortcut is gone' (-not (Test-Path $startMenu))
Check 'the Desktop shortcut is gone' (-not (Test-Path $desktop))
Check 'PATH is back as it was' (-not (PathHas (Split-Path $exe)))
if (-not $PerUser) {
    Check 'the firewall rule is gone' ($null -eq (FirewallRule))
}
Check 'Apps and Features no longer lists it' ($null -eq (ArpEntry))

# Not checked here, because a script cannot: that the wizard looks right, and
# that Apps and Features actually draws the icon. Both want a person.

# Deliberately kept: pairings and identity live here, and discarding them would
# mean pairing every receiver and display again after a reinstall.
$state = Join-Path $env:APPDATA 'castr'
if (Test-Path $state) { "NOTE  %APPDATA%\castr kept, as intended (pairings and identity)" }

$failed = ($results.GetEnumerator() | Where-Object { -not $_.Value }).Count
"`n=== $($results.Count - $failed)/$($results.Count) passed ==="
if ($failed -gt 0) { "install log: $log"; exit 1 }
exit 0
