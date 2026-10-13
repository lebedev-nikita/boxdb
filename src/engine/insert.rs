//! Recognize complete literal INSERT ... VALUES statements. Any richer syntax
//! falls back to sqlparser before changing state.
use super::*;
pub(crate) struct Insert {
    table: String,
    rows: Vec<Vec<Value>>,
}
struct Lexer<'a> {
    text: &'a str,
    position: usize,
}
impl<'a> Lexer<'a> {
    fn whitespace(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
    }
    fn token(&mut self, byte: u8) -> bool {
        self.whitespace();
        if self.text.as_bytes().get(self.position) == Some(&byte) {
            self.position += 1;
            true
        } else {
            false
        }
    }
    fn keyword(&mut self, word: &str) -> bool {
        self.whitespace();
        let end = self.position + word.len();
        if self
            .text
            .get(self.position..end)
            .is_some_and(|s| s.eq_ignore_ascii_case(word))
            && !self
                .text
                .as_bytes()
                .get(end)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            self.position = end;
            true
        } else {
            false
        }
    }
    fn identifier(&mut self) -> Option<String> {
        self.whitespace();
        let start = self.position;
        let bytes = self.text.as_bytes();
        if !bytes
            .get(start)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return None;
        }
        while bytes
            .get(self.position)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b'.'))
        {
            self.position += 1;
        }
        let name = &self.text[start..self.position];
        if name.split('.').any(|part| {
            part.is_empty()
                || !part.as_bytes()[0].is_ascii_alphabetic() && part.as_bytes()[0] != b'_'
                || sqlparser::keywords::ALL_KEYWORDS
                    .binary_search(&part.to_ascii_uppercase().as_str())
                    .is_ok()
        }) {
            return None;
        }
        Some(name.to_ascii_lowercase())
    }
    fn literal(&mut self) -> Option<Value> {
        if self.keyword("NULL") {
            return Some(Value::Null);
        }
        if self.keyword("TRUE") {
            return Some(Value::Boolean(true));
        }
        if self.keyword("FALSE") {
            return Some(Value::Boolean(false));
        }
        if self.token(b'\'') {
            let mut result = String::new();
            let mut start = self.position;
            loop {
                match self.text.as_bytes().get(self.position)? {
                    b'\\' => return None,
                    b'\'' => {
                        result.push_str(&self.text[start..self.position]);
                        self.position += 1;
                        if self.text.as_bytes().get(self.position) != Some(&b'\'') {
                            return Some(Value::Text(result));
                        }
                        result.push('\'');
                        self.position += 1;
                        start = self.position;
                    }
                    _ => self.position += 1,
                }
            }
        }
        self.whitespace();
        let start = self.position;
        let bytes = self.text.as_bytes();
        if bytes
            .get(self.position)
            .is_some_and(|b| matches!(*b, b'+' | b'-'))
        {
            self.position += 1;
        }
        let digits = self.position;
        while bytes.get(self.position).is_some_and(u8::is_ascii_digit) {
            self.position += 1;
        }
        let mut seen = self.position > digits;
        if bytes.get(self.position) == Some(&b'.') {
            self.position += 1;
            let digits = self.position;
            while bytes.get(self.position).is_some_and(u8::is_ascii_digit) {
                self.position += 1;
            }
            seen |= self.position > digits;
        }
        if !seen {
            return None;
        }
        if bytes
            .get(self.position)
            .is_some_and(|b| matches!(*b, b'e' | b'E'))
        {
            self.position += 1;
            if bytes
                .get(self.position)
                .is_some_and(|b| matches!(*b, b'+' | b'-'))
            {
                self.position += 1;
            }
            let digits = self.position;
            while bytes.get(self.position).is_some_and(u8::is_ascii_digit) {
                self.position += 1;
            }
            if digits == self.position {
                return None;
            }
        }
        let number = &self.text[start..self.position];
        Some(
            number
                .parse::<i64>()
                .map(Value::Integer)
                .unwrap_or_else(|_| Value::Numeric(number.into())),
        )
    }
}
pub(crate) fn parse(sql: &str) -> Option<Insert> {
    let mut lexer = Lexer {
        text: sql,
        position: 0,
    };
    if !lexer.keyword("INSERT") || !lexer.keyword("INTO") {
        return None;
    }
    let table = lexer.identifier()?;
    if !lexer.keyword("VALUES") {
        return None;
    }
    let mut rows = Vec::new();
    loop {
        if !lexer.token(b'(') {
            return None;
        }
        let mut row = Vec::new();
        loop {
            row.push(lexer.literal()?);
            if lexer.token(b')') {
                break;
            }
            if !lexer.token(b',') {
                return None;
            }
        }
        rows.push(row);
        if !lexer.token(b',') {
            break;
        }
    }
    lexer.token(b';');
    lexer.whitespace();
    (lexer.position == sql.len()).then_some(Insert { table, rows })
}
impl State {
    pub fn insert_literals(&mut self, insert: Insert) -> Result<QueryResult> {
        let table = self
            .tables
            .get_mut(&insert.table)
            .ok_or_else(|| Error("no such table".into()))?;
        let count = insert.rows.len();
        let (primary, mut maximum) = table.primary_sequence();
        for values in insert.rows {
            if values.len() != table.columns.len() {
                return Err(Error("column count mismatch".into()));
            }
            let mut row = table
                .columns
                .iter()
                .zip(values)
                .map(|(column, value)| column.ty.normalize(value))
                .collect::<Result<Vec<_>>>()?;
            table.normalize_primary(&mut row, primary, &mut maximum)?;
            table.rows.push(table.next_rowid, row)?;
            table.next_rowid = table
                .next_rowid
                .checked_add(1)
                .ok_or_else(|| Error("row id exhausted".into()))?;
        }
        Ok(empty(count))
    }
}
