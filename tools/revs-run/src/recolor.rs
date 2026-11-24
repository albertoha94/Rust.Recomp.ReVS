//! Colour variants of the characters' outfits (tools/colors.json).
//!
//! Each player's character loads its own copy of its outfit texture, so every
//! player can wear a different colour. The recipe for a character says which
//! parts of its texture are the outfit (rectangles and outlines in the
//! texture's own 512-pixel coordinates, plus colour limits); each part is
//! turned into a gray base (its shading, brought to one brightness) and then
//! coloured. The same recipe works on the game's texture and on a bigger
//! replacement. A recipe is numbers only: the pixels are the user's own.
//!
//! Which player an upload belongs to: a match creates your character first
//! (`GM_CtrlMain_Data::Create_ChPL`), then the other three
//! (`Create_ChRPC(index)`, which asks `BioVS_Protocol::GetRPCPlayerNo` for
//! the player number), and their outfit textures are uploaded afterwards in
//! the same order. The port notes each player number as it is created and
//! hands them out to the outfit uploads in turn.

use crate::Host;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

/// `GM_CtrlMain_Data::Create_ChPL()`: your character; the first of a match.
pub const CREATE_CH_PL: u32 = 0x11f054;
/// `BioVS_Protocol::GetRPCPlayerNo(ERpcIndex)`: player = TABLE[local * 3 + index].
pub const GET_RPC_PLAYER_NO: u32 = 0x75acc;
/// `BioVS_Protocol::GetEntryNameUncode(...)`: called every frame on the
/// lobbies (the players' names); only its `this` is used.
pub const GET_ENTRY_NAME: u32 = 0x77494;
pub const HOOKS: [u32; 3] = [CREATE_CH_PL, GET_RPC_PLAYER_NO, GET_ENTRY_NAME];
/// The return address of that call inside `Create_ChRPC` (it has other callers).
const FROM_CREATE_CH_RPC: u32 = 0x11f27c;
/// Your player number (0..3) in the protocol object, and the table of the others.
const LOCAL_NO: u32 = 0x11e8;
const PLAYER_TABLE: u32 = 0x31f14c;

enum Shape {
    Ellipse { cx: f64, cy: f64, rx: f64, ry: f64 },
    Poly(Vec<(f64, f64)>),
}

impl Shape {
    fn parse(v: &Value) -> Option<Shape> {
        if let Some(e) = v.get("ellipse") {
            let n: Vec<f64> = e.as_array()?.iter().filter_map(|x| x.as_f64().map(|f| f as f64)).collect();
            return Some(Shape::Ellipse { cx: *n.first()?, cy: *n.get(1)?, rx: *n.get(2)?, ry: *n.get(3)? });
        }
        let pts = v.get("poly")?.as_array()?.iter().filter_map(|p| Some((p.get(0)?.as_f64()? as f64, p.get(1)?.as_f64()? as f64))).collect();
        Some(Shape::Poly(pts))
    }

    fn inside(&self, x: f64, y: f64) -> bool {
        match self {
            Shape::Ellipse { cx, cy, rx, ry } => ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) <= 1.0,
            Shape::Poly(p) => {
                let mut c = false;
                let mut j = p.len() - 1;
                for i in 0..p.len() {
                    let ((xi, yi), (xj, yj)) = (p[i], p[j]);
                    if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                        c = !c;
                    }
                    j = i;
                }
                c
            }
        }
    }

    /// [x0, y0, x1, y1), as the recipe tool scans it.
    fn bbox(&self) -> [f64; 4] {
        match self {
            Shape::Ellipse { cx, cy, rx, ry } => [cx - rx, cy - ry, cx + rx + 1.0, cy + ry + 1.0],
            Shape::Poly(p) => {
                let f = |g: fn(f64, f64) -> f64, s: f64, k: fn(&(f64, f64)) -> f64| p.iter().map(k).fold(s, g);
                [f(f64::min, f64::MAX, |q| q.0), f(f64::min, f64::MAX, |q| q.1), f(f64::max, f64::MIN, |q| q.0) + 1.0, f(f64::max, f64::MIN, |q| q.1) + 1.0]
            }
        }
    }
}

/// A hue range in degrees; inside it a pixel counts only if it is no brighter
/// than `vmax` or no more saturated than `smax` (when given).
struct Hue {
    lo: f64,
    hi: f64,
    vmax: Option<f64>,
    smax: Option<f64>,
}

struct Part {
    regions: Vec<[f64; 4]>,
    shapes: Vec<Shape>,
    exclude: Vec<Shape>,
    hues: Vec<Hue>,
    smin: f64,
    smax: f64,
    /// Lowest alpha (0..1) of a picked pixel: keeps the transparent
    /// background of a picture out (its colour is only filled in).
    amin: f64,
    vmin: f64,
    vmax: f64,
    /// Mean brightness (0..255) of the gray base; None keeps the texture's own.
    gray_level: Option<f64>,
    /// Gray = gray_level * (lum / mean) ^ gamma (softer contrast) instead of lum * gain.
    gamma: Option<f64>,
    fill: u32,
    /// Blend of the brightness with its blur (0..1), the blur's radius.
    smooth: f64,
    blur: f64,
    sat_scale: f64,
}

#[derive(Clone, Copy)]
enum Look {
    Original,
    Gray,
    Tint { hue: f64, sat: f64, lift: f64 },
}

struct Character {
    name: String,
    hash: u64,
    start: usize,
    size: f64,
    parts: Vec<Part>,
    looks: Vec<Look>,
    /// Its picture on character select (one sheet for all the characters).
    art: Option<Art>,
}

/// A character's area on the character select sheet, coloured the same way.
struct Art {
    hash: u64,
    size: f64,
    parts: Vec<Part>,
}

struct Recipes {
    order: Vec<String>,
    swatches: Vec<[f32; 3]>,
    chars: Vec<Character>,
}

fn f(v: &Value, k: &str, d: f64) -> f64 {
    v.get(k).and_then(Value::as_f64).map_or(d, |x| x as f64)
}

fn parse(s: &str) -> Option<Recipes> {
    let v: Value = serde_json::from_str(s).ok()?;
    let order: Vec<String> = v["order"].as_array()?.iter().filter_map(|x| Some(x.as_str()?.to_string())).collect();
    let mut chars = Vec::new();
    for c in v["characters"].as_array()? {
        let rects = |p: &Value, k: &str| -> Vec<[f64; 4]> {
            p.get(k).and_then(Value::as_array).map_or(Vec::new(), |a| {
                a.iter()
                    .filter_map(|r| {
                        let n: Vec<f64> = r.as_array()?.iter().filter_map(|x| x.as_f64().map(|f| f as f64)).collect();
                        Some([*n.first()?, *n.get(1)?, *n.get(2)?, *n.get(3)?])
                    })
                    .collect()
            })
        };
        let shapes = |p: &Value, k: &str| -> Vec<Shape> { p.get(k).and_then(Value::as_array).map_or(Vec::new(), |a| a.iter().filter_map(Shape::parse).collect()) };
        let parse_parts = |a: &Vec<Value>| -> Vec<Part> {
            a.iter()
            .map(|p| Part {
                regions: rects(p, "regions"),
                shapes: shapes(p, "shapes"),
                exclude: shapes(p, "exclude"),
                hues: p.get("hues").and_then(Value::as_array).map_or_else(
                    || vec![Hue { lo: 0.0, hi: 360.0, vmax: None, smax: None }],
                    |a| {
                        a.iter()
                            .filter_map(|h| {
                                let n: Vec<f64> = h.as_array()?.iter().filter_map(|x| x.as_f64().map(|f| f as f64)).collect();
                                Some(Hue { lo: *n.first()?, hi: *n.get(1)?, vmax: n.get(2).copied(), smax: n.get(3).copied() })
                            })
                            .collect()
                    },
                ),
                smin: f(p, "smin", 0.0),
                smax: f(p, "smax", 1.0),
                amin: f(p, "amin", 0.0),
                vmin: f(p, "vmin", 0.0),
                vmax: f(p, "vmax", 1.0),
                gray_level: p.get("gray_level").and_then(Value::as_f64).map(|x| x as f64),
                gamma: p.get("gamma").and_then(Value::as_f64).map(|x| x as f64),
                fill: f(p, "fill", 2.0) as u32,
                smooth: f(p, "smooth", 0.0),
                blur: f(p, "blur", 2.0),
                sat_scale: f(p, "sat_scale", 1.0),
            })
            .collect()
        };
        let parts = parse_parts(c["parts"].as_array()?);
        let art = c.get("art").and_then(|a| {
            Some(Art { hash: u64::from_str_radix(a["hash"].as_str()?, 16).ok()?, size: f(a, "size", 512.0), parts: parse_parts(a["parts"].as_array()?) })
        });
        let looks = c["colors"]
            .as_array()?
            .iter()
            .map(|l| match l.as_str() {
                Some("gray") => Look::Gray,
                Some(_) => Look::Original,
                None => {
                    let n: Vec<f64> = l.as_array().map_or(Vec::new(), |a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f64)).collect());
                    Look::Tint { hue: n.first().copied().unwrap_or(0.0), sat: n.get(1).copied().unwrap_or(0.0), lift: n.get(2).copied().unwrap_or(1.0) }
                }
            })
            .collect();
        let start = c["start"].as_str().and_then(|s| order.iter().position(|o| o == s)).unwrap_or(0);
        chars.push(Character {
            name: c["name"].as_str()?.to_string(),
            hash: u64::from_str_radix(c["hash"].as_str()?, 16).ok()?,
            start,
            size: f(c, "size", 512.0),
            parts,
            looks,
            art,
        });
    }
    let swatches = v["swatches"]
        .as_array()
        .map_or(Vec::new(), |a| a.iter().map(|c| [0, 1, 2].map(|k| c.get(k).and_then(Value::as_f64).unwrap_or(0.5) as f32)).collect());
    Some(Recipes { order, swatches, chars })
}

fn recipes() -> &'static Recipes {
    static R: OnceLock<Recipes> = OnceLock::new();
    R.get_or_init(|| parse(include_str!("../../colors.json")).expect("tools/colors.json"))
}

/// The colour names, in the order the menus show them.
pub fn names() -> &'static [String] {
    &recipes().order
}

/// How a colour looks on a menu chip (RGB 0..1).
pub fn swatch(colour: usize) -> [f32; 3] {
    recipes().swatches.get(colour).copied().unwrap_or([0.5; 3])
}

/// Number of characters with colours (their index is the game's: the
/// lobby portraits and the character select pages go in the same order).
pub fn characters() -> usize {
    recipes().chars.len()
}

// ---- Which colour each player wears.
//
// - your own player: the colour you last chose for that character (saved in
//   the port's home folder), else the character's starting colour;
// - the others (COM): the starting colour, or, when an earlier player
//   already wears it on the same character, the next free one in the order;
// - a colour picked for a player on character select (COM players too) is
//   theirs while they keep that character.

#[derive(Default)]
struct Choices {
    /// `--colors` (tests): always this colour.
    forced: [Option<usize>; 4],
    /// Picked on character select: (character, colour).
    picked: [Option<(usize, usize)>; 4],
    /// Your colour per character, and the file it is kept in.
    saved: HashMap<usize, usize>,
    file: Option<std::path::PathBuf>,
    /// The characters in the lobby (P1..P4), as last seen.
    lobby: [Option<usize>; 4],
}

static CHOICES: Mutex<Option<Choices>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Choices) -> R) -> R {
    f(CHOICES.lock().unwrap().get_or_insert_with(Choices::default))
}

/// Reads your saved colours (`<home>/port_colors.txt`: "Chris Red" per line).
pub fn init(home: &std::path::Path) {
    let file = home.join("port_colors.txt");
    let r = recipes();
    let mut saved = HashMap::new();
    for l in std::fs::read_to_string(&file).unwrap_or_default().lines() {
        let mut it = l.split_whitespace();
        let (Some(c), Some(n)) = (it.next(), it.next()) else { continue };
        let ch = r.chars.iter().position(|x| x.name.eq_ignore_ascii_case(c));
        let col = r.order.iter().position(|x| x.eq_ignore_ascii_case(n));
        if let (Some(ch), Some(col)) = (ch, col) {
            saved.insert(ch, col);
        }
    }
    with(|s| {
        s.saved = saved;
        s.file = Some(file);
    });
}

/// `--colors=P1:Red,P3:Teal` (tests): these players always wear these colours.
pub fn parse_arg(v: &str) {
    for item in v.split(',') {
        let Some((p, c)) = item.split_once(':') else { continue };
        let p = p.trim().trim_start_matches(['P', 'p']).parse::<usize>().unwrap_or(0);
        match names().iter().position(|n| n.eq_ignore_ascii_case(c.trim())) {
            Some(i) if (1..=4).contains(&p) => with(|s| s.forced[p - 1] = Some(i)),
            _ => println!("colors: '{item}' not understood (P1..P4 : {})", names().join(", ")),
        }
    }
}

/// Your player number (0..3), read from the game's protocol object whenever
/// a hook sees it (0 until then).
static LOCAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn local_player() -> usize {
    LOCAL.load(std::sync::atomic::Ordering::Relaxed)
}

/// The characters in the lobby (P1..P4), from its portraits.
pub fn set_lobby(chars: [Option<usize>; 4]) {
    with(|s| s.lobby = chars);
}

fn colour_in(s: &Choices, slot: usize, ch: usize, local: usize, depth: u32) -> usize {
    let r = recipes();
    let n = r.order.len();
    if let Some(c) = s.forced[slot] {
        return c;
    }
    if let Some((pc, c)) = s.picked[slot] {
        if pc == ch {
            return c;
        }
    }
    let start = r.chars.get(ch).map_or(0, |c| c.start);
    if slot == local {
        return s.saved.get(&ch).copied().unwrap_or(start);
    }
    // Colours earlier players wear on the same character.
    let taken: Vec<usize> = if depth > 4 {
        Vec::new()
    } else {
        (0..slot).filter(|&j| s.lobby[j] == Some(ch)).map(|j| colour_in(s, j, ch, local, depth + 1)).collect()
    };
    let mut c = start;
    for _ in 0..n {
        if !taken.contains(&c) {
            break;
        }
        c = (c + 1) % n;
    }
    c
}

/// The colour player `slot` wears on character `ch`.
pub fn colour_of(slot: usize, ch: usize) -> usize {
    let local = local_player();
    with(|s| colour_in(s, slot.min(3), ch, local, 0))
}

/// A colour picked on character select for player `slot`; yours is also
/// saved for next time.
pub fn pick(slot: usize, ch: usize, colour: usize) {
    let local = local_player();
    let r = recipes();
    with(|s| {
        s.picked[slot.min(3)] = Some((ch, colour));
        if slot == local {
            s.saved.insert(ch, colour);
            if let Some(f) = &s.file {
                let mut text = String::new();
                for (c, col) in r.chars.iter().enumerate().filter_map(|(i, c)| Some((c, *s.saved.get(&i)?))) {
                    text += &format!("{} {}\n", c.name, r.order[col]);
                }
                if let Err(e) = std::fs::write(f, text) {
                    println!("colors: can't save {}: {e}", f.display());
                }
            }
        }
    });
    println!("colors: P{} {} -> {}", slot + 1, r.chars.get(ch).map_or("?", |c| c.name.as_str()), r.order[colour]);
}

// ---- Player numbers, as the match creates the characters.

/// Player numbers in creation order; None = your own (known once the first
/// other player is created).
static QUEUE: Mutex<VecDeque<Option<u32>>> = Mutex::new(VecDeque::new());

/// At the entry of a HOOKS function.
pub fn on_enter(h: &Host, addr: u32, r0: u32, r1: u32, lr: u32) {
    if addr == GET_RPC_PLAYER_NO || addr == GET_ENTRY_NAME {
        LOCAL.store((h.rd32(r0 + LOCAL_NO) as usize).min(3), std::sync::atomic::Ordering::Relaxed);
    }
    let mut q = QUEUE.lock().unwrap();
    match addr {
        CREATE_CH_PL => {
            // A new match: anything left from the last one is stale.
            q.clear();
            q.push_back(None);
        }
        GET_RPC_PLAYER_NO if lr == FROM_CREATE_CH_RPC => {
            let local = h.rd32(r0 + LOCAL_NO);
            for e in q.iter_mut().filter(|e| e.is_none()) {
                *e = Some(local);
            }
            q.push_back(Some(h.rd32(PLAYER_TABLE + (local * 3 + r1) * 4)));
        }
        _ => {}
    }
}

/// A character's outfit in one colour.
#[derive(Clone, Copy)]
pub struct Variant {
    ch: usize,
    look: usize,
    player: u32,
}

/// For an upload of texture `key`: the colour it gets, if it is an outfit
/// and its player wears a colour other than the original.
pub fn take(key: u64) -> Option<Variant> {
    let r = recipes();
    let ch = r.chars.iter().position(|c| c.hash == key)?;
    let player = QUEUE.lock().unwrap().pop_front().map(|p| p.unwrap_or(0))?;
    let look = colour_of(player as usize, ch);
    if matches!(r.chars[ch].looks.get(look), None | Some(Look::Original)) {
        return None;
    }
    Some(Variant { ch, look, player })
}

// ---- The recolouring.

/// One outfit pixel: index, cover (0..9 ninths), part, gray base (0..65535).
#[derive(Clone, Copy)]
struct Px {
    i: u32,
    cover: u8,
    part: u8,
    gray: u16,
}

/// Covered pixels per (character, its picture, width, height): worked out once.
static CACHE: Mutex<Option<HashMap<(usize, bool, u32, u32), std::sync::Arc<Vec<Px>>>>> = Mutex::new(None);

fn lum(px: &[u8], i: usize) -> f64 {
    0.3 * px[i * 4] as f64 + 0.59 * px[i * 4 + 1] as f64 + 0.11 * px[i * 4 + 2] as f64
}

/// HSV of a pixel: hue in degrees, saturation and value 0..1.
fn hsv(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let s = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
    if mx == mn {
        return (0.0, s, mx);
    }
    let d = mx - mn;
    let (rc, gc, bc) = ((mx - r) / d, (mx - g) / d, (mx - b) / d);
    let h = if r == mx {
        bc - gc
    } else if g == mx {
        2.0 + rc - bc
    } else {
        4.0 + gc - rc
    };
    ((h / 6.0).rem_euclid(1.0) * 360.0, s, mx)
}

/// One part's cover per pixel (0..9 ninths): the pixels its rules pick, specks
/// removed, small holes filled, edges softened.
fn part_mask(w: usize, h: usize, px: &[u8], size: f64, p: &Part) -> Vec<u8> {
    let k = w as f64 / size;
    let mut m = vec![0u8; w * h];
    let boxes = p.regions.iter().copied().chain(p.shapes.iter().map(Shape::bbox));
    for [x0, y0, x1, y1] in boxes {
        for y in (y0 * k).max(0.0) as usize..((y1 * k).min(h as f64) as usize) {
            for x in (x0 * k).max(0.0) as usize..((x1 * k).min(w as f64) as usize) {
                let (fx, fy) = (x as f64 / k, y as f64 / k);
                if !p.shapes.is_empty() && p.regions.is_empty() && !p.shapes.iter().any(|s| s.inside(fx, fy)) {
                    continue;
                }
                if p.exclude.iter().any(|s| s.inside(fx, fy)) {
                    continue;
                }
                let i = (y * w + x) * 4;
                if (px[i + 3] as f64) < p.amin * 255.0 {
                    continue;
                }
                let (hd, s, v) = hsv(px[i], px[i + 1], px[i + 2]);
                if p.vmin <= v
                    && v <= p.vmax
                    && p.smin <= s
                    && s <= p.smax
                    && p.hues.iter().any(|r| r.lo <= hd && hd <= r.hi && (r.vmax.is_none() || r.vmax.is_some_and(|m| v <= m) || r.smax.is_some_and(|m| s <= m)))
                {
                    m[y * w + x] = 1;
                }
            }
        }
    }
    let nb = |a: &[u8], x: usize, y: usize| -> u8 { (y - 1..=y + 1).map(|yy| a[yy * w + x - 1] + a[yy * w + x] + a[yy * w + x + 1]).sum() };
    // The outer pixel border keeps the picked pixels as they are.
    let mut out = m.clone();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            out[y * w + x] = (nb(&m, x, y) >= 5) as u8;
        }
    }
    for _ in 0..p.fill {
        let prev = out.clone();
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                if prev[y * w + x] == 0 && nb(&prev, x, y) >= 5 {
                    out[y * w + x] = 1;
                }
            }
        }
    }
    let mut soft = vec![0u8; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let n = nb(&out, x, y);
            if out[y * w + x] != 0 || n >= 3 {
                soft[y * w + x] = n;
            }
        }
    }
    soft
}

/// The pixels `parts` cover in an RGBA image (sheet `size` points wide), with
/// their gray base.
fn prepare(parts: &[Part], size: f64, w: usize, h: usize, px: &[u8]) -> Vec<Px> {
    let masks: Vec<Vec<u8>> = parts.iter().map(|p| part_mask(w, h, px, size, p)).collect();
    let mut gray_of = Vec::new();
    for (p, m) in parts.iter().zip(&masks) {
        let (mut sum, mut n) = (0.0f64, 0u64);
        for i in 0..w * h {
            if m[i] == 9 {
                sum += lum(px, i) as f64;
                n += 1;
            }
        }
        let mean = ((sum / n.max(1) as f64) as f64).max(1.0);
        // Its brightness, smoothed if asked (a box blur over an integral image).
        let blurred: Option<Vec<f64>> = (p.smooth > 0.0).then(|| {
            let rad = ((p.blur * w as f64 / size) as usize).max(1);
            let mut integ = vec![0.0f64; (w + 1) * (h + 1)];
            for y in 0..h {
                let mut row = 0.0;
                for x in 0..w {
                    row += lum(px, y * w + x) as f64;
                    integ[(y + 1) * (w + 1) + x + 1] = integ[y * (w + 1) + x + 1] + row;
                }
            }
            (0..w * h)
                .map(|i| {
                    if m[i] == 0 {
                        return 0.0;
                    }
                    let (y, x) = (i / w, i % w);
                    let (y0, y1, x0, x1) = (y.saturating_sub(rad), (y + rad + 1).min(h), x.saturating_sub(rad), (x + rad + 1).min(w));
                    let s = integ[y1 * (w + 1) + x1] - integ[y0 * (w + 1) + x1] - integ[y1 * (w + 1) + x0] + integ[y0 * (w + 1) + x0];
                    (s / ((y1 - y0) * (x1 - x0)) as f64) as f64
                })
                .collect()
        });
        gray_of.push((mean, blurred));
    }
    let mut out = Vec::new();
    for i in 0..w * h {
        // The part that covers it most (the later one on a tie).
        let mut best: Option<(u8, usize)> = None;
        for (j, m) in masks.iter().enumerate() {
            if m[i] > 0 && best.map_or(true, |(a, _)| m[i] >= a) {
                best = Some((m[i], j));
            }
        }
        let Some((cover, j)) = best else { continue };
        let (p, (mean, blurred)) = (&parts[j], &gray_of[j]);
        let mut l = lum(px, i);
        if let Some(b) = blurred {
            l = l * (1.0 - p.smooth) + b[i] * p.smooth;
        }
        let g = match (p.gray_level, p.gamma) {
            (Some(gl), Some(gm)) => gl * (l.max(0.0) / mean).powf(gm),
            (Some(gl), None) => l * gl / mean,
            (None, _) => l,
        };
        out.push(Px { i: i as u32, cover, part: j as u8, gray: ((g / 255.0).clamp(0.0, 1.0) * 65535.0) as u16 });
    }
    out
}

fn hls_to_rgb(h: f64, l: f64, s: f64) -> [f64; 3] {
    if s == 0.0 {
        return [l; 3];
    }
    let m2 = if l <= 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let m1 = 2.0 * l - m2;
    let v = |hue: f64| {
        let hue = hue.rem_euclid(1.0);
        if hue < 1.0 / 6.0 {
            m1 + (m2 - m1) * hue * 6.0
        } else if hue < 0.5 {
            m2
        } else if hue < 2.0 / 3.0 {
            m1 + (m2 - m1) * (2.0 / 3.0 - hue) * 6.0
        } else {
            m1
        }
    };
    [v(h + 1.0 / 3.0), v(h), v(h - 1.0 / 3.0)]
}

/// Paints `look` on the pixels `parts` cover (cached per key), in place.
fn paint(key: (usize, bool, u32, u32), parts: &[Part], size: f64, look: Look, px: &mut [u8]) {
    let (_, _, w, h) = key;
    let pixels = {
        let mut cache = CACHE.lock().unwrap();
        let cache = cache.get_or_insert_with(HashMap::new);
        cache.entry(key).or_insert_with(|| std::sync::Arc::new(prepare(parts, size, w as usize, h as usize, px))).clone()
    };
    for q in pixels.iter() {
        let l = q.gray as f64 / 65535.0;
        let col = match look {
            Look::Original => return,
            Look::Gray => [l; 3],
            Look::Tint { hue, sat, lift } => hls_to_rgb(hue / 360.0, (l * lift).min(0.92), sat * parts[q.part as usize].sat_scale),
        };
        let a = q.cover as f64 / 9.0;
        let i = q.i as usize * 4;
        for k in 0..3 {
            px[i + k] = (px[i + k] as f64 * (1.0 - a) + col[k] * 255.0 * a).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Recolours an RGBA image of the outfit (any size) in place.
pub fn apply(v: Variant, w: u32, h: u32, px: &mut [u8]) {
    let r = recipes();
    let c = &r.chars[v.ch];
    let t = std::time::Instant::now();
    paint((v.ch, false, w, h), &c.parts, c.size, c.looks[v.look], px);
    println!("colors: P{} {} in {} ({} ms)", v.player + 1, c.name, r.order[v.look], t.elapsed().as_millis());
}

/// The character select sheet (all the characters' pictures), if any
/// character has a picture recipe.
pub fn art_hash() -> Option<u64> {
    recipes().chars.iter().find_map(|c| c.art.as_ref().map(|a| a.hash))
}

/// Paints character `ch`'s picture on the character select sheet (RGBA, any
/// size) in `colour`, in place. False: nothing to paint (its own colours).
pub fn apply_art(ch: usize, colour: usize, w: u32, h: u32, px: &mut [u8]) -> bool {
    let r = recipes();
    let Some(c) = r.chars.get(ch) else { return false };
    let (Some(art), Some(&look)) = (&c.art, c.looks.get(colour)) else { return false };
    if matches!(look, Look::Original) {
        return false;
    }
    paint((ch, true, w, h), &art.parts, art.size, look, px);
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn recipes_parse() {
        let r = super::recipes();
        assert_eq!(r.order.len(), 10);
        assert_eq!(r.chars.len(), 3);
        for c in &r.chars {
            assert_eq!(c.looks.len(), 10, "{}", c.name);
            assert!(matches!(c.looks[c.start], super::Look::Original), "{} starts in its own colours", c.name);
        }
    }

    /// REVS_RECOLOR=in.png,CHARACTER,COLOUR,out.png cargo test -- --ignored:
    /// recolours a texture file (to compare with the recipe tool's output).
    #[test]
    #[ignore]
    fn recolor_file() {
        let arg = std::env::var("REVS_RECOLOR").expect("REVS_RECOLOR=in.png,CHARACTER,COLOUR,out.png");
        let a: Vec<&str> = arg.split(',').collect();
        let r = super::recipes();
        let ch = r.chars.iter().position(|c| c.name.eq_ignore_ascii_case(a[1])).unwrap();
        let look = r.order.iter().position(|o| o.eq_ignore_ascii_case(a[2])).unwrap();
        let (w, h, mut px) = crate::textures::png_read(&std::fs::read(a[0]).unwrap()).unwrap();
        super::apply(super::Variant { ch, look, player: 0 }, w, h, &mut px);
        std::fs::write(a[3], crate::textures::png_write(w, h, &px)).unwrap();
    }

    #[test]
    fn colour_space() {
        let (h, s, v) = super::hsv(255, 0, 0);
        assert!((h - 0.0).abs() < 1e-3 && (s - 1.0).abs() < 1e-6 && (v - 1.0).abs() < 1e-6);
        let c = super::hls_to_rgb(0.0, 0.5, 1.0);
        assert!((c[0] - 1.0).abs() < 1e-6 && c[1].abs() < 1e-6 && c[2].abs() < 1e-6);
    }
}
