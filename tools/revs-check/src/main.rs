//! revs-check: runs every recompiled function on touchHLE's ARM CPU (dynarmic)
//! and as compiled C, from the same registers and memory, and compares.
//!
//! Memory: one 4 GiB guest block with Windows write watch. After each run the
//! OS lists every page that was written; those pages are compared and then
//! put back to their original content, so runs never see each other's writes.
//!
//! RULE: reads the game from the user's own .ipa; the compiled-in C comes from
//! the git-ignored build folder. Nothing here is shared.
//!
//! Usage: revs-check <game.ipa> [build_dir] [inputs_per_function] [only: addr | list.json] [--replay=rec.bin]
//!   --replay: inputs from a touchHLE recording (REVS_RECORD) instead of random;
//!   inputs_per_function is then the most samples used per function.
//!   REVS_TRACE=1 prints each function before it runs.

use std::collections::{BTreeSet, HashMap};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::path::PathBuf;

use revs_analyze::{ipa, macho};

// ---- dynarmic (touchHLE's wrapper) ----
type Wrapper = c_void;
extern "C" {
    fn touchHLE_DynarmicWrapper_new(direct: *mut c_void, null_pages: usize) -> *mut Wrapper;
    fn touchHLE_DynarmicWrapper_invalidate_cache_range(cpu: *mut Wrapper, start: u32, size: u32);
    fn touchHLE_DynarmicWrapper_swap_context(cpu: *mut Wrapper, ctx: *mut DynCtx);
    fn touchHLE_DynarmicWrapper_run_or_step(cpu: *mut Wrapper, mem: *mut c_void, ticks: Option<&mut u64>) -> i32;
}

/// touchHLE_DynarmicContext: the whole CPU state, swapped in and out.
#[repr(C)]
#[derive(Clone, Copy)]
struct DynCtx {
    regs: [u32; 16],
    extregs: [u32; 64],
    cpsr: u32,
    fpscr: u32,
}

// Memory callbacks the wrapper needs (used when it can't access memory
// directly). `mem` is our guest base pointer.
unsafe fn at(mem: *mut c_void, addr: u32) -> *mut u8 {
    (mem as *mut u8).add(addr as usize)
}
#[no_mangle]
unsafe extern "C" fn touchHLE_cpu_read_u8(mem: *mut c_void, addr: u32, error: *mut bool) -> u8 {
    *error = false;
    at(mem, addr).read()
}
#[no_mangle]
unsafe extern "C" fn touchHLE_cpu_read_u16(mem: *mut c_void, addr: u32, error: *mut bool) -> u16 {
    *error = false;
    (at(mem, addr) as *const u16).read_unaligned()
}
/// Where dynarmic may fetch instructions: the game's code and the sentinel.
/// dynarmic reads memory directly; this callback is only used to fetch code,
/// so a run that jumps into data (random input) halts instead of executing it.
static CODE_START: AtomicU32 = AtomicU32::new(0);
static CODE_END: AtomicU32 = AtomicU32::new(0);
/// OS import stubs: fetching there means the run reached an OS function.
static STUB_START: AtomicU32 = AtomicU32::new(0);
static STUB_END: AtomicU32 = AtomicU32::new(0);
/// Set when a fetch was refused at a stub (cleared before each run).
static HIT_STUB: AtomicU32 = AtomicU32::new(0);
/// Set when a fetch was refused outside the game's code (cleared before each
/// run): a call into touchHLE's libraries or host code, or a wild jump.
static LEFT_CODE: AtomicU32 = AtomicU32::new(0);
/// The run started in Thumb state: code words are pairs of halfwords, so the
/// ARM coprocessor filter below doesn't apply (dynarmic fetches aligned words).
static THUMB_RUN: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
unsafe extern "C" fn touchHLE_cpu_read_u32(mem: *mut c_void, addr: u32, error: *mut bool) -> u32 {
    if addr >= STUB_START.load(Relaxed) && addr < STUB_END.load(Relaxed) {
        HIT_STUB.store(1, Relaxed);
        *error = true;
        return 0;
    }
    let code = addr >= CODE_START.load(Relaxed) && addr < CODE_END.load(Relaxed);
    *error = !code && addr != SENTINEL;
    if *error {
        LEFT_CODE.store(1, Relaxed);
        return 0;
    }
    // A word that is a coprocessor instruction other than VFP (cp10/11) or
    // CP15 makes dynarmic assert instead of reporting it. Real game code has
    // none, so only a wild jump (random input) gets here: refuse to fetch it.
    let w = (at(mem, addr) as *const u32).read_unaligned();
    let coproc = if THUMB_RUN.load(Relaxed) != 0 {
        // Thumb: a 32-bit coprocessor instruction whose halves are both in
        // this word (first halfword low) and isn't VFP/NEON/CP15.
        let (h1, h2) = (w & 0xFFFF, w >> 16);
        let cp = (h1 & 0xEE00 == 0xEC00 || h1 & 0xEF00 == 0xEE00) && !matches!((h2 >> 8) & 0xF, 10 | 11 | 15);
        cp && h1 >> 8 != 0xEF
    } else {
        let coproc = (w >> 24) & 0xF == 0xE || (w >> 25) & 7 == 6;
        coproc && (w >> 28 == 0xF || !matches!((w >> 8) & 0xF, 10 | 11 | 15))
    };
    if coproc {
        *error = true;
        return 0;
    }
    (at(mem, addr) as *const u32).read_unaligned()
}
#[no_mangle]
unsafe extern "C" fn touchHLE_cpu_read_u64(mem: *mut c_void, addr: u32, error: *mut bool) -> u64 {
    *error = false;
    (at(mem, addr) as *const u64).read_unaligned()
}
#[no_mangle]
unsafe extern "C" fn touchHLE_cpu_write_u8(mem: *mut c_void, addr: u32, v: u8) -> bool {
    at(mem, addr).write(v);
    false
}
#[no_mangle]
unsafe extern "C" fn touchHLE_cpu_write_u16(mem: *mut c_void, addr: u32, v: u16) -> bool {
    (at(mem, addr) as *mut u16).write_unaligned(v);
    false
}
#[no_mangle]
unsafe extern "C" fn touchHLE_cpu_write_u32(mem: *mut c_void, addr: u32, v: u32) -> bool {
    (at(mem, addr) as *mut u32).write_unaligned(v);
    false
}
#[no_mangle]
unsafe extern "C" fn touchHLE_cpu_write_u64(mem: *mut c_void, addr: u32, v: u64) -> bool {
    (at(mem, addr) as *mut u64).write_unaligned(v);
    false
}

// ---- recompiled code ----
#[repr(C)]
struct Cpu {
    r: [u32; 16],
    n: u32,
    z: u32,
    c: u32,
    v: u32,
    m: *mut u8,
    budget: i32,
    halt: u32,
    halt_addr: u32,
    ext: [u32; 64],
    fpscr: u32,
}
/// FPSCR bits compared: NZCV and the control bits. The cumulative exception
/// flags (bits 0-4, 7) aren't modelled by the C.
const FPSCR_CMP: u32 = !0x9F;
const RC_HALT_TRAP: u32 = 2;
const RC_HALT_IMPORT: u32 = 3;
type RcFn = unsafe extern "C" fn(*mut Cpu);

/// The checker has no OS layer: an OS call or a call to untranslated code
/// stops the C run there.
#[no_mangle]
unsafe extern "C" fn rc_import(c: *mut Cpu, stub: u32, _index: u32) {
    rc_trap(c, stub, RC_HALT_IMPORT);
}
#[no_mangle]
unsafe extern "C" fn rc_miss(c: *mut Cpu, target: u32) {
    rc_trap(c, target, RC_HALT_TRAP);
}
extern "C" {
    fn rc_lookup(addr: u32) -> Option<RcFn>;
    fn rc_trap(c: *mut Cpu, addr: u32, why: u32);
}

// ---- host memory with write watch ----
extern "system" {
    fn VirtualAlloc(addr: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
    fn GetWriteWatch(
        flags: u32,
        base: *mut c_void,
        size: usize,
        addrs: *mut *mut c_void,
        count: *mut usize,
        granularity: *mut u32,
    ) -> u32;
    fn ResetWriteWatch(base: *mut c_void, size: usize) -> u32;
}
const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_WRITE_WATCH: u32 = 0x0020_0000;
const PAGE_READWRITE: u32 = 0x04;
const PAGE: usize = 4096;
/// Written-page addresses fetched from the OS per call.
const CHUNK: usize = 4096;

/// Committed past the 4 GiB end, so an access that wraps or straddles the top
/// lands in memory (multi-word accesses near 0xFFFFFFFF on random inputs).
const GUARD: usize = 0x1_0000;
const SIZE: usize = (1 << 32) + GUARD;

// Test layout (guest addresses).
const SCRATCH: u32 = 0x1000_0000;
const SCRATCH_SIZE: u32 = 0x1_0000;
const STACK: u32 = 0x2000_0000;
const STACK_SIZE: u32 = 0x1_0000;
/// Fake return address (lr) holding `svc #0x5a5a`: header padding before
/// __text (VM 0x3000-0x3f80 is zero), never written by the game and never in a
/// recording (the recorder leaves out pages that touch __text).
const SENTINEL: u32 = 0x3000;
const SVC_MAGIC: u32 = 0x5A5A;

#[derive(Clone)]
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
    fn scratch_ptr(&mut self) -> u32 {
        SCRATCH + ((self.next() % (SCRATCH_SIZE / 2)) & !3)
    }
    /// VFP inputs: mostly ordinary floats, some small ints (for int -> float
    /// conversions), some special values, some raw bits.
    fn vfp_regs(&mut self) -> [u32; 64] {
        const SPECIAL: [u32; 10] = [
            0, 0x8000_0000, 0x3F80_0000, 0x7F80_0000, 0xFF80_0000, 0x7FC0_0000, 0x7F80_0001, 0x0000_0001, 0x7F7F_FFFF, 0xCF00_0000,
        ];
        let mut e = [0u32; 64];
        for w in e.iter_mut() {
            *w = match self.next() % 10 {
                0..=5 => ((self.next() as i32 as f32) / 2_147_483.6).to_bits(), // about -1000..1000
                6 | 7 => self.next() % 1000,
                8 => SPECIAL[(self.next() % 10) as usize],
                _ => self.next() ^ (self.next() << 16),
            };
        }
        e
    }
}

struct Guest {
    base: *mut u8,
    /// Original content of every page that doesn't start as zero.
    pristine: HashMap<usize, Box<[u8]>>,
    /// Ranges asked about after every run. Asking about all 4 GiB costs ~2 ms
    /// (the OS walks a million pages), so that happens once per function;
    /// `full` switches every run to the whole block (rechecking a function
    /// that wrote outside these ranges).
    hot: Vec<(usize, usize)>,
    full: bool,
}

impl Guest {
    fn mem(&self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.base, SIZE) }
    }
    /// Pages written in [start, end) since the last reset (offsets); resets.
    fn written_in(&self, start: usize, end: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut addrs = vec![std::ptr::null_mut::<c_void>(); CHUNK];
        let mut s = start;
        while s < end {
            let mut count = CHUNK;
            let mut gran = 0u32;
            let r = unsafe {
                GetWriteWatch(0, self.base.add(s) as *mut c_void, end - s, addrs.as_mut_ptr(), &mut count, &mut gran)
            };
            assert_eq!(r, 0, "GetWriteWatch failed");
            out.extend(addrs[..count].iter().map(|&p| p as usize - self.base as usize));
            if count < CHUNK {
                break;
            }
            s = out[out.len() - 1] + PAGE;
        }
        unsafe { ResetWriteWatch(self.base.add(start) as *mut c_void, end - start) };
        out
    }
    /// Pages written since the last call.
    fn take_written(&self) -> Vec<usize> {
        if self.full {
            return self.written_in(0, SIZE);
        }
        self.hot.iter().flat_map(|&(a, b)| self.written_in(a, b)).collect()
    }
    fn restore(&self, pages: &[usize]) {
        let mem = self.mem();
        for &p in pages {
            match self.pristine.get(&p) {
                Some(orig) => mem[p..p + PAGE].copy_from_slice(orig),
                None => mem[p..p + PAGE].fill(0),
            }
        }
    }
    fn page(&self, p: usize) -> &[u8] {
        &self.mem()[p..p + PAGE]
    }
    fn pristine_page(&self, p: usize) -> Vec<u8> {
        self.pristine.get(&p).map_or_else(|| vec![0; PAGE], |b| b.to_vec())
    }
}

/// What one run leaves behind, for comparing.
struct Outcome {
    regs: [u32; 14], // r0-r13
    nzcv: u32,
    ext: [u32; 64],
    fpscr: u32,
    pages: HashMap<usize, Vec<u8>>,
}

/// One starting state for a function.
struct Input {
    regs: [u32; 16],
    nzcv: u32,
    ext: [u32; 64],
    fpscr: u32,
    /// Start in Thumb state.
    thumb: bool,
}

/// Result of one input.
enum One {
    Pass,
    /// Matched up to a call into an OS function (both stopped at its stub).
    PassAtCall,
    Differ(String),
    CHalt(String),
    /// Reached an OS function or untranslated code: nothing to compare.
    Reached,
    /// Left the game's code (a library / host call, or a wild jump).
    LeftCode,
    /// dynarmic didn't come back in time.
    NoReturn,
}

struct Ctx {
    cpu: *mut Wrapper,
    text: std::ops::Range<u32>,
    inputs: u32,
    /// Thumb functions (from functions.json).
    thumb: std::collections::HashSet<u32>,
}

impl Ctx {
    fn restore(&self, g: &Guest, pages: &[usize]) {
        g.restore(pages);
        let t = &self.text;
        if pages.iter().any(|&p| (p as u32) < t.end && (p + PAGE) as u32 > t.start) {
            unsafe { touchHLE_DynarmicWrapper_invalidate_cache_range(self.cpu, t.start, t.end - t.start) };
        }
        g.take_written(); // the restore itself wrote these pages
    }

    /// Runs the function from one state on dynarmic and as C, compares, and
    /// puts memory back.
    fn run_one(&self, g: &Guest, cfn: RcFn, inp: &Input) -> One {
        // 1. dynarmic
        let (dyn_ctx, res) = unsafe {
            let t = if inp.thumb { 0x20 } else { 0 };
            let mut ctx = DynCtx { regs: inp.regs, extregs: inp.ext, cpsr: 0x10 | t | (inp.nzcv << 28), fpscr: inp.fpscr };
            THUMB_RUN.store(inp.thumb as u32, Relaxed);
            touchHLE_DynarmicWrapper_swap_context(self.cpu, &mut ctx);
            let mut ticks = ticks_limit();
            HIT_STUB.store(0, Relaxed);
            LEFT_CODE.store(0, Relaxed);
            let res = touchHLE_DynarmicWrapper_run_or_step(self.cpu, g.base as *mut c_void, Some(&mut ticks));
            touchHLE_DynarmicWrapper_swap_context(self.cpu, &mut ctx);
            (ctx, res)
        };
        let mut dyn_regs = [0u32; 14];
        dyn_regs.copy_from_slice(&dyn_ctx.regs[..14]);
        let dyn_written = g.take_written();
        let dyn_ok = res == SVC_MAGIC as i32;
        // dynarmic caches a block whose fetch was refused, so later runs fault
        // there without asking again: also go by where the PC is.
        let pc = dyn_ctx.regs[15];
        let in_stub = pc >= STUB_START.load(Relaxed) && pc < STUB_END.load(Relaxed);
        let in_code = pc >= CODE_START.load(Relaxed) && pc < CODE_END.load(Relaxed);
        let at_stub = !dyn_ok && (HIT_STUB.load(Relaxed) != 0 || (res == -2 && in_stub));
        let keep = dyn_ok || at_stub;
        let dyn_out = Outcome {
            regs: dyn_regs,
            nzcv: dyn_ctx.cpsr >> 28,
            ext: dyn_ctx.extregs,
            fpscr: dyn_ctx.fpscr,
            pages: if keep { dyn_written.iter().map(|&p| (p, g.page(p).to_vec())).collect() } else { HashMap::new() },
        };
        self.restore(g, &dyn_written);
        if !keep {
            return if LEFT_CODE.load(Relaxed) != 0 || (res == -2 && !in_code) {
                One::LeftCode
            } else {
                if std::env::var_os("REVS_TRACE").is_some() {
                    eprintln!("  out of time: res={res} pc={pc:#x} lr={:#x} sp={:#x}", dyn_ctx.regs[14], dyn_ctx.regs[13]);
                }
                One::NoReturn
            };
        }

        // 2. recompiled C
        let n = inp.nzcv;
        let mut c = Cpu {
            r: inp.regs,
            n: (n >> 3) & 1,
            z: (n >> 2) & 1,
            c: (n >> 1) & 1,
            v: n & 1,
            m: g.base,
            budget: ticks_limit() as i32,
            halt: 0,
            halt_addr: 0,
            ext: inp.ext,
            fpscr: inp.fpscr,
        };
        unsafe { cfn(&mut c) };
        let c_written = g.take_written();
        // Both stopped at the same OS function's stub (12 bytes each): compare
        // the state there, i.e. everything up to the call and its arguments.
        let boundary = at_stub && c.halt == RC_HALT_IMPORT && pc.wrapping_sub(c.halt_addr) < 12;
        if (c.halt == RC_HALT_TRAP || c.halt == RC_HALT_IMPORT || at_stub) && !boundary {
            self.restore(g, &c_written);
            return One::Reached;
        }
        let c_flags = (c.n << 3) | (c.z << 2) | (c.c << 1) | c.v;

        // 3. compare registers, flags and every page either side wrote
        let result = if c.halt != 0 && !boundary {
            One::CHalt(format!("C halted ({}) at {:#x}", c.halt, c.halt_addr))
        } else {
            let mut d = String::new();
            for k in 0..14 {
                if c.r[k] != dyn_out.regs[k] {
                    d += &format!(" r{k} dyn={:#x} c={:#x}", dyn_out.regs[k], c.r[k]);
                }
            }
            for k in 0..64 {
                if c.ext[k] != dyn_out.ext[k] {
                    d += &format!(" s{k} dyn={:#x} c={:#x}", dyn_out.ext[k], c.ext[k]);
                }
            }
            if (c.fpscr ^ dyn_out.fpscr) & FPSCR_CMP != 0 {
                d += &format!(" fpscr dyn={:#x} c={:#x}", dyn_out.fpscr, c.fpscr);
            }
            if c_flags != dyn_out.nzcv {
                d += &format!(" nzcv dyn={:04b} c={c_flags:04b}", dyn_out.nzcv);
            }
            let pages: BTreeSet<usize> = dyn_written.iter().chain(&c_written).copied().collect();
            for p in pages {
                let want = dyn_out.pages.get(&p).cloned().unwrap_or_else(|| g.pristine_page(p));
                let got = g.page(p);
                if let Some(off) = want.iter().zip(got).position(|(x, y)| x != y) {
                    d += &format!(" mem@{:#x} dyn={:#x} c={:#x}", p + off, want[off], got[off]);
                    break;
                }
            }
            match (d.is_empty(), boundary) {
                (true, false) => One::Pass,
                (true, true) => One::PassAtCall,
                (false, false) => One::Differ(d),
                (false, true) => One::Differ(format!(" (at the call to OS stub {pc:#x}){d}")),
            }
        };
        self.restore(g, &c_written);
        result
    }

    /// Runs one function on random inputs. Returns (verdict, detail, inputs
    /// compared, inputs where it reached untranslated code or an OS call).
    fn check(&self, g: &Guest, f: u32, cfn: RcFn, rng: &mut Rng) -> (&'static str, String, u32, u32) {
        let (mut ran, mut reached) = (0, 0);
        for i in 0..self.inputs {
            // r0-r3 pointers into scratch or small numbers; r4-r12 small.
            let mut regs = [0u32; 16];
            for (k, r) in regs.iter_mut().enumerate().take(13) {
                let ptr = rng.scratch_ptr();
                *r = match (i % 3, k) {
                    (0, 0..=3) => ptr,
                    (1, 0..=3) => rng.next() % 0x200,
                    (2, 0) => ptr,
                    _ => rng.next() % 0x100,
                };
            }
            regs[13] = STACK + STACK_SIZE / 2;
            regs[14] = SENTINEL;
            regs[15] = f;
            let nzcv = rng.next() & 0xF;
            let inp = Input { regs, nzcv, ext: rng.vfp_regs(), fpscr: 0, thumb: self.thumb.contains(&f) };
            match self.run_one(g, cfn, &inp) {
                One::Pass | One::PassAtCall => ran += 1,
                One::Reached => reached += 1,
                One::NoReturn | One::LeftCode => {}
                One::Differ(d) => return ("fail", format!("input {i}:{d}"), ran + 1, reached),
                One::CHalt(d) => return ("c-halt", format!("input {i}: {d}"), ran + 1, reached),
            }
        }
        ("pass", String::new(), ran, reached)
    }
}

/// Totals, printed at the end.
#[derive(Default)]
struct Tally {
    pass: u32,
    fail: u32,
    c_halted: u32,
    skipped: u32,
    blocked: u32,
    shown: u32,
    fails: Vec<String>,
    passed: Vec<String>,
}

impl Tally {
    fn add(&mut self, f: u32, verdict: &str, detail: &str, ran: u32, reached: u32) {
        match verdict {
            "pass" if ran == 0 && reached > 0 => self.blocked += 1,
            "pass" if ran == 0 => self.skipped += 1,
            "pass" => {
                self.pass += 1;
                self.passed.push(format!("{f:#x}"));
            }
            "c-halt" => self.c_halted += 1,
            _ => self.fail += 1,
        }
        if verdict != "pass" {
            self.fails.push(format!("{f:#x} {verdict} {detail}"));
            if self.shown < 15 {
                println!("{f:#x}: {verdict}: {detail}");
                self.shown += 1;
            }
        }
    }
    fn finish(&self, build: &std::path::Path, what: &str, extra: &str) {
        println!(
            "checked {what}: {} pass, {} differ, {} C halted, {} never returned on dynarmic, {} always reached untranslated code{extra}",
            self.pass, self.fail, self.c_halted, self.skipped, self.blocked
        );
        let _ = std::fs::write(build.join("recomp/check_failures.txt"), self.fails.join("\n"));
        let _ = std::fs::write(build.join("recomp/check_passed.json"), serde_json::to_string(&self.passed).unwrap());
    }
}

/// Replays a recording from touchHLE (src/revs_record.rs): rebuilds the
/// game's memory page by page, and at every sample of a translated function
/// runs it from exactly that state. Up to `per_fn` samples per function.
fn replay(ctx: &Ctx, g: &mut Guest, path: &str, funcs: &[u32], per_fn: u32, build: &std::path::Path, trace: bool) {
    use std::io::Read;
    let wanted: std::collections::HashSet<u32> = funcs.iter().copied().collect();
    let mut r = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path).expect("recording"));
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic).unwrap();
    assert_eq!(&magic, b"REVSREC1", "not a recording");
    let word = |r: &mut std::io::BufReader<std::fs::File>| {
        let mut b = [0u8; 4];
        r.read_exact(&mut b).unwrap();
        u32::from_le_bytes(b)
    };
    // Every write below is the game's own state, not a test write.
    g.full = true;
    let (mut n_reached, mut n_left, mut n_noret, mut n_at_call) = (0u32, 0u32, 0u32, 0u32);
    // Per function: (samples run, compared, reached, verdict, detail)
    let mut st: HashMap<u32, (u32, u32, u32, &'static str, String)> = HashMap::new();
    let (mut samples, mut pages) = (0u64, 0u64);
    let mut dirty = false;
    loop {
        let mut tag = [0u8; 1];
        if r.read_exact(&mut tag).is_err() {
            break; // recording cut short: use what is there
        }
        match tag[0] {
            b'P' => {
                let a = word(&mut r) as usize;
                let mut page = vec![0u8; PAGE].into_boxed_slice();
                r.read_exact(&mut page).unwrap();
                assert!(a != SENTINEL as usize, "the recording uses the sentinel page");
                g.mem()[a..a + PAGE].copy_from_slice(&page);
                g.pristine.insert(a, page);
                pages += 1;
                dirty = true;
            }
            b'S' => {
                let f = word(&mut r);
                let mut v = [0u32; 82];
                for x in v.iter_mut() {
                    *x = word(&mut r);
                }
                samples += 1;
                if dirty {
                    g.written_in(0, SIZE); // the page loads above
                    dirty = false;
                }
                if !wanted.contains(&f) {
                    continue;
                }
                let Some(cfn) = (unsafe { rc_lookup(f) }) else { continue };
                let e = st.entry(f).or_insert((0, 0, 0, "pass", String::new()));
                if e.0 >= per_fn || e.3 != "pass" {
                    continue;
                }
                e.0 += 1;
                let mut regs = [0u32; 16];
                regs.copy_from_slice(&v[..16]);
                regs[14] = SENTINEL;
                regs[15] = f;
                let mut ext = [0u32; 64];
                ext.copy_from_slice(&v[18..82]);
                let inp = Input { regs, nzcv: v[16] >> 28, ext, fpscr: v[17], thumb: v[16] & 0x20 != 0 };
                if trace {
                    eprintln!("{f:#x} sample {samples}");
                }
                match ctx.run_one(g, cfn, &inp) {
                    One::Pass => e.1 += 1,
                    One::PassAtCall => {
                        e.1 += 1;
                        n_at_call += 1;
                    }
                    One::Reached => {
                        e.2 += 1;
                        n_reached += 1;
                    }
                    One::LeftCode => {
                        e.2 += 1;
                        n_left += 1;
                    }
                    One::NoReturn => n_noret += 1,
                    One::Differ(d) => {
                        e.1 += 1;
                        e.3 = "fail";
                        e.4 = format!("sample {samples}:{d}");
                    }
                    One::CHalt(d) => {
                        e.1 += 1;
                        e.3 = "c-halt";
                        e.4 = format!("sample {samples}: {d}");
                    }
                }
            }
            b'E' => break,
            t => panic!("bad record tag {t:#x}"),
        }
    }
    let mut t = Tally::default();
    let mut fs: Vec<_> = st.into_iter().collect();
    fs.sort_by_key(|x| x.0);
    let n = fs.len();
    for (f, (_, ran, reached, verdict, detail)) in fs {
        t.add(f, verdict, &detail, ran, reached);
    }
    t.finish(
        build,
        &format!("{n} functions from a recording ({samples} samples, {pages} pages)"),
        &format!(". {n_at_call} samples matched up to an OS call. Not compared: {n_reached} reached an OS function or untranslated code, {n_left} left the game's code, {n_noret} ran out of time"),
    );
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let replay_file = args.iter().position(|a| a.starts_with("--replay=")).map(|i| args.remove(i)["--replay=".len()..].to_string());
    if args.len() < 2 {
        eprintln!("usage: revs-check <game.ipa> [build_dir] [inputs_per_function] [only: addr | list.json] [--replay=recording.bin]");
        std::process::exit(2);
    }
    let build = PathBuf::from(args.get(2).map(String::as_str).unwrap_or("build"));
    let inputs: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(12);
    let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok();
    // Restrict to one function, or to a JSON list of addresses.
    let only: Option<Vec<u32>> = args.get(4).map(|s| {
        if s.ends_with(".json") {
            let l: Vec<String> = serde_json::from_str(&std::fs::read_to_string(s).expect("list")).unwrap();
            l.iter().filter_map(|a| hex(a)).collect()
        } else {
            hex(s).into_iter().collect()
        }
    });
    let trace = std::env::var("REVS_TRACE").is_ok();

    let (_, fat) = ipa::read_executable(&PathBuf::from(&args[1])).unwrap_or_else(|e| panic!("{e}"));
    let bin = macho::Binary::parse_first_slice(&fat).unwrap();
    let list: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(build.join("recomp/translated.json")).expect("translated.json"))
            .unwrap();
    let mut funcs: Vec<u32> = list.iter().map(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap()).collect();
    if let Some(o) = only {
        funcs.retain(|a| o.contains(a));
    }

    // 4 GiB guest memory, like touchHLE (untouched pages cost nothing).
    let base =
        unsafe { VirtualAlloc(std::ptr::null_mut(), SIZE, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE) }
            as *mut u8;
    assert!(!base.is_null(), "could not allocate 4 GiB of guest memory");
    let mut g = Guest { base, pristine: HashMap::new(), hot: Vec::new(), full: false };
    let mem = g.mem();
    for &(vm, _vmsize, fo, fsz) in bin.segments() {
        if fsz > 0 {
            mem[vm as usize..(vm + fsz) as usize].copy_from_slice(&bin.data[fo as usize..(fo + fsz) as usize]);
        }
    }
    mem[SENTINEL as usize..SENTINEL as usize + 4].copy_from_slice(&(0xEF00_0000u32 | SVC_MAGIC).to_le_bytes());
    // Scratch memory: words that are either small numbers or pointers back
    // into scratch, so pointer-chasing code mostly stays in known memory.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for k in (0..SCRATCH_SIZE).step_by(4) {
        let w = if rng.next() & 1 == 0 { rng.scratch_ptr() } else { rng.next() & 0xFF };
        let o = (SCRATCH + k) as usize;
        mem[o..o + 4].copy_from_slice(&w.to_le_bytes());
    }
    // Remember every page that isn't zero.
    let mut nonzero: Vec<(usize, usize)> =
        bin.segments().iter().filter(|s| s.3 > 0).map(|s| (s.0 as usize, s.3 as usize)).collect();
    nonzero.push((SCRATCH as usize, SCRATCH_SIZE as usize));
    nonzero.push((SENTINEL as usize, 4));
    let mut pristine = HashMap::new();
    for &(a, n) in &nonzero {
        for p in (a & !(PAGE - 1)..a + n).step_by(PAGE) {
            pristine.insert(p, mem[p..p + PAGE].to_vec().into_boxed_slice());
        }
    }
    g.pristine = pristine;
    // Hot ranges: zero page up to the end of the game's segments, scratch,
    // stack, sentinel.
    let img_end = bin.segments().iter().map(|s| (s.0 + s.1) as usize).max().unwrap_or(0);
    let up = |x: usize| (x + PAGE - 1) & !(PAGE - 1);
    g.hot = vec![
        (0, up(img_end)),
        (SCRATCH as usize, SCRATCH as usize + SCRATCH_SIZE as usize),
        (STACK as usize, STACK as usize + STACK_SIZE as usize),
        (SENTINEL as usize, SENTINEL as usize + 0x1_0000), // code may address off lr
    ];
    g.written_in(0, SIZE); // start watching from here

    CODE_START.store(bin.text.addr, Relaxed);
    CODE_END.store(bin.text.addr + bin.text.size, Relaxed);
    if let Some(st) = bin.section("__TEXT", "__symbol_stub4") {
        STUB_START.store(st.addr, Relaxed);
        STUB_END.store(st.addr + st.size, Relaxed);
    }
    let ctx = Ctx {
        cpu: unsafe { touchHLE_DynarmicWrapper_new(base as *mut c_void, 0) },
        text: bin.text.addr..bin.text.addr + bin.text.size,
        inputs,
        thumb: {
            let fs: Vec<serde_json::Value> =
                serde_json::from_str(&std::fs::read_to_string(build.join("functions.json")).expect("functions.json")).unwrap();
            fs.iter()
                .filter(|f| f["thumb"].as_bool() == Some(true))
                .filter_map(|f| match &f["addr"] {
                    serde_json::Value::String(s) => hex(s),
                    v => v.as_u64().map(|n| n as u32),
                })
                .collect()
        },
    };

    if let Some(path) = replay_file {
        replay(&ctx, &mut g, &path, &funcs, inputs, &build, trace);
        return;
    }

    let mut t = Tally::default();
    let mut wild = 0u32;
    for &f in &funcs {
        let Some(cfn) = (unsafe { rc_lookup(f) }) else { continue };
        if trace {
            eprintln!("{f:#x}");
        }
        let saved = rng.clone();
        let mut res = ctx.check(&g, f, cfn, &mut rng);
        // Anything written outside the hot ranges: put it back and recheck
        // this function (same inputs) asking about all 4 GiB after each run.
        let outside = g.written_in(0, SIZE);
        if !outside.is_empty() {
            wild += 1;
            if trace {
                eprintln!("  outside: {:x?}", &outside[..outside.len().min(8)]);
            }
            ctx.restore(&g, &outside);
            g.full = true;
            rng = saved;
            res = ctx.check(&g, f, cfn, &mut rng);
            g.full = false;
        }
        let (verdict, detail, ran, reached) = res;
        t.add(f, verdict, &detail, ran, reached);
    }
    t.finish(
        &build,
        &format!("{} functions x {inputs} random inputs", funcs.len()),
        &format!(" ({wild} wrote outside the usual areas, rechecked)"),
    );
}

/// Steps a run may take on each side (dynarmic ticks, C loop budget):
/// REVS_TICKS, default 50,000. Long loops (skinning a whole mesh) need more.
fn ticks_limit() -> u64 {
    static T: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var("REVS_TICKS").ok().and_then(|v| v.parse().ok()).unwrap_or(50_000).min(i32::MAX as u64))
}
