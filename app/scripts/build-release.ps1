#!/usr/bin/env pwsh
# Phase 13.A pre-public hardening: a wrapper for `npm run tauri build` that
# guarantees the developer-path scrub is applied to the app.exe inside the
# MSI / NSIS. `build-linux-cli.ps1` sets RUSTFLAGS for the CLI bundles, but
# its env-var lifetime ends when the script returns — `npm run tauri build`
# spawns a fresh cargo that doesn't inherit it. This script sets RUSTFLAGS
# in the parent process, then forwards every CLI arg to `npm run tauri build`.
#
#   -Bundles "nsis"   restrict the bundler (default: whatever tauri.conf says,
#                     i.e. "all" = MSI + NSIS). Tag builds must keep both.
#   -Timings          emit cargo's HTML timing report to
#                     src-tauri/target/cargo-timings/cargo-timing.html
param(
    [string]$Bundles = "",
    [switch]$Timings
)
$ErrorActionPreference = "Stop"

$cargoHome   = if ($env:CARGO_HOME)   { $env:CARGO_HOME }   else { Join-Path $env:USERPROFILE ".cargo" }
$rustupHome  = if ($env:RUSTUP_HOME)  { $env:RUSTUP_HOME }  else { Join-Path $env:USERPROFILE ".rustup" }
$cargoFwd    = $cargoHome    -replace "\\", "/"
$rustupFwd   = $rustupHome   -replace "\\", "/"
$homeBack    = $env:USERPROFILE
$homeFwd     = $homeBack     -replace "\\", "/"

$flags = @(
    "--remap-path-prefix=$cargoHome=cargo"
    "--remap-path-prefix=$cargoFwd=cargo"
    "--remap-path-prefix=$rustupHome=rustup"
    "--remap-path-prefix=$rustupFwd=rustup"
    "--remap-path-prefix=$homeBack=user"
    "--remap-path-prefix=$homeFwd=user"
) -join " "

$env:RUSTFLAGS = $flags
Write-Host "RUSTFLAGS = $flags"

# There used to be a "force the app.exe to rebuild so the new remap is
# applied" block here. It was a no-op twice over and has been removed:
#   1. RUSTFLAGS is part of Cargo's fingerprint, so Cargo *cannot* reuse a
#      compilation made without the remap flags. That is the real guarantee.
#   2. The code didn't do what it claimed anyway — bin artifacts in
#      target/release/deps are unhashed (app.exe, app.d, app.pdb), so the
#      `-Filter app-*` glob matched nothing, and target/release/app.exe is a
#      hardlink Cargo re-creates for fresh units without recompiling.
# The scrub is now *asserted* in build-windows.yml instead of assumed.

$tauriArgs = @()
if ($Bundles) { $tauriArgs += @("--bundles", $Bundles) }
$tauriArgs += $args
# Cargo args go after a second `--`: npm eats one, the Tauri CLI eats the next.
if ($Timings) { $tauriArgs += @("--", "--timings") }

Write-Host "Forwarding to: npm run tauri build -- $($tauriArgs -join ' ')"
$sw = [Diagnostics.Stopwatch]::StartNew()
& npm run tauri build -- @tauriArgs
if ($LASTEXITCODE -ne 0) { throw "npm run tauri build failed (exit $LASTEXITCODE)" }
Write-Host ("[timing] tauri build (frontend + cargo app + bundlers): {0:n1}s" -f $sw.Elapsed.TotalSeconds)
