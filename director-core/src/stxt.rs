//! STXT (Styled Text) chunk parser.
//!
//! The STXT chunk contains stylized text with font/color formatting runs.
//!
//! Format (big-endian):
//!   u32 offset          — should be 12
//!   u32 strLen          — length of the text string
//!   u32 dataLen         — total length of data
//!   [u8; strLen]        — raw text bytes (MacRoman encoding)
//!   u16 formattingCount — number of formatting runs
//!   For each formatting run:
//!     u32 formatStartOffset
//!     u16 height
//!     u16 ascent
//!     u16 fontId
//!     u8  textSlant
//!     u8  padding
//!     u16 fontSize
//!     u16 r, g, b       — text color

use director_rifx::{Chunk, Endian};
use crate::ParseError;

/// A styled text run with font formatting.
#[derive(Debug, Clone)]
pub struct TextFormatRun {
    pub start_offset: u32,
    pub font_id: u16,
    pub font_size: u16,
    pub text_slant: u8,
    pub height: u16,
    pub ascent: u16,
    pub color: (u16, u16, u16), // (r, g, b)
}

/// A parsed STXT (styled text) chunk.
#[derive(Debug, Clone)]
pub struct StyledText {
    pub text: String,
    pub formatting: Vec<TextFormatRun>,
    pub raw_data: Vec<u8>,
}

/// Parse an STXT chunk.
pub fn read_stxt(chunk: &Chunk) -> Result<StyledText, ParseError> {
    let data = chunk.data();
    let endian = chunk.endian;
    if data.len() < 16 {
        return Err(ParseError::InvalidData("STXT chunk too small".into()));
    }

    let mut pos = 0;

    let offset = read_u32(data, &mut pos, endian);
    if offset != 12 {
        // Non-standard offset, treat entire chunk as raw data
        return Ok(StyledText {
            text: String::new(),
            formatting: vec![],
            raw_data: data.to_vec(),
        });
    }

    let str_len = read_u32(data, &mut pos, endian) as usize;
    let _data_len = read_u32(data, &mut pos, endian);

    // Read text
    let text_bytes = if str_len > 0 && pos + str_len <= data.len() {
        let bytes = &data[pos..pos + str_len];
        pos += str_len;
        bytes.to_vec()
    } else {
        Vec::new()
    };

    // Convert MacRoman-ish bytes to a string
    let text = String::from_utf8_lossy(&text_bytes).to_string();

    // Read formatting runs
    let mut formatting = Vec::new();
    if pos + 2 <= data.len() {
        let fmt_count = read_u16(data, &mut pos, endian) as usize;
        for _ in 0..fmt_count {
            if pos + 20 > data.len() {
                break;
            }
            let start_offset = read_u32(data, &mut pos, endian);
            let height = read_u16(data, &mut pos, endian);
            let ascent = read_u16(data, &mut pos, endian);
            let font_id = read_u16(data, &mut pos, endian);
            let text_slant = data[pos]; pos += 1;
            pos += 1; // padding
            let font_size = read_u16(data, &mut pos, endian);
            let r = read_u16(data, &mut pos, endian);
            let g = read_u16(data, &mut pos, endian);
            let b = read_u16(data, &mut pos, endian);

            formatting.push(TextFormatRun {
                start_offset,
                font_id,
                font_size,
                text_slant,
                height,
                ascent,
                color: (r, g, b),
            });
        }
    }

    Ok(StyledText {
        text,
        formatting,
        raw_data: data.to_vec(),
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
