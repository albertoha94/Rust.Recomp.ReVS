//! The game's own on-screen buttons, read live from its layout objects (the
//! LYT_ classes), for controller focus and the Backtrack back button.
//!
//! Every screen is a tree of layout parts (LYT_CmpProduct). The root of each
//! tree goes through `LYT_CmpProduct::Draw` (0x3456c) every frame; children
//! are drawn inside it. The run loop marks that function so `on_draw` runs
//! just before each root is drawn: it walks the tree and
//! - lists the buttons that are on screen (visible with all their parents)
//!   with their landscape rectangle, name and enabled flag;
//! - hides the pictures of every `BtnBack` (the ↺ back button): the button
//!   itself stays where it is and still works, so the controller's B presses
//!   it, but it is not drawn (touchHLE v17 did this in the texture).
//!
//! A part (layout notes from touchHLE v14 and the setters here):
//!   +0x08 its layout node; node +0x30 = name (C string)
//!   +0x0c children array, +0x18 child count
//!   +0x3c x, y, w, h on screen (landscape points, i32)
//!   +0x50 enabled, +0x51 visible (`Set_State` 0x35ad0 writes both)
//! Button classes are told apart by vtable slot 12, their own `doMsgProc`
//! (Button 0x42b4c, PushButton 0x59f64, ActionButton 0x470bc, and
//! SliderButton 0x45f14: the slide-to-confirm bar `SlBtnYes`).

use crate::Host;
use std::collections::HashMap;
use std::sync::Mutex;

/// `LYT_CmpProduct::Draw`.
pub const DRAW: u32 = 0x3456c;

/// Menu screens changed by the port (touchHLE patches v11-v14 rebuilt).
/// Their per-frame update or render gets (this, context); the screen's
/// layouts are the array at `[context + 0x7c]` (Bio4_ResourceMgr::Get_Layout).
/// The hook only notes the root; `on_draw` changes it.
/// `BioVS_Hdl_Ttl_TMenu` update: the title menu (BtnMovie: the video button).
pub const TMENU_UPDATE: u32 = 0x90fcc;
/// `BioVS_Hdl_OptTop` update: Options (BtnSel3 "Get More", BtnSel4 "Language").
pub const OPT_UPDATE: u32 = 0x1cebd0;
/// `Bio4_CTTask_ControllerSettingB::Render`: Controls > Other Settings
/// (layout 1; "Icon Layout" has the only Dec / Inc arrows there).
pub const CTRLB_RENDER: u32 = 0x10b50;
pub const SCREEN_HOOKS: [u32; 3] = [TMENU_UPDATE, OPT_UPDATE, CTRLB_RENDER];

/// Roots of the title menu, Options and Other Settings while they run.
static SCREENS: Mutex<[u32; 3]> = Mutex::new([0; 3]);

/// Called at the entry of a SCREEN_HOOKS function with its context (r1).
pub fn on_update(h: &Host, addr: u32, ctx: u32) {
    let Some(i) = SCREEN_HOOKS.iter().position(|&a| a == addr) else { return };
    let layout = if addr == CTRLB_RENDER { 1 } else { 0 };
    let p = h.rd32(ctx + 0x7c);
    SCREENS.lock().unwrap()[i] = if p == 0 { 0 } else { h.rd32(p + 4 * layout) };
}

/// Every part called `name` under `o`.
fn find_all(h: &Host, o: u32, name: &str, depth: u32, out: &mut Vec<u32>) {
    if o == 0 || depth > 16 {
        return;
    }
    if name_of(h, o) == name {
        out.push(o);
    }
    let (n, kids) = (h.rd32(o + 0x18), h.rd32(o + 0xc));
    if kids != 0 && n < 256 {
        for i in 0..n {
            find_all(h, h.rd32(kids + 4 * i), name, depth + 1, out);
        }
    }
}

/// The part called `name` under `o` (depth first).
fn find(h: &Host, o: u32, name: &str, depth: u32) -> Option<u32> {
    if o == 0 || depth > 16 {
        return None;
    }
    if name_of(h, o) == name {
        return Some(o);
    }
    let (n, kids) = (h.rd32(o + 0x18), h.rd32(o + 0xc));
    if kids == 0 || n >= 256 {
        return None;
    }
    (0..n).find_map(|i| find(h, h.rd32(kids + 4 * i), name, depth + 1))
}

/// Removes a part: not drawn, not active, and (for a button) its two touch
/// rectangles off screen, so no touch, click or controller press reaches it.
fn remove(h: &Host, o: u32) {
    h.wr8(o + 0x50, 0);
    h.wr8(o + 0x51, 0);
    if kind_of_vt(h, o).is_some() {
        h.wr32(o + 0xb0, OFFSCREEN);
        h.wr32(o + 0xc4, OFFSCREEN);
    }
}
const OFFSCREEN: u32 = 0x4000_0000;

/// Moves a part and everything in it down by `dy` points (y fields of the
/// layout, the screen rectangle and the drawn position).
fn shift(h: &Host, o: u32, dy: i32, depth: u32) {
    if o == 0 || depth > 16 {
        return;
    }
    for off in [0x34, 0x40, 0x5c, 0x68] {
        h.wr32(o + off, (h.rd32(o + off) as i32 + dy) as u32);
    }
    let (n, kids) = (h.rd32(o + 0x18), h.rd32(o + 0xc));
    if kids != 0 && n < 256 {
        for i in 0..n {
            shift(h, h.rd32(kids + 4 * i), dy, depth + 1);
        }
    }
}

/// The first part under `o` whose image is the sheet area `src` (x, y, w, h
/// at +0x98..+0xa4).
fn find_src(h: &Host, o: u32, src: [u32; 4], depth: u32) -> Option<u32> {
    if o == 0 || depth > 16 {
        return None;
    }
    if [0x98, 0x9c, 0xa0, 0xa4].map(|k| h.rd32(o + k)) == src {
        return Some(o);
    }
    let (n, kids) = (h.rd32(o + 0x18), h.rd32(o + 0xc));
    if kids == 0 || n >= 256 {
        return None;
    }
    (0..n).find_map(|i| find_src(h, h.rd32(kids + 4 * i), src, depth + 1))
}

/// Title screen: the "(c) CAPCOM 2011" part shows the longer line the port
/// put on the sheet ("(c) CAPCOM 2011 . Recomp by albertoha94", textures.rs),
/// as wide as that line and centred. Part fields: x at +0x30 / +0x3c / +0x58
/// / +0x64, width at +0x44 / +0x6c, image area at +0x98..+0xa4.
fn title_credit(h: &Host, root: u32) {
    let line_w = crate::textures::title_line_w();
    if root == 0 || line_w == 0 || find(h, root, "ImgTitle", 0).is_none() {
        return;
    }
    let Some(o) = find_src(h, root, crate::textures::COPYRIGHT_SRC, 0) else { return };
    let (ax, ay) = crate::textures::TITLE_LINE_AT;
    for (k, v) in [(0x98, ax), (0x9c, ay), (0xa0, line_w)] {
        h.wr32(o + k, v);
    }
    let x = (480 - line_w.min(480)) / 2;
    for k in [0x30, 0x3c, 0x58, 0x64] {
        h.wr32(o + k, x);
    }
    for k in [0x44, 0x6c] {
        h.wr32(o + k, line_w);
    }
}

/// The port's changes to the menu screens, made just before each draw:
/// - title menu: the video button (and its animated overlay) removed;
/// - Options: "Get More" removed and "Language" moved up into its place;
/// - Other Settings: the "Icon Layout" arrows removed, so the on-screen
///   button layout the controller mapping uses stays as saved.
fn edit_screens(h: &Host, root: u32) {
    title_credit(h, root);
    let [title, opt, ctrlb] = *SCREENS.lock().unwrap();
    if root != 0 && root == ctrlb {
        let mut arrows = Vec::new();
        find_all(h, root, "Dec", 0, &mut arrows);
        find_all(h, root, "Inc", 0, &mut arrows);
        for o in arrows {
            remove(h, o);
        }
    }
    if root != 0 && root == title {
        for name in ["BtnMovie", "ImgAnimBtn"] {
            if let Some(o) = find(h, root, name, 0) {
                remove(h, o);
            }
        }
    }
    if root != 0 && root == opt {
        let (Some(get_more), Some(lang)) = (find(h, root, "BtnSel3", 0), find(h, root, "BtnSel4", 0)) else { return };
        remove(h, get_more);
        let dy = h.rd32(get_more + 0x68) as i32 - h.rd32(lang + 0x68) as i32;
        if dy != 0 {
            for off in [0xb4, 0xc8] {
                h.wr32(lang + off, (h.rd32(lang + off) as i32 + dy) as u32);
            }
            shift(h, lang, dy, 0);
        }
    }
}

fn kind_of_vt(h: &Host, o: u32) -> Option<Kind> {
    let vt = h.rd32(o);
    if vt == 0 || vt >= 0x40_0000 {
        return None;
    }
    let f = h.rd32(vt + 4 * MSGPROC_SLOT) & !1;
    MARKS.iter().find(|e| e.0 == f).map(|e| e.1)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Button,
    Action,
    Slider,
}

#[derive(Clone, Debug)]
pub struct Btn {
    pub name: String,
    /// The root layout it belongs to (e.g. "Sub_lytAC", the inventory).
    pub root: String,
    pub kind: Kind,
    pub enabled: bool,
    /// x, y, w, h in landscape game points.
    pub rect: [f32; 4],
}

impl Btn {
    pub fn centre(&self) -> (f32, f32) {
        (self.rect[0] + self.rect[2] / 2.0, self.rect[1] + self.rect[3] / 2.0)
    }
    pub fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.rect[0] && p.1 >= self.rect[1] && p.0 < self.rect[0] + self.rect[2] && p.1 < self.rect[1] + self.rect[3]
    }
}

#[derive(Default)]
struct Lists {
    /// Presented frame the `current` list is being filled for.
    frame: u64,
    current: Vec<Btn>,
    /// The last complete frame's buttons.
    last: Vec<Btn>,
    /// Names of the visible root layouts drawn, this frame and the last one.
    current_roots: Vec<String>,
    last_roots: Vec<String>,
    /// Class of each vtable seen (None: not a button).
    kinds: HashMap<u32, Option<Kind>>,
}

static LISTS: Mutex<Option<Lists>> = Mutex::new(None);

/// Each button class's own `doMsgProc` (vtable slot 12 of every LYT part).
const MARKS: [(u32, Kind); 4] =
    [(0x42b4c, Kind::Button), (0x59f64, Kind::Button), (0x470bc, Kind::Action), (0x45f14, Kind::Slider)];
const MSGPROC_SLOT: u32 = 12;

fn name_of(h: &Host, o: u32) -> String {
    let node = h.rd32(o + 8);
    if node == 0 {
        return String::new();
    }
    let p = h.rd32(node + 0x30);
    if p == 0 { String::new() } else { h.cstr(p) }
}

fn kind_of(h: &Host, l: &mut Lists, o: u32) -> Option<Kind> {
    let vt = h.rd32(o);
    *l.kinds.entry(vt).or_insert_with(|| {
        if vt == 0 || vt >= 0x40_0000 {
            return None;
        }
        let f = h.rd32(vt + 4 * MSGPROC_SLOT) & !1;
        MARKS.iter().find(|e| e.0 == f).map(|e| e.1)
    })
}

fn rect_of(h: &Host, o: u32) -> [f32; 4] {
    [0, 4, 8, 12].map(|k| h.rd32(o + 0x3c + k) as i32 as f32)
}

/// REVS_LYT=2: the whole tree of a root, every 30 frames (names, flags, rects).
fn dump(h: &Host, o: u32, depth: u32) {
    if o == 0 || depth > 16 {
        return;
    }
    println!(
        "  {:indent$}{:?} vis={} en={} rect={:?} hit={:?}",
        "",
        name_of(h, o),
        h.rd8(o + 0x51),
        h.rd8(o + 0x50),
        rect_of(h, o),
        [0, 4, 8, 12].map(|k| h.rd32(o + 0xb0 + k) as i32),
        indent = 2 * depth as usize
    );
    let (n, kids) = (h.rd32(o + 0x18), h.rd32(o + 0xc));
    if kids != 0 && n < 256 {
        for i in 0..n {
            dump(h, h.rd32(kids + 4 * i), depth + 1);
        }
    }
}

fn walk(h: &Host, l: &mut Lists, o: u32, root: &str, depth: u32) {
    if o == 0 || depth > 16 || h.rd8(o + 0x51) == 0 {
        return;
    }
    let name = name_of(h, o);
    let n = h.rd32(o + 0x18);
    let kids = h.rd32(o + 0xc);
    if let Some(kind) = kind_of(h, l, o) {
        if name == "BtnBack" {
            // Backtrack: the button stays, its pictures are not drawn.
            if kids != 0 && n < 64 {
                for i in 0..n {
                    let k = h.rd32(kids + 4 * i);
                    if k != 0 {
                        h.wr8(k + 0x51, 0);
                    }
                }
            }
        }
        let enabled = h.rd8(o + 0x50) != 0;
        l.current.push(Btn { name, root: root.to_string(), kind, enabled, rect: rect_of(h, o) });
        // A button's children are its own pictures and texts.
        return;
    }
    if kids != 0 && n < 256 {
        for i in 0..n {
            let k = h.rd32(kids + 4 * i);
            walk(h, l, k, root, depth + 1);
        }
    }
}

/// Called just before `LYT_CmpProduct::Draw(this)` runs.
pub fn on_draw(h: &Host, this: u32) {
    let frame = h.gfx.as_ref().map_or(0, |g| g.frames);
    let mut guard = LISTS.lock().unwrap();
    let l = guard.get_or_insert_with(Lists::default);
    if frame != l.frame {
        l.last = std::mem::take(&mut l.current);
        l.last_roots = std::mem::take(&mut l.current_roots);
        l.frame = frame;
        // REVS_LYT=1: print the buttons on screen every 30 frames.
        if frame % 30 == 0 && std::env::var_os("REVS_LYT").is_some() {
            println!("lyt f{frame}: {} buttons", l.last.len());
            for b in &l.last {
                println!("  {:<12} {:<14} {:?} enabled={} {:?}", b.name, b.root, b.kind, b.enabled, b.rect);
            }
        }
    }
    edit_screens(h, this);
    let root = name_of(h, this);
    if std::env::var_os("REVS_LYT").is_some() && frame % 30 == 0 {
        println!("lyt root {this:#x} {root:?} visible={}", h.rd8(this + 0x51));
        if std::env::var("REVS_LYT").as_deref() == Ok("2") && h.rd8(this + 0x51) != 0 {
            dump(h, this, 0);
        }
    }
    if h.rd8(this + 0x51) != 0 {
        l.current_roots.push(root.clone());
    }
    walk(h, l, this, &root, 0);
}

/// Names of the visible root layouts drawn in the last complete frame
/// (e.g. "Sub_lytScore1", the scoreboard of a match screen).
pub fn roots() -> Vec<String> {
    let guard = LISTS.lock().unwrap();
    guard.as_ref().map_or(Vec::new(), |l| l.last_roots.clone())
}

/// The buttons on screen in the last complete frame.
pub fn buttons() -> Vec<Btn> {
    let guard = LISTS.lock().unwrap();
    guard.as_ref().map_or(Vec::new(), |l| l.last.clone())
}
