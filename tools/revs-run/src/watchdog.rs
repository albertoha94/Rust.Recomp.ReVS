//! --watchdog helpers: where the main thread is in the host code.
//!
//! Windows reads the thread's registers (watchdog_win.rs). Other systems have
//! no sampler yet: the watchdog still stops a stuck run and names the last OS
//! call, but host code offsets print as 0.

#[cfg(windows)]
#[path = "watchdog_win.rs"]
mod imp;

#[cfg(not(windows))]
mod imp {
    /// The main thread, for the samplers (unused here).
    pub fn main_thread() -> isize {
        0
    }

    /// No sampler on this system: 0 (unknown).
    pub fn sample_rva(_t: isize) -> u64 {
        0
    }

    /// No sampler on this system: 0 (unknown).
    pub fn sample_rip(_t: isize) -> u64 {
        0
    }
}

pub use imp::*;
