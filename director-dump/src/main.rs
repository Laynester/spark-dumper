//! director-dump — CLI tool for inspecting Director/Shockwave file structure.
//!
//! Reads DCR/DIR files and dumps their chunk tree with decoded contents
//! for CASt, BITD, STXT, LSCR, KEY*, SND, and more.

mod export;

use clap::Parser;
use std::path::{Path, PathBuf};

use director_core::{mmap, cast, lscr, stxt, key, bitd, sound};
use director_rifx;

/// Export Director/Shockwave (.cct/.dcr/.cst) casts and projects to disk.
#[derive(Parser)]
#[command(name = "sparkd", author, version, about)]
struct Cli {
    /// Input file (.cct/.dcr/.cst) or directory to export
    input: PathBuf,

    /// Output directory
    output: PathBuf,

    /// Show raw chunk data hex dump (single-file debug)
    #[arg(short = 'x', long)]
    hex: bool,

    /// Show memory map details (single-file debug)
    #[arg(short = 'm', long)]
    memory: bool,

    /// Show only the chunk tree (single-file debug, default: all)
    #[arg(short, long)]
    tree: bool,

    /// Write decompressed RIFX data to this file path
    #[arg(short = 'o', long)]
    out: Option<PathBuf>,

    /// Report palette resolution for every bitmap member
    #[arg(long)]
    palettes: bool,

    /// Recompress an Afterburner file (round-trip) to this output path
    #[arg(short = 'c', long = "compress-out")]
    compress_out: Option<PathBuf>,

    /// Worker threads for directory export (default 1)
    #[arg(short = 'j', long)]
    threads: Option<usize>,

    /// Also write a .lasm disassembly next to each exported .ls script
    /// (raw LSCR bytecode, opcode-level — for verifying decompiler output)
    #[arg(long)]
    lasm: bool,
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();

    match run(&cli) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let input = &cli.input;
    let out = &cli.output;

    if !input.exists() {
        return Err(format!("file not found: {}", input.display()).into());
    }

    if input.is_dir() {
        return run_directory(cli, input, out);
    }

    // Single file: export directly into the output directory. Debug flags
    // (-x, -m, -t, -o, -c, --palettes, --lasm) still inspect/dump as before.
    run_inspect(cli, input, out)
}

/// Single-file export/inspect mode. The project exports to `out` directly
/// (LibreShockwave-style layout); debug flags dump instead.
fn run_inspect(cli: &Cli, path: &Path, out: &Path) -> Result<(), Box<dyn std::error::Error>> {

    let file_size = std::fs::metadata(path)?.len();
    let data = std::fs::read(path)?;
    let is_compressed = director_rifx::is_compressed(&data);

    println!("File:          {}", path.display());
    println!("Size:          {file_size} bytes");
    println!("Compressed:    {is_compressed}");
    println!();

    // Write decompressed RIFX binary if --out is specified
    if let Some(out_path) = &cli.out {
        if is_compressed {
            let af = director_rifx::afterburner::parse_afterburner(&data)
                .map_err(|e| format!("decompression failed: {e}"))?;
            std::fs::write(out_path, &af.rifx_data)?;
            println!("Decompressed: {} → {} ({} bytes)",
                path.display(), out_path.display(), af.rifx_data.len());
        } else {
            std::fs::write(out_path, &data)?;
            println!("Copied: {} → {} ({} bytes)",
                path.display(), out_path.display(), data.len());
        }
    }

    let root = director_rifx::read_bytes(&data)?;

    println!("=== Chunk Tree ===");
    println!("{}", director_rifx::dump_tree(&root));

    if cli.tree {
        return Ok(());
    }

    // --- Parse and display KEY* ---
    let key_chunks = root.children_by(b"KEY*");
    for key_chunk in &key_chunks {
        println!("=== KEY* (Key Table) ===");
        match key::read_key(key_chunk) {
            Ok(kt) => {
                println!("  Entry size: {}, Total: {}, Used: {}",
                    kt.entry_size, kt.entry_count, kt.used_count);
                if kt.entries.len() <= 30 {
                    for e in &kt.entries {
                        let tag_str = std::str::from_utf8(&e.child_tag).unwrap_or("????");
                        println!("    child={:5} parent={:5} childType={}", 
                            e.child_index, e.parent_index, tag_str);
                    }
                } else {
                    println!("  ({} entries, use --tree for full list)", kt.entries.len());
                }
            }
            Err(e) => println!("  Parse error: {e}"),
        }
        println!();
    }

    // --- Parse and display CASt chunks ---
    let cast_chunks = root.children_by(b"CASt");
    if !cast_chunks.is_empty() {
        println!("=== Cast Members (CASt) ===");
        for (i, c) in cast_chunks.iter().enumerate() {
            let len = c.data_len();
            match cast::read_cast_member(c) {
                Ok(cm) => {
                    println!("  [{i}] {:5} bytes  {}", len, cm.summary());
                }
                Err(_) => {
                    println!("  [{i}] {:5} bytes  (unparseable)", len);
                }
            }
        }
        println!();
    }

    // --- Parse and display STXT chunks ---
    let stxt_chunks = root.children_by(b"STXT");
    for stxt_chunk in &stxt_chunks {
        println!("=== STXT (Styled Text) ===");
        match stxt::read_stxt(stxt_chunk) {
            Ok(s) => {
                let preview: String = s.text.chars().take(80).collect();
                println!("  Text ({} chars): {}", s.text.chars().count(),
                    if s.text.chars().count() > 80 {
                        format!("{preview}...")
                    } else {
                        s.text.clone()
                    });
                if !s.formatting.is_empty() {
                    println!("  Formatting runs: {}", s.formatting.len());
                }
            }
            Err(e) => println!("  Parse error: {e}"),
        }
        println!();
    }

    // --- Parse and display LSCR chunks ---
    let lscr_chunks = root.children_by(b"Lscr");
    if !lscr_chunks.is_empty() {
        println!("=== Lingo Scripts (LSCR) ===");
        let lnam = root
            .children_by(b"Lnam")
            .first()
            .and_then(|c| lscr::read_script_names(c).ok());
        for (i, l) in lscr_chunks.iter().enumerate() {
            let handler_names = match lscr::read_script(l, true) {
                Ok(ls) => ls
                    .handlers
                    .iter()
                    .map(|h| match &lnam {
                        Some(n) => n
                            .name(h.name_id as i32)
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("#{}", h.name_id)),
                        None => format!("#{}", h.name_id),
                    })
                    .collect::<Vec<_>>(),
                Err(_) => vec![],
            };
            if handler_names.is_empty() {
                println!("  [{i}] {:5} bytes  (unparseable)", l.data_len());
            } else {
                println!("  [{i}] {:5} bytes, {} handlers: {:?}",
                    l.data_len(), handler_names.len(), handler_names);
            }
        }
        println!();
    }

    // --- Parse and display BITD chunks ---
    let bitd_chunks = root.children_by(b"BITD");
    if !bitd_chunks.is_empty() {
        println!("=== Bitmaps (BITD) ===");
        for (i, b) in bitd_chunks.iter().enumerate() {
            match bitd::read_bitd(b) {
                Ok(bd) => {
                    println!("  [{i}] {:5} bytes  pixel data: {} bytes", 
                        b.data_len(), bd.pixel_data.len());
                }
                Err(e) => println!("  [{i}] Parse error: {e}"),
            }
        }
        println!();
    }

    // --- Parse and display SND chunks ---
    let snd_tags = [b"SND ", b"snd ", b"sndH", b"sndS", b"ediM"];
    for tag in &snd_tags {
        let snd_chunks = root.children_by(tag);
        let tag_str = std::str::from_utf8(tag.as_slice()).unwrap_or("????");
        for snd_chunk in &snd_chunks {
            println!("=== Sound ({tag_str}) ===");
            let res = if **tag == *b"ediM" {
                sound::read_edim(snd_chunk).map(|info| sound::SoundData {
                    info,
                    raw_data: snd_chunk.data().to_vec(),
                })
            } else {
                sound::read_snd(snd_chunk)
            };
            match res {
                Ok(sd) => {
                    let fmt = match sd.info.format {
                        sound::SoundFormat::MacSnd => "Mac 'snd '",
                        sound::SoundFormat::Wav => "WAV",
                        sound::SoundFormat::Aiff => "AIFF",
                        sound::SoundFormat::Moa => "MOA",
                        sound::SoundFormat::Unknown => "Unknown",
                    };
                    println!("  Format: {fmt}, rate={}Hz, {}bit, {}ch, looping={}",
                        sd.info.sample_rate, sd.info.sample_size, sd.info.channels, sd.info.looping);
                }
                Err(e) => println!("  Parse error: {e}"),
            }
            println!();
        }
    }

    // --- Memory map ---
    if let Some(mmap_chunk) = root.child(b"mmap") {
        println!("=== Memory Map (mmap) ===");
        match mmap::read_mmap(mmap_chunk) {
            Ok(mmap) => {
                println!("  Header length:     {}", mmap.header_length);
                println!("  Entry length:      {}", mmap.entry_length);
                println!("  Allocated elements: {}", mmap.allocated_elements);
                println!("  Used elements:     {}", mmap.used_elements);
                println!("  Entries:           {}", mmap.entries.len());
                println!();

                if cli.memory || mmap.entries.len() <= 50 {
                    for entry in &mmap.entries {
                        println!(
                            "    [{:5}] {:4}  addr={:6}  len={}",
                            entry.resource_id,
                            entry.fourcc,
                            entry.address,
                            entry.length,
                        );
                    }
                } else {
                    println!("  (too many entries, use --memory to force)");
                }
            }
            Err(e) => {
                println!("  Failed to parse mmap: {e}");
            }
        }
        println!();
    }

    // --- Lnam chunks ---
    let lnam_chunks = root.children_by(b"Lnam");
    if !lnam_chunks.is_empty() {
        println!("=== Names (Lnam) ===");
        for (i, l) in lnam_chunks.iter().enumerate() {
            let d = l.data();
            let name = String::from_utf8_lossy(d);
            println!("  [{i}] {name}");
        }
        println!();
    }

    // --- Recompile (round-trip) ---
    if let Some(out_path) = &cli.compress_out {
        // classify() reads the codec bytes at offset 8, which is the codec
        // chunk for both XFIR- and plain-RIFX-wrapped files.
        match director_rifx::afterburner::classify(&data) {
            "afterburner" => {
                let af = director_rifx::afterburner::parse_afterburner(&data)
                    .map_err(|e| format!("decompression failed: {e}"))?;
                let out = director_rifx::afterburner::compress_parsed(&af)
                    .map_err(|e| format!("compression failed: {e}"))?;
                std::fs::write(out_path, &out)?;
                let kind = if is_compressed { "Afterburner XFIR" } else { "Afterburner RIFX" };
                println!("Recompiled ({kind}): {} → {} ({} bytes, source {} bytes)",
                    path.display(), out_path.display(), out.len(), data.len());
            }
            "memory-map" => {
                return Err("memory-map file (MV93/MC95) — Afterburner recompression not supported".into());
            }
            _ => {
                // Plain RIFX with no known codec: re-serialize the chunk tree.
                let out = director_rifx::chunk::write_chunk(&root);
                std::fs::write(out_path, &out)?;
                println!("Recompiled (RIFX tree): {} → {} ({} bytes, source {} bytes)",
                    path.display(), out_path.display(), out.len(), data.len());
            }
        }
        return Ok(());
    }

    // --- Palette resolution report ---
    if cli.palettes {
        export::report_palettes(&root);
    }

    // --- Export project ---
    let name = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("export");
        println!("Exporting project to {}...", out.display());
        export::export_project(&root, out, name, cli.lasm)?;
        println!("Export complete.");

    Ok(())
}

/// Director file extensions we try to process when walking a directory.
const DIRECTOR_EXTS: [&str; 3] = ["cct", "dcr", "cst"];

/// Directory mode: recursively find Director files under `dir` and export each
/// one into `export_root`, mirroring the input tree. HTML stubs (cct downloads
/// that are really error pages) are skipped. `--threads N` runs the per-file
/// work on N worker threads (default 1).
fn run_directory(cli: &Cli, dir: &Path, export_root: &Path) -> Result<(), Box<dyn std::error::Error>> {

    let mut files: Vec<PathBuf> = Vec::new();
    collect_director_files(dir, &mut files)?;
    files.sort();

    let threads = cli.threads.unwrap_or(1).max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let ok = std::sync::atomic::AtomicUsize::new(0);
    let skip = std::sync::atomic::AtomicUsize::new(0);
    let fail = std::sync::atomic::AtomicUsize::new(0);
    use std::sync::atomic::Ordering::Relaxed;

    std::thread::scope(|s| {
        let files = &files;
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Relaxed);
                if i >= files.len() {
                    break;
                }
                match export_one(&files[i], dir, export_root, cli.lasm) {
                    ExportOutcome::Ok => {
                        ok.fetch_add(1, Relaxed);
                        println!("OK   {}", files[i].display());
                    }
                    ExportOutcome::Skipped(reason) => {
                        skip.fetch_add(1, Relaxed);
                        println!("SKIP ({reason}): {}", files[i].display());
                    }
                    ExportOutcome::Failed(e) => {
                        fail.fetch_add(1, Relaxed);
                        println!("FAIL {}: {e}", files[i].display());
                    }
                }
            });
        }
    });

    let ok = ok.load(Relaxed);
    let skip = skip.load(Relaxed);
    let fail = fail.load(Relaxed);
    println!(
        "exported ok={ok} skip={skip} fail={fail} files={}",
        ok + skip + fail
    );
    Ok(())
}

enum ExportOutcome {
    Ok,
    Skipped(&'static str),
    Failed(String),
}

/// Export a single Director file under `export_root`, mirroring its path
/// relative to `dir`. Read, HTML-stub check, parse and export all happen here
/// so the caller can run this on a worker thread.
fn export_one(file: &Path, dir: &Path, export_root: &Path, lasm: bool) -> ExportOutcome {
    let data = match std::fs::read(file) {
        Ok(d) => d,
        Err(e) => return ExportOutcome::Failed(e.to_string()),
    };
    // Skip HTML error pages (named .cct/.dcr but actually HTML).
    if data.len() >= 15 && data[..15].windows(9).any(|w| w == b"<!DOCTYPE") {
        return ExportOutcome::Skipped("html");
    }
    let root = match director_rifx::read_bytes(&data) {
        Ok(r) => r,
        Err(e) => return ExportOutcome::Failed(e.to_string()),
    };
    // Mirror the input tree: <export_root>/<rel_dir>/<stem>/
    let rel = file
        .strip_prefix(dir)
        .unwrap_or(file)
        .parent()
        .unwrap_or(Path::new(""));
    let out_dir = export_root.join(rel);
    let name = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("export");
    if let Err(e) = export::export_project(&root, &out_dir, name, lasm) {
        return ExportOutcome::Failed(e.to_string());
    }
    ExportOutcome::Ok
}

/// Recursively collect `.cct` / `.dcr` / `.cst` files under `dir`.
fn collect_director_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_director_files(&path, out)?;
        } else if let Some(ext) = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
        {
            if DIRECTOR_EXTS.contains(&ext.as_str()) {
                out.push(path);
            }
        }
    }
    Ok(())
}
