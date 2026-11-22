//! Loads one program image: the game from the user's .ipa (or an unpacked
//! Payload folder), or a library file (a dylib) at a chosen load address.

use crate::{ipa, macho};
use std::path::Path;

pub fn load(path: &Path, base: Option<u32>) -> Result<macho::Binary, String> {
    let is_ipa = path.is_dir() || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ipa"));
    if is_ipa {
        let (_, fat) = ipa::read_executable(path)?;
        return macho::Binary::parse_first_slice(&fat);
    }
    let file = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    macho::Binary::parse_lib(&file, base)
}

/// `image.json` in a build folder: which file it was made from, and where it loads.
pub fn read_image_json(dir: &Path) -> Option<(std::path::PathBuf, Option<u32>)> {
    let s = std::fs::read_to_string(dir.join("image.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let path = std::path::PathBuf::from(v["path"].as_str()?);
    let base = v["base"].as_str().and_then(|b| u32::from_str_radix(b.trim_start_matches("0x"), 16).ok());
    Some((path, base))
}
