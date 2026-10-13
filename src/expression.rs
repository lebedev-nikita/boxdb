use crate::{Error, Result, Value};
use bigdecimal::BigDecimal;
use sqlparser::ast::BinaryOperator;
use std::{cmp::Ordering, str::FromStr};

pub(crate) fn truth(v: &Value) -> Result<Option<bool>> {
    match v {
        Value::Null => Ok(None),
        Value::Boolean(b) => Ok(Some(*b)),
        Value::Integer(n) => Ok(Some(*n != 0)),
        Value::Real(n) => Ok(Some(*n != 0.0)),
        Value::Numeric(s) => Ok(Some(
            s.parse::<BigDecimal>().map_err(|e| Error(e.to_string()))? != 0,
        )),
        _ => Err(Error("boolean expression required".into())),
    }
}
pub(crate) fn compare(a: &Value, b: &Value) -> Result<Ordering> {
    if a == b {
        return Ok(Ordering::Equal);
    }
    match (a, b) {
        (Value::Null, _) => Ok(Ordering::Less),
        (_, Value::Null) => Ok(Ordering::Greater),
        (Value::Integer(a), Value::Integer(b)) => Ok(a.cmp(b)),
        (
            Value::Integer(_) | Value::Real(_) | Value::Numeric(_),
            Value::Integer(_) | Value::Real(_) | Value::Numeric(_),
        ) => Ok(decimal(a)?.cmp(&decimal(b)?)),
        (Value::Text(a), Value::Text(b)) | (Value::Uuid(a), Value::Uuid(b)) => Ok(a.cmp(b)),
        (Value::Date(a), Value::Date(b)) => {
            let parse = |s: &str| {
                chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| Error(e.to_string()))
            };
            Ok(parse(a)?.cmp(&parse(b)?))
        }
        (Value::Time(a), Value::Time(b)) => {
            let parse = |s: &str| {
                chrono::NaiveTime::parse_from_str(s, "%H:%M:%S%.f")
                    .map_err(|e| Error(e.to_string()))
            };
            Ok(parse(a)?.cmp(&parse(b)?))
        }
        (Value::Timestamp(a), Value::Timestamp(b)) => {
            let parse = |s: &str| {
                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                    .map_err(|e| Error(e.to_string()))
            };
            Ok(parse(a)?.cmp(&parse(b)?))
        }
        (Value::Timestamptz(a), Value::Timestamptz(b)) => {
            let parse =
                |s: &str| chrono::DateTime::parse_from_rfc3339(s).map_err(|e| Error(e.to_string()));
            Ok(parse(a)?.cmp(&parse(b)?))
        }
        (Value::Array(a), Value::Array(b)) => {
            for (a, b) in a.iter().zip(b) {
                let order = compare(a, b)?;
                if !order.is_eq() {
                    return Ok(order);
                }
            }
            Ok(a.len().cmp(&b.len()))
        }
        (Value::Boolean(a), Value::Boolean(b)) => Ok(a.cmp(b)),
        (Value::Bytea(a), Value::Bytea(b)) => Ok(a.cmp(b)),
        _ => Err(Error("incompatible comparison types".into())),
    }
}
fn decimal(v: &Value) -> Result<BigDecimal> {
    BigDecimal::from_str(&v.to_string()).map_err(|_| Error("number required".into()))
}
pub(crate) fn binary(a: Value, op: &BinaryOperator, b: Value) -> Result<Value> {
    use BinaryOperator::*;
    if matches!(op, And | Or) {
        let (a, b) = (truth(&a)?, truth(&b)?);
        return Ok(match (op, a, b) {
            (And, Some(false), _) | (And, _, Some(false)) => Value::Boolean(false),
            (Or, Some(true), _) | (Or, _, Some(true)) => Value::Boolean(true),
            (_, None, _) | (_, _, None) => Value::Null,
            (And, Some(a), Some(b)) => Value::Boolean(a && b),
            (Or, Some(a), Some(b)) => Value::Boolean(a || b),
            _ => unreachable!(),
        });
    }
    if a == Value::Null || b == Value::Null {
        return Ok(Value::Null);
    }
    if matches!(op, Eq | NotEq | Gt | GtEq | Lt | LtEq) {
        let c = compare(&a, &b)?;
        return Ok(Value::Boolean(match op {
            Eq => c.is_eq(),
            NotEq => !c.is_eq(),
            Gt => c.is_gt(),
            GtEq => !c.is_lt(),
            Lt => c.is_lt(),
            LtEq => !c.is_gt(),
            _ => unreachable!(),
        }));
    }
    if let (StringConcat, Value::Text(a), Value::Text(b)) = (op, &a, &b) {
        return Ok(Value::Text(format!("{a}{b}")));
    }
    if let (Value::Integer(a), Value::Integer(b)) = (&a, &b) {
        let n = match op {
            Plus => a.checked_add(*b),
            Minus => a.checked_sub(*b),
            Multiply => a.checked_mul(*b),
            Divide if *b == 0 => return Ok(Value::Null),
            Divide => a.checked_div(*b),
            Modulo if *b == 0 => return Ok(Value::Null),
            Modulo => {
                if *b == -1 {
                    Some(0)
                } else {
                    a.checked_rem(*b)
                }
            }
            BitwiseAnd => Some(a & b),
            BitwiseOr => Some(a | b),
            BitwiseXor | PGBitwiseXor => Some(a ^ b),
            PGBitwiseShiftLeft | PGBitwiseShiftRight => {
                let left = matches!(op, PGBitwiseShiftLeft) ^ (*b < 0);
                let shift = b.unsigned_abs();
                Some(if shift >= 64 {
                    if !left && *a < 0 { -1 } else { 0 }
                } else if left {
                    a.wrapping_shl(shift as u32)
                } else {
                    a >> shift
                })
            }
            _ => return Err(Error(format!("unsupported operator: {op}"))),
        };
        return n
            .map(Value::Integer)
            .ok_or_else(|| Error("integer overflow".into()));
    }
    let (x, y) = (decimal(&a)?, decimal(&b)?);
    if matches!(op, Divide | Modulo) && y == 0 {
        return Ok(Value::Null);
    }
    let n = match op {
        Plus => x + y,
        Minus => x - y,
        Multiply => x * y,
        Divide => x / y,
        Modulo => x % y,
        _ => return Err(Error(format!("unsupported numeric operator: {op}"))),
    };
    if matches!(a, Value::Real(_)) || matches!(b, Value::Real(_)) {
        return n
            .to_string()
            .parse()
            .map(Value::Real)
            .map_err(|_| Error("floating-point overflow".into()));
    }
    Ok(Value::Numeric(n.normalized().to_plain_string()))
}
