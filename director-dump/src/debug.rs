use std::env;
use std::fs;

use director_rifx::Chunk;
use director_core::{cast, key, clut, bitd};

fn member_name(cm: &cast::CastMember) -> String {
    if cm.cast_info_size > 0 {
        let info = &cm.raw_data[12..(12 + cm.cast_info_size as usize).min(cm.raw_data.len())];
        if let Some(n) = cast::read_member_name(info) {
            return n;
        }
    }
    String::new()
}

fn build_member_number_map(root: &Chunk) -> std::collections::HashMap<u32, u32> {
    let min_member = root
        .child(b"DRCF")
        .map(|d| {
            let data = d.data();
            if data.len() >= 14 {
                u16::from_be_bytes([data[12], data[13]]) as u32
            } else {
                1
            }
        })
        .unwrap_or(1);
    let mut map = std::collections::HashMap::new();
    for cas in root.children_by(b"CAS*") {
        for (i, res) in cast::parse_cast_member_list(cas.data()).into_iter().enumerate() {
            if res != 0 {
                map.insert(min_member + i as u32, res);
            }
        }
    }
    map
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: director-debug <file>");
        std::process::exit(1);
    }
    let data = fs::read(&args[1]).unwrap();
    let root = director_rifx::read_bytes(&data).unwrap();

    let min_member = root
        .child(b"DRCF")
        .map(|d| {
            let d = d.data();
            if d.len() >= 14 { u16::from_be_bytes([d[12], d[13]]) } else { 1 }
        })
        .unwrap_or(1);

    // KEY* links
    let mut member_clut: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut member_bitd: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for kc in root.children_by(b"KEY*") {
        if let Ok(kt) = key::read_key(kc) {
            for e in &kt.entries {
                if &e.child_tag == b"CLUT" {
                    member_clut.insert(e.parent_index, e.child_index);
                } else if &e.child_tag == b"BITD" {
                    member_bitd.insert(e.parent_index, e.child_index);
                }
            }
        }
    }

    let num_map = build_member_number_map(&root);

    println!("min_member={min_member}");
    println!("== member# -> CASt res (CAS* map) ==");
    let mut by_res: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for (num, res) in &num_map {
        by_res.insert(*res, *num);
    }
    let mut nums: Vec<(u32, u32)> = num_map.iter().map(|(a, b)| (*a, *b)).collect();
    nums.sort();
    for (num, res) in &nums {
        let clut = member_clut.get(res).map(|c| format!("CLUT {c}")).unwrap_or_else(|| "-".into());
        println!("  member {num:3} -> CASt res {res:4} -> KEY* CLUT: {clut}");
    }

    println!("\n== CASt chunks (source_id) -> name/type/clutId ==");
    for (i, c) in root.children_by(b"CASt").iter().enumerate() {
        let sid = c.source_id.unwrap_or(i as u32);
        let Ok(cm) = cast::read_cast_member(c) else { continue };
        let name = member_name(&cm);
        let mut clut_id = String::new();
        if cm.cast_info_size > 0 && cm.cast_data_size > 0 {
            let info_start = 12 + cm.cast_info_size as usize;
            let sd_len = cm.cast_data_size as usize;
            let sd = if info_start + sd_len <= cm.raw_data.len() {
                &cm.raw_data[info_start..info_start + sd_len]
            } else if info_start <= cm.raw_data.len() {
                &cm.raw_data[info_start..]
            } else {
                &[]
            };
            let info = bitd::parse_d7_bitmap_info(sd);
            if info.width > 0 && info.height > 0 {
                clut_id = format!(" clutId={} {}x{} bpp={} bitd={:?}", info.palette_id, info.width, info.height, info.bits_per_pixel, member_bitd.get(&sid));
            }
        }
        println!("  [{i:2}] res={sid:5} type={:?} name={name:?}{clut_id}", cm.member_type);
    }

    println!("\n== CLUT resources ==");
    for c in root.children_by(b"CLUT") {
        let id = c.source_id.unwrap_or(0);
        let n = c.data().len();
        println!("  res={id:5} size={n}");
    }

    println!("\n== specificData hex for key members ==");
    let interesting: [&[u8]; 7] = [b"matik_lower", b"roomkiosk_palette", b"Roomkiosk Object Palette", b"infokiosk_a_0_4_0", b"matik_lo", b"matik_sids", b"matik_screen"];
    for (i, c) in root.children_by(b"CASt").iter().enumerate() {
        let sid = c.source_id.unwrap_or(i as u32);
        let Ok(cm) = cast::read_cast_member(c) else { continue };
        if cm.cast_info_size == 0 || cm.cast_data_size == 0 { continue; }
        let name = member_name(&cm);
        if !interesting.iter().any(|n| n == &name.as_bytes()) { continue; }
        let info_start = 12 + cm.cast_info_size as usize;
        let sd = &cm.raw_data[info_start..(info_start + cm.cast_data_size as usize).min(cm.raw_data.len())];
        println!("  res={sid:5} name={name:?} sd({}): {:02x?}", sd.len(), sd);
    }
    let _ = clut::system_mac_palette();
}
