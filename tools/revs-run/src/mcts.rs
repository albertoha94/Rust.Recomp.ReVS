//! `--extract-textures`: saves every 3D texture (characters, enemies, stages,
//! weapons, effects) from the user's own .ipa to `<build>/textures/dump`,
//! without playing. The names are the ones the port gives a texture when the
//! game uploads it (textures.rs), so a file made from one loads in its place.
//!
//! Where they are: the `Acv_3D*.bin` archives hold `.mcts` files, each a list
//! of `MCT_` textures:
//!
//! ```text
//! .mcts   u32 count, (count + 1) x u32 offsets (the last one is the end)
//! MCT_    byte 11 format, u16 width at 0x22, u16 height at 0x24,
//!         data from 42 to the end less 2 bytes: what the game gives
//!         glCompressedTexImage2D
//! ```
//!
//! The format follows from the data size (the format byte's flags don't map
//! to it cleanly): w*h/2 or w*h/4 bytes = PVRTC 4 or 2 bits per pixel; w*h
//! plus 512, 768 or 1024 = an 8-bit palette (RGB565, RGB8, RGBA8) before the
//! indices. Checked against the hashes of a played match.

use crate::fs::Fs;
use std::path::Path;

const ARCHIVES: [&str; 5] = ["Acv_3DBase.bin", "Acv_3DChar.bin", "Acv_3DStg01.bin", "Acv_3DStg02.bin", "Acv_3DStg03.bin"];

const PVRTC_4: u32 = 0x8c02;
const PVRTC_2: u32 = 0x8c03;
const PAL8_RGB8: u32 = 0x8b95;
const PAL8_RGBA8: u32 = 0x8b96;
const PAL8_565: u32 = 0x8b97;

/// `Acv_*.bin`: u32 count, names offset, data offset, then count x (name
/// start, length) and count x (data start, size).
fn acv_entries(d: &[u8]) -> Vec<(String, &[u8])> {
    let u = |o: usize| d.get(o..o + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let (n, names, data) = (u(0), u(4), u(8));
    (0..n)
        .filter_map(|i| {
            let (s, l) = (u(12 + 8 * i), u(16 + 8 * i));
            let (o, sz) = (u(12 + 8 * n + 8 * i), u(16 + 8 * n + 8 * i));
            let name = String::from_utf8_lossy(d.get(names + s..names + s + l)?).into_owned();
            Some((name, d.get(data + o..data + o + sz)?))
        })
        .collect()
}

fn mcts_entries(b: &[u8]) -> Vec<&[u8]> {
    let u = |o: usize| b.get(o..o + 4).map_or(0, |x| u32::from_le_bytes(x.try_into().unwrap()) as usize);
    let n = u(0);
    (0..n).filter_map(|i| b.get(u(4 + 4 * i)..u(8 + 4 * i))).collect()
}

/// One texture as the game uploads it: (GL format, w, h, data), or None for
/// an empty slot or a layout not seen before.
fn mct(e: &[u8]) -> Option<(u32, u32, u32, &[u8])> {
    if e.len() < 44 || &e[..4] != b"MCT_" {
        return None;
    }
    let w = u16::from_le_bytes([e[0x22], e[0x23]]) as u32;
    let h = u16::from_le_bytes([e[0x24], e[0x25]]) as u32;
    let data = &e[42..e.len() - 2];
    let px = (w * h) as usize;
    let fmt = match data.len() {
        n if n == px / 2 => PVRTC_4,
        n if n == px / 4 => PVRTC_2,
        n if n == 512 + px => PAL8_565,
        n if n == 768 + px => PAL8_RGB8,
        n if n == 1024 + px => PAL8_RGBA8,
        _ => return None,
    };
    Some((fmt, w, h, data))
}

/// RGBA8 pixels of one texture.
fn decode(fmt: u32, w: u32, h: u32, data: &[u8]) -> Vec<u8> {
    if fmt == PVRTC_4 || fmt == PVRTC_2 {
        let px = crate::gles::decode_pvrtc(data, fmt == PVRTC_2, w, h);
        return px.iter().flat_map(|p| p.to_le_bytes()).collect();
    }
    let pe = match fmt {
        PAL8_565 => 2,
        PAL8_RGB8 => 3,
        _ => 4,
    };
    let (pal, idx) = data.split_at(256 * pe);
    let x5 = |c: u16| ((c << 3) | (c >> 2)) as u8;
    let x6 = |c: u16| ((c << 2) | (c >> 4)) as u8;
    idx.iter()
        .flat_map(|&i| {
            let p = &pal[i as usize * pe..i as usize * pe + pe];
            match pe {
                2 => {
                    let v = u16::from_le_bytes([p[0], p[1]]);
                    [x5(v >> 11), x6((v >> 5) & 63), x5(v & 31), 255]
                }
                3 => [p[0], p[1], p[2], 255],
                _ => [p[0], p[1], p[2], p[3]],
            }
        })
        .collect()
}

/// Saves them all; writes `index.txt` (hash, size, archive file, slot) next to
/// them so the images can be told apart. Returns how many were saved.
pub fn extract(fs: &mut Fs, out: &Path) -> usize {
    let _ = std::fs::create_dir_all(out);
    let mut index = String::from("# hash            size       file               slot  format\n");
    let mut seen = std::collections::HashSet::new();
    for arc in ARCHIVES {
        let Some(d) = fs.bundle_file(arc) else {
            println!("textures: {arc} not in the .ipa");
            continue;
        };
        for (name, file) in acv_entries(&d) {
            if !name.ends_with(".mcts") {
                continue;
            }
            for (slot, e) in mcts_entries(file).into_iter().enumerate() {
                let Some((fmt, w, h, data)) = mct(e) else {
                    if e.len() >= 44 {
                        println!("textures: {name} #{slot}: unknown layout (format byte {})", e[11]);
                    }
                    continue;
                };
                let key = crate::textures::hash(fmt, w, h, data);
                let pvrtc = fmt == PVRTC_4 || fmt == PVRTC_2;
                index += &format!("{key:016x}  {:<9}  {name:<18} {slot:>4}  {fmt:#x}\n", format!("{w}x{h}"));
                if !seen.insert(key) {
                    continue;
                }
                let mut px = decode(fmt, w, h, data);
                crate::textures::bleed(w as usize, h as usize, &mut px);
                let tag = if pvrtc { "_pvrtc" } else { "" };
                let _ = std::fs::write(out.join(format!("{key:016x}_{w}x{h}{tag}.png")), crate::textures::png_write(w, h, &px));
            }
        }
    }
    let _ = std::fs::write(out.join("index.txt"), index);
    seen.len()
}
