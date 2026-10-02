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

use crate::log::{log_error, log_line, panic_text, wa_directory, BUILD_ID};
use lua_host::mods_root_from;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering};
use std::sync::Mutex;
use weapons::{
    conflict_text, list_mods, parse_index, slot_conflicts, LoaderSettings, ModEntry, PackIndex,
    PackKind, RemotePack, SlotConflict,
};
use weapons::components::BundleManifest;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateFontW, GetSysColor, GetSysColorBrush, InvalidateRect, SetBkMode,
    SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_BTNFACE, DEFAULT_CHARSET,
    DEFAULT_PITCH, FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD, HDC, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemServices::{SS_BITMAP, SS_ENDELLIPSIS};
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW,
    EM_SETCUEBANNER, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW,
    LVIF_STATE, LVIF_TEXT, LVIS_SELECTED, LVIS_STATEIMAGEMASK, LVITEMW, LVM_DELETEALLITEMS,
    LVM_GETITEMSTATE, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW,
    LVNI_SELECTED, LVN_ITEMCHANGED, LVSCW_AUTOSIZE_USEHEADER, LVS_EX_CHECKBOXES,
    LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP, LVS_REPORT, LVS_SHOWSELALWAYS,
    NMHDR, NMITEMACTIVATE, NMLISTVIEW, NMLVCUSTOMDRAW, NM_CUSTOMDRAW, NM_DBLCLK, WC_LISTVIEWW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetDlgItem,
    GetMessageW, GetWindowTextW, IsWindow, LoadCursorW, MoveWindow, PostMessageW, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetWindowTextW, ShowWindow, TranslateMessage, BM_SETCHECK,
    BM_SETSTYLE, BS_AUTORADIOBUTTON, BS_DEFPUSHBUTTON, BS_GROUPBOX, BS_PUSHBUTTON, BS_PUSHLIKE,
    CW_USEDEFAULT, EN_CHANGE, ES_AUTOHSCROLL, IDC_ARROW, IMAGE_BITMAP, MINMAXINFO, MSG,
    STM_SETIMAGE, SW_SHOWNORMAL, WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORSTATIC,
    WM_DESTROY, WM_GETMINMAXINFO, WM_NOTIFY, WM_SETFONT, WM_SIZE, WNDCLASSEXW, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_GROUP, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
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
const ID_SEARCH: usize = 113;
const ID_ENABLE_ALL: usize = 114;
const ID_DISABLE_ALL: usize = 115;
const ID_GROUP_PACKS: usize = 116;
const ID_GROUP_ONLINE: usize = 117;
/// Legend swatches, one per `Tone`, in `Tone::ALL` order.
const ID_LEGEND: usize = 120;
const NETWORK_BUTTONS: [usize; 3] = [ID_CHECK, ID_INSTALL, ID_UPDATE_ALL];
const TAB_BUTTONS: [(usize, &str); 3] = [
    (ID_TAB_ALL, "All"),
    (ID_TAB_WEAPONS, "Weapons"),
    (ID_TAB_GAMEPLAY, "Gameplay"),
];
const PACK_BUTTONS: [(usize, &str); 4] = [
    (ID_ENABLE_ALL, "Enable all"),
    (ID_DISABLE_ALL, "Disable all"),
    (ID_REFRESH, "Refresh"),
    (ID_OPEN, "Open Mods folder"),
];
const ONLINE_BUTTONS: [(usize, &str); 3] = [
    (ID_CHECK, "Check online"),
    (ID_INSTALL, "Install selected"),
    (ID_UPDATE_ALL, "Update all"),
];

/// 0 = all, 1 = weapons, 2 = gameplay.
static FILTER: AtomicUsize = AtomicUsize::new(0);

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
/// The rows the list shows (tab + search), index for index.
static ROWS: Mutex<Vec<Row>> = Mutex::new(Vec::new());
/// Colour of each shown row, index for index with `ROWS`.
static TONES: Mutex<Vec<Tone>> = Mutex::new(Vec::new());
/// Full merged list; `ROWS` is the filtered view.
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

/// What a row's colour says, most urgent first.
#[derive(Clone, Copy, PartialEq)]
enum Tone {
    On,
    Off,
    Update,
    OnlineOnly,
    Problem,
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

impl Tone {
    const ALL: [Tone; 5] = [Tone::On, Tone::Off, Tone::Update, Tone::OnlineOnly, Tone::Problem];

    fn legend(self) -> &'static str {
        match self {
            Tone::On => "\u{25A0} Enabled",
            Tone::Off => "\u{25A0} Off",
            Tone::Update => "\u{25A0} Update available",
            Tone::OnlineOnly => "\u{25A0} Online only",
            Tone::Problem => "\u{25A0} Problem",
        }
    }

    fn text(self) -> COLORREF {
        match self {
            Tone::On => rgb(0x1B, 0x6B, 0x2A),
            Tone::Off => rgb(0x80, 0x80, 0x80),
            Tone::Update => rgb(0x8A, 0x5A, 0x00),
            Tone::OnlineOnly => rgb(0x1E, 0x5A, 0xA8),
            Tone::Problem => rgb(0xB0, 0x1E, 0x1E),
        }
    }

    fn back(self) -> COLORREF {
        match self {
            Tone::On => rgb(0xFF, 0xFF, 0xFF),
            Tone::Off => rgb(0xF6, 0xF6, 0xF6),
            Tone::Update => rgb(0xFF, 0xF4, 0xD1),
            Tone::OnlineOnly => rgb(0xE8, 0xF1, 0xFC),
            Tone::Problem => rgb(0xFD, 0xE7, 0xE7),
        }
    }
}

impl Row {
    fn id(&self) -> String {
        if let Some(l) = &self.local {
            if let Ok(m) = &l.manifest {
                return m.id.clone();
            }
            if let Some(Ok(b)) = &l.bundle {
                return b.id.clone();
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

    fn toggleable(&self) -> bool {
        self.local.as_ref().is_some_and(|l| l.manifest.is_ok() || matches!(l.bundle, Some(Ok(_))))
    }

    /// A component bundle's manifest, or why it doesn't parse.
    fn bundle(&self) -> Option<&Result<BundleManifest, String>> {
        self.local.as_ref().and_then(|l| l.bundle.as_ref())
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

    fn needs_install(&self, settings: &LoaderSettings) -> bool {
        matches!(self.online(settings), Online::NotInstalled | Online::UpdateAvailable)
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
    refresh_list();
}

/// Rescan `Mods\` if the loader window is open (e.g. after lobby sync installs).
pub fn refresh_list() {
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

// ---------------------------------------------------------------- visual styles

/// `ACTCTXW` (winbase.h); kept local so no extra windows-sys feature is needed.
#[repr(C)]
struct ActCtxW {
    cb_size: u32,
    dw_flags: u32,
    lp_source: *const u16,
    w_processor_architecture: u16,
    w_lang_id: u16,
    lp_assembly_directory: *const u16,
    lp_resource_name: *const u16,
    lp_application_name: *const u16,
    h_module: *mut c_void,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateActCtxW(ctx: *const ActCtxW) -> *mut c_void;
    fn ActivateActCtx(ctx: *mut c_void, cookie: *mut usize) -> i32;
}

const COMCTL6_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;

/// WA.exe has no common-controls v6 manifest, so its windows draw in the
/// Windows 95 style. Activating one on this thread only gives the loader
/// themed controls without touching WA's own windows.
unsafe fn enable_visual_styles() {
    let path = std::env::temp_dir().join("wormforge-modloader.manifest");
    if let Err(e) = std::fs::write(&path, COMCTL6_MANIFEST) {
        log_error(&format!("mod loader: visual styles manifest: {e}"));
        return;
    }
    let source = wide(&path.to_string_lossy());
    let ctx = ActCtxW {
        cb_size: std::mem::size_of::<ActCtxW>() as u32,
        dw_flags: 0,
        lp_source: source.as_ptr(),
        w_processor_architecture: 0,
        w_lang_id: 0,
        lp_assembly_directory: std::ptr::null(),
        lp_resource_name: std::ptr::null(),
        lp_application_name: std::ptr::null(),
        h_module: std::ptr::null_mut(),
    };
    let handle = CreateActCtxW(&ctx);
    if handle as isize == -1 {
        log_error("mod loader: CreateActCtxW failed; classic controls");
        return;
    }
    let mut cookie = 0usize;
    if ActivateActCtx(handle, &mut cookie) == 0 {
        log_error("mod loader: ActivateActCtx failed; classic controls");
    }
}

unsafe fn ui_font(weight: u32) -> *mut c_void {
    let face = wide("Segoe UI");
    CreateFontW(
        -12,
        0,
        0,
        0,
        weight as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        (DEFAULT_PITCH | FF_DONTCARE) as u32,
        face.as_ptr(),
    )
}

static FONT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static FONT_BOLD: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

unsafe fn ui_thread() {
    enable_visual_styles();
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES,
    };
    InitCommonControlsEx(&icc);
    FONT.store(ui_font(FW_NORMAL), Ordering::SeqCst);
    FONT_BOLD.store(ui_font(FW_SEMIBOLD), Ordering::SeqCst);

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
    let title = wide(&format!("WormForge Mods  \u{00B7}  {BUILD_ID}"));
    let hwnd = CreateWindowExW(
        0,
        class.as_ptr(),
        title.as_ptr(),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        960,
        680,
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
        WM_GETMINMAXINFO => {
            let mm = lparam as *mut MINMAXINFO;
            if !mm.is_null() {
                (*mm).ptMinTrackSize.x = 780;
                (*mm).ptMinTrackSize.y = 520;
            }
            Some(0)
        }
        WM_COMMAND => {
            let id = wparam & 0xFFFF;
            let code = ((wparam >> 16) & 0xFFFF) as u32;
            match id {
                ID_SEARCH if code == EN_CHANGE => {
                    fill_list_from_all();
                }
                ID_SEARCH => return None,
                ID_REFRESH => populate(),
                ID_OPEN => open_folder(hwnd),
                ID_ENABLE_ALL => set_all_shown(true),
                ID_DISABLE_ALL => set_all_shown(false),
                ID_CHECK => check_online(hwnd),
                ID_INSTALL => install_selected(hwnd),
                ID_UPDATE_ALL => install_all(hwnd),
                ID_TAB_ALL => set_filter(0),
                ID_TAB_WEAPONS => set_filter(1),
                ID_TAB_GAMEPLAY => set_filter(2),
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
            if hdr.is_null() || (*hdr).idFrom != ID_LIST {
                return None;
            }
            match (*hdr).code {
                LVN_ITEMCHANGED => {
                    on_item_changed(&*(lparam as *const NMLISTVIEW));
                    Some(0)
                }
                NM_CUSTOMDRAW => Some(custom_draw(&mut *(lparam as *mut NMLVCUSTOMDRAW))),
                NM_DBLCLK => {
                    on_double_click(hwnd, (*(lparam as *const NMITEMACTIVATE)).iItem);
                    Some(0)
                }
                _ => None,
            }
        }
        WM_CTLCOLORSTATIC => colour_static(wparam as HDC, lparam as HWND),
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

/// Status line colour from what it says: red for trouble, amber for updates.
fn status_tone(text: &str) -> Option<Tone> {
    let t = text.to_ascii_lowercase();
    if ["conflict", "error", "failed", "cannot", "could not", "online:", "mismatch"]
        .iter()
        .any(|k| t.contains(k))
    {
        Some(Tone::Problem)
    } else if t.contains("new or updated") || t.contains("with updates") {
        Some(Tone::Update)
    } else if t.starts_with("installed") || t.starts_with("saved") || t.contains("up to date") {
        Some(Tone::On)
    } else {
        None
    }
}

unsafe fn colour_static(hdc: HDC, ctl: HWND) -> Option<LRESULT> {
    let hwnd = WINDOW.load(Ordering::SeqCst);
    let id = (0..Tone::ALL.len())
        .find(|i| GetDlgItem(hwnd, (ID_LEGEND + i) as i32) == ctl)
        .map(|i| Tone::ALL[i]);
    let tone = if let Some(t) = id {
        Some(t)
    } else if ctl == STATUS_LABEL.load(Ordering::SeqCst) {
        status_tone(&STATUS.lock().map(|g| g.clone()).unwrap_or_default())
    } else if ctl == PEER_LABEL.load(Ordering::SeqCst) {
        status_tone(&PEER.lock().map(|g| g.clone()).unwrap_or_default())
    } else {
        return None;
    };
    if let Some(t) = tone {
        SetTextColor(hdc, t.text());
    }
    SetBkMode(hdc, TRANSPARENT as i32);
    Some(GetSysColorBrush(COLOR_BTNFACE) as LRESULT)
}

unsafe fn custom_draw(cd: &mut NMLVCUSTOMDRAW) -> LRESULT {
    match cd.nmcd.dwDrawStage {
        CDDS_PREPAINT => CDRF_NOTIFYITEMDRAW as LRESULT,
        CDDS_ITEMPREPAINT => {
            if let Some(tone) = TONES.lock().ok().and_then(|t| t.get(cd.nmcd.dwItemSpec).copied()) {
                cd.clrText = tone.text();
                cd.clrTextBk = tone.back();
            }
            CDRF_DODEFAULT as LRESULT
        }
        _ => CDRF_DODEFAULT as LRESULT,
    }
}

unsafe fn create_children(parent: HWND) {
    let hinst = GetModuleHandleW(std::ptr::null());
    let font = FONT.load(Ordering::SeqCst);
    let child_ex = |ex: u32, class: *const u16, text: &str, style: u32, id: usize| -> HWND {
        let t = wide(text);
        let h = CreateWindowExW(
            ex,
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
    let child = |class: *const u16, text: &str, style: u32, id: usize| child_ex(0, class, text, style, id);
    let button = wide("BUTTON");
    let static_class = wide("STATIC");
    let edit = wide("EDIT");

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

    // Tabs: push-like radio buttons, so the active one stays pressed.
    for (i, (id, label)) in TAB_BUTTONS.iter().enumerate() {
        let style = WS_TABSTOP | (BS_AUTORADIOBUTTON | BS_PUSHLIKE) as u32 | if i == 0 { WS_GROUP } else { 0 };
        child(button.as_ptr(), label, style, *id);
    }
    let search = child_ex(
        WS_EX_CLIENTEDGE,
        edit.as_ptr(),
        "",
        WS_TABSTOP | WS_GROUP | ES_AUTOHSCROLL as u32,
        ID_SEARCH,
    );
    let cue = wide("Search name, author, slot or status…");
    SendMessageW(search, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);

    let list = child_ex(
        WS_EX_CLIENTEDGE,
        WC_LISTVIEWW,
        "",
        WS_TABSTOP | LVS_REPORT | LVS_SHOWSELALWAYS,
        ID_LIST,
    );
    SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
    );
    LIST.store(list, Ordering::SeqCst);
    for (i, (name, width)) in [
        ("Mod", 190),
        ("Version", 90),
        ("Author", 110),
        ("Slot / role", 150),
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

    for (i, tone) in Tone::ALL.iter().enumerate() {
        child(static_class.as_ptr(), tone.legend(), 0, ID_LEGEND + i);
    }

    let bold = FONT_BOLD.load(Ordering::SeqCst);
    for (id, label) in [(ID_GROUP_PACKS, "Packs"), (ID_GROUP_ONLINE, "Online catalogue")] {
        let g = child(button.as_ptr(), label, BS_GROUPBOX as u32, id);
        SendMessageW(g, WM_SETFONT, bold as usize, 1);
    }
    for (id, label) in PACK_BUTTONS.iter().chain(ONLINE_BUTTONS.iter()) {
        child(button.as_ptr(), label, WS_TABSTOP, *id);
    }

    let status = child(static_class.as_ptr(), "", 0, ID_STATUS);
    SendMessageW(status, WM_SETFONT, bold as usize, 1);
    STATUS_LABEL.store(status, Ordering::SeqCst);
    let peer = child(static_class.as_ptr(), "", SS_ENDELLIPSIS, ID_PEER);
    PEER_LABEL.store(peer, Ordering::SeqCst);
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

/// Lay out `ids` as equal-width buttons inside a group box at `(x, y, w)`.
unsafe fn layout_group(hwnd: HWND, group: usize, ids: &[usize], x: i32, y: i32, w: i32) {
    let ctl = |id: usize| -> HWND { GetDlgItem(hwnd, id as i32) };
    let (inner, gap, button_h) = (10, 6, 28);
    MoveWindow(ctl(group), x, y, w, button_h + 34, 1);
    let n = ids.len() as i32;
    let bw = ((w - 2 * inner - (n - 1) * gap) / n).max(60);
    for (i, id) in ids.iter().enumerate() {
        MoveWindow(ctl(*id), x + inner + i as i32 * (bw + gap), y + 22, bw, button_h, 1);
    }
}

unsafe fn layout(hwnd: HWND) {
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;
    let pad = 12;
    let line_h = 18;
    let (bw, bh) = (BANNER_W.load(Ordering::SeqCst), BANNER_H.load(Ordering::SeqCst));
    let ctl = |id: usize| -> HWND { GetDlgItem(hwnd, id as i32) };

    // Header banner, centred, natural size.
    MoveWindow(ctl(ID_BANNER), ((w - bw) / 2).max(0), pad, bw, bh, 1);
    let mut y = pad + bh + pad;

    // Tabs left, search right.
    let (tab_w, tab_h) = (120, 28);
    for (i, (id, _)) in TAB_BUTTONS.iter().enumerate() {
        MoveWindow(ctl(*id), pad + i as i32 * (tab_w + 4), y, tab_w, tab_h, 1);
    }
    let search_w = (w - 2 * pad - 3 * (tab_w + 4) - pad).clamp(160, 320);
    MoveWindow(ctl(ID_SEARCH), w - pad - search_w, y + 2, search_w, tab_h - 4, 1);
    y += tab_h + 8;

    // List fills the middle; legend, action groups and status sit below it.
    let group_h = 28 + 34;
    let footer = 6 + line_h + 8 + group_h + 8 + 2 * line_h + line_h + pad;
    let list_h = (h - y - footer).max(60);
    let list = ctl(ID_LIST);
    MoveWindow(list, pad, y, w - 2 * pad, list_h, 1);
    SendMessageW(list, LVM_SETCOLUMNWIDTH, 4, LVSCW_AUTOSIZE_USEHEADER as isize);
    y += list_h + 6;

    let mut x = pad;
    for (i, tone) in Tone::ALL.iter().enumerate() {
        let lw = 18 + 7 * tone.legend().chars().count() as i32;
        MoveWindow(ctl(ID_LEGEND + i), x, y, lw, line_h, 1);
        x += lw + 14;
    }
    y += line_h + 8;

    let avail = w - 3 * pad;
    let left_w = avail * 4 / 7;
    let pack_ids: Vec<usize> = PACK_BUTTONS.iter().map(|(id, _)| *id).collect();
    let online_ids: Vec<usize> = ONLINE_BUTTONS.iter().map(|(id, _)| *id).collect();
    layout_group(hwnd, ID_GROUP_PACKS, &pack_ids, pad, y, left_w);
    layout_group(hwnd, ID_GROUP_ONLINE, &online_ids, 2 * pad + left_w, y, avail - left_w);
    y += group_h + 8;

    MoveWindow(ctl(ID_STATUS), pad, y, w - 2 * pad, 2 * line_h, 1);
    MoveWindow(ctl(ID_PEER), pad, y + 2 * line_h, w - 2 * pad, line_h, 1);
}

unsafe fn set_filter(filter: usize) {
    FILTER.store(filter, Ordering::SeqCst);
    fill_list_from_all();
}

/// Tab labels carry their pack counts; the active tab stays pressed.
unsafe fn paint_tabs(hwnd: HWND, all: &[Row]) {
    let filter = FILTER.load(Ordering::SeqCst);
    for (i, (id, label)) in TAB_BUTTONS.iter().enumerate() {
        let n = all.iter().filter(|r| row_matches_filter(r, i)).count();
        let tab = GetDlgItem(hwnd, *id as i32);
        SetWindowTextW(tab, wide(&format!("{label}  ({n})")).as_ptr());
        SendMessageW(tab, BM_SETCHECK, (i == filter) as usize, 0);
    }
}

unsafe fn set_network_buttons(hwnd: HWND, enabled: bool) {
    for id in NETWORK_BUTTONS {
        EnableWindow(GetDlgItem(hwnd, id as i32), enabled as i32);
    }
    if enabled {
        update_buttons();
    }
}

/// Grey out actions that would do nothing, and give the next useful one the
/// default-button accent: Check online first, then Update all.
unsafe fn update_buttons() {
    let hwnd = WINDOW.load(Ordering::SeqCst);
    if hwnd.is_null() {
        return;
    }
    let busy = BUSY.load(Ordering::SeqCst);
    let settings = LoaderSettings::load(&mods_dir());
    let fetched = REMOTE.lock().map(|g| g.is_some()).unwrap_or(false);
    let pending = ALL_ROWS
        .lock()
        .map(|rows| rows.iter().filter(|r| r.needs_install(&settings)).count())
        .unwrap_or(0);
    let selectable = selected_rows().iter().any(|r| r.remote.is_some());
    let shown_toggleable = ROWS
        .lock()
        .map(|rows| rows.iter().any(Row::toggleable))
        .unwrap_or(false);
    let enable = |id: usize, on: bool| {
        EnableWindow(GetDlgItem(hwnd, id as i32), on as i32);
    };
    enable(ID_CHECK, !busy);
    enable(ID_INSTALL, !busy && selectable);
    enable(ID_UPDATE_ALL, !busy && pending > 0);
    enable(ID_ENABLE_ALL, shown_toggleable);
    enable(ID_DISABLE_ALL, shown_toggleable);
    let update_label = if pending > 0 {
        format!("Update all ({pending})")
    } else {
        "Update all".to_string()
    };
    SetWindowTextW(GetDlgItem(hwnd, ID_UPDATE_ALL as i32), wide(&update_label).as_ptr());
    let accent = |id: usize, on: bool| {
        let style = if on { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON };
        SendMessageW(GetDlgItem(hwnd, id as i32), BM_SETSTYLE, style as usize, 1);
    };
    accent(ID_CHECK, !fetched);
    accent(ID_UPDATE_ALL, fetched && pending > 0);
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
    match row.bundle() {
        Some(Ok(b)) => {
            let n = b.components.len();
            return format!("{n} component{}", if n == 1 { "" } else { "s" });
        }
        Some(Err(_)) => return String::new(),
        None => {}
    }
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
        // A bundle can hold weapons, a scheme and features: both tabs.
        1 => row.bundle().is_some() || row.kind() == PackKind::Weapons,
        2 => row.bundle().is_some() || row.kind() == PackKind::Gameplay,
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

/// Per-row status text and colour: manifest errors, missing `requires`,
/// duplicate ids, every slot fight with the packs named, then the online state.
fn row_status(rows: &[Row], conflicts: &[SlotConflict], settings: &LoaderSettings, row: &Row) -> (String, Tone) {
    let state = row.online(settings);
    let online = match state {
        Online::Unknown => String::new(),
        Online::NotInstalled => "online only - double-click or Install selected".into(),
        Online::UpToDate => "up to date".into(),
        Online::UpdateAvailable => format!(
            "update available (online v{})",
            row.remote.as_ref().map(|r| r.version.as_str()).unwrap_or("?")
        ),
    };
    let Some(local) = &row.local else {
        return (online, Tone::OnlineOnly);
    };
    match &local.bundle {
        Some(Err(e)) => return (format!("error: {e}"), Tone::Problem),
        // What runs is the profile's choice; the checkbox only makes the
        // bundle's components available to it.
        Some(Ok(_)) if local.enabled => return ("on (components the profile selects run)".into(), Tone::On),
        Some(Ok(_)) => return ("off".into(), Tone::Off),
        None => {}
    }
    let m = match &local.manifest {
        Ok(m) => m,
        Err(e) => return (format!("error: {e}"), Tone::Problem),
    };
    let mut parts = Vec::new();
    let mut problem = false;
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
                problem = true;
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
            problem = true;
        }
        let conflict = conflict_text(conflicts, &m.id);
        if !conflict.is_empty() {
            parts.push(format!("CONFLICT: {conflict}"));
            problem = true;
        }
        if parts.is_empty() {
            parts.push("on".to_string());
        }
    }
    if !online.is_empty() {
        parts.push(online);
    }
    let tone = if problem {
        Tone::Problem
    } else if state == Online::UpdateAvailable {
        Tone::Update
    } else if local.enabled {
        Tone::On
    } else {
        Tone::Off
    };
    (parts.join(" | "), tone)
}

/// "N of M packs enabled" plus a conflict count and the contested slots.
fn summary_line(rows: &[Row], conflicts: &[SlotConflict], settings: &LoaderSettings) -> String {
    let total = rows.iter().filter(|r| r.local.is_some()).count();
    let enabled = rows.iter().filter(|r| r.enabled()).count();
    let mut s = format!("{enabled} of {total} packs enabled.");
    let updates = rows.iter().filter(|r| r.needs_install(settings)).count();
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

unsafe fn item_checked(list: HWND, item: i32) -> bool {
    let state = SendMessageW(list, LVM_GETITEMSTATE, item as usize, LVIS_STATEIMAGEMASK as isize);
    ((state as u32 & LVIS_STATEIMAGEMASK) >> 12) == 2
}

/// Rescan `Mods\`, merge the cached index, rebuild every row.
unsafe fn populate() {
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
    fill_list_from_all();
}

fn search_text() -> String {
    let hwnd = WINDOW.load(Ordering::SeqCst);
    if hwnd.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(GetDlgItem(hwnd, ID_SEARCH as i32), buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize]).trim().to_lowercase()
}

/// The text a row shows in each column, for display and for search.
fn row_columns(row: &Row, settings: &LoaderSettings) -> Option<[String; 4]> {
    let (name, version, author) = match (&row.local, &row.remote) {
        (Some(l), remote) => match &l.manifest {
            Ok(m) => {
                let version = match remote {
                    Some(r) if r.version != m.version && row.online(settings) == Online::UpdateAvailable => {
                        format!("{} \u{2192} {}", m.version, r.version)
                    }
                    _ => m.version.clone(),
                };
                (display_name(&m.id), version, m.author.clone())
            }
            Err(_) => match &l.bundle {
                Some(Ok(b)) => (display_name(&b.id), b.version.clone(), b.author.clone()),
                _ => (format!("{} (broken mod.toml)", l.folder), String::new(), String::new()),
            },
        },
        (None, Some(r)) => (display_name(&r.id), r.version.clone(), r.author.clone()),
        (None, None) => return None,
    };
    Some([name, version, author, role_column(row)])
}

/// Paint the current tab + search from `ALL_ROWS` into the list view and
/// return the summary line it set.
unsafe fn fill_list_from_all() -> String {
    let list = LIST.load(Ordering::SeqCst);
    let hwnd = WINDOW.load(Ordering::SeqCst);
    if list.is_null() {
        return String::new();
    }
    let dir = mods_dir();
    let settings = LoaderSettings::load(&dir);
    let filter = FILTER.load(Ordering::SeqCst);
    let query = search_text();
    let all = ALL_ROWS.lock().map(|g| g.clone()).unwrap_or_default();
    let conflicts = slot_conflicts(&local_entries(&all));
    if !hwnd.is_null() {
        paint_tabs(hwnd, &all);
    }

    let mut rows = Vec::new();
    let mut tones = Vec::new();
    let mut shown = Vec::new();
    for row in all.iter().filter(|r| row_matches_filter(r, filter)) {
        let Some(cols) = row_columns(row, &settings) else {
            continue;
        };
        // Conflicts / requires use the full set so a Weapons-tab pack still
        // shows fights against a Gameplay pack that is enabled but hidden.
        let (status, tone) = row_status(&all, &conflicts, &settings, row);
        if !query.is_empty()
            && !cols.iter().chain(std::iter::once(&status)).any(|c| c.to_lowercase().contains(&query))
        {
            continue;
        }
        rows.push(row.clone());
        tones.push(tone);
        shown.push((cols, status));
    }
    if let Ok(mut g) = TONES.lock() {
        *g = tones;
    }

    POPULATING.store(true, Ordering::SeqCst);
    SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
    for (i, ((cols, status), row)) in shown.iter().zip(rows.iter()).enumerate() {
        let i = i as i32;
        for (c, text) in cols.iter().enumerate() {
            set_item_text(list, i, c as i32, text);
        }
        set_item_text(list, i, 4, status);
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
    } else if rows.is_empty() && !query.is_empty() {
        format!("Nothing in {tab} matches \"{query}\".")
    } else if rows.is_empty() {
        format!("No {tab} packs. Try another tab or Check online.")
    } else {
        summary_line(&all, &conflicts, &settings)
    };
    if let Ok(mut g) = ROWS.lock() {
        *g = rows;
    }
    set_status(&summary);
    update_buttons();
    summary
}

unsafe fn refresh_status_column() {
    let list = LIST.load(Ordering::SeqCst);
    let Ok(rows) = ROWS.lock() else {
        return;
    };
    let all = ALL_ROWS.lock().map(|g| g.clone()).unwrap_or_else(|_| rows.clone());
    let settings = LoaderSettings::load(&mods_dir());
    let conflicts = slot_conflicts(&local_entries(&all));
    let mut tones = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let (status, tone) = row_status(&all, &conflicts, &settings, row);
        set_item_text(list, i as i32, 4, &status);
        tones.push(tone);
    }
    drop(rows);
    if let Ok(mut g) = TONES.lock() {
        *g = tones;
    }
    InvalidateRect(list, std::ptr::null(), 1);
    set_status(&format!("Saved. {}", summary_line(&all, &conflicts, &settings)));
}

/// "Enable all" / "Disable all": every pack the list shows right now (tab and
/// search applied), saved in one write.
unsafe fn set_all_shown(checked: bool) {
    let ids: Vec<String> = ROWS
        .lock()
        .map(|rows| rows.iter().filter(|r| r.toggleable()).map(Row::id).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return;
    }
    let dir = mods_dir();
    let mut settings = LoaderSettings::load(&dir);
    for id in &ids {
        settings.set_enabled(id, checked);
    }
    if let Err(e) = settings.save(&dir) {
        set_status(&format!("Could not write {}: {e}", weapons::SETTINGS_FILE));
        return;
    }
    if let Ok(mut all) = ALL_ROWS.lock() {
        for row in all.iter_mut().filter(|r| ids.contains(&r.id())) {
            if let Some(l) = row.local.as_mut() {
                l.enabled = checked;
            }
        }
    }
    log_line(&format!(
        "mod loader: {} {} pack(s)",
        if checked { "enabled" } else { "disabled" },
        ids.len()
    ));
    let summary = fill_list_from_all();
    set_status(&format!(
        "Saved: {} {} pack(s). {summary}",
        if checked { "enabled" } else { "disabled" },
        ids.len()
    ));
}

unsafe fn on_item_changed(nm: &NMLISTVIEW) {
    if POPULATING.load(Ordering::SeqCst) || nm.uChanged & LVIF_STATE == 0 {
        return;
    }
    if (nm.uOldState ^ nm.uNewState) & LVIS_SELECTED != 0 {
        update_buttons();
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
        let row_id = row.id();
        match row.local.as_mut() {
            None => Outcome::NotInstalled,
            Some(local) => match local.manifest.is_ok() || matches!(local.bundle, Some(Ok(_))) {
                true => {
                    local.enabled = checked;
                    let id = row_id;
                    if let Ok(mut all) = ALL_ROWS.lock() {
                        if let Some(full) = all.iter_mut().find(|r| r.id() == id) {
                            if let Some(l) = full.local.as_mut() {
                                l.enabled = checked;
                            }
                        }
                    }
                    Outcome::Toggle(id)
                }
                false => Outcome::Broken,
            },
        }
    };
    let id = match outcome {
        Outcome::Toggle(id) => id,
        Outcome::Broken => {
            if checked {
                set_item_checked(LIST.load(Ordering::SeqCst), nm.iItem, false);
            }
            set_status("Fix that pack's mod.toml before enabling it.");
            return;
        }
        Outcome::NotInstalled => {
            if checked {
                set_item_checked(LIST.load(Ordering::SeqCst), nm.iItem, false);
            }
            set_status("That pack is online only. Double-click it or press Install selected.");
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

/// Double-click: toggle an installed pack, install an online-only one.
unsafe fn on_double_click(hwnd: HWND, item: i32) {
    if item < 0 {
        return;
    }
    let Some(row) = ROWS.lock().ok().and_then(|r| r.get(item as usize).cloned()) else {
        return;
    };
    if row.local.is_none() {
        install_rows(hwnd, vec![row]);
        return;
    }
    let list = LIST.load(Ordering::SeqCst);
    set_item_checked(list, item, !item_checked(list, item));
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
        Ok(if new + upd == 0 {
            format!("Online index has {} packs; everything is up to date.", idx.packs.len())
        } else {
            format!(
                "Online index has {} packs ({}): {new} not installed, {upd} with updates.",
                idx.packs.len(),
                if idx.generated.is_empty() { "no date" } else { idx.generated.as_str() }
            )
        })
    });
}

fn selected_rows() -> Vec<Row> {
    let list = LIST.load(Ordering::SeqCst);
    if list.is_null() {
        return Vec::new();
    }
    let Ok(rows) = ROWS.lock() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut i: isize = -1;
    loop {
        i = unsafe { SendMessageW(list, LVM_GETNEXTITEM, i as usize, LVNI_SELECTED as isize) };
        if i < 0 {
            break;
        }
        if let Some(r) = rows.get(i as usize) {
            out.push(r.clone());
        }
    }
    out
}

unsafe fn install_rows(hwnd: HWND, rows: Vec<Row>) {
    let Some(index) = REMOTE.lock().ok().and_then(|g| g.clone()) else {
        set_status("Press Check online first.");
        return;
    };
    let packs: Vec<RemotePack> = rows.into_iter().filter_map(|r| r.remote).collect();
    if packs.is_empty() {
        set_status("Those packs are not in the online index; nothing to install.");
        return;
    }
    run_network(hwnd, move || install_packs(&index, &packs));
}

unsafe fn install_selected(hwnd: HWND) {
    let rows = selected_rows();
    if rows.is_empty() {
        set_status("Select one or more packs in the list first.");
        return;
    }
    install_rows(hwnd, rows);
}

unsafe fn install_all(hwnd: HWND) {
    let settings = LoaderSettings::load(&mods_dir());
    let todo: Vec<Row> = ALL_ROWS
        .lock()
        .map(|rows| rows.iter().filter(|r| r.needs_install(&settings)).cloned().collect())
        .unwrap_or_default();
    if todo.is_empty() {
        set_status("Everything is up to date.");
        return;
    }
    install_rows(hwnd, todo);
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
    let sha = assets::sha256::hex(&bytes);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_row(enabled: bool, bundle: Result<BundleManifest, String>) -> Row {
        Row {
            local: Some(ModEntry {
                folder: "component_demo".into(),
                root: std::path::PathBuf::new(),
                manifest: Err(weapons::BUNDLE_NOT_LOADED.into()),
                enabled,
                bundle: Some(bundle),
            }),
            remote: None,
        }
    }

    fn demo_manifest() -> BundleManifest {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/examples/component_demo/mod.toml"),
        )
        .unwrap();
        weapons::components::parse_bundle(&src).unwrap()
    }

    #[test]
    fn a_component_bundle_is_a_row_that_can_be_toggled() {
        let settings = LoaderSettings::default();
        let row = demo_row(true, Ok(demo_manifest()));
        assert_eq!(row.id(), "example.components");
        assert!(row.toggleable());
        assert!(row_matches_filter(&row, 1) && row_matches_filter(&row, 2));
        let cols = row_columns(&row, &settings).unwrap();
        assert_eq!((cols[0].as_str(), cols[3].as_str()), ("components", "3 components"));
        let rows = [demo_row(true, Ok(demo_manifest()))];
        assert_eq!(row_status(&rows, &[], &settings, &row).0, "on (components the profile selects run)");
        let off = demo_row(false, Ok(demo_manifest()));
        let (text, tone) = row_status(&rows, &[], &settings, &off);
        assert!(text == "off" && tone == Tone::Off, "{text}");
    }

    #[test]
    fn a_broken_bundle_shows_its_error() {
        let settings = LoaderSettings::default();
        let row = demo_row(false, Err("demo.components: no entry".into()));
        assert!(!row.toggleable());
        assert_eq!(row_columns(&row, &settings).unwrap()[0], "component_demo (broken mod.toml)");
        assert_eq!(row_status(&[], &[], &settings, &row).0, "error: demo.components: no entry");
    }
}
