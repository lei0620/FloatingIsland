//! Everything related to text, fonts, text layout, cursors etc.

pub mod cursor;
mod font;
#[cfg(windows)]
#[allow(unsafe_code)]
mod directwrite;
mod fonts;
mod text_layout;
mod text_layout_types;

/// One `\t` character is this many spaces wide.
pub const TAB_SIZE: usize = 4;

pub use {
    fonts::{
        FontData, FontDefinitions, FontFamily, FontId, FontInsert, FontPriority, FontTweak, Fonts,
        FontsImpl, InsertFontFamily,
    },
    text_layout::layout,
    text_layout_types::*,
};

/// Suggested character to use to replace those in password text fields.
pub const PASSWORD_REPLACEMENT_CHAR: char = '•';

/// Successful native glyph rasters and fallback attempts, for opt-in diagnostics.
#[cfg(windows)]
pub fn directwrite_rasterization_stats()->(u64,u64){directwrite::stats()}
