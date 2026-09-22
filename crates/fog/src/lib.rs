//! Fog-of-war visibility engine.
//!
//! Pure and WA-agnostic, on the same split as `crates/render`: inputs are a
//! solidity closure over the landscape's 1bpp collision grid, a list of vision
//! sources, and a scheme of settings. Nothing here knows about Worms
//! Armageddon's memory layout, the render queue, or D3D — that glue lives in
//! `engine/src/fog.rs`. Keeping the model host-only is what lets the shadowcast
//! and discovery logic be tested off-Windows.
//!
//! The model is a Rust port of wkFOW's `Vision.cpp` / `Discovery.cpp`, reduced
//! to the core fog feature set (vision circles/cones, LOS, terrain memory). The
//! parallel cast and the two-tier LOD are ported at full fidelity: profiling
//! showed they are what make the cast meet WA's frame budget, so they are load
//! -bearing, not optional.

pub mod discovery;
pub mod settings;
pub mod vision;

pub use settings::{Scheme, Value, MAX_VISION_DISTANCE, SETTINGS};
pub use vision::{Fog, SourceKind, VisionSource};
