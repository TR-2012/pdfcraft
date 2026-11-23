//! Explicit document-font discovery. Interface fallbacks never enter this catalogue.
use std::path::{Path, PathBuf};

/// A font file's face as the standalone program `Edit::RegisterTextFont` takes, and the most
/// bytes of a font file read for one: for callers that load a face of a collection.
pub use pdfcraft_fonts::embedding::{MAX_FONT_FILE_BYTES, standalone_face};

#[derive(Clone, Debug)]
pub struct FontFile {
    pub path: PathBuf,
    pub family: String,
    pub face: String,
    /// Which face of the file this is: 0 for a standalone font, its place in a collection
    /// (`.ttc`, `.otc`) otherwise. `pdfcraft_fonts::embedding::standalone_face` takes it.
    pub face_index: u32,
}

/// Standard user-installed font folders; discovery is performed only on request.
pub fn installed_directories() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        let mut dirs = vec![PathBuf::from("/Library/Fonts")];
        if let Some(home) = home {
            dirs.push(home.join("Library/Fonts"));
        }
        dirs
    } else if cfg!(windows) {
        let mut dirs = std::env::var_os("WINDIR").map(|p| PathBuf::from(p).join("Fonts")).into_iter().collect::<Vec<_>>();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft/Windows/Fonts"));
        }
        dirs
    } else {
        let mut dirs = vec![PathBuf::from("/usr/share/fonts"), PathBuf::from("/usr/local/share/fonts")];
        if let Some(home) = home {
            dirs.extend([home.join(".local/share/fonts"), home.join(".fonts")]);
        }
        dirs
    }
}

/// The total size of the fonts one scan lists.
const SCAN_BUDGET: u64 = 64 << 20;

/// The same in a build that accepts any document font: every installed font qualifies there, and
/// a machine's font folder holds a gigabyte or more of them.
const ANY_FONT_SCAN_BUDGET: u64 = 4 << 30;

/// Most faces listed from one collection file.
const MAX_FACES_PER_FILE: u32 = 16;

/// Bounded scan with resolved roots and no descendant symlinks.
/// Only embeddable faces are listed, openly licensed ones unless the build accepts any document
/// font; a collection lists each of its faces.
/// The caller controls roots (automation resolves confinement before calling this).
pub fn discover(roots: &[PathBuf]) -> Vec<FontFile> {
    let budget = if pdfcraft_fonts::embedding::accepts_any_document_font() { ANY_FONT_SCAN_BUDGET } else { SCAN_BUDGET };
    discover_budgeted(roots, budget)
}

/// Bounded scan with resolved roots and no descendant symlinks. `budget` caps the total
/// size of the fonts listed: a rejected file (unreadable, not openly licensed, a duplicate)
/// does not spend it. Every file read
/// counts against four times the budget, so a folder of unusable fonts cannot stall the scan.
pub fn discover_budgeted(roots: &[PathBuf], mut budget: u64) -> Vec<FontFile> {
    use std::io::Read;
    // User font directories themselves are often links. Resolve only the explicitly
    // supplied roots; links found below them must not escape an automation read root.
    let mut pending: Vec<_> = roots.iter().take(16).filter_map(|r| r.canonicalize().ok()).map(|r| (r, 0usize)).collect();
    let mut files = Vec::new();
    let mut entries = 0usize;
    let mut read_allowance = budget.saturating_mul(4);
    let mut seen = std::collections::HashSet::new();
    let mut seen_face_content = std::collections::HashSet::new();
    while let Some((path, depth)) = pending.pop() {
        if entries >= 4096 || budget == 0 || read_allowance == 0 {
            break;
        }
        entries += 1;
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() || !seen.insert(path.clone()) {
            continue;
        }
        if meta.is_dir() && depth < 4 {
            if let Ok(children) = std::fs::read_dir(&path) {
                let remaining = 4096usize.saturating_sub(entries.saturating_add(pending.len()));
                // Best effort: unreadable entries are skipped and do not count against the entry allowance.
                let mut sorted_children: Vec<_> = children.filter_map(Result::ok).take(remaining).map(|p| p.path()).collect();
                sorted_children.sort();
                pending.extend(sorted_children.into_iter().rev().map(|p| (p, depth + 1)));
            }
        } else if meta.is_file() && meta.len() <= pdfcraft_fonts::embedding::MAX_FONT_FILE_BYTES as u64 && font_extension(&path) {
            if meta.len() > budget || meta.len() > read_allowance {
                continue;
            }
            read_allowance = read_allowance.saturating_sub(meta.len());
            let mut bytes = Vec::new();
            let read = std::fs::File::open(&path).and_then(|f| f.take(meta.len().saturating_add(1)).read_to_end(&mut bytes));
            if read.is_err() || bytes.len() != meta.len() as usize {
                continue;
            }
            if list_faces(&path, &bytes, &mut seen_face_content, &mut files) {
                budget = budget.saturating_sub(meta.len());
            }
        }
    }
    files.sort_by(|a, b| a.face.cmp(&b.face).then(a.path.cmp(&b.path)));
    files
}

/// Lists the document fonts in one font file: each face that passes
/// `validate_document_input` as the standalone program it would be registered as, once per
/// distinct content. Returns whether any face was listed.
fn list_faces(path: &Path, file: &[u8], seen: &mut std::collections::HashSet<(String, (usize, u64))>, files: &mut Vec<FontFile>) -> bool {
    let Ok(count) = pdfcraft_fonts::embedding::face_count(file) else { return false };
    let mut listed = false;
    for face_index in 0..count.min(MAX_FACES_PER_FILE) {
        let Ok(program) = pdfcraft_fonts::embedding::standalone_face(file, face_index) else { continue };
        if pdfcraft_fonts::embedding::validate_document_input(&program).is_err() {
            continue;
        }
        let Ok((family, face)) = pdfcraft_fonts::embedding::names(&program) else { continue };
        if seen.insert((face.clone(), font_content_key(&program))) {
            files.push(FontFile { path: path.to_path_buf(), family, face, face_index });
            listed = true;
        }
    }
    listed
}

fn font_content_key(bytes: &[u8]) -> (usize, u64) {
    use std::hash::{DefaultHasher, Hasher};
    let mut h = DefaultHasher::new();
    h.write(bytes);
    (bytes.len(), h.finish())
}
fn font_extension(path: &Path) -> bool {
    const FONT_FILE_EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];
    path.extension().and_then(std::ffi::OsStr::to_str).is_some_and(|e| FONT_FILE_EXTENSIONS.iter().any(|known| e.eq_ignore_ascii_case(known)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_linked_font_root_is_scanned_but_descendant_links_are_not() {
        let root = std::env::temp_dir().join(format!("pdfcraft-font-links-{}", std::process::id()));
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/Inter-Regular.ttf");
        std::fs::copy(&source, real.join("regular.ttf")).unwrap();
        let link = root.join("linked");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::os::unix::fs::symlink(&real, real.join("cycle")).unwrap();
        std::os::unix::fs::symlink(source.with_file_name("Inter-Medium.ttf"), real.join("escape.ttf")).unwrap();
        let found = discover(&[link]);
        let expected = real.canonicalize().unwrap().join("regular.ttf");
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found.first().unwrap().face, "Inter-Regular");
        assert_eq!(found.first().unwrap().path, expected);
    }

    #[test]
    fn discovery_keeps_all_valid_faces_within_its_scan_budget() {
        let root = std::env::temp_dir().join(format!("pdfcraft-font-count-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/Inter-Regular.ttf");
        let bytes = std::fs::read(source).unwrap();
        for id in 0..501u32 {
            let mut font = bytes.clone();
            // Valid standalone fonts with distinct bytes: an unreferenced trailer
            // does not change their tables or embedding licence.
            font.extend_from_slice(&id.to_le_bytes());
            std::fs::write(root.join(format!("font-{id:03}.ttf")), font).unwrap();
        }
        let found = discover_budgeted(std::slice::from_ref(&root), (bytes.len() as u64 + 4) * 501);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(found.len(), 501, "do not silently discard accepted fonts after sorting");
    }

    /// A collection (`.ttc`) lists each of its faces with its place in the file; collections used
    /// to be skipped.
    #[test]
    fn a_collection_lists_each_of_its_faces() {
        let root = std::env::temp_dir().join(format!("pdfcraft-font-collection-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
        let regular = std::fs::read(fonts.join("Inter-Regular.ttf")).unwrap();
        let medium = std::fs::read(fonts.join("Inter-Medium.ttf")).unwrap();
        let collection = pdfcraft_fonts::embedding::collection_of(&[&regular, &medium]).unwrap();
        std::fs::write(root.join("inter.ttc"), &collection).unwrap();
        let found = discover(std::slice::from_ref(&root));
        std::fs::remove_dir_all(&root).unwrap();
        let listed: Vec<(&str, u32)> = found.iter().map(|font| (font.face.as_str(), font.face_index)).collect();
        assert_eq!(listed, [("Inter-Medium", 1), ("Inter-Regular", 0)], "sorted by face name");
        assert!(found.iter().all(|font| font.path.ends_with("inter.ttc")));
        let face = standalone_face(&collection, 1).unwrap();
        assert_eq!(pdfcraft_fonts::embedding::names(&face).unwrap().1, "Inter-Medium");
    }

    #[test]
    fn test_installed_directories() {
        let dirs = installed_directories();
        assert!(!dirs.is_empty());
    }

    #[test]
    fn test_font_extension() {
        assert!(font_extension(Path::new("Arial.ttf")));
        assert!(font_extension(Path::new("Noto.otf")));
        assert!(font_extension(Path::new("CUSTOM.TTF")));
        assert!(font_extension(Path::new("ROBOTO.OTF")));
        assert!(font_extension(Path::new("gulim.ttc")) && font_extension(Path::new("Collection.OTC")));
        assert!(!font_extension(Path::new("document.pdf")));
        assert!(!font_extension(Path::new("font.woff")));
        assert!(!font_extension(Path::new("font.woff2")));
        assert!(!font_extension(Path::new("no_extension")));
    }

    #[test]
    fn test_font_content_key() {
        let empty = font_content_key(&[]);
        assert_eq!(empty.0, 0);

        let a = font_content_key(b"TrueType font sample data bytes");
        let b = font_content_key(b"TrueType font sample data bytes");
        let c = font_content_key(b"Different font sample data bytes");

        assert_eq!(a, b, "identical bytes must produce identical keys");
        assert_ne!(a, c, "differing bytes must produce distinct keys");
        assert_eq!(a.0, b"TrueType font sample data bytes".len());
    }

    #[test]
    fn test_discover_empty_and_nonexistent_roots() {
        let empty = discover(&[]);
        assert!(empty.is_empty());

        let nonexistent = discover(&[PathBuf::from("Z:\\nonexistent_font_path_xyz_12345")]);
        assert!(nonexistent.is_empty());
    }

    #[test]
    fn test_discover_deduplicates_non_adjacent_same_content() {
        let root = std::env::temp_dir().join(format!("pdfcraft-font-dedup-test-{}", std::process::id()));
        let dir_a = root.join("a");
        let dir_b = root.join("b");
        let dir_c = root.join("c");
        let _ = std::fs::create_dir_all(&dir_a);
        let _ = std::fs::create_dir_all(&dir_b);
        let _ = std::fs::create_dir_all(&dir_c);

        let inter_reg = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/Inter-Regular.ttf");
        let inter_med = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/Inter-Medium.ttf");

        if inter_reg.exists() && inter_med.exists() {
            let _ = std::fs::copy(&inter_reg, dir_a.join("FontA.ttf"));
            let _ = std::fs::copy(&inter_med, dir_b.join("FontB.ttf"));
            let _ = std::fs::copy(&inter_reg, dir_c.join("FontC.ttf"));

            let discovered = discover(std::slice::from_ref(&root));
            let _ = std::fs::remove_dir_all(&root);

            assert_eq!(discovered.len(), 2, "identical content in non-adjacent paths is deduplicated");
            let faces: Vec<&str> = discovered.iter().map(|f| f.face.as_str()).collect();
            assert!(faces.contains(&"Inter-Regular"));
            assert!(faces.contains(&"Inter-Medium"));
        } else {
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn rejected_fonts_do_not_spend_the_budget_and_the_order_is_deterministic() {
        let root = std::env::temp_dir().join(format!("pdfcraft-font-budget-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
        let reg_len = std::fs::metadata(fonts.join("Inter-Regular.ttf")).unwrap().len();
        let med_len = std::fs::metadata(fonts.join("Inter-Medium.ttf")).unwrap().len();

        // An unusable candidate, scanned first (sorted before the real fonts).
        std::fs::write(root.join("00_candidate_invalid.ttf"), vec![0u8; 400_000]).unwrap();
        std::fs::copy(fonts.join("Inter-Regular.ttf"), root.join("01_Inter-Regular.ttf")).unwrap();

        // The budget fits Inter-Regular alone; the rejected file must not use it up first.
        let run1 = discover_budgeted(std::slice::from_ref(&root), reg_len + 10);
        let run2 = discover_budgeted(std::slice::from_ref(&root), reg_len + 10);
        let faces = |run: &[FontFile]| run.iter().map(|f| (f.face.clone(), f.path.clone())).collect::<Vec<_>>();
        assert_eq!(run1.iter().map(|f| f.face.as_str()).collect::<Vec<_>>(), ["Inter-Regular"], "a rejected file spends no budget");
        assert_eq!(faces(&run1), faces(&run2));

        // Accepted fonts do spend it: Inter-Medium no longer fits after Inter-Regular.
        std::fs::copy(fonts.join("Inter-Medium.ttf"), root.join("02_Inter-Medium.ttf")).unwrap();
        let one = discover_budgeted(std::slice::from_ref(&root), reg_len + 10);
        assert_eq!(one.iter().map(|f| f.face.as_str()).collect::<Vec<_>>(), ["Inter-Regular"]);
        let both = discover_budgeted(std::slice::from_ref(&root), reg_len + med_len);
        assert_eq!(both.len(), 2);

        // Reading stops at four times the budget, whatever was rejected.
        for i in 0..4 {
            std::fs::write(root.join(format!("00_unusable_{i}.ttf")), vec![0u8; reg_len as usize]).unwrap();
        }
        let capped = discover_budgeted(std::slice::from_ref(&root), reg_len + 10);
        let _ = std::fs::remove_dir_all(&root);
        assert!(capped.is_empty(), "the read allowance ends the scan before the real fonts");
    }
}
