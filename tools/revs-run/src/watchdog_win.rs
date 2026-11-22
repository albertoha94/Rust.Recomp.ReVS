//! --watchdog helpers on Windows x64: pause the main thread and read where it is.

#[repr(C, align(16))]
struct Context([u8; 1232]);

extern "system" {
    fn GetCurrentProcess() -> isize;
    fn GetCurrentThread() -> isize;
    fn DuplicateHandle(sp: isize, sh: isize, tp: isize, th: *mut isize, access: u32, inherit: i32, opts: u32) -> i32;
    fn SuspendThread(t: isize) -> u32;
    fn ResumeThread(t: isize) -> u32;
    fn GetThreadContext(t: isize, ctx: *mut Context) -> i32;
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn GetModuleHandleExW(flags: u32, addr: usize, out: *mut isize) -> i32;
    fn GetModuleFileNameW(m: isize, buf: *mut u16, n: u32) -> u32;
    fn VirtualQuery(addr: usize, info: *mut [u64; 6], len: usize) -> usize;
}

/// The DLL (or exe) holding a host address, and the offset into it.
pub fn module_of(addr: u64) -> String {
    unsafe {
        let mut m = 0;
        // FROM_ADDRESS | UNCHANGED_REFCOUNT
        if GetModuleHandleExW(4 | 2, addr as usize, &mut m) == 0 {
            return format!("{addr:#x} (no module)");
        }
        let mut buf = [0u16; 260];
        let n = GetModuleFileNameW(m, buf.as_mut_ptr(), 260) as usize;
        let name = String::from_utf16_lossy(&buf[..n]);
        format!("{}+{:#x}", name.rsplit('\\').next().unwrap_or(&name), addr - m as u64)
    }
}

/// A handle to the calling thread that other threads can use.
pub fn main_thread() -> isize {
    let mut h = 0;
    // DUPLICATE_SAME_ACCESS
    unsafe { DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(), &mut h, 0, 0, 2) };
    h
}

/// The thread's instruction pointer as an offset into the exe (0 if unknown).
pub fn sample_rva(t: isize) -> u64 {
    unsafe {
        let mut ctx = Context([0; 1232]);
        // ContextFlags at 0x30: CONTEXT_CONTROL (AMD64). Rip at 0xF8.
        ctx.0[0x30..0x34].copy_from_slice(&0x0010_0001u32.to_le_bytes());
        SuspendThread(t);
        let ok = GetThreadContext(t, &mut ctx);
        ResumeThread(t);
        std::thread::sleep(std::time::Duration::from_millis(20));
        if ok == 0 {
            return 0;
        }
        let rip = u64::from_le_bytes(ctx.0[0xF8..0x100].try_into().unwrap());
        // Rsp at 0x98: also name the first few return addresses on the stack
        // that are in the exe (where the blocked call came from).
        let rsp = u64::from_le_bytes(ctx.0[0x98..0xA0].try_into().unwrap());
        let exe = GetModuleHandleW(std::ptr::null()) as u64;
        let mut frames = Vec::new();
        // Only the committed region holding rsp (BaseAddress + RegionSize).
        let mut info = [0u64; 6];
        VirtualQuery(rsp as usize, &mut info, 48);
        let end = info[0] + info[3];
        for k in 0..((end.saturating_sub(rsp) / 8).min(65536)) {
            let v = *((rsp + 8 * k) as *const u64);
            if v > exe && v < exe + 0x0400_0000 && frames.len() < 12 {
                frames.push(format!("{:#x}", v - exe));
            }
        }
        println!("watchdog: thread at {}; exe return addresses on its stack: {}", module_of(rip), frames.join(" "));
        rip.wrapping_sub(GetModuleHandleW(std::ptr::null()) as u64)
    }
}

/// Only the thread's instruction pointer as an exe offset (0 if unknown), for
/// profiles: no printing, no stack walk.
pub fn sample_rip(t: isize) -> u64 {
    unsafe {
        let mut ctx = Context([0; 1232]);
        ctx.0[0x30..0x34].copy_from_slice(&0x0010_0001u32.to_le_bytes());
        SuspendThread(t);
        let ok = GetThreadContext(t, &mut ctx);
        ResumeThread(t);
        if ok == 0 {
            return 0;
        }
        let rip = u64::from_le_bytes(ctx.0[0xF8..0x100].try_into().unwrap());
        rip.wrapping_sub(GetModuleHandleW(std::ptr::null()) as u64)
    }
}
