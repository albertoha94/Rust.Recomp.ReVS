//! Objective-C 2 metadata (32-bit): every class in __objc_classlist, its
//! instance methods and (through the metaclass) its class methods. Each method
//! gives a function start with a name and an ARM/Thumb mode (low bit of imp).

use serde::Serialize;

use crate::macho::Binary;

#[derive(Serialize)]
pub struct Method {
    pub name: String, // "-[Class sel]" / "+[Class sel]"
    #[serde(serialize_with = "crate::hex")]
    pub imp: u32,
    pub thumb: bool,
}

pub struct Classes {
    pub class_count: usize,
    pub methods: Vec<Method>,
}

pub fn read(bin: &Binary) -> Classes {
    let mut out = Classes { class_count: 0, methods: Vec::new() };
    let Some(list) = bin.section("__DATA", "__objc_classlist") else { return out };
    for i in 0..list.size / 4 {
        let Some(cls) = bin.try_u32(list.addr + i * 4) else { continue };
        out.class_count += 1;
        read_class(bin, cls, '-', &mut out.methods);
        if let Some(meta) = bin.try_u32(cls) {
            read_class(bin, meta, '+', &mut out.methods);
        }
    }
    out.methods.sort_by_key(|m| m.imp);
    out
}

/// class_t: isa, superclass, cache, vtable, data (class_ro_t*, low bits = flags).
/// class_ro_t: flags, instanceStart, instanceSize, ivarLayout, name (+16), baseMethods (+20).
/// method_list_t: entsize_and_flags, count, then { name, types, imp } entries.
fn read_class(bin: &Binary, cls: u32, kind: char, out: &mut Vec<Method>) -> Option<()> {
    let ro = bin.try_u32(cls + 16)? & !3;
    let class_name = bin.cstr(bin.try_u32(ro + 16)?)?;
    let list = bin.try_u32(ro + 20)?;
    if list == 0 {
        return Some(());
    }
    let entsize = bin.try_u32(list)? & !3;
    let count = bin.try_u32(list + 4)?;
    for k in 0..count {
        let e = list + 8 + k * entsize;
        let sel = bin.cstr(bin.try_u32(e)?)?;
        let imp = bin.try_u32(e + 8)?;
        out.push(Method { name: format!("{kind}[{class_name} {sel}]"), imp: imp & !1, thumb: imp & 1 != 0 });
    }
    Some(())
}
