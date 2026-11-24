//! Native runner (Phase 3): runs the recompiled game without an ARM emulator.
//!
//! Loads every image listed in build/recomp/images.json (the game from the
//! user's own .ipa, and the libraries it links: touchHLE's open-source ARM
//! dylibs, recompiled with the game) into a 4 GiB guest block, links them like
//! dyld, runs the static initialisers (libraries first) and `main` on the
//! recompiled C, and lists every OS function they call, in first-use order.
//!
//! Imports are matched by name: an export of a loaded library first, then a
//! native version in `os.rs`. With neither, a call returns 0 (survey mode) or
//! stops the run (`--strict`).
//!
//! Guest memory map:
//!   game segments      from the Mach-O (below 0x0070_0000)
//!   libgcc, libstdc++  moved to 0x0070_0000 / 0x0080_0000 (see images.json)
//!   SQLite             its own address, 0x3642_2000
//!   heap               0x4000_0000 .. 0xD000_0000 (os::Heap)
//!   ObjC runtime area  0xD000_0000 .. (native classes, selector names)
//!   host data blocks   0xE000_0000 .. (one per imported data symbol, zeroed)
//!   stack              0xF000_0000 .. +8 MiB
//!   host functions     0xFFF0_0000 + 4 * index (never mapped: calling one
//!                      through a pointer lands in rc_miss, which dispatches)

mod audio;
mod fs;
mod game;
mod gamekit;
mod gfx;
mod gles;
mod nsfile;
mod objc;
mod import;
mod interp;
mod input;
mod lyt;
mod mcts;
mod textures;
mod tweaks;
mod net;
mod os;
mod touch;
mod uikit;
mod watchdog;
mod window;

use revs_analyze::{image, macho};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Same layout as `Cpu` in rc_cpu.h.
#[repr(C)]
pub struct Cpu {
    pub r: [u32; 16],
    pub n: u32,
    pub z: u32,
    pub c: u32,
    pub v: u32,
    pub m: *mut u8,
    pub budget: i32,
    pub halt: u32,
    pub halt_addr: u32,
    pub ext: [u32; 64],
    pub fpscr: u32,
}
pub type RcFn = unsafe extern "C" fn(*mut Cpu);
use game::{lookup as rc_lookup, trap as rc_trap};
const RC_HALT_TRAP: u32 = 2;
const RC_HALT_IMPORT: u32 = 3;

pub const HOST_FN: u32 = 0xFFF0_0000;
pub const HOST_DATA: u32 = 0xE000_0000;
const HOST_DATA_BLOCK: u32 = 0x100;
pub const HEAP_START: u32 = 0x4000_0000;
pub const HEAP_END: u32 = 0xD000_0000;
const STACK: u32 = 0xF000_0000;
const STACK_SIZE: u32 = 8 << 20;
const SIZE: usize = 1 << 32;

/// Where a call to an imported function goes.
#[derive(Clone, Copy)]
enum Target {
    /// A function in a loaded image (Thumb bit set for Thumb).
    Guest(u32),
    /// Host function `index` (native in os.rs, or missing).
    Host(u32),
}

/// How a host function was used.
struct Use {
    order: u32,
    calls: u64,
    first_lr: u32,
    native: bool,
}

pub struct Host {
    pub base: *mut u8,
    /// Host function index -> symbol name (without the Mach-O leading `_`).
    fn_names: Vec<String>,
    fn_index: HashMap<String, u32>,
    /// Import stub address (any image) -> where it goes.
    stubs: HashMap<u32, Target>,
    /// Untranslated library functions that run natively instead (os::OVERRIDE),
    /// by address -> host function index (reached through rc_miss).
    addr_host: HashMap<u32, u32>,
    /// Host data block -> symbol name.
    data_names: Vec<String>,
    data_index: HashMap<String, u32>,
    pub heap: os::Heap,
    uses: HashMap<String, Use>,
    misses: HashMap<u32, (u32, u64)>,
    strict: bool,
    /// `_Unwind_SjLj_Register` list head (used only if libgcc isn't loaded).
    pub sjlj_head: u32,
    /// `__cxa_atexit` handlers (function, argument).
    pub atexit: Vec<(u32, u32)>,
    /// pthread keys (thread-specific values; one thread for now).
    pub tls: Vec<u32>,
    pub objc: objc::Runtime,
    pub ui: uikit::Ui,
    /// The window and GL context (made by the first EAGLContext).
    pub gfx: Option<gfx::Gfx>,
    pub gl_warned: bool,
    /// Bundle (from the .ipa, read-only) and the port's own home folder.
    pub fs: fs::Fs,
    pub audio: audio::Audio,
    pub touch: touch::Touch,
}

static mut HOST: *mut Host = std::ptr::null_mut();
/// Last host function called (index | lr << 32), for --watchdog.
static LAST_CALL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
static LAST_ARGS: [std::sync::atomic::AtomicU32; 3] =
    [std::sync::atomic::AtomicU32::new(0), std::sync::atomic::AtomicU32::new(0), std::sync::atomic::AtomicU32::new(0)];
static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn host() -> &'static mut Host {
    unsafe { &mut *HOST }
}

impl Host {
    pub fn at(&self, a: u32) -> *mut u8 {
        unsafe { self.base.add(a as usize) }
    }
    pub fn rd32(&self, a: u32) -> u32 {
        unsafe { (self.at(a) as *const u32).read_unaligned() }
    }
    pub fn wr32(&self, a: u32, v: u32) {
        unsafe { (self.at(a) as *mut u32).write_unaligned(v) }
    }
    pub fn rd8(&self, a: u32) -> u8 {
        unsafe { *self.at(a) }
    }
    pub fn wr8(&self, a: u32, v: u8) {
        unsafe { *self.at(a) = v }
    }
    pub fn cstr(&self, a: u32) -> String {
        let mut s = Vec::new();
        let mut p = a;
        while self.rd8(p) != 0 && s.len() < 4096 {
            s.push(self.rd8(p));
            p += 1;
        }
        String::from_utf8_lossy(&s).into_owned()
    }

    fn host_fn(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.fn_index.get(name) {
            return i;
        }
        let i = self.fn_names.len() as u32;
        self.fn_names.push(name.to_string());
        self.fn_index.insert(name.to_string(), i);
        i
    }
    fn data_block(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.data_index.get(name) {
            return HOST_DATA + HOST_DATA_BLOCK * i;
        }
        let i = self.data_names.len() as u32;
        self.data_names.push(name.to_string());
        self.data_index.insert(name.to_string(), i);
        HOST_DATA + HOST_DATA_BLOCK * i
    }

    fn call_host(&mut self, c: &mut Cpu, idx: u32) {
        LAST_CALL.store(idx as u64 | (c.r[14] as u64) << 32, std::sync::atomic::Ordering::Relaxed);
        for k in 0..3 {
            LAST_ARGS[k].store(c.r[k], std::sync::atomic::Ordering::Relaxed);
        }
        CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = self.fn_names[idx as usize].clone();
        let order = self.uses.len() as u32;
        let first_lr = c.r[14];
        let handled = objc::call(self, c, &name) || gfx::call(self, c, &name) || fs::call(self, c, &name) || audio::call(self, c, &name) || net::call(self, c, &name) || os::call(self, c, &name);
        if !handled && !self.uses.contains_key(&name) {
            println!("first call to missing OS function {name} from {first_lr:#x}");
        }
        let u = self.uses.entry(name).or_insert(Use { order, calls: 0, first_lr, native: handled });
        u.calls += 1;
        if !handled {
            if self.strict {
                unsafe { rc_trap(c, HOST_FN + 4 * idx, RC_HALT_IMPORT) };
            } else {
                c.r[0] = 0;
                c.r[1] = 0;
            }
        }
    }
}

/// Calls the guest function at `t` (already set up by the caller: arguments,
/// lr); a function that wasn't translated stops the run.
pub(crate) unsafe fn call_guest(c: *mut Cpu, t: u32) {
    match rc_lookup(t) {
        Some(f) => f(c),
        None => rc_trap(c, t, RC_HALT_TRAP),
    }
}

/// Calls guest function `f` from a native OS function (a callback, e.g. the
/// pthread_once routine) on the same CPU state. Returns its r0. Argument
/// registers and lr are put back afterwards.
pub fn guest_call(c: &mut Cpu, f: u32, args: &[u32]) -> u32 {
    let saved = (c.r[0], c.r[1], c.r[2], c.r[3], c.r[12], c.r[14]);
    for (k, &a) in args.iter().enumerate().take(4) {
        c.r[k] = a;
    }
    unsafe { call_guest(c, f) };
    let r = c.r[0];
    (c.r[0], c.r[1], c.r[2], c.r[3], c.r[12], c.r[14]) = saved;
    r
}

/// Called by the generated C for `bl <import stub>` (in any image).
#[no_mangle]
pub(crate) unsafe extern "C" fn rc_import(c: *mut Cpu, stub: u32, _index: u32) {
    let h = host();
    match h.stubs.get(&stub).copied() {
        Some(Target::Guest(t)) => call_guest(c, t),
        Some(Target::Host(i)) => h.call_host(&mut *c, i),
        None => rc_trap(c, stub, RC_HALT_TRAP),
    }
}

/// Called by the generated C for an indirect call to an address that isn't a
/// translated function.
#[no_mangle]
pub(crate) unsafe extern "C" fn rc_miss(c: *mut Cpu, target: u32) {
    let h = host();
    let t = target & !1;
    if t >= HOST_FN && ((t - HOST_FN) / 4) < h.fn_names.len() as u32 {
        h.call_host(&mut *c, (t - HOST_FN) / 4);
        return;
    }
    if let Some(&i) = h.addr_host.get(&t) {
        h.call_host(&mut *c, i);
        return;
    }
    if t == 0 {
        let c = &*c;
        eprintln!("call to 0: lr={:#x} sp={:#x} r0={:#x} r1={:#x} r2={:#x} r3={:#x} r4={:#x} r5={:#x}", c.r[14], c.r[13], c.r[0], c.r[1], c.r[2], c.r[3], c.r[4], c.r[5]);
    }
    let order = h.misses.len() as u32;
    let e = h.misses.entry(t).or_insert((order, 0));
    e.1 += 1;
    rc_trap(c, target, RC_HALT_TRAP);
}

fn data_name(h: &Host, a: u32) -> Option<&str> {
    if a >= HOST_DATA && a < STACK {
        h.data_names.get(((a - HOST_DATA) / HOST_DATA_BLOCK) as usize).map(String::as_str)
    } else {
        None
    }
}

/// Runs one guest function on the recompiled C. Ok(r0) or why it stopped.
fn run(h: &Host, addr: u32, args: [u32; 4], sp: u32) -> Result<u32, String> {
    let Some(f) = (unsafe { rc_lookup(addr) }) else { return Err(format!("{addr:#x} is not translated")) };
    let mut c = Cpu {
        r: [0; 16],
        n: 0,
        z: 0,
        c: 0,
        v: 0,
        m: h.base,
        budget: i32::MAX,
        halt: 0,
        halt_addr: 0,
        ext: [0; 64],
        fpscr: 0,
    };
    c.r[..4].copy_from_slice(&args);
    c.r[13] = sp;
    c.r[15] = addr;
    unsafe { f(&mut c) };
    match c.halt {
        0 => Ok(c.r[0]),
        RC_HALT_IMPORT => {
            let i = ((c.halt_addr - HOST_FN) / 4) as usize;
            Err(format!("OS function {} has no native version yet", h.fn_names[i]))
        }
        RC_HALT_TRAP => Err(match data_name(h, c.halt_addr) {
            Some(n) => format!("called imported data {n}"),
            None => format!("reached untranslated code at {:#x}", c.halt_addr),
        }),
        w => Err(format!("halted ({w}) at {:#x}", c.halt_addr)),
    }
}

/// Reserves `size` bytes of zeroed, read-write memory. Pages cost nothing until
/// touched, so the whole 4 GiB guest address space can be one block.
#[cfg(windows)]
fn reserve_memory(size: usize) -> *mut u8 {
    extern "system" {
        fn VirtualAlloc(addr: *mut std::ffi::c_void, size: usize, kind: u32, protect: u32) -> *mut std::ffi::c_void;
    }
    // MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE.
    unsafe { VirtualAlloc(std::ptr::null_mut(), size, 0x3000, 0x04) as *mut u8 }
}

#[cfg(unix)]
fn reserve_memory(size: usize) -> *mut u8 {
    // MAP_NORESERVE: no swap is set aside for pages never touched.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED { std::ptr::null_mut() } else { p as *mut u8 }
}

/// One loaded image.
struct Loaded {
    name: String,
    bin: macho::Binary,
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let strict = args.iter().any(|a| a == "--strict");
    args.retain(|a| a != "--strict");
    // --watchdog=N: after N seconds, say what the last OS call was and stop
    // (to find where a run loops).
    let watchdog: Option<u64> = args.iter().find_map(|a| a.strip_prefix("--watchdog=")?.parse().ok());
    args.retain(|a| !a.starts_with("--watchdog="));
    // --frames=N: stop after N presented frames; --screenshot=file.bmp: save the last one.
    let frames: Option<u64> = args.iter().find_map(|a| a.strip_prefix("--frames=")?.parse().ok());
    let screenshot: Option<PathBuf> = args.iter().find_map(|a| a.strip_prefix("--screenshot=").map(PathBuf::from));
    args.retain(|a| !a.starts_with("--frames=") && !a.starts_with("--screenshot="));
    // --home=DIR: the port's own Documents/Library/tmp (never a touchHLE save).
    let home: PathBuf = args.iter().find_map(|a| a.strip_prefix("--home=").map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("build/home"));
    args.retain(|a| !a.starts_with("--home="));
    // --import-save=DIR: copy a touchHLE save into that home first (import.rs).
    let import_from: Option<PathBuf> = args.iter().find_map(|a| a.strip_prefix("--import-save=").map(PathBuf::from));
    args.retain(|a| !a.starts_with("--import-save="));
    // --player=N (Local Match, one window per player): player N's own save
    // folder (build/home_pN, first filled with a copy of player 1's save),
    // "PLAYER N" in the window title, and controller N only.
    // --controller=N: only controller N (1 = the first connected) drives this
    // window; 0 = all of them. --gk-peer=HOST: also look for Local Match
    // players at HOST (gamekit.rs).
    let player: u32 = args.iter().find_map(|a| a.strip_prefix("--player=")?.parse().ok()).unwrap_or(1).max(1);
    let player_given = args.iter().any(|a| a.starts_with("--player="));
    let controller: Option<u32> = args.iter().find_map(|a| a.strip_prefix("--controller=")?.parse().ok());
    gamekit::parse_args(&args);
    args.retain(|a| !a.starts_with("--player=") && !a.starts_with("--controller=") && !a.starts_with("--gk-peer="));
    let _ = window::PLAYER.set(player);
    // --windowed: start in a window (default: full screen). Test runs
    // (--frames) and Local Match windows always start windowed.
    let _ = window::WINDOWED.set(args.iter().any(|a| a == "--windowed") || frames.is_some() || player_given);
    args.retain(|a| a != "--windowed");
    // --scale=N: the game draws N times its 480 x 320 (sharper picture);
    // default: picked from the desktop size (1080p: 4). --scale=1 = original.
    if let Some(n) = args.iter().find_map(|a| a.strip_prefix("--scale=")?.parse::<i32>().ok()) {
        let _ = gfx::RENDER_SCALE.set(n.clamp(1, 8));
    }
    args.retain(|a| !a.starts_with("--scale="));
    let _ = window::ONLY_PAD.set(match controller {
        Some(0) => None,
        Some(n) => Some(n as usize - 1),
        // --player=N given: controller N (so two windows never share a pad).
        None if player_given => Some(player as usize - 1),
        None => None,
    });
    // --tap=F:x,y: tap at frame F, landscape game points (480 x 320).
    // --hold=F1-F2:x,y[>x2,y2]: finger down from F1 to F2 (optionally dragged).
    let taps = touch::parse_taps(&args);
    args.retain(|a| !a.starts_with("--tap=") && !a.starts_with("--hold="));
    // --key=F1-F2:NAME: hold a key (SDL name) from frame F1 to F2.
    input::parse_keys(&args);
    args.retain(|a| !a.starts_with("--key="));
    // --shots=F1,F2,...: also save these frames, as <screenshot stem>_F.bmp.
    let shots: Vec<u64> = args
        .iter()
        .find_map(|a| a.strip_prefix("--shots="))
        .map(|v| v.split(',').filter_map(|f| f.parse().ok()).collect())
        .unwrap_or_default();
    args.retain(|a| !a.starts_with("--shots="));
    // --coverage=FILE: the game functions that ran, written at the end
    // (--coverage-from=F: count only from presented frame F on).
    if let Some(p) = args.iter().find_map(|a| a.strip_prefix("--coverage=")) {
        let _ = COV_PATH.set(PathBuf::from(p));
    }
    if let Some(f) = args.iter().find_map(|a| a.strip_prefix("--coverage-from=")).and_then(|v| v.parse::<u64>().ok()) {
        COV_FROM.store(f, std::sync::atomic::Ordering::Relaxed);
    }
    args.retain(|a| !a.starts_with("--coverage"));
    // --dump=F:FILE: guest memory (image and heap) after presented frame F.
    if let Some((f, p)) = args.iter().find_map(|a| a.strip_prefix("--dump=")).and_then(|v| v.split_once(':')) {
        let _ = DUMP.set((f.parse().expect("--dump=FRAME:FILE"), PathBuf::from(p)));
    }
    args.retain(|a| !a.starts_with("--dump="));
    // --fps=60 (default), 40 or 20: frames drawn per second. The game runs
    // at 20 steps a second; above 20, in-between frames are drawn with the
    // camera, models and animation blended between steps (interp.rs).
    let fps: u32 = args.iter().find_map(|a| a.strip_prefix("--fps=")?.parse().ok()).unwrap_or(60);
    let _ = FPS.set(fps.clamp(20, 60) / 20 * 20);
    args.retain(|a| !a.starts_with("--fps="));
    // --mute: sound runs but the output is silent (for test runs).
    let mute = args.iter().any(|a| a == "--mute");
    args.retain(|a| a != "--mute");
    // --dump-textures: save the game's 2D images to <build>/textures/dump
    // (textures.rs); replacements are read from <build>/textures/load.
    let dump_textures = args.iter().any(|a| a == "--dump-textures");
    args.retain(|a| a != "--dump-textures");
    // --extract-textures: save every 3D texture from the .ipa to the same
    // folder, named the same way, then quit (mcts.rs). No playing needed.
    let extract_textures = args.iter().any(|a| a == "--extract-textures");
    args.retain(|a| a != "--extract-textures");
    // --upscale=off: no built-in enlargement of the 2D images (files in
    // textures/load are still used).
    let upscale = !args.iter().any(|a| a == "--upscale=off");
    args.retain(|a| !a.starts_with("--upscale="));
    let build = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("build"));
    // A relative build folder that isn't in the current folder: look next to
    // the project this exe was built in (target/release/ -> ../../).
    let build = if build.is_relative() && !build.join("recomp/images.json").exists() {
        std::env::current_exe()
            .ok()
            .and_then(|e| Some(e.parent()?.parent()?.parent()?.join(&build)))
            .filter(|b| b.join("recomp/images.json").exists())
            .unwrap_or(build)
    } else {
        build
    };
    textures::init(&build, dump_textures, upscale);
    if let Err(e) = game::load(&build) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    // Hooks and REVS_TRACE_FN marks go into the game's coverage table (needs the game loaded).
    trace_setup();
    // The default home lives inside that build folder.
    let default_home = home == PathBuf::from("build/home");
    let home = match (default_home, player) {
        (true, 1) => build.join("home"),
        (true, n) => build.join(format!("home_p{n}")),
        (false, _) => home,
    };
    // A new player's folder starts with a copy of player 1's save.
    if default_home && player > 1 && import_from.is_none() && !home.join("Documents").join("savevs7.dat").exists() {
        let first = build.join("home");
        if first.join("Documents").join("savevs7.dat").exists() {
            if let Err(e) = import::import_save(&first, &home) {
                println!("player {player}: couldn't copy player 1's save: {e}");
            }
        }
    }
    if let Some(src) = &import_from {
        if let Err(e) = import::import_save(src, &home) {
            eprintln!("import: {e}");
            std::process::exit(1);
        }
    }
    let list = build.join("recomp/images.json");
    let images: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&list).unwrap_or_else(|e| panic!("{}: {e} (run `revs-analyze recompile` first)", list.display())))
            .unwrap();

    let base = reserve_memory(SIZE);
    assert!(!base.is_null(), "could not allocate 4 GiB of guest memory");
    let mut h = Host {
        base,
        fn_names: Vec::new(),
        fn_index: HashMap::new(),
        stubs: HashMap::new(),
        addr_host: HashMap::new(),
        data_names: Vec::new(),
        data_index: HashMap::new(),
        heap: os::Heap::new(HEAP_START, HEAP_END),
        uses: HashMap::new(),
        misses: HashMap::new(),
        strict,
        sjlj_head: 0,
        atexit: Vec::new(),
        tls: Vec::new(),
        objc: objc::Runtime::new(),
        ui: uikit::Ui::new(),
        gfx: None,
        gl_warned: false,
        fs: fs::Fs::new(),
        audio: audio::Audio::new(),
        touch: touch::Touch::default(),
    };
    h.touch.script = taps;
    h.audio.mute = mute;
    h.ui.max_frames = frames;
    h.ui.screenshot = screenshot;
    h.ui.shots = shots;
    h.objc.ipa = build.join(images[0]["path"].as_str().unwrap());
    let ipa = h.objc.ipa.clone();
    if let Err(e) = h.fs.mount(&ipa, home.clone()) {
        println!("files: {e}");
    } else {
        println!("files: bundle {} from the .ipa (read-only), home {}", h.fs.bundle_path, home.display());
    }
    if extract_textures {
        let out = build.join("textures").join("dump");
        let n = mcts::extract(&mut h.fs, &out);
        println!("textures: saved {n} 3D textures to {} (list: index.txt)", out.display());
        return;
    }

    // 1. Load every image.
    let mut loaded = Vec::new();
    for im in &images {
        let path = build.join(im["path"].as_str().unwrap());
        let b = im["base"].as_str().and_then(|b| u32::from_str_radix(b.trim_start_matches("0x"), 16).ok());
        let bin = image::load(&path, b).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for &(vm, _, fo, fsz) in bin.segments() {
            if fsz > 0 {
                unsafe { std::ptr::copy_nonoverlapping(bin.data.as_ptr().add(fo as usize), h.at(vm), fsz as usize) };
            }
        }
        let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        println!("loaded {name} at {:#x}", bin.base());
        loaded.push(Loaded { name, bin });
    }

    // 2. What the libraries export: name -> address (Thumb bit from the symbol table).
    let mut exports: HashMap<String, u32> = HashMap::new();
    for l in loaded.iter().skip(1) {
        let mut syms: HashMap<&str, u32> = HashMap::new();
        for s in &l.bin.text_symbols {
            syms.insert(&s.name, s.addr | s.thumb as u32);
        }
        for s in &l.bin.data_symbols {
            syms.entry(&s.name).or_insert(s.addr);
        }
        for (n, a) in &l.bin.exports {
            exports.entry(n.clone()).or_insert(syms.get(n.as_str()).copied().unwrap_or(*a));
        }
    }

    // Library helpers that run natively also when the library calls them itself.
    for l in loaded.iter().skip(1) {
        for s in &l.bin.text_symbols {
            let short = s.name.strip_prefix('_').unwrap_or(&s.name);
            if os::OVERRIDE.contains(&short) {
                let i = h.host_fn(short);
                h.addr_host.insert(s.addr & !1, i);
            }
        }
    }

    // 3. Link (dyld's job): stubs and bound pointer slots of every image.
    let (mut to_lib, mut to_host, mut to_data, mut to_objc) = (0, 0, 0, 0);
    for l in &loaded {
        let stub_names: Vec<(u32, String)> = l
            .bin
            .imports
            .iter()
            .filter(|i| i.kind == "stub")
            .filter_map(|i| Some((u32::from_str_radix(i.addr.trim_start_matches("0x"), 16).ok()?, i.name.clone())))
            .collect();
        for (stub, name) in stub_names {
            let short = name.strip_prefix('_').unwrap_or(&name);
            let t = match exports.get(&name).filter(|_| !os::OVERRIDE.contains(&short)) {
                Some(&a) => {
                    to_lib += 1;
                    Target::Guest(a)
                }
                None => {
                    to_host += 1;
                    Target::Host(h.host_fn(name.strip_prefix('_').unwrap_or(&name)))
                }
            };
            h.stubs.insert(stub, t);
        }
        let mut binds: Vec<(&u32, &String)> = l.bin.binds.iter().collect();
        binds.sort();
        let fn_like: std::collections::HashSet<&str> =
            l.bin.imports.iter().filter(|i| i.kind == "stub").map(|i| i.name.as_str()).collect();
        for (&slot, name) in binds {
            let short = name.strip_prefix('_').unwrap_or(name);
            let v = if let Some(&a) = exports.get(name.as_str()).filter(|_| !os::OVERRIDE.contains(&short)) {
                to_lib += 1;
                a
            } else if let Some(a) = objc::bind(&mut h, short) {
                to_objc += 1;
                a
            } else if fn_like.contains(name.as_str()) || os::NATIVE.contains(&short) || fs::NATIVE.contains(&short) || audio::NATIVE.contains(&short) || net::NATIVE.contains(&short) || os::OVERRIDE.contains(&short) || objc::NATIVE.contains(&short) {
                to_host += 1;
                HOST_FN + 4 * h.host_fn(short)
            } else {
                to_data += 1;
                h.data_block(short)
            };
            h.wr32(slot, v.wrapping_add(l.bin.bind_addends.get(&slot).copied().unwrap_or(0) as u32));
        }
    }
    println!(
        "linked: {to_lib} to library exports, {to_host} to OS functions, {to_objc} to Apple classes, {to_data} to {} OS data symbols",
        h.data_names.len()
    );
    // The game's classes, categories and selectors (needs the class binds above).
    objc::load(&mut h, &loaded[0].bin);
    nsfile::fill_data_symbols(&mut h);

    // argv / envp on the stack.
    let top = STACK + STACK_SIZE;
    let prog = top - 64;
    for (k, b) in b"BioVS_en\0".iter().enumerate() {
        h.wr8(prog + k as u32, *b);
    }
    let argv = top - 128;
    h.wr32(argv, prog);
    h.wr32(argv + 4, 0);
    h.wr32(argv + 8, 0); // envp
    let sp = top - 256;

    let names = h.fn_names.clone();
    if let Some(secs) = watchdog {
        let main_thread = watchdog::main_thread();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs.saturating_sub(1)));
            // Where the main thread is in the host code (offsets into the exe,
            // named with target/release/revs-run.map).
            let rvas: Vec<String> = (0..2).map(|_| format!("{:#x}", watchdog::sample_rva(main_thread))).collect();
            println!("watchdog host code offsets: {}", rvas.join(" "));
            // REVS_PROFILE=N: N more samples of the host offset, 3 ms apart (name
            // them with the linker map: rc_f_<addr> is a game function).
            if let Some(n) = std::env::var("REVS_PROFILE").ok().and_then(|v| v.parse::<usize>().ok()) {
                let mut hist: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
                for _ in 0..n {
                    *hist.entry(watchdog::sample_rip(main_thread)).or_default() += 1;
                    std::thread::sleep(std::time::Duration::from_millis(3));
                }
                let mut v: Vec<_> = hist.into_iter().collect();
                v.sort_by_key(|e| std::cmp::Reverse(e.1));
                let line: Vec<String> = v.iter().map(|(a, k)| format!("{a:#x}:{k}")).collect();
                println!("watchdog profile: {}", line.join(" "));
            }
            // Sample the last OS call 40 times over one second: where the time goes.
            let mut seen: Vec<(u64, u32)> = Vec::new();
            for _ in 0..40 {
                let v = LAST_CALL.load(std::sync::atomic::Ordering::Relaxed);
                match seen.iter_mut().find(|e| e.0 == v) {
                    Some(e) => e.1 += 1,
                    None => seen.push((v, 1)),
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            seen.sort_by_key(|e| std::cmp::Reverse(e.1));
            for (v, n) in seen.iter().take(8) {
                let what = names.get((v & 0xFFFF_FFFF) as usize).map_or("none", String::as_str);
                println!("watchdog sample: {n:2}/40  {what} from {:#x}", v >> 32);
            }
            let v = LAST_CALL.load(std::sync::atomic::Ordering::Relaxed);
            let what = names.get((v & 0xFFFF_FFFF) as usize).map_or("none", String::as_str);
            let a: Vec<String> = LAST_ARGS.iter().map(|x| format!("{:#x}", x.load(std::sync::atomic::Ordering::Relaxed))).collect();
            println!(
                "watchdog: still running after {secs} s ({} OS calls); last OS call {what}({}) from {:#x}",
                CALLS.load(std::sync::atomic::Ordering::Relaxed),
                a.join(", "),
                v >> 32
            );
            // The UI state (read while the main thread is most likely asleep).
            let h = host();
            for l in h.ui.log.iter().rev().take(15).rev() {
                println!("watchdog ui: {l}");
            }
            let now = h.ui.now();
            for t in &h.ui.timers {
                println!(
                    "watchdog timer {:#x}: every {:.3} s, valid {}, scheduled {}, repeats {}, due in {:.3} s",
                    t.obj, t.interval, t.valid, t.scheduled, t.repeats, t.next - now
                );
            }
            if let Some(g) = &h.gfx {
                println!("watchdog: frames presented {}", g.frames);
            }
            objc::report(h);
            coverage_write();
            std::process::exit(2);
        });
    }
    unsafe { HOST = &mut h };
    // 4. Static initialisers: libraries first (dyld runs dependencies first), then the game.
    let mut report = Vec::new();
    let order: Vec<usize> = (1..loaded.len()).chain(std::iter::once(0)).collect();
    for i in order {
        let l = &loaded[i];
        let Some(s) = l.bin.section("__DATA", "__mod_init_func") else { continue };
        let (mut ok, mut stopped) = (0, 0);
        for k in 0..s.size / 4 {
            let f = host().rd32(s.addr + 4 * k);
            match run(host(), f, [1, argv, argv + 8, 0], sp) {
                Ok(_) => ok += 1,
                Err(e) => {
                    stopped += 1;
                    report.push(format!("  {} initialiser {f:#x}: {e}", l.name));
                }
            }
        }
        println!("{}: {ok} static initialisers ran to the end, {stopped} stopped", l.name);
    }
    let main_fn = loaded[0].bin.text_symbols.iter().find(|s| s.name == "_main").map(|s| s.addr | s.thumb as u32);
    let outcome = match main_fn.map(|m| run(host(), m, [1, argv, argv + 8, 0], sp)) {
        Some(Ok(r)) => format!("main returned {r}"),
        Some(Err(e)) => format!("main stopped: {e}"),
        None => "no _main symbol".to_string(),
    };
    println!("{outcome}");
    for l in report.iter().take(20) {
        println!("{l}");
    }

    let h = host();
    let mut uses: Vec<(&String, &Use)> = h.uses.iter().collect();
    uses.sort_by_key(|u| u.1.order);
    let missing = uses.iter().filter(|u| !u.1.native).count();
    println!(
        "\nOS functions used: {} ({} with a native version, {missing} without), in first-use order:",
        uses.len(),
        uses.len() - missing
    );
    for (n, u) in &uses {
        println!(
            "  {:>3} {:<7} {:>8} calls  first from {:#x}  {n}",
            u.order,
            if u.native { "native" } else { "MISSING" },
            u.calls,
            u.first_lr
        );
    }
    let mut misses: Vec<_> = h.misses.iter().collect();
    misses.sort_by_key(|m| m.1 .0);
    for (a, (_, n)) in misses.iter().take(20) {
        let what = data_name(h, **a).map(|n| format!("imported data {n}")).unwrap_or_else(|| "untranslated code".into());
        println!("  call to {a:#x} ({what}): {n} times");
    }
    objc::report(h);
    fs::report(h);
    for l in &h.touch.log {
        println!("  {l}");
    }
    for l in &h.audio.log {
        println!("  {l}");
    }
    println!("audio: {} frames out, peak {}{}", h.audio.frames_out, h.audio.peak, if h.audio.mute { " (muted)" } else { "" });
    for l in &h.ui.log {
        println!("  {l}");
    }
    if let Some(g) = &h.gfx {
        println!("frames presented: {}", g.frames);
        if let Some(r) = interp::report() {
            println!("{r}");
        }
        if g.extra_frames > 0 {
            println!("in-between frames (--fps): {}", g.extra_frames);
        }
    }
    println!("heap: {} KB in use", (h.heap.used() + 1023) / 1024);
    coverage_write();
    // Repeated last, so the reason the run ended is never scrolled away.
    println!("end of run: {outcome}");
}

// ---- --coverage: which game functions ran (RC_ENTER in the generated C; game::cov).
static COV_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
pub static COV_FROM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

/// Called after each presented frame: clears the coverage at --coverage-from
/// and writes the --dump file.
pub fn coverage_frame(frames: u64) {
    if frames == COV_FROM.load(std::sync::atomic::Ordering::Relaxed) {
        // Keep the REVS_TRACE_FN marks (bit 1).
        unsafe { (*game::cov()).iter_mut().for_each(|b| *b &= 2) };
    }
    if let Some((f, p)) = DUMP.get() {
        if *f == frames {
            dump_memory(p);
        }
    }
}

// ---- REVS_TRACE_FN=0xADDR,...: log each call of these game functions.
/// Functions REVS_TRACE_FN asked to log (others marked are hooks only).
static TRACED: std::sync::OnceLock<std::collections::HashSet<u32>> = std::sync::OnceLock::new();

fn trace_setup() {
    let list = std::env::var("REVS_TRACE_FN").unwrap_or_default();
    let traced: std::collections::HashSet<u32> =
        list.split(',').filter_map(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()).collect();
    // Hooks: lyt.rs sees every layout root just before it is drawn.
    for &a in traced.iter().chain([lyt::DRAW, RENDER].iter()).chain(STEP_ONLY.iter()).chain(lyt::SCREEN_HOOKS.iter()).chain(tweaks::HOOKS.iter()) {
        if a < 0x40_0000 {
            unsafe { (*game::cov())[(a >> 1) as usize] |= 2 };
        }
    }
    add_hook(MIS_DRAW);
    let _ = TRACED.set(traced);
}

/// Frames drawn per second (--fps): 20, 40 or 60.
static FPS: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// In-between frames per game step: 0, 1 or 2 from --fps;
/// REVS_EXTRA_RENDER=N overrides it (tests).
pub fn extra_frames() -> u32 {
    static N: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("REVS_EXTRA_RENDER").ok().and_then(|v| v.parse().ok()).unwrap_or(FPS.get().copied().unwrap_or(20) / 20 - 1)
    })
}

/// Blending of in-between frames: 1 (delay, the default with in-between
/// frames) or 0 (off); REVS_INTERP=0/1/2 overrides it (2 = predict, tests).
pub fn interp_mode() -> u8 {
    std::env::var("REVS_INTERP").ok().and_then(|v| v.parse().ok()).unwrap_or(if extra_frames() > 0 { 1 } else { 0 })
}

/// `GM_MisConduct::Draw(GM_CtrlMain*)`: draws the HUD. It reads the step's
/// event (Climb Up, Raise Up...) from this +0x46 and sets it to -1 once
/// used; with -1 it leaves out the message window. In-between frames get the
/// event their step's frame saw (EVENT), so they draw the window too.
const MIS_DRAW: u32 = 0x165e04;
static EVENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0xff);

/// REVS_FORCE_EVENT=N (test only): every step's first Render sees event N,
/// as if the player stood at a ladder, so the prompt can be checked anywhere.
fn force_event() -> Option<u8> {
    static F: std::sync::OnceLock<Option<u8>> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var("REVS_FORCE_EVENT").ok().and_then(|v| v.parse().ok()))
}

/// Makes rc_trace run at the entry of game function `a` (a hook, not logged).
pub fn add_hook(a: u32) {
    if a < 0x40_0000 {
        unsafe { (*game::cov())[(a >> 1) as usize] |= 2 };
    }
}

static TRACE_LINES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Called by RC_ENTER for a traced function: frame, address, r0-r3, return address.
#[no_mangle]
pub(crate) unsafe extern "C" fn rc_trace(c: *mut Cpu, addr: u32) -> i32 {
    // In-between frames leave out the game logic that lives in drawing code.
    if STEP_ONLY.contains(&addr) && IN_EXTRA.load(std::sync::atomic::Ordering::Relaxed) {
        return 1;
    }
    if addr == MIS_DRAW {
        let at = (*c).r[0] + 0x46;
        if IN_EXTRA.load(std::sync::atomic::Ordering::Relaxed) {
            host().wr8(at, EVENT.load(std::sync::atomic::Ordering::Relaxed));
        } else {
            if let Some(n) = force_event() {
                host().wr8(at, n);
            }
            EVENT.store(host().rd8(at), std::sync::atomic::Ordering::Relaxed);
        }
    }
    if addr == lyt::DRAW {
        lyt::on_draw(host(), (*c).r[0]);
    }
    if lyt::SCREEN_HOOKS.contains(&addr) {
        lyt::on_update(host(), addr, (*c).r[1]);
    }
    if tweaks::HOOKS.contains(&addr) && tweaks::on_enter(host(), addr, (*c).r[0], (*c).r[1]) {
        return 1;
    }
    if addr == RENDER {
        tweaks::each_frame(host());
        RENDER_APP.store((*c).r[0], std::sync::atomic::Ordering::Relaxed);
    }
    if [interp::SET_G3D, interp::SET_CAMERA, interp::DRAW_FIGURE, interp::SET_FRAME, interp::SET_FRAME_ALL, interp::MODEL_DIRECT_DRAW, interp::MODEL_DRAW]
        .contains(&addr)
    {
        interp::on_enter(host(), &mut *c, addr);
    }
    if !TRACED.get().is_some_and(|t| t.contains(&addr)) {
        return 0;
    }
    // REVS_TRACE_FROM=F: start at presented frame F.
    static FROM: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let from = *FROM.get_or_init(|| std::env::var("REVS_TRACE_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(0));
    if host().gfx.as_ref().map_or(0, |g| g.frames) < from {
        return 0;
    }
    if TRACE_LINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 2000 {
        return 0;
    }
    let c = &*c;
    let frame = host().gfx.as_ref().map_or(0, |g| g.frames);
    println!("trace f{frame} {addr:#x}(r0={:#x}, r1={:#x}, r2={:#x}, r3={:#x}) lr={:#x}", c.r[0], c.r[1], c.r[2], c.r[3], c.r[14]);
    // REVS_TRACE_VEC=1: also the three floats each argument points to (a vector, if it is one).
    if std::env::var_os("REVS_TRACE_VEC").is_some() {
        let h = host();
        let mut line = String::from("     ");
        for i in 0..4 {
            let p = c.r[i];
            if (0x1000..0xF100_0000).contains(&p) && p % 4 == 0 {
                let v: Vec<f32> = (0..3).map(|k| f32::from_bits(h.rd32(p + 4 * k))).collect();
                line += &format!(" *r{i}=({:.2}, {:.2}, {:.2})", v[0], v[1], v[2]);
            }
        }
        println!("{line}");
    }
    // REVS_TRACE_MTX=N: the 16 floats r<N> points to (an engine transform).
    if let Some(n) = std::env::var("REVS_TRACE_MTX").ok().and_then(|v| v.parse::<usize>().ok()) {
        let h = host();
        let p = c.r[n.min(3)];
        let m: Vec<String> = (0..16).map(|k| format!("{:.2}", f32::from_bits(h.rd32(p + 4 * k)))).collect();
        println!("      mtx r{n}: [{}]", m.join(", "));
    }
    0
}

// ---- REVS_RENDER_DIFF=F: what one extra CT_Application::Render() writes.
/// CT_Application::Render(): onTick calls it once per 20 Hz game step.
pub const RENDER: u32 = 0x13f78;
/// Game logic that runs inside Render(), once per step. GM_MisConduct::Draw
/// takes the step's event (GM_CtrlMain +0x46: Climb Up, melee...), passes it
/// to the message window and the action button, then sets it to -1: the
/// first Render of a step uses it up. In-between frames (REVS_EXTRA_RENDER)
/// skip these, so they keep what the step's frame set; run with -1 they hid
/// the button (the prompt stopped working) and cleared the message window
/// (it flashed). SetItemInfo is the game's other SkipRendering call.
/// `GM_CtrlLyt_Input::Set_EveBtn(GM_CtrlMain*, long)`,
/// `GM_CtrlLayout::Set_WinActMess(GM_CtrlMain*, long)`,
/// `GM_CtrlMain_Event::SetItemInfo(GM_CtrlMain*)`.
pub const STEP_ONLY: [u32; 3] = [0x6b99c, 0x63f04, 0x127528];
/// Set while the run loop draws an in-between frame.
pub static IN_EXTRA: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Its `this` (r0), seen the last time the game called it.
pub static RENDER_APP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Once the presented frame count reaches F: copies the game image and heap,
/// calls Render() one more time, and prints the address ranges it changed.
pub fn render_diff(h: &mut Host, c: &mut Cpu) {
    static AT: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Some(at) = *AT.get_or_init(|| std::env::var("REVS_RENDER_DIFF").ok().and_then(|v| v.parse().ok())) else { return };
    let app = RENDER_APP.load(std::sync::atomic::Ordering::Relaxed);
    if app == 0 || h.gfx.as_ref().map_or(0, |g| g.frames) < at || DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let regions = [(0u32, 0x40_0000u32), (HEAP_START, h.heap.top() - HEAP_START)];
    let snap = |h: &Host| -> Vec<Vec<u8>> {
        regions.iter().map(|&(s, l)| unsafe { std::slice::from_raw_parts(h.base.add(s as usize), l as usize) }.to_vec()).collect()
    };
    let before = snap(h);
    guest_call(c, RENDER, &[app]);
    let after = snap(h);
    let mut total = 0usize;
    println!("render diff at frame {at}: this={app:#x}");
    for (k, &(start, _)) in regions.iter().enumerate() {
        // Changed words, merged into ranges when less than 64 bytes apart.
        let (b, a) = (&before[k], &after[k]);
        let mut ranges: Vec<(u32, u32, usize)> = Vec::new();
        for off in (0..b.len().min(a.len()) & !3).step_by(4) {
            if b[off..off + 4] != a[off..off + 4] {
                let addr = start + off as u32;
                match ranges.last_mut() {
                    Some(r) if addr - r.1 < 64 => { r.1 = addr + 4; r.2 += 1; }
                    _ => ranges.push((addr, addr + 4, 1)),
                }
            }
        }
        for r in &ranges {
            total += r.2;
            let first = u32::from_le_bytes(b[(r.0 - start) as usize..][..4].try_into().unwrap());
            let now = u32::from_le_bytes(a[(r.0 - start) as usize..][..4].try_into().unwrap());
            println!("  {:#010x}..{:#010x} {} words (first {first:#x} -> {now:#x})", r.0, r.1, r.2);
        }
    }
    println!("render diff: {total} words changed");
}

// ---- --dump: "REVSDMP1", then regions (u32 start, u32 length, bytes), little-endian.
static DUMP: std::sync::OnceLock<(u64, PathBuf)> = std::sync::OnceLock::new();

fn dump_memory(p: &Path) {
    let h = host();
    let regions = [(0u32, 0x40_0000u32), (HEAP_START, h.heap.top() - HEAP_START)];
    let mut out = b"REVSDMP1".to_vec();
    for (start, len) in regions {
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(h.base.add(start as usize), len as usize) });
    }
    match std::fs::write(p, &out) {
        Ok(()) => println!("dump: {} MB written to {}", out.len() >> 20, p.display()),
        Err(e) => println!("dump: {e}"),
    }
}

fn coverage_write() {
    let Some(p) = COV_PATH.get() else { return };
    let cov = unsafe { &*game::cov() };
    let list: Vec<String> = cov.iter().enumerate().filter(|e| *e.1 & 1 != 0).map(|(i, _)| format!("{:#x}", i * 2)).collect();
    match std::fs::write(p, list.join("
")) {
        Ok(()) => println!("coverage: {} game functions ran, written to {}", list.len(), p.display()),
        Err(e) => println!("coverage: {e}"),
    }
}
