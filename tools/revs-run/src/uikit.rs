//! UIKit, Core Animation, EAGL and Foundation pieces the game uses, as native
//! methods (looked up after the game's own methods, see objc.rs), plus the
//! run loop that fires the game's timers.
//!
//! Screen model: an iPhone in portrait, 320 x 480 points at scale 1. The game
//! draws its landscape picture rotated into that; gfx.rs turns it back.

use crate::objc::{self, isa, send, string_of, Msg, NativeImp, Obj};
use crate::{gfx, Cpu, Host};
use std::collections::HashMap;
use std::time::Instant;

pub struct View {
    pub frame: [f32; 4],
    pub layer: u32,
    pub subviews: Vec<u32>,
    pub superview: u32,
    pub hidden: bool,
}

pub struct Layer {
    pub view: u32,
    pub props: u32,
    pub opaque: bool,
}

pub struct Timer {
    pub obj: u32,
    pub interval: f64,
    pub target: u32,
    pub sel: u32,
    pub info: u32,
    pub repeats: bool,
    pub scheduled: bool,
    pub valid: bool,
    pub next: f64,
}

pub struct Observer {
    pub observer: u32,
    pub sel: u32,
    pub name: Option<String>,
    pub object: u32,
}

pub struct Ui {
    pub views: HashMap<u32, View>,
    pub layers: HashMap<u32, Layer>,
    pub vc_view: HashMap<u32, u32>,
    pub timers: Vec<Timer>,
    pub observers: Vec<Observer>,
    pub singletons: HashMap<&'static str, u32>,
    pub current_ctx: u32,
    pub key_window: u32,
    pub start: Instant,
    /// Stop after this many presented frames (--frames=N).
    pub max_frames: Option<u64>,
    /// Save the last presented frame here as a BMP (--screenshot=path).
    pub screenshot: Option<std::path::PathBuf>,
    /// Frames also saved, next to `screenshot` as <stem>_F.bmp (--shots=).
    pub shots: Vec<u64>,
    pub stop: bool,
    /// REVS_EXTRA_RENDER: times of the in-between Render() calls still to do.
    /// (time, k of n) of the in-between Render() calls still to do.
    pub extra_at: Vec<(f64, u32, u32)>,
    pub log: Vec<String>,
}

impl Ui {
    pub fn new() -> Ui {
        Ui {
            views: HashMap::new(),
            layers: HashMap::new(),
            vc_view: HashMap::new(),
            timers: Vec::new(),
            observers: Vec::new(),
            singletons: HashMap::new(),
            current_ctx: 0,
            key_window: 0,
            start: Instant::now(),
            max_frames: None,
            screenshot: None,
            shots: Vec::new(),
            stop: false,
            extra_at: Vec::new(),
            log: Vec::new(),
        }
    }
    pub fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

// ---------------------------------------------------------------- helpers

fn fl(h: &Host, c: &Cpu, m: &Msg, i: u32) -> f32 {
    f32::from_bits(m.arg(h, c, i))
}

fn dbl(h: &Host, c: &Cpu, m: &Msg, i: u32) -> f64 {
    // iOS passes a double in the next two words (no even-word alignment).
    let w = m.first + i;
    f64::from_bits(crate::os::arg(h, c, w) as u64 | (crate::os::arg(h, c, w + 1) as u64) << 32)
}

/// Returns a double in r0:r1 (the caller only sets r0 from the result).
fn ret_dbl(c: &mut Cpu, v: f64) -> u32 {
    let b = v.to_bits();
    c.r[1] = (b >> 32) as u32;
    b as u32
}

/// Writes a struct result (CGRect, CGPoint, CGSize) to the stret pointer.
fn ret_floats(h: &Host, c: &Cpu, v: &[f32]) -> u32 {
    let dest = c.r[0];
    for (k, x) in v.iter().enumerate() {
        h.wr32(dest + 4 * k as u32, x.to_bits());
    }
    0
}

pub(crate) fn singleton(h: &mut Host, class: &'static str) -> u32 {
    if let Some(&o) = h.ui.singletons.get(class) {
        return o;
    }
    let cls = objc::native_class(h, class);
    let o = objc::alloc_instance(h, cls);
    h.ui.singletons.insert(class, o);
    o
}

pub(crate) fn new_obj(h: &mut Host, class: &str, state: Obj) -> u32 {
    let cls = objc::native_class(h, class);
    let o = objc::alloc_instance(h, cls);
    h.objc.objs.insert(o, state);
    o
}

fn number(h: &Host, o: u32) -> f64 {
    match h.objc.objs.get(&o) {
        Some(Obj::Number(v)) => *v,
        _ => 0.0,
    }
}

/// Seconds since 2001-01-01 (NSDate's reference date).
fn reference_now() -> f64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    unix - 978_307_200.0
}

fn keys_equal(h: &Host, a: u32, b: u32) -> bool {
    a == b || (string_of(h, a).is_some() && string_of(h, a) == string_of(h, b))
}

/// Sets up a view's state (frame, layer from +layerClass).
fn setup_view(h: &mut Host, c: &mut Cpu, this: u32, frame: [f32; 4]) {
    let cls = isa(h, this);
    let layer_cls = send(h, c, cls, "layerClass", &[]);
    let layer_cls = if layer_cls == 0 { objc::native_class(h, "CALayer") } else { layer_cls };
    let layer = objc::alloc_instance(h, layer_cls);
    h.ui.layers.insert(layer, Layer { view: this, props: 0, opaque: false });
    h.ui.views.insert(this, View { frame, layer, subviews: Vec::new(), superview: 0, hidden: false });
}

fn frame_of(h: &Host, v: u32) -> [f32; 4] {
    h.ui.views.get(&v).map_or([0.0; 4], |v| v.frame)
}

fn add_subview(h: &mut Host, parent: u32, child: u32) {
    if child == 0 {
        return;
    }
    if let Some(old) = h.ui.views.get(&child).map(|v| v.superview) {
        if let Some(p) = h.ui.views.get_mut(&old) {
            p.subviews.retain(|&s| s != child);
        }
    }
    if let Some(v) = h.ui.views.get_mut(&child) {
        v.superview = parent;
    }
    if let Some(p) = h.ui.views.get_mut(&parent) {
        p.subviews.push(child);
    }
}

fn info_orientation(h: &Host) -> String {
    revs_analyze::ipa::read_app_file(&h.objc.ipa, "Info.plist")
        .ok()
        .and_then(|d| plist::from_bytes::<plist::Value>(&d).ok())
        .and_then(|v| v.as_dictionary()?.get("UIInterfaceOrientation")?.as_string().map(String::from))
        .unwrap_or_default()
}

/// UIInterfaceOrientation of the app (from Info.plist; landscape right if unset).
fn interface_orientation(h: &Host) -> u32 {
    match info_orientation(h).as_str() {
        "UIInterfaceOrientationPortrait" => 1,
        "UIInterfaceOrientationPortraitUpsideDown" => 2,
        "UIInterfaceOrientationLandscapeLeft" => 4,
        _ => 3,
    }
}

fn notification(h: &mut Host, name: u32, object: u32) -> u32 {
    new_obj(h, "NSNotification", Obj::Notification { name, object })
}

/// Delivers a notification to every observer registered for it.
pub fn post(h: &mut Host, c: &mut Cpu, name: &str, object: u32) {
    let targets: Vec<(u32, u32)> = h
        .ui
        .observers
        .iter()
        .filter(|o| o.name.as_deref().is_none_or(|n| n == name) && (o.object == 0 || o.object == object))
        .map(|o| (o.observer, o.sel))
        .collect();
    if targets.is_empty() {
        return;
    }
    let n = objc::new_string(h, name.to_string());
    let note = notification(h, n, object);
    for (obs, s) in targets {
        let sn = objc::sel_name(h, s);
        send(h, c, obs, &sn, &[note]);
    }
}

// ---------------------------------------------------------------- run loop

/// What UIKit does after launching: the app becomes active, then the main
/// run loop fires timers until the window closes (or --frames is reached).
pub fn run_loop(h: &mut Host, c: &mut Cpu) {
    let (app, d) = (h.objc.app, h.objc.delegate);
    if objc::responds(h, d, "applicationDidBecomeActive:") {
        send(h, c, d, "applicationDidBecomeActive:", &[app]);
    }
    post(h, c, "UIApplicationDidBecomeActiveNotification", app);
    for a in crate::interp::setup(h) {
        crate::add_hook(a);
    }
    let mut idle_since = None;
    while c.halt == 0 && !h.ui.stop {
        crate::audio::pump(h, c);
        let mut events = Vec::new();
        if let Some(g) = h.gfx.as_mut() {
            events = g.window.poll_events();
            if g.window.quit {
                h.ui.log.push("window closed".into());
                break;
            }
        }
        crate::input::pump(h, c, events);
        crate::gamekit::pump(h, c);
        if c.halt != 0 {
            break;
        }
        let now = h.ui.now();
        let mut next = now + 0.25;
        let mut fired = false;
        if h.ui.extra_at.first().is_some_and(|e| e.0 <= now) {
            let (_, k, n) = h.ui.extra_at.remove(0);
            crate::interp::set_between(k, n);
            extra_render(h, c);
            fired = true;
        }
        if let Some(&(t, _, _)) = h.ui.extra_at.first() {
            next = next.min(t);
        }
        let mut i = 0;
        while i < h.ui.timers.len() && c.halt == 0 && !h.ui.stop {
            let t = &mut h.ui.timers[i];
            i += 1;
            if !t.valid || !t.scheduled {
                continue;
            }
            if t.next <= now {
                let (obj, target, s) = (t.obj, t.target, t.sel);
                if t.repeats {
                    t.next = (t.next + t.interval.max(0.001)).max(now);
                } else {
                    t.valid = false;
                }
                let sn = objc::sel_name(h, s);
                let tick = sn == "onTick";
                if tick {
                    crate::interp::begin_step();
                }
                send(h, c, target, &sn, &[obj]);
                fired = true;
                crate::render_diff(h, c);
                if tick {
                    crate::interp::end_step();
                    plan_extra_renders(h);
                }
            }
            if let Some(t) = h.ui.timers.get(i - 1) {
                if t.valid {
                    next = next.min(t.next);
                }
            }
        }
        if !fired {
            // Nothing to do for 10 seconds with no timer: the game is waiting
            // for something this runner doesn't do yet.
            let since = *idle_since.get_or_insert(now);
            if h.ui.timers.iter().all(|t| !t.valid) && now - since > 10.0 {
                h.ui.log.push("run loop: no timers for 10 s, stopping".into());
                break;
            }
            let wait = (next - h.ui.now()).clamp(0.0, 0.05);
            std::thread::sleep(std::time::Duration::from_secs_f64(wait));
        } else {
            idle_since = None;
        }
    }
}

/// After each game step (onTick), crate::extra_frames() more Render() calls
/// (--fps) spread evenly until the next step.
fn plan_extra_renders(h: &mut Host) {
    let n = crate::extra_frames();
    if n == 0 {
        return;
    }
    let now = h.ui.now();
    let Some(next) = h.ui.timers.iter().filter(|t| t.valid && t.scheduled).map(|t| t.next).reduce(f64::min) else { return };
    let step = (next - now).max(0.0) / (n + 1) as f64;
    h.ui.extra_at = (1..=n).map(|k| (now + step * k as f64, k, n)).collect();
}

/// One in-between frame: Render() again, shown but not counted as a frame.
fn extra_render(h: &mut Host, c: &mut Cpu) {
    let app = crate::RENDER_APP.load(std::sync::atomic::Ordering::Relaxed);
    if app == 0 {
        return;
    }
    // REVS_EXTRA_SHOTS=F or F-G: also save the in-between frames after steps
    // F to G, as <screenshot>_F_<k>.bmp.
    static SHOT: std::sync::OnceLock<Option<(u64, u64)>> = std::sync::OnceLock::new();
    let shot = *SHOT.get_or_init(|| {
        let v = std::env::var("REVS_EXTRA_SHOTS").ok()?;
        let (a, b) = v.split_once('-').unwrap_or((&v, &v));
        Some((a.parse().ok()?, b.parse().ok()?))
    });
    let frames = h.gfx.as_ref().map_or(0, |g| g.frames);
    let path = h.ui.screenshot.clone().filter(|_| shot.is_some_and(|(a, b)| (a..=b).contains(&frames))).map(|base| {
        let stem = base.file_stem().map_or("shot".into(), |s| s.to_string_lossy().into_owned());
        let k = h.gfx.as_ref().map_or(0, |g| g.extra_frames) % 1000;
        base.with_file_name(format!("{stem}_{frames}_{k}.bmp"))
    });
    // A capture already asked for the next step frame is kept for it.
    let mut pending = false;
    if let Some(g) = h.gfx.as_mut() {
        g.extra = true;
        pending = std::mem::replace(&mut g.capture_next, path.is_some());
    }
    crate::IN_EXTRA.store(true, std::sync::atomic::Ordering::Relaxed);
    crate::guest_call(c, crate::RENDER, &[app]);
    crate::IN_EXTRA.store(false, std::sync::atomic::Ordering::Relaxed);
    if let Some(g) = h.gfx.as_mut() {
        g.extra = false;
        g.capture_next = pending;
        if let Some(p) = &path {
            if let Err(e) = unsafe { g.save_window_bmp(p) } {
                h.ui.log.push(format!("screenshot: {e}"));
            }
        }
    }
}

/// REVS_WATCH=EXPR,...: print each value when it changes (checked after every
/// presented frame). EXPR is `0xADDR` (the word there) or `*0xADDR+0xOFF` (the
/// word at OFF past the pointer stored at ADDR); a `b:` prefix reads a byte.
fn watch_frame(h: &Host, frame: u64) {
    use std::sync::Mutex;
    static WATCH: Mutex<Option<Vec<(String, Option<u32>)>>> = Mutex::new(None);
    let mut w = WATCH.lock().unwrap();
    let list = w.get_or_insert_with(|| {
        std::env::var("REVS_WATCH")
            .map(|v| v.split(',').filter(|e| !e.is_empty()).map(|e| (e.to_string(), None)).collect())
            .unwrap_or_default()
    });
    let num = |t: &str| u32::from_str_radix(t.trim_start_matches("0x"), 16).ok();
    for (expr, last) in list.iter_mut() {
        let (byte, e) = match expr.strip_prefix("b:") {
            Some(r) => (true, r),
            None => (false, expr.as_str()),
        };
        let addr = match e.strip_prefix('*') {
            Some(r) => {
                let (base, off) = r.split_once('+').unwrap_or((r, "0"));
                let (Some(base), Some(off)) = (num(base), num(off)) else { continue };
                let p = h.rd32(base);
                if p == 0 {
                    continue;
                }
                p.wrapping_add(off)
            }
            None => match num(e) {
                Some(a) => a,
                None => continue,
            },
        };
        let v = if byte { h.rd8(addr) as u32 } else { h.rd32(addr) };
        if *last != Some(v) {
            println!("watch f{frame} {expr} = {v:#x}");
            *last = Some(v);
        }
    }
}

/// After a presented frame: count it, save a screenshot, stop at --frames.
fn after_present(h: &mut Host) {
    let frames = h.gfx.as_ref().map_or(0, |g| g.frames);
    crate::coverage_frame(frames);
    watch_frame(h, frames);
    if h.ui.shots.contains(&frames) {
        if let Some(base) = h.ui.screenshot.clone() {
            let stem = base.file_stem().map_or("shot".into(), |s| s.to_string_lossy().into_owned());
            let path = base.with_file_name(format!("{stem}_{frames}.bmp"));
            if let Some(g) = h.gfx.as_mut() {
                if let Err(e) = unsafe { g.save_window_bmp(&path) } {
                    h.ui.log.push(format!("screenshot: {e}"));
                }
            }
        }
    }
    if let Some(max) = h.ui.max_frames {
        if frames >= max {
            if let Some(path) = h.ui.screenshot.clone() {
                if let Some(g) = h.gfx.as_mut() {
                    match unsafe { g.save_window_bmp(&path) } {
                        Ok(()) => h.ui.log.push(format!("saved frame {frames} to {}", path.display())),
                        Err(e) => h.ui.log.push(format!("screenshot: {e}")),
                    }
                }
            }
            h.ui.stop = true;
        }
    }
    // Will the next frame be saved? Then `present` keeps its picture.
    let next = frames + 1;
    let want = h.ui.screenshot.is_some() && (h.ui.shots.contains(&next) || h.ui.max_frames == Some(next));
    if let Some(g) = h.gfx.as_mut() {
        g.capture_next = want;
    }
}

// ---------------------------------------------------------------- native methods

pub fn native_imp(class: &str, meta: bool, s: &str) -> Option<NativeImp> {
    Some(match (class, meta, s) {
        // ---- UIView
        ("UIView", true, "layerClass") => |h, _, _| objc::native_class(h, "CALayer"),
        ("UIView", false, "init") => |h, c, m| {
            setup_view(h, c, m.this, [0.0; 4]);
            m.this
        },
        ("UIView", false, "initWithFrame:") => |h, c, m| {
            let f = [fl(h, c, m, 0), fl(h, c, m, 1), fl(h, c, m, 2), fl(h, c, m, 3)];
            setup_view(h, c, m.this, f);
            m.this
        },
        // Nib: UIBounds (rect) and UICenter (point) give the frame; UISubviews the children.
        ("UIView", false, "initWithCoder:") => |h, c, m| {
            let b = objc::nib_floats(h, "UIBounds", 4).unwrap_or(vec![0.0, 0.0, 0.0, 0.0]);
            let ctr = objc::nib_floats(h, "UICenter", 2).unwrap_or(vec![b[2] / 2.0, b[3] / 2.0]);
            let frame = [ctr[0] - b[2] / 2.0, ctr[1] - b[3] / 2.0, b[2], b[3]];
            setup_view(h, c, m.this, frame);
            let subs = objc::decode_object(h, c, "UISubviews");
            let list = match h.objc.objs.get(&subs) {
                Some(Obj::Array(a)) => a.clone(),
                _ => Vec::new(),
            };
            for v in list {
                add_subview(h, m.this, v);
            }
            if objc::nib_bool(h, "UIHidden") {
                if let Some(v) = h.ui.views.get_mut(&m.this) {
                    v.hidden = true;
                }
            }
            m.this
        },
        ("UIView", false, "layer") => |h, _, m| h.ui.views.get(&m.this).map_or(0, |v| v.layer),
        ("UIView", false, "frame") => |h, c, m| {
            let f = frame_of(h, m.this);
            ret_floats(h, c, &f)
        },
        ("UIView", false, "bounds") => |h, c, m| {
            let f = frame_of(h, m.this);
            ret_floats(h, c, &[0.0, 0.0, f[2], f[3]])
        },
        ("UIView", false, "center") => |h, c, m| {
            let f = frame_of(h, m.this);
            ret_floats(h, c, &[f[0] + f[2] / 2.0, f[1] + f[3] / 2.0])
        },
        ("UIView", false, "setFrame:") => |h, c, m| {
            let f = [fl(h, c, m, 0), fl(h, c, m, 1), fl(h, c, m, 2), fl(h, c, m, 3)];
            if let Some(v) = h.ui.views.get_mut(&m.this) {
                v.frame = f;
            }
            0
        },
        ("UIView", false, "setBounds:") => |h, c, m| {
            let (w, ht) = (fl(h, c, m, 2), fl(h, c, m, 3));
            if let Some(v) = h.ui.views.get_mut(&m.this) {
                let (cx, cy) = (v.frame[0] + v.frame[2] / 2.0, v.frame[1] + v.frame[3] / 2.0);
                v.frame = [cx - w / 2.0, cy - ht / 2.0, w, ht];
            }
            0
        },
        ("UIView", false, "setCenter:") => |h, c, m| {
            let (x, y) = (fl(h, c, m, 0), fl(h, c, m, 1));
            if let Some(v) = h.ui.views.get_mut(&m.this) {
                v.frame[0] = x - v.frame[2] / 2.0;
                v.frame[1] = y - v.frame[3] / 2.0;
            }
            0
        },
        ("UIView", false, "addSubview:") => |h, c, m| {
            let v = m.arg(h, c, 0);
            add_subview(h, m.this, v);
            0
        },
        ("UIView", false, "removeFromSuperview") => |h, _, m| {
            let p = h.ui.views.get(&m.this).map_or(0, |v| v.superview);
            if let Some(pv) = h.ui.views.get_mut(&p) {
                pv.subviews.retain(|&s| s != m.this);
            }
            if let Some(v) = h.ui.views.get_mut(&m.this) {
                v.superview = 0;
            }
            0
        },
        ("UIView", false, "superview") => |h, _, m| h.ui.views.get(&m.this).map_or(0, |v| v.superview),
        ("UIView", false, "subviews") => |h, _, m| {
            let s = h.ui.views.get(&m.this).map_or(Vec::new(), |v| v.subviews.clone());
            new_obj(h, "NSArray", Obj::Array(s))
        },
        ("UIView", false, "setHidden:") => |h, c, m| {
            let b = m.arg(h, c, 0) & 0xFF != 0;
            if let Some(v) = h.ui.views.get_mut(&m.this) {
                v.hidden = b;
            }
            0
        },
        ("UIView", false, "isHidden") => |h, _, m| h.ui.views.get(&m.this).is_some_and(|v| v.hidden) as u32,
        ("UIView", false, "window") => |h, _, m| {
            let mut v = m.this;
            let win = objc::native_class(h, "UIWindow");
            for _ in 0..64 {
                if v == 0 || objc::is_kind_of(h, v, win) {
                    return v;
                }
                v = h.ui.views.get(&v).map_or(0, |x| x.superview);
            }
            0
        },
        (
            "UIView",
            false,
            "setMultipleTouchEnabled:" | "setUserInteractionEnabled:" | "setBackgroundColor:" | "setOpaque:"
            | "setAlpha:" | "setNeedsDisplay" | "setNeedsLayout" | "layoutSubviews" | "setExclusiveTouch:"
            | "setClipsToBounds:" | "setAutoresizingMask:" | "setAutoresizesSubviews:" | "setContentMode:" | "setTag:"
            | "setTransform:" | "bringSubviewToFront:" | "sendSubviewToBack:" | "setClearsContextBeforeDrawing:",
        ) => |_, _, _| 0,
        // ---- UIWindow
        ("UIWindow", false, "makeKeyAndVisible" | "makeKeyWindow") => |h, _, m| {
            h.ui.key_window = m.this;
            if let Some(v) = h.ui.views.get_mut(&m.this) {
                v.hidden = false;
            }
            0
        },
        // ---- UIViewController: the view comes from the nib ("UIView" key) or loadView.
        ("UIViewController", false, "initWithCoder:") => |h, c, m| {
            let v = objc::decode_object(h, c, "UIView");
            if v != 0 {
                h.ui.vc_view.insert(m.this, v);
            }
            m.this
        },
        ("UIViewController", false, "initWithNibName:bundle:" | "init") => |_, _, m| m.this,
        ("UIViewController", false, "view") => |h, c, m| {
            if let Some(&v) = h.ui.vc_view.get(&m.this) {
                return v;
            }
            send(h, c, m.this, "loadView", &[]);
            if !h.ui.vc_view.contains_key(&m.this) {
                let cls = objc::native_class(h, "UIView");
                let v = objc::alloc_instance(h, cls);
                setup_view(h, c, v, [0.0, 0.0, gfx::SCREEN_W as f32, gfx::SCREEN_H as f32]);
                h.ui.vc_view.insert(m.this, v);
            }
            send(h, c, m.this, "viewDidLoad", &[]);
            h.ui.vc_view[&m.this]
        },
        ("UIViewController", false, "setView:") => |h, c, m| {
            let v = m.arg(h, c, 0);
            h.ui.vc_view.insert(m.this, v);
            0
        },
        ("UIViewController", false, "isViewLoaded") => |h, _, m| h.ui.vc_view.contains_key(&m.this) as u32,
        ("UIViewController", false, "loadView" | "viewDidLoad" | "viewWillAppear:" | "viewDidAppear:") => |_, _, _| 0,
        ("UIViewController", false, "interfaceOrientation") => |h, _, _| interface_orientation(h),
        // ---- CALayer / CAEAGLLayer
        ("CALayer", false, "setOpaque:") => |h, c, m| {
            let b = m.arg(h, c, 0) & 0xFF != 0;
            if let Some(l) = h.ui.layers.get_mut(&m.this) {
                l.opaque = b;
            }
            0
        },
        ("CALayer", false, "isOpaque") => |h, _, m| h.ui.layers.get(&m.this).is_some_and(|l| l.opaque) as u32,
        ("CALayer", false, "setDrawableProperties:") => |h, c, m| {
            let p = m.arg(h, c, 0);
            if let Some(l) = h.ui.layers.get_mut(&m.this) {
                l.props = p;
            }
            0
        },
        ("CALayer", false, "drawableProperties") => |h, _, m| h.ui.layers.get(&m.this).map_or(0, |l| l.props),
        ("CALayer", false, "bounds") => |h, c, m| {
            let v = h.ui.layers.get(&m.this).map_or(0, |l| l.view);
            let f = frame_of(h, v);
            ret_floats(h, c, &[0.0, 0.0, f[2], f[3]])
        },
        ("CALayer", false, "delegate") => |h, _, m| h.ui.layers.get(&m.this).map_or(0, |l| l.view),
        ("CALayer", false, "contentsScale") => |_, _, _| 1.0f32.to_bits(),
        ("CALayer", false, "setContentsScale:" | "setNeedsDisplay" | "setMasksToBounds:") => |_, _, _| 0,
        // ---- EAGLContext (one GL context, in the window)
        ("EAGLContext", false, "initWithAPI:" | "initWithAPI:sharegroup:") => |h, c, m| {
            let api = m.arg(h, c, 0);
            if api != 1 {
                h.ui.log.push(format!("EAGLContext initWithAPI:{api} refused (only OpenGL ES 1)"));
                return 0;
            }
            if h.gfx.is_none() {
                let rot = if interface_orientation(h) == 4 { -90.0 } else { 90.0 };
                match gfx::Gfx::new(rot) {
                    Ok(g) => h.gfx = Some(g),
                    Err(e) => {
                        h.ui.log.push(format!("could not open the window: {e}"));
                        return 0;
                    }
                }
            }
            m.this
        },
        ("EAGLContext", true, "setCurrentContext:") => |h, c, m| {
            h.ui.current_ctx = m.arg(h, c, 0);
            1
        },
        ("EAGLContext", true, "currentContext") => |h, _, _| h.ui.current_ctx,
        ("EAGLContext", false, "API") => |_, _, _| 1,
        ("EAGLContext", false, "renderbufferStorage:fromDrawable:") => |h, c, m| {
            let layer = m.arg(h, c, 1);
            let v = h.ui.layers.get(&layer).map_or(0, |l| l.view);
            let f = frame_of(h, v);
            let (w, ht) = (f[2].round() as i32, f[3].round() as i32);
            let target = m.arg(h, c, 0);
            if let Some(g) = h.gfx.as_mut() {
                unsafe { g.gl().RenderbufferStorageOES(target, crate::gles::gles11_raw::RGBA8_OES, w * crate::gfx::render_scale(), ht * crate::gfx::render_scale()) };
            }
            h.ui.log.push(format!("renderbuffer {w} x {ht} for the layer of {}", objc::class_name(h, isa(h, v))));
            1
        },
        ("EAGLContext", false, "presentRenderbuffer:") => |h, _, _| {
            let extra = h.gfx.as_ref().is_some_and(|g| g.extra);
            if let Some(g) = h.gfx.as_mut() {
                unsafe { g.present() };
            }
            if !extra {
                after_present(h);
            }
            1
        },
        // ---- NSTimer and the run loop
        ("NSTimer", true, "scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:" | "timerWithTimeInterval:target:selector:userInfo:repeats:") => {
            |h, c, m| {
                let interval = dbl(h, c, m, 0);
                // After the double (words 2-3 from `first`): target, selector, userInfo, repeats.
                let (target, s, info, rep) = (m.arg(h, c, 2), m.arg(h, c, 3), m.arg(h, c, 4), m.arg(h, c, 5));
                let scheduled = objc::sel_name(h, m.sel).starts_with("scheduled");
                let cls = objc::native_class(h, "NSTimer");
                let obj = objc::alloc_instance(h, cls);
                let next = h.ui.now() + interval;
                // REVS_TIMER_LOG=1: every timer the game schedules (time, interval, presented frames).
                if std::env::var_os("REVS_TIMER_LOG").is_some() {
                    let frames = h.gfx.as_ref().map_or(0, |g| g.frames);
                    println!("timer t={:.4} interval={interval:.4} frame={frames}", h.ui.now());
                }
                h.ui.timers.push(Timer { obj, interval, target, sel: s, info, repeats: rep & 0xFF != 0, scheduled, valid: true, next });
                // The game re-arms a one-shot timer every tick: log each target once.
                let what = format!("-> -[{} {}]", objc::class_name(h, isa(h, target)), objc::sel_name(h, s));
                if !h.ui.log.iter().any(|l| l.starts_with("timer") && l.contains(&what)) {
                    h.ui.log.push(format!("timer every {interval:.4} s {what} (repeats of this one not logged)"));
                }
                obj
            }
        }
        ("NSTimer", false, "invalidate") => |h, _, m| {
            for t in h.ui.timers.iter_mut().filter(|t| t.obj == m.this) {
                t.valid = false;
            }
            0
        },
        ("NSTimer", false, "isValid") => |h, _, m| h.ui.timers.iter().any(|t| t.obj == m.this && t.valid) as u32,
        ("NSTimer", false, "userInfo") => |h, _, m| h.ui.timers.iter().find(|t| t.obj == m.this).map_or(0, |t| t.info),
        ("NSTimer", false, "timeInterval") => |h, c, m| {
            let v = h.ui.timers.iter().find(|t| t.obj == m.this).map_or(0.0, |t| t.interval);
            ret_dbl(c, v)
        },
        ("NSRunLoop", true, "currentRunLoop" | "mainRunLoop") => |h, _, _| singleton(h, "NSRunLoop"),
        ("NSRunLoop", false, "addTimer:forMode:") => |h, c, m| {
            let t = m.arg(h, c, 0);
            for x in h.ui.timers.iter_mut().filter(|x| x.obj == t) {
                x.scheduled = true;
            }
            0
        },
        ("NSRunLoop", false, "getCFRunLoop") => |h, _, m| m.this.max(h.ui.singletons.get("NSRunLoop").copied().unwrap_or(0)),
        // ---- NSNotificationCenter
        ("NSNotificationCenter", true, "defaultCenter") => |h, _, _| singleton(h, "NSNotificationCenter"),
        ("NSNotificationCenter", false, "addObserver:selector:name:object:") => |h, c, m| {
            let (observer, s, name, object) = (m.arg(h, c, 0), m.arg(h, c, 1), m.arg(h, c, 2), m.arg(h, c, 3));
            let name = string_of(h, name);
            h.ui.observers.push(Observer { observer, sel: s, name, object });
            0
        },
        ("NSNotificationCenter", false, "removeObserver:" | "removeObserver:name:object:") => |h, c, m| {
            let o = m.arg(h, c, 0);
            h.ui.observers.retain(|x| x.observer != o);
            0
        },
        ("NSNotificationCenter", false, "postNotificationName:object:") => |h, c, m| {
            let (n, o) = (m.arg(h, c, 0), m.arg(h, c, 1));
            let name = string_of(h, n).unwrap_or_default();
            post(h, c, &name, o);
            0
        },
        ("NSNotification", false, "name") => |h, _, m| match h.objc.objs.get(&m.this) {
            Some(Obj::Notification { name, .. }) => *name,
            _ => 0,
        },
        ("NSNotification", false, "object") => |h, _, m| match h.objc.objs.get(&m.this) {
            Some(Obj::Notification { object, .. }) => *object,
            _ => 0,
        },
        // ---- UIApplication
        ("UIApplication", false, "keyWindow") => |h, _, _| h.ui.key_window,
        ("UIApplication", false, "statusBarOrientation") => |h, _, _| interface_orientation(h),
        ("UIApplication", false, "applicationState") => |_, _, _| 0,
        (
            "UIApplication",
            false,
            "setIdleTimerDisabled:" | "setStatusBarHidden:" | "setStatusBarHidden:animated:" | "setStatusBarHidden:withAnimation:"
            | "setStatusBarOrientation:" | "setStatusBarOrientation:animated:" | "setApplicationIconBadgeNumber:"
            | "beginIgnoringInteractionEvents" | "endIgnoringInteractionEvents",
        ) => |_, _, _| 0,
        // ---- UIDevice, UIScreen, UIAccelerometer
        ("UIDevice", true, "currentDevice") => |h, _, _| singleton(h, "UIDevice"),
        ("UIDevice", false, "orientation") => |h, _, _| interface_orientation(h),
        ("UIDevice", false, "beginGeneratingDeviceOrientationNotifications" | "endGeneratingDeviceOrientationNotifications") => |_, _, _| 0,
        ("UIDevice", false, "model" | "localizedModel") => |h, _, _| objc::new_string(h, "iPhone".into()),
        ("UIDevice", false, "systemName") => |h, _, _| objc::new_string(h, "iPhone OS".into()),
        ("UIDevice", false, "systemVersion") => |h, _, _| objc::new_string(h, "4.2.1".into()),
        ("UIDevice", false, "name") => |h, _, _| objc::new_string(h, "iPhone".into()),
        ("UIDevice", false, "uniqueIdentifier") => |h, _, _| objc::new_string(h, "0000000000000000000000000000000000000000".into()),
        ("UIScreen", true, "mainScreen") => |h, _, _| singleton(h, "UIScreen"),
        ("UIScreen", false, "bounds" | "applicationFrame") => |h, c, _| {
            ret_floats(h, c, &[0.0, 0.0, gfx::SCREEN_W as f32, gfx::SCREEN_H as f32])
        },
        ("UIScreen", false, "scale") => |_, _, _| 1.0f32.to_bits(),
        ("UIAccelerometer", true, "sharedAccelerometer") => |h, _, _| singleton(h, "UIAccelerometer"),
        ("UIAccelerometer", false, "setDelegate:" | "setUpdateInterval:") => |_, _, _| 0,
        // ---- NSNumber
        ("NSNumber", true, "numberWithBool:" | "numberWithChar:" | "numberWithUnsignedChar:") => |h, c, m| {
            let v = (m.arg(h, c, 0) & 0xFF) as i8 as f64;
            new_obj(h, "NSNumber", Obj::Number(v))
        },
        ("NSNumber", true, "numberWithInt:" | "numberWithInteger:" | "numberWithLong:" | "numberWithShort:") => |h, c, m| {
            let v = m.arg(h, c, 0) as i32 as f64;
            new_obj(h, "NSNumber", Obj::Number(v))
        },
        ("NSNumber", true, "numberWithUnsignedInt:" | "numberWithUnsignedInteger:") => |h, c, m| {
            let v = m.arg(h, c, 0) as f64;
            new_obj(h, "NSNumber", Obj::Number(v))
        },
        ("NSNumber", true, "numberWithFloat:") => |h, c, m| {
            let v = fl(h, c, m, 0) as f64;
            new_obj(h, "NSNumber", Obj::Number(v))
        },
        ("NSNumber", true, "numberWithDouble:") => |h, c, m| {
            let v = dbl(h, c, m, 0);
            new_obj(h, "NSNumber", Obj::Number(v))
        },
        ("NSNumber", false, "boolValue") => |h, _, m| (number(h, m.this) != 0.0) as u32,
        ("NSNumber", false, "intValue" | "integerValue" | "longValue" | "unsignedIntValue" | "unsignedIntegerValue") => {
            |h, _, m| number(h, m.this) as i64 as u32
        }
        ("NSNumber", false, "floatValue") => |h, _, m| (number(h, m.this) as f32).to_bits(),
        ("NSNumber", false, "doubleValue") => |h, c, m| {
            let v = number(h, m.this);
            ret_dbl(c, v)
        },
        // ---- NSDictionary (pairs, keys compared by pointer or string)
        ("NSDictionary", true, "dictionary") => |h, _, _| new_obj(h, "NSMutableDictionary", Obj::Dict(Vec::new())),
        ("NSDictionary", true, "dictionaryWithObjectsAndKeys:") => |h, c, m| {
            // object, key, object, key, ..., nil
            let mut pairs = Vec::new();
            let mut i = 0;
            loop {
                let o = m.arg(h, c, i);
                if o == 0 || i > 200 {
                    break;
                }
                pairs.push((m.arg(h, c, i + 1), o));
                i += 2;
            }
            new_obj(h, "NSDictionary", Obj::Dict(pairs))
        },
        ("NSDictionary", true, "dictionaryWithObject:forKey:") => |h, c, m| {
            let (o, k) = (m.arg(h, c, 0), m.arg(h, c, 1));
            new_obj(h, "NSDictionary", Obj::Dict(vec![(k, o)]))
        },
        ("NSDictionary", false, "init") => |h, _, m| {
            h.objc.objs.insert(m.this, Obj::Dict(Vec::new()));
            m.this
        },
        ("NSDictionary", false, "objectForKey:") => |h, c, m| {
            let k = m.arg(h, c, 0);
            match h.objc.objs.get(&m.this) {
                Some(Obj::Dict(p)) => p.iter().find(|(x, _)| keys_equal(h, *x, k)).map_or(0, |(_, v)| *v),
                _ => 0,
            }
        },
        ("NSDictionary", false, "count") => |h, _, m| match h.objc.objs.get(&m.this) {
            Some(Obj::Dict(p)) => p.len() as u32,
            _ => 0,
        },
        ("NSMutableDictionary", true, "dictionaryWithCapacity:") => |h, _, _| new_obj(h, "NSMutableDictionary", Obj::Dict(Vec::new())),
        ("NSMutableDictionary", false, "initWithCapacity:") => |h, _, m| {
            h.objc.objs.insert(m.this, Obj::Dict(Vec::new()));
            m.this
        },
        ("NSMutableDictionary", false, "setObject:forKey:") => |h, c, m| {
            let (o, k) = (m.arg(h, c, 0), m.arg(h, c, 1));
            let mut p = match h.objc.objs.remove(&m.this) {
                Some(Obj::Dict(p)) => p,
                _ => Vec::new(),
            };
            p.retain(|(x, _)| !keys_equal(h, *x, k));
            p.push((k, o));
            h.objc.objs.insert(m.this, Obj::Dict(p));
            0
        },
        ("NSMutableDictionary", false, "removeObjectForKey:") => |h, c, m| {
            let k = m.arg(h, c, 0);
            let mut p = match h.objc.objs.remove(&m.this) {
                Some(Obj::Dict(p)) => p,
                _ => Vec::new(),
            };
            p.retain(|(x, _)| !keys_equal(h, *x, k));
            h.objc.objs.insert(m.this, Obj::Dict(p));
            0
        },
        // ---- NSDate (seconds since 2001-01-01)
        ("NSDate", true, "date") => |h, _, _| new_obj(h, "NSDate", Obj::Date(reference_now())),
        ("NSDate", true, "timeIntervalSinceReferenceDate") => |_, c, _| ret_dbl(c, reference_now()),
        ("NSDate", false, "timeIntervalSinceReferenceDate") => |h, c, m| {
            let t = match h.objc.objs.get(&m.this) {
                Some(Obj::Date(t)) => *t,
                _ => 0.0,
            };
            ret_dbl(c, t)
        },
        ("NSDate", false, "timeIntervalSince1970") => |h, c, m| {
            let t = match h.objc.objs.get(&m.this) {
                Some(Obj::Date(t)) => *t,
                _ => 0.0,
            };
            ret_dbl(c, t + 978_307_200.0)
        },
        ("NSDate", false, "timeIntervalSinceNow") => |h, c, m| {
            let t = match h.objc.objs.get(&m.this) {
                Some(Obj::Date(t)) => *t,
                _ => 0.0,
            };
            ret_dbl(c, t - reference_now())
        },
        ("NSDate", false, "timeIntervalSinceDate:") => |h, c, m| {
            let o = m.arg(h, c, 0);
            let get = |x: u32| match h.objc.objs.get(&x) {
                Some(Obj::Date(t)) => *t,
                _ => 0.0,
            };
            let v = get(m.this) - get(o);
            ret_dbl(c, v)
        },
        // ---- NSFileManager (files come in Phase 3.5)
        ("NSFileManager", true, "defaultManager") => |h, _, _| singleton(h, "NSFileManager"),
        _ => return crate::nsfile::native_imp(class, meta, s),
    })
}
