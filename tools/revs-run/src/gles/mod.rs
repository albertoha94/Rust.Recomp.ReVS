//! OpenGL ES 1.1 for the native runner.
//!
//! `gles_generic.rs`, `gles1_on_gl2.rs` and `util.rs` are copied from touchHLE
//! (MPL-2.0, see their headers): OpenGL ES 1.1 implemented on a desktop OpenGL
//! 2.1 compatibility context. The only change is where they find the window
//! and the PVRTC decoder. `../gfx.rs` connects the game's GL calls to them.
#![allow(non_snake_case, non_upper_case_globals, clippy::upper_case_acronyms)]


macro_rules! log {
    ($($arg:tt)*) => { eprintln!("GL: {}", format!($($arg)*)) };
}
macro_rules! log_dbg {
    ($($arg:tt)*) => { if false { eprintln!("GL: {}", format!($($arg)*)) } };
}

#[allow(dead_code, non_upper_case_globals, non_snake_case, unused, clippy::all)]
pub mod gl21compat_raw {
    include!(concat!(env!("OUT_DIR"), "/gl21compat.rs"));
}
#[allow(dead_code, non_upper_case_globals, non_snake_case, unused, clippy::all)]
pub mod gles11_raw {
    include!(concat!(env!("OUT_DIR"), "/gles11.rs"));
}

#[allow(dead_code, unused, clippy::all)]
pub mod gles1_on_gl2;
#[allow(dead_code, unused, clippy::all)]
mod gles_generic;
#[allow(dead_code, unused, clippy::all)]
mod util;

pub use gles_generic::{GLESContext, GLES};

extern "C" {
    fn revs_decompress_pvrtc(data: *const u8, is_2bit: bool, width: u32, height: u32, rgba: *mut u8) -> u32;
}

/// PVRTC (2 or 4 bits per pixel) to RGBA8, one u32 per pixel.
pub fn decode_pvrtc(data: &[u8], is_2bit: bool, width: u32, height: u32) -> Vec<u32> {
    let expected = if is_2bit {
        (width.max(16) as usize * height.max(8) as usize * 2).div_ceil(8)
    } else {
        (width.max(8) as usize * height.max(8) as usize * 4).div_ceil(8)
    };
    assert_eq!(data.len(), expected, "PVRTC data size");
    let mut out = vec![0u32; width as usize * height as usize];
    unsafe { revs_decompress_pvrtc(data.as_ptr(), is_2bit, width, height, out.as_mut_ptr() as *mut u8) };
    out
}
