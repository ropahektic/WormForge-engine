//! Full-window GDI overlays for screen-space cut-ins (speed lines + portrait).
//! Fill and front are composited off-screen, then blitted once so the shader
//! cannot paint over the cutout mid-frame.

use crate::sprite;
use game::offsets::{DISPLAY_HEIGHT, DISPLAY_HWND, DISPLAY_WIDTH};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiAlphaBlend, GetDC, ReleaseDC,
    SelectObject, SetStretchBltMode, StretchBlt, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, COLORONCOLOR, DIB_RGB_COLORS, HDC, SRCCOPY,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

#[derive(Clone, Copy)]
struct Live {
    sprite: i32,
    phase: u32,
    fill: bool,
}

static LIVE: Mutex<Vec<Live>> = Mutex::new(Vec::new());

pub fn push(sprite: i32, phase: u32, fill: bool) {
    if sprite == 0 {
        return;
    }
    if let Ok(mut g) = LIVE.lock() {
        if fill {
            g.retain(|o| !o.fill);
        } else {
            g.retain(|o| o.fill);
        }
        g.push(Live {
            sprite,
            phase,
            fill,
        });
    }
}

pub fn present(display: *mut u8) {
    let mut items = match LIVE.lock() {
        Ok(g) => g.clone(),
        Err(_) => return,
    };
    if items.is_empty() || display.is_null() {
        return;
    }
    items.sort_by_key(|o| if o.fill { 0 } else { 1 });
    unsafe {
        let hwnd = *(display.add(DISPLAY_HWND) as *const HWND);
        if hwnd.is_null() {
            return;
        }
        let hdc = GetDC(hwnd);
        if hdc.is_null() {
            return;
        }
        let (dw, dh) = client_size(hwnd, display);
        if dw > 0 && dh > 0 {
            composite(hdc, dw, dh, &items);
        }
        ReleaseDC(hwnd, hdc);
    }
}

pub fn clear() {
    if let Ok(mut g) = LIVE.lock() {
        g.clear();
    }
}

unsafe fn client_size(hwnd: HWND, display: *mut u8) -> (i32, i32) {
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if GetClientRect(hwnd, &mut rc) != 0 {
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        if w > 8 && h > 8 {
            return (w, h);
        }
    }
    (
        *(display.add(DISPLAY_WIDTH) as *const i32),
        *(display.add(DISPLAY_HEIGHT) as *const i32),
    )
}

unsafe fn composite(hdc: HDC, dw: i32, dh: i32, items: &[Live]) {
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let info = dib_info(dw as u32, dh as u32);
    let mem = CreateCompatibleDC(hdc);
    if mem.is_null() {
        return;
    }
    let dib = CreateDIBSection(
        mem,
        &info,
        DIB_RGB_COLORS,
        &mut bits,
        std::ptr::null_mut(),
        0,
    );
    if dib.is_null() || bits.is_null() {
        DeleteDC(mem);
        return;
    }
    let old = SelectObject(mem, dib as _);
    let _ = SetStretchBltMode(mem, COLORONCOLOR);
    for item in items {
        blit_one(mem, dw, dh, item);
    }
    let _ = SetStretchBltMode(hdc, COLORONCOLOR);
    let _ = StretchBlt(hdc, 0, 0, dw, dh, mem, 0, 0, dw, dh, SRCCOPY);
    SelectObject(mem, old);
    DeleteObject(dib as _);
    DeleteDC(mem);
}

fn dib_info(w: u32, h: u32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [windows_sys::Win32::Graphics::Gdi::RGBQUAD {
            rgbBlue: 0,
            rgbGreen: 0,
            rgbRed: 0,
            rgbReserved: 0,
        }],
    }
}

unsafe fn blit_one(hdc: HDC, dw: i32, dh: i32, item: &Live) {
    let Some((sw, sh, pixels)) = sprite::overlay_frame(item.sprite, item.phase) else {
        return;
    };
    if sw == 0 || sh == 0 || pixels.len() < (sw * sh * 4) as usize {
        return;
    }
    let (dx, dy, ddw, ddh) = if item.fill {
        (0, 0, dw, dh)
    } else {
        let max_w = (dw * 9) / 10;
        let max_h = (dh * 7) / 10;
        let mut cw = max_w;
        let mut ch = ((sh as i64 * cw as i64) / sw as i64) as i32;
        if ch > max_h {
            ch = max_h;
            cw = ((sw as i64 * ch as i64) / sh as i64) as i32;
        }
        ((dw - cw) / 2, (dh - ch) / 2, cw.max(1), ch.max(1))
    };
    let info = dib_info(sw, sh);
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let mem = CreateCompatibleDC(hdc);
    if mem.is_null() {
        return;
    }
    let dib = CreateDIBSection(
        mem,
        &info,
        DIB_RGB_COLORS,
        &mut bits,
        std::ptr::null_mut(),
        0,
    );
    if dib.is_null() || bits.is_null() {
        DeleteDC(mem);
        return;
    }
    let n = (sw * sh * 4) as usize;
    if item.fill {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, n);
    } else {
        premultiply(bits as *mut u8, &pixels, n);
    }
    let old = SelectObject(mem, dib as _);
    if item.fill {
        let _ = StretchBlt(
            hdc, dx, dy, ddw, ddh, mem, 0, 0, sw as i32, sh as i32, SRCCOPY,
        );
    } else {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = GdiAlphaBlend(
            hdc, dx, dy, ddw, ddh, mem, 0, 0, sw as i32, sh as i32, blend,
        );
    }
    SelectObject(mem, old);
    DeleteObject(dib as _);
    DeleteDC(mem);
}

fn premultiply(dst: *mut u8, src: &[u8], n: usize) {
    unsafe {
        for i in (0..n).step_by(4) {
            let a = src[i + 3] as u16;
            *dst.add(i) = ((src[i] as u16 * a) / 255) as u8;
            *dst.add(i + 1) = ((src[i + 1] as u16 * a) / 255) as u8;
            *dst.add(i + 2) = ((src[i + 2] as u16 * a) / 255) as u8;
            *dst.add(i + 3) = src[i + 3];
        }
    }
}
