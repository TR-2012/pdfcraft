//! What draws a character that no font at hand has, in a build that draws it instead of refusing
//! the text: the outline of a face installed on the machine ([`crate::installed`]), else a
//! placeholder (an empty box, or nothing for white space and characters that are never seen).
//!
//! The character itself stays in the text (the generated font's ToUnicode map), so it is still
//! found, copied and edited. Nothing is shaped: each character is one glyph.

use crate::{GlyphOutline, ShapedCluster, ShapedScript};

/// The advance of a tab that no font has a glyph for, in em: about four spaces.
const TAB_ADVANCE_EM: f64 = 1.0;

/// The advance of any other white space no font has, in em: about one space.
const SPACE_ADVANCE_EM: f64 = 0.25;

/// The advance of the box that stands for a missing character, in em.
const BOX_ADVANCE_EM: f64 = 0.6;

/// The same for the characters East Asian text sets a full em wide, and those after them.
const WIDE_BOX_ADVANCE_EM: f64 = 1.0;

/// The first of the wide characters (CJK Radicals Supplement).
const FIRST_WIDE_CHARACTER: u32 = 0x2E80;

/// The box in em: the gap at each side, its height and its line width.
const BOX_SIDE_GAP_EM: f64 = 0.06;
const BOX_HEIGHT_EM: f64 = 0.7;
const BOX_LINE_WIDTH_EM: f64 = 0.05;

#[cfg(test)]
thread_local! {
    /// Set by a test of this crate to draw as a build that accepts any document font does.
    pub(crate) static DRAWS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether text is drawn with this last resort instead of being refused over a character nothing
/// has a glyph for. It is the switch that lets any font into a document: the build of a person
/// who chose to answer for the fonts on their own machine. Every other build keeps the refusal,
/// which says how to get the character drawn, and keeps installed fonts out of PDFs (AGENTS.md §1.4).
pub fn draws() -> bool {
    #[cfg(test)]
    if DRAWS.with(std::cell::Cell::get) {
        return true;
    }
    crate::embedding::accepts_any_document_font()
}

/// Whether `c` is left to the last resort beside a standard font (WinAnsiEncoding): a character
/// that font has no code for. Line breaks never are; they end a line and draw nothing.
pub fn takes(c: char) -> bool {
    !matches!(c, '\n' | '\r') && !crate::win_ansi_encodable(c)
}

/// `ch` as the last resort draws it, in em: an installed face's glyph where the build reads
/// installed fonts and one has it, else [`placeholder`].
pub fn outline(ch: char) -> GlyphOutline {
    crate::installed::face_with(ch).and_then(|face| crate::installed::glyph(face, ch).ok()).unwrap_or_else(|| placeholder(ch))
}

/// What stands for `ch` when no face at all has it. White space and the characters that are
/// never seen (controls, joiners, direction marks) show nothing; anything else shows an empty
/// box, the usual sign of a missing glyph.
pub fn placeholder(ch: char) -> GlyphOutline {
    if ch == '\t' {
        return blank(TAB_ADVANCE_EM);
    }
    if is_never_seen(ch) {
        return blank(0.0);
    }
    if ch.is_whitespace() {
        return blank(SPACE_ADVANCE_EM);
    }
    missing_box(if u32::from(ch) >= FIRST_WIDE_CHARACTER { WIDE_BOX_ADVANCE_EM } else { BOX_ADVANCE_EM })
}

/// `text` as last-resort clusters in drawing order, one per character: its glyph is named by the
/// character's code point ([`ShapedScript::LastResort`]). Right-to-left text is reversed, the
/// only reordering it gets.
pub fn shape(text: &str, rtl: bool) -> Vec<ShapedCluster> {
    let cluster = |ch: char| ShapedCluster {
        script: ShapedScript::LastResort,
        glyphs: vec![(u32::from(ch), [0.0, 0.0])],
        advance: outline(ch).width,
        text: ch.to_string(),
    };
    if rtl { text.chars().rev().map(cluster).collect() } else { text.chars().map(cluster).collect() }
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

/// A glyph that paints nothing and advances `advance` em.
fn blank(advance: f64) -> GlyphOutline {
    GlyphOutline { contours: Vec::new(), width: advance, bbox: [0.0, 0.0, advance, 0.0] }
}

/// An empty box `advance` em wide. The inner square runs the other way round, so a non-zero
/// fill leaves it open.
fn missing_box(advance: f64) -> GlyphOutline {
    let (left, right) = (BOX_SIDE_GAP_EM, advance - BOX_SIDE_GAP_EM);
    let (bottom, top) = (0.0, BOX_HEIGHT_EM);
    let (inner_left, inner_right) = (left + BOX_LINE_WIDTH_EM, right - BOX_LINE_WIDTH_EM);
    let (inner_bottom, inner_top) = (bottom + BOX_LINE_WIDTH_EM, top - BOX_LINE_WIDTH_EM);
    let outer = vec![[left, bottom], [right, bottom], [right, top], [left, top]];
    let inner = vec![[inner_left, inner_bottom], [inner_left, inner_top], [inner_right, inner_top], [inner_right, inner_bottom]];
    GlyphOutline { contours: vec![outer, inner], width: advance, bbox: [left, bottom, right, top] }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twice the area a contour encloses: positive when it runs counter-clockwise.
    fn doubled_area(contour: &[[f64; 2]]) -> f64 {
        let next = contour.iter().cycle().skip(1);
        contour.iter().zip(next).map(|(a, b)| a[0] * b[1] - b[0] * a[1]).sum()
    }

    #[test]
    fn white_space_and_unseen_characters_paint_nothing() {
        for (ch, advance) in [('\t', 1.0), ('\u{2003}', 0.25), ('\u{200B}', 0.0), ('\u{0007}', 0.0), ('\u{FEFF}', 0.0), ('\u{00AD}', 0.0)] {
            let glyph = placeholder(ch);
            assert!(glyph.contours.is_empty(), "U+{:04X}", u32::from(ch));
            assert_eq!(glyph.width, advance, "U+{:04X}", u32::from(ch));
        }
    }

    #[test]
    fn a_missing_character_is_an_open_box_as_wide_as_its_script_sets_it() {
        let (narrow, wide) = (placeholder('\u{0378}'), placeholder('\u{2FE0}'));
        assert_eq!((narrow.width, wide.width), (BOX_ADVANCE_EM, WIDE_BOX_ADVANCE_EM));
        assert_eq!(narrow.contours.len(), 2, "an outer and an inner square");
        assert!(doubled_area(&narrow.contours[0]) > 0.0 && doubled_area(&narrow.contours[1]) < 0.0, "they run opposite ways, so the box is open");
        assert!(narrow.bbox[0] >= 0.0 && narrow.bbox[2] <= narrow.width, "the box stays inside its advance");
    }

    #[test]
    fn text_is_one_cluster_a_character_named_by_its_code_point() {
        let clusters = shape("a\u{0378}", false);
        let named: Vec<(u32, &str)> = clusters.iter().map(|c| (c.glyphs[0].0, c.text.as_str())).collect();
        assert_eq!(named, [(0x61, "a"), (0x378, "\u{0378}")]);
        assert!(clusters.iter().all(|c| c.script == ShapedScript::LastResort));
        let reversed: String = shape("ab", true).iter().map(|c| c.text.as_str()).collect();
        assert_eq!(reversed, "ba", "right-to-left text comes in drawing order");
    }

    #[test]
    fn only_line_breaks_and_what_a_standard_font_encodes_are_left_out() {
        assert!(takes('\u{0540}') && takes('\u{7B7E}') && takes('\u{200B}'));
        assert!(!takes('A') && !takes('é') && !takes('\n') && !takes('\r'));
    }

    #[test]
    fn only_a_build_that_accepts_any_document_font_draws_with_it() {
        assert_eq!(draws(), crate::embedding::accepts_any_document_font());
        DRAWS.with(|draws| draws.set(true));
        assert!(draws());
        DRAWS.with(|draws| draws.set(false));
    }
}
