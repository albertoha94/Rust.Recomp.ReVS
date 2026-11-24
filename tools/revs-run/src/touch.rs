//! Touch input: up to 16 fingers, each its own UITouch (as on the device).
//!
//! Finger 0 is the left mouse button and `--tap`; finger 1 is `--hold`;
//! fingers 2.. are the controller and keyboard (input.rs). A point is turned
//! into the game's portrait screen points by undoing the rotation the frame is
//! drawn with, then sent to the view under it as UIKit does:
//! touchesBegan/Moved/Ended:withEvent: with an NSSet of the touch that changed
//! and a UIEvent whose allTouches are every finger down.
//!
//! `--tap=F:x,y` taps at presented frame F, at (x, y) in landscape game
//! points (480 x 320, origin top left), for scripted test runs.
//! `--hold=F1-F2:x,y` keeps finger 1 down from frame F1 to F2;
//! `--hold=F1-F2:x,y>x2,y2` also drags it to (x2, y2) (the virtual stick).

use crate::objc::{self, Msg, NativeImp, Obj};
use crate::{Cpu, Host};

pub const FINGERS: usize = 16;
/// Finger of the mouse and of `--tap`.
pub const MOUSE: usize = 0;
/// Finger of `--hold`.
pub const HOLD: usize = 1;

/// One finger.
#[derive(Default, Clone)]
pub struct Finger {
    /// The UITouch object (made once per finger and reused, like UIKit).
    pub obj: u32,
    pub view: u32,
    /// Window (screen) points, portrait 320 x 480.
    pub loc: (f32, f32),
    pub prev: (f32, f32),
    pub phase: u32,
    pub taps: u32,
    pub time: f64,
    pub down: bool,
    last_up: f64,
}

#[derive(Default)]
pub struct Touch {
    pub fingers: Vec<Finger>,
    pub event: u32,
    /// The NSSet of every finger down, for UIEvent allTouches.
    pub all: u32,
    pub time: f64,
    /// Scripted presses/moves/releases: (frame, finger, phase, landscape x, y).
    pub script: Vec<(u64, usize, u32, f32, f32)>,
    pub log: Vec<String>,
}

/// Window coordinates -> portrait screen points, undoing gfx's rotation.
fn to_screen(h: &Host, wx: f32, wy: f32) -> Option<(f32, f32)> {
    let g = h.gfx.as_ref()?;
    let (px, py, w, ht) = g.window.picture();
    let x = 2.0 * (wx - px) / w - 1.0;
    let y = 1.0 - 2.0 * (wy - py) / ht;
    let t = g.rotation.to_radians();
    let (s, c) = t.sin_cos();
    // Inverse rotation, back to the quad's own coordinates (texture space).
    let p = x * c + y * s;
    let q = -x * s + y * c;
    let (u, v) = ((p + 1.0) / 2.0, (q + 1.0) / 2.0);
    // GL rows go up; UIKit's y goes down.
    Some((u * crate::gfx::SCREEN_W as f32, (1.0 - v) * crate::gfx::SCREEN_H as f32))
}

/// Landscape game point (as the player sees it) -> window coordinates.
pub fn landscape_to_window(h: &Host, x: f32, y: f32) -> Option<(f32, f32)> {
    let g = h.gfx.as_ref()?;
    let (px, py, w, ht) = g.window.picture();
    Some((px + x / 480.0 * w, py + y / 320.0 * ht))
}

/// Window coordinates -> landscape game point (inverse of the above).
pub fn window_to_landscape(h: &Host, wx: f32, wy: f32) -> Option<(f32, f32)> {
    let g = h.gfx.as_ref()?;
    let (px, py, w, ht) = g.window.picture();
    Some(((wx - px) / w * 480.0, (wy - py) / ht * 320.0))
}

/// The view's origin in window points and whether `pt` is inside it.
fn origin_of(h: &Host, v: u32) -> (f32, f32) {
    let (mut x, mut y, mut k) = (0.0, 0.0, v);
    for _ in 0..32 {
        let Some(view) = h.ui.views.get(&k) else { break };
        x += view.frame[0];
        y += view.frame[1];
        k = view.superview;
    }
    (x, y)
}

/// Deepest visible view under `pt` (screen points), from the key window.
fn hit_test(h: &Host, v: u32, pt: (f32, f32)) -> Option<u32> {
    let view = h.ui.views.get(&v)?;
    if view.hidden {
        return None;
    }
    let (ox, oy) = origin_of(h, v);
    let f = view.frame;
    let inside = pt.0 >= ox && pt.1 >= oy && pt.0 < ox + f[2].max(1.0) && pt.1 < oy + f[3].max(1.0);
    if !inside && f[2] > 0.0 {
        return None;
    }
    for &s in view.subviews.iter().rev() {
        if let Some(t) = hit_test(h, s, pt) {
            return Some(t);
        }
    }
    Some(v)
}

/// The view that gets the touch: under the point, then up to one that
/// handles touches itself.
fn target(h: &mut Host, pt: (f32, f32)) -> u32 {
    let root = h.ui.key_window;
    let mut v = hit_test(h, root, pt).unwrap_or(root);
    for _ in 0..32 {
        if v == 0 {
            break;
        }
        if objc::responds(h, v, "touchesBegan:withEvent:") && !is_native_only(h, v) {
            return v;
        }
        v = h.ui.views.get(&v).map_or(0, |x| x.superview);
    }
    // Fall back to the game's GL view.
    h.ui.vc_view.values().next().copied().unwrap_or(root)
}

/// True if the object's class is an Apple class (no game override).
fn is_native_only(h: &Host, o: u32) -> bool {
    h.objc.native.contains_key(&objc::isa(h, o))
}

fn new_set(h: &mut Host, items: Vec<u32>) -> u32 {
    crate::uikit::new_obj(h, "NSSet", Obj::Array(items))
}

/// True if finger `id` is down.
/// A finger the port took counts as down (so a held button presses once).
pub fn is_down(h: &Host, id: usize) -> bool {
    h.touch.fingers.get(id).is_some_and(|f| f.down) || CLAIMED.get(id).is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
}

/// One finger event at a portrait screen point: phase 0 down, 1 move, 3 up.
/// A down on a finger already down, a move or up on a finger that isn't, and
/// a move to the same point are ignored.
pub fn finger(h: &mut Host, c: &mut Cpu, id: usize, phase: u32, pt: (f32, f32)) {
    if id >= FINGERS {
        return;
    }
    // The port's finger: the game never saw it go down.
    if CLAIMED[id].load(std::sync::atomic::Ordering::Relaxed) {
        if phase == 3 {
            CLAIMED[id].store(false, std::sync::atomic::Ordering::Relaxed);
        }
        return;
    }
    if h.touch.fingers.len() < FINGERS {
        h.touch.fingers.resize(FINGERS, Finger::default());
    }
    let now = h.ui.now();
    let f = &h.touch.fingers[id];
    match phase {
        0 if f.down => return,
        1 if !f.down || pt == f.loc => return,
        3 if !f.down => return,
        _ => {}
    }
    if h.touch.fingers[id].obj == 0 {
        let cls = objc::native_class(h, "UITouch");
        h.touch.fingers[id].obj = objc::alloc_instance(h, cls);
    }
    if h.touch.event == 0 {
        let ecls = objc::native_class(h, "UIEvent");
        h.touch.event = objc::alloc_instance(h, ecls);
    }
    if phase == 0 {
        let view = target(h, pt);
        let f = &mut h.touch.fingers[id];
        f.taps = if now - f.last_up < 0.35 { f.taps + 1 } else { 1 };
        f.view = view;
        f.down = true;
        f.prev = pt;
    } else {
        let f = &mut h.touch.fingers[id];
        f.prev = f.loc;
        if phase == 3 {
            f.down = false;
            f.last_up = now;
        }
    }
    let f = &mut h.touch.fingers[id];
    f.loc = pt;
    f.phase = phase;
    f.time = now;
    h.touch.time = now;
    let (obj, view) = (f.obj, f.view);
    // allTouches: every finger down, plus this one while it ends.
    let all: Vec<u32> = h.touch.fingers.iter().filter(|g| g.down || g.obj == obj).map(|g| g.obj).collect();
    h.touch.all = new_set(h, all);
    let set = new_set(h, vec![obj]);
    if h.touch.log.len() < 100 {
        let line = format!(
            "touch {} finger {id} at ({:.0}, {:.0}) -> {}",
            ["began", "moved", "", "ended"][phase as usize],
            pt.0,
            pt.1,
            objc::class_name(h, objc::isa(h, view))
        );
        h.touch.log.push(line);
    }
    let sel = match phase {
        0 => "touchesBegan:withEvent:",
        1 => "touchesMoved:withEvent:",
        3 => "touchesEnded:withEvent:",
        _ => "touchesCancelled:withEvent:",
    };
    if view != 0 {
        let ev = h.touch.event;
        objc::send(h, c, view, sel, &[set, ev]);
    }
}

/// Fingers whose press went to the port's own menus (colorui.rs): their moves
/// and release don't reach the game either.
static CLAIMED: [std::sync::atomic::AtomicBool; FINGERS] = [const { std::sync::atomic::AtomicBool::new(false) }; FINGERS];

/// True if this event is the port's, not the game's: a press the port takes
/// (colorui.rs), and the finger's moves and release after it.
fn claimed(h: &Host, id: usize, phase: u32, at: (f32, f32)) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    let Some(flag) = CLAIMED.get(id) else { return false };
    if phase == 0 && !is_down(h, id) && crate::colorui::press(at) {
        flag.store(true, Relaxed);
    }
    let mine = flag.load(Relaxed);
    if phase == 3 {
        flag.store(false, Relaxed);
    }
    mine
}

/// A finger event at a landscape game point (480 x 320, as the player sees it).
pub fn finger_at(h: &mut Host, c: &mut Cpu, id: usize, phase: u32, x: f32, y: f32) {
    if claimed(h, id, phase, (x, y)) {
        return;
    }
    let Some((wx, wy)) = landscape_to_window(h, x, y) else { return };
    let Some(pt) = to_screen(h, wx, wy) else { return };
    finger(h, c, id, phase, pt);
}

/// One mouse event (left button = finger 0) in window coordinates.
pub fn mouse(h: &mut Host, c: &mut Cpu, phase: u32, wx: f32, wy: f32) {
    let at = window_to_landscape(h, wx, wy).unwrap_or((-1.0, -1.0));
    if claimed(h, MOUSE, phase, at) {
        return;
    }
    let Some(pt) = to_screen(h, wx, wy) else { return };
    finger(h, c, MOUSE, phase, pt);
}

/// Scripted taps and holds: called by the run loop on every pass.
pub fn pump_script(h: &mut Host, c: &mut Cpu) {
    let frame = h.gfx.as_ref().map_or(0, |g| g.frames);
    while let Some(&(f, id, p, x, y)) = h.touch.script.first() {
        if frame < f || c.halt != 0 {
            break;
        }
        h.touch.script.remove(0);
        finger_at(h, c, id, p, x, y);
    }
}

/// Parses `--tap=F:x,y` (finger 0) and `--hold=F1-F2:x,y[>x2,y2]` (finger 1).
/// Each tap is a press at frame F and a release at F + 3.
pub fn parse_taps(args: &[String]) -> Vec<(u64, usize, u32, f32, f32)> {
    let mut v = Vec::new();
    let point = |s: &str| -> Option<(f32, f32)> {
        let (x, y) = s.split_once(',')?;
        Some((x.parse().ok()?, y.parse().ok()?))
    };
    for a in args {
        let Some(r) = a.strip_prefix("--hold=") else { continue };
        let Some((frames, pts)) = r.split_once(':') else { continue };
        let Some((f1, f2)) = frames.split_once('-') else { continue };
        let (Ok(f1), Ok(f2)) = (f1.parse::<u64>(), f2.parse::<u64>()) else { continue };
        let (from, to) = match pts.split_once('>') {
            Some((p1, p2)) => (point(p1), point(p2)),
            None => (point(pts), point(pts)),
        };
        let (Some((x, y)), Some((x2, y2))) = (from, to) else { continue };
        v.push((f1, HOLD, 0, x, y));
        if (x2, y2) != (x, y) {
            // A drag in a few steps, as a finger moves.
            for k in 1..=4u64 {
                let t = k as f32 / 4.0;
                v.push((f1 + 1 + k, HOLD, 1, x + (x2 - x) * t, y + (y2 - y) * t));
            }
        }
        v.push((f2.max(f1 + 6), HOLD, 3, x2, y2));
    }
    for a in args {
        let Some(r) = a.strip_prefix("--tap=") else { continue };
        let Some((f, xy)) = r.split_once(':') else { continue };
        let Some((x, y)) = point(xy) else { continue };
        let Ok(f) = f.parse::<u64>() else { continue };
        v.push((f, MOUSE, 0, x, y));
        v.push((f + 3, MOUSE, 3, x, y));
    }
    v.sort_by_key(|e| e.0);
    v
}

/// The finger whose UITouch is `o`.
fn finger_of(h: &Host, o: u32) -> Finger {
    h.touch.fingers.iter().find(|f| f.obj == o).cloned().unwrap_or_default()
}

/// A CGPoint result: through the stret pointer when the game used
/// objc_msgSend_stret, else in r0:r1.
fn ret_point(h: &Host, c: &mut Cpu, m: &Msg, p: (f32, f32)) -> u32 {
    if m.first == 3 {
        let d = c.r[0];
        h.wr32(d, p.0.to_bits());
        h.wr32(d + 4, p.1.to_bits());
        0
    } else {
        c.r[1] = p.1.to_bits();
        p.0.to_bits()
    }
}

fn ret_dbl(c: &mut Cpu, v: f64) -> u32 {
    let b = v.to_bits();
    c.r[1] = (b >> 32) as u32;
    b as u32
}

/// The point in `view`'s coordinates (nil: the window's).
fn in_view(h: &Host, view: u32, p: (f32, f32)) -> (f32, f32) {
    if view == 0 || view == h.ui.key_window {
        return p;
    }
    let (ox, oy) = origin_of(h, view);
    (p.0 - ox, p.1 - oy)
}

pub fn native_imp(class: &str, meta: bool, s: &str) -> Option<NativeImp> {
    if meta {
        return crate::gamekit::native_imp(class, meta, s);
    }
    Some(match (class, s) {
        ("UITouch", "locationInView:") => |h, c, m| {
            let v = m.arg(h, c, 0);
            let p = in_view(h, v, finger_of(h, m.this).loc);
            ret_point(h, c, m, p)
        },
        ("UITouch", "previousLocationInView:") => |h, c, m| {
            let v = m.arg(h, c, 0);
            let p = in_view(h, v, finger_of(h, m.this).prev);
            ret_point(h, c, m, p)
        },
        ("UITouch", "phase") => |h, _, m| finger_of(h, m.this).phase,
        ("UITouch", "tapCount") => |h, _, m| finger_of(h, m.this).taps,
        ("UITouch", "timestamp") => |h, c, m| {
            let t = finger_of(h, m.this).time;
            ret_dbl(c, t)
        },
        ("UITouch", "view") => |h, _, m| finger_of(h, m.this).view,
        ("UITouch", "window") => |h, _, _| h.ui.key_window,
        ("UIEvent", "allTouches" | "touchesForView:" | "touchesForWindow:") => |h, _, _| h.touch.all,
        ("UIEvent", "timestamp") => |h, c, _| {
            let t = h.touch.time;
            ret_dbl(c, t)
        },
        ("UIEvent", "type") => |_, _, _| 0,
        // NSSet over the same storage as NSArray.
        ("NSSet", "count") => |h, _, m| items(h, m.this).len() as u32,
        ("NSSet", "anyObject") => |h, _, m| items(h, m.this).first().copied().unwrap_or(0),
        ("NSSet", "allObjects") => |h, _, m| {
            let v = items(h, m.this);
            objc::new_array(h, v)
        },
        ("NSSet", "containsObject:" | "member:") => |h, c, m| {
            let o = m.arg(h, c, 0);
            if items(h, m.this).contains(&o) { o.max(1) } else { 0 }
        },
        ("NSSet" | "NSArray", "objectEnumerator") => |h, _, m| {
            let v = items(h, m.this);
            crate::uikit::new_obj(h, "NSEnumerator", Obj::Array(v))
        },
        ("NSEnumerator", "nextObject") => |h, _, m| match h.objc.objs.get_mut(&m.this) {
            Some(Obj::Array(v)) if !v.is_empty() => v.remove(0),
            _ => 0,
        },
        ("NSSet" | "NSArray" | "NSEnumerator", "countByEnumeratingWithState:objects:count:") => |h, c, m| {
            // NSFastEnumerationState: state, itemsPtr, mutationsPtr, extra[5].
            let (st, buf, len) = (m.arg(h, c, 0), m.arg(h, c, 1), m.arg(h, c, 2));
            let v = items(h, m.this);
            let start = h.rd32(st) as usize;
            if start >= v.len() {
                return 0;
            }
            let n = (v.len() - start).min(len as usize);
            for (k, &o) in v[start..start + n].iter().enumerate() {
                h.wr32(buf + 4 * k as u32, o);
            }
            h.wr32(st, (start + n) as u32);
            h.wr32(st + 4, buf);
            // mutationsPtr: any stable word that doesn't change.
            h.wr32(st + 8, st + 12);
            n as u32
        },
        _ => return crate::gamekit::native_imp(class, meta, s),
    })
}

fn items(h: &Host, o: u32) -> Vec<u32> {
    match h.objc.objs.get(&o) {
        Some(Obj::Array(v)) => v.clone(),
        _ => Vec::new(),
    }
}
