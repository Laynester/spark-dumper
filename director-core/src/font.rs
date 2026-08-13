//! PFR1 (Portable Font Resource 1) parser and PFR1 → TTF converter.
//!
//! Faithful port of LibreShockwave's `cpp/src/font/PfrBitReader.cpp`,
//! `cpp/src/font/Pfr1Font.cpp`, `cpp/src/font/Pfr1TtfConverter.cpp` and the
//! `FontMapChunk::read` in `cpp/src/chunks/FontMapChunk.cpp`. Habbo CCTs embed
//! fonts as XMED chunks whose payload starts with the "PFR1" magic; the
//! converter turns the outline glyphs into a TrueType font file.
//!
//! Correctness is verified externally: exporting hh_interface.cct produces
//! fonts/0001_Volter_400_0.ttf, fonts/0002_Volter_700_0.ttf and fonts.txt
//! that are byte-identical to LibreShockwave's exports (there are no
//! self-contained PFR1 fixtures for `cargo test`).

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// PfrBitReader
// ---------------------------------------------------------------------------

struct PfrBitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit_buffer: u32,
    bits_left: u32,
}

impl<'a> PfrBitReader<'a> {
    fn new(data: &'a [u8], offset: usize) -> Self {
        PfrBitReader { data, pos: offset, bit_buffer: 0, bits_left: 0 }
    }

    fn position(&self) -> usize {
        self.pos
    }

    fn set_position(&mut self, position: usize) {
        self.pos = position;
        self.bit_buffer = 0;
        self.bits_left = 0;
    }

    fn remaining(&self) -> usize {
        if self.pos >= self.data.len() {
            0
        } else {
            self.data.len() - self.pos
        }
    }

    fn align_to_byte(&mut self) {
        self.bit_buffer = 0;
        self.bits_left = 0;
    }

    fn read_u8(&mut self) -> i32 {
        self.align_to_byte();
        if self.pos >= self.data.len() {
            return 0;
        }
        let v = self.data[self.pos] & 0xFF;
        self.pos += 1;
        v as i32
    }

    #[allow(dead_code)]
    fn read_i8(&mut self) -> i32 {
        sign8(self.read_u8())
    }

    fn read_u16(&mut self) -> i32 {
        let hi = self.read_u8();
        let lo = self.read_u8();
        (hi << 8) | lo
    }

    #[allow(dead_code)]
    fn read_i16(&mut self) -> i32 {
        let value = self.read_u16();
        if value & 0x8000 != 0 {
            value | !0xFFFF
        } else {
            value
        }
    }

    fn read_u24(&mut self) -> i32 {
        let b0 = self.read_u8();
        let b1 = self.read_u8();
        let b2 = self.read_u8();
        (b0 << 16) | (b1 << 8) | b2
    }

    fn read_i24(&mut self) -> i32 {
        let value = self.read_u24();
        if value & 0x800000 != 0 {
            value | !0xFFFFFF
        } else {
            value
        }
    }

    fn skip(&mut self, count: usize) {
        self.align_to_byte();
        self.pos = (self.pos + count).min(self.data.len());
    }

    fn read_bits(&mut self, count: i32) -> i32 {
        if count == 0 {
            return 0;
        }
        let mut result: i32 = 0;
        let mut remaining = count;
        while remaining > 0 {
            if self.bits_left == 0 {
                if self.pos >= self.data.len() {
                    return result;
                }
                self.bit_buffer = self.data[self.pos] as u32 & 0xFF;
                self.pos += 1;
                self.bits_left = 8;
            }
            let take = remaining.min(self.bits_left as i32);
            let shift = self.bits_left - take as u32;
            let mask = (((1u32 << take) - 1) << shift) as u32;
            let bits = (self.bit_buffer & mask) >> shift;
            result = (result << take) | bits as i32;
            self.bits_left -= take as u32;
            remaining -= take;
        }
        result
    }

    fn read_bit(&mut self) -> bool {
        self.read_bits(1) != 0
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Sign-extend a 16-bit value.
fn sign16(value: i32) -> i32 {
    if value & 0x8000 != 0 {
        value | !0xFFFF
    } else {
        value
    }
}

/// Sign-extend an 8-bit value.
fn sign8(value: i32) -> i32 {
    let v = value & 0xFF;
    if v & 0x80 != 0 {
        v | !0xFF
    } else {
        v
    }
}

fn read_unsigned_n(data: &[u8], pos: usize, n: usize) -> i32 {
    let mut value: i32 = 0;
    for i in 0..n {
        if pos + i >= data.len() {
            break;
        }
        value = (value << 8) | (data[pos + i] as i32 & 0xFF);
    }
    value
}

fn read_signed_n(data: &[u8], pos: usize, n: usize) -> i32 {
    let value = read_unsigned_n(data, pos, n);
    if n == 0 {
        return 0;
    }
    // C++ `1 << (n*8 - 1)` is UB at n=4; x86 masks the shift count, replicate
    // that via wrapping_shl.
    let sign_bit = 1i32.wrapping_shl((n * 8 - 1) as u32);
    if value & sign_bit != 0 {
        // C++ `(-1) << (n*8)` is also UB at n=4; x86 masks to 0 → -1.
        let mask = (-1i32).wrapping_shl((n * 8) as u32);
        value | mask
    } else {
        value
    }
}

fn decode_rle_bitmap(data: &[u8], offset: usize, data_len: usize, width: usize, height: usize) -> Vec<u8> {
    let total_bits = width * height;
    if total_bits == 0 || total_bits > 1_000_000 {
        return Vec::new();
    }
    let total_bytes = (total_bits + 7) / 8;
    let mut result = vec![0u8; total_bytes];
    let mut out_pos: usize = 0;
    let mut pos = offset;
    let end = offset + data_len;

    while pos < end && pos < data.len() && out_pos < total_bits {
        let byte = data[pos] & 0xFF;
        pos += 1;
        let count = ((byte >> 4) & 0x0F) as usize;
        let value = (byte & 0x0F) as usize;
        for _ in 0..count {
            if out_pos >= total_bits {
                break;
            }
            if value != 0 {
                result[out_pos / 8] |= 1u8 << (7 - (out_pos % 8));
            }
            out_pos += 1;
        }
    }

    result
}

/// `orusLookup`: find the control value at `current + direction` steps past
/// the first control value strictly above `current` (or below for negatives).
fn orus_lookup(control_values: &[i32], current: i32, direction: i32) -> i32 {
    if control_values.is_empty() || direction == 0 {
        return current;
    }

    if direction > 0 {
        let mut pos = 0usize;
        while pos < control_values.len() && control_values[pos] <= current {
            pos += 1;
        }
        if pos >= control_values.len() {
            return current;
        }
        let target = (pos + (direction as usize - 1)).min(control_values.len() - 1);
        return control_values[target];
    }

    for i in (0..control_values.len()).rev() {
        if control_values[i] < current {
            let idx = (i as i32 + direction + 1).max(0) as usize;
            return control_values[idx.min(control_values.len() - 1)];
        }
    }
    current
}

// ---------------------------------------------------------------------------
// PFR1 glyph data parsing
// ---------------------------------------------------------------------------

const CURVE_TABLE_9: [u32; 16] = [
    0x0451, 0x0452, 0x0461, 0x0462, 0x0491, 0x0492, 0x04A1, 0x04A2,
    0x0851, 0x0852, 0x0861, 0x0862, 0x0891, 0x0892, 0x08A1, 0x08A2,
];

const CURVE_TABLE_10: [u32; 16] = [
    0x0154, 0x0158, 0x0164, 0x0168, 0x0194, 0x0198, 0x01A4, 0x01A8,
    0x0254, 0x0258, 0x0264, 0x0268, 0x0294, 0x0298, 0x02A4, 0x02A8,
];

const CURVE_TABLE_13: [u32; 16] = [
    0x0FFF, 0x03AA, 0x0CAA, 0x0AA3, 0x0AAC, 0x0AAA, 0x02AA, 0x08AA,
    0x0AA2, 0x0AA8, 0x00AA, 0x0555, 0x0155, 0x0455, 0x0551, 0x0554,
];

const CURVE_TABLE_14A: [u32; 8] = [1, 2, 4, 5, 6, 8, 9, 10];
const CURVE_TABLE_14B: [u32; 4] = [5, 6, 9, 10];

#[derive(Default)]
struct CoordRead {
    value: i32,
    pos: usize,
    nibble_high: bool,
}

#[derive(Default)]
struct CoordPairRead {
    x: i32,
    y: i32,
    pos: usize,
    nibble_high: bool,
}

#[derive(Default)]
struct TransformRead {
    scale: i32,
    offset: i32,
    pos: usize,
}

#[derive(Default)]
struct GlyphOffsetRead {
    offset: i32,
    size: i32,
    pos: usize,
    accumulator: i32,
}

struct ByteRead {
    value: i32,
    pos: usize,
}

struct NibbleRead {
    value: i32,
    pos: usize,
    nibble_high: bool,
}

/// Read one byte-aligned value. `nibble_high` marks that the next byte's high
/// nibble holds data (PFR uses 12-bit packing).
fn read_byte_aligned(data: &[u8], mut pos: usize, nibble_high: bool) -> ByteRead {
    if pos >= data.len() {
        return ByteRead { value: 0, pos };
    }
    if nibble_high {
        let lo = (data[pos] & 0x0F) as i32;
        pos += 1;
        let hi = if pos < data.len() { ((data[pos] >> 4) & 0x0F) as i32 } else { 0 };
        return ByteRead { value: (lo << 4) | hi, pos };
    }
    ByteRead { value: data[pos] as i32 & 0xFF, pos: pos + 1 }
}

/// Read a 4-bit nibble, toggling between high/low nibbles across calls.
fn read_nibble(data: &[u8], pos: usize, nibble_high: bool) -> NibbleRead {
    if pos >= data.len() {
        return NibbleRead { value: -1, pos, nibble_high };
    }
    let nibble_high = !nibble_high;
    if nibble_high {
        return NibbleRead { value: ((data[pos] >> 4) & 0x0F) as i32, pos, nibble_high };
    }
    NibbleRead { value: (data[pos] & 0x0F) as i32, pos: pos + 1, nibble_high }
}

/// PFR coordinate decoding (LS readCe9dCoordValue).
fn read_ce9d_coord_value(
    data: &[u8],
    mut pos: usize,
    v8: i32,
    three_byte_mode: bool,
    nibble_aligned: bool,
) -> CoordRead {
    if pos >= data.len() {
        return CoordRead { value: 0, pos, nibble_high: nibble_aligned };
    }

    if v8 & 1 == 0 {
        let result;
        if nibble_aligned {
            let lo = (data[pos] & 0x0F) as i32;
            pos += 1;
            let hi = if pos < data.len() { ((data[pos] >> 4) & 0x0Fu8) as i32 } else { 0 };
            result = sign16((lo << 4) | hi);
        } else {
            result = sign16(data[pos] as i32 & 0xFF);
            pos += 1;
        }
        return CoordRead { value: result, pos, nibble_high: nibble_aligned };
    }

    if three_byte_mode {
        let result;
        if nibble_aligned {
            let b0_low = if pos > 0 { (data[pos - 1] & 0x0F) as i32 } else { 0 };
            let b1 = if pos < data.len() { data[pos] as i32 & 0xFF } else { 0 };
            pos += 1;
            let b2_high = if pos < data.len() { ((data[pos] >> 4) & 0x0Fu8) as i32 } else { 0 };
            result = sign16((b0_low << 12) | (b1 << 4) | b2_high);
        } else {
            let b0 = if pos < data.len() { data[pos] as i32 & 0xFF } else { 0 };
            pos += 1;
            let b1 = if pos < data.len() { data[pos] as i32 & 0xFF } else { 0 };
            pos += 1;
            result = sign16((b0 << 8) | b1);
        }
        return CoordRead { value: result, pos, nibble_high: nibble_aligned };
    }

    if nibble_aligned {
        let lo = (data[pos] & 0x0F) as i32;
        pos += 1;
        let next_byte = if pos < data.len() { data[pos] as i32 & 0xFF } else { 0 };
        pos += 1;
        return CoordRead { value: sign16(next_byte + 16 * sign8(lo << 4)), pos, nibble_high: false };
    }
    let signed_byte = sign8(data[pos] as i32);
    pos += 1;
    let next_high = if pos < data.len() { ((data[pos] >> 4) & 0x0Fu8) as i32 } else { 0 };
    CoordRead { value: sign16(next_high + 16 * signed_byte), pos, nibble_high: true }
}

/// Decode the ORUS control point arrays (LS parseControlValues).
fn parse_control_values(
    data: &[u8],
    mut pos: usize,
    flags: i32,
    control_x: &mut [i32],
    control_y: &mut [i32],
) -> usize {
    if control_x.is_empty() && control_y.is_empty() {
        return pos;
    }

    let three_byte_mode = (flags & 3) == 3;
    let flag_per_coordinate = (flags & 0x40) != 0;
    let mut nibble_aligned = false;
    let mut flag_cache: i32 = 0;
    let mut flag_cache_count: i32 = 0;

    let mut accumulated_x: i32 = 0;
    for i in 0..control_x.len() {
        if pos >= data.len() {
            break;
        }
        let mut v8 = 0;
        if i == 0 {
            v8 = (flags >> 4) & 1;
        } else if flag_per_coordinate {
            if flag_cache_count > 0 {
                v8 = (flag_cache >> 1) & 1;
                flag_cache >>= 1;
                flag_cache_count -= 1;
            } else if pos < data.len() {
                let nibble_value;
                if nibble_aligned {
                    nibble_value = (data[pos] & 0x0F) as i32;
                    pos += 1;
                    nibble_aligned = false;
                } else {
                    nibble_value = ((data[pos] >> 4) & 0x0F) as i32;
                    nibble_aligned = true;
                }
                v8 = nibble_value & 1;
                flag_cache = nibble_value;
                flag_cache_count = 3;
            }
        }
        let read = read_ce9d_coord_value(data, pos, v8, three_byte_mode, nibble_aligned);
        pos = read.pos;
        nibble_aligned = read.nibble_high;
        accumulated_x += read.value;
        control_x[i] = accumulated_x;
    }

    let mut accumulated_y: i32 = 0;
    for i in 0..control_y.len() {
        if pos >= data.len() {
            break;
        }
        let mut v8 = 0;
        if i == 0 {
            v8 = (flags >> 5) & 1;
        } else if flag_per_coordinate {
            if flag_cache_count > 0 {
                v8 = (flag_cache >> 1) & 1;
                flag_cache >>= 1;
                flag_cache_count -= 1;
            } else if pos < data.len() {
                let nibble_value;
                if nibble_aligned {
                    nibble_value = (data[pos] & 0x0F) as i32;
                    pos += 1;
                    nibble_aligned = false;
                } else {
                    nibble_value = ((data[pos] >> 4) & 0x0F) as i32;
                    nibble_aligned = true;
                }
                v8 = nibble_value & 1;
                flag_cache = nibble_value;
                flag_cache_count = 3;
            }
        }
        let read = read_ce9d_coord_value(data, pos, v8, three_byte_mode, nibble_aligned);
        pos = read.pos;
        nibble_aligned = read.nibble_high;
        accumulated_y += read.value;
        control_y[i] = accumulated_y;
    }

    if nibble_aligned {
        pos += 1;
    }
    pos
}

/// Decode one coordinate under the given encoding (LS readEncodedCoordValue).
fn read_encoded_coord_value(
    data: &[u8],
    mut pos: usize,
    encoding: i32,
    mut nibble_high: bool,
    current: i32,
    control_values: &[i32],
) -> CoordRead {
    match encoding {
        1 => {
            nibble_high = !nibble_high;
            if pos >= data.len() {
                return CoordRead { value: current, pos, nibble_high };
            }
            let nibble = if nibble_high {
                (data[pos] >> 4) & 0x0F
            } else {
                let v = data[pos] & 0x0F;
                pos += 1;
                v
            };
            CoordRead { value: sign16(current + nibble as i32 - 8), pos, nibble_high }
        }
        2 => {
            let read = read_byte_aligned(data, pos, nibble_high);
            let signed_byte = sign8(read.value);
            if signed_byte >= -8 && signed_byte < 8 {
                let direction = if signed_byte & 0x80 == 0 { signed_byte + 1 } else { signed_byte };
                return CoordRead { value: orus_lookup(control_values, current, direction), pos: read.pos, nibble_high };
            }
            CoordRead { value: sign16(current + signed_byte), pos: read.pos, nibble_high }
        }
        3 => {
            let high_read = read_byte_aligned(data, pos, nibble_high);
            pos = high_read.pos;
            nibble_high = !nibble_high;
            let low = if pos >= data.len() {
                0
            } else if nibble_high {
                (data[pos] >> 4) & 0x0F
            } else {
                let v = data[pos] & 0x0F;
                pos += 1;
                v
            };

            let delta12 = (sign8(high_read.value) << 4) | low as i32;
            if delta12 >= -128 && delta12 < 128 {
                let extra_read = read_byte_aligned(data, pos, nibble_high);
                let delta16 = (delta12 << 8) | (extra_read.value & 0xFF);
                return CoordRead { value: sign16(current + delta16), pos: extra_read.pos, nibble_high };
            }
            CoordRead { value: sign16(current + delta12), pos, nibble_high }
        }
        _ => CoordRead { value: current, pos, nibble_high },
    }
}

/// Decode an (x, y) pair, updating the running current/previous coordinates
/// (LS readEncodedCoordsInto).
#[allow(clippy::too_many_arguments)]
fn read_encoded_coords_into(
    data: &[u8],
    pos: usize,
    encoding: i32,
    current_x: &mut i32,
    current_y: &mut i32,
    previous_x: &mut i32,
    previous_y: &mut i32,
    out_x: i32,
    out_y: i32,
    nibble_high: bool,
    control_x: &[i32],
    control_y: &[i32],
) -> CoordPairRead {
    let x_encoding = encoding & 3;
    let y_encoding = (encoding >> 2) & 3;
    let mut pos = pos;
    let mut nibble_high = nibble_high;
    let mut out_x = out_x;
    let mut out_y = out_y;

    if x_encoding != 0 {
        let read = read_encoded_coord_value(data, pos, x_encoding, nibble_high, *current_x, control_x);
        out_x = read.value;
        pos = read.pos;
        nibble_high = read.nibble_high;
    }
    *previous_x = *current_x;
    *current_x = out_x;

    if y_encoding != 0 {
        let read = read_encoded_coord_value(data, pos, y_encoding, nibble_high, *current_y, control_y);
        out_y = read.value;
        pos = read.pos;
        nibble_high = read.nibble_high;
    }
    *previous_y = *current_y;
    *current_y = out_y;

    CoordPairRead { x: out_x, y: out_y, pos, nibble_high }
}

fn calculate_curve_encoding_14(byte: u32) -> u32 {
    let value = CURVE_TABLE_14B[((byte >> 3) & 0x03) as usize] + 16 * CURVE_TABLE_14A[((byte >> 5) & 0x07) as usize];
    CURVE_TABLE_14A[(byte & 0x07) as usize] + value * 16
}

/// Parse the nibble-coded drawing commands of a simple outline glyph
/// (LS parseNibbleCommands).
fn parse_nibble_commands(
    data: &[u8],
    start_pos: usize,
    end_limit: usize,
    glyph: &mut OutlineGlyph,
    control_x: &[i32],
    control_y: &[i32],
) {
    let mut pos = start_pos;
    let end_pos = if end_limit > 0 { end_limit - 1 } else { 0 };
    let mut nibble_high = false;
    let mut current_x: i32 = 0;
    let mut current_y: i32 = 0;
    let mut previous_x: i32 = 0;
    let mut previous_y: i32 = 0;
    let mut current_contour = Contour::new();
    let mut first_iteration = true;

    for _ in 0..500 {
        if pos >= end_pos && (pos != end_pos || nibble_high) {
            break;
        }
        if pos >= data.len() {
            break;
        }

        let command;
        if first_iteration {
            command = 6;
            first_iteration = false;
        } else {
            nibble_high = !nibble_high;
            if nibble_high {
                command = (data[pos] >> 4) & 0x0F;
            } else {
                command = data[pos] & 0x0F;
                pos += 1;
            }
        }

        let before_x = current_x;
        let before_y = current_y;

        match command {
            0 => {
                let nibble_read = read_nibble(data, pos, nibble_high);
                if nibble_read.value < 0 {
                    // break out of switch
                } else {
                    let nibble = nibble_read.value;
                    pos = nibble_read.pos;
                    nibble_high = nibble_read.nibble_high;
                    let direction = if nibble & 4 != 0 { (nibble & 7) - 8 } else { (nibble & 7) + 1 };
                    previous_x = before_x;
                    previous_y = before_y;
                    if nibble & 8 != 0 {
                        current_y = orus_lookup(control_y, current_y, direction);
                    } else {
                        current_x = orus_lookup(control_x, current_x, direction);
                    }
                    current_contour.line_to(current_x as f32, current_y as f32);
                }
            }
            1 => {
                let read = read_byte_aligned(data, pos, nibble_high);
                pos = read.pos;
                previous_x = before_x;
                previous_y = before_y;
                current_x = sign16(current_x + sign8(read.value));
                current_contour.line_to(current_x as f32, current_y as f32);
            }
            2 => {
                let read = read_byte_aligned(data, pos, nibble_high);
                pos = read.pos;
                previous_x = before_x;
                previous_y = before_y;
                current_y = sign16(current_y + sign8(read.value));
                current_contour.line_to(current_x as f32, current_y as f32);
            }
            3 | 4 => {
                let high_read = read_byte_aligned(data, pos, nibble_high);
                pos = high_read.pos;
                nibble_high = !nibble_high;
                let low = if pos >= data.len() {
                    0
                } else if nibble_high {
                    (data[pos] >> 4) & 0x0F
                } else {
                    let v = data[pos] & 0x0F;
                    pos += 1;
                    v
                };

                let delta12 = (sign8(high_read.value) << 4) | low as i32;
                let mut delta = delta12;
                if delta12 >= -128 && delta12 < 128 {
                    let extra_read = read_byte_aligned(data, pos, nibble_high);
                    pos = extra_read.pos;
                    delta = (delta12 << 8) | (extra_read.value & 0xFF);
                }
                if command == 3 {
                    current_x += sign16(delta);
                } else {
                    current_y += sign16(delta);
                }
                previous_x = before_x;
                previous_y = before_y;
                current_contour.line_to(current_x as f32, current_y as f32);
            }
            5 | 6 => {
                let encoding_read = read_nibble(data, pos, nibble_high);
                if encoding_read.value >= 0 {
                    pos = encoding_read.pos;
                    nibble_high = encoding_read.nibble_high;
                    let out_x = current_x;
                    let out_y = current_y;
                    let read = read_encoded_coords_into(
                        data,
                        pos,
                        encoding_read.value,
                        &mut current_x,
                        &mut current_y,
                        &mut previous_x,
                        &mut previous_y,
                        out_x,
                        out_y,
                        nibble_high,
                        control_x,
                        control_y,
                    );
                    pos = read.pos;
                    nibble_high = read.nibble_high;

                    if command == 6 {
                        if !current_contour.commands.is_empty() {
                            glyph.contours.push(std::mem::take(&mut current_contour));
                            current_contour = Contour::new();
                        }
                        current_contour.move_to(current_x as f32, current_y as f32);
                    } else {
                        current_contour.line_to(current_x as f32, current_y as f32);
                    }
                }
            }
            _ => {
                // command >= 7: curve segments.
                if command >= 7 {
                    let mut encoding: u32 = 0;
                    let mut path: i32 = 0;
                    match command {
                        7 => {
                            encoding = 2210;
                            path = 49;
                        }
                        8 => {
                            encoding = 680;
                            path = 54;
                        }
                        9 => {
                            let nibble_read = read_nibble(data, pos, nibble_high);
                            if nibble_read.value >= 0 {
                                pos = nibble_read.pos;
                                nibble_high = nibble_read.nibble_high;
                                encoding = CURVE_TABLE_9[(nibble_read.value & 0x0F) as usize];
                                path = 49;
                            }
                        }
                        10 => {
                            let nibble_read = read_nibble(data, pos, nibble_high);
                            if nibble_read.value >= 0 {
                                pos = nibble_read.pos;
                                nibble_high = nibble_read.nibble_high;
                                encoding = CURVE_TABLE_10[(nibble_read.value & 0x0F) as usize];
                                path = 54;
                            }
                        }
                        11 => {
                            let byte_read = read_byte_aligned(data, pos, nibble_high);
                            pos = byte_read.pos;
                            let byte = byte_read.value as u32;
                            encoding = (byte & 3) + 4 * ((byte & 0x3C) + 4 * (byte & 0xC0));
                            path = 49;
                        }
                        12 => {
                            let byte_read = read_byte_aligned(data, pos, nibble_high);
                            pos = byte_read.pos;
                            encoding = byte_read.value as u32 * 4;
                            path = 54;
                        }
                        13 => {
                            let nibble_read = read_nibble(data, pos, nibble_high);
                            if nibble_read.value >= 0 {
                                pos = nibble_read.pos;
                                nibble_high = nibble_read.nibble_high;
                                encoding = CURVE_TABLE_13[(nibble_read.value & 0x0F) as usize];
                                path = 70;
                            }
                        }
                        14 => {
                            let byte_read = read_byte_aligned(data, pos, nibble_high);
                            pos = byte_read.pos;
                            encoding = calculate_curve_encoding_14(byte_read.value as u32);
                            path = 70;
                        }
                        15 => {
                            let nibble_read = read_nibble(data, pos, nibble_high);
                            if nibble_read.value >= 0 {
                                pos = nibble_read.pos;
                                nibble_high = nibble_read.nibble_high;
                                let byte_read = read_byte_aligned(data, pos, nibble_high);
                                pos = byte_read.pos;
                                encoding = byte_read.value as u32 | ((nibble_read.value as u32) << 8);
                                path = 70;
                            }
                        }
                        _ => {}
                    }

                    if path != 0 {
                        let start_x = before_x;
                        let start_y = before_y;
                        let mut control1_x = current_x;
                        let mut control1_y = current_y;
                        let mut control2_x: i32;
                        let mut control2_y: i32;
                        let mut end_x: i32;
                        let mut end_y: i32;

                        if path == 49 {
                            let mut read = read_encoded_coords_into(
                                data,
                                pos,
                                (encoding & 0x0F) as i32,
                                &mut current_x,
                                &mut current_y,
                                &mut previous_x,
                                &mut previous_y,
                                control1_x,
                                control1_y,
                                nibble_high,
                                control_x,
                                control_y,
                            );
                            control1_x = read.x;
                            control1_y = read.y;
                            pos = read.pos;
                            nibble_high = read.nibble_high;

                            control2_x = orus_lookup(control_x, current_x, 0);
                            control2_y = control1_y;
                            read = read_encoded_coords_into(
                                data,
                                pos,
                                ((encoding >> 4) & 0x0F) as i32,
                                &mut current_x,
                                &mut current_y,
                                &mut previous_x,
                                &mut previous_y,
                                control2_x,
                                control2_y,
                                nibble_high,
                                control_x,
                                control_y,
                            );
                            control2_x = read.x;
                            control2_y = read.y;
                            pos = read.pos;
                            nibble_high = read.nibble_high;

                            end_x = control2_x;
                            end_y = orus_lookup(control_y, current_y, 0);
                            read = read_encoded_coords_into(
                                data,
                                pos,
                                ((encoding >> 8) & 0x0F) as i32,
                                &mut current_x,
                                &mut current_y,
                                &mut previous_x,
                                &mut previous_y,
                                end_x,
                                end_y,
                                nibble_high,
                                control_x,
                                control_y,
                            );
                            end_x = read.x;
                            end_y = read.y;
                            pos = read.pos;
                            nibble_high = read.nibble_high;
                        } else if path == 54 {
                            let mut read = read_encoded_coords_into(
                                data,
                                pos,
                                (encoding & 0x0F) as i32,
                                &mut current_x,
                                &mut current_y,
                                &mut previous_x,
                                &mut previous_y,
                                control1_x,
                                control1_y,
                                nibble_high,
                                control_x,
                                control_y,
                            );
                            control1_x = read.x;
                            control1_y = read.y;
                            pos = read.pos;
                            nibble_high = read.nibble_high;

                            control2_x = control1_x;
                            control2_y = orus_lookup(control_y, current_y, 0);
                            read = read_encoded_coords_into(
                                data,
                                pos,
                                ((encoding >> 4) & 0x0F) as i32,
                                &mut current_x,
                                &mut current_y,
                                &mut previous_x,
                                &mut previous_y,
                                control2_x,
                                control2_y,
                                nibble_high,
                                control_x,
                                control_y,
                            );
                            control2_x = read.x;
                            control2_y = read.y;
                            pos = read.pos;
                            nibble_high = read.nibble_high;

                            end_x = orus_lookup(control_x, current_x, 0);
                            end_y = control2_y;
                            read = read_encoded_coords_into(
                                data,
                                pos,
                                ((encoding >> 8) & 0x0F) as i32,
                                &mut current_x,
                                &mut current_y,
                                &mut previous_x,
                                &mut previous_y,
                                end_x,
                                end_y,
                                nibble_high,
                                control_x,
                                control_y,
                            );
                            end_x = read.x;
                            end_y = read.y;
                            pos = read.pos;
                            nibble_high = read.nibble_high;
                        } else {
                            control1_x = sign16(current_x + (current_x - previous_x));
                            control1_y = sign16(current_y + (current_y - previous_y));
                            let mut read = read_encoded_coords_into(
                                data,
                                pos,
                                (encoding & 0x0F) as i32,
                                &mut current_x,
                                &mut current_y,
                                &mut previous_x,
                                &mut previous_y,
                                control1_x,
                                control1_y,
                                nibble_high,
                                control_x,
                                control_y,
                            );
                            control1_x = read.x;
                            control1_y = read.y;
                            pos = read.pos;
                            nibble_high = read.nibble_high;

                            control2_x = control1_x;
                            control2_y = control1_y;
                            read = read_encoded_coords_into(
                                data,
                                pos,
                                ((encoding >> 4) & 0x0F) as i32,
                                &mut current_x,
                                &mut current_y,
                                &mut previous_x,
                                &mut previous_y,
                                control2_x,
                                control2_y,
                                nibble_high,
                                control_x,
                                control_y,
                            );
                            control2_x = read.x;
                            control2_y = read.y;
                            pos = read.pos;
                            nibble_high = read.nibble_high;

                            end_x = control2_x;
                            end_y = control2_y;
                            read = read_encoded_coords_into(
                                data,
                                pos,
                                ((encoding >> 8) & 0x0F) as i32,
                                &mut current_x,
                                &mut current_y,
                                &mut previous_x,
                                &mut previous_y,
                                end_x,
                                end_y,
                                nibble_high,
                                control_x,
                                control_y,
                            );
                            end_x = read.x;
                            end_y = read.y;
                            pos = read.pos;
                            nibble_high = read.nibble_high;
                        }

                        if current_contour.commands.is_empty() {
                            current_contour.move_to(start_x as f32, start_y as f32);
                        }
                        current_contour.curve_to(
                            control1_x as f32,
                            control1_y as f32,
                            control2_x as f32,
                            control2_y as f32,
                            end_x as f32,
                            end_y as f32,
                        );
                        previous_x = control2_x;
                        previous_y = control2_y;
                        current_x = end_x;
                        current_y = end_y;
                    }
                }
            }
        }

        if (current_x - before_x).abs() > 8192 || (current_y - before_y).abs() > 8192 {
            current_x = before_x;
            current_y = before_y;
        }
        if current_contour.commands.len() > 300 {
            break;
        }
    }

    if !current_contour.commands.is_empty() {
        glyph.contours.push(current_contour);
    }
}

fn parse_transform_modulo_6(data: &[u8], mut pos: usize, format: i32) -> TransformRead {
    let mut result = TransformRead { scale: 4096, offset: 0, pos };

    if format <= 2 {
        result.scale = 4096;
    } else if format == 5 {
        result.scale = 0;
    } else if pos + 2 <= data.len() {
        result.scale = ((data[pos] as i32 & 0xFF) << 8) | (data[pos + 1] as i32 & 0xFF);
        pos += 2;
    }

    if format == 0 || format == 5 {
        result.offset = 0;
    } else if format == 1 || format == 3 {
        if pos < data.len() {
            result.offset = sign8(data[pos] as i32);
            pos += 1;
        }
    } else if pos + 2 <= data.len() {
        result.offset = sign16(((data[pos] as i32 & 0xFF) << 8) | (data[pos + 1] as i32 & 0xFF));
        pos += 2;
    }

    result.pos = pos;
    result
}

fn parse_glyph_offset_modulo_6(data: &[u8], mut pos: usize, format: i32, accumulator: i32) -> GlyphOffsetRead {
    let mut result = GlyphOffsetRead { offset: 0, size: 0, pos, accumulator };

    match format {
        0 => {
            if pos < data.len() {
                let delta = data[pos] as i32 & 0xFF;
                pos += 1;
                result.size = delta;
                result.accumulator -= delta;
                result.offset = result.accumulator;
            }
        }
        1 => {
            if pos < data.len() {
                let delta = (data[pos] as i32 & 0xFF) + 256;
                pos += 1;
                result.size = delta;
                result.accumulator -= delta;
                result.offset = result.accumulator;
            }
        }
        2 => {
            if pos + 2 <= data.len() {
                let delta = ((data[pos] as i32 & 0xFF) << 8) | (data[pos + 1] as i32 & 0xFF);
                pos += 2;
                result.size = delta;
                result.accumulator -= delta;
                result.offset = result.accumulator;
            }
        }
        3 => {
            if pos + 3 <= data.len() {
                let combined = ((data[pos] as i32 & 0xFF) << 16)
                    | ((data[pos + 1] as i32 & 0xFF) << 8)
                    | (data[pos + 2] as i32 & 0xFF);
                pos += 3;
                result.size = combined >> 15;
                let delta = combined & 0x7FFF;
                result.offset = result.accumulator - delta;
            }
        }
        4 => {
            if pos + 3 <= data.len() {
                let combined = ((data[pos] as i32 & 0xFF) << 16)
                    | ((data[pos + 1] as i32 & 0xFF) << 8)
                    | (data[pos + 2] as i32 & 0xFF);
                pos += 3;
                result.size = combined >> 15;
                result.offset = combined & 0x7FFF;
            }
        }
        5 => {
            if pos + 4 <= data.len() {
                let combined = ((data[pos] as u32 & 0xFF) << 24)
                    | ((data[pos + 1] as u32 & 0xFF) << 16)
                    | ((data[pos + 2] as u32 & 0xFF) << 8)
                    | (data[pos + 3] as u32 & 0xFF);
                pos += 4;
                result.size = ((combined >> 23) & 0x1FF) as i32;
                result.offset = (combined & 0x7FFFFF) as i32;
            }
        }
        _ => {
            if pos + 5 <= data.len() {
                result.size = ((data[pos] as i32 & 0xFF) << 8) | (data[pos + 1] as i32 & 0xFF);
                pos += 2;
                result.offset = ((data[pos] as i32 & 0xFF) << 16)
                    | ((data[pos + 1] as i32 & 0xFF) << 8)
                    | (data[pos + 2] as i32 & 0xFF);
                pos += 3;
            }
        }
    }

    result.pos = pos;
    result
}

fn find_next_offset(sorted_offsets: &[i32], current_offset: i32) -> i32 {
    match sorted_offsets.binary_search(&current_offset) {
        Ok(idx) if idx + 1 < sorted_offsets.len() => sorted_offsets[idx + 1],
        Err(idx) if idx < sorted_offsets.len() => sorted_offsets[idx],
        _ => i32::MAX,
    }
}

fn append_transformed_contours(
    glyph: &mut OutlineGlyph,
    sub_glyph: &OutlineGlyph,
    x_scale: i32,
    y_scale: i32,
    x_offset: i32,
    y_offset: i32,
) {
    let x_scale_f = x_scale as f32 / 4096.0;
    let y_scale_f = y_scale as f32 / 4096.0;
    let x_offset_f = x_offset as f32;
    let y_offset_f = y_offset as f32;

    for source_contour in &sub_glyph.contours {
        let mut transformed = Contour::new();
        for command in &source_contour.commands {
            let x = command.x * x_scale_f + x_offset_f;
            let y = command.y * y_scale_f + y_offset_f;
            match command.type_ {
                0 => transformed.move_to(x, y),
                1 => transformed.line_to(x, y),
                2 => transformed.curve_to(
                    command.x1 * x_scale_f + x_offset_f,
                    command.y1 * y_scale_f + y_offset_f,
                    command.x2 * x_scale_f + x_offset_f,
                    command.y2 * y_scale_f + y_offset_f,
                    x,
                    y,
                ),
                _ => {}
            }
        }
        if !transformed.commands.is_empty() {
            glyph.contours.push(transformed);
        }
    }
}

fn parse_compound_glyph(
    font: &Pfr1Font,
    data: &[u8],
    start: usize,
    size: usize,
    glyph: &mut OutlineGlyph,
    known_offsets: &[i32],
    depth: i32,
) {
    if depth >= 8 {
        return;
    }

    let component_count = (data[start] & 0x3F) as i32;
    let mut pos = start + 1;

    if data[start] & 0x40 != 0 && pos + 2 <= start + size {
        let extra_count = (data[pos] as i32 & 0xFF) | ((data[pos + 1] as i32 & 0xFF) << 8);
        pos += 2;
        for _ in 0..extra_count {
            if pos >= start + size {
                break;
            }
            let length = data[pos] as i32 & 0xFF;
            pos += length as usize + 2;
        }
    }

    let glyph_gps_offset = start as i32 - font.gps_offset;
    let mut offset_accumulator = glyph_gps_offset;

    for _ in 0..component_count {
        if pos >= start + size {
            break;
        }

        let format_byte = data[pos] as i32 & 0xFF;
        pos += 1;
        let x_format = format_byte % 6;
        let y_format = (format_byte / 6) % 6;
        let offset_format = format_byte / 36;

        let x_transform = parse_transform_modulo_6(data, pos, x_format);
        pos = x_transform.pos;
        let y_transform = parse_transform_modulo_6(data, pos, y_format);
        pos = y_transform.pos;

        let glyph_offset = parse_glyph_offset_modulo_6(data, pos, offset_format, offset_accumulator);
        pos = glyph_offset.pos;
        offset_accumulator = glyph_offset.accumulator;

        let absolute_position = font.gps_offset + glyph_offset.offset;
        if absolute_position < 0 || absolute_position as usize >= data.len() {
            continue;
        }
        let absolute_position = absolute_position as usize;

        let mut max_size = data.len() - absolute_position;
        if font.gps_size > 0 && glyph_offset.offset < font.gps_size {
            max_size = max_size.min((font.gps_size - glyph_offset.offset) as usize);
        }
        let next_offset = find_next_offset(known_offsets, glyph_offset.offset);
        if next_offset > glyph_offset.offset {
            max_size = max_size.min((next_offset - glyph_offset.offset) as usize);
        }

        let effective_size = if glyph_offset.size > 0 {
            (glyph_offset.size as usize).min(max_size)
        } else {
            64.min(max_size)
        };
        if effective_size == 0 {
            continue;
        }

        let mut sub_record = CharacterRecord::default();
        sub_record.char_code = glyph.char_code;
        sub_record.set_width = glyph.set_width as i32;
        let sub_glyph = parse_outline_glyph(
            font,
            data,
            absolute_position,
            effective_size,
            &sub_record,
            known_offsets,
            depth + 1,
        );
        if !sub_glyph.contours.is_empty() {
            append_transformed_contours(
                glyph,
                &sub_glyph,
                x_transform.scale,
                y_transform.scale,
                x_transform.offset,
                y_transform.offset,
            );
        }
    }
}

fn parse_outline_glyph(
    font: &Pfr1Font,
    data: &[u8],
    start: usize,
    size: usize,
    record: &CharacterRecord,
    known_offsets: &[i32],
    depth: i32,
) -> OutlineGlyph {
    let mut glyph = OutlineGlyph {
        char_code: record.char_code,
        set_width: record.set_width as f32,
        contours: Vec::new(),
    };
    if size == 0 || start + size > data.len() {
        return glyph;
    }

    let flags = data[start] as i32 & 0xFF;
    let outline_format = (flags >> 6) & 0x03;
    let compound_glyph = outline_format >= 2 && (flags & 0x3F) > 0;
    if compound_glyph {
        parse_compound_glyph(font, data, start, size, &mut glyph, known_offsets, depth);
        return glyph;
    }

    parse_simple_outline_glyph(data, start, size, record, &mut glyph);
    glyph
}

fn parse_simple_outline_glyph(
    data: &[u8],
    start: usize,
    size: usize,
    _record: &CharacterRecord,
    glyph: &mut OutlineGlyph,
) {
    if size == 0 || start + size > data.len() {
        return;
    }

    let flags = data[start] as i32 & 0xFF;
    let mut pos = start + 1;
    let count_encoding = flags & 3;

    let mut x_orus_count = 0usize;
    let mut y_orus_count = 0usize;
    match count_encoding {
        1 => {
            if pos < start + size {
                let count_byte = data[pos] as i32 & 0xFF;
                pos += 1;
                x_orus_count = (count_byte & 0x0F) as usize;
                y_orus_count = ((count_byte >> 4) & 0x0F) as usize;
            }
        }
        2 | 3 => {
            if pos + 1 < start + size {
                x_orus_count = data[pos] as usize & 0xFF;
                pos += 1;
                y_orus_count = data[pos] as usize & 0xFF;
                pos += 1;
            }
        }
        _ => {}
    }

    let mut control_x = vec![0i32; x_orus_count];
    let mut control_y = vec![0i32; y_orus_count];
    pos = parse_control_values(data, pos, flags, &mut control_x, &mut control_y);

    if flags & 0x08 != 0 && pos < start + size {
        let extra_count = data[pos] as i32 & 0xFF;
        pos += 1;
        for _ in 0..extra_count {
            if pos + 1 >= start + size {
                break;
            }
            let item_length = data[pos] as i32 & 0xFF;
            pos += item_length as usize + 2;
        }
    }

    parse_nibble_commands(data, pos, start + size, glyph, &control_x, &control_y);
}

// ---------------------------------------------------------------------------
// Pfr1Font
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct CharacterRecord {
    pub char_code: i32,
    pub set_width: i32,
    pub gps_size: i32,
    pub gps_offset: i32,
}

#[derive(Debug, Clone)]
pub struct FontMetrics {
    pub outline_resolution: i32,
    pub metrics_resolution: i32,
    pub x_min: i32,
    pub y_min: i32,
    pub x_max: i32,
    pub y_max: i32,
    pub ascender: i32,
    pub descender: i32,
    pub std_vw: i32,
    pub std_hw: i32,
    pub has_bitmap_section: bool,
    pub font_id: String,
}

impl Default for FontMetrics {
    fn default() -> Self {
        FontMetrics {
            outline_resolution: 2048,
            metrics_resolution: 2048,
            x_min: 0,
            y_min: 0,
            x_max: 0,
            y_max: 0,
            ascender: 0,
            descender: 0,
            std_vw: 0,
            std_hw: 0,
            has_bitmap_section: false,
            font_id: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Command {
    pub type_: i32,
    pub x: f32,
    pub y: f32,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

#[derive(Debug, Clone)]
pub struct Contour {
    pub commands: Vec<Command>,
}

impl Default for Contour {
    fn default() -> Self {
        Contour::new()
    }
}

impl Contour {
    fn new() -> Self {
        Contour { commands: Vec::new() }
    }

    fn move_to(&mut self, x: f32, y: f32) {
        self.commands.push(Command { type_: 0, x, y, x1: 0.0, y1: 0.0, x2: 0.0, y2: 0.0 });
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.commands.push(Command { type_: 1, x, y, x1: 0.0, y1: 0.0, x2: 0.0, y2: 0.0 });
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.commands.push(Command { type_: 2, x, y, x1, y1, x2, y2 });
    }
}

#[derive(Debug, Clone)]
pub struct OutlineGlyph {
    pub char_code: i32,
    pub set_width: f32,
    pub contours: Vec<Contour>,
}

#[derive(Debug, Clone)]
pub struct BitmapGlyph {
    pub char_code: i32,
    pub image_format: i32,
    pub x_pos: i32,
    pub y_pos: i32,
    pub x_size: i32,
    pub y_size: i32,
    pub set_width: i32,
    pub image_data: Vec<u8>,
}

/// A parsed PFR1 font (LS Pfr1Font).
pub struct Pfr1Font {
    pub font_name: String,
    pub metrics: FontMetrics,
    pub char_records: Vec<CharacterRecord>,
    pub glyphs: HashMap<i32, OutlineGlyph>,
    pub bitmap_glyphs: HashMap<i32, BitmapGlyph>,
    pub font_matrix: [i32; 4],
    pub pfr_black_pixel: bool,
    pub gps_offset: i32,
    pub gps_size: i32,
    stored_max_chars: i32,
}

impl Pfr1Font {
    fn parse_header(&mut self, data: &[u8]) {
        let mut reader = PfrBitReader::new(data, 4);

        reader.read_u16();
        reader.read_u16();
        reader.read_u16();
        let log_font_dir_size = reader.read_u16();
        let log_font_dir_offset = reader.read_u16();
        reader.read_u16();
        let log_font_section_size = reader.read_u24();
        let log_font_section_offset = reader.read_u24();
        reader.read_u16();
        reader.read_u24();
        reader.read_u24();
        reader.read_u16();
        self.gps_size = reader.read_u24();
        self.gps_offset = reader.read_u24();
        reader.read_u8();
        reader.read_u8();
        reader.read_u8();
        reader.read_u8();

        let flags_byte = reader.read_u8();
        self.pfr_black_pixel = flags_byte & 0x01 != 0;

        reader.read_u24();
        reader.read_u24();
        reader.read_u24();
        reader.read_u16();
        reader.read_u8();
        reader.read_u8();
        self.stored_max_chars = reader.read_u16();

        self.parse_logical_font_directory(
            data,
            log_font_dir_size,
            log_font_dir_offset,
            log_font_section_size,
            log_font_section_offset,
        );
    }

    fn parse_logical_font_directory(
        &mut self,
        data: &[u8],
        dir_size: i32,
        dir_offset: i32,
        section_size: i32,
        section_offset: i32,
    ) {
        if dir_offset == 0 || dir_size == 0 {
            return;
        }

        if dir_size < 14 {
            if section_size >= 18 && section_offset > 0 && (section_offset as usize) < data.len() {
                let mut reader = PfrBitReader::new(data, section_offset as usize);
                for i in 0..4 {
                    self.font_matrix[i] = reader.read_i24();
                }
            }
            return;
        }

        if (dir_offset as usize) >= data.len() {
            return;
        }
        let mut reader = PfrBitReader::new(data, dir_offset as usize);
        let n_log_fonts = reader.read_u16();
        if n_log_fonts > 0 {
            for i in 0..4 {
                self.font_matrix[i] = reader.read_i24();
            }
        }
    }

    fn parse_physical_font(&mut self, data: &[u8]) {
        let mut header = PfrBitReader::new(data, 4);
        header.read_u16();
        header.read_u16();
        header.read_u16();
        header.read_u16();
        header.read_u16();
        header.read_u16();
        header.read_u24();
        header.read_u24();
        header.read_u16();
        let phys_font_section_size = header.read_u24();
        let phys_offset = header.read_u24();

        if (phys_offset as usize) >= data.len() {
            return;
        }
        let mut phys_end = data.len().min(phys_offset as usize + phys_font_section_size as usize);
        if self.gps_offset > phys_offset && (self.gps_offset as usize) <= data.len() {
            phys_end = phys_end.min(self.gps_offset as usize);
        }

        let mut reader = PfrBitReader::new(data, phys_offset as usize);
        reader.read_u16();
        self.metrics.outline_resolution = reader.read_u16();
        if self.metrics.outline_resolution == 0 {
            self.metrics.outline_resolution = 2048;
        }
        self.metrics.metrics_resolution = reader.read_u16();
        if self.metrics.metrics_resolution == 0 {
            self.metrics.metrics_resolution = self.metrics.outline_resolution;
        }

        self.metrics.x_min = sign16(reader.read_u16());
        self.metrics.y_min = sign16(reader.read_u16());
        self.metrics.x_max = sign16(reader.read_u16());
        self.metrics.y_max = sign16(reader.read_u16());
        self.metrics.ascender = self.metrics.y_max;
        self.metrics.descender = self.metrics.y_min;

        let extra_items_present = reader.read_bit();
        reader.read_bit();
        reader.read_bit();
        reader.read_bit();
        reader.read_bit();
        let proportional_escapement = reader.read_bit();
        reader.read_bit();
        reader.read_bit();

        let mut standard_set_width = 0;
        if !proportional_escapement {
            standard_set_width = sign16(reader.read_u16());
        }

        if extra_items_present {
            let n_extra_items = reader.read_u8();
            for _ in 0..n_extra_items {
                if reader.remaining() < 2 {
                    break;
                }
                let item_size = reader.read_u8();
                let item_type = reader.read_u8();
                let item_start = reader.position();

                if item_type == 1 {
                    self.metrics.has_bitmap_section = true;
                    reader.skip(item_size as usize);
                } else if item_type == 2 {
                    let mut id = String::new();
                    for _ in 0..item_size {
                        let ch = reader.read_u8();
                        if ch == 0 {
                            break;
                        }
                        id.push(ch as u8 as char);
                    }
                    self.metrics.font_id = id.clone();
                    self.font_name = id;
                    let consumed = reader.position() - item_start;
                    if consumed < item_size as usize {
                        reader.skip(item_size as usize - consumed);
                    }
                } else {
                    reader.skip(item_size as usize);
                }
            }
        }

        let n_aux_bytes = reader.read_u24();
        if n_aux_bytes > 0 && n_aux_bytes < 10000 {
            reader.skip(n_aux_bytes as usize);
        } else if n_aux_bytes >= 10000 {
            while reader.position() < phys_end {
                let probe_pos = reader.position();
                let n_blue_values = reader.read_u8();
                let byte_counter = n_blue_values as usize * 2 + 6;
                let n_chars_pos = reader.position() + byte_counter;
                if n_chars_pos + 2 > phys_end {
                    reader.set_position(probe_pos + 1);
                    continue;
                }
                reader.set_position(n_chars_pos);
                let n_characters = reader.read_u16();
                if n_characters == self.stored_max_chars {
                    reader.set_position(probe_pos);
                    break;
                }
                reader.set_position(probe_pos + 1);
            }
        }

        let n_blue_values = reader.read_u8();
        for _ in 0..n_blue_values {
            reader.read_u16();
        }
        reader.read_u8();
        reader.read_u8();

        self.metrics.std_vw = sign16(reader.read_u16());
        self.metrics.std_hw = sign16(reader.read_u16());
        let n_characters = reader.read_u16();
        self.parse_delta_encoded_char_records(&mut reader, n_characters, standard_set_width);
    }

    fn parse_delta_encoded_char_records(
        &mut self,
        reader: &mut PfrBitReader,
        n_characters: i32,
        standard_set_width: i32,
    ) {
        let mut char_code: i32 = -1;
        let mut set_width = standard_set_width;
        let mut glyph_size: i32 = 0;
        let mut glyph_offset: i32 = 0;

        for _ in 0..n_characters {
            if reader.remaining() < 1 {
                break;
            }

            let flags = reader.read_u8();
            let next_gps_offset = glyph_offset + glyph_size;

            let char_code_mode = flags & 0x03;
            char_code += 1;
            match char_code_mode {
                1 => char_code += reader.read_u8(),
                2 => char_code += reader.read_u16(),
                _ => {}
            }

            let set_width_mode = (flags >> 2) & 0x03;
            match set_width_mode {
                1 => set_width += reader.read_u8(),
                2 => set_width -= reader.read_u8(),
                3 => set_width = sign16(reader.read_u16()),
                _ => {}
            }

            let gps_size_mode = (flags >> 4) & 0x03;
            match gps_size_mode {
                0 => glyph_size = reader.read_u8(),
                1 => glyph_size = reader.read_u8() + 256,
                2 => glyph_size = reader.read_u8() + 512,
                3 => glyph_size = reader.read_u16(),
                _ => {}
            }

            let gps_offset_mode = (flags >> 6) & 0x03;
            match gps_offset_mode {
                0 => glyph_offset = next_gps_offset,
                1 => glyph_offset = next_gps_offset + reader.read_u8(),
                2 => glyph_offset = reader.read_u16(),
                3 => glyph_offset = reader.read_u24(),
                _ => {}
            }

            self.char_records.push(CharacterRecord {
                char_code,
                set_width,
                gps_size: glyph_size,
                gps_offset: glyph_offset,
            });
        }
    }

    fn parse_glyph_stubs_and_bitmaps(&mut self, data: &[u8]) {
        if (self.gps_offset + self.gps_size) as usize > data.len() {
            return;
        }

        let mut known_offsets: Vec<i32> = self.char_records.iter().map(|r| r.gps_offset).collect();
        known_offsets.sort_unstable();

        for record in &self.char_records {
            if record.gps_size <= 1 {
                self.glyphs.insert(
                    record.char_code,
                    OutlineGlyph { char_code: record.char_code, set_width: record.set_width as f32, contours: Vec::new() },
                );
                continue;
            }

            let start = self.gps_offset + record.gps_offset;
            let size = record.gps_size;
            if start < 0 || size <= 0 || (start as usize) + (size as usize) > data.len() {
                continue;
            }
            let start = start as usize;
            let size = size as usize;

            if self.metrics.has_bitmap_section {
                let zeros_field = (data[start] >> 4) & 0x07;
                if zeros_field != 0 {
                    if let Some(mut glyph) = self.parse_bitmap_glyph(data, start, size, record.char_code) {
                        if record.set_width > 0 {
                            glyph.set_width = record.set_width;
                        }
                        self.bitmap_glyphs.insert(record.char_code, glyph);
                    }
                }
            }

            let glyph = parse_outline_glyph(self, data, start, size, record, &known_offsets, 0);
            self.glyphs.insert(record.char_code, glyph);
        }

        for lower in b'a'..=b'z' {
            let upper = lower - 32;
            if let Some(lower_glyph) = self.glyphs.get(&(lower as i32)) {
                if !lower_glyph.contours.is_empty() {
                    continue;
                }
            }
            if let Some(upper_glyph) = self.glyphs.get(&(upper as i32)) {
                if !upper_glyph.contours.is_empty() {
                    let mut copy = upper_glyph.clone();
                    copy.char_code = lower as i32;
                    self.glyphs.insert(lower as i32, copy);
                }
            }
        }
    }

    fn parse_bitmap_glyph(&self, data: &[u8], start: usize, size: usize, char_code: i32) -> Option<BitmapGlyph> {
        if size < 2 {
            return None;
        }

        let end = start + size;
        let mut pos = start;
        let format_byte = data[pos] as i32 & 0xFF;
        pos += 1;

        let image_format = (format_byte >> 6) & 0x03;
        let escapement_format = (format_byte >> 4) & 0x03;
        let size_format = (format_byte >> 2) & 0x03;
        let position_format = format_byte & 0x03;

        let pos_bytes = match position_format {
            0 => 0,
            1 => 2,
            2 => 4,
            _ => 8,
        };
        let size_bytes = match size_format {
            0 => 2,
            1 => 4,
            2 => 6,
            _ => 8,
        };
        let esc_bytes = match escapement_format {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        if pos + pos_bytes + size_bytes + esc_bytes > end || pos + pos_bytes + size_bytes + esc_bytes > data.len() {
            return None;
        }

        let mut x_pos: i32 = 0;
        let mut y_pos: i32 = 0;
        match position_format {
            1 => {
                x_pos = read_signed_n(data, pos, 1);
                pos += 1;
                y_pos = read_signed_n(data, pos, 1);
                pos += 1;
            }
            2 => {
                x_pos = read_signed_n(data, pos, 2);
                pos += 2;
                y_pos = read_signed_n(data, pos, 2);
                pos += 2;
            }
            3 => {
                x_pos = read_signed_n(data, pos, 4);
                pos += 4;
                y_pos = read_signed_n(data, pos, 4);
                pos += 4;
            }
            _ => {}
        }

        let x_size: i32;
        let y_size: i32;
        match size_format {
            0 => {
                x_size = read_unsigned_n(data, pos, 1);
                pos += 1;
                y_size = read_unsigned_n(data, pos, 1);
                pos += 1;
            }
            1 => {
                x_size = read_unsigned_n(data, pos, 2);
                pos += 2;
                y_size = read_unsigned_n(data, pos, 2);
                pos += 2;
            }
            2 => {
                x_size = read_unsigned_n(data, pos, 3);
                pos += 3;
                y_size = read_unsigned_n(data, pos, 3);
                pos += 3;
            }
            _ => {
                x_size = read_unsigned_n(data, pos, 4);
                pos += 4;
                y_size = read_unsigned_n(data, pos, 4);
                pos += 4;
            }
        }

        let mut set_width = x_size;
        match escapement_format {
            1 => {
                set_width = read_unsigned_n(data, pos, 1);
                pos += 1;
            }
            2 => {
                set_width = read_unsigned_n(data, pos, 2);
                pos += 2;
            }
            3 => {
                set_width = read_unsigned_n(data, pos, 4);
                pos += 4;
            }
            _ => {}
        }

        if x_size <= 0 || y_size <= 0 || x_size > 4096 || y_size > 4096 {
            return None;
        }
        let total_bits = x_size * y_size;
        if total_bits <= 0 || total_bits > 1_000_000 {
            return None;
        }

        let remaining = end - pos;
        if remaining == 0 || pos > data.len() {
            return None;
        }

        let image_data: Vec<u8> = if image_format == 0 {
            let expected = (total_bits as usize + 7) / 8;
            if expected > remaining || expected == 0 {
                return None;
            }
            data[pos..pos + expected].to_vec()
        } else if image_format == 1 {
            decode_rle_bitmap(data, pos, remaining, x_size as usize, y_size as usize)
        } else {
            data[pos..end].to_vec()
        };

        Some(BitmapGlyph {
            char_code,
            image_format,
            x_pos: sign16(x_pos & 0xFFFF),
            y_pos: sign16(y_pos & 0xFFFF),
            x_size,
            y_size,
            set_width,
            image_data,
        })
    }
}

/// Parse a PFR1 font. Returns `None` for non-PFR1 or too-small payloads; a
/// partially-parsed font is returned for malformed interiors (LS's lenient
/// try/catch behavior — Rust side never panics).
pub fn parse_fr1(data: &[u8]) -> Option<Pfr1Font> {
    if data.len() < 58 || &data[0..4] != b"PFR1" {
        return None;
    }

    let mut font = Pfr1Font {
        font_name: String::new(),
        metrics: FontMetrics::default(),
        char_records: Vec::new(),
        glyphs: HashMap::new(),
        bitmap_glyphs: HashMap::new(),
        font_matrix: [256, 0, 0, 256],
        pfr_black_pixel: false,
        gps_offset: 0,
        gps_size: 0,
        stored_max_chars: 0,
    };
    font.parse_header(data);
    font.parse_physical_font(data);
    font.parse_glyph_stubs_and_bitmaps(data);
    Some(font)
}

// ---------------------------------------------------------------------------
// FontMapChunk reader (fonts.txt manifest)
// ---------------------------------------------------------------------------

/// One entry of a Fmap (font map) chunk.
#[derive(Debug, Clone)]
pub struct FmapEntry {
    pub font_id: i32,
    pub platform: i32,
    pub font_name: String,
}

fn read_be_i32(data: &[u8], pos: &mut usize) -> i32 {
    if *pos + 4 > data.len() {
        return 0;
    }
    let v = i32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    v
}

fn read_be_u16(data: &[u8], pos: &mut usize) -> i32 {
    if *pos + 2 > data.len() {
        return 0;
    }
    let v = u16::from_be_bytes([data[*pos], data[*pos + 1]]) as i32;
    *pos += 2;
    v
}

/// Parse a Fmap chunk (LS FontMapChunk::read).
pub fn parse_fmap(data: &[u8]) -> Vec<FmapEntry> {
    let mut pos = 0usize;
    let map_length = read_be_i32(data, &mut pos);
    let _ = read_be_i32(data, &mut pos); // reserved
    let body_start = pos;
    let names_start = body_start + map_length as usize + 2;

    pos += 8;
    let entries_used = read_be_i32(data, &mut pos);
    pos += 16;

    let mut entries = Vec::new();
    for _ in 0..entries_used {
        if pos + 8 > data.len() {
            break;
        }
        let name_offset = read_be_i32(data, &mut pos);
        let platform = read_be_u16(data, &mut pos);
        let font_id = read_be_u16(data, &mut pos);

        let return_position = pos;
        let mut font_name = String::new();
        let name_position = names_start as i64 + name_offset as i64;
        if name_position >= 0 && (name_position as usize) + 2 <= data.len() {
            let np = name_position as usize;
            let name_length = u16::from_be_bytes([data[np], data[np + 1]]) as usize;
            if name_length <= data.len() - np - 2 {
                font_name = crate::lscr::decode_mac_roman(&data[np + 2..np + 2 + name_length]);
            }
        }
        pos = return_position;
        entries.push(FmapEntry { font_id, platform, font_name });
    }
    entries
}

// ---------------------------------------------------------------------------
// PFR1 → TTF converter
// ---------------------------------------------------------------------------

struct ByteWriter {
    bytes: Vec<u8>,
}

impl ByteWriter {
    fn new() -> Self {
        ByteWriter { bytes: Vec::new() }
    }

    fn write_u8(&mut self, value: i32) {
        self.bytes.push((value & 0xFF) as u8);
    }

    fn write_u16(&mut self, value: i32) {
        let raw = (value & 0xFFFF) as u16;
        self.bytes.push((raw >> 8) as u8);
        self.bytes.push(raw as u8);
    }

    fn write_u32(&mut self, value: u32) {
        self.bytes.push((value >> 24) as u8);
        self.bytes.push((value >> 16) as u8);
        self.bytes.push((value >> 8) as u8);
        self.bytes.push(value as u8);
    }

    fn write_i64_zero(&mut self) {
        for _ in 0..8 {
            self.write_u8(0);
        }
    }

    fn write_bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    fn write_ascii_tag(&mut self, tag: &[u8; 4]) {
        for &b in tag {
            self.write_u8(b as i32);
        }
    }
}

struct TtPoint {
    x: i32,
    y: i32,
    on_curve: bool,
}

struct GlyphEntry {
    char_code: i32,
    advance_width: i32,
    lsb: i32,
    glyph: OutlineGlyph,
}

fn highest_one_bit(value: i32) -> i32 {
    let mut result = 1;
    while result <= value / 2 {
        result <<= 1;
    }
    result
}

fn trailing_zero_count(mut value: i32) -> i32 {
    let mut result = 0;
    while value > 1 && value & 1 == 0 {
        result += 1;
        value >>= 1;
    }
    result
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let len = (data.len() + 3) & !3;
    let mut i = 0;
    while i < len {
        let b0 = if i < data.len() { data[i] as u32 } else { 0 };
        let b1 = if i + 1 < data.len() { data[i + 1] as u32 } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] as u32 } else { 0 };
        let b3 = if i + 3 < data.len() { data[i + 3] as u32 } else { 0 };
        sum = sum.wrapping_add((b0 << 24) | (b1 << 16) | (b2 << 8) | b3);
        i += 4;
    }
    sum
}

fn build_head(units_per_em: i32, metrics: &FontMetrics) -> Vec<u8> {
    let mut writer = ByteWriter::new();
    writer.write_u32(0x00010000);
    writer.write_u32(0x00005000);
    writer.write_u32(0);
    writer.write_u32(0x5F0F3CF5);
    writer.write_u16(0x000B);
    writer.write_u16(units_per_em);
    writer.write_i64_zero();
    writer.write_i64_zero();
    writer.write_u16(metrics.x_min);
    writer.write_u16(metrics.y_min);
    writer.write_u16(metrics.x_max);
    writer.write_u16(metrics.y_max);
    writer.write_u16(0);
    writer.write_u16(8);
    writer.write_u16(2);
    writer.write_u16(0);
    writer.write_u16(0);
    writer.bytes
}

fn build_hhea(metrics: &FontMetrics, entries: &[GlyphEntry]) -> Vec<u8> {
    let mut max_advance_width = 0;
    for entry in entries {
        max_advance_width = max_advance_width.max(entry.advance_width);
    }

    let mut writer = ByteWriter::new();
    writer.write_u32(0x00010000);
    writer.write_u16(metrics.ascender);
    writer.write_u16(metrics.descender);
    writer.write_u16(0);
    writer.write_u16(max_advance_width);
    writer.write_u16(metrics.x_min);
    writer.write_u16(metrics.x_min);
    writer.write_u16(metrics.x_max);
    writer.write_u16(1);
    writer.write_u16(0);
    writer.write_u16(0);
    writer.write_i64_zero();
    writer.write_u16(0);
    writer.write_u16(entries.len() as i32);
    writer.bytes
}

fn build_maxp(num_glyphs: i32) -> Vec<u8> {
    let mut writer = ByteWriter::new();
    writer.write_u32(0x00010000);
    writer.write_u16(num_glyphs);
    writer.write_u16(256);
    writer.write_u16(32);
    writer.write_u16(0);
    writer.write_u16(0);
    writer.write_u16(1);
    writer.write_u16(0);
    writer.write_u16(0);
    writer.write_u16(0);
    writer.write_u16(0);
    writer.write_u16(0);
    writer.write_u16(0);
    writer.write_u16(0);
    writer.write_u16(0);
    writer.bytes
}

fn build_os2(metrics: &FontMetrics, units_per_em: i32, cmap_entries: &std::collections::BTreeMap<i32, i32>) -> Vec<u8> {
    let mut first_char = 0x0020;
    let mut last_char = 0x00FF;
    if !cmap_entries.is_empty() {
        first_char = *cmap_entries.keys().next().unwrap();
        last_char = *cmap_entries.keys().last().unwrap();
    }

    let mut writer = ByteWriter::new();
    writer.write_u16(4);
    writer.write_u16(units_per_em / 2);
    writer.write_u16(400);
    writer.write_u16(5);
    writer.write_u16(0);
    writer.write_u16(units_per_em / 10);
    writer.write_u16(units_per_em / 10);
    writer.write_u16(0);
    writer.write_u16(units_per_em / 5);
    writer.write_u16(units_per_em / 10);
    writer.write_u16(units_per_em / 10);
    writer.write_u16(0);
    writer.write_u16(units_per_em / 3);
    writer.write_u16(if metrics.std_vw > 0 { metrics.std_vw } else { units_per_em / 20 });
    writer.write_u16(metrics.ascender / 2);
    writer.write_u16(0);
    for _ in 0..10 {
        writer.write_u8(0);
    }
    writer.write_u32(0);
    writer.write_u32(0);
    writer.write_u32(0);
    writer.write_u32(0);
    writer.write_ascii_tag(b"    ");
    writer.write_u16(0x0040);
    writer.write_u16(first_char);
    writer.write_u16(last_char.min(0xFFFF));
    writer.write_u16(metrics.ascender);
    writer.write_u16(metrics.descender);
    writer.write_u16(0);
    writer.write_u16(metrics.ascender.max(0));
    writer.write_u16((metrics.descender.min(0)).abs());
    writer.write_u32(1);
    writer.write_u32(0);
    writer.write_u16(metrics.ascender * 8 / 10);
    writer.write_u16(metrics.ascender);
    writer.write_u16(0);
    writer.write_u16(0x0020);
    writer.write_u16(1);
    writer.bytes
}

fn utf16be_ascii(value: &str) -> Vec<u8> {
    let mut result = Vec::with_capacity(value.len() * 2);
    for ch in value.bytes() {
        result.push(0);
        result.push(ch);
    }
    result
}

fn remove_spaces(value: &str) -> String {
    value.chars().filter(|&c| c != ' ').collect()
}

fn build_name(family_name: &str) -> Vec<u8> {
    let names: Vec<String> = vec![
        String::new(),
        family_name.to_string(),
        "Regular".to_string(),
        format!("{family_name}-Regular"),
        family_name.to_string(),
        "Version 1.0".to_string(),
        remove_spaces(family_name),
    ];

    let mut encoded: Vec<Vec<u8>> = Vec::with_capacity(names.len());
    for name in &names {
        encoded.push(utf16be_ascii(name));
    }

    let count = encoded.len() as i32;
    let storage_offset = 6 + count * 12;
    let mut writer = ByteWriter::new();
    writer.write_u16(0);
    writer.write_u16(count);
    writer.write_u16(storage_offset);

    let mut string_offset = 0;
    for (i, enc) in encoded.iter().enumerate() {
        writer.write_u16(3);
        writer.write_u16(1);
        writer.write_u16(0x0409);
        writer.write_u16(i as i32);
        writer.write_u16(enc.len() as i32);
        writer.write_u16(string_offset);
        string_offset += enc.len() as i32;
    }

    for text in &encoded {
        writer.write_bytes(text);
    }
    writer.bytes
}

fn build_cmap(char_to_glyph: &std::collections::BTreeMap<i32, i32>) -> Vec<u8> {
    let mut segments: Vec<(i32, i32)> = Vec::new();
    if !char_to_glyph.is_empty() {
        let mut iter = char_to_glyph.keys();
        let mut start = *iter.next().unwrap();
        let mut end = start;
        for &key in iter {
            if key == end + 1 {
                end = key;
            } else {
                segments.push((start, end));
                start = key;
                end = key;
            }
        }
        segments.push((start, end));
    }
    segments.push((0xFFFF, 0xFFFF));

    let seg_count = segments.len() as i32;
    let search_range = highest_one_bit(seg_count) * 2;
    let entry_selector = trailing_zero_count(highest_one_bit(seg_count));
    let range_shift = seg_count * 2 - search_range;

    let mut subtable = ByteWriter::new();
    for segment in &segments {
        subtable.write_u16(segment.1);
    }
    subtable.write_u16(0);
    for segment in &segments {
        subtable.write_u16(segment.0);
    }

    let mut glyph_id_array_entries: Vec<i32> = Vec::new();
    let mut id_range_offsets: Vec<i32> = vec![0; segments.len()];
    for (i, segment) in segments.iter().enumerate() {
        if segment.0 == 0xFFFF {
            id_range_offsets[i] = 0;
        } else {
            let array_start_index = glyph_id_array_entries.len() as i32;
            let remaining_offsets = seg_count - i as i32;
            id_range_offsets[i] = (remaining_offsets + array_start_index) * 2;
            for code in segment.0..=segment.1 {
                let mapped = char_to_glyph.get(&code);
                glyph_id_array_entries.push(mapped.map_or(0, |&v| v));
            }
        }
    }

    for segment in &segments {
        subtable.write_u16(if segment.0 == 0xFFFF { 1 } else { 0 });
    }
    for &offset in &id_range_offsets {
        subtable.write_u16(offset);
    }
    for &glyph_id in &glyph_id_array_entries {
        subtable.write_u16(glyph_id);
    }

    let subtable_data = subtable.bytes;
    let subtable_length = 14 + subtable_data.len() as i32;

    let mut writer = ByteWriter::new();
    writer.write_u16(0);
    writer.write_u16(1);
    writer.write_u16(3);
    writer.write_u16(1);
    writer.write_u32(12);
    writer.write_u16(4);
    writer.write_u16(subtable_length);
    writer.write_u16(0);
    writer.write_u16(seg_count * 2);
    writer.write_u16(search_range);
    writer.write_u16(entry_selector);
    writer.write_u16(range_shift);
    writer.write_bytes(&subtable_data);
    writer.bytes
}

fn build_post() -> Vec<u8> {
    let mut writer = ByteWriter::new();
    writer.write_u32(0x00030000);
    writer.write_u32(0);
    writer.write_u16(-100);
    writer.write_u16(50);
    writer.write_u32(0);
    writer.write_u32(0);
    writer.write_u32(0);
    writer.write_u32(0);
    writer.write_u32(0);
    writer.bytes
}

fn build_hmtx(entries: &[GlyphEntry]) -> Vec<u8> {
    let mut writer = ByteWriter::new();
    for entry in entries {
        writer.write_u16(entry.advance_width);
        writer.write_u16(entry.lsb);
    }
    writer.bytes
}

fn build_loca(offsets: &[i32]) -> Vec<u8> {
    let mut writer = ByteWriter::new();
    for &offset in offsets {
        writer.write_u16(offset);
    }
    writer.bytes
}

fn round_to_int(value: f32) -> i32 {
    value.round() as i32
}

fn cubic_to_quadratic(
    points: &mut Vec<TtPoint>,
    x0: f32,
    y0: f32,
    c1x: f32,
    c1y: f32,
    c2x: f32,
    c2y: f32,
    ex: f32,
    ey: f32,
) {
    let m01x = (x0 + c1x) / 2.0;
    let m01y = (y0 + c1y) / 2.0;
    let m12x = (c1x + c2x) / 2.0;
    let m12y = (c1y + c2y) / 2.0;
    let m23x = (c2x + ex) / 2.0;
    let m23y = (c2y + ey) / 2.0;
    let m012x = (m01x + m12x) / 2.0;
    let m012y = (m01y + m12y) / 2.0;
    let m123x = (m12x + m23x) / 2.0;
    let m123y = (m12y + m23y) / 2.0;
    let mid_x = (m012x + m123x) / 2.0;
    let mid_y = (m012y + m123y) / 2.0;

    let q1x = (m01x + m012x) / 2.0;
    let q1y = (m01y + m012y) / 2.0;
    points.push(TtPoint { x: round_to_int(q1x), y: round_to_int(q1y), on_curve: false });
    points.push(TtPoint { x: round_to_int(mid_x), y: round_to_int(mid_y), on_curve: true });

    let q2x = (m123x + m23x) / 2.0;
    let q2y = (m123y + m23y) / 2.0;
    points.push(TtPoint { x: round_to_int(q2x), y: round_to_int(q2y), on_curve: false });
    points.push(TtPoint { x: round_to_int(ex), y: round_to_int(ey), on_curve: true });
}

fn build_glyf(entry: &GlyphEntry) -> Vec<u8> {
    if entry.glyph.contours.is_empty() {
        return Vec::new();
    }

    let mut tt_contours: Vec<Vec<TtPoint>> = Vec::new();
    for contour in &entry.glyph.contours {
        let mut points: Vec<TtPoint> = Vec::new();
        let mut current_x: f32 = 0.0;
        let mut current_y: f32 = 0.0;

        for command in &contour.commands {
            if command.type_ == 0 {
                if !points.is_empty() {
                    tt_contours.push(std::mem::take(&mut points));
                    points = Vec::new();
                }
                current_x = command.x;
                current_y = command.y;
                points.push(TtPoint { x: round_to_int(current_x), y: round_to_int(current_y), on_curve: true });
            } else if command.type_ == 1 {
                current_x = command.x;
                current_y = command.y;
                points.push(TtPoint { x: round_to_int(current_x), y: round_to_int(current_y), on_curve: true });
            } else if command.type_ == 2 {
                cubic_to_quadratic(
                    &mut points,
                    current_x,
                    current_y,
                    command.x1,
                    command.y1,
                    command.x2,
                    command.y2,
                    command.x,
                    command.y,
                );
                current_x = command.x;
                current_y = command.y;
            }
        }
        if !points.is_empty() {
            tt_contours.push(points);
        }
    }

    if tt_contours.is_empty() {
        return Vec::new();
    }

    let mut x_min = i32::MAX;
    let mut y_min = i32::MAX;
    let mut x_max = i32::MIN;
    let mut y_max = i32::MIN;
    for contour in &tt_contours {
        for point in contour {
            x_min = x_min.min(point.x);
            y_min = y_min.min(point.y);
            x_max = x_max.max(point.x);
            y_max = y_max.max(point.y);
        }
    }

    let mut writer = ByteWriter::new();
    writer.write_u16(tt_contours.len() as i32);
    writer.write_u16(x_min);
    writer.write_u16(y_min);
    writer.write_u16(x_max);
    writer.write_u16(y_max);

    let mut index: i32 = -1;
    for contour in &tt_contours {
        index += contour.len() as i32;
        writer.write_u16(index);
    }
    writer.write_u16(0);

    let mut flags: Vec<i32> = Vec::new();
    let mut x_coords: Vec<i32> = Vec::new();
    let mut y_coords: Vec<i32> = Vec::new();
    let mut previous_x: i32 = 0;
    let mut previous_y: i32 = 0;
    for contour in &tt_contours {
        for point in contour {
            let dx = point.x - previous_x;
            let dy = point.y - previous_y;
            let mut flag = if point.on_curve { 1 } else { 0 };

            if dx == 0 {
                flag |= 0x10;
            } else if dx >= -255 && dx <= 255 {
                flag |= 0x02;
                if dx > 0 {
                    flag |= 0x10;
                }
            }

            if dy == 0 {
                flag |= 0x20;
            } else if dy >= -255 && dy <= 255 {
                flag |= 0x04;
                if dy > 0 {
                    flag |= 0x20;
                }
            }

            flags.push(flag);
            x_coords.push(dx);
            y_coords.push(dy);
            previous_x = point.x;
            previous_y = point.y;
        }
    }

    for &flag in &flags {
        writer.write_u8(flag);
    }

    for i in 0..x_coords.len() {
        let dx = x_coords[i];
        let flag = flags[i];
        if flag & 0x02 != 0 {
            writer.write_u8(dx.abs());
        } else if flag & 0x10 == 0 {
            writer.write_u16(dx);
        }
    }

    for i in 0..y_coords.len() {
        let dy = y_coords[i];
        let flag = flags[i];
        if flag & 0x04 != 0 {
            writer.write_u8(dy.abs());
        } else if flag & 0x20 == 0 {
            writer.write_u16(dy);
        }
    }

    writer.bytes
}

fn glyph_lsb(glyph: &OutlineGlyph) -> i32 {
    let mut min_x = i32::MAX;
    for contour in &glyph.contours {
        for command in &contour.commands {
            min_x = min_x.min(round_to_int(command.x));
        }
    }
    if min_x == i32::MAX { 0 } else { min_x }
}

fn assemble_ttf(tags: &[&str], tables: &[Vec<u8>]) -> Vec<u8> {
    let num_tables = tags.len() as i32;
    let search_range = highest_one_bit(num_tables) * 16;
    let entry_selector = trailing_zero_count(highest_one_bit(num_tables));
    let range_shift = num_tables * 16 - search_range;

    let header_size = 12 + num_tables * 16;
    let mut offsets: Vec<i32> = vec![0; num_tables as usize];
    let mut current_offset = header_size;
    for (i, table) in tables.iter().enumerate() {
        offsets[i] = current_offset;
        current_offset += ((table.len() + 3) & !3) as i32;
    }

    let mut writer = ByteWriter::new();
    writer.write_u32(0x00010000);
    writer.write_u16(num_tables);
    writer.write_u16(search_range);
    writer.write_u16(entry_selector);
    writer.write_u16(range_shift);

    for (i, table) in tables.iter().enumerate() {
        writer.write_ascii_tag(tags[i].as_bytes().try_into().unwrap());
        writer.write_u32(checksum(table));
        writer.write_u32(offsets[i] as u32);
        writer.write_u32(table.len() as u32);
    }

    for table in tables {
        writer.write_bytes(table);
        let pad = (4 - (table.len() % 4)) % 4;
        for _ in 0..pad {
            writer.write_u8(0);
        }
    }

    writer.bytes
}

/// Convert a parsed PFR1 font into a TTF byte stream (LS Pfr1TtfConverter::convert).
pub fn convert_ttf(font: &Pfr1Font, family_name: &str) -> Vec<u8> {
    let mut entries: Vec<GlyphEntry> = Vec::new();
    entries.push(GlyphEntry {
        char_code: 0,
        advance_width: 0,
        lsb: 0,
        glyph: OutlineGlyph { char_code: 0, set_width: 0.0, contours: Vec::new() },
    });

    let units_per_em = if font.metrics.outline_resolution > 0 { font.metrics.outline_resolution } else { 2048 };
    for record in &font.char_records {
        let Some(glyph) = font.glyphs.get(&record.char_code) else { continue };
        entries.push(GlyphEntry {
            char_code: record.char_code,
            advance_width: record.set_width,
            lsb: glyph_lsb(glyph),
            glyph: glyph.clone(),
        });
    }

    let mut cmap_entries: std::collections::BTreeMap<i32, i32> = std::collections::BTreeMap::new();
    for (i, entry) in entries.iter().enumerate().skip(1) {
        cmap_entries.insert(entry.char_code, i as i32);
    }

    let head_table = build_head(units_per_em, &font.metrics);
    let hhea_table = build_hhea(&font.metrics, &entries);
    let maxp_table = build_maxp(entries.len() as i32);
    let os2_table = build_os2(&font.metrics, units_per_em, &cmap_entries);
    let name_table = build_name(family_name);
    let cmap_table = build_cmap(&cmap_entries);
    let post_table = build_post();

    let mut glyf_writer = ByteWriter::new();
    let mut loca_offsets: Vec<i32> = Vec::new();
    for entry in &entries {
        while glyf_writer.bytes.len() % 2 != 0 {
            glyf_writer.write_u8(0);
        }
        loca_offsets.push((glyf_writer.bytes.len() / 2) as i32);
        glyf_writer.write_bytes(&build_glyf(entry));
    }
    while glyf_writer.bytes.len() % 2 != 0 {
        glyf_writer.write_u8(0);
    }
    loca_offsets.push((glyf_writer.bytes.len() / 2) as i32);

    let glyf_table = glyf_writer.bytes;
    let loca_table = build_loca(&loca_offsets);
    let hmtx_table = build_hmtx(&entries);

    assemble_ttf(
        &["cmap", "glyf", "head", "hhea", "hmtx", "loca", "maxp", "name", "OS/2", "post"],
        &[cmap_table, glyf_table, head_table, hhea_table, hmtx_table, loca_table, maxp_table, name_table, os2_table, post_table],
    )
}
