//! Code pointers stored as data: C++ vtables (named `__ZTV...` symbols) and
//! any other word in a data section (or loaded from a literal pool) whose value
//! points into __text: callbacks, function tables, static initialisers.

use serde::Serialize;

use crate::macho::Binary;

#[derive(Serialize)]
pub struct Vtable {
    pub class: String,
    #[serde(serialize_with = "crate::hex")]
    pub addr: u32,
    /// One entry per virtual slot, in order. Several groups when the class has
    /// more than one base (secondary vtables follow the primary one).
    pub groups: Vec<VtableGroup>,
}

#[derive(Serialize)]
pub struct VtableGroup {
    pub offset_to_top: i32,
    #[serde(serialize_with = "crate::hex")]
    pub slots_at: u32,
    /// Slot values as stored: function address (odd = Thumb), or 0 for an
    /// entry the loader fills in (e.g. `__cxa_pure_virtual`).
    #[serde(serialize_with = "crate::hex_vec")]
    pub slots: Vec<u32>,
}

/// A data word that points into __text.
pub struct CodePtr {
    pub at: u32,       // where the pointer is stored (0 = literal pool word)
    pub target: u32,   // Thumb bit cleared
    pub thumb: bool,
    /// "vtable", "data" (a data section), "literal" (a literal-pool word used
    /// as is) or "pic" (pc-relative: literal + `add rX, pc`).
    pub from: &'static str,
}

pub fn in_text(bin: &Binary, v: u32) -> bool {
    let t = v & !1;
    t >= bin.text.addr && t < bin.text.addr + bin.text.size
}

/// Reads every `__ZTV` vtable. Layout per group: offset_to_top, typeinfo
/// pointer, then slots until the next group header or the next symbol.
pub fn vtables(bin: &Binary) -> Vec<Vtable> {
    let syms = &bin.data_symbols;
    let mut out = Vec::new();
    for (i, s) in syms.iter().enumerate() {
        if !s.name.starts_with("__ZTV") {
            continue;
        }
        let end = syms[i + 1..].iter().map(|n| n.addr).find(|&a| a > s.addr).unwrap_or(s.addr + 0x400);
        let mut groups = Vec::new();
        let mut a = s.addr;
        while a + 8 <= end {
            let (Some(top), Some(_ti)) = (bin.try_u32(a), bin.try_u32(a + 4)) else { break };
            let mut slots = Vec::new();
            let mut p = a + 8;
            while p + 4 <= end {
                let Some(v) = bin.try_u32(p) else { break };
                if v != 0 && !in_text(bin, v) {
                    break; // the next group's offset_to_top
                }
                // A 0 followed by a non-code word is the next group's header.
                if v == 0 && p + 8 <= end && bin.try_u32(p + 4).is_some_and(|n| n != 0 && !in_text(bin, n)) {
                    break;
                }
                slots.push(v);
                p += 4;
            }
            groups.push(VtableGroup { offset_to_top: top as i32, slots_at: a + 8, slots });
            if p == a + 8 {
                break;
            }
            a = p;
        }
        let class = crate::demangle(&s.name)
            .map(|d| {
                // cpp_demangle writes "{vtable(X)}"; c++filt writes "vtable for X"
                let d = d.strip_prefix("vtable for ").unwrap_or(&d);
                d.strip_prefix("{vtable(").and_then(|x| x.strip_suffix(")}")).unwrap_or(d).to_string()
            })
            .unwrap_or_else(|| s.name.clone());
        out.push(Vtable { class, addr: s.addr, groups });
    }
    out
}

/// Every aligned word in the data sections that points into __text.
pub fn data_code_ptrs(bin: &Binary) -> Vec<CodePtr> {
    let mut out = Vec::new();
    for sec in &bin.sections {
        let zerofill = matches!(sec.flags & 0xff, 0x1 | 0xc);
        let code_or_strings = sec.flags & 0x8000_0000 != 0 || matches!(sec.flags & 0xff, 0x2 | 0x6 | 0x7 | 0x8);
        if zerofill || code_or_strings || sec.name.starts_with("__objc_methname") {
            continue;
        }
        let mut a = (sec.addr + 3) & !3;
        while a + 4 <= sec.addr + sec.size {
            if let Some(v) = bin.try_u32(a) {
                if in_text(bin, v) {
                    out.push(CodePtr { at: a, target: v & !1, thumb: v & 1 != 0, from: "data" });
                }
            }
            a += 4;
        }
    }
    out
}

/// What is already known at the pointer's target, to decide whether it is a
/// function start.
pub struct PtrContext {
    /// The target is already a known function start.
    pub known_start: bool,
    /// The function the target falls inside (start, Thumb), when not a start.
    pub inside: Option<(u32, bool)>,
    /// The target is also a decoded switch-case target (an address inside a
    /// function, used by a jump table).
    pub is_case_target: bool,
    /// The instruction at the target is `push {..., lr}` (a typical entry).
    pub looks_like_entry: bool,
    /// The instruction before the target is a return, `b` or padding.
    pub prev_ends: bool,
    /// An ARM leaf start: the code before ends (maybe followed by its literal
    /// pool) and the target is an unconditional instruction (code::leaf_boundary).
    pub leaf_boundary: bool,
    /// The target is also stored by PIC code (an SjLj landing pad).
    pub is_pic_target: bool,
}

/// Should this code pointer (whose target is not a known start yet) create a
/// new function start? Measured on the original 1.10.10 binary:
/// - "data" mid-function hits: ~530, nearly all integers or packed data that
///   happen to fall in the __text range (0x656c = "le", 0x200077...); 2 look
///   like an entry.
/// - "pic" mid-function hits: ~1 430, nearly all SjLj exception landing pads
///   (1 434 functions call `_Unwind_SjLj_Register` and store the address of
///   their own cleanup code). These are jump targets inside a function, not
///   starts.
/// - "literal" words: ~40 000 integer constants in the __text range.
/// - Leaf functions without `push` (tables of float helpers near 0x382300):
///   24 ARM targets, all real instructions after a return / `b` (and maybe a
///   literal pool); one of them stopped the first match at 0x2e7f20.
/// - Literal words: only with code::literal_leaf_boundary (a return before,
///   not a branch target, not a landing pad): 4 on 1.10.10, incl. the qsort
///   comparators that stopped the first match at 0x2e0b68.
/// A wrong "yes" splits a real function in two; a wrong "no" leaves a function
/// merged into the one before it (its size is too big, and it has no entry).
pub fn accept_code_pointer(p: &CodePtr, ctx: &PtrContext) -> bool {
    // Only pointers stored in data sections, and only when the code there has
    // the shape of a function boundary: the previous instruction ends a
    // function and this one is an entry `push`. Literal words are mostly
    // integers and PIC hits are landing pads, so they never create starts.
    let shape = match p.from {
        "data" => (ctx.looks_like_entry && ctx.prev_ends) || ctx.leaf_boundary,
        "literal" => ctx.leaf_boundary && !ctx.is_pic_target,
        _ => false,
    };
    shape
        && !ctx.is_case_target
        && ctx.inside.map_or(true, |(_, thumb)| thumb == p.thumb)
}

/// A C++ class from its typeinfo (`__ZTI...`): name, kind and direct bases.
#[derive(Serialize)]
pub struct Class {
    pub name: String,
    #[serde(serialize_with = "crate::hex")]
    pub typeinfo: u32,
    /// "root" (no base), "single" (one public base) or "multiple".
    pub kind: &'static str,
    pub bases: Vec<String>,
    #[serde(serialize_with = "crate::hex_opt")]
    pub vtable: Option<u32>,
}

fn typeinfo_name(sym: &str) -> String {
    crate::demangle(sym)
        .map(|d| {
            let d = d.strip_prefix("typeinfo for ").unwrap_or(&d);
            d.strip_prefix("{typeinfo(").and_then(|x| x.strip_suffix(")}")).unwrap_or(d).to_string()
        })
        .unwrap_or_else(|| sym.to_string())
}

/// Reads every class typeinfo. Its first word is filled in by the loader with
/// the address of `__class_type_info` / `__si_class_type_info` /
/// `__vmi_class_type_info`'s vtable, which gives the kind. Bases are other
/// typeinfo pointers: local symbols, or bound OS symbols (e.g. std::exception).
pub fn classes(bin: &Binary, vtables: &[Vtable]) -> Vec<Class> {
    let local: std::collections::HashMap<u32, &str> =
        bin.data_symbols.iter().map(|s| (s.addr, s.name.as_str())).collect();
    let vt_by_class: std::collections::HashMap<&str, u32> =
        vtables.iter().map(|v| (v.class.as_str(), v.addr)).collect();
    let base_at = |slot: u32| -> Option<String> {
        if let Some(n) = bin.binds.get(&slot) {
            return Some(typeinfo_name(n));
        }
        let p = bin.try_u32(slot)?;
        local.get(&p).map(|n| typeinfo_name(n))
    };
    let mut out = Vec::new();
    for s in bin.data_symbols.iter().filter(|s| s.name.starts_with("__ZTI")) {
        let Some(meta) = bin.binds.get(&s.addr) else { continue };
        let (kind, bases) = if meta.contains("__si_class_type_info") {
            ("single", base_at(s.addr + 8).into_iter().collect())
        } else if meta.contains("__vmi_class_type_info") {
            let n = bin.try_u32(s.addr + 12).unwrap_or(0).min(64);
            ("multiple", (0..n).filter_map(|i| base_at(s.addr + 16 + i * 8)).collect())
        } else if meta.contains("__class_type_info") {
            ("root", Vec::new())
        } else {
            continue; // pointer / fundamental typeinfo
        };
        let name = typeinfo_name(&s.name);
        let vtable = vt_by_class.get(name.as_str()).copied();
        out.push(Class { name, typeinfo: s.addr, kind, bases, vtable });
    }
    out
}
