#!/usr/bin/env pwsh
<#
.SYNOPSIS
  Put the machine back to "never ran ymux's local setup" so the Smart
  Install wizard can be tested from a clean state.

.DESCRIPTION
  The wizard is idempotent by design: once a step has done its work it
  reports "already installed" and skips. Correct in production, useless
  for testing, because you cannot watch a step actually run twice. This
  undoes what the setup steps create.

  Two levels:

    Soft (default) - remove ymux's own artifacts only.
      Windows : the ymux hooks block in ~/.claude/settings.json,
                ~/.claude/hooks.json
      WSL     : ~/.ymux (bin, tmux.conf, run), and the ymux hooks
                block in the distro's ~/.claude/settings.json
      Leaves the distro, the linux user, tmux and /etc/wsl.conf alone, so
      re-running the wizard exercises the deploy steps but not the
      install/user steps.

    Full - Soft, plus `wsl --unregister <distro>`.
      DELETES THE ENTIRE LINUX FILESYSTEM: every file, the user,
      installed packages. It is the only way to retest InstallWsl and
      CreateWslUser, and it is not undoable.

  Nothing runs without -Apply. Without it you get a dry run that prints
  exactly what would be touched.

  NEVER touched: %APPDATA%\ymux\workspaces.json and settings.json -
  your workspaces, panes and preferences. This resets the setup wizard's
  footprint, it does not wipe ymux.

  ASCII only, deliberately: Windows PowerShell 5.1 reads a .ps1 as ANSI
  when the file has no BOM, so a stray em-dash breaks the parser.

.PARAMETER Level
  Soft (default) or Full.

.PARAMETER Distro
  WSL distro to clean. Defaults to the current default distro.

.PARAMETER Apply
  Actually make the changes. Without it, nothing is written.

.EXAMPLE
  ./scripts/reset-local-setup.ps1
  Dry run at Soft level - shows what would be removed.

.EXAMPLE
  ./scripts/reset-local-setup.ps1 -Apply
  Soft reset. Re-run the wizard to retest the deploy steps.

.EXAMPLE
  ./scripts/reset-local-setup.ps1 -Level Full -Apply
  Wipes the distro too. Only for retesting InstallWsl / CreateWslUser.

.NOTES
  The hooks in ~/.claude/settings.json belong to Claude Code sessions
  running ON THIS MACHINE. Sessions on a remote server are unaffected.
  settings.json is backed up to settings.json.ymux-reset.<timestamp>
  before any edit.
#>
[CmdletBinding()]
param(
    [ValidateSet("Soft", "Full")]
    [string]$Level = "Soft",
    [string]$Distro,
    [switch]$Apply
)

$ErrorActionPreference = "Stop"
$script:Planned = 0

function Step([string]$msg) { Write-Host "  $msg" }
function Would([string]$msg) {
    $script:Planned++
    if ($Apply) { Write-Host "  [do]   $msg" -ForegroundColor Yellow }
    else { Write-Host "  [plan] $msg" -ForegroundColor DarkGray }
}

# --- WSL helpers -----------------------------------------------------
# Scripts go in on STDIN, never as an argv element: something between
# wsl.exe and the distro expands the text once before the shell sees it,
# so any variable a script assigns reads back empty. Same reason
# local_setup.rs::wsl_exec uses `sh -ls`. Measured 2026-08-10.
function Invoke-Wsl {
    param([string]$Script, [string]$User)
    $wslArgs = @()
    if ($Distro) { $wslArgs += @("-d", $Distro) }
    if ($User) { $wslArgs += @("-u", $User) }
    $wslArgs += @("--", "sh", "-ls")
    $env:WSL_UTF8 = "1"
    $out = $Script | & wsl.exe @wslArgs 2>&1
    # The relay prints getpwnam noise on some distros; not ours.
    ($out | Where-Object { "$_" -notmatch "getpwnam" }) -join "`n"
}

function Test-WslUp {
    try {
        $null = & wsl.exe --status 2>&1
        return ($LASTEXITCODE -eq 0)
    } catch { return $false }
}

Write-Host ""
Write-Host "ymux local-setup reset - level: $Level" -ForegroundColor Cyan
if (-not $Apply) {
    Write-Host "DRY RUN. Nothing will be changed. Add -Apply to make it real." -ForegroundColor Cyan
}
Write-Host ""

# --- 1. Windows-side Claude hooks ------------------------------------
Write-Host "Windows - Claude Code hooks" -ForegroundColor White
$claudeDir = Join-Path $env:USERPROFILE ".claude"
$settingsPath = Join-Path $claudeDir "settings.json"
$hooksPath = Join-Path $claudeDir "hooks.json"

if (Test-Path $settingsPath) {
    $raw = Get-Content $settingsPath -Raw
    if ($raw -match "ymux_hooks_version") {
        Would "strip the ymux hooks block from $settingsPath"
        if ($Apply) {
            $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
            $bk = "$settingsPath.ymux-reset.$stamp"
            Copy-Item $settingsPath $bk -Force
            Step "backup -> $bk"
            $j = $raw | ConvertFrom-Json
            # Remove only what ymux added; the rest is the user's.
            $j.PSObject.Properties.Remove("hooks")
            $j.PSObject.Properties.Remove("ymux_hooks_version")
            ($j | ConvertTo-Json -Depth 40) | Set-Content $settingsPath -Encoding utf8
            Step "rewrote $settingsPath"
        }
    } else {
        Step "no ymux hooks in $settingsPath - leaving it"
    }
} else {
    Step "no $settingsPath"
}

if (Test-Path $hooksPath) {
    Would "delete $hooksPath"
    if ($Apply) { Remove-Item $hooksPath -Force }
} else {
    Step "no $hooksPath"
}

# --- 2. WSL-side artifacts -------------------------------------------
Write-Host ""
Write-Host "WSL - ymux artifacts" -ForegroundColor White

if (-not (Test-WslUp)) {
    Step "WSL is not available - skipping the linux side"
} else {
    $target = if ($Distro) { $Distro } else { "(default distro)" }
    Step "target: $target"

    $probe = Invoke-Wsl -Script 'ls -d "$HOME"/.ymux >/dev/null 2>&1 && echo HAVE || echo NONE; echo "user=$(id -nu) home=$HOME"'
    Step "state: $($probe -replace "`n", ' | ')"

    if ($probe -match "NONE") {
        Step "no ~/.ymux in the distro - nothing to remove"
    } else {
        Would "remove ~/.ymux from the distro (bin, tmux.conf, run)"
        if ($Apply) {
            $r = Invoke-Wsl -Script 'rm -rf "$HOME"/.ymux && echo REMOVED'
            Step "  -> $r"
        }
    }

    $hookProbe = Invoke-Wsl -Script 'grep -q ymux_hooks_version "$HOME"/.claude/settings.json 2>/dev/null && echo HAVE || echo NONE'
    if ($hookProbe -match "NONE") {
        Step "no ymux hooks in the distro's ~/.claude/settings.json"
    } else {
        Would "strip the ymux hooks block inside the distro"
        if ($Apply) {
            # python3 ships with Ubuntu; jq does not. One-liner rather
            # than a heredoc - a heredoc nested in a PowerShell
            # here-string does not survive PowerShell's parser.
            $py = 'import json,sys;p=sys.argv[1];d=json.load(open(p));' +
                  'd.pop("hooks",None);d.pop("ymux_hooks_version",None);' +
                  'json.dump(d,open(p,"w"),indent=2);print("REWROTE")'
            $sh = 'f="$HOME/.claude/settings.json"; ' +
                  'cp "$f" "$f.ymux-reset.$(date +%Y%m%d-%H%M%S)"; ' +
                  "python3 -c '$py' " + '"$f"'
            $r = Invoke-Wsl -Script $sh
            Step "  -> $r"
        }
    }
}

# --- 3. Full: unregister the distro ----------------------------------
if ($Level -eq "Full") {
    Write-Host ""
    Write-Host "WSL - full wipe" -ForegroundColor Red
    $d = $Distro
    if (-not $d) {
        $d = (& wsl.exe -l -q 2>$null | Where-Object { "$_".Trim() } | Select-Object -First 1)
        if ($d) { $d = "$d".Trim() }
    }
    if (-not $d) {
        Step "no distro registered - nothing to unregister"
    } else {
        Would "wsl --unregister $d  <-- DELETES THE ENTIRE LINUX FILESYSTEM"
        if ($Apply) {
            Write-Host ""
            Write-Host "  About to permanently delete the '$d' distro:" -ForegroundColor Red
            Write-Host "  every file, the linux user, installed packages." -ForegroundColor Red
            $answer = Read-Host "  Type the distro name to confirm"
            if ($answer -eq $d) {
                & wsl.exe --terminate $d 2>&1 | Out-Null
                & wsl.exe --unregister $d
                Step "unregistered $d"
            } else {
                Step "name did not match - skipped"
            }
        }
    }
}

Write-Host ""
if ($Apply) {
    Write-Host "Done. $script:Planned action(s) applied." -ForegroundColor Green
    Write-Host "Re-run the Smart Install wizard to test from this state."
    if ($Level -eq "Soft") {
        Write-Host "Soft leaves the distro, the linux user, tmux and /etc/wsl.conf" -ForegroundColor DarkGray
        Write-Host "in place, so InstallWsl and CreateWslUser will still report" -ForegroundColor DarkGray
        Write-Host "'already done'. Use -Level Full to retest those." -ForegroundColor DarkGray
    }
} else {
    Write-Host "$script:Planned action(s) would run. Re-run with -Apply to make it real." -ForegroundColor Cyan
}
Write-Host ""
