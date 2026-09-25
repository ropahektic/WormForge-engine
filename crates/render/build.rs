//! Builds the COM shim on Windows, and guards the embedded shader everywhere.
//!
//! The staleness check runs on all hosts on purpose: an edited .hlsl whose
//! header was never regenerated would otherwise ship stale bytecode, and that
//! failure is invisible until it renders wrong on someone else's machine.
fn fnv1a(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in data {
        hash = (hash ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Re-check that a shader header's stamped source hash still matches its
/// .hlsl, so an edited shader whose header was never regenerated fails the
/// build instead of shipping stale bytecode.
fn check_fresh(name: &str) {
    let source = format!("shaders/{name}.hlsl");
    let header = format!("shaders/{name}.h");
    println!("cargo:rerun-if-changed={source}");
    println!("cargo:rerun-if-changed={header}");
    let hlsl = std::fs::read(&source).unwrap_or_else(|_| panic!("{source}"));
    // Hash LF bytes only — Windows checkouts may CRLF the .hlsl while the
    // stamp (and bytecode) were produced from the LF blob in git.
    let hlsl: Vec<u8> = hlsl
        .into_iter()
        .filter(|&b| b != b'\r')
        .collect();
    let text = std::fs::read_to_string(&header).unwrap_or_else(|_| panic!("{header}"));
    let stamped = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("// HLSL-FNV1A: "))
        .unwrap_or_else(|| panic!("{header} is missing its source hash"));
    assert_eq!(
        stamped,
        format!("{:016x}", fnv1a(&hlsl)),
        "stale shader: regenerate with crates/render/probe/compile-shader.py"
    );
}

fn main() {
    println!("cargo:rerun-if-changed=native/wf_gpu.cpp");
    check_fresh("sprite");
    check_fresh("fog");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        cc::Build::new()
            .cpp(true)
            .file("native/wf_gpu.cpp")
            .flag_if_supported("/std:c++17")
            .compile("wormforge_gpu");
    }
}
