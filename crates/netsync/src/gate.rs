//! Whether the host may start the round (`docs/network-asset-sync-plan.md`,
//! step 3).
//!
//! While a generation is lit, every machine in the lobby must hold exactly
//! that generation: a round loads its packs from the synced copy, and a
//! machine without it would play different scripts and desync. A player
//! without WormForge cannot take part in such a round at all.

use crate::proto::State;

/// What the host knows of one connected client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerView {
    pub name: String,
    /// Its WA packets reach the lobby handler: it is in the lobby.
    pub in_lobby: bool,
    /// Answered our hello: runs WormForge.
    pub wormforge: bool,
    /// Greeted long enough ago that no answer means no WormForge.
    pub gave_up: bool,
    /// Its latest status: generation and state.
    pub status: Option<(u32, State)>,
}

/// `Ok` when the round may start; otherwise the reason, for the lobby.
///
/// `generation` is the lit generation (0 when the host's bulb is out) and
/// `served` the one the host has frozen and loaded, with its pack count. A
/// generation without packs syncs nothing, so it holds nobody back: a host
/// with no packs enabled plays with anyone, as before WormForge.
pub fn verdict(generation: u32, served: Option<(u32, usize)>, peers: &[PeerView]) -> Result<(), String> {
    if generation == 0 || peers.is_empty() {
        // Alone, nobody can be out of step; a round started before the freeze
        // lands falls back to the installed packs.
        return Ok(());
    }
    match served {
        Some((g, 0)) if g == generation => return Ok(()),
        Some((g, _)) if g == generation => {}
        _ => return Err("still freezing the packs".into()),
    }
    let mut waiting = Vec::new();
    for p in peers {
        if !p.in_lobby {
            waiting.push(format!("{} is still joining", p.name));
        } else if !p.wormforge {
            if p.gave_up {
                return Err(format!("{} does not have WormForge and cannot play this round", p.name));
            }
            waiting.push(format!("{} has not answered yet", p.name));
        } else {
            match p.status {
                Some((g, State::Ready)) if g == generation => {}
                Some((g, State::Failed)) if g == generation => {
                    return Err(format!("{} could not take the packs", p.name));
                }
                Some((g, State::Working)) if g == generation => {
                    waiting.push(format!("{} is receiving the packs", p.name));
                }
                _ => waiting.push(format!("{} has not taken the packs yet", p.name)),
            }
        }
    }
    if waiting.is_empty() {
        Ok(())
    } else {
        Err(waiting.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str) -> PeerView {
        PeerView {
            name: name.into(),
            in_lobby: true,
            wormforge: true,
            gave_up: false,
            status: Some((3, State::Ready)),
        }
    }

    #[test]
    fn a_lit_generation_waits_for_every_player() {
        assert_eq!(verdict(0, None, &[]), Ok(()), "bulb out: WA decides alone");
        assert_eq!(verdict(3, Some((2, 1)), &[]), Ok(()), "alone, even mid-freeze");
        assert!(verdict(3, Some((2, 1)), &[peer("naja")]).unwrap_err().contains("freezing"));
        assert_eq!(verdict(3, Some((3, 1)), &[]), Ok(()), "alone in the lobby");
        assert_eq!(verdict(3, Some((3, 1)), &[peer("naja"), peer("rico")]), Ok(()));

        let mut behind = peer("rico");
        behind.status = Some((2, State::Ready));
        let why = verdict(3, Some((3, 1)), &[peer("naja"), behind]).unwrap_err();
        assert!(why.contains("rico has not taken the packs yet"), "{why}");

        let mut busy = peer("naja");
        busy.status = Some((3, State::Working));
        assert!(verdict(3, Some((3, 1)), &[busy]).unwrap_err().contains("receiving"));

        let mut broken = peer("naja");
        broken.status = Some((3, State::Failed));
        assert!(verdict(3, Some((3, 1)), &[broken]).unwrap_err().contains("could not"));
    }

    #[test]
    fn a_player_without_wormforge_blocks_the_round() {
        let mut vanilla = peer("guest");
        vanilla.wormforge = false;
        vanilla.status = None;
        assert!(verdict(3, Some((3, 1)), &[vanilla.clone()]).unwrap_err().contains("not answered"));
        vanilla.gave_up = true;
        assert!(verdict(3, Some((3, 1)), &[vanilla]).unwrap_err().contains("does not have WormForge"));
        let mut joining = peer("new");
        joining.in_lobby = false;
        assert!(verdict(3, Some((3, 1)), &[joining]).unwrap_err().contains("joining"));
    }

    #[test]
    fn no_packs_means_nothing_to_wait_for() {
        let mut vanilla = peer("guest");
        vanilla.wormforge = false;
        vanilla.gave_up = true;
        assert_eq!(verdict(3, Some((3, 0)), &[vanilla]), Ok(()));
    }
}
