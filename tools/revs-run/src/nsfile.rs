//! Foundation's file side (Phase 3.5): NSBundle, NSFileManager, NSData, the
//! NSString path and file methods, NSSearchPathForDirectoriesInDomains, and
//! the OS data constants the game reads (NSString keys, stdio FILE pointers).
//! Paths go through `fs.rs` (bundle from the .ipa, the port's own home).

use crate::fs::HOME;
use crate::objc::{self, new_string, string_of, Msg, NativeImp, Obj};
use crate::{Cpu, Host};

fn arg_str(h: &Host, c: &Cpu, m: &Msg, i: u32) -> Option<String> {
    string_of(h, m.arg(h, c, i))
}

fn yes(b: bool) -> u32 {
    b as u32
}

/// A new NSData (or NSMutableData) holding a copy of `bytes` in guest memory.
pub fn new_data(h: &mut Host, class: &str, bytes: &[u8]) -> u32 {
    let ptr = crate::os::alloc_zeroed(h, bytes.len().max(1) as u32);
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), h.at(ptr), bytes.len()) };
    crate::uikit::new_obj(h, class, Obj::Data { ptr, len: bytes.len() as u32 })
}

fn data_bytes(h: &Host, o: u32) -> Option<Vec<u8>> {
    match h.objc.objs.get(&o) {
        Some(Obj::Data { ptr, len }) => {
            Some(unsafe { std::slice::from_raw_parts(h.at(*ptr), *len as usize) }.to_vec())
        }
        _ => None,
    }
}

/// An NSArray of NSStrings.
fn string_array(h: &mut Host, items: Vec<String>) -> u32 {
    let v: Vec<u32> = items.into_iter().map(|s| new_string(h, s)).collect();
    objc::new_array(h, v)
}

/// A plist value as Foundation objects (Info.plist -> NSDictionary).
fn plist_obj(h: &mut Host, v: &plist::Value) -> u32 {
    use plist::Value as V;
    match v {
        V::String(s) => new_string(h, s.clone()),
        V::Boolean(b) => crate::uikit::new_obj(h, "NSNumber", Obj::Number(*b as u8 as f64)),
        V::Integer(i) => crate::uikit::new_obj(h, "NSNumber", Obj::Number(i.as_signed().unwrap_or(0) as f64)),
        V::Real(r) => crate::uikit::new_obj(h, "NSNumber", Obj::Number(*r)),
        V::Array(a) => {
            let items: Vec<u32> = a.iter().map(|x| plist_obj(h, x)).collect();
            objc::new_array(h, items)
        }
        V::Dictionary(d) => {
            let pairs: Vec<(u32, u32)> = d.iter().map(|(k, x)| (new_string(h, k.clone()), plist_obj(h, x))).collect();
            crate::uikit::new_obj(h, "NSDictionary", Obj::Dict(pairs))
        }
        V::Data(d) => new_data(h, "NSData", d),
        _ => 0,
    }
}

fn info_dictionary(h: &mut Host) -> u32 {
    if let Some(&d) = h.ui.singletons.get("Info.plist") {
        return d;
    }
    let data = h.fs.bundle_file("Info.plist");
    let d = match data.and_then(|d| plist::from_bytes::<plist::Value>(&d).ok()) {
        Some(v) => plist_obj(h, &v),
        None => 0,
    };
    h.ui.singletons.insert("Info.plist", d);
    d
}

fn dict_get(h: &Host, d: u32, key: &str) -> u32 {
    match h.objc.objs.get(&d) {
        Some(Obj::Dict(p)) => p.iter().find(|(k, _)| string_of(h, *k).as_deref() == Some(key)).map_or(0, |e| e.1),
        _ => 0,
    }
}

// NSString path helpers (Foundation's rules for '/').
fn last_component(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return if p.starts_with('/') { "/".into() } else { String::new() };
    }
    t.rsplit('/').next().unwrap_or(t).to_string()
}
fn delete_last_component(p: &str) -> String {
    let t = p.trim_end_matches('/');
    match t.rfind('/') {
        Some(0) => "/".into(),
        Some(i) => t[..i].to_string(),
        None => String::new(),
    }
}
fn append_component(p: &str, c: &str) -> String {
    if p.is_empty() {
        return c.to_string();
    }
    format!("{}/{}", p.trim_end_matches('/'), c.trim_start_matches('/'))
}
fn extension(p: &str) -> String {
    let l = last_component(p);
    match l.rfind('.') {
        Some(i) if i > 0 => l[i + 1..].to_string(),
        _ => String::new(),
    }
}
fn delete_extension(p: &str) -> String {
    let ext = extension(p);
    if ext.is_empty() {
        p.trim_end_matches('/').to_string()
    } else {
        let t = p.trim_end_matches('/');
        t[..t.len() - ext.len() - 1].to_string()
    }
}

fn path_for_resource(h: &mut Host, name: Option<String>, ty: Option<String>, dir: Option<String>) -> u32 {
    let Some(name) = name else { return 0 };
    let file = match ty.filter(|t| !t.is_empty()) {
        Some(t) => format!("{name}.{t}"),
        None => name,
    };
    let base = match dir.filter(|d| !d.is_empty()) {
        Some(d) => append_component(&h.fs.bundle_path, &d),
        None => h.fs.bundle_path.clone(),
    };
    let p = append_component(&base, &file);
    if h.fs.exists(&p).is_some() {
        new_string(h, p)
    } else {
        if h.fs.misses.len() < 200 {
            h.fs.misses.push((p, 2));
        }
        0
    }
}

/// Civil date (UTC) from seconds since 1970.
fn civil(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400) as u32;
    // Howard Hinnant's days -> civil.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + (m <= 2) as i64;
    (y, m, d, secs / 3600, secs / 60 % 60, secs % 60)
}

/// NSDateFormatter's common patterns (yyyy MM dd HH mm ss, others copied).
fn format_date(fmt: &str, since_2001: f64) -> String {
    let (y, mo, d, hh, mi, ss) = civil(since_2001.floor() as i64 + 978307200);
    let mut out = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == ch {
            n += 1;
        }
        match ch {
            'y' if n == 2 => out += &format!("{:02}", y % 100),
            'y' => out += &format!("{:04}", y),
            'M' => out += &format!("{:0w$}", mo, w = n.min(2)),
            'd' => out += &format!("{:0w$}", d, w = n.min(2)),
            'H' => out += &format!("{:0w$}", hh, w = n.min(2)),
            'm' => out += &format!("{:0w$}", mi, w = n.min(2)),
            's' => out += &format!("{:0w$}", ss, w = n.min(2)),
            '\'' => {}
            _ => out.extend(std::iter::repeat(ch).take(n)),
        }
        i += n;
    }
    out
}

pub fn native_imp(class: &str, meta: bool, s: &str) -> Option<NativeImp> {
    Some(match (class, meta, s) {
        // ---- NSBundle
        ("NSBundle", true, "mainBundle") => |h, _, _| crate::uikit::singleton(h, "NSBundle"),
        ("NSBundle", false, "bundlePath" | "resourcePath") => |h, _, _| {
            let p = h.fs.bundle_path.clone();
            new_string(h, p)
        },
        ("NSBundle", false, "executablePath") => |h, _, _| {
            let p = format!("{}/BioVS_en", h.fs.bundle_path);
            new_string(h, p)
        },
        ("NSBundle", false, "pathForResource:ofType:") => |h, c, m| {
            let (n, t) = (arg_str(h, c, m, 0), arg_str(h, c, m, 1));
            path_for_resource(h, n, t, None)
        },
        ("NSBundle", false, "pathForResource:ofType:inDirectory:") => |h, c, m| {
            let (n, t, d) = (arg_str(h, c, m, 0), arg_str(h, c, m, 1), arg_str(h, c, m, 2));
            path_for_resource(h, n, t, d)
        },
        ("NSBundle", false, "infoDictionary") => |h, _, _| info_dictionary(h),
        ("NSBundle", false, "objectForInfoDictionaryKey:") => |h, c, m| {
            let d = info_dictionary(h);
            let k = arg_str(h, c, m, 0).unwrap_or_default();
            dict_get(h, d, &k)
        },
        ("NSBundle", false, "bundleIdentifier") => |h, _, _| {
            let d = info_dictionary(h);
            dict_get(h, d, "CFBundleIdentifier")
        },
        // ---- NSFileManager
        ("NSFileManager", false, "fileExistsAtPath:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            yes(h.fs.exists(&p).is_some())
        },
        ("NSFileManager", false, "fileExistsAtPath:isDirectory:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            let out = m.arg(h, c, 1);
            match h.fs.exists(&p) {
                Some(dir) => {
                    if out != 0 {
                        h.wr8(out, dir as u8);
                    }
                    1
                }
                None => 0,
            }
        },
        ("NSFileManager", false, "isReadableFileAtPath:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            yes(h.fs.exists(&p) == Some(false))
        },
        ("NSFileManager", false, "createDirectoryAtPath:withIntermediateDirectories:attributes:error:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            yes(h.fs.mkdir_all(&p))
        },
        ("NSFileManager", false, "contentsOfDirectoryAtPath:error:" | "directoryContentsAtPath:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            match h.fs.list(&p) {
                Some(v) => string_array(h, v),
                None => 0,
            }
        },
        ("NSFileManager", false, "removeItemAtPath:error:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            yes(h.fs.remove(&p))
        },
        ("NSFileManager", false, "createFileAtPath:contents:attributes:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            let d = data_bytes(h, m.arg(h, c, 1)).unwrap_or_default();
            yes(h.fs.write_all(&p, &d))
        },
        ("NSFileManager", false, "copyItemAtPath:toPath:error:" | "moveItemAtPath:toPath:error:") => |h, c, m| {
            let (p, q) = (arg_str(h, c, m, 0).unwrap_or_default(), arg_str(h, c, m, 1).unwrap_or_default());
            if h.fs.exists(&q).is_some() {
                return 0;
            }
            let Some(d) = h.fs.read_all(&p) else { return 0 };
            if !h.fs.write_all(&q, &d) {
                return 0;
            }
            if objc::sel_name(h, m.sel).starts_with("move") {
                h.fs.remove(&p);
            }
            1
        },
        ("NSFileManager", false, "contentsAtPath:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            match h.fs.read_all(&p) {
                Some(d) => new_data(h, "NSData", &d),
                None => 0,
            }
        },
        // ---- NSData
        ("NSData", true, "data") => |h, _, _| new_data(h, "NSData", &[]),
        ("NSMutableData", true, "data") => |h, _, _| new_data(h, "NSMutableData", &[]),
        ("NSData", true, "dataWithContentsOfFile:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            match h.fs.read_all(&p) {
                Some(d) => {
                    let cls = objc::class_name(h, m.this);
                    new_data(h, &cls, &d)
                }
                None => 0,
            }
        },
        ("NSData", false, "initWithContentsOfFile:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            match h.fs.read_all(&p) {
                Some(d) => {
                    let cls = objc::class_name(h, objc::isa(h, m.this));
                    new_data(h, &cls, &d)
                }
                None => 0,
            }
        },
        ("NSData", true, "dataWithLength:") => |h, c, m| {
            let n = m.arg(h, c, 0);
            let cls = objc::class_name(h, m.this);
            new_data(h, &cls, &vec![0; n as usize])
        },
        ("NSData", true, "dataWithBytes:length:") => |h, c, m| {
            let (p, n) = (m.arg(h, c, 0), m.arg(h, c, 1));
            let b = unsafe { std::slice::from_raw_parts(h.at(p), n as usize) }.to_vec();
            let cls = objc::class_name(h, m.this);
            new_data(h, &cls, &b)
        },
        ("NSData", true, "dataWithBytesNoCopy:length:" | "dataWithBytesNoCopy:length:freeWhenDone:") => |h, c, m| {
            // Uses the caller's bytes as they are.
            let (ptr, len) = (m.arg(h, c, 0), m.arg(h, c, 1));
            let cls = objc::class_name(h, m.this);
            crate::uikit::new_obj(h, &cls, Obj::Data { ptr, len })
        },
        ("NSData", false, "initWithBytes:length:") => |h, c, m| {
            let (p, n) = (m.arg(h, c, 0), m.arg(h, c, 1));
            let b = unsafe { std::slice::from_raw_parts(h.at(p), n as usize) }.to_vec();
            init_data(h, m.this, &b)
        },
        ("NSData", false, "initWithBytesNoCopy:length:" | "initWithBytesNoCopy:length:freeWhenDone:") => |h, c, m| {
            let (ptr, len) = (m.arg(h, c, 0), m.arg(h, c, 1));
            h.objc.objs.insert(m.this, Obj::Data { ptr, len });
            m.this
        },
        ("NSData", false, "initWithLength:") => |h, c, m| {
            let n = m.arg(h, c, 0);
            init_data(h, m.this, &vec![0; n as usize])
        },
        ("NSData", false, "initWithData:") => |h, c, m| {
            let d = data_bytes(h, m.arg(h, c, 0)).unwrap_or_default();
            init_data(h, m.this, &d)
        },
        ("NSData", false, "bytes" | "mutableBytes") => |h, _, m| match h.objc.objs.get(&m.this) {
            Some(Obj::Data { ptr, .. }) => *ptr,
            _ => 0,
        },
        ("NSData", false, "length") => |h, _, m| match h.objc.objs.get(&m.this) {
            Some(Obj::Data { len, .. }) => *len,
            _ => 0,
        },
        ("NSData", false, "writeToFile:atomically:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            let d = data_bytes(h, m.this).unwrap_or_default();
            yes(h.fs.write_all(&p, &d))
        },
        ("NSMutableData", false, "appendData:") => |h, c, m| {
            let mut d = data_bytes(h, m.this).unwrap_or_default();
            d.extend(data_bytes(h, m.arg(h, c, 0)).unwrap_or_default());
            replace_data(h, m.this, &d);
            0
        },
        ("NSMutableData", false, "appendBytes:length:") => |h, c, m| {
            let (p, n) = (m.arg(h, c, 0), m.arg(h, c, 1));
            let mut d = data_bytes(h, m.this).unwrap_or_default();
            d.extend_from_slice(unsafe { std::slice::from_raw_parts(h.at(p), n as usize) });
            replace_data(h, m.this, &d);
            0
        },
        // ---- NSString paths and files
        ("NSString", false, "stringByAppendingPathComponent:") => |h, c, m| {
            let (p, x) = (string_of(h, m.this).unwrap_or_default(), arg_str(h, c, m, 0).unwrap_or_default());
            new_string(h, append_component(&p, &x))
        },
        ("NSString", false, "stringByAppendingPathExtension:") => |h, c, m| {
            let (p, x) = (string_of(h, m.this).unwrap_or_default(), arg_str(h, c, m, 0).unwrap_or_default());
            new_string(h, format!("{}.{x}", p.trim_end_matches('/')))
        },
        ("NSString", false, "lastPathComponent") => |h, _, m| {
            let p = string_of(h, m.this).unwrap_or_default();
            new_string(h, last_component(&p))
        },
        ("NSString", false, "stringByDeletingLastPathComponent") => |h, _, m| {
            let p = string_of(h, m.this).unwrap_or_default();
            new_string(h, delete_last_component(&p))
        },
        ("NSString", false, "pathExtension") => |h, _, m| {
            let p = string_of(h, m.this).unwrap_or_default();
            new_string(h, extension(&p))
        },
        ("NSString", false, "stringByDeletingPathExtension") => |h, _, m| {
            let p = string_of(h, m.this).unwrap_or_default();
            new_string(h, delete_extension(&p))
        },
        ("NSString", false, "stringByStandardizingPath" | "stringByExpandingTildeInPath") => |h, _, m| {
            let p = string_of(h, m.this).unwrap_or_default();
            let p = match p.strip_prefix('~') {
                Some(r) => format!("{HOME}{r}"),
                None => p,
            };
            new_string(h, p)
        },
        ("NSString", false, "fileSystemRepresentation") => |h, _, m| {
            // Like UTF8String: a NUL-terminated copy in guest memory.
            let p = string_of(h, m.this).unwrap_or_default();
            let a = crate::os::alloc_zeroed(h, p.len() as u32 + 1);
            unsafe { std::ptr::copy_nonoverlapping(p.as_ptr(), h.at(a), p.len()) };
            a
        },
        ("NSString", true, "stringWithContentsOfFile:" | "stringWithContentsOfFile:encoding:error:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            match h.fs.read_all(&p) {
                Some(d) => new_string(h, String::from_utf8_lossy(&d).into_owned()),
                None => 0,
            }
        },
        ("NSString", false, "initWithContentsOfFile:" | "initWithContentsOfFile:encoding:error:") => |h, c, m| {
            let p = arg_str(h, c, m, 0).unwrap_or_default();
            match h.fs.read_all(&p) {
                Some(d) => new_string(h, String::from_utf8_lossy(&d).into_owned()),
                None => 0,
            }
        },
        ("NSString", false, "initWithData:encoding:") => |h, c, m| {
            let d = data_bytes(h, m.arg(h, c, 0)).unwrap_or_default();
            let t = String::from_utf8_lossy(&d).trim_end_matches('\0').to_string();
            new_string(h, t)
        },
        ("NSString", false, "writeToFile:atomically:" | "writeToFile:atomically:encoding:error:") => |h, c, m| {
            let (p, t) = (arg_str(h, c, m, 0).unwrap_or_default(), string_of(h, m.this).unwrap_or_default());
            yes(h.fs.write_all(&p, t.as_bytes()))
        },
        // ---- NSFileHandle (a descriptor in fs.rs)
        ("NSFileHandle", true, "fileHandleForReadingAtPath:" | "fileHandleForWritingAtPath:" | "fileHandleForUpdatingAtPath:") => {
            |h, c, m| {
                let p = arg_str(h, c, m, 0).unwrap_or_default();
                let sel = objc::sel_name(h, m.sel);
                let flags = if sel.contains("Reading") { 0 } else if sel.contains("Writing") { 1 } else { 2 };
                match h.fs.open(&p, flags) {
                    Ok(fd) => crate::uikit::new_obj(h, "NSFileHandle", Obj::Handle(fd)),
                    Err(e) => {
                        if h.fs.misses.len() < 200 {
                            h.fs.misses.push((p, e));
                        }
                        0
                    }
                }
            }
        }
        ("NSFileHandle", false, "readDataOfLength:" | "readDataToEndOfFile" | "availableData") => |h, c, m| {
            let Some(fd) = handle_fd(h, m.this) else { return 0 };
            let want = if objc::sel_name(h, m.sel) == "readDataOfLength:" {
                m.arg(h, c, 0) as u64
            } else {
                h.fs.len(fd).unwrap_or(0).saturating_sub(h.fs.pos(fd))
            };
            let left = h.fs.len(fd).unwrap_or(0).saturating_sub(h.fs.pos(fd));
            let mut buf = vec![0u8; want.min(left) as usize];
            let n = h.fs.read(fd, &mut buf, None).unwrap_or(0);
            buf.truncate(n);
            new_data(h, "NSData", &buf)
        },
        ("NSFileHandle", false, "writeData:") => |h, c, m| {
            if let Some(fd) = handle_fd(h, m.this) {
                let d = data_bytes(h, m.arg(h, c, 0)).unwrap_or_default();
                let _ = h.fs.write(fd, &d, None);
            }
            0
        },
        ("NSFileHandle", false, "seekToFileOffset:") => |h, c, m| {
            // unsigned long long: words 0-1.
            if let Some(fd) = handle_fd(h, m.this) {
                let off = m.arg(h, c, 0) as u64 | (m.arg(h, c, 1) as u64) << 32;
                let _ = h.fs.seek(fd, off as i64, 0);
            }
            0
        },
        ("NSFileHandle", false, "seekToEndOfFile" | "offsetInFile") => |h, c, m| {
            let Some(fd) = handle_fd(h, m.this) else { return 0 };
            let p = if objc::sel_name(h, m.sel) == "seekToEndOfFile" { h.fs.seek(fd, 0, 2).unwrap_or(0) } else { h.fs.pos(fd) };
            c.r[1] = (p >> 32) as u32;
            p as u32
        },
        ("NSFileHandle", false, "truncateFileAtOffset:" | "synchronizeFile") => |_, _, _| 0,
        ("NSFileHandle", false, "closeFile") => |h, _, m| {
            if let Some(fd) = handle_fd(h, m.this) {
                h.fs.close(fd);
                h.objc.objs.insert(m.this, Obj::Handle(u32::MAX));
            }
            0
        },
        // ---- NSLocale: English, United States.
        ("NSLocale", true, "preferredLanguages") => |h, _, _| string_array(h, vec!["en".into()]),
        ("NSLocale", true, "currentLocale" | "systemLocale" | "autoupdatingCurrentLocale") => {
            |h, _, _| crate::uikit::singleton(h, "NSLocale")
        }
        ("NSLocale", false, "objectForKey:") => |h, c, m| {
            let k = arg_str(h, c, m, 0).unwrap_or_default();
            let v = match k.as_str() {
                "kCFLocaleCountryCodeKey" => "US",
                "kCFLocaleLanguageCodeKey" => "en",
                _ => return 0,
            };
            new_string(h, v.into())
        },
        ("NSLocale", false, "localeIdentifier") => |h, _, _| new_string(h, "en_US".into()),
        ("NSString", false, "compare:" | "compare:options:" | "caseInsensitiveCompare:") => |h, c, m| {
            let a = string_of(h, m.this).unwrap_or_default();
            let b = arg_str(h, c, m, 0).unwrap_or_default();
            let sel = objc::sel_name(h, m.sel);
            let fold = sel == "caseInsensitiveCompare:" || (sel == "compare:options:" && m.arg(h, c, 1) & 1 != 0);
            let o = if fold { a.to_lowercase().cmp(&b.to_lowercase()) } else { a.cmp(&b) };
            o as i32 as u32
        },
        // ---- NSDateFormatter / NSTimeZone (UTC only)
        ("NSTimeZone", true, "timeZoneWithName:" | "systemTimeZone" | "localTimeZone" | "defaultTimeZone") => {
            |h, _, _| crate::uikit::singleton(h, "NSTimeZone")
        }
        ("NSDateFormatter", false, "setDateFormat:") => |h, c, m| {
            let f = arg_str(h, c, m, 0).unwrap_or_default();
            h.objc.objs.insert(m.this, Obj::Str(f));
            0
        },
        ("NSDateFormatter", false, "setTimeZone:" | "setLocale:") => |_, _, _| 0,
        ("NSDateFormatter", false, "stringFromDate:") => |h, c, m| {
            let f = match h.objc.objs.get(&m.this) {
                Some(Obj::Str(f)) => f.clone(),
                _ => "yyyy-MM-dd HH:mm:ss".into(),
            };
            let t = match h.objc.objs.get(&m.arg(h, c, 0)) {
                Some(Obj::Date(t)) => *t,
                _ => 0.0,
            };
            new_string(h, format_date(&f, t))
        },
        _ => return crate::audio::native_imp(class, meta, s),
    })
}

fn handle_fd(h: &Host, o: u32) -> Option<u32> {
    match h.objc.objs.get(&o) {
        Some(Obj::Handle(fd)) if *fd != u32::MAX => Some(*fd),
        _ => None,
    }
}

/// -init... on an allocated NSData: store the bytes, return self.
fn init_data(h: &mut Host, o: u32, b: &[u8]) -> u32 {
    replace_data(h, o, b);
    o
}

fn replace_data(h: &mut Host, o: u32, d: &[u8]) {
    let ptr = crate::os::alloc_zeroed(h, d.len().max(1) as u32);
    unsafe { std::ptr::copy_nonoverlapping(d.as_ptr(), h.at(ptr), d.len()) };
    h.objc.objs.insert(o, Obj::Data { ptr, len: d.len() as u32 });
}

/// NSSearchPathForDirectoriesInDomains(directory, domainMask, expandTilde).
pub fn search_path(h: &mut Host, dir: u32, expand: bool) -> u32 {
    let rel = match dir {
        9 => "Documents",                         // NSDocumentDirectory
        5 => "Library",                           // NSLibraryDirectory
        13 => "Library/Caches",                   // NSCachesDirectory
        14 => "Library/Application Support",      // NSApplicationSupportDirectory
        _ => return objc::new_array(h, Vec::new()),
    };
    let p = if expand { format!("{HOME}/{rel}") } else { format!("~/{rel}") };
    string_array(h, vec![p])
}

/// Fills the OS data symbols the game reads with what Apple's would hold:
/// NSString constants (keys, notification names, run loop modes), the stdio
/// FILE pointers and CGAffineTransformIdentity. Others stay zero.
pub fn fill_data_symbols(h: &mut Host) {
    let names = h.data_names.clone();
    for (i, name) in names.iter().enumerate() {
        let block = crate::HOST_DATA + crate::HOST_DATA_BLOCK * i as u32;
        let text = match name.as_str() {
            "NSFileModificationDate" | "NSFileSize" | "NSFileType" => Some(name.as_str()),
            "NSGregorianCalendar" => Some("gregorian"),
            "NSLocaleCountryCode" => Some("kCFLocaleCountryCodeKey"),
            "NSDefaultRunLoopMode" | "kCFRunLoopDefaultMode" => Some("kCFRunLoopDefaultMode"),
            "NSRunLoopCommonModes" | "kCFRunLoopCommonModes" => Some("kCFRunLoopCommonModes"),
            "kEAGLColorFormatRGB565" => Some("EAGLColorFormatRGB565"),
            "kEAGLColorFormatRGBA8" => Some("EAGLColorFormatRGBA8"),
            "kEAGLDrawablePropertyColorFormat" => Some("EAGLDrawablePropertyColorFormat"),
            "kEAGLDrawablePropertyRetainedBacking" => Some("EAGLDrawablePropertyRetainedBacking"),
            "kCAMediaTimingFunctionEaseIn" => Some("easeIn"),
            "kCAMediaTimingFunctionEaseOut" => Some("easeOut"),
            "kCAMediaTimingFunctionLinear" => Some("linear"),
            n if n.ends_with("Notification") || n.ends_with("NotificationName") => Some(n),
            _ => None,
        };
        if let Some(t) = text {
            let s = new_string(h, t.to_string());
            h.wr32(block, s);
            continue;
        }
        match name.as_str() {
            "__stdinp" | "__stdoutp" | "__stderrp" => {
                let fd = ["__stdinp", "__stdoutp", "__stderrp"].iter().position(|x| x == name).unwrap() as u32;
                let f = crate::fs::new_file(h, fd);
                h.wr32(block, f);
            }
            "CGAffineTransformIdentity" => {
                for (k, v) in [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0].iter().enumerate() {
                    h.wr32(block + 4 * k as u32, v.to_bits());
                }
            }
            _ => {}
        }
    }
}
