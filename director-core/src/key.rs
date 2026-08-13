//! KEY\* chunk parser.
//!
//! The KEY\* (key table) maps resources to their parent resources and links
//! cast members to their sub-resources (e.g. a bitmap cast member linking to
//! its BITD and CLUT data chunks).
//!
//! Format (endianness follows the container — XFIR-wrapped CCTs store it
//! little-endian, plain RIFX/Mac files big-endian):
//!   u16 entrySize         — should always be 12 (3 × u32)
//!   u16 entrySize2        — same as entrySize
//!   u32 entryCount        — total number of entries (may include unused)
//!   u32 usedCount         — number of actual entries
//!   For each used entry:
//!     u32 childIndex      — resource ID of the child
//!     u32 parentIndex     — resource ID of the parent
//!     u32 childTag        — FourCC of the child resource

use director_rifx::Chunk;
use crate::ParseError;

/// The KEY\* key table: maps parent resources to their child resources.
#[derive(Debug, Clone)]
pub struct KeyTable {
    pub entry_size: u16,
    pub entry_count: u32,
    pub used_count: u32,
    pub entries: Vec<KeyEntry>,
}

/// A single entry in the key table, linking a child resource to its parent.
#[derive(Debug, Clone)]
pub struct KeyEntry {
    pub child_index: u32,
    pub parent_index: u32,
    pub child_tag: [u8; 4],
}

/// Parse a KEY\* chunk.
///
/// Standard (D4+) format:
///   u16 entrySize         — should be 12 (3 × u32)
///   u16 entrySize2        — same
///   u32 entryCount
///   u32 usedCount
///   Entries: childIndex(u32) + parentIndex(u32) + childTag(u32)  (each 12 bytes)
///
/// Director 3 format (no header, used by Afterburner-compressed Habbo CCTs):
///   u8  padding           — always 0x00
///   Entries: fourCC(u32 BE bytes) + tag(u32 LE) + childId(u32 LE)  (each 12 bytes)
pub fn read_key(chunk: &Chunk) -> Result<KeyTable, ParseError> {
    let data = chunk.data();
    if data.len() < 12 {
        return Err(ParseError::InvalidData("KEY* chunk too small".into()));
    }

    // Standard D4+ format: u16 entrySize (12), u16 entrySize2, u32 entryCount,
    // u32 usedCount, then 12-byte entries (childIndex, parentIndex, childTag).
    // The endianness follows the container: XFIR-wrapped (Afterburner CCTs)
    // files store it little-endian, plain RIFX/Mac files big-endian. Try LE
    // first (the Habbo CCTs), then BE, then the D3 no-header format.

    // Try standard D4+ little-endian first.
    let mut pos = 0;
    let entry_size = read_u16_le(data, &mut pos);
    let _entry_size2 = read_u16_le(data, &mut pos);
    let entry_count = read_u32_le(data, &mut pos);
    let used_count = read_u32_le(data, &mut pos);

    let read_be = entry_size != 12;
    if read_be {
        pos = 0;
    }

    if !read_be {
        // Standard D4+ little-endian format (XFIR-wrapped CCTs).
        let entries = read_entries(
            data,
            &mut pos,
            used_count.min(entry_count).min(100000) as usize,
            |d, p| {
                let v = u32::from_le_bytes([d[*p], d[*p + 1], d[*p + 2], d[*p + 3]]);
                *p += 4;
                v
            },
        );
        return Ok(KeyTable {
            entry_size,
            entry_count,
            used_count,
            entries,
        });
    }

    // Fall back to big-endian D4+ (plain RIFX / Mac-origin files).
    let entry_size_be = read_u16_be(data, &mut pos);
    let _entry_size2_be = read_u16_be(data, &mut pos);
    let entry_count_be = read_u32_be(data, &mut pos);
    let used_count_be = read_u32_be(data, &mut pos);

    if entry_size_be == 12 {
        let entries = read_entries(
            data,
            &mut pos,
            used_count_be.min(entry_count_be).min(100000) as usize,
            |d, p| {
                let v = u32::from_be_bytes([d[*p], d[*p + 1], d[*p + 2], d[*p + 3]]);
                *p += 4;
                v
            },
        );
        return Ok(KeyTable {
            entry_size: entry_size_be,
            entry_count: entry_count_be,
            used_count: used_count_be,
            entries,
        });
    }

    // D3 format: no standard header, just 1-byte padding (0x00) followed by 12-byte entries.
    // In D3, child_index holds a resource tag/offset (not a direct cast member index).
    //    parent_index holds the cast member ID.
    // Detect: first byte is 0x00, bytes 1..5 look like a FourCC (printable ASCII),
    //   and remaining data divides cleanly into 12-byte entries (padding ≤ 3 bytes).
    let d3_data_ok = data.len() >= 5
        && data[0] == 0x00
        && data[1..5].iter().all(|b| b.is_ascii_graphic())
        && (data.len() - 1) % 12 <= 3;  // allow 0-3 bytes trailing padding

    if d3_data_ok {
        let entry_size = 12u16;
        let data_start = 1usize;
        let max_entries = ((data.len() - data_start) / 12).min(100000);
        let mut entries = Vec::with_capacity(max_entries);

        let mut p = data_start;
        while p + 12 <= data.len() && entries.len() < 100000 {
            let fourcc_bytes = [data[p], data[p+1], data[p+2], data[p+3]];
            let child_index = u32::from_le_bytes([data[p+4], data[p+5], data[p+6], data[p+7]]);
            let parent_index = u32::from_le_bytes([data[p+8], data[p+9], data[p+10], data[p+11]]);

            entries.push(KeyEntry {
                child_index,
                parent_index,
                child_tag: fourcc_bytes,
            });
            p += 12;
        }

        return Ok(KeyTable {
            entry_size,
            entry_count: entries.len() as u32,
            used_count: entries.len() as u32,
            entries,
        });
    }

    // Neither D4+ nor recognized D3 format
    Err(ParseError::InvalidData(
        format!("KEY* unexpected entry size: {entry_size}, expected 12")
    ))
}

/// Read `count` 12-byte KEY* entries at `pos`, using the supplied u32 reader.
/// Either endianness yields the numeric MKTAG value, which is converted back
/// to its canonical FourCC spelling with `to_be_bytes()`.
fn read_entries(
    data: &[u8],
    pos: &mut usize,
    count: usize,
    read_u32: impl Fn(&[u8], &mut usize) -> u32,
) -> Vec<KeyEntry> {
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if *pos + 12 > data.len() {
            break;
        }
        let child_index = read_u32(data, pos);
        let parent_index = read_u32(data, pos);
        let child_tag_raw = read_u32(data, pos);
        entries.push(KeyEntry {
            child_index,
            parent_index,
            child_tag: child_tag_raw.to_be_bytes(),
        });
    }
    entries
}

fn read_u16_le(data: &[u8], pos: &mut usize) -> u16 {
    let v = u16::from_le_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    v
}

fn read_u16_be(data: &[u8], pos: &mut usize) -> u16 {
    let v = u16::from_be_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    v
}

fn read_u32_le(data: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    v
}

fn read_u32_be(data: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use director_rifx::chunk::read_chunk;

    fn make_key_chunk(data: Vec<u8>) -> Chunk {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"KEY*");
        raw.extend_from_slice(&(data.len() as u32).to_be_bytes());
        raw.extend_from_slice(&data);
        let mut pos = 0u64;
        let (chunk, _) = read_chunk(&raw, &mut pos).unwrap();
        chunk
    }

    #[test]
    fn key_be_d4_parse() {
        // Big-endian D4+ KEY* (plain RIFX / Mac files): entrySize=12, entrySize2=12,
        // count=2, used=2, then (child, parent, tag) triples with canonical tags.
        let mut d = Vec::new();
        d.extend_from_slice(&12u16.to_be_bytes());
        d.extend_from_slice(&12u16.to_be_bytes());
        d.extend_from_slice(&2u32.to_be_bytes());
        d.extend_from_slice(&2u32.to_be_bytes());
        d.extend_from_slice(&151u32.to_be_bytes());
        d.extend_from_slice(&5u32.to_be_bytes());
        d.extend_from_slice(&0x434C5554u32.to_be_bytes()); // "CLUT"
        d.extend_from_slice(&153u32.to_be_bytes());
        d.extend_from_slice(&6u32.to_be_bytes());
        d.extend_from_slice(&0x434C5554u32.to_be_bytes()); // "CLUT"

        let kt = read_key(&make_key_chunk(d)).unwrap();
        assert_eq!(kt.entry_size, 12);
        assert_eq!(kt.entries.len(), 2);
        assert_eq!(kt.entries[0].child_index, 151);
        assert_eq!(kt.entries[0].parent_index, 5);
        assert_eq!(&kt.entries[0].child_tag, b"CLUT");
        assert_eq!(kt.entries[1].child_index, 153);
        assert_eq!(&kt.entries[1].child_tag, b"CLUT");
    }

    #[test]
    fn key_le_d4_parse() {
        // Little-endian D4+ KEY* (XFIR-wrapped CCTs): tags stored as LE u32 of
        // the MKTAG value, e.g. "DTIB" which reads back as canonical "BITD".
        let mut d = Vec::new();
        d.extend_from_slice(&12u16.to_le_bytes());
        d.extend_from_slice(&12u16.to_le_bytes());
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&42u32.to_le_bytes());
        d.extend_from_slice(&7u32.to_le_bytes());
        d.extend_from_slice(&0x42495444u32.to_le_bytes()); // MKTAG('B','I','T','D') LE
        d.extend_from_slice(&43u32.to_le_bytes());
        d.extend_from_slice(&8u32.to_le_bytes());
        d.extend_from_slice(&0x42495444u32.to_le_bytes());

        let kt = read_key(&make_key_chunk(d)).unwrap();
        assert_eq!(kt.entry_size, 12);
        assert_eq!(kt.entries.len(), 2);
        assert_eq!(kt.entries[0].child_index, 42);
        assert_eq!(kt.entries[0].parent_index, 7);
        assert_eq!(&kt.entries[0].child_tag, b"BITD");
        assert_eq!(&kt.entries[1].child_tag, b"BITD");
    }
}
