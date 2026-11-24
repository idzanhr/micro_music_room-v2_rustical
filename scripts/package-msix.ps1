# Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
#
# Builds an MSIX from target\release\MicroMusicRoom.exe.
#   -Mode Store : the real identity of v1 on the Store, UNSIGNED (Microsoft signs it on ingestion).
#                 Upload this one to Partner Center.
#   -Mode Test  : a different identity ("...MicroMusicRoomTest"), signed with a throw-away
#                 certificate, so it installs NEXT TO the Store version without touching it.
#                 Used to try the packaged behaviour on a real PC before submitting.
param(
    [ValidateSet("Store", "Test")][string]$Mode = "Store",
    [string]$OutputDirectory = "msix-out"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\release\MicroMusicRoom.exe"
if (-not (Test-Path -LiteralPath $exe)) { throw "Build first: cargo build --release ($exe not found)" }

$cargo = Get-Content -Raw -LiteralPath (Join-Path $root "Cargo.toml")
if ($cargo -notmatch '(?m)^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"') { throw "Cargo.toml needs a plain x.y.z version." }
$version = "$($Matches[1]).$($Matches[2]).$($Matches[3]).0"

$layout = Join-Path $env:RUNNER_TEMP ("mmr-msix-" + [Guid]::NewGuid().ToString("N"))
if (-not $env:RUNNER_TEMP) { $layout = Join-Path ([IO.Path]::GetTempPath()) ("mmr-msix-" + [Guid]::NewGuid().ToString("N")) }
New-Item -ItemType Directory -Path $layout | Out-Null
Copy-Item -LiteralPath $exe -Destination $layout

# --- manifest --------------------------------------------------------------------------------
$manifestPath = Join-Path $layout "AppxManifest.xml"
Copy-Item -LiteralPath (Join-Path $root "msix\AppxManifest.xml") -Destination $manifestPath
[xml]$manifest = Get-Content -Raw -LiteralPath $manifestPath
$ns = New-Object System.Xml.XmlNamespaceManager($manifest.NameTable)
$ns.AddNamespace("m", "http://schemas.microsoft.com/appx/manifest/foundation/windows10")
$ns.AddNamespace("uap", "http://schemas.microsoft.com/appx/manifest/uap/windows10")
$identity = $manifest.SelectSingleNode("/m:Package/m:Identity", $ns)
$identity.SetAttribute("Version", $version)
if ($Mode -eq "Test") {
    $identity.SetAttribute("Name", "IdzanMarko.MicroMusicRoomTest")
    $identity.SetAttribute("Publisher", "CN=MicroMusicRoomTest")
    $manifest.SelectSingleNode("/m:Package/m:Properties/m:DisplayName", $ns).InnerText = "Micro Music Room (Test)"
    $manifest.SelectSingleNode("//uap:VisualElements", $ns).SetAttribute("DisplayName", "Micro Music Room (Test)")
}
$manifest.Save($manifestPath)

# --- logos, from msix\icon-256.png -------------------------------------------------------------
$assets = Join-Path $layout "Assets"
New-Item -ItemType Directory -Path $assets | Out-Null
Add-Type -AssemblyName System.Drawing
$source = [System.Drawing.Image]::FromFile((Join-Path $root "msix\icon-256.png"))
function New-Logo([string]$name, [int]$size) {
    $bitmap = [System.Drawing.Bitmap]::new($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
        $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $g.DrawImage($source, 0, 0, $size, $size)
        $bitmap.Save((Join-Path $assets $name), [System.Drawing.Imaging.ImageFormat]::Png)
    } finally { $g.Dispose(); $bitmap.Dispose() }
}
try {
    foreach ($s in 100, 200, 400) {
        New-Logo "StoreLogo.scale-$s.png" ([int](50 * $s / 100))
        New-Logo "Square44x44Logo.scale-$s.png" ([int](44 * $s / 100))
        New-Logo "Square150x150Logo.scale-$s.png" ([int](150 * $s / 100))
    }
    foreach ($t in 16, 24, 32, 48, 256) {
        New-Logo "Square44x44Logo.targetsize-$t.png" $t
        New-Logo "Square44x44Logo.targetsize-${t}_altform-unplated.png" $t
    }
} finally { $source.Dispose() }

# --- pack ------------------------------------------------------------------------------------
$sdk = Join-Path ([Environment]::GetFolderPath("ProgramFilesX86")) "Windows Kits\10\bin"
function Find-SdkTool([string]$name) {
    $tool = Get-ChildItem -Path $sdk -Filter $name -Recurse -ErrorAction Stop |
        Where-Object { $_.FullName -match "\\x64\\" } | Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $tool) { throw "$name was not found in the Windows SDK." }
    $tool.FullName
}
New-Item -ItemType Directory -Path (Join-Path $root $OutputDirectory) -Force | Out-Null
$out = Join-Path $root $OutputDirectory
$package = Join-Path $out "MicroMusicRoom-v$version-x64-$Mode.msix"
& (Find-SdkTool "makeappx.exe") pack /d $layout /p $package /o /v
if ($LASTEXITCODE -ne 0) { throw "MakeAppx failed." }

if ($Mode -eq "Test") {
    $cert = New-SelfSignedCertificate -Type Custom -Subject "CN=MicroMusicRoomTest" -KeyUsage DigitalSignature `
        -FriendlyName "Micro Music Room test signing" -CertStoreLocation "Cert:\CurrentUser\My" `
        -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
    $password = ConvertTo-SecureString -String ([Guid]::NewGuid().ToString()) -Force -AsPlainText
    $pfx = Join-Path $layout "test.pfx"
    Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $password | Out-Null
    Export-Certificate -Cert $cert -FilePath (Join-Path $out "MicroMusicRoomTest.cer") | Out-Null
    $plain = [Runtime.InteropServices.Marshal]::PtrToStringAuto([Runtime.InteropServices.Marshal]::SecureStringToBSTR($password))
    & (Find-SdkTool "signtool.exe") sign /fd SHA256 /f $pfx /p $plain $package
    if ($LASTEXITCODE -ne 0) { throw "SignTool failed." }
    Remove-Item -LiteralPath $cert.PSPath -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath (Join-Path $root "scripts\Install-Test.ps1") -Destination $out
}
Write-Host "Created $package (version $version, $Mode)."
