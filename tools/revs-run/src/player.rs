//! The local player's state in a match, from the game's own player code
//! (`GM_CtrlMain_Player`), for the port's gameplay input.
//!
//! `GM_CtrlMain_Player::_Change_State(this, short state)` (0x13b5c8) switches
//! the player's state machine (move, aim, reload, dash, ...). The port notes
//! the last state. REVS_PLAYERLOG=1 also prints the HUD's input handlers,
//! reloads and equips as they run.

use crate::Host;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::Mutex;

pub const CHANGE_STATE: u32 = 0x13b5c8;
/// `GM_CtrlMain_Player::Main_Mission(this, GM_CtrlMain*)`: the player's step.
const MAIN_MISSION: u32 = 0x14d86c;
/// `GM_CtrlLyt_Input` (the match HUD's input, a static object) +0x24: the
/// buttons and gestures pressed this step, as the HUD's handlers set them
/// (`_HdlInput_*`; `Clear_Press` empties it); +0x28: those held down.
const INPUT_TRIG: u32 = 0x39299c + 0x24;
const INPUT_HELD: u32 = 0x39299c + 0x28;

/// Bits of the HUD's input word. The gun button aims, or fires while aiming;
/// "Post" lowers the gun; a tap on the move stick reloads; a double flick of
/// the stick back turns around.
pub const GUN: u32 = 0x80;
pub const POST: u32 = 0x10;
pub const EVENT: u32 = 0x20;
pub const RELOAD: u32 = 0x3000;
/// A double flick of the move stick, per direction (the stick handler's
/// directions 1-4). Back turns around; forward is Wesker's dash; left and
/// right dodge while aiming (`_State_AtkAimM` tests all four).
pub const FLICK_FORWARD: u32 = 0x4000;
pub const FLICK_BACK: u32 = 0x8000;
pub const FLICK_LEFT: u32 = 0x10000;
pub const FLICK_RIGHT: u32 = 0x20000;

/// Player states (`_Change_State`): raising the gun (`_State_AtkAimS`),
/// aiming (`_State_AtkAimM`), firing (`_State_AtkMain`).
const MOVE: u16 = 0;
const AIM_START: u16 = 1;
const AIM: u16 = 2;
const FIRE: u16 = 6;
/// Research only (REVS_PLAYERLOG): reload, equip, the HUD layout's handlers.
const SET_AC_RELOAD: u32 = 0x154d64;
const EQUIP: u32 = 0x13b454;
const SET_EQUIP_CTG: u32 = 0x13b838;
const QUICK_EQP_MODE: u32 = 0x6cfe8;
const SET_EVE_BTN: u32 = 0x6b99c;
const HDL_PAD: u32 = 0x6bc54;
const HDL_KNIF: u32 = 0x6be08;
const HDL_GUN: u32 = 0x6beb0;
const HDL_EVE: u32 = 0x6bf58;
const HDL_POST: u32 = 0x6b880;
const HDL_SCOPE: u32 = 0x6b894;
const AIM_CANCEL: u32 = 0x13b6f0;
/// `GM_CtrlLyt_AC::SelectItem(this, n)`: the inventory selects grid item n
/// (-1: none; the grid is the case's items without the knife, in order).
const SELECT_ITEM: u32 = 0x953ac;
pub fn hooks() -> Vec<u32> {
    let mut v = vec![CHANGE_STATE, MAIN_MISSION, SELECT_ITEM, SET_AC_RELOAD, EQUIP, SET_EQUIP_CTG, QUICK_EQP_MODE, SET_EVE_BTN, HDL_PAD, HDL_KNIF, HDL_GUN, HDL_EVE, HDL_POST, HDL_SCOPE, AIM_CANCEL];
    if log() {
        v.extend(STATE_FNS.iter().map(|e| e.0));
    }
    v
}

/// Research only (REVS_PLAYERLOG): the state functions, to name the states.
const STATE_FNS: [(u32, &str); 38] = [
    (0x13c924, "_State_MvtPlay"),
    (0x13cb20, "_State_Obj_LadderDown"),
    (0x1405fc, "_State_DashAttack"),
    (0x1406a0, "_State_Team_HelpFromTrap"),
    (0x1407cc, "_State_Team_BeHelpedFromDying"),
    (0x140a98, "_State_Team_HelpFromDying"),
    (0x140e48, "_State_Obj_Equip"),
    (0x141294, "_State_Obj_SetFixItem"),
    (0x141548, "_State_Obj_FlyDown"),
    (0x141928, "_State_Obj_VerLadderClimb"),
    (0x141b6c, "_State_Obj_LadderClimb"),
    (0x141df4, "_State_Intrude"),
    (0x14219c, "_State_Wep_EqpRemove"),
    (0x1422f8, "_State_Down"),
    (0x142c6c, "_State_Damage"),
    (0x142ef4, "_State_StandReload"),
    (0x143044, "_State_AtkReload"),
    (0x14620c, "_State_AtkMain"),
    (0x146b8c, "_State_AtkAimE"),
    (0x146cd0, "_State_AtkAimScope"),
    (0x147cdc, "_State_AtkAimMove"),
    (0x1482f8, "_State_AtkAimM"),
    (0x148bc0, "_State_AtkAimS"),
    (0x148fac, "_State_Move"),
    (0x149c7c, "_State_Dash"),
    (0x14a574, "_State_Obj_Trap"),
    (0x14a9f4, "_State_Obj_KickTimeBonus"),
    (0x14abe0, "_State_Obj_DoorOpen"),
    (0x14b01c, "_State_Obj_LadderThrow"),
    (0x14b200, "_State_Obj_LadderStand"),
    (0x14b4a0, "_State_Wep_MNonToSAim"),
    (0x14b660, "_State_Wep_SAimToMNon"),
    (0x14b878, "_State_Wep_MNonToMAim"),
    (0x14ba94, "_State_Wep_MAimToMNon"),
    (0x14bcb4, "_State_Wep_MNonToMSta"),
    (0x14bef0, "_State_Wep_MStaToMNon"),
    (0x14c158, "_State_Respawn"),
    (0x14d3f0, "_State_HoldDamage"),
];

#[derive(Default)]
struct State {
    /// The player object and its last state.
    state: Option<(u32, u16)>,
}

static STATE: Mutex<State> = Mutex::new(State { state: None });

fn log() -> bool {
    std::env::var_os("REVS_PLAYERLOG").is_some()
}

/// At the entry of a HOOKS function.
pub fn on_enter(h: &Host, frame: u64, addr: u32, r: [u32; 4]) {
    if addr == MAIN_MISSION {
        let held = HELD.load(Ordering::Relaxed);
        if held != 0 {
            h.wr32(INPUT_HELD, h.rd32(INPUT_HELD) | held);
        }
        let dir = SWAP.swap(0, Ordering::Relaxed);
        if dir != 0 {
            swap_weapon(h, frame, r[0], dir);
        }
        let bits = PENDING.swap(0, Ordering::Relaxed);
        if bits != 0 {
            h.wr32(INPUT_TRIG, h.rd32(INPUT_TRIG) | bits);
            if log() {
                println!("player f{frame}: trig {bits:#x}");
            }
        }
        return;
    }
    if addr == CHANGE_STATE {
        let st = r[1] as u16;
        let mut s = STATE.lock().unwrap();
        if log() && s.state != Some((r[0], st)) {
            println!("player f{frame}: {:#x} state {st}", r[0]);
        }
        s.state = Some((r[0], st));
        return;
    }
    if addr == SELECT_ITEM {
        SELECTED.store(r[1] as i32, Ordering::Relaxed);
        return;
    }
    if !log() {
        return;
    }
    if let Some(&(_, n)) = STATE_FNS.iter().find(|e| e.0 == addr) {
        static LAST: Mutex<u32> = Mutex::new(0);
        let mut l = LAST.lock().unwrap();
        if *l != addr {
            println!("player f{frame}: runs {n}");
        }
        *l = addr;
        return;
    }
    let name = match addr {
        SET_AC_RELOAD => "Set_AC_Reload",
        EQUIP => "Equip",
        SET_EQUIP_CTG => "Set_EquipCtg",
        QUICK_EQP_MODE => "Set_QuickEqpMode",
        SET_EVE_BTN => "Set_EveBtn",
        HDL_PAD => "HdlInput_Pad",
        HDL_KNIF => "HdlInput_KnifBtn",
        HDL_GUN => "HdlInput_GunBtn",
        HDL_EVE => "HdlInput_EveBtn",
        HDL_POST => "HdlInput_PostBtn",
        HDL_SCOPE => "HdlInput_ScopeBtn",
        AIM_CANCEL => "_Check_AimCancel",
        _ => return,
    };
    // The pad handler runs for every stick move: only its messages' kinds.
    if addr == HDL_PAD {
        static LAST: Mutex<u32> = Mutex::new(u32::MAX);
        let mut l = LAST.lock().unwrap();
        if *l == r[2] {
            return;
        }
        *l = r[2];
    }
    println!("player f{frame}: {name}({:#x}, {:#x}, {:#x}, {:#x})", r[0], r[1], r[2], r[3]);
}

/// The local player's last state (`_Change_State`), if a match has one.
pub fn state() -> Option<u16> {
    STATE.lock().unwrap().state.map(|s| s.1)
}

/// Whether `addr` is one of hooks().
pub fn is_hook(addr: u32) -> bool {
    static H: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    H.get_or_init(hooks).contains(&addr)
}

/// Bits for the HUD's input word, set at the player's next step.
static PENDING: AtomicU32 = AtomicU32::new(0);

/// Presses the game's own trigger bits (`INPUT_TRIG`) at the player's next step.
pub fn trigger(bits: u32) {
    PENDING.fetch_or(bits, Ordering::Relaxed);
}

/// Bits held down (`INPUT_HELD`) at every player step until changed.
static HELD: AtomicU32 = AtomicU32::new(0);

/// Holds these bits down from the player's next step on (0: none).
pub fn hold(bits: u32) {
    HELD.store(bits, Ordering::Relaxed);
}

/// The player has the gun up (raising it, aiming or firing).
pub fn aiming() -> bool {
    matches!(state(), Some(AIM_START | AIM | FIRE))
}

/// The player can fire (aiming, or between shots).
pub fn can_fire() -> bool {
    matches!(state(), Some(AIM | FIRE))
}

/// `GM_CtrlParam` (a static object) is also the player's attaché case
/// (`BioVS_GameAttacheCase`): 10 item slots (`Bio4_ItemNode`, 0x2c bytes:
/// prev, next, item, count) from +4; +0x1c4 the slot of the gun in hand,
/// +0x1c8 the knife's (`GetEqpWeapon` 0 / 1).
const CASE: u32 = 0x392288;
const SLOTS: u32 = CASE + 4;
const SLOT_SIZE: u32 = 0x2c;
const EQP_GUN: u32 = CASE + 0x1c4;
const EQP_KNIFE: u32 = CASE + 0x1c8;
/// Player +0x15c: the slot to equip next (`GM_CtrlMain_Player::Equip` sets
/// it; -2: none waiting).
const EQUIP_REQ: u32 = 0x15c;
const NO_REQ: u32 = 0xffff_fffe;

/// Weapon swap waiting for the player's next step: +1 next, -1 previous.
static SWAP: AtomicI32 = AtomicI32::new(0);

/// Takes the next (+1) or previous (-1) main weapon out at the player's next step.
pub fn swap(dir: i32) {
    SWAP.store(dir.signum(), Ordering::Relaxed);
}

/// The slots holding a main weapon, in case order: a weapon (item type,
/// +0xc, 0) that isn't thrown (+0xe 0: grenades) and isn't the knife.
fn main_weapons(h: &Host) -> Vec<u32> {
    let knife = h.rd32(EQP_KNIFE);
    (0..10)
        .filter(|i| {
            let node = SLOTS + i * SLOT_SIZE;
            let item = h.rd32(node + 8);
            item != 0 && node != knife && h.rd8(item + 0xc) == 0 && h.rd8(item + 0xe) == 0
        })
        .collect()
}

/// Asks the game to equip the main weapon after (dir 1) or before (-1) the
/// one in hand, the way the quick-equip ring does (`Equip`: the slot index
/// into +0x15c), when standing or aiming and no other equip is waiting.
/// With something else in hand (grenades) either way gives the first main
/// weapon (the starting handgun).
fn swap_weapon(h: &Host, frame: u64, player: u32, dir: i32) {
    if !matches!(state(), Some(MOVE | AIM)) || h.rd32(player + EQUIP_REQ) != NO_REQ {
        return;
    }
    let list = main_weapons(h);
    let held = h.rd32(EQP_GUN).wrapping_sub(SLOTS) / SLOT_SIZE;
    // Something else in hand (a grenade): back to the first main weapon,
    // the starting handgun.
    let to = match list.iter().position(|&i| i == held) {
        Some(_) if list.len() < 2 => return,
        Some(at) => list[(at as i32 + dir).rem_euclid(list.len() as i32) as usize],
        None => match list.first() {
            Some(&first) => first,
            None => return,
        },
    };
    h.wr32(player + EQUIP_REQ, to);
    if log() {
        println!("player f{frame}: swap slot {held} -> {to} (of {list:?})");
    }
}

/// The inventory's selected grid item (`SelectItem`; -1: none).
static SELECTED: AtomicI32 = AtomicI32::new(-1);

/// The case slots the inventory grid shows, in order: every item but the knife.
fn grid_slots(h: &Host) -> Vec<u32> {
    let knife = h.rd32(EQP_KNIFE);
    (0..10)
        .filter(|i| {
            let node = SLOTS + i * SLOT_SIZE;
            node != knife && h.rd32(node + 8) != 0
        })
        .collect()
}

/// The inventory grid's place of the item in hand (gun or grenades).
pub fn equipped_grid_index(h: &Host) -> Option<usize> {
    let held = h.rd32(EQP_GUN).wrapping_sub(SLOTS) / SLOT_SIZE;
    grid_slots(h).iter().position(|&i| i == held)
}

/// The item selected in the inventory is used, not equipped: not a weapon
/// (item type, +0xc, not 0: the spray, herbs...). Its first command then
/// says "Use".
pub fn selected_is_usable(h: &Host) -> bool {
    let Ok(n) = usize::try_from(SELECTED.load(Ordering::Relaxed)) else { return false };
    let Some(&slot) = grid_slots(h).get(n) else { return false };
    let item = h.rd32(SLOTS + slot * SLOT_SIZE + 8);
    item != 0 && h.rd8(item + 0xc) != 0
}
