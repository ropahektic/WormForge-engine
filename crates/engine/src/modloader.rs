//! Companion "WormForge Mods" window: enable / disable packs without moving
//! folders, and install or update them one by one from the online index.
//!
//! The window is a plain Win32 list view with check boxes, created on its own
//! thread right after the hooks are installed — before WA's front end exists.
//! It writes `Mods\wormforge.toml`; the scanner in `apply::on_gfx_ready` reads
//! that file at every match start, so a toggle needs no further plumbing and
//! simply applies to the next match. Nothing here touches WA's own windows.
//!
//! Online side: `index.toml` (see `weapons::PackIndex`) lists one zip per
//! pack with its sha256. The loader records the hash it installed, so "update
//! available" works even when a pack author forgets to bump `version`.

use crate::log::{log_error, log_line, panic_text, wa_directory};
use lua_host::mods_root_from;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering};
use std::sync::Mutex;
use weapons::{
    conflict_text, list_mods, parse_index, slot_conflicts, LoaderSettings, ModEntry, PackIndex,
    PackKind, RemotePack, SlotConflict,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, GetStockObject, GetSysColor, COLOR_BTNFACE, DEFAULT_GUI_FONT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemServices::SS_BITMAP;
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, INITCOMMONCONTROLSEX, ICC_LISTVIEW_CLASSES, LVCF_TEXT, LVCF_WIDTH,
    LVCOLUMNW, LVIF_STATE, LVIF_TEXT, LVIS_STATEIMAGEMASK, LVITEMW, LVM_DELETEALLITEMS,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVNI_SELECTED, LVN_ITEMCHANGED, LVS_EX_CHECKBOXES,
    LVS_EX_FULLROWSELECT, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMHDR, NMLISTVIEW,
    WC_LISTVIEWW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetDlgItem,
    GetMessageW, IsWindow, LoadCursorW, MoveWindow, PostMessageW, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetWindowTextW, ShowWindow, TranslateMessage, CW_USEDEFAULT,
    IDC_ARROW, IMAGE_BITMAP, MSG, STM_SETIMAGE, SW_SHOWNORMAL, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_NOTIFY, WM_SETFONT, WM_SIZE, WNDCLASSEXW, WS_BORDER, WS_CHILD,
    WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
};

const CLASS_NAME: &str = "WormForgeModLoader";
const ID_LIST: usize = 101;
const ID_REFRESH: usize = 102;
const ID_OPEN: usize = 103;
const ID_STATUS: usize = 104;
const ID_PEER: usize = 105;
const ID_CHECK: usize = 106;
const ID_INSTALL: usize = 107;
const ID_UPDATE_ALL: usize = 108;
const ID_BANNER: usize = 109;
const ID_TAB_ALL: usize = 110;
const ID_TAB_WEAPONS: usize = 111;
const ID_TAB_GAMEPLAY: usize = 112;
const NETWORK_BUTTONS: [usize; 3] = [ID_CHECK, ID_INSTALL, ID_UPDATE_ALL];
const TAB_BUTTONS: [(usize, &str); 3] = [
    (ID_TAB_ALL, "All"),
    (ID_TAB_WEAPONS, "Weapons"),
    (ID_TAB_GAMEPLAY, "Gameplay"),
];

/// 0 = all, 1 = weapons, 2 = gameplay.
static FILTER: AtomicUsize = AtomicUsize::new(0);
/// Full merged list; `ROWS` is the tab-filtered view shown in the list.

/// Online catalogue produced by `tools/publish-packs.ps1`.
pub const INDEX_URL: &str = "https://ropahektic.github.io/WormForge/downloads/packs/index.toml";
/// Staging folder inside `Mods\`; the `_` prefix keeps the scanner out of it.
const DOWNLOAD_STAGING: &str = "_download_tmp";

const BANNER_PNG: &[u8] = include_bytes!("../assets/modloader_banner.png");
const BANNER_WIDTH: i32 = 360;

/// Re-read `STATUS` / `PEER` into their labels (posted from any thread).
const WM_APP_TEXT: u32 = WM_APP + 1;
/// Rescan `Mods\` and rebuild the list (posted from any thread).
const WM_APP_REFRESH: u32 = WM_APP + 2;
/// Network worker finished; re-enable the online buttons and rescan.
const WM_APP_NET_DONE: u32 = WM_APP + 3;

static BUSY: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static LIST: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static STATUS_LABEL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static PEER_LABEL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static BANNER_W: AtomicI32 = AtomicI32::new(0);
static BANNER_H: AtomicI32 = AtomicI32::new(0);
/// Item-state notifications fire while the list is being filled; ignore them.
static POPULATING: AtomicBool = AtomicBool::new(false);
static STATUS: Mutex<String> = Mutex::new(String::new());
static PEER: Mutex<String> = Mutex::new(String::new());
static ROWS: Mutex<Vec<Row>> = Mutex::new(Vec::new());
static ALL_ROWS: Mutex<Vec<Row>> = Mutex::new(Vec::new());
/// Last index fetched by "Check online"; merged into the list until restart.
static REMOTE: Mutex<Option<PackIndex>> = Mutex::new(None);

/// One list row: a folder on disk, an entry in the online index, or both.
#[derive(Clone)]
struct Row {
    local: Option<ModEntry>,
    remote: Option<RemotePack>,
}

#[derive(PartialEq)]
enum Online {
    Unknown,
    NotInstalled,
    UpToDate,
    UpdateAvailable,
}

impl Row {
    fn id(&self) -> String {
        if let Some(l) = &self.local {
            if let Ok(m) = &l.manifest {
                return m.id.clone();
            }
        }
        if let Some(r) = &self.remote {
            return r.id.clone();
        }
        self.local
            .as_ref()
            .map(|l| l.folder.clone())
            .unwrap_or_default()
    }

    fn enabled(&self) -> bool {
        self.local.as_ref().is_some_and(|l| l.enabled)
    }

    fn kind(&self) -> PackKind {
        if let Some(l) = &self.local {
            if let Ok(m) = &l.manifest {
                return m.kind;
            }
        }
        self.remote
            .as_ref()
            .map(|r| r.kind)
            .unwrap_or(PackKind::Weapons)
    }

    fn online(&self, settings: &LoaderSettings) -> Online {
        let Some(r) = &self.remote else {
            return Online::Unknown;
        };
        if self.local.is_none() {
            return Online::NotInstalled;
        }
        match settings.installed.get(&r.id) {
            Some(sha) if sha.eq_ignore_ascii_case(&r.sha256) => Online::UpToDate,
            _ => Online::UpdateAvailable,
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

fn mods_dir() -> PathBuf {
    mods_root_from(&wa_directory())
}

fn set_text(slot: &Mutex<String>, text: &str) {
    if let Ok(mut g) = slot.lock() {
        *g = text.to_string();
    }
    let hwnd = WINDOW.load(Ordering::SeqCst);
    if !hwnd.is_null() {
        unsafe {
            PostMessageW(hwnd, WM_APP_TEXT, 0, 0);
        }
    }
}

/// Local status line: what the last scan loaded.
pub fn set_status(text: &str) {
    set_text(&STATUS, text);
}

/// Network line: result of comparing our modset with a peer's.
pub fn peer_report(text: &str) {
    set_text(&PEER, text);
}

pub fn match_loaded(mods: &[weapons::LoadedMod]) {
    let names: Vec<&str> = mods.iter().map(|m| m.manifest.id.as_str()).collect();
    let text = if names.is_empty() {
        "Match started with no packs.".to_string()
    } else {
        format!("Match started with {}: {}", names.len(), names.join(", "))
    };
    set_status(&text);
    peer_report("");
}

pub fn match_ended() {
    set_status("Back in the front end. Toggles apply to the next match.");
    let hwnd = WINDOW.load(Ordering::SeqCst);
    if !hwnd.is_null() {
        unsafe {
            PostMessageW(hwnd, WM_APP_REFRESH, 0, 0);
        }
    }
}

/// Spawn the UI thread. Safe to call once; the thread owns the window.
pub fn start() {
    std::thread::Builder::new()
        .name("wf-modloader".into())
        .spawn(|| {
            let result = std::panic::catch_unwind(|| unsafe { ui_thread() });
            if let Err(p) = result {
                log_error(&format!("mod loader window panicked: {}", panic_text(p)));
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| log_error(&format!("mod loader thread: {e}")));
}

unsafe fn ui_thread() {
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES,
    };
    InitCommonControlsEx(&icc);

    let hinst = GetModuleHandleW(std::ptr::null());
    let class = wide(CLASS_NAME);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: std::ptr::null_mut(),
        hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
        // COLOR_BTNFACE (15) + 1: system brush handle by index.
        hbrBackground: 16usize as *mut c_void,
        lpszMenuName: std::ptr::null(),
        lpszClassName: class.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    if RegisterClassExW(&wc) == 0 {
        log_error("mod loader: RegisterClassExW failed");
        return;
    }
    let title = wide("WormForge Mods");
    let hwnd = CreateWindowExW(
        0,
        class.as_ptr(),
        title.as_ptr(),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        880,
        640,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        hinst,
        std::ptr::null(),
    );
    if hwnd.is_null() {
        log_error("mod loader: CreateWindowExW failed");
        return;
    }
    WINDOW.store(hwnd, Ordering::SeqCst);
    ShowWindow(hwnd, SW_SHOWNORMAL);
    log_line(&format!(
        "mod loader window open; settings {}",
        mods_dir().join(weapons::SETTINGS_FILE).display()
    ));

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    WINDOW.store(std::ptr::null_mut(), Ordering::SeqCst);
    log_line("mod loader window closed");
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let result = std::panic::catch_unwind(|| unsafe { handle(hwnd, msg, wparam, lparam) });
    match result {
        Ok(Some(r)) => r,
        Ok(None) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        Err(p) => {
            log_error(&format!("mod loader wndproc panic: {}", panic_text(p)));
            0
        }
    }
}

unsafe fn handle(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_CREATE => {
            create_children(hwnd);
            populate();
            Some(0)
        }
        WM_SIZE => {
            layout(hwnd);
            Some(0)
        }
        WM_COMMAND => {
            match wparam & 0xFFFF {
                ID_REFRESH => populate(),
                ID_OPEN => open_folder(hwnd),
                ID_CHECK => check_online(hwnd),
                ID_INSTALL => install_selected(hwnd),
                ID_UPDATE_ALL => install_all(hwnd),
                ID_TAB_ALL => set_filter(hwnd, 0),
                ID_TAB_WEAPONS => set_filter(hwnd, 1),
                ID_TAB_GAMEPLAY => set_filter(hwnd, 2),
                _ => return None,
            }
            Some(0)
        }
        WM_APP_NET_DONE => {
            set_network_buttons(hwnd, true);
            populate();
            Some(0)
        }
        WM_NOTIFY => {
            let hdr = lparam as *const NMHDR;
            if hdr.is_null() {
                return None;
            }
            if (*hdr).idFrom == ID_LIST && (*hdr).code == LVN_ITEMCHANGED {
                let nm = lparam as *const NMLISTVIEW;
                on_item_changed(&*nm);
            }
            Some(0)
        }
        WM_APP_TEXT => {
            let status = STATUS.lock().map(|g| g.clone()).unwrap_or_default();
            let peer = PEER.lock().map(|g| g.clone()).unwrap_or_default();
            SetWindowTextW(STATUS_LABEL.load(Ordering::SeqCst), wide(&status).as_ptr());
            SetWindowTextW(PEER_LABEL.load(Ordering::SeqCst), wide(&peer).as_ptr());
            Some(0)
        }
        WM_APP_REFRESH => {
            populate();
            Some(0)
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            Some(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            Some(0)
        }
        _ => None,
    }
}

unsafe fn create_children(parent: HWND) {
    let hinst = GetModuleHandleW(std::ptr::null());
    let font = GetStockObject(DEFAULT_GUI_FONT);
    let child = |class: *const u16, text: &str, style: u32, id: usize| -> HWND {
        let t = wide(text);
        let h = CreateWindowExW(
            0,
            class,
            t.as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            10,
            10,
            parent,
            id as *mut c_void,
            hinst,
            std::ptr::null(),
        );
        SendMessageW(h, WM_SETFONT, font as usize, 1);
        h
    };
    let button = wide("BUTTON");
    let static_class = wide("STATIC");

    let list = child(
        WC_LISTVIEWW,
        "",
        WS_TABSTOP | WS_BORDER | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
        ID_LIST,
    );
    SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT) as isize,
    );
    LIST.store(list, Ordering::SeqCst);
    for (i, (name, width)) in [
        ("Mod", 160),
        ("Version", 90),
        ("Author", 80),
        ("Slot / role", 120),
        ("Status", 360),
    ]
    .iter()
    .enumerate()
    {
        let mut text = wide(name);
        let col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            fmt: 0,
            cx: *width,
            pszText: text.as_mut_ptr(),
            cchTextMax: 0,
            iSubItem: i as i32,
            iImage: 0,
            iOrder: 0,
            cxMin: 0,
            cxDefault: 0,
            cxIdeal: 0,
        };
        SendMessageW(list, LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
    }

    for (id, label) in TAB_BUTTONS {
        child(button.as_ptr(), label, WS_TABSTOP, id);
    }
    child(button.as_ptr(), "Check online", WS_TABSTOP, ID_CHECK);
    child(button.as_ptr(), "Install / update selected", WS_TABSTOP, ID_INSTALL);
    child(button.as_ptr(), "Install all updates", WS_TABSTOP, ID_UPDATE_ALL);
    child(button.as_ptr(), "Refresh", WS_TABSTOP, ID_REFRESH);
    child(button.as_ptr(), "Open Mods folder", WS_TABSTOP, ID_OPEN);
    let status = child(static_class.as_ptr(), "", 0, ID_STATUS);
    STATUS_LABEL.store(status, Ordering::SeqCst);
    let peer = child(static_class.as_ptr(), "", 0, ID_PEER);
    PEER_LABEL.store(peer, Ordering::SeqCst);

    let banner = child(static_class.as_ptr(), "", SS_BITMAP, ID_BANNER);
    match banner_bitmap() {
        Ok((bmp, w, h)) => {
            SendMessageW(banner, STM_SETIMAGE, IMAGE_BITMAP as usize, bmp as isize);
            // Two atomics: usize is 32 bits in WA's process, so packing both
            // into one word silently dropped the width.
            BANNER_W.store(w, Ordering::SeqCst);
            BANNER_H.store(h, Ordering::SeqCst);
        }
        Err(e) => log_error(&format!("mod loader banner: {e}")),
    }
}

/// Decode the embedded PNG, box-filter it to `BANNER_WIDTH` if needed,
/// composite over the dialog colour and hand GDI a 32-bit DDB.
unsafe fn banner_bitmap() -> Result<(*mut c_void, i32, i32), String> {
    let decoder = png::Decoder::new(BANNER_PNG);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (sw, sh) = (info.width as usize, info.height as usize);
    let bpp = info.color_type.samples();
    let px = |x: usize, y: usize| -> [u8; 4] {
        let i = (y * sw + x) * bpp;
        match bpp {
            4 => [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]],
            3 => [buf[i], buf[i + 1], buf[i + 2], 255],
            2 => [buf[i], buf[i], buf[i], buf[i + 1]],
            _ => [buf[i], buf[i], buf[i], 255],
        }
    };
    let dw = (BANNER_WIDTH as usize).min(sw);
    let dh = (sh * dw / sw).max(1);
    let bg = GetSysColor(COLOR_BTNFACE);
    let (br, bgc, bb) = ((bg & 0xFF) as f32, ((bg >> 8) & 0xFF) as f32, ((bg >> 16) & 0xFF) as f32);
    let mut out = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1).min(sh);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1).min(sw);
            let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
            let n = ((y1 - y0) * (x1 - x0)) as f32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = px(x, y);
                    let pa = p[3] as f32 / 255.0;
                    r += p[0] as f32 * pa;
                    g += p[1] as f32 * pa;
                    b += p[2] as f32 * pa;
                    a += pa;
                }
            }
            let a_avg = a / n;
            let (r, g, b) = if a > 0.0 { (r / a, g / a, b / a) } else { (0.0, 0.0, 0.0) };
            let mix = |c: f32, back: f32| (c * a_avg + back * (1.0 - a_avg)).round() as u8;
            let o = (dy * dw + dx) * 4;
            out[o] = mix(b, bb);
            out[o + 1] = mix(g, bgc);
            out[o + 2] = mix(r, br);
            out[o + 3] = 255;
        }
    }
    let bmp = CreateBitmap(dw as i32, dh as i32, 1, 32, out.as_ptr() as *const c_void);
    if bmp.is_null() {
        return Err("CreateBitmap failed".into());
    }
    Ok((bmp, dw as i32, dh as i32))
}

unsafe fn layout(hwnd: HWND) {
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;
    let pad = 8;
    let button_h = 26;
    let line_h = 18;
    let (bw, bh) = (BANNER_W.load(Ordering::SeqCst), BANNER_H.load(Ordering::SeqCst));
    let ctl = |id: usize| -> HWND { GetDlgItem(hwnd, id as i32) };

    // Header banner, centred, natural size.
    MoveWindow(ctl(ID_BANNER), ((w - bw) / 2).max(0), pad, bw, bh, 1);
    let mut y = pad + bh + pad;

    // Kind tabs: All / Weapons / Gameplay.
    let tab_w = 96;
    for (i, (id, _)) in TAB_BUTTONS.iter().enumerate() {
        MoveWindow(ctl(*id), pad + i as i32 * (tab_w + pad), y, tab_w, button_h, 1);
    }
    y += button_h + pad;

    // List, then one row of buttons, then the two status lines.
    let footer = button_h + pad + line_h + 4 + line_h + pad;
    let list_h = (h - y - footer - pad).max(40);
    MoveWindow(ctl(ID_LIST), pad, y, w - 2 * pad, list_h, 1);
    y = y + list_h + pad;
    let buttons = [ID_CHECK, ID_INSTALL, ID_UPDATE_ALL, ID_REFRESH, ID_OPEN];
    let n = buttons.len() as i32;
    let bwidth = ((w - 2 * pad - (n - 1) * pad) / n).max(80);
    for (i, id) in buttons.iter().enumerate() {
        MoveWindow(ctl(*id), pad + i as i32 * (bwidth + pad), y, bwidth, button_h, 1);
    }
    y = y + button_h + pad;
    MoveWindow(ctl(ID_STATUS), pad, y, w - 2 * pad, line_h, 1);
    MoveWindow(ctl(ID_PEER), pad, y + line_h + 4, w - 2 * pad, line_h, 1);
}

unsafe fn set_filter(hwnd: HWND, filter: usize) {
    FILTER.store(filter, Ordering::SeqCst);
    paint_tabs(hwnd);
    fill_list_from_all();
}

unsafe fn paint_tabs(hwnd: HWND) {
    let filter = FILTER.load(Ordering::SeqCst);
    for (i, (id, label)) in TAB_BUTTONS.iter().enumerate() {
        let text = if i == filter {
            format!("[{label}]")
        } else {
            (*label).to_string()
        };
        SetWindowTextW(GetDlgItem(hwnd, *id as i32), wide(&text).as_ptr());
    }
}

unsafe fn set_network_buttons(hwnd: HWND, enabled: bool) {
    for id in NETWORK_BUTTONS {
        EnableWindow(GetDlgItem(hwnd, id as i32), enabled as i32);
    }
}

unsafe fn open_folder(hwnd: HWND) {
    let dir = mods_dir();
    let _ = std::fs::create_dir_all(&dir);
    let verb = wide("open");
    let path = wide(&dir.to_string_lossy());
    ShellExecuteW(
        hwnd,
        verb.as_ptr(),
        path.as_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        SW_SHOWNORMAL as i32,
    );
}

// ---------------------------------------------------------------- rows

/// Shown in the Mod column. The `example.` vendor prefix carries no
/// information for a player; ids elsewhere (settings, log, wire) keep it.
fn display_name(id: &str) -> String {
    id.strip_prefix("example.").unwrap_or(id).to_string()
}

fn normalise_slot(n: &str) -> String {
    weapons::parse_stock_weapon(n)
        .map(|s| s.name().to_string())
        .unwrap_or_else(|_| format!("{n}?"))
}

fn weapon_slots<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let mut out: Vec<String> = names.map(|n| normalise_slot(n)).collect();
    out.sort();
    out.dedup();
    out.join(", ")
}

/// Stock panel slots this pack takes (`replace` + `overlay`), or a role label
/// for gameplay packs that claim none.
fn role_column(row: &Row) -> String {
    match row.kind() {
        PackKind::Gameplay => "ruleset".into(),
        PackKind::Weapons => {
            let slots = match (&row.local, &row.remote) {
                (Some(l), _) => match &l.manifest {
                    Ok(m) => weapon_slots(m.weapons.replace.iter().chain(m.weapons.overlay.iter())),
                    Err(_) => String::new(),
                },
                (None, Some(r)) => weapon_slots(r.slots.iter()),
                _ => String::new(),
            };
            if slots.is_empty() {
                "—".into()
            } else {
                slots
            }
        }
    }
}

fn row_matches_filter(row: &Row, filter: usize) -> bool {
    match filter {
        1 => row.kind() == PackKind::Weapons,
        2 => row.kind() == PackKind::Gameplay,
        _ => true,
    }
}

/// Disk entries merged with the last fetched index, sorted by display name.
fn build_rows(entries: Vec<ModEntry>, remote: Option<&PackIndex>) -> Vec<Row> {
    let mut rows: Vec<Row> = entries
        .into_iter()
        .map(|e| Row {
            local: Some(e),
            remote: None,
        })
        .collect();
    if let Some(idx) = remote {
        for r in &idx.packs {
            match rows.iter_mut().find(|row| {
                row.local
                    .as_ref()
                    .is_some_and(|l| l.manifest.as_ref().is_ok_and(|m| m.id == r.id))
            }) {
                Some(row) => row.remote = Some(r.clone()),
                None => rows.push(Row {
                    local: None,
                    remote: Some(r.clone()),
                }),
            }
        }
    }
    rows.sort_by_key(|r| display_name(&r.id()).to_lowercase());
    rows
}

fn local_entries(rows: &[Row]) -> Vec<ModEntry> {
    rows.iter().filter_map(|r| r.local.clone()).collect()
}

/// Per-row status text: manifest errors, missing `requires`, duplicate ids,
/// every slot fight with the packs named, then the online state.
fn row_status(rows: &[Row], conflicts: &[SlotConflict], settings: &LoaderSettings, row: &Row) -> String {
    let online = match row.online(settings) {
        Online::Unknown => String::new(),
        Online::NotInstalled => "not installed - select it and press Install".into(),
        Online::UpToDate => "up to date".into(),
        Online::UpdateAvailable => format!(
            "update available (online v{})",
            row.remote.as_ref().map(|r| r.version.as_str()).unwrap_or("?")
        ),
    };
    let Some(local) = &row.local else {
        return online;
    };
    let m = match &local.manifest {
        Ok(m) => m,
        Err(e) => return format!("error: {e}"),
    };
    let mut parts = Vec::new();
    if !local.enabled {
        parts.push("off".to_string());
    } else {
        let enabled_ids: Vec<&str> = rows
            .iter()
            .filter(|r| r.enabled())
            .filter_map(|r| r.local.as_ref())
            .filter_map(|l| l.manifest.as_ref().ok().map(|m| m.id.as_str()))
            .collect();
        let first = rows.iter().find(|r| {
            r.enabled()
                && r.local
                    .as_ref()
                    .is_some_and(|l| l.manifest.as_ref().is_ok_and(|rm| rm.id == m.id))
        });
        if let Some(first) = first.and_then(|r| r.local.as_ref()) {
            if first.folder != local.folder {
                parts.push(format!("duplicate id; {} loads instead", first.folder));
            }
        }
        let missing: Vec<&str> = m
            .weapons
            .requires
            .iter()
            .map(String::as_str)
            .filter(|r| !enabled_ids.contains(r))
            .collect();
        if !missing.is_empty() {
            parts.push(format!("skipped: needs {}", missing.join(", ")));
        }
        let conflict = conflict_text(conflicts, &m.id);
        if !conflict.is_empty() {
            parts.push(format!("CONFLICT: {conflict}"));
        }
    }
    if !online.is_empty() {
        parts.push(online);
    }
    parts.join(" | ")
}

/// "N of M packs enabled" plus a conflict count and the contested slots.
fn summary_line(rows: &[Row], conflicts: &[SlotConflict], settings: &LoaderSettings) -> String {
    let total = rows.iter().filter(|r| r.local.is_some()).count();
    let enabled = rows.iter().filter(|r| r.enabled()).count();
    let mut s = format!("{enabled} of {total} packs enabled.");
    let updates = rows
        .iter()
        .filter(|r| matches!(r.online(settings), Online::UpdateAvailable | Online::NotInstalled))
        .count();
    if updates > 0 {
        s.push_str(&format!(" {updates} online pack(s) new or updated."));
    }
    if conflicts.is_empty() {
        s.push_str(" Toggles apply to the next match.");
    } else {
        let slots: Vec<String> = conflicts
            .iter()
            .map(|c| format!("{} ({})", c.slot, c.claimants.join(" vs ")))
            .collect();
        s.push_str(&format!(
            " {} slot conflict{}: {}",
            conflicts.len(),
            if conflicts.len() == 1 { "" } else { "s" },
            slots.join("; ")
        ));
    }
    s
}

unsafe fn set_item_text(list: HWND, item: i32, sub: i32, text: &str) {
    let mut t = wide(text);
    let lvi = LVITEMW {
        mask: LVIF_TEXT,
        iItem: item,
        iSubItem: sub,
        state: 0,
        stateMask: 0,
        pszText: t.as_mut_ptr(),
        cchTextMax: 0,
        iImage: 0,
        lParam: 0,
        iIndent: 0,
        iGroupId: 0,
        cColumns: 0,
        puColumns: std::ptr::null_mut(),
        piColFmt: std::ptr::null_mut(),
        iGroup: 0,
    };
    if sub == 0 {
        SendMessageW(list, LVM_INSERTITEMW, 0, &lvi as *const _ as isize);
    } else {
        SendMessageW(list, LVM_SETITEMTEXTW, item as usize, &lvi as *const _ as isize);
    }
}

unsafe fn set_item_checked(list: HWND, item: i32, checked: bool) {
    let lvi = LVITEMW {
        mask: LVIF_STATE,
        iItem: item,
        iSubItem: 0,
        state: ((if checked { 2 } else { 1 }) << 12) as u32,
        stateMask: LVIS_STATEIMAGEMASK,
        pszText: std::ptr::null_mut(),
        cchTextMax: 0,
        iImage: 0,
        lParam: 0,
        iIndent: 0,
        iGroupId: 0,
        cColumns: 0,
        puColumns: std::ptr::null_mut(),
        piColFmt: std::ptr::null_mut(),
        iGroup: 0,
    };
    SendMessageW(list, LVM_SETITEMSTATE, item as usize, &lvi as *const _ as isize);
}

/// Rescan `Mods\`, merge the cached index, rebuild every row.
unsafe fn populate() {
    let hwnd = WINDOW.load(Ordering::SeqCst);
    let dir = mods_dir();
    let entries = match list_mods(&dir) {
        Ok(e) => e,
        Err(e) => {
            set_status(&format!("Cannot read {}: {e}", dir.display()));
            return;
        }
    };
    let all = {
        let remote = REMOTE.lock().map(|g| g.clone()).unwrap_or(None);
        build_rows(entries, remote.as_ref())
    };
    if let Ok(mut g) = ALL_ROWS.lock() {
        *g = all;
    }
    if !hwnd.is_null() {
        paint_tabs(hwnd);
    }
    fill_list_from_all();
}

/// Paint the current tab's rows from `ALL_ROWS` into the list view.
unsafe fn fill_list_from_all() {
    let list = LIST.load(Ordering::SeqCst);
    if list.is_null() {
        return;
    }
    let dir = mods_dir();
    let settings = LoaderSettings::load(&dir);
    let filter = FILTER.load(Ordering::SeqCst);
    let all = ALL_ROWS.lock().map(|g| g.clone()).unwrap_or_default();
    let conflicts = slot_conflicts(&local_entries(&all));
    let rows: Vec<Row> = all
        .iter()
        .filter(|r| row_matches_filter(r, filter))
        .cloned()
        .collect();

    POPULATING.store(true, Ordering::SeqCst);
    SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
    for (i, row) in rows.iter().enumerate() {
        let i = i as i32;
        let (name, version, author) = match (&row.local, &row.remote) {
            (Some(l), remote) => match &l.manifest {
                Ok(m) => {
                    let version = match remote {
                        Some(r)
                            if r.version != m.version
                                && row.online(&settings) == Online::UpdateAvailable =>
                        {
                            format!("{} -> {}", m.version, r.version)
                        }
                        _ => m.version.clone(),
                    };
                    (display_name(&m.id), version, m.author.clone())
                }
                Err(_) => (
                    format!("{} (broken mod.toml)", l.folder),
                    String::new(),
                    String::new(),
                ),
            },
            (None, Some(r)) => (display_name(&r.id), r.version.clone(), r.author.clone()),
            (None, None) => continue,
        };
        set_item_text(list, i, 0, &name);
        set_item_text(list, i, 1, &version);
        set_item_text(list, i, 2, &author);
        set_item_text(list, i, 3, &role_column(row));
        // Conflicts / requires use the full set so a Weapons-tab pack still
        // shows fights against a Gameplay pack that is enabled but hidden.
        set_item_text(list, i, 4, &row_status(&all, &conflicts, &settings, row));
        set_item_checked(list, i, row.enabled());
    }
    POPULATING.store(false, Ordering::SeqCst);

    let tab = match filter {
        1 => "Weapons",
        2 => "Gameplay",
        _ => "All",
    };
    let summary = if !dir.is_dir() {
        format!("No Mods folder yet: {}", dir.display())
    } else if all.is_empty() {
        format!("No packs in {}. Press Check online.", dir.display())
    } else if rows.is_empty() {
        format!("No {tab} packs. Try another tab or Check online.")
    } else {
        let mut s = summary_line(&all, &conflicts, &settings);
        s.push_str(&format!(" Showing {tab} ({}/{}).", rows.len(), all.len()));
        s
    };
    if let Ok(mut g) = ROWS.lock() {
        *g = rows;
    }
    set_status(&summary);
}

unsafe fn refresh_status_column() {
    let list = LIST.load(Ordering::SeqCst);
    let Ok(rows) = ROWS.lock() else {
        return;
    };
    let all = ALL_ROWS.lock().map(|g| g.clone()).unwrap_or_else(|_| rows.clone());
    let settings = LoaderSettings::load(&mods_dir());
    let conflicts = slot_conflicts(&local_entries(&all));
    for (i, row) in rows.iter().enumerate() {
        set_item_text(list, i as i32, 4, &row_status(&all, &conflicts, &settings, row));
    }
    set_status(&format!("Saved. {}", summary_line(&all, &conflicts, &settings)));
}

unsafe fn on_item_changed(nm: &NMLISTVIEW) {
    if POPULATING.load(Ordering::SeqCst) || nm.uChanged & LVIF_STATE == 0 {
        return;
    }
    let old = nm.uOldState & LVIS_STATEIMAGEMASK;
    let new = nm.uNewState & LVIS_STATEIMAGEMASK;
    if old == new || new == 0 {
        return;
    }
    let checked = (new >> 12) == 2;
    let dir = mods_dir();
    enum Outcome {
        Toggle(String),
        Broken,
        NotInstalled,
    }
    let outcome = {
        let Ok(mut rows) = ROWS.lock() else {
            return;
        };
        let Some(row) = rows.get_mut(nm.iItem as usize) else {
            return;
        };
        match row.local.as_mut() {
            None => Outcome::NotInstalled,
            Some(local) => match &local.manifest {
                Ok(m) => {
                    local.enabled = checked;
                    let id = m.id.clone();
                    if let Ok(mut all) = ALL_ROWS.lock() {
                        if let Some(full) = all.iter_mut().find(|r| r.id() == id) {
                            if let Some(l) = full.local.as_mut() {
                                l.enabled = checked;
                            }
                        }
                    }
                    Outcome::Toggle(id)
                }
                Err(_) => Outcome::Broken,
            },
        }
    };
    let id = match outcome {
        Outcome::Toggle(id) => id,
        Outcome::Broken => {
            set_item_checked(LIST.load(Ordering::SeqCst), nm.iItem, false);
            set_status("Fix that pack's mod.toml before enabling it.");
            return;
        }
        Outcome::NotInstalled => {
            set_item_checked(LIST.load(Ordering::SeqCst), nm.iItem, false);
            set_status("That pack is online only. Select it and press Install / update selected.");
            return;
        }
    };
    let mut settings = LoaderSettings::load(&dir);
    settings.set_enabled(&id, checked);
    match settings.save(&dir) {
        Ok(()) => log_line(&format!(
            "mod loader: {id} {}",
            if checked { "enabled" } else { "disabled" }
        )),
        Err(e) => {
            log_error(&format!("mod loader: cannot save {}: {e}", weapons::SETTINGS_FILE));
            set_status(&format!("Could not write {}: {e}", weapons::SETTINGS_FILE));
        }
    }
    refresh_status_column();
}

// ---------------------------------------------------------------- online

/// Run a network job off the UI thread; buttons come back via WM_APP_NET_DONE.
unsafe fn run_network(hwnd: HWND, job: impl FnOnce() -> Result<String, String> + Send + 'static) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    set_network_buttons(hwnd, false);
    let hwnd_val = hwnd as usize;
    let spawned = std::thread::Builder::new()
        .name("wf-modloader-net".into())
        .spawn(move || {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) {
                Ok(Ok(summary)) => {
                    log_line(&format!("mod loader: {summary}"));
                    set_status(&summary);
                }
                Ok(Err(e)) => {
                    log_error(&format!("mod loader online: {e}"));
                    set_status(&format!("Online: {e}"));
                }
                Err(p) => {
                    log_error(&format!("mod loader online panicked: {}", panic_text(p)));
                    set_status("Online operation failed (internal error; see wkLua.log).");
                }
            }
            BUSY.store(false, Ordering::SeqCst);
            PostMessageW(hwnd_val as HWND, WM_APP_NET_DONE, 0, 0);
        });
    if let Err(e) = spawned {
        BUSY.store(false, Ordering::SeqCst);
        set_network_buttons(hwnd, true);
        set_status(&format!("Could not start network thread: {e}"));
    }
}

fn fetch_index() -> Result<PackIndex, String> {
    set_status("Fetching pack index…");
    let bytes = crate::http::get(INDEX_URL, |_, _| {})?;
    let text = String::from_utf8(bytes).map_err(|_| "index is not UTF-8".to_string())?;
    let idx = parse_index(&text)?;
    if let Ok(mut g) = REMOTE.lock() {
        *g = Some(idx.clone());
    }
    Ok(idx)
}

unsafe fn check_online(hwnd: HWND) {
    run_network(hwnd, || {
        let idx = fetch_index()?;
        let settings = LoaderSettings::load(&mods_dir());
        let local = list_mods(&mods_dir()).unwrap_or_default();
        let rows = build_rows(local, Some(&idx));
        let new = rows.iter().filter(|r| r.online(&settings) == Online::NotInstalled).count();
        let upd = rows
            .iter()
            .filter(|r| r.online(&settings) == Online::UpdateAvailable)
            .count();
        Ok(format!(
            "Online index has {} packs ({}): {new} not installed, {upd} with updates.",
            idx.packs.len(),
            if idx.generated.is_empty() { "no date" } else { idx.generated.as_str() }
        ))
    });
}

fn selected_row() -> Option<Row> {
    let list = LIST.load(Ordering::SeqCst);
    let i = unsafe { SendMessageW(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize) };
    if i < 0 {
        return None;
    }
    ROWS.lock().ok()?.get(i as usize).cloned()
}

unsafe fn install_selected(hwnd: HWND) {
    let Some(row) = selected_row() else {
        set_status("Select a pack in the list first.");
        return;
    };
    let Some(remote) = row.remote.clone() else {
        set_status(if REMOTE.lock().map(|g| g.is_none()).unwrap_or(true) {
            "Press Check online first."
        } else {
            "That pack is not in the online index; nothing to install."
        });
        return;
    };
    let Some(index) = REMOTE.lock().ok().and_then(|g| g.clone()) else {
        return;
    };
    run_network(hwnd, move || install_packs(&index, &[remote]));
}

unsafe fn install_all(hwnd: HWND) {
    let Some(index) = REMOTE.lock().ok().and_then(|g| g.clone()) else {
        set_status("Press Check online first.");
        return;
    };
    let settings = LoaderSettings::load(&mods_dir());
    let todo: Vec<RemotePack> = ROWS
        .lock()
        .map(|rows| {
            rows.iter()
                .filter(|r| matches!(r.online(&settings), Online::NotInstalled | Online::UpdateAvailable))
                .filter_map(|r| r.remote.clone())
                .collect()
        })
        .unwrap_or_default();
    if todo.is_empty() {
        set_status("Everything is up to date.");
        return;
    }
    run_network(hwnd, move || install_packs(&index, &todo));
}

/// Download, verify and install each pack in turn; record its hash.
fn install_packs(index: &PackIndex, packs: &[RemotePack]) -> Result<String, String> {
    let mods_dir = mods_dir();
    std::fs::create_dir_all(&mods_dir)
        .map_err(|e| format!("create {}: {e}", mods_dir.display()))?;
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for (n, pack) in packs.iter().enumerate() {
        let label = format!("[{}/{}] {}", n + 1, packs.len(), display_name(&pack.id));
        match install_one(index, pack, &mods_dir, &label) {
            Ok(()) => done.push(display_name(&pack.id)),
            Err(e) => {
                log_error(&format!("mod loader install {}: {e}", pack.id));
                failed.push(format!("{} ({e})", display_name(&pack.id)));
            }
        }
    }
    let mut s = String::new();
    if !done.is_empty() {
        s.push_str(&format!("Installed {}: {}.", done.len(), done.join(", ")));
    }
    if !failed.is_empty() {
        s.push_str(&format!(" Failed: {}.", failed.join("; ")));
    }
    if done.is_empty() {
        return Err(s.trim().to_string());
    }
    Ok(s.trim().to_string())
}

fn install_one(index: &PackIndex, pack: &RemotePack, mods_dir: &Path, label: &str) -> Result<(), String> {
    let url = pack.url(index);
    let bytes = crate::http::get(&url, |got, total| {
        if total > 0 {
            set_status(&format!("{label}: downloading {} / {} KB", got / 1024, total / 1024));
        } else {
            set_status(&format!("{label}: downloading {} KB", got / 1024));
        }
    })?;
    let sha = crate::sha256::hex(&bytes);
    if !sha.eq_ignore_ascii_case(&pack.sha256) {
        return Err(format!("sha256 mismatch (index {}, got {sha})", pack.sha256));
    }
    set_status(&format!("{label}: unpacking"));
    let staging = mods_dir.join(DOWNLOAD_STAGING);
    let _ = std::fs::remove_dir_all(&staging);
    crate::zip::extract_all(&bytes, &staging)?;
    let mut found = Vec::new();
    collect_packs(&staging, &mut found)?;
    let src = found
        .into_iter()
        .next()
        .ok_or_else(|| "archive has no folder with a mod.toml".to_string())?;
    let folder = if pack.folder.is_empty() || weapons::is_ignored_folder(&pack.folder) {
        src.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| display_name(&pack.id))
    } else {
        pack.folder.clone()
    };
    let dest = mods_dir.join(&folder);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("replace {}: {e}", dest.display()))?;
    }
    std::fs::rename(&src, &dest).map_err(|e| format!("install {}: {e}", dest.display()))?;
    let _ = std::fs::remove_dir_all(&staging);

    let mut settings = LoaderSettings::load(mods_dir);
    settings.installed.insert(pack.id.clone(), sha.clone());
    settings.save(mods_dir)?;
    log_line(&format!("mod loader: installed {} v{} -> {} ({sha})", pack.id, pack.version, dest.display()));
    Ok(())
}

fn collect_packs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if dir.join("mod.toml").is_file() {
        out.push(dir.to_path_buf());
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            collect_packs(&path, out)?;
        }
    }
    Ok(())
}

/// True while the window exists (used by tests / diagnostics only).
#[allow(dead_code)]
pub fn is_open() -> bool {
    let hwnd = WINDOW.load(Ordering::SeqCst);
    !hwnd.is_null() && unsafe { IsWindow(hwnd) } != 0
}
