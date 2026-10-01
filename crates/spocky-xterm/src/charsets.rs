//! G0 to G3 character sets, including DEC special graphics.
//!
//! Ported from xterm.js 6.0.0 `src/common/data/Charsets.ts`.
//! Copyright (c) 2016 The xterm.js authors. MIT License.

/// A national replacement or DEC special graphics character set. US ASCII
/// (`B`, the default) is no charset at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Charset {
    DecSpecialGraphics,
    British,
    Dutch,
    Finnish,
    French,
    FrenchCanadian,
    German,
    Italian,
    NorwegianDanish,
    Spanish,
    Swedish,
    Swiss,
}

const DEC_SPECIAL_GRAPHICS: &[(u8, char)] = &[
    (b'`', '\u{25c6}'),
    (b'a', '\u{2592}'),
    (b'b', '\u{2409}'),
    (b'c', '\u{240c}'),
    (b'd', '\u{240d}'),
    (b'e', '\u{240a}'),
    (b'f', '\u{00b0}'),
    (b'g', '\u{00b1}'),
    (b'h', '\u{2424}'),
    (b'i', '\u{240b}'),
    (b'j', '\u{2518}'),
    (b'k', '\u{2510}'),
    (b'l', '\u{250c}'),
    (b'm', '\u{2514}'),
    (b'n', '\u{253c}'),
    (b'o', '\u{23ba}'),
    (b'p', '\u{23bb}'),
    (b'q', '\u{2500}'),
    (b'r', '\u{23bc}'),
    (b's', '\u{23bd}'),
    (b't', '\u{251c}'),
    (b'u', '\u{2524}'),
    (b'v', '\u{2534}'),
    (b'w', '\u{252c}'),
    (b'x', '\u{2502}'),
    (b'y', '\u{2264}'),
    (b'z', '\u{2265}'),
    (b'{', '\u{03c0}'),
    (b'|', '\u{2260}'),
    (b'}', '\u{00a3}'),
    (b'~', '\u{00b7}'),
];
const BRITISH: &[(u8, char)] = &[(b'#', '£')];
// `[` maps to the two-character string "ij"; `print` keeps its first unit.
const DUTCH: &[(u8, char)] = &[
    (b'#', '£'),
    (b'@', '¾'),
    (b'[', 'i'),
    (b'\\', '½'),
    (b']', '|'),
    (b'{', '¨'),
    (b'|', 'f'),
    (b'}', '¼'),
    (b'~', '´'),
];
const FINNISH: &[(u8, char)] = &[
    (b'[', 'Ä'),
    (b'\\', 'Ö'),
    (b']', 'Å'),
    (b'^', 'Ü'),
    (b'`', 'é'),
    (b'{', 'ä'),
    (b'|', 'ö'),
    (b'}', 'å'),
    (b'~', 'ü'),
];
const FRENCH: &[(u8, char)] = &[
    (b'#', '£'),
    (b'@', 'à'),
    (b'[', '°'),
    (b'\\', 'ç'),
    (b']', '§'),
    (b'{', 'é'),
    (b'|', 'ù'),
    (b'}', 'è'),
    (b'~', '¨'),
];
const FRENCH_CANADIAN: &[(u8, char)] = &[
    (b'@', 'à'),
    (b'[', 'â'),
    (b'\\', 'ç'),
    (b']', 'ê'),
    (b'^', 'î'),
    (b'`', 'ô'),
    (b'{', 'é'),
    (b'|', 'ù'),
    (b'}', 'è'),
    (b'~', 'û'),
];
const GERMAN: &[(u8, char)] = &[
    (b'@', '§'),
    (b'[', 'Ä'),
    (b'\\', 'Ö'),
    (b']', 'Ü'),
    (b'{', 'ä'),
    (b'|', 'ö'),
    (b'}', 'ü'),
    (b'~', 'ß'),
];
const ITALIAN: &[(u8, char)] = &[
    (b'#', '£'),
    (b'@', '§'),
    (b'[', '°'),
    (b'\\', 'ç'),
    (b']', 'é'),
    (b'`', 'ù'),
    (b'{', 'à'),
    (b'|', 'ò'),
    (b'}', 'è'),
    (b'~', 'ì'),
];
const NORWEGIAN_DANISH: &[(u8, char)] = &[
    (b'@', 'Ä'),
    (b'[', 'Æ'),
    (b'\\', 'Ø'),
    (b']', 'Å'),
    (b'^', 'Ü'),
    (b'`', 'ä'),
    (b'{', 'æ'),
    (b'|', 'ø'),
    (b'}', 'å'),
    (b'~', 'ü'),
];
const SPANISH: &[(u8, char)] = &[
    (b'#', '£'),
    (b'@', '§'),
    (b'[', '¡'),
    (b'\\', 'Ñ'),
    (b']', '¿'),
    (b'{', '°'),
    (b'|', 'ñ'),
    (b'}', 'ç'),
];
const SWEDISH: &[(u8, char)] = &[
    (b'@', 'É'),
    (b'[', 'Ä'),
    (b'\\', 'Ö'),
    (b']', 'Å'),
    (b'^', 'Ü'),
    (b'`', 'é'),
    (b'{', 'ä'),
    (b'|', 'ö'),
    (b'}', 'å'),
    (b'~', 'ü'),
];
const SWISS: &[(u8, char)] = &[
    (b'#', 'ù'),
    (b'@', 'à'),
    (b'[', 'é'),
    (b'\\', 'ç'),
    (b']', 'ê'),
    (b'^', 'î'),
    (b'_', 'è'),
    (b'`', 'ô'),
    (b'{', 'ä'),
    (b'|', 'ö'),
    (b'}', 'ü'),
    (b'~', 'û'),
];

impl Charset {
    /// Whether `flag` is a key of `CHARSETS`, so `ESC ( flag` and its
    /// siblings have a handler.
    pub(crate) fn is_flag(flag: u8) -> bool {
        flag == b'B' || Self::from_flag(flag).is_some()
    }

    /// The `CHARSETS` entry for a designation final byte; US ASCII (`B`)
    /// and bytes without an entry are no charset.
    pub(crate) fn from_flag(flag: u8) -> Option<Self> {
        Some(match flag {
            b'0' => Self::DecSpecialGraphics,
            b'A' => Self::British,
            b'4' => Self::Dutch,
            b'C' | b'5' => Self::Finnish,
            b'R' => Self::French,
            b'Q' => Self::FrenchCanadian,
            b'K' => Self::German,
            b'Y' => Self::Italian,
            b'E' | b'6' => Self::NorwegianDanish,
            b'Z' => Self::Spanish,
            b'H' | b'7' => Self::Swedish,
            b'=' => Self::Swiss,
            _ => return None,
        })
    }

    /// The replacement code point for an ASCII code, if this set maps it.
    pub(crate) fn map(self, code: u32) -> Option<u32> {
        let table = match self {
            Self::DecSpecialGraphics => DEC_SPECIAL_GRAPHICS,
            Self::British => BRITISH,
            Self::Dutch => DUTCH,
            Self::Finnish => FINNISH,
            Self::French => FRENCH,
            Self::FrenchCanadian => FRENCH_CANADIAN,
            Self::German => GERMAN,
            Self::Italian => ITALIAN,
            Self::NorwegianDanish => NORWEGIAN_DANISH,
            Self::Spanish => SPANISH,
            Self::Swedish => SWEDISH,
            Self::Swiss => SWISS,
        };
        table
            .iter()
            .find(|&&(from, _)| u32::from(from) == code)
            .map(|&(_, to)| u32::from(to))
    }
}

#[cfg(test)]
mod tests {
    use super::Charset;

    #[test]
    fn maps_dec_line_drawing_and_national_sets() {
        let dec = Charset::from_flag(b'0').expect("dec");
        assert_eq!(dec.map(u32::from('q')), Some(0x2500));
        assert_eq!(dec.map(u32::from('A')), None);
        let dutch = Charset::from_flag(b'4').expect("dutch");
        assert_eq!(dutch.map(u32::from('[')), Some(u32::from('i')));
        assert_eq!(Charset::from_flag(b'B'), None);
        assert!(Charset::is_flag(b'B'));
        assert!(!Charset::is_flag(b'X'));
    }
}
