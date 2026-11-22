#!/bin/sh
# Builds the port on Linux or macOS from your own .ipa.
#
#   tools/build-unix.sh <game.ipa> [dylibs folder]
#
# <game.ipa>: the unedited Resident_Evil_Mercenaries_VS._1.10.10_ios_4.2.ipa.
# [dylibs folder]: open-source ARM builds of libgcc, libstdc++ and SQLite that
# the game links (recompiled with it). By default they are downloaded from
# touchHLE's repository (folder touchHLE_dylibs, a fixed commit, checked by
# SHA-256) into build/dylibs. touchHLE itself is not needed.
#
# Everything generated goes into build/ (git-ignored): never commit or share it.
# Then run: target/release/revs-run build
set -e

if [ $# -lt 1 ] || [ $# -gt 2 ] || [ ! -f "$1" ]; then
    echo "usage: $0 <game.ipa> [dylibs folder]" >&2
    exit 1
fi
IPA=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
if [ $# -eq 2 ]; then
    DYLIBS=$(cd "$2" && pwd)
fi
cd "$(dirname "$0")/.."
if [ -z "$DYLIBS" ]; then
    # touchHLE's touchHLE_dylibs at a fixed commit; the COPYING files are their licences.
    COMMIT=e7790a835399a4de7335b61b2cb85ad129527637
    DYLIBS=$(pwd)/build/dylibs
    mkdir -p "$DYLIBS"
    for entry in \
        "libgcc_s.1.dylib d758fbf1ac952532955d62866bd02df55315c0599f64bdd978ff941fccbdcf83" \
        "libstdc++.6.0.9.dylib 22bb45b80a1e590ca6f2e0bd07581084ac56b347b0c6005b05ca5551ef27142a" \
        "libsqlite3.dylib d3795f741db6ade461fa07edf13962858fcebc75c738c4c32634c390c93131b6" \
        "COPYING.libgcc -" "COPYING.libstdcxx -" "COPYING.libsqlite3 -"; do
        name=${entry% *}
        want=${entry#* }
        if [ ! -f "$DYLIBS/$name" ]; then
            echo "Downloading $name"
            curl -fsSL -o "$DYLIBS/$name" "https://raw.githubusercontent.com/touchHLE/touchHLE/$COMMIT/touchHLE_dylibs/$name"
        fi
        if [ "$want" != "-" ]; then
            got=$( (sha256sum "$DYLIBS/$name" 2>/dev/null || shasum -a 256 "$DYLIBS/$name") | cut -d' ' -f1)
            if [ "$got" != "$want" ]; then
                rm -f "$DYLIBS/$name"
                echo "$name: unexpected SHA-256 (run again to download it again)" >&2
                exit 1
            fi
        fi
    done
fi
for lib in libgcc_s.1.dylib libstdc++.6.0.9.dylib libsqlite3.dylib; do
    if [ ! -f "$DYLIBS/$lib" ]; then
        echo "$DYLIBS/$lib not found" >&2
        exit 1
    fi
done

# SDL2 is built with CMake: .cargo/config.toml names the Visual Studio generator
# for Windows, and a variable set here wins over it.
export CMAKE_GENERATOR="Unix Makefiles"

cargo build --release -p revs-analyze
A=target/release/revs-analyze
"$A" functions "$IPA" build
"$A" functions "$DYLIBS/libgcc_s.1.dylib" build/lib/libgcc --base=0x700000
"$A" functions "$DYLIBS/libstdc++.6.0.9.dylib" build/lib/libstdc++ --base=0x800000
"$A" functions "$DYLIBS/libsqlite3.dylib" build/lib/libsqlite3
"$A" recompile "$IPA" build all --lib=build/lib/libgcc --lib=build/lib/libstdc++ --lib=build/lib/libsqlite3
# The game is about 19,000 C functions: this step takes several minutes.
cargo build --release -p revs-run
echo
echo "Built target/release/revs-run. Run it with: target/release/revs-run build"
