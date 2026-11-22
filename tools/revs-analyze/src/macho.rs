//! Minimal Mach-O view of the game's executable: first slice of the fat
//! binary (the one every address in the project notes refers to), __text,
//! its symbols and the imported OS symbols.
//!
//! Also reads the libraries the game links (touchHLE's open-source ARM
//! dylibs): `parse_lib` takes the armv7 slice and can move it to a chosen load
//! address, applying the dylib's rebase table first, so every later step sees
//! the library at its final address.

use goblin::mach::load_command::CommandVariant;
use goblin::mach::MachO;
use serde::Serialize;

pub struct Section {
    pub addr: u32,
    pub size: u32,
}

/// A section of the executable, for the analysis passes.
pub struct SectionInfo {
    pub seg: String,
    pub name: String,
    pub addr: u32,
    pub size: u32,
    pub flags: u32,
}

pub struct Symbol {
    pub addr: u32,
    pub name: String,
    pub thumb: bool,
}

/// One imported OS symbol and the place the game reaches it through.
#[derive(Serialize)]
pub struct Import {
    /// Stub or pointer slot address in the game.
    pub addr: String,
    pub kind: &'static str,
    pub name: String,
}

pub struct Binary {
    pub slice_count: u32,
    pub data: Vec<u8>,
    segments: Vec<(u32, u32, u32, u32)>, // vmaddr, vmsize, fileoff, filesize
    pub sections: Vec<SectionInfo>,
    pub text: Section,
    pub text_symbols: Vec<Symbol>,
    /// Defined symbols outside __text (vtables, typeinfo, globals).
    pub data_symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    /// Data words the loader fills with an OS symbol's address (bind info):
    /// address -> symbol name (e.g. typeinfo vtables, `__cxa_pure_virtual`).
    pub binds: std::collections::HashMap<u32, String>,
    /// The non-zero bind addends: the word gets symbol address + addend
    /// (C++ typeinfo objects point 8 bytes into a libstdc++ vtable).
    pub bind_addends: std::collections::HashMap<u32, i32>,
    /// Exported symbols (libraries): name -> address.
    pub exports: Vec<(String, u32)>,
}

const N_STAB: u8 = 0xe0;
const N_TYPE: u8 = 0x0e;
const N_SECT: u8 = 0x0e;
const N_ARM_THUMB_DEF: u16 = 0x0008;
const S_NON_LAZY_SYMBOL_POINTERS: u32 = 0x6;
const S_LAZY_SYMBOL_POINTERS: u32 = 0x7;
const S_SYMBOL_STUBS: u32 = 0x8;
const INDIRECT_SYMBOL_LOCAL: u32 = 0x8000_0000;
const INDIRECT_SYMBOL_ABS: u32 = 0x4000_0000;

impl Binary {
    pub fn parse_first_slice(file: &[u8]) -> Result<Binary, String> {
        let be = |o: usize| u32::from_be_bytes(file[o..o + 4].try_into().unwrap());
        let (slice, slice_count) = if be(0) == 0xCAFE_BABE {
            let n = be(4);
            let (off, size) = (be(16) as usize, be(20) as usize); // first fat_arch
            (file[off..off + size].to_vec(), n)
        } else {
            (file.to_vec(), 1)
        };
        Self::parse_slice(slice, slice_count)
    }

    /// A library: the armv7 slice (CPU subtype 9) of a fat file, or the file
    /// itself. With `base`, the image is moved so its __TEXT starts there.
    pub fn parse_lib(file: &[u8], base: Option<u32>) -> Result<Binary, String> {
        let be = |o: usize| u32::from_be_bytes(file[o..o + 4].try_into().unwrap());
        let (slice, slice_count) = if be(0) == 0xCAFE_BABE {
            let n = be(4);
            let pick = (0..n as usize)
                .map(|i| (be(8 + 20 * i + 4), be(8 + 20 * i + 8) as usize, be(8 + 20 * i + 12) as usize))
                .find(|a| a.0 == 9)
                .ok_or("no armv7 slice")?;
            (file[pick.1..pick.1 + pick.2].to_vec(), n)
        } else {
            (file.to_vec(), 1)
        };
        let mut b = Self::parse_slice(slice, slice_count)?;
        if let Some(base) = base {
            b.move_to(base)?;
        }
        Ok(b)
    }

    /// Start of the __TEXT segment (the image's load address).
    pub fn base(&self) -> u32 {
        self.segments.iter().find(|s| s.3 > 0).map_or(0, |s| s.0)
    }

    /// Moves the image to `base`: applies the dyld rebase table to the file
    /// bytes (pointer words += slide) and shifts every address.
    fn move_to(&mut self, base: u32) -> Result<(), String> {
        let slide = base.wrapping_sub(self.base());
        if slide == 0 {
            return Ok(());
        }
        for (seg, off) in rebase_locations(&self.data)? {
            let &(_, _, fo, _) = self.segments.get(seg).ok_or("rebase: bad segment")?;
            let o = (fo + off) as usize;
            let w = u32::from_le_bytes(self.data[o..o + 4].try_into().unwrap());
            self.data[o..o + 4].copy_from_slice(&w.wrapping_add(slide).to_le_bytes());
        }
        for s in &mut self.segments {
            if s.1 > 0 {
                s.0 = s.0.wrapping_add(slide);
            }
        }
        for s in &mut self.sections {
            s.addr = s.addr.wrapping_add(slide);
        }
        self.text.addr = self.text.addr.wrapping_add(slide);
        for s in self.text_symbols.iter_mut().chain(self.data_symbols.iter_mut()) {
            s.addr = s.addr.wrapping_add(slide);
        }
        for i in &mut self.imports {
            let a = u32::from_str_radix(i.addr.trim_start_matches("0x"), 16).unwrap_or(0);
            i.addr = format!("{:#x}", a.wrapping_add(slide));
        }
        self.binds = std::mem::take(&mut self.binds).into_iter().map(|(a, n)| (a.wrapping_add(slide), n)).collect();
        self.bind_addends =
            std::mem::take(&mut self.bind_addends).into_iter().map(|(a, n)| (a.wrapping_add(slide), n)).collect();
        for e in &mut self.exports {
            e.1 = e.1.wrapping_add(slide);
        }
        Ok(())
    }

    fn parse_slice(slice: Vec<u8>, slice_count: u32) -> Result<Binary, String> {
        let m = MachO::parse(&slice, 0).map_err(|e| e.to_string())?;

        let (segments, sections) = raw_segments(&slice)?;
        let text_idx = sections
            .iter()
            .position(|s| s.sectname == "__text" && s.segname == "__TEXT")
            .ok_or("no __TEXT,__text")?;
        let ts = &sections[text_idx];
        let text = Section { addr: ts.addr, size: ts.size };

        // Symbols defined in __text.
        let syms: Vec<(String, goblin::mach::symbols::Nlist)> = m
            .symbols
            .as_ref()
            .ok_or("no symbol table")?
            .iter()
            .filter_map(|r| r.ok())
            .map(|(n, l)| (n.to_string(), l))
            .collect();
        let mut text_symbols: Vec<Symbol> = syms
            .iter()
            .filter(|(_, l)| {
                l.n_type & N_STAB == 0 && l.n_type & N_TYPE == N_SECT && l.n_sect == text_idx + 1
            })
            .map(|(n, l)| Symbol {
                addr: l.n_value as u32 & !1,
                name: n.clone(),
                thumb: l.n_desc & N_ARM_THUMB_DEF != 0,
            })
            .collect();
        text_symbols.sort_by_key(|s| s.addr);
        let mut data_symbols: Vec<Symbol> = syms
            .iter()
            .filter(|(_, l)| {
                l.n_type & N_STAB == 0 && l.n_type & N_TYPE == N_SECT && l.n_sect != text_idx + 1
            })
            .map(|(n, l)| Symbol { addr: l.n_value as u32, name: n.clone(), thumb: false })
            .collect();
        data_symbols.sort_by_key(|s| s.addr);

        // Imports through the indirect symbol table.
        let mut imports = Vec::new();
        let dysym = m.load_commands.iter().find_map(|lc| match &lc.command {
            CommandVariant::Dysymtab(d) => Some((d.indirectsymoff as usize, d.nindirectsyms as usize)),
            _ => None,
        });
        if let Some((off, n)) = dysym {
            let indirect: Vec<u32> = (0..n)
                .map(|i| u32::from_le_bytes(slice[off + i * 4..off + i * 4 + 4].try_into().unwrap()))
                .collect();
            for s in &sections {
                let (kind, entry) = match s.flags & 0xff {
                    S_SYMBOL_STUBS => ("stub", s.reserved2.max(1)),
                    S_LAZY_SYMBOL_POINTERS => ("lazy_ptr", 4),
                    S_NON_LAZY_SYMBOL_POINTERS => ("ptr", 4),
                    _ => continue,
                };
                for k in 0..(s.size / entry) {
                    let Some(&idx) = indirect.get(s.reserved1 as usize + k as usize) else { break };
                    if idx & (INDIRECT_SYMBOL_LOCAL | INDIRECT_SYMBOL_ABS) != 0 {
                        continue;
                    }
                    if let Some((name, _)) = syms.get(idx as usize) {
                        imports.push(Import {
                            addr: format!("{:#x}", s.addr + k * entry),
                            kind,
                            name: name.clone(),
                        });
                    }
                }
            }
        }

        let all_imports = m.imports().unwrap_or_default();
        let binds = all_imports.iter().map(|i| (i.address as u32, i.name.to_string())).collect();
        let bind_addends =
            all_imports.iter().filter(|i| i.addend != 0).map(|i| (i.address as u32, i.addend as i32)).collect();
        let sections = sections
            .iter()
            .map(|s| SectionInfo {
                seg: s.segname.clone(),
                name: s.sectname.clone(),
                addr: s.addr,
                size: s.size,
                flags: s.flags,
            })
            .collect();
        let text_vm = segments.iter().find(|s| s.3 > 0).map_or(0, |s| s.0);
        let exports: Vec<(String, u32)> = m
            .exports()
            .map(|v| v.into_iter().map(|e| (e.name, text_vm.wrapping_add(e.offset as u32))).collect())
            .unwrap_or_default();
        // Older dylibs (e.g. touchHLE's SQLite) have no export trie: their exports
        // are the external defined symbols (N_EXT, N_SECT) of the symbol table.
        let exports = if exports.is_empty() {
            m.symbols()
                .flatten()
                .filter(|(_, n)| n.n_type & 0x01 != 0 && n.n_type & 0x0e == 0x0e)
                .map(|(name, n)| (name.to_string(), n.n_value as u32))
                .collect()
        } else {
            exports
        };
        Ok(Binary { slice_count, data: slice, segments, sections, text, text_symbols, data_symbols, imports, binds, bind_addends, exports })
    }

    /// (vmaddr, vmsize, fileoff, filesize) of every segment, in load order.
    pub fn segments(&self) -> &[(u32, u32, u32, u32)] {
        &self.segments
    }

    pub fn section(&self, seg: &str, name: &str) -> Option<&SectionInfo> {
        self.sections.iter().find(|s| s.seg == seg && s.name == name)
    }

    /// File bytes at a VM address, or None outside the file-backed part of a segment.
    pub fn bytes(&self, addr: u32, n: u32) -> Option<&[u8]> {
        let &(vm, _, fo, fsz) =
            self.segments.iter().find(|(vm, sz, _, _)| addr >= *vm && addr < vm.wrapping_add(*sz))?;
        let rel = addr - vm;
        if rel.checked_add(n)? > fsz {
            return None;
        }
        let o = (fo + rel) as usize;
        self.data.get(o..o + n as usize)
    }

    pub fn try_u32(&self, addr: u32) -> Option<u32> {
        self.bytes(addr, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }

    /// Little-endian word at a VM address (must be inside a segment).
    pub fn u32(&self, addr: u32) -> u32 {
        self.try_u32(addr).expect("address outside the file-backed segments")
    }

    pub fn u16(&self, addr: u32) -> u16 {
        let b = self.bytes(addr, 2).expect("address outside the file-backed segments");
        u16::from_le_bytes(b.try_into().unwrap())
    }

    /// NUL-terminated string at a VM address.
    pub fn cstr(&self, addr: u32) -> Option<String> {
        let mut out = Vec::new();
        let mut a = addr;
        loop {
            let c = *self.bytes(a, 1)?.first()?;
            if c == 0 || out.len() > 4096 {
                break;
            }
            out.push(c);
            a += 1;
        }
        Some(String::from_utf8_lossy(&out).into_owned())
    }
}

/// A section_32 header, with the fields goblin doesn't expose.
struct RawSection {
    sectname: String,
    segname: String,
    addr: u32,
    size: u32,
    flags: u32,
    reserved1: u32,
    reserved2: u32,
}

/// Reads LC_SEGMENT (32-bit) commands: (vmaddr, vmsize, fileoff, filesize) per segment
/// and every section header, in load order (= symbol table n_sect order).
fn raw_segments(b: &[u8]) -> Result<(Vec<(u32, u32, u32, u32)>, Vec<RawSection>), String> {
    let le = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let name = |o: usize| {
        let raw = &b[o..o + 16];
        let end = raw.iter().position(|&c| c == 0).unwrap_or(16);
        String::from_utf8_lossy(&raw[..end]).into_owned()
    };
    if le(0) != 0xFEED_FACE {
        return Err("not a 32-bit Mach-O slice".into());
    }
    let ncmds = le(16) as usize;
    let (mut segs, mut sects) = (Vec::new(), Vec::new());
    let mut o = 28;
    for _ in 0..ncmds {
        let (cmd, size) = (le(o), le(o + 4) as usize);
        if cmd == 0x1 {
            segs.push((le(o + 24), le(o + 28), le(o + 32), le(o + 36)));
            let nsects = le(o + 48) as usize;
            for i in 0..nsects {
                let s = o + 56 + i * 68;
                sects.push(RawSection {
                    sectname: name(s),
                    segname: name(s + 16),
                    addr: le(s + 32),
                    size: le(s + 36),
                    flags: le(s + 56),
                    reserved1: le(s + 60),
                    reserved2: le(s + 64),
                });
            }
        }
        o += size;
    }
    Ok((segs, sects))
}

/// (segment index, offset) of every word the dyld rebase table moves.
fn rebase_locations(b: &[u8]) -> Result<Vec<(usize, u32)>, String> {
    let le = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let ncmds = le(16) as usize;
    let mut o = 28;
    let mut info = None;
    for _ in 0..ncmds {
        let (cmd, size) = (le(o), le(o + 4) as usize);
        if cmd == 0x22 || cmd == 0x8000_0022 {
            info = Some((le(o + 8) as usize, le(o + 12) as usize));
        }
        o += size;
    }
    let Some((off, size)) = info else { return Err("no dyld info: the image can't be moved".into()) };
    let ops = &b[off..off + size];
    let mut i = 0;
    let uleb = |i: &mut usize| {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let x = ops[*i];
            *i += 1;
            v |= ((x & 0x7F) as u64) << shift;
            shift += 7;
            if x & 0x80 == 0 {
                return v as u32;
            }
        }
    };
    let (mut seg, mut addr) = (0usize, 0u32);
    let mut out = Vec::new();
    while i < ops.len() {
        let x = ops[i];
        i += 1;
        let imm = (x & 0x0F) as u32;
        match x & 0xF0 {
            0x00 => break,
            0x10 => {}
            0x20 => {
                seg = imm as usize;
                addr = uleb(&mut i);
            }
            0x30 => addr = addr.wrapping_add(uleb(&mut i)),
            0x40 => addr = addr.wrapping_add(imm * 4),
            0x50 => {
                for _ in 0..imm {
                    out.push((seg, addr));
                    addr += 4;
                }
            }
            0x60 => {
                for _ in 0..uleb(&mut i) {
                    out.push((seg, addr));
                    addr += 4;
                }
            }
            0x70 => {
                out.push((seg, addr));
                addr = addr.wrapping_add(uleb(&mut i) + 4);
            }
            0x80 => {
                let (n, skip) = (uleb(&mut i), uleb(&mut i));
                for _ in 0..n {
                    out.push((seg, addr));
                    addr = addr.wrapping_add(skip + 4);
                }
            }
            _ => return Err(format!("unknown rebase opcode {x:#x}")),
        }
    }
    Ok(out)
}
