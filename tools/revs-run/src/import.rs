//! `--import-save=DIR`: copy a touchHLE save into the port's own home folder.
//!
//! DIR is touchHLE's sandbox folder for the game
//! (`touchHLE_sandbox/jp.co.capcom.biovsus`, or a `_pN` player copy), or its
//! `Documents` folder. The save is `Documents/savevs7.dat` plus
//! `cloudiaDatabase.db` (and any other file there); a `Library/Preferences`
//! plist is copied too. The game writes the save itself, so the files are the
//! same format in touchHLE and the port.
//!
//! The source is only read. A save already in the port's home is kept, moved
//! to `Documents.before-import-<unix time>` (and the same for Preferences), so
//! an import never loses anything.

use std::fs;
use std::path::{Path, PathBuf};

const SAVE: &str = "savevs7.dat";

/// The touchHLE folder that holds `Documents/savevs7.dat`, from DIR or DIR's parent.
fn sandbox_of(dir: &Path) -> Option<PathBuf> {
    if dir.join("Documents").join(SAVE).is_file() {
        Some(dir.to_path_buf())
    } else if dir.join(SAVE).is_file() {
        dir.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

/// Copies the files (not subfolders) of `from` into `to`, creating it.
fn copy_files(from: &Path, to: &Path) -> Result<Vec<String>, String> {
    fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut copied = Vec::new();
    let entries = fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() {
            let name = e.file_name();
            fs::copy(&p, to.join(&name)).map_err(|err| format!("{}: {err}", p.display()))?;
            copied.push(name.to_string_lossy().into_owned());
        }
    }
    copied.sort();
    Ok(copied)
}

/// Moves `dir` aside as `<dir>.before-import-<stamp>` if it holds any file.
fn keep_old(dir: &Path, stamp: u64) -> Result<Option<PathBuf>, String> {
    let has_files = fs::read_dir(dir).map(|mut r| r.next().is_some()).unwrap_or(false);
    if !has_files {
        return Ok(None);
    }
    let name = format!("{}.before-import-{stamp}", dir.file_name().map_or("old".into(), |n| n.to_string_lossy()));
    let to = dir.with_file_name(name);
    fs::rename(dir, &to).map_err(|e| format!("{} -> {}: {e}", dir.display(), to.display()))?;
    Ok(Some(to))
}

/// Imports the save at `src` into `home`. Prints what it did.
pub fn import_save(src: &Path, home: &Path) -> Result<(), String> {
    let sandbox = sandbox_of(src).ok_or_else(|| {
        format!("{}: no {SAVE} there or in its Documents folder (expected touchHLE_sandbox/jp.co.capcom.biovsus)", src.display())
    })?;
    let docs = home.join("Documents");
    if sandbox.join("Documents").canonicalize().ok() == docs.canonicalize().ok() {
        return Err("the source is the port's own save folder".into());
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let prefs_from = sandbox.join("Library").join("Preferences");
    let prefs_to = home.join("Library").join("Preferences");
    for (dir, wanted) in [(&docs, true), (&prefs_to, prefs_from.is_dir())] {
        if wanted {
            if let Some(old) = keep_old(dir, stamp)? {
                println!("import: kept the port's previous files in {}", old.display());
            }
        }
    }
    let copied = copy_files(&sandbox.join("Documents"), &docs)?;
    println!("import: {} -> {}: {}", sandbox.join("Documents").display(), docs.display(), copied.join(", "));
    if prefs_from.is_dir() {
        let copied = copy_files(&prefs_from, &prefs_to)?;
        println!("import: {} -> {}: {}", prefs_from.display(), prefs_to.display(), copied.join(", "));
    }
    Ok(())
}
