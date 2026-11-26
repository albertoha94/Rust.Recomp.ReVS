//! The controls picture: F1 / View in a match (or a click on the help icon
//! left of the pause button) shows how to play with the
//! control scheme in use (notice.rs): a drawing of the controller, or of
//! the keyboard and mouse, with the keys used lit, and every action with its
//! key or button. Drawn by the port in the actions bar's style (no game
//! pictures). It fades in and out, and crossfades when the scheme changes
//! while it is up.

use crate::actions::{keycap, pad_prompt, Pad, PROMPT};
use crate::gfx::{label_width, Ui};
use crate::notice::Scheme;
use std::sync::Mutex;

/// Fade time (seconds).
const FADE: f32 = 0.2;

struct State {
    /// Asked to show.
    shown: bool,
    /// Opacity of each scheme's picture (keyboard, controller).
    alpha: [f32; 2],
    /// Time of the last frame (seconds).
    last: f64,
}

static STATE: Mutex<State> = Mutex::new(State { shown: false, alpha: [0.0; 2], last: 0.0 });

/// Shows the picture, or hides it if shown.
pub fn toggle() {
    let mut s = STATE.lock().unwrap();
    s.shown = !s.shown;
}

/// The picture is on screen (or fading).
pub fn visible() -> bool {
    let s = STATE.lock().unwrap();
    s.shown || s.alpha.iter().any(|&a| a > 0.0)
}

/// Hides it (fading out).
pub fn hide() {
    STATE.lock().unwrap().shown = false;
}

/// The picture to draw this frame (nothing once faded out).
pub fn frame(now: f64, scheme: Scheme) -> Vec<Ui> {
    let mut s = STATE.lock().unwrap();
    let dt = if s.last == 0.0 { 0.0 } else { (now - s.last).clamp(0.0, 0.1) as f32 };
    s.last = now;
    let step = dt / FADE;
    for (i, sch) in [Scheme::Keyboard, Scheme::Controller].into_iter().enumerate() {
        let target = if s.shown && sch == scheme { 1.0 } else { 0.0 };
        let a = &mut s.alpha[i];
        *a = if *a < target { (*a + step).min(target) } else { (*a - step).max(target) };
    }
    let plate = s.alpha[0].max(s.alpha[1]);
    if plate <= 0.0 {
        return Vec::new();
    }
    let mut ui: Vec<Ui> = panel().into_iter().map(|u| u.faded(plate)).collect();
    for (i, sch) in [Scheme::Keyboard, Scheme::Controller].into_iter().enumerate() {
        if s.alpha[i] > 0.0 {
            let body = match sch {
                Scheme::Keyboard => keyboard(),
                Scheme::Controller => controller(),
            };
            ui.extend(body.into_iter().map(|u| u.faded(s.alpha[i])));
        }
    }
    ui
}

const TEAL: [f32; 4] = [0.33, 0.78, 0.68, 1.0];
const TEAL_HI: [f32; 4] = [0.66, 0.97, 0.88, 1.0];
const INK: [f32; 4] = [0.02, 0.07, 0.07, 1.0];
const WHITE: [f32; 4] = [0.93, 0.93, 0.89, 1.0];
const DIM: [f32; 4] = [0.30, 0.38, 0.37, 1.0];
const BODY: [f32; 4] = [0.07, 0.17, 0.16, 1.0];
const BODY_LOW: [f32; 4] = [0.03, 0.09, 0.09, 1.0];
const WELL: [f32; 4] = [0.02, 0.05, 0.05, 1.0];

/// The help icon: the game's own "?" (the menus' help button picture, on
/// the action icon sheet at 192,0) left of the match's pause button
/// (`BtnPause`, [133, -4, 46, 46], centre 156,19: a disc about 34 points
/// across), as big as it; the "?" disc fills 50 of its 64 texels. Under
/// each, the key / button that does the same.
const PAUSE: (f32, f32) = (156.0, 19.0);
const HELP: (f32, f32) = (118.0, 19.0);
const HELP_SIZE: f32 = 45.0;
const HELP_SRC: [f32; 4] = [192.0, 0.0, 64.0, 64.0];
/// The disc's radius (clicks), and the prompts' centre line under the discs.
const DISC_R: f32 = 17.0;
const PROMPT_Y: f32 = 45.0;

/// A point on the help icon (a mouse click there opens the picture).
pub fn on_help(at: (f32, f32)) -> bool {
    let (dx, dy) = (at.0 - HELP.0, at.1 - HELP.1);
    dx * dx + dy * dy <= (DISC_R + 2.0) * (DISC_R + 2.0)
}

/// The help icon and its key, and the pause button's key (`pause`: the game
/// shows its pause button; single player only), in a match's gameplay.
pub fn help_icon(scheme: Scheme, pause: bool) -> Vec<Ui> {
    let h = HELP_SIZE;
    let mut ui = vec![Ui::Icon([HELP.0 - h / 2.0, HELP.1 - h / 2.0, h, h], HELP_SRC, 1.0)];
    let mut under = |c: (f32, f32), key: &str, pad: Option<Pad>| match scheme {
        Scheme::Keyboard => {
            let w = (label_width(key, 10.0) + 6.0).max(17.0);
            keycap(&mut ui, key, [c.0 - w / 2.0, c.1 - 7.0, w, 14.0]);
        }
        Scheme::Controller => {
            pad_prompt(&mut ui, pad.unwrap_or(Pad::Face("")), c);
            if pad.is_none() {
                view_glyph(&mut ui, c, 1.0, TEAL_HI);
            }
        }
    };
    under((HELP.0, PROMPT_Y), "F1", None);
    if pause {
        under((PAUSE.0, PROMPT_Y), "Esc", Some(Pad::Menu));
    }
    ui
}

/// The panel (x, y, w, h), the title's centre line, the drawing's centre,
/// the rows' first line, the row step and the two columns' left ends.
const PANEL: [f32; 4] = [40.0, 30.0, 400.0, 282.0];
const TITLE_Y: f32 = 44.0;
const DRAWING: (f32, f32) = (240.0, 106.0);
const ROW_Y: f32 = 166.0;
const ROW_STEP: f32 = 19.0;
const COLS: [f32; 2] = [56.0, 262.0];
/// Where each column's labels start, from the column (after its widest
/// prompt): keyboard and controller.
const KEY_LABEL_X: [f32; 2] = [66.0, 26.0];
const PAD_LABEL_X: [f32; 2] = [26.0, 26.0];
/// The controller's prompts are centred in a slot this wide.
const SLOT: f32 = 18.0;
const TEXT: f32 = 12.0;

fn panel() -> Vec<Ui> {
    let [x, y, w, h] = PANEL;
    vec![
        Ui::Rect([x, y, w, h], [0.02, 0.07, 0.07, 0.88]),
        Ui::Rect([x, y + h - 1.0, w, 1.0], TEAL_HI),
        Ui::Rect([x, y, w, 1.0], TEAL_HI),
        Ui::Rect([x + 16.0, TITLE_Y + 10.0, w - 32.0, 1.0], [0.66, 0.97, 0.88, 0.5]),
    ]
}

fn title(ui: &mut Vec<Ui>, text: &str) {
    let size = 15.0;
    ui.push(Ui::Label((240.0 - label_width(text, size) / 2.0, TITLE_Y), text.into(), size, WHITE));
}

/// Mouse buttons.
#[derive(Clone, Copy, PartialEq)]
enum Click {
    Left,
    Right,
    Wheel,
}

/// D-pad arms lit: up, down, left, right.
type Arms = [bool; 4];

/// What a row shows before its label.
enum Prompt {
    /// Key caps joined by "+".
    Keys(&'static [&'static str]),
    /// A small mouse with one button lit.
    Mouse(Click),
    /// A round face button (A, B, X, Y) or Menu.
    Face(Pad),
    /// The View button (two windows).
    View,
    /// A pill with its name (LB, RT...).
    Pill(&'static str),
    /// A thumbstick, "L" or "R".
    Stick(&'static str),
    /// The D-pad's cross with these arms lit.
    DPad(Arms),
}

fn row(ui: &mut Vec<Ui>, col: usize, i: usize, p: Prompt, label: &str, label_x: [f32; 2]) {
    let x = COLS[col];
    let cy = ROW_Y + ROW_STEP * i as f32;
    let h = PROMPT + 1.0;
    // The controller's prompts: centred in the slot, so they line up.
    let mid = (x + SLOT / 2.0, cy);
    let end = match p {
        Prompt::Keys(keys) => {
            let mut kx = x;
            for (n, k) in keys.iter().enumerate() {
                if n > 0 {
                    ui.push(Ui::Label((kx + 1.0, cy), "+".into(), 10.0, TEAL_HI));
                    kx += 8.0;
                }
                let w = (label_width(k, 10.0) + 6.0).max(PROMPT);
                keycap(ui, k, [kx, cy - h / 2.0, w, h]);
                kx += w;
            }
            kx
        }
        Prompt::Mouse(b) => {
            mouse(ui, [x + 2.0, cy - 8.5, 12.0, 17.0], Some(b));
            x + 14.0
        }
        Prompt::Face(pad) => {
            pad_prompt(ui, pad, mid);
            x + SLOT
        }
        Prompt::View => {
            pad_prompt(ui, Pad::Face(""), mid);
            view_glyph(ui, mid, 1.0, TEAL_HI);
            x + SLOT
        }
        Prompt::Pill(name) => {
            let w = (label_width(name, 10.0) + 6.0).max(20.0);
            keycap(ui, name, [mid.0 - w / 2.0, cy - h / 2.0 + 1.0, w, h - 2.0]);
            mid.0 + w / 2.0
        }
        Prompt::Stick(side) => {
            // The stick's cap from above: rim, a groove, and its letter.
            let r = PROMPT / 2.0 + 0.5;
            ui.push(Ui::Circle(mid, r, TEAL_HI));
            ui.push(Ui::Circle(mid, r - 1.2, INK));
            ui.push(Ui::Circle(mid, r - 2.4, TEAL));
            ui.push(Ui::Circle(mid, r - 3.2, INK));
            let w = label_width(side, 9.0);
            ui.push(Ui::Label((mid.0 - w / 2.0, mid.1), side.into(), 9.0, TEAL_HI));
            x + SLOT
        }
        Prompt::DPad(arms) => {
            dpad(ui, mid, PROMPT / 2.0 + 0.5, arms);
            x + SLOT
        }
    };
    let lx = (x + label_x[col]).max(end + 6.0);
    ui.push(Ui::Label((lx, cy), label.into(), TEXT, WHITE));
}

/// A D-pad of half-size `r` centred on `c`: the lit arms bright, the others dim.
fn dpad(ui: &mut Vec<Ui>, c: (f32, f32), r: f32, arms: Arms) {
    let t = r * 0.66;
    ui.push(Ui::RoundRect([c.0 - r, c.1 - t / 2.0, 2.0 * r, t], 1.2, DIM, DIM));
    ui.push(Ui::RoundRect([c.0 - t / 2.0, c.1 - r, t, 2.0 * r], 1.2, DIM, DIM));
    let arm = r - t / 2.0 + 0.3;
    let rects = [
        [c.0 - t / 2.0, c.1 - r, t, arm],
        [c.0 - t / 2.0, c.1 + t / 2.0 - 0.3, t, arm],
        [c.0 - r, c.1 - t / 2.0, arm, t],
        [c.0 + t / 2.0 - 0.3, c.1 - t / 2.0, arm, t],
    ];
    for (on, q) in arms.into_iter().zip(rects) {
        if on {
            ui.push(Ui::RoundRect(q, 1.0, TEAL_HI, TEAL_HI));
        }
    }
    let centre = if arms.iter().all(|&a| a) { TEAL_HI } else { DIM };
    ui.push(Ui::Rect([c.0 - t / 2.0, c.1 - t / 2.0, t, t], centre));
}

/// A thumbstick seen from above, radius `r`.
fn stick(ui: &mut Vec<Ui>, c: (f32, f32), r: f32) {
    ui.push(Ui::Circle(c, r, TEAL_HI));
    ui.push(Ui::Circle(c, r * 0.84, INK));
    ui.push(Ui::Circle(c, r * 0.68, TEAL));
    ui.push(Ui::Circle(c, r * 0.5, [0.45, 0.88, 0.78, 1.0]));
}

/// The View button's picture: two overlapping windows, `k` times its size in
/// a prompt (filled shapes only, so it fades).
fn view_glyph(ui: &mut Vec<Ui>, c: (f32, f32), k: f32, colour: [f32; 4]) {
    for (dx, dy) in [(-1.2, -1.2), (1.2, 1.2)] {
        let (w, h) = (4.4 * k, 3.4 * k);
        let (x, y) = (c.0 + dx * k - w / 2.0, c.1 + dy * k - h / 2.0);
        ui.push(Ui::Rect([x, y, w, h], colour));
        ui.push(Ui::Rect([x + 0.8 * k, y + 0.8 * k, w - 1.6 * k, h - 1.6 * k], INK));
    }
}

/// A closed curve through the points (Catmull-Rom, `n` steps per span).
fn smooth(pts: &[(f32, f32)], n: usize) -> Vec<(f32, f32)> {
    let m = pts.len();
    let mut out = Vec::with_capacity(m * n);
    for i in 0..m {
        let p0 = pts[(i + m - 1) % m];
        let p1 = pts[i];
        let p2 = pts[(i + 1) % m];
        let p3 = pts[(i + 2) % m];
        for j in 0..n {
            let t = j as f32 / n as f32;
            let (t2, t3) = (t * t, t * t * t);
            let f = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * (2.0 * b + (c - a) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (3.0 * b - a - 3.0 * c + d) * t3)
            };
            out.push((f(p0.0, p1.0, p2.0, p3.0), f(p0.1, p1.1, p2.1, p3.1)));
        }
    }
    out
}

/// The outline moved out by `d` (each point along its corner's normal, so
/// the rim is even all round, unlike a scaled copy).
fn grow(pts: &[(f32, f32)], d: f32) -> Vec<(f32, f32)> {
    let m = pts.len();
    let area: f32 = (0..m).map(|i| pts[i].0 * pts[(i + 1) % m].1 - pts[(i + 1) % m].0 * pts[i].1).sum();
    let out = if area > 0.0 { 1.0 } else { -1.0 };
    (0..m)
        .map(|i| {
            let (a, b, c) = (pts[(i + m - 1) % m], pts[i], pts[(i + 1) % m]);
            let norm = |p: (f32, f32), q: (f32, f32)| {
                let (dx, dy) = (q.0 - p.0, q.1 - p.1);
                let l = (dx * dx + dy * dy).sqrt().max(1e-4);
                (dy / l * out, -dx / l * out)
            };
            let (n1, n2) = (norm(a, b), norm(b, c));
            let (nx, ny) = (n1.0 + n2.0, n1.1 + n2.1);
            let l = (nx * nx + ny * ny).sqrt().max(1e-4);
            // Miter length, capped for sharp corners.
            let cos = (l / 2.0).max(0.5);
            (b.0 + nx / l * d / cos, b.1 + ny / l * d / cos)
        })
        .collect()
}

/// The controller's outline around its centre: the right half, mirrored.
fn controller_outline() -> Vec<(f32, f32)> {
    let right = [
        (0.0, -27.0),
        (22.0, -28.0),
        (40.0, -32.0),
        (54.0, -29.0),
        (63.0, -19.0),
        (70.0, -2.0),
        (76.0, 20.0),
        (78.0, 38.0),
        (72.0, 48.0),
        (60.0, 49.0),
        (48.0, 38.0),
        (36.0, 25.0),
        (20.0, 21.0),
    ];
    let mut pts: Vec<(f32, f32)> = right.to_vec();
    pts.push((0.0, 21.0));
    pts.extend(right[1..].iter().rev().map(|&(x, y)| (-x, y)));
    smooth(&pts, 5)
}

/// The controller: an Xbox-style drawing, and the actions.
fn controller() -> Vec<Ui> {
    let mut ui = Vec::new();
    title(&mut ui, "Controller");
    // Drawn around (0, 0), then scaled into place.
    let mut d = Vec::new();
    // Triggers (behind) and bumpers along the top.
    for side in [-1.0f32, 1.0] {
        let x = if side < 0.0 { -54.0 } else { 34.0 };
        d.push(Ui::RoundRect([x, -45.0, 20.0, 12.0], 4.0, TEAL, [0.10, 0.30, 0.27, 1.0]));
        let bumper: Vec<(f32, f32)> =
            [(26.0, -30.0), (40.0, -37.0), (55.0, -35.0), (64.0, -26.0), (60.0, -24.0), (52.0, -30.0), (40.0, -32.0), (26.0, -27.0)]
                .iter()
                .map(|&(x, y)| (x * side, y))
                .collect();
        d.push(Ui::Poly(bumper, TEAL_HI, TEAL));
    }
    let outline = controller_outline();
    d.push(Ui::Poly(grow(&outline, 1.6), TEAL_HI, TEAL));
    d.push(Ui::Poly(outline, BODY, BODY_LOW));
    // Left stick (top left) and right stick (bottom right), in their wells.
    for (x, y) in [(-40.0, -9.0), (20.0, 12.0)] {
        d.push(Ui::Circle((x, y), 11.0, WELL));
        stick(&mut d, (x, y), 8.5);
    }
    // D-pad (bottom left), in its well.
    d.push(Ui::Circle((-20.0, 12.0), 10.5, WELL));
    dpad(&mut d, (-20.0, 12.0), 8.0, [true; 4]);
    // Face buttons in their colours: Y top, B right, A bottom, X left.
    let face = [
        (40.0, -19.0, [0.95, 0.80, 0.25, 1.0]),
        (49.0, -9.0, [0.90, 0.30, 0.25, 1.0]),
        (40.0, 1.0, [0.40, 0.80, 0.30, 1.0]),
        (31.0, -9.0, [0.30, 0.55, 0.95, 1.0]),
    ];
    for (x, y, colour) in face {
        d.push(Ui::Circle((x, y), 5.4, colour));
        d.push(Ui::Circle((x, y), 4.2, INK));
        d.push(Ui::Circle((x, y), 2.4, [colour[0], colour[1], colour[2], 0.6]));
    }
    // Guide button, then View and Menu below it.
    d.push(Ui::Circle((0.0, -17.0), 6.5, TEAL_HI));
    d.push(Ui::Circle((0.0, -17.0), 5.0, [0.10, 0.30, 0.27, 1.0]));
    d.push(Ui::Circle((0.0, -17.0), 2.6, TEAL_HI));
    for (x, view) in [(-12.0, true), (12.0, false)] {
        d.push(Ui::Circle((x, -5.0), 3.6, DIM));
        d.push(Ui::Circle((x, -5.0), 2.8, INK));
        if view {
            view_glyph(&mut d, (x, -5.0), 0.55, TEAL_HI);
        } else {
            for dy in [-1.0, 0.0, 1.0] {
                d.push(Ui::Rect([x - 1.3, -5.0 + dy - 0.25, 2.6, 0.5], TEAL_HI));
            }
        }
    }
    ui.extend(d.into_iter().map(|u| u.scaled(0.92, DRAWING)));
    use Prompt::*;
    let rows: [(usize, Prompt, &str); 14] = [
        (0, Stick("L"), "Move"),
        (0, DPad([false, true, false, false]), "180 Turn"),
        (0, DPad([true, false, false, false]), "Dash (Wesker only)"),
        (0, DPad([true; 4]), "While aiming - Dodge"),
        (0, Pill("LT"), "Aim (Hold)"),
        (0, Stick("R"), "Knife (press)"),
        (0, View, "Controls"),
        (0, Face(Pad::Menu), "Pause (Single Player Only)"),
        (1, Pill("LB"), "Previous weapon"),
        (1, Pill("RB"), "Next weapon"),
        (1, Pill("RT"), "While aiming - Fire"),
        (1, Face(Pad::Face("A")), "Interact / Pick up"),
        (1, Face(Pad::Face("Y")), "Reload"),
        (1, Face(Pad::Face("X")), "Inventory"),
    ];
    let mut n = [0; 2];
    for (col, p, label) in rows {
        row(&mut ui, col, n[col], p, label, PAD_LABEL_X);
        n[col] += 1;
    }
    ui
}

/// A mouse in `r`; with `lit`, only that button lit (else both and the wheel).
fn mouse(ui: &mut Vec<Ui>, r: [f32; 4], lit: Option<Click>) {
    let [mx, my, mw, mh] = r;
    let rad = mw * 0.45;
    let split = mh * 0.4;
    ui.push(Ui::RoundRect(r, rad, TEAL_HI, TEAL));
    ui.push(Ui::RoundRect([mx + 1.0, my + 1.0, mw - 2.0, mh - 2.0], rad - 1.0, BODY, INK));
    let on = |b: Click| lit.map_or(true, |l| l == b);
    let glow = [0.33, 0.78, 0.68, 0.55];
    let half = mw / 2.0 - 2.5;
    for (b, x) in [(Click::Left, mx + 2.0), (Click::Right, mx + mw / 2.0 + 0.5)] {
        if on(b) {
            ui.push(Ui::RoundRect([x, my + 2.0, half, split - 2.5], (rad - 2.0).max(1.0), glow, [0.33, 0.78, 0.68, 0.3]));
        }
    }
    ui.push(Ui::Rect([mx + 1.0, my + split, mw - 2.0, 0.8], TEAL_HI));
    ui.push(Ui::Rect([mx + mw / 2.0 - 0.4, my + 1.0, 0.8, split], TEAL_HI));
    let ww = (mw * 0.16).max(1.8);
    let wheel = if on(Click::Wheel) { TEAL_HI } else { DIM };
    ui.push(Ui::RoundRect([mx + mw / 2.0 - ww / 2.0, my + split * 0.25, ww, split * 0.5], ww / 2.0, wheel, wheel));
}

/// The keyboard and mouse: a drawing with the keys used lit, and the actions.
fn keyboard() -> Vec<Ui> {
    let mut ui = Vec::new();
    title(&mut ui, "Keyboard and mouse");
    // Keys: (name, x, y, width, lit) from the drawing's top left.
    let (ox, oy) = (172.0, 66.0);
    let k = 17.0;
    let keys: [(&str, f32, f32, f32, bool); 13] = [
        ("Esc", 0.0, 0.0, 22.0, true),
        ("F1", 28.0, 0.0, k, true),
        ("Tab", 0.0, 22.0, 24.0, true),
        ("Q", 26.0, 22.0, k, false),
        ("W", 45.0, 22.0, k, true),
        ("E", 64.0, 22.0, k, true),
        ("R", 83.0, 22.0, k, true),
        ("A", 31.0, 41.0, k, true),
        ("S", 50.0, 41.0, k, true),
        ("D", 69.0, 41.0, k, true),
        ("F", 88.0, 41.0, k, true),
        ("Shift", 0.0, 60.0, 36.0, true),
        ("Z", 38.0, 60.0, k, false),
    ];
    for (name, x, y, w, lit) in keys {
        let r = [ox + x, oy + y, w, k];
        if lit {
            keycap(&mut ui, name, r);
        } else {
            ui.push(Ui::RoundRect(r, 2.5, DIM, DIM));
            ui.push(Ui::RoundRect([r[0] + 1.0, r[1] + 1.0, r[2] - 2.0, r[3] - 2.5], 1.8, INK, INK));
        }
    }
    // The mouse: both buttons and the wheel lit.
    mouse(&mut ui, [ox + 122.0, oy + 4.0, 32.0, 56.0], None);
    use Prompt::*;
    let rows: [(usize, Prompt, &str); 13] = [
        (0, Keys(&["WASD"]), "Move"),
        (0, Keys(&["Shift", "S"]), "180 Turn"),
        (0, Keys(&["Shift", "W"]), "Dash (Wesker only)"),
        (0, Keys(&["Shift", "WASD"]), "While aiming - Dodge"),
        (0, Keys(&["E"]), "Interact / Pick up"),
        (0, Keys(&["R"]), "Reload"),
        (0, Keys(&["F"]), "Knife"),
        (1, Mouse(Click::Right), "Aim (Toggle On/Off)"),
        (1, Mouse(Click::Left), "While aiming - Fire"),
        (1, Mouse(Click::Wheel), "Weapon swap"),
        (1, Keys(&["Tab"]), "Inventory"),
        (1, Keys(&["Esc"]), "Pause (Single Player Only)"),
        (1, Keys(&["F1"]), "Controls"),
    ];
    let mut n = [0; 2];
    for (col, p, label) in rows {
        row(&mut ui, col, n[col], p, label, KEY_LABEL_X);
        n[col] += 1;
    }
    ui
}
