//! Static recompiler, ARM (A32) and Thumb (T32) → C.
//!
//! One C function per guest function. Every reachable instruction gets a label
//! (`L_<addr>`), branches become `goto`, `bl` becomes a direct C call, a return
//! (`bx lr`, `pop {.., pc}`) becomes `return`. Only code reachable from the
//! entry is translated, so literal pools and jump tables are never decoded.
//! Anything not supported yet rejects the whole function with a reason; the
//! reasons are the to-do list.
//!
//! The C output is derived from the game: it is written only to the
//! git-ignored build folder and must never be committed or shared.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;

use crate::macho::Binary;

#[path = "recomp_thumb.rs"]
mod thumb;

pub struct Func {
    pub addr: u32,
    pub size: u32,
    pub thumb: bool,
}

/// Everything the translator needs to know about the rest of the program.
pub struct Program<'a> {
    pub bin: &'a Binary,
    /// Known function starts -> Thumb.
    pub starts: &'a BTreeMap<u32, bool>,
    /// Import stub address -> index into the import list.
    pub stubs: &'a HashMap<u32, u32>,
    /// Switch instruction address -> case targets (from the Phase 1 scan).
    pub switches: &'a HashMap<u32, Vec<u32>>,
    /// Thumb `mov pc` switch address -> its table of `b.w` entries (the
    /// jump register holds table + 4 * case).
    pub mov_pc_tables: &'a HashMap<u32, u32>,
    /// End of the read-only __TEXT segment (literals below it can be folded).
    pub ro_end: u32,
}

pub fn c_name(addr: u32) -> String {
    format!("rc_f_{addr:08x}")
}

pub(crate) const COND: [&str; 15] = [
    "c->z",
    "!c->z",
    "c->c",
    "!c->c",
    "c->n",
    "!c->n",
    "c->v",
    "!c->v",
    "(c->c && !c->z)",
    "(!c->c || c->z)",
    "(c->n == c->v)",
    "(c->n != c->v)",
    "(!c->z && c->n == c->v)",
    "(c->z || c->n != c->v)",
    "1",
];

/// How control leaves an instruction.
#[derive(Default)]
struct Flow {
    fallthrough: bool,
    gotos: Vec<u32>,
}

struct Tr<'a> {
    p: &'a Program<'a>,
    f: &'a Func,
    end: u32,
    /// Thumb function: pc reads as the instruction + 4, not + 8.
    thumb: bool,
}

fn bit(x: u32, n: u32) -> bool {
    (x >> n) & 1 != 0
}

fn arm_imm(x: u32) -> (u32, u32) {
    let rot = ((x >> 8) & 0xF) * 2;
    let v = (x & 0xFF).rotate_right(rot);
    (v, rot)
}

impl<'a> Tr<'a> {
    fn in_fn(&self, t: u32) -> bool {
        t >= self.f.addr && t < self.end
    }

    /// Register read as a C expression (pc reads as the instruction + 8, or
    /// + 4 in Thumb).
    fn r(&self, n: u32, a: u32) -> String {
        if n == 15 {
            format!("0x{:x}u", a.wrapping_add(if self.thumb { 4 } else { 8 }))
        } else {
            format!("c->r[{n}]")
        }
    }

    /// Immediate-shifted register operand: (value expr, carry-out expr).
    /// `m` is the C variable holding the register value.
    fn shift_imm(&self, ty: u32, n: u32, m: &str) -> (String, String) {
        match (ty, n) {
            (0, 0) => (m.into(), "c->c".into()),
            (0, n) => (format!("({m} << {n})"), format!("(({m} >> {}) & 1)", 32 - n)),
            (1, 0) => ("0u".into(), format!("({m} >> 31)")),
            (1, n) => (format!("({m} >> {n})"), format!("(({m} >> {}) & 1)", n - 1)),
            (2, 0) => (format!("(uint32_t)((int32_t){m} >> 31)"), format!("({m} >> 31)")),
            (2, n) => (format!("(uint32_t)((int32_t){m} >> {n})"), format!("(({m} >> {}) & 1)", n - 1)),
            (3, 0) => (format!("((c->c << 31) | ({m} >> 1))"), format!("({m} & 1)")),
            (_, n) => (format!("rc_ror({m}, {n})"), format!("(({m} >> {}) & 1)", n - 1)),
        }
    }

    /// Operand 2 of a data-processing instruction. Returns C statements that
    /// set `op2` and `sc` (shifter carry).
    fn operand2(&self, x: u32, a: u32) -> Result<String, String> {
        if bit(x, 25) {
            let (v, rot) = arm_imm(x);
            let carry = if rot == 0 { "c->c".to_string() } else { format!("{}u", v >> 31) };
            return Ok(format!("uint32_t op2 = 0x{v:x}u; uint32_t sc = {carry};"));
        }
        let rm = x & 0xF;
        let ty = (x >> 5) & 3;
        if bit(x, 4) {
            // shift by register
            let rs = (x >> 8) & 0xF;
            if rm == 15 || rs == 15 {
                return Err("pc in register-shifted operand".into());
            }
            let f = ["rc_lsl_c", "rc_lsr_c", "rc_asr_c", "rc_ror_c"][ty as usize];
            return Ok(format!("uint32_t sc = c->c; uint32_t op2 = {f}(c->r[{rm}], c->r[{rs}], &sc);"));
        }
        let n = (x >> 7) & 31;
        let (v, cy) = self.shift_imm(ty, n, "m");
        Ok(format!("uint32_t m = {}; uint32_t op2 = {v}; uint32_t sc = {cy};", self.r(rm, a)))
    }

    fn data_processing(&self, x: u32, a: u32, flow: &mut Flow) -> Result<String, String> {
        let op = (x >> 21) & 0xF;
        let s = bit(x, 20);
        let rn = (x >> 16) & 0xF;
        let rd = (x >> 12) & 0xF;
        if (8..=11).contains(&op) && !s {
            return Err(format!("msr/misc insn {x:08x}"));
        }
        if rn == 15 && bit(x, 4) && !matches!(op, 13 | 15) {
            return Err("pc base with register shift".into());
        }
        let op2 = self.operand2(x, a)?;
        self.dp(op, s, rn, rd, &op2, a, flow, !bit(x, 25) && (x & 0xFFF) == 14)
    }

    /// Data processing from parts: ARM opcode `op` (16 = Thumb `orn`), and C
    /// statements that set `op2` and `sc`. `lr_operand`: op2 is plain lr.
    #[allow(clippy::too_many_arguments)]
    fn dp(&self, op: u32, s: bool, rn: u32, rd: u32, op2: &str, a: u32, flow: &mut Flow, lr_operand: bool) -> Result<String, String> {
        let mut out = String::from("{ ");
        out += op2;
        if !matches!(op, 13 | 15) {
            write!(out, " uint32_t n_ = {};", self.r(rn, a)).unwrap();
        }
        let sf = s as u32;
        let (res, logical) = match op {
            0 => ("n_ & op2".to_string(), true),
            1 => ("n_ ^ op2".into(), true),
            2 => (format!("rc_add(c, n_, ~op2, 1, {sf})"), false),
            3 => (format!("rc_add(c, op2, ~n_, 1, {sf})"), false),
            4 => (format!("rc_add(c, n_, op2, 0, {sf})"), false),
            5 => (format!("rc_add(c, n_, op2, c->c, {sf})"), false),
            6 => (format!("rc_add(c, n_, ~op2, c->c, {sf})"), false),
            7 => (format!("rc_add(c, op2, ~n_, c->c, {sf})"), false),
            8 => ("n_ & op2".into(), true),
            9 => ("n_ ^ op2".into(), true),
            10 => ("rc_add(c, n_, ~op2, 1, 1)".into(), false),
            11 => ("rc_add(c, n_, op2, 0, 1)".into(), false),
            12 => ("n_ | op2".into(), true),
            13 => ("op2".into(), true),
            14 => ("n_ & ~op2".into(), true),
            16 => ("n_ | ~op2".into(), true),
            _ => ("~op2".into(), true),
        };
        if (8..=11).contains(&op) {
            // tst / teq / cmp / cmn: flags only
            if logical {
                write!(out, " uint32_t res = {res}; rc_nz(c, res); c->c = sc;").unwrap();
            } else {
                write!(out, " (void){res};").unwrap();
            }
            out += " }";
            flow.fallthrough = true;
            return Ok(out);
        }
        write!(out, " uint32_t res = {res};").unwrap();
        if s && logical {
            out += " rc_nz(c, res); c->c = sc;";
        }
        if rd == 15 {
            if s {
                return Err("flag-setting write to pc".into());
            }
            // mov pc, lr = return; add pc, pc, rX, lsl #2 = switch (handled before)
            if op == 13 && lr_operand {
                out += " (void)res; return; }";
                return Ok(out);
            }
            return Err("write to pc by data processing".into());
        }
        write!(out, " c->r[{rd}] = res; }}").unwrap();
        flow.fallthrough = true;
        Ok(out)
    }

    /// ldr/str/ldrb/strb (word and unsigned byte).
    fn load_store(&self, x: u32, a: u32, flow: &mut Flow) -> Result<String, String> {
        let (p, u, b, w, l) = (bit(x, 24), bit(x, 23), bit(x, 22), bit(x, 21), bit(x, 20));
        let rn = (x >> 16) & 0xF;
        let rt = (x >> 12) & 0xF;
        let off = if !bit(x, 25) {
            format!("0x{:x}u", x & 0xFFF)
        } else {
            if bit(x, 4) {
                return Err(format!("media instruction {x:08x}"));
            }
            let rm = x & 0xF;
            if rm == 15 {
                return Err("pc as index".into());
            }
            let (v, _) = self.shift_imm((x >> 5) & 3, (x >> 7) & 31, &format!("c->r[{rm}]"));
            v
        };
        let sign = if u { '+' } else { '-' };
        let wb = !p || w;
        if wb && rn == 15 {
            return Err("pc writeback".into());
        }
        // Literal load from read-only __TEXT: fold to a constant.
        if rn == 15 && l && !b && !bit(x, 25) && rt != 15 {
            let lit = if u { a + 8 + (x & 0xFFF) } else { a + 8 - (x & 0xFFF) };
            if lit >= self.p.bin.text.addr && lit + 4 <= self.p.ro_end {
                if let Some(v) = self.p.bin.try_u32(lit) {
                    flow.fallthrough = true;
                    return Ok(format!("c->r[{rt}] = 0x{v:x}u; /* literal */"));
                }
            }
        }
        let base = self.r(rn, a);
        let addr = if p { format!("{base} {sign} {off}") } else { base.clone() };
        let mut out = format!("{{ uint32_t ea = {addr};");
        if l {
            let v = if b { "rc_rd8(c, ea)" } else { "rc_rd32(c, ea)" };
            if rt == 15 {
                // ldr pc, [sp], #4 = return (pop {pc})
                if rn == 13 && !p && u && (x & 0xFFF) == 4 && !b {
                    out += " c->r[13] += 4; return; }";
                    return Ok(out);
                }
                if b {
                    return Err("load to pc".into());
                }
                write!(out, " uint32_t t = {v};").unwrap();
                if wb {
                    write!(out, " c->r[{rn}] = {base} {sign} {off};").unwrap();
                }
                // `mov lr, pc` just before (same condition): an indirect call
                // through a table of function pointers, returning to a + 4.
                let prev = self.p.bin.try_u32(a.wrapping_sub(4)).unwrap_or(0);
                if prev & 0x0FFF_FFFF == 0x01A0_E00F && prev >> 28 == x >> 28 {
                    flow.fallthrough = true;
                    write!(out, " c->r[14] = 0x{:x}u; rc_call(c, t); RC_AFTER_CALL(); }}", a + 4).unwrap();
                } else {
                    // Otherwise a jump to a loaded address: a tail call.
                    flow.fallthrough = x >> 28 != 0xE;
                    out += " rc_call(c, t); return; }";
                }
                return Ok(out);
            }
            write!(out, " uint32_t v = {v};").unwrap();
            if wb {
                write!(out, " c->r[{rn}] = {base} {sign} {off};").unwrap();
            }
            write!(out, " c->r[{rt}] = v; }}").unwrap();
        } else {
            let v = self.r(rt, a);
            let wr = if b { "rc_wr8" } else { "rc_wr32" };
            write!(out, " {wr}(c, ea, {v});").unwrap();
            if wb {
                write!(out, " c->r[{rn}] = {base} {sign} {off};").unwrap();
            }
            out += " }";
        }
        flow.fallthrough = true;
        Ok(out)
    }

    /// ldrh/strh/ldrsb/ldrsh/ldrd/strd.
    fn extra_load_store(&self, x: u32, a: u32, flow: &mut Flow) -> Result<String, String> {
        let (p, u, i, w, l) = (bit(x, 24), bit(x, 23), bit(x, 22), bit(x, 21), bit(x, 20));
        let rn = (x >> 16) & 0xF;
        let rt = (x >> 12) & 0xF;
        let sh = (x >> 5) & 3;
        let off = if i {
            format!("0x{:x}u", ((x >> 4) & 0xF0) | (x & 0xF))
        } else {
            if x & 0xF == 15 {
                return Err("pc as index".into());
            }
            format!("c->r[{}]", x & 0xF)
        };
        let sign = if u { '+' } else { '-' };
        let wb = !p || w;
        if rt == 15 || (wb && rn == 15) {
            return Err("pc in halfword/dual access".into());
        }
        let base = self.r(rn, a);
        let addr = if p { format!("{base} {sign} {off}") } else { base.clone() };
        let mut out = format!("{{ uint32_t ea = {addr};");
        let wbs = if wb { format!(" c->r[{rn}] = {base} {sign} {off};") } else { String::new() };
        match (l, sh) {
            (true, 1) => write!(out, " uint32_t v = rc_rd16(c, ea);{wbs} c->r[{rt}] = v; }}").unwrap(),
            (true, 2) => write!(out, " uint32_t v = (uint32_t)(int32_t)(int8_t)rc_rd8(c, ea);{wbs} c->r[{rt}] = v; }}").unwrap(),
            (true, 3) => write!(out, " uint32_t v = (uint32_t)(int32_t)(int16_t)rc_rd16(c, ea);{wbs} c->r[{rt}] = v; }}").unwrap(),
            (false, 1) => write!(out, " rc_wr16(c, ea, {});{wbs} }}", self.r(rt, a)).unwrap(),
            (false, 2) | (false, 3) => {
                if rt % 2 != 0 || rt == 14 {
                    return Err("odd register pair".into());
                }
                if sh == 2 {
                    write!(out, " uint32_t v0 = rc_rd32(c, ea), v1 = rc_rd32(c, ea + 4);{wbs} c->r[{rt}] = v0; c->r[{}] = v1; }}", rt + 1).unwrap();
                } else {
                    write!(out, " rc_wr32(c, ea, c->r[{rt}]); rc_wr32(c, ea + 4, c->r[{}]);{wbs} }}", rt + 1).unwrap();
                }
            }
            _ => return Err("unknown extra load/store".into()),
        }
        flow.fallthrough = true;
        Ok(out)
    }

    /// ldm/stm (push/pop).
    fn block(&self, x: u32, a: u32, flow: &mut Flow) -> Result<String, String> {
        let (p, u, s, w, l) = (bit(x, 24), bit(x, 23), bit(x, 22), bit(x, 21), bit(x, 20));
        let rn = (x >> 16) & 0xF;
        let list = x & 0xFFFF;
        if s || rn == 15 || list == 0 {
            return Err("ldm/stm user registers or pc base".into());
        }
        let n = list.count_ones();
        let start = match (p, u) {
            (false, true) => "0".to_string(),
            (true, true) => "4".into(),
            (false, false) => format!("-{}", 4 * n - 4),
            (true, false) => format!("-{}", 4 * n),
        };
        let mut out = format!("{{ uint32_t ea = c->r[{rn}] + (uint32_t)({start});");
        let mut k = 0;
        let mut pc_loaded = false;
        for r in 0..16 {
            if list & (1 << r) == 0 {
                continue;
            }
            if l {
                if r == 15 {
                    pc_loaded = true;
                } else {
                    write!(out, " uint32_t v{r} = rc_rd32(c, ea + {});", 4 * k).unwrap();
                }
            } else {
                let v = if r == 15 { format!("0x{:x}u", a + 8) } else { format!("c->r[{r}]") };
                write!(out, " rc_wr32(c, ea + {}, {v});", 4 * k).unwrap();
            }
            k += 1;
        }
        if w && !(l && list & (1 << rn) != 0) {
            let d = if u { format!("+ {}", 4 * n) } else { format!("- {}", 4 * n) };
            write!(out, " c->r[{rn}] = c->r[{rn}] {d};").unwrap();
        }
        if l {
            for r in 0..15 {
                if list & (1 << r) != 0 {
                    write!(out, " c->r[{r}] = v{r};").unwrap();
                }
            }
        }
        if pc_loaded {
            out += " return; }";
            return Ok(out);
        }
        out += " }";
        flow.fallthrough = true;
        Ok(out)
    }

    fn multiply(&self, x: u32, flow: &mut Flow) -> Result<String, String> {
        let rd = (x >> 16) & 0xF;
        let ra = (x >> 12) & 0xF;
        let rs = (x >> 8) & 0xF;
        let rm = x & 0xF;
        let s = bit(x, 20);
        let out = if (x & 0x0FE0_00F0) == 0x0000_0090 {
            // mul
            format!("{{ uint32_t res = c->r[{rm}] * c->r[{rs}]; c->r[{rd}] = res;{} }}", if s { " rc_nz(c, res);" } else { "" })
        } else if (x & 0x0FE0_00F0) == 0x0020_0090 {
            format!("{{ uint32_t res = c->r[{rm}] * c->r[{rs}] + c->r[{ra}]; c->r[{rd}] = res;{} }}", if s { " rc_nz(c, res);" } else { "" })
        } else if (x & 0x0FF0_00F0) == 0x0060_0090 {
            format!("c->r[{rd}] = c->r[{ra}] - c->r[{rm}] * c->r[{rs}];")
        } else if (x & 0x0F80_00F0) == 0x0080_0090 {
            // umull / umlal / smull / smlal: RdHi = 19-16, RdLo = 15-12
            let (signed, acc) = (bit(x, 22), bit(x, 21));
            let prod = if signed {
                format!("(uint64_t)((int64_t)(int32_t)c->r[{rm}] * (int64_t)(int32_t)c->r[{rs}])")
            } else {
                format!("((uint64_t)c->r[{rm}] * (uint64_t)c->r[{rs}])")
            };
            let accs = if acc { format!(" + (((uint64_t)c->r[{rd}] << 32) | c->r[{ra}])") } else { String::new() };
            let fl = if s { " c->n = (uint32_t)(res >> 63); c->z = res == 0;" } else { "" };
            format!("{{ uint64_t res = {prod}{accs}; c->r[{ra}] = (uint32_t)res; c->r[{rd}] = (uint32_t)(res >> 32);{fl} }}")
        } else {
            return Err("unknown multiply".into());
        };
        if rd == 15 || ra == 15 || rs == 15 || rm == 15 {
            return Err("pc in multiply".into());
        }
        flow.fallthrough = true;
        Ok(out)
    }

    /// ARMv6 signed multiplies: smmul/smmla/smmls (high word of a 64-bit
    /// product, R = round) and smulxy/smlaxy (16 x 16, halves picked by N/M).
    /// The Q (saturation) flag smlaxy can set isn't modelled.
    fn dsp_multiply(&self, x: u32, flow: &mut Flow) -> Option<Result<String, String>> {
        let (rd, ra, rm, rn) = ((x >> 16) & 0xF, (x >> 12) & 0xF, (x >> 8) & 0xF, x & 0xF);
        let prod = format!("(uint64_t)((int64_t)(int32_t)c->r[{rn}] * (int64_t)(int32_t)c->r[{rm}])");
        let round = if bit(x, 5) { " + 0x80000000u" } else { "" };
        let half = |r: u32, top: bool| {
            if top {
                format!("(int32_t)(int16_t)(c->r[{r}] >> 16)")
            } else {
                format!("(int32_t)(int16_t)c->r[{r}]")
            }
        };
        let out = if (x & 0x0FF0_F0D0) == 0x0750_F010 {
            format!("c->r[{rd}] = (uint32_t)(({prod}{round}) >> 32);")
        } else if (x & 0x0FF0_00D0) == 0x0750_0010 {
            format!("c->r[{rd}] = (uint32_t)((((uint64_t)c->r[{ra}] << 32) + {prod}{round}) >> 32);")
        } else if (x & 0x0FF0_00D0) == 0x0750_00D0 {
            format!("c->r[{rd}] = (uint32_t)((((uint64_t)c->r[{ra}] << 32) - {prod}{round}) >> 32);")
        } else if (x & 0x0FF0_0090) == 0x0160_0080 {
            format!("c->r[{rd}] = (uint32_t)({} * {});", half(rn, bit(x, 5)), half(rm, bit(x, 6)))
        } else if (x & 0x0FF0_0090) == 0x0100_0080 {
            format!("c->r[{rd}] = (uint32_t)({} * {}) + c->r[{ra}];", half(rn, bit(x, 5)), half(rm, bit(x, 6)))
        } else {
            return None;
        };
        if rd == 15 || rm == 15 || rn == 15 || (ra == 15 && (x & 0x0FF0_F0D0) != 0x0750_F010 && (x & 0x0FF0_0090) != 0x0160_0080) {
            return Some(Err("pc in multiply".into()));
        }
        flow.fallthrough = true;
        Some(Ok(out))
    }

    /// One instruction → (C statement, flow). Conditions are applied by the caller.
    fn insn(&self, x: u32, a: u32, flow: &mut Flow) -> Result<String, String> {
        let cond = x >> 28;
        if cond == 0xF {
            // Unconditional space: barriers, preload, blx imm.
            if (x & 0xFFFF_FFF0) == 0xF57F_F050 || (x & 0xFFFF_FFF0) == 0xF57F_F040 || (x & 0xFFFF_FFF0) == 0xF57F_F060 {
                flow.fallthrough = true;
                return Ok("/* barrier */".into());
            }
            if (x & 0xFD70_F000) == 0xF550_F000 {
                flow.fallthrough = true;
                return Ok("/* pld */".into());
            }
            if (x & 0xFE00_0000) == 0xFA00_0000 {
                let t = a.wrapping_add(8).wrapping_add((((x << 8) as i32) >> 6) as u32) + ((x >> 24) & 1) * 2;
                flow.fallthrough = !self.noreturn(t);
                return self.call(t, Some(a + 4));
            }
            // NEON vadd/vsub/vmul.f32 on d registers (two float lanes): the
            // only NEON the game uses.
            let op = match x & 0xFFB0_0F50 {
                0xF200_0D00 => Some("add"),
                0xF220_0D00 => Some("sub"),
                0xF300_0D10 => Some("mul"),
                _ => None,
            };
            if let Some(op) = op {
                let dd = (((x >> 22) & 1) << 4) | ((x >> 12) & 0xF);
                let dn = (((x >> 7) & 1) << 4) | ((x >> 16) & 0xF);
                let dm = (((x >> 5) & 1) << 4) | (x & 0xF);
                flow.fallthrough = true;
                return Ok(format!(
                    "{{ uint32_t n0 = c->ext[{}], n1 = c->ext[{}], m0 = c->ext[{}], m1 = c->ext[{}]; \
                     c->ext[{}] = rc_n{op}_s(n0, m0); c->ext[{}] = rc_n{op}_s(n1, m1); }}",
                    2 * dn,
                    2 * dn + 1,
                    2 * dm,
                    2 * dm + 1,
                    2 * dd,
                    2 * dd + 1
                ));
            }
            if matches!(x >> 24, 0xF2 | 0xF3 | 0xF4) {
                return Err("NEON".into());
            }
            return Err(format!("unconditional-space insn {x:08x}"));
        }
        let rd = (x >> 12) & 0xF;
        // nop / hints
        if (x & 0x0FFF_FF00) == 0x0320_F000 {
            flow.fallthrough = true;
            return Ok("/* hint */".into());
        }
        // Branches
        if (x & 0x0E00_0000) == 0x0A00_0000 {
            let t = a.wrapping_add(8).wrapping_add((((x << 8) as i32) >> 6) as u32);
            if bit(x, 24) {
                flow.fallthrough = cond != 0xE || !self.noreturn(t);
                return self.call(t, Some(a + 4));
            }
            if self.in_fn(t) {
                flow.gotos.push(t);
                flow.fallthrough = cond != 0xE;
                let guard = if t <= a { "RC_LOOP_GUARD(); " } else { "" };
                return Ok(format!("{{ {guard}goto L_{t:x}; }}"));
            }
            // tail call
            flow.fallthrough = cond != 0xE;
            let call = self.call(t, None)?;
            return Ok(format!("{{ {call} return; }}"));
        }
        // bx / blx register
        if (x & 0x0FFF_FFD0) == 0x012F_FF10 {
            let rm = x & 0xF;
            if bit(x, 5) {
                flow.fallthrough = true;
                return Ok(format!("{{ uint32_t t = c->r[{rm}]; c->r[14] = 0x{:x}u; rc_call(c, t); RC_AFTER_CALL(); }}", a + 4));
            }
            flow.fallthrough = cond != 0xE;
            if rm == 14 {
                return Ok("return;".into());
            }
            return Ok(format!("{{ rc_call(c, c->r[{rm}]); return; }}"));
        }
        // Switch tables (from the Phase 1 scan).
        if (x & 0x0FFF_FFF0) == 0x008F_F100 || (x & 0x0FFF_FFF0) == 0x079F_F100 {
            let Some(targets) = self.p.switches.get(&a) else { return Err("switch without a table".into()) };
            let rm = x & 0xF;
            let mut s = format!("switch (c->r[{rm}]) {{");
            for (i, &t) in targets.iter().enumerate() {
                let t = if (x & 0x0FFF_FFF0) == 0x008F_F100 { a + 8 + 4 * i as u32 } else { t };
                if !self.in_fn(t) {
                    return Err("switch case outside function".into());
                }
                flow.gotos.push(t);
                write!(s, " case {i}: goto L_{t:x};").unwrap();
            }
            s += " }";
            flow.fallthrough = true; // out-of-range index: falls to the default branch
            return Ok(s);
        }
        // movw / movt
        if (x & 0x0FF0_0000) == 0x0300_0000 {
            let imm = ((x >> 4) & 0xF000) | (x & 0xFFF);
            flow.fallthrough = true;
            return Ok(format!("c->r[{rd}] = 0x{imm:x}u;"));
        }
        if (x & 0x0FF0_0000) == 0x0340_0000 {
            let imm = ((x >> 4) & 0xF000) | (x & 0xFFF);
            flow.fallthrough = true;
            return Ok(format!("c->r[{rd}] = (c->r[{rd}] & 0xFFFFu) | 0x{:x}u;", imm << 16));
        }
        // clz
        if (x & 0x0FFF_0FF0) == 0x016F_0F10 {
            flow.fallthrough = true;
            return Ok(format!("c->r[{rd}] = rc_clz(c->r[{}]);", x & 0xF));
        }
        // mrs rd, apsr / msr apsr_nzcvq, rn
        if (x & 0x0FFF_0FFF) == 0x010F_0000 {
            flow.fallthrough = true;
            return Ok(format!("c->r[{rd}] = (c->n << 31) | (c->z << 30) | (c->c << 29) | (c->v << 28) | 0x10u;"));
        }
        if (x & 0x0FFF_FFF0) == 0x0128_F000 {
            let rn = x & 0xF;
            flow.fallthrough = true;
            return Ok(format!("{{ uint32_t v = c->r[{rn}]; c->n = v >> 31; c->z = (v >> 30) & 1; c->c = (v >> 29) & 1; c->v = (v >> 28) & 1; }}"));
        }
        // ldrex / strex (single core: strex always succeeds)
        if (x & 0x0FF0_0FFF) == 0x0190_0F9F {
            flow.fallthrough = true;
            return Ok(format!("c->r[{rd}] = rc_rd32(c, c->r[{}]);", (x >> 16) & 0xF));
        }
        if (x & 0x0FF0_0FF0) == 0x0180_0F90 {
            flow.fallthrough = true;
            return Ok(format!("{{ rc_wr32(c, c->r[{}], c->r[{}]); c->r[{rd}] = 0; }}", (x >> 16) & 0xF, x & 0xF));
        }
        // Media: extend, bitfields, reverse
        if (x & 0x0F80_03F0) == 0x0680_0070 {
            let op = (x >> 20) & 7;
            let rn = (x >> 16) & 0xF;
            let rot = ((x >> 10) & 3) * 8;
            let v = format!("rc_ror(c->r[{}], {rot})", x & 0xF);
            if op == 0 || op == 4 {
                // sxtb16 / uxtb16 (sxtab16 / uxtab16 with rn): bytes 0 and 2 of the
                // rotated value, each extended into its own 16-bit lane; the add
                // form adds lane by lane (no carry between the halves).
                let lane = |s: &str| {
                    if op == 0 {
                        format!("(uint32_t)(uint16_t)(int16_t)(int8_t)({s})")
                    } else {
                        format!("(({s}) & 0xFFu)")
                    }
                };
                let (lo, hi) = (lane("v_"), lane("v_ >> 16"));
                let (blo, bhi) = if rn == 15 { (String::new(), String::new()) } else { (format!("c->r[{rn}] + "), format!("(c->r[{rn}] >> 16) + ")) };
                flow.fallthrough = true;
                return Ok(format!(
                    "{{ uint32_t v_ = {v}; c->r[{rd}] = (({blo}{lo}) & 0xFFFFu) | (({bhi}{hi}) << 16); }}"
                ));
            }
            let ext = match op {
                2 => format!("(uint32_t)(int32_t)(int8_t){v}"),
                3 => format!("(uint32_t)(int32_t)(int16_t){v}"),
                6 => format!("({v} & 0xFFu)"),
                7 => format!("({v} & 0xFFFFu)"),
                _ => return Err("packed (16-bit lane) extend".into()),
            };
            flow.fallthrough = true;
            return Ok(if rn == 15 {
                format!("c->r[{rd}] = {ext};")
            } else {
                format!("c->r[{rd}] = c->r[{rn}] + {ext};")
            });
        }
        if (x & 0x0FA0_0070) == 0x07A0_0050 {
            // ubfx / sbfx
            let lsb = (x >> 7) & 31;
            let width = ((x >> 16) & 31) + 1;
            let rn = x & 0xF;
            flow.fallthrough = true;
            return Ok(if bit(x, 22) {
                let mask = if width == 32 { 0xFFFF_FFFFu32 } else { (1u32 << width) - 1 };
                format!("c->r[{rd}] = (c->r[{rn}] >> {lsb}) & 0x{mask:x}u;")
            } else {
                format!("c->r[{rd}] = (uint32_t)((int32_t)(c->r[{rn}] << {}) >> {});", 32 - lsb - width, 32 - width)
            });
        }
        if (x & 0x0FE0_0070) == 0x07C0_0010 {
            // bfi / bfc
            let lsb = (x >> 7) & 31;
            let msb = (x >> 16) & 31;
            if msb < lsb {
                return Err("bad bitfield".into());
            }
            let width = msb - lsb + 1;
            let mask: u32 = (if width == 32 { 0xFFFF_FFFF } else { (1u32 << width) - 1 }) << lsb;
            let rn = x & 0xF;
            flow.fallthrough = true;
            return Ok(if rn == 15 {
                format!("c->r[{rd}] &= ~0x{mask:x}u;")
            } else {
                format!("c->r[{rd}] = (c->r[{rd}] & ~0x{mask:x}u) | ((c->r[{rn}] << {lsb}) & 0x{mask:x}u);")
            });
        }
        if (x & 0x0FFF_0FF0) == 0x06BF_0F30 {
            flow.fallthrough = true;
            return Ok(format!("c->r[{rd}] = rc_rev(c->r[{}]);", x & 0xF));
        }
        if (x & 0x0FFF_0FF0) == 0x06BF_0FB0 {
            let m = x & 0xF;
            flow.fallthrough = true;
            return Ok(format!("c->r[{rd}] = ((c->r[{m}] & 0x00FF00FFu) << 8) | ((c->r[{m}] >> 8) & 0x00FF00FFu);"));
        }
        if (x & 0x0FFF_0FF0) == 0x06FF_0FB0 {
            let m = x & 0xF;
            flow.fallthrough = true;
            return Ok(format!("c->r[{rd}] = (uint32_t)(int32_t)(int16_t)(((c->r[{m}] & 0xFFu) << 8) | ((c->r[{m}] >> 8) & 0xFFu));"));
        }
        if let Some(r) = self.dsp_multiply(x, flow) {
            return r;
        }
        match (x >> 25) & 7 {
            0 if (x & 0x90) == 0x90 && (x & 0x60) == 0 => self.multiply(x, flow),
            0 if (x & 0x90) == 0x90 => self.extra_load_store(x, a, flow),
            0 if (x & 0x0190_0000) == 0x0100_0000 => Err(format!("misc insn {x:08x}")),
            0 | 1 => self.data_processing(x, a, flow),
            2 | 3 => self.load_store(x, a, flow),
            4 => self.block(x, a, flow),
            6 | 7 if (x & 0x0E00_0E00) == 0x0C00_0A00 || (x & 0x0F00_0E10) == 0x0E00_0A00 || (x & 0x0F00_0E10) == 0x0E00_0A10 => {
                self.vfp(x, a, flow)
            }
            7 if bit(x, 24) => Err("svc".into()),
            _ => Err(format!("unknown insn {x:08x}")),
        }
    }

    /// VFPv3 (cp10 = single, cp11 = double). Float math goes through the
    /// rc_f*_s / rc_f*_d helpers in rc_cpu.h, which follow the ARM NaN rules.
    fn vfp(&self, x: u32, a: u32, flow: &mut Flow) -> Result<String, String> {
        flow.fallthrough = true;
        let dbl = bit(x, 8);
        let b = |n: u32| bit(x, n) as u32;
        // Register numbers: singles are Vx:bit, doubles bit:Vx.
        let single = |v: u32, hi: u32| (v << 1) | hi;
        let double = |v: u32, hi: u32| (hi << 4) | v;
        let (fd, fn_, fm) = ((x >> 12) & 0xF, (x >> 16) & 0xF, x & 0xF);
        let pick = |v: u32, hi: u32| if dbl { double(v, hi) } else { single(v, hi) };
        let (vd, vn, vm) = (pick(fd, b(22)), pick(fn_, b(7)), pick(fm, b(5)));
        let sfx = if dbl { "d" } else { "s" };
        let get = |n: u32| if dbl { format!("rc_getd(c, {n})") } else { format!("c->ext[{n}]") };
        let set = |n: u32, v: String| if dbl { format!("rc_setd(c, {n}, {v});") } else { format!("c->ext[{n}] = {v};") };

        // Transfers between core registers and VFP.
        if (x & 0x0F00_0E10) == 0x0E00_0A10 {
            let rt = (x >> 12) & 0xF;
            if (x & 0x0FFF_FFFF) == 0x0EF1_FA10 {
                // vmrs APSR_nzcv, fpscr (after vcmp)
                return Ok("{ uint32_t f = c->fpscr; c->n = f >> 31; c->z = (f >> 30) & 1; c->c = (f >> 29) & 1; c->v = (f >> 28) & 1; }".into());
            }
            if rt == 15 {
                return Err("VFP transfer with pc".into());
            }
            if (x & 0x0FFF_0FFF) == 0x0EF1_0A10 {
                return Ok(format!("c->r[{rt}] = c->fpscr;"));
            }
            if (x & 0x0FFF_0FFF) == 0x0EE1_0A10 {
                return Ok(format!("c->fpscr = c->r[{rt}];"));
            }
            if (x & 0x0FE0_0F7F) == 0x0E00_0A10 {
                let sn = single(fn_, b(7));
                return Ok(if bit(x, 20) { format!("c->r[{rt}] = c->ext[{sn}];") } else { format!("c->ext[{sn}] = c->r[{rt}];") });
            }
            if (x & 0x0FD0_0F7F) == 0x0E00_0B10 || (x & 0x0FD0_0F7F) == 0x0E10_0B10 {
                // vmov.32 dN[i] <-> rt
                let w = 2 * double(fn_, b(7)) + b(21);
                return Ok(if bit(x, 20) { format!("c->r[{rt}] = c->ext[{w}];") } else { format!("c->ext[{w}] = c->r[{rt}];") });
            }
            return Err("VFP transfer (NEON scalar)".into());
        }

        // Loads and stores.
        if (x & 0x0E00_0E00) == 0x0C00_0A00 {
            let rn = (x >> 16) & 0xF;
            let rt = (x >> 12) & 0xF;
            if (x & 0x0FE0_0FD0) == 0x0C40_0A10 || (x & 0x0FE0_0FD0) == 0x0C40_0B10 {
                // vmov rt, rt2 <-> two singles / one double
                let w0 = if dbl { 2 * double(fm, b(5)) } else { single(fm, b(5)) };
                if rt == 15 || rn == 15 || (!dbl && w0 == 31) {
                    return Err("bad two-register vmov".into());
                }
                return Ok(if bit(x, 20) {
                    format!("{{ uint32_t a_ = c->ext[{w0}], b_ = c->ext[{}]; c->r[{rt}] = a_; c->r[{rn}] = b_; }}", w0 + 1)
                } else {
                    format!("c->ext[{w0}] = c->r[{rt}]; c->ext[{}] = c->r[{rn}];", w0 + 1)
                });
            }
            let (p, u, w, l) = (bit(x, 24), bit(x, 23), bit(x, 21), bit(x, 20));
            let sign = if u { '+' } else { '-' };
            let first = if dbl { 2 * vd } else { vd };
            if p && !w {
                // vldr / vstr
                let imm = (x & 0xFF) * 4;
                let words = if dbl { 2 } else { 1 };
                if rn == 15 {
                    if !l {
                        return Err("vstr to pc".into());
                    }
                    let pc = if self.thumb { (a + 4) & !3 } else { a + 8 };
                    let lit = if u { pc + imm } else { pc - imm };
                    if lit >= self.p.bin.text.addr && lit + 4 * words <= self.p.ro_end {
                        let mut s = String::new();
                        for k in 0..words {
                            let v = self.p.bin.try_u32(lit + 4 * k).ok_or("VFP literal unreadable")?;
                            write!(s, "c->ext[{}] = 0x{v:x}u; ", first + k).unwrap();
                        }
                        return Ok(format!("{s}/* literal */"));
                    }
                    return Err("VFP literal outside __TEXT".into());
                }
                let mut s = format!("{{ uint32_t ea = c->r[{rn}] {sign} 0x{imm:x}u;");
                for k in 0..words {
                    if l {
                        write!(s, " c->ext[{}] = rc_rd32(c, ea + {});", first + k, 4 * k).unwrap();
                    } else {
                        write!(s, " rc_wr32(c, ea + {}, c->ext[{}]);", 4 * k, first + k).unwrap();
                    }
                }
                return Ok(s + " }");
            }
            // vldm / vstm (vpush / vpop): increment after, or decrement before with writeback
            if p == u || (p && !w) || rn == 15 {
                return Err("VFP load/store mode".into());
            }
            let n = x & 0xFF;
            if dbl && n % 2 != 0 {
                return Err("fldmx/fstmx".into());
            }
            if first + n > if dbl { 64 } else { 32 } || n == 0 {
                return Err("VFP register list out of range".into());
            }
            let start = if u { format!("c->r[{rn}]") } else { format!("c->r[{rn}] - {}u", 4 * n) };
            let mut s = format!("{{ uint32_t ea = {start};");
            for k in 0..n {
                if l {
                    write!(s, " c->ext[{}] = rc_rd32(c, ea + {});", first + k, 4 * k).unwrap();
                } else {
                    write!(s, " rc_wr32(c, ea + {}, c->ext[{}]);", 4 * k, first + k).unwrap();
                }
            }
            if w {
                write!(s, " c->r[{rn}] = c->r[{rn}] {sign} {}u;", 4 * n).unwrap();
            }
            return Ok(s + " }");
        }

        // Data processing.
        let o1 = (x >> 20) & 0xB;
        let op = bit(x, 6);
        let (d, n, m) = (get(vd), get(vn), get(vm));
        let neg = |e: String| format!("rc_fneg_{sfx}({e})");
        let f2 = |name: &str, l: &str, r: &str| format!("rc_f{name}_{sfx}({l}, {r})");
        Ok(match (o1, op) {
            (0 | 1, _) => {
                // vmla / vmls / vnmls / vnmla: product rounded, then added (not fused)
                let prod = f2("mul", &n, &m);
                let (acc, add) = match (o1, op) {
                    (0, false) => (d.clone(), prod),
                    (0, true) => (d.clone(), neg(prod)),
                    (1, false) => (neg(d.clone()), prod),
                    _ => (neg(d.clone()), neg(prod)),
                };
                set(vd, f2("add", &acc, &add))
            }
            (2, false) => set(vd, f2("mul", &n, &m)),
            (2, true) => set(vd, neg(f2("mul", &n, &m))),
            (3, false) => set(vd, f2("add", &n, &m)),
            (3, true) => set(vd, f2("sub", &n, &m)),
            (8, false) => set(vd, f2("div", &n, &m)),
            (0xB, false) => {
                // vmov immediate (VFPExpandImm)
                let imm8 = (((x >> 16) & 0xF) << 4) | (x & 0xF);
                let (s_, b6, cd, efgh) = ((imm8 >> 7) & 1, (imm8 >> 6) & 1, (imm8 >> 4) & 3, imm8 & 0xF);
                if dbl {
                    let v: u64 = ((s_ as u64) << 63)
                        | (((b6 ^ 1) as u64) << 62)
                        | (if b6 == 1 { 0xFFu64 << 54 } else { 0 })
                        | ((cd as u64) << 52)
                        | ((efgh as u64) << 48);
                    set(vd, format!("0x{v:x}ull"))
                } else {
                    let v: u32 = (s_ << 31) | ((b6 ^ 1) << 30) | (if b6 == 1 { 0x1F << 25 } else { 0 }) | (cd << 23) | (efgh << 19);
                    set(vd, format!("0x{v:x}u"))
                }
            }
            (0xB, true) => {
                let opc2 = (x >> 16) & 0xF;
                let b7 = bit(x, 7);
                match (opc2, b7) {
                    (0, false) => set(vd, m),
                    (0, true) => set(vd, format!("rc_fabs_{sfx}({m})")),
                    (1, false) => set(vd, neg(m)),
                    (1, true) => set(vd, format!("rc_fsqrt_{sfx}({m})")),
                    (4 | 5, _) => {
                        let rhs = if opc2 == 5 { if dbl { "0ull".to_string() } else { "0u".into() } } else { m };
                        format!("c->fpscr = (c->fpscr & 0x0FFFFFFFu) | rc_fcmp_{sfx}({d}, {rhs});")
                    }
                    (7, true) => {
                        // vcvt between single and double: the destination is the other size
                        if dbl {
                            format!("c->ext[{}] = rc_dtos({m});", single(fd, b(22)))
                        } else {
                            format!("rc_setd(c, {}, rc_stod({m}));", double(fd, b(22)))
                        }
                    }
                    (8, _) => {
                        // int -> float: the source is always a single register
                        let sm = single(fm, b(5));
                        let f = if b7 { "sitof" } else { "uitof" };
                        set(vd, format!("rc_{f}_{sfx}(c->ext[{sm}])"))
                    }
                    (12 | 13, _) => {
                        // float -> int: the destination is always a single register
                        let f = if opc2 == 13 { "ftosi" } else { "ftoui" };
                        format!("c->ext[{}] = rc_{f}_{sfx}({m}, {});", single(fd, b(22)), b7 as u32)
                    }
                    _ => return Err("VFP convert (fixed point / half)".into()),
                }
            }
            _ => return Err(format!("VFP data op {x:08x}")),
        })
    }

    /// A direct call to a guest address. `bl` sets lr (`Some`); a tail call
    /// (`b` to another function) leaves lr as it is, so the callee returns to
    /// our caller.
    /// True if `t` is the stub of an imported function that never returns
    /// (C++ throw helpers, abort, ...). Code after a call to it is usually a
    /// literal pool, not instructions.
    fn noreturn(&self, t: u32) -> bool {
        let Some(&idx) = self.p.stubs.get(&t) else { return false };
        let Some(i) = self.p.bin.imports.iter().filter(|i| i.kind == "stub").nth(idx as usize) else { return false };
        let n = i.name.as_str();
        n.contains("__throw_")
            || matches!(
                n,
                "_abort" | "_exit" | "__exit" | "___cxa_throw" | "___cxa_rethrow" | "__Unwind_SjLj_Resume" | "__Unwind_Resume"
                    | "___stack_chk_fail" | "_objc_exception_throw" | "_longjmp" | "_siglongjmp" | "___assert_rtn"
                    | "___cxa_bad_cast" | "___cxa_pure_virtual"
            )
    }

    fn call(&self, t: u32, lr: Option<u32>) -> Result<String, String> {
        let lr = match lr {
            Some(v) => format!("c->r[14] = 0x{v:x}u; "),
            None => String::new(),
        };
        if let Some(&idx) = self.p.stubs.get(&t) {
            // The host decides: the checker stops here, the native runner calls
            // its own version of the OS function.
            let end = if self.noreturn(t) { " return;" } else { "" };
            return Ok(format!("{{ {lr}rc_import(c, 0x{t:x}u, {idx}u); RC_AFTER_CALL();{end} }}"));
        }
        if self.p.starts.contains_key(&t) {
            return Ok(format!("{{ {lr}{}(c); RC_AFTER_CALL(); }}", c_name(t)));
        }
        Err(format!("call to non-function {t:#x}"))
    }

    /// One ARM instruction: (size, condition, C, flow).
    fn step_arm(&self, a: u32) -> Result<(u32, u32, String, Flow), String> {
        if a % 4 != 0 {
            return Err(format!("control flow leaves the function at {a:#x}"));
        }
        let x = self.p.bin.u32(a);
        let mut flow = Flow::default();
        let body = self.insn(x, a, &mut flow)?;
        let cond = if x >> 28 == 0xF { 0xE } else { x >> 28 };
        Ok((4, cond, body, flow))
    }

    fn translate(&self) -> Result<(String, BTreeSet<u32>), String> {
        // 1. Reachable instructions: address -> (size, condition, C, flow).
        let mut seen: BTreeMap<u32, (u32, u32, String, Flow)> = BTreeMap::new();
        let mut work = vec![self.f.addr];
        let mut callees = BTreeSet::new();
        while let Some(a) = work.pop() {
            if seen.contains_key(&a) {
                continue;
            }
            if !self.in_fn(a) {
                // Running off the end into the next function (a prologue the
                // scan split in two): the same as a tail call to it, since
                // registers, stack and lr are unchanged.
                if a == self.end && self.p.starts.get(&a) == Some(&self.thumb) {
                    callees.insert(a);
                    seen.insert(a, (0, 0xE, format!("{{ {}(c); return; }}", c_name(a)), Flow::default()));
                    continue;
                }
                return Err(format!("control flow leaves the function at {a:#x}"));
            }
            let (size, cond, body, mut flow) = if self.thumb { self.step_thumb(a)? } else { self.step_arm(a)? };
            if cond != 0xE {
                flow.fallthrough = true;
            }
            for &t in &flow.gotos {
                work.push(t);
            }
            if flow.fallthrough {
                work.push(a + size);
            }
            let mut rest = body.as_str();
            while let Some(t) = rest.find("rc_f_") {
                if let Some(h) = rest.get(t + 5..t + 13) {
                    if let Ok(v) = u32::from_str_radix(h, 16) {
                        callees.insert(v);
                    }
                }
                rest = &rest[t + 5..];
            }
            seen.insert(a, (size, cond, body, flow));
        }
        // Thumb: a branch into the middle of an instruction already decoded.
        let mut prev_end = 0;
        for (&a, (size, ..)) in &seen {
            if a < prev_end {
                return Err(format!("overlapping instructions at {a:#x}"));
            }
            prev_end = a + size;
        }
        // 2. Emit in address order.
        let mut out = String::new();
        writeln!(out, "void {}(Cpu *c) {{", c_name(self.f.addr)).unwrap();
        writeln!(out, "RC_ENTER({:#x}u);", self.f.addr).unwrap();
        let addrs: Vec<u32> = seen.keys().copied().collect();
        for (i, a) in addrs.iter().enumerate() {
            let (size, cond, body, flow) = &seen[a];
            if *cond < 0xE {
                writeln!(out, "L_{a:x}: if ({}) {body}", COND[*cond as usize]).unwrap();
            } else {
                writeln!(out, "L_{a:x}: {body}").unwrap();
            }
            if flow.fallthrough && addrs.get(i + 1) != Some(&(a + size)) {
                writeln!(out, "  goto L_{:x};", a + size).unwrap();
            }
        }
        out += "}\n";
        Ok((out, callees))
    }
}

pub struct Result_ {
    pub ok: BTreeMap<u32, String>,
    pub callees: BTreeMap<u32, BTreeSet<u32>>,
    pub failed: BTreeMap<u32, String>,
}

pub fn translate_all(p: &Program, funcs: &[Func]) -> Result_ {
    let mut r = Result_ { ok: BTreeMap::new(), callees: BTreeMap::new(), failed: BTreeMap::new() };
    for f in funcs {
        let t = Tr { p, f, end: f.addr + f.size, thumb: f.thumb };
        match t.translate() {
            Ok((c, callees)) => {
                r.ok.insert(f.addr, c);
                r.callees.insert(f.addr, callees);
            }
            Err(e) => {
                r.failed.insert(f.addr, e);
            }
        }
    }
    r
}
