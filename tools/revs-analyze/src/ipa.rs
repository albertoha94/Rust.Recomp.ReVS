//! Reads the game's executable from the user's own .ipa (a zip). Nothing is
//! written to disk here.

use std::io::Read;
use std::path::Path;

/// Returns (executable name, bytes) of `Payload/<App>.app/<exe>`, where the
/// executable is named by CFBundleExecutable or, failing that, is the file
/// named like the .app folder.
pub fn read_executable(ipa: &Path) -> Result<(String, Vec<u8>), String> {
    if ipa.is_dir() {
        return read_unpacked(ipa);
    }
    let f = std::fs::File::open(ipa).map_err(|e| format!("{}: {e}", ipa.display()))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let app = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .find_map(|n| {
            let rest = n.strip_prefix("Payload/")?;
            let app = rest.split('/').next()?;
            app.ends_with(".app").then(|| app.to_string())
        })
        .ok_or("no Payload/*.app in the ipa")?;
    let exe = app.trim_end_matches(".app").to_string();
    let path = format!("Payload/{app}/{exe}");
    let mut entry = zip.by_name(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
    Ok((exe, data))
}

/// The same for an unpacked ipa: a folder holding `Payload/<App>.app/` (or the
/// Payload folder itself).
fn read_unpacked(dir: &Path) -> Result<(String, Vec<u8>), String> {
    let payload = if dir.join("Payload").is_dir() { dir.join("Payload") } else { dir.to_path_buf() };
    let app = std::fs::read_dir(&payload)
        .map_err(|e| format!("{}: {e}", payload.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "app"))
        .ok_or("no *.app in the Payload folder")?;
    let exe = app.file_stem().unwrap().to_string_lossy().into_owned();
    let data = std::fs::read(app.join(&exe)).map_err(|e| format!("{exe}: {e}"))?;
    Ok((exe, data))
}

/// Reads `Payload/<App>.app/<rel>` (a resource such as Info.plist or a nib)
/// from the user's .ipa or unpacked folder, into memory.
pub fn read_app_file(ipa: &Path, rel: &str) -> Result<Vec<u8>, String> {
    if ipa.is_dir() {
        let payload = if ipa.join("Payload").is_dir() { ipa.join("Payload") } else { ipa.to_path_buf() };
        let app = std::fs::read_dir(&payload)
            .map_err(|e| format!("{}: {e}", payload.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "app"))
            .ok_or("no *.app in the Payload folder")?;
        return std::fs::read(app.join(rel)).map_err(|e| format!("{rel}: {e}"));
    }
    let f = std::fs::File::open(ipa).map_err(|e| format!("{}: {e}", ipa.display()))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let path = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .find(|n| n.strip_prefix("Payload/").and_then(|r| r.split_once(".app/")).is_some_and(|(_, f)| f == rel))
        .ok_or(format!("{rel}: not in the ipa"))?;
    let mut entry = zip.by_name(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
    Ok(data)
}

/// The files of `Payload/<App>.app/` in the user's .ipa (or unpacked folder),
/// indexed once and read on demand. Read-only; nothing is extracted to disk.
pub struct Bundle {
    zip: Option<zip::ZipArchive<std::fs::File>>,
    /// Unpacked: the .app folder on disk.
    dir: Option<std::path::PathBuf>,
    /// The .app folder name, e.g. `BioVS_en.app`.
    pub app: String,
    /// Relative path -> (zip index, size).
    files: std::collections::HashMap<String, (usize, u64)>,
    dirs: std::collections::HashSet<String>,
}

impl Bundle {
    pub fn open(ipa: &Path) -> Result<Bundle, String> {
        let mut b = Bundle {
            zip: None,
            dir: None,
            app: String::new(),
            files: Default::default(),
            dirs: Default::default(),
        };
        b.dirs.insert(String::new());
        if ipa.is_dir() {
            let payload = if ipa.join("Payload").is_dir() { ipa.join("Payload") } else { ipa.to_path_buf() };
            let app = std::fs::read_dir(&payload)
                .map_err(|e| format!("{}: {e}", payload.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|x| x == "app"))
                .ok_or("no *.app in the Payload folder")?;
            b.app = app.file_name().unwrap().to_string_lossy().into_owned();
            b.dir = Some(app);
            return Ok(b);
        }
        let f = std::fs::File::open(ipa).map_err(|e| format!("{}: {e}", ipa.display()))?;
        let mut zip = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
        for i in 0..zip.len() {
            let Ok(e) = zip.by_index_raw(i) else { continue };
            let name = e.name().to_string();
            let Some(rest) = name.strip_prefix("Payload/") else { continue };
            let Some((app, rel)) = rest.split_once('/') else { continue };
            if !app.ends_with(".app") {
                continue;
            }
            if b.app.is_empty() {
                b.app = app.to_string();
            }
            let rel = rel.trim_end_matches('/');
            // Every parent folder exists even without its own zip entry.
            let mut p = rel;
            while let Some((parent, _)) = p.rsplit_once('/') {
                b.dirs.insert(parent.to_string());
                p = parent;
            }
            if e.is_dir() {
                b.dirs.insert(rel.to_string());
            } else if !rel.is_empty() {
                b.files.insert(rel.to_string(), (i, e.size()));
            }
        }
        if b.app.is_empty() {
            return Err("no Payload/*.app in the ipa".into());
        }
        b.zip = Some(zip);
        Ok(b)
    }

    /// Size of the file at `rel`, or None (also for folders).
    pub fn file_size(&self, rel: &str) -> Option<u64> {
        match &self.dir {
            Some(d) => std::fs::metadata(d.join(rel)).ok().filter(|m| m.is_file()).map(|m| m.len()),
            None => self.files.get(rel).map(|f| f.1),
        }
    }

    pub fn is_dir(&self, rel: &str) -> bool {
        match &self.dir {
            Some(d) => d.join(rel).is_dir(),
            None => self.dirs.contains(rel),
        }
    }

    pub fn read(&mut self, rel: &str) -> Result<Vec<u8>, String> {
        if let Some(d) = &self.dir {
            return std::fs::read(d.join(rel)).map_err(|e| format!("{rel}: {e}"));
        }
        let &(i, size) = self.files.get(rel).ok_or(format!("{rel}: not in the ipa"))?;
        let mut e = self.zip.as_mut().unwrap().by_index(i).map_err(|e| e.to_string())?;
        let mut data = Vec::with_capacity(size as usize);
        e.read_to_end(&mut data).map_err(|e| e.to_string())?;
        Ok(data)
    }

    /// Names directly inside the folder `rel`.
    pub fn list(&self, rel: &str) -> Vec<String> {
        if let Some(d) = &self.dir {
            return std::fs::read_dir(d.join(rel))
                .map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                .unwrap_or_default();
        }
        let prefix = if rel.is_empty() { String::new() } else { format!("{rel}/") };
        let mut out: Vec<String> = self
            .files
            .keys()
            .chain(self.dirs.iter())
            .filter_map(|p| p.strip_prefix(&prefix))
            .filter(|r| !r.is_empty() && !r.contains('/'))
            .map(str::to_string)
            .collect();
        out.sort();
        out.dedup();
        out
    }
}
