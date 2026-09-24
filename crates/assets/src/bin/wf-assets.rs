//! Build, verify and list `.WFAssets` collections.
//!
//! ```text
//! wf-assets build --version N --lock <file> --out <file>
//!                 [--previous <file>] [--add <dir>]... [--ext png,wav]
//! wf-assets verify <file> [--lock <file>]
//! wf-assets list <file>
//! wf-assets check <file> <pack dir>
//! ```
//!
//! `build` starts from the previously shipped collection (every entry carried
//! forward), adds every file under each `--add` directory whose extension is
//! listed, appends a lock line for each new blob, and refuses to write a
//! collection missing any locked hash. `list` prints one line per entry on
//! stdout, so `wf-assets list <file> | wc -l` is the entry count. `check`
//! tells a pack author which of their files a replay would store as a
//! reference (`asset`) and which it would embed (`embed`).

use assets::{sha256, Builder, Collection, Lock, LockEntry};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn main() {
    if let Err(e) = run() {
        eprintln!("wf-assets: {e}");
        std::process::exit(1);
    }
}

const USAGE: &str = "usage:
  wf-assets build --version N --lock <file> --out <file> [--previous <file>] [--add <dir>]... [--ext png,wav]
  wf-assets verify <file> [--lock <file>]
  wf-assets list <file>
  wf-assets check <file> <pack dir>";

#[derive(Default)]
struct Args {
    positional: Vec<PathBuf>,
    version: Option<u32>,
    lock: Option<PathBuf>,
    out: Option<PathBuf>,
    previous: Option<PathBuf>,
    add: Vec<PathBuf>,
    ext: Option<String>,
}

fn parse(mut raw: impl Iterator<Item = std::ffi::OsString>) -> Result<Args, String> {
    let mut args = Args::default();
    while let Some(arg) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{} needs a value\n{USAGE}", arg.to_string_lossy()));
        match arg.to_string_lossy().as_ref() {
            "--version" => {
                let v = value()?;
                args.version = Some(v.to_string_lossy().parse().map_err(|_| "--version must be a number")?)
            }
            "--lock" => args.lock = Some(value()?.into()),
            "--out" => args.out = Some(value()?.into()),
            "--previous" => args.previous = Some(value()?.into()),
            "--add" => args.add.push(value()?.into()),
            "--ext" => args.ext = Some(value()?.to_string_lossy().into_owned()),
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n{USAGE}")),
            _ => args.positional.push(arg.into()),
        }
    }
    Ok(args)
}

fn run() -> Result<(), String> {
    let mut raw = std::env::args_os().skip(1);
    let command = raw.next().ok_or(USAGE)?;
    let args = parse(raw)?;
    match command.to_string_lossy().as_ref() {
        "build" => build(&args),
        "verify" => verify(&args),
        "list" => list(&args),
        "check" => check(&args),
        _ => Err(USAGE.to_string()),
    }
}

fn one_file(args: &Args) -> Result<&Path, String> {
    match args.positional.as_slice() {
        [file] => Ok(file),
        _ => Err(USAGE.to_string()),
    }
}

fn open(path: &Path) -> Result<Collection<std::fs::File>, String> {
    Collection::open(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_lock(path: &Path) -> Result<Lock, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Lock::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn build(args: &Args) -> Result<(), String> {
    let (Some(version), Some(lock_path), Some(out)) = (args.version, &args.lock, &args.out) else {
        return Err(USAGE.to_string());
    };
    if !args.positional.is_empty() {
        return Err(USAGE.to_string());
    }
    let exts: Vec<String> = args
        .ext
        .as_deref()
        .unwrap_or("png,wav")
        .split(',')
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    let mut lock = read_lock(lock_path)?;

    // Hashes carried from the previous collection, with its release number.
    let mut carried_from: Option<(u32, HashSet<[u8; 32]>)> = None;
    let mut builder = match &args.previous {
        Some(path) => {
            let previous = open(path)?;
            let hashes = previous.entries().iter().map(|e| e.sha256).collect();
            carried_from = Some((previous.version(), hashes));
            Builder::from_previous(&previous).map_err(|e| format!("{}: {e}", path.display()))?
        }
        None => Builder::new(),
    };
    let carried = builder.len();

    let (mut added, mut duplicates) = (0usize, 0usize);
    for dir in &args.add {
        let mut files = Vec::new();
        walk(dir, &mut files).map_err(|e| format!("{}: {e}", dir.display()))?;
        files.sort();
        for file in files {
            let wanted = file
                .extension()
                .map(|e| exts.contains(&e.to_string_lossy().to_ascii_lowercase()))
                .unwrap_or(false);
            if !wanted {
                continue;
            }
            let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let rel = file.strip_prefix(dir).unwrap_or(&file);
            let rel = rel.to_string_lossy().replace('\\', "/");
            if builder.add(&rel, bytes).1 {
                added += 1;
            } else {
                duplicates += 1;
            }
        }
    }

    // The append-only rule, before anything is written.
    lock.check(|sha| builder.contains(sha)).map_err(|e| e.to_string())?;

    let mut out_bytes = Vec::new();
    builder.write(version, &mut out_bytes).map_err(|e| e.to_string())?;
    // Re-read what we are about to ship: the index must parse and every blob
    // must hash to its key.
    let written = Collection::read(std::io::Cursor::new(out_bytes.as_slice())).map_err(|e| e.to_string())?;
    written.verify_all().map_err(|e| e.to_string())?;

    let mut new_lines = Vec::new();
    for e in written.entries() {
        if !lock.contains(&e.sha256) {
            let entry = LockEntry {
                sha256: e.sha256,
                size: e.len as u64,
                // A carried entry the lock never recorded shipped with the
                // previous collection, not with this one.
                first_release: match &carried_from {
                    Some((v, hashes)) if hashes.contains(&e.sha256) => *v,
                    _ => version,
                },
                path: e.path.clone(),
            };
            new_lines.push(entry.line());
            lock.entries.push(entry);
        }
    }

    let tmp = out.with_extension("WFAssets.tmp");
    std::fs::write(&tmp, &out_bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, out).map_err(|e| format!("{}: {e}", out.display()))?;
    if !new_lines.is_empty() {
        use std::io::Write;
        let fresh = !lock_path.exists();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(lock_path)
            .map_err(|e| format!("{}: {e}", lock_path.display()))?;
        let mut text = String::new();
        if fresh {
            text.push_str("# .WFAssets lock: every hash any shipped collection contains.\n");
            text.push_str("# Append-only. sha256 size first_release path\n");
        }
        for line in &new_lines {
            text.push_str(line);
            text.push('\n');
        }
        f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", lock_path.display()))?;
    }

    let total: u64 = written.entries().iter().map(|e| e.len as u64).sum();
    eprintln!(
        "collection {version}: {} entries ({carried} carried, {added} added, {duplicates} duplicate files skipped), \
         {:.1} MB of blobs -> {}; {} lock line(s) appended",
        written.entries().len(),
        total as f64 / 1e6,
        out.display(),
        new_lines.len()
    );
    Ok(())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn verify(args: &Args) -> Result<(), String> {
    let file = one_file(args)?;
    let c = open(file)?;
    c.verify_all().map_err(|e| format!("{}: {e}", file.display()))?;
    if let Some(lock_path) = &args.lock {
        let lock = read_lock(lock_path)?;
        lock.check(|sha| c.contains(sha)).map_err(|e| e.to_string())?;
        for e in &lock.entries {
            let size = c.find(&e.sha256).map(|x| x.len as u64);
            if size != Some(e.size) {
                return Err(format!(
                    "{}: size {} in the lock, {size:?} in the collection",
                    sha256::to_hex(&e.sha256),
                    e.size
                ));
            }
        }
        eprintln!("lock: all {} locked hashes present", lock.entries.len());
    }
    eprintln!("collection {}: {} entries, every blob verified", c.version(), c.entries().len());
    Ok(())
}

fn check(args: &Args) -> Result<(), String> {
    let [file, dir] = args.positional.as_slice() else {
        return Err(USAGE.to_string());
    };
    let c = open(file)?;
    let mut files = Vec::new();
    walk(dir, &mut files).map_err(|e| format!("{}: {e}", dir.display()))?;
    files.sort();
    let (mut referenced, mut embedded) = (0u64, 0u64);
    for path in files {
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // The same rule the recorder applies: Lua and mod.toml always embed.
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_ascii_lowercase();
        let always = name == "mod.toml" || name.ends_with(".lua");
        if !always && c.contains(&sha256::digest(&bytes)) {
            println!("asset  {rel}");
            referenced += bytes.len() as u64;
        } else {
            println!("embed  {rel}");
            embedded += bytes.len() as u64;
        }
    }
    eprintln!(
        "a replay embeds {:.1} KB of this pack; {:.1} KB resolve from collection {}",
        embedded as f64 / 1024.0,
        referenced as f64 / 1024.0,
        c.version()
    );
    Ok(())
}

fn list(args: &Args) -> Result<(), String> {
    let c = open(one_file(args)?)?;
    for e in c.entries() {
        println!("{} {} {}", sha256::to_hex(&e.sha256), e.len, e.path);
    }
    eprintln!("collection {}: {} entries", c.version(), c.entries().len());
    Ok(())
}
