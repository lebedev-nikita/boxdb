//! Bound, borrowed-row execution for ordinary single-table projections.
use super::*;

pub(crate) enum Plan {
    Select(SelectPlan),
    Aggregate(super::aggregate::AggregatePlan),
}
impl State {
    pub fn prepare(&self, statement: &Statement) -> Result<Option<Plan>> {
        if let Some(plan) = self.prepare_aggregate(statement)? {
            return Ok(Some(Plan::Aggregate(plan)));
        }
        self.prepare_select(statement).map(|p| p.map(Plan::Select))
    }
    pub fn run(&self, plan: &Plan) -> Result<QueryResult> {
        match plan {
            Plan::Select(plan) => self.run_select(plan),
            Plan::Aggregate(plan) => self.run_aggregate(plan),
        }
    }
}
enum Access {
    Constraint(usize),
    Named(String),
}
pub(crate) struct SelectPlan {
    table: String,
    columns: Vec<String>,
    projection: Vec<usize>,
    filter: Option<(usize, IntegerBounds)>,
    access: Option<Access>,
    ordered_index: bool,
    order: Vec<(usize, OrderByExpr)>,
    offset: usize,
    limit: usize,
}
fn all_integer(expr: &Expr, column: &str, prefix: &str) -> bool {
    match expr {
        Expr::Nested(expr) => all_integer(expr, column, prefix),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => all_integer(left, column, prefix) && all_integer(right, column, prefix),
        Expr::BinaryOp {
            op:
                BinaryOperator::Eq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq,
            ..
        } => IntegerBounds {
            lower: (i64::MIN, true),
            upper: (i64::MAX, true),
        }
        .collect(expr, column, prefix),
        _ => false,
    }
}
impl State {
    fn prepare_select(&self, statement: &Statement) -> Result<Option<SelectPlan>> {
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
            || s.from.len() != 1
            || !s.from[0].joins.is_empty()
        {
            return Ok(None);
        }
        let GroupByExpr::Expressions(groups, modifiers) = &s.group_by else {
            return Ok(None);
        };
        if !groups.is_empty() || !modifiers.is_empty() {
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
        let table = self
            .tables
            .get(&table_name)
            .ok_or_else(|| Error(format!("no such table: {table_name}")))?;
        let prefix = alias
            .as_ref()
            .map(|a| a.name.value.to_lowercase())
            .unwrap_or_else(|| table_name.clone());
        let source = table
            .columns
            .iter()
            .map(|c| format!("{prefix}.{}", c.name))
            .collect::<Vec<_>>();
        let Some(projection) = simple_projection(&s.projection, &source)? else {
            return Ok(None);
        };
        let columns = project_group(
            &s.projection,
            &source,
            &vec![Value::Null; source.len()],
            None,
        )?
        .0;
        let filter = if let Some(expr) = &s.selection {
            bind(expr, &source, false)?;
            let Some((column, _)) = table.columns.iter().enumerate().find(|(_, c)| {
                matches!(c.ty, Type::SmallInt | Type::Integer | Type::BigInt)
                    && all_integer(expr, &c.name, &prefix)
            }) else {
                return Ok(None);
            };
            let mut bounds = IntegerBounds {
                lower: (i64::MIN, true),
                upper: (i64::MAX, true),
            };
            bounds.collect(expr, &table.columns[column].name, &prefix);
            Some((column, bounds))
        } else {
            None
        };
        let mut order = Vec::new();
        if let Some(order_by) = &q.order_by {
            for expr in &order_by.exprs {
                let column = match &expr.expr {
                    Expr::Value(sqlparser::ast::Value::Number(n, _)) => {
                        let position = n
                            .parse::<usize>()
                            .ok()
                            .filter(|i| *i > 0 && *i <= projection.len())
                            .ok_or_else(|| Error("ORDER BY position out of range".into()))?;
                        projection[position - 1]
                    }
                    Expr::Identifier(n)
                        if columns.iter().any(|c| c.eq_ignore_ascii_case(&n.value)) =>
                    {
                        projection[lookup_index(&n.value, &columns)?]
                    }
                    Expr::Identifier(n) => lookup_index(&n.value, &source)?,
                    Expr::CompoundIdentifier(ids) => lookup_index(
                        &ids.iter()
                            .map(|n| n.value.as_str())
                            .collect::<Vec<_>>()
                            .join("."),
                        &source,
                    )?,
                    _ => return Ok(None),
                };
                order.push((column, expr.clone()));
            }
        }
        let key_column = filter.as_ref().map(|(column, _)| *column).or_else(|| {
            if let [(column, _)] = order.as_slice() {
                let not_null = table.columns[*column].not_null || table.constraints.iter().any(|c| matches!(c, Constraint::Unique { columns, primary: true } if columns.contains(column)));
                (not_null && matches!(table.columns[*column].ty, Type::SmallInt | Type::Integer | Type::BigInt)).then_some(*column)
            } else { None }
        });
        let access = key_column.and_then(|column| {
            table
                .constraint_indexes
                .iter()
                .position(|i| i.columns == [column])
                .map(Access::Constraint)
                .or_else(|| {
                    self.indexes
                        .iter()
                        .find(|(_, i)| i.table == table_name && i.columns == [column])
                        .map(|(name, _)| Access::Named(name.clone()))
                })
        });
        let ordered_index = access.is_some()
            && matches!(order.as_slice(), [(column, _)] if Some(*column) == key_column);
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
        Ok(Some(SelectPlan {
            table: table_name,
            columns,
            projection,
            filter,
            access,
            ordered_index,
            order,
            offset,
            limit,
        }))
    }
    fn run_select(&self, plan: &SelectPlan) -> Result<QueryResult> {
        let table = &self.tables[&plan.table];
        let materialize = |positions: &mut dyn Iterator<Item = usize>| {
            positions
                .skip(plan.offset)
                .take(plan.limit)
                .map(|position| {
                    let row = &table.rows[position];
                    plan.projection
                        .iter()
                        .map(|i| row[*i].clone())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let index = plan.access.as_ref().map(|access| match access {
            Access::Constraint(i) => &table.constraint_indexes[*i],
            Access::Named(name) => &self.indexes[name],
        });
        let bounds = plan.filter.as_ref().map(|(_, b)| b);
        let empty = bounds.is_some_and(|b| {
            b.lower.0 > b.upper.0 || (b.lower.0 == b.upper.0 && !(b.lower.1 && b.upper.1))
        });
        let rows = if empty || plan.limit == 0 || table.rows.is_empty() {
            vec![]
        } else if let Some(index) = index {
            if let Some(bounds) = bounds
                && bounds.lower.0 == bounds.upper.0
                && (plan.order.is_empty() || plan.ordered_index)
            {
                // Equality needs one tree lookup, without constructing a range
                // cursor or copying/sorting physical row positions.
                return Ok(QueryResult {
                    columns: plan.columns.clone(),
                    rows: materialize(
                        &mut index
                            .entries
                            .get(&IndexKey::Integer(bounds.lower.0))
                            .into_iter()
                            .flatten()
                            .copied(),
                    ),
                    affected: 0,
                });
            }
            use std::ops::Bound::{Excluded, Included};
            let (low, high) = bounds
                .map(|b| (b.lower, b.upper))
                .unwrap_or(((i64::MIN, true), (i64::MAX, true)));
            let low = if low.1 {
                Included(IndexKey::Integer(low.0))
            } else {
                Excluded(IndexKey::Integer(low.0))
            };
            let high = if high.1 {
                Included(IndexKey::Integer(high.0))
            } else {
                Excluded(IndexKey::Integer(high.0))
            };
            let range = index.entries.range((low, high));
            if plan.ordered_index {
                if plan.order[0].1.asc == Some(false) {
                    materialize(&mut range.rev().flat_map(|(_, p)| p.iter().copied()))
                } else {
                    materialize(&mut range.flat_map(|(_, p)| p.iter().copied()))
                }
            } else {
                let mut positions = range
                    .flat_map(|(_, p)| p.iter().copied())
                    .collect::<Vec<_>>();
                sort_positions(table, plan, &mut positions)?;
                materialize(&mut positions.into_iter())
            }
        } else {
            let mut positions = table
                .rows
                .positions()
                .filter(|(_, row)| {
                    plan.filter.as_ref().is_none_or(|(column, bounds)| {
                        if let Value::Integer(n) = row[*column] {
                            (n > bounds.lower.0 || (n == bounds.lower.0 && bounds.lower.1))
                                && (n < bounds.upper.0 || (n == bounds.upper.0 && bounds.upper.1))
                        } else {
                            false
                        }
                    })
                })
                .map(|(position, _)| position);
            if plan.order.is_empty() {
                materialize(&mut positions)
            } else {
                let mut positions = positions.collect::<Vec<_>>();
                sort_positions(table, plan, &mut positions)?;
                materialize(&mut positions.into_iter())
            }
        };
        Ok(QueryResult {
            columns: plan.columns.clone(),
            rows,
            affected: 0,
        })
    }
}
fn sort_positions(table: &Table, plan: &SelectPlan, positions: &mut [usize]) -> Result<()> {
    if plan.order.is_empty() {
        positions.sort_unstable();
        return Ok(());
    }
    let mut failure = None;
    positions.sort_by(|a, b| {
        for (column, order) in &plan.order {
            let (a, b) = (&table.rows[*a][*column], &table.rows[*b][*column]);
            let mut comparison = match crate::expression::compare(a, b) {
                Ok(c) => c,
                Err(e) => {
                    failure = Some(e);
                    return std::cmp::Ordering::Equal;
                }
            };
            if order.asc == Some(false) {
                comparison = comparison.reverse();
            }
            if (a == &Value::Null || b == &Value::Null)
                && a != b
                && let Some(first) = order.nulls_first
            {
                comparison = if (a == &Value::Null) == first {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            if !comparison.is_eq() {
                return comparison;
            }
        }
        std::cmp::Ordering::Equal
    });
    failure.map_or(Ok(()), Err)
}
