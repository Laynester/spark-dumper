//! Lingo bytecode disassembler for Director scripts (D4+ V4 bytecodes).
//!
//! The LSCR chunk contains compiled Lingo bytecode organized by handler.
//! Each handler has a name (an index into the Lnam table), a bytecode offset
//! and length. The bytecode uses the V4 instruction set.
//!
//! Format reference: LibreShockwave cpp/src/chunks/ScriptChunk.cpp (argument
//! widths: op >= 0xC0 → 4 bytes, >= 0x80 → 2 bytes, >= 0x40 → 1 byte) and
//! ScummVM lingo-bytecode.cpp (lingoV4[] opcode table).

use crate::{lscr, ParseError};

// ---------------------------------------------------------------------------
// Opcode enum (semantic base opcodes, LibreShockwave lingo/Opcode.hpp)
// ---------------------------------------------------------------------------

/// Semantic Lingo opcodes. The raw bytecode opcode is `0x40 + (raw % 0x40)`
/// for raw >= 0x40 (the top bits widen the argument: 0x80 → 2 bytes,
/// 0xC0 → 4 bytes); raw < 0x40 map directly. Values match
/// LibreShockwave lingo::Opcode so the decompiler port stays 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Opcode {
    Invalid = 0x00,
    Ret = 0x01,
    RetFactory = 0x02,
    PushZero = 0x03,
    Mul = 0x04,
    Add = 0x05,
    Sub = 0x06,
    Div = 0x07,
    Mod = 0x08,
    Inv = 0x09,
    JoinStr = 0x0a,
    JoinPadStr = 0x0b,
    Lt = 0x0c,
    LtEq = 0x0d,
    NtEq = 0x0e,
    Eq = 0x0f,
    Gt = 0x10,
    GtEq = 0x11,
    And = 0x12,
    Or = 0x13,
    Not = 0x14,
    ContainsStr = 0x15,
    Contains0Str = 0x16,
    GetChunk = 0x17,
    HiliteChunk = 0x18,
    OntoSpr = 0x19,
    IntoSpr = 0x1a,
    GetField = 0x1b,
    StartTell = 0x1c,
    EndTell = 0x1d,
    PushList = 0x1e,
    PushPropList = 0x1f,
    Swap = 0x21,
    CallJavaScript = 0x26,
    PushInt8 = 0x41,
    PushArgListNoRet = 0x42,
    PushArgList = 0x43,
    PushCons = 0x44,
    PushSymb = 0x45,
    PushVarRef = 0x46,
    GetGlobal2 = 0x48,
    GetGlobal = 0x49,
    GetProp = 0x4a,
    GetParam = 0x4b,
    GetLocal = 0x4c,
    SetGlobal2 = 0x4e,
    SetGlobal = 0x4f,
    SetProp = 0x50,
    SetParam = 0x51,
    SetLocal = 0x52,
    Jmp = 0x53,
    EndRepeat = 0x54,
    JmpIfZ = 0x55,
    LocalCall = 0x56,
    ExtCall = 0x57,
    ObjCallV4 = 0x58,
    Put = 0x59,
    PutChunk = 0x5a,
    DeleteChunk = 0x5b,
    Get = 0x5c,
    Set = 0x5d,
    GetMovieProp = 0x5f,
    SetMovieProp = 0x60,
    GetObjProp = 0x61,
    SetObjProp = 0x62,
    TellCall = 0x63,
    Peek = 0x64,
    Pop = 0x65,
    TheBuiltin = 0x66,
    ObjCall = 0x67,
    PushChunkVarRef = 0x6d,
    PushInt16 = 0x6e,
    PushInt32 = 0x6f,
    GetChainedProp = 0x70,
    PushFloat32 = 0x71,
    GetTopLevelProp = 0x72,
    NewObj = 0x73,
}

impl Opcode {
    /// Map a raw bytecode byte to its semantic opcode (LibreShockwave
    /// opcodeFromCode: raw >= 0x40 → 0x40 + raw % 0x40).
    pub fn from_raw(raw: u8) -> Opcode {
        let base = if raw >= 0x40 { 0x40 + raw % 0x40 } else { raw };
        match base {
            0x00 => Opcode::Invalid,
            0x01 => Opcode::Ret,
            0x02 => Opcode::RetFactory,
            0x03 => Opcode::PushZero,
            0x04 => Opcode::Mul,
            0x05 => Opcode::Add,
            0x06 => Opcode::Sub,
            0x07 => Opcode::Div,
            0x08 => Opcode::Mod,
            0x09 => Opcode::Inv,
            0x0a => Opcode::JoinStr,
            0x0b => Opcode::JoinPadStr,
            0x0c => Opcode::Lt,
            0x0d => Opcode::LtEq,
            0x0e => Opcode::NtEq,
            0x0f => Opcode::Eq,
            0x10 => Opcode::Gt,
            0x11 => Opcode::GtEq,
            0x12 => Opcode::And,
            0x13 => Opcode::Or,
            0x14 => Opcode::Not,
            0x15 => Opcode::ContainsStr,
            0x16 => Opcode::Contains0Str,
            0x17 => Opcode::GetChunk,
            0x18 => Opcode::HiliteChunk,
            0x19 => Opcode::OntoSpr,
            0x1a => Opcode::IntoSpr,
            0x1b => Opcode::GetField,
            0x1c => Opcode::StartTell,
            0x1d => Opcode::EndTell,
            0x1e => Opcode::PushList,
            0x1f => Opcode::PushPropList,
            0x21 => Opcode::Swap,
            0x26 => Opcode::CallJavaScript,
            0x41 => Opcode::PushInt8,
            0x42 => Opcode::PushArgListNoRet,
            0x43 => Opcode::PushArgList,
            0x44 => Opcode::PushCons,
            0x45 => Opcode::PushSymb,
            0x46 => Opcode::PushVarRef,
            0x48 => Opcode::GetGlobal2,
            0x49 => Opcode::GetGlobal,
            0x4a => Opcode::GetProp,
            0x4b => Opcode::GetParam,
            0x4c => Opcode::GetLocal,
            0x4e => Opcode::SetGlobal2,
            0x4f => Opcode::SetGlobal,
            0x50 => Opcode::SetProp,
            0x51 => Opcode::SetParam,
            0x52 => Opcode::SetLocal,
            0x53 => Opcode::Jmp,
            0x54 => Opcode::EndRepeat,
            0x55 => Opcode::JmpIfZ,
            0x56 => Opcode::LocalCall,
            0x57 => Opcode::ExtCall,
            0x58 => Opcode::ObjCallV4,
            0x59 => Opcode::Put,
            0x5a => Opcode::PutChunk,
            0x5b => Opcode::DeleteChunk,
            0x5c => Opcode::Get,
            0x5d => Opcode::Set,
            0x5f => Opcode::GetMovieProp,
            0x60 => Opcode::SetMovieProp,
            0x61 => Opcode::GetObjProp,
            0x62 => Opcode::SetObjProp,
            0x63 => Opcode::TellCall,
            0x64 => Opcode::Peek,
            0x65 => Opcode::Pop,
            0x66 => Opcode::TheBuiltin,
            0x67 => Opcode::ObjCall,
            0x6d => Opcode::PushChunkVarRef,
            0x6e => Opcode::PushInt16,
            0x6f => Opcode::PushInt32,
            0x70 => Opcode::GetChainedProp,
            0x71 => Opcode::PushFloat32,
            0x72 => Opcode::GetTopLevelProp,
            0x73 => Opcode::NewObj,
            _ => Opcode::Invalid,
        }
    }

    /// LibreShockwave lingo::mnemonic (used for error comments).
    pub fn mnemonic(self) -> &'static str {
        match self {
            Opcode::Invalid => "invalid",
            Opcode::Ret => "ret",
            Opcode::RetFactory => "retFactory",
            Opcode::PushZero => "pushZero",
            Opcode::Mul => "mul",
            Opcode::Add => "add",
            Opcode::Sub => "sub",
            Opcode::Div => "div",
            Opcode::Mod => "mod",
            Opcode::Inv => "inv",
            Opcode::JoinStr => "joinStr",
            Opcode::JoinPadStr => "joinPadStr",
            Opcode::Lt => "lt",
            Opcode::LtEq => "ltEq",
            Opcode::NtEq => "ntEq",
            Opcode::Eq => "eq",
            Opcode::Gt => "gt",
            Opcode::GtEq => "gtEq",
            Opcode::And => "and",
            Opcode::Or => "or",
            Opcode::Not => "not",
            Opcode::ContainsStr => "containsStr",
            Opcode::Contains0Str => "contains0Str",
            Opcode::GetChunk => "getChunk",
            Opcode::HiliteChunk => "hiliteChunk",
            Opcode::OntoSpr => "ontoSpr",
            Opcode::IntoSpr => "intoSpr",
            Opcode::GetField => "getField",
            Opcode::StartTell => "startTell",
            Opcode::EndTell => "endTell",
            Opcode::PushList => "pushList",
            Opcode::PushPropList => "pushPropList",
            Opcode::Swap => "swap",
            Opcode::CallJavaScript => "callJavaScript",
            Opcode::PushInt8 => "pushInt8",
            Opcode::PushArgListNoRet => "pushArgListNoRet",
            Opcode::PushArgList => "pushArgList",
            Opcode::PushCons => "pushCons",
            Opcode::PushSymb => "pushSymb",
            Opcode::PushVarRef => "pushVarRef",
            Opcode::GetGlobal2 => "getGlobal2",
            Opcode::GetGlobal => "getGlobal",
            Opcode::GetProp => "getProp",
            Opcode::GetParam => "getParam",
            Opcode::GetLocal => "getLocal",
            Opcode::SetGlobal2 => "setGlobal2",
            Opcode::SetGlobal => "setGlobal",
            Opcode::SetProp => "setProp",
            Opcode::SetParam => "setParam",
            Opcode::SetLocal => "setLocal",
            Opcode::Jmp => "jmp",
            Opcode::EndRepeat => "endRepeat",
            Opcode::JmpIfZ => "jmpIfZ",
            Opcode::LocalCall => "localCall",
            Opcode::ExtCall => "extCall",
            Opcode::ObjCallV4 => "objCallV4",
            Opcode::Put => "put",
            Opcode::PutChunk => "putChunk",
            Opcode::DeleteChunk => "deleteChunk",
            Opcode::Get => "get",
            Opcode::Set => "set",
            Opcode::GetMovieProp => "getMovieProp",
            Opcode::SetMovieProp => "setMovieProp",
            Opcode::GetObjProp => "getObjProp",
            Opcode::SetObjProp => "setObjProp",
            Opcode::TellCall => "tellCall",
            Opcode::Peek => "peek",
            Opcode::Pop => "pop",
            Opcode::TheBuiltin => "theBuiltin",
            Opcode::ObjCall => "objCall",
            Opcode::PushChunkVarRef => "pushChunkVarRef",
            Opcode::PushInt16 => "pushInt16",
            Opcode::PushInt32 => "pushInt32",
            Opcode::GetChainedProp => "getChainedProp",
            Opcode::PushFloat32 => "pushFloat32",
            Opcode::GetTopLevelProp => "getTopLevelProp",
            Opcode::NewObj => "newObj",
        }
    }
}

/// A disassembled Lingo handler.
#[derive(Debug, Clone)]
pub struct DisassembledHandler {
    pub name: String,
    pub bytecode_offset: u32,
    pub bytecode_size: u32,
    pub instructions: Vec<String>,
}

/// A fully disassembled Lingo script.
#[derive(Debug, Clone)]
pub struct DisassembledScript {
    pub handlers: Vec<DisassembledHandler>,
    pub raw_data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// V4 opcode table (base opcodes; the 0x80/0xC0 prefixes widen the argument)
// ---------------------------------------------------------------------------

struct V4Op {
    opcode: u8,
    name: &'static str,
    proto: &'static str,
}

const V4_OPCODES: &[V4Op] = &[
    V4Op { opcode: 0x01, name: "procret", proto: "" },
    V4Op { opcode: 0x02, name: "procret", proto: "" },
    V4Op { opcode: 0x03, name: "zeropush", proto: "" },
    V4Op { opcode: 0x04, name: "mul", proto: "" },
    V4Op { opcode: 0x05, name: "add", proto: "" },
    V4Op { opcode: 0x06, name: "sub", proto: "" },
    V4Op { opcode: 0x07, name: "div", proto: "" },
    V4Op { opcode: 0x08, name: "mod", proto: "" },
    V4Op { opcode: 0x09, name: "negate", proto: "" },
    V4Op { opcode: 0x0a, name: "ampersand", proto: "" },
    V4Op { opcode: 0x0b, name: "concat", proto: "" },
    V4Op { opcode: 0x0c, name: "lt", proto: "" },
    V4Op { opcode: 0x0d, name: "le", proto: "" },
    V4Op { opcode: 0x0e, name: "neq", proto: "" },
    V4Op { opcode: 0x0f, name: "eq", proto: "" },
    V4Op { opcode: 0x10, name: "gt", proto: "" },
    V4Op { opcode: 0x11, name: "ge", proto: "" },
    V4Op { opcode: 0x12, name: "and", proto: "" },
    V4Op { opcode: 0x13, name: "or", proto: "" },
    V4Op { opcode: 0x14, name: "not", proto: "" },
    V4Op { opcode: 0x15, name: "contains", proto: "" },
    V4Op { opcode: 0x16, name: "starts", proto: "" },
    V4Op { opcode: 0x17, name: "of", proto: "" },
    V4Op { opcode: 0x18, name: "hilite", proto: "" },
    V4Op { opcode: 0x19, name: "intersects", proto: "" },
    V4Op { opcode: 0x1a, name: "within", proto: "" },
    V4Op { opcode: 0x1b, name: "field", proto: "" },
    V4Op { opcode: 0x1c, name: "tell", proto: "" },
    V4Op { opcode: 0x1d, name: "telldone", proto: "" },
    V4Op { opcode: 0x1e, name: "list", proto: "" },
    V4Op { opcode: 0x1f, name: "proplist", proto: "" },
    V4Op { opcode: 0x41, name: "intpush", proto: "v" },
    V4Op { opcode: 0x42, name: "argcnoretpush", proto: "v" },
    V4Op { opcode: 0x43, name: "argcpush", proto: "v" },
    V4Op { opcode: 0x45, name: "namepush", proto: "n" },
    V4Op { opcode: 0x46, name: "varrefpush", proto: "n" },
    V4Op { opcode: 0x48, name: "globalpush", proto: "n" },
    V4Op { opcode: 0x49, name: "globalpush", proto: "n" },
    V4Op { opcode: 0x4a, name: "thepush", proto: "n" },
    V4Op { opcode: 0x4b, name: "varpush", proto: "pn" },
    V4Op { opcode: 0x4c, name: "varpush", proto: "pn" },
    V4Op { opcode: 0x4e, name: "globalassign", proto: "n" },
    V4Op { opcode: 0x4f, name: "globalassign", proto: "n" },
    V4Op { opcode: 0x50, name: "theassign", proto: "n" },
    V4Op { opcode: 0x51, name: "varassign", proto: "pn" },
    V4Op { opcode: 0x52, name: "varassign", proto: "pn" },
    V4Op { opcode: 0x53, name: "jump", proto: "j" },
    V4Op { opcode: 0x54, name: "jump", proto: "jn" },
    V4Op { opcode: 0x55, name: "jumpifz", proto: "j" },
    V4Op { opcode: 0x56, name: "localcall", proto: "v" },
    V4Op { opcode: 0x57, name: "call", proto: "vN" },
    V4Op { opcode: 0x58, name: "objectcall", proto: "v" },
    V4Op { opcode: 0x59, name: "v4assign", proto: "v" },
    V4Op { opcode: 0x5a, name: "v4assign2", proto: "v" },
    V4Op { opcode: 0x5b, name: "delete", proto: "v" },
    V4Op { opcode: 0x5c, name: "theentitypush", proto: "v" },
    V4Op { opcode: 0x5d, name: "theentityassign", proto: "v" },
    V4Op { opcode: 0x5f, name: "thepush2", proto: "n" },
    V4Op { opcode: 0x60, name: "theassign2", proto: "n" },
    V4Op { opcode: 0x61, name: "objectfieldpush", proto: "n" },
    V4Op { opcode: 0x62, name: "objectfieldassign", proto: "n" },
    V4Op { opcode: 0x63, name: "tellcall", proto: "n" },
    V4Op { opcode: 0x64, name: "stackpeek", proto: "v" },
    V4Op { opcode: 0x65, name: "stackdrop", proto: "v" },
    V4Op { opcode: 0x66, name: "theentitynamepush", proto: "n" },
    V4Op { opcode: 0x67, name: "objcall", proto: "vN" },
    V4Op { opcode: 0x6b, name: "getglobal", proto: "v" },
    V4Op { opcode: 0x6c, name: "getparam", proto: "v" },
    V4Op { opcode: 0x6d, name: "getlocal", proto: "v" },
    V4Op { opcode: 0x6e, name: "setglobal", proto: "v" },
    V4Op { opcode: 0x6f, name: "setparam", proto: "v" },
    V4Op { opcode: 0x70, name: "setlocal", proto: "v" },
    V4Op { opcode: 0x71, name: "jump", proto: "j" },
    V4Op { opcode: 0x72, name: "jumpifz", proto: "j" },
    V4Op { opcode: 0x73, name: "pushint16", proto: "w" },
    V4Op { opcode: 0x74, name: "localcall", proto: "w" },
    V4Op { opcode: 0x75, name: "call", proto: "wN" },
    V4Op { opcode: 0x76, name: "objcall", proto: "wN" },
    V4Op { opcode: 0x77, name: "pushfloat32", proto: "w" },
    V4Op { opcode: 0x78, name: "thepush2", proto: "n" },
    V4Op { opcode: 0x79, name: "theassign2", proto: "n" },
    V4Op { opcode: 0x7a, name: "objectfieldpush", proto: "n" },
    V4Op { opcode: 0x7b, name: "objectfieldassign", proto: "n" },
    V4Op { opcode: 0x7c, name: "objcall", proto: "n" },
    V4Op { opcode: 0x7d, name: "newobj", proto: "n" },
    V4Op { opcode: 0x7e, name: "theentitynamepush", proto: "n" },
    V4Op { opcode: 0x7f, name: "theentityassign", proto: "n" },
];

// ---------------------------------------------------------------------------
// Disassembler
// ---------------------------------------------------------------------------

fn build_opcode_map() -> [Option<&'static V4Op>; 256] {
    let mut map: [Option<&'static V4Op>; 256] = [None; 256];
    for op in V4_OPCODES {
        map[op.opcode as usize] = Some(op);
    }
    map
}

/// Resolve a raw opcode byte to its semantic base entry.
///   op <  0x40: single-byte opcode (no argument)
///   op >= 0x40: argument widths widen with the top bits; the semantic opcode
///               is `0x40 + (op - 0x40) % 0x40` (LibreShockwave).
fn base_op(map: &[Option<&'static V4Op>; 256], opcode: u8) -> Option<&'static V4Op> {
    if opcode < 0x40 {
        return map[opcode as usize];
    }
    let base = 0x40 + ((opcode - 0x40) % 0x40);
    map[base as usize]
}

fn arg_width(opcode: u8) -> usize {
    match opcode {
        0x40..=0x7f => 1,
        0x80..=0xbf => 2,
        0xc0..=0xff => 4,
        _ => 0,
    }
}

fn read_arg(data: &[u8], pos: usize, width: usize) -> (i64, usize) {
    let mut val: i64 = 0;
    let mut end = pos;
    for _ in 0..width {
        if end >= data.len() {
            break;
        }
        val = (val << 8) | data[end] as i64;
        end += 1;
    }
    // Sign-extend per width.
    let val = match width {
        1 => (val as u8) as i8 as i64,
        2 => (val as u16) as i16 as i64,
        _ => val as i32 as i64,
    };
    (val, end)
}

/// Disassemble one V4 instruction at `offset` inside `data` (the full LSCR
/// chunk data). Returns (text, next_offset) or an error.
fn disassemble_at(
    data: &[u8],
    offset: usize,
    map: &[Option<&'static V4Op>; 256],
) -> Result<(String, usize), String> {
    if offset >= data.len() {
        return Err("offset past end".into());
    }
    let opcode = data[offset];
    let mut pos = offset + 1;

    // Inline string constants (0x44 / 0x84): null-terminated, then 4-aligned.
    if opcode == 0x44 || opcode == 0x84 {
        let start = pos;
        while pos < data.len() && data[pos] != 0 {
            pos += 1;
        }
        let s = if pos > start {
            String::from_utf8_lossy(&data[start..pos]).to_string()
        } else {
            String::new()
        };
        if pos < data.len() {
            pos += 1;
        }
        let aligned = (pos - offset + 3) & !3;
        pos = offset + aligned;
        return Ok((format!("push \"{s}\""), pos));
    }

    let width = arg_width(opcode);
    let Some(op) = base_op(map, opcode) else {
        return Ok((format!("<unknown 0x{opcode:02x}>"), pos));
    };

    let mut args = String::new();
    if width > 0 {
        let (val, next) = read_arg(data, pos, width);
        pos = next;
        args.push_str(&format!("{val}"));
    } else if op.proto.contains('N') {
        // call/objcall variants with an inline name string.
        let start = pos;
        while pos < data.len() && data[pos] != 0 {
            pos += 1;
        }
        let s = if pos > start {
            String::from_utf8_lossy(&data[start..pos]).to_string()
        } else {
            String::new()
        };
        if pos < data.len() {
            pos += 1;
        }
        let aligned = (pos - offset + 3) & !3;
        pos = offset + aligned;
        args.push_str(&format!("\"{s}\""));
    }

    let text = if args.is_empty() {
        op.name.to_string()
    } else {
        format!("{} {}", op.name, args)
    };
    Ok((text, pos))
}

/// Disassemble a parsed Lscr script, resolving handler names via the Lnam
/// table. `data` must be the script's `raw_data` (handler bytecode offsets are
/// relative to its start).
pub fn disassemble_script(script: &lscr::Script, names: Option<&lscr::ScriptNames>) -> DisassembledScript {
    let data = &script.raw_data;
    let opcode_map = build_opcode_map();
    let name_of = |id: i16| -> String {
        if id < 0 {
            return format!("handler_{id}");
        }
        match names {
            Some(n) => n.name(id as i32).map(|s| s.to_string())
                .unwrap_or_else(|| format!("handler_{id}")),
            None => format!("handler_{id}"),
        }
    };

    let mut dis_handlers = Vec::new();
    for h in &script.handlers {
        let offset = h.bytecode_offset as usize;
        let size = h.bytecode_len as usize;
        let end = (offset + size).min(data.len());

        let mut instructions = Vec::new();
        if offset < data.len() {
            let mut pc = offset;
            while pc < end {
                match disassemble_at(data, pc, &opcode_map) {
                    Ok((instr, next)) => {
                        instructions.push(format!("  {:5}: {}", pc - offset, instr));
                        if next <= pc {
                            break;
                        }
                        pc = next;
                    }
                    Err(e) => {
                        instructions.push(format!("  {:5}: <error: {e}>", pc - offset));
                        break;
                    }
                }
            }
        }

        dis_handlers.push(DisassembledHandler {
            name: name_of(h.name_id),
            bytecode_offset: h.bytecode_offset,
            bytecode_size: h.bytecode_len,
            instructions,
        });
    }

    DisassembledScript {
        handlers: dis_handlers,
        raw_data: data.clone(),
    }
}

/// Convenience: disassemble an Lscr chunk with an optional Lnam table.
pub fn disassemble_lscr(
    chunk: &director_rifx::Chunk,
    names: Option<&lscr::ScriptNames>,
) -> Result<DisassembledScript, ParseError> {
    let script = lscr::read_script(chunk, true)?;
    Ok(disassemble_script(&script, names))
}

/// Format a disassembled script as human-readable text.
pub fn format_script(script: &DisassembledScript) -> String {
    let mut out = String::new();
    for h in &script.handlers {
        out.push_str(&format!("on {}\n", h.name));
        for instr in &h.instructions {
            out.push_str(instr);
            out.push('\n');
        }
        out.push_str("end\n\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disassemble_simple() {
        // push 5 (0x41 0x05), push 10 (0x41 0x0a), add (0x05), procret (0x01)
        let data = vec![
            0x41, 0x05,  // intpush(5)
            0x41, 0x0a,  // intpush(10)
            0x05,        // add
            0x01,        // procret
        ];
        let map = build_opcode_map();
        let mut pc = 0;
        let mut instrs = Vec::new();
        while pc < data.len() {
            if let Ok((instr, next)) = disassemble_at(&data, pc, &map) {
                instrs.push(instr);
                pc = next;
            } else {
                break;
            }
        }
        assert_eq!(instrs.len(), 4);
        assert!(instrs[0].contains("intpush"));
        assert!(instrs[1].contains("intpush"));
        assert_eq!(instrs[2], "add");
        assert_eq!(instrs[3], "procret");
    }

    #[test]
    fn test_wide_opcode_2byte_arg() {
        // 0x73 = pushint16 with a 2-byte arg (0x81 = 0x40+0x41? no — 0x73 IS
        // the 2-byte form; check a raw 0x80-prefixed op instead):
        // 0x8a = thepush with 2-byte name id
        let data = vec![0x8a, 0x00, 0x07];
        let map = build_opcode_map();
        let (instr, next) = disassemble_at(&data, 0, &map).unwrap();
        assert_eq!(instr, "thepush 7");
        assert_eq!(next, 3);
    }

    #[test]
    fn test_4byte_arg() {
        // 0xc1 = intpush with 4-byte arg (0x40 + 1)
        let data = vec![0xc1, 0x00, 0x00, 0x00, 0x05];
        let map = build_opcode_map();
        let (instr, next) = disassemble_at(&data, 0, &map).unwrap();
        assert_eq!(instr, "intpush 5");
        assert_eq!(next, 5);
    }
}
