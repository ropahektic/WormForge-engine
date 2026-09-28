//! Pointer and size constants confirmed in Ghidra against InitWeaponTable (0x0053CAB0).

pub const IMAGE_BASE: u32 = 0x0040_0000;

pub const INIT_WEAPON_TABLE: u32 = 0x0053_CAB0;
/// `ReplayLoader(GameInfoPrefix*, mode)`, stdcall, RET 8.
pub const REPLAY_LOADER: u32 = 0x0046_2DF0;
pub const GAME_ENGINE_SHUTDOWN: u32 = 0x0056_DCD0;
pub const STR_VERSION_381: u32 = 0x0064_1C60;
pub const G_GAME_SESSION: u32 = 0x007A_0884;
/// Frontend replay-prefix singleton, named by the `push imm32` operand at
/// wkFOW's `ReferencesDDMain` site (0x004E3B40 + 0x24).
pub const G_REPLAY_PREFIX: u32 = 0x0087_D3F8;

/// Replay metadata lives in the frontend singleton named by the immediate at
/// `ReferencesDDMain + 0x24`. wkFOW calls the address 0x40 bytes after this
/// singleton DDMain, then subtracts the prefix again before using these fields.
pub const REPLAY_PREFIX_SIZE: usize = 0x40;
pub const REPLAY_ACTIVE: usize = 0xDB48;
pub const REPLAY_INPUT_PATH: usize = 0xDB60;
pub const REPLAY_OUTPUT_PATH: usize = 0xDF60;
pub const REPLAY_PATH_CAPACITY: usize = 0x400;

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
/// Per-team alliance (team colour) -- the ammo/delay ROW. Read at
/// `arena + TEAM_ARENA_ALLIANCE + team * WORLD_TEAM_STRIDE`, team 1-based.
/// This is what WA's own `TeamArena__GetAmmo` (0x5225E0) and
/// `TeamArena__AddAmmo` (0x522640) read; `WORLD_TEAM_ALLIANCE` is the same
/// field measured from GameWorld. Not OpenWA's logical-record `+0x508`:
/// writing through that put every team's ammo into one row.
pub const TEAM_ARENA_ALLIANCE: usize = 0x4;
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
/// WA 3.8.1 SetState dispatch case that enters WormEntity__setup_Drilling.
pub const WORM_STATE_DRILLING: u32 = 0x6E;
/// Worm states by the setup each `SetState` case calls (jump table 0x50F484,
/// base 0x64): 0x65 setup_Standing, 0x69 setup_PreFiring, 0x6C
/// setup_Punching, 0x6F setup_Teleporting, 0x78 setup_JetPacking. 0x7E has
/// no setup: it is the tumbling/sliding physics state WA sets on knockback,
/// on contact and when a worm walks off a ledge (`WORM_STATE_AIRBORNE`).
pub const WORM_STATE_STANDING: u32 = 0x65;
pub const WORM_STATE_PREFIRING: u32 = 0x69;
pub const WORM_STATE_PUNCHING: u32 = 0x6C;
pub const WORM_STATE_TELEPORTING: u32 = 0x6F;
pub const WORM_STATE_JETPACKING: u32 = 0x78;
/// setup_Parachuting; also entered directly by WormEntity__fire_parachute
/// (0x51A680), from TryFireWeaponSpecial and the aiming handler.
pub const WORM_STATE_PARACHUTING: u32 = 0x7D;
/// `setup_Bungeing` (SetState's table at 0x50F484): hanging from a bungee.
/// Project X's Swim Suit holds a swimming worm here (WS_BUNGEE).
pub const WORM_STATE_BUNGEE: u32 = 0x7C;
/// Drowning: WA's hurt handler enters it for water damage (kind 1); Project
/// X's WS_SINKING.
pub const WORM_STATE_SINKING: u32 = 0x7F;
/// Jet pack fuel (16.16), drained by WormEntity__handle_JetPacking by the
/// thrust / 10 each frame; 0 is unlimited, and running out sets 0x65.
pub const WORM_JET_FUEL: usize = 0x220;
/// The worm's move requests, set to 1 by WA's lockstep MoveUp / MoveDown /
/// MoveLeft / MoveRight messages and read by its movement code (the jet
/// pack's control_jetpack reads up, left and right). Project X's
/// PressedUp / PressedDown / PressedLeft / PressedRight, 0x10 lower.
pub const WORM_KEY_UP: usize = 0x1D4;
pub const WORM_KEY_DOWN: usize = 0x1D8;
pub const WORM_KEY_LEFT: usize = 0x1DC;
pub const WORM_KEY_RIGHT: usize = 0x1E0;
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
/// The name the weapon panel shows under the icons: `WeaponPanelIcons`'
/// text draw reads `[weapon_table + id*0x1D0 + 4]` (0x5680DA, 0x568145).
pub const WEAPON_NAME2: usize = 0x04;
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
/// MineEntity ctor copies fire_params[0..7] (entry+0x3C..+0x58).
/// Missiles ignore these first slots; mines use them as the whole brain.
pub const WEAPON_MINE_PROXIMITY: usize = 0x3C;
pub const WEAPON_MINE_FUSE: usize = 0x40;
pub const WEAPON_MINE_MASK: usize = 0x44;
pub const WEAPON_MINE_TRIGGER: usize = 0x48;
pub const WEAPON_MINE_BLAST: usize = 0x54;
pub const WEAPON_DAMAGE: usize = 0x50;
pub const MINE_ENTITY_CTOR: u32 = 0x0050_6660;
pub const MINE_ENTITY_DTOR: u32 = 0x0050_6AB0;
pub const MINE_PICK_SPRITE: u32 = 0x0050_6E60;
pub const MINE_EXPLODE: u32 = 0x0050_7110;
pub const MINE_PROXIMITY_SCAN: u32 = 0x0050_7140;
pub const MINE_SPRITE_ON: i32 = 0x2D;
pub const MINE_SPRITE_OFF: i32 = 0x2F;
/// Mine bounce scales (ctor writes 0x9999 here, ~60%).
pub const MINE_BOUNCE_X: usize = 0x6C;
pub const MINE_BOUNCE_Y: usize = 0x70;
pub const MINE_WIND: usize = 0x74;
/// Missile sprite id (WeaponFireParams+0x24). Bazooka=48, Grenade=50.
pub const WEAPON_SPRITE_ID: usize = 0x60;
pub const WEAPON_GRAVITY_PCT: usize = 0x70;
pub const WEAPON_WIND_PCT: usize = 0x74;
pub const WEAPON_BOUNCE_PCT: usize = 0x78;
/// Cluster slab is `WeaponFireParams.cluster_params` at entry+0x10C.
/// OpenWA mirrors primary `[3..]` (collision_radius..) into that array, so
/// primary sprite_id `[9]` is cluster_params[6], not [9].
pub const WEAPON_CLUSTER_DAMAGE: usize = 0x114;
pub const WEAPON_CLUSTER_SPRITE: usize = 0x124;
pub const WEAPON_CLUSTER_GRAVITY: usize = 0x134;
pub const WEAPON_CLUSTER_WIND: usize = 0x138;
pub const WEAPON_CLUSTER_BOUNCE: usize = 0x13C;
/// cluster_params[23] — same discriminator as primary missile_type (1=homing).
pub const WEAPON_CLUSTER_MISSILE_TYPE: usize = 0x168;
/// cluster_params[25] — primary `render_timer`. Homing burn blits this id
/// instead of cluster_params[6]; stock cluster leaves it 0 (invisible).
pub const WEAPON_CLUSTER_RENDER_TIMER: usize = 0x170;
/// cluster_params[26..30] — homing_params (strength, sprite, turn, accel, speed).
pub const WEAPON_CLUSTER_HOMING: usize = 0x174;
/// cluster_params[31] — unknown_0xc4[0]: 1=direct lock, 2=pigeon/magic dodge.
pub const WEAPON_CLUSTER_HOMING_KIND: usize = 0x188;
/// GameWorld+0x144: two frontend .img pointers per stock weapon (hi then lo).
/// WeaponPanelIcons draw: `[world + 4*(frame + 2*weapon_id) + 0x144]`.
pub const GAME_WORLD_PANEL_IMG: usize = 0x144;
/// GameWorld+0x548: the weapon panel object (`WeaponPanelIcons`). Its update
/// (0x5697B0, called with `[world+0x548]` at 0x5540CD) rebuilds the panel
/// surface via `redraw_panel` (0x568220) only while `WEAPON_PANEL_DIRTY` is
/// set, then clears it.
pub const GAME_WORLD_WEAPON_PANEL: usize = 0x548;
/// Panel dirty flag. WA sets it from `GameMessageRouter` (0x556F56 ff.) and
/// `GameRuntime__AdvanceWormFrame` (0x55DDC7) -- never from
/// `TeamArena__AddAmmo`, which is a bare ammo-table write.
pub const WEAPON_PANEL_DIRTY: usize = 0x1CC;
/// Unused stock id: GetSpriteFrameForBlit rejects `id-1 >= 0x3FE`, so 1023
/// draws nothing. Do not put custom sheets here — install into 800..=1022.
pub const CUSTOM_SPRITE_SENTINEL: i32 = 1023;
/// MissileEntity.weapon_data[9] — copy of fire-params sprite_id.
pub const MISSILE_WEAPON_SPRITE: usize = 0x180;
/// Mirror of WeaponEntry+0xAC inside the copied fire-params blob
/// (`weapon_data` base 0x15C + 0x70). Stock HM keeps hmissil1 here.
pub const MISSILE_WEAPON_ALT_SPRITE: usize = 0x1CC;
/// MissileEntity's copy of its weapon's fire params (94 dwords from the
/// constructor's `fire_params`: WeaponEntry+0x3C onward for a projectile).
/// Cluster bits are constructed from their parent's copy here.
pub const MISSILE_WEAPON_DATA: usize = 0x15C;

// Launching a missile as `FireWeapon__CreateWeaponProjectile` (0x51E0F0)
// does: check the world's object pool, allocate the entity, zero it and call
// the constructor (0x507D10) with (entity, parent, fire_params, shoot_desc).

/// `MissileEntity` size, and how much of it the launch zeroes before the
/// constructor runs (0x51E165, 0x51E171).
pub const MISSILE_ENTITY_SIZE: usize = 0x40C;
pub const MISSILE_ENTITY_ZEROED: usize = 0x3EC;
/// GameWorld's object pool counter: a missile costs 7, and none is created
/// past 700 (0x51E10C..0x51E115).
pub const GAME_WORLD_OBJECT_POOL: usize = 0x72A4;
pub const OBJECT_POOL_MISSILE: i32 = 7;
pub const OBJECT_POOL_LIMIT: i32 = 700;
/// A weapon entry's fire params: WeaponEntry+0x3C to its end.
pub const FIRE_PARAMS_SIZE: usize = WEAPON_ENTRY_SIZE - WEAPON_FIRE_PARAMS;
/// Fire params fields, from their start, by Project X's `CWeaponLaunch`
/// (pxdata.pxd): `explosion.damage`, `anim.spriteIndex`,
/// `timeBeforeExplosion` (ms).
pub const FIRE_DAMAGE: usize = 0x18;
pub const FIRE_SPRITE: usize = 0x24;
pub const FIRE_FUSE: usize = 0x50;
/// `countdownFrom`, ms: the record's countdown (5000 on the scheme's Chilli
/// Bomb, 3000 on WA's super banana); PX zeroes it for the Chilli's children.
pub const FIRE_COUNTDOWN: usize = 0x4C;
/// `explodeOnSpace`: the fire key sets it off in flight (1 on WA's super
/// banana).
pub const FIRE_EXPLODE_ON_SPACE: usize = 0x64;
/// `sound.soundIndex`: the sound the missile plays in flight (0 none).
pub const FIRE_SOUND: usize = 0x54;
/// `explodeInto`: what it bursts into, WA's `impact_spawn_kind` (entry
/// +0xF0): 0 nothing, 1 clustlets (the cluster bomb, mortar and banana
/// records; `CreateClusters`, from the record's own cluster params), 2 fire.
pub const FIRE_EXPLODE_INTO: usize = 0xB4;
/// The shoot description the constructor takes (0x2C bytes; OpenWA's
/// `WeaponReleaseContext`, Project X's `CShootDesc`).
pub const SHOOT_DESC_SIZE: usize = 0x2C;
pub const SHOOT_TEAM: usize = 0x00;
pub const SHOOT_WORM: usize = 0x04;
pub const SHOOT_X: usize = 0x08;
pub const SHOOT_Y: usize = 0x0C;
pub const SHOOT_VX: usize = 0x10;
pub const SHOOT_VY: usize = 0x14;
/// The aim cursor at release: a homing weapon's target.
pub const SHOOT_TARGET_X: usize = 0x18;
pub const SHOOT_TARGET_Y: usize = 0x1C;
/// Bounce-settle frames: WA writes 30, 60 or 0 from the worm's bounce choice.
pub const SHOOT_SETTLE: usize = 0x24;
/// Fuse in ms, used only where the fire params' own fuse is 0.
pub const SHOOT_FUSE: usize = 0x28;
/// Mirror of the fire-params `missile_type` (WeaponEntry+0xA4, blob +0x68):
/// 0 none, 1 homing, 2 standard, 3 sheep, 5 cluster. Same discriminator as
/// `WEAPON_CLUSTER_MISSILE_TYPE`. (MissileEntity's own dispatch field,
/// render_data[0x17], numbers these differently -- this is not that.)
pub const MISSILE_TYPE: usize = 0x1C4;
/// MissileEntity's copy of its shoot description (the constructor copies it
/// to +0x130), and the pellet index in it (+0x20): non-zero for a cluster
/// bit.
pub const MISSILE_SHOOT_DESC: usize = 0x130;
pub const MISSILE_SHOOT_PELLET: usize = MISSILE_SHOOT_DESC + 0x20;
/// `MissileEntity__CreateClusters` (0x5096A0): usercall, the missile in ESI,
/// (x, y) on the stack, `ret 8`. Makes the record's cluster pellets.
pub const MISSILE_CREATE_CLUSTERS: u32 = 0x0050_96A0;
pub const MISSILE_TYPE_HOMING: i32 = 1;
/// MissileEntity.fire_particle_trigger — sprite id missile render actually blits.
pub const MISSILE_RENDER_SPRITE: usize = 0x2EC;
/// Alternate sprite bank when entity+0x100 is set (render reads +0x338).
pub const MISSILE_ALT_BANK_SPRITE: usize = 0x338;
/// Used when entity+0x3A4 is set instead of the normal render sprite.
pub const MISSILE_RENDER_SPRITE_ALT: usize = 0x368;
/// Homing-mode blink pair (stock reads these while entity+0x394 == 1).
pub const MISSILE_HOMING_SPRITE_A: usize = 0x36C;
pub const MISSILE_HOMING_SPRITE_B: usize = 0x370;
/// Homing physics/render mode: 1 = lock engaged (burn blit path).
pub const MISSILE_HOMING_MODE: usize = 0x394;
/// MissileEntity.animation_phase — GetSpriteFrameForBlit anim_value.
pub const MISSILE_ANIM_PHASE: usize = 0x3E8;

pub const APPLY_PROJECTILE_GRAVITY: u32 = 0x004F_CB30;
pub const MISSILE_HANDLE_MESSAGE: u32 = 0x0050_B400;
/// `MissileEntity__Free`, vtable slot 1: thiscall (this, flags), RET 4. The
/// destructor 0x5086F0 has no other caller, so every missile ends here.
pub const MISSILE_FREE: u32 = 0x0050_8330;
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
/// WormEntity turn side (+0x1A8): -1 left, +1 right. The face-left/right
/// messages write it at once, and WA's fire code reads it to place a shot
/// (`facing * 26px`). `WORM_FACING` is a copy that can still hold the old
/// side when a worm turns and fires in the same aim (OpenWA
/// `facing_direction_2`; PX's `TurnSide`).
pub const WORM_TURN_SIDE: usize = 0x1A8;
/// WormEntity show-cursor latch (`+0x268`). Strike targeting.
pub const WORM_SHOW_CURSOR: usize = 0x268;
/// WormEntity rendered aim sweep (+0x270).
pub const WORM_AIM_ANGLE: usize = 0x270;
/// WorldRootEntity: non-zero pauses the HUD turn clock (grenade-in-flight).
pub const WORLD_ROOT_WORM_ACTIVE: usize = 0x108;
pub const WORLD_ROOT_TURN_TIMER_DISPLAY: usize = 0x188;
pub const WORLD_ROOT_TURN_TIMER: usize = 0x18C;
pub const WORLD_ROOT_TURN_ENDED: usize = 0x150;
/// Retreat countdown in ms: set from the scheme when retreat starts, 0 before.
pub const WORLD_ROOT_RETREAT_TIMER: usize = 0x178;
/// GameWorld copy of WorldRoot `worm_active`.
pub const GAME_WORLD_WORM_ACTIVE: usize = 0x7234;
pub const SHARED_KEY_TEAM: u32 = 0x15;
/// `TeamEntity__reset_cursor` — thiscall, clears planted strike X.
pub const TEAM_RESET_CURSOR: u32 = 0x0055_6D00;
/// `TeamEntity` land-cursor blit (markerr / bounce aim). EAX=this. Skip like worm DrawSprite.
pub const TEAM_DRAW_CURSOR: u32 = 0x0055_6AD0;
/// `TeamEntity` vtable message handler (`0x669eec` slot → `FUN_00557310`).
/// thiscall(this, a, msg, b, data). Msg `0x32` = lockstep land-cursor click:
/// plants X (`+0x400/404/408`), calls reset_cursor, forwards to the worm
/// (which runs FireWeapon → launch anim + turn end). Eat here for cursor packs.
pub const TEAM_MSG_HANDLER: u32 = 0x0055_7310;
pub const TEAM_MSG_CURSOR_CLICK: u32 = 0x32;
/// TeamEntity index (`+0x38`); `data[0]` of every team message.
pub const TEAM_INDEX: usize = 0x38;
/// TeamEntity: the machine that controls the team, copied from its GameInfo
/// team record's `owner_player_slot` (0 the host, 1.. the clients; negative
/// for a computer team).
pub const TEAM_OWNER_MACHINE: usize = 0x40;
/// GameWorld's GameInfo, and in it this machine's index (`local_team_index`,
/// set per machine by `InitTeamsFromLobby`: 0 the host, 1.. the clients).
pub const GAME_WORLD_GAME_INFO: usize = 0x24;
pub const GAME_INFO_LOCAL_MACHINE: usize = 0xD9DC;
/// GameInfo: the match's gravity, 16.16, from the scheme. Every frame
/// `TurnManager__ProcessFrame` (0x55FDA0, at 0x5603CF) sets the collision
/// manager's gravity to it times `GAME_WORLD_GRAVITY_PCT` / 100.
pub const GAME_INFO_GRAVITY: usize = 0xD9A8;
/// GameWorld: the gravity percentage that recompute uses: 50 while a team's
/// low gravity is on, else 100.
pub const GAME_WORLD_GRAVITY_PCT: usize = 0x5F4;
/// TeamEntity selected weapon slot (`+0x60`).
pub const TEAM_SELECTED_WEAPON: usize = 0x60;
/// TeamEntity colour index (`+0x50`). Stock land cursor sprite is
/// `colour + 0x20` (`cursorr`..`cursorc`, ids 32..=37); planted X is `+0x26`.
pub const TEAM_COLOUR: usize = 0x50;
pub const TEAM_CURSOR_MOVE: usize = 0x3D0;
/// Live cursor anim phase (`+0x3D4`). `show_cursor` resets it to 0 and eases
/// it to `TEAM_CURSOR_PHASE_END` (20 % of the gap per frame, min step 0x400).
pub const TEAM_CURSOR_SPRITE: usize = 0x3D4;
pub const TEAM_CURSOR_PHASE_END: i32 = 0x9FFB;
pub const TEAM_CURSOR_MARK: usize = 0x3E0;
pub const STRIKE_FIRE: u32 = 0x0051_E2C0;
/// `FireWeapon` type switch (EAX=weapon entry, stack worm, ECX=ctx). RET 4.
/// After StrikeFire it sets `worm+0x3C` (ENT_ACTIVE) — click-only packs must eat here.
pub const FIRE_WEAPON_DISPATCH: u32 = 0x0051_EE60;
pub const CREATE_WEAPON_PROJECTILE: u32 = 0x0051_E0F0;
pub const WORLD_ENTITY_CTOR: u32 = 0x004F_ED50;
pub const WORLD_ENTITY_FREE: u32 = 0x004F_EF10;
pub const WORLD_ENTITY_TRY_MOVE: u32 = 0x004F_E070;
pub const WORLD_ENTITY_VTABLE: u32 = 0x0066_41F8;
pub const WA_CRT_MALLOC: u32 = 0x005C_0AB8;
pub const WA_MALLOC: u32 = 0x005C_0AE3;
pub const CREATE_EXPLOSION: u32 = 0x0054_8080;
/// WorldEntity explosion knock (EDI=this, stdcall damage/radius/x/y). VA 0x004FF390.
pub const APPLY_EXPLOSION_KNOCK: u32 = 0x004F_F390;
pub const RQ_DRAW_SPRITE_LOCAL: u32 = 0x0054_2060;
pub const WORLD_ENTITY_VTABLE_SLOTS: usize = 32;
pub const STUB_RETURNS0: u32 = 0x0054_5780;
pub const STUB_RETURNS1: u32 = 0x0059_2A50;
pub const STUB_NOOP: u32 = 0x004A_A060;
pub const SET_TEXTBOX_TEXT: u32 = 0x004F_B070;
/// Textbox: its style row (0 small, 1 standard, 2 medium fonts), which
/// `SetTextboxText` multiplies by 10 into its font table (0x6A9020) with the
/// call's font index; and its cached scale, -1 to make the next call redraw
/// (the cache compares the text, font index, colours and scale, not the style).
pub const TEXTBOX_STYLE: usize = 0x8;
pub const TEXTBOX_CACHED_SCALE: usize = 0x130;
/// The small fonts' style row: font index 6 is `smlwht1.fnt`, 7 pixels high,
/// half the HUD's (the world textbox is style 2, `medwht1.fnt`).
pub const TEXTBOX_STYLE_SMALL: i32 = 0;
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
/// Knock mass for CreateExplosion (WorldEntity+0xCC). 0 = no blast throw.
pub const ENT_BLAST_MASS: usize = 0xCC;
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
/// `DamageWorms` — subtract HP, never kills (floors at 1).
pub const MSG_NONLETHAL: u32 = 0x3B;
/// `ApplyPoison` tick.
pub const MSG_POISON: u32 = 0x3E;
pub const MSG_ENABLE_WEAPONS: u32 = 0x45;
/// Bat / prod / drill box (`SpecialImpact`).
pub const MSG_HIT: u32 = 0x4B;
/// `CommitPendingHealth` — floating HP numbers after momentum ends.
pub const MSG_COMMIT_HEALTH: u32 = 0x44;
/// Bazooka-style blast (`Explosion`).
pub const MSG_EXPLOSION: u32 = 0x1C;
/// SetWind: WA's wind change (RandomiseWind), data[0] the wind, 16.16.
pub const MSG_SET_WIND: u32 = 0x54;
/// The shared-data key WA's wind change sends it to.
pub const SHARED_KEY_WIND: u32 = 0x14;
/// Shot contact (`ProjectileImpact`) — same payload as explosion.
pub const MSG_IMPACT: u32 = 0x76;

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
/// Per-tick poison amount. ApplyPoison (msg 0x3E) subtracts this, floor 1 HP.
pub const WORM_POISON: usize = 0x148;
pub const ARENA_WORM_HP: usize = 0x40EC;
/// Team alliance id. Index `world + 0x462C + team * 0x51C`.
pub const WORLD_TEAM_ALLIANCE: usize = 0x462C;
/// ExplosionMessage / SpecialImpactMessage.damage
pub const BLAST_AMOUNT: usize = 0x10;
pub const BLAST_TEAM: usize = 0x18;
pub const HIT_AMOUNT: usize = 0x14;
pub const HIT_TEAM: usize = 0x18;
/// SpecialImpact `flag`. `10` = percent of current HP (battle axe).
pub const HIT_FLAG: usize = 0x00;
pub const HIT_PERCENT: i32 = 10;
pub const WORLD_TEAM_STRIDE: usize = 0x51C;
pub const ARENA_WORM_STRIDE: usize = 0x9C;
pub const MAX_TEAMS: u32 = 6;
pub const MAX_WORMS_PER_TEAM: u32 = 8;
pub const GAME_RUNTIME_WORLD_ROOT: usize = 0x8;
pub const COLLISION_LAYER_MISSILE: u32 = 0xF;
pub const DROP_SKY_Y: i32 = -0x00D0_0000;
pub const LUA_ACTOR_MAGIC: u32 = 0x774B_4C41;
/// Grown to include presentation state and the stable actor sequence id.
/// The allocation is ours (`spawn_lua_actor` mallocs and zeroes it).
pub const LUA_ACTOR_SIZE: usize = 0x154;
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
/// Use first-frame opaque GIF/PNG silhouette for collision probes.
pub const ACTOR_FLAG_MASK: i32 = 8192;
/// Mask blocks / supports worms.
pub const ACTOR_FLAG_MASK_WORMS: i32 = 16384;
/// Mask vs landscape (stand on dirt / push out of soil).
pub const ACTOR_FLAG_MASK_LAND: i32 = 32768;
/// Mask bounces stock missiles / grenades.
pub const ACTOR_FLAG_MASK_WEAPONS: i32 = 65536;
/// Mask separates from other `mask_actors` LuaActors.
pub const ACTOR_FLAG_MASK_ACTORS: i32 = 131072;
/// `wa.cursor.spawn`: follow `world::pointer()` every frame; stock cursor
/// sheets (32..=37) play WA's ease-in instead of looping.
pub const ACTOR_FLAG_CURSOR: i32 = 262144;
/// `RQ_DrawSpriteLocal` sprite-id flag: flip X.
pub const SPRITE_MIRROR_X: u32 = 0x0004_0000;
/// Sprite-id flag: play the sheet backwards. `DisplayGfx__BlitSprite`
/// (0x56B080) draws such a sprite at phase `0x10000 - phase`. A worm puts a
/// weapon away by playing its draw sheet with this flag.
pub const SPRITE_REVERSE: u32 = 0x0080_0000;
pub const ACTOR_FEET_X0: usize = 0x12C;
pub const ACTOR_FEET_X1: usize = 0x130;
pub const ACTOR_FEET_Y: usize = 0x134;
pub const ACTOR_CONTACT: usize = 0x138;
/// Lua brain id (`0` = spawn-table sugar). Last dword of `LUA_ACTOR_SIZE`.
pub const ACTOR_BRAIN: usize = 0x13C;
pub const ACTOR_ANGLE: usize = 0x140;
pub const ACTOR_SCALE: usize = 0x144;
pub const ACTOR_TINT: usize = 0x148;
pub const ACTOR_DEPTH: usize = 0x14C;
/// Sequential actor id, assigned at spawn and reset each match.
///
/// `a.id` used to be the entity pointer. A heap address is not an identity:
/// it differs between peers and between runs, so a pack comparing ids, or
/// iterating a table keyed by them, got a different answer on each machine.
/// Lua hashes integers without a per-process seed, so a table keyed by this
/// also iterates in a reproducible order, which an address-keyed one does not.
pub const ACTOR_SEQ: usize = 0x150;
pub const SHARED_WORLD_ROOT: u32 = 0x14;
pub const GAME_WORLD_LANDSCAPE: usize = 0x20;
/// Determinism probes. WA is a lockstep engine and these are the words that
/// prove two network peers agree; a divergence appears here first.
/// Ported from the OpenWA catalog (`re/GameWorld.toml`), same 3.8.1 build.
/// Cross-checked before trusting: both catalogs agree on struct size 0x98D8
/// and on landscape 0x20, weapon_table 0x510, team_arena 0x4628,
/// water_level 0x5E0 and level_width 0x77C0 -- so the base pointer is the
/// same object and these land on the intended words.
///
/// `GameWorld.frame_counter` — incremented at the end of each frame in
/// GameFrameDispatcher. WA keeps its own sim clock, so the transport does not
/// need to invent one.
/// `GameWorld__AdvanceRNG` — `__fastcall`, `this` in ECX, new value in EAX.
///
/// Six instructions, verified byte-for-byte in 3.8.1's WA.exe:
/// ```text
///   mov  eax, [ecx + 0x5cc]      ; frame_counter
///   add  eax, [ecx + 0x45ec]     ; + game_rng
///   imul eax, eax, 0x19660d
///   add  eax, 0x3c6ef35f
///   mov  [ecx + 0x45ec], eax
///   ret
/// ```
/// That disassembly is also what confirms `GAME_WORLD_FRAME_COUNTER` and
/// `GAME_WORLD_GAME_RNG` below: both appear in it as literal displacements.
pub const GAME_WORLD_ADVANCE_RNG: u32 = 0x0053_F320;
/// First six bytes of the above, checked before we ever call it.
pub const ADVANCE_RNG_PROLOGUE: [u8; 6] = [0x8b, 0x81, 0xcc, 0x05, 0x00, 0x00];

/// `GameWorld.net_session` — the per-match peer session. This, rather than the
/// adjacent `net_game`, is WA's online/offline discriminator.
pub const GAME_WORLD_NET_SESSION: usize = 0x1C;
/// `GameWorld.net_game` — a separate network game object passed to the world
/// constructor. It can be null during an online round and must not be used as
/// the online/offline discriminator.
pub const GAME_WORLD_NET_GAME: usize = 0x28;
pub const GAME_WORLD_FRAME_COUNTER: usize = 0x5CC;
/// `GameWorld.game_rng` — `rng = (frame_counter + rng) * 0x19660D + 0x3C6EF35F`.
///
/// ONE shared RNG for gameplay and visual effects alike. OpenWA's desync
/// journal turned on exactly this: a skipped `debris.spr` load changed how
/// often `GenerateDebrisParticles` ticked this word (15,470 writes vs 17,106)
/// and the whole simulation drifted. Treat it as the canary.
pub const GAME_WORLD_GAME_RNG: usize = 0x45EC;
/// Client presentation RNG (`GameTask__fxrandom`). Used by score bubbles,
/// smoke, sea bubbles, and similar effects; it need not match across peers.
pub const GAME_WORLD_RNG_STATE_1: usize = 0x72EC;
/// Network RNG (`GameTask__netrandom`). This is part of lockstep state.
pub const GAME_WORLD_RNG_STATE_2: usize = 0x72F0;
/// `GameWorld.recorded_frame_counter` — -1 while a replay is playing back.
/// This is WA's own equivalent of Project X's `IsReplay()`.
pub const GAME_WORLD_RECORDED_FRAME: usize = 0x7EF0;
pub const GAME_WORLD_WATER_LEVEL: usize = 0x5E0;
pub const GAME_WORLD_WATER_KILL_Y: usize = 0x5E4;
/// `GameWorld.is_cavern` — nonzero on a cavern level. Set by
/// `GameRuntime__InitLandscapeBorders` (0x528480) alongside the indestructible
/// borders, so it is the flag WA itself acts on (clouds, bounds, super-weapon
/// rules). Offset and meaning from OpenWA's GameWorld catalog.
pub const GAME_WORLD_IS_CAVERN: usize = 0x777C;
/// Map pixel size (GameWorld). Blas / BitGrid fallback.
pub const GAME_WORLD_MAP_W: usize = 0x77C0;
/// GameWorld.viewport_pixel_width / height (GameRender).
pub const GAME_WORLD_VIEWPORT_W: usize = 0x77AC;
pub const GAME_WORLD_VIEWPORT_H: usize = 0x77B0;
/// `viewport_coords[3]` camera centre (Fixed 16.16). Entry starts at
/// `GameWorld+0x8CEC` with `center_x` then `center_y` (openWA CoordEntry;
/// worm draw compares pos.x to `+0x8CEC` as midpoint X). A prior swap to
/// "Y then X" broke `wa.world.pointer()` (displaced / axis-scrambled cursor).
pub const GAME_WORLD_VIEW_CX: usize = 0x8CEC;
pub const GAME_WORLD_VIEW_CY: usize = 0x8CF0;
/// GameWorld land targeting cursor (16.16). Teleport / girder / strike marker
/// written by `TeamEntity__process_cursor` (wkAimbot `sample_aim_cursor`).
/// openWA mislabels these as `viewport_width/height`; do not treat as pixels.
pub const GAME_WORLD_LAND_CURSOR_X: usize = 0x7380;
pub const GAME_WORLD_LAND_CURSOR_Y: usize = 0x7384;
pub const GAME_WORLD_LAND_CURSOR_X_LAST: usize = 0x7388;
pub const GAME_WORLD_LAND_CURSOR_Y_LAST: usize = 0x738C;
/// WorldRootEntity active team, 1-based.
pub const WORLD_ROOT_CURRENT_TEAM: usize = 0x12C;
pub const SPRITE_CURSOR_R: i32 = 32;
pub const SPRITE_MARKER_R: i32 = 38;
pub const GAME_WORLD_MAP_H: usize = 0x77C4;
/// Plot scratch BitGrid (Landscape+0x0D0). Not walk collision. See FUN_0057d040.
pub const LANDSCAPE_PLOT_SCRATCH: usize = 0x0D0;
/// Locomotion collision BitGrid (Landscape+0x914). Walk/jump. Ghidra FUN_0057cc90.
pub const LANDSCAPE_COLLISION: usize = 0x914;
/// 8bpp fill / picture. Soil-coloured mass can remain after a blast. Not walk.
pub const LANDSCAPE_TERRAIN_IMAGE: usize = 0x910;
/// Destruct / soil overlay paint. Walk in front of it.
pub const LANDSCAPE_SOIL: usize = 0x91C;
/// WA 3.8.1 Landscape vtable and its terrain-cutting entries.
pub const LANDSCAPE_VTABLE: u32 = 0x0066_B208;
pub const LANDSCAPE_APPLY_EXPLOSION: u32 = 0x0057_C820;
pub const LANDSCAPE_MASK: u32 = 0x0057_CCF0;
pub const LANDSCAPE_SHAPES: usize = 0x88;
/// Set by Landscape::SetControlFlag for indestructible terrain.
pub const LANDSCAPE_CONTROL_FLAG: usize = 0xB3C;
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
/// WA CrossEntity, the gravestone left by a dead worm.
pub const CLASS_GRAVE: i32 = 21;
/// WormEntity::Constructor(this, team parent, team, worm, setup), WA 3.8.1.
pub const WORM_ENTITY_CTOR: u32 = 0x0050_BFB0;
/// The landscape (ClassType 28, 0x1C). It takes part in WA's entity
/// contacts, so a worm touching down reaches `wa.on.contact` with it.
pub const CLASS_LAND: i32 = 28;
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
/// PaletteContext__MapColor — thiscall(this=PaletteContext, rgb), RET 4.
/// Exact cache hit or pop free list; returns 0xFFFFFFFF on overflow.
pub const MAP_COLOR: u32 = 0x0054_12B0;
/// PaletteContext__NearestColor — usercall EDI=PaletteContext, stdcall(rgb, *quality) RET 8.
/// Same routine PX falls through to on WA 3.6.31 (`0x510550`); searches MapColor cache only.
pub const NEAREST_COLOR: u32 = 0x0054_1340;
/// Apply palette remap lut in-place to sprite pixels (ProcessSprite's post-MapColor step).
/// stdcall(pixels, pitch_arg, remap, dword_width, rows) RET 0x14.
pub const APPLY_SPRITE_REMAP: u32 = 0x005B_2BEB;
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
        assert_eq!(FIRE_WEAPON_DISPATCH, 0x0051_EE60);
        assert_eq!(CREATE_WEAPON_PROJECTILE, 0x0051E0F0);
        assert_eq!(SHARED_KEY_WORM, 0x02);
        assert_eq!(SHARED_KEY_TEAM, 0x15);
        assert_eq!(WORLD_ROOT_WORM_ACTIVE, 0x108);
        assert_eq!(WORLD_ROOT_TURN_TIMER, 0x18C);
        assert_eq!(GAME_WORLD_WORM_ACTIVE, 0x7234);
        assert_eq!(TEAM_CURSOR_MARK, 0x3E0);
        assert_eq!(TEAM_DRAW_CURSOR, 0x0055_6AD0);
        assert_eq!(TEAM_RESET_CURSOR, 0x0055_6D00);
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
        assert_eq!(MAP_COLOR, 0x005412B0);
        assert_eq!(NEAREST_COLOR, 0x00541340);
        assert_eq!(APPLY_SPRITE_REMAP, 0x005B2BEB);
        assert_eq!(GFX_HANDLER_LOAD_SPRITES, 0x00570B50);
        assert_eq!(G_REPLAY_PREFIX, 0x0087D3F8);
        assert_eq!(GAME_RUNTIME_DISPLAY, 0x4D0);
        assert_eq!(DISPLAY_PALETTE_ENTRIES, 0x358D);
        assert_eq!(PALETTE_COMMIT, 0x0056CD20);
        assert_eq!(MISSILE_RENDER_SPRITE, 0x2EC);
        // 0x507DFC: rep movsd of 0xB dwords to [ebp+0x130]; 0x507E51 tests
        // the shoot description's +0x20 to pick the cluster half.
        assert_eq!(MISSILE_SHOOT_PELLET, 0x150);
        assert_eq!(MISSILE_WEAPON_ALT_SPRITE, 0x1CC);
        // Both mirror the same fire-params blob, copied to weapon_data at 0x15C
        // from WeaponEntry+0x3C.
        assert_eq!(MISSILE_TYPE, 0x15C + (0xA4 - 0x3C));
        assert_eq!(MISSILE_WEAPON_ALT_SPRITE, 0x15C + (0xAC - 0x3C));
        assert_eq!(MISSILE_ALT_BANK_SPRITE, 0x338);
        assert_eq!(MISSILE_RENDER_SPRITE_ALT, 0x368);
        assert_eq!(MISSILE_HOMING_SPRITE_A, 0x36C);
        assert_eq!(MISSILE_HOMING_SPRITE_B, 0x370);
        assert_eq!(MISSILE_HOMING_MODE, 0x394);
        assert_eq!(MISSILE_ANIM_PHASE, 0x3E8);
        assert_eq!(LANDSCAPE_PLOT_SCRATCH, 0x0D0);
        assert_eq!(LANDSCAPE_COLLISION, 0x914);
        assert_eq!(LANDSCAPE_SOIL, 0x91C);
        assert_eq!(LANDSCAPE_VTABLE, 0x0066_B208);
        assert_eq!(LANDSCAPE_APPLY_EXPLOSION, 0x0057_C820);
        assert_eq!(LANDSCAPE_MASK, 0x0057_CCF0);
        assert_eq!(LANDSCAPE_SHAPES, 0x88);
        assert_eq!(LANDSCAPE_CONTROL_FLAG, 0xB3C);
        assert_eq!(WORM_STATE_DRILLING, 0x6E);
        assert_eq!(GAME_WORLD_MAP_W, 0x77C0);
        assert_eq!(GAME_WORLD_IS_CAVERN, 0x777C);
        assert_eq!(GAME_WORLD_VIEWPORT_W, 0x77AC);
        assert_eq!(GAME_WORLD_VIEW_CX, 0x8CEC);
        assert_eq!(GAME_WORLD_VIEW_CY, 0x8CF0);
        assert_eq!(GAME_WORLD_LAND_CURSOR_X, 0x7380);
        assert_eq!(WORM_START_FIRING, 0x0051B7F0);
        assert_eq!(WORM_RELEASE_WEAPON, 0x0051C010);
        assert_eq!(WORM_WEAPONS_ENABLED, 0x28C);
        assert_eq!(WORM_ACTIVE_WEAPON, 0x36C);
        assert_eq!(MSG_FIRE_WEAPON, 0x26);
        assert_eq!(MSG_RELEASE_WEAPON, 0x27);
        assert_eq!(MSG_EXPLOSION, 0x1C);
        assert_eq!(MSG_IMPACT, 0x76);
        assert_eq!(MSG_NONLETHAL, 0x3B);
        assert_eq!(MSG_POISON, 0x3E);
        assert_eq!(MSG_HIT, 0x4B);
        assert_eq!(HIT_FLAG, 0x00);
        assert_eq!(HIT_PERCENT, 10);
        assert_eq!(WORM_POISON, 0x148);
        assert_eq!(TEAM_ARENA_AMMO, 0x1EB4);
        assert_eq!(TEAM_ARENA_DELAY, 0x1FD0);
        // Two names for one field; they must never drift apart.
        assert_eq!(GAME_WORLD_TEAM_ARENA + TEAM_ARENA_ALLIANCE, WORLD_TEAM_ALLIANCE);
        // Delays sit right after the 71 ammo counts of each row.
        assert_eq!(TEAM_ARENA_DELAY, TEAM_ARENA_AMMO + 71 * 4);
        assert_eq!(PLAY_SOUND_LOCAL, 0x004FDFE0);
        assert_eq!(DS_SOUND_LOAD_WAV, 0x00573FF0);
        assert_eq!(CUSTOM_SOUND_SLOT_MIN, 451);
        assert_eq!(GAME_WORLD_SOUND, 0x008);
        assert_eq!(CLASS_CRATE, 15);
        assert_eq!(CLASS_MINE, 8);
        assert_eq!(ENT_CHILDREN_DATA, 0x14);
        // Determinism probes, ported from OpenWA's 3.8.1 catalog.
        assert_eq!(ACTOR_SEQ, 0x150);
        // The sequence id must fit inside the allocation.
        assert!(ACTOR_SEQ + 4 <= LUA_ACTOR_SIZE);
        assert_eq!(GAME_WORLD_NET_SESSION, 0x1C);
        assert_eq!(GAME_WORLD_NET_GAME, 0x28);
        assert_eq!(GAME_WORLD_ADVANCE_RNG, 0x0053_F320);
        // The prologue must load frame_counter, or the offsets disagree with
        // the code that uses them.
        assert_eq!(
            u32::from_le_bytes([
                ADVANCE_RNG_PROLOGUE[2],
                ADVANCE_RNG_PROLOGUE[3],
                ADVANCE_RNG_PROLOGUE[4],
                ADVANCE_RNG_PROLOGUE[5],
            ]) as usize,
            GAME_WORLD_FRAME_COUNTER
        );
        assert_eq!(GAME_WORLD_FRAME_COUNTER, 0x5CC);
        assert_eq!(GAME_WORLD_GAME_RNG, 0x45EC);
        assert_eq!(GAME_WORLD_RNG_STATE_1, 0x72EC);
        assert_eq!(GAME_WORLD_RNG_STATE_2, 0x72F0);
        assert_eq!(GAME_WORLD_RECORDED_FRAME, 0x7EF0);
        assert!(GAME_WORLD_RECORDED_FRAME < 0x98D8);
    }
}
