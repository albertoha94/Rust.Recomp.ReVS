# Upscales the game's images saved by `revs-run --dump-textures` (buttons, icons, fonts, HUD, 3D atlases) with
# Real-ESRGAN, for the port to load instead of the originals.
#
#   build\textures\dump\<hash>_<w>x<h>.png  ->  build\textures\load\<hash>_<w>x<h>[.font|.sharp].png
#
# tools\textures.txt says which images to do and how (2D images, and 3D atlases as `model`). Only images
# not in load\ yet are done, so it can be run again after more dumping; -Force redoes all of them.
# Real-ESRGAN (realesrgan-ncnn-vulkan, from github.com/xinntao/Real-ESRGAN releases) is not part of this
# repository: pass its folder with -Tool. Both folders hold images made from your own game: they stay in
# the git-ignored build folder and are never shared.
param(
    [string]$Build = (Join-Path $PSScriptRoot "..\build"),
    [string]$Tool = "C:\tools\realesrgan",
    [switch]$Force
)
$ErrorActionPreference = "Stop"
$exe = Join-Path $Tool "realesrgan-ncnn-vulkan.exe"
if (-not (Test-Path $exe)) { throw "Real-ESRGAN not found: $exe (pass -Tool <folder>)" }
$dump = Join-Path $Build "textures\dump"
$load = Join-Path $Build "textures\load"
New-Item -ItemType Directory -Force $load | Out-Null
$models = @{ ui = "realesrgan-x4plus-anime"; photo = "realesrgan-x4plus"; model = "realesrgan-x4plus"; font = "realesrgan-x4plus"; sharp = "" }
$kinds = @{}
foreach ($line in Get-Content (Join-Path $PSScriptRoot "textures.txt")) {
    $p = ($line -replace "#.*", "").Trim() -split "\s+"
    if ($p.Count -ge 2 -and $models.ContainsKey($p[1])) { $kinds[$p[0]] = $p[1] }
}
$todo = @()
foreach ($f in Get-ChildItem $dump -Filter *.png) {
    $kind = $kinds[$f.Name.Substring(0, 16)]
    if (-not $kind) { continue }
    $out = if ($kind -eq "font" -or $kind -eq "sharp") { $f.BaseName + ".$kind.png" } else { $f.Name }
    $out = Join-Path $load $out
    if ($Force -or -not (Test-Path $out)) { $todo += , @($f, $kind, $out) }
}
Write-Output "$($todo.Count) image(s) to upscale"
# Real-ESRGAN prints progress on stderr, which Windows PowerShell 5.1 turns into errors under "Stop".
$ErrorActionPreference = "Continue"
$n = 0
foreach ($t in $todo) {
    $n++
    Write-Output "[$n/$($todo.Count)] $($t[0].Name) ($($t[1]))"
    if ($t[1] -eq "sharp") {
        # Enlarged by the port when it loads the image.
        Copy-Item $t[0].FullName $t[2]
    } else {
        & $exe -i $t[0].FullName -o $t[2] -n $models[$t[1]] -s 4 -f png 2>&1 | Out-Null
    }
    if (-not (Test-Path $t[2])) { Write-Output "  failed" }
}
