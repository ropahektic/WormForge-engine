//! The loader window's Profile tab: the component selection a match runs,
//! edited in place and saved to `[profile]` in `Mods\wormforge.toml` at every
//! change, as pack toggles are. The next match (or lobby freeze) reads it.
//!
//! Scheme dropdown on top; the loadout (left) and the features (right) below,
//! each with check boxes; under them the selected item's settings: a weapon's
//! position dropdown, or a feature's options. The search box lists the whole
//! weapon catalog instead of the loadout. Editing goes through
//! `weapons::profile_editor`, so the window shows what `resolve` will run.

use crate::log::{log_error, log_line};
use crate::modloader::{
    item_checked, mods_dir, search_text, set_item_checked, set_item_text, set_status, wide, Tone,
    FONT, FONT_BOLD, WINDOW,
};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use weapons::components::{
    list_bundles, ComponentKind, OptionDecl, OptionKind, OptionValue, Profile, ProfileSpec,
};
use weapons::panel::PanelPosition;
use weapons::profile_editor::{Clash, Editor};
use weapons::LoaderSettings;
use windows_sys::Win32::Foundation::{HWND, LRESULT};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemServices::SS_ENDELLIPSIS;
use windows_sys::Win32::UI::Controls::{
    BST_CHECKED, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_STATE, LVIS_SELECTED, LVIS_STATEIMAGEMASK,
    LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW, LVM_SETEXTENDEDLISTVIEWSTYLE, LVN_ITEMCHANGED, LVS_EX_CHECKBOXES, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT,
    LVS_EX_LABELTIP, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMHDR, NMLISTVIEW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetDlgItem, GetWindowTextW, MessageBoxW,
    MoveWindow, PostMessageW, SendMessageW, SetWindowTextW, ShowWindow, BM_GETCHECK, BM_SETCHECK,
    BN_CLICKED, BS_AUTOCHECKBOX, CBN_SELCHANGE, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL,
    CB_RESETCONTENT, CB_SETCURSEL, EN_KILLFOCUS, ES_AUTOHSCROLL, IDYES, MB_ICONQUESTION, MB_YESNO,
    SW_HIDE, SW_SHOW, WM_APP, WM_SETFONT, WS_CHILD, WS_EX_CLIENTEDGE, WS_TABSTOP, WS_VSCROLL,
};

pub(crate) const ID_TAB_PROFILE: usize = 118;
/// The tab's index in the loader's filter (after All, Weapons, Gameplay).
pub(crate) const FILTER_PROFILE: usize = 3;
/// Rebuild the tab's lists outside the list view's own notification.
pub(crate) const WM_APP_PROFILE: u32 = WM_APP + 4;

const ID_SCHEME_LABEL: usize = 130;
const ID_SCHEME: usize = 131;
const ID_WEAPONS: usize = 132;
const ID_FEATURES: usize = 133;
const ID_DETAIL: usize = 134;
const ID_POSITION: usize = 135;
const ID_PROFILE_STATUS: usize = 136;
/// Option controls: a label at `ID_OPTION_BASE + 2i`, its control at `+ 2i + 1`.
const ID_OPTION_BASE: usize = 140;
const MAX_OPTIONS: usize = 12;
const FIXED_IDS: [usize; 7] = [ID_SCHEME_LABEL, ID_SCHEME, ID_WEAPONS, ID_FEATURES, ID_DETAIL, ID_POSITION, ID_PROFILE_STATUS];

#[derive(Clone, PartialEq)]
enum Detail {
    None,
    Weapon(String),
    Feature(String),
}

struct State {
    editor: Editor,
    /// Why the saved `[profile]` (or a bundle) could not be read.
    load_errors: Vec<String>,
    schemes: Vec<Option<String>>,
    weapons: Vec<String>,
    features: Vec<String>,
    positions: Vec<PanelPosition>,
    options: Vec<OptionDecl>,
    detail: Detail,
    status: (String, Tone),
    /// Where the tab lays out: x, y, width, height.
    area: (i32, i32, i32, i32),
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// The status line's colour, apart from `STATE`: Windows asks for it while the
/// line's text is set, which happens with `STATE` held.
static STATUS_TONE: Mutex<Option<Tone>> = Mutex::new(None);
/// Set while the tab fills its own controls, so their notifications are not
/// taken for the player's edits.
static FILLING: AtomicBool = AtomicBool::new(false);

/// Run `f` on the tab's state. Windows delivers some messages synchronously
/// while the tab is rebuilding with the state held; such a re-entrant call gets
/// `None` instead of deadlocking the window's thread.
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    match STATE.try_lock() {
        Ok(mut g) => g.as_mut().map(f),
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().as_mut().map(f),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

unsafe fn ctl(id: usize) -> HWND {
    GetDlgItem(WINDOW.load(Ordering::SeqCst), id as i32)
}

unsafe fn make(parent: HWND, ex: u32, class: &str, text: &str, style: u32, id: usize) -> HWND {
    let (c, t) = (wide(class), wide(text));
    let h = CreateWindowExW(
        ex,
        c.as_ptr(),
        t.as_ptr(),
        WS_CHILD | style,
        0,
        0,
        10,
        10,
        parent,
        id as *mut c_void,
        GetModuleHandleW(std::ptr::null()),
        std::ptr::null(),
    );
    SendMessageW(h, WM_SETFONT, FONT.load(Ordering::SeqCst) as usize, 1);
    h
}

unsafe fn add_columns(list: HWND, columns: &[(&str, i32)]) {
    for (i, (name, width)) in columns.iter().enumerate() {
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
}

/// The tab's controls, hidden until the tab is shown.
pub(crate) unsafe fn create(parent: HWND) {
    make(parent, 0, "STATIC", "Scheme:", 0, ID_SCHEME_LABEL);
    make(parent, 0, "COMBOBOX", "", WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32, ID_SCHEME);
    for (id, columns) in [
        (ID_WEAPONS, &[("Weapon", 170), ("Position", 90), ("Stock slot", 110), ("Bundle", 130)][..]),
        (ID_FEATURES, &[("Feature", 150), ("Bundle", 110), ("Settings", 200)][..]),
    ] {
        let list = make(
            parent,
            WS_EX_CLIENTEDGE,
            "SysListView32",
            "",
            WS_TABSTOP | LVS_REPORT | LVS_SHOWSELALWAYS | LVS_SINGLESEL,
            id,
        );
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
        );
        add_columns(list, columns);
    }
    let detail = make(parent, 0, "STATIC", "", SS_ENDELLIPSIS, ID_DETAIL);
    SendMessageW(detail, WM_SETFONT, FONT_BOLD.load(Ordering::SeqCst) as usize, 1);
    make(parent, 0, "COMBOBOX", "", WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32, ID_POSITION);
    let status = make(parent, 0, "STATIC", "", SS_ENDELLIPSIS, ID_PROFILE_STATUS);
    SendMessageW(status, WM_SETFONT, FONT_BOLD.load(Ordering::SeqCst) as usize, 1);
}

/// Show or hide the tab; showing it reloads the bundles and `[profile]`.
pub(crate) unsafe fn show(on: bool) {
    let cmd = if on { SW_SHOW } else { SW_HIDE };
    for id in FIXED_IDS {
        ShowWindow(ctl(id), cmd);
    }
    for i in 0..2 * MAX_OPTIONS {
        let h = ctl(ID_OPTION_BASE + i);
        if !h.is_null() {
            ShowWindow(h, cmd);
        }
    }
    if on {
        load();
        refresh();
    }
}

fn read_profile() -> (Editor, Vec<String>) {
    let dir = mods_dir();
    let (bundles, mut errors) = list_bundles(&dir);
    let profile = match LoaderSettings::load(&dir).profile {
        None => Profile::default(),
        Some(table) => match ProfileSpec::from_table(&table).map_err(|e| vec![e]).and_then(|s| s.to_profile()) {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!("[profile] in {} can't be read ({}); editing here replaces it", weapons::SETTINGS_FILE, e.join("; ")));
                Profile::default()
            }
        },
    };
    (Editor::new(bundles.into_iter().map(|b| b.manifest).collect(), profile), errors)
}

unsafe fn load() {
    let (editor, load_errors) = read_profile();
    if let Ok(mut g) = STATE.try_lock() {
        let (area, detail) = g.as_ref().map(|s| (s.area, s.detail.clone())).unwrap_or(((0, 0, 0, 0), Detail::None));
        *g = Some(State {
            editor,
            load_errors,
            schemes: Vec::new(),
            weapons: Vec::new(),
            features: Vec::new(),
            positions: Vec::new(),
            options: Vec::new(),
            detail,
            status: (String::new(), Tone::On),
            area,
        });
    }
}

/// Write the edited selection to `[profile]`.
fn save(editor: &Editor) {
    let dir = mods_dir();
    let mut settings = LoaderSettings::load(&dir);
    settings.profile = (!editor.is_empty()).then(|| editor.to_table());
    match settings.save(&dir) {
        Ok(()) => log_line("mod loader: profile saved"),
        Err(e) => {
            log_error(&format!("mod loader: cannot save profile: {e}"));
            set_status(&format!("Cannot save the profile: {e}"));
        }
    }
}

/// Apply an edit, save it, and rebuild the tab after the current message.
fn edit(f: impl FnOnce(&mut Editor)) {
    with(|s| {
        f(&mut s.editor);
        s.load_errors.retain(|e| !e.starts_with("[profile]"));
        save(&s.editor);
    });
    unsafe {
        PostMessageW(WINDOW.load(Ordering::SeqCst), WM_APP_PROFILE, 0, 0);
    }
}

fn position_text(p: PanelPosition) -> String {
    p.to_string().trim_matches(|c| c == '(' || c == ')').replace(", ", " ")
}

fn value_text(v: &OptionValue) -> String {
    match v {
        OptionValue::Bool(b) => if *b { "on" } else { "off" }.into(),
        OptionValue::Int(i) => i.to_string(),
        OptionValue::Choice(c) => c.clone(),
    }
}

/// `WM_APP_PROFILE`: `wparam` 1 redraws only the settings under the lists (a
/// row was selected; rebuilding the lists would lose their scroll position).
pub(crate) unsafe fn on_message(wparam: usize) {
    if wparam == 1 {
        let area = with(|s| {
            FILLING.store(true, Ordering::SeqCst);
            fill_detail(s);
            FILLING.store(false, Ordering::SeqCst);
            s.area
        });
        if let Some(area) = area {
            layout_area(area);
        }
    } else {
        refresh();
    }
}

/// Rebuild every control from the editor.
pub(crate) unsafe fn refresh() {
    let Ok(mut g) = STATE.try_lock() else {
        return;
    };
    let Some(s) = g.as_mut() else {
        return;
    };
    FILLING.store(true, Ordering::SeqCst);
    fill_schemes(s);
    fill_weapons(s);
    fill_features(s);
    fill_detail(s);
    s.status = status_line(s);
    if let Ok(mut t) = STATUS_TONE.lock() {
        *t = Some(s.status.1);
    }
    SetWindowTextW(ctl(ID_PROFILE_STATUS), wide(&s.status.0).as_ptr());
    FILLING.store(false, Ordering::SeqCst);
    let area = s.area;
    drop(g);
    layout_area(area);
}

unsafe fn fill_schemes(s: &mut State) {
    let combo = ctl(ID_SCHEME);
    SendMessageW(combo, CB_RESETCONTENT, 0, 0);
    s.schemes = vec![None];
    SendMessageW(combo, CB_ADDSTRING, 0, wide("(no scheme: only the weapons and features below)").as_ptr() as isize);
    for e in s.editor.catalog(ComponentKind::Scheme) {
        s.schemes.push(Some(e.component.id.clone()));
        let text = format!("{}  ({})", e.name(), e.bundle.id);
        SendMessageW(combo, CB_ADDSTRING, 0, wide(&text).as_ptr() as isize);
    }
    let current = s.editor.profile.scheme.clone();
    let index = s.schemes.iter().position(|id| *id == current);
    if index.is_none() {
        // A saved scheme that isn't installed: keep it listed, as the status says.
        s.schemes.push(current.clone());
        let text = format!("{} (not installed)", current.as_deref().unwrap_or(""));
        SendMessageW(combo, CB_ADDSTRING, 0, wide(&text).as_ptr() as isize);
    }
    SendMessageW(combo, CB_SETCURSEL, index.unwrap_or(s.schemes.len() - 1), 0);
}

unsafe fn fill_weapons(s: &mut State) {
    let list = ctl(ID_WEAPONS);
    let query = search_text();
    let loadout = s.editor.loadout();
    let ids: Vec<String> = if query.is_empty() {
        loadout.iter().map(|p| p.id.clone()).collect()
    } else {
        s.editor
            .catalog(ComponentKind::Weapon)
            .iter()
            .filter(|e| {
                [e.name(), e.component.id.as_str(), e.bundle.id.as_str()]
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query))
            })
            .map(|e| e.component.id.clone())
            .collect()
    };
    SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
    for (i, id) in ids.iter().enumerate() {
        let i = i as i32;
        let Some(e) = s.editor.entry(id) else {
            continue;
        };
        let placed = loadout.iter().find(|p| &p.id == id);
        let position = placed.map(|p| p.position).or_else(|| e.component.compatible_positions.first().copied());
        set_item_text(list, i, 0, e.name());
        set_item_text(
            list,
            i,
            1,
            &match (placed, position) {
                (Some(_), Some(p)) => position_text(p),
                (None, Some(p)) => format!("({})", position_text(p)),
                _ => "—".into(),
            },
        );
        set_item_text(list, i, 2, position.and_then(|p| p.slot()).map(|w| w.name()).unwrap_or("—"));
        set_item_text(list, i, 3, &e.bundle.id);
        set_item_checked(list, i, placed.is_some());
    }
    s.weapons = ids;
    select_row(list, &s.weapons, &s.detail, |d| matches!(d, Detail::Weapon(_)));
}

unsafe fn fill_features(s: &mut State) {
    let list = ctl(ID_FEATURES);
    SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
    let entries = s.editor.catalog(ComponentKind::Feature);
    s.features = entries.iter().map(|e| e.component.id.clone()).collect();
    for (i, e) in entries.iter().enumerate() {
        let i = i as i32;
        let id = &e.component.id;
        let settings: Vec<String> = e
            .component
            .options
            .iter()
            .map(|o| format!("{} {}", o.label, value_text(&s.editor.option(id, o))))
            .collect();
        set_item_text(list, i, 0, e.name());
        set_item_text(list, i, 1, &e.bundle.id);
        set_item_text(list, i, 2, &settings.join(", "));
        set_item_checked(list, i, s.editor.profile.features.contains(id));
    }
    let features = s.features.clone();
    select_row(list, &features, &s.detail, |d| matches!(d, Detail::Feature(_)));
}

/// Keep the detail's row selected across a rebuild.
unsafe fn select_row(list: HWND, ids: &[String], detail: &Detail, kind: impl Fn(&Detail) -> bool) {
    let id = match detail {
        Detail::Weapon(id) | Detail::Feature(id) if kind(detail) => id,
        _ => return,
    };
    if let Some(i) = ids.iter().position(|x| x == id) {
        let lvi = windows_sys::Win32::UI::Controls::LVITEMW {
            mask: LVIF_STATE,
            iItem: i as i32,
            iSubItem: 0,
            state: LVIS_SELECTED,
            stateMask: LVIS_SELECTED,
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
        SendMessageW(list, windows_sys::Win32::UI::Controls::LVM_SETITEMSTATE, i, &lvi as *const _ as isize);
        SendMessageW(list, windows_sys::Win32::UI::Controls::LVM_ENSUREVISIBLE, i, 0);
    }
}

/// The selected weapon's position dropdown, or the selected feature's options.
unsafe fn fill_detail(s: &mut State) {
    let parent = WINDOW.load(Ordering::SeqCst);
    for i in 0..2 * MAX_OPTIONS {
        let h = ctl(ID_OPTION_BASE + i);
        if !h.is_null() {
            DestroyWindow(h);
        }
    }
    s.options.clear();
    s.positions.clear();
    let combo = ctl(ID_POSITION);
    SendMessageW(combo, CB_RESETCONTENT, 0, 0);
    ShowWindow(combo, SW_HIDE);
    let title = match s.detail.clone() {
        Detail::None => "Select a weapon to place it, or a feature to set it up.".to_string(),
        Detail::Weapon(id) => match s.editor.entry(&id) {
            None => String::new(),
            Some(e) => {
                let loadout = s.editor.loadout();
                let current = loadout.iter().find(|p| p.id == id).map(|p| p.position);
                for p in &e.component.compatible_positions {
                    let slot = p.slot().map(|w| w.name()).unwrap_or("?");
                    let taken = s
                        .editor
                        .occupant(*p, &id)
                        .and_then(|o| s.editor.entry(&o).map(|e| e.name().to_string()))
                        .map(|n| format!("   — {n} is there"))
                        .unwrap_or_default();
                    let text = format!("{}  ({slot}){taken}", position_text(*p));
                    SendMessageW(combo, CB_ADDSTRING, 0, wide(&text).as_ptr() as isize);
                    s.positions.push(*p);
                }
                let index = current.and_then(|c| s.positions.iter().position(|p| *p == c));
                SendMessageW(combo, CB_SETCURSEL, index.map_or(usize::MAX, |i| i), 0);
                ShowWindow(combo, SW_SHOW);
                if current.is_some() {
                    format!("{}: position", e.name())
                } else {
                    format!("{}: not in the loadout; pick a position to add it", e.name())
                }
            }
        },
        Detail::Feature(id) => match s.editor.entry(&id) {
            None => String::new(),
            Some(e) => {
                let on = s.editor.profile.features.contains(&id);
                let decls: Vec<OptionDecl> = e.component.options.iter().take(MAX_OPTIONS).cloned().collect();
                for (i, o) in decls.iter().enumerate() {
                    let (label_id, control_id) = (ID_OPTION_BASE + 2 * i, ID_OPTION_BASE + 2 * i + 1);
                    let value = s.editor.option(&id, o);
                    let control = match (&o.kind, &value) {
                        (OptionKind::Bool { .. }, OptionValue::Bool(b)) => {
                            make(parent, 0, "STATIC", "", 0, label_id);
                            let h = make(parent, 0, "BUTTON", &o.label, WS_TABSTOP | BS_AUTOCHECKBOX as u32, control_id);
                            SendMessageW(h, BM_SETCHECK, *b as usize, 0);
                            h
                        }
                        (OptionKind::Int { min, max, .. }, OptionValue::Int(v)) => {
                            make(parent, 0, "STATIC", &format!("{} ({min}–{max}):", o.label), 0, label_id);
                            make(parent, WS_EX_CLIENTEDGE, "EDIT", &v.to_string(), WS_TABSTOP | ES_AUTOHSCROLL as u32, control_id)
                        }
                        (OptionKind::Choice { choices, .. }, OptionValue::Choice(v)) => {
                            make(parent, 0, "STATIC", &format!("{}:", o.label), 0, label_id);
                            let h = make(parent, 0, "COMBOBOX", "", WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32, control_id);
                            for c in choices {
                                SendMessageW(h, CB_ADDSTRING, 0, wide(c).as_ptr() as isize);
                            }
                            let index = choices.iter().position(|c| c == v).unwrap_or(0);
                            SendMessageW(h, CB_SETCURSEL, index, 0);
                            h
                        }
                        _ => continue,
                    };
                    // Options of a feature that isn't selected don't run.
                    EnableWindow(control, on as i32);
                    ShowWindow(ctl(label_id), SW_SHOW);
                    ShowWindow(control, SW_SHOW);
                }
                s.options = decls;
                let more = e.component.options.len().saturating_sub(MAX_OPTIONS);
                match (on, e.component.options.is_empty(), more) {
                    (_, true, _) => format!("{}: no settings", e.name()),
                    (false, _, _) => format!("{}: tick it to change its settings", e.name()),
                    (true, _, 0) => format!("{}: settings", e.name()),
                    (true, _, n) => format!("{}: settings ({n} more in wormforge.toml only)", e.name()),
                }
            }
        },
    };
    SetWindowTextW(ctl(ID_DETAIL), wide(&title).as_ptr());
}

fn status_line(s: &State) -> (String, Tone) {
    let mut problems = s.load_errors.clone();
    let text = match s.editor.resolve() {
        _ if s.editor.is_empty() => "No profile: matches load the packs only.".to_string(),
        Ok(r) => {
            let scheme = r
                .scheme
                .as_ref()
                .and_then(|sc| s.editor.entry(&sc.id).map(|e| e.name().to_string()))
                .unwrap_or_else(|| "no scheme".into());
            format!(
                "Next match runs: {scheme}, {} weapon{}, {} feature{}.",
                r.weapons.len(),
                if r.weapons.len() == 1 { "" } else { "s" },
                r.features.len(),
                if r.features.len() == 1 { "" } else { "s" }
            )
        }
        Err(errors) => {
            problems.splice(0..0, errors);
            "This profile doesn't resolve; matches load the packs only.".to_string()
        }
    };
    match problems.split_first() {
        None => (text, Tone::On),
        Some((first, rest)) if rest.is_empty() => (format!("{text} {first}"), Tone::Problem),
        Some((first, rest)) => (format!("{text} {first} (and {} more)", rest.len()), Tone::Problem),
    }
}

/// The colour of the tab's status line, for `WM_CTLCOLORSTATIC`.
pub(crate) unsafe fn tone_of(control: HWND) -> Option<Tone> {
    if control != ctl(ID_PROFILE_STATUS) {
        return None;
    }
    STATUS_TONE.lock().ok().and_then(|t| *t)
}

/// Lay the tab out in `(x, y, w, h)`.
pub(crate) unsafe fn layout(x: i32, y: i32, w: i32, h: i32) {
    with(|s| s.area = (x, y, w, h));
    layout_area((x, y, w, h));
}

unsafe fn layout_area((x, y, w, h): (i32, i32, i32, i32)) {
    if w <= 0 {
        return;
    }
    let (row_h, line_h, gap) = (26, 18, 8);
    MoveWindow(ctl(ID_SCHEME_LABEL), x, y + 5, 60, line_h, 1);
    MoveWindow(ctl(ID_SCHEME), x + 64, y, (w - 64).min(460), 300, 1);
    let options = with(|s| s.options.clone()).unwrap_or_default();
    // Options flow in rows of fixed-width cells.
    let cell_w = 300;
    let per_row = ((w / cell_w).max(1)) as usize;
    let option_rows = options.len().div_ceil(per_row).max(1) as i32;
    let detail_h = line_h + 4 + option_rows * (row_h + 4);
    let lists_y = y + row_h + gap;
    let lists_h = (h - (row_h + gap) - (detail_h + gap) - (line_h + gap)).max(60);
    let left_w = (w - gap) * 3 / 5;
    MoveWindow(ctl(ID_WEAPONS), x, lists_y, left_w, lists_h, 1);
    MoveWindow(ctl(ID_FEATURES), x + left_w + gap, lists_y, w - left_w - gap, lists_h, 1);
    let detail_y = lists_y + lists_h + gap;
    MoveWindow(ctl(ID_DETAIL), x, detail_y, w, line_h, 1);
    let controls_y = detail_y + line_h + 4;
    MoveWindow(ctl(ID_POSITION), x, controls_y, (w).min(420), 300, 1);
    for (i, o) in options.iter().enumerate() {
        let cx = x + (i % per_row) as i32 * cell_w;
        let cy = controls_y + (i / per_row) as i32 * (row_h + 4);
        let (label, control) = (ctl(ID_OPTION_BASE + 2 * i), ctl(ID_OPTION_BASE + 2 * i + 1));
        match o.kind {
            OptionKind::Bool { .. } => {
                MoveWindow(label, cx, cy, 0, 0, 1);
                MoveWindow(control, cx, cy + 2, cell_w - 12, row_h - 4, 1);
            }
            OptionKind::Int { .. } => {
                MoveWindow(label, cx, cy + 4, 170, line_h, 1);
                MoveWindow(control, cx + 172, cy, 80, row_h - 2, 1);
            }
            OptionKind::Choice { .. } => {
                MoveWindow(label, cx, cy + 4, 120, line_h, 1);
                MoveWindow(control, cx + 122, cy, cell_w - 134, 200, 1);
            }
        }
    }
    MoveWindow(ctl(ID_PROFILE_STATUS), x, y + h - line_h, w, line_h, 1);
}

/// The search box changed: list the matching catalog weapons, or the loadout.
pub(crate) unsafe fn search_changed() {
    refresh();
}

/// Ask before `weapon` takes `clash.position` from the weapon there.
unsafe fn confirm_replace(editor: &Editor, weapon: &str, clash: &Clash) -> bool {
    let name = |id: &str| editor.entry(id).map(|e| e.name().to_string()).unwrap_or_else(|| id.to_string());
    let text = format!(
        "{} is at {}.\n\nReplace it with {}? It leaves the loadout.",
        name(&clash.occupant),
        position_text(clash.position),
        name(weapon)
    );
    ask(&text)
}

unsafe fn ask(text: &str) -> bool {
    MessageBoxW(
        WINDOW.load(Ordering::SeqCst),
        wide(text).as_ptr(),
        wide("WormForge profile").as_ptr(),
        MB_YESNO | MB_ICONQUESTION,
    ) == IDYES
}

/// Place `weapon` at `position` (or add it where it fits), asking before a
/// replacement.
unsafe fn place(weapon: &str, position: Option<PanelPosition>) {
    let Some(editor) = with(|s| s.editor.clone()) else {
        return;
    };
    let mut trial = editor.clone();
    let attempt = match position {
        Some(p) => trial.move_weapon(weapon, p),
        None => trial.add_weapon(weapon),
    };
    match attempt {
        Ok(_) => edit(|e| *e = trial),
        Err(clash) if confirm_replace(&editor, weapon, &clash) => edit(|e| e.replace(weapon, clash.position)),
        Err(_) => {
            PostMessageW(WINDOW.load(Ordering::SeqCst), WM_APP_PROFILE, 0, 0);
        }
    }
}

/// `WM_COMMAND` from the tab's controls; false if not one of them.
pub(crate) unsafe fn command(id: usize, code: u32) -> bool {
    if FILLING.load(Ordering::SeqCst) {
        return matches!(id, ID_SCHEME | ID_POSITION) || (ID_OPTION_BASE..ID_OPTION_BASE + 2 * MAX_OPTIONS).contains(&id);
    }
    match id {
        ID_SCHEME if code == CBN_SELCHANGE => {
            let index = SendMessageW(ctl(ID_SCHEME), CB_GETCURSEL, 0, 0);
            let Some((scheme, edits, current)) = with(|s| {
                (s.schemes.get(index as usize).cloned().flatten(), s.editor.loadout_edits(), s.editor.profile.scheme.clone())
            }) else {
                return true;
            };
            if scheme != current
                && edits > 0
                && !ask(&format!(
                    "Switching the scheme drops your {edits} change{} to the current loadout and starts from the new scheme's preset.\n\nSwitch?",
                    if edits == 1 { "" } else { "s" }
                ))
            {
                PostMessageW(WINDOW.load(Ordering::SeqCst), WM_APP_PROFILE, 0, 0);
                return true;
            }
            edit(|e| e.set_scheme(scheme));
            true
        }
        ID_POSITION if code == CBN_SELCHANGE => {
            let index = SendMessageW(ctl(ID_POSITION), CB_GETCURSEL, 0, 0);
            let picked = with(|s| match &s.detail {
                Detail::Weapon(id) => s.positions.get(index as usize).map(|p| (id.clone(), *p)),
                _ => None,
            })
            .flatten();
            if let Some((weapon, position)) = picked {
                place(&weapon, Some(position));
            }
            true
        }
        ID_SCHEME | ID_POSITION => true,
        _ if (ID_OPTION_BASE..ID_OPTION_BASE + 2 * MAX_OPTIONS).contains(&id) => {
            let i = (id - ID_OPTION_BASE) / 2;
            let Some((feature, decl)) = with(|s| match &s.detail {
                Detail::Feature(f) => s.options.get(i).map(|o| (f.clone(), o.clone())),
                _ => None,
            })
            .flatten() else {
                return true;
            };
            let control = ctl(id);
            let value = match &decl.kind {
                OptionKind::Bool { .. } if code == BN_CLICKED => {
                    Some(OptionValue::Bool(SendMessageW(control, BM_GETCHECK, 0, 0) as u32 == BST_CHECKED))
                }
                OptionKind::Int { .. } if code == EN_KILLFOCUS => {
                    let mut buf = [0u16; 32];
                    let n = GetWindowTextW(control, buf.as_mut_ptr(), buf.len() as i32);
                    let text = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
                    match text.trim().parse::<i64>() {
                        Ok(v) => Some(OptionValue::Int(v)),
                        Err(_) => {
                            set_status(&format!("{}: \"{}\" is not a whole number.", decl.label, text.trim()));
                            PostMessageW(WINDOW.load(Ordering::SeqCst), WM_APP_PROFILE, 0, 0);
                            None
                        }
                    }
                }
                OptionKind::Choice { choices, .. } if code == CBN_SELCHANGE => {
                    let index = SendMessageW(control, CB_GETCURSEL, 0, 0);
                    choices.get(index as usize).map(|c| OptionValue::Choice(c.clone()))
                }
                _ => None,
            };
            if let Some(value) = value {
                let current = with(|s| s.editor.option(&feature, &decl));
                if current.as_ref() != Some(&value) {
                    let result = with(|s| {
                        let mut trial = s.editor.clone();
                        trial.set_option(&feature, &decl.key, value.clone())
                    })
                    .unwrap_or(Ok(()));
                    match result {
                        Ok(()) => edit(|e| {
                            let _ = e.set_option(&feature, &decl.key, value);
                        }),
                        Err(e) => {
                            set_status(&format!("{e}"));
                            PostMessageW(WINDOW.load(Ordering::SeqCst), WM_APP_PROFILE, 0, 0);
                        }
                    }
                }
            }
            true
        }
        _ => false,
    }
}

/// `WM_NOTIFY` from the tab's lists.
pub(crate) unsafe fn notify(hdr: *const NMHDR) -> Option<LRESULT> {
    let from = (*hdr).idFrom;
    if from != ID_WEAPONS && from != ID_FEATURES {
        return None;
    }
    if (*hdr).code != LVN_ITEMCHANGED || FILLING.load(Ordering::SeqCst) {
        return Some(0);
    }
    let nm = &*(hdr as *const NMLISTVIEW);
    let item = nm.iItem;
    let id = with(|s| if from == ID_WEAPONS { s.weapons.get(item as usize) } else { s.features.get(item as usize) }.cloned())
        .flatten();
    let Some(id) = id else {
        return Some(0);
    };
    // Selected: show its settings.
    if nm.uNewState & LVIS_SELECTED != 0 && nm.uOldState & LVIS_SELECTED == 0 {
        let detail = if from == ID_WEAPONS { Detail::Weapon(id.clone()) } else { Detail::Feature(id.clone()) };
        if with(|s| std::mem::replace(&mut s.detail, detail.clone()) != detail).unwrap_or(false) {
            PostMessageW(WINDOW.load(Ordering::SeqCst), WM_APP_PROFILE, 1, 0);
        }
    }
    // Ticked or unticked.
    let (old, new) = (nm.uOldState & LVIS_STATEIMAGEMASK, nm.uNewState & LVIS_STATEIMAGEMASK);
    if old != new && new != 0 && old != 0 {
        let checked = item_checked(ctl(from), item);
        if from == ID_FEATURES {
            edit(|e| e.set_feature(&id, checked));
        } else if checked {
            place(&id, None);
        } else {
            edit(|e| e.remove_weapon(&id));
        }
    }
    Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_read_as_row_and_column() {
        let p = PanelPosition::of(weapons::StockWeapon::Bazooka).unwrap();
        assert_eq!(position_text(p), "F1 1");
        assert_eq!(value_text(&OptionValue::Bool(true)), "on");
    }
}
