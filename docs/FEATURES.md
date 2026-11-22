# Features to rebuild in the port

**Input** (what everyone uses): the unedited `Resident_Evil_Mercenaries_VS._1.10.10_ios_4.2.ipa`.
**Reference** for features: the earlier touchHLE version of this mod (game patches v11-v17 with their addresses and
how each works, plus the touchHLE controller and launcher changes).

The port never ships either `.ipa`. Features are rebuilt as port code (C++ replacing a recompiled function, or a
hook in the recompiler), never as copied game bytes. In this file, "port" means C++ or a recompiler hook.

## Game patches (v11–v19) → port code

| From | Feature | Notes for the port |
|---|---|---|
| v11–v12, v17 | Settings > Control B: "Icon Layout" locked, arrows removed; config bytes 6/7 forced to 0 | in `Bio4_CTTask_ControllerSettingB`; port: fixed layout, since the native input layer replaces on-screen buttons anyway; **done in the port (2026-11-21): arrows removed, bytes 6/7 kept 0 (`lyt.rs`, `tweaks.rs`)** |
| v12–v14 | Options: "Get More" removed, Language moved up | `BioVS_Hdl_OptTop`; port: remove the button from the layout; **done in the port (2026-11-21, `lyt.rs`)** |
| v14 | Title screen video button removed | **done in the port (2026-11-21, `lyt.rs`)** |
| v15–v16 | Spanish "Single Player" text: "Un jugador" | a text-table edit; port: apply to the loaded text at run time; **done in the port (2026-11-21, `tweaks.rs`)** |
| v17 | Backtrack: the ↺ back sprite is invisible; B presses the hidden back button | port: native input action "back"; **done in the port (`lyt.rs`, `input.rs`)** |
| v18 | Stage 4 "Docks-2" (copy of Docks) in every stage list, with its missions, picture, icon and name | port: data tables extended in code, without the code-cave tricks; **skipped: proofs of concept only (user, 2026-11-21)** |
| v19 | Character 4 "Chris-2" (copy of Chris) | port: a 4th character slot done properly (own pc_param slot, own model and loadout later); **skipped: proofs of concept only (user, 2026-11-21)** |

**Not needed** (touchHLE workarounds): v3/v10 GameKit toggle, v4–v7 Thumb fixes, v7–v9 removed
`glEnableClientState`/`glDisableClientState`/`glDisable`/`glWeightPointerOES` calls.
"Remove" a UI item means invisible and not pressable by controller, keyboard or mouse.

## touchHLE / launcher features → native layer

- Xbox controller play:
  - triggers, shoulder and stick-click buttons
  - stick and D-pad double tap (dodge)
  - per-screen layouts: gameplay vs menu, switched by the top-left scoreboard (a "Game Screen" has it); inventory
    screen; slide-to-confirm dialogs, where Backtrack presses Cancel
  - menu cursor, D-pad focus rectangle, button-to-touch and button-to-swipe mappings
  - on-screen button overlay (View toggles it)
  - In the port these become real input actions: no screen-pixel probes, since the game state is known.
- Local Match with 2–4 players: one copy per player, own save each, tiled borderless windows with a player label.
  In the port: one process with split screen, or several over UDP.
- Launcher: resolution / scale / fullscreen, controller bindings, frame-rate option (off), icon.
- Audio: the SFX crackle fix (byte order) is a touchHLE bug; the native audio layer must decode the same buffers
  correctly.
- 60 fps: the logic counts frames (no time step). Interpolate frames first, then make the update code time-based
  during decompilation (`GM_CtrlLyt_Score::ScoreInfo::Frame` is one of the frame counters).

## Libraries in the game

| Library | Size | Plan | Why |
|---|---|---|---|
| C++ runtime (old GCC libstdc++, `std::`, `__gnu_cxx::`) | ~170 KB | recompile with the game | Game code inlines it, and its objects (`std::string`, `std::vector`, `std::map`) live in guest memory with the old 32-bit layout. A modern C++ library can't be swapped in underneath. Replace it only inside code that has been decompiled. |
| OpenSSL (~2010 build, Thumb, one block) | ~440 KB, 14 % of the code | don't recompile; stub the `net::` wrappers (`net::SSLContext`, `net::EVP`, `net::MD5`, `net::BSDSocket` SSL paths, `net::cgc::Sealed`) | Only used for online play with Capcom's servers, which are gone. Old and insecure. New online play, if ever, gets a modern library behind the same wrappers or new netcode. |
| zlib + minizip | ~13 KB | recompile with the game | Tiny. Its structs live in guest memory, so a host zlib would need marshalling. |
| Cloudia (ads/news), movie players, StoreKit receipts | ~355 KB | drop | Not part of the game. |
