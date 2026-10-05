//! `wa.crates.spawn`: one of WA's own crates, built as WA's turn drop builds
//! one (0x561AD2): WA runs its pickup, parachute and detonation, so it is a
//! crate to every weapon and worm. A weapon or utility crate's contents are
//! WA's draw from the match's crate deck; a health crate holds its health.

use game::entity::{read_i32, write_i32};
use game::offsets::{
    CRATE_CONTENTS_DEFINED, CRATE_CONTENTS_RANDOM, CRATE_DESC_CONTENTS, CRATE_DESC_HEALTH,
    CRATE_DESC_KIND, CRATE_DESC_SIZE, CRATE_DESC_X, CRATE_DESC_Y, CRATE_ENTITY_SIZE,
    CRATE_ENTITY_ZEROED, CRATE_UTILITY_FIRST, CRATE_UTILITY_LAST, GAME_WORLD_CRATE_DECK,
    GAME_WORLD_CRATE_DECK_COUNT, GAME_WORLD_CRATE_DRAWN, GAME_WORLD_CRATE_DRAWN_COUNT,
};

const KIND_WEAPON: i32 = 1;
const KIND_HEALTH: i32 = 2;
const KIND_UTILITY: i32 = 4;
/// The deck and the drawn list hold at most this many each (0x527F26).
const DECK_MAX: i32 = 100;

type CrateCtorFn = unsafe extern "stdcall" fn(*mut u8, *mut u8, *mut u8, u32) -> *mut u8;

/// Whether a weapon WA's crate deck holds goes in a crate of `kind`.
fn suits(weapon: i32, kind: i32) -> bool {
    let utility = (CRATE_UTILITY_FIRST..=CRATE_UTILITY_LAST).contains(&weapon);
    match kind {
        KIND_WEAPON => !utility,
        KIND_UTILITY => utility,
        _ => false,
    }
}

/// Whether the match's crate deck, drawn or not, holds anything a crate of
/// `kind` can be filled with. WA draws until it finds one, so without it a
/// collected crate would freeze the game.
unsafe fn deck_holds(world: *mut u8, kind: i32) -> bool {
    let part = |at: usize, count_at: usize| -> bool {
        let n = unsafe { read_i32(world, count_at) }.clamp(0, DECK_MAX) as usize;
        (0..n).any(|i| suits(unsafe { read_i32(world, at + i * 4) }, kind))
    };
    part(GAME_WORLD_CRATE_DECK, GAME_WORLD_CRATE_DECK_COUNT)
        || part(GAME_WORLD_CRATE_DRAWN, GAME_WORLD_CRATE_DRAWN_COUNT)
}

/// The description the constructor copies: everything zero but the place,
/// the kind, and its contents: a health crate's health, or WA's random draw.
fn desc(args: &lua_host::CrateArgs) -> [u8; CRATE_DESC_SIZE] {
    let mut d = [0u8; CRATE_DESC_SIZE];
    let base = d.as_mut_ptr();
    unsafe {
        write_i32(base, CRATE_DESC_X, args.x);
        write_i32(base, CRATE_DESC_Y, args.y);
        write_i32(base, CRATE_DESC_KIND, args.kind);
        write_i32(base, CRATE_DESC_HEALTH, args.health);
        let contents = if args.kind == KIND_HEALTH { CRATE_CONTENTS_DEFINED } else { CRATE_CONTENTS_RANDOM };
        write_i32(base, CRATE_DESC_CONTENTS, contents);
    }
    d
}

pub fn spawn(args: &lua_host::CrateArgs) -> Result<u32, String> {
    let Some(ctor) = crate::wa::try_address("CRATE_ENTITY_CTOR") else {
        return Err("wa.crates.spawn: WA's crate constructor was not located".into());
    };
    let root = crate::world::session_root();
    if root.is_null() {
        return Err("wa.crates.spawn: no match running".into());
    }
    if args.kind != KIND_HEALTH {
        let world = unsafe { crate::world::session_world_ptr() };
        if world.is_null() || !unsafe { deck_holds(world, args.kind) } {
            let what = if args.kind == KIND_UTILITY { "utility" } else { "weapon" };
            return Err(format!("wa.crates.spawn: this match's crates hold no {what}"));
        }
    }
    // PX's Root->GetObject(25, 0): the world's physics parent, as for actors.
    let parent = crate::actor::physics_parent(root).ok_or("wa.crates.spawn: no physics parent")?;
    let mut d = desc(args);
    unsafe {
        let malloc: unsafe extern "C" fn(usize) -> *mut u8 =
            std::mem::transmute(crate::wa::address("WA_MALLOC") as usize);
        let this = malloc(CRATE_ENTITY_SIZE);
        if this.is_null() {
            return Err("wa.crates.spawn: malloc failed".into());
        }
        std::ptr::write_bytes(this, 0, CRATE_ENTITY_ZEROED);
        let ctor: CrateCtorFn = std::mem::transmute(ctor as usize);
        let p = ctor(this, parent, d.as_mut_ptr(), args.drop as u32);
        if p.is_null() {
            return Err("wa.crates.spawn: the crate constructor failed".into());
        }
        Ok(p as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_health_crate_holds_its_health_and_the_others_wa_s_draw() {
        let weapon = lua_host::CrateArgs { x: 0, y: 0, kind: 1, health: 0, drop: 2 };
        let mut d = desc(&weapon);
        assert_eq!(unsafe { read_i32(d.as_mut_ptr(), 0x154) }, 2, "a weapon crate: PX's WCT_RANDOM");
        let utility = lua_host::CrateArgs { kind: 4, ..weapon };
        let mut d = desc(&utility);
        assert_eq!(unsafe { read_i32(d.as_mut_ptr(), 0x154) }, 2, "a utility crate too");
    }

    #[test]
    fn a_crate_s_kind_must_be_in_the_deck() {
        // A world's deck as WA lays it out: weapons, then a drawn list.
        let mut world = vec![0u8; 0x4000];
        let w = world.as_mut_ptr();
        unsafe {
            write_i32(w, GAME_WORLD_CRATE_DECK, 1); // bazooka
            write_i32(w, GAME_WORLD_CRATE_DECK_COUNT, 1);
            assert!(deck_holds(w, KIND_WEAPON));
            assert!(!deck_holds(w, KIND_UTILITY), "no utility: WA would draw for ever");
            write_i32(w, GAME_WORLD_CRATE_DRAWN, 0x40); // a utility, drawn already
            write_i32(w, GAME_WORLD_CRATE_DRAWN_COUNT, 1);
            assert!(deck_holds(w, KIND_UTILITY), "the drawn list comes back into the deck");
            assert!(!deck_holds(w, KIND_HEALTH), "a health crate is never drawn for");
        }
        assert!(suits(0x3E, KIND_UTILITY) && suits(0x46, KIND_UTILITY) && suits(0x47, KIND_WEAPON) && suits(0x3D, KIND_WEAPON));
    }

    #[test]
    fn the_description_is_pxs_crate_desc_with_its_health() {
        let args = lua_host::CrateArgs { x: 0x100000, y: -0x320000, kind: 2, health: 45, drop: 2 };
        let mut d = desc(&args);
        let p = d.as_mut_ptr();
        unsafe {
            assert_eq!(read_i32(p, 0x04), 0x100000);
            assert_eq!(read_i32(p, 0x08), -0x320000);
            assert_eq!(read_i32(p, 0x14), 2, "PX's CT_HEALTH");
            assert_eq!(read_i32(p, 0x13C), 45);
            assert_eq!(read_i32(p, 0x154), 0, "PX's WCT_DEFINED: random contents would draw for ever");
        }
        assert!(d.iter().enumerate().all(|(i, &b)| b == 0
            || [0x04..0x08, 0x08..0x0C, 0x14..0x18, 0x13C..0x140].iter().any(|r| r.contains(&i))));
    }
}
