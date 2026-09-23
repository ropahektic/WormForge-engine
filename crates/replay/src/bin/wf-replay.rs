use replay::{decode_checkpoints, decode_inputs, decode_pack, Container};
use std::path::Path;

fn main() {
    if let Err(e) = run() {
        eprintln!("wf-replay: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let command = args.next().ok_or_else(usage)?;
    let input = args.next().ok_or_else(usage)?;
    let output = args.next();
    if args.next().is_some() {
        return Err(usage());
    }
    let bytes = std::fs::read(&input)
        .map_err(|e| format!("could not read {}: {e}", Path::new(&input).display()))?;
    let container = Container::parse(&bytes).map_err(|e| e.to_string())?;

    match command.to_string_lossy().as_ref() {
        "inspect" if output.is_none() => inspect(&container),
        "extract" => {
            let output = output.ok_or_else(usage)?;
            let wa = container.wa().map_err(|e| e.to_string())?;
            std::fs::write(&output, wa)
                .map_err(|e| format!("could not write {}: {e}", Path::new(&output).display()))?;
            println!(
                "extracted {} bytes to {}",
                wa.len(),
                Path::new(&output).display()
            );
            Ok(())
        }
        _ => Err(usage()),
    }
}

fn inspect(container: &Container<'_>) -> Result<(), String> {
    let header = container.header();
    println!("format:       {}", replay::FORMAT_VERSION);
    println!("complete:     {}", header.complete);
    println!("WA version:   {}", header.wa_version);
    println!("wkLua build:  {}", header.build_hash);
    println!("modset:       {}", header.modset);

    let pack_bytes = container.pack().map_err(|e| e.to_string())?;
    let wa = container.wa().map_err(|e| e.to_string())?;
    let wf = container.wf().map_err(|e| e.to_string())?;
    let chk = container.chk().map_err(|e| e.to_string())?;
    let packs = decode_pack(pack_bytes).map_err(|e| e.to_string())?;
    let inputs = decode_inputs(wf).map_err(|e| e.to_string())?;
    let checkpoints = decode_checkpoints(chk).map_err(|e| e.to_string())?;
    let files: usize = packs.iter().map(|p| p.files.len()).sum();

    println!(
        "packs:        {} ({files} files, {} encoded bytes)",
        packs.len(),
        pack_bytes.len()
    );
    println!("vanilla WA:   {} bytes", wa.len());
    println!(
        "input frames: {} ({} encoded bytes)",
        inputs.len(),
        wf.len()
    );
    let keys = inputs.iter().fold(0, |mask, sample| mask | sample.keys);
    let button_frames = inputs.iter().filter(|sample| sample.down).count();
    let drag_moves = inputs
        .windows(2)
        .filter(|pair| {
            pair[0].down && pair[1].down && (pair[0].px, pair[0].py) != (pair[1].px, pair[1].py)
        })
        .count();
    println!(
        "input controls: keys={keys:#04x}, button frames={button_frames}, drag moves={drag_moves}"
    );
    println!(
        "checkpoints:  {} ({} encoded bytes)",
        checkpoints.len(),
        chk.len()
    );
    if let (Some(first), Some(last)) = (inputs.first(), inputs.last()) {
        println!("frame range:   {}..={}", first.frame, last.frame);
    }
    for pack in packs {
        println!(
            "pack:          {}@{} ({} files)",
            pack.id,
            pack.version,
            pack.files.len()
        );
    }
    Ok(())
}

fn usage() -> String {
    "usage: wf-replay inspect FILE.WF_WAgame\n       wf-replay extract FILE.WF_WAgame OUTPUT.WAgame"
        .into()
}
