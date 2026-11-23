//! The recompiled game's code (generated from the user's .ipa).
//!
//! Developer builds link it in (build.rs compiles build/recomp). Release builds
//! (feature `dll`) contain none of it: the launcher's setup step compiles it on
//! the player's PC into `rc_game.dll` (`librc_game.so` on Linux) in the build
//! folder, and `load` opens it at start. The interface is the same either way:
//! the C calls rc_import / rc_miss / rc_trace (main.rs), the runner calls
//! rc_lookup / rc_trap and reads rc_cov (rc_runtime.c).

#[cfg(not(feature = "dll"))]
mod imp {
    use crate::{Cpu, RcFn};
    extern "C" {
        fn rc_lookup(addr: u32) -> Option<RcFn>;
        fn rc_trap(c: *mut Cpu, addr: u32, why: u32);
        static mut rc_cov: [u8; 0x200000];
    }
    pub fn load(_build: &std::path::Path) -> Result<(), String> {
        Ok(())
    }
    #[inline]
    pub unsafe fn lookup(addr: u32) -> Option<RcFn> {
        rc_lookup(addr)
    }
    #[inline]
    pub unsafe fn trap(c: *mut Cpu, addr: u32, why: u32) {
        rc_trap(c, addr, why)
    }
    pub fn cov() -> *mut [u8; 0x200000] {
        std::ptr::addr_of_mut!(rc_cov)
    }
}

#[cfg(feature = "dll")]
mod imp {
    use crate::{Cpu, RcFn};
    use std::path::Path;

    /// Must match `RcHost` in tools/revs-check/runtime/rc_runtime.c.
    #[repr(C)]
    struct Host {
        size: u32,
        import: unsafe extern "C" fn(*mut Cpu, u32, u32),
        miss: unsafe extern "C" fn(*mut Cpu, u32),
        trace: unsafe extern "C" fn(*mut Cpu, u32) -> i32,
    }
    const ABI: u32 = 1;

    struct Api {
        lookup: unsafe extern "C" fn(u32) -> Option<RcFn>,
        trap: unsafe extern "C" fn(*mut Cpu, u32, u32),
        cov: *mut [u8; 0x200000],
        _lib: libloading::Library,
    }
    // The pointers stay valid as long as the library, which is never unloaded.
    unsafe impl Send for Api {}
    unsafe impl Sync for Api {}
    static API: std::sync::OnceLock<Api> = std::sync::OnceLock::new();

    pub const FILE: &str = if cfg!(windows) {
        "rc_game.dll"
    } else if cfg!(target_os = "macos") {
        "librc_game.dylib"
    } else {
        "librc_game.so"
    };

    pub fn load(build: &Path) -> Result<(), String> {
        let path = build.join(FILE);
        let not_set_up = |e: &dyn std::fmt::Display| format!("{}: {e} (the game isn't set up yet: run revs-prepare)", path.display());
        let lib = unsafe { libloading::Library::new(&path) }.map_err(|e| not_set_up(&e))?;
        unsafe {
            let sym = |name: &[u8]| lib.get::<*const ()>(name).map(|s| *s).map_err(|e| not_set_up(&e));
            let cpu_size: unsafe extern "C" fn() -> u32 = std::mem::transmute(sym(b"rc_cpu_size")?);
            if cpu_size() as usize != std::mem::size_of::<Cpu>() {
                return Err(format!("{}: made for another version of revs-run (set up the game again)", path.display()));
            }
            let init: unsafe extern "C" fn(*const Host) -> u32 = std::mem::transmute(sym(b"rc_dll_init")?);
            let host = Host { size: std::mem::size_of::<Host>() as u32, import: crate::rc_import, miss: crate::rc_miss, trace: crate::rc_trace };
            if init(&host) != ABI {
                return Err(format!("{}: made for another version of revs-run (set up the game again)", path.display()));
            }
            let cov_ptr: unsafe extern "C" fn() -> *mut [u8; 0x200000] = std::mem::transmute(sym(b"rc_cov_ptr")?);
            let api = Api {
                lookup: std::mem::transmute(sym(b"rc_lookup")?),
                trap: std::mem::transmute(sym(b"rc_trap")?),
                cov: cov_ptr(),
                _lib: lib,
            };
            let _ = API.set(api);
        }
        println!("game code: {}", path.display());
        Ok(())
    }

    fn api() -> &'static Api {
        API.get().expect("game code not loaded")
    }
    #[inline]
    pub unsafe fn lookup(addr: u32) -> Option<RcFn> {
        (api().lookup)(addr)
    }
    #[inline]
    pub unsafe fn trap(c: *mut Cpu, addr: u32, why: u32) {
        (api().trap)(c, addr, why)
    }
    pub fn cov() -> *mut [u8; 0x200000] {
        api().cov
    }
}

pub use imp::*;
