//! The port's colour menus (the colours themselves: recolor.rs).
//!
//! - Lobby: a colour ball on the top right corner of each player's portrait;
//!   LB / RB (Q / E) change the colour of the portrait the selection box is on.
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

use crate::gfx::{label_width, Ui};
use crate::lyt::{self, Btn, Kind};
use crate::recolor;
use crate::Host;
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
/// The bar's frame, the game's menu green, and its bright edge.
const TEAL: [f32; 3] = [0.33, 0.78, 0.68];
const TEAL_HI: [f32; 3] = [0.66, 0.97, 0.88];
/// Lobby ball radius (points).
const BALL_R: f32 = 5.5;

/// A colour ball: a dark outline, a teal rim like the game's frames, and the
/// colour with a soft light from the top left (dark at the foot, a highlight).
pub fn ball(c: (f32, f32), r: f32, rgb: [f32; 3]) -> Vec<Ui> {
    let k = |f: f32, a: f32| [rgb[0] * f, rgb[1] * f, rgb[2] * f, a];
    let lift = |f: f32| [rgb[0] + (1.0 - rgb[0]) * f, rgb[1] + (1.0 - rgb[1]) * f, rgb[2] + (1.0 - rgb[2]) * f, 1.0];
    let at = |dx: f32, dy: f32| (c.0 + dx * r, c.1 + dy * r);
    vec![
        Ui::Circle(c, r + 1.3, [0.02, 0.07, 0.07, 0.9]),
        Ui::Circle(c, r + 0.5, [TEAL_HI[0], TEAL_HI[1], TEAL_HI[2], 1.0]),
        Ui::Circle(c, r - 0.4, [TEAL[0] * 0.5, TEAL[1] * 0.5, TEAL[2] * 0.5, 1.0]),
        Ui::Circle(c, r - 0.9, k(0.45, 1.0)),
        Ui::Circle(at(-0.08, -0.1), r * 0.78, k(0.8, 1.0)),
        Ui::Circle(at(-0.15, -0.2), r * 0.55, k(1.0, 1.0)),
        Ui::Circle(at(-0.25, -0.32), r * 0.3, lift(0.45)),
        Ui::Circle(at(-0.3, -0.38), r * 0.13, [1.0, 1.0, 1.0, 0.8]),
    ]
}

/// The lobby slot (0..3) of a portrait button `BtnChara1P`..`4P`.
pub fn portrait_slot(name: &str) -> Option<usize> {
    let n = name.strip_prefix("BtnChara")?.strip_suffix('P')?.parse::<usize>().ok()?;
    (1..=4).contains(&n).then(|| n - 1)
}

/// Lobby: the next (+1) or previous (-1) colour for player `slot`'s character.
pub fn lobby_step(slot: usize, d: i32) {
    let Some(ch) = STATE.lock().unwrap().chars.get(slot).copied().flatten() else { return };
    let n = recolor::names().len() as i32;
    let c = (recolor::colour_of(slot, ch) as i32 + d).rem_euclid(n) as usize;
    recolor::pick(slot, ch, c);
}

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
    /// Lobby: each player's portrait, and character.
    portraits: [Option<[f32; 4]>; 4],
    chars: [Option<usize>; 4],
}

static STATE: Mutex<State> = Mutex::new(State { screen: Screen::Other, edit: None, sel_ch: None, portraits: [None; 4], chars: [None; 4] });

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
            // The portrait among the button's pictures (frames, marks...):
            // the ball goes on its corner (the button is taller).
            let pic = b.pics.iter().find(|(p, _)| p[2] == PORTRAIT_W && p[3] == PORTRAIT_H && p[0] % PORTRAIT_W == 0);
            rects[i - 1] = Some(pic.map_or(b.rect, |p| p.1));
            chars[i - 1] = pic.map(|(p, _)| (p[0] / PORTRAIT_W) as usize).filter(|&c| c < recolor::characters());
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
pub fn frame(h: &Host, btns: &[Btn], menu: bool) -> Vec<Ui> {
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
        s.chars = chars;
        for (slot, (r, ch)) in rects.iter().zip(chars).enumerate() {
            let (Some(r), Some(ch)) = (r, ch) else { continue };
            let c = recolor::swatch(recolor::colour_of(slot, ch));
            ui.extend(ball((r[0] + r[2] - 2.0, r[1] + 2.0), BALL_R, c));
        }
    } else if let Some(balls) = icon_balls(h, s.chars) {
        s.screen = Screen::Other;
        ui.extend(balls);
    } else if let Some(ch) = select(btns) {
        s.screen = Screen::Select;
        s.sel_ch = Some(ch);
        let slot = s.edit.unwrap_or_else(recolor::local_player);
        let colour = recolor::colour_of(slot, ch);
        // The character's picture in that colour.
        // SAFETY: called on the game's thread, whose GL context is current.
        unsafe { crate::textures::show_art(Some((ch, colour))) };
        let name = recolor::names()[colour].clone();
        let [x, y, w, h] = BAR;
        let cy = y + h / 2.0;
        ui.push(Ui::Rect(BAR, [0.0, 0.0, 0.0, 0.65]));
        ui.push(Ui::Frame(BAR, TEAL, 1.0));
        ui.push(Ui::Tri([(x + 5.0, cy), (x + 13.0, cy - 5.5), (x + 13.0, cy + 5.5)], TEAL));
        ui.push(Ui::Tri([(x + w - 5.0, cy), (x + w - 13.0, cy - 5.5), (x + w - 13.0, cy + 5.5)], TEAL));
        let size = 13.0;
        let tw = label_width(&name, size);
        let left = x + w / 2.0 - (12.0 + 5.0 + tw) / 2.0;
        ui.extend(ball((left + 6.0, cy), 4.5, recolor::swatch(colour)));
        ui.push(Ui::Label((left + 17.0, cy), name, size, [0.93, 0.93, 0.89, 1.0]));
    } else {
        s.screen = Screen::Other;
        // SAFETY: as above.
        unsafe { crate::textures::show_art(None) };
    }
    ui
}

/// Character portraits in rows (`ImgIconChara1`..`5`, 40 x 26, the
/// character by the picture, 40 points each): a ball on each.
/// - Match results (Total Results, Player Kills...): each row's "P1".."P4"
///   picture (`ImgP<row>`, 23 x 20, at x 213 + 23 per player on the sheet)
///   tells whose row it is: that player's colour.
/// - Pages without P pictures (Player Kills: the names are text): the
///   lobby's players (`lobby`: each one's character, kept from the lobby) in order (all, or all but you), where the portrait is
///   that player's character.
/// - Records (Coin Shoot's stage pages, 1st to 5th; every portrait matches
///   the records table): the colour noted when the record was set (records.rs).
fn icon_balls(h: &Host, lobby: [Option<usize>; 4]) -> Option<Vec<Ui>> {
    let imgs = lyt::images();
    let row = |name: &str, prefix: &str| name.strip_prefix(prefix).and_then(|n| n.parse::<usize>().ok()).filter(|n| (1..=5).contains(n));
    let mut icons: Vec<(usize, usize, [f32; 4])> = imgs
        .iter()
        .filter_map(|i| {
            let ch = (i.src[2] == ICON_W && i.src[3] == ICON_H).then(|| (i.src[0] / ICON_W) as usize).filter(|&c| c < recolor::characters())?;
            Some((row(&i.name, "ImgIconChara")?, ch, i.rect))
        })
        .collect();
    // (A layout can be drawn more than once a frame.)
    icons.sort_by_key(|e| e.0);
    icons.dedup_by_key(|e| e.0);
    if icons.is_empty() {
        return None;
    }
    let mut players = [None; 6];
    for i in &imgs {
        if let Some(r) = row(&i.name, "ImgP") {
            if i.src[1] == P_Y && i.src[2] == P_W && i.src[0] >= P_X && (i.src[0] - P_X) % P_W == 0 && (i.src[0] - P_X) / P_W < 4 {
                players[r] = Some(((i.src[0] - P_X) / P_W) as usize);
            }
        }
    }
    // P pictures count only with one per row (another page's layout can
    // leave one drawn).
    let rows: Vec<usize> = icons.iter().map(|e| e.0).collect();
    let pictured = rows.iter().all(|&r| players[r].is_some()) && players.iter().filter(|p| p.is_some()).count() == rows.len();
    let records = crate::records::rows(h);
    let is_record = |r: usize, ch: usize| records.get(r - 1).copied().flatten().filter(|&(c, _)| c == ch);
    let record_page = !pictured && !records.is_empty() && icons.iter().all(|&(r, ch, _)| is_record(r, ch).is_some());
    if !pictured && !record_page {
        // No P pictures (Player Kills...: the names are text): the rows are
        // the lobby's players in order, everyone or everyone but you. A row
        // only counts if its portrait is that player's character.
        let all: Vec<usize> = (0..4).filter(|&i| lobby[i].is_some()).collect();
        let others: Vec<usize> = all.iter().copied().filter(|&i| i != recolor::local_player()).collect();
        let order = if rows.len() == others.len() { others } else if rows.len() == all.len() { all } else { Vec::new() };
        players = [None; 6];
        for (&r, &slot) in rows.iter().zip(&order) {
            players[r] = Some(slot);
        }
        for &(r, ch, _) in &icons {
            if players[r].is_some_and(|slot| lobby[slot] != Some(ch)) {
                players[r] = None;
            }
        }
    }
    let mut ui = Vec::new();
    for (r, ch, rect) in icons {
        let colour = if record_page { is_record(r, ch).map(|(_, colour)| colour) } else { players[r].map(|slot| recolor::colour_of(slot, ch)) };
        if let Some(colour) = colour {
            ui.extend(ball((rect[0] + rect[2] - 2.0, rect[1] + 2.0), BALL_R, recolor::swatch(colour)));
        }
    }
    Some(ui)
}

/// A portrait in the result and record rows, and the "P1".."P4" pictures
/// (sheet x of P1, their y and width).
const ICON_W: u32 = 40;
const ICON_H: u32 = 26;
const P_X: u32 = 213;
const P_Y: u32 = 50;
const P_W: u32 = 23;

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
