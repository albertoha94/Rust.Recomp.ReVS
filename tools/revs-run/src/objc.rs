//! Objective-C runtime (Phase 3.3), working directly on the game's own ObjC2
//! metadata in guest memory.
//!
//! - Apple classes the game links against (`_OBJC_CLASS_$_UIView`, ...) become
//!   native class objects in the runtime area: real 32-bit class_t/class_ro_t
//!   structs, so the game's own classes can sit on top of them unchanged.
//! - `objc_msgSend` looks a selector up the class chain: the game's method
//!   lists (and categories) first, then the native methods below.
//! - Selectors are made unique by name, as the real runtime does: every
//!   selref is rewritten to one canonical address per name.
//! - `UIApplicationMain` loads the main nib (NIBArchive) from the .ipa, which
//!   creates the app delegate, window and view controller, then sends
//!   `application:didFinishLaunchingWithOptions:`.
//!
//! A message with no implementation anywhere is listed (survey mode) and
//! returns nil (or self, for `init...`).

use crate::{Cpu, Host};
use nibarchive::{NIBArchive, ValueVariant};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Guest memory the runtime owns (native classes, selector names, buffers).
pub const AREA: u32 = 0xD000_0000;
const AREA_END: u32 = 0xE000_0000;

/// One message being handled natively.
pub struct Msg {
    pub this: u32,
    pub sel: u32,
    /// Argument word index of the first argument after self and _cmd
    /// (2 normally, 3 for a struct return, where r0 is the result pointer).
    pub first: u32,
}
pub(crate) type NativeImp = fn(&mut Host, &mut Cpu, &Msg) -> u32;

#[derive(Clone, Copy)]
enum Imp {
    Guest(u32),
    Native(NativeImp),
}

/// State behind a native object.
pub enum Obj {
    Str(String),
    Array(Vec<u32>),
    Number(f64),
    /// (key, value) pairs.
    Dict(Vec<(u32, u32)>),
    /// Seconds since 2001-01-01.
    Date(f64),
    Notification { name: u32, object: u32 },
    /// NSData: `len` bytes at guest address `ptr`.
    Data { ptr: u32, len: u32 },
    /// NSFileHandle: a descriptor in fs.rs.
    Handle(u32),
    Conn { src: u32, dst: u32, label: String },
}

struct Nib {
    arc: NIBArchive,
    cur: u32,
    done: Vec<Option<u32>>,
    owner: u32,
    decoder: u32,
}

struct Missing {
    order: u32,
    calls: u64,
    first_lr: u32,
    receiver: String,
}

pub struct Runtime {
    pub ipa: PathBuf,
    area: u32,
    sels: HashMap<String, u32>,
    sel_names: HashMap<u32, String>,
    /// Native class object -> (name, is metaclass).
    pub(crate) native: HashMap<u32, (String, bool)>,
    /// Every class by name (game and native).
    by_name: HashMap<String, u32>,
    game_classes: u32,
    /// Class -> selector -> guest IMP (the game's methods and categories).
    methods: HashMap<u32, HashMap<u32, u32>>,
    cache: HashMap<(u32, u32), Option<Imp>>,
    initialized: HashSet<u32>,
    missing: HashMap<String, Missing>,
    pub objs: HashMap<u32, Obj>,
    utf8: HashMap<u32, u32>,
    const_string_class: u32,
    nib: Option<Nib>,
    pub app: u32,
    pub delegate: u32,
    pub log: Vec<String>,
}

impl Runtime {
    pub fn new() -> Runtime {
        Runtime {
            ipa: PathBuf::new(),
            area: AREA,
            sels: HashMap::new(),
            sel_names: HashMap::new(),
            native: HashMap::new(),
            by_name: HashMap::new(),
            game_classes: 0,
            methods: HashMap::new(),
            cache: HashMap::new(),
            initialized: HashSet::new(),
            missing: HashMap::new(),
            objs: HashMap::new(),
            utf8: HashMap::new(),
            const_string_class: 0,
            nib: None,
            app: 0,
            delegate: 0,
            log: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------- memory

fn area(h: &mut Host, size: u32) -> u32 {
    let p = h.objc.area;
    h.objc.area = (p + size + 7) & !7;
    assert!(h.objc.area < AREA_END, "ObjC runtime area full");
    p
}

fn area_str(h: &mut Host, s: &str) -> u32 {
    let p = area(h, s.len() as u32 + 1);
    for (k, b) in s.bytes().enumerate() {
        h.wr8(p + k as u32, b);
    }
    h.wr8(p + s.len() as u32, 0);
    p
}

fn word(h: &Host, c: &Cpu, i: u32) -> u32 {
    crate::os::arg(h, c, i)
}

impl Msg {
    pub fn arg(&self, h: &Host, c: &Cpu, i: u32) -> u32 {
        word(h, c, self.first + i)
    }
}

// ---------------------------------------------------------------- classes

pub(crate) fn isa(h: &Host, o: u32) -> u32 {
    h.rd32(o)
}
pub(crate) fn superclass(h: &Host, cls: u32) -> u32 {
    h.rd32(cls + 4)
}
fn ro(h: &Host, cls: u32) -> u32 {
    h.rd32(cls + 16) & !3
}
fn is_meta(h: &Host, cls: u32) -> bool {
    h.rd32(ro(h, cls)) & 1 != 0
}
pub fn class_name(h: &Host, cls: u32) -> String {
    if cls == 0 {
        return "nil".into();
    }
    h.cstr(h.rd32(ro(h, cls) + 16))
}

/// The superclass of an Apple class, for the classes the game uses. None: not
/// a class this runtime knows (NSClassFromString returns nil for it).
fn parent(name: &str) -> Option<&'static str> {
    Some(match name {
        "NSObject" => "",
        "NSMutableString" | "__NSCFConstantString" => "NSString",
        "NSMutableArray" => "NSArray",
        "NSMutableDictionary" => "NSDictionary",
        "NSMutableData" => "NSData",
        "NSMutableSet" => "NSSet",
        "NSNumber" => "NSValue",
        "NSKeyedArchiver" | "NSKeyedUnarchiver" | "UINibDecoder" => "NSCoder",
        "NSString" | "NSArray" | "NSDictionary" | "NSData" | "NSSet" | "NSValue" | "NSCoder" | "NSAutoreleasePool"
        | "NSUserDefaults" | "NSTimer" | "NSBundle" | "NSFileManager" | "NSNotificationCenter" | "NSNotification"
        | "NSDate" | "NSLocale" | "NSRunLoop" | "NSThread" | "NSProcessInfo" | "NSCalendar" | "NSDateComponents"
        | "NSDateFormatter" | "NSURL" | "NSURLRequest" | "NSURLConnection" | "NSError" | "NSIndexPath"
        | "NSInvocation" | "NSMethodSignature" | "NSNull" | "NSLock" | "NSCondition" | "NSRecursiveLock"
        | "NSCharacterSet" | "NSScanner" | "NSFileHandle" | "NSPropertyListSerialization" | "NSTimeZone"
        | "NSException" | "NSSortDescriptor" | "NSPredicate" | "NSOperationQueue" | "NSOperation" | "NSEnumerator" => "NSObject",
        "UIResponder" | "UIImage" | "UIColor" | "UIFont" | "UIScreen" | "UIDevice" | "UITouch" | "UIEvent"
        | "UIAccelerometer" | "UIAcceleration" | "UIGestureRecognizer" | "UIBarButtonItem" | "UINib"
        | "UIRuntimeOutletConnection" | "UIRuntimeEventConnection" | "UIPasteboard" | "UILocalNotification"
        | "EAGLContext" | "EAGLSharegroup" | "CALayer" | "CADisplayLink" | "CAAnimation" | "CAMediaTimingFunction"
        | "AVAudioPlayer" | "AVAudioSession" | "GKPlayer" | "GKScore" | "GKAchievement" | "GKLeaderboard"
        | "GKMatchRequest" | "GKMatch" | "GKMatchmaker" | "GKVoiceChat" | "GKSession" => "NSObject",
        "CAEAGLLayer" => "CALayer",
        "CABasicAnimation" => "CAAnimation",
        "GKLocalPlayer" => "GKPlayer",
        "UIView" | "UIViewController" | "UIApplication" => "UIResponder",
        "UIWindow" | "UIImageView" | "UILabel" | "UIControl" | "UIScrollView" | "UIAlertView" | "UIActionSheet"
        | "UITableViewCell" | "UIActivityIndicatorView" | "UIWebView" | "UIToolbar" | "UINavigationBar"
        | "UIPickerView" | "UIProgressView" => "UIView",
        "UIButton" | "UITextField" | "UISwitch" | "UISlider" | "UISegmentedControl" | "UIPageControl" => "UIControl",
        "UITableView" | "UITextView" => "UIScrollView",
        "UINavigationController" | "UITabBarController" | "UITableViewController" | "UIImagePickerController"
        | "GKMatchmakerViewController" | "GKLeaderboardViewController" | "GKAchievementViewController" => {
            "UIViewController"
        }
        _ => return None,
    })
}

/// The native class object for Apple class `name` (made on first use).
pub fn native_class(h: &mut Host, name: &str) -> u32 {
    if let Some(&a) = h.objc.by_name.get(name) {
        return a;
    }
    let sup = match parent(name) {
        Some("") => None,
        Some(p) => Some(native_class(h, p)),
        None => Some(native_class(h, "NSObject")),
    };
    let (cls, meta) = (area(h, 20), area(h, 20));
    let (ro_c, ro_m, nm) = (area(h, 40), area(h, 40), area_str(h, name));
    let root_meta = match sup {
        None => meta,
        Some(_) => {
            let root = native_class(h, "NSObject");
            isa(h, root)
        }
    };
    for (o, v) in [(0, meta), (4, sup.unwrap_or(0)), (8, 0), (12, 0), (16, ro_c)] {
        h.wr32(cls + o, v);
    }
    for (o, v) in [(0, root_meta), (4, sup.map_or(cls, |s| isa(h, s))), (8, 0), (12, 0), (16, ro_m)] {
        h.wr32(meta + o, v);
    }
    for (o, v) in [(0, 0), (4, 4), (8, 16), (16, nm)] {
        h.wr32(ro_c + o, v);
    }
    for (o, v) in [(0, 1), (4, 20), (8, 20), (16, nm)] {
        h.wr32(ro_m + o, v);
    }
    h.objc.native.insert(cls, (name.to_string(), false));
    h.objc.native.insert(meta, (name.to_string(), true));
    h.objc.by_name.insert(name.to_string(), cls);
    cls
}

/// What a bound ObjC symbol points at (None: not an ObjC symbol).
pub fn bind(h: &mut Host, short: &str) -> Option<u32> {
    if let Some(n) = short.strip_prefix("OBJC_CLASS_$_") {
        Some(native_class(h, n))
    } else if let Some(n) = short.strip_prefix("OBJC_METACLASS_$_") {
        let c = native_class(h, n);
        Some(isa(h, c))
    } else if short == "__CFConstantStringClassReference" {
        let c = native_class(h, "__NSCFConstantString");
        h.objc.const_string_class = c;
        Some(c)
    } else {
        None
    }
}

pub fn class_named(h: &mut Host, name: &str) -> u32 {
    match h.objc.by_name.get(name) {
        Some(&c) => c,
        None => native_class(h, name),
    }
}

// ---------------------------------------------------------------- selectors

/// The canonical selector for the name at guest address `a` (`a` itself if
/// the name is new).
fn intern_at(h: &mut Host, a: u32) -> u32 {
    let name = h.cstr(a);
    if let Some(&s) = h.objc.sels.get(&name) {
        return s;
    }
    h.objc.sels.insert(name.clone(), a);
    h.objc.sel_names.insert(a, name);
    a
}

pub fn sel(h: &mut Host, name: &str) -> u32 {
    if let Some(&s) = h.objc.sels.get(name) {
        return s;
    }
    let a = area_str(h, name);
    h.objc.sels.insert(name.to_string(), a);
    h.objc.sel_names.insert(a, name.to_string());
    a
}

pub(crate) fn sel_name(h: &Host, s: u32) -> String {
    h.objc.sel_names.get(&s).cloned().unwrap_or_else(|| h.cstr(s))
}

// ---------------------------------------------------------------- loading

fn add_methods(h: &mut Host, cls: u32, list: u32) {
    if list == 0 || cls == 0 {
        return;
    }
    let entsize = h.rd32(list) & !3;
    let count = h.rd32(list + 4);
    for k in 0..count {
        let e = list + 8 + k * entsize;
        let s = intern_at(h, h.rd32(e));
        let imp = h.rd32(e + 8);
        h.objc.methods.entry(cls).or_default().insert(s, imp);
    }
}

/// Registers the game's classes, categories and selectors (after linking).
pub fn load(h: &mut Host, bin: &revs_analyze::macho::Binary) {
    let sect = |s: &str| bin.section("__DATA", s).map(|x| (x.addr, x.size));
    if let Some((a, n)) = sect("__objc_selrefs") {
        for k in 0..n / 4 {
            let s = intern_at(h, h.rd32(a + 4 * k));
            h.wr32(a + 4 * k, s);
        }
    }
    if let Some((a, n)) = sect("__objc_classlist") {
        for k in 0..n / 4 {
            let cls = h.rd32(a + 4 * k);
            let meta = isa(h, cls);
            let name = class_name(h, cls);
            h.objc.by_name.insert(name, cls);
            h.objc.game_classes += 1;
            for c in [cls, meta] {
                let list = h.rd32(ro(h, c) + 20);
                add_methods(h, c, list);
            }
        }
    }
    // category_t: name, cls, instanceMethods, classMethods, protocols, properties.
    if let Some((a, n)) = sect("__objc_catlist") {
        for k in 0..n / 4 {
            let cat = h.rd32(a + 4 * k);
            let cls = h.rd32(cat + 4);
            if cls == 0 {
                continue;
            }
            let (im, cm) = (h.rd32(cat + 8), h.rd32(cat + 12));
            add_methods(h, cls, im);
            let meta = isa(h, cls);
            add_methods(h, meta, cm);
        }
    }
}

// ---------------------------------------------------------------- messaging

fn lookup(h: &mut Host, cls: u32, s: u32) -> Option<Imp> {
    if let Some(&r) = h.objc.cache.get(&(cls, s)) {
        return r;
    }
    let name = sel_name(h, s);
    let (mut k, mut r, mut depth) = (cls, None, 0);
    while k != 0 && depth < 64 {
        if let Some(&imp) = h.objc.methods.get(&k).and_then(|m| m.get(&s)) {
            r = Some(Imp::Guest(imp));
            break;
        }
        if let Some((n, meta)) = h.objc.native.get(&k) {
            if let Some(f) = native_imp(n, *meta, &name) {
                r = Some(Imp::Native(f));
                break;
            }
        }
        k = superclass(h, k);
        depth += 1;
    }
    h.objc.cache.insert((cls, s), r);
    r
}

pub fn responds(h: &mut Host, o: u32, s: &str) -> bool {
    if o == 0 {
        return false;
    }
    let s = sel(h, s);
    lookup(h, isa(h, o), s).is_some()
}

/// Sends +initialize the first time a class (or an instance of it) gets a message.
fn ensure_initialized(h: &mut Host, c: &mut Cpu, class: u32) {
    if class == 0 || !h.objc.initialized.insert(class) {
        return;
    }
    let sup = superclass(h, class);
    if sup != 0 && !is_meta(h, sup) {
        ensure_initialized(h, c, sup);
    }
    send(h, c, class, "initialize", &[]);
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Normal,
    Stret,
    Super2,
}

fn dispatch(h: &mut Host, c: &mut Cpu, kind: Kind) {
    let (this, s, cls, first) = match kind {
        Kind::Normal => {
            if c.r[0] == 0 {
                c.r[1] = 0;
                return;
            }
            (c.r[0], c.r[1], isa(h, c.r[0]), 2)
        }
        Kind::Stret => {
            if c.r[1] == 0 {
                return;
            }
            (c.r[1], c.r[2], isa(h, c.r[1]), 3)
        }
        // r0 = struct objc_super { receiver, current class }.
        Kind::Super2 => {
            let sup = c.r[0];
            let this = h.rd32(sup);
            c.r[0] = this;
            (this, c.r[1], superclass(h, h.rd32(sup + 4)), 2)
        }
    };
    if kind != Kind::Super2 {
        let class = if is_meta(h, cls) { this } else { cls };
        ensure_initialized(h, c, class);
        if c.halt != 0 {
            return;
        }
    }
    match lookup(h, cls, s) {
        Some(Imp::Guest(f)) => unsafe { crate::call_guest(c, f) },
        Some(Imp::Native(f)) => {
            let r = f(h, c, &Msg { this, sel: s, first });
            if kind != Kind::Stret {
                c.r[0] = r;
            }
        }
        None => {
            let name = sel_name(h, s);
            note_missing(h, c, cls, &name, this);
            if kind != Kind::Stret {
                c.r[0] = if name.starts_with("init") { this } else { 0 };
                c.r[1] = 0;
            }
        }
    }
}

fn note_missing(h: &mut Host, c: &Cpu, cls: u32, name: &str, this: u32) {
    // Name it after the first Apple class up the chain (where it belongs).
    let (mut k, mut depth) = (cls, 0);
    while k != 0 && !h.objc.native.contains_key(&k) && depth < 64 {
        k = superclass(h, k);
        depth += 1;
    }
    let (owner, meta) = h.objc.native.get(&k).cloned().unwrap_or((class_name(h, cls), is_meta(h, cls)));
    let key = format!("{}[{owner} {name}]", if meta { '+' } else { '-' });
    let order = h.objc.missing.len() as u32;
    let recv = class_name(h, if meta { this } else { cls });
    let m = h.objc.missing.entry(key).or_insert(Missing { order, calls: 0, first_lr: c.r[14], receiver: recv });
    m.calls += 1;
}

/// Sends a message from native code (at most 2 arguments). Returns r0.
pub fn send(h: &mut Host, c: &mut Cpu, recv: u32, selector: &str, args: &[u32]) -> u32 {
    assert!(args.len() <= 2, "send: too many arguments");
    let s = sel(h, selector);
    let saved = (c.r[0], c.r[1], c.r[2], c.r[3], c.r[12], c.r[14]);
    c.r[0] = recv;
    c.r[1] = s;
    for (k, &a) in args.iter().enumerate() {
        c.r[2 + k] = a;
    }
    dispatch(h, c, Kind::Normal);
    let r = c.r[0];
    (c.r[0], c.r[1], c.r[2], c.r[3], c.r[12], c.r[14]) = saved;
    r
}

/// Like `send`, with any number of arguments: the first two in r2/r3, the
/// rest on the stack (as the ARM calling convention passes them).
pub fn send_n(h: &mut Host, c: &mut Cpu, recv: u32, selector: &str, args: &[u32]) -> u32 {
    if args.len() <= 2 {
        return send(h, c, recv, selector, args);
    }
    let s = sel(h, selector);
    let saved = (c.r[0], c.r[1], c.r[2], c.r[3], c.r[12], c.r[13], c.r[14]);
    let extra = &args[2..];
    let room = (extra.len() as u32 * 4 + 7) & !7;
    c.r[13] -= room;
    for (k, &a) in extra.iter().enumerate() {
        h.wr32(c.r[13] + 4 * k as u32, a);
    }
    c.r[0] = recv;
    c.r[1] = s;
    c.r[2] = args[0];
    c.r[3] = args[1];
    dispatch(h, c, Kind::Normal);
    let r = c.r[0];
    (c.r[0], c.r[1], c.r[2], c.r[3], c.r[12], c.r[13], c.r[14]) = saved;
    r
}

// ---------------------------------------------------------------- objects

pub(crate) fn alloc_instance(h: &mut Host, cls: u32) -> u32 {
    let size = h.rd32(ro(h, cls) + 8).max(4);
    let p = crate::os::alloc_zeroed(h, size);
    h.wr32(p, cls);
    p
}

pub fn new_string(h: &mut Host, s: String) -> u32 {
    let cls = native_class(h, "NSString");
    let o = alloc_instance(h, cls);
    h.objc.objs.insert(o, Obj::Str(s));
    o
}

pub(crate) fn new_array(h: &mut Host, items: Vec<u32>) -> u32 {
    let cls = native_class(h, "NSArray");
    let o = alloc_instance(h, cls);
    h.objc.objs.insert(o, Obj::Array(items));
    o
}

/// The text of an NSString (native or a constant CFString in the image).
pub fn string_of(h: &Host, o: u32) -> Option<String> {
    if o == 0 {
        return None;
    }
    if let Some(Obj::Str(s)) = h.objc.objs.get(&o) {
        return Some(s.clone());
    }
    if isa(h, o) == h.objc.const_string_class && o != 0 {
        // CFString constant: isa, flags, chars, length. Flag 0x10 = UTF-16.
        let (flags, p, len) = (h.rd32(o + 4), h.rd32(o + 8), h.rd32(o + 12));
        return Some(if flags & 0x10 != 0 {
            let u: Vec<u16> = (0..len).map(|k| h.rd8(p + 2 * k) as u16 | (h.rd8(p + 2 * k + 1) as u16) << 8).collect();
            String::from_utf16_lossy(&u)
        } else {
            String::from_utf8_lossy(&(0..len).map(|k| h.rd8(p + k)).collect::<Vec<_>>()).into_owned()
        });
    }
    None
}

fn describe(h: &Host, o: u32) -> String {
    if o == 0 {
        return "(null)".into();
    }
    string_of(h, o).unwrap_or_else(|| format!("<{}: {o:#x}>", class_name(h, isa(h, o))))
}

/// printf/NSLog-style formatting; arguments start at word `w`.
pub fn format(h: &Host, c: &Cpu, fmt: &str, mut w: u32) -> String {
    let mut out = String::new();
    let mut it = fmt.chars().peekable();
    let next = |w: &mut u32| {
        let v = word(h, c, *w);
        *w += 1;
        v
    };
    while let Some(ch) = it.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let (mut left, mut zero, mut plus) = (false, false, false);
        while let Some(&f) = it.peek() {
            match f {
                '-' => left = true,
                '0' => zero = true,
                '+' => plus = true,
                ' ' | '#' => {}
                _ => break,
            }
            it.next();
        }
        let mut width = 0usize;
        if it.peek() == Some(&'*') {
            it.next();
            width = next(&mut w) as usize;
        }
        while let Some(d) = it.peek().and_then(|d| d.to_digit(10)) {
            width = width * 10 + d as usize;
            it.next();
        }
        let mut prec = None;
        if it.peek() == Some(&'.') {
            it.next();
            let mut p = 0usize;
            if it.peek() == Some(&'*') {
                it.next();
                p = next(&mut w) as usize;
            }
            while let Some(d) = it.peek().and_then(|d| d.to_digit(10)) {
                p = p * 10 + d as usize;
                it.next();
            }
            prec = Some(p);
        }
        let mut long = 0;
        while let Some(&l) = it.peek() {
            match l {
                'l' | 'q' => long += 1,
                'h' | 'z' | 't' | 'j' => {}
                _ => break,
            }
            it.next();
        }
        let Some(conv) = it.next() else { break };
        let wide = |w: &mut u32| {
            *w += *w & 1; // 64-bit values sit at an even word
            let lo = word(h, c, *w) as u64;
            let hi = word(h, c, *w + 1) as u64;
            *w += 2;
            lo | hi << 32
        };
        let mut numeric = true;
        let s = match conv {
            '%' => {
                out.push('%');
                continue;
            }
            'd' | 'i' => {
                let v = if long >= 2 { wide(&mut w) as i64 } else { next(&mut w) as i32 as i64 };
                if plus && v >= 0 {
                    format!("+{v}")
                } else {
                    v.to_string()
                }
            }
            'u' => if long >= 2 { wide(&mut w) } else { next(&mut w) as u64 }.to_string(),
            'x' => format!("{:x}", if long >= 2 { wide(&mut w) } else { next(&mut w) as u64 }),
            'X' => format!("{:X}", if long >= 2 { wide(&mut w) } else { next(&mut w) as u64 }),
            'o' => format!("{:o}", next(&mut w)),
            'p' => format!("{:#x}", next(&mut w)),
            'c' => {
                numeric = false;
                ((next(&mut w) as u8) as char).to_string()
            }
            'f' | 'F' | 'e' | 'g' | 'G' => {
                let v = f64::from_bits(wide(&mut w));
                match conv {
                    'e' => format!("{:.*e}", prec.unwrap_or(6), v),
                    'g' | 'G' => format!("{v}"),
                    _ => format!("{:.*}", prec.unwrap_or(6), v),
                }
            }
            's' => {
                numeric = false;
                let mut t = h.cstr(next(&mut w));
                if let Some(p) = prec {
                    t.truncate(p);
                }
                t
            }
            '@' => {
                numeric = false;
                describe(h, next(&mut w))
            }
            other => {
                numeric = false;
                format!("%{other}")
            }
        };
        let pad = width.saturating_sub(s.chars().count());
        if left {
            out += &s;
            out += &" ".repeat(pad);
        } else if zero && numeric {
            let (sign, digits) = if s.starts_with('-') || s.starts_with('+') { s.split_at(1) } else { ("", s.as_str()) };
            out += sign;
            out += &"0".repeat(pad);
            out += digits;
        } else {
            out += &" ".repeat(pad);
            out += &s;
        }
    }
    out
}

// ---------------------------------------------------------------- nib loading

fn nib_value(h: &Host, idx: u32, key: &str) -> Option<ValueVariant> {
    let nib = h.objc.nib.as_ref()?;
    let o = nib.arc.objects().get(idx as usize)?;
    o.values(nib.arc.values()).iter().find(|v| v.key(nib.arc.keys()) == key).map(|v| v.value().clone())
}

fn nib_class(h: &Host, idx: u32) -> String {
    let nib = h.objc.nib.as_ref().unwrap();
    nib.arc.objects()[idx as usize].class_name(nib.arc.class_names()).name().to_string()
}

/// The text of an NSString object in the archive.
fn nib_string(h: &Host, idx: u32) -> String {
    match nib_value(h, idx, "NS.bytes") {
        Some(ValueVariant::Data(d)) => String::from_utf8_lossy(&d).into_owned(),
        _ => String::new(),
    }
}

fn nib_ref_string(h: &Host, idx: u32, key: &str) -> String {
    match nib_value(h, idx, key) {
        Some(ValueVariant::ObjectRef(i)) => nib_string(h, i),
        _ => String::new(),
    }
}

/// Decodes the object stored under `key` in the object being decoded.
pub(crate) fn decode_object(h: &mut Host, c: &mut Cpu, key: &str) -> u32 {
    let Some(cur) = h.objc.nib.as_ref().map(|n| n.cur) else { return 0 };
    match nib_value(h, cur, key) {
        Some(ValueVariant::ObjectRef(i)) => unarchive(h, c, i),
        _ => 0,
    }
}

fn unarchive(h: &mut Host, c: &mut Cpu, idx: u32) -> u32 {
    if let Some(v) = h.objc.nib.as_ref().unwrap().done[idx as usize] {
        return v;
    }
    let set = |h: &mut Host, v: u32| h.objc.nib.as_mut().unwrap().done[idx as usize] = Some(v);
    let cn = nib_class(h, idx);
    if std::env::var_os("REVS_NIB_LOG").is_some() {
        println!("nib object {idx}: {cn}");
    }
    let v = match cn.as_str() {
        "UIProxyObject" => {
            let id = nib_ref_string(h, idx, "UIProxiedObjectIdentifier");
            if id == "IBFilesOwner" {
                h.objc.nib.as_ref().unwrap().owner
            } else {
                0
            }
        }
        "NSString" | "NSMutableString" => {
            let s = nib_string(h, idx);
            new_string(h, s)
        }
        "NSArray" | "NSMutableArray" => {
            let refs: Vec<u32> = {
                let nib = h.objc.nib.as_ref().unwrap();
                let o = &nib.arc.objects()[idx as usize];
                o.values(nib.arc.values())
                    .iter()
                    .filter(|v| v.key(nib.arc.keys()) == "UINibEncoderEmptyKey")
                    .filter_map(|v| if let ValueVariant::ObjectRef(i) = v.value() { Some(*i) } else { None })
                    .collect()
            };
            let a = new_array(h, Vec::new());
            set(h, a);
            let items: Vec<u32> = refs.into_iter().map(|i| unarchive(h, c, i)).collect();
            h.objc.objs.insert(a, Obj::Array(items));
            a
        }
        "UICustomObject" => {
            let name = nib_ref_string(h, idx, "UIClassName");
            let cls = class_named(h, &name);
            let o = send(h, c, cls, "alloc", &[]);
            set(h, o);
            send(h, c, o, "init", &[])
        }
        // A custom object that was class-swapped gets plain init (as UIKit/touchHLE do).
        "UIClassSwapper" if nib_ref_string(h, idx, "UIOriginalClassName") == "UICustomObject" => {
            let name = nib_ref_string(h, idx, "UIClassName");
            let cls = class_named(h, &name);
            let o = send(h, c, cls, "alloc", &[]);
            set(h, o);
            send(h, c, o, "init", &[])
        }
        _ => {
            let name = if cn == "UIClassSwapper" { nib_ref_string(h, idx, "UIClassName") } else { cn };
            let cls = class_named(h, &name);
            let o = send(h, c, cls, "alloc", &[]);
            set(h, o);
            let (saved, dec) = {
                let n = h.objc.nib.as_mut().unwrap();
                (std::mem::replace(&mut n.cur, idx), n.decoder)
            };
            let r = send(h, c, o, "initWithCoder:", &[dec]);
            h.objc.nib.as_mut().unwrap().cur = saved;
            r
        }
    };
    set(h, v);
    v
}

/// Byte offset of instance variable `name` (or `_name`) in `cls` or a superclass.
fn find_ivar(h: &Host, mut cls: u32, name: &str) -> Option<u32> {
    let alt = format!("_{name}");
    let mut depth = 0;
    while cls != 0 && depth < 64 {
        let list = h.rd32(ro(h, cls) + 28);
        if list != 0 && !h.objc.native.contains_key(&cls) {
            let (entsize, count) = (h.rd32(list), h.rd32(list + 4));
            for k in 0..count {
                let e = list + 8 + k * entsize;
                let n = h.cstr(h.rd32(e + 4));
                if n == name || n == alt {
                    return Some(h.rd32(h.rd32(e)));
                }
            }
        }
        cls = superclass(h, cls);
        depth += 1;
    }
    None
}

/// Connects an outlet like UIKit's setValue:forKey: (setter, else the ivar).
fn connect(h: &mut Host, c: &mut Cpu, src: u32, dst: u32, label: &str) {
    if src == 0 || label.is_empty() {
        return;
    }
    let setter = format!("set{}{}:", label[..1].to_uppercase(), &label[1..]);
    let what = if responds(h, src, &setter) {
        send(h, c, src, &setter, &[dst]);
        "setter"
    } else if let Some(off) = find_ivar(h, isa(h, src), label) {
        h.wr32(src + off, dst);
        "ivar"
    } else {
        "NOT FOUND"
    };
    let line = format!(
        "nib outlet {}.{label} = {} ({what})",
        class_name(h, isa(h, src)),
        if dst == 0 { "nil".into() } else { class_name(h, isa(h, dst)) }
    );
    h.objc.log.push(line);
}

fn load_nib(h: &mut Host, c: &mut Cpu, name: &str, owner: u32) -> Result<(), String> {
    let data = revs_analyze::ipa::read_app_file(&h.objc.ipa, &format!("{name}.nib"))?;
    let arc = NIBArchive::from_bytes(&data).map_err(|e| format!("{name}.nib: {e:?}"))?;
    let n = arc.objects().len();
    let dcls = native_class(h, "UINibDecoder");
    let decoder = alloc_instance(h, dcls);
    h.objc.nib = Some(Nib { arc, cur: 0, done: vec![None; n], owner, decoder });
    let _objects = decode_object(h, c, "UINibObjectsKey");
    let conns = decode_object(h, c, "UINibConnectionsKey");
    let list = match h.objc.objs.get(&conns) {
        Some(Obj::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    for k in list {
        if let Some(Obj::Conn { src, dst, label }) = h.objc.objs.get(&k) {
            let (s, d, l) = (*src, *dst, label.clone());
            connect(h, c, s, d, &l);
        }
    }
    h.objc.log.push(format!("loaded {name}.nib: {n} archived objects"));
    h.objc.nib = None;
    Ok(())
}

fn main_nib(h: &Host) -> Option<String> {
    let data = revs_analyze::ipa::read_app_file(&h.objc.ipa, "Info.plist").ok()?;
    let v: plist::Value = plist::from_bytes(&data).ok()?;
    v.as_dictionary()?.get("NSMainNibFile")?.as_string().map(String::from)
}

fn ui_application_main(h: &mut Host, c: &mut Cpu) {
    let delegate_name = string_of(h, c.r[3]);
    let cls = native_class(h, "UIApplication");
    let app = send(h, c, cls, "alloc", &[]);
    let app = send(h, c, app, "init", &[]);
    h.objc.app = app;
    if let Some(n) = delegate_name {
        let dc = class_named(h, &n);
        let d = send(h, c, dc, "alloc", &[]);
        h.objc.delegate = send(h, c, d, "init", &[]);
    } else if let Some(nib) = main_nib(h) {
        if let Err(e) = load_nib(h, c, &nib, app) {
            h.objc.log.push(format!("main nib: {e}"));
        }
    }
    if c.halt != 0 {
        return;
    }
    let d = h.objc.delegate;
    let dn = class_name(h, if d == 0 { 0 } else { isa(h, d) });
    h.objc.log.push(format!("UIApplicationMain: delegate {dn}"));
    if responds(h, d, "application:didFinishLaunchingWithOptions:") {
        h.objc.log.push(format!("sending -[{dn} application:didFinishLaunchingWithOptions:]"));
        send(h, c, d, "application:didFinishLaunchingWithOptions:", &[app, 0]);
    } else if responds(h, d, "applicationDidFinishLaunching:") {
        h.objc.log.push(format!("sending -[{dn} applicationDidFinishLaunching:]"));
        send(h, c, d, "applicationDidFinishLaunching:", &[app]);
    }
    if c.halt != 0 {
        return;
    }
    h.objc.log.push("finished launching; running the run loop".into());
    crate::uikit::run_loop(h, c);
}

/// Floats stored under `key` in the nib object being decoded (CGPoint: 2,
/// CGRect: 4): a type byte (6 = floats) then little-endian floats.
pub(crate) fn nib_floats(h: &Host, key: &str, n: usize) -> Option<Vec<f32>> {
    let cur = h.objc.nib.as_ref()?.cur;
    match nib_value(h, cur, key)? {
        ValueVariant::Data(d) if d.len() >= 1 + 4 * n => {
            Some((0..n).map(|i| f32::from_le_bytes(d[1 + 4 * i..5 + 4 * i].try_into().unwrap())).collect())
        }
        _ => None,
    }
}

pub(crate) fn nib_bool(h: &Host, key: &str) -> bool {
    let Some(cur) = h.objc.nib.as_ref().map(|n| n.cur) else { return false };
    matches!(nib_value(h, cur, key), Some(ValueVariant::Bool(true)))
}

/// isKindOfClass: for native code.
pub(crate) fn is_kind_of(h: &Host, o: u32, cls: u32) -> bool {
    let mut k = isa(h, o);
    for _ in 0..64 {
        if k == 0 {
            return false;
        }
        if k == cls {
            return true;
        }
        k = superclass(h, k);
    }
    false
}

// ---------------------------------------------------------------- C functions

/// ObjC/Foundation/UIKit C functions handled here.
pub const NATIVE: &[&str] = &[
    "objc_msgSend", "objc_msgSend_stret", "objc_msgSendSuper2", "objc_getProperty", "objc_setProperty",
    "objc_copyStruct", "objc_sync_enter", "objc_sync_exit", "objc_enumerationMutation", "NSClassFromString", "NSLog",
    "UIApplicationMain",
];

pub fn call(h: &mut Host, c: &mut Cpu, name: &str) -> bool {
    match name {
        "objc_msgSend" => dispatch(h, c, Kind::Normal),
        "objc_msgSend_stret" => dispatch(h, c, Kind::Stret),
        "objc_msgSendSuper2" => dispatch(h, c, Kind::Super2),
        // objc_getProperty(self, _cmd, offset, atomic)
        "objc_getProperty" => c.r[0] = h.rd32(c.r[0].wrapping_add(c.r[2])),
        // objc_setProperty(self, _cmd, offset, value, atomic, shouldCopy)
        "objc_setProperty" => {
            let (o, off, mut v) = (c.r[0], c.r[2], c.r[3]);
            if word(h, c, 5) & 0xFF != 0 && v != 0 {
                v = send(h, c, v, "copy", &[]);
            }
            h.wr32(o.wrapping_add(off), v);
        }
        // objc_copyStruct(dest, src, size, atomic, hasStrong)
        "objc_copyStruct" => unsafe { std::ptr::copy(h.at(c.r[1]), h.at(c.r[0]), c.r[2] as usize) },
        "objc_sync_enter" | "objc_sync_exit" => c.r[0] = 0,
        "objc_enumerationMutation" => {
            let line = format!("objc_enumerationMutation on {:#x} (collection changed while enumerating)", c.r[0]);
            h.objc.log.push(line);
        }
        "NSClassFromString" => {
            let n = string_of(h, c.r[0]).unwrap_or_default();
            c.r[0] = match h.objc.by_name.get(&n) {
                Some(&k) => k,
                None if parent(&n).is_some() => native_class(h, &n),
                None => 0,
            };
        }
        "NSLog" => {
            let f = string_of(h, c.r[0]).unwrap_or_default();
            let line = format!("NSLog: {}", format(h, c, &f, 1));
            println!("{line}");
        }
        "UIApplicationMain" => {
            ui_application_main(h, c);
            c.r[0] = 0;
        }
        _ => return false,
    }
    true
}

// ---------------------------------------------------------------- native methods

fn native_imp(class: &str, meta: bool, s: &str) -> Option<NativeImp> {
    Some(match (class, meta, s) {
        // NSObject, class side
        ("NSObject", true, "alloc" | "allocWithZone:") => |h, _, m| alloc_instance(h, m.this),
        ("NSObject", true, "new") => |h, c, m| {
            let o = alloc_instance(h, m.this);
            send(h, c, o, "init", &[])
        },
        ("NSObject", true, "initialize" | "load") => |_, _, _| 0,
        ("NSObject", true, "class") => |_, _, m| m.this,
        ("NSObject", true, "superclass") => |h, _, m| superclass(h, m.this),
        ("NSObject", true, "instancesRespondToSelector:") => |h, c, m| {
            let s = m.arg(h, c, 0);
            lookup(h, m.this, s).is_some() as u32
        },
        ("NSObject", true, "isSubclassOfClass:") => |h, c, m| {
            let want = m.arg(h, c, 0);
            let mut k = m.this;
            while k != 0 && k != want {
                k = superclass(h, k);
            }
            (k != 0) as u32
        },
        // NSObject, instance side (also reached by classes, through the root metaclass)
        ("NSObject", false, "init" | "retain" | "autorelease" | "self" | "copy") => |_, _, m| m.this,
        ("NSObject", false, "release" | "dealloc") => |_, _, _| 0,
        ("NSObject", false, "retainCount") => |_, _, _| 1,
        ("NSObject", false, "class") => |h, _, m| isa(h, m.this),
        ("NSObject", false, "hash") => |_, _, m| m.this,
        ("NSObject", false, "isEqual:") => |h, c, m| (m.arg(h, c, 0) == m.this) as u32,
        ("NSObject", false, "zone") => |_, _, _| 0,
        ("NSObject", false, "conformsToProtocol:") => |_, _, _| 0,
        ("NSObject", false, "respondsToSelector:") => |h, c, m| {
            let s = m.arg(h, c, 0);
            let cls = isa(h, m.this);
            lookup(h, cls, s).is_some() as u32
        },
        ("NSObject", false, "isKindOfClass:") => |h, c, m| {
            let want = m.arg(h, c, 0);
            let mut k = isa(h, m.this);
            while k != 0 && k != want {
                k = superclass(h, k);
            }
            (k != 0) as u32
        },
        ("NSObject", false, "isMemberOfClass:") => |h, c, m| (isa(h, m.this) == m.arg(h, c, 0)) as u32,
        ("NSObject", false, "performSelector:") => |h, c, m| {
            let s = m.arg(h, c, 0);
            let n = sel_name(h, s);
            send(h, c, m.this, &n, &[])
        },
        ("NSObject", false, "performSelector:withObject:") => |h, c, m| {
            let (s, a) = (m.arg(h, c, 0), m.arg(h, c, 1));
            let n = sel_name(h, s);
            send(h, c, m.this, &n, &[a])
        },
        ("NSObject", false, "description") => |h, _, m| {
            let d = describe(h, m.this);
            new_string(h, d)
        },
        // NSAutoreleasePool: objects are never freed yet, so draining is a no-op.
        ("NSAutoreleasePool", false, "drain") => |_, _, _| 0,
        ("NSAutoreleasePool", true, "addObject:") => |_, _, _| 0,
        // NSString
        ("NSString", true, "string") => |h, _, _| new_string(h, String::new()),
        ("NSString", true, "stringWithUTF8String:" | "stringWithCString:" | "stringWithCString:encoding:") => {
            |h, c, m| {
                let s = h.cstr(m.arg(h, c, 0));
                new_string(h, s)
            }
        }
        ("NSString", true, "stringWithString:") => |h, c, m| {
            let s = string_of(h, m.arg(h, c, 0)).unwrap_or_default();
            new_string(h, s)
        },
        ("NSString", true, "stringWithFormat:") => |h, c, m| {
            let f = string_of(h, m.arg(h, c, 0)).unwrap_or_default();
            let s = format(h, c, &f, m.first + 1);
            new_string(h, s)
        },
        ("NSString", false, "initWithUTF8String:" | "initWithCString:" | "initWithCString:encoding:") => {
            |h, c, m| {
                let s = h.cstr(m.arg(h, c, 0));
                h.objc.objs.insert(m.this, Obj::Str(s));
                m.this
            }
        }
        ("NSString", false, "initWithString:") => |h, c, m| {
            let s = string_of(h, m.arg(h, c, 0)).unwrap_or_default();
            h.objc.objs.insert(m.this, Obj::Str(s));
            m.this
        },
        ("NSString", false, "initWithFormat:") => |h, c, m| {
            let f = string_of(h, m.arg(h, c, 0)).unwrap_or_default();
            let s = format(h, c, &f, m.first + 1);
            h.objc.objs.insert(m.this, Obj::Str(s));
            m.this
        },
        ("NSString", false, "UTF8String" | "cString" | "cStringUsingEncoding:") => |h, _, m| {
            if let Some(&p) = h.objc.utf8.get(&m.this) {
                return p;
            }
            let s = string_of(h, m.this).unwrap_or_default();
            let p = crate::os::alloc_zeroed(h, s.len() as u32 + 1);
            for (k, b) in s.bytes().enumerate() {
                h.wr8(p + k as u32, b);
            }
            h.objc.utf8.insert(m.this, p);
            p
        },
        ("NSString", false, "length") => {
            |h, _, m| string_of(h, m.this).map_or(0, |s| s.encode_utf16().count() as u32)
        }
        ("NSString", false, "characterAtIndex:") => |h, c, m| {
            let i = m.arg(h, c, 0) as usize;
            string_of(h, m.this).and_then(|s| s.encode_utf16().nth(i)).unwrap_or(0) as u32
        },
        ("NSString", false, "isEqualToString:" | "isEqual:") => |h, c, m| {
            let o = m.arg(h, c, 0);
            (string_of(h, o).is_some() && string_of(h, o) == string_of(h, m.this)) as u32
        },
        ("NSString", false, "hash") => |h, _, m| {
            string_of(h, m.this).map_or(0, |s| s.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32)))
        },
        ("NSString", false, "description" | "copy") => |_, _, m| m.this,
        ("NSString", false, "stringByAppendingString:") => |h, c, m| {
            let s = string_of(h, m.this).unwrap_or_default() + &string_of(h, m.arg(h, c, 0)).unwrap_or_default();
            new_string(h, s)
        },
        ("NSString", false, "intValue" | "integerValue") => |h, _, m| {
            let s = string_of(h, m.this).unwrap_or_default();
            let t: String = s.trim().chars().take_while(|ch| ch.is_ascii_digit() || *ch == '-').collect();
            t.parse::<i32>().unwrap_or(0) as u32
        },
        // A 64-bit return: low word in r0, high word in r1. GameKit peer IDs
        // ("292328603") reach the game's C++ wrapper this way.
        ("NSString", false, "longLongValue") => |h, c, m| {
            let s = string_of(h, m.this).unwrap_or_default();
            let t = s.trim_start();
            let (sign, digits) = match t.strip_prefix('-') {
                Some(rest) => (-1i64, rest),
                None => (1, t.strip_prefix('+').unwrap_or(t)),
            };
            let d: String = digits.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            let v = d.parse::<i64>().map(|n| n * sign).unwrap_or(0) as u64;
            c.r[1] = (v >> 32) as u32;
            v as u32
        },
        ("NSString", false, "floatValue" | "doubleValue") => |h, c, m| {
            // Leading number like strtod: "1.5abc" -> 1.5, no number -> 0.
            let s = string_of(h, m.this).unwrap_or_default();
            let t = s.trim_start();
            let end = (0..=t.len()).rev().find(|&i| t.is_char_boundary(i) && t[..i].parse::<f64>().is_ok()).unwrap_or(0);
            let v = t[..end].parse::<f64>().unwrap_or(0.0);
            if sel_name(h, m.sel) == "doubleValue" {
                let b = v.to_bits();
                c.r[1] = (b >> 32) as u32;
                b as u32
            } else {
                (v as f32).to_bits()
            }
        },
        // NSArray
        ("NSArray", false, "count") => |h, _, m| match h.objc.objs.get(&m.this) {
            Some(Obj::Array(a)) => a.len() as u32,
            _ => 0,
        },
        // +array: an empty NSMutableArray (an NSArray too). The game's GKSession
        // wrapper builds the peer list for sendData:toPeers: with it.
        ("NSArray" | "NSMutableArray", true, "array") => |h, _, _| {
            crate::uikit::new_obj(h, "NSMutableArray", Obj::Array(Vec::new()))
        },
        ("NSArray", false, "addObject:") => |h, c, m| {
            let o = m.arg(h, c, 0);
            if let Some(Obj::Array(a)) = h.objc.objs.get_mut(&m.this) {
                a.push(o);
            }
            0
        },
        ("NSArray", false, "removeAllObjects") => |h, _, m| {
            if let Some(Obj::Array(a)) = h.objc.objs.get_mut(&m.this) {
                a.clear();
            }
            0
        },
        ("NSArray", false, "objectAtIndex:") => |h, c, m| {
            let i = m.arg(h, c, 0) as usize;
            match h.objc.objs.get(&m.this) {
                Some(Obj::Array(a)) => a.get(i).copied().unwrap_or(0),
                _ => 0,
            }
        },
        // UIApplication
        ("UIApplication", true, "sharedApplication") => |h, _, _| h.objc.app,
        ("UIApplication", false, "delegate") => |h, _, _| h.objc.delegate,
        ("UIApplication", false, "setDelegate:") => |h, c, m| {
            h.objc.delegate = m.arg(h, c, 0);
            0
        },
        // Nib loading: objects decoded from a nib.
        ("UIResponder" | "UIColor", false, "initWithCoder:") => |_, _, m| m.this,
        ("UIRuntimeOutletConnection", false, "initWithCoder:") => |h, c, m| {
            let src = decode_object(h, c, "UISource");
            let dst = decode_object(h, c, "UIDestination");
            let l = decode_object(h, c, "UILabel");
            let label = string_of(h, l).unwrap_or_default();
            h.objc.objs.insert(m.this, Obj::Conn { src, dst, label });
            m.this
        },
        ("UINibDecoder", false, "decodeObjectForKey:") => |h, c, m| {
            let k = string_of(h, m.arg(h, c, 0)).unwrap_or_default();
            decode_object(h, c, &k)
        },
        ("UINibDecoder", false, "containsValueForKey:") => |h, c, m| {
            let k = string_of(h, m.arg(h, c, 0)).unwrap_or_default();
            let cur = h.objc.nib.as_ref().map_or(0, |n| n.cur);
            nib_value(h, cur, &k).is_some() as u32
        },
        ("UINibDecoder", false, "decodeBoolForKey:" | "decodeIntegerForKey:" | "decodeIntForKey:" | "decodeFloatForKey:") => {
            |h, c, m| {
                let k = string_of(h, m.arg(h, c, 0)).unwrap_or_default();
                let cur = h.objc.nib.as_ref().map_or(0, |n| n.cur);
                let float = sel_name(h, m.sel) == "decodeFloatForKey:";
                match nib_value(h, cur, &k) {
                    Some(ValueVariant::Bool(b)) => b as u32,
                    Some(ValueVariant::Int8(i)) => i as u32,
                    Some(ValueVariant::Int16(i)) => i as u32,
                    Some(ValueVariant::Int32(i)) => i as u32,
                    Some(ValueVariant::Int64(i)) => i as u32,
                    Some(ValueVariant::Float(f)) if float => f.to_bits(),
                    Some(ValueVariant::Double(d)) if float => (d as f32).to_bits(),
                    Some(ValueVariant::Float(f)) => f as i32 as u32,
                    _ => 0,
                }
            }
        }
        // CGPoint / CGSize / CGRect: stored as a type byte (6 = floats) and floats.
        ("UINibDecoder", false, "decodeCGPointForKey:" | "decodeCGSizeForKey:" | "decodeCGRectForKey:") => {
            |h, c, m| {
                let dest = c.r[0];
                let k = string_of(h, m.arg(h, c, 0)).unwrap_or_default();
                let cur = h.objc.nib.as_ref().map_or(0, |n| n.cur);
                let n = if sel_name(h, m.sel) == "decodeCGRectForKey:" { 4 } else { 2 };
                let d = match nib_value(h, cur, &k) {
                    Some(ValueVariant::Data(d)) => d,
                    _ => Vec::new(),
                };
                for i in 0..n {
                    let b = d.get(1 + 4 * i..5 + 4 * i).map_or([0; 4], |s| s.try_into().unwrap());
                    h.wr32(dest + 4 * i as u32, u32::from_le_bytes(b));
                }
                0
            }
        }
        _ => return crate::uikit::native_imp(class, meta, s),
    })
}

// ---------------------------------------------------------------- report

pub fn report(h: &Host) {
    let r = &h.objc;
    println!("\nObjective-C: {} game classes, {} Apple classes made natively", r.game_classes, r.native.len() / 2);
    for l in &r.log {
        println!("  {l}");
    }
    let mut m: Vec<(&String, &Missing)> = r.missing.iter().collect();
    m.sort_by_key(|x| x.1.order);
    println!("Methods with no native version yet: {} (first-use order):", m.len());
    for (k, v) in m {
        println!("  {:>3} {:>8} calls  first from {:#x}  {k}  (receiver {})", v.order, v.calls, v.first_lr, v.receiver);
    }
}
