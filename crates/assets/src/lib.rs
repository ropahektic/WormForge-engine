//! The `.WFAssets` collection: raw assets shipped with each release, so a
//! replay can refer to a pack file by its SHA-256 instead of embedding it.
//!
//! - [`collection`]: the file format -- build, read the index, fetch and
//!   re-hash blobs.
//! - [`lock`]: `assets/wfassets.lock`, the committed record of every hash
//!   ever shipped, which keeps collections append-only.
//! - [`sha256`]: the hash everything is keyed by.
//!
//! See `docs/replay-wagame-plan.md` and `docs/wfassets.md`.

pub mod collection;
pub mod error;
pub mod lock;
pub mod sha256;

pub use collection::{Builder, Collection, Entry, FORMAT_VERSION};
pub use error::Error;
pub use lock::{Lock, LockEntry};

/// The collection's file name, installed next to `wkLua.dll`.
pub const FILE_NAME: &str = "WormForge.WFAssets";
