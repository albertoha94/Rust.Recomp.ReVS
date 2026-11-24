//! Graphics: the SDL window, one OpenGL ES 1.1 context (touchHLE's
//! GLES1-on-GL2 layer, see gles/) and the game's GL calls.
//!
//! The game's GL arguments come straight from its registers and stack
//! (softfp: a float is passed as its bits in a core register). Guest memory is
//! one block of host memory, so a guest pointer becomes `base + address`; a
//! vertex/index "pointer" is an offset instead while a buffer is bound.

use crate::gles::gles1_on_gl2::GLES1OnGL2Context;
use crate::gles::{gl21compat_raw as gl21, gles11_raw as gles11, GLESContext, GLES};
use crate::window::Window;
use crate::{Cpu, Host};
use std::collections::HashMap;

/// The iPhone screen in points (portrait), and the window scale.
pub const SCREEN_W: u32 = 320;
pub const SCREEN_H: u32 = 480;
pub const SCALE: u32 = 2;

/// Render scale: the game draws into a framebuffer this many times its own
/// 320 x 480 (as touchHLE's --scale-hack), and present() fits that to the
/// window. `--scale=N` (or REVS_RENDER_SCALE=N) sets it; by default it is
/// picked from the desktop size in Gfx::new (see auto_scale).
pub static RENDER_SCALE: std::sync::OnceLock<i32> = std::sync::OnceLock::new();

pub fn render_scale() -> i32 {
    RENDER_SCALE.get().copied().unwrap_or(1)
}

/// Enough scale for a full-screen picture on a `w` x `h` desktop: its 3:2
/// picture height over the game's 320, rounded up (1080p: 4, 1440p: 5,
/// 4K: 7), at most 8.
pub fn auto_scale(w: u32, h: u32) -> i32 {
    let (_, _, _, ph) = crate::window::fit(w, h);
    ((ph / 320.0).ceil() as i32).clamp(1, 8)
}

pub struct Gfx {
    pub window: Window,
    ctx: GLES1OnGL2Context,
    strings: HashMap<u32, u32>,
    present_fbo: u32,
    present_tex: u32,
    /// Rotation (degrees, counter-clockwise) from the portrait renderbuffer to
    /// the landscape window.
    pub rotation: f32,
    pub frames: u64,
    /// Set while the run loop draws an in-between frame (REVS_EXTRA_RENDER):
    /// it is shown but not counted, and not captured.
    pub extra: bool,
    /// In-between frames shown so far.
    pub extra_frames: u64,
    /// Menu pointer to draw (landscape game points), from input.rs.
    pub pointer: Option<(f32, f32)>,
    /// Focused button's rectangle (x, y, w, h, landscape points), from input.rs.
    pub focus: Option<[f32; 4]>,
    /// Controller overlay badges, from input.rs.
    pub badges: Vec<Badge>,
    /// The port's own menu parts (colour bar, lobby chips), from colorui.rs.
    pub ui: Vec<Ui>,
    /// Set by uikit.rs when the next presented frame is saved as a screenshot:
    /// `present` then reads the finished picture before the buffer swap.
    pub capture_next: bool,
    /// That picture: width, height, BGR rows bottom-up.
    captured: Option<(u32, u32, Vec<u8>)>,
}

/// One controller overlay badge: a coloured disc with the button's name.
#[derive(Clone, Copy)]
pub struct Badge {
    /// Centre, landscape game points.
    pub at: (f32, f32),
    pub label: &'static str,
    pub colour: [f32; 3],
}

/// One of the port's own menu parts, in landscape game points.
#[derive(Clone)]
pub enum Ui {
    /// Filled rectangle (x, y, w, h), RGBA.
    Rect([f32; 4], [f32; 4]),
    /// Rectangle outline, RGB, line width in points.
    Frame([f32; 4], [f32; 3], f32),
    /// Disc: centre, radius, RGB, with a rim of this colour.
    Disc((f32, f32), f32, [f32; 3], [f32; 3]),
    /// Filled triangle, RGB.
    Tri([(f32, f32); 3], [f32; 3]),
    /// Text in capitals (5 x 7 font): left end and vertical centre, dot size, RGB.
    Text((f32, f32), String, f32, [f32; 3]),
}

/// 5 x 7 capitals for the port's menu text (rows top down, bit 4 = left).
fn glyph5(ch: char) -> [u8; 7] {
    match ch {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        _ => [0; 7],
    }
}

/// Width of `text` in the 5 x 7 font at dot size `px`.
pub fn text_width(text: &str, px: f32) -> f32 {
    (text.chars().count() as f32 * 6.0 - 1.0).max(0.0) * px
}

unsafe fn draw_ui(items: &[Ui]) {
    use gl21 as gl;
    let quad = |x: f32, y: f32, w: f32, h: f32| {
        for (qx, qy) in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
            let (vx, vy) = ndc(qx, qy);
            gl::Vertex2f(vx, vy);
        }
    };
    for it in items {
        match it {
            Ui::Rect([x, y, w, h], c) => {
                gl::Enable(gl::BLEND);
                gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
                gl::Color4f(c[0], c[1], c[2], c[3]);
                gl::Begin(gl::QUADS);
                quad(*x, *y, *w, *h);
                gl::End();
                gl::Disable(gl::BLEND);
            }
            Ui::Frame([x, y, w, h], c, t) => {
                gl::Color3f(c[0], c[1], c[2]);
                gl::Begin(gl::QUADS);
                quad(*x, *y, *w, *t);
                quad(*x, y + h - t, *w, *t);
                quad(*x, *y, *t, *h);
                quad(x + w - t, *y, *t, *h);
                gl::End();
            }
            Ui::Disc((cx, cy), r, c, rim) => {
                for (rr, col) in [(r + 1.2, rim), (*r, c)] {
                    gl::Color3f(col[0], col[1], col[2]);
                    gl::Begin(gl::TRIANGLE_FAN);
                    let (vx, vy) = ndc(*cx, *cy);
                    gl::Vertex2f(vx, vy);
                    for k in 0..=28 {
                        let a = k as f32 / 28.0 * std::f32::consts::TAU;
                        let (vx, vy) = ndc(cx + rr * a.cos(), cy + rr * a.sin());
                        gl::Vertex2f(vx, vy);
                    }
                    gl::End();
                }
            }
            Ui::Tri(p, c) => {
                gl::Color3f(c[0], c[1], c[2]);
                gl::Begin(gl::TRIANGLES);
                for (x, y) in p {
                    let (vx, vy) = ndc(*x, *y);
                    gl::Vertex2f(vx, vy);
                }
                gl::End();
            }
            Ui::Text((x0, cy), text, px, c) => {
                gl::Color3f(c[0], c[1], c[2]);
                gl::Begin(gl::QUADS);
                let y0 = cy - 3.5 * px;
                for (i, ch) in text.chars().enumerate() {
                    for (row, bits) in glyph5(ch.to_ascii_uppercase()).iter().enumerate() {
                        for col in 0..5 {
                            if bits & (16 >> col) != 0 {
                                quad(x0 + (i as f32 * 6.0 + col as f32) * px, y0 + row as f32 * px, *px, *px);
                            }
                        }
                    }
                }
                gl::End();
            }
        }
    }
}

/// 3 x 5 glyphs for the badge labels (rows top down, bit 2 = left column).
fn glyph(ch: char) -> [u8; 5] {
    match ch {
        'A' => [2, 5, 7, 5, 5],
        'B' => [6, 5, 6, 5, 6],
        'X' => [5, 5, 2, 5, 5],
        'Y' => [5, 5, 2, 2, 2],
        'L' => [4, 4, 4, 4, 7],
        'R' => [6, 5, 6, 5, 5],
        'T' => [7, 2, 2, 2, 2],
        'S' => [3, 4, 2, 1, 6],
        _ => [0; 5],
    }
}

/// Landscape game point -> GL window coordinates (-1..1, y up).
fn ndc(x: f32, y: f32) -> (f32, f32) {
    (x / 480.0 * 2.0 - 1.0, 1.0 - y / 320.0 * 2.0)
}

/// Draws the focus rectangle and the badges (GL state already set up for
/// untextured 2D in window space by `present`).
unsafe fn draw_overlay(focus: Option<[f32; 4]>, badges: &[Badge]) {
    use gl21 as gl;
    if let Some([x, y, w, h]) = focus {
        for (width, r, g, b, grow) in [(5.0, 0.0, 0.0, 0.0, 2.0), (3.0, 1.0, 0.85, 0.2, 2.0)] {
            gl::LineWidth(width);
            gl::Color3f(r, g, b);
            gl::Begin(gl::LINE_LOOP);
            for (px, py) in [(x - grow, y - grow), (x + w + grow, y - grow), (x + w + grow, y + h + grow), (x - grow, y + h + grow)] {
                let (vx, vy) = ndc(px, py);
                gl::Vertex2f(vx, vy);
            }
            gl::End();
        }
    }
    for bd in badges {
        let (cx, cy) = bd.at;
        // Dark rim, then the coloured disc.
        for (r, col) in [(10.0, [0.05, 0.05, 0.05]), (8.5, bd.colour)] {
            gl::Color3f(col[0], col[1], col[2]);
            gl::Begin(gl::TRIANGLE_FAN);
            let (vx, vy) = ndc(cx, cy);
            gl::Vertex2f(vx, vy);
            for k in 0..=24 {
                let a = k as f32 / 24.0 * std::f32::consts::TAU;
                let (vx, vy) = ndc(cx + r * a.cos(), cy + r * a.sin());
                gl::Vertex2f(vx, vy);
            }
            gl::End();
        }
        // The label in white, centred.
        let n = bd.label.chars().count() as f32;
        let px = if n > 1.0 { 1.5 } else { 2.0 };
        let width = (n * 4.0 - 1.0) * px;
        let (x0, y0) = (cx - width / 2.0, cy - 2.5 * px);
        gl::Color3f(1.0, 1.0, 1.0);
        gl::Begin(gl::QUADS);
        for (i, ch) in bd.label.chars().enumerate() {
            for (row, bits) in glyph(ch).iter().enumerate() {
                for col in 0..3 {
                    if bits & (4 >> col) != 0 {
                        let x = x0 + (i as f32 * 4.0 + col as f32) * px;
                        let y = y0 + row as f32 * px;
                        for (qx, qy) in [(x, y), (x + px, y), (x + px, y + px), (x, y + px)] {
                            let (vx, vy) = ndc(qx, qy);
                            gl::Vertex2f(vx, vy);
                        }
                    }
                }
            }
        }
        gl::End();
    }
}

impl Gfx {
    /// Opens the window (landscape: the game's 480 x 320 screen, scaled).
    pub fn new(rotation: f32) -> Result<Gfx, String> {
        // REVS_WINDOW=WxH: another window size (tests of scaled output).
        let (ww, wh) = std::env::var("REVS_WINDOW").ok().and_then(|v| {
            let (a, b) = v.split_once('x')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        }).unwrap_or((SCREEN_H * SCALE, SCREEN_W * SCALE));
        let mut window = Window::new("Resident Evil: The Mercenaries VS. (native)", ww, wh)?;
        let scale = std::env::var("REVS_RENDER_SCALE").ok().and_then(|v| v.parse().ok());
        let scale = RENDER_SCALE.get().copied().or(scale).unwrap_or_else(|| {
            let (dw, dh) = window.desktop_size();
            auto_scale(dw, dh)
        });
        let _ = RENDER_SCALE.set(scale.clamp(1, 8));
        println!("render scale: {}", render_scale());
        let ctx = GLES1OnGL2Context::new(&mut window)?;
        let mut g = Gfx { window, ctx, strings: HashMap::new(), present_fbo: 0, present_tex: 0, rotation, frames: 0, extra: false, extra_frames: 0, pointer: None, focus: None, badges: Vec::new(), ui: Vec::new(), capture_next: false, captured: None };
        let desc = unsafe { g.gl().driver_description() };
        println!("OpenGL: {}", desc.replace('\n', ", "));
        Ok(g)
    }

    pub fn gl(&mut self) -> Box<dyn GLES + '_> {
        self.ctx.make_current(&mut self.window)
    }

    /// Shows the bound renderbuffer in the window (presentRenderbuffer:).
    /// Draws it as a rotated, scaled quad and puts the game's GL state back.
    pub unsafe fn present(&mut self) {
        let (ww, wh) = self.window.drawable_size();
        let rotation = self.rotation;
        let pointer = self.pointer;
        let focus = self.focus;
        let badges = self.badges.clone();
        let ui = self.ui.clone();
        let (mut fbo, mut tex) = (self.present_fbo, self.present_tex);
        {
            let _g = self.gl(); // makes the context current; gl21 calls below use it
            use gl21 as gl;
            // presentRenderbuffer: is not a GL call on iOS, so it must leave no
            // GL error. Keep an error the game left for its own glGetError,
            // and clear the ones this code raises (the driver's glPopAttrib
            // gives GL_INVALID_OPERATION every frame). A leftover error made
            // the game's texture creation fail: Chris's body was never drawn.
            let game_err = gl::GetError();
            let geti = |p: u32| {
                let mut v = 0;
                gl::GetIntegerv(p, &mut v);
                v
            };
            let rb = geti(gl::RENDERBUFFER_BINDING_EXT) as u32;
            let (mut w, mut h) = (0, 0);
            gl::GetRenderbufferParameterivEXT(gl::RENDERBUFFER_EXT, gl::RENDERBUFFER_WIDTH_EXT, &mut w);
            gl::GetRenderbufferParameterivEXT(gl::RENDERBUFFER_EXT, gl::RENDERBUFFER_HEIGHT_EXT, &mut h);
            let old_fb = geti(gl::FRAMEBUFFER_BINDING_EXT) as u32;
            let old_active = geti(gl::ACTIVE_TEXTURE) as u32;
            let old_client = geti(gl::CLIENT_ACTIVE_TEXTURE) as u32;
            let old_array = geti(gl::ARRAY_BUFFER_BINDING) as u32;
            gl::PushAttrib(gl::ALL_ATTRIB_BITS);
            gl::PushClientAttrib(gl::CLIENT_ALL_ATTRIB_BITS);
            gl::ActiveTexture(gl::TEXTURE1);
            gl::Disable(gl::TEXTURE_2D);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::ClientActiveTexture(gl::TEXTURE0);
            let old_tex = geti(gl::TEXTURE_BINDING_2D) as u32;
            if fbo == 0 {
                gl::GenFramebuffersEXT(1, &mut fbo);
                gl::GenTextures(1, &mut tex);
            }
            // Renderbuffer -> texture.
            gl::BindFramebufferEXT(gl::FRAMEBUFFER_EXT, fbo);
            gl::FramebufferRenderbufferEXT(gl::FRAMEBUFFER_EXT, gl::COLOR_ATTACHMENT0_EXT, gl::RENDERBUFFER_EXT, rb);
            gl::BindTexture(gl::TEXTURE_2D, tex);
            gl::CopyTexImage2D(gl::TEXTURE_2D, 0, gl::RGB, 0, 0, w, h, 0);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as _);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as _);
            gl::BindFramebufferEXT(gl::FRAMEBUFFER_EXT, 0);
            // Texture -> window.
            // The 3:2 picture, centred; the black bars come from the Clear
            // below (Clear fills the whole window, not just the viewport).
            let (vx, vy, vw, vh) = crate::window::fit(ww, wh);
            gl::Viewport(vx.round() as _, vy.round() as _, vw.round() as _, vh.round() as _);
            for cap in [
                gl::LIGHTING, gl::DEPTH_TEST, gl::BLEND, gl::CULL_FACE, gl::FOG, gl::ALPHA_TEST, gl::SCISSOR_TEST,
                gl::STENCIL_TEST, gl::COLOR_MATERIAL, gl::MATRIX_PALETTE_ARB,
            ] {
                gl::Disable(cap);
            }
            gl::ColorMask(1, 1, 1, 1);
            gl::Enable(gl::TEXTURE_2D);
            gl::TexEnvi(gl::TEXTURE_ENV, gl::TEXTURE_ENV_MODE, gl::REPLACE as _);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            for mode in [gl::TEXTURE, gl::PROJECTION, gl::MODELVIEW] {
                gl::MatrixMode(mode);
                gl::PushMatrix();
                gl::LoadIdentity();
            }
            gl::Rotatef(rotation, 0.0, 0.0, 1.0);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
            gl::Begin(gl::QUADS);
            for (u, v) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
                gl::TexCoord2f(u, v);
                gl::Vertex2f(u * 2.0 - 1.0, v * 2.0 - 1.0);
            }
            gl::End();
            if !ui.is_empty() {
                gl::LoadIdentity();
                gl::Disable(gl::TEXTURE_2D);
                draw_ui(&ui);
            }
            if focus.is_some() || !badges.is_empty() {
                gl::LoadIdentity();
                gl::Disable(gl::TEXTURE_2D);
                draw_overlay(focus, &badges);
            }
            // The controller's menu pointer: a ring with a dot, in window space.
            if let Some((px, py)) = pointer {
                gl::LoadIdentity();
                gl::Disable(gl::TEXTURE_2D);
                let (cx, cy) = (px / 480.0 * 2.0 - 1.0, 1.0 - py / 320.0 * 2.0);
                let (rx, ry) = (9.0 / 480.0 * 2.0, 9.0 / 320.0 * 2.0);
                for (width, r, g, b) in [(4.0, 0.0, 0.0, 0.0), (2.0, 1.0, 0.85, 0.2)] {
                    gl::LineWidth(width);
                    gl::Color3f(r, g, b);
                    gl::Begin(gl::LINE_LOOP);
                    for k in 0..24 {
                        let a = k as f32 / 24.0 * std::f32::consts::TAU;
                        gl::Vertex2f(cx + rx * a.cos(), cy + ry * a.sin());
                    }
                    gl::End();
                }
                gl::PointSize(4.0);
                gl::Begin(gl::POINTS);
                gl::Vertex2f(cx, cy);
                gl::End();
            }
            for mode in [gl::TEXTURE, gl::PROJECTION, gl::MODELVIEW] {
                gl::MatrixMode(mode);
                gl::PopMatrix();
            }
            gl::BindTexture(gl::TEXTURE_2D, old_tex);
            gl::BindBuffer(gl::ARRAY_BUFFER, old_array);
            gl::PopClientAttrib();
            gl::PopAttrib();
            gl::ActiveTexture(old_active);
            gl::ClientActiveTexture(old_client);
            gl::BindFramebufferEXT(gl::FRAMEBUFFER_EXT, old_fb);
            gl::BindRenderbufferEXT(gl::RENDERBUFFER_EXT, rb);
            for _ in 0..16 {
                if gl::GetError() == 0 {
                    break;
                }
            }
            if game_err != 0 {
                let mut pending = GL_ERR_PENDING.lock().unwrap();
                if *pending == 0 {
                    *pending = game_err;
                }
            }
        }
        self.present_fbo = fbo;
        self.present_tex = tex;
        // A screenshot is read from the back buffer before the swap: what the
        // front buffer holds after a swap is undefined (Mesa on Wayland gives
        // garbage; NVIDIA on Windows happened to keep the frame).
        if self.extra {
            if std::mem::take(&mut self.capture_next) {
                self.captured = Some(self.read_back_buffer(ww, wh));
            }
            self.window.swap_window();
            self.extra_frames += 1;
            return;
        }
        if std::mem::take(&mut self.capture_next) {
            self.captured = Some(self.read_back_buffer(ww, wh));
        }
        self.window.swap_window();
        self.frames += 1;
    }
}

impl Gfx {
    /// Saves what the window shows now as a 24-bit BMP (for checking a run).
    /// The picture is the game's own output: keep it in build/, never share it.
    /// The window's back buffer as BGR rows, bottom-up (call before the swap).
    unsafe fn read_back_buffer(&mut self, w: u32, h: u32) -> (u32, u32, Vec<u8>) {
        let mut px = vec![0u8; (w * h * 3) as usize];
        let _g = self.gl();
        let mut old_fb = 0;
        let mut old_align = 0;
        gl21::GetIntegerv(gl21::FRAMEBUFFER_BINDING_EXT, &mut old_fb);
        gl21::GetIntegerv(gl21::PACK_ALIGNMENT, &mut old_align);
        gl21::BindFramebufferEXT(gl21::FRAMEBUFFER_EXT, 0);
        gl21::ReadBuffer(gl21::BACK);
        gl21::PixelStorei(gl21::PACK_ALIGNMENT, 1);
        gl21::ReadPixels(0, 0, w as _, h as _, gl21::BGR, gl21::UNSIGNED_BYTE, px.as_mut_ptr() as *mut _);
        gl21::PixelStorei(gl21::PACK_ALIGNMENT, old_align);
        gl21::BindFramebufferEXT(gl21::FRAMEBUFFER_EXT, old_fb as u32);
        while gl21::GetError() != 0 {} // leave no error for the game's glGetError
        (w, h, px)
    }

    pub unsafe fn save_window_bmp(&mut self, path: &std::path::Path) -> Result<(), String> {
        let (w, h, px) = self.captured.take().ok_or("no frame was captured for this screenshot")?;
        let row = (w * 3 + 3) & !3;
        let size = 54 + row * h;
        let mut f = Vec::with_capacity(size as usize);
        f.extend_from_slice(b"BM");
        for v in [size, 0, 54, 40, w, h] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&1u16.to_le_bytes());
        f.extend_from_slice(&24u16.to_le_bytes());
        for v in [0u32, row * h, 2835, 2835, 0, 0] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        for y in 0..h {
            let start = (y * w * 3) as usize;
            f.extend_from_slice(&px[start..start + (w * 3) as usize]);
            f.extend(std::iter::repeat(0).take((row - w * 3) as usize));
        }
        std::fs::write(path, f).map_err(|e| e.to_string())
    }
}

/// Client arrays the GL layer implements (vertex, normal, color, texcoord).
const CLIENT_ARRAYS: [u32; 4] = [0x8074, 0x8075, 0x8076, 0x8078];

/// The GL functions the game imports that are handled here.
pub fn is_gl(name: &str) -> bool {
    name.starts_with("gl") && !matches!(name, "glCurrentPaletteMatrixOES" | "glMatrixIndexPointerOES" | "glWeightPointerOES")
}

// Formats glGetIntegerv reports as compressed (as touchHLE: PVRTC + paletted).
const COMPRESSED_FORMATS: &[u32] = &[
    gles11::COMPRESSED_RGBA_PVRTC_2BPPV1_IMG,
    gles11::COMPRESSED_RGBA_PVRTC_4BPPV1_IMG,
    gles11::COMPRESSED_RGB_PVRTC_2BPPV1_IMG,
    gles11::COMPRESSED_RGB_PVRTC_4BPPV1_IMG,
    gles11::PALETTE4_R5_G6_B5_OES,
    gles11::PALETTE4_RGB5_A1_OES,
    gles11::PALETTE4_RGB8_OES,
    gles11::PALETTE4_RGBA4_OES,
    gles11::PALETTE4_RGBA8_OES,
    gles11::PALETTE8_R5_G6_B5_OES,
    gles11::PALETTE8_RGB5_A1_OES,
    gles11::PALETTE8_RGB8_OES,
    gles11::PALETTE8_RGBA4_OES,
    gles11::PALETTE8_RGBA8_OES,
];

/// glGetString answers of the real device (iPod touch 2nd gen, iOS 4.2.1; the
/// same values touchHLE reports).
fn gl_string(name: u32) -> &'static str {
    match name {
        gles11::VENDOR => "Imagination Technologies",
        gles11::RENDERER => "PowerVR MBXLite with VGPLite",
        gles11::VERSION => "OpenGL ES-CM 1.1 (76)",
        _ => "GL_APPLE_framebuffer_multisample GL_APPLE_texture_max_level GL_EXT_discard_framebuffer GL_EXT_texture_filter_anisotropic GL_EXT_texture_lod_bias GL_IMG_read_format GL_IMG_texture_compression_pvrtc GL_IMG_texture_format_BGRA8888 GL_OES_blend_subtract GL_OES_compressed_paletted_texture GL_OES_depth24 GL_OES_draw_texture GL_OES_framebuffer_object GL_OES_mapbuffer GL_OES_matrix_palette GL_OES_point_size_array GL_OES_point_sprite GL_OES_read_format GL_OES_rgb8_rgba8 GL_OES_texture_mirrored_repeat GL_OES_vertex_array_object ",
    }
}

/// touchHLE's workaround: fog with start == end draws nothing on desktop GL.
unsafe fn clamp_fog(g: &mut dyn GLES) -> Option<(f32, f32)> {
    let mut on = 0;
    g.GetBooleanv(gles11::FOG, &mut on);
    if on != 0 {
        let (mut s, mut e) = (0.0, 0.0);
        g.GetFloatv(gles11::FOG_START, &mut s);
        g.GetFloatv(gles11::FOG_END, &mut e);
        if s == e {
            g.Fogf(gles11::FOG_START, e - 0.001);
            return Some((s, e));
        }
    }
    None
}
unsafe fn restore_fog(g: &mut dyn GLES, b: Option<(f32, f32)>) {
    if let Some((s, e)) = b {
        g.Fogf(gles11::FOG_START, s);
        g.Fogf(gles11::FOG_END, e);
    }
}

/// Runs the game's GL call `name`. Returns false if it isn't one handled here.
pub fn call(h: &mut Host, c: &mut Cpu, name: &str) -> bool {
    if !is_gl(name) {
        return false;
    }
    if h.gfx.is_none() {
        if !h.gl_warned {
            h.gl_warned = true;
            println!("{name} called before an EAGLContext exists (ignored)");
        }
        c.r[0] = 0;
        return true;
    }
    let a: [u32; 9] = std::array::from_fn(|i| crate::os::arg(h, c, i as u32));
    let f = |i: usize| f32::from_bits(a[i]);
    let int = |i: usize| a[i] as i32;
    let base = h.base;
    let p = |i: usize| if a[i] == 0 { std::ptr::null_mut() } else { unsafe { base.add(a[i] as usize) as *mut std::ffi::c_void } };
    if name == "glGetString" {
        if let Some(&s) = h.gfx.as_ref().unwrap().strings.get(&a[0]) {
            c.r[0] = s;
        } else {
            let text = gl_string(a[0]);
            let s = crate::os::alloc_zeroed(h, text.len() as u32 + 1);
            for (k, b) in text.bytes().enumerate() {
                h.wr8(s + k as u32, b);
            }
            h.gfx.as_mut().unwrap().strings.insert(a[0], s);
            c.r[0] = s;
        }
        return true;
    }
    let gfx = h.gfx.as_mut().unwrap();
    let frame = gfx.frames;
    let mut gb = gfx.gl();
    let g: &mut dyn GLES = gb.as_mut();
    // Argument `i` as a pointer, or as an offset into the buffer bound at `binding`.
    macro_rules! array_ptr {
        ($i:expr, $binding:expr) => {{
            let mut bound = 0;
            g.GetIntegerv($binding, &mut bound);
            if bound != 0 {
                a[$i] as usize as *const std::ffi::c_void
            } else {
                p($i) as *const std::ffi::c_void
            }
        }};
    }
    let ab = gles11::ARRAY_BUFFER_BINDING;
    let r0: u32 = unsafe {
        match name {
            "glGetError" => match std::mem::take(&mut *GL_ERR_PENDING.lock().unwrap()) {
                0 => g.GetError(),
                e => e,
            },
            // GL_MATRIX_PALETTE_OES: the same calls RE_VS v7-v9 NOP for touchHLE.
            "glEnable" | "glDisable" | "glIsEnabled" if a[0] == 0x8840 => 0,
            "glEnable" => {
                g.Enable(a[0]);
                0
            }
            "glDisable" => {
                g.Disable(a[0]);
                0
            }
            "glEnableClientState" | "glDisableClientState" if !CLIENT_ARRAYS.contains(&a[0]) => {
                // Matrix-palette arrays (skinning) aren't in touchHLE's layer yet: note and skip.
                if h.ui.log.len() < 200 {
                    h.ui.log.push(format!("{name}({:#x}) skipped: array not supported", a[0]));
                }
                0
            }
            "glEnableClientState" => {
                g.EnableClientState(a[0]);
                0
            }
            "glDisableClientState" => {
                g.DisableClientState(a[0]);
                0
            }
            "glClientActiveTexture" => {
                g.ClientActiveTexture(a[0]);
                0
            }
            "glActiveTexture" => {
                g.ActiveTexture(a[0]);
                0
            }
            "glGetIntegerv" => {
                match a[0] {
                    gles11::NUM_COMPRESSED_TEXTURE_FORMATS => *(p(1) as *mut i32) = COMPRESSED_FORMATS.len() as i32,
                    gles11::COMPRESSED_TEXTURE_FORMATS => {
                        for (k, &fmt) in COMPRESSED_FORMATS.iter().enumerate() {
                            *(p(1) as *mut i32).add(k) = fmt as i32;
                        }
                    }
                    0x8cdf | 0x8d57 => *(p(1) as *mut i32) = 1, // MAX_COLOR_ATTACHMENTS, MAX_SAMPLES
                    pname => g.GetIntegerv(pname, p(1) as *mut i32),
                }
                0
            }
            "glHint" => {
                g.Hint(a[0], a[1]);
                0
            }
            "glFlush" => {
                g.Flush();
                0
            }
            "glAlphaFunc" => {
                g.AlphaFunc(a[0], f(1));
                0
            }
            "glBlendFunc" => {
                g.BlendFunc(a[0], a[1]);
                0
            }
            "glColorMask" => {
                g.ColorMask(a[0] as u8, a[1] as u8, a[2] as u8, a[3] as u8);
                0
            }
            "glCullFace" => {
                g.CullFace(a[0]);
                0
            }
            "glDepthFunc" => {
                g.DepthFunc(a[0]);
                0
            }
            "glDepthMask" => {
                g.DepthMask(a[0] as u8);
                0
            }
            "glFrontFace" => {
                g.FrontFace(a[0]);
                0
            }
            "glPolygonOffset" => {
                g.PolygonOffset(f(0), f(1));
                0
            }
            "glShadeModel" => {
                g.ShadeModel(a[0]);
                0
            }
            "glScissor" => {
                let k = render_scale();
                g.Scissor(int(0) * k, int(1) * k, int(2) * k, int(3) * k);
                0
            }
            "glViewport" => {
                let k = render_scale();
                g.Viewport(int(0) * k, int(1) * k, int(2) * k, int(3) * k);
                0
            }
            "glFogf" => {
                g.Fogf(a[0], f(1));
                0
            }
            "glFogfv" => {
                g.Fogfv(a[0], p(1) as *const f32);
                0
            }
            "glLightf" => {
                g.Lightf(a[0], a[1], f(2));
                0
            }
            "glLightfv" => {
                g.Lightfv(a[0], a[1], p(2) as *const f32);
                0
            }
            "glLightModelf" => {
                g.LightModelf(a[0], f(1));
                0
            }
            "glLightModelfv" => {
                g.LightModelfv(a[0], p(1) as *const f32);
                0
            }
            "glMaterialf" => {
                g.Materialf(a[0], a[1], f(2));
                0
            }
            "glMaterialfv" => {
                g.Materialfv(a[0], a[1], p(2) as *const f32);
                0
            }
            // Buffers
            "glGenBuffers" => {
                g.GenBuffers(int(0), p(1) as *mut u32);
                0
            }
            "glDeleteBuffers" => {
                g.DeleteBuffers(int(0), p(1) as *const u32);
                0
            }
            "glBindBuffer" => {
                g.BindBuffer(a[0], a[1]);
                0
            }
            "glBufferData" => {
                g.BufferData(a[0], a[1] as i32 as _, p(2), a[3]);
                0
            }
            "glBufferSubData" => {
                g.BufferSubData(a[0], a[1] as i32 as _, a[2] as i32 as _, p(3));
                0
            }
            // Vertex data
            "glColor4f" => {
                g.Color4f(f(0), f(1), f(2), f(3));
                0
            }
            "glColorPointer" => {
                let q = array_ptr!(3, ab);
                g.ColorPointer(int(0), a[1], int(2), q);
                0
            }
            "glNormalPointer" => {
                let q = array_ptr!(2, ab);
                g.NormalPointer(a[0], int(1), q);
                0
            }
            "glTexCoordPointer" => {
                let q = array_ptr!(3, ab);
                g.TexCoordPointer(int(0), a[1], int(2), q);
                0
            }
            "glVertexPointer" => {
                let q = array_ptr!(3, ab);
                let mut bound = 0;
                g.GetIntegerv(ab, &mut bound);
                *VP_DEBUG.lock().unwrap() = (a[0], a[1], a[2], a[3], bound != 0);
                g.VertexPointer(int(0), a[1], int(2), q);
                0
            }
            "glDrawArrays" => {
                let fog = clamp_fog(g);
                g.DrawArrays(a[0], int(1), int(2));
                restore_fog(g, fog);
                0
            }
            "glDrawElements" => {
                let q = array_ptr!(3, gles11::ELEMENT_ARRAY_BUFFER_BINDING);
                if debug_draws() {
                    let mut eb = 0;
                    g.GetIntegerv(gles11::ELEMENT_ARRAY_BUFFER_BINDING, &mut eb);
                    let (mut mv, mut pr) = ([0f32; 16], [0f32; 16]);
                    g.GetFloatv(gles11::MODELVIEW_MATRIX, mv.as_mut_ptr());
                    g.GetFloatv(gles11::PROJECTION_MATRIX, pr.as_mut_ptr());
                    let mut mode = 0;
                    g.GetIntegerv(gles11::MATRIX_MODE, &mut mode);
                    log_draw(base, a[1], a[2], a[3], eb != 0, &mv, &pr, mode as u32);
                }
                let fog = clamp_fog(g);
                g.DrawElements(a[0], int(1), a[2], q);
                restore_fog(g, fog);
                0
            }
            "glClear" => {
                g.Clear(a[0]);
                0
            }
            "glClearColor" => {
                g.ClearColor(f(0), f(1), f(2), f(3));
                0
            }
            // Textures
            "glPixelStorei" => {
                g.PixelStorei(a[0], int(1));
                0
            }
            "glGenTextures" => {
                g.GenTextures(int(0), p(1) as *mut u32);
                0
            }
            "glDeleteTextures" => {
                crate::textures::on_delete(std::slice::from_raw_parts(p(1) as *const u32, int(0).max(0) as usize));
                g.DeleteTextures(int(0), p(1) as *const u32);
                0
            }
            "glBindTexture" => {
                g.BindTexture(a[0], a[1]);
                0
            }
            "glTexParameteri" => {
                g.TexParameteri(a[0], a[1], crate::textures::filter(a[1], int(2) as f32) as i32);
                0
            }
            "glTexParameterf" => {
                g.TexParameterf(a[0], a[1], crate::textures::filter(a[1], f(2)));
                0
            }
            "glTexImage2D" => {
                g.TexImage2D(a[0], int(1), int(2), int(3), int(4), int(5), a[6], a[7], p(8));
                crate::textures::on_upload(&mut *g, a[0], int(1), a[6], a[7], a[3], a[4], 0, p(8) as *const u8);
                0
            }
            "glTexSubImage2D" => {
                g.TexSubImage2D(a[0], int(1), int(2), int(3), int(4), int(5), a[6], a[7], p(8));
                0
            }
            "glCompressedTexImage2D" => {
                g.CompressedTexImage2D(a[0], int(1), a[2], int(3), int(4), int(5), int(6), p(7));
                crate::textures::on_upload(&mut *g, a[0], int(1), a[2], 0, a[3], a[4], a[6] as usize, p(7) as *const u8);
                0
            }
            "glTexEnvf" => {
                g.TexEnvf(a[0], a[1], f(2));
                0
            }
            "glTexEnvfv" => {
                g.TexEnvfv(a[0], a[1], p(2) as *const f32);
                0
            }
            // Matrices
            "glMatrixMode" => {
                g.MatrixMode(a[0]);
                0
            }
            "glLoadIdentity" => {
                g.LoadIdentity();
                0
            }
            "glLoadMatrixf" => {
                g.LoadMatrixf(p(0) as *const f32);
                0
            }
            "glMultMatrixf" => {
                g.MultMatrixf(p(0) as *const f32);
                0
            }
            "glPushMatrix" => {
                g.PushMatrix();
                0
            }
            "glPopMatrix" => {
                g.PopMatrix();
                0
            }
            "glRotatef" => {
                g.Rotatef(f(0), f(1), f(2), f(3));
                0
            }
            "glScalef" => {
                g.Scalef(f(0), f(1), f(2));
                0
            }
            "glTranslatef" => {
                g.Translatef(f(0), f(1), f(2));
                0
            }
            // Framebuffers (OES_framebuffer_object)
            "glGenFramebuffersOES" => {
                g.GenFramebuffersOES(int(0), p(1) as *mut u32);
                0
            }
            "glGenRenderbuffersOES" => {
                g.GenRenderbuffersOES(int(0), p(1) as *mut u32);
                0
            }
            "glDeleteFramebuffersOES" => {
                g.DeleteFramebuffersOES(int(0), p(1) as *const u32);
                0
            }
            "glDeleteRenderbuffersOES" => {
                g.DeleteRenderbuffersOES(int(0), p(1) as *const u32);
                0
            }
            "glBindFramebufferOES" => {
                g.BindFramebufferOES(a[0], a[1]);
                0
            }
            "glBindRenderbufferOES" => {
                g.BindRenderbufferOES(a[0], a[1]);
                0
            }
            "glRenderbufferStorageOES" => {
                g.RenderbufferStorageOES(a[0], a[1], int(2) * render_scale(), int(3) * render_scale());
                0
            }
            "glFramebufferRenderbufferOES" => {
                g.FramebufferRenderbufferOES(a[0], a[1], a[2], a[3]);
                0
            }
            "glFramebufferTexture2DOES" => {
                g.FramebufferTexture2DOES(a[0], a[1], a[2], a[3], int(4));
                0
            }
            "glGetRenderbufferParameterivOES" => {
                g.GetRenderbufferParameterivOES(a[0], a[1], p(2) as *mut i32);
                // The game sees its own size (WIDTH / HEIGHT), not the scaled one.
                if a[1] == 0x8d42 || a[1] == 0x8d43 {
                    *(p(2) as *mut i32) /= render_scale();
                }
                0
            }
            "glCheckFramebufferStatusOES" => g.CheckFramebufferStatusOES(a[0]),
            _ => return false,
        }
    };
    if debug_errors() && name != "glGetError" {
        let e = unsafe { g.GetError() };
        if e != 0 {
            println!("glerr f{} {name}({:#x}, {:#x}, {:#x}, {:#x}) -> {e:#x} lr={:#x}", frame, a[0], a[1], a[2], a[3], c.r[14]);
            let mut pending = GL_ERR_PENDING.lock().unwrap();
            if *pending == 0 {
                *pending = e;
            }
        }
    }
    c.r[0] = r0;
    true
}

// A GL error the game raised and has not read yet, kept here when host GL work
// (present, the REVS_GLERR check) had to read it; glGetError returns it first.
// REVS_GLERR=1 prints each game GL call that raises a GL error (frame, name,
// first arguments, return address); the error stays pending, so the run
// behaves the same.
static GL_ERR_PENDING: std::sync::Mutex<u32> = std::sync::Mutex::new(0);

fn debug_errors() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("REVS_GLERR").is_some())
}

// ---- REVS_GLDEBUG=1: one line per distinct draw (vertex pointer, count) with
// the bounding box of the vertices its indices use, to find broken geometry.
static VP_DEBUG: std::sync::Mutex<(u32, u32, u32, u32, bool)> = std::sync::Mutex::new((0, 0, 0, 0, false));
static DRAWS_SEEN: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());

fn debug_draws() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("REVS_GLDEBUG").is_some())
}

#[allow(clippy::too_many_arguments)]
fn log_draw(base: *mut u8, count: u32, ty: u32, idx: u32, idx_in_buffer: bool, mv: &[f32; 16], pr: &[f32; 16], mode: u32) {
    let (size, vty, stride, vaddr, v_in_buffer) = *VP_DEBUG.lock().unwrap();
    {
        let mut seen = DRAWS_SEEN.lock().unwrap();
        if seen.len() > 400 || seen.contains(&(vaddr, count)) {
            return;
        }
        seen.push((vaddr, count));
    }
    if idx_in_buffer || v_in_buffer || vty != gles11::FLOAT {
        println!("draw: {count} indices, vertices {size}x{vty:#x} at {vaddr:#x} (buffer {v_in_buffer}, indices in buffer {idx_in_buffer})");
        return;
    }
    let step = if stride == 0 { size * 4 } else { stride };
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    let (mut bad, mut maxi) = (0, 0);
    for k in 0..count {
        let i = unsafe {
            if ty == gles11::UNSIGNED_SHORT { *(base.add((idx + 2 * k) as usize) as *const u16) as u32 } else { *base.add((idx + k) as usize) as u32 }
        };
        maxi = maxi.max(i);
        for d in 0..3.min(size) {
            let v = unsafe { *(base.add((vaddr + i * step + 4 * d) as usize) as *const f32) };
            if !v.is_finite() || v.abs() > 1e6 {
                bad += 1;
            } else {
                lo[d as usize] = lo[d as usize].min(v);
                hi[d as usize] = hi[d as usize].max(v);
            }
        }
    }
    // Box centre through modelview then projection (column-major), to window -1..1.
    let cpos = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0, 1.0];
    let mul = |m: &[f32; 16], v: [f32; 4]| -> [f32; 4] { std::array::from_fn(|r| (0..4).map(|k| m[k * 4 + r] * v[k]).sum()) };
    let eye = mul(mv, cpos);
    let clip = mul(pr, eye);
    println!(
        "draw: {count} indices (max index {maxi}), vertices {size} floats stride {stride} at {vaddr:#x}: box {lo:?}..{hi:?}, {bad} bad values; eye {:?} ndc ({:.2}, {:.2}, {:.2}) mode {mode:#x}",
        &eye[..3], clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]
    );
}
