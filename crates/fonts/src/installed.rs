//! Fonts installed on this machine, as the last source of an outline for a character that
//! neither the page's fonts nor the bundled faces have.
//!
//! Only a build that accepts any document font reads them (`embedding::accepts_any_document_font`).
//! Every other build keeps installed fonts out of PDFs (AGENTS.md §1.4): [`face_with`] is `None` there.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, MetadataProvider};

use crate::embedding::{MAX_FONT_FILE_BYTES, accepts_any_document_font, face_count, names, standalone_face, validate_document_input};
use crate::{CraftFont, GlyphError, GlyphOutline};

/// Most font files one machine's folders are listed for.
const MAX_FONT_FILES: usize = 2048;

/// How deep the font folders are walked: families sit in subfolders of the system folder.
const MAX_FOLDER_DEPTH: usize = 3;

/// Most faces read from one collection file.
const MAX_FACES_PER_FILE: u32 = 16;

/// Most character mappings read from one face: a damaged cmap can repeat the whole of Unicode.
const MAX_MAPPED_CHARACTERS: usize = 1 << 21;

/// Most faces kept in memory. A kept face stays for the rest of the run, so both caps bound it.
const MAX_KEPT_FACES: usize = 12;

/// Most bytes of kept faces.
const MAX_KEPT_BYTES: usize = 256 << 20;

/// The widest advance taken from a face, in em: the widest ligatures span a few.
const MAX_ADVANCE_EM: f64 = 4.0;

/// Plain sans faces the desktop systems ship, by file name, best first. They are read before the
/// rest of the folder so that a character takes one of them where the machine has it, rather
/// than whichever file sorts first (a display or serif face).
const PREFERRED_FILE_NAMES: [&str; 12] = [
    "malgun.ttf",
    "segoeui.ttf",
    "seguisym.ttf",
    "msyh.ttc",
    "yugothr.ttc",
    "leelawui.ttf",
    "nirmala.ttc",
    "seguiemj.ttf",
    "applesdgothicneo.ttc",
    "arial unicode.ttf",
    "notosans-regular.ttf",
    "dejavusans.ttf",
];

const FONT_FILE_EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];

/// An installed face that has `ch`, the first by [`PREFERRED_FILE_NAMES`] and then by path.
/// `None` when the build does not accept any document font, or no usable installed face has it.
pub fn face_with(ch: char) -> Option<&'static CraftFont> {
    if !accepts_any_document_font() {
        return None;
    }
    static CATALOGUE: OnceLock<Mutex<Catalogue>> = OnceLock::new();
    let catalogue = CATALOGUE.get_or_init(|| Mutex::new(Catalogue::of(&installed_folders())));
    // A lookup that panicked left lists of paths and ranges behind, which are still valid.
    catalogue.lock().unwrap_or_else(PoisonError::into_inner).face_with(ch)
}

/// The outline of `ch` in an installed face, bounded like the bundled faces' glyphs. A mark that
/// sits on the letter before it has no advance: it is drawn where the face places it.
pub fn glyph(face: &CraftFont, ch: char) -> Result<GlyphOutline, GlyphError> {
    let font = FontRef::new(face.bytes).map_err(|_| GlyphError::Missing)?;
    let glyph = font.charmap().map(ch).filter(|glyph| glyph.to_u32() != 0).ok_or(GlyphError::Missing)?;
    let location = LocationRef::default();
    let units_per_em = f64::from(font.metrics(Size::unscaled(), location).units_per_em.max(1));
    let width = f64::from(font.glyph_metrics(Size::unscaled(), location).advance_width(glyph).unwrap_or(0.0)) / units_per_em;
    if !(0.0..=MAX_ADVANCE_EM).contains(&width) {
        return Err(GlyphError::Missing);
    }
    crate::script::bounded_outline(&font, glyph, width)
}

/// The folders fonts are installed in, for the system and for the person signed in.
fn installed_folders() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(windows) {
        let system = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot")).map(|root| PathBuf::from(root).join("Fonts"));
        let personal = std::env::var_os("LOCALAPPDATA").map(|local| PathBuf::from(local).join("Microsoft/Windows/Fonts"));
        system.into_iter().chain(personal).collect()
    } else if cfg!(target_os = "macos") {
        let personal = home.map(|home| home.join("Library/Fonts"));
        [PathBuf::from("/System/Library/Fonts"), PathBuf::from("/Library/Fonts")].into_iter().chain(personal).collect()
    } else {
        let personal = home.into_iter().flat_map(|home| [home.join(".local/share/fonts"), home.join(".fonts")]);
        [PathBuf::from("/usr/share/fonts"), PathBuf::from("/usr/local/share/fonts")].into_iter().chain(personal).collect()
    }
}

/// The font files under `folders`, in path order. Links are not followed.
fn font_files(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut pending: Vec<(PathBuf, usize)> = folders.iter().map(|folder| (folder.clone(), 0)).collect();
    let mut files = Vec::new();
    let mut visited = 0_usize;
    while let Some((path, depth)) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > MAX_FONT_FILES.saturating_mul(4) || files.len() >= MAX_FONT_FILES {
            break;
        }
        let Ok(entry) = std::fs::symlink_metadata(&path) else { continue };
        if entry.is_file() && has_font_extension(&path) {
            files.push(path);
        } else if entry.is_dir() && depth < MAX_FOLDER_DEPTH {
            let children = std::fs::read_dir(&path).into_iter().flatten().filter_map(Result::ok);
            pending.extend(children.map(|child| (child.path(), depth.saturating_add(1))));
        }
    }
    files.sort();
    files
}

fn has_font_extension(path: &Path) -> bool {
    path.extension().and_then(std::ffi::OsStr::to_str).is_some_and(|e| FONT_FILE_EXTENSIONS.iter().any(|known| e.eq_ignore_ascii_case(known)))
}

/// Where `path` stands among [`PREFERRED_FILE_NAMES`]; every other file comes after them.
fn preference(path: &Path) -> usize {
    let name = path.file_name().and_then(std::ffi::OsStr::to_str).unwrap_or_default().to_ascii_lowercase();
    PREFERRED_FILE_NAMES.iter().position(|preferred| *preferred == name).unwrap_or(PREFERRED_FILE_NAMES.len())
}

/// A font file whole, or `None` for one that cannot be read or is over the size a font file may have.
fn read_font_file(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let length = std::fs::metadata(path).ok()?.len();
    if length > MAX_FONT_FILE_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(length.saturating_add(1)).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 == length).then_some(bytes)
}

/// The characters `font` maps to a glyph, as sorted inclusive ranges.
fn coverage(font: &FontRef) -> Vec<(u32, u32)> {
    let mapped = font.charmap().mappings().take(MAX_MAPPED_CHARACTERS).filter(|(_, glyph)| glyph.to_u32() != 0);
    let mut codes: Vec<u32> = mapped.map(|(code, _)| code).collect();
    codes.sort_unstable();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for code in codes {
        match ranges.last_mut() {
            Some((_, end)) if code <= end.saturating_add(1) => *end = code,
            _ => ranges.push((code, code)),
        }
    }
    ranges
}

fn covers(ranges: &[(u32, u32)], code: u32) -> bool {
    use std::cmp::Ordering;
    let place = |&(start, end): &(u32, u32)| {
        if code < start {
            Ordering::Greater
        } else if code > end {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    };
    ranges.binary_search_by(place).is_ok()
}

/// Face `index` of the file at `path` as a face the fallback fonts can draw from, within
/// `allowance` bytes. It is a font the document may hold (`validate_document_input`: one whose
/// own embedding permission allows it), like a font the person loads by hand.
///
/// The face is leaked on purpose: the fallback machinery holds `&'static CraftFont`, as it does
/// for the bundled faces, and [`MAX_KEPT_FACES`] and [`MAX_KEPT_BYTES`] bound what is ever kept.
fn load_face(path: &Path, index: u32, allowance: usize) -> Option<&'static CraftFont> {
    let file = read_font_file(path)?;
    let program = standalone_face(&file, index).ok()?;
    if program.len() > allowance || validate_document_input(&program).is_err() {
        return None;
    }
    let (family, _) = names(&program).ok()?;
    let bytes: &'static [u8] = Box::leak(program.into_owned().into_boxed_slice());
    let family: &'static str = Box::leak(family.into_boxed_str());
    Some(Box::leak(Box::new(CraftFont { family, style: "Regular", scripts: &[], bytes })))
}

enum FaceState {
    /// Known by its coverage only; its file is read again when a character needs it.
    Listed,
    Kept(&'static CraftFont),
    /// It could not be loaded, or the caps were reached: it is not tried again.
    Refused,
}

/// One face of a font file: where it is, and which characters it maps.
struct ListedFace {
    path: PathBuf,
    index: u32,
    coverage: Vec<(u32, u32)>,
    state: FaceState,
}

impl ListedFace {
    fn kept_with(&self, code: u32) -> Option<&'static CraftFont> {
        match self.state {
            FaceState::Kept(face) if covers(&self.coverage, code) => Some(face),
            _ => None,
        }
    }

    fn may_draw(&self, code: u32) -> bool {
        matches!(self.state, FaceState::Listed) && covers(&self.coverage, code)
    }
}

/// The machine's font files, read one at a time and only as far as a character needs: a file is
/// listed (its faces' coverage kept, its bytes dropped) the first time a lookup reaches it.
struct Catalogue {
    /// Files not listed yet; the next one to read is last.
    unread: Vec<PathBuf>,
    listed: Vec<ListedFace>,
    kept_faces: usize,
    kept_bytes: usize,
}

impl Catalogue {
    fn of(folders: &[PathBuf]) -> Self {
        let mut unread = font_files(folders);
        // Stable: path order within the preferred files and within the rest.
        unread.sort_by_key(|path| preference(path));
        unread.reverse();
        Self { unread, listed: Vec::new(), kept_faces: 0, kept_bytes: 0 }
    }

    fn face_with(&mut self, ch: char) -> Option<&'static CraftFont> {
        let code = u32::from(ch);
        // A face already in memory first, so one face draws as many of the characters as it can.
        if let Some(face) = self.listed.iter().find_map(|face| face.kept_with(code)) {
            return Some(face);
        }
        let mut position = 0_usize;
        loop {
            if position >= self.listed.len() {
                let path = self.unread.pop()?;
                self.list(&path);
                continue;
            }
            if self.listed.get(position).is_some_and(|face| face.may_draw(code))
                && let Some(face) = self.keep(position)
            {
                return Some(face);
            }
            position = position.saturating_add(1);
        }
    }

    /// Adds the faces of the file at `path` to the list, by their coverage.
    fn list(&mut self, path: &Path) {
        let Some(file) = read_font_file(path) else { return };
        let Ok(count) = face_count(&file) else { return };
        for index in 0..count.min(MAX_FACES_PER_FILE) {
            let Ok(font) = FontRef::from_index(&file, index) else { continue };
            self.listed.push(ListedFace { path: path.to_path_buf(), index, coverage: coverage(&font), state: FaceState::Listed });
        }
    }

    /// Loads listed face `position` and keeps it, or marks it refused.
    fn keep(&mut self, position: usize) -> Option<&'static CraftFont> {
        let listed = self.listed.get(position)?;
        let allowance = MAX_KEPT_BYTES.saturating_sub(self.kept_bytes);
        let face = if self.kept_faces < MAX_KEPT_FACES { load_face(&listed.path, listed.index, allowance) } else { None };
        let listed = self.listed.get_mut(position)?;
        let Some(face) = face else {
            listed.state = FaceState::Refused;
            return None;
        };
        listed.state = FaceState::Kept(face);
        self.kept_faces = self.kept_faces.saturating_add(1);
        self.kept_bytes = self.kept_bytes.saturating_add(face.bytes.len());
        Some(face)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own under the temporary folder, holding `files`.
    fn folder_with(name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("pdfcraft-installed-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        for (file_name, bytes) in files {
            std::fs::write(folder.join(file_name), bytes).unwrap();
        }
        folder
    }

    #[test]
    fn a_character_comes_from_the_installed_face_that_has_it() {
        let folder = folder_with("face", &[("Inter-Regular.ttf", crate::INTER_REGULAR), ("notes.txt", b"not a font")]);
        let mut catalogue = Catalogue::of(std::slice::from_ref(&folder));
        let face = catalogue.face_with('A').expect("Inter has A");
        assert_eq!(face.family, "Inter");
        assert!(glyph(face, 'A').is_ok_and(|outline| !outline.contours.is_empty()));
        assert!(std::ptr::eq(catalogue.face_with('B').unwrap(), face), "a face in memory is used again rather than loaded twice");
        assert!(catalogue.face_with('한').is_none(), "Inter has no Hangul, and nothing else is installed here");
        assert!(catalogue.unread.is_empty(), "a character no face has reads every file once");
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_face_of_an_installed_collection_is_used() {
        let collection = crate::embedding::collection_of(&[crate::INTER_REGULAR, crate::INTER_REGULAR]).unwrap();
        let folder = folder_with("collection", &[("Inter.ttc", &collection)]);
        let mut catalogue = Catalogue::of(std::slice::from_ref(&folder));
        let face = catalogue.face_with('A').expect("the collection's first face has A");
        assert!(FontRef::new(face.bytes).is_ok(), "the kept face is a standalone font, which the outlines are read from");
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_damaged_or_missing_font_file_is_passed_over() {
        let folder = folder_with("damaged", &[("broken.ttf", b"\0\x01\0\0 not really a font"), ("Inter-Regular.ttf", crate::INTER_REGULAR)]);
        let mut catalogue = Catalogue::of(&[folder.clone(), folder.join("no such folder")]);
        assert!(catalogue.face_with('A').is_some());
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn coverage_is_sorted_ranges_that_answer_by_character() {
        let font = FontRef::new(crate::INTER_REGULAR).unwrap();
        let ranges = coverage(&font);
        assert!(ranges.windows(2).all(|pair| pair[0].1.saturating_add(1) < pair[1].0), "ranges are sorted and never touch");
        assert!(covers(&ranges, u32::from('A')) && covers(&ranges, u32::from('z')));
        assert!(!covers(&ranges, u32::from('한')) && !covers(&ranges, 0x10_FFFF));
    }

    #[test]
    fn a_mark_without_an_advance_still_has_an_outline() {
        let face = CraftFont { family: "Inter", style: "Regular", scripts: &[], bytes: crate::INTER_REGULAR };
        let mark = glyph(&face, '\u{0301}').expect("Inter has the combining acute accent");
        assert_eq!(mark.width, 0.0);
        assert!(!mark.contours.is_empty());
        assert_eq!(glyph(&face, '한').err(), Some(GlyphError::Missing));
    }

    #[test]
    fn only_a_build_that_accepts_any_document_font_reads_installed_fonts() {
        if !accepts_any_document_font() {
            assert!(face_with('A').is_none());
        }
    }
}
