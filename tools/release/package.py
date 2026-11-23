"""Makes the Windows release zip for RecompLauncher (and for anyone who wants
to set it up by hand). It holds no game code or assets: the game is
recompiled on the player's PC from their own .ipa by revs-prepare.

    py tools/release/package.py --version=0.1.0 --clang=<trimmed llvm-mingw> --dylibs=<touchHLE_dylibs> [--out=dist]

Expects the release builds:
    cargo build --release -p revs-analyze -p revs-prepare
    cargo build --release -p revs-run --features dll --target-dir target/dll
--clang is the output of tools/release/trim-llvm-mingw.py.

Writes dist/Rust.Recomp.ReVS-v<version>-windows-x64.zip (files at the top, with
recomplauncher.json next to the programs) and prints its SHA-256.
"""
import argparse
import hashlib
import json
import os
import shutil
import sys
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
NAME = "Rust.Recomp.ReVS"
DYLIBS = ["libgcc_s.1.dylib", "libstdc++.6.0.9.dylib", "libsqlite3.dylib",
          "COPYING.libgcc", "COPYING.libstdcxx", "COPYING.libsqlite3"]
DYLIB_SHA256 = {
    "libgcc_s.1.dylib": "d758fbf1ac952532955d62866bd02df55315c0599f64bdd978ff941fccbdcf83",
    "libstdc++.6.0.9.dylib": "22bb45b80a1e590ca6f2e0bd07581084ac56b347b0c6005b05ca5551ef27142a",
    "libsqlite3.dylib": "d3795f741db6ade461fa07edf13962858fcebc75c738c4c32634c390c93131b6",
}

THIRD_PARTY = """Third-party software in this release
=====================================

Rust.Recomp.ReVS itself: MIT (LICENSE). Source: https://github.com/albertoha94/Rust.Recomp.ReVS

No part of Resident Evil: The Mercenaries VS. is included. The game belongs to
Capcom; the setup step recompiles it on your PC from your own copy.

clang/        LLVM clang and lld from llvm-mingw (https://github.com/mstorsjo/llvm-mingw),
              Apache-2.0 with LLVM exceptions (clang/LICENSE.TXT), with the MinGW-w64
              headers and runtime libraries (clang/licenses-mingw-w64/).
dylibs/       ARM builds of libgcc, libstdc++ (GPL-3.0 with the GCC Runtime Library
              Exception) and SQLite (public domain), from touchHLE's touchHLE_dylibs
              (https://github.com/touchHLE/touchHLE); licences in dylibs/COPYING.*.
revs-run.exe  includes:
              - touchHLE's OpenGL ES layer and GameKit session code (MPL-2.0; source:
                tools/revs-run/src/gles/ and tools/revs-run/src/gamekit.rs in this
                project's repository, and https://github.com/touchHLE/touchHLE)
              - SDL2 (zlib licence, https://libsdl.org) through touchHLE's rust-sdl2 fork (MIT)
              - Imagination Technologies' PVRTC decoder (MIT, vendor/PVRTDecompress)
              - symphonia (MPL-2.0, https://github.com/pdeljanov/Symphonia)
              - other Rust crates listed in Cargo.lock in the repository, under their own
                licences (MIT and/or Apache-2.0)
"""


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", required=True)
    ap.add_argument("--clang", required=True)
    ap.add_argument("--dylibs", required=True)
    ap.add_argument("--out", default=os.path.join(REPO, "dist"))
    a = ap.parse_args()
    version = a.version.lstrip("v")

    stage = os.path.join(a.out, f"{NAME}-v{version}")
    shutil.rmtree(stage, ignore_errors=True)
    os.makedirs(stage)
    exes = {
        "revs-run.exe": os.path.join(REPO, "target", "dll", "release", "revs-run.exe"),
        "revs-prepare.exe": os.path.join(REPO, "target", "release", "revs-prepare.exe"),
        "revs-analyze.exe": os.path.join(REPO, "target", "release", "revs-analyze.exe"),
    }
    for name, path in exes.items():
        if not os.path.isfile(path):
            sys.exit(f"{path} missing: build it first (see this script's header)")
        shutil.copy2(path, os.path.join(stage, name))
    # A revs-run that has the game linked in must never go out: the release one
    # is the small `dll` build (the developer build is about 20 MB bigger).
    if os.path.getsize(exes["revs-run.exe"]) > 12 << 20:
        sys.exit("revs-run.exe looks like a developer build with the game linked in: use --features dll")

    os.makedirs(os.path.join(stage, "runtime"))
    for f in ["rc_runtime.c", "rc_cpu.h"]:
        shutil.copy2(os.path.join(REPO, "tools", "revs-check", "runtime", f), os.path.join(stage, "runtime", f))
    os.makedirs(os.path.join(stage, "dylibs"))
    for f in DYLIBS:
        src = os.path.join(a.dylibs, f)
        if f in DYLIB_SHA256 and sha256(src) != DYLIB_SHA256[f]:
            sys.exit(f"{src}: unexpected SHA-256")
        shutil.copy2(src, os.path.join(stage, "dylibs", f))
    if not os.path.isfile(os.path.join(a.clang, "bin", "clang.exe")):
        sys.exit(f"{a.clang}: not a trimmed llvm-mingw (run trim-llvm-mingw.py)")
    shutil.copytree(a.clang, os.path.join(stage, "clang"))

    for f in ["LICENSE", "README.md"]:
        shutil.copy2(os.path.join(REPO, f), os.path.join(stage, f))
    with open(os.path.join(stage, "THIRD-PARTY.txt"), "w", newline="\r\n") as f:
        f.write(THIRD_PARTY)
    manifest = open(os.path.join(REPO, "tools", "release", "recomplauncher.json"), encoding="utf-8").read()
    manifest = manifest.replace("@VERSION@", version)
    json.loads(manifest)  # still valid JSON
    with open(os.path.join(stage, "recomplauncher.json"), "w", encoding="utf-8", newline="\n") as f:
        f.write(manifest)

    # Nothing made from a game may be in here.
    for d, _, files in os.walk(stage):
        for f in files:
            low = f.lower()
            if low.endswith((".ipa", ".bin", ".mcms", ".h2z", ".png", ".bmp")) or low.startswith("rc_funcs") or low == "rc_game.dll":
                sys.exit(f"refusing to package {os.path.join(d, f)}")

    zip_path = os.path.join(a.out, f"{NAME}-v{version}-windows-x64.zip")
    if os.path.exists(zip_path):
        os.remove(zip_path)
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for d, _, files in os.walk(stage):
            for f in sorted(files):
                full = os.path.join(d, f)
                z.write(full, os.path.relpath(full, stage).replace(os.sep, "/"))
    print(f"{zip_path}: {os.path.getsize(zip_path) / 1048576:.0f} MB, SHA-256 {sha256(zip_path)}")


if __name__ == "__main__":
    main()
