//! Sound chunk parser for Director sound resources.
//!
//! Director stores sounds in several formats:
//! - 'SND ' — Standard Macintosh sound resource (System 7 'snd ')
//! - 'snd ' — Same as above, lowercase tag
//! - 'sndH' — Sound header (D6+ MOA format)
//! - 'sndS' — Sound sample data (D6+ MOA format)
//! - 'ediM' — Embedded media (AIFF, WAV, etc.)
//!
//! Reference: ScummVM Director engine sound.cpp, castmember/sound.cpp

use director_rifx::{Chunk, Endian};
use crate::ParseError;

/// Known sound formats in Director.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundFormat {
    MacSnd,       // System 7 'snd ' resource
    Wav,          // WAV audio
    Aiff,         // AIFF audio
    Moa,          // MOA (Director's internal format)
    Unknown,
}

/// Metadata about a sound chunk.
#[derive(Debug, Clone)]
pub struct SoundInfo {
    pub format: SoundFormat,
    pub data_size: u32,
    pub sample_rate: u32,
    pub sample_size: u8,
    pub channels: u8,
    pub looping: bool,
    pub has_loop_bounds: bool,
    pub loop_start: u32,
    pub loop_end: u32,
}

/// A parsed sound chunk.
#[derive(Debug, Clone)]
pub struct SoundData {
    pub info: SoundInfo,
    pub raw_data: Vec<u8>,
}

/// Parse an SND sound chunk.
///
/// Mac 'snd ' resource format (simplified):
///   u16 format          — 1 = standard, 2 = extended
///   u16 dataTypeCount   — number of data type entries
///   For each data type:
///     u32 dataType      — 'snd ' type
///   ...
pub fn read_snd(chunk: &Chunk) -> Result<SoundData, ParseError> {
    let data = chunk.data();
    let endian = chunk.endian;
    if data.len() < 8 {
        return Err(ParseError::InvalidData("SND chunk too small".into()));
    }

    let mut pos = 0usize;

    // Try to detect format
    let format_id = read_u16(data, &mut pos, endian);

    let info = match format_id {
        1 | 2 => {
            // Standard/Extended Mac 'snd ' format
            let data_type_count = read_u16(data, &mut pos, endian);

            // Skip data type entries
            for _ in 0..data_type_count {
                if pos + 4 <= data.len() {
                    let _data_type = read_u32(data, &mut pos, endian);
                }
            }

            // Read sound header
            if format_id == 2 && pos + 16 <= data.len() {
                // Extended format has sample rate, loop info
                let sample_rate = read_u32(data, &mut pos, endian);
                let loop_start = read_u32(data, &mut pos, endian);
                let loop_end = read_u32(data, &mut pos, endian);
                let _encode = read_u16(data, &mut pos, endian);
                let has_loop = loop_end > loop_start;
                let _base_freq = read_u16(data, &mut pos, endian);

                SoundInfo {
                    format: SoundFormat::MacSnd,
                    data_size: data.len() as u32 - pos as u32,
                    sample_rate,
                    sample_size: 8,
                    channels: 1,
                    looping: has_loop,
                    has_loop_bounds: has_loop,
                    loop_start,
                    loop_end,
                }
            } else {
                SoundInfo {
                    format: SoundFormat::MacSnd,
                    data_size: data.len() as u32,
                    sample_rate: 22050,
                    sample_size: 8,
                    channels: 1,
                    looping: false,
                    has_loop_bounds: false,
                    loop_start: 0,
                    loop_end: 0,
                }
            }
        }
        _ => {
            // Unknown format, treat as raw
            SoundInfo {
                format: SoundFormat::Unknown,
                data_size: data.len() as u32,
                sample_rate: 0,
                sample_size: 0,
                channels: 0,
                looping: false,
                has_loop_bounds: false,
                loop_start: 0,
                loop_end: 0,
            }
        }
    };

    Ok(SoundData {
        info,
        raw_data: data.to_vec(),
    })
}

/// Parse an ediM chunk (embedded media like AIFF).
pub fn read_edim(chunk: &Chunk) -> Result<SoundInfo, ParseError> {
    let data = chunk.data();
    if data.len() < 4 {
        return Err(ParseError::InvalidData("ediM chunk too small".into()));
    }

    // Check for AIFF or WAV magic
    let magic = &data[0..4];
    let format = if magic == b"FORM" {
        SoundFormat::Aiff
    } else if magic == b"WAVE" {
        SoundFormat::Wav
    } else {
        SoundFormat::Unknown
    };

    Ok(SoundInfo {
        format,
        data_size: data.len() as u32,
        sample_rate: 0,
        sample_size: 0,
        channels: 0,
        looping: false,
        has_loop_bounds: false,
        loop_start: 0,
        loop_end: 0,
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
