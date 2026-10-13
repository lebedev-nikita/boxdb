use crate::{Error, QueryResult, Result, Type, Value};
use serde::{Deserialize, Serialize};
use sqlparser::ast::helpers::stmt_create_table::CreateTableBuilder;
use sqlparser::ast::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct State {
    pub tables: BTreeMap<String, Table>,
    pub indexes: BTreeMap<String, Index>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Index {
    pub table: String,
    pub columns: Vec<usize>,
    pub unique: bool,
    pub sql: String,
    #[serde(skip)]
    pub entries: BTreeMap<String, Vec<usize>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Table {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    pub constraints: Vec<Constraint>,
    pub row_ids: Vec<u64>,
    pub next_rowid: u64,
    pub sql: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Column {
    pub name: String,
    pub ty: Type,
    pub not_null: bool,
    pub default: Option<Expr>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
                        constraints,
                        rows: vec![],
                        row_ids: vec![],
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
                        row[index] = table.columns[index].ty.validate(&value)?;
                    }
                    if let Some(index) = table.constraints.iter().find_map(|c| {
                        if let Constraint::Unique {
                            columns,
                            primary: true,
                        } = c
                        {
                            (columns.len() == 1 && table.columns[columns[0]].ty == Type::Integer)
                                .then_some(columns[0])
                        } else {
                            None
                        }
                    }) && row[index] == Value::Null
                    {
                        let previous = table
                            .rows
                            .iter()
                            .filter_map(|row| {
                                if let Value::Integer(n) = row[index] {
                                    Some(n)
                                } else {
                                    None
                                }
                            })
                            .max()
                            .unwrap_or(0);
                        row[index] = table.columns[index].ty.validate(&Value::Integer(
                            previous
                                .checked_add(1)
                                .ok_or_else(|| Error("primary key exhausted".into()))?,
                        ))?;
                    }
                    if let Some(items) = &i.returning {
                        returned.push(project(items, &names, &row)?.1);
                    }
                    table.rows.push(row);
                    table.row_ids.push(table.next_rowid);
                    table.next_rowid = table
                        .next_rowid
                        .checked_add(1)
                        .ok_or_else(|| Error("row id exhausted".into()))?;
                }
                self.validate()?;
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
                for row in &mut t.rows {
                    if !selected(selection.as_ref(), &names, row)? {
                        continue;
                    }
                    let old = row.clone();
                    for a in assignments {
                        let AssignmentTarget::ColumnName(n) = &a.target else {
                            return Err(Error("tuple assignments unsupported".into()));
                        };
                        let index = names
                            .iter()
                            .position(|c| *c == name(n))
                            .ok_or_else(|| Error(format!("no such column: {n}")))?;
                        row[index] = t.columns[index]
                            .ty
                            .validate(&eval(&a.value, &names, &old)?)?;
                    }
                    affected += 1;
                    if old != *row {
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
                self.validate()?;
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
                let t = self
                    .tables
                    .get_mut(&name(n))
                    .ok_or_else(|| Error(format!("no such table: {n}")))?;
                let names = t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
                if let Some(e) = &d.selection {
                    bind(e, &names, false)?;
                }
                let mut remaining = vec![];
                let mut remaining_ids = vec![];
                let mut returned = vec![];
                let mut affected = 0;
                let mut changes = vec![];
                for (id, row) in t.row_ids.iter().zip(&t.rows) {
                    if selected(d.selection.as_ref(), &names, row)? {
                        affected += 1;
                        changes.push((name(n), row.clone(), None));
                        if let Some(p) = &d.returning {
                            returned.push(project(p, &names, row)?.1);
                        }
                    } else {
                        remaining.push(row.clone());
                        remaining_ids.push(*id);
                    }
                }
                t.rows = remaining;
                t.row_ids = remaining_ids;
                let columns = d
                    .returning
                    .as_ref()
                    .map(|p| project(p, &names, &vec![Value::Null; names.len()]).map(|r| r.0))
                    .transpose()?
                    .unwrap_or_default();
                self.cascade(changes)?;
                self.validate()?;
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
                        entries: BTreeMap::new(),
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
                for position in (0..table.rows.len()).rev() {
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
                        table.row_ids.remove(position);
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
        use crate::expression::truth;
        for (table_name, table) in &self.tables {
            let names = table
                .columns
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>();
            if table.columns.is_empty()
                || table.next_rowid == 0
                || table.row_ids.len() != table.rows.len()
                || table.row_ids.windows(2).any(|w| w[0] >= w[1])
                || table
                    .row_ids
                    .last()
                    .is_some_and(|id| *id >= table.next_rowid)
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
            for (row_index, row) in table.rows.iter().enumerate() {
                if row.len() != table.columns.len() {
                    return Err(Error("column count mismatch".into()));
                }
                for (column, value) in table.columns.iter().zip(row) {
                    column.ty.validate(value)?;
                    if column.not_null && *value == Value::Null {
                        return Err(Error(format!(
                            "NOT NULL constraint failed: {table_name}.{}",
                            column.name
                        )));
                    }
                }
                for constraint in &table.constraints {
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
                            if table.rows[..row_index]
                                .iter()
                                .any(|other| columns.iter().all(|i| row[*i] == other[*i]))
                            {
                                return Err(Error(format!(
                                    "UNIQUE constraint failed: {table_name}"
                                )));
                            }
                        }
                        Constraint::Foreign {
                            columns,
                            table: target_table,
                            target,
                            ..
                        } => {
                            let target_table = self.tables.get(target_table).ok_or_else(|| {
                                Error("foreign key references missing table".into())
                            })?;
                            let target_indexes = reference_columns(target_table, target)?;
                            if columns.len() != target_indexes.len() {
                                return Err(Error("foreign key column count mismatch".into()));
                            }
                            if columns.iter().any(|i| row[*i] == Value::Null) {
                                continue;
                            }
                            if !target_table.rows.iter().any(|other| {
                                columns
                                    .iter()
                                    .zip(&target_indexes)
                                    .all(|(a, b)| equivalent(&row[*a], &other[*b]))
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
            if index.unique {
                let mut seen = vec![];
                for row in &table.rows {
                    let key = index
                        .columns
                        .iter()
                        .map(|i| row[*i].clone())
                        .collect::<Vec<_>>();
                    if key.contains(&Value::Null) {
                        continue;
                    }
                    if seen.contains(&key) {
                        return Err(Error(format!("UNIQUE index constraint failed: {name}")));
                    }
                    seen.push(key);
                }
            }
        }
        Ok(())
    }
    pub fn rebuild_indexes(&mut self) -> Result<()> {
        for index in self.indexes.values_mut() {
            index.entries.clear();
            for (position, row) in self.tables[&index.table].rows.iter().enumerate() {
                let key = index
                    .columns
                    .iter()
                    .map(|i| row[*i].clone())
                    .collect::<Vec<_>>();
                index
                    .entries
                    .entry(serde_json::to_string(&key).map_err(|e| Error(e.to_string()))?)
                    .or_default()
                    .push(position);
            }
        }
        Ok(())
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
                    rows = rows
                        .iter()
                        .flat_map(|l| {
                            right_rows
                                .iter()
                                .map(move |r| [l.clone(), r.clone()].concat())
                        })
                        .collect();
                    columns.extend(right_columns);
                }
                let filtered = rows
                    .iter()
                    .filter_map(|row| match selected(s.selection.as_ref(), &columns, row) {
                        Ok(true) => Some(Ok(row.clone())),
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
                    let mut groups: Vec<(Vec<Value>, Vec<Vec<Value>>)> = vec![];
                    for row in filtered {
                        let key = group_exprs
                            .iter()
                            .map(|e| eval(e, &columns, &row))
                            .collect::<Result<Vec<_>>>()?;
                        if let Some((_, rows)) = groups.iter_mut().find(|(k, _)| *k == key) {
                            rows.push(row);
                        } else {
                            groups.push((key, vec![row]));
                        }
                    }
                    groups.into_iter().map(|(_, rows)| rows).collect::<Vec<_>>()
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
                    let projected = project_group(&s.projection, &columns, row, context)?.1;
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
                let mut positions = None;
                if let Some(Expr::BinaryOp {
                    left,
                    op: BinaryOperator::Eq,
                    right,
                }) = selection
                {
                    for (column, value) in [
                        (left.as_ref(), right.as_ref()),
                        (right.as_ref(), left.as_ref()),
                    ] {
                        let column = match column {
                            Expr::Identifier(i) => Some(i.value.to_lowercase()),
                            Expr::CompoundIdentifier(ids)
                                if ids.len() == 2 && ids[0].value.eq_ignore_ascii_case(&prefix) =>
                            {
                                Some(ids[1].value.to_lowercase())
                            }
                            _ => None,
                        };
                        if let Some(column) = column
                            && let Some(column) = t.columns.iter().position(|c| c.name == column)
                            && let Ok(value) = eval(value, &[], &[])
                                .and_then(|v| t.columns[column].ty.validate(&v))
                            && let Some(index) = self
                                .indexes
                                .values()
                                .find(|i| i.table == name(n) && i.columns == [column])
                        {
                            let key = serde_json::to_string(&vec![value])
                                .map_err(|e| Error(e.to_string()))?;
                            positions = Some(index.entries.get(&key).cloned().unwrap_or_default());
                            break;
                        }
                    }
                }
                let rows = positions
                    .map(|positions| positions.into_iter().map(|i| t.rows[i].clone()).collect())
                    .unwrap_or_else(|| t.rows.clone());
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
fn lookup(n: &str, columns: &[String], row: &[Value]) -> Result<Value> {
    let indexes = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            if n.contains('.') {
                c.eq_ignore_ascii_case(n)
            } else {
                c.rsplit('.').next().unwrap().eq_ignore_ascii_case(n)
            }
        })
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    match indexes.as_slice() {
        [i] => Ok(row[*i].clone()),
        [] => Err(Error(format!("no such column: {n}"))),
        _ => Err(Error(format!("ambiguous column: {n}"))),
    }
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
