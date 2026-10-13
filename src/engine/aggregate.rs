//! Streaming integer GROUP BY / COUNT / SUM without copying source rows.
use super::*;

enum Item {
    Group,
    Count(Option<usize>),
    Sum(usize),
}
pub(crate) struct AggregatePlan {
    table: String,
    group: Option<usize>,
    items: Vec<Item>,
    columns: Vec<String>,
    order: Vec<(usize, OrderByExpr)>,
    offset: usize,
    limit: usize,
}
fn column(expr: &Expr, source: &[String]) -> Result<Option<usize>> {
    match expr {
        Expr::Identifier(n) => lookup_index(&n.value, source).map(Some),
        Expr::CompoundIdentifier(ids) => lookup_index(
            &ids.iter()
                .map(|n| n.value.as_str())
                .collect::<Vec<_>>()
                .join("."),
            source,
        )
        .map(Some),
        _ => Ok(None),
    }
}
impl State {
    pub(super) fn prepare_aggregate(&self, statement: &Statement) -> Result<Option<AggregatePlan>> {
        let Statement::Query(q) = statement else {
            return Ok(None);
        };
        let SetExpr::Select(s) = q.body.as_ref() else {
            return Ok(None);
        };
        if q.with.is_some()
            || !q.locks.is_empty()
            || q.fetch.is_some()
            || !q.limit_by.is_empty()
            || q.for_clause.is_some()
            || q.settings.is_some()
            || q.format_clause.is_some()
            || s.top.is_some()
            || s.into.is_some()
            || s.prewhere.is_some()
            || !s.lateral_views.is_empty()
            || !s.cluster_by.is_empty()
            || !s.distribute_by.is_empty()
            || !s.sort_by.is_empty()
            || !s.named_window.is_empty()
            || s.qualify.is_some()
            || s.connect_by.is_some()
            || s.distinct.is_some()
            || s.having.is_some()
            || s.selection.is_some()
            || s.from.len() != 1
            || !s.from[0].joins.is_empty()
        {
            return Ok(None);
        }
        let TableFactor::Table {
            name: table_name,
            alias,
            args: None,
            ..
        } = &s.from[0].relation
        else {
            return Ok(None);
        };
        let table_name = name(table_name);
        if matches!(table_name.as_str(), "sqlite_schema" | "sqlite_master") {
            return Ok(None);
        }
        let Some(table) = self.tables.get(&table_name) else {
            return Ok(None);
        };
        let prefix = alias
            .as_ref()
            .map(|a| a.name.value.to_lowercase())
            .unwrap_or_else(|| table_name.clone());
        let source = table
            .columns
            .iter()
            .map(|c| format!("{prefix}.{}", c.name))
            .collect::<Vec<_>>();
        let GroupByExpr::Expressions(groups, modifiers) = &s.group_by else {
            return Ok(None);
        };
        if groups.len() > 1 || !modifiers.is_empty() {
            return Ok(None);
        }
        let group = if let Some(expr) = groups.first() {
            let Some(column) = column(expr, &source)? else {
                return Ok(None);
            };
            if !matches!(
                table.columns[column].ty,
                Type::SmallInt | Type::Integer | Type::BigInt
            ) {
                return Ok(None);
            }
            Some(column)
        } else {
            None
        };
        let mut items = Vec::new();
        let mut aggregated = false;
        for projection in &s.projection {
            let (SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. }) =
                projection
            else {
                return Ok(None);
            };
            if let Expr::Function(f) = expr {
                if f.filter.is_some()
                    || f.over.is_some()
                    || !f.within_group.is_empty()
                    || !matches!(f.parameters, FunctionArguments::None)
                {
                    return Ok(None);
                }
                let FunctionArguments::List(args) = &f.args else {
                    return Ok(None);
                };
                if !args.clauses.is_empty()
                    || matches!(args.duplicate_treatment, Some(DuplicateTreatment::Distinct))
                {
                    return Ok(None);
                }
                match (name(&f.name).as_str(), args.args.as_slice()) {
                    ("count", [] | [FunctionArg::Unnamed(FunctionArgExpr::Wildcard)]) => {
                        items.push(Item::Count(None))
                    }
                    ("count", [FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))]) => {
                        let Some(column) = column(expr, &source)? else {
                            return Ok(None);
                        };
                        items.push(Item::Count(Some(column)));
                    }
                    ("sum", [FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))]) => {
                        let Some(column) = column(expr, &source)? else {
                            return Ok(None);
                        };
                        if !matches!(
                            table.columns[column].ty,
                            Type::SmallInt | Type::Integer | Type::BigInt
                        ) {
                            return Ok(None);
                        }
                        items.push(Item::Sum(column));
                    }
                    _ => return Ok(None),
                }
                aggregated = true;
            } else if group.is_some() && column(expr, &source)? == group {
                items.push(Item::Group);
            } else {
                return Ok(None);
            }
        }
        if !aggregated && group.is_none() {
            return Ok(None);
        }
        let columns = project_group(
            &s.projection,
            &source,
            &vec![Value::Null; source.len()],
            Some(&[]),
        )?
        .0;
        let mut order = Vec::new();
        if let Some(order_by) = &q.order_by {
            for expr in &order_by.exprs {
                let column = match &expr.expr {
                    Expr::Identifier(n)
                        if columns.iter().any(|c| c.eq_ignore_ascii_case(&n.value)) =>
                    {
                        lookup_index(&n.value, &columns)?
                    }
                    Expr::Value(sqlparser::ast::Value::Number(n, _)) => n
                        .parse::<usize>()
                        .ok()
                        .filter(|i| *i > 0 && *i <= columns.len())
                        .map(|i| i - 1)
                        .ok_or_else(|| Error("ORDER BY position out of range".into()))?,
                    _ => return Ok(None),
                };
                order.push((column, expr.clone()));
            }
        }
        Ok(Some(AggregatePlan {
            table: table_name,
            group,
            items,
            columns,
            order,
            offset: q
                .offset
                .as_ref()
                .map(|o| nonnegative(&o.value))
                .transpose()?
                .unwrap_or(0),
            limit: q
                .limit
                .as_ref()
                .map(nonnegative)
                .transpose()?
                .unwrap_or(usize::MAX),
        }))
    }
    pub(super) fn run_aggregate(&self, plan: &AggregatePlan) -> Result<QueryResult> {
        let table = &self.tables[&plan.table];
        let initialize = |key: Option<i64>| {
            plan.items
                .iter()
                .map(|item| match item {
                    Item::Group => key,
                    Item::Count(_) => Some(0),
                    Item::Sum(_) => None,
                })
                .collect::<Vec<_>>()
        };
        let mut positions = HashMap::new();
        let mut groups = Vec::new();
        if plan.group.is_none() {
            positions.insert(None, 0);
            groups.push(initialize(None));
        }
        for row in table.rows.iter() {
            let key = plan.group.and_then(|column| match row[column] {
                Value::Integer(n) => Some(n),
                _ => None,
            });
            let position = *positions.entry(key).or_insert_with(|| {
                groups.push(initialize(key));
                groups.len() - 1
            });
            let values = &mut groups[position];
            for (i, item) in plan.items.iter().enumerate() {
                match item {
                    Item::Group => (),
                    Item::Count(column) => {
                        if column.is_none_or(|c| row[c] != Value::Null) {
                            values[i] = Some(values[i].unwrap() + 1);
                        }
                    }
                    Item::Sum(column) => {
                        if let Value::Integer(n) = row[*column] {
                            values[i] = Some(
                                values[i]
                                    .unwrap_or(0)
                                    .checked_add(n)
                                    .ok_or_else(|| Error("integer overflow".into()))?,
                            );
                        }
                    }
                }
            }
        }
        let mut rows = groups
            .into_iter()
            .map(|g| {
                g.into_iter()
                    .map(|n| n.map(Value::Integer).unwrap_or(Value::Null))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| {
            for (column, order) in &plan.order {
                let (a, b) = (&a[*column], &b[*column]);
                let mut c = match (a, b) {
                    (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
                    (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
                    (Value::Null, _) => std::cmp::Ordering::Less,
                    _ => std::cmp::Ordering::Greater,
                };
                if order.asc == Some(false) {
                    c = c.reverse();
                }
                if (a == &Value::Null || b == &Value::Null)
                    && a != b
                    && let Some(first) = order.nulls_first
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
        Ok(QueryResult {
            columns: plan.columns.clone(),
            rows: rows
                .into_iter()
                .skip(plan.offset)
                .take(plan.limit)
                .collect(),
            affected: 0,
        })
    }
}
