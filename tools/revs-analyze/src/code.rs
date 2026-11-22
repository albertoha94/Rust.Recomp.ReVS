//! Scans one function's instructions (ARM or Thumb-2) in a single forward pass:
//! direct calls and tail calls, calls/jumps through registers, switch tables,
//! and the literal-pool words it loads. Data inside the function (literal
//! pools, `tbb`/`tbh` offset tables, `ldr pc` address tables) is skipped, so it
//! is never decoded as instructions.

use std::collections::HashSet;

use serde::Serialize;

use crate::macho::Binary;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeKind {
    Call,
    TailCall,
}

pub struct Edge {
    pub target: u32,
    pub thumb: bool,
    pub kind: EdgeKind,
}

#[derive(Serialize)]
pub struct Switch {
    #[serde(serialize_with = "crate::hex")]
    pub addr: u32,
    pub kind: &'static str,
    #[serde(serialize_with = "crate::hex")]
    pub table: u32,
    /// Case count from the bounds check (`cmp rX, #N` gives N + 1); None when the
    /// bound wasn't found and the table end was guessed.
    pub cases_from_cmp: Option<u32>,
    #[serde(serialize_with = "crate::hex_vec")]
    pub targets: Vec<u32>,
}

#[derive(Default)]
pub struct Scan {
    pub edges: Vec<Edge>,
    pub switches: Vec<Switch>,
    pub indirect_calls: u32,
    pub indirect_jumps: u32,
    /// Words loaded from literal pools and used as they are (absolute values).
    pub literal_words: Vec<u32>,
    /// Addresses built position-independently: a literal-pool offset loaded
    /// into rX, then `add rX, pc` (the usual iOS way to take an address).
    /// (instruction address, computed address)
    pub pic_sites: Vec<(u32, u32)>,
    /// Every call/jump through a register, with what the register held.
    pub indirect: Vec<IndirectCall>,
}

#[derive(Serialize, Clone, Copy)]
pub struct IndirectCall {
    #[serde(serialize_with = "crate::hex")]
    pub at: u32,
    /// "virtual": slot `offset` of a vtable (`ldr v,[obj]; ldr f,[v,#offset]`).
    /// "field": function pointer at `[obj, #offset]`. "deref": word 0 of
    /// something that is not a vtable pattern. "unknown": anything else.
    /// "import": function pointer read from an import pointer slot (`addr`).
    /// "computed": an address built pc-relatively (`addr`, odd = Thumb).
    pub kind: &'static str,
    pub offset: i32,
    #[serde(serialize_with = "crate::hex")]
    pub addr: u32,
    /// `bx` (a jump, e.g. a tail call) instead of `blx`.
    pub jump: bool,
}

/// What a register holds, as far as the indirect-call patterns go.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Val {
    Unknown,
    Word0,      // loaded from [rN, #0]: maybe an object's vtable pointer
    Field(i32), // loaded from [rN, #off]
    Slot(i32),  // loaded from [Word0 register, #off]: a vtable slot
    Pic(u32),   // an address built with `add rX, pc`
    Got(u32),   // loaded from a pc-relative address (an import pointer slot)
}

fn indirect_of(at: u32, v: Val, jump: bool) -> IndirectCall {
    let (kind, offset, addr) = match v {
        Val::Slot(o) => ("virtual", o, 0),
        Val::Field(o) => ("field", o, 0),
        Val::Word0 => ("deref", 0, 0),
        Val::Pic(p) => ("computed", 0, p),
        Val::Got(p) => ("import", 0, p),
        Val::Unknown => ("unknown", 0, 0),
    };
    IndirectCall { at, kind, offset, addr, jump }
}

/// Value of `ldr rT, [rN, #off]` given what rN holds.
fn load_val(base: Val, rn: u32, off: i32) -> Val {
    if rn == 13 || rn == 15 {
        Val::Unknown
    } else if let Val::Pic(p) = base {
        Val::Got(p.wrapping_add(off as u32))
    } else if base == Val::Word0 {
        Val::Slot(off)
    } else if off == 0 {
        Val::Word0
    } else {
        Val::Field(off)
    }
}

/// Registers a call may change (r0-r3, r9, r12, lr on iOS).
const CALL_CLOBBER: u16 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 9) | (1 << 12) | (1 << 14);

fn clobber(val: &mut [Val; 16], mask: u16) {
    for (r, v) in val.iter_mut().enumerate() {
        if mask & (1 << r) != 0 {
            *v = Val::Unknown;
        }
    }
}

/// Registers an ARM instruction writes (approximate; errs toward clearing).
fn arm_writes(x: u32) -> u16 {
    if x >> 28 == 0xF {
        return 0;
    }
    let rd = 1u16 << ((x >> 12) & 0xF);
    let rn = 1u16 << ((x >> 16) & 0xF);
    match (x >> 25) & 7 {
        0 if (x & 0x90) == 0x90 => {
            if (x & 0x0F00_00F0) == 0x0000_0090 {
                // mul/mla (Rd = 19-16); long multiplies also write 15-12
                rn | if x & (1 << 23) != 0 { rd } else { 0 }
            } else if x & (1 << 20) != 0 {
                rd // ldrh/ldrsb/ldrsh
            } else if (x & 0xF0) == 0xD0 {
                rd | (rd << 1) // ldrd
            } else {
                0
            }
        }
        0 | 1 => {
            let op = (x >> 21) & 0xF;
            if (8..=11).contains(&op) {
                if (x >> 20) & 1 == 1 {
                    0 // tst/teq/cmp/cmn
                } else if (x & 0x0FB0_0000) == 0x0300_0000 || (x & 0x0FFF_0FF0) == 0x016F_0F10 {
                    rd // movw/movt, clz
                } else {
                    0
                }
            } else {
                rd
            }
        }
        2 | 3 => {
            if x & (1 << 20) != 0 {
                rd
            } else {
                0
            }
        }
        4 => {
            if x & (1 << 20) != 0 {
                (x & 0xFFFF) as u16
            } else {
                0
            }
        }
        6 if (x & 0x0FF0_0FD0) == 0x0C50_0B10 => rd | rn, // vmov rT, rT2, dM
        7 if (x & 0x0F10_0F10) == 0x0E10_0A10 => rd,     // vmov rT, sN
        _ => 0,
    }
}

fn track_arm(x: u32, a: u32, val: &mut [Val; 16], out: &mut Scan) {
    let cond = x >> 28;
    if (x & 0x0FFF_FFF0) == 0x012F_FF30 {
        out.indirect.push(indirect_of(a, val[(x & 0xF) as usize], false));
    } else if (x & 0x0FFF_FFF0) == 0x012F_FF10 && x & 0xF != 14 {
        out.indirect.push(indirect_of(a, val[(x & 0xF) as usize], true));
    }
    if (x & 0x0F70_0000) == 0x0510_0000 {
        // ldr rT, [rN, #+-imm12]
        let (rn, rt) = ((x >> 16) & 0xF, ((x >> 12) & 0xF) as usize);
        let off = if x & (1 << 23) != 0 { (x & 0xFFF) as i32 } else { -((x & 0xFFF) as i32) };
        val[rt] = load_val(val[rn as usize], rn, off);
    } else if (x & 0x0FEF_0FF0) == 0x01A0_0000 {
        val[((x >> 12) & 0xF) as usize] = val[(x & 0xF) as usize]; // mov rD, rM
    } else {
        clobber(val, arm_writes(x));
    }
    let is_call = (cond != 0xF && (x & 0x0F00_0000) == 0x0B00_0000)
        || (x & 0xFE00_0000) == 0xFA00_0000
        || (x & 0x0FFF_FFF0) == 0x012F_FF30;
    if is_call {
        clobber(val, CALL_CLOBBER);
    }
}

/// Registers a 16-bit Thumb instruction writes (approximate).
fn t16_writes(x: u32) -> u16 {
    let lo = 1u16 << (x & 7);
    let r8 = 1u16 << ((x >> 8) & 7);
    match x >> 11 {
        0x00..=0x03 => lo,
        0x04 => r8,
        0x05 => 0,
        0x06 | 0x07 => r8,
        0x08 if x & 0x0400 == 0 => {
            if matches!((x >> 6) & 0xF, 8 | 10 | 11) {
                0
            } else {
                lo
            }
        }
        0x08 => match (x >> 8) & 3 {
            0 | 2 => 1u16 << ((x & 7) | ((x >> 4) & 8)),
            _ => 0,
        },
        0x09 => r8,
        0x0A => {
            if (x >> 9) & 7 == 3 {
                lo
            } else {
                0
            }
        }
        0x0B | 0x0D | 0x0F | 0x11 => lo,
        0x13 | 0x14 | 0x15 => r8,
        0x16 => {
            if (x & 0xFF00) == 0xB200 {
                lo
            } else {
                0
            }
        }
        0x17 => {
            if (x & 0x0E00) == 0x0C00 {
                (x & 0xFF) as u16 | if x & 0x100 != 0 { 0x8000 } else { 0 }
            } else if (x & 0xFF00) == 0xBA00 {
                lo
            } else {
                0
            }
        }
        0x19 => (x & 0xFF) as u16,
        _ => 0,
    }
}

/// Registers a 32-bit Thumb-2 instruction writes (approximate).
fn t32_writes(x: u32, y: u32) -> u16 {
    let rd = 1u16 << ((y >> 8) & 0xF);
    let rt = 1u16 << ((y >> 12) & 0xF);
    match x >> 8 {
        0xE8 | 0xE9 => {
            if x & 0x40 == 0 {
                if x & 0x10 != 0 {
                    y as u16 // ldm
                } else {
                    0
                }
            } else if x & 0x10 != 0 && (x & 0xFFF0) != 0xE8D0 {
                rt | rd // ldrd
            } else {
                0
            }
        }
        0xEA | 0xEB => rd,
        0xEC..=0xEF | 0xFC..=0xFF => {
            if (x & 0xFF10) == 0xEE10 && (y & 0x0F10) == 0x0A10 {
                rt // vmov rT, sN
            } else if (x & 0xFFF0) == 0xEC50 {
                rt | 1u16 << (x & 0xF) // vmov rT, rT2, dM
            } else {
                0
            }
        }
        0xF0..=0xF7 if y & 0x8000 == 0 => rd,
        0xF8 | 0xF9 => {
            if x & 0x10 != 0 {
                rt
            } else {
                0
            }
        }
        0xFA | 0xFB => {
            if (x & 0xFF80) == 0xFB80 {
                rt | rd
            } else {
                rd
            }
        }
        _ => 0,
    }
}

fn track_t16(x: u32, a: u32, val: &mut [Val; 16], out: &mut Scan) {
    let rm = ((x >> 3) & 0xF) as usize;
    if (x & 0xFF87) == 0x4780 {
        out.indirect.push(indirect_of(a, val[rm], false));
    } else if (x & 0xFF87) == 0x4700 && rm != 14 {
        out.indirect.push(indirect_of(a, val[rm], true));
    }
    if (x & 0xF800) == 0x6800 {
        let (rn, rt) = ((x >> 3) & 7, (x & 7) as usize);
        val[rt] = load_val(val[rn as usize], rn, (((x >> 6) & 0x1F) * 4) as i32);
    } else if (x & 0xFF00) == 0x4600 {
        val[((x & 7) | ((x >> 4) & 8)) as usize] = val[rm];
    } else {
        clobber(val, t16_writes(x));
    }
    if (x & 0xFF87) == 0x4780 {
        clobber(val, CALL_CLOBBER);
    }
}

fn track_t32(x: u32, y: u32, val: &mut [Val; 16]) {
    if (x & 0xFFF0) == 0xF8D0 {
        let (rn, rt) = (x & 0xF, ((y >> 12) & 0xF) as usize);
        val[rt] = load_val(val[rn as usize], rn, (y & 0xFFF) as i32);
    } else {
        clobber(val, t32_writes(x, y));
    }
    if (x & 0xF800) == 0xF000 && (y & 0xC000) == 0xC000 {
        clobber(val, CALL_CLOBBER); // bl / blx
    }
}

/// Last `cmp rN, #imm` seen, to bound the next switch on rN.
#[derive(Clone, Copy)]
struct Cmp {
    at: u32,
    reg: u32,
    imm: u32,
}

const MAX_CASES: u32 = 1024;

fn sext(v: u32, bits: u32) -> i32 {
    ((v << (32 - bits)) as i32) >> (32 - bits)
}

/// The bound check must be close before the switch (a few instructions).
fn bound(cmp: Option<Cmp>, reg: u32, at: u32, window: u32) -> Option<u32> {
    let c = cmp?;
    (c.reg == reg && at > c.at && at - c.at <= window && c.imm < MAX_CASES).then_some(c.imm + 1)
}

pub fn scan(bin: &Binary, start: u32, end: u32, thumb: bool) -> Scan {
    if thumb {
        scan_thumb(bin, start, end)
    } else {
        scan_arm(bin, start, end)
    }
}

fn arm_imm(x: u32) -> u32 {
    (x & 0xFF).rotate_right(((x >> 8) & 0xF) * 2)
}

fn scan_arm(bin: &Binary, start: u32, end: u32) -> Scan {
    let mut out = Scan::default();
    let mut skip: HashSet<u32> = HashSet::new(); // 4-byte data words inside the function
    let skip_words = |skip: &mut HashSet<u32>, at: u32, bytes: u32| {
        let mut w = at & !3;
        while w < at + bytes {
            skip.insert(w);
            w += 4;
        }
    };
    let mut cmp: Option<Cmp> = None;
    let mut loaded: [Option<u32>; 16] = [None; 16]; // literal value last loaded per register
    let mut val = [Val::Unknown; 16];
    let mut a = start;
    while a + 4 <= end {
        if skip.contains(&a) {
            a += 4;
            continue;
        }
        let x = bin.u32(a);
        track_arm(x, a, &mut val, &mut out);
        if (x & 0x0FFF_0FF0) == 0x008F_0000 {
            // add rD, pc, rM: rM held a pc-relative offset
            if let Some(v) = loaded[(x & 0xF) as usize] {
                out.pic_sites.push((a, v.wrapping_add(a + 8)));
                val[((x >> 12) & 0xF) as usize] = Val::Pic(v.wrapping_add(a + 8));
            }
        } else if (x & 0x0FFF_0FF0) == 0x079F_0000 {
            // ldr rT, [pc, rM]: rM held a pc-relative offset to a pointer slot
            if let Some(v) = loaded[(x & 0xF) as usize] {
                val[((x >> 12) & 0xF) as usize] = Val::Got(v.wrapping_add(a + 8));
            }
        }
        let cond = x >> 28;
        let pc = a.wrapping_add(8);
        let off24 = (sext(x & 0x00FF_FFFF, 24) << 2) as u32;
        if cond != 0xF && (x & 0x0F00_0000) == 0x0B00_0000 {
            out.edges.push(Edge { target: pc.wrapping_add(off24), thumb: false, kind: EdgeKind::Call });
        } else if (x & 0xFE00_0000) == 0xFA00_0000 {
            let t = pc.wrapping_add(off24) + ((x >> 24) & 1) * 2;
            out.edges.push(Edge { target: t, thumb: true, kind: EdgeKind::Call });
        } else if cond != 0xF && (x & 0x0F00_0000) == 0x0A00_0000 {
            let t = pc.wrapping_add(off24);
            if t < start || t >= end {
                out.edges.push(Edge { target: t, thumb: false, kind: EdgeKind::TailCall });
            }
        } else if (x & 0x0FFF_FFF0) == 0x012F_FF30 {
            out.indirect_calls += 1;
        } else if (x & 0x0FFF_FFF0) == 0x012F_FF10 {
            if x & 0xF != 14 {
                out.indirect_jumps += 1;
            }
        } else if (x & 0x0FF0_F000) == 0x0350_0000 {
            cmp = Some(Cmp { at: a, reg: (x >> 16) & 0xF, imm: arm_imm(x) });
        } else if (x & 0x0FFF_FFF0) == 0x008F_F100 {
            // addls pc, pc, rX, lsl #2 ; b default ; b case0 ; b case1 ...
            let reg = x & 0xF;
            let n = bound(cmp, reg, a, 16);
            let table = a + 8;
            let mut targets = Vec::new();
            let mut i = 0;
            while i < n.unwrap_or(MAX_CASES) && table + i * 4 + 4 <= end {
                let e = bin.u32(table + i * 4);
                if (e & 0xFF00_0000) != 0xEA00_0000 {
                    break; // the guessed end: first entry that isn't `b`
                }
                targets.push((table + i * 4 + 8).wrapping_add((sext(e & 0x00FF_FFFF, 24) << 2) as u32));
                i += 1;
            }
            out.switches.push(Switch { addr: a, kind: "arm_add_pc", table, cases_from_cmp: n, targets });
            // The entries are ordinary `b` instructions: keep decoding them.
        } else if (x & 0x0FFF_FFF0) == 0x079F_F100 {
            // ldrls pc, [pc, rX, lsl #2] ; b default ; .word case0 ; .word case1 ...
            let reg = x & 0xF;
            let n = bound(cmp, reg, a, 16);
            let table = a + 8;
            let mut targets = Vec::new();
            let mut i = 0;
            while i < n.unwrap_or(MAX_CASES) && table + i * 4 + 4 <= end {
                let t = bin.u32(table + i * 4);
                if n.is_none() && (t < start || t >= end) {
                    break;
                }
                targets.push(t);
                i += 1;
            }
            skip_words(&mut skip, table, i * 4);
            out.switches.push(Switch { addr: a, kind: "arm_ldr_pc", table, cases_from_cmp: n, targets });
        } else if (x & 0x0F7F_0000) == 0x051F_0000 {
            // ldr rT, [pc, #+-imm12]
            let imm = x & 0xFFF;
            let lit = if x & (1 << 23) != 0 { pc.wrapping_add(imm) } else { pc.wrapping_sub(imm) };
            if lit >= start && lit + 4 <= end {
                skip_words(&mut skip, lit, 4);
            }
            if let Some(w) = bin.try_u32(lit) {
                out.literal_words.push(w);
                loaded[((x >> 12) & 0xF) as usize] = Some(w);
            }
        } else if (x & 0x0F3F_0E00) == 0x0D1F_0A00 {
            // vldr sD/dD, [pc, #+-imm8*4]
            let imm = (x & 0xFF) * 4;
            let lit = if x & (1 << 23) != 0 { pc.wrapping_add(imm) } else { pc.wrapping_sub(imm) };
            if lit >= start && lit < end {
                skip_words(&mut skip, lit, if x & 0x100 != 0 { 8 } else { 4 });
            }
        } else if (x & 0x0E5F_00F0) == 0x004F_00D0 || (x & 0x0E5F_00F0) == 0x005F_00B0 {
            // ldrd / ldrh literal (imm4H:imm4L)
            let imm = ((x >> 4) & 0xF0) | (x & 0xF);
            let lit = if x & (1 << 23) != 0 { pc.wrapping_add(imm) } else { pc.wrapping_sub(imm) };
            if lit >= start && lit < end {
                skip_words(&mut skip, lit, if (x & 0xF0) == 0xD0 { 8 } else { 2 });
            }
        }
        a += 4;
    }
    out
}

/// Thumb-2 BL/BLX/B.W T4 offset (S:I1:I2:imm10:imm11:0).
fn t32_branch_offset(x: u32, y: u32) -> i32 {
    let s = (x >> 10) & 1;
    let (j1, j2) = ((y >> 13) & 1, (y >> 11) & 1);
    let i1 = !(j1 ^ s) & 1;
    let i2 = !(j2 ^ s) & 1;
    let imm = (s << 24) | (i1 << 23) | (i2 << 22) | ((x & 0x3FF) << 12) | ((y & 0x7FF) << 1);
    sext(imm, 25)
}

fn scan_thumb(bin: &Binary, start: u32, end: u32) -> Scan {
    let mut out = Scan::default();
    let mut skip: HashSet<u32> = HashSet::new(); // halfwords of data inside the function
    let skip_range = |skip: &mut HashSet<u32>, at: u32, bytes: u32| {
        let mut h = at & !1;
        while h < at + bytes {
            skip.insert(h);
            h += 2;
        }
    };
    let mut cmp: Option<Cmp> = None;
    let mut addw: Option<(u32, u32, u32)> = None; // (at, rd, address) of `addw rd, pc, #imm` or `adr rd, #imm`
    let mut loaded: [Option<u32>; 16] = [None; 16]; // literal value last loaded per register
    let mut val = [Val::Unknown; 16];
    let mut a = start;
    while a + 2 <= end {
        if skip.contains(&a) {
            a += 2;
            continue;
        }
        let x = bin.u16(a) as u32;
        let pc = a + 4;
        let pc_al = pc & !3;
        if matches!(x >> 11, 0x1D | 0x1E | 0x1F) {
            if a + 4 > end {
                break;
            }
            let y = bin.u16(a + 2) as u32;
            track_t32(x, y, &mut val);
            if (x & 0xF800) == 0xF000 && (y & 0xC000) == 0xC000 {
                let off = t32_branch_offset(x, y) as u32;
                if y & 0x1000 != 0 {
                    out.edges.push(Edge { target: pc.wrapping_add(off), thumb: true, kind: EdgeKind::Call });
                } else {
                    out.edges.push(Edge { target: pc_al.wrapping_add(off), thumb: false, kind: EdgeKind::Call });
                }
            } else if (x & 0xF800) == 0xF000 && (y & 0xD000) == 0x9000 {
                let t = pc.wrapping_add(t32_branch_offset(x, y) as u32);
                if t < start || t >= end {
                    out.edges.push(Edge { target: t, thumb: true, kind: EdgeKind::TailCall });
                }
            } else if (x & 0xFBF0) == 0xF1B0 && (y & 0x8F00) == 0x0F00 {
                // cmp.w rN, #imm (only plain 8-bit immediates are bounds we can use)
                let small = (x >> 10) & 1 == 0 && (y >> 12) & 7 == 0;
                cmp = small.then_some(Cmp { at: a, reg: x & 0xF, imm: y & 0xFF });
            } else if (x & 0xFFF0) == 0xE8D0 && (y & 0xFFE0) == 0xF000 && (x & 0xF) == 15 {
                // tbb [pc, rM] / tbh [pc, rM, lsl #1]; the table follows.
                let half = (y >> 4) & 1 != 0;
                let es = if half { 2 } else { 1 };
                let n = bound(cmp, y & 0xF, a, 8);
                let table = a + 4;
                let mut targets = Vec::new();
                let mut lowest = u32::MAX; // the table can't run into the first case
                let mut i = 0;
                while i < n.unwrap_or(MAX_CASES) {
                    let e = table + i * es;
                    if e + es > end || (n.is_none() && e >= lowest) {
                        break;
                    }
                    let v = if half { bin.u16(e) as u32 } else { bin.bytes(e, 1).unwrap()[0] as u32 };
                    let t = table + v * 2;
                    lowest = lowest.min(t);
                    targets.push(t);
                    i += 1;
                }
                skip_range(&mut skip, table, (i * es + 1) & !1);
                let kind = if half { "thumb_tbh" } else { "thumb_tbb" };
                out.switches.push(Switch { addr: a, kind, table, cases_from_cmp: n, targets });
            } else if x == 0xF8DF || x == 0xF85F {
                // ldr.w rT, [pc, #+-imm12]
                let imm = y & 0xFFF;
                let lit = if x == 0xF8DF { pc_al + imm } else { pc_al.wrapping_sub(imm) };
                if lit >= start && lit + 4 <= end {
                    skip_range(&mut skip, lit, 4);
                }
                if let Some(w) = bin.try_u32(lit) {
                    out.literal_words.push(w);
                    loaded[((y >> 12) & 0xF) as usize] = Some(w);
                }
            } else if (x & 0xFF7F) == 0xE95F {
                // ldrd rT, rT2, [pc, #+-imm8*4]
                let imm = (y & 0xFF) * 4;
                let lit = if x & 0x80 != 0 { pc_al + imm } else { pc_al.wrapping_sub(imm) };
                if lit >= start && lit < end {
                    skip_range(&mut skip, lit, 8);
                }
            } else if (x & 0xFF3F) == 0xED1F && (y & 0x0E00) == 0x0A00 {
                // vldr sD/dD, [pc, #+-imm8*4]
                let imm = (y & 0xFF) * 4;
                let lit = if x & 0x80 != 0 { pc_al + imm } else { pc_al.wrapping_sub(imm) };
                if lit >= start && lit < end {
                    skip_range(&mut skip, lit, if y & 0x100 != 0 { 8 } else { 4 });
                }
            } else if (x & 0xFBFF) == 0xF20F && (y & 0x8000) == 0 {
                // addw rD, pc, #imm12 (base of a `mov pc` branch table)
                let imm = ((x >> 10) & 1) << 11 | ((y >> 12) & 7) << 8 | (y & 0xFF);
                addw = Some((a, (y >> 8) & 0xF, pc_al + imm));
            }
            a += 4;
            continue;
        }
        track_t16(x, a, &mut val, &mut out);
        if (x & 0xF800) == 0x4800 {
            // ldr rT, [pc, #imm8*4]
            let lit = pc_al + (x & 0xFF) * 4;
            if lit + 4 <= end {
                skip_range(&mut skip, lit, 4);
            }
            if let Some(w) = bin.try_u32(lit) {
                out.literal_words.push(w);
                loaded[((x >> 8) & 7) as usize] = Some(w);
            }
        } else if (x & 0xFF78) == 0x4478 {
            // add rD, pc (rD = D:rdn)
            let rd = (x & 7) | ((x >> 4) & 8);
            if let Some(v) = loaded[rd as usize] {
                out.pic_sites.push((a, v.wrapping_add(pc)));
                val[rd as usize] = Val::Pic(v.wrapping_add(pc));
            }
        } else if (x & 0xF800) == 0xA000 {
            // adr rD, #imm8*4 (16-bit base of a `mov pc` branch table)
            addw = Some((a, (x >> 8) & 7, pc_al + (x & 0xFF) * 4));
        } else if (x & 0xF800) == 0x2800 {
            cmp = Some(Cmp { at: a, reg: (x >> 8) & 7, imm: x & 0xFF });
        } else if (x & 0xFF87) == 0x4780 {
            out.indirect_calls += 1;
        } else if (x & 0xFF87) == 0x4700 {
            if (x >> 3) & 0xF != 14 {
                out.indirect_jumps += 1;
            }
        } else if (x & 0xFF87) == 0x4687 {
            // mov pc, rM: a table of 4-byte `b.w` entries at the addw base.
            match addw.filter(|&(at, _, _)| a - at <= 12) {
                Some((at, rd, table)) => {
                    let n = bound(cmp, rd_index_reg(bin, at, a).unwrap_or(rd), a, 20);
                    let mut targets = Vec::new();
                    let mut i = 0;
                    while i < n.unwrap_or(MAX_CASES) && table + i * 4 + 4 <= end {
                        let (ex, ey) = (bin.u16(table + i * 4) as u32, bin.u16(table + i * 4 + 2) as u32);
                        if (ex & 0xF800) != 0xF000 || (ey & 0xD000) != 0x9000 {
                            break;
                        }
                        targets.push((table + i * 4 + 4).wrapping_add(t32_branch_offset(ex, ey) as u32));
                        i += 1;
                    }
                    out.switches.push(Switch { addr: a, kind: "thumb_mov_pc", table, cases_from_cmp: n, targets });
                }
                None => out.indirect_jumps += 1,
            }
        } else if (x & 0xF800) == 0xE000 {
            let t = pc.wrapping_add((sext(x & 0x7FF, 11) << 1) as u32);
            if t < start || t >= end {
                out.edges.push(Edge { target: t, thumb: true, kind: EdgeKind::TailCall });
            }
        }
        a += 2;
    }
    out
}

/// In `addw rD, pc, #i ; add.w rD, rD, rX, lsl #2 ; mov pc, rD` (or 16-bit
/// `adr rD, #i` first) the case index is rX: find it in the `add.w` between.
fn rd_index_reg(bin: &Binary, addw_at: u32, mov_at: u32) -> Option<u32> {
    let mut a = addw_at + if (bin.u16(addw_at) as u32 & 0xF800) == 0xA000 { 2 } else { 4 };
    while a + 4 <= mov_at {
        let (x, y) = (bin.u16(a) as u32, bin.u16(a + 2) as u32);
        if (x & 0xFFE0) == 0xEB00 {
            return Some(y & 0xF);
        }
        a += 2;
    }
    None
}

/// The instruction at `t` looks like a function entry: `push {..., lr}`
/// (ARM `stmdb sp!`, Thumb `push` with lr, or Thumb-2 `stmdb.w sp!`).
pub fn looks_like_entry(bin: &Binary, t: u32, thumb: bool) -> bool {
    if thumb {
        bin.bytes(t, 2).is_some() && {
            let x = bin.u16(t);
            (x & 0xFF00) == 0xB500 || x == 0xE92D
        }
    } else {
        bin.try_u32(t).is_some_and(|x| (x & 0xFFFF_4000) == 0xE92D_4000)
    }
}

/// An ARM leaf function (no `push`) starting at `t`, inside the function that
/// starts at `f`: the code before `t` ends (a return or `b`), possibly followed
/// by that code's literal pool (words loaded by `ldr`/`vldr [pc, #imm]` from
/// `f..t`), and the word at `t` is an unconditional instruction, not a literal.
/// Seen in tables of small float helpers (3 per entry around 0x382300) that
/// the game calls only through those tables.
pub fn leaf_boundary(bin: &Binary, f: u32, t: u32, thumb: bool) -> bool {
    if thumb || t < 4 || bin.try_u32(t).is_none() {
        return false;
    }
    let mut lits = std::collections::HashSet::new();
    let mut a = f;
    while a < t {
        let x = bin.u32(a);
        if x & 0x0F7F_0000 == 0x051F_0000 {
            // ldr rX, [pc, #+-imm12]
            let off = x & 0xFFF;
            lits.insert(if x & 1 << 23 != 0 { a + 8 + off } else { (a + 8).wrapping_sub(off) });
        } else if x & 0x0F3F_0E00 == 0x0D1F_0A00 {
            // vldr sX/dX, [pc, #+-imm8*4]
            let off = (x & 0xFF) * 4;
            lits.insert(if x & 1 << 23 != 0 { a + 8 + off } else { (a + 8).wrapping_sub(off) });
        }
        a += 4;
    }
    let mut p = t - 4;
    while lits.contains(&p) && p > f {
        p -= 4;
    }
    !lits.contains(&t) && bin.u32(t) >> 28 == 0xE && prev_ends(bin, p + 4, false)
}

/// Stricter `leaf_boundary` for pointers held in literal pools (most of those
/// are integers, SjLj landing pads or switch cases): the code before must be a
/// return (`bx lr` / `pop {..., pc}`), not a `b`; the target is not a `nop`
/// and no branch inside its function `f..end` jumps to it. Accepts the qsort
/// comparators at 0x2e0b68 / 0x2e0b80 (the second match-start stop) and two
/// more leaves, 4 in all on 1.10.10.
pub fn literal_leaf_boundary(bin: &Binary, f: u32, end: u32, t: u32, thumb: bool) -> bool {
    if !leaf_boundary(bin, f, t, thumb) || bin.u32(t) == 0xE1A0_0000 {
        return false;
    }
    let mut a = f;
    while a < end {
        let x = bin.u32(a);
        if (x >> 25) & 7 == 5 && x >> 28 != 0xF && x & 1 << 24 == 0 {
            let off = ((x << 8) as i32 >> 8) * 4;
            if a.wrapping_add(8).wrapping_add(off as u32) == t {
                return false;
            }
        }
        a += 4;
    }
    let mut lits = std::collections::HashSet::new();
    let mut a = f;
    while a < t {
        let x = bin.u32(a);
        if x & 0x0F7F_0000 == 0x051F_0000 || x & 0x0F3F_0E00 == 0x0D1F_0A00 {
            let off = if x & 0x0F7F_0000 == 0x051F_0000 { x & 0xFFF } else { (x & 0xFF) * 4 };
            lits.insert(if x & 1 << 23 != 0 { a + 8 + off } else { (a + 8).wrapping_sub(off) });
        }
        a += 4;
    }
    let mut p = t - 4;
    while lits.contains(&p) && p > f {
        p -= 4;
    }
    let x = bin.u32(p);
    x == 0xE12F_FF1E || (x & 0xFFFF_8000) == 0xE8BD_8000
}

/// The instruction before `t` ends a function: a return, an unconditional
/// branch, or padding.
pub fn prev_ends(bin: &Binary, t: u32, thumb: bool) -> bool {
    if thumb {
        bin.bytes(t.wrapping_sub(2), 2).is_some() && {
            let x = bin.u16(t - 2);
            matches!(x, 0x4770 | 0xBF00 | 0x46C0) || (x & 0xFF00) == 0xBD00 || (x & 0xF800) == 0xE000
        }
    } else {
        bin.try_u32(t.wrapping_sub(4)).is_some_and(|x| {
            matches!(x, 0xE12F_FF1E | 0) || (x & 0xFFFF_8000) == 0xE8BD_8000 || (x & 0xFF00_0000) == 0xEA00_0000
        })
    }
}
