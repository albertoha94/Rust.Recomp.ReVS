//! Files for the game (Phase 3.5).
//!
//! The guest sees an iOS app sandbox:
//! - `HOME/<App>.app/...`: the app bundle, read-only, read straight from the
//!   user's own .ipa (nothing is extracted to disk);
//! - `HOME/Documents`, `HOME/Library/...`, `HOME/tmp`: the port's own folder on
//!   the host (`--home=`, default `build/home`). It never touches a touchHLE save.
//!
//! POSIX calls (open/read/write/lseek/stat/...) and stdio (fopen/fread/...)
//! work on one descriptor table. A `FILE` is a guest block of Darwin's size
//! with the descriptor in `_file` (offset 14), like the real one.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::rc::Rc;

use revs_analyze::ipa::Bundle;

use crate::os::arg;
use crate::{Cpu, Host};

/// The app's home folder as the game sees it.
pub const HOME: &str = "/User/Applications/00000000-0000-0000-0000-RE-VS-PORT00";

const ENOENT: u32 = 2;
const EIO: u32 = 5;
const EBADF: u32 = 9;
const EACCES: u32 = 13;
const EEXIST: u32 = 17;
const ENOTDIR: u32 = 20;
const EISDIR: u32 = 21;
const EINVAL: u32 = 22;
const ENOTSUP: u32 = 45;

const O_ACCMODE: u32 = 3;
const O_APPEND: u32 = 0x8;
const O_CREAT: u32 = 0x200;
const O_TRUNC: u32 = 0x400;
const O_EXCL: u32 = 0x800;

const S_IFDIR: u16 = 0o040000;
const S_IFREG: u16 = 0o100000;

/// Every name `call` handles (for linking).
pub const NATIVE: &[&str] = &[
    "open", "close", "read", "write", "pread", "pwrite", "lseek", "stat", "lstat", "fstat", "access", "unlink",
    "mkdir", "rmdir", "rename", "fsync", "ftruncate", "fcntl", "flock", "fchmod", "utimes", "fsctl", "statfs",
    "getcwd", "__error", "fopen", "fclose", "fread", "fwrite", "fseek", "ftell", "rewind", "feof", "ferror",
    "clearerr", "fflush", "fileno", "fgets", "fgetc", "getc", "fputc", "putc", "fputs", "fprintf", "vfprintf",
    "printf", "vprintf", "puts", "putchar", "sprintf", "snprintf", "vsprintf", "vsnprintf", "strerror",
    "NSHomeDirectory", "NSTemporaryDirectory", "NSSearchPathForDirectoriesInDomains",
];

enum Data {
    /// A bundle file (shared between descriptors).
    Mem(Rc<Vec<u8>>),
    /// A folder (opened read-only, e.g. by SQLite to fsync it).
    Dir,
    Host(std::fs::File),
    /// stdin / stdout / stderr.
    Console,
}

struct Open {
    data: Data,
    pos: u64,
    append: bool,
    writable: bool,
    eof: bool,
    err: bool,
}

/// Where a guest path lives.
enum Node {
    /// Relative path inside the bundle ("" = the .app folder).
    Bundle(String),
    /// A file or folder in the port's home folder on the host.
    Home(PathBuf),
    /// A folder above HOME ("/", "/User", ...): exists, nothing in it.
    Above,
    Outside,
}

pub struct Fs {
    bundle: Option<Bundle>,
    /// `HOME/<App>.app`.
    pub bundle_path: String,
    pub home: PathBuf,
    files: HashMap<u32, Open>,
    cache: HashMap<String, Rc<Vec<u8>>>,
    errno: u32,
    errno_addr: u32,
    strerror_buf: u32,
    /// Opens that failed (path, errno), first ones only: what the game looked for.
    pub misses: Vec<(String, u32)>,
    pub opened: u32,
    pub guest_out: Vec<String>,
}

impl Fs {
    pub fn new() -> Fs {
        let mut files = HashMap::new();
        for fd in 0..3 {
            files.insert(fd, Open { data: Data::Console, pos: 0, append: true, writable: fd != 0, eof: false, err: false });
        }
        Fs {
            bundle: None,
            bundle_path: String::new(),
            home: PathBuf::new(),
            files,
            cache: HashMap::new(),
            errno: 0,
            errno_addr: 0,
            strerror_buf: 0,
            misses: Vec::new(),
            opened: 0,
            guest_out: Vec::new(),
        }
    }

    /// Opens the user's .ipa as the bundle and makes the home folders.
    pub fn mount(&mut self, ipa: &std::path::Path, home: PathBuf) -> Result<(), String> {
        let b = Bundle::open(ipa)?;
        self.bundle_path = format!("{HOME}/{}", b.app);
        self.bundle = Some(b);
        for d in ["Documents", "Library/Caches", "Library/Preferences", "tmp"] {
            std::fs::create_dir_all(home.join(d)).map_err(|e| format!("{}: {e}", home.join(d).display()))?;
        }
        self.home = home;
        Ok(())
    }

    /// The whole contents of a bundle file (for NSBundle/NSData and nibs).
    pub fn bundle_file(&mut self, rel: &str) -> Option<Rc<Vec<u8>>> {
        if let Some(d) = self.cache.get(rel) {
            return Some(d.clone());
        }
        let data = Rc::new(self.bundle.as_mut()?.read(rel).ok()?);
        // Keep small files; big ones (movies, sound banks) are read again when needed.
        if data.len() < 16 << 20 {
            self.cache.insert(rel.to_string(), data.clone());
        }
        Some(data)
    }

    fn normalize(p: &str) -> String {
        let abs = if p.starts_with('/') { p.to_string() } else { format!("{HOME}/{p}") };
        let mut parts: Vec<&str> = Vec::new();
        for c in abs.split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                c => parts.push(c),
            }
        }
        format!("/{}", parts.join("/"))
    }

    fn node(&self, p: &str) -> Node {
        let n = Self::normalize(p);
        let under = |root: &str| -> Option<String> {
            if n == root {
                Some(String::new())
            } else {
                n.strip_prefix(root).and_then(|r| r.strip_prefix('/')).map(str::to_string)
            }
        };
        if !self.bundle_path.is_empty() {
            if let Some(r) = under(&self.bundle_path) {
                return Node::Bundle(r);
            }
        }
        if let Some(r) = under(HOME) {
            return Node::Home(if r.is_empty() { self.home.clone() } else { self.home.join(r) });
        }
        if HOME.starts_with(&n) && (n == "/" || HOME[n.len()..].starts_with('/')) {
            return Node::Above;
        }
        Node::Outside
    }

    /// (is folder, size, mtime seconds) or errno.
    fn stat_path(&self, p: &str) -> Result<(bool, u64, u64), u32> {
        match self.node(p) {
            Node::Bundle(r) => {
                let b = self.bundle.as_ref().ok_or(ENOENT)?;
                if let Some(s) = b.file_size(&r) {
                    Ok((false, s, 0))
                } else if b.is_dir(&r) {
                    Ok((true, 0, 0))
                } else {
                    Err(ENOENT)
                }
            }
            Node::Home(h) => {
                let m = std::fs::metadata(&h).map_err(|_| ENOENT)?;
                let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
                Ok((m.is_dir(), if m.is_dir() { 0 } else { m.len() }, t))
            }
            Node::Above => Ok((true, 0, 0)),
            Node::Outside => Err(ENOENT),
        }
    }

    /// Does `p` exist, and is it a folder? (for NSFileManager)
    pub fn exists(&self, p: &str) -> Option<bool> {
        self.stat_path(p).ok().map(|s| s.0)
    }

    /// Names inside a folder (for NSFileManager).
    pub fn list(&self, p: &str) -> Option<Vec<String>> {
        match self.node(p) {
            Node::Bundle(r) => self.bundle.as_ref().filter(|b| b.is_dir(&r)).map(|b| b.list(&r)),
            Node::Home(h) => std::fs::read_dir(h)
                .ok()
                .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect()),
            _ => None,
        }
    }

    /// Reads a whole file (for NSData/NSString contentsOfFile:).
    pub fn read_all(&mut self, p: &str) -> Option<Vec<u8>> {
        match self.node(p) {
            Node::Bundle(r) => self.bundle_file(&r).map(|d| d.as_ref().clone()),
            Node::Home(h) => std::fs::read(h).ok(),
            _ => None,
        }
    }

    /// Writes a whole file in the home folder (for NSData writeToFile:).
    pub fn write_all(&mut self, p: &str, data: &[u8]) -> bool {
        match self.node(p) {
            Node::Home(h) => std::fs::write(h, data).is_ok(),
            _ => false,
        }
    }

    pub fn mkdir_all(&mut self, p: &str) -> bool {
        match self.node(p) {
            Node::Home(h) => std::fs::create_dir_all(h).is_ok(),
            Node::Above => true,
            Node::Bundle(r) => self.bundle.as_ref().is_some_and(|b| b.is_dir(&r)),
            Node::Outside => false,
        }
    }

    pub fn remove(&mut self, p: &str) -> bool {
        match self.node(p) {
            Node::Home(h) if h != self.home => {
                if h.is_dir() { std::fs::remove_dir_all(h).is_ok() } else { std::fs::remove_file(h).is_ok() }
            }
            _ => false,
        }
    }

    pub(crate) fn open(&mut self, p: &str, flags: u32) -> Result<u32, u32> {
        let acc = flags & O_ACCMODE;
        let writable = acc != 0;
        let data = match self.node(p) {
            Node::Bundle(r) => {
                if writable || flags & (O_CREAT | O_TRUNC) != 0 {
                    return Err(EACCES);
                }
                let b = self.bundle.as_ref().ok_or(ENOENT)?;
                if b.file_size(&r).is_some() {
                    Data::Mem(self.bundle_file(&r).ok_or(EIO)?)
                } else if b.is_dir(&r) {
                    Data::Dir
                } else {
                    return Err(ENOENT);
                }
            }
            Node::Home(h) => {
                if h.is_dir() {
                    if writable {
                        return Err(EISDIR);
                    }
                    Data::Dir
                } else {
                    let mut o = std::fs::OpenOptions::new();
                    o.read(acc != 1).write(writable);
                    if flags & O_CREAT != 0 {
                        if flags & O_EXCL != 0 {
                            o.create_new(true);
                        } else {
                            o.create(true);
                        }
                    }
                    if flags & O_TRUNC != 0 {
                        o.truncate(true);
                    }
                    let f = o.open(&h).map_err(|e| match e.kind() {
                        std::io::ErrorKind::NotFound => ENOENT,
                        std::io::ErrorKind::AlreadyExists => EEXIST,
                        std::io::ErrorKind::PermissionDenied => EACCES,
                        _ => EIO,
                    })?;
                    Data::Host(f)
                }
            }
            Node::Above if !writable => Data::Dir,
            // Random bytes for OpenSSL's seeding.
            Node::Outside if !writable && matches!(p, "/dev/urandom" | "/dev/random") => {
                let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
                let v: Vec<u8> = (0..65536)
                    .map(|_| {
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
                        x as u8
                    })
                    .collect();
                Data::Mem(Rc::new(v))
            }
            Node::Above => return Err(EISDIR),
            Node::Outside => return Err(ENOENT),
        };
        let fd = (3..).find(|f| !self.files.contains_key(f)).unwrap();
        self.files.insert(fd, Open { data, pos: 0, append: flags & O_APPEND != 0, writable, eof: false, err: false });
        self.opened += 1;
        Ok(fd)
    }

    pub(crate) fn read(&mut self, fd: u32, buf: &mut [u8], at: Option<u64>) -> Result<usize, u32> {
        let o = self.files.get_mut(&fd).ok_or(EBADF)?;
        let pos = at.unwrap_or(o.pos);
        let n = match &mut o.data {
            Data::Mem(d) => {
                let start = (pos as usize).min(d.len());
                let n = buf.len().min(d.len() - start);
                buf[..n].copy_from_slice(&d[start..start + n]);
                n
            }
            Data::Host(f) => {
                f.seek(SeekFrom::Start(pos)).map_err(|_| EIO)?;
                let mut n = 0;
                while n < buf.len() {
                    match f.read(&mut buf[n..]) {
                        Ok(0) => break,
                        Ok(k) => n += k,
                        Err(_) => return Err(EIO),
                    }
                }
                n
            }
            Data::Console => 0,
            Data::Dir => return Err(EISDIR),
        };
        if at.is_none() {
            o.pos = pos + n as u64;
        }
        Ok(n)
    }

    pub(crate) fn write(&mut self, fd: u32, buf: &[u8], at: Option<u64>) -> Result<usize, u32> {
        let o = self.files.get_mut(&fd).ok_or(EBADF)?;
        if !o.writable {
            return Err(EBADF);
        }
        match &mut o.data {
            Data::Console => {
                let s = String::from_utf8_lossy(buf).into_owned();
                if self.guest_out.len() < 2000 {
                    self.guest_out.push(s);
                }
                Ok(buf.len())
            }
            Data::Mem(_) | Data::Dir => Err(EBADF),
            Data::Host(f) => {
                let pos = if o.append && at.is_none() { f.seek(SeekFrom::End(0)).map_err(|_| EIO)? } else { at.unwrap_or(o.pos) };
                f.seek(SeekFrom::Start(pos)).map_err(|_| EIO)?;
                f.write_all(buf).map_err(|_| EIO)?;
                if at.is_none() {
                    o.pos = pos + buf.len() as u64;
                }
                Ok(buf.len())
            }
        }
    }

    pub(crate) fn close(&mut self, fd: u32) {
        if fd > 2 {
            self.files.remove(&fd);
        }
    }

    pub(crate) fn pos(&self, fd: u32) -> u64 {
        self.files.get(&fd).map_or(0, |o| o.pos)
    }

    pub(crate) fn len(&self, fd: u32) -> Result<u64, u32> {
        match &self.files.get(&fd).ok_or(EBADF)?.data {
            Data::Mem(d) => Ok(d.len() as u64),
            Data::Host(f) => f.metadata().map(|m| m.len()).map_err(|_| EIO),
            Data::Console | Data::Dir => Ok(0),
        }
    }

    pub(crate) fn seek(&mut self, fd: u32, off: i64, whence: u32) -> Result<u64, u32> {
        let end = self.len(fd)? as i64;
        let o = self.files.get_mut(&fd).ok_or(EBADF)?;
        let base = match whence {
            0 => 0,
            1 => o.pos as i64,
            2 => end,
            _ => return Err(EINVAL),
        };
        let p = base + off;
        if p < 0 {
            return Err(EINVAL);
        }
        o.pos = p as u64;
        o.eof = false;
        Ok(o.pos)
    }
}

// ------------------------------------------------------------------ guest side

pub(crate) fn set_errno(h: &mut Host, e: u32) {
    h.fs.errno = e;
    let a = errno_addr(h);
    h.wr32(a, e);
}

/// The guest word `__error()` returns.
fn errno_addr(h: &mut Host) -> u32 {
    if h.fs.errno_addr == 0 {
        h.fs.errno_addr = crate::os::alloc_zeroed(h, 4);
    }
    h.fs.errno_addr
}

fn guest_slice<'a>(h: &Host, a: u32, n: u32) -> &'a mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(h.at(a), n as usize) }
}

fn write_stat(h: &Host, a: u32, dir: bool, size: u64, mtime: u64, ino: u64) {
    // Darwin struct stat (64-bit inodes, packed, 108 bytes).
    guest_slice(h, a, 108).fill(0);
    h.wr32(a, 1); // st_dev
    let mode = if dir { S_IFDIR | 0o755 } else { S_IFREG | 0o644 };
    h.wr8(a + 4, mode as u8);
    h.wr8(a + 5, (mode >> 8) as u8);
    h.wr8(a + 6, 1); // st_nlink
    h.wr32(a + 8, ino as u32);
    h.wr32(a + 12, (ino >> 32) as u32);
    h.wr32(a + 16, 501); // uid
    h.wr32(a + 20, 501); // gid
    for t in [28, 36, 44, 52] {
        h.wr32(a + t, mtime as u32);
    }
    h.wr32(a + 60, size as u32);
    h.wr32(a + 64, (size >> 32) as u32);
    let blocks = size.div_ceil(512);
    h.wr32(a + 68, blocks as u32);
    h.wr32(a + 72, (blocks >> 32) as u32);
    h.wr32(a + 76, 4096); // st_blksize
}

fn ino_of(p: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut s = std::collections::hash_map::DefaultHasher::new();
    Fs::normalize(p).hash(&mut s);
    s.finish() | 1
}

/// The descriptor of a guest FILE* (null or unknown: stderr).
fn file_fd(h: &Host, f: u32) -> u32 {
    if f == 0 || f >= 0xD000_0000 {
        return 2;
    }
    let fd = (h.rd8(f + 14) as u16 | (h.rd8(f + 15) as u16) << 8) as i16;
    if fd < 0 {
        2
    } else {
        fd as u32
    }
}

pub(crate) fn new_file(h: &mut Host, fd: u32) -> u32 {
    // Darwin FILE is 88 bytes; _file (short) at 14, _flags at 12.
    let f = crate::os::alloc_zeroed(h, 88);
    h.wr8(f + 12, 0x0C); // __SRD | __SWR
    h.wr8(f + 14, fd as u8);
    h.wr8(f + 15, (fd >> 8) as u8);
    f
}

/// printf-style formatting of the arguments from word `w`, or from a
/// `va_list` in guest memory.
fn fmt_args(h: &Host, c: &mut Cpu, fmt: u32, w: Option<u32>, va: u32) -> String {
    let f = h.cstr(fmt);
    match w {
        Some(w) => crate::objc::format(h, c, &f, w),
        None => {
            // Point the stack at the va_list: word 4 is then its first entry.
            let sp = c.r[13];
            c.r[13] = va;
            let s = crate::objc::format(h, c, &f, 4);
            c.r[13] = sp;
            s
        }
    }
}

/// Writes `s` NUL-terminated to `buf` (at most `n` bytes with the NUL; n=None:
/// no limit). Returns the full length, like snprintf.
fn put_cstr(h: &Host, buf: u32, n: Option<u32>, s: &str) -> u32 {
    let b = s.as_bytes();
    let k = match n {
        Some(0) => return b.len() as u32,
        Some(n) => b.len().min(n as usize - 1),
        None => b.len(),
    };
    if buf != 0 {
        guest_slice(h, buf, k as u32).copy_from_slice(&b[..k]);
        h.wr8(buf + k as u32, 0);
    }
    b.len() as u32
}

fn strerror_text(e: u32) -> &'static str {
    match e {
        0 => "Undefined error: 0",
        ENOENT => "No such file or directory",
        EIO => "Input/output error",
        EBADF => "Bad file descriptor",
        EACCES => "Permission denied",
        EEXIST => "File exists",
        ENOTDIR => "Not a directory",
        EISDIR => "Is a directory",
        EINVAL => "Invalid argument",
        ENOTSUP => "Operation not supported",
        _ => "Unknown error",
    }
}

fn fail(h: &mut Host, e: u32) -> u32 {
    set_errno(h, e);
    u32::MAX
}

fn note_miss(h: &mut Host, p: &str, e: u32) {
    if h.fs.misses.len() < 200 && !h.fs.misses.iter().any(|m| m.0 == p) {
        h.fs.misses.push((p.to_string(), e));
    }
}

/// Handles a file / stdio import. Returns false if `name` isn't one.
pub fn call(h: &mut Host, c: &mut Cpu, name: &str) -> bool {
    if !NATIVE.contains(&name) {
        return false;
    }
    let (a0, a1, a2, a3, a4) = (arg(h, c, 0), arg(h, c, 1), arg(h, c, 2), arg(h, c, 3), arg(h, c, 4));
    let r0: u32 = match name {
        "__error" => errno_addr(h),
        "strerror" => {
            let s = strerror_text(a0);
            if h.fs.strerror_buf == 0 {
                h.fs.strerror_buf = crate::os::alloc_zeroed(h, 64);
            }
            put_cstr(h, h.fs.strerror_buf, Some(64), s);
            h.fs.strerror_buf
        }
        "getcwd" => {
            let n = if a0 == 0 { 0 } else { a1 };
            if a0 == 0 || (HOME.len() as u32) >= n {
                fail(h, EINVAL);
                0
            } else {
                put_cstr(h, a0, Some(n), HOME);
                a0
            }
        }
        "open" => {
            let p = h.cstr(a0);
            match h.fs.open(&p, a1) {
                Ok(fd) => fd,
                Err(e) => {
                    note_miss(h, &p, e);
                    fail(h, e)
                }
            }
        }
        "close" => {
            if a0 > 2 && h.fs.files.remove(&a0).is_some() {
                0
            } else if a0 <= 2 {
                0
            } else {
                fail(h, EBADF)
            }
        }
        "read" | "pread" => {
            let at = (name == "pread").then(|| a3 as u64 | (a4 as u64) << 32);
            let buf = guest_slice(h, a1, a2);
            match h.fs.read(a0, buf, at) {
                Ok(n) => n as u32,
                Err(e) => fail(h, e),
            }
        }
        "write" | "pwrite" => {
            let at = (name == "pwrite").then(|| a3 as u64 | (a4 as u64) << 32);
            let buf = guest_slice(h, a1, a2);
            match h.fs.write(a0, buf, at) {
                Ok(n) => n as u32,
                Err(e) => fail(h, e),
            }
        }
        "lseek" => {
            // off_t in words 1-2, whence in word 3 (iOS: no register-pair alignment).
            let off = (a1 as u64 | (a2 as u64) << 32) as i64;
            match h.fs.seek(a0, off, a3) {
                Ok(p) => {
                    c.r[1] = (p >> 32) as u32;
                    p as u32
                }
                Err(e) => {
                    c.r[1] = u32::MAX;
                    fail(h, e)
                }
            }
        }
        "stat" | "lstat" => {
            let p = h.cstr(a0);
            match h.fs.stat_path(&p) {
                Ok((dir, size, t)) => {
                    write_stat(h, a1, dir, size, t, ino_of(&p));
                    0
                }
                Err(e) => {
                    note_miss(h, &p, e);
                    fail(h, e)
                }
            }
        }
        "fstat" => match h.fs.len(a0) {
            Ok(size) => {
                let dir = matches!(&h.fs.files[&a0].data, Data::Dir);
                write_stat(h, a1, dir, size, 0, 0x100 + a0 as u64);
                0
            }
            Err(e) => fail(h, e),
        },
        "access" => {
            let p = h.cstr(a0);
            match h.fs.stat_path(&p) {
                // W_OK (2) inside the bundle: read-only.
                Ok(_) if a1 & 2 != 0 && matches!(h.fs.node(&p), Node::Bundle(_)) => fail(h, EACCES),
                Ok(_) => 0,
                Err(e) => fail(h, e),
            }
        }
        "unlink" | "rmdir" => {
            let p = h.cstr(a0);
            match h.fs.node(&p) {
                Node::Home(f) => {
                    let r = if name == "rmdir" { std::fs::remove_dir(&f) } else { std::fs::remove_file(&f) };
                    if r.is_ok() { 0 } else { fail(h, ENOENT) }
                }
                Node::Bundle(_) | Node::Above => fail(h, EACCES),
                Node::Outside => fail(h, ENOENT),
            }
        }
        "mkdir" => {
            let p = h.cstr(a0);
            match h.fs.node(&p) {
                Node::Home(f) => match std::fs::create_dir(&f) {
                    Ok(()) => 0,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => fail(h, EEXIST),
                    Err(_) => fail(h, ENOENT),
                },
                Node::Bundle(_) | Node::Above => fail(h, EEXIST),
                Node::Outside => fail(h, EACCES),
            }
        }
        "rename" => {
            let (p, q) = (h.cstr(a0), h.cstr(a1));
            match (h.fs.node(&p), h.fs.node(&q)) {
                (Node::Home(f), Node::Home(g)) => {
                    if std::fs::rename(f, g).is_ok() { 0 } else { fail(h, ENOENT) }
                }
                _ => fail(h, EACCES),
            }
        }
        "ftruncate" => {
            let len = a1 as u64 | (a2 as u64) << 32;
            match h.fs.files.get(&a0).map(|o| &o.data) {
                Some(Data::Host(f)) => {
                    if f.set_len(len).is_ok() { 0 } else { fail(h, EIO) }
                }
                Some(_) => fail(h, EINVAL),
                None => fail(h, EBADF),
            }
        }
        "fsync" | "flock" | "fchmod" | "utimes" => 0,
        "fcntl" => match a1 {
            // F_GETLK: nobody else holds a lock (l_type = F_UNLCK at +20).
            7 => {
                h.wr8(a2 + 20, 2);
                h.wr8(a2 + 21, 0);
                0
            }
            _ => 0,
        },
        "fsctl" | "statfs" => fail(h, ENOTSUP),
        "NSHomeDirectory" => crate::objc::new_string(h, HOME.to_string()),
        "NSTemporaryDirectory" => crate::objc::new_string(h, format!("{HOME}/tmp/")),
        "NSSearchPathForDirectoriesInDomains" => crate::nsfile::search_path(h, a0, a2 & 0xFF != 0),

        // ---- stdio
        "fopen" => {
            let (p, mode) = (h.cstr(a0), h.cstr(a1));
            let plus = mode.contains('+');
            let flags = match mode.chars().next() {
                Some('r') => if plus { 2 } else { 0 },
                Some('w') => (if plus { 2 } else { 1 }) | O_CREAT | O_TRUNC,
                Some('a') => (if plus { 2 } else { 1 }) | O_CREAT | O_APPEND,
                _ => {
                    fail(h, EINVAL);
                    u32::MAX
                }
            };
            if flags == u32::MAX {
                0
            } else {
                match h.fs.open(&p, flags) {
                    Ok(fd) => new_file(h, fd),
                    Err(e) => {
                        note_miss(h, &p, e);
                        fail(h, e);
                        0
                    }
                }
            }
        }
        "fclose" => {
            let fd = file_fd(h, a0);
            if fd > 2 {
                h.fs.files.remove(&fd);
            }
            if a0 != 0 && a0 < 0xD000_0000 {
                let mut heap = std::mem::replace(&mut h.heap, crate::os::Heap::new(0, 0));
                heap.free(h, a0);
                h.heap = heap;
            }
            0
        }
        "fread" | "fwrite" => {
            let total = a1.saturating_mul(a2);
            let fd = file_fd(h, a3);
            let buf = guest_slice(h, a0, total);
            let r = if name == "fread" { h.fs.read(fd, buf, None) } else { h.fs.write(fd, buf, None) };
            match r {
                Ok(n) => {
                    if (n as u32) < total {
                        if let Some(o) = h.fs.files.get_mut(&fd) {
                            o.eof |= name == "fread";
                        }
                    }
                    if a1 == 0 { 0 } else { n as u32 / a1 }
                }
                Err(e) => {
                    if let Some(o) = h.fs.files.get_mut(&fd) {
                        o.err = true;
                    }
                    set_errno(h, e);
                    0
                }
            }
        }
        "fseek" => {
            let fd = file_fd(h, a0);
            match h.fs.seek(fd, a1 as i32 as i64, a2) {
                Ok(_) => 0,
                Err(e) => fail(h, e),
            }
        }
        "rewind" => {
            let fd = file_fd(h, a0);
            let _ = h.fs.seek(fd, 0, 0);
            0
        }
        "ftell" => {
            let fd = file_fd(h, a0);
            h.fs.files.get(&fd).map_or(u32::MAX, |o| o.pos as u32)
        }
        "feof" => h.fs.files.get(&file_fd(h, a0)).is_some_and(|o| o.eof) as u32,
        "ferror" => h.fs.files.get(&file_fd(h, a0)).is_some_and(|o| o.err) as u32,
        "clearerr" => {
            if let Some(o) = h.fs.files.get_mut(&file_fd(h, a0)) {
                o.eof = false;
                o.err = false;
            }
            0
        }
        "fflush" => 0,
        "fileno" => file_fd(h, a0),
        "fgetc" | "getc" => {
            let fd = file_fd(h, a0);
            let mut b = [0u8];
            match h.fs.read(fd, &mut b, None) {
                Ok(1) => b[0] as u32,
                _ => {
                    if let Some(o) = h.fs.files.get_mut(&fd) {
                        o.eof = true;
                    }
                    u32::MAX
                }
            }
        }
        "fgets" => {
            // Up to n-1 bytes, stopping after a newline.
            let fd = file_fd(h, a2);
            let mut out = Vec::new();
            let mut b = [0u8];
            while (out.len() as u32) + 1 < a1 {
                match h.fs.read(fd, &mut b, None) {
                    Ok(1) => {
                        out.push(b[0]);
                        if b[0] == b'\n' {
                            break;
                        }
                    }
                    _ => {
                        if let Some(o) = h.fs.files.get_mut(&fd) {
                            o.eof = true;
                        }
                        break;
                    }
                }
            }
            if out.is_empty() {
                0
            } else {
                guest_slice(h, a0, out.len() as u32).copy_from_slice(&out);
                h.wr8(a0 + out.len() as u32, 0);
                a0
            }
        }
        "fputc" | "putc" => {
            let fd = file_fd(h, a1);
            match h.fs.write(fd, &[a0 as u8], None) {
                Ok(_) => a0 & 0xFF,
                Err(_) => u32::MAX,
            }
        }
        "putchar" => {
            let _ = h.fs.write(1, &[a0 as u8], None);
            a0 & 0xFF
        }
        "fputs" | "puts" => {
            let mut s = h.cstr(a0).into_bytes();
            let fd = if name == "puts" {
                s.push(b'\n');
                1
            } else {
                file_fd(h, a1)
            };
            match h.fs.write(fd, &s, None) {
                Ok(_) => 0,
                Err(_) => u32::MAX,
            }
        }
        "fprintf" | "vfprintf" | "printf" | "vprintf" => {
            let s = match name {
                "fprintf" => fmt_args(h, c, a1, Some(2), 0),
                "vfprintf" => fmt_args(h, c, a1, None, a2),
                "printf" => fmt_args(h, c, a0, Some(1), 0),
                _ => fmt_args(h, c, a0, None, a1),
            };
            let fd = if name.ends_with("fprintf") { file_fd(h, a0) } else { 1 };
            let _ = h.fs.write(fd, s.as_bytes(), None);
            s.len() as u32
        }
        "sprintf" | "snprintf" | "vsprintf" | "vsnprintf" => {
            let s = match name {
                "sprintf" => fmt_args(h, c, a1, Some(2), 0),
                "snprintf" => fmt_args(h, c, a2, Some(3), 0),
                "vsprintf" => fmt_args(h, c, a1, None, a2),
                _ => fmt_args(h, c, a2, None, a3),
            };
            let n = name.contains('n').then_some(a1);
            put_cstr(h, a0, n, &s)
        }
        _ => return false,
    };
    c.r[0] = r0;
    true
}

/// Prints what the game opened and what it looked for but didn't find.
pub fn report(h: &Host) {
    println!("files: {} opened, {} not found (first ones):", h.fs.opened, h.fs.misses.len());
    for (p, e) in h.fs.misses.iter().take(25) {
        println!("    {} ({})", p, strerror_text(*e));
    }
    if !h.fs.guest_out.is_empty() {
        let all: String = h.fs.guest_out.concat();
        println!("game output (stdout/stderr), last lines:");
        for l in all.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev() {
            println!("    {l}");
        }
    }
}
