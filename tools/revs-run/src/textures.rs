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
    /// Images the port enlarges by itself when there is no file for them.
    auto: HashMap<u64, Kind>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Reads the replacement folder; with `dump`, also saves the game's 2D images;
/// with `upscale`, enlarges the listed images itself (tools/textures.txt).
pub fn init(build: &Path, dump: bool, upscale: bool) {
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
    let auto = if upscale { builtin_list() } else { HashMap::new() };
    if !auto.is_empty() {
        println!("textures: the port enlarges {} 2D images itself (--upscale=off: as the game drew them)", auto.len());
    }
    *STATE.lock().unwrap() = Some(State { dump, load, dumped: HashSet::new(), replaced: HashSet::new(), auto });
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
    if level != 0 || target != gles11::TEXTURE_2D {
        return;
    }
    let mut tex = 0;
    gl21::GetIntegerv(gl21::TEXTURE_BINDING_2D, &mut tex);
    let tex = tex as u32;
    // New contents for a texture that held an enlarged image (any format, 3D
    // ones too): it is the game's again, with filtering that needs no mipmaps.
    if st.replaced.remove(&tex) {
        gl21::TexParameteri(gl21::TEXTURE_2D, gl21::GENERATE_MIPMAP, gl21::FALSE as _);
        gl21::TexParameteri(gl21::TEXTURE_2D, gl21::TEXTURE_MIN_FILTER, gl21::LINEAR as _);
    }
    if data.is_null() || is_pvrtc(fmt) {
        return;
    }
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
                upload_big(g, w2, h2, &px);
                st.replaced.insert(tex);
                tex_w = w2;
            }
            Err(e) => println!("textures: {}: {e}", path.display()),
        }
    } else if let Some(&kind) = st.auto.get(&key) {
        // The game's own pixels as RGBA (whatever format it uploaded).
        let mut px = vec![0u8; (w * h * 4) as usize];
        gl21::PixelStorei(gl21::PACK_ALIGNMENT, 1);
        gl21::GetTexImage(gl21::TEXTURE_2D, 0, gl21::RGBA, gl21::UNSIGNED_BYTE, px.as_mut_ptr() as *mut _);
        let (w2, h2, big) = auto_upscale(kind, w, h, &px);
        upload_big(g, w2, h2, &big);
        st.replaced.insert(tex);
        tex_w = w2;
    }
    if key == TITLE_SHEET {
        add_title_credit(g, tex_w / w, w, h);
    }
}

/// Uploads an enlarged image (RGBA8) to the bound texture with mipmaps, filtered
/// trilinear. A 4x image is usually drawn smaller than its own size (at 1080p
/// one game texel is about 2 screen pixels, so 4 of its texels share 2), and
/// mipmaps keep that from skipping texels and shimmering.
unsafe fn upload_big(g: &mut dyn GLES, w: u32, h: u32, px: &[u8]) {
    gl21::TexParameteri(gl21::TEXTURE_2D, gl21::GENERATE_MIPMAP, gl21::TRUE as _);
    g.PixelStorei(gles11::UNPACK_ALIGNMENT, 1);
    g.TexImage2D(gles11::TEXTURE_2D, 0, gles11::RGBA as _, w as _, h as _, 0, gles11::RGBA, gles11::UNSIGNED_BYTE, px.as_ptr() as *const _);
    gl21::TexParameteri(gl21::TEXTURE_2D, gl21::TEXTURE_MIN_FILTER, gl21::LINEAR_MIPMAP_LINEAR as _);
    gl21::TexParameteri(gl21::TEXTURE_2D, gl21::TEXTURE_MAG_FILTER, gl21::LINEAR as _);
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

/// glDeleteTextures: the GL names are free for other textures (the game reuses
/// them, for 3D models too); they must not keep the enlarged images' filtering,
/// which needs mipmaps those textures don't have (they would draw white).
pub fn on_delete(names: &[u32]) {
    if let Some(st) = STATE.lock().unwrap().as_mut() {
        for n in names {
            st.replaced.remove(n);
        }
    }
}

/// glTexParameter: replaced textures keep trilinear minification and LINEAR
/// magnification, whatever the game asks for.
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
        if pname == gles11::TEXTURE_MIN_FILTER { gles11::LINEAR_MIPMAP_LINEAR as f32 } else { gles11::LINEAR as f32 }
    } else {
        value
    }
}

/// How the port enlarges one of the game's 2D images by itself (no files
/// needed): the kinds of tools/textures.txt, which also lists the images.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Buttons, panels, HUD art: edge-aware, then a firm sharpen.
    Ui,
    /// Pictures and icons: edge-aware, a light sharpen.
    Photo,
    /// Text sheets: edge-aware, then crisp glyph edges.
    Font,
    /// Tiny pictures: bicubic and a mild sharpen.
    Sharp,
}

/// tools/textures.txt: texture hash -> kind. Hashes only (no game content).
pub fn builtin_list() -> HashMap<u64, Kind> {
    include_str!("../../textures.txt")
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let mut it = l.split_whitespace();
            let h = u64::from_str_radix(it.next()?, 16).ok()?;
            let k = match it.next()? {
                "ui" => Kind::Ui,
                "photo" => Kind::Photo,
                "font" => Kind::Font,
                "sharp" => Kind::Sharp,
                _ => return None,
            };
            Some((h, k))
        })
        .collect()
}

/// The built-in enlargement: 4x, RGBA8 in and out.
pub fn auto_upscale(kind: Kind, w: u32, h: u32, px: &[u8]) -> (u32, u32, Vec<u8>) {
    if kind == Kind::Sharp {
        return sharp_upscale(w, h, px);
    }
    let (w4, h4) = (4 * w as usize, 4 * h as usize);
    let mut big = edge_upscale(w as usize, h as usize, px, 4);
    match kind {
        Kind::Ui | Kind::Font => sharpen(w4, h4, &mut big, 0.7),
        Kind::Photo => sharpen(w4, h4, &mut big, 0.35),
        _ => {}
    }
    let mut out = unpremultiply(&big);
    if kind == Kind::Font {
        // Softer than sharpen_alpha (made for the AI's thicker strokes): the
        // game's thin glyphs are only partly opaque, and a high cut-off would
        // eat holes into them.
        for p in out.chunks_mut(4) {
            let t = ((p[3] as f32 / 255.0 - 0.2) / 0.35).clamp(0.0, 1.0);
            p[3] = (255.0 * t * t * (3.0 - 2.0 * t)).round() as u8;
        }
    }
    (w4 as u32, h4 as u32, out)
}

/// FSR 1's polynomial stand-in for Lanczos-2, as a function of distance
/// squared (0 at 2 texels and beyond).
fn lanczos2_sq(d2: f32) -> f32 {
    if d2 >= 4.0 {
        return 0.0;
    }
    let a = 2.0 / 5.0 * d2 - 1.0;
    let b = 0.25 * d2 - 1.0;
    (25.0 / 16.0 * a * a - (25.0 / 16.0 - 1.0)) * b * b
}

/// Edge-aware enlargement by `k` (the idea of AMD FSR 1's EASU, written for
/// still images): each output pixel is a Lanczos-2 blend of the 4 x 4 texels
/// around it, with the kernel stretched along the local edge (found from the
/// structure tensor of a luma + alpha signal) and narrowed across it, so edges
/// stay sharp and smooth instead of stair-stepped or blurred. The result is
/// held within the 2 x 2 nearest texels (no halos). Works on premultiplied
/// colour, so transparent edges get no fringe; returns premultiplied 0..1.
fn edge_upscale(w: usize, h: usize, px: &[u8], k: usize) -> Vec<[f32; 4]> {
    let src: Vec<[f32; 4]> = px
        .chunks(4)
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [p[0] as f32 / 255.0 * a, p[1] as f32 / 255.0 * a, p[2] as f32 / 255.0 * a, a]
        })
        .collect();
    let at = |x: isize, y: isize| y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize;
    let luma: Vec<f32> = src.iter().map(|p| 0.5 * (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]) + 0.5 * p[3]).collect();
    let tensor: Vec<[f32; 3]> = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            let gx = 0.5 * (luma[at(x + 1, y)] - luma[at(x - 1, y)]);
            let gy = 0.5 * (luma[at(x, y + 1)] - luma[at(x, y - 1)]);
            [gx * gx, gy * gy, gx * gy]
        })
        .collect();
    let (ow, oh) = (w * k, h * k);
    let mut out = vec![[0f32; 4]; ow * oh];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    let rows_per = oh.div_ceil(threads).max(1);
    let (src, tensor) = (&src, &tensor);
    std::thread::scope(|s| {
        for (ci, chunk) in out.chunks_mut(rows_per * ow).enumerate() {
            s.spawn(move || {
                for (ry, row) in chunk.chunks_mut(ow).enumerate() {
                    let fy = ((ci * rows_per + ry) as f32 + 0.5) / k as f32 - 0.5;
                    let yi = fy.floor() as isize;
                    let ty = fy - yi as f32;
                    for (ox, o) in row.iter_mut().enumerate() {
                        let fx = (ox as f32 + 0.5) / k as f32 - 0.5;
                        let xi = fx.floor() as isize;
                        let tx = fx - xi as f32;
                        // Edge direction and strength here: the bilinear mix of
                        // the 4 nearest texels' structure tensors.
                        let mut t = [0f32; 3];
                        for (dx, dy, wt) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
                            let q = tensor[at(xi + dx, yi + dy)];
                            for c in 0..3 {
                                t[c] += q[c] * wt;
                            }
                        }
                        let (a, b, c) = (t[0], t[1], t[2]);
                        let disc = ((a - b) * (a - b) + 4.0 * c * c).sqrt();
                        let (l1, l2) = (0.5 * (a + b + disc), 0.5 * (a + b - disc));
                        let (mut gx, mut gy) = if c.abs() > 1e-12 { (l1 - b, c) } else if a >= b { (1.0, 0.0) } else { (0.0, 1.0) };
                        let n = (gx * gx + gy * gy).sqrt().max(1e-12);
                        gx /= n;
                        gy /= n;
                        let coherence = if l1 + l2 > 1e-10 { (l1 - l2) / (l1 + l2) } else { 0.0 };
                        let e = ((l1.sqrt() - 0.01) / 0.07).clamp(0.0, 1.0);
                        let strength = coherence * e * e * (3.0 - 2.0 * e);
                        let along = 1.0 + 1.5 * strength;
                        let across = 1.0 + 0.5 * strength;
                        let (mut acc, mut wsum) = ([0f32; 4], 0f32);
                        let (mut lo, mut hi) = ([f32::MAX; 4], [f32::MIN; 4]);
                        for j in -1..=2isize {
                            for i in -1..=2isize {
                                let (dx, dy) = ((xi + i) as f32 - fx, (yi + j) as f32 - fy);
                                let u = (dx * gx + dy * gy) * across;
                                let v = (dy * gx - dx * gy) / along;
                                let wt = lanczos2_sq(u * u + v * v);
                                let p = src[at(xi + i, yi + j)];
                                for c in 0..4 {
                                    acc[c] += p[c] * wt;
                                }
                                wsum += wt;
                                if (0..=1).contains(&i) && (0..=1).contains(&j) {
                                    for c in 0..4 {
                                        lo[c] = lo[c].min(p[c]);
                                        hi[c] = hi[c].max(p[c]);
                                    }
                                }
                            }
                        }
                        let inv = if wsum.abs() > 1e-6 { 1.0 / wsum } else { 0.0 };
                        for c in 0..4 {
                            o[c] = (acc[c] * inv).clamp(lo[c], hi[c]);
                        }
                        // Premultiplied: colour never above alpha.
                        for c in 0..3 {
                            o[c] = o[c].min(o[3]);
                        }
                    }
                }
            });
        }
    });
    out
}

/// Unsharp mask (5 x 5 box) held within each pixel's 3 x 3 neighbourhood, so it
/// firms up edges without halos. Premultiplied colour and alpha alike.
fn sharpen(w: usize, h: usize, px: &mut [[f32; 4]], amount: f32) {
    let pass = |src: &[[f32; 4]], horiz: bool, r: usize, op: Op| line_pass(w, h, src, horiz, r, op);
    let blur = pass(&pass(px, true, 2, Op::Mean), false, 2, Op::Mean);
    let lo = pass(&pass(px, true, 1, Op::Min), false, 1, Op::Min);
    let hi = pass(&pass(px, true, 1, Op::Max), false, 1, Op::Max);
    for i in 0..px.len() {
        for c in 0..4 {
            px[i][c] = (px[i][c] + amount * (px[i][c] - blur[i][c])).clamp(lo[i][c], hi[i][c]);
        }
        for c in 0..3 {
            px[i][c] = px[i][c].min(px[i][3]);
        }
    }
}

#[derive(Clone, Copy)]
enum Op {
    Mean,
    Min,
    Max,
}

/// One direction of a separable box filter (mean, min or max over 2r + 1
/// pixels, edges clamped), rows split across threads.
fn line_pass(w: usize, h: usize, src: &[[f32; 4]], horiz: bool, r: usize, op: Op) -> Vec<[f32; 4]> {
    let mut out = vec![[0f32; 4]; src.len()];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    let rows_per = h.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (ci, chunk) in out.chunks_mut(rows_per * w).enumerate() {
            s.spawn(move || {
                for (ry, row) in chunk.chunks_mut(w).enumerate() {
                    let y = ci * rows_per + ry;
                    for (x, o) in row.iter_mut().enumerate() {
                        let mut acc = match op {
                            Op::Mean => [0.0; 4],
                            Op::Min => [f32::MAX; 4],
                            Op::Max => [f32::MIN; 4],
                        };
                        for d in 0..=2 * r {
                            let p = if horiz { src[y * w + (x + d).saturating_sub(r).min(w - 1)] } else { src[(y + d).saturating_sub(r).min(h - 1) * w + x] };
                            for c in 0..4 {
                                acc[c] = match op {
                                    Op::Mean => acc[c] + p[c],
                                    Op::Min => acc[c].min(p[c]),
                                    Op::Max => acc[c].max(p[c]),
                                };
                            }
                        }
                        if let Op::Mean = op {
                            for v in &mut acc {
                                *v /= (2 * r + 1) as f32;
                            }
                        }
                        *o = acc;
                    }
                }
            });
        }
    });
    out
}

/// Premultiplied 0..1 -> straight RGBA8.
fn unpremultiply(px: &[[f32; 4]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(px.len() * 4);
    for p in px {
        let a = p[3].clamp(0.0, 1.0);
        let inv = if a > 1.0 / 512.0 { 1.0 / a } else { 0.0 };
        for c in 0..3 {
            out.push((p[c] * inv * 255.0).round().clamp(0.0, 255.0) as u8);
        }
        out.push((a * 255.0).round() as u8);
    }
    out
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
    fn auto_upscale_flat_and_list() {
        // A flat colour stays flat, size is 4x, and the list parses.
        let px: Vec<u8> = std::iter::repeat([200u8, 40, 10, 128]).take(7 * 3).flatten().collect();
        for kind in [super::Kind::Ui, super::Kind::Photo] {
            let (w, h, up) = super::auto_upscale(kind, 7, 3, &px);
            assert_eq!((w, h, up.len()), (28, 12, 28 * 12 * 4));
            assert!(up.chunks(4).all(|p| p[3] == 128 && (p[0] as i32 - 200).abs() <= 1 && (p[1] as i32 - 40).abs() <= 1 && (p[2] as i32 - 10).abs() <= 1), "{kind:?}");
        }
        assert_eq!(super::builtin_list().len(), 20);
    }

    /// Side-by-side sheets of the busiest part of each listed image: the
    /// original (enlarged blocky), the built-in result, the Real-ESRGAN file.
    /// Reads the user's own dumps; writes to a folder of theirs:
    ///   REVS_PREVIEW_DUMP=build/textures/dump REVS_PREVIEW_LOAD=build/textures/load
    ///   REVS_PREVIEW_OUT=<folder> cargo test -p revs-run --release -- --ignored upscale_preview
    #[test]
    #[ignore]
    fn upscale_preview() {
        let var = |k: &str| std::path::PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")));
        let (dump, load, out) = (var("REVS_PREVIEW_DUMP"), var("REVS_PREVIEW_LOAD"), var("REVS_PREVIEW_OUT"));
        std::fs::create_dir_all(&out).unwrap();
        let find = |dir: &std::path::Path, h: u64| {
            std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().unwrap().to_string_lossy().starts_with(&format!("{h:016x}")))
        };
        let mut list: Vec<_> = super::builtin_list().into_iter().collect();
        list.sort_by_key(|e| e.0);
        for (h, kind) in list {
            let Some(src) = find(&dump, h) else { continue };
            let (w, hh, px) = super::png_read(&std::fs::read(&src).unwrap()).unwrap();
            let t = std::time::Instant::now();
            let (_, _, auto) = super::auto_upscale(kind, w, hh, &px);
            let ms = t.elapsed().as_millis();
            let ai = find(&load, h).map(|p| {
                let name = p.to_string_lossy().to_ascii_lowercase();
                let (w2, h2, mut q) = super::png_read(&std::fs::read(&p).unwrap()).unwrap();
                if name.ends_with(".font.png") {
                    super::sharpen_alpha(&mut q);
                }
                if name.ends_with(".sharp.png") { super::sharp_upscale(w2, h2, &q).2 } else { q }
            });
            // Busiest 40 x 28 window of the original (alpha-weighted edges).
            let (cw, ch) = (40u32.min(w), 28u32.min(hh));
            let lum = |x: u32, y: u32| {
                let p = &px[(4 * (y * w + x)) as usize..][..4];
                (p[0] as i32 + 2 * p[1] as i32 + p[2] as i32) * p[3] as i32 / 1020 + p[3] as i32
            };
            let (mut best, mut bx, mut by) = (-1i64, 0, 0);
            for y0 in (0..=hh - ch).step_by(4) {
                for x0 in (0..=w - cw).step_by(4) {
                    let mut e = 0i64;
                    for y in y0..y0 + ch - 1 {
                        for x in x0..x0 + cw - 1 {
                            e += ((lum(x, y) - lum(x + 1, y)).abs() + (lum(x, y) - lum(x, y + 1)).abs()) as i64;
                        }
                    }
                    if e > best {
                        (best, bx, by) = (e, x0, y0);
                    }
                }
            }
            // Panels: each crop at 4x, shown at 2x more (blocky) for viewing.
            let (pw, ph, gap) = (cw * 8, ch * 8, 8u32);
            let panels: Vec<Option<&Vec<u8>>> = vec![None, Some(&auto), ai.as_ref()];
            let sw = 3 * pw + 2 * gap;
            let mut sheet = vec![255u8; (sw * ph * 4) as usize];
            for (n, img) in panels.iter().enumerate() {
                if n == 2 && img.is_none() {
                    continue;
                }
                for y in 0..ph {
                    for x in 0..pw {
                        let (bxx, byy) = (4 * bx + x / 2, 4 * by + y / 2);
                        let p = match img {
                            Some(im) => &im[(4 * (byy * 4 * w + bxx)) as usize..][..4],
                            None => &px[(4 * ((byy / 4) * w + bxx / 4)) as usize..][..4],
                        };
                        let checker = if ((x / 16) + (y / 16)) % 2 == 0 { 70.0 } else { 110.0 };
                        let a = p[3] as f32 / 255.0;
                        let i = (4 * (y * sw + n as u32 * (pw + gap) + x)) as usize;
                        for c in 0..3 {
                            sheet[i + c] = (p[c] as f32 * a + checker * (1.0 - a)).round() as u8;
                        }
                        sheet[i + 3] = 255;
                    }
                }
            }
            std::fs::write(out.join(format!("{h:016x}_{kind:?}.png")), super::png_write(sw, ph, &sheet)).unwrap();
            println!("{h:016x} {kind:?} {w}x{hh}: {ms} ms");
        }
    }

    #[test]
    fn png_round_trip() {
        let px: Vec<u8> = (0..4 * 5 * 3).map(|i| (i * 37 % 256) as u8).collect();
        let (w, h, back) = super::png_read(&super::png_write(5, 3, &px)).unwrap();
        assert_eq!((w, h), (5, 3));
        assert_eq!(back, px);
    }
}
