//! Export module — LibreShockwave-style cast exporter.
//!
//! Produces a per-movie project folder:
//!   <name>/
//!     movie.txt                  ← stage/config (tab-separated, like LibreShockwave)
//!     casts.txt                  ← cast library list
//!     bitmaps/0001_bitmap_<name>.png  + .pal (JASC-PAL) + .regpoint (regX/regY)
//!     sounds/0001_sound_<name>.wav | .aiff | .mp3 | .bin
//!     scripts/0001_script_<name>.ls   ← disassembled Lingo
//!     texts/0001_text_<name>.txt
//!     palettes/0001_palette_<name>.pal
//!     fonts/0001_font_<name>.ttf   (+ NNNN_<fontName>.ttf from XMED/PFR1 links)
//!     fonts.txt                     ← font map manifest (Fmap)
//!     shapes/0001_shape_<name>.bin
//!
//! Cast members are sorted into per-type subfolders, mirroring LibreShockwave's
//! CastExporter layout (cpp/apps/tools/CastExporter.cpp). Filenames use the
//! 1-based cast member NUMBER (CAS* index + DRCF minMember) zero-padded to 4
//! digits, so they match LibreShockwave exports exactly
//! (e.g. `0001_bitmap_pool_tablep_a_0_0_0.png`).

use std::path::Path;
use std::fs;

use director_rifx::Chunk;
use director_core::{cast, key, clut, bitd, decomp, font, lingo, lscr, stxt, sound};
use director_core::cast::CastListEntry;

/// Export a Director file's contents to a project folder.
pub fn export_project(
    root: &Chunk,
    output_dir: &Path,
    name: &str,
    lasm: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let base = output_dir.join(sanitize_filename(name));
    fs::create_dir_all(&base)?;

    write_movie_txt(root, &base)?;
    write_casts_txt(root, &base, name)?;
    write_linked_casts_txt(root, &base)?;
    export_cast_members(root, &base, lasm)?;
    export_movie_fonts(root, &base)?;

    Ok(())
}

/// Effective row pitch for a bitmap (stored pitch, else minimum padded to even).
fn pitch_for(info: &bitd::BitmapInfo) -> usize {
    if info.pitch > 0 {
        info.pitch as usize
    } else {
        let min_pitch = (info.width as usize * info.bits_per_pixel as usize + 7) / 8;
        if min_pitch % 2 == 1 {
            min_pitch + 1
        } else {
            min_pitch
        }
    }
}

/// LibreShockwave closestMacLikePaletteForIndexCounts: score every file CLUT
/// against combined palette-index usage. `covered` counts used indices whose
/// candidate entry is non-black (a black entry means the palette carries no
/// data for that index); ties break on squared-RGB distance to the System Mac
/// palette at the same index. Adopt the winner only when it covers at least
/// half the used pixels. Returns the winning CLUT's index in `palettes` and the
/// covered/total tallies, or None (keep SystemMac).
fn closest_mac_like(
    used_counts: &[i64; 256],
    total: i64,
    palettes: &[Vec<(u8, u8, u8)>],
    mac_colors: &[(u8, u8, u8)],
) -> Option<(usize, i64, i64)> {
    if total == 0 {
        return None;
    }
    let mut best: Option<(usize, i64, i64)> = None;
    let mut best_covered = i64::MIN;
    let mut best_mac_dist = i64::MAX;
    for (ci, colors) in palettes.iter().enumerate() {
        if colors.is_empty() {
            continue;
        }
        let mut covered = 0i64;
        let mut mac_dist = 0i64;
        for (index, &count) in used_counts.iter().enumerate() {
            if count == 0 {
                continue;
            }
            if index >= colors.len() || index >= mac_colors.len() {
                mac_dist += 255 * 255 * 3 * count;
                continue;
            }
            let c = colors[index];
            if c != (0, 0, 0) {
                covered += count;
            }
            let m = mac_colors[index];
            let dr = c.0 as i64 - m.0 as i64;
            let dg = c.1 as i64 - m.1 as i64;
            let db = c.2 as i64 - m.2 as i64;
            mac_dist += (dr * dr + dg * dg + db * db) * count;
        }
        if covered > best_covered || (covered == best_covered && mac_dist < best_mac_dist) {
            best_covered = covered;
            best_mac_dist = mac_dist;
            best = Some((ci, covered, mac_dist));
        }
    }
    if let Some((ci, covered, _)) = best {
        if covered * 2 >= total {
            Some((ci, covered, total))
        } else {
            None
        }
    } else {
        None
    }
}

/// LibreShockwave resolveUnlinkedMacLikePaletteForPaletteId: when a positive
/// clutId fails to resolve to a linked CLUT, aggregate the raw palette-index
/// usage across every bitmap member referencing that clutId (decoded against
/// SystemMac), then pick the single file CLUT best covering the combined usage
/// (one palette for the whole group — the members were authored against one
/// missing palette). Returns clutId -> (palette index in `palettes`, covered,
/// total) when adopted, else clutId -> None (keep SystemMac).
fn resolve_unlinked_defaults(
    root: &Chunk,
    member_bitd: &std::collections::HashMap<u32, u32>,
    palettes: &[Vec<(u8, u8, u8)>],
    mac_colors: &[(u8, u8, u8)],
) -> std::collections::HashMap<i16, Option<(usize, i64, i64)>> {
    use std::collections::{HashMap, HashSet};

    let mut counts_by_clut: HashMap<i16, [i64; 256]> = HashMap::new();
    let mut totals_by_clut: HashMap<i16, i64> = HashMap::new();
    let mut clut_ids: HashSet<i16> = HashSet::new();

    for (i, cast_chunk) in root.children_by(b"CASt").iter().enumerate() {
        let member_id = cast_chunk.source_id.unwrap_or(i as u32);
        let Ok(cm) = cast::read_cast_member(cast_chunk) else { continue };
        if cm.member_type != cast::CastMemberType::Bitmap && cm.member_type != cast::CastMemberType::Picture {
            continue;
        }
        if !(cm.cast_info_size > 0 && cm.cast_data_size > 0 && cm.raw_data.len() >= 12) {
            continue;
        }
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
        if info.width == 0 || info.height == 0 || info.palette_id <= 0 {
            continue;
        }
        if !matches!(info.bits_per_pixel, 1 | 2 | 4 | 8) {
            continue;
        }
        let Some(bitd_id) = member_bitd.get(&member_id) else { continue };
        let Some(bc) = root.children.iter().find(|c| c.source_id == Some(*bitd_id)) else {
            continue;
        };
        let pitch = pitch_for(&info);
        let Ok(indices) = bitd::decompress_rle(
            bc.data(),
            info.width as usize,
            info.height as usize,
            info.bits_per_pixel,
            pitch,
        ) else {
            continue;
        };
        let counts = counts_by_clut.entry(info.palette_id).or_insert([0i64; 256]);
        for idx in indices {
            counts[idx as usize] += 1;
        }
        *totals_by_clut.entry(info.palette_id).or_insert(0) +=
            (info.width as u32 * info.height as u32) as i64;
        clut_ids.insert(info.palette_id);
    }

    let mut out = HashMap::new();
    for cid in clut_ids {
        let counts = counts_by_clut.get(&cid).unwrap();
        let total = *totals_by_clut.get(&cid).unwrap();
        out.insert(cid, closest_mac_like(counts, total, palettes, mac_colors));
    }
    out
}

/// LibreShockwave resolveFromChunkId + paletteFromChunk: palettes resolved
/// through the KEY* chain get the gray tail fill (indices 246-254).
fn resolve_from_key(
    res: u32,
    member_clut: &std::collections::HashMap<u32, u32>,
    clut_by_id: &std::collections::HashMap<u32, Vec<(u8, u8, u8)>>,
) -> Option<Vec<(u8, u8, u8)>> {
    let colors = member_clut.get(&res).and_then(|c| clut_by_id.get(c))?;
    Some(clut::mac_like_tail_fill(colors))
}

/// LibreShockwave PaletteResolver::resolveExact(paletteId) for a stored
/// clutId: LS reads `paletteId = storedI16 - 1`, so the stored value maps to
/// member number = stored, and the fallbacks use paletteId (= stored - 1).
/// Order: member number (KEY* CLUT child) -> chunk id == paletteId (KEY* CLUT
/// child) -> CLUT chunk with resource id == paletteId or paletteId+1 ->
/// palette member at index paletteId (its KEY* CLUT child). All results go
/// through paletteFromChunk (gray tail fill).
fn resolve_exact_colors(
    clut_id: i16,
    member_num_map: &std::collections::HashMap<u32, u32>,
    member_clut: &std::collections::HashMap<u32, u32>,
    clut_by_id: &std::collections::HashMap<u32, Vec<(u8, u8, u8)>>,
    palette_member_res: &[u32],
) -> Option<Vec<(u8, u8, u8)>> {
    if clut_id <= 0 {
        return None;
    }
    let palette_id = clut_id - 1;
    // 1. member number = stored clutId (LS: memberNumber = paletteId + 1).
    if let Some(colors) = member_num_map
        .get(&(clut_id as u32))
        .and_then(|res| resolve_from_key(*res, member_clut, clut_by_id))
    {
        return Some(colors);
    }
    // 2. resolveFromChunkId(paletteId): KEY* owner chunk id == paletteId.
    if palette_id > 0 {
        if let Some(colors) = resolve_from_key(palette_id as u32, member_clut, clut_by_id) {
            return Some(colors);
        }
    }
    // 3. CLUT chunk with resource id == paletteId or paletteId+1.
    for id in [palette_id, palette_id + 1] {
        if id > 0 {
            if let Some(colors) = clut_by_id.get(&(id as u32)) {
                return Some(clut::mac_like_tail_fill(colors));
            }
        }
    }
    // 4. palette member at index paletteId (0-based, cast order).
    if palette_id >= 0 {
        if let Some(res) = palette_member_res.get(palette_id as usize) {
            if let Some(colors) = resolve_from_key(*res, member_clut, clut_by_id) {
                return Some(colors);
            }
        }
    }
    None
}

/// CASt resource ids of every Palette member, in cast order (LibreShockwave
/// resolveExact paletteIndex fallback and exportPalette ordering).
fn palette_member_resources(root: &Chunk) -> Vec<u32> {
    root.children_by(b"CASt")
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            cast::read_cast_member(c)
                .ok()
                .filter(|cm| cm.member_type == cast::CastMemberType::Palette)
                .map(|_| c.source_id.unwrap_or(i as u32))
        })
        .collect()
}

/// Sanitize a movie folder name (remove extensions, special chars, control bytes).
fn sanitize_filename(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() { return "export".to_string(); }
    // Remove .cct, .dcr, .rifx extensions
    let name = name
        .strip_suffix(".cct").unwrap_or(name)
        .strip_suffix(".dcr").unwrap_or(name)
        .strip_suffix(".rifx").unwrap_or(name);
    let mut out = String::new();
    for c in name.chars() {
        if c.is_control() {
            continue; // drop NULs and other control characters
        }
        out.push(match c {
            '/' | '\\' | ':' | ' ' | '.' => '_',
            other => other,
        });
    }
    let out = out.trim_matches('_').to_string();
    let out = if out.len() > 64 { out[..64].to_string() } else { out };
    if out.is_empty() { "member".to_string() } else { out }
}

// ---------------------------------------------------------------------------
// movie.txt / casts.txt
// ---------------------------------------------------------------------------

/// Movie-level config, tab-separated, mirroring LibreShockwave's movie.txt.
///
/// Field offsets follow LibreShockwave ConfigChunk::read (D7 DRCF, big-endian):
///   @0  fileVersion (u16)          @2  fileVersion2 (u16; movie_version)
///   @4  stageTop   @6  stageLeft   @8  stageBottom   @10 stageRight (i16)
///   @12 minMember  @14 maxMember  @16 skip(2)
///   @18 d7StageColorG  @19 d7StageColorB   (D7+; D6: skip 2)
///   @20 commentFont  @22 commentSize  @24 commentStyle
///   @26 isRgb (u8)  @27 stageColorR (u8)
///   @28 bgColor (i16, palette index)
///   @30 skip2  @32 skip4  @36 directorVersion (u16)
///   @38 skip2  @40 skip4  @44 skip4  @48 skip4  @52 skip2
///   @54 tempo (i16)  @56 platform (i16)
///   (trailing, if >= 22 bytes left) skip18  @+18 defaultPaletteCastLib (i16)
///                                            @+20 defaultPaletteMember (i16)
/// stage_width/height = stageRight-stageLeft / stageBottom-stageTop (like LS,
/// which yields 0 for the cast-only Habbo CCTs). Colors: bgColor is a PALETTE
/// index, stageColor packs isRgb<<8|R, stageColorRgb is the resolved RGB
/// (isRgb ? R<<16|G<<8|B : systemMac[stageColorR]). Writes the same 16-field
/// layout LibreShockwave's exportMovieConfig emits (verified against
/// exported-old/habbo/movie.txt).
/// Director version from the DRCF config chunk (u16 @ offset 36), used to gate
/// the 32-bit bitmap channel layout (separated planes for Director >= 1000,
/// interleaved ARGB before — LibreShockwave decode32Bit).
fn director_version_of(root: &Chunk) -> u16 {
    root.child(b"DRCF")
        .and_then(|d| {
            let d = d.data();
            if d.len() >= 38 {
                Some(u16::from_be_bytes([d[36], d[37]]))
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn write_movie_txt(root: &Chunk, base: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let Some(drcf) = root.child(b"DRCF") else { return Ok(()) };
    let d = drcf.data();
    // CAS*/DRCF payloads are Mac-format big-endian regardless of wrapper.
    let be16 = |o: usize| -> Option<u16> {
        if d.len() >= o + 2 {
            Some(u16::from_be_bytes([d[o], d[o + 1]]))
        } else {
            None
        }
    };
    let bei16 = |o: usize| -> Option<i16> {
        if d.len() >= o + 2 {
            Some(i16::from_be_bytes([d[o], d[o + 1]]))
        } else {
            None
        }
    };
    let beu8 = |o: usize| -> Option<u8> { d.get(o).copied() };

    // 0xRRGGBB hex like LibreShockwave's colorHex (uppercase, 6 digits).
    let color_hex = |v: i32| format!("0x{:06X}", v & 0xFFFFFF);

    // Director version gates the D7 color layout (LibreShockwave: >= 0x208).
    let director_version = be16(36).unwrap_or(0);
    let d7 = director_version >= 0x208;

    let mut out = String::new();
    if let (Some(l), Some(r)) = (bei16(6), bei16(10)) {
        out.push_str(&format!("stage_width\t{}\n", r as i32 - l as i32));
    }
    if let (Some(t), Some(b)) = (bei16(4), bei16(8)) {
        out.push_str(&format!("stage_height\t{}\n", b as i32 - t as i32));
    }
    if let (Some(l), Some(t)) = (bei16(6), bei16(4)) {
        out.push_str(&format!("stage_left\t{l}\n"));
        out.push_str(&format!("stage_top\t{t}\n"));
    }
    if let (Some(r), Some(b)) = (bei16(10), bei16(8)) {
        out.push_str(&format!("stage_right\t{r}\n"));
        out.push_str(&format!("stage_bottom\t{b}\n"));
    }
    // background_color: the DRCF palette-index bg color (LibreShockwave emits
    // the raw index value, e.g. 0x000020 for index 32).
    if let Some(bg) = bei16(28) {
        out.push_str(&format!("background_color\t{}\n", color_hex(bg as i32)));
    }
    // stage_color (isRgb<<8|R) and stage_color_rgb (resolved RGB).
    if d7 {
        if let (Some(g), Some(b)) = (beu8(18), beu8(19)) {
            if let (Some(is_rgb), Some(r)) = (beu8(26), beu8(27)) {
                let stage_color = ((is_rgb as i32) << 8) | r as i32;
                let stage_color_rgb = if is_rgb != 0 {
                    ((r as i32) << 16) | ((g as i32) << 8) | b as i32
                } else {
                    // Non-RGB: stageColorR indexes the system Mac palette
                    // (LibreShockwave ConfigChunk::read resolves it through
                    // systemMacPalette().getColor). fuse_client's stage color
                    // is index 255 = white — keeping the raw index made the
                    // loading-bar/stage backdrop black.
                    let mac = clut::system_mac_palette();
                    let (cr, cg, cb) = mac.colors.get(r as usize % mac.colors.len()).copied().unwrap_or((0, 0, 0));
                    ((cr as i32) << 16) | ((cg as i32) << 8) | cb as i32
                };
                out.push_str(&format!("stage_color\t{}\n", color_hex(stage_color)));
                out.push_str(&format!("stage_color_rgb\t{}\n", color_hex(stage_color_rgb)));
            }
        }
    } else if let Some(sc) = bei16(26) {
        out.push_str(&format!("stage_color\t{}\n", color_hex(sc as i32)));
    }
    if let Some(t) = be16(54) {
        out.push_str(&format!("tempo\t{t}\n"));
    }
    if let Some(min) = be16(12) { out.push_str(&format!("min_member\t{min}\n")); }
    if let Some(max) = be16(14) { out.push_str(&format!("max_member\t{max}\n")); }
    // default_palette castLib:member (trailing DRCF fields, D7).
    if let Some(platform) = bei16(56) {
        let trailing = 58usize;
        if d.len() >= trailing + 22 {
            let cast_lib = i16::from_be_bytes([d[trailing + 18], d[trailing + 19]]);
            let member = i16::from_be_bytes([d[trailing + 20], d[trailing + 21]]);
            out.push_str(&format!("default_palette\t{cast_lib}:{member}\n"));
        }
        out.push_str(&format!("director_version\t{director_version}\n"));
        if let Some(mv) = be16(2) {
            out.push_str(&format!("movie_version\t{mv}\n"));
        }
        out.push_str(&format!("platform\t{platform}\n"));
    }

    fs::write(base.join("movie.txt"), out)?;
    Ok(())
}

/// External casts linked from this movie — one absolute path per line, derived
/// from the MCsL cast list (every entry with a non-empty path; deduped, in
/// list order). Mirrors LibreShockwave's linked_casts.txt (exportLinkedCasts +
/// DirectorFile::getExternalCastPaths). The bundler/runtime read it to know
/// which cast files the movie links (fuse_client, empty.cst, ...) so the
/// CastLoad Manager can fetch and register them — WITHOUT it the linked casts
/// never load and the boot scripts (e.g. fuse_client's global declarations)
/// never run.
fn write_linked_casts_txt(root: &Chunk, base: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut paths: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for chunk in root.children_by(b"MCsL") {
        for entry in cast::parse_cast_list(chunk.data()) {
            if entry.path.is_empty() || seen.contains(&entry.path) {
                continue;
            }
            seen.insert(entry.path.clone());
            paths.push(entry.path);
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    let mut out = String::from("# External casts linked from this movie, one per line.\n");
    for p in &paths {
        out.push_str(p);
        out.push('\n');
    }
    fs::write(base.join("linked_casts.txt"), out)?;
    Ok(())
}

/// Cast library list, mirroring LibreShockwave's casts.txt. The MCsL (Cast
/// List) chunk holds the movie's FULL cast library table — cast 0 (internal)
/// plus every LINKED cast (external .cst/.cct files) and the pre-allocated
/// empty slots, each with its Director-time file path. Previously this only
/// walked the local CAS* member array, so linked casts (fuse_client, the
/// `empty N` placeholders, …) never appeared — exactly the corruption the
/// CastLoad Manager guards against (`the number of castLibs` must match the
/// movie's real list, not just the casts present in this one file).
fn write_casts_txt(root: &Chunk, base: &Path, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = String::from(
        "# Cast libraries in this movie. path is empty for internal casts; \
         member_count 0 means an empty cast.\nid\tname\tpath\tmin_member\tmax_member\tmember_count\n",
    );

    let mcsl_chunks = root.children_by(b"MCsL");
    let entries: Vec<CastListEntry> = mcsl_chunks
        .iter()
        .flat_map(|c| cast::parse_cast_list(c.data()))
        .collect();

    if !entries.is_empty() {
        // The movie's own cast list — names, paths, member ranges (LibreShockwave
        // exportCastList format, verified byte-identical against habbo.dcr).
        for e in &entries {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                e.id, e.name, e.path, e.min_member, e.max_member, e.member_count
            ));
        }
        fs::write(base.join("casts.txt"), out)?;
        return Ok(());
    }

    // Fallback (no MCsL — older files): the local CAS* member arrays.
    let cas_chunks = root.children_by(b"CAS*");
    if cas_chunks.is_empty() {
        return Ok(());
    }
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

    for (i, cas) in cas_chunks.iter().enumerate() {
        let members = cast::parse_cast_member_list(cas.data());
        let member_count = members.iter().filter(|&&r| r != 0).count();
        let max_member = if members.is_empty() {
            0
        } else {
            min_member + members.len() as u32 - 1
        };
        let cast_name = if i == 0 { name } else { &format!("cast{i}") };
        out.push_str(&format!(
            "{i}\t{cast_name}\t\t{min_member}\t{max_member}\t{member_count}\n"
        ));
    }
    fs::write(base.join("casts.txt"), out)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Cast Member Export
// ---------------------------------------------------------------------------

/// Build the cast member number -> CASt resource id map.
///
/// Director D7 bitmap specificData stores `paletteId` as a cast member
/// NUMBER (not a resource id). The CAS* chunk lists the CASt resource ids in
/// member order; entry i has member number `minMember + i` (minMember from the
/// DRCF config). This map is what lets us resolve a stored paletteId to the
/// palette cast member's CASt resource, whose CLUT child (via KEY*) holds the
/// actual 256-color table. Verified against hh_room_pool: stored 53 -> member
/// 53 -> res 628 (pool_palette) -> CLUT 2572.
fn build_member_number_map(root: &Chunk) -> std::collections::HashMap<u32, u32> {
    // CAS*/DRCF payloads are Mac-format big-endian regardless of whether the
    // file is XFIR- or RIFX-wrapped (verified against hh_room_* CCTs). Do NOT
    // switch these reads to chunk.endian — it would silently break the map.
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

/// Subfolder a cast member type exports into (LibreShockwave memberFolderName).
fn member_folder(t: cast::CastMemberType) -> &'static str {
    match t {
        cast::CastMemberType::Bitmap | cast::CastMemberType::Picture => "bitmaps",
        cast::CastMemberType::Sound => "sounds",
        cast::CastMemberType::Script => "scripts",
        cast::CastMemberType::Text | cast::CastMemberType::Button => "texts",
        cast::CastMemberType::Palette => "palettes",
        cast::CastMemberType::Font => "fonts",
        cast::CastMemberType::Shape => "shapes",
        _ => "other",
    }
}

/// Lowercase type name used in filenames (LibreShockwave cast::name).
fn member_type_name(t: cast::CastMemberType) -> &'static str {
    match t {
        cast::CastMemberType::Bitmap | cast::CastMemberType::Picture => "bitmap",
        cast::CastMemberType::Sound => "sound",
        cast::CastMemberType::Script => "script",
        cast::CastMemberType::Text | cast::CastMemberType::Button => "text",
        cast::CastMemberType::Palette => "palette",
        cast::CastMemberType::Font => "font",
        cast::CastMemberType::Shape => "shape",
        _ => "member",
    }
}

/// LibreShockwave sanitizeFileName: keep alnum + ._- , replace the rest with _.
fn sanitize_member_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

fn export_cast_members(root: &Chunk, base: &Path, lasm: bool) -> Result<(), Box<dyn std::error::Error>> {
    let director_version = director_version_of(root);

    // Collect CLUT chunks keyed by their resource id (for KEY*-linked palettes),
    // plus the file-order list used by the unlinked-palette scorer.
    let mut clut_by_id: std::collections::HashMap<u32, Vec<(u8, u8, u8)>> = std::collections::HashMap::new();
    let mut palettes: Vec<Vec<(u8, u8, u8)>> = Vec::new();
    for c in root.children_by(b"CLUT") {
        if let Ok(p) = clut::read_clut(c) {
            if let Some(id) = c.source_id {
                clut_by_id.insert(id, p.colors.clone());
            }
            palettes.push(p.colors.clone());
        }
    }
    // Fallback palette for members whose clutId doesn't resolve (dead positive
    // references in Habbo CCTs). The Mac system palette is the Director default
    // and empirically renders better than the file's first CLUT.
    let default_palette = clut::system_mac_palette().colors;
    let palette: &[(u8, u8, u8)] = &default_palette;

    // KEY* links each cast member (parent) to its child resources. Build
    // member id -> child resource id maps per child type. child_tag is
    // canonical FourCC (key.rs normalizes the stored spelling).
    let mut member_bitd: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut member_clut: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut member_stxt: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut member_snd: std::collections::HashMap<u32, (u32, [u8; 4])> = std::collections::HashMap::new();
    let mut member_xmed: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for key_chunk in root.children_by(b"KEY*") {
        if let Ok(kt) = key::read_key(key_chunk) {
            for e in &kt.entries {
                match &e.child_tag {
                    b"BITD" => { member_bitd.insert(e.parent_index, e.child_index); }
                    b"CLUT" => { member_clut.insert(e.parent_index, e.child_index); }
                    b"STXT" => { member_stxt.insert(e.parent_index, e.child_index); }
                    b"SND " | b"snd " | b"ediM" => {
                        member_snd.insert(e.parent_index, (e.child_index, e.child_tag));
                    }
                    b"XMED" => { member_xmed.insert(e.parent_index, e.child_index); }
                    _ => {}
                }
            }
        }
    }

    // The %04d prefix is the REAL Lingo member number (the CAS*-derived
    // number, minMember + CAS* index), NOT the 1-based CASt chunk position.
    // Director addresses members by these numbers — "the next cast member"
    // semantics (ink 9 Mask: the sprite's member + 1, DirPlayer
    // rendering.rs / LibreShockwave SpriteBaker resolveMaskMember) only
    // survive the export when the prefix keeps the original numbering. The
    // chunk-position ordinal can drift far from the member number (empty
    // slots + resource-id ordering), which is exactly how the pool water
    // (vesi2 -> vesimask2) lost its mask adjacency in earlier exports.
    // Verified against the original ccts: pool vesi1=84 -> vesimask1=85,
    // vesi2=89 -> vesimask2=90, terrace dew_vesi1=33 -> dew_vesimask1=34.
    let member_num_map = build_member_number_map(root);
    // Inverse: CASt resource id -> real member number (the CAS* map is
    // member number -> resource id). Members with no CAS* entry (unnapped
    // chunks) fall back to the chunk-position ordinal below.
    let member_num_of: std::collections::HashMap<u32, u32> = member_num_map
        .iter()
        .map(|(num, res)| (*res, *num))
        .collect();

    // Palette members whose clutId fails the KEY* chain get the LibreShockwave
    // unlinked-mac-like resolution: aggregate index usage across every member
    // with that clutId and pick the file CLUT best covering it.
    let unlinked_defaults = resolve_unlinked_defaults(root, &member_bitd, &palettes, &default_palette);

    // Text members whose STXT child isn't in KEY* fall back to the STXT chunk
    // at the same ordinal among text members (Habbo CCT pattern), then to the
    // first STXT chunk.
    let stxt_chunks = root.children_by(b"STXT").to_vec();
    let mut text_member_index = 0usize;

    for (i, cast_chunk) in root.children_by(b"CASt").iter().enumerate() {
        let member_id = cast_chunk.source_id.unwrap_or(i as u32);
        let Ok(cm) = cast::read_cast_member(cast_chunk) else { continue };
        let ordinal = i as u32 + 1;
        // The export prefix is the real Lingo member number when the CAS*
        // map knows this member; the chunk-position ordinal only as a
        // fallback for chunks the map doesn't list.
        let member_number = member_num_of.get(&member_id).copied().unwrap_or(ordinal);

        // Member name comes from the CASt info block (D4+), not Lnam (Lnam is
        // the Lingo script-name table).
        let member_name = if cm.cast_info_size > 0 {
            let info = &cm.raw_data[12..(12 + cm.cast_info_size as usize).min(cm.raw_data.len())];
            cast::read_member_name(info)
        } else {
            None
        }
        // Unnamed members fall back to "member_<chunk source id>" (the CASt
        // source_id, i.e. the member number), matching LibreShockwave's
        // `safeName = "member_" + member->id()` — not the ordinal.
        .unwrap_or_else(|| format!("member_{member_id}"));
        let safe_name = sanitize_member_name(&member_name);
        let safe_name = if safe_name.is_empty() {
            format!("member_{member_id}")
        } else {
            safe_name
        };

        let base_name = format!("{:04}_{}_{}", member_number, member_type_name(cm.member_type), safe_name);
        // Dir is created lazily by each exporter right before its first write,
        // so movies with members that export nothing (no scriptId, no STXT,
        // unknown types) don't get stray empty folders.
        let dir = base.join(member_folder(cm.member_type));

        let result = match cm.member_type {
            cast::CastMemberType::Bitmap | cast::CastMemberType::Picture => export_bitmap(
                &cm, root, member_bitd.get(&member_id).copied(), member_id,
                &dir, &base_name, &member_clut, &clut_by_id, &member_num_map,
                palette, &unlinked_defaults, &palettes, director_version,
            ),
            cast::CastMemberType::Script => export_script(root, &cm, &dir, &base_name, lasm),
            cast::CastMemberType::Text | cast::CastMemberType::Button => {
                let stxt = member_stxt
                    .get(&member_id)
                    .and_then(|res| root.children.iter().find(|c| c.is(b"STXT") && c.source_id == Some(*res)))
                    .or_else(|| stxt_chunks.get(text_member_index).copied());
                let r = export_text(stxt, &dir, &base_name);
                text_member_index += 1;
                r
            }
            cast::CastMemberType::Sound => export_sound(root, &cm, &dir, &base_name, &member_snd, member_id),
            cast::CastMemberType::Palette => {
                // The palette member's OWN CLUT child (KEY* parent = this
                // member's CASt resource). This was previously resolved as
                // member number `palette_member_index + 1` (the chunk ordinal
                // among palette members), which is only correct when palette
                // chunk order happens to equal member numbering. In
                // hh_room_floorlobbies the palette chunks run in member order
                // [7, 6, 4, 3, 2, 5, 1, 105, 104], so every palette file was
                // filled from a DIFFERENT member's CLUT (floorlobby_a got
                // floor_lamp's all-black table; the true olive/teal basement
                // table ended up under "topwindows"). Resolve by the member's
                // REAL number so each .pal carries its own table.
                let colors = member_num_map
                    .get(&member_number)
                    .and_then(|res| member_clut.get(res))
                    .and_then(|clut| clut_by_id.get(clut))
                    .map(|c| clut::mac_like_tail_fill(c))
                    .or_else(|| member_clut.get(&member_id).and_then(|c| clut_by_id.get(c)).map(|c| clut::mac_like_tail_fill(c)));
                let r = export_palette(&dir, &base_name, colors.as_ref());
                r
            }
            cast::CastMemberType::Font => export_font(root, &cm, &dir, &base_name, &member_xmed, member_id),
            cast::CastMemberType::Shape => export_shape(&cm, &dir, &base_name),
            _ => Ok(()),
        };

        if let Err(e) = result {
            eprintln!("  FAIL {base_name}: {e}");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Palette resolution report (--palettes)
// ---------------------------------------------------------------------------

/// Print how every bitmap member's palette resolves: built-in system palette,
/// a CLUT linked via KEY*, or the fallback CLUT — plus how the render looks
/// (distinct colors / % black) so palette bugs are visible at a glance.
pub fn report_palettes(root: &Chunk) {
    let director_version = director_version_of(root);
    // CLUT resources by resource id.
    let mut clut_by_id: std::collections::HashMap<u32, Vec<(u8, u8, u8)>> = std::collections::HashMap::new();
    let mut palettes: Vec<Vec<(u8, u8, u8)>> = Vec::new();
    for c in root.children_by(b"CLUT") {
        if let Ok(p) = clut::read_clut(c) {
            if let Some(id) = c.source_id {
                clut_by_id.insert(id, p.colors.clone());
            }
            palettes.push(p.colors.clone());
        }
    }
    // SystemMac is the fallback for unresolvable clutIds (see export_bitmap).
    let builtin_mac_colors = clut::system_mac_palette().colors;

    // member -> BITD resource, member -> CLUT resource (via KEY*)
    let mut member_bitd: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut member_clut: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for key_chunk in root.children_by(b"KEY*") {
        if let Ok(kt) = key::read_key(key_chunk) {
            for e in &kt.entries {
                if &e.child_tag == b"BITD" {
                    member_bitd.insert(e.parent_index, e.child_index);
                } else if &e.child_tag == b"CLUT" {
                    member_clut.insert(e.parent_index, e.child_index);
                }
            }
        }
    }

    let member_num_map = build_member_number_map(root);

    let unlinked_defaults = resolve_unlinked_defaults(root, &member_bitd, &palettes, &builtin_mac_colors);

    let mut fallback_count = 0usize;
    let mut fallback_members = Vec::new();
    println!("=== Palette Resolution ===");
    for (i, cast_chunk) in root.children_by(b"CASt").iter().enumerate() {
        let member_id = cast_chunk.source_id.unwrap_or(i as u32);
        let Ok(cm) = cast::read_cast_member(cast_chunk) else { continue };
        if cm.member_type != cast::CastMemberType::Bitmap {
            continue;
        }
        let is_d5 = cm.cast_info_size > 0 && cm.cast_data_size > 0 && cm.raw_data.len() >= 12;
        if !is_d5 {
            continue;
        }
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
        if info.width == 0 || info.height == 0 {
            continue;
        }

        // Resolve to owned colors first (built-in palettes are heap-allocated),
        // then pick a reference for decoding.
        let builtin = clut::builtin_palette_for_clut_id(info.palette_id);
        let chosen; // assigned in every branch before use
        let mut used_fallback = false;
        let mut owned_colors: Option<Vec<(u8, u8, u8)>> = None;
        let palette_members = palette_member_resources(root);
        // LibreShockwave BitmapInfo::parse reads a stored clutId ONLY when the
        // pitch flag (0x8000) is set (paletteId = stored - 1). No-flag 1-bit
        // mask members leave paletteId at its initializer 0, so they resolve
        // via resolveExact(0) -> palette member index 0, not SystemMac.
        let clut_key = if info.has_palette { info.palette_id } else { 1 };
        let palette_used: Option<&[(u8, u8, u8)]> = if clut_key > 0 {
            // Full LibreShockwave resolveExact chain: member number -> chunk id
            // -> CLUT res id -> palette member index (all with gray tail fill),
            // then the unlinked-mac-like aggregation, then SystemMac.
            let owned = resolve_exact_colors(
                clut_key,
                &member_num_map,
                &member_clut,
                &clut_by_id,
                palette_members.as_slice(),
            );
            if let Some(colors) = owned {
                chosen = if info.has_palette {
                    format!("resolveExact (paletteId {})", info.palette_id)
                } else {
                    "resolveExact (paletteId 0, 1-bit mask -> palette member index 0)".to_string()
                };
                owned_colors = Some(colors);
                owned_colors.as_deref()
            } else if let Some(Some((ci, covered, total))) = unlinked_defaults.get(&clut_key) {
                chosen = format!(
                    "UNLINKED-CLUT[{}] covered {covered}/{total} (paletteId {} unresolvable)",
                    ci, clut_key
                );
                palettes.get(*ci).map(|p| p.as_slice())
            } else {
                used_fallback = true;
                chosen = format!("FALLBACK SystemMac (paletteId {} unresolvable)", clut_key);
                Some(&builtin_mac_colors)
            }
        } else {
            match &builtin {
                Some(b) => {
                    chosen = match info.palette_id {
                        0 => "builtin(SystemMac)".to_string(),
                        -2 => "builtin(Grayscale)".to_string(),
                        -101 => "builtin(SystemWinD5)".to_string(),
                        _ => format!("builtin({})", info.palette_id),
                    };
                    Some(b.colors.as_slice())
                }
                None => {
                    used_fallback = true;
                    chosen = format!("FALLBACK SystemMac (clutId {} unhandled)", info.palette_id);
                    Some(&builtin_mac_colors)
                }
            }
        };
        let Some(colors) = palette_used else { continue };
        if used_fallback {
            fallback_count += 1;
            fallback_members.push(member_id);
        }

        // Decode and measure.
        let bitd_chunk = member_bitd
            .get(&member_id)
            .and_then(|id| root.children.iter().find(|c| c.source_id == Some(*id)));
        let (black_frac, distinct) = if let Some(bc) = bitd_chunk {
            let pitch = pitch_for(&info);
            match bitd::decode_to_rgba(
                bc.data(),
                info.width as usize,
                info.height as usize,
                info.bits_per_pixel,
                pitch,
                colors,
                director_version,
            ) {
                Ok(rgba) => {
                    let mut black = 0u32;
                    let mut set = std::collections::HashSet::new();
                    for px in rgba.chunks_exact(4) {
                        if px[0] < 24 && px[1] < 24 && px[2] < 24 {
                            black += 1;
                        }
                        set.insert(((px[0] >> 4), (px[1] >> 4), (px[2] >> 4)));
                    }
                    let n = (info.width as u32 * info.height as u32).max(1);
                    (black as f32 / n as f32, set.len())
                }
                Err(_) => (1.0, 0),
            }
        } else {
            (0.0, 0)
        };

        let name = cast::read_member_name(
            &cm.raw_data[12..(12 + cm.cast_info_size as usize).min(cm.raw_data.len())],
        )
        .unwrap_or_else(|| format!("member_{member_id}"));
        let flag = if black_frac > 0.6 || distinct <= 1 {
            " <-- SUSPICIOUS"
        } else {
            ""
        };
        println!(
            "  member {member_id:4} ({name:40}) clutId={:4} bpp={} {:.0}x{:.0} | {chosen} | {} colors, {:.0}% black{flag}",
            info.palette_id,
            info.bits_per_pixel,
            info.width,
            info.height,
            distinct,
            black_frac * 100.0,
        );

        // For fallback members, show every CLUT's render so the best fallback
        // is visible in one run.
        // For fallback and unlinked-resolved members, show every CLUT's render
        // so the best fallback is visible in one run.
        if chosen.starts_with("FALLBACK") || chosen.starts_with("UNLINKED-CLUT") {
            if let Some(bc) = bitd_chunk {
                let pitch = pitch_for(&info);
                let mut per_clut = String::new();
                for (ci, colors) in palettes.iter().enumerate() {
                    if let Ok(rgba) = bitd::decode_to_rgba(
                        bc.data(),
                        info.width as usize,
                        info.height as usize,
                        info.bits_per_pixel,
                        pitch,
                        colors,
                        director_version,
                    ) {
                        let mut black = 0u32;
                        let mut set = std::collections::HashSet::new();
                        for px in rgba.chunks_exact(4) {
                            if px[0] < 24 && px[1] < 24 && px[2] < 24 {
                                black += 1;
                            }
                            set.insert(((px[0] >> 4), (px[1] >> 4), (px[2] >> 4)));
                        }
                        let n = (info.width as u32 * info.height as u32).max(1);
                        per_clut.push_str(&format!(
                            " CLUT[{ci}]={}col/{:.0}%blk",
                            set.len(),
                            black as f32 / n as f32 * 100.0
                        ));
                    }
                }
                // SystemMac for comparison
                let mac = clut::system_mac_palette();
                if let Ok(rgba) = bitd::decode_to_rgba(
                    bc.data(),
                    info.width as usize,
                    info.height as usize,
                    info.bits_per_pixel,
                    pitch,
                    &mac.colors,
                    director_version,
                ) {
                    let mut black = 0u32;
                    let mut set = std::collections::HashSet::new();
                    for px in rgba.chunks_exact(4) {
                        if px[0] < 24 && px[1] < 24 && px[2] < 24 {
                            black += 1;
                        }
                        set.insert(((px[0] >> 4), (px[1] >> 4), (px[2] >> 4)));
                    }
                    let n = (info.width as u32 * info.height as u32).max(1);
                    per_clut.push_str(&format!(
                        " SystemMac={}col/{:.0}%blk",
                        set.len(),
                        black as f32 / n as f32 * 100.0
                    ));
                }
                println!("       alternatives:{per_clut}");
            }
        }
    }
    println!("\n{fallback_count} members fell back to SystemMac: {:?}", fallback_members);
}

// ---------------------------------------------------------------------------
// Bitmap export → PNG (+ .pal + .regpoint sidecars)
// ---------------------------------------------------------------------------

/// Scan CASt/BITD data for D3 child resource entries and find where pixel data starts.
///
/// The D3 format stores 12-byte child resource entries before the actual pixel data.
/// Each entry: u32 le_tag + u32 le_childId + u32 fourCC.
///
/// Uses a two-phase approach:
/// Phase 1: byte-by-byte scan to find the FIRST valid entry (known fourCC + valid
///   tag in 1000-15000, child < 5000).
/// Phase 2: 12-byte stride from first entry, accepting ANY valid tag+child combination
///   regardless of fourCC. This handles entries with unknown fourCC types that
///   appear in Habbo CCTs (e.g. TXTS, ediM). Breaks after 3 consecutive failures
///   to avoid false-positives in RLE-compressed pixel data.
fn find_d3_pixel_data_offset(data: &[u8]) -> Option<usize> {
    let sl = data.len();
    if sl < 16 {
        return None;
    }

    // Known fourCCs for Phase 1 byte-by-byte detection.
    // TXTS (STXT reversed) and ediM (MIDE reversed) appear in Habbo CCT entries.
    let known_fourccs: &[&[u8]] = &[
        b"DTIB", b"muhT", b"TULC", b"FCRD", b"rcsL", b"tXTS", b" DNS",
        b"TXTS", b"ediM", b"fniC", b"*SAC",
    ];

    // Phase 1: find the first valid entry (byte-by-byte scan)
    let mut first_entry_pos = None;
    for p in 0..sl.saturating_sub(11) {
        let candidate = &data[p + 8..p + 12];
        if known_fourccs.contains(&candidate) {
            let tag = u32::from_le_bytes([data[p], data[p+1], data[p+2], data[p+3]]);
            let child_id = u32::from_le_bytes([data[p+4], data[p+5], data[p+6], data[p+7]]);
            if (1000..=15000).contains(&tag) && child_id < 5000 {
                first_entry_pos = Some(p);
                break;
            }
        }
    }

    let first_pos = first_entry_pos?;
    let mut last_valid_end = first_pos + 12;
    let mut consecutive_fails = 0;
    let max_fails = 3;

    // Phase 2: 12-byte stride. Accept ANY valid tag+child regardless of fourCC.
    let mut pos = first_pos + 12;
    while pos + 12 <= sl {
        let tag = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]);
        let child_id = u32::from_le_bytes([data[pos+4], data[pos+5], data[pos+6], data[pos+7]]);

        if (1000..=15000).contains(&tag) && child_id < 5000 {
            last_valid_end = pos + 12;
            consecutive_fails = 0;
        } else {
            consecutive_fails += 1;
            if consecutive_fails >= max_fails {
                break;
            }
        }

        pos += 12;
    }

    Some(last_valid_end)
}

/// JASC-PAL palette text (LibreShockwave paletteExportText).
fn jasc_pal(colors: &[(u8, u8, u8)]) -> String {
    let mut out = format!("JASC-PAL\n0100\n{}\n", colors.len());
    for (r, g, b) in colors {
        out.push_str(&format!("{r} {g} {b}\n"));
    }
    out
}

fn export_bitmap(
    cm: &cast::CastMember,
    root: &Chunk,
    bitd_id: Option<u32>,
    _member_id: u32,
    dir: &Path,
    base_name: &str,
    member_clut: &std::collections::HashMap<u32, u32>,
    clut_by_id: &std::collections::HashMap<u32, Vec<(u8, u8, u8)>>,
    member_num_map: &std::collections::HashMap<u32, u32>,
    fallback_palette: &[(u8, u8, u8)],
    unlinked_defaults: &std::collections::HashMap<i16, Option<(usize, i64, i64)>>,
    palettes: &[Vec<(u8, u8, u8)>],
    director_version: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let is_d5 = cm.cast_info_size > 0 && cm.cast_data_size > 0 && cm.raw_data.len() >= 12;

    let (width, height, bpp, pitch, pixel_data, info): (usize, usize, u8, usize, Vec<u8>, bitd::BitmapInfo) =
        if is_d5 {
            // D5+ (incl. D7 Habbo CCTs): the trailing `dataLen` bytes of the CASt
            // member hold the bitmap info; the pixels live in the BITD resource
            // linked through KEY*.
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
            if info.width == 0 || info.height == 0 {
                return Ok(()); // no bitmap here
            }
            let w = info.width as usize;
            let h = info.height as usize;
            let bpp = info.bits_per_pixel;
            let pitch = if info.pitch > 0 { info.pitch as usize } else {
                let min_pitch = (w * bpp as usize + 7) / 8;
                if min_pitch % 2 == 1 { min_pitch + 1 } else { min_pitch }
            };
            let bitd_chunk = bitd_id
                .and_then(|id| root.children.iter().find(|c| c.source_id == Some(id)));
            let Some(bitd_chunk) = bitd_chunk else {
                return Ok(()); // no BITD resource for this member
            };
            (w, h, bpp, pitch, bitd_chunk.data().to_vec(), info)
        } else {
            // D3 fallback: dimensions from the D3 header, pixels inline after
            // the child resource entries.
            let endian = root.endian;
            let (w, h) = match cast::CastMemberType::parse_d3_bitmap_dims(&cm.raw_data, endian) {
                Some((w, h)) => (w as usize, h as usize),
                None => return Ok(()),
            };
            let bpp = 8u8;
            let pitch = (w * bpp as usize + 7) / 8;
            let pitch = if pitch % 2 == 1 { pitch + 1 } else { pitch };
            let data = if let Some(offset) = find_d3_pixel_data_offset(&cm.raw_data) {
                let remaining = cm.raw_data.len() - offset;
                if remaining > 50 {
                    cm.raw_data[offset..].to_vec()
                } else {
                    cm.raw_data.clone()
                }
            } else {
                cm.raw_data.clone()
            };
            (w, h, bpp, pitch, data, bitd::BitmapInfo::default())
        };

    if width == 0 || height == 0 || bpp == 0 {
        return Ok(());
    }

    // Resolve this member's palette. The stored paletteId is a cast member
    // NUMBER (not a resource id): number -> CASt resource (CAS*) -> that
    // palette member's CLUT child (KEY* -> CLUT resource) -> 256 colors.
    //   clutId <= 0 → built-in system palette (stored id - 1:
    //                 0→SystemMac, -2→Grayscale, -101→SystemWinD5, ...)
    let builtin = clut::builtin_palette_for_clut_id(info.palette_id);
    let mut owned_palette: Option<Vec<(u8, u8, u8)>> = None;
    // LibreShockwave BitmapInfo::parse reads a stored clutId ONLY when the
    // pitch flag (0x8000) is set (paletteId = stored - 1). No-flag 1-bit
    // mask members leave paletteId at its initializer 0, so they resolve via
    // resolveExact(0) -> palette member index 0, not SystemMac. Only applies
    // to D5+ members; D3 fallback members keep builtin(SystemMac).
    let clut_key = if is_d5 && !info.has_palette {
        1
    } else {
        info.palette_id
    };
    let member_palette: &[(u8, u8, u8)] = if clut_key > 0 {
        // Full LibreShockwave resolveExact chain: member number -> chunk id ->
        // CLUT res id -> palette member index (all with the gray tail fill),
        // then the unlinked-mac-like aggregation, then SystemMac.
        owned_palette = resolve_exact_colors(
            clut_key,
            member_num_map,
            member_clut,
            clut_by_id,
            palette_member_resources(root).as_slice(),
        );
        if let Some(colors) = owned_palette.as_deref() {
            colors
        } else if let Some(colors) = unlinked_defaults
            .get(&clut_key)
            .and_then(|opt| *opt)
            .and_then(|(ci, _, _)| palettes.get(ci))
        {
            colors.as_slice()
        } else {
            fallback_palette
        }
    } else {
        builtin
            .as_ref()
            .map(|p| p.colors.as_slice())
            .unwrap_or(fallback_palette)
    };
    let rgba = bitd::decode_to_rgba(
        &pixel_data,
        width,
        height,
        bpp,
        pitch,
        member_palette,
        director_version,
    )
    .map_err(|e| format!("decode: {e}"))?;

    fs::create_dir_all(dir)?;
    let png_path = dir.join(format!("{base_name}.png"));
    // 8-bit palette bitmaps are written as INDEXED PNGs (color type 3, PLTE =
    // the member's palette) so the pixel INDICES survive the export. The
    // runtime's `member.paletteRef = member(<pattern palette>)` remap (room
    // wall/floor patterns: Private Room Engine setWallPaper/setFloorPattern)
    // recolors each pixel by its index through the target palette — an RGBA
    // export loses that mapping when several indices share one color (the
    // fuzzy floor tile's dither interior is all-white in its own palette but
    // a white/gray checkerboard through floor_fuzzy1). Indexed is also
    // smaller (1 byte/px vs 4). All other bit depths keep the RGBA export.
    if bpp == 8 && !member_palette.is_empty() {
        let indices = bitd::decompress_rle(&pixel_data, width, height, bpp, pitch)
            .map_err(|e| format!("index decode: {e}"))?;
        let pal: Vec<u8> = member_palette
            .iter()
            .take(256)
            .flat_map(|(r, g, b)| [*r, *g, *b])
            .collect();
        let file = std::fs::File::create(&png_path)?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
        enc.set_color(png::ColorType::Indexed);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_palette(pal);
        let mut writer = enc.write_header()?;
        writer.write_image_data(&indices)?;
    } else {
        let Some(img) = image::RgbaImage::from_raw(width as u32, height as u32, rgba) else {
            return Err("Failed to create RgbaImage from raw pixels".into());
        };
        img.save(&png_path)?;
    }

    // .pal sidecar (JASC-PAL) — skipped for 32bpp+ (no palette; LibreShockwave
    // exportBitmapPalette rule).
    if bpp < 32 && !member_palette.is_empty() {
        fs::write(dir.join(format!("{base_name}.pal")), jasc_pal(member_palette))?;
    }
    // .regpoint sidecar
    let reg = format!("regX={}\nregY={}\n", info.reg_x, info.reg_y);
    fs::write(dir.join(format!("{base_name}.regpoint")), reg)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Script export → decompiled Lingo source (.ls, LibreShockwave header style)
// ---------------------------------------------------------------------------

fn export_script(
    root: &Chunk,
    cm: &cast::CastMember,
    dir: &Path,
    base_name: &str,
    lasm: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // A script member's info block stores a `scriptId` — a 1-based index into
    // the LctX ScriptContext entries. That entry's `sectionId` is the LSCR
    // resource id holding the bytecode; the LctX also names the Lnam resource
    // whose name table resolves handler name ids to identifiers.
    // (Verified against hh_room_park/fuse_client: member 652 park_a -> scriptId
    // 4 -> LctX entry[3] -> Lscr 770; fuse Object API -> 4 -> Lscr 142.)
    let info = if cm.cast_info_size > 0 {
        &cm.raw_data[12..(12 + cm.cast_info_size as usize).min(cm.raw_data.len())]
    } else {
        &cm.raw_data[12..]
    };
    let Some(script_id) = cast::read_member_script_id(info) else {
        return Ok(()); // no scriptId → script lives in another cast
    };

    let Some(lctx) = root
        .children_by(b"LctX")
        .into_iter()
        .chain(root.children_by(b"Lctx"))
        .next()
    else {
        return Ok(());
    };
    let ctx = lscr::read_script_context(lctx)?;
    let Some(lscr_res) = ctx.script_resource(script_id) else {
        return Ok(()); // scriptId not in LctX → shared/foreign script
    };
    let Some(lscr_chunk) = root
        .children
        .iter()
        .find(|c| c.is(b"Lscr") && c.source_id == Some(lscr_res))
    else {
        return Err(format!("Lscr resource {lscr_res} not found in file").into());
    };

    // Optional Lnam name table (named by the LctX) for handler identifiers.
    let names = (ctx.lnam_section_id > 0).then(|| {
        root.children
            .iter()
            .find(|c| c.is(b"Lnam") && c.source_id == Some(ctx.lnam_section_id as u32))
            .and_then(|c| lscr::read_script_names(c).ok())
    }).flatten();

    let script = lscr::read_script(lscr_chunk, true)?;

    // Director file version (DRCF directorVersion @36). LibreShockwave's
    // DirectorFile::version() = config_->directorVersion(), which drives the
    // decompiler's dot-syntax switch (>= 700) and variable multiplier.
    let version = root
        .child(b"DRCF")
        .map(|d| {
            let data = d.data();
            if data.len() >= 38 {
                u16::from_be_bytes([data[36], data[37]]) as i32
            } else {
                0
            }
        })
        .unwrap_or(0);

    // `-- Type:` header uses the RESOLVED script type, replicating
    // LibreShockwave ScriptChunk::resolvedScriptType -> DirectorFile::
    // getScriptType: first the KEY* owner cast member of the Lscr chunk, then
    // the LctX scriptId owner member, else the Lscr's own type.
    let resolved_type = key_owner_script_type(root, lscr_res)
        .or_else(|| lctx_script_id_owner_type(root, script_id))
        .unwrap_or(script.script_type);

    let text = decomp::decompile_script(&script, names.as_ref(), version, true);

    // LibreShockwave exportScript header (CastExporter.cpp):
    // `-- Cast member: <name>` + `-- Type: <display name>` + blank line.
    let member_name = cast::read_member_name(info).unwrap_or_default();
    let header = format!(
        "-- Cast member: {member_name}\n-- Type: {}\n\n",
        resolved_type.display_name()
    );
    fs::create_dir_all(dir)?;
    fs::write(dir.join(format!("{base_name}.ls")), format!("{header}{text}"))?;

    // --lasm: also write the raw bytecode disassembly (opcode-level) so the
    // decompiled .ls can be verified against the real LSCR instructions.
    if lasm {
        let dasm = lingo::disassemble_script(&script, names.as_ref());
        fs::write(
            dir.join(format!("{base_name}.lasm")),
            lingo::format_script(&dasm),
        )?;
    }
    Ok(())
}

/// A script member's type code (specificData[0..2], u16 BE —
/// CastMemberChunk::getScriptType), when it is a D5+ script member.
fn script_member_type(cm: &cast::CastMember) -> Option<lscr::ScriptType> {
    if cm.member_type != cast::CastMemberType::Script {
        return None;
    }
    member_script_type(cm)
}

/// The KEY\* owner cast member of a Lscr resource → its script type code
/// (DirectorFile::getScriptType keyTable_ path).
fn key_owner_script_type(root: &Chunk, lscr_res: u32) -> Option<lscr::ScriptType> {
    for key_chunk in root.children_by(b"KEY*") {
        let Ok(table) = key::read_key(key_chunk) else { continue };
        for entry in &table.entries {
            if entry.child_index != lscr_res {
                continue;
            }
            let cast_chunks = root.children_by(b"CASt");
            let Some(cas) = cast_chunks.iter().find(|c| c.source_id == Some(entry.parent_index)) else {
                continue;
            };
            let Ok(cm) = cast::read_cast_member(cas) else { continue };
            if let Some(t) = script_member_type(&cm) {
                return Some(t);
            }
        }
    }
    None
}

/// The LctX scriptId owner member's type code (ScriptLookup::getScriptType:
/// a script CASt member whose info-block scriptId equals the LctX 1-based
/// index of the entry pointing at this Lscr).
fn lctx_script_id_owner_type(root: &Chunk, script_id: i32) -> Option<lscr::ScriptType> {
    for cas in root.children_by(b"CASt") {
        let Ok(cm) = cast::read_cast_member(cas) else { continue };
        if cm.member_type != cast::CastMemberType::Script {
            continue;
        }
        let info = if cm.cast_info_size > 0 {
            &cm.raw_data[12..(12 + cm.cast_info_size as usize).min(cm.raw_data.len())]
        } else {
            continue;
        };
        if cast::read_member_script_id(info) == Some(script_id) {
            if let Some(t) = member_script_type(&cm) {
                return Some(t);
            }
        }
    }
    None
}

/// The cast member's script type code (specificData[0..2], u16 BE), when the
/// member is a D5+ script member with enough specific data.
fn member_script_type(cm: &cast::CastMember) -> Option<lscr::ScriptType> {
    if cm.cast_info_size == 0 {
        return None;
    }
    let sd_start = 12 + cm.cast_info_size as usize;
    let sd_len = cm.cast_data_size as usize;
    let sd = if sd_start + sd_len <= cm.raw_data.len() {
        &cm.raw_data[sd_start..sd_start + sd_len]
    } else if sd_start <= cm.raw_data.len() {
        &cm.raw_data[sd_start..]
    } else {
        &[]
    };
    if sd.len() >= 2 {
        Some(lscr::ScriptType::from_code(u16::from_be_bytes([sd[0], sd[1]]) as i32))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Sound export → WAV (SND) or aiff/mp3/bin (ediM)
// ---------------------------------------------------------------------------

fn export_sound(
    root: &Chunk,
    _cm: &cast::CastMember,
    dir: &Path,
    base_name: &str,
    member_snd: &std::collections::HashMap<u32, (u32, [u8; 4])>,
    _member_id: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    // Prefer the SND/ediM chunk linked through KEY*.
    if let Some((res, _tag)) = member_snd.get(&_member_id) {
        if let Some(chunk) = root.children.iter().find(|c| c.source_id == Some(*res)) {
            if write_sound_chunk(chunk, dir, base_name).is_ok() {
                return Ok(());
            }
        }
    }
    // Fallback: scan all sound chunks (unlinked members).
    for tag in [b"SND ", b"snd ", b"sndH", b"sndS", b"ediM"].iter() {
        for chunk in root.children_by(tag) {
            if write_sound_chunk(chunk, dir, base_name).is_ok() {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn write_sound_chunk(chunk: &Chunk, dir: &Path, base_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let data = chunk.data();
    // Container sniff FIRST — it applies to every sound tag, not just ediM.
    // The Habbo corpus ships its sound members inside a Sulake custom
    // container (format fields + a "© 2007 Sulake Corp" block + an offset
    // table) followed by the REAL MP3 frames, and that container can arrive
    // under 'SND ' just as often as 'ediM'. The old SND path misread the
    // container as Mac 'snd ' data (its first u16s parsed as "extended format"
    // with a bogus 98385 Hz sample rate) and dumped the WHOLE container as
    // 8-bit PCM — which plays back as static. Detect the wrapper by scanning
    // for the first valid MPEG frame sync and strip it (plus any trailing
    // 0xFF padding) so the runtime gets a standard MP3, byte-identical to the
    // libreshockwave export.
    if data.starts_with(b"FORM") {
        fs::create_dir_all(dir)?;
        fs::write(dir.join(format!("{base_name}.aiff")), data)?;
        return Ok(());
    }
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WAVE" {
        fs::create_dir_all(dir)?;
        fs::write(dir.join(format!("{base_name}.wav")), data)?;
        return Ok(());
    }
    if data.starts_with(b"ID3") || (data.len() > 2 && data[0] == 0xFF && (data[1] & 0xE0) == 0xE0) {
        fs::create_dir_all(dir)?;
        fs::write(dir.join(format!("{base_name}.mp3")), data)?;
        return Ok(());
    }
    if let Some(sync) = find_mp3_sync(data) {
        // Sulake-wrapped MP3: the bytes from the first frame sync are the real
        // MP3. Trim the trailing 0xFF padding run like the reference export.
        let mut end = data.len();
        while end > sync + 4 && data[end - 1] == 0xFF {
            end -= 1;
        }
        fs::create_dir_all(dir)?;
        fs::write(dir.join(format!("{base_name}.mp3")), &data[sync..end])?;
        return Ok(());
    }
    if chunk.is(b"ediM") {
        // Embedded media that isn't a recognizable container: keep the raw
        // bytes under .bin so no member is lost (the bundler ranks it lowest).
        fs::create_dir_all(dir)?;
        fs::write(dir.join(format!("{base_name}.bin")), data)?;
        return Ok(());
    }
    // Real Mac 'snd ' resource (the sniffs above only fall through for actual
    // PCM sound data) — decode the header and write a proper WAV.
    match sound::read_snd(chunk) {
        Ok(sd) => {
            let sample_rate = if sd.info.sample_rate > 0 { sd.info.sample_rate } else { 22050 };
            let sample_size = if sd.info.sample_size > 0 { sd.info.sample_size } else { 8 };
            let channels = if sd.info.channels > 0 { sd.info.channels } else { 1 };
            let wav = make_wav(&sd.raw_data, sample_rate, sample_size, channels);
            fs::create_dir_all(dir)?;
            fs::write(dir.join(format!("{base_name}.wav")), &wav)?;
            Ok(())
        }
        Err(_) => Err("SND parse failed".into()),
    }
}

/// First valid MPEG audio frame sync (0xFF Ex with a well-formed header), or
/// None. Validates the version/layer/bitrate/sample-rate fields so 0xFF
/// padding runs inside container headers are skipped (0xFF FF FF parses as a
/// free-format bitrate index and is rejected).
fn find_mp3_sync(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(4) {
        if data[i] != 0xFF || (data[i + 1] & 0xE0) != 0xE0 {
            continue;
        }
        let version = (data[i + 1] >> 3) & 0x3;
        let layer_bits = (data[i + 1] >> 1) & 0x3;
        let bitrate_idx = (data[i + 2] >> 4) & 0xF;
        let sr_idx = (data[i + 2] >> 2) & 0x3;
        if version == 1 || layer_bits == 0 || sr_idx == 3 || bitrate_idx == 0 || bitrate_idx == 15 {
            continue;
        }
        return Some(i);
    }
    None
}

/// Build a WAV file from raw PCM data.
fn make_wav(pcm_data: &[u8], sample_rate: u32, sample_size: u8, channels: u8) -> Vec<u8> {
    let bytes_per_sample = (sample_size as u32) / 8;
    let block_align = bytes_per_sample * channels as u32;
    let byte_rate = sample_rate * block_align;
    let data_size = pcm_data.len() as u32;
    let file_size = 36 + data_size;

    let mut wav = Vec::with_capacity(file_size as usize);

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes());  // PCM format
    wav.extend_from_slice(&(channels as u16).to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&(block_align as u16).to_le_bytes());
    wav.extend_from_slice(&(sample_size as u16).to_le_bytes());

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(pcm_data);

    wav
}

// ---------------------------------------------------------------------------
// Text export → .txt
// ---------------------------------------------------------------------------

fn export_text(
    stxt_chunk: Option<&Chunk>,
    dir: &Path,
    base_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(chunk) = stxt_chunk else { return Ok(()) };
    if let Ok(s) = stxt::read_stxt(chunk) {
        // LibreShockwave normalizeText: \r -> \n, collapsing \r\n to one \n.
        let text = s.text.replace("\r\n", "\n").replace('\r', "\n");
        fs::create_dir_all(dir)?;
        fs::write(dir.join(format!("{base_name}.txt")), text)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Palette export → .pal
// ---------------------------------------------------------------------------

fn export_palette(
    dir: &Path,
    base_name: &str,
    colors: Option<&Vec<(u8, u8, u8)>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(colors) = colors {
        fs::create_dir_all(dir)?;
        fs::write(dir.join(format!("{base_name}.pal")), jasc_pal(colors))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Font export → .ttf (XMED/PFR1 payloads) + movie-wide font pass + fonts.txt
// ---------------------------------------------------------------------------

/// Find the XMED chunk referenced by a KEY* link and, if it carries a PFR1
/// payload, parse it and convert it to TTF bytes plus the font's internal name.
fn try_font_ttf(root: &Chunk, res: u32) -> Option<(Vec<u8>, String)> {
    let chunk = root.children.iter().find(|c| c.source_id == Some(res))?;
    let data = chunk.data();
    if data.len() < 4 || &data[..4] != b"PFR1" {
        return None;
    }
    let font = font::parse_fr1(data)?;
    Some((font::convert_ttf(&font, &font.font_name), font.font_name))
}

/// Export a Font cast member's linked XMED chunk as `.ttf`, falling back to
/// the raw `.bin` when no PFR1 payload is available (LibreShockwave's
/// exportFont, with the raw .bin preserved as our previous behavior).
fn export_font(
    root: &Chunk,
    cm: &cast::CastMember,
    dir: &Path,
    base_name: &str,
    member_xmed: &std::collections::HashMap<u32, u32>,
    member_id: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(res) = member_xmed.get(&member_id) {
        if let Some((ttf, _)) = try_font_ttf(root, *res) {
            fs::create_dir_all(dir)?;
            fs::write(dir.join(format!("{base_name}.ttf")), ttf)?;
            return Ok(());
        }
    }
    export_raw(cm, dir, base_name)
}

/// Movie-wide font pass (LibreShockwave exportMovieFonts): every member's
/// linked XMED chunk that carries a PFR1 payload is exported as
/// `fonts/NNNN_<fontName>.ttf`, numbered by export order and deduped by chunk
/// id. Also writes `fonts.txt` from Fmap chunks when present.
///
/// Verified against LibreShockwave's exported hh_interface: both .ttf files
/// and fonts.txt are byte-identical.
fn export_movie_fonts(root: &Chunk, base: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // KEY* XMED links: member id -> linked media chunk id.
    let mut member_xmed: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for key_chunk in root.children_by(b"KEY*") {
        if let Ok(kt) = key::read_key(key_chunk) {
            for e in &kt.entries {
                if &e.child_tag == b"XMED" {
                    member_xmed.insert(e.parent_index, e.child_index);
                }
            }
        }
    }

    let mut exported: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut font_ordinal = 0u32;
    for (i, cast_chunk) in root.children_by(b"CASt").iter().enumerate() {
        let member_id = cast_chunk.source_id.unwrap_or(i as u32);
        let Ok(cm) = cast::read_cast_member(cast_chunk) else { continue };
        if cm.member_type == cast::CastMemberType::Font {
            // Font members are handled by export_font in the member pass.
            continue;
        }
        let Some(res) = member_xmed.get(&member_id) else { continue };
        // Dedupe by the linked chunk id (KEY* child index == LS chunk id).
        if !exported.insert(*res) {
            continue;
        }
        let Some((ttf, font_name)) = try_font_ttf(root, *res) else { continue };
        let safe_name = sanitize_member_name(&font_name);
        let safe_name = if safe_name.is_empty() {
            format!("font_{res}")
        } else {
            safe_name
        };
        font_ordinal += 1;
        let name = format!("{font_ordinal:04}_{safe_name}.ttf");
        let dir = base.join("fonts");
        if let Err(e) = fs::create_dir_all(&dir).and_then(|_| fs::write(dir.join(&name), ttf)) {
            // Keep exporting the remaining fonts (LibreShockwave catches per-font).
            eprintln!("  FAIL fonts/{name}: {e}");
        }
    }

    // fonts.txt manifest from Fmap chunks.
    let mut manifest = String::new();
    for fmap in root.children_by(b"Fmap") {
        for entry in font::parse_fmap(fmap.data()) {
            manifest.push_str(&format!("{}\t{}\t{}\n", entry.font_id, entry.platform, entry.font_name));
        }
    }
    if !manifest.is_empty() {
        fs::write(base.join("fonts.txt"), manifest)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shape export (shapes) → .txt
// ---------------------------------------------------------------------------

/// Shape members export as `shapes/NNNN_shape_<name>.txt` (key: value lines)
/// — the format the runtime's parseShapeText consumes to draw the entry/room
/// scenes' solid-fill rects/ovals. The data block sits after the CASt header
/// (u32 type, u32 infoLen, u32 dataLen) and the info section.
fn export_shape(cm: &cast::CastMember, dir: &Path, base_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let is_d5 = cm.cast_info_size > 0 && cm.cast_data_size > 0 && cm.raw_data.len() >= 12;
    let block: &[u8] = if is_d5 {
        let start = 12 + cm.cast_info_size as usize;
        let len = cm.cast_data_size as usize;
        if start + len <= cm.raw_data.len() {
            &cm.raw_data[start..start + len]
        } else if start <= cm.raw_data.len() {
            &cm.raw_data[start..]
        } else {
            &[]
        }
    } else {
        &cm.raw_data
    };
    let Some(info) = cast::ShapeInfo::parse(block) else {
        // Not parseable as a shape — fall back to raw .bin so nothing is lost.
        fs::create_dir_all(dir)?;
        fs::write(dir.join(format!("{base_name}.bin")), &cm.raw_data)?;
        return Ok(());
    };
    fs::create_dir_all(dir)?;
    fs::write(dir.join(format!("{base_name}.txt")), info.to_text())?;
    Ok(())
}

/// Raw member data (fonts fall back here only when no linked PFR1 XMED payload
/// exists). The .bin is the member's full CASt data (D5 header + info block).
fn export_raw(cm: &cast::CastMember, dir: &Path, base_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join(format!("{base_name}.bin")), &cm.raw_data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use director_rifx::chunk::read_chunk;

    /// The real Sulake container header from hh_purse's naw_snd_cash member:
    /// 82 bytes of format fields before the first MPEG frame sync (0xFF FB 50).
    /// read_snd misreads these first u16s as an "extended" Mac 'snd ' header,
    /// reporting a bogus 98385 Hz / 8-bit format — dumping the whole container
    /// as PCM is what produced the static sound in the old export.
    const SULAKE_HEADER: [u8; 82] = [
        0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x80, 0x51, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xac, 0x44,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x3c,
        0x00, 0x00, 0xac, 0x11, 0x40, 0x0e, 0xac, 0x44, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x71,
    ];

    /// A real MPEG1 Layer III frame header (44.1 kHz, 128 kbps) plus payload —
    /// the frame-sync validator in find_mp3_sync must accept it.
    const MP3_SYNC: [u8; 4] = [0xFF, 0xFB, 0x50, 0x00];

    fn write_chunk(tag: &[u8; 4], data: &[u8]) -> Chunk {
        let mut raw = Vec::new();
        raw.extend_from_slice(tag);
        raw.extend_from_slice(&(data.len() as u32).to_be_bytes());
        raw.extend_from_slice(data);
        let mut pos = 0u64;
        let (chunk, _) = read_chunk(&raw, &mut pos).unwrap();
        chunk
    }

    #[test]
    fn sulake_wrapped_mp3_under_snd_tag_exports_clean_mp3() {
        // Container: format-fields header + real MP3 frames + trailing 0xFF
        // padding run (the Sulake wrapper layout found in the Habbo corpus).
        let mut container = Vec::new();
        container.extend_from_slice(&SULAKE_HEADER);
        container.extend_from_slice(&MP3_SYNC);
        container.extend_from_slice(b"\x00\x12\x34\x56\x78\x9a\xbc\xde\xf0");
        container.extend_from_slice(&[0xFF; 8]); // trailing padding

        let dir = std::env::temp_dir().join(format!(
            "sparkd-sound-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        write_sound_chunk(&write_chunk(b"SND ", &container), &dir, "0021_sound_naw_snd_cash")
            .expect("SND-tagged Sulake container must export");

        // The header + padding must be stripped: the file is a standard MP3.
        let out = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect::<Vec<_>>();
        assert_eq!(out, vec!["0021_sound_naw_snd_cash.mp3".to_string()]);
        let bytes = fs::read(dir.join("0021_sound_naw_snd_cash.mp3")).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&MP3_SYNC);
        expected.extend_from_slice(b"\x00\x12\x34\x56\x78\x9a\xbc\xde\xf0");
        assert_eq!(bytes, expected, "must be the MP3 frames with no header/padding");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_wav_container_passes_through_untouched() {
        // A genuine RIFF/WAVE chunk must be written as-is (not mangled).
        let wav = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00";
        let dir = std::env::temp_dir().join(format!(
            "sparkd-sound-test-wav-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        write_sound_chunk(&write_chunk(b"ediM", wav), &dir, "0012_sound_tone")
            .expect("plain WAV must export");
        let bytes = fs::read(dir.join("0012_sound_tone.wav")).unwrap();
        assert_eq!(bytes, wav);
        let _ = fs::remove_dir_all(&dir);
    }
}
