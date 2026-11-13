#requires -Version 5.1
<#
Release build of Velocity with its engine: target\release\velocity.exe plus the DLLs the engine
needs beside it (ANGLE, GStreamer). Needs the velocity-engine folder next to this one, libclang
(`pip install libclang`) and GStreamer (winget install gstreamerproject.gstreamer).
Usage: powershell -File scripts\build.ps1 [-Target <cargo target folder>]
#>
param([string]$Target)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$engine = Join-Path (Split-Path $root -Parent) 'velocity-engine\scripts'
if (-not (Test-Path $engine)) { throw "velocity-engine not found next to Velocity ($engine)" }
if (-not $Target) { $Target = Join-Path $root 'target' }
. (Join-Path $engine 'engine-env.ps1')
cargo build --release --manifest-path "$root\Cargo.toml" --target-dir $Target
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
& (Join-Path $engine 'engine-runtime.ps1') -Release (Join-Path $Target 'release')
