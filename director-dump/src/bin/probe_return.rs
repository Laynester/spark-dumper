// Dump handler bytecode WITH the script's Lnam name table and DECRYPTED
// constants (mirrors export_script's resolution), so the `return RETURN,
// error(...)` emission can be decoded.
// usage: probe_return <file.cct> <script-member-1based>
use director_core::{cast, decomp, lingo, lscr};
use director_rifx;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: probe_return <file.cct> [member]");
    let member_idx: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let root = director_rifx::read_file(&path)?;
    let cas: Vec<_> = root.children_by(b"CASt").into_iter().collect();
    let mut n = 0;
    for (i, c) in cas.iter().enumerate() {
        let Ok(cm) = cast::read_cast_member(c) else { continue };
        if cm.member_type != cast::CastMemberType::Script { continue; }
        n += 1;
        if member_idx != 0 && n != member_idx { continue; }
        println!("===== script member #{n} (chunk {}) =====", i + 1);
        let info = if cm.cast_info_size > 0 {
            &cm.raw_data[12..(12 + cm.cast_info_size as usize).min(cm.raw_data.len())]
        } else {
            &cm.raw_data[12..]
        };
        let Some(script_id) = cast::read_member_script_id(info) else {
            println!("  no scriptId"); continue;
        };
        let Some(lctx) = root
            .children_by(b"LctX")
            .into_iter()
            .chain(root.children_by(b"Lctx"))
            .next()
        else { println!("  no LctX"); continue; };
        let ctx = lscr::read_script_context(lctx)?;
        let Some(lscr_res) = ctx.script_resource(script_id) else { println!("  no lscr res"); continue; };
        let Some(lscr_chunk) = root
            .children
            .iter()
            .find(|c| c.is(b"Lscr") && c.source_id == Some(lscr_res))
        else { println!("  Lscr {lscr_res} not found"); continue; };
        let names = (ctx.lnam_section_id > 0).then(|| {
            root.children
                .iter()
                .find(|c| c.is(b"Lnam") && c.source_id == Some(ctx.lnam_section_id as u32))
                .and_then(|c| lscr::read_script_names(c).ok())
        }).flatten();
        let script = lscr::read_script(lscr_chunk, true)?;
        let version = root
            .child(b"DRCF")
            .map(|d| {
                let data = d.data();
                if data.len() >= 38 {
                    u16::from_be_bytes([data[36], data[37]]) as i32
                } else { 0 }
            })
            .unwrap_or(0);
        println!("  scriptId={script_id} lscr_res={lscr_res} version={version} handlers={}", script.handlers.len());
        println!("-- literals --");
        for (li, lit) in script.literals.iter().enumerate() {
            match lit.kind {
                4 => println!("  {li}: int {}", lit.int_value.unwrap_or(0)),
                9 => println!("  {li}: float {:?}", lit.float_value),
                1 => println!("  {li}: str {:?}", lit.string_value),
                _ => println!("  {li}: kind {} bytes {:?}", lit.kind, lit.bytes),
            }
        }
        let dasm = lingo::disassemble_script(&script, names.as_ref());
        println!("{}", lingo::format_script(&dasm));
        let text = decomp::decompile_script(&script, names.as_ref(), version, true);
        println!("-- decompiled --\n{text}");
        if let Some(nm) = &names {
            println!("-- names --");
            for id in 0..1024 {
                if let Some(name) = nm.name(id) {
                    if !name.is_empty() { println!("  {id}: {name}"); }
                }
            }
        }
    }
    Ok(())
}
