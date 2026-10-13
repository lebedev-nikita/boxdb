use crate::{Error, QueryResult, Result, Type, Value};
type Positions = smallvec::SmallVec<[usize; 1]>;
mod aggregate;
pub(crate) mod insert;
mod plan;
pub(crate) use plan::Plan;
use sqlparser::ast::helpers::stmt_create_table::CreateTableBuilder;
use sqlparser::ast::*;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    pub tables: BTreeMap<String, Table>,
    pub indexes: BTreeMap<String, Index>,
}
#[derive(Clone, Debug)]
pub(crate) struct Index {
    pub table: String,
    pub columns: Vec<usize>,
    pub unique: bool,
    pub sql: String,
    // Persistent B+ tree: a transaction clone shares nodes; editing a key only
    // copies the affected path, rather than the complete index.
    pub entries: imbl::OrdMap<IndexKey, Positions>,
}

// Integer keys have numeric order; other keys retain exact typed equality.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum IndexKey {
    Integer(i64),
    Other(String),
}
impl IndexKey {
    fn new(row: &[Value], columns: &[usize]) -> Result<Self> {
        if let [column] = columns
            && let Value::Integer(n) = row[*column]
        {
            return Ok(Self::Integer(n));
        }
        let values = columns
            .iter()
            .map(|i| normalized_key(&row[*i]))
            .collect::<Vec<_>>();
        serde_json::to_string(&values)
            .map(Self::Other)
            .map_err(|e| Error(e.to_string()))
    }
}
fn normalized_key(value: &Value) -> Value {
    match value {
        Value::Real(n) if *n == 0.0 => Value::Real(0.0),
        Value::Array(values) => Value::Array(values.iter().map(normalized_key).collect()),
        Value::Json(json) => {
            fn normalize(json: &serde_json::Value) -> serde_json::Value {
                use serde_json::Value as Json;
                match json {
                    Json::Number(n) if n.is_f64() && n.as_f64() == Some(0.0) => {
                        serde_json::json!(0.0)
                    }
                    Json::Array(values) => Json::Array(values.iter().map(normalize).collect()),
                    Json::Object(values) => Json::Object(
                        values
                            .iter()
                            .map(|(k, v)| (k.clone(), normalize(v)))
                            .collect(),
                    ),
                    _ => json.clone(),
                }
            }
            Value::Json(normalize(json))
        }
        _ => value.clone(),
    }
}

fn column_matches(expr: &Expr, column: &str, prefix: &str) -> bool {
    match expr {
        Expr::Identifier(n) => n.value.eq_ignore_ascii_case(column),
        Expr::CompoundIdentifier(ids) if ids.len() == 2 => {
            ids[0].value.eq_ignore_ascii_case(prefix) && ids[1].value.eq_ignore_ascii_case(column)
        }
        _ => false,
    }
}

struct IntegerBounds {
    lower: (i64, bool),
    upper: (i64, bool),
}
impl IntegerBounds {
    fn lower(&mut self, n: i64, inclusive: bool) {
        if n > self.lower.0 {
            self.lower = (n, inclusive);
        } else if n == self.lower.0 {
            self.lower.1 &= inclusive;
        }
    }
    fn upper(&mut self, n: i64, inclusive: bool) {
        if n < self.upper.0 {
            self.upper = (n, inclusive);
        } else if n == self.upper.0 {
            self.upper.1 &= inclusive;
        }
    }
    fn collect(&mut self, expr: &Expr, column: &str, prefix: &str) -> bool {
        use BinaryOperator::*;
        if let Expr::Nested(expr) = expr {
            return self.collect(expr, column, prefix);
        }
        let Expr::BinaryOp { left, op, right } = expr else {
            return false;
        };
        if *op == And {
            let a = self.collect(left, column, prefix);
            let b = self.collect(right, column, prefix);
            return a || b;
        }
        let (literal, op) = if column_matches(left, column, prefix) {
            (right.as_ref(), op.clone())
        } else if column_matches(right, column, prefix) {
            (
                left.as_ref(),
                match op {
                    Lt => Gt,
                    LtEq => GtEq,
                    Gt => Lt,
                    GtEq => LtEq,
                    other => other.clone(),
                },
            )
        } else {
            return false;
        };
        let Ok(Value::Integer(n)) = eval(literal, &[], &[]) else {
            return false;
        };
        match op {
            Eq => {
                self.lower(n, true);
                self.upper(n, true);
            }
            Gt => self.lower(n, false),
            GtEq => self.lower(n, true),
            Lt => self.upper(n, false),
            LtEq => self.upper(n, true),
            _ => return false,
        }
        true
    }
    fn positions(&self, index: &Index) -> Vec<usize> {
        use std::ops::Bound::{Excluded, Included};
        if self.lower.0 > self.upper.0
            || (self.lower.0 == self.upper.0 && !(self.lower.1 && self.upper.1))
        {
            return vec![];
        }
        let lower = if self.lower.1 {
            Included(IndexKey::Integer(self.lower.0))
        } else {
            Excluded(IndexKey::Integer(self.lower.0))
        };
        let upper = if self.upper.1 {
            Included(IndexKey::Integer(self.upper.0))
        } else {
            Excluded(IndexKey::Integer(self.upper.0))
        };
        let mut positions = index
            .entries
            .range((lower, upper))
            .flat_map(|(_, rows)| rows.iter().copied())
            .collect::<Vec<_>>();
        // Preserve table order for queries without ORDER BY and DML RETURNING.
        positions.sort_unstable();
        positions
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Table {
    pub columns: Vec<Column>,
    pub rows: crate::rows::Rows,
    pub constraints: Vec<Constraint>,
    pub constraint_indexes: Vec<Index>,
    pub next_rowid: u64,
    pub sql: String,
}
impl Table {
    fn primary_sequence(&self) -> (Option<usize>, Option<i64>) {
        let column = self.constraints.iter().find_map(|c| match c {
            Constraint::Unique {
                columns,
                primary: true,
            } if columns.len() == 1 && self.columns[columns[0]].ty == Type::Integer => {
                Some(columns[0])
            }
            _ => None,
        });
        let maximum = column
            .and_then(|column| {
                self.constraint_indexes
                    .iter()
                    .find(|i| i.columns == [column])
            })
            .and_then(|i| i.entries.get_max())
            .and_then(|(key, _)| {
                if let IndexKey::Integer(n) = key {
                    Some(*n)
                } else {
                    None
                }
            });
        (column, maximum)
    }
    fn normalize_primary(
        &self,
        row: &mut [Value],
        primary: Option<usize>,
        maximum: &mut Option<i64>,
    ) -> Result<()> {
        if let Some(column) = primary {
            if row[column] == Value::Null {
                row[column] = self.columns[column].ty.validate(&Value::Integer(
                    maximum
                        .unwrap_or(0)
                        .checked_add(1)
                        .ok_or_else(|| Error("primary key exhausted".into()))?,
                ))?;
            }
            if let Value::Integer(n) = row[column] {
                *maximum = Some(maximum.map_or(n, |old| old.max(n)));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Column {
    pub name: String,
    pub ty: Type,
    pub not_null: bool,
    pub default: Option<Expr>,
}
#[derive(Clone, Debug)]
pub(crate) enum Constraint {
    Unique {
        columns: Vec<usize>,
        primary: bool,
    },
    Check(Box<Expr>),
    Foreign {
        columns: Vec<usize>,
        table: String,
        target: Vec<String>,
        on_delete: Option<ReferentialAction>,
        on_update: Option<ReferentialAction>,
    },
}

pub(crate) fn name(n: &ObjectName) -> String {
    n.0.iter()
        .map(|i| i.value.to_lowercase())
        .collect::<Vec<_>>()
        .join(".")
}
fn empty(affected: usize) -> QueryResult {
    QueryResult {
        columns: vec![],
        rows: vec![],
        affected,
    }
}
fn equivalent(a: &Value, b: &Value) -> bool {
    a == b || crate::expression::compare(a, b).is_ok_and(|order| order.is_eq())
}
fn same_columns(a: &[usize], b: &[usize]) -> bool {
    a.len() == b.len() && a.iter().all(|column| b.contains(column))
}
fn index_key(index: &Index, row: &[Value]) -> Result<Option<IndexKey>> {
    if index.unique && index.columns.iter().any(|i| row[*i] == Value::Null) {
        return Ok(None);
    }
    IndexKey::new(row, &index.columns).map(Some)
}
fn rebuild_index(index: &mut Index, rows: &crate::rows::Rows) -> Result<()> {
    let mut keys = rows
        .positions()
        .filter_map(|(position, row)| {
            index_key(index, row)
                .transpose()
                .map(|key| key.map(|key| (key, position)))
        })
        .collect::<Result<Vec<_>>>()?;
    keys.sort_unstable();
    let mut entries: Vec<(IndexKey, Positions)> = Vec::with_capacity(keys.len());
    for (key, position) in keys {
        if let Some((previous, positions)) = entries.last_mut()
            && *previous == key
        {
            if index.unique {
                return Err(Error("UNIQUE constraint failed".into()));
            }
            positions.push(position);
        } else {
            entries.push((key, smallvec::smallvec![position]));
        }
    }
    index.entries = entries.into_iter().collect();
    Ok(())
}
fn update_index(index: &mut Index, changes: &[crate::rows::Change<'_>]) -> Result<()> {
    let mut edits = Vec::new();
    for change in changes {
        if let (Some(old), Some(new)) = (change.old, change.new)
            && index.columns.iter().all(|i| old[*i] == new[*i])
        {
            continue;
        }
        let old = change
            .old
            .map(|row| index_key(index, row))
            .transpose()?
            .flatten();
        let new = change
            .new
            .map(|row| index_key(index, row))
            .transpose()?
            .flatten();
        if old != new {
            edits.push((change.position, old, new));
        }
    }
    if edits.is_empty() {
        return Ok(());
    }
    let entries = &mut index.entries;
    // Remove all old keys before inserting new ones: a single UPDATE may swap
    // unique keys. Constraint checks apply to the final statement state.
    for (position, old, _) in &edits {
        if let Some(old) = old
            && let Some(positions) = entries.get_mut(old)
        {
            positions.retain(|p| *p != *position);
            if positions.is_empty() {
                entries.remove(old);
            }
        }
    }
    for (position, _, new) in edits {
        if let Some(new) = new {
            let positions = entries.entry(new).or_default();
            if index.unique && !positions.is_empty() {
                return Err(Error("UNIQUE constraint failed".into()));
            }
            positions.push(position);
            positions.sort_unstable();
        }
    }
    Ok(())
}
fn shared_constraint_index(
    table: &Table,
    index: &Index,
) -> Option<imbl::OrdMap<IndexKey, Positions>> {
    let non_null = index.columns.iter().all(|i| table.columns[*i].not_null || table.constraints.iter().any(|c| matches!(c, Constraint::Unique { columns, primary: true } if columns.contains(i))));
    if !index.unique && !non_null {
        return None;
    }
    table
        .constraint_indexes
        .iter()
        .find(|i| i.columns == index.columns)
        .map(|i| i.entries.clone())
}
fn indexed_reference(
    parent: &Table,
    index: &Index,
    target: &[usize],
    columns: &[usize],
    row: &[Value],
) -> Option<bool> {
    let mut values = Vec::with_capacity(index.columns.len());
    for column in &index.columns {
        let value = &row[columns[target.iter().position(|i| i == column)?]];
        let ty = &parent.columns[*column].ty;
        let normalized = ty
            .normalize(value.clone())
            .or_else(|e| {
                if matches!(
                    value,
                    Value::Integer(_) | Value::Real(_) | Value::Numeric(_)
                ) && matches!(
                    ty,
                    Type::SmallInt
                        | Type::Integer
                        | Type::BigInt
                        | Type::Real
                        | Type::Double
                        | Type::Numeric(_)
                ) {
                    ty.cast(value.clone())
                } else {
                    Err(e)
                }
            })
            .ok()?;
        if !equivalent(value, &normalized) {
            return None;
        }
        values.push(normalized);
    }
    let key = IndexKey::new(&values, &(0..values.len()).collect::<Vec<_>>()).ok()?;
    Some(index.entries.contains_key(&key))
}
impl State {
    pub fn execute(&mut self, statement: &Statement) -> Result<QueryResult> {
        self.apply(statement, true)
    }
    pub fn declare(&mut self, statement: &Statement) -> Result<()> {
        if !matches!(
            statement,
            Statement::CreateTable(_) | Statement::CreateIndex(_)
        ) {
            return Err(Error(
                "snapshot declarations must be CREATE TABLE or CREATE INDEX".into(),
            ));
        }
        self.apply(statement, false)?;
        Ok(())
    }
    fn apply(&mut self, statement: &Statement, validate_declaration: bool) -> Result<QueryResult> {
        match statement {
            Statement::CreateTable(c) => {
                if c.strict {
                    return Err(Error("STRICT is unnecessary: all tables are strict".into()));
                }
                let supported = CreateTableBuilder::new(c.name.clone())
                    .columns(c.columns.clone())
                    .constraints(c.constraints.clone())
                    .if_not_exists(c.if_not_exists)
                    .build();
                if supported.to_string() != statement.to_string() {
                    return Err(Error("unsupported CREATE TABLE clause".into()));
                }
                if c.temporary
                    || c.external
                    || c.or_replace
                    || c.without_rowid
                    || c.like.is_some()
                    || c.clone.is_some()
                    || c.engine.is_some()
                    || !c.with_options.is_empty()
                    || !c.table_properties.is_empty()
                    || c.primary_key.is_some()
                    || c.order_by.is_some()
                    || c.partition_by.is_some()
                    || c.cluster_by.is_some()
                {
                    return Err(Error("unsupported CREATE TABLE clause".into()));
                }
                if self.tables.contains_key(&name(&c.name)) {
                    if c.if_not_exists {
                        return Ok(empty(0));
                    }
                    return Err(Error(format!("table already exists: {}", c.name)));
                }
                if matches!(name(&c.name).as_str(), "sqlite_schema" | "sqlite_master")
                    || self.indexes.contains_key(&name(&c.name))
                {
                    return Err(Error("reserved or occupied object name".into()));
                }
                if c.query.is_some() || c.columns.is_empty() {
                    return Err(Error("CREATE TABLE needs explicitly typed columns".into()));
                }
                let mut columns = vec![];
                let mut constraints = vec![];
                for (index, c) in c.columns.iter().enumerate() {
                    if c.collation.is_some() {
                        return Err(Error("column collations unsupported".into()));
                    }
                    if columns
                        .iter()
                        .any(|v: &Column| v.name.eq_ignore_ascii_case(&c.name.value))
                    {
                        return Err(Error("duplicate column name".into()));
                    }
                    let mut column = Column {
                        name: c.name.value.to_lowercase(),
                        ty: Type::parse(&c.data_type.to_string())?,
                        not_null: false,
                        default: None,
                    };
                    for option in &c.options {
                        match &option.option {
                            ColumnOption::Null => (),
                            ColumnOption::NotNull => column.not_null = true,
                            ColumnOption::Default(e) => {
                                column.ty.validate(&eval(e, &[], &[])?)?;
                                column.default = Some(e.clone());
                            }
                            ColumnOption::Unique {
                                is_primary,
                                characteristics,
                            } => {
                                if characteristics.is_some() {
                                    return Err(Error(
                                        "constraint characteristics unsupported".into(),
                                    ));
                                }
                                if *is_primary {
                                    column.not_null = true;
                                }
                                constraints.push(Constraint::Unique {
                                    columns: vec![index],
                                    primary: *is_primary,
                                });
                            }
                            ColumnOption::Check(e) => {
                                constraints.push(Constraint::Check(Box::new(e.clone())))
                            }
                            ColumnOption::ForeignKey {
                                foreign_table,
                                referred_columns,
                                on_delete,
                                on_update,
                                characteristics,
                            } => {
                                if characteristics.is_some() {
                                    return Err(Error("deferred foreign keys unsupported".into()));
                                }
                                constraints.push(Constraint::Foreign {
                                    columns: vec![index],
                                    table: name(foreign_table),
                                    target: referred_columns
                                        .iter()
                                        .map(|c| c.value.to_lowercase())
                                        .collect(),
                                    on_delete: *on_delete,
                                    on_update: *on_update,
                                });
                            }
                            _ => {
                                return Err(Error(format!(
                                    "unsupported column option: {}",
                                    option.option
                                )));
                            }
                        }
                    }
                    columns.push(column);
                }
                let resolve = |names: &[Ident]| -> Result<Vec<usize>> {
                    names
                        .iter()
                        .map(|n| {
                            columns
                                .iter()
                                .position(|c| c.name.eq_ignore_ascii_case(&n.value))
                                .ok_or_else(|| Error(format!("no such column: {n}")))
                        })
                        .collect()
                };
                for constraint in &c.constraints {
                    match constraint {
                        TableConstraint::Unique {
                            columns: names,
                            characteristics,
                            nulls_distinct,
                            ..
                        } => {
                            if characteristics.is_some()
                                || !matches!(nulls_distinct, NullsDistinctOption::None)
                            {
                                return Err(Error("constraint characteristics unsupported".into()));
                            }
                            constraints.push(Constraint::Unique {
                                columns: resolve(names)?,
                                primary: false,
                            });
                        }
                        TableConstraint::PrimaryKey {
                            columns: names,
                            characteristics,
                            ..
                        } => {
                            if characteristics.is_some() {
                                return Err(Error("constraint characteristics unsupported".into()));
                            }
                            constraints.push(Constraint::Unique {
                                columns: resolve(names)?,
                                primary: true,
                            });
                        }
                        TableConstraint::Check { expr, .. } => {
                            constraints.push(Constraint::Check(expr.clone()))
                        }
                        TableConstraint::ForeignKey {
                            columns: names,
                            foreign_table,
                            referred_columns,
                            on_delete,
                            on_update,
                            characteristics,
                            ..
                        } => {
                            if characteristics.is_some() {
                                return Err(Error("deferred foreign keys unsupported".into()));
                            }
                            constraints.push(Constraint::Foreign {
                                columns: resolve(names)?,
                                table: name(foreign_table),
                                target: referred_columns
                                    .iter()
                                    .map(|c| c.value.to_lowercase())
                                    .collect(),
                                on_delete: *on_delete,
                                on_update: *on_update,
                            });
                        }
                        _ => return Err(Error("unsupported table constraint".into())),
                    }
                }
                if constraints
                    .iter()
                    .filter(|c| matches!(c, Constraint::Unique { primary: true, .. }))
                    .count()
                    > 1
                {
                    return Err(Error("multiple primary keys".into()));
                }
                self.tables.insert(
                    name(&c.name),
                    Table {
                        columns,
                        constraint_indexes: constraints
                            .iter()
                            .filter_map(|constraint| {
                                if let Constraint::Unique { columns, .. } = constraint {
                                    Some(Index {
                                        table: name(&c.name),
                                        columns: columns.clone(),
                                        unique: true,
                                        sql: String::new(),
                                        entries: imbl::OrdMap::new(),
                                    })
                                } else {
                                    None
                                }
                            })
                            .collect(),
                        constraints,
                        rows: crate::rows::Rows::default(),
                        next_rowid: 1,
                        sql: c.to_string(),
                    },
                );
                if validate_declaration {
                    self.validate()?;
                }
                Ok(empty(0))
            }
            Statement::Insert(i) => {
                if i.or.is_some()
                    || i.on.is_some()
                    || i.overwrite
                    || i.ignore
                    || i.replace_into
                    || i.partitioned.is_some()
                    || i.table_alias.is_some()
                {
                    return Err(Error("unsupported INSERT clause".into()));
                }
                let result = if let Some(source) = &i.source {
                    self.query(source)?
                } else {
                    empty(0)
                };
                let table = self
                    .tables
                    .get_mut(&name(&i.table_name))
                    .ok_or_else(|| Error("no such table".into()))?;
                let sources = if i.source.is_none() {
                    vec![vec![]]
                } else {
                    result.rows
                };
                let count = sources.len();
                let mut returned = vec![];
                let mut seen = std::collections::BTreeSet::new();
                if i.columns
                    .iter()
                    .any(|c| !seen.insert(c.value.to_lowercase()))
                {
                    return Err(Error("duplicate INSERT column".into()));
                }
                let names = table
                    .columns
                    .iter()
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>();
                let columns = i
                    .returning
                    .as_ref()
                    .map(|p| project(p, &names, &vec![Value::Null; names.len()]).map(|r| r.0))
                    .transpose()?
                    .unwrap_or_default();
                let (primary, mut maximum) = table.primary_sequence();
                for source in sources {
                    let mut row = table
                        .columns
                        .iter()
                        .map(|c| {
                            c.default
                                .as_ref()
                                .map(|e| c.ty.validate(&eval(e, &[], &[])?))
                                .unwrap_or(Ok(Value::Null))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let indexes = if i.source.is_none() {
                        vec![]
                    } else if i.columns.is_empty() {
                        (0..row.len()).collect::<Vec<_>>()
                    } else {
                        i.columns
                            .iter()
                            .map(|n| {
                                table
                                    .columns
                                    .iter()
                                    .position(|c| c.name.eq_ignore_ascii_case(&n.value))
                                    .ok_or_else(|| Error(format!("no such column: {n}")))
                            })
                            .collect::<Result<_>>()?
                    };
                    if source.len() != indexes.len() {
                        return Err(Error("column count mismatch".into()));
                    }
                    for (value, index) in source.into_iter().zip(indexes) {
                        row[index] = table.columns[index].ty.normalize(value)?;
                    }
                    table.normalize_primary(&mut row, primary, &mut maximum)?;
                    if let Some(items) = &i.returning {
                        returned.push(project(items, &names, &row)?.1);
                    }
                    table.rows.push(table.next_rowid, row)?;
                    table.next_rowid = table
                        .next_rowid
                        .checked_add(1)
                        .ok_or_else(|| Error("row id exhausted".into()))?;
                }
                Ok(QueryResult {
                    columns,
                    rows: returned,
                    affected: count,
                })
            }
            Statement::Query(q) => self.query(q),
            Statement::Update {
                table,
                assignments,
                selection,
                returning,
                from,
                or,
            } => {
                if from.is_some() || or.is_some() || !table.joins.is_empty() {
                    return Err(Error("unsupported UPDATE clause".into()));
                }
                let TableFactor::Table { name: n, .. } = &table.relation else {
                    return Err(Error("UPDATE needs table".into()));
                };
                let positions = self.index_positions(&name(n), &name(n), selection.as_ref())?;
                let cascades = self.tables.values().any(|table| {
                    table.constraints.iter().any(
                        |c| matches!(c, Constraint::Foreign { table, .. } if table == &name(n)),
                    )
                });
                let t = self
                    .tables
                    .get_mut(&name(n))
                    .ok_or_else(|| Error(format!("no such table: {n}")))?;
                let names = t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
                if let Some(e) = selection {
                    bind(e, &names, false)?;
                }
                for a in assignments {
                    let AssignmentTarget::ColumnName(n) = &a.target else {
                        return Err(Error("tuple assignments unsupported".into()));
                    };
                    if !names.contains(&name(n)) {
                        return Err(Error(format!("no such column: {n}")));
                    }
                    bind(&a.value, &names, false)?;
                }
                let mut returned = vec![];
                let mut affected = 0;
                let mut changes = vec![];
                for position in
                    positions.unwrap_or_else(|| t.rows.positions().map(|(i, _)| i).collect())
                {
                    if !selected(selection.as_ref(), &names, &t.rows[position])? {
                        continue;
                    }
                    let old = &t.rows[position];
                    let mut edits = Vec::with_capacity(assignments.len());
                    for a in assignments {
                        let AssignmentTarget::ColumnName(n) = &a.target else {
                            return Err(Error("tuple assignments unsupported".into()));
                        };
                        let index = names
                            .iter()
                            .position(|c| *c == name(n))
                            .ok_or_else(|| Error(format!("no such column: {n}")))?;
                        edits.push((
                            index,
                            t.columns[index]
                                .ty
                                .normalize(eval(&a.value, &names, old)?)?,
                        ));
                    }
                    let old = cascades.then(|| old.clone());
                    let row = &mut t.rows[position];
                    for (index, value) in edits {
                        row[index] = value;
                    }
                    affected += 1;
                    if let Some(old) = old
                        && old != *row
                    {
                        changes.push((name(n), old, Some(row.clone())));
                    }
                    if let Some(p) = returning {
                        returned.push(project(p, &names, row)?.1);
                    }
                }
                let columns = returning
                    .as_ref()
                    .map(|p| project(p, &names, &vec![Value::Null; names.len()]).map(|r| r.0))
                    .transpose()?
                    .unwrap_or_default();
                self.cascade(changes)?;
                Ok(QueryResult {
                    columns,
                    rows: returned,
                    affected,
                })
            }
            Statement::Delete(d) => {
                if d.using.is_some()
                    || !d.tables.is_empty()
                    || d.limit.is_some()
                    || !d.order_by.is_empty()
                {
                    return Err(Error("unsupported DELETE clause".into()));
                }
                let (FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from)) = &d.from;
                if from.len() != 1 || !from[0].joins.is_empty() {
                    return Err(Error("DELETE needs one table".into()));
                }
                let TableFactor::Table { name: n, .. } = &from[0].relation else {
                    return Err(Error("DELETE needs table".into()));
                };
                let positions = self.index_positions(&name(n), &name(n), d.selection.as_ref())?;
                let cascades = self.tables.values().any(|table| {
                    table.constraints.iter().any(
                        |c| matches!(c, Constraint::Foreign { table, .. } if table == &name(n)),
                    )
                });
                let t = self
                    .tables
                    .get_mut(&name(n))
                    .ok_or_else(|| Error(format!("no such table: {n}")))?;
                let names = t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
                if let Some(e) = &d.selection {
                    bind(e, &names, false)?;
                }
                let mut returned = vec![];
                let mut affected = 0;
                let mut changes = vec![];
                let candidates =
                    positions.unwrap_or_else(|| t.rows.positions().map(|(i, _)| i).collect());
                for position in candidates {
                    let row = &t.rows[position];
                    if selected(d.selection.as_ref(), &names, row)? {
                        affected += 1;
                        if cascades {
                            changes.push((name(n), row.clone(), None));
                        }
                        if let Some(p) = &d.returning {
                            returned.push(project(p, &names, row)?.1);
                        }
                        t.rows.remove(position);
                    }
                }
                let columns = d
                    .returning
                    .as_ref()
                    .map(|p| project(p, &names, &vec![Value::Null; names.len()]).map(|r| r.0))
                    .transpose()?
                    .unwrap_or_default();
                self.cascade(changes)?;
                Ok(QueryResult {
                    columns,
                    rows: returned,
                    affected,
                })
            }
            Statement::Drop {
                object_type: ObjectType::Table,
                names,
                if_exists,
                cascade,
                ..
            } => {
                if *cascade {
                    return Err(Error("DROP CASCADE is unsupported".into()));
                }
                for n in names {
                    if self.tables.remove(&name(n)).is_none() && !if_exists {
                        return Err(Error(format!("no such table: {n}")));
                    }
                    self.indexes.retain(|_, index| index.table != name(n));
                }
                self.validate()?;
                Ok(empty(0))
            }
            Statement::CreateIndex(i) => {
                if i.using.is_some()
                    || i.concurrently
                    || !i.include.is_empty()
                    || i.predicate.is_some()
                    || !i.with.is_empty()
                    || i.nulls_distinct.is_some()
                {
                    return Err(Error("unsupported index clause".into()));
                }
                let index_name = name(
                    i.name
                        .as_ref()
                        .ok_or_else(|| Error("index needs a name".into()))?,
                );
                if self.indexes.contains_key(&index_name) {
                    if i.if_not_exists {
                        return Ok(empty(0));
                    }
                    return Err(Error("index already exists".into()));
                }
                if self.tables.contains_key(&index_name) {
                    return Err(Error("object name already occupied".into()));
                }
                let t = self
                    .tables
                    .get(&name(&i.table_name))
                    .ok_or_else(|| Error("no such table".into()))?;
                let columns = i
                    .columns
                    .iter()
                    .map(|o| {
                        let Expr::Identifier(n) = &o.expr else {
                            return Err(Error("only column indexes supported".into()));
                        };
                        t.columns
                            .iter()
                            .position(|c| c.name.eq_ignore_ascii_case(&n.value))
                            .ok_or_else(|| Error(format!("no such column: {n}")))
                    })
                    .collect::<Result<Vec<_>>>()?;
                if columns.is_empty() {
                    return Err(Error("index needs columns".into()));
                }
                self.indexes.insert(
                    index_name,
                    Index {
                        table: name(&i.table_name),
                        columns,
                        unique: i.unique,
                        sql: i.to_string(),
                        entries: imbl::OrdMap::new(),
                    },
                );
                if validate_declaration {
                    self.validate()?;
                }
                Ok(empty(0))
            }
            Statement::Drop {
                object_type: ObjectType::Index,
                names,
                if_exists,
                ..
            } => {
                for n in names {
                    if self.indexes.remove(&name(n)).is_none() && !if_exists {
                        return Err(Error("no such index".into()));
                    }
                }
                Ok(empty(0))
            }
            _ => Err(Error(format!("unsupported statement: {statement}"))),
        }
    }
    fn cascade(
        &mut self,
        mut changes: Vec<(String, Vec<Value>, Option<Vec<Value>>)>,
    ) -> Result<()> {
        let mut processed = 0;
        while let Some((parent, old, new)) = changes.pop() {
            processed += 1;
            if processed > 100_000 {
                return Err(Error("foreign key cascade exceeds limit".into()));
            }
            let children = self
                .tables
                .iter()
                .flat_map(|(name, t)| {
                    t.constraints.iter().filter_map(|c| {
                        if matches!(c,Constraint::Foreign { table,.. } if table==&parent) {
                            Some((name.clone(), c.clone()))
                        } else {
                            None
                        }
                    })
                })
                .collect::<Vec<_>>();
            for (child, constraint) in children {
                let Constraint::Foreign {
                    columns,
                    target,
                    on_delete,
                    on_update,
                    ..
                } = constraint
                else {
                    unreachable!()
                };
                let target = reference_columns(&self.tables[&parent], &target)?;
                if columns.len() != target.len() {
                    return Err(Error("foreign key column count mismatch".into()));
                }
                if new
                    .as_ref()
                    .is_some_and(|new| target.iter().all(|i| old[*i] == new[*i]))
                {
                    continue;
                }
                let action = if new.is_some() { on_update } else { on_delete }
                    .unwrap_or(ReferentialAction::NoAction);
                if action == ReferentialAction::NoAction {
                    continue;
                }
                let table = self.tables.get_mut(&child).unwrap();
                for position in table.rows.positions().map(|(i, _)| i).collect::<Vec<_>>() {
                    let row = &table.rows[position];
                    if columns.iter().any(|i| row[*i] == Value::Null)
                        || !columns
                            .iter()
                            .zip(&target)
                            .all(|(a, b)| equivalent(&row[*a], &old[*b]))
                    {
                        continue;
                    }
                    if action == ReferentialAction::Restrict {
                        return Err(Error("FOREIGN KEY RESTRICT constraint failed".into()));
                    }
                    let old_child = row.clone();
                    if action == ReferentialAction::Cascade && new.is_none() {
                        table.rows.remove(position);
                        changes.push((child.clone(), old_child, None));
                    } else {
                        for (column, target) in columns.iter().zip(&target) {
                            let value = match action {
                                ReferentialAction::Cascade => {
                                    new.as_ref().unwrap()[*target].clone()
                                }
                                ReferentialAction::SetNull => Value::Null,
                                ReferentialAction::SetDefault => table.columns[*column]
                                    .default
                                    .as_ref()
                                    .map(|e| eval(e, &[], &[]))
                                    .unwrap_or(Ok(Value::Null))?,
                                _ => return Err(Error("unsupported referential action".into())),
                            };
                            table.rows[position][*column] =
                                table.columns[*column].ty.validate(&value)?;
                        }
                        let new_child = table.rows[position].clone();
                        if old_child != new_child {
                            changes.push((child.clone(), old_child, Some(new_child)));
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        self.validate_inner(true)
    }
    pub fn validate_import(&mut self) -> Result<()> {
        // SQL declaration parsing has bound schema/index columns, and codecs
        // have checked row widths. Building indexes checks unique keys once.
        self.rebuild_indexes()?;
        self.validate_inner(false)
    }
    fn validate_inner(&self, unique: bool) -> Result<()> {
        use crate::expression::truth;
        for (table_name, table) in &self.tables {
            let names = table
                .columns
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>();
            if table.columns.is_empty()
                || table.next_rowid == 0
                || table.rows.last_id() >= table.next_rowid
            {
                return Err(Error("invalid table metadata".into()));
            }
            let mut names_seen = std::collections::BTreeSet::new();
            for column in &table.columns {
                if column.name.is_empty() || !names_seen.insert(column.name.to_lowercase()) {
                    return Err(Error("invalid or duplicate column name".into()));
                }
                column.ty.validate(&Value::Null)?;
                if let Some(e) = &column.default {
                    bind(e, &[], false)?;
                    column.ty.validate(&eval(e, &[], &[])?)?;
                }
            }
            for constraint in &table.constraints {
                match constraint {
                    Constraint::Check(e) => bind(e, &names, false)?,
                    Constraint::Unique { columns, .. } | Constraint::Foreign { columns, .. } => {
                        let unique = columns.iter().collect::<std::collections::BTreeSet<_>>();
                        if columns.is_empty()
                            || unique.len() != columns.len()
                            || columns.iter().any(|i| *i >= table.columns.len())
                        {
                            return Err(Error("invalid constraint column".into()));
                        }
                        if let Constraint::Foreign {
                            table: parent,
                            target,
                            ..
                        } = constraint
                            && let Some(parent) = self.tables.get(parent)
                        {
                            let indexes = reference_columns(parent, target)?;
                            let is_unique = parent.constraints.iter().any(|c| matches!(c,Constraint::Unique { columns,.. } if same_columns(columns,&indexes))) || self.indexes.values().any(|i| i.table==match constraint { Constraint::Foreign { table,.. } => table.as_str(), _ => unreachable!() } && i.unique && same_columns(&i.columns,&indexes));
                            if columns.len() != indexes.len() || !is_unique {
                                return Err(Error(
                                    "foreign key must reference a unique key of matching width"
                                        .into(),
                                ));
                            }
                        }
                    }
                }
            }
            let mut unique_keys = vec![HashSet::new(); table.constraints.len()];
            for row in table.rows.iter() {
                if row.len() != table.columns.len() {
                    return Err(Error("column count mismatch".into()));
                }
                for (column, value) in table.columns.iter().zip(row) {
                    column.ty.check(value)?;
                    if column.not_null && *value == Value::Null {
                        return Err(Error(format!(
                            "NOT NULL constraint failed: {table_name}.{}",
                            column.name
                        )));
                    }
                }
                for (constraint_index, constraint) in table.constraints.iter().enumerate() {
                    match constraint {
                        Constraint::Check(e) => {
                            if truth(&eval(e, &names, row)?)? == Some(false) {
                                return Err(Error(format!(
                                    "CHECK constraint failed: {table_name}"
                                )));
                            }
                        }
                        Constraint::Unique { columns, primary } => {
                            let has_null = columns.iter().any(|i| row[*i] == Value::Null);
                            if has_null {
                                if *primary {
                                    return Err(Error("PRIMARY KEY cannot contain NULL".into()));
                                }
                                continue;
                            }
                            if unique
                                && !unique_keys[constraint_index]
                                    .insert(IndexKey::new(row, columns)?)
                            {
                                return Err(Error(format!(
                                    "UNIQUE constraint failed: {table_name}"
                                )));
                            }
                        }
                        Constraint::Foreign {
                            columns,
                            table: parent_name,
                            target,
                            ..
                        } => {
                            let target_table = self.tables.get(parent_name).ok_or_else(|| {
                                Error("foreign key references missing table".into())
                            })?;
                            let target_indexes = reference_columns(target_table, target)?;
                            if columns.len() != target_indexes.len() {
                                return Err(Error("foreign key column count mismatch".into()));
                            }
                            if columns.iter().any(|i| row[*i] == Value::Null) {
                                continue;
                            }
                            let cached = if !unique {
                                target_table
                                    .constraint_indexes
                                    .iter()
                                    .chain(
                                        self.indexes.values().filter(|i| i.table == *parent_name),
                                    )
                                    .find(|i| i.unique && same_columns(&i.columns, &target_indexes))
                                    .and_then(|i| {
                                        indexed_reference(
                                            target_table,
                                            i,
                                            &target_indexes,
                                            columns,
                                            row,
                                        )
                                    })
                            } else {
                                None
                            };
                            if !cached.unwrap_or_else(|| {
                                target_table.rows.iter().any(|other| {
                                    columns
                                        .iter()
                                        .zip(&target_indexes)
                                        .all(|(a, b)| equivalent(&row[*a], &other[*b]))
                                })
                            }) {
                                return Err(Error("FOREIGN KEY constraint failed".into()));
                            }
                        }
                    }
                }
            }
        }
        for (name, index) in &self.indexes {
            let table = self
                .tables
                .get(&index.table)
                .ok_or_else(|| Error("index references missing table".into()))?;
            if index.columns.is_empty() || index.columns.iter().any(|i| *i >= table.columns.len()) {
                return Err(Error("invalid index column".into()));
            }
            if unique && index.unique {
                let mut seen = HashSet::new();
                for row in table.rows.iter() {
                    if index.columns.iter().any(|i| row[*i] == Value::Null) {
                        continue;
                    }
                    if !seen.insert(IndexKey::new(row, &index.columns)?) {
                        return Err(Error(format!("UNIQUE index constraint failed: {name}")));
                    }
                }
            }
        }
        Ok(())
    }
    pub fn rebuild_indexes(&mut self) -> Result<()> {
        for (name, table) in &mut self.tables {
            for index in &mut table.constraint_indexes {
                rebuild_index(index, &table.rows)?;
            }
            for index in self.indexes.values_mut().filter(|i| i.table == *name) {
                if let Some(entries) = shared_constraint_index(table, index) {
                    index.entries = entries;
                } else {
                    rebuild_index(index, &table.rows)?;
                }
            }
        }
        Ok(())
    }
    pub fn validate_changes(&mut self, previous: &State, mutation: bool) -> Result<()> {
        for table in self.tables.values_mut() {
            table.rows.compact();
        }
        let local = mutation
            && self.tables.iter().all(|(name, table)| {
                previous
                    .tables
                    .get(name)
                    .is_some_and(|old| table.rows.shares_history(&old.rows))
                    && !table
                        .constraints
                        .iter()
                        .any(|c| matches!(c, Constraint::Foreign { .. }))
            });
        if !local {
            return self.validate_import();
        }
        for (name, table) in &mut self.tables {
            let changes = table.rows.changes(&previous.tables[name].rows);
            if changes.is_empty() {
                continue;
            }
            let columns = table
                .columns
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>();
            for change in &changes {
                if let Some(row) = change.new {
                    for (column, value) in table.columns.iter().zip(row) {
                        // INSERT/UPDATE have already normalized and checked
                        // assigned cells. Constraints below check the new row.
                        if column.not_null && *value == Value::Null {
                            return Err(Error(format!(
                                "NOT NULL constraint failed: {name}.{}",
                                column.name
                            )));
                        }
                    }
                    for constraint in &table.constraints {
                        match constraint {
                            Constraint::Check(expr)
                                if crate::expression::truth(&eval(expr, &columns, row)?)?
                                    == Some(false) =>
                            {
                                return Err(Error(format!("CHECK constraint failed: {name}")));
                            }
                            Constraint::Unique {
                                columns,
                                primary: true,
                            } if columns.iter().any(|i| row[*i] == Value::Null) => {
                                return Err(Error("PRIMARY KEY cannot contain NULL".into()));
                            }
                            _ => (),
                        }
                    }
                }
            }
            for index in &mut table.constraint_indexes {
                update_index(index, &changes)?;
            }
            for index in self.indexes.values_mut().filter(|i| i.table == *name) {
                if let Some(entries) = shared_constraint_index(table, index) {
                    index.entries = entries;
                } else {
                    update_index(index, &changes)?;
                }
            }
        }
        Ok(())
    }
    fn index_positions(
        &self,
        table_name: &str,
        prefix: &str,
        selection: Option<&Expr>,
    ) -> Result<Option<Vec<usize>>> {
        let (Some(table), Some(selection)) = (self.tables.get(table_name), selection) else {
            return Ok(None);
        };
        for index in table
            .constraint_indexes
            .iter()
            .chain(self.indexes.values().filter(|i| i.table == table_name))
            .filter(|i| i.columns.len() == 1)
        {
            let column = &table.columns[index.columns[0]];
            if matches!(column.ty, Type::SmallInt | Type::Integer | Type::BigInt) {
                let mut bounds = IntegerBounds {
                    lower: (i64::MIN, true),
                    upper: (i64::MAX, true),
                };
                if bounds.collect(selection, &column.name, prefix) {
                    return Ok(Some(bounds.positions(index)));
                }
            }
            if let Expr::BinaryOp {
                left,
                op: BinaryOperator::Eq,
                right,
            } = selection
            {
                for (expr, literal) in [
                    (left.as_ref(), right.as_ref()),
                    (right.as_ref(), left.as_ref()),
                ] {
                    if column_matches(expr, &column.name, prefix)
                        && let Ok(value) =
                            eval(literal, &[], &[]).and_then(|value| column.ty.validate(&value))
                    {
                        let key = IndexKey::new(&[value], &[0])?;
                        return Ok(Some(
                            index
                                .entries
                                .get(&key)
                                .map(|p| p.to_vec())
                                .unwrap_or_default(),
                        ));
                    }
                }
            }
        }
        Ok(None)
    }
    fn query(&self, q: &Query) -> Result<QueryResult> {
        if q.with.is_some()
            || !q.locks.is_empty()
            || q.fetch.is_some()
            || !q.limit_by.is_empty()
            || q.for_clause.is_some()
            || q.settings.is_some()
            || q.format_clause.is_some()
        {
            return Err(Error("unsupported query clause".into()));
        }
        match q.body.as_ref() {
            SetExpr::Values(v) => {
                let width = v.rows.first().map_or(0, Vec::len);
                if v.rows.iter().any(|row| row.len() != width) {
                    return Err(Error("VALUES rows must have the same width".into()));
                }
                if q.order_by.is_some() {
                    return Err(Error("ORDER BY on standalone VALUES is unsupported".into()));
                }
                for row in &v.rows {
                    for expression in row {
                        bind(expression, &[], false)?;
                    }
                }
                let offset = q
                    .offset
                    .as_ref()
                    .map(|o| nonnegative(&o.value))
                    .transpose()?
                    .unwrap_or(0);
                let limit = q
                    .limit
                    .as_ref()
                    .map(nonnegative)
                    .transpose()?
                    .unwrap_or(usize::MAX);
                Ok(QueryResult {
                    columns: (1..=width).map(|i| format!("column{i}")).collect(),
                    affected: 0,
                    rows: v
                        .rows
                        .iter()
                        .skip(offset)
                        .take(limit)
                        .map(|r| r.iter().map(|e| eval(e, &[], &[])).collect())
                        .collect::<Result<_>>()?,
                })
            }
            SetExpr::Select(s) => {
                if s.top.is_some()
                    || s.into.is_some()
                    || s.prewhere.is_some()
                    || !s.lateral_views.is_empty()
                    || !s.cluster_by.is_empty()
                    || !s.distribute_by.is_empty()
                    || !s.sort_by.is_empty()
                    || !s.named_window.is_empty()
                    || s.qualify.is_some()
                    || s.connect_by.is_some()
                    || matches!(s.distinct, Some(Distinct::On(_)))
                {
                    return Err(Error("unsupported SELECT clause".into()));
                }
                let mut columns = vec![];
                let mut rows = vec![vec![]];
                for source in &s.from {
                    let (mut right_columns, mut right_rows) = self.relation(
                        &source.relation,
                        if s.from.len() == 1 && source.joins.is_empty() {
                            s.selection.as_ref()
                        } else {
                            None
                        },
                    )?;
                    for join in &source.joins {
                        let (join_columns, join_rows) = self.relation(&join.relation, None)?;
                        let constraint = match &join.join_operator {
                            JoinOperator::Inner(c) | JoinOperator::LeftOuter(c) => Some(c),
                            JoinOperator::CrossJoin => None,
                            _ => return Err(Error("unsupported join".into())),
                        };
                        let left_len = right_columns.len();
                        let combined_columns =
                            [right_columns.clone(), join_columns.clone()].concat();
                        match constraint {
                            Some(JoinConstraint::On(e)) => bind(e, &combined_columns, false)?,
                            Some(JoinConstraint::Using(_) | JoinConstraint::Natural) => {
                                return Err(Error(
                                    "JOIN USING and NATURAL JOIN are unsupported".into(),
                                ));
                            }
                            _ => {}
                        }
                        let mut combined_rows = vec![];
                        for left in &right_rows {
                            let mut matched = false;
                            for right in &join_rows {
                                let combined = [left.clone(), right.clone()].concat();
                                let selected = match constraint {
                                    Some(JoinConstraint::On(e)) => {
                                        selected(Some(e), &combined_columns, &combined)?
                                    }
                                    _ => true,
                                };
                                if selected {
                                    matched = true;
                                    combined_rows.push(combined);
                                }
                            }
                            if !matched && matches!(join.join_operator, JoinOperator::LeftOuter(_))
                            {
                                let mut combined = left.clone();
                                combined.resize(left_len + join_columns.len(), Value::Null);
                                combined_rows.push(combined);
                            }
                        }
                        right_columns = combined_columns;
                        right_rows = combined_rows;
                    }
                    rows = if columns.is_empty() && rows == [vec![]] {
                        right_rows
                    } else {
                        rows.iter()
                            .flat_map(|l| {
                                right_rows
                                    .iter()
                                    .map(move |r| [l.clone(), r.clone()].concat())
                            })
                            .collect()
                    };
                    columns.extend(right_columns);
                }
                let filtered = rows
                    .into_iter()
                    .filter_map(|row| match selected(s.selection.as_ref(), &columns, &row) {
                        Ok(true) => Some(Ok(row)),
                        Ok(false) => None,
                        Err(e) => Some(Err(e)),
                    })
                    .collect::<Result<Vec<_>>>()?;
                if let Some(e) = &s.selection {
                    bind(e, &columns, false)?;
                }
                let GroupByExpr::Expressions(group_exprs, modifiers) = &s.group_by else {
                    return Err(Error("GROUP BY ALL unsupported".into()));
                };
                if !modifiers.is_empty() {
                    return Err(Error("GROUP BY modifiers unsupported".into()));
                }
                for e in group_exprs {
                    bind(e, &columns, false)?;
                }
                for p in &s.projection {
                    if let SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } =
                        p
                    {
                        bind(e, &columns, true)?;
                    }
                }
                if let Some(e) = &s.having {
                    bind(e, &columns, true)?;
                }
                let aggregated = !group_exprs.is_empty()
                    || s.projection.iter().any(|p| match p {
                        SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                            has_aggregate(e)
                        }
                        _ => false,
                    })
                    || s.having.as_ref().is_some_and(has_aggregate);
                let groups = if !group_exprs.is_empty() {
                    let mut groups: Vec<Vec<Vec<Value>>> = vec![];
                    let mut positions: HashMap<IndexKey, usize> = HashMap::new();
                    let key_columns = (0..group_exprs.len()).collect::<Vec<_>>();
                    for row in filtered {
                        let key = group_exprs
                            .iter()
                            .map(|e| eval(e, &columns, &row))
                            .collect::<Result<Vec<_>>>()?;
                        let key = IndexKey::new(&key, &key_columns)?;
                        let position = *positions.entry(key).or_insert_with(|| {
                            groups.push(vec![]);
                            groups.len() - 1
                        });
                        groups[position].push(row);
                    }
                    groups
                } else if aggregated {
                    vec![filtered]
                } else {
                    filtered.into_iter().map(|row| vec![row]).collect()
                };
                let mut output = vec![];
                let output_columns = project_group(
                    &s.projection,
                    &columns,
                    &vec![Value::Null; columns.len()],
                    Some(&[]),
                )?
                .0;
                let projection = if aggregated {
                    None
                } else {
                    simple_projection(&s.projection, &columns)?
                };
                if let Some(order) = &q.order_by {
                    for o in &order.exprs {
                        match &o.expr {
                            Expr::Value(sqlparser::ast::Value::Number(n, _)) => {
                                if !n
                                    .parse::<usize>()
                                    .is_ok_and(|i| i > 0 && i <= output_columns.len())
                                {
                                    return Err(Error("ORDER BY position out of range".into()));
                                }
                            }
                            Expr::Identifier(n)
                                if output_columns
                                    .iter()
                                    .any(|c| c.eq_ignore_ascii_case(&n.value)) =>
                            {
                                bind(&o.expr, &output_columns, false)?;
                            }
                            _ => bind(&o.expr, &columns, true)?,
                        }
                    }
                }
                for group in &groups {
                    let blank = vec![Value::Null; columns.len()];
                    let row = group.first().unwrap_or(&blank);
                    let context = aggregated.then_some(group.as_slice());
                    if let Some(having) = &s.having
                        && crate::expression::truth(&eval_group(having, &columns, row, context)?)?
                            != Some(true)
                    {
                        continue;
                    }
                    let projected = if let Some(projection) = &projection {
                        projection.iter().map(|i| row[*i].clone()).collect()
                    } else {
                        project_group(&s.projection, &columns, row, context)?.1
                    };
                    let mut keys = vec![];
                    if let Some(order) = &q.order_by {
                        for o in &order.exprs {
                            let value = match &o.expr {
                                Expr::Value(sqlparser::ast::Value::Number(n, _)) => n
                                    .parse::<usize>()
                                    .ok()
                                    .filter(|i| *i > 0)
                                    .and_then(|i| projected.get(i - 1))
                                    .cloned()
                                    .ok_or_else(|| {
                                        Error("ORDER BY position out of range".into())
                                    })?,
                                Expr::Identifier(n)
                                    if output_columns
                                        .iter()
                                        .any(|c| c.eq_ignore_ascii_case(&n.value)) =>
                                {
                                    eval(&o.expr, &output_columns, &projected)?
                                }
                                _ => eval_group(&o.expr, &columns, row, context)?,
                            };
                            keys.push(value);
                        }
                    }
                    output.push((keys, projected));
                }
                if let Some(order) = &q.order_by {
                    let mut failure = None;
                    output.sort_by(|(a, _), (b, _)| {
                        for ((a, b), o) in a.iter().zip(b).zip(&order.exprs) {
                            let mut c = match crate::expression::compare(a, b) {
                                Ok(c) => c,
                                Err(e) => {
                                    failure = Some(e);
                                    return std::cmp::Ordering::Equal;
                                }
                            };
                            if o.asc == Some(false) {
                                c = c.reverse();
                            }
                            if (a == &Value::Null || b == &Value::Null)
                                && a != b
                                && let Some(first) = o.nulls_first
                            {
                                c = if (a == &Value::Null) == first {
                                    std::cmp::Ordering::Less
                                } else {
                                    std::cmp::Ordering::Greater
                                };
                            }
                            if !c.is_eq() {
                                return c;
                            }
                        }
                        std::cmp::Ordering::Equal
                    });
                    if let Some(e) = failure {
                        return Err(e);
                    }
                }
                if s.distinct.is_some() {
                    let mut seen = vec![];
                    output.retain(|(_, row)| {
                        if seen.contains(row) {
                            false
                        } else {
                            seen.push(row.clone());
                            true
                        }
                    });
                }
                let offset = q
                    .offset
                    .as_ref()
                    .map(|o| nonnegative(&o.value))
                    .transpose()?
                    .unwrap_or(0);
                let limit = q
                    .limit
                    .as_ref()
                    .map(nonnegative)
                    .transpose()?
                    .unwrap_or(usize::MAX);
                Ok(QueryResult {
                    columns: output_columns,
                    rows: output
                        .into_iter()
                        .skip(offset)
                        .take(limit)
                        .map(|r| r.1)
                        .collect(),
                    affected: 0,
                })
            }
            SetExpr::Query(inner) => self.query(inner),
            SetExpr::SetOperation {
                op,
                set_quantifier,
                left,
                right,
            } => {
                if !matches!(
                    set_quantifier,
                    SetQuantifier::None | SetQuantifier::All | SetQuantifier::Distinct
                ) || (*op != SetOperator::Union && *set_quantifier == SetQuantifier::All)
                {
                    return Err(Error("unsupported compound query quantifier".into()));
                }
                let mut sub = q.clone();
                sub.order_by = None;
                sub.limit = None;
                sub.offset = None;
                sub.body = left.clone();
                let mut a = self.query(&sub)?;
                sub.body = right.clone();
                let b = self.query(&sub)?;
                if a.columns.len() != b.columns.len() {
                    return Err(Error("compound query column count mismatch".into()));
                }
                match op {
                    SetOperator::Union => a.rows.extend(b.rows),
                    SetOperator::Intersect => a.rows.retain(|r| b.rows.contains(r)),
                    SetOperator::Except => a.rows.retain(|r| !b.rows.contains(r)),
                }
                if !matches!(set_quantifier, SetQuantifier::All) {
                    let mut seen = vec![];
                    a.rows.retain(|r| {
                        if seen.contains(r) {
                            false
                        } else {
                            seen.push(r.clone());
                            true
                        }
                    });
                }
                if let Some(order) = &q.order_by {
                    let mut failure = None;
                    a.rows.sort_by(|a_row, b_row| {
                        for o in &order.exprs {
                            let get = |row: &[Value]| -> Result<Value> {
                                if let Expr::Value(sqlparser::ast::Value::Number(n, _)) = &o.expr {
                                    return n
                                        .parse::<usize>()
                                        .ok()
                                        .filter(|i| *i > 0)
                                        .and_then(|i| row.get(i - 1))
                                        .cloned()
                                        .ok_or_else(|| {
                                            Error("ORDER BY position out of range".into())
                                        });
                                }
                                if let Expr::CompoundIdentifier(ids) = &o.expr {
                                    return lookup(&ids.last().unwrap().value, &a.columns, row);
                                }
                                eval(&o.expr, &a.columns, row)
                            };
                            let c = get(a_row).and_then(|a| {
                                get(b_row).and_then(|b| crate::expression::compare(&a, &b))
                            });
                            match c {
                                Ok(mut c) => {
                                    if o.asc == Some(false) {
                                        c = c.reverse();
                                    }
                                    if !c.is_eq() {
                                        return c;
                                    }
                                }
                                Err(e) => {
                                    failure = Some(e);
                                    return std::cmp::Ordering::Equal;
                                }
                            }
                        }
                        std::cmp::Ordering::Equal
                    });
                    if let Some(e) = failure {
                        return Err(e);
                    }
                }
                let offset = q
                    .offset
                    .as_ref()
                    .map(|o| nonnegative(&o.value))
                    .transpose()?
                    .unwrap_or(0);
                let limit = q
                    .limit
                    .as_ref()
                    .map(nonnegative)
                    .transpose()?
                    .unwrap_or(usize::MAX);
                a.rows = a.rows.into_iter().skip(offset).take(limit).collect();
                Ok(a)
            }
            _ => Err(Error("unsupported query".into())),
        }
    }
    fn relation(
        &self,
        factor: &TableFactor,
        selection: Option<&Expr>,
    ) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        match factor {
            TableFactor::Table {
                name: n,
                alias,
                args,
                ..
            } => {
                if args.is_some() {
                    return Err(Error("table functions unsupported".into()));
                }
                if matches!(name(n).as_str(), "sqlite_schema" | "sqlite_master") {
                    let prefix = alias
                        .as_ref()
                        .map(|a| a.name.value.to_lowercase())
                        .unwrap_or(name(n));
                    let columns = ["type", "name", "tbl_name", "sql"]
                        .iter()
                        .map(|n| format!("{prefix}.{n}"))
                        .collect();
                    let mut rows = self
                        .tables
                        .iter()
                        .map(|(n, t)| {
                            vec![
                                Value::Text("table".into()),
                                Value::Text(n.clone()),
                                Value::Text(n.clone()),
                                Value::Text(t.sql.clone()),
                            ]
                        })
                        .collect::<Vec<_>>();
                    rows.extend(self.indexes.iter().map(|(n, i)| {
                        vec![
                            Value::Text("index".into()),
                            Value::Text(n.clone()),
                            Value::Text(i.table.clone()),
                            Value::Text(i.sql.clone()),
                        ]
                    }));
                    return Ok((columns, rows));
                }
                let t = self
                    .tables
                    .get(&name(n))
                    .ok_or_else(|| Error(format!("no such table: {n}")))?;
                let prefix = alias
                    .as_ref()
                    .map(|a| a.name.value.to_lowercase())
                    .unwrap_or(name(n));
                let positions = self.index_positions(&name(n), &prefix, selection)?;
                let rows = positions
                    .map(|positions| positions.into_iter().map(|i| t.rows[i].clone()).collect())
                    .unwrap_or_else(|| t.rows.iter().cloned().collect());
                Ok((
                    t.columns
                        .iter()
                        .map(|c| format!("{prefix}.{}", c.name))
                        .collect(),
                    rows,
                ))
            }
            TableFactor::Derived {
                subquery, alias, ..
            } => {
                let r = self.query(subquery)?;
                let prefix = alias
                    .as_ref()
                    .map(|a| format!("{}.", a.name.value.to_lowercase()))
                    .unwrap_or_default();
                Ok((
                    r.columns.iter().map(|c| format!("{prefix}{c}")).collect(),
                    r.rows,
                ))
            }
            _ => Err(Error("unsupported FROM relation".into())),
        }
    }
}
fn nonnegative(e: &Expr) -> Result<usize> {
    match eval(e, &[], &[])? {
        Value::Integer(n) if n >= 0 => {
            usize::try_from(n).map_err(|_| Error("limit overflow".into()))
        }
        _ => Err(Error("nonnegative integer required".into())),
    }
}
fn selected(e: Option<&Expr>, columns: &[String], row: &[Value]) -> Result<bool> {
    e.map(|e| crate::expression::truth(&eval(e, columns, row)?).map(|v| v == Some(true)))
        .unwrap_or(Ok(true))
}
fn project(
    items: &[SelectItem],
    columns: &[String],
    row: &[Value],
) -> Result<(Vec<String>, Vec<Value>)> {
    project_group(items, columns, row, None)
}
fn project_group(
    items: &[SelectItem],
    columns: &[String],
    row: &[Value],
    group: Option<&[Vec<Value>]>,
) -> Result<(Vec<String>, Vec<Value>)> {
    let mut names = vec![];
    let mut values = vec![];
    for item in items {
        match item {
            SelectItem::Wildcard(_) => {
                names.extend(
                    columns
                        .iter()
                        .map(|c| c.rsplit('.').next().unwrap().to_string()),
                );
                values.extend_from_slice(row);
            }
            SelectItem::UnnamedExpr(e) => {
                names.push(match e {
                    Expr::Identifier(i) => i.value.clone(),
                    Expr::CompoundIdentifier(ids) => ids.last().unwrap().value.clone(),
                    _ => e.to_string(),
                });
                values.push(eval_group(e, columns, row, group)?);
            }
            SelectItem::ExprWithAlias { expr, alias } => {
                names.push(alias.value.clone());
                values.push(eval_group(expr, columns, row, group)?);
            }
            SelectItem::QualifiedWildcard(prefix, _) => {
                let prefix = format!("{}.", name(prefix));
                for (c, v) in columns.iter().zip(row) {
                    if c.starts_with(&prefix) {
                        names.push(c.rsplit('.').next().unwrap().to_string());
                        values.push(v.clone());
                    }
                }
            }
        }
    }
    Ok((names, values))
}
pub(crate) fn eval(e: &Expr, columns: &[String], row: &[Value]) -> Result<Value> {
    use crate::expression::{binary, truth};
    match e {
        Expr::Value(sqlparser::ast::Value::Number(n, _)) => {
            if let Ok(n) = n.parse::<i64>() {
                Ok(Value::Integer(n))
            } else {
                let ty = Type::Numeric(None);
                ty.cast(Value::Text(n.clone()))
            }
        }
        Expr::Value(sqlparser::ast::Value::Boolean(b)) => Ok(Value::Boolean(*b)),
        Expr::Value(sqlparser::ast::Value::HexStringLiteral(s)) => hex::decode(s)
            .map(Value::Bytea)
            .map_err(|e| Error(e.to_string())),
        Expr::Value(sqlparser::ast::Value::SingleQuotedString(s)) => Ok(Value::Text(s.clone())),
        Expr::Value(sqlparser::ast::Value::Null) => Ok(Value::Null),
        Expr::Cast {
            expr,
            data_type,
            kind,
            format,
        } => {
            if !matches!(kind, CastKind::Cast | CastKind::DoubleColon) || format.is_some() {
                return Err(Error("unsupported cast kind or format".into()));
            }
            Type::parse(&data_type.to_string())?.cast(eval(expr, columns, row)?)
        }
        Expr::TypedString { data_type, value } => {
            Type::parse(&data_type.to_string())?.cast(Value::Text(value.clone()))
        }
        Expr::Array(array) => Ok(Value::Array(
            array
                .elem
                .iter()
                .map(|e| eval(e, columns, row))
                .collect::<Result<_>>()?,
        )),
        Expr::Nested(e) => eval(e, columns, row),
        Expr::BinaryOp { left, op, right } => {
            binary(eval(left, columns, row)?, op, eval(right, columns, row)?)
        }
        Expr::UnaryOp { op, expr } => {
            if matches!(op, UnaryOperator::Minus)
                && let Expr::Value(sqlparser::ast::Value::Number(n, _)) = expr.as_ref()
                && let Ok(n) = format!("-{n}").parse::<i64>()
            {
                return Ok(Value::Integer(n));
            }
            let v = eval(expr, columns, row)?;
            if v == Value::Null {
                return Ok(v);
            }
            match op {
                UnaryOperator::Not => Ok(Value::Boolean(!truth(&v)?.unwrap())),
                UnaryOperator::Plus => Ok(v),
                UnaryOperator::Minus => binary(Value::Integer(0), &BinaryOperator::Minus, v),
                UnaryOperator::PGBitwiseNot => {
                    if let Value::Integer(n) = v {
                        Ok(Value::Integer(!n))
                    } else {
                        Err(Error("bitwise NOT requires integer".into()))
                    }
                }
                _ => Err(Error(format!("unsupported unary operator: {op}"))),
            }
        }
        Expr::IsNull(e) => Ok(Value::Boolean(eval(e, columns, row)? == Value::Null)),
        Expr::IsNotNull(e) => Ok(Value::Boolean(eval(e, columns, row)? != Value::Null)),
        Expr::IsDistinctFrom(a, b) | Expr::IsNotDistinctFrom(a, b) => {
            let (a, b) = (eval(a, columns, row)?, eval(b, columns, row)?);
            let equal = if a == Value::Null || b == Value::Null {
                a == b
            } else {
                crate::expression::compare(&a, &b)?.is_eq()
            };
            Ok(Value::Boolean(if matches!(e, Expr::IsDistinctFrom(..)) {
                !equal
            } else {
                equal
            }))
        }
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => {
            let v = eval(expr, columns, row)?;
            let a = binary(v.clone(), &BinaryOperator::GtEq, eval(low, columns, row)?)?;
            let b = binary(v, &BinaryOperator::LtEq, eval(high, columns, row)?)?;
            let result = binary(a, &BinaryOperator::And, b)?;
            Ok(if *negated {
                truth(&result)?
                    .map(|b| Value::Boolean(!b))
                    .unwrap_or(Value::Null)
            } else {
                result
            })
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            let v = eval(expr, columns, row)?;
            let mut has_null = v == Value::Null;
            for e in list {
                match binary(v.clone(), &BinaryOperator::Eq, eval(e, columns, row)?)? {
                    Value::Boolean(true) => return Ok(Value::Boolean(!negated)),
                    Value::Null => has_null = true,
                    _ => (),
                }
            }
            Ok(if has_null {
                Value::Null
            } else {
                Value::Boolean(*negated)
            })
        }
        Expr::Case {
            operand,
            conditions,
            results,
            else_result,
        } => {
            for (c, r) in conditions.iter().zip(results) {
                let condition = if let Some(o) = operand {
                    binary(
                        eval(o, columns, row)?,
                        &BinaryOperator::Eq,
                        eval(c, columns, row)?,
                    )?
                } else {
                    eval(c, columns, row)?
                };
                if truth(&condition)? == Some(true) {
                    return eval(r, columns, row);
                }
            }
            else_result
                .as_ref()
                .map(|e| eval(e, columns, row))
                .unwrap_or(Ok(Value::Null))
        }
        Expr::Function(f) => {
            if f.filter.is_some() || f.over.is_some() || !f.within_group.is_empty() {
                return Err(Error("unsupported function clause".into()));
            }
            let FunctionArguments::List(args) = &f.args else {
                return Err(Error("unsupported function arguments".into()));
            };
            if !args.clauses.is_empty() || args.duplicate_treatment.is_some() {
                return Err(Error("unsupported scalar function clause".into()));
            }
            let args = args
                .args
                .iter()
                .map(|a| match a {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => eval(e, columns, row),
                    _ => Err(Error("unsupported function argument".into())),
                })
                .collect::<Result<Vec<_>>>()?;
            match name(&f.name).as_str() {
                "coalesce" | "ifnull" => Ok(args
                    .into_iter()
                    .find(|a| *a != Value::Null)
                    .unwrap_or(Value::Null)),
                "min" | "max" if args.len() >= 2 => {
                    if args.contains(&Value::Null) {
                        return Ok(Value::Null);
                    }
                    let mut result = args[0].clone();
                    for value in &args[1..] {
                        let c = crate::expression::compare(value, &result)?;
                        if (name(&f.name) == "min" && c.is_lt())
                            || (name(&f.name) == "max" && c.is_gt())
                        {
                            result = value.clone();
                        }
                    }
                    Ok(result)
                }
                _ => Err(Error(format!("unsupported function: {}", f.name))),
            }
        }
        Expr::Identifier(i) => lookup(&i.value, columns, row),
        Expr::CompoundIdentifier(ids) => lookup(
            &ids.iter()
                .map(|i| i.value.as_str())
                .collect::<Vec<_>>()
                .join("."),
            columns,
            row,
        ),
        _ => Err(Error(format!("unsupported expression: {e}"))),
    }
}
fn lookup_index(n: &str, columns: &[String]) -> Result<usize> {
    let mut indexes = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            if n.contains('.') {
                c.eq_ignore_ascii_case(n)
            } else {
                c.rsplit('.').next().unwrap().eq_ignore_ascii_case(n)
            }
        })
        .map(|(i, _)| i);
    match (indexes.next(), indexes.next()) {
        (Some(i), None) => Ok(i),
        (None, _) => Err(Error(format!("no such column: {n}"))),
        _ => Err(Error(format!("ambiguous column: {n}"))),
    }
}
fn lookup(n: &str, columns: &[String], row: &[Value]) -> Result<Value> {
    Ok(row[lookup_index(n, columns)?].clone())
}

// Bind direct column projections once; the row loop only copies result cells.
fn simple_projection(items: &[SelectItem], columns: &[String]) -> Result<Option<Vec<usize>>> {
    let mut projection = Vec::new();
    for item in items {
        match item {
            SelectItem::Wildcard(_) => projection.extend(0..columns.len()),
            SelectItem::QualifiedWildcard(prefix, _) => {
                let prefix = format!("{}.", name(prefix));
                projection.extend(
                    columns
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.starts_with(&prefix))
                        .map(|(i, _)| i),
                );
            }
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                let column = match expr {
                    Expr::Identifier(n) => n.value.clone(),
                    Expr::CompoundIdentifier(ids) => ids
                        .iter()
                        .map(|n| n.value.as_str())
                        .collect::<Vec<_>>()
                        .join("."),
                    _ => return Ok(None),
                };
                projection.push(lookup_index(&column, columns)?);
            }
        }
    }
    Ok(Some(projection))
}
fn reference_columns(table: &Table, names: &[String]) -> Result<Vec<usize>> {
    if names.is_empty() {
        return table
            .constraints
            .iter()
            .find_map(|c| {
                if let Constraint::Unique {
                    columns,
                    primary: true,
                } = c
                {
                    Some(columns.clone())
                } else {
                    None
                }
            })
            .ok_or_else(|| Error("referenced table has no primary key".into()));
    }
    names
        .iter()
        .map(|n| {
            table
                .columns
                .iter()
                .position(|c| c.name == *n)
                .ok_or_else(|| Error("foreign key references missing column".into()))
        })
        .collect()
}
fn bind(e: &Expr, columns: &[String], allow_aggregate: bool) -> Result<()> {
    let blank = vec![Value::Null; columns.len()];
    let result = visit_expressions(e, |e| {
        let check = match e {
            Expr::Identifier(n) => lookup(&n.value, columns, &blank).map(|_| ()),
            Expr::CompoundIdentifier(ids) => lookup(
                &ids.iter()
                    .map(|i| i.value.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
                columns,
                &blank,
            )
            .map(|_| ()),
            Expr::Function(f) => {
                if f.filter.is_some()
                    || f.over.is_some()
                    || !f.within_group.is_empty()
                    || !matches!(f.parameters, FunctionArguments::None)
                    || matches!(&f.args,FunctionArguments::List(args) if !args.clauses.is_empty())
                {
                    Err(Error("unsupported function clause".into()))
                } else if !allow_aggregate && aggregate_function(f) {
                    Err(Error("aggregate not permitted here".into()))
                } else if !matches!(
                    name(&f.name).as_str(),
                    "coalesce" | "ifnull" | "min" | "max" | "count" | "sum" | "avg" | "total"
                ) {
                    Err(Error(format!("unsupported function: {}", f.name)))
                } else {
                    Ok(())
                }
            }
            Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
                Err(Error("subqueries in expressions are unsupported".into()))
            }
            Expr::Value(_)
            | Expr::Nested(_)
            | Expr::BinaryOp { .. }
            | Expr::UnaryOp { .. }
            | Expr::IsNull(_)
            | Expr::IsNotNull(_)
            | Expr::IsDistinctFrom(..)
            | Expr::IsNotDistinctFrom(..)
            | Expr::Between { .. }
            | Expr::InList { .. }
            | Expr::Case { .. }
            | Expr::Array(_)
            | Expr::Cast { .. }
            | Expr::TypedString { .. } => Ok(()),
            _ => Err(Error(format!("unsupported expression: {e}"))),
        };
        match check {
            Ok(()) => std::ops::ControlFlow::Continue(()),
            Err(e) => std::ops::ControlFlow::Break(e),
        }
    });
    match result {
        std::ops::ControlFlow::Continue(()) => Ok(()),
        std::ops::ControlFlow::Break(e) => Err(e),
    }
}
fn aggregate_function(f: &Function) -> bool {
    let FunctionArguments::List(args) = &f.args else {
        return false;
    };
    matches!(name(&f.name).as_str(), "count" | "sum" | "total" | "avg")
        || (matches!(name(&f.name).as_str(), "min" | "max") && args.args.len() == 1)
}
fn has_aggregate(e: &Expr) -> bool {
    matches!(
        visit_expressions(e, |e| {
            if matches!(e,Expr::Function(f) if aggregate_function(f)) {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        }),
        std::ops::ControlFlow::Break(())
    )
}
fn eval_group(
    e: &Expr,
    columns: &[String],
    row: &[Value],
    group: Option<&[Vec<Value>]>,
) -> Result<Value> {
    let Some(group) = group else {
        return eval(e, columns, row);
    };
    struct Fold<'a> {
        columns: &'a [String],
        group: &'a [Vec<Value>],
    }
    impl VisitorMut for Fold<'_> {
        type Break = Error;
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<Error> {
            if let Expr::Function(f) = expr
                && aggregate_function(f)
            {
                match aggregate(f, self.columns, self.group) {
                    Ok(v) => *expr = value_expression(&v),
                    Err(e) => return std::ops::ControlFlow::Break(e),
                }
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut e = e.clone();
    if let std::ops::ControlFlow::Break(error) =
        VisitMut::visit(&mut e, &mut Fold { columns, group })
    {
        return Err(error);
    }
    eval(&e, columns, row)
}
pub(crate) fn value_expression(v: &Value) -> Expr {
    use sqlparser::ast::Value as SqlValue;
    if matches!(v, Value::Real(_) | Value::Numeric(_)) {
        return Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(Expr::Value(SqlValue::SingleQuotedString(v.to_string()))),
            data_type: if matches!(v, Value::Real(_)) {
                DataType::Double
            } else {
                DataType::Numeric(ExactNumberInfo::None)
            },
            format: None,
        };
    }
    if let Some(ty) = match v {
        Value::Date(_) => Some("DATE"),
        Value::Time(_) => Some("TIME"),
        Value::Timestamp(_) => Some("TIMESTAMP"),
        Value::Timestamptz(_) => Some("TIMESTAMPTZ"),
        Value::Uuid(_) => Some("UUID"),
        Value::Json(_) => Some("JSON"),
        _ => None,
    } {
        return Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(Expr::Value(SqlValue::SingleQuotedString(v.to_string()))),
            data_type: DataType::Custom(ObjectName(vec![Ident::new(ty)]), vec![]),
            format: None,
        };
    }
    if let Value::Bytea(bytes) = v {
        return Expr::Value(SqlValue::HexStringLiteral(hex::encode(bytes)));
    }
    if let Value::Array(values) = v {
        return Expr::Array(Array {
            elem: values.iter().map(value_expression).collect(),
            named: true,
        });
    }
    Expr::Value(match v {
        Value::Null => SqlValue::Null,
        Value::Boolean(b) => SqlValue::Boolean(*b),
        Value::Integer(_) | Value::Real(_) | Value::Numeric(_) => {
            SqlValue::Number(v.to_string(), false)
        }
        _ => SqlValue::SingleQuotedString(v.to_string()),
    })
}
fn aggregate(f: &Function, columns: &[String], group: &[Vec<Value>]) -> Result<Value> {
    if f.filter.is_some() || f.over.is_some() || !f.within_group.is_empty() {
        return Err(Error("unsupported aggregate clause".into()));
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err(Error("invalid aggregate".into()));
    };
    if !args.clauses.is_empty() {
        return Err(Error("unsupported aggregate argument clause".into()));
    }
    let function = name(&f.name);
    if args.args.len() > 1 || (args.args.is_empty() && function != "count") {
        return Err(Error("invalid aggregate arity".into()));
    }
    let expr = match args.args.first() {
        Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(e))) => Some(e),
        Some(FunctionArg::Unnamed(FunctionArgExpr::Wildcard)) if function == "count" => None,
        None if function == "count" => None,
        _ => return Err(Error("invalid aggregate argument".into())),
    };
    let mut values = group
        .iter()
        .map(|row| {
            expr.map(|e| eval(e, columns, row))
                .unwrap_or(Ok(Value::Integer(1)))
        })
        .collect::<Result<Vec<_>>>()?;
    values.retain(|v| *v != Value::Null);
    if matches!(args.duplicate_treatment, Some(DuplicateTreatment::Distinct)) {
        let mut seen = vec![];
        values.retain(|v| {
            if seen.contains(v) {
                false
            } else {
                seen.push(v.clone());
                true
            }
        });
    }
    if function == "count" {
        return Ok(Value::Integer(values.len() as i64));
    }
    if values.is_empty() {
        return Ok(if function == "total" {
            Value::Real(0.0)
        } else {
            Value::Null
        });
    }
    if matches!(function.as_str(), "min" | "max") {
        let mut result = values[0].clone();
        for v in &values[1..] {
            let c = crate::expression::compare(v, &result)?;
            if (function == "min" && c.is_lt()) || (function == "max" && c.is_gt()) {
                result = v.clone();
            }
        }
        return Ok(result);
    }
    let count = values.len();
    let mut sum = Value::Integer(0);
    for value in values {
        sum = crate::expression::binary(sum, &BinaryOperator::Plus, value)?;
    }
    if function == "avg" && matches!(sum, Value::Numeric(_)) {
        return crate::expression::binary(
            sum,
            &BinaryOperator::Divide,
            Value::Numeric(count.to_string()),
        );
    }
    if function == "avg" || function == "total" {
        let n: f64 = sum
            .to_string()
            .parse()
            .map_err(|_| Error("numeric aggregate required".into()))?;
        return Ok(Value::Real(if function == "avg" {
            n / count as f64
        } else {
            n
        }));
    }
    Ok(sum)
}
