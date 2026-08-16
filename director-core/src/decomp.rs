//! Lingo decompiler: LSCR bytecode → readable Lingo source.
//!
//! Faithful port of LibreShockwave's decompiler
//! (cpp/src/lingo/decompiler/LingoDecompiler.cpp + LingoNode.cpp +
//! LingoProperties.cpp). The stack machine is simulated with an expression
//! tree; jumps are structured into if/else, case, and repeat-while / repeat
//! with-in / repeat with-to loops; names resolve through the Lnam table and
//! the per-handler arg/local name-id tables.
//!
//! Output matches LibreShockwave's CastExporter `.ls` files byte-for-byte for
//! the same input (dot-syntax on for D5+ files, EMPTY/ENTER/TAB/RETURN/
//! QUOTE/PI/SPACE/VOID constants, `set x to y` when forced, chunk refs like
//! `char 1 of str`, proplists as `[#a: 1]`, and so on).

use crate::{lscr, lingo::Opcode};
use std::cell::RefCell;
use std::rc::Rc;

type RcCell<T> = Rc<RefCell<T>>;

// ---------------------------------------------------------------------------
// Loop / case tagging constants (LingoDecompiler.cpp)
// ---------------------------------------------------------------------------

const TAG_NONE: u8 = 0;
const TAG_SKIP: u8 = 1;
const TAG_REPEAT_WHILE: u8 = 2;
const TAG_REPEAT_WITH_IN: u8 = 3;
const TAG_REPEAT_WITH_TO: u8 = 4;
const TAG_REPEAT_WITH_DOWNTO: u8 = 5;
const TAG_NEXT_REPEAT_TARGET: u8 = 6;

const EXPECT_POP: u8 = 0;
const EXPECT_OR: u8 = 1;
const EXPECT_NEXT: u8 = 2;
const EXPECT_OTHERWISE: u8 = 3;

// ---------------------------------------------------------------------------
// Expression tree (LingoNode.hpp)
// ---------------------------------------------------------------------------

/// Value type of a literal-ish node (LibreShockwave ValueType).
#[derive(Clone, Copy, PartialEq, Eq)]
enum VType {
    Void,
    Symbol,
    VarRef,
    Str,
    Int,
    Float,
    List,
    ArgList,
    ArgListNoRet,
    PropList,
}

/// A literal value node (LibreShockwave LiteralNode): typed value + items.
#[derive(Clone)]
struct Lit {
    vtype: VType,
    int: i32,
    float: f64,
    string: String,
    items: Vec<Expr>,
}

impl Lit {
    fn int(v: i32) -> Expr {
        Expr::Lit(Lit { vtype: VType::Int, int: v, float: 0.0, string: String::new(), items: Vec::new() })
    }
    fn float(v: f64) -> Expr {
        Expr::Lit(Lit { vtype: VType::Float, int: 0, float: v, string: String::new(), items: Vec::new() })
    }
    fn str(s: impl Into<String>) -> Expr {
        Expr::Lit(Lit { vtype: VType::Str, int: 0, float: 0.0, string: s.into(), items: Vec::new() })
    }
    fn sym(s: impl Into<String>) -> Expr {
        Expr::Lit(Lit { vtype: VType::Symbol, int: 0, float: 0.0, string: s.into(), items: Vec::new() })
    }
    fn varref(s: impl Into<String>) -> Expr {
        Expr::Lit(Lit { vtype: VType::VarRef, int: 0, float: 0.0, string: s.into(), items: Vec::new() })
    }
    fn args(items: Vec<Expr>, noret: bool) -> Expr {
        Expr::Lit(Lit {
            vtype: if noret { VType::ArgListNoRet } else { VType::ArgList },
            int: 0,
            float: 0.0,
            string: String::new(),
            items,
        })
    }
}

/// Expression / simple-statement nodes. Block-owning statements (if, case,
/// loops, tell) live in `Stmt` as `RcCell` nodes so the translator can mutate
/// them (end positions, case chains, else blocks) while blocks are nested.
#[derive(Clone)]
enum Expr {
    Err,
    Lit(Lit),
    Var(String),
    Inverse(Box<Expr>),
    Not(Box<Expr>),
    Bin(Opcode, Box<Expr>, Box<Expr>),
    The(String),
    Member { member_type: String, member_id: Box<Expr>, cast_id: Option<Box<Expr>> },
    ObjProp { obj: Box<Expr>, prop: String },
    ObjBracket { obj: Box<Expr>, prop: Box<Expr> },
    ObjPropIndex { obj: Box<Expr>, prop: String, index: Box<Expr>, index2: Option<Box<Expr>> },
    TheProp { obj: Box<Expr>, prop: String },
    ChunkExpr { chunk_type: u8, first: Box<Expr>, last: Box<Expr>, string: Box<Expr> },
    LastStringChunk { chunk_type: u8, string: Box<Expr> },
    StringChunkCount { chunk_type: u8, string: Box<Expr> },
    SpriteIntersects { a: Box<Expr>, b: Box<Expr> },
    SpriteWithin { a: Box<Expr>, b: Box<Expr> },
    MenuProp { menu: Box<Expr>, prop: u8 },
    MenuItemProp { menu: Box<Expr>, item: Box<Expr>, prop: u8 },
    SoundProp { sound: Box<Expr>, prop: u8 },
    SpriteProp { sprite: Box<Expr>, prop: u8 },
    NewObj { obj_type: String, args: Box<Expr> },
    Call { name: String, args: Vec<Expr> },
    ObjCall { name: String, args: Vec<Expr> },
    ObjCallV4 { obj: Box<Expr>, args: Vec<Expr> },
}

/// Statement nodes added to a block.
#[derive(Clone)]
enum Stmt {
    /// An expression used as a statement (a no-return call).
    Expr(Expr),
    Exit,
    ExitRepeat,
    NextRepeat,
    Return(Box<Expr>),
    Assign { var: Box<Expr>, val: Box<Expr>, force_verbose: bool },
    Put { put_type: u8, var: Box<Expr>, val: Box<Expr> },
    Hilite(Box<Expr>),
    Delete(Box<Expr>),
    When { event: u8, script: String },
    Call { name: String, args: Vec<Expr> },
    If(RcCell<IfNode>),
    Cases(RcCell<CasesNode>),
    Loop(RcCell<LoopNode>),
    Tell(RcCell<TellNode>),
    Comment(String),
}

struct IfNode {
    cond: Expr,
    true_block: RcCell<Block>,
    false_block: RcCell<Block>,
    has_else: bool,
}

struct CasesNode {
    value: Expr,
    first_case: RcCell<CaseNode>,
    end_pos: i32,
}

struct CaseNode {
    value: Expr,
    expect: u8,
    next_or: Option<RcCell<CaseNode>>,
    next_case: Option<RcCell<CaseNode>>,
    block: Option<RcCell<Block>>,
    otherwise: Option<RcCell<Block>>,
    /// The CasesNode this case belongs to (set for the chain's first case;
    /// later cases copy it from their predecessor). Mirrors LibreShockwave's
    /// ancestorStatement lookup — no global "current cases" state, so nested
    /// cases inside a case body cannot clobber the outer chain.
    cases: Option<RcCell<CasesNode>>,
}

enum LoopKind {
    While { cond: Expr },
    WithIn { var: String, list: Expr },
    WithTo { var: String, start: Expr, up: bool, end: Expr },
}

struct LoopNode {
    start_index: i32,
    kind: LoopKind,
    block: RcCell<Block>,
}

struct TellNode {
    window: Expr,
    block: RcCell<Block>,
}

#[derive(Default)]
struct Block {
    children: Vec<Stmt>,
    end_pos: i32,
    current_case: Option<RcCell<CaseNode>>,
}

impl Block {
    fn new() -> RcCell<Block> {
        Rc::new(RefCell::new(Block::default()))
    }
}

/// Who owns each open block level (mirrors LingoNode parent pointers; used by
/// ancestorStatement / ancestorLoop during translation).
#[derive(Clone)]
enum Owner {
    Root,
    If(RcCell<IfNode>, bool),
    Cases(RcCell<CasesNode>),
    Loop(RcCell<LoopNode>),
    Tell,
}

/// A translation result: an expression (pushed on the value stack) or a
/// statement (added to the current block).
enum Trans {
    Expr(Expr),
    Stmt(Stmt),
}

// ---------------------------------------------------------------------------
// Property name tables (LingoProperties.cpp)
// ---------------------------------------------------------------------------

fn binary_op_name(op: Opcode) -> &'static str {
    match op {
        Opcode::Mul => "*",
        Opcode::Add => "+",
        Opcode::Sub => "-",
        Opcode::Div => "/",
        Opcode::Mod => "mod",
        Opcode::JoinStr => "&",
        Opcode::JoinPadStr => "&&",
        Opcode::Lt => "<",
        Opcode::LtEq => "<=",
        Opcode::NtEq => "<>",
        Opcode::Eq => "=",
        Opcode::Gt => ">",
        Opcode::GtEq => ">=",
        Opcode::And => "and",
        Opcode::Or => "or",
        Opcode::ContainsStr => "contains",
        Opcode::Contains0Str => "starts",
        _ => "?",
    }
}

fn chunk_type_name(id: i32) -> &'static str {
    match id {
        1 => "char",
        2 => "word",
        3 => "item",
        4 => "line",
        _ => "chunk",
    }
}

fn put_type_name(id: i32) -> &'static str {
    match id {
        1 => "into",
        2 => "after",
        3 => "before",
        _ => "into",
    }
}

fn movie_property_name(id: i32) -> &'static str {
    match id {
        0x00 => "floatPrecision",
        0x01 => "mouseDownScript",
        0x02 => "mouseUpScript",
        0x03 => "keyDownScript",
        0x04 => "keyUpScript",
        0x05 => "timeoutScript",
        0x06 => "short time",
        0x07 => "abbr time",
        0x08 => "long time",
        0x09 => "short date",
        0x0a => "abbr date",
        0x0b => "long date",
        _ => "ERROR",
    }
}

fn when_event_name(id: i32) -> &'static str {
    match id {
        0x01 => "mouseDown",
        0x02 => "mouseUp",
        0x03 => "keyDown",
        0x04 => "keyUp",
        0x05 => "timeOut",
        _ => "ERROR",
    }
}

fn menu_property_name(id: i32) -> &'static str {
    match id {
        0x01 => "name",
        0x02 => "number of menuItems",
        _ => "ERROR",
    }
}

fn menu_item_property_name(id: i32) -> &'static str {
    match id {
        0x01 => "name",
        0x02 => "checkMark",
        0x03 => "enabled",
        0x04 => "script",
        _ => "ERROR",
    }
}

fn sound_property_name(id: i32) -> &'static str {
    match id {
        0x01 => "volume",
        _ => "ERROR",
    }
}

fn sprite_property_name(id: i32) -> &'static str {
    match id {
        0x01 => "type",
        0x02 => "backColor",
        0x03 => "bottom",
        0x04 => "castNum",
        0x05 => "constraint",
        0x06 => "cursor",
        0x07 => "foreColor",
        0x08 => "height",
        0x09 => "immediate",
        0x0a => "ink",
        0x0b => "left",
        0x0c => "lineSize",
        0x0d => "locH",
        0x0e => "locV",
        0x0f => "movieRate",
        0x10 => "movieTime",
        0x11 => "pattern",
        0x12 => "puppet",
        0x13 => "right",
        0x14 => "startTime",
        0x15 => "stopTime",
        0x16 => "stretch",
        0x17 => "top",
        0x18 => "trails",
        0x19 => "visible",
        0x1a => "volume",
        0x1b => "width",
        0x1c => "blend",
        0x1d => "scriptNum",
        0x1e => "moveableSprite",
        0x1f => "editableText",
        0x20 => "scoreColor",
        0x21 => "loc",
        0x22 => "rect",
        0x23 => "memberNum",
        0x24 => "castLibNum",
        0x25 => "member",
        0x26 => "scriptInstanceList",
        0x27 => "currentTime",
        0x28 => "mostRecentCuePoint",
        0x29 => "tweened",
        0x2a => "name",
        _ => "ERROR",
    }
}

fn animation_property_name(id: i32) -> &'static str {
    match id {
        0x01 => "beepOn",
        0x02 => "buttonStyle",
        0x03 => "centerStage",
        0x04 => "checkBoxAccess",
        0x05 => "checkboxType",
        0x06 => "colorDepth",
        0x07 => "colorQD",
        0x08 => "exitLock",
        0x09 => "fixStageSize",
        0x0a => "fullColorPermit",
        0x0b => "imageDirect",
        0x0c => "doubleClick",
        0x0d => "key",
        0x0e => "lastClick",
        0x0f => "lastEvent",
        0x10 => "keyCode",
        0x11 => "lastKey",
        0x12 => "lastRoll",
        0x13 => "timeoutLapsed",
        0x14 => "multiSound",
        0x15 => "pauseState",
        0x16 => "quickTimePresent",
        0x17 => "selEnd",
        0x18 => "selStart",
        0x19 => "soundEnabled",
        0x1a => "soundLevel",
        0x1b => "stageColor",
        0x1d => "switchColorDepth",
        0x1e => "timeoutKeyDown",
        0x1f => "timeoutLength",
        0x20 => "timeoutMouse",
        0x21 => "timeoutPlay",
        0x22 => "timer",
        0x23 => "preLoadRAM",
        0x24 => "videoForWindowsPresent",
        0x25 => "netPresent",
        0x26 => "safePlayer",
        0x27 => "soundKeepDevice",
        0x28 => "soundMixMedia",
        _ => "ERROR",
    }
}

fn animation2_property_name(id: i32) -> &'static str {
    match id {
        0x01 => "perFrameHook",
        0x02 => "number of castMembers",
        0x03 => "number of menus",
        0x04 => "number of castLibs",
        0x05 => "number of xtras",
        _ => "ERROR",
    }
}

fn member_property_name(id: i32) -> &'static str {
    match id {
        0x01 => "name",
        0x02 => "text",
        0x03 => "textStyle",
        0x04 => "textFont",
        0x05 => "textHeight",
        0x06 => "textAlign",
        0x07 => "textSize",
        0x08 => "picture",
        0x09 => "hilite",
        0x0a => "number",
        0x0b => "size",
        0x0c => "loop",
        0x0d => "duration",
        0x0e => "controller",
        0x0f => "directToStage",
        0x10 => "sound",
        0x11 => "foreColor",
        0x12 => "backColor",
        0x13 => "type",
        _ => "ERROR",
    }
}

// ---------------------------------------------------------------------------
// Emitters (LingoNode.cpp toLingo)
// ---------------------------------------------------------------------------

/// C++ `ostringstream << setprecision(15) << double` — printf %g with 15
/// significant digits (trailing zeros trimmed, scientific for exp < -4 or
/// >= 15, exponent with sign and >= 2 digits).
fn cxx_g15(value: f64) -> String {
    let neg = value < 0.0;
    let v = value.abs();
    if v == 0.0 {
        return if neg { "-0".to_string() } else { "0".to_string() };
    }
    let x = v.log10().floor() as i32;
    let prec: i32 = 15;
    let sign = if neg { "-" } else { "" };
    if x >= -4 && x < prec {
        // Fixed notation with prec-1-x decimals, trailing zeros trimmed.
        let decimals = (prec - 1 - x).max(0) as usize;
        let mut s = format!("{:.*}", decimals, v);
        while s.len() > 1 && s.contains('.') && s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
        format!("{sign}{s}")
    } else {
        // Scientific: d.dddddde±XX with prec-1 decimals.
        let decimals = (prec - 1) as usize;
        let mut mant = format!("{:.*}", decimals, v / 10f64.powi(x));
        while mant.len() > 1 && mant.ends_with('0') {
            mant.pop();
        }
        if mant.ends_with('.') {
            mant.pop();
        }
        let esign = if x < 0 { '-' } else { '+' };
        let eabs = x.abs();
        let etext = if eabs < 10 { format!("0{eabs}") } else { eabs.to_string() };
        format!("{sign}{mant}e{esign}{etext}")
    }
}

/// LibreShockwave formatFloat: %g(15) text, then ensure a decimal point.
fn format_float(value: f64) -> String {
    let text = cxx_g15(value);
    if text.contains('e') || text.contains('E') {
        return text;
    }
    if !text.contains('.') {
        return format!("{text}.0");
    }
    let mut text = text;
    while text.ends_with('0') && text.contains('.') && !text.ends_with(".0") {
        text.pop();
    }
    if text.ends_with('.') {
        text.push('0');
    }
    text
}

fn bin_precedence(op: Opcode) -> i32 {
    match op {
        Opcode::Mul | Opcode::Div | Opcode::Mod => 1,
        Opcode::Add | Opcode::Sub => 2,
        Opcode::Lt | Opcode::LtEq | Opcode::NtEq | Opcode::Eq | Opcode::Gt | Opcode::GtEq => 3,
        Opcode::And => 4,
        Opcode::Or => 5,
        _ => 0,
    }
}

fn expr_vtype(e: &Expr) -> VType {
    match e {
        Expr::Lit(l) => l.vtype,
        _ => VType::Void,
    }
}

fn expr_int_value(e: &Expr) -> i32 {
    match e {
        Expr::Lit(l) => l.int,
        _ => 0,
    }
}

fn expr_string_value(e: &Expr) -> String {
    match e {
        Expr::Lit(l) => l.string.clone(),
        _ => String::new(),
    }
}

fn expr_arg_nodes(e: &Expr) -> &[Expr] {
    match e {
        Expr::Lit(l) => &l.items,
        _ => &[],
    }
}

fn take_arg_nodes(e: &mut Expr) -> Vec<Expr> {
    match e {
        Expr::Lit(l) if matches!(l.vtype, VType::ArgList | VType::ArgListNoRet) => {
            std::mem::take(&mut l.items)
        }
        _ => Vec::new(),
    }
}

fn arglist_noret(e: &Expr) -> bool {
    matches!(e, Expr::Lit(l) if l.vtype == VType::ArgListNoRet)
}

fn is_zero_literal(e: &Expr) -> bool {
    expr_vtype(e) == VType::Int && expr_int_value(e) == 0
}

fn is_int_literal_zero(e: Option<&Expr>) -> bool {
    match e {
        Some(e) => expr_vtype(e) == VType::Int && expr_int_value(e) == 0,
        None => false,
    }
}

fn expr_lingo(e: &Expr, dot: bool) -> String {
    match e {
        Expr::Err => "ERROR".to_string(),
        Expr::Lit(l) => lit_lingo(l, dot),
        Expr::Var(name) => name.clone(),
        Expr::Inverse(a) => {
            if matches!(**a, Expr::Bin(..)) {
                format!("-({})", expr_lingo(a, dot))
            } else {
                format!("-{}", expr_lingo(a, dot))
            }
        }
        Expr::Not(a) => {
            if matches!(**a, Expr::Bin(..)) {
                format!("not ({})", expr_lingo(a, dot))
            } else {
                format!("not {}", expr_lingo(a, dot))
            }
        }
        Expr::Bin(op, l, r) => {
            let mut left = expr_lingo(l, dot);
            let mut right = expr_lingo(r, dot);
            let prec = bin_precedence(*op);
            if prec > 0 {
                if let Expr::Bin(lop, _, _) = &**l {
                    if bin_precedence(*lop) > prec {
                        left = format!("({left})");
                    }
                }
                if let Expr::Bin(rop, _, _) = &**r {
                    if bin_precedence(*rop) >= prec {
                        right = format!("({right})");
                    }
                }
            }
            format!("{left} {} {right}", binary_op_name(*op))
        }
        Expr::The(prop) => format!("the {prop}"),
        Expr::Member { member_type, member_id, cast_id } => {
            let no_cast = cast_id.is_none() || is_int_literal_zero(cast_id.as_deref());
            if no_cast {
                if dot {
                    format!("{member_type}({})", expr_lingo(member_id, dot))
                } else if matches!(**member_id, Expr::Bin(..)) {
                    format!("{member_type} ({})", expr_lingo(member_id, dot))
                } else {
                    format!("{member_type} {}", expr_lingo(member_id, dot))
                }
            } else {
                format!(
                    "{member_type}({}, {})",
                    expr_lingo(member_id, dot),
                    expr_lingo(cast_id.as_ref().unwrap(), dot)
                )
            }
        }
        Expr::ObjProp { obj, prop } => {
            if dot {
                format!("{}.{}", maybe_paren_dot(obj, dot), prop)
            } else {
                format!("the {prop} of {}", expr_lingo(obj, dot))
            }
        }
        Expr::ObjBracket { obj, prop } => {
            format!("{}[{}]", expr_lingo(obj, dot), expr_lingo(prop, dot))
        }
        Expr::ObjPropIndex { obj, prop, index, index2 } => {
            let mut result = format!("{}.{}[", maybe_paren_dot(obj, dot), prop);
            result.push_str(&expr_lingo(index, dot));
            if let Some(i2) = index2 {
                result.push_str("..");
                result.push_str(&expr_lingo(i2, dot));
            }
            result.push(']');
            result
        }
        Expr::TheProp { obj, prop } => format!("the {prop} of {}", expr_lingo(obj, false)),
        Expr::ChunkExpr { chunk_type, first, last, string } => {
            let mut result = format!("{} {}", chunk_type_name(*chunk_type as i32), expr_lingo(first, dot));
            if !is_zero_literal(last) {
                result.push_str(" to ");
                result.push_str(&expr_lingo(last, dot));
            }
            result.push_str(" of ");
            result.push_str(&expr_lingo(string, false));
            result
        }
        Expr::LastStringChunk { chunk_type, string } => {
            format!("the last {} in {}", chunk_type_name(*chunk_type as i32), expr_lingo(string, false))
        }
        Expr::StringChunkCount { chunk_type, string } => {
            format!("the number of {}s in {}", chunk_type_name(*chunk_type as i32), expr_lingo(string, false))
        }
        Expr::SpriteIntersects { a, b } => {
            format!("sprite {} intersects {}", expr_lingo(a, dot), expr_lingo(b, dot))
        }
        Expr::SpriteWithin { a, b } => {
            format!("sprite {} within {}", expr_lingo(a, dot), expr_lingo(b, dot))
        }
        Expr::MenuProp { menu, prop } => {
            format!("the {} of menu {}", menu_property_name(*prop as i32), expr_lingo(menu, dot))
        }
        Expr::MenuItemProp { menu, item, prop } => format!(
            "the {} of menuItem {} of menu {}",
            menu_item_property_name(*prop as i32),
            expr_lingo(item, dot),
            expr_lingo(menu, dot)
        ),
        Expr::SoundProp { sound, prop } => {
            format!("the {} of sound {}", sound_property_name(*prop as i32), expr_lingo(sound, dot))
        }
        Expr::SpriteProp { sprite, prop } => {
            format!("the {} of sprite {}", sprite_property_name(*prop as i32), expr_lingo(sprite, dot))
        }
        Expr::NewObj { obj_type, args } => {
            format!("new {obj_type}({})", expr_lingo(args, dot))
        }
        Expr::Call { name, args } => {
            if args.is_empty() {
                match name.as_str() {
                    "pi" => return "PI".to_string(),
                    "space" => return "SPACE".to_string(),
                    "void" => return "VOID".to_string(),
                    _ => {}
                }
            }
            let inner: Vec<String> = args.iter().map(|a| expr_lingo(a, dot)).collect();
            format!("{name}({})", inner.join(", "))
        }
        Expr::ObjCall { name, args } => {
            if args.is_empty() {
                return format!("???.{name}()");
            }
            let mut result = format!("{}.{name}(", maybe_paren_dot(&args[0], dot));
            for (i, a) in args.iter().enumerate().skip(1) {
                if i > 1 {
                    result.push_str(", ");
                }
                result.push_str(&expr_lingo(a, dot));
            }
            result.push(')');
            result
        }
        Expr::ObjCallV4 { obj, args } => {
            let inner: Vec<String> = args.iter().map(|a| expr_lingo(a, dot)).collect();
            format!("{}({})", expr_lingo(obj, dot), inner.join(", "))
        }
    }
}

fn lit_lingo(l: &Lit, dot: bool) -> String {
    match l.vtype {
        VType::Void => "VOID".to_string(),
        VType::Symbol => format!("#{}", l.string),
        VType::VarRef => l.string.clone(),
        VType::Str => {
            if l.string.is_empty() {
                "EMPTY".to_string()
            } else if l.string.len() == 1 {
                match l.string.as_bytes()[0] {
                    0x03 => "ENTER".to_string(),
                    0x08 => "BACKSPACE".to_string(),
                    0x09 => "TAB".to_string(),
                    0x0d => "RETURN".to_string(),
                    b'"' => "QUOTE".to_string(),
                    _ => format!("\"{}\"", l.string),
                }
            } else {
                format!("\"{}\"", l.string)
            }
        }
        VType::Int => l.int.to_string(),
        VType::Float => format_float(l.float),
        VType::List => {
            let inner: Vec<String> = l.items.iter().map(|i| expr_lingo(i, dot)).collect();
            format!("[{}]", inner.join(", "))
        }
        VType::ArgList | VType::ArgListNoRet => {
            let inner: Vec<String> = l.items.iter().map(|i| expr_lingo(i, dot)).collect();
            inner.join(", ")
        }
        VType::PropList => {
            if l.items.is_empty() {
                return "[:]".to_string();
            }
            let mut result = String::from("[");
            let mut idx = 0usize;
            while idx < l.items.len() {
                if idx > 0 {
                    result.push_str(", ");
                }
                result.push_str(&expr_lingo(&l.items[idx], dot));
                result.push_str(": ");
                if idx + 1 < l.items.len() {
                    result.push_str(&expr_lingo(&l.items[idx + 1], dot));
                }
                idx += 2;
            }
            result.push(']');
            result
        }
    }
}

/// LibreShockwave LingoNode::indent: two spaces per line; an empty trailing
/// line produces a bare newline; a final newline is dropped when the input
/// does not end with one.
fn indent(text: &str) -> String {
    let mut result = String::new();
    let mut start = 0usize;
    loop {
        if start > text.len() {
            break;
        }
        let end = text[start..].find('\n').map(|i| start + i).unwrap_or(text.len());
        let line = &text[start..end];
        if !line.is_empty() {
            result.push_str("  ");
            result.push_str(line);
        }
        result.push('\n');
        if end == text.len() {
            break;
        }
        start = end + 1;
    }
    if !text.is_empty() && !text.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }
    result
}

fn block_lingo(b: &RcCell<Block>, dot: bool) -> String {
    let b = b.borrow();
    let mut result = String::new();
    for child in &b.children {
        result.push_str(&indent(&stmt_lingo(child, dot)));
        result.push('\n');
    }
    result
}

fn stmt_lingo(s: &Stmt, dot: bool) -> String {
    match s {
        Stmt::Expr(e) => expr_lingo(e, dot),
        Stmt::Exit => "exit".to_string(),
        Stmt::ExitRepeat => "exit repeat".to_string(),
        Stmt::NextRepeat => "next repeat".to_string(),
        Stmt::Return(v) => format!("return {}", expr_lingo(v, false)),
        Stmt::Assign { var, val, force_verbose } => {
            if !dot || *force_verbose {
                format!("set {} to {}", expr_lingo(var, false), expr_lingo(val, dot))
            } else {
                format!("{} = {}", expr_lingo(var, dot), expr_lingo(val, dot))
            }
        }
        Stmt::Put { put_type, var, val } => format!(
            "put {} {} {}",
            expr_lingo(val, dot),
            put_type_name(*put_type as i32),
            expr_lingo(var, false)
        ),
        Stmt::Hilite(chunk) => format!("hilite {}", expr_lingo(chunk, dot)),
        Stmt::Delete(chunk) => format!("delete {}", expr_lingo(chunk, dot)),
        Stmt::When { event, script } => when_lingo(*event as i32, script),
        Stmt::Call { name, args } => {
            let inner: Vec<String> = args.iter().map(|a| expr_lingo(a, dot)).collect();
            if name == "put" || name == "return" {
                format!("{name} {}", inner.join(", "))
            } else {
                format!("{name}({})", inner.join(", "))
            }
        }
        Stmt::If(n) => {
            let n = n.borrow();
            let mut result = format!("if {} then\n", expr_lingo(&n.cond, dot));
            result.push_str(&block_lingo(&n.true_block, dot));
            if n.has_else {
                result.push_str("else\n");
                result.push_str(&block_lingo(&n.false_block, dot));
            }
            result.push_str("end if");
            result
        }
        Stmt::Cases(n) => {
            let n = n.borrow();
            let mut result = format!("case {} of\n", expr_lingo(&n.value, dot));
            result.push_str(&indent(&case_lingo(&n.first_case, dot)));
            result.push_str("end case");
            result
        }
        Stmt::Loop(n) => {
            let n = n.borrow();
            match &n.kind {
                LoopKind::While { cond } => {
                    format!("repeat while {}\n{}", expr_lingo(cond, dot), block_lingo(&n.block, dot))
                        + "end repeat"
                }
                LoopKind::WithIn { var, list } => format!(
                    "repeat with {var} in {}\n{}",
                    expr_lingo(list, dot),
                    block_lingo(&n.block, dot)
                ) + "end repeat",
                LoopKind::WithTo { var, start, up, end } => {
                    let direction = if *up { " to " } else { " down to " };
                    format!(
                        "repeat with {var} = {}{direction}{}\n{}",
                        expr_lingo(start, dot),
                        expr_lingo(end, dot),
                        block_lingo(&n.block, dot)
                    ) + "end repeat"
                }
            }
        }
        Stmt::Tell(n) => {
            let n = n.borrow();
            format!("tell {}\n{}", expr_lingo(&n.window, dot), block_lingo(&n.block, dot)) + "end tell"
        }
        Stmt::Comment(text) => format!("-- {text}"),
    }
}

fn when_lingo(event: i32, script: &str) -> String {
    let mut result = format!("when {} then ", when_event_name(event));
    let bytes = script.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() && bytes[index] != b'\r' {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        while index < bytes.len() && bytes[index] != b'\r' {
            result.push(bytes[index] as char);
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        if index < bytes.len() - 1 {
            result.push_str("\n  ");
        }
        index += 1;
    }
    result
}

fn case_lingo(c: &RcCell<CaseNode>, dot: bool) -> String {
    let c = c.borrow();
    let mut result = expr_lingo(&c.value, dot);
    if let Some(nx) = &c.next_or {
        result.push_str(", ");
        result.push_str(&case_lingo(nx, dot));
    } else {
        result.push_str(":\n");
        if let Some(b) = &c.block {
            result.push_str(&block_lingo(b, dot));
        }
    }
    if let Some(nc) = &c.next_case {
        result.push_str(&case_lingo(nc, dot));
    } else if let Some(o) = &c.otherwise {
        result.push_str("otherwise:\n");
        result.push_str(&block_lingo(o, dot));
    }
    result
}

fn handler_lingo(name: &str, args: &[String], block: &RcCell<Block>, dot: bool) -> String {
    let mut result = format!("on {name}");
    if !args.is_empty() {
        result.push(' ');
        result.push_str(&args.join(", "));
    }
    result.push('\n');
    result.push_str(&block_lingo(block, dot));
    result.push_str("end");
    result
}

fn maybe_paren_dot(e: &Expr, dot: bool) -> String {
    let text = expr_lingo(e, dot);
    match e {
        Expr::Var(_)
        | Expr::ObjCall { .. }
        | Expr::ObjCallV4 { .. }
        | Expr::Call { .. }
        | Expr::ObjProp { .. }
        | Expr::ObjBracket { .. }
        | Expr::ObjPropIndex { .. } => text,
        _ => format!("({text})"),
    }
}

// ---------------------------------------------------------------------------
// Decoder (LingoDecompiler.cpp)
// ---------------------------------------------------------------------------

struct Decoder<'a> {
    script: &'a lscr::Script,
    names: Option<&'a lscr::ScriptNames>,
    version: i32,
    capital_x: bool,
    dot: bool,
    // Per-handler state (cloned so helpers never fight the borrow checker).
    instrs: Vec<lscr::Instruction>,
    arg_ids: Vec<i16>,
    local_ids: Vec<i16>,
    tags: Vec<u8>,
    owner_loops: Vec<i32>,
    stack: Vec<Expr>,
    block_stack: Vec<RcCell<Block>>,
    owner_stack: Vec<Owner>,
    last_consumed: usize,
}

impl<'a> Decoder<'a> {
    fn new(script: &'a lscr::Script, names: Option<&'a lscr::ScriptNames>, version: i32, capital_x: bool) -> Decoder<'a> {
        Decoder {
            script,
            names,
            version,
            capital_x,
            dot: version >= 700,
            instrs: Vec::new(),
            arg_ids: Vec::new(),
            local_ids: Vec::new(),
            tags: Vec::new(),
            owner_loops: Vec::new(),
            stack: Vec::new(),
            block_stack: Vec::new(),
            owner_stack: Vec::new(),
            last_consumed: 1,
        }
    }

    // -- names -----------------------------------------------------------

    fn resolve_name(&self, name_id: i32) -> String {
        if let Some(n) = self.names {
            if name_id >= 0 && (name_id as usize) < n.names.len() {
                return n.names[name_id as usize].clone();
            }
        }
        format!("#{name_id}")
    }

    fn variable_multiplier(&self) -> i32 {
        if self.capital_x {
            1
        } else if self.version >= 500 {
            8
        } else {
            6
        }
    }

    fn get_argument_name(&self, raw_index: i32) -> String {
        let index = raw_index / self.variable_multiplier();
        if index >= 0 && (index as usize) < self.arg_ids.len() {
            return self.resolve_name(self.arg_ids[index as usize] as i32);
        }
        format!("UNKNOWN_ARG_{index}")
    }

    fn get_local_name(&self, raw_index: i32) -> String {
        let index = raw_index / self.variable_multiplier();
        if index >= 0 && (index as usize) < self.local_ids.len() {
            return self.resolve_name(self.local_ids[index as usize] as i32);
        }
        format!("UNKNOWN_LOCAL_{index}")
    }

    // -- blocks ----------------------------------------------------------

    fn current_block(&self) -> RcCell<Block> {
        self.block_stack.last().cloned().expect("block stack non-empty")
    }

    fn enter_block(&mut self, owner: Owner, block: RcCell<Block>) {
        self.owner_stack.push(owner);
        self.block_stack.push(block);
    }

    fn exit_block(&mut self) {
        if self.block_stack.is_empty() {
            return;
        }
        self.block_stack.pop();
        self.owner_stack.pop();
    }

    /// Nearest enclosing loop owner (LingoNode::ancestorLoop).
    fn ancestor_loop(&self) -> Option<RcCell<LoopNode>> {
        for owner in self.owner_stack.iter().rev() {
            if let Owner::Loop(l) = owner {
                return Some(l.clone());
            }
        }
        None
    }

    fn pop(&mut self) -> Expr {
        self.stack.pop().unwrap_or(Expr::Err)
    }

    // -- literal / instruction helpers ------------------------------------

    fn instruction_index_for_offset(&self, offset: i32) -> Option<i32> {
        self.instrs.iter().position(|i| i.offset == offset).map(|i| i as i32)
    }

    fn get_var_name_from_set(&self, ins: lscr::Instruction) -> String {
        match ins.opcode {
            Opcode::SetGlobal | Opcode::SetGlobal2 | Opcode::SetProp => self.resolve_name(ins.argument),
            Opcode::SetParam => self.get_argument_name(ins.argument),
            Opcode::SetLocal => self.get_local_name(ins.argument),
            _ => "ERROR".to_string(),
        }
    }

    fn literal_to_node(&self, lit: &lscr::Literal) -> Expr {
        match lit.kind {
            1 => Lit::str(lit.string_value.clone().unwrap_or_default()),
            4 => Lit::int(lit.int_value.unwrap_or(0)),
            9 => Lit::float(lit.float_value.unwrap_or(0.0)),
            _ => Lit::str(self.raw_literal_string(lit)),
        }
    }

    fn raw_literal_string(&self, lit: &lscr::Literal) -> String {
        if let Some(s) = &lit.string_value {
            return s.clone();
        }
        if let Some(v) = lit.int_value {
            return v.to_string();
        }
        if let Some(b) = &lit.bytes {
            return format!("[bytes:{}]", b.len());
        }
        "null".to_string()
    }

    // -- loop tagging -----------------------------------------------------

    fn tag_loops(&mut self) {
        let n = self.instrs.len();
        self.tags = vec![TAG_NONE; n];
        self.owner_loops = vec![-1; n];
        for start in 0..n {
            let jmp = self.instrs[start];
            if jmp.opcode != Opcode::JmpIfZ {
                continue;
            }
            let end_pos = jmp.offset + jmp.argument;
            let Some(end_index) = self.instruction_index_for_offset(end_pos) else {
                continue;
            };
            if end_index < 1 || end_index > n as i32 {
                continue;
            }
            let end_repeat = self.instrs[(end_index - 1) as usize];
            if end_repeat.opcode != Opcode::EndRepeat {
                continue;
            }
            if end_repeat.offset - end_repeat.argument > jmp.offset {
                continue;
            }
            if self.is_repeat_with_in_loop(start, end_index) {
                self.tags[start] = TAG_REPEAT_WITH_IN;
                for idx in (start - 7)..=start - 1 {
                    self.tags[idx] = TAG_SKIP;
                }
                for idx in (start + 1)..=(start + 5) {
                    self.tags[idx] = TAG_SKIP;
                }
                self.tags[(end_index - 3) as usize] = TAG_NEXT_REPEAT_TARGET;
                self.owner_loops[(end_index - 3) as usize] = start as i32;
                self.tags[(end_index - 2) as usize] = TAG_SKIP;
                self.tags[(end_index - 1) as usize] = TAG_SKIP;
                self.owner_loops[(end_index - 1) as usize] = start as i32;
                if (end_index as usize) < self.tags.len() {
                    self.tags[end_index as usize] = TAG_SKIP;
                }
                continue;
            }
            if self.is_repeat_with_to_loop(start, end_index) {
                let end_repeat = self.instrs[(end_index - 1) as usize];
                let Some(cond_start) = self.instruction_index_for_offset(end_repeat.offset - end_repeat.argument) else {
                    continue;
                };
                if cond_start < 1 {
                    continue;
                }
                self.tags[start] = if self.instrs[start - 1].opcode == Opcode::LtEq {
                    TAG_REPEAT_WITH_TO
                } else {
                    TAG_REPEAT_WITH_DOWNTO
                };
                self.tags[(cond_start - 1) as usize] = TAG_SKIP;
                self.tags[cond_start as usize] = TAG_SKIP;
                self.tags[start - 1] = TAG_SKIP;
                self.tags[(end_index - 5) as usize] = TAG_NEXT_REPEAT_TARGET;
                self.owner_loops[(end_index - 5) as usize] = start as i32;
                self.tags[(end_index - 4) as usize] = TAG_SKIP;
                self.tags[(end_index - 3) as usize] = TAG_SKIP;
                self.tags[(end_index - 2) as usize] = TAG_SKIP;
                self.tags[(end_index - 1) as usize] = TAG_SKIP;
                self.owner_loops[(end_index - 1) as usize] = start as i32;
                continue;
            }
            self.tags[start] = TAG_REPEAT_WHILE;
            self.tags[(end_index - 1) as usize] = TAG_NEXT_REPEAT_TARGET;
            self.owner_loops[(end_index - 1) as usize] = start as i32;
        }
    }

    fn is_repeat_with_in_loop(&self, start: usize, end_index: i32) -> bool {
        let instrs = &self.instrs;
        if start < 7 || start + 5 >= instrs.len() || end_index < 3 || end_index as usize >= instrs.len() {
            return false;
        }
        let at = |i: usize| instrs[i];
        at(start - 7).opcode == Opcode::Peek && at(start - 7).argument == 0
            && at(start - 6).opcode == Opcode::PushArgList && at(start - 6).argument == 1
            && at(start - 5).opcode == Opcode::ExtCall && self.resolve_name(at(start - 5).argument) == "count"
            && at(start - 4).opcode == Opcode::PushInt8 && at(start - 4).argument == 1
            && at(start - 3).opcode == Opcode::Peek && at(start - 3).argument == 0
            && at(start - 2).opcode == Opcode::Peek && at(start - 2).argument == 2
            && at(start - 1).opcode == Opcode::LtEq
            && at(start + 1).opcode == Opcode::Peek && at(start + 1).argument == 2
            && at(start + 2).opcode == Opcode::Peek && at(start + 2).argument == 1
            && at(start + 3).opcode == Opcode::PushArgList && at(start + 3).argument == 2
            && at(start + 4).opcode == Opcode::ExtCall && self.resolve_name(at(start + 4).argument) == "getAt"
            && matches!(
                at(start + 5).opcode,
                Opcode::SetGlobal | Opcode::SetProp | Opcode::SetParam | Opcode::SetLocal
            )
            && at((end_index - 3) as usize).opcode == Opcode::PushInt8
            && at((end_index - 3) as usize).argument == 1
            && at((end_index - 2) as usize).opcode == Opcode::Add
            && at(end_index as usize).opcode == Opcode::Pop
            && at(end_index as usize).argument == 3
    }

    fn is_repeat_with_to_loop(&self, start: usize, end_index: i32) -> bool {
        let instrs = &self.instrs;
        if start < 1 || end_index < 5 {
            return false;
        }
        let comp = instrs[start - 1].opcode;
        if comp != Opcode::LtEq && comp != Opcode::GtEq {
            return false;
        }
        let end_repeat = instrs[(end_index - 1) as usize];
        let Some(cond_start) = self.instruction_index_for_offset(end_repeat.offset - end_repeat.argument) else {
            return false;
        };
        if cond_start < 1 || cond_start as usize >= instrs.len() {
            return false;
        }
        let set_op = instrs[(cond_start - 1) as usize].opcode;
        let Some(get_op) = set_opcode_to_get_opcode(set_op) else {
            return false;
        };
        let var_id = instrs[(cond_start - 1) as usize].argument;
        let expected_increment = if comp == Opcode::LtEq { 1 } else { -1 };
        instrs[cond_start as usize].opcode == get_op
            && instrs[cond_start as usize].argument == var_id
            && instrs[(end_index - 5) as usize].opcode == Opcode::PushInt8
            && instrs[(end_index - 5) as usize].argument == expected_increment
            && instrs[(end_index - 4) as usize].opcode == get_op
            && instrs[(end_index - 4) as usize].argument == var_id
            && instrs[(end_index - 3) as usize].opcode == Opcode::Add
            && instrs[(end_index - 2) as usize].opcode == set_op
            && instrs[(end_index - 2) as usize].argument == var_id
    }

    // -- handler translation ----------------------------------------------

    fn translate_handler(&mut self, handler: &lscr::ScriptHandler) -> String {
        self.instrs = handler.instructions.clone();
        self.arg_ids = handler.arg_name_ids.clone();
        self.local_ids = handler.local_name_ids.clone();
        self.tag_loops();
        self.stack.clear();
        self.block_stack.clear();
        self.owner_stack.clear();

        let arg_names: Vec<String> = handler
            .arg_name_ids
            .iter()
            .map(|&id| self.resolve_name(id as i32))
            .collect();
        let handler_name = self.resolve_name(handler.name_id as i32);

        let root = Block::new();
        self.block_stack.push(root.clone());
        self.owner_stack.push(Owner::Root);

        let mut index = 0usize;
        while index < self.instrs.len() {
            let ins = self.instrs[index];
            // Handle block boundaries: instructions whose offset matches the
            // end of the current block close it (and maybe open an else /
            // otherwise body).
            loop {
                let boundary = self.block_stack.last().map(|b| b.borrow().end_pos == ins.offset);
                if boundary != Some(true) {
                    break;
                }
                let exited = self.block_stack.pop().expect("boundary block");
                let ancestor = self.owner_stack.pop().expect("boundary owner");
                match ancestor {
                    Owner::If(ifn, is_true) => {
                        let ifb = ifn.borrow_mut();
                        if ifb.has_else && is_true {
                            let fb = ifb.false_block.clone();
                            drop(ifb);
                            self.enter_block(Owner::If(ifn, false), fb);
                            continue;
                        }
                    }
                    Owner::Cases(cases) => {
                        let case_node = self.block_stack.last().and_then(|b| b.borrow().current_case.clone());
                        if let Some(cn) = case_node {
                            let mut cnb = cn.borrow_mut();
                            let cblock = cnb.block.clone();
                            if cnb.expect == EXPECT_OTHERWISE {
                                if let Some(cb) = cblock {
                                    if Rc::ptr_eq(&cb, &exited) {
                                        let cases_end = cases.borrow().end_pos;
                                        let otherwise = Block::new();
                                        otherwise.borrow_mut().end_pos = cases_end;
                                        cnb.otherwise = Some(otherwise.clone());
                                        drop(cnb);
                                        self.enter_block(Owner::Cases(cases), otherwise);
                                        continue;
                                    }
                                }
                                let eb = self.block_stack.last().expect("enclosing block");
                                eb.borrow_mut().current_case = None;
                            } else if cnb.expect == EXPECT_POP {
                                let cases_end = cases.borrow().end_pos;
                                let exited_end = exited.borrow().end_pos;
                                if self.cases_has_otherwise_body(cases_end, exited_end) {
                                    let otherwise = Block::new();
                                    otherwise.borrow_mut().end_pos = cases_end;
                                    cnb.otherwise = Some(otherwise.clone());
                                    drop(cnb);
                                    self.enter_block(Owner::Cases(cases), otherwise);
                                    continue;
                                }
                                let eb = self.block_stack.last().expect("enclosing block");
                                eb.borrow_mut().current_case = None;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if self.block_stack.is_empty() {
                self.block_stack.push(root.clone());
                self.owner_stack.push(Owner::Root);
            }
            self.last_consumed = 1;
            self.translate_instruction(ins, index);
            index += self.last_consumed.max(1);
        }

        handler_lingo(&handler_name, &arg_names, &root, self.dot)
    }

    /// Gap between a case's pop and the cases end contains a non-pop/ret
    /// instruction → a real `otherwise` body (LingoDecompiler.cpp
    /// translateHandler EXPECT_POP handling).
    fn cases_has_otherwise_body(&self, cases_end: i32, exited_end: i32) -> bool {
        if cases_end <= exited_end {
            return false;
        }
        for ins in &self.instrs {
            if ins.offset > exited_end && ins.offset < cases_end {
                if !matches!(ins.opcode, Opcode::Pop | Opcode::Ret | Opcode::RetFactory) {
                    return true;
                }
            }
        }
        false
    }

    fn translate_instruction(&mut self, ins: lscr::Instruction, index: usize) {
        if index < self.tags.len()
            && (self.tags[index] == TAG_SKIP || self.tags[index] == TAG_NEXT_REPEAT_TARGET)
        {
            return;
        }

        let op = ins.opcode;
        let arg = ins.argument;
        let mut translation: Option<Trans> = None;

        match op {
            Opcode::Ret | Opcode::RetFactory => {
                if index + 1 == self.instrs.len() {
                    return;
                }
                translation = Some(Trans::Stmt(Stmt::Exit));
            }
            Opcode::PushZero => translation = Some(Trans::Expr(Lit::int(0))),
            Opcode::Mul
            | Opcode::Add
            | Opcode::Sub
            | Opcode::Div
            | Opcode::Mod
            | Opcode::JoinStr
            | Opcode::JoinPadStr
            | Opcode::Lt
            | Opcode::LtEq
            | Opcode::NtEq
            | Opcode::Eq
            | Opcode::Gt
            | Opcode::GtEq
            | Opcode::And
            | Opcode::Or
            | Opcode::ContainsStr
            | Opcode::Contains0Str => {
                let right = self.pop();
                let left = self.pop();
                translation = Some(Trans::Expr(Expr::Bin(op, Box::new(left), Box::new(right))));
            }
            Opcode::Inv => translation = Some(Trans::Expr(Expr::Inverse(Box::new(self.pop())))),
            Opcode::Not => translation = Some(Trans::Expr(Expr::Not(Box::new(self.pop())))),
            Opcode::GetChunk => {
                let s = self.pop();
                translation = Some(Trans::Expr(self.read_chunk_ref(s)));
            }
            Opcode::HiliteChunk => {
                let cast_id = if self.version >= 500 { Some(self.pop()) } else { None };
                let field_id = self.pop();
                let field = Expr::Member {
                    member_type: "field".into(),
                    member_id: Box::new(field_id),
                    cast_id: cast_id.map(Box::new),
                };
                translation = Some(Trans::Stmt(Stmt::Hilite(Box::new(self.read_chunk_ref(field)))));
            }
            Opcode::OntoSpr => {
                let right = self.pop();
                let left = self.pop();
                translation = Some(Trans::Expr(Expr::SpriteIntersects {
                    a: Box::new(left),
                    b: Box::new(right),
                }));
            }
            Opcode::IntoSpr => {
                let right = self.pop();
                let left = self.pop();
                translation = Some(Trans::Expr(Expr::SpriteWithin {
                    a: Box::new(left),
                    b: Box::new(right),
                }));
            }
            Opcode::GetField => {
                let cast_id = if self.version >= 500 { Some(self.pop()) } else { None };
                let member_id = self.pop();
                translation = Some(Trans::Expr(Expr::Member {
                    member_type: "field".into(),
                    member_id: Box::new(member_id),
                    cast_id: cast_id.map(Box::new),
                }));
            }
            Opcode::StartTell => {
                let tell = Rc::new(RefCell::new(TellNode {
                    window: self.pop(),
                    block: Block::new(),
                }));
                let block = tell.borrow().block.clone();
                self.current_block().borrow_mut().children.push(Stmt::Tell(tell.clone()));
                self.enter_block(Owner::Tell, block);
                return;
            }
            Opcode::EndTell => {
                self.exit_block();
                return;
            }
            Opcode::PushInt8 | Opcode::PushInt16 | Opcode::PushInt32 => {
                translation = Some(Trans::Expr(Lit::int(arg)));
            }
            Opcode::PushFloat32 => {
                translation = Some(Trans::Expr(Lit::float(f32::from_bits(arg as u32) as f64)));
            }
            Opcode::PushArgList | Opcode::PushArgListNoRet => {
                let count = arg.max(0) as usize;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.insert(0, self.pop());
                }
                translation = Some(Trans::Expr(Lit::args(items, op == Opcode::PushArgListNoRet)));
            }
            Opcode::PushList | Opcode::PushPropList => {
                let popped = self.pop();
                let list = op == Opcode::PushList;
                translation = Some(Trans::Expr(match popped {
                    Expr::Lit(mut l) => {
                        l.vtype = if list { VType::List } else { VType::PropList };
                        Expr::Lit(l)
                    }
                    other => other,
                }));
            }
            Opcode::PushCons => {
                let literal_id = arg / self.variable_multiplier();
                if literal_id >= 0 && (literal_id as usize) < self.script.literals.len() {
                    translation =
                        Some(Trans::Expr(self.literal_to_node(&self.script.literals[literal_id as usize])));
                } else {
                    translation = Some(Trans::Expr(Expr::Err));
                }
            }
            Opcode::PushSymb => {
                translation = Some(Trans::Expr(Lit::sym(self.resolve_name(arg))));
            }
            Opcode::PushVarRef => {
                translation = Some(Trans::Expr(Lit::varref(self.resolve_name(arg))));
            }
            Opcode::GetGlobal | Opcode::GetGlobal2 | Opcode::GetProp | Opcode::GetTopLevelProp => {
                translation = Some(Trans::Expr(Expr::Var(self.resolve_name(arg))));
            }
            Opcode::GetParam => {
                translation = Some(Trans::Expr(Expr::Var(self.get_argument_name(arg))));
            }
            Opcode::GetLocal => {
                translation = Some(Trans::Expr(Expr::Var(self.get_local_name(arg))));
            }
            Opcode::SetGlobal | Opcode::SetGlobal2 | Opcode::SetProp => {
                let var = Expr::Var(self.resolve_name(arg));
                let val = self.pop();
                translation = Some(Trans::Stmt(Stmt::Assign {
                    var: Box::new(var),
                    val: Box::new(val),
                    force_verbose: false,
                }));
            }
            Opcode::SetParam => {
                let var = Expr::Var(self.get_argument_name(arg));
                let val = self.pop();
                translation = Some(Trans::Stmt(Stmt::Assign {
                    var: Box::new(var),
                    val: Box::new(val),
                    force_verbose: false,
                }));
            }
            Opcode::SetLocal => {
                let var = Expr::Var(self.get_local_name(arg));
                let val = self.pop();
                translation = Some(Trans::Stmt(Stmt::Assign {
                    var: Box::new(var),
                    val: Box::new(val),
                    force_verbose: false,
                }));
            }
            Opcode::Put => {
                let put_type = ((arg >> 4) & 0xF) as u8;
                let var_type = arg & 0xF;
                let variable = self.read_var(var_type);
                let value = self.pop();
                translation = Some(Trans::Stmt(Stmt::Put {
                    put_type,
                    var: Box::new(variable),
                    val: Box::new(value),
                }));
            }
            Opcode::PutChunk => {
                let put_type = ((arg >> 4) & 0xF) as u8;
                let var_type = arg & 0xF;
                let variable = self.read_var(var_type);
                let chunk = self.read_chunk_ref(variable);
                let value = self.pop();
                translation = Some(Trans::Stmt(Stmt::Put {
                    put_type,
                    var: Box::new(chunk),
                    val: Box::new(value),
                }));
            }
            Opcode::DeleteChunk => {
                let variable = self.read_var(arg);
                let chunk = self.read_chunk_ref(variable);
                translation = Some(Trans::Stmt(Stmt::Delete(Box::new(chunk))));
            }
            Opcode::Get => {
                let prop_id = expr_int_value(&self.pop());
                translation = Some(self.read_v4_property(arg, prop_id));
            }
            Opcode::Set => {
                let prop_id = expr_int_value(&self.pop());
                let value = self.pop();
                if arg == 0x00
                    && (0x01..=0x05).contains(&prop_id)
                    && expr_vtype(&value) == VType::Str
                {
                    let script = expr_string_value(&value);
                    if !script.is_empty() && (script.starts_with(' ') || script.contains('\r')) {
                        translation = Some(Trans::Stmt(Stmt::When {
                            event: prop_id as u8,
                            script,
                        }));
                    }
                }
                if translation.is_none() {
                    let property = self.read_v4_property(arg, prop_id);
                    translation = Some(match property {
                        Trans::Stmt(Stmt::Comment(_)) => property,
                        Trans::Expr(prop) => Trans::Stmt(Stmt::Assign {
                            var: Box::new(prop),
                            val: Box::new(value),
                            force_verbose: true,
                        }),
                        other => other,
                    });
                }
            }
            Opcode::LocalCall => {
                let mut arg_list = self.pop();
                let noret = arglist_noret(&arg_list);
                let mut call_name = format!("handler#{arg}");
                if arg >= 0 && (arg as usize) < self.script.handlers.len() {
                    call_name =
                        self.resolve_name(self.script.handlers[arg as usize].name_id as i32);
                }
                let args = take_arg_nodes(&mut arg_list);
                if noret {
                    translation = Some(Trans::Stmt(Stmt::Call { name: call_name, args }));
                } else {
                    translation = Some(Trans::Expr(Expr::Call { name: call_name, args }));
                }
            }
            Opcode::ExtCall | Opcode::TellCall => {
                let call_name = self.resolve_name(arg);
                let mut arg_list = self.pop();
                let noret = arglist_noret(&arg_list);
                let is_return = call_name == "return"
                    && matches!(expr_vtype(&arg_list), VType::ArgList | VType::ArgListNoRet);
                let args = take_arg_nodes(&mut arg_list);
                if is_return {
                    if args.is_empty() {
                        translation = Some(Trans::Stmt(Stmt::Exit));
                    } else if args.len() == 1 {
                        translation = Some(Trans::Stmt(Stmt::Return(Box::new(args[0].clone()))));
                    } else {
                        // `return RETURN, error(me, ...)` — the R31 compiler
                        // emits the return-with-error-call idiom with the CR
                        // literal as a leading sentinel arg (Sulake wrote
                        // `return RETURN, error(...)`; the constant pool holds
                        // a lone "\r" literal used only by these statements).
                        // The value is a discard — the SAME handlers pair it
                        // with plain `return error(...)` on their other error
                        // paths, and LibreShockwave's decompiler materializes
                        // the 2+ arg form as a plain call whose value is
                        // dropped. Render it as a return of the LAST value
                        // (the error() result) so the output stays a clean
                        // return statement that matches the sibling lines.
                        translation = Some(Trans::Stmt(Stmt::Return(Box::new(args[args.len() - 1].clone()))));
                    }
                } else if noret {
                    translation = Some(Trans::Stmt(Stmt::Call { name: call_name, args }));
                } else {
                    translation = Some(Trans::Expr(Expr::Call { name: call_name, args }));
                }
            }
            Opcode::ObjCallV4 => {
                let object = self.read_var(arg);
                let mut arg_list = self.pop();
                let noret = arglist_noret(&arg_list);
                let mut args = take_arg_nodes(&mut arg_list);
                if let Some(first) = args.first_mut() {
                    if expr_vtype(first) == VType::Symbol {
                        *first = Expr::Var(expr_string_value(first));
                    }
                }
                if noret {
                    translation = Some(Trans::Stmt(Stmt::Expr(Expr::ObjCallV4 {
                        obj: Box::new(object),
                        args,
                    })));
                } else {
                    translation = Some(Trans::Expr(Expr::ObjCallV4 {
                        obj: Box::new(object),
                        args,
                    }));
                }
            }
            Opcode::GetMovieProp => {
                translation = Some(Trans::Expr(Expr::The(self.resolve_name(arg))));
            }
            Opcode::SetMovieProp => {
                let val = self.pop();
                translation = Some(Trans::Stmt(Stmt::Assign {
                    var: Box::new(Expr::The(self.resolve_name(arg))),
                    val: Box::new(val),
                    force_verbose: false,
                }));
            }
            Opcode::GetObjProp | Opcode::GetChainedProp => {
                let obj = self.pop();
                translation = Some(Trans::Expr(Expr::ObjProp {
                    obj: Box::new(obj),
                    prop: self.resolve_name(arg),
                }));
            }
            Opcode::SetObjProp => {
                let value = self.pop();
                let object = self.pop();
                translation = Some(Trans::Stmt(Stmt::Assign {
                    var: Box::new(Expr::ObjProp {
                        obj: Box::new(object),
                        prop: self.resolve_name(arg),
                    }),
                    val: Box::new(value),
                    force_verbose: false,
                }));
            }
            Opcode::Peek => {
                self.translate_peek(index);
                return;
            }
            Opcode::TheBuiltin => {
                self.pop();
                translation = Some(Trans::Expr(Expr::The(self.resolve_name(arg))));
            }
            Opcode::ObjCall => {
                translation = Some(self.translate_obj_call(arg));
            }
            Opcode::Jmp => {
                let target_offset = ins.offset + arg;
                let target_index = self.instruction_index_for_offset(target_offset);
                if let Some(ti) = target_index {
                    if let Some(loop_owner) = self.ancestor_loop() {
                        let start_index = loop_owner.borrow().start_index;
                        if ti >= 1
                            && self.instrs[(ti - 1) as usize].opcode == Opcode::EndRepeat
                            && ((ti - 1) as usize) < self.owner_loops.len()
                            && self.owner_loops[(ti - 1) as usize] == start_index
                        {
                            translation = Some(Trans::Stmt(Stmt::ExitRepeat));
                        } else if (ti as usize) < self.tags.len()
                            && self.tags[ti as usize] == TAG_NEXT_REPEAT_TARGET
                            && self.owner_loops[ti as usize] == start_index
                        {
                            translation = Some(Trans::Stmt(Stmt::NextRepeat));
                        }
                    }
                    if translation.is_none() && index + 1 < self.instrs.len() {
                        let end_pos = self.current_block().borrow().end_pos;
                        if self.instrs[index + 1].offset == end_pos {
                            match self.owner_stack.last() {
                                Some(Owner::If(ifn, true)) => {
                                    ifn.borrow_mut().has_else = true;
                                    ifn.borrow_mut().false_block.borrow_mut().end_pos = target_offset;
                                    return;
                                }
                                Some(Owner::Cases(cases)) => {
                                    cases.borrow_mut().end_pos = target_offset;
                                    return;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                // LibreShockwave `break`s out of the switch after ExitRepeat /
                // NextRepeat, skipping the fallback comment below.
                if translation.is_none() {
                    translation =
                        Some(Trans::Stmt(Stmt::Comment("ERROR: Could not identify jmp".into())));
                }
            }
            Opcode::JmpIfZ => {
                let end_pos = ins.offset + arg;
                let idx_tag = self.tags.get(index).copied().unwrap_or(TAG_NONE);
                if idx_tag == TAG_REPEAT_WHILE {
                    let cond = self.pop();
                    let loop_node = Rc::new(RefCell::new(LoopNode {
                        start_index: index as i32,
                        kind: LoopKind::While { cond },
                        block: Block::new(),
                    }));
                    loop_node.borrow().block.borrow_mut().end_pos = end_pos;
                    let block = loop_node.borrow().block.clone();
                    self.current_block().borrow_mut().children.push(Stmt::Loop(loop_node.clone()));
                    self.enter_block(Owner::Loop(loop_node), block);
                    return;
                }
                if idx_tag == TAG_REPEAT_WITH_IN {
                    let var = self.get_var_name_from_set(self.instrs[index + 5]);
                    let list = self.pop();
                    let loop_node = Rc::new(RefCell::new(LoopNode {
                        start_index: index as i32,
                        kind: LoopKind::WithIn { var, list },
                        block: Block::new(),
                    }));
                    loop_node.borrow().block.borrow_mut().end_pos = end_pos;
                    let block = loop_node.borrow().block.clone();
                    self.current_block().borrow_mut().children.push(Stmt::Loop(loop_node.clone()));
                    self.enter_block(Owner::Loop(loop_node), block);
                    return;
                }
                if idx_tag == TAG_REPEAT_WITH_TO || idx_tag == TAG_REPEAT_WITH_DOWNTO {
                    let end = self.pop();
                    let start = self.pop();
                    let end_index = self.instruction_index_for_offset(end_pos);
                    if let Some(ei) = end_index {
                        if ei >= 1 {
                            let end_repeat = self.instrs[(ei - 1) as usize];
                            let cond_start =
                                self.instruction_index_for_offset(end_repeat.offset - end_repeat.argument);
                            if let Some(cs) = cond_start {
                                if cs >= 1 {
                                    let var =
                                        self.get_var_name_from_set(self.instrs[(cs - 1) as usize]);
                                    let up = idx_tag == TAG_REPEAT_WITH_TO;
                                    let loop_node = Rc::new(RefCell::new(LoopNode {
                                        start_index: index as i32,
                                        kind: LoopKind::WithTo { var, start, up, end },
                                        block: Block::new(),
                                    }));
                                    loop_node.borrow().block.borrow_mut().end_pos = end_pos;
                                    let block = loop_node.borrow().block.clone();
                                    self.current_block()
                                        .borrow_mut()
                                        .children
                                        .push(Stmt::Loop(loop_node.clone()));
                                    self.enter_block(Owner::Loop(loop_node), block);
                                    return;
                                }
                            }
                        }
                    }
                    translation = Some(Trans::Stmt(Stmt::Comment(
                        "ERROR: Could not identify repeat with to".into(),
                    )));
                } else {
                    let cond = self.pop();
                    let if_node = Rc::new(RefCell::new(IfNode {
                        cond,
                        true_block: Block::new(),
                        false_block: Block::new(),
                        has_else: false,
                    }));
                    if_node.borrow().true_block.borrow_mut().end_pos = end_pos;
                    let tb = if_node.borrow().true_block.clone();
                    self.current_block().borrow_mut().children.push(Stmt::If(if_node.clone()));
                    self.enter_block(Owner::If(if_node, true), tb);
                    return;
                }
            }
            Opcode::EndRepeat => {
                translation = Some(Trans::Stmt(Stmt::Comment("ERROR: Stray endrepeat".into())));
            }
            Opcode::PushChunkVarRef => {
                translation = Some(Trans::Expr(self.read_var(arg)));
            }
            Opcode::NewObj => {
                let args = self.pop();
                translation = Some(Trans::Expr(Expr::NewObj {
                    obj_type: self.resolve_name(arg),
                    args: Box::new(args),
                }));
            }
            Opcode::Swap => {
                if self.stack.len() >= 2 {
                    let n = self.stack.len();
                    self.stack.swap(n - 1, n - 2);
                }
                return;
            }
            Opcode::Pop => {
                for _ in 0..arg.max(0) {
                    self.pop();
                }
                return;
            }
            Opcode::CallJavaScript => {
                self.stack.clear();
                let mut script_text = String::new();
                if let Some(first) = self.script.literals.first() {
                    if let Some(s) = &first.string_value {
                        script_text = s.clone();
                    }
                }
                translation = Some(Trans::Stmt(Stmt::Comment(format!("@js\n{script_text}"))));
            }
            _ => {
                let mut text = op.mnemonic().to_string();
                if ins.raw_opcode >= 0x40 {
                    text.push(' ');
                    text.push_str(&arg.to_string());
                }
                translation = Some(Trans::Stmt(Stmt::Comment(text)));
                self.stack.clear();
            }
        }

        let translation = translation.unwrap_or(Trans::Expr(Expr::Err));
        match translation {
            Trans::Expr(e) => self.stack.push(e),
            Trans::Stmt(s) => self.current_block().borrow_mut().children.push(s),
        }
    }

    fn translate_peek(&mut self, index: usize) {
        let add_error = |dec: &mut Decoder, text: &str, consumed: usize| {
            dec.current_block()
                .borrow_mut()
                .children
                .push(Stmt::Comment(text.to_string()));
            dec.last_consumed = consumed.max(1);
        };

        let instrs = self.instrs.clone();
        let previous_case = self.current_block().borrow().current_case.clone();
        let mut peeked_value = None;
        if previous_case.is_none() {
            peeked_value = Some(self.pop());
        }

        let original_stack_size = self.stack.len();
        let mut current_index = index + 1;
        while current_index < instrs.len() && instrs[current_index].opcode == Opcode::Peek {
            current_index += 1;
        }
        while current_index < instrs.len() {
            let i = current_index;
            self.translate_instruction(instrs[i], i);
            current_index += 1;
            if current_index < instrs.len()
                && self.stack.len() == original_stack_size + 1
                && (instrs[current_index].opcode == Opcode::Eq
                    || instrs[current_index].opcode == Opcode::NtEq)
            {
                break;
            }
        }
        if current_index >= instrs.len() {
            add_error(self, "ERROR: Expected eq or nteq", current_index - index + 1);
            return;
        }

        let case_value = self.pop();
        if instrs[current_index].opcode != Opcode::Eq && instrs[current_index].opcode != Opcode::NtEq {
            add_error(self, "ERROR: Expected eq or nteq", current_index - index + 1);
            return;
        }
        let not_equal = instrs[current_index].opcode == Opcode::NtEq;
        current_index += 1;
        if current_index >= instrs.len() || instrs[current_index].opcode != Opcode::JmpIfZ {
            add_error(self, "ERROR: Expected jmpifz", current_index - index + 1);
            return;
        }

        let jump = instrs[current_index];
        let jump_pos = jump.offset + jump.argument;
        let target_index = self.instruction_index_for_offset(jump_pos);

        let mut expect = EXPECT_OTHERWISE;
        if not_equal {
            expect = EXPECT_OR;
        } else if let Some(ti) = target_index {
            if (ti as usize) < instrs.len() && instrs[ti as usize].opcode == Opcode::Peek {
                expect = EXPECT_NEXT;
            } else if (ti as usize) < instrs.len() && instrs[ti as usize].opcode == Opcode::Pop {
                expect = EXPECT_POP;
            }
        }

        let case = Rc::new(RefCell::new(CaseNode {
            value: case_value,
            expect,
            next_or: None,
            next_case: None,
            block: None,
            otherwise: None,
            cases: None,
        }));
        let mut chain_cases: Option<RcCell<CasesNode>> = None;
        let mut attached = false;
        if previous_case.is_none() {
            let cases = Rc::new(RefCell::new(CasesNode {
                value: peeked_value.unwrap_or(Expr::Err),
                first_case: case.clone(),
                end_pos: -1,
            }));
            case.borrow_mut().cases = Some(cases.clone());
            self.current_block().borrow_mut().children.push(Stmt::Cases(cases.clone()));
            chain_cases = Some(cases);
            attached = true;
        } else if previous_case.as_ref().unwrap().borrow().expect == EXPECT_OR {
            previous_case.as_ref().unwrap().borrow_mut().next_or = Some(case.clone());
            chain_cases = previous_case.as_ref().unwrap().borrow().cases.clone();
            attached = true;
        } else if previous_case.as_ref().unwrap().borrow().expect == EXPECT_NEXT {
            previous_case.as_ref().unwrap().borrow_mut().next_case = Some(case.clone());
            chain_cases = previous_case.as_ref().unwrap().borrow().cases.clone();
            attached = true;
        }

        if !attached {
            add_error(self, "ERROR: Unexpected case branch", current_index - index + 1);
            return;
        }
        if let Some(cases) = &chain_cases {
            case.borrow_mut().cases = Some(cases.clone());
        }

        self.current_block().borrow_mut().current_case = Some(case.clone());
        if expect != EXPECT_OR {
            let case_block = Block::new();
            case_block.borrow_mut().end_pos = jump_pos;
            case.borrow_mut().block = Some(case_block.clone());
            let cases = chain_cases.expect("cases owner");
            self.enter_block(Owner::Cases(cases), case_block);
        }
        self.last_consumed = current_index - index + 1;
    }

    fn translate_obj_call(&mut self, name_id: i32) -> Trans {
        let method = self.resolve_name(name_id);
        let mut arg_list = self.pop();
        let noret = arglist_noret(&arg_list);
        let raw_args: Vec<Expr> = expr_arg_nodes(&arg_list).to_vec();
        let nargs = raw_args.len();

        if method == "getAt" && nargs == 2 {
            let mut args = take_arg_nodes(&mut arg_list);
            let b = args.pop().unwrap();
            let a = args.pop().unwrap();
            return Trans::Expr(Expr::ObjBracket {
                obj: Box::new(a),
                prop: Box::new(b),
            });
        }
        if method == "setAt" && nargs == 3 {
            let mut args = take_arg_nodes(&mut arg_list);
            let val = args.pop().unwrap();
            let idx = args.pop().unwrap();
            let obj = args.pop().unwrap();
            return Trans::Stmt(Stmt::Assign {
                var: Box::new(Expr::ObjBracket {
                    obj: Box::new(obj),
                    prop: Box::new(idx),
                }),
                val: Box::new(val),
                force_verbose: false,
            });
        }
        if (method == "getProp" || method == "getPropRef")
            && (nargs == 3 || nargs == 4)
            && expr_vtype(&raw_args[1]) == VType::Symbol
        {
            // args = [obj, #prop, index, (index2)] — LibreShockwave indexes
            // directly (args[0], args[2], args[3]) rather than popping.
            let prop_name = expr_string_value(&raw_args[1]);
            let mut args = take_arg_nodes(&mut arg_list);
            let mut index2 = None;
            if nargs == 4 {
                index2 = Some(args.remove(3));
            }
            let idx = args.remove(2);
            let obj = args.remove(0);
            return Trans::Expr(Expr::ObjPropIndex {
                obj: Box::new(obj),
                prop: prop_name,
                index: Box::new(idx),
                index2: index2.map(Box::new),
            });
        }
        if method == "setProp"
            && (nargs == 4 || nargs == 5)
            && expr_vtype(&raw_args[1]) == VType::Symbol
        {
            // args = [obj, #prop, index, (index2), value]
            let prop_name = expr_string_value(&raw_args[1]);
            let mut args = take_arg_nodes(&mut arg_list);
            let last = args.remove(nargs - 1);
            let mut index2 = None;
            if nargs == 5 {
                index2 = Some(args.remove(3));
            }
            let idx = args.remove(2);
            let obj = args.remove(0);
            return Trans::Stmt(Stmt::Assign {
                var: Box::new(Expr::ObjPropIndex {
                    obj: Box::new(obj),
                    prop: prop_name,
                    index: Box::new(idx),
                    index2: index2.map(Box::new),
                }),
                val: Box::new(last),
                force_verbose: false,
            });
        }
        if method == "count" && nargs == 2 && expr_vtype(&raw_args[1]) == VType::Symbol {
            let prop_name = expr_string_value(&raw_args[1]);
            let mut args = take_arg_nodes(&mut arg_list);
            let obj = args.remove(0);
            let inner = Expr::ObjProp {
                obj: Box::new(obj),
                prop: prop_name,
            };
            return Trans::Expr(Expr::ObjProp {
                obj: Box::new(inner),
                prop: "count".into(),
            });
        }
        if (method == "setContents" || method == "setContentsAfter" || method == "setContentsBefore")
            && nargs == 2
        {
            let put_type = if method == "setContents" {
                1
            } else if method == "setContentsAfter" {
                2
            } else {
                3
            };
            let mut args = take_arg_nodes(&mut arg_list);
            let val = args.pop().unwrap();
            let var = args.pop().unwrap();
            return Trans::Stmt(Stmt::Put {
                put_type,
                var: Box::new(var),
                val: Box::new(val),
            });
        }
        if method == "hilite" && nargs == 1 {
            let mut args = take_arg_nodes(&mut arg_list);
            return Trans::Stmt(Stmt::Hilite(Box::new(args.pop().unwrap())));
        }
        if method == "delete" && nargs == 1 {
            let mut args = take_arg_nodes(&mut arg_list);
            return Trans::Stmt(Stmt::Delete(Box::new(args.pop().unwrap())));
        }

        let args = take_arg_nodes(&mut arg_list);
        if noret {
            Trans::Stmt(Stmt::Expr(Expr::ObjCall { name: method, args }))
        } else {
            Trans::Expr(Expr::ObjCall { name: method, args })
        }
    }

    fn read_var(&mut self, var_type: i32) -> Expr {
        let mut cast_id = None;
        if var_type == 0x6 && self.version >= 500 {
            cast_id = Some(self.pop());
        }
        let id = self.pop();
        match var_type {
            0x1 | 0x2 | 0x3 => id,
            0x4 => Lit::varref(self.get_argument_name(expr_int_value(&id))),
            0x5 => Lit::varref(self.get_local_name(expr_int_value(&id))),
            0x6 => Expr::Member {
                member_type: "field".into(),
                member_id: Box::new(id),
                cast_id: cast_id.map(Box::new),
            },
            _ => Expr::Err,
        }
    }

    fn read_chunk_ref(&mut self, mut string: Expr) -> Expr {
        let last_line = self.pop();
        let first_line = self.pop();
        let last_item = self.pop();
        let first_item = self.pop();
        let last_word = self.pop();
        let first_word = self.pop();
        let last_char = self.pop();
        let first_char = self.pop();

        if !is_zero_literal(&first_line) {
            string = Expr::ChunkExpr {
                chunk_type: 4,
                first: Box::new(first_line),
                last: Box::new(last_line),
                string: Box::new(string),
            };
        }
        if !is_zero_literal(&first_item) {
            string = Expr::ChunkExpr {
                chunk_type: 3,
                first: Box::new(first_item),
                last: Box::new(last_item),
                string: Box::new(string),
            };
        }
        if !is_zero_literal(&first_word) {
            string = Expr::ChunkExpr {
                chunk_type: 2,
                first: Box::new(first_word),
                last: Box::new(last_word),
                string: Box::new(string),
            };
        }
        if !is_zero_literal(&first_char) {
            string = Expr::ChunkExpr {
                chunk_type: 1,
                first: Box::new(first_char),
                last: Box::new(last_char),
                string: Box::new(string),
            };
        }
        string
    }

    fn read_v4_property(&mut self, property_type: i32, property_id: i32) -> Trans {
        match property_type {
            0x00 => {
                if property_id <= 0x0b {
                    Trans::Expr(Expr::The(movie_property_name(property_id).to_string()))
                } else {
                    Trans::Expr(Expr::LastStringChunk {
                        chunk_type: (property_id - 0x0b) as u8,
                        string: Box::new(self.pop()),
                    })
                }
            }
            0x01 => Trans::Expr(Expr::StringChunkCount {
                chunk_type: property_id as u8,
                string: Box::new(self.pop()),
            }),
            0x02 => Trans::Expr(Expr::MenuProp {
                menu: Box::new(self.pop()),
                prop: property_id as u8,
            }),
            0x03 => {
                let menu_id = self.pop();
                Trans::Expr(Expr::MenuItemProp {
                    menu: Box::new(menu_id),
                    item: Box::new(self.pop()),
                    prop: property_id as u8,
                })
            }
            0x04 => Trans::Expr(Expr::SoundProp {
                sound: Box::new(self.pop()),
                prop: property_id as u8,
            }),
            0x05 => Trans::Stmt(Stmt::Comment("ERROR: Resource property".into())),
            0x06 => Trans::Expr(Expr::SpriteProp {
                sprite: Box::new(self.pop()),
                prop: property_id as u8,
            }),
            0x07 => Trans::Expr(Expr::The(animation_property_name(property_id).to_string())),
            0x08 => {
                if property_id == 0x02 && self.version >= 500 {
                    let cast_lib = self.pop();
                    if !is_zero_literal(&cast_lib) {
                        return Trans::Expr(Expr::TheProp {
                            obj: Box::new(Expr::Member {
                                member_type: "castLib".into(),
                                member_id: Box::new(cast_lib),
                                cast_id: None,
                            }),
                            prop: animation2_property_name(property_id).to_string(),
                        });
                    }
                }
                Trans::Expr(Expr::The(animation2_property_name(property_id).to_string()))
            }
            0x09..=0x15 => {
                let cast_id = if self.version >= 500 { Some(self.pop()) } else { None };
                let member_id = self.pop();
                let prefix = if property_type == 0x0b || property_type == 0x0c {
                    "field"
                } else if property_type == 0x14 || property_type == 0x15 {
                    "script"
                } else if self.version >= 500 {
                    "member"
                } else {
                    "cast"
                };
                let mut entity = Expr::Member {
                    member_type: prefix.to_string(),
                    member_id: Box::new(member_id),
                    cast_id: cast_id.map(Box::new),
                };
                if property_type == 0x0a || property_type == 0x0c || property_type == 0x15 {
                    entity = self.read_chunk_ref(entity);
                }
                Trans::Expr(Expr::TheProp {
                    obj: Box::new(entity),
                    prop: member_property_name(property_id).to_string(),
                })
            }
            _ => Trans::Stmt(Stmt::Comment(format!(
                "ERROR: Unknown property type {property_type}"
            ))),
        }
    }
}

fn set_opcode_to_get_opcode(set_op: Opcode) -> Option<Opcode> {
    match set_op {
        Opcode::SetGlobal => Some(Opcode::GetGlobal),
        Opcode::SetGlobal2 => Some(Opcode::GetGlobal2),
        Opcode::SetProp => Some(Opcode::GetProp),
        Opcode::SetParam => Some(Opcode::GetParam),
        Opcode::SetLocal => Some(Opcode::GetLocal),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Decompile a parsed Lscr script to readable Lingo source, matching
/// LibreShockwave's `LingoDecompiler::decompile`. `version` is the Director
/// file version (DRCF directorVersion; >= 700 enables dot syntax) and
/// `capital_x` selects the variable-multiplier (1 for LctX files).
pub fn decompile_script(
    script: &lscr::Script,
    names: Option<&lscr::ScriptNames>,
    version: i32,
    capital_x: bool,
) -> String {
    let mut result = format!("-- {}\n", script.script_type.name());
    for &pid in &script.property_name_ids {
        result.push_str(&format!("property {}\n", resolve_name_ext(names, pid as i32)));
    }
    for &gid in &script.global_name_ids {
        result.push_str(&format!("global {}\n", resolve_name_ext(names, gid as i32)));
    }
    let has_declarations = !script.property_name_ids.is_empty() || !script.global_name_ids.is_empty();
    if has_declarations && !script.handlers.is_empty() {
        result.push('\n');
    }
    for (i, handler) in script.handlers.iter().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        let mut dec = Decoder::new(script, names, version, capital_x);
        result.push_str(&dec.translate_handler(handler));
    }
    result
}

fn resolve_name_ext(names: Option<&lscr::ScriptNames>, name_id: i32) -> String {
    if let Some(n) = names {
        if name_id >= 0 && (name_id as usize) < n.names.len() {
            return n.names[name_id as usize].clone();
        }
    }
    format!("#{name_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lscr;
    use director_rifx::Chunk;

    fn chunk_of(data: &[u8]) -> Chunk {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"Lscr");
        raw.extend_from_slice(&(data.len() as u32).to_be_bytes());
        raw.extend_from_slice(data);
        let (c, _) = director_rifx::chunk::read_chunk(&raw, &mut 0u64).unwrap();
        c
    }

    fn names_chunk(names: &[&str]) -> lscr::ScriptNames {
        let mut d = vec![0u8; 20];
        d[16..18].copy_from_slice(&20u16.to_be_bytes());
        d[18..20].copy_from_slice(&(names.len() as u16).to_be_bytes());
        for n in names {
            d.push(n.len() as u8);
            d.extend_from_slice(n.as_bytes());
        }
        let c = chunk_of(&d);
        lscr::read_script_names(&c).unwrap()
    }

    /// Build a minimal Lscr: one handler with bytecode `bc`, name table ids,
    /// optional literal records + literal data.
fn script_with_handler(
        name_id: i16,
        bc: &[u8],
        arg_ids: &[i16],
        local_ids: &[i16],
        literals: &[(i32, Vec<u8>)], // (kind, raw value bytes)
    ) -> lscr::Script {
        let ho = 100usize;
        let bc_off = ho + 46;
        // Layout: bytecode, then per-handler name tables, then literal
        // records, then literal data.
        let names_off = bc_off + bc.len();
        let names_len = (arg_ids.len() + local_ids.len()) * 2;
        let lit_rec_off = names_off + names_len;
        let lit_data_off = lit_rec_off + literals.len() * 8;
        let mut total = lit_data_off;
        for (_, v) in literals {
            total += 4 + v.len();
        }
        let mut d = vec![0u8; total];
        d[8..12].copy_from_slice(&(total as u32).to_be_bytes());
        d[16..18].copy_from_slice(&92u16.to_be_bytes());
        d[72..74].copy_from_slice(&1u16.to_be_bytes());
        d[74..78].copy_from_slice(&(ho as u32).to_be_bytes());
        d[78..80].copy_from_slice(&(literals.len() as u16).to_be_bytes());
        d[80..84].copy_from_slice(&(lit_rec_off as u32).to_be_bytes());
        d[84..88].copy_from_slice(&(total as u32 - lit_data_off as u32).to_be_bytes());
        d[88..92].copy_from_slice(&(lit_data_off as u32).to_be_bytes());

        d[ho..ho + 2].copy_from_slice(&name_id.to_be_bytes());
        d[ho + 4..ho + 8].copy_from_slice(&(bc.len() as u32).to_be_bytes());
        d[ho + 8..ho + 12].copy_from_slice(&(bc_off as u32).to_be_bytes());
        d[ho + 12..ho + 14].copy_from_slice(&(arg_ids.len() as u16).to_be_bytes());
        d[ho + 14..ho + 18].copy_from_slice(&(arg_ids.len() as u32 * 2 + names_off as u32).to_be_bytes());
        d[ho + 18..ho + 20].copy_from_slice(&(local_ids.len() as u16).to_be_bytes());
        d[ho + 20..ho + 24].copy_from_slice(&(names_off as u32 + arg_ids.len() as u32 * 2).to_be_bytes());
        d[ho + 46..ho + 46 + bc.len()].copy_from_slice(bc);

        let mut cursor = names_off;
        for &id in arg_ids {
            d[cursor..cursor + 2].copy_from_slice(&id.to_be_bytes());
            cursor += 2;
        }
        for &id in local_ids {
            d[cursor..cursor + 2].copy_from_slice(&id.to_be_bytes());
            cursor += 2;
        }

        // literal records + data
        let mut lc = lit_rec_off;
        let mut ld = lit_data_off;
        for (kind, v) in literals {
            d[lc..lc + 4].copy_from_slice(&kind.to_be_bytes());
            d[lc + 4..lc + 8].copy_from_slice(&((ld - lit_data_off) as i32).to_be_bytes());
            lc += 8;
            d[ld..ld + 4].copy_from_slice(&(v.len() as i32).to_be_bytes());
            d[ld + 4..ld + 4 + v.len()].copy_from_slice(v);
            ld += 4 + v.len();
        }

        let c = chunk_of(&d);
        lscr::read_script(&c, true).unwrap()
    }

    #[test]
    fn decompile_if_return() {
        // if objectp(gCore) then return gCore
        let mut names = vec!["x"; 200];
        names[0] = "constructObjectManager";
        names[12] = "return";
        names[145] = "gCore";
        names[170] = "objectp";
        let names = names_chunk(&names);
        let script = script_with_handler(
            0,
            &[
                0x48, 145, // getGlobal2 gCore
                0x43, 1,   // pushArgList 1
                0x57, 170, // extCall objectp
                0x55, 9,   // jmpIfZ +9 (to ret)
                0x48, 145, // getGlobal2 gCore
                0x42, 1,   // pushArgListNoRet 1
                0x57, 12,  // extCall return
                0x01,      // ret
            ],
            &[],
            &[],
            &[],
        );
        let text = decompile_script(&script, Some(&names), 1850, true);
        assert!(text.contains("on constructObjectManager"), "no handler name: {text}");
        assert!(text.contains("if objectp(gCore) then"), "no if: {text}");
        assert!(text.contains("return gCore"), "no return: {text}");
        assert!(text.contains("end if"), "no end if: {text}");
    }

    #[test]
    fn decompile_objcall() {
        // gCore.deconstruct()
        let mut names = vec!["x"; 200];
        names[0] = "h";
        names[145] = "gCore";
        names[175] = "deconstruct";
        let names = names_chunk(&names);
        let script = script_with_handler(
            0,
            &[
                0x48, 145, 0x42, 1, 0x67, 175, // gCore.deconstruct()
                0x01,
            ],
            &[],
            &[],
            &[],
        );
        let text = decompile_script(&script, Some(&names), 1850, true);
        assert!(text.contains("gCore.deconstruct()"), "got: {text}");
    }

    #[test]
    fn decompile_literal_string() {
        // set gStr to "hello" (dot syntax → gStr = "hello")
        let mut names = vec!["x"; 200];
        names[0] = "h";
        names[145] = "gStr";
        let names = names_chunk(&names);
        let script = script_with_handler(
            0,
            &[
                0x44, 0,   // pushCons literal 0
                0x4f, 145, // setGlobal gStr
                0x01,
            ],
            &[],
            &[],
            &[(1, b"hello".to_vec())],
        );
        let text = decompile_script(&script, Some(&names), 1850, true);
        assert!(text.contains("gStr = \"hello\""), "got: {text}");
    }

    #[test]
    fn decompile_float_literal() {
        // pushCons type-9 float 1.5, set gStr
        let mut names = vec!["x"; 200];
        names[0] = "h";
        names[145] = "gNum";
        let names = names_chunk(&names);
        let mut fbytes = vec![0u8; 4];
        fbytes.copy_from_slice(&1.5f32.to_be_bytes());
        let script = script_with_handler(
            0,
            &[
                0x44, 0,   // pushCons literal 0
                0x4f, 145, // setGlobal gNum
                0x01,
            ],
            &[],
            &[],
            &[(9, fbytes)],
        );
        let text = decompile_script(&script, Some(&names), 1850, true);
        assert!(text.contains("gNum = 1.5"), "got: {text}");
    }

    #[test]
    fn cxx_float_formatting() {
        assert_eq!(format_float(0.5), "0.5");
        assert_eq!(format_float(1.0), "1.0");
        assert_eq!(format_float(3.14159265358979), "3.14159265358979");
        assert_eq!(format_float(-2.5), "-2.5");
        assert_eq!(format_float(1e10), "10000000000.0");
        assert_eq!(format_float(1e-5), "1e-05");
        assert_eq!(format_float(1e20), "1e+20");
    }

    #[test]
    fn decompile_repeat_while() {
        // repeat while x < 10 ... bytecode (offsets in parens):
        //   getLocal 0 (0); pushInt8 10 (2); lt (4); jmpIfZ +12 -> ret (5);
        //   getLocal 0 (7); pushInt8 1 (9); add (11); setLocal 0 (12);
        //   endRepeat back 9 -> jmpIfZ at 5 (14); ret (17)
        let names = names_chunk(&["h", "x"]);
        let script = script_with_handler(
            0,
            &[
                0x4c, 0,           // getLocal 0 (x)
                0x41, 10,          // pushInt8 10
                0x0c,              // lt
                0x55, 12,          // jmpIfZ +12 → ret at 17
                0x4c, 0,           // getLocal 0 (x)
                0x41, 1,           // pushInt8 1
                0x05,              // add
                0x52, 0,           // setLocal 0
                0x94, 0x00, 0x09,  // endRepeat, backward 9 → jmpIfZ at 5
                0x01,              // ret
            ],
            &[],
            &[1], // name id of "x"
            &[],
        );
        let text = decompile_script(&script, Some(&names), 1850, true);
        assert!(text.contains("repeat while"), "no repeat: {text}");
        assert!(text.contains("x < 10"), "no cond: {text}");
        assert!(text.contains("x = x + 1"), "no body: {text}");
        assert!(text.contains("end repeat"), "no end repeat: {text}");
    }
}

