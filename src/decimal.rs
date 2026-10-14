//! Decimal SQL functions; the representation and VM arithmetic live with SQLite.
use crate::{Error, Result};
use bigdecimal::{BigDecimal, RoundingMode};
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, Zero};
use rusqlite::{
    Connection,
    functions::{Context, FunctionFlags},
    types::{ToSql, ToSqlOutput, Value as SqlValue, ValueRef},
};

pub(crate) use libsqlite3_sys::decimal::{PREFIX, decode};
pub(crate) use libsqlite3_sys::wire::{decode_blob, encode_blob};
pub(crate) fn parse(text: &str) -> Result<BigDecimal> {
    libsqlite3_sys::decimal::parse(text).map_err(|error| Error::new(error.0))
}
pub(crate) fn encode(text: &str) -> Result<Vec<u8>> {
    libsqlite3_sys::decimal::encode(text).map_err(|error| Error::new(error.0))
}

// A distinct transport keeps ordinary sqlite3_bind_blob/result_blob inputs safe.
// SQLite copies this borrowed descriptor during the bind/result API call.
#[repr(C)]
struct WireBlob {
    data: *const u8,
    length: i32,
}

pub(crate) struct WireValue {
    value: SqlValue,
    blob: WireBlob,
}

impl WireValue {
    pub(crate) fn new(value: SqlValue) -> Self {
        let blob = match &value {
            SqlValue::Blob(bytes) => WireBlob {
                data: bytes.as_ptr(),
                length: i32::try_from(bytes.len()).unwrap_or(-1),
            },
            _ => WireBlob {
                data: std::ptr::null(),
                length: 0,
            },
        };
        Self { value, blob }
    }
}

impl ToSql for WireValue {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        if matches!(self.value, SqlValue::Blob(_)) {
            if self.blob.length < 0 {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_TOOBIG),
                    None,
                ));
            }
            Ok(ToSqlOutput::Pointer((
                std::ptr::from_ref(&self.blob).cast(),
                c"boxdb.wire-blob.v1",
                None,
            )))
        } else {
            self.value.to_sql()
        }
    }
}

fn rounded_divide(
    left: BigDecimal,
    right: BigDecimal,
    scale: i64,
    mode: RoundingMode,
) -> Result<BigDecimal> {
    let (mut numerator, left_scale) = left.as_bigint_and_exponent();
    let (mut denominator, right_scale) = right.as_bigint_and_exponent();
    if denominator.is_zero() {
        return Err(Error::new("DECIMAL division by zero".into()));
    }
    let negative = numerator.is_negative() != denominator.is_negative();
    numerator = numerator.abs();
    denominator = denominator.abs();
    let shift = right_scale + scale - left_scale;
    if shift >= 0 {
        numerator *= BigInt::from(10).pow(shift as u32);
    } else {
        denominator *= BigInt::from(10).pow((-shift) as u32);
    }
    let (mut quotient, remainder) = numerator.div_rem(&denominator);
    let twice = &remainder * 2;
    let increment = !remainder.is_zero()
        && match mode {
            RoundingMode::Up => true,
            RoundingMode::Down => false,
            RoundingMode::Ceiling => !negative,
            RoundingMode::Floor => negative,
            RoundingMode::HalfUp => twice >= denominator,
            RoundingMode::HalfDown => twice > denominator,
            RoundingMode::HalfEven => {
                twice > denominator || (twice == denominator && quotient.is_odd())
            }
        };
    if increment {
        quotient += 1;
    }
    if negative {
        quotient = -quotient;
    }
    Ok(BigDecimal::new(quotient, scale))
}

fn argument(ctx: &Context<'_>, index: usize) -> Result<BigDecimal> {
    match ctx.get_raw(index) {
        ValueRef::Integer(number) => Ok(BigDecimal::from(number)),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            parse(std::str::from_utf8(bytes).map_err(|error| {
                Error::new("invalid DECIMAL encoding".into()).with_source(error)
            })?)
        }
        _ => Err(Error::new(
            "DECIMAL requires decimal text or an integer".into(),
        )),
    }
}

fn rounding(ctx: &Context<'_>, scale_index: usize) -> Result<(i64, RoundingMode)> {
    let scale: i64 = ctx.get(scale_index)?;
    if !(0..=4096).contains(&scale) {
        return Err(Error::new("invalid DECIMAL rounding scale".into()));
    }
    let mode: String = ctx.get(scale_index + 1)?;
    let mode = match mode.as_str() {
        "half_even" => RoundingMode::HalfEven,
        "half_up" => RoundingMode::HalfUp,
        "half_down" => RoundingMode::HalfDown,
        "up" => RoundingMode::Up,
        "down" => RoundingMode::Down,
        "ceiling" => RoundingMode::Ceiling,
        "floor" => RoundingMode::Floor,
        _ => return Err(Error::new("unsupported DECIMAL rounding mode".into())),
    };
    Ok((scale, mode))
}

fn sql_result(number: BigDecimal) -> Result<WireValue> {
    let text = number.to_plain_string();
    parse(&text)?;
    Ok(WireValue::new(SqlValue::Blob(
        [PREFIX, text.as_bytes()].concat(),
    )))
}

pub(crate) fn register(db: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8
        | FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_INNOCUOUS;
    db.create_scalar_function("decimal_round", 3, flags, |ctx| {
        let result = (|| -> Result<WireValue> {
            if (0..3).any(|index| matches!(ctx.get_raw(index), ValueRef::Null)) {
                return Ok(WireValue::new(SqlValue::Null));
            }
            let (scale, mode) = rounding(ctx, 1)?;
            sql_result(argument(ctx, 0)?.with_scale_round(scale, mode))
        })();
        result.map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
    })?;
    db.create_scalar_function("decimal_div", 4, flags, |ctx| {
        let result = (|| -> Result<WireValue> {
            if (0..4).any(|index| matches!(ctx.get_raw(index), ValueRef::Null)) {
                return Ok(WireValue::new(SqlValue::Null));
            }
            let (scale, mode) = rounding(ctx, 2)?;
            sql_result(rounded_divide(
                argument(ctx, 0)?,
                argument(ctx, 1)?,
                scale,
                mode,
            )?)
        })();
        result.map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
    })?;
    Ok(())
}
