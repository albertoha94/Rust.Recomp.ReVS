# Development notes

How the port works, for development. For building and playing, see `README.md`.

- **Feature reference:** the touchHLE patches this port replaces (game patches v11-v17 and the touchHLE controller
  features), rebuilt as port code; see `docs/FEATURES.md`.
- An unpacked `.ipa` that touchHLE has patched is **not** original. Don't use it as input.

## Releases

A release must never hold game code, so the runner is split in two:

- `revs-run --features dll` (built in CI) has no game code. At start it loads `rc_game.dll` (`librc_game.so` on
  Linux) from the build folder (`src/game.rs`); the C and the runner meet through a tiny interface:
  `rc_dll_init` hands the DLL the runner's `rc_import` / `rc_miss` / `rc_trace`, and the runner takes
  `rc_lookup`, `rc_trap` and the coverage table (`rc_cov_ptr`). `rc_cpu_size` and the ABI number catch a DLL made
  for another runner. Developer builds (no feature) still link the C in, as before.
- `revs-prepare` (`tools/revs-prepare`) is the setup step: copies the `.ipa` into `--out` (`game.ipa`), runs the
  bundled `revs-analyze` (functions for the game and the three libraries, `recompile`), rewrites `images.json` with
  paths relative to `--out` (the runner resolves relative paths against the build folder), compiles the C with the
  bundled clang (`-O2 -ffp-contract=off -DRC_NATIVE -DRC_DLL`, 8 at a time) and links `rc_game.dll`. It prints
  `progress <0..1> <text>` lines for RecompLauncher and logs to `<out>/prepare.log`. Without `--ipa` it reuses the
  kept `game.ipa`.
- `tools/release/trim-llvm-mingw.py` cuts llvm-mingw down to clang, lld and the 17 headers / 13 libraries a probe
  build of the runtime uses (133 MB, 50 MB zipped). `tools/release/package.py` makes
  `Rust.Recomp.ReVS-v<version>-windows-x64.zip` and refuses a developer `revs-run` or anything made from a game.
  `tools/release/recomplauncher.json` is the launcher manifest (`@VERSION@` filled in).
- `.github/workflows/release.yml` does all of it on a `v*` tag and makes a GitHub pre-release.

Checked by hand (2026-11-22): the zip unpacked into a new folder, setup with only Windows' own folders on PATH
(35 s on 20 threads), menus and a Training match (walk, aim, fire), and setup again without `--ipa`.

## Status

| Phase | | |
|---|---|---|
| 0 | Repo + function map | done |
| 1 | Analysis: jump tables, vtables, ObjC methods, call graph | done |
| 1b | Indirect-call targets, SjLj landing pads per function, C++ classes | done |
| 2 | ARM/Thumb → C recompiler, checked function by function against touchHLE | in progress: ARM, Thumb, VFP, NEON done; rare cases left |
| 3 | Native layer for the 371 imported OS functions (clean native, SDL) | in progress: menus, sound, saves and touch work; a Training match starts (characters not skinned yet) |
| 4 | Native boot: menu → match → saves → Local Match → other platforms | in progress: saves, controller, Local Match, Linux build work; macOS to check |
| 5 | Readable C++, system by system (timing first, for true 60 fps) | |


## Tools

Rust workspace; build with `cargo build --release`.

### Linux and macOS

`tools/build-unix.sh <game.ipa> [dylibs folder]` runs the whole pipeline below (function maps for the
game and the three libraries, `recompile`, `cargo build`) and leaves `target/release/revs-run`; run it with
`target/release/revs-run build`. Generated files go to `build/` as on Windows. The analyzer writes the same C on
every system (checked: byte-identical on Windows and Linux).

Needs Rust (rustup), CMake and a C compiler; SDL2 is built from source (bundled).
- **Linux (Debian / Ubuntu):** `build-essential cmake pkg-config` and SDL's headers: `libx11-dev libxext-dev
  libxrandr-dev libxcursor-dev libxi-dev libxss-dev libxfixes-dev libxinerama-dev libwayland-dev libxkbcommon-dev
  wayland-protocols libdecor-0-dev libgl-dev libegl-dev libpulse-dev libasound2-dev libudev-dev libdbus-1-dev`.
- **macOS:** the Xcode command line tools (`xcode-select --install`) and CMake (`brew install cmake`).

Differences from Windows: `--watchdog` still stops a stuck run and names the last OS call, but host code offsets
print as 0 (the thread sampler is Windows-only); the guest's 4 GiB is one `mmap` (`MAP_NORESERVE`). The repo's
`.cargo/config.toml` sets `CMAKE_POLICY_VERSION_MINIMUM=3.5` (CMake 4 refuses SDL2's old minimum) and C17 for
Linux targets (GCC 15 defaults to C23, which SDL2's source predates); the script sets
`CMAKE_GENERATOR="Unix Makefiles"` over the file's Visual Studio generator. The generated C is compiled with
`-ffp-contract=off` on gcc / clang: the game's VFP multiplies and adds round separately, and fusing them on ARM
hosts (Apple Silicon) would change results and let a Local Match drift between a Mac and a PC.

```
target\release\revs-analyze functions <game.ipa | unpacked Payload folder> [out_dir]
```

Reads the executable (first Mach-O slice; all project addresses refer to it) and writes to `out_dir` (default
`build/`):
- `functions.json`: every function start (from the symbol table, `bl`/`blx` targets and ARM prologues) with size,
  ARM/Thumb, name, demangled name, subsystem, and where the start came from.
  It also holds the call graph: direct calls, tail calls, OS imports called, caller count, indirect
  calls/jumps, switch count, whether the address is stored anywhere, and SjLj landing pads (extra entry
  points the exception unwinder jumps to).
- `imports.json`: each stub / pointer slot and the OS symbol it reaches.
- `objc_methods.json`: every Objective-C method (name, address, mode).
- `vtables.json`: every C++ vtable (`__ZTV` symbols) with its slots, per base class.
- `switches.json`: every jump table (ARM `add pc`/`ldr pc`, Thumb `tbb`/`tbh`/`mov pc`) with its case targets.
- `code_pointers.json`: words that point into code (data, vtables, pc-relative loads) and what they hit.
- `classes.json`: every C++ class from its typeinfo, with its bases and vtable.
- `indirect_calls.json`: every call/jump through a register: `virtual` (vtable slot, with the number of
  functions that can sit there), `field` (function pointer in a struct), `game_via_slot` / `import`
  (through a pointer slot, with the target), `global`, `computed`, or `unknown`.

It prints a summary: counts and code size per subsystem, call-graph totals, and consistency checks
(switch cases outside their function, call/target mode mismatches, tail calls into mid-function), which
should all be 0. HUD (`GM_CtrlLyt_`) is separate from the rest of the
match code. Library groups (`lib:c++`, `lib:openssl`, `lib:zlib`) and `drop:*` show what won't be recompiled or
will be replaced; see `docs/FEATURES.md`.

### Recompiler

```
target\release\revs-analyze recompile <game.ipa> [build_dir] [leaf|all]
```

Needs `functions.json` and `switches.json` from `functions` in `build_dir`. Turns each game function into one C
function `rc_f_<addr>(Cpu*)` that works on the guest registers and the guest's 4 GiB memory, the same layout
touchHLE uses. A whole function is translated or rejected (with a reason). Writes to `build_dir/recomp/`:
- `rc_funcs_NNN.c`, `rc_table.c`: the C (generated from your `.ipa`, never commit it).
- `translated.json`, `failed.json`: what was translated, and why the rest wasn't.

`leaf` (default) keeps only functions that call nothing; `all` keeps every translated function, with calls going
straight to the other translated C (and a trap for anything not translated). It prints coverage and the reasons for
rejected functions. VFP (floating point) follows the ARM rules with touchHLE's FPSCR (0: round to nearest,
denormals kept, NaNs propagated). NEON: only `vadd`/`vsub`/`vmul.f32` on d registers, which is all the game uses;
they follow NEON's fixed rules (flush denormals to zero, default NaN). Thumb (16- and 32-bit): most instructions are
re-encoded as their ARM twin and share the ARM translation; pc-relative forms, IT blocks (translated as one unit),
`cbz`/`cbnz`, `tbb`/`tbh`, `mov pc` branch tables (`addw`/`adr` base, `b.w` entries, from `switches.json`) and
Thumb immediates have their own code. Thumb functions are also listed in
`translated_thumb.json`.

```
cargo build --release -p revs-check
target\release\revs-check <game.ipa> [build_dir] [inputs_per_function] [only: addr | list.json]
```

Compiles the generated C into the checker and links touchHLE's dynarmic (the ARM CPU touchHLE runs the game on;
build touchHLE in release first, or set `TOUCHHLE_DYNARMIC_OUT`). For each function and each random input, it runs
the original code on dynarmic and then the C, from the same registers and memory, and compares registers, flags and
every memory page either one wrote (Windows write watch). Written pages are put back between runs, so no run sees
another's writes. VFP registers and FPSCR are compared too; Thumb functions start in Thumb state. dynarmic may only fetch code from the game's `__text`;
a run that reaches an OS import stub, or jumps into data, stops there and that input isn't compared. Failures go to
`build_dir/recomp/check_failures.txt`, passing functions to `check_passed.json`. `REVS_TRACE=1` prints each
function. When both sides stop at the same OS import stub, the state there is compared (everything up to the call
and its arguments).

#### Real inputs (recordings)

Random inputs leave many functions unchecked (loops, wild pointers). A recording gives real ones:

```
py tools\record\make_record_ipa.py <game.ipa> <touchHLE-fixed BioVS_en executable>
powershell -File tools\record\record.ps1 -Name match -Seconds 180
target\release\revs-check <game.ipa> build 4 --replay=build\record_match.bin
```

- `make_record_ipa.py` builds `build/record_game.ipa`: your `.ipa` with the early touchHLE-fixed executable
  (the unedited game doesn't boot in touchHLE). The checker still runs the original code from your `.ipa`.
- `record.ps1` runs a patched touchHLE (`-TouchHLE <checkout>`, default `touchHLE-src` next to this repository;
  `src/revs_record.rs`, only active with
  `REVS_RECORD` set) on the sandbox save of player 9. It puts a `bkpt` on every translated function (the 2-byte
  Thumb one on Thumb functions, from `translated_thumb.json`); on a hit it
  writes the pages changed since the last sample, then the CPU state, and continues. Replaying the file rebuilds the
  exact memory at every sample. Up to 4 samples per function; stops after `-Seconds` or `-MaxMB`.
- `--replay=` uses those samples instead of random inputs (`inputs_per_function` = most samples per function).
- Recordings hold game memory: they stay in `build/`.

### Native runner (Phase 3)

The game links libstdc++, libgcc and SQLite. They come from touchHLE's open-source ARM builds
(`build\dylibs`, downloaded by the build scripts; not game content) and are recompiled with the game. Map each library once
(`--base=` moves a library built for address 0, applying its rebase table), then recompile everything together:

```
set D=build\dylibs
target\release\revs-analyze functions %D%\libgcc_s.1.dylib build\lib\libgcc --base=0x700000
target\release\revs-analyze functions %D%\libstdc++.6.0.9.dylib build\lib\libstdc++ --base=0x800000
target\release\revs-analyze functions %D%\libsqlite3.dylib build\lib\libsqlite3
target\release\revs-analyze recompile <game.ipa> build all --lib=build\lib\libgcc --lib=build\lib\libstdc++ --lib=build\lib\libsqlite3
cargo build --release -p revs-run
target\release\revs-run build [--strict] [--watchdog=N]
```

`recompile` writes one set of C files and one function table for all images. `translated.json` stays game-only (the
checker and recorder use it), `translated_libs.json` lists the library functions, and `images.json` lists what the
runner loads and where.

Runs the recompiled game without an ARM emulator. Its build compiles the same generated C as the checker, in native
mode. It loads every image from `images.json` into a 4 GiB guest block, links them like dyld (each import goes to a
library export by name, else to a native OS function, else OS data goes to a zeroed host block), then runs the static
initialisers (libraries first) and `main`. It lists every OS function they call, in first-use order, with whether it
has a native version yet (`src/os.rs`). Without a native version a call returns 0;
with `--strict` it stops there. The generated C calls OS functions through `rc_import` and unknown addresses through
`rc_miss`, which each host provides (the checker's versions stop the run, as before).

Objective-C (`src/objc.rs`) works on the game's own ObjC2 metadata in guest memory. Each Apple class the game links
against (`_OBJC_CLASS_$_UIView`, ...) gets a real 32-bit class object in the runtime area (0xD0000000), so the
game's classes sit on top of it unchanged. Selectors are made unique by name. `objc_msgSend` looks up the class
chain: the game's methods and categories first, then the native methods. `UIApplicationMain` reads `NSMainNibFile`
from the .ipa's Info.plist and loads that nib (NIBArchive) like UIKit: objects get `initWithCoder:` from a native
decoder, and outlets are connected by setter or ivar. Then it sends the delegate's launch message. Any message with
no implementation is listed as `-[Class selector]` in first-use order and returns nil.

Some libgcc helpers (integer division, soft-float) are hand-written ARM that isn't translated. They run natively
(`os::OVERRIDE`), also when libgcc calls them itself. Untranslated functions' stubs go through `rc_miss` for this.
Files and sound (Phase 3.5). `src/fs.rs` gives the game an iOS sandbox: `HOME/BioVS_en.app` is the bundle, read
straight from your .ipa (read-only, nothing extracted), and `HOME/Documents`, `Library`, `tmp` are the port's own
folder (`--home=DIR`, default `build/home`; never a touchHLE save). POSIX calls and stdio share one descriptor table
(`FILE` is Darwin's layout, descriptor at +14). `src/nsfile.rs` has NSBundle, NSFileManager, NSFileHandle, NSData,
the NSString path methods, `NSSearchPathForDirectoriesInDomains`, and fills the OS data constants the game reads
(NSString keys, `__stderrp`, `CGAffineTransformIdentity`). `src/audio.rs` feeds an SDL2 queue (44.1 kHz stereo)
from the run loop: sound effects come from the game's own RemoteIO render callback (two buffers at the hardware rate,
like touchHLE does for this engine), music from AVAudioPlayer with the MP3 decoded by symphonia. `--mute` keeps the
sound running but silent (for test runs).

Touch (`src/touch.rs`): the left mouse button is one finger. A click is turned back into the game's portrait points
by undoing the rotation the frame is drawn with, hit-tested from the key window, and sent as
`touchesBegan/Moved/Ended:withEvent:` with a UITouch in an NSSet (NSSet, NSEnumerator and fast enumeration are
native). `--tap=F:x,y` presses at presented frame F and releases 3 frames later, at (x, y) in landscape game points
(480 x 320), for scripted test runs.

Graphics and UIKit (Phase 3.4). `src/window.rs` opens an SDL2 window (touchHLE's rust-sdl2 fork, SDL built from
source) with a desktop OpenGL 2.1 compatibility context. `src/gles/` is touchHLE's OpenGL ES 1.1 layer on top of it
(`gles1_on_gl2.rs`, `gles_generic.rs`, `util.rs`, copied unchanged apart from paths, MPL-2.0 headers kept), with the
PVRTC decoder from `vendor/PVRTDecompress` (MIT). `src/gfx.rs` connects the game's GL imports to it, gives the
iPod touch GL strings, and presents: the 320x480 renderbuffer is drawn into the 960x640 window, rotated for the
`UIInterfaceOrientation` in Info.plist. `src/uikit.rs` has the UIKit/Foundation classes the game uses at start
(UIView/UIWindow/UIViewController from the nib, CAEAGLLayer, EAGLContext, NSTimer, NSRunLoop,
NSNotificationCenter, NSNumber, NSDictionary, ...) and the run loop: after launch it fires due timers (the game's
`-[Bio4AppView onTick]`) until the window closes.

The matrix-palette (skinning) calls touchHLE can't do are skipped, the same calls RE_VS v7-v9 NOP for touchHLE.

```
target\release\revs-run build --frames=60 --screenshot=build\frame.bmp --watchdog=60
```

`--frames=N` stops after N presented frames, `--screenshot=` saves the last one (game output: it stays in `build/`).
`--watchdog=N` stops a run after N seconds and says where it is: the last OS calls, the host code the main thread is
in (named with the linker map), the UI log and the timers.

At the end of a run the reason it ended is printed again as the last line (`end of run: ...`), so it is never
scrolled away. A timer the game re-arms every tick is logged once.

First match (2026-11-16). Starting Single Player > Training stopped twice at untranslated code:
- 0x18698 used `uxtb16`. The recompiler now does the packed extends `sxtb16` / `uxtb16` (and the
  `sxtab16` / `uxtab16` add forms, lane by lane), ARM and Thumb.
- 0x2e7f20 and 0x2e0b68 are functions only reached through pointers: leaf float helpers in tables near
  0x382300, and qsort comparators held in a literal pool. `functions` now also accepts a pointer as a start
  when the code before it ends (maybe followed by its literal pool) and the target is an unconditional
  instruction (`code::leaf_boundary`); pool words additionally need a real return before, no branch to the
  target inside its function, no landing pad, no `nop` (`code::literal_leaf_boundary`). 28 new starts, 12,035
  of 12,035 game functions translated; checker 13,438 pass, 0 differ.
- `gettimeofday`, `localtime_r` and `gmtime_r` are native (UTC).
The match then runs (Docks, scoreboard, HUD). Characters draw wrong: the skinning (matrix palette) arrays are
still skipped.

Debug switches (environment variables): `REVS_PROFILE=N` with `--watchdog` adds N samples of where the main thread
is (host offsets; name them with the linker map, `rc_f_<addr>` is a game function). `REVS_GLDEBUG=1` prints one
line per distinct `glDrawElements` (vertex pointer, index count) with the bounding box of the vertices it uses and
where its centre lands on screen. For the checker, `REVS_TICKS=N` raises the step limit per run (default 50,000);
long loops such as `_mceBoneDeformer_transPoints` need about 20,000,000 to finish on real samples.
`REVS_TRACE_FN=0xADDR,...` prints each call of those game functions (frame, r0-r3, return address; first 2,000
lines; `REVS_TRACE_FROM=F` starts at presented frame F, `REVS_TRACE_VEC=1` adds the 3 floats each argument
points to). `REVS_GLERR=1` prints each game GL call that raises a GL error (the error stays pending for the
game). `REVS_WATCH=EXPR,...` prints a guest value whenever it changes (checked after each frame; `0xADDR`,
`*0xADDR+0xOFF` through a pointer, `b:` for a byte). Runner flags: `--key=F1-F2:NAME` holds a key (SDL name, e.g.
`W`, `Space`, `Return`, `Left Shift`) from frame F1 to F2 through the controller/keyboard mapping;
`--hold=F1-F2:x,y` keeps a second finger down from frame F1 to F2 (`>x2,y2` drags it there, for
the virtual stick; `--tap` is the first finger, so the two can overlap); `--shots=F1,F2,...` also saves those frames next to `--screenshot` as
`<stem>_F.bmp`; `--coverage=FILE` writes the game functions that ran (`--coverage-from=F`: only from presented
frame F on); `--dump=F:FILE` writes guest memory (image below 0x400000 and the used heap) after frame F, as
`REVSDMP1` then regions (u32 start, u32 length, bytes).

Skinning is done on the CPU by the game (`_mceBoneDeformer_transPoints` & co., checked on the match recording:
19 pass, 0 differ); `_hwalDrawMP_ES1` (the GL matrix-palette path) is never called, so the skipped palette calls
don't matter. In the first Training match Chris's body draw is not issued (his head piece and knife are), and the
timer and pause button were missing, unlike touchHLE: the match never started.

Match start fixed (2026-11-16): the game's clock was wrong. `GM_CtrlParam` keeps the match time in ms from
`net::GetMicroTime` (`mach_absolute_time` divided with `__udivdi3`). libgcc's `__udivdi3` calls libgcc's own
`__udivsi3` directly, and that hand-written ARM translates wrong, so the clock read about 49 days (sometimes
going backwards). The Training events wait on the clock (`_Check_Cond_TimeElapsed` / `_TimeLeft`), so the
event that calls `EnableInput(1)` never fired: no touch buttons, start message or timer. `__udivdi3`,
`__divdi3`, `__umoddi3` and `__moddi3` are now native. The match now starts like in touchHLE (Hunnigan's
message, virtual pad and action buttons, timer, pause). Menus run at real speed now, so automated taps need
new frames: title 400, Single Player 560, Training 700, OK 1000, lobby OK 1400 (match from about 1460).

Chris's body fixed (2026-11-16): our frame present (drawing the renderbuffer to the window) ran
`glPopAttrib`, and the desktop driver gives `GL_INVALID_OPERATION` for it every frame. Nothing read that
error, so it was still pending when the game created a texture: `_hwalCreateTexture_ES1` checks
`glGetError` right after `glGenTextures` and gives up on any error. Chris's 512 x 512 body texture is the
first one the match creates, so `__mceTexture_createGLTexture` failed, the element setup returned an error
and `__mceGraphics3D_drawMesh` skipped the mesh (his head piece and weapons had textures from the menus).
Present now leaves no GL error (it clears its own and keeps one the game left for the game's next
`glGetError`). Chris is drawn whole, like in touchHLE.

Play-test (2026-11-16, Training, scripted input): the stick moves and turns Chris, the gun button aims (laser
sight) and a second tap fires (ammo 50 -> 49), the knife button works, the top button opens the inventory
(Equip / Give / Combine / Discard), pause opens the Quit dialog and Cancel returns. Left idle, a whole match
plays: enemies attack, Chris dies and respawns, the COM players score and get time bonuses, the timer runs out,
MISSION OVER, then Total Results. Not seen as port bugs: the game probes for `3d_*.h2z` files the .ipa does not
have.

Networking (2026-11-20), offline like touchHLE with network access off (`tools/revs-run/src/net.rs`): the
game's online check (Capcom "Cloudia") gets real `SCNetworkReachability` refs whose flags are 0 / false (not
reachable), callbacks and run-loop scheduling are refused, `CFArrayCreateMutable` makes an `NSMutableArray`
(the `CFArray*` calls work on it), `inet_pton` / `inet_aton` parse IPv4, `select` reports nothing ready, and
`socket` / `connect` fail (OpenSSL's `RAND_query_egd_bytes` then skips its entropy-daemon socket; returning 0
would have given it stdin). The Training run now calls no missing OS function.

All game functions translated (2026-11-20): the last six (Cloudia `connectionDidFinish:` and `getRequest`, OpenSSL
`EVP_CipherInit_ex`, `asn1_template_clear`, `GENERAL_NAME_cmp`, `X509_policy_check`) jump through Thumb `mov pc`
branch tables. The analyzer already found most tables; it now also takes a 16-bit `adr` as the table base (two
more tables, 309 in all), and the Thumb translator turns `mov pc` into a `switch` on (register - table) / 4.
`recompile` now translates 12,035 of 12,035 game functions. revs-check (200 random inputs each): 4 pass,
0 differ; `connectionDidFinish:` and `X509_policy_check` never returned on dynarmic with random inputs (not
compared). None of the six runs in the recordings or in Training (online and certificate code).

Saves (Phase 4 milestone 3, 2026-11-20): the port reads and writes the same save as touchHLE (the game writes it
itself: `Documents/savevs7.dat` + `cloudiaDatabase.db`; touchHLE's preferences plist is an empty dictionary).
Checked on a copy of a touchHLE save: it loads (player name, Auto Help setting), a full Training match writes it
(only the byte changed in the menus differs) and it loads again. To bring a touchHLE save over once:
`revs-run --import-save=<touchHLE_sandbox/jp.co.capcom.biovsus>` (or its `Documents` folder; `tools/revs-run/src/import.rs`)
copies it into the port's home (`--home`, default `build/home`) and then starts the game. The source is only read;
a save already in the home is kept as `Documents.before-import-<unix time>` (Preferences likewise). The two
programs never share one save folder.

Controller and keyboard, step 1 (2026-11-20; `tools/revs-run/src/input.rs`): touch input is multi-finger now (16
fingers, each its own UITouch; the mouse is finger 0). An Xbox-style controller (SDL game controller, also when
plugged in later) and the keyboard press fingers on the game's touch screen with the user's touchHLE layout.
The layout follows the game's state: gameplay while `GM_CtrlLayout`+0x8c (input on, 0x395598) is set and
`GM_CtrlParam`+0x1da (paused, 0x392462) is clear; menus otherwise (also the pause dialog and MISSION OVER).

| Gameplay | Controller | Keyboard | Touch (landscape points) |
|---|---|---|---|
| move / turn | left stick, D-pad | W A S D | stick, centre 73,247, radius 50 |
| aim / fire | LT | Space, right mouse | 440,210 |
| knife | LB | F | 405,280 |
| contextual | A / B | Q / R | 371,210 / 441,140 |
| inventory | X | E | 371,138 |
| weapon icon | Y | Tab | 453,40 |
| pause | Start | Esc | 156,18 |

| Menus (pause dialog too) | Controller | Keyboard | Touch |
|---|---|---|---|
| focus rectangle, jumps between the game's buttons | D-pad | arrows | - |
| pointer (drawn as a ring) | left stick | - (mouse) | - |
| press the focused button / touch at the pointer | A | Enter | its centre / pointer |
| slide-to-confirm bar (focused) | A | Enter | swipe across the bar |
| back (hidden back button, else a dialog's Cancel) | B | Esc, Backspace | BtnBack / BtnNo centre |
| OK / Cancel (bottom centre) | Start | Shift + Enter | 240,300 |
| side buttons | Y / LB / LT / RB / RT | 1 / 2 / 3 / 4 / 5 | 39,141 / 39,211 / 441,141 / 441,211 / 441,281 |
| swipe | X | X | 147,228 -> 353,228 |

| Inventory | Controller | Keyboard | Touch |
|---|---|---|---|
| focus over the item grid and the four commands | D-pad | arrows | - |
| press the focused button | A | Enter | its centre |
| Equip (Use) / Give / Combine / Discard | Y / RB / X / LB | 1 / 2 / 3 / 4 | BtnSel1-4 centre (when enabled) |
| close | B | Esc, Backspace | BtnBack |

Checked with scripted keys: walking and aiming at once (two fingers), firing, inventory, pause, pointer, Cancel.

Controller and keyboard, step 2 (2026-11-20; `tools/revs-run/src/lyt.rs`, `input.rs`, `gfx.rs`): the port reads the
game's own layout objects instead of pixel probes. `LYT_CmpProduct::Draw` (0x3456c) gets each visible screen's root
layout every frame (children are drawn inside it); `rc_trace` calls `lyt::on_draw` for it (the function is marked
in `rc_cov` like a REVS_TRACE_FN entry). The walk lists the buttons on screen (visible with all parents; a part's
+0x08 node / node+0x30 name, +0x0c children, +0x18 count, +0x3c x,y,w,h, +0x50 enabled, +0x51 visible; button
class = vtable slot 12 `doMsgProc`: Button 0x42b4c, PushButton 0x59f64, ActionButton 0x470bc, SliderButton
0x45f14) and the root names. `REVS_LYT=1` prints both every 30 frames.
- Screens: gameplay = input on, not paused, and a scoreboard root (`Sub_lytScore1-4`) on screen (quitting from
  the pause dialog leaves input on); inventory = that plus the attache-case root `Sub_lytAC`; menus otherwise.
- Backtrack: every `BtnBack` stays (B presses it) but its pictures (its children) get visible = 0 just before
  drawing, so the ↺ sprite is never shown; a mouse press on it is ignored, the pointer's A can't press it and
  the D-pad focus skips it (the original game files are unchanged).
- Menu changes (touchHLE v11-v17 rebuilt, `lyt.rs` / `tweaks.rs`; the game files are unchanged). The screen's own
  update or render function is hooked only to note its layout root; just before that root is drawn the port
  removes parts (enabled = visible = 0 and both touch rectangles off screen, so no touch, click or controller
  press reaches them) or moves them:
  - title menu: the video button (`BtnMovie`, overlay `ImgAnimBtn`) removed;
  - Options: "Get More" (`BtnSel3`) removed, "Language" (`BtnSel4`) moved up into its place;
  - Controls > Other Settings: the "Icon Layout" arrows removed, so the layout the controller mapping uses stays
    as saved; "Icon Transparency" and "Shake Sensitivity" stay 0 (setters store 0, both bytes cleared every
    frame, old saves included). Transparency 0 means the game's own on-screen touch buttons are not drawn.
  - Spanish: "Monojugador" reads "Un jugador" (menu button, help, How to Play), fixed in the loaded text the
    first time each string is read (same length; `cargo test -p revs-run tweaks`).
- Focus: D-pad / arrows step to the nearest enabled button ahead (distance ahead + 2 x sideways), repeat after
  0.4 s then every 0.15 s; the first press shows it on the button nearest the pointer. Moving the stick or the
  mouse hides it.
- Overlay: coloured badges (A green, B red, X blue, Y yellow, others grey) at each mapped point: HUD buttons in
  gameplay, Equip/Give/Combine/Discard and the back button in the inventory, the back button or Cancel in menus.
  Shown while a controller is connected (`REVS_OVERLAY=1` forces it for screenshots); holding View 0.8 s
  toggles it.
Checked with scripted keys (`--key`, REVS_OVERLAY=1): main menu focus and hidden back button, inventory open /
focus / select / close, pause dialog Cancel, slide-to-quit, back from Single Player to the main menu. Not
checked by script: real mouse presses on the hidden back button, controller buttons (same code paths as keys).

Local Match (Phase 4 milestone 4, 2026-11-20; `tools/revs-run/src/gamekit.rs`): the port has its own `GKSession`
(adapted from the touchHLE fork's `gk_session.rs`, MPL-2.0) with the same wire protocol, so port windows can play
each other and touchHLE. Discovery: UDP on ports 41794-41801, `touchHLE-GKSession-1<TAB>session<TAB>peer<TAB>mode<TAB>tcpport<TAB>name`
announced to this computer, the LAN broadcast address and each `--gk-peer=HOST`; data: TCP frames
`[u32 LE length][u8 type][payload]` (Hello 1, Accept 2, Deny 3, Data 4, Bye 5). Delegate calls
(`session:peer:didChangeState:`, connection requests, failures) and the data handler run from the run loop.
The game's own C++ wrapper (`gkcpp::GKSessionCPPWrapper`) needed two more Foundation methods: `-[NSString
longLongValue]` (peer IDs become 64-bit numbers; without it every peer was 0, listed as "unknown", and joining
failed) and `+[NSMutableArray array]` / `addObject:` (the peer list for `sendData:toPeers:`). The "Local Matches
require Bluetooth" screen is only a notice; its OK leads to Create / Join Session.

| Option | |
|---|---|
| `--player=N` | one window per player: its own save folder `build/home_pN` (N > 1; first a copy of `build/home`'s save), "PLAYER N" in the title, controller N only |
| `--controller=N` | only controller N (1 = first connected) drives this window; 0 = all. Without `--player` / `--controller`: all |
| `--gk-peer=HOST` | also announce to HOST (another computer whose broadcasts don't arrive) |

Controllers keep working in a window without focus (`SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS`), so two pads drive
two windows on one computer. Checked with two scripted windows (`--player=1` hosts with 2 COM players, `--player=2`
joins): the guest finds the session, connects, both lobbies show both players, the guest's OK reaches the host,
the host's OK starts the match and both play the same Docks match (same scores, timer, "COM 1 defeated COM 2"
message); closing the guest puts the host's P2 back to "Waiting". Not checked yet: two real controllers, a match to
the end, play against touchHLE or another computer.

Linux and macOS (Phase 4 milestone 5, 2026-11-20): checked on Ubuntu 26.04 (WSL2, Mesa, GCC 15) with
`tools/build-unix.sh` from a fresh copy without `build/`: menus, a Training match (stick drag walks Chris) and a
Local Match against a Windows window (Windows hosts, Linux joins with `--gk-peer`; WSL2 is behind NAT, so each
side names the other) with the same scores on both screens. Screenshots now read the back buffer before the swap
(`gfx.rs`): the front buffer after a swap is undefined, and Mesa on Wayland returned garbage. Not checked: macOS,
controllers and sound on Linux (WSL has neither controller nor checked audio), a native Linux desktop.

Smoother picture (Phase 4, 2026-11-21): the game runs 20 steps a second and draws once per step. By default
the port draws 60 frames a second: after each step, two in-between frames (`interp.rs`) with the camera, every
model's position and its animation frame blended between the last two steps, so the picture is one step (50 ms)
behind. Only what is drawn changes; the game's speed is the same.

| Option | |
|---|---|
| `--fps=60` | default: two in-between frames per step |
| `--fps=40` | one in-between frame per step |
| `--fps=20` | the original: one frame per step, nothing blended |

The game does a little step logic while drawing: `GM_MisConduct::Draw` uses up the step's event (Climb Up, Raise
Up...) when it sets the action button and message window. In-between frames get the step's event back and skip
those setters (`STEP_ONLY` in `main.rs`), so they draw the same HUD and the prompt works. Things placed from an
already blended holder (held weapons, the laser dot) are drawn as they come, not blended again. Checked in
Training and a two-window Local Match, and played by hand (prompts, weapons, message window). Test overrides:
`REVS_EXTRA_RENDER=N` (in-between frames), `REVS_INTERP=0/1/2` (off / delay / predict: no added delay, a little
overshoot when something stops or turns), `REVS_FORCE_EVENT=N` (every step sees event N, e.g. 1 = "Examine").

Full screen (2026-11-21): the game starts full screen (borderless, at the desktop's resolution) and **Alt+Enter**
switches between full screen and a window; the window can also be resized. The picture keeps its 3:2 shape, with
black bars on a screen of another shape, and mouse clicks are mapped through the same rectangle (`window::fit`).
Alt+Enter is not passed to the game as Enter. `--windowed` starts in a window; test runs (`--frames`) and Local
Match windows (`--player`) always start in a window.

Sharper picture (2026-11-21): the game draws into a framebuffer several times its own 480 x 320 (as touchHLE's
`--scale-hack`), which `present()` then fits to the screen. The game still sees its own size: `glViewport` and
`glScissor` are scaled up and the renderbuffer size it reads back is scaled down (`gfx.rs`). 3D edges come out
clean; text and HUD images are the game's own small images, so they gain less. `--scale=N` (1-8) sets it;
`--scale=1` is the original look. By default it is picked from the desktop size: the full-screen picture height
over 320, rounded up (1080p: 4, 1440p: 5, 4K: 7). Test overrides: `REVS_RENDER_SCALE=N`, `REVS_WINDOW=WxH`
(start with another window size).

Cleaner buttons and text (2026-11-21): the game's 2D images (buttons, icons, fonts, HUD) were drawn for a 480 x 320
phone and look rough when enlarged. The port can load cleaner, bigger versions from `build/textures/load/`
(`textures.rs`): each texture the game uploads is named by a hash of its data, and a PNG of any size whose name
starts with that hash is uploaded instead. Replaced textures are always filtered LINEAR (the game asks for NEAREST on
many 2D images). These images are made from your own game: they stay in the git-ignored `build/` and are never
shared or committed. To make them:

1. `revs-run.exe build --dump-textures` and visit the screens you want: the 2D images (8-bit palette and RGBA; the
   PVRTC ones are 3D) are saved to `build/textures/dump/<hash>_<w>x<h>.png`, with the colour under transparent
   pixels filled in from their neighbours so an upscaler leaves no fringes.
2. `tools/upscale-textures.ps1 [-Tool <folder>]` upscales the images listed in `tools/textures.txt` 4x with
   [Real-ESRGAN](https://github.com/xinntao/Real-ESRGAN) (`realesrgan-ncnn-vulkan`, not part of this repository):
   `ui` with the anime model, `photo` with the general one, `font` with the general one plus a sharpened alpha
   edge when the port loads it (`*.font.png`). Images not in the list (3D, effects, and the few 2D ones that looked
   worse upscaled) are left alone. Images seen for the first time need a line in the list. `sharp` (the 40-pixel
   character portraits) uses no AI, which invents detail on faces that small: the port enlarges the original 4x
   itself (bicubic and a mild sharpen) when it loads `*.sharp.png`.

Built-in upscaling (2026-11-23): release players have no `textures/load` (the images come from the game, so they
can't ship), so the port enlarges the images in `tools/textures.txt` itself when the game uploads them and there is
no file for them (`textures.rs`, list compiled in with `include_str!`; hashes only). After the upload it reads the
texture back as RGBA (`glGetTexImage`) and enlarges it 4x:
- `ui`, `photo`, `font`: `edge_upscale`, the idea of AMD FSR 1's EASU for still images, written here: each output
  pixel is a Lanczos-2 blend (FSR's polynomial form) of the 4 x 4 nearest texels, the kernel stretched along the
  local edge (structure tensor of a luma + alpha signal, bilinear from the 4 nearest texels) and narrowed across it,
  then held within the 2 x 2 nearest texels (no halos). In premultiplied alpha, so transparent edges get no fringe.
  Then `sharpen`: a 5 x 5 unsharp mask held within each pixel's 3 x 3 range (0.7 for `ui` and `font`, 0.35 for
  `photo`). `font` then gets a soft alpha step (0.2..0.55; the AI files' 0.35..0.65 eats holes into the game's thin,
  partly transparent glyphs).
- `sharp`: the same bicubic + mild sharpen as `*.sharp.png`.
Threads split the rows: about 0.18 s for a 512 x 512 image, 2.3 s for all 20, spread over the screens where they
first appear. Every enlarged or replaced image is uploaded with mipmaps (`GL_GENERATE_MIPMAP`) and kept trilinear,
since a 4x image is drawn smaller than its own size at usual resolutions. The game reuses GL texture names (3D
models get names of deleted 2D images), so a name leaves the "enlarged" set on `glDeleteTextures` and on any new
upload, which also turns its mipmap filtering back off: a 3D texture left with a mipmap filter and no mipmaps is
incomplete and draws white (first build of this, characters in a match). `--upscale=off` turns the built-in path
off. To compare it with the AI files on the user's own dumps:
`REVS_PREVIEW_DUMP=build/textures/dump REVS_PREVIEW_LOAD=build/textures/load REVS_PREVIEW_OUT=<folder> cargo test
--release -p revs-run --features dll --target-dir target/dll -- --ignored upscale_preview` (crops: original, built-in,
AI file).

Main menu and title (2026-11-21): B / Esc / Backspace on the main menu (Online Match / Local Match / Single Player,
no dialog open) closes the game instead of going back to the title screen (`input.rs`). The title screen's bottom
line reads "(c) CAPCOM 2011 . Recomp by albertoha94": the port copies the game's own copyright line into an empty
band of the title sheet, adds the credit text after it (`tools/revs-run/assets/title_credit.png`, made by
`tools/make-title-credit.ps1`), and points that layout part at the longer line, centred (`textures.rs`, `lyt.rs`).
Capcom's notice stays.
