//! Memory map (mmap) chunk.
//!
//! The memory map is an array of chunk resource entries, indexed by resource ID.
//! It serves as a directory of all chunks in the Director file.

use director_rifx::{Chunk, Endian, FourCC};
use crate::ParseError;

/// A single memory map entry pointing to a resource in the file.
#[derive(Debug, Clone)]
pub struct MmapEntry {
    /// The FourCC identifying the chunk type.
    pub fourcc: FourCC,
    /// Address (offset) of the chunk from the start of the file.
    ///
    /// NOTE: For Director 4/5/6 this is a u16 from the mmap entry.
    /// For D8+ the entry format changes. Large files may have
    /// addresses that overflow u16. This will need revisiting.
    pub address: u16,
    /// Length of the chunk data.
    pub length: u32,
    /// Resource ID (index in the mmap array).
    pub resource_id: u32,
}

/// The parsed memory map (mmap chunk).
#[derive(Debug, Clone)]
pub struct MemoryMap {
    pub entries: Vec<MmapEntry>,
    pub header_length: u16,
    pub entry_length: u16,
    pub allocated_elements: u32,
    pub used_elements: u32,
}

/// Parse an mmap chunk from its raw data.
pub fn read_mmap(chunk: &Chunk) -> Result<MemoryMap, ParseError> {
    let data = chunk.data();
    let endian = chunk.endian;
    if data.len() < 20 {
        return Err(ParseError::InvalidData(
            "mmap chunk too small".into(),
        ));
    }

    let mut pos = 0;

    // Chunk header
    let header_length = read_u16(data, &mut pos, endian);
    let entry_length = read_u16(data, &mut pos, endian);
    let allocated_elements = read_u32(data, &mut pos, endian);
    let used_elements = read_u32(data, &mut pos, endian);

    // Skip: junk_entry_position (4 bytes)
    pos += 4;
    // Skip: unknown (4 bytes)
    pos += 4;
    // Skip: free_entry_position (4 bytes)
    pos += 4;

    // Align to the actual entry data start based on header_length
    if header_length > 0 {
        pos = header_length as usize;
    }

    let mut entries = Vec::with_capacity(used_elements as usize);
    for rid in 0..used_elements {
        if pos + entry_length as usize > data.len() {
            break;
        }

        let cc_bytes: [u8; 4] = data[pos..pos + 4].try_into().unwrap();
        let fourcc = FourCC(cc_bytes);

        // Skip "free" and "junk" entries (deleted/unused chunks)
        if fourcc == FourCC(*b"free") || fourcc == FourCC(*b"junk") {
            pos += entry_length as usize;
            continue;
        }

        // The FourCC is stored as raw bytes (always ASCII, same in BE and LE).
        // Address and length are stored in the file's native endianness.
        let address = match endian {
            Endian::Big => u16::from_be_bytes([data[pos + 4], data[pos + 5]]),
            Endian::Little => u16::from_le_bytes([data[pos + 4], data[pos + 5]]),
        };
        let length = match endian {
            Endian::Big => u32::from_be_bytes([data[pos + 6], data[pos + 7], data[pos + 8], data[pos + 9]]),
            Endian::Little => u32::from_le_bytes([data[pos + 6], data[pos + 7], data[pos + 8], data[pos + 9]]),
        };

        entries.push(MmapEntry {
            fourcc,
            address,
            length,
            resource_id: rid,
        });

        pos += entry_length as usize;
    }

    Ok(MemoryMap {
        entries,
        header_length,
        entry_length,
        allocated_elements,
        used_elements,
    })
}

fn read_u16(data: &[u8], pos: &mut usize, endian: Endian) -> u16 {
    let v = match endian {
        Endian::Big => u16::from_be_bytes([data[*pos], data[*pos + 1]]),
        Endian::Little => u16::from_le_bytes([data[*pos], data[*pos + 1]]),
    };
    *pos += 2;
    v
}

fn read_u32(data: &[u8], pos: &mut usize, endian: Endian) -> u32 {
    let v = match endian {
        Endian::Big => u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]),
        Endian::Little => u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]),
    };
    *pos += 4;
    v
}
