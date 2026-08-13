//! RIFX chunk types and parser.

use crate::{Error, Result};

/// Byte order (endianness) of a chunk's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Big,
    Little,
}

impl Default for Endian {
    fn default() -> Self { Endian::Big }
}

/// A FourCC (four-character code) identifies a chunk type.
/// e.g. "RIFX", "mmap", "CASt", "Lscr"
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCC(pub [u8; 4]);

impl FourCC {
    pub fn from_bytes(b: &[u8]) -> Self {
        let mut arr = [0u8; 4];
        let len = b.len().min(4);
        arr[..len].copy_from_slice(&b[..len]);
        FourCC(arr)
    }

    pub fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("????")
    }
}

impl fmt::Display for FourCC {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in &self.0 {
            if b.is_ascii_graphic() || b == b' ' {
                write!(f, "{}", b as char)?;
            } else {
                write!(f, "\\x{b:02x}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for FourCC {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FourCC({self})")
    }
}

use std::fmt;

/// A parsed RIFX chunk.
///
/// RIFX chunks have:
/// - 4 bytes: FourCC identifier
/// - 4 bytes: length (big-endian u32)
/// - N bytes: chunk data
/// - 1 byte padding if N is odd
#[derive(Clone)]
pub struct Chunk {
    fourcc: FourCC,
    raw_data: Vec<u8>,
    pub children: Vec<Chunk>,
    pub offset: u64,
    header_len: u64,
    pub endian: Endian,
    /// Original resource ID in the source file (Afterburner/memory-map resources).
    pub source_id: Option<u32>,
}

impl Chunk {
    pub fn new(fourcc: FourCC) -> Self {
        Chunk {
            fourcc,
            raw_data: Vec::new(),
            children: Vec::new(),
            offset: 0,
            header_len: 0,
            endian: Endian::Big,
            source_id: None,
        }
    }

    pub fn fourcc(&self) -> FourCC {
        self.fourcc
    }

    pub fn data_len(&self) -> u64 {
        self.header_len
    }

    pub fn data(&self) -> &[u8] {
        &self.raw_data
    }

    pub fn into_data(self) -> Vec<u8> {
        self.raw_data
    }

    /// Check if this chunk has a specific FourCC.
    pub fn is(&self, cc: &[u8; 4]) -> bool {
        self.fourcc == FourCC(*cc)
    }

    /// Find the first child with a given FourCC.
    pub fn child(&self, cc: &[u8; 4]) -> Option<&Chunk> {
        self.children.iter().find(|c| c.is(cc))
    }

    /// Find all children with a given FourCC.
    pub fn children_by(&self, cc: &[u8; 4]) -> Vec<&Chunk> {
        let target = FourCC(*cc);
        self.children.iter().filter(|c| c.fourcc == target).collect()
    }
}

impl fmt::Debug for Chunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chunk")
            .field("fourcc", &self.fourcc)
            .field("data_len", &self.header_len)
            .field("children", &self.children.len())
            .finish()
    }
}

/// Read a single chunk starting at `pos` in `data`.
///
/// `parent_endian` is the endianness inherited from the parent container.
pub fn read_chunk(data: &[u8], pos: &mut u64) -> Result<(Chunk, u64)> {
    read_chunk_with_endian(data, pos, Endian::Big)
}

/// Read a chunk with a specified endianness for its content data.
/// The frame header (FourCC + size) is always big-endian (RIFX format).
pub fn read_chunk_with_endian(data: &[u8], pos: &mut u64, parent_endian: Endian) -> Result<(Chunk, u64)> {
    let start = *pos as usize;
    if start + 8 > data.len() {
        return Err(Error::Truncated("cannot read chunk header".into()));
    }

    let fourcc_bytes: [u8; 4] = data[start..start + 4].try_into().unwrap();
    let fourcc = FourCC(fourcc_bytes);

    let length_bytes: [u8; 4] = data[start + 4..start + 8].try_into().unwrap();
    let chunk_len = u32::from_be_bytes(length_bytes) as u64;

    let mut chunk = Chunk {
        fourcc,
        header_len: chunk_len,
        raw_data: Vec::new(),
        children: Vec::new(),
        offset: start as u64,
        endian: parent_endian,
        source_id: None,
    };

    *pos += 8;

    let available = data.len().saturating_sub(start + 8) as u64;
    let actual_len = chunk_len.min(available);

    if is_container(fourcc) {
        let end = *pos + actual_len;
        while *pos < end {
            if *pos as usize + 8 > data.len() {
                break;
            }
            if end.saturating_sub(*pos) < 8 {
                break;
            }
            match read_chunk_with_endian(data, pos, parent_endian) {
                Ok((child, new_pos)) => {
                    chunk.children.push(child);
                    *pos = new_pos;
                }
                Err(_) => break,
            }
        }
        let consumed = (*pos).min(start as u64 + 8 + chunk_len);
        let ls = consumed as usize;
        let le = (start as u64 + 8 + chunk_len).min(data.len() as u64) as usize;
        if le > ls {
            chunk.raw_data = data[ls..le].to_vec();
        }
    } else {
        let end = start + 8 + actual_len as usize;
        if end > start + 8 {
            chunk.raw_data = data[start + 8..end].to_vec();
        }
        *pos += actual_len;
    }

    if chunk_len % 2 == 1 {
        *pos = (*pos + 1).min(data.len() as u64);
    }

    Ok((chunk, *pos))
}

fn is_container(fourcc: FourCC) -> bool {
    fourcc == FourCC(*b"RIFX") || fourcc == FourCC(*b"LIST")
}

/// Serialize a chunk tree back to RIFX bytes (reverse of `read_chunk`).
///
/// Container chunks (RIFX/LIST) write their children; the length field covers
/// the serialized children (including their odd-length padding). Leaf chunks
/// write their raw data with one pad byte when the length is odd.
pub fn write_chunk(chunk: &Chunk) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(chunk.fourcc().as_bytes());

    if chunk.children.is_empty() {
        let data = chunk.data();
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
    } else {
        // Container: length covers the serialized children. No trailing pad
        // byte is added after the container body (matches the original files,
        // e.g. RIFX files with an odd-length body have no pad after it).
        let mut body = Vec::new();
        for c in &chunk.children {
            body.extend_from_slice(&write_chunk(c));
        }
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
    }
    out
}
