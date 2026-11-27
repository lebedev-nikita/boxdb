//! Spelling of cell values: plain text, quoted text, typed scalars and the
//! explicit mappings used by untyped columns.
use super::*;

pub(super) fn native_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Decimal => "decimal",
        Kind::Uuid => "uuid",
        Kind::Uint8 => "uint8",
        Kind::Uint16 => "uint16",
        Kind::Uint32 => "uint32",
        Kind::Uint64 => "uint64",
        Kind::Instant => "instant",
        Kind::Date => "date",
        Kind::Time => "time",
        Kind::DateTime => "datetime",
        Kind::Interval => "interval",
    }
}

/// The native type and canonical text of a tagged BLOB.
pub(super) fn native_text(bytes: &[u8]) -> Option<(Kind, std::borrow::Cow<'_, str>)> {
    if let Some(text) = crate::decimal::decode(bytes) {
        return Some((Kind::Decimal, text.into()));
    }
    libsqlite3_sys::native::decode(bytes).map(|(kind, text)| (kind, text.into()))
}

// A leading letter rules out YAML numbers, dates and special floats; the
// remaining characters cannot start a comment, flow item or mapping value.
// Other text is double-quoted or, when it spans lines, a block scalar.
pub(super) fn plain_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && bytes.len() <= 80
        && !bytes.ends_with(b" ")
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_- .@/+".contains(b))
        && !["null", "true", "false", "yes", "no", "on", "off", "y", "n"]
            .iter()
            .any(|s| text.eq_ignore_ascii_case(s))
}
/// Typed text is read as a string, so numbers and dates may stay unquoted.
/// Only structure and null spellings need quotes.
pub(super) fn bare_or_quoted(text: &str) -> String {
    let bytes = text.as_bytes();
    let bare = !bytes.is_empty()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"+-./:=_ ".contains(b))
        && !matches!(bytes[0], b' ' | b':')
        && !matches!(bytes[bytes.len() - 1], b' ' | b':')
        && !text.starts_with("- ")
        && text != "-"
        && !text.contains(": ")
        && !text.eq_ignore_ascii_case("null");
    if bare { text.into() } else { quoted(text) }
}
/// A single-line YAML double-quoted scalar. Escapes cover YAML's
/// non-printable characters and line breaks.
pub(super) fn quoted(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c if c < ' '
                || ('\u{7f}'..='\u{9f}').contains(&c)
                || matches!(
                    c,
                    '\u{2028}' | '\u{2029}' | '\u{feff}' | '\u{fffe}' | '\u{ffff}'
                ) =>
            {
                write!(out, "\\u{:04X}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
