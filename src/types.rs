use crate::{Error, Result};
use bigdecimal::BigDecimal;
use chrono::{DateTime, Datelike, SecondsFormat};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Integer(i64),
    UnsignedInteger(u64),
    Real(f64),
    Numeric(String),
    Boolean(bool),
    Text(String),
    Bytea(Vec<u8>),
    Uuid(String),
    Json(serde_json::Value),
    /// An RFC 3339 instant, normalized to UTC with nanosecond precision.
    Timestamp(String),
    Array(Vec<Value>),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Type {
    Int8,
    #[serde(alias = "SmallInt")]
    Int16,
    #[serde(alias = "Integer")]
    Int32,
    #[serde(alias = "BigInt")]
    Int64,
    Uint8,
    Uint16,
    Uint32,
    Real,
    Double,
    Numeric(Option<(u32, u32)>),
    Boolean,
    Text,
    Varchar(Option<usize>),
    Bytea,
    Uuid,
    Json,
    Timestamp,
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
            "INT8" => Self::Int8,
            "INT16" => Self::Int16,
            "INT32" => Self::Int32,
            "INT64" => Self::Int64,
            "UINT8" => Self::Uint8,
            "UINT16" => Self::Uint16,
            "UINT32" => Self::Uint32,
            "FLOAT4" => Self::Real,
            "REAL" | "DOUBLE" | "DOUBLE PRECISION" | "FLOAT" | "FLOAT8" => Self::Double,
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
            "TIMESTAMP" => Self::Timestamp,
            _ => return Err(Error(format!("unsupported or missing type: {input}"))),
        };
        if !args.is_empty() && !matches!(result, Self::Numeric(_) | Self::Varchar(_)) {
            return Err(Error("type does not accept parameters".into()));
        }
        Ok(result)
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
            (Self::Int8, Value::Integer(n)) if i8::try_from(*n).is_ok() => Ok(value.clone()),
            (Self::Int16, Value::Integer(n)) if i16::try_from(*n).is_ok() => Ok(value.clone()),
            (Self::Int32, Value::Integer(n)) if i32::try_from(*n).is_ok() => Ok(value.clone()),
            (Self::Int64, Value::Integer(_)) => Ok(value.clone()),
            (Self::Uint8 | Self::Uint16 | Self::Uint32, Value::Integer(n)) => {
                let n = u64::try_from(*n).map_err(|_| invalid())?;
                self.validate(&Value::UnsignedInteger(n))
            }
            (Self::Uint8, Value::UnsignedInteger(n)) if u8::try_from(*n).is_ok() => {
                Ok(value.clone())
            }
            (Self::Uint16, Value::UnsignedInteger(n)) if u16::try_from(*n).is_ok() => {
                Ok(value.clone())
            }
            (Self::Uint32, Value::UnsignedInteger(n)) if u32::try_from(*n).is_ok() => {
                Ok(value.clone())
            }
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
            (Self::Timestamp, Value::Timestamp(s)) => {
                // Chrono accepts leap seconds and truncates extra fractional
                // digits. Neither is part of this exact, nanosecond contract.
                if s.split_once('.').is_some_and(|(_, fraction)| {
                    fraction.chars().take_while(char::is_ascii_digit).count() > 9
                }) {
                    return Err(invalid());
                }
                let instant = DateTime::parse_from_rfc3339(&s.replace(' ', "T"))
                    .map_err(|_| invalid())?
                    .with_timezone(&chrono::Utc);
                if !(0..=9999).contains(&instant.year())
                    || instant.timestamp_subsec_nanos() >= 1_000_000_000
                {
                    return Err(invalid());
                }
                Ok(Value::Timestamp(
                    instant.to_rfc3339_opts(SecondsFormat::Nanos, true),
                ))
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
            Self::Int8 | Self::Int16 | Self::Int32 | Self::Int64 => Value::Integer(
                text.parse()
                    .map_err(|_| Error("invalid integer cast".into()))?,
            ),
            Self::Uint8 | Self::Uint16 | Self::Uint32 => Value::UnsignedInteger(
                text.parse()
                    .map_err(|_| Error("invalid unsigned integer cast".into()))?,
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
            Self::Timestamp => Value::Timestamp(text),
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
            Self::UnsignedInteger(n) => write!(f, "{n}"),
            Self::Real(n) => write!(f, "{n}"),
            Self::Boolean(b) => write!(f, "{b}"),
            Self::Bytea(bytes) => write!(f, "{}", hex::encode(bytes)),
            Self::Numeric(s) | Self::Text(s) | Self::Uuid(s) | Self::Timestamp(s) => {
                write!(f, "{s}")
            }
            Self::Json(j) => write!(f, "{j}"),
            Self::Array(_) => write!(
                f,
                "{}",
                serde_json::to_string(self).map_err(|_| std::fmt::Error)?
            ),
        }
    }
}
