//! The `.WF_WAgame` container: a byte-exact `.WAgame` plus everything needed
//! to re-simulate it.
//!
//! ```text
//! 2026-09-22 20.14.03 [Online] @eule.WF_WAgame
//!   [hdr]  magic "WFRP", format version, WA version, wkLua build hash,
//!          modset fingerprint, container flags, two-phase `complete`
//!   [pack] every loaded pack: mod.toml + Lua + assets (deflate)
//!   [wa]   WA's own .WAgame bytes, verbatim
//!   [wf]   WormForge frame stream, keyed to world-root FRAME_FINISH
//!   [chk]  sync checkpoints
//! ```
//!
//! Two hard constraints, both inherited from the plan:
//!
//! - **No dependency on `engine`.** `engine` cannot compile on darwin
//!   (minhook rejects it), so format code placed there would never be tested
//!   on a developer's Mac. Everything here is pure bytes and is tested here,
//!   host-side, like `crates/render`.
//! - **Never copy `log.rs`'s drop-on-contention writer.** A recorder that
//!   silently loses frames is worse than no recorder: playback would diverge
//!   quietly. The stream refuses (caps, monotonic frames, torn records)
//!   rather than degrading. Actual file I/O lives in `engine`, which decides
//!   what to do on a full disk; this crate only defines what "full" means.
//!
//! The reader's refusals are distinct variants (see [`Error`]) because W3 has
//! to "refuse cleanly" and name the reason: wrong format version, wrong WA
//! version, missing `complete`.

pub mod error;
pub mod format;
pub mod pack;

pub use error::{tag_str, Error, Offset};
pub use format::{
    assemble, decode_checkpoints, decode_inputs, encode_checkpoints, encode_inputs, Checkpoint,
    Container, Header, InputSample, FORMAT_VERSION, HDR_SCHEMA, KNOWN_TAGS, MAGIC, MAX_WF_BYTES,
    TAG_CHK, TAG_HDR, TAG_PACK, TAG_WA, TAG_WF,
};
pub use pack::{
    decode as decode_pack, encode as encode_pack, Pack, PackFile, MAX_PACKS, MAX_PACK_BYTES,
    MAX_PACK_FILES,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// One end-to-end pass through every stage the recorder will call, in
    /// order: streams first, packs next, container last, parse back out.
    /// This is the contract `engine`'s recorder is written against.
    #[test]
    fn a_full_recording_round_trips_end_to_end() {
        let inputs = [
            InputSample {
                frame: 49,
                keys: 4,
                px: 320,
                py: 120,
                down: false,
            },
            InputSample {
                frame: 50,
                keys: 4 | 1,
                px: 318,
                py: 120,
                down: false,
            },
        ];
        let chk = [Checkpoint {
            frame: 49,
            rng: 0x6af2_2799,
            worms: 0xa35e_6047,
            actors: 0x57bd_1fde,
        }];
        let packs = vec![Pack {
            id: "synctest".into(),
            version: "0.1.0".into(),
            files: vec![PackFile {
                path: "weapons.lua".into(),
                raw: b"return {}\n".to_vec(),
            }],
        }];

        // Phase one: the file as it exists while the round runs.
        let open = Header {
            wa_version: "3.8.1.0".into(),
            build_hash: "dev".into(),
            modset: "synctest@0.1.0".into(),
            flags: 0,
            complete: false,
        };
        let wf = encode_inputs(&inputs).unwrap();
        let chk_bytes = encode_checkpoints(&chk).unwrap();
        let pack_bytes = encode_pack(&packs).unwrap();
        let temp = assemble(&open, &pack_bytes, b"WAGAME BYTES", &wf, &chk_bytes).unwrap();
        // Playback refuses it --
        assert_eq!(
            Container::parse(&temp).unwrap().wf().unwrap_err(),
            Error::Incomplete
        );

        // Phase two: round ended cleanly, rewrite with complete = true.
        let done = Header {
            complete: true,
            ..open
        };
        let file = assemble(&done, &pack_bytes, b"WAGAME BYTES", &wf, &chk_bytes).unwrap();
        let c = Container::parse(&file).unwrap();
        assert_eq!(c.header().modset, "synctest@0.1.0");
        assert_eq!(c.wa().unwrap(), b"WAGAME BYTES", "[wa] verbatim");
        assert_eq!(decode_inputs(c.wf().unwrap()).unwrap(), inputs);
        assert_eq!(decode_checkpoints(c.chk().unwrap()).unwrap(), chk);
        assert_eq!(decode_pack(c.pack().unwrap()).unwrap(), packs);
    }

    /// The `[wa]` section is the container's whole justification: extracting
    /// it must give back the exact vanilla bytes, or nothing is gained over a
    /// sidecar.
    #[test]
    fn extracting_wa_returns_byte_identical_vanilla_bytes() {
        let mut fake_wagame = Vec::new();
        for i in 0..4096u32 {
            fake_wagame.extend_from_slice(&i.wrapping_mul(0x0100_0193).to_le_bytes());
        }
        let header = Header {
            complete: true,
            ..Header::default()
        };
        let file = assemble(&header, &encode_pack(&[]).unwrap(), &fake_wagame, b"", b"").unwrap();
        let c = Container::parse(&file).unwrap();
        assert_eq!(c.wa().unwrap(), fake_wagame.as_slice());
    }

    #[test]
    fn wrong_wa_version_is_a_distinct_refusal() {
        let header = Header {
            wa_version: "3.9.0".into(),
            complete: true,
            ..Header::default()
        };
        let file = assemble(&header, &encode_pack(&[]).unwrap(), b"WA", b"", b"").unwrap();
        let container = Container::parse(&file).unwrap();
        assert_eq!(
            container.require_wa_version("3.8.1"),
            Err(Error::WaVersion {
                found: "3.9.0".into(),
                supported: "3.8.1".into(),
            })
        );
    }
}
