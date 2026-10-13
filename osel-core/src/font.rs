//! OSel Console: two native bitmap sizes, derived from Terminus Font.
//! The font data is OFL-1.1; see ../fonts/OFL.txt for attribution and terms.

include!("../fonts/console.rs");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    Small,
    Large,
}

impl Font {
    pub const fn width(self) -> usize {
        match self {
            Self::Small => 8,
            Self::Large => 16,
        }
    }

    pub const fn height(self) -> usize {
        match self {
            Self::Small => 16,
            Self::Large => 32,
        }
    }

    /// Prefer the native large raster only if an 80x25 terminal fits.
    pub const fn for_screen(width: usize, height: usize) -> Self {
        if width >= 80 * Self::Large.width() && height >= 25 * Self::Large.height() {
            Self::Large
        } else {
            Self::Small
        }
    }

    pub fn glyph(self, c: char) -> &'static [u16] {
        let index = match c {
            ' '..='~' => c as usize - ' ' as usize,
            'Ж' => 95,
            _ => 96,
        };
        match self {
            Self::Small => &SMALL[index],
            Self::Large => &LARGE[index],
        }
    }

    pub fn pixel(self, row: u16, x: usize) -> bool {
        x < self.width() && row & (1 << (self.width() - 1 - x)) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_selection_keeps_eighty_by_twenty_five_cells() {
        assert_eq!(Font::for_screen(1280, 800), Font::Large);
        assert_eq!(Font::for_screen(1280, 1024), Font::Large);
        assert_eq!(Font::for_screen(1279, 800), Font::Small);
        assert_eq!(Font::for_screen(1280, 799), Font::Small);
        assert_eq!(Font::for_screen(640, 480), Font::Small);
    }

    #[test]
    fn glyphs_fit_and_unknown_characters_have_a_visible_fallback() {
        for font in [Font::Small, Font::Large] {
            assert!(font.glyph(' ').iter().all(|&row| row == 0));
            for c in ('!'..='~').chain(['Ж', '\u{fffd}']) {
                let rows = font.glyph(c);
                assert_eq!(rows.len(), font.height());
                assert!(rows.iter().any(|&row| row != 0), "{c}");
                assert!(rows.iter().all(|&row| (row as u32) < (1 << font.width())));
            }
            assert_eq!(font.glyph('😀'), font.glyph('\u{fffd}'));
            assert_ne!(font.glyph('Ж'), font.glyph('\u{fffd}'));
        }
    }

    #[test]
    fn large_font_is_not_a_doubled_small_raster() {
        let small = Font::Small.glyph('A');
        let large = Font::Large.glyph('A');
        assert!((0..32).any(|y| {
            (0..16)
                .any(|x| Font::Large.pixel(large[y], x) != Font::Small.pixel(small[y / 2], x / 2))
        }));
        assert_eq!(large[6], 0x0ff0);
        assert_eq!(large[16], 0x3ffc);
    }
}
