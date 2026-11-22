//! Networking, offline: the port reports no network, as touchHLE does with
//! network access off. The game's online code (Capcom "Cloudia") gets real
//! objects and well-defined answers, so it never touches the network and
//! never works on a NULL it didn't expect.
//!
//! - SCNetworkReachability: refs are real (a small zeroed block, freed by the
//!   no-op CFRelease); GetFlags writes 0 and returns false (unknown, not
//!   reachable); callbacks and run-loop scheduling are refused.
//! - CFMutableArray: an NSMutableArray (toll-free bridged on iOS), so the
//!   game's CFArray calls and NSArray messages see the same items.
//! - inet_pton / inet_aton: parse dotted IPv4, as libc does.
//! - select: nothing is ever ready; it returns 0 (timeout) with the sets
//!   cleared. OpenSSL's RAND_poll uses it to wait on /dev/urandom.
//! - socket / connect: fail (-1, errno EACCES / ECONNREFUSED). OpenSSL's
//!   RAND_query_egd_bytes tries an entropy-daemon socket and then skips it.

use crate::objc::Obj;
use crate::{Cpu, Host};

const AF_INET: u32 = 2;
// Darwin errno values.
const EACCES: u32 = 13;
const ECONNREFUSED: u32 = 61;

/// Every name `call` handles (linking: these are functions, not data).
pub const NATIVE: &[&str] = &[
    "SCNetworkReachabilityCreateWithAddress",
    "SCNetworkReachabilityCreateWithName",
    "SCNetworkReachabilityGetFlags",
    "SCNetworkReachabilitySetCallback",
    "SCNetworkReachabilityScheduleWithRunLoop",
    "SCNetworkReachabilityUnscheduleFromRunLoop",
    "CFArrayCreateMutable",
    "CFArrayGetCount",
    "CFArrayGetValueAtIndex",
    "CFArrayAppendValue",
    "CFArrayRemoveAllValues",
    "inet_pton",
    "inet_aton",
    "select",
    "socket",
    "connect",
];

/// Dotted IPv4 ("a.b.c.d", each 0-255) as its 4 bytes in network order.
fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    s.parse::<std::net::Ipv4Addr>().ok().map(|a| a.octets())
}

fn write_bytes(h: &Host, addr: u32, bytes: &[u8]) {
    for (k, &b) in bytes.iter().enumerate() {
        h.wr8(addr + k as u32, b);
    }
}

fn array_items(h: &mut Host, a: u32) -> Option<&mut Vec<u32>> {
    match h.objc.objs.get_mut(&a) {
        Some(Obj::Array(v)) => Some(v),
        _ => None,
    }
}

/// Runs `name` if it is one of these. Returns false otherwise.
pub fn call(h: &mut Host, c: &mut Cpu, name: &str) -> bool {
    if !NATIVE.contains(&name) {
        return false;
    }
    let a = |i| crate::os::arg(h, c, i);
    let (a0, a1, a2, a3, a4) = (a(0), a(1), a(2), a(3), a(4));
    let r0 = match name {
        "SCNetworkReachabilityCreateWithAddress" | "SCNetworkReachabilityCreateWithName" => {
            crate::os::alloc_zeroed(h, 16)
        }
        "SCNetworkReachabilityGetFlags" => {
            if a1 != 0 {
                h.wr32(a1, 0);
            }
            0
        }
        "SCNetworkReachabilitySetCallback"
        | "SCNetworkReachabilityScheduleWithRunLoop"
        | "SCNetworkReachabilityUnscheduleFromRunLoop" => 0,
        "CFArrayCreateMutable" => crate::uikit::new_obj(h, "NSMutableArray", Obj::Array(Vec::new())),
        "CFArrayGetCount" => array_items(h, a0).map_or(0, |v| v.len() as u32),
        "CFArrayGetValueAtIndex" => array_items(h, a0).and_then(|v| v.get(a1 as usize).copied()).unwrap_or(0),
        "CFArrayAppendValue" => {
            if let Some(v) = array_items(h, a0) {
                v.push(a1);
            }
            0
        }
        "CFArrayRemoveAllValues" => {
            if let Some(v) = array_items(h, a0) {
                v.clear();
            }
            0
        }
        // inet_pton(af, src, dst): 1 parsed, 0 not an address, -1 family not supported.
        "inet_pton" => {
            if a0 != AF_INET {
                u32::MAX
            } else if let Some(b) = parse_ipv4(&h.cstr(a1)) {
                write_bytes(h, a2, &b);
                1
            } else {
                0
            }
        }
        // inet_aton(src, in_addr*): 1 parsed, 0 not.
        "inet_aton" => match parse_ipv4(&h.cstr(a0)) {
            Some(b) => {
                if a1 != 0 {
                    write_bytes(h, a1, &b);
                }
                1
            }
            None => 0,
        },
        // select(n, read, write, except, timeout): no descriptor is ever ready.
        "select" => {
            let bytes = (a0.min(1024) + 7) / 8;
            for set in [a1, a2, a3] {
                if set != 0 {
                    write_bytes(h, set, &vec![0; bytes as usize]);
                }
            }
            let _ = a4;
            0
        }
        "socket" => {
            crate::fs::set_errno(h, EACCES);
            u32::MAX
        }
        "connect" => {
            crate::fs::set_errno(h, ECONNREFUSED);
            u32::MAX
        }
        _ => return false,
    };
    c.r[0] = r0;
    true
}
