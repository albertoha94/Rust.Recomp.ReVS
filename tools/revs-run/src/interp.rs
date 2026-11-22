//! REVS_INTERP: frames drawn between the game's 20 Hz steps get a blended
//! camera and blended model positions, so the picture moves at the display's
//! rate while the game itself still runs 20 steps a second.
//!
//! Camera: the game builds its view matrix during the step
//! (`GM_CtrlMain_Camera::Main`) and only hands it to the 3D engine while
//! drawing: `ERP_Camera::Set_G3D()` calls `_mceGraphics3D_setCamera(g3d,
//! camera, &erp_camera[0x44])`, and the engine copies the matrix.
//!
//! Models: every character, weapon and prop goes through
//! `_mceGraphics3D_drawFigure(g3d, figure, transform*, flags)` with a
//! world-space transform built while drawing.
//!
//! Animation: a model's animation frame is set while drawing too
//! (`_mceAnimatable_setFrame`, `_mceActionController_setFrameAll`), with a
//! float that advances 1.0 per step; in-between frames get the fraction.
//!
//! In all cases the call's argument is pointed at a blended copy; the
//! game's own matrices are never written, so its logic can't change.
//!
//! - REVS_INTERP=1 (delay): every frame shows the blend between the previous
//!   and the current step. Always right, but the picture is one step (50 ms)
//!   behind the controls.
//! - REVS_INTERP=2 (predict): frames go on past the current step in the
//!   direction things were moving. No added lag; overshoots a little when
//!   something stops or turns.
//!
//! In-between frames come from REVS_EXTRA_RENDER (uikit.rs); without it every
//! drawn frame is a step frame and only the delay is seen.

use crate::{Cpu, Host};
use std::collections::HashMap;
use std::sync::Mutex;

/// `ERP_Camera::Set_G3D()`.
pub const SET_G3D: u32 = 0x187d0;
/// `_mceGraphics3D_setCamera(g3d, camera, transform*)`.
pub const SET_CAMERA: u32 = 0x2f9644;
/// `_mceGraphics3D_drawFigure(g3d, figure, transform*, flags)`.
pub const DRAW_FIGURE: u32 = 0x2f9f18;
/// `GM_Element_Model::Direct_Draw(tagTransform*)` and `::Draw()`: every model
/// draw goes through one of them; their caller says what is drawn.
pub const MODEL_DIRECT_DRAW: u32 = 0x16428c;
pub const MODEL_DRAW: u32 = 0x164a20;
/// Draws attached to a holder: these get their matrix from the holder's bones
/// as the engine posed them, i.e. from the already blended holder, so they are
/// drawn as they come (blending them again made held weapons jitter).
/// Measured: only weapon draws changed between two in-between frames of a step.
/// `GM_Obj_Eqp_Weapon::Draw_Direct`, `GM_Obj_Eqp_Plaga::Draw_Direct`,
/// `GM_Hdl_EffBase::Draw_Direct` (all `(tagTransform* holder)`), and
/// `GM_CtrlSub_LaserSite::Draw` (the laser dot, placed each frame from the
/// held gun by Set_LaserParam): start, size.
const ATTACHED: [(u32, u32); 4] = [(0xf62e0, 428), (0xf567c, 784), (0x108b2c, 112), (0x15a210, 448)];
/// `_mceAnimatable_setFrame(animatable, float frame)`.
pub const SET_FRAME: u32 = 0x30e118;
/// `_mceActionController_setFrameAll(controller, float frame, ...)`.
pub const SET_FRAME_ALL: u32 = 0x2ea628;
/// Return addresses of those two in `GM_Element_Model::Draw()` and
/// `GM_Element_Model::Set_DrawActions()`: a model's animation frame, set while
/// drawing. (The engine's own calls of setFrame pass bones' derived frames.)
const MODEL_SET_FRAME_LR: u32 = 0x164bb4;
const MODEL_SET_FRAME_ALL_LR: u32 = 0x1641f8;
/// Largest animation advance per step that is blended. Animations are made at
/// 20 frames a second: a playing one advances 1.0 per step.
const MAX_FRAME_STEP: f32 = 2.0;
/// Return address in `GM_CtrlMain_Camera::Draw_Fog()` after its Set_G3D call:
/// the one made while drawing a frame.
const DRAW_FOG_LR: u32 = 0x1188a0;
/// Return address in Set_G3D after its setCamera call.
const SET_G3D_LR: u32 = 0x187ec;
/// Blended model matrices one frame can hold (a Training frame draws ~20).
const SLOTS: u32 = 4096;

/// The engine's transform (tagTransform): three axis vectors of 4 floats (the
/// 4th is 0; their length is the model's scale), then the position at floats
/// 12-14 and a 4th float. `ERP_CalcMtx::Set_Translate` writes the position at
/// +0x30; for the camera that is +0x74, where `ERP_Camera::Set_ViewTrans`
/// puts the eye. Positions are world space, for models and camera alike.
type Mtx = [f32; 16];

struct State {
    /// 0 off, 1 delay, 2 predict.
    mode: u8,
    /// The view matrix of the step before the current one, and of the current one.
    prev: Option<Mtx>,
    cur: Option<Mtx>,
    /// Model matrices drawn in the previous step's frame and in this one, by figure.
    figs_prev: HashMap<u32, Vec<Mtx>>,
    figs_cur: HashMap<u32, Vec<Mtx>>,
    /// Animation frames set in the previous step's frame and in this one, by
    /// (function, animatable or controller).
    anim_prev: HashMap<(u32, u32), f32>,
    anim_cur: HashMap<(u32, u32), f32>,
    /// The frame being drawn is the step's own (its draws are recorded).
    step_frame: bool,
    /// Where the frame being drawn sits: 0 = prev, 1 = cur, more = predicted.
    alpha: f32,
    /// The next setCamera is the drawing one (Draw_Fog's Set_G3D).
    draw_next: bool,
    /// Guest memory: the camera matrix, then SLOTS model matrices.
    scratch: u32,
    /// Next free model slot in this frame.
    slot: u32,
    /// Steps where the camera jumped (a cut): drawn without blending.
    cuts: u64,
    /// Frames drawn with a blended camera; model draws blended and not.
    blended: u64,
    models: u64,
    models_plain: u64,
    anims: u64,
    /// Return address of the current Direct_Draw / Draw call (who draws it).
    caller: u32,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Turns blending on (crate::interp_mode). Returns the addresses rc_trace
/// must hook (none if off).
pub fn setup(h: &mut Host) -> Vec<u32> {
    let mode = crate::interp_mode();
    if mode == 0 {
        return Vec::new();
    }
    let scratch = crate::os::alloc_zeroed(h, 64 * (SLOTS + 1));
    *STATE.lock().unwrap() = Some(State {
        mode: mode.min(2),
        prev: None,
        cur: None,
        figs_prev: HashMap::new(),
        figs_cur: HashMap::new(),
        anim_prev: HashMap::new(),
        anim_cur: HashMap::new(),
        step_frame: false,
        alpha: 1.0,
        draw_next: false,
        scratch,
        slot: 0,
        cuts: 0,
        blended: 0,
        models: 0,
        models_plain: 0,
        anims: 0,
        caller: 0,
    });
    h.ui.log.push(format!("interp: blending on ({})", if mode == 1 { "delay" } else { "predict" }));
    vec![SET_G3D, SET_CAMERA, DRAW_FIGURE, SET_FRAME, SET_FRAME_ALL, MODEL_DIRECT_DRAW, MODEL_DRAW]
}

fn read(h: &Host, p: u32) -> Mtx {
    std::array::from_fn(|i| f32::from_bits(h.rd32(p + 4 * i as u32)))
}

fn write(h: &Host, p: u32, m: &Mtx) {
    for (i, v) in m.iter().enumerate() {
        h.wr32(p + 4 * i as u32, v.to_bits());
    }
}

/// Called by rc_trace at the entry of a hooked function.
pub fn on_enter(h: &Host, c: &mut Cpu, addr: u32) {
    let mut g = STATE.lock().unwrap();
    let Some(s) = g.as_mut() else { return };
    match addr {
        SET_G3D => s.draw_next = c.r[14] == DRAW_FOG_LR,
        MODEL_DIRECT_DRAW | MODEL_DRAW => s.caller = c.r[14],
        SET_CAMERA if c.r[14] == SET_G3D_LR && c.r[2] != 0 => {
            let m = read(h, c.r[2]);
            if !std::mem::take(&mut s.draw_next) {
                // Set by the step's logic: the new current camera.
                s.cur = Some(m);
                return;
            }
            let (Some(prev), Some(cur)) = (s.prev, s.cur) else { return };
            if m != cur {
                return; // not the camera this module follows
            }
            write(h, s.scratch, &blend(&prev, &cur, s.alpha));
            c.r[2] = s.scratch;
            s.blended += 1;
        }
        DRAW_FIGURE if c.r[2] != 0 => {
            let (fig, m) = (c.r[1], read(h, c.r[2]));
            if s.step_frame {
                s.figs_cur.entry(fig).or_default().push(m);
            }
            // The same figure can be drawn several times (enemies share one),
            // and draws come and go with culling: pair with the nearest draw
            // of this figure in the previous step.
            let near = s.figs_prev.get(&fig).and_then(|v| {
                v.iter().map(|p| (change(p, &m).1, p)).min_by(|a, b| a.0.total_cmp(&b.0)).map(|x| *x.1)
            });
            // REVS_INTERP_DRAWS=F or F-G: every model draw while steps F to G are shown.
            static DRAWS: std::sync::OnceLock<Option<(u64, u64)>> = std::sync::OnceLock::new();
            let range = *DRAWS.get_or_init(|| {
                let v = std::env::var("REVS_INTERP_DRAWS").ok()?;
                let (a, b) = v.split_once('-').unwrap_or((&v, &v));
                Some((a.parse().ok()?, b.parse().ok()?))
            });
            let attached = ATTACHED.iter().any(|&(a, n)| (a..a + n).contains(&s.caller));
            let frame = h.gfx.as_ref().map_or(0, |g| g.frames);
            if range.is_some_and(|(a, b)| (a..=b).contains(&frame)) {
                let o = if attached { None } else { near.map(|p| blend(&p, &m, s.alpha)) };
                println!(
                    "draw f{frame} a={:.2} step={} caller={:#x} fig={fig:#x} m=({:.2},{:.2},{:.2}) prev={:?} out={:?} axis0={:?}",
                    s.alpha, s.step_frame, s.caller, m[12], m[13], m[14],
                    near.map(|p| (p[12], p[13], p[14], change(&p, &m))),
                    o.map(|o| (o[12], o[13], o[14])), o.map(|o| axis(&o, 0))
                );
            }
            match near {
                Some(p) if !is_cut(&p, &m) && !attached => {
                    // The game passes a matrix it has just built on its stack
                    // for this one draw: blend it in place, so the engine (which
                    // keeps the pointer and reads it again, e.g. to place a
                    // held weapon from the hand) sees what the original game
                    // would. Any other matrix may be game state: use a copy.
                    let on_stack = c.r[2] >= c.r[13] && c.r[2] - c.r[13] < 0x10_0000;
                    if on_stack {
                        write(h, c.r[2], &blend(&p, &m, s.alpha));
                    } else {
                        let at = s.scratch + 64 * (1 + s.slot);
                        s.slot = (s.slot + 1) % SLOTS;
                        write(h, at, &blend(&p, &m, s.alpha));
                        c.r[2] = at;
                    }
                    s.models += 1;
                }
                _ => s.models_plain += 1,
            }
        }
        SET_FRAME | SET_FRAME_ALL
            if c.r[14] == if addr == SET_FRAME { MODEL_SET_FRAME_LR } else { MODEL_SET_FRAME_ALL_LR } =>
        {
            let key = (addr, c.r[0]);
            let f = f32::from_bits(c.r[1]);
            if s.step_frame {
                s.anim_cur.insert(key, f);
            }
            if let Some(&p) = s.anim_prev.get(&key) {
                if f > p && f - p <= MAX_FRAME_STEP {
                    c.r[1] = (p + (f - p) * s.alpha).to_bits();
                    s.anims += 1;
                }
            }
        }
        _ => {}
    }
}

/// Before a game step (onTick): the current camera and models become the
/// previous ones, and the step's own frame is drawn at 0 (delay) or 1 (predict).
pub fn begin_step() {
    let mut g = STATE.lock().unwrap();
    let Some(s) = g.as_mut() else { return };
    s.prev = s.cur;
    // A step that drew nothing (none happened yet) keeps the last models.
    if !s.figs_cur.is_empty() {
        s.figs_prev = std::mem::take(&mut s.figs_cur);
        s.anim_prev = std::mem::take(&mut s.anim_cur);
    }
    s.alpha = if s.mode == 1 { 0.0 } else { 1.0 };
    s.step_frame = true;
}

/// After the step: a camera cut (new scene, camera mode change) is not
/// blended across.
pub fn end_step() {
    let mut g = STATE.lock().unwrap();
    let Some(s) = g.as_mut() else { return };
    s.step_frame = false;
    // REVS_INTERP_LOG=1: how far the camera moves and turns each step.
    if std::env::var_os("REVS_INTERP_LOG").is_some() {
        if let (Some(p), Some(c)) = (s.prev, s.cur) {
            let (turn, moved) = change(&p, &c);
            println!("interp step: moved {moved:.3} cos {turn:.5} m={c:.3?}");
        }
    }
    if let (Some(p), Some(c)) = (s.prev, s.cur) {
        if is_cut(&p, &c) {
            s.prev = s.cur;
            s.cuts += 1;
        }
    }
}

/// For in-between frame `k` of `n`: where it sits between the steps.
pub fn set_between(k: u32, n: u32) {
    let mut g = STATE.lock().unwrap();
    let Some(s) = g.as_mut() else { return };
    let f = k as f32 / (n + 1) as f32;
    s.alpha = if s.mode == 1 { f } else { 1.0 + f };
}

pub fn report() -> Option<String> {
    let g = STATE.lock().unwrap();
    let s = g.as_ref()?;
    Some(format!(
        "interp: {} frames with a blended camera ({} cuts not blended); model draws {} blended, {} not; {} animation frames blended",
        s.blended, s.cuts, s.models, s.models_plain, s.anims
    ))
}

type V3 = [f32; 3];

fn axis(m: &Mtx, r: usize) -> V3 {
    [m[4 * r], m[4 * r + 1], m[4 * r + 2]]
}

fn dot(x: V3, y: V3) -> f32 {
    x[0] * y[0] + x[1] * y[1] + x[2] * y[2]
}

fn len(x: V3) -> f32 {
    dot(x, x).sqrt()
}

fn scaled(x: V3, k: f32) -> V3 {
    [x[0] * k, x[1] * k, x[2] * k]
}

/// Element-wise blend, then the axes made perpendicular again with their
/// blended lengths (a plain blend of two rotations shrinks and skews slightly;
/// the lengths are the model's scale and must stay).
fn blend(a: &Mtx, b: &Mtx, t: f32) -> Mtx {
    let mut m: Mtx = std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t);
    let lens: Vec<f32> = (0..3).map(|r| len(axis(a, r)) + (len(axis(b, r)) - len(axis(a, r))) * t).collect();
    let unit = |x: V3| scaled(x, 1.0 / len(x).max(1e-12));
    let r0 = unit(axis(&m, 0));
    let r1 = axis(&m, 1);
    let r1 = unit([r1[0] - dot(r1, r0) * r0[0], r1[1] - dot(r1, r0) * r0[1], r1[2] - dot(r1, r0) * r0[2]]);
    // Third axis: the cross product, on the side of the original (a mirrored
    // model keeps its handedness).
    let mut r2 = [r0[1] * r1[2] - r0[2] * r1[1], r0[2] * r1[0] - r0[0] * r1[2], r0[0] * r1[1] - r0[1] * r1[0]];
    if dot(r2, axis(&m, 2)) < 0.0 {
        r2 = scaled(r2, -1.0);
    }
    for (r, v) in [r0, r1, r2].iter().enumerate() {
        m[4 * r..4 * r + 3].copy_from_slice(&scaled(*v, lens[r]));
    }
    m
}

/// Smallest cosine between matching axes (1 = no turn), and how far the
/// position moved.
fn change(a: &Mtx, b: &Mtx) -> (f32, f32) {
    let turn = (0..3)
        .map(|r| dot(axis(a, r), axis(b, r)) / (len(axis(a, r)) * len(axis(b, r))).max(1e-12))
        .fold(1.0f32, f32::min);
    let moved = len([a[12] - b[12], a[13] - b[13], a[14] - b[14]]);
    (turn, moved)
}

/// A jump nothing makes in one 50 ms step: more than about 25 degrees of turn
/// (cosine under 0.9), or more than MAX_MOVE. Drawn as is, not blended.
fn is_cut(a: &Mtx, b: &Mtx) -> bool {
    let (turn, moved) = change(a, b);
    turn < 0.9 || moved > MAX_MOVE
}

/// Largest move per step that is still blended (in the game's units).
/// Measured in Training: walking moves the camera about 2.7 per step, a fast
/// swing up to 16; the jump to the start of a match was 465.
const MAX_MOVE: f32 = 50.0;
