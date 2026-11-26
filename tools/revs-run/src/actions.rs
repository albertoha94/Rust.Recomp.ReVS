//! The available-actions bar: bottom left in every menu, what the controller
//! or the keyboard does there ("(A) Select  (B) Back  (View) Help  (Menu) OK"),
//! for the control scheme in use (notice.rs), in the header's style: a
//! see-through dark plate, a teal underline, teal prompts and the game's own
//! font. lyt.rs moves any game button under it up, out of its way.

use crate::gfx::{label_width, Ui};
use crate::notice::Scheme;

/// One action on the bar.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Act {
    Select,
    Back,
    /// Back on the main menu: closes the game.
    Quit,
    Help,
    Ok,
    /// Lobby: the colour of the portrait the selection box is on.
    Colour,
    /// Inventory commands (the game's BtnSel1-4); Use is Equip's button for
    /// an item that isn't a weapon.
    Equip,
    Use,
    Give,
    Combine,
    Discard,
}

impl Act {
    fn label(self) -> &'static str {
        match self {
            Act::Select => "Select",
            Act::Back => "Back",
            Act::Quit => "Quit",
            Act::Help => "Help",
            Act::Ok => "OK",
            Act::Colour => "Color",
            Act::Equip => "Equip",
            Act::Use => "Use",
            Act::Give => "Give",
            Act::Combine => "Combine",
            Act::Discard => "Discard",
        }
    }

    /// Its controller button.
    fn pad(self) -> Pad {
        match self {
            Act::Select => Pad::Face("A"),
            Act::Back | Act::Quit => Pad::Face("B"),
            Act::Help => Pad::View,
            Act::Ok => Pad::Menu,
            Act::Colour => Pad::Bumpers,
            Act::Equip | Act::Use => Pad::Face("Y"),
            Act::Give => Pad::Pill("RB"),
            Act::Combine => Pad::Face("X"),
            Act::Discard => Pad::Pill("LB"),
        }
    }

    /// Its keys (several: either one, shown "Q / E"; none: not on the keyboard's bar).
    fn keys(self) -> &'static [&'static str] {
        match self {
            Act::Select => &["Enter"],
            Act::Back | Act::Quit => &["Esc"],
            Act::Help => &["F1"],
            // No key: Enter on the selected OK does it.
            Act::Ok => &[],
            Act::Colour => &["Q", "E"],
            Act::Equip | Act::Use => &["1"],
            Act::Give => &["2"],
            Act::Combine => &["3"],
            Act::Discard => &["4"],
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Pad {
    /// A round face button with its letter.
    Face(&'static str),
    /// View (two overlapping windows).
    View,
    /// Menu (three lines).
    Menu,
    /// LB and RB, two small pills.
    Bumpers,
    /// One of them.
    Pill(&'static str),
}

/// Bumper pill width (points) and the space between the two.
const PILL_W: f32 = 17.0;
const PILL_GAP: f32 = 2.0;

fn pad_w(p: Pad) -> f32 {
    match p {
        Pad::Bumpers => 2.0 * PILL_W + PILL_GAP,
        Pad::Pill(_) => PILL_W,
        _ => PROMPT,
    }
}

const TEAL: [f32; 3] = [0.33, 0.78, 0.68];
const TEAL_HI: [f32; 3] = [0.66, 0.97, 0.88];
const INK: [f32; 4] = [0.02, 0.07, 0.07, 1.0];
const WHITE: [f32; 4] = [0.93, 0.93, 0.89, 1.0];

/// Bar height, margin from the screen's edge, text size, prompt size (points).
pub const H: f32 = 18.0;
const MARGIN: f32 = 4.0;
const TEXT: f32 = 13.0;
const KEY_TEXT: f32 = 10.0;
const FACE_TEXT: f32 = 12.0;
pub(crate) const PROMPT: f32 = 13.0;
/// Space inside the plate's ends, between a prompt and its label, between actions.
const PAD_X: f32 = 6.0;
const GAP_IN: f32 = 3.0;
const GAP: f32 = 9.0;

fn rgba(c: [f32; 3], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}

/// Width of a prompt.
fn prompt_w(scheme: Scheme, a: Act) -> f32 {
    match scheme {
        Scheme::Controller => pad_w(a.pad()),
        Scheme::Keyboard => {
            let keys = a.keys();
            keys.iter().map(|k| key_w(k)).sum::<f32>() + (keys.len() as f32 - 1.0) * 8.0
        }
    }
}

fn key_w(k: &str) -> f32 {
    (label_width(k, KEY_TEXT) + 6.0).max(PROMPT)
}

/// The actions the scheme has a button for.
fn shown(scheme: Scheme, acts: &[Act]) -> Vec<Act> {
    acts.iter().copied().filter(|a| scheme == Scheme::Controller || !a.keys().is_empty()).collect()
}

/// The bar's rectangle (x, y, w, h) for these actions.
pub fn rect(scheme: Scheme, acts: &[Act]) -> Option<[f32; 4]> {
    let acts = &shown(scheme, acts)[..];
    if acts.is_empty() {
        return None;
    }
    let w: f32 = acts.iter().map(|&a| prompt_w(scheme, a) + GAP_IN + label_width(a.label(), TEXT)).sum::<f32>()
        + GAP * (acts.len() as f32 - 1.0)
        + 2.0 * PAD_X;
    Some([MARGIN, 320.0 - MARGIN - H, w, H])
}

/// The bar to draw.
pub fn frame(scheme: Scheme, acts: &[Act]) -> Vec<Ui> {
    let Some(r) = rect(scheme, acts) else { return Vec::new() };
    let acts = &shown(scheme, acts)[..];
    let mut ui = vec![
        Ui::Rect(r, [0.02, 0.07, 0.07, 0.5]),
        Ui::Rect([r[0], r[1] + r[3] - 1.0, r[2], 1.0], rgba(TEAL_HI, 1.0)),
    ];
    let cy = r[1] + (r[3] - 1.0) / 2.0;
    let mut x = r[0] + PAD_X;
    for &a in acts {
        match scheme {
            Scheme::Controller => match a.pad() {
                Pad::Bumpers => {
                    for (i, name) in ["LB", "RB"].into_iter().enumerate() {
                        keycap(&mut ui, name, [x + i as f32 * (PILL_W + PILL_GAP), cy - PROMPT / 2.0 + 1.0, PILL_W, PROMPT - 2.0]);
                    }
                }
                Pad::Pill(name) => keycap(&mut ui, name, [x, cy - PROMPT / 2.0 + 1.0, PILL_W, PROMPT - 2.0]),
                p => pad_prompt(&mut ui, p, (x + PROMPT / 2.0, cy)),
            },
            Scheme::Keyboard => {
                let mut kx = x;
                for (i, k) in a.keys().iter().enumerate() {
                    if i > 0 {
                        ui.push(Ui::Label((kx + 2.0, cy), "/".into(), KEY_TEXT, rgba(TEAL_HI, 1.0)));
                        kx += 8.0;
                    }
                    let w = key_w(k);
                    keycap(&mut ui, k, [kx, cy - PROMPT / 2.0, w, PROMPT]);
                    kx += w;
                }
            }
        }
        x += prompt_w(scheme, a) + GAP_IN;
        ui.push(Ui::Label((x, cy), a.label().into(), TEXT, WHITE));
        x += label_width(a.label(), TEXT) + GAP;
    }
    ui
}

/// A controller button: a teal disc with a dark centre and its symbol.
pub(crate) fn pad_prompt(ui: &mut Vec<Ui>, p: Pad, c: (f32, f32)) {
    let r = PROMPT / 2.0;
    ui.push(Ui::Circle(c, r, rgba(TEAL_HI, 1.0)));
    ui.push(Ui::Circle(c, r - 1.2, INK));
    let hi = rgba(TEAL_HI, 1.0);
    match p {
        Pad::Face(letter) => {
            let w = label_width(letter, FACE_TEXT);
            ui.push(Ui::Label((c.0 - w / 2.0, c.1), letter.into(), FACE_TEXT, hi));
        }
        Pad::View => {
            // Two overlapping window outlines.
            for (dx, dy) in [(-1.3, -1.3), (1.3, 1.3)] {
                let f = [c.0 + dx - 2.0, c.1 + dy - 1.6, 4.0, 3.2];
                ui.push(Ui::Rect(f, INK));
                ui.push(Ui::Frame(f, TEAL_HI, 0.8));
            }
        }
        Pad::Bumpers | Pad::Pill(_) => {}
        Pad::Menu => {
            for dy in [-1.8, 0.0, 1.8] {
                ui.push(Ui::Rect([c.0 - 2.6, c.1 + dy - 0.45, 5.2, 0.9], hi));
            }
        }
    }
}

/// A key: a teal keycap with its name.
pub(crate) fn keycap(ui: &mut Vec<Ui>, name: &str, r: [f32; 4]) {
    ui.push(Ui::RoundRect(r, 2.5, rgba(TEAL_HI, 1.0), rgba(TEAL, 1.0)));
    ui.push(Ui::RoundRect([r[0] + 1.0, r[1] + 1.0, r[2] - 2.0, r[3] - 2.5], 1.8, [0.05, 0.15, 0.14, 1.0], INK));
    let w = label_width(name, KEY_TEXT);
    ui.push(Ui::Label((r[0] + (r[2] - w) / 2.0, r[1] + r[3] / 2.0 - 0.5), name.into(), KEY_TEXT, rgba(TEAL_HI, 1.0)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_fits_and_grows() {
        let one = rect(Scheme::Controller, &[Act::Select]).unwrap();
        let four = rect(Scheme::Controller, &[Act::Select, Act::Back, Act::Help, Act::Ok]).unwrap();
        assert!(four[2] > one[2]);
        assert_eq!(one[1] + one[3], 320.0 - MARGIN);
        assert!(rect(Scheme::Keyboard, &[]).is_none());
        assert!(frame(Scheme::Keyboard, &[Act::Ok]).is_empty());
        assert!(!frame(Scheme::Controller, &[Act::Ok]).is_empty());
    }
}

/// One key or button prompt in the bar's style, `size` points tall,
/// centred on `c` (the interact prompt under the match's message window,
/// lyt.rs `event_icon`): drawn at the bar's size and scaled up.
pub fn prompt(scheme: Scheme, key: &str, face: &'static str, c: (f32, f32), size: f32) -> Vec<Ui> {
    let mut ui = Vec::new();
    match scheme {
        Scheme::Controller => pad_prompt(&mut ui, Pad::Face(face), (0.0, 0.0)),
        Scheme::Keyboard => {
            let w = key_w(key);
            keycap(&mut ui, key, [-w / 2.0, -PROMPT / 2.0, w, PROMPT]);
        }
    }
    let k = size / PROMPT;
    ui.into_iter().map(|u| u.scaled(k, c)).collect()
}
