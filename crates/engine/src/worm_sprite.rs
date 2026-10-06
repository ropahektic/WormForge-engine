//! Slot-scoped worm art. Stock calculates animation, slope, position and flip;
//! this wrap changes only the resulting sprite id before the stock blit.
//!
//! `weaponlnk` / `u` / `d` are custom aim-hold sheets. Stock may output:
//! - sprite id 0 while states 0x67..=0x69 (generic custom-weapon path), or
//! - a donor fire-path sheet (`wbaz*`, `wbaz2*`, …, `wthrgrn*`, …) when
//!   `copy_from` is bazooka / homing / fuse / cluster.
//! We map all of those to the claimed link sheets and keep stock's aim phase.
//!
//! The uzi, minigun, shotgun and handgun aim sheets map to the link sheets
//! too, their draw sheets to `wthrow`*, and their fire sheets to
//! `weaponfire`*, which keeps stock's aim phase as the link sheets do.
//!
//! `wthrow`* and the donor draw/appear sheets (`wbazlnk*`, `wgrnlnk*`, …) are
//! draw/undraw. Unset throw claims copy the aim sheet and freeze; real draw
//! GIFs play via `drive_phase`.
//!
//! Empty sprite metadata is initialized separately: the stock animator saves
//! these outputs even when GetSpriteInfo(0) fails, so stack garbage can enter
//! its serialized simulation state.

use crate::{log_line, sprite};
use game::{
    entity::read_i32,
    offsets::{WORM_SELECTED_WEAPON, WORM_SLOPE_DIR, WORM_STATE, WORM_TURN_ACTIVE},
};
use std::{
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicPtr, Ordering},
    },
};
use weapons::StockWeapon;

static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_SPRITE_INFO: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_HOLD: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// The combined Skip Go/Surrender claim can show stock Surrender's armed pose
/// while Ctrl selects its alternate stock fire record.
static SKIP_GO_SURRENDER_POSE: AtomicBool = AtomicBool::new(false);

pub fn claim_skip_go_surrender_pose() {
    SKIP_GO_SURRENDER_POSE.store(true, Ordering::Relaxed);
}

fn surrender_pose(sprite: u32) -> u32 {
    if sprite & game::offsets::SPRITE_REVERSE != 0 {
        return sprite;
    }
    let stock = (sprite & 0xffff) as i32;
    let replacement = match stock {
        391..=393 => 448 + stock - 391, // wskplnk* -> wsurlnk*: armed pose
        263..=265 => 242 + stock - 263, // wskipgo* -> wsurndr*: fire pose
        _ => return sprite,
    };
    (sprite & !0xffff) | replacement as u32
}

#[cfg(test)]
mod surrender_pose_tests {
    use super::surrender_pose;
    use game::offsets::SPRITE_REVERSE;

    #[test]
    fn ctrl_pose_uses_surrender_art_and_preserves_sprite_flags() {
        for (skip, surrender) in [(391, 448), (392, 449), (393, 450), (263, 242), (264, 243), (265, 244)] {
            assert_eq!(surrender_pose(skip), surrender);
            assert_eq!(surrender_pose(0x40000 | skip), 0x40000 | surrender);
            assert_eq!(surrender_pose(SPRITE_REVERSE | skip), SPRITE_REVERSE | skip);
        }
        assert_eq!(surrender_pose(525), 525);
    }
}
#[cfg(test)]
mod toggle_tests {
    use super::*;

    #[test]
    fn a_toggled_worm_draws_the_toggle_sheet_of_its_slot() {
        if let (Ok(mut c), Ok(mut t)) = (TOGGLE_CLAIMS.lock(), TOGGLES.lock()) {
            *c = vec![(35, 334, 900), (35, 335, 901)];
            *t = vec![(0x1000, true, false), (0x2000, false, false)];
        }
        assert_eq!(toggled_sheet(0x1000, 35, 334), Some(900));
        assert_eq!(toggled_sheet(0x1000, 35, 335), Some(901));
        assert_eq!(toggled_sheet(0x1000, 35, 336), None, "a pose the toggle does not name");
        assert_eq!(toggled_sheet(0x1000, 34, 334), None, "another slot");
        assert_eq!(toggled_sheet(0x2000, 35, 334), None, "toggled off");
        assert_eq!(toggled_sheet(0x3000, 35, 334), None, "never toggled");
        // Only this test's state: clear() would reset claims other tests use.
        if let (Ok(mut c), Ok(mut t)) = (TOGGLE_CLAIMS.lock(), TOGGLES.lock()) {
            c.clear();
            t.clear();
        }
    }
}


/// Placed weapons (fire type 2, method 2): WA picks what the worm holds from
/// the flight sprite (+0x60), stock sprite -> held sheet (`Weapon__hold_sheet`,
/// 0x51C060). A pack sheet there is an id WA does not know, and the worm held
/// nothing. `HELD` remembers the stock sheet of the sprite a pack replaced.
const PLACED_HELD: [(i32, i32); 8] = [
    (74, 337),  // mingvase -> wvaslnk
    (78, 334),  // dynamite -> wdynlnk
    (153, 343), // sheepwlk -> wshplnk
    (162, 346), // old woman -> wwmnlnk
    (163, 355), // old woman -> wwmn2lk
    (164, 340), // cow -> wcowlnk
    (166, 352), // mole -> wmollnk
    (173, 349), // skunk -> wsknlnk
];
static HELD: Mutex<Vec<(usize, i32)>> = Mutex::new(Vec::new());

type SpriteInfoFn = unsafe extern "thiscall" fn(*mut u8, u32, *mut u32, *mut u32, *mut u32) -> u32;

static CLAIMS: Mutex<Vec<(usize, i32, i32)>> = Mutex::new(Vec::new());
/// Per worm, the slot whose art it was last drawn with, and the stock
/// sheets that art replaced. When a weapon's use ends -- a gun's last shot
/// -- WA selects the next weapon at once, yet goes on drawing the old one's
/// sheets while it puts it away; the stock gun showed for those frames. So
/// while WA still draws sheets that slot's art replaced, or puts a weapon
/// away (its draw sheet reversed, in any slope variant), they are replaced
/// with that slot's art, whatever is selected, until WA draws anything else.
static LAST_SLOT: Mutex<Vec<(usize, usize, Vec<u32>)>> = Mutex::new(Vec::new());

/// The slot whose art to draw WA's `sprite` with: the one it was last
/// replaced for while WA is still putting that weapon away, else the
/// selected one. A reversed sprite is the put-away itself.
///
/// The put-away happens with the worm standing. Once it aims (`aiming`:
/// states 0x67..=0x69), the selected weapon is out, even where it draws
/// the same stock sheet as the old one: the Igniter, on the homing
/// missile's slot, is drawn out on the bazooka's `wbazlnk`, and a bazooka
/// drawn out next took the Igniter's empty-handed posture.
fn pose_slot(selected: usize, last: Option<(usize, &[u32])>, sprite: u32, aiming: bool) -> usize {
    let putting_away = sprite & game::offsets::SPRITE_REVERSE != 0;
    match last {
        Some((slot, sheets))
            if slot != selected
                && !aiming
                && (putting_away || sheets.contains(&(sprite & 0xffff))) =>
        {
            slot
        }
        _ => selected,
    }
}

fn last_slot(worm: usize) -> Option<(usize, Vec<u32>)> {
    LAST_SLOT
        .lock()
        .ok()?
        .iter()
        .find(|(w, _, _)| *w == worm)
        .map(|(_, s, sheets)| (*s, sheets.clone()))
}

/// Note that `sheet` was replaced with `slot`'s art for this worm.
fn remember_sheet(worm: usize, slot: usize, sheet: u32) {
    if let Ok(mut g) = LAST_SLOT.lock() {
        match g.iter_mut().find(|(w, _, _)| *w == worm) {
            Some((_, s, sheets)) if *s == slot => {
                if !sheets.contains(&sheet) {
                    sheets.push(sheet);
                }
            }
            Some(entry) => *entry = (worm, slot, vec![sheet]),
            None => g.push((worm, slot, vec![sheet])),
        }
    }
}

fn forget_slot(worm: usize) {
    if let Ok(mut g) = LAST_SLOT.lock() {
        g.retain(|(w, _, _)| *w != worm);
    }
}
/// Per-team-colour sheets: (slot, alliance, pose, sheet). `CLAIMS` still
/// holds each pose's default, so pose and phase logic never looks at team;
/// only the final sheet id is swapped for the drawing worm's colour.
static TEAM_CLAIMS: Mutex<Vec<(usize, u8, i32, i32)>> = Mutex::new(Vec::new());
/// `worm_sprites_toggle`: the sheets a claim's slot draws instead while the
/// worm's toggle is on: (slot, pose, sheet). Project X's Electromagnet: the
/// red magnet while attracting, the blue one while repelling.
static TOGGLE_CLAIMS: Mutex<Vec<(usize, i32, i32)>> = Mutex::new(Vec::new());
/// Per worm: (worm, toggle on, the + key held last frame). The engine reads
/// the synchronized key itself; a pack keeping
/// its own mode by the same rule (a press while the turn's worm has the slot
/// selected) stays in step with it.
static TOGGLES: Mutex<Vec<(usize, bool, bool)>> = Mutex::new(Vec::new());
/// Slots whose `weaponlnk*` sheets are one-shot animations (equip, unfold),
/// not aim-angle sheets: `params.worm_sprites_animate = true`.
static ANIMATE: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Per worm: which claimed pose is playing, how far through it, and how many
/// game frames since it was last drawn: (worm, pose, phase, sheet,
/// reversed, idle).
///
/// Used for draw/undraw-style sheets where stock only hands 0x0 / 0x8000.
/// Aiming (`weaponlnk`*) keeps stock's aim-angle phase instead.
static ANIM: Mutex<Vec<(usize, i32, u32, i32, bool, u32)>> = Mutex::new(Vec::new());
/// Game frames a claimed pose may go undrawn and still continue. Any longer
/// and its next draw starts over: several weapons share one draw sheet (PX's
/// posture), so drawing one out after another, with no put-away between,
/// is the same pose and sheet and would otherwise start on the last frame.
/// A frame or two of slack covers render frames that skip a game frame.
const IDLE_RESTART: u32 = 2;

const LINK: i32 = -1;
const LINK_U: i32 = -2;
const LINK_D: i32 = -3;
const FIRE: i32 = -4;
const FIRE_U: i32 = -5;
const FIRE_D: i32 = -6;
const THROW: i32 = 591;
const THROW_U: i32 = 592;
const THROW_D: i32 = 593;

/// Stock aim-hold triads (flat, up, down) remapped to `weaponlnk*`.
/// Covers bazooka / HM / pigeon / MB barrels plus fuse/cluster throw-holds.
const AIM_HOLD: &[(i32, i32, i32)] = &[
    (525, 526, 527), // wbaz
    (531, 532, 533), // wbaz2
    (534, 535, 536), // wbaz3
    (685, 686, 687), // wbaz4
    (688, 689, 690), // wbaz5
    (528, 529, 530), // wflm
    (576, 577, 578), // wthrgrn
    (579, 580, 581), // wthrcls
    (582, 583, 584), // wthrban
    (585, 586, 587), // wthrpbm
    (588, 589, 590), // wthrhgr
    (564, 565, 566), // wuzi
    (570, 571, 572), // wmini
    (543, 544, 545), // wshotg
    (558, 559, 560), // whandg
];

/// Stock draw/appear triads remapped to `wthrow*`.
const AIM_DRAW: &[(i32, i32, i32)] = &[
    (591, 592, 593), // wthrow
    (436, 437, 438), // wbazlnk
    (442, 443, 444), // wbz2lnk
    (445, 446, 447), // wbz3lnk
    (691, 692, 693), // wbz4lnk
    (694, 695, 696), // wbz5lnk
    (439, 440, 441), // wflmlnk
    (313, 314, 315), // wgrnlnk
    (316, 317, 318), // wclslnk
    (319, 320, 321), // wbanlnk
    (322, 323, 324), // wpbmlnk
    (325, 326, 327), // whgrlnk
    (488, 489, 490), // wuzilnk
    (503, 504, 505), // wmgnlnk (minigun; wminlnk is the mine's)
    (491, 492, 493), // wshglnk
    (500, 501, 502), // whgnlnk
];

/// Stock gun fire triads remapped to `weaponfire*`. Like the aim holds,
/// their phase is the aim angle.
const AIM_FIRE: &[(i32, i32, i32)] = &[
    (567, 568, 569), // wuzif
    (573, 574, 575), // wminif
    (549, 550, 551), // wshotf
    (561, 562, 563), // whandf
];

fn is_link(pose: i32) -> bool {
    matches!(pose, LINK | LINK_U | LINK_D)
}

fn is_throw(pose: i32) -> bool {
    matches!(pose, THROW | THROW_U | THROW_D)
}

/// WA's own throw: `wthrow` played forward once the worm is out of the
/// aiming states (0x67..=0x69), in the firing states and just after. It is
/// not a draw, so it stays WA's, as does the hand's way back (`wthrbak`)
/// that follows. Claimed as the draw pose, a thrown weapon threw with the
/// pack's draw posture: a worm reaching for a weapon it had just thrown.
fn is_wa_throw(sprite: u32, state: i32) -> bool {
    let sheet = (sprite & 0xffff) as i32;
    is_throw(sheet)
        && sprite & game::offsets::SPRITE_REVERSE == 0
        && !(0x67..=0x69).contains(&state)
}

fn is_fire(pose: i32) -> bool {
    matches!(pose, FIRE | FIRE_U | FIRE_D)
}

/// The draw pose matching an undraw pose, and vice versa.
fn partner_pose(pose: i32) -> Option<i32> {
    Some(match pose {
        LINK => THROW,
        LINK_U => THROW_U,
        LINK_D => THROW_D,
        THROW => LINK,
        THROW_U => LINK_U,
        THROW_D => LINK_D,
        _ => return None,
    })
}

/// True when one sheet serves both the draw and the undraw pose.
///
/// That is the shape the web-editor aim wizard produces, and only there does
/// the phase mean an aim angle instead of a frame number. A pack supplying two
/// distinct sheets wants two animations played, so it must not be treated this
/// way.
fn is_aim_sheet(claims: &[(usize, i32, i32)], slot: usize, pose: i32, id: i32) -> bool {
    let Some(partner) = partner_pose(pose) else {
        return false;
    };
    claims
        .iter()
        .any(|(s, p, sheet)| *s == slot && *p == partner && *sheet == id)
}

fn slope_link(slope: i32) -> i32 {
    match slope.cmp(&0) {
        std::cmp::Ordering::Less => LINK_U,
        std::cmp::Ordering::Greater => LINK_D,
        std::cmp::Ordering::Equal => LINK,
    }
}

fn triad_to(
    stock: i32,
    flat: i32,
    up: i32,
    down: i32,
    to_flat: i32,
    to_up: i32,
    to_down: i32,
) -> Option<i32> {
    if stock == flat {
        Some(to_flat)
    } else if stock == up {
        Some(to_up)
    } else if stock == down {
        Some(to_down)
    } else {
        None
    }
}

fn pose_key(original: u32, state: i32, slope: i32) -> i32 {
    let stock = (original & 0xffff) as i32;
    if stock == 0 && matches!(state, 0x67..=0x69) {
        return slope_link(slope);
    }
    for &(flat, up, down) in AIM_HOLD {
        if let Some(pose) = triad_to(stock, flat, up, down, LINK, LINK_U, LINK_D) {
            return pose;
        }
    }
    for &(flat, up, down) in AIM_DRAW {
        if let Some(pose) = triad_to(stock, flat, up, down, THROW, THROW_U, THROW_D) {
            return pose;
        }
    }
    for &(flat, up, down) in AIM_FIRE {
        if let Some(pose) = triad_to(stock, flat, up, down, FIRE, FIRE_U, FIRE_D) {
            return pose;
        }
    }
    stock
}

/// The worm on `slot` keeps holding what WA gives `stock_sprite`, the placed
/// weapon's flight sprite before a pack replaced it. False when WA has no
/// held sheet for that sprite.
pub fn hold_as(slot: usize, stock_sprite: i32) -> bool {
    let Some(&(_, sheet)) = PLACED_HELD.iter().find(|(s, _)| *s == stock_sprite) else {
        return false;
    };
    if let Ok(mut held) = HELD.lock() {
        held.retain(|(s, _)| *s != slot);
        held.push((slot, sheet));
    }
    true
}

unsafe extern "C" fn held_sheet(worm: *mut u8) -> i32 {
    if worm.is_null() {
        return 0;
    }
    let slot = read_i32(worm, WORM_SELECTED_WEAPON) as usize;
    HELD.lock()
        .ok()
        .and_then(|held| held.iter().find(|(s, _)| *s == slot).map(|(_, id)| *id))
        .unwrap_or(0)
}

/// `Weapon__hold_sheet`: EAX = entry, stack = worm, RET 4, sheet in EAX.
/// Only when WA finds none is the remembered stock sheet asked for; ECX and
/// EDX survive that call as they survive WA's own.
#[unsafe(naked)]
unsafe extern "C" fn hold_sheet_hook() {
    core::arch::naked_asm!(
        "push dword ptr [esp + 4]",
        "call dword ptr [{original}]",
        "test eax, eax",
        "jnz 2f",
        "push ecx",
        "push edx",
        "push dword ptr [esp + 12]",
        "call {held}",
        "add esp, 4",
        "pop edx",
        "pop ecx",
        "2:",
        "ret 4",
        original = sym ORIGINAL_HOLD,
        held = sym held_sheet,
    );
}

pub fn clear() {
    SKIP_GO_SURRENDER_POSE.store(false, Ordering::Relaxed);
    if let Ok(mut g) = TOGGLE_CLAIMS.lock() {
        g.clear();
    }
    if let Ok(mut g) = TOGGLES.lock() {
        g.clear();
    }
    if let Ok(mut held) = HELD.lock() {
        held.clear();
    }
    if let Ok(mut anim) = ANIM.lock() {
        anim.clear();
    }
    if let Ok(mut g) = ANIMATE.lock() {
        g.clear();
    }
    if let Ok(mut claims) = CLAIMS.lock() {
        claims.clear();
    }
    if let Ok(mut team) = TEAM_CLAIMS.lock() {
        team.clear();
    }
    if let Ok(mut last) = LAST_SLOT.lock() {
        last.clear();
    }
}

/// Fill missing slope variants from the first present link sheet, then eat
/// draw/undraw (`wthrow`*) with the matching link sheet when unset.
fn expand_aim_claims(poses: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let link = poses.iter().find(|(p, _)| *p == LINK).map(|(_, id)| *id);
    let link_u = poses.iter().find(|(p, _)| *p == LINK_U).map(|(_, id)| *id);
    let link_d = poses.iter().find(|(p, _)| *p == LINK_D).map(|(_, id)| *id);
    let any = link.or(link_u).or(link_d);
    let Some(fallback) = any else {
        return poses.to_vec();
    };
    let p = link.unwrap_or(fallback);
    let u = link_u.unwrap_or(fallback);
    let d = link_d.unwrap_or(fallback);
    let mut out: Vec<(i32, i32)> = poses
        .iter()
        .copied()
        .filter(|(pose, _)| !is_link(*pose) && !is_throw(*pose) && !is_fire(*pose))
        .collect();
    out.push((LINK, p));
    out.push((LINK_U, u));
    out.push((LINK_D, d));
    let throw = poses
        .iter()
        .find(|(pose, _)| *pose == THROW)
        .map(|(_, id)| *id);
    let throw_u = poses
        .iter()
        .find(|(pose, _)| *pose == THROW_U)
        .map(|(_, id)| *id);
    let throw_d = poses
        .iter()
        .find(|(pose, _)| *pose == THROW_D)
        .map(|(_, id)| *id);
    out.push((THROW, throw.unwrap_or(p)));
    out.push((THROW_U, throw_u.unwrap_or(u)));
    out.push((THROW_D, throw_d.unwrap_or(d)));
    // A gun firing: its own sheets, any one filling the others; without
    // them, the aim sheet, so a custom gun never flashes back to stock.
    let fire = poses.iter().find(|(pose, _)| *pose == FIRE).map(|(_, id)| *id);
    let fire_u = poses.iter().find(|(pose, _)| *pose == FIRE_U).map(|(_, id)| *id);
    let fire_d = poses.iter().find(|(pose, _)| *pose == FIRE_D).map(|(_, id)| *id);
    let any_fire = fire.or(fire_u).or(fire_d);
    out.push((FIRE, fire.or(any_fire).unwrap_or(p)));
    out.push((FIRE_U, fire_u.or(any_fire).unwrap_or(u)));
    out.push((FIRE_D, fire_d.or(any_fire).unwrap_or(d)));
    out
}

/// A claim's `worm_sprites_toggle` sheets, drawn in place of its
/// `worm_sprites` while the worm's toggle is on.
pub fn claim_toggle(slot: usize, mod_id: &str, poses: &[(i32, Option<u8>, String)]) {
    let mut list: Vec<(i32, i32)> = Vec::new();
    for (stock, _, path) in poses {
        if let Some(sheet) = sprite::lookup_key(&gfx::sprite_key(mod_id, path)) {
            log_line(&format!("worm sprite toggle slot={slot} stock={stock} custom={} path={path}", sheet.id));
            list.push((*stock, sheet.id));
        }
    }
    if let Ok(mut claims) = TOGGLE_CLAIMS.lock() {
        claims.retain(|(s, _, _)| *s != slot);
        for (stock, id) in expand_aim_claims(&list) {
            claims.push((slot, stock, id));
        }
    }
}

/// The worm's FrameFinish: a press of the synchronized + key while the
/// turn's worm has a toggle claim's slot selected flips its toggle.
pub unsafe fn toggle_frame(worm: *mut u8) {
    let slot = read_i32(worm, WORM_SELECTED_WEAPON) as usize;
    let claimed = TOGGLE_CLAIMS.lock().map(|g| g.iter().any(|(s, _, _)| *s == slot)).unwrap_or(false);
    let pressed = claimed
        && read_i32(worm, WORM_TURN_ACTIVE) != 0
        && crate::world::keys() & game::sync_input::PLUS_KEY != 0;
    let Ok(mut toggles) = TOGGLES.lock() else {
        return;
    };
    match toggles.iter_mut().find(|(w, _, _)| *w == worm as usize) {
        Some(entry) => {
            if pressed && !entry.2 {
                entry.1 = !entry.1;
            }
            entry.2 = pressed;
        }
        None if pressed => toggles.push((worm as usize, true, true)),
        None => {}
    }
}

/// The toggle claim's sheet for `pose` on `slot`, while the worm's toggle is on.
fn toggled_sheet(worm: usize, slot: usize, pose: i32) -> Option<i32> {
    let on = TOGGLES.lock().ok()?.iter().any(|(w, on, _)| *w == worm && *on);
    if !on {
        return None;
    }
    TOGGLE_CLAIMS.lock().ok()?.iter().find(|(s, p, _)| *s == slot && *p == pose).map(|(_, _, id)| *id)
}

pub fn claim(slot: usize, mod_id: &str, poses: &[(i32, Option<u8>, String)], animate: bool) {
    if let Ok(mut g) = ANIMATE.lock() {
        g.retain(|s| *s != slot);
        if animate {
            g.push(slot);
        }
    }
    // Sheet ids per team colour, `None` for the sheet every worm uses.
    let mut by_team: Vec<(Option<u8>, Vec<(i32, i32)>)> = Vec::new();
    for (stock, team, path) in poses {
        if let Some(sheet) = sprite::lookup_key(&gfx::sprite_key(mod_id, path)) {
            log_line(&format!(
                "worm sprite claim slot={slot} stock={stock} team={team:?} custom={} path={path}",
                sheet.id
            ));
            match by_team.iter_mut().find(|(t, _)| t == team) {
                Some((_, list)) => list.push((*stock, sheet.id)),
                None => by_team.push((*team, vec![(*stock, sheet.id)])),
            }
        }
    }
    by_team.sort_by_key(|(team, _)| *team);
    // A pack that only lists team sheets still needs a default: the lowest
    // team colour's sheet stands in for each pose nobody gave one.
    let mut defaults: Vec<(i32, i32)> = Vec::new();
    for (_, list) in &by_team {
        for &(stock, id) in list {
            if !defaults.iter().any(|(p, _)| *p == stock) {
                defaults.push((stock, id));
            }
        }
    }
    if let Ok(mut claims) = CLAIMS.lock() {
        claims.retain(|(s, _, _)| *s != slot);
        for (stock, id) in expand_aim_claims(&defaults) {
            claims.push((slot, stock, id));
        }
    }
    if let Ok(mut team_claims) = TEAM_CLAIMS.lock() {
        team_claims.retain(|(s, _, _, _)| *s != slot);
        for (team, list) in &by_team {
            let Some(team) = team else { continue };
            for (stock, id) in expand_aim_claims(list) {
                team_claims.push((slot, *team, stock, id));
            }
        }
    }
}

/// The drawing worm's coalition colour (0 red .. 5 cyan), as `worm.alliance`.
unsafe fn worm_alliance(worm: *mut u8) -> Option<u8> {
    game::team::worm_team_colour(worm)
}

/// This worm's colour of `sheet`, when the pack gave one for `pose`.
unsafe fn team_sheet(worm: *mut u8, slot: usize, pose: i32, sheet: i32) -> i32 {
    let Ok(team_claims) = TEAM_CLAIMS.lock() else {
        return sheet;
    };
    if !team_claims.iter().any(|(s, _, _, _)| *s == slot) {
        return sheet;
    }
    let Some(team) = worm_alliance(worm) else {
        return sheet;
    };
    team_claims
        .iter()
        .find(|(s, t, p, _)| *s == slot && *t == team && *p == pose)
        .map(|(_, _, _, id)| *id)
        .unwrap_or(sheet)
}

pub fn install() -> Result<(), String> {
    for (name, detour, original) in [
        ("WORM_CALC_SPRITE", calc_sprite_hook as *mut c_void, &ORIGINAL),
        ("DISPLAY_GET_SPRITE_INFO", sprite_info_hook as *mut c_void, &ORIGINAL_SPRITE_INFO),
        ("WEAPON_HOLD_SHEET", hold_sheet_hook as *mut c_void, &ORIGINAL_HOLD),
    ] {
        let trampoline = unsafe {
            crate::wa::create_hook(crate::wa::address(name) as *mut c_void, detour)?
        };
        original.store(trampoline, Ordering::SeqCst);
    }
    log_line("worm sprite wrap and deterministic empty-sprite metadata installed");
    Ok(())
}

/// SpriteAnimation__play_1 (0x4F5BD0) passes its two uninitialized stack locals
/// to GetSpriteInfo, ignores failure, then copies them into animator+0x14/0x18
/// and its queue at +0x30/+0x34. Sentry's empty stock equip pose uses ID 0.
/// Supply zero rate/flags and one frame. A nonzero divisor also covers an
/// empty sprite with high flag bits: advance checks the whole sprite word
/// before dividing by frame count. Keep stock failure and real-sprite lookup.
unsafe extern "thiscall" fn sprite_info_hook(
    display: *mut u8,
    sprite_id: u32,
    rate_out: *mut u32,
    flags_out: *mut u32,
    frames_out: *mut u32,
) -> u32 {
    if sprite_id == 0 {
        for out in [rate_out, flags_out] {
            if !out.is_null() {
                *out = 0;
            }
        }
        if !frames_out.is_null() {
            *frames_out = 1;
        }
    }
    let original: SpriteInfoFn = std::mem::transmute(ORIGINAL_SPRITE_INFO.load(Ordering::Relaxed));
    original(display, sprite_id, rate_out, flags_out, frames_out)
}

unsafe extern "C" fn after_calc(
    worm: *mut u8,
    sprite_out: *mut u32,
    _x_out: *const i32,
    _y_out: *const i32,
    phase_out: *mut u32,
) {
    if worm.is_null() || sprite_out.is_null() {
        return;
    }
    after_calc_inner(worm, sprite_out, phase_out);
}

unsafe fn after_calc_inner(worm: *mut u8, sprite_out: *mut u32, phase_out: *mut u32) {
    let selected = read_i32(worm, WORM_SELECTED_WEAPON) as usize;
    if selected == StockWeapon::SkipGo.index()
        && SKIP_GO_SURRENDER_POSE.load(Ordering::Relaxed)
        && read_i32(worm, WORM_TURN_ACTIVE) != 0
        && crate::world::keys() & game::sync_input::CTRL != 0
    {
        *sprite_out = surrender_pose(*sprite_out);
    }
    let original = *sprite_out;
    let state = read_i32(worm, WORM_STATE);
    let last = last_slot(worm as usize);
    let slot = pose_slot(
        selected,
        last.as_ref().map(|(s, sheets)| (*s, sheets.as_slice())),
        original,
        (0x67..=0x69).contains(&state),
    );
    // WA has moved on from the weapon it was putting away.
    if let Some((s, _)) = &last {
        if *s != slot {
            forget_slot(worm as usize);
        }
    }
    let Ok(claims) = CLAIMS.lock() else {
        return;
    };
    if !claims.iter().any(|(s, _, _)| *s == slot) || is_wa_throw(original, state) {
        return;
    }
    let stock = pose_key(original, state, read_i32(worm, WORM_SLOPE_DIR));
    if let Some((_, _, id)) = claims
        .iter()
        .find(|(s, claimed, _)| *s == slot && *claimed == stock)
    {
        remember_sheet(worm as usize, slot, original & 0xffff);
        // Pose and aim logic compare the default sheet; only the drawn one
        // follows the worm's team colour.
        let sheet = team_sheet(worm, slot, stock, *id);
        // A toggled worm draws the toggle claim's sheet: a new sheet, so a
        // one-shot pose plays again from its start (drive_phase), as PX
        // replays the Electromagnet's deploy in its new colour.
        let sheet = toggled_sheet(worm as usize, slot, stock).unwrap_or(sheet);
        *sprite_out = (original & !0xffff) | sheet as u32;
        // Two kinds of pack use these poses:
        //
        // - The web-editor aim wizard: `weaponlnk*` is an aim-angle sheet
        //   (stock phase = angle across 180°). It may ship its own `wthrow*`
        //   draw GIF, or none -- `expand_aim_claims` then falls the throw
        //   poses back to the link sheet and we hold a mid frame instead of
        //   playing the aim sheet as an animation. This is the default.
        // - `params.worm_sprites_animate = true` (sentry gun): every claimed
        //   pose is a one-shot animation driven frame by frame, including the
        //   link poses. A distinct draw sheet alone cannot be the signal --
        //   the wizard produces exactly that shape too.
        let animate = ANIMATE.lock().map(|g| g.contains(&slot)).unwrap_or(false);
        let aim_sheet = !animate
            && (is_link(stock) || is_fire(stock) || is_aim_sheet(&claims, slot, stock, *id));
        if aim_sheet {
            if is_throw(stock) {
                // Eat draw/undraw: hold a mid frame, no play-through.
                if !phase_out.is_null() {
                    *phase_out = 0x8000;
                }
            }
            // Link and fire: leave the stock phase, which is the aim angle.
        } else {
            // WA puts a weapon away by playing its draw sheet with the
            // reverse flag: the same pose and sheet, but a new animation.
            let reversed = original & game::offsets::SPRITE_REVERSE != 0;
            drive_phase(worm as usize, stock, sheet, reversed, phase_out);
        }
    } else if let Ok(mut anim) = ANIM.lock() {

        // On a claimed slot but not currently in a claimed pose -- walking
        // between equips, say. `drive_phase` only resets a worm's stored phase
        // when the pose or sheet differs from last time, so if the next equip
        // lands on the SAME pose key (same slope sign is common, the worm
        // rarely moves between shots) the stale entry is still sitting at its
        // old phase: the final held frame. The animation would then appear to
        // jump straight to its last frame instead of playing. Dropping the
        // entry the moment the worm leaves a claimed pose guarantees the next
        // one starts fresh.
        anim.retain(|(w, _, _, _, _, _)| *w != worm as usize);
    }
}

/// Game frames each animation frame is held for.
const HOLD: u32 = 2;

/// Advance one worm's claimed pose, once per game frame.
///
/// Called from the worm's FrameFinish. The sprite calc runs several times per
/// frame, so advancing in the draw hook made the sequence play several times
/// too fast and snap to its last frame almost at once.
pub fn tick(worm: usize) {
    let Ok(mut anim) = ANIM.lock() else {
        return;
    };
    let Some(entry) = anim.iter_mut().find(|(w, _, _, _, _, _)| *w == worm) else {
        return;
    };
    entry.5 = entry.5.saturating_add(1);
    let Some(frames) = sprite::frame_count(entry.3) else {
        return;
    };
    if frames < 2 {
        return;
    }
    let step = 0x1_0000u32 / (frames as u32 * HOLD);
    let last = 0x1_0000u32 - (0x1_0000u32 / frames as u32) / 2;
    entry.2 = (entry.2 + step).min(last);
}

/// The phase of a claimed pose, counted by `tick` from 0 up to its last
/// frame. A reversed pose counts up too; the renderer plays it backwards
/// (`sprite::draw_phase`).
unsafe fn drive_phase(worm: usize, pose: i32, sprite: i32, reversed: bool, phase_out: *mut u32) {
    if phase_out.is_null() {
        return;
    }
    let Ok(mut anim) = ANIM.lock() else {
        return;
    };
    let phase = match anim.iter_mut().find(|(w, _, _, _, _, _)| *w == worm) {
        Some(entry) => {
            if entry.1 != pose || entry.3 != sprite || entry.4 != reversed || entry.5 > IDLE_RESTART {
                entry.1 = pose;
                entry.3 = sprite;
                entry.4 = reversed;
                entry.2 = 0;
            }
            entry.5 = 0;
            entry.2
        }
        None => {
            anim.push((worm, pose, 0, sprite, reversed, 0));
            0
        }
    };
    unsafe {
        *phase_out = phase;
    }
}

#[cfg(test)]
mod pose_slot_tests {
    use super::pose_slot;

    #[test]
    fn a_weapon_being_put_away_keeps_its_art() {
        // As seen in WA: the Sniper Rifle (slot 12) replaced 560 and 502;
        // after its last shot WA selects the bazooka (1) and still draws
        // them, then the bazooka's own 438.
        let handgun: &[u32] = &[560, 563, 502];
        assert_eq!(pose_slot(1, Some((12, handgun)), 560, false), 12, "still putting the rifle away");
        assert_eq!(pose_slot(1, Some((12, handgun)), 502, false), 12);
        assert_eq!(pose_slot(1, Some((12, handgun)), 438, false), 1, "the bazooka's own sheet");
        assert_eq!(pose_slot(12, Some((12, handgun)), 560, false), 12, "the selection's own art");
        assert_eq!(pose_slot(1, None, 560, false), 1, "nothing replaced before");
    }

    #[test]
    fn a_weapon_put_away_on_another_slope_keeps_its_art() {
        use game::offsets::SPRITE_REVERSE;
        // As seen in WA: the rifle was drawn with 500 and put away with 502
        // reversed (0x008001F6), which it had never replaced.
        let rifle: &[u32] = &[500, 560, 563];
        assert_eq!(pose_slot(1, Some((12, rifle)), SPRITE_REVERSE | 502, false), 12);
        assert_eq!(pose_slot(1, Some((12, rifle)), 502, false), 1, "not reversed: not a put-away");
        assert_eq!(pose_slot(1, None, SPRITE_REVERSE | 502, false), 1, "nothing to put away");
    }

    #[test]
    fn the_next_weapon_aimed_is_its_own_even_on_shared_sheets() {
        // The Igniter (homing missile, slot 2) is drawn out on wbazlnk (436)
        // and aimed on wbaz2 (531). After its shot and put-away, WA draws
        // the bazooka (1) out on wbazlnk too, aiming (state 0x67).
        let igniter: &[u32] = &[436, 531];
        assert_eq!(pose_slot(1, Some((2, igniter)), 436, false), 2, "still putting it away");
        assert_eq!(pose_slot(1, Some((2, igniter)), 436, true), 1, "the bazooka, drawn out");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        expand_aim_claims, pose_key, FIRE, FIRE_D, FIRE_U, LINK, LINK_D, LINK_U, THROW, THROW_D,
        THROW_U,
    };

    #[test]
    fn gun_aim_draw_and_fire_map_to_link_throw_and_fire() {
        assert_eq!(pose_key(570, 0x67, 0), LINK); // wmini
        assert_eq!(pose_key(565, 0x67, 0), LINK_U); // wuziu
        assert_eq!(pose_key(505, 0x6a, 0), THROW_D); // wmgnlnkd
        assert_eq!(pose_key(388, 0x6a, 0), 388, "wminlnk is the mine's draw, not a gun's");
        assert_eq!(pose_key(573, 0x67, 0), FIRE); // wminif
        assert_eq!(pose_key(550, 0x67, 0), FIRE_U); // wshotfu
        assert_eq!(pose_key(563, 0x67, 0), FIRE_D); // whandfd
    }

    #[test]
    fn fire_falls_back_to_its_own_sheets_then_the_aim_sheet() {
        let out = expand_aim_claims(&[(LINK, 1), (LINK_U, 2), (LINK_D, 3)]);
        assert!(out.contains(&(FIRE, 1)) && out.contains(&(FIRE_U, 2)) && out.contains(&(FIRE_D, 3)));
        let out = expand_aim_claims(&[(LINK, 1), (FIRE, 7)]);
        assert!(out.contains(&(FIRE, 7)) && out.contains(&(FIRE_U, 7)) && out.contains(&(FIRE_D, 7)));
    }

    #[test]
    fn a_placed_weapon_keeps_the_held_sheet_of_its_stock_sprite() {
        assert!(super::hold_as(23, 153));
        assert_eq!(super::HELD.lock().unwrap().iter().find(|(s, _)| *s == 23).map(|(_, id)| *id), Some(343));
        assert!(!super::hold_as(23, 900), "no held sheet for an unknown sprite");
    }

    #[test]
    fn putting_a_weapon_away_plays_the_draw_again() {
        use super::{drive_phase, ANIM};
        let (worm, sheet) = (0x5EED_0001usize, 0x0100_0061);
        let phase = |reversed| {
            let mut out = 0u32;
            unsafe { drive_phase(worm, THROW, sheet, reversed, &mut out) };
            out
        };
        assert_eq!(phase(false), 0);
        // The draw has played through, as `tick` leaves it.
        for entry in ANIM.lock().unwrap().iter_mut().filter(|e| e.0 == worm) {
            entry.2 = 0xF334;
        }
        assert_eq!(phase(false), 0xF334, "the same draw holds its last frame");
        // WA puts it away: the same pose and sheet, reversed.
        assert_eq!(phase(true), 0, "the put-away starts over");
        ANIM.lock().unwrap().retain(|e| e.0 != worm);
    }

    #[test]
    fn a_draw_after_a_gap_starts_over() {
        use super::{drive_phase, tick, ANIM, IDLE_RESTART};
        // The Sniper Rifle drawn out, aimed a while, then the Igniter drawn
        // out: both on PX's posture draw sheet, with no put-away between.
        let (worm, sheet) = (0x5EED_0002usize, 0x0100_0062);
        let phase = || {
            let mut out = 0u32;
            unsafe { drive_phase(worm, THROW, sheet, false, &mut out) };
            out
        };
        assert_eq!(phase(), 0);
        for entry in ANIM.lock().unwrap().iter_mut().filter(|e| e.0 == worm) {
            entry.2 = 0xF334;
        }
        tick(worm);
        assert_eq!(phase(), 0xF334, "drawn every frame: it goes on");
        for _ in 0..=IDLE_RESTART {
            tick(worm);
        }
        assert_eq!(phase(), 0, "undrawn for a while: it starts over");
        ANIM.lock().unwrap().retain(|e| e.0 != worm);
    }

    #[test]
    fn custom_link_uses_state_and_slope_but_throw_keeps_stock_id() {
        assert_eq!(pose_key(0x40000, 0x67, 0), LINK);
        assert_eq!(pose_key(0x40000, 0x68, -1), LINK_U);
        assert_eq!(pose_key(0x40000, 0x69, 1), LINK_D);
        assert_eq!(pose_key(0x4024f, 0x6a, 0), THROW);
        assert_eq!(pose_key(0x40000, 0x6a, 0), 0);
    }

    #[test]
    fn a_throw_is_wa_s_own_not_a_draw() {
        use super::is_wa_throw;
        use game::offsets::SPRITE_REVERSE;
        // As seen in WA: the Sticky Bomb thrown on wthrowd (593), firing
        // (0x6A, 0x6B) and standing just after.
        assert!(is_wa_throw(0x0004_0251, 0x6a));
        assert!(is_wa_throw(0x0004_0251, 0x65));
        assert!(!is_wa_throw(0x0004_0251, 0x67), "aiming: a draw on wthrow is still a draw");
        assert!(!is_wa_throw(SPRITE_REVERSE | 0x0251, 0x65), "reversed: a put-away");
        assert!(!is_wa_throw(0x013a, 0x6a), "wgrnlnk is no throw");
    }

    #[test]
    fn bazooka_and_homing_barrels_map_to_link() {
        assert_eq!(pose_key(525, 0x67, 0), LINK);
        assert_eq!(pose_key(526, 0x67, 0), LINK_U);
        assert_eq!(pose_key(527, 0x67, 0), LINK_D);
        assert_eq!(pose_key(531, 0x67, 0), LINK); // wbaz2 — HM family
        assert_eq!(pose_key(532, 0x67, 0), LINK_U);
        assert_eq!(pose_key(535, 0x67, 0), LINK_U); // wbaz3u
        assert_eq!(pose_key(536, 0x67, 0), LINK_D); // wbaz3d
    }

    #[test]
    fn fuse_and_cluster_holds_map_to_link() {
        assert_eq!(pose_key(576, 0x67, 0), LINK); // wthrgrn
        assert_eq!(pose_key(579, 0x67, 0), LINK); // wthrcls
        assert_eq!(pose_key(436, 0x6a, 0), THROW); // wbazlnk draw
        assert_eq!(pose_key(313, 0x6a, 0), THROW); // wgrnlnk
    }

    #[test]
    fn one_link_sheet_fills_slopes_and_eats_throw() {
        let out = expand_aim_claims(&[(LINK, 900)]);
        assert!(out.contains(&(LINK, 900)));
        assert!(out.contains(&(LINK_U, 900)));
        assert!(out.contains(&(LINK_D, 900)));
        assert!(out.contains(&(THROW, 900)));
        assert!(out.contains(&(THROW_U, 900)));
        assert!(out.contains(&(THROW_D, 900)));
    }

    #[test]
    fn three_link_sheets_keep_slopes_and_eat_throw() {
        let out = expand_aim_claims(&[(LINK, 1), (LINK_U, 2), (LINK_D, 3)]);
        assert!(out.contains(&(LINK, 1)));
        assert!(out.contains(&(LINK_U, 2)));
        assert!(out.contains(&(LINK_D, 3)));
        assert!(out.contains(&(THROW, 1)));
        assert!(out.contains(&(THROW_U, 2)));
        assert!(out.contains(&(THROW_D, 3)));
    }
}

/// WA 3.8.1 usercall: ESI=worm, EAX=sprite_out, stack=x_out,y_out,phase_out.
/// Save the output pointer across the original, then remap only its result.
#[unsafe(naked)]
unsafe extern "C" fn calc_sprite_hook() {
    core::arch::naked_asm!(
        "push ebp",
        "mov ebp, esp",
        "push eax",
        "push esi",
        "push dword ptr [ebp + 16]",
        "push dword ptr [ebp + 12]",
        "push dword ptr [ebp + 8]",
        "call dword ptr [{original}]",
        "pushad",
        "push dword ptr [ebp + 16]",
        "push dword ptr [ebp + 12]",
        "push dword ptr [ebp + 8]",
        "push dword ptr [ebp - 4]",
        "push dword ptr [ebp - 8]",
        "call {after}",
        "add esp, 20",
        "popad",
        "mov esp, ebp",
        "pop ebp",
        "ret 12",
        original = sym ORIGINAL,
        after = sym after_calc,
    );
}
