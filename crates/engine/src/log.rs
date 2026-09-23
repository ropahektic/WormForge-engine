use std::fs::{OpenOptions, create_dir_all};
use std::io::Write;
use std::os::windows::ffi::OsStringExt;
use std::panic;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    AllocConsole, GetStdHandle, STD_OUTPUT_HANDLE, SetConsoleTitleW, WriteConsoleA,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
static ERR: Mutex<Vec<std::fs::File>> = Mutex::new(Vec::new());
static CONSOLE: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(core::ptr::null_mut());

pub(crate) const BUILD_ID: &str = "20260923-replay-w4-decoder-route";

pub fn log_path() -> PathBuf {
    wormforge_dir().join("wkLua.log")
}

/// Same folder as `wkLua.log` (`%LOCALAPPDATA%\WormForge\wkluaerror.txt`).
pub fn error_log_path() -> PathBuf {
    wormforge_dir().join("wkluaerror.txt")
}

pub(crate) fn wormforge_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("WormForge")
}

pub(crate) fn wa_directory() -> PathBuf {
    unsafe {
        let handle = GetModuleHandleW(std::ptr::null());
        let mut buf = [0u16; 260];
        let n = GetModuleFileNameW(handle, buf.as_mut_ptr(), buf.len() as u32);
        let os = std::ffi::OsString::from_wide(&buf[..n as usize]);
        PathBuf::from(os)
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

fn open_trunc(path: &PathBuf) -> Option<std::fs::File> {
    if let Some(dir) = path.parent() {
        let _ = create_dir_all(dir);
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .ok()
}

/// AllocConsole so a GUI `WA.exe` has a window for `[wkLua]` lines.
pub fn attach_console() {
    if !CONSOLE.load(Ordering::SeqCst).is_null() {
        return;
    }
    unsafe {
        let _ = AllocConsole();
        let mut title: Vec<u16> = "wkLua".encode_utf16().collect();
        title.push(0);
        SetConsoleTitleW(title.as_ptr());
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if !h.is_null() {
            CONSOLE.store(h, Ordering::SeqCst);
        }
    }
    console_write("wkLua.dll loaded — this console is the live log\r\n");
}

pub fn init() {
    attach_console();
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = create_dir_all(dir);
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => *FILE.lock().unwrap() = Some(f),
        Err(e) => console_write(&format!("could not open {}: {e}\r\n", path.display())),
    }

    let err_path = error_log_path();
    let wa_err = wa_directory().join("wkluaerror.txt");
    let mut errs = Vec::new();
    match open_trunc(&err_path) {
        Some(f) => errs.push(f),
        None => console_write(&format!("could not create {}\r\n", err_path.display())),
    }
    if wa_err != err_path {
        match open_trunc(&wa_err) {
            Some(f) => errs.push(f),
            None => console_write(&format!("could not create {}\r\n", wa_err.display())),
        }
    }
    *ERR.lock().unwrap() = errs;

    let prev = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        log_error(&format!("panic: {info}"));
        prev(info);
    }));

    log_line(&format!("wkLua build {BUILD_ID}"));
    log_line(&format!("log file {}", path.display()));
    log_line(&format!("error log {}", err_path.display()));
    if wa_err != err_path {
        log_line(&format!("error log copy {}", wa_err.display()));
    }
    write_err(&format!(
        "=== wkLua session {BUILD_ID} ===\r\nerror log {}\r\n",
        err_path.display()
    ));
}

pub fn log_line(msg: &str) {
    let line = format!("[wkLua] {msg}\r\n");
    console_write(&line);
    write_file(&FILE, &format!("{msg}\r\n"));
}

pub fn log_error(msg: &str) {
    let line = format!("[wkLua ERROR] {msg}\r\n");
    console_write(&line);
    write_file(&FILE, &format!("ERROR {msg}\r\n"));
    write_err(&format!("ERROR {msg}\r\n"));
}

pub fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "Box<dyn Any>".into()
    }
}

fn console_write(text: &str) {
    let h = CONSOLE.load(Ordering::SeqCst);
    if h.is_null() {
        return;
    }
    let mut written = 0u32;
    unsafe {
        WriteConsoleA(
            h as HANDLE,
            text.as_ptr(),
            text.len() as u32,
            &mut written,
            core::ptr::null(),
        );
    }
}

fn write_err(text: &str) {
    let Ok(mut guard) = ERR.try_lock() else {
        return;
    };
    for f in guard.iter_mut() {
        let _ = f.write_all(text.as_bytes());
        let _ = f.flush();
    }
}

fn write_file(slot: &Mutex<Option<std::fs::File>>, text: &str) {
    let Ok(mut guard) = slot.try_lock() else {
        return;
    };
    if let Some(f) = guard.as_mut() {
        let _ = f.write_all(text.as_bytes());
        let _ = f.flush();
    }
}
