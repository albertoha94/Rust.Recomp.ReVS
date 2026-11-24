//! The port's colour menus (the colours themselves: recolor.rs).
//!
//! - Lobby: a colour chip left of each player's portrait.
//! - Character select: a bar "◀ ● GREEN ▶" at the foot of the character's
//!   picture, above the page number. Tap or click its arrows; or move the
//!   D-pad focus onto it, then left / right change the colour (A: the next).
//!   The picture itself shows the colour too (textures.rs `show_art`).
//!
//! The bar changes the colour of the player whose lobby portrait was pressed
//! last (pressing a portrait opens character select for that player, a COM
//! one too), else yours. The screens are told apart by their own layout
//! parts: the lobby has the portraits `BtnChara1P`..`4P` (the character by
//! the portrait's picture on the sheet, 42 points per character), character
//! select has `ImgChara` (158 points per character) and `BtnPrev` / `BtnNext`.

use crate::gfx::{text_width, Ui};
use crate::lyt::{self, Btn, Kind};
use crate::recolor;
use std::sync::Mutex;

/// The bar, landscape points.
pub const BAR: [f32; 4] = [100.0, 236.0, 110.0, 19.0];
/// Its name in the D-pad focus list (input.rs).
pub const BAR_NAME: &str = "PortColor";
/// Width of one character's picture on the lobby portraits' sheet and on the
/// character select sheet (Chris, Jill, Wesker in a row).
const PORTRAIT_W: u32 = 42;
const PORTRAIT_H: u32 = 28;
const ART_W: u32 = 158;
/// The bar's frame, the game's menu green.
const TEAL: [f32; 3] = [0.33, 0.78, 0.68];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Screen {
    Other,
    Lobby,
    Select,
}

struct State {
    screen: Screen,
    /// The player character select is for (0..3).
    edit: Option<usize>,
    /// Character select: the character on the page.
    sel_ch: Option<usize>,
    /// Lobby: each player's portrait.
    portraits: [Option<[f32; 4]>; 4],
}

static STATE: Mutex<State> = Mutex::new(State { screen: Screen::Other, edit: None, sel_ch: None, portraits: [None; 4] });

/// The lobby's portraits and characters, if this is a lobby. In a Local
/// Match lobby only your own portrait is a live button; the others' are
/// hidden buttons whose pictures the game still draws (dimmed).
fn lobby(btns: &[Btn]) -> Option<([Option<[f32; 4]>; 4], [Option<usize>; 4])> {
    let portrait = |b: &Btn| b.name.starts_with("BtnChara") && b.name.ends_with('P');
    // Once everyone is ready, your own portrait is hidden too.
    let hidden = lyt::hidden_buttons();
    if !btns.iter().chain(hidden.iter()).any(portrait) {
        return None;
    }
    let mut rects = [None; 4];
    let mut chars = [None; 4];
    for (i, b) in btns.iter().chain(hidden.iter().filter(|b| portrait(b))).filter_map(|b| Some((b.name.strip_prefix("BtnChara")?.strip_suffix('P')?.parse::<usize>().ok()?, b))) {
        if (1..=4).contains(&i) && rects[i - 1].is_none() {
            rects[i - 1] = Some(b.rect);
            // The portrait among the button's pictures (frames, marks...).
            let pic = b.pics.iter().find(|p| p[2] == PORTRAIT_W && p[3] == PORTRAIT_H && p[0] % PORTRAIT_W == 0);
            chars[i - 1] = pic.map(|p| (p[0] / PORTRAIT_W) as usize).filter(|&c| c < recolor::characters());
        }
    }
    rects.iter().any(Option::is_some).then_some((rects, chars))
}

/// The character on the character select page, if this is that screen.
fn select(btns: &[Btn]) -> Option<usize> {
    let has = |n: &str| btns.iter().any(|b| b.name == n);
    if !has("BtnPrev") || !has("BtnNext") {
        return None;
    }
    let src = lyt::image("ImgChara")?;
    (src[2] == ART_W && src[0] % ART_W == 0).then(|| (src[0] / ART_W) as usize).filter(|&c| c < recolor::characters())
}

fn contains(r: [f32; 4], p: (f32, f32)) -> bool {
    p.0 >= r[0] && p.1 >= r[1] && p.0 < r[0] + r[2] && p.1 < r[1] + r[3]
}

/// Once per pass on the menus (input.rs; `menu` false during a match): works
/// out the screen and returns what to draw.
pub fn frame(btns: &[Btn], menu: bool) -> Vec<Ui> {
    let mut s = STATE.lock().unwrap();
    let mut ui = Vec::new();
    if !menu {
        s.screen = Screen::Other;
        s.edit = None;
        // SAFETY: called on the game's thread, whose GL context is current.
        unsafe { crate::textures::show_art(None) };
        return ui;
    }
    if let Some((rects, chars)) = lobby(btns) {
        s.screen = Screen::Lobby;
        // Pressable: your own portrait, and COM ones on the host (live buttons).
        s.portraits = [0, 1, 2, 3].map(|i| btns.iter().find(|b| b.name == format!("BtnChara{}P", i + 1) && b.enabled).map(|b| b.rect));
        recolor::set_lobby(chars);
        for (slot, (r, ch)) in rects.iter().zip(chars).enumerate() {
            let (Some(r), Some(ch)) = (r, ch) else { continue };
            let c = recolor::swatch(recolor::colour_of(slot, ch));
            ui.push(Ui::Disc((r[0] - 11.0, r[1] + r[3] / 2.0), 6.5, c, [0.85, 0.85, 0.85]));
        }
    } else if let Some(ch) = select(btns) {
        s.screen = Screen::Select;
        s.sel_ch = Some(ch);
        let slot = s.edit.unwrap_or_else(recolor::local_player);
        let colour = recolor::colour_of(slot, ch);
        // The character's picture in that colour.
        // SAFETY: called on the game's thread, whose GL context is current.
        unsafe { crate::textures::show_art(Some((ch, colour))) };
        let name = recolor::names()[colour].to_uppercase();
        let [x, y, w, h] = BAR;
        let cy = y + h / 2.0;
        ui.push(Ui::Rect(BAR, [0.0, 0.0, 0.0, 0.65]));
        ui.push(Ui::Frame(BAR, TEAL, 1.0));
        ui.push(Ui::Tri([(x + 5.0, cy), (x + 13.0, cy - 5.5), (x + 13.0, cy + 5.5)], TEAL));
        ui.push(Ui::Tri([(x + w - 5.0, cy), (x + w - 13.0, cy - 5.5), (x + w - 13.0, cy + 5.5)], TEAL));
        let px = 1.2;
        let tw = text_width(&name, px);
        let left = x + w / 2.0 - (12.0 + 5.0 + tw) / 2.0;
        ui.push(Ui::Disc((left + 6.0, cy), 5.0, recolor::swatch(colour), [0.85, 0.85, 0.85]));
        ui.push(Ui::Text((left + 17.0, cy), name, px, [0.95, 0.95, 0.95]));
    } else {
        s.screen = Screen::Other;
        // SAFETY: as above.
        unsafe { crate::textures::show_art(None) };
    }
    ui
}

/// The port's own buttons on this screen, for the D-pad focus.
pub fn buttons() -> Vec<Btn> {
    if STATE.lock().unwrap().screen != Screen::Select {
        return Vec::new();
    }
    vec![Btn { name: BAR_NAME.to_string(), root: "port".to_string(), kind: Kind::Button, enabled: true, rect: BAR, pics: Vec::new() }]
}

/// The colour bar: the next (+1) or previous (-1) colour.
pub fn step(d: i32) {
    let (slot, ch) = {
        let s = STATE.lock().unwrap();
        let Some(ch) = s.sel_ch.filter(|_| s.screen == Screen::Select) else { return };
        (s.edit.unwrap_or_else(recolor::local_player), ch)
    };
    let n = recolor::names().len() as i32;
    let c = (recolor::colour_of(slot, ch) as i32 + d).rem_euclid(n) as usize;
    recolor::pick(slot, ch, c);
}

/// A finger or click going down at `at` (landscape points), before the game
/// sees it. True: it is the port's (the game must not get it).
pub fn press(at: (f32, f32)) -> bool {
    let screen = {
        let mut s = STATE.lock().unwrap();
        if s.screen == Screen::Lobby {
            // A portrait: character select will be for this player.
            if let Some(i) = s.portraits.iter().position(|r| r.is_some_and(|r| contains(r, at))) {
                s.edit = Some(i);
            }
        }
        s.screen
    };
    if screen == Screen::Select && contains(BAR, at) {
        // The left arrow's third goes back; the rest (the centre too: A) on.
        step(if at.0 < BAR[0] + BAR[2] * 0.3 { -1 } else { 1 });
        return true;
    }
    false
}
