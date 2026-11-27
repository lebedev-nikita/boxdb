//! Log entries: the steps of one transaction and their YAML text.
//!
//! ```yaml
//! - {boxdb: 5}
//! - {schema: ["CREATE TABLE notes (id INTEGER PRIMARY KEY,body TEXT) STRICT"]}
//! - {insert: notes, rows: [{id: 1, body: first}]}
//! - [{update: notes, key: {id: 1}, set: {body: second}, was: {body: first}}, {delete: notes, rows: [{id: 2, body: old}]}]
//! - {pragma: {user_version: 2}, was: {user_version: 1}}
//! - {sequences: {notes: 7}}
//! ```
use super::*;
use std::borrow::Cow;

/// Mapping entries of a row in the log, decoded through the table layout.
pub(super) type Fields<'a> = Vec<(Scalar<'a>, Node<'a>)>;

pub(super) enum Step<'a> {
    Header(i64),
    Schema(Vec<Cow<'a, str>>),
    Insert {
        table: Cow<'a, str>,
        rows: Vec<Fields<'a>>,
    },
    Delete {
        table: Cow<'a, str>,
        rows: Vec<Fields<'a>>,
    },
    Update {
        table: Cow<'a, str>,
        key: Fields<'a>,
        set: Fields<'a>,
        was: Fields<'a>,
    },
    Pragma {
        set: Vec<(String, i64)>,
        was: Option<Vec<(String, i64)>>,
    },
    Sequences(Vec<(Cow<'a, str>, Option<i64>)>),
}

impl Step<'_> {
    /// Row changes form a segment that is applied as a whole.
    pub(super) fn table(&self) -> Option<&str> {
        match self {
            Step::Insert { table, .. }
            | Step::Delete { table, .. }
            | Step::Update { table, .. } => Some(table),
            _ => None,
        }
    }
}

/// The steps of one entry: a mapping or a sequence of mappings.
pub(super) fn steps(node: Node<'_>) -> std::result::Result<Vec<Step<'_>>, String> {
    match node {
        Node::Map(entries) => Ok(vec![step(entries)?]),
        Node::Seq(items) => items
            .into_iter()
            .map(|item| match item {
                Node::Map(entries) => step(entries),
                _ => Err("transaction steps are mappings".into()),
            })
            .collect(),
        Node::Scalar(_) => Err("entries are mappings or sequences".into()),
    }
}

const KINDS: &[&str] = &[
    "boxdb",
    "schema",
    "insert",
    "update",
    "delete",
    "pragma",
    "sequences",
];

fn step<'a>(entries: Vec<(Scalar<'a>, Node<'a>)>) -> std::result::Result<Step<'a>, String> {
    let mut fields: Vec<(Cow<'a, str>, Node<'a>)> = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if fields.iter().any(|(name, _)| *name == key.text) {
            return Err(format!("duplicate key: {}", key.text));
        }
        fields.push((key.text, value));
    }
    let kinds: Vec<&str> = fields
        .iter()
        .map(|(name, _)| name.as_ref())
        .filter(|name| KINDS.contains(name))
        .collect();
    let [kind] = kinds.as_slice() else {
        return Err(format!("a step has one of the keys {}", KINDS.join(", ")));
    };
    let kind = (*kind).to_owned();
    let allowed: &[&str] = match kind.as_str() {
        "insert" | "delete" => &["rows"],
        "update" => &["key", "set", "was"],
        "pragma" => &["was"],
        _ => &[],
    };
    if let Some((name, _)) = fields
        .iter()
        .find(|(name, _)| *name != kind && !allowed.contains(&name.as_ref()))
    {
        return Err(format!("unknown key in {kind} step: {name}"));
    }
    let mut take = |name: &str| {
        fields
            .iter()
            .position(|(field, _)| field == name)
            .map(|i| fields.swap_remove(i).1)
    };
    let main = take(&kind).unwrap();
    Ok(match kind.as_str() {
        "boxdb" => Step::Header(integer(&main)?),
        "schema" => Step::Schema(
            seq(main)?
                .into_iter()
                .map(text)
                .collect::<std::result::Result<_, _>>()?,
        ),
        "insert" | "delete" => {
            let table = text(main)?;
            let rows = seq(take("rows").ok_or("missing rows")?)?
                .into_iter()
                .map(map)
                .collect::<std::result::Result<_, _>>()?;
            if kind == "insert" {
                Step::Insert { table, rows }
            } else {
                Step::Delete { table, rows }
            }
        }
        "update" => Step::Update {
            table: text(main)?,
            key: map(take("key").ok_or("missing key")?)?,
            set: map(take("set").ok_or("missing set")?)?,
            was: map(take("was").ok_or("missing was")?)?,
        },
        "pragma" => {
            let header = |node| {
                map(node)?
                    .into_iter()
                    .map(|(name, value)| match name.text.as_ref() {
                        "user_version" | "application_id" => {
                            Ok((name.text.into_owned(), integer(&value)?))
                        }
                        other => Err(format!("unsupported pragma: {other}")),
                    })
                    .collect::<std::result::Result<Vec<_>, String>>()
            };
            Step::Pragma {
                set: header(main)?,
                was: take("was").map(header).transpose()?,
            }
        }
        "sequences" => Step::Sequences(
            map(main)?
                .into_iter()
                .map(|(name, value)| {
                    let value = match &value {
                        Node::Scalar(s) if s.is_null() => None,
                        _ => Some(integer(&value)?),
                    };
                    Ok((name.text, value))
                })
                .collect::<std::result::Result<_, String>>()?,
        ),
        _ => unreachable!(),
    })
}

fn text(node: Node<'_>) -> std::result::Result<Cow<'_, str>, String> {
    match node {
        Node::Scalar(scalar) => Ok(scalar.text),
        _ => Err("expected text".into()),
    }
}
fn integer(node: &Node<'_>) -> std::result::Result<i64, String> {
    match node {
        Node::Scalar(scalar) => match scalar.resolve()? {
            Resolved::Integer(n) => Ok(n),
            _ => Err(format!("expected an integer: {}", scalar.text)),
        },
        _ => Err("expected an integer".into()),
    }
}
fn seq(node: Node<'_>) -> std::result::Result<Vec<Node<'_>>, String> {
    match node {
        Node::Seq(items) => Ok(items),
        _ => Err("expected a sequence".into()),
    }
}
fn map(node: Node<'_>) -> std::result::Result<Fields<'_>, String> {
    match node {
        Node::Map(entries) => Ok(entries),
        _ => Err("expected a mapping".into()),
    }
}

/// A complete row: values in layout order and the rowid of rowid tables.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub(super) rowid: Option<i64>,
    pub(super) values: Vec<SqlValue>,
}

/// Changes of one table within a segment, written as insert, update and
/// delete steps.
pub(super) struct TableChanges {
    pub(super) layout: Arc<Layout>,
    pub(super) inserts: Vec<Row>,
    pub(super) updates: Vec<(Row, Row)>,
    pub(super) deletes: Vec<Row>,
}

/// YAML text of log steps.
pub(super) struct Writer {
    steps: Vec<String>,
}
impl Writer {
    pub(super) fn new() -> Self {
        Self { steps: vec![] }
    }
    pub(super) fn into_steps(self) -> Vec<String> {
        self.steps
    }
    pub(super) fn extend(&mut self, steps: impl IntoIterator<Item = String>) {
        self.steps.extend(steps);
    }
    pub(super) fn schema<'s>(&mut self, statements: impl IntoIterator<Item = &'s str>) {
        let statements: Vec<String> = statements.into_iter().map(quoted).collect();
        if !statements.is_empty() {
            self.steps
                .push(format!("{{schema: [{}]}}", statements.join(", ")));
        }
    }
    pub(super) fn changes(&mut self, changes: &TableChanges) -> Result<()> {
        let layout = &changes.layout;
        let table = key(&layout.table);
        if !changes.inserts.is_empty() {
            let mut step = format!("{{insert: {table}, rows: [");
            for (n, row) in changes.inserts.iter().enumerate() {
                if n > 0 {
                    step.push_str(", ");
                }
                inserted(&mut step, layout, row)?;
            }
            step.push_str("]}");
            self.steps.push(step);
        }
        for (before, after) in &changes.updates {
            let changed = |i: usize| before.values[i] != after.values[i];
            let mut step = format!("{{update: {table}, key: ");
            identity_fields(&mut step, layout, after)?;
            step.push_str(", set: ");
            fields(&mut step, layout, after, None, changed)?;
            step.push_str(", was: ");
            fields(&mut step, layout, before, None, changed)?;
            step.push('}');
            self.steps.push(step);
        }
        if !changes.deletes.is_empty() {
            let mut step = format!("{{delete: {table}, rows: [");
            for (n, row) in changes.deletes.iter().enumerate() {
                if n > 0 {
                    step.push_str(", ");
                }
                fields(&mut step, layout, row, stored_rowid(layout, row)?, |_| true)?;
            }
            step.push_str("]}");
            self.steps.push(step);
        }
        Ok(())
    }
    pub(super) fn pragma(&mut self, set: &[(&str, i64)], was: Option<&[(&str, i64)]>) {
        let map = |entries: &[(&str, i64)]| {
            entries
                .iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        self.steps.push(match was {
            Some(was) => format!("{{pragma: {{{}}}, was: {{{}}}}}", map(set), map(was)),
            None => format!("{{pragma: {{{}}}}}", map(set)),
        });
    }
    pub(super) fn sequences(&mut self, values: &BTreeMap<String, Option<i64>>) {
        if values.is_empty() {
            return;
        }
        let entries = values
            .iter()
            .map(|(table, value)| {
                format!(
                    "{}: {}",
                    key(table),
                    value.map_or("null".into(), |v| v.to_string())
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.steps.push(format!("{{sequences: {{{entries}}}}}"));
    }
    /// One line holding all steps, or `None` without steps.
    pub(super) fn line(&self) -> Option<String> {
        match self.steps.as_slice() {
            [] => None,
            [step] => Some(format!("- {step}\n")),
            steps => Some(format!("- [{}]\n", steps.join(", "))),
        }
    }
    /// One line per step.
    pub(super) fn lines(&self) -> impl Iterator<Item = String> + '_ {
        self.steps.iter().map(|step| format!("- {step}\n"))
    }
}

/// The rowid written with a row whose identity it is.
fn stored_rowid(layout: &Layout, row: &Row) -> Result<Option<i64>> {
    match layout.identity {
        Identity::Rowid(_) => row
            .rowid
            .map(Some)
            .ok_or_else(|| Error::internal("a stored rowid is missing")),
        _ => Ok(None),
    }
}

/// Append an inserted row. Omitted cells take the column default, so NULL
/// cells are omitted where the default is NULL.
pub(super) fn inserted(out: &mut String, layout: &Layout, row: &Row) -> Result<()> {
    fields(out, layout, row, stored_rowid(layout, row)?, |i| {
        !(layout.null_default[i] && row.values[i] == SqlValue::Null)
    })
}

/// Append a row as a flow mapping.
fn fields(
    out: &mut String,
    layout: &Layout,
    row: &Row,
    rowid: Option<i64>,
    include: impl Fn(usize) -> bool,
) -> Result<()> {
    use std::fmt::Write;
    out.push('{');
    let mut first = true;
    if let (Identity::Rowid(name), Some(rowid)) = (&layout.identity, rowid) {
        write!(out, "{name}: {rowid}").unwrap();
        first = false;
    }
    for (i, value) in row.values.iter().enumerate() {
        if include(i) {
            if !std::mem::take(&mut first) {
                out.push_str(", ");
            }
            out.push_str(&layout.names[i]);
            out.push_str(": ");
            layout.cell(out, i, ValueRef::from(value))?;
        }
    }
    out.push('}');
    Ok(())
}
fn identity_fields(out: &mut String, layout: &Layout, row: &Row) -> Result<()> {
    match &layout.identity {
        Identity::Alias(i) => fields(out, layout, row, None, |c| c == *i),
        Identity::Key(key) => fields(out, layout, row, None, |c| key.contains(&c)),
        Identity::Rowid(name) => {
            let rowid = row
                .rowid
                .ok_or_else(|| Error::internal("a stored rowid is missing"))?;
            out.push_str(&format!("{{{name}: {rowid}}}"));
            Ok(())
        }
        Identity::Row => Err(Error::internal("rows without identity are not updated")),
    }
}
