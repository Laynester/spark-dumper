// List ALL members with palette_id == 0 or has_palette false, with their
// first distinct pixel colors from the OLD export.
use director_core::cast;
use director_rifx;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: probe_bg <file.cct>");
    let root = director_rifx::read_file(&path)?;
    let cas: Vec<_> = root.children_by(b"CASt").into_iter().collect();
    let mut n = 0;
    for (i, c) in cas.iter().enumerate() {
        let Ok(cm) = cast::read_cast_member(c) else { continue };
        if cm.member_type != cast::CastMemberType::Bitmap { continue; }
        let is_d5 = cm.cast_info_size > 0 && cm.cast_data_size > 0 && cm.raw_data.len() >= 12;
        let sd = if is_d5 {
            let s = 12 + cm.cast_info_size as usize;
            let l = cm.cast_data_size as usize;
            if s + l <= cm.raw_data.len() { &cm.raw_data[s..s+l] } else if s <= cm.raw_data.len() { &cm.raw_data[s..] } else { &[] }
        } else { &cm.raw_data };
        let info = director_core::bitd::parse_d7_bitmap_info(sd);
        if info.palette_id == 0 && info.bits_per_pixel > 1 {
            n += 1;
            if n <= 6 {
                println!("member {} (chunk {}) bpp={} has_palette={} palette_id={}", i+1, i, info.bits_per_pixel, info.has_palette, info.palette_id);
            }
        }
    }
    println!("total bpp>1 palette_id=0 members: {n}");
    Ok(())
}
