# Resident Evil: Mercenaries VS Recomp

![Title screen](https://github.com/user-attachments/assets/abbdabb7-61e7-4643-a00d-0aa36e11f257)

[![Get it on RecompLauncher](https://recomplauncher.com/assets/press/launcher/badge-dark.svg)](https://recomplauncher.com/game/rust-recomp-revs)

A native PC port of **Resident Evil: The Mercenaries VS.** (iOS, version 1.10.10). The build recompiles the game's
own ARM code to C on your computer, from your own copy of the game, and runs it natively: no emulator. Written in
Rust.

| System | Status |
|---|---|
| Windows | ✅ Runs |
| Steam Deck | ⚠️ Partially works |
| macOS | ⚠️ Not tested |

**This repository contains no part of the game.** You need your own `.ipa`: see "Playing it" below.

## Features

✅ Tested: checked in real runs, by script or by hand. ⚠️ Not tested yet: the code is there but nobody has checked
it yet, please report what you find. ❌ Not available.

### Main Features

| Feature | Status |
|---|---|
| Native recompiled game: all 12,035 game functions translated, menus, Training and full matches play to Total Results | ✅ Tested |
| Sound effects and music (MP3 music decoded natively) | ✅ Tested |
| Offline Play | ✅ Tested |
| Online Play | ❌ Not available: Capcom's servers are gone |
| Full screen by default (borderless, desktop resolution); **Alt+Enter** switches to a resizable window; `--windowed` | ✅ Tested |
| 60 fps (camera, models and animation blended); `--fps=60/40/20` (20 = original) | ✅ Tested |
| Sharper picture: renders several times the game's 480 x 320, picked from your screen (1080p: 4x); `--scale=1..8` | ✅ Tested (1080p) |
| Cleaner buttons, fonts and HUD: the port enlarges the game's 2D images itself (4x, edge-aware), no files needed | ✅ Tested (menus and a match, by script and by hand) |
| Even cleaner: optional AI-upscaled 2D images you make on your own PC | ✅ Tested |
| Sharper character portraits in the lobby and character select | ✅ Tested |
| Color variants: 10 colors per character, picked on character select, COM players too (see "New feature: Color variants" below) | ✅ Tested (Training and two-window Local Match, by script) |

### Menu changes

| Feature | Status |
|---|---|
| Added Backtrack (↺): B / Esc goes back (the game's back sprite is hidden) | ✅ Tested |
| Selection box (teal, always on a button, the top one when a screen opens) on the D-pad / arrows; pointer always shown (mouse, either stick), taking the box to the button under it | ✅ Tested |
| Header: "Control scheme changed." and the scheme's icon when you switch between keyboard / mouse and a controller; "Control disconnected" when a controller is unplugged | ✅ Tested (message); ⚠️ unplugging not tested yet |
| Actions bar (bottom left, in the game's font): Select, Back / Quit, Help, OK, with the controller's or the keyboard's buttons; game buttons under it move up | ✅ Tested |
| Help ("?") on Select / F1, moved to the top left under the title bar | ✅ Tested |
| Online Match greyed on the main menu (not pressable by controller, keyboard or mouse) | ✅ Tested |
| Match scoreboard (P1-P4) moved down a little, under the header's message | ✅ Tested |
| Lobby: a colour ball on each player's portrait (LB / RB or Q / E change it); results and records show it too | ✅ Tested |
| Title screen: "Tap" reads "Press any key", in the same style | ✅ Tested |
| Main menu logo: drawn sharp, from the title screen's logo | ✅ Tested |
| Slide-to-confirm bars pressed with A / Enter | ✅ Tested |
| Button badges (menus and inventory, shown while the controller is in use) | ✅ Tested |
| B / Esc on the main menu closes the game | ✅ Tested |

### Local Match (2-4 players)

| Feature | Status |
|---|---|
| Local Match between port windows on one PC (`--player=N`: own save and controller per window) | ✅ Tested (2 windows, scripted) |
| Local Match between a Windows PC and Linux (`--gk-peer=HOST`) | ✅ Tested (Windows host, Linux in WSL2) |
| Two real controllers on one PC | ⚠️ Not tested yet |
| A Local Match played to the end | ⚠️ Not tested yet |
| Playing against touchHLE (same protocol) | ⚠️ Not tested yet |
| Two separate computers on a home network | ⚠️ Not tested yet |
| 3-4 players | ⚠️ Not tested yet |

## Playing it

### With RecompLauncher, or from a release

Releases are Windows zips with **no game files in them**. A setup step recompiles the game on your PC from your own
`Resident_Evil_Mercenaries_VS._1.10.10_ios_4.2.ipa` (unedited, from your own purchase). It takes about a minute,
and needs nothing else installed: the zip brings a small C compiler (clang from llvm-mingw).

Only version **1.10.10** works for now. Setup checks the game's code first and says so if your copy is another
version (such as 1.10.20) or isn't the unedited game.

- **RecompLauncher:** install the game, pick your `.ipa` when it asks, let setup finish, then Play. Saves go in the
  launcher's data folder.
- **By hand:** unzip the release, then run:

  ```
  revs-prepare.exe --ipa=<your .ipa>
  revs-run.exe game
  ```

  `revs-prepare` makes the `game` folder: a copy of your `.ipa`, maps and `rc_game.dll` (the recompiled game). It's
  yours only: never share that folder.

### Building it from your own copy

You need:
- **The game:** `Resident_Evil_Mercenaries_VS._1.10.10_ios_4.2.ipa`, unedited, from your own purchase.
- **Tools:** [Rust](https://rustup.rs), CMake, and a C/C++ compiler (Windows: Visual Studio Build Tools with
  "Desktop development with C++"; Linux / macOS: see `docs/DEVELOPMENT.md`, "Linux and macOS").

For developers, or for Linux. Windows:

```
powershell -ExecutionPolicy Bypass -File tools\build-windows.ps1 -Ipa <game.ipa>
target\release\revs-run.exe build
```

Linux / macOS:

```
tools/build-unix.sh <game.ipa>
target/release/revs-run build
```

**Never share what you build.** `build/` and a built `revs-run` contain the recompiled game: they are yours only.
The same goes for anything made from your `.ipa`: textures, dumps, saves, screenshots.

### Options

| Option | |
|---|---|
| `--windowed` | start in a window (Alt+Enter switches at any time) |
| `--fps=60` / `40` / `20` | frames drawn per second (20 = the original) |
| `--scale=N` | render scale 1-8 (default: from your screen; 1 = the original look) |
| `--mute` | no sound |
| `--player=N` | Local Match window N: own save (`build/home_pN`), "PLAYER N" in the title, controller N |
| `--controller=N` | only controller N drives this window (0 = all) |
| `--gk-peer=HOST` | Local Match: also announce to HOST (another computer whose broadcasts don't arrive) |
| `--import-save=<folder>` | copy a touchHLE save (`jp.co.capcom.biovsus` or its `Documents`) into the port, then start |
| `--home=DIR` | save folder (default `build/home`) |
| `--upscale=off` | draw the game's 2D images as they are (no built-in enlargement) |
| `--colors=P1:Red,P3:Teal` | these players always wear these colours (for tests) |
| `--dump-textures` | save the game's 2D images to `build/textures/dump` |

### New feature: Color variants

![Color variants on character select](https://github.com/user-attachments/assets/75aadc14-b3fa-43cc-ad4a-e81da1cf2a5d)

Each character has 10 outfit colors:

| | Color |
|---|---|
| 🔵 | Blue |
| 🔴 | Red |
| 🟡 | Yellow |
| 🟤 | Brown |
| 🟠 | Orange |
| 🟢 | Green |
| 🩶 | Gray |
| 🟣 | Purple |
| 🩵 | Teal |
| ⚫ | Black |

Each one starts in its original outfit:

| Character | Default color |
|---|---|
| Chris | 🟢 Green |
| Jill | 🔵 Blue |
| Wesker | ⚫ Black |

- **Character select:** the bar under the character's picture shows the colour (◀ ● GREEN ▶). Click or tap its
  arrows, or move the selection box onto it, then use left / right (A: the next colour). The character's picture
  shows the colour too.
- **Lobby:** a colour ball on the top right corner of each player's portrait. With the selection box on a
  portrait, LB / RB (Q / E) step that player's colour back / on.
- **Results and records:** the match result pages (Total Results, Player Kills...) show each player's ball on their
  portrait; Coin Shoot's stage records show the colour worn when each record was set (records set before this
  shows the character's own colour), kept in `port_records.txt` in the save folder.
- **COM players:** tap a COM's portrait to open character select for it, then pick its colour the same way. By
  default a COM wears its character's starting colour, or the next free one if an earlier player already wears that
  colour on the same character.
- **Saved:** your colour for each character is kept in `port_colors.txt` in the save folder.

The port paints the colours itself when the game loads the outfit, from recipes in `tools/colors.json` (numbers
only, no game images). In a Local Match between port windows, each player sees the others' colours too (a player
on touchHLE sees none).

## Controls

In menus the keys press the game's own touch buttons, so everything the touch screen does still works with the
mouse. In a match the left mouse button fires instead of touching the screen. In a match, F1 / View (or the "?"
icon left of the pause button) shows these pictures:

![Keyboard and mouse controls](https://github.com/user-attachments/assets/fa777ae6-9833-4ea5-b5ca-40e57ce94890)

![Controller controls](https://github.com/user-attachments/assets/91579ae9-5fd2-43db-ac84-f7b9c2219bac)

| Gameplay | Controller | Keyboard / mouse |
|---|---|---|
| move / turn | left stick | W A S D |
| turn around | D-pad down | Shift + S |
| dodge (while aiming) | D-pad, any direction | Shift + W / A / S / D |
| dash (Wesker) | D-pad up | Shift + W |
| aim | LT (held) | right mouse (on / off) |
| fire (only while aiming) | RT | left mouse |
| interact / pick up (prompt under the message: the game's icon and the key) | A | E |
| reload | Y | R |
| previous / next main weapon | LB / RB | mouse wheel up / down |
| knife | right stick press | F |
| inventory | X | Tab |
| pause (single player only) | Start | Esc |
| controls picture (on / off; follows the scheme in use) | View | F1, or click the game's "?" icon left of the pause button |

## Licences and credits

- This project's code: MIT, see `LICENSE`.
- `tools/revs-run/src/gles/`: from [touchHLE](https://github.com/touchHLE/touchHLE), Mozilla Public License 2.0
  (see each file's header). The Local Match session is adapted from a touchHLE fork's GameKit code (MPL-2.0), and
  the build downloads touchHLE's open-source ARM libraries (their licences come with them).
- `vendor/PVRTDecompress/`: Imagination Technologies' PVRTC decoder, MIT (`vendor/PVRTDecompress/LICENSE.md`).
- Built at compile time, not stored here: SDL2 (zlib licence) through touchHLE's rust-sdl2 fork (MIT), symphonia
  (MPL-2.0) and the other crates in `Cargo.lock` under their own licences.
- Resident Evil: The Mercenaries VS. and everything in it belong to Capcom. This repository holds none of it, and it
  is not affiliated with or endorsed by Capcom.

## For developers

How the recompiler, checker and native layer work, with every debug switch: `docs/DEVELOPMENT.md`. Features
rebuilt from the earlier touchHLE version of this mod: `docs/FEATURES.md`.
