pub fn check_381() -> Result<(), String> {
    let addr = crate::wa::address("STR_VERSION_381") as *const u8;
    if addr.is_null() {
        return Err("STR_VERSION_381 is null".into());
    }
    let bytes = unsafe { std::slice::from_raw_parts(addr, 5) };
    if bytes != b"3.8.1" {
        let got = String::from_utf8_lossy(bytes);
        return Err(format!("expected 3.8.1 at {addr:?}, found {got:?}"));
    }
    Ok(())
}
