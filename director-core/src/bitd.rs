//! BITD (Bitmap Data) chunk parser and pixel decoder.
//!
//! BITD chunks contain RLE-compressed pixel data for cast member bitmaps.
//! The data is stored as palette indices (1/2/4/8 bpp) or raw pixels (16/32 bpp).
//!
//! RLE format (from ScummVM images.cpp):
//!   Each byte controls a run:
//!     If byte & 0x80: repeat run. len = ((byte ^ 0xFF) & 0xFF) + 2, next byte = value
//!     Else: literal run. len = byte + 1, read len bytes
//!   If the data size exactly matches expected (pitch * height), skip decompression.
//!
//! Format reference: ScummVM Director engine, images.cpp (BITDDecoder::loadStream)

use director_rifx::{Chunk, Endian};
use crate::ParseError;

/// Bitmap metadata extracted from the parent CASt header.
#[derive(Debug, Clone, Default)]
pub struct BitmapInfo {
    pub width: u16,
    pub height: u16,
    pub bits_per_pixel: u8,
    pub has_palette: bool,
    pub palette_id: i16,
    pub reg_x: i16,
    pub reg_y: i16,
    pub pitch: u16,
}

/// Decoded RGBA pixel data ready for image output.
#[derive(Debug, Clone)]
pub struct DecodedBitmap {
    pub width: usize,
    pub height: usize,
    /// RGBA pixels (4 bytes per pixel, row-major, top-to-bottom)
    pub rgba: Vec<u8>,
    pub info: BitmapInfo,
}

/// Parsed BITD chunk with raw pixel data and optional decoded RGBA output.
#[derive(Debug, Clone)]
pub struct BitmapData {
    pub info: BitmapInfo,
    pub pixel_data: Vec<u8>,
    pub decoded: Option<DecodedBitmap>,
}

fn looks_like_zlib(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0x78 && matches!(data[1], 0x01 | 0x5e | 0x9c | 0xda)
}

/// Decompress a raw byte stream to at most `expected` bytes.
/// Handles zlib-compressed payloads, already-uncompressed data, and
/// byte-level RLE (ScummVM/LibreShockwave compatible: never errors,
/// pads with zeros at the end).
fn decompress_bytes(data: &[u8], expected: usize) -> Result<Vec<u8>, ParseError> {
    if looks_like_zlib(data) {
        let mut decoder = flate2::read::ZlibDecoder::new(data);
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut out)
            .map_err(|e| ParseError::InvalidData(format!("BITD zlib: {e}")))?;
        out.truncate(expected);
        return Ok(out);
    }

    // If the data is at least as big as expected, treat it as uncompressed.
    if data.len() >= expected {
        return Ok(data[..expected].to_vec());
    }

    // RLE decompression: runs never error, pad at the end.
    let mut pixels = Vec::with_capacity(expected);
    let mut i = 0;
    while i < data.len() && pixels.len() < expected {
        let byte = data[i];
        i += 1;
        if byte & 0x80 != 0 {
            // Repeat run: len = (~byte & 0xFF) + 2
            let len = ((byte ^ 0xFF) & 0xFF) as usize + 2;
            if i >= data.len() {
                break;
            }
            let value = data[i];
            i += 1;
            let actual_len = len.min(expected - pixels.len());
            pixels.resize(pixels.len() + actual_len, value);
        } else {
            // Literal run: len = byte + 1
            let len = (byte as usize) + 1;
            let available = data.len() - i;
            let actual_len = len.min(available).min(expected - pixels.len());
            pixels.extend_from_slice(&data[i..i + actual_len]);
            i += actual_len;
        }
    }
    pixels.resize(expected, 0);
    Ok(pixels)
}

/// Decompress RLE-encoded BITD pixel data.
/// Returns flat pixel indices (one per pixel, palette-indexed for bpp <= 8).
///
/// Follows ScummVM's Director engine behavior:
/// - If the data size exactly matches expected, treats as uncompressed.
/// - If a run at the end of data would exceed the buffer, reads what's available.
/// - After decompression, pads with zeros to expected_bytes if needed.
pub fn decompress_rle(data: &[u8], width: usize, height: usize, bpp: u8, pitch: usize) -> Result<Vec<u8>, ParseError> {
    let expected_bytes = pitch * height;
    let raw = decompress_bytes(data, expected_bytes)?;

    // Now unpack raw bytes into per-pixel values
    let pixel_count = width * height;
    let mut indices = Vec::with_capacity(pixel_count);

    match bpp {
        1 => {
            for y in 0..height {
                let row_start = y * pitch;
                for x in 0..width {
                    let byte_idx = row_start + (x >> 3);
                    let bit = 7 - (x & 7);
                    let val = if byte_idx < raw.len() {
                        (raw[byte_idx] >> bit) & 1
                    } else {
                        0
                    };
                    indices.push(val);
                }
            }
        }
        2 => {
            for y in 0..height {
                let row_start = y * pitch;
                for x in 0..width {
                    let byte_idx = row_start + (x >> 2);
                    let shift = 2 * (3 - (x & 3));
                    let val = if byte_idx < raw.len() {
                        (raw[byte_idx] >> shift) & 3
                    } else {
                        0
                    };
                    indices.push(val);
                }
            }
        }
        4 => {
            for y in 0..height {
                let row_start = y * pitch;
                for x in 0..width {
                    let byte_idx = row_start + (x >> 1);
                    let shift = 4 * (1 - (x & 1));
                    let val = if byte_idx < raw.len() {
                        (raw[byte_idx] >> shift) & 0xF
                    } else {
                        0
                    };
                    indices.push(val);
                }
            }
        }
        8 => {
            for y in 0..height {
                let row_start = y * pitch;
                for x in 0..width {
                    let val = if row_start + x < raw.len() {
                        raw[row_start + x]
                    } else {
                        0
                    };
                    indices.push(val);
                }
            }
        }
        _ => return Err(ParseError::InvalidData(format!("unsupported bpp: {bpp}"))),
    }

    Ok(indices)
}

/// Convert palette-indexed pixel data to RGBA using a color table.
/// `pixels` is a Vec of u8 palette indices.
/// `palette` is a Vec of (r, g, b) tuples, length 256.
pub fn apply_palette(pixels: &[u8], palette: &[(u8, u8, u8)], width: usize, height: usize) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let pi = if idx < pixels.len() { pixels[idx] as usize } else { 0 };
            let (r, g, b) = if pi < palette.len() { palette[pi] } else { (0, 0, 0) };
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            rgba.push(255); // opaque
        }
    }
    rgba
}

/// Decode 16-bit pixels (5-5-5) to RGBA, following LibreShockwave decode16Bit.
/// When the data was RLE-compressed, each row is laid out as high bytes then
/// low bytes; otherwise pixels are interleaved big-endian u16s.
fn decode_16bit(data: &[u8], width: usize, height: usize, pitch: usize) -> Result<Vec<u8>, ParseError> {
    let scan_width = if pitch > 0 { (pitch * 8) / 16 } else { width };
    let expected = scan_width * height * 2;
    let was_compressed = data.len() < expected;
    let raw = decompress_bytes(data, expected)?;

    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let pixel16 = if was_compressed {
                let row_off = y * scan_width * 2;
                let hi = row_off + x;
                let lo = row_off + scan_width + x;
                if lo >= raw.len() {
                    rgba.extend_from_slice(&[0, 0, 0, 255]);
                    continue;
                }
                ((raw[hi] as u16) << 8) | raw[lo] as u16
            } else {
                let off = (y * scan_width + x) * 2;
                if off + 1 >= raw.len() {
                    rgba.extend_from_slice(&[0, 0, 0, 255]);
                    continue;
                }
                ((raw[off] as u16) << 8) | raw[off + 1] as u16
            };
            let r5 = (pixel16 >> 10) & 0x1F;
            let g5 = (pixel16 >> 5) & 0x1F;
            let b5 = pixel16 & 0x1F;
            rgba.extend_from_slice(&[
                ((r5 << 3) | (r5 >> 2)) as u8,
                ((g5 << 3) | (g5 >> 2)) as u8,
                ((b5 << 3) | (b5 >> 2)) as u8,
                255,
            ]);
        }
    }
    Ok(rgba)
}

/// Decode 32-bit pixels (byte order A,R,G,B) to RGBA, following
/// LibreShockwave decode32Bit (interleaved, pre-D10).
fn decode_32bit(
    data: &[u8],
    width: usize,
    height: usize,
    pitch: usize,
    director_version: u16,
) -> Result<Vec<u8>, ParseError> {
    let scan_width = if pitch > 0 { pitch / 4 } else { width };
    let expected = scan_width * height * 4;
    let raw = decompress_bytes(data, expected)?;

    // LibreShockwave decode32Bit: Director >= 1000 (D5+) stores 32-bit pixels
    // as SEPARATED channel planes (four scan_width*height planes in A,R,G,B
    // order); older files interleave ARGB per pixel. Habbo's D7 CCTs are
    // separated — decoding them interleaved scrambles the channels (the
    // catalog's club_neg/club_pos icons and room pen masks render as
    // semi-transparent black smudges).
    let channels_separated = director_version >= 1000;
    let mut rgba = Vec::with_capacity(width * height * 4);
    if channels_separated {
        for y in 0..height {
            let line = y * scan_width * 4;
            for x in 0..width {
                let a = line + x;
                let r = line + x + scan_width;
                let g = line + x + scan_width * 2;
                let b = line + x + scan_width * 3;
                if b >= raw.len() {
                    rgba.extend_from_slice(&[0, 0, 0, 255]);
                    continue;
                }
                rgba.extend_from_slice(&[raw[r], raw[g], raw[b], raw[a]]);
            }
        }
    } else {
        for y in 0..height {
            for x in 0..width {
                let byte_idx = (y * scan_width + x) * 4;
                if byte_idx + 3 >= raw.len() {
                    rgba.extend_from_slice(&[0, 0, 0, 255]);
                    continue;
                }
                // Stored byte order: A, R, G, B
                rgba.extend_from_slice(&[
                    raw[byte_idx + 1],
                    raw[byte_idx + 2],
                    raw[byte_idx + 3],
                    raw[byte_idx],
                ]);
            }
        }
    }
    Ok(rgba)
}

/// Decode BITD pixel data to RGBA for any supported bit depth.
/// 1/2/4/8 bpp use the palette; 16 bpp is 5-5-5; 32 bpp is ARGB bytes.
///
/// 1-bit exception: Director stores 1-bit bitmaps as pure ink/paper bits, NOT
/// palette indices. ScummVM's BITDDecoder pre-converts them to 0x00/0xff
/// ("1bpp - this is preconverted to 0x00 and 0xff, change nothing") and its
/// dither path leaves 1bpp untouched. Palette-mapping a 1-bit shadow through
/// SystemWinD5 gives cyan (entry 1 = 0,255,255) instead of black; the Habbo
/// CCTs confirm it: the same artwork exists as 8-bit (renders black) and 1-bit
/// (must render identically). So for bpp == 1 we ignore the palette: bit=1
/// paints black (0,0,0), bit=0 paints white (255,255,255).
pub fn decode_to_rgba(
    data: &[u8],
    width: usize,
    height: usize,
    bpp: u8,
    pitch: usize,
    palette: &[(u8, u8, u8)],
    director_version: u16,
) -> Result<Vec<u8>, ParseError> {
    match bpp {
        1 => {
            let indices = decompress_rle(data, width, height, bpp, pitch)?;
            let mut rgba = Vec::with_capacity(width * height * 4);
            for idx in indices {
                if idx != 0 {
                    rgba.extend_from_slice(&[0, 0, 0, 255]);
                } else {
                    rgba.extend_from_slice(&[255, 255, 255, 255]);
                }
            }
            Ok(rgba)
        }
        2 | 4 | 8 => {
            let indices = decompress_rle(data, width, height, bpp, pitch)?;
            // LibreShockwave BitmapDecoder scales low-depth indices to their
            // System-palette positions before the palette lookup: 2-bit values
            // 0..3 -> 0/85/170/255 and 4-bit 0..15 -> 0/17/.../255 (round(
            // value/max*255)). Classic Director authored these against the Mac
            // system palette, whose 2-bit gray levels live at 0/85/170/255 and
            // 4-bit at multiples of 17 — so raw index 3 of a SystemMac member
            // resolves to palette[255] (black), NOT palette[3] (yellow).
            // Without this, e.g. hh_interface's info_stand_txt_bg and the pet
            // shadow masks render yellow instead of black. 8-bit stays raw.
            let indices: Vec<u8> = if bpp == 2 || bpp == 4 {
                let max = if bpp == 2 { 3u32 } else { 15u32 };
                indices
                    .iter()
                    .map(|&v| ((v as u32 * 255 + max / 2) / max) as u8)
                    .collect()
            } else {
                indices
            };
            Ok(apply_palette(&indices, palette, width, height))
        }
        16 => decode_16bit(data, width, height, pitch),
        32 => decode_32bit(data, width, height, pitch, director_version),
        _ => Err(ParseError::InvalidData(format!("unsupported bpp: {bpp}"))),
    }
}

/// Decode a BITD chunk using the given palette and metadata.
/// Returns RGBA pixels ready for image export.
pub fn decode_bitmap(chunk: &Chunk, info: &BitmapInfo, palette: &[(u8, u8, u8)]) -> Result<DecodedBitmap, ParseError> {
    let data = chunk.data();
    let width = info.width as usize;
    let height = info.height as usize;
    let bpp = info.bits_per_pixel;
    let pitch = if info.pitch > 0 { info.pitch as usize } else {
        // Calculate minimum pitch
        let min_pitch = (width * bpp as usize + 7) / 8;
        // Round up to even
        if min_pitch % 2 == 1 { min_pitch + 1 } else { min_pitch }
    };

    let rgba = decode_to_rgba(data, width, height, bpp, pitch, palette, 0)?;

    Ok(DecodedBitmap {
        width,
        height,
        rgba,
        info: info.clone(),
    })
}

/// Parse a BITD chunk. Returns basic info and raw pixel data.
pub fn read_bitd(chunk: &Chunk) -> Result<BitmapData, ParseError> {
    let data = chunk.data();
    Ok(BitmapData {
        info: BitmapInfo::default(),
        pixel_data: data.to_vec(),
        decoded: None,
    })
}

/// Parse bitmap metadata from a D5+ cast member's specificData (the trailing
/// `dataLen` bytes of the CASt chunk).
///
/// Layout per ScummVM BitmapCastMember (D6-D9) and LibreShockwave BitmapInfo::parse:
///   u16 pitch (rawPitch; 0x8000 bit = color bitmap)
///   s16 top, left, bottom, right        — initialRect (width = right-left, height = bottom-top)
///   u8  alphaThreshold, u8 padding
///   u16 editVersion
///   s16 scrollY, s16 scrollX
///   s16 regY, s16 regX
///   u8  updateFlags
///   if pitch & 0x8000:
///     u8  bitDepth
///     s16 clutCastLib
///     s16 clutId (palette id; <= 0 = builtin)
///
/// Note: a positive clutId is the palette member's CAST NUMBER (index in the
/// CAS* list + DRCF minMember), not a CLUT resource id — resolve via
/// cast::parse_cast_member_list + the KEY* CLUT child. Verified against the
/// Habbo room CCTs (pool stored 53 -> member 53 = pool_palette res 628).
pub fn parse_d7_bitmap_info(data: &[u8]) -> BitmapInfo {
    let mut info = BitmapInfo::default();
    if data.len() < 10 {
        return info;
    }

    let raw_pitch = u16::from_be_bytes([data[0], data[1]]);
    let top = i16::from_be_bytes([data[2], data[3]]);
    let left = i16::from_be_bytes([data[4], data[5]]);
    let bottom = i16::from_be_bytes([data[6], data[7]]);
    let right = i16::from_be_bytes([data[8], data[9]]);

    let width = (right as i32 - left as i32).unsigned_abs() as u16;
    let height = (bottom as i32 - top as i32).unsigned_abs() as u16;
    if width > 0 && width < 10000 {
        info.width = width;
    }
    if height > 0 && height < 10000 {
        info.height = height;
    }

    info.pitch = if raw_pitch & 0x8000 != 0 {
        raw_pitch & 0x3FFF
    } else {
        raw_pitch & 0x0FFF
    };

    // regY/regX at offsets 18/20 (LibreShockwave BitmapInfo::parse reads them
    // unconditionally — including for 1-bit mask members, whose rawPitch has
    // no 0x8000 flag). Verified: pool cursor_arrow_l_mask reg=(8,7) via this
    // read; before, 1-bit members always exported regpoint 0,0.
    if data.len() >= 22 {
        info.reg_y = i16::from_be_bytes([data[18], data[19]]);
        info.reg_x = i16::from_be_bytes([data[20], data[21]]);
    }

    if raw_pitch & 0x8000 != 0 {
        // bitDepth at 23, clutId at 26/27 (palette member CAST NUMBER)
        info.bits_per_pixel = data.get(23).copied().unwrap_or(1);
        info.has_palette = true;
        if data.len() >= 28 {
            info.palette_id = i16::from_be_bytes([data[26], data[27]]);
        }
    } else {
        info.bits_per_pixel = 1;
    }
    if info.bits_per_pixel == 0 {
        info.bits_per_pixel = 1;
    }
    info
}

/// Parse bitmap metadata from a CASt chunk's data section.
pub fn parse_bitmap_info_from_cast(cast_data: &[u8], is_d5: bool, endian: Endian) -> Result<BitmapInfo, ParseError> {
    if cast_data.len() < 16 {
        return Ok(BitmapInfo::default());
    }

    let mut info = BitmapInfo::default();
    let mut pos;

    if !is_d5 {
        // D3/D4 format (starts after 7-byte header: dataSize(2) + infoSize(4) + type(1))
        pos = 7;
        if pos + 16 > cast_data.len() {
            return Ok(info);
        }
        let bytes = read_u16(cast_data, &mut pos, endian);
        let _top = read_s16(cast_data, &mut pos, endian);
        let _left = read_s16(cast_data, &mut pos, endian);
        let _bottom = read_s16(cast_data, &mut pos, endian);
        let _right = read_s16(cast_data, &mut pos, endian);

        // For D4, the pitch is in `bytes & 0x0fff`
        if !is_d5 {
            info.pitch = bytes & 0x0fff;
        }

        info.reg_y = read_s16(cast_data, &mut pos, endian);
        info.reg_x = read_s16(cast_data, &mut pos, endian);

        if bytes & 0x8000 != 0 {
            info.bits_per_pixel = cast_data.get(pos).copied().unwrap_or(1);
            pos += 2; // skip u16
            let palette_id = read_s16(cast_data, &mut pos, endian);
            info.palette_id = palette_id;
            info.has_palette = palette_id != 0 || info.bits_per_pixel > 1;
        } else {
            info.bits_per_pixel = 1;
        }
    }

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use director_rifx::FourCC;
    use crate::cast;

    /// Regression: decode member 4 of hh_people_2.cct through the full pipeline
    /// (Afterburner -> KEY* -> BITD) and check the first scanline.
    /// Reference (verified against a Python decode): first row is 12x white(0),
    /// 13x black(255), 9x white(0).
    /// 1-bit bitmaps are ink/paper (black/white), NOT palette-indexed.
    /// Regression: a 1-bit shadow with palette_id=-101 (SystemWinD5, whose
    /// entry 1 is cyan) must render black/white, never cyan.
    #[test]
    fn decode_1bit_is_black_and_white() {
        // 8 pixels, 1 row: bits 1,0,1,0,1,0,1,0 (0xAA)
        let data = [0xAAu8];
        // SystemWinD5 palette (entry 0 white, entry 1 cyan) would map idx 1
        // to cyan if the palette were wrongly applied.
        let palette = [(255, 255, 255), (0, 255, 255)];
        let rgba = decode_to_rgba(&data, 8, 1, 1, 1, &palette, 0).expect("decode");
        let mut px = rgba.chunks_exact(4);
        let expected = [
            [0, 0, 0, 255],      // bit=1 -> black
            [255, 255, 255, 255], // bit=0 -> white
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
        ];
        for e in &expected {
            assert_eq!(px.next().unwrap(), e);
        }
        assert!(px.next().is_none());
    }

    /// Regression: 2-bit members index the palette at System-palette
    /// positions (0/85/170/255), not raw 0..3. hh_interface member 353
    /// (info_stand_txt_bg) is 2-bit SystemMac: raw index 3 must decode to
    /// SystemMac[255] = black, never SystemMac[3] = yellow.
    #[test]
    fn decode_2bit_scales_indices_to_system_positions() {
        // 4 pixels, 1 row of 2-bit values 0,1,2,3 packed into one byte:
        // 00 01 10 11 = 0b00011011 = 0x1B
        let data = [0x1Bu8];
        let system_mac = crate::clut::system_mac_palette();
        let palette = system_mac.colors;
        let rgba = decode_to_rgba(&data, 4, 1, 2, 1, &palette, 0).expect("decode");
        let mut px = rgba.chunks_exact(4);
        // raw 0 -> palette[0]   (white)
        // raw 1 -> palette[85]  (mid gray)
        // raw 2 -> palette[170] (dark gray)
        // raw 3 -> palette[255] (black)
        let p0 = palette[0];
        let p85 = palette[85];
        let p170 = palette[170];
        let p255 = palette[255];
        let p3 = palette[3];
        assert_eq!(px.next().unwrap(), &[p0.0, p0.1, p0.2, 255]);
        assert_eq!(px.next().unwrap(), &[p85.0, p85.1, p85.2, 255]);
        assert_eq!(px.next().unwrap(), &[p170.0, p170.1, p170.2, 255]);
        assert_eq!(px.next().unwrap(), &[p255.0, p255.1, p255.2, 255]);
        assert!(px.next().is_none());
        // The whole point: SystemMac[255] is black, SystemMac[3] is yellow.
        assert_eq!(p255, (0, 0, 0));
        assert_eq!(p3, palette[3]);
        assert_ne!(p3, (0, 0, 0));
    }

    #[test]
    fn decode_member4_people2() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../director-files/hh_people_2.cct");
        let data = std::fs::read(path).expect("read test file");
        let root = director_rifx::read_bytes(&data).expect("parse");

        let member = root
            .children
            .iter()
            .find(|c| c.fourcc() == FourCC(*b"CASt") && c.source_id == Some(4))
            .expect("member 4");
        let cm = cast::read_cast_member(member).expect("cast member");
        let info_start = 12 + cm.cast_info_size as usize;
        let sd = &cm.raw_data[info_start..info_start + cm.cast_data_size as usize];
        let info = parse_d7_bitmap_info(sd);
        assert_eq!((info.width, info.height), (34, 29));
        assert_eq!(info.bits_per_pixel, 8);
        assert_eq!(info.pitch, 34);

        let bitd = root
            .children
            .iter()
            .find(|c| c.source_id == Some(1174))
            .expect("BITD 1174");
        let indices = decompress_rle(
            bitd.data(),
            info.width as usize,
            info.height as usize,
            info.bits_per_pixel,
            info.pitch as usize,
        )
        .expect("rle");
        assert_eq!(indices.len(), 34 * 29);

        let expected: Vec<u8> = {
            let mut v = Vec::new();
            v.extend(std::iter::repeat(0u8).take(12)); // 12x white
            v.extend(std::iter::repeat(255u8).take(13)); // 13x black
            v.extend(std::iter::repeat(0u8).take(9)); // 9x white
            v
        };
        assert_eq!(&indices[0..34], &expected[..], "first scanline mismatch");
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

fn read_s16(data: &[u8], pos: &mut usize, endian: Endian) -> i16 {
    let v = match endian {
        Endian::Big => i16::from_be_bytes([data[*pos], data[*pos + 1]]),
        Endian::Little => i16::from_le_bytes([data[*pos], data[*pos + 1]]),
    };
    *pos += 2;
    v
}
