//! Director cast library and cast member types.
//!
//! Casts (CASt/CAS\*) hold the actual resources: scripts, images, sounds, etc.
//! Each cast member has a type and member-specific data.
//!
//! Format reference: ScummVM Director engine cast.cpp (loadCastData)

use director_rifx::{Chunk, Endian};
use crate::ParseError;

/// Types of cast members in Director.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastMemberType {
    Bitmap,
    FilmLoop,
    Text,
    Palette,
    Picture,
    Sound,
    Button,
    Shape,
    Movie,
    DigitalVideo,
    Script,
    Xtra,
    Frame,
    Font,
    Unknown(u16),
}

impl CastMemberType {
    pub fn from_raw(raw: u16) -> Self {
        match raw {
            1 => CastMemberType::Bitmap,
            2 => CastMemberType::FilmLoop,
            3 => CastMemberType::Text,
            4 => CastMemberType::Palette,
            5 => CastMemberType::Picture,
            6 => CastMemberType::Sound,
            7 => CastMemberType::Button,
            8 => CastMemberType::Shape,
            9 => CastMemberType::Movie,
            10 => CastMemberType::DigitalVideo,
            11 => CastMemberType::Script,
            12 => CastMemberType::Xtra,
            13 => CastMemberType::Frame,
            14 => CastMemberType::Font,
            n => CastMemberType::Unknown(n),
        }
    }

    /// Try to parse D3 bitmap dimensions from the cast data header.
    /// D3 bitmaps have: u16 bytes + Rect initialRect + Rect boundingRect + regY + regX
    /// Returns (width, height) if parseable.
    pub fn parse_d3_bitmap_dims(data: &[u8], endian: Endian) -> Option<(u16, u16)> {
        if data.len() < 20 {
            return None;
        }
        // D3 header starts at offset 0 (no D4 dataSize/infoSize/type header):
        //   u16 bytes (2 bytes)
        //   s16 top, left, bottom, right (8 bytes) — initialRect
        //   s16 top, left, bottom, right (8 bytes) — boundingRect
        //   s16 regY (2 bytes)
        // Width/height from initialRect
        let rect_dims = |top: i16, left: i16, bottom: i16, right: i16| -> Option<(u16, u16)> {
            let w = (right as i32 - left as i32).unsigned_abs() as u16;
            let h = (bottom as i32 - top as i32).unsigned_abs() as u16;
            if w > 0 && w < 10000 && h > 0 && h < 10000 { Some((w, h)) } else { None }
        };

        let top = read_s16(data, 2, endian);
        let left = read_s16(data, 4, endian);
        let bottom = read_s16(data, 6, endian);
        let right = read_s16(data, 8, endian);
        if let Some(dims) = rect_dims(top, left, bottom, right) {
            return Some(dims);
        }

        // Try offset 8 (skip 8-byte D4-ish header: dataSize + infoSize + type + flags)
        let top2 = read_s16(data, 8, endian);
        let left2 = read_s16(data, 10, endian);
        let bottom2 = read_s16(data, 12, endian);
        let right2 = read_s16(data, 14, endian);
        if let Some(dims) = rect_dims(top2, left2, bottom2, right2) {
            return Some(dims);
        }

        None
    }

    pub fn name(&self) -> &'static str {
        match self {
            CastMemberType::Bitmap => "Bitmap",
            CastMemberType::FilmLoop => "FilmLoop",
            CastMemberType::Text => "Text",
            CastMemberType::Palette => "Palette",
            CastMemberType::Picture => "Picture",
            CastMemberType::Sound => "Sound",
            CastMemberType::Button => "Button",
            CastMemberType::Shape => "Shape",
            CastMemberType::Movie => "Movie",
            CastMemberType::DigitalVideo => "DigitalVideo",
            CastMemberType::Script => "Script",
            CastMemberType::Xtra => "Xtra",
            CastMemberType::Frame => "Frame",
            CastMemberType::Font => "Font",
            CastMemberType::Unknown(n) => return Box::leak(format!("Unknown({n})").into_boxed_str()),
        }
    }
}

/// Parse a CAS* (cast library member list) chunk: a list of u32 big-endian
/// CASt resource ids, one per cast member.
///
/// The cast member NUMBER of entry `i` is `minMember + i` (minMember comes
/// from the DRCF config chunk). The stored `paletteId` in D7 bitmap
/// specificData is such a member number — to find the palette you must map
/// number -> CASt resource via this list, then follow that member's KEY* CLUT
/// child. (Verified against hh_room_pool/hh_room_bar/hh_room_cafe/hh_room_pizza:
/// stored 53 -> member 53 -> pool_palette res 628; stored 2 -> pizzapaletti 700.)
pub fn parse_cast_member_list(data: &[u8]) -> Vec<u32> {
    data.chunks_exact(4)
        .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Find where the D3 child resource entries table ends using robust scanning.
///
/// Phase 1: byte-by-byte scan for first valid entry (known fourCC + valid tag/child).
/// Phase 2: 12-byte stride, accepting ANY valid tag (1000-15000) + child (<5000),
///          regardless of fourCC. This handles entries with unknown fourCC types.
///          Breaks after 3 consecutive failures to avoid RLE pixel false positives.
pub fn find_d3_entries_end(data: &[u8]) -> Option<usize> {
    let sl = data.len();
    if sl < 16 {
        return None;
    }

    let known_fourccs: &[&[u8]] = &[
        b"DTIB", b"muhT", b"TULC", b"FCRD", b"rcsL", b"tXTS", b" DNS",
        b"TXTS", b"ediM", b"fniC", b"*SAC",
    ];

    // Phase 1: byte-by-byte to find first valid entry
    let mut first_pos = None;
    for p in 0..sl.saturating_sub(11) {
        let c = &data[p+8..p+12];
        if known_fourccs.contains(&c) {
            let tag = u32::from_le_bytes([data[p], data[p+1], data[p+2], data[p+3]]);
            let child = u32::from_le_bytes([data[p+4], data[p+5], data[p+6], data[p+7]]);
            if (1000..=15000).contains(&tag) && child < 5000 {
                first_pos = Some(p);
                break;
            }
        }
    }

    let first_pos = first_pos?;
    let mut last_valid_end = first_pos + 12;
    let mut consecutive_fails = 0;
    let max_fails = 3;

    // Phase 2: 12-byte stride, tag+child validation (ignore fourCC)
    let mut pos = first_pos + 12;
    while pos + 12 <= sl {
        let tag = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]);
        let child = u32::from_le_bytes([data[pos+4], data[pos+5], data[pos+6], data[pos+7]]);

        if (1000..=15000).contains(&tag) && child < 5000 {
            last_valid_end = pos + 12;
            consecutive_fails = 0;
        } else {
            consecutive_fails += 1;
            if consecutive_fails >= max_fails {
                break;
            }
        }

        pos += 12;
    }

    Some(last_valid_end)
}

/// Read a script cast member's scriptId from its info block (D4+).
///
/// The info block layout (shared with the name reader) has the scriptId as
/// the 5th i32 (offset 16): `dataOffset, i32, i32, i32, scriptId`. This id is
/// an INDEX into the LctX ScriptContext's entries (entry `scriptId - 1`'s
/// `sectionId` is the LSCR resource id holding the bytecode). Verified against
/// hh_room_park/fuse_client: member 652 park_a -> scriptId 4 -> LctX entry[3]
/// -> Lscr 770; fuse Object API -> 4 -> Lscr 142.
pub fn read_member_script_id(info: &[u8]) -> Option<i32> {
    if info.len() < 20 {
        return None;
    }
    Some(i32::from_be_bytes([info[16], info[17], info[18], info[19]]))
}

/// Read a cast member's name from its info block (D4+).
///
/// Port of LibreShockwave CastMemberChunk.cpp readNameFromInfo:
///   i32 dataOffset  — offset of the string table within the info block
///   i32, i32, i32   — unused
///   i32 scriptId
///   at dataOffset: u16 offsetTableLen, then offsetTableLen × i32 offsets,
///   then itemsLen i32; the name is a Pascal string at offsets[1].
pub fn read_member_name(info: &[u8]) -> Option<String> {
    if info.len() < 20 {
        return None;
    }
    let data_offset = i32::from_be_bytes([info[0], info[1], info[2], info[3]]) as i64;
    if data_offset <= 0 || data_offset as usize >= info.len() {
        return None;
    }
    let mut p = data_offset as usize;
    if p + 2 > info.len() {
        return None;
    }
    let offset_table_len = u16::from_be_bytes([info[p], info[p + 1]]) as usize;
    p += 2;
    if offset_table_len == 0 || offset_table_len > 10000 {
        return None;
    }
    if p + offset_table_len * 4 + 4 > info.len() {
        return None;
    }
    let mut offsets = Vec::with_capacity(offset_table_len);
    for _ in 0..offset_table_len {
        let o = i32::from_be_bytes([info[p], info[p + 1], info[p + 2], info[p + 3]]) as i64;
        offsets.push(o);
        p += 4;
    }
    let items_len = i32::from_be_bytes([info[p], info[p + 1], info[p + 2], info[p + 3]]) as i64;
    let items_start = p + 4;
    if offsets.len() <= 1 {
        return None;
    }
    let name_offset = offsets[1];
    let name_end = if offsets.len() > 2 { offsets[2] } else { items_len };
    let name_len = name_end - name_offset;
    if name_offset < 0 || name_len <= 0 {
        return None;
    }
    let name_start = items_start as i64 + name_offset;
    if name_start < 0 || (name_start as usize) >= info.len() {
        return None;
    }
    let mut np = name_start as usize;
    let pascal_len = info[np] as usize;
    np += 1;
    if pascal_len == 0 || np + pascal_len > info.len() {
        return None;
    }
    let name = String::from_utf8_lossy(&info[np..np + pascal_len]).to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// A parsed cast member (CASt chunk data).
#[derive(Debug, Clone)]
pub struct CastMember {
    pub member_type: CastMemberType,
    pub member_id: u16,
    pub flags1: u8,
    pub cast_data_size: u32,
    pub cast_info_size: u32,
    pub raw_data: Vec<u8>,
    /// Child resource FourCCs embedded in the CASt data (D3 format).
    /// For D3 bitmaps, this includes "BITD" (bitmap data) and "Thum" (thumbnail).
    pub child_tags: Vec<String>,
}

impl CastMember {
    /// Human-readable summary of the cast member.
    pub fn summary(&self) -> String {
        let extra = if self.child_tags.is_empty() {
            String::new()
        } else {
            format!(" children={:?}", self.child_tags)
        };
        format!("Cast #{}, type={} (flags=0x{:02x}){}",
            self.member_id,
            self.member_type.name(),
            self.flags1,
            extra,
        )
    }
}

/// Parse a CASt (cast member) chunk.
///
/// Director 4 format:
///   u16 castDataSize  (2 bytes)
///   u32 castInfoSize  (4 bytes)
///   u8  type          (1 byte)
///   u8  flags1        (1 byte, if type != 0xFF)
///   ... castData ...
///   ... castInfo ...
///
/// Director 5+ format:
///   u32 type          (4 bytes)
///   u32 castInfoSize  (4 bytes)
///   u32 castDataSize  (4 bytes)
///   ... castInfo ...
///   ... castData ...
pub fn read_cast_member(chunk: &Chunk) -> Result<CastMember, ParseError> {
    let data = chunk.data();
    let endian = chunk.endian;
    if data.len() < 6 {
        return Err(ParseError::InvalidData("CASt chunk too small".into()));
    }

    // Try D5+ format first (type as u32, then sizes)
    if data.len() >= 12 {
        let candidate_type = read_u32_with(data, 0, endian);
        let candidate_info_size = read_u32_with(data, 4, endian);
        let candidate_data_size = read_u32_with(data, 8, endian);

        // Heuristic: if the "type" value is in 1..20 (valid cast types), it's D5+
        if candidate_type >= 1 && candidate_type <= 20
            && candidate_info_size < data.len() as u32
            && candidate_data_size < data.len() as u32 {
            let member_type = CastMemberType::from_raw(candidate_type as u16);
            return Ok(CastMember {
                member_type,
                member_id: 0,
                flags1: 0,
                cast_data_size: candidate_data_size,
                cast_info_size: candidate_info_size,
                raw_data: data.to_vec(),
                child_tags: Vec::new(),
            });
        }
    }

    // D4 format
    let mut pos = 0;
    let cast_data_size = read_u16(data, &mut pos, endian) as u32;
    let cast_info_size = read_u32(data, &mut pos, endian);
    let type_raw = data[pos] as u16;
    let flags1 = if type_raw != 0xFF && pos + 1 < data.len() {
        data[pos + 1]
    } else {
        0
    };

    let member_type = CastMemberType::from_raw(type_raw);

    // Always scan for D3 child resource tags (runs regardless of D4 result)
    // D3 CASt data contains embedded child resource tags like "DTIB" (BITD reversed).
    // When found, these override the D4 type detection since D3 doesn't have a type byte.
    if data.len() >= 16 {
        let child_tags = scan_child_tags(data);
        if !child_tags.is_empty() {
            let infer_type = infer_cast_type(&child_tags);
            // Only override D4 if the D3 inference found a specific type
            if infer_type != CastMemberType::Unknown(0) {
                return Ok(CastMember {
                    member_type: infer_type,
                    member_id: 0,
                    flags1,
                    cast_data_size,
                    cast_info_size,
                    raw_data: data.to_vec(),
                    child_tags,
                });
            }
        }
    }

    Ok(CastMember {
        member_type,
        member_id: 0,
        flags1,
        cast_data_size,
        cast_info_size,
        raw_data: data.to_vec(),
        child_tags: Vec::new(),
    })
}

/// Scan CASt data for D3 child resource FourCC tags.
/// D3 format embeds child resource entries in the CASt data.
/// Each entry: u32 tag (reversed FourCC) + u32 childId + u32 fourCC
fn scan_child_tags(data: &[u8]) -> Vec<String> {
    // Known FourCC values as they appear in D3 Afterburner entry data.
    // These are stored as-is (not reversed) in the 12-byte entries.
    let known_patterns: &[&[u8]] = &[
        b"DTIB",  // BITD reversed — bitmap data
        b"muhT",  // Thum reversed — thumbnail
        b"rcsL",  // Lscr reversed — Lingo script
        b"tXTS",  // STXt reversed — styled text
        b" DNS",  // "SND " reversed — sound
        b"TULC",  // CLUT reversed — palette
        b"FCRD",  // DRCF reversed — config
        b"TXTS",  // STXT reversed — styled text (alternate 4CC)
        b"ediM",  // Mide reversed — sound
        b"fniC",  // CinF reversed — cast info
        b"*SAC",  // CAS* reversed — cast
    ];

    let mut tags = Vec::new();
    let mut offset = 0;
    while offset + 4 <= data.len() {
        for pat in known_patterns {
            if offset + pat.len() <= data.len() && &data[offset..offset + pat.len()] == *pat {
                // Found a tag — reverse to get canonical FourCC
                let mut cc = [data[offset], data[offset+1], data[offset+2], data[offset+3]];
                cc.reverse();
                if let Ok(s) = std::str::from_utf8(&cc) {
                    if !tags.contains(&s.to_string()) {
                        tags.push(s.trim_end().to_string());
                    }
                }
                break;
            }
        }
        offset += 1;
    }
    tags
}

/// Infer cast member type from child resource tags.
fn infer_cast_type(tags: &[String]) -> CastMemberType {
    for tag in tags {
        match tag.as_str() {
            "BITD" | "Thum" => return CastMemberType::Bitmap,
            "LSCR" | "Lscr" => return CastMemberType::Script,
            "STXT" | "Stxt" => return CastMemberType::Text,
            "SND " | "snd " => return CastMemberType::Sound,
            "CLUT" => return CastMemberType::Palette,
            _ => {}
        }
    }
    CastMemberType::Unknown(0)
}

fn read_u32_with(data: &[u8], pos: usize, endian: Endian) -> u32 {
    match endian {
        Endian::Big => u32::from_be_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]),
        Endian::Little => u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]),
    }
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

fn read_s16(data: &[u8], pos: usize, endian: Endian) -> i16 {
    match endian {
        Endian::Big => i16::from_be_bytes([data[pos], data[pos + 1]]),
        Endian::Little => i16::from_le_bytes([data[pos], data[pos + 1]]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cast_member_list_be() {
        // Two entries: resource 0x274 (628) and 0x2cd (717), big-endian.
        let data = [0x00, 0x00, 0x02, 0x74, 0x00, 0x00, 0x02, 0xcd];
        assert_eq!(parse_cast_member_list(&data), vec![628, 717]);
    }
}
