//! Director/Shockwave file format data types.
//!
//! This crate provides the data structures and parsers for the various
//! Director file format chunks, including the memory map, cast members,
//! Lingo script bytecode, styled text, bitmaps, sounds, and more.

pub mod mmap;
pub mod cast;
pub mod lscr;
pub mod stxt;
pub mod bitd;
pub mod key;
pub mod sound;
pub mod clut;
pub mod lingo;
pub mod decomp;
pub mod font;
pub mod palette_data;

use director_rifx::Chunk as RifxChunk;

/// Parse a complete Director file chunk tree into a DirectorMovie structure.
pub fn parse_movie(root: &RifxChunk) -> Result<DirectorMovie, ParseError> {
    let mmap = root.child(&*b"mmap").ok_or(ParseError::MissingChunk("mmap"))?;
    let memory_map = mmap::read_mmap(mmap)?;

    Ok(DirectorMovie {
        memory_map,
    })
}

/// High-level representation of a Director movie.
pub struct DirectorMovie {
    pub memory_map: mmap::MemoryMap,
}

// ---------------------------------------------------------------------------
// ParseError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ParseError {
    MissingChunk(&'static str),
    InvalidData(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MissingChunk(s) => write!(f, "missing chunk: {s}"),
            ParseError::InvalidData(s) => write!(f, "invalid data: {s}"),
            ParseError::Io(e) => write!(f, "I/O: {e}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<std::io::Error> for ParseError {
    fn from(e: std::io::Error) -> Self {
        ParseError::Io(e)
    }
}
