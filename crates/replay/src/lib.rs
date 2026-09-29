//! WormForge replays: WA's own `.WAgame`, with a trailer appended that holds
//! everything needed to re-simulate the match.
//!
//! ```text
//! 2026-09-22 20.14.03 [Online] @eule.WAgame
//!   WA's bytes, untouched
//!   trailer
//!     [hdr]  WA version, wkLua build hash, modset, .WFAssets version
//!     [pack] every loaded pack: mod.toml + Lua embedded, assets embedded
//!            or referenced into the .WFAssets collection (deflate)
//!     [wf]   WormForge frame stream, keyed to world-root FRAME_FINISH
//!     [chk]  sync checkpoints
//!   footer   "WFTR", format, lengths, CRC
//! ```
//!
//! WA plays the file as an ordinary replay; a build without WormForge sees a
//! vanilla game. See `docs/replay-wagame-plan.md`.
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
//! The reader's refusals are distinct variants (see [`Error`]) because
//! playback has to refuse cleanly and name the reason: wrong format version,
//! wrong WA version, a damaged trailer, missing assets.

pub mod error;
pub mod format;
pub mod pack;

pub use error::{tag_str, Error, Offset};
pub use format::{
    append_trailer, append_trailer_with_format, decode_checkpoints, decode_inputs,
    encode_checkpoints, encode_inputs, Checkpoint, Header, InputSample, Trailer, FOOTER_LEN,
    FORMAT_VERSION, HDR_SCHEMA, KNOWN_TAGS, LEGACY_FORMAT_VERSION, MAGIC, MAX_WF_BYTES,
    TAG_CHK, TAG_HDR, TAG_PACK, TAG_WF,
};
pub use pack::{
    always_embedded, asset_refs, decode as decode_pack, encode as encode_pack, encode_dedup,
    resolve as resolve_packs, Content, Pack, PackFile, MAX_PACKS, MAX_PACK_BYTES, MAX_PACK_FILES,
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
            rng_words: Some([1, 2, 3]),
        }];
        let packs = vec![Pack {
            id: "synctest".into(),
            version: "0.1.0".into(),
            files: vec![PackFile {
                path: "weapons.lua".into(),
                content: Content::Raw(b"return {}\n".to_vec()),
            }],
        }];
        let header = Header {
            wa_version: "3.8.1.0".into(),
            build_hash: "dev".into(),
            modset: "synctest@0.1.0".into(),
            collection_version: 1,
        };
        let wf = encode_inputs(&inputs).unwrap();
        let chk_bytes = encode_checkpoints(&chk).unwrap();
        let pack_bytes = encode_pack(&packs).unwrap();
        let file = append_trailer(b"WAGAME BYTES", &header, &pack_bytes, &wf, &chk_bytes).unwrap();
        let t = Trailer::find(&file).unwrap().expect("a WormForge replay");
        assert_eq!(t.header(), &header);
        assert_eq!(t.wa_prefix(), b"WAGAME BYTES", "WA's bytes verbatim");
        assert_eq!(decode_inputs(t.wf()).unwrap(), inputs);
        assert_eq!(decode_checkpoints(t.chk(), t.format_version()).unwrap(), chk);
        assert_eq!(decode_pack(t.pack()).unwrap(), packs);
    }

    /// WA's bytes are the file's prefix: they must come back exactly, or WA
    /// would be handed a different game than it recorded.
    #[test]
    fn the_wa_prefix_is_byte_identical() {
        let mut fake_wagame = Vec::new();
        for i in 0..4096u32 {
            fake_wagame.extend_from_slice(&i.wrapping_mul(0x0100_0193).to_le_bytes());
        }
        let file = append_trailer(
            &fake_wagame,
            &Header::default(),
            &encode_pack(&[]).unwrap(),
            b"",
            b"",
        )
        .unwrap();
        assert_eq!(&file[..fake_wagame.len()], fake_wagame.as_slice());
        let t = Trailer::find(&file).unwrap().unwrap();
        assert_eq!(t.wa_prefix(), fake_wagame.as_slice());
    }

    #[test]
    fn wrong_wa_version_is_a_distinct_refusal() {
        let header = Header {
            wa_version: "3.9.0".into(),
            ..Header::default()
        };
        let file = append_trailer(b"WA", &header, &encode_pack(&[]).unwrap(), b"", b"").unwrap();
        let t = Trailer::find(&file).unwrap().unwrap();
        assert_eq!(
            t.require_wa_version("3.8.1"),
            Err(Error::WaVersion {
                found: "3.9.0".into(),
                supported: "3.8.1".into(),
            })
        );
    }
}
