fn main() {
    println!("cargo:rerun-if-changed=native/truecolor.cpp");
    println!("cargo:rerun-if-changed=shaders/truecolor.h");
    println!("cargo:rerun-if-changed=shaders/truecolor.hlsl");
    let source = std::fs::read("shaders/truecolor.hlsl").expect("read HLSL");
    // Hash LF bytes only — Windows checkouts may CRLF the .hlsl while the
    // stamp was produced from the LF blob in git.
    let hash = source
        .iter()
        .copied()
        .filter(|&b| b != b'\r')
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        });
    let header = std::fs::read_to_string("shaders/truecolor.h").expect("read shader header");
    assert!(
        header.starts_with(&format!("// HLSL-FNV1A: {hash:016x}\n")),
        "stale truecolor shader: regenerate the header with tools/compile-truecolor-shader.py"
    );
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        cc::Build::new()
            .cpp(true)
            .file("native/truecolor.cpp")
            .flag_if_supported("/std:c++17")
            .compile("wormforge_truecolor");
    }
}
