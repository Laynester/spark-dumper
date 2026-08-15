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

/// Shape member kind (Director ShapeType codes; port of LibreShockwave
/// shapeTypeFromCode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeType {
    Rect,
    OvalRect,
    Oval,
    Line,
    Unknown,
}

/// Parsed Director shape member (the CASt member's data block, LibreShockwave
/// ShapeInfo). The exporter renders this as `shapes/NNNN_shape_<name>.txt`
/// (key: value lines), which the runtime parses back to draw the entry/room
/// scenes' solid-fill rects/ovals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeInfo {
    pub shape_type: ShapeType,
    pub reg_x: u16,
    pub reg_y: u16,
    pub width: u16,
    pub height: u16,
    pub color: u8,
    pub back_color: u8,
    pub fill_type: u8,
    pub line_thickness: u8,
    pub line_direction: u8,
}

impl ShapeInfo {
    pub fn is_filled(&self) -> bool {
        self.fill_type != 0
    }

    pub fn is_outline_invisible(&self) -> bool {
        !self.is_filled() && self.line_thickness <= 1
    }

    /// Parse from the CASt member's DATA block (after the D5 header + info
    /// section). Layout (big-endian, LibreShockwave ShapeInfo::parse):
    ///   u16 shapeTypeRaw, u16 regY, u16 regX, u16 height, u16 width,
    ///   skip 2, u8 color, u8 backColor, u8 fillType, u8 lineThickness,
    ///   u8 lineDirection.
    pub fn parse(data: &[u8]) -> Option<ShapeInfo> {
        if data.len() < 17 {
            return None;
        }
        let shape_type = match u16::from_be_bytes([data[0], data[1]]) {
            0x01 => ShapeType::Rect,
            0x02 => ShapeType::OvalRect,
            0x03 => ShapeType::Oval,
            0x08 => ShapeType::Line,
            _ => ShapeType::Unknown,
        };
        Some(ShapeInfo {
            shape_type,
            reg_y: u16::from_be_bytes([data[2], data[3]]),
            reg_x: u16::from_be_bytes([data[4], data[5]]),
            height: u16::from_be_bytes([data[6], data[7]]),
            width: u16::from_be_bytes([data[8], data[9]]),
            color: data[12],
            back_color: data[13],
            fill_type: data[14],
            line_thickness: data[15],
            line_direction: data[16],
        })
    }

    /// The runtime's text form (`shapes/*.txt`): the exact key: value lines
    /// LibreShockwave's CastExporter writes (color/backColor as hex like the
    /// C++ `std::hex << std::showbase` — 0 prints as `0`, nonzero as `0x..`).
    pub fn to_text(&self) -> String {
        let color = if self.color == 0 { "0".to_string() } else { format!("0x{:x}", self.color) };
        let back = if self.back_color == 0 { "0".to_string() } else { format!("0x{:x}", self.back_color) };
        let shape_type = match self.shape_type {
            ShapeType::Rect => "rect",
            ShapeType::OvalRect => "ovalRect",
            ShapeType::Oval => "oval",
            ShapeType::Line => "line",
            ShapeType::Unknown => "unknown",
        };
        format!(
            "shapeType: {shape_type}\nregX: {}\nregY: {}\nwidth: {}\nheight: {}\ncolor: {color}\nbackColor: {back}\nfillType: {}\nlineThickness: {}\nlineDirection: {}\nfilled: {}\noutlineInvisible: {}\n",
            self.reg_x,
            self.reg_y,
            self.width,
            self.height,
            self.fill_type,
            self.line_thickness,
            self.line_direction,
            if self.is_filled() { "yes" } else { "no" },
            if self.is_outline_invisible() { "yes" } else { "no" },
        )
    }
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

/// A cast library entry from the MCsL (Cast List) chunk — the movie's full
/// cast library table. Cast 0 is the internal cast; every other entry names a
/// LINKED cast file (external .cst/.cct) or an empty internal slot (a
/// pre-allocated placeholder like the corpus's `empty N` casts). `path` is
/// the Director-time file path (e.g. `D:\\LINGO\\Builds\\...\\empty.cst`),
/// empty for internal casts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CastListEntry {
    pub name: String,
    pub path: String,
    pub min_member: u32,
    pub max_member: u32,
    pub member_count: u32,
    /// Cast library id as stored in the entry's member-range item (the value
    /// LibreShockwave prints as casts.txt's `id` column).
    pub id: i32,
}

/// Parse the MCsL (Cast List) chunk: the movie's cast library table.
///
/// Layout (Director 7+, big-endian, per LibreShockwave CastListChunk::read):
///
/// ```text
/// i32 dataOffset      — offset from chunk start to the offset table
/// u16 (unknown)
/// u16 itemCount       — number of cast library entries
/// u16 itemsPerEntry   — items per entry (4: name, path, preload, member-range)
/// u16 (unknown)
///   at dataOffset:
/// u16 offsetTableLen  — number of u32 offsets
/// i32 offsets[offsetTableLen]
/// i32 itemsLen        — byte length of the items blob
///   items blob: one Pascal string or small struct per item
/// ```
///
/// Each entry reads items at `base = i * itemsPerEntry`: item+1 = cast name,
/// item+2 = linked-cast file path (empty for internal casts), item+3 =
/// preload settings (ignored), item+4 = member-range struct (u16 minMember,
/// u16 maxMember, i32 id).
pub fn parse_cast_list(data: &[u8]) -> Vec<CastListEntry> {
    if data.len() < 12 {
        return Vec::new();
    }
    let rd = |pos: usize| -> usize { u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize };
    let data_offset = rd(0);
    let _ = rd(4); // unknown u16 (part of the 12-byte header: unk, itemCount, itemsPerEntry, unk)
    let item_count = u16::from_be_bytes([data[6], data[7]]) as usize;
    let items_per_entry = u16::from_be_bytes([data[8], data[9]]) as usize;
    let _ = u16::from_be_bytes([data[10], data[11]]);

    if data_offset >= data.len() || item_count > 1000 || items_per_entry == 0 {
        return Vec::new();
    }
    let table = &data[data_offset..];
    if table.len() < 2 {
        return Vec::new();
    }
    let offset_table_len = u16::from_be_bytes([table[0], table[1]]) as usize;
    if offset_table_len == 0 || offset_table_len > 10000 || table.len() < 2 + offset_table_len * 4 + 4 {
        return Vec::new();
    }
    let mut offsets = Vec::with_capacity(offset_table_len);
    for i in 0..offset_table_len {
        offsets.push(rd(data_offset + 2 + i * 4) as i32);
    }
    let items_len_pos = data_offset + 2 + offset_table_len * 4;
    let items_len = rd(items_len_pos) as i32;
    let list_offset = items_len_pos + 4;
    let mut items: Vec<Vec<u8>> = Vec::with_capacity(offsets.len());
    for (index, &offset) in offsets.iter().enumerate() {
        let next = if index + 1 < offsets.len() { offsets[index + 1] } else { items_len };
        let item_len = next - offset;
        let start = list_offset as i64 + offset as i64;
        if offset >= 0 && item_len > 0 && item_len < 10000 && start >= 0 && (start as usize) <= data.len() {
            let s = start as usize;
            let e = s + item_len as usize;
            if e <= data.len() {
                items.push(data[s..e].to_vec());
                continue;
            }
        }
        items.push(Vec::new());
    }
    let pascal = |item: &[u8]| -> String {
        if item.is_empty() {
            return String::new();
        }
        let len = item[0] as usize;
        if len == 0 || len > item.len() - 1 {
            return String::new();
        }
        // MacRoman-ish; printable ASCII covers the corpus cast names/paths.
        item[1..=len].iter().map(|&b| if b >= 32 && b < 127 { b as char } else { '?' }).collect()
    };
    let mut entries = Vec::with_capacity(item_count);
    for cast_index in 0..item_count {
        let base = cast_index * items_per_entry;
        let name = pascal(items.get(base + 1).map(Vec::as_slice).unwrap_or(&[]));
        let path = pascal(items.get(base + 2).map(Vec::as_slice).unwrap_or(&[]));
        let mut entry = CastListEntry { name, path, min_member: 0, max_member: 0, member_count: 0, id: (cast_index + 1) as i32 };
        if let Some(member_data) = items.get(base + 4) {
            if member_data.len() >= 8 {
                let min_member = u16::from_be_bytes([member_data[0], member_data[1]]) as u32;
                let max_member = u16::from_be_bytes([member_data[2], member_data[3]]) as u32;
                let id = i32::from_be_bytes([member_data[4], member_data[5], member_data[6], member_data[7]]);
                entry.min_member = min_member;
                entry.max_member = max_member;
                entry.id = id;
                if max_member >= min_member {
                    entry.member_count = max_member - min_member + 1;
                }
            }
        }
        entries.push(entry);
    }
    entries
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

    /// Build an MCsL blob the way Director lays it out (see parse_cast_list
    /// docs) and assert parse_cast_list round-trips it. Layout mirrors the
    /// real habbo.dcr: item 0 is a global placeholder, then each cast library
    /// entry spans 4 items — name, path, preload, member-range — so the
    /// parser's `base + 1..=base + 4` indexing lands on the right ones.
    fn build_mcsl(entries: &[(u16, u16, i32, &str, &str)]) -> Vec<u8> {
        let items_per_entry = 4usize;
        let mut items: Vec<Vec<u8>> = vec![vec![0]]; // leading global item
        for (min, max, id, name, path) in entries {
            let mut name_item = vec![name.len() as u8];
            name_item.extend_from_slice(name.as_bytes());
            let mut path_item = vec![path.len() as u8];
            path_item.extend_from_slice(path.as_bytes());
            let mut range = Vec::new();
            range.extend_from_slice(&min.to_be_bytes());
            range.extend_from_slice(&max.to_be_bytes());
            range.extend_from_slice(&id.to_be_bytes());
            items.push(name_item);
            items.push(path_item);
            items.push(vec![0, 0]); // preload settings
            items.push(range);
        }
        // Offset table + items blob, packed back to back.
        let mut blob = Vec::new();
        let mut offsets: Vec<u32> = Vec::new();
        for item in &items {
            offsets.push(blob.len() as u32);
            blob.extend_from_slice(item);
        }
        let mut data = Vec::new();
        // Header: dataOffset=12, unk=0, itemCount, itemsPerEntry, unk=0.
        data.extend_from_slice(&12u32.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        data.extend_from_slice(&(items_per_entry as u16).to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&(offsets.len() as u16).to_be_bytes());
        for o in &offsets {
            data.extend_from_slice(&o.to_be_bytes());
        }
        data.extend_from_slice(&(blob.len() as u32).to_be_bytes());
        data.extend_from_slice(&blob);
        data
    }

    #[test]
    fn parse_shape_info_and_text() {
        // hh_entry_au member 247 (skyleft): rect 720x359, color 78 (0x4e).
        let mut data = Vec::new();
        data.extend_from_slice(&0x01u16.to_be_bytes()); // shapeType rect
        data.extend_from_slice(&0u16.to_be_bytes()); // regY
        data.extend_from_slice(&0u16.to_be_bytes()); // regX
        data.extend_from_slice(&359u16.to_be_bytes()); // height
        data.extend_from_slice(&720u16.to_be_bytes()); // width
        data.extend_from_slice(&[0, 0]); // skip 2
        data.extend_from_slice(&[78, 0, 1, 1, 5]); // color, backColor, fillType, lineThickness, lineDirection
        let s = ShapeInfo::parse(&data).expect("parse");
        assert_eq!(s.shape_type, ShapeType::Rect);
        assert_eq!(s.width, 720);
        assert_eq!(s.height, 359);
        assert_eq!(s.color, 78);
        assert_eq!(s.back_color, 0);
        assert!(s.is_filled());
        let text = s.to_text();
        assert!(text.contains("shapeType: rect\n"));
        assert!(text.contains("width: 720\n"));
        assert!(text.contains("color: 0x4e\n"));
        assert!(text.contains("backColor: 0\n"));
        assert!(text.contains("filled: yes\n"));
        assert!(text.contains("outlineInvisible: no\n"));
        // A thin unfilled line: outlineInvisible yes, color 0 prints as 0.
        let mut line = Vec::new();
        line.extend_from_slice(&0x08u16.to_be_bytes()); // line
        line.extend_from_slice(&[0; 10]);
        line.extend_from_slice(&[0, 0, 0, 1, 5]);
        let l = ShapeInfo::parse(&line).expect("parse line");
        assert_eq!(l.shape_type, ShapeType::Line);
        assert!(!l.is_filled());
        assert!(l.is_outline_invisible());
        assert!(l.to_text().contains("color: 0\n"));
        assert!(l.to_text().contains("filled: no\n"));
        assert!(l.to_text().contains("outlineInvisible: yes\n"));
    }

    #[test]
    fn parse_cast_list_round_trip() {
        let blob = build_mcsl(&[
            (1, 4, 66560, "Internal", ""),
            (1, 82, 1024, "fuse_client", r"D:\LINGO\Builds\release14.1_bx\fuse_client.cst"),
            (1, 0, 132096, "bin", ""),
            (1, 0, 1024, "empty 1", r"D:\LINGO\Builds\release14.1_bx\empty.cst"),
        ]);
        let entries = parse_cast_list(&blob);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].name, "Internal");
        assert_eq!(entries[0].path, "");
        assert_eq!(entries[0].min_member, 1);
        assert_eq!(entries[0].max_member, 4);
        assert_eq!(entries[0].member_count, 4);
        assert_eq!(entries[0].id, 66560);
        assert_eq!(entries[1].name, "fuse_client");
        assert_eq!(entries[1].path, r"D:\LINGO\Builds\release14.1_bx\fuse_client.cst");
        assert_eq!(entries[1].member_count, 82);
        // Empty cast: member_count 0 (min 1, max 0 → no range).
        assert_eq!(entries[2].member_count, 0);
        assert_eq!(entries[3].name, "empty 1");
        assert_eq!(entries[3].member_count, 0);
    }
}
