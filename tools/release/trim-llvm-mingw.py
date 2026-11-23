"""Copies the parts of an llvm-mingw (UCRT, x86_64 host) folder that the setup
step (revs-prepare) needs to compile and link the game's C into rc_game.dll:
clang, lld, and only the headers and libraries the game's C uses. Leaves out
other targets, C++, debuggers, Python and tools.

    py tools/release/trim-llvm-mingw.py <llvm-mingw folder> <output folder>

Which headers and libraries: a probe build of the runtime (rc_cpu.h includes
every header the generated C uses) with the full toolchain lists them (-MD, and
the linker's -verbose). llvm-mingw: https://github.com/mstorsjo/llvm-mingw
(LLVM: Apache-2.0 with LLVM exceptions; MinGW-w64 runtime: its own licences).
LICENSE.TXT is copied along.
"""
import glob
import os
import re
import shutil
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
RUNTIME = os.path.join(REPO, "tools", "revs-check", "runtime")


def main():
    if len(sys.argv) != 3 or not os.path.isfile(os.path.join(sys.argv[1], "bin", "clang-target-wrapper.exe")):
        sys.exit("usage: trim-llvm-mingw.py <llvm-mingw folder> <output folder>")
    src, dst = (os.path.abspath(p) for p in sys.argv[1:])
    shutil.rmtree(dst, ignore_errors=True)
    os.makedirs(os.path.join(dst, "bin"))

    # The real compiler (clang.exe in llvm-mingw is a small wrapper) and the
    # linker, with the DLLs they load. The .cfg files give clang llvm-mingw's
    # defaults (compiler-rt, lld) for --target=x86_64-w64-mingw32.
    clang = sorted(glob.glob(os.path.join(src, "bin", "clang-[0-9]*.exe")))[0]
    shutil.copy2(clang, os.path.join(dst, "bin", "clang.exe"))
    for pat in ["ld.lld.exe", "libLLVM-*.dll", "libclang-cpp.dll", "libc++.dll", "libunwind.dll",
                "x86_64-w64-windows-gnu.cfg", "mingw32-common.cfg"]:
        for f in glob.glob(os.path.join(src, "bin", pat)):
            shutil.copy2(f, os.path.join(dst, "bin"))
    shutil.copy2(os.path.join(dst, "bin", "x86_64-w64-windows-gnu.cfg"), os.path.join(dst, "bin", "x86_64-w64-mingw32.cfg"))

    # Probe build with the full toolchain.
    cc = os.path.join(src, "bin", "x86_64-w64-mingw32-clang.exe")
    with tempfile.TemporaryDirectory() as probe:
        with open(os.path.join(probe, "table.c"), "w") as f:
            f.write("const int rc_table[1];\nconst unsigned rc_table_len = 0;\n")
        rt_o, dep = os.path.join(probe, "rt.o"), os.path.join(probe, "rt.d")
        subprocess.run([cc, "-O2", "-DRC_NATIVE", "-DRC_DLL", "-I", RUNTIME, "-MD", "-MF", dep,
                        "-c", os.path.join(RUNTIME, "rc_runtime.c"), "-o", rt_o], check=True)
        link = subprocess.run([cc, "-shared", "-static", "-s", "-o", os.path.join(probe, "probe.dll"), rt_o,
                               os.path.join(probe, "table.c"), "-Wl,-verbose"],
                              check=True, capture_output=True, text=True)
        deps = open(dep).read().replace("\\\n", " ")
        used = set()
        for word in re.split(r"\s+", deps):
            if word.endswith(".h"):
                used.add(word)
        for word in re.findall(r"[^\s()]+\.(?:a|o)\b", link.stdout + link.stderr):
            used.add(word)

    norm_src = os.path.normcase(os.path.normpath(src))
    headers = libs = 0
    for path in sorted(used):
        full = os.path.normcase(os.path.normpath(path))
        if not full.startswith(norm_src + os.sep):
            continue  # the runtime's own headers, the probe's objects
        rel = os.path.relpath(full, norm_src)
        os.makedirs(os.path.join(dst, os.path.dirname(rel)), exist_ok=True)
        shutil.copy2(os.path.join(src, rel), os.path.join(dst, rel))
        if rel.endswith(".h"):
            headers += 1
        else:
            libs += 1
    shutil.copy2(os.path.join(src, "LICENSE.TXT"), os.path.join(dst, "LICENSE.TXT"))
    # MinGW-w64's licences (its runtime is linked into rc_game.dll).
    shutil.copytree(os.path.join(src, "x86_64-w64-mingw32", "share", "mingw32"), os.path.join(dst, "licenses-mingw-w64"))
    size = sum(os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk(dst) for f in fs)
    print(f"kept {headers} headers, {libs} libraries: {size / 1048576:.0f} MB in {dst}")
    if headers == 0 or libs == 0:
        sys.exit("nothing found to keep: the probe build's output wasn't understood")


if __name__ == "__main__":
    main()
