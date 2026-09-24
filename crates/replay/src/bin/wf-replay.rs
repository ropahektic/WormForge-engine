use replay::{decode_checkpoints, decode_inputs, decode_pack, Content, Trailer};
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
    let Some(trailer) = Trailer::find(&bytes).map_err(|e| e.to_string())? else {
        return Err(format!(
            "{} is a vanilla WA replay: no WormForge trailer",
            Path::new(&input).display()
        ));
    };

    match command.to_string_lossy().as_ref() {
        "inspect" if output.is_none() => inspect(&trailer),
        "extract" => {
            let output = output.ok_or_else(usage)?;
            let wa = trailer.wa_prefix();
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

fn inspect(trailer: &Trailer<'_>) -> Result<(), String> {
    let header = trailer.header();
    println!("format:       {}", replay::FORMAT_VERSION);
    println!("WA version:   {}", header.wa_version);
    println!("wkLua build:  {}", header.build_hash);
    println!("modset:       {}", header.modset);
    println!("WFAssets:     {}", header.collection_version);

    let pack_bytes = trailer.pack();
    let packs = decode_pack(pack_bytes).map_err(|e| e.to_string())?;
    let inputs = decode_inputs(trailer.wf()).map_err(|e| e.to_string())?;
    let checkpoints = decode_checkpoints(trailer.chk()).map_err(|e| e.to_string())?;
    let files: usize = packs.iter().map(|p| p.files.len()).sum();
    let refs = replay::asset_refs(&packs).len();

    println!(
        "packs:        {} ({files} files, {refs} collection references, {} encoded bytes)",
        packs.len(),
        pack_bytes.len()
    );
    println!("vanilla WA:   {} bytes", trailer.wa_prefix().len());
    println!(
        "input frames: {} ({} encoded bytes)",
        inputs.len(),
        trailer.wf().len()
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
        trailer.chk().len()
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
        for file in &pack.files {
            match &file.content {
                Content::Raw(bytes) => println!("  {:<40} Raw({} bytes)", file.path, bytes.len()),
                Content::Asset(sha) => println!(
                    "  {:<40} Asset({})",
                    file.path,
                    assets::sha256::to_hex(sha)
                ),
            }
        }
    }
    Ok(())
}

fn usage() -> String {
    "usage: wf-replay inspect FILE.WAgame\n       wf-replay extract FILE.WAgame OUTPUT.WAgame"
        .into()
}
