# Records real function inputs with the patched touchHLE (src/revs_record.rs),
# for `revs-check --replay=<file>`.
#
# Uses the sandbox save of player 9 (--player=9), never the normal save.
# The recording holds game memory: it goes to the git-ignored build/ folder.
#
# usage: powershell -File tools\record\record.ps1 -Name match -Seconds 180
param(
    [string]$Name = "run",
    [int]$Seconds = 120,
    [int]$After = 0,
    [int]$PerFunction = 4,
    [int]$MaxMB = 3000,
    # A patched touchHLE checkout built in release (default: touchHLE-src next to this repository).
    [string]$TouchHLE = "$PSScriptRoot\..\..\..\touchHLE-src",
    [string]$Ipa = "$PSScriptRoot\..\..\build\record_game.ipa"
)
$build = (Resolve-Path "$PSScriptRoot\..\..\build").Path
$out = Join-Path $build "record_$Name.bin"
$env:REVS_RECORD = $out
$env:REVS_RECORD_FUNCS = Join-Path $build "recomp\translated.json"
$env:REVS_RECORD_SECS = "$Seconds"
$env:REVS_RECORD_AFTER = "$After"
$env:REVS_RECORD_PER_FN = "$PerFunction"
$env:REVS_RECORD_MAX_MB = "$MaxMB"
Write-Host "Recording to $out for $Seconds s (starting after $After s). Close the game window when done."
Push-Location $TouchHLE
try {
    & "$TouchHLE\target\release\touchHLE.exe" (Resolve-Path $Ipa).Path --player=9 2>&1 |
        Select-String "REVS recorder" | ForEach-Object { $_.Line }
} finally {
    Pop-Location
    Remove-Item Env:REVS_RECORD*
}
