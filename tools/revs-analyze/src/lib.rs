//! Shared readers for the port tools: the .ipa and the game's Mach-O.
//!
//! RULE: these read the user's own .ipa at run time. Nothing derived from
//! the game is stored in this repository.

pub mod image;
pub mod ipa;
pub mod macho;
