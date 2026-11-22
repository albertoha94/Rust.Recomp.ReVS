//! Builds the native runner: compiles the generated C (build/recomp, derived
//! from the user's .ipa and never committed) with the shared runtime from
//! revs-check, in native mode (RC_NATIVE: no loop budget).
//!
//! Env override: REVS_RECOMP_DIR (default <repo>/build/recomp).

use std::path::PathBuf;

fn main() {
    let pkg = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runtime = pkg.join("../revs-check/runtime");
    let recomp = std::env::var("REVS_RECOMP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| pkg.join("../../build/recomp"));
    println!("cargo:rerun-if-env-changed=REVS_RECOMP_DIR");
    println!("cargo:rerun-if-changed={}", runtime.display());
    println!("cargo:rerun-if-changed={}", recomp.display());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&recomp)
        .unwrap_or_else(|_| panic!("{} missing: run `revs-analyze recompile <ipa> build all` first", recomp.display()))
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
        .define("RC_NATIVE", None)
        .opt_level(2)
        .warnings(false)
        // gcc / clang (Linux, macOS): never fuse a*b+c into one FMA. The game's
        // VFP multiplies and adds round separately; fusing would change results
        // on ARM hosts (Apple Silicon) and let a Local Match drift between a Mac
        // and a PC. (MSVC for x64 never fuses without /arch:AVX2.)
        .flag_if_supported("-ffp-contract=off")
        .compile("rc_game_native");

    // PVRTC decoder (vendor/PVRTDecompress, MIT).
    println!("cargo:rerun-if-changed=native/pvrtc.cpp");
    cc::Build::new().cpp(true).file(pkg.join("native/pvrtc.cpp")).opt_level(2).warnings(false).compile("revs_pvrtc");

    gl_bindings();
    // CARGO_CFG_TARGET_OS: the system being built for (build.rs itself runs on the build machine).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // SDL2 (static) reads audio device names from the registry.
        println!("cargo:rustc-link-lib=advapi32");
        // A symbol map next to the exe, to name the code a --watchdog sample was in.
        println!("cargo:rustc-link-arg-bins=/MAP");
    }
}

/// GL bindings for the GLES layer (the same set touchHLE generates).
fn gl_bindings() {
    use gl_generator::{Api, Fallbacks, GlobalGenerator, Profile, Registry};
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut f = std::fs::File::create(out.join("gl21compat.rs")).unwrap();
    Registry::new(
        Api::Gl,
        (2, 1),
        Profile::Compatibility,
        Fallbacks::None,
        [
            "GL_EXT_framebuffer_object",
            "GL_EXT_texture_filter_anisotropic",
            "GL_EXT_texture_lod_bias",
            "GL_ARB_matrix_palette",
            "GL_ARB_vertex_blend",
            "GL_EXT_blend_subtract",
        ],
    )
    .write_bindings(GlobalGenerator, &mut f)
    .unwrap();
    let mut f = std::fs::File::create(out.join("gles11.rs")).unwrap();
    Registry::new(
        Api::Gles1,
        (1, 1),
        Profile::Core,
        Fallbacks::None,
        [
            "GL_OES_framebuffer_object",
            "GL_OES_rgb8_rgba8",
            "GL_EXT_texture_filter_anisotropic",
            "GL_IMG_texture_compression_pvrtc",
            "GL_EXT_texture_lod_bias",
            "GL_EXT_texture_format_BGRA8888",
            "GL_OES_draw_texture",
            "GL_OES_mapbuffer",
            "GL_OES_compressed_paletted_texture",
            "GL_OES_matrix_palette",
            "GL_OES_blend_subtract",
        ],
    )
    .write_bindings(GlobalGenerator, &mut f)
    .unwrap();
}
