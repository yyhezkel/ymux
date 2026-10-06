#!/usr/bin/env pwsh
# Stages CLI binaries into src-tauri/resources/ for the Tauri bundler.
#  - ymux-linux-x64 (cross-compiled, static-musl) — uploaded to remote SSH servers
#    by `remote_bootstrap`
#  - ymux-cli.exe (Windows release build) — bundled in the MSI alongside the app
#    so installing ymux gets you both the GUI and the CLI in one shot
# Also (re)writes remote-manifest.json (UTF-8 without BOM).
#
# Staging is CONTENT-GUARDED (sha256, not mtime). Everything under resources/
# is pulled into the app crate with include_bytes! / include_str!
# (remote_bootstrap.rs, addons.rs, lib.rs:7659), and Cargo's staleness check
# is mtime-based — so a `Copy-Item -Force` of a byte-identical file used to
# force a full recompile of the 9.4k-line lib plus a relink of a 55 MB exe on
# every single build. Identical content => leave the file, and its mtime,
# alone. Different content => copy, always.
$ErrorActionPreference = "Stop"

function Get-Sha256([string]$Path) {
    if (-not (Test-Path $Path)) { return "" }
    return (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower()
}

# Copies only when the content actually differs. Returns the source sha256.
function Copy-IfChanged([string]$Source, [string]$Destination, [string]$Label) {
    $srcHash = Get-Sha256 $Source
    if ($srcHash -eq (Get-Sha256 $Destination)) {
        Write-Host "$Label unchanged (sha256=$srcHash) - left in place"
    } else {
        Copy-Item -Path $Source -Destination $Destination -Force
        Write-Host "$Label restaged (sha256=$srcHash)"
    }
    return $srcHash
}

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$tauriDir = Join-Path $root "src-tauri"
$resourcesDir = Join-Path $tauriDir "resources"
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (Test-Path $cargoBin) { $env:Path = "$cargoBin;$env:Path" }

# Pre-public hardening: scrub absolute developer paths (incl. Windows
# username) from compiled-in strings. `file!()` macros and panic
# locations in dependencies bake the build machine's $CARGO_HOME +
# $RUSTUP_HOME into .rodata; `strip = "symbols"` cannot remove them.
# We force every release build through this script to apply consistent
# --remap-path-prefix flags so the resulting binary is byte-identical
# regardless of who built it.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE ".rustup" }
$cargoHomeFwd = $cargoHome -replace "\\", "/"
$rustupHomeFwd = $rustupHome -replace "\\", "/"
$userHomeFwd = $env:USERPROFILE -replace "\\", "/"
$env:RUSTFLAGS = "--remap-path-prefix=$cargoHome=cargo --remap-path-prefix=$cargoHomeFwd=cargo --remap-path-prefix=$rustupHome=rustup --remap-path-prefix=$rustupHomeFwd=rustup --remap-path-prefix=$env:USERPROFILE=user --remap-path-prefix=$userHomeFwd=user"
Write-Host "RUSTFLAGS scrub: CARGO_HOME=$cargoHome RUSTUP_HOME=$rustupHome HOME=$env:USERPROFILE"

# Ensure target installed.
$targets = & rustup target list --installed
if (-not ($targets -contains "x86_64-unknown-linux-musl")) {
    Write-Host "Installing x86_64-unknown-linux-musl target..."
    & rustup target add x86_64-unknown-linux-musl
}

if ($env:GITHUB_ACTIONS) { Write-Host "::group::cargo build ymux (musl cross)" }
$sw = [Diagnostics.Stopwatch]::StartNew()
Push-Location $tauriDir
try {
    & cargo build --release --target x86_64-unknown-linux-musl -p ymux
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}
if ($env:GITHUB_ACTIONS) { Write-Host "::endgroup::" }
Write-Host ("[timing] cargo ymux (musl cross): {0:n1}s" -f $sw.Elapsed.TotalSeconds)

$src = Join-Path $tauriDir "target\x86_64-unknown-linux-musl\release\ymux"
$dst = Join-Path $resourcesDir "ymux-linux-x64"
New-Item -ItemType Directory -Path $resourcesDir -Force | Out-Null
$hash = Copy-IfChanged $src $dst "ymux-linux-x64"

$size = (Get-Item $dst).Length

# ymux-tmux.conf — bundled scrollback-friendly tmux config. Read its
# bytes + SHA so the bootstrap can detect drift and refresh without
# touching the binary. The file is hand-written (not built), so it
# lives in resources/ directly and we just hash it.
$tmuxConfPath = Join-Path $resourcesDir "ymux-tmux.conf"
$tmuxConfHash = Get-Sha256 $tmuxConfPath
$tmuxConfSize = (Get-Item $tmuxConfPath).Length

# remote-manifest.json is include_str!'d by the app crate (lib.rs:7659,
# remote_bootstrap.rs:27), so rewriting it with a fresh `built_at` on every
# run used to force a full app-crate rebuild — and produced the git churn
# ci-windows.yml has a whole step apologising for. Only rewrite when a
# hash or size actually moved; otherwise keep the existing file verbatim.
$manifestPath = Join-Path $resourcesDir "remote-manifest.json"
$manifestCurrent = $null
if (Test-Path $manifestPath) {
    try { $manifestCurrent = Get-Content $manifestPath -Raw | ConvertFrom-Json } catch { $manifestCurrent = $null }
}
$manifestFresh = $null -ne $manifestCurrent `
    -and $manifestCurrent.'x86_64-linux'.sha256 -eq $hash `
    -and $manifestCurrent.'x86_64-linux'.size -eq $size `
    -and $manifestCurrent.'tmux-conf'.sha256 -eq $tmuxConfHash `
    -and $manifestCurrent.'tmux-conf'.size -eq $tmuxConfSize

if ($manifestFresh) {
    Write-Host "remote-manifest.json unchanged - left in place"
} else {
    $iso = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $manifest = @{
        "x86_64-linux" = @{
            path     = "ymux-linux-x64"
            sha256   = $hash
            size     = $size
            built_at = $iso
        }
        "tmux-conf" = @{
            path     = "ymux-tmux.conf"
            sha256   = $tmuxConfHash
            size     = $tmuxConfSize
            built_at = $iso
        }
    } | ConvertTo-Json -Depth 10
    # Write UTF-8 WITHOUT BOM (Windows PowerShell 5.1 `Set-Content -Encoding utf8` adds BOM,
    # which serde_json refuses with "expected value at line 1 column 1").
    [System.IO.File]::WriteAllText($manifestPath, $manifest, [System.Text.UTF8Encoding]::new($false))
    Write-Host "remote-manifest.json rewritten"
}

Write-Host "ymux-linux-x64: $size bytes, sha256=$hash"
Write-Host "ymux-tmux.conf: $tmuxConfSize bytes, sha256=$tmuxConfHash"

# Also build the Windows release of the CLI and stage it for the MSI bundler.
if ($env:GITHUB_ACTIONS) { Write-Host "::group::cargo build ymux (windows host)" }
$sw = [Diagnostics.Stopwatch]::StartNew()
Push-Location $tauriDir
try {
    & cargo build --release -p ymux
    if ($LASTEXITCODE -ne 0) { throw "cargo build ymux (Windows release) failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}
if ($env:GITHUB_ACTIONS) { Write-Host "::endgroup::" }
Write-Host ("[timing] cargo ymux (windows host): {0:n1}s" -f $sw.Elapsed.TotalSeconds)

$srcWin = Join-Path $tauriDir "target\release\ymux.exe"
$dstWin = Join-Path $resourcesDir "ymux-cli.exe"
$null = Copy-IfChanged $srcWin $dstWin "ymux-cli.exe"
Write-Host "Staged ymux-cli.exe: $((Get-Item $dstWin).Length) bytes"

# Stage the LICENSE next to src-tauri so Tauri's MSI bundler picks it up via the
# relative `licenseFile` setting. We don't commit this copy — the repo's canonical
# LICENSE is at the project root.
$projectLicense = Join-Path $root "..\LICENSE"
$tauriLicense = Join-Path $tauriDir "LICENSE"
$null = Copy-IfChanged $projectLicense $tauriLicense "LICENSE"
