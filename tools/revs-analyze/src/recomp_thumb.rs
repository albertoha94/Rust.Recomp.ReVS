//! Thumb (T32) for the recompiler.
//!
//! Most Thumb instructions have an ARM twin with the same meaning; those are
//! re-encoded as the ARM word and go through the (already checked) ARM
//! translator. Only what differs gets its own C here: pc-relative forms (pc
//! reads as the instruction + 4, literals use it rounded down to 4), branches,
//! `cbz`/`cbnz`, IT blocks, table branches, Thumb's modified immediates and
//! `orn`.
//!
//! In an IT block the 16-bit data-processing instructions don't set flags;
//! the block is translated as one unit (nothing may branch into its middle).

use std::fmt::Write;

use super::{bit, Flow, Tr, COND};

/// Sign-extends the low `bits` bits.
fn sx(v: u32, bits: u32) -> u32 {
    (((v << (32 - bits)) as i32) >> (32 - bits)) as u32
}

/// ThumbExpandImm_C: (value, carry-out as a C expression).
fn expand_imm(imm12: u32) -> (u32, String) {
    let b = imm12 & 0xFF;
    if imm12 >> 10 == 0 {
        let v = match (imm12 >> 8) & 3 {
            0 => b,
            1 => b | (b << 16),
            2 => (b << 8) | (b << 24),
            _ => b | (b << 8) | (b << 16) | (b << 24),
        };
        (v, "c->c".into())
    } else {
        let v = (0x80 | (imm12 & 0x7F)).rotate_right(imm12 >> 7);
        (v, format!("{}u", v >> 31))
    }
}

impl<'a> Tr<'a> {
    /// One Thumb instruction, or a whole IT block: (size, condition, C, flow).
    pub(super) fn step_thumb(&self, a: u32) -> Result<(u32, u32, String, Flow), String> {
        if a % 2 != 0 {
            return Err(format!("odd Thumb address {a:#x}"));
        }
        let h = self.p.bin.u16(a) as u32;
        if h & 0xFF00 == 0xBF00 && h & 0xF != 0 {
            if (h >> 4) & 0xF == 0xF {
                return Err("bad IT".into());
            }
            let mut state = h & 0xFF;
            let mut body = String::from("{");
            let mut at = a + 2;
            let mut flow = Flow::default();
            loop {
                let cond = state >> 4;
                let last = state & 0xF == 8;
                if !self.in_fn(at) {
                    return Err("IT block leaves the function".into());
                }
                let (size, own, b, f) = self.thumb_one(at, true)?;
                if own != 0xE {
                    return Err("conditional branch inside an IT block".into());
                }
                if !last && (!f.fallthrough || !f.gotos.is_empty()) {
                    return Err("branch before the end of an IT block".into());
                }
                if cond == 0xE {
                    write!(body, " {b}").unwrap();
                } else {
                    write!(body, " if ({}) {b}", COND[cond as usize]).unwrap();
                }
                at += size;
                if last {
                    flow.gotos = f.gotos;
                    flow.fallthrough = f.fallthrough || cond != 0xE;
                    break;
                }
                state = (state & 0xE0) | ((state << 1) & 0x1F);
            }
            body += " }";
            return Ok((at - a, 0xE, body, flow));
        }
        self.thumb_one(a, false)
    }

    fn thumb_one(&self, a: u32, in_it: bool) -> Result<(u32, u32, String, Flow), String> {
        let h = self.p.bin.u16(a) as u32;
        let mut flow = Flow::default();
        if h >> 11 >= 0x1D {
            if !self.in_fn(a + 2) {
                return Err("32-bit Thumb instruction at the end of the function".into());
            }
            let x = (h << 16) | self.p.bin.u16(a + 2) as u32;
            let (cond, body) = self.t32(x, a, &mut flow)?;
            return Ok((4, cond, body, flow));
        }
        let (cond, body) = self.t16(h, a, in_it, &mut flow)?;
        Ok((2, cond, body, flow))
    }

    /// The ARM twin of a Thumb instruction (always-condition ARM word).
    fn arm(&self, x: u32, a: u32, flow: &mut Flow) -> Result<(u32, String), String> {
        Ok((0xE, self.insn(x, a, flow)?))
    }

    /// `b` to `t`: a goto inside the function, otherwise a tail call.
    fn t_branch(&self, t: u32, a: u32, flow: &mut Flow) -> Result<String, String> {
        if self.in_fn(t) {
            flow.gotos.push(t);
            let guard = if t <= a { "RC_LOOP_GUARD(); " } else { "" };
            return Ok(format!("{{ {guard}goto L_{t:x}; }}"));
        }
        let call = self.call(t, None)?;
        Ok(format!("{{ {call} return; }}"))
    }

    /// Literal address from pc (Align(pc, 4) ± imm), folded to a constant
    /// when it is in read-only __TEXT.
    fn t_literal(&self, a: u32, imm: u32, u: bool) -> u32 {
        let pc = (a + 4) & !3;
        if u {
            pc.wrapping_add(imm)
        } else {
            pc.wrapping_sub(imm)
        }
    }

    /// Single load/store. kind: 0 word, 1 byte, 2 halfword, 3 signed byte,
    /// 4 signed halfword. `off` is a C expression.
    #[allow(clippy::too_many_arguments)]
    fn t_ls(&self, l: bool, kind: u32, rt: u32, rn: u32, off: &str, p: bool, u: bool, w: bool, a: u32, flow: &mut Flow) -> Result<String, String> {
        let wb = w || !p;
        if rn == 15 && wb {
            return Err("pc writeback".into());
        }
        flow.fallthrough = true;
        if l && rt == 15 && kind != 0 {
            return Ok("/* preload */".into());
        }
        let sign = if u { '+' } else { '-' };
        let ea = if rn == 15 {
            let imm = u32::from_str_radix(off.trim_start_matches("0x").trim_end_matches('u'), 16).map_err(|_| "pc + register")?;
            let lit = self.t_literal(a, imm, u);
            if l && kind == 0 && rt != 15 && lit >= self.p.bin.text.addr && lit + 4 <= self.p.ro_end {
                if let Some(v) = self.p.bin.try_u32(lit) {
                    return Ok(format!("c->r[{rt}] = 0x{v:x}u; /* literal */"));
                }
            }
            format!("0x{lit:x}u")
        } else if p {
            format!("c->r[{rn}] {sign} {off}")
        } else {
            format!("c->r[{rn}]")
        };
        let wbs = if wb { format!(" c->r[{rn}] = c->r[{rn}] {sign} {off};") } else { String::new() };
        if l {
            if rt == 15 {
                // ldr pc, [sp], #4 = return (pop {pc})
                if rn == 13 && !p && u && off == "0x4u" {
                    flow.fallthrough = false;
                    return Ok("{ c->r[13] += 4; return; }".into());
                }
                return Err("load to pc".into());
            }
            let v = match kind {
                0 => "rc_rd32(c, ea)",
                1 => "rc_rd8(c, ea)",
                2 => "rc_rd16(c, ea)",
                3 => "(uint32_t)(int32_t)(int8_t)rc_rd8(c, ea)",
                _ => "(uint32_t)(int32_t)(int16_t)rc_rd16(c, ea)",
            };
            Ok(format!("{{ uint32_t ea = {ea}; uint32_t v = {v};{wbs} c->r[{rt}] = v; }}"))
        } else {
            if rt == 15 {
                return Err("store of pc".into());
            }
            let f = match kind {
                0 => "rc_wr32",
                1 => "rc_wr8",
                2 => "rc_wr16",
                _ => return Err("signed store".into()),
            };
            Ok(format!("{{ uint32_t ea = {ea}; {f}(c, ea, c->r[{rt}]);{wbs} }}"))
        }
    }

    /// Thumb-2 data processing with Thumb opcode `op`; `op2` are C statements
    /// that set `op2` and `sc`.
    #[allow(clippy::too_many_arguments)]
    fn t_dp(&self, op: u32, s: bool, rn: u32, rd: u32, op2: &str, a: u32, flow: &mut Flow) -> Result<(u32, String), String> {
        let test = |arm: u32, plain: u32| -> Result<u32, String> {
            if rd != 15 {
                Ok(plain)
            } else if s {
                Ok(arm)
            } else {
                Err("Thumb-2 write to pc".into())
            }
        };
        let aop = match op {
            0 => test(8, 0)?,
            1 => 14,
            2 => {
                if rn == 15 {
                    13
                } else {
                    12
                }
            }
            3 => {
                if rn == 15 {
                    15
                } else {
                    16
                }
            }
            4 => test(9, 1)?,
            8 => test(11, 4)?,
            10 => 5,
            11 => 6,
            13 => test(10, 2)?,
            14 => 3,
            _ => return Err("Thumb-2 data op (pkh)".into()),
        };
        if rd == 15 && !(8..=11).contains(&aop) {
            return Err("Thumb-2 write to pc".into());
        }
        Ok((0xE, self.dp(aop, s, rn, rd, op2, a, flow, false)?))
    }

    /// 16-bit Thumb: (condition, C).
    fn t16(&self, h: u32, a: u32, in_it: bool, flow: &mut Flow) -> Result<(u32, String), String> {
        let s = (!in_it) as u32;
        let rd = h & 7;
        let r3 = (h >> 3) & 7;
        // lsl / lsr / asr #imm
        if h >> 13 == 0 && (h >> 11) & 3 != 3 {
            let (op, imm5) = ((h >> 11) & 3, (h >> 6) & 31);
            return self.arm(0xE1A0_0000 | s << 20 | rd << 12 | imm5 << 7 | op << 5 | r3, a, flow);
        }
        // add / sub, register or imm3
        if h >> 11 == 3 {
            let aop = if bit(h, 9) { 2 } else { 4 };
            let x = 0xE000_0000 | (bit(h, 10) as u32) << 25 | aop << 21 | s << 20 | r3 << 16 | rd << 12 | ((h >> 6) & 7);
            return self.arm(x, a, flow);
        }
        // mov / cmp / add / sub #imm8
        if h >> 13 == 1 {
            let (r, imm) = ((h >> 8) & 7, h & 0xFF);
            let x = match (h >> 11) & 3 {
                0 => 0xE3A0_0000 | s << 20 | r << 12 | imm,
                1 => 0xE350_0000 | r << 16 | imm,
                2 => 0xE280_0000 | s << 20 | r << 16 | r << 12 | imm,
                _ => 0xE240_0000 | s << 20 | r << 16 | r << 12 | imm,
            };
            return self.arm(x, a, flow);
        }
        // data processing, registers
        if h >> 10 == 0x10 {
            let (op, m, d) = ((h >> 6) & 0xF, r3, rd);
            let x = match op {
                2 | 3 | 4 | 7 => {
                    let ty = match op {
                        2 => 0,
                        3 => 1,
                        4 => 2,
                        _ => 3,
                    };
                    0xE1A0_0010 | s << 20 | d << 12 | m << 8 | ty << 5 | d
                }
                8 => 0xE110_0000 | d << 16 | m,
                9 => 0xE260_0000 | s << 20 | m << 16 | d << 12,
                10 => 0xE150_0000 | d << 16 | m,
                11 => 0xE170_0000 | d << 16 | m,
                13 => 0xE000_0090 | s << 20 | d << 16 | d << 8 | m,
                15 => 0xE1E0_0000 | s << 20 | d << 12 | m,
                _ => {
                    let aop = match op {
                        0 => 0,
                        1 => 1,
                        5 => 5,
                        6 => 6,
                        12 => 12,
                        _ => 14,
                    };
                    0xE000_0000 | aop << 21 | s << 20 | d << 16 | d << 12 | m
                }
            };
            return self.arm(x, a, flow);
        }
        // high registers, bx / blx
        if h >> 10 == 0x11 {
            let d = ((h >> 7) & 1) << 3 | rd;
            let m = (h >> 3) & 0xF;
            return match (h >> 8) & 3 {
                0 => {
                    if d == 15 {
                        return Err("add pc (computed jump)".into());
                    }
                    self.arm(0xE080_0000 | d << 16 | d << 12 | m, a, flow)
                }
                1 => self.arm(0xE150_0000 | d << 16 | m, a, flow),
                2 => {
                    if d == 15 {
                        if m == 14 {
                            return Ok((0xE, "return;".into()));
                        }
                        // A branch table: rm = table + 4 * case, each entry a `b.w`
                        // to the case (Thumb ignores bit 0 of a mov to pc).
                        let (Some(targets), Some(&table)) = (self.p.switches.get(&a), self.p.mov_pc_tables.get(&a)) else {
                            return Err("mov pc (computed jump)".into());
                        };
                        let mut s = format!("switch ((c->r[{m}] & ~1u) - 0x{table:x}u) {{");
                        for (i, &t) in targets.iter().enumerate() {
                            let t = t & !1;
                            if !self.in_fn(t) {
                                return Err("switch case outside function".into());
                            }
                            flow.gotos.push(t);
                            write!(s, " case {}: goto L_{t:x};", i * 4).unwrap();
                        }
                        s += " }";
                        return Ok((0xE, s));
                    }
                    self.arm(0xE1A0_0000 | d << 12 | m, a, flow)
                }
                _ => {
                    if bit(h, 7) {
                        flow.fallthrough = true;
                        Ok((0xE, format!("{{ uint32_t t = c->r[{m}]; c->r[14] = 0x{:x}u; rc_call(c, t); RC_AFTER_CALL(); }}", (a + 2) | 1)))
                    } else if m == 14 {
                        Ok((0xE, "return;".into()))
                    } else {
                        Ok((0xE, format!("{{ rc_call(c, c->r[{m}]); return; }}")))
                    }
                }
            };
        }
        // ldr rt, [pc, #imm8*4]
        if h >> 11 == 9 {
            let off = format!("0x{:x}u", (h & 0xFF) * 4);
            return Ok((0xE, self.t_ls(true, 0, (h >> 8) & 7, 15, &off, true, true, false, a, flow)?));
        }
        // load / store, register offset
        if h >> 12 == 5 {
            let (l, kind) = [(false, 0), (false, 2), (false, 1), (true, 3), (true, 0), (true, 2), (true, 1), (true, 4)][((h >> 9) & 7) as usize];
            let off = format!("c->r[{}]", (h >> 6) & 7);
            return Ok((0xE, self.t_ls(l, kind, rd, r3, &off, true, true, false, a, flow)?));
        }
        // load / store, immediate offset
        if h >> 13 == 3 || h >> 12 == 8 {
            let imm5 = (h >> 6) & 31;
            let (kind, off) = if h >> 12 == 8 {
                (2, imm5 * 2)
            } else if bit(h, 12) {
                (1, imm5)
            } else {
                (0, imm5 * 4)
            };
            return Ok((0xE, self.t_ls(bit(h, 11), kind, rd, r3, &format!("0x{off:x}u"), true, true, false, a, flow)?));
        }
        // sp-relative load / store
        if h >> 12 == 9 {
            let off = format!("0x{:x}u", (h & 0xFF) * 4);
            return Ok((0xE, self.t_ls(bit(h, 11), 0, (h >> 8) & 7, 13, &off, true, true, false, a, flow)?));
        }
        // adr / add rd, sp, #imm
        if h >> 11 == 0x14 || h >> 11 == 0x15 {
            let r = (h >> 8) & 7;
            let imm = (h & 0xFF) * 4;
            flow.fallthrough = true;
            return Ok((
                0xE,
                if h >> 11 == 0x14 {
                    format!("c->r[{r}] = 0x{:x}u;", self.t_literal(a, imm, true))
                } else {
                    format!("c->r[{r}] = c->r[13] + 0x{imm:x}u;")
                },
            ));
        }
        // miscellaneous
        if h >> 12 == 0xB {
            if h & 0xFF00 == 0xB000 {
                flow.fallthrough = true;
                let op = if bit(h, 7) { '-' } else { '+' };
                return Ok((0xE, format!("c->r[13] {op}= 0x{:x}u;", (h & 0x7F) * 4)));
            }
            if h & 0xF500 == 0xB100 {
                // cbz / cbnz
                let t = a + 4 + ((((h >> 9) & 1) << 6) | (((h >> 3) & 31) << 1));
                if !self.in_fn(t) {
                    return Err("cbz out of the function".into());
                }
                flow.gotos.push(t);
                flow.fallthrough = true;
                let cmp = if bit(h, 11) { "!=" } else { "==" };
                return Ok((0xE, format!("if (c->r[{rd}] {cmp} 0) goto L_{t:x};")));
            }
            if h & 0xFF00 == 0xB200 {
                let aop = [3, 2, 7, 6][((h >> 6) & 3) as usize];
                return self.arm(0xE68F_0070 | aop << 20 | rd << 12 | r3, a, flow);
            }
            if h & 0xFE00 == 0xB400 {
                return self.arm(0xE92D_0000 | (h & 0xFF) | ((h >> 8) & 1) << 14, a, flow);
            }
            if h & 0xFE00 == 0xBC00 {
                return self.arm(0xE8BD_0000 | (h & 0xFF) | ((h >> 8) & 1) << 15, a, flow);
            }
            if h & 0xFF00 == 0xBA00 {
                let base = match (h >> 6) & 3 {
                    0 => 0xE6BF_0F30,
                    1 => 0xE6BF_0FB0,
                    3 => 0xE6FF_0FB0,
                    _ => return Err("hlt".into()),
                };
                return self.arm(base | rd << 12 | r3, a, flow);
            }
            if h & 0xFF0F == 0xBF00 {
                flow.fallthrough = true;
                return Ok((0xE, "/* hint */".into()));
            }
            return Err(format!("Thumb misc {h:04x}"));
        }
        // ldm / stm
        if h >> 12 == 0xC {
            let (rn, list) = ((h >> 8) & 7, h & 0xFF);
            let x = if bit(h, 11) {
                0xE890_0000 | ((list >> rn) & 1 ^ 1) << 21 | rn << 16 | list
            } else {
                0xE8A0_0000 | rn << 16 | list
            };
            return self.arm(x, a, flow);
        }
        // b<cond>, svc
        if h >> 12 == 0xD {
            let cond = (h >> 8) & 0xF;
            if cond >= 0xE {
                return Err(if cond == 0xF { "svc".into() } else { "udf".into() });
            }
            let t = a.wrapping_add(4).wrapping_add(sx((h & 0xFF) << 1, 9));
            return Ok((cond, self.t_branch(t, a, flow)?));
        }
        if h >> 11 == 0x1C {
            let t = a.wrapping_add(4).wrapping_add(sx((h & 0x7FF) << 1, 12));
            return Ok((0xE, self.t_branch(t, a, flow)?));
        }
        Err(format!("Thumb insn {h:04x}"))
    }

    /// 32-bit Thumb-2: (condition, C). `x` = first halfword << 16 | second.
    fn t32(&self, x: u32, a: u32, flow: &mut Flow) -> Result<(u32, String), String> {
        let (h1, h2) = (x >> 16, x & 0xFFFF);
        let rn = h1 & 0xF;
        let rd = (h2 >> 8) & 0xF;
        let rm = h2 & 0xF;
        let imm12 = ((h1 >> 10) & 1) << 11 | ((h2 >> 12) & 7) << 8 | (h2 & 0xFF);

        // Branches and miscellaneous control.
        if x & 0xF800_8000 == 0xF000_8000 {
            let s_ = (h1 >> 10) & 1;
            let (j1, j2) = ((h2 >> 13) & 1, (h2 >> 11) & 1);
            let (i1, i2) = (!(j1 ^ s_) & 1, !(j2 ^ s_) & 1);
            let long = s_ << 24 | i1 << 23 | i2 << 22 | (h1 & 0x3FF) << 12;
            return match (h2 >> 12) & 5 {
                0 => {
                    let cond = (h1 >> 6) & 0xF;
                    if cond < 0xE {
                        let imm = s_ << 20 | j2 << 19 | j1 << 18 | (h1 & 0x3F) << 12 | (h2 & 0x7FF) << 1;
                        let t = a.wrapping_add(4).wrapping_add(sx(imm, 21));
                        return Ok((cond, self.t_branch(t, a, flow)?));
                    }
                    flow.fallthrough = true;
                    if h1 == 0xF3BF && h2 & 0xFF00 == 0x8F00 {
                        Ok((0xE, "/* barrier */".into()))
                    } else if h1 == 0xF3AF && h2 & 0xFF00 == 0x8000 {
                        Ok((0xE, "/* hint */".into()))
                    } else if h1 == 0xF3EF && h2 & 0xF0FF == 0x8000 {
                        Ok((0xE, format!("c->r[{rd}] = (c->n << 31) | (c->z << 30) | (c->c << 29) | (c->v << 28);")))
                    } else if h1 & 0xFFF0 == 0xF380 && h2 == 0x8800 {
                        Ok((0xE, format!("{{ uint32_t v = c->r[{rn}]; c->n = v >> 31; c->z = (v >> 30) & 1; c->c = (v >> 29) & 1; c->v = (v >> 28) & 1; }}")))
                    } else {
                        Err(format!("Thumb-2 control {x:08x}"))
                    }
                }
                1 => {
                    let t = a.wrapping_add(4).wrapping_add(sx(long | (h2 & 0x7FF) << 1, 25));
                    Ok((0xE, self.t_branch(t, a, flow)?))
                }
                5 => {
                    let t = a.wrapping_add(4).wrapping_add(sx(long | (h2 & 0x7FF) << 1, 25));
                    flow.fallthrough = true;
                    Ok((0xE, self.call(t, Some((a + 4) | 1))?))
                }
                _ => {
                    if h2 & 1 != 0 {
                        return Err("bad blx".into());
                    }
                    let t = ((a + 4) & !3).wrapping_add(sx(long | (h2 & 0x7FE) << 1, 25));
                    flow.fallthrough = true;
                    Ok((0xE, self.call(t, Some((a + 4) | 1))?))
                }
            };
        }
        // Data processing, modified immediate.
        if x & 0xFA00_8000 == 0xF000_0000 {
            let (v, carry) = expand_imm(imm12);
            let op2 = format!("uint32_t op2 = 0x{v:x}u; uint32_t sc = {carry};");
            return self.t_dp((h1 >> 5) & 0xF, bit(h1, 4), rn, rd, &op2, a, flow);
        }
        // Data processing, plain binary immediate.
        if x & 0xFA00_8000 == 0xF200_0000 {
            let lsb = ((h2 >> 12) & 7) << 2 | (h2 >> 6) & 3;
            flow.fallthrough = true;
            return match (h1 >> 4) & 0x1F {
                0 | 0xA => {
                    let sub = (h1 >> 4) & 0x1F == 0xA;
                    if rn == 15 {
                        Ok((0xE, format!("c->r[{rd}] = 0x{:x}u;", self.t_literal(a, imm12, !sub))))
                    } else {
                        Ok((0xE, format!("c->r[{rd}] = c->r[{rn}] {} 0x{imm12:x}u;", if sub { '-' } else { '+' })))
                    }
                }
                4 => Ok((0xE, format!("c->r[{rd}] = 0x{:x}u;", (h1 & 0xF) << 12 | imm12))),
                0xC => Ok((0xE, format!("c->r[{rd}] = (c->r[{rd}] & 0xFFFFu) | 0x{:x}u;", ((h1 & 0xF) << 12 | imm12) << 16))),
                0x14 => self.arm(0xE7A0_0050 | (h2 & 0x1F) << 16 | rd << 12 | lsb << 7 | rn, a, flow),
                0x1C => self.arm(0xE7E0_0050 | (h2 & 0x1F) << 16 | rd << 12 | lsb << 7 | rn, a, flow),
                0x16 => self.arm(0xE7C0_0010 | (h2 & 0x1F) << 16 | rd << 12 | lsb << 7 | rn, a, flow),
                _ => Err("Thumb-2 saturate".into()),
            };
        }
        // Load / store multiple.
        if x & 0xFE40_0000 == 0xE800_0000 {
            let base = match (h1 >> 7) & 3 {
                1 => 0xE880_0000,
                2 => 0xE900_0000,
                _ => return Err("srs / rfe".into()),
            };
            return self.arm(base | (h1 & 0x30) << 16 | rn << 16 | h2, a, flow);
        }
        // Load / store dual, exclusive, table branch.
        if x & 0xFE40_0000 == 0xE840_0000 {
            let (p, u, w, l) = (bit(h1, 8), bit(h1, 7), bit(h1, 5), bit(h1, 4));
            let rt = (h2 >> 12) & 0xF;
            flow.fallthrough = true;
            if !p && !w {
                if h1 & 0xFFF0 == 0xE850 {
                    return Ok((0xE, format!("c->r[{rt}] = rc_rd32(c, c->r[{rn}] + 0x{:x}u);", (h2 & 0xFF) * 4)));
                }
                if h1 & 0xFFF0 == 0xE840 {
                    return Ok((0xE, format!("{{ rc_wr32(c, c->r[{rn}] + 0x{:x}u, c->r[{rt}]); c->r[{rd}] = 0; }}", (h2 & 0xFF) * 4)));
                }
                if h1 & 0xFFF0 == 0xE8D0 && h2 & 0xFFE0 == 0xF000 {
                    if rn != 15 {
                        return Err("table branch off a register".into());
                    }
                    let Some(targets) = self.p.switches.get(&a) else { return Err("table branch without a table".into()) };
                    let mut s = format!("switch (c->r[{rm}]) {{");
                    for (i, &t) in targets.iter().enumerate() {
                        let t = t & !1;
                        if !self.in_fn(t) {
                            return Err("switch case outside function".into());
                        }
                        flow.gotos.push(t);
                        write!(s, " case {i}: goto L_{t:x};").unwrap();
                    }
                    s += " }";
                    return Ok((0xE, s));
                }
                return Err("Thumb-2 exclusive (byte / half / dual)".into());
            }
            let rt2 = rd;
            let imm = (h2 & 0xFF) * 4;
            let sign = if u { '+' } else { '-' };
            let wb = w || !p;
            if rn == 15 && (wb || !l) {
                return Err("pc-relative dual store / writeback".into());
            }
            if rt == 15 || rt2 == 15 {
                return Err("pc in dual access".into());
            }
            let base = if rn == 15 { format!("0x{:x}u", (a + 4) & !3) } else { format!("c->r[{rn}]") };
            let ea = if p { format!("{base} {sign} 0x{imm:x}u") } else { base };
            let wbs = if wb { format!(" c->r[{rn}] = c->r[{rn}] {sign} 0x{imm:x}u;") } else { String::new() };
            return Ok((
                0xE,
                if l {
                    format!("{{ uint32_t ea = {ea}; uint32_t v0 = rc_rd32(c, ea), v1 = rc_rd32(c, ea + 4);{wbs} c->r[{rt}] = v0; c->r[{rt2}] = v1; }}")
                } else {
                    format!("{{ uint32_t ea = {ea}; rc_wr32(c, ea, c->r[{rt}]); rc_wr32(c, ea + 4, c->r[{rt2}]);{wbs} }}")
                },
            ));
        }
        // Data processing, shifted register.
        if x & 0xFE00_0000 == 0xEA00_0000 {
            if rm == 15 {
                return Err("pc as shifted operand".into());
            }
            let n = ((h2 >> 12) & 7) << 2 | (h2 >> 6) & 3;
            let op2 = self.operand2(0xE000_0000 | n << 7 | ((h2 >> 4) & 3) << 5 | rm, a)?;
            return self.t_dp((h1 >> 5) & 0xF, bit(h1, 4), rn, rd, &op2, a, flow);
        }
        // Coprocessor: VFP (same encoding as ARM), NEON (moved opcode bits).
        if x & 0xEC00_0000 == 0xEC00_0000 {
            if (x >> 24) & 0xEF == 0xEF {
                return self.arm(0xF200_0000 | ((x >> 28) & 1) << 24 | (x & 0x00FF_FFFF), a, flow);
            }
            if !bit(x, 28) && matches!((h2 >> 8) & 0xF, 10 | 11) {
                return self.arm(x, a, flow);
            }
            return Err(format!("coprocessor {x:08x}"));
        }
        // Load / store single.
        if x & 0xFE00_0000 == 0xF800_0000 {
            let l = bit(h1, 4);
            if x >> 24 == 0xF9 && !l {
                return self.arm(0xF400_0000 | (x & 0x00FF_FFFF), a, flow);
            }
            let kind = match ((h1 >> 5) & 3, bit(h1, 8)) {
                (2, false) => 0,
                (0, false) => 1,
                (1, false) => 2,
                (0, true) => 3,
                (1, true) => 4,
                _ => return Err(format!("Thumb-2 load/store {x:08x}")),
            };
            let rt = (h2 >> 12) & 0xF;
            if rn == 15 {
                let off = format!("0x{:x}u", h2 & 0xFFF);
                return Ok((0xE, self.t_ls(l, kind, rt, 15, &off, true, bit(h1, 7), false, a, flow)?));
            }
            if bit(h1, 7) {
                let off = format!("0x{:x}u", h2 & 0xFFF);
                return Ok((0xE, self.t_ls(l, kind, rt, rn, &off, true, true, false, a, flow)?));
            }
            if bit(h2, 11) {
                let (p, u, w) = (bit(h2, 10), bit(h2, 9), bit(h2, 8));
                if !p && !w {
                    return Err("bad load/store mode".into());
                }
                let off = format!("0x{:x}u", h2 & 0xFF);
                return Ok((0xE, self.t_ls(l, kind, rt, rn, &off, p, u, w, a, flow)?));
            }
            if (h2 >> 6) & 0x3F == 0 {
                if rm == 15 || rm == 13 {
                    return Err("sp / pc as index".into());
                }
                let off = format!("(c->r[{rm}] << {})", (h2 >> 4) & 3);
                return Ok((0xE, self.t_ls(l, kind, rt, rn, &off, true, true, false, a, flow)?));
            }
            return Err(format!("Thumb-2 load/store {x:08x}"));
        }
        // Data processing, register.
        if x & 0xFF80_F0F0 == 0xFA00_F000 {
            return self.arm(0xE1A0_0010 | (bit(h1, 4) as u32) << 20 | rd << 12 | rm << 8 | ((h1 >> 5) & 3) << 5 | rn, a, flow);
        }
        if x & 0xFF80_F0C0 == 0xFA00_F080 {
            let aop = match (h1 >> 4) & 7 {
                0 => 3,
                1 => 7,
                2 => 0, // sxtab16 / sxtb16
                3 => 4, // uxtab16 / uxtb16
                4 => 2,
                5 => 6,
                _ => return Err("packed (16-bit lane) extend".into()),
            };
            return self.arm(0xE680_0070 | aop << 20 | rn << 16 | rd << 12 | ((h2 >> 4) & 3) << 10 | rm, a, flow);
        }
        if x & 0xFFC0_F0C0 == 0xFA80_F080 {
            let base = match ((h1 >> 4) & 3, (h2 >> 4) & 3) {
                (1, 0) => 0xE6BF_0F30,
                (1, 1) => 0xE6BF_0FB0,
                (1, 3) => 0xE6FF_0FB0,
                (3, 0) => 0xE16F_0F10,
                _ => return Err(format!("Thumb-2 misc {x:08x}")),
            };
            return self.arm(base | rd << 12 | rm, a, flow);
        }
        // Multiply.
        if x & 0xFFF0_00C0 == 0xFB00_0000 {
            let ra = (h2 >> 12) & 0xF;
            let base = match ((h2 >> 4) & 3, ra) {
                (0, 15) => 0xE000_0090,
                (0, _) => 0xE020_0090 | ra << 12,
                (1, _) => 0xE060_0090 | ra << 12,
                _ => return Err("Thumb-2 multiply".into()),
            };
            return self.arm(base | rd << 16 | rm << 8 | rn, a, flow);
        }
        // smulxy / smlaxy (N = bit 5, M = bit 4 here; bits 5 / 6 in ARM).
        if x & 0xFFF0_00C0 == 0xFB10_0000 {
            let ra = (h2 >> 12) & 0xF;
            let base = if ra == 15 { 0xE160_0080 } else { 0xE100_0080 | ra << 12 };
            let (n, m) = ((h2 >> 5) & 1, (h2 >> 4) & 1);
            return self.arm(base | rd << 16 | rm << 8 | m << 6 | n << 5 | rn, a, flow);
        }
        // smmul / smmla (op1 5), smmls (op1 6); R = bit 4 here, bit 5 in ARM.
        if x & 0xFF80_00E0 == 0xFB00_0000 && matches!((h1 >> 4) & 7, 5 | 6) {
            let ra = (h2 >> 12) & 0xF;
            let r = (h2 >> 4) & 1;
            let base = match ((h1 >> 4) & 7, ra) {
                (5, 15) => 0xE750_F010,
                (5, _) => 0xE750_0010 | ra << 12,
                (6, _) => 0xE750_00D0 | ra << 12,
                _ => return Err(format!("Thumb-2 insn {x:08x}")),
            };
            return self.arm(base | rd << 16 | rm << 8 | r << 5 | rn, a, flow);
        }
        if x & 0xFF80_00F0 == 0xFB80_0000 {
            let op1 = (h1 >> 4) & 7;
            if op1 & 1 != 0 {
                return Err("Thumb-2 long multiply".into());
            }
            let (signed, acc) = (op1 == 0 || op1 == 4, op1 >= 4);
            let lo = (h2 >> 12) & 0xF;
            return self.arm(0xE080_0090 | (signed as u32) << 22 | (acc as u32) << 21 | rd << 16 | lo << 12 | rm << 8 | rn, a, flow);
        }
        Err(format!("Thumb-2 insn {x:08x}"))
    }
}
