//! RIFX container format I/O for Director/Shockwave files.
//!
//! RIFX is big-endian RIFF (Resource Interchange File Format).
//! Director files use RIFX as the root container, with nested chunks
//! identified by FourCC codes.

use std::fmt;
use std::io;

pub mod afterburner;
pub mod chunk;

pub use afterburner::is_compressed;
pub use chunk::{Chunk, Endian, FourCC};

/// The root FourCC for Director files.
pub const RIFX_MAGIC: FourCC = FourCC(*b"RIFX");

/// Read a complete Director file into a RIFX root Chunk.
pub fn read_file<P: AsRef<std::path::Path>>(path: P) -> Result<Chunk> {
    let data = std::fs::read(path.as_ref())?;
    read_bytes(&data)
}

/// Read from an in-memory buffer.
pub fn read_bytes(data: &[u8]) -> Result<Chunk> {
    if data.len() < 8 {
        return Err(Error::Truncated("too small".into()));
    }

    // Afterburner content arrives either in an XFIR wrapper (compressed CCTs)
    // or in a plain RIFX wrapper whose codec chunk is FGDM/FGDC/memory-map
    // (no length field after the codec — the body starts immediately). Both
    // must be fully decompressed, never parsed as a raw chunk tree.
    let is_afterburner = is_compressed(data)
        || (&data[0..4] == b"RIFX"
            && data.len() >= 12
            && matches!(
                afterburner::classify(data),
                "afterburner" | "memory-map"
            ));

    if is_afterburner {
        let file = afterburner::parse_afterburner(data)?;
        chunk_from_afterburner(file)
    } else {
        parse_rifx(data)
    }
}

/// Rebuild the chunk tree from a parsed Afterburner/memory-map file.
fn chunk_from_afterburner(file: afterburner::AfterburnerFile) -> Result<Chunk> {
    let mut root = parse_rifx(&file.rifx_data)?;
    // Attach source resource IDs to the reconstructed top-level chunks.
    for (child, sid) in root.children.iter_mut().zip(file.chunk_source_ids.iter()) {
        child.source_id = Some(*sid);
    }
    // If the source file was a memory-map (LE) file, propagate endianness
    // to all child chunks so their content data is read as little-endian.
    if file.endian == Endian::Little {
        set_endian_recursive(&mut root, Endian::Little);
    }
    Ok(root)
}

/// Recursively set the endianness on all children of a chunk.
fn set_endian_recursive(chunk: &mut Chunk, endian: Endian) {
    chunk.endian = endian;
    for child in &mut chunk.children {
        set_endian_recursive(child, endian);
    }
}

fn parse_rifx(data: &[u8]) -> Result<Chunk> {
    let mut pos = 0;
    let (chunk, _) = chunk::read_chunk(data, &mut pos)?;
    Ok(chunk)
}

/// Dump a chunk tree to string (for debugging).
pub fn dump_tree(chunk: &Chunk) -> String {
    let mut out = String::new();
    dump_chunk(chunk, 0, &mut out);
    out
}

fn dump_chunk(chunk: &Chunk, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let fourcc = chunk.fourcc();
    let len = chunk.data_len();
    let extra = if chunk.children.is_empty() {
        String::new()
    } else {
        format!(" ({} children)", chunk.children.len())
    };
    out.push_str(&format!("{indent}{fourcc}  len={len}{extra}\n"));
    for child in &chunk.children {
        dump_chunk(child, depth + 1, out);
    }
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Truncated(String),
    UnknownFormat(String),
    Compression(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Truncated(s) => write!(f, "truncated data: {s}"),
            Error::UnknownFormat(s) => write!(f, "unknown format: {s}"),
            Error::Compression(s) => write!(f, "compression error: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::read_chunk;

    #[test]
    fn roundtrip_simple_chunk() {
        let sub_data = b"hello";
        let sub_len = sub_data.len() as u32;

        let mut raw = Vec::new();
        raw.extend_from_slice(b"RIFX");
        let data_size: u32 = 8 + sub_len + (sub_len % 2);
        raw.extend_from_slice(&data_size.to_be_bytes());
        raw.extend_from_slice(b"TEST");
        raw.extend_from_slice(&sub_len.to_be_bytes());
        raw.extend_from_slice(sub_data);
        raw.push(0);

        let mut pos = 0u64;
        let (chunk, _) = read_chunk(&raw, &mut pos).unwrap();

        assert_eq!(chunk.fourcc(), FourCC(*b"RIFX"));
        assert_eq!(chunk.data_len(), data_size as u64);
        assert_eq!(chunk.children.len(), 1);
        let child = &chunk.children[0];
        assert_eq!(child.fourcc(), FourCC(*b"TEST"));
        assert_eq!(child.data_len(), sub_len as u64);
        assert_eq!(child.data(), b"hello");
    }

    #[test]
    fn roundtrip_multiple_children() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"RIFX");

        let mut child1 = Vec::new();
        child1.extend_from_slice(b"ONE ");
        child1.extend_from_slice(&3u32.to_be_bytes());
        child1.extend_from_slice(b"abc");
        child1.push(0);

        let mut child2 = Vec::new();
        child2.extend_from_slice(b"TWO ");
        child2.extend_from_slice(&2u32.to_be_bytes());
        child2.extend_from_slice(b"xy");

        let total_data = child1.len() + child2.len();
        raw.extend_from_slice(&(total_data as u32).to_be_bytes());
        raw.extend_from_slice(&child1);
        raw.extend_from_slice(&child2);

        let mut pos = 0u64;
        let (chunk, _) = read_chunk(&raw, &mut pos).unwrap();

        assert_eq!(chunk.fourcc(), FourCC(*b"RIFX"));
        assert_eq!(chunk.children.len(), 2);
        assert_eq!(chunk.children[0].fourcc(), FourCC(*b"ONE "));
        assert_eq!(chunk.children[0].data(), b"abc");
        assert_eq!(chunk.children[1].fourcc(), FourCC(*b"TWO "));
        assert_eq!(chunk.children[1].data(), b"xy");
    }

    #[test]
    fn detect_compressed() {
        assert!(!is_compressed(b"RIFX\x00\x00\x00\x00"));
        assert!(is_compressed(b"XFIR\x00\x00\x00\x00"));
        assert!(!is_compressed(b""));
    }
}
