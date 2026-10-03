//! Game-thread wraps: projectile gravity leaf, missile frames, worm lockstep messages.
//!
//! Scripted stock missiles: Lua is the brain, stock MissileEntity is the body.
//!
//! `CollisionManager__apply_projectile_gravity` (0x004FCB30, thiscall RET 0x4):
//! `speed_y += particle_gravity_y * entity+0x60`, then
//! `speed_x += wind_bias * entity+0x5C` if a 1px wind probe is clear, then
//! both components × entity+0x58 (damp). Bounce/land is *not* this function —
//! that is `CollisionManager__rope` from `update_0`. Lua therefore:
//! - `gravity_before` writes the three scales so stock wind/gravity see them
//! - `gravity_after` edits velocity (extra bounce, stickiness, spin kicks)

use crate::log_line;
use game::entity::{is_missile, read_cm_forces, read_i32, read_motion, write_i32, write_motion, write_scales};
use game::offsets::{
     CUSTOM_SPRITE_SENTINEL, CUSTOM_SPRITE_SLOT_MIN, ENT_MOVED, ENT_POS_X,
    ENT_POS_Y, ENT_SPEED_X, ENT_SPEED_Y, ENT_WORM_TEAM, HIT_AMOUNT, HIT_TEAM, MISSILE_ALT_BANK_SPRITE,
      MISSILE_HOMING_MODE, MISSILE_HOMING_SPRITE_A, MISSILE_HOMING_SPRITE_B,
    MISSILE_RENDER_SPRITE, MISSILE_SHOOT_PELLET, MISSILE_RENDER_SPRITE_ALT, MISSILE_TYPE, MISSILE_TYPE_HOMING,
     MISSILE_WEAPON_ALT_SPRITE,
    MISSILE_WEAPON_SPRITE, MSG_COMMIT_HEALTH, MSG_FINISH_TURN, MSG_FIRE_WEAPON, MSG_FRAME_FINISH, MSG_HIT,
    MSG_RENDER_SCENE, MSG_RETREAT_FINISHED, MSG_RETREAT_STARTED, MSG_START_TURN,


    WORM_SHOW_CURSOR, WORM_FACING,   WORM_STATE,
    WORM_STATE_AIRBORNE, WORM_TURN_ACTIVE, WORLD_ROOT_TURN_ENDED,
    COLLISION_LANDSCAPE_BIT, ENT_COLLISION_MASK, MSG_JUMP, MSG_JUMP_UP, WORM_KEY_DOWN, WORM_KEY_LEFT,
    WORM_KEY_RIGHT, WORM_KEY_UP, WORM_STATE_JETPACKING, WORM_STATE_TUMBLING, WORM_STATE_HIT,
    WORM_STATE_BLASTED, WORM_SELECTED_WEAPON,
    WORLD_ROOT_TURN_TIMER, ENT_WORLD, ENT_HELPER, GAME_WORLD_COLLISION_MANAGER,
};
use std::ffi::c_void;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::sync::atomic::{AtomicPtr, Ordering};
use weapons::StockWeapon;

type ApplyGravityFn = unsafe extern "thiscall" fn(*mut u8, *mut u8);
type HandleMessageFn = unsafe extern "thiscall" fn(*mut u8, u32, u32, u32, *mut u8);

static ORIG_GRAVITY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_MISSILE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static WORM_MSG_ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ORIG_WORM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static TURN_PHASE: Mutex<TurnPhase> = Mutex::new(TurnPhase { worm: 0, completed: 0 });

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnEvent {
    Retreat,
    End,
    Cleanup,
}

/// The stock retreat timer can be zero. Fill any skipped phase when WA sends
/// the next lifecycle message, so packs still see all three events in order.
struct TurnPhase {
    worm: u32,
    completed: u8,
}

/// Between WA's retreat-started (0x3C) and retreat-finished (0x3D) messages
/// to the turn's worm.
fn retreating() -> bool {
    TURN_PHASE.lock().map(|p| p.worm != 0 && p.completed == 1).unwrap_or(false)
}

impl TurnPhase {
    fn start(&mut self, worm: u32) {
        self.worm = worm;
        self.completed = 0;
    }

    fn advance(&mut self, worm: u32, msg: u32) -> Vec<TurnEvent> {
        let target = match msg {
            MSG_RETREAT_STARTED => 1,
            MSG_RETREAT_FINISHED => 2,
            MSG_FINISH_TURN => 3,
            _ => return Vec::new(),
        };
        if self.worm != worm {
            self.start(worm);
        }
        let mut events = Vec::new();
        while self.completed < target {
            self.completed += 1;
            events.push(match self.completed {
                1 => TurnEvent::Retreat,
                2 => TurnEvent::End,
                _ => TurnEvent::Cleanup,
            });
        }
        events
    }
}

#[cfg(test)]
mod turn_phase_tests {
    use super::*;

    #[test]
    fn normal_and_zero_retreat_emit_each_boundary_once() {
        let mut phase = TurnPhase { worm: 0, completed: 0 };
        phase.start(1);
        assert_eq!(phase.advance(1, MSG_RETREAT_STARTED), [TurnEvent::Retreat]);
        assert!(phase.advance(1, MSG_RETREAT_STARTED).is_empty());
        assert_eq!(phase.advance(1, MSG_RETREAT_FINISHED), [TurnEvent::End]);
        assert_eq!(phase.advance(1, MSG_FINISH_TURN), [TurnEvent::Cleanup]);
        assert!(phase.advance(1, MSG_FINISH_TURN).is_empty());

        // A zero retreat can skip both retreat messages entirely.
        phase.start(2);
        assert_eq!(
            phase.advance(2, MSG_FINISH_TURN),
            [TurnEvent::Retreat, TurnEvent::End, TurnEvent::Cleanup]
        );

        // If only the start message arrives, finish fills the remaining phases.
        phase.start(3);
        assert_eq!(phase.advance(3, MSG_RETREAT_STARTED), [TurnEvent::Retreat]);
        assert_eq!(
            phase.advance(3, MSG_FINISH_TURN),
            [TurnEvent::End, TurnEvent::Cleanup]
        );
    }
}
static ORIG_ROOT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_DRAW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_NAME: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_HUD: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// Missile ptr → body sprite that should survive the lock-mode bank switch.
///
/// Keyed by address, so an entry must not outlive its missile: WA reuses a
/// freed missile's memory for the next one, which would inherit the dead
/// missile's body. `missile_free_hook` forgets each missile as WA frees it,
/// and `clear_homing` empties both tables at every match start in case a
/// teardown ever skips the per-entity free.
static HM_CUSTOM: Mutex<Option<HashMap<usize, i32>>> = Mutex::new(None);
/// Logged (entity, mode) pairs so we see ballistic and lock once each.
static HM_LOGGED: Mutex<Option<HashSet<(usize, i32)>>> = Mutex::new(None);
static ORIG_MISSILE_FREE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

type MissileFreeFn = unsafe extern "thiscall" fn(*mut u8, u32) -> *mut u8;

/// Drop everything remembered about homing missiles. Called at match start.
pub fn clear_homing() {
    crate::missile_boom::clear_weapons();
    if let Ok(mut g) = BODY_HIDDEN.lock() {
        g.clear();
    }
    crate::contact::clear();
    if let Ok(mut g) = HM_CUSTOM.lock() {
        *g = None;
    }
    if let Ok(mut g) = HM_LOGGED.lock() {
        *g = None;
    }
}

fn forget_missile(key: usize) {
    crate::missile_boom::forget_weapon(key);
    if let Ok(mut g) = HM_CUSTOM.lock() {
        if let Some(m) = g.as_mut() {
            m.remove(&key);
        }
    }
    if let Ok(mut g) = HM_LOGGED.lock() {
        if let Some(s) = g.as_mut() {
            s.remove(&(key, 0));
            s.remove(&(key, 1));
        }
    }
}

/// The missiles whose messages WA is handling, innermost last, each with
/// whether WA freed it meanwhile. A missile that explodes in its frame is
/// freed inside its own handler. Its memory is then the heap's, and what
/// sits there differs between machines, so nothing after the handler may
/// read it: the frame's post report once gave the dead missile a fresh id.
static HANDLING: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());

fn begin_handling(missile: usize) {
    if let Ok(mut g) = HANDLING.lock() {
        g.push((missile, false));
    }
}

fn mark_freed(missile: usize) {
    if let Ok(mut g) = HANDLING.lock() {
        for entry in g.iter_mut().filter(|(m, _)| *m == missile) {
            entry.1 = true;
        }
    }
}

/// Whether WA freed the missile while handling its message.
fn end_handling(missile: usize) -> bool {
    let Ok(mut g) = HANDLING.lock() else {
        return false;
    };
    match g.iter().rposition(|(m, _)| *m == missile) {
        Some(i) => g.remove(i).1,
        None => false,
    }
}

/// `MissileEntity__Free`: forget the missile before WA releases its memory,
/// so the next missile allocated there starts clean.
unsafe extern "thiscall" fn missile_free_hook(this: *mut u8, flags: u32) -> *mut u8 {
    mark_freed(this as usize);
    forget_missile(this as usize);
    let orig = ORIG_MISSILE_FREE.load(Ordering::SeqCst);
    if orig.is_null() {
        return this;
    }
    let f: MissileFreeFn = unsafe { std::mem::transmute(orig) };
    unsafe { f(this, flags) }
}
static ORIG_HEALTH: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_CURSOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_OFFMAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_TURN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_ARROW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_AIM_CURSOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIG_CROSSHAIR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// A worm a pack holds (`worm:carry`, `worm:take`): pinned where the pack
/// puts it each frame, after WA's own handler.
struct Carry {
    handle: u32,
    x: i32,
    y: i32,
    facing: i32,
    /// The turn's timer is held while it is held (`carry`, `take{turn =
    /// "freeze"}`); a vehicle inside the turn (`take{turn = "run"}`) lets it run.
    freeze: bool,
    /// Taken with `worm:take`: the engine consumes its move keys and jump
    /// messages, and its water bit is off until it is released.
    taken: bool,
    /// The collision mask it had when taken, restored on release.
    mask: Option<i32>,
    /// Its WA state at the last pin, so a taken worm WA knocks flying (into
    /// a hit state) is let go rather than held.
    state: u32,
}

/// WA's states for a worm knocked out of control: a hit (0x4B) and a
/// blast's knockback (0x1C), both traced.
fn knocked(state: u32) -> bool {
    matches!(state, WORM_STATE_TUMBLING | WORM_STATE_HIT | WORM_STATE_BLASTED)
}

static CARRY: Mutex<Vec<Carry>> = Mutex::new(Vec::new());
/// A hard vehicle landing may enter 0x7E with its parachute still selected.
/// WA otherwise fires that parachute on the very next frame, before the worm
/// can hit land at its original speed. Only the selected slot is masked while
/// WA handles that worm's frame; the real selection is restored afterwards.
static SUPPRESS_AUTO_PARACHUTE: LazyLock<Mutex<HashSet<u32>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static HIDDEN: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// Worms whose body alone is hidden (`worm.body_visible = false`): only the
/// body sprite is skipped, and WA still draws their labels, fuel and cursor,
/// as Project X's RenderWorm override does.
static BODY_HIDDEN: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn holding() -> bool {
    CARRY
        .lock()
        .ok()
        .map(|g| !g.is_empty())
        .unwrap_or(false)
}

/// Some held worm holds the turn's timer.
fn freezing() -> bool {
    CARRY
        .lock()
        .ok()
        .map(|g| g.iter().any(|c| c.freeze))
        .unwrap_or(false)
}

/// Taken with `worm:take`: its move keys and jumps are the pack's.
pub fn taken(handle: u32) -> bool {
    CARRY
        .lock()
        .ok()
        .map(|g| g.iter().any(|c| c.handle == handle && c.taken))
        .unwrap_or(false)
}

pub fn drop_all() {
    let handles = CARRY
        .lock()
        .ok()
        .map(|g| g.iter().map(|c| c.handle).collect::<Vec<_>>())
        .unwrap_or_default();
    for h in handles {
        drop_carry(h);
    }
}

type SetStateFn = unsafe extern "thiscall" fn(*mut u8, u32);

/// A new match: forget every hold and steer from the last one. Its worms are
/// gone, and a new worm may reuse an old one's address.
pub fn clear_handoffs() {
    if let Ok(mut g) = CARRY.lock() {
        g.clear();
    }
    if let Ok(mut g) = STEERED.lock() {
        g.clear();
    }
    if let Ok(mut g) = SUPPRESS_AUTO_PARACHUTE.lock() {
        g.clear();
    }
}

pub fn carry(handle: u32, x: i32, y: i32) {
    if handle == 0 {
        return;
    }
    if let Ok(mut g) = CARRY.lock() {
        if let Some(slot) = g.iter_mut().find(|c| c.handle == handle) {
            slot.x = x;
            slot.y = y;
            // The pack may have turned it (worm.facing) since the last pin.
            slot.facing = unsafe { read_i32(handle as *mut u8, WORM_FACING) };
        } else {
            let facing = unsafe { read_i32(handle as *mut u8, WORM_FACING) };
            g.push(Carry {
                handle,
                x,
                y,
                facing,
                freeze: true,
                taken: false,
                mask: None,
                state: 0,
            });
            crate::world::freeze_turn();
        }
    }
    crate::world::hold_turn_timer();
}

/// `worm:take{ hide, turn }`: the pack owns the worm until it releases it.
/// Built on the carry: the worm stays where it is until the pack moves it
/// (`worm:carry(x, y)`), with no speed of its own. Its move keys and jump
/// messages are consumed after the pack's `worm_message` hooks have read
/// them, so WA never walks or jumps it. Its landscape/water bit is off, so a
/// vehicle may hold it in water; release puts the bit back, and a worm
/// released in water drowns by WA's own path (tumbling, 0x7F -> 0x76 ->
/// 0x85 -> 0x87, seen in traces). The turn's end releases every held worm.
pub fn take(handle: u32, hide: bool, freeze: bool) {
    if handle == 0 {
        return;
    }
    let this = handle as *mut u8;
    let mut first_freeze = false;
    if let Ok(mut g) = CARRY.lock() {
        let mask = unsafe { read_i32(this, ENT_COLLISION_MASK) };
        let slot = match g.iter_mut().position(|c| c.handle == handle) {
            Some(i) => &mut g[i],
            None => {
                let (x, y, facing) = unsafe {
                    (read_i32(this, ENT_POS_X), read_i32(this, ENT_POS_Y), read_i32(this, WORM_FACING))
                };
                let state = unsafe { read_i32(this, WORM_STATE) } as u32;
                g.push(Carry { handle, x, y, facing, freeze: false, taken: false, mask: None, state });
                g.last_mut().unwrap()
            }
        };
        if slot.mask.is_none() {
            slot.mask = Some(mask);
            unsafe {
                write_i32(this, ENT_COLLISION_MASK, mask & !(COLLISION_LANDSCAPE_BIT as i32));
            }
        }
        slot.taken = true;
        if freeze && !slot.freeze {
            slot.freeze = true;
            first_freeze = true;
        }
    }
    if first_freeze {
        crate::world::freeze_turn();
    }
    if hide {
        hide_worm(handle, true);
    }
}

/// `worm:release(vx, vy, { tumble, suppress_auto_parachute })`: give the worm
/// back to WA moving at (vx, vy). A held worm is dropped (shown again, its mask back, the turn
/// thawed); any worm then enters falling-under-control (0x7E) through WA's
/// own SetState, as a closed parachute does, and WA flies and lands it. The
/// Jump, a vehicle's exit and worm_toss's throw are this. With `tumble` it
/// enters WA's out-of-control state (0x7F) instead, as a hit does: on land it
/// gets up (0x76), in water it drowns (0x85, 0x87; both traced after hits).
/// `suppress_auto_parachute` keeps the selected parachute from reopening
/// during that worm's airborne frames, so WA can handle a hard impact.
pub fn release(
    handle: u32,
    vx: i32,
    vy: i32,
    tumble: bool,
    suppress_auto_parachute: bool,
) {
    if handle == 0 {
        return;
    }
    if let Ok(mut g) = SUPPRESS_AUTO_PARACHUTE.lock() {
        if suppress_auto_parachute {
            g.insert(handle);
        } else {
            g.remove(&handle);
        }
    }
    let held = CARRY
        .lock()
        .ok()
        .map(|g| g.iter().any(|c| c.handle == handle))
        .unwrap_or(false);
    if held {
        // Let go; into 0x7E here unless it tumbles (one SetState either way).
        let_go(handle, (!tumble).then_some("release"));
    }
    if tumble || !held {
        let state = if tumble { WORM_STATE_TUMBLING } else { WORM_STATE_AIRBORNE };
        unsafe {
            write_i32(handle as *mut u8, ENT_MOVED, 1);
            let f: SetStateFn = std::mem::transmute(crate::wa::address("WORM_SET_STATE") as usize);
            crate::state_trace::with_cause("release", || f(handle as *mut u8, state));
        }
    }
    unsafe {
        write_i32(handle as *mut u8, ENT_SPEED_X, vx);
        write_i32(handle as *mut u8, ENT_SPEED_Y, vy);
    }
    if suppress_auto_parachute {
        let slot = unsafe { read_i32(handle as *mut u8, WORM_SELECTED_WEAPON) };
        log_line(&format!(
            "auto parachute suppression armed worm={handle:#x} slot={slot} vy={vy}"
        ));
    }
}

/// Worms whose jet pack a pack steers (`worm:jetpack_steer(true)`), and
/// whether their jet pack has run yet (it may be armed before it starts).
static STEERED: Mutex<Vec<(u32, bool)>> = Mutex::new(Vec::new());

type CollidesAtFn = unsafe extern "stdcall" fn(*mut u8, *mut u8, i32, i32, i32, i32) -> i32;

/// `worm:fits(x, y)`: WA's own test (0x4FB9D0, as its landing check
/// 0x5018F0 calls it) of the worm's collision helper at (x, y) against what
/// its mask collides with: true when it touches nothing. A held worm is
/// tested with the mask it had when taken (taking it clears the landscape
/// bit), so a pack can keep it clear of land by WA's own measure.
pub fn fits(handle: u32, x: i32, y: i32) -> bool {
    if handle == 0 {
        return true;
    }
    let this = handle as *mut u8;
    let saved = CARRY
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|c| c.handle == handle).and_then(|c| c.mask));
    unsafe {
        let world = read_i32(this, ENT_WORLD) as *mut u8;
        if world.is_null() {
            return true;
        }
        let cm = read_i32(world, GAME_WORLD_COLLISION_MANAGER) as *mut u8;
        let Some(addr) = crate::wa::try_address("ENTITY_COLLIDES_AT") else {
            return true;
        };
        if cm.is_null() {
            return true;
        }
        let mask = saved.unwrap_or_else(|| read_i32(this, ENT_COLLISION_MASK));
        let f: CollidesAtFn = std::mem::transmute(addr as usize);
        f(cm, this, x, y, read_i32(this, ENT_HELPER), mask) == 0
    }
}

/// `worm:jetpack_steer(on)`: while WA's jet pack runs (0x78), the engine
/// consumes the worm's move keys after the pack's hooks read them, so WA
/// neither thrusts nor burns fuel and the pack flies the worm with its
/// speed and fuel. WA still owns the jet pack: its collisions, its fuel HUD
/// and its end, when it lands the worm. It lapses when the jet pack ends.
pub fn jetpack_steer(handle: u32, on: bool) {
    if handle == 0 {
        return;
    }
    if let Ok(mut g) = STEERED.lock() {
        g.retain(|(h, _)| *h != handle);
        if on {
            g.push((handle, false));
        }
    }
}

/// Before WA handles a worm's message, after the pack's hooks: consume what
/// a taken or jet-pack-steered worm's pack owns. Returns true to eat the
/// message (a taken worm's jump).
fn owned_input(this: *mut u8, msg: u32) -> bool {
    if this.is_null() {
        return false;
    }
    let handle = this as u32;
    // WA latches each move key as its message arrives; clear them once, at
    // the frame's end after the pack's hooks read them, so keys that came in
    // separate messages (up and left) are all seen.
    let frame = msg == MSG_FRAME_FINISH;
    let jetpacking = unsafe { read_i32(this, WORM_STATE) } as u32 == WORM_STATE_JETPACKING;
    let mut lapsed = false;
    if let Ok(mut g) = STEERED.lock() {
        if let Some(entry) = g.iter_mut().find(|(h, _)| *h == handle) {
            if jetpacking {
                entry.1 = true;
                if frame {
                    clear_move_keys(this);
                }
            } else if entry.1 && msg == MSG_FRAME_FINISH {
                lapsed = true;
            }
        }
    }
    if lapsed {
        jetpack_steer(handle, false);
    }
    if !taken(handle) {
        return false;
    }
    if msg == MSG_JUMP || msg == MSG_JUMP_UP {
        return true;
    }
    if frame {
        clear_move_keys(this);
    }
    false
}

fn clear_move_keys(this: *mut u8) {
    for off in [WORM_KEY_UP, WORM_KEY_DOWN, WORM_KEY_LEFT, WORM_KEY_RIGHT] {
        unsafe { write_i32(this, off, 0) };
    }
}

/// WA's own SetState on a worm, so the state's setup runs.
pub fn set_worm_state(handle: u32, state: u32) {
    if handle == 0 {
        return;
    }
    unsafe {
        let f: SetStateFn = std::mem::transmute(crate::wa::address("WORM_SET_STATE") as usize);
        crate::state_trace::with_cause("lua worm:set_state", || f(handle as *mut u8, state));
    }
}

pub fn drop_carry(handle: u32) {
    drop_held(handle, "drop_carry");
}

/// Give a held worm back: shown again, its collision mask as it was, into
/// falling-under-control (0x7E) through SetState, and the turn thawed once no
/// held worm freezes it.
fn drop_held(handle: u32, cause: &'static str) {
    let_go(handle, Some(cause));
}

/// Give a held worm back; `cause` None leaves its WA state alone (WA has
/// already moved it on, e.g. knocked it flying).
fn let_go(handle: u32, cause: Option<&'static str>) {
    hide_worm(handle, false);
    let mask = CARRY
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|c| c.handle == handle).and_then(|c| c.mask));
    if handle != 0 {
        unsafe {
            if let Some(mask) = mask {
                write_i32(handle as *mut u8, ENT_COLLISION_MASK, mask);
            }
            if let Some(cause) = cause {
                write_i32(handle as *mut u8, ENT_MOVED, 1);
                let f: SetStateFn =
                    std::mem::transmute(crate::wa::address("WORM_SET_STATE") as usize);
                crate::state_trace::with_cause(cause, || f(handle as *mut u8, WORM_STATE_AIRBORNE));
            }
        }
    }
    let thaw = if let Ok(mut g) = CARRY.lock() {
        g.retain(|c| c.handle != handle);
        !g.iter().any(|c| c.freeze)
    } else {
        false
    };
    if thaw {
        crate::world::thaw_turn();
    }
}

pub fn hide_worm(handle: u32, hide: bool) {
    if handle == 0 {
        return;
    }
    if let Ok(mut g) = HIDDEN.lock() {
        if hide {
            if !g.contains(&handle) {
                g.push(handle);
            }
        } else {
            g.retain(|h| *h != handle);
        }
    }
    if hide {
        unsafe {
            write_i32(handle as *mut u8, WORM_SHOW_CURSOR, 0);
        }
    }
}

pub fn hide_body(handle: u32, hide: bool) {
    if handle == 0 {
        return;
    }
    if let Ok(mut g) = BODY_HIDDEN.lock() {
        g.retain(|h| *h != handle);
        if hide {
            g.push(handle);
        }
    }
}

pub fn body_hidden(handle: u32) -> bool {
    BODY_HIDDEN
        .lock()
        .ok()
        .map(|g| g.contains(&handle))
        .unwrap_or(false)
}

pub fn worm_hidden(handle: u32) -> bool {
    HIDDEN
        .lock()
        .ok()
        .map(|g| g.contains(&handle))
        .unwrap_or(false)
}

pub fn install() -> Result<(), String> {
    unsafe {
        ORIG_GRAVITY.store(
            crate::wa::create_hook(crate::wa::address("APPLY_PROJECTILE_GRAVITY") as *mut c_void, gravity_hook as *mut c_void)
                .map_err(|e| format!("gravity hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_MISSILE.store(
            crate::wa::create_hook(crate::wa::address("MISSILE_HANDLE_MESSAGE") as *mut c_void, missile_hook as *mut c_void)
                .map_err(|e| format!("missile hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_MISSILE_FREE.store(
            crate::wa::create_hook(crate::wa::address("MISSILE_FREE") as *mut c_void, missile_free_hook as *mut c_void)
                .map_err(|e| format!("missile free hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_WORM.store(
            crate::wa::create_hook(crate::wa::address("WORM_HANDLE_MESSAGE") as *mut c_void, worm_hook as *mut c_void)
                .map_err(|e| format!("worm hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_ROOT.store(
            crate::wa::create_hook(crate::wa::address("WORLD_ROOT_HANDLE_MESSAGE") as *mut c_void, root_hook as *mut c_void)
                .map_err(|e| format!("world-root hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_DRAW.store(
            crate::wa::create_hook(
                crate::wa::address("WORM_DRAW_SPRITE") as *mut c_void,
                draw_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-sprite hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_NAME.store(
            crate::wa::create_hook(crate::wa::address("WORM_DRAW_NAME") as *mut c_void, name_hook as *mut c_void)
                .map_err(|e| format!("draw-name hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_HUD.store(
            crate::wa::create_hook(crate::wa::address("WORM_DRAW_HUD") as *mut c_void, hud_hook as *mut c_void)
                .map_err(|e| format!("draw-hud hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_HEALTH.store(
            crate::wa::create_hook(
                crate::wa::address("WORM_DRAW_HEALTH") as *mut c_void,
                health_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-health hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_CURSOR.store(
            crate::wa::create_hook(
                crate::wa::address("WORM_DRAW_CURSOR") as *mut c_void,
                cursor_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-cursor hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_OFFMAP.store(
            crate::wa::create_hook(
                crate::wa::address("WORM_DRAW_OFFMAP") as *mut c_void,
                offmap_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-offmap hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_TURN.store(
            crate::wa::create_hook(crate::wa::address("WORM_DRAW_TURN") as *mut c_void, turn_hook as *mut c_void)
                .map_err(|e| format!("draw-turn hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_ARROW.store(
            crate::wa::create_hook(
                crate::wa::address("WORM_DRAW_ARROW") as *mut c_void,
                arrow_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-arrow hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_AIM_CURSOR.store(
            crate::wa::create_hook(
                crate::wa::address("WORM_DRAW_AIM_CURSOR") as *mut c_void,
                aim_cursor_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-aim-cursor hook: {e:?}"))?,
            Ordering::SeqCst,
        );
        ORIG_CROSSHAIR.store(
            crate::wa::create_hook(
                crate::wa::address("WORM_DRAW_CROSSHAIR") as *mut c_void,
                crosshair_hook as *mut c_void,
            )
            .map_err(|e| format!("draw-crosshair hook: {e:?}"))?,
            Ordering::SeqCst,
        );
    }
    log_line("physics wraps: gravity leaf, missile HM, worm HM, world root, draw skip");
    Ok(())
}

fn missile_vt() -> u32 {
    crate::wa::address("MISSILE_VTABLE")
}

fn with_cm_forces(cm: *mut u8, mut m: game::entity::Motion) -> game::entity::Motion {
    let (gy, wind) = unsafe { read_cm_forces(cm) };
    m.cm_gravity = gy;
    m.cm_wind = wind;
    m
}

unsafe extern "thiscall" fn gravity_hook(cm: *mut u8, entity: *mut u8) {
    let missile = unsafe { is_missile(entity, missile_vt()) };
    if missile {
        if let Some(mut m) = unsafe { read_motion(entity) } {
            m.id = crate::missile_boom::missile_id(entity);
            let m = with_cm_forces(cm, m);
            if let Ok(before) = lua_host::gravity_before(m) {
                unsafe { write_scales(entity, &before) };
            }
        }
    }
    let orig = ORIG_GRAVITY.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: ApplyGravityFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(cm, entity) };
    }
    if missile {
        if let Some(mut m) = unsafe { read_motion(entity) } {
            m.id = crate::missile_boom::missile_id(entity);
            let m = with_cm_forces(cm, m);
            if let Ok(after) = lua_host::gravity_after(m) {
                unsafe { write_motion(entity, &after) };
            }
        }
    }
}

unsafe extern "thiscall" fn missile_hook(
    this: *mut u8,
    a: u32,
    msg: u32,
    b: u32,
    c: *mut u8,
) {
    // Homing lock switches blit banks mid-flight (entity+0x394). Keep every
    // body-graphic slot on our custom sheet *before* stock runs. Never on
    // WA's draw (0x3): these fields are simulation state (CreateClusters and
    // the impact path read the flight sprite), and how often a machine draws
    // is its own frame rate, not the lockstep's.
    let remap = msg != MSG_RENDER_SCENE;
    if remap {
        unsafe { remap_homing_body_sprites(this) };
    }
    if msg == MSG_FRAME_FINISH {
        unsafe { missile_frame_hook(this, msg, "pre") };
    }
    let orig = ORIG_MISSILE.load(Ordering::SeqCst);
    begin_handling(this as usize);
    if !orig.is_null() {
        let f: HandleMessageFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(this, a, msg, b, c) };
    }
    if end_handling(this as usize) {
        return;
    }
    // Stock may refresh burn banks when lock engages — push custom again.
    if remap {
        unsafe { remap_homing_body_sprites(this) };
    }
    if msg == MSG_FRAME_FINISH {
        unsafe { missile_frame_hook(this, msg, "post") };
        crate::sprite::advance_loop(this);
    }
}

/// `wa.on.missile_frame`: its motion, and the sprite WA draws it with
/// (`m.sprite`, +0x2EC), written back when a pack changes them.
unsafe fn missile_frame_hook(this: *mut u8, msg: u32, phase: &str) {
    if let Some(mut m) = unsafe { read_motion(this) } {
        // The missile's lockstep id, not its address (`missile_boom::missile_id`).
        m.id = crate::missile_boom::missile_id(this);
        let weapon = crate::missile_boom::weapon_of(this);
        let before = unsafe { read_i32(this, MISSILE_RENDER_SPRITE) };
        let mask_before = unsafe { read_i32(this, game::offsets::ENT_COLLISION_MASK) };
        let (mut sprite, mut mask) = (before, mask_before);
        if let Ok(after) = lua_host::missile_frame(m, msg, phase, weapon, &mut sprite, &mut mask) {
            unsafe { write_motion(this, &after) };
            if sprite != before {
                unsafe { write_i32(this, MISSILE_RENDER_SPRITE, sprite) };
            }
            if mask != mask_before {
                unsafe { write_i32(this, game::offsets::ENT_COLLISION_MASK, mask) };
            }
        }
    }
}

/// Body graphic slots stock HM / pigeon may blit from across flight → lock.
const BODY_GFX_OFFS: &[usize] = &[
    MISSILE_WEAPON_SPRITE,
    MISSILE_WEAPON_ALT_SPRITE,
    MISSILE_RENDER_SPRITE,
    MISSILE_ALT_BANK_SPRITE,
    MISSILE_RENDER_SPRITE_ALT,
    MISSILE_HOMING_SPRITE_A,
    MISSILE_HOMING_SPRITE_B,
];

/// Stock HM engages a second physics/render mode (`+0x394 == 1`) that blits the
/// burn blink pair instead of the flight sprite. Remember the body sheet from
/// the ballistic phase (custom *or* any non-HM replacement like wroll) and keep
/// every body slot on it through the mode switch.
unsafe fn remap_homing_body_sprites(entity: *mut u8) {
    if entity.is_null() {
        return;
    }
    // Homing missiles only. This runs on every message of every missile, and
    // for anything else it did damage: it took the flight sprite as the
    // "body", wrote it into every empty body slot and over the homing pair.
    // The super sheep switches to its caped sprite at take-off from a slot
    // that was empty until then -- already filled with the walking sheep, so
    // the cape never appeared, with no pack touching the sheep at all. Only a
    // homing missile ever enters the lock mode this exists for.
    if unsafe { read_i32(entity, MISSILE_TYPE) } != MISSILE_TYPE_HOMING {
        return;
    }
    // And only a missile of a slot a pack gave its own body. A stock homing
    // missile keeps WA's sprites: guessing a "body" from its fields took
    // Project X's Pigeon Strike's feather trail (134) for one and drew its
    // pigeons as feathers. Strike bombs have no slot here and are left alone.
    let pack = crate::missile_boom::weapon_of(entity).and_then(crate::missile_boom::pack_body);
    if pack.is_none() {
        return;
    }
    let key = entity as usize;
    let mode = unsafe { read_i32(entity, MISSILE_HOMING_MODE) };
    let flight = unsafe { read_i32(entity, MISSILE_RENDER_SPRITE) };
    let weapon = unsafe { read_i32(entity, MISSILE_WEAPON_SPRITE) };
    let pellet = unsafe { read_i32(entity, MISSILE_SHOOT_PELLET) } != 0;
    let mut slots = [0i32; BODY_GFX_OFFS.len()];
    for (id, &off) in slots.iter_mut().zip(BODY_GFX_OFFS) {
        *id = unsafe { read_i32(entity, off) };
    }
    let remembered = HM_CUSTOM
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&key).copied()));
    let Some(body) = homing_body(pack, pellet, &slots, remembered, mode, flight, weapon) else {
        return;
    };
    if let Ok(mut g) = HM_CUSTOM.lock() {
        g.get_or_insert_with(HashMap::new).insert(key, body);
    }

    // The body slots only. WA's homing sprites in the fire params copy are at
    // +0x180 (the flight sprite) and +0x1CC (the lock sprite), both in
    // BODY_GFX_OFFS; nothing else there is a sprite, so nothing else is
    // written.
    let mut wrote = false;
    for &off in BODY_GFX_OFFS {
        let id = unsafe { read_i32(entity, off) };
        if id != body && (id <= 0 || is_stock_homing_body(id)) {
            unsafe { write_i32(entity, off, body) };
            wrote = true;
        }
    }
    for off in [MISSILE_HOMING_SPRITE_A, MISSILE_HOMING_SPRITE_B] {
        if unsafe { read_i32(entity, off) } != body {
            unsafe { write_i32(entity, off, body) };
            wrote = true;
        }
    }

    if wrote || mode == 1 {
        if let Ok(mut logged) = HM_LOGGED.lock() {
            if logged.get_or_insert_with(HashSet::new).insert((key, mode)) {
                log_line(&format!(
                    "hm body remap entity={key:#x} body={body} mode={mode} flight={flight} a={} b={}",
                    unsafe { read_i32(entity, MISSILE_HOMING_SPRITE_A) },
                    unsafe { read_i32(entity, MISSILE_HOMING_SPRITE_B) },
                ));
            }
        }
    }
}

/// The body a homing missile keeps through its lock: the one its slot's pack
/// gave it, or, for a cluster bit, the pack art in its own body slots; else
/// the one remembered for it, or the sheet it flies with ballistically.
/// None leaves the missile alone: without a pack body (`pack`), WA's.
fn homing_body(
    pack: Option<i32>,
    pellet: bool,
    slots: &[i32],
    remembered: Option<i32>,
    mode: i32,
    flight: i32,
    weapon: i32,
) -> Option<i32> {
    let pack = pack?;
    if !pellet {
        return Some(pack);
    }
    slots
        .iter()
        .copied()
        .find(|&id| is_replacement_body(id))
        .or(remembered)
        .or_else(|| {
            // Ballistic: remember whatever non-HM sheet is actually flying so
            // lock cannot fall back to stock 58/59.
            if mode != 0 {
                return None;
            }
            [flight, weapon].into_iter().find(|&id| id > 0 && !is_stock_homing_body(id))
        })
}

fn is_custom_sprite(id: i32) -> bool {
    id >= CUSTOM_SPRITE_SLOT_MIN && id < CUSTOM_SPRITE_SENTINEL
}

/// Custom install slots, or any positive id that is not a stock HM/pigeon body.
fn is_replacement_body(id: i32) -> bool {
    is_custom_sprite(id) || (id > 0 && !is_stock_homing_body(id) && id < CUSTOM_SPRITE_SENTINEL)
}

fn is_stock_homing_body(id: i32) -> bool {
    matches!(id, 58 | 59 | 49 | 175 | 176 | 177 | 178 | 673 | 674)
}

/// Cluster debris (`particles`): a game frame of motion, from WA's own
/// gravity, wind, water line and landscape, all read-only.
fn tick_particles() {
    if crate::particles::count() == 0 {
        return;
    }
    let (gravity, wind) = crate::world::forces();
    let water = crate::world::water_fixed() as f32 / 65536.0;
    crate::particles::tick(
        gravity as f32 / 65536.0,
        wind as f32 / 65536.0,
        if water > 0.0 { water } else { f32::MAX },
        |x, y| crate::world::solid_fixed(x << 16, y << 16),
    );
}

unsafe extern "thiscall" fn root_hook(this: *mut u8, a: u32, msg: u32, b: u32, c: *mut u8) {
    let _perf = match msg {
        MSG_FRAME_FINISH => crate::perf::probe(crate::perf::Section::Sim),
        MSG_RENDER_SCENE => crate::perf::probe(crate::perf::Section::Scene),
        _ => None,
    };
    // BEFORE stock entity blits fill the GPU pending list. Clearing after
    // would wipe quads between enqueue and dispatch (ghost trails).
    if msg == MSG_RENDER_SCENE {
        crate::gpu::begin_frame();
        crate::actor::begin_puff_frame();
        crate::gpu::collect_draws();
    }
    // Stamp mask silhouettes into the walk grid before stock physics so worms,
    // hitscan, and missile bounce treat GIF/PNG solids as dirt.
    if msg == MSG_FRAME_FINISH {
        unsafe { crate::mask_collide::prepare_frame(this) };
    }
    let orig = ORIG_ROOT.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: HandleMessageFn = unsafe { std::mem::transmute(orig) };
        unsafe { f(this, a, msg, b, c) };
    }
    if msg == MSG_FRAME_FINISH {
        crate::actor::flush_deferred();
        crate::contact::flush();
        crate::actor::tick_puffs();
        tick_particles();
        unsafe { crate::mask_collide::resolve_all(this) };
    }
    if msg == MSG_RENDER_SCENE {
        crate::truecolor::begin_frame();
        crate::actor::draw_puffs(this);
        crate::actor::draw_particles(this);
    }
    // End of a sim frame, after WA's own handler: this is the state both
    // peers must agree on, and the point WA increments its frame counter.
    // Deliberately ABOVE the holding() gate -- that is the worm-carry state,
    // false in ordinary play, so anything below it never runs in a normal
    // turn and the probe would never fire.
    if msg == MSG_FRAME_FINISH {
        unsafe { crate::replay::observe(crate::world::session_world()) };
        unsafe { crate::sync::observe(crate::world::session_world()) };
    }
    if msg == MSG_FRAME_FINISH
        && !this.is_null()
        && unsafe { read_i32(this, WORLD_ROOT_TURN_ENDED) != 0 }
    {
        if let Ok(mut g) = STEERED.lock() {
            g.clear();
        }
    }
    if !holding() {
        return;
    }
    if (msg == MSG_FRAME_FINISH || msg == MSG_RENDER_SCENE) && freezing() {
        crate::world::freeze_turn();
    }
    if msg == MSG_FRAME_FINISH && !this.is_null() {
        let ended = unsafe { read_i32(this, WORLD_ROOT_TURN_ENDED) != 0 };
        // A turn that runs while a worm is held (take{ turn = "run" }) ends
        // when its clock runs out; WA then waits for every worm to settle,
        // which a held worm never does, and sends no turn-end message. The
        // ended flag alone was never seen set in that case (a swimmer held
        // past its turn), so the clock counts too -- but not during a
        // retreat, after a shot, whose end (msg 0x3D) lets go instead.
        let timer = unsafe { read_i32(this, WORLD_ROOT_TURN_TIMER) };
        let out_of_time = !freezing() && timer <= 0 && !retreating();
        if ended || out_of_time {
            crate::log_line(&format!(
                "turn over with a worm held (ended flag {}, turn clock {timer}): letting go",
                ended as u8
            ));
            crate::mask_collide::clear();
            drop_all();
        }
    }
}

/// Every other Lua dispatch site logs its errors; this one used to discard
/// them with `unwrap_or(false)`, so a pack whose `wa.on.worm_message` hook
/// threw looked exactly like a pack that had no hook at all. That cost real
/// time to diagnose once and would have cost it again.
///
/// Deduplicated by message text, following the same idiom as `gpu.rs`:
/// this runs for every worm on every message, so an error that repeats every
/// frame would otherwise bury the log it is trying to help with.
fn worm_message_reporting_errors(worm: u32, team: u32, msg: u32) -> bool {
    match lua_host::worm_message(worm, team, msg) {
        Ok(eat) => eat,
        Err(e) => {
            if let Ok(mut seen) = WORM_MSG_ERRORS.lock() {
                if !seen.iter().any(|s| s == &e) {
                    seen.push(e.clone());
                    crate::log_error(&format!("worm_message (msg {msg:#x}): {e}"));
                }
            }
            false
        }
    }
}

unsafe extern "thiscall" fn worm_hook(this: *mut u8, a: u32, msg: u32, b: u32, c: *mut u8) {
    let turn_transition = matches!(msg, MSG_RETREAT_STARTED | MSG_RETREAT_FINISHED | MSG_FINISH_TURN);
    let turn_was_active = turn_transition && !this.is_null()
        && unsafe { read_i32(this, WORM_TURN_ACTIVE) != 0 };
    let team = if this.is_null() {
        0
    } else {
        unsafe { game::entity::read_i32(this, ENT_WORM_TEAM) as u32 }
    };
    let hp_before = crate::world::worm_hp(this as u32);
    let hurt = crate::hurt::is_hurt(msg);
    let mut eat = false;
    if hurt {
        eat = crate::hurt::before(this, a, msg, b, c);
    }
    eat = eat || worm_message_reporting_errors(this as u32, team, msg);
    // After the pack's hooks (which read the keys), before WA's handler.
    eat = eat || owned_input(this, msg);
    if msg == MSG_FIRE_WEAPON { crate::worm_hold::on_fire(this as u32); }
    if msg == MSG_START_TURN {
        if let Err(e) = lua_host::turn_start(this as u32) {
            crate::log_error(&format!("turn_start: {e}"));
        }
    }
    if eat {
        return;
    }
    // WA 3.8.1 makes a dead worm's gravestone while the worm handles one of
    // its messages, before WormEntity::Free (Project X, on 3.6.31, paired
    // them in Free). Whatever grave appears during it is this worm's, the
    // same on every machine: the Lightning Strike revives it by that.
    let graveless = crate::world::graveless_dead_worm(this);
    let graves_before = if graveless != 0 {
        crate::world::list_kind(game::offsets::CLASS_GRAVE, 0)
    } else {
        Vec::new()
    };
    let orig = ORIG_WORM.load(Ordering::SeqCst);
    if !orig.is_null() {
        let f: HandleMessageFn = unsafe { std::mem::transmute(orig) };
        // WA's special-weapon path runs inside its frame handler and checks
        // the selected slot before it can reopen the parachute.
        // Mask only this worm's parachute slot for that call, then restore it.
        // This leaves the equipped weapon intact for all other messages and
        // for the next turn, while native collision and fall damage still run.
        let masked_slot = if msg == MSG_FRAME_FINISH
            && !this.is_null()
            && matches!(
                unsafe { read_i32(this, WORM_STATE) as u32 },
                WORM_STATE_AIRBORNE | WORM_STATE_TUMBLING
            )
            && SUPPRESS_AUTO_PARACHUTE
                .lock()
                .map(|g| g.contains(&(this as u32)))
                .unwrap_or(false)
            && unsafe { read_i32(this, WORM_SELECTED_WEAPON) } == StockWeapon::Parachute as i32
        {
            unsafe { write_i32(this, WORM_SELECTED_WEAPON, 0) };
            true
        } else {
            false
        };
        crate::state_trace::message(this, msg, || unsafe { f(this, a, msg, b, c) });
        if masked_slot && unsafe { read_i32(this, WORM_SELECTED_WEAPON) } == 0 {
            unsafe { write_i32(this, WORM_SELECTED_WEAPON, StockWeapon::Parachute as i32) };
        }
        if !this.is_null() {
            let state = unsafe { read_i32(this, WORM_STATE) as u32 };
            if !matches!(state, WORM_STATE_AIRBORNE | WORM_STATE_TUMBLING) {
                if let Ok(mut g) = SUPPRESS_AUTO_PARACHUTE.lock() {
                    if g.remove(&(this as u32)) {
                        log_line(&format!(
                            "auto parachute suppression ended worm={:#x} state={state:#x}",
                            this as u32
                        ));
                    }
                }
            }
        }
    }
    if !this.is_null() {
        let worm = this as u32;
        if msg == MSG_START_TURN && unsafe { read_i32(this, WORM_TURN_ACTIVE) != 0 } {
            if let Ok(mut phase) = TURN_PHASE.lock() {
                phase.start(worm);
            }
        } else if turn_was_active {
            let events = TURN_PHASE
                .lock()
                .map(|mut phase| phase.advance(worm, msg))
                .unwrap_or_default();
            for event in events {
                let (name, result) = match event {
                    TurnEvent::Retreat => ("turn_retreat", lua_host::turn_retreat(worm)),
                    TurnEvent::End => {
                        // The turn is over: every held worm goes back to WA
                        // first, so turn_end handlers see it let go.
                        if holding() {
                            crate::log_line("turn end with a worm held: letting go");
                            crate::mask_collide::clear();
                            drop_all();
                        }
                        ("turn_end", lua_host::turn_end(worm))
                    }
                    TurnEvent::Cleanup => ("turn_cleanup", lua_host::turn_cleanup(worm)),
                };
                if let Err(e) = result {
                    crate::log_error(&format!("{name}: {e}"));
                }
            }
        }
    }
    if graveless != 0 {
        for grave in crate::world::list_kind(game::offsets::CLASS_GRAVE, 0) {
            if !graves_before.contains(&grave) {
                crate::world::remember_grave_owner(grave, graveless);
            }
        }
    }
    if hurt {
        crate::hurt::after(this, a, msg, b, c, hp_before);
    }
    if msg == MSG_FRAME_FINISH {
        crate::hurt::observe_death(this);
        crate::actor::on_weapon_slot(this);
        crate::world::pump_space_fire(this);
        apply_carry(this);
        crate::worm_sprite::tick(this as usize);
        crate::worm_hold::tick(this as u32);
    }
}

/// Stock SpecialImpact (0x4B) through the hurt wrap. Knockback is undone so
/// pending HP still queues; a standing worm commits the floating number now,
/// a moving one waits until stock ends its momentum.
///
/// Only a worm standing idle, or holding one of the turn's controlled
/// states, gets its state back. Any other stays in the Hurt state (0x80)
/// WA's hit set it in, and WA flies it on at its own speed, lands it and
/// commits its damage. Writing a physics state back skips SetState's setup
/// and leaves WA's landing steps stuck: a falling worm (0x7E) was parked in
/// 0x7A with no speed, in mid-air, and a landing one (0x7F) in 0x76, for
/// good, so the turn never ended (seen under the Highlander's Laser Gun,
/// which cuts the ground from under the worms it burns).
///
/// A worm at rest (standing, or the turn's worm ready) goes back through
/// WA's own SetState, as WA's getting-up does: WA's hit moves the worm into
/// collision layer 3 (entity +0x30) and sets +0x15C, and only SetState's
/// standing setup puts it back in layer 2 and clears +0x15C. A direct write
/// left a burned worm in layer 3, and WA took its next turn from it at once,
/// as lost control (traced in a replay of an online round: Laser Gun hits
/// at frames 47482..47510, then "loses turn due to loss of control" one
/// frame into the worm's turn at 47966).
pub fn hit_worm(target: u32, from: u32, amount: i32) {
    if target == 0 || amount <= 0 {
        return;
    }
    let (from, by) = crate::world::hit_sender(from);
    let this = target as *mut u8;
    unsafe {
        let x = read_i32(this, ENT_POS_X);
        let y = read_i32(this, ENT_POS_Y);
        let vx = read_i32(this, ENT_SPEED_X);
        let vy = read_i32(this, ENT_SPEED_Y);
        let state = read_i32(this, WORM_STATE);
        let airborne = vx != 0 || vy != 0;
        let hp0 = crate::world::worm_hp(target);
        let mut data = [0i32; 8];
        data[1] = x;
        data[2] = y;
        data[HIT_AMOUNT / 4] = amount;
        data[HIT_TEAM / 4] = by;
        crate::state_trace::with_cause("worm:hurt", || {
            crate::hurt::synthetic(|| worm_hook(this, from, MSG_HIT, 0, data.as_mut_ptr() as *mut u8))
        });
        write_i32(this, ENT_POS_X, x);
        write_i32(this, ENT_POS_Y, y);
        write_i32(this, ENT_SPEED_X, vx);
        write_i32(this, ENT_SPEED_Y, vy);
        let hp = crate::world::worm_hp(target);
        if hp > 0 && !airborne && at_rest(state) {
            let set_state: SetStateFn = std::mem::transmute(crate::wa::address("WORM_SET_STATE") as usize);
            crate::state_trace::with_cause("worm:hurt restore", || set_state(this, state as u32));
        } else if hp > 0 && !airborne && restores_after_hit(state) {
            // Mid-shot or aiming: SetState would restart the shot, so the
            // state is written back (the worm's turn ends on the hit anyway).
            let hit = read_i32(this, WORM_STATE) as u32;
            write_i32(this, WORM_STATE, state);
            crate::state_trace::wrote(this, hit, state as u32, "worm:hurt restore");
        }
        if !airborne && hp < hp0 {
            worm_hook(this, from, MSG_COMMIT_HEALTH, 0, std::ptr::null_mut());
        }
    }
}

/// Standing idle (0x65) or the turn's worm ready (0x67): the states WA's own
/// getting-up returns a hit worm to through SetState.
fn at_rest(state: i32) -> bool {
    matches!(state as u32, 0x65 | 0x67)
}

/// The states a hit worm may be put back in (see `hit_worm`): standing idle
/// (0x65), and the turn worm's own (0x66-0x6B active and firing, charging,
/// weapon selected, aiming). Physics and landing states are WA's.
fn restores_after_hit(state: i32) -> bool {
    use game::offsets::{WORM_STATE_AIMING, WORM_STATE_CHARGING};
    matches!(state as u32, 0x65..=0x6B | WORM_STATE_CHARGING | 0x77 | WORM_STATE_AIMING)
}

fn apply_carry(this: *mut u8) {
    if this.is_null() {
        return;
    }
    let handle = this as u32;
    let now = unsafe { read_i32(this, WORM_STATE) } as u32;
    let Some((x, y, facing, let_loose)) = CARRY.lock().ok().and_then(|mut g| {
        g.iter_mut().find(|c| c.handle == handle).map(|c| {
            // Knocked flying while taken: WA owns it again, with its speed.
            let loose = c.taken && knocked(now) && !knocked(c.state);
            c.state = now;
            (c.x, c.y, c.facing, loose)
        })
    }) else {
        return;
    };
    // Dead while held (a pack's damage, or a blast WA did not knock it
    // flying with): WA starts a worm's death only once it has come to rest,
    // which a held worm never does, so the match would wait for it, its turn
    // frozen if the hold froze it. WA owns it again, in whatever state it is.
    if let_loose || crate::world::worm_hp(handle) <= 0 {
        let_go(handle, None);
        return;
    }
    unsafe {
        write_i32(this, ENT_POS_X, x);
        write_i32(this, ENT_POS_Y, y);
        write_i32(this, ENT_SPEED_X, 0);
        write_i32(this, ENT_SPEED_Y, 0);
        write_i32(this, WORM_FACING, facing);
        if worm_hidden(handle) {
            write_i32(this, WORM_SHOW_CURSOR, 0);
        }
        crate::world::hold_turn_timer();
    }
}

/// `WormEntity__DrawSprite_Maybe` is usercall EAX=this. Skip stock blit when hidden.
/// Must stay naked: an `extern "C"` trampoline clobbers EAX (prior match-start AV).
unsafe extern "C" fn draw_hidden(this: *mut u8) -> u8 {
    if this.is_null() {
        return 0;
    }
    worm_hidden(this as u32) as u8
}

/// The body sprite alone also skips for `worm.body_visible = false`.
unsafe extern "C" fn body_draw_hidden(this: *mut u8) -> u8 {
    if this.is_null() {
        return 0;
    }
    (worm_hidden(this as u32) || body_hidden(this as u32)) as u8
}

#[unsafe(naked)]
unsafe extern "C" fn draw_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push eax",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym body_draw_hidden,
        orig = sym ORIG_DRAW,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn cursor_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push eax",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_CURSOR,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn name_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push esi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_NAME,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn hud_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push esi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_HUD,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn health_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push esi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_HEALTH,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn offmap_hook() {
    core::arch::naked_asm!(
        "push ecx",
        "push edx",
        "push dword ptr [esp+12]",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "ret 4",
        check = sym draw_hidden,
        orig = sym ORIG_OFFMAP,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn turn_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push ecx",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_TURN,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn arrow_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push esi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_ARROW,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn aim_cursor_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push esi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_AIM_CURSOR,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn crosshair_hook() {
    core::arch::naked_asm!(
        "push eax",
        "push ecx",
        "push edx",
        "push edi",
        "call {check}",
        "add esp, 4",
        "test al, al",
        "jnz 2f",
        "pop edx",
        "pop ecx",
        "pop eax",
        "jmp dword ptr [{orig}]",
        "2:",
        "pop edx",
        "pop ecx",
        "pop eax",
        "xor eax, eax",
        "ret",
        check = sym draw_hidden,
        orig = sym ORIG_CROSSHAIR,
    );
}

#[cfg(test)]
mod homing_body_tests {
    use super::homing_body;

    #[test]
    fn a_stock_homing_missile_keeps_was_sprites() {
        // Project X's Pigeon Strike: stock pigeons (175) with a feather
        // trail (134) in a body slot, on a slot no pack retargeted.
        assert_eq!(homing_body(None, false, &[175, 134, 175, 0, 0, 175, 175], None, 0, 175, 175), None);
        assert_eq!(homing_body(None, true, &[175, 134], Some(134), 1, 134, 175), None);
    }

    #[test]
    fn a_pack_missile_keeps_its_slots_body() {
        // The Blaster Worm: its record's 175 retargeted to pack sprite 807.
        assert_eq!(homing_body(Some(807), false, &[134, 807, 175], None, 1, 175, 175), Some(807));
    }

    #[test]
    fn a_pack_cluster_bit_keeps_its_own_art() {
        assert_eq!(homing_body(Some(807), true, &[807, 808, 175], None, 0, 808, 807), Some(807));
        assert_eq!(homing_body(Some(807), true, &[0, 58, 59], Some(808), 1, 58, 59), Some(808));
        assert_eq!(homing_body(Some(807), true, &[0, 58, 59], None, 0, 808, 58), Some(808));
    }
}

#[cfg(test)]
mod freed_in_handler_tests {
    use super::{begin_handling, end_handling, mark_freed};

    #[test]
    fn a_missile_freed_in_its_handler_is_reported_freed() {
        begin_handling(0x7100);
        mark_freed(0x7100);
        assert!(end_handling(0x7100));
    }

    #[test]
    fn only_the_freed_one_of_nested_handlers() {
        // A blast in one missile's frame frees another, whose own message
        // WA handles inside the first's.
        begin_handling(0x7200);
        begin_handling(0x7300);
        mark_freed(0x7300);
        assert!(end_handling(0x7300));
        assert!(!end_handling(0x7200));
    }

    #[test]
    fn a_free_outside_any_handler_marks_nothing() {
        mark_freed(0x7400);
        begin_handling(0x7400);
        assert!(!end_handling(0x7400), "a new missile at a freed address is live");
    }
}
