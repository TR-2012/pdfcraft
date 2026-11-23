//! What a paragraph edit draws for a character that no font of the page and no bundled face
//! has, in a build that does not refuse an edit over it: the outline of a face installed on the
//! machine, else a placeholder. The character itself stays in the text either way (the generated
//! font's ToUnicode map), so it is still found, copied and edited.

use super::{CraftFont, glyph_path};

/// The advance of a tab that no font has a glyph for, in em: about four spaces.
const TAB_ADVANCE_EM: f64 = 1.0;

/// The advance of any other white space no font has, in em: about one space.
const SPACE_ADVANCE_EM: f64 = 0.25;

/// The least advance a glyph is given, in em: one glyph unit, a hundredth of a point at 12 pt.
/// A mark or an unseen character has no advance of its own, but a width of 0 in a font's
/// `/Widths` is read back as "this code has no glyph" (subsets write 0 for the codes they dropped),
/// and the next edit of the paragraph could then not show the character again.
const LEAST_ADVANCE_EM: f64 = 0.001;

/// The advance of the box that stands for a missing character, in em.
const BOX_ADVANCE_EM: f64 = 0.6;

/// The same for the characters East Asian text sets a full em wide, and those after them.
const WIDE_BOX_ADVANCE_EM: f64 = 1.0;

/// The first of the wide characters (CJK Radicals Supplement).
const FIRST_WIDE_CHARACTER: u32 = 0x2E80;

/// The box in glyph units (1000 to the em): the gap at each side, its height and its line width.
const BOX_SIDE_GAP: f64 = 60.0;
const BOX_HEIGHT: f64 = 700.0;
const BOX_LINE_WIDTH: f64 = 50.0;

#[cfg(test)]
thread_local! {
    /// Set by a test to edit as a build that accepts any document font does.
    pub(crate) static DRAWS_UNDRAWABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether an edit draws a character nothing has a glyph for, instead of refusing it. It is the
/// switch that lets any font into a document: the build of a person who chose to answer for the
/// fonts on their own machine. Every other build keeps the refusal, which says how to get the
/// character drawn, and keeps installed fonts out of PDFs (AGENTS.md §1.4).
pub(super) fn draws_undrawable_characters() -> bool {
    #[cfg(test)]
    if DRAWS_UNDRAWABLE.with(std::cell::Cell::get) {
        return true;
    }
    pdfcraft_fonts::embedding::accepts_any_document_font()
}

/// `ch` as an installed face draws it: the face, the glyph procedure and the advance in em.
pub(super) fn installed_glyph(ch: char) -> Option<(&'static CraftFont, Vec<u8>, f64)> {
    let face = pdfcraft_fonts::installed::face_with(ch)?;
    let mut outline = pdfcraft_fonts::installed::glyph(face, ch).ok()?;
    outline.width = outline.width.max(LEAST_ADVANCE_EM);
    Some((face, glyph_path(&outline), outline.width))
}

/// What stands for `ch` when no face at all has it: its glyph procedure and advance in em.
/// White space and the characters that are never seen (controls, joiners, direction marks) show
/// nothing; anything else shows an empty box, the usual sign of a missing glyph.
pub(super) fn placeholder_glyph(ch: char) -> (Vec<u8>, f64) {
    if ch == '\t' {
        return (blank(TAB_ADVANCE_EM), TAB_ADVANCE_EM);
    }
    if is_never_seen(ch) {
        return (blank(LEAST_ADVANCE_EM), LEAST_ADVANCE_EM);
    }
    if ch.is_whitespace() {
        return (blank(SPACE_ADVANCE_EM), SPACE_ADVANCE_EM);
    }
    let advance = if u32::from(ch) >= FIRST_WIDE_CHARACTER { WIDE_BOX_ADVANCE_EM } else { BOX_ADVANCE_EM };
    (missing_box(advance), advance)
}

/// Controls and the format characters a renderer shows nothing for: the soft hyphen, joiners,
/// zero-width spaces, direction marks, variation selectors, the byte order mark and tags.
fn is_never_seen(ch: char) -> bool {
    ch.is_control()
        || matches!(
            u32::from(ch),
            0x00AD
                | 0x034F
                | 0x061C
                | 0x180B..=0x180F
                | 0x200B..=0x200F
                | 0x202A..=0x202E
                | 0x2060..=0x206F
                | 0xFE00..=0xFE0F
                | 0xFEFF
                | 0xFFF9..=0xFFFB
                | 0xE0000..=0xE0FFF
        )
}

/// A glyph procedure that paints nothing and advances `advance` em.
fn blank(advance: f64) -> Vec<u8> {
    format!("{:.0} 0 0 0 0 0 d1\n", advance * 1000.0).into_bytes()
}

/// A glyph procedure for an empty box `advance` em wide. The inner square runs the other way
/// round, so the non-zero fill leaves it open.
fn missing_box(advance: f64) -> Vec<u8> {
    let (left, right) = (BOX_SIDE_GAP, advance * 1000.0 - BOX_SIDE_GAP);
    let (bottom, top) = (0.0, BOX_HEIGHT);
    let (inner_left, inner_right) = (left + BOX_LINE_WIDTH, right - BOX_LINE_WIDTH);
    let (inner_bottom, inner_top) = (bottom + BOX_LINE_WIDTH, top - BOX_LINE_WIDTH);
    format!(
        "{:.0} 0 {left:.0} {bottom:.0} {right:.0} {top:.0} d1\n\
         {left:.0} {bottom:.0} m\n{right:.0} {bottom:.0} l\n{right:.0} {top:.0} l\n{left:.0} {top:.0} l\nh\n\
         {inner_left:.0} {inner_bottom:.0} m\n{inner_left:.0} {inner_top:.0} l\n{inner_right:.0} {inner_top:.0} l\n{inner_right:.0} {inner_bottom:.0} l\nh\n\
         f\n",
        advance * 1000.0
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(procedure: &[u8]) -> &str {
        std::str::from_utf8(procedure).unwrap()
    }

    #[test]
    fn white_space_and_unseen_characters_paint_nothing() {
        for (ch, advance) in [('\t', "1000"), ('\u{2003}', "250"), ('\u{200B}', "1"), ('\u{0007}', "1"), ('\u{FEFF}', "1"), ('\u{00AD}', "1")] {
            let (procedure, _) = placeholder_glyph(ch);
            assert_eq!(text(&procedure), format!("{advance} 0 0 0 0 0 d1\n"), "U+{:04X}", u32::from(ch));
        }
    }

    #[test]
    fn a_missing_character_is_an_open_box_as_wide_as_its_script_sets_it() {
        let (narrow, narrow_advance) = placeholder_glyph('\u{0540}');
        let (wide, wide_advance) = placeholder_glyph('\u{7B7E}');
        assert_eq!((narrow_advance, wide_advance), (BOX_ADVANCE_EM, WIDE_BOX_ADVANCE_EM));
        assert!(text(&narrow).starts_with("600 0 60 0 540 700 d1\n"), "{}", text(&narrow));
        assert!(text(&wide).starts_with("1000 0 60 0 940 700 d1\n"), "{}", text(&wide));
        assert_eq!(text(&narrow).matches("\nh\n").count(), 2, "an outer and an inner square");
        assert!(text(&narrow).ends_with("f\n"));
    }

    #[test]
    fn only_a_build_that_accepts_any_document_font_draws_them() {
        assert_eq!(draws_undrawable_characters(), pdfcraft_fonts::embedding::accepts_any_document_font());
        DRAWS_UNDRAWABLE.with(|draws| draws.set(true));
        assert!(draws_undrawable_characters());
        DRAWS_UNDRAWABLE.with(|draws| draws.set(false));
    }
}
