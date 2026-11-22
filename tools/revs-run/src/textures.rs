//! Replacement textures: cleaner, bigger versions of the game's 2D images
//! (buttons, icons, fonts, HUD), loaded from a local folder.
//!
//! Every texture the game uploads is named by a hash of the data it gave
//! (format, size, bytes). `--dump-textures` saves the 2D ones (8-bit palette
//! and plain RGBA; PVRTC is the 3D models) as `<build>/textures/dump/
//! <hash>_<w>x<h>.png`. A PNG of any size in `<build>/textures/load/` whose
//! name starts with the same hash is uploaded instead of the game's image.
//! Texture coordinates are 0..1, so a bigger image draws in the same place.
//! A `*.font.png` (a text sheet) gets its glyph edges sharpened on loading;
//! a `*.sharp.png` (a small picture, given at its own size) is enlarged 4x
//! by the port itself (bicubic and a mild sharpen).
//! Replaced textures are always filtered LINEAR (the game asks for NEAREST on
//! many 2D images, which is blocky when enlarged).
//!
//! Both folders hold images made from the user's own game: they live in the
//! git-ignored build folder and are never shared.

use crate::gles::{gl21compat_raw as gl21, gles11_raw as gles11, GLES};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

struct State {
    dump: Option<PathBuf>,
    /// Hash -> replacement PNG.
    load: HashMap<u64, PathBuf>,
    dumped: HashSet<u64>,
    /// GL names of textures that hold a replacement.
    replaced: HashSet<u32>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Reads the replacement folder; with `dump`, also saves the game's 2D images.
pub fn init(build: &Path, dump: bool) {
    let dir = build.join("textures");
    let mut load = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("load")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.to_ascii_lowercase().ends_with(".png") {
                continue;
            }
            if let Some(h) = name.get(..16).and_then(|s| u64::from_str_radix(s, 16).ok()) {
                load.insert(h, e.path());
            }
        }
    }
    let dump = dump.then(|| dir.join("dump"));
    if let Some(d) = &dump {
        let _ = std::fs::create_dir_all(d);
        println!("textures: saving 2D images to {}", d.display());
    }
    if !load.is_empty() {
        println!("textures: {} replacements in {}", load.len(), dir.join("load").display());
    }
    *STATE.lock().unwrap() = Some(State { dump, load, dumped: HashSet::new(), replaced: HashSet::new() });
}

fn is_pvrtc(fmt: u32) -> bool {
    (0x8c00..=0x8c03).contains(&fmt)
}

/// Bytes of a glTexImage2D upload (0 if unknown).
fn tex_bytes(format: u32, type_: u32, w: u32, h: u32) -> usize {
    let px = match (format, type_) {
        (_, gles11::UNSIGNED_SHORT_5_6_5 | gles11::UNSIGNED_SHORT_4_4_4_4 | gles11::UNSIGNED_SHORT_5_5_5_1) => 2,
        (gles11::RGBA, gles11::UNSIGNED_BYTE) | (0x80e1, gles11::UNSIGNED_BYTE) => 4, // RGBA, BGRA
        (gles11::RGB, gles11::UNSIGNED_BYTE) => 3,
        (gles11::LUMINANCE_ALPHA, gles11::UNSIGNED_BYTE) => 2,
        (gles11::ALPHA | gles11::LUMINANCE, gles11::UNSIGNED_BYTE) => 1,
        _ => 0,
    };
    px * w as usize * h as usize
}

fn hash(fmt: u32, w: u32, h: u32, data: &[u8]) -> u64 {
    // FNV-1a, 64-bit.
    let mut x: u64 = 0xcbf29ce484222325;
    for b in fmt.to_le_bytes().iter().chain(&w.to_le_bytes()).chain(&h.to_le_bytes()).chain(data) {
        x = (x ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    x
}

/// After the game's glTexImage2D / glCompressedTexImage2D: `fmt` is the
/// internal format of a compressed upload, else the pixel format; `size` is
/// imageSize for a compressed upload, else 0 (worked out from format/type).
///
/// # Safety
/// `data` points to the game's pixels (or is null); the GL context is current.
pub unsafe fn on_upload(g: &mut dyn GLES, target: u32, level: i32, fmt: u32, type_: u32, w: u32, h: u32, size: usize, data: *const u8) {
    let mut st = STATE.lock().unwrap();
    let Some(st) = st.as_mut() else { return };
    if level != 0 || target != gles11::TEXTURE_2D || data.is_null() || is_pvrtc(fmt) {
        return;
    }
    let mut tex = 0;
    gl21::GetIntegerv(gl21::TEXTURE_BINDING_2D, &mut tex);
    let tex = tex as u32;
    st.replaced.remove(&tex);
    let size = if size > 0 { size } else { tex_bytes(fmt, type_, w, h) };
    if size == 0 {
        return;
    }
    let key = hash(fmt, w, h, std::slice::from_raw_parts(data, size));
    if let Some(dir) = &st.dump {
        if st.dumped.insert(key) {
            let mut px = vec![0u8; (w * h * 4) as usize];
            gl21::PixelStorei(gl21::PACK_ALIGNMENT, 1);
            gl21::GetTexImage(gl21::TEXTURE_2D, 0, gl21::RGBA, gl21::UNSIGNED_BYTE, px.as_mut_ptr() as *mut _);
            bleed(w as usize, h as usize, &mut px);
            let _ = std::fs::write(dir.join(format!("{key:016x}_{w}x{h}.png")), png_write(w, h, &px));
        }
    }
    let mut tex_w = w;
    if let Some(path) = st.load.get(&key) {
        match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| png_read(&b)) {
            Ok((w2, h2, mut px)) => {
                let lower = path.to_string_lossy().to_ascii_lowercase();
                if lower.ends_with(".font.png") {
                    sharpen_alpha(&mut px);
                }
                let (w2, h2) = if lower.ends_with(".sharp.png") {
                    let (w4, h4, up) = sharp_upscale(w2, h2, &px);
                    px = up;
                    (w4, h4)
                } else {
                    (w2, h2)
                };
                g.PixelStorei(gles11::UNPACK_ALIGNMENT, 1);
                g.TexImage2D(gles11::TEXTURE_2D, 0, gles11::RGBA as _, w2 as _, h2 as _, 0, gles11::RGBA, gles11::UNSIGNED_BYTE, px.as_ptr() as *const _);
                st.replaced.insert(tex);
                tex_w = w2;
            }
            Err(e) => println!("textures: {}: {e}", path.display()),
        }
    }
    if key == TITLE_SHEET {
        add_title_credit(g, tex_w / w, w, h);
    }
}

/// The title screen's sheet (logos and the "(c) CAPCOM 2011" line).
const TITLE_SHEET: u64 = 0x1f7cd367695d3dc5;
/// Where the game's copyright line is on it, and where the port puts the
/// longer line (an empty band of the sheet), in the game's texels.
pub const COPYRIGHT_SRC: [u32; 4] = [12, 378, 120, 15];
pub const TITLE_LINE_AT: (u32, u32) = (8, 456);
/// The text added after it (tools/make-title-credit.ps1), 4x the game's size.
const CREDIT_PNG: &[u8] = include_bytes!("../assets/title_credit.png");
/// Width of the new line in game points, once it is on the sheet.
static TITLE_LINE_W: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn title_line_w() -> u32 {
    TITLE_LINE_W.load(std::sync::atomic::Ordering::Relaxed)
}

/// Builds "(c) CAPCOM 2011 . Recomp by albertoha94" in the empty band of the
/// title sheet: the game's own copyright pixels, then the credit text, at the
/// texture's scale `k` (1, or 4 with a replacement). lyt.rs points the
/// copyright part at it.
unsafe fn add_title_credit(g: &mut dyn GLES, k: u32, w: u32, h: u32) {
    let Ok((cw4, ch4, credit)) = png_read(CREDIT_PNG) else { return };
    let (tw, th) = (w * k, h * k);
    let mut sheet = vec![0u8; (tw * th * 4) as usize];
    gl21::PixelStorei(gl21::PACK_ALIGNMENT, 1);
    gl21::GetTexImage(gl21::TEXTURE_2D, 0, gl21::RGBA, gl21::UNSIGNED_BYTE, sheet.as_mut_ptr() as *mut _);
    let [sx, sy, sw, sh] = COPYRIGHT_SRC.map(|v| v * k);
    // The credit at this scale: 4x as made, else box-filtered down.
    let f = 4 / k.max(1);
    let (cw, chh) = (cw4 / f, (ch4 / f).min(sh));
    let line_w = sw + cw;
    let mut line = vec![0u8; (line_w * sh * 4) as usize];
    for y in 0..sh {
        let src = (((sy + y) * tw + sx) * 4) as usize;
        let dst = (y * line_w * 4) as usize;
        line[dst..dst + (sw * 4) as usize].copy_from_slice(&sheet[src..src + (sw * 4) as usize]);
    }
    for y in 0..chh {
        for x in 0..cw {
            let mut acc = [0u32; 4];
            for yy in 0..f {
                for xx in 0..f {
                    let i = (((y * f + yy) * cw4 + x * f + xx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += credit[i + c] as u32;
                    }
                }
            }
            let i = ((y * line_w + sw + x) * 4) as usize;
            for c in 0..4 {
                line[i + c] = (acc[c] / (f * f)) as u8;
            }
        }
    }
    let (ax, ay) = (TITLE_LINE_AT.0 * k, TITLE_LINE_AT.1 * k);
    if ax + line_w > tw || ay + sh > th {
        return;
    }
    g.PixelStorei(gles11::UNPACK_ALIGNMENT, 1);
    g.TexSubImage2D(gles11::TEXTURE_2D, 0, ax as _, ay as _, line_w as _, sh as _, gles11::RGBA, gles11::UNSIGNED_BYTE, line.as_ptr() as *const _);
    TITLE_LINE_W.store(line_w / k, std::sync::atomic::Ordering::Relaxed);
}

/// glTexParameter: replaced textures keep LINEAR min/mag filters.
pub fn filter(pname: u32, value: f32) -> f32 {
    if pname != gles11::TEXTURE_MIN_FILTER && pname != gles11::TEXTURE_MAG_FILTER {
        return value;
    }
    let st = STATE.lock().unwrap();
    let Some(st) = st.as_ref() else { return value };
    if st.replaced.is_empty() {
        return value;
    }
    let mut tex = 0;
    unsafe { gl21::GetIntegerv(gl21::TEXTURE_BINDING_2D, &mut tex) };
    if st.replaced.contains(&(tex as u32)) {
        gles11::LINEAR as f32
    } else {
        value
    }
}

/// Small pictures (`*.sharp.png`, the original image): 4x bicubic, then a
/// mild unsharp mask (5 x 5 box, amount 0.8). On faces only 40 pixels wide
/// this looks more natural than the AI upscalers, which invent detail.
fn sharp_upscale(w: u32, h: u32, px: &[u8]) -> (u32, u32, Vec<u8>) {
    let (w, h) = (w as i32, h as i32);
    let (w4, h4) = (4 * w, 4 * h);
    let at = |x: i32, y: i32, c: usize| px[(4 * (y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize) + c] as f32;
    let cub = |t: f32| {
        let (t, a) = (t.abs(), -0.5f32);
        if t < 1.0 { (a + 2.0) * t * t * t - (a + 3.0) * t * t + 1.0 } else if t < 2.0 { a * t * t * t - 5.0 * a * t * t + 8.0 * a * t - 4.0 * a } else { 0.0 }
    };
    let mut big = vec![0f32; (w4 * h4 * 4) as usize];
    for y in 0..h4 {
        let fy = (y as f32 + 0.5) / 4.0 - 0.5;
        let yi = fy.floor() as i32;
        for x in 0..w4 {
            let fx = (x as f32 + 0.5) / 4.0 - 0.5;
            let xi = fx.floor() as i32;
            let mut c = [0f32; 4];
            for j in -1..3 {
                let wy = cub(fy - (yi + j) as f32);
                for i in -1..3 {
                    let wt = cub(fx - (xi + i) as f32) * wy;
                    for k in 0..4 {
                        c[k] += at(xi + i, yi + j, k) * wt;
                    }
                }
            }
            big[(4 * (y * w4 + x)) as usize..][..4].copy_from_slice(&c);
        }
    }
    // Box blur (separable, radius 2) of the colour, for the unsharp mask.
    let blur_pass = |src: &[f32], horiz: bool| {
        let mut out = vec![0f32; src.len()];
        for y in 0..h4 {
            for x in 0..w4 {
                let mut s = [0f32; 3];
                for d in -2..=2 {
                    let (xx, yy) = if horiz { ((x + d).clamp(0, w4 - 1), y) } else { (x, (y + d).clamp(0, h4 - 1)) };
                    let i = (4 * (yy * w4 + xx)) as usize;
                    for k in 0..3 {
                        s[k] += src[i + k];
                    }
                }
                let i = (4 * (y * w4 + x)) as usize;
                for k in 0..3 {
                    out[i + k] = s[k] / 5.0;
                }
            }
        }
        out
    };
    let blur = blur_pass(&blur_pass(&big, true), false);
    let out = (0..big.len())
        .map(|i| {
            let v = if i % 4 == 3 { big[i] } else { big[i] + 0.8 * (big[i] - blur[i]) };
            v.round().clamp(0.0, 255.0) as u8
        })
        .collect();
    (w4 as u32, h4 as u32, out)
}

/// Text sheets (`*.font.png`): an upscaled glyph has soft, wide edges; a
/// smooth step on alpha (0.35..0.65) makes them crisp at the same weight.
fn sharpen_alpha(px: &mut [u8]) {
    for p in px.chunks_mut(4) {
        let t = ((p[3] as f32 / 255.0 - 0.35) / 0.3).clamp(0.0, 1.0);
        p[3] = (255.0 * t * t * (3.0 - 2.0 * t)).round() as u8;
    }
}

/// Gives fully transparent pixels the colour of their visible neighbours
/// (alpha stays 0, so the image looks the same). An upscaler works on colour
/// and alpha apart: leftover colour under transparent pixels would otherwise
/// bleed into the edges of glyphs and buttons as a coloured fringe.
fn bleed(w: usize, h: usize, px: &mut [u8]) {
    let mut known: Vec<bool> = px.chunks(4).map(|p| p[3] != 0).collect();
    for _ in 0..8 {
        let mut next = known.clone();
        let mut changed = false;
        for y in 0..h {
            for x in 0..w {
                if known[y * w + x] {
                    continue;
                }
                let (mut sum, mut n) = ([0u32; 3], 0);
                for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 || !known[ny as usize * w + nx as usize] {
                        continue;
                    }
                    let k = 4 * (ny as usize * w + nx as usize);
                    for c in 0..3 {
                        sum[c] += px[k + c] as u32;
                    }
                    n += 1;
                }
                if n > 0 {
                    for c in 0..3 {
                        px[4 * (y * w + x) + c] = (sum[c] / n) as u8;
                    }
                    next[y * w + x] = true;
                    changed = true;
                }
            }
        }
        known = next;
        if !changed {
            break;
        }
    }
}

/// RGBA8 -> PNG (no filtering, zlib from flate2).
fn png_write(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut raw = Vec::with_capacity(rgba.len() + h as usize);
    for row in rgba.chunks(w as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&raw).unwrap();
    let idat = z.finish().unwrap();
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |t: &[u8], d: &[u8]| {
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        let mut c = crc32fast::Hasher::new();
        c.update(t);
        c.update(d);
        out.extend_from_slice(t);
        out.extend_from_slice(d);
        out.extend_from_slice(&c.finalize().to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &idat);
    chunk(b"IEND", &[]);
    out
}

/// PNG (8-bit RGB or RGBA, not interlaced) -> (w, h, RGBA8).
fn png_read(b: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    use std::io::Read;
    if b.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
        return Err("not a PNG".into());
    }
    let (mut i, mut idat, mut hdr) = (8, Vec::new(), None);
    while i + 8 <= b.len() {
        let n = u32::from_be_bytes(b[i..i + 4].try_into().unwrap()) as usize;
        let t = &b[i + 4..i + 8];
        let d = b.get(i + 8..i + 8 + n).ok_or("cut short")?;
        match t {
            b"IHDR" => hdr = Some((u32::from_be_bytes(d[0..4].try_into().unwrap()), u32::from_be_bytes(d[4..8].try_into().unwrap()), d[8], d[9], d[12])),
            b"IDAT" => idat.extend_from_slice(d),
            _ => {}
        }
        i += 12 + n;
    }
    let (w, h, depth, ct, lace) = hdr.ok_or("no IHDR")?;
    let bpp = match (depth, ct, lace) {
        (8, 2, 0) => 3,
        (8, 6, 0) => 4,
        _ => return Err(format!("unsupported PNG (depth {depth}, colour type {ct}, interlace {lace})")),
    };
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&idat[..]).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let s = w as usize * bpp;
    if raw.len() < (s + 1) * h as usize {
        return Err("image data cut short".into());
    }
    let mut px = vec![0u8; s * h as usize];
    for y in 0..h as usize {
        let f = raw[y * (s + 1)];
        for x in 0..s {
            let v = raw[y * (s + 1) + 1 + x];
            let a = if x >= bpp { px[y * s + x - bpp] as i32 } else { 0 };
            let u = if y > 0 { px[(y - 1) * s + x] as i32 } else { 0 };
            let c = if x >= bpp && y > 0 { px[(y - 1) * s + x - bpp] as i32 } else { 0 };
            let pred = match f {
                1 => a,
                2 => u,
                3 => (a + u) / 2,
                4 => {
                    let p = a + u - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - u).abs(), (p - c).abs());
                    if pa <= pb && pa <= pc { a } else if pb <= pc { u } else { c }
                }
                _ => 0,
            };
            px[y * s + x] = v.wrapping_add(pred as u8);
        }
    }
    if bpp == 3 {
        px = px.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    }
    Ok((w, h, px))
}

#[cfg(test)]
mod tests {
    #[test]
    fn sharp_upscale_size_and_flat() {
        // A flat colour stays flat (no ringing) and the size is 4x.
        let px: Vec<u8> = std::iter::repeat([90u8, 120, 200, 255]).take(6 * 5).flatten().collect();
        let (w, h, up) = super::sharp_upscale(6, 5, &px);
        assert_eq!((w, h, up.len()), (24, 20, 24 * 20 * 4));
        assert!(up.chunks(4).all(|p| p == [90, 120, 200, 255]));
    }

    #[test]
    fn png_round_trip() {
        let px: Vec<u8> = (0..4 * 5 * 3).map(|i| (i * 37 % 256) as u8).collect();
        let (w, h, back) = super::png_read(&super::png_write(5, 3, &px)).unwrap();
        assert_eq!((w, h), (5, 3));
        assert_eq!(back, px);
    }
}
