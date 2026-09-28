//! Resolved WA addresses and live hook-site validation.

use minhook::MinHook;
use re_catalog::signature::{self, PeImage};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE, PAGE_EXECUTE_READ,
    PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS,
};

struct Symbol {
    address: u32,
    text: bool,
    clean_entry: Vec<Option<u8>>,
}

struct Addresses {
    symbols: HashMap<String, Symbol>,
}

static ADDRESSES: OnceLock<Addresses> = OnceLock::new();
static CREATED_HOOKS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

pub fn init() -> Result<(), String> {
    let path = std::env::current_exe().map_err(|e| format!("WA executable path: {e}"))?;
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let pe = PeImage::parse(&bytes)?;
    let module = crate::rebase::wa_base();
    if module == 0 || pe.image_size() == 0 {
        return Err("WA module has invalid base or image size".into());
    }
    let live_header = unsafe { std::slice::from_raw_parts(module as *const u8, 0x1000) };
    if live_header.get(..2) != Some(b"MZ") {
        return Err("loaded WA module has no MZ header".into());
    }
    let pe_at = u32::from_le_bytes(live_header[0x3c..0x40].try_into().unwrap()) as usize;
    if pe_at
        .checked_add(84)
        .is_none_or(|end| end > live_header.len())
        || live_header.get(pe_at..pe_at + 4) != Some(b"PE\0\0")
    {
        return Err("loaded WA module has no PE32 header".into());
    }
    let live_timestamp = u32::from_le_bytes(live_header[pe_at + 8..pe_at + 12].try_into().unwrap());
    let live_size = u32::from_le_bytes(
        live_header[pe_at + 24 + 56..pe_at + 24 + 60]
            .try_into()
            .unwrap(),
    );
    if live_timestamp != pe.timestamp() || live_size != pe.image_size() {
        return Err("WA.exe on disk differs from the loaded module".into());
    }
    let locators = signature::builtin()?;
    let mut symbols = HashMap::with_capacity(locators.len());
    for locator in locators {
        let result = match pe.resolve(&locator) {
            Ok(result) => result,
            Err(e) if !locator.required => {
                crate::log::log_error(&format!("optional WA locator unavailable: {e}"));
                continue;
            }
            Err(e) => return Err(e),
        };
        let address = module
            .checked_add(result.rva)
            .ok_or_else(|| format!("{}: live address overflow", locator.name))?;
        let text = locator.target_section == ".text";
        let clean_entry = if text {
            if locator.section != ".text"
                || !matches!(locator.derive, signature::Derive::Match)
                || locator.addend != 0
            {
                return Err(format!(
                    "{}: code entry needs a direct code signature",
                    locator.name
                ));
            }
            signature::parse_pattern(&locator.pattern)?
                .into_iter()
                .take(16)
                .collect()
        } else {
            Vec::new()
        };
        symbols.insert(
            locator.name,
            Symbol {
                address,
                text,
                clean_entry,
            },
        );
    }
    ADDRESSES
        .set(Addresses { symbols })
        .map_err(|_| "WA signatures already resolved".to_string())?;
    crate::log::log_line(&format!(
        "resolved {} WA symbols from {}",
        locators_count(),
        path.display()
    ));
    Ok(())
}

fn locators_count() -> usize {
    ADDRESSES.get().map_or(0, |a| a.symbols.len())
}

pub fn address(name: &str) -> u32 {
    ADDRESSES
        .get()
        .expect("WA addresses not initialized")
        .symbols
        .get(name)
        .unwrap_or_else(|| panic!("WA symbol {name} missing"))
        .address
}

pub fn try_address(name: &str) -> Option<u32> {
    ADDRESSES
        .get()?
        .symbols
        .get(name)
        .map(|symbol| symbol.address)
}

fn symbol_for_address(addr: u32) -> Result<(&'static str, &'static Symbol), String> {
    let addresses = ADDRESSES.get().ok_or("WA addresses not initialized")?;
    addresses
        .symbols
        .iter()
        .find(|(_, s)| s.address == addr && s.text)
        .map(|(name, s)| (name.as_str(), s))
        .ok_or_else(|| format!("hook target {addr:#010x} is not a resolved WA code symbol"))
}

fn page_info(addr: usize) -> Option<MEMORY_BASIC_INFORMATION> {
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let size = unsafe {
        VirtualQuery(
            addr as *const c_void,
            &mut info,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    (size == std::mem::size_of::<MEMORY_BASIC_INFORMATION>() && info.State == MEM_COMMIT)
        .then_some(info)
}

fn accessible(addr: usize, len: usize) -> bool {
    let Some(info) = page_info(addr) else {
        return false;
    };
    if info.Protect & (PAGE_GUARD | PAGE_NOACCESS) != 0 {
        return false;
    }
    addr.checked_add(len)
        .zip((info.BaseAddress as usize).checked_add(info.RegionSize))
        .is_some_and(|(end, region_end)| end <= region_end)
}

pub fn executable(addr: u32) -> bool {
    let Some(info) = page_info(addr as usize) else {
        return false;
    };
    matches!(
        info.Protect & 0xff,
        PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY
    )
}

fn check_entry(addr: u32, symbol: &Symbol, name: &str) -> Result<(), String> {
    if !accessible(addr as usize, 16) {
        return Err(format!("{name}: live entry unreadable"));
    }
    let live = unsafe { std::slice::from_raw_parts(addr as *const u8, 16) };
    if symbol
        .clean_entry
        .iter()
        .enumerate()
        .all(|(i, expected)| expected.is_none_or(|byte| live[i] == byte))
    {
        return Ok(());
    }
    let destination = match live[0] {
        0xe9 => {
            let delta = i32::from_le_bytes(live[1..5].try_into().unwrap());
            Some(addr.wrapping_add(5).wrapping_add(delta as u32))
        }
        0xeb => {
            let delta = live[1] as i8 as i32;
            Some(addr.wrapping_add(2).wrapping_add(delta as u32))
        }
        0xff if live[1] == 0x25 => {
            let slot = u32::from_le_bytes(live[2..6].try_into().unwrap());
            if !accessible(slot as usize, 4) {
                return Err(format!("{name}: indirect jump slot unreadable"));
            }
            Some(unsafe { std::ptr::read_unaligned(slot as *const u32) })
        }
        _ => None,
    };
    let Some(destination) = destination else {
        return Err(format!("{name}: unsupported live entry patch"));
    };
    if destination == addr || !executable(destination) {
        return Err(format!(
            "{name}: jump destination {destination:#010x} is not executable"
        ));
    }
    crate::log::log_line(&format!(
        "{name}: chaining existing jump at {addr:#010x} to {destination:#010x}"
    ));
    Ok(())
}

pub fn check_code_pattern(name: &str) -> Result<u32, String> {
    let addresses = ADDRESSES.get().ok_or("WA addresses not initialized")?;
    let symbol = addresses
        .symbols
        .get(name)
        .ok_or_else(|| format!("missing WA symbol {name}"))?;
    if !symbol.text || !accessible(symbol.address as usize, 16) {
        return Err(format!("{name}: live code unreadable"));
    }
    let live = unsafe { std::slice::from_raw_parts(symbol.address as *const u8, 16) };
    if !symbol
        .clean_entry
        .iter()
        .enumerate()
        .all(|(i, expected)| expected.is_none_or(|byte| live[i] == byte))
    {
        return Err(format!("{name}: live code differs outside masked operands"));
    }
    Ok(symbol.address)
}

pub fn check_callable(name: &str) -> Result<u32, String> {
    let addresses = ADDRESSES.get().ok_or("WA addresses not initialized")?;
    let symbol = addresses
        .symbols
        .get(name)
        .ok_or_else(|| format!("missing WA symbol {name}"))?;
    if !symbol.text {
        return Err(format!("{name} is not executable code"));
    }
    check_entry(symbol.address, symbol, name)?;
    Ok(symbol.address)
}

/// Validate the original WA entry, then let MinHook trampoline through a prior jump.
pub unsafe fn create_hook(target: *mut c_void, detour: *mut c_void) -> Result<*mut c_void, String> {
    let addr = target as u32;
    let (name, symbol) = symbol_for_address(addr)?;
    check_entry(addr, symbol, name)?;
    let trampoline = unsafe { MinHook::create_hook(target, detour) }
        .map_err(|e| format!("{name}: MinHook {e:?}"))?;
    match CREATED_HOOKS.lock() {
        Ok(mut hooks) => hooks.push(target as usize),
        Err(_) => {
            let _ = unsafe { MinHook::remove_hook(target) };
            return Err("hook registry poisoned".into());
        }
    }
    Ok(trampoline)
}

pub fn hook_checkpoint() -> usize {
    CREATED_HOOKS.lock().map_or(0, |hooks| hooks.len())
}

pub fn rollback_to(checkpoint: usize) {
    if let Ok(mut hooks) = CREATED_HOOKS.lock() {
        while hooks.len() > checkpoint {
            let target = hooks.pop().unwrap() as *mut c_void;
            let _ = unsafe { MinHook::disable_hook(target) };
            let _ = unsafe { MinHook::remove_hook(target) };
        }
    }
}

pub fn rollback_hooks() {
    let _ = unsafe { MinHook::disable_all_hooks() };
    rollback_to(0);
}
