//! The game's own text font (`rcx::font::ImageFont`), for the port's text.
//!
//! The game draws its menu text from a sheet of glyphs (an `ImageFont`, made
//! into a GL texture by `rcx::font::TexFont`). The port learns where the font
//! object is from the game's own glyph lookups (`ImageFont::FindGlyph`, hooked),
//! reads its glyph table from memory, and draws its own text from the same GL
//! texture (textures.rs notes its name when the sheet is uploaded), so any
//! text gets the game's letters. Nothing of the font is kept on disk.
//!
//! `ImageFont` (seen in its getters): +0x14 char height, +0x18 ascent, +0x1c
//! descent, +0x20 leading, +0x30 / +0x34 cell width / height, +0x38 / +0x3c
//! image width / height, +0x40 pixels, +0x444 / +0x448 the glyph table (begin,
//! end; 8 bytes each, sorted by code): +0 u16 code, +2 u8 x offset in the cell,
//! +3 u8 width, +4 u16 column, +6 u16 row.

use crate::Host;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// `rcx::font::ImageFont::FindGlyph(unsigned short) const`.
pub const FIND_GLYPH: u32 = 0x2acbc;
pub const HOOKS: [u32; 1] = [FIND_GLYPH];
/// The menu font's image width (texels): the font the port uses.
const MENU_FONT_W: u32 = 512;

/// One glyph on the sheet, in texels.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub x: f32,
    pub y: f32,
    pub w: f32,
}

pub struct Font {
    glyphs: HashMap<char, Glyph>,
    /// Cell height (the height of every glyph's rectangle), texels.
    pub cell_h: f32,
    /// The GL texture's size (the image rounded up to powers of two).
    pub tex_w: f32,
    pub tex_h: f32,
}

/// The menu font's address and table, once seen.
static FONT: Mutex<Option<(u32, Arc<Font>)>> = Mutex::new(None);

/// At the entry of a HOOKS function.
pub fn on_enter(h: &Host, addr: u32, r0: u32) {
    if addr != FIND_GLYPH {
        return;
    }
    let mut f = FONT.lock().unwrap();
    if f.as_ref().is_some_and(|(a, _)| *a == r0) || h.rd32(r0 + 0x38) != MENU_FONT_W {
        return;
    }
    if std::env::var_os("REVS_FONTDUMP").is_some() {
        dump(h, r0);
    }
    let font = read(h, r0);
    println!("font: the game's menu font at {r0:#x}, {} glyphs", font.glyphs.len());
    *f = Some((r0, Arc::new(font)));
}

fn read(h: &Host, f: u32) -> Font {
    let (cw, ch) = (h.rd32(f + 0x30) as f32, h.rd32(f + 0x34) as f32);
    let (iw, ih) = (h.rd32(f + 0x38), h.rd32(f + 0x3c));
    let (begin, end) = (h.rd32(f + 0x444), h.rd32(f + 0x448));
    let mut glyphs = HashMap::new();
    let mut g = begin;
    while g < end && glyphs.len() < 4096 {
        if let Some(c) = char::from_u32(h.rd16(g) as u32) {
            let x = h.rd16(g + 4) as f32 * cw + h.rd8(g + 2) as f32;
            let y = h.rd16(g + 6) as f32 * ch;
            glyphs.insert(c, Glyph { x, y, w: h.rd8(g + 3) as f32 });
        }
        g += 8;
    }
    Font { glyphs, cell_h: ch, tex_w: iw.next_power_of_two() as f32, tex_h: ih.next_power_of_two() as f32 }
}

/// The menu font, once the game has used it.
pub fn get() -> Option<Arc<Font>> {
    FONT.lock().unwrap().as_ref().map(|(_, f)| f.clone())
}

impl Font {
    /// A character's glyph (unknown ones: '?').
    pub fn glyph(&self, c: char) -> Option<Glyph> {
        self.glyphs.get(&c).or_else(|| self.glyphs.get(&'?')).copied()
    }

    /// Width of `text` drawn with cells `size` points high.
    pub fn width(&self, text: &str, size: f32) -> f32 {
        let k = size / self.cell_h;
        text.chars().filter_map(|c| self.glyph(c)).map(|g| g.w * k).sum()
    }
}

/// REVS_FONTDUMP=1: the font's header and glyph table, once.
fn dump(h: &Host, f: u32) {
    let r = |o: u32| h.rd32(f + o);
    let (begin, end) = (r(0x444), r(0x448));
    println!(
        "font {f:#x}: char h {} ascent {} descent {} leading {} cell {}x{} image {}x{} glyphs {}",
        r(0x14),
        r(0x18),
        r(0x1c),
        r(0x20),
        r(0x30),
        r(0x34),
        r(0x38),
        r(0x3c),
        end.wrapping_sub(begin) / 8
    );
    let mut g = begin;
    while g < end {
        let code = h.rd16(g);
        let ch = char::from_u32(code as u32).unwrap_or('?');
        println!("  glyph {code:#06x} {ch:?} x+{} w={} col={} row={}", h.rd8(g + 2), h.rd8(g + 3), h.rd16(g + 4), h.rd16(g + 6));
        g += 8;
    }
}
