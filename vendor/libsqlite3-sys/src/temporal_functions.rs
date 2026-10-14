//! Typed SQL functions; numeric results stay exact and temporal results stay native.
use crate::{
    decimal::{Error, Result},
    native::Kind,
    temporal::{self, Interval},
};
use bigdecimal::{BigDecimal, RoundingMode};
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use num_bigint::BigInt;
use num_traits::ToPrimitive;

pub(crate) struct Argument<'a> {
    pub kind: i32,
    pub bytes: &'a [u8],
}

fn number(arg: &Argument<'_>) -> Result<BigDecimal> {
    if !matches!(arg.kind, -2 | -1 | 1) {
        return Err(Error(
            "temporal function requires a numeric argument".into(),
        ));
    }
    temporal::scalar(arg.kind, arg.bytes)
}

fn field(arg: &Argument<'_>) -> Result<String> {
    if arg.kind != 0 {
        return Err(Error("temporal field must be TEXT".into()));
    }
    let field = std::str::from_utf8(arg.bytes)
        .map_err(|_| Error("invalid temporal field".into()))?
        .trim()
        .to_ascii_lowercase();
    Ok(match field.as_str() {
        "centuries" => "century".into(),
        "millennia" => "millennium".into(),
        _ => field.strip_suffix('s').unwrap_or(&field).to_owned(),
    })
}

fn calendar(arg: &Argument<'_>) -> Result<(Kind, NaiveDateTime)> {
    match arg.kind {
        3 => Ok((Kind::Instant, temporal::read_datetime(arg.bytes)?)),
        4 => Ok((
            Kind::Date,
            temporal::read_date(arg.bytes)?
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        )),
        5 => Ok((Kind::DateTime, temporal::read_datetime(arg.bytes)?)),
        _ => Err(Error(
            "temporal function requires DATE, DATETIME or INSTANT".into(),
        )),
    }
}

fn scaled(value: i128, scale: i64) -> BigDecimal {
    BigDecimal::new(BigInt::from(value), scale)
}

fn extract(args: &[Argument<'_>]) -> Result<Vec<u8>> {
    let field = field(&args[0])?;
    let value = if args[1].kind == Kind::Interval as i32 {
        let value = temporal::read_interval(args[1].bytes)?;
        let second = i128::from(value.micros) % (60 * temporal::SECOND);
        match field.as_str() {
            "millennium" => BigDecimal::from(value.months / 12_000),
            "century" => BigDecimal::from(value.months / 1_200),
            "decade" => BigDecimal::from(value.months / 120),
            "year" => BigDecimal::from(value.months / 12),
            "quarter" => BigDecimal::from(if value.months < 0 {
                -((-(i64::from(value.months)) % 12) / 3 + 1)
            } else {
                i64::from(value.months % 12) / 3 + 1
            }),
            "month" => BigDecimal::from(value.months % 12),
            "week" => BigDecimal::from(value.days / 7),
            "day" => BigDecimal::from(value.days),
            "hour" => BigDecimal::from(value.micros / 3_600_000_000),
            "minute" => BigDecimal::from(value.micros / 60_000_000 % 60),
            "second" => scaled(second, 6),
            "millisecond" => scaled(second, 3),
            "microsecond" => BigDecimal::from(second),
            "epoch" => scaled(
                i128::from(value.months / 12) * (365 * temporal::DAY + temporal::DAY / 4)
                    + (i128::from(value.months % 12) * 30 + i128::from(value.days)) * temporal::DAY
                    + i128::from(value.micros),
                6,
            ),
            _ => {
                return Err(Error(format!(
                    "extract field {field} is not supported for INTERVAL"
                )))
            }
        }
    } else {
        let (kind, value) = calendar(&args[1])?;
        let second =
            i128::from(value.second()) * temporal::SECOND + i128::from(value.nanosecond() / 1_000);
        match field.as_str() {
            "millennium" => BigDecimal::from((value.year() - 1).div_euclid(1_000) + 1),
            "century" => BigDecimal::from((value.year() - 1).div_euclid(100) + 1),
            "decade" => BigDecimal::from(value.year() / 10),
            "year" => BigDecimal::from(value.year()),
            "isoyear" => BigDecimal::from(value.iso_week().year()),
            "quarter" => BigDecimal::from(value.month0() / 3 + 1),
            "month" => BigDecimal::from(value.month()),
            "week" => BigDecimal::from(value.iso_week().week()),
            "day" => BigDecimal::from(value.day()),
            "doy" => BigDecimal::from(value.ordinal()),
            "dow" => BigDecimal::from(value.weekday().num_days_from_sunday()),
            "isodow" => BigDecimal::from(value.weekday().number_from_monday()),
            "hour" if kind != Kind::Date => BigDecimal::from(value.hour()),
            "minute" if kind != Kind::Date => BigDecimal::from(value.minute()),
            "second" if kind != Kind::Date => scaled(second, 6),
            "millisecond" if kind != Kind::Date => scaled(second, 3),
            "microsecond" if kind != Kind::Date => BigDecimal::from(second),
            "timezone" | "timezone_hour" | "timezone_minute" if kind == Kind::Instant => {
                BigDecimal::from(0)
            }
            "julian" => {
                // Fifteen decimal places preserve every microsecond within a day.
                let day = i128::from(value.num_days_from_ce()) + 1_721_425;
                let time = i128::from(value.num_seconds_from_midnight()) * temporal::SECOND
                    + i128::from(value.nanosecond() / 1_000);
                let numerator = (day * temporal::DAY + time) * 1_000_000_000_000_000;
                let mut quotient = numerator / temporal::DAY;
                let remainder = numerator % temporal::DAY;
                if remainder * 2 > temporal::DAY
                    || (remainder * 2 == temporal::DAY && quotient % 2 == 1)
                {
                    quotient += 1;
                }
                scaled(quotient, 15)
            }
            "epoch" => scaled(
                i128::from(value.and_utc().timestamp()) * temporal::SECOND
                    + i128::from(value.nanosecond() / 1_000),
                6,
            ),
            _ => {
                return Err(Error(format!(
                    "extract field {field} is not supported for this type"
                )))
            }
        }
    };
    Ok(value.normalized().to_plain_string().into_bytes())
}

fn make_interval(args: &[Argument<'_>]) -> Result<Vec<u8>> {
    if args.len() > 7 {
        return Err(Error(
            "make_interval accepts at most seven arguments".into(),
        ));
    }
    let mut fields = [0i32; 6];
    for (field, arg) in fields.iter_mut().zip(args) {
        let value = number(arg)?;
        *field = value
            .to_i32()
            .ok_or_else(|| Error("interval field overflow".into()))?;
        if BigDecimal::from(*field) != value {
            return Err(Error(
                "calendar, hour and minute fields must be integers".into(),
            ));
        }
    }
    let seconds = args.get(6).map(number).transpose()?.unwrap_or_default();
    let micros = (BigDecimal::from(fields[4]) * 3_600_000_000i64
        + BigDecimal::from(fields[5]) * 60_000_000i64
        + seconds * 1_000_000i64)
        .with_scale_round(0, RoundingMode::HalfEven)
        .to_i64()
        .ok_or_else(|| Error("interval time overflow".into()))?;
    let months = i32::try_from(i64::from(fields[0]) * 12 + i64::from(fields[1]))
        .map_err(|_| Error("interval month overflow".into()))?;
    let days = i32::try_from(i64::from(fields[2]) * 7 + i64::from(fields[3]))
        .map_err(|_| Error("interval day overflow".into()))?;
    Ok(temporal::interval_bytes(Interval {
        months,
        days,
        micros,
    }))
}

fn calendar_age(left: NaiveDateTime, right: NaiveDateTime) -> Result<Vec<u8>> {
    let (later, earlier, sign) = if left < right {
        (right, left, -1)
    } else {
        (left, right, 1)
    };
    let mut months =
        (later.year() - earlier.year()) * 12 + later.month() as i32 - earlier.month() as i32;
    let mut days = later.day() as i32 - earlier.day() as i32;
    let mut micros = (i64::from(later.num_seconds_from_midnight())
        - i64::from(earlier.num_seconds_from_midnight()))
        * 1_000_000
        + i64::from(later.nanosecond() / 1_000)
        - i64::from(earlier.nanosecond() / 1_000);
    if micros < 0 {
        micros += temporal::DAY as i64;
        days -= 1;
    }
    if days < 0 {
        // Symbolic differences borrow a month of the earlier date's length.
        let first = earlier.date().with_day(1).unwrap();
        let next = first
            .checked_add_months(chrono::Months::new(1))
            .ok_or_else(|| Error("calendar age overflow".into()))?;
        days += (next - first).num_days() as i32;
        months -= 1;
    }
    Ok(temporal::interval_bytes(Interval {
        months: months * sign,
        days: days * sign,
        micros: micros * i64::from(sign),
    }))
}

fn age(args: &[Argument<'_>]) -> Result<Vec<u8>> {
    let (left_kind, left) = calendar(&args[0])?;
    let (right_kind, right) = calendar(&args[1])?;
    if (left_kind == Kind::Instant) != (right_kind == Kind::Instant) {
        return Err(Error(
            "age between INSTANT and calendar values requires an explicit cast".into(),
        ));
    }
    calendar_age(left, right)
}

fn age_current_date(args: &[Argument<'_>]) -> Result<Vec<u8>> {
    let (_, value) = calendar(&args[0])?;
    let seconds = i64::from_ne_bytes(
        args[1]
            .bytes
            .try_into()
            .map_err(|_| Error("invalid statement clock".into()))?,
    );
    let today = chrono::DateTime::from_timestamp(seconds, 0)
        .ok_or_else(|| Error("statement clock overflow".into()))?
        .date_naive();
    temporal::checked_date(today)?;
    calendar_age(today.and_hms_opt(0, 0, 0).unwrap(), value)
}

fn date_trunc(args: &[Argument<'_>]) -> Result<(Kind, Vec<u8>)> {
    let field = field(&args[0])?;
    let unsupported = || {
        Error(format!(
            "date_trunc field {field} is not supported for this type"
        ))
    };
    if args[1].kind == Kind::Interval as i32 {
        let value = temporal::read_interval(args[1].bytes)?;
        let (months_unit, clear_days, time_unit): (i32, bool, i64) = match field.as_str() {
            "millennium" => (12_000, true, 0),
            "century" => (1_200, true, 0),
            "decade" => (120, true, 0),
            "year" => (12, true, 0),
            "quarter" => (3, true, 0),
            "month" => (1, true, 0),
            "day" => (1, false, 0),
            "hour" => (1, false, 3_600_000_000),
            "minute" => (1, false, 60_000_000),
            "second" => (1, false, 1_000_000),
            "millisecond" => (1, false, 1_000),
            "microsecond" => (1, false, 1),
            _ => return Err(unsupported()),
        };
        return Ok((
            Kind::Interval,
            temporal::interval_bytes(Interval {
                months: value.months / months_unit * months_unit,
                days: if clear_days { 0 } else { value.days },
                micros: if time_unit == 0 {
                    0
                } else {
                    value.micros / time_unit * time_unit
                },
            }),
        ));
    }
    let (kind, value) = calendar(&args[1])?;
    let time_unit = match field.as_str() {
        "microsecond" => Some(1),
        "millisecond" => Some(1_000),
        "second" => Some(1_000_000),
        "minute" => Some(60_000_000),
        "hour" => Some(3_600_000_000),
        _ => None,
    };
    let result = if let Some(unit) = time_unit {
        let time = i64::from(value.num_seconds_from_midnight()) * 1_000_000
            + i64::from(value.nanosecond() / 1_000);
        value
            .date()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .checked_add_signed(chrono::Duration::microseconds(time / unit * unit))
            .ok_or_else(unsupported)?
    } else {
        let date = match field.as_str() {
            "day" => Some(value.date()),
            "week" => value
                .date()
                .checked_sub_signed(chrono::Duration::days(i64::from(
                    value.weekday().num_days_from_monday(),
                ))),
            "month" => value.date().with_day(1),
            "quarter" => NaiveDate::from_ymd_opt(value.year(), value.month0() / 3 * 3 + 1, 1),
            "year" => NaiveDate::from_ymd_opt(value.year(), 1, 1),
            "decade" => NaiveDate::from_ymd_opt(value.year() / 10 * 10, 1, 1),
            "century" => {
                NaiveDate::from_ymd_opt((value.year() - 1).div_euclid(100) * 100 + 1, 1, 1)
            }
            "millennium" => {
                NaiveDate::from_ymd_opt((value.year() - 1).div_euclid(1_000) * 1_000 + 1, 1, 1)
            }
            _ => return Err(unsupported()),
        }
        .ok_or_else(unsupported)?;
        date.and_hms_opt(0, 0, 0).unwrap()
    };
    temporal::checked_date(result.date())?;
    Ok((
        if kind == Kind::Instant {
            kind
        } else {
            Kind::DateTime
        },
        temporal::datetime_bytes(result),
    ))
}

pub(crate) fn evaluate(function: i32, args: &[Argument<'_>]) -> Result<(Kind, Vec<u8>)> {
    match function {
        1 => Ok((Kind::Decimal, extract(args)?)),
        2 => Ok((Kind::Interval, make_interval(args)?)),
        3 => Ok((Kind::Interval, age(args)?)),
        4 => date_trunc(args),
        5 => Ok((Kind::Interval, age_current_date(args)?)),
        _ => Err(Error("unknown temporal function".into())),
    }
}
