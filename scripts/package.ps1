# Builds the downloads: dist\Velocity-Setup-<version>.exe (installer) and dist\Velocity-<version>-windows-x64.zip
# (portable: unzip anywhere and run velocity.exe). Both carry velocity.exe, uBlock Origin Lite and a README. Usage: powershell -File scripts\package.ps1
param(
    # The unpacked uBlock Origin Lite to bundle (by default the one this PC's Velocity uses).
    [string]$Ubo = "$env:LOCALAPPDATA\velocity\extensions\uBOLite"
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if (-not (Test-Path "$Ubo\manifest.json")) { throw "uBlock Origin Lite not found at $Ubo (pass -Ubo <folder>)" }

# Its own target folder, so a Velocity that is running from target\release doesn't block the build.
cargo build --release --manifest-path "$root\Cargo.toml" --target-dir "$root\target\dist"
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$dist = "$root\dist"
$stage = "$dist\Velocity"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory "$stage\extensions" | Out-Null
Copy-Item "$root\target\dist\release\velocity.exe" $stage
Copy-Item -Recurse $Ubo "$stage\extensions\uBOLite"
$uboVersion = (Get-Content "$Ubo\manifest.json" -Raw | ConvertFrom-Json).version

@"
Velocity $version
==============

A fast, light browser you can set up the way you like.

Start
-----
Run velocity.exe. Nothing is installed: you can keep this folder anywhere (Desktop, a USB stick).
To uninstall, delete this folder and %LOCALAPPDATA%\velocity (your settings, history and profiles).

Windows may say "Windows protected your PC" the first time, because this build isn't signed yet.
Click "More info", then "Run anyway".

Needs
-----
Windows 10 or 11 (64-bit) with the Microsoft Edge WebView2 Runtime. Windows 11 already has it.
On Windows 10, get it from https://developer.microsoft.com/microsoft-edge/webview2/ if Velocity doesn't open.

Included
--------
uBlock Origin Lite $uboVersion (extensions\uBOLite), an ad and tracker blocker by Raymond Hill,
licensed under the GNU GPLv3 (see extensions\uBOLite\LICENSE.txt). Source: https://github.com/uBlockOrigin/uBOL-home
On first start Velocity copies it into %LOCALAPPDATA%\velocity\extensions.
"@ | Set-Content -Encoding utf8 "$stage\README.txt"

$zip = "$dist\Velocity-$version-windows-x64.zip"
Compress-Archive -Path $stage -DestinationPath $zip
"Built $zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"

# Setup.exe with Inno Setup (winget install JRSoftware.InnoSetup). The icon is the one build.rs made from the logo.
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { "Inno Setup not found: skipped Velocity-Setup.exe"; return }
$icon = Get-ChildItem "$root\target\dist\release\build\velocity-*\out\velocity.ico" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
& $iscc /Q "/DAppVersion=$version" "/DStage=$stage" "/DIcon=$($icon.FullName)" "/O$dist" "$root\installer\velocity.iss"
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed" }
$setup = "$dist\Velocity-Setup-$version.exe"
"Built $setup ($([math]::Round((Get-Item $setup).Length / 1MB, 1)) MB)"
