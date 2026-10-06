//! One static GPU-only preview per weapon, rebuilt each match.
use game::offsets::*;
use gfx::{RgbaFrame, SpriteStrip};
use lua_host::WeaponVisual;
use std::sync::Mutex;
use weapons::{OverlayClaim, PanelIcon};
static VISUALS: Mutex<Vec<(u32, WeaponVisual)>> = Mutex::new(Vec::new());
pub fn clear() {
    VISUALS.lock().unwrap().clear();
}
pub fn visual(slot: u32) -> Option<WeaponVisual> {
    let v = VISUALS
        .lock()
        .ok()?
        .iter()
        .find(|(s, _)| *s == slot)?
        .1
        .clone();
    crate::sprite::lookup_key(&v.sprite)
        .filter(|s| crate::sprite::is_virtual(s.id))
        .map(|_| v)
}
fn strip(frame: RgbaFrame) -> SpriteStrip {
    SpriteStrip {
        frames: vec![frame],
        fps: 1,
        looping: false,
    }
}
/// Crop transparent borders before fitting into the user's preview box.
fn crop(frame: RgbaFrame) -> Option<RgbaFrame> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if w == 0 || h == 0 || frame.rgba.len() != w * h * 4 {
        return None;
    }
    let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if frame.rgba[(y * w + x) * 4 + 3] != 0 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    if right <= left || bottom <= top {
        return None;
    }
    let mut rgba = Vec::new();
    for y in top..bottom {
        rgba.extend_from_slice(&frame.rgba[(y * w + left) * 4..(y * w + right) * 4]);
    }
    Some(RgbaFrame {
        width: (right - left) as u32,
        height: (bottom - top) as u32,
        rgba,
    })
}
/// Panel artwork uses opaque black as its background as well as index 0.
/// Key black to alpha only in the popup copy, before trimming that border.
fn transparent_icon(mut frame: RgbaFrame) -> RgbaFrame {
    for pixel in frame.rgba.chunks_exact_mut(4) {
        if pixel[..3] == [0, 0, 0] {
            pixel[3] = 0;
        }
    }
    frame
}

/// Called before atlas packing, using the world from InitWeaponTable's
/// runtime argument. Its global session pointer need not be ready yet.
/// Copies panel art; never writes WA's weapon table, icon grids or palette.
pub unsafe fn prepare(
    world: *mut u8,
    display: *mut u8,
    claims: &[OverlayClaim],
) -> Vec<(String, SpriteStrip)> {
    clear();
    if world.is_null() || display.is_null() {
        crate::log_line(&format!(
            "collect_viewer preview: unavailable world={world:p} display={display:p}"
        ));
        return Vec::new();
    }
    let mut copies = Vec::new();
    for slot in 1..STOCK_WEAPON_COUNT as u32 {
        let claim = claims.iter().find(|c| c.slot.index() == slot as usize);
        let icon = || -> Option<RgbaFrame> {
            match claim.and_then(|c| c.params.panel_icon.as_ref()) {
                Some(PanelIcon::File {
                    width: 32,
                    height: 32,
                    rgba,
                    ..
                }) => Some(RgbaFrame {
                    width: 24,
                    height: 24,
                    rgba: gfx::icon::crop(rgba, 32, 4, 4, 24, 24),
                }),
                Some(PanelIcon::File {
                    width,
                    height,
                    rgba,
                    ..
                }) => Some(RgbaFrame {
                    width: *width,
                    height: *height,
                    rgba: rgba.clone(),
                }),
                Some(PanelIcon::Stock(s)) => unsafe {
                    crate::panel_icon::preview_frame(world, s.index())
                },
                None => unsafe { crate::panel_icon::preview_frame(world, slot as usize) },
            }
            .map(transparent_icon)
            .and_then(crop)
        };
        let Some(frame) = icon() else {
            continue;
        };
        let key = format!("wa-preview:{slot}");
        VISUALS.lock().unwrap().push((
            slot,
            WeaponVisual {
                sprite: key.clone(),
                frame: 0,
                width: frame.width,
                height: frame.height,
            },
        ));
        copies.push((key, strip(frame)));
    }
    let visuals = VISUALS.lock().unwrap();
    crate::log_line(&format!(
        "collect_viewer preview: prepared={} icons missing={} world={world:p} display={display:p}",
        visuals.len(),
        STOCK_WEAPON_COUNT - 1 - visuals.len(),
    ));
    copies
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previews_use_panel_symbols_for_projectiles_and_equipment_without_wa_slots() {
        use game::entity::write_i32;
        use weapons::{OverlayParams, StockWeapon};
        let mut world = vec![0u32; 0x1000 / 4];
        let mut display = vec![0u32; 0x4000 / 4];
        let mut palette = vec![0u8; 0x404];
        palette[8..11].copy_from_slice(&[10, 20, 30]);
        // Nonzero palette index 2 is opaque black in stock panel artwork.
        let mut pixels = vec![2u8; 32 * 32];
        for y in 9..14 {
            for x in 10..18 {
                pixels[y * 32 + x] = 1;
            }
        }
        let mut grid = vec![0u32; 11];
        grid[2] = pixels.as_mut_ptr() as u32;
        grid[4] = 32;
        grid[5] = 32;
        grid[6] = 32;
        let w = world.as_mut_ptr() as *mut u8;
        let d = display.as_mut_ptr() as *mut u8;
        unsafe {
            write_i32(w, GAME_WORLD_DISPLAY, d as i32);
            write_i32(d, DISPLAY_LAYER_CONTEXTS + 4, palette.as_mut_ptr() as i32);
            write_i32(
                w,
                GAME_WORLD_PANEL_IMG + (22 * 2 + 1) * 4,
                grid.as_mut_ptr() as i32,
            );
        }
        let frame = |width, height, colour: [u8; 4]| RgbaFrame {
            width,
            height,
            rgba: colour.repeat((width * height) as usize),
        };
        let custom = SpriteStrip {
            frames: vec![frame(2, 2, [255, 0, 0, 255]), frame(6, 3, [0, 0, 255, 255])],
            fps: 10,
            looping: true,
        };
        let mut icon_rgba = [0, 255, 0, 255].repeat(64);
        icon_rgba[..4].copy_from_slice(&[0, 0, 0, 255]);
        icon_rgba[4..8].copy_from_slice(&[0, 1, 0, 255]); // near-black art stays opaque
        let claims = vec![
            OverlayClaim {
                mod_id: "projectile-pack".into(),
                logical_id: None,
                slot: StockWeapon::Prod,
                params: OverlayParams {
                    sprite_sheet: Some(custom.clone()),
                    panel_icon: Some(PanelIcon::Stock(StockWeapon::Mine)),
                    ..Default::default()
                },
            },
            OverlayClaim {
                mod_id: "equipment-pack".into(),
                logical_id: None,
                slot: StockWeapon::JetPack,
                params: OverlayParams {
                    sprite_sheet: Some(custom.clone()),
                    panel_icon: Some(PanelIcon::Stock(StockWeapon::Mine)),
                    ..Default::default()
                },
            },
            OverlayClaim {
                mod_id: "icon-pack".into(),
                logical_id: None,
                slot: StockWeapon::Grenade,
                params: OverlayParams {
                    sprite_sheet: Some(custom),
                    panel_icon: Some(PanelIcon::File {
                        path: "icon.png".into(),
                        width: 8,
                        height: 8,
                        rgba: icon_rgba.clone(),
                    }),
                    ..Default::default()
                },
            },
        ];
        let slots = crate::sprite::slots_used();
        let sheets = unsafe { prepare(w, d, &claims) };
        assert_eq!(crate::sprite::slots_used(), slots);
        let get = |slot| {
            &sheets
                .iter()
                .find(|(key, _)| key == &format!("wa-preview:{slot}"))
                .unwrap()
                .1
                .frames[0]
        };
        assert_eq!(get(20).rgba, [10, 20, 30, 255].repeat(40));
        assert_eq!((get(62).width, get(62).height), (8, 5));
        assert_eq!(get(62).rgba, [10, 20, 30, 255].repeat(40));
        icon_rgba[3] = 0;
        assert_eq!(get(6).rgba, icon_rgba);
    }

    #[test]
    fn opaque_crop_preserves_shape_and_discards_blank_frames() {
        let mut rgba = vec![0; 8 * 6 * 4];
        for y in 2..4 {
            for x in 1..6 {
                rgba[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].copy_from_slice(&[12, 34, 56, 255]);
            }
        }
        let cropped = crop(RgbaFrame {
            width: 8,
            height: 6,
            rgba,
        })
        .unwrap();
        assert_eq!((cropped.width, cropped.height), (5, 2));
        assert_eq!(cropped.rgba, [12, 34, 56, 255].repeat(10));
        assert!(crop(RgbaFrame {
            width: 2,
            height: 2,
            rgba: vec![0; 16]
        })
        .is_none());
    }
}
