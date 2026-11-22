//! Small changes to the game's settings and texts (touchHLE patches v15-v17
//! rebuilt as port code; the game's files are not changed).
//!
//! Controls > Other Settings: "Icon Transparency" (BioVS_UserData byte 6) and
//! "Shake Sensitivity" (byte 7) stay 0, as the controller play needs: the
//! setters store 0 and every frame clears both bytes (an old save's values
//! too). The settings object is a static one at USER_DATA.
//!
//! Spanish "Single Player": the game says "Monojugador"; the port shows
//! "Un jugador" (in the menu button, its help text and How to Play). The text
//! tables stay in memory as loaded from the user's game; the first time a
//! string is read through Get_Game / Get_HowtoInfo it is fixed in place at the
//! same length (the spare character becomes a space before the line break).

use crate::Host;
use std::collections::HashSet;
use std::sync::Mutex;

/// `Bio4_ResTextSys::Get_Game(long)`: text = [this+8] + [[this+4] + id*48 + lang*4].
pub const GET_GAME: u32 = 0x6e2c4;
/// `Bio4_ResTextSys::Get_HowtoInfo(long)`: entry e = [this+0x58] + id*48; if
/// [e + lang*4 + 0x18] != 0, text = [this+0x5c] + [e + lang*4].
pub const GET_HOWTO_INFO: u32 = 0x6e424;
/// Strings already looked at.
static SEEN: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

/// Spanish words replaced (same length in UTF-16 units minus one).
const WORDS: [(&str, &str); 2] = [("Monojugador", "Un jugador"), ("MONOJUGADOR", "UN JUGADOR")];

/// `BioVS_UserData` (the game's settings, saved with the game).
const USER_DATA: u32 = 0x38f56c;
/// `BioVS_UserData::SetIconAlpha(char)`, `::SetShakeSensitivity(char)`.
pub const SET_ICON_ALPHA: u32 = 0x9cfc8;
pub const SET_SHAKE: u32 = 0x9cfe4;
pub const HOOKS: [u32; 4] = [SET_ICON_ALPHA, SET_SHAKE, GET_GAME, GET_HOWTO_INFO];

/// At the entry of a HOOKS function: does its job the port's way. Returns
/// true when the game's own function must be skipped.
pub fn on_enter(h: &Host, addr: u32, this: u32, id: u32) -> bool {
    match addr {
        GET_GAME | GET_HOWTO_INFO => {
            let lang = h.rd32(this + 0x8c);
            let text = if addr == GET_GAME {
                h.rd32(this + 8).wrapping_add(h.rd32(h.rd32(this + 4) + id * 48 + lang * 4))
            } else {
                let e = h.rd32(this + 0x58) + id * 48;
                if h.rd32(e + lang * 4 + 0x18) == 0 {
                    return false;
                }
                h.rd32(this + 0x5c).wrapping_add(h.rd32(e + lang * 4))
            };
            let mut seen = SEEN.lock().unwrap();
            if seen.get_or_insert_with(HashSet::new).insert(text) {
                fix_text(h, text);
            }
            return false;
        }
        SET_ICON_ALPHA => h.wr8(this + 6, 0),
        SET_SHAKE => h.wr8(this + 7, 0),
        _ => return false,
    }
    true
}

/// Replaces WORDS in the UTF-16 string at `p` (ends with a 0), keeping its
/// length: the rest of the line moves left one unit and a space goes before
/// the line break (or the end).
fn fix_text(h: &Host, p: u32) {
    let rd = |i: u32| u16::from_le_bytes([h.rd8(p + 2 * i), h.rd8(p + 2 * i + 1)]);
    let wr = |i: u32, v: u16| {
        h.wr8(p + 2 * i, v as u8);
        h.wr8(p + 2 * i + 1, (v >> 8) as u8);
    };
    let mut s: Vec<u16> = Vec::new();
    while s.len() < 4096 {
        let u = rd(s.len() as u32);
        if u == 0 {
            break;
        }
        s.push(u);
    }
    let changed = replace_words(&mut s);
    if changed {
        for (i, &u) in s.iter().enumerate() {
            wr(i as u32, u);
        }
    }
}

/// WORDS replaced in `s`, same length: the rest of each changed line moves
/// left and a space goes before its line break (or the end).
fn replace_words(s: &mut Vec<u16>) -> bool {
    let mut changed = false;
    for (from, to) in WORDS {
        let (from, to): (Vec<u16>, Vec<u16>) = (from.encode_utf16().collect(), to.encode_utf16().collect());
        while let Some(i) = s.windows(from.len()).position(|w| w == from) {
            let end = s[i..].iter().position(|&u| u == 0x0a).map_or(s.len(), |k| i + k);
            let mut line: Vec<u16> = to.clone();
            line.extend_from_slice(&s[i + from.len()..end]);
            line.push(u16::from(b' '));
            s.splice(i..end, line);
            changed = true;
        }
    }
    changed
}

/// Once per drawn frame.
pub fn each_frame(h: &Host) {
    h.wr8(USER_DATA + 6, 0);
    h.wr8(USER_DATA + 7, 0);
}

#[cfg(test)]
mod tests {
    fn fixed(s: &str) -> String {
        let mut v: Vec<u16> = s.encode_utf16().collect();
        super::replace_words(&mut v);
        String::from_utf16(&v).unwrap()
    }

    #[test]
    fn spanish_single_player() {
        // The touchHLE v15 / v16 results, same length as before.
        assert_eq!(fixed("Monojugador"), "Un jugador ");
        assert_eq!(fixed("MONOJUGADOR"), "UN JUGADOR ");
        assert_eq!(fixed("El modo Monojugador te permite
jugar"), "El modo Un jugador te permite 
jugar");
        assert_eq!(fixed("en Monojugador te ayudará
"), "en Un jugador te ayudará 
");
        assert_eq!(fixed("Partida local"), "Partida local");
    }
}
