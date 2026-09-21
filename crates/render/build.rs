//! Builds the COM shim on Windows, and guards the embedded shader everywhere.
//!
//! The staleness check runs on all hosts on purpose: an edited .hlsl whose
//! header was never regenerated would otherwise ship stale bytecode, and that
//! failure is invisible until it renders wrong on someone else's machine.
use std::path::Path;

fn fnv1a(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in data {
        hash = (hash ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn main() {
    println!("cargo:rerun-if-changed=native/wf_gpu.cpp");
    println!("cargo:rerun-if-changed=shaders/sprite.hlsl");
    println!("cargo:rerun-if-changed=shaders/sprite.h");

    let source = Path::new("shaders/sprite.hlsl");
    let header = Path::new("shaders/sprite.h");
    let hlsl = std::fs::read(source).expect("shaders/sprite.hlsl");
    let text = std::fs::read_to_string(header).expect("shaders/sprite.h");
    let stamped = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("// HLSL-FNV1A: "))
        .expect("shaders/sprite.h is missing its source hash");
    assert_eq!(
        stamped,
        format!("{:016x}", fnv1a(&hlsl)),
        "stale shader: regenerate with crates/render/probe/compile-shader.py"
    );

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        cc::Build::new()
            .cpp(true)
            .file("native/wf_gpu.cpp")
            .flag_if_supported("/std:c++17")
            .compile("wormforge_gpu");
    }
}
