//! Frame timing, turned on by a pack's `wa.perf.enable{ counter, profile }`.
//!
//! - `counter`: a one-line FPS counter at the top right of the screen: the
//!   live frame rate (the last quarter second), the average over the last
//!   two seconds, and the longest frame in the last five.
//! - `profile`: time per section of a rendered frame, summed per frame and
//!   written to `wkLua.log` every ten seconds.
//!
//! A frame is one pass of WA's GameRender, closed at the HUD hook, which runs
//! once per rendered frame on every renderer. The sections are the places
//! WormForge wraps anyway, so each costs a clock read at either end:
//!
//! ```text
//!   sim      the world root's FRAME_FINISH: WA's physics step, with our hooks
//!   scene    the world root's RENDER_SCENE: entities queue their draws
//!   draw     RQ_RenderDrawingQueue: WA draws the queue (GPU capture inside)
//!   present  our GPU sprites, after D3D9ShaderPresent
//!   hud      wa.on.hud and its text, this counter included
//!   lua      every engine call into Lua, wherever it happens (a part of the
//!            sections above, not added to them)
//!   other    the rest of the frame: WA's own work and waiting for vsync
//! ```
//!
//! Presentation only. Nothing here is read by the simulation, and Lua cannot
//! read a timing back. Off, a probe is one relaxed load.

use crate::log_line;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Instant;

#[derive(Clone, Copy)]
pub enum Section {
    Sim,
    Scene,
    Draw,
    Present,
    Hud,
}

const SECTIONS: usize = 5;
const NAMES: [&str; SECTIONS] = ["sim", "scene", "draw", "present", "hud"];

/// The counter's spike covers this much time, and it keeps frames this long.
const WINDOW_NS: u64 = 5_000_000_000;
/// Its average covers this much.
const AVERAGE_NS: u64 = 2_000_000_000;
/// Its live rate covers this much, and its text is rebuilt this often.
const LIVE_NS: u64 = 250_000_000;
/// The profiler writes a report this often.
const REPORT_NS: u64 = 10_000_000_000;

pub fn profiling() -> bool {
    lua_host::perf_flags().profile
}

fn counting() -> bool {
    lua_host::perf_flags().counter
}

/// Time until dropped goes to `section`, when profiling.
pub fn probe(section: Section) -> Option<Probe> {
    profiling().then(|| Probe {
        section,
        start: Instant::now(),
    })
}

pub struct Probe {
    section: Section,
    start: Instant,
}

impl Drop for Probe {
    fn drop(&mut self) {
        let ns = self.start.elapsed().as_nanos() as u64;
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let i = self.section as usize;
            s.frame[i] += ns;
            s.calls[i] += 1;
        });
    }
}

#[derive(Default)]
struct Stat {
    sum: u64,
    max: u64,
    calls: u64,
}

impl Stat {
    fn add(&mut self, ns: u64, calls: u64) {
        self.sum += ns;
        self.max = self.max.max(ns);
        self.calls += calls;
    }
}

#[derive(Default)]
struct Report {
    start: Option<Instant>,
    frames: u64,
    frame: Stat,
    sections: [Stat; SECTIONS],
    lua: Stat,
}

#[derive(Default)]
struct State {
    last: Option<Instant>,
    /// This frame's time per section, and calls.
    frame: [u64; SECTIONS],
    calls: [u64; SECTIONS],
    /// The counter's frames: when each ended, and how long it took, in ns.
    recent: VecDeque<(Instant, u64)>,
    text: String,
    text_at: Option<Instant>,
    report: Report,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Close a rendered frame. Called once per GameRender, at the HUD hook.
pub fn frame() {
    let flags = lua_host::perf_flags();
    if !flags.counter && !flags.profile {
        return;
    }
    let now = Instant::now();
    let (lua_ns, lua_calls) = lua_host::take_lua_time();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let s = &mut *s;
        let Some(last) = s.last.replace(now) else {
            s.frame = [0; SECTIONS];
            s.calls = [0; SECTIONS];
            return;
        };
        let dt = now.duration_since(last).as_nanos() as u64;
        if flags.counter {
            s.recent.push_back((now, dt));
            while s
                .recent
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t).as_nanos() as u64 > WINDOW_NS)
            {
                s.recent.pop_front();
            }
            let stale = s
                .text_at
                .is_none_or(|t| now.duration_since(t).as_nanos() as u64 >= LIVE_NS);
            if stale {
                let frames: Vec<(u64, u64)> = s
                    .recent
                    .iter()
                    .map(|(t, dt)| (now.duration_since(*t).as_nanos() as u64, *dt))
                    .collect();
                s.text = counter_text(&frames);
                s.text_at = Some(now);
            }
        }
        if flags.profile {
            let r = &mut s.report;
            let start = *r.start.get_or_insert(last);
            r.frames += 1;
            r.frame.add(dt, 1);
            for i in 0..SECTIONS {
                r.sections[i].add(s.frame[i], s.calls[i]);
            }
            r.lua.add(lua_ns, lua_calls as u64);
            let span = now.duration_since(start).as_nanos() as u64;
            if span >= REPORT_NS {
                log_line(&report_line(span, r));
                s.report = Report::default();
            }
        }
        s.frame = [0; SECTIONS];
        s.calls = [0; SECTIONS];
    });
}

/// The counter's text, when it is on.
pub fn counter() -> Option<String> {
    if !counting() {
        return None;
    }
    STATE.with(|s| {
        let s = s.borrow();
        (!s.text.is_empty()).then(|| s.text.clone())
    })
}

/// `frames`: (how long ago each frame ended, how long it took), in ns, over
/// the last five seconds.
fn counter_text(frames: &[(u64, u64)]) -> String {
    let rate = |it: &mut dyn Iterator<Item = u64>| {
        let (n, sum) = it.fold((0u64, 0u64), |(n, sum), dt| (n + 1, sum + dt));
        if sum == 0 {
            0.0
        } else {
            n as f64 * 1e9 / sum as f64
        }
    };
    let live = rate(&mut frames.iter().filter(|(ago, _)| *ago < LIVE_NS).map(|(_, dt)| *dt));
    let avg = rate(&mut frames.iter().filter(|(ago, _)| *ago < AVERAGE_NS).map(|(_, dt)| *dt));
    let spike = frames.iter().map(|(_, dt)| *dt).max().unwrap_or(0);
    format!("{live:.0} fps  avg {avg:.1}  spike {:.1} ms", spike as f64 / 1e6)
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

fn report_line(span: u64, r: &Report) -> String {
    let frames = r.frames.max(1);
    let total = r.frame.sum.max(1);
    let mut line = format!(
        "perf: {:.1} s, {} frames ({:.1} fps), frame avg {:.2} max {:.2} ms",
        span as f64 / 1e9,
        r.frames,
        r.frames as f64 * 1e9 / span.max(1) as f64,
        ms(r.frame.sum / frames),
        ms(r.frame.max),
    );
    let mut part = |name: &str, s: &Stat| {
        line.push_str(&format!(
            " | {name} avg {:.2} max {:.2} ms ({:.0}%, {:.1}/frame)",
            ms(s.sum / frames),
            ms(s.max),
            s.sum as f64 * 100.0 / total as f64,
            s.calls as f64 / frames as f64,
        ));
    };
    for (i, name) in NAMES.iter().enumerate() {
        part(name, &r.sections[i]);
    }
    part("lua", &r.lua);
    let measured: u64 = r.sections.iter().map(|s| s.sum).sum();
    let other = r.frame.sum.saturating_sub(measured);
    line.push_str(&format!(
        " | other avg {:.2} ms ({:.0}%)",
        ms(other / frames),
        other as f64 * 100.0 / total as f64
    ));
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn counter_shows_live_rate_average_and_spike() {
        // 5 s at 20 ms a frame, one 80 ms frame 3 s ago, and the last
        // quarter second at 10 ms a frame.
        let mut frames = Vec::new();
        let mut ago = 0;
        for _ in 0..25 {
            frames.push((ago, 10 * MS));
            ago += 10 * MS;
        }
        while ago < 5_000 * MS {
            let dt = if (3_000 * MS..3_020 * MS).contains(&ago) { 80 * MS } else { 20 * MS };
            frames.push((ago, dt));
            ago += 20 * MS;
        }
        let text = counter_text(&frames);
        assert!(text.starts_with("100 fps  avg "), "{text}");
        assert!(text.ends_with("spike 80.0 ms"), "{text}");
        // The average: the last 2 s only, so without the 80 ms frame.
        let recent: Vec<u64> = frames.iter().filter(|(ago, _)| *ago < 2_000 * MS).map(|(_, dt)| *dt).collect();
        let avg = recent.len() as f64 * 1e9 / recent.iter().sum::<u64>() as f64;
        assert!(text.contains(&format!("avg {avg:.1}")), "{text}");
        assert!(!recent.contains(&(80 * MS)));
    }

    #[test]
    fn counter_with_no_frames_reads_zero() {
        assert_eq!(counter_text(&[]), "0 fps  avg 0.0  spike 0.0 ms");
    }

    #[test]
    fn report_gives_each_section_and_the_rest() {
        let mut r = Report { frames: 2, ..Default::default() };
        r.frame.add(20 * MS, 1);
        r.frame.add(30 * MS, 1);
        r.sections[Section::Sim as usize].add(10 * MS, 2);
        r.sections[Section::Draw as usize].add(5 * MS, 1);
        r.lua.add(4 * MS, 6);
        let line = report_line(1_000 * MS, &r);
        assert!(line.starts_with("perf: 1.0 s, 2 frames (2.0 fps), frame avg 25.00 max 30.00 ms"), "{line}");
        assert!(line.contains("| sim avg 5.00 max 10.00 ms (20%, 1.0/frame)"), "{line}");
        assert!(line.contains("| draw avg 2.50 max 5.00 ms (10%, 0.5/frame)"), "{line}");
        assert!(line.contains("| lua avg 2.00 max 4.00 ms (8%, 3.0/frame)"), "{line}");
        assert!(line.ends_with("| other avg 17.50 ms (70%)"), "{line}");
    }
}
