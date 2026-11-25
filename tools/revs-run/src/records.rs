//! The colour worn for each single-player record (the Stage records table,
//! 1st to 5th per stage, with the character's portrait).
//!
//! The game's save keeps a record's score and character, not the colour (a
//! port feature). So when the game saves a new record the port notes the
//! colour you wear then, in `<home>/port_records.txt` ("stage score character
//! colour" per line); the records screen draws each row's ball from it.
//! Records from before the port noted them show the character's default
//! colour.
//!
//! `BioVS_UserData::SetSingleModeRanking(this, stage 0..2, score, char)`
//! (0x9da44) inserts a record (scores capped at 999999). The table: per stage
//! 10 words from `this + 0xba0 + stage * 40`: 5 scores (u32), then 5
//! characters (the low byte of each word, signed; -1: none).
//! `GetSingleModeRanking_Score(this, stage, rank)` (0x9db64) reads one: the
//! records screen calls it for the stage on the page.

use crate::{recolor, Host};
use std::path::PathBuf;
use std::sync::Mutex;

pub const SET_RANKING: u32 = 0x9da44;
pub const GET_SCORE: u32 = 0x9db64;
pub const HOOKS: [u32; 2] = [SET_RANKING, GET_SCORE];
const STAGES: u32 = 3;
const RANKS: u32 = 5;
const MAX_SCORE: u32 = 999_999;

#[derive(Default)]
struct State {
    file: Option<PathBuf>,
    /// (stage, score, character, colour), oldest first.
    noted: Vec<(u32, u32, usize, usize)>,
    /// The user data object and the stage last read by the records screen.
    table: Option<(u32, u32)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().unwrap().get_or_insert_with(State::default))
}

/// Reads the noted colours.
pub fn init(home: &std::path::Path) {
    let file = home.join("port_records.txt");
    let mut noted = Vec::new();
    for l in std::fs::read_to_string(&file).unwrap_or_default().lines() {
        if let Some(e) = parse_line(l) {
            noted.push(e);
        }
    }
    with(|s| {
        s.noted = noted;
        s.file = Some(file);
    });
}

fn parse_line(l: &str) -> Option<(u32, u32, usize, usize)> {
    let mut it = l.split_whitespace();
    let stage = it.next()?.parse().ok()?;
    let score = it.next()?.parse().ok()?;
    let ch = recolor::char_index(it.next()?)?;
    let name = it.next()?;
    let colour = recolor::names().iter().position(|n| n.eq_ignore_ascii_case(name))?;
    Some((stage, score, ch, colour))
}

/// At the entry of a HOOKS function.
pub fn on_enter(addr: u32, r: [u32; 4]) {
    let [this, stage, a2, a3] = r;
    if stage >= STAGES {
        return;
    }
    match addr {
        SET_RANKING => {
            let ch = a3 as u8 as i8;
            if ch < 0 || ch as usize >= recolor::characters() {
                return;
            }
            let (score, ch) = (a2.min(MAX_SCORE), ch as usize);
            let colour = recolor::colour_of(recolor::local_player(), ch);
            println!("records: stage {stage} score {score} {} in {}", recolor::char_name(ch), recolor::names()[colour]);
            with(|s| {
                s.noted.push((stage, score, ch, colour));
                let Some(f) = &s.file else { return };
                let text: String = s
                    .noted
                    .iter()
                    .map(|&(st, sc, c, col)| format!("{st} {sc} {} {}\n", recolor::char_name(c), recolor::names()[col]))
                    .collect();
                if let Err(e) = std::fs::write(f, text) {
                    println!("records: can't save {}: {e}", f.display());
                }
            });
        }
        GET_SCORE if a2 < RANKS => with(|s| {
            if s.table != Some((this, stage)) {
                println!("records: the screen shows stage {stage}");
            }
            s.table = Some((this, stage));
        }),
        _ => {}
    }
}

/// The records of the stage the records screen last read, best first: the
/// character and its colour (noted, else the default) per row.
pub fn rows(h: &Host) -> Vec<Option<(usize, usize)>> {
    let Some((this, stage)) = with(|s| s.table) else { return Vec::new() };
    let base = this + 0xba0 + stage * 40;
    (0..RANKS)
        .map(|rank| {
            let score = h.rd32(base + rank * 4);
            let ch = h.rd8(base + 20 + rank * 4) as i8;
            if ch < 0 || ch as usize >= recolor::characters() {
                return None;
            }
            let ch = ch as usize;
            // The latest note of this record, else the default colour.
            let noted = with(|s| s.noted.iter().rev().find(|e| (e.0, e.1, e.2) == (stage, score, ch)).map(|e| e.3));
            Some((ch, noted.unwrap_or_else(|| recolor::default_colour(ch))))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn line_round_trip() {
        let name = crate::recolor::char_name(0);
        let colour = &crate::recolor::names()[1];
        let e = super::parse_line(&format!("2 52082 {name} {colour}")).unwrap();
        assert_eq!(e, (2, 52082, 0, 1));
        assert!(super::parse_line("x").is_none());
    }
}
