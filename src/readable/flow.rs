//! One-line YAML flow nodes, the subset readable logs use for their entries.
//! Each entry is a block sequence item at column zero, followed by a flow
//! mapping or sequence. Scalars keep their source text, so typed columns read
//! `12.50` verbatim; untyped columns resolve plain scalars with YAML 1.2 core
//! schema rules.
use std::borrow::Cow;

#[derive(Debug)]
pub(super) enum Node<'a> {
    Scalar(Scalar<'a>),
    Map(Vec<(Scalar<'a>, Node<'a>)>),
    Seq(Vec<Node<'a>>),
}

#[derive(Debug)]
pub(super) struct Scalar<'a> {
    pub(super) text: Cow<'a, str>,
    /// Plain scalars resolve to null, booleans and numbers; quoted ones are text.
    pub(super) plain: bool,
}

/// A plain scalar resolved with the YAML 1.2 core schema.
#[derive(Debug, PartialEq)]
pub(super) enum Resolved<'s> {
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    Text(&'s str),
}

impl Scalar<'_> {
    pub(super) fn is_null(&self) -> bool {
        self.plain && matches!(self.text.as_ref(), "" | "~" | "null" | "Null" | "NULL")
    }
    pub(super) fn resolve(&self) -> Result<Resolved<'_>, String> {
        let text = self.text.as_ref();
        if !self.plain {
            return Ok(Resolved::Text(text));
        }
        Ok(match text {
            "" | "~" | "null" | "Null" | "NULL" => Resolved::Null,
            "true" | "True" | "TRUE" => Resolved::Bool(true),
            "false" | "False" | "FALSE" => Resolved::Bool(false),
            _ if integer(text) => Resolved::Integer(
                text.parse()
                    .map_err(|_| format!("integer out of range: {text}"))?,
            ),
            _ if text.starts_with("0x") && hex(&text[2..], 16) => Resolved::Integer(
                i64::from_str_radix(&text[2..], 16)
                    .map_err(|_| format!("integer out of range: {text}"))?,
            ),
            _ if text.starts_with("0o") && hex(&text[2..], 8) => Resolved::Integer(
                i64::from_str_radix(&text[2..], 8)
                    .map_err(|_| format!("integer out of range: {text}"))?,
            ),
            _ if float(text) => {
                let value: f64 = text
                    .parse()
                    .map_err(|_| format!("invalid number: {text}"))?;
                if !value.is_finite() {
                    return Err(format!("non-finite number: {text}"));
                }
                Resolved::Real(value)
            }
            _ if special_float(text) => return Err(format!("non-finite number: {text}")),
            _ => Resolved::Text(text),
        })
    }
}

fn integer(text: &str) -> bool {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}
fn hex(digits: &str, radix: u32) -> bool {
    !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix))
}
/// `[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?`
fn float(text: &str) -> bool {
    let bytes = text.strip_prefix(['-', '+']).unwrap_or(text).as_bytes();
    let mut i = 0;
    let whole = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    i += whole;
    let mut fraction = 0;
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        fraction = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        i += fraction;
    }
    if whole == 0 && fraction == 0 {
        return false;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'-' | b'+')) {
            i += 1;
        }
        let exponent = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        if exponent == 0 {
            return false;
        }
        i += exponent;
    }
    i == bytes.len()
}
fn special_float(text: &str) -> bool {
    let text = text.strip_prefix(['-', '+']).unwrap_or(text);
    matches!(text, ".inf" | ".Inf" | ".INF" | ".nan" | ".NaN" | ".NAN")
}

/// A syntax error at a byte column of the line.
#[derive(Debug)]
pub(super) struct SyntaxError {
    pub(super) column: usize,
    pub(super) message: String,
}

/// Parse one line. Blank and comment lines have no entry.
pub(super) fn parse_line(line: &str) -> Result<Option<Node<'_>>, SyntaxError> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(None);
    }
    let mut parser = Parser { text: line, at: 0 };
    if !line.starts_with('-') || !line[1..].starts_with([' ', '\t']) {
        return Err(parser.error("entries start with \"- \" at column one"));
    }
    parser.at = 1;
    parser.space();
    let node = match parser.peek() {
        Some(b'{') | Some(b'[') => parser.node()?,
        _ => return Err(parser.error("entries are flow mappings or sequences")),
    };
    parser.space();
    match parser.peek() {
        None => Ok(Some(node)),
        Some(b'#') if parser.at > 0 && line.as_bytes()[parser.at - 1].is_ascii_whitespace() => {
            Ok(Some(node))
        }
        _ => Err(parser.error("unexpected text after the entry")),
    }
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, message: &str) -> SyntaxError {
        SyntaxError {
            column: self.at + 1,
            message: message.into(),
        }
    }
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }
    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.at += 1;
        }
    }
    fn expect(&mut self, byte: u8) -> Result<(), SyntaxError> {
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected '{}'", byte as char)))
        }
    }
    fn node(&mut self) -> Result<Node<'a>, SyntaxError> {
        match self.peek() {
            Some(b'{') => self.map(),
            Some(b'[') => self.seq(),
            _ => self.scalar(false).map(Node::Scalar),
        }
    }
    fn map(&mut self) -> Result<Node<'a>, SyntaxError> {
        self.expect(b'{')?;
        let mut entries = vec![];
        loop {
            self.space();
            if self.peek() == Some(b'}') {
                self.at += 1;
                return Ok(Node::Map(entries));
            }
            let key = self.scalar(true)?;
            self.space();
            self.expect(b':')?;
            self.space();
            let value = match self.peek() {
                Some(b',' | b'}') => Node::Scalar(Scalar {
                    text: Cow::Borrowed(""),
                    plain: true,
                }),
                _ => self.node()?,
            };
            entries.push((key, value));
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {}
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }
    fn seq(&mut self) -> Result<Node<'a>, SyntaxError> {
        self.expect(b'[')?;
        let mut items = vec![];
        loop {
            self.space();
            if self.peek() == Some(b']') {
                self.at += 1;
                return Ok(Node::Seq(items));
            }
            items.push(self.node()?);
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {}
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }
    fn scalar(&mut self, key: bool) -> Result<Scalar<'a>, SyntaxError> {
        match self.peek() {
            Some(b'"') => self.double_quoted(),
            Some(b'\'') => self.single_quoted(),
            Some(b',' | b'[' | b']' | b'{' | b'}' | b'#' | b'&' | b'*' | b'!' | b'|' | b'>')
            | Some(b'%' | b'@' | b'`') => Err(self.error("unsupported YAML syntax")),
            Some(b'-' | b'?' | b':')
                if self
                    .text
                    .as_bytes()
                    .get(self.at + 1)
                    .is_none_or(|b| b" \t,[]{}".contains(b)) =>
            {
                Err(self.error("unsupported YAML syntax"))
            }
            None => Err(self.error("unexpected end of line")),
            _ => Ok(self.plain(key)),
        }
    }
    /// Plain scalars end before `: `, ` #` and flow indicators.
    fn plain(&mut self, key: bool) -> Scalar<'a> {
        let bytes = self.text.as_bytes();
        let start = self.at;
        let mut end = start;
        let mut i = start;
        while let Some(&b) = bytes.get(i) {
            match b {
                b',' | b'[' | b']' | b'{' | b'}' => break,
                b':' if bytes.get(i + 1).is_none_or(|b| b" \t,[]{}".contains(b)) => {
                    break;
                }
                b'#' if i > start && bytes[i - 1].is_ascii_whitespace() => break,
                b' ' | b'\t' => {}
                _ => end = i + 1,
            }
            i += 1;
        }
        let _ = key;
        self.at = end;
        Scalar {
            text: Cow::Borrowed(&self.text[start..end]),
            plain: true,
        }
    }
    fn single_quoted(&mut self) -> Result<Scalar<'a>, SyntaxError> {
        self.at += 1;
        let start = self.at;
        let mut owned: Option<String> = None;
        loop {
            let rest = &self.text[self.at..];
            let Some(offset) = rest.find('\'') else {
                return Err(self.error("unterminated quoted scalar"));
            };
            if rest[offset + 1..].starts_with('\'') {
                let text = owned.get_or_insert_with(String::new);
                text.push_str(&rest[..=offset]);
                self.at += offset + 2;
                continue;
            }
            let text = match owned {
                Some(mut text) => {
                    text.push_str(&rest[..offset]);
                    Cow::Owned(text)
                }
                None => Cow::Borrowed(&self.text[start..self.at + offset]),
            };
            self.at += offset + 1;
            return Ok(Scalar { text, plain: false });
        }
    }
    fn double_quoted(&mut self) -> Result<Scalar<'a>, SyntaxError> {
        self.at += 1;
        let start = self.at;
        let bytes = self.text.as_bytes();
        let mut i = start;
        while let Some(&b) = bytes.get(i) {
            match b {
                b'"' => {
                    self.at = i + 1;
                    return Ok(Scalar {
                        text: Cow::Borrowed(&self.text[start..i]),
                        plain: false,
                    });
                }
                b'\\' => break,
                _ => i += 1,
            }
        }
        let mut text = String::from(&self.text[start..i]);
        self.at = i;
        loop {
            let Some(c) = self.text[self.at..].chars().next() else {
                return Err(self.error("unterminated quoted scalar"));
            };
            self.at += c.len_utf8();
            match c {
                '"' => {
                    return Ok(Scalar {
                        text: Cow::Owned(text),
                        plain: false,
                    });
                }
                '\\' => {
                    let Some(escape) = self.text[self.at..].chars().next() else {
                        return Err(self.error("unterminated escape"));
                    };
                    self.at += escape.len_utf8();
                    let decoded = match escape {
                        '0' => '\0',
                        'a' => '\u{7}',
                        'b' => '\u{8}',
                        't' | '\t' => '\t',
                        'n' => '\n',
                        'v' => '\u{b}',
                        'f' => '\u{c}',
                        'r' => '\r',
                        'e' => '\u{1b}',
                        ' ' => ' ',
                        '"' => '"',
                        '/' => '/',
                        '\\' => '\\',
                        'N' => '\u{85}',
                        '_' => '\u{a0}',
                        'L' => '\u{2028}',
                        'P' => '\u{2029}',
                        'x' => self.code_point(2)?,
                        'u' => self.code_point(4)?,
                        'U' => self.code_point(8)?,
                        _ => return Err(self.error("unknown escape")),
                    };
                    text.push(decoded);
                }
                c => text.push(c),
            }
        }
    }
    fn code_point(&mut self, digits: usize) -> Result<char, SyntaxError> {
        let hex = self
            .text
            .get(self.at..self.at + digits)
            .ok_or_else(|| self.error("short escape"))?;
        let value = u32::from_str_radix(hex, 16).map_err(|_| self.error("invalid escape"))?;
        self.at += digits;
        char::from_u32(value).ok_or_else(|| self.error("invalid code point"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(node: &Node) -> String {
        match node {
            Node::Scalar(s) if s.plain => format!("<{}>", s.text),
            Node::Scalar(s) => format!("'{}'", s.text),
            Node::Map(entries) => format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(k, v)| format!("{}={}", k.text, text(v)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Node::Seq(items) => {
                format!("[{}]", items.iter().map(text).collect::<Vec<_>>().join(","))
            }
        }
    }

    #[test]
    fn parses_flow_entries_with_plain_and_quoted_scalars() {
        let node = parse_line(
            r#"- {insert: notes, rows: [{id: 1, at: 2024-02-29T12:00:00Z, body: "a\n\"b\"", n: 'it''s', e: , x: a b}]}  # c"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            text(&node),
            "{insert=<notes>,rows=[{id=<1>,at=<2024-02-29T12:00:00Z>,body='a\n\"b\"',n='it's',e=<>,x=<a b>}]}"
        );
        assert!(parse_line("# comment").unwrap().is_none());
        assert!(parse_line("   ").unwrap().is_none());
        for invalid in [
            "{a: 1}",
            "- a: 1",
            "- {a: [1}",
            "- {a: &x 1}",
            "- {a: !tag 1}",
            "- {a: \"open}",
            "- {a: 1} trailing",
            "- {a: |}",
        ] {
            assert!(parse_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn plain_scalars_resolve_with_the_core_schema() {
        let resolve = |text: &str| {
            Scalar {
                text: Cow::Borrowed(text),
                plain: true,
            }
            .resolve()
            .map(|r| format!("{r:?}"))
        };
        assert_eq!(resolve("~").unwrap(), "Null");
        assert_eq!(resolve("True").unwrap(), "Bool(true)");
        assert_eq!(resolve("-12").unwrap(), "Integer(-12)");
        assert_eq!(resolve("0x1F").unwrap(), "Integer(31)");
        assert_eq!(resolve("1e3").unwrap(), "Real(1000.0)");
        assert_eq!(resolve(".5").unwrap(), "Real(0.5)");
        assert_eq!(resolve("yes").unwrap(), "Text(\"yes\")");
        assert_eq!(resolve("1.2.3").unwrap(), "Text(\"1.2.3\")");
        assert!(resolve("9223372036854775808").is_err());
        assert!(resolve(".inf").is_err());
        assert!(resolve("1e999").is_err());
    }
}
