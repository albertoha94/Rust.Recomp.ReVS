//! Analysis tools for the RE Mercenaries VS. source port.
//!
//! RULE: everything here reads the game from the user's own .ipa, given on the
//! command line. Output goes to the git-ignored `build/` folder and must never
//! be committed or shared: it is derived from the game.
//!
//! Usage:
//!   revs-analyze functions <game.ipa | unpacked Payload folder> [out_dir]   (default: build)
//!     writes functions.json (function map + call graph), imports.json,
//!     objc_methods.json, vtables.json, switches.json and prints a summary.

mod code;
mod data;
mod objc;
mod recomp;
mod subsystems;

use revs_analyze::{image, macho};

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;

use serde::{Serialize, Serializer};

use code::EdgeKind;
use data::{CodePtr, PtrContext};

/// Where a function start came from (several can apply).
#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "snake_case")]
enum Source {
    Symbol,
    Objc,
    Vtable,
    Prologue,
    BlTarget,
    BlxTarget,
    CodePointer,
}

#[derive(Serialize)]
struct Function {
    #[serde(serialize_with = "hex")]
    addr: u32,
    size: u32,
    thumb: bool,
    name: Option<String>,
    demangled: Option<String>,
    subsystem: &'static str,
    sources: Vec<Source>,
    /// Direct calls (`bl`/`blx`) to functions in the game.
    #[serde(serialize_with = "hex_vec")]
    calls: Vec<u32>,
    /// `b` / `b.w` to the start of another function.
    #[serde(serialize_with = "hex_vec")]
    tail_calls: Vec<u32>,
    /// OS functions called through stubs.
    imports: Vec<String>,
    callers: u32,
    indirect_calls: u32,
    indirect_jumps: u32,
    switches: u32,
    /// Its address is stored in data, a vtable or a literal pool.
    address_taken: bool,
    /// SjLj exception landing pads inside it: extra entry points the unwinder
    /// jumps to (resume address stored in the function context).
    #[serde(serialize_with = "hex_vec")]
    landing_pads: Vec<u32>,
}

/// One call/jump through a register, for indirect_calls.json.
#[derive(Serialize)]
struct IndirectRow {
    #[serde(serialize_with = "hex")]
    function: u32,
    #[serde(flatten)]
    call: code::IndirectCall,
    /// For "virtual": how many different functions sit in that slot across
    /// all vtables (an upper bound before narrowing by class).
    candidates: u32,
    /// For "import": the OS function behind the pointer slot.
    import: Option<String>,
    /// For "game_via_slot" / "computed": the game function it reaches.
    #[serde(serialize_with = "hex_opt")]
    target: Option<u32>,
}

struct Start {
    thumb: bool,
    name: Option<String>,
    sources: Vec<Source>,
}

pub(crate) fn hex<S: Serializer>(v: &u32, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{v:#x}"))
}

pub(crate) fn hex_opt<S: Serializer>(v: &Option<u32>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) => s.serialize_str(&format!("{x:#x}")),
        None => s.serialize_none(),
    }
}

pub(crate) fn hex_vec<S: Serializer>(v: &[u32], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(v.iter().map(|x| format!("{x:#x}")))
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let flag = |args: &mut Vec<String>, k: &str| -> Vec<String> {
        let v = args.iter().filter_map(|a| a.strip_prefix(k).map(String::from)).collect();
        args.retain(|a| !a.starts_with(k));
        v
    };
    let base = flag(&mut args, "--base=").first().and_then(|b| u32::from_str_radix(b.trim_start_matches("0x"), 16).ok());
    let libs: Vec<PathBuf> = flag(&mut args, "--lib=").into_iter().map(PathBuf::from).collect();
    match args.get(1).map(String::as_str) {
        Some("functions") if args.len() >= 3 => {
            let out = PathBuf::from(args.get(3).map(String::as_str).unwrap_or("build"));
            if let Err(e) = functions(&PathBuf::from(&args[2]), &out, base) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some("recompile") if args.len() >= 3 => {
            let out = PathBuf::from(args.get(3).map(String::as_str).unwrap_or("build"));
            let set = args.get(4).map(String::as_str).unwrap_or("leaf");
            if let Err(e) = recompile(&PathBuf::from(&args[2]), &out, set, &libs) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some("check") if args.len() >= 3 => {
            let ipa = PathBuf::from(&args[2]);
            match revs_analyze::ipa::code_fingerprint(&ipa) {
                Ok(fp) => println!("code fingerprint {fp:016x}"),
                Err(e) => eprintln!("error: {e}"),
            }
            match revs_analyze::ipa::check_game(&ipa) {
                Ok(()) => println!("ok: version {}", revs_analyze::ipa::SUPPORTED_VERSION),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("usage: revs-analyze check <game.ipa>   (is it the game version the port supports?)");
            eprintln!("       revs-analyze functions <game.ipa | library.dylib> [out_dir] [--base=0x...]");
            eprintln!("       revs-analyze recompile <game.ipa> [out_dir] [leaf|all] [--lib=<library out_dir>]...   (run `functions` first)");
            std::process::exit(2);
        }
    }
}

fn hex_field(v: &serde_json::Value) -> u32 {
    v.as_str().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0)
}

/// One program image (the game or a library) ready to translate.
struct Img<'a> {
    bin: macho::Binary,
    funcs: Vec<recomp::Func>,
    starts: BTreeMap<u32, bool>,
    subsystem: HashMap<u32, String>,
    switches: HashMap<u32, Vec<u32>>,
    mov_pc_tables: HashMap<u32, u32>,
    stubs: HashMap<u32, u32>,
    ro_end: u32,
    _p: std::marker::PhantomData<&'a ()>,
}

fn load_img<'a>(path: &PathBuf, out: &PathBuf, base: Option<u32>) -> Result<Img<'a>, String> {
    let bin = image::load(path, base)?;
    let read = |f: &str| -> Result<serde_json::Value, String> {
        let s = std::fs::read_to_string(out.join(f)).map_err(|e| format!("{f}: {e} (run `functions` first)"))?;
        serde_json::from_str(&s).map_err(|e| e.to_string())
    };
    let fj = read("functions.json")?;
    let sj = read("switches.json")?;
    let mut funcs = Vec::new();
    let mut starts = BTreeMap::new();
    let mut subsystem = HashMap::new();
    for f in fj.as_array().ok_or("functions.json")? {
        let addr = hex_field(&f["addr"]);
        let thumb = f["thumb"].as_bool().unwrap_or(false);
        starts.insert(addr, thumb);
        subsystem.insert(addr, f["subsystem"].as_str().unwrap_or("").to_string());
        funcs.push(recomp::Func { addr, size: f["size"].as_u64().unwrap_or(0) as u32, thumb });
    }
    let switches: HashMap<u32, Vec<u32>> = sj
        .as_array()
        .ok_or("switches.json")?
        .iter()
        .map(|s| (hex_field(&s["addr"]), s["targets"].as_array().map_or(vec![], |t| t.iter().map(hex_field).collect())))
        .collect();
    let mov_pc_tables: HashMap<u32, u32> = sj
        .as_array()
        .ok_or("switches.json")?
        .iter()
        .filter(|s| s["kind"].as_str() == Some("thumb_mov_pc"))
        .map(|s| (hex_field(&s["addr"]), hex_field(&s["table"])))
        .collect();
    let stubs: HashMap<u32, u32> = bin
        .imports
        .iter()
        .filter(|i| i.kind == "stub")
        .enumerate()
        .filter_map(|(k, i)| Some((u32::from_str_radix(i.addr.trim_start_matches("0x"), 16).ok()?, k as u32)))
        .collect();
    let ro_end = bin.segments().iter().find(|s| s.2 == 0 && s.3 > 0).map_or(0, |s| s.0 + s.1);
    Ok(Img { bin, funcs, starts, subsystem, switches, mov_pc_tables, stubs, ro_end, _p: std::marker::PhantomData })
}

fn translate_img(img: &Img) -> recomp::Result_ {
    let p = recomp::Program { bin: &img.bin, starts: &img.starts, stubs: &img.stubs, switches: &img.switches, mov_pc_tables: &img.mov_pc_tables, ro_end: img.ro_end };
    recomp::translate_all(&p, &img.funcs)
}

/// Translates ARM functions to C (build/recomp). `set`: "leaf" writes only
/// functions that call nothing (the first test set), "all" writes every
/// translated function plus a trapping stub for every other start. `libs`:
/// library build folders (from `functions <dylib> <dir> --base=`), translated
/// into the same output so the native runner can link them.
fn recompile(ipa_path: &PathBuf, out: &PathBuf, set: &str, libs: &[PathBuf]) -> Result<(), String> {
    let game = load_img(ipa_path, out, None)?;
    let res = translate_img(&game);
    let (funcs, subsystem, starts) = (&game.funcs, &game.subsystem, &game.starts);

    // Coverage: why functions failed, for the game code (not dropped/stubbed libraries).
    let skip = ["lib:openssl", "drop:objc", "drop:store"];
    let (mut ok_n, mut ok_b, mut all_n, mut all_b) = (0, 0u64, 0, 0u64);
    let mut reasons: BTreeMap<String, (u32, u64)> = BTreeMap::new();
    for f in funcs {
        if skip.contains(&subsystem[&f.addr].as_str()) {
            continue;
        }
        all_n += 1;
        all_b += f.size as u64;
        match res.failed.get(&f.addr) {
            None => {
                ok_n += 1;
                ok_b += f.size as u64;
            }
            Some(r) => {
                // group "unknown insn e1234567" style reasons by their words only
                let key: String = r.split_whitespace().filter(|w| !w.chars().any(|c| c.is_ascii_digit())).collect::<Vec<_>>().join(" ");
                let e = reasons.entry(key).or_default();
                e.0 += 1;
                e.1 += f.size as u64;
            }
        }
    }
    println!("translated {ok_n} of {all_n} game functions ({:.1} % of {all_b} code bytes)", 100.0 * ok_b as f64 / all_b as f64);
    let mut rows: Vec<_> = reasons.into_iter().collect();
    rows.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    println!("not translated, by first reason (functions, bytes):");
    for (k, (n, b)) in rows.iter().take(25) {
        println!("  {n:>6} {b:>9}  {k}");
    }

    // Selected set.
    let chosen: Vec<u32> = res
        .ok
        .iter()
        .filter(|(a, c)| set == "all" || (res.callees[a].is_empty() && !c.contains("rc_call") && !c.contains("rc_trap")))
        .map(|(a, _)| *a)
        .collect();
    // Libraries: everything that translates is written.
    let mut images = vec![serde_json::json!({ "path": std::fs::canonicalize(ipa_path).unwrap_or(ipa_path.clone()), "dir": out, "base": null })];
    let mut all_starts: BTreeMap<u32, bool> = starts.clone();
    let mut code: BTreeMap<u32, String> = chosen.iter().map(|a| (*a, res.ok[a].clone())).collect();
    let mut lib_chosen = Vec::new();
    for d in libs {
        let (path, base) = image::read_image_json(d).ok_or(format!("{}: no image.json (run `functions` on the library first)", d.display()))?;
        let img = load_img(&path, d, base)?;
        let r = translate_img(&img);
        let bytes: u64 = img.funcs.iter().map(|f| f.size as u64).sum();
        let ok_b: u64 = img.funcs.iter().filter(|f| r.ok.contains_key(&f.addr)).map(|f| f.size as u64).sum();
        println!(
            "library {}: translated {} of {} functions ({:.1} % of {bytes} code bytes)",
            path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()),
            r.ok.len(),
            img.funcs.len(),
            100.0 * ok_b as f64 / bytes.max(1) as f64
        );
        for (a, c) in r.ok {
            if code.insert(a, c).is_some() {
                return Err(format!("{a:#x} is in two images"));
            }
            lib_chosen.push(a);
        }
        all_starts.extend(img.starts.iter());
        let failed: BTreeMap<String, &String> = r.failed.iter().map(|(a, r)| (format!("{a:#x}"), r)).collect();
        std::fs::write(d.join("failed.json"), serde_json::to_string_pretty(&failed).unwrap()).map_err(|e| e.to_string())?;
        images.push(serde_json::json!({ "path": path, "dir": d, "base": base.map(|b| format!("{b:#x}")) }));
    }
    let all: Vec<u32> = code.keys().copied().collect();

    let dir = out.join("recomp");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for old in std::fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
        if old.path().extension().is_some_and(|e| e == "c") {
            let _ = std::fs::remove_file(old.path());
        }
    }
    let header = "/* GENERATED from the user's own .ipa (and libraries). Do not commit or share. */\n#include \"rc_cpu.h\"\n";
    let mut decls = String::new();
    for a in all_starts.keys() {
        writeln!(decls, "void {}(Cpu *c);", recomp::c_name(*a)).map_err(|e| e.to_string())?;
    }
    for (k, chunk) in all.chunks(400).enumerate() {
        let mut s = String::from(header);
        s += "#pragma warning(disable: 4102 4189 4101)\n";
        s += &decls;
        for a in chunk {
            s += &code[a];
        }
        std::fs::write(dir.join(format!("rc_funcs_{k:03}.c")), s).map_err(|e| e.to_string())?;
    }
    let mut t = String::from(header);
    t += "typedef struct RcEntry { uint32_t addr; RcFn fn; } RcEntry;\n";
    t += &decls;
    if set == "all" {
        for a in all_starts.keys().filter(|a| !code.contains_key(a)) {
            writeln!(t, "void {}(Cpu *c) {{ rc_miss(c, 0x{a:x}u); }}", recomp::c_name(*a)).map_err(|e| e.to_string())?;
        }
    }
    t += "const RcEntry rc_table[] = {\n";
    for a in &all {
        writeln!(t, "  {{0x{a:x}u, {}}},", recomp::c_name(*a)).map_err(|e| e.to_string())?;
    }
    if all.is_empty() {
        t += "  {0, 0},\n";
    }
    writeln!(t, "}};\nconst uint32_t rc_table_len = {};", all.len()).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("rc_table.c"), t).map_err(|e| e.to_string())?;
    let hexes = |v: &[u32]| serde_json::to_string(&v.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>()).unwrap();
    // translated.json: the game's functions only (the checker and the recorder use it).
    std::fs::write(dir.join("translated.json"), hexes(&chosen)).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("translated_libs.json"), hexes(&lib_chosen)).map_err(|e| e.to_string())?;
    // The Thumb ones, for the touchHLE recorder (it needs a Thumb bkpt there).
    let thumb: Vec<u32> = chosen.iter().filter(|a| starts.get(a) == Some(&true)).copied().collect();
    std::fs::write(dir.join("translated_thumb.json"), hexes(&thumb)).map_err(|e| e.to_string())?;
    let failed: BTreeMap<String, &String> = res.failed.iter().map(|(a, r)| (format!("{a:#x}"), r)).collect();
    std::fs::write(dir.join("failed.json"), serde_json::to_string_pretty(&failed).unwrap()).map_err(|e| e.to_string())?;
    // images.json: what the native runner loads, and where.
    std::fs::write(dir.join("images.json"), serde_json::to_string_pretty(&images).unwrap()).map_err(|e| e.to_string())?;
    println!("wrote {} game + {} library functions ({set}) to {}", chosen.len(), lib_chosen.len(), dir.display());
    Ok(())
}

/// Adds or updates a start; returns true when it is new.
fn add(starts: &mut BTreeMap<u32, Start>, addr: u32, thumb: bool, src: Source, name: Option<String>) -> bool {
    let new = !starts.contains_key(&addr);
    let s = starts.entry(addr).or_insert(Start { thumb, name: None, sources: Vec::new() });
    if src == Source::Symbol {
        s.thumb = thumb; // the symbol table is the most reliable mode source
    }
    if name.is_some() && s.name.is_none() {
        s.name = name;
    }
    if !s.sources.contains(&src) {
        s.sources.push(src);
    }
    new
}

/// (start, end, thumb) for every function, end = next start.
fn ranges(starts: &BTreeMap<u32, Start>, text_end: u32) -> Vec<(u32, u32, bool)> {
    let v: Vec<(u32, bool)> = starts.iter().map(|(&a, s)| (a, s.thumb)).collect();
    v.iter()
        .enumerate()
        .map(|(i, &(a, t))| (a, v.get(i + 1).map_or(text_end, |n| n.0), t))
        .collect()
}

/// The function containing `addr` (index into `ranges`).
fn containing(r: &[(u32, u32, bool)], addr: u32) -> Option<usize> {
    let i = r.partition_point(|f| f.0 <= addr);
    (i > 0 && addr < r[i - 1].1).then(|| i - 1)
}

/// For a pointer into the middle of a known function: that function's range.
fn from_mid(p: &CodePtr, known: bool, r: &[(u32, u32, bool)]) -> Option<(u32, u32)> {
    (!known).then(|| containing(r, p.target)).flatten().map(|i| (r[i].0, r[i].1))
}

/// One code pointer and what it hits, for code_pointers.json.
#[derive(Serialize)]
struct PtrRow {
    from: &'static str,
    /// Where it is stored (0 for pointers built in code).
    #[serde(serialize_with = "hex")]
    at: u32,
    #[serde(serialize_with = "hex")]
    target: u32,
    thumb: bool,
    hit: &'static str,
    /// Start of the function the target falls in.
    #[serde(serialize_with = "hex")]
    function: u32,
}

#[derive(Default)]
struct Stats {
    switches: BTreeMap<&'static str, (u32, u32)>,
    switch_targets_outside: u32,
    switch_target_is_start: u32,
    tail_into_middle: u32,
    edges_outside_text: u32,
    sjlj_functions: u32,
    sjlj_without_pad: u32,
    sjlj_multi_pad: u32,
    indirect: BTreeMap<(&'static str, bool), u32>,
    virtual_no_candidate: u32,
}

fn functions(ipa_path: &PathBuf, out: &PathBuf, base: Option<u32>) -> Result<(), String> {
    let bin = image::load(ipa_path, base)?;
    let exe_name = ipa_path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let img = serde_json::json!({
        "path": std::fs::canonicalize(ipa_path).unwrap_or(ipa_path.clone()),
        "base": base.map(|b| format!("{b:#x}")),
    });
    std::fs::write(out.join("image.json"), serde_json::to_string_pretty(&img).unwrap()).map_err(|e| e.to_string())?;
    println!(
        "{exe_name}: slice 0 of {} ({} bytes), __text {:#x}..{:#x}",
        bin.slice_count,
        bin.data.len(),
        bin.text.addr,
        bin.text.addr + bin.text.size
    );
    let (t0, t1) = (bin.text.addr, bin.text.addr + bin.text.size);
    let in_text = |a: u32| a >= t0 && a < t1;
    let stubs: HashMap<u32, &str> = bin
        .imports
        .iter()
        .filter(|i| i.kind == "stub")
        .filter_map(|i| Some((u32::from_str_radix(i.addr.trim_start_matches("0x"), 16).ok()?, i.name.as_str())))
        .collect();

    let mut starts: BTreeMap<u32, Start> = BTreeMap::new();

    // 1. Symbols defined in __text.
    for sym in &bin.text_symbols {
        add(&mut starts, sym.addr, sym.thumb, Source::Symbol, Some(sym.name.clone()));
    }
    // 2. Objective-C methods.
    let classes = objc::read(&bin);
    for m in &classes.methods {
        if in_text(m.imp) {
            add(&mut starts, m.imp, m.thumb, Source::Objc, Some(m.name.clone()));
        }
    }
    // 3. Vtable slots are always function starts.
    let vtables = data::vtables(&bin);
    let mut ptrs: Vec<CodePtr> = Vec::new();
    for vt in &vtables {
        for g in &vt.groups {
            for (k, &v) in g.slots.iter().enumerate() {
                if v != 0 && in_text(v & !1) {
                    add(&mut starts, v & !1, v & 1 != 0, Source::Vtable, None);
                    let at = g.slots_at + k as u32 * 4;
                    ptrs.push(CodePtr { at, target: v & !1, thumb: v & 1 != 0, from: "vtable" });
                }
            }
        }
    }
    // 4. ARM prologues: push {..., lr}, not preceded by another push. Only in
    //    ARM-mode code, so skip the ranges of known Thumb functions.
    {
        let r = ranges(&starts, t1);
        let mut a = t0;
        while a + 4 <= t1 {
            if containing(&r, a).is_some_and(|i| r[i].2) {
                a += 2;
                continue;
            }
            let w = bin.u32(a);
            if (w & 0xFFFF_4000) == 0xE92D_4000 {
                let prev = if a >= t0 + 4 { bin.u32(a - 4) } else { 0 };
                if (prev & 0xFFFF_0000) != 0xE92D_0000 {
                    add(&mut starts, a, false, Source::Prologue, None);
                }
            }
            a += 4;
        }
    }

    // 5. Scan every function; new call targets and accepted code pointers
    //    become starts; repeat until nothing new is found. Tail-call targets
    //    never create starts (a `b` may go to shared code inside a function).
    ptrs.extend(data::data_code_ptrs(&bin));
    let data_ptr_count = ptrs.len();
    let mut mode_conflicts;
    let mut scans;
    let mut pass = 0;
    loop {
        pass += 1;
        mode_conflicts = 0u32;
        let r = ranges(&starts, t1);
        scans = r.iter().map(|&(a, e, th)| code::scan(&bin, a, e, th)).collect::<Vec<_>>();
        let mut new_starts: Vec<(u32, bool, Source)> = Vec::new();
        for sc in &scans {
            for e in sc.edges.iter().filter(|e| e.kind == EdgeKind::Call && in_text(e.target)) {
                if let Some(s) = starts.get(&e.target) {
                    mode_conflicts += (s.thumb != e.thumb) as u32;
                    continue;
                }
                let same_mode = containing(&r, e.target).is_some_and(|i| r[i].2 == e.thumb);
                new_starts.push((e.target, e.thumb, if same_mode { Source::BlTarget } else { Source::BlxTarget }));
            }
        }
        let case_targets: BTreeSet<u32> =
            scans.iter().flat_map(|s| s.switches.iter().flat_map(|w| w.targets.iter().copied())).collect();
        ptrs.truncate(data_ptr_count);
        for sc in &scans {
            for &w in sc.literal_words.iter().filter(|&&w| in_text(w & !1)) {
                ptrs.push(CodePtr { at: 0, target: w & !1, thumb: w & 1 != 0, from: "literal" });
            }
            for &(at, w) in sc.pic_sites.iter().filter(|&&(_, w)| in_text(w & !1)) {
                ptrs.push(CodePtr { at, target: w & !1, thumb: w & 1 != 0, from: "pic" });
            }
        }
        let pic_targets: std::collections::HashSet<u32> = ptrs.iter().filter(|p| p.from == "pic").map(|p| p.target).collect();
        for p in &ptrs {
            let known = starts.get(&p.target);
            let ctx = PtrContext {
                known_start: known.is_some(),
                inside: if known.is_some() { None } else { containing(&r, p.target).map(|i| (r[i].0, r[i].2)) },
                is_case_target: case_targets.contains(&p.target),
                looks_like_entry: code::looks_like_entry(&bin, p.target, p.thumb),
                prev_ends: code::prev_ends(&bin, p.target, p.thumb),
                leaf_boundary: match (p.from, from_mid(p, known.is_some(), &r)) {
                    ("data", Some((f, _))) => code::leaf_boundary(&bin, f, p.target, p.thumb),
                    ("literal", Some((f, end))) => code::literal_leaf_boundary(&bin, f, end, p.target, p.thumb),
                    _ => false,
                },
                is_pic_target: pic_targets.contains(&p.target),
            };
            if !ctx.known_start && data::accept_code_pointer(p, &ctx) {
                new_starts.push((p.target, p.thumb, Source::CodePointer));
            }
        }
        let mut found = 0;
        for (a, th, src) in new_starts {
            found += add(&mut starts, a, th, src, None) as u32;
        }
        println!("pass {pass}: {} functions, {found} new", starts.len());
        if found == 0 || pass >= 8 {
            break;
        }
    }

    // 6. Function table with call-graph data.
    let r = ranges(&starts, t1);
    let taken: BTreeSet<u32> = ptrs.iter().map(|p| p.target).collect();
    let mut callers: HashMap<u32, u32> = HashMap::new();
    let mut funcs = Vec::with_capacity(r.len());
    let mut stats = Stats::default();
    // Every function that can sit in each vtable slot (byte offset -> targets).
    let mut slot_targets: HashMap<i32, BTreeSet<u32>> = HashMap::new();
    for g in vtables.iter().flat_map(|v| &v.groups) {
        for (k, &v) in g.slots.iter().enumerate() {
            if v != 0 {
                slot_targets.entry(k as i32 * 4).or_default().insert(v & !1);
            }
        }
    }
    let mut indirect_rows: Vec<IndirectRow> = Vec::new();
    // Import pointer slots (non-lazy / lazy) -> OS symbol.
    let slot_names: HashMap<u32, &str> = bin
        .imports
        .iter()
        .filter(|i| i.kind != "stub")
        .filter_map(|i| Some((u32::from_str_radix(i.addr.trim_start_matches("0x"), 16).ok()?, i.name.as_str())))
        .collect();
    // Game functions by symbol name (a pointer slot may name one of them).
    let local_fn: HashMap<&str, u32> = bin.text_symbols.iter().map(|s| (s.name.as_str(), s.addr)).collect();
    for (i, &(addr, end, thumb)) in r.iter().enumerate() {
        let s = &starts[&addr];
        let sc = &scans[i];
        // SjLj: the pc-relative addresses a function computes inside itself are
        // its landing pads (resume points the unwinder longjmps to).
        let uses_sjlj = sc.edges.iter().any(|e| stubs.get(&e.target).is_some_and(|n| n.contains("SjLj_Register")));
        let pads: BTreeSet<u32> = sc
            .pic_sites
            .iter()
            .map(|&(_, w)| w)
            // Thumb code sets the mode bit afterwards (`orr rX, #1`): ignore it.
            .filter(|&w| (w & !1) > addr && (w & !1) < end)
            .map(|w| w & !1)
            .collect();
        let landing_pads: Vec<u32> = if uses_sjlj { pads.into_iter().collect() } else { Vec::new() };
        if uses_sjlj {
            stats.sjlj_functions += 1;
            stats.sjlj_without_pad += landing_pads.is_empty() as u32;
            stats.sjlj_multi_pad += (landing_pads.len() > 1) as u32;
        }
        let (mut calls, mut tails, mut imports) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        for c in &sc.indirect {
            let mut c = *c;
            let candidates = if c.kind == "virtual" {
                slot_targets.get(&c.offset).map_or(0, |t| t.len() as u32)
            } else {
                0
            };
            let mut import = None;
            let mut target = None;
            if c.kind == "import" {
                match slot_names.get(&c.addr) {
                    // A pointer slot can also name a game function (weak C++
                    // templates are reached this way): then it is a direct call.
                    Some(n) => match local_fn.get(n) {
                        Some(&t) => {
                            c.kind = "game_via_slot";
                            target = Some(t);
                            if c.jump { tails.insert(t) } else { calls.insert(t) };
                        }
                        None => {
                            imports.insert(n.to_string());
                            import = Some(n.to_string());
                        }
                    },
                    // Not an import slot: a function pointer in a global variable.
                    None => c.kind = "global",
                }
            } else if c.kind == "computed" && starts.contains_key(&(c.addr & !1)) {
                target = Some(c.addr & !1);
                if c.jump { tails.insert(c.addr & !1) } else { calls.insert(c.addr & !1) };
            }
            *stats.indirect.entry((c.kind, c.jump)).or_default() += 1;
            stats.virtual_no_candidate += (c.kind == "virtual" && candidates == 0) as u32;
            indirect_rows.push(IndirectRow { function: addr, call: c, candidates, import, target });
        }
        let demangled = s.name.as_deref().and_then(demangle);
        let subsystem = subsystems::classify(demangled.as_deref().or(s.name.as_deref()));
        for e in &sc.edges {
            if let Some(name) = stubs.get(&e.target) {
                imports.insert(name.to_string());
            } else if starts.contains_key(&e.target) {
                if e.kind == EdgeKind::Call {
                    calls.insert(e.target);
                } else {
                    tails.insert(e.target);
                }
            } else if !in_text(e.target) {
                stats.edges_outside_text += 1;
            } else if e.kind == EdgeKind::TailCall {
                stats.tail_into_middle += 1;
            }
        }
        for &c in calls.iter().chain(&tails) {
            *callers.entry(c).or_default() += 1;
        }
        for w in &sc.switches {
            let e = stats.switches.entry(w.kind).or_default();
            e.0 += 1;
            e.1 += w.cases_from_cmp.is_none() as u32;
            stats.switch_targets_outside += w.targets.iter().any(|&t| t < addr || t >= end) as u32;
            stats.switch_target_is_start += w.targets.iter().any(|&t| t != addr && starts.contains_key(&t)) as u32;
        }
        let mut sources = s.sources.clone();
        sources.sort();
        funcs.push(Function {
            addr,
            size: end - addr,
            thumb,
            name: s.name.clone(),
            demangled,
            subsystem,
            sources,
            calls: calls.into_iter().collect(),
            tail_calls: tails.into_iter().collect(),
            imports: imports.into_iter().collect(),
            callers: 0,
            indirect_calls: sc.indirect_calls,
            indirect_jumps: sc.indirect_jumps,
            switches: sc.switches.len() as u32,
            address_taken: taken.contains(&addr),
            landing_pads,
        });
    }
    for f in &mut funcs {
        f.callers = callers.get(&f.addr).copied().unwrap_or(0);
    }
    let mut subs: Vec<(u32, &'static str)> = funcs.iter().map(|f| (f.addr, f.subsystem)).collect();
    subsystems::claim_library_block(&mut subs, "lib:openssl");
    for (f, (_, s)) in funcs.iter_mut().zip(subs) {
        f.subsystem = s;
    }

    // 7. Code pointers by what they hit (evidence for accept_code_pointer).
    let case_targets: BTreeSet<u32> =
        scans.iter().flat_map(|s| s.switches.iter().flat_map(|w| w.targets.iter().copied())).collect();
    let mut ptr_kinds: BTreeMap<(&str, &str), u32> = BTreeMap::new();
    let mut ptr_rows: Vec<PtrRow> = Vec::new();
    for p in &ptrs {
        let hit = match starts.get(&p.target) {
            Some(s) if s.thumb == p.thumb => "start, same mode",
            Some(_) => "start, other mode",
            None if case_targets.contains(&p.target) => "switch case",
            None => match containing(&r, p.target) {
                Some(i) if r[i].2 == p.thumb => "mid-function, same mode",
                Some(_) => "mid-function, other mode",
                None => "outside",
            },
        };
        *ptr_kinds.entry((p.from, hit)).or_default() += 1;
        // Raw literal words are mostly integer constants: keep only exact hits.
        if p.from != "literal" || hit.starts_with("start") {
            let f = containing(&r, p.target).map(|i| r[i].0).unwrap_or(0);
            ptr_rows.push(PtrRow { from: p.from, at: p.at, target: p.target, thumb: p.thumb, hit, function: f });
        }
    }

    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let write = |file: &str, v: &dyn erased::Json| -> Result<(), String> {
        std::fs::write(out.join(file), v.to_json()).map_err(|e| e.to_string())
    };
    let switches: Vec<&code::Switch> = scans.iter().flat_map(|s| s.switches.iter()).collect();
    write("functions.json", &funcs)?;
    write("imports.json", &bin.imports)?;
    write("objc_methods.json", &classes.methods)?;
    write("vtables.json", &vtables)?;
    write("switches.json", &switches)?;
    write("code_pointers.json", &ptr_rows)?;
    let class_list = data::classes(&bin, &vtables);
    write("classes.json", &class_list)?;
    write("indirect_calls.json", &indirect_rows)?;

    // Summary.
    let named = funcs.iter().filter(|f| f.name.is_some()).count();
    let thumb = funcs.iter().filter(|f| f.thumb).count();
    println!("functions: {} ({} named, {} Thumb)", funcs.len(), named, thumb);
    let mut by_src: BTreeMap<String, usize> = BTreeMap::new();
    for f in funcs.iter().filter(|f| f.name.is_none()) {
        *by_src.entry(format!("{:?}", f.sources)).or_default() += 1;
    }
    println!("unnamed functions by how they were found:");
    for (k, n) in &by_src {
        println!("  {n:>6} {k}");
    }
    let mut by_sub: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for f in &funcs {
        let e = by_sub.entry(f.subsystem).or_default();
        e.0 += 1;
        e.1 += f.size as u64;
    }
    let mut rows: Vec<_> = by_sub.into_iter().collect();
    rows.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    println!("by subsystem (functions, code bytes):");
    for (k, (n, b)) in rows {
        println!("  {k:<12} {n:>6} {b:>9}");
    }
    let edges: usize = funcs.iter().map(|f| f.calls.len() + f.tail_calls.len()).sum();
    let import_edges: usize = funcs.iter().map(|f| f.imports.len()).sum();
    let ind_c: u32 = funcs.iter().map(|f| f.indirect_calls).sum();
    let ind_j: u32 = funcs.iter().map(|f| f.indirect_jumps).sum();
    let roots = funcs.iter().filter(|f| f.callers == 0 && !f.address_taken).count();
    println!("call graph: {edges} game edges, {import_edges} import edges, {ind_c} indirect calls, {ind_j} indirect jumps");
    println!("  {roots} functions with no known caller and no stored pointer");
    println!("objc: {} classes, {} methods; vtables: {}", classes.class_count, classes.methods.len(), vtables.len());
    let by_kind = |k: &str| class_list.iter().filter(|c| c.kind == k).count();
    println!(
        "c++ classes (typeinfo): {} ({} root, {} single base, {} multiple), {} with a vtable",
        class_list.len(),
        by_kind("root"),
        by_kind("single"),
        by_kind("multiple"),
        class_list.iter().filter(|c| c.vtable.is_some()).count()
    );
    println!("calls/jumps through a register:");
    for ((k, jump), n) in &stats.indirect {
        println!("  {n:>6} {k:<8} {}", if *jump { "jump (bx)" } else { "call (blx)" });
    }
    println!("  {} virtual calls with a slot no vtable has", stats.virtual_no_candidate);
    let pads: usize = funcs.iter().map(|f| f.landing_pads.len()).sum();
    println!(
        "sjlj: {} functions, {pads} landing pads ({} with none found, {} with several)",
        stats.sjlj_functions, stats.sjlj_without_pad, stats.sjlj_multi_pad
    );
    for (k, (n, guessed)) in &stats.switches {
        println!("switches {k:<13} {n:>4} ({guessed} without a cmp bound)");
    }
    println!("checks:");
    println!("  {} switches with a case outside their function", stats.switch_targets_outside);
    println!("  {} switches with a case on another function's start", stats.switch_target_is_start);
    println!("  {mode_conflicts} calls whose ARM/Thumb mode disagrees with the start");
    println!("  {} tail calls into the middle of a function", stats.tail_into_middle);
    println!("  {} branch targets outside __text and stubs", stats.edges_outside_text);
    println!("code pointers by what they hit:");
    for ((from, hit), n) in &ptr_kinds {
        println!("  {n:>6} {from:<8} {hit}");
    }
    println!("imports: {} OS symbols", bin.imports.len());
    println!("wrote {}", out.display());
    Ok(())
}

/// Demangles an Itanium C++ name (Mach-O adds a leading underscore).
fn demangle(name: &str) -> Option<String> {
    let n = name.strip_prefix('_').unwrap_or(name);
    if !n.starts_with("_Z") {
        return None;
    }
    cpp_demangle::Symbol::new(n).ok().map(|s| s.to_string())
}

/// Small helper so `write` can take every output type.
mod erased {
    pub trait Json {
        fn to_json(&self) -> String;
    }
    impl<T: serde::Serialize> Json for T {
        fn to_json(&self) -> String {
            serde_json::to_string_pretty(self).unwrap()
        }
    }
}
