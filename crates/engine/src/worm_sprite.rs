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

use crate::{log_line, rebase::rb, sprite};
use game::{
    entity::read_i32,
    offsets::{
        ENT_WORLD, ENT_WORM_TEAM, WORLD_TEAM_ALLIANCE, WORLD_TEAM_STRIDE, WORM_SELECTED_WEAPON,
        WORM_SLOPE_DIR, WORM_STATE,
    },
};
use minhook::MinHook;
use re_catalog::Catalog;
use std::{
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicPtr, Ordering},
    },
};

static ORIGINAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_SPRITE_INFO: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_HOLD: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

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
/// Per-team-colour sheets: (slot, alliance, pose, sheet). `CLAIMS` still
/// holds each pose's default, so pose and phase logic never looks at team;
/// only the final sheet id is swapped for the drawing worm's colour.
static TEAM_CLAIMS: Mutex<Vec<(usize, u8, i32, i32)>> = Mutex::new(Vec::new());
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
    let throw = poses.iter().find(|(pose, _)| *pose == THROW).map(|(_, id)| *id);
    let throw_u = poses.iter().find(|(pose, _)| *pose == THROW_U).map(|(_, id)| *id);
    let throw_d = poses.iter().find(|(pose, _)| *pose == THROW_D).map(|(_, id)| *id);
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

/// The drawing worm's team colour (0 red .. 5 cyan), as `worm.alliance`.
unsafe fn worm_alliance(worm: *mut u8) -> Option<u8> {
    let world = read_i32(worm, ENT_WORLD) as *mut u8;
    let team = read_i32(worm, ENT_WORM_TEAM);
    if world.is_null() || team < 0 {
        return None;
    }
    u8::try_from(read_i32(world, WORLD_TEAM_ALLIANCE + team as usize * WORLD_TEAM_STRIDE)).ok()
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

pub fn install(catalog: &Catalog) -> Result<(), String> {
    let va = catalog
        .function_va("WormEntity__calc_sprite")
        .ok_or("missing WormEntity__calc_sprite catalog entry")?;
    let original = unsafe {
        MinHook::create_hook(rb(va) as *mut c_void, calc_sprite_hook as *mut c_void)
            .map_err(|e| format!("worm sprite hook: {e:?}"))?
    };
    ORIGINAL.store(original, Ordering::SeqCst);
    let info_va = catalog
        .function_va("DisplayGfx__GetSpriteInfo")
        .ok_or("missing DisplayGfx__GetSpriteInfo catalog entry")?;
    let info = rb(info_va) as *mut c_void;
    // 3.8.1: MOV EAX,[ESP+4]; LEA EDX,[EAX-1]. The unsigned range
    // check immediately following rejects ID 0 without touching outputs.
    let expected = [0x8b, 0x44, 0x24, 0x04, 0x8d, 0x50, 0xff];
    unsafe {
        if std::slice::from_raw_parts(info as *const u8, expected.len()) != expected {
            return Err(format!("sprite info: WA bytes differ at {info_va:#010x}"));
        }
        let trampoline = MinHook::create_hook(info, sprite_info_hook as *mut c_void)
            .map_err(|e| format!("sprite info hook: {e:?}"))?;
        ORIGINAL_SPRITE_INFO.store(trampoline, Ordering::SeqCst);
    }
    let hold_va = catalog
        .function_va("Weapon__hold_sheet")
        .ok_or("missing Weapon__hold_sheet catalog entry")?;
    let hold = unsafe {
        MinHook::create_hook(rb(hold_va) as *mut c_void, hold_sheet_hook as *mut c_void)
            .map_err(|e| format!("hold sheet hook: {e:?}"))?
    };
    ORIGINAL_HOLD.store(hold, Ordering::SeqCst);
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
    let slot = read_i32(worm, WORM_SELECTED_WEAPON) as usize;
    let original = *sprite_out;
    let state = read_i32(worm, WORM_STATE);
    let Ok(claims) = CLAIMS.lock() else {
        return;
    };
    if !claims.iter().any(|(s, _, _)| *s == slot) {
        return;
    }
    let stock = pose_key(original, state, read_i32(worm, WORM_SLOPE_DIR));
    if let Some((_, _, id)) = claims
        .iter()
        .find(|(s, claimed, _)| *s == slot && *claimed == stock)
    {
        // Pose and aim logic compare the default sheet; only the drawn one
        // follows the worm's team colour.
        let sheet = team_sheet(worm, slot, stock, *id);
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
