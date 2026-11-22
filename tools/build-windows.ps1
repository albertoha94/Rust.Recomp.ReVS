# Builds the port on Windows from your own .ipa.
#
#   powershell -ExecutionPolicy Bypass -File tools\build-windows.ps1 -Ipa <game.ipa> [-Dylibs <folder>]
#
# -Ipa: the unedited Resident_Evil_Mercenaries_VS._1.10.10_ios_4.2.ipa.
# -Dylibs: open-source ARM builds of libgcc, libstdc++ and SQLite that the game links (recompiled with it). By
#   default they are downloaded from touchHLE's repository (https://github.com/touchHLE/touchHLE, folder
#   touchHLE_dylibs, a fixed commit, checked by SHA-256) into build\dylibs. touchHLE itself is not needed.
#
# Needs Rust (rustup, MSVC toolchain), Visual Studio Build Tools (C++) and CMake. Everything generated goes into
# build\ (git-ignored): never commit or share it. Then run: target\release\revs-run.exe build
param(
    [Parameter(Mandatory = $true)][string]$Ipa,
    [string]$Dylibs = ""
)
$ErrorActionPreference = "Stop"
if (-not (Test-Path $Ipa -PathType Leaf)) { throw "$Ipa not found" }
$Ipa = (Resolve-Path $Ipa).Path
if ($Dylibs) { $Dylibs = (Resolve-Path $Dylibs).Path }
Set-Location (Join-Path $PSScriptRoot "..")

if (-not $Dylibs) {
    # touchHLE's touchHLE_dylibs at a fixed commit; the COPYING files are their licences.
    $commit = "e7790a835399a4de7335b61b2cb85ad129527637"
    $want = [ordered]@{
        "libgcc_s.1.dylib"      = "d758fbf1ac952532955d62866bd02df55315c0599f64bdd978ff941fccbdcf83"
        "libstdc++.6.0.9.dylib" = "22bb45b80a1e590ca6f2e0bd07581084ac56b347b0c6005b05ca5551ef27142a"
        "libsqlite3.dylib"      = "d3795f741db6ade461fa07edf13962858fcebc75c738c4c32634c390c93131b6"
        "COPYING.libgcc"        = ""
        "COPYING.libstdcxx"     = ""
        "COPYING.libsqlite3"    = ""
    }
    $Dylibs = Join-Path (Get-Location) "build\dylibs"
    New-Item -ItemType Directory -Force $Dylibs | Out-Null
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    foreach ($name in $want.Keys) {
        $out = Join-Path $Dylibs $name
        if (-not (Test-Path $out)) {
            Write-Output "Downloading $name"
            Invoke-WebRequest -UseBasicParsing -OutFile $out "https://raw.githubusercontent.com/touchHLE/touchHLE/$commit/touchHLE_dylibs/$name"
        }
        if ($want[$name] -and (Get-FileHash -Algorithm SHA256 $out).Hash.ToLower() -ne $want[$name]) {
            Remove-Item $out
            throw "${name}: unexpected SHA-256 (run again to download it again)"
        }
    }
}
foreach ($lib in "libgcc_s.1.dylib", "libstdc++.6.0.9.dylib", "libsqlite3.dylib") {
    if (-not (Test-Path (Join-Path $Dylibs $lib))) { throw "$(Join-Path $Dylibs $lib) not found" }
}

function Invoke-Step([string]$exe, [string[]]$argv) {
    & $exe @argv
    if ($LASTEXITCODE -ne 0) { throw "$exe $($argv -join ' ') failed ($LASTEXITCODE)" }
}

Write-Output "[1/4] Building the recompiler"
Invoke-Step cargo @("build", "--release", "-p", "revs-analyze")
$A = "target\release\revs-analyze.exe"
Write-Output "[2/4] Mapping the game and its libraries"
Invoke-Step $A @("functions", $Ipa, "build")
Invoke-Step $A @("functions", (Join-Path $Dylibs "libgcc_s.1.dylib"), "build\lib\libgcc", "--base=0x700000")
Invoke-Step $A @("functions", (Join-Path $Dylibs "libstdc++.6.0.9.dylib"), "build\lib\libstdc++", "--base=0x800000")
Invoke-Step $A @("functions", (Join-Path $Dylibs "libsqlite3.dylib"), "build\lib\libsqlite3")
Write-Output "[3/4] Recompiling to C"
Invoke-Step $A @("recompile", $Ipa, "build", "all", "--lib=build\lib\libgcc", "--lib=build\lib\libstdc++", "--lib=build\lib\libsqlite3")
# The game is about 19,000 C functions: this step takes several minutes.
Write-Output "[4/4] Building the game (several minutes)"
Invoke-Step cargo @("build", "--release", "-p", "revs-run")
Write-Output ""
Write-Output "Built target\release\revs-run.exe. Run it with: target\release\revs-run.exe build"
