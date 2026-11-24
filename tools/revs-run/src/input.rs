//! Controller and keyboard play: held buttons and keys become fingers on the
//! game's touch screen (touch.rs), with the layout of the user's touchHLE
//! setup. Points are landscape game points (480 x 320, as the player sees it).
//!
//! Three layouts, picked from the game's own state:
//! - gameplay (`in_match`): the left stick / D-pad / WASD drive the virtual
//!   stick; LT, LB, A, B, X, Y and Start (or keys) hold fingers on the HUD.
//! - inventory (in a match, the inventory layout `Sub_lytAC` on screen):
//!   D-pad / arrows move a focus rectangle over its buttons, A / Enter press
//!   the focused one, Y / RB / X / LB (1-4) press Equip / Give / Combine /
//!   Discard, B / Esc / Backspace close it.
//! - menus (everything else, the pause dialog included): D-pad / arrows move
//!   the focus over the game's own buttons on screen (lyt.rs), the left stick
//!   moves a pointer; A / Enter press the focused button (or touch at the
//!   pointer, whichever moved last; on a slide-to-confirm bar A slides it);
//!   B / Esc / Backspace press the back button (or the dialog's Cancel);
//!   on the main menu they close the game;
//!   other buttons touch fixed menu points.
//!
//! The back button is not drawn (Backtrack, lyt.rs) and the mouse can't press
//! it: only B does. The mouse stays finger 0 everywhere. Switching layout
//! lifts every finger.
//!
//! Overlay: while a controller is connected, badges show which pad button
//! touches where; holding View (Back) for 0.8 s hides or shows them.

use crate::colorui;
use crate::gfx::Badge;
use crate::lyt::{self, Btn, Kind};
use crate::touch;
use crate::window::Input;
use crate::{Cpu, Host};
use sdl2::controller::{Axis, Button};
use sdl2::keyboard::Scancode as K;
use std::collections::HashSet;
use std::sync::Mutex;

/// Finger of the virtual stick (gameplay) and of the swipe (menus).
const STICK: usize = 2;
/// Fingers of the buttons: FIRST + index in the layout's table.
const FIRST: usize = 3;

/// Virtual stick: centre and radius (touchHLE `--stick-to-touch=23,197,100,100`).
const STICK_CENTRE: (f32, f32) = (73.0, 247.0);
const STICK_RADIUS: f32 = 50.0;
/// Below this the stick counts as centred.
const DEAD_ZONE: f32 = 0.25;
/// Menu pointer speed at full tilt, points per second.
const POINTER_SPEED: f32 = 320.0;

/// One button of a layout: what presses it, and where it touches.
struct Bind {
    pad: &'static [Button],
    /// Pressed while this trigger is past half way.
    trigger: Option<Axis>,
    keys: &'static [K],
    /// Also needs Shift (Shift + Enter for Start in menus).
    shift: bool,
    /// Also pressed by the right mouse button.
    right_mouse: bool,
    at: (f32, f32),
}

const fn b(pad: &'static [Button], keys: &'static [K], x: f32, y: f32) -> Bind {
    Bind { pad, trigger: None, keys, shift: false, right_mouse: false, at: (x, y) }
}

/// Gameplay HUD (the user's touchHLE `--button-to-touch` points).
const GAMEPLAY: &[Bind] = &[
    // LT: aim the gun / fire.
    Bind { pad: &[], trigger: Some(Axis::TriggerLeft), keys: &[K::Space], shift: false, right_mouse: true, at: (440.0, 210.0) },
    b(&[Button::LeftShoulder], &[K::F], 405.0, 280.0), // knife
    b(&[Button::A], &[K::Q], 371.0, 210.0),
    b(&[Button::B], &[K::R], 441.0, 140.0),
    b(&[Button::X], &[K::E], 371.0, 138.0), // inventory
    b(&[Button::Y], &[K::Tab], 453.0, 40.0), // weapon icon
    b(&[Button::Start], &[K::Escape], 156.0, 18.0), // pause
];
/// Overlay badge of each GAMEPLAY entry.
const GAMEPLAY_LABELS: [&str; 7] = ["LT", "LB", "A", "B", "X", "Y", "ST"];

/// Menus (the user's touchHLE `--menu-button-to-touch` points). A and B are
/// handled apart (the focused button, the back button).
const MENU: &[Bind] = &[
    Bind { pad: &[Button::Start], trigger: None, keys: &[K::Return, K::KpEnter], shift: true, right_mouse: false, at: (240.0, 300.0) },
    b(&[Button::Y], &[K::Num1], 39.0, 141.0),
    b(&[Button::LeftShoulder], &[K::Num2], 39.0, 211.0),
    Bind { pad: &[], trigger: Some(Axis::TriggerLeft), keys: &[K::Num3], shift: false, right_mouse: false, at: (441.0, 141.0) },
    b(&[Button::RightShoulder], &[K::Num4], 441.0, 211.0),
    Bind { pad: &[], trigger: Some(Axis::TriggerRight), keys: &[K::Num5], shift: false, right_mouse: false, at: (441.0, 281.0) },
];

/// Menu swipe on X (touchHLE `--menu-button-to-swipe=X,147,228,353,228`).
const SWIPE: ((f32, f32), (f32, f32)) = ((147.0, 228.0), (353.0, 228.0));

/// Inventory buttons: pad, key, the game's button, badge label.
const INVENTORY: [(Button, K, &str, &str); 4] = [
    (Button::Y, K::Num1, "BtnSel1", "Y"),              // Equip
    (Button::RightShoulder, K::Num2, "BtnSel2", "RB"), // Give
    (Button::X, K::Num3, "BtnSel3", "X"),              // Combine
    (Button::LeftShoulder, K::Num4, "BtnSel4", "LB"),  // Discard
];
/// The inventory's root layout (the attache case).
const INVENTORY_ROOT: &str = "Sub_lytAC";
/// D-pad focus: first repeat after this long held, then every NAV_REPEAT s.
const NAV_DELAY: f64 = 0.4;
const NAV_REPEAT: f64 = 0.15;
/// Hold View this long to hide / show the overlay.
const OVERLAY_TOGGLE_HOLD: f64 = 0.8;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Screen {
    Gameplay,
    Inventory,
    Menu,
}

#[derive(Default)]
struct State {
    keys: HashSet<K>,
    pad: HashSet<Button>,
    /// Left stick, -1..1 (y down), and triggers 0..1.
    stick: (f32, f32),
    lt: f32,
    rt: f32,
    right_mouse: bool,
    /// The layout in use.
    screen: Option<Screen>,
    /// Menu pointer (landscape points) and whether it is shown.
    pointer: (f32, f32),
    pointer_shown: bool,
    /// Focused button (its centre) and whether the focus rectangle is shown
    /// (the D-pad was used after the pointer).
    focus: Option<(f32, f32)>,
    focus_shown: bool,
    /// D-pad direction last pass and when it repeats.
    nav: (i32, i32),
    nav_next: f64,
    /// A / B down last pass, and where their finger is held (None: nowhere).
    a_held: bool,
    a_at: Option<(f32, f32)>,
    /// A's finger follows the pointer (it was pressed at the pointer).
    a_follow: bool,
    b_held: bool,
    b_at: Option<(f32, f32)>,
    /// The mouse press started on the hidden back button: ignored to the end.
    mouse_blocked: bool,
    overlay_hidden: bool,
    /// When View went down, and whether this hold already toggled.
    view_since: Option<f64>,
    view_done: bool,
    last: f64,
    /// X was down on the last pass (a swipe starts on the press).
    swipe_held: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Scripted keys (`--key=F1-F2:NAME`): (frame, key, down), in frame order.
static KEY_SCRIPT: Mutex<Vec<(u64, K, bool)>> = Mutex::new(Vec::new());

/// Parses `--key=F1-F2:NAME` (SDL key names: W, Space, Return, Left Shift,
/// Escape, Up, ...): the key is held from presented frame F1 to F2.
pub fn parse_keys(args: &[String]) {
    let mut v = Vec::new();
    for a in args {
        let Some(r) = a.strip_prefix("--key=") else { continue };
        let Some((frames, name)) = r.split_once(':') else { continue };
        let Some((f1, f2)) = frames.split_once('-') else { continue };
        let (Ok(f1), Ok(f2)) = (f1.parse::<u64>(), f2.parse::<u64>()) else { continue };
        match K::from_name(name) {
            Some(k) => {
                v.push((f1, k, true));
                v.push((f2.max(f1 + 1), k, false));
            }
            None => println!("--key: unknown key name {name:?}"),
        }
    }
    v.sort_by_key(|e| e.0);
    *KEY_SCRIPT.lock().unwrap() = v;
}

fn bind_down(s: &State, bd: &Bind) -> bool {
    let shift = s.keys.contains(&K::LShift) || s.keys.contains(&K::RShift);
    bd.pad.iter().any(|p| s.pad.contains(p))
        || bd.trigger.is_some_and(|t| if t == Axis::TriggerLeft { s.lt } else { s.rt } > 0.5)
        || (bd.right_mouse && s.right_mouse)
        || (bd.keys.iter().any(|k| s.keys.contains(k)) && shift == bd.shift)
}

/// The left stick past the dead zone, else (0, 0).
fn stick(s: &State) -> (f32, f32) {
    let (sx, sy) = s.stick;
    let l = (sx * sx + sy * sy).sqrt();
    if l < DEAD_ZONE {
        (0.0, 0.0)
    } else {
        let k = l.min(1.0) / l;
        (sx * k, sy * k)
    }
}

/// Direction from the D-pad or the given keys (up, left, down, right), else the stick.
fn direction(s: &State, keys: [K; 4]) -> (f32, f32) {
    let on = |b: Button, k: K| s.pad.contains(&b) || s.keys.contains(&k);
    let x = on(Button::DPadRight, keys[3]) as i32 - on(Button::DPadLeft, keys[1]) as i32;
    let y = on(Button::DPadDown, keys[2]) as i32 - on(Button::DPadUp, keys[0]) as i32;
    if x != 0 || y != 0 {
        let l = ((x * x + y * y) as f32).sqrt();
        return (x as f32 / l, y as f32 / l);
    }
    stick(s)
}

/// The D-pad or arrow keys as a direction (-1, 0, 1 each).
fn dpad(s: &State) -> (i32, i32) {
    let on = |b: Button, k: K| s.pad.contains(&b) || s.keys.contains(&k);
    (
        on(Button::DPadRight, K::Right) as i32 - on(Button::DPadLeft, K::Left) as i32,
        on(Button::DPadDown, K::Down) as i32 - on(Button::DPadUp, K::Up) as i32,
    )
}

/// `GM_CtrlLayout` (a static object) +0x8c: the player's input is on. The
/// match's `EnableInput(1)` event sets it at the start; it clears when time
/// runs out (MISSION OVER). It stays set in the pause dialog and inventory.
const INPUT_ON: u32 = 0x39550c + 0x8c;
/// `GM_CtrlParam` (a static object) +0x1da: the match is paused (set by
/// `GM_CtrlParam::Pause(bool)`; the pause dialog sets it, the inventory doesn't).
const PAUSED: u32 = 0x392288 + 0x1da;

/// True during play in a match (not paused): gameplay or the inventory.
/// Seen with REVS_WATCH: on from the match start (frame ~1511), off in the
/// pause dialog and from MISSION OVER on; the inventory keeps it on. Quitting
/// from the pause dialog leaves INPUT_ON set, so the match screen's
/// scoreboard (the "Sub_lytScore1" layout, top left) must be on screen too.
fn in_match(h: &Host) -> bool {
    h.rd8(INPUT_ON) != 0 && h.rd8(PAUSED) == 0 && lyt::roots().iter().any(|r| r.starts_with("Sub_lytScore"))
}

/// The back button on screen (it is not drawn, but B presses it).
fn back_button(btns: &[Btn]) -> Option<&Btn> {
    btns.iter().find(|b| b.name == "BtnBack" && b.enabled)
}

/// What B presses: the back button, else a dialog's Cancel (`BtnNo`).
fn b_target(btns: &[Btn]) -> Option<&Btn> {
    back_button(btns).or_else(|| btns.iter().find(|b| b.name == "BtnNo" && b.enabled))
}

/// The main menu (Online Match / Local Match / Single Player), with no
/// dialog over it: there B closes the game instead of going back to the
/// title screen.
fn main_menu(btns: &[Btn]) -> bool {
    let has = |n: &str| btns.iter().any(|b| b.name == n && b.enabled);
    has("BtnSingle") && has("BtnOffline") && !has("BtnNo")
}

/// The buttons the D-pad focus can land on.
fn focusable(btns: &[Btn], screen: Screen) -> Vec<&Btn> {
    btns.iter()
        .filter(|b| b.enabled && b.name != "BtnBack" && b.rect[2] > 0.0 && b.rect[3] > 0.0)
        .filter(|b| screen != Screen::Inventory || b.root == INVENTORY_ROOT)
        .collect()
}

fn nearest<'a>(list: &[&'a Btn], p: (f32, f32)) -> Option<&'a Btn> {
    let d = |b: &Btn| {
        let (x, y) = b.centre();
        (x - p.0).powi(2) + (y - p.1).powi(2)
    };
    list.iter().copied().min_by(|a, b| d(a).total_cmp(&d(b)))
}

/// The button the focus is on: the one under it, else the nearest.
fn resolve<'a>(list: &[&'a Btn], focus: (f32, f32)) -> Option<&'a Btn> {
    list.iter().copied().find(|b| b.contains(focus)).or_else(|| nearest(list, focus))
}

/// The next button from `cur` in direction (dx, dy): ahead of it, favouring
/// the ones nearly in line.
fn step<'a>(list: &[&'a Btn], cur: &Btn, dx: f32, dy: f32) -> Option<&'a Btn> {
    let (fx, fy) = cur.centre();
    list.iter()
        .copied()
        .filter_map(|b| {
            let (x, y) = b.centre();
            let (ex, ey) = (x - fx, y - fy);
            let ahead = ex * dx + ey * dy;
            (ahead > 4.0).then(|| (ahead + 2.0 * (ex * dy - ey * dx).abs(), b))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|e| e.1)
}

/// A scripted drag on the stick finger (one swipe), unless one is running.
fn swipe(h: &mut Host, from: (f32, f32), to: (f32, f32)) {
    if touch::is_down(h, STICK) || h.touch.script.iter().any(|e| e.1 == STICK) {
        return;
    }
    let f = h.gfx.as_ref().map_or(0, |g| g.frames);
    h.touch.script.push((f, STICK, 0, from.0, from.1));
    for k in 1..=4u64 {
        let t = k as f32 / 4.0;
        h.touch.script.push((f + k, STICK, 1, from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t));
    }
    h.touch.script.push((f + 6, STICK, 3, to.0, to.1));
    h.touch.script.sort_by_key(|e| e.0);
}

fn badge_colour(label: &str) -> [f32; 3] {
    match label {
        "A" => [0.20, 0.65, 0.20],
        "B" => [0.80, 0.18, 0.18],
        "X" => [0.20, 0.40, 0.90],
        "Y" => [0.85, 0.70, 0.10],
        _ => [0.30, 0.30, 0.32],
    }
}

fn badge(at: (f32, f32), label: &'static str) -> Badge {
    Badge { at: (at.0.clamp(10.0, 470.0), at.1.clamp(10.0, 310.0)), label, colour: badge_colour(label) }
}

/// A badge at a button's left end (inside it, so it reads as that button's).
fn badge_left(b: &Btn, label: &'static str) -> Badge {
    badge((b.rect[0] + 14.0, b.rect[1] + b.rect[3] / 2.0), label)
}

fn lift_all(h: &mut Host, c: &mut Cpu) {
    for id in STICK..touch::FINGERS {
        if touch::is_down(h, id) {
            let pt = h.touch.fingers.get(id).map_or((0.0, 0.0), |f| f.loc);
            touch::finger(h, c, id, 3, pt);
        }
    }
}

/// Holds or lifts finger `id` at landscape point `at`.
fn hold(h: &mut Host, c: &mut Cpu, id: usize, down: bool, at: (f32, f32)) {
    match (down, touch::is_down(h, id)) {
        (true, false) => touch::finger_at(h, c, id, 0, at.0, at.1),
        (true, true) => touch::finger_at(h, c, id, 1, at.0, at.1),
        // A finger lifts where it is.
        (false, true) => {
            let pt = h.touch.fingers.get(id).map_or((0.0, 0.0), |f| f.loc);
            touch::finger(h, c, id, 3, pt);
        }
        (false, false) => {}
    }
}

/// Window events and scripted taps: called by the run loop on every pass.
pub fn pump(h: &mut Host, c: &mut Cpu, events: Vec<Input>) {
    let mut guard = STATE.lock().unwrap();
    let s = guard.get_or_insert_with(|| State { pointer: (240.0, 160.0), ..State::default() });
    for e in events {
        match e {
            Input::Mouse(p, x, y) => {
                s.pointer_shown = false;
                s.focus_shown = false;
                // Backtrack: a press on the hidden back button is ignored.
                if p == 0 {
                    let at = touch::window_to_landscape(h, x, y).unwrap_or((-1.0, -1.0));
                    s.mouse_blocked = back_button(&lyt::buttons()).is_some_and(|b| b.contains(at));
                }
                if !s.mouse_blocked {
                    touch::mouse(h, c, p, x, y);
                }
                if p == 3 {
                    s.mouse_blocked = false;
                }
            }
            Input::RightMouse(d) => s.right_mouse = d,
            Input::Key(k, true, _) => {
                s.keys.insert(k);
            }
            Input::Key(k, false, _) => {
                s.keys.remove(&k);
            }
            Input::Pad(b, true) => {
                s.pad.insert(b);
            }
            Input::Pad(b, false) => {
                s.pad.remove(&b);
            }
            Input::Axis(a, v) => {
                let f = (v as f32 / 32767.0).clamp(-1.0, 1.0);
                match a {
                    Axis::LeftX => s.stick.0 = f,
                    Axis::LeftY => s.stick.1 = f,
                    Axis::TriggerLeft => s.lt = f,
                    Axis::TriggerRight => s.rt = f,
                    _ => {}
                }
            }
        }
        if c.halt != 0 {
            return;
        }
    }
    touch::pump_script(h, c);
    if c.halt != 0 || h.gfx.is_none() {
        return;
    }
    {
        let frame = h.gfx.as_ref().map_or(0, |g| g.frames);
        let mut ks = KEY_SCRIPT.lock().unwrap();
        while let Some(&(f, k, down)) = ks.first() {
            if frame < f {
                break;
            }
            ks.remove(0);
            if down {
                s.keys.insert(k);
            } else {
                s.keys.remove(&k);
            }
        }
    }
    let now = h.ui.now();
    let dt = if s.last == 0.0 { 0.0 } else { (now - s.last).clamp(0.0, 0.1) as f32 };
    s.last = now;

    let mut btns = lyt::buttons();
    let screen = if !in_match(h) {
        Screen::Menu
    } else if btns.iter().any(|b| b.root == INVENTORY_ROOT) {
        Screen::Inventory
    } else {
        Screen::Gameplay
    };
    // The port's colour menus (lobby chips, the character select bar).
    let port_ui = colorui::frame(&btns, screen == Screen::Menu);
    btns.extend(colorui::buttons());
    if s.screen != Some(screen) {
        if s.screen.is_some() {
            lift_all(h, c);
        }
        s.screen = Some(screen);
        s.a_at = None;
        s.b_at = None;
        // A and B need a fresh press on the new screen (Esc that opened the
        // pause dialog must not also cancel it).
        s.a_held = true;
        s.b_held = true;
    }
    let mut badges = Vec::new();
    let mut focus_rect = None;
    if screen == Screen::Gameplay {
        s.pointer_shown = false;
        s.focus_shown = false;
        let (dx, dy) = direction(s, [K::W, K::A, K::S, K::D]);
        if dx == 0.0 && dy == 0.0 {
            hold(h, c, STICK, false, STICK_CENTRE);
        } else if !touch::is_down(h, STICK) {
            // The touch starts at the stick's centre; it moves next pass.
            hold(h, c, STICK, true, STICK_CENTRE);
        } else {
            let at = (STICK_CENTRE.0 + dx * STICK_RADIUS, STICK_CENTRE.1 + dy * STICK_RADIUS);
            hold(h, c, STICK, true, at);
        }
        for (i, bd) in GAMEPLAY.iter().enumerate() {
            let down = bind_down(s, bd);
            hold(h, c, FIRST + i, down, bd.at);
            badges.push(badge((bd.at.0 - 18.0, bd.at.1 - 18.0), GAMEPLAY_LABELS[i]));
        }
        badges.push(badge((STICK_CENTRE.0 - 30.0, STICK_CENTRE.1 - 30.0), "LS"));
    } else {
        // The pointer: the left stick.
        let (dx, dy) = stick(s);
        if dx != 0.0 || dy != 0.0 {
            s.pointer_shown = true;
            s.focus_shown = false;
            s.pointer.0 = (s.pointer.0 + dx * POINTER_SPEED * dt).clamp(0.0, 479.0);
            s.pointer.1 = (s.pointer.1 + dy * POINTER_SPEED * dt).clamp(0.0, 319.0);
        }
        // The focus: the D-pad / arrows, over the game's own buttons.
        let list = focusable(&btns, screen);
        let mut cur = s.focus.and_then(|f| resolve(&list, f));
        let nav = dpad(s);
        if nav != (0, 0) && (nav != s.nav || now >= s.nav_next) {
            s.nav_next = now + if nav != s.nav { NAV_DELAY } else { NAV_REPEAT };
            if !s.focus_shown || cur.is_none() {
                // The first press shows the focus nearest the pointer.
                cur = nearest(&list, s.pointer);
            } else if cur.is_some_and(|b| b.name == colorui::BAR_NAME) && nav.0 != 0 && nav.1 == 0 {
                // On the colour bar left / right change the colour.
                colorui::step(nav.0);
            } else if let Some(from) = cur {
                let l = ((nav.0 * nav.0 + nav.1 * nav.1) as f32).sqrt();
                cur = step(&list, from, nav.0 as f32 / l, nav.1 as f32 / l).or(cur);
            }
            s.focus_shown = cur.is_some();
            s.pointer_shown = false;
        }
        s.nav = nav;
        if let Some(b) = cur {
            s.focus = Some(b.centre());
        }
        let focused = cur.filter(|_| s.focus_shown).cloned();

        // A / Enter: the focused button, else at the pointer.
        let shift = s.keys.contains(&K::LShift) || s.keys.contains(&K::RShift);
        let a = s.pad.contains(&Button::A) || ((s.keys.contains(&K::Return) || s.keys.contains(&K::KpEnter)) && !shift);
        if a && !s.a_held {
            s.a_follow = false;
            s.a_at = None;
            match &focused {
                Some(b) if b.kind == Kind::Slider => {
                    // Slide-to-confirm: A slides the knob across the bar.
                    let y = b.rect[1] + b.rect[3] / 2.0;
                    swipe(h, (b.rect[0] + 38.0, y), (b.rect[0] + b.rect[2] - 18.0, y));
                }
                Some(b) => s.a_at = Some(b.centre()),
                None => {
                    s.pointer_shown = true;
                    // Backtrack: the pointer can't press the hidden back button.
                    if !back_button(&btns).is_some_and(|b| b.contains(s.pointer)) {
                        s.a_at = Some(s.pointer);
                        s.a_follow = true;
                    }
                }
            }
        }
        if !a {
            s.a_at = None;
        } else if s.a_follow {
            s.a_at = Some(s.pointer);
        }
        s.a_held = a;
        hold(h, c, FIRST, s.a_at.is_some(), s.a_at.unwrap_or(s.pointer));

        // B / Esc / Backspace: the back button, or a dialog's Cancel.
        let bk = s.pad.contains(&Button::B) || s.keys.contains(&K::Escape) || s.keys.contains(&K::Backspace);
        if bk && !s.b_held {
            s.b_at = b_target(&btns).map(|b| b.centre());
            if main_menu(&btns) {
                println!("B on the main menu: closing the game");
                s.b_at = None;
                if let Some(g) = h.gfx.as_mut() {
                    g.window.quit = true;
                }
            }
        }
        if !bk {
            s.b_at = None;
        }
        s.b_held = bk;
        hold(h, c, FIRST + 1, s.b_at.is_some(), s.b_at.unwrap_or((0.0, 0.0)));
        if let Some(b) = b_target(&btns) {
            badges.push(if b.name == "BtnBack" { badge(b.centre(), "B") } else { badge_left(b, "B") });
        }

        if screen == Screen::Inventory {
            for (i, &(pad, key, name, label)) in INVENTORY.iter().enumerate() {
                let target = btns.iter().find(|b| b.name == name && b.root == INVENTORY_ROOT);
                let down = s.pad.contains(&pad) || s.keys.contains(&key);
                let at = target.filter(|b| b.enabled).map(|b| b.centre());
                hold(h, c, FIRST + 2 + i, down && at.is_some(), at.unwrap_or((0.0, 0.0)));
                if let Some(b) = target {
                    badges.push(badge_left(b, label));
                }
            }
        } else {
            for (i, bd) in MENU.iter().enumerate() {
                let down = bind_down(s, bd);
                hold(h, c, FIRST + 2 + i, down, bd.at);
            }
            // X: one swipe per press.
            let x = s.pad.contains(&Button::X) || s.keys.contains(&K::X);
            if x && !s.swipe_held {
                swipe(h, SWIPE.0, SWIPE.1);
            }
            s.swipe_held = x;
        }
        if let Some(b) = &focused {
            s.pointer_shown = false;
            focus_rect = Some(b.rect);
        }
    }

    // The overlay: shown while a controller is connected; hold View to toggle.
    let view = s.pad.contains(&Button::Back);
    match (view, s.view_since) {
        (true, None) => {
            s.view_since = Some(now);
            s.view_done = false;
        }
        (true, Some(t)) if !s.view_done && now - t >= OVERLAY_TOGGLE_HOLD => {
            s.overlay_hidden = !s.overlay_hidden;
            s.view_done = true;
            println!("controller overlay {}", if s.overlay_hidden { "hidden" } else { "shown" });
        }
        (false, _) => s.view_since = None,
        _ => {}
    }
    let pads = h.gfx.as_ref().map_or(0, |g| g.window.pad_count());
    // REVS_OVERLAY=1 shows it without a controller (for test screenshots).
    let show_badges = (pads > 0 || std::env::var_os("REVS_OVERLAY").is_some()) && !s.overlay_hidden;
    let shown = s.pointer_shown.then_some(s.pointer);
    if let Some(g) = h.gfx.as_mut() {
        g.pointer = shown;
        g.focus = focus_rect;
        g.badges = if show_badges { badges } else { Vec::new() };
        g.ui = port_ui;
    }
}
