//! Setup step of a release (RecompLauncher's "prepare", or run by hand): builds
//! the game on the player's PC from their own .ipa. A release holds no game
//! code; this makes it, and it never leaves that PC.
//!
//!   revs-prepare --ipa=<game.ipa> [--out=<folder>] [--jobs=N] [--cc=<compiler>]
//!
//! Next to this exe, the release has revs-analyze(.exe), dylibs/ (touchHLE's
//! open-source ARM libraries), runtime/ (rc_runtime.c, rc_cpu.h) and, on
//! Windows, clang/ (a trimmed llvm-mingw). Into --out (default: game/ next to
//! this exe) it writes:
//!   game.ipa                 a copy of the player's .ipa (the game reads its files at run time)
//!   dylibs/, lib/, recomp/   the libraries, function maps and images.json (paths relative to --out)
//!   rc_game.dll              the recompiled game (librc_game.so on Linux), loaded by revs-run
//! The generated C and object files are deleted at the end. Without --ipa, the
//! copy kept in --out is used (a new release can rebuild without the .ipa).
//!
//! Progress goes to stdout as `progress <0..1> <text>` lines (the launcher shows
//! them); everything else also goes to <out>/prepare.log. Exit code 0 = done.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const LIBS: [(&str, &str, Option<&str>); 3] = [
    ("libgcc_s.1.dylib", "libgcc", Some("0x700000")),
    ("libstdc++.6.0.9.dylib", "libstdc++", Some("0x800000")),
    ("libsqlite3.dylib", "libsqlite3", None),
];
const DLL: &str = if cfg!(windows) {
    "rc_game.dll"
} else if cfg!(target_os = "macos") {
    "librc_game.dylib"
} else {
    "librc_game.so"
};

static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);

fn log(s: &str) {
    if let Some(f) = LOG.lock().unwrap().as_mut() {
        let _ = writeln!(f, "{s}");
    }
}

fn say(s: &str) {
    println!("{s}");
    let _ = std::io::stdout().flush();
    log(s);
}

fn progress(f: f64, text: &str) {
    say(&format!("progress {:.3} {text}", f.clamp(0.0, 1.0)));
}

fn main() {
    if let Err(e) = run() {
        eprintln!("setup failed: {e}");
        log(&format!("setup failed: {e}"));
        std::process::exit(1);
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().find_map(|a| a.strip_prefix(&format!("--{name}=")).map(str::to_string))
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: revs-prepare --ipa=<game.ipa> [--out=<folder>] [--jobs=N] [--cc=<compiler>]");
        return Ok(());
    }
    let exe_dir = std::env::current_exe().map_err(|e| e.to_string())?.parent().unwrap().to_path_buf();
    let out = arg(&args, "out").map(PathBuf::from).unwrap_or_else(|| exe_dir.join("game"));
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let out = std::fs::canonicalize(&out).map_err(|e| e.to_string())?;
    *LOG.lock().unwrap() = std::fs::File::create(out.join("prepare.log")).ok();
    log(&format!("revs-prepare {} {}", env!("CARGO_PKG_VERSION"), args.join(" ")));

    // The setup is finished only when the library exists: take the old one away first.
    let dll = out.join(DLL);
    let _ = std::fs::remove_file(&dll);

    // 1. The player's copy.
    progress(0.0, "Checking your copy of the game");
    let kept = out.join("game.ipa");
    match arg(&args, "ipa").filter(|s| !s.is_empty()) {
        Some(ipa) => {
            let ipa = PathBuf::from(ipa);
            if !ipa.is_file() {
                return Err(format!("{} not found", ipa.display()));
            }
            // Before copying anything: the port only works with one game version.
            revs_analyze::ipa::check_game(&ipa).map_err(|e| format!("{}: {e}", ipa.display()))?;
            if std::fs::canonicalize(&ipa).ok() != std::fs::canonicalize(&kept).ok() {
                std::fs::copy(&ipa, &kept).map_err(|e| format!("copying {}: {e}", ipa.display()))?;
            }
        }
        None if kept.is_file() => {}
        None => return Err("no game given: pass --ipa=<your Resident_Evil_Mercenaries_VS._1.10.10_ios_4.2.ipa>".into()),
    }

    // 2. touchHLE's libraries, copied next to it so the folder stands on its own.
    let dylibs = out.join("dylibs");
    std::fs::create_dir_all(&dylibs).map_err(|e| e.to_string())?;
    for (file, _, _) in LIBS {
        let from = exe_dir.join("dylibs").join(file);
        std::fs::copy(&from, dylibs.join(file)).map_err(|e| format!("{}: {e} (is the release complete?)", from.display()))?;
    }

    // 3. Map and recompile (revs-analyze, from the same release).
    let analyze = exe_dir.join(if cfg!(windows) { "revs-analyze.exe" } else { "revs-analyze" });
    progress(0.02, "Mapping the game's code");
    tool(&analyze, &["functions".into(), "game.ipa".into(), ".".into()], &out)?;
    for (k, (file, dir, base)) in LIBS.iter().enumerate() {
        progress(0.06 + 0.02 * k as f64, &format!("Mapping {file}"));
        let mut a = vec!["functions".into(), format!("dylibs/{file}"), format!("lib/{dir}")];
        if let Some(b) = base {
            a.push(format!("--base={b}"));
        }
        tool(&analyze, &a, &out)?;
    }
    progress(0.12, "Recompiling the game to C");
    let mut a: Vec<String> = vec!["recompile".into(), "game.ipa".into(), ".".into(), "all".into()];
    a.extend(LIBS.iter().map(|(_, dir, _)| format!("--lib=lib/{dir}")));
    tool(&analyze, &a, &out)?;
    relative_images(&out)?;

    // 4. Compile the C into the game library.
    let recomp = out.join("recomp");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&recomp)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "c"))
        .collect();
    files.sort();
    let runtime = exe_dir.join("runtime");
    files.push(runtime.join("rc_runtime.c"));
    let cc = compiler(&args, &exe_dir)?;
    let obj = out.join("obj");
    let _ = std::fs::remove_dir_all(&obj);
    std::fs::create_dir_all(&obj).map_err(|e| e.to_string())?;
    let jobs: usize = arg(&args, "jobs")
        .and_then(|j| j.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8))
        .max(1);
    say(&format!("compiling {} files with {} ({jobs} at a time)", files.len(), cc.display()));
    let next = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let failed: Mutex<Option<String>> = Mutex::new(None);
    progress(0.30, "Compiling the game (this takes a few minutes)");
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= files.len() || failed.lock().unwrap().is_some() {
                    break;
                }
                let src = &files[i];
                let o = obj.join(format!("{i:03}.o"));
                let mut cmd = Command::new(&cc);
                cmd.args(cflags()).arg("-I").arg(&runtime).arg("-c").arg(src).arg("-o").arg(&o);
                if let Err(e) = run_cmd(cmd, &out) {
                    *failed.lock().unwrap() = Some(format!("{}: {e}", src.display()));
                    break;
                }
                let n = finished.fetch_add(1, Ordering::SeqCst) + 1;
                progress(0.30 + 0.65 * n as f64 / files.len() as f64, &format!("Compiling the game ({n} of {})", files.len()));
            });
        }
    });
    if let Some(e) = failed.into_inner().unwrap() {
        return Err(e);
    }

    // 5. Link.
    progress(0.96, "Linking");
    let tmp = out.join(format!("{DLL}.tmp"));
    let mut cmd = Command::new(&cc);
    cmd.args(ldflags()).arg("-o").arg(&tmp);
    for i in 0..files.len() {
        cmd.arg(obj.join(format!("{i:03}.o")));
    }
    run_cmd(cmd, &out)?;
    std::fs::rename(&tmp, &dll).map_err(|e| e.to_string())?;

    // 6. The generated C and objects aren't needed to play.
    for f in files.iter().filter(|f| f.starts_with(&recomp)) {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all(&obj);
    progress(1.0, "Done");
    say(&format!("set up: {}", dll.display()));
    Ok(())
}

/// Same rules as the developer build (tools/revs-run/build.rs): native mode,
/// -O2, and never fuse a*b+c (the game's VFP rounds each step).
fn cflags() -> Vec<&'static str> {
    let mut f = vec!["-O2", "-w", "-ffp-contract=off", "-DRC_NATIVE", "-DRC_DLL"];
    if cfg!(windows) {
        f.insert(0, "--target=x86_64-w64-mingw32");
    } else {
        f.extend(["-fPIC", "-fvisibility=hidden", "-std=gnu17"]);
    }
    f
}

fn ldflags() -> Vec<&'static str> {
    if cfg!(windows) {
        // -static: no MinGW runtime DLLs next to it (Windows 10+ has the UCRT).
        vec!["--target=x86_64-w64-mingw32", "-shared", "-static", "-fuse-ld=lld", "-s"]
    } else if cfg!(target_os = "macos") {
        vec!["-dynamiclib", "-lm"]
    } else {
        vec!["-shared", "-lm", "-s"]
    }
}

fn compiler(args: &[String], exe_dir: &Path) -> Result<PathBuf, String> {
    if let Some(c) = arg(args, "cc") {
        return Ok(PathBuf::from(c));
    }
    if cfg!(windows) {
        let c = exe_dir.join("clang/bin/clang.exe");
        return if c.is_file() { Ok(c) } else { Err(format!("{} not found (is the release complete?)", c.display())) };
    }
    Ok(PathBuf::from(std::env::var("CC").unwrap_or_else(|_| "cc".into())))
}

/// Runs revs-analyze in `dir`, its output into the log.
fn tool(exe: &Path, args: &[String], dir: &Path) -> Result<(), String> {
    let mut cmd = Command::new(exe);
    cmd.args(args);
    run_cmd(cmd, dir)
}

fn run_cmd(mut cmd: Command, dir: &Path) -> Result<(), String> {
    cmd.current_dir(dir);
    let shown = format!("{cmd:?}");
    log(&format!("> {shown}"));
    let o = cmd.output().map_err(|e| format!("{shown}: {e}"))?;
    log(&String::from_utf8_lossy(&o.stdout));
    log(&String::from_utf8_lossy(&o.stderr));
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let last: Vec<&str> = err.lines().rev().take(8).collect();
        return Err(format!("{shown} failed ({}): {}", o.status, last.into_iter().rev().collect::<Vec<_>>().join(" | ")));
    }
    Ok(())
}

/// images.json with paths relative to the build folder, so it still works if
/// the launcher moves the game's folder.
fn relative_images(out: &Path) -> Result<(), String> {
    let p = out.join("recomp/images.json");
    let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut v: serde_json::Value = serde_json::from_str(&s).map_err(|e| e.to_string())?;
    for im in v.as_array_mut().ok_or("images.json: not a list")? {
        for key in ["path", "dir"] {
            if let Some(path) = im[key].as_str() {
                let path = PathBuf::from(path);
                let full = std::fs::canonicalize(out.join(&path)).unwrap_or(path);
                if let Ok(rel) = full.strip_prefix(out) {
                    im[key] = serde_json::Value::String(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).map_err(|e| e.to_string())
}
