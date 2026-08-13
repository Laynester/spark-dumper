//! CLUT (Color Lookup Table) palette chunk parser.
//!
//! Director stores palettes in CLUT chunks. Each palette entry is 6 bytes
//! in Mac color format: u16 r, u16 g, u16 b (each channel 0-65535).
//!
//! Two layouts exist:
//!   A) Headerless full palette: exactly 256 entries × 6 bytes = 1536 bytes,
//!      starting directly with the colors (used by Afterburner Habbo CCTs).
//!   B) With header: u16 startColor, u16 endColor, then (endColor-startColor+1)
//!      entries × 6 bytes.
//!
//! Reference: LibreShockwave PaletteChunk.cpp (bytesLeft/6 entries, no header),
//! ScummVM Director engine palette handling.

use director_rifx::Chunk;
use crate::ParseError;

/// A color palette with 256 RGB entries.
#[derive(Debug, Clone)]
pub struct Palette {
    pub colors: Vec<(u8, u8, u8)>, // (r, g, b) 0-255
    pub start_color: u16,
    pub end_color: u16,
    pub raw_data: Vec<u8>,
}

/// Convert a Mac-style 16-bit color channel (0-65535) to 8-bit (0-255).
fn mac_to_8bit(v: u16) -> u8 {
    ((v as u32 + 128) / 257) as u8
}

/// Parse a CLUT chunk.
pub fn read_clut(chunk: &Chunk) -> Result<Palette, ParseError> {
    let data = chunk.data();
    if data.len() < 4 {
        return Err(ParseError::InvalidData("CLUT chunk too small".into()));
    }

    let mut pos = 0usize;
    let (start_color, end_color, count) = if data.len() == 256 * 6 {
        // Headerless full palette: 256 entries starting at index 0.
        (0u16, 255u16, 256usize)
    } else if data.len() >= 4 && (data.len() - 4) % 6 == 0 {
        // Standard format with startColor/endColor header.
        let start = read_u16_be(data, &mut pos);
        let end = read_u16_be(data, &mut pos);
        let count = if end >= start {
            (end - start + 1) as usize
        } else {
            256
        };
        (start, end, count)
    } else if data.len() % 6 == 0 {
        // Headerless but not a full 256-entry palette.
        (0u16, (data.len() / 6 - 1) as u16, data.len() / 6)
    } else {
        return Err(ParseError::InvalidData(format!(
            "CLUT chunk size {} not a multiple of 6",
            data.len()
        )));
    };

    let mut colors: Vec<(u8, u8, u8)> = Vec::with_capacity(256);
    for _ in 0..256 {
        colors.push((0u8, 0u8, 0u8));
    }

    for i in 0..count.min(256) {
        if pos + 6 > data.len() {
            break;
        }
        let r = read_u16_be(data, &mut pos);
        let g = read_u16_be(data, &mut pos);
        let b = read_u16_be(data, &mut pos);

        let idx = start_color as usize + i;
        if idx < 256 {
            colors[idx] = (mac_to_8bit(r), mac_to_8bit(g), mac_to_8bit(b));
        }
    }

    Ok(Palette {
        colors,
        start_color,
        end_color,
        raw_data: data.to_vec(),
    })
}

/// Map a bitmap member's stored clutId to the built-in palette it references.
///
/// Director convention (ScummVM BitmapCastMember D6-D9 parse):
/// `if (clutId <= 0) clut = CastMemberID(clutId - 1, -1)` — a stored clutId of
/// 0 or less names a *built-in* system palette with id `clutId - 1`:
///
/// | stored clutId | built-in id | palette           |
/// |---------------|-------------|-------------------|
/// |  0            | -1          | SystemMac         |
/// | -1            | -2          | Rainbow           |
/// | -2            | -3          | Grayscale         |
/// | -3            | -4          | Pastels           |
/// | -4            | -5          | Vivid             |
/// | -5            | -6          | NTSC              |
/// | -6            | -7          | Metallic          |
/// | -101          | -102        | SystemWinD5       |
///
/// (SystemWin itself is -101; the D5 Windows palette is -102. Stored -101 is
/// what the Habbo pool CCTs use.)
pub fn builtin_palette_for_clut_id(clut_id: i16) -> Option<Palette> {
    if clut_id > 0 {
        return None;
    }
    builtin_palette(clut_id as i32 - 1)
}

/// Return a built-in Director palette by ScummVM PaletteType id
/// (see the table in `builtin_palette_for_clut_id`).
pub fn builtin_palette(builtin_id: i32) -> Option<Palette> {
    let raw: &[u8; 768] = match builtin_id {
        -1 => &crate::palette_data::SYSTEM_MAC_RAW,
        -2 => &crate::palette_data::RAINBOW_RAW,
        -3 => &crate::palette_data::GRAYSCALE_RAW,
        -4 => &crate::palette_data::PASTELS_RAW,
        -5 => &crate::palette_data::VIVID_RAW,
        -6 => &crate::palette_data::NTSC_RAW,
        -7 => &crate::palette_data::METALLIC_RAW,
        -101 => &crate::palette_data::SYSTEM_WIN_RAW,
        -102 => &crate::palette_data::SYSTEM_WIN_D5_RAW,
        _ => return None,
    };
    let colors = raw.chunks_exact(3).map(|c| (c[0], c[1], c[2])).collect();
    Some(Palette {
        colors,
        start_color: 0,
        end_color: 255,
        raw_data: Vec::new(),
    })
}

/// Return the built-in Mac system palette (the default when no CLUT is specified).
pub fn system_mac_palette() -> Palette {
    builtin_palette(-1).expect("SystemMac palette is embedded")
}

/// LibreShockwave `paletteFromChunk` tail fill: palettes resolved through the
/// KEY*-chain (member-number, chunk-id, palette-member-index) have black
/// entries at indices 246-254 replaced with the SystemMac color at the same
/// index. The unlinked-mac-like fallback does NOT apply this fill (LS builds
/// that palette directly from the raw chunk colors), so it is applied only at
/// chain resolution sites, not in read_clut itself.
pub fn mac_like_tail_fill(colors: &[(u8, u8, u8)]) -> Vec<(u8, u8, u8)> {
    if colors.len() != 256 {
        return colors.to_vec();
    }
    let mac = system_mac_palette().colors;
    let mut out = colors.to_vec();
    for i in 246..=254 {
        if i < out.len() && out[i] == (0, 0, 0) && i < mac.len() && mac[i] != (0, 0, 0) {
            out[i] = mac[i];
        }
    }
    out
}

fn read_u16_be(data: &[u8], pos: &mut usize) -> u16 {
    let v = u16::from_be_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    v
}
