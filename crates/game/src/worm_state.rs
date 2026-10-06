//! WA 3.8.1 worm states (`WorldEntity.state`, worm +0x44): the one table of
//! what each number is, for logs and debugging. Packs never set these; they
//! read the few names `worm.state` gives (lua-host `worm_state_name`).
//!
//! Evidence, in two kinds:
//! - SetState (`WORM_SET_STATE`) has a case per state 0x64..=0x8B, and the
//!   case's setup routine names some outright (Standing, PreFiring, Punching,
//!   Drilling, Teleporting, JetPacking, Bungeeing, Parachuting).
//! - The worm state trace (engine `state_trace`) and the worm_state_logger
//!   pack, over played and replayed rounds: which key or message led where.
//!   Seen so far: walking 0x66; the turn's worm idle 0x67; fire key (msg
//!   0x26) 0x68, release (msg 0x27) 0x69, then 0x6A/0x6B and back to 0x65;
//!   jump key (msg 0x24) 0x77 -> 0x79 -> 0x7A; a hit (msg 0x4B) 0x7F -> 0x76
//!   -> 0x65; blast knockback (msg 0x1C) 0x81 -> 0x80 or 0x7F; a parachute
//!   closed by the jump key 0x7D -> 0x7E; in water 0x7F -> 0x76 -> 0x85 ->
//!   0x87; dying on land 0x65 -> 0x82 -> 0x87.
//!
//! - Disassembly: SetState 0x6D's setup (0x518A50) and its frame (0x518C10)
//!   are the Kamikaze's: the flight countdown at +0x278, digging, the blast
//!   (100, the record's +0x3C), and 0x85 when the worm reaches the water.
//!
//! A label ending in `?` rests on one observation or one source only; check
//! it before building on it.

/// A debug label for a worm state, or `None` for one nobody has seen yet.
pub fn label(state: u32) -> Option<&'static str> {
    Some(match state {
        0x65 => "standing",
        0x66 => "walking",
        0x67 => "ready",
        0x68 => "charging",
        0x69 => "prefiring",
        0x6A => "firing",
        0x6B => "fired",
        0x6C => "punching",
        0x6D => "kamikaze",
        0x6E => "drilling",
        0x6F => "teleporting",
        0x70 => "rope_firing?",
        0x71 => "torching?",
        0x73 => "rope_rolling?",
        0x76 => "getting_up",
        0x77 => "jump_prepare",
        0x78 => "jetpacking",
        0x79 => "jumping",
        0x7A => "jump_falling",
        0x7B => "roping?",
        0x7C => "bungee",
        0x7D => "parachuting",
        0x7E => "falling",
        0x7F => "tumbling",
        0x80 => "hit",
        0x81 => "blasted",
        0x82 => "dying?",
        0x85 => "drowning?",
        0x87 => "dead?",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setup_named_states_have_their_names() {
        assert_eq!(label(0x65), Some("standing"));
        assert_eq!(label(0x69), Some("prefiring"));
        assert_eq!(label(0x78), Some("jetpacking"));
        assert_eq!(label(0x7C), Some("bungee"));
        assert_eq!(label(0x7D), Some("parachuting"));
    }

    #[test]
    fn a_hit_worm_tumbles_it_does_not_sink() {
        // 0x7F follows a hit (msg 0x4B) on land and resolves through 0x76 to
        // standing; it was long mislabelled as drowning.
        assert_eq!(label(0x7F), Some("tumbling"));
        assert_eq!(label(0x76), Some("getting_up"));
    }

    #[test]
    fn unseen_states_have_no_label() {
        assert_eq!(label(0x64), None);
        assert_eq!(label(0x8B), None);
    }
}
