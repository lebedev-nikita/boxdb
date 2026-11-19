//! Spelling of cell values: plain text, quoted text, typed scalars and the
//! explicit mappings used by untyped columns.
use super::*;

/// Typed text is read verbatim, so `12.50` is not rounded through a float.
pub(super) struct NativeSeed(pub(super) Kind);
impl<'de> DeserializeSeed<'de> for NativeSeed {
    type Value = SqlValue;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<SqlValue, D::Error> {
        let Some(text) = Option::<String>::deserialize(deserializer)? else {
            return Ok(SqlValue::Null);
        };
        libsqlite3_sys::native::encode(self.0, &text)
            .map(SqlValue::Blob)
            .map_err(|error| de::Error::custom(format!("{}: {text}", error.0)))
    }
}

pub(super) struct ByteaSeed;
impl<'de> DeserializeSeed<'de> for ByteaSeed {
    type Value = SqlValue;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<SqlValue, D::Error> {
        let Some(text) = Option::<String>::deserialize(deserializer)? else {
            return Ok(SqlValue::Null);
        };
        BASE64
            .decode(text)
            .map(|bytes| SqlValue::Blob(crate::decimal::encode_blob(&bytes)))
            .map_err(de::Error::custom)
    }
}

/// Untyped SQLite cells have natural YAML scalars. BLOBs and native values use
/// explicit mappings so text, byte arrays and numbers remain distinguishable.
pub(super) struct Cell(pub(super) SqlValue);
impl<'de> Deserialize<'de> for Cell {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct CellVisitor;
        impl<'de> Visitor<'de> for CellVisitor {
            type Value = Cell;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a SQLite scalar or a {blob: base64} mapping")
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Null))
            }
            fn visit_none<E: de::Error>(self) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Null))
            }
            fn visit_i64<E: de::Error>(self, n: i64) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Integer(n)))
            }
            fn visit_u64<E: de::Error>(self, n: u64) -> std::result::Result<Cell, E> {
                i64::try_from(n)
                    .map(|n| Cell(SqlValue::Integer(n)))
                    .map_err(E::custom)
            }
            fn visit_f64<E: de::Error>(self, n: f64) -> std::result::Result<Cell, E> {
                if n.is_finite() {
                    Ok(Cell(SqlValue::Real(n)))
                } else {
                    Err(E::custom("non-finite SQLite value"))
                }
            }
            fn visit_bool<E: de::Error>(self, b: bool) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Integer(i64::from(b))))
            }
            fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Text(s.into())))
            }
            fn visit_string<E: de::Error>(self, s: String) -> std::result::Result<Cell, E> {
                Ok(Cell(SqlValue::Text(s)))
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Cell, M::Error> {
                let Some(name) = map.next_key::<String>()? else {
                    return Err(de::Error::invalid_length(0, &self));
                };
                let text = map.next_value::<String>()?;
                if map.next_key::<IgnoredAny>()?.is_some() {
                    return Err(de::Error::invalid_length(2, &self));
                }
                let kind = match name.as_str() {
                    "blob" => {
                        return BASE64
                            .decode(text)
                            .map(|bytes| Cell(SqlValue::Blob(crate::decimal::encode_blob(&bytes))))
                            .map_err(de::Error::custom);
                    }
                    "decimal" => Kind::Decimal,
                    "uuid" => Kind::Uuid,
                    "uint8" => Kind::Uint8,
                    "uint16" => Kind::Uint16,
                    "uint32" => Kind::Uint32,
                    "uint64" => Kind::Uint64,
                    "instant" => Kind::Instant,
                    "date" => Kind::Date,
                    "time" => Kind::Time,
                    "datetime" => Kind::DateTime,
                    "interval" => Kind::Interval,
                    _ => return Err(de::Error::unknown_field(&name, NATIVE_NAMES)),
                };
                libsqlite3_sys::native::encode(kind, &text)
                    .map(|b| Cell(SqlValue::Blob(b)))
                    .map_err(|e| de::Error::custom(e.0))
            }
        }
        deserializer.deserialize_any(CellVisitor)
    }
}

pub(super) const NATIVE_NAMES: &[&str] = &[
    "blob", "decimal", "uuid", "uint8", "uint16", "uint32", "uint64", "instant", "date", "time",
    "datetime", "interval",
];

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
