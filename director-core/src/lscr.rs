//! Director 7 Lingo chunk parsers: Lscr (bytecode), LctX (script context),
//! Lnam (script names).
//!
//! Format reference: LibreShockwave cpp/src/chunks/{ScriptChunk,ScriptContextChunk,
//! ScriptNamesChunk}.cpp and ProjectorRays lingodec (ScummVM lingodec/script.cpp).
//! Lingo data is always big-endian.
//!
//! Key facts (verified against the Habbo CCTs):
//! - A script cast member's info block stores a `scriptId` (see
//!   cast::read_member_script_id). `scriptId - 1` indexes into the LctX
//!   ScriptContext's entries; that entry's `sectionId` is the LSCR resource id
//!   holding the bytecode. (fuse_client: member 12 Object API -> scriptId 4 ->
//!   entry[3] -> Lscr 142.)
//! - The LctX also names the Lnam resource (`lnamSectionId`) whose names table
//!   maps handler `nameId`s to identifiers.
//! - The Lscr chunk data begins with an 8-byte prefix (the container header
//!   copy) before the script header fields; handler/bytecode/literal offsets
//!   are relative to the start of the chunk data.

use director_rifx::Chunk;
use crate::{lingo::Opcode, ParseError};

// ---------------------------------------------------------------------------
// Script type (LibreShockwave ScriptChunkType)
// ---------------------------------------------------------------------------

/// The kind of script an Lscr chunk holds (from `behaviorFlags & 0x0F`, or the
/// script number when that is 0). Used for the `-- <type>` header line of
/// decompiled output and for resolving member-level script types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptType {
    Score = 1,
    Behavior = 2,
    MovieScript = 3,
    Parent = 7,
    Unknown = -1,
}

impl ScriptType {
    pub fn from_code(code: i32) -> ScriptType {
        match code {
            1 => ScriptType::Score,
            2 => ScriptType::Behavior,
            3 => ScriptType::MovieScript,
            7 => ScriptType::Parent,
            _ => ScriptType::Unknown,
        }
    }

    /// LibreShockwave format::getScriptTypeName (used by decompile()).
    pub fn name(self) -> &'static str {
        match self {
            ScriptType::MovieScript => "Movie Script",
            ScriptType::Behavior => "Behavior",
            ScriptType::Parent => "Parent Script",
            ScriptType::Score => "Score Script",
            ScriptType::Unknown => "Script",
        }
    }

    /// CastExporter scriptTypeDisplayName (used by the .ls file header).
    pub fn display_name(self) -> &'static str {
        match self {
            ScriptType::Score => "Score",
            ScriptType::Behavior => "Behavior",
            ScriptType::MovieScript => "Movie Script",
            ScriptType::Parent => "Parent",
            ScriptType::Unknown => "Unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// ScriptContext (LctX)
// ---------------------------------------------------------------------------

/// One ScriptContext entry: maps a script index to an LSCR resource id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptContextEntry {
    /// The LSCR resource id containing this script's bytecode. <= 0 = empty slot.
    pub section_id: i32,
    /// Entry flags.
    pub flags: u16,
}

/// A parsed LctX (Script Context) chunk.
#[derive(Debug, Clone)]
pub struct ScriptContext {
    pub entry_count: i32,
    pub lnam_section_id: i32,
    pub entries: Vec<ScriptContextEntry>,
    pub raw_data: Vec<u8>,
}

impl ScriptContext {
    /// Resolve a scriptId (1-based) to its LSCR resource id, if present.
    pub fn script_resource(&self, script_id: i32) -> Option<u32> {
        if script_id <= 0 {
            return None;
        }
        let idx = (script_id - 1) as usize;
        self.entries
            .get(idx)
            .filter(|e| e.section_id > 0)
            .map(|e| e.section_id as u32)
    }
}

/// Parse an LctX chunk. The data layout (big-endian):
///   i32 unk1, i32 unk2, i32 entryCount, i32 entryCount2,
///   u16 entriesOffset, u16, i32, i32, i32,
///   i32 lnamSectionId, u16 validCount, u16 flags, i16 freePtr,
///   then entryCount × 12-byte entries: i32 unk, i32 sectionId, u16 flags, u16.
pub fn read_script_context(chunk: &Chunk) -> Result<ScriptContext, ParseError> {
    let data = chunk.data();
    if data.len() < 42 {
        return Err(ParseError::InvalidData("LctX chunk too small".into()));
    }
    let be = |o: usize| o < data.len();
    let read_i32 = |o: usize| {
        i32::from_be_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
    };
    let read_u16 = |o: usize| u16::from_be_bytes([data[o], data[o + 1]]);

    let entry_count = read_i32(8);
    let entries_offset = read_u16(16) as usize;
    let lnam_section_id = read_i32(32);

    let mut entries = Vec::new();
    if entry_count > 0 && entry_count < 1_000_000 && be(entries_offset) {
        for i in 0..entry_count as usize {
            let o = entries_offset + i * 12;
            if o + 12 > data.len() {
                break;
            }
            entries.push(ScriptContextEntry {
                section_id: read_i32(o + 4),
                flags: read_u16(o + 8),
            });
        }
    }

    Ok(ScriptContext {
        entry_count,
        lnam_section_id,
        entries,
        raw_data: data.to_vec(),
    })
}

// ---------------------------------------------------------------------------
// ScriptNames (Lnam)
// ---------------------------------------------------------------------------

/// A parsed Lnam (Script Names) chunk: the name table used by scripts.
#[derive(Debug, Clone)]
pub struct ScriptNames {
    pub names: Vec<String>,
    pub raw_data: Vec<u8>,
}

impl ScriptNames {
    pub fn name(&self, id: i32) -> Option<&str> {
        if id < 0 {
            return None;
        }
        self.names.get(id as usize).map(|s| s.as_str())
    }
}

/// Parse an Lnam chunk. Layout (big-endian):
///   i32 unk1, i32 unk2, u32 len1, u32 len2, u16 namesOffset, u16 namesCount,
///   then at namesOffset: namesCount Pascal strings (u8 length + bytes).
pub fn read_script_names(chunk: &Chunk) -> Result<ScriptNames, ParseError> {
    let data = chunk.data();
    if data.len() < 20 {
        return Err(ParseError::InvalidData("Lnam chunk too small".into()));
    }
    let read_u16 = |o: usize| u16::from_be_bytes([data[o], data[o + 1]]);

    let names_offset = read_u16(16) as usize;
    let names_count = read_u16(18) as usize;
    if names_count > 100_000 {
        return Err(ParseError::InvalidData("Lnam names count implausible".into()));
    }

    let mut names = Vec::with_capacity(names_count);
    if names_count > 0 && names_offset < data.len() {
        let mut p = names_offset;
        for _ in 0..names_count {
            if p >= data.len() {
                break;
            }
            let len = data[p] as usize;
            p += 1;
            if p + len > data.len() {
                break;
            }
            let s = String::from_utf8_lossy(&data[p..p + len]).to_string();
            names.push(s);
            p += len;
        }
    }

    Ok(ScriptNames {
        names,
        raw_data: data.to_vec(),
    })
}

// ---------------------------------------------------------------------------
// Script (Lscr)
// ---------------------------------------------------------------------------

/// A single decoded bytecode instruction (LibreShockwave
/// ScriptChunk::Instruction). `offset` is relative to the handler's bytecode
/// start; `opcode` is the semantic opcode; `raw_opcode` is the raw byte (for
/// argument-width and mnemonic decisions); `argument` follows the signedness
/// rules of ScriptChunk::read.
#[derive(Debug, Clone, Copy)]
pub struct Instruction {
    pub offset: i32,
    pub opcode: Opcode,
    pub raw_opcode: u8,
    pub argument: i32,
}

/// A parsed script handler (function) inside an LSCR chunk.
#[derive(Debug, Clone)]
pub struct ScriptHandler {
    /// Index into the Lnam name table (the handler's identifier).
    pub name_id: i16,
    /// Bytecode length in bytes.
    pub bytecode_len: u32,
    /// Bytecode offset relative to the start of the LSCR chunk data.
    pub bytecode_offset: u32,
    pub arg_count: u16,
    pub arg_offset: u32,
    pub local_count: u16,
    pub local_offset: u32,
    pub globals_count: u16,
    pub globals_offset: u32,
    pub line_count: u16,
    /// Name-id tables (into the Lnam table) for this handler's arguments,
    /// locals and globals (lingodec `Handler::readData`).
    pub arg_name_ids: Vec<i16>,
    pub local_name_ids: Vec<i16>,
    pub global_name_ids: Vec<i16>,
    /// Decoded instructions (LibreShockwave ScriptChunk::read loop).
    pub instructions: Vec<Instruction>,
}

/// A literal (constant) referenced by script bytecode, with its parsed value.
#[derive(Debug, Clone)]
pub struct Literal {
    pub kind: i32,
    pub offset: u32,
    /// Type-4 (int) literal value (the offset field itself).
    pub int_value: Option<i32>,
    /// Type-9 (float) literal value, decoded from the literal data.
    pub float_value: Option<f64>,
    /// Type-1 (string) literal, decoded from MacRoman.
    pub string_value: Option<String>,
    /// Unknown-type literal raw bytes.
    pub bytes: Option<Vec<u8>>,
}

/// A parsed Lscr (Lingo Script) chunk.
#[derive(Debug, Clone)]
pub struct Script {
    pub total_length: u32,
    pub total_length2: u32,
    pub header_length: u16,
    pub script_number: u16,
    pub parent_number: i16,
    pub script_flags: u32,
    /// The script kind (behaviorFlags & 0x0F, or scriptNumber if that is 0).
    pub script_type: ScriptType,
    pub property_name_ids: Vec<i16>,
    pub global_name_ids: Vec<i16>,
    pub handlers: Vec<ScriptHandler>,
    pub literals: Vec<Literal>,
    pub raw_data: Vec<u8>,
}

impl Script {
    /// True when this script defines a factory (parent) class.
    pub fn is_factory(&self) -> bool {
        self.script_flags & 0x01 != 0
    }
}

/// Parse an Lscr chunk.
///
/// The chunk data begins with an 8-byte prefix (a copy of the container
/// header) — LibreShockwave's ScriptChunk::read seeks 8 before reading, so
/// the header fields are at chunk-data offset +8, while handler/bytecode/
/// literal offsets are relative to offset 0. `capital_x` selects the 46-byte
/// (vs 42-byte) handler record used when the file has an LctX chunk.
pub fn read_script(chunk: &Chunk, capital_x: bool) -> Result<Script, ParseError> {
    let data = chunk.data();
    if data.len() < 92 {
        return Err(ParseError::InvalidData("Lscr chunk too small".into()));
    }
    let read_i32 = |o: usize| {
        i32::from_be_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
    };
    let read_u32 = |o: usize| {
        u32::from_be_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
    };
    let read_u16 = |o: usize| u16::from_be_bytes([data[o], data[o + 1]]);

    let total_length = read_u32(8);
    let total_length2 = read_u32(12);
    let header_length = read_u16(16);
    let script_number = read_u16(18);
    let parent_number = i16::from_be_bytes([data[22], data[23]]);
    let script_flags = read_u32(38);

    // Script kind: behaviorFlags & 0x0F, falling back to the script number
    // (LibreShockwave ScriptChunk::read).
    let script_type = {
        let mut code = (script_flags & 0x0F) as i32;
        if code == 0 {
            code = script_number as i32;
        }
        ScriptType::from_code(code)
    };

    let properties_count = read_u16(60) as usize;
    let properties_offset = read_u32(62) as usize;
    let globals_count = read_u16(66) as usize;
    let globals_offset = read_u32(68) as usize;
    let handlers_count = read_u16(72) as usize;
    let handlers_offset = read_u32(74) as usize;
    let literals_count = read_u16(78) as usize;
    let literals_offset = read_u32(80) as usize;
    let _literal_data_count = read_u32(84);
    let literal_data_offset = read_u32(88) as usize;

    let read_ids = |count: usize, offset: usize| -> Vec<i16> {
        let mut ids = Vec::with_capacity(count);
        for i in 0..count {
            let o = offset + i * 2;
            if o + 2 > data.len() {
                break;
            }
            ids.push(i16::from_be_bytes([data[o], data[o + 1]]));
        }
        ids
    };

    let property_name_ids = read_ids(properties_count, properties_offset);
    let global_name_ids = read_ids(globals_count, globals_offset);

    let handler_record_len = if capital_x { 46usize } else { 42usize };
    let mut handlers = Vec::new();
    if handlers_count > 0 && handlers_count < 10_000 && handlers_offset < data.len() {
        for i in 0..handlers_count {
            let o = handlers_offset + i * handler_record_len;
            if o + handler_record_len > data.len() {
                break;
            }
            handlers.push(ScriptHandler {
                name_id: i16::from_be_bytes([data[o], data[o + 1]]),
                bytecode_len: read_u32(o + 4),
                bytecode_offset: read_u32(o + 8),
                arg_count: read_u16(o + 12),
                arg_offset: read_u32(o + 14),
                local_count: read_u16(o + 18),
                local_offset: read_u32(o + 20),
                globals_count: read_u16(o + 24),
                globals_offset: read_u32(o + 26),
                line_count: read_u16(o + 36),
                arg_name_ids: Vec::new(),
                local_name_ids: Vec::new(),
                global_name_ids: Vec::new(),
                instructions: Vec::new(),
            });
        }
    }

    // Per-handler name tables (name ids into the Lnam table). Offsets are
    // relative to the start of the chunk data.
    for h in &mut handlers {
        h.arg_name_ids = read_ids(h.arg_count as usize, h.arg_offset as usize);
        h.local_name_ids = read_ids(h.local_count as usize, h.local_offset as usize);
        h.global_name_ids = read_ids(h.globals_count as usize, h.globals_offset as usize);
    }

    // Decode each handler's bytecode into instructions (LibreShockwave
    // ScriptChunk::read loop): offsets relative to the bytecode start, argument
    // width by raw opcode range (1/2/4 bytes), signedness per opcode.
    let is_push8 = |op: Opcode| op == Opcode::PushInt8;
    let is_push16 = |op: Opcode| op == Opcode::PushInt16;
    for h in &mut handlers {
        let start = h.bytecode_offset as usize;
        let end = (start + h.bytecode_len as usize).min(data.len());
        let mut instrs = Vec::new();
        if start < data.len() {
            let mut pc = start;
            while pc < end {
                let instr_offset = (pc - start) as i32;
                let raw = data[pc];
                pc += 1;
                let op = Opcode::from_raw(raw);
                let mut argument: i32 = 0;
                if raw >= 0xC0 {
                    if pc + 4 > end {
                        break;
                    }
                    argument = i32::from_be_bytes([data[pc], data[pc + 1], data[pc + 2], data[pc + 3]]);
                    pc += 4;
                } else if raw >= 0x80 {
                    if pc + 2 > end {
                        break;
                    }
                    argument = if is_push16(op) || is_push8(op) {
                        i16::from_be_bytes([data[pc], data[pc + 1]]) as i32
                    } else {
                        u16::from_be_bytes([data[pc], data[pc + 1]]) as i32
                    };
                    pc += 2;
                } else if raw >= 0x40 {
                    if pc + 1 > end {
                        break;
                    }
                    argument = if is_push8(op) {
                        data[pc] as i8 as i32
                    } else {
                        data[pc] as i32
                    };
                    pc += 1;
                }
                instrs.push(Instruction {
                    offset: instr_offset,
                    opcode: op,
                    raw_opcode: raw,
                    argument,
                });
            }
        }
        h.instructions = instrs;
    }

    // Literal records: (kind i32, offset i32), 8 bytes each (D5+; D4 used
    // 6-byte records — all our target files are D7). Values are read from the
    // literal data region, mirroring LibreShockwave ScriptChunk::read:
    //   i32 dataLen, then type 1 = MacRoman string (trailing NUL stripped),
    //   type 4 = the record offset itself, type 9 = float (4/8/10-byte),
    //   anything else = raw bytes.
    let mut literals = Vec::new();
    if literals_count > 0 && literals_count < 100_000 && literals_offset < data.len() {
        for i in 0..literals_count {
            let o = literals_offset + i * 8;
            if o + 8 > data.len() {
                break;
            }
            let kind = read_i32(o);
            let offset = read_u32(o + 4) as usize;
            let mut int_value = None;
            let mut float_value = None;
            let mut string_value = None;
            let mut bytes = None;
            if kind == 4 {
                int_value = Some(offset as i32);
            } else if literal_data_offset < data.len() {
                let at = literal_data_offset + offset;
                if at + 4 <= data.len() {
                    let data_len = read_i32(at) as i64;
                    let body_at = at + 4;
                    if (0..=0x100000).contains(&data_len) && body_at + data_len as usize <= data.len() {
                        let body = &data[body_at..body_at + data_len as usize];
                        match kind {
                            1 => {
                                let mut s = decode_mac_roman(body);
                                if s.ends_with('\0') {
                                    s.pop();
                                }
                                string_value = Some(s);
                            }
                            9 => {
                                float_value = Some(match body.len() {
                                    4 => f32::from_be_bytes([body[0], body[1], body[2], body[3]]) as f64,
                                    8 => f64::from_be_bytes([
                                        body[0], body[1], body[2], body[3], body[4], body[5], body[6], body[7],
                                    ]),
                                    10 => apple_float80(body),
                                    _ => 0.0,
                                });
                            }
                            _ => bytes = Some(body.to_vec()),
                        }
                    }
                }
            }
            literals.push(Literal {
                kind,
                offset: offset as u32,
                int_value,
                float_value,
                string_value,
                bytes,
            });
        }
    }

    Ok(Script {
        total_length,
        total_length2,
        header_length,
        script_number,
        parent_number,
        script_flags,
        script_type,
        property_name_ids,
        global_name_ids,
        handlers,
        literals,
        raw_data: data.to_vec(),
    })
}

/// Decode MacRoman bytes to a UTF-8 String (LibreShockwave decodeMacRoman).
/// Bytes < 0x80 pass through; the high half uses the Apple MacRoman table.
pub fn decode_mac_roman(bytes: &[u8]) -> String {
    const HIGH: &[char] = &[
        '\u{00C4}', '\u{00C5}', '\u{00C7}', '\u{00C9}', '\u{00D1}', '\u{00D6}', '\u{00DC}', '\u{00E1}',
        '\u{00E0}', '\u{00E2}', '\u{00E4}', '\u{00E3}', '\u{00E5}', '\u{00E7}', '\u{00E9}', '\u{00E8}',
        '\u{00EA}', '\u{00EB}', '\u{00ED}', '\u{00EC}', '\u{00EE}', '\u{00EF}', '\u{00F1}', '\u{00F3}',
        '\u{00F2}', '\u{00F4}', '\u{00F6}', '\u{00F5}', '\u{00FA}', '\u{00F9}', '\u{00FB}', '\u{00FC}',
        '\u{2020}', '\u{00B0}', '\u{00A2}', '\u{00A3}', '\u{00A7}', '\u{2022}', '\u{00B6}', '\u{00DF}',
        '\u{00AE}', '\u{00A9}', '\u{2122}', '\u{00B4}', '\u{00A8}', '\u{2260}', '\u{00C6}', '\u{00D8}',
        '\u{221E}', '\u{00B1}', '\u{2264}', '\u{2265}', '\u{00A5}', '\u{00B5}', '\u{2202}', '\u{2211}',
        '\u{220F}', '\u{03C0}', '\u{222B}', '\u{00AA}', '\u{00BA}', '\u{03A9}', '\u{00E6}', '\u{00F8}',
        '\u{00BF}', '\u{00A1}', '\u{00AC}', '\u{221A}', '\u{0192}', '\u{2248}', '\u{2206}', '\u{00AB}',
        '\u{00BB}', '\u{2026}', '\u{00A0}', '\u{00C0}', '\u{00C3}', '\u{00D5}', '\u{0152}', '\u{0153}',
        '\u{2013}', '\u{2014}', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}', '\u{00F7}', '\u{25CA}',
        '\u{00FF}', '\u{0178}', '\u{2044}', '\u{20AC}', '\u{2039}', '\u{203A}', '\u{FB01}', '\u{FB02}',
        '\u{2021}', '\u{00B7}', '\u{201A}', '\u{201E}', '\u{2030}', '\u{00C2}', '\u{00CA}', '\u{00C1}',
        '\u{00CB}', '\u{00C8}', '\u{00CD}', '\u{00CE}', '\u{00CF}', '\u{00CC}', '\u{00D3}', '\u{00D4}',
        '\u{F8FF}', '\u{00D2}', '\u{00DA}', '\u{00DB}', '\u{00D9}', '\u{0131}', '\u{02C6}', '\u{02DC}',
        '\u{00AF}', '\u{02D8}', '\u{02D9}', '\u{02DA}', '\u{00B8}', '\u{02DD}', '\u{02DB}', '\u{02C7}',
    ];
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b < 0x80 {
            out.push(b as char);
        } else {
            out.push(HIGH[(b - 0x80) as usize]);
        }
    }
    out
}

/// Convert an Apple 80-bit extended-precision float (SANE Extended, big-endian)
/// to f64 (LibreShockwave appleFloat80ToDouble).
fn apple_float80(data: &[u8]) -> f64 {
    if data.len() < 10 {
        return 0.0;
    }
    let exponent_word = u16::from_be_bytes([data[0], data[1]]);
    let sign = ((exponent_word & 0x8000) as u64) << 48;
    let exponent = (exponent_word & 0x7fff) as i64;
    let mut fraction: u64 = 0;
    for i in 0..8 {
        fraction = (fraction << 8) | data[2 + i] as u64;
    }
    fraction &= 0x7fff_ffff_ffff_ffff;

    let f64_exp = if exponent == 0 {
        0u64
    } else if exponent == 0x7fff {
        0x7ffu64
    } else {
        let norm = exponent - 0x3fff;
        if norm < -0x3fe || norm >= 0x3ff {
            return 0.0;
        }
        (norm + 0x3ff) as u64
    };
    let bits = sign | (f64_exp << 52) | (fraction >> 11);
    f64::from_bits(bits)
}

/// Find the first LctX/Lctx chunk in a file (used to detect "capital X" files).
pub fn has_capital_x(root: &Chunk) -> bool {
    !root.children_by(b"LctX").is_empty() || !root.children_by(b"Lctx").is_empty()
}

/// Look up a chunk by its resource id (`source_id`), preferring a given fourcc.
pub fn chunk_by_resource<'a>(root: &'a Chunk, fourcc: &[u8; 4], res_id: u32) -> Option<&'a Chunk> {
    root.children
        .iter()
        .find(|c| c.is(fourcc) && c.source_id == Some(res_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_context_parse() {
        // Minimal LctX: unk1, unk2, entryCount=2, entryCount2=2, entriesOffset=42,
        // then two entries: (unk=0, sectionId=770, flags=0), (0, 596, 4).
        let mut d = vec![0u8; 42 + 24];
        d[8..12].copy_from_slice(&2i32.to_be_bytes());
        d[12..16].copy_from_slice(&2i32.to_be_bytes());
        d[16..18].copy_from_slice(&42u16.to_be_bytes());
        // entries at 42: entry 0
        d[42 + 4..42 + 8].copy_from_slice(&770i32.to_be_bytes());
        // entry 1
        d[42 + 12 + 4..42 + 12 + 8].copy_from_slice(&596i32.to_be_bytes());
        d[42 + 12 + 8..42 + 12 + 10].copy_from_slice(&4u16.to_be_bytes());

        let chunk = Chunk::new(director_rifx::FourCC(*b"LctX"));
        // Need to inject raw data; Chunk::new leaves data empty, so build via
        // a serialized RIFX frame instead.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"LctX");
        raw.extend_from_slice(&(d.len() as u32).to_be_bytes());
        raw.extend_from_slice(&d);
        let (chunk, _) = director_rifx::chunk::read_chunk(&raw, &mut 0u64).unwrap();
        let ctx = read_script_context(&chunk).unwrap();
        assert_eq!(ctx.entry_count, 2);
        assert_eq!(ctx.entries.len(), 2);
        assert_eq!(ctx.script_resource(1), Some(770));
        assert_eq!(ctx.script_resource(2), Some(596));
        assert_eq!(ctx.script_resource(0), None);
        assert_eq!(ctx.script_resource(3), None);
    }

    #[test]
    fn script_names_parse() {
        // Header: unk1, unk2, len1, len2 (0..16); namesOffset=20, namesCount=2;
        // then Pascal strings "hi", "there" at offset 20.
        let mut d = vec![0u8; 20 + 2 + 2 + 5];
        d[16..18].copy_from_slice(&20u16.to_be_bytes());
        d[18..20].copy_from_slice(&2u16.to_be_bytes());
        d[20] = 2;
        d[21..23].copy_from_slice(b"hi");
        d[23] = 5;
        d[24..29].copy_from_slice(b"there");

        let mut raw = Vec::new();
        raw.extend_from_slice(b"Lnam");
        raw.extend_from_slice(&(d.len() as u32).to_be_bytes());
        raw.extend_from_slice(&d);
        let (chunk, _) = director_rifx::chunk::read_chunk(&raw, &mut 0u64).unwrap();
        let names = read_script_names(&chunk).unwrap();
        assert_eq!(names.names, vec!["hi", "there"]);
        assert_eq!(names.name(0), Some("hi"));
        assert_eq!(names.name(1), Some("there"));
        assert_eq!(names.name(2), None);
    }

    #[test]
    fn script_parse_capital_x() {
        // Build a fake Lscr with 8-byte prefix, header, and one handler whose
        // bytecode is a single procret (0x01).
        let mut d = vec![0u8; 8 + 92];
        d[8..12].copy_from_slice(&100u32.to_be_bytes()); // totalLength
        d[16..18].copy_from_slice(&92u16.to_be_bytes()); // headerLength
        d[18..20].copy_from_slice(&5u16.to_be_bytes()); // scriptNumber
        d[22..24].copy_from_slice(&(-1i16).to_be_bytes()); // parentNumber
        // handlers: 1 record at offset 100, 46 bytes (capitalX)
        let handlers_offset = 100u32;
        d.resize(handlers_offset as usize + 46 + 4, 0);
        d[72..74].copy_from_slice(&1u16.to_be_bytes());
        d[74..78].copy_from_slice(&handlers_offset.to_be_bytes());
        let ho = handlers_offset as usize;
        d[ho..ho + 2].copy_from_slice(&7i16.to_be_bytes()); // nameId
        d[ho + 4..ho + 8].copy_from_slice(&4u32.to_be_bytes()); // bcLen
        d[ho + 8..ho + 12].copy_from_slice(&((ho as u32) + 46).to_be_bytes()); // bcOffset
        d[ho + 46] = 0x01; // procret

        let mut raw = Vec::new();
        raw.extend_from_slice(b"Lscr");
        raw.extend_from_slice(&(d.len() as u32).to_be_bytes());
        raw.extend_from_slice(&d);
        let (chunk, _) = director_rifx::chunk::read_chunk(&raw, &mut 0u64).unwrap();
        let script = read_script(&chunk, true).unwrap();
        assert_eq!(script.script_number, 5);
        assert_eq!(script.handlers.len(), 1);
        assert_eq!(script.handlers[0].name_id, 7);
        assert_eq!(script.handlers[0].bytecode_len, 4);
        assert_eq!(script.handlers[0].bytecode_offset as usize, ho + 46);
        assert_eq!(script.raw_data[ho + 46], 0x01);
    }
}
