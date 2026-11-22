//! Native versions of OS functions, working on guest memory.
//!
//! Arguments follow the iOS ARM ABI (softfp): r0-r3, then the stack; results
//! in r0 (and r1).

use crate::{Cpu, Host};

/// Size-class allocator in the guest heap. Each block has an 8-byte header
/// (class, requested size) in front of what the game gets.
pub struct Heap {
    next: u32,
    end: u32,
    free: Vec<Vec<u32>>,
    used: u64,
}

impl Heap {
    pub fn new(start: u32, end: u32) -> Heap {
        Heap { next: start, end, free: vec![Vec::new(); 32], used: 0 }
    }
    pub fn used(&self) -> u64 {
        self.used
    }
    /// End of the part of the heap handed out so far.
    pub fn top(&self) -> u32 {
        self.next
    }
    fn class(size: u32) -> u32 {
        (size.max(1) as u64 + 8).next_power_of_two().max(16).trailing_zeros()
    }
    pub fn alloc(&mut self, h: &Host, size: u32) -> u32 {
        let k = Self::class(size);
        let p = match self.free[k as usize].pop() {
            Some(p) => p,
            None => {
                let p = self.next;
                let n = 1u32 << k;
                assert!(self.end - self.next >= n, "guest heap full");
                self.next += n;
                p
            }
        };
        h.wr32(p, k);
        h.wr32(p + 4, size);
        self.used += 1 << k;
        p + 8
    }
    pub fn free(&mut self, h: &Host, a: u32) {
        if a == 0 {
            return;
        }
        let k = h.rd32(a - 8);
        self.used -= 1 << k;
        self.free[k as usize].push(a - 8);
    }
    pub fn size(&self, h: &Host, a: u32) -> u32 {
        h.rd32(a - 4)
    }
}

/// Zeroed guest heap block of `size` bytes (calloc).
pub fn alloc_zeroed(h: &mut Host, size: u32) -> u32 {
    let mut heap = std::mem::replace(&mut h.heap, Heap::new(0, 0));
    let p = heap.alloc(h, size);
    h.heap = heap;
    unsafe { std::ptr::write_bytes(h.at(p), 0, size as usize) };
    p
}

static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

pub fn arg(h: &Host, c: &Cpu, i: u32) -> u32 {
    if i < 4 {
        c.r[i as usize]
    } else {
        h.rd32(c.r[13] + 4 * (i - 4))
    }
}

fn strlen(h: &Host, a: u32) -> u32 {
    let mut n = 0;
    while h.rd8(a + n) != 0 {
        n += 1;
    }
    n
}

fn cmp(h: &Host, a: u32, b: u32, max: u32, stop_at_nul: bool) -> u32 {
    for k in 0..max {
        let (x, y) = (h.rd8(a + k), h.rd8(b + k));
        if x != y {
            return (x as i32 - y as i32) as u32;
        }
        if stop_at_nul && x == 0 {
            return 0;
        }
    }
    0
}

/// Every name `call` handles (used when linking: a bound pointer to one of
/// these is a function, not data).
pub const NATIVE: &[&str] = &[
    "memcpy", "memmove", "memset", "bzero", "memcmp", "strlen", "strcmp", "strncmp", "strcpy", "strncpy", "strcat",
    "strchr", "strrchr", "strstr", "malloc", "_Znwm", "_Znam", "calloc", "free", "_ZdlPv", "_ZdaPv", "realloc",
    "__cxa_atexit", "__cxa_guard_acquire", "__cxa_guard_release", "__cxa_guard_abort", "_Unwind_SjLj_Register",
    "_Unwind_SjLj_Unregister", "pthread_once", "pthread_key_create", "pthread_getspecific", "pthread_setspecific",
    "pthread_mutex_init", "pthread_mutex_destroy", "pthread_mutex_lock", "pthread_mutex_unlock", "pthread_mutex_trylock",
    "pthread_cond_init", "pthread_cond_destroy", "pthread_cond_signal", "pthread_cond_broadcast", "pthread_mutexattr_init", "pthread_mutexattr_settype", "pthread_mutexattr_destroy",
    "btowc", "wctob", "pthread_self", "atoi", "mach_timebase_info", "mach_absolute_time", "time", "getpid", "getenv", "gettimeofday", "localtime_r", "gmtime_r",
    "qsort", "memchr", "__tolower", "__toupper", "getuid", "geteuid", "CFRelease", "CFRetain",
];

/// libgcc helpers run natively even though libgcc is loaded: they are
/// hand-written ARM (computed jumps, shared tails) the recompiler doesn't
/// translate, and are plain arithmetic. softfp: a double is r0:r1 (then
/// r2:r3), a float is r0 (then r1); results in r0 (and r1).
pub const OVERRIDE: &[&str] = &[
    "__udivsi3", "__divsi3", "__umodsi3", "__modsi3", "__aeabi_uidiv", "__aeabi_idiv",
    "__adddf3", "__subdf3", "__muldf3", "__divdf3", "__negdf2", "__floatsidf", "__floatunsidf", "__floatdidf",
    "__floatundidf", "__extendsfdf2", "__truncdfsf2", "__fixdfsi", "__fixunsdfsi", "__eqdf2", "__nedf2", "__gedf2",
    "__gtdf2", "__ledf2", "__ltdf2", "__unorddf2",
    "__addsf3", "__subsf3", "__mulsf3", "__divsf3", "__negsf2", "__floatsisf", "__floatunsisf", "__floatdisf",
    "__floatundisf", "__fixsfsi", "__fixunssfsi", "__eqsf2", "__nesf2", "__gesf2", "__gtsf2", "__lesf2", "__ltsf2",
    "__unordsf2",
    "__addvsi3", "__subvsi3", "__mulvsi3", "__negvsi2", "__addvdi3", "__subvdi3", "__mulvdi3", "__negvdi2",
    // 64-bit division: libgcc's own versions call its translated __udivsi3,
    // which gives wrong results (the match clock read ~49 days).
    "__udivdi3", "__divdi3", "__umoddi3", "__moddi3",
];

fn math(c: &mut Cpu, name: &str) -> bool {
    let r = c.r;
    let d = |i: usize| f64::from_bits(r[i] as u64 | (r[i + 1] as u64) << 32);
    let f = |i: usize| f32::from_bits(r[i]);
    let l = |i: usize| (r[i] as u64 | (r[i + 1] as u64) << 32) as i64;
    // Comparisons: <0, 0, >0 like the libgcc ones; `nan` is the unordered result.
    let cmp = |o: Option<std::cmp::Ordering>, nan: i32| o.map_or(nan, |o| o as i32) as u32 as u64;
    let v: u64 = match name {
        "__udivsi3" | "__aeabi_uidiv" => r[0].checked_div(r[1]).unwrap_or(0) as u64,
        "__divsi3" | "__aeabi_idiv" => (r[0] as i32).checked_div(r[1] as i32).unwrap_or(0) as u32 as u64,
        "__umodsi3" => r[0].checked_rem(r[1]).unwrap_or(r[0]) as u64,
        "__modsi3" => (r[0] as i32).checked_rem(r[1] as i32).unwrap_or(r[0] as i32) as u32 as u64,
        // Division by zero is undefined in C; 0 keeps the game running.
        "__udivdi3" => (l(0) as u64).checked_div(l(2) as u64).unwrap_or(0),
        "__divdi3" => l(0).checked_div(l(2)).unwrap_or(0) as u64,
        "__umoddi3" => (l(0) as u64).checked_rem(l(2) as u64).unwrap_or(l(0) as u64),
        "__moddi3" => l(0).checked_rem(l(2)).unwrap_or(l(0)) as u64,
        "__adddf3" => (d(0) + d(2)).to_bits(),
        "__subdf3" => (d(0) - d(2)).to_bits(),
        "__muldf3" => (d(0) * d(2)).to_bits(),
        "__divdf3" => (d(0) / d(2)).to_bits(),
        "__negdf2" => (-d(0)).to_bits(),
        "__floatsidf" => (r[0] as i32 as f64).to_bits(),
        "__floatunsidf" => (r[0] as f64).to_bits(),
        "__floatdidf" => (l(0) as f64).to_bits(),
        "__floatundidf" => (l(0) as u64 as f64).to_bits(),
        "__extendsfdf2" => (f(0) as f64).to_bits(),
        "__truncdfsf2" => (d(0) as f32).to_bits() as u64,
        "__fixdfsi" => d(0) as i32 as u32 as u64,
        "__fixunsdfsi" => d(0) as u32 as u64,
        "__eqdf2" | "__nedf2" => cmp(d(0).partial_cmp(&d(2)), 1),
        "__gedf2" | "__gtdf2" => cmp(d(0).partial_cmp(&d(2)), -1),
        "__ledf2" | "__ltdf2" => cmp(d(0).partial_cmp(&d(2)), 1),
        "__unorddf2" => (d(0).is_nan() || d(2).is_nan()) as u64,
        "__addsf3" => (f(0) + f(1)).to_bits() as u64,
        "__subsf3" => (f(0) - f(1)).to_bits() as u64,
        "__mulsf3" => (f(0) * f(1)).to_bits() as u64,
        "__divsf3" => (f(0) / f(1)).to_bits() as u64,
        "__negsf2" => (-f(0)).to_bits() as u64,
        "__floatsisf" => (r[0] as i32 as f32).to_bits() as u64,
        "__floatunsisf" => (r[0] as f32).to_bits() as u64,
        "__floatdisf" => (l(0) as f32).to_bits() as u64,
        "__floatundisf" => (l(0) as u64 as f32).to_bits() as u64,
        "__fixsfsi" => f(0) as i32 as u32 as u64,
        "__fixunssfsi" => f(0) as u32 as u64,
        "__eqsf2" | "__nesf2" => cmp(f(0).partial_cmp(&f(1)), 1),
        "__gesf2" | "__gtsf2" => cmp(f(0).partial_cmp(&f(1)), -1),
        "__lesf2" | "__ltsf2" => cmp(f(0).partial_cmp(&f(1)), 1),
        "__unordsf2" => (f(0).is_nan() || f(1).is_nan()) as u64,
        // The trapping-overflow variants (-ftrapv); the game never overflows here.
        "__addvsi3" => (r[0] as i32).wrapping_add(r[1] as i32) as u32 as u64,
        "__subvsi3" => (r[0] as i32).wrapping_sub(r[1] as i32) as u32 as u64,
        "__mulvsi3" => (r[0] as i32).wrapping_mul(r[1] as i32) as u32 as u64,
        "__negvsi2" => (r[0] as i32).wrapping_neg() as u32 as u64,
        "__addvdi3" => l(0).wrapping_add(l(2)) as u64,
        "__subvdi3" => l(0).wrapping_sub(l(2)) as u64,
        "__mulvdi3" => l(0).wrapping_mul(l(2)) as u64,
        "__negvdi2" => l(0).wrapping_neg() as u64,
        _ => return false,
    };
    c.r[0] = v as u32;
    c.r[1] = (v >> 32) as u32;
    true
}

/// Runs OS function `name` if it has a native version. Returns false if not.
pub fn call(h: &mut Host, c: &mut Cpu, name: &str) -> bool {
    if math(c, name) {
        return true;
    }
    let a = |i| arg(h, c, i);
    let (a0, a1, a2) = (a(0), a(1), a(2));
    let r0 = match name {
        // memory and strings
        "memcpy" | "memmove" => {
            unsafe { std::ptr::copy(h.at(a1), h.at(a0), a2 as usize) };
            a0
        }
        "memset" => {
            unsafe { std::ptr::write_bytes(h.at(a0), a1 as u8, a2 as usize) };
            a0
        }
        "bzero" => {
            unsafe { std::ptr::write_bytes(h.at(a0), 0, a1 as usize) };
            0
        }
        "memcmp" => cmp(h, a0, a1, a2, false),
        "strlen" => strlen(h, a0),
        "strcmp" => cmp(h, a0, a1, u32::MAX, true),
        "strncmp" => cmp(h, a0, a1, a2, true),
        "strcpy" => {
            let n = strlen(h, a1) + 1;
            unsafe { std::ptr::copy(h.at(a1), h.at(a0), n as usize) };
            a0
        }
        "strncpy" => {
            let n = strlen(h, a1).min(a2);
            unsafe {
                std::ptr::copy(h.at(a1), h.at(a0), n as usize);
                std::ptr::write_bytes(h.at(a0 + n), 0, (a2 - n) as usize);
            }
            a0
        }
        "strcat" => {
            let (d, n) = (strlen(h, a0), strlen(h, a1) + 1);
            unsafe { std::ptr::copy(h.at(a1), h.at(a0 + d), n as usize) };
            a0
        }
        "strchr" | "strrchr" => {
            let (len, ch) = (strlen(h, a0), a1 as u8);
            let hits = (0..=len).filter(|&k| h.rd8(a0 + k) == ch);
            let k = if name == "strchr" { hits.min() } else { hits.max() };
            k.map_or(0, |k| a0 + k)
        }
        "strstr" => {
            let (n, m) = (strlen(h, a0), strlen(h, a1));
            (0..=n.saturating_sub(m)).find(|&k| m <= n && cmp(h, a0 + k, a1, m, false) == 0).map_or(0, |k| a0 + k)
        }
        // heap
        "malloc" | "_Znwm" | "_Znam" => {
            let mut heap = std::mem::replace(&mut h.heap, Heap::new(0, 0));
            let p = heap.alloc(h, a0);
            h.heap = heap;
            p
        }
        "calloc" => {
            let n = a0.saturating_mul(a1);
            let mut heap = std::mem::replace(&mut h.heap, Heap::new(0, 0));
            let p = heap.alloc(h, n);
            h.heap = heap;
            unsafe { std::ptr::write_bytes(h.at(p), 0, n as usize) };
            p
        }
        "free" | "_ZdlPv" | "_ZdaPv" => {
            let mut heap = std::mem::replace(&mut h.heap, Heap::new(0, 0));
            heap.free(h, a0);
            h.heap = heap;
            0
        }
        "realloc" => {
            let mut heap = std::mem::replace(&mut h.heap, Heap::new(0, 0));
            let p = if a0 == 0 {
                heap.alloc(h, a1)
            } else {
                let old = heap.size(h, a0);
                let p = heap.alloc(h, a1);
                unsafe { std::ptr::copy(h.at(a0), h.at(p), old.min(a1) as usize) };
                heap.free(h, a0);
                p
            };
            h.heap = heap;
            p
        }
        // C++ runtime pieces that are plain bookkeeping
        "__cxa_atexit" => {
            h.atexit.push((a0, a1));
            0
        }
        "__cxa_guard_acquire" => (h.rd8(a0) == 0) as u32,
        "__cxa_guard_release" => {
            h.wr8(a0, 1);
            0
        }
        "__cxa_guard_abort" => 0,
        // SjLj exceptions: each function context starts with `prev`.
        "_Unwind_SjLj_Register" => {
            h.wr32(a0, h.sjlj_head);
            h.sjlj_head = a0;
            0
        }
        "_Unwind_SjLj_Unregister" => {
            h.sjlj_head = h.rd32(a0);
            0
        }
        // Threads: the runner is single-threaded for now, so these only keep
        // state. pthread_once_t is {sig, opaque[4]}: the opaque word is "done".
        "pthread_once" => {
            if h.rd32(a0 + 4) == 0 {
                h.wr32(a0 + 4, 1);
                crate::guest_call(c, a1, &[]);
            }
            0
        }
        "pthread_key_create" => {
            h.tls.push(0);
            h.wr32(a0, h.tls.len() as u32 - 1);
            0
        }
        "pthread_getspecific" => h.tls.get(a0 as usize).copied().unwrap_or(0),
        "pthread_setspecific" => {
            if let Some(v) = h.tls.get_mut(a0 as usize) {
                *v = a1;
            }
            0
        }
        "pthread_mutex_init" | "pthread_mutex_destroy" | "pthread_mutex_lock" | "pthread_mutex_unlock"
        | "pthread_mutex_trylock" | "pthread_cond_init" | "pthread_cond_destroy" | "pthread_cond_signal"
        | "pthread_cond_broadcast" | "pthread_mutexattr_init" | "pthread_mutexattr_settype" | "pthread_mutexattr_destroy" => 0,
        // No environment variables (files are in fs.rs).
        "getenv" => 0,
        // Clock: mach ticks are nanoseconds (timebase 1/1) since the runner started.
        "mach_timebase_info" => {
            h.wr32(a0, 1);
            h.wr32(a0 + 4, 1);
            0
        }
        "mach_absolute_time" => {
            let ns = START.get_or_init(std::time::Instant::now).elapsed().as_nanos() as u64;
            c.r[1] = (ns >> 32) as u32;
            ns as u32
        }
        "time" => {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as u32;
            if a0 != 0 {
                h.wr32(a0, t);
            }
            t
        }
        // struct timeval { time_t tv_sec; suseconds_t tv_usec; }: two 32-bit words.
        "gettimeofday" => {
            let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            if a0 != 0 {
                h.wr32(a0, d.as_secs() as u32);
                h.wr32(a0 + 4, d.subsec_micros());
            }
            0
        }
        // Darwin struct tm: 9 ints, tm_gmtoff (long), tm_zone (char *) = 44 bytes.
        // Filled as UTC (the host's time zone is not needed by the game).
        "localtime_r" | "gmtime_r" => {
            let t = h.rd32(a0) as i32 as i64;
            let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
            // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
            let z = days + 719468;
            let era = z.div_euclid(146097);
            let doe = z - era * 146097;
            let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let mday = doy - (153 * mp + 2) / 5 + 1;
            let mon = if mp < 10 { mp + 3 } else { mp - 9 };
            let year = yoe + era * 400 + (mon <= 2) as i64;
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            let yday = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334][(mon - 1) as usize] + mday - 1 + (leap && mon > 2) as i64;
            let f = [secs % 60, secs / 60 % 60, secs / 3600, mday, mon - 1, year - 1900, (days + 4).rem_euclid(7), yday, 0, 0, 0];
            for (k, v) in f.iter().enumerate() {
                h.wr32(a1 + 4 * k as u32, *v as u32);
            }
            a1
        }
        "getpid" => 1,
        // Sorts indices with the game's comparator (which reads the untouched
        // array), then writes the elements back in that order.
        "qsort" => {
            let (base, n, size, cmp) = (a0, a1, a2, arg(h, c, 3));
            let mut idx: Vec<u32> = (0..n).collect();
            idx.sort_by(|&x, &y| {
                let r = crate::guest_call(c, cmp, &[base + x * size, base + y * size]) as i32;
                r.cmp(&0)
            });
            let old: Vec<u8> = unsafe { std::slice::from_raw_parts(h.at(base), (n * size) as usize) }.to_vec();
            for (k, &i) in idx.iter().enumerate() {
                let src = &old[(i * size) as usize..((i + 1) * size) as usize];
                unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), h.at(base + k as u32 * size), size as usize) };
            }
            0
        }
        "memchr" => (0..a2).find(|&k| h.rd8(a0 + k) == a1 as u8).map_or(0, |k| a0 + k),
        "__tolower" => if (b'A' as u32..=b'Z' as u32).contains(&a0) { a0 + 32 } else { a0 },
        "__toupper" => if (b'a' as u32..=b'z' as u32).contains(&a0) { a0 - 32 } else { a0 },
        "getuid" | "geteuid" => 501,
        "CFRelease" => 0,
        "CFRetain" => a0,
        "atoi" => {
            let s = h.cstr(a0);
            let t = s.trim_start();
            let (neg, t) = match t.as_bytes().first() {
                Some(b'-') => (true, &t[1..]),
                Some(b'+') => (false, &t[1..]),
                _ => (false, t),
            };
            let v = t.bytes().take_while(u8::is_ascii_digit).fold(0i32, |v, d| v.wrapping_mul(10).wrapping_add((d - b'0') as i32));
            (if neg { v.wrapping_neg() } else { v }) as u32
        }
        // One thread: any non-zero id will do (SQLite compares them).
        "pthread_self" => 1,
        // Darwin's "C" locale: every byte is the character of the same value.
        "btowc" => {
            if a0 == u32::MAX {
                u32::MAX
            } else {
                a0 & 0xFF
            }
        }
        "wctob" => {
            if a0 < 0x100 {
                a0
            } else {
                u32::MAX
            }
        }
        _ => return false,
    };
    c.r[0] = r0;
    true
}
