//! Afterburner decompression for Shockwave DCR/CCT files.
//!
//! Afterburner format (following ProjectorRays' dirfile.cpp, which is the
//! authoritative reference for Habbo CCTs):
//!   XFIR(12) + Fver + Fcdr(zlib) + ABMP(zlib) + FGEI(zlib) + body resources
//!
//! Header structure (all tags stored byte-reversed as ASCII, e.g. "revF"):
//!   u32 magic "XFIR" (LE file)
//!   u32 file length (LE)
//!   u32 codec        (LE; FGDM/FGDC = Afterburner, MV93/MC95/APPL = memory map)
//!
//! All Afterburner varints are MSB-first: `v = (v << 7) | (byte & 0x7F)`.
//!
//! ABMP (decompressed): u1, u2, resCount, then per resource:
//!   varint resId, varint offset(i32), varint compSize, varint uncompSize,
//!   varint compressionType, u32 fourCC (LE, byte-reversed "tSAC" for CASt).
//! Resources whose data lives in the ILS have offset == -1 (0xFFFFFFFF).
//!
//! FGEI (decompressed = the ILS buffer) is a sequence of:
//!   varint resId + compSize raw bytes of chunk data  (not compressed).
//! The remaining resources are stored zlib-compressed in the file body at
//! `offset + ilsBodyOffset`, where ilsBodyOffset is the file position where
//! the ILS zlib stream begins.

use flate2::read::ZlibDecoder;
use std::collections::HashMap;
use std::io::Read;

use crate::{Endian, Error, Result};

/// Maximum RIFX chunk data size we'll extract (safety limit).
const MAX_CHUNK_SIZE: usize = 50_000_000;

/// The Macromedia "ziplib" compression GUID listed in Fcdr.
const ZLIB_COMPRESSION_GUID: [u8; 16] = [
    0x99, 0xac, 0x70, 0x00, 0x36, 0x0b, 0x00, 0x00, 0x08, 0x00, 0x07, 0x37, 0x7a, 0x34, 0x4d, 0x61,
];

/// The container wrapper a compressed file arrives in. Determines the magic
/// and size-field endianness a recompression must use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileContainer {
    /// XFIR wrapper (Afterburner-compressed .CCT/.DCR files). Size is LE.
    Xfire,
    /// Plain RIFX wrapper with an Afterburner codec inside (no XFIR). Size is BE.
    Rifx,
}

/// Parsed Afterburner file with reconstructed RIFX data.
#[derive(Debug, Clone)]
pub struct AfterburnerFile {
    pub version: u32,
    pub version_string: String,
    pub resources: Vec<AbmpEntry>,
    /// Fully reconstructed RIFX bytes (ready for chunk parsing).
    pub rifx_data: Vec<u8>,
    /// Source resource IDs of each top-level chunk in `rifx_data`, in order.
    pub chunk_source_ids: Vec<u32>,
    /// Endianness of the chunk content data.
    /// Afterburner files produce big-endian RIFX (Mac format).
    /// Memory-map files produce little-endian chunks (Windows format).
    pub endian: Endian,
    /// Which container wrapper the source file used.
    pub container: FileContainer,
    // --- Original container constants, captured so a recompression can be
    // byte-faithful (the real client may be picky about these). ---
    /// The 4 codec bytes after the container header ("CDGF" or "MDGF").
    pub codec: [u8; 4],
    pub fver_imap_v: u32,
    pub fver_dir_v: u32,
    pub abmp_u1: u32,
    pub abmp_u2: u32,
    pub fgei_unk: u32,
    /// Compression GUIDs from the Fcdr section.
    pub fcdr_guids: Vec<[u8; 16]>,
}

#[derive(Debug, Clone)]
pub struct AbmpEntry {
    pub resource_id: u32,
    /// Signed file-body offset (relative to the ILS body start).
    /// 0 when the resource lives in the ILS instead (offset was -1).
    pub ils_offset: u64,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub compression_type: u32,
    /// Canonical FourCC (e.g. "CASt").
    pub fourcc: [u8; 4],
}

/// XFIR stores all FourCC values as little-endian u32s.
/// These constants are the values as read by `read_be_u32`, which means they are
/// the byte-reversed MKTAG values as they appear in the file.
const TAG_FVER: u32 = 0x72657646; // "revF" (LE of MKTAG('F','v','e','r') = 0x46766572)
const TAG_FCDR: u32 = 0x72646346; // "rdcF" (LE of MKTAG('F','c','d','r') = 0x46736463)
const TAG_ABMP: u32 = 0x504D4241; // "PMBA" (LE of MKTAG('A','B','M','P') = 0x504D4241 → same!)
const TAG_FGEI: u32 = 0x49454746; // "IEGF" (LE of MKTAG('F','G','E','I') = 0x49454746 → same!)
const TAG_FGDM: u32 = 0x4D444746; // "MDGF" (LE of MKTAG('F','G','D','M') = 0x4647444D)
const TAG_FGDC: u32 = 0x43444746; // "CDGF" (LE of MKTAG('F','G','D','C') = 0x46474443)

/// Memory-map RIFX types (also stored as LE in XFIR, read as BE).
const RIFX_MV93: u32 = 0x3339564D; // "39VM" (LE of MKTAG('M','V','9','3') = 0x4D563933)
const RIFX_MC95: u32 = 0x3539434D; // "59CM" (LE of MKTAG('M','C','9','5') = 0x3543394D)
const RIFX_APPL: u32 = 0x4C505041; // "LPPA" (LE of MKTAG('A','P','P','L') = 0x4150504C)
const RIFX_MMPM: u32 = 0x524D4D50; // "RMMP" as BE u32 (RIFF Director format)

pub fn parse_afterburner(data: &[u8]) -> Result<AfterburnerFile> {
    read_xfire_header(data)
}

pub fn is_compressed(data: &[u8]) -> bool {
    data.len() >= 4 && &data[0..4] == b"XFIR"
}

/// True when `tag` (read as BE u32 from the file) matches `expected` in either
/// spelling. XFIR stores tags byte-reversed ("revF"), plain RIFX stores them
/// canonical ("Fver").
fn tag_matches(tag: u32, expected: u32) -> bool {
    tag == expected || tag == expected.swap_bytes()
}

/// Classify a file by its 4 codec bytes (offset 8): "afterburner" for
/// FGDM/FGDC, "memory-map" for MV93/MC95/APPL/RMMP. Handles both XFIR
/// (byte-reversed "CDGF") and plain-RIFX (canonical "FGDC") spellings.
pub fn classify(data: &[u8]) -> &'static str {
    if data.len() < 12 {
        return "unknown";
    }
    let codec = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    if tag_matches(codec, TAG_FGDM) || tag_matches(codec, TAG_FGDC) {
        "afterburner"
    } else if tag_matches(codec, RIFX_MV93)
        || tag_matches(codec, RIFX_MC95)
        || tag_matches(codec, RIFX_APPL)
        || tag_matches(codec, RIFX_MMPM)
    {
        "memory-map"
    } else {
        "unknown"
    }
}

/// Read a 4-byte big-endian u32 at the current position.
fn read_be_u32(data: &[u8], pos: &mut usize) -> Result<u32> {
    if *pos + 4 > data.len() {
        return Err(Error::Truncated("expected 4 bytes for u32".into()));
    }
    let val = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(val)
}

/// Read a 4-byte little-endian u32 at the current position.
fn read_le_u32(data: &[u8], pos: &mut usize) -> Result<u32> {
    if *pos + 4 > data.len() {
        return Err(Error::Truncated("expected 4 bytes for u32".into()));
    }
    let val = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(val)
}

/// Read a 2-byte little-endian u16 at the current position.
fn read_le_u16(data: &[u8], pos: &mut usize) -> Result<u16> {
    if *pos + 2 > data.len() {
        return Err(Error::Truncated("expected 2 bytes for u16".into()));
    }
    let val = u16::from_le_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    Ok(val)
}

fn check_tag(data: &[u8], pos: &mut usize, expected: u32) -> Result<()> {
    let tag = read_be_u32(data, pos)?;
    if !tag_matches(tag, expected) {
        let actual = [data[*pos - 4], data[*pos - 3], data[*pos - 2], data[*pos - 1]];
        return Err(Error::Compression(format!(
            "expected tag {:02x?}, got {:02x?}",
            expected.to_be_bytes(),
            actual,
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main parse
// ---------------------------------------------------------------------------

fn read_xfire_header(data: &[u8]) -> Result<AfterburnerFile> {
    if data.len() < 12 {
        return Err(Error::Truncated("container header too small".into()));
    }
    let container = match &data[0..4] {
        b"XFIR" => FileContainer::Xfire,
        b"RIFX" => FileContainer::Rifx,
        _ => return Err(Error::Compression("expected XFIR or RIFX magic".into())),
    };

    // Size field endianness depends on the container (RIFF convention: size = file size - 8).
    let _size = match container {
        FileContainer::Xfire => u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        FileContainer::Rifx => u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
    };
    let codec = [data[8], data[9], data[10], data[11]];
    // The container stores the rifx_type as a LE u32. Reading as BE gives the byte-reversed
    // Canonical MKTAG, which is how we compare against tag constants.
    let rifx_type_be = read_be_u32(data, &mut 8usize)?;

    match rifx_type_be {
        t if tag_matches(t, TAG_FGDM) || tag_matches(t, TAG_FGDC) => {
            // Afterburner-compressed file
            let mut pos: usize = 12;
            let (version, version_string, fver_imap_v, fver_dir_v) = read_fver(data, &mut pos)?;
            let compression_guids = read_fcdr(data, &mut pos)?;
            let (mut resources, abmp_u1, abmp_u2) = read_abmp(data, &mut pos, container)?;
            let (ils_data, ils_body_offset, fgei_unk) = read_fgei(data, &mut pos)?;

            // Walk the ILS buffer: [varint resId][compSize raw bytes] per chunk.
            let mut ils_chunks: HashMap<u32, Vec<u8>> = HashMap::new();
            let mut ip = 0usize;
            while ip < ils_data.len() {
                let res_id = match read_varint(&ils_data, &mut ip) {
                    Ok(id) => id,
                    Err(_) => break,
                };
                let Some(entry) = resources.iter().find(|e| e.resource_id == res_id) else {
                    break;
                };
                let size = entry.compressed_size as usize;
                if ip + size > ils_data.len() || size > MAX_CHUNK_SIZE {
                    break;
                }
                ils_chunks.insert(res_id, ils_data[ip..ip + size].to_vec());
                ip += size;
            }

            // Extract every resource (ILS-resident or body) and build chunk frames.
            let mut frames: Vec<(u32, [u8; 4], Vec<u8>)> = Vec::new();
            resources.sort_by_key(|e| e.resource_id);
            for entry in &resources {
                if entry.resource_id <= 2 {
                    continue; // 0 = RIFX container, 1 = imap-equivalent, 2 = the ILS itself
                }
                let data: Vec<u8> = if let Some(d) = ils_chunks.get(&entry.resource_id) {
                    d.clone()
                } else {
                    if entry.ils_offset == 0 {
                        continue; // offset was -1 but not present in the ILS walk
                    }
                    let start = entry.ils_offset as usize + ils_body_offset;
                    let comp_size = entry.compressed_size as usize;
                    if start + comp_size > data.len() || comp_size > MAX_CHUNK_SIZE {
                        continue;
                    }
                    let comp = &data[start..start + comp_size];
                    let guid = compression_guids.get(entry.compression_type as usize).copied();
                    decompress_chunk(comp, entry.compressed_size, entry.uncompressed_size, guid)?
                };
                if data.is_empty() || data.len() > MAX_CHUNK_SIZE {
                    continue;
                }
                frames.push((entry.resource_id, entry.fourcc, data));
            }

            if frames.is_empty() {
                return Err(Error::Compression("no chunks extracted".into()));
            }
            let chunk_source_ids = frames.iter().map(|f| f.0).collect();
            let rifx_data = build_rifx_container(&frames);

            Ok(AfterburnerFile {
                version,
                version_string,
                resources,
                rifx_data,
                chunk_source_ids,
                endian: Endian::Big,
                container,
                codec,
                fver_imap_v,
                fver_dir_v,
                abmp_u1,
                abmp_u2,
                fgei_unk,
                fcdr_guids: compression_guids,
            })
        }
        t
            if tag_matches(t, RIFX_MV93)
                || tag_matches(t, RIFX_MC95)
                || tag_matches(t, RIFX_APPL)
                || tag_matches(t, RIFX_MMPM) =>
        {
            // Memory-map file (no Afterburner compression)
            read_memory_map(data, 12, container)
        }
        _ => Err(Error::Compression(format!(
            "unrecognized XFIR type: {:02x?}",
            rifx_type_be
        ))),
    }
}

/// Decompress a single resource chunk. zlib when the Fcdr GUID says so,
/// when the data looks like zlib, or when the sizes differ and the GUID
/// isn't the null GUID. Otherwise returns the raw bytes.
fn decompress_chunk(
    comp: &[u8],
    comp_size: u32,
    uncomp_size: u32,
    guid: Option<[u8; 16]>,
) -> Result<Vec<u8>> {
    let is_zlib_guid = guid == Some(ZLIB_COMPRESSION_GUID);
    let is_null_guid = guid == Some([0u8; 16]);
    let looks_zlib = comp.len() >= 2
        && comp[0] == 0x78
        && matches!(comp[1], 0x01 | 0x5e | 0x9c | 0xda);
    let try_zlib = is_zlib_guid
        || looks_zlib
        || (comp_size != uncomp_size && !is_null_guid);

    if try_zlib {
        if let Ok(out) = inflate(comp) {
            return Ok(out);
        }
        if is_zlib_guid {
            return Err(Error::Compression(format!(
                "zlib decompression failed for {comp_size} bytes"
            )));
        }
        // Fall through: not actually zlib, return raw bytes.
    }
    Ok(comp.to_vec())
}

fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| Error::Compression(format!("zlib: {e}")))?;
    Ok(out)
}

fn build_rifx_container(frames: &[(u32, [u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut data = Vec::new();
    for (_, fourcc, chunk_data) in frames {
        data.extend_from_slice(fourcc);
        data.extend_from_slice(&(chunk_data.len() as u32).to_be_bytes());
        data.extend_from_slice(chunk_data);
        if chunk_data.len() % 2 == 1 {
            data.push(0);
        }
    }
    if data.len() % 2 == 1 {
        data.push(0);
    }
    let mut wrapped = Vec::with_capacity(data.len() + 8);
    wrapped.extend_from_slice(b"RIFX");
    wrapped.extend_from_slice(&(data.len() as u32).to_be_bytes());
    wrapped.extend_from_slice(&data);
    wrapped
}

// ---------------------------------------------------------------------------
// Memory map parser (for MV93/MC95/APPL Director files in XFIR wrapper)
// ---------------------------------------------------------------------------

/// TAG for "imap" and "mmap" sections as read via `read_be_u32`.
const TAG_IMAP: u32 = 0x70616D69; // "pami" (LE of MKTAG('i','m','a','p') = 0x70616D69)
const TAG_MMAP: u32 = 0x70616D6D; // "pamm" (LE of MKTAG('m','m','a','p') = 0x70616D6D)

fn read_memory_map(
    data: &[u8],
    rifx_start: usize,
    container: FileContainer,
) -> Result<AfterburnerFile> {
    let mut pos = rifx_start;

    // --- Parse imap (initial memory map) ---
    let tag = read_be_u32(data, &mut pos)?;
    if !tag_matches(tag, TAG_IMAP) {
        return Err(Error::Compression(format!(
            "expected imap tag, got {:02x?}",
            tag
        )));
    }
    let _imap_length = read_le_u32(data, &mut pos)?;
    let _map_version = read_le_u32(data, &mut pos)?;
    let mmap_offset_rifx = read_le_u32(data, &mut pos)? as usize;
    let version = read_le_u32(data, &mut pos)?;

    // --- Seek to mmap ---
    pos = mmap_offset_rifx as usize;

    let tag = read_be_u32(data, &mut pos)?;
    if !tag_matches(tag, TAG_MMAP) {
        return Err(Error::Compression(format!(
            "expected mmap tag, got {:02x?}",
            tag
        )));
    }
    let _mmap_length = read_le_u32(data, &mut pos)?;
    let _header_size = read_le_u16(data, &mut pos)?;
    let entry_size = read_le_u16(data, &mut pos)? as usize;
    let _total_count = read_le_u32(data, &mut pos)?;
    let res_count = read_le_u32(data, &mut pos)? as usize;
    pos += 8; // skip padding
    let _first_free = read_le_u32(data, &mut pos)?;

    // --- Build resources and extract chunks ---
    let mut chunks: Vec<(u32, [u8; 4], Vec<u8>)> = Vec::new();
    let version_string = String::new();

    for i in 0..res_count {
        if pos + entry_size > data.len() {
            break;
        }
        let start_pos = pos;

        let tag_le = read_le_u32(data, &mut pos)?;
        let size = read_le_u32(data, &mut pos)? as usize;
        let offset_file = read_le_u32(data, &mut pos)? as usize;
        let _flags = read_le_u16(data, &mut pos)?;
        let _unk1 = read_le_u16(data, &mut pos)?;
        let _next_free = read_le_u32(data, &mut pos)?;

        // Skip resource ID 0 (the RIFX container = self-reference at index 0)
        // Also skip zero-size or out-of-bounds resources
        if i == 0 || size == 0 || size > MAX_CHUNK_SIZE {
            continue;
        }

        if offset_file + size > data.len() {
            continue;
        }

        // XFIR stores the FourCC tag as a LE u32 of the MKTAG value; plain RIFX
        // stores it canonical. Reconstruct the canonical spelling either way.
        let fourcc = match container {
            FileContainer::Xfire => tag_le.to_be_bytes(),
            FileContainer::Rifx => tag_le.to_le_bytes(),
        };

        // Memory-map resources are stored WITH their own 8-byte chunk header
        // (byte-reversed FourCC + LE length) at the mmap offset. The mmap
        // `size` field is the DATA length: the resource's own header mirrors
        // it (verified: header length == mmap size for every resource, and
        // resources are packed at [offset, offset + 8 + size)).
        // build_rifx_container writes the canonical header from this entry's
        // tag, so skip the resource's own header and keep the full `size`
        // bytes of data — LibreShockwave DirectorFile::loadRIFX slices
        // [offset + 8, offset + 8 + length). Slicing [offset, offset+size)
        // then stripping 8 here would drop the tail 8 bytes of every
        // resource: for CASt members that truncates the Pascal name at the
        // end of the info block, yielding `member_N` fallback names, and for
        // LctX/Lscr it loses trailing entries/bytes. Degenerate stubs (data
        // not starting with the reversed tag) are passed through untouched.
        let raw = &data[offset_file..(offset_file + size + 8).min(data.len())];
        let rev: [u8; 4] = [fourcc[3], fourcc[2], fourcc[1], fourcc[0]];
        let chunk_data = if raw.len() >= 8 && raw[..4] == rev {
            &raw[8..]
        } else {
            raw
        };
        chunks.push((i as u32, fourcc, chunk_data.to_vec()));

        // Restore position in case entry_size > actual bytes consumed
        pos = start_pos + entry_size;
    }

    if chunks.is_empty() {
        return Err(Error::Compression("no chunks extracted from memory map".into()));
    }

    let chunk_source_ids = chunks.iter().map(|c| c.0).collect();
    let rifx_data = build_rifx_container(&chunks);

    // Content endianness: the mmap/imap tables and the KEY* key table are
    // little-endian (read with explicit LE readers), but the actual chunk
    // contents (CASt member data, BITD, LctX, STXT, Lscr, ...) are big-endian
    // Director format — verified against Habbo v31 MV93 files (fuse_client,
    // hh_ig_game_snowwar: CASt member types read as 1..14 only when big-endian).
    // Afterburner files use Endian::Big for the same content; the container
    // format (XFIR vs RIFX) must not change how chunk contents are read.
    Ok(AfterburnerFile {
        version,
        version_string,
        resources: Vec::new(), // memory map doesn't use ABMP resources
        rifx_data,
        chunk_source_ids,
        endian: Endian::Big,
        container,
        codec: [0; 4],
        fver_imap_v: 0,
        fver_dir_v: 0,
        abmp_u1: 0,
        abmp_u2: 0,
        fgei_unk: 0,
        fcdr_guids: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Varint (Afterburner MSB-first: v = (v << 7) | (byte & 0x7F))
// ---------------------------------------------------------------------------

/// Write an Afterburner MSB-first varint.
pub fn write_varint(out: &mut Vec<u8>, mut v: u32) {
    // Collect 7-bit groups least-significant first, then emit MSB-first with
    // the continuation bit set on every byte except the last.
    let mut groups = Vec::with_capacity(5);
    groups.push((v & 0x7F) as u8);
    v >>= 7;
    while v > 0 {
        groups.push(0x80 | (v & 0x7F) as u8);
        v >>= 7;
    }
    groups.reverse();
    out.extend_from_slice(&groups);
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<u32> {
    let mut result = 0u32;
    for _ in 0..5 {
        if *pos >= data.len() {
            return Err(Error::Truncated("varint past end".into()));
        }
        let byte = data[*pos];
        *pos += 1;
        result = (result << 7) | ((byte & 0x7F) as u32);
        if byte & 0x80 == 0 {
            return Ok(result);
        }
    }
    Err(Error::Compression("varint too long".into()))
}

// ---------------------------------------------------------------------------
// Section parsers
// ---------------------------------------------------------------------------

fn read_fver(data: &[u8], pos: &mut usize) -> Result<(u32, String, u32, u32)> {
    check_tag(data, pos, TAG_FVER)?;
    let fver_length = read_varint(data, pos)? as usize;
    let start = *pos;

    let ab_version = read_varint(data, pos)?;
    let mut vs = String::new();
    let mut imap_v = 0u32;
    let mut dir_v = 0u32;

    if ab_version >= 0x401 {
        imap_v = read_varint(data, pos)?;
        dir_v = read_varint(data, pos)?;
    }
    if ab_version >= 0x501 {
        if *pos >= data.len() {
            return Err(Error::Truncated("Fver string len truncated".into()));
        }
        let sl = data[*pos] as usize;
        *pos += 1;
        if *pos + sl > data.len() {
            return Err(Error::Truncated("Fver string truncated".into()));
        }
        if sl > 0 {
            vs = String::from_utf8_lossy(&data[*pos..*pos + sl]).to_string();
            *pos += sl;
        }
    }
    let consumed = *pos - start;
    if consumed < fver_length {
        *pos = start + fver_length;
    }
    Ok((ab_version, vs, imap_v, dir_v))
}

/// Fcdr lists the compression type GUIDs used by ABMP compressionType indexes.
fn read_fcdr(data: &[u8], pos: &mut usize) -> Result<Vec<[u8; 16]>> {
    check_tag(data, pos, TAG_FCDR)?;
    let fcdr_length = read_varint(data, pos)? as usize;

    let (fcdr_data, consumed) = decompress_zlib_block(data, *pos, fcdr_length)?;
    *pos += consumed;

    let mut guids = Vec::new();
    if fcdr_data.len() >= 2 {
        let count = u16::from_le_bytes([fcdr_data[0], fcdr_data[1]]) as usize;
        let mut p = 2usize;
        for _ in 0..count.min(64) {
            if p + 16 > fcdr_data.len() {
                break;
            }
            let mut g = [0u8; 16];
            g.copy_from_slice(&fcdr_data[p..p + 16]);
            guids.push(g);
            p += 16;
        }
    }
    Ok(guids)
}

fn read_abmp(
    data: &[u8],
    pos: &mut usize,
    container: FileContainer,
) -> Result<(Vec<AbmpEntry>, u32, u32)> {
    check_tag(data, pos, TAG_ABMP)?;

    let _abmp_length = read_varint(data, pos)? as usize;
    let _compression_type = read_varint(data, pos)?;
    let _uncomp_length = read_varint(data, pos)?;

    // Don't rely on abmp_length as the region bound — the varint can be
    // too small, truncating the zlib stream. Pass the remaining file data and
    // let zlib self-delimit via total_in() to find the exact end position.
    let (abmp_data, consumed) = decompress_zlib_block(data, *pos, data.len().saturating_sub(*pos))?;
    *pos += consumed;

    let mut ap = 0usize;
    let u1 = match read_varint(&abmp_data, &mut ap) {
        Ok(v) => v,
        Err(_) => return Ok((Vec::new(), 1, 0)),
    };
    let u2 = match read_varint(&abmp_data, &mut ap) {
        Ok(v) => v,
        Err(_) => return Ok((Vec::new(), u1, 0)),
    };
    let _ = u1;
    let res_count = match read_varint(&abmp_data, &mut ap) {
        Ok(v) => v as usize,
        Err(_) => return Ok((Vec::new(), u1, u2)),
    };

    let mut resources = Vec::with_capacity(res_count.min(100_000));
    for _ in 0..res_count.min(100_000) {
        if ap + 4 > abmp_data.len() {
            break;
        }
        let res_id = match read_varint(&abmp_data, &mut ap) {
            Ok(v) => v,
            Err(_) => break,
        };
        let offset_raw = match read_varint(&abmp_data, &mut ap) {
            Ok(v) => v,
            Err(_) => break,
        };
        // Offset is signed; -1 (0xFFFFFFFF) marks ILS-resident resources.
        let offset = if (offset_raw as i32) >= 0 {
            offset_raw as u64
        } else {
            0
        };
        let comp_size = match read_varint(&abmp_data, &mut ap) {
            Ok(v) => v,
            Err(_) => break,
        };
        let uncomp_size = match read_varint(&abmp_data, &mut ap) {
            Ok(v) => v,
            Err(_) => break,
        };
        let compression_type = match read_varint(&abmp_data, &mut ap) {
            Ok(v) => v,
            Err(_) => break,
        };

        let mut fourcc = [0u8; 4];
        if ap + 4 <= abmp_data.len() {
            let raw = [
                abmp_data[ap],
                abmp_data[ap + 1],
                abmp_data[ap + 2],
                abmp_data[ap + 3],
            ];
            // XFIR stores the fourcc as a LE u32 ("tSAC"), plain RIFX stores it
            // canonical ("CASt"). Reconstruct the canonical spelling either way.
            fourcc = match container {
                FileContainer::Xfire => u32::from_le_bytes(raw).to_be_bytes(),
                FileContainer::Rifx => raw,
            };
            ap += 4;
        }

        resources.push(AbmpEntry {
            resource_id: res_id,
            ils_offset: offset,
            compressed_size: comp_size,
            uncompressed_size: uncomp_size,
            compression_type,
            fourcc,
        });
    }

    Ok((resources, u1, u2))
}

/// Reads the FGEI section and returns (decompressed ILS buffer, file offset of
/// the ILS zlib data — used as the base for body resource offsets, unknown varint).
fn read_fgei(data: &[u8], pos: &mut usize) -> Result<(Vec<u8>, usize, u32)> {
    // After ABMP zlib data, there may be padding bytes before FGEI.
    // Scan for the FGEI tag within a reasonable window instead of requiring
    // it at an exact offset.
    let start = *pos;
    let search_end = (start + 64).min(data.len().saturating_sub(4));
    let found = (start..search_end)
        .find(|&i| tag_matches(u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]), TAG_FGEI))
        .ok_or_else(|| {
            let nearby = &data[start..(start + 16).min(data.len())];
            Error::Compression(format!(
                "FGEI tag not found within 64 bytes after offset 0x{:x} (data: {:02x?})",
                start, nearby
            ))
        })?;
    *pos = found + 4; // position past the FGEI tag
    let ils_unk1 = read_varint(data, pos)?;

    let ils_body_offset = *pos;
    let ils_max_len = data.len().saturating_sub(*pos);
    let (ils_data, consumed) = decompress_zlib_block(data, *pos, ils_max_len)?;
    *pos += consumed;
    Ok((ils_data, ils_body_offset, ils_unk1))
}

/// Decompress a zlib block starting at `start` in `data`.
/// Returns (decompressed_bytes, number_of_input_bytes_consumed_by_zlib).
fn decompress_zlib_block(data: &[u8], start: usize, max_len: usize) -> Result<(Vec<u8>, usize)> {
    let end = (start + max_len).min(data.len());
    if start >= end {
        return Err(Error::Compression("empty zlib block".into()));
    }
    let region = &data[start..end];
    let zlib_off = region
        .iter()
        .position(|&b| b == 0x78)
        .ok_or_else(|| Error::Compression("no zlib magic (0x78) found".into()))?;

    let mut decoder = ZlibDecoder::new(&region[zlib_off..]);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| Error::Compression(format!("zlib: {e}")))?;

    // total_in() returns how many bytes the decoder consumed from the input
    let consumed = decoder.total_in() as usize;
    Ok((out, zlib_off + consumed))
}

// ---------------------------------------------------------------------------
// Afterburner compression (writer)
// ---------------------------------------------------------------------------

/// Options for writing an Afterburner-compressed file. The defaults are the
/// values observed in the Habbo CCTs (Director 7.5.1 Afterburner / "CDGF").
#[derive(Debug, Clone)]
pub struct CompressOptions {
    /// The container wrapper to emit (XFIR = LE size, RIFX = BE size).
    pub container: FileContainer,
    /// The 4 codec bytes after the container header ("CDGF" or "MDGF").
    pub codec: [u8; 4],
    pub fver_ab_version: u32,
    pub fver_imap_v: u32,
    pub fver_dir_v: u32,
    pub fver_string: String,
    /// Compression GUIDs for the Fcdr section (index 0 = the zlib GUID).
    pub fcdr_guids: Vec<[u8; 16]>,
    pub abmp_u1: u32,
    pub abmp_u2: u32,
    pub fgei_unk: u32,
}

impl Default for CompressOptions {
    fn default() -> Self {
        CompressOptions {
            container: FileContainer::Xfire,
            codec: *b"CDGF",
            fver_ab_version: 0x501,
            fver_imap_v: 1,
            fver_dir_v: 1850,
            fver_string: "10.0#188".to_string(),
            // The zlib compression GUID exactly as stored in the Habbo CCTs' Fcdr.
            fcdr_guids: vec![[
                0x04, 0xe9, 0x99, 0xac, 0x70, 0x00, 0x36, 0x0b, 0x00, 0x00, 0x08, 0x00, 0x07,
                0x37, 0x7a, 0x34,
            ]],
            abmp_u1: 1,
            abmp_u2: 2805,
            fgei_unk: 0,
        }
    }
}

impl CompressOptions {
    /// Copy the header constants captured from a parsed Afterburner file so a
    /// recompression stays byte-faithful to the original container.
    pub fn from_parsed(af: &AfterburnerFile) -> CompressOptions {
        let mut o = CompressOptions::default();
        o.container = af.container;
        // Memory-map files don't carry a codec ([0;4]) — keep the default
        // (compressing a memory-map file isn't supported anyway).
        if af.codec != [0; 4] {
            o.codec = af.codec;
        }
        o.fver_ab_version = af.version;
        o.fver_imap_v = af.fver_imap_v;
        o.fver_dir_v = af.fver_dir_v;
        o.fver_string = af.version_string.clone();
        if !af.fcdr_guids.is_empty() {
            o.fcdr_guids = af.fcdr_guids.clone();
        }
        o.abmp_u1 = af.abmp_u1;
        o.abmp_u2 = af.abmp_u2;
        o.fgei_unk = af.fgei_unk;
        o
    }
}

fn zlib_compress(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).expect("zlib write");
    enc.finish().expect("zlib finish")
}

/// One resource to write. `in_ils` selects the Initial Loading Set (raw bytes
/// inside the FGEI zlib) vs the compressed body (its own zlib stream).
pub struct CompressResource {
    pub resource_id: u32,
    pub fourcc: [u8; 4],
    pub data: Vec<u8>,
    pub in_ils: bool,
}

/// Write an Afterburner (XFIR) file from resource frames.
///
/// Layout (mirrors what the Habbo CCTs do):
///   XFIR header + Fver + Fcdr(zlib GUID list) + ABMP(zlib metadata)
///   + FGEI(varint + zlib of the ILS) + zlib body resources
/// Resource 2 is the ILS itself (offset 0). ILS-resident resources have
/// offset -1 and are stored raw inside the ILS; body resources are individual
/// zlib streams whose ABMP offset is relative to the ILS zlib start.
pub fn compress(frames: &[CompressResource], opts: &CompressOptions) -> Vec<u8> {
    // Sort by resource id (matches the decompressor's ordering).
    let mut sorted: Vec<&CompressResource> = frames.iter().collect();
    sorted.sort_by_key(|f| f.resource_id);

    let ils_frames: Vec<&&CompressResource> = sorted.iter().filter(|f| f.in_ils).collect();
    let body_frames: Vec<&&CompressResource> = sorted.iter().filter(|f| !f.in_ils).collect();

    // ILS data: [varint resId][raw chunk bytes] per ILS-resident resource.
    let mut ils_data = Vec::new();
    for f in &ils_frames {
        write_varint(&mut ils_data, f.resource_id);
        ils_data.extend_from_slice(&f.data);
    }
    let ils_zlib = zlib_compress(&ils_data);

    // Body resources: each its own zlib stream, laid out after the ILS zlib.
    let body_blobs: Vec<(u32, Vec<u8>)> = body_frames
        .iter()
        .map(|f| (f.resource_id, zlib_compress(&f.data)))
        .collect();

    // ABMP metadata: u1, u2, resCount, then per-resource entries.
    // resCount = 1 (resource 2, the ILS itself) + all frames.
    let mut abmp = Vec::new();
    write_varint(&mut abmp, opts.abmp_u1);
    write_varint(&mut abmp, opts.abmp_u2);
    write_varint(&mut abmp, 1 + sorted.len() as u32);

    // Resource 2 = the ILS: offset 0, comp = ils zlib len, uncomp = ils len.
    write_varint(&mut abmp, 2);
    write_varint(&mut abmp, 0);
    write_varint(&mut abmp, ils_zlib.len() as u32);
    write_varint(&mut abmp, ils_data.len() as u32);
    write_varint(&mut abmp, 0);
    // The ILS fourcc: XFIR stores it reversed ("ILS "), RIFX canonical.
    match opts.container {
        FileContainer::Xfire => abmp.extend_from_slice(&[b' ', b'S', b'L', b'I']),
        FileContainer::Rifx => abmp.extend_from_slice(b"ILS "),
    }

    // Per-resource entries (XFIR stores the fourcc byte-reversed, RIFX canonical).
    // Body offsets are relative to the ILS zlib start, so they begin AFTER the
    // ILS zlib stream (offset 0 is reserved for the ILS itself, resource 2 — a
    // body resource at offset 0 would be misread as ILS-resident and dropped).
    let mut body_off = ils_zlib.len() as u32;
    let mut body_index = 0usize;
    for f in &sorted {
        let stored_tag = match opts.container {
            FileContainer::Xfire => [f.fourcc[3], f.fourcc[2], f.fourcc[1], f.fourcc[0]],
            FileContainer::Rifx => f.fourcc,
        };
        if f.in_ils {
            write_varint(&mut abmp, f.resource_id);
            write_varint(&mut abmp, u32::MAX); // -1: ILS-resident
            write_varint(&mut abmp, f.data.len() as u32);
            write_varint(&mut abmp, f.data.len() as u32);
            write_varint(&mut abmp, 0);
            abmp.extend_from_slice(&stored_tag);
        } else {
            let (_, blob) = &body_blobs[body_index];
            write_varint(&mut abmp, f.resource_id);
            write_varint(&mut abmp, body_off);
            write_varint(&mut abmp, blob.len() as u32);
            write_varint(&mut abmp, f.data.len() as u32);
            write_varint(&mut abmp, 0);
            abmp.extend_from_slice(&stored_tag);
            body_off += blob.len() as u32;
            body_index += 1;
        }
    }
    let abmp_zlib = zlib_compress(&abmp);

    // Fcdr: u16le count + GUIDs, zlib.
    let mut fcdr = Vec::new();
    fcdr.extend_from_slice(&(opts.fcdr_guids.len() as u16).to_le_bytes());
    for g in &opts.fcdr_guids {
        fcdr.extend_from_slice(g);
    }
    let fcdr_zlib = zlib_compress(&fcdr);

    // Fver.
    let mut fver = Vec::new();
    write_varint(&mut fver, opts.fver_ab_version);
    if opts.fver_ab_version >= 0x401 {
        write_varint(&mut fver, opts.fver_imap_v);
        write_varint(&mut fver, opts.fver_dir_v);
    }
    if opts.fver_ab_version >= 0x501 {
        let s = opts.fver_string.as_bytes();
        fver.push(s.len() as u8);
        fver.extend_from_slice(s);
    }

    // Assemble the file. The 8-byte header is magic + size; the codec chunk
    // right after it carries no length field (matching the originals).
    // Section tags: XFIR stores them reversed ("revF"), RIFX canonical ("Fver").
    let mut out = Vec::new();
    match opts.container {
        FileContainer::Xfire => out.extend_from_slice(b"XFIR"),
        FileContainer::Rifx => out.extend_from_slice(b"RIFX"),
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // size placeholder (patched below)
    out.extend_from_slice(&opts.codec);

    let (fver_tag, fcdr_tag, abmp_tag, fgei_tag) = match opts.container {
        FileContainer::Xfire => (b"revF" as &[u8], b"rdcF" as &[u8], b"PMBA" as &[u8], b"IEGF" as &[u8]),
        FileContainer::Rifx => (b"Fver" as &[u8], b"Fcdr" as &[u8], b"ABMP" as &[u8], b"FGEI" as &[u8]),
    };

    out.extend_from_slice(fver_tag);
    write_varint(&mut out, fver.len() as u32);
    out.extend_from_slice(&fver);

    out.extend_from_slice(fcdr_tag);
    write_varint(&mut out, fcdr_zlib.len() as u32);
    out.extend_from_slice(&fcdr_zlib);

    out.extend_from_slice(abmp_tag);
    write_varint(&mut out, abmp_zlib.len() as u32);
    write_varint(&mut out, 0); // compression type
    write_varint(&mut out, abmp.len() as u32); // uncompressed length
    out.extend_from_slice(&abmp_zlib);

    out.extend_from_slice(fgei_tag);
    write_varint(&mut out, opts.fgei_unk);
    out.extend_from_slice(&ils_zlib);
    for (_, blob) in &body_blobs {
        out.extend_from_slice(blob);
    }

    // The size field covers everything after the 8-byte magic+size header
    // (matches the original files: pool 169072 = 169080 - 8). XFIR is LE, RIFX BE.
    let total = out.len() as u32;
    let size = total.wrapping_sub(8);
    match opts.container {
        FileContainer::Xfire => out[4..8].copy_from_slice(&size.to_le_bytes()),
        FileContainer::Rifx => out[4..8].copy_from_slice(&size.to_be_bytes()),
    }
    out
}

/// Recompress a parsed Afterburner file (byte-faithful round-trip: same
/// resource ids, same ILS/body layout, same container constants).
pub fn compress_parsed(af: &AfterburnerFile) -> Result<Vec<u8>> {
    let mut pos = 0u64;
    let (root, _) = crate::chunk::read_chunk(&af.rifx_data, &mut pos)
        .map_err(|e| Error::Compression(format!("re-parse for compression: {e}")))?;

    let mut frames = Vec::new();
    for (i, child) in root.children.iter().enumerate() {
        let res_id = af.chunk_source_ids.get(i).copied().unwrap_or(3 + i as u32);
        // ILS-resident iff the original ABMP entry had offset 0 (and isn't res 2).
        let in_ils = af
            .resources
            .iter()
            .find(|e| e.resource_id == res_id)
            .map(|e| e.ils_offset == 0)
            .unwrap_or(false);
        let mut fourcc = [0u8; 4];
        fourcc.copy_from_slice(child.fourcc().as_bytes());
        frames.push(CompressResource {
            resource_id: res_id,
            fourcc,
            data: child.data().to_vec(),
            in_ils,
        });
    }

    let opts = CompressOptions::from_parsed(af);
    Ok(compress(&frames, &opts))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_single_byte() {
        let data = [0x05];
        let mut pos = 0;
        assert_eq!(read_varint(&data, &mut pos).unwrap(), 5);
        assert_eq!(pos, 1);
    }

    #[test]
    fn varint_multi_byte_msb() {
        // MSB-first: 1174 = 0b100_10010110 → bytes [0x89, 0x16]
        let data = [0x89, 0x16];
        let mut pos = 0;
        assert_eq!(read_varint(&data, &mut pos).unwrap(), 1174);
        assert_eq!(pos, 2);
    }

    #[test]
    fn varint_max_five_bytes() {
        // 4294967295 (0xFFFFFFFF) encodes as 5 bytes MSB-first:
        // 1111 | 1111111 | 1111111 | 1111111 | 1111111
        let data = [0x8F, 0xFF, 0xFF, 0xFF, 0x7F];
        let mut pos = 0;
        assert_eq!(read_varint(&data, &mut pos).unwrap(), u32::MAX);
        assert_eq!(pos, 5);
    }

    #[test]
    fn detect_compressed() {
        assert!(is_compressed(b"XFIR\x00\x00\x00\x00"));
        assert!(!is_compressed(b"RIFX\x00\x00\x00\x00"));
    }

    #[test]
    fn tag_matches_both_spellings() {
        // XFIR stores section tags reversed ("revF"), plain RIFX canonical ("Fver").
        let rev_f = u32::from_be_bytes(*b"revF");
        let fver = u32::from_be_bytes(*b"Fver");
        assert_eq!(TAG_FVER, rev_f);
        assert!(tag_matches(rev_f, TAG_FVER));
        assert!(tag_matches(fver, TAG_FVER));
        assert!(!tag_matches(0xDEAD_BEEF, TAG_FVER));
    }

    #[test]
    fn classify_both_containers() {
        // XFIR: codec stored reversed ("CDGF"). RIFX: canonical ("FGDC").
        let mut xfir = vec![b'X'; 12];
        xfir[8..12].copy_from_slice(b"CDGF");
        assert_eq!(classify(&xfir), "afterburner");
        let mut rifx = vec![b'R'; 12];
        rifx[8..12].copy_from_slice(b"FGDC");
        assert_eq!(classify(&rifx), "afterburner");
        let mut mm = vec![b'X'; 12];
        mm[8..12].copy_from_slice(b"39VM");
        assert_eq!(classify(&mm), "memory-map");
    }

    #[test]
    fn rifx_container_roundtrip() {
        // A RIFX-wrapped Afterburner file must re-parse as container Rifx and
        // round-trip its body resource losslessly.
        let mut opts = CompressOptions::default();
        opts.container = FileContainer::Rifx;
        opts.codec = *b"FGDC"; // RIFX stores the codec canonical (XFIR stores "CDGF")
        let frames = vec![CompressResource {
            resource_id: 3,
            fourcc: *b"CASt",
            data: b"hello director".to_vec(),
            in_ils: false,
        }];
        let out = compress(&frames, &opts);
        assert_eq!(&out[0..4], b"RIFX");
        assert_eq!(&out[8..12], b"FGDC");
        assert_eq!(&out[12..16], b"Fver"); // canonical tag for RIFX

        let af = parse_afterburner(&out).unwrap();
        assert_eq!(af.container, FileContainer::Rifx);
        assert_eq!(af.chunk_source_ids, vec![3]);
        assert_eq!(af.rifx_data.len(), 8 + 8 + b"hello director".len());
    }
}
