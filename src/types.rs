use crate::{Error, Result};
use bigdecimal::BigDecimal;
use num_traits::FromPrimitive;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Integer(i64),
    UnsignedInteger(u64),
    Real(f64),
    Decimal(String),
    Boolean(bool),
    Text(String),
    Bytea(Vec<u8>),
    Uuid(String),
    Json(serde_json::Value),
    /// An RFC 3339 instant, normalized to UTC with microsecond precision.
    Instant(String),
    Date(String),
    /// Calendar date and time without a time zone, with microsecond precision.
    DateTime(String),
    /// Separate signed months, days and microseconds.
    Interval(String),
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
    Decimal(Option<(u32, u32)>),
    Boolean,
    Text,
    Varchar(Option<usize>),
    Bytea,
    Uuid,
    Json,
    Instant,
    Date,
    DateTime,
    Interval,
    Array(Box<Type>),
}

impl Type {
    pub(crate) fn native_kind(&self) -> Option<libsqlite3_sys::native::Kind> {
        use libsqlite3_sys::native::Kind;
        match self {
            Self::Decimal(_) => Some(Kind::Decimal),
            Self::Uuid => Some(Kind::Uuid),
            Self::Instant => Some(Kind::Instant),
            Self::Date => Some(Kind::Date),
            Self::DateTime => Some(Kind::DateTime),
            Self::Interval => Some(Kind::Interval),
            _ => None,
        }
    }

    pub fn parse(input: &str) -> Result<Self> {
        if input.len() > 4096
            || input.matches("[]").count() + input.to_uppercase().matches("ARRAY<").count() > 32
        {
            return Err(Error::new("type nesting or length exceeds limit".into()));
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
                .ok_or_else(|| Error::new("invalid type parameters".into()))?;
            let args = tail
                .split(',')
                .map(|s| {
                    s.trim().parse::<u32>().map_err(|error| {
                        Error::new("invalid type parameter".into()).with_source(error)
                    })
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
            "INT64" | "INTEGER" => Self::Int64,
            "UINT8" => Self::Uint8,
            "UINT16" => Self::Uint16,
            "UINT32" => Self::Uint32,
            "FLOAT4" => Self::Real,
            "REAL" | "DOUBLE" | "DOUBLE PRECISION" | "FLOAT" | "FLOAT8" => Self::Double,
            "DECIMAL" => {
                if args.is_empty() {
                    Self::Decimal(None)
                } else {
                    let p = args[0];
                    let s = *args.get(1).unwrap_or(&0);
                    if args.len() > 2 || p == 0 || p > 1000 || s > p {
                        return Err(Error::new("invalid DECIMAL precision/scale".into()));
                    }
                    Self::Decimal(Some((p, s)))
                }
            }
            "BOOLEAN" | "BOOL" => Self::Boolean,
            "TEXT" => Self::Text,
            "VARCHAR" | "CHARACTER VARYING" => {
                if args.len() > 1 || args.first() == Some(&0) {
                    return Err(Error::new("invalid VARCHAR length".into()));
                }
                Self::Varchar(args.first().map(|n| *n as usize))
            }
            "BYTEA" | "BLOB" => Self::Bytea,
            "UUID" => Self::Uuid,
            "JSON" | "JSONB" => Self::Json,
            "INSTANT" => Self::Instant,
            "DATE" => Self::Date,
            "DATETIME" => Self::DateTime,
            "INTERVAL" => Self::Interval,
            _ => return Err(Error::new(format!("unsupported or missing type: {input}"))),
        };
        if !args.is_empty() && !matches!(result, Self::Decimal(_) | Self::Varchar(_)) {
            return Err(Error::new("type does not accept parameters".into()));
        }
        Ok(result)
    }
    pub fn validate(&self, value: &Value) -> Result<Value> {
        if matches!(self,Self::Decimal(Some((p,s))) if *p==0 || *p>1000 || s>p)
            || matches!(self, Self::Varchar(Some(0)))
        {
            return Err(Error::new("invalid type parameters".into()));
        }
        if let Self::Array(inner) = self {
            inner.validate(&Value::Null)?;
        }
        if matches!(value, Value::Null) {
            return Ok(Value::Null);
        }
        let invalid = || Error::new(format!("type mismatch: expected {self:?}, got {value:?}"));
        match (self, value) {
            (Self::Int8, Value::Integer(n)) if i8::try_from(*n).is_ok() => Ok(value.clone()),
            (Self::Int16, Value::Integer(n)) if i16::try_from(*n).is_ok() => Ok(value.clone()),
            (Self::Int32, Value::Integer(n)) if i32::try_from(*n).is_ok() => Ok(value.clone()),
            (Self::Int64, Value::Integer(_)) => Ok(value.clone()),
            (Self::Uint8 | Self::Uint16 | Self::Uint32, Value::Integer(n)) => {
                let n = u64::try_from(*n).map_err(|error| invalid().with_source(error))?;
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
            (Self::Real | Self::Double, Value::Integer(_) | Value::Decimal(_) | Value::Real(_)) => {
                let n: f64 = value
                    .to_string()
                    .parse()
                    .map_err(|error| invalid().with_source(error))?;
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
            (Self::Decimal(limits), Value::Integer(_) | Value::Decimal(_)) => {
                let decimal = crate::decimal::parse(&value.to_string())?;
                let text = decimal.normalized().to_plain_string();
                let unsigned = text.trim_start_matches('-');
                let (whole, frac) = unsigned.split_once('.').unwrap_or((unsigned, ""));
                if let Some((precision, scale)) = limits {
                    if frac.len() > *scale as usize
                        || whole.trim_start_matches('0').len() > (precision - scale) as usize
                    {
                        return Err(Error::new("DECIMAL precision or scale overflow".into()));
                    }
                    let text = decimal.with_scale(*scale as i64).to_plain_string();
                    Ok(Value::Decimal(text))
                } else {
                    Ok(Value::Decimal(text))
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
            (Self::Json, Value::Json(_)) => Ok(value.clone()),
            (Self::Uuid, Value::Uuid(s))
            | (Self::Instant, Value::Instant(s))
            | (Self::Date, Value::Date(s))
            | (Self::DateTime, Value::DateTime(s))
            | (Self::Interval, Value::Interval(s)) => {
                let kind = self.native_kind().unwrap();
                let text =
                    libsqlite3_sys::native::canonical(kind, s).map_err(|e| Error::new(e.0))?;
                Ok(match self {
                    Self::Uuid => Value::Uuid(text),
                    Self::Instant => Value::Instant(text),
                    Self::Date => Value::Date(text),
                    Self::DateTime => Value::DateTime(text),
                    Self::Interval => Value::Interval(text),
                    _ => unreachable!(),
                })
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
        let source = match &value {
            Value::Decimal(_) => Some(libsqlite3_sys::native::Kind::Decimal),
            Value::Uuid(_) => Some(libsqlite3_sys::native::Kind::Uuid),
            Value::Instant(_) => Some(libsqlite3_sys::native::Kind::Instant),
            Value::Date(_) => Some(libsqlite3_sys::native::Kind::Date),
            Value::DateTime(_) => Some(libsqlite3_sys::native::Kind::DateTime),
            Value::Interval(_) => Some(libsqlite3_sys::native::Kind::Interval),
            _ => None,
        };
        if let (Some(target), Some(source)) = (self.native_kind(), source)
            && target != source
        {
            let text = libsqlite3_sys::native::cast(target, source, &value.to_string())
                .map_err(|e| Error::new(e.0))?;
            return self.cast(Value::Text(text));
        }
        let text = if let (Self::Decimal(_), Value::Real(number)) = (self, &value) {
            BigDecimal::from_f64(*number)
                .ok_or_else(|| Error::new("non-finite REAL cannot be DECIMAL".into()))?
                .to_plain_string()
        } else {
            value.to_string()
        };
        let parsed = match self {
            Self::Int8 | Self::Int16 | Self::Int32 | Self::Int64 => {
                Value::Integer(text.parse().map_err(|error| {
                    Error::new("invalid integer cast".into()).with_source(error)
                })?)
            }
            Self::Uint8 | Self::Uint16 | Self::Uint32 => {
                Value::UnsignedInteger(text.parse().map_err(|error| {
                    Error::new("invalid unsigned integer cast".into()).with_source(error)
                })?)
            }
            Self::Real | Self::Double => Value::Real(text.parse().map_err(|error| {
                Error::new("invalid floating-point cast".into()).with_source(error)
            })?),
            Self::Decimal(_) => Value::Decimal(text),
            Self::Boolean => Value::Boolean(match text.to_lowercase().as_str() {
                "true" => true,
                "false" => false,
                _ => return Err(Error::new("invalid boolean cast".into())),
            }),
            Self::Text | Self::Varchar(_) => Value::Text(text),
            Self::Uuid => Value::Uuid(text),
            Self::Instant => Value::Instant(text),
            Self::Date => Value::Date(text),
            Self::DateTime => Value::DateTime(text),
            Self::Interval => Value::Interval(text),
            Self::Json => Value::Json(serde_json::from_str(&text).map_err(Error::validation)?),
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
            Self::Decimal(s)
            | Self::Text(s)
            | Self::Uuid(s)
            | Self::Instant(s)
            | Self::Date(s)
            | Self::DateTime(s)
            | Self::Interval(s) => {
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
