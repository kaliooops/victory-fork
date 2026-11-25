# Starts the title.
#
#   .\play.ps1              release build, the safe settings (about 2 fps in the world)
#   .\play.ps1 -Fast        with the speed switches (about 4 fps), which lose geometry:
#                           the prison's walls and gate go missing (2026-11-25)
#   .\play.ps1 -Diag        diagnostics build (frame capture, debug layer, ...)
#
# The game folder comes from victory.toml (`game_dir`) or VICTORY_GAME_DIR.
# The log goes to victory-data\last-run.log beside the game folder.
param(
    [switch]$Diag,
    [switch]$Fast,
    # Kept so older notes still work: the default is already without the switches.
    [switch]$NoSpeed
)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

$profileName = if ($Diag) { 'diag' } else { 'release' }
$exe = Join-Path $PSScriptRoot "target\$profileName\victory.exe"
if (-not (Test-Path $exe)) {
    Write-Host "Building the $profileName build first..."
    if ($Diag) { cargo build --profile diag -p victory --features diagnostics }
    else { cargo build --release -p victory }
}

$gameDir = $env:VICTORY_GAME_DIR
if (-not $gameDir -and (Test-Path victory.toml)) {
    $line = Select-String -Path victory.toml -Pattern "^\s*game_dir\s*=\s*['""](.+)['""]" | Select-Object -First 1
    if ($line) { $gameDir = $line.Matches[0].Groups[1].Value }
}
if (-not $gameDir) { throw 'No game folder: set game_dir in victory.toml or VICTORY_GAME_DIR.' }
$title = Get-ChildItem -Path $gameDir -Filter '*.exe' | Where-Object { $_.Name -like 'game_*' } | Select-Object -First 1
if (-not $title) { throw "No game_*.exe in $gameDir" }

if ($Fast -and -not $NoSpeed) {
    # Measured on an RTX 5070 in the prison scene, 2026-11-24: about 230 ms
    # a frame with these, against 450-520 ms without (docs/audit-2026-11-24.md).
    # But with them the prison's walls and its gate are not drawn (2026-11-25:
    # a run without them draws everything); which switch does it is not
    # known yet.
    $env:VICTORY_RELAY = '2'
    $env:VICTORY_LATE_WAIT = '3'
    $env:VICTORY_WIDE_BACK = '1'
    $env:VICTORY_CARD_BUFFERS_MIN_MB = '1'
    $env:VICTORY_CARD_BUFFERS_MB = '1024'
}
$env:NO_COLOR = '1'

$log = Join-Path (Split-Path $gameDir -Parent) 'victory-data\last-run.log'
New-Item -ItemType Directory -Force (Split-Path $log -Parent) | Out-Null
Write-Host "Running $($title.Name) ($profileName); log: $log"
# Through cmd so the log is written unwrapped (PowerShell wraps redirected
# lines at the console width).
cmd /c "`"$exe`" run `"$($title.FullName)`" --execute > `"$log`" 2>&1"
