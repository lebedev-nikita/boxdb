use crate::{Error, Result};
use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Numeric(String),
    Boolean(bool),
    Text(String),
    Bytea(Vec<u8>),
    Uuid(String),
    Json(serde_json::Value),
    Date(String),
    Time(String),
    Timestamp(String),
    Timestamptz(String),
    Array(Vec<Value>),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Type {
    SmallInt,
    Integer,
    BigInt,
    Real,
    Double,
    Numeric(Option<(u32, u32)>),
    Boolean,
    Text,
    Varchar(Option<usize>),
    Bytea,
    Uuid,
    Json,
    Date,
    Time,
    Timestamp,
    Timestamptz,
    Array(Box<Type>),
}

impl Type {
    pub fn parse(input: &str) -> Result<Self> {
        if input.len() > 4096
            || input.matches("[]").count() + input.to_uppercase().matches("ARRAY<").count() > 32
        {
            return Err(Error("type nesting or length exceeds limit".into()));
        }
        let ty = input.trim().to_uppercase();
        if let Some(inner) = ty.strip_suffix("[]") {
            return Ok(Self::Array(Box::new(Self::parse(inner)?)));
        }
        if let Some(inner) = ty.strip_prefix("ARRAY<").and_then(|s| s.strip_suffix('>')) {
            return Ok(Self::Array(Box::new(Self::parse(inner)?)));
        }
        let (base, args) = if let Some((base, tail)) = ty.split_once('(') {
            let tail = tail
                .strip_suffix(')')
                .ok_or_else(|| Error("invalid type parameters".into()))?;
            let args = tail
                .split(',')
                .map(|s| {
                    s.trim()
                        .parse::<u32>()
                        .map_err(|_| Error("invalid type parameter".into()))
                })
                .collect::<Result<Vec<_>>>()?;
            (base.trim(), args)
        } else {
            (ty.as_str(), vec![])
        };
        let result = match base {
            "SMALLINT" | "INT2" => Self::SmallInt,
            "INT" | "INTEGER" | "INT4" => Self::Integer,
            "BIGINT" | "INT8" => Self::BigInt,
            "REAL" | "FLOAT4" => Self::Real,
            "DOUBLE" | "DOUBLE PRECISION" | "FLOAT" | "FLOAT8" => Self::Double,
            "NUMERIC" | "DECIMAL" => {
                if args.is_empty() {
                    Self::Numeric(None)
                } else {
                    let p = args[0];
                    let s = *args.get(1).unwrap_or(&0);
                    if args.len() > 2 || p == 0 || p > 1000 || s > p {
                        return Err(Error("invalid NUMERIC precision/scale".into()));
                    }
                    Self::Numeric(Some((p, s)))
                }
            }
            "BOOLEAN" | "BOOL" => Self::Boolean,
            "TEXT" => Self::Text,
            "VARCHAR" | "CHARACTER VARYING" => {
                if args.len() > 1 || args.first() == Some(&0) {
                    return Err(Error("invalid VARCHAR length".into()));
                }
                Self::Varchar(args.first().map(|n| *n as usize))
            }
            "BYTEA" | "BLOB" => Self::Bytea,
            "UUID" => Self::Uuid,
            "JSON" | "JSONB" => Self::Json,
            "DATE" => Self::Date,
            "TIME" | "TIME WITHOUT TIME ZONE" => Self::Time,
            "TIMESTAMP" | "TIMESTAMP WITHOUT TIME ZONE" => Self::Timestamp,
            "TIMESTAMPTZ" | "TIMESTAMP WITH TIME ZONE" => Self::Timestamptz,
            _ => return Err(Error(format!("unsupported or missing type: {input}"))),
        };
        if !args.is_empty() && !matches!(result, Self::Numeric(_) | Self::Varchar(_)) {
            return Err(Error("type does not accept parameters".into()));
        }
        Ok(result)
    }
    pub(crate) fn normalize(&self, value: Value) -> Result<Value> {
        self.validate(&Value::Null)?;
        if let Self::Array(inner) = self
            && let Value::Array(values) = value
        {
            return Ok(Value::Array(
                values
                    .into_iter()
                    .map(|v| inner.normalize(v))
                    .collect::<Result<_>>()?,
            ));
        }
        if let (Self::Real | Self::Double, Value::Real(n)) = (self, &value) {
            let n = if matches!(self, Self::Real) {
                (*n as f32) as f64
            } else {
                *n
            };
            if !n.is_finite() {
                return Err(Error("non-finite floating point value".into()));
            }
            return Ok(Value::Real(n));
        }
        match (self, &value) {
            (_, Value::Null)
            | (Self::Text | Self::Varchar(_), Value::Text(_))
            | (Self::SmallInt | Self::Integer | Self::BigInt, Value::Integer(_))
            | (Self::Boolean, Value::Boolean(_))
            | (Self::Bytea, Value::Bytea(_))
            | (Self::Json, Value::Json(_)) => {
                self.check(&value)?;
                Ok(value)
            }
            _ => self.validate(&value),
        }
    }
    pub(crate) fn check(&self, value: &Value) -> Result<()> {
        match (self, value) {
            (_, Value::Null)
            | (Self::BigInt, Value::Integer(_))
            | (Self::Text, Value::Text(_))
            | (Self::Bytea, Value::Bytea(_))
            | (Self::Boolean, Value::Boolean(_))
            | (Self::Json, Value::Json(_)) => Ok(()),
            (Self::SmallInt, Value::Integer(n)) if i16::try_from(*n).is_ok() => Ok(()),
            (Self::Integer, Value::Integer(n)) if i32::try_from(*n).is_ok() => Ok(()),
            (Self::Real | Self::Double, Value::Real(n)) if n.is_finite() => Ok(()),
            (Self::Array(inner), Value::Array(values)) => {
                for value in values {
                    inner.check(value)?;
                }
                Ok(())
            }
            (Self::Varchar(limit), Value::Text(s))
                if limit.is_none_or(|n| s.chars().count() <= n) =>
            {
                Ok(())
            }
            _ => self.validate(value).map(|_| ()),
        }
    }
    pub fn validate(&self, value: &Value) -> Result<Value> {
        if matches!(self,Self::Numeric(Some((p,s))) if *p==0 || *p>1000 || s>p)
            || matches!(self, Self::Varchar(Some(0)))
        {
            return Err(Error("invalid type parameters".into()));
        }
        if let Self::Array(inner) = self {
            inner.validate(&Value::Null)?;
        }
        if matches!(value, Value::Null) {
            return Ok(Value::Null);
        }
        let invalid = || Error(format!("type mismatch: expected {self:?}, got {value:?}"));
        match (self, value) {
            (Self::SmallInt, Value::Integer(n)) if i16::try_from(*n).is_ok() => Ok(value.clone()),
            (Self::Integer, Value::Integer(n)) if i32::try_from(*n).is_ok() => Ok(value.clone()),
            (Self::BigInt, Value::Integer(_)) => Ok(value.clone()),
            (Self::Real | Self::Double, Value::Integer(_) | Value::Numeric(_) | Value::Real(_)) => {
                let n: f64 = value.to_string().parse().map_err(|_| invalid())?;
                let n = if matches!(self, Self::Real) {
                    (n as f32) as f64
                } else {
                    n
                };
                if !n.is_finite() {
                    return Err(invalid());
                }
                Ok(Value::Real(n))
            }
            (Self::Numeric(limits), Value::Integer(_) | Value::Numeric(_)) => {
                let decimal = BigDecimal::from_str(&value.to_string()).map_err(|_| invalid())?;
                let (coefficient, exponent) = decimal.as_bigint_and_exponent();
                if coefficient.to_string().len() > 4096 || exponent.unsigned_abs() > 4096 {
                    return Err(Error("NUMERIC exceeds 4096-digit limit".into()));
                }
                let text = decimal.normalized().to_plain_string();
                let unsigned = text.trim_start_matches('-');
                let (whole, frac) = unsigned.split_once('.').unwrap_or((unsigned, ""));
                if let Some((precision, scale)) = limits {
                    if frac.len() > *scale as usize
                        || whole.trim_start_matches('0').len() > (precision - scale) as usize
                    {
                        return Err(Error("NUMERIC precision or scale overflow".into()));
                    }
                    Ok(Value::Numeric(
                        decimal.with_scale(*scale as i64).to_plain_string(),
                    ))
                } else {
                    Ok(Value::Numeric(text))
                }
            }
            (Self::Boolean, Value::Boolean(_))
            | (Self::Text, Value::Text(_))
            | (Self::Bytea, Value::Bytea(_)) => Ok(value.clone()),
            (Self::Varchar(limit), Value::Text(s))
                if limit.is_none_or(|n| s.chars().count() <= n) =>
            {
                Ok(value.clone())
            }
            (Self::Uuid, Value::Uuid(s)) => uuid::Uuid::parse_str(s)
                .map(|u| Value::Uuid(u.to_string()))
                .map_err(|_| invalid()),
            (Self::Json, Value::Json(_)) => Ok(value.clone()),
            (Self::Date, Value::Date(s)) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(|d| Value::Date(d.to_string()))
                .map_err(|_| invalid()),
            (Self::Time, Value::Time(s)) => NaiveTime::parse_from_str(s, "%H:%M:%S%.f")
                .map(|t| Value::Time(t.to_string()))
                .map_err(|_| invalid()),
            (Self::Timestamp, Value::Timestamp(s)) => {
                NaiveDateTime::parse_from_str(&s.replace('T', " "), "%Y-%m-%d %H:%M:%S%.f")
                    .map(|t| Value::Timestamp(t.to_string()))
                    .map_err(|_| invalid())
            }
            (Self::Timestamptz, Value::Timestamptz(s)) => {
                DateTime::parse_from_rfc3339(&s.replace(' ', "T"))
                    .map(|t| {
                        Value::Timestamptz(
                            t.with_timezone(&chrono::Utc)
                                .to_rfc3339_opts(SecondsFormat::AutoSi, true),
                        )
                    })
                    .map_err(|_| invalid())
            }
            (Self::Array(ty), Value::Array(values)) => Ok(Value::Array(
                values
                    .iter()
                    .map(|v| ty.validate(v))
                    .collect::<Result<_>>()?,
            )),
            _ => Err(invalid()),
        }
    }
    pub fn cast(&self, value: Value) -> Result<Value> {
        if matches!(value, Value::Null) {
            return Ok(value);
        }
        let text = value.to_string();
        let parsed = match self {
            Self::SmallInt | Self::Integer | Self::BigInt => Value::Integer(
                text.parse()
                    .map_err(|_| Error("invalid integer cast".into()))?,
            ),
            Self::Real | Self::Double => Value::Real(
                text.parse()
                    .map_err(|_| Error("invalid floating-point cast".into()))?,
            ),
            Self::Numeric(_) => {
                BigDecimal::from_str(&text).map_err(|_| Error("invalid numeric cast".into()))?;
                Value::Numeric(text)
            }
            Self::Boolean => Value::Boolean(match text.to_lowercase().as_str() {
                "true" => true,
                "false" => false,
                _ => return Err(Error("invalid boolean cast".into())),
            }),
            Self::Text | Self::Varchar(_) => Value::Text(text),
            Self::Uuid => Value::Uuid(text),
            Self::Date => Value::Date(text),
            Self::Time => Value::Time(text),
            Self::Timestamp => Value::Timestamp(text),
            Self::Timestamptz => Value::Timestamptz(text),
            Self::Json => {
                Value::Json(serde_json::from_str(&text).map_err(|e| Error(e.to_string()))?)
            }
            Self::Bytea | Self::Array(_) => value,
        };
        self.validate(&parsed)
    }
}
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => write!(f, "NULL"),
            Self::Integer(n) => write!(f, "{n}"),
            Self::Real(n) => write!(f, "{n}"),
            Self::Boolean(b) => write!(f, "{b}"),
            Self::Bytea(bytes) => write!(f, "{}", hex::encode(bytes)),
            Self::Numeric(s)
            | Self::Text(s)
            | Self::Uuid(s)
            | Self::Date(s)
            | Self::Time(s)
            | Self::Timestamp(s)
            | Self::Timestamptz(s) => write!(f, "{s}"),
            Self::Json(j) => write!(f, "{j}"),
            Self::Array(_) => write!(
                f,
                "{}",
                serde_json::to_string(self).map_err(|_| std::fmt::Error)?
            ),
        }
    }
}
