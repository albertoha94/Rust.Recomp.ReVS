//! Builds the checker: compiles the generated C (build/recomp, derived from the
//! user's .ipa and never committed) with the runtime, and links touchHLE's
//! prebuilt dynarmic (the ARM CPU touchHLE runs the game on).
//!
//! Env overrides:
//!   REVS_RECOMP_DIR        generated C (default: <repo>/build/recomp)
//!   TOUCHHLE_DYNARMIC_OUT  dynarmic build output (default: newest
//!                          <TOUCHHLE_DIR>\target\release\build\touchHLE_dynarmic_wrapper\*\out)
//!   TOUCHHLE_DIR           a touchHLE checkout built in release (default: touchHLE-src next to
//!                          this repository). Only this developer tool needs touchHLE; the port does not.

use std::path::{Path, PathBuf};

fn newest_dynarmic_out() -> Option<PathBuf> {
    let dir = std::env::var_os("TOUCHHLE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../touchHLE-src"));
    let root = dir.join("target/release/build/touchHLE_dynarmic_wrapper");
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path().join("out"))
        .filter(|p| p.join("dynarmic_wrapper.lib").exists())
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
}

fn main() {
    let pkg = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runtime = pkg.join("runtime");
    let recomp = std::env::var("REVS_RECOMP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| pkg.join("../../build/recomp"));
    println!("cargo:rerun-if-env-changed=REVS_RECOMP_DIR");
    println!("cargo:rerun-if-env-changed=TOUCHHLE_DYNARMIC_OUT");
    println!("cargo:rerun-if-env-changed=TOUCHHLE_DIR");
    println!("cargo:rerun-if-changed={}", runtime.display());
    println!("cargo:rerun-if-changed={}", recomp.display());

    let mut files: Vec<PathBuf> = std::fs::read_dir(&recomp)
        .unwrap_or_else(|_| panic!("{} missing: run `revs-analyze recompile <ipa>` first", recomp.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "c"))
        .collect();
    files.sort();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    cc::Build::new()
        .file(runtime.join("rc_runtime.c"))
        .files(&files)
        .include(&runtime)
        .opt_level(2)
        .warnings(false)
        .compile("rc_game");

    let dyn_out = std::env::var("TOUCHHLE_DYNARMIC_OUT")
        .map(PathBuf::from)
        .ok()
        .or_else(newest_dynarmic_out)
        .expect("touchHLE's dynarmic build not found: build touchHLE (release) or set TOUCHHLE_DYNARMIC_OUT");
    println!("cargo:rustc-link-search=native={}", dyn_out.display());
    println!("cargo:rustc-link-search=native={}", dyn_out.join("lib").display());
    for lib in ["dynarmic_wrapper", "dynarmic", "mcl", "fmt", "Zydis"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
}
