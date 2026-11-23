//! What an edit draws for a character that no font of the page and no bundled face has, in a
//! build that does not refuse an edit over it: the outline of a face installed on the machine,
//! else a placeholder (`pdfcraft_fonts::last_resort`). The character itself stays in the text
//! either way (the generated font's ToUnicode map), so it is still found, copied and edited.

use super::{CraftFont, glyph_path};

/// The least advance a glyph is given, in em: one glyph unit, a hundredth of a point at 12 pt.
/// A mark or an unseen character has no advance of its own, but a width of 0 in a font's
/// `/Widths` is read back as "this code has no glyph" (subsets write 0 for the codes they dropped),
/// and the next edit of the paragraph could then not show the character again.
const LEAST_ADVANCE_EM: f64 = 0.001;

#[cfg(test)]
thread_local! {
    /// Set by a test to edit as a build that accepts any document font does.
    pub(crate) static DRAWS_UNDRAWABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether an edit draws a character nothing has a glyph for, instead of refusing it
/// (`pdfcraft_fonts::last_resort::draws`: the build of a person who chose to answer for the
/// fonts on their own machine). Every other build keeps the refusal, which says how to get the
/// character drawn, and keeps installed fonts out of PDFs (AGENTS.md §1.4).
pub(crate) fn draws_undrawable_characters() -> bool {
    #[cfg(test)]
    if DRAWS_UNDRAWABLE.with(std::cell::Cell::get) {
        return true;
    }
    pdfcraft_fonts::last_resort::draws()
}

/// `ch` as an installed face draws it: the face, the glyph procedure and the advance in em.
pub(super) fn installed_glyph(ch: char) -> Option<(&'static CraftFont, Vec<u8>, f64)> {
    let face = pdfcraft_fonts::installed::face_with(ch)?;
    let mut outline = pdfcraft_fonts::installed::glyph(face, ch).ok()?;
    outline.width = outline.width.max(LEAST_ADVANCE_EM);
    Some((face, glyph_path(&outline), outline.width))
}

/// What stands for `ch` when no face at all has it: its glyph procedure and advance in em. An
/// empty box, or nothing for white space and the characters that are never seen.
pub(super) fn placeholder_glyph(ch: char) -> (Vec<u8>, f64) {
    let mut outline = pdfcraft_fonts::last_resort::placeholder(ch);
    outline.width = outline.width.max(LEAST_ADVANCE_EM);
    (glyph_path(&outline), outline.width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_that_is_never_seen_still_has_an_advance_a_font_can_list() {
        let (procedure, advance) = placeholder_glyph('\u{200B}');
        assert_eq!(advance, LEAST_ADVANCE_EM);
        assert!(procedure.starts_with(b"1 0 "), "the glyph's own width says the same: {}", String::from_utf8_lossy(&procedure));
    }

    #[test]
    fn a_missing_character_is_a_box_of_two_closed_squares() {
        let (procedure, advance) = placeholder_glyph('\u{0378}');
        assert!(advance > 0.5);
        assert_eq!(String::from_utf8_lossy(&procedure).matches("\nh\n").count(), 2);
    }

    #[test]
    fn only_a_build_that_accepts_any_document_font_draws_them() {
        assert_eq!(draws_undrawable_characters(), pdfcraft_fonts::embedding::accepts_any_document_font());
        DRAWS_UNDRAWABLE.with(|draws| draws.set(true));
        assert!(draws_undrawable_characters());
        DRAWS_UNDRAWABLE.with(|draws| draws.set(false));
    }
}
