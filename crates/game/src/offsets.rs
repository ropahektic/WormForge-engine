//! Pointer and size constants confirmed in Ghidra against InitWeaponTable (0x0053CAB0).

pub const IMAGE_BASE: u32 = 0x0040_0000;

pub const INIT_WEAPON_TABLE: u32 = 0x0053_CAB0;
pub const STR_VERSION_381: u32 = 0x0064_1C60;
pub const G_GAME_SESSION: u32 = 0x007A_0884;

/// `GameRuntime.world` — InitWeaponTable reads `[runtime+0x488]`.
pub const GAME_RUNTIME_WORLD: usize = 0x488;
/// `GameRuntime.display` — world constructor `param_2[0x134]`.
pub const GAME_RUNTIME_DISPLAY: usize = 0x4D0;
/// `GameWorld.weapon_table` — pointer to 71 entries, memset size 0x80B0.
pub const GAME_WORLD_WEAPON_TABLE: usize = 0x510;
/// TeamArena at GameWorld+0x4628 (GetAmmo / AddAmmo).
pub const GAME_WORLD_TEAM_ARENA: usize = 0x4628;
pub const TEAM_GET_AMMO: u32 = 0x0052_25E0;
pub const TEAM_ADD_AMMO: u32 = 0x0052_2640;
/// TeamArena ammo dword table. Index `alliance * 142 + weapon`.
pub const TEAM_ARENA_AMMO: usize = 0x1EB4;
/// TeamArena delay dword table. Same index as ammo.
pub const TEAM_ARENA_DELAY: usize = 0x1FD0;
pub const TEAM_AMMO_STRIDE: usize = 142;
/// WormEntity__SelectWeapon — usercall EDI=this, stdcall (weapon, ammo) RET 8.
pub const WORM_SELECT_WEAPON: u32 = 0x0051_AE50;
/// WormEntity__StartFiring — usercall EAX=this. Space-down: zeros power bar, enters charge.
pub const WORM_START_FIRING: u32 = 0x0051_B7F0;
/// WormEntity__ReleaseWeapon — Space-up: launches the charged shot.
pub const WORM_RELEASE_WEAPON: u32 = 0x0051_C010;
/// WormEntity.turn_active — StartTurn 1, FinishTurn 0.
pub const WORM_TURN_ACTIVE: usize = 0x104;
/// WormEntity.weapons_enabled (+0x28C). ReleaseWeapon no-ops if 0.
pub const WORM_WEAPONS_ENABLED: usize = 0x28C;
/// WormEntity.selected_weapon_ammo (+0x174).
pub const WORM_SELECTED_AMMO: usize = 0x174;
/// WormEntity.active_weapon_entry (+0x36C) — `&WeaponTable[selected]`.
pub const WORM_ACTIVE_WEAPON: usize = 0x36C;
/// WorldEntity.state (+0x44).
pub const WORM_STATE: usize = 0x44;
/// Ground slope selector used by `WormEntity__calc_sprite`: negative adds the
/// uphill (+1) pose, positive adds the downhill (+2) pose.
pub const WORM_SLOPE_DIR: usize = 0x1AC;
/// WeaponCharging — power bar live.
pub const WORM_STATE_CHARGING: u32 = 0x73;
/// AimingAngle — crosshair up, ready for Space.
pub const WORM_STATE_AIMING: u32 = 0x7B;

pub const WEAPON_ENTRY_SIZE: usize = 0x1D0;
pub const STOCK_WEAPON_COUNT: usize = 71;
pub const WEAPON_TABLE_BYTES: usize = STOCK_WEAPON_COUNT * WEAPON_ENTRY_SIZE;

pub const WEAPON_NAME1: usize = 0x00;
pub const WEAPON_PANEL_STATE: usize = 0x08;
pub const WEAPON_REQUIRES_AIMING: usize = 0x0C;
pub const WEAPON_DEFINED: usize = 0x10;
pub const WEAPON_UNKNOWN_18: usize = 0x18;
pub const WEAPON_CREATES_PROJECTILE: usize = 0x20;
pub const WEAPON_AVAILABILITY: usize = 0x24;
pub const WEAPON_ENABLED: usize = 0x28;
pub const WEAPON_FIRE_TYPE: usize = 0x30;
pub const WEAPON_SPECIAL_SUBTYPE: usize = 0x34;
pub const WEAPON_FIRE_METHOD: usize = 0x38;
pub const WEAPON_FIRE_PARAMS: usize = 0x3C;
pub const WEAPON_DAMAGE: usize = 0x50;
/// Missile sprite id (WeaponFireParams+0x24). Bazooka=48, Grenade=50.
pub const WEAPON_SPRITE_ID: usize = 0x60;
pub const WEAPON_GRAVITY_PCT: usize = 0x70;
pub const WEAPON_WIND_PCT: usize = 0x74;
pub const WEAPON_BOUNCE_PCT: usize = 0x78;
/// Unused stock id: GetSpriteFrameForBlit rejects `id-1 >= 0x3FE`, so 1023
/// draws nothing. Do not put custom sheets here — install into 800..=1022.
pub const CUSTOM_SPRITE_SENTINEL: i32 = 1023;
/// MissileEntity.weapon_data[9] — copy of fire-params sprite_id.
pub const MISSILE_WEAPON_SPRITE: usize = 0x180;
/// MissileEntity.fire_particle_trigger — sprite id missile render actually blits.
pub const MISSILE_RENDER_SPRITE: usize = 0x2EC;
/// MissileEntity.animation_phase — GetSpriteFrameForBlit anim_value.
pub const MISSILE_ANIM_PHASE: usize = 0x3E8;

pub const APPLY_PROJECTILE_GRAVITY: u32 = 0x004F_CB30;
pub const MISSILE_HANDLE_MESSAGE: u32 = 0x0050_B400;
pub const WORM_HANDLE_MESSAGE: u32 = 0x0051_0B40;
pub const WORLD_ROOT_HANDLE_MESSAGE: u32 = 0x0055_DC00;
/// WormEntity__SetState — thiscall ECX=this, stack state. Slot 14 override.
pub const WORM_SET_STATE: u32 = 0x0050_E850;
/// Post-fire / airborne (stock uses `wroll` once moving, not jump-hang).
pub const WORM_STATE_AIRBORNE: u32 = 0x7E;
/// WormEntity__DrawSprite_Maybe — usercall EAX=this. Body sprite blit.
pub const WORM_DRAW_SPRITE: u32 = 0x0050_FCD0;
/// WormEntity__DrawWormName_Maybe — usercall ESI=this.
pub const WORM_DRAW_NAME: u32 = 0x0050_FB50;
/// WormEntity__DrawHudLabels_Maybe — usercall ESI=this (HP / banners).
pub const WORM_DRAW_HUD: u32 = 0x0050_FDC0;
/// WormEntity__DrawHealthLabel_Maybe — usercall ESI=this.
pub const WORM_DRAW_HEALTH: u32 = 0x0051_0260;
/// WormEntity__DrawCursorMarker_Maybe — usercall EAX=this (aim arrow / crosshair).
pub const WORM_DRAW_CURSOR: u32 = 0x0051_03A0;
/// WormEntity__DrawOffMapMarker_Maybe — stdcall this on the stack.
pub const WORM_DRAW_OFFMAP: u32 = 0x0050_F900;
/// WormEntity__DrawTurnIndicator — thiscall ECX=this.
pub const WORM_DRAW_TURN: u32 = 0x0050_F810;
/// WormEntity__DrawAimingArrow_Maybe — usercall ESI=this.
pub const WORM_DRAW_ARROW: u32 = 0x0051_3A90;
/// WormEntity__DrawAimCursor_Maybe — usercall ESI=this.
pub const WORM_DRAW_AIM_CURSOR: u32 = 0x0051_D680;
/// DrawCrosshairLine — usercall EDI=this (shotgun / bazooka line).
pub const WORM_DRAW_CROSSHAIR: u32 = 0x0051_97D0;
/// Stock `wflylnk.spr` — jump hang, ping-pong in the .spr flags.
pub const SPRITE_WFLY_LNK: u32 = 278;
pub const WORM_FACING: usize = 0x3DC;
/// WormEntity show-cursor latch (`+0x268`). Strike targeting.
pub const WORM_SHOW_CURSOR: usize = 0x268;
/// WorldRootEntity: non-zero pauses the HUD turn clock (grenade-in-flight).
pub const WORLD_ROOT_WORM_ACTIVE: usize = 0x108;
pub const WORLD_ROOT_TURN_TIMER_DISPLAY: usize = 0x188;
pub const WORLD_ROOT_TURN_TIMER: usize = 0x18C;
pub const WORLD_ROOT_TURN_ENDED: usize = 0x150;
/// GameWorld copy of WorldRoot `worm_active`.
pub const GAME_WORLD_WORM_ACTIVE: usize = 0x7234;
pub const SHARED_KEY_TEAM: u32 = 0x15;
/// `TeamEntity__reset_cursor` — thiscall, clears planted strike X.
pub const TEAM_RESET_CURSOR: u32 = 0x0055_6D00;
pub const TEAM_CURSOR_MOVE: usize = 0x3D0;
pub const TEAM_CURSOR_SPRITE: usize = 0x3D4;
pub const TEAM_CURSOR_MARK: usize = 0x3E0;
pub const STRIKE_FIRE: u32 = 0x0051_E2C0;
pub const CREATE_WEAPON_PROJECTILE: u32 = 0x0051_E0F0;
pub const WORLD_ENTITY_CTOR: u32 = 0x004F_ED50;
pub const WORLD_ENTITY_FREE: u32 = 0x004F_EF10;
pub const WORLD_ENTITY_TRY_MOVE: u32 = 0x004F_E070;
pub const WORLD_ENTITY_VTABLE: u32 = 0x0066_41F8;
pub const WA_CRT_MALLOC: u32 = 0x005C_0AB8;
pub const WA_MALLOC: u32 = 0x005C_0AE3;
pub const CREATE_EXPLOSION: u32 = 0x0054_8080;
pub const RQ_DRAW_SPRITE_LOCAL: u32 = 0x0054_2060;
pub const WORLD_ENTITY_VTABLE_SLOTS: usize = 32;
pub const STUB_RETURNS0: u32 = 0x0054_5780;
pub const STUB_RETURNS1: u32 = 0x0059_2A50;
pub const STUB_NOOP: u32 = 0x004A_A060;
pub const SET_TEXTBOX_TEXT: u32 = 0x004F_B070;
pub const RENDER_WAITING_FOR_PEERS: u32 = 0x0053_4F20;
pub const GAME_RENDER_WAIT_CALL: u32 = 0x0053_448B;
pub const MISSILE_VTABLE: u32 = 0x0066_4438;
pub const COLLISION_MANAGER_VTABLE: u32 = 0x0066_4194;

pub const ENT_VTABLE: usize = 0x00;
pub const ENT_SHARED_DATA: usize = 0x24;
pub const ENT_WORLD: usize = 0x2C;
pub const ENT_CLASS_TYPE: usize = 0x20;
pub const ENT_COLLISION_LAYER: usize = 0x30;
pub const ENT_COLLISION_MASK: usize = 0x34;
pub const ENT_ACTIVE: usize = 0x3C;
/// Parent-defaults overlay starts at entity+0x38. Damp / wind / gravity live here.
pub const ENT_DAMP: usize = 0x58;
pub const ENT_WIND_SCALE: usize = 0x5C;
pub const ENT_GRAVITY_SCALE: usize = 0x60;
pub const ENT_POS_X: usize = 0x84;
pub const ENT_POS_Y: usize = 0x88;
pub const ENT_SPEED_X: usize = 0x90;
pub const ENT_SPEED_Y: usize = 0x94;
pub const ENT_SETTLE: usize = 0xAC;
pub const ENT_MOVED: usize = 0xB8;
/// WorldEntity explosion-damage accumulator; subclasses consume and clear it.
pub const ENT_DAMAGE_ACCUM: usize = 0xD4;
pub const ENT_HELPER: usize = 0xDC;
pub const ENT_WORM_TEAM: usize = 0xFC;

pub const MSG_FRAME_FINISH: u32 = 0x2;
pub const MSG_RENDER_SCENE: u32 = 0x3;
pub const MSG_JUMP: u32 = 0x24;
pub const MSG_JUMP_UP: u32 = 0x25;
/// Space down — `StartFiring`, power bar.
pub const MSG_FIRE_WEAPON: u32 = 0x26;
/// Space up — `ReleaseWeapon`, the shot leaves.
pub const MSG_RELEASE_WEAPON: u32 = 0x27;
pub const MSG_START_TURN: u32 = 0x34;
pub const MSG_ENABLE_WEAPONS: u32 = 0x45;

pub const GAME_WORLD_DISPLAY: usize = 0x004;
/// `GameWorld.sound` — DSSound* (Jelly LoadWav / PlaySoundLocal).
pub const GAME_WORLD_SOUND: usize = 0x008;
pub const GAME_WORLD_RENDER_QUEUE: usize = 0x524;
pub const GAME_WORLD_COLLISION_MANAGER: usize = 0x528;
pub const GAME_WORLD_MISSILE_WEIGHT: usize = 0x72A4;
pub const WORM_SELECTED_WEAPON: usize = 0x170;
pub const WORM_CURSOR_X: usize = 0x2E0;
pub const WORM_CURSOR_Y: usize = 0x2E4;
pub const CTX_SPAWN_X: usize = 0x08;
pub const CTX_SPAWN_Y: usize = 0x0C;
pub const CTX_VEL_X: usize = 0x10;
pub const CTX_VEL_Y: usize = 0x14;
pub const CTX_CURSOR_X: usize = 0x18;
pub const CTX_CURSOR_Y: usize = 0x1C;
pub const SHARED_FILTER_PHYSICS: u32 = 0x19;
pub const SHARED_KEY_WORM: u32 = 0x02;
pub const WORM_INDEX: usize = 0x100;
pub const ARENA_WORM_HP: usize = 0x40EC;
pub const WORLD_TEAM_STRIDE: usize = 0x51C;
pub const ARENA_WORM_STRIDE: usize = 0x9C;
pub const MAX_TEAMS: u32 = 6;
pub const MAX_WORMS_PER_TEAM: u32 = 8;
pub const GAME_RUNTIME_WORLD_ROOT: usize = 0x8;
pub const COLLISION_LAYER_MISSILE: u32 = 0xF;
pub const DROP_SKY_Y: i32 = -0x00D0_0000;
pub const LUA_ACTOR_MAGIC: u32 = 0x774B_4C41;
pub const LUA_ACTOR_SIZE: usize = 0x140;
pub const ACTOR_MAGIC: usize = 0xFC;
pub const ACTOR_SPRITE: usize = 0x100;
pub const ACTOR_PHASE: usize = 0x104;
pub const ACTOR_DELTA: usize = 0x108;
pub const ACTOR_DEAD: usize = 0x10C;
pub const ACTOR_OWNER: usize = 0x110;
pub const ACTOR_DAMAGE: usize = 0x114;
pub const ACTOR_BOOM_ID: usize = 0x118;
pub const ACTOR_ORIGIN: usize = 0x11C;
pub const ACTOR_SPR_W: usize = 0x120;
pub const ACTOR_SPR_H: usize = 0x124;
pub const ACTOR_FLAGS: usize = 0x128;
pub const ACTOR_ORIGIN_CENTER: i32 = 0;
pub const ACTOR_ORIGIN_BOTTOM: i32 = 1;
pub const ACTOR_FLAG_EXPLODE: i32 = 1;
pub const ACTOR_FLAG_GRAVITY: i32 = 2;
pub const ACTOR_FLAG_LOOP: i32 = 4;
pub const ACTOR_FLAG_COLLIDE_TERRAIN: i32 = 8;
pub const ACTOR_FLAG_COLLIDE_WORMS: i32 = 16;
pub const ACTOR_FLAG_DAMAGE_TERRAIN: i32 = 32;
pub const ACTOR_FLAG_DAMAGE_WORMS: i32 = 64;
pub const ACTOR_FLAG_PERSIST: i32 = 128;
pub const ACTOR_FLAG_DROWN: i32 = 256;
pub const ACTOR_FLAG_EXPLODE_BELOW: i32 = 512;
/// Stretch this actor's sheet over the whole window (GDI overlay).
pub const ACTOR_FLAG_SCREEN_FILL: i32 = 1024;
/// Draw this actor's sheet centered on top of fill overlays.
pub const ACTOR_FLAG_SCREEN_FRONT: i32 = 2048;
/// Mirror the actor sprite on X (art faces right). OR'd into the blit id.
pub const ACTOR_FLAG_MIRROR_X: i32 = 4096;
/// `RQ_DrawSpriteLocal` sprite-id flag: flip X.
pub const SPRITE_MIRROR_X: u32 = 0x0004_0000;
pub const ACTOR_FEET_X0: usize = 0x12C;
pub const ACTOR_FEET_X1: usize = 0x130;
pub const ACTOR_FEET_Y: usize = 0x134;
pub const ACTOR_CONTACT: usize = 0x138;
/// Lua brain id (`0` = spawn-table sugar). Last dword of `LUA_ACTOR_SIZE`.
pub const ACTOR_BRAIN: usize = 0x13C;
pub const SHARED_WORLD_ROOT: u32 = 0x14;
pub const GAME_WORLD_LANDSCAPE: usize = 0x20;
pub const GAME_WORLD_WATER_LEVEL: usize = 0x5E0;
pub const GAME_WORLD_WATER_KILL_Y: usize = 0x5E4;
/// Map pixel size (GameWorld). Blas / BitGrid fallback.
pub const GAME_WORLD_MAP_W: usize = 0x77C0;
/// GameWorld.viewport_pixel_width / height (GameRender).
pub const GAME_WORLD_VIEWPORT_W: usize = 0x77AC;
pub const GAME_WORLD_VIEWPORT_H: usize = 0x77B0;
/// viewport_coords[3] camera center (Fixed 16.16). ClipContext / listener.
pub const GAME_WORLD_VIEW_CX: usize = 0x8CEC;
pub const GAME_WORLD_VIEW_CY: usize = 0x8CF0;
/// GameWorld land targeting cursor (16.16). Blas / teleport marker.
pub const GAME_WORLD_LAND_CURSOR_X: usize = 0x7380;
pub const GAME_WORLD_LAND_CURSOR_Y: usize = 0x7384;
pub const GAME_WORLD_LAND_CURSOR_X_LAST: usize = 0x7388;
pub const GAME_WORLD_LAND_CURSOR_Y_LAST: usize = 0x738C;
/// WorldRootEntity active team, 1-based.
pub const WORLD_ROOT_CURRENT_TEAM: usize = 0x12C;
pub const SPRITE_CURSOR_R: i32 = 32;
pub const SPRITE_MARKER_R: i32 = 38;
pub const GAME_WORLD_MAP_H: usize = 0x77C4;
/// Landscape solid BitGrid (Landscape+0x910). Blas / wkGolf.
pub const LANDSCAPE_SOLID_GRID: usize = 0x910;
pub const BITGRID_BITS: usize = 0x08;
pub const BITGRID_BPP: usize = 0x0C;
pub const BITGRID_STRIDE: usize = 0x10;
pub const BITGRID_W: usize = 0x14;
pub const BITGRID_H: usize = 0x18;
pub const LANDSCAPE_RAY_COLLISION: u32 = 0x0057_D240;
/// PlaySoundLocal — usercall EAX=pitch, ECX=volume, EDI=entity, stack id+flags, RET 8.
pub const PLAY_SOUND_LOCAL: u32 = 0x004F_DFE0;
/// DSSound vtable slot 12 — thiscall(this, slot_idx, path). Jelly custom WAV.
pub const DS_SOUND_LOAD_WAV: u32 = 0x0057_3FF0;
/// First free DSSound slot after stock effects + last-team speech (Jelly 451).
pub const CUSTOM_SOUND_SLOT_MIN: i32 = 451;
pub const ENT_PARENT: usize = 0x04;
pub const ENT_CHILDREN_WATERMARK: usize = 0x10;
pub const ENT_CHILDREN_DATA: usize = 0x14;
pub const CLASS_MINE: i32 = 8;
pub const CLASS_MISSILE: i32 = 11;
pub const CLASS_CRATE: i32 = 15;
pub const CLASS_WORM: i32 = 18;
pub const CLASS_OIL: i32 = 30;
pub const MSG_EXPLOSION: u32 = 28;
pub const GAME_WORLD_TEXTBOX: usize = 0x544;
pub const GAME_SESSION_RUNTIME: usize = 0xA0;

pub const CM_SIZE: usize = 0x264;
pub const CM_ENTITIES: usize = 0x18;
pub const CM_PARTICLE_GRAVITY_Y: usize = 0x22C;
pub const CM_WIND_BIAS: usize = 0x230;
pub const CM_WORLD: usize = 0x240;

pub const DISPLAY_SPRITE_CACHE: usize = 0x004;
pub const DISPLAY_SPRITE_LAYERS: usize = 0x008;
pub const DISPLAY_SPRITE_PTRS: usize = 0x1008;
pub const DISPLAY_SPRITE_BANKS: usize = 0x2008;
pub const DISPLAY_VT_DRAW_SCALED: usize = 0x50;
/// `DisplayBase.layer_contexts[4]` — PaletteContext* per layer (0 unused).
pub const DISPLAY_LAYER_CONTEXTS: usize = 0x311C;
/// `DisplayBase.layer_visibility[4]` — LoadSprite increments the layer slot.
pub const DISPLAY_LAYER_VISIBILITY: usize = 0x3530;
/// Main game layer. Stock `LoadSprite(..., 3, id, ...)` for debris/weapons.
pub const DISPLAY_GAME_LAYER: u32 = 3;
pub const DISPLAY_VIEW_X: usize = 0x3560;
pub const DISPLAY_VIEW_Y: usize = 0x3564;
pub const DISPLAY_WIDTH: usize = 0x3548;
pub const DISPLAY_HEIGHT: usize = 0x354C;
pub const DISPLAY_HWND: usize = 0x3578;
pub const BITGRID_DISPLAY_VTABLE: u32 = 0x0066_4144;
pub const SPRITE_VTABLE: u32 = 0x0066_418C;
pub const CONSTRUCT_SPRITE: u32 = 0x004F_AA30;
pub const PROCESS_SPRITE: u32 = 0x004F_AB80;
/// GfxHandler::LoadSprites — stdcall(runtime, sprite_data, flags, gfx_dir), ESI=PaletteContext, RET 16.
/// World ctor calls it at 0x56E76A / 0x56E81C (after sprites.dat). PX InitGraphic is this window.
pub const GFX_HANDLER_LOAD_SPRITES: u32 = 0x0057_0B50;
/// DisplayGfx.palette_entries — 256×RGBA, UpdatePalette writes here.
pub const DISPLAY_PALETTE_ENTRIES: usize = 0x358D;
pub const DISPLAY_PALETTE_DIRTY_MIN: usize = 0x3D90;
pub const DISPLAY_PALETTE_DIRTY_MAX: usize = 0x3D94;
/// PaletteCommit — usercall EAX=dirty_min, EDX=dirty_max, stack=display, RET 4.
pub const PALETTE_COMMIT: u32 = 0x0056_CD20;
/// First id we try for custom sheets. Stock KnownSpriteId tops out ~683.
pub const CUSTOM_SPRITE_SLOT_MIN: i32 = 800;
/// GetSpriteFrameForBlit accepts 1..=0x3FE.
pub const CUSTOM_SPRITE_SLOT_MAX: i32 = 0x3FE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_byte_size_matches_init_memset() {
        assert_eq!(WEAPON_TABLE_BYTES, 0x80B0);
        assert_eq!(CM_SIZE, 0x264);
        assert_eq!(GAME_WORLD_COLLISION_MANAGER, 0x528);
        assert_eq!(WEAPON_DEFINED, 0x10);
        assert_eq!(WEAPON_PANEL_STATE, 0x08);
        assert_eq!(WEAPON_FIRE_TYPE, 0x30);
        assert_eq!(WEAPON_SPECIAL_SUBTYPE, 0x34);
        assert_eq!(WEAPON_CREATES_PROJECTILE, 0x20);
        assert_eq!(WEAPON_AVAILABILITY, 0x24);
        assert_eq!(STRIKE_FIRE, 0x0051E2C0);
        assert_eq!(CREATE_WEAPON_PROJECTILE, 0x0051E0F0);
        assert_eq!(SHARED_KEY_WORM, 0x02);
        assert_eq!(SHARED_KEY_TEAM, 0x15);
        assert_eq!(WORLD_ROOT_WORM_ACTIVE, 0x108);
        assert_eq!(WORLD_ROOT_TURN_TIMER, 0x18C);
        assert_eq!(GAME_WORLD_WORM_ACTIVE, 0x7234);
        assert_eq!(TEAM_CURSOR_MARK, 0x3E0);
        assert_eq!(CTX_SPAWN_X, 0x08);
        assert_eq!(WORLD_ENTITY_CTOR, 0x004FED50);
        assert_eq!(GAME_WORLD_RENDER_QUEUE, 0x524);
        assert_eq!(WORM_CURSOR_X, 0x2E0);
        assert_eq!(LUA_ACTOR_MAGIC, 0x774B4C41);
        assert_eq!(MSG_RENDER_SCENE, 3);
        assert_eq!(WEAPON_WIND_PCT, 0x74);
        assert_eq!(WEAPON_BOUNCE_PCT, 0x78);
        assert_eq!(CUSTOM_SPRITE_SENTINEL, 1023);
        assert_eq!(CUSTOM_SPRITE_SLOT_MIN, 800);
        assert_eq!(DISPLAY_LAYER_CONTEXTS, 0x311C);
        assert_eq!(DISPLAY_LAYER_VISIBILITY, 0x3530);
        assert_eq!(DISPLAY_GAME_LAYER, 3);
        assert_eq!(CONSTRUCT_SPRITE, 0x004FAA30);
        assert_eq!(PROCESS_SPRITE, 0x004FAB80);
        assert_eq!(GFX_HANDLER_LOAD_SPRITES, 0x00570B50);
        assert_eq!(GAME_RUNTIME_DISPLAY, 0x4D0);
        assert_eq!(DISPLAY_PALETTE_ENTRIES, 0x358D);
        assert_eq!(PALETTE_COMMIT, 0x0056CD20);
        assert_eq!(MISSILE_RENDER_SPRITE, 0x2EC);
        assert_eq!(MISSILE_ANIM_PHASE, 0x3E8);
        assert_eq!(LANDSCAPE_SOLID_GRID, 0x910);
        assert_eq!(GAME_WORLD_MAP_W, 0x77C0);
        assert_eq!(GAME_WORLD_VIEWPORT_W, 0x77AC);
        assert_eq!(GAME_WORLD_VIEW_CX, 0x8CEC);
        assert_eq!(GAME_WORLD_LAND_CURSOR_X, 0x7380);
        assert_eq!(WORM_START_FIRING, 0x0051B7F0);
        assert_eq!(WORM_RELEASE_WEAPON, 0x0051C010);
        assert_eq!(WORM_WEAPONS_ENABLED, 0x28C);
        assert_eq!(WORM_ACTIVE_WEAPON, 0x36C);
        assert_eq!(MSG_FIRE_WEAPON, 0x26);
        assert_eq!(MSG_RELEASE_WEAPON, 0x27);
        assert_eq!(TEAM_ARENA_AMMO, 0x1EB4);
        assert_eq!(TEAM_ARENA_DELAY, 0x1FD0);
        assert_eq!(PLAY_SOUND_LOCAL, 0x004FDFE0);
        assert_eq!(DS_SOUND_LOAD_WAV, 0x00573FF0);
        assert_eq!(CUSTOM_SOUND_SLOT_MIN, 451);
        assert_eq!(GAME_WORLD_SOUND, 0x008);
        assert_eq!(CLASS_CRATE, 15);
        assert_eq!(CLASS_MINE, 8);
        assert_eq!(ENT_CHILDREN_DATA, 0x14);
    }
}
