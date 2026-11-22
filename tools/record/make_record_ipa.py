# Builds the .ipa used for recordings: the user's original .ipa with its
# executable swapped for the early-touchHLE-fixed one (18 small patches: Thumb
# fixes, GL client-state NOPs, GameKit off), because the unedited game doesn't
# boot in touchHLE. The checker always runs the ORIGINAL code from the .ipa on
# the recorded state, so the comparison stays exact.
#
# The output contains the game: it goes to the git-ignored build/ folder only.
#
# usage: py make_record_ipa.py <original.ipa> <fixed executable> [out.ipa]
import sys
import zipfile

src, exe = sys.argv[1], sys.argv[2]
dst = sys.argv[3] if len(sys.argv) > 3 else "build/record_game.ipa"
binary = open(exe, "rb").read()
zi = zipfile.ZipFile(src)
# The executable: the biggest file directly inside Payload/<name>.app/ with no extension.
main = max(
    (i for i in zi.infolist()
     if i.filename.count("/") == 2 and ".app/" in i.filename and "." not in i.filename.split("/")[-1]),
    key=lambda i: i.file_size,
)
with zipfile.ZipFile(dst, "w", zipfile.ZIP_DEFLATED) as zo:
    for i in zi.infolist():
        zo.writestr(i, binary if i.filename == main.filename else zi.read(i))
print(f"wrote {dst} ({main.filename} replaced)")
