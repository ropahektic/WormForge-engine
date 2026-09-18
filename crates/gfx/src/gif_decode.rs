//! GIF → equal-sized RGBA frames, with disposal compositing.
//!
//! WA blit maps `animation_phase` evenly across `.spr` frames. We keep each
//! GIF frame once and set `fps` from the average delay so the missile loop
//! matches the file.

use crate::{RgbaFrame, SpriteStrip};
use gif::{ColorOutput, DecodeOptions, DisposalMethod};
use std::io::Cursor;
use std::path::Path;

pub fn load_gif_file(path: &Path) -> Result<SpriteStrip, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    load_gif_bytes(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn load_gif_bytes(bytes: &[u8]) -> Result<SpriteStrip, String> {
    let mut opts = DecodeOptions::new();
    opts.set_color_output(ColorOutput::RGBA);
    let mut decoder = opts
        .read_info(Cursor::new(bytes))
        .map_err(|e| format!("gif header: {e}"))?;
    let width = decoder.width() as u32;
    let height = decoder.height() as u32;
    if width == 0 || height == 0 {
        return Err("empty gif".into());
    }

    let mut canvas = vec![0u8; (width as usize) * (height as usize) * 4];
    let mut saved = canvas.clone();
    let mut frames = Vec::new();
    let mut delays = Vec::new();

    loop {
        let frame = match decoder.read_next_frame() {
            Ok(Some(frame)) => frame.clone(),
            Ok(None) => break,
            Err(e) => return Err(format!("gif frame: {e}")),
        };
        let left = frame.left as u32;
        let top = frame.top as u32;
        let fw = frame.width as u32;
        let fh = frame.height as u32;
        if frame.dispose == DisposalMethod::Previous {
            saved.copy_from_slice(&canvas);
        }
        blit_frame(&mut canvas, width, height, left, top, fw, fh, &frame.buffer);
        frames.push(RgbaFrame {
            width,
            height,
            rgba: canvas.clone(),
        });
        delays.push(if frame.delay == 0 { 10 } else { frame.delay });
        match frame.dispose {
            DisposalMethod::Background => {
                clear_rect(&mut canvas, width, height, left, top, fw, fh);
            }
            DisposalMethod::Previous => canvas.copy_from_slice(&saved),
            _ => {}
        }
    }

    if frames.is_empty() {
        return Err("gif has no frames".into());
    }

    let looping = frames.len() > 1;
    Ok(SpriteStrip {
        frames,
        fps: if looping { fps_from_delays(&delays) } else { 10 },
        looping,
    })
}

fn fps_from_delays(delays: &[u16]) -> u16 {
    let n = delays.len().max(1) as u32;
    let sum: u32 = delays
        .iter()
        .map(|&d| if d == 0 { 10 } else { d as u32 })
        .sum();
    let avg_cs = (sum / n).max(1);
    (100 / avg_cs).clamp(1, 50) as u16
}

fn blit_frame(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    left: u32,
    top: u32,
    fw: u32,
    fh: u32,
    src: &[u8],
) {
    for y in 0..fh {
        let dy = top + y;
        if dy >= canvas_h {
            continue;
        }
        for x in 0..fw {
            let dx = left + x;
            if dx >= canvas_w {
                continue;
            }
            let si = ((y * fw + x) * 4) as usize;
            if si + 3 >= src.len() {
                continue;
            }
            if src[si + 3] < 16 {
                continue;
            }
            let di = ((dy * canvas_w + dx) * 4) as usize;
            canvas[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
}

fn clear_rect(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    left: u32,
    top: u32,
    fw: u32,
    fh: u32,
) {
    for y in 0..fh {
        let dy = top + y;
        if dy >= canvas_h {
            continue;
        }
        for x in 0..fw {
            let dx = left + x;
            if dx >= canvas_w {
                continue;
            }
            let di = ((dy * canvas_w + dx) * 4) as usize;
            canvas[di..di + 4].copy_from_slice(&[0, 0, 0, 0]);
        }
    }
}
