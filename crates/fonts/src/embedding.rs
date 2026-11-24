//! Unicode embedding of an explicitly supplied, embeddable TrueType face. Font files are
//! document inputs or the optional craft-fonts build input, never system-interface fallbacks.

use std::collections::HashMap;
use std::fmt::Write as _;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};
use skrifa::{
    FontRef, MetadataProvider,
    instance::{LocationRef, Size},
    raw::TableProvider,
};

/// Compare PDF subset/PostScript names with the full face's family name.
pub fn face_key(name: &str) -> String {
    name.rsplit('+').next().unwrap_or(name).chars().filter(char::is_ascii_alphanumeric).flat_map(char::to_lowercase).collect()
}

/// Full, approved faces of the source family. This never silently selects another family.
pub fn matching_faces(name: &str) -> Vec<&'static crate::CraftFont> {
    let key = face_key(name);
    crate::CRAFT_FONTS.iter().filter(|f| face_key(f.family) == key || face_key(&f.name()) == key).collect()
}

/// Whether a complete font program can show every non-newline character.
pub fn covers(bytes: &[u8], text: &str) -> bool {
    FontRef::new(bytes).is_ok_and(|f| text.chars().all(|c| matches!(c, '\n' | '\r') || f.charmap().map(c).is_some_and(|g| g.to_u32() != 0)))
}

/// Names come from the program, never a caller's guessed family.
pub fn names(bytes: &[u8]) -> Result<(String, String), String> {
    use skrifa::string::StringId;
    validate(bytes)?;
    let f = FontRef::new(bytes).map_err(|e| e.to_string())?;
    let name = |id| {
        f.localized_strings(id)
            .find(|s| s.language() == Some("en"))
            .or_else(|| f.localized_strings(id).next())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty() && s.len() <= 128)
    };
    let family = name(StringId::FAMILY_NAME).ok_or("the font has no family name")?;
    let postscript = name(StringId::POSTSCRIPT_NAME).unwrap_or_else(|| family.replace(' ', ""));
    Ok((family, postscript))
}

/// Whether this build takes any font a person picks as a document font, whatever its name table
/// says about its licence. Only a local build made with `PDFCRAFT_ANY_DOCUMENT_FONT` set does:
/// someone editing their own files with fonts they hold a licence for, who answers for that
/// licence themselves. Release builds never set it, so the asset policy (AGENTS.md §1.1 and §1.2)
/// stays the default, and what [`validate`] checks, the font's own embedding permissions among
/// them, applies either way.
const ANY_DOCUMENT_FONT: bool = option_env!("PDFCRAFT_ANY_DOCUMENT_FONT").is_some();

/// Whether [`validate_document_input`] skips the licence policy in this build
/// (`PDFCRAFT_ANY_DOCUMENT_FONT` was set when it was compiled).
pub fn accepts_any_document_font() -> bool {
    ANY_DOCUMENT_FONT
}

/// A selected external font is document input. Require its own open-licence declaration;
/// never turn a proprietary system-interface fallback into an exported PDF font. A build that
/// [`accepts_any_document_font`] checks only that the font can be embedded.
pub fn validate_document_input(bytes: &[u8]) -> Result<(), String> {
    use skrifa::string::StringId;
    let (family, _) = names(bytes)?;
    if ANY_DOCUMENT_FONT {
        return Ok(());
    }
    let key = face_key(&family);
    if ["sourcehan", "sourceserif", "sourcesans", "notosanscjk", "notoserifcjk"].iter().any(|n| key.starts_with(n)) {
        return Err("this font family is excluded by the project's asset policy".into());
    }
    let f = FontRef::new(bytes).map_err(|e| e.to_string())?;
    // An Adobe scan that could not finish fails closed, like a match.
    if any_name_record(&f, &[StringId::COPYRIGHT_NOTICE, StringId::MANUFACTURER], |t| t.to_ascii_lowercase().contains("adobe")) != Some(false) {
        return Err("Adobe-authored font assets are excluded by the project's asset policy".into());
    }
    let licences = ["SIL OPEN FONT", "SIL Open Font", "Apache License", "Ubuntu Font Licence", "Bitstream Vera", "MIT License"];
    if any_name_record(&f, &[StringId::LICENSE_DESCRIPTION], |t| licences.iter().any(|l| t.contains(l))) != Some(true) {
        return Err("select an openly licensed font whose name table declares its licence (OFL, Apache, Ubuntu, Bitstream Vera or MIT)".into());
    }
    Ok(())
}

/// Name-table text one policy check reads in all.
const MAX_NAME_BYTES: usize = 1 << 20;

/// Whether `found` holds for any record of the name-table strings `ids`, read one at a time
/// within [`MAX_NAME_BYTES`] in all: `Some(true)` for a match, `Some(false)` when every record was
/// read and none matched, `None` when the allowance ran out first (a record that does not fit what
/// is left is not searched). The table can point many records at one large string, so the records
/// are never joined.
fn any_name_record(font: &FontRef, ids: &[skrifa::string::StringId], found: impl Fn(&str) -> bool) -> Option<bool> {
    let mut left = MAX_NAME_BYTES;
    for s in ids.iter().flat_map(|id| font.localized_strings(*id)) {
        let text = s.to_string();
        if text.len() > left {
            return None;
        }
        left -= text.len().max(1).min(left);
        if found(&text) {
            return Some(true);
        }
    }
    Some(false)
}

/// Most bytes of one face embedded as a document font: a full CJK face is 10 to 25 MiB.
pub const MAX_FONT_BYTES: usize = 32 << 20;

/// Most bytes of a font file read to find a document font in: a collection (`.ttc`) holds several
/// faces that share their glyph data.
pub const MAX_FONT_FILE_BYTES: usize = 64 << 20;

/// Tables that vary a font along its axes. A PDF draws one fixed shape per glyph, so
/// [`standalone_face`] leaves them out and the face keeps its default instance.
const VARIATION_TABLES: [[u8; 4]; 8] = [*b"fvar", *b"gvar", *b"avar", *b"cvar", *b"HVAR", *b"VVAR", *b"MVAR", *b"STAT"];

/// How many faces a font file holds: 1 for a standalone font, more for a collection (`.ttc`,
/// `.otc`). An error for bytes that are neither.
pub fn face_count(file: &[u8]) -> Result<u32, String> {
    match skrifa::raw::FileRef::new(file) {
        Ok(skrifa::raw::FileRef::Font(_)) => Ok(1),
        Ok(skrifa::raw::FileRef::Collection(collection)) => Ok(collection.len()),
        Err(_) => Err("select a TrueType/OpenType font file".into()),
    }
}

/// Face `index` of a font file as the standalone, static program a PDF embeds: a face of a
/// collection with its tables gathered into a file of its own, and a variable font without its
/// variation tables (its default instance). A standalone static font comes back as it is, byte
/// for byte. An error for a file that is not a font, an index it does not have, tables that lie
/// outside the file, or a face over [`MAX_FONT_BYTES`]; [`validate`] still has to pass the result.
pub fn standalone_face(file: &[u8], index: u32) -> Result<std::borrow::Cow<'_, [u8]>, String> {
    if file.len() > MAX_FONT_FILE_BYTES {
        return Err("the selected font file exceeds 64 MiB".into());
    }
    let count = face_count(file)?;
    let font = FontRef::from_index(file, index).map_err(|_| format!("the font file has {count} faces; face {index} is not one of them"))?;
    let in_a_collection = matches!(skrifa::raw::FileRef::new(file), Ok(skrifa::raw::FileRef::Collection(_)));
    if !in_a_collection && font.fvar().is_err() {
        return Ok(std::borrow::Cow::Borrowed(file));
    }
    let mut tables: Vec<([u8; 4], &[u8])> = Vec::new();
    for record in font.table_directory.table_records() {
        let tag = record.tag().to_be_bytes();
        if VARIATION_TABLES.contains(&tag) {
            continue;
        }
        let start = usize::try_from(record.offset()).map_err(|_| "the font file is damaged")?;
        let length = usize::try_from(record.length()).map_err(|_| "the font file is damaged")?;
        let data = start.checked_add(length).and_then(|end| file.get(start..end)).ok_or("the font file is damaged: a table lies outside it")?;
        tables.push((tag, data));
    }
    tables.sort_by_key(|(tag, _)| *tag);
    write_sfnt(font.table_directory.sfnt_version(), &tables).map(std::borrow::Cow::Owned)
}

/// A font collection (`.ttc`) holding the standalone fonts `faces`, each kept whole: the inverse
/// of [`standalone_face`]. It gives tests of collection handling a fixture built from fonts the
/// repository already has, so no collection file is committed (AGENTS.md §1.4). An error for
/// bytes that are not a standalone font, or past [`MAX_FONT_FILE_BYTES`].
pub fn collection_of(faces: &[&[u8]]) -> Result<Vec<u8>, String> {
    const TTC_HEADER_BYTES: usize = 12;
    let too_large = || "the collection exceeds 64 MiB".to_string();
    let count = u32::try_from(faces.len()).map_err(|_| too_large())?;
    let header = faces.len().checked_mul(4).and_then(|n| n.checked_add(TTC_HEADER_BYTES)).and_then(padded).ok_or_else(too_large)?;
    let mut out = b"ttcf".to_vec();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    let mut base = header;
    for face in faces {
        out.extend_from_slice(&u32::try_from(base).map_err(|_| too_large())?.to_be_bytes());
        base = padded(face.len()).and_then(|n| base.checked_add(n)).filter(|end| *end <= MAX_FONT_FILE_BYTES).ok_or_else(too_large)?;
    }
    out.resize(header, 0);
    for face in faces {
        append_collection_face(&mut out, face)?;
    }
    Ok(out)
}

/// Appends the standalone font `face` to a collection being written. Its table offsets counted
/// from its own start; in a collection they count from the start of the file.
fn append_collection_face(out: &mut Vec<u8>, face: &[u8]) -> Result<(), String> {
    let damaged = || "a face of the collection is not a standalone font".to_string();
    let font = match skrifa::raw::FileRef::new(face) {
        Ok(skrifa::raw::FileRef::Font(font)) => font,
        _ => return Err(damaged()),
    };
    let base = out.len();
    let moved_by = u32::try_from(base).map_err(|_| damaged())?;
    out.extend_from_slice(face);
    out.resize(padded(out.len()).ok_or_else(damaged)?, 0);
    for (index, record) in font.table_directory.table_records().iter().enumerate() {
        let offset = record.offset().checked_add(moved_by).ok_or_else(damaged)?;
        let field = index.checked_mul(SFNT_RECORD_BYTES).and_then(|n| n.checked_add(base + SFNT_HEADER_BYTES + 8)).ok_or_else(damaged)?;
        out.get_mut(field..field.saturating_add(4)).ok_or_else(damaged)?.copy_from_slice(&offset.to_be_bytes());
    }
    Ok(())
}

/// The length of a table padded to the four-byte boundary the next one starts at.
fn padded(length: usize) -> Option<usize> {
    length.checked_add(3).map(|n| n & !3)
}

/// The OpenType checksum of `data`: the wrapping sum of its big-endian 32-bit words, the last one
/// padded with zeros.
fn table_checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0u8; 4];
        for (to, from) in word.iter_mut().zip(chunk) {
            *to = *from;
        }
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

/// Where `head` keeps the checksum adjustment of the whole file, and the constant it adjusts to.
const HEAD_CHECKSUM_ADJUSTMENT: std::ops::Range<usize> = 8..12;
const SFNT_CHECKSUM_MAGIC: u32 = 0xB1B0_AFBA;

/// One table of a font file being written: its tag and its bytes.
type SfntTable<'a> = ([u8; 4], &'a [u8]);

const SFNT_HEADER_BYTES: usize = 12;
const SFNT_RECORD_BYTES: usize = 16;

fn font_too_large() -> String {
    "the selected font exceeds 32 MiB".into()
}

/// A font file holding `tables` (sorted by tag): the header and directory OpenType asks for, each
/// table on a four-byte boundary with its checksum, and the file's checksum adjustment in `head`.
/// An error past [`MAX_FONT_BYTES`] or 65 535 tables.
fn write_sfnt(version: u32, tables: &[SfntTable]) -> Result<Vec<u8>, String> {
    let count = u16::try_from(tables.len()).map_err(|_| "the font has too many tables")?;
    let directory = SFNT_RECORD_BYTES.checked_mul(tables.len()).and_then(|n| n.checked_add(SFNT_HEADER_BYTES)).ok_or_else(font_too_large)?;
    let total = tables.iter().try_fold(directory, |sum, (_, data)| padded(data.len()).and_then(|n| sum.checked_add(n)));
    let total = total.filter(|bytes| *bytes <= MAX_FONT_BYTES).ok_or_else(font_too_large)?;
    let mut out = Vec::with_capacity(total);
    write_sfnt_header(&mut out, version, count);
    let head_at = write_table_records(&mut out, tables, directory)?;
    for (_, data) in tables {
        out.extend_from_slice(data);
        out.resize(padded(out.len()).ok_or_else(font_too_large)?, 0);
    }
    set_checksum_adjustment(&mut out, head_at);
    Ok(out)
}

/// The 12-byte header: the version, the table count, and the three numbers that describe a binary
/// search of the directory (searchRange, entrySelector, rangeShift).
fn write_sfnt_header(out: &mut Vec<u8>, version: u32, count: u16) {
    let entry_selector = u16::try_from(count.checked_ilog2().unwrap_or(0)).unwrap_or(0);
    let search_range = 16u16.checked_shl(u32::from(entry_selector)).unwrap_or(0);
    let range_shift = count.saturating_mul(16).saturating_sub(search_range);
    out.extend_from_slice(&version.to_be_bytes());
    for field in [count, search_range, entry_selector, range_shift] {
        out.extend_from_slice(&field.to_be_bytes());
    }
}

/// The directory: each table's tag, checksum, offset and length, the data starting at `offset`.
/// Returns where `head` will lie, for [`set_checksum_adjustment`].
fn write_table_records(out: &mut Vec<u8>, tables: &[SfntTable], mut offset: usize) -> Result<Option<usize>, String> {
    let mut head_at = None;
    for (tag, data) in tables {
        if tag == b"head" {
            head_at = Some(offset);
        }
        out.extend_from_slice(tag);
        out.extend_from_slice(&record_checksum(tag, data).to_be_bytes());
        out.extend_from_slice(&u32::try_from(offset).map_err(|_| font_too_large())?.to_be_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).map_err(|_| font_too_large())?.to_be_bytes());
        offset = padded(data.len()).and_then(|n| offset.checked_add(n)).ok_or_else(font_too_large)?;
    }
    Ok(head_at)
}

/// A table's checksum for the directory. `head` is summed with its adjustment field zero, which
/// is how the file's own sum needs it.
fn record_checksum(tag: &[u8; 4], data: &[u8]) -> u32 {
    if tag != b"head" {
        return table_checksum(data);
    }
    let mut head = data.to_vec();
    if let Some(field) = head.get_mut(HEAD_CHECKSUM_ADJUSTMENT) {
        field.fill(0);
    }
    table_checksum(&head)
}

/// Sets `head`'s checksum adjustment so the whole file sums to the OpenType constant. A file
/// without a `head` table (or with a short one) is left as it is; [`validate`] refuses it.
fn set_checksum_adjustment(out: &mut [u8], head_at: Option<usize>) {
    let Some(at) = head_at else { return };
    let field = at.checked_add(HEAD_CHECKSUM_ADJUSTMENT.start).zip(at.checked_add(HEAD_CHECKSUM_ADJUSTMENT.end)).map(|(start, end)| start..end);
    let Some(field) = field else { return };
    let Some(bytes) = out.get_mut(field.clone()) else { return };
    bytes.fill(0);
    let adjustment = SFNT_CHECKSUM_MAGIC.wrapping_sub(table_checksum(out));
    if let Some(bytes) = out.get_mut(field) {
        bytes.copy_from_slice(&adjustment.to_be_bytes());
    }
}

/// Validate before allocating any PDF objects, so an invalid font cannot partly mutate a doc.
pub fn validate(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_FONT_BYTES {
        return Err("the selected font exceeds 32 MiB".into());
    }
    let font = FontRef::new(bytes).map_err(|_| "select a standalone TrueType/OpenType font (not a font collection)")?;
    if font.fvar().is_ok() {
        return Err("select a static instance of this variable font before embedding".into());
    }
    if (font.glyf().is_err() && font.cff().is_err()) || font.head().is_err() || font.hhea().is_err() || font.hmtx().is_err() {
        return Err("the selected font has no usable TrueType/CFF outlines and metrics".into());
    }
    if font.os2().is_ok_and(|t| t.fs_type() & (0x0002 | 0x0004 | 0x0200) != 0) {
        return Err("this font's embedding permissions prohibit editable outline embedding".into());
    }
    let m = font.metrics(Size::unscaled(), LocationRef::default());
    if m.units_per_em == 0 || font.charmap().mappings().next().is_none() {
        return Err("the selected font has no Unicode mapping or valid units per em".into());
    }
    if font.charmap().mappings().take(65_535).count() >= 65_535 {
        return Err("the selected font has too many Unicode mappings for one PDF font".into());
    }
    if font.charmap().mappings().any(|(_, g)| g.to_u32() >= u32::from(m.glyph_count)) {
        return Err("the selected font maps Unicode to a glyph outside its program".into());
    }
    Ok(())
}

fn stream(doc: &mut Document, bytes: Vec<u8>) -> Object {
    Object::Ref(doc.add(Object::Stream(Stream::from_raw(Dict::new(), bytes))))
}

/// Embed the complete font once and provide separate CIDs for Unicode aliases. /ToUnicode,
/// /CIDToGIDMap and widths agree, including supplementary characters, so reopening needs no
/// installed font. Keeping the full program also lets subsequent edits add other glyphs.
pub fn embed(doc: &mut Document, fonts: &mut Dict, bytes: &[u8], family: &str) -> Result<String, String> {
    validate(bytes)?;
    let font = FontRef::new(bytes).map_err(|e| e.to_string())?;
    let mappings: Vec<_> = font
        .charmap()
        .mappings()
        .filter(|(_, gid)| gid.to_u32() > 0)
        .filter_map(|(cp, gid)| char::from_u32(cp).map(|c| (c.to_string(), gid)))
        .collect();
    embed_mapping(doc, fonts, bytes, family, &mappings)
}

/// Explicit glyph mapping for shaped text. A CID can represent a contextual glyph or a
/// ligature, while its ToUnicode retains the logical cluster. The full source program stays exact.
pub fn embed_mapping(
    doc: &mut Document,
    fonts: &mut Dict,
    bytes: &[u8],
    family: &str,
    mappings: &[(String, skrifa::GlyphId)],
) -> Result<String, String> {
    validate(bytes)?;
    if mappings.len() > 65_534 || mappings.iter().any(|(s, _)| s.len() > 4096) {
        return Err("shaped font mapping exceeds its budget".into());
    }
    let font = FontRef::new(bytes).map_err(|e| e.to_string())?;
    let cff = font.glyf().is_err();
    let loc = LocationRef::default();
    let m = font.metrics(Size::unscaled(), loc);
    let scale = 1000.0 / f64::from(m.units_per_em);
    let advances = font.glyph_metrics(Size::unscaled(), loc);
    let mut cid_encoding = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CMapName /PCGlyphs def\n/CMapType 1 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let mut cff_widths = std::collections::BTreeMap::new();
    let mut gids = vec![0, 0];
    let mut widths = Vec::with_capacity(mappings.len());
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CMapName /PCUnicode def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for (chunk_index, chunk) in mappings.chunks(100).enumerate() {
        let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
        let _ = writeln!(cid_encoding, "{} begincidchar", chunk.len());
        for (offset, (unicode, gid)) in chunk.iter().enumerate() {
            let cid = u16::try_from(chunk_index.saturating_mul(100).saturating_add(offset).saturating_add(1)).map_err(|_| "font mapping limit")?;
            let glyph = u16::try_from(gid.to_u32()).map_err(|_| "font glyph limit")?;
            if glyph == 0 || u32::from(glyph) >= u32::from(m.glyph_count) {
                return Err("font contains an unavailable shaped glyph".into());
            }
            let width = advances.advance_width(*gid).unwrap_or(0.0) as f64 * scale;
            if !width.is_finite() || !(0.0..=16_000.0).contains(&width) {
                return Err("font contains an invalid advance".into());
            }
            gids.extend_from_slice(&glyph.to_be_bytes());
            widths.push(Object::Real(width));
            cff_widths.insert(glyph, width);
            let _ = writeln!(cid_encoding, "<{cid:04X}> {glyph}");
            let hex: String = unicode.encode_utf16().map(|u| format!("{u:04X}")).collect();
            let _ = writeln!(cmap, "<{cid:04X}> <{hex}>");
        }
        cmap.push_str("endbfchar\n");
        cid_encoding.push_str("endcidchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cid_encoding.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    let name = family.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect::<String>();
    if name.is_empty() || name.len() > 128 {
        return Err("the font family name is invalid".into());
    }
    let glyph_map = if cff { cid_encoding.as_bytes() } else { gids.as_slice() };
    let mut programs = ProgramMatch::new(bytes);
    if let Some(resource) = same_resource(doc, fonts, &name, &mut programs, glyph_map, cmap.as_bytes()) {
        return Ok(resource);
    }
    let mut descriptor = Dict::new();
    descriptor.set(b"Type".to_vec(), Object::name("FontDescriptor"));
    descriptor.set(b"FontName".to_vec(), Object::name(&name));
    descriptor.set(b"FontFamily".to_vec(), Object::String(PdfString::literal(family.as_bytes().to_vec())));
    descriptor.set(b"Flags".to_vec(), Object::Int(4));
    descriptor.set(b"ItalicAngle".to_vec(), Object::Real(f64::from(m.italic_angle)));
    descriptor.set(b"Ascent".to_vec(), Object::Real(f64::from(m.ascent) * scale));
    descriptor.set(b"Descent".to_vec(), Object::Real(f64::from(m.descent) * scale));
    descriptor.set(b"CapHeight".to_vec(), Object::Real(f64::from(m.cap_height.unwrap_or(m.ascent)) * scale));
    descriptor.set(b"StemV".to_vec(), Object::Int(80));
    let bounds = m.bounds.ok_or("font has no outline bounds")?;
    descriptor.set(
        b"FontBBox".to_vec(),
        Object::Array([bounds.x_min, bounds.y_min, bounds.x_max, bounds.y_max].map(|v| Object::Real(f64::from(v) * scale)).to_vec()),
    );
    // Reuse the program another resource of this page already has, else one registered on another
    // page; source subset programs stay untouched.
    let existing = existing_program(doc, fonts, &mut programs);
    let program = existing.unwrap_or_else(|| {
        let mut d = Dict::new();
        d.set(b"Length1".to_vec(), Object::Int(bytes.len() as i64));
        if cff {
            d.set(b"Subtype".to_vec(), Object::name("OpenType"));
        }
        d.set(b"PCFullFont".to_vec(), Object::Bool(true));
        Object::Ref(doc.add(Object::Stream(Stream::flate(d, bytes))))
    });
    descriptor.set(if cff { b"FontFile3".to_vec() } else { b"FontFile2".to_vec() }, program);
    let desc = Object::Ref(doc.add(Object::Dict(descriptor)));
    let mut system = Dict::new();
    system.set(b"Registry".to_vec(), Object::String(PdfString::literal(b"Adobe".to_vec())));
    system.set(b"Ordering".to_vec(), Object::String(PdfString::literal(b"Identity".to_vec())));
    system.set(b"Supplement".to_vec(), Object::Int(0));
    let mut cid = Dict::new();
    cid.set(b"Type".to_vec(), Object::name("Font"));
    cid.set(b"Subtype".to_vec(), Object::name(if cff { "CIDFontType0" } else { "CIDFontType2" }));
    cid.set(b"BaseFont".to_vec(), Object::name(&name));
    cid.set(b"CIDSystemInfo".to_vec(), Object::Dict(system));
    cid.set(b"FontDescriptor".to_vec(), desc);
    if cff {
        cid.set(
            b"W".to_vec(),
            Object::Array(cff_widths.into_iter().flat_map(|(gid, w)| [Object::Int(i64::from(gid)), Object::Array(vec![Object::Real(w)])]).collect()),
        );
    } else {
        cid.set(b"CIDToGIDMap".to_vec(), stream(doc, gids));
        cid.set(b"W".to_vec(), Object::Array(vec![Object::Int(1), Object::Array(widths)]));
    }
    let descendant = Object::Ref(doc.add(Object::Dict(cid)));
    let mut top = Dict::new();
    top.set(b"Type".to_vec(), Object::name("Font"));
    top.set(b"Subtype".to_vec(), Object::name("Type0"));
    top.set(b"BaseFont".to_vec(), Object::name(&name));
    top.set(b"Encoding".to_vec(), if cff { stream(doc, cid_encoding.into_bytes()) } else { Object::name("Identity-H") });
    top.set(b"DescendantFonts".to_vec(), Object::Array(vec![descendant]));
    top.set(b"ToUnicode".to_vec(), stream(doc, cmap.into_bytes()));
    let mut index = 0usize;
    while fonts.contains(format!("PCFull{index}").as_bytes()) {
        index = index.saturating_add(1);
    }
    let resource = format!("PCFull{index}");
    fonts.set(resource.as_bytes().to_vec(), Object::Ref(doc.add(Object::Dict(top))));
    Ok(resource)
}

/// Whether the stream `object` refers to decodes to `expected`.
fn stream_is(doc: &Document, object: Option<&Object>, expected: &[u8]) -> bool {
    object.is_some_and(|o| match doc.resolve(o).as_ref() {
        Object::Stream(s) => s.decoded_within(expected.len()).is_ok_and(|d| d == expected),
        _ => false,
    })
}

/// Full programs one embedding decodes at most while looking for its own. A real match passes
/// the cheap checks and decodes once; many candidates that pass them come from a hostile file.
const MAX_PROGRAM_DECODES: usize = 16;

/// Recognises the full program written for `bytes`: each candidate stream decodes at most once
/// and within `bytes.len()`, and at most [`MAX_PROGRAM_DECODES`] decode in all.
struct ProgramMatch<'a> {
    bytes: &'a [u8],
    known: HashMap<ObjRef, bool>,
    decodes: usize,
}

impl<'a> ProgramMatch<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, known: HashMap::new(), decodes: 0 }
    }

    /// Whether `r`, holding `object`, is the program.
    fn is(&mut self, doc: &Document, r: ObjRef, object: &Object) -> bool {
        if let Some(&known) = self.known.get(&r) {
            return known;
        }
        let found = match object {
            Object::Stream(s)
                if s.dict.get(b"PCFullFont").is_some()
                    && s.dict.get(b"Length1").and_then(|l| doc.resolve(l).as_int()) == i64::try_from(self.bytes.len()).ok()
                    && self.decodes < MAX_PROGRAM_DECODES =>
            {
                self.decodes = self.decodes.saturating_add(1);
                s.decoded_within(self.bytes.len()).is_ok_and(|data| data == self.bytes)
            }
            _ => false,
        };
        self.known.insert(r, found);
        found
    }

    /// Whether the resource's program is this one.
    fn written(&mut self, doc: &Document, w: &Written) -> bool {
        w.program.as_ref().is_some_and(|r| self.is(doc, r, &doc.get(r)))
    }
}

/// The program for `programs.bytes` that this page's `PCFull` resources, or else the first
/// objects of the document, already have.
fn existing_program(doc: &Document, fonts: &Dict, programs: &mut ProgramMatch) -> Option<Object> {
    written(doc, fonts)
        .find(|w| programs.written(doc, w))
        .map(|w| w.program)
        .or_else(|| doc.scan_objects().take(8192).find_map(|(r, o)| programs.is(doc, r, &o).then_some(Object::Ref(r))))
}

/// A `PCFull` resource that [`embed_mapping`] wrote.
struct Written {
    resource: String,
    top: Dict,
    cid: Dict,
    /// The reference to its full program.
    program: Object,
}

/// The `PCFull` resources in `fonts`, in order.
fn written<'a>(doc: &'a Document, fonts: &'a Dict) -> impl Iterator<Item = Written> + 'a {
    fonts.iter().filter(|(key, _)| key.starts_with(b"PCFull")).take(256).filter_map(move |(key, value)| {
        let top = doc.dict(value)?;
        let cid = doc.dict(doc.resolve(top.get(b"DescendantFonts")?).as_array()?.first()?)?;
        let descriptor = doc.dict(cid.get(b"FontDescriptor")?)?;
        let program = descriptor.get(b"FontFile2").or_else(|| descriptor.get(b"FontFile3")).filter(|p| matches!(p, Object::Ref(_)))?.clone();
        Some(Written { resource: String::from_utf8_lossy(key).into_owned(), top, cid, program })
    })
}

/// A resource in `fonts` that already is this embedding: the same font name, program, glyph map
/// and Unicode map. Editing the same text again reuses it instead of adding another copy.
fn same_resource(doc: &Document, fonts: &Dict, name: &str, programs: &mut ProgramMatch, glyph_map: &[u8], unicode: &[u8]) -> Option<String> {
    written(doc, fonts)
        .find(|w| {
            let glyphs = w.cid.get(b"CIDToGIDMap").or_else(|| w.top.get(b"Encoding"));
            w.top.name(b"BaseFont") == Some(name.as_bytes())
                && stream_is(doc, w.top.get(b"ToUnicode"), unicode)
                && stream_is(doc, glyphs, glyph_map)
                && programs.written(doc, w)
        })
        .map(|w| w.resource)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::Metrics;

    #[test]
    fn full_unicode_embedding_keeps_outlines_advances_and_roundtrip_codes() {
        let bytes = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let mut doc = Document::new_empty();
        let mut fonts = Dict::new();
        let name = embed(&mut doc, &mut fonts, bytes, "Inter").unwrap();
        let font = doc.dict(fonts.get(name.as_bytes()).unwrap()).unwrap();
        let m = Metrics::from_dict(&doc, &font);
        let text = "Aé ΩЖ €";
        let codes = m.encode(text).unwrap();
        assert_eq!(m.decode(&codes), text);
        assert_eq!(crate::pdf::embedded_sfnt(&doc, &font).unwrap(), bytes);
        let sfnt = FontRef::new(bytes).unwrap();
        for (ch, (code, _)) in text.chars().zip(m.codes(&codes)) {
            let gid = sfnt.charmap().map(ch).unwrap();
            let w = sfnt.glyph_metrics(Size::unscaled(), LocationRef::default()).advance_width(gid).unwrap() as f64
                / f64::from(sfnt.metrics(Size::unscaled(), LocationRef::default()).units_per_em);
            assert!((m.width(code) - w).abs() < 1e-8);
        }
        let before = doc.object_numbers().len();
        assert!(embed(&mut doc, &mut fonts, b"broken font", "Broken").is_err());
        assert!(embed_mapping(&mut doc, &mut fonts, bytes, "Inter", &[("X".into(), skrifa::GlyphId::new(65_535))]).is_err());
        assert_eq!(doc.object_numbers().len(), before);
        let programs = || doc.scan_objects().filter(|(_, o)| matches!(o.as_ref(), Object::Stream(s) if s.dict.get(b"PCFullFont").is_some())).count();
        assert_eq!(programs(), 1);
        let mut second = Dict::new();
        embed(&mut doc, &mut second, bytes, "Inter").unwrap();
        assert_eq!(doc.scan_objects().filter(|(_, o)| matches!(o.as_ref(), Object::Stream(s) if s.dict.get(b"PCFullFont").is_some())).count(), 1);
        assert_eq!(face_key("ABCDEF+Droid-Sans-Fallback"), face_key("Droid Sans Fallback"));
    }

    /// A face of a collection (`.ttc`) comes out as a standalone font a PDF can embed, with the
    /// names, glyphs and advances it had in the collection; a standalone static font comes back
    /// byte for byte. Collections used to be refused ("not a font collection").
    #[test]
    fn a_face_of_a_collection_is_a_standalone_font() {
        let regular: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let medium: &[u8] = include_bytes!("../../../assets/fonts/Inter-Medium.ttf");
        assert_eq!(face_count(regular), Ok(1));
        assert!(matches!(standalone_face(regular, 0), Ok(std::borrow::Cow::Borrowed(bytes)) if bytes == regular));
        assert!(standalone_face(regular, 1).is_err(), "a standalone font has one face");
        let collection = collection_of(&[regular, medium]).unwrap();
        assert_eq!(face_count(&collection), Ok(2));
        assert!(validate(&collection).is_err(), "the collection itself is not a font program");
        for (index, original) in [(0u32, regular), (1, medium)] {
            let face = standalone_face(&collection, index).unwrap();
            validate(&face).unwrap();
            assert_eq!(names(&face), names(original), "face {index}");
            let (extracted, source) = (FontRef::new(&face).unwrap(), FontRef::new(original).unwrap());
            for ch in ['A', 'é', 'Ω', 'Ж'] {
                let glyph = source.charmap().map(ch).unwrap();
                assert_eq!(extracted.charmap().map(ch), Some(glyph), "{ch}");
                let advance = |font: &FontRef| font.glyph_metrics(Size::unscaled(), LocationRef::default()).advance_width(glyph);
                assert_eq!(advance(&extracted), advance(&source), "{ch}");
            }
            // The file's checksum adjustment makes the whole file sum to the OpenType constant.
            assert_eq!(table_checksum(&face), SFNT_CHECKSUM_MAGIC, "face {index}");
            // It embeds like any font.
            let (mut doc, mut fonts) = (Document::new_empty(), Dict::new());
            embed(&mut doc, &mut fonts, &face, "Inter").unwrap();
        }
        assert!(standalone_face(&collection, 2).is_err(), "there is no third face");
        // Damaged input is an error, never a panic: cut anywhere, and with offsets past the end.
        // (The last few bytes are padding, so the cut near the end goes well into a table.)
        for cut in [0, 3, 11, 12, 20, 40, collection.len() / 2, collection.len() - 64] {
            let damaged = collection.get(..cut).unwrap();
            assert!(face_count(damaged).and_then(|_| standalone_face(damaged, 1).map(|_| ())).is_err(), "cut at {cut}");
        }
        assert!(face_count(b"not a font").is_err() && standalone_face(b"not a font", 0).is_err());
    }

    /// A variable font is loaded as its default instance: the tables that vary it are left out, so
    /// the PDF holds an ordinary static font. Variable fonts used to be refused.
    #[test]
    fn a_variable_font_is_its_default_instance() {
        let regular: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let source = FontRef::new(regular).unwrap();
        // One weight axis from 400 to 700, default 400, and no named instances.
        let mut fvar = vec![0, 1, 0, 0, 0, 16, 0, 2, 0, 1, 0, 20, 0, 0, 0, 8];
        fvar.extend_from_slice(b"wght");
        for value in [400u32 << 16, 400 << 16, 700 << 16] {
            fvar.extend_from_slice(&value.to_be_bytes());
        }
        fvar.extend_from_slice(&[0, 0, 1, 0]);
        let mut tables: Vec<SfntTable> = Vec::new();
        for record in source.table_directory.table_records() {
            let (start, length) = (record.offset() as usize, record.length() as usize);
            tables.push((record.tag().to_be_bytes(), &regular[start..start + length]));
        }
        tables.push((*b"fvar", &fvar));
        tables.push((*b"gvar", b"variations"));
        tables.sort_by_key(|(tag, _)| *tag);
        let variable = write_sfnt(source.table_directory.sfnt_version(), &tables).unwrap();
        assert!(FontRef::new(&variable).unwrap().fvar().is_ok(), "the fixture is a variable font");
        assert!(validate(&variable).is_err(), "a variable program is not embedded as it is");
        let face = standalone_face(&variable, 0).unwrap();
        validate(&face).unwrap();
        let instance = FontRef::new(&face).unwrap();
        let tags: Vec<[u8; 4]> = instance.table_directory.table_records().iter().map(|record| record.tag().to_be_bytes()).collect();
        assert!(!tags.contains(b"fvar") && !tags.contains(b"gvar"), "{tags:?}");
        assert_eq!(tags.len(), source.table_directory.table_records().len(), "every other table is kept");
        assert_eq!(names(&face), names(regular));
        assert_eq!(instance.charmap().map('A'), source.charmap().map('A'));
    }

    /// A font whose name table declares no open licence is refused, as the asset policy asks,
    /// unless the build was made with `PDFCRAFT_ANY_DOCUMENT_FONT` set. What can't be embedded at
    /// all is refused either way.
    #[test]
    fn a_font_without_an_open_licence_is_refused_unless_the_build_accepts_any() {
        let mut unlicensed = include_bytes!("../../../assets/fonts/Inter-Regular.ttf").to_vec();
        // Blot out the licence wording wherever the name table says it (UTF-16 and single-byte).
        for wording in ["SIL Open Font", "SIL OPEN FONT"] {
            let wide: Vec<u8> = wording.encode_utf16().flat_map(u16::to_be_bytes).collect();
            for needle in [wide, wording.as_bytes().to_vec()] {
                let mut at = 0;
                while let Some(found) = unlicensed[at..].windows(needle.len()).position(|window| window == needle.as_slice()) {
                    unlicensed[at + found..at + found + needle.len()].fill(b'x');
                    at += found + needle.len();
                }
            }
        }
        validate(&unlicensed).unwrap();
        assert_eq!(validate_document_input(&unlicensed).is_ok(), accepts_any_document_font());
        assert_eq!(accepts_any_document_font(), option_env!("PDFCRAFT_ANY_DOCUMENT_FONT").is_some());
        assert!(validate_document_input(b"not a font").is_err());
    }

    #[test]
    fn licence_policy_reads_name_records_one_at_a_time() {
        use skrifa::string::StringId;
        let bytes = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        assert!(validate_document_input(bytes).is_ok(), "an OFL font passes");
        let face = FontRef::new(bytes).unwrap();
        assert_eq!(any_name_record(&face, &[StringId::LICENSE_DESCRIPTION], |t| t.contains("SIL Open Font")), Some(true));
        assert_eq!(any_name_record(&face, &[StringId::LICENSE_DESCRIPTION], |_| false), Some(false));
        assert_eq!(any_name_record(&face, &[], |_| true), Some(false), "no ids, no records");
    }

    #[test]
    fn embedding_the_same_text_again_reuses_its_resource_and_program() {
        let bytes = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let face = FontRef::new(bytes).unwrap();
        let glyph = |c| face.charmap().map(c).unwrap();
        let programs = |doc: &Document| {
            doc.scan_objects().filter(|(_, o)| matches!(o.as_ref(), Object::Stream(s) if s.dict.get(b"PCFullFont").is_some())).count()
        };
        // Objects before the program push it past the first 8192 that the document-wide search reads.
        let mut doc = Document::new_empty();
        for _ in 0..9000 {
            doc.add(Object::Null);
        }
        let mut fonts = Dict::new();
        let full = embed(&mut doc, &mut fonts, bytes, "Inter").unwrap();
        let objects = doc.scan_objects().count();
        assert_eq!(embed(&mut doc, &mut fonts, bytes, "Inter").unwrap(), full, "the same embedding is the same resource");
        let shaped = [("fi".to_string(), glyph('f')), ("é".to_string(), glyph('é'))];
        let first = embed_mapping(&mut doc, &mut fonts, bytes, "Inter", &shaped).unwrap();
        assert_ne!(first, full, "another mapping is another resource");
        assert_eq!(embed_mapping(&mut doc, &mut fonts, bytes, "Inter", &shaped).unwrap(), first);
        assert_eq!(embed_mapping(&mut doc, &mut fonts, bytes, "Inter", &shaped[..1]).map(|r| r != first), Ok(true), "a different mapping");
        assert_eq!(programs(&doc), 1, "one program for every mapping");
        assert_eq!(fonts.iter().count(), 3);
        assert!(doc.scan_objects().count() > objects);
        // Another writer may have compressed the streams since; the page still recognises them.
        for (r, o) in doc.scan_objects().collect::<Vec<_>>() {
            if let Object::Stream(st) = o.as_ref()
                && st.dict.get(b"Filter").is_none()
            {
                let data = st.decoded().unwrap();
                doc.set(r, Object::Stream(Stream::flate(st.dict.clone(), &data)));
            }
        }
        let objects = doc.scan_objects().count();
        assert_eq!(embed(&mut doc, &mut fonts, bytes, "Inter").unwrap(), full);
        assert_eq!(embed_mapping(&mut doc, &mut fonts, bytes, "Inter", &shaped).unwrap(), first);
        assert_eq!(doc.scan_objects().count(), objects, "nothing new is written");
        // A different program under the same name is not taken for this one.
        let other = include_bytes!("../../../assets/fonts/Inter-Medium.ttf");
        let mut more = fonts.clone();
        let medium = embed(&mut doc, &mut more, other, "Inter").unwrap();
        assert!(!fonts.contains(medium.as_bytes()), "{medium} is new");
        assert_eq!(programs(&doc), 2);
    }

    #[test]
    fn the_program_search_decodes_each_candidate_once_and_few_in_all() {
        let bytes = b"the program".as_slice();
        let decoy = |doc: &mut Document| {
            let mut d = Dict::new();
            d.set(b"PCFullFont".to_vec(), Object::Bool(true));
            d.set(b"Length1".to_vec(), Object::Int(bytes.len() as i64));
            doc.add(Object::Stream(Stream::flate(d, &[0; 4096])))
        };
        let resource = |doc: &mut Document, program: ObjRef| {
            let mut descriptor = Dict::new();
            descriptor.set(b"FontFile2".to_vec(), Object::Ref(program));
            let mut cid = Dict::new();
            cid.set(b"FontDescriptor".to_vec(), Object::Dict(descriptor));
            let mut top = Dict::new();
            top.set(b"DescendantFonts".to_vec(), Object::Array(vec![Object::Dict(cid)]));
            doc.add(Object::Dict(top))
        };
        // 200 resources sharing one decoy program decode it once.
        let mut doc = Document::new_empty();
        let shared = decoy(&mut doc);
        let mut fonts = Dict::new();
        for i in 0..200 {
            let r = resource(&mut doc, shared);
            fonts.set(format!("PCFull{i}").into_bytes(), Object::Ref(r));
        }
        let mut programs = ProgramMatch::new(bytes);
        assert_eq!(existing_program(&doc, &fonts, &mut programs), None);
        assert_eq!(programs.decodes, 1);
        // 100 distinct decoys decode a bounded number of times.
        let mut doc = Document::new_empty();
        let mut fonts = Dict::new();
        for i in 0..100 {
            let program = decoy(&mut doc);
            let r = resource(&mut doc, program);
            fonts.set(format!("PCFull{i}").into_bytes(), Object::Ref(r));
        }
        let mut programs = ProgramMatch::new(bytes);
        assert_eq!(existing_program(&doc, &fonts, &mut programs), None);
        assert_eq!(programs.decodes, MAX_PROGRAM_DECODES);
    }

    #[test]
    fn the_full_program_is_written_compressed_and_another_page_reuses_it() {
        let bytes = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let mut doc = Document::new_empty();
        let mut first = Dict::new();
        embed(&mut doc, &mut first, bytes, "Inter").unwrap();
        let program = || {
            doc.scan_objects()
                .find_map(|(_, o)| match o.as_ref() {
                    Object::Stream(s) if s.dict.get(b"PCFullFont").is_some() => Some(s.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let stream = program();
        assert_eq!(stream.dict.name(b"Filter"), Some(b"FlateDecode".as_slice()));
        assert!(stream.raw.len() < bytes.len(), "{} of {} bytes", stream.raw.len(), bytes.len());
        assert_eq!(stream.decoded().unwrap(), bytes);
        // Another page's resources don't have it, so it is found among the document's objects.
        let objects = doc.scan_objects().count();
        let mut second = Dict::new();
        embed(&mut doc, &mut second, bytes, "Inter").unwrap();
        let programs = doc.scan_objects().filter(|(_, o)| matches!(o.as_ref(), Object::Stream(s) if s.dict.get(b"PCFullFont").is_some())).count();
        assert_eq!(programs, 1, "one program for both pages");
        assert!(doc.scan_objects().count() > objects, "the second page gets its own resource");
    }
}
