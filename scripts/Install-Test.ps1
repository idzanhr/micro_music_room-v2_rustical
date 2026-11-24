# Installs the TEST build of Micro Music Room (identity "...MicroMusicRoomTest") next to the
# Store version. Right-click this file, "Run with PowerShell", and answer Yes to the admin prompt.
# It trusts the throw-away test certificate that signed the package (needs admin) and installs it.
# To remove everything afterwards: uninstall "Micro Music Room (Test)" in Settings > Apps, and
# delete "Micro Music Room test signing" from certmgr.msc > Trusted People.

if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Start-Process powershell -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    exit
}
$here = Split-Path -Parent $PSCommandPath
Import-Certificate -FilePath (Join-Path $here "MicroMusicRoomTest.cer") -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
$package = Get-ChildItem -Path $here -Filter "*-Test.msix" | Select-Object -First 1
Add-AppxPackage -Path $package.FullName
Write-Host "Installed. Find 'Micro Music Room (Test)' in the Start Menu."
Read-Host "Press Enter to close"
